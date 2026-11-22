use crate::{
    ja::{LibraryAdmin, LibraryOp, LibraryUpdate, Root, ScanJob},
    library_activity::{Activity, Kind},
    settings::tr,
    ui::{MUTED, WARNING, button},
};
use eframe::egui::{Response, RichText, TextEdit, Ui};
use std::{fmt::Write as _, fs::File, io::{self, Read as _}, time::{Duration, Instant}};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending { Load, Start, Poll, Cancel, Save }

const SCAN_POLL: Duration = Duration::from_millis(750);

struct ScanWatch {
    id: String,
    poll_at: Instant,
}

#[derive(Default)]
pub(crate) struct AdminView {
    server: Option<LibraryAdmin>,
    roots: Vec<Root>,
    draft_revision: String,
    dirty: bool,
    tried_load: bool,
    confirm_removal: bool,
    pending: Option<Pending>,
    scan: Option<ScanWatch>,
    notice: Option<&'static str>,
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
                if !self.dirty || pending == Some(Pending::Save) { self.reset_draft(); }
                self.error = None;
                self.notice = (pending == Some(Pending::Save)).then_some("Library sources saved. No scan was started.");
                pending == Some(Pending::Save)
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
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled_ui(self.pending.is_none(), |ui| localized_button(ui, "Refresh", false)).inner.clicked() {
                self.request(LibraryOp::Load, Pending::Load, &mut send);
            }
            if self.pending.is_some_and(|pending| pending != Pending::Poll) { ui.spinner(); }
        });
        if let Some(notice) = self.notice { ui.label(tr(ui, notice)); }
        if let Some(error) = &self.error { ui.label(RichText::new(tr(ui, error)).color(WARNING)); }
        let Some(server) = &self.server else { return; };
        let active = self.scan.is_some();
        let stale = self.draft_revision != server.revision;
        let idle = self.pending.is_none_or(|pending| pending == Pending::Poll);
        let editable = can_edit && idle && !stale;

        ui.add_space(8.0);
        ui.label(RichText::new(tr(ui, "Library sources")).strong());
        ui.label(RichText::new(tr(ui, "Paths refer to directories on the jastreamer server.")).color(MUTED));
        if self.roots.is_empty() { ui.label(tr(ui, "No library sources configured.")); }
        if stale { ui.label(RichText::new(tr(ui, "Server configuration changed. Discard edits to load the current sources.")).color(WARNING)); }
        let mut remove = None;
        let mut changed = false;
        ui.add_enabled_ui(editable, |ui| {
            for (index, root) in self.roots.iter_mut().enumerate() {
                ui.push_id(&root.id, |ui| {
                    ui.group(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&root.id).monospace().color(MUTED));
                            if localized_button(ui, "Remove source", false).clicked() { remove = Some(index); }
                        });
                        ui.label(tr(ui, "Name"));
                        changed |= ui.add(TextEdit::singleline(&mut root.name).desired_width(f32::INFINITY)).changed();
                        ui.label(tr(ui, "Server path"));
                        changed |= ui.add(TextEdit::singleline(&mut root.path).desired_width(f32::INFINITY)).changed();
                    });
                });
            }
        });
        if let Some(index) = remove {
            self.roots.remove(index);
            changed = true;
        }
        let add = ui.add_enabled_ui(editable && self.roots.len() < 64, |ui| localized_button(ui, "Add source", false)).inner.clicked();
        if add {
            match new_root(&self.roots, &server.roots) {
                Ok(root) => { self.roots.push(root); changed = true; }
                Err(_) => self.error = Some("Could not create a library source ID.".into()),
            }
        }
        if changed {
            self.dirty = self.roots != server.roots;
            self.confirm_removal = false;
            self.notice = None;
        }
        let mut discard = false;
        let mut save = false;
        if self.dirty {
            let fields_complete = self.roots.iter().all(|root| !root.name.trim().is_empty() && !root.path.trim().is_empty());
            ui.horizontal_wrapped(|ui| {
                save = ui.add_enabled_ui(editable && !active && fields_complete, |ui| localized_button(ui, "Save sources", true)).inner.clicked();
                discard = ui.add_enabled_ui(self.pending.is_none(), |ui| localized_button(ui, "Discard edits", false)).inner.clicked();
            });
            if active { ui.label(tr(ui, "Wait for the active scan before saving sources.")); }
            if !fields_complete { ui.label(tr(ui, "Every source needs a name and server path.")); }
        }
        if discard {
            self.reset_draft();
            self.error = None;
        }
        if save {
            if self.server.as_ref().is_some_and(|server| server.roots.iter().any(|saved| !self.roots.iter().any(|root| root.id == saved.id))) {
                self.confirm_removal = true;
            } else {
                self.save(&mut send);
            }
        }
        if self.confirm_removal {
            ui.label(RichText::new(tr(ui, "Saving removes the selected sources from the library. It does not delete media files.")).color(WARNING));
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled_ui(editable && !active && self.pending.is_none(), |ui| localized_button(ui, "Confirm removal and save", true)).inner.clicked() {
                    self.save(&mut send);
                }
                if localized_button(ui, "Back", false).clicked() { self.confirm_removal = false; }
            });
        }

        ui.add_space(12.0);
        ui.label(RichText::new(tr(ui, "Library scan")).strong());
        let scan_allowed = can_edit && self.pending.is_none() && !active && !self.dirty
            && !self.roots.is_empty() && !activity.jastreamer_failed();
        let mut request = None;
        ui.add_enabled_ui(scan_allowed, |ui| {
            ui.horizontal_wrapped(|ui| {
                if localized_button(ui, "Incremental scan", true).clicked() { request = Some((LibraryOp::Incremental, Pending::Start)); }
                if localized_button(ui, "Full scan", false).clicked() { request = Some((LibraryOp::Full, Pending::Start)); }
            });
        });
        if self.dirty { ui.label(tr(ui, "Save or discard source edits before starting a scan.")); }
        if let Some(scan) = &self.scan {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr(ui, "Scanning the music library…"));
                if ui.add_enabled_ui(can_edit && self.pending.is_none(), |ui| localized_button(ui, "Stop scan", false)).inner.clicked() {
                    request = Some((LibraryOp::Cancel(scan.id.clone()), Pending::Cancel));
                }
            });
        }
        if let Some((op, pending)) = request {
            if pending == Pending::Start {
                activity.begin(Kind::Jastreamer);
                if !self.request(op, pending, &mut send) {
                    activity.finish_jastreamer(Err(self.error.take().unwrap_or_default()));
                }
            } else { self.request(op, pending, &mut send); }
        }
    }

    fn reset_draft(&mut self) {
        if let Some(server) = &self.server {
            self.roots.clone_from(&server.roots);
            self.draft_revision.clone_from(&server.revision);
        }
        self.dirty = false;
        self.confirm_removal = false;
    }

    fn save(&mut self, send: &mut impl FnMut(LibraryOp) -> bool) {
        self.request(LibraryOp::SaveRoots { revision: self.draft_revision.clone(), roots: self.roots.clone() }, Pending::Save, send);
        if self.pending.is_some() { self.confirm_removal = false; }
    }

    fn request(&mut self, op: LibraryOp, pending: Pending, send: &mut impl FnMut(LibraryOp) -> bool) -> bool {
        self.notice = None;
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

fn localized_button(ui: &mut Ui, key: &'static str, selected: bool) -> Response {
    let label = tr(ui, key);
    button(ui, &label, selected)
}

fn new_root(draft: &[Root], saved: &[Root]) -> io::Result<Root> {
    let mut random = File::open("/dev/urandom")?;
    loop {
        let mut bytes = [0u8; 16];
        random.read_exact(&mut bytes)?;
        let mut id = String::with_capacity(37);
        id.push_str("root-");
        for byte in bytes { let _ = write!(id, "{byte:02x}"); }
        if !draft.iter().chain(saved).any(|root| root.id == id) {
            return Ok(Root { id, name: String::new(), path: String::new() });
        }
    }
}
