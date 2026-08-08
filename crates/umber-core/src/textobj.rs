//! What makes a text layer still text tomorrow.
//!
//! [`crate::text`] turns a string into coverage and hands it to the paste path;
//! that is the whole of placing text, and once it is down the pixels are paint.
//! This module is the record that says what made them, so a layer can be set
//! again rather than retyped: a [`TextObject`] is the string, the face, the
//! figures and the placement, written into a saved document under `umber/text/`
//! and pointed at by [`crate::docformat::TEXT_ATTR`].
//!
//! `docs/text-tool.md` §3 has the argument. These are the rules it lives by.
//!
//! # The document version does not move, and the fingerprint is why
//!
//! `umber-text` is an attribute every other OpenRaster reader ignores, beside a
//! layer whose pixels are an ordinary PNG. An older Umber, or GIMP, or Krita,
//! decodes that PNG and shows **the identical picture**; what it loses is that
//! the text can be edited again. Plainer, not wrong, which is the line
//! [`crate::docformat::VERSION`] is drawn on — so it stays where it is.
//!
//! That argument has a hole in it, and it is the hole the whole of this module
//! is shaped around. An older build can open the file, let the artist paint over
//! the text layer, and save. The PNG then says one thing and the record says
//! another, and a build that trusted the record would re-render over somebody's
//! brushwork. So the record is written with a **[`Fingerprint`] of the pixels it
//! rendered** — the rectangle the layer PNG occupies and a hash of its bytes —
//! and on load a mismatch **discards the record and keeps the picture**. The
//! layer becomes an ordinary painted layer, which is what it now is. Exactly the
//! rule a saved history lives by: anything that does not line up is dropped,
//! whole.
//!
//! **The fingerprint is a property of the file and never of the session.** It is
//! computed by the writer from the bytes it is writing and checked by the reader
//! against the bytes it read, and it is then thrown away — a [`TextObject`] in
//! memory does not carry one. That is deliberate and it is what stops a second
//! class of bug: an in-memory fingerprint would go stale the moment a canvas
//! flip mirrored the layer, and it would have to be recomputed from a GPU
//! readback after every edit. In memory the two halves are kept in step by
//! **the paint gate** ([`crate::layer::EditRefusal::Text`]) and by
//! [`Placement::flipped`]; in a file they are kept in step by the fingerprint.
//!
//! It is a hash and **not a signature**. It detects change, accidental or
//! otherwise, and it says nothing whatever about who made it. Nothing here may
//! be described as verifying anything.
//!
//! # A missing font freezes, and never substitutes
//!
//! The record names a family, a style and — where the face could be asked for
//! one — its PostScript name. On another machine the exact face may be absent,
//! and [`TextFace::resolve`] then answers `None`: the saved pixels stand, the
//! layer draws them, and an attempt to edit the text raises a notice naming the
//! font that is missing.
//!
//! **`FontLibrary::resolve` is deliberately not what does this.** That one is
//! total by construction — it falls back to the nearest weight in the family and
//! then to the first face in the library, because a *preference* naming a font
//! that is gone still has to draw something. Re-rendering somebody's caption in
//! a substituted face changes the picture, silently, which is worse than not
//! re-rendering it at all. So this asks only for the exact pair and refuses.
//!
//! **Embedding the font in the `.ora` is the obvious repair and is refused.** It
//! would be font redistribution performed by the artist, without their
//! knowledge, in a file they may email; for a machine-licensed system font that
//! is a licence breach they did not commit, and for a commercial one it is worse.
//! It would also put half a megabyte to sixteen megabytes into every document.
//! The whole of `docs/text-tool.md` §2 exists to keep Umber out of the business
//! of moving font files around.
//!
//! # The record has a size bound of its own
//!
//! [`MAX_RECORD_BYTES`]. Every other entry in a saved document is sized by the
//! *canvas*, which [`crate::ImportedDocument::MAX_TOTAL_BYTES`] already bounds;
//! this one is sized by how much somebody typed, so that bound does not reach
//! it. A record over the limit is **not written**, and the save says so, rather
//! than writing one the reader will refuse: a text layer that quietly stopped
//! being editable at the next open would be a loss nobody was told about.

use glam::UVec2;
use serde::{Deserialize, Serialize};
use skrifa::string::StringId;
use skrifa::{FontRef, MetadataProvider};

use crate::color::Color;
use crate::fonts::{Face, FontLibrary};
use crate::geom::{FlipAxis, PixelRect};
use crate::text::{Align, TextBlock};
use crate::transform::Transform;

/// Revision of the record layout, independent of
/// [`crate::docformat::VERSION`](crate::docformat::VERSION).
///
/// Separate for the reason [`crate::docformat::history::VERSION`] is separate:
/// it governs something a build that cannot read it **discards** rather than
/// misreads. A record from a newer revision drops the text object and opens the
/// picture whole, which is exactly what every build before this one did with
/// every document. That is the whole of why saving one does not move the
/// document's version.
///
/// The bar for raising it is the same one, one level down: a revision an older
/// build would **misread**. Adding an optional field does not qualify — serde
/// ignores a field it has never heard of, so such a build reads the record it
/// understands and merely renders without whatever was added. Changing what an
/// existing field *means* does.
pub const VERSION: u32 = 1;

/// The most a single text record may occupy in the archive.
///
/// One mebibyte, and it is a bound on the *record* rather than on the canvas —
/// see the module docs for why nothing else in a saved document bounds it. A
/// mebibyte of UTF-8 is on the order of a million characters, which is a few
/// hundred pages of prose in one layer; [`crate::text::MAX_PIXELS`] will refuse
/// to set anything near it long before this bites at any legible size.
///
/// It bounds both directions. The writer measures the encoded record and writes
/// none at all when it is over, with a [`crate::SaveWarning`] naming the layer;
/// the reader refuses to read an entry longer than this without decompressing
/// it, which is what stops a small archive claiming a large record.
///
/// # Why this is not `MAX_EFFECTS_BYTES`, and what the two do share
///
/// A layer effects record has the same hazard and its bound is deliberately a
/// different *kind* of figure. `docimport::openraster::MAX_EFFECTS_BYTES` is
/// **derived** — one effect per kind, `effect::MAX_ENABLED` per document, times a
/// measured bytes-per-effect, doubled — so the model already makes an over-long
/// record unwritable and the bound is needed on the reading side alone. This one
/// **cannot be derived**: [`crate::text::MAX_PIXELS`] bounds the area a block
/// renders to, not how much somebody typed, so a legal block really can outrun
/// any figure chosen here. That is the whole reason this is public and checked at
/// both ends where that one is private and checked at one.
///
/// So one shared constant would be wrong in both directions: at a mebibyte the
/// effects bound would be sixteen times looser than its own model permits, and at
/// 64 KiB this one would refuse tens of thousands of characters somebody could
/// legitimately set. **What they do share is where the figure lives: with
/// whichever side can violate it.** A bound only a stranger's file can breach
/// belongs in the reader; one the writer can breach belongs beside the model, so
/// the refusal and the warning are one statement rather than two.
pub const MAX_RECORD_BYTES: usize = 1 << 20;

/// The longest a family, a style or a PostScript name may be in a record.
///
/// A bound of its own because [`MAX_RECORD_BYTES`] does not usefully reach these:
/// a record is a file somebody else wrote, and nothing stops it carrying most of
/// a mebibyte in the family field. That string reaches [`TextFace::label`] and
/// from there a notice, which is a sentence a person reads. A hundred and
/// twenty-eight bytes is comfortably past the longest real face name — "Source
/// Han Sans HW SC ExtraLight" is twenty-six — and it is enforced by
/// [`clean_name`] on the way in *and* on the way out, so a name too long to write
/// is one that comes back the same.
pub const MAX_NAME_BYTES: usize = 128;

/// The exact face a block of text was set in.
///
/// Three names rather than one, because they answer different questions when the
/// face is not here: a family nobody has is a font to go and install, a style
/// missing from a family that *is* here is a weight that was never in it, and
/// the PostScript name is the one string a foundry's own catalogue can be
/// searched for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextFace {
    /// The typographic family name, as [`Face::family`] holds it.
    pub family: String,
    /// The style within it — "Regular", "Bold Italic", "Condensed Light" — as
    /// [`Face::style`] holds it.
    pub style: String,
    /// The face's own PostScript name, or empty where it could not be read.
    ///
    /// Recorded rather than used as the lookup key, and that is a limit rather
    /// than a decision: [`Face`] does not carry one, so there is nothing in a
    /// [`FontLibrary`] to compare it against without loading every candidate off
    /// the disk. What it is for is the notice — it is the name somebody types
    /// into a search when they go looking for the font — and it is what a later
    /// revision would match on if `Face` ever grew the field.
    pub postscript: String,
}

impl TextFace {
    /// The record for a face, with its PostScript name where the caller could
    /// read one.
    ///
    /// The name is passed in rather than read here because reading it needs the
    /// font's bytes, and [`Face::load`] blocks on a file read — see
    /// [`postscript_name`], which is what the caller uses on the `FontRef` it
    /// already has in hand for the setting.
    pub fn of(face: &Face, postscript: impl Into<String>) -> Self {
        Self {
            family: face.family.clone(),
            style: face.style.clone(),
            postscript: postscript.into(),
        }
    }

    /// The face this record names, or `None` where this machine has not got it.
    ///
    /// **Exact, and never a substitution.** This is `FontLibrary::resolve`'s
    /// first clause and deliberately none of its fallbacks: that method is total
    /// because a *preference* naming a font that has gone still has to draw
    /// something, and re-rendering a picture in a face its author did not choose
    /// is a silent change to the picture. See the module docs.
    ///
    /// Case-insensitively on both halves, exactly as `resolve` matches: the
    /// spelling in a file is whatever the machine that wrote it spelled, and
    /// refusing over capitals would freeze text for no reason at all.
    pub fn resolve<'a>(&self, library: &'a FontLibrary) -> Option<&'a Face> {
        library.faces().iter().find(|f| {
            f.family.eq_ignore_ascii_case(&self.family) && f.style.eq_ignore_ascii_case(&self.style)
        })
    }

    /// How the face reads in a sentence: "Archivo Bold".
    pub fn label(&self) -> String {
        format!("{} {}", self.family, self.style)
    }

    /// What to tell somebody who has asked to edit text set in a face this
    /// machine has not got.
    ///
    /// Two sentences and no em-dash, because this is a notice and not a comment.
    /// It names the font, says the picture is untouched, and says what would fix
    /// it — which is the whole of what an artist can act on.
    ///
    /// **Nothing draws it yet**, because nothing in this build can ask to edit a
    /// text layer. It says "install the font to edit it again", which is a promise
    /// about a control wave two adds; drawing it before then would be the lying
    /// notice this project refuses, so the panel that shows it is the panel that
    /// makes it true.
    pub fn missing_notice(&self) -> String {
        let mut out = format!("This text was set in {}", self.label());
        if !self.postscript.is_empty() {
            out.push_str(&format!(" ({})", self.postscript));
        }
        out.push_str(
            ", which is not on this machine. The text is shown exactly as it was saved. \
             Install the font to edit it again.",
        );
        out
    }
}

/// A face's PostScript name, for [`TextFace::of`].
///
/// Empty where the font does not carry one, which is legal and happens with
/// hand-built and subset files. Its own function here rather than a method on
/// [`Face`] because `fonts` holds paths and not bytes, deliberately: a library
/// of several hundred faces that kept every one of them parsed would be hundreds
/// of megabytes resident to set one caption.
pub fn postscript_name(font: &FontRef<'_>) -> String {
    font.localized_strings(StringId::POSTSCRIPT_NAME)
        .next()
        .map(|s| s.to_string().trim().to_string())
        .unwrap_or_default()
}

/// Where a block of text sits on the canvas.
///
/// Exactly [`Transform`]'s four numbers, because placing text *is* a paste and
/// the transform tool is what moved it: a source rectangle at identity, an
/// offset, a per-axis scale whose sign carries a flip, and an angle. Recording
/// the numbers rather than an [`crate::Affine`] is what lets the record be
/// turned back into a `Transform` the tool can pick up again, and it is what
/// makes a mirror expressible — see [`Placement::flipped`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Where the setting's own pixels sat before anything was dragged, in
    /// document space. Its centre is the pivot for the scale and the rotation.
    pub source: PixelRect,
    /// Document pixels the source has been moved by.
    pub offset: [f32; 2],
    /// Scale about the pivot, per axis. Negative is a flip.
    pub scale: [f32; 2],
    /// Rotation about the pivot, radians, clockwise on screen.
    pub angle: f32,
}

impl Placement {
    /// Text that has been placed and not moved.
    pub fn identity(source: PixelRect) -> Self {
        Self {
            source,
            offset: [0.0, 0.0],
            scale: [1.0, 1.0],
            angle: 0.0,
        }
    }

    /// The placement a floating transform has reached.
    pub fn of(transform: &Transform) -> Self {
        let source = transform.source();
        Self {
            source: PixelRect {
                x: source.min.x.max(0.0) as u32,
                y: source.min.y.max(0.0) as u32,
                width: (source.max.x - source.min.x).max(0.0) as u32,
                height: (source.max.y - source.min.y).max(0.0) as u32,
            },
            offset: transform.offset.to_array(),
            scale: transform.scale.to_array(),
            angle: transform.angle,
        }
    }

    /// The transform this placement is, so the tool can pick the text up again.
    pub fn transform(&self) -> Transform {
        let mut t = Transform::identity(self.source);
        t.offset = glam::Vec2::from_array(self.offset);
        t.scale = glam::Vec2::from_array(self.scale);
        t.angle = self.angle;
        t
    }

    /// Every figure a rebuilt [`Transform`] would divide by or draw with is a
    /// real number, and neither scale is zero.
    ///
    /// Checked on the way in rather than trusted: a `0` scale makes
    /// [`Transform::inverse`] a matrix nothing can invert, and a non-finite
    /// offset reaches vertex positions, where one NaN is a mesh discarded whole.
    /// The same reason [`crate::text::set`] refuses a size that is not a number
    /// instead of clamping it.
    pub fn is_sane(&self) -> bool {
        self.offset.iter().all(|v| v.is_finite())
            && self.scale.iter().all(|v| v.is_finite() && *v != 0.0)
            && self.angle.is_finite()
            && self.source.width > 0
            && self.source.height > 0
    }

    /// The same text, mirrored with the canvas.
    ///
    /// **Exact, and it is the reason a canvas flip does not cost a text layer
    /// its record.** Undoing a flip is another flip, so dropping the record here
    /// would destroy something no undo could put back — which is the failure
    /// `Selection::flipped` exists to avoid, in the same place.
    ///
    /// The algebra, with `m = R(θ)·diag(s)`, `A(p) = m(p − c) + c + o` and the
    /// mirror `N` being `diag(-1, 1)` or `diag(1, -1)`: the picture a re-render
    /// must reproduce is the old one read through the mirror, so the map it
    /// needs is `N ∘ A ∘ (translate by −d)` where `d` is how far the mirrored
    /// source rectangle moved. Both terms fall out to one rule, because
    /// `diag(-1, 1)·R(θ)·diag(sx, sy)` **is** `R(−θ)·diag(−sx, sy)`: mirror the
    /// source rectangle, negate the angle, and negate the scale and the offset
    /// on the axis that flipped. Nothing is approximated and flipping twice is
    /// the identity, which is what
    /// `flipping_a_placement_twice_puts_every_pixel_back` pins.
    ///
    /// `None` where the source rectangle is not inside the canvas, which cannot
    /// arise from `Clip::place` — it crops to the document — but would produce a
    /// mirrored rectangle wrapping past zero if it did. A record that lies about
    /// where its pixels are is worse than no record, so the caller drops it.
    pub fn flipped(&self, axis: FlipAxis, canvas: UVec2) -> Option<Self> {
        let (x, y) = (self.source.x, self.source.y);
        let (w, h) = (self.source.width, self.source.height);
        if x.checked_add(w)? > canvas.x || y.checked_add(h)? > canvas.y {
            return None;
        }
        let (source, offset, scale) = match axis {
            FlipAxis::Horizontal => (
                PixelRect {
                    x: canvas.x - x - w,
                    y,
                    width: w,
                    height: h,
                },
                [-self.offset[0], self.offset[1]],
                [-self.scale[0], self.scale[1]],
            ),
            FlipAxis::Vertical => (
                PixelRect {
                    x,
                    y: canvas.y - y - h,
                    width: w,
                    height: h,
                },
                [self.offset[0], -self.offset[1]],
                [self.scale[0], -self.scale[1]],
            ),
        };
        Some(Self {
            source,
            offset,
            scale,
            angle: -self.angle,
        })
    }
}

/// A hash of the pixels a text record rendered, and the rectangle they occupy.
///
/// **What is hashed is what goes into the file**: the layer's PNG is trimmed to
/// its own non-transparent bounding box and written as straight-alpha sRGB, and
/// this is that rectangle and those bytes. Hashing the canvas-sized
/// layer-texture buffer instead is the obvious reading and is wrong in one
/// direction that matters: `docformat`'s `trim` drops every fully transparent
/// pixel, so a buffer holding a premultiplied `(5, 5, 5, 0)` comes back as
/// `(0, 0, 0, 0)` and a fingerprint over it would refuse a document nobody had
/// touched. What the file holds round-trips through PNG byte for byte.
///
/// The rectangle is compared first because it is four integers, and because it
/// is the half that catches the ordinary case: paint added to a text layer
/// almost always moves the bounding box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    /// Where the layer's own image sits on the canvas, and how big it is.
    pub rect: PixelRect,
    /// [`hash_bytes`] of the image's straight-alpha sRGB bytes.
    pub hash: u64,
}

impl Fingerprint {
    /// Take a fingerprint of one placed image.
    pub fn of(rect: PixelRect, bytes: &[u8]) -> Self {
        Self {
            rect,
            hash: hash_bytes(bytes),
        }
    }

    /// Is this the same image?
    ///
    /// A hash, so this is "no reason to think otherwise" rather than proof — and
    /// **the rectangle and 64 bits are all that stands between an older build's
    /// brushwork and a re-render**, which is worth stating plainly rather than
    /// appealing to a gate. The gate that refuses a brush on a text layer belongs
    /// to *this* build; the case this exists for is a build that has never heard
    /// of any of it, painting freely on the layer and saving. So there is no
    /// second line of defence behind this one.
    ///
    /// The trade is still sound: the rectangle catches nearly every real case,
    /// because paint added to a layer moves its non-transparent bounding box, and
    /// a 64-bit collision on the remainder is not something an accident produces.
    /// It is a hash and not a signature, and it is not described as one anywhere.
    pub fn matches(&self, rect: PixelRect, bytes: &[u8]) -> bool {
        self.rect == rect && self.hash == hash_bytes(bytes)
    }
}

/// FNV-1a, 64-bit, written out rather than taken from `DefaultHasher`.
///
/// This number goes into a file and is compared against on another machine and
/// in another build, so it has to be the same arithmetic everywhere and for
/// ever. `std`'s `DefaultHasher` promises neither: the algorithm behind it is
/// explicitly unspecified and free to change between releases. `autosave.rs`
/// writes the same twelve lines out for the same reason.
///
/// Not a cryptographic hash and not a signature — see the module docs.
///
/// **What it costs is bounded by the trimmed image and not by the canvas**, which
/// for a caption is a few kilobytes and nothing at all. A text layer whose ink
/// genuinely spans a 10000² canvas is the other end: a byte at a time, that is a
/// few tenths of a second — once on the save, beside the blocking readback and
/// the PNG encode it already pays for, and once on the open beside the decode.
/// Worth knowing before somebody puts a fingerprint on something larger.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// What made a text layer's pixels.
///
/// Held by [`crate::Layer`] and written into a saved document. It is
/// deliberately **not** a rendering: the pixels are in the layer, as paint, and
/// this is the recipe beside them. Nothing reads it to draw the document — a
/// text layer composites exactly as any other layer does, which is why nothing
/// in `umber-render` and nothing in `composite.wgsl` learns that text exists.
#[derive(Clone, Debug, PartialEq)]
pub struct TextObject {
    /// The string and the figures, exactly as [`crate::text::set`] takes them.
    pub block: TextBlock,
    pub face: TextFace,
    /// The colour the coverage was painted in, including its alpha — see
    /// `Setting::clip`, which multiplies the two.
    pub colour: Color,
    pub placement: Placement,
}

impl TextObject {
    pub fn new(block: TextBlock, face: TextFace, colour: Color, placement: Placement) -> Self {
        Self {
            block,
            face,
            colour,
            placement,
        }
    }

    /// Roughly what this costs in memory, for the undo budget.
    ///
    /// A record parked in a structural undo entry is kilobytes against a
    /// canvas-sized texture slice, so this changes no eviction anybody will ever
    /// see. It is counted anyway for the reason [`crate::StackShape::byte_len`]
    /// counts a parked slice: a budget blind to part of what it holds is a
    /// budget that will be wrong later, and the string is the one field here
    /// with no bound but [`MAX_RECORD_BYTES`].
    pub fn byte_len(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.block.text.len()
            + self.face.family.len()
            + self.face.style.len()
            + self.face.postscript.len()
    }

    /// The same text, mirrored with the canvas — or `None` where it cannot be.
    /// See [`Placement::flipped`].
    pub fn flipped(&self, axis: FlipAxis, canvas: UVec2) -> Option<Self> {
        Some(Self {
            placement: self.placement.flipped(axis, canvas)?,
            ..self.clone()
        })
    }

    /// The record as it goes into the archive, fingerprinted with the image the
    /// writer is writing.
    ///
    /// The fingerprint is the *caller's*, never this object's: a `TextObject`
    /// carries none, so there is nothing here that can be stale. See the module
    /// docs.
    ///
    /// **It refuses everything the reader would refuse, and says which.** A
    /// record written that [`Self::from_json`] then declines is the failure
    /// [`MAX_RECORD_BYTES`] exists to prevent, one field over: the layer would
    /// quietly stop being editable at the next open, with the save having said
    /// nothing. So the placement is checked here as well as there, and the reason
    /// travels out so the notice names it rather than blaming the length —
    /// `serde_json` refuses a non-finite figure, which used to come back as "too
    /// much text".
    pub fn to_json(&self, print: &Fingerprint) -> Result<Vec<u8>, NotRecorded> {
        if !self.placement.is_sane()
            || !self.block.size.is_finite()
            || !self.block.line_spacing.is_finite()
            || !self.block.tracking.is_finite()
        {
            return Err(NotRecorded::Impossible);
        }
        // Compact rather than pretty, and that is not only a matter of size.
        // `ron::ser::PrettyConfig::new` takes the *platform's* line ending, so a
        // pretty-printed record would make the same document differ byte for
        // byte between Windows and Linux — right for a preferences file, wrong
        // for one that travels. `serde_json::to_vec` emits no line endings at
        // all.
        let json =
            serde_json::to_vec(&Record::of(self, print)).map_err(|_| NotRecorded::Impossible)?;
        if json.len() > MAX_RECORD_BYTES {
            return Err(NotRecorded::TooLarge);
        }
        Ok(json)
    }

    /// Read a record back, with the fingerprint it was written with.
    ///
    /// The caller compares that fingerprint against the image it decoded and
    /// **drops this object on a mismatch**. Returning the two together rather
    /// than checking here is deliberate: this module cannot see the archive, and
    /// a reader that had to remember to make a second call is a reader that will
    /// forget.
    pub fn from_json(bytes: &[u8]) -> Result<(Self, Fingerprint), RecordError> {
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(RecordError::TooLarge);
        }
        let record: Record =
            serde_json::from_slice(bytes).map_err(|e| RecordError::Unreadable(e.to_string()))?;
        if record.version > VERSION {
            return Err(RecordError::NewerVersion {
                version: record.version,
                supported: VERSION,
            });
        }
        record.into_object()
    }
}

/// Why a record could not be **written**.
///
/// Its own enum rather than [`RecordError`] because the sentences face the other
/// way: these become part of a [`crate::SaveWarning`] about a document being
/// saved, where those become part of an [`crate::ImportWarning`] about one being
/// opened. Two variants because the artist can act on the difference — one is
/// "there is too much of it" and the other is a figure that is not a figure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotRecorded {
    /// The encoded record is longer than [`MAX_RECORD_BYTES`].
    TooLarge,
    /// A figure that cannot be written or could not be read back: a zero scale,
    /// an infinite size, an empty source rectangle.
    Impossible,
}

impl NotRecorded {
    /// The clause that goes inside the save warning's sentence. Exhaustive, so a
    /// third reason cannot arrive without words for it.
    pub fn reason(self) -> &'static str {
        match self {
            Self::TooLarge => "there is too much text on it to record",
            Self::Impossible => "its size or placement is not a figure Umber can record",
        }
    }
}

/// Why a record could not be read.
///
/// Typed for the reason [`crate::ImportWarning`] is: every one of these becomes
/// a sentence about one layer, and the sentences differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordError {
    /// Longer than [`MAX_RECORD_BYTES`].
    TooLarge,
    /// Not the JSON this module writes.
    Unreadable(String),
    /// Written in a revision this build does not read. Discarded rather than
    /// refused — see [`VERSION`].
    NewerVersion { version: u32, supported: u32 },
    /// A figure that is not a figure: a zero scale, a non-finite offset, an
    /// empty source rectangle. Refused rather than clamped, for the reason
    /// [`Placement::is_sane`] gives.
    Nonsense,
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "its record of the text is larger than Umber will read"),
            Self::Unreadable(detail) => {
                write!(f, "the record of the text could not be read ({detail})")
            }
            Self::NewerVersion { version, supported } => write!(
                f,
                "the text was recorded in a newer form than this build reads \
                 (text {version}, this build reads up to {supported})"
            ),
            Self::Nonsense => write!(f, "the record of the text does not describe a placement"),
        }
    }
}

/// The stable name for an [`Align`] in a record.
///
/// An exhaustive `match` rather than `format!("{:?}")`, which is what
/// `docformat::blend_id` and `history::kind_id` do. The derive is right there
/// and it is the wrong tool here for one reason: those two are `pub` names an
/// older file already carries, where this one is new, so there is nothing to be
/// bug-compatible with and everything to gain from the compiler refusing to
/// build when a variant is added without a name being chosen for it.
///
/// **What is written is a format and not a name.** `the_names_written_into_a_
/// text_record_are_these_exact_strings` spells the set out as literal text,
/// which is what catches a rename; a round trip against [`align_from_id`]
/// cannot, because both sides would move together.
pub fn align_id(align: Align) -> &'static str {
    match align {
        Align::Left => "left",
        Align::Centre => "centre",
        Align::Right => "right",
    }
}

/// Inverse of [`align_id`]. `None` for a name this build does not know.
pub fn align_from_id(id: &str) -> Option<Align> {
    Align::ALL.into_iter().find(|a| align_id(*a) == id)
}

/// A colour as a record spells it: `#rrggbbaa`, sRGB.
///
/// Eight digits rather than [`crate::docformat::background_id`]'s six, and the
/// difference is real rather than a variation on a theme: a document background
/// is only ever opaque, and text set in a colour the artist dialled to half
/// opacity is half-opacity text, because `Setting::clip` multiplies the coverage
/// by the colour's own alpha. Dropping the alpha here would re-render a faint
/// caption solid.
///
/// Eight-bit sRGB rather than linear floats for the reason a [`crate::Swatch`]
/// is: it is the only form in which the value is exactly what a colour picker
/// showed, and the pair is exactly invertible.
pub fn colour_id(colour: Color) -> String {
    let [r, g, b, a] = colour.to_srgb_u8();
    format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
}

/// Inverse of [`colour_id`]. `None` for anything malformed, which drops the
/// record rather than re-rendering in a colour guessed out of a broken string.
pub fn colour_from_id(id: &str) -> Option<Color> {
    let hex = id.strip_prefix('#')?;
    if hex.len() != 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color::from_srgb_u8(byte(0)?, byte(2)?, byte(4)?, byte(6)?))
}

/// The record as the archive holds it.
///
/// A struct of its own rather than serde on [`TextObject`], for the reason
/// `docformat::history::Manifest` is one: **the field names are the file
/// format**, so they are stated once, here, where a test can pin them as
/// literal text — and a field renamed in the model cannot silently rename
/// itself on disk.
#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    text: String,
    family: String,
    style: String,
    /// Empty where the face carried none, and then absent from the file — so a
    /// record for a font with no PostScript name is byte for byte what it would
    /// have been before this field existed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    postscript: String,
    size: f32,
    line_spacing: f32,
    tracking: f32,
    /// [`align_id`].
    align: String,
    /// [`colour_id`].
    colour: String,
    /// The placement's source rectangle: `x`, `y`, `width`, `height`.
    source: [u32; 4],
    offset: [f32; 2],
    scale: [f32; 2],
    angle: f32,
    /// The fingerprint's rectangle, the same four figures in the same order.
    print_rect: [u32; 4],
    /// [`hash_bytes`] as sixteen lowercase hex digits.
    ///
    /// A string rather than a JSON number, because a `u64` past 2^53 is a number
    /// some JSON readers cannot carry exactly and this one is a bit pattern
    /// rather than a quantity. Nothing but Umber reads this entry, so the choice
    /// costs nothing and removes a way for the comparison to be wrong on a
    /// machine nobody here has.
    print_hash: String,
}

impl Record {
    fn of(obj: &TextObject, print: &Fingerprint) -> Self {
        let p = &obj.placement;
        Self {
            version: VERSION,
            text: obj.block.text.clone(),
            // Cleaned on the way out as well as on the way in, so what a notice
            // shows is what a reopen gives back. A font's own name tables are not
            // this module's to trust either: they are a file somebody else wrote.
            family: clean_name(&obj.face.family),
            style: clean_name(&obj.face.style),
            postscript: clean_name(&obj.face.postscript),
            size: obj.block.size,
            line_spacing: obj.block.line_spacing,
            tracking: obj.block.tracking,
            align: align_id(obj.block.align).to_string(),
            colour: colour_id(obj.colour),
            source: rect_array(p.source),
            offset: p.offset,
            scale: p.scale,
            angle: p.angle,
            print_rect: rect_array(print.rect),
            print_hash: format!("{:016x}", print.hash),
        }
    }

    fn into_object(self) -> Result<(TextObject, Fingerprint), RecordError> {
        let align = align_from_id(&self.align).ok_or(RecordError::Nonsense)?;
        let colour = colour_from_id(&self.colour).ok_or(RecordError::Nonsense)?;
        let hash = u64::from_str_radix(&self.print_hash, 16).map_err(|_| RecordError::Nonsense)?;
        let placement = Placement {
            source: array_rect(self.source),
            offset: self.offset,
            scale: self.scale,
            angle: self.angle,
        };
        // Every figure, before any of them is used. A record is a file somebody
        // else wrote, and the three that reach arithmetic — a zero scale, a
        // non-finite offset, an empty rectangle — each end somewhere that cannot
        // report a failure. Refused whole, which is this module's one rule.
        if !placement.is_sane()
            || !self.size.is_finite()
            || !self.line_spacing.is_finite()
            || !self.tracking.is_finite()
        {
            return Err(RecordError::Nonsense);
        }
        Ok((
            TextObject {
                block: TextBlock {
                    text: self.text,
                    size: self.size,
                    line_spacing: self.line_spacing,
                    tracking: self.tracking,
                    align,
                },
                face: TextFace {
                    family: clean_name(&self.family),
                    style: clean_name(&self.style),
                    postscript: clean_name(&self.postscript),
                },
                colour,
                placement,
            },
            Fingerprint {
                rect: array_rect(self.print_rect),
                hash,
            },
        ))
    }
}

/// A face name fit to put in a sentence: no control characters, trimmed, and no
/// longer than [`MAX_NAME_BYTES`].
///
/// The same rule `palette::clean_line` follows and for the same reason — what a
/// notice shows has to be what a save and a reopen give back, so it is applied on
/// both sides rather than at the point of drawing. A control character is not
/// whitespace, which is exactly the trap that once wrote a colour named `"\u{7}"`
/// back out as "Untitled palette".
///
/// Truncation is on a **character** boundary, because slicing a `String` at a
/// byte in the middle of one panics.
fn clean_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(MAX_NAME_BYTES));
    for c in name.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_NAME_BYTES {
            break;
        }
        out.push(c);
    }
    out.trim().to_string()
}

fn rect_array(rect: PixelRect) -> [u32; 4] {
    [rect.x, rect.y, rect.width, rect.height]
}

fn array_rect(a: [u32; 4]) -> PixelRect {
    PixelRect {
        x: a[0],
        y: a[1],
        width: a[2],
        height: a[3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Vec2, vec2};

    fn rect(x: u32, y: u32, w: u32, h: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn object() -> TextObject {
        TextObject::new(
            TextBlock {
                text: "Hello\nthere".into(),
                size: 48.0,
                line_spacing: 1.2,
                tracking: -0.5,
                align: Align::Centre,
            },
            TextFace {
                family: "Archivo".into(),
                style: "Bold".into(),
                postscript: "Archivo-Bold".into(),
            },
            Color::from_srgb_u8(12, 34, 56, 200),
            Placement {
                source: rect(10, 20, 100, 40),
                offset: [3.0, -4.0],
                scale: [1.5, 0.75],
                angle: 0.3,
            },
        )
    }

    /// The published FNV-1a 64 test vectors, so this pins the **algorithm** and
    /// not merely that the implementation agrees with itself. A hash that
    /// changed would silently drop every text layer in every saved document.
    #[test]
    fn the_fingerprints_hash_is_fnv_1a() {
        assert_eq!(hash_bytes(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash_bytes(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash_bytes(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let obj = object();
        let print = Fingerprint::of(rect(1, 2, 3, 4), b"pixels");
        let json = obj.to_json(&print).expect("well under the bound");
        let (back, back_print) = TextObject::from_json(&json).expect("its own output");
        assert_eq!(back, obj);
        assert_eq!(back_print, print);
    }

    /// **The field names and the value spellings are the file format**, so they
    /// are pinned as literal text. A rename in the model that took the file with
    /// it would drop every text layer in every document already on disk, and a
    /// round trip against the reader cannot catch it because both sides move
    /// together. Same guard `the_names_written_into_the_blend_attribute_are_
    /// these_exact_strings` is.
    #[test]
    fn the_names_written_into_a_text_record_are_these_exact_strings() {
        let json = String::from_utf8(
            object()
                .to_json(&Fingerprint::of(rect(1, 2, 3, 4), b"pixels"))
                .unwrap(),
        )
        .unwrap();
        for name in [
            "\"version\"",
            "\"text\"",
            "\"family\"",
            "\"style\"",
            "\"postscript\"",
            "\"size\"",
            "\"line_spacing\"",
            "\"tracking\"",
            "\"align\"",
            "\"colour\"",
            "\"source\"",
            "\"offset\"",
            "\"scale\"",
            "\"angle\"",
            "\"print_rect\"",
            "\"print_hash\"",
        ] {
            assert!(json.contains(name), "{name} is missing from {json}");
        }
        assert_eq!(align_id(Align::Left), "left");
        assert_eq!(align_id(Align::Centre), "centre");
        assert_eq!(align_id(Align::Right), "right");
        for align in Align::ALL {
            assert_eq!(align_from_id(align_id(align)), Some(align));
        }
        assert_eq!(align_from_id("middle"), None);

        // **`Align::ALL` is checked by an exhaustive match whose arms index it**,
        // not by walking it. `align_from_id` searches that array, so an alignment
        // added to the enum and not to `ALL` would be written to disk by
        // `align_id` — which is exhaustive and would force a name — and then
        // refused as `Nonsense` on every reopen, dropping the whole record. The
        // array lives in `text.rs`, which this change may not touch, so the guard
        // for it lives here beside the reader that depends on it.
        for align in Align::ALL {
            let at = match align {
                Align::Left => 0,
                Align::Centre => 1,
                Align::Right => 2,
            };
            assert_eq!(Align::ALL[at], align);
        }
    }

    /// A colour keeps its alpha, which the background's six-digit spelling
    /// deliberately drops. Re-rendering a half-opacity caption solid is the bug
    /// this exists to prevent.
    #[test]
    fn a_records_colour_keeps_the_alpha_the_text_was_set_at() {
        let colour = Color::from_srgb_u8(200, 100, 50, 128);
        assert_eq!(colour_id(colour), "#c8643280");
        assert_eq!(colour_from_id(&colour_id(colour)), Some(colour));
        assert_eq!(colour_from_id("#c86432"), None, "six digits is not this");
        assert_eq!(colour_from_id("c86432ff"), None, "no hash");
        assert_eq!(colour_from_id("#c86432zz"), None);
    }

    #[test]
    fn a_record_from_a_newer_revision_is_discarded_and_not_refused() {
        let print = Fingerprint::of(rect(0, 0, 1, 1), b"x");
        let mut json = String::from_utf8(object().to_json(&print).unwrap()).unwrap();
        json = json.replace("\"version\":1", "\"version\":2");
        assert_eq!(
            TextObject::from_json(json.as_bytes()),
            Err(RecordError::NewerVersion {
                version: 2,
                supported: VERSION,
            })
        );
    }

    /// A figure that is not a figure is refused whole, rather than clamped into
    /// something plausible: a zero scale is a matrix `Transform::inverse` cannot
    /// invert, and a non-finite offset reaches vertex positions.
    #[test]
    fn a_record_naming_an_impossible_placement_is_refused() {
        let print = Fingerprint::of(rect(0, 0, 1, 1), b"x");
        let good = String::from_utf8(object().to_json(&print).unwrap()).unwrap();
        for bad in [
            good.replace("\"scale\":[1.5,0.75]", "\"scale\":[0.0,0.75]"),
            good.replace("\"source\":[10,20,100,40]", "\"source\":[10,20,0,40]"),
            // Past what an `f32` can hold, which serde reads as an infinity
            // rather than as a failure: `1e39` is an ordinary JSON number and
            // the cast is what loses it. This is the one route a non-finite
            // figure has into a record, and it is why `is_finite` is checked
            // here rather than left to the parser.
            good.replace("\"size\":48.0", "\"size\":1e39"),
        ] {
            assert_eq!(
                TextObject::from_json(bad.as_bytes()).err(),
                Some(RecordError::Nonsense),
                "{bad}"
            );
        }
    }

    /// **The writer refuses exactly what the reader would**, and names which of
    /// the two reasons it was. A record written that the reader then declines is a
    /// text layer that stops being editable at the next open with the save having
    /// said nothing, which is the loss `MAX_RECORD_BYTES` exists to prevent.
    #[test]
    fn a_record_the_reader_would_refuse_is_not_written() {
        let print = Fingerprint::of(rect(0, 0, 1, 1), b"x");

        let mut long = object();
        long.block.text = "x".repeat(MAX_RECORD_BYTES + 1);
        assert_eq!(long.to_json(&print), Err(NotRecorded::TooLarge));

        // A figure that is not a figure, which `serde_json` refuses on its own
        // and used to come back as "too much text".
        let mut wild = object();
        wild.block.size = f32::INFINITY;
        assert_eq!(wild.to_json(&print), Err(NotRecorded::Impossible));

        // And a placement the reader's own `is_sane` would decline, which the
        // writer used to produce happily.
        let mut flat = object();
        flat.placement.source.width = 0;
        assert_eq!(flat.to_json(&print), Err(NotRecorded::Impossible));
        let mut still = object();
        still.placement.scale = [0.0, 1.0];
        assert_eq!(still.to_json(&print), Err(NotRecorded::Impossible));

        for why in [NotRecorded::TooLarge, NotRecorded::Impossible] {
            assert!(!why.reason().is_empty());
            assert!(!why.reason().contains('—'), "{}", why.reason());
        }
    }

    /// A face name out of a file somebody else wrote is cleaned to something that
    /// can go in a sentence, on the way in **and** on the way out, so what a
    /// notice shows is what a reopen gives back.
    ///
    /// **The two halves are driven separately, and that is the whole point of the
    /// test.** A round trip through `to_json` cleans on the way out, so it leaves
    /// the *reader's* clean unguarded — measured, by taking that one out: the round
    /// trip still passed. A record this build did not write, out of a hand-edited
    /// file or a later revision, is exactly the case where only the reader's half
    /// runs, so it gets its own raw JSON.
    #[test]
    fn a_face_name_from_a_hostile_file_is_cleaned_on_both_sides() {
        let print = Fingerprint::of(rect(0, 0, 1, 1), b"x");
        let dirty = format!("  Arch\u{7}ivo{}  ", "x".repeat(MAX_NAME_BYTES));
        let clean = |s: &str| {
            !s.contains('\u{7}') && s.len() <= MAX_NAME_BYTES && s.trim() == s && !s.is_empty()
        };

        // The writer's half: what goes into the archive is already fit to read.
        let mut obj = object();
        obj.face.family = dirty.clone();
        let written = String::from_utf8(obj.to_json(&print).unwrap()).unwrap();
        assert!(
            !written.contains("\\u0007"),
            "the record itself must be clean: {written}"
        );

        // The reader's half, on JSON this build did not write. Every field the
        // record needs, with a family nobody would want in a sentence.
        let raw = written.replace(
            &written[written.find("\"family\"").unwrap()..written.find("\"style\"").unwrap()],
            &format!("\"family\":{},", serde_json::to_string(&dirty).unwrap()),
        );
        let (back, _) = TextObject::from_json(raw.as_bytes()).expect("still a record");
        assert!(clean(&back.face.family), "{:?}", back.face.family);

        // And it is a fixed point: a second save and reopen gives the same face.
        let (again, _) = TextObject::from_json(&back.to_json(&print).unwrap()).unwrap();
        assert_eq!(again.face, back.face);
    }

    #[test]
    fn a_fingerprint_notices_a_changed_rectangle_and_changed_bytes() {
        let print = Fingerprint::of(rect(4, 5, 6, 7), b"the text's own pixels");
        assert!(print.matches(rect(4, 5, 6, 7), b"the text's own pixels"));
        assert!(
            !print.matches(rect(4, 5, 6, 8), b"the text's own pixels"),
            "a bounding box that moved is paint added to the layer"
        );
        assert!(!print.matches(rect(4, 5, 6, 7), b"the text's own pixelt"));
    }

    /// The whole of [`Placement::flipped`]'s claim, checked where it matters:
    /// **every point of the re-rendered text lands where the mirrored pixels
    /// are.**
    ///
    /// The comparison is over the map from the setting's own space, because that
    /// is what a re-render walks: `A'(source'.min + local)` against
    /// `mirror(A(source.min + local))`.
    #[test]
    fn a_flipped_placement_maps_every_point_where_the_mirror_put_it() {
        let canvas = UVec2::new(400, 300);
        let place = Placement {
            source: rect(30, 40, 120, 60),
            offset: [7.0, -11.0],
            scale: [1.3, -0.8],
            angle: 0.7,
        };
        for axis in [FlipAxis::Horizontal, FlipAxis::Vertical] {
            let flipped = place.flipped(axis, canvas).expect("inside the canvas");
            let a = place.transform().matrix();
            let b = flipped.transform().matrix();
            let from = Vec2::new(place.source.x as f32, place.source.y as f32);
            let to = Vec2::new(flipped.source.x as f32, flipped.source.y as f32);
            let size = Vec2::new(canvas.x as f32, canvas.y as f32);
            for local in [
                vec2(0.0, 0.0),
                vec2(120.0, 0.0),
                vec2(120.0, 60.0),
                vec2(0.0, 60.0),
                vec2(53.0, 17.0),
            ] {
                let want = axis.mirror(a.apply(from + local), size);
                let got = b.apply(to + local);
                assert!(
                    (want - got).length() < 1e-3,
                    "{axis:?} at {local}: wanted {want}, got {got}"
                );
            }
        }
    }

    /// Undoing a flip is another flip, so this has to be exact rather than
    /// close: any drift would compound every time somebody flipped and undid.
    #[test]
    fn flipping_a_placement_twice_puts_every_pixel_back() {
        let canvas = UVec2::new(400, 300);
        let place = Placement {
            source: rect(30, 40, 120, 60),
            offset: [7.0, -11.0],
            scale: [1.3, -0.8],
            angle: 0.7,
        };
        for axis in [FlipAxis::Horizontal, FlipAxis::Vertical] {
            let twice = place
                .flipped(axis, canvas)
                .and_then(|p| p.flipped(axis, canvas))
                .expect("inside the canvas");
            assert_eq!(twice, place, "{axis:?}");
        }
    }

    /// A source rectangle that is not inside the canvas cannot be mirrored into
    /// it, and a record that lies about where its pixels are is worse than none.
    ///
    /// **Both axes, whichever one is flipping.** Only the flipping axis needs
    /// mirroring, so refusing on the other one looks like an over-check and is
    /// not: `Clip::place` crops to the document, so a rectangle hanging off the
    /// canvas is one this writer never produced, and mirroring it would hand back
    /// a placement that is wrong on the axis nobody looked at.
    #[test]
    fn a_placement_outside_the_canvas_declines_to_mirror() {
        let canvas = UVec2::new(100, 100);
        let place = Placement::identity(rect(60, 10, 80, 10));
        assert_eq!(place.flipped(FlipAxis::Horizontal, canvas), None);
        assert_eq!(place.flipped(FlipAxis::Vertical, canvas), None);
        // And one that does fit mirrors on both.
        let inside = Placement::identity(rect(60, 10, 30, 10));
        assert!(inside.flipped(FlipAxis::Horizontal, canvas).is_some());
        assert!(inside.flipped(FlipAxis::Vertical, canvas).is_some());
    }

    /// A placement survives being turned into a [`Transform`] and read back, so
    /// the transform tool can pick text up and put it down again without moving
    /// it.
    #[test]
    fn a_placement_and_its_transform_are_the_same_thing() {
        let place = Placement {
            source: rect(3, 4, 50, 20),
            offset: [1.5, 2.5],
            scale: [-2.0, 0.5],
            angle: -0.25,
        };
        assert_eq!(Placement::of(&place.transform()), place);
    }

    /// **The resolution is exact, and never `FontLibrary::resolve`'s
    /// substitution.** A face this machine has not got freezes the text; one
    /// that merely differs in capitals does not, because the spelling in a file
    /// is whatever wrote it.
    #[test]
    fn a_face_that_is_not_here_resolves_to_nothing_rather_than_to_something_else() {
        let mut library = FontLibrary::default();
        library.add_builtin("test", crate::fonts::TEST_FONT);
        let here = library.faces().first().expect("the built-in face").clone();

        let exact = TextFace {
            family: here.family.clone(),
            style: here.style.clone(),
            postscript: String::new(),
        };
        assert_eq!(
            exact.resolve(&library).map(|f| f.style.clone()),
            Some(here.style.clone())
        );

        let cased = TextFace {
            family: here.family.to_uppercase(),
            style: here.style.to_lowercase(),
            postscript: String::new(),
        };
        assert!(cased.resolve(&library).is_some(), "capitals are not a loss");

        let gone = TextFace {
            family: "A Font Nobody Has".into(),
            style: "Regular".into(),
            postscript: "AFontNobodyHas-Regular".into(),
        };
        assert!(gone.resolve(&library).is_none());
        // And `FontLibrary::resolve` would have answered with something, which
        // is exactly the difference this test exists for.
        assert!(library.resolve(&gone.family, &gone.style).is_some());
    }

    #[test]
    fn a_missing_fonts_notice_names_it_and_claims_nothing_else() {
        let face = TextFace {
            family: "Archivo".into(),
            style: "Bold".into(),
            postscript: "Archivo-Bold".into(),
        };
        let notice = face.missing_notice();
        assert!(notice.contains("Archivo Bold"));
        assert!(notice.contains("Archivo-Bold"));
        assert!(
            !notice.contains('—') && !notice.contains("–"),
            "no em-dash in a notice: {notice}"
        );
    }

    /// The PostScript name comes off the face's own tables, and a font without
    /// one answers with nothing rather than with a guess.
    #[test]
    fn a_faces_postscript_name_is_read_from_the_font() {
        let font = FontRef::new(crate::fonts::TEST_FONT).expect("the built-in face parses");
        let name = postscript_name(&font);
        assert!(!name.is_empty(), "Archivo carries one");
        assert!(!name.contains(' '), "a PostScript name has no spaces");
    }
}
