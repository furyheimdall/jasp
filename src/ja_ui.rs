use crate::{
    chrome::{self, Controls, Motion, Song, Transport},
    dac::{OutputFormat, SourceCodec},
    ja::{self, Action, Event, UsbPlay, Worker},
    ja_library::Library,
    ja_library_admin_ui::AdminView,
    model::{QueuePlacement, TrackDetails},
    settings::tr,
    theme, ui::{MUTED, WARNING, RailIcon, button, rail_item},
    track_dialog::TrackDialog,
};
use std::time::Instant;
use eframe::egui::{self, RichText, TextureHandle, Ui, vec2};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page { Playing, Queue, Library }

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Server,
    Username,
    Password,
    Search,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Self::Server => "Server URL",
            Self::Username => "Username",
            Self::Password => "Password",
            Self::Search => "Library search",
        }
    }
}

#[derive(Default)]
pub(crate) struct Keyboard {
    target: Option<Field>,
    shift: bool,
    symbols: bool,
}

enum Key {
    Character(char),
    Backspace,
}

impl Keyboard {
    pub(crate) fn show_search(&mut self, ui: &mut Ui, text: &mut String) -> bool {
        self.target = Some(Field::Search);
        match self.show(ui) {
            Some(Key::Character(ch)) => text.push(ch),
            Some(Key::Backspace) => { text.pop(); }
            None => {}
        }
        self.target.is_some()
    }

    fn key_size(ui: &Ui) -> (f32, f32, usize) {
        let width = ui.available_width();
        let height = (ui.ctx().content_rect().height() * 0.085).clamp(30.0, 49.0);
        let row_gap = (height * 0.12).clamp(3.0, 6.0);
        let columns = if width >= 400.0 { 6 } else if width >= 205.0 { 3 } else { 2 };
        (height, row_gap, columns)
    }

    fn dock_height(ui: &Ui) -> f32 {
        let (height, gap, columns) = Self::key_size(ui);
        let rows = 4 + 6_usize.div_ceil(columns);
        36.0 + rows as f32 * (height + gap)
    }

    fn show(&mut self, ui: &mut Ui) -> Option<Key> {
        let target = self.target?;
        let mut key = None;
        ui.label(RichText::new(format!("{} · {}", tr(ui, "Touch keyboard"), tr(ui, target.label())))
            .size(20.0).strong());
        let lines: [&str; 4] = match (self.symbols, self.shift) {
            (false, false) => ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"],
            (false, true) => ["1234567890", "QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM"],
            (true, _) => ["!@#$%^&*()", "-_=+[]{}<>", "~`'\";:,.?/", "\\|"],
        };
        let width = ui.available_width();
        let gap = (width * 0.014).clamp(3.0, 8.0);
        let cell = ((width - 9.0 * gap - 1.0) / 10.0).clamp(1.0, 62.0);
        let (height, row_gap, columns) = Self::key_size(ui);
        let font = (cell * 0.48).clamp(10.0, 21.0);
        ui.scope(|ui| {
            let spacing = ui.spacing_mut();
            spacing.item_spacing = vec2(gap, row_gap);
            spacing.button_padding = vec2(2.0, 1.0);
            for line in lines {
                let count = line.len() as f32; // Key layouts contain ASCII only.
                let row_width = count * cell + (count - 1.0) * gap;
                ui.horizontal(|ui| {
                    ui.add_space(((width - row_width) / 2.0 - gap).max(0.0));
                    for ch in line.chars() {
                        if ui.add_sized([cell, height], egui::Button::new(RichText::new(ch).size(font).color(egui::Color32::WHITE))
                            .corner_radius(6)).clicked() { key = Some(Key::Character(ch)); }
                    }
                });
            }
            let labels = ["ABC", "!@#", "Shift", "Space", "Backspace", "Hide"];
            let cell = ((width - (columns - 1) as f32 * gap - 1.0) / columns as f32).max(1.0);
            ui.add_space(4.0);
            for (row, labels) in labels.chunks(columns).enumerate() {
                ui.horizontal(|ui| {
                    for (column, label) in labels.iter().enumerate() {
                        let index = row * columns + column;
                        let selected = match index {
                            0 => !self.symbols, 1 => self.symbols, 2 => self.shift && !self.symbols, _ => false,
                        };
                        let text = RichText::new(tr(ui, label)).size(13.0)
                            .color(if selected { theme::ON_ACCENT } else { egui::Color32::WHITE });
                        let control = egui::Button::new(text).selected(selected).corner_radius(6)
                            .wrap_mode(egui::TextWrapMode::Truncate);
                        if ui.add_sized([cell, height], if selected { control.fill(theme::accent(ui)) } else { control }).clicked() {
                            match index {
                                0 => self.symbols = false,
                                1 => self.symbols = true,
                                2 => self.shift = !self.shift,
                                3 => key = Some(Key::Character(' ')),
                                4 => key = Some(Key::Backspace),
                                _ => self.target = None,
                            }
                        }
                    }
                });
            }
        });
        key
    }
}

pub(crate) enum UsbNotice {
    Prepared(u64, Result<(), String>),
    Committed(u64, Result<(), String>),
    Released(u64, Result<(), String>),
}

struct UsbSignal {
    codec: SourceCodec,
    source: String,
    pcm: String,
}

impl From<OutputFormat> for UsbSignal {
    fn from(format: OutputFormat) -> Self {
        let OutputFormat { codec, rate, channels, source_bits, pcm } = format;
        let source = match source_bits {
            Some(bits) => format!("{rate} Hz · {bits}-bit FLAC"),
            None => format!("{rate} Hz · MP3"),
        };
        Self { codec, source, pcm: format!("{pcm:?} · {channels} ch") }
    }
}

impl UsbSignal {
    fn path<'a>(&'a self, device: &'a str) -> chrome::SignalPath<'a> {
        use chrome::SignalQuality::Preserved;
        let (decoder, note) = match self.codec {
            SourceCodec::Flac => ("Symphonia · FLAC", "Bit-perfect to ALSA · No resampling, channel remix or DSP"),
            SourceCodec::Mp3 => ("Symphonia · MP3", "Bit-perfect PCM to ALSA · After decoding"),
        };
        let device = device.strip_prefix("hw:CARD=").and_then(|value| value.split_once(",DEV="))
            .map_or(device, |(name, _)| name);
        chrome::SignalPath {
            stages: [("jastreamer", &self.source, Preserved), ("Decoding", decoder, Preserved),
                ("PCM", &self.pcm, Preserved), (device, "ALSA hw · USB DAC", Preserved)],
            note, flowing: true,
        }
    }
}

pub struct View {
    worker: Worker,
    page: Page,
    url: String,
    allow_http: bool,
    remember: bool,
    start_selected: bool,
    identity: Option<String>,
    username: String,
    password: Zeroizing<String>,
    keyboard: Keyboard,
    signed_in: bool,
    read_only: bool,
    player: Option<ja::PlayerState>,
    player_received: Instant,
    entrance: chrome::Entrance,
    queue: Option<ja::Queue>,
    library: Library,
    library_admin: AdminView,
    renderers: Vec<ja::Renderer>,
    devices: Vec<String>,
    local: Option<(String, String)>,
    local_pending: bool,
    usb_play: Option<UsbPlay>,
    usb_notice: Option<UsbNotice>,
    moode_may_hold_usb: bool,
    format: Option<UsbSignal>,
    error: Option<String>,
    cover: Option<(String, TextureHandle)>,
    transport: Transport,
    queue_clear: Option<u64>,
}

impl View {
    pub fn new(ctx: egui::Context, url: String, read_only: bool) -> anyhow::Result<Self> {
        let (saved, startup_warning) = match ja::saved_server() {
            Ok(saved) => (saved, None),
            Err(error) => (None, Some(format!("Could not read saved jastreamer settings: {error:#}"))),
        };
        let saved = saved.filter(|(server, _)| url.trim().is_empty()
            || ja::server_url(&url).is_ok_and(|candidate| candidate.as_str() == server));
        let start_selected = saved.is_some();
        let (url, allow_http) = saved.unwrap_or((url, false));
        let mut view = Self {
            worker: Worker::start(read_only, ctx)?, page: Page::Playing, url,
            allow_http, remember: true, start_selected, identity: None, username: String::new(), password: Zeroizing::new(String::new()),
            keyboard: Keyboard::default(),
            signed_in: false, read_only, player: None, queue: None, library: Library::default(),
            library_admin: AdminView::default(),
            player_received: Instant::now(), entrance: chrome::Entrance::default(),
            renderers: Vec::new(), devices: Vec::new(), local: None,
            local_pending: false, format: None, error: None, cover: None, transport: Transport::default(),
            usb_play: None, usb_notice: None,
            moode_may_hold_usb: false,
            queue_clear: None,
        };
        if !view.url.is_empty() { view.connect(); }
        view.error = startup_warning;
        Ok(view)
    }

    pub(crate) fn start_selected(&self) -> bool { self.start_selected }

    pub(crate) fn library_settings(&mut self, ui: &mut Ui, activity: &mut crate::library_activity::Activity) {
        if !self.signed_in {
            ui.label(tr(ui, "Sign in to manage the jastreamer library."));
            return;
        }
        let tx = &self.worker.tx;
        self.library_admin.show(ui, !self.read_only, activity, |op| tx.try_send(Action::LibraryAdmin(op)).is_ok());
    }

    pub(crate) fn poll_library_scan(&mut self, ctx: &egui::Context) {
        let tx = &self.worker.tx;
        self.library_admin.poll(ctx, |op| tx.try_send(Action::LibraryAdmin(op)).is_ok());
    }

    pub fn owns_local(&self) -> bool { self.local.is_some() || self.local_pending }

    pub(crate) fn usb_devices(&self) -> &[String] { &self.devices }
    pub(crate) fn set_moode_usb_interlock(&mut self, may_hold: bool) { self.moode_may_hold_usb = may_hold; }
    pub(crate) fn usb_device(&self) -> Option<&str> { self.local.as_ref().map(|(_, device)| device.as_str()) }
    pub(crate) fn take_usb_play(&mut self) -> Option<UsbPlay> { self.usb_play.take() }
    pub(crate) fn take_usb_notice(&mut self) -> Option<UsbNotice> { self.usb_notice.take() }
    pub(crate) fn finish_usb_play(&mut self) { self.library.clear_pending(); }

    pub fn has_transport(&self) -> bool { self.signed_in && self.player.is_some() }

    pub(crate) fn send(&mut self, action: Action) -> bool {
        match self.worker.tx.try_send(action) {
            Ok(()) => { self.error = None; true }
            Err(err) => { self.error = Some(format!("jastreamer command could not be sent: {err}")); false }
        }
    }

    fn connect(&mut self) {
        self.identity = None;
        self.password.zeroize();
        self.keyboard.target = None;
        self.send(Action::Connect(self.url.clone()));
    }

    pub fn events(&mut self, ctx: &egui::Context, activity: &mut crate::library_activity::Activity,
        mut track_dialog: Option<&mut TrackDialog>) {
        while let Ok(event) = self.worker.rx.try_recv() {
            match event {
                Event::Connected { url, label } if url == self.url => {
                    self.identity = Some(label);
                    self.keyboard.target = Some(Field::Username);
                }
                Event::Connected { .. } => {}
                Event::SignedIn(signed_in) => {
                    self.signed_in = signed_in;
                    self.password.zeroize();
                    if signed_in { self.keyboard = Keyboard::default(); }
                    self.page = Page::Playing;
                    self.entrance.begin();
                    self.transport.reset();
                    self.player = None; self.queue = None; self.library = Library::default();
                    self.library_admin = AdminView::default();
                    self.queue_clear = None;
                    if let Some(info) = track_dialog.as_deref_mut() { info.invalidate_jastreamer(); }
                    activity.clear_jastreamer();
                    self.cover = None; self.renderers.clear();
                }
                Event::Player(player) => {
                    self.player_received = Instant::now();
                    if self.player.as_ref().is_none_or(|prev| prev.current_entry_id != player.current_entry_id) {
                        self.transport.reset();
                        if self.page == Page::Playing && self.player.is_some() { self.entrance.begin(); }
                    }
                    if self.cover.as_ref().is_some_and(|(id, _)| player.track.as_ref().is_none_or(|t| t.artwork_id != *id)) { self.cover = None; }
                    self.player = Some(player);
                }
                Event::Queue(queue) => self.queue = Some(queue),
                Event::Catalog(query, result) => self.library.loaded(query, result),
                Event::Thumbnail(id, image) => self.library.thumbnail(ctx, id, image),
                Event::TrackDetails(id, result) => {
                    if let Some(info) = track_dialog.as_deref_mut() { info.receive(&id, result); }
                }
                Event::CatalogChanged(message) => self.library.changed(Ok(message)),
                Event::CatalogMutationFailed(error) => self.library.changed(Err(error)),
                Event::TrackLiked(id, liked) => {
                    self.library.liked(&id, liked);
                    if let Some(queue) = &mut self.queue {
                        for entry in queue.entries.iter_mut().filter(|entry| entry.track.id == id) { entry.track.liked = liked; }
                    }
                }
                Event::Renderers(items) => self.renderers = items,
                Event::Art(id, image) => {
                    if self.player.as_ref().and_then(|p| p.track.as_ref()).is_some_and(|track| track.artwork_id == id) {
                        self.cover = Some((id, ctx.load_texture("jastreamer artwork", image, egui::TextureOptions::LINEAR)));
                    }
                }
                Event::Devices(devices) => self.devices = devices,
                Event::UsbPrepared { request, result } => self.usb_notice = Some(UsbNotice::Prepared(request, result)),
                Event::UsbCommitted { request, result } => self.usb_notice = Some(UsbNotice::Committed(request, result)),
                Event::UsbReleased { request, result } => self.usb_notice = Some(UsbNotice::Released(request, result)),
                Event::Local(local) => { self.local = local; self.local_pending = false; }
                Event::Format(format) => self.format = format.map(UsbSignal::from),
                Event::Error(error) => { self.error = Some(error); }
                Event::LibraryAdmin(result) => {
                    if self.library_admin.loaded(result, activity) { self.library.reload(); }
                }
            }
        }
    }

    pub(crate) fn account(&mut self, ui: &mut Ui, compact: bool) {
        let tint = theme::accent(ui);
        if self.signed_in && rail_item(ui, RailIcon::SignOut, "Sign out", false, compact, true, tint).clicked() {
            self.send(Action::Logout);
        }
        if !compact {
            let label = self.identity.as_deref().unwrap_or("Not connected");
            ui.add(egui::Label::new(RichText::new(label).size(12.0).color(MUTED)).truncate()).on_hover_text(label);
        }
    }

    pub(crate) fn navigation(&mut self, ui: &mut Ui, compact: bool, in_settings: bool) -> bool {
        let tint = theme::accent(ui);
        let mut clicked = false;
        for (page, icon, label) in [
            (Page::Playing, RailIcon::Playing, "Now playing"),
            (Page::Queue, RailIcon::Queue, "Queue"),
            (Page::Library, RailIcon::Library, "Library"),
        ] {
            let active = self.signed_in && !in_settings && self.page == page;
            if rail_item(ui, icon, label, active, compact, self.signed_in, tint).clicked() {
                clicked = true;
                if self.page != page {
                    self.entrance.begin_in((page as u8) > (self.page as u8));
                    self.page = page;
                    if page == Page::Queue { self.send(Action::Queue); }
                }
            }
        }
        clicked
    }

    pub(crate) fn has_status(&self) -> bool {
        self.read_only || self.error.is_some() || self.player.as_ref().is_some_and(|state| !state.error.is_empty())
    }

    pub fn status(&self, ui: &mut Ui) {
        if let Some(error) = &self.error { chrome::status_ticker(ui, "ja-request-error", "", error, WARNING); }
        if let Some(error) = self.player.as_ref().map(|s| s.error.as_str()).filter(|s| !s.is_empty()) {
            chrome::status_ticker(ui, "ja-player-error", "", error, WARNING);
        }
        if self.read_only {
            chrome::status_ticker(ui, "ja-read-only", "", "READ ONLY  ·  Server playback and outputs cannot be changed", MUTED);
        }
    }

    pub fn show(&mut self, ui: &mut Ui, track_dialog: &mut Option<TrackDialog>) {
        let entrance = self.entrance;
        entrance.show(ui, |ui| {
            if !self.signed_in { self.sign_in(ui); return; }
            match self.page {
                Page::Playing => { egui::ScrollArea::vertical().show(ui, |ui| self.playing(ui, track_dialog)); }
                Page::Queue => self.queue(ui),
                Page::Library => {
                    let can_edit = !self.read_only;
                    let can_play = self.can_start();
                    let local = self.can_start_without_handoff();
                    let tx = &self.worker.tx;
                    let request = &mut self.usb_play;
                    self.library.show(ui, &mut self.keyboard, can_edit, can_play,
                        |action| Self::route_play(local, tx, request, action));
                }
            }
        });
    }

    fn sign_in(&mut self, ui: &mut Ui) {
        let width = ui.available_width();
        let docked = width < 810.0 && self.keyboard.target.is_some();
        let dock_id = ui.id().with("login-keyboard-docked");
        let was_docked = ui.data_mut(|data| {
            let prior = data.get_temp::<bool>(dock_id).unwrap_or_default();
            data.insert_temp(dock_id, docked);
            prior
        });
        if width >= 810.0 && self.keyboard.target.is_some() {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width((width * 0.45).clamp(315.0, 445.0));
                        self.login_form(ui, false);
                    });
                    ui.add_space(24.0);
                    ui.vertical(|ui| self.login_keyboard(ui));
                });
            });
        } else {
            let keyboard_open = self.keyboard.target.is_some();
            let form_height = if keyboard_open {
                (ui.available_height() - Keyboard::dock_height(ui) - 10.0).max(80.0)
            } else { ui.available_height() };
            egui::ScrollArea::vertical().id_salt("compact-login-fields")
                .max_height(form_height).auto_shrink([false, false]).show(ui, |ui| self.login_form(ui, docked && !was_docked));
            if keyboard_open {
                ui.separator();
                self.login_keyboard(ui);
            } else if self.keyboard.target.is_some() {
                ui.ctx().request_repaint();
            }
        }
    }

    fn login_form(&mut self, ui: &mut Ui, reveal_active: bool) {
        ui.label(RichText::new("jastreamer Server").size(30.0).strong());
        ui.label(RichText::new(tr(ui, "Control the server queue or play music on a local USB DAC.")).color(MUTED));
        ui.add_space(14.0);
        let width = ui.available_width().min(470.0);
        let server = login_field(ui, Field::Server, width, &mut self.url, &mut self.keyboard.target);
        reveal_login_field(&server, Field::Server, self.keyboard.target, reveal_active);
        if server.changed() { self.server_edited(); }
        ui.checkbox(&mut self.allow_http, tr(ui, "Allow unencrypted sign-in on this trusted LAN"));
        if button(ui, "Connect / Verify", false).clicked() { self.connect(); }
        if self.identity.is_some() {
            ui.add_space(14.0);
            let username = login_field(ui, Field::Username, width, &mut self.username, &mut self.keyboard.target);
            reveal_login_field(&username, Field::Username, self.keyboard.target, reveal_active);
            let password = login_field(ui, Field::Password, width, &mut self.password, &mut self.keyboard.target);
            reveal_login_field(&password, Field::Password, self.keyboard.target, reveal_active);
            let enter = password.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.checkbox(&mut self.remember, tr(ui, "Remember this server and stay signed in"));
            if (button(ui, "Sign in", true).clicked() || enter) && !self.username.is_empty() && !self.password.is_empty() {
                let password = std::mem::take(&mut self.password);
                self.send(Action::Login { username: self.username.clone(), password,
                    allow_http: self.allow_http, remember: self.remember });
            }
            ui.label(RichText::new(tr(ui, "Only the session is saved, never your password. Use HTTP only on a trusted LAN.")).color(MUTED));
        }
    }

    fn login_keyboard(&mut self, ui: &mut Ui) {
        let Some(target) = self.keyboard.target else { return; };
        let Some(key) = self.keyboard.show(ui) else { return; };
        let value = match target {
            Field::Server => &mut self.url,
            Field::Username => &mut self.username,
            Field::Password => &mut *self.password,
            Field::Search => return,
        };
        match key {
            Key::Character(ch) => value.push(ch),
            Key::Backspace => { value.pop(); }
        }
        if target == Field::Server { self.server_edited(); }
    }

    fn server_edited(&mut self) {
        self.identity = None;
        self.password.zeroize();
        self.allow_http = false;
    }

    fn enabled(&self) -> bool {
        !self.read_only && !self.local_pending
            && self.player.as_ref().is_some_and(|s| s.pending_command.is_empty())
    }

    fn can_start(&self) -> bool {
        self.enabled() && (!self.using_usb() || self.player.as_ref().is_some_and(|s| s.capabilities.play))
    }

    fn can_start_without_handoff(&self) -> bool {
        self.using_usb() && (!self.moode_may_hold_usb || self.player.as_ref().is_some_and(|s| s.state == "playing"))
    }

    fn route_play(local: bool, tx: &ja::ActionSender, play: &mut Option<UsbPlay>, action: Action) -> bool {
        let action = if local { action } else {
            let intent = match action {
                Action::Player("play", entry, None) => UsbPlay::Player(entry),
                Action::CatalogQueue(QueuePlacement::Play, target) => UsbPlay::Catalog(target),
                other => return tx.try_send(other).is_ok(),
            };
            if play.is_some() { return false; }
            *play = Some(intent);
            return true;
        };
        tx.try_send(action).is_ok()
    }

    fn using_usb(&self) -> bool {
        self.local.as_ref().is_some_and(|(id, _)| self.player.as_ref().is_some_and(|p| p.renderer_id == *id))
    }

    fn playing(&self, ui: &mut Ui, track_dialog: &mut Option<TrackDialog>) {
        let Some(state) = &self.player else { ui.label(tr(ui, "Waiting for player state…")); return; };
        let label = match state.state.as_str() {
            "playing" if !self.using_usb() => "PLAYING ELSEWHERE",
            "playing" => "PLAYING", "paused" => "PAUSED", "starting" => "STARTING",
            "error" => "ERROR", "unavailable" => "UNAVAILABLE", _ => "STOPPED",
        };
        let output = if self.using_usb() { "Pi USB DAC · Direct" } else { "" };
        let song = Self::song(state, self.cover.as_ref().map(|(_, texture)| texture));
        let path = if self.using_usb() {
            self.format.as_ref().zip(self.usb_device()).map(|(format, device)| format.path(device))
        } else { None };
        let track = state.track.as_ref().filter(|track| !track.id.is_empty());
        if chrome::now_playing(ui, &song, label, output, &state.pending_command, path, track.is_some())
            && let Some(track) = track {
            let info = TrackDetails { codec: track.format.clone(), disc: (track.disc > 0).then_some(track.disc),
                number: (track.track > 0).then_some(track.track), ..TrackDetails::default() };
            let duration = if track.duration_ms > 0 { track.duration_ms } else { state.duration_ms };
            let mut modal = TrackDialog::new("jastreamer", &track.title, &track.artist, &track.album, duration as f64 / 1000.0, info)
                .pending(track.id.clone());
            if self.worker.tx.try_send(Action::TrackDetails(track.id.clone())).is_err() { modal.invalidate_jastreamer(); }
            *track_dialog = Some(modal);
        }
    }

    fn output_name(&self, id: &str) -> &str {
        self.renderers.iter().find(|renderer| renderer.id == id)
            .map_or("No server output selected", |renderer| renderer.name.as_str())
    }

    fn song<'a>(state: &'a ja::PlayerState, cover: Option<&'a TextureHandle>) -> Song<'a> {
        let track = state.track.as_ref();
        Song {
            title: track.map_or("", |t| t.title.as_str()),
            artist: track.map_or("", |t| t.artist.as_str()),
            album: track.map_or("", |t| t.album.as_str()),
            cover,
        }
    }

    pub fn transport(&mut self, ui: &mut Ui) {
        let Some(state) = &self.player else { return; };
        let local = self.using_usb();
        let enabled = self.enabled() && local;
        let playing = local && state.state == "playing";
        let song = Self::song(state, self.cover.as_ref().map(|(_, texture)| texture));
        let controls = Controls {
            elapsed: state.position_ms as f64 / 1000.0 + if playing && state.pending_command.is_empty() {
                self.player_received.elapsed().as_secs_f64().min(2.0)
            } else { 0.0 },
            duration: state.duration_ms as f64 / 1000.0,
            playing, can_seek: enabled && state.capabilities.seek,
            can_navigate: enabled && state.capabilities.play,
            can_toggle: if playing { enabled && state.capabilities.pause } else { self.can_start() },
            can_set_modes: self.enabled(), shuffle: state.shuffle, repeat: state.repeat_mode,
            volume: None, muted: false, can_volume: false,
        };
        if let Some(action) = self.transport.show(ui, &song, controls) {
            let (action, position) = match action {
                Motion::Previous => ("previous", None),
                Motion::Next => ("next", None),
                Motion::PlayPause => (if playing { "pause" } else { "play" }, None),
                Motion::Seek(seconds) => ("seek", Some((seconds * 1000.0).round() as u64)),
                Motion::Volume(_) => return,
                Motion::Shuffle(shuffle) => { self.send(Action::SetShuffle(shuffle)); return; }
                Motion::Repeat(mode) => { self.send(Action::SetRepeat(mode)); return; }
            };
            if action == "play" && !self.can_start_without_handoff() { self.usb_play = Some(UsbPlay::Player(None)); }
            else { self.send(Action::Player(action, None, position)); }
        }
    }

    fn queue(&mut self, ui: &mut Ui) {
        let can_edit = self.enabled();
        let local = self.using_usb();
        let can_play = self.can_start();
        if !local && self.player.as_ref().is_some_and(|p| p.state == "playing") {
            ui.label(RichText::new(tr(ui, "Server queue · playback is on another output")).color(MUTED));
        }
        let tx = &self.worker.tx;
        let local_start = self.can_start_without_handoff();
        let request = &mut self.usb_play;
        self.queue_clear = self.library.show_queue(ui, self.queue.as_ref(), self.player.as_ref(), self.cover.as_ref().map(|(_, cover)| cover),
            can_edit, can_play, |action| Self::route_play(local_start, tx, request, action));
    }

    pub(crate) fn take_queue_clear(&mut self) -> Option<u64> { self.queue_clear.take() }

    pub(crate) fn can_clear_queue(&self, revision: u64) -> bool {
        self.signed_in && self.enabled()
            && self.queue.as_ref().is_some_and(|queue| queue.revision == revision && !queue.entries.is_empty())
    }

    pub(crate) fn clear_queue(&mut self, revision: u64) {
        if self.can_clear_queue(revision) && self.send(Action::QueueClear { revision }) {
            self.library.set_queue_pending();
        }
    }


    pub(crate) fn scan_usb(&mut self) { self.send(Action::ScanDac); }

    pub(crate) fn outputs(&mut self, ui: &mut Ui, moode_playing: bool) {
        let mut command = None;
        let player = self.player.as_ref();
        let active = player.map_or("", |s| s.renderer_id.as_str());
        let allowed = self.signed_in && self.enabled() && !moode_playing;
        ui.label(RichText::new(tr(ui, "USB devices")).size(22.0).strong());
        ui.label(RichText::new(tr(ui, "Local USB audio · Direct output without a software mixer.")).color(MUTED));
        if self.devices.is_empty() {
            ui.label(RichText::new(tr(ui, "No USB playback device detected.")).color(MUTED));
        }
        for device in &self.devices {
            let is_registered = self.local.as_ref().is_some_and(|(_, registered)| device == registered);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(device).monospace());
                if is_registered {
                    let selected = self.using_usb();
                    ui.label(RichText::new(if selected { "ACTIVE FOR JASTREAMER" } else { "REGISTERED" }).size(12.0).color(theme::accent(ui)));
                } else if self.local.is_none() {
                    ui.add_enabled_ui(allowed, |ui| {
                        if button(ui, "Register for jastreamer", false).clicked() {
                            command = Some(Action::Attach(device.clone()));
                        }
                    });
                }
            });
        }
        if !self.signed_in {
            ui.label(RichText::new("Select jastreamer in the sidebar and sign in to use the USB device there.").color(MUTED));
        } else if moode_playing {
            ui.label(RichText::new("Stop moOde playback before assigning this USB output to jastreamer.").color(WARNING));
        }
        if let Some((id, device)) = &self.local {
            let selected = self.using_usb();
            if !self.devices.contains(device) {
                ui.label(RichText::new("The registered USB device is not currently detected.").color(WARNING));
            }
            ui.horizontal_wrapped(|ui| {
                if !selected {
                    let stopped = player.is_some_and(|s| s.state == "stopped");
                    let fallback = player.filter(|s| s.state == "unavailable" && !active.is_empty()
                        && !self.renderers.iter().any(|r| r.id == active && r.online));
                    ui.add_enabled_ui(allowed && self.devices.contains(device) && (stopped || fallback.is_some()), |ui| {
                        if button(ui, "Use this USB DAC", true).clicked() {
                            command = Some(if let Some(state) = fallback {
                                Action::Fallback { renderer: id.clone(), expected: state.renderer_id.clone(), revision: state.revision }
                            } else { Action::Select(id.clone()) });
                        }
                    });
                    if !active.is_empty() && !stopped && fallback.is_none() {
                        ui.add_enabled_ui(allowed && player.is_some_and(|s| s.capabilities.stop), |ui| {
                            if button(ui, "Stop other output", false).clicked() {
                                command = Some(Action::StopForUsb { expected: active.to_owned() });
                            }
                        });
                    }
                }
                ui.add_enabled_ui(!self.local_pending && !self.read_only, |ui| {
                    if button(ui, "Release USB DAC", false).clicked() { command = Some(Action::Release); }
                });
            });
        }
        if !active.is_empty() && !self.using_usb() {
            ui.label(format!("Current server output: {}. This Pi plays only to local USB.", self.output_name(active)));
        }
        ui.label(RichText::new("Manage network outputs in the jastreamer web UI.").color(MUTED));
        if let Some(command) = command {
            let pending = matches!(command, Action::Attach(_) | Action::Release);
            if self.send(command) && pending { self.local_pending = true; }
        }
    }
}

fn login_field(ui: &mut Ui, field: Field, width: f32, value: &mut String, target: &mut Option<Field>) -> egui::Response {
    let label = RichText::new(tr(ui, field.label())).color(if *target == Some(field) { theme::accent(ui) } else { MUTED });
    ui.label(label);
    let input = egui::TextEdit::singleline(value).password(field == Field::Password);
    let response = ui.add_sized(vec2(width, 42.0), input);
    if response.clicked() || response.gained_focus() { *target = Some(field); }
    response
}

fn reveal_login_field(response: &egui::Response, field: Field, target: Option<Field>, reveal: bool) {
    if reveal && target == Some(field) { response.scroll_to_me(Some(egui::Align::Center)); }
}
