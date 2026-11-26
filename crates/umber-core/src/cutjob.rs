//! What a cutter is sent: vector paths turned into the machine's own commands.
//!
//! This is the pure half of sending a job, bytes and nothing else. No USB, no
//! thread, no window: the device code takes the blocks this module builds and
//! writes them one at a time. Keeping the encoding here is what lets every byte
//! a blade will follow be pinned by a test that needs no machine, and the tests
//! below compare literal byte strings for exactly that reason. A command that
//! reads better but is not what the machine expects is a cut in the wrong place,
//! so nothing here is to be "tidied".
//!
//! The protocol is the Silhouette one (GPGL). A command is ASCII text ended by
//! ETX, a point is sent **y first** as whole machine units of a twentieth of a
//! millimetre, and with marks the origin is the registration square rather than
//! the page corner, because the machine has measured where the square really
//! sits and moves everything by the difference.
//!
//! Three decisions worth knowing:
//!
//! - **The job is planned whole before a byte is built.** A path that would
//!   leave the cut area refuses the *job* rather than being skipped, because a
//!   sheet with one sticker not cut is a sheet thrown away, and the artist
//!   should be told before the blade comes down rather than after.
//! - **Closed paths are cut smallest first.** A hole inside a shape has to be
//!   cut while the shape is still held by the sheet around it; cut the outline
//!   first and the piece can shift under the blade. Sorting by area is the
//!   ordinary approximation of "inside first", and it needs no containment test.
//! - **Points are counted as they will be sent**, passes included, because
//!   [`MAX_POINTS`] is a bound on what the machine is handed, not on what is
//!   drawn.

use glam::Vec2;

use crate::document::Document;
use crate::layer::LayerStack;
use crate::sheet::{MAX_CUT_WIDTH_MM, Marks, mm_per_pixel};
use crate::trace::ring_area;
use crate::vector::{Path, PathRole};

/// Machine units in a millimetre.
pub const UNITS_PER_MM: f32 = 20.0;
/// The byte that ends every command.
pub const ETX: u8 = 0x03;
/// How far a flattened curve may stray from the true one. A machine unit is
/// 0.05 mm, so a finer flattening would only produce points the rounding merges.
pub const FLATTEN_MM: f32 = 0.05;
/// The most points one job may send, passes included.
pub const MAX_POINTS: usize = 200_000;
/// The longest media the machine is told about.
pub const MAX_LENGTH_MM: f32 = 18290.0;
/// The most bytes in one write to the machine.
pub const BLOCK_BYTES: usize = 1024;

/// Half a machine unit: a point drawn exactly on a boundary is admitted, and
/// floating point a hair past it is not a reason to refuse the job.
const EDGE_MM: f32 = 0.5 / UNITS_PER_MM;

/// What the machine is asked to do with the paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobSettings {
    /// Which paths are sent.
    pub role: PathRole,
    /// Whether the machine reads the registration marks first.
    pub use_marks: bool,
    /// Blade or pen force, `1..=33`.
    pub pressure: u8,
    /// `1..=10`.
    pub speed: u8,
    /// How many times every path is followed, `1..=5`.
    pub passes: u8,
}

impl JobSettings {
    /// The starting point for a role: a blade presses harder and moves slower
    /// than a pen. Ink is never sent, so it takes the gentler of the two rather
    /// than having no answer.
    pub fn new(role: PathRole) -> Self {
        let (pressure, speed) = match role {
            PathRole::Cut => (10, 5),
            PathRole::Plot | PathRole::Ink => (5, 8),
        };
        Self {
            role,
            use_marks: true,
            pressure,
            speed,
            passes: 1,
        }
    }

    /// These settings inside what the machine accepts. A value it does not
    /// take is not something to send and hope.
    fn clamped(&self) -> JobSettings {
        JobSettings {
            pressure: self.pressure.clamp(1, 33),
            speed: self.speed.clamp(1, 10),
            passes: self.passes.clamp(1, 5),
            ..*self
        }
    }
}

/// Why a job cannot be sent.
#[derive(Clone, Debug, PartialEq)]
pub enum JobRefusal {
    /// Nothing of this role survived to be sent.
    NoPaths(PathRole),
    /// Ink paths are for the canvas, not a machine.
    InkRole,
    /// A path leaves the rectangle between the registration marks.
    OutsideCutArea,
    /// A path leaves the page.
    OutsidePage,
    /// A path reaches past the widest the machine can cut.
    PageTooWide,
    /// Marks were asked for and this document has none.
    NoMarks,
    /// More points than one job may send.
    TooManyPoints,
}

impl JobRefusal {
    /// What to tell somebody, in a sentence with no symbols in it.
    pub fn sentence(&self) -> String {
        match self {
            JobRefusal::NoPaths(role) => {
                let what = match role {
                    PathRole::Cut => "cut",
                    PathRole::Plot => "plot",
                    PathRole::Ink => "ink",
                };
                format!(
                    "There are no {what} paths to send. Draw one with the Path tool on a visible vector layer."
                )
            }
            JobRefusal::InkRole => {
                "Ink paths are drawn on the canvas and cannot be sent to a cutter. Choose Cut or Plot."
                    .to_string()
            }
            JobRefusal::OutsideCutArea => {
                "A path goes outside the area between the registration marks. Move it inside them and send again."
                    .to_string()
            }
            JobRefusal::OutsidePage => {
                "A path goes off the edge of the page. Move it onto the page and send again."
                    .to_string()
            }
            JobRefusal::PageTooWide => {
                "A path reaches further across than the cutter can, which is 203 mm from the left edge."
                    .to_string()
            }
            JobRefusal::NoMarks => {
                "This document has no registration marks. Turn them on and print the sheet, or send without them."
                    .to_string()
            }
            JobRefusal::TooManyPoints => format!(
                "These paths are too detailed to send in one job. The cutter takes at most {MAX_POINTS} points."
            ),
        }
    }
}

/// The sheet a job is planned against, in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Page {
    pub size_mm: Vec2,
    pub mm_per_pixel: f32,
    /// Present only where the document asks for marks and its page can hold
    /// them, which is the same answer the sheet writer reaches.
    pub marks: Option<Marks>,
}

impl Page {
    pub fn of(doc: &Document) -> Page {
        let mm = mm_per_pixel(doc.dpi);
        Page {
            size_mm: doc.size.as_vec2() * mm,
            mm_per_pixel: mm,
            marks: if doc.marks {
                Marks::for_document(doc).ok()
            } else {
                None
            },
        }
    }
}

/// Every path of `role` on a visible vector layer, in stack order.
pub fn paths_of(stack: &LayerStack, role: PathRole) -> Vec<&Path> {
    stack
        .layers()
        .iter()
        .enumerate()
        .filter(|&(i, _)| stack.effective_visible(i))
        .filter_map(|(_, layer)| layer.vector())
        .flat_map(|v| v.paths())
        .filter(|p| p.role == role)
        .collect()
}

/// A planned job: the paths as machine points, in the order they are sent.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    settings: JobSettings,
    /// Media size in machine units, `(length, width)`.
    media: (i64, i64),
    /// The span between the marks in machine units, `(y, x)`, where marks are used.
    span: Option<(i64, i64)>,
    /// Each path's points in machine units, `(y, x)`, consecutive ones distinct.
    paths: Vec<Vec<(i64, i64)>>,
    length_mm: f32,
}

/// Plan a job, or say why it cannot be sent.
///
/// `paths` may hold paths of any role; only those of `settings.role` are
/// considered, so a caller can hand over everything it has.
pub fn plan(paths: &[&Path], settings: &JobSettings, page: &Page) -> Result<Job, JobRefusal> {
    let settings = settings.clamped();
    match settings.role {
        PathRole::Cut | PathRole::Plot => {}
        PathRole::Ink => return Err(JobRefusal::InkRole),
    }
    let marks = if settings.use_marks {
        Some(page.marks.ok_or(JobRefusal::NoMarks)?)
    } else {
        None
    };
    let media_length_mm = page.size_mm.y.min(MAX_LENGTH_MM);
    let width_mm = page.size_mm.x.min(MAX_CUT_WIDTH_MM);

    // Where a point may be, where coordinates are counted from, and how far
    // they reach, all in millimetres.
    let (origin, extent) = match &marks {
        Some(m) => (m.origin(), m.span),
        None => (Vec2::ZERO, Vec2::new(page.size_mm.x, media_length_mm)),
    };
    let max_units = (
        (extent.y * UNITS_PER_MM).round() as i64,
        (extent.x * UNITS_PER_MM).round() as i64,
    );

    let tolerance_px = FLATTEN_MM / page.mm_per_pixel;
    let passes = usize::from(settings.passes);
    let mut closed: Vec<(f32, Vec<(i64, i64)>)> = Vec::new();
    let mut open: Vec<Vec<(i64, i64)>> = Vec::new();
    let mut sent = 0usize;
    let mut flat = Vec::new();
    for path in paths.iter().filter(|p| p.role == settings.role) {
        flat.clear();
        path.flatten(tolerance_px, &mut flat);
        for p in flat.iter_mut() {
            *p *= page.mm_per_pixel;
        }
        for &p in &flat {
            // Each test is written so that NaN fails it.
            if let Some(m) = &marks {
                let area = m.cut_area();
                let inside = p.x >= area.min.x - EDGE_MM
                    && p.y >= area.min.y - EDGE_MM
                    && p.x <= area.max.x + EDGE_MM
                    && p.y <= area.max.y + EDGE_MM;
                if !inside {
                    return Err(JobRefusal::OutsideCutArea);
                }
            } else {
                let on_page = p.x >= -EDGE_MM
                    && p.y >= -EDGE_MM
                    && p.x <= page.size_mm.x + EDGE_MM
                    && p.y <= media_length_mm + EDGE_MM;
                if !on_page {
                    return Err(JobRefusal::OutsidePage);
                }
                // With marks the page admitted them, so the cut area already
                // fits the machine; without, only a path can say it is too wide.
                if p.x > width_mm + EDGE_MM {
                    return Err(JobRefusal::PageTooWide);
                }
            }
        }

        let mut units: Vec<(i64, i64)> = Vec::with_capacity(flat.len());
        for &p in &flat {
            let q = (p - origin) * UNITS_PER_MM;
            // Clamped because a point admitted on the edge tolerance can round
            // to one unit outside, and a blade is never sent past its media.
            let u = (
                (q.y.round() as i64).clamp(0, max_units.0),
                (q.x.round() as i64).clamp(0, max_units.1),
            );
            if units.last() != Some(&u) {
                units.push(u);
            }
        }
        if units.len() < 2 {
            continue;
        }
        sent = sent.saturating_add(units.len().saturating_mul(passes));
        if sent > MAX_POINTS {
            return Err(JobRefusal::TooManyPoints);
        }
        if path.closed {
            closed.push((ring_area(&flat).abs(), units));
        } else {
            open.push(units);
        }
    }
    if closed.is_empty() && open.is_empty() {
        return Err(JobRefusal::NoPaths(settings.role));
    }
    // Stable, so equal areas keep the order they were drawn in.
    closed.sort_by(|a, b| a.0.total_cmp(&b.0));
    let paths: Vec<Vec<(i64, i64)>> = closed.into_iter().map(|(_, u)| u).chain(open).collect();

    let length_mm = paths
        .iter()
        .flat_map(|p| p.windows(2))
        .map(|w| {
            let d = Vec2::new((w[1].1 - w[0].1) as f32, (w[1].0 - w[0].0) as f32);
            d.length() / UNITS_PER_MM
        })
        .sum();

    Ok(Job {
        settings,
        media: (
            (media_length_mm * UNITS_PER_MM).round() as i64,
            (width_mm * UNITS_PER_MM).round() as i64,
        ),
        span: marks.map(|m| {
            (
                (m.span.y * UNITS_PER_MM).round() as i64,
                (m.span.x * UNITS_PER_MM).round() as i64,
            )
        }),
        paths,
        length_mm,
    })
}

/// Append one command and its terminator.
fn command(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(text.as_bytes());
    out.push(ETX);
}

impl Job {
    /// Media, force, speed and the tool, sent before anything moves.
    pub fn setup_block(&self) -> Vec<u8> {
        let s = &self.settings;
        let (len, wid) = self.media;
        // The blade sits behind the pen axis and has to be swung round a
        // corner; a pen does not, and offsetting one draws every corner wrong.
        let tool = match s.role {
            PathRole::Cut => "FC18,1,1",
            PathRole::Plot | PathRole::Ink => "FC0,1,1",
        };
        let mut out = Vec::new();
        for c in [
            "TG0".to_string(),
            "FN0".to_string(),
            "TB50,0".to_string(),
            "\\0,0".to_string(),
            format!("Z{len},{wid}"),
            format!("FX{},1", s.pressure),
            format!("!{},1", s.speed),
            tool.to_string(),
            "FE0,1".to_string(),
        ] {
            command(&mut out, &c);
        }
        out
    }

    /// The search for the registration marks, ending with the command that
    /// tells the machine how far apart they are. `None` for a job without marks.
    pub fn marks_block(&self) -> Option<Vec<u8>> {
        let (y, x) = self.span?;
        let mut out = Vec::new();
        for c in [
            "TB99".to_string(),
            "TB52,2".to_string(),
            "TB51,400".to_string(),
            "TB53,10".to_string(),
            "TB55,1".to_string(),
            format!("TB123,{y},{x},0,0"),
        ] {
            command(&mut out, &c);
        }
        Some(out)
    }

    /// Every path, each followed whole `passes` times, cut into blocks of at
    /// most [`BLOCK_BYTES`] that each end on a command's ETX.
    pub fn path_blocks(&self) -> Vec<Vec<u8>> {
        let mut blocks = Vec::new();
        let mut block = Vec::new();
        let mut cmd = Vec::new();
        for path in &self.paths {
            for _ in 0..self.settings.passes {
                for (i, (y, x)) in path.iter().enumerate() {
                    cmd.clear();
                    let verb = if i == 0 { 'M' } else { 'D' };
                    command(&mut cmd, &format!("{verb}{y},{x}"));
                    if block.len() + cmd.len() > BLOCK_BYTES {
                        blocks.push(std::mem::take(&mut block));
                    }
                    block.extend_from_slice(&cmd);
                }
            }
        }
        if !block.is_empty() {
            blocks.push(block);
        }
        blocks
    }

    /// Lift and go home.
    pub fn end_block(&self) -> Vec<u8> {
        let mut out = Vec::new();
        command(&mut out, "M0,0");
        out
    }

    /// Paths that will be sent, each counted once whatever the passes.
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    /// How far the tool travels down over one pass of every path.
    pub fn length_mm(&self) -> f32 {
        self.length_mm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{Node, VectorObject};

    /// The brief's notation: `·` is ETX.
    fn etx(s: &str) -> Vec<u8> {
        s.replace('·', "\u{3}").into_bytes()
    }

    /// A4 at 25.4 dpi, so one document pixel is exactly one millimetre and
    /// every coordinate below can be read as millimetres.
    fn a4(marks: bool) -> Page {
        let mut doc = Document::new(210, 297).with_marks(marks);
        doc.dpi = 25.4;
        Page::of(&doc)
    }

    fn page_mm(w: u32, h: u32, marks: bool) -> Page {
        let mut doc = Document::new(w, h).with_marks(marks);
        doc.dpi = 25.4;
        Page::of(&doc)
    }

    fn path(role: PathRole, closed: bool, pts: &[(f32, f32)]) -> Path {
        Path {
            role,
            closed,
            nodes: pts
                .iter()
                .map(|&(x, y)| Node::corner(Vec2::new(x, y)))
                .collect(),
            ink: None,
        }
    }

    fn square(role: PathRole, lo: f32, hi: f32) -> Path {
        path(role, true, &[(lo, lo), (hi, lo), (hi, hi), (lo, hi)])
    }

    fn joined(job: &Job) -> Vec<u8> {
        job.path_blocks().concat()
    }

    fn cut(use_marks: bool) -> JobSettings {
        JobSettings {
            use_marks,
            ..JobSettings::new(PathRole::Cut)
        }
    }

    /// Every point of every path command, as machine units `(y, x)`.
    fn points(bytes: &[u8]) -> Vec<(i64, i64)> {
        bytes
            .split(|&b| b == ETX)
            .filter(|c| !c.is_empty())
            .map(|c| {
                let s = std::str::from_utf8(c).unwrap();
                assert!(s.starts_with('M') || s.starts_with('D'), "{s}");
                let (y, x) = s[1..].split_once(',').unwrap();
                (y.parse().unwrap(), x.parse().unwrap())
            })
            .collect()
    }

    #[test]
    fn a_square_becomes_these_exact_bytes() {
        let sq = square(PathRole::Cut, 20.0, 30.0);
        let job = plan(&[&sq], &JobSettings::new(PathRole::Cut), &a4(true)).unwrap();
        assert_eq!(
            job.setup_block(),
            etx("TG0·FN0·TB50,0·\\0,0·Z5940,4060·FX10,1·!5,1·FC18,1,1·FE0,1·")
        );
        assert_eq!(
            job.marks_block(),
            Some(etx(
                "TB99·TB52,2·TB51,400·TB53,10·TB55,1·TB123,5540,3800,0,0·"
            ))
        );
        assert_eq!(
            job.path_blocks(),
            vec![etx("M200,200·D200,400·D400,400·D400,200·D200,200·")]
        );
        assert_eq!(job.end_block(), etx("M0,0·"));
        assert_eq!(job.path_count(), 1);
        assert!((job.length_mm() - 40.0).abs() < 1e-3, "{}", job.length_mm());
    }

    #[test]
    fn without_marks_coordinates_are_from_the_page_corner() {
        let sq = square(PathRole::Cut, 20.0, 30.0);
        let job = plan(&[&sq], &cut(false), &a4(true)).unwrap();
        assert_eq!(job.marks_block(), None);
        assert_eq!(
            joined(&job),
            etx("M400,400·D400,600·D600,600·D600,400·D400,400·")
        );
    }

    #[test]
    fn a_plot_job_has_no_blade_offset() {
        let sq = square(PathRole::Plot, 20.0, 30.0);
        let job = plan(&[&sq], &JobSettings::new(PathRole::Plot), &a4(true)).unwrap();
        let setup = job.setup_block();
        assert_eq!(
            setup,
            etx("TG0·FN0·TB50,0·\\0,0·Z5940,4060·FX5,1·!8,1·FC0,1,1·FE0,1·")
        );
        assert!(!setup.windows(4).any(|w| w == b"FC18"));
    }

    #[test]
    fn a_point_is_sent_y_first() {
        let line = path(PathRole::Cut, false, &[(10.0, 30.0), (50.0, 30.0)]);
        let job = plan(&[&line], &cut(false), &a4(false)).unwrap();
        assert_eq!(joined(&job), etx("M600,200·D600,1000·"));
    }

    #[test]
    fn passes_repeat_each_path_whole() {
        let a = path(PathRole::Cut, false, &[(10.0, 30.0), (50.0, 30.0)]);
        let b = path(PathRole::Cut, false, &[(10.0, 40.0), (20.0, 40.0)]);
        let settings = JobSettings {
            passes: 2,
            ..cut(false)
        };
        let job = plan(&[&a, &b], &settings, &a4(false)).unwrap();
        assert_eq!(
            joined(&job),
            etx("M600,200·D600,1000·M600,200·D600,1000·M800,200·D800,400·M800,200·D800,400·")
        );
        assert_eq!(job.path_count(), 2);
    }

    #[test]
    fn small_closed_paths_are_cut_before_large_ones() {
        let line = path(PathRole::Cut, false, &[(100.0, 200.0), (110.0, 200.0)]);
        let big = square(PathRole::Cut, 20.0, 80.0);
        let small = square(PathRole::Cut, 40.0, 50.0);
        // Wound the other way, so its signed area is negative: the order has
        // to be by size and not by sign.
        let mut mid = square(PathRole::Cut, 30.0, 60.0);
        mid.nodes.reverse();
        let job = plan(&[&line, &big, &mid, &small], &cut(false), &a4(false)).unwrap();
        let moves: Vec<(i64, i64)> = joined(&job)
            .split(|&b| b == ETX)
            .filter(|c| c.first() == Some(&b'M'))
            .flat_map(points)
            .collect();
        assert_eq!(
            moves,
            vec![(800, 800), (1200, 600), (400, 400), (4000, 2000)]
        );
    }

    #[test]
    fn a_curve_is_flattened_within_a_twentieth_of_a_millimetre() {
        let (c, r) = (Vec2::new(100.0, 100.0), 30.0f32);
        let k = 0.552_284_8 * r;
        let node = |at: Vec2, tangent: Vec2| Node {
            at,
            handle_in: -tangent * k,
            handle_out: tangent * k,
            pressure: 1.0,
        };
        let circle = Path {
            role: PathRole::Cut,
            closed: true,
            nodes: vec![
                node(c + Vec2::new(r, 0.0), Vec2::Y),
                node(c + Vec2::new(0.0, r), -Vec2::X),
                node(c + Vec2::new(-r, 0.0), -Vec2::Y),
                node(c + Vec2::new(0.0, -r), Vec2::X),
            ],
            ink: None,
        };
        let job = plan(&[&circle], &cut(false), &a4(false)).unwrap();
        let pts = points(&joined(&job));
        assert!(pts.len() > 16, "{}", pts.len());
        for (y, x) in pts {
            let p = Vec2::new(x as f32, y as f32) / UNITS_PER_MM;
            let off = (p.distance(c) - r).abs();
            assert!(off < 0.1, "{p:?} is {off} mm off the circle");
        }
    }

    #[test]
    fn blocks_never_split_a_command_and_never_exceed_the_limit() {
        let pts: Vec<(f32, f32)> = (0..600)
            .map(|i| {
                (
                    10.0 + (i % 150) as f32,
                    10.0 + (i / 150) as f32 * 0.5 + (i % 2) as f32,
                )
            })
            .collect();
        let long = path(PathRole::Cut, false, &pts);
        let settings = JobSettings {
            passes: 3,
            ..cut(false)
        };
        let job = plan(&[&long], &settings, &a4(false)).unwrap();
        let blocks = job.path_blocks();
        assert!(blocks.len() > 3, "{}", blocks.len());
        for b in &blocks {
            assert!(b.len() <= BLOCK_BYTES, "{}", b.len());
            assert_eq!(b.last(), Some(&ETX));
        }
        // The unsplit stream, rebuilt from the points it carries.
        let all = blocks.concat();
        let p = points(&all);
        let mut expect = Vec::new();
        for (i, (y, x)) in p.iter().enumerate() {
            let starts = i % (p.len() / 3) == 0;
            let cmd = if starts { 'M' } else { 'D' };
            expect.extend(format!("{cmd}{y},{x}\u{3}").into_bytes());
        }
        assert_eq!(all, expect);
        assert_eq!(p.len(), 3 * 600);
    }

    #[test]
    fn a_path_outside_the_cut_area_refuses_the_whole_job() {
        let inside = square(PathRole::Cut, 20.0, 30.0);
        let outside = square(PathRole::Cut, 5.0, 30.0);
        assert_eq!(
            plan(&[&inside, &outside], &cut(true), &a4(true)).unwrap_err(),
            JobRefusal::OutsideCutArea
        );
        // Exactly on the boundary is inside.
        let edge = path(
            PathRole::Cut,
            true,
            &[(10.0, 10.0), (200.0, 10.0), (200.0, 287.0)],
        );
        assert!(plan(&[&edge], &cut(true), &a4(true)).is_ok());
        // And without marks the same path is merely on the page.
        assert!(plan(&[&outside], &cut(false), &a4(true)).is_ok());
    }

    #[test]
    fn a_path_outside_the_page_is_refused_without_marks() {
        let off = path(PathRole::Cut, false, &[(-1.0, 20.0), (30.0, 20.0)]);
        assert_eq!(
            plan(&[&off], &cut(false), &a4(false)).unwrap_err(),
            JobRefusal::OutsidePage
        );
        let below = path(PathRole::Cut, false, &[(20.0, 20.0), (20.0, 298.0)]);
        assert_eq!(
            plan(&[&below], &cut(false), &a4(false)).unwrap_err(),
            JobRefusal::OutsidePage
        );
        // A4 is wider than the machine, so the far corner it can reach is 203 mm
        // across rather than the page's own 210.
        let edge = path(PathRole::Cut, false, &[(0.0, 0.0), (203.0, 297.0)]);
        let job = plan(&[&edge], &cut(false), &a4(false)).unwrap();
        assert_eq!(joined(&job), etx("M0,0·D5940,4060·"));
    }

    #[test]
    fn a_wide_page_is_refused_only_where_a_path_reaches_past_the_machine() {
        let wide = page_mm(300, 300, false);
        let near = path(PathRole::Cut, false, &[(20.0, 20.0), (203.0, 20.0)]);
        let job = plan(&[&near], &cut(false), &wide).unwrap();
        assert_eq!(
            job.setup_block(),
            etx("TG0·FN0·TB50,0·\\0,0·Z6000,4060·FX10,1·!5,1·FC18,1,1·FE0,1·")
        );
        let far = path(PathRole::Cut, false, &[(20.0, 20.0), (250.0, 20.0)]);
        assert_eq!(
            plan(&[&far], &cut(false), &wide).unwrap_err(),
            JobRefusal::PageTooWide
        );
    }

    #[test]
    fn marks_asked_for_on_a_document_without_them_is_refused() {
        let sq = square(PathRole::Cut, 20.0, 30.0);
        assert_eq!(
            plan(&[&sq], &cut(true), &a4(false)).unwrap_err(),
            JobRefusal::NoMarks
        );
        // A document that asks for marks on a page too wide to carry them has none.
        let wide = page_mm(300, 300, true);
        assert!(wide.marks.is_none());
        assert_eq!(
            plan(&[&sq], &cut(true), &wide).unwrap_err(),
            JobRefusal::NoMarks
        );
    }

    #[test]
    fn no_paths_of_the_role_is_refused() {
        assert_eq!(
            plan(&[], &cut(true), &a4(true)).unwrap_err(),
            JobRefusal::NoPaths(PathRole::Cut)
        );
        let plotted = square(PathRole::Plot, 20.0, 30.0);
        assert_eq!(
            plan(&[&plotted], &cut(true), &a4(true)).unwrap_err(),
            JobRefusal::NoPaths(PathRole::Cut)
        );
        let cutting = square(PathRole::Cut, 20.0, 30.0);
        assert_eq!(
            plan(&[&cutting], &JobSettings::new(PathRole::Plot), &a4(true)).unwrap_err(),
            JobRefusal::NoPaths(PathRole::Plot)
        );
        let ink = square(PathRole::Ink, 20.0, 30.0);
        assert_eq!(
            plan(&[&ink], &JobSettings::new(PathRole::Ink), &a4(true)).unwrap_err(),
            JobRefusal::InkRole
        );
    }

    #[test]
    fn a_degenerate_path_is_dropped_and_alone_refuses() {
        let speck = path(
            PathRole::Cut,
            true,
            &[(20.0, 20.0), (20.01, 20.0), (20.01, 20.01)],
        );
        let lone = path(PathRole::Cut, false, &[(20.0, 20.0)]);
        assert_eq!(
            plan(&[&speck, &lone], &cut(true), &a4(true)).unwrap_err(),
            JobRefusal::NoPaths(PathRole::Cut)
        );
        let sq = square(PathRole::Cut, 20.0, 30.0);
        let job = plan(&[&speck, &sq, &lone], &cut(true), &a4(true)).unwrap();
        assert_eq!(job.path_count(), 1);
        assert_eq!(
            joined(&job),
            etx("M200,200·D200,400·D400,400·D400,200·D200,200·")
        );
    }

    #[test]
    fn consecutive_points_in_one_machine_unit_are_merged() {
        let p = path(
            PathRole::Cut,
            false,
            &[(10.0, 30.0), (10.01, 30.0), (50.0, 30.0), (50.0, 30.02)],
        );
        let job = plan(&[&p], &cut(false), &a4(false)).unwrap();
        assert_eq!(joined(&job), etx("M600,200·D600,1000·"));
    }

    #[test]
    fn too_many_points_is_refused() {
        let pts: Vec<(f32, f32)> = (0..=MAX_POINTS)
            .map(|i| (20.0 + (i % 2) as f32, 20.0 + (i % 100) as f32))
            .collect();
        let many = path(PathRole::Cut, false, &pts);
        assert_eq!(
            plan(&[&many], &cut(true), &a4(true)).unwrap_err(),
            JobRefusal::TooManyPoints
        );
        // Passes count, because every one of them is sent.
        let half: Vec<(f32, f32)> = pts[..MAX_POINTS / 2 + 1].to_vec();
        let half = path(PathRole::Cut, false, &half);
        assert!(plan(&[&half], &cut(true), &a4(true)).is_ok());
        let twice = JobSettings {
            passes: 2,
            ..cut(true)
        };
        assert_eq!(
            plan(&[&half], &twice, &a4(true)).unwrap_err(),
            JobRefusal::TooManyPoints
        );
    }

    #[test]
    fn settings_are_clamped_into_what_the_machine_takes() {
        let line = path(PathRole::Cut, false, &[(10.0, 30.0), (50.0, 30.0)]);
        let low = JobSettings {
            pressure: 0,
            speed: 0,
            passes: 0,
            ..cut(false)
        };
        let job = plan(&[&line], &low, &a4(false)).unwrap();
        assert_eq!(
            job.setup_block(),
            etx("TG0·FN0·TB50,0·\\0,0·Z5940,4060·FX1,1·!1,1·FC18,1,1·FE0,1·")
        );
        assert_eq!(joined(&job), etx("M600,200·D600,1000·"));
        let high = JobSettings {
            pressure: 200,
            speed: 99,
            passes: 9,
            ..cut(false)
        };
        let job = plan(&[&line], &high, &a4(false)).unwrap();
        assert_eq!(
            job.setup_block(),
            etx("TG0·FN0·TB50,0·\\0,0·Z5940,4060·FX33,1·!10,1·FC18,1,1·FE0,1·")
        );
        assert_eq!(joined(&job), etx(&"M600,200·D600,1000·".repeat(5)));
    }

    #[test]
    fn the_defaults_are_these() {
        let c = JobSettings::new(PathRole::Cut);
        assert_eq!(
            (c.use_marks, c.pressure, c.speed, c.passes),
            (true, 10, 5, 1)
        );
        let p = JobSettings::new(PathRole::Plot);
        assert_eq!(
            (p.use_marks, p.pressure, p.speed, p.passes),
            (true, 5, 8, 1)
        );
        let i = JobSettings::new(PathRole::Ink);
        assert_eq!(
            (i.use_marks, i.pressure, i.speed, i.passes),
            (true, 5, 8, 1)
        );
    }

    #[test]
    fn every_refusal_has_its_own_sentence() {
        // Exhaustive by position: a new variant fails to compile here until it
        // is given a place in `all`.
        fn place(r: &JobRefusal) -> usize {
            match r {
                JobRefusal::NoPaths(role) => match role {
                    PathRole::Cut => 0,
                    PathRole::Plot => 1,
                    PathRole::Ink => 2,
                },
                JobRefusal::InkRole => 3,
                JobRefusal::OutsideCutArea => 4,
                JobRefusal::OutsidePage => 5,
                JobRefusal::PageTooWide => 6,
                JobRefusal::NoMarks => 7,
                JobRefusal::TooManyPoints => 8,
            }
        }
        let all = [
            JobRefusal::NoPaths(PathRole::Cut),
            JobRefusal::NoPaths(PathRole::Plot),
            JobRefusal::NoPaths(PathRole::Ink),
            JobRefusal::InkRole,
            JobRefusal::OutsideCutArea,
            JobRefusal::OutsidePage,
            JobRefusal::PageTooWide,
            JobRefusal::NoMarks,
            JobRefusal::TooManyPoints,
        ];
        let mut seen = std::collections::HashSet::new();
        for (i, r) in all.iter().enumerate() {
            assert_eq!(place(r), i);
            let s = r.sentence();
            assert!(!s.is_empty(), "{r:?}");
            assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'), "{s}");
            assert!(s.is_ascii(), "{s}");
            assert!(seen.insert(s.clone()), "{r:?} repeats {s}");
        }
    }

    #[test]
    fn paths_of_skips_hidden_layers_and_the_other_role() {
        let mut stack = LayerStack::new();
        let a = square(PathRole::Cut, 20.0, 30.0);
        let b = square(PathRole::Plot, 20.0, 30.0);
        let c = square(PathRole::Cut, 40.0, 50.0);
        let d = square(PathRole::Cut, 60.0, 70.0);
        stack
            .add_vector_layer("one", VectorObject::Paths(vec![a.clone(), b.clone()]))
            .unwrap();
        let hidden = stack
            .add_vector_layer("two", VectorObject::Paths(vec![d.clone()]))
            .unwrap();
        stack
            .add_vector_layer("three", VectorObject::Paths(vec![c.clone()]))
            .unwrap();
        stack.get_mut(hidden).unwrap().visible = false;
        assert_eq!(paths_of(&stack, PathRole::Cut), vec![&a, &c]);
        assert_eq!(paths_of(&stack, PathRole::Plot), vec![&b]);
        assert!(paths_of(&stack, PathRole::Ink).is_empty());
    }

    #[test]
    fn a_page_is_measured_from_its_document() {
        let page = a4(true);
        assert_eq!(page.size_mm, Vec2::new(210.0, 297.0));
        assert_eq!(page.mm_per_pixel, 1.0);
        assert_eq!(page.marks.unwrap().span, Vec2::new(190.0, 277.0));
        assert!(a4(false).marks.is_none());
    }
}
