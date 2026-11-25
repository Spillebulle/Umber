//! Where a vector layer's cut and plot paths land on screen, cached.
//!
//! The model half of the overlay `ui::vector_paths` paints, with no painting in
//! it — the division `dock.rs` keeps against `panels.rs`. What it decides is
//! which paths are drawn at all, how finely each is flattened, where every
//! point lands in screen points, and when all of that can be reused.
//!
//! **A still view costs no flattening.** The lines are rebuilt only when the
//! [`Key`] moves: the document, [`LayerStack::vector_revision`] (moved inside
//! every method that writes a record and every structural edit), which vector
//! layers are visible, the camera, the canvas region and the display scale.
//! Visibility is read afresh every frame rather than folded into the revision,
//! because a layer's eye is a public field the interface writes directly and
//! there is no method to move a revision from; reading it is one pass over at
//! most [`LayerStack::MAX`] entries and allocates nothing. An undo of a path
//! edit needs no repaint of its own for the same reason: it arrives on a key
//! press, which is already a frame, and that frame finds the revision moved.
//!
//! **The screen points are the marquee's**, mapped by the same camera call at
//! the same pivot and divided by the same scale, so a path made from a
//! rectangle selection lands on the marquee it was made from.
//! `the_lines_land_where_the_marquee_does` in `ui.rs` reads both.

use std::ops::Range;

use egui::{Pos2, Rect, pos2};
use glam::Vec2;
use umber_core::{Camera, LayerStack, PathRole};

use crate::session::DocId;

/// How far outside the canvas region a path's bounds may lie and still be
/// flattened, in points. A little more than the widest pass is drawn at, so a
/// path just off the edge is not cut off where its stroke would show.
const CULL_MARGIN: f32 = 8.0;

/// How far outside the canvas region an emitted point may lie, in points.
///
/// Clamped rather than dropped, so a path crossing the view keeps its shape
/// where it is seen. At [`Camera::MAX_ZOOM`] a path a million document pixels
/// away is sixty-four million screen pixels away, which `f32` holds but a
/// tessellator offsetting it by a pixel-wide stroke does not hold *well*; four
/// thousand points out, nothing a clamp bends can be seen.
const CLAMP_MARGIN: f32 = 4096.0;

/// The most points the overlay emits in one build, over every line.
///
/// A record is bounded by node count, not by what it flattens to: at the
/// highest zoom one cubic can ask for [`umber_core::vector`]'s 256 segments,
/// and a record may hold sixty-five thousand nodes. This keeps the worst build
/// to a few megabytes of points and the tessellation after it to a bounded
/// time, and stops there — the lines past it are simply not drawn, which is a
/// worse picture of a pathological record and never a stall.
pub const MAX_POINTS: usize = 262_144;

/// What the overlay was built against. Floats compared by their bits, because
/// "the same view" means exactly the same view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    doc: DocId,
    revision: u64,
    /// Bit `i` set where entry `i` is a vector layer and effectively visible.
    shown: u64,
    centre: [u32; 2],
    zoom: u32,
    view: [u32; 4],
    ppp: u32,
}

// The visibility mask holds one bit per stack entry, so the stack may not
// outgrow it. A compile error rather than a silent wrap, naming the reason.
const _: () = assert!(
    LayerStack::MAX <= 64,
    "the overlay's visibility mask is one u64 bit per stack entry"
);

/// One polyline of the overlay: a role, and the run of
/// [`VectorView`]'s shared points it occupies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenLine {
    pub role: PathRole,
    pub points: Range<usize>,
}

/// What [`VectorView::lines`] hands back: the lines, the one buffer of points
/// they index, and a number that moves whenever either was rebuilt, for a
/// cache built *from* them to key on.
#[derive(Clone, Copy, Debug)]
pub struct Lines<'a> {
    pub lines: &'a [ScreenLine],
    pub points: &'a [Pos2],
    pub generation: u64,
}

/// The overlay's cache. Lives on `Editor` above the `--- documents ---` line,
/// keyed by document, so a tab switch is simply a key that moved.
#[derive(Default)]
pub struct VectorView {
    key: Option<Key>,
    lines: Vec<ScreenLine>,
    points: Vec<Pos2>,
    /// The flattener's output in document pixels, reused per path.
    scratch: Vec<Vec2>,
    generation: u64,
    /// Whether the cap has been reported, so a record that meets it is said
    /// once rather than on every rebuild of a moving view.
    capped: bool,
    /// Paths flattened since the cache was made, for the reuse guard.
    #[cfg(test)]
    flattened: usize,
}

impl VectorView {
    /// The lines to draw for `stack`, in screen points, over the canvas region
    /// `view` at `ppp` physical pixels to the point.
    ///
    /// A stack with no vector layer returns at once, before the key is even
    /// built, so a document that never had one pays one pass over its entries.
    pub fn lines<'a>(
        &'a mut self,
        doc: DocId,
        stack: &LayerStack,
        camera: &Camera,
        view: Rect,
        ppp: f32,
    ) -> Lines<'a> {
        if !stack.has_vector_layers() {
            return Lines {
                lines: &[],
                points: &[],
                generation: self.generation,
            };
        }
        let key = Key {
            doc,
            revision: stack.vector_revision(),
            shown: shown(stack),
            centre: [camera.center.x.to_bits(), camera.center.y.to_bits()],
            zoom: camera.zoom.to_bits(),
            view: [
                view.min.x.to_bits(),
                view.min.y.to_bits(),
                view.max.x.to_bits(),
                view.max.y.to_bits(),
            ],
            ppp: ppp.to_bits(),
        };
        if self.key != Some(key) {
            self.rebuild(stack, camera, view, ppp, key.shown);
            self.key = Some(key);
            self.generation = self.generation.wrapping_add(1);
        }
        Lines {
            lines: &self.lines,
            points: &self.points,
            generation: self.generation,
        }
    }

    fn rebuild(&mut self, stack: &LayerStack, camera: &Camera, view: Rect, ppp: f32, shown: u64) {
        self.lines.clear();
        self.points.clear();
        let ppp = ppp.max(1e-3);
        let zoom = camera.zoom.max(1e-6);
        // A quarter of a *screen* pixel in document pixels: finer than that is
        // a polyline nobody can tell from the curve, coarser shows facets.
        let tolerance = 0.25 / zoom / ppp;
        // The marquee's pivot: this frame's canvas rect, in physical pixels.
        let pivot = Vec2::new(view.center().x, view.center().y) * ppp;
        let to_screen = |doc: Vec2| {
            let s = camera.doc_to_screen(doc, pivot);
            pos2(s.x / ppp, s.y / ppp)
        };
        let cull = view.expand(CULL_MARGIN);
        let clamp = view.expand(CLAMP_MARGIN);

        let Self {
            lines,
            points,
            scratch,
            capped,
            #[cfg(test)]
            flattened,
            ..
        } = self;
        for (i, layer) in stack.layers().iter().enumerate() {
            if shown & (1 << i) == 0 {
                continue;
            }
            // An opaque record answers no paths, so a record from a newer
            // Umber draws nothing here: its pixels are the layer's.
            let Some(record) = layer.vector() else {
                continue;
            };
            for path in record.paths() {
                match path.role {
                    PathRole::Cut | PathRole::Plot => {}
                    // Ink is pixels on the layer, drawn by the composite.
                    PathRole::Ink => continue,
                }
                let Some((lo, hi)) = path.bounds() else {
                    continue;
                };
                if !Rect::from_two_pos(to_screen(lo), to_screen(hi)).intersects(cull) {
                    continue;
                }
                scratch.clear();
                path.flatten(tolerance, scratch);
                #[cfg(test)]
                {
                    *flattened += 1;
                }
                let room = MAX_POINTS - points.len();
                let take = scratch.len().min(room);
                let start = points.len();
                points.extend(
                    scratch[..take]
                        .iter()
                        .map(|&p| to_screen(p).clamp(clamp.min, clamp.max)),
                );
                if take >= 2 {
                    lines.push(ScreenLine {
                        role: path.role,
                        points: start..points.len(),
                    });
                } else {
                    points.truncate(start);
                }
                if take < scratch.len() {
                    if !*capped {
                        log::debug!(
                            "the vector overlay stopped at {MAX_POINTS} points; \
                             lines past it are not drawn"
                        );
                        *capped = true;
                    }
                    return;
                }
            }
        }
    }

    /// How many paths have been flattened since this cache was made.
    #[cfg(test)]
    pub fn flattened(&self) -> usize {
        self.flattened
    }
}

/// The overlay tessellated into one mesh, and what it was built from.
///
/// **What makes a still frame allocate nothing.** egui wants a path's points
/// by value, so handing it the lines afresh would copy every point into a new
/// `Vec` twice a frame — once per pass — for as long as a vector layer is on
/// screen. A `Shape::Mesh` holds an `Arc`, so a frame whose key has not moved
/// hands egui a reference count. Data only: `ui::vector_paths` builds it, for
/// the reason the lines are built here and painted there.
#[derive(Default)]
pub struct Tessellated {
    pub key: Option<MeshKey>,
    pub mesh: std::sync::Arc<egui::epaint::Mesh>,
}

/// What a [`Tessellated`] mesh depends on beyond the lines: the scale it was
/// feathered and snapped at, and the three colours it was drawn in, which a
/// theme or accent change moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshKey {
    pub generation: u64,
    pub ppp: u32,
    pub colours: [egui::Color32; 3],
}

/// One bit per stack entry: set where the entry is a vector layer and its own
/// eye and every enclosing folder's are open.
fn shown(stack: &LayerStack) -> u64 {
    let mut bits = 0u64;
    for (i, layer) in stack.layers().iter().enumerate() {
        if layer.is_vector() && stack.effective_visible(i) {
            bits |= 1 << i;
        }
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use umber_core::{Node, Path, VectorObject};

    const VIEW: Rect = Rect {
        min: pos2(0.0, 0.0),
        max: pos2(400.0, 300.0),
    };

    fn doc() -> DocId {
        DocId::for_test(1)
    }

    /// Zoom 1, the document's (200, 150) at the centre of [`VIEW`], so a
    /// document point lands on the same screen point.
    fn camera() -> Camera {
        Camera {
            center: Vec2::new(200.0, 150.0),
            zoom: 1.0,
        }
    }

    fn line(role: PathRole, from: Vec2, to: Vec2) -> Path {
        Path {
            role,
            closed: false,
            nodes: vec![Node::corner(from), Node::corner(to)],
            ink: None,
        }
    }

    fn record(paths: Vec<Path>) -> VectorObject {
        VectorObject::empty().appended(paths).expect("legal")
    }

    /// A stack whose top layer holds one horizontal cut line across the view.
    fn stack_with_a_cut() -> (LayerStack, usize) {
        let mut s = LayerStack::new();
        let at = s
            .add_vector_layer(
                "Cut lines",
                record(vec![line(
                    PathRole::Cut,
                    Vec2::new(50.0, 100.0),
                    Vec2::new(350.0, 100.0),
                )]),
            )
            .expect("room");
        (s, at)
    }

    fn count(view: &mut VectorView, s: &LayerStack, cam: &Camera) -> usize {
        view.lines(doc(), s, cam, VIEW, 1.0).lines.len()
    }

    #[test]
    fn a_visible_vector_layer_yields_its_lines_and_a_hidden_one_none() {
        let (mut s, at) = stack_with_a_cut();
        let mut view = VectorView::default();
        let drawn = view.lines(doc(), &s, &camera(), VIEW, 1.0);
        assert_eq!(drawn.lines.len(), 1);
        assert_eq!(drawn.lines[0].role, PathRole::Cut);
        let run = drawn.lines[0].points.clone();
        assert_eq!(
            &drawn.points[run],
            &[pos2(50.0, 100.0), pos2(350.0, 100.0)],
            "at zoom 1 a document point lands on the same screen point"
        );

        s.get_mut(at).unwrap().visible = false;
        assert_eq!(count(&mut view, &s, &camera()), 0, "its own eye is shut");

        s.get_mut(at).unwrap().visible = true;
        let folder = s.group(&[at]).expect("grouped");
        assert_eq!(
            count(&mut view, &s, &camera()),
            1,
            "an open folder shows it"
        );
        s.get_mut(folder).unwrap().visible = false;
        assert_eq!(
            count(&mut view, &s, &camera()),
            0,
            "a shut folder hides what is inside it"
        );
    }

    /// Ink is the layer's pixels and a newer build's record is opaque; neither
    /// is a line over the canvas.
    #[test]
    fn ink_and_an_opaque_record_draw_nothing() {
        let mut s = LayerStack::new();
        s.add_vector_layer(
            "Ink",
            record(vec![line(
                PathRole::Ink,
                Vec2::new(50.0, 100.0),
                Vec2::new(350.0, 100.0),
            )]),
        )
        .expect("room");
        s.add_vector_layer("Newer", VectorObject::Opaque(b"{}".to_vec()))
            .expect("room");
        let mut view = VectorView::default();
        assert_eq!(count(&mut view, &s, &camera()), 0);
    }

    #[test]
    fn an_unchanged_view_reuses_the_cache() {
        let (s, _) = stack_with_a_cut();
        let mut view = VectorView::default();
        let first = view.lines(doc(), &s, &camera(), VIEW, 1.0).generation;
        assert_eq!(view.flattened(), 1);
        let second = view.lines(doc(), &s, &camera(), VIEW, 1.0).generation;
        assert_eq!(view.flattened(), 1, "a still view flattened again");
        assert_eq!(first, second, "a still view claimed to be new");
    }

    #[test]
    fn a_path_edit_or_a_camera_move_rebuilds_it() {
        let (mut s, at) = stack_with_a_cut();
        let mut view = VectorView::default();
        count(&mut view, &s, &camera());
        assert_eq!(view.flattened(), 1);

        // The edit, through the door the commands and undo both use.
        s.set_vector(
            at,
            record(vec![line(
                PathRole::Plot,
                Vec2::new(50.0, 120.0),
                Vec2::new(350.0, 120.0),
            )]),
        )
        .expect("legal");
        let drawn = view.lines(doc(), &s, &camera(), VIEW, 1.0);
        assert_eq!(
            drawn.lines[0].role,
            PathRole::Plot,
            "the old path is still drawn"
        );
        assert_eq!(view.flattened(), 2);

        let mut moved = camera();
        moved.center.x += 10.0;
        let drawn = view.lines(doc(), &s, &moved, VIEW, 1.0);
        assert_eq!(drawn.points[drawn.lines[0].points.start], pos2(40.0, 120.0));
        assert_eq!(view.flattened(), 3);

        let mut zoomed = moved;
        zoomed.zoom = 2.0;
        count(&mut view, &s, &zoomed);
        assert_eq!(view.flattened(), 4, "a zoom reused the old lines");

        count(&mut view, &s, &zoomed);
        assert_eq!(view.flattened(), 4);
        view.lines(DocId::for_test(2), &s, &zoomed, VIEW, 1.0);
        assert_eq!(
            view.flattened(),
            5,
            "another document reused this one's lines"
        );
        view.lines(DocId::for_test(2), &s, &zoomed, VIEW, 1.5);
        assert_eq!(
            view.flattened(),
            6,
            "a new display scale reused the old lines"
        );
        view.lines(
            DocId::for_test(2),
            &s,
            &zoomed,
            VIEW.translate(egui::vec2(5.0, 0.0)),
            1.5,
        );
        assert_eq!(
            view.flattened(),
            7,
            "a moved canvas region reused the old lines"
        );
    }

    #[test]
    fn a_path_off_screen_is_culled_and_a_huge_one_is_clamped() {
        let mut s = LayerStack::new();
        let at = s
            .add_vector_layer(
                "Far",
                record(vec![line(
                    PathRole::Cut,
                    Vec2::new(1.0e6, 1.0e6),
                    Vec2::new(1.0e6 - 10.0, 1.0e6 - 5.0),
                )]),
            )
            .expect("room");
        let cam = Camera {
            center: Vec2::new(200.0, 150.0),
            zoom: Camera::MAX_ZOOM,
        };
        let mut view = VectorView::default();
        assert_eq!(count(&mut view, &s, &cam), 0, "a path far away was drawn");
        assert_eq!(view.flattened(), 0, "a path far away was flattened");

        // One crossing the view from far off on both sides.
        s.set_vector(
            at,
            record(vec![line(
                PathRole::Cut,
                Vec2::new(-1.0e6, 150.0),
                Vec2::new(1.0e6, 150.0),
            )]),
        )
        .expect("legal");
        let drawn = view.lines(doc(), &s, &cam, VIEW, 1.0);
        assert_eq!(drawn.lines.len(), 1);
        let limit = VIEW.expand(CLAMP_MARGIN);
        for p in &drawn.points[drawn.lines[0].points.clone()] {
            assert!(p.x.is_finite() && p.y.is_finite(), "{p:?}");
            assert!(limit.contains(*p), "{p:?} is past the clamp");
        }
        assert_eq!(
            drawn.points[0].y, 150.0,
            "the clamp bent the line where it is seen"
        );
    }

    /// A record that flattens past [`MAX_POINTS`] stops there rather than
    /// building without bound.
    #[test]
    fn the_points_stop_at_the_cap() {
        // A zigzag of curves, each bowed by its handles, at the highest zoom,
        // where every cubic asks for close to two hundred segments.
        let nodes: Vec<Node> = (0..2_000)
            .map(|i| {
                let x = 199.0 + (i % 2) as f32 * 2.0;
                Node {
                    at: Vec2::new(x, 150.0),
                    handle_in: Vec2::new(0.0, -100.0),
                    handle_out: Vec2::new(0.0, 100.0),
                    pressure: 1.0,
                }
            })
            .collect();
        let mut s = LayerStack::new();
        s.add_vector_layer(
            "Busy",
            record(vec![Path {
                role: PathRole::Plot,
                closed: false,
                nodes,
                ink: None,
            }]),
        )
        .expect("room");
        let cam = Camera {
            center: Vec2::new(200.0, 150.0),
            zoom: Camera::MAX_ZOOM,
        };
        let mut view = VectorView::default();
        let drawn = view.lines(doc(), &s, &cam, VIEW, 1.0);
        assert_eq!(drawn.points.len(), MAX_POINTS);
        assert_eq!(drawn.lines.len(), 1);
    }

    #[test]
    fn a_stack_with_no_vector_layer_builds_nothing() {
        let s = LayerStack::new();
        let mut view = VectorView::default();
        assert_eq!(count(&mut view, &s, &camera()), 0);
        assert!(
            view.key.is_none(),
            "the cache was keyed for a plain document"
        );
    }
}
