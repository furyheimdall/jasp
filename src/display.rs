use super::{read_preference, save_preference, tr};
use eframe::egui::{self, Event, RawInput, Ui};
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
    draft: Preference,
    last_input: Instant,
    asleep: bool,
    wake_until: Option<Instant>,
    error: Option<&'static str>,
}

impl Display {
    pub(crate) fn new(x11: bool, lite: bool) -> Self {
        let panel = (x11 || lite).then(Backlight::detect).flatten();
        let draft = Preference::load();
        let mut display = Self { x11, panel, x11_timers: None, applied: Preference::default(), draft,
            last_input: Instant::now(), asleep: false, wake_until: None, error: None };
        if display.available() {
            display.apply();
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

    fn apply(&mut self) {
        let mut next = self.draft;
        if !self.supports(next.mode) { next.mode = Mode::System; }
        if self.x11 && next.mode != Mode::System && self.x11_timers.is_none() {
            match X11Timers::suspend() {
                Ok(timers) => self.x11_timers = Some(timers),
                Err(_) => { self.error = Some("Could not change the X11 idle timers."); return; }
            }
        }
        if let Some(panel) = &mut self.panel
            && (next.mode.timed() || next.brightness.is_some() || self.asleep) {
            let target = next.brightness.map_or(panel.normal, |p| panel.units(p));
            if panel.set(target).is_err() {
                if self.applied.mode == Mode::System { self.x11_timers = None; }
                self.error = Some("Could not set the display brightness.");
                return;
            }
            panel.normal = target;
        }
        if next.mode == Mode::System { self.x11_timers = None; }
        self.applied = next;
        self.draft = next;
        self.asleep = false;
        self.last_input = Instant::now();
        self.error = None;
    }

    pub(crate) fn show(&mut self, ui: &mut Ui) {
        if !self.available() {
            ui.label(tr(ui, "This display is managed by the desktop session."));
            return;
        }
        if let Some(panel) = &self.panel {
            let mut brightness = self.draft.brightness.unwrap_or_else(|| panel.percent(panel.normal));
            if ui.add(egui::Slider::new(&mut brightness, 1..=100).suffix("%").text(tr(ui, "Brightness"))).changed() {
                self.draft.brightness = Some(brightness);
            }
        }
        ui.label(tr(ui, "When idle"));
        ui.horizontal_wrapped(|ui| {
            for mode in [Mode::System, Mode::On, Mode::Off, Mode::Dim] {
                if !self.supports(mode) { continue; }
                if crate::ui::button(ui, mode.label(), mode == self.draft.mode).clicked() { self.draft.mode = mode; }
            }
        });
        if self.draft.mode.timed() {
            ui.add(egui::Slider::new(&mut self.draft.minutes, 1..=120).text(tr(ui, "Idle minutes")));
            if self.draft.mode == Mode::Dim {
                ui.add(egui::Slider::new(&mut self.draft.dim, 1..=100).suffix("%").text(tr(ui, "Dimmed brightness")));
            }
            ui.label(tr(ui, "Touch the screen to wake it."));
        }
        if ui.add_enabled_ui(self.draft != self.applied, |ui| crate::ui::button(ui, "Apply display settings", true)).inner.clicked() {
            self.apply();
            if self.error.is_none() && serde_json::to_string(&self.applied).ok()
                .is_none_or(|value| save_preference(FILE, &value).is_err()) {
                self.error = Some("Could not save display settings.");
            }
            ui.ctx().request_repaint();
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
            draft: Preference::default(), last_input: Instant::now(), asleep: false, wake_until: None, error: None,
        };
        let ctx = egui::Context::default();
        for (mode, expected) in [(Mode::Dim, 6), (Mode::Off, 0)] {
            display.draft.mode = mode;
            display.apply();
            display.last_input -= display.applied.timeout();
            display.tick(&ctx);
            assert_eq!(number(&dir.join("brightness")), Some(expected));
            let mut raw = RawInput { events: vec![Event::PointerMoved(egui::pos2(12.0, 20.0))], ..Default::default() };
            display.input(&mut raw);
            assert!(raw.events.is_empty(), "the wake gesture must not reach playback controls");
            assert_eq!(number(&dir.join("brightness")), Some(25));
        }
        display.draft.brightness = Some(50);
        display.apply();
        assert_eq!(number(&dir.join("brightness")), Some(16));
        drop(display);
        assert_eq!(number(&dir.join("brightness")), Some(25));
    }
}
