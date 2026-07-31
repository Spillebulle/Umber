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

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub accent: Color32,
    /// Behind the document — the darkest surface.
    pub backdrop: Color32,
    /// Panel interiors and inset wells.
    pub window: Color32,
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
}

impl Palette {
    pub const fn graphite() -> Self {
        Self {
            accent: Color32::from_rgb(0xC1, 0x8B, 0x5E),
            backdrop: Color32::from_rgb(0x0D, 0x0E, 0x10),
            window: Color32::from_rgb(0x11, 0x12, 0x14),
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
        }
    }

    pub const fn paper() -> Self {
        Self {
            accent: Color32::from_rgb(0x9C, 0x62, 0x2F),
            backdrop: Color32::from_rgb(0xE4, 0xE0, 0xD9),
            window: Color32::from_rgb(0xEF, 0xEC, 0xE7),
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
        }
    }

    pub fn of(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Graphite => Self::graphite(),
            ThemeKind::Paper => Self::paper(),
        }
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
    pub const MENU_BAR: f32 = 36.0;
    pub const STATUS_BAR: f32 = 26.0;
    pub const TOOL_RAIL: f32 = 48.0;
    pub const TOOL_BUTTON: f32 = 36.0;
    pub const PANEL: f32 = 264.0;
    pub const PANEL_PAD: f32 = 14.0;
    pub const RADIUS: f32 = 5.0;
    pub const RADIUS_LARGE: f32 = 6.0;
    pub const SLIDER_ROW: f32 = 16.0;
    pub const SLIDER_RAIL: f32 = 3.0;
    pub const SLIDER_KNOB: f32 = 11.0;
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
