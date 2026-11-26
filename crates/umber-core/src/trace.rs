//! Tracing artwork into outlines.
//!
//! Pure CPU, in three steps: [`region`] crops a document's alpha to what is
//! worth tracing and averages it down to a working grid, [`rings`] turns that
//! grid into closed polylines, and [`trace`] fits cubic nodes to them through
//! [`fit_closed`] and hands back paths in document pixels. Nothing here knows
//! about a GPU or a window.
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
use std::ops::RangeInclusive;

use glam::{IVec2, UVec2, Vec2};

use crate::vector::{Path, PathRole};

pub use fit::{CORNER_DEGREES, fit_closed, fit_open};

/// Alpha at or above this is inside. About 10%.
pub const THRESHOLD: u8 = 26;
/// A connected piece of inside smaller than this many cells is removed.
pub const MIN_SPECK: usize = 4;
/// The largest margin, in cells, that [`rings`] will honour, and in document
/// pixels, that [`region`] and [`trace`] will.
pub const MAX_MARGIN: f32 = 512.0;
/// The most cells [`region`] is asked to leave in a working grid.
pub const MAX_CELLS: usize = 8_000_000;
/// The fitter's tolerance, in document pixels.
pub const SMOOTHING: RangeInclusive<f32> = 0.25..=8.0;
/// A ring enclosing less than this many cells is not an outline. A one-cell
/// line at the threshold contours as a sliver a few hundredths of a cell wide,
/// which encloses nothing anybody could cut.
const MIN_AREA: f32 = 1.0;

/// What [`trace`] is asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TraceSettings {
    /// Document pixels the outline stands off the artwork.
    pub margin: f32,
    /// The fitter's tolerance, in document pixels; see [`SMOOTHING`].
    pub smoothing: f32,
    /// Outline the holes as well as the outer edge.
    pub keep_holes: bool,
    /// The role every path is given.
    pub role: PathRole,
}

impl TraceSettings {
    /// No margin, a tolerance of one pixel, holes filled.
    pub fn new(role: PathRole) -> Self {
        Self {
            margin: 0.0,
            smoothing: 1.0,
            keep_holes: false,
            role,
        }
    }
}

/// The part of a document's alpha worth tracing, ready for [`trace`].
///
/// Cell `(i, j)` covers the `cell`-square of document pixels whose top-left
/// corner is `origin + (i, j) * cell`. `origin` may be negative and the region
/// may reach past the document, because the margin is padded in as well;
/// anything off the document is empty.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub origin: IVec2,
    pub cell: u32,
    pub size: UVec2,
    pub alpha: Vec<u8>,
}

/// `margin`, clamped to `0.0..=MAX_MARGIN`, with NaN as zero.
fn clamp_margin(margin: f32) -> f32 {
    if margin > 0.0 {
        margin.min(MAX_MARGIN)
    } else {
        0.0
    }
}

/// Crop `alpha` (document-sized) to what is inside, pad by `margin`, and
/// average down by a whole factor until it is at most `max_cells`.
/// `None` when nothing reaches [`THRESHOLD`], or `alpha` is not `doc` long.
///
/// The padding is counted in the budget because the grid [`rings`] grows over
/// is that large too. A cell of an averaged grid is the plain mean of the
/// pixels it covers, with anything off the document counted as empty.
pub fn region(doc: UVec2, alpha: &[u8], margin: f32, max_cells: usize) -> Option<Region> {
    let (w, h) = (doc.x as usize, doc.y as usize);
    if w == 0 || h == 0 || alpha.len() != w * h {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for (y, row) in alpha.chunks_exact(w).enumerate() {
        let Some(first) = row.iter().position(|&a| a >= THRESHOLD) else {
            continue;
        };
        let last = row.iter().rposition(|&a| a >= THRESHOLD).unwrap_or(first);
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    if x0 == usize::MAX {
        return None;
    }
    let pad = clamp_margin(margin).ceil() as i64;
    let (lx, ly) = (x0 as i64 - pad, y0 as i64 - pad);
    let rw = (x1 - x0) as u64 + 2 * pad as u64;
    let rh = (y1 - y0) as u64 + 2 * pad as u64;
    let budget = max_cells.max(1) as u64;
    let cells = |f: u64| rw.div_ceil(f) * rh.div_ceil(f);
    // A guess from the area, then walked to the smallest factor that fits.
    let mut f = (((rw * rh) as f64 / budget as f64).sqrt() as u64).max(1);
    while f > 1 && cells(f - 1) <= budget {
        f -= 1;
    }
    while cells(f) > budget {
        f += 1;
    }
    let (sw, sh) = (rw.div_ceil(f) as usize, rh.div_ceil(f) as usize);
    let f = f as usize;
    let mut sums = vec![0u64; sw * sh];
    let ys = ly.max(0) as usize..(ly + rh as i64).min(h as i64) as usize;
    let xs = lx.max(0) as usize..(lx + rw as i64).min(w as i64) as usize;
    for y in ys {
        let row = &alpha[y * w..(y + 1) * w];
        let base = (y as i64 - ly) as usize / f * sw;
        for x in xs.clone() {
            sums[base + (x as i64 - lx) as usize / f] += u64::from(row[x]);
        }
    }
    let area = (f * f) as u64;
    Some(Region {
        origin: IVec2::new(lx as i32, ly as i32),
        cell: f as u32,
        size: UVec2::new(sw as u32, sh as u32),
        alpha: sums
            .into_iter()
            .map(|s| ((s + area / 2) / area) as u8)
            .collect(),
    })
}

/// Closed paths in document pixels, largest first.
///
/// `margin` is clamped to `0.0..=MAX_MARGIN` and `smoothing` to
/// [`SMOOTHING`] (NaN as its default of 1.0), and both are turned into cells
/// by dividing by `region.cell`. The tolerance is never less than a quarter of
/// a cell: on an averaged grid that is the finest detail there is, and asking
/// for less fits the grid's own staircase. A ring enclosing less than a cell,
/// or that fits to fewer than two nodes, is dropped. With `keep_holes` a hole
/// is a closed path like any other, sorted by the size of what it encloses.
pub fn trace(region: &Region, settings: &TraceSettings) -> Vec<Path> {
    let cell = region.cell.max(1) as f32;
    let margin = clamp_margin(settings.margin) / cell;
    let smoothing = if settings.smoothing.is_nan() {
        1.0
    } else {
        settings
            .smoothing
            .clamp(*SMOOTHING.start(), *SMOOTHING.end())
    };
    let tolerance = (smoothing / cell).max(*SMOOTHING.start());
    let cov = Coverage {
        size: region.size,
        alpha: &region.alpha,
    };
    let mut found: Vec<(f32, Vec<Vec2>)> = rings(&cov, margin, settings.keep_holes)
        .into_iter()
        .map(|r| (ring_area(&r).abs(), r))
        .filter(|(area, _)| *area >= MIN_AREA)
        .collect();
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    let origin = region.origin.as_vec2();
    found
        .iter()
        .filter_map(|(_, ring)| {
            let nodes = fit_closed(ring, tolerance);
            if nodes.len() < 2 {
                return None;
            }
            let nodes = nodes
                .into_iter()
                .map(|mut n| {
                    n.at = origin + n.at * cell;
                    n.handle_in *= cell;
                    n.handle_out *= cell;
                    n
                })
                .collect();
            Some(Path {
                role: settings.role,
                closed: true,
                nodes,
                ink: None,
            })
        })
        .collect()
}

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

    fn outline(size: UVec2, alpha: &[u8], margin: f32, holes: bool) -> Vec<Vec<Vec2>> {
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
        assert!(outline(s, &a, 0.0, false).is_empty());
        assert!(outline(s, &a, 5.0, true).is_empty());
    }

    #[test]
    fn a_block_at_margin_zero_is_outlined_on_its_own_edge() {
        let (s, a) = grid(20, 20, |x, y| if block(x, y, 5, 15) { 255 } else { 0 });
        let r = outline(s, &a, 0.0, false);
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
        let r = outline(s, &a, 6.0, false);
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
        assert!(outline(s, &a, 0.0, false).is_empty());
        let (s, a) = grid(12, 12, |x, y| if block(x, y, 3, 9) { 26 } else { 0 });
        assert_eq!(outline(s, &a, 0.0, false).len(), 1);
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
        assert!(outline(s, &a, 10.0, false).is_empty());
        let (s, a) = grid(12, 12, |x, y| {
            if y == 5 && (4..8).contains(&x) {
                255
            } else {
                0
            }
        });
        assert_eq!(outline(s, &a, 0.0, false).len(), 1);
        // Diagonal neighbours are connected.
        let (s, a) = grid(12, 12, |x, y| {
            if x == y && (3..7).contains(&x) {
                255
            } else {
                0
            }
        });
        assert_eq!(outline(s, &a, 0.0, false).len(), 1);
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
        let filled = outline(s, &a, 0.0, false);
        assert_eq!(filled.len(), 1);
        assert!((ring_area(&filled[0]) - 900.0).abs() < 20.0);
        let kept = outline(s, &a, 0.0, true);
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
        assert_eq!(outline(s, &a, 0.0, false).len(), 1);
        assert_eq!(outline(s, &a, 0.0, true).len(), 3);
    }

    #[test]
    fn artwork_touching_the_grid_edge_still_closes() {
        let (s, a) = grid(20, 20, |_, _| 255);
        for margin in [0.0, 4.0] {
            let r = outline(s, &a, margin, false);
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
        assert_eq!(outline(s, &a, 2.0, false).len(), 2);
        assert_eq!(outline(s, &a, 4.0, false).len(), 1);
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
        let r = outline(s, &a, 0.0, false);
        assert_eq!(r.len(), 1);
        let (_, hi) = bounds(&r[0]);
        // Inside column 8, short of its edge: a binary field would put the line
        // at 9.0, and the half coverage pulls it back to about 8.81.
        assert!(hi.x > 8.5 && hi.x < 8.95, "{}", hi.x);
    }

    #[test]
    fn a_grid_of_the_wrong_length_has_no_rings() {
        let a = vec![255u8; 99];
        assert!(outline(UVec2::new(10, 10), &a, 0.0, false).is_empty());
    }

    #[test]
    fn a_grown_corner_falls_a_fifth_of_a_cell_short_on_the_diagonal() {
        // The figure `rings`' docs give: a circle of `margin + 0.5` about the
        // corner cell's centre, against `margin` beyond the corner itself.
        let (s, a) = grid(40, 40, |x, y| if block(x, y, 15, 25) { 255 } else { 0 });
        let r = outline(s, &a, 6.0, false);
        let p = r[0]
            .iter()
            .copied()
            .filter(|p| p.x < 15.0 && p.y < 15.0)
            .min_by(|a, b| (a.x - a.y).abs().total_cmp(&(b.x - b.y).abs()))
            .unwrap();
        let short = 6.0 - (Vec2::splat(15.0) - p).length();
        assert!(short > 0.15 && short < 0.25, "{short}");
    }

    use crate::pathedit::is_smooth;
    use crate::vector::PathRole;

    fn flat_bounds(path: &Path) -> (Vec2, Vec2) {
        let mut pts = Vec::new();
        path.flatten(0.1, &mut pts);
        bounds(&pts)
    }

    fn disc(doc: UVec2, centre: Vec2, r: f32) -> Vec<u8> {
        grid(doc.x, doc.y, |x, y| {
            if (Vec2::new(x as f32, y as f32) + 0.5).distance(centre) <= r {
                255
            } else {
                0
            }
        })
        .1
    }

    fn settings(margin: f32) -> TraceSettings {
        TraceSettings {
            margin,
            ..TraceSettings::new(PathRole::Cut)
        }
    }

    #[test]
    fn settings_start_from_no_margin_and_a_pixel_of_smoothing() {
        let s = TraceSettings::new(PathRole::Plot);
        assert_eq!(s.margin, 0.0);
        assert_eq!(s.smoothing, 1.0);
        assert!(!s.keep_holes);
        assert_eq!(s.role, PathRole::Plot);
    }

    #[test]
    fn a_traced_block_keeps_four_corners() {
        let (s, a) = grid(80, 80, |x, y| if block(x, y, 10, 70) { 255 } else { 0 });
        let region = region(s, &a, 0.0, MAX_CELLS).unwrap();
        let paths = trace(&region, &settings(0.0));
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].nodes.len(), 4, "{:?}", paths[0].nodes);
        for n in &paths[0].nodes {
            assert!(!is_smooth(n), "{n:?}");
            let near = [10.0f32, 70.0]
                .iter()
                .flat_map(|&x| [10.0f32, 70.0].map(|y| Vec2::new(x, y)))
                .any(|c| c.distance(n.at) <= 1.0);
            assert!(near, "{}", n.at);
        }
    }

    #[test]
    fn a_grown_block_has_round_corners() {
        let (s, a) = grid(100, 100, |x, y| if block(x, y, 20, 80) { 255 } else { 0 });
        let region = region(s, &a, 10.0, MAX_CELLS).unwrap();
        let paths = trace(&region, &settings(10.0));
        assert_eq!(paths.len(), 1);
        let nodes = &paths[0].nodes;
        assert!(nodes.len() <= 12, "{}", nodes.len());
        for n in nodes {
            let corner = (n.at.x < 20.0 || n.at.x > 80.0) && (n.at.y < 20.0 || n.at.y > 80.0);
            if corner {
                assert!(is_smooth(n), "a hard corner in the rounding: {n:?}");
            }
        }
        let (lo, hi) = flat_bounds(&paths[0]);
        assert!((lo - Vec2::splat(10.0)).abs().max_element() < 1.0, "{lo}");
        assert!((hi - Vec2::splat(90.0)).abs().max_element() < 1.0, "{hi}");
    }

    #[test]
    fn a_faint_line_too_thin_to_enclose_anything_is_dropped() {
        let (s, a) = grid(30, 10, |x, y| {
            if y == 4 && (5..25).contains(&x) {
                THRESHOLD
            } else {
                0
            }
        });
        let slivers = outline(s, &a, 0.0, false);
        assert!(!slivers.is_empty());
        assert!(slivers.iter().all(|r| ring_area(r).abs() < 1.0));
        let region = region(s, &a, 0.0, MAX_CELLS).unwrap();
        assert!(trace(&region, &settings(0.0)).is_empty());
        // Grown, the same line is an outline.
        let region = super::region(s, &a, 2.0, MAX_CELLS).unwrap();
        assert_eq!(trace(&region, &settings(2.0)).len(), 1);
    }

    #[test]
    fn a_traced_disc_lands_where_it_was_drawn() {
        let doc = UVec2::new(400, 300);
        let a = disc(doc, Vec2::new(250.0, 120.0), 40.0);
        let region = region(doc, &a, 10.0, MAX_CELLS).unwrap();
        assert_eq!(region.cell, 1);
        let paths = trace(&region, &settings(10.0));
        assert_eq!(paths.len(), 1);
        let path = &paths[0];
        assert_eq!(path.role, PathRole::Cut);
        assert!(path.closed);
        assert!(path.nodes.len() <= 16, "{}", path.nodes.len());
        let (lo, hi) = flat_bounds(path);
        assert!(
            (lo - Vec2::new(200.0, 70.0)).abs().max_element() <= 1.5,
            "{lo}"
        );
        assert!(
            (hi - Vec2::new(300.0, 170.0)).abs().max_element() <= 1.5,
            "{hi}"
        );
    }

    #[test]
    fn a_region_over_the_ceiling_is_averaged_down_and_still_lands_in_place() {
        let doc = UVec2::new(400, 300);
        let a = disc(doc, Vec2::new(250.0, 120.0), 40.0);
        let region = region(doc, &a, 10.0, 5_000).unwrap();
        assert!(region.cell > 1);
        assert!((region.size.x * region.size.y) as usize <= 5_000);
        assert_eq!(region.alpha.len(), (region.size.x * region.size.y) as usize);
        let paths = trace(&region, &settings(10.0));
        assert_eq!(paths.len(), 1);
        let slack = region.cell as f32 + 1.5;
        let (lo, hi) = flat_bounds(&paths[0]);
        assert!(
            (lo - Vec2::new(200.0, 70.0)).abs().max_element() <= slack,
            "{lo}"
        );
        assert!(
            (hi - Vec2::new(300.0, 170.0)).abs().max_element() <= slack,
            "{hi}"
        );
    }

    #[test]
    fn a_region_reaches_past_the_document_by_the_margin() {
        let (s, a) = grid(50, 40, |x, y| {
            if x < 10 && (5..20).contains(&y) {
                255
            } else {
                0
            }
        });
        let r = region(s, &a, 7.5, MAX_CELLS).unwrap();
        assert_eq!(r.origin, IVec2::new(-8, -3));
        assert_eq!(r.size, UVec2::new(26, 31));
        assert_eq!(r.cell, 1);
        // Off the document is empty, and the document is copied as it was.
        assert_eq!(r.alpha[0], 0);
        assert_eq!(r.alpha[8 * 26 + 8], 255);
    }

    #[test]
    fn nothing_inside_is_no_region() {
        let (s, a) = grid(30, 30, |_, _| 0);
        assert!(region(s, &a, 5.0, MAX_CELLS).is_none());
        let (s, a) = grid(30, 30, |_, _| THRESHOLD - 1);
        assert!(region(s, &a, 5.0, MAX_CELLS).is_none());
        assert!(region(UVec2::new(30, 30), &a[1..], 5.0, MAX_CELLS).is_none());
        assert!(region(UVec2::ZERO, &[], 5.0, MAX_CELLS).is_none());
    }

    #[test]
    fn paths_come_largest_first() {
        let doc = UVec2::new(300, 200);
        let small = disc(doc, Vec2::new(50.0, 100.0), 12.0);
        let large = disc(doc, Vec2::new(200.0, 100.0), 40.0);
        let middle = disc(doc, Vec2::new(110.0, 60.0), 20.0);
        let a: Vec<u8> = (0..small.len())
            .map(|i| small[i].max(large[i]).max(middle[i]))
            .collect();
        let region = region(doc, &a, 0.0, MAX_CELLS).unwrap();
        let paths = trace(&region, &settings(0.0));
        assert_eq!(paths.len(), 3);
        let widths: Vec<f32> = paths
            .iter()
            .map(|p| {
                let (lo, hi) = flat_bounds(p);
                hi.x - lo.x
            })
            .collect();
        assert!(widths[0] > widths[1] && widths[1] > widths[2], "{widths:?}");
    }

    #[test]
    fn a_kept_hole_is_a_closed_path_of_its_own() {
        let (s, a) = grid(40, 40, holed);
        let region = region(s, &a, 0.0, MAX_CELLS).unwrap();
        let paths = trace(
            &region,
            &TraceSettings {
                keep_holes: true,
                ..settings(0.0)
            },
        );
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|p| p.closed && p.nodes.len() >= 2));
        let (lo, hi) = flat_bounds(&paths[1]);
        assert!((lo - Vec2::splat(15.0)).abs().max_element() < 1.0, "{lo}");
        assert!((hi - Vec2::splat(25.0)).abs().max_element() < 1.0, "{hi}");
        assert_eq!(trace(&region, &settings(0.0)).len(), 1);
    }

    #[test]
    fn a_margin_past_the_ceiling_is_clamped() {
        let doc = UVec2::new(64, 64);
        let a = disc(doc, Vec2::new(32.0, 32.0), 10.0);
        let budget = 40_000;
        let over = region(doc, &a, 1.0e6, budget).unwrap();
        let at = region(doc, &a, MAX_MARGIN, budget).unwrap();
        assert_eq!(over, at);
        let far = trace(&over, &settings(f32::INFINITY));
        let ceiling = trace(&at, &settings(MAX_MARGIN));
        assert_eq!(far, ceiling);
        assert_eq!(far.len(), 1);
        let (lo, hi) = flat_bounds(&far[0]);
        let slack = over.cell as f32 + 1.5;
        let reach = 10.0 + MAX_MARGIN;
        assert!(
            (lo - Vec2::splat(32.0 - reach)).abs().max_element() <= slack,
            "{lo}"
        );
        assert!(
            (hi - Vec2::splat(32.0 + reach)).abs().max_element() <= slack,
            "{hi}"
        );
        // Below the floor and not a number are both no margin at all.
        let none = region(doc, &a, -5.0, budget).unwrap();
        assert_eq!(none, region(doc, &a, f32::NAN, budget).unwrap());
        assert_eq!(
            trace(&none, &settings(-5.0)),
            trace(&none, &settings(f32::NAN))
        );
    }

    #[test]
    fn smoothing_outside_its_range_is_clamped() {
        let doc = UVec2::new(200, 200);
        let a = disc(doc, Vec2::new(100.0, 100.0), 60.0);
        let region = region(doc, &a, 0.0, MAX_CELLS).unwrap();
        let with = |smoothing: f32| {
            trace(
                &region,
                &TraceSettings {
                    smoothing,
                    ..settings(0.0)
                },
            )
        };
        assert_eq!(with(100.0), with(*SMOOTHING.end()));
        assert_eq!(with(0.0), with(*SMOOTHING.start()));
        assert_eq!(with(f32::NAN), with(1.0));
    }
}
