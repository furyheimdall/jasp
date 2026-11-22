use crate::theme::Palette;
use eframe::egui::{self, Align2, Color32, FontId, LayerId, Painter, Rect, Stroke, StrokeKind, vec2};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const DURATION: Duration = Duration::from_millis(1360);
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
static SHOWN: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
pub(crate) struct Splash(State);

#[derive(Default)]
enum State {
    #[default]
    Ready,
    Playing(Instant),
    Done,
}

impl Splash {
    /// Returns true while the splash owns this frame instead of the player UI.
    pub(crate) fn show(&mut self, ctx: &egui::Context, palette: Palette) -> bool {
        let (content, elapsed) = match self.0 {
            State::Ready => {
                let content = ctx.content_rect();
                if !content.is_positive() {
                    return true;
                }
                if SHOWN.swap(true, Ordering::Relaxed) {
                    self.0 = State::Done;
                    return false;
                }
                self.0 = State::Playing(Instant::now());
                (content, Duration::ZERO)
            }
            State::Playing(start) => {
                let elapsed = start.elapsed();
                if elapsed >= DURATION {
                    self.0 = State::Done;
                    return false;
                }
                (ctx.content_rect(), elapsed)
            }
            State::Done => return false,
        };

        ctx.request_repaint_after(FRAME_INTERVAL.min(DURATION - elapsed));
        let mut painter = Painter::new(ctx.clone(), LayerId::background(), ctx.viewport_rect());
        painter.rect_filled(painter.clip_rect(), 0, palette.background);
        if !content.is_positive() {
            return true;
        }

        let t = elapsed.as_secs_f32();
        let exit = smoothstep((DURATION - elapsed).as_secs_f32() / 0.34);
        let zoom = 1.0 - (1.0 - (t / 0.48).min(1.0)).powi(3);
        let fit = (content.width() / 176.0).min(content.height() / 144.0).min(1.0);
        let center = content.center();
        let mark_center = center + vec2(0.0, -23.0 * fit);
        let scale = (0.82 + 0.18 * zoom) * fit;
        let plate = Rect::from_center_size(mark_center, vec2(76.0, 76.0) * scale);
        painter.set_opacity((0.55 + 0.45 * zoom) * exit);
        painter.rect(
            plate,
            (19.0 * scale).round() as u8,
            palette.background.lerp_to_gamma(palette.accent, 0.09),
            Stroke::new(1.2 * scale, palette.background.lerp_to_gamma(palette.accent, 0.27)),
            StrokeKind::Inside,
        );

        let rise = 0.2 + 0.8 * smoothstep(t / 0.42);
        let points = [(-24.0, 2.0), (-8.0, -42.0), (8.0, 42.0), (24.0, -2.0)]
            .map(|(x, y)| mark_center + vec2(x, y * rise) * scale);
        painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
            points, false, Color32::TRANSPARENT, Stroke::new(3.5 * scale, palette.accent),
        ));

        let title = smoothstep((t - 0.14) / 0.34);
        if title > 0.0 {
            painter.set_opacity(title * exit);
            painter.text(
                center + vec2(0.0, (43.0 + 6.0 * (1.0 - title)) * fit),
                Align2::CENTER_CENTER,
                crate::APP_NAME,
                FontId::proportional(27.0 * fit),
                Color32::from_gray(240),
            );
        }
        true
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
