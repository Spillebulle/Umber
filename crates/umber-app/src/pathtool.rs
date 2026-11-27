//! The Path tool's state: drawing a path point by point, and reshaping the
//! paths already on the layer.
//!
//! The model only. Nothing here reads a pointer event type or paints, which is
//! what lets every rule below be tested without a window, the same division
//! [`crate::layerdrag`] keeps against [`crate::panels`]. Positions arrive in
//! **document** space, and the two distances a press needs (`Reach`) arrive
//! already converted from screen pixels by the caller, because only the camera
//! knows the zoom.
//!
//! What is decided here:
//!
//! - **The draft is not on the layer.** Nodes accumulate in the tool until a
//!   `Commit::Land` is handed back, and the draft **stays up until the caller
//!   says how the landing went** (`landed`). A refusal (a full stack, too many
//!   nodes) therefore costs the artist nothing: the draft is still there and
//!   `said` says why.
//! - **Continuing a path is the same draft with a head start.** A press on an
//!   end node of an open path takes its nodes up, reversed when it was the
//!   first one that was pressed, so the new nodes always go on at the end. The
//!   path on the layer is not touched until the landing replaces it, which is
//!   what makes Escape free.
//! - **A press is a held thing.** `Held` says what, so a release knows whether
//!   it ends a drag of handles, a closing press, a continuation or an edit.
//! - **An edit drag is absolute and works on a copy.** The path is cloned at
//!   the press and every move recomputes from that clone and the pointer, never
//!   from the last move, so a drag that returns to where it began is exactly
//!   the path it began as and commits nothing. The layer's record is not
//!   written until the release, which is what makes one gesture one `Set`.
//! - **Smooth is decided at the press.** `pathedit::with_handle` is handed the
//!   node as it was then, so a handle dragged through zero cannot change which
//!   rule applies half way.

use glam::{UVec2, Vec2};
use umber_core::FlipAxis;
use umber_core::pathedit::{self, Side};
use umber_core::vector::{Node, Path, PathRole, VectorObject, VectorRefusal};

/// Which half of the tool is in hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathMode {
    Draw,
    Edit,
}

impl PathMode {
    pub const ALL: [PathMode; 2] = [PathMode::Draw, PathMode::Edit];

    pub fn label(self) -> &'static str {
        match self {
            PathMode::Draw => "Draw",
            PathMode::Edit => "Edit",
        }
    }
}

/// How many of the picked path's nodes may be marked and hit at once. Past
/// it, inside the view, no node is marked and no node or handle is hit; the
/// line can still be picked up and moved. A path traced from a lasso can hold
/// tens of thousands of nodes, and a mark per node per frame is the cost the
/// standing rule forbids. `Editor::path_nodes_shown` is the one reading.
pub const NODE_MARK_CAP: usize = 2048;

/// Document-space distances for this press, from the camera.
#[derive(Clone, Copy, Debug)]
pub struct Reach {
    /// How close a press must be to count as on a node.
    pub hit: f32,
    /// How far a press may travel and still be a tap rather than a drag.
    pub tap: f32,
}

/// What the tool may touch on this event.
#[derive(Clone, Copy)]
pub struct Ground<'a> {
    /// The active layer's record, only where `Editor::editable_paths` gave one.
    pub paths: Option<&'a VectorObject>,
    pub reach: Reach,
    /// False when more than the cap of the picked path's nodes are in view.
    pub nodes_shown: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Commit {
    Nothing,
    /// Finished paths to land. The draft stays up until `landed` is called.
    Land {
        paths: Vec<Path>,
        replaces: Option<usize>,
    },
    /// The active layer's whole record after an edit.
    Set(VectorObject),
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub nodes: Vec<Node>,
    pub closed: bool,
    pub continues: Option<usize>,
}

/// What is picked on the layer: a path, and perhaps one node of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pick {
    pub path: usize,
    pub node: Option<usize>,
}

/// What a held press is doing.
#[derive(Clone, Debug)]
enum Held {
    Nothing,
    /// A node was placed at `press`; moving pulls its handles out.
    Placing {
        press: Vec2,
    },
    /// The press landed on the first node and closed the draft.
    Closing,
    /// The press took an existing path up. It added no node and has no drag.
    Continuing,
    /// The press picked another path. Nothing follows the pointer.
    Picked,
    /// Dragging a handle of `start`'s node.
    Handle {
        path: usize,
        node: usize,
        side: Side,
        start: Path,
    },
    /// Dragging a node, from where the press was.
    Node {
        path: usize,
        node: usize,
        press: Vec2,
        start: Path,
    },
    /// On the picked path's line: a tap splits it, a drag carries all of it.
    Segment {
        path: usize,
        segment: usize,
        t: f32,
        press: Vec2,
        tap: f32,
        start: Path,
    },
}

pub struct PathTool {
    pub mode: PathMode,
    pub role: PathRole,
    draft: Option<Draft>,
    /// The continued path's role, where there is one. Otherwise the tool's
    /// role is read when the path lands, so changing the picker mid-draft
    /// changes what lands, as the picker beside the canvas suggests.
    draft_role: Option<PathRole>,
    /// How many nodes the continued path had, so a pop cannot eat into it.
    base_len: usize,
    /// The continued path was taken up from its first node, so the draft's
    /// base is the path reversed.
    base_reversed: bool,
    held: Held,
    hover: Option<Vec2>,
    said: Option<&'static str>,
    pick: Option<Pick>,
    /// The pick before the last `Set` was asked for, which a refusal restores.
    before: Option<Option<Pick>>,
    /// The path being dragged as it would be if released now.
    dragged: Option<Path>,
    /// Shift on the last move, so the release shapes the path as the drag did.
    alone: bool,
    /// Moves whenever something the painter draws may have changed. See
    /// [`PathTool::generation`].
    generation: u64,
    /// The Smoothing rail's reading for the picked path. See
    /// [`PathTool::preview_smoothing`].
    resmooth: Option<Resmooth>,
}

/// What the Smoothing rail is doing to one path.
///
/// **The refit is always from `source`**, the path as it stood when the rail
/// was first moved on it, never from the last refit. Smoothing only takes
/// detail away, so refitting a refit would ratchet: each drag would start from
/// less than the last, and turning the figure back down could not give back
/// what turning it up took. Kept across the rail's own commits for that reason,
/// and dropped by every other edit, by a foreign change to the record and by
/// letting go of the pick, after which the path as it then stands is the new
/// source and the rail reads zero.
#[derive(Clone, Debug)]
struct Resmooth {
    path: usize,
    source: Path,
    amount: f32,
    /// A preview is up that the release has not committed yet.
    pending: bool,
}

/// Said when the refit would leave less than a path.
const TOO_SMALL_TO_SMOOTH: &str = "This path is too small to smooth.";

impl Default for PathTool {
    fn default() -> Self {
        Self {
            mode: PathMode::Draw,
            role: PathRole::Cut,
            draft: None,
            draft_role: None,
            base_len: 0,
            base_reversed: false,
            held: Held::Nothing,
            hover: None,
            said: None,
            pick: None,
            before: None,
            dragged: None,
            alone: false,
            generation: 0,
            resmooth: None,
        }
    }
}

/// What is said of a figure past the bound. The record's own sentence is about
/// turning a shape into a path, which is not what a drag did.
const OUT_OF_BOUNDS: &str = "That would put the path too far from the canvas.";

fn refusal_sentence(why: VectorRefusal) -> &'static str {
    match why {
        VectorRefusal::Impossible => OUT_OF_BOUNDS,
        VectorRefusal::TooManyNodes | VectorRefusal::TooManyPaths | VectorRefusal::Opaque => {
            why.sentence()
        }
    }
}

/// Paths the tool may pick and change. Ink describes pixels, not a line.
fn is_line(path: &Path) -> bool {
    match path.role {
        PathRole::Cut | PathRole::Plot => path.ink.is_none(),
        PathRole::Ink => false,
    }
}

impl PathTool {
    pub fn draft(&self) -> Option<&Draft> {
        self.draft.as_ref()
    }

    pub fn hover(&self) -> Option<Vec2> {
        self.hover
    }

    /// Put a sentence on the strip, for a refusal the editor found before the
    /// tool was asked.
    pub fn say(&mut self, sentence: &'static str) {
        self.said = Some(sentence);
    }

    /// The sentence to show until the next press, if any.
    pub fn said(&self) -> Option<&'static str> {
        self.said
    }

    pub fn pick(&self) -> Option<Pick> {
        self.pick
    }

    /// The path being dragged, as it would be if released now.
    pub fn dragged(&self) -> Option<&Path> {
        self.dragged.as_ref()
    }

    /// A number that moves whenever what the painter draws of the tool may
    /// have changed: the draft, the drag, the hover, the pick, or whether a
    /// press is held (which hides the rubber band).
    ///
    /// **Bumped inside the methods that change those, never beside their call
    /// sites**, so the painter can skip a frame on an equal number without a
    /// list of call sites to keep in step. Every `&mut self` method bumps it
    /// except [`PathTool::say`], whose sentence is on the strip and not the
    /// canvas. A bump that changed nothing costs one rebuild on a frame that
    /// already had an event; a missing one would leave a stale picture, so the
    /// rule errs the cheap way. `mode` and `role` are plain fields and are not
    /// covered: the painter reads `role` into its own key, and nothing it draws
    /// reads `mode`.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn is_held(&self) -> bool {
        match self.held {
            Held::Nothing => false,
            Held::Placing { .. }
            | Held::Closing
            | Held::Continuing
            | Held::Picked
            | Held::Handle { .. }
            | Held::Node { .. }
            | Held::Segment { .. } => true,
        }
    }

    /// Drop the draft, the pick and any press, keeping the mode and the role,
    /// which are tool settings. For the document going away under the tool,
    /// where there is nothing left to land a path on.
    pub fn reset(&mut self) {
        // The generation carries on rather than starting again at zero, or a
        // reset could land back on a number a painter has already cached.
        *self = Self {
            mode: self.mode,
            role: self.role,
            generation: self.generation.wrapping_add(1),
            ..Self::default()
        };
    }

    /// The active layer changed under a draft that continues one of its
    /// paths. The draft now lands as a new path and the old one is untouched.
    ///
    /// **A continuation given nothing yet is dropped instead.** It holds only
    /// the path's own nodes as they were when it was taken up, so landing it
    /// as a new path would put down a copy of a path nobody drew, beside the
    /// edit that made the detach happen.
    pub fn detach(&mut self) {
        self.touch();
        if self.untouched_continuation() {
            self.draft = None;
            self.draft_role = None;
            self.base_len = 0;
            self.base_reversed = false;
            return;
        }
        if let Some(draft) = self.draft.as_mut() {
            draft.continues = None;
            self.base_len = 0;
            self.base_reversed = false;
        }
    }

    /// Whether the path a continued draft names in `record` is still the
    /// path it took up: an open line of the same role whose nodes are the
    /// draft's base, reversed where it was taken from the first node. True
    /// where the draft continues nothing, since there is nothing to detach.
    ///
    /// **Content, not count.** An undo, a redo or a History jump can leave the
    /// count where it was while the index names another path, and a landing
    /// that trusted the index would replace a path nobody touched. Asked only
    /// when the record has moved, so the comparison is not on the frame path.
    pub fn still_continues(&self, record: Option<&VectorObject>) -> bool {
        let Some(draft) = self.draft.as_ref() else {
            return true;
        };
        let Some(index) = draft.continues else {
            return true;
        };
        let Some(path) = record.and_then(|r| r.paths().get(index)) else {
            return false;
        };
        if path.closed
            || !is_line(path)
            || Some(path.role) != self.draft_role
            || path.nodes.len() != self.base_len
            || draft.nodes.len() < self.base_len
        {
            return false;
        }
        let base = &draft.nodes[..self.base_len];
        if self.base_reversed {
            // `pathedit::reversed`, compared node by node without the copy.
            path.nodes.iter().rev().zip(base).all(|(on, up)| {
                let turned = Node {
                    handle_in: on.handle_out,
                    handle_out: on.handle_in,
                    ..*on
                };
                turned == *up
            })
        } else {
            path.nodes == base
        }
    }

    /// Whatever press is held ends without committing. The tool's own edits
    /// call this first, or the release that follows would commit a drag that
    /// began against a record that is no longer there.
    fn cancel_hold(&mut self) {
        self.held = Held::Nothing;
        self.dragged = None;
    }

    /// The press ended without a release: another gesture took the pointer
    /// over (a middle-button pan, a second finger) or the window lost focus.
    ///
    /// Nothing is committed, and a node the press was *placing* is taken back
    /// off, since the release that would have settled it as a corner or a
    /// curve is never coming, and a node pulled half into a curve by a gesture
    /// that was then taken away is not something anybody drew. It ends the
    /// draft where it was the only node. A closing press reopens the draft,
    /// since closing happens on the release. A drag is dropped and the record
    /// is untouched, which is what an unreleased drag always was.
    pub fn interrupt(&mut self) {
        self.touch();
        let held = std::mem::replace(&mut self.held, Held::Nothing);
        self.dragged = None;
        match held {
            Held::Placing { .. } => {
                if let Some(draft) = self.draft.as_mut() {
                    draft.nodes.pop();
                    if draft.nodes.is_empty() {
                        self.draft = None;
                        self.draft_role = None;
                        self.base_len = 0;
                    }
                }
            }
            Held::Closing => {
                if let Some(draft) = self.draft.as_mut() {
                    draft.closed = false;
                }
            }
            Held::Nothing
            | Held::Continuing
            | Held::Picked
            | Held::Handle { .. }
            | Held::Node { .. }
            | Held::Segment { .. } => {}
        }
    }

    /// The canvas was mirrored. A draft is drawn over the artwork, so it is
    /// mirrored with it, by the arithmetic the paths on the layers use. A held
    /// press is ended first, because its press point is in the old orientation.
    pub fn mirror(&mut self, axis: FlipAxis, canvas: UVec2) {
        self.touch();
        self.interrupt();
        if let Some(draft) = self.draft.as_mut() {
            for node in &mut draft.nodes {
                *node = node.flipped(axis, canvas);
            }
        }
        self.hover = None;
    }

    /// Drop the pick (active layer changed, foreign revision, tab switch).
    pub fn let_go(&mut self) {
        self.touch();
        self.pick = None;
        self.before = None;
        self.dragged = None;
        self.resmooth = None;
        match self.held {
            Held::Nothing | Held::Placing { .. } | Held::Closing | Held::Continuing => {}
            Held::Picked | Held::Handle { .. } | Held::Node { .. } | Held::Segment { .. } => {
                self.held = Held::Nothing;
            }
        }
    }

    /// A press. `edit` is `mode == Edit || ctrl`.
    pub fn press(&mut self, at: Vec2, edit: bool, ground: Ground<'_>) {
        self.touch();
        // A press while one is held replaces it, leaving no stale drag behind.
        self.cancel_hold();
        if edit {
            self.press_edit(at, ground);
            return;
        }
        self.said = None;
        // A plain press is a drawing press, and drawing lets go of the pick.
        self.pick = None;
        self.before = None;
        self.dragged = None;
        let hit = ground.reach.hit;

        let Some(draft) = self.draft.as_mut() else {
            if let Some((draft, role, reversed)) = continuation(ground, at, hit) {
                self.base_len = draft.nodes.len();
                self.base_reversed = reversed;
                self.draft_role = Some(role);
                self.draft = Some(draft);
                self.held = Held::Continuing;
                return;
            }
            self.base_len = 0;
            self.draft_role = None;
            self.draft = Some(Draft {
                nodes: vec![Node::corner(at)],
                closed: false,
                continues: None,
            });
            self.held = Held::Placing { press: at };
            return;
        };

        // Three nodes make a polygon; with fewer the first node is just a node
        // that happens to be near.
        if draft.nodes.len() >= 3 && draft.nodes[0].at.distance(at) <= hit {
            draft.closed = true;
            self.held = Held::Closing;
            return;
        }
        // A node after a refused close means the artist went on drawing.
        draft.closed = false;
        draft.nodes.push(Node::corner(at));
        self.held = Held::Placing { press: at };
    }

    /// An edit press, in the spec's order, first match winning.
    fn press_edit(&mut self, at: Vec2, ground: Ground<'_>) {
        let Some(record) = ground.paths else {
            // Not an editable layer: the press is nobody's.
            return;
        };
        self.said = None;
        self.before = None;
        self.dragged = None;
        self.sanitize(ground);
        let reach = ground.reach;

        if let Some(pick) = self.pick {
            let path = &record.paths()[pick.path];
            if ground.nodes_shown {
                if let Some(n) = pick.node
                    && let Some(side) = pathedit::handle_at(&path.nodes[n], at, reach.hit)
                {
                    self.held = Held::Handle {
                        path: pick.path,
                        node: n,
                        side,
                        start: path.clone(),
                    };
                    return;
                }
                if let Some(n) = pathedit::node_at(path, at, reach.hit) {
                    self.pick = Some(Pick {
                        path: pick.path,
                        node: Some(n),
                    });
                    self.held = Held::Node {
                        path: pick.path,
                        node: n,
                        press: at,
                        start: path.clone(),
                    };
                    return;
                }
            }
            if let Some(hit) = pathedit::segment_at(path, at, reach.hit) {
                self.held = Held::Segment {
                    path: pick.path,
                    segment: hit.segment,
                    t: hit.t,
                    press: at,
                    tap: reach.tap,
                    start: path.clone(),
                };
                return;
            }
        }

        // The nearest line of any other path in reach.
        let mut best: Option<(usize, f32)> = None;
        for (i, path) in record.paths().iter().enumerate() {
            if !is_line(path) || self.pick.is_some_and(|p| p.path == i) {
                continue;
            }
            if let Some(hit) = pathedit::segment_at(path, at, reach.hit)
                && best.is_none_or(|(_, d)| hit.distance < d)
            {
                best = Some((i, hit.distance));
            }
        }
        if let Some((path, _)) = best {
            self.pick = Some(Pick { path, node: None });
            self.held = Held::Picked;
            return;
        }
        self.pick = None;
    }

    /// The pointer moved, held or not. `alone` is Shift.
    ///
    /// **A drag clones the path once, on its first move, and every move after
    /// writes into that copy**: a node drag or a handle drag rewrites the one
    /// node it moves, a whole-path drag rewrites every node in place. A path
    /// traced from a lasso can hold tens of thousands of nodes, and a clone per
    /// pointer event was an allocation of all of them a hundred times a
    /// second. Still absolute against the press, because each write is from
    /// the press-time copy and the pointer, never from the last move.
    pub fn moved(&mut self, at: Vec2, alone: bool) {
        self.touch();
        self.hover = Some(at);
        self.alone = alone;
        let Self {
            held,
            dragged,
            draft,
            ..
        } = self;
        match &*held {
            Held::Placing { .. } => {
                if let Some(node) = draft.as_mut().and_then(|d| d.nodes.last_mut()) {
                    let out = at - node.at;
                    node.handle_out = out;
                    node.handle_in = -out;
                }
            }
            Held::Handle { start, .. } | Held::Node { start, .. } | Held::Segment { start, .. } => {
                let out = dragged.get_or_insert_with(|| start.clone());
                shape_into(held, at, alone, out);
            }
            Held::Nothing | Held::Closing | Held::Continuing | Held::Picked => {}
        }
    }

    pub fn release(&mut self, at: Vec2, ground: Ground<'_>) -> Commit {
        self.touch();
        self.sanitize(ground);
        let held = std::mem::replace(&mut self.held, Held::Nothing);
        self.dragged = None;
        match held {
            Held::Nothing | Held::Continuing | Held::Picked => Commit::Nothing,
            Held::Placing { press } => {
                // A wobble inside the slop is a corner, not a tiny curve.
                if at.distance(press) <= ground.reach.tap
                    && let Some(node) = self.draft.as_mut().and_then(|d| d.nodes.last_mut())
                {
                    node.handle_in = Vec2::ZERO;
                    node.handle_out = Vec2::ZERO;
                }
                Commit::Nothing
            }
            Held::Closing => self.land(true),
            Held::Handle {
                path, ref start, ..
            }
            | Held::Node {
                path, ref start, ..
            } => {
                let Some(record) = ground.paths else {
                    return Commit::Nothing;
                };
                let Some(next) = shaped(&held, at, self.alone) else {
                    return Commit::Nothing;
                };
                if next == *start {
                    return Commit::Nothing;
                }
                let keep = self.pick;
                self.set(record.with_path(path, next), keep)
            }
            Held::Segment {
                path,
                segment,
                t,
                press,
                tap,
                ref start,
            } => {
                let Some(record) = ground.paths else {
                    return Commit::Nothing;
                };
                if at.distance(press) <= tap {
                    let Some((next, node)) = pathedit::split(start, segment, t) else {
                        return Commit::Nothing;
                    };
                    let pick = Some(Pick {
                        path,
                        node: Some(node),
                    });
                    return self.set(record.with_path(path, next), pick);
                }
                let Some(next) = shaped(&held, at, self.alone) else {
                    return Commit::Nothing;
                };
                if next == *start {
                    return Commit::Nothing;
                }
                let keep = self.pick;
                self.set(record.with_path(path, next), keep)
            }
        }
    }

    /// Enter.
    pub fn finish(&mut self) -> Commit {
        self.touch();
        self.cancel_hold();
        // A path taken up and given nothing is put back as it was: there is
        // nothing to land, and the draft would otherwise sit there waiting.
        if self.untouched_continuation() {
            self.draft = None;
            self.draft_role = None;
            self.base_len = 0;
            return Commit::Nothing;
        }
        // Enter ends an open path; only the closing press lands one closed.
        self.land(false)
    }

    /// Backspace, Delete or Undo with a draft up. True when it took something.
    pub fn pop(&mut self) -> bool {
        self.touch();
        self.cancel_hold();
        let Some(draft) = self.draft.as_mut() else {
            return false;
        };
        // Popping into the continued path would edit what is on the layer by
        // the back door, so it gives the draft up instead.
        if draft.continues.is_some() && draft.nodes.len() <= self.base_len {
            self.abandon();
            return true;
        }
        draft.nodes.pop();
        // A refused close leaves the draft shut; taking a node off reopens it.
        draft.closed = false;
        if draft.nodes.is_empty() {
            self.abandon();
        }
        true
    }

    /// Escape. True when it consumed the key. A draft goes first, then the
    /// node picked, then the path, one for each press of the key.
    pub fn abandon(&mut self) -> bool {
        self.touch();
        self.said = None;
        match self.held {
            Held::Nothing
            | Held::Placing { .. }
            | Held::Closing
            | Held::Continuing
            | Held::Picked => {}
            Held::Handle { .. } | Held::Node { .. } | Held::Segment { .. } => {
                // Escape in the middle of a drag cancels the drag and no more.
                self.held = Held::Nothing;
                self.dragged = None;
                return true;
            }
        }
        self.held = Held::Nothing;
        if self.draft.take().is_some() {
            return true;
        }
        if let Some(pick) = self.pick.as_mut() {
            if pick.node.take().is_some() {
                return true;
            }
            self.pick = None;
            return true;
        }
        false
    }

    /// Tool change, tab switch, close, resize, flip.
    pub fn settle(&mut self) -> Commit {
        self.touch();
        self.held = Held::Nothing;
        self.dragged = None;
        let commit = self.land(false);
        if commit == Commit::Nothing {
            self.draft = None;
        }
        commit
    }

    /// The outcome of the last `Commit::Land`. Ok clears the draft; Err keeps
    /// it up and says why.
    pub fn landed(&mut self, outcome: Result<(), &'static str>) {
        self.touch();
        match outcome {
            Ok(()) => {
                self.draft = None;
                self.said = None;
            }
            Err(why) => self.said = Some(why),
        }
    }

    /// The outcome of the last `Commit::Set`. Err says why and puts the pick
    /// back where it was before the edit asked to move it.
    pub fn applied(&mut self, outcome: Result<(), &'static str>) {
        self.touch();
        match outcome {
            Ok(()) => self.before = None,
            Err(why) => {
                self.said = Some(why);
                // The record is not what the rail thinks it installed, so
                // whatever is there now is where the next drag starts from.
                self.resmooth = None;
                // Only a `Set` that is outstanding has a pick to put back.
                if let Some(previous) = self.before.take() {
                    self.pick = previous;
                }
            }
        }
    }

    /// Delete or Backspace with no draft: the node picked, or else the path.
    pub fn remove(&mut self, ground: Ground<'_>) -> Commit {
        self.touch();
        self.cancel_hold();
        self.sanitize(ground);
        let (Some(record), Some(pick)) = (ground.paths, self.pick) else {
            return Commit::Nothing;
        };
        let path = &record.paths()[pick.path];
        if let Some(n) = pick.node
            && let Some(next) = pathedit::without_node(path, n)
        {
            let keep = Some(Pick {
                path: pick.path,
                node: None,
            });
            return self.set(record.with_path(pick.path, next), keep);
        }
        // No node, or a path too short to lose one: the path goes whole.
        self.set(record.without_path(pick.path), None)
    }

    /// The Cut / Plot switch. With a pick it changes that path alone and never
    /// the role for new paths; without one it sets the role for new paths.
    pub fn set_role(&mut self, role: PathRole, ground: Ground<'_>) -> Commit {
        self.touch();
        self.cancel_hold();
        self.sanitize(ground);
        let Some(pick) = self.pick else {
            self.role = role;
            return Commit::Nothing;
        };
        // A pick on a layer that cannot be read is still a pick: the default
        // is not what the artist pressed the switch for.
        let Some(record) = ground.paths else {
            return Commit::Nothing;
        };
        let mut next = record.paths()[pick.path].clone();
        if next.role == role {
            return Commit::Nothing;
        }
        next.role = role;
        let keep = self.pick;
        self.set(record.with_path(pick.path, next), keep)
    }

    /// The pick's role while there is a pick, otherwise the one for new paths.
    pub fn role_shown(&self, ground: Ground<'_>) -> PathRole {
        self.picked(ground).map_or(self.role, |(_, path)| path.role)
    }

    /// Close an open path or open a closed one. An open path needs three nodes
    /// to close, and with fewer nothing happens and nothing is said: the
    /// button is not the place to teach geometry.
    pub fn toggle_closed(&mut self, ground: Ground<'_>) -> Commit {
        self.touch();
        self.cancel_hold();
        self.sanitize(ground);
        let (Some(record), Some(pick)) = (ground.paths, self.pick) else {
            return Commit::Nothing;
        };
        let mut next = record.paths()[pick.path].clone();
        if !next.closed && next.nodes.len() < 3 {
            return Commit::Nothing;
        }
        next.closed = !next.closed;
        let keep = self.pick;
        self.set(record.with_path(pick.path, next), keep)
    }

    /// Both handles of the picked node to zero.
    pub fn corner(&mut self, ground: Ground<'_>) -> Commit {
        self.reshape_node(ground, |path, n| Some(pathedit::cornered(path.nodes[n])))
    }

    /// The picked node's handles along the line between its neighbours.
    pub fn smooth(&mut self, ground: Ground<'_>) -> Commit {
        self.reshape_node(ground, pathedit::smoothed)
    }

    pub fn can_smooth(&self, ground: Ground<'_>) -> bool {
        self.picked(ground)
            .zip(self.pick.and_then(|p| p.node))
            .is_some_and(|((_, path), n)| pathedit::smoothed(path, n).is_some())
    }

    /// A Smoothing preview is up and waiting for the pointer to come up.
    pub fn smoothing_pending(&self) -> bool {
        self.resmooth.as_ref().is_some_and(|r| r.pending) && self.dragged.is_some()
    }

    /// What the Smoothing rail reads for the pick: the amount last asked of
    /// this path, or zero, which is the path as it stands.
    pub fn smoothing_shown(&self) -> f32 {
        match (&self.resmooth, self.pick) {
            (Some(r), Some(pick)) if r.path == pick.path => r.amount,
            _ => 0.0,
        }
    }

    /// The Smoothing rail moved to `amount`: the picked path fitted again from
    /// its source at that tolerance, shown in place of the path as a drag is,
    /// and not written until [`PathTool::commit_smoothing`]. Zero is the
    /// source itself, so dragging back to the start puts the path back.
    ///
    /// The node pick is let go, because the refit's nodes are not the ones
    /// that were picked and a handle drawn for the old index would sit on a
    /// node that is not there.
    pub fn preview_smoothing(&mut self, amount: f32, ground: Ground<'_>) {
        self.touch();
        self.sanitize(ground);
        let Some((pick, path)) = self.picked(ground) else {
            return;
        };
        let fresh = !matches!(&self.resmooth, Some(r) if r.path == pick.path);
        if fresh {
            self.resmooth = Some(Resmooth {
                path: pick.path,
                source: path.clone(),
                amount: 0.0,
                pending: false,
            });
        }
        let Some(r) = self.resmooth.as_mut() else {
            return;
        };
        let amount = amount.max(0.0);
        let next = if amount > 0.0 {
            let floor = *umber_core::trace::SMOOTHING.start();
            match pathedit::resmoothed(&r.source, amount.max(floor)) {
                Some(next) => next,
                None => {
                    self.said = Some(TOO_SMALL_TO_SMOOTH);
                    return;
                }
            }
        } else {
            r.source.clone()
        };
        r.amount = amount;
        r.pending = true;
        self.said = None;
        self.pick = Some(Pick {
            path: pick.path,
            node: None,
        });
        self.dragged = Some(next);
    }

    /// The pointer came off the Smoothing rail, or a figure was typed into it:
    /// the preview becomes one `Set`. Nothing where no preview is waiting or
    /// where it is the path already on the layer.
    pub fn commit_smoothing(&mut self, ground: Ground<'_>) -> Commit {
        self.touch();
        let Some(mut r) = self.resmooth.take().filter(|r| r.pending) else {
            return Commit::Nothing;
        };
        r.pending = false;
        let next = self.dragged.take();
        let (Some(record), Some(next)) = (ground.paths, next) else {
            return Commit::Nothing;
        };
        if self.pick.map(|p| p.path) != Some(r.path) {
            return Commit::Nothing;
        }
        let Some(current) = record.paths().get(r.path) else {
            return Commit::Nothing;
        };
        if *current == next {
            self.resmooth = Some(r);
            return Commit::Nothing;
        }
        let path = r.path;
        let commit = self.set(
            record.with_path(path, next),
            Some(Pick { path, node: None }),
        );
        // `set` drops the rail's state for every edit but this one.
        if matches!(commit, Commit::Set(_)) {
            self.resmooth = Some(r);
        }
        commit
    }

    fn reshape_node(
        &mut self,
        ground: Ground<'_>,
        make: impl Fn(&Path, usize) -> Option<Node>,
    ) -> Commit {
        // For `corner` and `smooth`, the two public doors in.
        self.touch();
        self.cancel_hold();
        self.sanitize(ground);
        let (Some(record), Some(pick)) = (ground.paths, self.pick) else {
            return Commit::Nothing;
        };
        let Some(n) = pick.node else {
            return Commit::Nothing;
        };
        let path = &record.paths()[pick.path];
        let Some(node) = make(path, n) else {
            return Commit::Nothing;
        };
        if node == path.nodes[n] {
            return Commit::Nothing;
        }
        let mut next = path.clone();
        next.nodes[n] = node;
        let keep = self.pick;
        self.set(record.with_path(pick.path, next), keep)
    }

    /// The pick and its path, where the ground still has both.
    fn picked<'a>(&self, ground: Ground<'a>) -> Option<(Pick, &'a Path)> {
        let pick = self.pick?;
        let path = ground.paths?.paths().get(pick.path)?;
        is_line(path).then_some((pick, path))
    }

    /// Drop a pick the ground no longer has: a path off the end, a node off
    /// the end of its path, or a path that is not a line. Indexing a stale pick
    /// would be a panic on the pointer path.
    fn sanitize(&mut self, ground: Ground<'_>) {
        let Some(record) = ground.paths else {
            return;
        };
        let Some(pick) = self.pick else {
            return;
        };
        match record.paths().get(pick.path).filter(|p| is_line(p)) {
            None => self.pick = None,
            Some(path) => {
                if pick.node.is_some_and(|n| n >= path.nodes.len()) {
                    self.pick = Some(Pick { node: None, ..pick });
                }
            }
        }
    }

    /// Turn the record `with_path` produced into a commit, moving the pick as
    /// the edit wants. A refusal says why and leaves the pick alone.
    fn set(&mut self, result: Result<VectorObject, VectorRefusal>, pick: Option<Pick>) -> Commit {
        match result {
            Ok(record) => {
                self.before = Some(self.pick);
                self.pick = pick;
                // Any other edit changes the path under the rail, so its
                // source is no longer what is on the layer.
                // `commit_smoothing` puts its own back after this.
                self.resmooth = None;
                Commit::Set(record)
            }
            Err(why) => {
                self.said = Some(refusal_sentence(why));
                Commit::Nothing
            }
        }
    }

    /// A draft that continues a path and has had nothing added to it.
    ///
    /// The count is the whole test because the base cannot change inside the
    /// draft: a press only ever pushes a node, a drag pulls the handles of the
    /// node it placed, `pop` gives the draft up rather than eat into the base,
    /// and a mirror mirrors the record under it in the same breath.
    fn untouched_continuation(&self) -> bool {
        self.draft
            .as_ref()
            .is_some_and(|d| d.continues.is_some() && d.nodes.len() == self.base_len)
    }

    /// The draft as a landing, where it is long enough to be a path and is
    /// more than a path taken up and put back. The draft itself stays put
    /// until `landed`.
    ///
    /// **An open landing of an untouched continuation is `Nothing`.** It would
    /// write the path back as it was, which is an undo entry that does
    /// nothing, a tab marked modified, a redo stack drained, and, where the
    /// first node was the one pressed, the path silently reversed. The closing
    /// press is the exception and is not refused: closing a path is an edit.
    fn land(&self, closed: bool) -> Commit {
        let Some(draft) = self.draft.as_ref() else {
            return Commit::Nothing;
        };
        if draft.nodes.len() < 2 || (!closed && self.untouched_continuation()) {
            return Commit::Nothing;
        }
        Commit::Land {
            paths: vec![Path {
                role: self.draft_role.unwrap_or(self.role),
                closed,
                nodes: draft.nodes.clone(),
                ink: None,
            }],
            replaces: draft.continues,
        }
    }
}

/// The dragged path for a hold with the pointer at `at`, recomputed from the
/// press state, as a fresh copy for the release. `None` for a hold that drags
/// nothing, and for a drag on a line that has not yet left the tap slop.
fn shaped(held: &Held, at: Vec2, alone: bool) -> Option<Path> {
    let start = match held {
        Held::Handle { start, .. } | Held::Node { start, .. } => start,
        Held::Segment {
            press, tap, start, ..
        } => {
            if at.distance(*press) <= *tap {
                return None;
            }
            start
        }
        Held::Nothing | Held::Placing { .. } | Held::Closing | Held::Continuing | Held::Picked => {
            return None;
        }
    };
    let mut out = start.clone();
    shape_into(held, at, alone, &mut out);
    Some(out)
}

/// Write the drag at `at` into `out`, which holds a copy of the press-time
/// path. Only what the drag moves is written, each from the press-time copy,
/// so `out` is never reallocated and a drag back to the press is the path it
/// began as. A line drag still inside the tap slop writes the path where it
/// was: on the release that press is a split, not a move.
fn shape_into(held: &Held, at: Vec2, alone: bool, out: &mut Path) {
    match held {
        Held::Handle {
            node, side, start, ..
        } => {
            let from = start.nodes[*node];
            // Smoothness is judged on the node as it was at the press, and
            // Shift is read now, so pressing it mid-drag takes effect.
            out.nodes[*node] = pathedit::with_handle(from, *side, at - from.at, !alone);
        }
        Held::Node {
            node, press, start, ..
        } => {
            out.nodes[*node].at = start.nodes[*node].at + (at - *press);
        }
        Held::Segment {
            press, tap, start, ..
        } => {
            // `pathedit::moved`, in place.
            let by = if at.distance(*press) > *tap {
                at - *press
            } else {
                Vec2::ZERO
            };
            for (moved, was) in out.nodes.iter_mut().zip(&start.nodes) {
                moved.at = was.at + by;
            }
        }
        Held::Nothing | Held::Placing { .. } | Held::Closing | Held::Continuing | Held::Picked => {}
    }
}

/// An open path with an end node under the press, as a draft that goes on at
/// that end. The last node wins where both ends are in reach, because that is
/// the end a path being drawn grows from.
fn continuation(ground: Ground<'_>, at: Vec2, hit: f32) -> Option<(Draft, PathRole, bool)> {
    let record = ground.paths?;
    for (index, path) in record.paths().iter().enumerate() {
        // An open Ink path is not a line, and landing it would strip its ink.
        if path.closed || !is_line(path) {
            continue;
        }
        let (Some(first), Some(last)) = (path.nodes.first(), path.nodes.last()) else {
            continue;
        };
        let (nodes, reversed) = if last.at.distance(at) <= hit {
            (path.nodes.clone(), false)
        } else if first.at.distance(at) <= hit {
            (pathedit::reversed(path).nodes, true)
        } else {
            continue;
        };
        return Some((
            Draft {
                nodes,
                closed: false,
                continues: Some(index),
            },
            path.role,
            reversed,
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    const REACH: Reach = Reach { hit: 8.0, tap: 6.0 };

    fn bare() -> Ground<'static> {
        Ground {
            paths: None,
            reach: REACH,
            nodes_shown: true,
        }
    }

    /// Press and release on one spot: a corner.
    fn tap(tool: &mut PathTool, at: Vec2, ground: Ground<'_>) -> Commit {
        tool.press(at, false, ground);
        tool.release(at, ground)
    }

    fn land_of(c: Commit) -> (Vec<Path>, Option<usize>) {
        match c {
            Commit::Land { paths, replaces } => (paths, replaces),
            Commit::Nothing | Commit::Set(_) => panic!("expected a landing, got {c:?}"),
        }
    }

    fn open(role: PathRole, points: &[(f32, f32)], closed: bool) -> Path {
        Path {
            role,
            closed,
            nodes: points.iter().map(|&(x, y)| Node::corner(v(x, y))).collect(),
            ink: None,
        }
    }

    #[test]
    fn an_interrupt_takes_back_the_node_being_placed_and_nothing_else() {
        let mut t = PathTool::default();
        tap(&mut t, v(0.0, 0.0), bare());
        t.press(v(50.0, 0.0), false, bare());
        t.moved(v(70.0, 20.0), false);
        t.release(v(70.0, 20.0), bare());
        let curved = t.draft().unwrap().nodes[1];
        assert_ne!(curved.handle_out, Vec2::ZERO, "the fixture pulls a curve");

        t.press(v(100.0, 0.0), false, bare());
        t.moved(v(130.0, 30.0), false);
        t.interrupt();
        assert!(!t.is_held());
        t.moved(v(200.0, 200.0), false);

        let draft = t.draft().unwrap();
        assert_eq!(draft.nodes.len(), 2);
        assert_eq!(draft.nodes[1], curved, "the node before is untouched");
    }

    #[test]
    fn an_interrupt_of_the_first_press_ends_the_draft() {
        let mut t = PathTool::default();
        t.press(v(0.0, 0.0), false, bare());
        t.interrupt();
        assert!(t.draft().is_none());
    }

    /// A closing press interrupted has not closed anything; a continuation
    /// press added no node and loses none.
    #[test]
    fn an_interrupted_close_reopens_and_an_interrupted_continuation_keeps_its_nodes() {
        let mut t = PathTool::default();
        for p in [v(0.0, 0.0), v(50.0, 0.0), v(50.0, 50.0)] {
            tap(&mut t, p, bare());
        }
        t.press(v(0.0, 0.0), false, bare());
        assert!(t.draft().unwrap().closed, "the press closed it");
        t.interrupt();
        let draft = t.draft().unwrap();
        assert!(!draft.closed);
        assert_eq!(draft.nodes.len(), 3);

        let record = VectorObject::empty()
            .appended(vec![open(
                PathRole::Cut,
                &[(0.0, 0.0), (100.0, 0.0)],
                false,
            )])
            .unwrap();
        let ground = Ground {
            paths: Some(&record),
            ..bare()
        };
        let mut t = PathTool::default();
        t.press(v(100.0, 0.0), false, ground);
        assert_eq!(t.draft().unwrap().continues, Some(0));
        t.interrupt();
        assert_eq!(t.draft().unwrap().nodes.len(), 2);
    }

    #[test]
    fn a_mirror_carries_the_draft_and_twice_puts_it_back() {
        let canvas = UVec2::new(1000, 800);
        let mut t = PathTool::default();
        tap(&mut t, v(100.0, 50.0), bare());
        t.press(v(300.0, 60.0), false, bare());
        t.moved(v(320.0, 90.0), false);
        t.release(v(320.0, 90.0), bare());
        let before = t.draft().unwrap().nodes.clone();

        t.mirror(FlipAxis::Horizontal, canvas);
        let after = &t.draft().unwrap().nodes;
        assert_eq!(after[0].at, v(900.0, 50.0));
        assert_eq!(after[1].at, v(700.0, 60.0));
        assert_eq!(after[1].handle_out, v(-20.0, 30.0));
        assert_eq!(after[1].handle_in, v(20.0, -30.0));
        t.mirror(FlipAxis::Horizontal, canvas);
        assert_eq!(t.draft().unwrap().nodes, before);

        t.mirror(FlipAxis::Vertical, canvas);
        assert_eq!(t.draft().unwrap().nodes[0].at, v(100.0, 750.0));
    }

    /// The document going away takes the draft and the pick, never the
    /// settings: Draw or Edit and Cut or Plot are what the artist chose for the
    /// tool, not for the document.
    #[test]
    fn a_reset_keeps_the_mode_and_the_role_and_drops_the_rest() {
        let mut t = PathTool {
            mode: PathMode::Edit,
            role: PathRole::Plot,
            ..PathTool::default()
        };
        // A draft, by borrowing Draw on a tap the Edit mode would not place.
        t.press(v(0.0, 0.0), false, bare());
        t.release(v(0.0, 0.0), bare());
        t.mode = PathMode::Draw;
        tap(&mut t, v(10.0, 0.0), bare());
        assert!(t.draft().is_some(), "the fixture has something to drop");
        t.mode = PathMode::Edit;
        t.reset();
        assert!(t.draft().is_none());
        assert_eq!(t.pick(), None);
        assert_eq!(t.said(), None);
        assert_eq!((t.mode, t.role), (PathMode::Edit, PathRole::Plot));
    }

    #[test]
    fn three_taps_and_enter_make_an_open_path_of_corners() {
        let mut t = PathTool::default();
        for p in [v(0.0, 0.0), v(50.0, 0.0), v(50.0, 50.0)] {
            assert_eq!(tap(&mut t, p, bare()), Commit::Nothing);
        }
        let c = t.finish();
        assert_eq!(
            c,
            Commit::Land {
                paths: vec![open(
                    PathRole::Cut,
                    &[(0.0, 0.0), (50.0, 0.0), (50.0, 50.0)],
                    false
                )],
                replaces: None,
            }
        );
        assert!(t.draft().is_some(), "the draft stays up until landed");
        t.landed(Ok(()));
        assert!(t.draft().is_none());
    }

    #[test]
    fn a_drag_pulls_opposite_handles_out() {
        let mut t = PathTool::default();
        t.press(v(10.0, 10.0), false, bare());
        assert!(t.is_held());
        t.moved(v(30.0, 10.0), false);
        t.release(v(30.0, 10.0), bare());
        assert!(!t.is_held());
        let n = t.draft().unwrap().nodes[0];
        assert_eq!(n.handle_out, v(20.0, 0.0));
        assert_eq!(n.handle_in, v(-20.0, 0.0));
    }

    #[test]
    fn a_wobble_inside_the_tap_slop_leaves_a_corner() {
        let mut t = PathTool::default();
        t.press(v(10.0, 10.0), false, bare());
        t.moved(v(14.0, 10.0), false);
        t.release(v(14.0, 10.0), bare());
        let n = t.draft().unwrap().nodes[0];
        assert_eq!(n.handle_out, Vec2::ZERO);
        assert_eq!(n.handle_in, Vec2::ZERO);
    }

    #[test]
    fn pressing_the_first_node_closes_a_path_of_three() {
        let mut t = PathTool::default();
        for p in [v(0.0, 0.0), v(50.0, 0.0), v(50.0, 50.0)] {
            tap(&mut t, p, bare());
        }
        // Press within reach of the first node, not exactly on it.
        t.press(v(3.0, 2.0), false, bare());
        let (paths, replaces) = land_of(t.release(v(3.0, 2.0), bare()));
        assert_eq!(replaces, None);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].closed);
        assert_eq!(paths[0].nodes.len(), 3, "the closing press adds no node");
    }

    #[test]
    fn pressing_the_first_node_of_two_is_an_ordinary_node() {
        let mut t = PathTool::default();
        tap(&mut t, v(0.0, 0.0), bare());
        tap(&mut t, v(50.0, 0.0), bare());
        t.press(v(1.0, 0.0), false, bare());
        assert_eq!(t.release(v(1.0, 0.0), bare()), Commit::Nothing);
        let d = t.draft().unwrap();
        assert_eq!(d.nodes.len(), 3);
        assert!(!d.closed);
    }

    #[test]
    fn enter_on_one_node_does_nothing_and_keeps_it() {
        let mut t = PathTool::default();
        tap(&mut t, v(0.0, 0.0), bare());
        assert_eq!(t.finish(), Commit::Nothing);
        assert_eq!(t.draft().unwrap().nodes.len(), 1);
    }

    #[test]
    fn backspace_takes_the_last_node_and_the_only_one_ends_the_draft() {
        let mut t = PathTool::default();
        assert!(!t.pop(), "nothing to take with no draft");
        tap(&mut t, v(0.0, 0.0), bare());
        tap(&mut t, v(9.0, 9.0), bare());
        assert!(t.pop());
        assert_eq!(t.draft().unwrap().nodes.len(), 1);
        assert!(t.pop());
        assert!(t.draft().is_none());
    }

    #[test]
    fn escape_abandons_and_says_so_only_when_there_was_a_draft() {
        let mut t = PathTool::default();
        tap(&mut t, v(0.0, 0.0), bare());
        assert!(t.abandon());
        assert!(t.draft().is_none());
        assert!(!t.abandon());
    }

    #[test]
    fn a_refused_landing_keeps_the_draft_and_says_why() {
        let mut t = PathTool::default();
        tap(&mut t, v(0.0, 0.0), bare());
        tap(&mut t, v(50.0, 0.0), bare());
        let _ = t.finish();
        t.landed(Err("x"));
        assert!(t.draft().is_some());
        assert_eq!(t.said(), Some("x"));
        t.press(v(80.0, 0.0), false, bare());
        assert_eq!(t.said(), None, "the next press clears it");
    }

    #[test]
    fn settling_keeps_two_nodes_and_drops_one() {
        let mut t = PathTool::default();
        tap(&mut t, v(0.0, 0.0), bare());
        assert_eq!(t.settle(), Commit::Nothing);
        assert!(t.draft().is_none());

        tap(&mut t, v(0.0, 0.0), bare());
        tap(&mut t, v(50.0, 0.0), bare());
        let (paths, _) = land_of(t.settle());
        assert_eq!(paths[0].nodes.len(), 2);
        assert!(t.draft().is_some(), "the caller says how it went");
    }

    #[test]
    fn the_role_in_hand_is_the_new_paths_role() {
        let mut t = PathTool {
            role: PathRole::Plot,
            ..PathTool::default()
        };
        tap(&mut t, v(0.0, 0.0), bare());
        tap(&mut t, v(50.0, 0.0), bare());
        let (paths, _) = land_of(t.finish());
        assert_eq!(paths[0].role, PathRole::Plot);
        assert_eq!(paths[0].ink, None);
        assert_eq!(paths[0].nodes[0].pressure, 1.0);
    }

    fn record() -> VectorObject {
        VectorObject::Paths(vec![open(
            PathRole::Plot,
            &[(0.0, 0.0), (50.0, 0.0)],
            false,
        )])
    }

    fn on<'a>(rec: &'a VectorObject) -> Ground<'a> {
        Ground {
            paths: Some(rec),
            reach: REACH,
            nodes_shown: true,
        }
    }

    #[test]
    fn a_press_on_an_open_paths_last_node_continues_it() {
        let rec = record();
        let mut t = PathTool::default();
        assert_eq!(t.role, PathRole::Cut);
        t.press(v(51.0, 0.0), false, on(&rec));
        assert!(t.is_held());
        assert_eq!(t.release(v(51.0, 0.0), on(&rec)), Commit::Nothing);
        assert_eq!(t.draft().unwrap().nodes.len(), 2, "the press added no node");
        tap(&mut t, v(50.0, 40.0), on(&rec));
        let (paths, replaces) = land_of(t.finish());
        assert_eq!(replaces, Some(0));
        let at: Vec<Vec2> = paths[0].nodes.iter().map(|n| n.at).collect();
        assert_eq!(at, vec![v(0.0, 0.0), v(50.0, 0.0), v(50.0, 40.0)]);
        assert_eq!(paths[0].role, PathRole::Plot);
    }

    #[test]
    fn continuing_from_the_first_node_reverses() {
        let rec = record();
        let mut t = PathTool::default();
        t.press(v(0.0, 0.0), false, on(&rec));
        t.release(v(0.0, 0.0), on(&rec));
        tap(&mut t, v(0.0, 40.0), on(&rec));
        let (paths, replaces) = land_of(t.finish());
        assert_eq!(replaces, Some(0));
        let at: Vec<Vec2> = paths[0].nodes.iter().map(|n| n.at).collect();
        assert_eq!(at, vec![v(50.0, 0.0), v(0.0, 0.0), v(0.0, 40.0)]);
    }

    #[test]
    fn the_last_node_wins_where_both_ends_are_in_reach() {
        // Ends ten apart, press between them: both within the hit radius.
        let rec = VectorObject::Paths(vec![open(
            PathRole::Cut,
            &[(0.0, 0.0), (40.0, 0.0), (10.0, 0.0)],
            false,
        )]);
        let mut t = PathTool::default();
        t.press(v(5.0, 0.0), false, on(&rec));
        let at: Vec<Vec2> = t.draft().unwrap().nodes.iter().map(|n| n.at).collect();
        assert_eq!(at, vec![v(0.0, 0.0), v(40.0, 0.0), v(10.0, 0.0)]);
    }

    #[test]
    fn a_closed_paths_node_does_not_continue_it() {
        let rec = VectorObject::Paths(vec![open(
            PathRole::Plot,
            &[(0.0, 0.0), (50.0, 0.0), (50.0, 50.0)],
            true,
        )]);
        let mut t = PathTool::default();
        t.press(v(50.0, 50.0), false, on(&rec));
        let d = t.draft().unwrap();
        assert_eq!(d.continues, None);
        assert_eq!(d.nodes.len(), 1, "a fresh draft with the press as its node");
    }

    #[test]
    fn popping_into_a_continued_path_abandons_the_draft() {
        let rec = record();
        let mut t = PathTool::default();
        t.press(v(51.0, 0.0), false, on(&rec));
        t.release(v(51.0, 0.0), on(&rec));
        tap(&mut t, v(50.0, 40.0), on(&rec));
        assert!(t.pop(), "the new node comes off");
        assert_eq!(t.draft().unwrap().nodes.len(), 2);
        assert!(t.pop(), "the next would eat the path, so the draft goes");
        assert!(t.draft().is_none());
    }

    #[test]
    fn the_rubber_band_follows_a_hover() {
        let mut t = PathTool::default();
        assert_eq!(t.hover(), None);
        t.moved(v(7.0, 8.0), false);
        assert_eq!(t.hover(), Some(v(7.0, 8.0)));
    }

    #[test]
    fn no_mode_is_missing_from_all() {
        // Exhaustive on purpose: a third mode must say where it sits.
        for mode in PathMode::ALL {
            let position = match mode {
                PathMode::Draw => 0,
                PathMode::Edit => 1,
            };
            assert_eq!(PathMode::ALL[position], mode);
        }
        assert_eq!(PathMode::Draw.label(), "Draw");
        assert_eq!(PathMode::Edit.label(), "Edit");
    }

    // ---- Edit mode ----

    fn corners(role: PathRole, closed: bool, points: &[(f32, f32)]) -> Path {
        open(role, points, closed)
    }

    /// Path 0 open Cut, path 1 closed Plot square, path 2 Ink.
    fn edit_record() -> VectorObject {
        VectorObject::Paths(vec![
            corners(PathRole::Cut, false, &[(0.0, 0.0), (100.0, 0.0)]),
            corners(
                PathRole::Plot,
                true,
                &[(200.0, 0.0), (300.0, 0.0), (300.0, 100.0), (200.0, 100.0)],
            ),
            corners(PathRole::Ink, false, &[(0.0, 200.0), (100.0, 200.0)]),
        ])
    }

    fn edit_tap(t: &mut PathTool, at: Vec2, g: Ground<'_>) -> Commit {
        t.press(at, true, g);
        t.release(at, g)
    }

    fn set_of(c: Commit) -> VectorObject {
        match c {
            Commit::Set(r) => r,
            Commit::Nothing | Commit::Land { .. } => panic!("expected a Set, got {c:?}"),
        }
    }

    /// Pick path 0 by its line, then its node 1 by the node.
    fn with_node_picked(rec: &VectorObject) -> PathTool {
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(rec));
        edit_tap(&mut t, v(100.0, 0.0), on(rec));
        assert_eq!(
            t.pick(),
            Some(Pick {
                path: 0,
                node: Some(1)
            })
        );
        t
    }

    #[test]
    fn a_press_on_a_line_picks_its_path_and_none_of_its_nodes() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 3.0), on(&rec));
        assert_eq!(
            t.pick(),
            Some(Pick {
                path: 0,
                node: None
            })
        );
    }

    #[test]
    fn the_nearest_of_two_paths_in_reach_is_picked() {
        let rec = VectorObject::Paths(vec![
            corners(PathRole::Cut, false, &[(0.0, 0.0), (100.0, 0.0)]),
            corners(PathRole::Cut, false, &[(0.0, 6.0), (100.0, 6.0)]),
        ]);
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 4.0), on(&rec));
        assert_eq!(t.pick().map(|p| p.path), Some(1));
    }

    #[test]
    fn an_ink_path_is_never_picked() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 200.0), on(&rec));
        assert_eq!(t.pick(), None);
    }

    #[test]
    fn a_press_on_nothing_lets_go() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        edit_tap(&mut t, v(500.0, 500.0), on(&rec));
        assert_eq!(t.pick(), None);
    }

    #[test]
    fn no_ground_means_a_press_does_nothing() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        let before = t.pick();
        t.press(v(250.0, 0.0), true, bare());
        assert_eq!(t.pick(), before);
        assert!(!t.is_held());
        assert!(t.dragged().is_none());
    }

    #[test]
    fn dragging_a_picked_node_moves_it_and_is_one_set() {
        let rec = edit_record();
        let mut t = with_node_picked(&rec);
        t.press(v(100.0, 0.0), true, on(&rec));
        t.moved(v(110.0, 10.0), false);
        assert_eq!(t.dragged().unwrap().nodes[1].at, v(110.0, 10.0));
        let got = set_of(t.release(v(120.0, 30.0), on(&rec)));
        assert_eq!(got.paths()[0].nodes[1].at, v(120.0, 30.0));
        assert_eq!(got.paths()[0].nodes[0].at, v(0.0, 0.0));
        assert_eq!(got.paths()[1], rec.paths()[1]);
        assert_eq!(got.paths()[2], rec.paths()[2]);
        assert!(t.dragged().is_none());
    }

    #[test]
    fn a_drag_back_to_where_it_began_commits_nothing() {
        let rec = edit_record();
        let mut t = with_node_picked(&rec);
        t.press(v(100.0, 0.0), true, on(&rec));
        t.moved(v(160.0, 40.0), false);
        t.moved(v(100.0, 0.0), false);
        assert_eq!(t.release(v(100.0, 0.0), on(&rec)), Commit::Nothing);
    }

    #[test]
    fn a_tap_on_the_picked_segment_inserts_a_node_and_picks_it() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        let got = set_of(edit_tap(&mut t, v(50.0, 0.0), on(&rec)));
        assert_eq!(got.paths()[0].nodes.len(), 3);
        assert_eq!(got.paths()[0].nodes[1].at, v(50.0, 0.0));
        assert_eq!(t.pick().unwrap().node, Some(1));
    }

    #[test]
    fn a_drag_on_the_picked_segment_moves_the_whole_path() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        t.press(v(50.0, 0.0), true, on(&rec));
        t.moved(v(50.0, 30.0), false);
        let got = set_of(t.release(v(50.0, 30.0), on(&rec)));
        let at: Vec<Vec2> = got.paths()[0].nodes.iter().map(|n| n.at).collect();
        assert_eq!(at, vec![v(0.0, 30.0), v(100.0, 30.0)]);
        assert_eq!(t.pick().unwrap().path, 0);
    }

    /// Path 0 with a smooth middle node at (50, 0), handles of ten.
    fn smooth_record() -> VectorObject {
        let mut p = corners(
            PathRole::Cut,
            false,
            &[(0.0, 0.0), (50.0, 0.0), (100.0, 0.0)],
        );
        p.nodes[1].handle_in = v(-10.0, 0.0);
        p.nodes[1].handle_out = v(10.0, 0.0);
        VectorObject::Paths(vec![p])
    }

    /// Drag the middle node's out handle from (60, 0) to (60, 20) and return
    /// the node that results.
    fn drag_out_handle(shift: bool) -> Node {
        let rec = smooth_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(25.0, 0.0), on(&rec));
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        t.press(v(60.0, 0.0), true, on(&rec));
        t.moved(v(60.0, 20.0), shift);
        let got = set_of(t.release(v(60.0, 20.0), on(&rec)));
        got.paths()[0].nodes[1]
    }

    #[test]
    fn dragging_a_smooth_handle_turns_its_twin_and_shift_does_not() {
        let turned = drag_out_handle(false);
        assert_eq!(turned.handle_out, v(10.0, 20.0));
        let dir = v(10.0, 20.0).normalize();
        assert!((turned.handle_in - (-dir * 10.0)).length() < 1e-4);
        let cusp = drag_out_handle(true);
        assert_eq!(cusp.handle_out, v(10.0, 20.0));
        assert_eq!(cusp.handle_in, v(-10.0, 0.0));
    }

    #[test]
    fn pressing_shift_mid_drag_takes_effect() {
        let rec = smooth_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(25.0, 0.0), on(&rec));
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        t.press(v(60.0, 0.0), true, on(&rec));
        t.moved(v(60.0, 20.0), false);
        t.moved(v(60.0, 22.0), true);
        assert_eq!(t.dragged().unwrap().nodes[1].handle_in, v(-10.0, 0.0));
    }

    #[test]
    fn a_handle_is_only_hit_on_the_picked_node() {
        let mut p = corners(PathRole::Cut, false, &[(0.0, 0.0), (100.0, 0.0)]);
        p.nodes[0].handle_out = v(0.0, -40.0);
        let rec = VectorObject::Paths(vec![p]);
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, -15.0), on(&rec));
        edit_tap(&mut t, v(100.0, 0.0), on(&rec));
        // Node 1 is picked; node 0's handle tip is not its to answer for.
        t.press(v(0.0, -40.0), true, on(&rec));
        t.moved(v(30.0, -40.0), false);
        assert_eq!(t.release(v(30.0, -40.0), on(&rec)), Commit::Nothing);
        assert_eq!(t.pick(), None);
    }

    #[test]
    fn with_nodes_hidden_a_press_on_a_node_reads_as_the_segment() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        let hidden = Ground {
            nodes_shown: false,
            ..on(&rec)
        };
        let got = set_of(edit_tap(&mut t, v(100.0, 0.0), hidden));
        assert_eq!(got.paths()[0].nodes.len(), 3);
    }

    #[test]
    fn delete_removes_the_node_then_the_short_path_goes_whole() {
        let rec = edit_record();
        let mut t = with_node_picked(&rec);
        let got = set_of(t.remove(on(&rec)));
        assert_eq!(got.paths().len(), 2);
        assert_eq!(got.paths()[0], rec.paths()[1]);
        assert_eq!(t.pick(), None);
    }

    #[test]
    fn delete_takes_one_node_from_a_longer_path_and_keeps_the_path() {
        let rec = smooth_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(25.0, 0.0), on(&rec));
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        let got = set_of(t.remove(on(&rec)));
        assert_eq!(got.paths()[0].nodes.len(), 2);
        assert_eq!(
            t.pick(),
            Some(Pick {
                path: 0,
                node: None
            })
        );
    }

    #[test]
    fn delete_with_no_node_removes_the_path() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        let got = set_of(t.remove(on(&rec)));
        assert_eq!(got.paths().len(), 2);
        assert_eq!(t.pick(), None);
    }

    #[test]
    fn the_role_switch_changes_the_pick_and_not_the_default() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(250.0, 0.0), on(&rec));
        assert_eq!(t.pick().unwrap().path, 1);
        assert_eq!(t.role_shown(on(&rec)), PathRole::Plot);
        let got = set_of(t.set_role(PathRole::Cut, on(&rec)));
        assert_eq!(got.paths()[1].role, PathRole::Cut);
        assert_eq!(t.role, PathRole::Cut);
        // The path is already Plot: nothing to record.
        assert_eq!(t.set_role(PathRole::Plot, on(&rec)), Commit::Nothing);
        assert_eq!(
            t.role,
            PathRole::Cut,
            "the default is not the pick's to set"
        );

        let mut t = PathTool {
            role: PathRole::Plot,
            ..PathTool::default()
        };
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        assert_eq!(t.role_shown(on(&rec)), PathRole::Cut);
        let got = set_of(t.set_role(PathRole::Plot, on(&rec)));
        assert_eq!(got.paths()[0].role, PathRole::Plot);
        assert_eq!(t.role, PathRole::Plot);

        // Tool Cut, pick a Cut path: the pick turns Plot, the default stays.
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        let got = set_of(t.set_role(PathRole::Plot, on(&rec)));
        assert_eq!(got.paths()[0].role, PathRole::Plot);
        assert_eq!(t.role, PathRole::Cut);
    }

    #[test]
    fn a_pick_on_unreadable_ground_leaves_the_default_alone() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        assert_eq!(t.set_role(PathRole::Plot, bare()), Commit::Nothing);
        assert_eq!(t.role, PathRole::Cut);
        assert!(t.pick().is_some());
    }

    #[test]
    fn with_no_pick_the_switch_sets_the_default() {
        let rec = edit_record();
        let mut t = PathTool::default();
        assert_eq!(t.set_role(PathRole::Plot, on(&rec)), Commit::Nothing);
        assert_eq!(t.role, PathRole::Plot);
        assert_eq!(t.role_shown(on(&rec)), PathRole::Plot);
    }

    #[test]
    fn closing_an_open_path_of_two_is_refused_quietly() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        assert_eq!(t.toggle_closed(on(&rec)), Commit::Nothing);
        assert_eq!(t.said(), None);
    }

    #[test]
    fn opening_and_closing_the_square_round_trips() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(250.0, 0.0), on(&rec));
        let opened = set_of(t.toggle_closed(on(&rec)));
        assert!(!opened.paths()[1].closed);
        let closed = set_of(t.toggle_closed(on(&opened)));
        assert_eq!(closed, rec);
    }

    #[test]
    fn corner_zeroes_both_handles_and_smooth_is_off_at_an_open_end() {
        let rec = smooth_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(25.0, 0.0), on(&rec));
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        let got = set_of(t.corner(on(&rec)));
        let n = got.paths()[0].nodes[1];
        assert_eq!((n.handle_in, n.handle_out), (Vec2::ZERO, Vec2::ZERO));
        // Now a corner between two neighbours: smoothing it is a real change.
        assert!(t.can_smooth(on(&got)));
        let smoothed = set_of(t.smooth(on(&got)));
        assert_ne!(smoothed.paths()[0].nodes[1].handle_out, Vec2::ZERO);

        // An end node of an open path is left alone.
        edit_tap(&mut t, v(0.0, 0.0), on(&rec));
        assert_eq!(t.pick().unwrap().node, Some(0));
        assert!(!t.can_smooth(on(&rec)));
        assert_eq!(t.smooth(on(&rec)), Commit::Nothing);
    }

    #[test]
    fn a_drag_past_the_bound_changes_nothing_and_says_so() {
        let rec = edit_record();
        let mut t = with_node_picked(&rec);
        t.press(v(100.0, 0.0), true, on(&rec));
        t.moved(v(1.0e6, 0.0), false);
        let c = t.release(v(2.0e6, 0.0), on(&rec));
        assert_eq!(c, Commit::Nothing);
        assert_eq!(t.said(), Some(OUT_OF_BOUNDS));
        assert_eq!(t.pick().unwrap().node, Some(1));
    }

    #[test]
    fn a_refused_set_puts_the_pick_back() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        let _ = set_of(edit_tap(&mut t, v(50.0, 0.0), on(&rec)));
        assert_eq!(t.pick().unwrap().node, Some(1));
        t.applied(Err("no"));
        assert_eq!(t.said(), Some("no"));
        assert_eq!(t.pick().unwrap().node, None);
    }

    #[test]
    fn a_stale_pick_is_dropped_not_indexed() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(250.0, 0.0), on(&rec));
        assert_eq!(t.pick().unwrap().path, 1);
        let short = VectorObject::Paths(vec![rec.paths()[0].clone()]);
        assert_eq!(t.remove(on(&short)), Commit::Nothing);
        assert_eq!(t.pick(), None);
    }

    #[test]
    fn a_stale_node_is_dropped_not_indexed() {
        let long = smooth_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(25.0, 0.0), on(&long));
        edit_tap(&mut t, v(100.0, 0.0), on(&long));
        assert_eq!(t.pick().unwrap().node, Some(2));
        // The same path, now two nodes long: node 2 is off its end.
        let short = edit_record();
        assert_eq!(t.corner(on(&short)), Commit::Nothing);
        assert_eq!(
            t.pick(),
            Some(Pick {
                path: 0,
                node: None
            })
        );
    }

    #[test]
    fn escape_lets_go_of_the_node_then_the_path() {
        let rec = edit_record();
        let mut t = with_node_picked(&rec);
        assert!(t.abandon());
        assert_eq!(
            t.pick(),
            Some(Pick {
                path: 0,
                node: None
            })
        );
        assert!(t.abandon());
        assert_eq!(t.pick(), None);
        assert!(!t.abandon());
    }

    #[test]
    fn ctrl_in_draw_is_an_edit_press_and_a_plain_one_lets_go_and_draws() {
        let rec = edit_record();
        let mut t = PathTool::default();
        assert_eq!(t.mode, PathMode::Draw);
        t.press(v(50.0, 0.0), true, on(&rec));
        t.release(v(50.0, 0.0), on(&rec));
        assert!(t.pick().is_some());
        t.press(v(500.0, 500.0), false, on(&rec));
        assert_eq!(t.pick(), None);
        assert_eq!(t.draft().unwrap().nodes.len(), 1);
    }

    #[test]
    fn an_open_ink_path_is_not_continued() {
        let rec = edit_record();
        let mut t = PathTool::default();
        t.press(v(100.0, 200.0), false, on(&rec));
        t.release(v(100.0, 200.0), on(&rec));
        assert_eq!(t.draft().unwrap().continues, None);
        assert_eq!(t.draft().unwrap().nodes.len(), 1);
    }

    #[test]
    fn a_refused_close_is_forgotten_once_drawing_goes_on() {
        let mut t = PathTool::default();
        for p in [v(0.0, 0.0), v(50.0, 0.0), v(50.0, 50.0)] {
            tap(&mut t, p, bare());
        }
        tap(&mut t, v(0.0, 0.0), bare());
        t.landed(Err("full"));
        tap(&mut t, v(0.0, 50.0), bare());
        assert!(!t.draft().unwrap().closed, "the draft is drawn open again");
        let (paths, _) = land_of(t.finish());
        assert_eq!(paths[0].nodes.len(), 4);
        assert!(!paths[0].closed);
    }

    #[test]
    fn enter_after_a_refused_close_lands_open() {
        let mut t = PathTool::default();
        for p in [v(0.0, 0.0), v(50.0, 0.0), v(50.0, 50.0)] {
            tap(&mut t, p, bare());
        }
        let (closed, _) = land_of(tap(&mut t, v(0.0, 0.0), bare()));
        assert!(closed[0].closed);
        t.landed(Err("full"));
        let (paths, _) = land_of(t.finish());
        assert!(!paths[0].closed);
        let (paths, _) = land_of(t.settle());
        assert!(!paths[0].closed);
    }

    #[test]
    fn an_edit_of_the_tools_own_ends_the_press_it_was_made_during() {
        let rec = edit_record();
        let mut t = with_node_picked(&rec);
        t.press(v(100.0, 0.0), true, on(&rec));
        t.moved(v(130.0, 0.0), false);
        let r1 = set_of(t.remove(on(&rec)));
        assert!(!t.is_held());
        assert!(t.dragged().is_none());
        assert_eq!(t.release(v(130.0, 0.0), on(&r1)), Commit::Nothing);
    }

    #[test]
    fn a_second_press_leaves_no_stale_drag() {
        let rec = edit_record();
        let mut t = with_node_picked(&rec);
        t.press(v(100.0, 0.0), true, on(&rec));
        t.moved(v(130.0, 0.0), false);
        assert!(t.dragged().is_some());
        t.press(v(500.0, 500.0), true, on(&rec));
        assert!(t.dragged().is_none());
        assert_eq!(t.release(v(500.0, 500.0), on(&rec)), Commit::Nothing);
    }

    #[test]
    fn a_stale_pick_is_dropped_by_every_call_that_takes_ground() {
        let rec = edit_record();
        let short = VectorObject::Paths(vec![rec.paths()[0].clone()]);
        let pick_square = || {
            let mut t = PathTool::default();
            edit_tap(&mut t, v(250.0, 0.0), on(&rec));
            assert_eq!(t.pick().unwrap().path, 1);
            t
        };
        let mut t = pick_square();
        t.press(v(50.0, 0.0), true, on(&short));
        assert_eq!(t.pick().map(|p| p.path), Some(0));

        let mut t = pick_square();
        assert_eq!(t.set_role(PathRole::Plot, on(&short)), Commit::Nothing);
        assert_eq!(t.pick(), None);
        assert_eq!(t.role, PathRole::Plot, "no pick left, so it is the default");

        let mut t = pick_square();
        assert_eq!(t.toggle_closed(on(&short)), Commit::Nothing);
        assert_eq!(t.pick(), None);

        let mut t = pick_square();
        assert_eq!(t.role_shown(on(&short)), PathRole::Cut);
        assert!(!t.can_smooth(on(&short)));
        t.release(v(0.0, 0.0), on(&short));
        assert_eq!(t.pick(), None);
    }

    #[test]
    fn a_refusal_with_nothing_outstanding_keeps_the_pick() {
        let rec = edit_record();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        t.applied(Err("no"));
        assert_eq!(t.pick().unwrap().path, 0);
        // And a Set answered before the next press is not undone by a later Err.
        let _ = set_of(t.set_role(PathRole::Plot, on(&rec)));
        t.press(v(500.0, 500.0), true, on(&rec));
        t.applied(Err("late"));
        assert_eq!(t.pick(), None);
    }

    /// A path taken up and given nothing lands nothing, from either end,
    /// by Enter or by a settle, and Enter puts the draft away.
    #[test]
    fn a_continuation_with_nothing_added_lands_nothing() {
        let rec = record();
        for end in [v(51.0, 0.0), v(0.0, 0.0)] {
            let mut t = PathTool::default();
            t.press(end, false, on(&rec));
            t.release(end, on(&rec));
            assert!(t.draft().is_some_and(|d| d.continues == Some(0)));
            assert_eq!(t.finish(), Commit::Nothing);
            assert!(t.draft().is_none(), "Enter puts it back");

            let mut t = PathTool::default();
            t.press(end, false, on(&rec));
            t.release(end, on(&rec));
            assert_eq!(t.settle(), Commit::Nothing);
            assert!(t.draft().is_none());
        }
        // One node added is a real edit.
        let mut t = PathTool::default();
        t.press(v(51.0, 0.0), false, on(&rec));
        t.release(v(51.0, 0.0), on(&rec));
        tap(&mut t, v(50.0, 40.0), on(&rec));
        let (_, replaces) = land_of(t.finish());
        assert_eq!(replaces, Some(0));
    }

    /// Closing a continued path with no node added still lands: the close is
    /// the edit.
    #[test]
    fn closing_a_continued_path_with_nothing_added_still_lands() {
        let rec = VectorObject::Paths(vec![open(
            PathRole::Cut,
            &[(0.0, 0.0), (50.0, 0.0), (50.0, 50.0)],
            false,
        )]);
        let mut t = PathTool::default();
        t.press(v(50.0, 50.0), false, on(&rec));
        t.release(v(50.0, 50.0), on(&rec));
        let (paths, replaces) = land_of(tap(&mut t, v(0.0, 0.0), on(&rec)));
        assert_eq!(replaces, Some(0));
        assert!(paths[0].closed);
        assert_eq!(paths[0].nodes.len(), 3);
    }

    #[test]
    fn detaching_makes_a_continued_draft_a_new_path() {
        let rec = record();
        let mut t = PathTool::default();
        t.press(v(51.0, 0.0), false, on(&rec));
        t.release(v(51.0, 0.0), on(&rec));
        tap(&mut t, v(50.0, 40.0), on(&rec));
        t.detach();
        let (paths, replaces) = land_of(t.finish());
        assert_eq!(replaces, None);
        assert_eq!(paths[0].nodes.len(), 3);
    }

    /// What the painter draws of the tool, as a value to compare: the draft,
    /// the drag, the hover, the pick and whether a press is held.
    fn drawn(t: &PathTool) -> String {
        format!(
            "{:?} {:?} {:?} {:?} {}",
            t.draft().map(|d| (&d.nodes, d.closed)),
            t.dragged(),
            t.hover(),
            t.pick(),
            t.is_held()
        )
    }

    /// **Every public method that changes what is drawn moves the generation**,
    /// each from a state where it does change something, and `say` (the one
    /// that changes only the strip) does not. A hand-kept list: a new `&mut
    /// self` method has to be added here, which is what the doc comment on
    /// `generation` says it bumps.
    #[test]
    fn every_change_to_what_is_drawn_moves_the_generation() {
        let rec = edit_record();
        let check = |name: &str, t: &mut PathTool, f: &mut dyn FnMut(&mut PathTool)| {
            let (was_drawn, was) = (drawn(t), t.generation());
            f(t);
            assert_ne!(
                drawn(t),
                was_drawn,
                "{name}: the fixture changes nothing drawn"
            );
            assert_ne!(
                t.generation(),
                was,
                "{name} changed the picture and kept the number"
            );
        };
        let drafting = || {
            let mut t = PathTool::default();
            tap(&mut t, v(0.0, 0.0), bare());
            tap(&mut t, v(50.0, 0.0), bare());
            t
        };

        check("press", &mut drafting(), &mut |t| {
            t.press(v(90.0, 0.0), false, bare())
        });
        check("moved", &mut drafting(), &mut |t| {
            t.moved(v(9.0, 9.0), false)
        });
        let mut t = drafting();
        t.press(v(90.0, 0.0), false, bare());
        check("release", &mut t, &mut |t| {
            t.release(v(90.0, 0.0), bare());
        });
        let mut t = drafting();
        t.press(v(90.0, 0.0), false, bare());
        check("interrupt", &mut t, &mut |t| t.interrupt());
        check("mirror", &mut drafting(), &mut |t| {
            t.mirror(FlipAxis::Horizontal, UVec2::new(100, 100))
        });
        check("pop", &mut drafting(), &mut |t| {
            t.pop();
        });
        check("abandon", &mut drafting(), &mut |t| {
            t.abandon();
        });
        check("reset", &mut drafting(), &mut |t| t.reset());
        let mut t = drafting();
        let _ = t.finish();
        check("landed", &mut t, &mut |t| t.landed(Ok(())));
        let mut t = PathTool::default();
        tap(&mut t, v(0.0, 0.0), bare());
        // A press held: Enter ends the press.
        t.press(v(40.0, 0.0), false, bare());
        check("finish", &mut t, &mut |t| {
            let _ = t.finish();
        });
        let mut t = PathTool::default();
        tap(&mut t, v(0.0, 0.0), bare());
        check("settle", &mut t, &mut |t| {
            let _ = t.settle();
        });

        let picked = || {
            let mut t = PathTool::default();
            edit_tap(&mut t, v(50.0, 0.0), on(&rec));
            t
        };
        check("let_go", &mut picked(), &mut |t| t.let_go());
        check("remove", &mut picked(), &mut |t| {
            let _ = t.remove(on(&rec));
        });
        let mut t = picked();
        let _ = set_of(edit_tap(&mut t, v(50.0, 0.0), on(&rec)));
        check("applied", &mut t, &mut |t| t.applied(Err("no")));
        let mut t = picked();
        t.press(v(100.0, 0.0), true, on(&rec));
        t.moved(v(120.0, 0.0), false);
        check("set_role", &mut t, &mut |t| {
            let _ = t.set_role(PathRole::Plot, on(&rec));
        });
        let mut t = picked();
        t.press(v(100.0, 0.0), true, on(&rec));
        t.moved(v(120.0, 0.0), false);
        check("toggle_closed", &mut t, &mut |t| {
            let _ = t.toggle_closed(on(&rec));
        });
        let mut t = with_node_picked(&rec);
        t.press(v(100.0, 0.0), true, on(&rec));
        t.moved(v(120.0, 0.0), false);
        check("corner", &mut t, &mut |t| {
            let _ = t.corner(on(&rec));
        });
        let mut t = with_node_picked(&rec);
        t.press(v(100.0, 0.0), true, on(&rec));
        t.moved(v(120.0, 0.0), false);
        check("smooth", &mut t, &mut |t| {
            let _ = t.smooth(on(&rec));
        });

        // `detach` changes nothing drawn, and still moves the number: the rule
        // errs towards a rebuild. `say` changes only the strip and must not.
        let mut t = drafting();
        let was = t.generation();
        t.detach();
        assert_ne!(t.generation(), was, "detach");
        let was = t.generation();
        t.say("Said.");
        assert_eq!(t.generation(), was, "say is not drawn on the canvas");
    }

    /// **A drag writes into one copy and never reallocates it**, for a node, a
    /// handle and a whole-path drag: the buffer the first move made is the
    /// buffer every later move writes, at the same address and capacity.
    #[test]
    fn a_drag_move_writes_into_one_copy_of_the_path() {
        let rec = smooth_record();
        // A node drag, a handle drag, and a drag on the line.
        let mut presses: Vec<(PathTool, Vec2)> = Vec::new();
        let mut t = PathTool::default();
        edit_tap(&mut t, v(25.0, 0.0), on(&rec));
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        presses.push((t, v(50.0, 0.0)));
        let mut t = PathTool::default();
        edit_tap(&mut t, v(25.0, 0.0), on(&rec));
        edit_tap(&mut t, v(50.0, 0.0), on(&rec));
        presses.push((t, v(60.0, 0.0)));
        let mut t = PathTool::default();
        edit_tap(&mut t, v(25.0, 0.0), on(&rec));
        presses.push((t, v(75.0, 0.0)));
        for (mut t, at) in presses {
            t.press(at, true, on(&rec));
            t.moved(at + v(20.0, 20.0), false);
            let first = t.dragged().expect("a drag").nodes.as_ptr();
            let capacity = t.dragged().unwrap().nodes.capacity();
            for step in 0..20 {
                t.moved(at + v(30.0 + step as f32, -10.0), step % 2 == 0);
                let now = &t.dragged().expect("still a drag").nodes;
                assert_eq!(now.as_ptr(), first, "reallocated at {at:?}");
                assert_eq!(now.capacity(), capacity);
            }
            // And the copy is still absolute: back at the press it is the path.
            t.moved(at, false);
            assert_eq!(t.dragged().unwrap(), &rec.paths()[0], "at {at:?}");
        }
    }

    #[test]
    fn every_refusal_has_a_sentence_and_only_the_bound_has_its_own_wording() {
        for why in VectorRefusal::ALL {
            let said = refusal_sentence(why);
            match why {
                VectorRefusal::Impossible => assert_eq!(said, OUT_OF_BOUNDS),
                VectorRefusal::TooManyNodes
                | VectorRefusal::TooManyPaths
                | VectorRefusal::Opaque => assert_eq!(said, why.sentence()),
            }
        }
    }

    /// One closed Cut path: a circle of radius 100 about (200, 200) as 360
    /// corner nodes, each pushed in or out by up to a pixel, as a lasso would
    /// hand it. Irregular on purpose: a zigzag as regular as a saw reads as
    /// real corners to the fitter, which keeps them at any tolerance.
    fn wobbly_record() -> VectorObject {
        let points: Vec<(f32, f32)> = (0..360)
            .map(|i| {
                let a = (i as f32).to_radians();
                let r = 100.0 + ((i * 7919) % 13) as f32 / 6.0 - 1.0;
                (200.0 + r * a.cos(), 200.0 + r * a.sin())
            })
            .collect();
        VectorObject::Paths(vec![open(PathRole::Cut, &points, true)])
    }

    fn with_wobbly_picked(rec: &VectorObject) -> PathTool {
        let mut t = PathTool::default();
        let at = rec.paths()[0].nodes[0].at;
        edit_tap(&mut t, at, on(rec));
        edit_tap(&mut t, at, on(rec));
        assert_eq!(t.pick().map(|p| p.path), Some(0));
        t
    }

    #[test]
    fn a_smoothing_preview_writes_nothing_and_the_release_writes_it_once() {
        let rec = wobbly_record();
        let mut t = with_wobbly_picked(&rec);
        assert_eq!(t.smoothing_shown(), 0.0);
        t.preview_smoothing(1.0, on(&rec));
        t.preview_smoothing(4.0, on(&rec));
        assert!(t.smoothing_pending());
        let shown = t.dragged().expect("a preview").nodes.len();
        assert!(shown < 30, "{shown} nodes from 360");
        assert_eq!(t.pick().and_then(|p| p.node), None, "the node pick went");

        let after = set_of(t.commit_smoothing(on(&rec)));
        assert_eq!(after.paths()[0].nodes.len(), shown);
        assert!(after.paths()[0].closed);
        assert!(!t.smoothing_pending() && t.dragged().is_none());
        t.applied(Ok(()));
        assert_eq!(t.smoothing_shown(), 4.0);
        // A second release with nothing moved writes nothing.
        assert_eq!(t.commit_smoothing(on(&after)), Commit::Nothing);
    }

    #[test]
    fn smoothing_back_to_off_puts_the_path_back_as_it_was() {
        let rec = wobbly_record();
        let mut t = with_wobbly_picked(&rec);
        t.preview_smoothing(6.0, on(&rec));
        let smoothed = set_of(t.commit_smoothing(on(&rec)));
        t.applied(Ok(()));
        // From the source, not from the refit: a lighter touch after a heavy
        // one keeps more nodes than the heavy one did.
        t.preview_smoothing(1.0, on(&smoothed));
        let lighter = set_of(t.commit_smoothing(on(&smoothed)));
        t.applied(Ok(()));
        assert!(lighter.paths()[0].nodes.len() > smoothed.paths()[0].nodes.len());
        // The node count alone holds for a refit of the refit too; this is
        // what tells the two apart.
        assert_eq!(
            lighter.paths()[0],
            pathedit::resmoothed(&rec.paths()[0], 1.0).unwrap()
        );
        t.preview_smoothing(0.0, on(&lighter));
        let back = set_of(t.commit_smoothing(on(&lighter)));
        assert_eq!(back.paths()[0], rec.paths()[0]);
    }

    #[test]
    fn another_edit_makes_the_path_as_it_then_stands_the_source() {
        let rec = wobbly_record();
        let mut t = with_wobbly_picked(&rec);
        t.preview_smoothing(6.0, on(&rec));
        let smoothed = set_of(t.commit_smoothing(on(&rec)));
        t.applied(Ok(()));
        let opened = set_of(t.toggle_closed(on(&smoothed)));
        t.applied(Ok(()));
        assert_eq!(t.smoothing_shown(), 0.0);
        // Off is now the opened, smoothed path, not the wobbly original.
        t.preview_smoothing(0.0, on(&opened));
        assert_eq!(t.dragged(), Some(&opened.paths()[0]));
        assert_eq!(t.commit_smoothing(on(&opened)), Commit::Nothing);
    }

    #[test]
    fn letting_go_of_the_pick_drops_the_preview_and_the_source() {
        let rec = wobbly_record();
        let mut t = with_wobbly_picked(&rec);
        t.preview_smoothing(4.0, on(&rec));
        t.let_go();
        assert!(!t.smoothing_pending());
        assert_eq!(t.smoothing_shown(), 0.0);
        assert_eq!(t.commit_smoothing(on(&rec)), Commit::Nothing);
    }

    #[test]
    fn smoothing_with_nothing_picked_does_nothing() {
        let rec = wobbly_record();
        let mut t = PathTool::default();
        t.preview_smoothing(4.0, on(&rec));
        assert!(t.dragged().is_none());
        assert_eq!(t.commit_smoothing(on(&rec)), Commit::Nothing);
    }
}
