//! Tracing artwork into outlines.
//!
//! This is the pure-CPU half that turns a grid of alpha coverage into closed
//! polylines; fitting curves to them is a later step. Nothing here knows about
//! a GPU or a window.
//!
//! - **Inside is a threshold, not a halfway point.** A cell at or above
//!   [`THRESHOLD`] counts. Anything fainter is not printed strongly enough to
//!   matter, and soft brush edges would otherwise push the line outwards by
//!   their whole falloff.
//! - **The contoured field is symmetric about the edge.** Every cell that is
//!   not inside is a flat -1 and an inside cell runs from just above 0 (at the
//!   threshold) up to 1 (opaque). A binary edge therefore lands exactly on the
//!   pixel boundary, halfway between an opaque centre and an empty one, and a
//!   partly covered cell moves the line towards its own centre. Contouring
//!   `alpha - THRESHOLD` instead would put a hard edge 0.4 of a cell outside
//!   the artwork, which reads as a margin nobody asked for.
//! - **A grown outline is measured to the cell's edge, not its centre.** The
//!   distance transform is between cell centres, so the field is
//!   `margin + 0.5 - distance`, which puts a straight edge exactly `margin`
//!   cells beyond the artwork and rounds its corners.
//! - **Holes are decided on the field, after growing.** A gap that the margin
//!   closes is not a hole any more, and one it does not close is filled only
//!   when it cannot reach the border.
//! - **The grid is padded before contouring**, so artwork touching its edge
//!   still closes, and the padding is subtracted again on the way out.

mod fit;

use std::collections::VecDeque;

use glam::{UVec2, Vec2};

pub use fit::{CORNER_DEGREES, fit_closed, fit_open};

/// Alpha at or above this is inside. About 10%.
pub const THRESHOLD: u8 = 26;
/// A connected piece of inside smaller than this many cells is removed.
pub const MIN_SPECK: usize = 4;
/// The largest margin, in cells, that [`rings`] will honour.
pub const MAX_MARGIN: f32 = 512.0;

/// One byte of alpha per cell, row-major, `size.x * size.y` long.
pub struct Coverage<'a> {
    pub size: UVec2,
    pub alpha: &'a [u8],
}

/// A cell with no inside cell anywhere: effectively infinite distance.
const FAR: f32 = 1.0e20;

/// The signed area of a closed polyline, positive for an outer ring.
pub fn ring_area(ring: &[Vec2]) -> f32 {
    let mut sum = 0.0f64;
    for (i, a) in ring.iter().enumerate() {
        let b = ring[(i + 1) % ring.len()];
        sum += f64::from(a.x) * f64::from(b.y) - f64::from(b.x) * f64::from(a.y);
    }
    (sum * 0.5) as f32
}

/// Closed outlines of the artwork in `cov`, in cell units with cell (0, 0)'s
/// centre at (0.5, 0.5). `margin` grows the outline outwards, in cells.
/// Outer rings have a positive [`ring_area`] and holes a negative one; holes
/// are present only when `keep_holes`.
///
/// With a margin, any cell at or above [`THRESHOLD`] is wholly inside and the
/// margin is measured from its pixel edge, so a round corner is a circle about
/// the corner cell's centre and, on the diagonal, falls about 0.21 of a
/// cell short of a true offset of the square.
pub fn rings(cov: &Coverage, margin: f32, keep_holes: bool) -> Vec<Vec<Vec2>> {
    let (w, h) = (cov.size.x as usize, cov.size.y as usize);
    if w == 0 || h == 0 || cov.alpha.len() != w * h {
        return Vec::new();
    }
    let margin = if margin > 0.0 {
        margin.min(MAX_MARGIN)
    } else {
        0.0
    };

    let clean = without_specks(cov.alpha, w, h);
    if clean.iter().all(|&a| a == 0) {
        return Vec::new();
    }

    let pad = margin.ceil() as usize + 2;
    let (pw, ph) = (w + 2 * pad, h + 2 * pad);
    let mut field = vec![-1.0f32; pw * ph];

    if margin > 0.0 {
        let mut d2 = vec![FAR; pw * ph];
        for y in 0..h {
            for x in 0..w {
                if clean[y * w + x] != 0 {
                    d2[(y + pad) * pw + x + pad] = 0.0;
                }
            }
        }
        distance_squared(&mut d2, pw, ph);
        for (f, d) in field.iter_mut().zip(&d2) {
            *f = margin + 0.5 - d.sqrt();
        }
    } else {
        let span = f32::from(255 - THRESHOLD) + 0.5;
        for y in 0..h {
            for x in 0..w {
                let a = clean[y * w + x];
                if a != 0 {
                    field[(y + pad) * pw + x + pad] = (f32::from(a - THRESHOLD) + 0.5) / span;
                }
            }
        }
    }

    if !keep_holes {
        fill_holes(&mut field, pw, ph);
    }
    contour(&field, pw, ph, pad)
}

/// `alpha`, with everything under the threshold and every 8-connected piece of
/// inside smaller than [`MIN_SPECK`] cells set to zero.
fn without_specks(alpha: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out: Vec<u8> = alpha
        .iter()
        .map(|&a| if a >= THRESHOLD { a } else { 0 })
        .collect();
    let mut seen = vec![false; w * h];
    let mut piece: Vec<usize> = Vec::new();
    for start in 0..w * h {
        if seen[start] || out[start] == 0 {
            continue;
        }
        piece.clear();
        piece.push(start);
        seen[start] = true;
        let mut head = 0;
        while head < piece.len() {
            let i = piece[head];
            head += 1;
            let (x, y) = (i % w, i / w);
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let n = ny * w + nx;
                    if !seen[n] && out[n] != 0 {
                        seen[n] = true;
                        piece.push(n);
                    }
                }
            }
        }
        if piece.len() < MIN_SPECK {
            for &i in &piece {
                out[i] = 0;
            }
        }
    }
    out
}

/// Felzenszwalb and Huttenlocher's squared Euclidean distance, exact to `f32`
/// precision, in place,
/// in time linear in the number of cells. `grid` holds 0 for a source and
/// [`FAR`] elsewhere.
fn distance_squared(grid: &mut [f32], w: usize, h: usize) {
    let n = w.max(h);
    let mut line = vec![0.0f32; n];
    let mut out = vec![0.0f32; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f32; n + 1];
    for x in 0..w {
        for y in 0..h {
            line[y] = grid[y * w + x];
        }
        transform_line(&line[..h], &mut out[..h], &mut v, &mut z);
        for y in 0..h {
            grid[y * w + x] = out[y];
        }
    }
    for y in 0..h {
        let row = &mut grid[y * w..(y + 1) * w];
        line[..w].copy_from_slice(row);
        transform_line(&line[..w], row, &mut v, &mut z);
    }
}

/// The one-dimensional lower envelope of parabolas.
fn transform_line(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    let at = |q: usize| q as f32;
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + at(q) * at(q)) - (f[p] + at(p) * at(p))) / (2.0 * at(q) - 2.0 * at(p));
            if s <= z[k] {
                if k > 0 {
                    k -= 1;
                    continue;
                }
                // The new parabola beats the first everywhere; replace it.
                v[0] = q;
                z[0] = f32::NEG_INFINITY;
                z[1] = f32::INFINITY;
            } else {
                k += 1;
                v[k] = q;
                z[k] = s;
                z[k + 1] = f32::INFINITY;
            }
            break;
        }
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while z[k + 1] < at(q) {
            k += 1;
        }
        let dq = at(q) - at(v[k]);
        *out = dq * dq + f[v[k]];
    }
}

/// Raise every outside cell that cannot reach the border to a small positive
/// value, so the contour never sees it. The border is always outside.
fn fill_holes(field: &mut [f32], w: usize, h: usize) {
    let mut reached = vec![false; w * h];
    let mut queue: VecDeque<usize> = VecDeque::new();
    for x in 0..w {
        for y in [0, h - 1] {
            let i = y * w + x;
            if field[i] < 0.0 && !reached[i] {
                reached[i] = true;
                queue.push_back(i);
            }
        }
    }
    for y in 0..h {
        for x in [0, w - 1] {
            let i = y * w + x;
            if field[i] < 0.0 && !reached[i] {
                reached[i] = true;
                queue.push_back(i);
            }
        }
    }
    while let Some(i) = queue.pop_front() {
        let (x, y) = (i % w, i / w);
        let mut neighbours = [usize::MAX; 4];
        if x > 0 {
            neighbours[0] = i - 1;
        }
        if x + 1 < w {
            neighbours[1] = i + 1;
        }
        if y > 0 {
            neighbours[2] = i - w;
        }
        if y + 1 < h {
            neighbours[3] = i + w;
        }
        for n in neighbours {
            if n != usize::MAX && field[n] < 0.0 && !reached[n] {
                reached[n] = true;
                queue.push_back(n);
            }
        }
    }
    for (f, &r) in field.iter_mut().zip(&reached) {
        if *f < 0.0 && !r {
            *f = 1.0e-3;
        }
    }
}

/// Marching squares at zero, stitched into closed rings. The inside is kept on
/// the right of the direction of travel (y down), so an outer ring turns
/// clockwise on screen and has a positive area.
fn contour(field: &[f32], w: usize, h: usize, pad: usize) -> Vec<Vec<Vec2>> {
    let horizontal = |x: usize, y: usize| (y * w + x) as u64;
    let vertical = |x: usize, y: usize| (w * h + y * w + x) as u64;
    let mut segments: Vec<(u64, u64)> = Vec::new();
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            let (a, b) = (field[y * w + x], field[y * w + x + 1]);
            let (d, c) = (field[(y + 1) * w + x], field[(y + 1) * w + x + 1]);
            let case = u8::from(a >= 0.0)
                | (u8::from(b >= 0.0) << 1)
                | (u8::from(c >= 0.0) << 2)
                | (u8::from(d >= 0.0) << 3);
            if case == 0 || case == 15 {
                continue;
            }
            let t = horizontal(x, y);
            let bo = horizontal(x, y + 1);
            let l = vertical(x, y);
            let r = vertical(x + 1, y);
            let centre_in = (a + b + c + d) >= 0.0;
            let mut add = |from: u64, to: u64| segments.push((from, to));
            match case {
                1 => add(t, l),
                2 => add(r, t),
                3 => add(r, l),
                4 => add(bo, r),
                5 => {
                    if centre_in {
                        add(t, r);
                        add(bo, l);
                    } else {
                        add(t, l);
                        add(bo, r);
                    }
                }
                6 => add(bo, t),
                7 => add(bo, l),
                8 => add(l, bo),
                9 => add(t, bo),
                10 => {
                    if centre_in {
                        add(l, t);
                        add(r, bo);
                    } else {
                        add(r, t);
                        add(l, bo);
                    }
                }
                11 => add(r, bo),
                12 => add(l, r),
                13 => add(t, r),
                14 => add(l, t),
                _ => {}
            }
        }
    }
    segments.sort_unstable();

    let point = |id: u64| -> Vec2 {
        let id = id as usize;
        let (x, y, step) = if id < w * h {
            (id % w, id / w, 1)
        } else {
            let k = id - w * h;
            (k % w, k / w, w)
        };
        let (f0, f1) = (field[y * w + x], field[y * w + x + step]);
        let t = f0 / (f0 - f1);
        let origin = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - Vec2::splat(pad as f32);
        if step == 1 {
            origin + Vec2::new(t, 0.0)
        } else {
            origin + Vec2::new(0.0, t)
        }
    };

    let mut used = vec![false; segments.len()];
    let mut out = Vec::new();
    for start in 0..segments.len() {
        if used[start] {
            continue;
        }
        let mut ring = Vec::new();
        let mut i = start;
        loop {
            used[i] = true;
            ring.push(point(segments[i].0));
            let to = segments[i].1;
            let next = segments.partition_point(|s| s.0 < to);
            let found = next < segments.len() && segments[next].0 == to;
            // The padding makes every crossing the end of one segment and the
            // start of exactly one other, so a ring only stops where it began.
            debug_assert!(found, "a contour segment with no successor");
            if !found || used[next] {
                debug_assert!(!found || next == start, "a ring closed onto another");
                break;
            }
            i = next;
        }
        if ring.len() >= 3 {
            out.push(ring);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn grid(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> (UVec2, Vec<u8>) {
        let mut a = Vec::new();
        for y in 0..h {
            for x in 0..w {
                a.push(f(x, y));
            }
        }
        (UVec2::new(w, h), a)
    }

    fn block(x: u32, y: u32, lo: u32, hi: u32) -> bool {
        (lo..hi).contains(&x) && (lo..hi).contains(&y)
    }

    fn trace(size: UVec2, alpha: &[u8], margin: f32, holes: bool) -> Vec<Vec<Vec2>> {
        rings(&Coverage { size, alpha }, margin, holes)
    }

    fn bounds(ring: &[Vec2]) -> (Vec2, Vec2) {
        ring.iter()
            .fold((Vec2::splat(1e9), Vec2::splat(-1e9)), |(lo, hi), p| {
                (lo.min(*p), hi.max(*p))
            })
    }

    #[test]
    fn an_empty_grid_has_no_rings() {
        let (s, a) = grid(10, 10, |_, _| 0);
        assert!(trace(s, &a, 0.0, false).is_empty());
        assert!(trace(s, &a, 5.0, true).is_empty());
    }

    #[test]
    fn a_block_at_margin_zero_is_outlined_on_its_own_edge() {
        let (s, a) = grid(20, 20, |x, y| if block(x, y, 5, 15) { 255 } else { 0 });
        let r = trace(s, &a, 0.0, false);
        assert_eq!(r.len(), 1);
        let (lo, hi) = bounds(&r[0]);
        assert!((lo - Vec2::splat(5.0)).abs().max_element() < 0.6, "{lo}");
        assert!((hi - Vec2::splat(15.0)).abs().max_element() < 0.6, "{hi}");
        let area = ring_area(&r[0]);
        assert!((area - 100.0).abs() < 2.0, "{area}");
    }

    #[test]
    fn a_margin_grows_the_outline_by_a_round_offset() {
        let (s, a) = grid(20, 20, |x, y| if block(x, y, 5, 15) { 255 } else { 0 });
        let r = trace(s, &a, 6.0, false);
        assert_eq!(r.len(), 1);
        let want = 100.0 + 40.0 * 6.0 + PI * 36.0;
        let area = ring_area(&r[0]);
        assert!((area - want).abs() < want * 0.03, "{area} vs {want}");
        let round = 5.0 - 6.0 / 2.0f32.sqrt();
        let square = 5.0 - 6.0;
        // The ring point nearest the block's top-left corner sits on the
        // rounded offset, not at a mitre.
        let corner = r[0]
            .iter()
            .copied()
            .min_by(|a, b| {
                a.distance_squared(Vec2::splat(-1.0))
                    .total_cmp(&b.distance_squared(Vec2::splat(-1.0)))
            })
            .unwrap();
        assert!(
            (corner.x - round).abs() < (corner.x - square).abs(),
            "{corner}"
        );
        assert!(
            (corner.y - round).abs() < (corner.y - square).abs(),
            "{corner}"
        );
    }

    #[test]
    fn a_faint_haze_is_outside() {
        let (s, a) = grid(12, 12, |x, y| if block(x, y, 3, 9) { 25 } else { 0 });
        assert!(trace(s, &a, 0.0, false).is_empty());
        let (s, a) = grid(12, 12, |x, y| if block(x, y, 3, 9) { 26 } else { 0 });
        assert_eq!(trace(s, &a, 0.0, false).len(), 1);
    }

    #[test]
    fn a_speck_is_dropped_and_a_small_mark_is_kept() {
        let (s, a) = grid(12, 12, |x, y| {
            if y == 5 && (4..7).contains(&x) {
                255
            } else {
                0
            }
        });
        assert!(trace(s, &a, 10.0, false).is_empty());
        let (s, a) = grid(12, 12, |x, y| {
            if y == 5 && (4..8).contains(&x) {
                255
            } else {
                0
            }
        });
        assert_eq!(trace(s, &a, 0.0, false).len(), 1);
        // Diagonal neighbours are connected.
        let (s, a) = grid(12, 12, |x, y| {
            if x == y && (3..7).contains(&x) {
                255
            } else {
                0
            }
        });
        assert_eq!(trace(s, &a, 0.0, false).len(), 1);
    }

    fn holed(x: u32, y: u32) -> u8 {
        if block(x, y, 5, 35) && !block(x, y, 15, 25) {
            255
        } else {
            0
        }
    }

    #[test]
    fn a_hole_is_filled_unless_asked_for() {
        let (s, a) = grid(40, 40, holed);
        let filled = trace(s, &a, 0.0, false);
        assert_eq!(filled.len(), 1);
        assert!((ring_area(&filled[0]) - 900.0).abs() < 20.0);
        let kept = trace(s, &a, 0.0, true);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept.iter().filter(|r| ring_area(r) < 0.0).count(), 1);
        assert_eq!(kept.iter().filter(|r| ring_area(r) > 0.0).count(), 1);
    }

    #[test]
    fn a_piece_inside_a_hole_is_not_outlined_when_holes_are_filled() {
        let (s, a) = grid(40, 40, |x, y| {
            if block(x, y, 18, 22) {
                255
            } else {
                holed(x, y)
            }
        });
        assert_eq!(trace(s, &a, 0.0, false).len(), 1);
        assert_eq!(trace(s, &a, 0.0, true).len(), 3);
    }

    #[test]
    fn artwork_touching_the_grid_edge_still_closes() {
        let (s, a) = grid(20, 20, |_, _| 255);
        for margin in [0.0, 4.0] {
            let r = trace(s, &a, margin, false);
            assert_eq!(r.len(), 1, "margin {margin}");
            let ring = &r[0];
            assert!(ring.len() >= 4);
            // Closed: every step, the last back to the first included, crosses
            // at most one cell, so the stitcher found a successor everywhere.
            for (i, p) in ring.iter().enumerate() {
                let q = ring[(i + 1) % ring.len()];
                assert!(p.distance(q) <= 1.5, "margin {margin}: {p} to {q}");
            }
            // Marching squares clips each square corner by an eighth of a
            // cell at margin zero, so the floor is the grid less that.
            let floor = if margin == 0.0 { 399.0 } else { 400.0 };
            assert!(ring_area(ring) >= floor, "{}", ring_area(ring));
        }
    }

    #[test]
    fn a_margin_that_joins_two_blobs_gives_one_ring() {
        let (s, a) = grid(40, 20, |x, y| {
            if (5..15).contains(&y) && ((5..11).contains(&x) || (17..23).contains(&x)) {
                255
            } else {
                0
            }
        });
        assert_eq!(trace(s, &a, 2.0, false).len(), 2);
        assert_eq!(trace(s, &a, 4.0, false).len(), 1);
    }

    #[test]
    fn an_antialiased_edge_places_the_line_between_pixels() {
        // Opaque columns 3..8, a half-covered column at 8, empty beyond.
        let (s, a) = grid(14, 12, |x, y| {
            if !(3..9).contains(&y) {
                0
            } else if (3..8).contains(&x) {
                255
            } else if x == 8 {
                128
            } else {
                0
            }
        });
        let r = trace(s, &a, 0.0, false);
        assert_eq!(r.len(), 1);
        let (_, hi) = bounds(&r[0]);
        // Inside column 8, short of its edge: a binary field would put the line
        // at 9.0, and the half coverage pulls it back to about 8.81.
        assert!(hi.x > 8.5 && hi.x < 8.95, "{}", hi.x);
    }

    #[test]
    fn a_grid_of_the_wrong_length_has_no_rings() {
        let a = vec![255u8; 99];
        assert!(trace(UVec2::new(10, 10), &a, 0.0, false).is_empty());
    }

    #[test]
    fn a_grown_corner_falls_a_fifth_of_a_cell_short_on_the_diagonal() {
        // The figure `rings`' docs give: a circle of `margin + 0.5` about the
        // corner cell's centre, against `margin` beyond the corner itself.
        let (s, a) = grid(40, 40, |x, y| if block(x, y, 15, 25) { 255 } else { 0 });
        let r = trace(s, &a, 6.0, false);
        let p = r[0]
            .iter()
            .copied()
            .filter(|p| p.x < 15.0 && p.y < 15.0)
            .min_by(|a, b| (a.x - a.y).abs().total_cmp(&(b.x - b.y).abs()))
            .unwrap();
        let short = 6.0 - (Vec2::splat(15.0) - p).length();
        assert!(short > 0.15 && short < 0.25, "{short}");
    }
}
