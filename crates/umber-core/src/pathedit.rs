//! The geometry a path editor needs, with no tool in it.
//!
//! Hit testing, splitting a cubic, removing a node and the rules for handles.
//! Every function takes a [`Path`] or a [`Node`] and hands back a new one, so
//! the tool above can build an undo entry from the before and the after and
//! this module needs no window to be tested.

use glam::Vec2;

use crate::vector::{Node, Path};

/// How far from opposite two handles may be, in degrees, and still count as
/// one smooth node. Hand-placed handles are never exactly opposite and a node
/// that mirrored only when they were would never mirror.
pub const SMOOTH_DEGREES: f32 = 1.0;

/// Steps `segment_at` samples a cubic at before refining.
const SAMPLES: usize = 32;
/// Rounds of ternary search on the two steps either side of the best sample.
const REFINE_ROUNDS: usize = 8;

/// Which handle of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    In,
    Out,
}

/// Where on a path a point landed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentHit {
    /// Index into `Path::segments()`, so the closing segment is last.
    pub segment: usize,
    pub t: f32,
    /// The point on the curve.
    pub at: Vec2,
    pub distance: f32,
}

/// Nearest node within `radius`, nearest first; ties to the lower index.
pub fn node_at(path: &Path, at: Vec2, radius: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, n) in path.nodes.iter().enumerate() {
        let d = n.at.distance(at);
        // Strictly nearer, so an exact tie keeps the lower index.
        if d <= radius && best.is_none_or(|(_, b)| d < b) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Which handle tip of `node` is within `radius`. A zero handle is never hit:
/// its tip is the node itself, and the node is the node's to answer for. Both
/// in reach: the nearer; a tie goes to `Out`.
pub fn handle_at(node: &Node, at: Vec2, radius: f32) -> Option<Side> {
    let reach = |h: Vec2| {
        if h == Vec2::ZERO {
            None
        } else {
            let d = (node.at + h).distance(at);
            (d <= radius).then_some(d)
        }
    };
    match (reach(node.handle_in), reach(node.handle_out)) {
        (None, None) => None,
        (Some(_), None) => Some(Side::In),
        (None, Some(_)) => Some(Side::Out),
        (Some(i), Some(o)) => Some(if i < o { Side::In } else { Side::Out }),
    }
}

fn eval(c: [Vec2; 4], t: f32) -> Vec2 {
    let u = 1.0 - t;
    c[0] * (u * u * u) + c[1] * (3.0 * u * u * t) + c[2] * (3.0 * u * t * t) + c[3] * (t * t * t)
}

/// Nearest point on the path's curve within `radius`. Culls a cubic whose
/// control-point box, grown by `radius`, misses `at`, before sampling it: one
/// legal path can be huge and this runs on every pointer move.
///
/// `segment` indexes `Path::segments()`, so the closing segment is last. Each
/// surviving cubic is sampled at 32 steps and the best `t` refined by ternary
/// search on its two neighbouring steps. Nothing is allocated.
pub fn segment_at(path: &Path, at: Vec2, radius: f32) -> Option<SegmentHit> {
    let mut best: Option<SegmentHit> = None;
    for (segment, c) in path.segments().enumerate() {
        let lo = c[0].min(c[1]).min(c[2]).min(c[3]) - Vec2::splat(radius);
        let hi = c[0].max(c[1]).max(c[2]).max(c[3]) + Vec2::splat(radius);
        if at.x < lo.x || at.y < lo.y || at.x > hi.x || at.y > hi.y {
            continue;
        }
        let step = 1.0 / SAMPLES as f32;
        let mut near = (0usize, f32::INFINITY);
        for i in 0..=SAMPLES {
            let d = eval(c, i as f32 * step).distance(at);
            if d < near.1 {
                near = (i, d);
            }
        }
        let mut a = (near.0.saturating_sub(1)) as f32 * step;
        let mut b = ((near.0 + 1).min(SAMPLES)) as f32 * step;
        for _ in 0..REFINE_ROUNDS {
            let m1 = a + (b - a) / 3.0;
            let m2 = b - (b - a) / 3.0;
            if eval(c, m1).distance(at) < eval(c, m2).distance(at) {
                b = m2;
            } else {
                a = m1;
            }
        }
        let t = 0.5 * (a + b);
        let p = eval(c, t);
        let distance = p.distance(at);
        // Refinement can only help, but the sample is the floor: never report
        // worse than a point already measured.
        let (t, p, distance) = if distance <= near.1 {
            (t, p, distance)
        } else {
            let t = near.0 as f32 * step;
            (t, eval(c, t), near.1)
        };
        if distance <= radius && best.is_none_or(|h| distance < h.distance) {
            best = Some(SegmentHit {
                segment,
                t,
                at: p,
                distance,
            });
        }
    }
    best
}

/// Split `segment` at `t` by de Casteljau. Returns the path and the new node's
/// index. The curve does not move. A straight segment, both handles zero, gives
/// a corner: de Casteljau would hand the new node handles along the line, which
/// are right and are a handle nobody drew.
///
/// Splitting the closing segment appends the node, since it sits between the
/// last node and the first. `None` for a segment off the end.
pub fn split(path: &Path, segment: usize, t: f32) -> Option<(Path, usize)> {
    let n = path.nodes.len();
    let c = path.segments().nth(segment)?;
    let a = *path.nodes.get(segment)?;
    let b = *path.nodes.get((segment + 1) % n)?;
    let t = t.clamp(0.0, 1.0);
    let straight = a.handle_out == Vec2::ZERO && b.handle_in == Vec2::ZERO;

    let p01 = c[0].lerp(c[1], t);
    let p12 = c[1].lerp(c[2], t);
    let p23 = c[2].lerp(c[3], t);
    let p012 = p01.lerp(p12, t);
    let p123 = p12.lerp(p23, t);
    let mid = p012.lerp(p123, t);

    let mut left = a;
    let mut right = b;
    // A straight segment is a plain lerp, exact where the three-stage form rounds.
    let mut new = Node::corner(if straight { a.at.lerp(b.at, t) } else { mid });
    new.pressure = a.pressure + (b.pressure - a.pressure) * t;
    if !straight {
        left.handle_out = p01 - a.at;
        right.handle_in = p23 - b.at;
        new.handle_in = p012 - mid;
        new.handle_out = p123 - mid;
    }

    let mut out = path.clone();
    out.nodes[segment] = left;
    let index = if segment + 1 < n {
        out.nodes[segment + 1] = right;
        out.nodes.insert(segment + 1, new);
        segment + 1
    } else {
        // The closing segment: `b` is node 0.
        out.nodes[0] = right;
        out.nodes.push(new);
        n
    };
    Some((out, index))
}

/// Remove node `i`, leaving its neighbours' handles as they were. `None` when
/// what is left is fewer than two nodes, or a closed path of fewer than three:
/// the path goes. Also `None` for an index off the end.
pub fn without_node(path: &Path, i: usize) -> Option<Path> {
    if i >= path.nodes.len() {
        return None;
    }
    let left = path.nodes.len() - 1;
    let least = if path.closed { 3 } else { 2 };
    if left < least {
        return None;
    }
    let mut out = path.clone();
    out.nodes.remove(i);
    Some(out)
}

/// Are the handles on a line, pointing opposite ways, within
/// [`SMOOTH_DEGREES`]? A node with either handle zero is a corner.
pub fn is_smooth(node: &Node) -> bool {
    let (Some(i), Some(o)) = (
        (-node.handle_in).try_normalize(),
        node.handle_out.try_normalize(),
    ) else {
        return false;
    };
    i.dot(o).clamp(-1.0, 1.0).acos().to_degrees() <= SMOOTH_DEGREES
}

/// Put one handle's tip at offset `to`. With `mirror` and a smooth node the
/// other handle turns to stay opposite, keeping its own length. Smoothness is
/// judged before the move, or dragging a handle would always be a cusp breaking.
pub fn with_handle(node: Node, side: Side, to: Vec2, mirror: bool) -> Node {
    let smooth = mirror && is_smooth(&node);
    let mut out = node;
    let other = match side {
        Side::In => {
            out.handle_in = to;
            &mut out.handle_out
        }
        Side::Out => {
            out.handle_out = to;
            &mut out.handle_in
        }
    };
    if smooth && let Some(dir) = to.try_normalize() {
        *other = -dir * other.length();
    }
    out
}

/// Both handles zero.
pub fn cornered(node: Node) -> Node {
    Node {
        handle_in: Vec2::ZERO,
        handle_out: Vec2::ZERO,
        ..node
    }
}

/// Handles along prev to next, each a third of the distance to its own
/// neighbour. `None` for an end node of an open path or a path under three
/// nodes.
pub fn smoothed(path: &Path, i: usize) -> Option<Node> {
    let n = path.nodes.len();
    if n < 3 || i >= n {
        return None;
    }
    let (prev, next) = if path.closed {
        ((i + n - 1) % n, (i + 1) % n)
    } else if i == 0 || i == n - 1 {
        return None;
    } else {
        (i - 1, i + 1)
    };
    let node = path.nodes[i];
    let prev = path.nodes[prev].at;
    let next = path.nodes[next].at;
    let dir = (next - prev).normalize_or_zero();
    Some(Node {
        handle_out: dir * (next.distance(node.at) / 3.0),
        handle_in: -dir * (prev.distance(node.at) / 3.0),
        ..node
    })
}

/// Every node carried by `by`; handles are offsets and do not move.
pub fn moved(path: &Path, by: Vec2) -> Path {
    let mut out = path.clone();
    for n in &mut out.nodes {
        n.at += by;
    }
    out
}

/// Node order reversed, each node's handles swapped. Same curve.
pub fn reversed(path: &Path) -> Path {
    let mut out = path.clone();
    out.nodes.reverse();
    for n in &mut out.nodes {
        std::mem::swap(&mut n.handle_in, &mut n.handle_out);
    }
    out
}

/// How closely a path is flattened before it is fitted again. Far finer than
/// any smoothing the fitter is asked for, so the refit answers to the curve
/// and not to the flattening.
const RESMOOTH_FLATTEN: f32 = 0.05;
/// The spacing the flattened line is resampled to, in document pixels. The
/// fitter's corner and tangent reaches are stated in cells of the trace grid,
/// about a pixel each, so this hands it the density it was written for: a
/// straight run flattens to its two ends, and two points do not say where a
/// corner four pixels along would be.
const RESMOOTH_STEP: f32 = 1.0;
/// The most points one refit is handed. A path many thousands of pixels long
/// is resampled more coarsely instead, which keeps the fitter's work bounded
/// on a control dragged a frame at a time.
const RESMOOTH_MAX_POINTS: usize = 1 << 18;

/// The same path fitted again with `tolerance` document pixels of slack, as
/// Trace outline fits a traced edge: corners where the line turns sharply,
/// as few smooth curves as stay within the tolerance between them. Role,
/// closure and ink are carried across.
///
/// **It can only take detail away.** The input is the path itself, not the
/// artwork it may have been traced from, so a tolerance tighter than the one
/// the path was made at gives back roughly the same path with more nodes,
/// never the edge it was smoothed off. A caller that lets somebody turn the
/// figure back down has to keep the path it started from and refit from that.
///
/// `None` where the fit leaves fewer than two nodes, which is less than a path.
pub fn resmoothed(path: &Path, tolerance: f32) -> Option<Path> {
    let mut line = Vec::new();
    path.flatten(RESMOOTH_FLATTEN, &mut line);
    if path.closed && line.len() > 1 && line.first() == line.last() {
        line.pop();
    }
    let points = resampled(&line, path.closed);
    let nodes = if path.closed {
        crate::trace::fit_closed(&points, tolerance)
    } else {
        crate::trace::fit_open(&points, tolerance)
    };
    // Two is enough for a closed path too: a circle fits as two smooth nodes,
    // which is what Trace outline lands for one.
    (nodes.len() >= 2).then(|| Path {
        role: path.role,
        closed: path.closed,
        nodes,
        ink: path.ink.clone(),
    })
}

/// `line` with points added so none is further than the step from the next,
/// every original vertex kept (a corner is a vertex, and losing one would
/// round it off before the fitter could see it).
fn resampled(line: &[Vec2], closed: bool) -> Vec<Vec2> {
    let edge = |i: usize| (line[i], line[(i + 1) % line.len()]);
    let edges = if closed {
        line.len()
    } else {
        line.len().saturating_sub(1)
    };
    let length: f32 = (0..edges).map(|i| edge(i).0.distance(edge(i).1)).sum();
    if !length.is_finite() {
        return line.to_vec();
    }
    let step = RESMOOTH_STEP.max(length / RESMOOTH_MAX_POINTS as f32);
    let mut out = Vec::with_capacity(line.len() + (length / step) as usize + 1);
    for i in 0..edges {
        let (a, b) = edge(i);
        out.push(a);
        let pieces = (a.distance(b) / step).ceil() as usize;
        for k in 1..pieces {
            out.push(a.lerp(b, k as f32 / pieces as f32));
        }
    }
    if !closed && let Some(&last) = line.last() {
        out.push(last);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::PathRole;

    const K: f32 = 0.552_284_7;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    fn path(points: &[(f32, f32)], closed: bool) -> Path {
        Path {
            role: PathRole::Cut,
            closed,
            nodes: points.iter().map(|&(x, y)| Node::corner(v(x, y))).collect(),
            ink: None,
        }
    }

    fn quarter() -> Path {
        let mut p = path(&[(100.0, 0.0), (0.0, 100.0)], false);
        p.nodes[0].handle_out = v(0.0, 100.0 * K);
        p.nodes[1].handle_in = v(100.0 * K, 0.0);
        p
    }

    fn polyline(p: &Path) -> Vec<Vec2> {
        let mut out = Vec::new();
        p.flatten(0.01, &mut out);
        out
    }

    /// Distance from `p` to the nearest point on the polyline `line`.
    fn off(line: &[Vec2], p: Vec2) -> f32 {
        line.windows(2)
            .map(|w| {
                let d = w[1] - w[0];
                let t = if d == Vec2::ZERO {
                    0.0
                } else {
                    ((p - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0)
                };
                (w[0] + d * t).distance(p)
            })
            .fold(f32::INFINITY, f32::min)
    }

    #[test]
    fn a_split_does_not_move_the_curve() {
        let q = quarter();
        let (s, i) = split(&q, 0, 0.3).unwrap();
        assert_eq!(s.nodes.len(), 3);
        assert_eq!(i, 1);
        let (a, b) = (polyline(&q), polyline(&s));
        assert!(b.iter().all(|&p| off(&a, p) < 0.02));
        assert!(a.iter().all(|&p| off(&b, p) < 0.02));
    }

    #[test]
    fn splitting_a_straight_segment_gives_a_corner() {
        let p = path(&[(0.0, 0.0), (10.0, 20.0)], false);
        let (s, i) = split(&p, 0, 0.25).unwrap();
        assert_eq!(s.nodes[i].at, v(2.5, 5.0));
        assert_eq!(s.nodes[i].handle_in, Vec2::ZERO);
        assert_eq!(s.nodes[i].handle_out, Vec2::ZERO);
    }

    #[test]
    fn splitting_the_closing_segment_appends() {
        let p = path(&[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)], true);
        let (s, i) = split(&p, 2, 0.5).unwrap();
        assert_eq!(i, 3);
        assert_eq!(s.nodes.len(), 4);
        assert_eq!(s.nodes[3].at, v(0.0, 5.0));
        assert!(split(&p, 3, 0.5).is_none());
    }

    #[test]
    fn a_node_is_hit_within_the_radius_and_the_nearer_wins() {
        let p = path(&[(0.0, 0.0), (4.0, 0.0), (50.0, 0.0)], false);
        assert_eq!(node_at(&p, v(3.0, 0.0), 5.0), Some(1));
        assert_eq!(node_at(&p, v(1.0, 0.0), 5.0), Some(0));
        assert_eq!(node_at(&p, v(2.0, 0.0), 5.0), Some(0), "a tie is the lower");
        assert_eq!(node_at(&p, v(20.0, 0.0), 5.0), None);
    }

    #[test]
    fn a_zero_handle_is_never_hit() {
        let n = Node::corner(v(5.0, 5.0));
        assert_eq!(handle_at(&n, v(5.0, 5.0), 10.0), None);
        let mut m = n;
        m.handle_out = v(10.0, 0.0);
        assert_eq!(handle_at(&m, v(15.0, 5.0), 2.0), Some(Side::Out));
        assert_eq!(handle_at(&m, v(5.0, 5.0), 2.0), None);
    }

    #[test]
    fn the_nearer_handle_wins_and_a_tie_is_out() {
        let mut n = Node::corner(Vec2::ZERO);
        n.handle_in = v(-10.0, 0.0);
        n.handle_out = v(10.0, 0.0);
        assert_eq!(handle_at(&n, v(-9.0, 0.0), 30.0), Some(Side::In));
        assert_eq!(handle_at(&n, v(9.0, 0.0), 30.0), Some(Side::Out));
        assert_eq!(handle_at(&n, v(0.0, 5.0), 30.0), Some(Side::Out));
    }

    #[test]
    fn a_segment_hit_lands_on_the_curve() {
        let q = quarter();
        let at = v(80.0, 80.0);
        let h = segment_at(&q, at, 20.0).unwrap();
        let expect = at.length() - 100.0;
        assert!((h.distance - expect.abs()).abs() < 0.5, "{}", h.distance);
        assert!(h.t > 0.4 && h.t < 0.6, "{}", h.t);
        assert_eq!(h.segment, 0);
        assert!((h.at.length() - 100.0).abs() < 0.5);
    }

    #[test]
    fn a_far_point_hits_no_segment() {
        assert!(segment_at(&quarter(), v(300.0, 300.0), 20.0).is_none());
        // Inside the culling box but beyond the radius.
        assert!(segment_at(&quarter(), v(20.0, 20.0), 5.0).is_none());
    }

    #[test]
    fn removing_a_node_keeps_the_neighbours_handles() {
        let mut p = path(&[(0.0, 0.0), (10.0, 0.0), (20.0, 0.0), (30.0, 5.0)], false);
        p.nodes[0].handle_out = v(1.0, 2.0);
        p.nodes[2].handle_in = v(-3.0, 4.0);
        let s = without_node(&p, 1).unwrap();
        assert_eq!(s.nodes.len(), 3);
        assert_eq!(s.nodes[0].handle_out, v(1.0, 2.0));
        assert_eq!(s.nodes[1].handle_in, v(-3.0, 4.0));
        assert!(without_node(&p, 9).is_none());
    }

    #[test]
    fn a_path_left_too_short_is_removed() {
        assert!(without_node(&path(&[(0.0, 0.0), (1.0, 0.0)], false), 0).is_none());
        let tri = path(&[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)], true);
        assert!(without_node(&tri, 0).is_none());
        let sq = path(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)], true);
        assert_eq!(without_node(&sq, 2).unwrap().nodes.len(), 3);
    }

    #[test]
    fn a_smooth_node_is_one_whose_handles_oppose() {
        let mut n = Node::corner(Vec2::ZERO);
        n.handle_out = v(10.0, 0.0);
        n.handle_in = v(-5.0, 0.0);
        assert!(is_smooth(&n));
        n.handle_in = v(-5.0, 0.2);
        assert!(!is_smooth(&n));
        n.handle_in = Vec2::ZERO;
        assert!(!is_smooth(&n));
    }

    #[test]
    fn a_mirrored_handle_keeps_the_other_ones_length() {
        let mut n = Node::corner(Vec2::ZERO);
        n.handle_in = v(-5.0, 0.0);
        n.handle_out = v(10.0, 0.0);
        let m = with_handle(n, Side::Out, v(0.0, 20.0), true);
        assert_eq!(m.handle_out, v(0.0, 20.0));
        assert!((m.handle_in - v(0.0, -5.0)).length() < 1e-4);
        let m = with_handle(n, Side::Out, v(0.0, 20.0), false);
        assert_eq!(m.handle_in, v(-5.0, 0.0));
        let mut cusp = n;
        cusp.handle_in = v(0.0, 5.0);
        let m = with_handle(cusp, Side::Out, v(0.0, 20.0), true);
        assert_eq!(m.handle_in, v(0.0, 5.0));
    }

    #[test]
    fn smoothing_points_handles_along_the_neighbours() {
        let p = path(&[(0.0, 0.0), (30.0, 30.0), (90.0, 30.0)], false);
        let n = smoothed(&p, 1).unwrap();
        let dir = v(90.0, 30.0).normalize();
        assert!((n.handle_out - dir * 20.0).length() < 1e-4);
        assert!((n.handle_in + dir * (30.0 * 2f32.sqrt() / 3.0)).length() < 1e-4);
        assert!(smoothed(&p, 0).is_none());
        assert!(smoothed(&p, 2).is_none());
        assert!(smoothed(&path(&[(0.0, 0.0), (1.0, 0.0)], true), 0).is_none());
        let closed = path(&[(0.0, 0.0), (30.0, 30.0), (90.0, 30.0)], true);
        assert!(smoothed(&closed, 0).is_some());
    }

    #[test]
    fn reversing_keeps_the_curve() {
        let q = quarter();
        let mut want = polyline(&q);
        want.reverse();
        let got = polyline(&reversed(&q));
        assert_eq!(got.len(), want.len());
        for (a, b) in got.iter().zip(&want) {
            assert!(a.distance(*b) < 1e-4);
        }
    }

    #[test]
    fn moving_carries_nodes_and_not_handles() {
        let q = quarter();
        let m = moved(&q, v(3.0, -2.0));
        assert_eq!(m.nodes[0].at, v(103.0, -2.0));
        assert_eq!(m.nodes[0].handle_out, q.nodes[0].handle_out);
        assert_eq!(cornered(m.nodes[0]).handle_out, Vec2::ZERO);
    }

    fn curved_triangle() -> Path {
        let mut p = path(&[(0.0, 0.0), (100.0, 0.0), (50.0, 80.0)], true);
        p.nodes[2].handle_out = v(-10.0, 40.0);
        p.nodes[0].handle_in = v(-30.0, 25.0);
        p.nodes[0].handle_out = v(15.0, -10.0);
        p.nodes[1].handle_in = v(-20.0, -15.0);
        p
    }

    #[test]
    fn a_curved_closing_split_does_not_move_the_curve() {
        let p = curved_triangle();
        let n = p.nodes.len();
        let (s, i) = split(&p, n - 1, 0.4).unwrap();
        assert_eq!(i, n);
        assert_eq!(s.nodes.len(), n + 1);
        let (a, b) = (polyline(&p), polyline(&s));
        assert!(b.iter().all(|&q| off(&a, q) < 0.02));
        assert!(a.iter().all(|&q| off(&b, q) < 0.02));
    }

    #[test]
    fn reversing_a_closed_curve_keeps_the_curve() {
        let p = curved_triangle();
        let (a, b) = (polyline(&p), polyline(&reversed(&p)));
        assert!(b.iter().all(|&q| off(&a, q) < 0.02));
        assert!(a.iter().all(|&q| off(&b, q) < 0.02));
    }

    #[test]
    fn a_hit_on_the_closing_edge_names_the_last_segment() {
        let p = path(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)], true);
        // Nearest the closing edge (100,100) -> (0,0).
        let h = segment_at(&p, v(40.0, 45.0), 10.0).unwrap();
        assert_eq!(h.segment, p.nodes.len() - 1);
    }

    #[test]
    fn smoothness_has_a_one_degree_boundary() {
        let node = |deg: f32| {
            let mut n = Node::corner(Vec2::ZERO);
            n.handle_out = v(10.0, 0.0);
            let r = deg.to_radians();
            n.handle_in = v(-r.cos(), -r.sin()) * 5.0;
            n
        };
        assert!(is_smooth(&node(0.9)));
        assert!(!is_smooth(&node(1.1)));
    }

    #[test]
    fn smoothing_a_closed_node_wraps_to_its_neighbours() {
        let p = path(&[(0.0, 0.0), (30.0, 30.0), (90.0, 30.0)], true);
        let n = smoothed(&p, 0).unwrap();
        // prev is (90,30), next is (30,30): the line runs along -x.
        let next = v(30.0, 30.0);
        let prev = v(90.0, 30.0);
        let dir = (next - prev).normalize();
        assert!((n.handle_out - dir * (next.length() / 3.0)).length() < 1e-4);
        assert!((n.handle_in + dir * (prev.length() / 3.0)).length() < 1e-4);
    }

    /// A circle of radius 100 drawn as many corner nodes, each pushed in or
    /// out by up to `wobble`: the kind of line a lasso hands Trace outline.
    fn wobbly_circle(wobble: f32) -> Path {
        let points: Vec<(f32, f32)> = (0..360)
            .map(|i| {
                let a = (i as f32).to_radians();
                let r = 100.0 + wobble * ((i * 7919) % 13) as f32 / 12.0 * 2.0 - wobble;
                (200.0 + r * a.cos(), 200.0 + r * a.sin())
            })
            .collect();
        path(&points, true)
    }

    #[test]
    fn resmoothing_a_wobbly_line_gives_fewer_nodes_close_to_it() {
        let before = wobbly_circle(1.0);
        let after = resmoothed(&before, 3.0).expect("a circle fits");
        assert!(after.closed);
        assert!(
            after.nodes.len() < 20,
            "{} nodes from {}",
            after.nodes.len(),
            before.nodes.len()
        );
        // Close to the line it came from: within the tolerance and the
        // flattening, measured from the new curve to the old one.
        let old = polyline(&before);
        let worst = polyline(&after)
            .into_iter()
            .map(|p| off(&old, p))
            .fold(0.0, f32::max);
        assert!(worst < 3.5, "strayed {worst} px");
    }

    #[test]
    fn more_smoothing_never_keeps_more_nodes_on_a_wobbly_line() {
        let before = wobbly_circle(2.0);
        let counts: Vec<usize> = [0.5, 1.0, 2.0, 4.0, 8.0]
            .iter()
            .map(|&t| resmoothed(&before, t).unwrap().nodes.len())
            .collect();
        assert!(counts.windows(2).all(|w| w[1] <= w[0]), "{counts:?}");
        assert!(counts[0] > counts[4], "{counts:?}");
    }

    #[test]
    fn resmoothing_keeps_a_squares_corners() {
        let square = path(
            &[(0.0, 0.0), (200.0, 0.0), (200.0, 200.0), (0.0, 200.0)],
            true,
        );
        let after = resmoothed(&square, 8.0).unwrap();
        for corner in [v(0.0, 0.0), v(200.0, 0.0), v(200.0, 200.0), v(0.0, 200.0)] {
            assert!(
                after.nodes.iter().any(|n| n.at.distance(corner) < 0.5),
                "lost the corner at {corner:?}: {:?}",
                after.nodes
            );
        }
    }

    #[test]
    fn resmoothing_an_open_line_keeps_its_ends_and_its_role() {
        let mut line = path(
            &[(0.0, 0.0), (50.0, 3.0), (100.0, -2.0), (150.0, 0.0)],
            false,
        );
        line.role = PathRole::Plot;
        let after = resmoothed(&line, 6.0).unwrap();
        assert!(!after.closed);
        assert_eq!(after.role, PathRole::Plot);
        assert_eq!(after.nodes.first().unwrap().at, v(0.0, 0.0));
        assert_eq!(after.nodes.last().unwrap().at, v(150.0, 0.0));
    }

    #[test]
    fn a_path_too_small_to_fit_is_refused_rather_than_shrunk() {
        let speck = path(&[(0.0, 0.0), (0.0, 0.0), (0.0, 0.0)], true);
        assert_eq!(resmoothed(&speck, 2.0), None);
    }
}
