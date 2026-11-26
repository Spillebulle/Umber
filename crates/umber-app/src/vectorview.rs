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
use umber_core::{Camera, LayerStack, Path, PathRole};

use crate::session::DocId;

/// How far outside the canvas region a line is kept, in points: what a
/// cubic's hull is culled against and what every segment is clipped to.
///
/// A little more than half the widest pass is drawn at, so a line clipped here
/// ends where its stroke can no longer be seen. **Clipped, never clamped.** A
/// point clamped per coordinate slides along each axis separately, which bends
/// a diagonal where it *is* seen and can join two far points into a chord
/// across the view that no path holds; a segment cut where it crosses the edge
/// keeps every point on the true line. It also keeps every coordinate small:
/// at [`Camera::MAX_ZOOM`] a path a million document pixels away is sixty-four
/// million points away, which `f32` holds and a tessellator offsetting it by a
/// pixel-wide stroke does not hold well.
const CULL_MARGIN: f32 = 8.0;

/// The flattener's scratch is shrunk back to this many points when a build has
/// left it larger. One cubic flattens to at most 256, so in practice it never
/// grows past this; the bound is what a test can assert.
pub const SCRATCH_KEEP: usize = 1024;

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
    /// The flattener's output in document pixels, reused per cubic.
    scratch: Vec<Vec2>,
    generation: u64,
    /// Whether the cap has been reported, so a record that meets it is said
    /// once rather than on every rebuild of a moving view.
    capped: bool,
    /// Cubics flattened since the cache was made, for the reuse and culling
    /// guards.
    #[cfg(test)]
    flattened: usize,
    /// The most points the scratch has held at once, for the guard that a
    /// build never holds more than a cubic: what it keeps *afterwards* is
    /// shrunk, so only the high-water mark can see a whole path flattened.
    #[cfg(test)]
    scratch_peak: usize,
}

impl VectorView {
    /// The lines to draw for `stack`, in screen points, over the canvas region
    /// `view` at `ppp` physical pixels to the point.
    ///
    /// A stack with no vector layer returns before the key is built, so a
    /// document that never had one pays one pass over at most
    /// [`LayerStack::MAX`] entries, asking each whether it holds a record.
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
        let tolerance = flatten_tolerance(camera, ppp);
        let to_screen = screen_mapping(camera, view, ppp);
        let keep = view.expand(CULL_MARGIN);

        let Self {
            lines,
            points,
            scratch,
            capped,
            #[cfg(test)]
            flattened,
            #[cfg(test)]
            scratch_peak,
            ..
        } = self;
        let mut runs = Runs {
            lines,
            points,
            role: PathRole::Cut,
            start: 0,
            pen: None,
        };
        'layers: for (i, layer) in stack.layers().iter().enumerate() {
            if !bit(shown, i) {
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
                let traced = trace_path(path, &mut runs, scratch, &to_screen, keep, tolerance);
                #[cfg(test)]
                {
                    *flattened += traced.flattened;
                    *scratch_peak = (*scratch_peak).max(traced.peak);
                }
                if traced.capped {
                    if !*capped {
                        log::debug!(
                            "the vector overlay stopped at {MAX_POINTS} points; \
                             lines past it are not drawn"
                        );
                        *capped = true;
                    }
                    break 'layers;
                }
            }
        }
        shrink_scratch(scratch);
    }

    /// The scratch buffer's capacity, for the guard that it stays bounded.
    #[cfg(test)]
    pub fn scratch_capacity(&self) -> usize {
        self.scratch.capacity()
    }

    /// How many cubics have been flattened since this cache was made.
    #[cfg(test)]
    pub fn flattened(&self) -> usize {
        self.flattened
    }
}

/// The flattener's tolerance in document pixels: `0.25 / zoom / ppp`.
///
/// The camera maps a document pixel to `zoom` device pixels, so `0.25 / zoom`
/// alone would be a quarter of a device pixel; the second division makes it a
/// quarter of a device pixel at 100% and finer than that at every larger scale
/// (an eighth at 200%). Finer than anybody can tell from the curve, at the cost
/// of a few more points on a high-density display; coarser shows facets.
fn flatten_tolerance(camera: &Camera, ppp: f32) -> f32 {
    0.25 / camera.zoom.max(1e-6) / ppp.max(1e-3)
}

/// Document pixels to screen points over the canvas region `view`, at `ppp`
/// physical pixels to the point: **the one statement of the overlay's mapping**,
/// the marquee's own (this frame's canvas rect, in physical pixels, as the
/// camera's pivot, then divided by the scale). The cut lines, the path in hand
/// and every mark on it call this and nothing else, so a mark cannot sit off
/// the line it belongs to.
pub fn screen_mapping(
    camera: &Camera,
    view: Rect,
    ppp: f32,
) -> impl Fn(Vec2) -> Pos2 + Copy + use<> {
    let ppp = ppp.max(1e-3);
    let camera = *camera;
    let pivot = Vec2::new(view.center().x, view.center().y) * ppp;
    move |doc: Vec2| {
        let s = camera.doc_to_screen(doc, pivot);
        pos2(s.x / ppp, s.y / ppp)
    }
}

/// The canvas region `view` as a box of document pixels, `(low, high)`: the
/// inverse of [`screen_mapping`] at two corners, which is enough because the
/// camera neither rotates nor shears. For culling a point in the document
/// before it is mapped at all.
pub fn view_in_document(camera: &Camera, view: Rect, ppp: f32) -> (Vec2, Vec2) {
    let ppp = ppp.max(1e-3);
    let pivot = Vec2::new(view.center().x, view.center().y) * ppp;
    let a = camera.screen_to_doc(Vec2::new(view.min.x, view.min.y) * ppp, pivot);
    let b = camera.screen_to_doc(Vec2::new(view.max.x, view.max.y) * ppp, pivot);
    (a.min(b), a.max(b))
}

/// What tracing one path did, for the caller to total.
struct Traced {
    /// The point budget ran out; the build ends here.
    capped: bool,
    /// Cubics flattened, for the culling guards.
    #[cfg(test)]
    flattened: usize,
    /// The most points the scratch held at once.
    #[cfg(test)]
    peak: usize,
}

/// One path's runs, culled and clipped to `keep`: the body both the cached
/// overlay and [`PathPreview`] share, so a path in hand is drawn by exactly the
/// arithmetic the same path will be once it has landed.
fn trace_path(
    path: &Path,
    runs: &mut Runs<'_>,
    scratch: &mut Vec<Vec2>,
    to_screen: &impl Fn(Vec2) -> Pos2,
    keep: Rect,
    tolerance: f32,
) -> Traced {
    #[cfg_attr(not(test), allow(unused_mut))]
    let mut traced = Traced {
        capped: false,
        #[cfg(test)]
        flattened: 0,
        #[cfg(test)]
        peak: 0,
    };
    // The whole path first, which is one box for a path that is nowhere near,
    // before its segments are looked at one by one.
    let Some((lo, hi)) = path.bounds() else {
        return traced;
    };
    if !Rect::from_two_pos(to_screen(lo), to_screen(hi)).intersects(keep) {
        return traced;
    }
    runs.begin(path.role);
    for cubic in path.segments() {
        // **Culled by its control-point hull before anything is spent on it.**
        // A cubic lies inside that hull, so a hull missing the region is a
        // curve missing it, and this is what keeps the budget for what can be
        // seen: flattening a whole path first and checking after was millions
        // of off-screen points for one legal path at the highest zoom.
        let hull = cubic.map(to_screen);
        if !Rect::from_points(&hull).intersects(keep) {
            runs.end();
            continue;
        }
        scratch.clear();
        umber_core::vector::flatten_cubic(cubic, tolerance, scratch);
        #[cfg(test)]
        {
            traced.flattened += 1;
            traced.peak = traced.peak.max(scratch.len());
        }
        let mut from = hull[0];
        for &doc in scratch.iter() {
            let to = to_screen(doc);
            if !runs.segment(from, to, keep) {
                runs.end();
                traced.capped = true;
                return traced;
            }
            from = to;
        }
    }
    runs.end();
    traced
}

/// Give back a scratch a build has left larger than [`SCRATCH_KEEP`].
fn shrink_scratch(scratch: &mut Vec<Vec2>) {
    if scratch.capacity() > SCRATCH_KEEP {
        scratch.clear();
        scratch.shrink_to(SCRATCH_KEEP);
    }
}

/// Screen-space runs for a handful of paths that are on no layer: the draft,
/// its rubber band, the dragged path, the picked path. Rebuilt on every call,
/// into buffers it keeps, so a warm frame allocates nothing here.
///
/// **Not cached, deliberately.** A draft moves with the pointer and the paths
/// are a few nodes; a key like [`VectorView`]'s would be a different number on
/// every frame this is used. The runs come from [`trace_path`], the overlay's
/// own body, so a path is clipped and culled exactly as it will be once it is
/// on a layer.
#[derive(Default)]
pub struct PathPreview {
    lines: Vec<ScreenLine>,
    points: Vec<Pos2>,
    scratch: Vec<Vec2>,
    generation: u64,
}

impl PathPreview {
    /// The lines of `paths`, in screen points over `view` at `ppp`. A path with
    /// the Ink role is skipped, as the overlay skips it.
    pub fn lines<'a>(
        &'a mut self,
        paths: &[&Path],
        camera: &Camera,
        view: Rect,
        ppp: f32,
    ) -> Lines<'a> {
        self.lines.clear();
        self.points.clear();
        self.generation = self.generation.wrapping_add(1);
        let tolerance = flatten_tolerance(camera, ppp);
        let to_screen = screen_mapping(camera, view, ppp);
        let keep = view.expand(CULL_MARGIN);
        let Self {
            lines,
            points,
            scratch,
            ..
        } = self;
        let mut runs = Runs {
            lines,
            points,
            role: PathRole::Cut,
            start: 0,
            pen: None,
        };
        for path in paths {
            match path.role {
                PathRole::Cut | PathRole::Plot => {}
                PathRole::Ink => continue,
            }
            if trace_path(path, &mut runs, scratch, &to_screen, keep, tolerance).capped {
                break;
            }
        }
        shrink_scratch(scratch);
        Lines {
            lines: &self.lines,
            points: &self.points,
            generation: self.generation,
        }
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
    /// The palette `colours` were derived from.
    pub palette: Option<crate::theme::Palette>,
    /// The underlay, cut and plot colours, derived once per palette.
    pub colours: [egui::Color32; 3],
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
///
/// An entry past the sixty-fourth has no bit and is a missing line, rather
/// than a shift overflow: a panic in a debug build, and in a release one a
/// wrapped mask that would read some *other* layer's eye. The `const` assert
/// above says that cannot happen today; this says what happens if it does.
fn shown(stack: &LayerStack) -> u64 {
    let mut bits = 0u64;
    for (i, layer) in stack.layers().iter().enumerate() {
        let Some(b) = mask_bit(i) else {
            break;
        };
        if layer.is_vector() && stack.effective_visible(i) {
            bits |= b;
        }
    }
    bits
}

/// Entry `i`'s bit in the visibility mask, or `None` past it.
fn mask_bit(i: usize) -> Option<u64> {
    u32::try_from(i).ok().and_then(|i| 1u64.checked_shl(i))
}

/// Is entry `i`'s bit set? False past the mask, for [`shown`]'s reason.
fn bit(mask: u64, i: usize) -> bool {
    mask_bit(i).is_some_and(|b| mask & b != 0)
}

/// The runs being written for one path: a path that leaves the kept region and
/// comes back is two lines, never one joined across the outside.
struct Runs<'a> {
    lines: &'a mut Vec<ScreenLine>,
    points: &'a mut Vec<Pos2>,
    role: PathRole,
    /// Where the open run's points begin.
    start: usize,
    /// The open run's last point, or `None` with no run open.
    pen: Option<Pos2>,
}

impl Runs<'_> {
    fn begin(&mut self, role: PathRole) {
        self.end();
        self.role = role;
    }

    /// Close the open run. A run of fewer than two points, or of points that
    /// are all one point, is dropped: there is nothing to see, and a
    /// zero-length polyline is not something to hand a tessellator.
    fn end(&mut self) {
        let run = &self.points[self.start..];
        if run.len() >= 2 && run.iter().any(|p| *p != run[0]) {
            self.lines.push(ScreenLine {
                role: self.role,
                points: self.start..self.points.len(),
            });
        } else {
            self.points.truncate(self.start);
        }
        self.start = self.points.len();
        self.pen = None;
    }

    /// Add the part of `a` to `b` inside `keep`. False when the point budget is
    /// spent, which ends the build.
    fn segment(&mut self, a: Pos2, b: Pos2, keep: Rect) -> bool {
        let Some((a2, b2)) = clip(a, b, keep) else {
            self.end();
            return true;
        };
        if self.pen != Some(a2) {
            self.end();
            if !self.push(a2) {
                return false;
            }
        }
        if !self.push(b2) {
            return false;
        }
        if b2 != b {
            // It left the region here; whatever comes back in is a new run.
            self.end();
        }
        true
    }

    fn push(&mut self, p: Pos2) -> bool {
        if self.points.len() >= MAX_POINTS {
            return false;
        }
        self.points.push(p);
        self.pen = Some(p);
        true
    }
}

/// The part of `a` to `b` inside `r`, by Liang-Barsky, or `None` where it
/// misses.
///
/// In `f64`, because an end can be tens of millions of points away and the
/// parameter that brings it to the edge is then tiny. An end already inside is
/// handed back **unchanged**, bit for bit, which is what lets consecutive
/// segments of one run meet exactly and what keeps an on-screen path's points
/// the marquee's.
fn clip(a: Pos2, b: Pos2, r: Rect) -> Option<(Pos2, Pos2)> {
    let (ax, ay) = (f64::from(a.x), f64::from(a.y));
    let (dx, dy) = (f64::from(b.x) - ax, f64::from(b.y) - ay);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, ax - f64::from(r.min.x)),
        (dx, f64::from(r.max.x) - ax),
        (-dy, ay - f64::from(r.min.y)),
        (dy, f64::from(r.max.y) - ay),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    let at = |t: f64| pos2((ax + dx * t) as f32, (ay + dy * t) as f32);
    let a2 = if t0 == 0.0 { a } else { at(t0) };
    let b2 = if t1 == 1.0 { b } else { at(t1) };
    Some((a2, b2))
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
    fn a_path_off_screen_is_culled_and_a_huge_one_is_clipped() {
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
        let keep = VIEW.expand(CULL_MARGIN);
        for p in &drawn.points[drawn.lines[0].points.clone()] {
            assert!(keep.contains(*p), "{p:?} is outside the kept region");
            assert_eq!(p.y, 150.0, "the clip bent the line where it is seen");
        }
    }

    /// Camera centred on the document origin at zoom 10, so a document point
    /// `d` lands at `d * 10 + (200, 150)` and a screen point is easy to place.
    fn origin_camera() -> Camera {
        Camera {
            center: Vec2::ZERO,
            zoom: 10.0,
        }
    }

    fn to_doc(screen: (f64, f64)) -> Vec2 {
        Vec2::new(
            ((screen.0 - 200.0) / 10.0) as f32,
            ((screen.1 - 150.0) / 10.0) as f32,
        )
    }

    /// **A diagonal with one end tens of millions of points away is drawn on
    /// its true line.** Every emitted point is measured against the line
    /// through the two ends, in `f64`. A per-coordinate clamp pulls the far end
    /// in along each axis separately, which turns the line where it is seen.
    #[test]
    fn a_far_diagonal_stays_on_its_true_line() {
        let near = Vec2::new(5.0, 3.0);
        let far = Vec2::new(-999_000.0, -310_000.0);
        let mut s = LayerStack::new();
        s.add_vector_layer("Diagonal", record(vec![line(PathRole::Cut, far, near)]))
            .expect("room");
        let mut view = VectorView::default();
        let drawn = view.lines(doc(), &s, &origin_camera(), VIEW, 1.0);
        assert_eq!(drawn.lines.len(), 1);
        let screen = |d: Vec2| (f64::from(d.x) * 10.0 + 200.0, f64::from(d.y) * 10.0 + 150.0);
        let (a, b) = (screen(far), screen(near));
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        for p in &drawn.points[drawn.lines[0].points.clone()] {
            let off = ((f64::from(p.x) - a.0) * dy - (f64::from(p.y) - a.1) * dx).abs() / len;
            assert!(off < 0.01, "{p:?} is {off} points off the true line");
        }
    }

    /// Two ends far off on opposite sides whose true segment passes below the
    /// view draw nothing. Clamped per coordinate, both ends land in opposite
    /// corners of a band round the view and the chord between them crosses it.
    #[test]
    fn two_far_ends_whose_segment_misses_the_view_draw_nothing() {
        // y = 0.5 x + 1000 on screen: at x in 0..400 it is below 1000.
        let a = to_doc((-9.99e6, -9.99e6 * 0.5 + 1000.0));
        let b = to_doc((9.99e6, 9.99e6 * 0.5 + 1000.0));
        let mut s = LayerStack::new();
        s.add_vector_layer("Below", record(vec![line(PathRole::Plot, a, b)]))
            .expect("room");
        let mut view = VectorView::default();
        assert_eq!(count(&mut view, &s, &origin_camera()), 0);
    }

    /// A path that goes out of the view and comes back is two lines, not one
    /// joined across the outside.
    #[test]
    fn a_path_that_leaves_and_comes_back_is_two_runs() {
        let mut s = LayerStack::new();
        s.add_vector_layer(
            "Loop",
            record(vec![Path {
                role: PathRole::Cut,
                closed: false,
                nodes: [
                    (100.0, 100.0),
                    (100.0, 5000.0),
                    (300.0, 5000.0),
                    (300.0, 100.0),
                ]
                .into_iter()
                .map(|(x, y)| Node::corner(Vec2::new(x, y)))
                .collect(),
                ink: None,
            }]),
        )
        .expect("room");
        let mut view = VectorView::default();
        let drawn = view.lines(doc(), &s, &camera(), VIEW, 1.0);
        let edge = VIEW.max.y + CULL_MARGIN;
        let runs: Vec<&[Pos2]> = drawn
            .lines
            .iter()
            .map(|l| &drawn.points[l.points.clone()])
            .collect();
        assert_eq!(
            runs,
            vec![
                &[pos2(100.0, 100.0), pos2(100.0, edge)][..],
                &[pos2(300.0, edge), pos2(300.0, 100.0)][..],
            ]
        );
    }

    /// A bowed cubic: from `a` to `b` with both handles pulled `bow` along y.
    fn bowed(a: Vec2, b: Vec2, bow: f32) -> [Node; 2] {
        [
            Node {
                at: a,
                handle_in: Vec2::ZERO,
                handle_out: Vec2::new(0.0, bow),
                pressure: 1.0,
            },
            Node {
                at: b,
                handle_in: Vec2::new(0.0, -bow),
                handle_out: Vec2::ZERO,
                pressure: 1.0,
            },
        ]
    }

    /// Of a path's many cubics, only the one whose hull reaches the view is
    /// flattened. Counted per cubic, so a rebuild that flattened the whole
    /// path and threw most of it away reads as many.
    #[test]
    fn only_the_cubic_that_crosses_the_view_is_flattened() {
        let mut nodes = Vec::new();
        // Far off to the left, bowed so each would cost something to flatten.
        for k in 0..20 {
            let y = k as f32 * 40.0;
            let [a, _] = bowed(Vec2::new(-5000.0, y), Vec2::new(-5000.0, y + 40.0), 30.0);
            nodes.push(a);
        }
        // Into the view and across it, then far off to the right.
        let [a, b] = bowed(Vec2::new(-5000.0, 800.0), Vec2::new(5000.0, -200.0), 0.0);
        nodes.push(a);
        nodes.push(b);
        for k in 0..20 {
            let [a, _] = bowed(Vec2::new(5000.0, k as f32 * 40.0), Vec2::ZERO, 30.0);
            nodes.push(a);
        }
        let mut s = LayerStack::new();
        s.add_vector_layer(
            "Mostly far",
            record(vec![Path {
                role: PathRole::Cut,
                closed: false,
                nodes,
                ink: None,
            }]),
        )
        .expect("room");
        let mut view = VectorView::default();
        let drawn = view.lines(doc(), &s, &camera(), VIEW, 1.0);
        assert_eq!(drawn.lines.len(), 1);
        assert_eq!(
            view.flattened(),
            1,
            "cubics nowhere near the view were flattened"
        );
    }

    /// The point budget goes to what can be seen. A path whose first twelve
    /// hundred cubics are just off screen — enough, flattened, to spend the
    /// whole budget twice over at this zoom — still draws the segment that
    /// comes into view after them.
    #[test]
    fn the_budget_is_spent_on_what_can_be_seen() {
        let mut nodes: Vec<Node> = (0..1_200)
            .map(|i| Node {
                at: Vec2::new(300.0 + (i % 2) as f32 * 2.0, 150.0),
                handle_in: Vec2::new(0.0, -100.0),
                handle_out: Vec2::new(0.0, 100.0),
                pressure: 1.0,
            })
            .collect();
        nodes.push(Node::corner(Vec2::new(200.0, 150.0)));
        let mut s = LayerStack::new();
        s.add_vector_layer(
            "Off to one side",
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
        assert!(
            drawn.points.iter().any(|p| VIEW.contains(*p)),
            "the visible segment was not drawn"
        );
        assert!(drawn.points.len() < MAX_POINTS);
    }

    /// A record that flattens past [`MAX_POINTS`] stops there, and building it
    /// holds no more than one cubic's worth of scratch.
    ///
    /// The largest record there is — [`umber_core::vector::MAX_NODES`] nodes of
    /// one bowed zigzag that stays inside the view — at the highest zoom. A
    /// rebuild that flattened the whole path before looking would hold about
    /// two million points here, on every frame of a pan.
    #[test]
    fn the_points_stop_at_the_cap_and_the_scratch_stays_small() {
        let nodes: Vec<Node> = (0..umber_core::vector::MAX_NODES)
            .map(|i| Node {
                at: Vec2::new(199.0 + (i % 2) as f32 * 2.0, 150.0),
                handle_in: Vec2::new(0.0, -2.0),
                handle_out: Vec2::new(0.0, 2.0),
                pressure: 1.0,
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
        assert!(
            drawn.points.iter().all(|p| VIEW.contains(*p)),
            "the zigzag was meant to stay in view"
        );
        assert!(
            view.scratch_peak <= 256, // one cubic flattens to at most 256 points
            "the scratch held {} points at once",
            view.scratch_peak
        );
        assert!(
            view.scratch_capacity() <= SCRATCH_KEEP,
            "the scratch keeps {} points",
            view.scratch_capacity()
        );

        // And a scratch something grew is given back after the next build.
        view.scratch.reserve(100_000);
        let moved = Camera {
            center: Vec2::new(200.5, 150.0),
            ..cam
        };
        view.lines(doc(), &s, &moved, VIEW, 1.0);
        assert!(view.scratch_capacity() <= SCRATCH_KEEP);
    }

    /// A path whose nodes are all one point is nothing to draw, and is never
    /// handed on as a zero-length line.
    #[test]
    fn a_path_of_one_repeated_point_draws_nothing() {
        let mut s = LayerStack::new();
        let p = Vec2::new(100.0, 100.0);
        s.add_vector_layer("Dot", record(vec![line(PathRole::Cut, p, p)]))
            .expect("room");
        let mut view = VectorView::default();
        assert_eq!(count(&mut view, &s, &camera()), 0);
    }

    /// The visibility mask answers "no" past its sixty-four bits rather than
    /// overflowing the shift.
    #[test]
    fn an_entry_past_the_mask_is_not_shown() {
        assert!(bit(u64::MAX, 63));
        assert!(!bit(u64::MAX, 64));
        assert!(!bit(u64::MAX, usize::MAX));
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

    #[test]
    fn a_preview_of_a_straight_path_is_its_two_screen_points() {
        let path = line(
            PathRole::Cut,
            Vec2::new(50.0, 100.0),
            Vec2::new(350.0, 120.0),
        );
        let mut preview = PathPreview::default();
        let drawn = preview.lines(&[&path], &camera(), VIEW, 1.0);
        assert_eq!(drawn.lines.len(), 1);
        assert_eq!(
            &drawn.points[drawn.lines[0].points.clone()],
            &[pos2(50.0, 100.0), pos2(350.0, 120.0)]
        );
        // The mapping the marks use is the one that put them there.
        let to_screen = screen_mapping(&camera(), VIEW, 1.0);
        assert_eq!(to_screen(Vec2::new(50.0, 100.0)), pos2(50.0, 100.0));
        let none = preview.lines(&[], &camera(), VIEW, 1.0);
        assert!(none.lines.is_empty() && none.points.is_empty());
    }

    #[test]
    fn a_preview_is_culled_and_clipped_like_the_overlay() {
        let mut preview = PathPreview::default();
        let far = line(
            PathRole::Cut,
            Vec2::new(1.0e6, 1.0e6),
            Vec2::new(1.0e6 - 10.0, 1.0e6),
        );
        assert!(
            preview
                .lines(&[&far], &camera(), VIEW, 1.0)
                .lines
                .is_empty(),
            "a path off screen was drawn"
        );
        let wide = line(
            PathRole::Plot,
            Vec2::new(-1.0e6, 150.0),
            Vec2::new(1.0e6, 150.0),
        );
        let drawn = preview.lines(&[&far, &wide], &camera(), VIEW, 1.0);
        assert_eq!(drawn.lines.len(), 1);
        assert_eq!(drawn.lines[0].role, PathRole::Plot);
        let keep = VIEW.expand(CULL_MARGIN);
        for p in &drawn.points[drawn.lines[0].points.clone()] {
            assert!(keep.contains(*p), "{p:?} is outside the kept region");
            assert_eq!(p.y, 150.0);
        }
        // Ink is pixels, not a line.
        let ink = line(
            PathRole::Ink,
            Vec2::new(50.0, 100.0),
            Vec2::new(350.0, 100.0),
        );
        assert!(
            preview
                .lines(&[&ink], &camera(), VIEW, 1.0)
                .lines
                .is_empty()
        );
    }

    #[test]
    fn a_preview_keeps_its_buffers() {
        let path = Path {
            role: PathRole::Cut,
            closed: false,
            nodes: vec![
                Node {
                    at: Vec2::new(50.0, 100.0),
                    handle_in: Vec2::ZERO,
                    handle_out: Vec2::new(80.0, -60.0),
                    pressure: 1.0,
                },
                Node::corner(Vec2::new(350.0, 100.0)),
            ],
            ink: None,
        };
        let mut preview = PathPreview::default();
        preview.lines(&[&path], &camera(), VIEW, 1.0);
        let caps = (
            preview.lines.capacity(),
            preview.points.capacity(),
            preview.scratch.capacity(),
        );
        assert!(caps.0 > 0 && caps.1 > 0 && caps.2 > 0);
        preview.lines(&[&path], &camera(), VIEW, 1.0);
        assert_eq!(
            caps,
            (
                preview.lines.capacity(),
                preview.points.capacity(),
                preview.scratch.capacity()
            ),
            "a second frame grew a buffer"
        );
    }
}
