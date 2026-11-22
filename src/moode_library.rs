use super::{MUTED, WARNING, button, paint_art};
use crate::theme;
use crate::chrome;
use crate::ja_ui::Keyboard;
use crate::model::{Album, Command, Entry, EntryKind, QueuePlacement, QueueSource, Snapshot, Track};
use crate::settings::tr;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, Ui, pos2, vec2};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::time::Duration;

const ROW_HEIGHT: f32 = 57.0;
const CACHE_LIMIT: usize = 128;
const ART_IN_FLIGHT: usize = 8;

struct CachedArt {
    texture: Option<TextureHandle>,
    used: u64,
}

pub(super) struct Artwork {
    requests: SyncSender<String>,
    pending: HashSet<String>,
    cache: HashMap<String, CachedArt>,
    clock: u64,
}

impl Artwork {
    pub(super) fn new(requests: SyncSender<String>) -> Self {
        Self { requests, pending: HashSet::new(), cache: HashMap::new(), clock: 0 }
    }

    pub(super) fn cached(&self, key: &str) -> Option<&TextureHandle> {
        self.cache.get(key)?.texture.as_ref()
    }

    fn get(&mut self, ui: &Ui, key: &str) -> Option<&TextureHandle> {
        self.clock += 1;
        if let Some(art) = self.cache.get_mut(key) {
            art.used = self.clock;
            return art.texture.as_ref();
        }
        if !key.is_empty() && !key.contains("://")
            && self.pending.len() < ART_IN_FLIGHT && !self.pending.contains(key)
        {
            match self.requests.try_send(key.to_owned()) {
                Ok(()) => { self.pending.insert(key.to_owned()); }
                Err(TrySendError::Full(_)) => ui.ctx().request_repaint_after(Duration::from_millis(120)),
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
        None
    }

    pub(super) fn received(&mut self, ctx: &egui::Context, key: String, rgba: Option<(Vec<u8>, usize, usize)>) {
        if !self.pending.remove(&key) { return; }
        if self.cache.len() >= CACHE_LIMIT {
            let oldest = self.cache.iter().min_by_key(|(_, art)| art.used).map(|(key, _)| key.clone());
            if let Some(oldest) = oldest { self.cache.remove(&oldest); }
        }
        self.clock += 1;
        let texture = rgba.map(|(rgba, width, height)| ctx.load_texture("moode-thumbnail",
            egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba), egui::TextureOptions::LINEAR));
        self.cache.insert(key, CachedArt { texture, used: self.clock });
        ctx.request_repaint();
    }
}

#[derive(Default, PartialEq, Eq, Hash, Clone)]
enum Page {
    #[default]
    Albums,
    Album(usize),
    Folders(String),
    Playlists,
    Playlist(String),
    Search(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Collection { Albums, Folders, Playlists }

impl Collection {
    fn root(self) -> Page {
        match self {
            Self::Albums => Page::Albums,
            Self::Folders => Page::Folders(String::new()),
            Self::Playlists => Page::Playlists,
        }
    }
}

impl Page {
    fn collection(&self) -> Option<Collection> {
        match self {
            Self::Albums | Self::Album(_) => Some(Collection::Albums),
            Self::Folders(_) => Some(Collection::Folders),
            Self::Playlists | Self::Playlist(_) => Some(Collection::Playlists),
            Self::Search(_) => None,
        }
    }

    fn command(&self) -> Option<Command> {
        Some(match self {
            Self::Albums => Command::ListAlbums,
            Self::Album(_) => return None,
            Self::Folders(path) => Command::Browse(path.clone()),
            Self::Playlists => Command::ListPlaylists,
            Self::Playlist(path) => Command::OpenPlaylist(path.clone()),
            Self::Search(query) => Command::Search(query.clone()),
        })
    }
}

struct Pending {
    page: Page,
    invalidated: bool,
}

#[derive(Default)]
pub(super) struct Library {
    page: Page,
    back: Vec<Page>,
    folder_motion: chrome::FolderMotion,
    pending: Option<Pending>,
    albums: Option<Vec<Album>>,
    listing: Option<(String, Vec<Entry>)>,
    error: Option<String>,
    search: String,
    keyboard: Keyboard,
    show_keys: bool,
}

impl Library {
    pub(super) fn invalidate(&mut self) {
        if let Some(pending) = &mut self.pending { pending.invalidated = true; }
        self.albums = None;
        self.listing = None;
        self.error = None;
        self.back.retain(|page| !matches!(page, Page::Album(_)));
        if matches!(self.page, Page::Album(_)) {
            self.page = Page::Albums;
            self.back.clear();
        }
    }

    pub(super) fn received_albums(&mut self, albums: Vec<Album>) {
        if self.pending.as_ref().is_some_and(|pending| pending.page == Page::Albums)
            && let Some(pending) = self.pending.take() && !pending.invalidated
        {
            self.albums = Some(albums);
        }
    }

    pub(super) fn received_entries(&mut self, heading: String, entries: Vec<Entry>) {
        if self.pending.as_ref().is_some_and(|pending| pending.page != Page::Albums)
            && let Some(pending) = self.pending.take()
            && !pending.invalidated && pending.page == self.page
        {
            self.listing = Some((heading, entries));
            self.folder_motion.arrived();
        }
    }

    pub(super) fn failed(&mut self, error: String) {
        if let Some(pending) = self.pending.take() && !pending.invalidated && pending.page == self.page {
            self.error = Some(error);
        }
    }

    fn set_page(&mut self, page: Page, forward: bool) {
        self.page = page;
        self.reset_listing(forward);
    }

    fn reset_listing(&mut self, forward: bool) {
        self.listing = None;
        self.error = None;
        if matches!(self.page, Page::Folders(_)) { self.folder_motion.prepare(forward); }
        else { self.folder_motion = chrome::FolderMotion::Idle; }
    }

    fn open(&mut self, page: Page) {
        if page != self.page {
            self.back.push(std::mem::replace(&mut self.page, page));
            self.reset_listing(true);
        }
    }

    fn back(&mut self) {
        if let Some(page) = self.back.pop() { self.set_page(page, false); }
    }

    fn request(&mut self, commands: &SyncSender<Command>) {
        // MPD runs queries sequentially. Keep one in flight even if the user
        // changes page; only apply its result to the page that requested it.
        if self.pending.is_some() || self.error.is_some() || self.listing.is_some()
            || (self.page == Page::Albums && self.albums.is_some()) { return; }
        if let Some(command) = self.page.command() {
            match commands.try_send(command) {
                Ok(()) => self.pending = Some(Pending { page: self.page.clone(), invalidated: false }),
                Err(error) => self.error = Some(format!("Could not request the library: {error}")),
            }
        }
    }

    pub(super) fn show(&mut self, ui: &mut Ui, commands: &SyncSender<Command>, art: &mut Artwork,
        can_play: bool, can_edit: bool) -> Option<Command> {
        let mut submit = false;
        let search_controls = |ui: &mut Ui, this: &mut Self, submit: &mut bool| {
            let field = ui.add_sized(vec2(245.0_f32.min(ui.available_width()), 44.0),
                egui::TextEdit::singleline(&mut this.search).hint_text(tr(ui, "Search music"))
                    .font(FontId::proportional(16.0)).margin(vec2(9.0, 9.0)));
            *submit |= field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if button(ui, if this.show_keys { "Hide keys" } else { "Keyboard" }, this.show_keys).clicked() {
                this.show_keys = !this.show_keys;
            }
            *submit |= button(ui, "Search", false).clicked();
        };
        if ui.available_width() >= 620.0 {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(ui, "Library")).size(29.0).strong());
                ui.add_space((ui.available_width() - 428.0).max(0.0));
                search_controls(ui, self, &mut submit);
            });
        } else {
            ui.label(RichText::new(tr(ui, "Library")).size(29.0).strong());
            ui.horizontal_wrapped(|ui| search_controls(ui, self, &mut submit));
        }
        if self.show_keys {
            let height = (ui.available_height() * 0.57).max(50.0);
            egui::Frame::new().fill(theme::card(ui)).corner_radius(10).inner_margin(8).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("moode-keyboard").max_height(height).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    self.show_keys = self.keyboard.show_search(ui, &mut self.search);
                });
            });
        }
        if submit && !self.search.trim().is_empty() {
            self.show_keys = false;
            let result_page = Page::Search(self.search.trim().to_owned());
            if matches!(self.page, Page::Search(_)) {
                if self.page != result_page { self.set_page(result_page, true); }
            } else {
                self.open(result_page);
            }
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for (category, name) in [(Collection::Albums, "Albums"), (Collection::Folders, "Folders"),
                (Collection::Playlists, "Playlists")]
            {
                if button(ui, name, self.page.collection() == Some(category)).clicked() {
                    let root = category.root();
                    if self.page != root {
                        let returning = matches!(&self.page, Page::Folders(_)) && category == Collection::Folders;
                        self.back.clear();
                        self.set_page(root, !returning);
                    }
                }
            }
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(9.0);
        if !matches!(&self.page, Page::Folders(_)) {
            if !self.back.is_empty() && button(ui, "‹  Back", false).clicked() { self.back(); }
            ui.add_space(8.0);
        }

        self.request(commands);
        let folder_frame = matches!(self.page, Page::Folders(_)).then(|| self.folder_motion.frame(ui, theme::companion(ui)));
        if folder_frame.is_none() && library_error(ui, &mut self.error) { return None; }
        match &self.page {
            Page::Albums => {
                if let Some(albums) = &self.albums {
                    if albums.is_empty() { empty(ui, "No tagged albums. Browse Folders for other music sources."); }
                    else if let Some(index) = album_grid(ui, albums, art) {
                        self.open(Page::Album(index));
                        ui.ctx().request_repaint();
                    }
                } else { loading(ui, "Loading albums…"); }
            }
            Page::Album(index) => {
                if let Some(album) = self.albums.as_ref().and_then(|albums| albums.get(*index)) {
                    if let Some(placement) = chrome::library_header(ui, "Play all", can_play,
                        can_edit && !album.tracks.is_empty(), |ui| album_heading(ui, album, art)) {
                        let source = QueueSource::Album(album.tracks.iter().map(|track| track.path.clone()).collect());
                        return Some(Command::QueueLibrary { placement, source });
                    }
                    return self.show_entries(ui, &album.tracks, art, Some(&album.cover_file), can_play, can_edit, None).command;
                }
            }
            page => {
                let title = match page {
                    Page::Folders(path) if path.is_empty() => tr(ui, "Music sources"),
                    Page::Folders(path) => basename(path).into(),
                    Page::Playlists => tr(ui, "Stored playlists"),
                    Page::Playlist(path) => basename(path).into(),
                    Page::Search(_) => self.listing.as_ref().map_or_else(|| tr(ui, "Search results"),
                        |(heading, _)| heading.as_str().into()),
                    _ => unreachable!(),
                };
                let entries = self.listing.as_ref().map(|(_, entries)| entries.as_slice());
                let bulk = match page {
                    Page::Folders(path) if !path.is_empty() => Some((path, EntryKind::Directory)),
                    Page::Playlist(path) => Some((path, EntryKind::Playlist)),
                    _ => None,
                };
                let folder_back = matches!(page, Page::Folders(path) if !path.is_empty()) && !self.back.is_empty();
                let mut go_back = false;
                let mut heading = |ui: &mut Ui| {
                    if let Some(frame) = folder_frame {
                        if folder_back { go_back = chrome::library_back(ui); }
                        frame.title(ui, title.as_ref());
                    }
                    else {
                        ui.add(egui::Label::new(RichText::new(title.as_ref()).size(22.0).strong())
                            .truncate()).on_hover_text(title.as_ref());
                    }
                };
                if let Some((path, kind)) = bulk {
                    let radio = is_radio(path);
                    let has_tracks = entries.is_some_and(|entries| entries.iter()
                        .any(|entry| matches!(entry.kind, EntryKind::Track | EntryKind::Directory)));
                    let editable = can_edit && (has_tracks || radio);
                    let placement = chrome::library_header(ui, if radio { "Play" } else { "Play all" },
                        can_play, editable, heading);
                    if go_back {
                        self.back();
                        ui.ctx().request_repaint();
                        return None;
                    }
                    if let Some(placement) = placement { return Some(queue_library(path, kind, placement)); }
                } else {
                    heading(ui);
                    ui.add_space(5.0);
                }
                if folder_frame.is_some() && library_error(ui, &mut self.error) { return None; }
                if let Some(entries) = entries {
                    if entries.is_empty() {
                        empty(ui, match page {
                            Page::Search(_) => "No matching tracks.",
                            Page::Playlists => "No stored playlists.",
                            _ => "No items in this location.",
                        });
                    } else {
                        let action = self.show_entries(ui, entries, art, None, can_play, can_edit, folder_frame);
                        if let Some(page) = action.open {
                            self.open(page);
                            ui.ctx().request_repaint();
                        }
                        return action.command;
                    }
                } else { loading(ui, "Loading library…"); }
            }
        }
        None
    }

    fn show_entries(&self, ui: &mut Ui, entries: &[Entry], art: &mut Artwork, album_art: Option<&str>,
        can_play: bool, can_edit: bool, folder_frame: Option<chrome::FolderFrame>) -> EntryAction {
        let mut action = EntryAction::default();
        egui::ScrollArea::vertical().id_salt(("moode-entries", &self.page)).auto_shrink([false, false])
            .show_rows(ui, ROW_HEIGHT, entries.len(), |ui, range| {
                for (rank, index) in range.enumerate() {
                    let entry = &entries[index];
                    let row = |ui: &mut Ui| {
                        let menu = match entry.kind {
                            EntryKind::Track => {
                                let cover = art.get(ui, album_art.filter(|key| !key.is_empty()).unwrap_or(&entry.path));
                                let rect = paint_track_row(ui, MediaRow {
                                    position: album_art.is_some().then_some(index + 1), title: &entry.label,
                                    artist: &entry.artist, album: if album_art.is_some() { "" } else { &entry.album },
                                    duration: entry.duration, cover, status: None,
                                }, None);
                                row_menu_button(ui, row_menu_rect(rect), "Queue")
                            }
                            EntryKind::Directory | EntryKind::Playlist => {
                                let (open, menu) = container_row(ui, entry);
                                if open {
                                    action.open = Some(if entry.kind == EntryKind::Directory {
                                        Page::Folders(entry.path.clone())
                                    } else { Page::Playlist(entry.path.clone()) });
                                }
                                menu
                            }
                        };
                        egui::Popup::menu(&menu).show(|ui| {
                            if let Some(placement) = placement_buttons(ui, "Play now", can_play, can_edit) {
                                action.command = Some(queue_library(&entry.path, entry.kind, placement));
                                ui.close();
                            }
                        });
                    };
                    if let Some(frame) = folder_frame { frame.row(ui, index, rank, ROW_HEIGHT, row); }
                    else { ui.push_id(index, row); }
                }
            });
        action
    }
}

#[derive(Default)]
struct EntryAction { open: Option<Page>, command: Option<Command> }

fn is_radio(path: &str) -> bool { path.starts_with("RADIO/") && path.ends_with(".pls") }

fn queue_library(path: &str, kind: EntryKind, placement: QueuePlacement) -> Command {
    let radio = is_radio(path);
    if radio && placement == QueuePlacement::Play { return Command::PlayPath(path.to_owned()); }
    let source = match kind {
        EntryKind::Directory => QueueSource::Folder(path.to_owned()),
        EntryKind::Track if !radio => QueueSource::Track(path.to_owned()),
        _ => QueueSource::Playlist(path.to_owned()),
    };
    Command::QueueLibrary { placement, source }
}

fn action_button(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    let label = tr(ui, label);
    ui.add_enabled(enabled, egui::Button::new(RichText::new(label).size(15.0).strong())
        .fill(theme::raised(ui)).corner_radius(7).min_size(vec2(64.0, 44.0)))
}

fn placement_buttons(ui: &mut Ui, play_label: &str, can_play: bool, can_edit: bool) -> Option<QueuePlacement> {
    let mut placement = None;
    for (label, value, enabled) in [
        (play_label, QueuePlacement::Play, can_edit && can_play),
        ("Play next", QueuePlacement::Next, can_edit),
        ("Add to queue", QueuePlacement::End, can_edit),
    ] {
        if action_button(ui, label, enabled).clicked() { placement = Some(value); }
    }
    placement
}

fn library_error(ui: &mut Ui, error: &mut Option<String>) -> bool {
    let Some(message) = error.as_ref() else { return false; };
    ui.label(RichText::new(message).color(WARNING));
    if button(ui, "Retry", false).clicked() {
        *error = None;
        ui.ctx().request_repaint();
    }
    true
}

fn album_grid(ui: &mut Ui, albums: &[Album], art: &mut Artwork) -> Option<usize> {
    let width = (ui.available_width() - 12.0).max(1.0);
    let gap = ui.spacing().item_spacing.x;
    let columns = ((width + gap) / (164.0 + gap)).floor().max(1.0) as usize;
    let cell = ((width - gap * (columns - 1) as f32) / columns as f32).max(1.0);
    let side = (cell - 14.0).max(1.0);
    let height = side + 58.0;
    let mut selected = None;
    let unknown_artist = tr(ui, "Unknown artist");
    ui.label(RichText::new(format!("{} {}", albums.len(), tr(ui, "albums"))).size(13.0).color(MUTED));
    egui::ScrollArea::vertical().id_salt("moode-album-grid").auto_shrink([false, false])
        .show_rows(ui, height, albums.len().div_ceil(columns), |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    for index in row * columns..((row + 1) * columns).min(albums.len()) {
                        let album = &albums[index];
                        let (rect, response) = ui.push_id(index, |ui|
                            ui.allocate_exact_size(vec2(cell, height), Sense::click())).inner;
                        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &album.title));
                        if ui.is_rect_visible(rect) {
                            chrome::focus_fill(ui, &response, theme::card(ui), theme::companion(ui).gamma_multiply(0.13));
                            ui.painter().rect_stroke(rect, 9, Stroke::new(1.0, theme::raised(ui)), egui::StrokeKind::Inside);
                            let cover = art.get(ui, &album.cover_file);
                            let image = Rect::from_min_size(rect.left_top() + vec2(7.0, 7.0), vec2(side, side));
                            paint_art(ui, image, cover);
                            if cover.is_some() { chrome::cover_halo(ui, image, theme::companion(ui), response.hovered()); }
                            line(ui, rect.left_top() + vec2(8.0, side + 11.0), cell - 16.0, &album.title, 16.0, Color32::WHITE);
                            line(ui, rect.left_top() + vec2(8.0, side + 33.0), cell - 16.0,
                                fallback(&album.artist, unknown_artist.as_ref()), 13.0, MUTED);
                        }
                        if response.on_hover_text(&album.title).clicked() { selected = Some(index); }
                    }
                });
            }
        });
    selected
}

fn album_heading(ui: &mut Ui, album: &Album, art: &mut Artwork) {
    if ui.available_width() >= 145.0 {
        let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
        let cover = art.get(ui, &album.cover_file);
        paint_art(ui, rect, cover);
    }
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 42.0), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.add(egui::Label::new(RichText::new(&album.title).size(20.0).strong()).truncate())
            .on_hover_text(&album.title);
        let artist = tr(ui, "Unknown artist");
        let details = format!("{}  ·  {}", fallback(&album.artist, artist.as_ref()), track_count(ui, album.tracks.len()));
        ui.add(egui::Label::new(RichText::new(&details).size(13.0).color(MUTED)).truncate()).on_hover_text(details);
    });
}

pub(super) fn queue(ui: &mut Ui, queue: Option<&[Track]>, snapshot: Option<&Snapshot>, current_cover: Option<&TextureHandle>,
    art: &mut Artwork, can_play: bool, can_edit: bool) -> Option<Command> {
    let current_id = snapshot.and_then(|state| state.track.id);
    let current = current_id.and_then(|id| queue?.iter().position(|track| track.id == Some(id)));
    let revision = snapshot.map(|state| state.queue_version);
    let mut jump = false;
    let mut selected = None;
    let clear_revision = revision.filter(|_| can_edit && queue.is_some_and(|queue| !queue.is_empty()));
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr(ui, "Play queue")).size(29.0).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if action_button(ui, "Clear queue", clear_revision.is_some()).clicked()
                && let Some(revision) = clear_revision {
                selected = Some(Command::ClearQueue { revision });
            }
            jump = action_button(ui, "Jump to current", current.is_some()).clicked();
            if let Some(queue) = queue { ui.label(RichText::new(track_count(ui, queue.len())).color(MUTED)); }
        });
    });
    ui.add_space(5.0);
    if let Some(state) = snapshot {
        current_queue_card(ui, state, current, current_cover, art);
    }
    ui.add_space(7.0);
    let Some(queue) = queue else { loading(ui, "Loading play queue…"); return None; };
    if queue.is_empty() { empty(ui, "The play queue is empty."); return None; }
    let mut scroll = egui::ScrollArea::vertical().id_salt("moode-queue").auto_shrink([false, false]);
    if let Some(index) = current.filter(|_| jump) {
        scroll = scroll.vertical_scroll_offset(index as f32 * (ROW_HEIGHT + ui.spacing().item_spacing.y));
    }
    scroll.show_rows(ui, ROW_HEIGHT, queue.len(), |ui, range| {
        for index in range {
            let track = &queue[index];
            let is_current = Some(index) == current;
            let status = snapshot.filter(|_| is_current).map(|state| queue_status(&state.state));
            let cover = current_cover.filter(|_| is_current && snapshot.is_some_and(|state| state.track.file == track.file))
                .or_else(|| art.get(ui, &track.file));
            ui.push_id(track.id.map_or((false, index), |id| (true, id as usize)), |ui| {
                let inline = ui.available_width() >= 690.0;
                let rect = paint_track_row(ui, MediaRow {
                    position: Some(index + 1), title: &track.title, artist: &track.artist, album: &track.album,
                    duration: track.duration, cover, status,
                }, inline.then_some(189.0));
                let Some(id) = track.id else { return; };
                let editable = revision.filter(|_| can_edit && !matches!(status, Some("PLAYING" | "PAUSED")));
                let actions = [
                    ("▶", "Play", can_play && status != Some("PLAYING")),
                    ("↑", "Move earlier", editable.is_some() && index > 0),
                    ("↓", "Move later", editable.is_some() && index + 1 < queue.len()),
                    ("×", "Remove from queue", editable.is_some()),
                ];
                let action = if inline {
                    let mut clicked = None;
                    for (i, &(icon, label, enabled)) in actions.iter().enumerate() {
                        let hit = Rect::from_min_size(pos2(rect.right() - 189.0 + i as f32 * 46.0, rect.top() + 7.0), vec2(42.0, 42.0));
                        if chrome::control_at(ui, hit, icon, label, enabled) { clicked = Some(i); }
                    }
                    clicked
                } else {
                    let menu = row_menu_button(ui, row_menu_rect(rect), "Actions");
                    egui::Popup::menu(&menu).show(|ui| {
                        let mut clicked = None;
                        for (i, &(_, label, enabled)) in actions.iter().enumerate() {
                            if action_button(ui, label, enabled).clicked() {
                                clicked = Some(i);
                            }
                        }
                        if clicked.is_some() { ui.close(); }
                        clicked
                    }).and_then(|popup| popup.inner)
                };
                if let Some(action) = action {
                    selected = match action {
                        0 => Some(Command::PlayId(id)),
                        1 | 2 => editable.map(|revision| Command::MoveQueueEntry { id, earlier: action == 1, revision }),
                        _ => editable.map(|revision| Command::RemoveQueueEntry { id, revision }),
                    };
                }
            });
        }
    });
    selected
}

fn current_queue_card(ui: &mut Ui, state: &Snapshot, current: Option<usize>, current_cover: Option<&TextureHandle>,
    art: &mut Artwork) {
    let status = queue_status(&state.state);
    let track = &state.track;
    if track.file.is_empty() && track.title.is_empty() {
        chrome::playback_badge(ui, status);
        return;
    }
    let texture = current_cover.or_else(|| art.get(ui, &track.file));
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 77.0), Sense::hover());
    let painter = ui.painter();
    let tint = theme::companion(ui);
    painter.rect_filled(rect, 10, theme::card(ui));
    painter.rect_stroke(rect, 10, Stroke::new(1.0, theme::line(ui)), egui::StrokeKind::Inside);
    painter.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 8.0), vec2(3.0, 61.0)), 2, tint);
    paint_art(ui, Rect::from_min_size(rect.left_top() + vec2(12.0, 9.0), vec2(59.0, 59.0)), texture);
    let signal = state.state == "play" && rect.width() >= 320.0;
    if signal {
        chrome::signal(ui, Rect::from_center_size(rect.right_center() - vec2(24.0, 0.0), vec2(24.0, 23.0)), tint);
    }
    let width = rect.width() - if signal { 126.0 } else { 94.0 };
    let status = tr(ui, status);
    let header = current.map(|i| format!("{status}   ·   {} {}", tr(ui, "Queue position"), i + 1));
    let unknown = tr(ui, "Untitled track");
    let detail = (!track.artist.is_empty() && !track.album.is_empty())
        .then(|| format!("{}  ·  {}", track.artist, track.album));
    line(ui, rect.left_top() + vec2(83.0, 8.0), width, header.as_deref().unwrap_or(&status), 12.0, tint);
    line(ui, rect.left_top() + vec2(83.0, 24.0), width, fallback(&track.title, &unknown), 22.0, Color32::WHITE);
    line(ui, rect.left_top() + vec2(83.0, 53.0), width,
        detail.as_deref().unwrap_or(fallback(&track.artist, &track.album)), 13.0, MUTED);
}

fn queue_status(state: &str) -> &'static str {
    match state {
        "play" => "PLAYING",
        "pause" => "PAUSED",
        "external" => "EXTERNAL SOURCE",
        _ => "STOPPED",
    }
}

struct MediaRow<'a> {
    position: Option<usize>,
    title: &'a str,
    artist: &'a str,
    album: &'a str,
    duration: f64,
    cover: Option<&'a TextureHandle>,
    status: Option<&'static str>,
}

fn paint_track_row(ui: &mut Ui, row: MediaRow<'_>, inline_controls: Option<f32>) -> Rect {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());
    let painter = ui.painter();
    if row.status.is_some() {
        painter.rect_filled(rect, 8, theme::companion(ui).gamma_multiply(0.15));
        painter.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 5.0), vec2(3.0, ROW_HEIGHT - 10.0)), 2, theme::companion(ui));
    } else {
        painter.hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(1.0, theme::raised(ui)));
    }
    let numbered = row.position.is_some() && rect.width() >= 300.0;
    if numbered && let Some(position) = row.position {
        let marker = rect.left_center() + vec2(23.0, 0.0);
        if row.status == Some("PLAYING") {
            chrome::signal(ui, Rect::from_center_size(marker, vec2(21.0, 17.0)), theme::accent(ui));
        } else {
            painter.text(marker, Align2::CENTER_CENTER, position.to_string(), FontId::monospace(13.0),
                if row.status.is_some() { theme::companion(ui) } else { MUTED });
        }
    }
    let cover_x = if numbered { 43.0 } else { 7.0 };
    let show_cover = rect.width() >= 210.0;
    if show_cover {
        paint_art(ui, Rect::from_min_size(rect.left_top() + vec2(cover_x, 6.0), vec2(45.0, 45.0)), row.cover);
    }
    let controls_left = inline_controls.map_or_else(|| row_menu_rect(rect).left(), |width| rect.right() - width);
    let text_x = rect.left() + if show_cover { cover_x + 55.0 } else { 9.0 };
    let time_x = controls_left - 12.0;
    let show_time = rect.width() >= 375.0;
    let wide = rect.width() >= 600.0;
    let meta_x = rect.left() + rect.width() * 0.58;
    let title_end = if wide { meta_x } else if show_time { time_x - 54.0 } else { controls_left };
    let unknown = tr(ui, "Untitled track");
    let title = fallback(row.title, unknown.as_ref());
    line(ui, pos2(text_x, rect.top() + 6.0), title_end - text_x - 10.0, title, 17.0,
        if row.status.is_some() { theme::companion(ui) } else { Color32::WHITE });
    let subtitle = if !wide && let Some(status) = row.status { tr(ui, status) }
        else if row.artist.is_empty() { row.album.into() } else { row.artist.into() };
    line(ui, pos2(text_x, rect.top() + 31.0), title_end - text_x - 10.0, subtitle.as_ref(), 13.0,
        if !wide && row.status.is_some() { theme::companion(ui) } else { MUTED });
    if wide {
        let width = (time_x - 58.0) - meta_x;
        if !row.artist.is_empty() { line(ui, pos2(meta_x, rect.top() + 7.0), width, row.album, 14.0, MUTED); }
        if let Some(status) = row.status { line(ui, pos2(meta_x, rect.top() + 30.0), width, tr(ui, status).as_ref(), 12.0, theme::companion(ui)); }
    }
    if show_time {
        painter.text(pos2(time_x, rect.center().y), Align2::RIGHT_CENTER, track_duration(row.duration),
            FontId::monospace(13.0), MUTED);
    }
    rect
}

fn container_row(ui: &mut Ui, entry: &Entry) -> (bool, egui::Response) {
    let (row, hit) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());
    let menu = row_menu_rect(row);
    let rect = Rect::from_min_max(row.left_top(), pos2(menu.left() - 6.0, row.bottom()));
    let response = ui.interact(rect, hit.id.with("open"), Sense::click());
    let folder = entry.kind == EntryKind::Directory;
    let (label, tint) = if folder { ("Folder", theme::companion(ui)) } else { ("Playlist", theme::accent(ui)) };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &entry.label));
    chrome::focus_fill(ui, &response, theme::card(ui), tint.gamma_multiply(0.10));
    let painter = ui.painter();
    let show_icon = rect.width() >= 112.0;
    if show_icon {
        let icon = Rect::from_min_size(rect.left_top() + vec2(7.0, 6.0), vec2(45.0, 45.0));
        painter.rect_filled(icon, 8, tint.gamma_multiply(0.13));
        let center = icon.center();
        let stroke = Stroke::new(1.5, tint);
        if folder {
            let body = Rect::from_center_size(center + vec2(0.0, 2.0), vec2(24.0, 15.0));
            painter.rect_stroke(body, 3, stroke, egui::StrokeKind::Inside);
            let edge = body.left_top() + vec2(0.5, 0.5);
            painter.line_segment([edge, edge - vec2(0.0, 3.5)], stroke);
            painter.line_segment([edge - vec2(0.0, 3.5), edge + vec2(8.0, -3.5)], stroke);
        } else {
            for y in [-6.0, 0.0, 6.0] {
                painter.circle_filled(center + vec2(-10.0, y), 1.3, tint);
                painter.line_segment([center + vec2(-5.0, y), center + vec2(10.0, y)], stroke);
            }
        }
    }
    let text_x = if show_icon { 63.0 } else { 8.0 };
    line(ui, rect.left_top() + vec2(text_x, 7.0), rect.width() - text_x - 24.0, &entry.label, 17.0, Color32::WHITE);
    line(ui, rect.left_top() + vec2(text_x, 31.0), rect.width() - text_x - 24.0, tr(ui, label).as_ref(), 13.0, tint);
    painter.text(rect.right_center() - vec2(9.0, 1.0), Align2::RIGHT_CENTER, "›", FontId::proportional(23.0), tint);
    (response.on_hover_text(&entry.path).clicked(), row_menu_button(ui, menu, "Queue"))
}

fn row_menu_rect(row: Rect) -> Rect {
    let width = (row.width() * 0.55).clamp(44.0, 116.0);
    Rect::from_min_size(pos2(row.right() - width - 6.0, row.top() + 6.0), vec2(width, 45.0))
}

fn row_menu_button(ui: &mut Ui, rect: Rect, label: &str) -> egui::Response {
    let label = tr(ui, label);
    ui.put(rect, egui::Button::new(RichText::new(label.as_ref()).size(15.0).strong())
        .right_text(RichText::new("›").color(MUTED))
        .fill(theme::raised(ui)).corner_radius(7).wrap_mode(egui::TextWrapMode::Truncate))
        .on_hover_text(label)
}

fn line(ui: &Ui, pos: Pos2, width: f32, text: &str, size: f32, color: Color32) {
    if width <= 1.0 || text.is_empty() { return; }
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), FontId::proportional(size), color);
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let painter = ui.painter();
    painter.galley(pos, painter.layout_job(job), color);
}

fn basename(path: &str) -> &str { path.rsplit('/').next().unwrap_or(path) }
fn fallback<'a>(value: &'a str, fallback: &'a str) -> &'a str { if value.is_empty() { fallback } else { value } }
fn track_count(ui: &Ui, n: usize) -> String { format!("{n} {}", tr(ui, if n == 1 { "track" } else { "tracks" })) }

fn track_duration(seconds: f64) -> String {
    let secs = seconds as u64;
    match secs {
        0 => "—".to_owned(),
        1..=3599 => format!("{}:{:02}", secs / 60, secs % 60),
        _ => format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60),
    }
}

fn loading(ui: &mut Ui, message: &str) {
    ui.horizontal(|ui| { ui.spinner(); ui.label(RichText::new(tr(ui, message)).color(MUTED)); });
}

fn empty(ui: &mut Ui, message: &str) { ui.label(RichText::new(tr(ui, message)).color(MUTED)); }
