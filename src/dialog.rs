use crate::theme;
use eframe::egui::{self, Color32, Ui};
use std::time::{Duration, Instant};

const OPEN_DURATION: Duration = Duration::from_millis(340);
const CLOSE_DURATION: Duration = Duration::from_millis(230);

#[derive(Default)]
pub(crate) struct Animation(Phase);

#[derive(Default)]
enum Phase {
    #[default]
    Waiting,
    Opening(Instant),
    Open,
    Closing(Instant),
    Finished,
}

impl Animation {
    pub(crate) fn start_close(&mut self) {
        if self.is_open() {
            self.0 = Phase::Closing(Instant::now());
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        matches!(self.0, Phase::Open)
    }

    pub(crate) fn is_finished(&self) -> bool {
        matches!(self.0, Phase::Finished)
    }
}

pub(crate) fn show<R>(ctx: &egui::Context, id: egui::Id, animation: &mut Animation, content: impl FnOnce(&mut Ui) -> R) -> egui::ModalResponse<R> {
    let (t, closing) = match animation.0 {
        Phase::Waiting => (0.0, false),
        Phase::Opening(started) => ((started.elapsed().as_secs_f32() / OPEN_DURATION.as_secs_f32()).min(1.0), false),
        Phase::Open => (1.0, false),
        Phase::Closing(started) => ((1.0 - started.elapsed().as_secs_f32() / CLOSE_DURATION.as_secs_f32()).max(0.0), true),
        Phase::Finished => (0.0, true),
    };
    if !animation.is_open() && !animation.is_finished() {
        ctx.request_repaint();
    }
    let eased = if closing { t } else { 1.0 - (1.0 - t).powi(3) };
    egui::Modal::new(id)
        .area(egui::Modal::default_area(id).fade_in(false))
        .backdrop_color(Color32::from_black_alpha((136.0 * eased) as u8))
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            if !ui.is_sizing_pass() {
                match animation.0 {
                    Phase::Waiting => animation.0 = Phase::Opening(Instant::now()),
                    Phase::Opening(_) if t == 1.0 => animation.0 = Phase::Open,
                    Phase::Closing(_) if t == 0.0 => animation.0 = Phase::Finished,
                    _ => {}
                }
            }
            let interactive = animation.is_open();
            let card = |ui: &mut Ui| {
                if !interactive {
                    let opacity = ui.opacity();
                    ui.disable();
                    // Disable hit targets without adding egui's extra disabled fade.
                    ui.set_opacity(opacity * if closing { eased } else { 0.48 + 0.52 * eased });
                }
                egui::Frame::new().fill(theme::card(ui))
                    .stroke(egui::Stroke::new(1.2, theme::line(ui)))
                    .corner_radius(18).inner_margin(20)
                    .shadow(egui::epaint::Shadow { offset: [0, 12], blur: 25, spread: 2, color: Color32::from_black_alpha(128) })
                    .show(ui, content).inner
            };
            if t == 1.0 {
                ui.scope(card).inner
            } else {
                let swing = t - 1.0;
                let zoom = if closing { 0.76 + 0.24 * t * t }
                    else { 0.80 + 0.20 * (1.0 + 2.5 * swing.powi(3) + 1.5 * swing.powi(2)) };
                let center = ctx.content_rect().center();
                let transform = egui::emath::TSTransform::new(center.to_vec2() * (1.0 - zoom), zoom);
                ui.with_visual_transform(transform, card).inner
            }
        })
}
