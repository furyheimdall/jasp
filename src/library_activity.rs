use crate::settings::tr;
use crate::ja::ScanCounts;
use eframe::egui::{self, Id, RichText, Ui, pos2};
use std::time::{Duration, Instant};

const LINGER: Duration = Duration::from_secs(7);
const ENTER: Duration = Duration::from_millis(340);
const EXIT: Duration = Duration::from_millis(210);

#[derive(Clone, Copy)]
pub(super) enum Kind { Sources, Scan, Jastreamer }

#[derive(Default)]
pub(super) struct Activity {
    sources: Option<Toast>,
    scan: Option<Toast>,
    jastreamer: Option<Toast>,
}

struct Toast {
    status: Status,
    opened: Instant,
    expires: Option<Instant>,
    hidden: Option<Instant>,
}

enum Status {
    Working(Option<ScanCounts>),
    Complete(Completion),
    Failed(String),
}

enum Completion {
    Total(Option<usize>),
    Changes(ScanCounts),
    Cancelled,
}

impl Activity {
    fn slot(&mut self, kind: Kind) -> &mut Option<Toast> {
        match kind { Kind::Sources => &mut self.sources, Kind::Scan => &mut self.scan, Kind::Jastreamer => &mut self.jastreamer }
    }

    pub(super) fn pending(&self) -> (bool, bool, bool) {
        let running = |toast: &Option<Toast>| toast.as_ref().is_some_and(|t| matches!(t.status, Status::Working(_)));
        let unacknowledged = Self::unacknowledged;
        (running(&self.sources) || unacknowledged(&self.sources), running(&self.scan), unacknowledged(&self.scan))
    }

    fn unacknowledged(toast: &Option<Toast>) -> bool {
        toast.as_ref().is_some_and(|t| t.hidden.is_none() && matches!(t.status, Status::Failed(_)))
    }

    pub(super) fn jastreamer_failed(&self) -> bool { Self::unacknowledged(&self.jastreamer) }

    pub(super) fn clear_jastreamer(&mut self) { self.jastreamer = None; }

    pub(super) fn jastreamer_progress(&mut self, counts: ScanCounts) {
        if let Some(toast) = &mut self.jastreamer { toast.status = Status::Working(Some(counts)); }
    }

    pub(super) fn finish_jastreamer(&mut self, result: Result<Option<ScanCounts>, String>) {
        self.complete(Kind::Jastreamer, result.map(|counts| counts.map_or(Completion::Cancelled, Completion::Changes)));
    }

    pub(super) fn begin(&mut self, kind: Kind) {
        *self.slot(kind) = Some(Toast { status: Status::Working(None), opened: Instant::now(), expires: None, hidden: None });
    }

    pub(super) fn finish(&mut self, kind: Kind, result: Result<Option<usize>, String>) {
        self.complete(kind, result.map(Completion::Total));
    }

    fn complete(&mut self, kind: Kind, result: Result<Completion, String>) {
        let slot = self.slot(kind);
        if let Some(toast) = slot {
            match result {
                Ok(_) if toast.hidden.is_some() => *slot = None,
                Ok(completion) => {
                    toast.expires = Some(Instant::now() + LINGER);
                    toast.status = Status::Complete(completion);
                }
                Err(error) => {
                    // A hidden progress card must not hide a failure.
                    if toast.hidden.take().is_some() { toast.opened = Instant::now(); }
                    toast.expires = None;
                    toast.status = Status::Failed(error);
                }
            }
        }
    }

    pub(super) fn show(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let viewport = ctx.content_rect();
        let width = 320.0_f32.min(viewport.width() - 30.0).max(1.0);
        let mut top = viewport.top() + 14.0;
        let mut slots = [
            ("music-source-notification", Kind::Sources, &mut self.sources),
            ("music-scan-notification", Kind::Scan, &mut self.scan),
            ("jastreamer-scan-notification", Kind::Jastreamer, &mut self.jastreamer),
        ];
        slots.sort_unstable_by_key(|(_, _, slot)| std::cmp::Reverse(slot.as_ref().map(|toast| toast.opened)));
        let mut front = None;
        let mut depth = 0.0;
        for (name, kind, slot) in slots {
            let Some(toast) = slot else { continue; };
            let deadline = toast.hidden.map(|started| started + EXIT).or(toast.expires);
            if deadline.is_some_and(|deadline| now >= deadline) {
                if !matches!(toast.status, Status::Working(_)) { *slot = None; }
                continue;
            }
            let entered = (now.duration_since(toast.opened).as_secs_f32() / ENTER.as_secs_f32()).min(1.0);
            if entered < 1.0 { ctx.request_repaint(); }
            let mut exit = 0.0;
            if let Some(deadline) = deadline {
                let left = deadline.saturating_duration_since(now);
                if left <= EXIT {
                    exit = 1.0 - left.as_secs_f32() / EXIT.as_secs_f32();
                    ctx.request_repaint();
                } else { ctx.request_repaint_after(left - EXIT); }
            }
            let offset = (width + 24.0) * (1.0 - entered).powi(3).max(exit.powi(2));
            let id = Id::new(name);
            front.get_or_insert(egui::LayerId::new(egui::Order::Foreground, id));
            let response = egui::Area::new(id).order(egui::Order::Foreground)
                .current_pos(pos2(viewport.right() - width - 14.0 - 12.0 * depth + offset, top))
                .constrain(false).movable(false).show(ctx, |ui| {
                    ui.set_width(width);
                    card(ui, kind, &toast.status)
                });
            // Only the padded corners overlap; both controls stay readable.
            top += (response.response.rect.height() - 11.0).max(12.0);
            depth += 1.0;
            if response.inner && toast.hidden.is_none() {
                toast.hidden = Some(now);
                ctx.request_repaint();
            }
        }
        if let Some(front) = front { ctx.move_to_top(front); }
    }
}

fn card(ui: &mut Ui, kind: Kind, status: &Status) -> bool {
    use crate::{theme, ui::{MUTED, WARNING}};
    let (heading, tint) = match (kind, status) {
        (Kind::Sources, Status::Working(_)) => ("Refreshing music sources…", theme::accent(ui)),
        (Kind::Sources, Status::Complete(_)) => ("Music sources refreshed", theme::accent(ui)),
        (Kind::Sources, Status::Failed(_)) => ("Could not refresh music sources", WARNING),
        (Kind::Scan, Status::Working(_)) => ("Scanning the music library…", theme::accent(ui)),
        (Kind::Scan, Status::Complete(_)) => ("Music scan complete", theme::accent(ui)),
        (Kind::Scan, Status::Failed(_)) => ("Music scan failed", WARNING),
        (Kind::Jastreamer, Status::Working(_)) => ("Scanning jastreamer library…", theme::accent(ui)),
        (Kind::Jastreamer, Status::Complete(Completion::Cancelled)) => ("jastreamer scan cancelled", MUTED),
        (Kind::Jastreamer, Status::Complete(_)) => ("jastreamer scan complete", theme::accent(ui)),
        (Kind::Jastreamer, Status::Failed(_)) => ("jastreamer scan problem", WARNING),
    };
    egui::Frame::new().fill(theme::card(ui)).corner_radius(10).inner_margin(12)
        .stroke(egui::Stroke::new(1.0, tint.gamma_multiply(0.45))).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                if matches!(status, Status::Working(_)) { ui.spinner(); }
                ui.label(RichText::new(tr(ui, heading)).size(17.0).strong().color(tint));
            });
            match status {
                Status::Complete(Completion::Total(Some(total))) => {
                    let label = match (kind, total) {
                        (Kind::Sources, 1) => "source found",
                        (Kind::Sources, _) => "sources found",
                        (Kind::Scan, 1) => "track in the index",
                        (Kind::Scan, _) => "tracks in the index",
                        (Kind::Jastreamer, _) => "tracks in the index",
                    };
                    ui.label(RichText::new(format!("{total} {}", tr(ui, label))).color(MUTED));
                }
                Status::Working(Some(counts)) => {
                    ui.label(RichText::new(format!("{}: {} / {}", tr(ui, "Processed"), counts.processed, counts.discovered)).color(MUTED));
                }
                Status::Complete(Completion::Changes(counts)) => {
                    ui.horizontal_wrapped(|ui| {
                        for (label, total) in [("New files", counts.added), ("Updated", counts.updated), ("Deleted / missing", counts.unavailable)] {
                            ui.label(RichText::new(format!("{}: {total}", tr(ui, label))).color(MUTED));
                        }
                        if counts.errors != 0 {
                            ui.label(RichText::new(format!("{}: {}", tr(ui, "Errors"), counts.errors)).color(WARNING));
                        }
                    });
                }
                Status::Failed(error) => { ui.label(RichText::new(error).color(WARNING)); }
                _ => {}
            }
            let dismiss = if matches!(status, Status::Failed(_)) { "Acknowledge" } else { "Hide" };
            crate::ui::button(ui, dismiss, false).clicked()
        }).inner
}
