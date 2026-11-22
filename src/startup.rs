use crate::{autostart::{Autostart, Backend}, dialog, settings::tr, ui::{MUTED, WARNING, button}};
use eframe::egui::{self, RichText, Ui};
use std::{fs::OpenOptions, path::PathBuf, sync::mpsc::{self, Receiver}};

#[derive(Default)]
pub(crate) struct Startup {
    probe_started: bool,
    state: Option<State>,
    pending: Option<Receiver<Result<State, String>>>,
    error: Option<String>,
    prompt: Option<dialog::Animation>,
    console_request: Option<PathBuf>,
    return_to_console: bool,
}

#[derive(Clone, Copy)]
struct State { backend: Backend, enabled: bool }

fn start_job(ctx: &egui::Context, change: Option<bool>) -> Receiver<Result<State, String>> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<State> {
            let setup = Autostart::detect()?;
            if let Some(enabled) = change { setup.set(enabled, false)?; }
            Ok(State { backend: setup.backend, enabled: setup.enabled()? })
        })();
        let _ = tx.send(result.map_err(|error| format!("{error:#}")));
        ctx.request_repaint();
    });
    rx
}

impl Startup {
    pub(crate) fn poll(&mut self, ctx: &egui::Context, editable: bool) {
        if !self.probe_started {
            self.probe_started = true;
            if std::env::var("JASP_LITE_KIOSK").as_deref() == Ok("1") {
                self.console_request = std::env::var_os("JASP_KIOSK_PREVIEW_REQUEST")
                    .map(PathBuf::from).filter(|path| path.is_absolute());
            }
            if crate::autostart::installed() { self.pending = Some(start_job(ctx, None)); }
        }
        let Some(pending) = &self.pending else { return; };
        let initial = self.state.is_none();
        let result = match pending.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(String::from("Autostart worker stopped")),
        };
        self.pending = None;
        match result {
            Ok(state) => {
                self.state = Some(state);
                self.error = None;
                if !initial && state.enabled && let Some(animation) = &mut self.prompt {
                    animation.start_close();
                    ctx.request_repaint();
                }
                if initial && editable && !state.enabled && (state.backend != Backend::Lite || self.console_request.is_some()) {
                    self.prompt = Some(dialog::Animation::default());
                }
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn active(&self) -> bool { self.prompt.is_some() }

    fn change(&mut self, ctx: &egui::Context, enabled: bool) {
        if self.pending.is_none() {
            self.error = None;
            self.pending = Some(start_job(ctx, Some(enabled)));
        }
    }

    pub(crate) fn show_prompt(&mut self, ctx: &egui::Context) {
        let (Some(state), Some(animation)) = (self.state, &mut self.prompt) else { return; };
        let modal = dialog::show(ctx, egui::Id::new("jasp-autostart"), animation, |ui| {
            ui.set_width((ctx.content_rect().width() - 56.0).clamp(205.0, 465.0));
            ui.label(RichText::new(tr(ui, "Start Jasp automatically?")).size(23.0).strong());
            ui.add_space(7.0);
            let reason = match state.backend {
                Backend::Moode => "Jasp will replace the local moOde web screen on the next boot. The moOde server and playback settings will not change.",
                Backend::Desktop => "Jasp will open after you log in. Your desktop and other startup apps are unchanged.",
                Backend::Lite => "Jasp will open full screen at boot. Confirm here, then approve sudo in the console. Jasp will reopen after approval.",
            };
            ui.label(RichText::new(tr(ui, reason)).size(15.0).color(MUTED));
            if let Some(error) = &self.error { ui.label(RichText::new(error).color(WARNING)); }
            if self.pending.is_some() { ui.weak(tr(ui, "Updating autostart…")); }
            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enable = ui.add_enabled_ui(self.pending.is_none(), |ui| button(ui, "Enable autostart", true).clicked()).inner;
                let later = button(ui, "Not now", false).clicked();
                (enable, later)
            }).inner
        });
        let enable = animation.is_open() && modal.inner.0;
        if animation.is_open() && (modal.inner.1 || modal.should_close()) {
            animation.start_close();
            ctx.request_repaint();
        }
        if enable {
            if let Some(path) = &self.console_request {
                match OpenOptions::new().write(true).create_new(true).open(path) {
                    Ok(_) => {
                        self.return_to_console = true;
                        animation.start_close();
                        ctx.request_repaint();
                    }
                    Err(error) => self.error = Some(format!("Could not request console approval: {error}")),
                }
            } else { self.change(ctx, true); }
        }
        if self.prompt.as_ref().is_some_and(dialog::Animation::is_finished) {
            self.prompt = None;
            if self.return_to_console { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
        }
    }

    pub(crate) fn show(&mut self, ui: &mut Ui, editable: bool) {
        let Some(state) = self.state else {
            if let Some(error) = &self.error { ui.label(RichText::new(error).color(WARNING)); }
            return;
        };
        let moode_kiosk = state.backend == Backend::Moode && std::env::var("JASP_MOODE_KIOSK").as_deref() == Ok("1");
        ui.add_space(8.0);
        ui.label(RichText::new(tr(ui, if moode_kiosk { "Next boot" } else { "Autostart" })).strong());
        let choice = match state.backend {
            Backend::Moode | Backend::Desktop => {
                let options = if moode_kiosk { [crate::APP_NAME, "moOde Web UI"] } else { ["Enable autostart", "Disable autostart"] };
                ui.add_enabled_ui(editable && self.pending.is_none(), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        options.into_iter().zip([true, false]).filter_map(|(label, enable)|
                            button(ui, label, enable == state.enabled).clicked().then_some(enable)
                        ).last()
                    }).inner
                }).inner
            }
            Backend::Lite => {
                if editable && !state.enabled && self.console_request.is_some() && button(ui, "Enable autostart", true).clicked() {
                    self.prompt = Some(dialog::Animation::default());
                }
                ui.monospace(if state.enabled { "jasp autostart disable" } else { "jasp autostart enable" });
                None
            }
        };
        if let Some(enable) = choice && enable != state.enabled { self.change(ui.ctx(), enable); }
        let hint = if self.pending.is_some() { "Updating autostart…" }
            else if !editable { "Read-only mode cannot change autostart." }
            else if state.backend == Backend::Desktop { "Changes apply the next time you log in." }
            else { "Changes apply on the next boot. The current player stays open." };
        ui.weak(tr(ui, hint));
        if let Some(error) = &self.error { ui.label(RichText::new(error).color(WARNING)); }
    }
}
