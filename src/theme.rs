use eframe::egui::{self, Color32, Stroke, Ui};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Theme {
    Green,
    Orange,
    Blue,
    Mono,
    Violet,
}

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) background: Color32,
    pub(crate) rail: Color32,
    pub(crate) footer: Color32,
    pub(crate) accent: Color32,
    companion: Color32,
    pub(crate) card: Color32,
    raised: Color32,
    line: Color32,
}

impl Theme {
    pub(crate) const ALL: [Self; 5] = [Self::Green, Self::Orange, Self::Blue, Self::Mono, Self::Violet];

    pub(crate) fn key(self) -> &'static str {
        match self { Self::Green => "green", Self::Orange => "orange", Self::Blue => "blue",
            Self::Mono => "mono", Self::Violet => "violet" }
    }

    pub(crate) fn label(self) -> &'static str {
        match self { Self::Green => "Green", Self::Orange => "Orange", Self::Blue => "Blue",
            Self::Mono => "Monochrome", Self::Violet => "Violet" }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.key() == key)
    }

    pub(crate) fn palette(self) -> Palette {
        let [bg, rail, footer, accent, companion] = match self {
            Self::Green  => [[19, 32, 33], [13, 25, 27], [24, 32, 33], [124, 224, 194], [149, 191, 240]],
            Self::Orange => [[32, 25, 22], [24, 18, 16], [33, 26, 23], [255, 173, 116], [246, 205, 160]],
            Self::Blue   => [[20, 29, 44], [13, 21, 34], [22, 31, 47], [124, 195, 255], [165, 186, 249]],
            Self::Mono   => [[24, 24, 24], [15, 15, 15], [28, 28, 28], [229, 229, 229], [190, 190, 190]],
            Self::Violet => [[30, 24, 41], [22, 17, 32], [33, 26, 44], [199, 166, 251], [230, 173, 216]],
        }.map(|[r, g, b]| Color32::from_rgb(r, g, b));
        Palette { background: bg, rail, footer, accent, companion, card: bg.lerp_to_gamma(accent, 0.054),
            raised: bg.lerp_to_gamma(accent, 0.12), line: bg.lerp_to_gamma(accent, 0.22) }
    }

    pub(crate) fn install(self, ctx: &egui::Context) {
        let id = egui::Id::new("native-theme");
        if ctx.data(|data| data.get_temp::<Self>(id)) == Some(self) { return; }
        ctx.data_mut(|data| data.insert_temp(id, self));
        let p = self.palette();
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = p.background;
        visuals.window_fill = p.card;
        visuals.faint_bg_color = p.card;
        visuals.extreme_bg_color = p.rail;
        visuals.window_stroke = Stroke::new(1.0, p.line);
        visuals.hyperlink_color = p.companion;
        visuals.selection.bg_fill = p.accent;
        visuals.selection.stroke = Stroke::new(1.0, ON_ACCENT);
        visuals.widgets.noninteractive.bg_fill = p.card;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
        for (style, fill, stroke) in [
            (&mut visuals.widgets.inactive, p.raised, p.line),
            (&mut visuals.widgets.hovered, p.raised.lerp_to_gamma(p.accent, 0.10), p.accent),
            (&mut visuals.widgets.active, p.raised.lerp_to_gamma(p.accent, 0.20), p.accent),
            (&mut visuals.widgets.open, p.raised, p.accent),
        ] {
            style.bg_fill = fill;
            style.weak_bg_fill = fill;
            style.bg_stroke = Stroke::new(1.0, stroke);
        }
        ctx.set_visuals(visuals);
    }
}

pub(crate) const ON_ACCENT: Color32 = Color32::from_gray(22);
pub(crate) fn accent(ui: &Ui) -> Color32 { ui.visuals().selection.bg_fill }
pub(crate) fn companion(ui: &Ui) -> Color32 { ui.visuals().hyperlink_color }
pub(crate) fn card(ui: &Ui) -> Color32 { ui.visuals().window_fill }
pub(crate) fn raised(ui: &Ui) -> Color32 { ui.visuals().widgets.inactive.bg_fill }
pub(crate) fn line(ui: &Ui) -> Color32 { ui.visuals().window_stroke.color }
