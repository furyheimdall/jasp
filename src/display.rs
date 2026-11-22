use super::{read_preference, save_preference, tr};
use eframe::egui::{self, Event, RawInput, Ui};
use crate::theme;
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}, time::{Duration, Instant}};
use x11rb::{connection::RequestConnection, protocol::{dpms::{self, ConnectionExt as _}, xproto::{ConnectionExt as _, GetScreenSaverReply}}, rust_connection::RustConnection};

const FILE: &str = "display.json";

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    #[default]
    System,
    On,
    Off,
    Dim,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::System => "System default",
            Self::On => "Always on",
            Self::Off => "Turn off after inactivity",
            Self::Dim => "Dim after inactivity",
        }
    }
    fn timed(self) -> bool { matches!(self, Self::Off | Self::Dim) }
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
struct Preference {
    mode: Mode,
    minutes: u8,
    brightness: Option<u8>,
    dim: u8,
}

impl Default for Preference {
    fn default() -> Self { Self { mode: Mode::System, minutes: 10, brightness: None, dim: 20 } }
}

impl Preference {
    fn load() -> Self {
        let mut p = read_preference(FILE).and_then(|saved| serde_json::from_str::<Self>(&saved).ok()).unwrap_or_default();
        p.minutes = p.minutes.clamp(1, 120);
        p.brightness = p.brightness.map(|percent| percent.clamp(1, 100));
        p.dim = p.dim.clamp(1, 100);
        p
    }
    fn timeout(self) -> Duration { Duration::from_secs(u64::from(self.minutes) * 60) }
}

struct Backlight {
    path: PathBuf,
    max: u32,
    normal: u32,
    initial: u32,
    current: u32,
}

fn number(path: &Path) -> Option<u32> { fs::read_to_string(path).ok()?.trim().parse().ok() }

impl Backlight {
    fn detect() -> Option<Self> {
        let mut available = fs::read_dir("/sys/class/backlight").ok()?.flatten()
            .filter_map(|entry| Self::open(&entry.path()));
        let first = available.next()?;
        available.next().is_none().then_some(first)
    }

    fn open(dir: &Path) -> Option<Self> {
        let max = number(&dir.join("max_brightness")).filter(|&v| v > 0)?;
        let path = dir.join("brightness");
        let initial = number(&path).filter(|&v| v <= max)?;
        fs::OpenOptions::new().write(true).open(&path).ok()?;
        let normal = if initial == 0 { max } else { initial };
        Some(Self { path, max, normal, initial, current: initial })
    }

    fn units(&self, percent: u8) -> u32 {
        ((u64::from(self.max) * u64::from(percent) + 50) / 100).max(1) as u32
    }

    fn percent(&self, units: u32) -> u8 {
        ((u64::from(units) * 100 + u64::from(self.max / 2)) / u64::from(self.max)).max(1) as u8
    }

    fn set(&mut self, level: u32) -> std::io::Result<()> {
        if self.current != level {
            fs::write(&self.path, level.to_string())?;
            self.current = level;
        }
        Ok(())
    }
}

impl Drop for Backlight {
    fn drop(&mut self) {
        let initial = if self.initial == 0 { self.normal } else { self.initial };
        if self.current != self.initial { let _ = self.set(initial); }
    }
}

// Own X11 idle timers only in a dedicated Jasp session and only while its policy is selected.
struct X11Timers {
    conn: RustConnection,
    saver: GetScreenSaverReply,
    dpms_enabled: bool,
}

impl X11Timers {
    fn suspend() -> anyhow::Result<Self> {
        let (conn, _) = x11rb::connect(None)?;
        let saver = conn.get_screen_saver()?.reply()?;
        let dpms_enabled = conn.extension_information(dpms::X11_EXTENSION_NAME)?.is_some()
            && conn.dpms_info()?.reply()?.state;
        let guard = Self { conn, saver, dpms_enabled };
        guard.conn.set_screen_saver(0, 0, guard.saver.prefer_blanking, guard.saver.allow_exposures)?.check()?;
        if guard.dpms_enabled { guard.conn.dpms_disable()?.check()?; }
        Ok(guard)
    }
}

impl Drop for X11Timers {
    fn drop(&mut self) {
        if let Ok(cookie) = self.conn.set_screen_saver(self.saver.timeout as i16, self.saver.interval as i16,
            self.saver.prefer_blanking, self.saver.allow_exposures) { let _ = cookie.check(); }
        if self.dpms_enabled && let Ok(cookie) = self.conn.dpms_enable() { let _ = cookie.check(); }
    }
}

pub(crate) struct Display {
    x11: bool,
    panel: Option<Backlight>,
    x11_timers: Option<X11Timers>,
    applied: Preference,
    save_at: Option<Instant>,
    last_input: Instant,
    asleep: bool,
    wake_until: Option<Instant>,
    error: Option<&'static str>,
}

impl Display {
    pub(crate) fn new(x11: bool, lite: bool) -> Self {
        let panel = (x11 || lite).then(Backlight::detect).flatten();
        let mut display = Self { x11, panel, x11_timers: None, applied: Preference::default(), save_at: None,
            last_input: Instant::now(), asleep: false, wake_until: None, error: None };
        if display.available() {
            display.apply(Preference::load());
        }
        display
    }

    fn available(&self) -> bool { self.x11 || self.panel.is_some() }

    fn supports(&self, mode: Mode) -> bool {
        match mode {
            Mode::System => true,
            Mode::On => self.x11,
            Mode::Off | Mode::Dim => self.panel.is_some(),
        }
    }

    fn apply(&mut self, mut next: Preference) -> bool {
        if !self.supports(next.mode) { next.mode = Mode::System; }
        if self.x11 && next.mode != Mode::System && self.x11_timers.is_none() {
            match X11Timers::suspend() {
                Ok(timers) => self.x11_timers = Some(timers),
                Err(_) => { self.error = Some("Could not change the X11 idle timers."); return false; }
            }
        }
        if let Some(panel) = &mut self.panel
            && (next.mode.timed() || next.brightness.is_some() || self.asleep) {
            let target = next.brightness.map_or(panel.normal, |p| panel.units(p));
            if panel.set(target).is_err() {
                if self.applied.mode == Mode::System { self.x11_timers = None; }
                self.error = Some("Could not set the display brightness.");
                return false;
            }
            panel.normal = target;
        }
        if next.mode == Mode::System { self.x11_timers = None; }
        self.applied = next;
        self.asleep = false;
        self.last_input = Instant::now();
        self.error = None;
        true
    }

    fn save(&mut self) {
        self.save_at = None;
        if save_preference(FILE, &serde_json::to_string(&self.applied).expect("display preference serializes")).is_err() {
            self.error = Some("Could not save display settings.");
        } else if self.error == Some("Could not save display settings.") {
            self.error = None;
        }
    }

    pub(crate) fn show(&mut self, ui: &mut Ui) {
        if !self.available() {
            ui.label(tr(ui, "This display is managed by the desktop session."));
            return;
        }
        let mut next = self.applied;
        let mut dragging = false;
        let mut released = false;
        let mut slider = |ui: &mut Ui, label: &str, value: &mut u8, max: u8, unit: &str| {
            let result = touch_slider(ui, label, value, max, unit);
            dragging |= result.dragged();
            released |= result.drag_stopped();
        };
        if let Some(panel) = &self.panel {
            let brightness = next.brightness.unwrap_or_else(|| panel.percent(panel.normal));
            let mut value = brightness;
            display_card(ui, |ui| slider(ui, "Brightness", &mut value, 100, "%"));
            if value != brightness { next.brightness = Some(value); }
            ui.add_space(12.0);
        }
        display_card(ui, |ui| {
            ui.label(egui::RichText::new(tr(ui, "When idle")).size(20.0).strong());
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                for mode in [Mode::System, Mode::On, Mode::Off, Mode::Dim] {
                    if self.supports(mode) && mode_button(ui, mode, next.mode == mode).clicked() {
                        next.mode = mode;
                    }
                }
            });
            let timed = ui.ctx().animate_bool_with_time(ui.id().with("idle-timing"), next.mode.timed(), 0.20);
            let dim = ui.ctx().animate_bool_with_time(ui.id().with("idle-dim"), next.mode == Mode::Dim, 0.20);
            if next.mode.timed() {
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(6.0);
                ui.scope(|ui| {
                    ui.add_space(8.0 * (1.0 - timed));
                    ui.multiply_opacity(0.58 + 0.42 * timed);
                    slider(ui, "Idle minutes", &mut next.minutes, 120, tr(ui, " min").as_ref());
                    if next.mode == Mode::Dim {
                        ui.add_space(9.0);
                        ui.scope(|ui| {
                            ui.add_space(8.0 * (1.0 - dim));
                            ui.multiply_opacity(0.58 + 0.42 * dim);
                            slider(ui, "Dimmed brightness", &mut next.dim, 100, "%");
                        });
                    }
                    ui.add_space(3.0);
                    ui.label(egui::RichText::new(tr(ui, "Touch the screen to wake it.")).color(crate::ui::MUTED));
                });
            }
        });
        if next != self.applied && self.apply(next) {
            if dragging && !released {
                // Brightness changes as the thumb moves. Avoid syncing the preference on each frame.
                self.save_at = Some(Instant::now() + Duration::from_millis(250));
            } else { self.save(); }
            ui.ctx().request_repaint();
        } else if released && self.save_at.is_some() {
            self.save();
        }
        if let Some(error) = self.error { ui.colored_label(crate::ui::WARNING, tr(ui, error)); }
    }

    pub(crate) fn input(&mut self, raw: &mut RawInput) {
        if !self.applied.mode.timed() { return; }
        let awake_input = raw.events.iter().any(|e| matches!(e, Event::PointerButton { .. } | Event::PointerMoved(_)
            | Event::Key { .. } | Event::Touch { .. } | Event::MouseWheel { .. }));
        if !awake_input { return; }
        let now = Instant::now();
        self.last_input = now;
        if self.asleep {
            let Some(panel) = &mut self.panel else { return; };
            if panel.set(panel.normal).is_err() {
                self.error = Some("Could not set the display brightness.");
                return;
            }
            self.asleep = false;
            self.wake_until = Some(now + Duration::from_millis(300));
        }
        // Discard the waking gesture, including its synthetic pointer events and release.
        if self.wake_until.is_some_and(|until| now < until) { raw.events.clear(); }
        else { self.wake_until = None; }
    }

    pub(crate) fn keep_awake(&mut self) { self.last_input = Instant::now(); }

    pub(crate) fn tick(&mut self, ctx: &egui::Context) {
        if let Some(at) = self.save_at {
            let remaining = at.saturating_duration_since(Instant::now());
            if remaining.is_zero() { self.save(); } else { ctx.request_repaint_after(remaining); }
        }
        if self.asleep || !self.applied.mode.timed() { return; }
        let remaining = self.applied.timeout().saturating_sub(self.last_input.elapsed());
        if !remaining.is_zero() { ctx.request_repaint_after(remaining); return; }
        let Some(panel) = &mut self.panel else { return; };
        let level = if self.applied.mode == Mode::Dim { panel.units(self.applied.dim).min(panel.normal) } else { 0 };
        match panel.set(level) {
            Ok(()) => self.asleep = true,
            Err(_) => { self.error = Some("Could not set the display brightness."); self.last_input = Instant::now(); }
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        if self.save_at.is_some() { self.save(); }
    }
}

fn display_card(ui: &mut Ui, content: impl FnOnce(&mut Ui)) {
    let width = ui.available_width().min(790.0);
    egui::Frame::new().fill(theme::card(ui)).stroke(egui::Stroke::new(1.0, theme::line(ui)))
        .corner_radius(14).inner_margin(14).show(ui, |ui| {
            ui.set_width((width - 30.0).max(0.0));
            content(ui);
        });
}

fn mode_button(ui: &mut Ui, mode: Mode, selected: bool) -> egui::Response {
    let accent = theme::accent(ui);
    let phase = ui.ctx().animate_bool_with_time(ui.id().with(mode.label()), selected, 0.20);
    let text = egui::RichText::new(tr(ui, mode.label())).size(17.0).strong()
        .color(egui::Color32::WHITE.lerp_to_gamma(accent, phase));
    let control = egui::Button::selectable(selected, text).min_size(egui::vec2(104.0, 54.0))
        .corner_radius(10)
        .fill(theme::raised(ui).lerp_to_gamma(accent, phase * 0.16))
        .stroke(egui::Stroke::new(1.0 + 0.5 * phase, theme::line(ui).lerp_to_gamma(accent, phase)));
    ui.add(control)
}

fn touch_slider(ui: &mut Ui, label: &str, value: &mut u8, max: u8, unit: &str) -> egui::Response {
    let width = ui.available_width().min(620.0);
    let accent = theme::accent(ui);
    ui.push_id(label, |ui| {
        let caption = ui.horizontal(|ui| {
            let caption = ui.label(egui::RichText::new(tr(ui, label)).size(19.0).strong());
            egui::Frame::new().fill(theme::raised(ui)).corner_radius(8).inner_margin(egui::Margin::symmetric(9, 3))
                .show(ui, |ui| ui.label(egui::RichText::new(format!("{value}{unit}")).size(22.0).strong().color(accent)));
            caption.id
        }).inner;
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let step = egui::vec2(54.0, 54.0);
            let step_button = |ui: &mut Ui, symbol| {
                ui.add_sized(step, egui::Button::new(egui::RichText::new(symbol).size(24.0)).corner_radius(10))
            };
            if step_button(ui, "−").clicked() { *value = value.saturating_sub(1).max(1); }
            let response = ui.scope(|ui| {
                let spacing = ui.spacing_mut();
                spacing.interact_size.y = 54.0;
                spacing.slider_width = (width - 2.0 * (step.x + spacing.item_spacing.x)).max(1.0);
                let visuals = ui.visuals_mut();
                visuals.widgets.inactive.fg_stroke.color = accent;
                visuals.widgets.hovered.fg_stroke.color = accent;
                visuals.widgets.active.fg_stroke.color = accent;
                ui.add(egui::Slider::new(value, 1..=max).show_value(false)
                    .trailing_fill(true).smart_aim(false).step_by(1.0)).labelled_by(caption)
            }).inner;
            if step_button(ui, "+").clicked() { *value = value.saturating_add(1).min(max); }
            response
        }).inner
    }).inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_dim_and_off_restore_normal_brightness_on_input() {
        let dir = std::env::temp_dir().join(format!("jasp-backlight-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(dir.clone());
        fs::write(dir.join("max_brightness"), "31").unwrap();
        fs::write(dir.join("brightness"), "25").unwrap();
        let mut display = Display {
            x11: false, panel: Backlight::open(&dir), x11_timers: None, applied: Preference::default(),
            save_at: None, last_input: Instant::now(), asleep: false, wake_until: None, error: None,
        };
        let ctx = egui::Context::default();
        for (mode, expected) in [(Mode::Dim, 6), (Mode::Off, 0)] {
            assert!(display.apply(Preference { mode, ..display.applied }));
            display.last_input -= display.applied.timeout();
            display.tick(&ctx);
            assert_eq!(number(&dir.join("brightness")), Some(expected));
            let mut raw = RawInput { events: vec![Event::PointerMoved(egui::pos2(12.0, 20.0))], ..Default::default() };
            display.input(&mut raw);
            assert!(raw.events.is_empty(), "the wake gesture must not reach playback controls");
            assert_eq!(number(&dir.join("brightness")), Some(25));
        }
        assert!(display.apply(Preference { brightness: Some(50), ..display.applied }));
        assert_eq!(number(&dir.join("brightness")), Some(16));
        drop(display);
        assert_eq!(number(&dir.join("brightness")), Some(25));
    }
}
