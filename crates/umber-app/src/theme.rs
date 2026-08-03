//! Design tokens.
//!
//! Values come from the "Graphite" exploration in the Umber design project
//! (screens 1b and 1e). The accents there are specified in OKLCH — the dark
//! accent is `oklch(0.68 0.09 60)` — and are converted to sRGB here, since egui
//! works in sRGB bytes.
//!
//! Everything the UI draws pulls from [`Palette`] rather than hard-coding a
//! colour, so a second theme is a table of values rather than an edit sweep.

use egui::Color32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeKind {
    /// Near-black flat workbench. The design's default.
    Graphite,
    /// Warm paper neutrals.
    Paper,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 2] = [Self::Graphite, Self::Paper];

    pub fn label(self) -> &'static str {
        match self {
            Self::Graphite => "Graphite",
            Self::Paper => "Paper",
        }
    }
}

/// The four accents the design offers.
///
/// Only the hue changes; every other token in [`Palette`] is shared, which is
/// what keeps this a preference rather than four more themes to maintain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Accent {
    /// The design's default, and the colour the application is named for.
    #[default]
    Umber,
    Sage,
    Steel,
    Clay,
}

impl Accent {
    pub const ALL: [Accent; 4] = [Self::Umber, Self::Sage, Self::Steel, Self::Clay];

    pub fn label(self) -> &'static str {
        match self {
            Self::Umber => "Umber",
            Self::Sage => "Sage",
            Self::Steel => "Steel",
            Self::Clay => "Clay",
        }
    }

    /// The design's swatch, as shown in the settings page.
    ///
    /// These are the dark-theme values. On Paper the accent is darkened for
    /// contrast against a light surface — see [`Accent::ink`].
    pub fn swatch(self) -> Color32 {
        match self {
            Self::Umber => Color32::from_rgb(0xC0, 0x8A, 0x4E),
            Self::Sage => Color32::from_rgb(0x8F, 0xA3, 0x6B),
            Self::Steel => Color32::from_rgb(0x7E, 0x96, 0xBA),
            Self::Clay => Color32::from_rgb(0xB8, 0x78, 0x78),
        }
    }

    /// The accent as it should read against a given theme's surface.
    ///
    /// Umber's two values are the design's own hand-picked pair and are used
    /// verbatim. The other three exist only as a single dark swatch, so the
    /// light variant is derived — darkened towards black far enough to clear
    /// text contrast on Paper, which is the same relationship Umber's two
    /// authored values already have (`#C08A4E` to `#9C622F`).
    pub fn ink(self, kind: ThemeKind) -> Color32 {
        match (self, kind) {
            (Self::Umber, ThemeKind::Graphite) => Color32::from_rgb(0xC0, 0x8A, 0x4E),
            (Self::Umber, ThemeKind::Paper) => Color32::from_rgb(0x9C, 0x62, 0x2F),
            (_, ThemeKind::Graphite) => self.swatch(),
            (_, ThemeKind::Paper) => mix(self.swatch(), Color32::BLACK, 0.30),
        }
    }
}

/// Linear mix of two sRGB bytes, `t` of the way from `a` to `b`.
///
/// Deliberately a plain byte lerp rather than a perceptual blend: it is only
/// used to derive muted variants of a colour that sits beside its own source,
/// where the cheap version is indistinguishable, and it stays `const`-friendly
/// arithmetic with no `powf` — which in this codebase has produced NaN before.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub accent: Color32,
    /// Muted accent — dashed outlines, subtle tints.
    pub accent_dim: Color32,
    /// Conflict and caution. Warm enough to read as "look at this" without the
    /// alarm of a true red, which the design reserves for nothing at all.
    pub warning: Color32,
    /// Filled background behind a warning badge.
    pub warning_bg: Color32,
    pub warning_border: Color32,
    /// Behind the document — the darkest surface.
    pub backdrop: Color32,
    /// Panel interiors and inset wells.
    pub window: Color32,
    /// The docked panel column.
    pub dock: Color32,
    /// Menu bar, rails, side panels.
    pub chrome: Color32,
    pub border: Color32,
    pub popover: Color32,
    pub popover_border: Color32,
    /// Resting button fill.
    pub control: Color32,
    pub control_hover: Color32,
    /// Selected tool / active pill.
    pub control_active: Color32,
    pub text_strong: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_dim: Color32,
    /// Slider track.
    pub rail: Color32,
    pub knob: Color32,
    /// One colour per link group, which is how a row says *which* set of
    /// layers it travels with.
    ///
    /// Tabulated per theme rather than derived from the accent: the accent is
    /// one hue and these have to be told apart from each other, and a set of
    /// six generated by rotating one hue lands two of them next to the accent
    /// itself — which already means "selected" on the row beside them.
    /// `LayerStack::LINK_GROUPS` is the length, and
    /// `link_colours_are_told_apart_from_each_other_and_from_every_accent`
    /// pins the length and the separation.
    pub link_colours: [Color32; umber_core::LayerStack::LINK_GROUPS],
}

impl Palette {
    pub const fn graphite() -> Self {
        Self {
            accent: Color32::from_rgb(0xC0, 0x8A, 0x4E),
            accent_dim: Color32::from_rgb(0x6B, 0x4E, 0x2E),
            warning: Color32::from_rgb(0xD0, 0x87, 0x70),
            warning_bg: Color32::from_rgb(0x2A, 0x1D, 0x18),
            warning_border: Color32::from_rgb(0x6E, 0x40, 0x34),
            backdrop: Color32::from_rgb(0x0D, 0x0E, 0x10),
            window: Color32::from_rgb(0x11, 0x12, 0x14),
            dock: Color32::from_rgb(0x14, 0x15, 0x17),
            chrome: Color32::from_rgb(0x17, 0x18, 0x1A),
            border: Color32::from_rgb(0x26, 0x28, 0x2B),
            popover: Color32::from_rgb(0x1B, 0x1C, 0x1F),
            popover_border: Color32::from_rgb(0x2C, 0x2E, 0x32),
            control: Color32::from_rgb(0x1F, 0x20, 0x23),
            control_hover: Color32::from_rgb(0x26, 0x28, 0x2B),
            control_active: Color32::from_rgb(0x2E, 0x2A, 0x25),
            text_strong: Color32::from_rgb(0xE6, 0xE7, 0xE9),
            text: Color32::from_rgb(0xC9, 0xCB, 0xCE),
            text_muted: Color32::from_rgb(0x9A, 0x9D, 0xA2),
            text_dim: Color32::from_rgb(0x84, 0x87, 0x8C),
            rail: Color32::from_rgb(0x26, 0x28, 0x2B),
            knob: Color32::from_rgb(0xE6, 0xE7, 0xE9),
            // Six hues spread round the wheel, at a lightness that reads
            // against this surface without competing with the layer name
            // beside it. Kept clear of all four *accents* as well as of each
            // other — the first set had a green a hair from `Accent::Sage` and
            // a gold a hair from `Accent::Umber`, which is only invisible if
            // you never test the accents nobody authored the palette in.
            // `link_colours_are_told_apart_from_each_other_and_from_every_
            // accent` is the measurement.
            link_colours: [
                Color32::from_rgb(0x3F, 0x7B, 0xE8), // blue
                Color32::from_rgb(0x46, 0xB0, 0x4A), // green
                Color32::from_rgb(0xA9, 0x6B, 0xE8), // violet
                Color32::from_rgb(0x1F, 0xB5, 0xB5), // teal
                Color32::from_rgb(0xEE, 0x5A, 0xA8), // rose
                Color32::from_rgb(0xF0, 0xD5, 0x3C), // yellow
            ],
        }
    }

    pub const fn paper() -> Self {
        Self {
            accent: Color32::from_rgb(0x9C, 0x62, 0x2F),
            accent_dim: Color32::from_rgb(0xC9, 0xB8, 0xA2),
            // The design specifies the conflict colours for Graphite only.
            // These are the light-surface analogues: the same hue, taken dark
            // enough to read as ink on paper and light enough to sit under it.
            warning: Color32::from_rgb(0x9E, 0x4E, 0x33),
            warning_bg: Color32::from_rgb(0xF7, 0xE9, 0xE2),
            warning_border: Color32::from_rgb(0xDF, 0xC1, 0xB0),
            backdrop: Color32::from_rgb(0xE4, 0xE0, 0xD9),
            window: Color32::from_rgb(0xEF, 0xEC, 0xE7),
            dock: Color32::from_rgb(0xF2, 0xEF, 0xEA),
            chrome: Color32::from_rgb(0xF7, 0xF5, 0xF1),
            border: Color32::from_rgb(0xDE, 0xDA, 0xD3),
            popover: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            popover_border: Color32::from_rgb(0xDE, 0xDA, 0xD3),
            control: Color32::from_rgb(0xEA, 0xE7, 0xE1),
            control_hover: Color32::from_rgb(0xE0, 0xDC, 0xD5),
            control_active: Color32::from_rgb(0xEA, 0xDF, 0xD2),
            text_strong: Color32::from_rgb(0x3A, 0x38, 0x36),
            text: Color32::from_rgb(0x3A, 0x38, 0x36),
            text_muted: Color32::from_rgb(0x6D, 0x6A, 0x66),
            text_dim: Color32::from_rgb(0x8D, 0x8A, 0x85),
            rail: Color32::from_rgb(0xDE, 0xDA, 0xD3),
            knob: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            // The same six hues taken dark enough to read as ink on paper, and
            // held to the same separation — the accents are darkened here too,
            // so a set that cleared them on Graphite need not clear them here.
            link_colours: [
                Color32::from_rgb(0x2A, 0x5A, 0xB4),
                Color32::from_rgb(0x2E, 0x7C, 0x33),
                Color32::from_rgb(0x77, 0x42, 0xAE),
                Color32::from_rgb(0x13, 0x7F, 0x7F),
                Color32::from_rgb(0xB0, 0x32, 0x6E),
                Color32::from_rgb(0x7E, 0x76, 0x0A),
            ],
        }
    }

    /// The colour that says which link group a row belongs to.
    ///
    /// Taken modulo the table rather than indexed with. Two things already stop
    /// an out-of-range number reaching here — `LayerStack::link` refuses to make
    /// a group this cannot colour, and the ORA reader drops a `umber-link-group`
    /// beyond `LINK_GROUPS` — so this is the third line and not the first. It is
    /// here because the alternative failure is an index panic on the *drawing
    /// path*, which is the worst place in the application to put one, and
    /// because the number's origin is a file rather than this process.
    pub fn link_colour(&self, group: u8) -> Color32 {
        self.link_colours[group as usize % self.link_colours.len()]
    }

    /// The theme in its authored accent.
    pub fn of(kind: ThemeKind) -> Self {
        Self::with_accent(kind, Accent::Umber)
    }

    /// The theme, re-hued to one of the design's four accents.
    ///
    /// Kept as a second constructor rather than a parameter on [`Palette::of`]
    /// so that the dozens of existing `of(kind)` call sites stay as they are —
    /// every one of them wants whatever the user chose, and threading an accent
    /// through them all would only move the lookup outwards.
    ///
    /// `accent_dim` is derived rather than tabulated for the alternates: it is
    /// the accent taken most of the way to the theme's own recessive surface,
    /// which is the relationship the two authored Umber values already have.
    /// Umber itself keeps its hand-picked pair, so the default is exactly the
    /// design's and only the three alternates are computed.
    pub fn with_accent(kind: ThemeKind, accent: Accent) -> Self {
        let mut palette = match kind {
            ThemeKind::Graphite => Self::graphite(),
            ThemeKind::Paper => Self::paper(),
        };
        if accent == Accent::Umber {
            return palette;
        }
        palette.accent = accent.ink(kind);
        palette.accent_dim = match kind {
            ThemeKind::Graphite => mix(palette.accent, palette.backdrop, 0.49),
            ThemeKind::Paper => mix(palette.accent, palette.window, 0.60),
        };
        palette
    }

    /// The document's surround, for the canvas shader.
    ///
    /// Display-space, not linear: the shader writes this straight to the
    /// surface without passing it through its gamma encode, so that the
    /// backdrop and the egui panels around it are the same colour.
    pub fn backdrop_display(&self) -> [f32; 3] {
        [
            self.backdrop.r() as f32 / 255.0,
            self.backdrop.g() as f32 / 255.0,
            self.backdrop.b() as f32 / 255.0,
        ]
    }

    pub fn is_dark(&self) -> bool {
        let sum = self.chrome.r() as u32 + self.chrome.g() as u32 + self.chrome.b() as u32;
        sum < 384
    }
}

/// Type scale. The design runs dense — 11–12 px — where egui defaults to 14.
pub mod text {
    pub const BODY: f32 = 12.0;
    pub const SMALL: f32 = 11.0;
    pub const TINY: f32 = 10.5;
    pub const CONTROL: f32 = 11.5;
    pub const HEADING: f32 = 13.0;
}

/// Fixed sizes taken straight from the design.
pub mod metrics {
    pub const MENU_BAR: f32 = 34.0;
    /// The document tab strip, between the menu bar and the tool options.
    pub const TAB_STRIP: f32 = 30.0;
    /// A tab within that strip. Shorter than the strip because the design sits
    /// the tabs on its bottom border rather than filling it.
    pub const TAB: f32 = 24.0;
    pub const OPTIONS_STRIP: f32 = 36.0;
    /// The layout-edit strip, which replaces nothing and is only there while
    /// the mode is on.
    pub const EDIT_BAR: f32 = 32.0;
    pub const STATUS_BAR: f32 = 26.0;
    /// Horizontal padding inside the chrome strips above — the design's
    /// `padding: 0 12px`.
    pub const STRIP_PAD: i8 = 12;
    /// The tool rail's width, now that the rail is a dockable module rather
    /// than a strip of chrome: the design's two-column button grid plus what a
    /// panel costs around it — [`PANEL_PAD`] either side of the body, and the
    /// [`SCROLL_BAR`] gutter. `the_tool_grid_fits_the_panel_it_sits_in` adds
    /// the three up.
    ///
    /// It is the narrowest a Tools column may be dragged, and *only* that:
    /// below it the design's two-column tool grid wraps to one and the rail
    /// stops reading as a rail. What such a column starts at is
    /// `dock::limits::SIDEBAR_MIN_WIDTH`, so that it has room to be dragged
    /// both ways — a column that opens on its own floor is a column half of
    /// whose splitter does nothing.
    pub const TOOL_RAIL: f32 = 100.0;
    pub const TOOL_BUTTON: f32 = 32.0;
    /// Gap between the rail's two columns.
    pub const TOOL_GAP: f32 = 2.0;
    pub const PANEL: f32 = 264.0;
    /// A docked panel's header: the design's 8 px padding around an 11 px line,
    /// and the strip the whole panel is dragged by.
    pub const PANEL_HEADER: f32 = 32.0;
    /// Horizontal padding inside a docked panel.
    pub const PANEL_PAD: i8 = 12;
    pub const RADIUS: f32 = 5.0;
    pub const RADIUS_LARGE: f32 = 6.0;
    /// The canvas scrollbars, along the bottom and right of the document
    /// region. Thin, because they sit over the picture rather than beside it.
    pub const SCROLLBAR: f32 = 11.0;
    /// egui's scroll bar — every panel body, the settings panes, the brush
    /// library — and the gap between it and the content.
    ///
    /// Deliberately *not* the canvas's [`SCROLLBAR`] above, which is the one
    /// place in Umber a bar legitimately floats: it lies over a picture that
    /// extends underneath it either way. Everywhere else a bar covering the
    /// content is a control hiding a reading, so [`super::apply`] makes these
    /// solid and they cost their own width.
    pub const SCROLL_BAR: f32 = 6.0;
    /// Between the content and the bar. The two together are what a solid
    /// vertical bar takes off the width a body has to lay out in, so anything
    /// sizing a column or a pane around its contents has to leave both over.
    pub const SCROLL_BAR_GAP: f32 = 4.0;
    /// Radius of the dot Umber draws where a pen is, in place of the arrow.
    ///
    /// Small on purpose: it says where the nib is and nothing else — how wide
    /// the mark will be is the brush preview's to answer, and a dot big enough
    /// to be mistaken for one would be a second answer to that question.
    pub const PEN_DOT: f32 = 2.5;
    pub const SLIDER_ROW: f32 = 16.0;
    pub const SLIDER_RAIL: f32 = 3.0;
    pub const SLIDER_KNOB: f32 = 11.0;
    /// A brush in the Brushes panel: sample and name on one line.
    pub const BRUSH_ROW: f32 = 26.0;
    /// A brush in the library browser, where a second line carries the
    /// attribution the shipped presets come with.
    pub const BRUSH_ROW_DETAIL: f32 = 40.0;
    /// One entry in the History module: a marker and a line of text, tighter
    /// than a brush row because it carries no picture.
    pub const HISTORY_ROW: f32 = 20.0;

    /// How far one level of layer nesting steps a row in.
    ///
    /// Small on purpose. The panel is [`PANEL`] wide and the row already spends
    /// most of it on a tick box, an eye, a thumbnail and a blend label, so a
    /// generous indent at four levels deep would leave a folder's contents with
    /// no name to read. It is also what `layerdrag` measures "into that folder"
    /// against, so there is one number rather than a paint-side and a
    /// model-side one that can disagree.
    pub const LAYER_INDENT: f32 = 12.0;
    /// The line above the layer list: the tick column's header at its left, and
    /// the ticked-layers strip right-aligned on the same line.
    ///
    /// A *fixed* height, and that is the whole reason it is a constant. The two
    /// halves are different sizes — the header box is
    /// `widgets`' `PICK_HIT` at 18 and the strip's chain is a 20 px
    /// `icon_toggle` — so a line sized by whatever is on it would be two
    /// pixels shorter with nothing ticked, and ticking the first layer would
    /// shunt the whole list down under the pointer that had just ticked it.
    /// That is the smaller version of the bug the shared line was made to fix,
    /// and the one that would have been left behind. The tallest thing on the
    /// line, so nothing is clipped.
    pub const LAYER_TICK_ROW: f32 = 20.0;
    /// The module library dialog. One card per module, each a picture beside
    /// two lines of text.
    pub const MODULE_LIBRARY_WIDTH: f32 = 470.0;
    /// The schematic of a module on one of those cards, in the proportions of
    /// the dock itself so it reads as the thing it stands for.
    pub const MODULE_PREVIEW: [f32; 2] = [78.0, 58.0];
    /// The brush library browser: a collection rail and a list of brushes.
    /// Smaller than the settings dialog because it shows one list rather than
    /// six panes.
    ///
    /// A *fixed* size, and that is the point of it being here rather than in
    /// `brushlib.rs`: the browser carries notices — what an import dropped,
    /// which is a sentence naming as many features as the file had — and a
    /// modal that grows to fit its own error message ends up wider than the
    /// screen with its corners out of reach.
    pub const BRUSH_LIBRARY: [f32; 2] = [780.0, 540.0];
    /// The collection rail down its left-hand side.
    pub const BRUSH_LIBRARY_RAIL: f32 = 210.0;
    /// The brush editor dialog. Wider than the other modals because the design
    /// lays its Tip section out as two columns and its Dynamics section as a
    /// row of curve panels, and neither survives being narrowed.
    pub const BRUSH_EDITOR_WIDTH: f32 = 560.0;
    /// One dynamics curve panel, square.
    pub const CURVE_PANEL: f32 = 150.0;
    /// A dropdown trigger — [`crate::widgets::dropdown`], which is every
    /// dropdown there is. One height wherever it is drawn: the Colour panel's
    /// header, the tool options strip, a panel body and the brush editor's
    /// two-column layout all put one somewhere, and four heights is how the
    /// four separate triggers this replaced came to look like four controls.
    pub const DROPDOWN: f32 = 18.0;
    /// The update dialog. Two columns on its offer screen — the versions on the
    /// left, the release notes on the right — so it is wider than About and
    /// narrower than the brush editor.
    ///
    /// Fixed, for the reason [`BRUSH_LIBRARY`] is: what it shows is a release's
    /// own notes, which is text nobody here wrote and nobody here can size. A
    /// modal that grew to fit them would be as wide as the longest line in
    /// somebody's changelog.
    pub const UPDATE_DIALOG_WIDTH: f32 = 560.0;
    /// The notes box on that screen. Tall enough for a release section, and
    /// scrolling rather than growing.
    pub const UPDATE_NOTES: [f32; 2] = [300.0, 170.0];
    /// A progress bar — the update dialog's, and the same weight the splash
    /// draws its own at.
    pub const PROGRESS_BAR: f32 = 4.0;
    /// The tallest a dropdown's menu grows before it scrolls.
    ///
    /// Some of the lists are long — thirteen dab inputs, ten blend modes, a
    /// user's collections — and a menu taller than the window has entries that
    /// cannot be reached at all.
    pub const DROPDOWN_MENU: f32 = 240.0;
}

/// Install Archivo, the typeface the design specifies.
///
/// The file is a *variable* font. `ab_glyph`, which egui rasterises with, does
/// not apply variation axes, so what we get is the default master — Regular.
/// That is enough here because egui's `strong()` changes colour rather than
/// weight, so no bold face is ever asked for. If a genuinely bold face is
/// needed later it has to be a second, separately instanced file.
///
/// Archivo is bundled under the SIL Open Font License; see `assets/fonts/`.
pub fn install_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "archivo".to_owned(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/Archivo[wdth,wght].ttf"
        ))),
    );

    // Insert ahead of egui's default face rather than replacing the list: the
    // fallbacks still cover anything Archivo lacks.
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "archivo".to_owned());

    ctx.set_fonts(fonts);
}

/// Push the palette into egui's own styling, so stock widgets (menus, scroll
/// bars, tooltips) match the hand-drawn ones instead of sitting in egui's
/// default blue-grey.
pub fn apply(ctx: &egui::Context, palette: &Palette) {
    // egui keeps separate light and dark styles and picks between them. Umber
    // drives its own themes, so both are written identically and the
    // preference is set to match — otherwise switching to Paper would leave
    // egui's internals still believing they are in dark mode.
    ctx.set_theme(if palette.is_dark() {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });
    ctx.all_styles_mut(|style| style_from(style, palette));
}

fn style_from(style: &mut egui::Style, palette: &Palette) {
    use egui::{FontFamily, FontId, TextStyle};

    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(text::HEADING, FontFamily::Proportional),
        ),
        (
            TextStyle::Body,
            FontId::new(text::BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(text::CONTROL, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(text::SMALL, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(text::TINY, FontFamily::Monospace),
        ),
    ]
    .into();

    // egui's default scroll bar *floats*: it is drawn over the content and
    // allocates nothing (`ScrollStyle::floating`, whose
    // `floating_allocated_width` is zero), so it sat on top of the settings
    // dialog's readings and over the right-hand edge of every panel body.
    // Solid instead — the bar takes its own gutter and the content is laid out
    // in what is left. Set here rather than on each `ScrollArea` because a
    // gutter chosen per call site is how two panes end up with different ones.
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: metrics::SCROLL_BAR,
        bar_inner_margin: metrics::SCROLL_BAR_GAP,
        bar_outer_margin: 0.0,
        ..egui::style::ScrollStyle::solid()
    };

    let v = &mut style.visuals;
    v.dark_mode = palette.is_dark();
    v.panel_fill = palette.chrome;
    v.window_fill = palette.popover;
    v.window_stroke = egui::Stroke::new(1.0, palette.popover_border);
    v.extreme_bg_color = palette.window;
    v.faint_bg_color = palette.control;
    v.hyperlink_color = palette.accent;
    v.selection.bg_fill = palette.accent.linear_multiply(0.35);
    v.selection.stroke = egui::Stroke::new(1.0, palette.text_strong);

    let r = egui::CornerRadius::same(metrics::RADIUS as u8);
    v.widgets.noninteractive.bg_fill = palette.chrome;
    v.widgets.noninteractive.weak_bg_fill = palette.chrome;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, palette.border);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, palette.text_muted);
    v.widgets.noninteractive.corner_radius = r;

    v.widgets.inactive.bg_fill = palette.control;
    v.widgets.inactive.weak_bg_fill = palette.control;
    v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, palette.text_muted);
    v.widgets.inactive.corner_radius = r;

    v.widgets.hovered.bg_fill = palette.control_hover;
    v.widgets.hovered.weak_bg_fill = palette.control_hover;
    v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, palette.text_strong);
    v.widgets.hovered.corner_radius = r;

    v.widgets.active.bg_fill = palette.control_hover;
    v.widgets.active.weak_bg_fill = palette.control_hover;
    v.widgets.active.bg_stroke = egui::Stroke::NONE;
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, palette.text_strong);
    v.widgets.active.corner_radius = r;

    v.widgets.open.bg_fill = palette.control_hover;
    v.widgets.open.weak_bg_fill = palette.control_hover;
    v.widgets.open.fg_stroke = egui::Stroke::new(1.0, palette.text_strong);
    v.widgets.open.corner_radius = r;

    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.spacing.button_padding = egui::vec2(9.0, 4.0);
    style.spacing.menu_margin = egui::Margin::same(6);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link group is told apart from its neighbours by the colour of a 12-px
    /// mark and by nothing else, so "no two are equal" is far too weak a test:
    /// two colours a dozen units apart are the same mark at that size. This
    /// measures the distance, in both themes and against **all four** accents.
    ///
    /// The two thresholds are deliberately different. Two link marks sit on
    /// adjacent rows and are the same shape, so they carry the strict bound; an
    /// accent appears as a row's fill and border rather than as a mark, so
    /// confusing the two is harder and the bound is looser. Checking only
    /// `Accent::Umber` — the one the palette was authored in — is what let the
    /// first set of these ship a green a hair from `Accent::Sage` and a gold a
    /// hair from Umber itself.
    #[test]
    fn link_colours_are_told_apart_from_each_other_and_from_every_accent() {
        // Sum of the per-channel differences. Crude next to a perceptual metric
        // and deliberately so: it is `mix`'s own arithmetic, it is honest about
        // what it measures, and the bounds were settled by looking at the marks
        // rather than derived from a model.
        let apart = |a: Color32, b: Color32| {
            let d = |x: u8, y: u8| u32::from(x.abs_diff(y));
            d(a.r(), b.r()) + d(a.g(), b.g()) + d(a.b(), b.b())
        };

        for kind in ThemeKind::ALL {
            let p = Palette::of(kind);
            assert_eq!(
                p.link_colours.len(),
                umber_core::LayerStack::LINK_GROUPS,
                "the model can make a group this cannot colour"
            );

            for (i, a) in p.link_colours.iter().enumerate() {
                for b in &p.link_colours[i + 1..] {
                    let d = apart(*a, *b);
                    assert!(d >= 100, "{kind:?}: {a:?} and {b:?} are only {d} apart");
                }
                for accent in Accent::ALL {
                    let ink = Palette::with_accent(kind, accent).accent;
                    let d = apart(*a, ink);
                    assert!(
                        d >= 60,
                        "{kind:?}/{accent:?}: {a:?} is only {d} from the accent"
                    );
                }
            }

            // A number out of a file may name a group this build has no colour
            // for; the drawing path must answer rather than panic.
            assert_eq!(p.link_colour(0), p.link_colour(u8::MAX / 6 * 6));
        }
    }

    /// The accent mechanism must not perturb the default. If re-hueing ever
    /// starts running for Umber, this catches it before the whole interface
    /// shifts colour by a couple of units.
    #[test]
    fn the_default_accent_is_the_designs_authored_pair() {
        for kind in ThemeKind::ALL {
            let plain = Palette::of(kind);
            let explicit = Palette::with_accent(kind, Accent::Umber);
            assert_eq!(plain.accent, explicit.accent);
            assert_eq!(plain.accent_dim, explicit.accent_dim);
        }
        assert_eq!(
            Palette::of(ThemeKind::Graphite).accent,
            Color32::from_rgb(0xC0, 0x8A, 0x4E),
        );
        assert_eq!(
            Palette::of(ThemeKind::Graphite).accent_dim,
            Color32::from_rgb(0x6B, 0x4E, 0x2E),
        );
    }

    #[test]
    fn every_accent_changes_the_hue_in_both_themes() {
        for kind in ThemeKind::ALL {
            let base = Palette::with_accent(kind, Accent::Umber).accent;
            for accent in Accent::ALL.into_iter().filter(|a| *a != Accent::Umber) {
                let p = Palette::with_accent(kind, accent);
                assert_ne!(p.accent, base, "{accent:?} on {kind:?} did not re-hue");
                assert_ne!(p.accent, p.accent_dim, "{accent:?} on {kind:?} has no dim");
            }
        }
    }

    /// A derived `accent_dim` has to stay on the recessive side of its accent,
    /// or the "muted" tint would come out louder than the thing it mutes.
    #[test]
    fn a_derived_dim_recedes_towards_its_own_surface() {
        let luma = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        for accent in Accent::ALL {
            let dark = Palette::with_accent(ThemeKind::Graphite, accent);
            assert!(
                luma(dark.accent_dim) < luma(dark.accent),
                "{accent:?} dim is not darker on Graphite",
            );
            let light = Palette::with_accent(ThemeKind::Paper, accent);
            assert!(
                luma(light.accent_dim) > luma(light.accent),
                "{accent:?} dim is not lighter on Paper",
            );
        }
    }

    /// Warning ink must contrast with the surface it is drawn on, in both
    /// themes — the whole point of the token is to be noticed.
    #[test]
    fn warning_ink_contrasts_with_its_own_fill() {
        let luma = |c: Color32| c.r() as i32 + c.g() as i32 + c.b() as i32;
        for kind in ThemeKind::ALL {
            let p = Palette::of(kind);
            assert!(
                (luma(p.warning) - luma(p.warning_bg)).abs() > 200,
                "{kind:?} warning ink is too close to its fill",
            );
        }
    }

    /// The rail is a panel now, so its width has to cover the design's
    /// two-column tool grid *and* what a panel puts around a body: the padding
    /// either side and the scroll bar. Widening the buttons without widening
    /// the rail would wrap the grid to one column — which is what the grid
    /// falls back to when it has to, and not the shape it should ship in.
    ///
    /// The gutter is counted rather than assumed to be somewhere in the slack.
    /// It used to be: the sum came to 90 against a 100-point rail, and the ten
    /// points left over happened to be exactly the gutter — which held only
    /// because nothing said so, and a scroll bar that floated over the buttons
    /// would have passed this either way.
    #[test]
    fn the_tool_grid_fits_the_panel_it_sits_in() {
        let grid = metrics::TOOL_BUTTON * 2.0 + metrics::TOOL_GAP;
        let padding = metrics::PANEL_PAD as f32 * 2.0;
        let gutter = metrics::SCROLL_BAR + metrics::SCROLL_BAR_GAP;
        assert!(
            grid + padding + gutter <= metrics::TOOL_RAIL,
            "a {grid}-point grid, {padding} points of panel padding and a \
             {gutter}-point scroll gutter do not fit a {}-point rail",
            metrics::TOOL_RAIL,
        );
    }
}
