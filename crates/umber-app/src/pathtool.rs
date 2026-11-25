//! The Path tool's state: drawing a path point by point.
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
//!   it ends a drag of handles, a closing press or a continuation. It is an
//!   enum because Edit mode's holds are more of the same kind.
//! - **An Edit press does nothing here yet.** Edit mode arrives in the next
//!   change, and until then a press that asks for it leaves the draft exactly
//!   as it was.

// Nothing calls the tool until the pointer is wired to it, and clippy runs
// with `-D warnings`. The wiring removes this line.
#![allow(dead_code)]

use glam::Vec2;
use umber_core::pathedit;
use umber_core::vector::{Node, Path, PathRole, VectorObject};

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

/// What a held press is doing.
#[derive(Clone, Copy, Debug)]
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
    held: Held,
    hover: Option<Vec2>,
    said: Option<&'static str>,
}

impl Default for PathTool {
    fn default() -> Self {
        Self {
            mode: PathMode::Draw,
            role: PathRole::Cut,
            draft: None,
            draft_role: None,
            base_len: 0,
            held: Held::Nothing,
            hover: None,
            said: None,
        }
    }
}

impl PathTool {
    pub fn draft(&self) -> Option<&Draft> {
        self.draft.as_ref()
    }

    pub fn hover(&self) -> Option<Vec2> {
        self.hover
    }

    /// The sentence to show until the next press, if any.
    pub fn said(&self) -> Option<&'static str> {
        self.said
    }

    pub fn is_held(&self) -> bool {
        match self.held {
            Held::Nothing => false,
            Held::Placing { .. } | Held::Closing | Held::Continuing => true,
        }
    }

    /// A press. `edit` is `mode == Edit || ctrl`.
    pub fn press(&mut self, at: Vec2, edit: bool, ground: Ground<'_>) {
        if edit {
            // Edit gestures on committed paths arrive in the next change. Until
            // then the press is not ours to act on, so the draft is untouched.
            return;
        }
        self.said = None;
        let hit = ground.reach.hit;

        let Some(draft) = self.draft.as_mut() else {
            if let Some((draft, role)) = continuation(ground, at, hit) {
                self.base_len = draft.nodes.len();
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
        draft.nodes.push(Node::corner(at));
        self.held = Held::Placing { press: at };
    }

    /// The pointer moved, held or not. `alone` is Shift.
    pub fn moved(&mut self, at: Vec2, _alone: bool) {
        // Shift has no meaning in Draw mode; Edit mode's moves are where it
        // will matter.
        self.hover = Some(at);
        if let (Held::Placing { .. }, Some(draft)) = (self.held, self.draft.as_mut())
            && let Some(node) = draft.nodes.last_mut()
        {
            let out = at - node.at;
            node.handle_out = out;
            node.handle_in = -out;
        }
    }

    pub fn release(&mut self, at: Vec2, ground: Ground<'_>) -> Commit {
        let held = std::mem::replace(&mut self.held, Held::Nothing);
        match held {
            Held::Nothing | Held::Continuing => Commit::Nothing,
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
            Held::Closing => self.land(),
        }
    }

    /// Enter.
    pub fn finish(&mut self) -> Commit {
        self.land()
    }

    /// Backspace, Delete or Undo with a draft up. True when it took something.
    pub fn pop(&mut self) -> bool {
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

    /// Escape. True when it consumed the key.
    pub fn abandon(&mut self) -> bool {
        self.held = Held::Nothing;
        self.said = None;
        self.draft.take().is_some()
    }

    /// Tool change, tab switch, close, resize, flip.
    pub fn settle(&mut self) -> Commit {
        self.held = Held::Nothing;
        let commit = self.land();
        if commit == Commit::Nothing {
            self.draft = None;
        }
        commit
    }

    /// The outcome of the last `Commit::Land`. Ok clears the draft; Err keeps
    /// it up and says why.
    pub fn landed(&mut self, outcome: Result<(), &'static str>) {
        match outcome {
            Ok(()) => {
                self.draft = None;
                self.said = None;
            }
            Err(why) => self.said = Some(why),
        }
    }

    /// The draft as a landing, where it is long enough to be a path. The draft
    /// itself stays put until `landed`.
    fn land(&self) -> Commit {
        let Some(draft) = self.draft.as_ref() else {
            return Commit::Nothing;
        };
        if draft.nodes.len() < 2 {
            return Commit::Nothing;
        }
        Commit::Land {
            paths: vec![Path {
                role: self.draft_role.unwrap_or(self.role),
                closed: draft.closed,
                nodes: draft.nodes.clone(),
                ink: None,
            }],
            replaces: draft.continues,
        }
    }
}

/// An open path with an end node under the press, as a draft that goes on at
/// that end. The last node wins where both ends are in reach, because that is
/// the end a path being drawn grows from.
fn continuation(ground: Ground<'_>, at: Vec2, hit: f32) -> Option<(Draft, PathRole)> {
    let record = ground.paths?;
    for (index, path) in record.paths().iter().enumerate() {
        if path.closed {
            continue;
        }
        let (Some(first), Some(last)) = (path.nodes.first(), path.nodes.last()) else {
            continue;
        };
        let nodes = if last.at.distance(at) <= hit {
            path.nodes.clone()
        } else if first.at.distance(at) <= hit {
            pathedit::reversed(path).nodes
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
    fn an_edit_press_changes_nothing_yet() {
        let mut t = PathTool::default();
        tap(&mut t, v(0.0, 0.0), bare());
        t.press(v(40.0, 0.0), true, bare());
        assert!(!t.is_held());
        assert_eq!(t.draft().unwrap().nodes.len(), 1);
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
}
