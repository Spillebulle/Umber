//! Cubic Bézier nodes fitted to polylines.
//!
//! The outline [`super::rings`] hands back is a polyline with a vertex every
//! cell or so; nobody can edit that. This turns it into a handful of nodes an
//! artist can, and [`fit_open`] is the same fitter for a run that does not
//! close, which a freehand drawing mode will call.
//!
//! - **Corners first, then curves.** A vertex where the outline turns by more
//!   than [`CORNER_DEGREES`] between the chords [`CORNER_REACH`] of arc either
//!   side of it is a corner, and only the sharpest within that reach survives.
//!   The reach is what lets marching squares' chamfered right angle still read
//!   as one corner, and what keeps a grown outline's rounded corner (radius at
//!   least the margin) from reading as one: over eight cells of arc a circle of
//!   radius ten turns by about 46 degrees.
//! - **Each run between corners is Schneider's algorithm**: chord-length
//!   parameters, least-squares handle lengths along fixed end tangents, up to
//!   [`NEWTON_PASSES`] of Newton reparameterisation when the fit is close, and
//!   a split at the worst point when it is still over tolerance. A split node takes one tangent
//!   for both sides, so it is smooth by construction; a corner takes whatever
//!   its two runs give it.
//! - **Error is measured against the input points**, as the distance from each
//!   point to the curve at that point's parameter. That over-reports the true
//!   distance to the curve, so a fit accepted here is at least as close as it
//!   says.
//! - **Every normalise has a fallback and every handle a bound.** Coincident
//!   points are merged before anything else, a tangent that cannot be measured
//!   falls back to the chord, and a least-squares handle that is not positive
//!   and no longer than the chord it spans is replaced by a third of the
//!   chord. Recursion stops at [`MAX_DEPTH`] whatever the error, so the work is
//!   at most the number of points times the depth.

use glam::Vec2;

use crate::vector::Node;

/// A turn sharper than this, over [`CORNER_REACH`] either side, is a corner.
pub const CORNER_DEGREES: f32 = 60.0;
/// How far along the outline, either side, a turn is measured over.
const CORNER_REACH: f32 = 4.0;
/// How far into a run its end tangent is measured to.
const TANGENT_REACH: f32 = 4.0;
/// How far into a run its end tangent is measured *from*. Skipping the first
/// cell is what keeps a chamfered corner from tilting the tangent of the edge
/// leaving it.
const TANGENT_SKIP: f32 = 1.0;
/// How far either side a split's tangent is measured over.
const CENTRE_REACH: f32 = 3.0;
/// The most Newton reparameterisation passes a fit is given before it is
/// split, Schneider's own figure. One pass leaves a grown block's rounded
/// rectangle at fourteen nodes and four bring it to ten: a split is a node an
/// artist has to edit, and a pass is a few multiplies per point.
const NEWTON_PASSES: usize = 4;
/// A fit further out than this many tolerances is split without being
/// reparameterised: it is not close enough for a nudge to bring it in.
const NEWTON_REACH: f32 = 4.0;
/// The deepest a run is split. Past it a fit is accepted as it stands.
const MAX_DEPTH: u32 = 40;
/// Points closer than this are one point.
const MIN_STEP: f32 = 1.0e-4;
/// The smallest tolerance asked for, so zero cannot split every run to pairs.
const MIN_TOLERANCE: f32 = 1.0e-3;

type Cubic = [Vec2; 4];

/// Cubic nodes through an open run of points, within `tolerance`.
///
/// The first and last node sit exactly on the first and last point, with no
/// handle on their outer side. Fewer than two distinct points gives no nodes,
/// and two give a straight line.
pub fn fit_open(points: &[Vec2], tolerance: f32) -> Vec<Node> {
    let mut pts = distinct(points, false);
    if pts.len() < 2 {
        return Vec::new();
    }
    if let Some(&last) = points.iter().rev().find(|p| p.is_finite()) {
        *pts.last_mut().expect("two points") = last;
    }
    if pts.len() == 2 {
        return vec![Node::corner(pts[0]), Node::corner(pts[1])];
    }
    let tolerance = sane(tolerance);
    let arc = arcs(&pts);
    let t1 = end_tangent(&pts, &arc, true);
    let t2 = end_tangent(&pts, &arc, false);
    let mut segs = Vec::new();
    fit_piece(&pts, t1, t2, tolerance, 0, &mut segs);
    open_nodes(&segs)
}

/// Cubic nodes round a closed ring, within `tolerance`, as the nodes of a
/// closed path. A ring of fewer than three distinct points gives no nodes.
pub fn fit_closed(ring: &[Vec2], tolerance: f32) -> Vec<Node> {
    let pts = distinct(ring, true);
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    let tolerance = sane(tolerance);
    let perimeter: f32 = (0..n).map(|i| pts[i].distance(pts[(i + 1) % n])).sum();
    if perimeter.is_nan() || perimeter <= MIN_STEP {
        return Vec::new();
    }
    let corners = corners(&pts, perimeter);
    let mut segs: Vec<Cubic> = Vec::new();
    match corners.len() {
        0 | 1 => {
            // One seam, at the corner if there is one and otherwise at vertex 0,
            // and a split halfway round. The handle bound would refuse one cubic
            // from a point back to itself anyway, since its chord is zero, but
            // then the split would land wherever the worst point was.
            let start = corners.first().copied().unwrap_or(0);
            let run: Vec<Vec2> = (0..=n).map(|k| pts[(start + k) % n]).collect();
            let arc = arcs(&run);
            let (t1, t2) = if corners.is_empty() {
                let t = ring_tangent(&pts, start);
                (t, -t)
            } else {
                (
                    end_tangent(&run, &arc, true),
                    end_tangent(&run, &arc, false),
                )
            };
            let total = arc[arc.len() - 1];
            let mid = index_at(&arc, total * 0.5).clamp(1, run.len() - 2);
            let tc = centred_tangent(&run, &arc, mid);
            fit_piece(&run[..=mid], t1, -tc, tolerance, 1, &mut segs);
            fit_piece(&run[mid..], tc, t2, tolerance, 1, &mut segs);
        }
        k => {
            for j in 0..k {
                let (a, b) = (corners[j], corners[(j + 1) % k]);
                let len = (b + n - a) % n;
                let run: Vec<Vec2> = (0..=len).map(|s| pts[(a + s) % n]).collect();
                let arc = arcs(&run);
                let t1 = end_tangent(&run, &arc, true);
                let t2 = end_tangent(&run, &arc, false);
                fit_piece(&run, t1, t2, tolerance, 0, &mut segs);
            }
        }
    }
    closed_nodes(&segs)
}

/// A usable tolerance: finite and above [`MIN_TOLERANCE`].
fn sane(tolerance: f32) -> f32 {
    if tolerance.is_finite() {
        tolerance.max(MIN_TOLERANCE)
    } else if tolerance == f32::INFINITY {
        f32::MAX
    } else {
        1.0
    }
}

/// The finite points, with each run of points closer than [`MIN_STEP`] merged
/// into its first. For a ring the last point is also dropped while it sits on
/// the first.
fn distinct(points: &[Vec2], closed: bool) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = Vec::with_capacity(points.len());
    for &p in points {
        if p.is_finite() && out.last().is_none_or(|q| q.distance(p) > MIN_STEP) {
            out.push(p);
        }
    }
    if closed {
        while out.len() > 1 && out[0].distance(out[out.len() - 1]) <= MIN_STEP {
            out.pop();
        }
    }
    out
}

/// Cumulative arc length along an open run, starting at zero.
fn arcs(pts: &[Vec2]) -> Vec<f32> {
    let mut out = Vec::with_capacity(pts.len());
    let mut s = 0.0f32;
    out.push(0.0);
    for w in pts.windows(2) {
        s += w[0].distance(w[1]);
        out.push(s);
    }
    out
}

/// The first index whose arc length is at least `target`, clamped to the run.
fn index_at(arc: &[f32], target: f32) -> usize {
    arc.partition_point(|&s| s < target).min(arc.len() - 1)
}

/// The last index whose arc length is at most `target`, clamped to the run.
fn index_before(arc: &[f32], target: f32) -> usize {
    arc.partition_point(|&s| s <= target).saturating_sub(1)
}

/// `v` normalised, or the first of `fallbacks` that normalises, or +x.
fn unit(v: Vec2, fallbacks: &[Vec2]) -> Vec2 {
    std::iter::once(v)
        .chain(fallbacks.iter().copied())
        .find_map(Vec2::try_normalize)
        .unwrap_or(Vec2::X)
}

/// The tangent at one end of an open run, pointing into the run: forward from
/// the start, backward from the end. It is the chord between the points
/// [`TANGENT_SKIP`] and [`TANGENT_REACH`] of arc in, shortened on a short run.
fn end_tangent(pts: &[Vec2], arc: &[f32], from_start: bool) -> Vec2 {
    let n = pts.len();
    let total = arc[n - 1];
    let reach = TANGENT_REACH.min(total / 3.0);
    let skip = TANGENT_SKIP.min(reach / 4.0);
    let (end, next, a, b) = if from_start {
        (
            pts[0],
            pts[1],
            pts[index_at(arc, skip)],
            pts[index_at(arc, reach)],
        )
    } else {
        (
            pts[n - 1],
            pts[n - 2],
            pts[index_before(arc, total - skip)],
            pts[index_before(arc, total - reach)],
        )
    };
    unit(b - a, &[b - end, next - end])
}

/// The tangent through an interior point of a run, forward along the run,
/// measured over [`CENTRE_REACH`] either side.
fn centred_tangent(pts: &[Vec2], arc: &[f32], k: usize) -> Vec2 {
    let n = pts.len();
    let before = pts[index_before(arc, arc[k] - CENTRE_REACH)];
    let after = pts[index_at(arc, arc[k] + CENTRE_REACH)];
    unit(
        after - before,
        &[
            pts[(k + 1).min(n - 1)] - pts[k.saturating_sub(1)],
            pts[n - 1] - pts[0],
        ],
    )
}

/// The index reached by walking from `i` round a ring in direction `step`
/// (1 or `n - 1`) until at least `reach` of arc is covered, or all of it.
fn ring_walk(pts: &[Vec2], i: usize, step: usize, reach: f32) -> usize {
    let n = pts.len();
    let mut j = i;
    let mut acc = 0.0;
    for _ in 1..n {
        let k = (j + step) % n;
        acc += pts[j].distance(pts[k]);
        j = k;
        if acc >= reach {
            break;
        }
    }
    j
}

/// The tangent through vertex `i` of a closed ring, forward.
fn ring_tangent(pts: &[Vec2], i: usize) -> Vec2 {
    let n = pts.len();
    let before = pts[ring_walk(pts, i, n - 1, CENTRE_REACH)];
    let after = pts[ring_walk(pts, i, 1, CENTRE_REACH)];
    unit(after - before, &[pts[(i + 1) % n] - pts[(i + n - 1) % n]])
}

/// The ring's corners, in ring order: vertices where the outline turns by more
/// than [`CORNER_DEGREES`] across [`CORNER_REACH`] either side, keeping the
/// sharpest within that reach of each other.
fn corners(pts: &[Vec2], perimeter: f32) -> Vec<usize> {
    let n = pts.len();
    let reach = CORNER_REACH.min(perimeter / 6.0);
    let mut turns: Vec<(f32, usize)> = Vec::new();
    for i in 0..n {
        let back = pts[ring_walk(pts, i, n - 1, reach)];
        let fwd = pts[ring_walk(pts, i, 1, reach)];
        let (Some(a), Some(b)) = (
            (pts[i] - back).try_normalize(),
            (fwd - pts[i]).try_normalize(),
        ) else {
            continue;
        };
        let turn = a.dot(b).clamp(-1.0, 1.0).acos().to_degrees();
        if turn > CORNER_DEGREES {
            turns.push((turn, i));
        }
    }
    turns.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut blocked = vec![false; n];
    let mut out = Vec::new();
    for (_, i) in turns {
        if blocked[i] {
            continue;
        }
        out.push(i);
        blocked[i] = true;
        for step in [1, n - 1] {
            let mut j = i;
            let mut acc = 0.0;
            for _ in 1..n {
                let k = (j + step) % n;
                acc += pts[j].distance(pts[k]);
                if acc >= reach {
                    break;
                }
                blocked[k] = true;
                j = k;
            }
        }
    }
    out.sort_unstable();
    out
}

/// Bernstein weights of a cubic at `t`.
fn bernstein(t: f32) -> [f32; 4] {
    let u = 1.0 - t;
    [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t]
}

fn at(c: &Cubic, t: f32) -> Vec2 {
    let b = bernstein(t);
    c[0] * b[0] + c[1] * b[1] + c[2] * b[2] + c[3] * b[3]
}

/// Fit one piece of a run, splitting it until every point is within
/// `tolerance` or [`MAX_DEPTH`] is reached, and append its cubics in order.
/// `t1` points into the piece from its start and `t2` into it from its end.
fn fit_piece(pts: &[Vec2], t1: Vec2, t2: Vec2, tolerance: f32, depth: u32, out: &mut Vec<Cubic>) {
    let n = pts.len();
    let (p0, p3) = (pts[0], pts[n - 1]);
    if n == 2 {
        let third = p0.distance(p3) / 3.0;
        out.push([p0, p0 + t1 * third, p3 + t2 * third, p3]);
        return;
    }
    let arc = arcs(pts);
    let total = arc[n - 1];
    let mut u: Vec<f32> = arc.iter().map(|s| s / total).collect();
    let mut cubic = generate(pts, &u, t1, t2);
    let (mut err, mut worst) = max_error(pts, &u, &cubic);
    for _ in 0..NEWTON_PASSES {
        if err <= tolerance || err > tolerance * NEWTON_REACH {
            break;
        }
        reparameterise(pts, &mut u, &cubic);
        let again = generate(pts, &u, t1, t2);
        let (e, w) = max_error(pts, &u, &again);
        if e < err {
            (cubic, err, worst) = (again, e, w);
        } else {
            break;
        }
    }
    if err <= tolerance || depth >= MAX_DEPTH {
        out.push(cubic);
        return;
    }
    let split = worst.clamp(1, n - 2);
    let tc = centred_tangent(pts, &arc, split);
    fit_piece(&pts[..=split], t1, -tc, tolerance, depth + 1, out);
    fit_piece(&pts[split..], tc, t2, tolerance, depth + 1, out);
}

/// The least-squares cubic through `pts` at parameters `u`, with its handles
/// along `t1` and `t2`.
fn generate(pts: &[Vec2], u: &[f32], t1: Vec2, t2: Vec2) -> Cubic {
    let n = pts.len();
    let (p0, p3) = (pts[0], pts[n - 1]);
    let chord = f64::from(p0.distance(p3));
    let (mut c00, mut c01, mut c11, mut x0, mut x1) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
    for (p, &t) in pts.iter().zip(u) {
        let b = bernstein(t);
        let a1 = t1 * b[1];
        let a2 = t2 * b[2];
        let rest = *p - (p0 * (b[0] + b[1]) + p3 * (b[2] + b[3]));
        c00 += f64::from(a1.dot(a1));
        c01 += f64::from(a1.dot(a2));
        c11 += f64::from(a2.dot(a2));
        x0 += f64::from(a1.dot(rest));
        x1 += f64::from(a2.dot(rest));
    }
    let det = c00 * c11 - c01 * c01;
    let third = chord / 3.0;
    let (mut l1, mut l2) = if det.abs() > 1.0e-12 * (c00 * c11).abs() && det != 0.0 {
        ((x0 * c11 - x1 * c01) / det, (c00 * x1 - c01 * x0) / det)
    } else {
        (third, third)
    };
    let fits = |l: f64| l.is_finite() && l > 1.0e-6 * chord && l <= chord;
    if !fits(l1) || !fits(l2) {
        (l1, l2) = (third, third);
    }
    [p0, p0 + t1 * l1 as f32, p3 + t2 * l2 as f32, p3]
}

/// The largest distance from a point to the curve at its parameter, and where.
fn max_error(pts: &[Vec2], u: &[f32], cubic: &Cubic) -> (f32, usize) {
    let mut worst = (0.0f32, pts.len() / 2);
    for (i, (p, &t)) in pts.iter().zip(u).enumerate().skip(1) {
        let d = at(cubic, t).distance(*p);
        if d.is_nan() || d > worst.0 {
            worst = (d, i);
        }
    }
    worst
}

/// One Newton step moving each parameter towards the nearest point of the
/// curve to its point. A step that cannot be taken leaves the parameter alone.
fn reparameterise(pts: &[Vec2], u: &mut [f32], c: &Cubic) {
    let d1 = [
        (c[1] - c[0]) * 3.0,
        (c[2] - c[1]) * 3.0,
        (c[3] - c[2]) * 3.0,
    ];
    let d2 = [(d1[1] - d1[0]) * 2.0, (d1[2] - d1[1]) * 2.0];
    let last = u.len() - 1;
    for (i, (p, t)) in pts.iter().zip(u.iter_mut()).enumerate() {
        if i == 0 || i == last {
            continue;
        }
        let s = 1.0 - *t;
        let q = at(c, *t) - *p;
        let q1 = d1[0] * (s * s) + d1[1] * (2.0 * s * *t) + d1[2] * (*t * *t);
        let q2 = d2[0] * s + d2[1] * *t;
        let den = q1.dot(q1) + q.dot(q2);
        let next = *t - q.dot(q1) / den;
        if den.abs() > f32::EPSILON && next.is_finite() {
            *t = next.clamp(0.0, 1.0);
        }
    }
}

fn node(at: Vec2, handle_in: Vec2, handle_out: Vec2) -> Node {
    Node {
        at,
        handle_in,
        handle_out,
        pressure: 1.0,
    }
}

/// The nodes of an open path through consecutive cubics.
fn open_nodes(segs: &[Cubic]) -> Vec<Node> {
    let mut out = Vec::with_capacity(segs.len() + 1);
    let mut handle_in = Vec2::ZERO;
    for c in segs {
        out.push(node(c[0], handle_in, c[1] - c[0]));
        handle_in = c[2] - c[3];
    }
    if let Some(c) = segs.last() {
        out.push(node(c[3], handle_in, Vec2::ZERO));
    }
    out
}

/// The nodes of a closed path through consecutive cubics, the last of which
/// ends where the first begins.
fn closed_nodes(segs: &[Cubic]) -> Vec<Node> {
    let m = segs.len();
    if m < 2 {
        return Vec::new();
    }
    (0..m)
        .map(|k| {
            let prev = &segs[(k + m - 1) % m];
            node(segs[k][0], prev[2] - prev[3], segs[k][1] - segs[k][0])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pathedit::is_smooth;
    use crate::vector::{Path, PathRole};
    use std::f32::consts::TAU;

    fn flat(nodes: &[Node], closed: bool) -> Vec<Vec2> {
        let path = Path {
            role: PathRole::Cut,
            closed,
            nodes: nodes.to_vec(),
            ink: None,
        };
        let mut out = Vec::new();
        path.flatten(0.1, &mut out);
        out
    }

    fn to_polyline(p: Vec2, poly: &[Vec2]) -> f32 {
        poly.windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                let ab = b - a;
                let t = if ab.length_squared() > 0.0 {
                    ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                p.distance(a + ab * t)
            })
            .fold(f32::INFINITY, f32::min)
    }

    fn circle(r: f32, n: usize) -> Vec<Vec2> {
        (0..n)
            .map(|i| {
                let t = i as f32 / n as f32 * TAU;
                Vec2::new(t.cos(), t.sin()) * r
            })
            .collect()
    }

    fn all_finite(nodes: &[Node]) -> bool {
        nodes
            .iter()
            .all(|n| n.at.is_finite() && n.handle_in.is_finite() && n.handle_out.is_finite())
    }

    #[test]
    fn a_circle_fits_in_a_handful_of_smooth_nodes() {
        let nodes = fit_closed(&circle(100.0, 628), 1.0);
        assert!((2..=12).contains(&nodes.len()), "{}", nodes.len());
        assert!(all_finite(&nodes));
        for n in &nodes {
            assert!(is_smooth(n), "{n:?}");
        }
        for q in flat(&nodes, true) {
            assert!((q.length() - 100.0).abs() <= 1.5, "{q}");
        }
        // No handle is longer than the chord its segment spans.
        let m = nodes.len();
        for k in 0..m {
            let next = &nodes[(k + 1) % m];
            let chord = nodes[k].at.distance(next.at);
            assert!(nodes[k].handle_out.length() <= chord + 1e-3);
            assert!(next.handle_in.length() <= chord + 1e-3);
        }
    }

    fn square() -> Vec<Vec2> {
        let mut pts = Vec::new();
        for i in 0..100 {
            pts.push(Vec2::new(i as f32, 0.0));
        }
        for i in 0..100 {
            pts.push(Vec2::new(100.0, i as f32));
        }
        for i in 0..100 {
            pts.push(Vec2::new(100.0 - i as f32, 100.0));
        }
        for i in 0..100 {
            pts.push(Vec2::new(0.0, 100.0 - i as f32));
        }
        // Start mid-edge, so the seam is not handed a corner for free.
        pts.rotate_left(50);
        pts
    }

    #[test]
    fn a_square_keeps_its_four_corners() {
        let nodes = fit_closed(&square(), 1.0);
        assert_eq!(nodes.len(), 4, "{nodes:?}");
        let corners = [
            Vec2::ZERO,
            Vec2::new(100.0, 0.0),
            Vec2::splat(100.0),
            Vec2::new(0.0, 100.0),
        ];
        for n in &nodes {
            assert!(corners.iter().any(|c| c.distance(n.at) <= 0.5), "{}", n.at);
            assert!(!is_smooth(n), "{n:?}");
        }
    }

    fn wobbly() -> Vec<Vec2> {
        (0..700)
            .map(|i| {
                let t = i as f32 / 700.0 * TAU;
                Vec2::new(t.cos(), t.sin()) * (80.0 + 10.0 * (5.0 * t).sin())
            })
            .collect()
    }

    #[test]
    fn a_fit_stays_within_its_tolerance() {
        let ring = wobbly();
        let mut last = usize::MAX;
        for tol in [0.5, 2.0, 8.0] {
            let nodes = fit_closed(&ring, tol);
            assert!(nodes.len() >= 2);
            assert!(all_finite(&nodes));
            let poly = flat(&nodes, true);
            for p in &ring {
                let d = to_polyline(*p, &poly);
                assert!(d <= tol + 0.5, "tolerance {tol}: {p} is {d} away");
            }
            assert!(
                nodes.len() <= last,
                "tolerance {tol}: {} nodes",
                nodes.len()
            );
            last = nodes.len();
        }
    }

    #[test]
    fn an_open_run_keeps_its_ends() {
        let mut pts: Vec<Vec2> = (0..=200)
            .map(|i| {
                let t = (i as f32).to_radians();
                Vec2::new(t.cos(), t.sin()) * 50.0 + Vec2::new(3.3, -7.1)
            })
            .collect();
        let nodes = fit_open(&pts, 1.0);
        assert!(nodes.len() >= 2);
        assert_eq!(nodes[0].at, pts[0]);
        assert_eq!(nodes[nodes.len() - 1].at, pts[pts.len() - 1]);
        assert_eq!(nodes[0].handle_in, Vec2::ZERO);
        assert_eq!(nodes[nodes.len() - 1].handle_out, Vec2::ZERO);
        // A last point a hair from the one before it is still the end.
        let end = pts[pts.len() - 1] + Vec2::splat(1.0e-5);
        pts.push(end);
        let nodes = fit_open(&pts, 1.0);
        assert_eq!(nodes[nodes.len() - 1].at, end);
        // Two points are a straight line.
        let line = fit_open(&[Vec2::ZERO, Vec2::new(5.0, 2.0)], 1.0);
        assert_eq!(line.len(), 2);
        assert_eq!(line[1].at, Vec2::new(5.0, 2.0));
    }

    #[test]
    fn no_handle_is_longer_than_the_chord_it_spans() {
        // A narrow U: up one arm, round the top, down the other. Its ends
        // are close and its tangents parallel, so least squares asks for
        // handles several times the chord, which is a loop an artist would
        // have to untie.
        let mut pts = Vec::new();
        for i in 0..=20 {
            pts.push(Vec2::new(0.0, 20.0 - i as f32));
        }
        for i in 1..20 {
            let t = i as f32 / 20.0 * std::f32::consts::PI;
            pts.push(Vec2::new(5.0 - 5.0 * t.cos(), -5.0 * t.sin()));
        }
        for i in 0..=20 {
            pts.push(Vec2::new(10.0, i as f32));
        }
        // Unbounded, this fits within 4 as one cubic with handles nearly
        // three times the chord.
        let nodes = fit_open(&pts, 4.0);
        assert!(all_finite(&nodes));
        for w in nodes.windows(2) {
            let chord = w[0].at.distance(w[1].at);
            assert!(w[0].handle_out.length() <= chord + 1e-3, "{w:?}");
            assert!(w[1].handle_in.length() <= chord + 1e-3, "{w:?}");
        }
    }

    #[test]
    fn a_ring_too_small_to_fit_is_dropped() {
        let a = Vec2::new(1.0, 2.0);
        let b = Vec2::new(4.0, 2.0);
        assert!(fit_closed(&[], 1.0).is_empty());
        assert!(fit_closed(&[a], 1.0).is_empty());
        assert!(fit_closed(&[a, b], 1.0).is_empty());
        assert!(fit_closed(&[a, a, b, b, a], 1.0).is_empty());
        assert!(fit_closed(&[a; 1000], 1.0).is_empty());
        assert!(fit_open(&[], 1.0).is_empty());
        assert!(fit_open(&[a; 50], 1.0).is_empty());
        assert!(fit_open(&[Vec2::NAN, a], 1.0).is_empty());
    }

    #[test]
    fn pathological_input_is_bounded_and_never_panics() {
        // A hundred thousand points of noise round a circle.
        let mut seed = 0x2545_f491_u32;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32 - 0.5
        };
        let ring: Vec<Vec2> = circle(1000.0, 100_000)
            .into_iter()
            .map(|p| p + Vec2::new(noise(), noise()) * 1.4)
            .collect();
        for tol in [0.0, 1.0, f32::NAN, f32::INFINITY] {
            let nodes = fit_closed(&ring, tol);
            assert!(nodes.len() >= 2, "tolerance {tol}");
            assert!(all_finite(&nodes), "tolerance {tol}");
            let open = fit_open(&ring, tol);
            assert!(all_finite(&open), "tolerance {tol}");
        }
        // Long stretches of repeated points between a few real ones.
        let mut stutter = Vec::new();
        for p in circle(30.0, 40) {
            stutter.extend(std::iter::repeat_n(p, 500));
        }
        let nodes = fit_closed(&stutter, 0.5);
        assert!(nodes.len() >= 2);
        assert!(all_finite(&nodes));
        // A spike that doubles back on itself.
        let spike = [
            Vec2::ZERO,
            Vec2::new(10.0, 0.0),
            Vec2::ZERO,
            Vec2::new(10.0, 0.0),
            Vec2::new(0.0, 0.1),
        ];
        assert!(all_finite(&fit_closed(&spike, 0.5)));
        assert!(all_finite(&fit_open(&spike, 0.5)));
    }
}
