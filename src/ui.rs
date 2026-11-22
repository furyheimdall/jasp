use crate::model::{Command, Connection, Event, QueuePlacement, Snapshot, Track};
use crate::chrome::{self, Controls, Motion, Song, Transport};
use crate::{dialog, settings::{tr, widgets as settings_ui}, theme, track_dialog::TrackDialog};
use eframe::egui::{self, Align, Align2, Color32, FontId, Painter, Rect, RichText, Sense, Stroke, TextureHandle, Ui, pos2, vec2};
use std::time::Instant;

#[path = "moode_library.rs"]
mod moode_library;
use moode_library::{Artwork, Library};
#[path = "usb_handoff.rs"]
mod usb_handoff;
use usb_handoff::Handoff;
use crate::library_activity::{Activity, Kind as LibraryActivity};
#[path = "audio_path.rs"]
mod audio_path;
use audio_path::MoodePath;

pub(crate) const MUTED: Color32 = Color32::from_rgb(171, 186, 191);
pub(crate) const WARNING: Color32 = Color32::from_rgb(252, 185, 125);

#[derive(Clone, Copy, PartialEq)]
enum Tab { Playing, Queue, Library }

#[derive(Clone, Copy, PartialEq)]
enum SettingsPage { General, App, Display, Outputs, Library, Official }

#[derive(Clone, Copy)]
enum ClearTarget { Moode(u64), Jastreamer(u64) }

struct QueueClear {
    target: ClearTarget,
    animation: dialog::Animation,
    confirmed: bool,
}

impl QueueClear {
    fn new(target: ClearTarget) -> Self { Self { target, animation: dialog::Animation::default(), confirmed: false } }
}

impl SettingsPage {
    const ALL: [Self; 6] = [Self::General, Self::App, Self::Display, Self::Outputs, Self::Library, Self::Official];

    fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::App => crate::APP_NAME,
            Self::Display => "Display",
            Self::Outputs => "Outputs",
            Self::Library => "Library settings",
            Self::Official => "Official moOde settings",
        }
    }
}

pub struct Player {
    connection: Connection,
    fit_bare_x11: Option<egui::Vec2>,
    ja: crate::ja_ui::View,
    showing_ja: bool,
    moode_outputs: Option<Vec<String>>,
    handoff: Handoff,
    queue_clear: Option<QueueClear>,
    track_dialog: Option<TrackDialog>,
    splash: crate::splash::Splash,
    library_activity: Activity,
    settings_page: Option<SettingsPage>,
    startup: crate::startup::Startup,
    settings: crate::settings::View,
    tab: Tab,
    snapshot: Option<Snapshot>,
    received: Instant,
    disconnect: Option<String>,
    message: Option<String>,
    audio_path: Option<MoodePath>,
    cover: Option<TextureHandle>,
    entrance: chrome::Entrance,
    transport: Transport,
    queue: Option<Vec<Track>>,
    library: Library,
    artwork: Artwork,
}

impl Player {
    pub fn new(connection: Connection, fit_bare_x11: Option<egui::Vec2>, ja: crate::ja_ui::View, show_moode: bool) -> Self {
        let settings = crate::settings::View::new(show_moode, fit_bare_x11.is_some(),
            std::env::var_os("JASP_LITE_KIOSK").as_deref() == Some(std::ffi::OsStr::new("1")));
        let showing_ja = !settings.moode_enabled() || ja.start_selected();
        let artwork = Artwork::new(connection.artwork.clone());
        Self {
            connection, fit_bare_x11, tab: Tab::Playing, snapshot: None, received: Instant::now(),
            ja, showing_ja,
            moode_outputs: None,
            handoff: Handoff::default(),
            queue_clear: None,
            track_dialog: None,
            splash: crate::splash::Splash::default(),
            library_activity: Activity::default(),
            startup: crate::startup::Startup::default(),
            settings_page: None, settings,
            disconnect: None, message: None, cover: None, transport: Transport::default(),
            entrance: chrome::Entrance::default(),
            audio_path: None,
            queue: None, library: Library::default(), artwork,
        }
    }

    fn send(&mut self, command: Command) -> bool {
        match self.connection.commands.try_send(command) {
            Ok(()) => { self.message = None; true }
            Err(err) => { self.message = Some(format!("Cannot send command: {err}")); false }
        }
    }

    fn events(&mut self, ctx: &egui::Context) {
        for event in self.connection.events.try_iter() {
            match event {
                Event::Snapshot(value) => {
                    if self.snapshot.as_ref().is_some_and(|prev| prev.library_updating) && !value.library_updating {
                        self.library.invalidate();
                    }
                    if self.snapshot.as_ref().is_some_and(|prev| prev.queue_version != value.queue_version) {
                        self.queue = None;
                    }
                    if self.snapshot.as_ref().is_none_or(|prev| prev.track.file != value.track.file) {
                        self.cover = None;
                        self.transport.reset();
                        if !self.showing_ja && self.tab == Tab::Playing && self.snapshot.is_some() {
                            self.entrance.begin();
                        }
                    }
                    self.audio_path = MoodePath::observed(&value);
                    self.snapshot = Some(value);
                    self.received = Instant::now();
                    self.disconnect = None;
                }
                Event::Queue(queue) => self.queue = Some(queue),
                Event::Outputs(outputs) => self.moode_outputs = Some(outputs),
                Event::LibrarySettings { notify, result } => match result {
                    Ok(sources) => {
                        if notify { self.library_activity.finish(LibraryActivity::Sources, Ok(Some(sources.len()))); }
                        self.settings.set_moode_sources(sources);
                    }
                    Err(error) if notify => self.library_activity.finish(LibraryActivity::Sources, Err(error)),
                    Err(error) => self.message = Some(error),
                },
                Event::LibraryScanned(result) => {
                    if result.is_ok() { self.library.invalidate(); }
                    self.library_activity.finish(LibraryActivity::Scan, result);
                }
                Event::UsbYielded { request, result } => self.handoff.yielded(request, result, &mut self.ja),
                Event::Albums(albums) => self.library.received_albums(albums),
                Event::Entries { heading, entries } => self.library.received_entries(heading, entries),
                Event::LibraryError(error) => self.library.failed(error),
                Event::Thumbnail { key, rgba } => self.artwork.received(ctx, key, rgba),
                Event::Cover { key, rgba, width, height } => {
                    if self.snapshot.as_ref().is_some_and(|s| s.track.file == key) {
                        self.cover = Some(ctx.load_texture("album-cover",
                            egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba),
                            egui::TextureOptions::LINEAR));
                    }
                }
                Event::CoverUnavailable(key) => {
                    if self.snapshot.as_ref().is_some_and(|s| s.track.file == key) { self.cover = None; }
                }
                Event::Error(message) => self.message = Some(message),
                Event::Disconnected(message) => self.disconnect = Some(message),
            }
        }
    }

    fn sidebar(&mut self, ui: &mut Ui, compact: bool) -> Option<bool> {
        let accent = theme::accent(ui);
        let mut switch = None;
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 5.0;
        if !compact {
            ui.label(RichText::new(crate::APP_NAME).size(11.0).strong().color(MUTED));
            ui.add_space(3.0);
        }
        for (jastreamer, icon, label) in [
            (false, RailIcon::Moode, "moOde"),
            (true, RailIcon::Jastreamer, "jastreamer"),
        ] {
            if !jastreamer && !self.settings.moode_enabled() { continue; }
            let tint = self.settings.theme(jastreamer).palette().accent;
            if rail_item(ui, icon, label, self.showing_ja == jastreamer, compact, true, tint).clicked()
                && (self.showing_ja != jastreamer || self.settings_page.is_some()) {
                switch = Some(jastreamer);
            }
        }
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(7.0);
        if !compact {
            ui.label(RichText::new(crate::settings::tr(ui, "BROWSE")).size(10.0).strong().color(MUTED));
            ui.add_space(3.0);
        }
        let shared_open = self.settings_page.is_some();
        if self.showing_ja {
            if self.ja.navigation(ui, compact, shared_open) && shared_open {
                self.settings_page = None;
                self.entrance.begin_in(false);
            }
        } else {
            for (tab, icon, name) in [
                (Tab::Playing, RailIcon::Playing, "Now playing"),
                (Tab::Queue, RailIcon::Queue, "Queue"),
                (Tab::Library, RailIcon::Library, "Library"),
            ] {
                if rail_item(ui, icon, name, !shared_open && self.tab == tab, compact, true, accent).clicked()
                    && (self.tab != tab || shared_open) {
                    self.entrance.begin_in(!shared_open && (tab as u8) > (self.tab as u8));
                    self.settings_page = None;
                    self.tab = tab;
                    if tab == Tab::Queue { self.send(Command::LoadQueue); }
                }
            }
        }
        if rail_item(ui, RailIcon::Settings, "Settings", shared_open, compact, true, accent).clicked() && !shared_open {
            self.enter_settings(SettingsPage::General);
        }
        ui.with_layout(egui::Layout::bottom_up(Align::Min), |ui| {
            if self.showing_ja { self.ja.account(ui, compact); }
            else {
                if rail_item(ui, RailIcon::Refresh, "Refresh", false, compact, true, accent).clicked() {
                    self.send(Command::Refresh);
                }
                if !compact {
                    let (label, tint) = if self.disconnect.is_some() { ("Offline", WARNING) }
                        else if self.snapshot.is_some() { ("Local player", theme::companion(ui)) }
                        else { ("Connecting", MUTED) };
                    ui.add(egui::Label::new(RichText::new(tr(ui, label)).size(12.0).color(tint)).truncate())
                        .on_hover_text(&self.connection.server);
                }
            }
            ui.separator();
        });
        switch
    }

    fn refresh_outputs(&mut self) {
        self.send(Command::ReadOutputs);
        self.ja.scan_usb();
    }

    fn outputs(&mut self, ui: &mut Ui) {
        settings_ui::card(ui, |ui| {
            settings_ui::hint(ui, "This Pi · One player can use the USB DAC at a time.");
            if settings_ui::button(ui, "Rescan USB", false).clicked() { self.refresh_outputs(); }
        });
        let moode_playing = self.snapshot.as_ref().is_some_and(|s| s.state == "play" || s.state == "external");
        if self.settings.moode_enabled() {
            settings_ui::section(ui, "moOde", |ui| {
                match &self.moode_outputs {
                    Some(list) if !list.is_empty() => {
                        settings_ui::hint(ui, "Configured MPD output");
                        ui.label(RichText::new(list.join(", ")).size(17.0).strong());
                    }
                    Some(_) => settings_ui::hint(ui, "No MPD output is enabled."),
                    None => settings_ui::hint(ui, "MPD output not reported."),
                };
                let hint = if self.ja.owns_local() { "Press Play in moOde to switch the USB DAC from jastreamer." }
                    else { "moOde uses its own audio configuration; this screen does not rewrite it." };
                settings_ui::hint(ui, hint);
            });
        }
        settings_ui::section(ui, "jastreamer", |ui| self.ja.outputs(ui, moode_playing));
    }

    fn enter_settings(&mut self, page: SettingsPage) {
        self.entrance.begin_in(self.settings_page.is_none_or(|current| (page as u8) > (current as u8)));
        self.settings_page = Some(page);
        match page {
            SettingsPage::Outputs => self.refresh_outputs(),
            SettingsPage::Library if !self.showing_ja => { self.send(Command::LoadLibrarySettings { notify: false }); }
            _ => {}
        }
    }

    fn settings(&mut self, ui: &mut Ui, page: SettingsPage) {
        let width = if ui.available_width() < 580.0 { 100.0 } else { 148.0 };
        let selection = egui::SidePanel::left("settings-navigation").exact_width(width)
            .resizable(false).show_separator_line(false)
            .frame(egui::Frame::new().fill(theme::card(ui)).corner_radius(12).inner_margin(8))
            .show_inside(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add_space(3.0);
                ui.label(RichText::new(tr(ui, "Settings")).size(15.0).strong().color(MUTED));
                ui.add_space(5.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut selection = None;
                    for item in SettingsPage::ALL {
                        if self.showing_ja && item == SettingsPage::Official { continue; }
                        let label = match item {
                            SettingsPage::Library => "Library",
                            SettingsPage::Official => "moOde settings",
                            _ => item.title(),
                        };
                        if setting_nav_item(ui, label, item == page).clicked() && item != page {
                            selection = Some(item);
                        }
                    }
                    selection
                }).inner
            }).inner;
        if let Some(next) = selection { self.enter_settings(next); }
        let bounds = ui.available_rect_before_wrap();
        let bounds = Rect::from_min_max(bounds.min + vec2(14.0, 0.0), bounds.max);
        let entrance = self.entrance;
        ui.scope_builder(egui::UiBuilder::new().max_rect(bounds), |ui| {
            entrance.show(ui, |ui| self.settings_content(ui, selection.unwrap_or(page)));
        });
    }

    fn settings_content(&mut self, ui: &mut Ui, page: SettingsPage) {
        egui::ScrollArea::vertical().id_salt(page.title()).show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(settings_ui::WIDTH));
            let spacing = ui.spacing_mut();
            spacing.item_spacing = vec2(9.0, 7.0);
            spacing.interact_size.y = 46.0;
            ui.label(RichText::new(tr(ui, page.title())).size(29.0).strong());
            ui.add_space(7.0);
            match page {
                SettingsPage::General => self.settings.general(ui),
                SettingsPage::App => {
                    self.settings.app_options(ui);
                    self.startup.show(ui, !self.connection.read_only);
                }
                SettingsPage::Display => self.settings.display.show(ui),
                SettingsPage::Outputs => self.outputs(ui),
                SettingsPage::Official => self.settings.official(ui, &self.connection.server),
                SettingsPage::Library if self.showing_ja => self.ja.library_settings(ui, &mut self.library_activity),
                SettingsPage::Library => {
                    let pending = self.library_activity.pending();
                    let can_control = self.controls_available();
                    if let Some(command) = self.settings.library(ui, self.snapshot.as_ref(), can_control, pending) {
                        let kind = match &command {
                            Command::ScanLibrary { .. } => LibraryActivity::Scan,
                            _ => LibraryActivity::Sources,
                        };
                        self.library_activity.begin(kind);
                        if !self.send(command) {
                            let error = self.message.take().unwrap_or_default();
                            self.library_activity.finish(kind, Err(error));
                        }
                    }
                }
            }
        });
    }

    fn official_settings_shell(&mut self, ctx: &egui::Context) -> bool {
        self.settings.poll_web();
        if !self.settings.web_open() { return false; }
        let palette = self.settings.theme(self.showing_ja).palette();
        egui::SidePanel::left("official-settings-return")
            .exact_width(crate::settings_web::RAIL_PX / ctx.pixels_per_point()).resizable(false)
            .frame(egui::Frame::new().fill(palette.rail).inner_margin(12)).show(ctx, |ui| {
                let label = RichText::new(tr(ui, "Back to app")).size(16.0).strong().color(theme::accent(ui));
                if ui.add_sized([ui.available_width(), 54.0], egui::Button::new(label).corner_radius(9)).clicked()
                    || ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                    self.settings.close_web();
                    ctx.request_repaint();
                }
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(palette.background)).show(ctx, |ui| {
            ui.centered_and_justified(|ui| ui.label(tr(ui, "Opening moOde settings…")));
        });
        true
    }

    fn playing(&mut self, ui: &mut Ui) {
        let Some(snapshot) = &self.snapshot else {
            ui.label(format!("Waiting for {}", self.connection.server));
            return;
        };
        let state = match snapshot.state.as_str() {
            "play" => "PLAYING", "pause" => "PAUSED", "stop" => "STOPPED", _ => "EXTERNAL SOURCE",
        };
        let output = snapshot.capability_note.as_deref()
            .or(snapshot.source.as_deref()).unwrap_or("moOde");
        let cover = self.cover.as_ref().or_else(|| self.artwork.cached(&snapshot.track.file));
        let path = self.audio_path.as_ref().filter(|_| self.disconnect.is_none()).map(MoodePath::display);
        if chrome::now_playing(ui, &Self::song(snapshot, cover), state, output, "", path, !snapshot.track.file.is_empty()) {
            let track = &snapshot.track;
            self.track_dialog = Some(TrackDialog::new("moOde", &track.title, &track.artist, &track.album,
                if track.duration > 0.0 { track.duration } else { snapshot.duration }, snapshot.track_details.clone()));
        }
    }

    fn song<'a>(state: &'a Snapshot, cover: Option<&'a TextureHandle>) -> Song<'a> {
        Song { title: &state.track.title, artist: &state.track.artist, album: &state.track.album, cover }
    }

    fn transport(&mut self, ui: &mut Ui) {
        let Some(state) = &self.snapshot else { return; };
        let enabled = !self.connection.read_only && self.disconnect.is_none() && !self.ja.owns_local();
        let transport = enabled && state.can_transport;
        let playing = state.state == "play";
        let can_handoff = self.ja.owns_local() && !playing && state.queue_length > 0 && self.can_request_moode_play();
        let position = state.elapsed + if playing && self.disconnect.is_none() {
            self.received.elapsed().as_secs_f64()
        } else { 0.0 };
        let model = Controls {
            elapsed: position, duration: state.duration, playing, can_navigate: transport,
            can_seek: transport && state.state != "external", can_toggle: transport || can_handoff,
            can_set_modes: !self.connection.read_only && self.disconnect.is_none() && state.can_transport && state.state != "external",
            shuffle: state.random, repeat: state.repeat,
            volume: state.volume, muted: state.muted, can_volume: enabled && state.can_volume,
        };
        let cover = self.cover.as_ref().or_else(|| self.artwork.cached(&state.track.file));
        let song = Self::song(state, cover);
        if let Some(action) = self.transport.show(ui, &song, model) {
            if matches!(action, Motion::PlayPause) && can_handoff {
                self.handoff.to_moode(Command::Play);
                return;
            }
            self.send(match action {
                Motion::Previous => Command::Previous,
                Motion::PlayPause => Command::TogglePlayback,
                Motion::Next => Command::Next,
                Motion::Shuffle(value) => Command::SetRandom(value),
                Motion::Repeat(mode) => Command::SetRepeat(mode),
                Motion::Seek(seconds) => Command::Seek(seconds),
                Motion::Volume(value) => Command::SetVolume(value),
            });
        }
    }

    fn queue(&mut self, ui: &mut Ui) {
        let can_play = self.can_request_moode_play();
        if let Some(command) = moode_library::queue(ui, self.queue.as_deref(), self.snapshot.as_ref(),
            self.cover.as_ref(), &mut self.artwork, can_play, can_play)
        {
            self.route_moode_queue(command);
        }
    }

    fn can_request_moode_play(&self) -> bool {
        !self.connection.read_only && self.disconnect.is_none()
            && self.snapshot.as_ref().is_some_and(|s| s.can_transport && s.state != "external")
    }

    fn request_moode_play(&mut self, command: Command) {
        if self.ja.owns_local() { self.handoff.to_moode(command); }
        else { self.send(command); }
    }

    fn controls_available(&self) -> bool {
        !self.connection.read_only && self.disconnect.is_none() && !self.ja.owns_local()
            && self.snapshot.as_ref().is_some_and(|s| s.can_transport)
    }

    fn library(&mut self, ui: &mut Ui) {
        let can_play = self.can_request_moode_play();
        if let Some(command) = self.library.show(ui, &self.connection.commands, &mut self.artwork, can_play, can_play) {
            self.route_moode_queue(command);
        }
    }

    fn route_moode_queue(&mut self, command: Command) {
        match command {
            Command::ClearQueue { revision } => {
                self.queue_clear = Some(QueueClear::new(ClearTarget::Moode(revision)));
            }
            Command::PlayId(_) | Command::PlayPath(_)
                | Command::QueueLibrary { placement: QueuePlacement::Play, .. } => self.request_moode_play(command),
            _ => { self.send(command); }
        }
    }

    fn show_queue_clear(&mut self, ctx: &egui::Context) {
        let Some(target) = self.queue_clear.as_ref().map(|clear| clear.target) else { return; };
        let (ready, title, message) = match target {
            ClearTarget::Moode(revision) => (self.can_request_moode_play()
                && self.snapshot.as_ref().is_some_and(|s| s.queue_version == revision && s.queue_length > 0),
                "Clear the moOde queue?",
                "All moOde queue entries will be removed and playback will stop. Music files are kept."),
            ClearTarget::Jastreamer(revision) => (self.ja.can_clear_queue(revision),
                "Clear the jastreamer queue?",
                "All jastreamer queue entries will be removed. The current track can finish. Music files are kept."),
        };
        let Some(clear) = &mut self.queue_clear else { return; };
        let modal = dialog::show(ctx, egui::Id::new("clear-queue"), &mut clear.animation, |ui| {
            ui.set_width((ctx.content_rect().width() - 56.0).clamp(205.0, 440.0));
            ui.label(RichText::new(tr(ui, title)).size(23.0).strong());
            ui.add_space(6.0);
            ui.label(RichText::new(tr(ui, message)).size(15.0).color(MUTED));
            if !ready { ui.label(RichText::new(tr(ui, "The queue changed. Cancel and review the updated queue.")).color(WARNING)); }
            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let confirm = ui.add_enabled(ready, centered_button(
                    RichText::new(tr(ui, "Clear queue")).size(15.0).strong().color(theme::ON_ACCENT))
                    .fill(WARNING).min_size(vec2(116.0, 43.0)).corner_radius(12)).clicked();
                let cancel = ui.add(centered_button(RichText::new(tr(ui, "Cancel")).size(15.0))
                    .min_size(vec2(82.0, 43.0)).corner_radius(12)).clicked();
                (confirm && ready, cancel)
            }).inner
        });
        if clear.animation.is_open() && (modal.inner.0 || modal.inner.1 || modal.should_close()) {
            clear.confirmed = modal.inner.0;
            clear.animation.start_close();
            ctx.request_repaint();
        }
        if clear.animation.is_finished() {
            let confirmed = clear.confirmed;
            self.queue_clear = None;
            if !confirmed { return; }
            match target {
                ClearTarget::Moode(revision) => { self.send(Command::ClearQueue { revision }); }
                ClearTarget::Jastreamer(revision) => self.ja.clear_queue(revision),
            }
        }
    }
}

impl eframe::App for Player {
    fn raw_input_hook(&mut self, _: &egui::Context, raw: &mut egui::RawInput) {
        self.settings.display.input(raw);
    }

    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.settings.install_language(ctx);
        let theme = self.settings.theme(self.showing_ja);
        theme.install(ctx);
        let palette = theme.palette();
        // A bare Xorg session has no WM to act on Winit's fullscreen request.
        if let Some(size) = self.fit_bare_x11.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size / ctx.pixels_per_point()));
        }
        self.events(ctx);
        self.ja.events(ctx, &mut self.library_activity, self.track_dialog.as_mut());
        self.ja.poll_library_scan(ctx);
        if let Some(notice) = self.ja.take_usb_notice() {
            self.handoff.jastreamer(notice, &mut self.ja, &self.connection.commands);
        }
        self.ja.set_moode_usb_interlock(self.snapshot.as_ref().is_some_and(|state| state.state != "stop"));
        if self.splash.show(ctx, palette) { return; }
        if self.official_settings_shell(ctx) {
            self.settings.display.keep_awake();
            return;
        }
        self.settings.display.tick(ctx);
        self.startup.poll(ctx, !self.connection.read_only);
        let modal_open = self.handoff.active() || self.queue_clear.is_some() || self.track_dialog.is_some() || self.startup.active();
        if !modal_open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !ctx.wants_keyboard_input() {
            match self.settings_page {
                Some(_) => { self.settings_page = None; self.entrance.begin_in(false); }
                None => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
        let frame = egui::Frame::new().fill(palette.background).inner_margin(egui::Margin::symmetric(18, 12));
        if (self.showing_ja && self.ja.has_transport()) || (!self.showing_ja && self.snapshot.is_some()) {
            let transport_frame = egui::Frame::new().fill(palette.footer).inner_margin(egui::Margin::symmetric(16, 8));
            egui::TopBottomPanel::bottom("transport").frame(transport_frame).show(ctx, |ui| {
                if modal_open { ui.disable(); }
                if self.showing_ja { self.ja.transport(ui); } else { self.transport(ui); }
            });
        }
        let moode_status = self.ja.owns_local() || self.disconnect.is_some() || self.message.is_some()
            || self.snapshot.as_ref().is_some_and(|state| state.playback_error.is_some()) || self.connection.read_only;
        let outputs_open = self.settings_page == Some(SettingsPage::Outputs);
        let show_ja_status = self.showing_ja || outputs_open;
        let show_moode_status = !self.showing_ja || outputs_open;
        if (show_ja_status && self.ja.has_status()) || (show_moode_status && moode_status) {
            egui::TopBottomPanel::bottom("status").frame(egui::Frame::new().fill(palette.background)
                .inner_margin(egui::Margin::symmetric(16, 5))).show(ctx, |ui| {
                if show_ja_status { self.ja.status(ui); }
                if !show_moode_status { return; }
                if self.ja.owns_local() && !outputs_open {
                    chrome::status_ticker(ui, "dac", "", &tr(ui, "Press Play in moOde to switch the USB DAC from jastreamer."), WARNING);
                }
                if let Some(error) = &self.disconnect { chrome::status_ticker(ui, "moode-connection", "", error, WARNING); }
                if let Some(error) = &self.message { chrome::status_ticker(ui, "moode-message", "", error, WARNING); }
                if let Some(error) = self.snapshot.as_ref().and_then(|s| s.playback_error.as_deref()) {
                    chrome::status_ticker(ui, "mpd-error", "MPD output: ", error, WARNING);
                }
                if self.connection.read_only {
                    chrome::status_ticker(ui, "moode-read-only", "", "READ ONLY  ·  Playback controls disabled", MUTED);
                }
            });
        }
        let compact = ctx.content_rect().width() < 620.0;
        let switch = egui::SidePanel::left("player-navigation").exact_width(if compact { 62.0 } else { 174.0 })
            .resizable(false).frame(egui::Frame::new().fill(palette.rail).inner_margin(egui::Margin::symmetric(10, 13)))
            .show(ctx, |ui| {
                if modal_open { ui.disable(); }
                self.sidebar(ui, compact)
            }).inner;
        let entrance = self.entrance;
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            if modal_open { ui.disable(); }
            if let Some(page) = self.settings_page {
                self.settings(ui, page);
                return;
            }
            entrance.show(ui, |ui| {
                if self.showing_ja { self.ja.show(ui, &mut self.track_dialog); return; }
                match self.tab {
                    Tab::Playing => { egui::ScrollArea::vertical().show(ui, |ui| self.playing(ui)); }
                    Tab::Queue => self.queue(ui),
                    Tab::Library => self.library(ui),
                }
            });
        });
        if self.settings.web_open() { ctx.request_repaint(); }
        self.library_activity.show(ctx);
        if let Some(play) = self.ja.take_usb_play() { self.handoff.to_jastreamer(play, &mut self.ja); }
        let moode_ready = !self.connection.read_only && self.disconnect.is_none() && self.snapshot.is_some();
        self.handoff.show(ctx, &mut self.ja, moode_ready);
        if let Some(revision) = self.ja.take_queue_clear() {
            self.queue_clear = Some(QueueClear::new(ClearTarget::Jastreamer(revision)));
        }
        if !self.handoff.active() {
            if self.queue_clear.is_some() { self.show_queue_clear(ctx); }
            else if let Some(info) = &mut self.track_dialog {
                if info.show(ctx) { self.track_dialog = None; }
            } else { self.startup.show_prompt(ctx); }
        }
        if let Some(jastreamer) = switch {
            self.showing_ja = jastreamer;
            self.settings_page = None;
            self.entrance.begin_in(jastreamer);
            ctx.request_repaint();
        }
        if !self.settings.moode_enabled() && !self.showing_ja {
            self.showing_ja = true;
            self.entrance.begin_in(true);
            ctx.request_repaint();
        }
    }
}

fn setting_nav_item(ui: &mut Ui, label: &str, selected: bool) -> egui::Response {
    let width = ui.available_width();
    let label = tr(ui, label);
    let text = ui.painter().layout(label.to_string(), FontId::proportional(16.0), Color32::PLACEHOLDER, (width - 20.0).max(1.0));
    let (rect, response) = ui.allocate_exact_size(vec2(width, (text.size().y + 16.0).max(44.0)), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), selected, label.as_ref()));
    let selection = ui.ctx().animate_bool_with_time(response.id.with("selected"), selected, 0.18);
    let focus = ui.ctx().animate_bool_with_time(response.id.with("focus"), response.hovered() || response.has_focus(), 0.13);
    let tint = theme::accent(ui);
    let strength = (selection * 0.18 + focus * 0.09).min(0.25);
    if strength > 0.0 {
        ui.painter().rect_filled(rect, 8, theme::card(ui).lerp_to_gamma(tint, strength));
    }
    if selection > 0.0 {
        let marker = Rect::from_min_size(rect.left_center() - vec2(0.0, 11.0), vec2(3.0, 22.0));
        ui.painter().rect_filled(marker, 2, tint.gamma_multiply(selection));
    }
    let origin = rect.left_center() + vec2(10.0, -text.size().y * 0.5);
    ui.painter().galley(origin, text, ui.visuals().strong_text_color().lerp_to_gamma(tint, selection));
    response
}

#[derive(Clone, Copy)]
pub(crate) enum RailIcon { Moode, Jastreamer, Playing, Queue, Library, Settings, Refresh, SignOut }

pub(crate) fn rail_item(ui: &mut Ui, icon: RailIcon, label: &str, selected: bool, compact: bool,
    enabled: bool, tint: Color32) -> egui::Response {
    let brand = matches!(icon, RailIcon::Moode | RailIcon::Jastreamer);
    let id = ui.make_persistent_id(("rail", label));
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), if brand { 46.0 } else { 43.0 }), Sense::hover());
    let response = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let shown = crate::settings::tr(ui, label);
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, selected, shown.as_ref()));
    let selection = ui.ctx().animate_bool_with_time(id.with("selected"), selected, 0.21);
    let focus = ui.ctx().animate_bool_with_time(id.with("focus"), enabled && (response.hovered() || response.has_focus()), 0.15);
    let press = ui.ctx().animate_bool_with_time(id.with("press"), response.is_pointer_button_down_on(), 0.09);
    let painter = ui.painter();
    let body = rect.shrink(press * 1.6);
    if brand { painter.rect_filled(body, 9, theme::raised(ui).gamma_multiply(0.64)); }
    if selection > 0.0 || focus > 0.0 {
        painter.rect_filled(body, 9, tint.gamma_multiply(selection * 0.14 + focus * 0.07));
    }
    if selection > 0.0 {
        let mark = Rect::from_center_size(pos2(rect.left() + 2.5, rect.center().y), vec2(3.0, rect.height() * 0.64 * selection));
        painter.rect_filled(mark.expand(2.0), 4, tint.gamma_multiply(selection * 0.18));
        painter.rect_filled(mark, 3, tint.gamma_multiply(selection));
    }
    let color = if enabled { MUTED.lerp_to_gamma(tint, selection.max(focus * 0.44)) } else { MUTED.gamma_multiply(0.44) };
    let center = pos2(if compact { rect.center().x } else { rect.left() + 24.0 }, rect.center().y);
    if brand {
        painter.rect_filled(Rect::from_center_size(center, vec2(29.0, 29.0)), 8, tint.gamma_multiply(0.13 + selection * 0.11));
    }
    paint_rail_icon(painter, center, icon, if brand && enabled { tint } else { color });
    if !compact {
        painter.text(pos2(rect.left() + 48.0, rect.center().y), Align2::LEFT_CENTER, shown.as_ref(),
            FontId::proportional(if brand { 17.0 } else { 15.0 }), color);
    }
    response.on_hover_text(shown.as_ref())
}

fn paint_rail_icon(painter: &Painter, center: egui::Pos2, icon: RailIcon, tint: Color32) {
    let stroke = Stroke::new(1.55, tint);
    let pt = |x, y| center + vec2(x, y);
    let line = |ax, ay, bx, by| { painter.line_segment([pt(ax, ay), pt(bx, by)], stroke); };
    let block = |x, y, w, h| Rect::from_min_size(pt(x, y), vec2(w, h));
    match icon {
        RailIcon::Moode => {
            painter.circle_stroke(center, 8.0, stroke);
            painter.circle_stroke(center, 4.7, stroke);
            painter.circle_filled(center, 1.55, tint);
        }
        RailIcon::Jastreamer => {
            for (i, height) in [8.0, 16.0, 11.0].into_iter().enumerate() {
                let x = -7.0 + i as f32 * 5.0;
                painter.rect_filled(block(x, -height / 2.0, 3.0, height), 2, tint);
            }
        }
        RailIcon::Playing => {
            line(-5.0, -8.0, 7.0, 0.0); line(7.0, 0.0, -5.0, 8.0); line(-5.0, 8.0, -5.0, -8.0);
        }
        RailIcon::Queue => for y in [-6.0, 0.0, 6.0] {
            painter.circle_filled(pt(-7.0, y), 1.25, tint);
            line(-3.0, y, 8.0, y);
        },
        RailIcon::Library => {
            for (x, y) in [(-8.0, -8.0), (1.0, -8.0), (-8.0, 1.0), (1.0, 1.0)] {
                painter.rect_stroke(block(x, y, 7.0, 7.0), 2, stroke, egui::StrokeKind::Inside);
            }
        }
        RailIcon::Settings => {
            painter.circle_stroke(center, 5.8, stroke);
            painter.circle_stroke(center, 2.1, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                let axis = vec2(a.cos(), a.sin());
                painter.line_segment([center + axis * 6.7, center + axis * 8.5], stroke);
            }
        }
        RailIcon::Refresh => {
            painter.circle_stroke(center, 6.5, stroke);
            line(6.0, -3.0, 7.0, -7.5); line(6.0, -3.0, 2.2, -4.0);
        }
        RailIcon::SignOut => {
            line(-8.0, -8.0, -8.0, 8.0); line(-8.0, -8.0, 0.0, -8.0); line(-8.0, 8.0, 0.0, 8.0);
            line(-4.0, 0.0, 8.0, 0.0); line(8.0, 0.0, 4.0, -4.0); line(8.0, 0.0, 4.0, 4.0);
        }
    }
}

pub(crate) fn button(ui: &mut Ui, text: &str, selected: bool) -> egui::Response {
    let shown = crate::settings::tr(ui, text);
    button_text(ui, shown.as_ref(), selected)
}

pub(crate) fn centered_button(text: RichText) -> egui::Button<'static> {
    // Balance the spare width on either side, regardless of the parent layout.
    egui::Button::new((egui::Atom::grow(), text, egui::Atom::grow())).wrap()
}

fn button_text(ui: &mut Ui, text: &str, selected: bool) -> egui::Response {
    let phase = ui.ctx().animate_bool_with_time(ui.next_auto_id().with("selected"), selected, 0.14);
    let color = if phase >= 0.48 { theme::ON_ACCENT } else { Color32::WHITE };
    ui.add(centered_button(RichText::new(text).color(color).size(15.0).strong())
        .fill(theme::raised(ui).lerp_to_gamma(theme::accent(ui), phase)).corner_radius(7).min_size(vec2(58.0, 44.0)))
}

pub(crate) fn art(ui: &mut Ui, cover: Option<&TextureHandle>, side: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(side, side), egui::Sense::hover());
    paint_art(ui, rect, cover);
}

pub(crate) fn paint_art(ui: &Ui, rect: Rect, cover: Option<&TextureHandle>) {
    let side = rect.width();
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, theme::raised(ui));
    if let Some(cover) = cover {
        let size = cover.size_vec2();
        let fit = size * (side / size.x.max(size.y));
        painter.image(cover.id(), egui::Rect::from_center_size(rect.center(), fit),
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
    } else {
        for fraction in [0.33, 0.27, 0.21] {
            painter.circle_stroke(rect.center(), side * fraction, egui::Stroke::new(1.0, MUTED.gamma_multiply(0.35)));
        }
        painter.circle_filled(rect.center(), side * 0.06, theme::accent(ui).gamma_multiply(0.5));
    }
}


