use crate::{
    chrome,
    ja::{Action, Browse, Catalog, Collection, PlayerState, Queue, QueueTarget, Rows, Scope, Track},
    ja_ui::Keyboard,
    model::QueuePlacement,
    settings::tr,
    theme, ui::{MUTED, WARNING},
};
use eframe::egui::{self, Align, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, TextureHandle, Ui, pos2, vec2};
use std::{collections::{HashMap, HashSet}, time::Duration};

const TEXT: Color32 = Color32::from_rgb(235, 242, 238);
const BAND_HEIGHT: f32 = 42.0;
const ROW_HEIGHT: f32 = 48.0;
const PAGE_BUTTON: f32 = 65.0;
const PAGE_RANGE: f32 = 126.0;
const KEYBOARD_BUTTON: f32 = 94.0;
const SEARCH_BUTTON: f32 = 68.0;
const TABS: [(Collection, &str, f32); 7] = [
    (Collection::Albums, "Albums", 76.0), (Collection::Artists, "Artists", 76.0),
    (Collection::Genres, "Genres", 76.0), (Collection::Folders, "Folders", 78.0),
    (Collection::Tracks, "Tracks", 76.0), (Collection::MostPlayed, "Most played", 108.0),
    (Collection::Liked, "Liked", 64.0),
];

#[derive(Clone)]
struct Location {
    browse: Browse,
    title: String,
    subtitle: String,
    art: String,
}

#[derive(Default)]
struct Thumbnails {
    entries: HashMap<String, (Option<TextureHandle>, u64)>,
    inflight: HashSet<String>,
    clock: u64,
}

impl Thumbnails {
    fn ready(&mut self, id: String, texture: Option<TextureHandle>) {
        if !self.inflight.remove(&id) { return; }
        if self.entries.len() == 96 && let Some(evict) = self.entries.iter()
            .min_by_key(|(_, (_, stamp))| *stamp).map(|(id, _)| id.clone()) {
            self.entries.remove(&evict);
        }
        self.entries.insert(id, (texture, self.clock));
    }

    fn get(&mut self, ui: &Ui, id: &str, send: &mut impl FnMut(Action) -> bool) -> Option<&TextureHandle> {
        if id.is_empty() { return None; }
        self.clock += 1;
        if self.entries.contains_key(id) {
            let (image, stamp) = self.entries.get_mut(id).expect("existing thumbnail");
            *stamp = self.clock;
            return image.as_ref();
        }
        if !self.inflight.contains(id) && self.inflight.len() < 4 {
            if send(Action::LibraryArt(id.to_owned())) { self.inflight.insert(id.to_owned()); }
            else { ui.ctx().request_repaint_after(Duration::from_millis(120)); }
        }
        None
    }
}

pub(crate) struct Library {
    location: Location,
    history: Vec<Location>,
    search: String,
    show_keys: bool,
    requested: bool,
    result: Option<Result<Catalog, String>>,
    view_generation: u64,
    entrance: chrome::Entrance,
    thumbnails: Thumbnails,
    mutating: bool,
    notice: Option<Result<String, String>>,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            location: Location {
                browse: Browse { collection: Collection::Albums, scope: None, query: String::new(), offset: 0 },
                title: String::new(), subtitle: String::new(), art: String::new(),
            },
            history: Vec::new(), search: String::new(), show_keys: false, requested: false,
            result: None, view_generation: 0, thumbnails: Thumbnails::default(), mutating: false, notice: None,
            entrance: chrome::Entrance::default(),
        }
    }
}

impl Library {
    pub(crate) fn loaded(&mut self, query: Browse, result: Result<Catalog, String>) {
        if query == self.location.browse {
            if let Ok(page) = &result && query.offset > 0 && query.offset >= page.total {
                self.location.browse.offset = page.total.saturating_sub(1) / query.limit() * query.limit();
                self.reload();
                return;
            }
            self.entrance.begin();
            self.result = Some(result);
        }
    }

    pub(crate) fn thumbnail(&mut self, ctx: &egui::Context, id: String, image: Option<egui::ColorImage>) {
        if !self.thumbnails.inflight.contains(&id) { return; }
        let texture = image.map(|image| ctx.load_texture(&id, image, egui::TextureOptions::LINEAR));
        self.thumbnails.ready(id, texture);
    }

    pub(crate) fn changed(&mut self, result: Result<String, String>) {
        self.mutating = false;
        self.notice = Some(result);
    }

    pub(crate) fn clear_pending(&mut self) { self.mutating = false; }
    pub(crate) fn set_queue_pending(&mut self) { self.mutating = true; self.notice = None; }

    pub(crate) fn liked(&mut self, id: &str, liked: bool) {
        if let Some(Ok(page)) = &mut self.result && let Rows::Tracks(tracks) = &mut page.items {
            if !liked && self.location.browse.collection == Collection::Liked {
                if let Some(index) = tracks.iter().position(|t| t.id == id) {
                    tracks.remove(index);
                    page.total = page.total.saturating_sub(1);
                    self.requested = false;
                }
            } else if let Some(track) = tracks.iter_mut().find(|t| t.id == id) { track.liked = liked; }
        }
        self.changed(Ok(if liked { "Added to Liked" } else { "Removed from Liked" }.to_owned()));
    }

    pub(crate) fn reload(&mut self) {
        self.requested = false;
        self.result = None;
        self.view_generation += 1;
    }

    fn open(&mut self, scope: Scope, title: &str, subtitle: &str, art: &str) {
        let collection = self.location.browse.collection;
        self.history.push(std::mem::replace(&mut self.location, Location {
            browse: Browse { collection, scope: Some(scope), query: String::new(), offset: 0 },
            title: title.to_owned(), subtitle: subtitle.to_owned(), art: art.to_owned(),
        }));
        self.search.clear();
        self.show_keys = false;
        self.reload();
    }

    pub(crate) fn show(&mut self, ui: &mut Ui, keyboard: &mut Keyboard, can_edit: bool, can_play: bool,
        mut send: impl FnMut(Action) -> bool) {
        let mut nav = None;
        let mut mutation = None;
        let mut submit = false;
        let gap = ui.spacing().item_spacing.x;
        let search_controls = KEYBOARD_BUTTON + SEARCH_BUTTON + 2.0 * gap;
        let title_reserve = 122.0;
        let width = ui.available_width();
        if width >= title_reserve + 200.0 + search_controls {
            let search_width = (width - title_reserve - search_controls).min(310.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(ui, "Library")).size(26.0).strong().color(TEXT));
                ui.add_space((ui.available_width() - search_width - search_controls).max(0.0));
                self.search_bar(ui, search_width, &mut submit);
            });
        } else {
            ui.label(RichText::new(tr(ui, "Library")).size(26.0).strong().color(TEXT));
            ui.horizontal_wrapped(|ui| self.search_bar(ui, (width - search_controls).max(68.0), &mut submit));
        }
        if self.show_keys {
            let height = ui.available_height() * 0.57;
            egui::Frame::new().fill(theme::card(ui)).corner_radius(10).inner_margin(8).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("catalog-keyboard").max_height(height).show(ui, |ui| {
                    self.show_keys = keyboard.show_search(ui, &mut self.search);
                });
            });
        }
        if submit {
            self.show_keys = false;
            let trimmed = self.search.trim();
            if self.location.browse.query != trimmed {
                self.location.browse.offset = 0;
                self.location.browse.query = trimmed.to_owned();
                self.reload();
            }
        }
        if !self.requested {
            self.requested = send(Action::Browse(self.location.browse.clone()));
            if !self.requested { ui.ctx().request_repaint_after(Duration::from_millis(120)); }
        }
        let location = &self.location;
        let query = &location.browse;
        let page = self.result.as_ref().and_then(|result| result.as_ref().ok());
        let offset = query.offset;
        let page_len = page.map_or(0, |p| match &p.items {
            Rows::Albums(v) => v.len(), Rows::Names(v) => v.len(), Rows::Folders(v) => v.len(), Rows::Tracks(v) => v.len(),
        });
        let total = page.map_or(0, |p| p.total);
        let active = can_edit && !self.mutating;
        let scoped = query.scope.is_some();
        let tabs_width = TABS.iter().map(|(_, _, width)| width).sum::<f32>() + gap * (TABS.len() - 1) as f32;
        let pager_width = 2.0 * (PAGE_BUTTON + gap) + PAGE_RANGE;
        let tabs = |ui: &mut Ui, nav: &mut Option<Navigation>| {
            for (category, label, width) in TABS {
                let selected = query.collection == category;
                let tab = pill(ui, label, selected, width);
                let tab = if category == Collection::MostPlayed {
                    tab.on_hover_text("Ranked by completed listening sessions")
                } else { tab };
                if tab.clicked() && (!selected || scoped) { *nav = Some(Navigation::Category(category)); }
            }
        };
        let pager = |ui: &mut Ui, nav: &mut Option<Navigation>| {
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if band_button(ui, "Next  ›", PAGE_BUTTON, offset + page_len < total && page_len > 0).clicked() {
                    *nav = Some(Navigation::Offset(offset + page_len));
                }
                if band_button(ui, "‹  Prev", PAGE_BUTTON, offset > 0).clicked() {
                    *nav = Some(Navigation::Offset(offset.saturating_sub(query.limit())));
                }
                if page.is_some() && ui.available_width() >= 1.0 {
                    let range = if total > 0 {
                        format!("{}–{} of {total}", if page_len == 0 { 0 } else { offset + 1 }, offset + page_len)
                    } else { "0 results".to_owned() };
                    ui.add(egui::Label::new(RichText::new(range).color(MUTED).size(13.0)).truncate());
                }
            });
        };
        if width >= tabs_width + pager_width + gap {
            ui.horizontal(|ui| {
                tabs(ui, &mut nav);
                pager(ui, &mut nav);
            });
        } else {
            ui.horizontal_wrapped(|ui| tabs(ui, &mut nav));
            ui.horizontal(|ui| pager(ui, &mut nav));
        }
        ui.add_space(4.0);
        if scoped {
            let folder = matches!(query.scope, Some(Scope::Folder { .. }));
            let has_tracks = total > 0 || page.is_some_and(|page| page.folders.iter().any(|f| f.track_count > 0));
            if let Some(placement) = chrome::library_header(ui, "Play all", can_play, active && has_tracks, |ui| {
                let compact = ui.available_width() < 230.0;
                if chrome::library_back(ui) { nav = Some(Navigation::Back); }
                if !compact {
                    let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                    let texture = self.thumbnails.get(ui, &location.art, &mut send);
                    artwork(ui, rect, texture, folder);
                }
                ui.allocate_ui_with_layout(vec2(ui.available_width(), 42.0), egui::Layout::top_down(Align::Min), |ui| {
                    ui.add(egui::Label::new(RichText::new(&location.title).color(TEXT).size(20.0).strong())
                        .truncate()).on_hover_text(&location.title);
                    if !location.subtitle.is_empty() {
                        let subtitle = tr(ui, &location.subtitle);
                        ui.add(egui::Label::new(RichText::new(subtitle.as_ref()).size(13.0).color(MUTED))
                            .truncate()).on_hover_text(subtitle);
                    }
                });
            }) {
                mutation = Some(Action::CatalogQueue(placement, QueueTarget::Scope(query.clone())));
            }
        }
        if let Some(message) = &self.notice {
            let (line, color) = match message { Ok(note) => (note, theme::accent(ui)), Err(error) => (error, WARNING) };
            ui.label(RichText::new(line).size(13.0).color(color));
        }
        if self.mutating { ui.label(RichText::new("Updating server…").size(13.0).color(MUTED)); }
        match &self.result {
            None => { ui.horizontal(|ui| { ui.spinner(); ui.label("Loading library…"); }); }
            Some(Err(error)) => {
                ui.label(RichText::new(error).color(WARNING));
                if tool(ui, "Retry", true).clicked() { nav = Some(Navigation::Offset(offset)); }
            }
            Some(Ok(page)) => {
                if page_len == 0 && page.folders.is_empty() {
                    let message = if !query.query.is_empty() { "No results for this search." }
                        else if query.collection == Collection::Liked { "No liked tracks yet." }
                        else { "Nothing in this collection yet." };
                    ui.label(RichText::new(message).color(MUTED));
                } else {
                    let entrance = self.entrance;
                    entrance.show(ui, |ui| {
                    let scroll = egui::ScrollArea::vertical().id_salt(("ja-catalog", self.view_generation));
                    match &page.items {
                        Rows::Albums(albums) => {
                            let gutter = 12.0; // Keep tiles clear of the scroll thumb.
                            let gap = 10.0;
                            let width = (ui.available_width() - gutter).max(1.0);
                            let cols = ((width + gap) / (158.0 + gap)).floor().clamp(1.0, 6.0) as usize;
                            let w = (width - gap * (cols - 1) as f32) / cols as f32;
                            let card_chrome = 53.0;
                            let art_limit = ((ui.available_height() - ui.spacing().item_spacing.y) / 2.0 - card_chrome)
                                .clamp(112.0, 156.0);
                            let art_side = (w - 14.0).clamp(1.0, art_limit);
                            let height = art_side + card_chrome;
                            scroll.show_rows(ui, height, albums.len().div_ceil(cols), |ui, range| {
                                for line in range {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = gap;
                                        for album in albums.iter().skip(line * cols).take(cols) {
                                            let cover = self.thumbnails.get(ui, &album.artwork_id, &mut send);
                                            let meta = format!("{}  ·  {}", present(&album.artist, "Unknown artist"), count(album.track_count));
                                            if album_tile(ui, [w, height], &album.title, &meta, cover, art_side) {
                                                nav = Some(Navigation::Scope(Scope::Album { id: album.id.clone() }, album.title.clone(), meta, album.artwork_id.clone()));
                                            }
                                        }
                                    });
                                }
                            });
                        }
                        Rows::Names(names) => {
                            let cols = (ui.available_width() / 240.0).floor().clamp(1.0, 4.0) as usize;
                            let w = (ui.available_width() - 12.0 - (cols - 1) as f32 * ui.spacing().item_spacing.x) / cols as f32;
                            scroll.show_rows(ui, ROW_HEIGHT, names.len().div_ceil(cols), |ui, range| {
                                for line in range {
                                    ui.horizontal(|ui| {
                                        for item in names.iter().skip(line * cols).take(cols) {
                                            let meta = count(item.track_count);
                                            if browse_row(ui, w, &item.name, &meta, false) {
                                                let name = item.name.clone();
                                                let scope = if query.collection == Collection::Artists { Scope::Artist { name } } else { Scope::Genre { name } };
                                                nav = Some(Navigation::Scope(scope, item.name.clone(), meta, String::new()));
                                            }
                                        }
                                    });
                                }
                            });
                        }
                        Rows::Folders(folders) => {
                            scroll.show_rows(ui, ROW_HEIGHT, folders.len(), |ui, range| {
                                for folder in &folders[range] {
                                    let label = format!("{}  ·  {}", present(&folder.path, "Music root"), count(folder.track_count));
                                    if browse_row(ui, ui.available_width(), &folder.name, &label, true) {
                                        nav = Some(folder_nav(folder));
                                    }
                                }
                            });
                        }
                        Rows::Tracks(tracks) => {
                            let folder_count = page.folders.len();
                            scroll.show_rows(ui, ROW_HEIGHT, folder_count + tracks.len(), |ui, range| {
                                for index in range {
                                    if index < folder_count {
                                        let folder = &page.folders[index];
                                        let label = format!("{}  ·  {}", folder.path, count(folder.track_count));
                                        if browse_row(ui, ui.available_width(), &folder.name, &label, true) { nav = Some(folder_nav(folder)); }
                                    } else {
                                        let index = index - folder_count;
                                        let track = &tracks[index];
                                        let cover = self.thumbnails.get(ui, &track.artwork_id, &mut send);
                                        let number = if matches!(query.scope, Some(Scope::Album { .. })) && track.track != 0 {
                                            if track.disc > 1 { format!("{}–{}", track.disc, track.track) } else { track.track.to_string() }
                                        } else { (offset + index + 1).to_string() };
                                        let ranked = query.collection == Collection::MostPlayed;
                                        let clicked = ui.push_id(&track.id, |ui| {
                                            track_row(ui, track, &number, cover, active, can_play, ranked)
                                        }).inner;
                                        if clicked.is_some() { mutation = clicked; }
                                    }
                                }
                            });
                        }
                    }
                    });
                }
            }
        }
        if let Some(nav) = nav {
            self.notice = None;
            match nav {
                Navigation::Category(collection) => {
                    self.history.clear();
                    self.location = Location { browse: Browse { collection, scope: None, query: self.search.trim().to_owned(), offset: 0 },
                        title: String::new(), subtitle: String::new(), art: String::new() };
                    self.reload();
                }
                Navigation::Scope(scope, title, subtitle, art) => self.open(scope, &title, &subtitle, &art),
                Navigation::Back => if let Some(previous) = self.history.pop() {
                    self.search.clone_from(&previous.browse.query);
                    self.location = previous;
                    self.reload();
                },
                Navigation::Offset(offset) => { self.location.browse.offset = offset; self.reload(); }
            }
            // Fetch navigation immediately even without another user event.
            self.requested = send(Action::Browse(self.location.browse.clone()));
            if !self.requested { ui.ctx().request_repaint_after(Duration::from_millis(120)); }
        }
        if let Some(action) = mutation {
            if send(action) { self.mutating = true; self.notice = None; }
            else { self.changed(Err("Could not send the library action.".to_owned())); }
        }
    }

    pub(crate) fn show_queue(&mut self, ui: &mut Ui, queue: Option<&Queue>, player: Option<&PlayerState>,
        current_cover: Option<&TextureHandle>, can_edit: bool, can_play: bool, mut send: impl FnMut(Action) -> bool) -> Option<u64> {
        let entries = queue.map_or(&[][..], |q| q.entries.as_slice());
        let current = player.filter(|p| !p.current_entry_id.is_empty())
            .and_then(|p| entries.iter().position(|entry| entry.id == p.current_entry_id));
        let mut jump = false;
        let mut clear = false;
        let can_edit = can_edit && !self.mutating;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("JASTREAMER  /  PLAYBACK").size(11.0).strong().color(theme::accent(ui)));
                ui.label(RichText::new(tr(ui, "Queue")).size(29.0).strong().color(TEXT));
            });
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                clear = tool(ui, "Clear queue", can_edit && !entries.is_empty()).clicked();
                if tool(ui, "Jump to current", current.is_some()).clicked() { jump = true; }
                if queue.is_some() {
                    let unit = if entries.len() == 1 { "track" } else { "tracks" };
                    ui.label(RichText::new(format!("{} {}", entries.len(), tr(ui, unit))).color(MUTED));
                }
            });
        });
        ui.add_space(5.0);
        if let Some(player) = player && let Some(track) = player.track.as_ref() {
            let status = tr(ui, queue_status(&player.state));
            let texture = current_cover.or_else(|| self.thumbnails.get(ui, &track.artwork_id, &mut send));
            let width = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(vec2(width, 77.0), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, 10, theme::card(ui));
            painter.rect_stroke(rect, 10, Stroke::new(1.0, theme::line(ui)), egui::StrokeKind::Inside);
            painter.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 8.0), vec2(3.0, 61.0)), 2, theme::accent(ui));
            artwork(ui, Rect::from_min_size(rect.left_top() + vec2(12.0, 9.0), vec2(59.0, 59.0)), texture, false);
            let header = match current {
                Some(i) => format!("{status}  ·  {} {}", tr(ui, "Queue position"), i + 1),
                None => status.into_owned(),
            };
            if player.state == "playing" {
                chrome::signal(ui, Rect::from_center_size(rect.right_center() - vec2(30.0, 0.0), vec2(24.0, 23.0)), theme::accent(ui));
            }
            line(ui, rect.left_top() + vec2(84.0, 8.0), width - 136.0, &header, 11.0, theme::accent(ui));
            line(ui, rect.left_top() + vec2(84.0, 23.0), width - 104.0, &track.title, 22.0, TEXT);
            let detail = if track.album.is_empty() { track.artist.clone() } else { format!("{}  ·  {}", track.artist, track.album) };
            line(ui, rect.left_top() + vec2(84.0, 52.0), width - 104.0, &detail, 13.0, MUTED);
            ui.add_space(7.0);
        }
        if let Some(message) = &self.notice {
            let (text, tint) = match message { Ok(text) => (text, theme::accent(ui)), Err(error) => (error, WARNING) };
            ui.label(RichText::new(tr(ui, text)).size(13.0).color(tint));
        }
        let Some(queue) = queue else { ui.label(tr(ui, "Loading play queue…")); return None; };
        if entries.is_empty() {
            ui.label(RichText::new(tr(ui, "The play queue is empty.")).color(MUTED));
            return None;
        }
        let mut scroll = egui::ScrollArea::vertical().id_salt("ja-queue-v2");
        if jump && let Some(i) = current {
            scroll = scroll.vertical_scroll_offset(i as f32 * (56.0 + ui.spacing().item_spacing.y));
        }
        let mut action = None;
        scroll.show_rows(ui, 56.0, entries.len(), |ui, range| {
            for index in range {
                let entry = &entries[index];
                let cover = self.thumbnails.get(ui, &entry.track.artwork_id, &mut send);
                let is_current = Some(index) == current;
                let status = if is_current {
                    player.map(|p| queue_status(&p.state))
                } else if entry.status == "error" {
                    Some("PLAYBACK FAILED")
                } else if !entry.track.available {
                    Some("FILE UNAVAILABLE")
                } else { None };
                ui.push_id(&entry.id, |ui| {
                    let width = ui.available_width();
                    let (rect, response) = ui.allocate_exact_size(vec2(width, 56.0), Sense::hover());
                    let painter = ui.painter().clone();
                    chrome::focus_fill(ui, &response, if is_current { theme::raised(ui) } else { Color32::TRANSPARENT }, theme::card(ui));
                    if is_current {
                        painter.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 5.0), vec2(3.0, 46.0)), 2, theme::accent(ui));
                    }
                    let marker = rect.left_center() + vec2(22.0, 0.0);
                    if is_current && player.is_some_and(|p| p.state == "playing") {
                        chrome::signal(ui, Rect::from_center_size(marker, vec2(21.0, 16.0)), theme::accent(ui));
                    } else if is_current {
                        painter.circle_filled(marker, 4.0, theme::accent(ui));
                    } else {
                        painter.text(marker, Align2::CENTER_CENTER, (index + 1).to_string(), FontId::monospace(13.0), MUTED);
                    }
                    artwork(ui, Rect::from_min_size(rect.left_top() + vec2(41.0, 6.0), vec2(44.0, 44.0)), cover, false);
                    let right = rect.right() - 189.0;
                    let meta = if width >= 820.0 { rect.left() + width * 0.61 } else { right };
                    line(ui, rect.left_top() + vec2(94.0, 7.0), meta - rect.left() - 106.0, &entry.track.title, 17.0,
                        if is_current { theme::accent(ui) } else { TEXT });
                    line(ui, rect.left_top() + vec2(94.0, 30.0), meta - rect.left() - 106.0, &entry.track.artist, 13.0, MUTED);
                    if width >= 820.0 {
                        line(ui, pos2(meta, rect.top() + 8.0), right - meta - 71.0, &entry.track.album, 14.0, TEXT);
                        if let Some(status) = status {
                            line(ui, pos2(meta, rect.top() + 29.0), right - meta - 71.0, &tr(ui, status), 11.0,
                                if is_current { theme::accent(ui) } else { WARNING });
                        }
                        painter.text(pos2(right - 12.0, rect.center().y), Align2::RIGHT_CENTER,
                            track_duration(entry.track.duration_ms), FontId::monospace(13.0), MUTED);
                    }
                    let position_locked = is_current || entry.status == "playing";
                    for (i, (label, help, enabled)) in [
                        ("▶", "Play", can_play && can_edit && entry.track.available),
                        ("↑", "Move earlier", can_edit && !position_locked && index > 0),
                        ("↓", "Move later", can_edit && !position_locked && index + 1 < entries.len()),
                        ("×", "Remove from queue", can_edit && !position_locked),
                    ].into_iter().enumerate() {
                        let hit = Rect::from_min_size(pos2(right + i as f32 * 46.0, rect.top() + 7.0), vec2(42.0, 42.0));
                        if chrome::control_at(ui, hit, label, help, enabled) {
                            action = Some(match i {
                                0 => Action::Player("play", Some(entry.id.clone()), None),
                                1 => Action::QueueEdit { revision: queue.revision, entry: entry.id.clone(), index: Some(index - 1) },
                                2 => Action::QueueEdit { revision: queue.revision, entry: entry.id.clone(), index: Some(index + 1) },
                                _ => Action::QueueEdit { revision: queue.revision, entry: entry.id.clone(), index: None },
                            });
                        }
                    }
                    painter.hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, theme::line(ui)));
                });
            }
        });
        if let Some(action) = action {
            let waits = matches!(action, Action::QueueEdit { .. });
            if send(action) { self.notice = None; self.mutating = waits; }
            else { self.changed(Err("Could not send the queue action.".to_owned())); }
        }
        clear.then_some(queue.revision)
    }

    fn search_bar(&mut self, ui: &mut Ui, width: f32, submit: &mut bool) {
        let search = egui::TextEdit::singleline(&mut self.search).hint_text(tr(ui, "Search this collection"))
            .font(FontId::proportional(16.0)).margin(vec2(10.0, 9.0));
        let field = ui.add_sized(vec2(width, BAND_HEIGHT), search);
        *submit |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let keyboard_label = if self.show_keys { "Hide keys" } else { "Keyboard" };
        if band_button(ui, keyboard_label, KEYBOARD_BUTTON, true).clicked() { self.show_keys = !self.show_keys; }
        *submit |= band_button(ui, "Search", SEARCH_BUTTON, true).clicked();
    }
}

enum Navigation { Category(Collection), Scope(Scope, String, String, String), Back, Offset(usize) }

fn folder_nav(folder: &crate::ja::Folder) -> Navigation {
    Navigation::Scope(Scope::Folder { root_id: folder.root_id.clone(), path: folder.path.clone() },
        folder.name.clone(), present(&folder.path, "Music root").to_owned(), String::new())
}

fn present<'a>(value: &'a str, fallback: &'a str) -> &'a str { if value.is_empty() { fallback } else { value } }
fn count(n: usize) -> String { format!("{n} {}", if n == 1 { "track" } else { "tracks" }) }

fn pill(ui: &mut Ui, label: &str, active: bool, width: f32) -> egui::Response {
    let animation = ui.ctx().animate_bool_with_time(ui.id().with(("catalog-pill", label)), active, 0.17);
    ui.add(crate::ui::centered_button(RichText::new(tr(ui, label)).size(15.0).strong().color(if active { theme::accent(ui) } else { MUTED }))
        .fill(theme::raised(ui).gamma_multiply(animation)).stroke(Stroke::new(1.0, theme::accent(ui).gamma_multiply(animation * 0.45)))
        .corner_radius(16).min_size(vec2(width, BAND_HEIGHT)).selected(active))
}

fn band_button(ui: &mut Ui, label: &str, width: f32, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, crate::ui::centered_button(RichText::new(tr(ui, label)).size(14.0).color(TEXT))
        .fill(theme::raised(ui)).corner_radius(8).min_size(vec2(width, BAND_HEIGHT)))
}

fn tool(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, crate::ui::centered_button(RichText::new(tr(ui, label)).size(14.0).color(TEXT))
        .fill(theme::raised(ui)).corner_radius(8).min_size(vec2(65.0, 40.0)))
}

fn queue_status(state: &str) -> &'static str {
    match state {
        "playing" => "NOW PLAYING",
        "starting" => "STARTING",
        "paused" => "PAUSED",
        "unavailable" => "OUTPUT UNAVAILABLE",
        "error" => "PLAYBACK FAILED",
        _ => "STOPPED",
    }
}

fn track_duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs == 0 { "—".to_owned() }
    else if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60) }
    else { format!("{}:{:02}", secs / 60, secs % 60) }
}


fn line(ui: &Ui, pos: egui::Pos2, width: f32, text: &str, size: f32, color: Color32) {
    if width <= 1.0 || text.is_empty() { return; }
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), FontId::proportional(size), color);
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let painter = ui.painter();
    painter.galley(pos, painter.layout_job(job), color);
}

fn artwork(ui: &Ui, rect: Rect, cover: Option<&TextureHandle>, folder: bool) {
    let painter = ui.painter();
    painter.rect_filled(rect, 8, theme::raised(ui));
    let center = rect.center();
    if let Some(cover) = cover {
        paint_cover(painter, rect, cover, Color32::WHITE);
    } else if folder {
        let side = rect.width().min(rect.height());
        let r = Rect::from_center_size(center, vec2(side * 0.48, side * 0.34));
        painter.rect_stroke(r, 4, Stroke::new(1.5, theme::accent(ui)), egui::StrokeKind::Inside);
        painter.line_segment([r.left_top() - vec2(0.0, side * 0.07), r.left_top()], Stroke::new(1.5, theme::accent(ui)));
        painter.line_segment([r.left_top() - vec2(0.0, side * 0.07), r.left_top() + vec2(side * 0.19, -side * 0.07)], Stroke::new(1.5, theme::accent(ui)));
    } else {
        let side = rect.width().min(rect.height());
        for fraction in [0.26, 0.20, 0.14] { painter.circle_stroke(center, side * fraction, Stroke::new(1.0, theme::line(ui))); }
        painter.circle_filled(center, side * 0.04, theme::accent(ui).gamma_multiply(0.6));
    }
}

fn paint_cover(painter: &egui::Painter, rect: Rect, cover: &TextureHandle, tint: Color32) {
    let source = cover.size_vec2();
    let scale = (rect.width() / source.x).min(rect.height() / source.y);
    let image = Rect::from_center_size(rect.center(), source * scale);
    painter.image(cover.id(), image, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
}

fn album_tile(ui: &mut Ui, size: [f32; 2], title: &str, meta: &str, cover: Option<&TextureHandle>, side: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(size.into(), Sense::click());
    let painter = ui.painter();
    chrome::focus_fill(ui, &response, theme::card(ui), theme::raised(ui));
    painter.rect_stroke(rect, 10, Stroke::new(1.0, theme::line(ui)), egui::StrokeKind::Inside);
    let press = ui.ctx().animate_bool_with_time(response.id.with("art-press"), response.is_pointer_button_down_on(), 0.11);
    let reveal = ui.ctx().animate_bool_with_time(response.id.with("art-reveal"), cover.is_some(), 0.18);
    let pressed_side = side * (1.0 - 0.035 * press);
    let cover_rect = Rect::from_center_size(
        pos2(rect.center().x, rect.top() + 7.0 + side / 2.0),
        vec2(pressed_side, pressed_side),
    );
    artwork(ui, cover_rect, cover.filter(|_| reveal >= 1.0), false);
    if let Some(cover) = cover.filter(|_| reveal > 0.0 && reveal < 1.0) {
        paint_cover(painter, cover_rect, cover, Color32::WHITE.gamma_multiply(reveal));
    }
    if cover.is_some() { chrome::cover_halo(ui, cover_rect, theme::accent(ui), response.hovered()); }
    line(ui, rect.left_top() + vec2(9.0, side + 12.0), rect.width() - 18.0, title, 16.0, TEXT);
    line(ui, rect.left_top() + vec2(9.0, side + 33.0), rect.width() - 18.0, meta, 13.0, MUTED);
    response.on_hover_text(title).clicked()
}

fn browse_row(ui: &mut Ui, width: f32, name: &str, detail: &str, folder: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::click());
    chrome::focus_fill(ui, &response, theme::card(ui), theme::raised(ui));
    artwork(ui, Rect::from_min_size(rect.left_top() + vec2(7.0, 7.0), vec2(34.0, 34.0)), None, folder);
    line(ui, rect.left_top() + vec2(51.0, 5.0), width - 79.0, name, 16.0, TEXT);
    line(ui, rect.left_top() + vec2(51.0, 25.0), width - 79.0, detail, 13.0, MUTED);
    ui.painter().text(rect.right_center() - vec2(14.0, 1.0), Align2::RIGHT_CENTER, "›", FontId::proportional(22.0), MUTED);
    response.on_hover_text(name).clicked()
}

fn track_row(ui: &mut Ui, track: &Track, number: &str, cover: Option<&TextureHandle>, can_edit: bool, can_play: bool, ranked: bool) -> Option<Action> {
    let width = ui.available_width();
    let (rect, r) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::hover());
    let painter = ui.painter().clone();
    chrome::focus_fill(ui, &r, Color32::TRANSPARENT, theme::card(ui));
    painter.hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, theme::line(ui)));
    let compact = width < 790.0;
    let thumbnail = width >= 460.0;
    let primary = if thumbnail { 86.0 } else { 40.0 };
    painter.text(rect.left_center() + vec2(21.0, 0.0), Align2::CENTER_CENTER, number, FontId::monospace(13.0), MUTED);
    if thumbnail { artwork(ui, Rect::from_min_size(rect.left_top() + vec2(40.0, 5.0), vec2(38.0, 38.0)), cover, false); }
    let actions_w = if compact { 132.0 } else { 190.0 };
    let right = rect.right() - actions_w;
    let meta_x = if compact { right } else { rect.left() + (right - rect.left()) * 0.66 };
    let title_x = rect.left() + primary;
    line(ui, pos2(title_x, rect.top() + 4.0), meta_x - title_x - 14.0, &track.title, 17.0, if track.available { TEXT } else { MUTED });
    let artist = present(&track.artist, "Unknown artist");
    let caption = if ranked { format!("{artist}  ·  {} plays", track.play_count) }
        else if compact && !track.album.is_empty() { format!("{artist}  ·  {}", track.album) } else { String::new() };
    line(ui, pos2(title_x, rect.top() + 26.0), meta_x - title_x - 14.0, present(&caption, artist), 13.0, MUTED);
    if !compact {
        let metadata_w = right - meta_x - 70.0;
        line(ui, pos2(meta_x, rect.top() + 6.0), metadata_w, present(&track.album, "Unknown album"), 14.0, TEXT);
        line(ui, pos2(meta_x, rect.top() + 25.0), metadata_w, if track.available { &track.format } else { "Unavailable" }, 12.0, MUTED);
        let duration = track_duration(track.duration_ms);
        painter.text(pos2(right - 16.0, rect.center().y), Align2::RIGHT_CENTER, duration, FontId::monospace(13.0), MUTED);
    }
    let play_enabled = can_edit && can_play && track.available;
    let queue_enabled = can_edit && track.available;
    let play_rect = Rect::from_min_size(pos2(right, rect.top() + 3.0), vec2(46.0, 42.0));
    let play = chrome::control_at(ui, play_rect, "▶", "Play now · keep existing queue", play_enabled);
    let menu_rect = Rect::from_min_size(pos2(right + 52.0, rect.top() + 3.0), vec2(if compact { 73.0 } else { 85.0 }, 42.0));
    let mut action = None;
    let menu = ui.put(menu_rect, egui::Button::new(RichText::new(tr(ui, "Queue")).size(14.0)).fill(theme::raised(ui)).corner_radius(7));
    let mark = menu_rect.right_center() - vec2(11.0, 0.0);
    let stroke = Stroke::new(1.1, MUTED);
    painter.line_segment([mark + vec2(-3.0, -1.5), mark + vec2(0.0, 1.5)], stroke);
    painter.line_segment([mark + vec2(0.0, 1.5), mark + vec2(3.0, -1.5)], stroke);
    egui::Popup::menu(&menu).show(|ui| {
        if tool(ui, "Play next", queue_enabled).clicked() {
            action = Some(Action::CatalogQueue(QueuePlacement::Next, QueueTarget::Track(track.id.clone())));
            ui.close();
        }
        if tool(ui, "Add to queue", queue_enabled).clicked() {
            action = Some(Action::CatalogQueue(QueuePlacement::End, QueueTarget::Track(track.id.clone())));
            ui.close();
        }
        if tool(ui, if track.liked { "Remove like" } else { "Like track" }, can_edit).clicked() {
            action = Some(Action::Like(track.id.clone(), !track.liked));
            ui.close();
        }
    });
    if !compact && track.liked {
        painter.text(pos2(rect.right() - 21.0, rect.center().y), Align2::CENTER_CENTER, "♥", FontId::proportional(17.0), theme::accent(ui));
    }
    if play { Some(Action::CatalogQueue(QueuePlacement::Play, QueueTarget::Track(track.id.clone()))) }
    else { action }
}
