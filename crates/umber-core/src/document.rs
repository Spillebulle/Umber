//! Document model.
//!
//! A document is a canvas size, what lies under the layer stack, and how many
//! of its pixels go to an inch. The layers themselves are [`crate::layer`]'s
//! and their pixels are the renderer's; nothing here holds an image.
//!
//! # The background is a property, not a layer
//!
//! Painting on white is the normal case, and there are three ways to arrange
//! it. Filling the bottom layer with white is what a painter would do by hand,
//! and it is wrong here: it cannot be changed afterwards without repainting,
//! erasing on that layer punches a hole to the checkerboard, and "transparent"
//! stops being expressible. Compositing white *over* the finished stack is
//! worse still. So the background is a document property, composited **under**
//! the stack inside the one composite pass the layers already use —
//! `composite.wgsl` adds it after the loop, which costs one multiply-add per
//! fragment and is the exact identity when it is transparent.
//!
//! Everything that reuses that pass therefore gets it for free and cannot
//! disagree with the screen: the PNG export, the eyedropper and a smudging
//! brush's canvas probe all composite through the same shader.

use glam::UVec2;

use crate::color::Color;

/// What lies under the layer stack.
///
/// Deliberately only two cases. A *partly* transparent background would have to
/// answer what an export means — is the checkerboard part of the picture? — and
/// buys nothing a bottom layer at reduced opacity does not already do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Background {
    /// Nothing under the stack. The checkerboard shows through on screen and an
    /// export keeps its alpha, which is what Umber has always done.
    Transparent,
    /// One opaque colour. Always opaque — see the type's own note.
    Colour(Color),
}

impl Default for Background {
    /// White, which is what a new document starts on.
    fn default() -> Self {
        Self::WHITE
    }
}

impl Background {
    pub const WHITE: Self = Self::Colour(Color::WHITE);
    pub const BLACK: Self = Self::Colour(Color::BLACK);

    /// A background of `colour`, with any alpha discarded.
    pub fn opaque(colour: Color) -> Self {
        Self::Colour(colour.with_alpha(1.0))
    }

    pub fn is_transparent(self) -> bool {
        matches!(self, Self::Transparent)
    }

    /// The colour, or `None` when there is nothing there.
    pub fn colour(self) -> Option<Color> {
        match self {
            Self::Transparent => None,
            Self::Colour(c) => Some(c),
        }
    }

    /// Premultiplied linear RGBA, the form the composite uniform wants.
    ///
    /// Transparent is all zeroes, and the shader's `acc + bg * (1 - acc.a)` is
    /// then the exact identity — a document with no background pays one
    /// multiply-add and no branch.
    pub fn premultiplied(self) -> [f32; 4] {
        match self {
            Self::Transparent => [0.0; 4],
            // Alpha is 1 by construction, so premultiplying is a copy. Written
            // out anyway so the contract is visible at the one place the GPU
            // reads it.
            Self::Colour(c) => [c.r * c.a, c.g * c.a, c.b * c.a, c.a],
        }
    }
}

/// A physical unit for the size readout beside the pixel dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Millimetres,
    Inches,
}

impl Unit {
    pub const ALL: [Unit; 2] = [Self::Millimetres, Self::Inches];

    pub fn label(self) -> &'static str {
        match self {
            Self::Millimetres => "mm",
            Self::Inches => "in",
        }
    }

    /// How many of this unit make an inch.
    fn per_inch(self) -> f32 {
        match self {
            Self::Millimetres => 25.4,
            Self::Inches => 1.0,
        }
    }
}

/// Physical size of `pixels` at `dpi`, in `unit`.
pub fn physical_size(pixels: u32, dpi: f32, unit: Unit) -> f32 {
    pixels as f32 / dpi.max(Document::MIN_DPI) * unit.per_inch()
}

/// The inverse: how many pixels `size` covers at `dpi`.
///
/// Rounded rather than truncated, and never zero — a canvas with no pixels in
/// it is not a document, and a text field on its way to "10 mm" passes through
/// "1 mm" as it is typed.
pub fn pixels_for(size: f32, dpi: f32, unit: Unit) -> u32 {
    let px = (size / unit.per_inch() * dpi.max(Document::MIN_DPI)).round();
    (px.max(1.0) as u32).clamp(1, Document::MAX_EDGE)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Document {
    pub size: UVec2,
    /// What lies under the layer stack. See [`Background`].
    pub background: Background,
    /// Pixels per inch. Metadata: it changes no pixel and is used only to say
    /// what the canvas measures on paper, and to size a print preset.
    pub dpi: f32,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            size: UVec2::new(2048, 2048),
            background: Background::default(),
            dpi: Self::DEFAULT_DPI,
        }
    }
}

impl Document {
    /// Same bound the importer holds itself to, so a document made here can
    /// always be saved and reopened. The device's own
    /// `max_texture_dimension_2d` may be lower and is checked by the caller.
    ///
    /// **32768, and what that is really a ceiling on is the *format*.** The
    /// machine decides what an artist can actually reach:
    /// `CanvasLimit::of_device` clamps every size the dialogs offer to
    /// `max_texture_dimension_2d` and says in a sentence what the machine
    /// holds, and `Document::new` clamps as a backstop — so raising this
    /// changes nothing on a device that will not go there. Measured with
    /// `umber-render`'s `measure-limits` on one real machine: an RTX 3080
    /// reports **32768 on Vulkan and 16384 on Dx12**, and an Intel iGPU reports
    /// 16384 on both. 16384 is a hard limit of the D3D12 specification and of
    /// Metal, so this is a Vulkan ceiling rather than a general one.
    ///
    /// **What it costs is the thing to know before using it.** A 32768² layer
    /// is 4.3 GB of texture, so a 10 GB card holds two of them and a document
    /// that asks for more fails at `create_texture` — which is fatal, and is a
    /// refusal Umber does not yet make. `MAX_TOTAL_BYTES` admits three such
    /// layers on import. And a full-canvas undo patch is 4.3 GB against a
    /// default budget of 512 MB, so the history holds none of them: the
    /// panel says "earlier edits discarded" and means it. None of that is new
    /// — it is the same arithmetic that already applies at 10000² — but it
    /// arrives four times faster here.
    pub const MAX_EDGE: u32 = 32768;

    /// The resolution a document has when nothing states one.
    ///
    /// 72 rather than 300: Umber is a screen-first painting application, an ORA
    /// with no `xres` means "unstated", and a wrong *large* number would make
    /// the physical readout quietly lie about a canvas nobody intends to print.
    /// Print presets carry their own.
    pub const DEFAULT_DPI: f32 = 72.0;
    pub const MIN_DPI: f32 = 1.0;
    pub const MAX_DPI: f32 = 2400.0;

    /// A document of this size, on the default background at the default
    /// resolution.
    ///
    /// Callers that know better — the importers, which must not invent a
    /// background a file does not have — say so with [`Document::with_background`].
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            size: UVec2::new(
                width.clamp(1, Self::MAX_EDGE),
                height.clamp(1, Self::MAX_EDGE),
            ),
            ..Self::default()
        }
    }

    pub fn with_background(self, background: Background) -> Self {
        Self { background, ..self }
    }

    pub fn with_dpi(self, dpi: f32) -> Self {
        Self {
            dpi: sane_dpi(dpi),
            ..self
        }
    }

    pub fn size_vec2(&self) -> glam::Vec2 {
        self.size.as_vec2()
    }

    /// Bytes one RGBA8 layer occupies.
    pub fn layer_bytes(&self) -> u64 {
        self.size.x as u64 * self.size.y as u64 * 4
    }

    /// Physical width and height in `unit`, at this document's resolution.
    pub fn physical(&self, unit: Unit) -> (f32, f32) {
        (
            physical_size(self.size.x, self.dpi, unit),
            physical_size(self.size.y, self.dpi, unit),
        )
    }
}

/// A resolution that can be shown and divided by.
///
/// A file is allowed to say anything, including zero and NaN, and the physical
/// readout divides by it.
pub fn sane_dpi(dpi: f32) -> f32 {
    if dpi.is_finite() && dpi > 0.0 {
        dpi.clamp(Document::MIN_DPI, Document::MAX_DPI)
    } else {
        Document::DEFAULT_DPI
    }
}

/// Where the old pixels sit inside a resized canvas.
///
/// Nine rather than one because the choice is free — it is two clamped
/// subtractions — and because "extend the canvas to the right" and "add room
/// all round" are both things people do constantly and neither is the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Centre,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// Row-major, top-left first — the order the nine-square control draws in.
    pub const GRID: [Anchor; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Centre,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    /// Horizontal and vertical position, each 0.0 (start), 0.5 or 1.0 (end).
    fn weights(self) -> (f32, f32) {
        let x = match self {
            Self::TopLeft | Self::Left | Self::BottomLeft => 0.0,
            Self::Top | Self::Centre | Self::Bottom => 0.5,
            Self::TopRight | Self::Right | Self::BottomRight => 1.0,
        };
        let y = match self {
            Self::TopLeft | Self::Top | Self::TopRight => 0.0,
            Self::Left | Self::Centre | Self::Right => 0.5,
            Self::BottomLeft | Self::Bottom | Self::BottomRight => 1.0,
        };
        (x, y)
    }
}

/// The copy a resize has to perform, in pixels.
///
/// Computed here rather than in the renderer so it can be tested without a
/// device, and so the app and the GPU cannot disagree about where the old
/// picture lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasCopy {
    /// Top-left of the region read out of the old canvas.
    pub from: UVec2,
    /// Top-left it is written to in the new one.
    pub to: UVec2,
    /// How much is copied. Zero on either axis means nothing survives, which
    /// cannot happen while both canvases have at least one pixel.
    pub size: UVec2,
}

impl CanvasCopy {
    /// Where `old` pixels land in a `new` canvas, held at `anchor`.
    pub fn plan(old: UVec2, new: UVec2, anchor: Anchor) -> Self {
        let (wx, wy) = anchor.weights();
        let (from_x, to_x) = axis(old.x, new.x, wx);
        let (from_y, to_y) = axis(old.y, new.y, wy);
        Self {
            from: UVec2::new(from_x, from_y),
            to: UVec2::new(to_x, to_y),
            size: UVec2::new(old.x.min(new.x), old.y.min(new.y)),
        }
    }
}

/// One axis of [`CanvasCopy::plan`]: the source and destination offsets.
///
/// Exactly one of them is non-zero — growing offsets the destination, cropping
/// offsets the source — which is why this is a pair of saturating subtractions
/// rather than a signed delta that both sides then have to interpret.
fn axis(old: u32, new: u32, weight: f32) -> (u32, u32) {
    let grow = new.saturating_sub(old);
    let crop = old.saturating_sub(new);
    (
        (crop as f32 * weight).round() as u32,
        (grow as f32 * weight).round() as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transparent_background_is_the_shaders_exact_identity() {
        // `acc + bg * (1 - acc.a)` with all-zero bg leaves the stack untouched,
        // which is what lets the background cost no branch.
        assert_eq!(Background::Transparent.premultiplied(), [0.0; 4]);
    }

    #[test]
    fn a_background_colour_is_opaque_whatever_it_was_given() {
        let bg = Background::opaque(Color::new(1.0, 0.5, 0.25, 0.3));
        assert_eq!(bg.premultiplied(), [1.0, 0.5, 0.25, 1.0]);
    }

    #[test]
    fn physical_size_is_pixels_over_resolution() {
        // A4 at 300 dpi is 2480 x 3508 px, which is the pair every print preset
        // in the interface is built from.
        let doc = Document::new(2480, 3508).with_dpi(300.0);
        let (w, h) = doc.physical(Unit::Millimetres);
        assert!((w - 210.0).abs() < 0.5, "{w}");
        assert!((h - 297.0).abs() < 0.5, "{h}");

        let (w, h) = doc.physical(Unit::Inches);
        assert!((w - 8.27).abs() < 0.02, "{w}");
        assert!((h - 11.69).abs() < 0.02, "{h}");
    }

    #[test]
    fn the_two_directions_of_the_readout_agree() {
        for dpi in [72.0, 96.0, 150.0, 300.0, 600.0] {
            for unit in Unit::ALL {
                for px in [1u32, 37, 1080, 2480, 4096] {
                    let back = pixels_for(physical_size(px, dpi, unit), dpi, unit);
                    assert_eq!(back, px, "{px} px at {dpi} dpi in {}", unit.label());
                }
            }
        }
    }

    #[test]
    fn a_canvas_never_rounds_away_to_nothing() {
        // Typing into the physical field passes through values that would
        // otherwise round to zero pixels, and a zero-sized texture is a
        // validation error rather than an empty canvas.
        assert_eq!(pixels_for(0.0, 300.0, Unit::Millimetres), 1);
        assert_eq!(pixels_for(-5.0, 300.0, Unit::Inches), 1);
        assert_eq!(
            pixels_for(1e9, 300.0, Unit::Inches),
            Document::MAX_EDGE,
            "the readout must not be able to ask for a canvas nothing can hold"
        );
    }

    #[test]
    fn a_file_cannot_hand_over_a_resolution_that_divides_by_zero() {
        assert_eq!(sane_dpi(0.0), Document::DEFAULT_DPI);
        assert_eq!(sane_dpi(f32::NAN), Document::DEFAULT_DPI);
        assert_eq!(sane_dpi(-300.0), Document::DEFAULT_DPI);
        assert_eq!(sane_dpi(300.0), 300.0);
        assert_eq!(sane_dpi(1e9), Document::MAX_DPI);
    }

    #[test]
    fn growing_a_canvas_offsets_the_destination_and_crops_nothing() {
        let plan = CanvasCopy::plan(UVec2::new(100, 100), UVec2::new(200, 200), Anchor::Centre);
        assert_eq!(plan.from, UVec2::ZERO);
        assert_eq!(plan.to, UVec2::new(50, 50));
        assert_eq!(plan.size, UVec2::new(100, 100));
    }

    #[test]
    fn shrinking_a_canvas_offsets_the_source_and_moves_nothing() {
        let plan = CanvasCopy::plan(UVec2::new(200, 200), UVec2::new(100, 100), Anchor::Centre);
        assert_eq!(plan.from, UVec2::new(50, 50));
        assert_eq!(plan.to, UVec2::ZERO);
        assert_eq!(plan.size, UVec2::new(100, 100));
    }

    #[test]
    fn the_corner_anchors_hold_the_corner_they_name() {
        let (old, new) = (UVec2::new(100, 100), UVec2::new(300, 200));
        let top_left = CanvasCopy::plan(old, new, Anchor::TopLeft);
        assert_eq!(top_left.to, UVec2::ZERO);

        let bottom_right = CanvasCopy::plan(old, new, Anchor::BottomRight);
        assert_eq!(bottom_right.to, UVec2::new(200, 100));

        // Growing one axis and cropping the other at once: each is decided on
        // its own, so the surviving strip is offset in both buffers.
        let mixed = CanvasCopy::plan(UVec2::new(100, 400), UVec2::new(300, 200), Anchor::Centre);
        assert_eq!(mixed.from, UVec2::new(0, 100));
        assert_eq!(mixed.to, UVec2::new(100, 0));
        assert_eq!(mixed.size, UVec2::new(100, 200));
    }

    #[test]
    fn a_copy_plan_always_stays_inside_both_canvases() {
        for anchor in Anchor::GRID {
            for old in [UVec2::new(1, 1), UVec2::new(7, 13), UVec2::new(400, 400)] {
                for new in [UVec2::new(1, 1), UVec2::new(13, 7), UVec2::new(400, 400)] {
                    let p = CanvasCopy::plan(old, new, anchor);
                    assert!(
                        (p.from + p.size).cmple(old).all(),
                        "{anchor:?} {old} -> {new}: {p:?}"
                    );
                    assert!(
                        (p.to + p.size).cmple(new).all(),
                        "{anchor:?} {old} -> {new}: {p:?}"
                    );
                }
            }
        }
    }
}
