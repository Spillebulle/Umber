//! Registration marks: where a cutter is told to look on a printed sheet.
//!
//! A print-and-cut machine reads a square and two L shaped corners off the
//! paper to learn where the print really sits, then moves the cut paths by the
//! difference. So the marks are not decoration. Their positions are numbers the
//! machine is *told* (the span between them), and the page has to hold them
//! exactly there.
//!
//! This module is that arithmetic and nothing else: millimetres from the page's
//! top left, no pixels, no PDF, no cutter. The sheet writer draws `fills` and
//! the cut command is built from `span`, so the two cannot disagree about where
//! a mark is.
//!
//! The geometry follows the Silhouette convention: the square sits at the top
//! left `INSET_MM` in from both edges, and the two Ls sit at the other two
//! corners of the rectangle the machine is told about, so the *span* is the page
//! less an inset on every side. An L is drawn as two thin filled rectangles
//! rather than a stroked path so that a viewer's line caps and joins cannot move
//! the corner the sensor is aiming at.

use glam::Vec2;

use crate::document::{Document, sane_dpi};

/// Distance from each page edge to the registration marks, in millimetres.
pub const INSET_MM: f32 = 10.0;
/// Side of the square mark.
pub const SQUARE_MM: f32 = 5.0;
/// Length of each arm of an L.
pub const ARM_MM: f32 = 20.0;
/// Thickness of an arm.
pub const STROKE_MM: f32 = 0.5;
/// Smallest span the machine will search for marks across.
pub const MIN_SPAN_MM: f32 = 60.0;
/// Widest span the machine can hold between marks on its narrowest mat feed.
pub const MAX_CUT_WIDTH_MM: f32 = 203.0;

/// A rectangle in millimetres from the page's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectMm {
    pub min: Vec2,
    pub max: Vec2,
}

impl RectMm {
    fn new(min: Vec2, max: Vec2) -> RectMm {
        RectMm { min, max }
    }

    /// Whether `other` lies wholly inside this rectangle, edges included.
    pub fn contains(&self, other: &RectMm) -> bool {
        other.min.x >= self.min.x
            && other.min.y >= self.min.y
            && other.max.x <= self.max.x
            && other.max.y <= self.max.y
    }
}

/// Why a page cannot carry marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoMarks {
    TooSmall,
    TooWide,
}

impl NoMarks {
    /// What to tell somebody, in a sentence with no symbols in it.
    pub fn sentence(self) -> &'static str {
        match self {
            NoMarks::TooSmall => {
                "This page is too small for registration marks. The cutter needs at least 60 mm between them."
            }
            NoMarks::TooWide => {
                "This page is too wide for the cutter. The marks can be at most 203 mm apart, so make the document narrower."
            }
        }
    }
}

/// Where the marks sit on one page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marks {
    /// Page size in millimetres.
    pub page: Vec2,
    /// Distance between the marks, which is what the machine is told.
    pub span: Vec2,
}

impl Marks {
    pub fn for_page(width_mm: f32, height_mm: f32) -> Result<Marks, NoMarks> {
        // Written so NaN fails the first test rather than slipping past both.
        let span = Vec2::new(width_mm - 2.0 * INSET_MM, height_mm - 2.0 * INSET_MM);
        let large_enough = span.x >= MIN_SPAN_MM && span.y >= MIN_SPAN_MM;
        if !large_enough {
            return Err(NoMarks::TooSmall);
        }
        if span.x > MAX_CUT_WIDTH_MM {
            return Err(NoMarks::TooWide);
        }
        Ok(Marks {
            page: Vec2::new(width_mm, height_mm),
            span,
        })
    }

    pub fn for_document(doc: &Document) -> Result<Marks, NoMarks> {
        let mm = mm_per_pixel(doc.dpi);
        Marks::for_page(doc.size.x as f32 * mm, doc.size.y as f32 * mm)
    }

    pub fn origin(&self) -> Vec2 {
        Vec2::splat(INSET_MM)
    }

    pub fn cut_area(&self) -> RectMm {
        RectMm::new(self.origin(), self.origin() + self.span)
    }

    /// The square, then two arms for the top right L, then two for the bottom
    /// left, each a filled rectangle.
    pub fn fills(&self) -> [RectMm; 5] {
        let area = self.cut_area();
        let o = area.min;
        let square = RectMm::new(o, o + Vec2::splat(SQUARE_MM));

        // Top right: arms run left and down from the corner.
        let c = Vec2::new(area.max.x, area.min.y);
        let tr_across = RectMm::new(c + Vec2::new(-ARM_MM, 0.0), c + Vec2::new(0.0, STROKE_MM));
        let tr_down = RectMm::new(c + Vec2::new(-STROKE_MM, 0.0), c + Vec2::new(0.0, ARM_MM));

        // Bottom left: arms run right and up from the corner.
        let c = Vec2::new(area.min.x, area.max.y);
        let bl_across = RectMm::new(c + Vec2::new(0.0, -STROKE_MM), c + Vec2::new(ARM_MM, 0.0));
        let bl_up = RectMm::new(c + Vec2::new(0.0, -ARM_MM), c + Vec2::new(STROKE_MM, 0.0));

        [square, tr_across, tr_down, bl_across, bl_up]
    }

    /// Squares around the three marked corners that artwork must stay out of,
    /// because the sensor reads anything dark there as part of the mark.
    pub fn keep_clear(&self) -> [RectMm; 3] {
        let area = self.cut_area();
        let side = Vec2::splat(ARM_MM);
        let top_left = area.min;
        let top_right = Vec2::new(area.max.x - ARM_MM, area.min.y);
        let bottom_left = Vec2::new(area.min.x, area.max.y - ARM_MM);
        [top_left, top_right, bottom_left].map(|min| RectMm::new(min, min + side))
    }
}

/// Millimetres in one document pixel at `dpi`.
pub fn mm_per_pixel(dpi: f32) -> f32 {
    25.4 / sane_dpi(dpi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a4() -> Marks {
        Marks::for_page(210.0, 297.0).unwrap()
    }

    fn touches(r: &RectMm, p: Vec2) -> bool {
        p.x >= r.min.x && p.x <= r.max.x && p.y >= r.min.y && p.y <= r.max.y
    }

    #[test]
    fn a4_marks_are_where_the_machine_is_told_they_are() {
        let m = a4();
        assert_eq!(m.span, Vec2::new(190.0, 277.0));
        let f = m.fills();
        assert_eq!(f[0].min, Vec2::new(10.0, 10.0));
        assert_eq!(f[0].max, Vec2::new(15.0, 15.0));
        let area = m.cut_area();
        for r in f {
            assert!(area.contains(&r), "{r:?} outside {area:?}");
        }
    }

    #[test]
    fn each_l_is_two_arms_meeting_at_its_corner() {
        let f = a4().fills();
        let size = |r: &RectMm| {
            let s = r.max - r.min;
            (s.x.max(s.y), s.x.min(s.y))
        };
        for arm in &f[1..3] {
            assert!(touches(arm, Vec2::new(200.0, 10.0)));
            assert_eq!(size(arm), (20.0, 0.5));
        }
        for arm in &f[3..5] {
            assert!(touches(arm, Vec2::new(10.0, 287.0)));
            assert_eq!(size(arm), (20.0, 0.5));
        }
    }

    #[test]
    fn a_page_too_small_or_too_wide_has_no_marks() {
        assert_eq!(Marks::for_page(79.9, 200.0), Err(NoMarks::TooSmall));
        assert!(Marks::for_page(80.0, 80.0).is_ok());
        assert_eq!(Marks::for_page(223.1, 297.0), Err(NoMarks::TooWide));
        assert!(Marks::for_page(223.0, 297.0).is_ok());
        assert_eq!(Marks::for_page(f32::NAN, 297.0), Err(NoMarks::TooSmall));
        assert_eq!(Marks::for_page(210.0, f32::NAN), Err(NoMarks::TooSmall));
        assert_eq!(Marks::for_page(-210.0, 297.0), Err(NoMarks::TooSmall));
        assert_eq!(Marks::for_page(210.0, -1.0), Err(NoMarks::TooSmall));
    }

    #[test]
    fn a_document_is_measured_by_its_resolution() {
        let mut doc = Document::new(2480, 3508);
        doc.dpi = 300.0;
        let m = Marks::for_document(&doc).unwrap();
        assert!((m.span.x - 189.97).abs() < 0.05, "{:?}", m.span);
        assert!((m.span.y - 277.01).abs() < 0.05, "{:?}", m.span);
    }

    #[test]
    fn keep_clear_covers_every_mark() {
        let m = a4();
        let clear = m.keep_clear();
        let area = m.cut_area();
        for q in clear {
            assert!(area.contains(&q));
        }
        for fill in m.fills() {
            assert!(
                clear.iter().any(|q| q.contains(&fill)),
                "{fill:?} is in no keep-clear square"
            );
        }
    }

    #[test]
    fn both_refusals_say_something_different_and_carry_no_dash() {
        let (a, b) = (NoMarks::TooSmall.sentence(), NoMarks::TooWide.sentence());
        assert_ne!(a, b);
        for s in [a, b] {
            assert!(!s.is_empty());
            assert!(!s.contains('\u{2014}'));
        }
    }
}
