//! What a canvas size *is*: the shapes on offer, the sizes under each shape,
//! the paper sizes, and the arithmetic that turns a sheet of paper and a
//! resolution into pixels.
//!
//! Here rather than in the dialog for the reason [`CanvasCopy::plan`] and
//! `Clip::place` are here: "how big is A4 at 300 dpi" is a *rule*, and a rule
//! is testable without a window. `canvasdlg.rs` draws what this module says and
//! decides none of it. The same tables serve the New document dialog and Canvas
//! settings, which is what stops New offering a size Canvas settings cannot
//! express.
//!
//! # A paper size is a physical size, so the resolution is half of it
//!
//! "A4" names 210 × 297 millimetres and says nothing about pixels. The pixels
//! follow from the resolution, and the resolution therefore has to travel with
//! the choice and reach [`Document::dpi`] — a canvas of 2480 × 3508 recorded at
//! 72 dpi is not A4, it is a 875 × 1238 millimetre poster. That is why
//! [`Sheet::pixels`] takes a `dpi` and why the dialog re-derives the size
//! whenever the resolution moves.
//!
//! # The rounding rule, stated
//!
//! Pixels are `round(inches × dpi)`, half away from zero, computed in `f64`.
//! Every sheet is kept in the units it is *defined* in — the ISO sizes in whole
//! millimetres, the US ones in inches — because converting Letter to
//! millimetres and back is a needless trip through a rounding this rule then
//! has to survive.
//!
//! Three things make that more than a coin toss.
//!
//! At 72 dpi the rule reproduces the PostScript page sizes exactly — A4 is
//! 595 × 842, Letter is 612 × 792 — which is an independent authority, fixed
//! decades before this file, agreeing with the arithmetic.
//!
//! At every resolution the quick-pick offers, nothing in the tables lands
//! anywhere near a half-pixel tie, so `f64`'s last bit decides nothing:
//! `no_offered_sheet_is_decided_by_a_rounding_tie` measures the margin rather
//! than assuming it.
//!
//! **Ties are reachable, and the rule is stated for them too.** A typed
//! resolution can be odd, and the US sheets are 8.5 inches wide, so 8.5 × 3 is
//! exactly 25.5 — a genuine tie, decided *up*, because that is what half away
//! from zero means. `a_genuine_tie_rounds_away_from_zero` pins it, and it is
//! pinned because the alternative is somebody later "tidying" the rounding into
//! banker's and moving a canvas by a pixel with no test to notice.
//!
//! [`CanvasCopy::plan`]: crate::document::CanvasCopy::plan

use std::ops::RangeInclusive;

use glam::UVec2;

use crate::document::Document;

/// The resolutions worth one click, each with the text its button carries.
///
/// 72 is the screen figure and Umber's own "unstated"; 300 is what a printer
/// asks for and is what [`Chosen::Sheet`] carries; 150 is the draft between
/// them and 600 is fine art. Anything else is typed into the resolution field
/// beside these, which is why this is a short list of the ones people actually
/// pick rather than an attempt at every one.
///
/// The label is in the table rather than formatted at the call site because the
/// quick-pick is drawn on every frame a modal is open, and "nothing on the
/// drawing path allocates per frame" is a rule of the house. A constant needs no
/// `String`. `every_resolution_is_labelled_with_itself` is what stops the two
/// halves of a row drifting apart.
pub const DPI_CHOICES: [(u32, &str); 4] = [(72, "72"), (150, "150"), (300, "300"), (600, "600")];

/// The resolution a paper size arrives at.
///
/// A sheet at 72 dpi is a legal canvas and almost never what somebody reaching
/// for "A4" wants, and 72 is the figure a document carries when nothing has
/// stated one — so choosing paper states one.
pub const PAPER_DPI: u32 = 300;

// ---------------------------------------------------------------- orientation

/// Which way up a sheet of paper is.
///
/// Only paper has one. The screen shapes name their own orientation — 16:9 and
/// 9:16 are two entries rather than one entry and a toggle — because that is
/// how anybody says them, where nobody says "A4 landscape" as a different
/// paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Orientation {
    /// Taller than it is wide. Every sheet in [`Sheet::ALL`] is defined this
    /// way up, which is what makes this the default.
    #[default]
    Portrait,
    Landscape,
}

impl Orientation {
    pub const ALL: [Orientation; 2] = [Self::Portrait, Self::Landscape];

    pub fn label(self) -> &'static str {
        match self {
            Self::Portrait => "Portrait",
            Self::Landscape => "Landscape",
        }
    }

    /// `portrait` turned this way up.
    fn apply(self, portrait: UVec2) -> UVec2 {
        match self {
            Self::Portrait => portrait,
            Self::Landscape => UVec2::new(portrait.y, portrait.x),
        }
    }
}

// ---------------------------------------------------------------------- paper

/// A sheet's physical size, in the units it is defined in.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SheetSize {
    /// Portrait width and height in millimetres. The ISO sizes are whole
    /// millimetres by definition.
    Millimetres(f64, f64),
    /// Portrait width and height in inches, which is how the US sizes are
    /// stated and the only form in which Letter is exactly 8.5 wide.
    Inches(f64, f64),
}

impl SheetSize {
    /// Portrait width and height in inches, which is the one unit the pixel
    /// arithmetic needs.
    fn inches(self) -> (f64, f64) {
        match self {
            Self::Millimetres(w, h) => (w / 25.4, h / 25.4),
            Self::Inches(w, h) => (w, h),
        }
    }
}

/// A standard sheet of paper.
///
/// An enum rather than a table of structs so [`Sheet::size`] is an exhaustive
/// match: a sheet cannot be added without somebody stating how big it is, and
/// `every_sheet_is_in_all` turns a short [`Sheet::ALL`] into a failure rather
/// than a row nobody notices is missing.
///
/// Seven rather than the whole of ISO 216: A2 and larger are 30 000 pixels on a
/// side at 600 dpi, past anything Umber can allocate, and A7 and smaller are a
/// postage stamp. What is here is the range somebody paints on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    A3,
    A4,
    A5,
    A6,
    Letter,
    Legal,
    Tabloid,
}

impl Sheet {
    /// Largest first, which is the order the row draws in and the order the
    /// sizes read in.
    pub const ALL: [Sheet; 7] = [
        Self::A3,
        Self::A4,
        Self::A5,
        Self::A6,
        Self::Letter,
        Self::Legal,
        Self::Tabloid,
    ];

    /// The sheet [`Chosen::Sheet`] hands over when "Paper" is picked.
    ///
    /// A4 rather than Letter because it is the paper of most of the world, and
    /// Umber has no notion of where it is running. The row of the other six is
    /// immediately beside it.
    pub const DEFAULT: Sheet = Self::A4;

    pub fn label(self) -> &'static str {
        match self {
            Self::A3 => "A3",
            Self::A4 => "A4",
            Self::A5 => "A5",
            Self::A6 => "A6",
            Self::Letter => "Letter",
            Self::Legal => "Legal",
            Self::Tabloid => "Tabloid",
        }
    }

    /// The physical size, portrait, in the units the sheet is defined in.
    fn size(self) -> SheetSize {
        match self {
            Self::A3 => SheetSize::Millimetres(297.0, 420.0),
            Self::A4 => SheetSize::Millimetres(210.0, 297.0),
            Self::A5 => SheetSize::Millimetres(148.0, 210.0),
            Self::A6 => SheetSize::Millimetres(105.0, 148.0),
            Self::Letter => SheetSize::Inches(8.5, 11.0),
            Self::Legal => SheetSize::Inches(8.5, 14.0),
            Self::Tabloid => SheetSize::Inches(11.0, 17.0),
        }
    }

    /// Pixels this sheet covers at `dpi`, turned `orientation`.
    ///
    /// `round(inches × dpi)`, in `f64`, and never fewer than one pixel on a
    /// side. See the module docs for why that rounding is the rule and what
    /// says it is the right one.
    ///
    /// **Deliberately not clamped at the top.** The caller holds a
    /// [`CanvasLimit`] and a sheet too large for it has to be *refused*, which
    /// means this has to be able to report a size that will not fit. Clamping
    /// here was the first draft and it is worse than wrong: A3 at 1402 dpi came
    /// back as 16384 × 16384, so an A3 button sat lit over a perfect square,
    /// [`read`] filed it as 1:1, and [`Sheet::max_dpi`] answered that it fitted.
    /// One clamp, three lies.
    ///
    /// `dpi` is held to the range a [`Document`] can carry, which is also what
    /// keeps the multiplication inside a `u32`: the largest sheet at
    /// [`Document::MAX_DPI`] is under 40 000 pixels.
    pub fn pixels(self, dpi: u32, orientation: Orientation) -> UVec2 {
        let (w, h) = self.size().inches();
        let dpi = f64::from(dpi.clamp(Document::MIN_DPI as u32, Document::MAX_DPI as u32));
        orientation.apply(UVec2::new(scale(w, dpi), scale(h, dpi)))
    }

    /// The highest resolution at which this sheet still fits `limit`.
    ///
    /// The resolution field is bounded by this while a sheet is in hand, so
    /// asking for A3 at 2400 dpi is unreachable rather than refused after the
    /// fact.
    ///
    /// **It is clamped at [`Document::MIN_DPI`] and no caller filters on it**,
    /// so on a device too small to hold the sheet at *any* resolution this
    /// answers 1 and the caller acts on it. That is unreachable rather than
    /// guarded: `downlevel_defaults` floors `max_texture_dimension_2d` at 2048,
    /// where A3 at one dot per inch is 12 × 17. What saves it if it ever is
    /// reachable is [`CanvasLimit::clamp`] at the far end of the dialog, not
    /// anything here — an earlier draft of this sentence claimed such a sheet
    /// "is not offered at all", and nothing anywhere declines to offer one.
    pub fn max_dpi(self, limit: CanvasLimit) -> u32 {
        let (_, long) = self.size().inches();
        // `round(long × dpi) <= max` iff `long × dpi < max + 0.5`, so this is
        // the answer directly. The loop is one step of insurance against the
        // division landing a hair over the boundary, and cannot run away: it
        // only ever steps down, and one is always a legal answer.
        let floor = Document::MIN_DPI as u32;
        let ceiling = Document::MAX_DPI as u32;
        let mut dpi =
            (((f64::from(limit.max_edge()) + 0.5) / long).floor() as u32).clamp(floor, ceiling);
        while dpi > floor && !limit.permits(self.pixels(dpi, Orientation::Portrait)) {
            dpi -= 1;
        }
        dpi
    }
}

/// One length in inches at `dpi`, as a canvas edge.
///
/// `round` is half away from zero, which is the rule the module docs state.
/// Never zero — a canvas with no pixels in it is a validation error rather than
/// a small picture — and never clamped at the top, for the reason
/// [`Sheet::pixels`] gives.
fn scale(inches: f64, dpi: f64) -> u32 {
    let px = (inches * dpi).round();
    if px.is_finite() {
        px.max(1.0) as u32
    } else {
        1
    }
}

// --------------------------------------------------------------------- shapes

/// A named pixel size under one fixed-ratio shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizePreset {
    /// What the button says.
    ///
    /// A familiar name where the size has one ("4K"), and otherwise the
    /// **long edge** — which is what makes 4:3's row and 3:4's row read as the
    /// same four sizes seen two ways rather than eight unrelated numbers. The
    /// pair of numbers is on the fields directly below, so the label never has
    /// to carry it.
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
}

impl SizePreset {
    pub fn size(self) -> UVec2 {
        UVec2::new(self.width, self.height)
    }
}

/// Square. Round numbers rather than powers of two: 2048 is a texture size and
/// 2000 is a canvas size, and nothing downstream of here cares which.
const SQUARE: &[SizePreset] = &[
    SizePreset {
        label: "1000",
        width: 1000,
        height: 1000,
    },
    SizePreset {
        label: "2000",
        width: 2000,
        height: 2000,
    },
    SizePreset {
        label: "5000",
        width: 5000,
        height: 5000,
    },
    SizePreset {
        label: "8000",
        width: 8000,
        height: 8000,
    },
    SizePreset {
        label: "12000",
        width: 12000,
        height: 12000,
    },
    SizePreset {
        label: "16384",
        width: 16384,
        height: 16384,
    },
];

/// The video resolutions, which is the whole of what 16:9 means to anybody.
const WIDE: &[SizePreset] = &[
    SizePreset {
        label: "1080p",
        width: 1920,
        height: 1080,
    },
    SizePreset {
        label: "1440p",
        width: 2560,
        height: 1440,
    },
    SizePreset {
        label: "4K",
        width: 3840,
        height: 2160,
    },
    SizePreset {
        label: "8K",
        width: 7680,
        height: 4320,
    },
];

/// The same four turned on their side. Same labels: they are the same sizes.
const WIDE_TALL: &[SizePreset] = &[
    SizePreset {
        label: "1080p",
        width: 1080,
        height: 1920,
    },
    SizePreset {
        label: "1440p",
        width: 1440,
        height: 2560,
    },
    SizePreset {
        label: "4K",
        width: 2160,
        height: 3840,
    },
    SizePreset {
        label: "8K",
        width: 4320,
        height: 7680,
    },
];

/// 4:3 is the display ratio it has always been — VGA through QXGA, and every
/// iPad — so the sizes are the display sizes, doubling from 1024. There is no
/// colloquial name for any of them the way "4K" names 3840 × 2160, which is why
/// these carry their long edge as the label instead.
const STANDARD: &[SizePreset] = &[
    SizePreset {
        label: "1024",
        width: 1024,
        height: 768,
    },
    SizePreset {
        label: "1600",
        width: 1600,
        height: 1200,
    },
    SizePreset {
        label: "2048",
        width: 2048,
        height: 1536,
    },
    SizePreset {
        label: "4096",
        width: 4096,
        height: 3072,
    },
];

const STANDARD_TALL: &[SizePreset] = &[
    SizePreset {
        label: "1024",
        width: 768,
        height: 1024,
    },
    SizePreset {
        label: "1600",
        width: 1200,
        height: 1600,
    },
    SizePreset {
        label: "2048",
        width: 1536,
        height: 2048,
    },
    SizePreset {
        label: "4096",
        width: 3072,
        height: 4096,
    },
];

/// Which set of sizes is in front.
///
/// The first control of the canvas dialogs, because "what shape" is the
/// question somebody answers before "how many pixels" — and because it is what
/// makes a row of six sizes six sizes rather than thirty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aspect {
    Square,
    /// 4:3, landscape.
    Standard,
    /// 3:4, portrait.
    StandardTall,
    /// 16:9, landscape.
    Wide,
    /// 9:16, portrait.
    WideTall,
    /// Paper, whose sizes are physical and therefore depend on the resolution.
    Paper,
    /// Anything else. The one entry that claims nothing about the size.
    Custom,
}

impl Aspect {
    pub const ALL: [Aspect; 7] = [
        Self::Square,
        Self::Standard,
        Self::StandardTall,
        Self::Wide,
        Self::WideTall,
        Self::Paper,
        Self::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Square => "1:1",
            Self::Standard => "4:3",
            Self::StandardTall => "3:4",
            Self::Wide => "16:9",
            Self::WideTall => "9:16",
            Self::Paper => "Paper",
            Self::Custom => "Custom",
        }
    }

    /// Width over height as exact whole numbers, for the shapes that are a
    /// ratio.
    ///
    /// `None` for Paper, whose seven sheets are seven ratios, and for Custom,
    /// which is the absence of one. Exact integers rather than a float because
    /// every use of this is a comparison or a division that must not drift.
    pub fn ratio(self) -> Option<(u32, u32)> {
        match self {
            Self::Square => Some((1, 1)),
            Self::Standard => Some((4, 3)),
            Self::StandardTall => Some((3, 4)),
            Self::Wide => Some((16, 9)),
            Self::WideTall => Some((9, 16)),
            Self::Paper | Self::Custom => None,
        }
    }

    /// The fixed pixel sizes under this shape.
    ///
    /// Empty for Paper, whose sizes are [`Sheet::pixels`]' and follow the
    /// resolution, and for Custom, which offers none by definition.
    pub fn presets(self) -> &'static [SizePreset] {
        match self {
            Self::Square => SQUARE,
            Self::Standard => STANDARD,
            Self::StandardTall => STANDARD_TALL,
            Self::Wide => WIDE,
            Self::WideTall => WIDE_TALL,
            Self::Paper | Self::Custom => &[],
        }
    }

    /// Whether this shape could be the one a canvas of `size` is in.
    ///
    /// What it is *for* is keeping the strip from lying while an edge is being
    /// nudged: see [`settle`]. Custom holds everything, which is what makes it
    /// sticky — somebody typing numbers is not asking to be filed.
    ///
    /// **A ratio holds the nearest whole pixel to itself, not only an exact
    /// multiple**, and that is the same arithmetic [`choose`] uses rather than a
    /// tolerance invented here. Exact cross-multiplication was the first draft
    /// and it is wrong twice. A canvas 1601 pixels wide *cannot* be exactly
    /// 16:9, so dragging the width across a few pixels made the whole row of
    /// sizes appear and vanish once per pixel; and `choose` itself produces
    /// sizes that are only nearest — 5000 square becomes 5000 × 2813 — so
    /// picking 16:9 gave a canvas that the same module then said was not 16:9.
    /// `a_size_a_choice_produces_is_one_that_shape_holds` pins the round trip.
    pub fn holds(self, size: UVec2, dpi: u32) -> bool {
        match self {
            Self::Custom => true,
            Self::Paper => Sheet::ALL.iter().any(|sheet| {
                Orientation::ALL
                    .iter()
                    .any(|&up| sheet.pixels(dpi, up) == size)
            }),
            // Either edge derived from the other, which is both directions
            // this module can produce a size in: [`choose`] drives from the
            // long edge and [`LockedShape`] drives from whichever one was
            // typed. Stating it symmetrically is what makes the round trip
            // structural rather than a coincidence of rounding — and it still
            // rejects the transpose, because deriving 1920 from 1080 at 16:9
            // gives 608 and deriving it the other way gives 3413.
            _ => match self.ratio() {
                Some((w, h)) => {
                    u64::from(size.y) == scaled(size.x, w, h)
                        || u64::from(size.x) == scaled(size.y, h, w)
                }
                None => false,
            },
        }
    }
}

/// What a canvas size reads as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub aspect: Aspect,
    /// Which entry of `aspect.presets()` the size is, when it is one exactly.
    pub preset: Option<usize>,
    /// Which sheet it is. Only ever `Some` when the aspect is [`Aspect::Paper`].
    pub sheet: Option<Sheet>,
    /// Which way up that sheet is. `Some` under exactly the same condition, so
    /// a shape that has no orientation cannot silently overwrite the one the
    /// paper row is holding.
    pub orientation: Option<Orientation>,
}

impl Reading {
    /// The reading a size gets when the shape is already decided.
    fn under(aspect: Aspect, size: UVec2, dpi: u32) -> Self {
        let (sheet, orientation) = if aspect == Aspect::Paper {
            match sheet_at(size, dpi) {
                Some((sheet, up)) => (Some(sheet), Some(up)),
                None => (None, None),
            }
        } else {
            (None, None)
        };
        Self {
            aspect,
            preset: aspect.presets().iter().position(|p| p.size() == size),
            sheet,
            orientation,
        }
    }
}

/// Which sheet, if any, `size` is at `dpi`.
fn sheet_at(size: UVec2, dpi: u32) -> Option<(Sheet, Orientation)> {
    Sheet::ALL.iter().find_map(|&sheet| {
        Orientation::ALL
            .iter()
            .find(|&&up| sheet.pixels(dpi, up) == size)
            .map(|&up| (sheet, up))
    })
}

/// Which shape a canvas of `size` at `dpi` belongs to.
///
/// A fixed ratio wins over paper, and that ordering is deliberate rather than
/// incidental: a canvas that is exactly 16:9 *is* 16:9 whatever sheet it may
/// also coincide with, and a rule with an order is one the reader can predict.
/// Nothing in the tables collides at the resolutions on offer, which
/// `no_sheet_reads_as_a_screen_shape` measures.
pub fn read(size: UVec2, dpi: u32) -> Reading {
    for aspect in Aspect::ALL {
        // Driven off `ratio()`, which is an exhaustive match, rather than off a
        // list of the two shapes that have none. A shape added later cannot then
        // be run through the fixed-ratio branch by default; it has to state
        // whether it is a ratio, which is a compile error until somebody does.
        if aspect.ratio().is_none() {
            continue;
        }
        if aspect.holds(size, dpi) {
            return Reading::under(aspect, size, dpi);
        }
    }
    match sheet_at(size, dpi) {
        Some((sheet, up)) => Reading {
            aspect: Aspect::Paper,
            preset: None,
            sheet: Some(sheet),
            orientation: Some(up),
        },
        None => Reading::under(Aspect::Custom, size, dpi),
    }
}

/// Where a hand-typed size leaves the strip.
///
/// `current` stays put while it still holds the size, and only then is the size
/// read afresh. Both halves matter. Staying is what keeps Custom sticky and
/// what stops a locked 16:9 flickering through Custom as an edge is nudged;
/// re-reading is what stops the strip claiming 16:9 over a canvas that has
/// stopped being it.
pub fn settle(current: Aspect, size: UVec2, dpi: u32) -> Reading {
    if current.holds(size, dpi) {
        Reading::under(current, size, dpi)
    } else {
        read(size, dpi)
    }
}

/// What picking a shape does to the canvas in front of you.
///
/// An enum rather than a size, because the three answers are genuinely
/// different and an exhaustive match is what stops a fourth shape being added
/// without somebody deciding what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chosen {
    /// Leave the canvas exactly as it is. Custom alone: it is the escape hatch,
    /// so reaching for it must not move anything.
    Unchanged,
    /// This pixel size.
    Size(UVec2),
    /// This sheet, at this resolution.
    Sheet { sheet: Sheet, dpi: u32 },
}

/// What picking `aspect` does to a canvas that is currently `size`.
///
/// The rule for a fixed ratio is that **the longer edge is kept and becomes the
/// new shape's longer edge**. One sentence, and it gives the two things a hand
/// expects for free: the scale is preserved, so 12000 square becomes
/// 12000 × 6750 rather than jumping to a video size; and switching between a
/// shape and its transpose is exactly a transpose, so 16:9's 1920 × 1080
/// becomes 9:16's 1080 × 1920 rather than a 1920-wide canvas nobody asked for.
///
/// Paper hands over [`Sheet::DEFAULT`] at [`PAPER_DPI`] rather than converting
/// anything, because a sheet is a physical size and there is no honest way to
/// read one off a pixel count.
///
/// Everything is bounded by `limit` here rather than by the caller: clamping
/// the long edge first is enough, since the derived edge is never the longer of
/// the two.
pub fn choose(aspect: Aspect, size: UVec2, limit: CanvasLimit) -> Chosen {
    match aspect {
        Aspect::Custom => Chosen::Unchanged,
        Aspect::Paper => Chosen::Sheet {
            sheet: Sheet::DEFAULT,
            dpi: PAPER_DPI,
        },
        _ => {
            let (w, h) = match aspect.ratio() {
                Some(ratio) => ratio,
                // Unreachable: every arm left here has a ratio. Answering
                // "unchanged" rather than panicking, because a dialog that
                // does nothing is a far better failure than one that takes the
                // application down.
                None => return Chosen::Unchanged,
            };
            let long = size.x.max(size.y).clamp(1, limit.max_edge());
            Chosen::Size(shaped(long, w, h))
        }
    }
}

/// The `w:h` canvas whose longer edge is `long`.
///
/// The one statement of the shape, shared by [`choose`] and [`Aspect::holds`],
/// so "what a choice produces" and "what a shape recognises" cannot drift apart
/// — which they had, when `holds` cross-multiplied exactly and `choose` rounded.
fn shaped(long: u32, w: u32, h: u32) -> UVec2 {
    let short = derive(long, w.min(h), w.max(h));
    if w >= h {
        UVec2::new(long, short)
    } else {
        UVec2::new(short, long)
    }
}

/// `value × to / from`, rounded half up, in integers.
///
/// **The one piece of ratio arithmetic in Umber.** [`shaped`], [`Aspect::holds`]
/// and [`LockedShape`] all go through it, which is what stops the strip's
/// honesty resting on two roundings agreeing: the lock used to derive its edge
/// in `f32` in the dialog while `holds` judged the answer with this, and they
/// agreed everywhere only by the accident that `1608 / (16/9)` in `f32` lands
/// exactly on `904.5`.
///
/// Integer rather than float so there is no question about what the exact
/// halves do: 5000 at 16:9 is 2812.5, and a rounding mode is one more thing
/// that could differ between two builds of the same number.
fn scaled(value: u32, from: u32, to: u32) -> u64 {
    let from = u64::from(from.max(1));
    (u64::from(value) * u64::from(to) + from / 2) / from
}

/// `long × short_part / long_part`, never past `long` and never zero.
fn derive(long: u32, short_part: u32, long_part: u32) -> u32 {
    scaled(long, long_part, short_part).clamp(1, u64::from(long)) as u32
}

/// The shape a "Lock aspect ratio" holds.
///
/// A pair of whole numbers compared only as a ratio, **captured** rather than
/// recomputed from the fields each time — recomputing lets a rounded edge feed
/// back, so a locked 16:9 stops being 16:9 after a few nudges. It lives here
/// rather than in the dialog because it is the same ratio arithmetic
/// [`Aspect::holds`] judges the result with, and the two being one function is
/// what lets a nudged edge stay on the shape the strip is claiming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockedShape {
    w: u32,
    h: u32,
}

impl Default for LockedShape {
    fn default() -> Self {
        Self { w: 1, h: 1 }
    }
}

impl LockedShape {
    /// The shape a canvas of this size has.
    ///
    /// Zero on either axis reads as square: a field on its way to a number
    /// passes through nothing, and a lock is arithmetic that gets handed
    /// whatever the field holds.
    pub fn of(width: u32, height: u32) -> Self {
        if width == 0 || height == 0 {
            Self::default()
        } else {
            Self {
                w: width,
                h: height,
            }
        }
    }

    /// The **exact** shape of a fixed ratio, for the shapes that have one.
    ///
    /// Not the same as `of` on the size that ratio produced: a 5000 square
    /// becomes 5000 × 2813, whose own ratio is 1.7775 rather than 16:9's
    /// 1.7778, and a lock holding *that* drives the next nudge a pixel off the
    /// shape it is meant to be holding.
    pub fn of_aspect(aspect: Aspect) -> Option<Self> {
        aspect.ratio().map(|(w, h)| Self { w, h })
    }

    /// The height that keeps this shape at `width`, and the width that keeps it
    /// at `height`.
    ///
    /// Clamped to a canvas that can exist, at both ends: an extreme shape and a
    /// large edge would otherwise drive the other one past what the device
    /// holds, or a very tall one round it down to nothing — and a canvas with no
    /// pixels in it is a validation error rather than a small picture.
    pub fn height_for(self, width: u32, limit: CanvasLimit) -> u32 {
        limit.clamp_edge(scaled(width, self.w, self.h))
    }

    pub fn width_for(self, height: u32, limit: CanvasLimit) -> u32 {
        limit.clamp_edge(scaled(height, self.h, self.w))
    }
}

// ---------------------------------------------------------------------- bound

/// The largest canvas this machine will actually hold.
///
/// [`Document::MAX_EDGE`] is the *format's* bound, and it is the smaller of two
/// numbers that matters here: `wgpu`'s `max_texture_dimension_2d` is what
/// decides whether a texture can be created at all, and a canvas past it is a
/// validation error, which is fatal. `Limits::downlevel_defaults` guarantees
/// only 2048 and `Gpu::new`'s `using_resolution` raises exactly that limit from
/// the adapter, so this is a **reading taken from the device**, injected the way
/// `install::detect`'s `Probe` is, and never a constant.
///
/// A preset past it is not drawn at all and [`Self::notice`] says why. Refusing
/// on Apply would be worse in both directions: it puts a dialog in front of
/// somebody who has already decided, and it leaves a control on screen that
/// promises something the engine will decline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasLimit {
    max_edge: u32,
}

impl Default for CanvasLimit {
    fn default() -> Self {
        Self::UNKNOWN
    }
}

impl CanvasLimit {
    /// What to assume before a device has answered: the format's own bound.
    ///
    /// Reachable only in tests and in the frames before `resumed` has built the
    /// graphics, and the dialogs cannot be drawn in those — but a `Default` that
    /// refused everything would make every test of this module say "not
    /// offered".
    pub const UNKNOWN: Self = Self {
        max_edge: Document::MAX_EDGE,
    };

    /// The bound for a device reporting `max_texture_dimension_2d`.
    pub fn of_device(max_texture_dimension_2d: u32) -> Self {
        Self {
            max_edge: max_texture_dimension_2d.clamp(1, Document::MAX_EDGE),
        }
    }

    pub fn max_edge(self) -> u32 {
        self.max_edge
    }

    /// Every edge a size field may accept.
    pub fn edges(self) -> RangeInclusive<u32> {
        1..=self.max_edge
    }

    pub fn permits(self, size: UVec2) -> bool {
        size.x >= 1 && size.y >= 1 && size.x <= self.max_edge && size.y <= self.max_edge
    }

    /// One edge brought inside the bound, from arithmetic that may have
    /// overflowed a `u32`'s worth of pixels on the way.
    pub fn clamp_edge(self, edge: u64) -> u32 {
        edge.clamp(1, u64::from(self.max_edge)) as u32
    }

    /// `size` brought inside the bound.
    ///
    /// The dialog's fields and presets are already bounded, so this is the last
    /// line rather than the first: it makes "no canvas dialog can ask for a
    /// texture the device refuses" a property of one function instead of the
    /// union of four call sites.
    pub fn clamp(self, size: UVec2) -> UVec2 {
        UVec2::new(
            size.x.clamp(1, self.max_edge),
            size.y.clamp(1, self.max_edge),
        )
    }

    /// A sentence saying what is missing from the rows, or `None` when nothing
    /// is.
    ///
    /// Only when the device is the binding constraint. A machine that reaches
    /// [`Document::MAX_EDGE`] is offered everything and is told nothing, which
    /// is most machines.
    pub fn notice(self) -> Option<String> {
        (self.max_edge < Document::MAX_EDGE).then(|| {
            format!(
                "The graphics here hold a canvas up to {} pixels on a side, \
                 so larger sizes are not offered.",
                self.max_edge
            )
        })
    }
}

/// What one layer of a canvas this size costs in memory, where that is worth
/// saying.
///
/// A canvas is not refused for being expensive: the engine will make it, and an
/// artist who asks for 16384 square on a machine that can hold it is entitled
/// to it. What they are also entitled to is knowing that each layer is then a
/// gigabyte, before rather than after. Silent below a quarter of a gigabyte,
/// which is every ordinary canvas.
pub fn memory_note(size: UVec2) -> Option<String> {
    const QUIET: u64 = 256 << 20;
    let bytes = u64::from(size.x) * u64::from(size.y) * 4;
    (bytes >= QUIET).then(|| {
        if bytes >= 1 << 30 {
            format!(
                "Every layer at this size needs {:.1} GB of graphics memory.",
                bytes as f64 / (1u64 << 30) as f64
            )
        } else {
            format!(
                "Every layer at this size needs {} MB of graphics memory.",
                bytes >> 20
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------ paper sizes

    #[test]
    fn the_paper_sizes_are_the_pixel_counts_every_table_states() {
        // The figures a print shop would quote, at the resolution they quote
        // them at. Written out rather than derived, because a test that
        // recomputes the code's own expression can only ever agree with it.
        let at_300 = [
            (Sheet::A3, 3508, 4961),
            (Sheet::A4, 2480, 3508),
            (Sheet::A5, 1748, 2480),
            (Sheet::A6, 1240, 1748),
            (Sheet::Letter, 2550, 3300),
            (Sheet::Legal, 2550, 4200),
            (Sheet::Tabloid, 3300, 5100),
        ];
        for (sheet, w, h) in at_300 {
            assert_eq!(
                sheet.pixels(300, Orientation::Portrait),
                UVec2::new(w, h),
                "{} at 300 dpi",
                sheet.label()
            );
        }
    }

    #[test]
    fn at_seventy_two_the_rule_reproduces_the_postscript_page_sizes() {
        // The independent authority. PostScript and PDF state a page in points,
        // which is exactly 1/72 inch, and those figures were fixed decades
        // before this file: A4 is 595 x 842 and Letter is 612 x 792. Any
        // rounding rule that disagreed with them would be wrong.
        //
        // **A6 is the one row where the authority is not unanimous**, and it is
        // named rather than quietly matched: 105 mm is 297.638 points, so
        // rounding gives 298 and Ghostscript's own table floors it to 297 —
        // while rounding every other entry, including A5's 419.53 up to 420.
        // One rule applied uniformly beats matching a table that is not
        // uniform with itself, so 298 is deliberate and is the only figure here
        // taken from this code rather than from outside it.
        let points = [
            (Sheet::A3, 842, 1191),
            (Sheet::A4, 595, 842),
            (Sheet::A5, 420, 595),
            (Sheet::A6, 298, 420),
            (Sheet::Letter, 612, 792),
            (Sheet::Legal, 612, 1008),
            (Sheet::Tabloid, 792, 1224),
        ];
        for (sheet, w, h) in points {
            assert_eq!(
                sheet.pixels(72, Orientation::Portrait),
                UVec2::new(w, h),
                "{} in points",
                sheet.label()
            );
        }
    }

    #[test]
    fn no_offered_sheet_is_decided_by_a_rounding_tie() {
        // The module docs claim the tables land nowhere near a half-pixel at the
        // resolutions on the quick-pick, which is what makes those seven sizes
        // a statement about the rule rather than about `f64`'s mood.
        let mut closest = 0.5f64;
        for sheet in Sheet::ALL {
            let (w, h) = sheet.size().inches();
            for (dpi, _) in DPI_CHOICES {
                for inches in [w, h] {
                    let exact = inches * f64::from(dpi);
                    let gap = (exact - exact.round()).abs();
                    closest = closest.min((0.5 - gap).abs());
                }
            }
        }
        // Nothing gets within a hundredth of a pixel of a tie, which is five
        // orders of magnitude clear of `f64`'s error at these magnitudes.
        assert!(closest > 1e-2, "closest approach to a tie was {closest}");
    }

    #[test]
    fn every_resolution_is_labelled_with_itself() {
        // The pair exists to keep an allocation off the drawing path, so the one
        // thing it can get wrong is a label that names a different figure from
        // the one clicking it applies.
        for (dpi, label) in DPI_CHOICES {
            assert_eq!(label, dpi.to_string(), "{dpi} is drawn as {label}");
            assert!(
                (Document::MIN_DPI as u32..=Document::MAX_DPI as u32).contains(&dpi),
                "{dpi} is outside what a document can carry"
            );
        }
        // Ascending, which is not decoration: the dialog draws the resolutions a
        // sheet can reach as a *prefix* of this table rather than a filtered
        // copy of it, so that it borrows a `const` instead of allocating on
        // every frame a modal is open. Reorder these and the strip silently
        // stops offering figures it should.
        assert!(
            DPI_CHOICES.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "the quick-pick has to be ascending"
        );
    }

    #[test]
    fn a_genuine_tie_rounds_away_from_zero() {
        // Ties *are* reachable, because the resolution field takes any whole
        // number and Letter is 8.5 inches wide: 8.5 x 3 is exactly 25.5. So the
        // rule has to be stated for them rather than asserted not to arise, or
        // somebody later swapping `round` for a banker's rounding moves a canvas
        // by a pixel with nothing to notice.
        assert_eq!(
            Sheet::Letter.pixels(3, Orientation::Portrait),
            UVec2::new(26, 33),
            "8.5 x 3 is 25.5 and must round up"
        );
        assert_eq!(
            Sheet::Legal.pixels(5, Orientation::Portrait),
            UVec2::new(43, 70),
            "8.5 x 5 is 42.5 and must round up"
        );
    }

    #[test]
    fn a_sheet_turned_on_its_side_is_the_same_two_numbers() {
        for sheet in Sheet::ALL {
            for (dpi, _) in DPI_CHOICES {
                let portrait = sheet.pixels(dpi, Orientation::Portrait);
                let landscape = sheet.pixels(dpi, Orientation::Landscape);
                assert_eq!(landscape, UVec2::new(portrait.y, portrait.x));
                assert!(portrait.y > portrait.x, "{} is not portrait", sheet.label());
            }
        }
    }

    #[test]
    fn every_sheet_is_in_all() {
        // Exhaustive over the enum rather than a walk of `ALL`, which could only
        // check what is already in it. The arms index `ALL`, so a short array is
        // an out-of-bounds panic as well as a mismatch.
        for (i, sheet) in Sheet::ALL.iter().enumerate() {
            let named = match sheet {
                Sheet::A3 => Sheet::ALL[0],
                Sheet::A4 => Sheet::ALL[1],
                Sheet::A5 => Sheet::ALL[2],
                Sheet::A6 => Sheet::ALL[3],
                Sheet::Letter => Sheet::ALL[4],
                Sheet::Legal => Sheet::ALL[5],
                Sheet::Tabloid => Sheet::ALL[6],
            };
            assert_eq!(named, *sheet, "position {i}");
        }
    }

    #[test]
    fn every_orientation_is_in_all() {
        for (i, up) in Orientation::ALL.iter().enumerate() {
            let named = match up {
                Orientation::Portrait => Orientation::ALL[0],
                Orientation::Landscape => Orientation::ALL[1],
            };
            assert_eq!(named, *up, "position {i}");
        }
    }

    #[test]
    fn every_aspect_is_in_all() {
        for (i, aspect) in Aspect::ALL.iter().enumerate() {
            let named = match aspect {
                Aspect::Square => Aspect::ALL[0],
                Aspect::Standard => Aspect::ALL[1],
                Aspect::StandardTall => Aspect::ALL[2],
                Aspect::Wide => Aspect::ALL[3],
                Aspect::WideTall => Aspect::ALL[4],
                Aspect::Paper => Aspect::ALL[5],
                Aspect::Custom => Aspect::ALL[6],
            };
            assert_eq!(named, *aspect, "position {i}");
        }
    }

    // -------------------------------------------------------------- the tables

    #[test]
    fn every_preset_is_exactly_the_ratio_it_is_filed_under() {
        for aspect in Aspect::ALL {
            let Some((w, h)) = aspect.ratio() else {
                assert!(
                    aspect.presets().is_empty(),
                    "{} offers sizes with no ratio to check them against",
                    aspect.label()
                );
                continue;
            };
            assert!(
                !aspect.presets().is_empty(),
                "{} is a ratio with nothing under it",
                aspect.label()
            );
            for preset in aspect.presets() {
                assert_eq!(
                    preset.width * h,
                    preset.height * w,
                    "{} is not {}",
                    preset.label,
                    aspect.label()
                );
            }
        }
    }

    #[test]
    fn a_transposed_shape_offers_the_same_sizes_the_other_way_up() {
        // The labels are shared deliberately: 9:16's "4K" is 16:9's "4K" turned
        // over, and two lists that had drifted would be the sort of thing
        // nobody notices until somebody counts.
        for (wide, tall) in [
            (Aspect::Wide, Aspect::WideTall),
            (Aspect::Standard, Aspect::StandardTall),
        ] {
            let a = wide.presets();
            let b = tall.presets();
            assert_eq!(
                a.len(),
                b.len(),
                "{} against {}",
                wide.label(),
                tall.label()
            );
            for (p, q) in a.iter().zip(b) {
                assert_eq!(p.label, q.label);
                assert_eq!(p.width, q.height);
                assert_eq!(p.height, q.width);
            }
        }
    }

    #[test]
    fn no_preset_asks_for_a_canvas_the_format_cannot_hold() {
        for aspect in Aspect::ALL {
            for preset in aspect.presets() {
                assert!(
                    CanvasLimit::UNKNOWN.permits(preset.size()),
                    "{} is past Document::MAX_EDGE",
                    preset.label
                );
            }
        }
    }

    #[test]
    fn no_sheet_reads_as_a_screen_shape() {
        // `read` prefers a fixed ratio over paper, so a sheet that happened to
        // land on one would file itself under the wrong row.
        //
        // Swept over every resolution both controls can reach above **4 dpi**,
        // and there is nothing. Below it there are four, all of them a sheet
        // rendered at twenty-odd pixels — A4 at 2 dpi is 17 × 23, and the
        // nearest 3:4 canvas 23 pixels tall is 17 × 23. That is not a defect in
        // the tables, it is what "the nearest whole pixel" means when the whole
        // picture is twenty pixels: `Aspect::holds` is a half-pixel bound, so it
        // widens as a fraction of the canvas as the canvas shrinks. The ordering
        // in [`read`] is what decides those, and one of them is pinned below.
        //
        // Excluded rather than the sweep being narrowed to the four figures on
        // the quick-pick, because "nothing above four dots per inch" is a far
        // stronger statement than "none of the four we happen to offer".
        for sheet in Sheet::ALL {
            for dpi in 5..=(Document::MAX_DPI as u32) {
                for up in Orientation::ALL {
                    let size = sheet.pixels(dpi, up);
                    for aspect in Aspect::ALL {
                        if aspect == Aspect::Paper || aspect == Aspect::Custom {
                            continue;
                        }
                        assert!(
                            !aspect.holds(size, dpi),
                            "{} at {dpi} dpi is {size}, which reads as {}",
                            sheet.label(),
                            aspect.label()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_size_a_choice_produces_is_one_that_shape_holds() {
        // The round trip that makes the strip stable: pick a shape, and the
        // canvas you are given is one that shape still recognises. It failed
        // under exact cross-multiplication, because a 5000 square becomes
        // 5000 x 2813 and that is not an exact 16:9.
        for aspect in Aspect::ALL {
            if aspect.ratio().is_none() {
                continue;
            }
            for size in [
                UVec2::new(1, 1),
                UVec2::new(7, 13),
                UVec2::new(1000, 1000),
                UVec2::new(1601, 900),
                UVec2::new(5000, 5000),
                UVec2::new(1920, 1080),
                UVec2::new(Document::MAX_EDGE, 1),
            ] {
                let Chosen::Size(out) = choose(aspect, size, CanvasLimit::UNKNOWN) else {
                    panic!("no size");
                };
                assert!(
                    aspect.holds(out, 72),
                    "{} produced {out} from {size} and does not hold it",
                    aspect.label()
                );
            }
        }
    }

    #[test]
    fn a_locked_edge_stays_on_the_shape_the_strip_is_claiming() {
        // The whole reason `LockedShape` is here rather than in the dialog. A
        // canvas 1601 pixels wide *cannot* be exactly 16:9, so if the lock's
        // rounding and `holds`' rounding were two different pieces of code the
        // row of sizes would appear and vanish once per pixel as an edge was
        // dragged.
        //
        // Swept over every edge rather than one convenient width. The earlier
        // guard used 1600, where 1600 x 9 / 16 is exactly 900 and no rounding
        // mode is exercised at all; the widths that decide this are the ones
        // landing on a half, which is 6% of the domain at 16:9 and 25% at 4:3.
        let limit = CanvasLimit::UNKNOWN;
        for aspect in Aspect::ALL {
            let Some(shape) = LockedShape::of_aspect(aspect) else {
                continue;
            };
            for edge in (1..=4000).chain([9999, 12345, Document::MAX_EDGE]) {
                // Skip only where the *bound* bit, never where the arithmetic
                // did. Saturation is its own case and is asserted below; if it
                // were folded in here it could hide a real rounding
                // disagreement at the top of the range.
                let by_width = UVec2::new(edge, shape.height_for(edge, limit));
                if by_width.y < limit.max_edge() {
                    assert!(
                        aspect.holds(by_width, 72),
                        "{}: width {edge} gave {by_width}",
                        aspect.label()
                    );
                }
                let by_height = UVec2::new(shape.width_for(edge, limit), edge);
                if by_height.x < limit.max_edge() {
                    assert!(
                        aspect.holds(by_height, 72),
                        "{}: height {edge} gave {by_height}",
                        aspect.label()
                    );
                }
            }
        }
    }

    #[test]
    fn a_lock_that_saturates_stops_being_the_shape_and_says_so() {
        // The one case the sweep above excludes, and it is not a defect: a 4:3
        // canvas tall enough wants to be wider than the bound, the bound pins
        // it, and what is left is genuinely not 4:3 any more. What matters is
        // that nothing pretends otherwise — the strip settles to Custom rather
        // than lighting 4:3 over a canvas that has stopped being it.
        //
        // **The height is derived from the bound**, and that is this test's own
        // lesson: it used to be a literal 12345, chosen because 4:3 of it was
        // 16460 and the ceiling was 16384. Raising the ceiling to 32768 made
        // 16460 fit, so nothing saturated and the case under test stopped
        // existing — the assertion failed, which was luck, since a test that
        // had merely stopped exercising its own case would have passed.
        let limit = CanvasLimit::UNKNOWN;
        let shape = LockedShape::of_aspect(Aspect::Standard).expect("4:3 is a ratio");
        // Just past the tallest 4:3 canvas that fits, so the width saturates.
        let height = Document::MAX_EDGE * 3 / 4 + 100;
        let width = shape.width_for(height, limit);
        assert_eq!(width, Document::MAX_EDGE);
        let size = UVec2::new(width, height);
        assert!(!Aspect::Standard.holds(size, 72));
        assert_eq!(settle(Aspect::Standard, size, 72).aspect, Aspect::Custom);
    }

    #[test]
    fn a_lock_can_never_ask_for_a_canvas_that_cannot_exist() {
        // The lock computes where the fields clamp, so an extreme shape and a
        // large edge could drive the other one past the bound, or a very tall
        // one round it down to nothing.
        for limit in [CanvasLimit::UNKNOWN, CanvasLimit::of_device(4096)] {
            let top = limit.max_edge();
            for shape in [
                LockedShape::of(1, 100_000),
                LockedShape::of(100_000, 1),
                LockedShape::of(0, 0),
                LockedShape::of(1920, 1080),
            ] {
                for edge in [1u32, 37, 4096, top, Document::MAX_EDGE] {
                    let h = shape.height_for(edge, limit);
                    let w = shape.width_for(edge, limit);
                    assert!((1..=top).contains(&h), "{shape:?} {edge} -> {h}");
                    assert!((1..=top).contains(&w), "{shape:?} {edge} -> {w}");
                }
            }
        }
        // A field passing through nothing on its way to a number must not make
        // the lock divide by it.
        assert_eq!(LockedShape::of(100, 0), LockedShape::default());
        assert_eq!(
            LockedShape::default().height_for(700, CanvasLimit::UNKNOWN),
            700
        );
    }

    #[test]
    fn a_shape_does_not_hold_its_own_transpose() {
        // Otherwise 16:9 and 9:16 would be one row drawn twice, and `read` would
        // file every portrait video canvas under the landscape shape.
        assert!(Aspect::Wide.holds(UVec2::new(1920, 1080), 72));
        assert!(!Aspect::Wide.holds(UVec2::new(1080, 1920), 72));
        assert!(Aspect::WideTall.holds(UVec2::new(1080, 1920), 72));
        assert!(!Aspect::WideTall.holds(UVec2::new(1920, 1080), 72));
        assert!(Aspect::Standard.holds(UVec2::new(1024, 768), 72));
        assert!(!Aspect::Standard.holds(UVec2::new(768, 1024), 72));
    }

    #[test]
    fn a_size_that_is_both_reads_as_the_ratio() {
        // A5 at 1 dpi. Six pixels by eight is the nearest 3:4 canvas eight
        // pixels tall and also, at that resolution, a sheet of paper. The
        // module states which way that goes rather than leaving it to the order
        // of a table somebody may one day reorder.
        let tiny = Sheet::A5.pixels(1, Orientation::Portrait);
        assert_eq!(tiny, UVec2::new(6, 8));
        assert!(Aspect::Paper.holds(tiny, 1));
        assert_eq!(read(tiny, 1).aspect, Aspect::StandardTall);

        // And the largest of the four, so the boundary the sweep excludes is
        // written down rather than only described.
        let a6 = Sheet::A6.pixels(4, Orientation::Portrait);
        assert_eq!(a6, UVec2::new(17, 23));
        assert_eq!(read(a6, 4).aspect, Aspect::StandardTall);
    }

    // ------------------------------------------------------------- the reading

    #[test]
    fn a_shipped_size_reads_back_as_the_row_it_came_from() {
        for aspect in Aspect::ALL {
            for (i, preset) in aspect.presets().iter().enumerate() {
                let reading = read(preset.size(), 72);
                assert_eq!(reading.aspect, aspect, "{}", preset.label);
                assert_eq!(reading.preset, Some(i), "{}", preset.label);
                assert_eq!(reading.sheet, None);
            }
        }
    }

    #[test]
    fn a_sheet_reads_back_as_that_sheet_at_that_resolution_and_no_other() {
        for sheet in Sheet::ALL {
            for (dpi, _) in DPI_CHOICES {
                for up in Orientation::ALL {
                    let size = sheet.pixels(dpi, up);
                    let reading = read(size, dpi);
                    assert_eq!(reading.aspect, Aspect::Paper, "{}", sheet.label());
                    assert_eq!(reading.sheet, Some(sheet));
                    assert_eq!(reading.orientation, Some(up));

                    // The whole point of carrying the resolution: A4 at 300 is
                    // a plain custom canvas at 72, because at 72 those pixels
                    // measure 875 x 1238 mm.
                    if dpi != 72 {
                        assert_eq!(
                            read(size, 72).aspect,
                            Aspect::Custom,
                            "{} at {dpi} dpi still read as paper at 72",
                            sheet.label()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_unfiled_size_is_custom_and_carries_no_sheet() {
        let reading = read(UVec2::new(1234, 567), 300);
        assert_eq!(reading.aspect, Aspect::Custom);
        assert_eq!(reading.preset, None);
        assert_eq!(reading.sheet, None);
        assert_eq!(reading.orientation, None);
    }

    #[test]
    fn custom_keeps_the_strip_where_it_is_and_a_left_shape_does_not() {
        // Both halves of `settle`, and they pull in opposite directions.
        let odd = UVec2::new(1234, 567);
        assert_eq!(settle(Aspect::Custom, odd, 72).aspect, Aspect::Custom);
        // Custom is sticky even over a size that would file itself.
        assert_eq!(
            settle(Aspect::Custom, UVec2::new(1920, 1080), 72).aspect,
            Aspect::Custom
        );
        // A shape that still holds is kept, with its preset lit.
        let settled = settle(Aspect::Wide, UVec2::new(3840, 2160), 72);
        assert_eq!(settled.aspect, Aspect::Wide);
        assert_eq!(settled.preset, Some(2));
        // A ratio that still holds but is nobody's preset lights nothing.
        let off = settle(Aspect::Wide, UVec2::new(1600, 900), 72);
        assert_eq!(off.aspect, Aspect::Wide);
        assert_eq!(off.preset, None);
        // One that has stopped holding is read afresh.
        assert_eq!(settle(Aspect::Wide, odd, 72).aspect, Aspect::Custom);
        assert_eq!(
            settle(Aspect::Wide, UVec2::new(2000, 2000), 72).aspect,
            Aspect::Square
        );
    }

    #[test]
    fn a_shape_never_reports_a_sheet_and_paper_never_reports_one_it_is_not() {
        // The `Option` on `Reading::orientation` is what stops a 16:9 reading
        // overwriting the paper row's own toggle, so it has to actually be
        // absent rather than merely ignored.
        for aspect in Aspect::ALL {
            let reading = Reading::under(aspect, UVec2::new(1920, 1080), 72);
            if aspect == Aspect::Paper {
                assert_eq!(reading.sheet, None, "1920x1080 is not a sheet at 72 dpi");
            }
            assert_eq!(reading.sheet.is_some(), reading.orientation.is_some());
        }
        let a4 = Sheet::A4.pixels(300, Orientation::Landscape);
        let reading = Reading::under(Aspect::Paper, a4, 300);
        assert_eq!(reading.sheet, Some(Sheet::A4));
        assert_eq!(reading.orientation, Some(Orientation::Landscape));
    }

    // -------------------------------------------------------------- the choice

    #[test]
    fn choosing_a_shape_keeps_the_longer_edge_and_makes_it_the_new_long_one() {
        let limit = CanvasLimit::UNKNOWN;
        // The transpose falls out of the rule, which is the point of it.
        assert_eq!(
            choose(Aspect::WideTall, UVec2::new(1920, 1080), limit),
            Chosen::Size(UVec2::new(1080, 1920))
        );
        assert_eq!(
            choose(Aspect::Wide, UVec2::new(1080, 1920), limit),
            Chosen::Size(UVec2::new(1920, 1080))
        );
        // So does keeping the scale: a 12000 square does not jump to a video
        // size.
        assert_eq!(
            choose(Aspect::Wide, UVec2::new(12000, 12000), limit),
            Chosen::Size(UVec2::new(12000, 6750))
        );
        assert_eq!(
            choose(Aspect::Standard, UVec2::new(1920, 1080), limit),
            Chosen::Size(UVec2::new(1920, 1440))
        );
        assert_eq!(
            choose(Aspect::StandardTall, UVec2::new(1920, 1080), limit),
            Chosen::Size(UVec2::new(1440, 1920))
        );
        assert_eq!(
            choose(Aspect::Square, UVec2::new(2480, 3508), limit),
            Chosen::Size(UVec2::new(3508, 3508))
        );
    }

    #[test]
    fn custom_moves_nothing_and_paper_states_its_resolution() {
        let limit = CanvasLimit::UNKNOWN;
        assert_eq!(
            choose(Aspect::Custom, UVec2::new(37, 41), limit),
            Chosen::Unchanged
        );
        assert_eq!(
            choose(Aspect::Paper, UVec2::new(37, 41), limit),
            Chosen::Sheet {
                sheet: Sheet::A4,
                dpi: PAPER_DPI
            }
        );
    }

    #[test]
    fn what_a_choice_produces_is_always_the_shape_it_was_asked_for() {
        // The rounding is integer and half up, so a ratio that does not divide
        // exactly cannot come out as some neighbouring shape.
        let limit = CanvasLimit::UNKNOWN;
        for aspect in Aspect::ALL {
            let Some((w, h)) = aspect.ratio() else {
                continue;
            };
            for size in [
                UVec2::new(1, 1),
                UVec2::new(7, 13),
                UVec2::new(5000, 5000),
                UVec2::new(1921, 1080),
                UVec2::new(Document::MAX_EDGE, 1),
                UVec2::new(1, Document::MAX_EDGE),
            ] {
                let Chosen::Size(out) = choose(aspect, size, limit) else {
                    panic!("{} did not answer with a size", aspect.label());
                };
                assert!(limit.permits(out), "{} {size} -> {out}", aspect.label());
                // Cross-multiplied, which is the exact statement of "the short
                // edge is the nearest whole pixel to the ratio" and needs no
                // float at all: the residual is the long part of the ratio
                // times however far the short edge was moved, so half the long
                // part is the whole of what rounding may cost.
                let residual =
                    (u64::from(out.x) * u64::from(h)).abs_diff(u64::from(out.y) * u64::from(w));
                assert!(
                    2 * residual <= u64::from(w.max(h)),
                    "{} {size} -> {out} is off {w}:{h} by {residual}",
                    aspect.label()
                );
            }
        }
    }

    #[test]
    fn a_choice_can_never_ask_for_more_than_the_device_holds() {
        let limit = CanvasLimit::of_device(4096);
        for aspect in Aspect::ALL {
            if aspect.ratio().is_none() {
                continue;
            }
            for size in [
                UVec2::new(1, 1),
                UVec2::new(16384, 16384),
                UVec2::new(16384, 1),
            ] {
                let Chosen::Size(out) = choose(aspect, size, limit) else {
                    panic!("no size");
                };
                assert!(limit.permits(out), "{} {size} -> {out}", aspect.label());
            }
        }
    }

    // --------------------------------------------------------------- the bound

    #[test]
    fn the_device_is_the_smaller_of_the_two_bounds_and_never_zero() {
        assert_eq!(CanvasLimit::of_device(8192).max_edge(), 8192);
        // A device claiming more than the format allows is still bounded by the
        // format, which is what keeps a document Umber makes one it can save.
        assert_eq!(CanvasLimit::of_device(32768).max_edge(), Document::MAX_EDGE);
        // A nonsense reading must not produce a canvas with no pixels in it.
        assert_eq!(CanvasLimit::of_device(0).max_edge(), 1);
        assert!(CanvasLimit::of_device(0).permits(UVec2::new(1, 1)));
    }

    #[test]
    fn a_machine_that_holds_everything_is_told_nothing() {
        assert_eq!(CanvasLimit::UNKNOWN.notice(), None);
        assert_eq!(CanvasLimit::of_device(Document::MAX_EDGE).notice(), None);
        let notice = CanvasLimit::of_device(8192).notice().expect("a sentence");
        assert!(notice.contains("8192"), "{notice}");
        // The house rule for anything the interface draws.
        assert!(!notice.contains('—'), "{notice}");
    }

    #[test]
    fn a_sheet_is_bounded_by_the_resolution_rather_than_refused_after_the_fact() {
        // The failure this prevents: A3 at 2400 dpi is 28063 x 39685, so a
        // resolution field that simply ran to `MAX_DPI` would let somebody ask
        // for a canvas nothing can allocate, with an A3 button lit beside it.
        for limit in [
            CanvasLimit::UNKNOWN,
            CanvasLimit::of_device(8192),
            CanvasLimit::of_device(4096),
            CanvasLimit::of_device(2048),
        ] {
            for sheet in Sheet::ALL {
                let top = sheet.max_dpi(limit);
                assert!(
                    limit.permits(sheet.pixels(top, Orientation::Portrait)),
                    "{} at its own top of {top} dpi does not fit {}",
                    sheet.label(),
                    limit.max_edge()
                );
                // And it is the *highest* such resolution, not merely a safe
                // one: a bound that answered 1 would pass the line above and be
                // useless.
                if top < Document::MAX_DPI as u32 {
                    assert!(
                        !limit.permits(sheet.pixels(top + 1, Orientation::Portrait)),
                        "{} would still fit at {} dpi",
                        sheet.label(),
                        top + 1
                    );
                }
            }
        }
    }

    #[test]
    fn every_sheet_at_every_offered_resolution_fits_an_ordinary_machine() {
        // Which is what makes the quick-pick's four figures the four figures
        // rather than a list that has to be filtered on most computers.
        for sheet in Sheet::ALL {
            for (dpi, _) in DPI_CHOICES {
                assert!(
                    CanvasLimit::UNKNOWN.permits(sheet.pixels(dpi, Orientation::Portrait)),
                    "{} at {dpi} dpi",
                    sheet.label()
                );
            }
        }
    }

    #[test]
    fn a_large_canvas_says_what_it_costs_and_an_ordinary_one_is_quiet() {
        assert_eq!(memory_note(UVec2::new(2048, 2048)), None);
        assert_eq!(memory_note(UVec2::new(5000, 5000)), None);
        let note = memory_note(UVec2::new(8192, 8192)).expect("a sentence");
        assert!(note.contains("MB"), "{note}");
        let big = memory_note(UVec2::new(16384, 16384)).expect("a sentence");
        assert!(big.contains("1.0 GB"), "{big}");
        assert!(!big.contains('—'), "{big}");
    }
}
