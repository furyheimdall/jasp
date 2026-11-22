use crate::{theme, ui::MUTED};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, EventFilter, FontId, Key, Modifiers, PointerButton, Rect,
    Response, Sense, Stroke, StrokeKind, WidgetInfo, pos2, vec2,
};
use std::time::Duration;

const HIT_HEIGHT: f32 = 28.0;

pub(crate) fn seek(
    ui: &mut egui::Ui,
    elapsed: f64,
    duration: f64,
    can_seek: bool,
    playing: bool,
    preview: &mut Option<f64>,
) -> Option<f64> {
    let known = duration.is_finite() && duration > 0.0;
    let elapsed = if elapsed.is_finite() { elapsed.max(0.0) } else { 0.0 };
    let elapsed = if known { elapsed.min(duration) } else { elapsed };
    let (id, bounds) = ui.allocate_space(vec2(ui.available_width(), HIT_HEIGHT + 15.0));
    let hit = Rect::from_min_size(bounds.min, vec2(bounds.width(), HIT_HEIGHT));
    let inset = (hit.width() * 0.5).min(13.0);
    let left = hit.left() + inset;
    let right = hit.right() - inset;
    let enabled = can_seek && known && ui.is_enabled() && right > left;

    // Slider's drag-only sense misses taps whose press and release land in one frame.
    // Use click-and-drag on the rail, exposing slider semantics rather than a button.
    let sense = if enabled { Sense::click_and_drag() } else if known {
        // Pending server commands temporarily disable seeking, not keyboard focus.
        Sense::focusable_noninteractive()
    } else { Sense::hover() };
    let mut response = ui.interact(hit, id, sense);
    let before = preview.unwrap_or(elapsed);
    let primary = PointerButton::Primary;
    let pointer_value = || {
        response.interact_pointer_pos().map(|pointer| {
            f64::from(((pointer.x - left) / (right - left)).clamp(0.0, 1.0)) * duration
        })
    };
    let commit = if !enabled {
        *preview = None;
        None
    } else if response.dragged_by(primary) {
        if response.drag_started() {
            response.request_focus();
        }
        *preview = pointer_value().or(*preview);
        None
    } else {
        let released = if response.drag_stopped_by(primary) && preview.is_some() {
            pointer_value().or(*preview)
        } else if response.clicked_by(primary) {
            pointer_value()
        } else {
            None
        };
        if released.is_some() {
            response.request_focus();
        }
        *preview = None;
        released.or_else(|| keyboard_seek(ui, &response, elapsed, duration))
    };
    let shown = commit.or(*preview).unwrap_or(elapsed);
    if commit.is_some() || shown != before {
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::slider(enabled, shown, "Playback position"));
    if enabled && (response.hovered() || preview.is_some()) {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    if ui.is_rect_visible(bounds) {
        let rail = Rect::from_min_max(
            pos2(left - 4.0, hit.center().y - 3.5),
            pos2(right + 4.0, hit.center().y + 3.5),
        ).intersect(hit);
        let painter = ui.painter();
        if enabled && response.has_focus() {
            painter.rect_stroke(hit.shrink(0.5), 8, Stroke::new(1.0, theme::accent(ui).gamma_multiply(0.55)), StrokeKind::Inside);
        }
        painter.rect_filled(rail, 4, theme::line(ui));
        if known {
            let center = pos2(egui::lerp(left..=right, (shown / duration).clamp(0.0, 1.0) as f32), rail.center().y);
            let animation = playing.then(|| {
                ui.ctx().request_repaint_after(Duration::from_millis(33));
                ui.input(|input| input.time)
            });
            let tint = if enabled { theme::accent(ui) } else { MUTED };
            if shown > 0.0 {
                let fill = Rect::from_min_max(rail.min, pos2((center.x + 4.0).min(rail.right()), rail.bottom()));
                paint_fill(painter, fill, tint, enabled, animation);
            }
            paint_handle(painter, center, tint, enabled, preview.is_some() || response.hovered(), animation);
        }
        let font = FontId::monospace(12.0);
        painter.text(pos2(hit.left(), hit.bottom()), Align2::LEFT_TOP, time(shown), font.clone(), MUTED);
        let total = if known { time(duration) } else { "--:--".into() };
        painter.text(pos2(hit.right(), hit.bottom()), Align2::RIGHT_TOP, total, font, MUTED);
    }
    commit
}

fn keyboard_seek(ui: &egui::Ui, response: &Response, elapsed: f64, duration: f64) -> Option<f64> {
    if !response.has_focus() {
        return None;
    }
    ui.memory_mut(|memory| {
        memory.set_focus_lock_filter(response.id, EventFilter {
            horizontal_arrows: true,
            vertical_arrows: true,
            ..Default::default()
        });
    });
    ui.input_mut(|input| {
        let home = input.consume_key(Modifiers::NONE, Key::Home);
        let end = input.consume_key(Modifiers::NONE, Key::End);
        let forward = input.count_and_consume_key(Modifiers::NONE, Key::ArrowRight)
            + input.count_and_consume_key(Modifiers::NONE, Key::ArrowUp);
        let backward = input.count_and_consume_key(Modifiers::NONE, Key::ArrowLeft)
            + input.count_and_consume_key(Modifiers::NONE, Key::ArrowDown);
        if home {
            Some(0.0)
        } else if end {
            Some(duration)
        } else if forward != backward {
            Some((elapsed + (forward as f64 - backward as f64) * 5.0).clamp(0.0, duration))
        } else {
            None
        }
    })
}

fn paint_fill(painter: &egui::Painter, fill: Rect, tint: Color32, enabled: bool, animation: Option<f64>) {
    let strength = if enabled { 1.0 } else { 0.5 };
    painter.rect_filled(fill.expand2(vec2(1.0, 4.0)), 8, tint.gamma_multiply(0.10 * strength));
    painter.rect_filled(fill.expand2(vec2(0.0, 2.0)), 6, tint.gamma_multiply(0.23 * strength));
    painter.rect_filled(fill, 4, tint.gamma_multiply(0.96 * strength));
    let sheen = Rect::from_min_max(fill.min + vec2(3.0, 1.0), pos2(fill.right() - 3.0, fill.top() + 3.0));
    painter.rect_filled(sheen, 1, tint.lerp_to_gamma(Color32::WHITE, 0.78).gamma_multiply(0.30 * strength));

    if let Some(time) = animation.filter(|_| fill.width() >= 32.0) {
        // Overlapping rounded bands read as a soft reflection, without a custom mesh.
        const WASH: f32 = 88.0;
        let x = fill.left() - WASH * 0.6 + (time * 112.0).rem_euclid(f64::from(fill.width() + WASH * 1.2)) as f32;
        let clipped = painter.with_clip_rect(fill.shrink2(vec2(3.0, 1.5)));
        for (width, alpha) in [(WASH, 0.11), (54.0, 0.20), (26.0, 0.27)] {
            let band = Rect::from_center_size(pos2(x, fill.center().y), vec2(width, 4.0));
            if band.intersects(clipped.clip_rect()) {
                clipped.rect_filled(band, 2, tint.lerp_to_gamma(Color32::WHITE, 0.78).gamma_multiply(alpha * strength));
            }
        }
    }
}

fn paint_handle(
    painter: &egui::Painter,
    center: egui::Pos2,
    tint: Color32,
    enabled: bool,
    active: bool,
    animation: Option<f64>,
) {
    let pulse = animation.map_or(0.0, |time| {
        let phase = (time / 2.4).fract() as f32 * std::f32::consts::TAU;
        0.5 + 0.5 * phase.sin()
    });
    let strength = if enabled { if active { 1.25 } else { 1.0 } } else { 0.45 };
    painter.circle_filled(center, 11.5 + 1.5 * pulse, tint.gamma_multiply((0.09 + 0.04 * pulse) * strength));
    painter.circle_filled(center, 9.5, tint.gamma_multiply((0.22 + 0.05 * pulse) * strength));
    painter.circle_filled(center, 6.5, tint.gamma_multiply(if enabled { 1.0 } else { 0.65 }));
    painter.circle_filled(center, 4.5, tint.lerp_to_gamma(Color32::WHITE, 0.78).gamma_multiply(if enabled { 1.0 } else { 0.45 }));
}

fn time(seconds: f64) -> String {
    let seconds = seconds as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
