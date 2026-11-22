use crate::{theme, ui::{MUTED, WARNING, art, paint_art}};
use crate::model::{QueuePlacement, RepeatMode};
use crate::settings::tr;
use eframe::egui::{self, Color32, Rect, RichText, Sense, TextureHandle, Ui, pos2, vec2};
use std::time::Instant;
use std::time::Duration;

#[derive(Clone, Copy)]
struct StatusMotion {
    message: egui::Id,
    started: f64,
    frame: u64,
}

pub(crate) fn status_ticker(ui: &mut Ui, key: &str, prefix: &str, text: &str, tint: Color32) {
    status_ticker_details(ui, key, prefix, text, tint, None);
}

pub(crate) fn status_ticker_details(ui: &mut Ui, key: &str, prefix: &str, text: &str, tint: Color32, details: Option<&str>) {
    let id = ui.id().with(key);
    let frame = ui.ctx().cumulative_frame_nr();
    let now = ui.input(|input| input.time);
    let message = id.with((prefix, text));
    let started = ui.data_mut(|data| {
        let motion = data.get_temp_mut_or_insert_with(id, || StatusMotion { message, started: now, frame });
        if motion.message != message || frame.saturating_sub(motion.frame) > 1 {
            *motion = StatusMotion { message, started: now, frame };
        }
        motion.frame = frame;
        motion.started
    });
    let mut line = String::with_capacity(prefix.len() + text.len());
    line.push_str(prefix);
    line.push_str(text);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_job(egui::text::LayoutJob::simple_singleline(line, font, tint));

    let (row, response) = ui.allocate_exact_size(vec2(ui.available_width(), galley.size().y), Sense::hover());
    let overflow = (galley.size().x - row.width()).max(0.0);
    let travel = overflow / 70.0;
    let elapsed = (now - started).max(0.0) as f32;
    let enter = 0.65;
    let hold = 1.4;
    let t = if overflow > 0.0 { elapsed % (enter + 2.0 * hold + travel) } else { elapsed };
    let x = if t < enter {
        row.width() * (1.0 - t / enter).powi(3)
    } else {
        -((t - enter - hold) * 70.0).clamp(0.0, overflow)
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), &galley.job.text));
    if let Some(details) = details {
        response.on_hover_text(details);
    } else if overflow > 0.0 {
        response.on_hover_text(galley.job.text.as_str());
    }
    ui.painter().with_clip_rect(row).galley(row.min + vec2(x, 0.0), galley, tint);
    if t < enter || (overflow > 0.0 && (enter + hold..enter + hold + travel).contains(&t)) {
        ui.ctx().request_repaint();
    } else if overflow > 0.0 {
        let next = if t < enter + hold { enter + hold } else { enter + 2.0 * hold + travel };
        ui.ctx().request_repaint_after(Duration::from_secs_f32(next - t));
    }
}

pub(crate) fn control_at(ui: &mut Ui, rect: Rect, label: &str, help: &str, enabled: bool) -> bool {
    let help = tr(ui, help);
    ui.put(rect, egui::Button::new(RichText::new(label).size(17.0)
        .color(if enabled { Color32::WHITE } else { MUTED.gamma_multiply(0.5) }))
        .fill(if enabled { theme::raised(ui) } else { theme::card(ui) }).corner_radius(7)
        .sense(if enabled { Sense::click() } else { Sense::hover() }))
        .on_hover_text(help).clicked() && enabled
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Entrance(Option<(Instant, f32)>);

impl Entrance {
    pub(crate) fn begin(&mut self) { self.begin_in(true); }

    pub(crate) fn begin_in(&mut self, forward: bool) {
        self.0 = Some((Instant::now(), if forward { 1.0 } else { -1.0 }));
    }

    pub(crate) fn show(&self, ui: &mut Ui, content: impl FnOnce(&mut Ui)) {
        let (phase, direction) = self.0.map_or((1.0, 0.0), |(start, direction)| {
            ((start.elapsed().as_secs_f32() / 0.25).min(1.0), direction)
        });
        let remaining = (1.0 - phase).powi(3);
        if phase < 1.0 { ui.ctx().request_repaint(); }
        let bounds = ui.available_rect_before_wrap();
        let shift = vec2(direction * 29.0 * remaining, 0.0);
        ui.scope_builder(egui::UiBuilder::new().max_rect(bounds.translate(shift)), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(bounds));
            ui.multiply_opacity(1.0 - 0.60 * remaining);
            content(ui);
        });
    }
}

#[derive(Default)]
pub(crate) enum FolderMotion {
    #[default]
    Idle,
    Awaiting { opened: Instant, direction: f32 },
    Revealing { opened: Instant, started: Instant, direction: f32 },
}

impl FolderMotion {
    pub(crate) fn prepare(&mut self, forward: bool) {
        *self = Self::Awaiting { opened: Instant::now(), direction: if forward { 1.0 } else { -1.0 } };
    }

    pub(crate) fn arrived(&mut self) {
        if let Self::Awaiting { opened, direction } = *self {
            *self = Self::Revealing { opened, started: Instant::now(), direction };
        }
    }

    pub(crate) fn frame(&self, ui: &Ui, tint: Color32) -> FolderFrame {
        let (title, rows) = match *self {
            Self::Idle => (None, None),
            Self::Awaiting { opened, direction } => (Some((opened.elapsed().as_secs_f32(), direction)), None),
            Self::Revealing { opened, started, direction } => (
                Some((opened.elapsed().as_secs_f32(), direction)),
                Some((started.elapsed().as_secs_f32(), direction)),
            ),
        };
        let title = title.filter(|(elapsed, _)| *elapsed < FolderFrame::DURATION);
        let rows = rows.filter(|(elapsed, _)| *elapsed < FolderFrame::DURATION + FolderFrame::STAGGER * FolderFrame::LAST as f32);
        if title.is_some() || rows.is_some() { ui.ctx().request_repaint(); }
        FolderFrame { title, rows, tint }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FolderFrame {
    title: Option<(f32, f32)>,
    rows: Option<(f32, f32)>,
    tint: Color32,
}

impl FolderFrame {
    const DURATION: f32 = 0.30;
    const STAGGER: f32 = 0.026;
    const LAST: usize = 9;

    pub(crate) fn title(self, ui: &mut Ui, title: &str) {
        self.slot(ui, "folder-title", 36.0, 0, false, |ui| {
            ui.add(egui::Label::new(RichText::new(title).size(25.0).strong().color(self.tint))
                .truncate()).on_hover_text(title);
        });
    }

    pub(crate) fn row<R>(self, ui: &mut Ui, id: usize, rank: usize, height: f32, content: impl FnOnce(&mut Ui) -> R) -> R {
        self.slot(ui, ("folder-row", id), height, rank, true, content)
    }

    fn slot<R>(self, ui: &mut Ui, id: impl std::hash::Hash, height: f32, rank: usize, row: bool,
        content: impl FnOnce(&mut Ui) -> R) -> R {
        // Reserve the final slot first: virtualized rows, clipping and scroll
        // positions must not move as successive rows enter.
        let (slot, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let motion = if row { self.rows } else { self.title };
        let (remaining, direction) = motion.map_or((0.0, 0.0), |(elapsed, direction)| {
            let delay = Self::STAGGER * rank.min(Self::LAST) as f32;
            let progress = ((elapsed - delay) / Self::DURATION).clamp(0.0, 1.0);
            ((1.0 - progress).powi(3), direction)
        });
        let translated = slot.translate(vec2(61.0 * direction, 8.0) * remaining);
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt(id).max_rect(translated)
            .layout(egui::Layout::top_down(egui::Align::Min)));
        child.set_clip_rect(ui.clip_rect().intersect(slot));
        child.multiply_opacity(1.0 - 0.84 * remaining);
        let result = content(&mut child);
        if row && remaining > 0.0 && remaining < 1.0 {
            let opacity = 0.68 * remaining * (1.0 - remaining);
            let x = if direction > 0.0 { translated.left() + 1.0 } else { translated.right() - 1.0 };
            child.painter().line_segment(
                [pos2(x, translated.top() + 11.0), pos2(x, translated.bottom() - 11.0)],
                egui::Stroke::new(2.0, self.tint.gamma_multiply(opacity)),
            );
        }
        result
    }
}

pub(crate) fn library_back(ui: &mut Ui) -> bool {
    let compact = ui.available_width() < 230.0;
    let label = if compact { "‹".into() } else { tr(ui, "‹  Back") };
    ui.add(egui::Button::new(RichText::new(label).size(14.0))
        .fill(theme::raised(ui)).corner_radius(8).min_size(vec2(if compact { 40.0 } else { 74.0 }, 42.0)))
        .on_hover_text(tr(ui, "‹  Back")).clicked()
}

pub(crate) fn library_header(ui: &mut Ui, play_label: &str, can_play: bool, can_edit: bool,
    details: impl FnOnce(&mut Ui)) -> Option<QueuePlacement> {
    let actions = [
        (play_label, QueuePlacement::Play, can_edit && can_play, 84.0),
        ("Play next", QueuePlacement::Next, can_edit, 101.0),
        ("Add to queue", QueuePlacement::End, can_edit, 124.0),
    ];
    let mut selected = None;
    egui::Frame::new().fill(theme::card(ui)).corner_radius(10).inner_margin(8).show(ui, |ui| {
        let width = ui.available_width();
        let gap = ui.spacing().item_spacing.x;
        let actions_width = actions.iter().map(|&(_, _, _, width)| width).sum::<f32>() + 2.0 * gap;
        let inline = width >= actions_width + gap + 180.0;
        let reserved = if inline { actions_width } else { 110.0 };
        ui.set_min_width(width);
        ui.horizontal(|ui| {
            let detail_size = vec2((width - reserved - gap).max(1.0), 44.0);
            ui.allocate_ui_with_layout(detail_size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_size(detail_size);
                details(ui);
            });
            let mut draw = |ui: &mut Ui| {
                for (label, placement, enabled, width) in actions {
                    let button = egui::Button::new(RichText::new(tr(ui, label)).size(14.0))
                        .min_size(vec2(width, 40.0)).wrap_mode(egui::TextWrapMode::Truncate)
                        .fill(theme::raised(ui)).corner_radius(8);
                    if ui.add_enabled(enabled, button).clicked() {
                        selected = Some(placement);
                        if !inline { ui.close(); }
                    }
                }
            };
            if inline { draw(ui); }
            else {
                let menu = ui.add(egui::Button::new(tr(ui, "Queue"))
                    .right_text(RichText::new("›").color(MUTED)).min_size(vec2(reserved, 40.0))
                    .wrap_mode(egui::TextWrapMode::Truncate).fill(theme::raised(ui)).corner_radius(8));
                egui::Popup::menu(&menu).show(draw);
            }
        });
    });
    ui.add_space(4.0);
    selected
}

pub(crate) fn signal(ui: &Ui, rect: Rect, tint: Color32) {
    let time = ui.input(|input| (input.time % 12.0) as f32);
    let painter = ui.painter();
    for (i, rate) in [2.5_f32, 3.5, 2.0].into_iter().enumerate() {
        let wave = (time * rate * std::f32::consts::PI + i as f32 * 0.8).sin();
        let height = rect.height() * (0.56 + 0.39 * wave);
        let x = rect.left() + (i as f32 + 0.5) * rect.width() / 3.0;
        painter.rect_filled(Rect::from_center_size(pos2(x, rect.bottom() - height * 0.5), vec2(
            (rect.width() / 5.0).min(4.0), height)), 2, tint);
    }
    ui.ctx().request_repaint();
}

pub(crate) fn playback_badge(ui: &mut Ui, state: &str) {
    let tint = match state {
        "PLAYING" => theme::accent(ui),
        "ERROR" | "UNAVAILABLE" => WARNING,
        _ => MUTED,
    };
    ui.horizontal(|ui| {
        let (indicator, _) = ui.allocate_exact_size(vec2(23.0, 18.0), Sense::hover());
        if state == "PLAYING" { signal(ui, indicator.shrink(1.0), tint); }
        else { ui.painter().circle_filled(indicator.center(), 3.0, tint); }
        ui.label(RichText::new(tr(ui, state)).strong().size(14.0).color(tint));
    });
}

pub(crate) fn focus_fill(ui: &Ui, response: &egui::Response, resting: Color32, active: Color32) {
    let focus = response.hovered() || response.has_focus();
    let t = ui.ctx().animate_bool_with_time(response.id.with("focus-motion"), focus, 0.15);
    let touch = ui.ctx().animate_bool_with_time(response.id.with("touch-motion"), response.is_pointer_button_down_on(), 0.08);
    let body = response.rect.shrink(touch * 1.5);
    ui.painter().rect_filled(body, 9, resting);
    if t > 0.0 || touch > 0.0 {
        ui.painter().rect_filled(body, 9, active.gamma_multiply((t + touch * 0.26).min(1.0)));
    }
}

pub struct Song<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub cover: Option<&'a TextureHandle>,
}

impl Song<'_> {
    fn headline(&self, ui: &Ui) -> std::borrow::Cow<'_, str> {
        if self.title.is_empty() { tr(ui, "No track selected") } else { self.title.into() }
    }
}

pub(crate) struct SignalPath<'a> {
    pub stages: [(&'a str, &'a str, SignalQuality); 4],
    pub note: &'a str,
    pub flowing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignalQuality { Preserved, Lossy, Converted, Unverified }

impl SignalQuality {
    fn tint(self, ui: &Ui) -> Color32 {
        match self {
            Self::Preserved => theme::accent(ui),
            Self::Lossy | Self::Converted => Color32::from_rgb(242, 204, 110),
            Self::Unverified => MUTED,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Preserved => "Lossless at this stage",
            Self::Lossy => "Lossy source",
            Self::Converted => "PCM format changed",
            Self::Unverified => "Bit-perfect not verified",
        }
    }
}

pub(crate) fn now_playing(ui: &mut Ui, song: &Song<'_>, state: &str, output: &str, pending: &str,
    path: Option<SignalPath<'_>>, has_track: bool, extra: impl FnOnce(&mut Ui)) -> bool {
    let wide = ui.available_width() >= 740.0;
    let cover_size = if wide {
        (ui.available_width() * 0.38).min(ui.clip_rect().height() - 28.0).clamp(140.0, 420.0)
    } else { (ui.available_width() * 0.64).clamp(110.0, 280.0) };
    let details = |ui: &mut Ui| {
        playback_badge(ui, state);
        ui.add_space(10.0);
        ui.add(egui::Label::new(RichText::new(song.headline(ui)).size(36.0).strong()).wrap());
        if !song.artist.is_empty() { ui.label(RichText::new(song.artist).size(22.0)); }
        if !song.album.is_empty() { ui.label(RichText::new(song.album).size(18.0).color(MUTED)); }
        extra(ui);
        ui.add_space(24.0);
        ui.separator();
        if let Some(path) = &path { signal_path(ui, path, state == "PLAYING"); }
        else if !output.is_empty() { ui.label(RichText::new(tr(ui, output)).size(17.0)); }
        if !pending.is_empty() {
            ui.horizontal(|ui| { ui.spinner(); ui.label(RichText::new(pending).color(MUTED)); });
        }
    };
    if wide {
        let height = ui.clip_rect().bottom() - ui.next_widget_position().y;
        ui.add_space(((height - cover_size - 24.0) * 0.4).clamp(0.0, 70.0));
        ui.horizontal_top(|ui| {
            let show_info = ui.vertical(|ui| stage_art(ui, song.cover, cover_size, state == "PLAYING", has_track)).inner;
            ui.add_space(28.0);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.add_space(30.0);
                details(ui);
            });
            show_info
        }).inner
    } else {
        let show_info = ui.vertical_centered(|ui| stage_art(ui, song.cover, cover_size, state == "PLAYING", has_track)).inner;
        ui.add_space(16.0);
        details(ui);
        show_info
    }
}

fn signal_path(ui: &mut Ui, path: &SignalPath<'_>, playing: bool) {
    ui.label(RichText::new(tr(ui, "Audio signal path")).size(16.0).strong().color(theme::accent(ui)));
    ui.add_space(6.0);
    let tick = (playing && path.flowing).then(|| {
        ui.ctx().request_repaint();
        ui.input(|i| ((i.time % 2.8) / 2.8) as f32 * std::f32::consts::TAU)
    });
    let card = |ui: &mut Ui, &(title, detail, quality): &(&str, &str, SignalQuality), width: f32| {
        let tint = quality.tint(ui);
        let fill = match quality {
            SignalQuality::Preserved => theme::raised(ui),
            SignalQuality::Lossy | SignalQuality::Converted => Color32::from_rgb(55, 46, 28),
            SignalQuality::Unverified => theme::card(ui),
        };
        egui::Frame::new().fill(fill)
            .stroke(egui::Stroke::new(1.0, tint.gamma_multiply(0.40)))
            .corner_radius(9).inner_margin(8).show(ui, |ui| {
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(width);
                    ui.set_min_height(58.0);
                    ui.add(egui::Label::new(RichText::new(tr(ui, title)).size(14.0).strong().color(tint))
                        .truncate()).on_hover_text(tr(ui, quality.label()));
                    ui.add(egui::Label::new(RichText::new(tr(ui, detail)).size(12.0).color(MUTED)).wrap());
                });
            });
    };
    let width = ui.available_width();
    if width >= 430.0 {
        let link = 20.0;
        let slot = (width - 3.0 * link) / 4.0 - 18.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (index, stage) in path.stages.iter().enumerate() {
                if index != 0 { let tint = stage.2.tint(ui); flow_connector(ui, vec2(link, 2.0), tick, index - 1, tint); }
                card(ui, stage, slot);
            }
        });
    } else {
        for (index, stage) in path.stages.iter().enumerate() {
            if index != 0 { let tint = stage.2.tint(ui); flow_connector(ui, vec2(2.0, 13.0), tick, index - 1, tint); }
            card(ui, stage, (width - 18.0).max(1.0));
        }
    }
    ui.add_space(7.0);
    let note_color = if path.stages.iter().all(|stage| stage.2 == SignalQuality::Preserved) { theme::accent(ui) } else { MUTED };
    ui.label(RichText::new(tr(ui, path.note)).size(13.0).color(note_color));
}

fn flow_connector(ui: &mut Ui, size: egui::Vec2, tick: Option<f32>, link: usize, tint: Color32) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let (a, b, normal) = if size.x > size.y {
        (rect.left_center(), rect.right_center(), vec2(0.0, 1.0))
    } else { (rect.center_top(), rect.center_bottom(), vec2(1.0, 0.0)) };
    let paint = ui.painter();
    let Some(tick) = tick else {
        paint.line_segment([a, b], egui::Stroke::new(1.3, tint.gamma_multiply(0.26)));
        return;
    };
    const SEGMENTS: u32 = 12;
    let mut mesh = egui::Mesh {
        vertices: Vec::with_capacity((4 * (SEGMENTS + 1)) as usize),
        indices: Vec::with_capacity((18 * SEGMENTS) as usize),
        ..Default::default()
    };
    for step in 0..=SEGMENTS {
        let x = step as f32 / SEGMENTS as f32;
        let phase = x * 5.4 + link as f32 * 1.35 - tick;
        let envelope = (x * std::f32::consts::PI).sin();
        let center = a.lerp(b, x) + normal * (2.5 * phase.sin() * envelope);
        let light = tint.gamma_multiply(0.52 + 0.17 * (phase - 0.6).cos());
        for (width, color) in [(-3.2, Color32::TRANSPARENT), (-0.65, light), (0.65, light), (3.2, Color32::TRANSPARENT)] {
            mesh.colored_vertex(center + normal * width, color);
        }
        if step != 0 {
            for lane in 0..3 {
                let i = (step - 1) * 4 + lane;
                mesh.add_triangle(i, i + 4, i + 1);
                mesh.add_triangle(i + 1, i + 4, i + 5);
            }
        }
    }
    paint.add(egui::Shape::mesh(mesh));
}

pub(crate) fn cover_halo(ui: &Ui, rect: Rect, tint: Color32, highlight: bool) {
    let alpha = if highlight { 0.20 } else { 0.10 };
    let paint = ui.painter();
    for (radius, width, fade) in [(1.0, 1.0, 1.0), (3.0, 2.0, 0.40)] {
        paint.rect_stroke(rect.expand(radius), 10.0, egui::Stroke::new(width, tint.gamma_multiply(alpha * fade)),
            egui::StrokeKind::Outside);
    }
}

fn stage_art(ui: &mut Ui, cover: Option<&TextureHandle>, side: f32, playing: bool, has_track: bool) -> bool {
    let (stage, slot) = ui.allocate_exact_size(vec2(side + 24.0, side + 24.0), Sense::hover());
    let rect = stage.shrink(12.0);
    let (breath, drift) = if playing && cover.is_some() {
        ui.ctx().request_repaint();
        let phase = ui.input(|i| ((i.time % 7.0) / 7.0) as f32 * std::f32::consts::TAU);
        (0.64 + 0.36 * phase.cos(), vec2(phase.sin() * 2.2, phase.cos() * 1.6))
    } else { (0.0, egui::Vec2::ZERO) };
    let painter = ui.painter();
    for (extent, blur, intensity) in [(7.0, 14.0, 0.035 + 0.25 * breath), (3.4, 6.0, 0.045 + 0.22 * breath)] {
        painter.add(egui::epaint::RectShape::filled(rect.expand(extent).translate(drift), 13.0,
            theme::accent(ui).gamma_multiply(intensity)).with_blur_width(blur));
    }
    paint_art(ui, rect, cover);
    painter.rect_stroke(rect.expand(1.0), 12, egui::Stroke::new(1.0, theme::accent(ui).gamma_multiply(0.14 + 0.19 * breath)), egui::StrokeKind::Outside);
    if !has_track { return false; }
    let response = ui.interact(rect, slot.id.with("track-info"), Sense::click());
    let label = tr(ui, "Track details");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label.as_ref()));
    let badge = rect.right_bottom() - vec2(20.0, 20.0);
    let accent = theme::accent(ui);
    painter.circle_filled(badge, 12.0, Color32::from_black_alpha(188));
    painter.circle_stroke(badge, 12.0, egui::Stroke::new(1.0, accent.gamma_multiply(if response.hovered() { 1.0 } else { 0.58 })));
    painter.text(badge, egui::Align2::CENTER_CENTER, "i", egui::FontId::proportional(16.0), Color32::WHITE);
    response.on_hover_text(label).clicked()
}

pub enum Motion { Previous, PlayPause, Next, Seek(f64), Volume(u8), Shuffle(bool), Repeat(RepeatMode) }

pub struct Controls {
    pub elapsed: f64,
    pub duration: f64,
    pub can_seek: bool,
    pub playing: bool,
    pub can_navigate: bool,
    pub can_toggle: bool,
    pub can_set_modes: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub volume: Option<u8>,
    pub muted: bool,
    pub can_volume: bool,
}

#[derive(Default)]
pub struct Transport {
    seek: Option<f64>,
    volume: Option<u8>,
}

impl Transport {
    pub fn reset(&mut self) { *self = Self::default(); }

    pub fn show(&mut self, ui: &mut Ui, song: &Song<'_>, state: Controls) -> Option<Motion> {
        if state.volume.is_none() { self.volume = None; }
        let mut result = None;
        if ui.available_width() >= 725.0 {
            ui.horizontal(|ui| {
                self.keys(ui, song.cover, &state, &mut result);
                ui.add_space(20.0);
                let gap = ui.spacing().item_spacing.x;
                let right = 2.0 * MODE_SIZE.x + gap + if state.volume.is_some() { 126.0 + gap } else { 0.0 };
                let middle = (ui.available_width() - right - gap).max(140.0);
                ui.allocate_ui_with_layout(vec2(middle, 60.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(middle);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    self.timeline(ui, song, &state, &mut result);
                });
                self.side_controls(ui, &state, &mut result);
            });
        } else {
            ui.horizontal_wrapped(|ui| self.keys(ui, song.cover, &state, &mut result));
            self.timeline(ui, song, &state, &mut result);
            self.side_controls(ui, &state, &mut result);
        }
        result
    }

    fn keys(&self, ui: &mut Ui, cover: Option<&TextureHandle>, state: &Controls, result: &mut Option<Motion>) {
        art(ui, cover, 60.0);
        ui.add_space(11.0);
        for (index, (icon, enabled, action)) in [
            (TransportIcon::Previous, state.can_navigate, Motion::Previous),
            (if state.playing { TransportIcon::Pause } else { TransportIcon::Play }, state.can_toggle, Motion::PlayPause),
            (TransportIcon::Next, state.can_navigate, Motion::Next),
        ].into_iter().enumerate() {
            if index != 0 { ui.add_space(6.0); }
            if transport_button(ui, icon, enabled) { *result = Some(action); }
        }
    }

    fn timeline(&mut self, ui: &mut Ui, song: &Song<'_>, state: &Controls, result: &mut Option<Motion>) {
        ui.horizontal(|ui| {
            if state.playing {
                let (rect, _) = ui.allocate_exact_size(vec2(17.0, 13.0), Sense::hover());
                signal(ui, rect, theme::accent(ui));
            }
            let title = egui::Label::new(RichText::new(song.headline(ui)).size(17.0).strong()).truncate();
            if ui.available_width() >= 325.0 && (!song.artist.is_empty() || !song.album.is_empty()) {
                let title_width = ui.available_width() * 0.55;
                ui.allocate_ui_with_layout(vec2(title_width, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_width(title_width);
                    ui.add(title);
                });
                let descriptor = if song.artist.is_empty() { song.album } else { song.artist };
                ui.add(egui::Label::new(RichText::new(descriptor).color(MUTED)).truncate());
            } else {
                ui.add(title);
            }
        });
        if let Some(pos) = crate::progress::seek(ui, state.elapsed, state.duration, state.can_seek, state.playing, &mut self.seek) {
            *result = Some(Motion::Seek(pos));
        }
    }

    fn side_controls(&mut self, ui: &mut Ui, state: &Controls, result: &mut Option<Motion>) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if transport_button(ui, TransportIcon::Repeat(state.repeat), state.can_set_modes) {
                let next = match state.repeat { RepeatMode::Off => RepeatMode::One, RepeatMode::One => RepeatMode::All, RepeatMode::All => RepeatMode::Off };
                *result = Some(Motion::Repeat(next));
            }
            if transport_button(ui, TransportIcon::Order(state.shuffle), state.can_set_modes) {
                *result = Some(Motion::Shuffle(!state.shuffle));
            }
            if let Some(current) = state.volume {
                ui.allocate_ui_with_layout(vec2(126.0, MODE_SIZE.y), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_width(126.0);
                    let mut value = self.volume.unwrap_or(current);
                    ui.spacing_mut().slider_width = 76.0;
                    let response = ui.add_enabled(state.can_volume, egui::Slider::new(&mut value, 0..=100).show_value(false));
                    if response.dragged() { self.volume = Some(value); }
                    if response.drag_stopped() || (response.changed() && !response.dragged()) {
                        self.volume = None;
                        *result = Some(Motion::Volume(value));
                    }
                    ui.label(RichText::new(if state.muted { tr(ui, "Muted").into_owned() } else { format!("{value}%") }).color(MUTED));
                });
            }
        });
    }
}

const MODE_SIZE: egui::Vec2 = vec2(48.0, 48.0);

#[derive(Clone, Copy)]
enum TransportIcon { Previous, Play, Pause, Next, Order(bool), Repeat(RepeatMode) }

pub(crate) fn repeat_label(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "Repeat off",
        RepeatMode::One => "Repeat one",
        RepeatMode::All => "Repeat all",
    }
}

fn transport_button(ui: &mut Ui, icon: TransportIcon, enabled: bool) -> bool {
    use TransportIcon::*;
    let (label, primary, active) = match icon {
        Previous => ("Previous", false, false),
        Play => ("Play", true, true),
        Pause => ("Pause", true, true),
        Next => ("Next", false, false),
        Order(false) => ("Sequential order", false, false),
        Order(true) => ("Shuffle order", false, true),
        Repeat(mode) => (repeat_label(mode), false, mode != RepeatMode::Off),
    };
    ui.add_enabled_ui(enabled, |ui| {
        let size = if primary { vec2(56.0, 56.0) } else { MODE_SIZE };
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let label = tr(ui, label);
        let accent = theme::accent(ui);
        let tint = if primary { theme::ON_ACCENT } else if active { accent } else { Color32::WHITE };
        let fill = if primary { accent } else if active { theme::raised(ui).lerp_to_gamma(accent, 0.12) } else { theme::card(ui) };
        let glow = ui.ctx().animate_bool_with_time(response.id.with("round-glow"),
            response.hovered() || response.has_focus(), 0.12);
        let p = ui.painter();
        let center = rect.center();
        let radius = rect.width() * 0.5;
        p.circle_filled(center, radius, fill.lerp_to_gamma(if primary { Color32::WHITE } else { accent }, 0.16 * glow));
        p.circle_stroke(center, radius - 0.5, egui::Stroke::new(1.0,
            if primary || active || response.has_focus() { accent } else { theme::line(ui) }));
        let glyph = if matches!(icon, Order(false) | Repeat(RepeatMode::Off)) { MUTED } else { tint };
        let stroke = egui::Stroke::new(1.8, glyph);
        let at = |point: [f32; 2]| center + vec2(point[0], point[1]) * 1.18;
        let line = |a: [f32; 2], b: [f32; 2]| { p.line_segment([at(a), at(b)], stroke); };
        let arrow = |x: f32, y: f32, direction: f32| {
            line([x - 4.0 * direction, y - 3.3], [x, y]);
            line([x, y], [x - 4.0 * direction, y + 3.3]);
        };
        match icon {
            Previous | Next | Play => {
                let (symbol, x) = match icon { Previous => ("◀", 2.0), Next => ("▶", -2.0), _ => ("▶", 1.5) };
                p.text(center + vec2(x, -0.5), egui::Align2::CENTER_CENTER, symbol, egui::FontId::proportional(25.0), tint);
                if matches!(icon, Previous | Next) {
                    let x = if matches!(icon, Previous) { -8.5 } else { 8.5 };
                    line([x, -7.0], [x, 7.0]);
                }
            }
            Pause => {
                for x in [-4.7, 4.7] {
                    p.rect_filled(Rect::from_center_size(center + vec2(x, 0.0), vec2(4.1, 20.0)), 1.2, tint);
                }
            }
            Order(false) => {
                line([-10.0, -5.0], [-2.0, -5.0]);
                line([-10.0, 0.0], [4.0, 0.0]);
                line([-10.0, 5.0], [10.0, 5.0]);
                arrow(10.0, 5.0, 1.0);
            }
            Order(true) => {
                for sign in [-1.0_f32, 1.0] {
                    line([-10.0, sign * 5.0], [-6.0, sign * 5.0]);
                    line([-6.0, sign * 5.0], [6.0, -sign * 5.0]);
                    line([6.0, -sign * 5.0], [10.0, -sign * 5.0]);
                    arrow(10.0, -sign * 5.0, 1.0);
                }
            }
            Repeat(mode) => {
                line([-10.0, 1.0], [-10.0, -6.0]);
                line([-10.0, -6.0], [10.0, -6.0]);
                arrow(10.0, -6.0, 1.0);
                line([10.0, -1.0], [10.0, 6.0]);
                line([10.0, 6.0], [-10.0, 6.0]);
                arrow(-10.0, 6.0, -1.0);
                match mode {
                    RepeatMode::One => { p.text(center, egui::Align2::CENTER_CENTER, "1", egui::FontId::monospace(10.0), tint); }
                    RepeatMode::Off => line([-3.5, 3.5], [3.5, -3.5]),
                    RepeatMode::All => {}
                }
            }
        }
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label.as_ref()));
        response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(label).clicked()
    }).inner
}

