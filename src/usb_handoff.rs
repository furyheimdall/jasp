use crate::{
    dialog::{self, Animation},
    ja::{Action, UsbPlay},
    ja_ui::{UsbNotice, View},
    model::Command,
    settings::tr,
    theme, ui::{MUTED, WARNING},
};
use eframe::egui::{self, Align, Color32, RichText, Ui};
use std::sync::mpsc::SyncSender;

#[derive(Default)]
pub(super) struct Handoff {
    state: Option<State>,
    sequence: u64,
    animation: Animation,
}

enum State {
    ConfirmJastreamer { play: UsbPlay, device: Option<String> },
    ConfirmMoode(Command),
    Preparing { request: u64, device: String },
    Yielding(u64),
    Starting(u64),
    Releasing { request: u64, play: Option<Command> },
    Failed(String),
}

#[derive(PartialEq, Eq)]
enum Choice { Wait, Cancel, Confirm, Rescan }

impl Handoff {
    pub(super) fn active(&self) -> bool { self.state.is_some() }

    pub(super) fn to_jastreamer(&mut self, play: UsbPlay, ja: &mut View) {
        if self.active() { return; }
        let device = ja.usb_device().map(str::to_owned);
        ja.scan_usb();
        self.state = Some(State::ConfirmJastreamer { play, device });
        self.animation = Animation::default();
    }

    pub(super) fn to_moode(&mut self, play: Command) {
        if !self.active() {
            self.state = Some(State::ConfirmMoode(play));
            self.animation = Animation::default();
        }
    }

    pub(super) fn yielded(&mut self, request: u64, result: Result<(), String>, ja: &mut View) {
        if !matches!(self.state, Some(State::Yielding(expected)) if expected == request) { return; }
        if let Err(error) = result { self.fail(error, ja); }
        else if ja.send(Action::CommitUsbPlay { request }) { self.state = Some(State::Starting(request)); }
        else { self.fail("Could not send the jastreamer playback request.", ja); }
    }

    pub(super) fn jastreamer(&mut self, notice: UsbNotice, ja: &mut View, moode: &SyncSender<Command>) {
        // A closing success still displays its progress state, but no longer accepts replies.
        if !self.animation.is_open() { return; }
        match notice {
            UsbNotice::Prepared(request, result)
                if matches!(self.state, Some(State::Preparing { request: expected, .. }) if expected == request) => {
                let Some(State::Preparing { device, .. }) = self.state.take() else { unreachable!() };
                if let Err(error) = result { self.fail(error, ja); }
                else if moode.try_send(Command::YieldUsb { request, device }).is_ok() {
                    self.state = Some(State::Yielding(request));
                } else { self.fail("Could not ask moOde to release the USB DAC.", ja); }
            }
            UsbNotice::Committed(request, result)
                if matches!(self.state, Some(State::Starting(expected)) if expected == request) => {
                if let Err(error) = result { self.fail(error, ja); }
                else { self.animation.start_close(); }
            }
            UsbNotice::Released(request, result) => {
                if let Some(State::Releasing { request: expected, play }) = &mut self.state
                    && *expected == request
                    && let Some(play) = play.take() {
                    if let Err(error) = result { self.fail(error, ja); }
                    else if moode.try_send(play).is_err() { self.fail("Could not send the moOde playback request.", ja); }
                    else { self.animation.start_close(); }
                }
            }
            _ => {}
        }
    }

    pub(super) fn show(&mut self, ctx: &egui::Context, ja: &mut View, moode_ready: bool) {
        let Some(state) = &mut self.state else { return; };
        let dismissible = matches!(state, State::ConfirmJastreamer { .. } | State::ConfirmMoode(_));
        let modal = dialog::show(ctx, egui::Id::new("shared-usb-playback"), &mut self.animation, |ui| {
            ui.set_width(400.0_f32.min(ctx.content_rect().width() - 64.0).max(1.0));
            match state {
                State::ConfirmJastreamer { device, .. } => {
                    title(ui, "Switch USB output to jastreamer?");
                    mode_path(ui, "moOde", "jastreamer");
                    description(ui, "If moOde is using this DAC, its playback will stop. The requested track will then play in jastreamer.");
                    ui.add_space(12.0);
                    let devices = ja.usb_devices();
                    if device.as_ref().is_some_and(|selected| !devices.contains(selected)) { *device = None; }
                    if device.is_none() && let [only] = devices { *device = Some(only.clone()); }
                    if devices.is_empty() {
                        ui.label(tr(ui, "No USB playback device detected."));
                        if action_button(ui, "Rescan USB", false).clicked() { return Choice::Rescan; }
                    } else {
                        let prompt = tr(ui, "Choose a USB DAC");
                        egui::ComboBox::from_id_salt("handoff-usb-device").width(234.0)
                            .selected_text(device.as_deref().map(device_label).unwrap_or(prompt.as_ref())).show_ui(ui, |ui| {
                                for available in devices {
                                    if ui.selectable_label(device.as_ref() == Some(available), device_label(available)).clicked() {
                                        *device = Some(available.clone());
                                    }
                                }
                            });
                    }
                    if !moode_ready { ui.label(tr(ui, "moOde is unavailable; the DAC cannot be handed over safely.")); }
                    confirmation(ui, device.is_some() && moode_ready)
                }
                State::ConfirmMoode(_) => {
                    title(ui, "Switch USB output to moOde?");
                    mode_path(ui, "jastreamer", "moOde");
                    description(ui, "The local jastreamer output will close before moOde starts the requested track. The server queue is kept.");
                    if !moode_ready { ui.label(tr(ui, "moOde is unavailable; the DAC cannot be handed over safely.")); }
                    confirmation(ui, moode_ready)
                }
                State::Preparing { .. } => progress(ui, "Checking the jastreamer track and USB output…"),
                State::Yielding(_) => progress(ui, "Waiting for moOde to release the USB DAC…"),
                State::Starting(_) => progress(ui, "Starting jastreamer on the USB DAC…"),
                State::Releasing { .. } => progress(ui, "Closing the jastreamer USB output…"),
                State::Failed(error) => {
                    title(ui, "Could not switch USB output");
                    ui.label(RichText::new(tr(ui, error)).color(WARNING));
                    ui.add_space(16.0);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if action_button(ui, "Acknowledge", true).clicked() { Choice::Cancel } else { Choice::Wait }
                    }).inner
                }
            }
        });
        if self.animation.is_finished() {
            // Failure already cleared playback pending when it was reported.
            if !matches!(state, State::Failed(_)) { ja.finish_usb_play(); }
            self.state = None;
            return;
        }
        if !self.animation.is_open() { return; }
        if (dismissible && modal.should_close()) || modal.inner == Choice::Cancel {
            self.animation.start_close();
            ctx.request_repaint();
        }
        else if modal.inner == Choice::Confirm { self.confirm(ja); }
        else if modal.inner == Choice::Rescan { ja.scan_usb(); }
    }

    fn confirm(&mut self, ja: &mut View) {
        self.sequence += 1;
        let request = self.sequence;
        match self.state.take() {
            Some(State::ConfirmJastreamer { play, device: Some(device) }) => {
                if ja.send(Action::PrepareUsbPlay { request, device: device.clone(), play }) {
                    self.state = Some(State::Preparing { request, device });
                } else { self.fail("Could not prepare jastreamer playback.", ja); }
            }
            Some(State::ConfirmMoode(play)) => {
                if ja.send(Action::ReleaseUsbForMoode { request }) {
                    self.state = Some(State::Releasing { request, play: Some(play) });
                } else { self.fail("Could not ask jastreamer to release the USB DAC.", ja); }
            }
            _ => unreachable!("only a ready confirmation can start a USB handoff"),
        }
    }

    fn fail(&mut self, error: impl Into<String>, ja: &mut View) {
        ja.finish_usb_play();
        self.state = Some(State::Failed(error.into()));
    }
}

fn confirmation(ui: &mut Ui, ready: bool) -> Choice {
    ui.add_space(15.0);
    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
        let confirm = ui.add_enabled_ui(ready, |ui| action_button(ui, "Switch and play", true)).inner.clicked();
        let cancel = action_button(ui, "Cancel", false).clicked();
        if confirm && ready { Choice::Confirm }
        else if cancel { Choice::Cancel }
        else { Choice::Wait }
    }).inner
}

fn title(ui: &mut Ui, label: &str) {
    ui.label(RichText::new(tr(ui, "USB audio")).size(12.0).strong().color(theme::accent(ui)));
    ui.add_space(4.0);
    ui.label(RichText::new(tr(ui, label)).size(24.0).strong().color(Color32::from_rgb(235, 248, 246)));
    ui.add_space(5.0);
}

fn description(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(tr(ui, text)).size(15.0).color(MUTED));
}

fn mode_path(ui: &mut Ui, from: &str, to: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(from).size(15.0).strong().color(MUTED));
        ui.label(RichText::new("→").size(18.0).color(theme::accent(ui)));
        ui.label(RichText::new(to).size(15.0).strong().color(theme::accent(ui)));
    });
    ui.add_space(6.0);
}

fn device_label(device: &str) -> &str {
    device.strip_prefix("hw:CARD=").and_then(|device| device.split_once(",DEV="))
        .map_or(device, |(name, _)| name)
}

fn action_button(ui: &mut Ui, text: &str, primary: bool) -> egui::Response {
    let normal = theme::raised(ui);
    let (fill, text_color) = if primary { (theme::accent(ui), theme::ON_ACCENT) }
        else { (normal, MUTED) };
    ui.add(crate::ui::centered_button(RichText::new(tr(ui, text)).size(15.0).strong().color(text_color))
        .fill(fill).min_size(egui::vec2(if primary { 139.0 } else { 86.0 }, 43.0)).corner_radius(12))
}

fn progress(ui: &mut Ui, label: &str) -> Choice {
    title(ui, "Switching output");
    ui.horizontal(|ui| { ui.spinner(); ui.label(tr(ui, label)); });
    Choice::Wait
}
