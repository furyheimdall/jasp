use crate::{settings::tr, theme, ui::{MUTED, centered_button}};
use eframe::egui::{self, Align2, Color32, FontId, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, vec2};

pub(crate) const WIDTH: f32 = 790.0;
const TOUCH: f32 = 54.0;

pub(crate) fn card<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    let width = ui.available_width().min(WIDTH);
    let result = egui::Frame::new().fill(theme::card(ui)).stroke(Stroke::new(1.0, theme::line(ui)))
        .corner_radius(14).inner_margin(14).show(ui, |ui| {
            ui.set_width((width - 30.0).max(0.0));
            content(ui)
        }).inner;
    ui.add_space(10.0);
    result
}

pub(crate) fn section<R>(ui: &mut Ui, title: &str, content: impl FnOnce(&mut Ui) -> R) -> R {
    card(ui, |ui| {
        heading(ui, title);
        ui.add_space(6.0);
        content(ui)
    })
}

pub(crate) fn heading(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(tr(ui, title)).size(20.0).strong());
}

pub(crate) fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(tr(ui, text)).size(14.0).color(MUTED));
}

pub(crate) fn button(ui: &mut Ui, label: &str, primary: bool) -> Response {
    let border = if primary { theme::accent(ui) } else { theme::line(ui) };
    ui.add(centered_button(RichText::new(tr(ui, label)).size(17.0).strong())
        .min_size(vec2(84.0, TOUCH)).corner_radius(10).stroke(Stroke::new(1.0, border)))
}

pub(crate) fn choice(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let tint = theme::accent(ui);
    let phase = ui.ctx().animate_bool_with_time(ui.id().with(label), selected, 0.20);
    let text = RichText::new(tr(ui, label)).size(17.0).strong()
        .color(ui.visuals().strong_text_color().lerp_to_gamma(tint, phase));
    ui.add(centered_button(text).selected(selected).frame(true)
        .min_size(vec2(104.0, TOUCH)).corner_radius(10)
        .fill(theme::raised(ui).lerp_to_gamma(tint, phase * 0.18))
        .stroke(Stroke::new(1.0 + 0.5 * phase, theme::line(ui).lerp_to_gamma(tint, phase))))
}

pub(super) fn swatch(ui: &mut Ui, label: &str, selected: bool, tint: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(42.0, 42.0), Sense::click());
    let label = tr(ui, label);
    response.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, ui.is_enabled(), selected, label.as_ref()));
    let phase = ui.ctx().animate_bool_with_time(response.id.with("selected"), selected, 0.18);
    let square = Rect::from_center_size(rect.center(), vec2(25.0, 25.0));
    let painter = ui.painter();
    painter.rect_filled(square, 5, tint);
    let focus = if response.hovered() || response.has_focus() { 0.55 } else { 0.0 };
    let opacity = phase.max(focus);
    if opacity > 0.0 {
        let ring = ui.visuals().strong_text_color().gamma_multiply(opacity);
        painter.rect_stroke(square.expand(3.5), 7, Stroke::new(1.5, ring), StrokeKind::Outside);
    }
    response.on_hover_text(label)
}

pub(super) fn toggle(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().min(460.0), 48.0), Sense::click());
    let label = tr(ui, label);
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), enabled, label.as_ref()));
    let accent = theme::accent(ui);
    let phase = ui.ctx().animate_bool_with_time(response.id, enabled, 0.18);
    let track = Rect::from_center_size(rect.right_center() - vec2(24.0, 0.0), vec2(48.0, 26.0));
    let painter = ui.painter();
    let border = if response.has_focus() || response.hovered() { accent } else { theme::line(ui) };
    painter.text(rect.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(18.0), ui.visuals().strong_text_color());
    painter.rect(track, 13, theme::raised(ui).lerp_to_gamma(accent, phase * 0.85), Stroke::new(1.0, border), StrokeKind::Inside);
    let thumb = track.left_center() + vec2(13.0 + phase * 22.0, 0.0);
    painter.circle_filled(thumb, 9.0, ui.visuals().strong_text_color().lerp_to_gamma(theme::ON_ACCENT, phase));
    response
}
