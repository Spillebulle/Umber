//! The interface's icons.
//!
//! **The shapes are Lucide's, and they were not always.** Everything here used
//! to be drawn from primitives — the reason is below and still holds against a
//! *font* — and the set was Umber's own, hand-drawn mark by mark. The house
//! rule is now one stroke set across every application (`STYLE-GUIDE.md` §11),
//! and Umber was the application that predated it. This module is that
//! migration: where Lucide carries a mark, Lucide's geometry is what gets
//! drawn, copied out of the package verbatim and flattened by [`crate::lucide`]
//! — which also carries the licence and the argument for taking the path data
//! rather than an SVG file.
//!
//! A hand-drawn icon is only ever as good as the hour it got, and it cannot be
//! compared against anything. Taking the set makes Umber's settings mark and
//! Muster's the same mark, which is the whole point of a house style.
//!
//! ## Why not a font, still
//!
//! The obvious alternative — Unicode glyphs like a wastebasket or a ring —
//! works only for as long as the UI font happens to carry them. A text face
//! such as Archivo carries none of those symbols, so they silently become blank
//! boxes, and platform fallback would render them at a different weight and
//! size on Windows, Linux and Android. Drawn geometry keeps the set at the
//! stroke weight of the rest of the interface and independent of whatever font
//! is loaded. That argument is what put the set here in the first place and is
//! untouched by where the shapes come from.
//!
//! ## The four marks Umber still draws
//!
//! §11 allows an application to draw a mark the set does not carry, to Lucide's
//! own construction and in this module. [`Drawn`] is the whole list, and each
//! variant says what was searched for and not found. What that exception does
//! **not** cover is redrawing a mark Lucide already has: a variant that could
//! name a Lucide icon must, and `the_marks_umber_draws_itself_are_these_four`
//! is what keeps the list from growing by habit.
//!
//! It was five, and the layer mask is the one that left. This module argued
//! that Lucide carried no mask because the only thing in the set answering to
//! the word is `venetian-mask` — a search of the *word* rather than of the
//! picture, which is how a set of seventeen hundred marks hides one. `view` is
//! a frame with an eye inside it, which is a layer and how much of it shows
//! through. The rule above is what caught it; the lesson is that a search
//! coming back empty is evidence about the search.
//!
//! Icons are authored against a 24x24 box and scaled to whatever rect they are
//! given, so a 16 px and a 32 px instance are the same shape.

use crate::lucide::{Node, Outline};
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2, pos2};
use std::sync::LazyLock;

/// The side of the box every icon is drawn against. Lucide's own.
const BOX: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    // Tools
    /// Lucide `brush`.
    Brush,
    /// Lucide `eraser`.
    Eraser,
    /// Lucide `square-dashed`: the marquee, which is what a selection looks
    /// like on the canvas.
    Select,
    /// Lucide `vector-square`: a box with a handle at each corner, which is
    /// exactly what the tool draws on the canvas. Deliberately *not* the dashed
    /// square — the dashes belong to the selection, and a transform box is a
    /// different thing that happens to be the same shape.
    ///
    /// `scaling` is the obvious pick and was drawn here first. It is a rounded
    /// box with a diagonal arrow leaving its top right corner, and so is
    /// `external-link`, which [`Icon::Link`] wears: side by side on a sheet of
    /// the whole set the two are one mark. A collision like that is what
    /// looking at the set catches and no assertion would have.
    Transform,
    /// Lucide `hand`.
    Pan,
    /// Lucide `search`, which is the plain magnifier. Not `zoom-in`: its plus
    /// would name half of a tool that zooms both ways, and the magnifier is
    /// what stands for zoom everywhere else in the family.
    Zoom,
    // The transform tool's own marks, drawn over the canvas beside the box.
    /// Lucide `rotate-cw`: an arrow curving round. Drag outside the box to turn
    /// it.
    Rotate,
    /// Lucide `flip-horizontal-2`: two shapes either side of a dashed axis,
    /// mirrored left to right.
    FlipHorizontal,
    /// Lucide `flip-vertical-2`: the same, mirrored top to bottom.
    FlipVertical,
    // Layers
    /// Lucide `plus`.
    Plus,
    /// Lucide `trash-2`.
    Trash,
    /// Lucide `chevron-up`.
    ChevronUp,
    /// Lucide `chevron-down`.
    ChevronDown,
    /// Lucide `eye`.
    Eye,
    /// Lucide `eye-off`.
    EyeOff,
    /// Lucide `view`: a frame with an eye inside it. The layer, and how much of
    /// it shows through.
    ///
    /// Umber drew this itself — a frame with a solid disc in it — on the
    /// argument that the set carried no layer mask, which was a search of the
    /// word rather than of the picture; see the module docs. Lucide's mark is
    /// the same idea with an eye where the disc was, so it reads as a mask
    /// without having to be learnt, and it puts this and [`Icon::Eye`] in one
    /// family, which is what a layer's eye and its mask are.
    ///
    /// **The hand-drawn mark carried a legibility argument and it has been
    /// answered rather than deleted**, because the substitution is precisely
    /// what it warned against. It read: a frame plus a *solid* shape rather
    /// than two outlines, because at 16 px two nested rings read as a target.
    /// Lucide's mark is two nested outlines, and it is drawn at 14 in a brush
    /// chip and at [`crate::ui::ICON_BUTTON_MARK`]'s 12 in a header — both
    /// under the size that argument was made against. Looked at on
    /// `icon_sheet`, which now shoots 12 for this reason, it holds: the two
    /// outlines are a rounded **rectangle** and a **lens**, so they do not
    /// concentre, and the pupil is the solid mark in the middle that the disc
    /// used to be. Rings were the hazard, not outlines.
    Mask,
    /// Lucide `corner-left-down`: an arrow turning down and to the left, which
    /// is the mark every application uses for "bounded by the layer below".
    ///
    /// Umber drew this over a rule, on the argument that without one it is just
    /// a return arrow. The rule is gone with the rest of the hand-drawn set:
    /// the arrow alone is what Photoshop and Krita put on a clipped layer, and
    /// there is no other arrow in this interface for it to be confused with.
    Clip,
    /// Lucide `lock`: a closed padlock.
    Lock,
    /// Lucide `lock-open`: the same padlock with its shackle open. A second
    /// icon rather than the first drawn dim: dim means "unavailable" everywhere
    /// else in the interface, and a lock that is merely *off* is very much
    /// available.
    Unlock,
    /// Lucide `link`: two chain links. These layers move together.
    ///
    /// Lucide's name for this is the one [`Icon::Link`] wears here, and the two
    /// are different marks — that one is `external-link`. The doc comment
    /// carries the Lucide name and the variant keeps the name the interface
    /// uses, because renaming either would make the pair harder to find than
    /// the crossover makes them.
    Chain,
    // Chrome
    /// Lucide `x`.
    Close,
    /// Lucide `pencil`.
    Pencil,
    /// Lucide `settings`: the cog.
    Gear,
    /// Lucide `check`.
    Check,
    // Brush library
    /// Lucide `layout-grid`: four cells, "show me the whole set", against the
    /// single column the Brushes panel has room for.
    Grid,
    /// Lucide `import`: an arrow dropping into a tray. The tray is what
    /// separates this from [`Icon::Download`] — the file is coming *into* a
    /// collection that already exists.
    Import,
    // About and updates
    /// Lucide `external-link`: an arrow leaving a box. This opens somewhere
    /// outside Umber. See [`Icon::Chain`] for why the names cross over.
    Link,
    /// Lucide `download`: an arrow onto a line. Fetch this.
    Download,
    // Layout
    /// Lucide `grip-vertical`: two columns of dots, the universal "drag me".
    Grip,
    /// Stepped diagonals in the bottom right: the corner a panel is resized by.
    /// See [`Drawn::Corner`].
    Corner,
    // Picker
    /// Lucide `contrast`: a disc filled down one side, which is the half-filled
    /// disc this mark already was. It is what Muster draws for a theme; here it
    /// opens the colour picker, and the shape says "two halves of a colour" in
    /// both places, which is what a shared set is for.
    HalfCircle,
    // History
    /// Lucide `file`: a sheet with its corner turned down, the document itself
    /// as opposed to something done to it.
    Document,
    // Added at the end deliberately — this enum is shared, and renumbering it
    // would be a merge that compiles and draws the wrong marks.
    //
    // There used to be a `BrushNew` here, a brush with a plus beside it,
    // because `Plus` alone meant "save what is in your hand" in the Brushes
    // panel's header and nowhere else in the interface. That header's plus
    // makes a brush now, which is what `Plus` means on the Layers header, the
    // Palette header and the tab strip, so the mark that existed to tell the
    // two apart had nothing left to say and is gone rather than left undrawn.
    /// Lucide `triangle-alert` — the crash box's mark, and the only place in
    /// the interface that something has gone irrecoverably wrong. A triangle
    /// rather than a circle: a circled `i` is information and a circled `!` is
    /// a warning somebody can carry on past, and this is neither.
    Alert,
    // The selection's own strip of controls, drawn over the canvas beside a
    // marquee. Added at the end for the reason the marks above were.
    /// Lucide `copy`: two sheets, one behind the other. Take a copy and leave
    /// the original.
    Copy,
    /// Lucide `scissors`: take it and leave nothing.
    Cut,
    /// [`Icon::Select`]'s dashed box with a stroke through it — not the box
    /// greyed, and not [`Icon::Close`]'s cross: this clears one specific thing,
    /// and the mark has to say *which*. See [`Drawn::Deselect`].
    Deselect,
    // Layer folders. At the end for the reason the brush marks above are: this
    // enum is shared, and renumbering it would be a merge that compiles and
    // draws the wrong marks.
    /// Lucide `folder`, as a layer group's row mark. Drawn where a layer's row
    /// draws its thumbnail — a folder has no picture of its own, and one of an
    /// arbitrary child would be a picture that lies about what is inside.
    Folder,
    /// Lucide `chevron-right`: this folder is shut. Its pair is
    /// [`Icon::ChevronDown`], which already exists and already points the way a
    /// disclosure open should.
    ChevronRight,
    // What a new selection does to the one already standing. At the end for
    // the reason the folder marks above are: this enum is shared, and
    // renumbering it would be a merge that compiles and draws the wrong marks.
    //
    // These four were a hand-drawn motif — a pair of overlapping squares with
    // the *result* filled in — because being one motif they could only be told
    // apart by that fill. Lucide carries the whole family and tells them apart
    // by the outline itself, which is a stroke set's answer to the same
    // problem and needs no fill to read at 16 px.
    /// Lucide `square`: the new shape becomes the selection. Deliberately a
    /// single square where its three neighbours are a pair, because replacing
    /// is the one of the four that does not involve what was already there.
    SelectReplace,
    /// Lucide `squares-unite`: the union, drawn as the one outline the two
    /// squares make together.
    SelectAdd,
    /// Lucide `squares-subtract`: the first square with the second taken out of
    /// it, the rest of the second left as a hint of where it was.
    SelectSubtract,
    /// Lucide `squares-intersect`: the overlap drawn whole, the two squares it
    /// came from left as corners.
    SelectIntersect,
    // The stamps and papers a brush is made of. At the end for the reason every
    // mark above it is: this enum is shared, and renumbering it would be a
    // merge that compiles and draws the wrong icons.
    /// Lucide `images`: a stack of pictures — the pictures a brush paints
    /// *through*, bitmap stamps and paper tiles. Distinct from [`Icon::Grid`],
    /// which means "the whole set of brushes"; this one is the set of pictures.
    Stamps,
    /// Lucide `text-cursor-input`: a caret between two fields. Change what this
    /// is *called*.
    ///
    /// Deliberately not a second pencil. [`Icon::Pencil`] means "open the brush
    /// editor" — in the Brushes panel header and on every row of the library
    /// browser — and a row that drew a pencil for renaming and a pencil for
    /// editing would be two marks with one meaning and two outcomes.
    Rename,
    // Text. At the end for the reason every mark above it is: this enum is
    // shared, and renumbering it would be a merge that compiles and draws the
    // wrong marks.
    /// Lucide `type`: the serifed `T` every application uses for text.
    ///
    /// Deliberately *drawn* rather than the letter set in Archivo. A glyph
    /// would be the one icon in the interface whose weight and proportions came
    /// from the font rather than from the stroke weight beside it, and it would
    /// change shape the day the interface changed typeface. Lucide's letter is
    /// geometry, so it is a drawn mark in exactly that sense.
    Text,
    // Structural undo's two rows. At the end for the reason every group above
    // is: this enum is shared, and renumbering it would be a merge that
    // compiles and draws the wrong marks.
    /// Lucide `chevrons-up-down`: two chevrons back to back, pointing apart —
    /// an entry moved in the stack. [`Icon::ChevronUp`] alone would say "moved
    /// up" on a row that may have moved down, and [`Icon::Grip`] is the drag
    /// *handle* rather than the act.
    MoveLayer,
    /// [`Icon::Mask`] with a stroke through it — the relationship
    /// [`Icon::Deselect`] already has to [`Icon::Select`], and for the same
    /// reason: this takes one specific thing off, and the mark has to say
    /// which. See [`Drawn::MaskOff`].
    ///
    /// Lucide has no `view-off`, read off the 1.34.0 git tree rather than
    /// assumed: `view` is the only mark in the whole set carrying that name,
    /// and none of the eighty-two `-off` twins it ships is one of them.
    /// `eye-off` is the near miss and is the wrong mark — it negates the eye
    /// alone, where this negates the frame with the eye in it. So this stays
    /// drawn, as [`Icon::Mask`]'s own geometry under Lucide's own
    /// `m2 2 20 20`.
    ///
    /// **The tree, not the contents API**, which pages at a thousand entries
    /// against Lucide's 1,777 and answered this question with sixty-two of the
    /// twins missing. A listing that comes back short looks exactly like a
    /// listing that came back complete, which is the same failure as the search
    /// that put [`Icon::Mask`] here in the first place.
    MaskOff,
    /// A colour wheel with three of its hues marked: the set of related colours
    /// the Colour panel's Harmony mode is showing, kept. See
    /// [`Drawn::Harmony`].
    ///
    /// Appended rather than filed beside [`Icon::Grid`] for the reason the two
    /// above give: this enum is shared, and inserting into the middle of it is
    /// a merge that compiles and draws the wrong marks.
    Harmony,
    // The Text module's two style controls. At the end for the reason every
    // group above is: this enum is shared, and renumbering it would be a merge
    // that compiles and draws the wrong marks.
    /// Lucide `bold`. The weight is the content of the mark, not the letter:
    /// set this text in the family's own bold.
    ///
    /// A letter rather than an abstraction, for the one reason that overrides
    /// this interface's usual taste for a symbol: `B` and `I` are what every
    /// application on every platform draws for these two, so a mark somebody
    /// had to learn would be worse. Lucide draws them as letters for the same
    /// reason, and as geometry rather than as type — see [`Icon::Text`].
    Bold,
    /// Lucide `italic`: a slanted stroke between two bars, the bars being what
    /// stops a lone oblique reading as a divider. See [`Icon::Bold`] for why
    /// these two are letters.
    Italic,
    /// Lucide `pipette`: the eyedropper tool. At the end for the reason every
    /// group above it is — this enum is shared, and inserting into the middle
    /// of it is a merge that compiles and draws the wrong marks.
    ///
    /// A pipette rather than the other common spelling, a dropper *over a
    /// swatch*: at 18 px the swatch is three pixels of flat colour and reads as
    /// a shadow under the mark. The pipette alone is what Photoshop, GIMP,
    /// Krita and Affinity all draw, so it is a shape somebody already knows.
    Eyedropper,
}

/// Where a mark's geometry comes from.
///
/// Two arms rather than an `Option<&[Node]>`, so that `drawn` below is an
/// exhaustive match over [`Drawn`] with no unreachable arm in it: the partial
/// exhaustiveness this codebase refuses everywhere else would otherwise arrive
/// here as a `_ => {}` that silently draws nothing.
enum Art {
    /// Lucide's, verbatim.
    Lucide(&'static [Node]),
    /// A mark Lucide does not carry.
    Drawn(Drawn),
}

/// The marks Umber draws itself, and what was searched for before each.
///
/// This is §11's first exception — "a mark the set does not carry" — and it is
/// the whole list. Adding to it means the search below it, written down, and
/// it may not be used for a mark Lucide has; see the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drawn {
    /// A layer mask with a stroke through it. Searched for `view-off`,
    /// `eye-off`, `mask`: the set carries no `-off` twin of `view`, and
    /// `eye-off` negates the eye rather than the frame around it, so a pair
    /// made of `view` and `eye-off` would be two objects rather than one with
    /// something done to it.
    MaskOff,
    /// The marquee with a stroke through it. Lucide has `square-slash`, and its
    /// box is *solid*: the pair has to read as the same object with something
    /// done to it, and [`Icon::Select`] is `square-dashed`.
    Deselect,
    /// The stepped diagonals in a panel's bottom right corner. Searched for
    /// `resize`, `corner`, `grip`: `move-diagonal` is a two-headed arrow naming
    /// the gesture, where this is the *texture* of the corner itself, drawn
    /// under the pointer rather than as a control. An arrow there would read as
    /// a button.
    Corner,
    /// A colour wheel with three hues marked. Searched for `harmony`, `triad`,
    /// `color-wheel`: `palette` is a painter's palette and means "colours",
    /// where this has to mean "these colours are related round the wheel", and
    /// `blend` is two discs overlapping.
    Harmony,
}

impl Icon {
    /// Every icon, which is what the tests walk.
    ///
    /// Hand-written and therefore checkable rather than self-evident: a test
    /// indexes it from an exhaustive match, so a variant added without a row
    /// here is a compile error rather than an icon nothing ever looks at.
    pub const ALL: [Self; 51] = [
        Self::Brush,
        Self::Eraser,
        Self::Select,
        Self::Transform,
        Self::Pan,
        Self::Zoom,
        Self::Rotate,
        Self::FlipHorizontal,
        Self::FlipVertical,
        Self::Plus,
        Self::Trash,
        Self::ChevronUp,
        Self::ChevronDown,
        Self::Eye,
        Self::EyeOff,
        Self::Mask,
        Self::Clip,
        Self::Lock,
        Self::Unlock,
        Self::Chain,
        Self::Close,
        Self::Pencil,
        Self::Gear,
        Self::Check,
        Self::Grid,
        Self::Import,
        Self::Link,
        Self::Download,
        Self::Grip,
        Self::Corner,
        Self::HalfCircle,
        Self::Document,
        Self::Alert,
        Self::Copy,
        Self::Cut,
        Self::Deselect,
        Self::Folder,
        Self::ChevronRight,
        Self::SelectReplace,
        Self::SelectAdd,
        Self::SelectSubtract,
        Self::SelectIntersect,
        Self::Stamps,
        Self::Rename,
        Self::Text,
        Self::MoveLayer,
        Self::MaskOff,
        Self::Harmony,
        Self::Bold,
        Self::Italic,
        Self::Eyedropper,
    ];

    /// The icon's elements, copied from Lucide 1.34.0 unchanged.
    ///
    /// Updating one is a paste over one line; adding one is a paste of a new
    /// arm, and the name in the doc comment on the variant is the name to
    /// search lucide.dev for. Nothing here reorders, simplifies or nudges what
    /// the package says, which is what makes an icon comparable against the
    /// site character by character.
    fn art(self) -> Art {
        match self {
            // brush
            Self::Brush => Art::Lucide(&[
                Node::Path("m11 10 3 3"),
                Node::Path(
                    "M6.5 21A3.5 3.5 0 1 0 3 17.5a2.62 2.62 0 0 1-.708 1.792A1 1 0 0 0 3 21z",
                ),
                Node::Path("M9.969 17.031 21.378 5.624a1 1 0 0 0-3.002-3.002L6.967 14.031"),
            ]),
            // eraser
            Self::Eraser => Art::Lucide(&[
                Node::Path(
                    "M21 21H8a2 2 0 0 1-1.42-.587l-3.994-3.999a2 2 0 0 1 0-2.828l10-10a2 2 0 0 1 2.829 0l5.999 6a2 2 0 0 1 0 2.828L12.834 21",
                ),
                Node::Path("m5.082 11.09 8.828 8.828"),
            ]),
            // square-dashed
            Self::Select => Art::Lucide(&[
                Node::Path("M5 3a2 2 0 0 0-2 2"),
                Node::Path("M19 3a2 2 0 0 1 2 2"),
                Node::Path("M21 19a2 2 0 0 1-2 2"),
                Node::Path("M5 21a2 2 0 0 1-2-2"),
                Node::Path("M9 3h1"),
                Node::Path("M9 21h1"),
                Node::Path("M14 3h1"),
                Node::Path("M14 21h1"),
                Node::Path("M3 9v1"),
                Node::Path("M21 9v1"),
                Node::Path("M3 14v1"),
                Node::Path("M21 14v1"),
            ]),
            // vector-square
            Self::Transform => Art::Lucide(&[
                Node::Path("M19.5 7a24 24 0 0 1 0 10"),
                Node::Path("M4.5 7a24 24 0 0 0 0 10"),
                Node::Path("M7 19.5a24 24 0 0 0 10 0"),
                Node::Path("M7 4.5a24 24 0 0 1 10 0"),
                Node::Rect {
                    x: 17.0,
                    y: 17.0,
                    w: 5.0,
                    h: 5.0,
                    r: 1.0,
                },
                Node::Rect {
                    x: 17.0,
                    y: 2.0,
                    w: 5.0,
                    h: 5.0,
                    r: 1.0,
                },
                Node::Rect {
                    x: 2.0,
                    y: 17.0,
                    w: 5.0,
                    h: 5.0,
                    r: 1.0,
                },
                Node::Rect {
                    x: 2.0,
                    y: 2.0,
                    w: 5.0,
                    h: 5.0,
                    r: 1.0,
                },
            ]),
            // hand
            Self::Pan => Art::Lucide(&[
                Node::Path("M18 11V6a2 2 0 0 0-2-2a2 2 0 0 0-2 2"),
                Node::Path("M14 10V4a2 2 0 0 0-2-2a2 2 0 0 0-2 2v2"),
                Node::Path("M10 10.5V6a2 2 0 0 0-2-2a2 2 0 0 0-2 2v8"),
                Node::Path(
                    "M18 8a2 2 0 1 1 4 0v6a8 8 0 0 1-8 8h-2c-2.8 0-4.5-.86-5.99-2.34l-3.6-3.6a2 2 0 0 1 2.83-2.82L7 15",
                ),
            ]),
            // search
            Self::Zoom => Art::Lucide(&[
                Node::Path("m21 21-4.34-4.34"),
                Node::Circle {
                    cx: 11.0,
                    cy: 11.0,
                    r: 8.0,
                },
            ]),
            // rotate-cw
            Self::Rotate => Art::Lucide(&[
                Node::Path("M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8"),
                Node::Path("M21 3v5h-5"),
            ]),
            // flip-horizontal-2
            Self::FlipHorizontal => Art::Lucide(&[
                Node::Path("m3 7 5 5-5 5V7"),
                Node::Path("m21 7-5 5 5 5V7"),
                Node::Path("M12 20v2"),
                Node::Path("M12 14v2"),
                Node::Path("M12 8v2"),
                Node::Path("M12 2v2"),
            ]),
            // flip-vertical-2
            Self::FlipVertical => Art::Lucide(&[
                Node::Path("m17 3-5 5-5-5h10"),
                Node::Path("m17 21-5-5-5 5h10"),
                Node::Path("M4 12H2"),
                Node::Path("M10 12H8"),
                Node::Path("M16 12h-2"),
                Node::Path("M22 12h-2"),
            ]),
            // plus
            Self::Plus => Art::Lucide(&[Node::Path("M5 12h14"), Node::Path("M12 5v14")]),
            // trash-2
            Self::Trash => Art::Lucide(&[
                Node::Path("M10 11v6"),
                Node::Path("M14 11v6"),
                Node::Path("M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"),
                Node::Path("M3 6h18"),
                Node::Path("M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"),
            ]),
            // chevron-up
            Self::ChevronUp => Art::Lucide(&[Node::Path("m18 15-6-6-6 6")]),
            // chevron-down
            Self::ChevronDown => Art::Lucide(&[Node::Path("m6 9 6 6 6-6")]),
            // eye
            Self::Eye => Art::Lucide(&[
                Node::Path(
                    "M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0",
                ),
                Node::Circle {
                    cx: 12.0,
                    cy: 12.0,
                    r: 3.0,
                },
            ]),
            // eye-off
            Self::EyeOff => Art::Lucide(&[
                Node::Path(
                    "M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575 1 1 0 0 1 0 .696 10.747 10.747 0 0 1-1.444 2.49",
                ),
                Node::Path("M14.084 14.158a3 3 0 0 1-4.242-4.242"),
                Node::Path(
                    "M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151 1 1 0 0 1 0-.696 10.75 10.75 0 0 1 4.446-5.143",
                ),
                Node::Path("m2 2 20 20"),
            ]),
            // view
            Self::Mask => Art::Lucide(&[
                Node::Path("M21 17v2a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-2"),
                Node::Path("M21 7V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v2"),
                Node::Circle {
                    cx: 12.0,
                    cy: 12.0,
                    r: 1.0,
                },
                Node::Path(
                    "M18.944 12.33a1 1 0 0 0 0-.66 7.5 7.5 0 0 0-13.888 0 1 1 0 0 0 0 .66 7.5 7.5 0 0 0 13.888 0",
                ),
            ]),
            // corner-left-down
            Self::Clip => Art::Lucide(&[
                Node::Path("m14 15-5 5-5-5"),
                Node::Path("M20 4h-7a4 4 0 0 0-4 4v12"),
            ]),
            // lock
            Self::Lock => Art::Lucide(&[
                Node::Rect {
                    x: 3.0,
                    y: 11.0,
                    w: 18.0,
                    h: 11.0,
                    r: 2.0,
                },
                Node::Path("M7 11V7a5 5 0 0 1 10 0v4"),
            ]),
            // lock-open
            Self::Unlock => Art::Lucide(&[
                Node::Rect {
                    x: 3.0,
                    y: 11.0,
                    w: 18.0,
                    h: 11.0,
                    r: 2.0,
                },
                Node::Path("M7 11V7a5 5 0 0 1 9.9-1"),
            ]),
            // link
            Self::Chain => Art::Lucide(&[
                Node::Path("M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"),
                Node::Path("M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"),
            ]),
            // x
            Self::Close => Art::Lucide(&[Node::Path("M18 6 6 18"), Node::Path("m6 6 12 12")]),
            // pencil
            Self::Pencil => Art::Lucide(&[
                Node::Path(
                    "M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z",
                ),
                Node::Path("m15 5 4 4"),
            ]),
            // settings
            Self::Gear => Art::Lucide(&[
                Node::Path(
                    "M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915",
                ),
                Node::Circle {
                    cx: 12.0,
                    cy: 12.0,
                    r: 3.0,
                },
            ]),
            // check
            Self::Check => Art::Lucide(&[Node::Path("M20 6 9 17l-5-5")]),
            // layout-grid
            Self::Grid => Art::Lucide(&[
                Node::Rect {
                    x: 3.0,
                    y: 3.0,
                    w: 7.0,
                    h: 7.0,
                    r: 1.0,
                },
                Node::Rect {
                    x: 14.0,
                    y: 3.0,
                    w: 7.0,
                    h: 7.0,
                    r: 1.0,
                },
                Node::Rect {
                    x: 14.0,
                    y: 14.0,
                    w: 7.0,
                    h: 7.0,
                    r: 1.0,
                },
                Node::Rect {
                    x: 3.0,
                    y: 14.0,
                    w: 7.0,
                    h: 7.0,
                    r: 1.0,
                },
            ]),
            // import
            Self::Import => Art::Lucide(&[
                Node::Path("M12 3v12"),
                Node::Path("m8 11 4 4 4-4"),
                Node::Path(
                    "M8 5H4a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-4",
                ),
            ]),
            // external-link
            Self::Link => Art::Lucide(&[
                Node::Path("M15 3h6v6"),
                Node::Path("M10 14 21 3"),
                Node::Path("M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"),
            ]),
            // download
            Self::Download => Art::Lucide(&[
                Node::Path("M12 15V3"),
                Node::Path("M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"),
                Node::Path("m7 10 5 5 5-5"),
            ]),
            // grip-vertical
            Self::Grip => Art::Lucide(&[
                Node::Circle {
                    cx: 9.0,
                    cy: 12.0,
                    r: 1.0,
                },
                Node::Circle {
                    cx: 9.0,
                    cy: 5.0,
                    r: 1.0,
                },
                Node::Circle {
                    cx: 9.0,
                    cy: 19.0,
                    r: 1.0,
                },
                Node::Circle {
                    cx: 15.0,
                    cy: 12.0,
                    r: 1.0,
                },
                Node::Circle {
                    cx: 15.0,
                    cy: 5.0,
                    r: 1.0,
                },
                Node::Circle {
                    cx: 15.0,
                    cy: 19.0,
                    r: 1.0,
                },
            ]),
            Self::Corner => Art::Drawn(Drawn::Corner),
            // contrast
            Self::HalfCircle => Art::Lucide(&[
                Node::Circle {
                    cx: 12.0,
                    cy: 12.0,
                    r: 10.0,
                },
                Node::Path("M12 18a6 6 0 0 0 0-12v12z"),
            ]),
            // file
            Self::Document => Art::Lucide(&[
                Node::Path(
                    "M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z",
                ),
                Node::Path("M14 2v5a1 1 0 0 0 1 1h5"),
            ]),
            // triangle-alert
            Self::Alert => Art::Lucide(&[
                Node::Path(
                    "m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3",
                ),
                Node::Path("M12 9v4"),
                Node::Path("M12 17h.01"),
            ]),
            // copy
            Self::Copy => Art::Lucide(&[
                Node::Rect {
                    x: 8.0,
                    y: 8.0,
                    w: 14.0,
                    h: 14.0,
                    r: 2.0,
                },
                Node::Path("M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"),
            ]),
            // scissors
            Self::Cut => Art::Lucide(&[
                Node::Circle {
                    cx: 6.0,
                    cy: 6.0,
                    r: 3.0,
                },
                Node::Path("M8.12 8.12 12 12"),
                Node::Path("M20 4 8.12 15.88"),
                Node::Circle {
                    cx: 6.0,
                    cy: 18.0,
                    r: 3.0,
                },
                Node::Path("M14.8 14.8 20 20"),
            ]),
            Self::Deselect => Art::Drawn(Drawn::Deselect),
            // folder
            Self::Folder => Art::Lucide(&[Node::Path(
                "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z",
            )]),
            // chevron-right
            Self::ChevronRight => Art::Lucide(&[Node::Path("m9 18 6-6-6-6")]),
            // square
            Self::SelectReplace => Art::Lucide(&[Node::Rect {
                x: 3.0,
                y: 3.0,
                w: 18.0,
                h: 18.0,
                r: 2.0,
            }]),
            // squares-unite
            Self::SelectAdd => Art::Lucide(&[Node::Path(
                "M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v3a1 1 0 0 0 1 1h3a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H10a2 2 0 0 1-2-2v-3a1 1 0 0 0-1-1z",
            )]),
            // squares-subtract
            Self::SelectSubtract => Art::Lucide(&[
                Node::Path("M10 22a2 2 0 0 1-2-2"),
                Node::Path("M16 22h-2"),
                Node::Path(
                    "M16 4a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h3a1 1 0 0 0 1-1v-5a2 2 0 0 1 2-2h5a1 1 0 0 0 1-1z",
                ),
                Node::Path("M20 8a2 2 0 0 1 2 2"),
                Node::Path("M22 14v2"),
                Node::Path("M22 20a2 2 0 0 1-2 2"),
            ]),
            // squares-intersect
            Self::SelectIntersect => Art::Lucide(&[
                Node::Path("M10 22a2 2 0 0 1-2-2"),
                Node::Path("M14 2a2 2 0 0 1 2 2"),
                Node::Path("M16 22h-2"),
                Node::Path("M2 10V8"),
                Node::Path("M2 4a2 2 0 0 1 2-2"),
                Node::Path("M20 8a2 2 0 0 1 2 2"),
                Node::Path("M22 14v2"),
                Node::Path("M22 20a2 2 0 0 1-2 2"),
                Node::Path("M4 16a2 2 0 0 1-2-2"),
                Node::Path("M8 10a2 2 0 0 1 2-2h5a1 1 0 0 1 1 1v5a2 2 0 0 1-2 2H9a1 1 0 0 1-1-1z"),
                Node::Path("M8 2h2"),
            ]),
            // images
            Self::Stamps => Art::Lucide(&[
                Node::Path("m22 11-1.296-1.296a2.4 2.4 0 0 0-3.408 0L11 16"),
                Node::Path("M4 8a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2"),
                Node::Disc {
                    cx: 13.0,
                    cy: 7.0,
                    r: 1.0,
                },
                Node::Rect {
                    x: 8.0,
                    y: 2.0,
                    w: 14.0,
                    h: 14.0,
                    r: 2.0,
                },
            ]),
            // text-cursor-input
            Self::Rename => Art::Lucide(&[
                Node::Path("M12 20h-1a2 2 0 0 1-2-2 2 2 0 0 1-2 2H6"),
                Node::Path("M13 8h7a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2h-7"),
                Node::Path("M5 16H4a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2h1"),
                Node::Path("M6 4h1a2 2 0 0 1 2 2 2 2 0 0 1 2-2h1"),
                Node::Path("M9 6v12"),
            ]),
            // type
            Self::Text => Art::Lucide(&[
                Node::Path("M12 4v16"),
                Node::Path("M4 7V5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v2"),
                Node::Path("M9 20h6"),
            ]),
            // chevrons-up-down
            Self::MoveLayer => {
                Art::Lucide(&[Node::Path("m7 15 5 5 5-5"), Node::Path("m7 9 5-5 5 5")])
            }
            Self::MaskOff => Art::Drawn(Drawn::MaskOff),
            Self::Harmony => Art::Drawn(Drawn::Harmony),
            // bold
            Self::Bold => Art::Lucide(&[Node::Path(
                "M6 12h9a4 4 0 0 1 0 8H7a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h7a4 4 0 0 1 0 8",
            )]),
            // italic
            Self::Italic => Art::Lucide(&[
                Node::Line {
                    x1: 19.0,
                    y1: 4.0,
                    x2: 10.0,
                    y2: 4.0,
                },
                Node::Line {
                    x1: 14.0,
                    y1: 20.0,
                    x2: 5.0,
                    y2: 20.0,
                },
                Node::Line {
                    x1: 15.0,
                    y1: 4.0,
                    x2: 9.0,
                    y2: 20.0,
                },
            ]),
            // pipette
            Self::Eyedropper => Art::Lucide(&[
                Node::Path(
                    "m12 9-8.414 8.414A2 2 0 0 0 3 18.828v1.344a2 2 0 0 1-.586 1.414A2 2 0 0 1 3.828 21h1.344a2 2 0 0 0 1.414-.586L15 12",
                ),
                Node::Path(
                    "m18 9 .4.4a1 1 0 1 1-3 3l-3.8-3.8a1 1 0 1 1 3-3l.4.4 3.4-3.4a1 1 0 1 1 3 3z",
                ),
                Node::Path("m2 22 .414-.414"),
            ]),
        }
    }
}

/// Every Lucide icon's outlines, flattened once.
///
/// The parse and the arcs are cheap, but they are the same answer every frame
/// for every row of the layer list, and an icon set is drawn some dozens of
/// times a frame. Held in the 24x24 box and scaled at the point of drawing, so
/// one copy serves every size. `None` is a [`Drawn`] mark, which has no
/// geometry to hold.
static GEOMETRY: LazyLock<Vec<Option<Vec<Outline>>>> = LazyLock::new(|| {
    Icon::ALL
        .iter()
        .map(|i| match i.art() {
            Art::Lucide(nodes) => Some(crate::lucide::flatten(nodes)),
            Art::Drawn(_) => None,
        })
        .collect()
});

fn geometry(icon: Icon) -> Option<&'static [Outline]> {
    let at = Icon::ALL
        .iter()
        .position(|i| *i == icon)
        .expect("every icon is in ALL");
    GEOMETRY[at].as_deref()
}

/// Draw `icon` centred in `rect`.
///
/// Stroke weight scales with the box so small icons stay legible and large
/// ones do not turn spindly.
pub fn draw(painter: &Painter, rect: Rect, icon: Icon, colour: Color32) {
    let size = rect.width().min(rect.height());
    if size <= 1.0 {
        return;
    }
    let scale = size / BOX;
    let origin = rect.center() - Vec2::splat(size * 0.5);
    let stroke = Stroke::new((2.0 * scale).max(1.0), colour);

    let Some(outlines) = geometry(icon) else {
        let Art::Drawn(mark) = icon.art() else {
            unreachable!("geometry answers None only for a drawn mark");
        };
        drawn(painter, origin, scale, stroke, colour, mark);
        return;
    };

    outlines_at(painter, origin, scale, stroke, colour, outlines);
}

/// Stroke flattened 24x24 geometry into the box at `origin`.
///
/// Its own function because [`Drawn::Deselect`] draws [`Icon::Select`]'s
/// outlines and then a stroke through them: the negated mark is the mark it
/// negates *plus* something, rather than a second drawing of the same box that
/// has to be kept in step with it by hand.
fn outlines_at(
    painter: &Painter,
    origin: Pos2,
    scale: f32,
    stroke: Stroke,
    colour: Color32,
    outlines: &[Outline],
) {
    for outline in outlines {
        let points: Vec<_> = outline
            .points
            .iter()
            .map(|p| pos2(origin.x + p.x * scale, origin.y + p.y * scale))
            .collect();
        if outline.is_dot() {
            // A dot is a stroke that goes nowhere, and egui would draw nothing
            // at all. Half the stroke's width is the radius SVG's round cap
            // would have given it.
            painter.circle_filled(points[0], stroke.width / 2.0, colour);
        } else if outline.filled {
            painter.add(Shape::convex_polygon(points, colour, Stroke::NONE));
        } else if outline.closed {
            painter.add(Shape::closed_line(points, stroke));
        } else {
            painter.add(Shape::line(points, stroke));
        }
    }
}

/// The four marks Lucide does not carry, drawn to its construction: the same
/// 24x24 box, the same two-unit stroke, the same round joins.
///
/// `origin` is the box's top left in screen space and `scale` takes a unit of
/// the box to a pixel, which is exactly what the loop above works in — so a
/// drawn mark and a pasted one cannot end up at different sizes.
fn drawn(
    painter: &Painter,
    origin: Pos2,
    scale: f32,
    stroke: Stroke,
    colour: Color32,
    mark: Drawn,
) {
    let at = |x: f32, y: f32| pos2(origin.x + x * scale, origin.y + y * scale);
    let line = |a: Pos2, b: Pos2| painter.line_segment([a, b], stroke);

    // Lucide's own way of saying a thing is off: `eye-off` and `pen-off` both
    // draw the mark and then `m2 2 20 20` across it. Both marks below negate
    // one that is on the sheet beside them, so they take that stroke rather
    // than an angle of their own.
    let slash = || line(at(2.0, 2.0), at(22.0, 22.0));

    match mark {
        Drawn::MaskOff => {
            // `Icon::Mask`'s own outlines, for the reason `Drawn::Deselect`
            // takes `Icon::Select`'s: the negated mark has to be the mark it
            // negates *plus* something. A second drawing of `view` here would
            // be a copy to keep in step with the one on the sheet beside it,
            // which is what the hand-drawn pair shared a `mask` function to
            // avoid; reaching for the geometry is that guarantee with nothing
            // left to maintain.
            let frame = geometry(Icon::Mask).expect("`Mask` is a Lucide mark");
            outlines_at(painter, origin, scale, stroke, colour, frame);
            slash();
        }
        Drawn::Deselect => {
            // `Icon::Select`'s own outlines, so the two cannot drift: this is
            // the marquee with a stroke through it, and the marquee is Lucide's
            // `square-dashed`. Drawing the dashes again here is what the first
            // version did, and it left the pair at two different sizes with two
            // different dash spans.
            let marquee = geometry(Icon::Select).expect("`Select` is a Lucide mark");
            outlines_at(painter, origin, scale, stroke, colour, marquee);
            slash();
        }
        Drawn::Corner => {
            // Resize grip: stepped diagonals in the bottom-right corner.
            line(at(20.0, 10.0), at(10.0, 20.0));
            line(at(20.0, 15.0), at(15.0, 20.0));
        }
        Drawn::Harmony => {
            // A wheel with three of its hues marked on it. Filled discs rather
            // than a second ring inside the first: at 18 px a ring within a
            // ring is a blur, and what this has to say is "several colours,
            // related round the wheel" — the marks are the relation.
            //
            // Three of them, and at 120 degrees, because a triad is the harmony
            // whose shape reads at this size; two would be a pair of dots and
            // four would sit on the axes and read as a compass. The mark names
            // the idea rather than whichever relation happens to be chosen.
            // The discs are small against the ring's radius on purpose: at 2.4
            // they merged into the stroke and the mark read as a clover rather
            // than as a wheel with points on it.
            painter.circle_stroke(at(12.0, 12.0), 8.0 * scale, stroke);
            for k in 0..3 {
                let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::TAU / 3.0;
                let (s, c) = a.sin_cos();
                painter.circle_filled(at(12.0 + c * 8.0, 12.0 + s * 8.0), 2.0 * scale, colour);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` holds every icon, at the position an exhaustive match gives it.
    ///
    /// Walking `ALL` could only ever check what is in it, so the arms are the
    /// authority and the array is what is checked: a variant added without a
    /// row is a compile error here, and one filed in the wrong place is a
    /// failure naming both.
    #[test]
    fn every_icon_is_in_all_where_the_match_puts_it() {
        let at = |icon: Icon| -> usize {
            match icon {
                Icon::Brush => 0,
                Icon::Eraser => 1,
                Icon::Select => 2,
                Icon::Transform => 3,
                Icon::Pan => 4,
                Icon::Zoom => 5,
                Icon::Rotate => 6,
                Icon::FlipHorizontal => 7,
                Icon::FlipVertical => 8,
                Icon::Plus => 9,
                Icon::Trash => 10,
                Icon::ChevronUp => 11,
                Icon::ChevronDown => 12,
                Icon::Eye => 13,
                Icon::EyeOff => 14,
                Icon::Mask => 15,
                Icon::Clip => 16,
                Icon::Lock => 17,
                Icon::Unlock => 18,
                Icon::Chain => 19,
                Icon::Close => 20,
                Icon::Pencil => 21,
                Icon::Gear => 22,
                Icon::Check => 23,
                Icon::Grid => 24,
                Icon::Import => 25,
                Icon::Link => 26,
                Icon::Download => 27,
                Icon::Grip => 28,
                Icon::Corner => 29,
                Icon::HalfCircle => 30,
                Icon::Document => 31,
                Icon::Alert => 32,
                Icon::Copy => 33,
                Icon::Cut => 34,
                Icon::Deselect => 35,
                Icon::Folder => 36,
                Icon::ChevronRight => 37,
                Icon::SelectReplace => 38,
                Icon::SelectAdd => 39,
                Icon::SelectSubtract => 40,
                Icon::SelectIntersect => 41,
                Icon::Stamps => 42,
                Icon::Rename => 43,
                Icon::Text => 44,
                Icon::MoveLayer => 45,
                Icon::MaskOff => 46,
                Icon::Harmony => 47,
                Icon::Bold => 48,
                Icon::Italic => 49,
                Icon::Eyedropper => 50,
            }
        };
        for icon in Icon::ALL {
            assert_eq!(Icon::ALL[at(icon)], icon, "{icon:?} is filed elsewhere");
        }
    }

    /// Every Lucide icon flattens to something, and to something the size of
    /// the box it was authored in.
    ///
    /// The parser answers a command it does not know by stopping, which would
    /// leave half an icon rather than a panic — this is what would catch it,
    /// along with a path whose numbers failed to parse and collapsed everything
    /// to the origin.
    #[test]
    fn every_icon_fills_its_box() {
        for icon in Icon::ALL {
            let Art::Lucide(nodes) = icon.art() else {
                continue;
            };
            let outlines = crate::lucide::flatten(nodes);
            assert!(!outlines.is_empty(), "{icon:?} flattened to nothing");

            let points: Vec<_> = outlines.iter().flat_map(|o| o.points.iter()).collect();
            let left = points.iter().map(|p| p.x).fold(f32::MAX, f32::min);
            let right = points.iter().map(|p| p.x).fold(f32::MIN, f32::max);
            let top = points.iter().map(|p| p.y).fold(f32::MAX, f32::min);
            let bottom = points.iter().map(|p| p.y).fold(f32::MIN, f32::max);

            assert!(
                left >= -0.5 && top >= -0.5 && right <= BOX + 0.5 && bottom <= BOX + 0.5,
                "{icon:?} runs outside the 24x24 box: {left}..{right}, {top}..{bottom}"
            );
            // Lucide leaves a unit or two of margin, so an icon reaching less
            // than half the box in *either* direction has lost a subpath. Half
            // is what the narrowest mark in the set actually reaches — a
            // chevron is 12 units across and 6 tall — so this is the floor the
            // shipped set meets rather than a round number.
            assert!(
                right - left >= BOX / 2.0 || bottom - top >= BOX / 2.0,
                "{icon:?} is too small to have all of its parts: {left}..{right}, {top}..{bottom}"
            );
        }
    }

    /// No icon uses a command the parser would stop at.
    ///
    /// Quadratics and the smooth forms are not implemented, and the day a
    /// pasted-in icon uses one this says so by name rather than by a shape
    /// somebody notices later.
    #[test]
    fn every_command_is_one_the_parser_knows() {
        const KNOWN: [char; 12] = ['M', 'm', 'L', 'l', 'H', 'h', 'V', 'v', 'C', 'c', 'A', 'a'];
        for icon in Icon::ALL {
            let Art::Lucide(nodes) = icon.art() else {
                continue;
            };
            for node in nodes {
                let Node::Path(d) = node else {
                    continue;
                };
                for c in d.chars().filter(|c| c.is_ascii_alphabetic()) {
                    // `e` is an exponent rather than a command; no icon here
                    // uses one, and the check would be wrong if one did.
                    assert!(
                        KNOWN.contains(&c) || c == 'Z' || c == 'z',
                        "{icon:?} uses the SVG command {c}, which the parser does not know"
                    );
                }
            }
        }
    }

    /// The marks Umber draws itself are these four, and no more.
    ///
    /// What it checks is the *list*, not Lucide: nothing here can ask the
    /// package whether it carries a layer mask, so a fifth hand-drawn mark
    /// fails this test rather than being caught by a search. That is the point
    /// — the failure is what makes somebody write down what they looked for
    /// before adding one, which is what [`Drawn`]'s variants hold.
    ///
    /// It was five. [`Icon::Mask`] was drawn here because a search for the
    /// *word* mask found only `venetian-mask`; the set carries `view`, which is
    /// the same picture, and the mask is Lucide's now. A list this test blesses
    /// is only ever as good as the searches written into [`Drawn`] beside it.
    #[test]
    fn the_marks_umber_draws_itself_are_these_four() {
        let drawn: Vec<Icon> = Icon::ALL
            .into_iter()
            .filter(|i| matches!(i.art(), Art::Drawn(_)))
            .collect();
        assert_eq!(
            drawn,
            vec![Icon::Corner, Icon::Deselect, Icon::MaskOff, Icon::Harmony,],
            "the set is Lucide's; a mark drawn here needs its search written \
             into `Drawn` and this list extended deliberately"
        );
    }

    /// The cache holds an entry for every icon and geometry for every Lucide
    /// one, which is what `draw`'s `else` branch rests on.
    #[test]
    fn every_lucide_icon_has_geometry_and_no_drawn_one_does() {
        for icon in Icon::ALL {
            match icon.art() {
                Art::Lucide(_) => {
                    let held = geometry(icon).unwrap_or_else(|| panic!("{icon:?} has no geometry"));
                    assert!(!held.is_empty(), "{icon:?} flattened to nothing");
                }
                Art::Drawn(_) => assert!(
                    geometry(icon).is_none(),
                    "{icon:?} is drawn and should hold no flattened geometry"
                ),
            }
        }
    }

    /// Not a guard: a way to look at the set.
    ///
    /// Writes a sheet of every icon in both themes, at the three sizes the
    /// interface actually draws them, so a migration can be judged by eye. It
    /// is what caught the one real defect in this one — `scaling` and
    /// `external-link` are the same mark at 18 px — which no assertion over
    /// path data could have found, because both icons were exactly what the
    /// package says they are.
    ///
    /// **The smallest row is `crate::ui::ICON_BUTTON_MARK`**, taken from the
    /// constant rather than typed, so the sheet cannot go on showing a size the
    /// interface has stopped drawing. It is also the size at which
    /// `icons::draw`'s stroke stops thinning: the weight is `(2 * size / 24)`
    /// held at a floor of 1.0, and 12 is exactly where the two meet, so
    /// anything drawn smaller than the header's mark gets a stroke too heavy
    /// for it and blots. That is the row to look at first.
    ///
    /// Into the temporary directory rather than the tree: nothing here is
    /// checked in, and the picture is out of date the moment an icon moves.
    #[test]
    #[ignore = "writes a picture to look at; run deliberately"]
    fn icon_sheet() {
        use crate::theme::{Palette, ThemeKind};
        let Some(mut stage) = crate::docshot::Stage::new() else {
            return;
        };
        for (kind, name) in [
            (ThemeKind::Graphite, "umber-icons-graphite.png"),
            (ThemeKind::Paper, "umber-icons-paper.png"),
        ] {
            let palette = Palette::of(kind);
            let cols = 9;
            let rows = Icon::ALL.len().div_ceil(cols);
            let cell = 58.0;
            let size = egui::vec2(cols as f32 * cell, rows as f32 * cell);
            let image = stage.shoot(size, 2.0, &palette, palette.window, |ui| {
                let painter = ui.painter().clone();
                let top = ui.max_rect().min;
                for (k, icon) in Icon::ALL.iter().enumerate() {
                    let x = (k % cols) as f32 * cell;
                    let y = (k / cols) as f32 * cell;
                    let at = egui::Rect::from_min_size(
                        top + egui::vec2(x + 8.0, y + 6.0),
                        egui::Vec2::splat(18.0),
                    );
                    draw(&painter, at, *icon, palette.text);
                    // The 14 px instance under it: a mark that only reads at
                    // the tool rail's size is a mark that is wrong in a layer
                    // row.
                    let small = egui::Rect::from_min_size(
                        top + egui::vec2(x + 8.0, y + 26.0),
                        egui::Vec2::splat(14.0),
                    );
                    draw(&painter, small, *icon, palette.text_dim);
                    // And a module header's own mark, which is the smallest
                    // instance in the interface and therefore the one that
                    // decides whether a mark reads at all.
                    let header = egui::Rect::from_min_size(
                        top + egui::vec2(x + 8.0, y + 42.0),
                        egui::Vec2::splat(crate::ui::ICON_BUTTON_MARK),
                    );
                    draw(&painter, header, *icon, palette.text_dim);
                }
            });
            let out = std::env::temp_dir().join(name);
            crate::docshot::write_png(&out, &image).expect("a picture");
            println!("wrote {}", out.display());
        }
    }
}
