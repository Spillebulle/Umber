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

/// The themes compiled into Umber.
///
/// The first two are the design's own. The other four are drawn from the
/// interfaces of four other painting applications, sampled from screenshots of
/// each: they exist because "which greys does a painter already know" is a
/// better answer to "give me a second dark theme" than another set invented
/// here. They are still nothing but a [`Palette`] — no branch anywhere that
/// draws, no second door into egui's styling — which is the whole reason a
/// theme is a table of values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeKind {
    /// Near-black flat workbench. The design's default.
    Graphite,
    /// Warm paper neutrals.
    Paper,
    /// Adobe Photoshop's neutral greys.
    Photoslop,
    /// Clip Studio Paint's dark chrome and its blue-grey selection.
    ShitStudio,
    /// Krita's mid grey and its slate-blue selection.
    Krita,
    /// MediBang Paint Pro's warm-dark chrome and bright blue.
    MediaBog,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 6] = [
        Self::Graphite,
        Self::Paper,
        Self::Photoslop,
        Self::ShitStudio,
        Self::Krita,
        Self::MediaBog,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Graphite => "Graphite",
            Self::Paper => "Paper",
            Self::Photoslop => "Photoslop",
            Self::ShitStudio => "Shit Studio Paint",
            Self::Krita => "Krita",
            Self::MediaBog => "MediaBog Pro",
        }
    }

    /// What a file calls this theme.
    ///
    /// One statement of it, read by `prefs` for the preferences file and by
    /// `themelib` for a `.umbertheme`'s `base` line. Those two used to hold a
    /// `match` each with a comment saying they were deliberately the same
    /// words, which is a thing that has to be true and nothing making it so.
    ///
    /// A `match` rather than a derive, for the reason `prefs::accent_id` gives:
    /// it is the point at which somebody adding a theme is forced to choose the
    /// name it will be stored under, instead of discovering later that renaming
    /// the variant silently reset everyone's theme. Stable for ever, and
    /// deliberately **not** [`ThemeKind::label`] lower-cased — a label is what
    /// the interface shows and is free to be reworded.
    /// `the_stored_name_of_every_theme_is_this_exact_string` pins them.
    pub fn id(self) -> &'static str {
        match self {
            Self::Graphite => "graphite",
            Self::Paper => "paper",
            Self::Photoslop => "photoslop",
            Self::ShitStudio => "shitstudio",
            Self::Krita => "krita",
            Self::MediaBog => "mediabog",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    /// Whether this theme's interface is dark.
    ///
    /// Stated per theme rather than measured off the palette, because it is
    /// asked by [`Accent::ink`] and by [`Palette::with_accent`] to decide how a
    /// re-hued accent and its muted twin are derived — and a theme somebody
    /// edits into the opposite lightness must not change what its *authored*
    /// derivation was. [`Palette::is_dark`] is the other reading, off the
    /// colours, and is what egui's own styling follows;
    /// `every_theme_agrees_with_itself_about_being_dark` holds the two
    /// together for the six compiled in.
    pub fn is_dark(self) -> bool {
        match self {
            Self::Graphite | Self::Photoslop | Self::ShitStudio | Self::Krita | Self::MediaBog => {
                true
            }
            Self::Paper => false,
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
    /// [`Accent::Umber`] is the *theme's own authored accent*, whatever that
    /// theme is: Graphite's and Paper's are the design's hand-picked pair
    /// (`#C08A4E` to `#9C622F`), and each preset theme carries the blue its
    /// application uses. Read off the palette rather than restated here, so the
    /// two cannot drift — and this used to spell Graphite's and Paper's out,
    /// which for a preset theme would have handed back the design's ochre for a
    /// theme that has never worn it.
    ///
    /// The other three accents exist only as a single dark swatch, so the light
    /// variant is derived — darkened towards black far enough to clear text
    /// contrast on a light surface, which is the same relationship Umber's two
    /// authored values already have. Keyed on
    /// [`ThemeKind::is_dark`], which is itself an exhaustive `match`, so a
    /// seventh theme still fails the build somewhere rather than quietly
    /// taking the dark answer here.
    pub fn ink(self, kind: ThemeKind) -> Color32 {
        if self == Self::Umber {
            return Palette::of(kind).accent;
        }
        if kind.is_dark() {
            self.swatch()
        } else {
            mix(self.swatch(), Color32::BLACK, 0.30)
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

/// A named colour in a [`Palette`] — one row of the theme editor, and one line
/// of a `.umbertheme` file.
///
/// An enum with exhaustive `match`es in [`Palette::token`] and
/// [`Palette::set_token`] rather than a table of names beside the struct,
/// because the point of it is that adding a field to `Palette` stops compiling
/// until somebody has decided what the editor calls it and what a file stores
/// it under. That is the rule the brush editor already lives by — "between them
/// they expose every field of `Brush`; adding one means adding a control, or
/// the library can use a brush nobody can make" — and it matters more here,
/// because a token nobody exposed is one a hand-written theme can set and the
/// editor silently reverts.
///
/// [`Token::id`] is what a file says and must never be reworded; [`Token::label`]
/// is what the interface shows and is free to be. The same division
/// `prefs::theme_id` keeps against [`ThemeKind::label`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Backdrop,
    Window,
    Dock,
    Chrome,
    Popover,
    Border,
    PopoverBorder,
    Control,
    ControlHover,
    ControlActive,
    Rail,
    Knob,
    TextStrong,
    Text,
    TextMuted,
    TextDim,
    Accent,
    AccentDim,
    Warning,
    WarningBg,
    WarningBorder,
    /// One of [`Palette::link_colours`], numbered from zero.
    Link(u8),
}

/// A heading in the theme editor, and nothing else — the file is a flat list of
/// tokens, so a group renamed changes no stored byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenGroup {
    Surfaces,
    Lines,
    Controls,
    Type,
    Accent,
    Warnings,
    Links,
}

impl TokenGroup {
    pub const ALL: [TokenGroup; 7] = [
        Self::Surfaces,
        Self::Lines,
        Self::Controls,
        Self::Type,
        Self::Accent,
        Self::Warnings,
        Self::Links,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Surfaces => "Surfaces",
            Self::Lines => "Lines",
            Self::Controls => "Controls",
            Self::Type => "Type",
            Self::Accent => "Accent",
            Self::Warnings => "Warnings",
            Self::Links => "Link colours",
        }
    }

    /// The tokens under this heading, in the order the editor draws them.
    pub fn tokens(self) -> Vec<Token> {
        Token::ALL
            .into_iter()
            .filter(|t| t.group() == self)
            .collect()
    }
}

impl Token {
    /// Every token, in the order the editor draws them — which is also the
    /// order they are written to a file, so a `.umbertheme` reads top to bottom
    /// like the pane it came from.
    pub const ALL: [Token; 21 + umber_core::LayerStack::LINK_GROUPS] = [
        Self::Backdrop,
        Self::Window,
        Self::Dock,
        Self::Chrome,
        Self::Popover,
        Self::Border,
        Self::PopoverBorder,
        Self::Control,
        Self::ControlHover,
        Self::ControlActive,
        Self::Rail,
        Self::Knob,
        Self::TextStrong,
        Self::Text,
        Self::TextMuted,
        Self::TextDim,
        Self::Accent,
        Self::AccentDim,
        Self::Warning,
        Self::WarningBg,
        Self::WarningBorder,
        Self::Link(0),
        Self::Link(1),
        Self::Link(2),
        Self::Link(3),
        Self::Link(4),
        Self::Link(5),
    ];

    /// What a `.umbertheme` calls this token.
    ///
    /// Stable for ever, like `prefs`'s own ids and for the same reason: the
    /// file has to parse in next year's build. Deliberately **not** the label
    /// lower-cased — a label is what the interface shows and is free to be
    /// reworded.
    pub fn id(self) -> &'static str {
        match self {
            Self::Backdrop => "backdrop",
            Self::Window => "window",
            Self::Dock => "dock",
            Self::Chrome => "chrome",
            Self::Popover => "popover",
            Self::Border => "border",
            Self::PopoverBorder => "popover_border",
            Self::Control => "control",
            Self::ControlHover => "control_hover",
            Self::ControlActive => "control_active",
            Self::Rail => "rail",
            Self::Knob => "knob",
            Self::TextStrong => "text_strong",
            Self::Text => "text",
            Self::TextMuted => "text_muted",
            Self::TextDim => "text_dim",
            Self::Accent => "accent",
            Self::AccentDim => "accent_dim",
            Self::Warning => "warning",
            Self::WarningBg => "warning_bg",
            Self::WarningBorder => "warning_border",
            // Numbered from one, because they are numbered from one everywhere
            // a person sees them.
            Self::Link(0) => "link_1",
            Self::Link(1) => "link_2",
            Self::Link(2) => "link_3",
            Self::Link(3) => "link_4",
            Self::Link(4) => "link_5",
            Self::Link(_) => "link_6",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.id() == id)
    }

    /// What the theme editor calls this token.
    ///
    /// The six the design draws keep the design's own words — Background,
    /// Panel, Canvas pit, Text, Accent, Hairline — rather than the field's
    /// name, because those are what the page has always said.
    pub fn label(self) -> &'static str {
        match self {
            Self::Backdrop => "Canvas pit",
            Self::Window => "Background",
            Self::Dock => "Dock column",
            Self::Chrome => "Panel",
            Self::Popover => "Menu",
            Self::Border => "Hairline",
            Self::PopoverBorder => "Menu edge",
            Self::Control => "Button",
            Self::ControlHover => "Button, hovered",
            Self::ControlActive => "Button, selected",
            Self::Rail => "Slider track",
            Self::Knob => "Slider knob",
            Self::TextStrong => "Text, strong",
            Self::Text => "Text",
            Self::TextMuted => "Text, muted",
            Self::TextDim => "Text, dim",
            Self::Accent => "Accent",
            Self::AccentDim => "Accent, muted",
            Self::Warning => "Warning ink",
            Self::WarningBg => "Warning fill",
            Self::WarningBorder => "Warning edge",
            Self::Link(0) => "Link 1",
            Self::Link(1) => "Link 2",
            Self::Link(2) => "Link 3",
            Self::Link(3) => "Link 4",
            Self::Link(4) => "Link 5",
            Self::Link(_) => "Link 6",
        }
    }

    pub fn group(self) -> TokenGroup {
        match self {
            Self::Backdrop | Self::Window | Self::Dock | Self::Chrome | Self::Popover => {
                TokenGroup::Surfaces
            }
            Self::Border | Self::PopoverBorder => TokenGroup::Lines,
            Self::Control | Self::ControlHover | Self::ControlActive | Self::Rail | Self::Knob => {
                TokenGroup::Controls
            }
            Self::TextStrong | Self::Text | Self::TextMuted | Self::TextDim => TokenGroup::Type,
            Self::Accent | Self::AccentDim => TokenGroup::Accent,
            Self::Warning | Self::WarningBg | Self::WarningBorder => TokenGroup::Warnings,
            Self::Link(_) => TokenGroup::Links,
        }
    }
}

/// `PartialEq` because a theme somebody is editing has to be comparable with
/// the one on disk — every field is a `Color32`, so it is a field-by-field byte
/// comparison and there is no tolerance to get wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// The link marks the four preset themes share.
///
/// Shared rather than copied four times, and that is not a token left carrying
/// another theme's value: a link colour is a position on the hue wheel and not
/// a property of the interface it sits on — Graphite's and Paper's own sets are
/// the same six hues at two lightnesses. What decides them here is the one
/// thing all four presets have in common: **every one of them accents in blue**,
/// so the blue Graphite leads with would sit a few units from the accent on the
/// row beside it. Orange takes its place, and the remaining five are Graphite's
/// unchanged.
/// `link_colours_are_told_apart_from_each_other_and_from_every_accent` is the
/// measurement, and it runs over every theme.
///
/// There is deliberately no light twin of this table. There was one, for a
/// [`ThemeKind::ShitStudio`] that had been built light against a brief that
/// misread its own reference; Paper is the only light theme, its own set is
/// authored on it, and a second table nothing named would be a set of colours
/// nobody could see to judge.
const PRESET_LINKS_DARK: [Color32; umber_core::LayerStack::LINK_GROUPS] = [
    Color32::from_rgb(0xE8, 0x6B, 0x32), // orange
    Color32::from_rgb(0x46, 0xB0, 0x4A), // green
    Color32::from_rgb(0xA9, 0x6B, 0xE8), // violet
    Color32::from_rgb(0x1F, 0xB5, 0xB5), // teal
    Color32::from_rgb(0xEE, 0x5A, 0xA8), // rose
    Color32::from_rgb(0xF0, 0xD5, 0x3C), // yellow
];

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

    /// Adobe Photoshop's neutral greys.
    ///
    /// Every grey here is sampled from the reference screenshot, which carries
    /// five distinct ones: `#282828` behind the document, then `#383838`,
    /// `#424242`, `#454545`, `#4A4A4A`, `#4D4D4D` and `#535353` up through the
    /// panels and strips. What is *not* the screenshot is where the panel
    /// surface sits in that ladder. That shot is Photoshop's lighter "Medium"
    /// interface, whose chrome is `#535353`; taken at face value it would put
    /// this theme's panels above Krita's `#474747` and MediBang's `#4A4747`,
    /// and three neutral greys within six units of each other are three themes
    /// nobody can tell apart. So the surface is pitched at the deeper end of
    /// the same ladder — which is also Photoshop's own default interface, the
    /// one most people picture — and the theme comes out dark, neutral and
    /// several greys deep, which is what it is for.
    ///
    /// The accent is Adobe's blue rather than a grey, because Umber draws
    /// hyperlinks, dashed marks and focus in it and a grey accent is no accent.
    /// It is restrained where Photoshop is restrained: nothing in the *palette*
    /// wears it, so the only places it appears are the ones the design already
    /// says are accented.
    pub const fn photoslop() -> Self {
        Self {
            accent: Color32::from_rgb(0x4A, 0x96, 0xF0),
            accent_dim: Color32::from_rgb(0x38, 0x5F, 0x8D),
            warning: Color32::from_rgb(0xE0, 0x87, 0x6A),
            warning_bg: Color32::from_rgb(0x3A, 0x26, 0x20),
            // Authored against this theme's own fill, not Graphite's. It was
            // `#6E4034` — Graphite's exactly — beside a `warning` and a
            // `warning_bg` that had both been moved, which is the shape of a
            // token somebody forgot rather than one they chose.
            warning_border: Color32::from_rgb(0x74, 0x46, 0x3A),
            backdrop: Color32::from_rgb(0x28, 0x28, 0x28),
            window: Color32::from_rgb(0x2E, 0x2E, 0x2E),
            dock: Color32::from_rgb(0x32, 0x32, 0x32),
            chrome: Color32::from_rgb(0x38, 0x38, 0x38),
            border: Color32::from_rgb(0x4A, 0x4A, 0x4A),
            popover: Color32::from_rgb(0x42, 0x42, 0x42),
            popover_border: Color32::from_rgb(0x56, 0x56, 0x56),
            control: Color32::from_rgb(0x45, 0x45, 0x45),
            control_hover: Color32::from_rgb(0x51, 0x51, 0x51),
            // Neutral rather than tinted towards the accent, unlike Graphite's:
            // a selected tool in Photoshop is a lighter grey and nothing else,
            // and that restraint is most of what the interface reads as.
            control_active: Color32::from_rgb(0x5A, 0x5A, 0x5A),
            text_strong: Color32::from_rgb(0xF1, 0xF0, 0xF0),
            text: Color32::from_rgb(0xDE, 0xDD, 0xDD),
            text_muted: Color32::from_rgb(0xAB, 0xAA, 0xAA),
            text_dim: Color32::from_rgb(0x8F, 0x8F, 0x8F),
            rail: Color32::from_rgb(0x26, 0x26, 0x26),
            knob: Color32::from_rgb(0xC6, 0xC6, 0xC6),
            link_colours: PRESET_LINKS_DARK,
        }
    }

    /// Clip Studio Paint's dark chrome and its blue-grey selection.
    ///
    /// Sampled from the reference screenshot, which is Clip Studio's **dark**
    /// interface: `#3F3F3F` panel bodies and rail, a `#4E4E4E` menu bar and
    /// every strip and panel header above it, `#383838` behind the document
    /// tabs, a `#2E2E2E` canvas surround, and `#606B7F` on the selected tool,
    /// the selected sub-tool row, the selected document tab and the selected
    /// layer row.
    ///
    /// That blue-grey is the one saturated thing in a Clip Studio window that
    /// is not the picture, and it is the same colour in all four of those
    /// places, so `control_active` is the measured value with nothing done to
    /// it.
    ///
    /// This was built light first, from a brief that said the reference showed
    /// Clip Studio's light interface. It does not, and the light draft was
    /// defended on the reasoning that a fourth dark preset would be hard to
    /// tell from the other three — which was never ours to decide in place of
    /// somebody asking for the reference to be followed. **The worry was real
    /// and is answered by the ladder rather than by the selection**, and the
    /// distinction is worth stating because the obvious answer is wrong:
    /// Krita's selected row is `#54718E` and MediaBog's is `#3E7EB2`, so "the
    /// only preset with a colour on a selected row" would be false. What is
    /// true is that this one's is **grey** — 24% saturated against their 41%
    /// and 65% — and that `#4E4E4E` is the lightest chrome of any preset here
    /// (Photoslop `#383838`, Krita `#474747`, MediaBog `#4A4747`), over
    /// `#3F3F3F` panels and a `#2E2E2E` pit. A brighter, wider ladder with a
    /// flat slate on it is what a Clip Studio window looks like across a room.
    ///
    /// Two departures, both forced by contrast and both stated.
    /// `control` is `#5C5C5C` where the measured button fill is `#676767`: at
    /// the measured value `text_muted` on a resting button is 2.73:1 and the
    /// accent on one is 2.74:1, both under the floors this palette's own
    /// `text` is held to on that same fill. Nothing is thrown away, because
    /// the measured grey is `control_hover` — a button lifts to Clip Studio's
    /// own tone under the pointer. And `popover` is placed in the ladder
    /// rather than sampled, because no menu is open in the reference; it sits
    /// between the panel and the menu bar, which is the direction Clip
    /// Studio's menus go.
    ///
    /// The accent is that same blue-grey lifted until it reads as ink.
    /// `#606B7F` on `#4E4E4E` is **1.55:1**, which is not an accent at all,
    /// and Umber draws hyperlinks, dashed marks and focus in it. The lift
    /// raises the saturation as well as the lightness, deliberately: merely
    /// brightening a blue-grey lands beside `Accent::Steel`, which is itself a
    /// blue-grey — `#A0AEC4` is 68 from it against a floor of 60 — and that is
    /// the trap Krita's measured slate already fell into. `#8FB8E6` is 95
    /// away and 4.03:1 on chrome.
    ///
    /// **The canvas pit needs no deviation, and the light draft had one.**
    /// `widgets.rs` inks the canvas scrollbar thumb and the pen dot in
    /// `text_dim` over `backdrop`, so the light draft had to darken its pit
    /// away from the mid grey to reach 2.70:1. Clip Studio's real pit is
    /// `#2E2E2E` and the thumb reads **4.95:1** on it, so here the faithful
    /// colour is also the one that passes and there is no trade to record.
    /// Krita is the theme that still pays it; see there.
    pub const fn shit_studio() -> Self {
        Self {
            accent: Color32::from_rgb(0x8F, 0xB8, 0xE6),
            // `mix(accent, window, 0.49)` exactly — the derivation
            // `with_accent` applies to the other three accents, which Krita's
            // and MediaBog's authored values also sit on and Photoslop's within
            // 12. This was `#54637A`, hand-picked and **61** off it, in the
            // direction of `control_active`: `accent_dim` is the border
            // `icon_toggle` and `layer_row` stroke round a selected row, and at
            // 25 from that row's own `#606B7F` fill it was a border nobody
            // could see. The derived value is 36 away and consistent with the
            // rest of the file.
            accent_dim: Color32::from_rgb(0x64, 0x79, 0x91),
            warning: Color32::from_rgb(0xE8, 0x90, 0x7A),
            warning_bg: Color32::from_rgb(0x3C, 0x28, 0x23),
            warning_border: Color32::from_rgb(0x78, 0x49, 0x3C),
            backdrop: Color32::from_rgb(0x2E, 0x2E, 0x2E),
            window: Color32::from_rgb(0x38, 0x38, 0x38),
            dock: Color32::from_rgb(0x3F, 0x3F, 0x3F),
            chrome: Color32::from_rgb(0x4E, 0x4E, 0x4E),
            // Darker than the surface it divides, unlike Photoslop's and
            // Krita's: Clip Studio separates its panels with a near-black line
            // where Photoshop separates them by tone. Measured off the
            // dividers either side of the canvas.
            border: Color32::from_rgb(0x30, 0x30, 0x30),
            popover: Color32::from_rgb(0x48, 0x48, 0x48),
            popover_border: Color32::from_rgb(0x5E, 0x5E, 0x5E),
            control: Color32::from_rgb(0x5C, 0x5C, 0x5C),
            // The measured fill — see above for why it is the hover and not
            // the resting state.
            control_hover: Color32::from_rgb(0x67, 0x67, 0x67),
            control_active: Color32::from_rgb(0x60, 0x6B, 0x7F),
            // The whole type ramp is a step above Krita's, and the reason is
            // this palette's own `chrome`: `#4E4E4E` against Krita's `#474747`
            // is 21 channel-sum units lighter, so every ink has that much less
            // headroom. The two strongest ranks were Krita's byte for byte —
            // `#F0F0F0` and `#DCDCDC` — which is the shape `photoslop`'s
            // `warning_border` and `mediabog`'s `popover_border` are both
            // commented as: a token nobody re-chose after the surfaces moved.
            // Neither is a measurement of Clip Studio's own text, which peaks
            // around `#B2B2B2` in the menu strip; these are Umber's ranks, and
            // they are pitched at this theme's surfaces.
            text_strong: Color32::from_rgb(0xF4, 0xF4, 0xF4),
            text: Color32::from_rgb(0xE2, 0xE2, 0xE2),
            text_muted: Color32::from_rgb(0xB4, 0xB4, 0xB4),
            // Held up to `#9C` by the `#4E4E4E` menu bar, which is the
            // lightest surface any theme here draws text on: at `#949494` it
            // is 2.74:1, under the 2.9 floor Paper set. Demonstrated by
            // mutation rather than argued — `#949494` is what
            // `text_reads_against_every_surface_it_is_drawn_on` was fed to
            // check that it can still see this palette at all.
            text_dim: Color32::from_rgb(0x9C, 0x9C, 0x9C),
            rail: Color32::from_rgb(0x2C, 0x2C, 0x2C),
            knob: Color32::from_rgb(0xCE, 0xCE, 0xCE),
            link_colours: PRESET_LINKS_DARK,
        }
    }

    /// Krita's mid grey and its slate-blue selection.
    ///
    /// `#474747` panels, `#414141` docker headers and `#383838` list wells are
    /// all sampled. **The canvas pit is not**, and it is the one place this
    /// theme knowingly departs from the application it is named for.
    ///
    /// Krita surrounds the page with a flat 50% `#808080` — lighter than its
    /// own interface, which is the thing that makes a Krita window
    /// recognisable across a room, and it was this palette's `backdrop` until
    /// it was measured against what gets drawn on it. Three marks are drawn in
    /// `text_dim` over `backdrop` and nothing else: the canvas scrollbar thumb,
    /// the dot that replaces the cursor under a pen, and the splash's status
    /// line. `widgets.rs` explains why — `text_dim` is the only ink that is a
    /// mid-grey whichever way the surfaces run — and names the bar it rejected:
    /// `rail` at **1.31:1**. A `#808080` pit puts the thumb at **1.34:1**,
    /// worse than the value that argument threw out, and the pen dot with it.
    /// No pit between the panels and 50% grey fixes it, because `text_dim` is
    /// itself a mid-grey: the contrast is lowest exactly where the pit is.
    /// So the pit is dark like every other theme's, at 5.11:1, and what carries
    /// Krita here is its mid grey and its slate selection — which is what it
    /// was asked for.
    /// **The other repair is the better one and is not in this file**: draw
    /// that thumb in something chosen against the backdrop rather than in
    /// `text_dim`. Do that and this can go back to `#808080`.
    ///
    /// `control_active` is the measured selection fill; `accent` is the same
    /// slate blue lifted until it reads as ink on `#474747` *and* clears
    /// `Accent::Steel`, which the measured blue did not — see
    /// `no_two_accents_look_alike_in_one_theme`. That is the relationship
    /// Graphite's `control_active` and `accent` already have.
    pub const fn krita() -> Self {
        Self {
            accent: Color32::from_rgb(0x66, 0xAA, 0xEC),
            accent_dim: Color32::from_rgb(0x4F, 0x72, 0x94),
            warning: Color32::from_rgb(0xE0, 0x8A, 0x6E),
            warning_bg: Color32::from_rgb(0x3B, 0x29, 0x24),
            warning_border: Color32::from_rgb(0x71, 0x44, 0x39),
            backdrop: Color32::from_rgb(0x26, 0x26, 0x26),
            window: Color32::from_rgb(0x38, 0x38, 0x38),
            dock: Color32::from_rgb(0x41, 0x41, 0x41),
            chrome: Color32::from_rgb(0x47, 0x47, 0x47),
            border: Color32::from_rgb(0x57, 0x57, 0x57),
            // Below the measured trio rather than inside it. `#3F3F3F` was the
            // reading and sat two units off `dock`; `#3A3A3A` moved it two
            // units off `window`, which is `extreme_bg_color` and therefore
            // every inset well a menu can be dropped over. This clears both,
            // and darker is the direction Krita's own menus go.
            popover: Color32::from_rgb(0x31, 0x31, 0x31),
            popover_border: Color32::from_rgb(0x5C, 0x5C, 0x5C),
            control: Color32::from_rgb(0x52, 0x52, 0x52),
            control_hover: Color32::from_rgb(0x5E, 0x5E, 0x5E),
            control_active: Color32::from_rgb(0x54, 0x71, 0x8E),
            text_strong: Color32::from_rgb(0xF0, 0xF0, 0xF0),
            text: Color32::from_rgb(0xDC, 0xDC, 0xDC),
            text_muted: Color32::from_rgb(0xB0, 0xB0, 0xB0),
            text_dim: Color32::from_rgb(0x96, 0x96, 0x96),
            rail: Color32::from_rgb(0x2A, 0x2A, 0x2A),
            knob: Color32::from_rgb(0xB8, 0xB8, 0xB8),
            link_colours: PRESET_LINKS_DARK,
        }
    }

    /// MediBang Paint Pro's warm-dark chrome and bright blue.
    ///
    /// The panels are a faintly warm `#4A4747` — 74, 71, 71, and the three
    /// channels being unequal is the whole of what separates this from Krita's
    /// flat `#474747` at a glance — over near-black `#252525` strips, with
    /// `#393737` panel headers. That near-black is this theme's *hairline*:
    /// MediBang separates by a dark line where Photoshop separates by tone, so
    /// `border` is darker than the surface here and lighter than it there.
    ///
    /// The pit is one step below the measured `#4A4747`, which the reference
    /// shares exactly with the panels. Faithful would be the same grey on both,
    /// and a canvas surround indistinguishable from the panel beside it is a
    /// document with no edge.
    pub const fn mediabog() -> Self {
        Self {
            // Lifted from the measured `#1883D7`/`#4FA8E8` far enough that the
            // *emphasised* text button's label clears 3:1 on `control`, which
            // is the one place the accent is ink on a fill rather than on a
            // panel. `#4FA8E8` reads 2.76 there.
            accent: Color32::from_rgb(0x5F, 0xB2, 0xEE),
            accent_dim: Color32::from_rgb(0x48, 0x71, 0x90),
            warning: Color32::from_rgb(0xE8, 0x89, 0x6B),
            warning_bg: Color32::from_rgb(0x3A, 0x2A, 0x24),
            warning_border: Color32::from_rgb(0x7A, 0x4A, 0x38),
            backdrop: Color32::from_rgb(0x44, 0x41, 0x41),
            window: Color32::from_rgb(0x30, 0x2E, 0x2E),
            dock: Color32::from_rgb(0x39, 0x37, 0x37),
            chrome: Color32::from_rgb(0x4A, 0x47, 0x47),
            border: Color32::from_rgb(0x26, 0x24, 0x24),
            // Off the measured `#393737` the panel headers wear, because the
            // dock column already has that value and a menu dropped over one
            // would be nothing but its own hairline. Graphite and Paper both
            // keep these two apart; noticed in `every_theme_preview`, which is
            // what a picture is for.
            popover: Color32::from_rgb(0x40, 0x3D, 0x3D),
            // Warm, like every other grey here. It was `#5C5C5C` — neutral,
            // and byte for byte Krita's, in the one theme whose stated identity
            // is that its channels are unequal.
            popover_border: Color32::from_rgb(0x5E, 0x5A, 0x5A),
            // Deep enough that `text` on a resting button clears 4.5:1. At the
            // measured `#5A5757` it is 4.28, which is under the floor this
            // palette's own `text_strong` is held to on the identical surface.
            control: Color32::from_rgb(0x55, 0x52, 0x52),
            control_hover: Color32::from_rgb(0x66, 0x62, 0x62),
            // The blue a selected row wears, taken deeper than the measured
            // `#559CD1` so that `text_strong` on it clears 3:1 — MediBang draws
            // white on its own lighter fill and gets 2.97, and this palette's
            // strong ink is not white. Measured: 3.61 here, 2.85 at `#4A90C8`,
            // which is the value this was until
            // `text_reads_against_every_surface_it_is_drawn_on` was pointed at
            // a selected row.
            control_active: Color32::from_rgb(0x3E, 0x7E, 0xB2),
            text_strong: Color32::from_rgb(0xEA, 0xEA, 0xEA),
            text: Color32::from_rgb(0xC8, 0xC8, 0xC8),
            text_muted: Color32::from_rgb(0xAD, 0xAD, 0xAD),
            text_dim: Color32::from_rgb(0x94, 0x94, 0x94),
            // Below `window` rather than equal to it, which is what it was: a
            // slider track inside an inset well would have had nothing to sit
            // on. No other theme sets those two the same.
            rail: Color32::from_rgb(0x27, 0x25, 0x25),
            knob: Color32::from_rgb(0xC8, 0xC8, 0xC8),
            link_colours: PRESET_LINKS_DARK,
        }
    }

    /// One token, by name.
    ///
    /// The exhaustive `match` is the point — see [`Token`].
    pub fn token(&self, token: Token) -> Color32 {
        match token {
            Token::Backdrop => self.backdrop,
            Token::Window => self.window,
            Token::Dock => self.dock,
            Token::Chrome => self.chrome,
            Token::Popover => self.popover,
            Token::Border => self.border,
            Token::PopoverBorder => self.popover_border,
            Token::Control => self.control,
            Token::ControlHover => self.control_hover,
            Token::ControlActive => self.control_active,
            Token::Rail => self.rail,
            Token::Knob => self.knob,
            Token::TextStrong => self.text_strong,
            Token::Text => self.text,
            Token::TextMuted => self.text_muted,
            Token::TextDim => self.text_dim,
            Token::Accent => self.accent,
            Token::AccentDim => self.accent_dim,
            Token::Warning => self.warning,
            Token::WarningBg => self.warning_bg,
            Token::WarningBorder => self.warning_border,
            Token::Link(n) => self.link_colours[n as usize % self.link_colours.len()],
        }
    }

    pub fn set_token(&mut self, token: Token, colour: Color32) {
        let slot = match token {
            Token::Backdrop => &mut self.backdrop,
            Token::Window => &mut self.window,
            Token::Dock => &mut self.dock,
            Token::Chrome => &mut self.chrome,
            Token::Popover => &mut self.popover,
            Token::Border => &mut self.border,
            Token::PopoverBorder => &mut self.popover_border,
            Token::Control => &mut self.control,
            Token::ControlHover => &mut self.control_hover,
            Token::ControlActive => &mut self.control_active,
            Token::Rail => &mut self.rail,
            Token::Knob => &mut self.knob,
            Token::TextStrong => &mut self.text_strong,
            Token::Text => &mut self.text,
            Token::TextMuted => &mut self.text_muted,
            Token::TextDim => &mut self.text_dim,
            Token::Accent => &mut self.accent,
            Token::AccentDim => &mut self.accent_dim,
            Token::Warning => &mut self.warning,
            Token::WarningBg => &mut self.warning_bg,
            Token::WarningBorder => &mut self.warning_border,
            Token::Link(n) => {
                let at = n as usize % self.link_colours.len();
                &mut self.link_colours[at]
            }
        };
        *slot = colour;
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
    ///
    /// The one place a [`ThemeKind`] becomes colours, which is what lets
    /// [`Accent::ink`] read a theme's own accent off its palette without the
    /// two of them calling each other in a circle.
    pub const fn of(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Graphite => Self::graphite(),
            ThemeKind::Paper => Self::paper(),
            ThemeKind::Photoslop => Self::photoslop(),
            ThemeKind::ShitStudio => Self::shit_studio(),
            ThemeKind::Krita => Self::krita(),
            ThemeKind::MediaBog => Self::mediabog(),
        }
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
        let mut palette = Self::of(kind);
        if accent == Accent::Umber {
            return palette;
        }
        palette.accent = accent.ink(kind);
        // Which surface a muted accent recedes towards. Graphite keeps its
        // backdrop, exactly as it always did; every other theme takes `window`,
        // because Krita's pit is *lighter* than its panels and an accent mixed
        // towards it would come out louder than the accent it mutes — which is
        // the one thing `accent_dim` may never be, and what
        // `a_derived_dim_recedes_towards_its_own_surface` measures. Paper's
        // recessive surface was already `window`, so nothing shipped moves.
        let recessive = match kind {
            ThemeKind::Graphite => palette.backdrop,
            ThemeKind::Paper
            | ThemeKind::Photoslop
            | ThemeKind::ShitStudio
            | ThemeKind::Krita
            | ThemeKind::MediaBog => palette.window,
        };
        let towards = if kind.is_dark() { 0.49 } else { 0.60 };
        palette.accent_dim = mix(palette.accent, recessive, towards);
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
    /// A dash, and the gap after it, wherever this interface draws a dashed
    /// mark: the dock's "dock here" indicator, the layer list's drop slot and
    /// the palette grid's drop ring.
    ///
    /// Here rather than beside any one of them, because dashed is how this
    /// interface spells "not a real piece of chrome, a place something is
    /// going" — three marks saying one thing, which they only do for as long
    /// as the rhythm is the same. `panels.rs` still holds a private pair of
    /// its own with these values in it; folding it into this is the whole of
    /// what is left to do here.
    pub const DASH: f32 = 5.0;
    pub const DASH_GAP: f32 = 4.0;
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
    ///
    /// A *fixed* size, for the reason [`BRUSH_LIBRARY`] is one and then some:
    /// its six sections are different lengths — Inputs is a list of arbitrary
    /// length and Tip is a fixed grid — so a dialog sized by its content was a
    /// different dialog on every tab, and a modal is centred, so changing
    /// height moves both of its edges and takes the tab strip out from under
    /// the pointer that had just clicked it.
    ///
    /// The height is measured rather than chosen, and the measurement is
    /// `brush_editor_preview` — a picture drawn through Umber's own style,
    /// which is the only reading worth taking. (A first attempt measured the
    /// six sections through a bare egui context and every gap in it was half
    /// the one on screen. See `ui::BRUSH_EDITOR_FOOTER`, which was six points
    /// short for the same reason.)
    ///
    /// Of the 600, `ui::BRUSH_EDITOR_FOOTER` and `ui::BRUSH_EDITOR_GAP` take an
    /// exact 59 at the bottom and the header and tab strip about 83 at the top,
    /// leaving a body of roughly 458. The tallest section that cannot grow is
    /// Scatter, at about 444 — so it clears by a little over ten points, and a
    /// caption that wrapped one more line would scroll rather than move the
    /// frame. The shortest, Texture and Blending, leave a good deal of it
    /// empty, and that is the price of one size, paid deliberately: what it
    /// buys is a tab strip that stays where it was clicked. Inputs is the one
    /// section with no ceiling — the shortest of the six until a brush has a
    /// modulation on it and by far the tallest afterwards. See
    /// `ui::brush_editor`.
    pub const BRUSH_EDITOR: [f32; 2] = [560.0, 600.0];
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
    /// One colour in the Palette module's grid, and the gap between two.
    ///
    /// Big enough that a colour can be judged against its neighbours and that
    /// the remove mark inside its corner is a real target, and small enough
    /// that a 264 px panel fits eight across — which is what makes a palette of
    /// the usual sixteen two tidy rows.
    pub const PALETTE_SWATCH: f32 = 26.0;
    pub const PALETTE_SWATCH_GAP: f32 = 4.0;
    /// The palette library: how wide the modal is, and how tall its list is.
    ///
    /// Fixed, for the reason [`BRUSH_LIBRARY`] is: the rows carry names
    /// somebody else chose and the modal carries notices naming a file that
    /// would not read, and one that grew to fit either ends up wider than the
    /// screen. The second figure is the *list*, not the modal — the header and
    /// the two buttons above it claim their own room first, and a scroll area
    /// given the whole height would push them off the bottom on a short window.
    pub const PALETTE_LIBRARY: [f32; 2] = [560.0, 380.0];
    /// A pill button — `crate::controls::text_button`'s height, wherever one is
    /// drawn.
    ///
    /// Here rather than at the call site because anything *reserving* room for
    /// a strip of them has to agree with what they cost: the settings dialog's
    /// footer is a hairline, a gap and one of these, and a reserve that guessed
    /// made the pane taller than the rail beside it.
    pub const TEXT_BUTTON: f32 = 22.0;
    /// Between two buttons that sit side by side.
    ///
    /// egui's own default item spacing, named — which is the point: the
    /// settings dialog butts its rail against its pane by setting the
    /// horizontal spacing to zero, and that zero is inherited all the way down
    /// into every row of every pane. Anything on such a row that wants a gap
    /// has to say so, and this is the gap it says.
    pub const BUTTON_GAP: f32 = 6.0;
    /// The stamps-and-papers browser: how wide the modal is, and how tall its
    /// list is.
    ///
    /// Fixed, for the reason [`BRUSH_LIBRARY`] is, plus one of its own: an
    /// import here says which reading it took and whether the tile joins to
    /// itself, which is the longest sentence in the interface after a Clip
    /// Studio import's. Narrower than the brush browser because the rows carry
    /// a picture and a name rather than a picture, a name, a credit line and
    /// two controls. The second figure is the *list*, as the palette library's
    /// is: the header, the pair of tabs and the footer claim their room first.
    pub const STAMP_LIBRARY: [f32; 2] = [520.0, 360.0];
    /// The square one stamp or paper is previewed in, in the browser's rows.
    pub const STAMP_PREVIEW: f32 = 44.0;
    /// One row of that browser, gap excluded.
    ///
    /// Stated rather than measured, because the list uses `show_rows` and has
    /// to know the height before it lays a row out — which is the whole point:
    /// a row that is laid out box-averages a picture of up to four million
    /// texels and uploads a texture. [`STAMP_PREVIEW`] plus the row frame's own
    /// six points of padding either side; the two lines of text beside the
    /// square are shorter than it and neither wraps.
    pub const STAMP_ROW: f32 = STAMP_PREVIEW + 12.0;
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

    /// The four accents are four adjacent circles in the settings pane, so two
    /// of them the same colour is a picker offering a choice that is not one.
    ///
    /// Nothing measured this before there were six themes, and nothing needed
    /// to: the four swatches are the design's own and are told apart by
    /// construction. What changed is that [`Accent::Umber`] now answers with
    /// *the theme's own* accent, so each preset theme puts a fifth colour into
    /// that comparison — and Krita's measured slate blue landed 35 from
    /// `Accent::Steel`, against a worst of 64 anywhere else. Both drawn as
    /// 18-point circles side by side, one of them labelled "Umber".
    ///
    /// The bound is the link test's metric and its accent figure, and 64 —
    /// Graphite's own Umber against its Clay — is the bar this cannot be
    /// tightened past without moving the design's swatches.
    #[test]
    fn no_two_accents_look_alike_in_one_theme() {
        let apart = |a: Color32, b: Color32| {
            let d = |x: u8, y: u8| u32::from(x.abs_diff(y));
            d(a.r(), b.r()) + d(a.g(), b.g()) + d(a.b(), b.b())
        };
        for kind in ThemeKind::ALL {
            let inks: Vec<Color32> = Accent::ALL
                .into_iter()
                .map(|a| Palette::with_accent(kind, a).accent)
                .collect();
            for (i, a) in inks.iter().enumerate() {
                for (j, b) in inks.iter().enumerate().skip(i + 1) {
                    let d = apart(*a, *b);
                    assert!(
                        d >= 60,
                        "{kind:?}: {:?} and {:?} are only {d} apart",
                        Accent::ALL[i],
                        Accent::ALL[j],
                    );
                }
            }
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
    ///
    /// Over **every** theme and not the shipped pair, and that is what the
    /// recessive-surface `match` in `with_accent` exists for: Krita's canvas
    /// pit is lighter than its panels, so a dim mixed towards `backdrop`
    /// — which is what this did before there were six themes — comes out
    /// *brighter* than the accent for all four alternates. It also covers the
    /// authored pairs, since `Accent::Umber` returns them untouched.
    #[test]
    fn a_derived_dim_recedes_towards_its_own_surface() {
        let luma = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        for kind in ThemeKind::ALL {
            for accent in Accent::ALL {
                let p = Palette::with_accent(kind, accent);
                if kind.is_dark() {
                    assert!(
                        luma(p.accent_dim) < luma(p.accent),
                        "{accent:?} dim is not darker on {kind:?}",
                    );
                } else {
                    assert!(
                        luma(p.accent_dim) > luma(p.accent),
                        "{accent:?} dim is not lighter on {kind:?}",
                    );
                }
            }
        }
    }

    /// What a file calls each theme, as literal text.
    ///
    /// These reach two files — the preferences file's `theme` line and a
    /// `.umbertheme`'s `base` line — so they are a *format* and not a name. A
    /// round trip is self-consistent under any rename, which is why the pin has
    /// to be the strings themselves; the same remedy `BlendMode`'s serialised
    /// names take, for the same mechanism.
    #[test]
    fn the_stored_name_of_every_theme_is_this_exact_string() {
        let stored: Vec<&str> = ThemeKind::ALL.into_iter().map(ThemeKind::id).collect();
        assert_eq!(
            stored,
            [
                "graphite",
                "paper",
                "photoslop",
                "shitstudio",
                "krita",
                "mediabog"
            ],
        );
        for kind in ThemeKind::ALL {
            assert_eq!(ThemeKind::from_id(kind.id()), Some(kind));
        }
        assert_eq!(ThemeKind::from_id("no such theme"), None);
        // A label is free to be reworded and an id is not, so nothing may be
        // reading one for the other.
        for kind in ThemeKind::ALL {
            assert_ne!(kind.id(), kind.label(), "{kind:?}");
        }
    }

    /// The two readings of "is this theme dark" — the stated one on
    /// [`ThemeKind`] and the measured one on the palette, which is what egui's
    /// own light/dark styling follows — must agree, or a theme would derive its
    /// accents as a dark theme and be styled as a light one.
    #[test]
    fn every_theme_agrees_with_itself_about_being_dark() {
        for kind in ThemeKind::ALL {
            assert_eq!(
                kind.is_dark(),
                Palette::of(kind).is_dark(),
                "{kind:?} disagrees with its own palette",
            );
        }
    }

    /// A theme nobody can read is worse than no theme.
    ///
    /// WCAG relative luminance, which is the only reading of "can this be
    /// read" that is not somebody's opinion — the crude channel sum the link
    /// marks are measured by answers a different question and would pass a
    /// theme printing mid-grey on mid-grey.
    ///
    /// It runs over all six rather than the four added, deliberately: a bound
    /// the shipped pair does not meet is a bound stated wrongly, and finding
    /// that out is worth more than a guard that only ever looks at new code.
    /// **It found one.** The floors were 4.5 / 4.5 / 3.0 / 3.0 and Paper's
    /// `text_dim` on its own `window` is **2.92** — the dimmest of the four
    /// panel surfaces below, shipped and looked at by somebody. So the dim
    /// floor is what Paper actually reaches rather than the round number, and
    /// the four themes added here clear even 3.0: nothing new may be dimmer
    /// than the dimmest thing already on screen. (Paper is dimmer still on
    /// `control` — 2.79 — and that pair is deliberately not below, because
    /// nothing draws `text_dim` on a button.)
    ///
    /// `control_active` is here too, and is the one token that is a background
    /// rather than a mark — a selected row draws `text_strong` on it. That is
    /// what caught MediaBog's selection blue at 2.85.
    #[test]
    fn text_reads_against_every_surface_it_is_drawn_on() {
        // sRGB byte to linear, WCAG's own piecewise curve.
        fn channel(b: u8) -> f64 {
            let c = b as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(c: Color32) -> f64 {
            0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
        }
        fn ratio(a: Color32, b: Color32) -> f64 {
            let (x, y) = (luminance(a), luminance(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        }

        for kind in ThemeKind::ALL {
            let p = Palette::of(kind);
            for (surface, where_) in [
                (p.chrome, "chrome"),
                (p.dock, "dock"),
                (p.window, "window"),
                (p.popover, "popover"),
            ] {
                for (ink, name, floor) in [
                    (p.text_strong, "text_strong", 4.5),
                    (p.text, "text", 4.5),
                    (p.text_muted, "text_muted", 3.0),
                    // Paper's own figure — see above.
                    (p.text_dim, "text_dim", 2.9),
                ] {
                    let r = ratio(ink, surface);
                    assert!(
                        r >= floor,
                        "{kind:?}: {name} on {where_} is {r:.2}:1, under {floor}:1",
                    );
                }
            }
            // The two control fills are surfaces as well, and they take the
            // inks `style_from` actually pairs them with rather than the whole
            // product above: `widgets.inactive.fg_stroke` is `text_muted` over
            // `control`, and hovered, active and open are all `text_strong`
            // over `control_hover`. Nothing anywhere draws `text_dim` on a
            // button, and a floor set by a pair that does not exist is a floor
            // set by nothing — this domain was a cross-product first, and
            // `text_dim` on `control` would have failed at 2.36 for a reading
            // no artist can ever take.
            for (ink, ink_name, surface, where_, floor) in [
                (p.text_muted, "text_muted", p.control, "control", 3.0),
                (p.text_strong, "text_strong", p.control, "control", 4.5),
                (
                    p.text_strong,
                    "text_strong",
                    p.control_hover,
                    "control_hover",
                    4.5,
                ),
                // `controls::text_button` fills with `control` and inks its
                // label in `text`, or in `accent` when it is the emphasised
                // one — which is the primary button of every dialog footer.
                // Reading `style_from` alone missed both: this interface's real
                // controls are *painted* in `controls.rs` and `widgets.rs`, so
                // the tokens egui is handed are only half the domain.
                (p.text, "text", p.control, "control", 4.5),
                (p.accent, "accent", p.control, "control", 3.0),
                // Three marks are drawn in `text_dim` over the *canvas pit* and
                // nowhere else: `widgets`'s canvas scrollbar thumb, `ui`'s pen
                // dot, and `splash`'s status line. `widgets.rs` states the bar
                // it rejected when it chose that ink — `rail`, at 1.31:1 — and
                // 2.6 is what the palette it was written against actually
                // reaches, which is Paper's 2.61. Krita's own 50% grey pit put
                // the thumb at 1.34, under the number that argument threw out.
                (p.text_dim, "text_dim", p.backdrop, "backdrop", 2.6),
            ] {
                let r = ratio(ink, surface);
                assert!(
                    r >= floor,
                    "{kind:?}: {ink_name} on {where_} is {r:.2}:1, under {floor}:1",
                );
            }

            // The accent is ink too — it is what a hyperlink is drawn in.
            let r = ratio(p.accent, p.chrome);
            assert!(r >= 3.0, "{kind:?}: the accent on chrome is {r:.2}:1");
            // And a selected row has to be readable, which is the one place
            // `control_active` is a background rather than a mark.
            let r = ratio(p.text_strong, p.control_active);
            assert!(
                r >= 3.0,
                "{kind:?}: text_strong on control_active is {r:.2}:1",
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

    /// A picture of every theme, so a palette is judged by looking at it.
    ///
    /// Written rather than asserted, for the reason `themes_pane_preview` is:
    /// the tests above measure separation and contrast, which are the two
    /// things arithmetic can settle, and neither of them can say whether a
    /// theme looks like the application it was sampled from. The Settings
    /// dialog is the subject because it puts most of the tokens on one screen —
    /// every surface, both borders, `control` and `control_hover`, the four
    /// text ranks, the accent, a rail and a knob — and because its own Themes
    /// pane draws each theme as a card, so one shot per theme is also six
    /// shots of all six.
    ///
    /// **`control_active` is the one token these shots do not show**, and it
    /// is worth knowing before a palette is judged from them: this pane's own
    /// selected rail row is `control_hover`, and the only thing on the Settings
    /// dialog that fills with `control_active` is the Shortcuts page's armed
    /// chord row. Scanning a shot for the exact byte triple finds none of it.
    /// It is a *selected row* colour — the layer list, the brush list, the tool
    /// grid, a segmented picker — so `panels::tests::layers_panel_preview` is
    /// where it can be looked at, and that one draws in Graphite. Anybody
    /// authoring a theme whose selection colour is the point of it has to
    /// render a panel to see it.
    ///
    /// ```sh
    /// cargo test -p umber-app every_theme_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn every_theme_preview() {
        use crate::docshot;
        use crate::editor::Editor;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/themes");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        for kind in ThemeKind::ALL {
            let mut ed = Editor::default();
            ed.layout = crate::dock::Layout::default();
            ed.ui.settings_open = true;
            ed.ui.settings_tab = crate::settings::SettingsTab::Themes;
            ed.ui.theme = kind;
            let palette = ed.palette();
            let image = stage.shoot(
                egui::vec2(1048.0, 688.0),
                1.5,
                &palette,
                palette.backdrop,
                |ui| {
                    crate::settings::show(
                        ui,
                        &palette,
                        &mut ed,
                        &mut crate::ui::UiActions::default(),
                    )
                },
            );
            let written =
                docshot::write_png(&dir.join(format!("{}.png", kind.id())), &image).expect("write");
            println!("{}", written.0.display());
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
