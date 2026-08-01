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
//! ## Why a drag lifts the panel out of the layout
//!
//! [`Layout::begin_drag`] removes the panel from wherever it was. The stack it
//! left reflows immediately, so the insertion index computed against the
//! remaining slots is exactly the index the drop will use — there is no
//! "does this index count the panel I am holding?" case to get wrong, and the
//! drop indicator cannot disagree with the result.
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
    pub const SIDEBAR_MIN_WIDTH: f32 = 190.0;
    pub const SIDEBAR_MAX_WIDTH: f32 = 460.0;
    pub const FLOAT_MIN_WIDTH: f32 = 190.0;
    pub const FLOAT_MIN_HEIGHT: f32 = 130.0;
    pub const FLOAT_MAX_WIDTH: f32 = 720.0;
    pub const FLOAT_MAX_HEIGHT: f32 = 900.0;
    /// How far in from the canvas edge counts as "drop into this sidebar" when
    /// that sidebar is currently empty and so has no rect of its own.
    pub const EMPTY_ZONE_WIDTH: f32 = 104.0;
    /// How much of a floating panel must stay inside the workspace. Its header
    /// is the only way to move it, so the header must never be unreachable.
    pub const FLOAT_KEEP_VISIBLE: f32 = 96.0;
}

/// A module that can be docked, floated or closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Colour,
    Brushes,
    Layers,
    History,
}

impl PanelKind {
    pub const ALL: [PanelKind; 4] = [Self::Colour, Self::Brushes, Self::Layers, Self::History];

    /// The arrangement Umber ships with: the design's three modules, in the
    /// right-hand sidebar.
    ///
    /// History is deliberately *not* among them, and that is the same answer a
    /// layout file written before History existed gets — an absent panel is a
    /// closed one, so an old config opens with it closed too. Putting it in the
    /// default instead would have made a fresh install and an upgraded one
    /// disagree about what the workspace contains, which is exactly the silent
    /// divergence the config's version header exists to prevent; and the
    /// alternative to that, bumping the version, would throw away every
    /// arrangement anybody has made to add one module they can reach from the
    /// Window menu in two clicks.
    pub const DEFAULT_DOCK: [PanelKind; 3] = [Self::Colour, Self::Brushes, Self::Layers];

    pub fn title(self) -> &'static str {
        match self {
            Self::Colour => "Colour",
            Self::Brushes => "Brushes",
            Self::Layers => "Layers",
            Self::History => "History",
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
            Self::Colour => {
                "Choose the painting colour — a hue ring, a saturation square \
                 or RGB sliders."
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
        }
    }

    /// Stable name for the config file. Deliberately not derived from `title`,
    /// so retitling a panel in the UI cannot silently invalidate saved layouts.
    fn key(self) -> &'static str {
        match self {
            Self::Colour => "colour",
            Self::Brushes => "brushes",
            Self::Layers => "layers",
            Self::History => "history",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    /// Share of the sidebar's flexible space in the default layout. The colour
    /// picker is the tallest thing in the dock, so it gets the most.
    fn default_weight(self) -> f32 {
        match self {
            Self::Colour => 3.0,
            Self::Brushes => 1.3,
            Self::Layers => 2.2,
            Self::History => 2.0,
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

/// A panel in a sidebar stack. `weight` is its share of the space left over
/// once every panel in the stack has its minimum height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Docked {
    pub kind: PanelKind,
    pub weight: f32,
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
        index: usize,
        weight: f32,
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
    Dock { side: Side, index: usize },
    Float,
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
    pub rail: Rect,
    pub sidebar: [Option<Rect>; 2],
    pub slots: [Vec<Rect>; 2],
    /// What is left for the document. Floating panels do **not** come out of
    /// this — they hover, so the canvas region and therefore the camera pivot
    /// are unaffected by them.
    pub canvas: Rect,
}

impl Geometry {
    /// The region that counts as "drop into this sidebar".
    ///
    /// An occupied sidebar is its own rect. An empty one has no rect, so a
    /// strip at that edge of the canvas stands in — otherwise a sidebar you
    /// emptied could never be filled again.
    pub fn drop_zone(&self, side: Side) -> Rect {
        if let Some(rect) = self.sidebar[side.index()] {
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
    pub fn drop_target(&self, pointer: Pos2) -> DropTarget {
        for side in Side::ALL {
            let zone = self.drop_zone(side);
            if zone.contains(pointer) {
                return DropTarget::Dock {
                    side,
                    index: insert_index(&self.slots[side.index()], pointer.y),
                };
            }
        }
        DropTarget::Float
    }

    /// The line a dock drop would insert at, for the drop indicator.
    pub fn insertion_line(&self, side: Side, index: usize) -> (Pos2, Pos2) {
        let zone = self.drop_zone(side);
        let slots = &self.slots[side.index()];
        let y = match slots.get(index) {
            Some(slot) => slot.top(),
            None => slots.last().map_or(zone.top(), |slot| slot.bottom()),
        };
        (pos2(zone.left(), y), pos2(zone.right(), y))
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
    sides: [Vec<Docked>; 2],
    /// Draw order, and therefore z-order: the last one is on top.
    floating: Vec<Floating>,
    widths: [f32; 2],
    rail_side: Side,
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
        Self {
            sides: [
                Vec::new(),
                PanelKind::DEFAULT_DOCK
                    .into_iter()
                    .map(|kind| Docked {
                        kind,
                        weight: kind.default_weight(),
                    })
                    .collect(),
            ],
            floating: Vec::new(),
            widths: [metrics::PANEL, metrics::PANEL],
            rail_side: Side::Left,
            drag: None,
            edit_mode: false,
            dirty: false,
        }
    }
}

impl Layout {
    // --- queries -----------------------------------------------------------

    pub fn docked(&self, side: Side) -> &[Docked] {
        &self.sides[side.index()]
    }

    pub fn floating(&self) -> &[Floating] {
        &self.floating
    }

    pub fn width(&self, side: Side) -> f32 {
        self.widths[side.index()]
    }

    pub fn rail_side(&self) -> Side {
        self.rail_side
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
            if let Some(index) = self.sides[side.index()].iter().position(|d| d.kind == kind) {
                return Some(Origin::Dock {
                    side,
                    index,
                    weight: self.sides[side.index()][index].weight,
                });
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
    pub fn geometry(&self, workspace: Rect, rail_width: f32) -> Geometry {
        // The strips above and below claim their height first, so on a window
        // shorter than the sum of them what arrives here is already inverted.
        let workspace = Rect::from_min_size(workspace.min, workspace.size().max(Vec2::ZERO));
        // A window narrower than the rail itself. The rail wins — it is the one
        // piece of chrome with no way to hide it — and the canvas gets nothing.
        let rail_width = rail_width.clamp(0.0, workspace.width());
        let mut canvas = workspace;
        let rail = match self.rail_side {
            Side::Left => {
                let r = Rect::from_min_max(
                    canvas.left_top(),
                    pos2(canvas.left() + rail_width, canvas.bottom()),
                );
                canvas.min.x = r.right();
                r
            }
            Side::Right => {
                let r = Rect::from_min_max(
                    pos2(canvas.right() - rail_width, canvas.top()),
                    canvas.right_bottom(),
                );
                canvas.max.x = r.left();
                r
            }
        };

        let mut sidebar = [None, None];
        let mut slots = [Vec::new(), Vec::new()];

        for side in Side::ALL {
            let stack = &self.sides[side.index()];
            if stack.is_empty() {
                continue;
            }
            // Never let the two sidebars eat the canvas entirely. `canvas` is
            // already non-negative, so this cannot come out below zero and the
            // sidebar cannot claim more than there is.
            let width = self.widths[side.index()]
                .min((canvas.width() - limits::SIDEBAR_MIN_WIDTH).max(0.0))
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
            sidebar[side.index()] = Some(rect);

            let weights: Vec<f32> = stack.iter().map(|d| d.weight).collect();
            let mut y = rect.top();
            for height in stack_heights(&weights, rect.height()) {
                slots[side.index()].push(Rect::from_min_size(
                    pos2(rect.left(), y),
                    vec2(rect.width(), height),
                ));
                y += height;
            }
        }

        Geometry {
            workspace,
            rail,
            sidebar,
            slots,
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

    pub fn set_width(&mut self, side: Side, width: f32) {
        let width = width
            .clamp(limits::SIDEBAR_MIN_WIDTH, limits::SIDEBAR_MAX_WIDTH)
            .round();
        if self.widths[side.index()] != width {
            self.widths[side.index()] = width;
            self.dirty = true;
        }
    }

    /// Move the boundary between panels `index` and `index + 1` by `delta`
    /// points, given the heights they currently have.
    pub fn resize_split(&mut self, side: Side, index: usize, delta: f32, heights: &[f32]) {
        let stack = &mut self.sides[side.index()];
        if index + 1 >= stack.len() || heights.len() != stack.len() {
            return;
        }
        let mut next: Vec<f32> = heights.to_vec();
        // Clamp the movement rather than the results, so the pair always keeps
        // the same total: clamping afterwards would let the stack grow.
        let min = limits::PANEL_MIN_HEIGHT;
        let delta = delta.max(min - next[index]).min(next[index + 1] - min);
        next[index] += delta;
        next[index + 1] -= delta;
        self.set_weights_from_heights(side, &next);
    }

    fn set_weights_from_heights(&mut self, side: Side, heights: &[f32]) {
        let stack = &mut self.sides[side.index()];
        if heights.len() != stack.len() {
            return;
        }
        let flexible: Vec<f32> = heights
            .iter()
            .map(|h| (h - limits::PANEL_MIN_HEIGHT).max(0.0))
            .collect();
        if flexible.iter().sum::<f32>() <= 1e-6 {
            for d in stack.iter_mut() {
                d.weight = 1.0;
            }
        } else {
            for (d, w) in stack.iter_mut().zip(flexible) {
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

    pub fn set_rail_side(&mut self, side: Side) {
        if self.rail_side == side {
            return;
        }
        self.rail_side = side;
        self.dirty = true;
    }

    pub fn close(&mut self, kind: PanelKind) {
        self.take(kind);
        self.dirty = true;
    }

    /// Put a closed panel back, at the bottom of whichever sidebar has room.
    pub fn open(&mut self, kind: PanelKind) {
        if self.is_open(kind) {
            return;
        }
        // Prefer the side that already has panels, so reopening does not
        // conjure a second sidebar the user never asked for.
        let side = if self.sides[Side::Right.index()].is_empty()
            && !self.sides[Side::Left.index()].is_empty()
        {
            Side::Left
        } else {
            Side::Right
        };
        self.sides[side.index()].push(Docked {
            kind,
            weight: kind.default_weight(),
        });
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

    /// Remove a panel from wherever it is, reporting where that was.
    fn take(&mut self, kind: PanelKind) -> Option<Origin> {
        for side in Side::ALL {
            if let Some(index) = self.sides[side.index()].iter().position(|d| d.kind == kind) {
                let removed = self.sides[side.index()].remove(index);
                return Some(Origin::Dock {
                    side,
                    index,
                    weight: removed.weight,
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
                index,
                weight,
            } => {
                let stack = &mut self.sides[side.index()];
                let index = index.min(stack.len());
                stack.insert(index, Docked { kind, weight });
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
        match target {
            DropTarget::Dock { side, index } => {
                let weight = match drag.origin {
                    Origin::Dock { weight, .. } => weight,
                    Origin::Float(_) | Origin::Closed => drag.kind.default_weight(),
                };
                let stack = &mut self.sides[side.index()];
                let index = index.min(stack.len());
                stack.insert(
                    index,
                    Docked {
                        kind: drag.kind,
                        weight,
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
        self.dirty = true;
    }

    /// Abandon the drag and put the panel back where it came from.
    pub fn cancel_drag(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        self.put(drag.kind, drag.origin);
        self.dirty = true;
    }

    // --- persistence -------------------------------------------------------

    /// Serialise to the config file's text form.
    ///
    /// Hand-rolled rather than serde: the whole format is six line shapes, and
    /// a parser that can be read in one screen is worth more here than a
    /// dependency plus derive macros on types that also carry drag state.
    pub fn to_config(&self) -> String {
        let mut out = String::from("umber-layout 1\n");
        out.push_str(&format!("rail {}\n", self.rail_side.key()));
        for side in Side::ALL {
            out.push_str(&format!(
                "width {} {:.0}\n",
                side.key(),
                self.widths[side.index()]
            ));
            for d in &self.sides[side.index()] {
                out.push_str(&format!(
                    "dock {} {} {:.4}\n",
                    side.key(),
                    d.kind.key(),
                    d.weight
                ));
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
    pub fn from_config(text: &str) -> Option<Self> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        if lines.next()? != "umber-layout 1" {
            return None;
        }

        let mut layout = Self {
            sides: [Vec::new(), Vec::new()],
            floating: Vec::new(),
            widths: [metrics::PANEL, metrics::PANEL],
            rail_side: Side::Left,
            drag: None,
            edit_mode: false,
            dirty: false,
        };
        let mut seen: Vec<PanelKind> = Vec::new();

        for line in lines {
            let f: Vec<&str> = line.split_whitespace().collect();
            match f.as_slice() {
                ["rail", side] => {
                    if let Some(side) = Side::from_key(side) {
                        layout.rail_side = side;
                    }
                }
                ["width", side, value] => {
                    if let (Some(side), Ok(value)) = (Side::from_key(side), value.parse::<f32>()) {
                        layout.widths[side.index()] = value
                            .clamp(limits::SIDEBAR_MIN_WIDTH, limits::SIDEBAR_MAX_WIDTH)
                            .round();
                    }
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
                    layout.sides[side.index()].push(Docked {
                        kind,
                        weight: weight.parse::<f32>().unwrap_or(1.0).clamp(0.0, 1000.0),
                    });
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
        let weights: Vec<f32> = layout
            .docked(Side::Right)
            .iter()
            .map(|d| d.weight)
            .collect();
        let heights = stack_heights(&weights, available);

        layout.resize_split(Side::Right, 0, 40.0, &heights);

        let after: Vec<f32> = layout
            .docked(Side::Right)
            .iter()
            .map(|d| d.weight)
            .collect();
        let replayed = stack_heights(&after, available);
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
        let weights: Vec<f32> = layout
            .docked(Side::Right)
            .iter()
            .map(|d| d.weight)
            .collect();
        let heights = stack_heights(&weights, available);

        // Shove the boundary far past the bottom panel's minimum.
        layout.resize_split(Side::Right, 0, 10_000.0, &heights);

        let after: Vec<f32> = layout
            .docked(Side::Right)
            .iter()
            .map(|d| d.weight)
            .collect();
        let replayed = stack_heights(&after, available);
        assert!(
            replayed
                .iter()
                .all(|h| *h >= limits::PANEL_MIN_HEIGHT - 1e-2),
            "{replayed:?}"
        );
        assert!((replayed.iter().sum::<f32>() - available).abs() < 1e-2);
    }

    #[test]
    fn sidebar_width_is_clamped() {
        let mut layout = Layout::default();
        layout.set_width(Side::Right, 10.0);
        assert_eq!(layout.width(Side::Right), limits::SIDEBAR_MIN_WIDTH);
        layout.set_width(Side::Right, 9999.0);
        assert_eq!(layout.width(Side::Right), limits::SIDEBAR_MAX_WIDTH);
    }

    #[test]
    fn geometry_leaves_the_canvas_between_the_rails() {
        let layout = Layout::default();
        let geo = layout.geometry(rect(0.0, 70.0, 1440.0, 800.0), 76.0);
        assert_eq!(geo.rail.left(), 0.0);
        assert_eq!(geo.rail.width(), 76.0);
        assert!(geo.sidebar[Side::Left.index()].is_none());
        let right = geo.sidebar[Side::Right.index()].expect("default docks on the right");
        assert_eq!(right.right(), 1440.0);
        assert_eq!(geo.canvas.left(), 76.0);
        assert_eq!(geo.canvas.right(), right.left());
        assert_eq!(geo.slots[Side::Right.index()].len(), 3);
    }

    /// Every rect the layout hands out has to survive a window dragged down to
    /// nothing. A `Rect` with its max left of its min does not panic — it draws
    /// in the wrong place, or fills the panel it was meant to sit inside — so
    /// this is the kind of bug that only shows up as "the interface looked odd
    /// for a moment while I resized".
    ///
    /// Run over the default layout *and* over one holding every module there
    /// is, since the stack's minimum height is per panel: a fourth module is a
    /// fourth minimum to fit into the same sliver of window.
    #[test]
    fn a_window_too_small_for_the_chrome_still_produces_sane_rects() {
        let mut crowded = Layout::default();
        for kind in PanelKind::ALL {
            crowded.open(kind);
        }
        for layout in [Layout::default(), crowded] {
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
            let geo = layout.geometry(rect(0.0, 70.0, w, h), 76.0);
            let mut all = vec![geo.workspace, geo.rail, geo.canvas];
            all.extend(geo.sidebar.iter().flatten().copied());
            all.extend(geo.slots.iter().flatten().copied());
            all.push(geo.drop_zone(Side::Left));
            all.push(geo.drop_zone(Side::Right));
            for r in all {
                assert!(
                    r.width() >= 0.0 && r.height() >= 0.0,
                    "{w}×{h} produced {r:?}",
                );
            }
            // And a drop still resolves to something rather than panicking on
            // the way through the empty slot list.
            let _ = geo.drop_target(pos2(0.0, 0.0));
            let _ = geo.insertion_line(Side::Right, 0);
        }
    }

    #[test]
    fn floating_panels_do_not_shrink_the_canvas() {
        // The camera pivot comes from the canvas region, so a panel that hovers
        // must not change it. This is the invariant that keeps a dab under the
        // cursor.
        let mut layout = Layout::default();
        let workspace = rect(0.0, 70.0, 1440.0, 800.0);
        let before = layout.geometry(workspace, 76.0).canvas;

        layout.close(PanelKind::Layers);
        layout.floating.push(Floating {
            kind: PanelKind::Layers,
            rect: rect(600.0, 300.0, 260.0, 300.0),
        });
        let after = layout.geometry(workspace, 76.0).canvas;

        assert_eq!(before, after);
    }

    #[test]
    fn dropping_over_a_sidebar_picks_the_slot_under_the_pointer() {
        let layout = Layout::default();
        let geo = layout.geometry(rect(0.0, 0.0, 1440.0, 900.0), 76.0);
        let slots = &geo.slots[Side::Right.index()];

        // Top half of the first slot inserts above it.
        let p = pos2(slots[0].center().x, slots[0].top() + 4.0);
        assert_eq!(
            geo.drop_target(p),
            DropTarget::Dock {
                side: Side::Right,
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
                index: slots.len()
            }
        );
    }

    #[test]
    fn an_empty_sidebar_still_accepts_a_drop() {
        let layout = Layout::default();
        let geo = layout.geometry(rect(0.0, 0.0, 1440.0, 900.0), 76.0);
        let zone = geo.drop_zone(Side::Left);
        assert_eq!(
            geo.drop_target(zone.center()),
            DropTarget::Dock {
                side: Side::Left,
                index: 0
            }
        );
    }

    #[test]
    fn the_middle_of_the_canvas_floats() {
        let layout = Layout::default();
        let geo = layout.geometry(rect(0.0, 0.0, 1440.0, 900.0), 76.0);
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
        assert_eq!(layout.docked(Side::Right).len(), 2);

        layout.cancel_drag();
        assert!(!layout.is_dragging());
        assert_eq!(layout.to_config(), before);
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
        layout.end_drag(DropTarget::Dock {
            side: Side::Left,
            index: 0,
        });
        assert_eq!(layout.docked(Side::Left).len(), 1);
        assert_eq!(layout.docked(Side::Left)[0].kind, PanelKind::Layers);
        assert_eq!(layout.docked(Side::Right).len(), 2);
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
        assert_eq!(layout.docked(Side::Right).len(), 2);

        let rect = layout.floating()[0].rect;
        layout.begin_drag(PanelKind::Colour, rect.center(), rect);
        layout.end_drag(DropTarget::Dock {
            side: Side::Right,
            index: 0,
        });
        assert!(layout.floating().is_empty());
        assert_eq!(layout.docked(Side::Right)[0].kind, PanelKind::Colour);
    }

    #[test]
    fn closing_and_reopening_keeps_one_of_each() {
        let mut layout = Layout::default();
        layout.close(PanelKind::Brushes);
        assert!(!layout.is_open(PanelKind::Brushes));
        layout.open(PanelKind::Brushes);
        layout.open(PanelKind::Brushes);
        let count = Side::ALL
            .iter()
            .map(|s| {
                layout
                    .docked(*s)
                    .iter()
                    .filter(|d| d.kind == PanelKind::Brushes)
                    .count()
            })
            .sum::<usize>();
        assert_eq!(count, 1);
    }

    #[test]
    fn config_round_trips() {
        let mut layout = Layout::default();
        layout.set_edit_mode(true);
        layout.set_rail_side(Side::Right);
        layout.set_width(Side::Left, 300.0);
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
        layout.end_drag(DropTarget::Dock {
            side: Side::Left,
            index: 0,
        });

        let text = layout.to_config();
        let back = Layout::from_config(&text).expect("valid config");
        assert_eq!(back.to_config(), text);
        assert_eq!(back.rail_side(), layout.rail_side());
        assert_eq!(back.width(Side::Left), layout.width(Side::Left));
        assert_eq!(back.docked(Side::Left), layout.docked(Side::Left));
        assert_eq!(back.floating(), layout.floating());
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
                    width right not-a-number\n\
                    dock right colour 3\n\
                    dock right colour 9\n\
                    dock left brushes 1\n\
                    nonsense here\n";
        let layout = Layout::from_config(text).expect("still loads");
        assert_eq!(layout.rail_side(), Side::Left, "bad side falls back");
        assert_eq!(layout.width(Side::Right), metrics::PANEL);
        assert_eq!(
            layout.docked(Side::Right).len(),
            1,
            "the duplicate is dropped"
        );
        assert_eq!(layout.docked(Side::Left).len(), 1);
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
        layout.end_drag(DropTarget::Dock {
            side: Side::Left,
            index: 0,
        });

        let text = layout.to_config();
        assert!(text.contains("history"), "{text}");
        let back = Layout::from_config(&text).expect("valid config");
        assert_eq!(back.to_config(), text);
        assert_eq!(back.docked(Side::Left)[0].kind, PanelKind::History);
    }

    /// A layout file written before the History module existed names three
    /// panels and knows nothing of a fourth. It must load exactly as it did —
    /// with History closed, which is where the shipped arrangement puts it too,
    /// so an upgraded workspace and a fresh one agree. That is the reason the
    /// version header did not have to move; see `PanelKind::DEFAULT_DOCK`.
    #[test]
    fn a_config_written_before_the_history_module_still_loads() {
        let text = "umber-layout 1\n\
                    rail left\n\
                    width left 264\n\
                    width right 300\n\
                    dock right colour 3\n\
                    dock right brushes 1.3\n\
                    dock right layers 2.2\n";
        let layout = Layout::from_config(text).expect("still loads");
        assert_eq!(layout.docked(Side::Right).len(), 3);
        assert_eq!(layout.width(Side::Right), 300.0);
        assert!(
            !layout.is_open(PanelKind::History),
            "an absent panel is a closed one",
        );
        // And it is reachable, so nothing has been lost by not being named.
        let mut layout = layout;
        layout.open(PanelKind::History);
        assert!(layout.is_open(PanelKind::History));
    }

    /// The edit-mode remove control. Whatever else it does, a removed module
    /// must be able to come back — which is what the module library is for.
    #[test]
    fn a_removed_module_can_be_added_back() {
        let mut layout = Layout::default();
        layout.close(PanelKind::Layers);
        assert!(!layout.is_open(PanelKind::Layers));
        assert_eq!(layout.docked(Side::Right).len(), 2);

        layout.add_dragging(PanelKind::Layers, pos2(600.0, 400.0));
        layout.end_drag(DropTarget::Dock {
            side: Side::Right,
            index: 2,
        });
        assert!(layout.is_open(PanelKind::Layers));
        assert_eq!(layout.docked(Side::Right).len(), 3);
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
