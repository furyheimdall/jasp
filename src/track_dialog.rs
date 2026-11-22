use crate::{dialog, model::TrackDetails, settings::tr, theme, ui::{MUTED, WARNING, button}};
use eframe::egui::{self, Align, RichText, Ui};

pub(crate) struct TrackDialog {
    animation: dialog::Animation,
    player: &'static str,
    title: String,
    artist: String,
    album: String,
    duration: Option<String>,
    details: Vec<(&'static str, String)>,
    lookup: Lookup,
}

enum Lookup { Ready, Pending(String), Unavailable }

impl TrackDialog {
    pub(crate) fn new(player: &'static str, title: &str, artist: &str, album: &str, seconds: f64,
        details: TrackDetails) -> Self {
        Self {
            animation: dialog::Animation::default(), player,
            title: title.to_owned(), artist: artist.to_owned(), album: album.to_owned(),
            duration: (seconds.is_finite() && seconds > 0.0).then(|| duration_label(seconds as u64)),
            details: metadata_rows(details, artist), lookup: Lookup::Ready,
        }
    }

    pub(crate) fn pending(mut self, id: String) -> Self {
        self.lookup = Lookup::Pending(id);
        self
    }

    pub(crate) fn receive(&mut self, id: &str, result: Result<TrackDetails, String>) {
        if !matches!(&self.lookup, Lookup::Pending(expected) if expected == id) { return; }
        self.lookup = match result {
            Ok(details) => {
                let mut rows = metadata_rows(details, &self.artist);
                for (label, value) in std::mem::take(&mut self.details) {
                    if !rows.iter().any(|(key, _)| *key == label) { rows.push((label, value)); }
                }
                self.details = rows;
                Lookup::Ready
            }
            Err(_) => Lookup::Unavailable,
        };
    }

    pub(crate) fn invalidate_jastreamer(&mut self) {
        if matches!(self.lookup, Lookup::Pending(_)) { self.lookup = Lookup::Unavailable; }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> bool {
        let modal = dialog::show(ctx, egui::Id::new("track-details"), &mut self.animation, |ui| {
            let bounds = ctx.content_rect();
            let width = (bounds.width() - 64.0).clamp(208.0, 510.0);
            ui.set_width(width);
            ui.label(RichText::new(tr(ui, "Track details")).size(13.0).color(theme::accent(ui)).strong());
            // Keep the name, basic tags and long library locations inside the same scrollable body.
            egui::ScrollArea::vertical().max_height((bounds.height() - 138.0).clamp(96.0, 435.0)).show(ui, |ui| {
                ui.set_width(width);
                let title = if self.title.is_empty() { tr(ui, "Untitled track") } else { self.title.as_str().into() };
                ui.add(egui::Label::new(RichText::new(title).size(27.0).strong()).wrap());
                for text in [&self.artist, &self.album] {
                    if !text.is_empty() { ui.add(egui::Label::new(RichText::new(text).size(16.0).color(MUTED)).wrap()); }
                }
                ui.add_space(8.0);
                ui.separator();
                ui.spacing_mut().item_spacing.y = 5.0;
                let label_width = (width * 0.33).min(125.0);
                row(ui, label_width, "Player", self.player);
                if let Some(duration) = self.duration.as_deref() { row(ui, label_width, "Duration", duration); }
                for (label, value) in &self.details { row(ui, label_width, label, value); }
                match self.lookup {
                    Lookup::Pending(_) => { ui.horizontal(|ui| { ui.spinner(); ui.label(tr(ui, "Loading track details…")); }); }
                    Lookup::Unavailable => { ui.label(RichText::new(tr(ui, "Detailed tags could not be loaded.")).color(WARNING)); }
                    Lookup::Ready => {}
                }
            });
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| button(ui, "Close", true).clicked()).inner
        });
        if self.animation.is_open() && (modal.inner || modal.should_close()) {
            self.animation.start_close();
            ctx.request_repaint();
        }
        self.animation.is_finished()
    }
}

fn metadata_rows(d: TrackDetails, artist: &str) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    let mut add = |label, value: String| { if !value.is_empty() { rows.push((label, value)); } };
    if d.album_artist != artist { add("Album artist", d.album_artist); }
    add("Release date", d.date);
    if !d.genres.is_empty() { add("Genre", d.genres.join(", ")); }
    if let Some(disc) = d.disc { add("Disc number", disc.to_string()); }
    if let Some(number) = d.number { add("Track number", number.to_string()); }
    add("Format", d.codec);
    if let Some(hz) = d.sample_rate { add("Sample rate", format!("{} kHz", f64::from(hz) / 1000.0)); }
    if let Some(bits) = d.bits_per_sample { add("Bit depth", format!("{bits}-bit")); }
    if let Some(channels) = d.channels { add("Channels", channels.to_string()); }
    if let Some(bit_rate) = d.bit_rate { add("Bitrate", format!("{:.0} kb/s", bit_rate as f64 / 1000.0)); }
    // Do not display stream URLs, which may include embedded credentials.
    if !d.path.contains("://") { add("Library path", d.path); }
    rows
}

fn row(ui: &mut Ui, label_width: f32, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.add_sized([label_width, 17.0], egui::Label::new(RichText::new(tr(ui, label)).size(14.0).color(MUTED)).truncate());
        ui.add(egui::Label::new(RichText::new(value).size(15.0)).wrap());
    });
}

fn duration_label(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}
