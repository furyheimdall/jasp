use crate::{
    ja::{LibraryAdmin, LibraryOp, LibraryUpdate, ScanJob},
    library_activity::{Activity, Kind},
    settings::{tr, widgets},
    ui::{MUTED, WARNING},
};
use eframe::egui::{RichText, Ui};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending { Load, Start, Poll, Cancel }

const SCAN_POLL: Duration = Duration::from_millis(750);

struct ScanWatch {
    id: String,
    poll_at: Instant,
}

#[derive(Default)]
pub(crate) struct AdminView {
    server: Option<LibraryAdmin>,
    tried_load: bool,
    pending: Option<Pending>,
    scan: Option<ScanWatch>,
    error: Option<String>,
}

impl AdminView {
    pub(crate) fn loaded(&mut self, result: Result<LibraryUpdate, String>, activity: &mut Activity) -> bool {
        let pending = self.pending.take();
        match result {
            Ok(LibraryUpdate::Settings(mut server)) => {
                if self.scan.is_none() && let Some(job) = server.active_scan.take() {
                    activity.begin(Kind::Jastreamer);
                    self.observe_scan(job, activity);
                }
                self.server = Some(server);
                self.error = None;
                false
            }
            Ok(LibraryUpdate::Scan(job)) => self.observe_scan(job, activity),
            Ok(LibraryUpdate::CancelRequested) => {
                if let Some(scan) = &mut self.scan { scan.poll_at = Instant::now(); }
                false
            }
            Err(error) => {
                if matches!(pending, Some(Pending::Start | Pending::Poll)) {
                    self.scan = None;
                    activity.finish_jastreamer(Err(error));
                } else { self.error = Some(error); }
                false
            }
        }
    }

    fn observe_scan(&mut self, job: ScanJob, activity: &mut Activity) -> bool {
        if job.is_active() {
            self.scan = Some(ScanWatch { id: job.id, poll_at: Instant::now() + SCAN_POLL });
            activity.jastreamer_progress(job.counts);
            return false;
        }
        self.scan = None;
        let result = match job.status.as_str() {
            "complete" => Ok(Some(job.counts)),
            "cancelled" => Ok(None),
            "failed" => Err(if job.error.is_empty() { "Music scan failed".into() } else { job.error }),
            _ => Err("Server did not return a recognized scan status.".into()),
        };
        activity.finish_jastreamer(result);
        true
    }

    pub(crate) fn poll(&mut self, ctx: &eframe::egui::Context, mut send: impl FnMut(LibraryOp) -> bool) {
        if self.pending.is_some() { return; }
        let Some(scan) = &mut self.scan else { return; };
        let remaining = scan.poll_at.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            ctx.request_repaint_after(remaining);
        } else if send(LibraryOp::Status(scan.id.clone())) {
            self.pending = Some(Pending::Poll);
        } else {
            scan.poll_at = Instant::now() + SCAN_POLL;
            ctx.request_repaint_after(SCAN_POLL);
        }
    }

    pub(crate) fn show(&mut self, ui: &mut Ui, can_edit: bool, activity: &mut Activity, mut send: impl FnMut(LibraryOp) -> bool) {
        if !self.tried_load {
            self.tried_load = true;
            if self.server.is_none() && self.pending.is_none() {
                self.request(LibraryOp::Load, Pending::Load, &mut send);
            }
        }
        widgets::section(ui, "Library sources", |ui| {
            widgets::hint(ui, "Manage sources in the jastreamer web UI.");
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled_ui(self.pending.is_none(), |ui| widgets::button(ui, "Refresh", false)).inner.clicked() {
                    self.request(LibraryOp::Load, Pending::Load, &mut send);
                }
                if self.pending.is_some_and(|pending| pending != Pending::Poll) { ui.spinner(); }
            });
            if let Some(error) = &self.error { ui.label(RichText::new(tr(ui, error)).color(WARNING)); }
            let Some(server) = &self.server else { return; };
            if server.roots.is_empty() { widgets::hint(ui, "No library sources configured."); }
            for root in &server.roots {
                ui.separator();
                ui.label(RichText::new(&root.name).size(16.0).strong());
                ui.label(RichText::new(&root.path).color(MUTED));
            }
        });

        let Some(server) = &self.server else { return; };
        let request = widgets::section(ui, "Library scan", |ui| {
            let active = self.scan.is_some();
            let scan_allowed = can_edit && self.pending.is_none() && !active
                && !server.roots.is_empty() && !activity.jastreamer_failed();
            let mut request = None;
            ui.add_enabled_ui(scan_allowed, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if widgets::button(ui, "Incremental scan", true).clicked() { request = Some((LibraryOp::Incremental, Pending::Start)); }
                    if widgets::button(ui, "Full scan", false).clicked() { request = Some((LibraryOp::Full, Pending::Start)); }
                });
            });
            if let Some(scan) = &self.scan {
                ui.horizontal_wrapped(|ui| {
                    widgets::hint(ui, "Scanning the music library…");
                    if ui.add_enabled_ui(can_edit && self.pending.is_none(), |ui| widgets::button(ui, "Stop scan", false)).inner.clicked() {
                        request = Some((LibraryOp::Cancel(scan.id.clone()), Pending::Cancel));
                    }
                });
            }
            request
        });
        if let Some((op, pending)) = request {
            if pending == Pending::Start {
                activity.begin(Kind::Jastreamer);
                if !self.request(op, pending, &mut send) {
                    activity.finish_jastreamer(Err(self.error.take().unwrap_or_default()));
                }
            } else { self.request(op, pending, &mut send); }
        }
    }

    fn request(&mut self, op: LibraryOp, pending: Pending, send: &mut impl FnMut(LibraryOp) -> bool) -> bool {
        if send(op) {
            self.pending = Some(pending);
            self.error = None;
            true
        } else {
            self.error = Some("Library request could not be queued. Try again.".into());
            false
        }
    }
}
