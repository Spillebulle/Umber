//! The layout model: which modules are docked where, which float, how big they
//! are, and where a dragged one would land.
//!
//! This module deliberately contains no drawing. Everything here is plain
//! geometry and state, which is what lets the awkward parts — insertion
//! indices, minimum-size clamping, the config round trip — be tested without a
//! window or a GPU. The painting lives in `panels.rs`.
//!
//! ## Why identity is a `PanelKind` and not an index
//!
//! An immediate-mode UI rebuilds the whole tree every frame, so a panel being
//! dragged has to be recognisable from one frame to the next by something
//! stable. There is exactly one Colour panel, one Brushes panel and one Layers
//! panel, so the kind *is* the identity. That also makes the config file
//! readable and makes a corrupt one easy to repair — a kind that appears twice
//! is dropped, a kind that appears nowhere is simply closed.
//!
//! ## Why a side is a list of columns
//!
//! Each edge of the workspace used to hold one stack of panels. It now holds a
//! list of [`Column`]s, each its own stack with its own width, laid out from
//! the window edge **inwards** — so Colour can sit at the far right with
//! Brushes full height immediately to its left.
//!
//! A column is exactly what a sidebar used to be, which is what let the config
//! file keep its version header. A file written before columns existed names no
//! column at all, and every `dock` line for a side falls into the one column
//! that side implicitly has; see [`Layout::from_config`]. Ordering columns from
//! the edge inwards rather than left to right is what makes the two sides
//! symmetric: index 0 is always the one against the window, on either edge, so
//! nothing downstream has to ask which way round this side counts.
//!
//! ## Why the tool rail is an ordinary module
//!
//! It used to be chrome — a fixed strip with a side of its own, its own
//! `egui::Panel` and its own drag handle. The one thing it could do, move to
//! the other edge, the dock model already did for four other modules; and
//! everything it could not do — be resized, share a column, float, be closed
//! and come back from the module library — the dock model gives away for free.
//! So [`PanelKind::Tools`] is a module like the rest, and the rail's side is no
//! longer a setting.
//!
//! It is also the one addition to [`PanelKind::DEFAULT_DOCK`] that does not
//! need the config's version bumped, and the reason is particular to it. The
//! rule elsewhere is that a file written before a module existed does not name
//! it, so a default containing it would make a fresh install and an upgraded
//! one disagree. A pre-columns file *does* name the rail: it always existed,
//! and the file records which edge it was on. `from_config` reads that `rail`
//! line as a Tools column at that edge, so an upgraded workspace opens exactly
//! as it closed and matches what a fresh install ships with. The writer no
//! longer emits `rail`, and its absence is therefore also what tells a later
//! load that a *closed* Tools module was meant rather than an old file.
//!
//! ## Why a drag lifts the panel out of the layout
//!
//! [`Layout::begin_drag`] removes the panel from wherever it was. The stack it
//! left reflows immediately, so the insertion index computed against the
//! remaining slots is exactly the index the drop will use — there is no
//! "does this index count the panel I am holding?" case to get wrong, and the
//! drop indicator cannot disagree with the result.
//!
//! A column emptied that way is deliberately *kept* until the drop resolves.
//! Removing it on the spot would slide every column outside it sideways under
//! the pointer mid-drag, and would renumber the very column indices the drop
//! target is being computed against. It is pruned by whatever ends the drag —
//! see [`Layout::prune`].
//!
//! That is also what makes *adding* a module a drag rather than a placement.
//! [`Layout::add_dragging`] puts a closed module straight into the pointer's
//! hand, so the same drop that moves an existing panel decides where the new
//! one lands, and the same Escape abandons it. Its origin is "closed", so
//! cancelling an add undoes the add.

use crate::theme::metrics;
use egui::{Pos2, Rect, Vec2, pos2, vec2};
use std::path::PathBuf;

/// Minimum and maximum sizes, in egui points.
///
/// These are guards against destroying a panel by dragging, not design values;
/// the design's own fixed sizes live in [`crate::theme::metrics`].
pub mod limits {
    /// Enough for a header plus a usable sliver of content.
    pub const PANEL_MIN_HEIGHT: f32 = 96.0;
    /// The narrowest a column holding an ordinary module may be dragged. A
    /// column holding only the tool rail may go narrower — see
    /// [`super::PanelKind::min_width`].
    pub const SIDEBAR_MIN_WIDTH: f32 = 190.0;
    pub const SIDEBAR_MAX_WIDTH: f32 = 460.0;
    /// What is always left for the document, however many columns are docked.
    /// Columns are laid out from the edge inwards, so this is taken off each in
    /// turn and the innermost one is the one that gives.
    pub const CANVAS_MIN_WIDTH: f32 = 190.0;
    pub const FLOAT_MIN_WIDTH: f32 = 190.0;
    pub const FLOAT_MIN_HEIGHT: f32 = 130.0;
    pub const FLOAT_MAX_WIDTH: f32 = 720.0;
    pub const FLOAT_MAX_HEIGHT: f32 = 900.0;
    /// How far in from the canvas edge counts as "drop into this side" when
    /// that side is currently empty and so has no rect of its own.
    pub const EMPTY_ZONE_WIDTH: f32 = 104.0;
    /// How far in from a column's outer or inner edge counts as "put a new
    /// column here" rather than "add to this column's stack". Capped at a share
    /// of the column so the two bands can never meet in the middle of a narrow
    /// one and leave it with no way to be stacked into.
    pub const NEW_COLUMN_ZONE: f32 = 40.0;
    /// How much of a floating panel must stay inside the workspace. Its header
    /// is the only way to move it, so the header must never be unreachable.
    pub const FLOAT_KEEP_VISIBLE: f32 = 96.0;
}

/// A module that can be docked, floated or closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Tools,
    Colour,
    Palette,
    Brushes,
    Layers,
    History,
    Text,
}

impl PanelKind {
    pub const ALL: [PanelKind; 7] = [
        Self::Tools,
        Self::Colour,
        Self::Palette,
        Self::Brushes,
        Self::Layers,
        Self::History,
        Self::Text,
    ];

    /// The arrangement Umber ships with, a column per side: the tool rail alone
    /// at the left, the design's three modules stacked at the right.
    ///
    /// History and Palette are deliberately *not* among them, and that is the
    /// same answer a layout file written before either existed gets — an absent
    /// panel is a closed one, so an old config opens with them closed too.
    /// Putting one in the default instead would have made a fresh install and
    /// an upgraded one disagree about what the workspace contains, which is
    /// exactly the silent divergence the config's version header exists to
    /// prevent; and the alternative to that, bumping the version, would throw
    /// away every arrangement anybody has made to add one module they can reach
    /// from the Window menu in two clicks.
    ///
    /// Tools *is* among them, and is the exception the module comment argues
    /// for: an old file records the rail's edge, so it opens with the rail
    /// where it was rather than without one.
    pub const DEFAULT_DOCK: [&'static [PanelKind]; 2] =
        [&[Self::Tools], &[Self::Colour, Self::Brushes, Self::Layers]];

    pub fn title(self) -> &'static str {
        match self {
            Self::Tools => "Tools",
            Self::Colour => "Colour",
            Self::Palette => "Palette",
            Self::Brushes => "Brushes",
            Self::Layers => "Layers",
            Self::History => "History",
            Self::Text => "Text",
        }
    }

    /// One line saying what the module is for, shown in the module library
    /// beside its picture.
    ///
    /// Here rather than in `panels.rs` because the description belongs to the
    /// identity: there is exactly one of each module, so there is exactly one
    /// thing each is for. Adding a kind and forgetting the sentence is then a
    /// missing match arm rather than a card with a blank half.
    pub fn description(self) -> &'static str {
        match self {
            Self::Tools => {
                "The tools, and the painting and background colours with the \
                 swap between them."
            }
            Self::Colour => {
                "Choose the painting colour — a hue ring, a saturation square, \
                 RGB sliders or a harmony wheel."
            }
            Self::Palette => {
                "The colours you are working with, and the library of palettes \
                 you keep them in."
            }
            Self::Brushes => {
                "The brushes in reach, and the way in to the whole library and \
                 to importing more."
            }
            Self::Layers => "The layer stack: order, visibility, opacity and blend mode.",
            Self::History => {
                "Every stroke on this document, and a click to go back to any \
                 of them."
            }
            Self::Text => {
                "Set a line of text in any font on this machine, and place it \
                 on the canvas to move, scale and turn."
            }
        }
    }

    /// Stable name for the config file. Deliberately not derived from `title`,
    /// so retitling a panel in the UI cannot silently invalidate saved layouts.
    fn key(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Colour => "colour",
            Self::Palette => "palette",
            Self::Brushes => "brushes",
            Self::Layers => "layers",
            Self::History => "history",
            Self::Text => "text",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    /// Share of the column's flexible space in the default layout. The colour
    /// picker is the tallest thing in the dock, so it gets the most.
    fn default_weight(self) -> f32 {
        match self {
            Self::Tools => 1.0,
            Self::Colour => 3.0,
            // A grid of swatches, so it wants less than a picker and about what
            // a shortlist of brushes wants.
            Self::Palette => 1.3,
            Self::Brushes => 1.3,
            Self::Layers => 2.2,
            Self::History => 2.0,
            // A field to type into, four controls and a preview: about what
            // the layer list wants, and more than the shortlist of brushes.
            Self::Text => 2.2,
        }
    }

    /// How wide a column holding only this module starts out.
    ///
    /// The rail starts at [`limits::SIDEBAR_MIN_WIDTH`] — the narrowest an
    /// ordinary column may be — rather than at [`metrics::TOOL_RAIL`], which is
    /// only its *floor*. Starting a column on its own minimum is a column that
    /// can only be dragged one way, and the rail arriving at the width the
    /// chrome version happened to have said "this is what a rail is" about a
    /// module whose whole point is that it is now the user's to size. A column
    /// starting where every other column bottoms out is the smallest width that
    /// leaves room to go either way, and it lines the rail's inner edge up with
    /// a sidebar dragged to its narrowest.
    ///
    /// Every other module wants the design's panel width.
    fn default_width(self) -> f32 {
        match self {
            Self::Tools => limits::SIDEBAR_MIN_WIDTH,
            _ => metrics::PANEL,
        }
    }

    /// The narrowest a column holding this module may be dragged.
    ///
    /// Per module rather than one number for every column, because the tool
    /// rail's whole point is being narrow and every other module is unusable
    /// there. A column takes the widest minimum among the modules in it, so
    /// dropping Colour into the rail's column widens it rather than leaving a
    /// picker three buttons across.
    pub fn min_width(self) -> f32 {
        match self {
            Self::Tools => metrics::TOOL_RAIL,
            _ => limits::SIDEBAR_MIN_WIDTH,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub const ALL: [Side; 2] = [Self::Left, Self::Right];

    pub fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }
}

/// A panel in a column's stack. `weight` is its share of the space left over
/// once every panel in the column has its minimum height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Docked {
    pub kind: PanelKind,
    pub weight: f32,
}

/// One column of docked panels: a stack, and how wide it is.
///
/// Exactly what a whole sidebar used to be. Columns on a side are ordered from
/// the window edge inwards.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub panels: Vec<Docked>,
    pub width: f32,
}

impl Column {
    fn of(kind: PanelKind) -> Self {
        Self {
            panels: vec![Docked {
                kind,
                weight: kind.default_weight(),
            }],
            width: kind.default_width(),
        }
    }

    /// The widest minimum among the modules in it. An empty column is a ghost
    /// left behind by a drag and is about to be pruned; the ordinary floor is
    /// the honest answer for one.
    pub fn min_width(&self) -> f32 {
        self.panels
            .iter()
            .map(|d| d.kind.min_width())
            .reduce(f32::max)
            .unwrap_or(limits::SIDEBAR_MIN_WIDTH)
    }
}

/// A panel hovering over the canvas, positioned in absolute window points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Floating {
    pub kind: PanelKind,
    pub rect: Rect,
}

/// Where a panel was before a drag started, so Escape can put it back.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Origin {
    Dock {
        side: Side,
        column: usize,
        index: usize,
        weight: f32,
        /// The column's width. Carried so that a panel dropped as a *new*
        /// column keeps the width it had rather than snapping back to the
        /// design's default every time it is moved.
        width: f32,
    },
    Float(Rect),
    /// It was not in the layout at all — the drag is an *add*, from the module
    /// library. Cancelling one therefore abandons the add rather than putting
    /// the panel anywhere.
    Closed,
}

/// A drag in progress. The panel itself has already been removed from the
/// layout — see the module comment.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub kind: PanelKind,
    /// Offset from the panel's top-left to the pointer when it was grabbed, so
    /// the panel does not jump to centre itself under the cursor.
    pub grab: Vec2,
    /// Size it will take if dropped as a float.
    pub float_size: Vec2,
    /// Latest pointer position, in window points.
    pub pointer: Pos2,
    /// Started by a *click* rather than by a press — adding a module from the
    /// library. The button that began it is already up, so such a drag cannot
    /// end on a release; it ends on the next press instead. See
    /// [`Layout::drag_should_drop`].
    pub sticky: bool,
    /// Sticky drags only: whether the pointer has been seen up since the drag
    /// began.
    armed: bool,
    origin: Origin,
}

impl Drag {
    /// Rect the panel would occupy if released here as a float.
    pub fn float_rect(&self) -> Rect {
        Rect::from_min_size(self.pointer - self.grab, self.float_size)
    }
}

/// Where a release would put the dragged panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropTarget {
    /// Into an existing column's stack.
    Dock {
        side: Side,
        column: usize,
        index: usize,
    },
    /// As a column of its own, inserted at `column` — counting, like every
    /// column index, from the window edge inwards.
    NewColumn {
        side: Side,
        column: usize,
    },
    Float,
}

/// One column's rects: the whole of it, and one slot per panel in it.
#[derive(Clone, Debug)]
pub struct ColumnGeometry {
    pub rect: Rect,
    pub slots: Vec<Rect>,
}

/// Everything the layout occupies this frame, in egui points.
///
/// Computed up front from the workspace rect so that hit testing and drawing
/// cannot disagree: the panels are drawn *into* these rects rather than being
/// laid out separately and measured afterwards.
#[derive(Clone, Debug)]
pub struct Geometry {
    /// Below the options strip, above the status bar, full window width.
    pub workspace: Rect,
    /// Per side, one entry per column, ordered from the window edge inwards.
    pub sides: [Vec<ColumnGeometry>; 2],
    /// What is left for the document. Floating panels do **not** come out of
    /// this — they hover, so the canvas region and therefore the camera pivot
    /// are unaffected by them.
    pub canvas: Rect,
}

impl Geometry {
    pub fn columns(&self, side: Side) -> &[ColumnGeometry] {
        &self.sides[side.index()]
    }

    /// The whole of one side, or `None` when nothing is docked there.
    pub fn sidebar(&self, side: Side) -> Option<Rect> {
        let columns = self.columns(side);
        Some(columns.first()?.rect.union(columns.last()?.rect))
    }

    /// The region that counts as "drop into this side".
    ///
    /// An occupied side is the union of its columns. An empty one has no rect,
    /// so a strip at that edge of the canvas stands in — otherwise a side you
    /// emptied could never be filled again.
    pub fn drop_zone(&self, side: Side) -> Rect {
        if let Some(rect) = self.sidebar(side) {
            return rect;
        }
        let width = limits::EMPTY_ZONE_WIDTH.min(self.canvas.width() * 0.5);
        match side {
            Side::Left => Rect::from_min_max(
                self.canvas.left_top(),
                pos2(self.canvas.left() + width, self.canvas.bottom()),
            ),
            Side::Right => Rect::from_min_max(
                pos2(self.canvas.right() - width, self.canvas.top()),
                self.canvas.right_bottom(),
            ),
        }
    }

    /// Where a release at `pointer` would put the panel.
    ///
    /// Within a column, a band at either edge means "a new column here" and the
    /// middle means "into this stack". The bands of two neighbouring columns
    /// name the same boundary and therefore resolve to the same index, so which
    /// of the pair the pointer is a hair inside cannot change the answer.
    pub fn drop_target(&self, pointer: Pos2) -> DropTarget {
        for side in Side::ALL {
            let columns = self.columns(side);
            if columns.is_empty() {
                if self.drop_zone(side).contains(pointer) {
                    return DropTarget::NewColumn { side, column: 0 };
                }
                continue;
            }
            for (index, column) in columns.iter().enumerate() {
                if !column.rect.contains(pointer) {
                    continue;
                }
                let band = limits::NEW_COLUMN_ZONE.min(column.rect.width() * 0.3);
                let (outer, inner) = match side {
                    Side::Left => (column.rect.left(), column.rect.right()),
                    Side::Right => (column.rect.right(), column.rect.left()),
                };
                if (pointer.x - outer).abs() <= band {
                    return DropTarget::NewColumn {
                        side,
                        column: index,
                    };
                }
                if (pointer.x - inner).abs() <= band {
                    return DropTarget::NewColumn {
                        side,
                        column: index + 1,
                    };
                }
                return DropTarget::Dock {
                    side,
                    column: index,
                    index: insert_index(&column.slots, pointer.y),
                };
            }
        }
        DropTarget::Float
    }

    /// The line a dock drop would insert at, for the drop indicator.
    pub fn insertion_line(&self, side: Side, column: usize, index: usize) -> (Pos2, Pos2) {
        let Some(column) = self.columns(side).get(column) else {
            let zone = self.drop_zone(side);
            return (zone.left_top(), zone.right_top());
        };
        let y = match column.slots.get(index) {
            Some(slot) => slot.top(),
            None => column
                .slots
                .last()
                .map_or(column.rect.top(), |slot| slot.bottom()),
        };
        (pos2(column.rect.left(), y), pos2(column.rect.right(), y))
    }

    /// Where a column inserted at `column` would appear, for the drop
    /// indicator. A hint at the boundary rather than the column's eventual
    /// width, which is not known until the drop resolves.
    pub fn new_column_strip(&self, side: Side, column: usize) -> Rect {
        let columns = self.columns(side);
        let edge = match columns.get(column) {
            // Against the outer edge of the column it would push inwards.
            Some(column) => match side {
                Side::Left => column.rect.left(),
                Side::Right => column.rect.right(),
            },
            // Past the innermost one, or — with nothing docked at all —
            // against the canvas edge, which is where the empty drop zone is.
            None => match columns.last() {
                Some(column) => match side {
                    Side::Left => column.rect.right(),
                    Side::Right => column.rect.left(),
                },
                None => match side {
                    Side::Left => self.canvas.left(),
                    Side::Right => self.canvas.right(),
                },
            },
        };
        let width = limits::EMPTY_ZONE_WIDTH
            .min(self.workspace.width())
            .max(0.0);
        let (a, b) = match side {
            Side::Left => (edge, edge + width),
            Side::Right => (edge - width, edge),
        };
        // Clamped in order, so the result cannot come out inverted however
        // small the window is.
        let left = a.max(self.workspace.left()).min(self.workspace.right());
        let right = b.min(self.workspace.right()).max(left);
        Rect::from_min_max(
            pos2(left, self.workspace.top()),
            pos2(right, self.workspace.bottom()),
        )
    }
}

/// Index a panel dropped at height `y` should be inserted at.
///
/// The midpoint of each existing slot is the boundary, which is what makes the
/// indicator land above the panel you are pointing at the top half of.
fn insert_index(slots: &[Rect], y: f32) -> usize {
    slots
        .iter()
        .position(|slot| y < slot.center().y)
        .unwrap_or(slots.len())
}

/// Divide `available` between `weights`, giving every panel at least
/// [`limits::PANEL_MIN_HEIGHT`].
///
/// A weight is a share of what is left *after* the minimums are handed out, not
/// a share of the whole. That is what makes dragging a splitter stable: writing
/// the resulting heights back as `height - minimum` and recomputing reproduces
/// exactly the heights the user dragged to. Sharing the whole proportionally
/// would not round-trip, and the stack would creep every frame.
pub fn stack_heights(weights: &[f32], available: f32) -> Vec<f32> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let available = available.max(0.0);
    // Too cramped to honour the minimums at all: share equally rather than
    // satisfying the first panels and starving the last.
    if available <= limits::PANEL_MIN_HEIGHT * n as f32 {
        return vec![available / n as f32; n];
    }
    let extra = available - limits::PANEL_MIN_HEIGHT * n as f32;
    let total: f32 = weights.iter().map(|w| w.max(0.0)).sum();
    if total <= 1e-6 {
        return vec![available / n as f32; n];
    }
    weights
        .iter()
        .map(|w| limits::PANEL_MIN_HEIGHT + extra * w.max(0.0) / total)
        .collect()
}

pub struct Layout {
    /// Per side, columns ordered from the window edge inwards.
    sides: [Vec<Column>; 2],
    /// Draw order, and therefore z-order: the last one is on top.
    floating: Vec<Floating>,
    drag: Option<Drag>,
    /// The design's layout edit mode. Panels are only draggable while it is on,
    /// and the canvas is paused for as long as it is — see [`Self::blocks_canvas`].
    ///
    /// Deliberately not persisted. It is a mode you enter to rearrange things
    /// and leave again, and starting a session in it would be a nasty surprise:
    /// the first stroke would silently do nothing.
    edit_mode: bool,
    /// Set by every mutation; cleared once the config file has been written.
    dirty: bool,
}

impl Default for Layout {
    fn default() -> Self {
        let side = |index: usize| -> Vec<Column> {
            let kinds = PanelKind::DEFAULT_DOCK[index];
            if kinds.is_empty() {
                return Vec::new();
            }
            vec![Column {
                panels: kinds
                    .iter()
                    .map(|kind| Docked {
                        kind: *kind,
                        weight: kind.default_weight(),
                    })
                    .collect(),
                // What the widest module in it asks for, so the shipped rail is
                // rail-width and the shipped dock is panel-width without either
                // number being written down a second time.
                width: kinds
                    .iter()
                    .map(|kind| kind.default_width())
                    .fold(0.0_f32, f32::max),
            }]
        };
        Self {
            sides: [side(0), side(1)],
            floating: Vec::new(),
            drag: None,
            edit_mode: false,
            dirty: false,
        }
    }
}

impl Layout {
    // --- queries -----------------------------------------------------------

    pub fn columns(&self, side: Side) -> &[Column] {
        &self.sides[side.index()]
    }

    /// The panels in one column, or nothing if there is no such column.
    pub fn docked(&self, side: Side, column: usize) -> &[Docked] {
        self.sides[side.index()]
            .get(column)
            .map_or(&[], |c| c.panels.as_slice())
    }

    pub fn floating(&self) -> &[Floating] {
        &self.floating
    }

    pub fn width(&self, side: Side, column: usize) -> f32 {
        self.sides[side.index()]
            .get(column)
            .map_or(metrics::PANEL, |c| c.width)
    }

    pub fn drag(&self) -> Option<Drag> {
        self.drag
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn edit_mode(&self) -> bool {
        self.edit_mode
    }

    pub fn set_edit_mode(&mut self, on: bool) {
        if self.edit_mode == on {
            return;
        }
        self.edit_mode = on;
        if !on {
            self.cancel_drag();
        }
    }

    pub fn is_open(&self, kind: PanelKind) -> bool {
        self.place_of(kind).is_some()
    }

    fn place_of(&self, kind: PanelKind) -> Option<Origin> {
        for side in Side::ALL {
            for (column, col) in self.sides[side.index()].iter().enumerate() {
                if let Some(index) = col.panels.iter().position(|d| d.kind == kind) {
                    return Some(Origin::Dock {
                        side,
                        column,
                        index,
                        weight: col.panels[index].weight,
                        width: col.width,
                    });
                }
            }
        }
        self.floating
            .iter()
            .find(|f| f.kind == kind)
            .map(|f| Origin::Float(f.rect))
    }

    /// True when the layout, rather than the canvas, owns the pointer.
    ///
    /// Three cases, and all three are real bugs if missed:
    ///
    /// * **Edit mode.** The design pauses the canvas outright while the layout
    ///   is being rearranged — "nothing you draw changes; panels are the only
    ///   thing that moves". That is also the sturdiest possible answer to
    ///   "a panel dragged over the canvas must not paint underneath itself".
    /// * **A live drag**, which by then is over open canvas, not over a widget.
    /// * **A floating panel**, which sits over the canvas rather than beside
    ///   it, so the central panel's rect cannot tell the canvas whose click it
    ///   was.
    ///
    /// `pointer` is in egui points.
    pub fn blocks_canvas(&self, pointer: Pos2) -> bool {
        self.edit_mode
            || self.drag.is_some()
            || self.floating.iter().any(|f| f.rect.contains(pointer))
    }

    // --- geometry ----------------------------------------------------------

    /// Lay the whole thing out inside `workspace`.
    ///
    /// Every rect this returns is non-negative in both directions, however
    /// small the window is. That is not fussiness: an `egui::Rect` whose max is
    /// left of its min reports a negative width, and everything downstream —
    /// drop zones, panel bodies, the canvas the camera pivot comes from — is
    /// derived from these by subtraction, so one inverted rect here turns into
    /// widgets painted somewhere they were never asked for.
    pub fn geometry(&self, workspace: Rect) -> Geometry {
        // The strips above and below claim their height first, so on a window
        // shorter than the sum of them what arrives here is already inverted.
        let workspace = Rect::from_min_size(workspace.min, workspace.size().max(Vec2::ZERO));
        let mut canvas = workspace;
        let mut sides = [Vec::new(), Vec::new()];
        for side in Side::ALL {
            for column in &self.sides[side.index()] {
                // Never let the columns eat the canvas entirely. `canvas` is
                // already non-negative, so this cannot come out below zero and
                // a column cannot claim more than there is. Applied per column
                // as they are peeled off, so the innermost is the one that
                // gives on a narrow window.
                let width = column
                    .width
                    .max(0.0)
                    .min((canvas.width() - limits::CANVAS_MIN_WIDTH).max(0.0))
                    .clamp(0.0, canvas.width())
                    .round();
                let rect = match side {
                    Side::Left => {
                        let r = Rect::from_min_max(
                            canvas.left_top(),
                            pos2(canvas.left() + width, canvas.bottom()),
                        );
                        canvas.min.x = r.right();
                        r
                    }
                    Side::Right => {
                        let r = Rect::from_min_max(
                            pos2(canvas.right() - width, canvas.top()),
                            canvas.right_bottom(),
                        );
                        canvas.max.x = r.left();
                        r
                    }
                };

                let weights: Vec<f32> = column.panels.iter().map(|d| d.weight).collect();
                let mut slots = Vec::with_capacity(weights.len());
                let mut y = rect.top();
                for height in stack_heights(&weights, rect.height()) {
                    slots.push(Rect::from_min_size(
                        pos2(rect.left(), y),
                        vec2(rect.width(), height),
                    ));
                    y += height;
                }
                sides[side.index()].push(ColumnGeometry { rect, slots });
            }
        }

        Geometry {
            workspace,
            sides,
            canvas,
        }
    }

    /// Pull floating panels back so their headers stay grabbable.
    ///
    /// Without this a layout saved on a large monitor strands its panels off
    /// the edge of a small one, with no way to get them back short of a reset.
    pub fn clamp_floating(&mut self, workspace: Rect) {
        for f in &mut self.floating {
            let size = vec2(
                f.rect
                    .width()
                    .clamp(limits::FLOAT_MIN_WIDTH, limits::FLOAT_MAX_WIDTH)
                    .min(workspace.width().max(limits::FLOAT_MIN_WIDTH)),
                f.rect
                    .height()
                    .clamp(limits::FLOAT_MIN_HEIGHT, limits::FLOAT_MAX_HEIGHT)
                    .min(workspace.height().max(limits::FLOAT_MIN_HEIGHT)),
            );
            let keep = limits::FLOAT_KEEP_VISIBLE.min(size.x);
            let x = f
                .rect
                .left()
                .clamp(workspace.left() - (size.x - keep), workspace.right() - keep);
            // The header must never go above the workspace or below its bottom
            // edge, since it is the only handle the panel has.
            let y = f.rect.top().clamp(
                workspace.top(),
                (workspace.bottom() - metrics::PANEL_HEADER).max(workspace.top()),
            );
            let next = Rect::from_min_size(pos2(x, y), size);
            if next != f.rect {
                f.rect = next;
            }
        }
    }

    // --- mutation ----------------------------------------------------------

    pub fn set_width(&mut self, side: Side, column: usize, width: f32) {
        let Some(col) = self.sides[side.index()].get_mut(column) else {
            return;
        };
        let width = width
            .clamp(col.min_width(), limits::SIDEBAR_MAX_WIDTH)
            .round();
        if col.width != width {
            col.width = width;
            self.dirty = true;
        }
    }

    /// Move the boundary between panels `index` and `index + 1` of one column
    /// by `delta` points, given the heights they currently have.
    pub fn resize_split(
        &mut self,
        side: Side,
        column: usize,
        index: usize,
        delta: f32,
        heights: &[f32],
    ) {
        let Some(col) = self.sides[side.index()].get(column) else {
            return;
        };
        if index + 1 >= col.panels.len() || heights.len() != col.panels.len() {
            return;
        }
        let mut next: Vec<f32> = heights.to_vec();
        // Clamp the movement rather than the results, so the pair always keeps
        // the same total: clamping afterwards would let the stack grow.
        let min = limits::PANEL_MIN_HEIGHT;
        let delta = delta.max(min - next[index]).min(next[index + 1] - min);
        next[index] += delta;
        next[index + 1] -= delta;
        self.set_weights_from_heights(side, column, &next);
    }

    fn set_weights_from_heights(&mut self, side: Side, column: usize, heights: &[f32]) {
        let Some(col) = self.sides[side.index()].get_mut(column) else {
            return;
        };
        if heights.len() != col.panels.len() {
            return;
        }
        let flexible: Vec<f32> = heights
            .iter()
            .map(|h| (h - limits::PANEL_MIN_HEIGHT).max(0.0))
            .collect();
        if flexible.iter().sum::<f32>() <= 1e-6 {
            for d in col.panels.iter_mut() {
                d.weight = 1.0;
            }
        } else {
            for (d, w) in col.panels.iter_mut().zip(flexible) {
                d.weight = w;
            }
        }
        self.dirty = true;
    }

    pub fn set_float_rect(&mut self, kind: PanelKind, rect: Rect) {
        if let Some(f) = self.floating.iter_mut().find(|f| f.kind == kind)
            && f.rect != rect
        {
            f.rect = rect;
            self.dirty = true;
        }
    }

    /// Bring a floating panel to the front of the stack.
    pub fn raise(&mut self, kind: PanelKind) {
        let Some(index) = self.floating.iter().position(|f| f.kind == kind) else {
            return;
        };
        if index + 1 == self.floating.len() {
            return;
        }
        let f = self.floating.remove(index);
        self.floating.push(f);
        self.dirty = true;
    }

    pub fn close(&mut self, kind: PanelKind) {
        self.take(kind);
        self.prune();
        self.dirty = true;
    }

    /// Put a closed panel back, at the bottom of whichever side has room.
    pub fn open(&mut self, kind: PanelKind) {
        if self.is_open(kind) {
            return;
        }
        // Prefer the side that already has columns, so reopening does not
        // conjure a second sidebar the user never asked for.
        let side = if self.sides[Side::Right.index()].is_empty()
            && !self.sides[Side::Left.index()].is_empty()
        {
            Side::Left
        } else {
            Side::Right
        };
        // Into the innermost column, which is the one nearest the canvas and
        // so the one the eye reaches first — or a column of its own where the
        // side is empty.
        match self.sides[side.index()].last_mut() {
            Some(col) => col.panels.push(Docked {
                kind,
                weight: kind.default_weight(),
            }),
            None => self.sides[side.index()].push(Column::of(kind)),
        }
        self.dirty = true;
    }

    /// Back to the arrangement the app ships with. Leaves edit mode alone, so
    /// "reset" does not also throw the user out of what they were doing.
    pub fn reset(&mut self) {
        let edit_mode = self.edit_mode;
        *self = Self::default();
        self.edit_mode = edit_mode;
        self.dirty = true;
    }

    /// Drop any column left with nothing in it.
    ///
    /// Not done by [`Self::take`], because a drag deliberately leaves the
    /// column it emptied standing until the drop resolves — see the module
    /// comment. Everything that *ends* a drag, and everything that removes a
    /// panel outright, calls this.
    fn prune(&mut self) {
        for side in &mut self.sides {
            side.retain(|c| !c.panels.is_empty());
        }
    }

    /// Remove a panel from wherever it is, reporting where that was. May leave
    /// an empty column behind; see [`Self::prune`].
    fn take(&mut self, kind: PanelKind) -> Option<Origin> {
        for side in Side::ALL {
            for column in 0..self.sides[side.index()].len() {
                let col = &mut self.sides[side.index()][column];
                let Some(index) = col.panels.iter().position(|d| d.kind == kind) else {
                    continue;
                };
                let removed = col.panels.remove(index);
                return Some(Origin::Dock {
                    side,
                    column,
                    index,
                    weight: removed.weight,
                    width: col.width,
                });
            }
        }
        let index = self.floating.iter().position(|f| f.kind == kind)?;
        Some(Origin::Float(self.floating.remove(index).rect))
    }

    fn put(&mut self, kind: PanelKind, origin: Origin) {
        match origin {
            Origin::Dock {
                side,
                column,
                index,
                weight,
                width,
            } => {
                let columns = &mut self.sides[side.index()];
                match columns.get_mut(column) {
                    Some(col) => {
                        let index = index.min(col.panels.len());
                        col.panels.insert(index, Docked { kind, weight });
                    }
                    // The column has gone since — a `close` between the two, or
                    // a layout replaced under it. Rebuild it where it was.
                    None => {
                        let at = column.min(columns.len());
                        columns.insert(
                            at,
                            Column {
                                panels: vec![Docked { kind, weight }],
                                width,
                            },
                        );
                    }
                }
            }
            Origin::Float(rect) => self.floating.push(Floating { kind, rect }),
            // It came from nowhere, so it goes back to nowhere.
            Origin::Closed => {}
        }
    }

    // --- dragging ----------------------------------------------------------

    /// Lift a panel out of the layout and start following the pointer.
    pub fn begin_drag(&mut self, kind: PanelKind, pointer: Pos2, panel_rect: Rect) {
        // Panels are locked outside edit mode, as the design has them: reaching
        // for a slider must never tear its panel off.
        if self.drag.is_some() || !self.edit_mode {
            return;
        }
        let Some(origin) = self.take(kind) else {
            return;
        };
        // A panel torn out of a dock has no float size of its own yet; keeping
        // its docked width but a sane height reads better than either extreme.
        let float_size = match origin {
            Origin::Float(rect) => rect.size(),
            Origin::Dock { .. } | Origin::Closed => vec2(
                panel_rect
                    .width()
                    .clamp(limits::FLOAT_MIN_WIDTH, limits::FLOAT_MAX_WIDTH),
                panel_rect
                    .height()
                    .clamp(limits::FLOAT_MIN_HEIGHT, limits::FLOAT_MAX_HEIGHT),
            ),
        };
        self.drag = Some(Drag {
            kind,
            grab: (pointer - panel_rect.left_top()).min(float_size - vec2(24.0, 0.0)),
            float_size,
            pointer,
            sticky: false,
            armed: false,
            origin,
        });
        self.dirty = true;
    }

    /// Add a module by picking it up: it enters the layout already in the
    /// pointer's hand, and the drop chooses where it lands.
    ///
    /// This is what "add it, then put it where you want" means here. The
    /// alternative — dropping it at the bottom of a sidebar and leaving the
    /// user to find and move it — is [`Layout::open`], which the Window menu's
    /// checkboxes still use because a checkbox that flung the pointer into a
    /// drag would be a surprise.
    ///
    /// Edit mode is switched on rather than required: a module in mid-air is
    /// only meaningful in the mode where panels move, and the design pauses the
    /// canvas for exactly as long as one is. Returns false when the drag could
    /// not be started, which is only when one already is.
    pub fn add_dragging(&mut self, kind: PanelKind, pointer: Pos2) -> bool {
        if self.drag.is_some() {
            return false;
        }
        self.edit_mode = true;
        // Already-open modules are lifted from wherever they are rather than
        // duplicated — there is exactly one of each, which is the whole of why
        // the kind is the identity.
        let origin = self.take(kind).unwrap_or(Origin::Closed);
        let float_size = match origin {
            Origin::Float(rect) => rect.size(),
            // A module that has never been placed has no size of its own. The
            // dock's own width and a little over half a sidebar's height is
            // what it would get by being docked, which is where most of them
            // end up.
            Origin::Dock { .. } | Origin::Closed => vec2(
                metrics::PANEL.clamp(limits::FLOAT_MIN_WIDTH, limits::FLOAT_MAX_WIDTH),
                260.0_f32.clamp(limits::FLOAT_MIN_HEIGHT, limits::FLOAT_MAX_HEIGHT),
            ),
        };
        self.drag = Some(Drag {
            kind,
            // Held by the middle of its header, since there is no panel under
            // the pointer for the grab to be measured against.
            grab: vec2(float_size.x * 0.5, metrics::PANEL_HEADER * 0.5),
            float_size,
            pointer,
            sticky: true,
            armed: false,
            origin,
        });
        self.dirty = true;
        true
    }

    /// Whether the drag in progress should be dropped, given this frame's
    /// pointer state.
    ///
    /// An ordinary drag ends when the button that began it is let go. One
    /// started by a *click* cannot: that button is already up by the time the
    /// drag exists, and testing "no button down" would drop the module on the
    /// button that added it, on the very next frame. A sticky drag ends on the
    /// next press instead — and only after a frame in which the pointer was
    /// idle, because a click fast enough to press and release inside one frame
    /// reports both at once and would otherwise do exactly the same thing.
    pub fn drag_should_drop(&mut self, any_down: bool, any_pressed: bool) -> bool {
        let Some(drag) = &mut self.drag else {
            return false;
        };
        if !drag.sticky {
            return !any_down;
        }
        if !any_down && !any_pressed {
            drag.armed = true;
        }
        drag.armed && any_pressed
    }

    pub fn drag_to(&mut self, pointer: Pos2) {
        if let Some(drag) = &mut self.drag {
            drag.pointer = pointer;
        }
    }

    /// Release the dragged panel onto `target`.
    pub fn end_drag(&mut self, target: DropTarget) {
        let Some(drag) = self.drag.take() else { return };
        let weight = match drag.origin {
            Origin::Dock { weight, .. } => weight,
            Origin::Float(_) | Origin::Closed => drag.kind.default_weight(),
        };
        let docked = Docked {
            kind: drag.kind,
            weight,
        };
        match target {
            DropTarget::Dock {
                side,
                column,
                index,
            } => {
                let columns = &mut self.sides[side.index()];
                match columns.get_mut(column) {
                    Some(col) => {
                        let index = index.min(col.panels.len());
                        col.panels.insert(index, docked);
                    }
                    None => columns.push(Column::of(drag.kind)),
                }
            }
            DropTarget::NewColumn { side, column } => {
                // A panel that had a column keeps its width, so moving the tool
                // rail one place along does not silently widen it into an
                // ordinary module column. Clamped to what the module itself
                // will accept, since the width may have come from a column it
                // was sharing with something wider.
                let width = match drag.origin {
                    Origin::Dock { width, .. } => width,
                    Origin::Float(_) | Origin::Closed => drag.kind.default_width(),
                }
                .clamp(drag.kind.min_width(), limits::SIDEBAR_MAX_WIDTH);
                let columns = &mut self.sides[side.index()];
                let at = column.min(columns.len());
                columns.insert(
                    at,
                    Column {
                        panels: vec![docked],
                        width,
                    },
                );
            }
            DropTarget::Float => {
                self.floating.push(Floating {
                    kind: drag.kind,
                    rect: drag.float_rect(),
                });
            }
        }
        // After the insert, not before: the target's column index was computed
        // against a layout that still had the column the drag emptied in it.
        self.prune();
        self.dirty = true;
    }

    /// Abandon the drag and put the panel back where it came from.
    pub fn cancel_drag(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        self.put(drag.kind, drag.origin);
        self.prune();
        self.dirty = true;
    }

    // --- persistence -------------------------------------------------------

    /// Serialise to the config file's text form.
    ///
    /// Hand-rolled rather than serde: the whole format is six line shapes, and
    /// a parser that can be read in one screen is worth more here than a
    /// dependency plus derive macros on types that also carry drag state.
    ///
    /// The version header has not moved, and the two lines that used to be
    /// written and are not any more are why it did not have to. `width` said
    /// how wide a side was, which a `column` now says for each of its columns;
    /// `rail` said which edge the tool rail was on, and the rail is a module.
    /// Both are still *read* — see [`Self::from_config`].
    pub fn to_config(&self) -> String {
        let mut out = String::from("umber-layout 1\n");
        for side in Side::ALL {
            for column in &self.sides[side.index()] {
                // A column with nothing in it is a ghost a drag left behind and
                // is about to be pruned. Never write one out: reading it back
                // would be an empty strip nobody asked for.
                if column.panels.is_empty() {
                    continue;
                }
                out.push_str(&format!("column {} {:.0}\n", side.key(), column.width));
                for d in &column.panels {
                    out.push_str(&format!(
                        "dock {} {} {:.4}\n",
                        side.key(),
                        d.kind.key(),
                        d.weight
                    ));
                }
            }
        }
        for f in &self.floating {
            out.push_str(&format!(
                "float {} {:.1} {:.1} {:.1} {:.1}\n",
                f.kind.key(),
                f.rect.left(),
                f.rect.top(),
                f.rect.width(),
                f.rect.height()
            ));
        }
        out
    }

    /// Parse the config file's text form.
    ///
    /// Deliberately forgiving: unknown lines are skipped and a panel named
    /// twice keeps its first placement. A layout file is a convenience, and
    /// refusing to start over a stray line would be a poor trade. A wrong
    /// version header *is* fatal, because silently reinterpreting an older
    /// format is how a layout ends up subtly wrong instead of obviously reset.
    ///
    /// Two line shapes are read and no longer written, and between them they
    /// are the whole of why columns and the tool rail's promotion to a module
    /// cost nobody their arrangement:
    ///
    /// * **`width <side> <points>`** — how wide that side was, back when a side
    ///   *was* one column. It sets the width of the column a side implicitly
    ///   has, so a file with no `column` line at all loads as exactly the
    ///   single column it describes.
    /// * **`rail <side>`** — which edge the tool rail was on. The rail was
    ///   chrome and always present, so a file that names it is a file from
    ///   before it was a module, and it becomes a Tools column at that edge:
    ///   the outermost, which is where the rail was drawn. A file this build
    ///   writes never names it, so a Tools module the user has *removed* stays
    ///   removed rather than reappearing every launch.
    pub fn from_config(text: &str) -> Option<Self> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        if lines.next()? != "umber-layout 1" {
            return None;
        }

        let mut layout = Self {
            sides: [Vec::new(), Vec::new()],
            floating: Vec::new(),
            drag: None,
            edit_mode: false,
            dirty: false,
        };
        let mut seen: Vec<PanelKind> = Vec::new();
        // The width a side's implicit column takes, from a pre-columns `width`
        // line. Only ever consulted by a file that has no `column` line at all.
        let mut implied_width = [metrics::PANEL; 2];
        let mut legacy_rail: Option<Side> = None;

        for line in lines {
            let f: Vec<&str> = line.split_whitespace().collect();
            match f.as_slice() {
                ["rail", side] => legacy_rail = Side::from_key(side),
                ["width", side, value] => {
                    if let (Some(side), Ok(value)) = (Side::from_key(side), value.parse::<f32>()) {
                        implied_width[side.index()] = value
                            .clamp(limits::SIDEBAR_MIN_WIDTH, limits::SIDEBAR_MAX_WIDTH)
                            .round();
                    }
                }
                ["column", side, width] => {
                    let (Some(side), Ok(width)) = (Side::from_key(side), width.parse::<f32>())
                    else {
                        continue;
                    };
                    if !width.is_finite() {
                        continue;
                    }
                    layout.sides[side.index()].push(Column {
                        panels: Vec::new(),
                        // The floor is applied once the column's panels are
                        // known, below: a Tools column is legitimately narrower
                        // than any other, and which it is cannot be told yet.
                        width: width.clamp(0.0, limits::SIDEBAR_MAX_WIDTH).round(),
                    });
                }
                ["dock", side, kind, weight] => {
                    let (Some(side), Some(kind)) =
                        (Side::from_key(side), PanelKind::from_key(kind))
                    else {
                        continue;
                    };
                    if seen.contains(&kind) {
                        continue;
                    }
                    seen.push(kind);
                    let columns = &mut layout.sides[side.index()];
                    if columns.is_empty() {
                        // A pre-columns file: every dock line on this side
                        // belongs to the one column it implicitly has.
                        columns.push(Column {
                            panels: Vec::new(),
                            width: implied_width[side.index()],
                        });
                    }
                    let weight = weight.parse::<f32>().unwrap_or(1.0).clamp(0.0, 1000.0);
                    columns
                        .last_mut()
                        .expect("just ensured")
                        .panels
                        .push(Docked { kind, weight });
                }
                ["float", kind, x, y, w, h] => {
                    let Some(kind) = PanelKind::from_key(kind) else {
                        continue;
                    };
                    if seen.contains(&kind) {
                        continue;
                    }
                    let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
                        x.parse::<f32>(),
                        y.parse::<f32>(),
                        w.parse::<f32>(),
                        h.parse::<f32>(),
                    ) else {
                        continue;
                    };
                    if ![x, y, w, h].iter().all(|v| v.is_finite()) {
                        continue;
                    }
                    seen.push(kind);
                    layout.floating.push(Floating {
                        kind,
                        rect: Rect::from_min_size(
                            pos2(x, y),
                            vec2(
                                w.clamp(limits::FLOAT_MIN_WIDTH, limits::FLOAT_MAX_WIDTH),
                                h.clamp(limits::FLOAT_MIN_HEIGHT, limits::FLOAT_MAX_HEIGHT),
                            ),
                        ),
                    });
                }
                _ => {}
            }
        }

        // A file from before the rail was a module. It says the rail exists and
        // which edge it was on, so it opens with one — outermost on that side,
        // which is where it was drawn.
        if let Some(side) = legacy_rail
            && !seen.contains(&PanelKind::Tools)
        {
            layout.sides[side.index()].insert(0, Column::of(PanelKind::Tools));
        }

        // A `column` line with nothing under it — a truncated write, or a file
        // somebody edited. An empty strip in the workspace is worse than a
        // column that was never there.
        layout.prune();
        // And now that every column's contents are known, hold each to the
        // floor of the widest module in it.
        for side in &mut layout.sides {
            for column in side.iter_mut() {
                column.width = column
                    .width
                    .clamp(column.min_width(), limits::SIDEBAR_MAX_WIDTH)
                    .round();
            }
        }

        Some(layout)
    }

    /// Read the saved layout, falling back to the default if anything is amiss.
    pub fn load_or_default() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match Self::from_config(&text) {
                Some(layout) => layout,
                None => {
                    log::warn!(
                        "{} is not a layout Umber understands; using the default",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                log::warn!("could not read {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Write the layout out if it has changed since the last write.
    ///
    /// A failure here is logged and forgotten: losing a window arrangement is
    /// not worth interrupting a painting session over.
    pub fn save_if_dirty(&mut self) {
        if !self.dirty || self.drag.is_some() {
            return;
        }
        self.dirty = false;
        let Some(path) = config_path() else { return };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            log::warn!("could not create {}: {e}", parent.display());
            return;
        }
        if let Err(e) = std::fs::write(&path, self.to_config()) {
            log::warn!("could not write {}: {e}", path.display());
        }
    }
}

/// Where the layout file lives.
///
/// Rolled by hand rather than pulling in a directories crate for one path:
/// three `env` lookups is the whole of it, and the fallback (no config
/// directory, so no persistence) is harmless.
fn config_path() -> Option<PathBuf> {
    let dir = if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("APPDATA")?).join("Umber")
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/Umber")
    } else {
        match std::env::var_os("XDG_CONFIG_HOME") {
            Some(base) => PathBuf::from(base).join("umber"),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".config/umber"),
        }
    };
    Some(dir.join("layout.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    /// The shipped arrangement's one column, which most of these work against.
    const DOCK: usize = 0;

    fn weights(layout: &Layout, side: Side, column: usize) -> Vec<f32> {
        layout
            .docked(side, column)
            .iter()
            .map(|d| d.weight)
            .collect()
    }

    fn kinds(layout: &Layout, side: Side, column: usize) -> Vec<PanelKind> {
        layout.docked(side, column).iter().map(|d| d.kind).collect()
    }

    #[test]
    fn stack_heights_fill_exactly() {
        let h = stack_heights(&[3.0, 1.0, 2.0], 600.0);
        assert_eq!(h.len(), 3);
        assert!((h.iter().sum::<f32>() - 600.0).abs() < 1e-2);
        assert!(h[0] > h[2] && h[2] > h[1]);
    }

    #[test]
    fn stack_heights_honour_the_minimum() {
        // A weight of zero must still leave a usable panel.
        let h = stack_heights(&[100.0, 0.0], 500.0);
        assert!(h[1] >= limits::PANEL_MIN_HEIGHT - 1e-3, "{h:?}");
        assert!((h.iter().sum::<f32>() - 500.0).abs() < 1e-2);
    }

    #[test]
    fn stack_heights_share_equally_when_starved() {
        let h = stack_heights(&[5.0, 1.0, 1.0], 60.0);
        assert!(h.iter().all(|v| (v - 20.0).abs() < 1e-3), "{h:?}");
    }

    #[test]
    fn dragging_a_splitter_round_trips() {
        // The heights the user drags to must be the heights they get back, or
        // the stack creeps every frame.
        let mut layout = Layout::default();
        let available = 700.0;
        let heights = stack_heights(&weights(&layout, Side::Right, DOCK), available);

        layout.resize_split(Side::Right, DOCK, 0, 40.0, &heights);

        let replayed = stack_heights(&weights(&layout, Side::Right, DOCK), available);
        assert!(
            (replayed[0] - (heights[0] + 40.0)).abs() < 1e-2,
            "{replayed:?}"
        );
        assert!(
            (replayed[1] - (heights[1] - 40.0)).abs() < 1e-2,
            "{replayed:?}"
        );
        assert!((replayed[2] - heights[2]).abs() < 1e-2, "{replayed:?}");
    }

    #[test]
    fn a_splitter_cannot_destroy_a_panel() {
        let mut layout = Layout::default();
        let available = 700.0;
        let heights = stack_heights(&weights(&layout, Side::Right, DOCK), available);

        // Shove the boundary far past the bottom panel's minimum.
        layout.resize_split(Side::Right, DOCK, 0, 10_000.0, &heights);

        let replayed = stack_heights(&weights(&layout, Side::Right, DOCK), available);
        assert!(
            replayed
                .iter()
                .all(|h| *h >= limits::PANEL_MIN_HEIGHT - 1e-2),
            "{replayed:?}"
        );
        assert!((replayed.iter().sum::<f32>() - available).abs() < 1e-2);
    }

    /// A splitter drag belongs to one column and must leave the others alone.
    #[test]
    fn a_splitter_only_moves_its_own_column() {
        let mut layout = two_columns_on_the_right();
        let before = weights(&layout, Side::Right, 0);
        let inner = stack_heights(&weights(&layout, Side::Right, 1), 700.0);
        layout.resize_split(Side::Right, 1, 0, 30.0, &inner);
        assert_eq!(weights(&layout, Side::Right, 0), before);
    }

    /// Layers pulled out into a column of its own at the very edge, with the
    /// other two inside it.
    fn two_columns_on_the_right() -> Layout {
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        layout.begin_drag(
            PanelKind::Layers,
            pos2(1200.0, 700.0),
            rect(1176.0, 680.0, 264.0, 200.0),
        );
        layout.end_drag(DropTarget::NewColumn {
            side: Side::Right,
            column: 0,
        });
        layout.set_edit_mode(false);
        assert_eq!(layout.columns(Side::Right).len(), 2);
        layout
    }

    #[test]
    fn a_column_width_is_clamped() {
        let mut layout = Layout::default();
        layout.set_width(Side::Right, DOCK, 10.0);
        assert_eq!(layout.width(Side::Right, DOCK), limits::SIDEBAR_MIN_WIDTH);
        layout.set_width(Side::Right, DOCK, 9999.0);
        assert_eq!(layout.width(Side::Right, DOCK), limits::SIDEBAR_MAX_WIDTH);
    }

    #[test]
    fn geometry_leaves_the_canvas_between_the_columns() {
        let layout = Layout::default();
        let geo = layout.geometry(rect(0.0, 70.0, 1440.0, 800.0));
        let rail = geo.columns(Side::Left);
        assert_eq!(rail.len(), 1, "the shipped rail");
        assert_eq!(rail[0].rect.left(), 0.0);
        assert_eq!(rail[0].rect.width(), limits::SIDEBAR_MIN_WIDTH.round());
        let right = geo
            .sidebar(Side::Right)
            .expect("default docks on the right");
        assert_eq!(right.right(), 1440.0);
        assert_eq!(geo.canvas.left(), rail[0].rect.right());
        assert_eq!(geo.canvas.right(), right.left());
        assert_eq!(geo.columns(Side::Right)[0].slots.len(), 3);
    }

    /// Two columns on one side sit side by side, outermost first, and the
    /// canvas is what is left inside both.
    #[test]
    fn two_columns_on_a_side_stack_inwards_from_the_edge() {
        let layout = two_columns_on_the_right();
        assert_eq!(kinds(&layout, Side::Right, 0), vec![PanelKind::Layers]);
        assert_eq!(
            kinds(&layout, Side::Right, 1),
            vec![PanelKind::Colour, PanelKind::Brushes]
        );

        let geo = layout.geometry(rect(0.0, 70.0, 1440.0, 800.0));
        let cols = geo.columns(Side::Right);
        assert_eq!(cols[0].rect.right(), 1440.0, "column 0 is at the edge");
        assert_eq!(cols[1].rect.right(), cols[0].rect.left());
        assert_eq!(geo.canvas.right(), cols[1].rect.left());
        // Both are full height, which is the whole point of the arrangement.
        assert_eq!(cols[0].rect.height(), cols[1].rect.height());
    }

    /// Every rect the layout hands out has to survive a window dragged down to
    /// nothing. A `Rect` with its max left of its min does not panic — it draws
    /// in the wrong place, or fills the panel it was meant to sit inside — so
    /// this is the kind of bug that only shows up as "the interface looked odd
    /// for a moment while I resized".
    ///
    /// Run over the default layout, over one holding every module there is, and
    /// over one with several columns a side, since each column is another
    /// minimum to fit into the same sliver of window.
    #[test]
    fn a_window_too_small_for_the_chrome_still_produces_sane_rects() {
        let mut crowded = Layout::default();
        for kind in PanelKind::ALL {
            crowded.open(kind);
        }

        let mut columned = Layout::default();
        columned.set_edit_mode(true);
        for (kind, side, column) in [
            (PanelKind::Layers, Side::Right, 0),
            (PanelKind::Brushes, Side::Left, 0),
            (PanelKind::History, Side::Left, 1),
        ] {
            columned.open(kind);
            columned.begin_drag(kind, pos2(0.0, 0.0), rect(0.0, 0.0, 264.0, 200.0));
            columned.end_drag(DropTarget::NewColumn { side, column });
        }

        for layout in [Layout::default(), crowded, columned] {
            small_window_sweep(&layout);
        }
    }

    fn small_window_sweep(layout: &Layout) {
        for (w, h) in [
            (0.0, 0.0),
            (10.0, 400.0),
            (76.0, 400.0),
            (200.0, 5.0),
            (240.0, 240.0),
            // Shorter than the strips: what reaches `geometry` is inverted.
            (900.0, -60.0),
        ] {
            let geo = layout.geometry(rect(0.0, 70.0, w, h));
            let mut all = vec![geo.workspace, geo.canvas];
            for side in Side::ALL {
                all.extend(geo.sidebar(side));
                all.push(geo.drop_zone(side));
                for column in 0..=geo.columns(side).len() {
                    all.push(geo.new_column_strip(side, column));
                }
                for column in geo.columns(side) {
                    all.push(column.rect);
                    all.extend(column.slots.iter().copied());
                }
            }
            for r in all {
                assert!(
                    r.width() >= 0.0 && r.height() >= 0.0,
                    "{w}×{h} produced {r:?}",
                );
            }
            // And a drop still resolves to something rather than panicking on
            // the way through the empty slot list.
            let _ = geo.drop_target(pos2(0.0, 0.0));
            let _ = geo.insertion_line(Side::Right, 0, 0);
        }
    }

    #[test]
    fn floating_panels_do_not_shrink_the_canvas() {
        // The camera pivot comes from the canvas region, so a panel that hovers
        // must not change it. This is the invariant that keeps a dab under the
        // cursor.
        let mut layout = Layout::default();
        let workspace = rect(0.0, 70.0, 1440.0, 800.0);
        let before = layout.geometry(workspace).canvas;

        layout.close(PanelKind::Layers);
        layout.floating.push(Floating {
            kind: PanelKind::Layers,
            rect: rect(600.0, 300.0, 260.0, 300.0),
        });
        let after = layout.geometry(workspace).canvas;

        assert_eq!(before, after);
    }

    #[test]
    fn dropping_over_a_column_picks_the_slot_under_the_pointer() {
        let layout = Layout::default();
        let geo = layout.geometry(rect(0.0, 0.0, 1440.0, 900.0));
        let slots = &geo.columns(Side::Right)[0].slots;

        // Top half of the first slot inserts above it. Horizontally in the
        // middle of the column, away from the new-column bands.
        let p = pos2(slots[0].center().x, slots[0].top() + 4.0);
        assert_eq!(
            geo.drop_target(p),
            DropTarget::Dock {
                side: Side::Right,
                column: 0,
                index: 0
            }
        );
        // Bottom half of the last slot appends.
        let last = slots.last().unwrap();
        let p = pos2(last.center().x, last.bottom() - 4.0);
        assert_eq!(
            geo.drop_target(p),
            DropTarget::Dock {
                side: Side::Right,
                column: 0,
                index: slots.len()
            }
        );
    }

    /// The edges of a column are where a *new* column is asked for, and the two
    /// sides of one boundary have to name the same index or the indicator would
    /// jump as the pointer crossed it.
    #[test]
    fn the_edge_of_a_column_asks_for_a_new_one() {
        let layout = two_columns_on_the_right();
        let geo = layout.geometry(rect(0.0, 0.0, 1440.0, 900.0));
        let cols = geo.columns(Side::Right);

        // Hard against the window edge: outside everything docked.
        assert_eq!(
            geo.drop_target(pos2(1439.0, 400.0)),
            DropTarget::NewColumn {
                side: Side::Right,
                column: 0
            }
        );
        // The boundary between the two columns, from either side of it.
        let boundary = cols[0].rect.left();
        for x in [boundary + 2.0, boundary - 2.0] {
            assert_eq!(
                geo.drop_target(pos2(x, 400.0)),
                DropTarget::NewColumn {
                    side: Side::Right,
                    column: 1
                },
                "at {x}"
            );
        }
        // And inside the innermost column, against the canvas.
        assert_eq!(
            geo.drop_target(pos2(cols[1].rect.left() + 2.0, 400.0)),
            DropTarget::NewColumn {
                side: Side::Right,
                column: 2
            }
        );
    }

    #[test]
    fn an_empty_side_still_accepts_a_drop() {
        let mut layout = Layout::default();
        layout.close(PanelKind::Tools);
        let geo = layout.geometry(rect(0.0, 0.0, 1440.0, 900.0));
        assert!(geo.columns(Side::Left).is_empty());
        let zone = geo.drop_zone(Side::Left);
        assert_eq!(
            geo.drop_target(zone.center()),
            DropTarget::NewColumn {
                side: Side::Left,
                column: 0
            }
        );
    }

    #[test]
    fn the_middle_of_the_canvas_floats() {
        let layout = Layout::default();
        let geo = layout.geometry(rect(0.0, 0.0, 1440.0, 900.0));
        assert_eq!(geo.drop_target(geo.canvas.center()), DropTarget::Float);
    }

    #[test]
    fn a_drag_lifts_the_panel_and_puts_it_back() {
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        let before = layout.to_config();

        layout.begin_drag(
            PanelKind::Brushes,
            pos2(1200.0, 400.0),
            rect(1176.0, 380.0, 264.0, 200.0),
        );
        assert!(layout.is_dragging());
        assert!(!layout.is_open(PanelKind::Brushes));
        assert_eq!(layout.docked(Side::Right, DOCK).len(), 2);

        layout.cancel_drag();
        assert!(!layout.is_dragging());
        assert_eq!(layout.to_config(), before);
    }

    /// A drag that empties a column leaves it standing until the drop resolves
    /// — otherwise every column outside it slides sideways under the pointer,
    /// and the column indices the drop is computed against renumber mid-drag.
    #[test]
    fn a_column_emptied_by_a_drag_survives_until_the_drop() {
        let mut layout = two_columns_on_the_right();
        layout.set_edit_mode(true);

        layout.begin_drag(
            PanelKind::Layers,
            pos2(1400.0, 200.0),
            rect(1176.0, 180.0, 264.0, 200.0),
        );
        assert_eq!(
            layout.columns(Side::Right).len(),
            2,
            "the column it emptied is still there"
        );
        assert!(layout.columns(Side::Right)[0].panels.is_empty());
        // And the empty one is never written down.
        assert_eq!(layout.to_config().matches("column right").count(), 1);

        layout.end_drag(DropTarget::Float);
        assert_eq!(layout.columns(Side::Right).len(), 1, "and then it goes");
    }

    #[test]
    fn a_drag_can_move_a_panel_across_sides() {
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        layout.begin_drag(
            PanelKind::Layers,
            pos2(1200.0, 700.0),
            rect(1176.0, 680.0, 264.0, 200.0),
        );
        layout.end_drag(DropTarget::NewColumn {
            side: Side::Left,
            column: 0,
        });
        assert_eq!(kinds(&layout, Side::Left, 0), vec![PanelKind::Layers]);
        assert_eq!(layout.docked(Side::Right, DOCK).len(), 2);
    }

    #[test]
    fn a_drag_can_tear_a_panel_off_and_re_dock_it() {
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        layout.begin_drag(
            PanelKind::Colour,
            pos2(1200.0, 200.0),
            rect(1176.0, 180.0, 264.0, 340.0),
        );
        layout.drag_to(pos2(600.0, 400.0));
        layout.end_drag(DropTarget::Float);
        assert_eq!(layout.floating().len(), 1);
        assert_eq!(layout.docked(Side::Right, DOCK).len(), 2);

        let rect = layout.floating()[0].rect;
        layout.begin_drag(PanelKind::Colour, rect.center(), rect);
        layout.end_drag(DropTarget::Dock {
            side: Side::Right,
            column: DOCK,
            index: 0,
        });
        assert!(layout.floating().is_empty());
        assert_eq!(layout.docked(Side::Right, DOCK)[0].kind, PanelKind::Colour);
    }

    /// A column moved somewhere else keeps the width it was dragged to. Losing
    /// it would make every move a silent resize.
    #[test]
    fn a_column_dropped_somewhere_else_keeps_its_width() {
        let mut layout = two_columns_on_the_right();
        layout.set_width(Side::Right, 0, 210.0);
        layout.set_edit_mode(true);

        layout.begin_drag(
            PanelKind::Layers,
            pos2(1400.0, 200.0),
            rect(1176.0, 180.0, 210.0, 200.0),
        );
        layout.end_drag(DropTarget::NewColumn {
            side: Side::Left,
            column: 0,
        });
        assert_eq!(layout.width(Side::Left, 0), 210.0);
        assert_eq!(kinds(&layout, Side::Left, 0), vec![PanelKind::Layers]);
    }

    #[test]
    fn closing_and_reopening_keeps_one_of_each() {
        let mut layout = Layout::default();
        layout.close(PanelKind::Brushes);
        assert!(!layout.is_open(PanelKind::Brushes));
        layout.open(PanelKind::Brushes);
        layout.open(PanelKind::Brushes);
        let count: usize = Side::ALL
            .iter()
            .flat_map(|s| layout.columns(*s))
            .map(|c| {
                c.panels
                    .iter()
                    .filter(|d| d.kind == PanelKind::Brushes)
                    .count()
            })
            .sum();
        assert_eq!(count, 1);
    }

    /// Closing the only module in a column takes the column with it, or the
    /// workspace keeps a blank strip nobody can fill or remove.
    #[test]
    fn closing_the_last_module_in_a_column_removes_the_column() {
        let mut layout = two_columns_on_the_right();
        layout.close(PanelKind::Layers);
        assert_eq!(layout.columns(Side::Right).len(), 1);
        assert_eq!(
            kinds(&layout, Side::Right, 0),
            vec![PanelKind::Colour, PanelKind::Brushes]
        );
    }

    #[test]
    fn config_round_trips() {
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        layout.set_width(Side::Right, DOCK, 300.0);
        layout.begin_drag(
            PanelKind::Colour,
            pos2(1200.0, 200.0),
            rect(1176.0, 180.0, 264.0, 340.0),
        );
        layout.drag_to(pos2(500.0, 300.0));
        layout.end_drag(DropTarget::Float);
        layout.begin_drag(
            PanelKind::Layers,
            pos2(1200.0, 700.0),
            rect(1176.0, 680.0, 264.0, 200.0),
        );
        layout.end_drag(DropTarget::NewColumn {
            side: Side::Left,
            column: 0,
        });

        let text = layout.to_config();
        let back = Layout::from_config(&text).expect("valid config");
        assert_eq!(back.to_config(), text);
        assert_eq!(back.columns(Side::Left), layout.columns(Side::Left));
        assert_eq!(back.columns(Side::Right), layout.columns(Side::Right));
        assert_eq!(back.floating(), layout.floating());
    }

    /// Several columns a side, with their own widths, have to come back in the
    /// same order — outermost first — or the workspace mirrors itself on the
    /// next launch.
    #[test]
    fn several_columns_a_side_round_trip_in_order() {
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        for (kind, column) in [(PanelKind::Colour, 0), (PanelKind::Brushes, 1)] {
            layout.begin_drag(kind, pos2(0.0, 0.0), rect(0.0, 0.0, 264.0, 200.0));
            layout.end_drag(DropTarget::NewColumn {
                side: Side::Right,
                column,
            });
        }
        layout.set_width(Side::Right, 0, 240.0);
        layout.set_width(Side::Right, 1, 320.0);
        assert_eq!(layout.columns(Side::Right).len(), 3);

        let text = layout.to_config();
        let back = Layout::from_config(&text).expect("valid config");
        assert_eq!(back.to_config(), text);
        assert_eq!(kinds(&back, Side::Right, 0), vec![PanelKind::Colour]);
        assert_eq!(kinds(&back, Side::Right, 1), vec![PanelKind::Brushes]);
        assert_eq!(back.width(Side::Right, 0), 240.0);
        assert_eq!(back.width(Side::Right, 1), 320.0);
    }

    #[test]
    fn a_config_from_another_version_is_refused() {
        assert!(Layout::from_config("umber-layout 9\nrail left\n").is_none());
        assert!(Layout::from_config("").is_none());
    }

    #[test]
    fn a_damaged_config_loses_only_the_damaged_lines() {
        let text = "umber-layout 1\n\
                    rail sideways\n\
                    column right not-a-number\n\
                    column right 300\n\
                    dock right colour 3\n\
                    dock right colour 9\n\
                    column left 200\n\
                    dock left brushes 1\n\
                    column left 250\n\
                    nonsense here\n";
        let layout = Layout::from_config(text).expect("still loads");
        assert_eq!(layout.width(Side::Right, 0), 300.0);
        assert_eq!(
            layout.docked(Side::Right, 0).len(),
            1,
            "the duplicate is dropped"
        );
        assert_eq!(layout.docked(Side::Left, 0).len(), 1);
        assert_eq!(
            layout.columns(Side::Left).len(),
            1,
            "a column with nothing under it is not a column"
        );
        assert!(
            !layout.is_open(PanelKind::Layers),
            "an absent panel is closed"
        );
    }

    #[test]
    fn a_stranded_floating_panel_is_pulled_back() {
        let mut layout = Layout::default();
        layout.close(PanelKind::Layers);
        layout.floating.push(Floating {
            kind: PanelKind::Layers,
            rect: rect(4000.0, 3000.0, 260.0, 300.0),
        });
        let workspace = rect(0.0, 70.0, 1440.0, 800.0);
        layout.clamp_floating(workspace);

        let r = layout.floating()[0].rect;
        assert!(r.left() <= workspace.right() - limits::FLOAT_KEEP_VISIBLE);
        assert!(r.top() <= workspace.bottom() - metrics::PANEL_HEADER);
        assert!(r.top() >= workspace.top());
    }

    #[test]
    fn edit_mode_pauses_the_canvas_and_a_drag_keeps_it_paused() {
        // A panel dragged across the canvas must not leave paint behind it.
        let mut layout = Layout::default();
        assert!(!layout.blocks_canvas(pos2(600.0, 400.0)));

        layout.set_edit_mode(true);
        assert!(
            layout.blocks_canvas(pos2(600.0, 400.0)),
            "edit mode pauses it"
        );

        layout.begin_drag(
            PanelKind::Colour,
            pos2(1200.0, 200.0),
            rect(1176.0, 180.0, 264.0, 340.0),
        );
        layout.drag_to(pos2(600.0, 400.0));
        assert!(layout.blocks_canvas(pos2(600.0, 400.0)));
    }

    #[test]
    fn panels_are_locked_outside_edit_mode() {
        let mut layout = Layout::default();
        layout.begin_drag(
            PanelKind::Colour,
            pos2(1200.0, 200.0),
            rect(1176.0, 180.0, 264.0, 340.0),
        );
        assert!(!layout.is_dragging());
        assert!(layout.is_open(PanelKind::Colour), "and it stayed put");
    }

    #[test]
    fn leaving_edit_mode_abandons_a_drag_in_progress() {
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        let before = layout.to_config();
        layout.begin_drag(
            PanelKind::Brushes,
            pos2(1200.0, 400.0),
            rect(1176.0, 380.0, 264.0, 200.0),
        );
        layout.drag_to(pos2(600.0, 400.0));
        layout.set_edit_mode(false);
        assert!(!layout.is_dragging());
        assert_eq!(layout.to_config(), before, "the panel went back");
    }

    /// The module Umber does not ship in the default arrangement still has to
    /// survive being placed and written out.
    #[test]
    fn the_history_module_round_trips_through_the_config() {
        let mut layout = Layout::default();
        assert!(
            !layout.is_open(PanelKind::History),
            "History is not in the shipped arrangement",
        );
        layout.set_edit_mode(true);
        layout.open(PanelKind::History);
        layout.begin_drag(
            PanelKind::History,
            pos2(1200.0, 700.0),
            rect(1176.0, 680.0, 264.0, 200.0),
        );
        layout.end_drag(DropTarget::NewColumn {
            side: Side::Left,
            column: 0,
        });

        let text = layout.to_config();
        assert!(text.contains("history"), "{text}");
        let back = Layout::from_config(&text).expect("valid config");
        assert_eq!(back.to_config(), text);
        assert_eq!(back.docked(Side::Left, 0)[0].kind, PanelKind::History);
    }

    /// A layout file written before the History module existed names three
    /// panels and knows nothing of a fourth. It must load exactly as it did —
    /// with History closed, which is where the shipped arrangement puts it too,
    /// so an upgraded workspace and a fresh one agree. That is the reason the
    /// version header did not have to move; see `PanelKind::DEFAULT_DOCK`.
    ///
    /// It also predates columns, so its one list per side has to arrive as one
    /// column per side, at the width the `width` line gives — which is the
    /// whole of what "no version bump" has to mean for the column change.
    #[test]
    fn a_config_written_before_columns_loads_as_one_column_a_side() {
        let text = "umber-layout 1\n\
                    rail left\n\
                    width left 264\n\
                    dock left history 2\n\
                    width right 300\n\
                    dock right colour 3\n\
                    dock right brushes 1.3\n\
                    dock right layers 2.2\n";
        let layout = Layout::from_config(text).expect("still loads");

        assert_eq!(
            layout.columns(Side::Right).len(),
            1,
            "one column, as before"
        );
        assert_eq!(
            kinds(&layout, Side::Right, 0),
            vec![PanelKind::Colour, PanelKind::Brushes, PanelKind::Layers]
        );
        assert_eq!(layout.width(Side::Right, 0), 300.0, "and at its own width");
        // The stack's own proportions come back untouched.
        assert_eq!(weights(&layout, Side::Right, 0), vec![3.0, 1.3, 2.2]);

        // The left side had the rail *and* a module, and the rail was drawn
        // outside the sidebar, so it arrives as the outer column of two.
        assert_eq!(layout.columns(Side::Left).len(), 2);
        assert_eq!(kinds(&layout, Side::Left, 0), vec![PanelKind::Tools]);
        assert_eq!(kinds(&layout, Side::Left, 1), vec![PanelKind::History]);
        assert_eq!(layout.width(Side::Left, 1), 264.0);
    }

    /// The tool rail used to be chrome, so a file from before it was a module
    /// says only which edge it was on. It must open with the rail on that edge:
    /// an upgraded workspace losing its tools would be exactly the divergence
    /// the version header exists to prevent, and it is the reason Tools could
    /// join `DEFAULT_DOCK` without one.
    #[test]
    fn a_config_written_before_the_rail_was_a_module_still_has_a_rail() {
        for (side, other) in [(Side::Left, Side::Right), (Side::Right, Side::Left)] {
            let text = format!(
                "umber-layout 1\nrail {}\nwidth right 264\ndock right colour 3\n",
                side.key()
            );
            let layout = Layout::from_config(&text).expect("still loads");
            assert!(layout.is_open(PanelKind::Tools), "{side:?}");
            assert_eq!(
                kinds(&layout, side, 0),
                vec![PanelKind::Tools],
                "outermost on {side:?}, which is where it was drawn",
            );
            assert_eq!(
                layout.width(side, 0),
                limits::SIDEBAR_MIN_WIDTH.round(),
                "at the width a fresh install gives it, not the old rail's",
            );
            // And nothing was invented on the other edge.
            assert!(!kinds(&layout, other, 0).contains(&PanelKind::Tools));
        }
    }

    /// The other half of the same rule: once the rail is a module, removing it
    /// has to stick. Nothing this build writes names a rail, so nothing
    /// resurrects it on the next launch.
    #[test]
    fn a_removed_tool_rail_stays_removed_across_a_save() {
        let mut layout = Layout::default();
        layout.close(PanelKind::Tools);
        let text = layout.to_config();
        assert!(!text.contains("rail"), "{text}");
        let back = Layout::from_config(&text).expect("valid config");
        assert!(!back.is_open(PanelKind::Tools));
    }

    /// The rail's column opens at an ordinary column's minimum and is then the
    /// user's, either way.
    ///
    /// It used to open at `metrics::TOOL_RAIL`, which is also its floor, so the
    /// splitter had nowhere to go but outwards — the rail arrived at the width
    /// the chrome version happened to have and half its handle did nothing.
    /// Pinned in both directions because either alone would pass while the
    /// column was still stuck: a start equal to the floor satisfies "it can be
    /// widened", and a floor equal to the start satisfies "it starts at 190".
    ///
    /// Both routes in, because a fresh install and an upgraded workspace
    /// arriving at different widths is the divergence the config's version
    /// header exists to prevent — and the rail is the one module that reaches
    /// `DEFAULT_DOCK` by both.
    #[test]
    fn the_rails_column_starts_where_a_column_bottoms_out_and_moves_both_ways() {
        let migrated =
            Layout::from_config("umber-layout 1\nrail left\ndock right colour 3\n").expect("loads");
        for mut layout in [Layout::default(), migrated] {
            assert_eq!(kinds(&layout, Side::Left, 0), vec![PanelKind::Tools]);
            assert_eq!(layout.width(Side::Left, 0), limits::SIDEBAR_MIN_WIDTH);

            layout.set_width(Side::Left, 0, limits::SIDEBAR_MIN_WIDTH + 60.0);
            assert_eq!(
                layout.width(Side::Left, 0),
                limits::SIDEBAR_MIN_WIDTH + 60.0,
                "and it widens",
            );
            layout.set_width(Side::Left, 0, limits::SIDEBAR_MIN_WIDTH - 40.0);
            assert_eq!(
                layout.width(Side::Left, 0),
                limits::SIDEBAR_MIN_WIDTH - 40.0,
                "and it narrows, because the floor is the rail's own",
            );
        }
    }

    /// The rail's column may be dragged narrower than any other, because that
    /// is the whole of what a rail is — and dropping an ordinary module into it
    /// raises the floor again, so a picker is never three buttons across.
    #[test]
    fn the_rails_column_may_be_narrower_than_a_modules() {
        let mut layout = Layout::default();
        assert!(PanelKind::Tools.min_width() < limits::SIDEBAR_MIN_WIDTH);

        layout.set_width(Side::Left, 0, 10.0);
        assert_eq!(layout.width(Side::Left, 0), PanelKind::Tools.min_width());

        layout.set_edit_mode(true);
        layout.begin_drag(
            PanelKind::Colour,
            pos2(1200.0, 200.0),
            rect(1176.0, 180.0, 264.0, 340.0),
        );
        layout.end_drag(DropTarget::Dock {
            side: Side::Left,
            column: 0,
            index: 1,
        });
        layout.set_width(Side::Left, 0, 0.0);
        assert_eq!(layout.width(Side::Left, 0), limits::SIDEBAR_MIN_WIDTH);
    }

    /// The edit-mode remove control. Whatever else it does, a removed module
    /// must be able to come back — which is what the module library is for.
    #[test]
    fn a_removed_module_can_be_added_back() {
        let mut layout = Layout::default();
        layout.close(PanelKind::Layers);
        assert!(!layout.is_open(PanelKind::Layers));
        assert_eq!(layout.docked(Side::Right, DOCK).len(), 2);

        layout.add_dragging(PanelKind::Layers, pos2(600.0, 400.0));
        layout.end_drag(DropTarget::Dock {
            side: Side::Right,
            column: DOCK,
            index: 2,
        });
        assert!(layout.is_open(PanelKind::Layers));
        assert_eq!(layout.docked(Side::Right, DOCK).len(), 3);
    }

    /// Adding from the library leaves the module in the pointer's hand, in the
    /// mode where it can be put down.
    #[test]
    fn adding_a_module_picks_it_up_ready_to_be_dropped() {
        let mut layout = Layout::default();
        assert!(!layout.edit_mode());

        assert!(layout.add_dragging(PanelKind::History, pos2(600.0, 400.0)));
        assert!(layout.edit_mode(), "a module in the air needs the mode");
        assert!(layout.is_dragging());
        let drag = layout.drag().expect("a drag");
        assert_eq!(drag.kind, PanelKind::History);
        assert!(drag.sticky, "the button that added it is already up");
        assert!(
            !layout.is_open(PanelKind::History),
            "it is in the air, not in the layout",
        );

        // A second request while one is in flight is refused rather than
        // replacing the module the user is already holding.
        assert!(!layout.add_dragging(PanelKind::Colour, pos2(10.0, 10.0)));

        layout.drag_to(pos2(200.0, 500.0));
        layout.end_drag(DropTarget::Float);
        assert_eq!(layout.floating().len(), 1);
        assert_eq!(layout.floating()[0].kind, PanelKind::History);
    }

    /// Escape during the pick-up abandons the add, rather than dropping the
    /// module somewhere the user never chose.
    #[test]
    fn cancelling_an_add_leaves_the_layout_as_it_was() {
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        let before = layout.to_config();

        layout.add_dragging(PanelKind::History, pos2(600.0, 400.0));
        layout.cancel_drag();

        assert!(!layout.is_open(PanelKind::History));
        assert_eq!(layout.to_config(), before);
    }

    /// And the same for a module lifted out of a column of its own: cancelling
    /// has to put the column back, not leave the side one short.
    #[test]
    fn cancelling_a_drag_out_of_a_lone_column_puts_the_column_back() {
        let mut layout = two_columns_on_the_right();
        layout.set_edit_mode(true);
        let before = layout.to_config();

        layout.begin_drag(
            PanelKind::Layers,
            pos2(1400.0, 200.0),
            rect(1176.0, 180.0, 264.0, 200.0),
        );
        layout.drag_to(pos2(600.0, 400.0));
        layout.cancel_drag();

        assert_eq!(layout.to_config(), before);
        assert_eq!(layout.columns(Side::Right).len(), 2);
    }

    /// A sticky drag cannot end on a release — the release that started it has
    /// already happened. It ends on the next press, and not before the pointer
    /// has been seen up, or a click fast enough to press and release inside one
    /// frame would drop the module on the button that added it.
    #[test]
    fn a_picked_up_module_waits_for_the_click_that_puts_it_down() {
        let mut layout = Layout::default();
        layout.add_dragging(PanelKind::History, pos2(600.0, 400.0));

        // The same frame the library was clicked on: press and release both
        // reported, and nothing must be dropped.
        assert!(!layout.drag_should_drop(false, true));
        // Pointer idle over the canvas.
        assert!(!layout.drag_should_drop(false, false));
        // And now a real press.
        assert!(layout.drag_should_drop(true, true));

        // An ordinary drag is unchanged: it ends when the button comes up.
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        layout.begin_drag(
            PanelKind::Colour,
            pos2(1200.0, 200.0),
            rect(1176.0, 180.0, 264.0, 340.0),
        );
        assert!(!layout.drag_should_drop(true, false));
        assert!(layout.drag_should_drop(false, false));
    }

    #[test]
    fn a_floating_panel_blocks_the_canvas_under_it() {
        let mut layout = Layout::default();
        layout.close(PanelKind::Layers);
        layout.floating.push(Floating {
            kind: PanelKind::Layers,
            rect: rect(600.0, 300.0, 260.0, 300.0),
        });
        assert!(layout.blocks_canvas(pos2(700.0, 400.0)));
        assert!(!layout.blocks_canvas(pos2(400.0, 400.0)));
    }
}
