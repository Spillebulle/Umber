//! Where a strip of controls drawn *over* the canvas goes.
//!
//! This is the **selection's** strip — Deselect, Copy, Cut — and deliberately
//! not a general placer for everything Umber draws over the canvas. The
//! floating transform's flip pair is the other such control and keeps a rule of
//! its own, in `ui.rs` where it is drawn: it sits above the box and is simply
//! *not offered* when the box has been dragged out from under it. That is right
//! there and wrong here, and the difference is the whole reason this module
//! exists rather than one shared function with a flag: a transform can be
//! dragged back into reach and a selection cannot, so a strip that declined to
//! appear would leave its commands with no control at all.
//!
//! What it is placing is a **rule**, testable without a window, in the same
//! division `Clip::place`, `CanvasCopy::plan` and `ScrollSpan` keep.
//!
//! The rule it exists to hold is one sentence: **a button drawn where it cannot
//! be clicked is worse than no button.** A selection can be scrolled half off
//! the view, pushed under a docked panel, dragged against the top of the window
//! or made larger than the whole viewport, and in every one of those the strip
//! still has to land somewhere the pointer can reach. So:
//!
//! * It is placed against the **visible** part of the thing it acts on — the
//!   intersection with the view — not against the whole of it. A selection
//!   three quarters off the left edge gets its strip over the quarter that can
//!   actually be seen, rather than centred on a midpoint that is off screen.
//! * It prefers to sit **above**, clear of the thing itself. For a selection
//!   that is also where a stroke would be clipped to nothing, so the strip
//!   costs no paintable canvas in the ordinary case.
//! * With no room above it goes **below**, which is what every application does
//!   with a popover pinned to something near the top of a window.
//! * With no room either side of it — a selection taller than the view — it
//!   goes **inside**, at the top. This is the one placement that covers canvas
//!   somebody might want to paint on, and it is the only alternative to not
//!   drawing the controls at all.
//! * Finally it is **clamped into the view** on both axes, which is what
//!   catches the near-corner cases the three placements above do not.
//!
//! Nothing here knows about egui, points or physical pixels: the caller works
//! in one space and passes every argument in it. The app passes egui points,
//! because that is what it draws in.

use crate::geom::Rect;
use glam::Vec2;

/// Which side of the thing it acts on a strip ended up on.
///
/// Reported rather than left to be inferred from the coordinates, so a test can
/// say *which of the three rules fired* instead of asserting a number that two
/// different rules could both have produced. No caller reads it today; the
/// tests are the reason it is here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Above,
    Below,
    /// Over the anchor itself, because there was no room on either side of it.
    Inside,
}

/// A placed strip: where it goes, and which rule put it there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strip {
    pub rect: Rect,
    pub side: Side,
}

/// Place a `size` strip against `anchor`, keeping it inside `view`.
///
/// `gap` is the clearance between the strip and the anchor. `None` when there
/// is nothing to place it against — an anchor entirely outside the view, or a
/// degenerate view or size. A strip floating in the middle of a canvas whose
/// selection has been scrolled out of sight would be a control pointing at
/// nothing, so the caller draws none.
pub fn place_strip(anchor: Rect, view: Rect, size: Vec2, gap: f32) -> Option<Strip> {
    if view.is_empty() || size.x <= 0.0 || size.y <= 0.0 {
        return None;
    }
    // A view too small to hold the strip has nowhere to put it. Pinning it to
    // the near edge and letting the rest hang off looks like the more helpful
    // answer and is not: the caller clips its painting to the view, so the
    // buttons that hang off are *invisible* and still live — which is the exact
    // shape of "a control drawn where it cannot be clicked" this module exists
    // to refuse. Nothing is lost by declining, since every command a strip
    // carries is also a keystroke.
    if size.x > view.size().x || size.y > view.size().y {
        return None;
    }
    let visible = anchor.intersection(&view)?;

    // Above, then below, then inside. Measured against the *visible* part: an
    // anchor whose top is scrolled off the view has no "above" to speak of, and
    // taking it from the anchor would put the strip off the top of the screen
    // for the clamp below to drag back down on top of the anchor anyway —
    // arriving at `Inside` by accident and reporting it as `Above`.
    let above = visible.min.y - gap - size.y;
    let below = visible.max.y + gap;
    let (y, side) = if above >= view.min.y {
        (above, Side::Above)
    } else if below + size.y <= view.max.y {
        (below, Side::Below)
    } else {
        (visible.min.y + gap, Side::Inside)
    };

    let min = Vec2::new(
        fit(
            visible.center().x - size.x * 0.5,
            size.x,
            view.min.x,
            view.max.x,
        ),
        fit(y, size.y, view.min.y, view.max.y),
    );
    Some(Strip {
        rect: Rect::new(min, min + size),
        side,
    })
}

/// Put a span of `len` starting at `start` inside `lo ..= hi`.
///
/// `place_strip` has already refused anything longer than the span, so the
/// guard covers only the exact fit — where `clamp`'s range would be empty and
/// it would panic on `lo > hi` rather than answer the one position available.
fn fit(start: f32, len: f32, lo: f32, hi: f32) -> f32 {
    if len >= hi - lo {
        return lo;
    }
    start.clamp(lo, hi - len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    /// A 200 × 100 viewport, which is what every case below is placed in.
    fn view() -> Rect {
        Rect::new(vec2(0.0, 0.0), vec2(200.0, 100.0))
    }

    fn strip() -> Vec2 {
        vec2(74.0, 22.0)
    }

    /// The ordinary case: clear of the thing it acts on, above it, and centred
    /// on it.
    #[test]
    fn a_strip_sits_centred_above_what_it_acts_on() {
        let anchor = Rect::new(vec2(60.0, 50.0), vec2(140.0, 90.0));
        let placed = place_strip(anchor, view(), strip(), 12.0).expect("somewhere to go");
        assert_eq!(placed.side, Side::Above);
        assert_eq!(placed.rect.min, vec2(100.0 - 37.0, 50.0 - 12.0 - 22.0));
        assert_eq!(placed.rect.size(), strip());
    }

    /// Pinned against the top of the view, there is no room above, so it goes
    /// below — rather than being drawn off the screen, or not drawn at all.
    #[test]
    fn a_strip_with_no_room_above_flips_below() {
        let anchor = Rect::new(vec2(60.0, 2.0), vec2(140.0, 40.0));
        let placed = place_strip(anchor, view(), strip(), 12.0).expect("somewhere to go");
        assert_eq!(placed.side, Side::Below);
        assert_eq!(placed.rect.min.y, 40.0 + 12.0);
    }

    /// An anchor taller than the view has no side to sit beside, so the strip
    /// goes over it. The one placement that costs paintable canvas, and the
    /// only alternative to offering no controls at all.
    #[test]
    fn an_anchor_larger_than_the_view_puts_the_strip_inside_it() {
        let anchor = Rect::new(vec2(-50.0, -80.0), vec2(250.0, 300.0));
        let placed = place_strip(anchor, view(), strip(), 12.0).expect("somewhere to go");
        assert_eq!(placed.side, Side::Inside);
        // Against the visible top, which is the view's, not the anchor's -80.
        assert_eq!(placed.rect.min.y, 12.0);
        // And centred on the visible part, which is the whole view.
        assert_eq!(placed.rect.center().x, 100.0);
    }

    /// The whole point of the module. An anchor mostly off the left edge is
    /// centred on the part that can be *seen*, and the strip is then pulled
    /// fully back inside the view — a button hanging off the edge is one nobody
    /// can press.
    #[test]
    fn a_strip_is_kept_on_screen_when_its_anchor_runs_off_an_edge() {
        let gap = 12.0;
        let left = Rect::new(vec2(-400.0, 50.0), vec2(20.0, 90.0));
        let placed = place_strip(left, view(), strip(), gap).expect("somewhere to go");
        assert!(placed.rect.min.x >= 0.0, "off the left: {:?}", placed.rect);
        assert!(placed.rect.max.x <= 200.0);
        // Centred on the visible 0..20 would start at -27; clamped to 0.
        assert_eq!(placed.rect.min.x, 0.0);

        let right = Rect::new(vec2(190.0, 50.0), vec2(600.0, 90.0));
        let placed = place_strip(right, view(), strip(), gap).expect("somewhere to go");
        assert_eq!(placed.rect.max.x, 200.0);

        // And on the other axis: an anchor whose bottom is off the view still
        // has a visible top to sit above.
        let low = Rect::new(vec2(60.0, 60.0), vec2(140.0, 400.0));
        let placed = place_strip(low, view(), strip(), gap).expect("somewhere to go");
        assert_eq!(placed.side, Side::Above);
        assert!(placed.rect.min.y >= 0.0 && placed.rect.max.y <= 100.0);
    }

    /// Scrolled out of sight entirely: no strip. There is nothing on screen for
    /// it to be a control *for*, and one floating over open canvas would be
    /// pointing at nothing.
    #[test]
    fn an_anchor_out_of_sight_has_no_strip() {
        let away = Rect::new(vec2(-900.0, -900.0), vec2(-800.0, -800.0));
        assert!(place_strip(away, view(), strip(), 12.0).is_none());
        let below = Rect::new(vec2(60.0, 400.0), vec2(140.0, 500.0));
        assert!(place_strip(below, view(), strip(), 12.0).is_none());
    }

    /// A view too narrow to hold the strip is offered none. Pinning it to the
    /// near edge looks kinder and is not: the caller clips its painting to the
    /// view, so whatever hangs off is an invisible live target — the one thing
    /// this module exists to refuse.
    #[test]
    fn a_view_too_small_to_hold_the_strip_is_offered_none() {
        let anchor = Rect::new(vec2(40.0, 50.0), vec2(60.0, 90.0));
        let narrow = Rect::new(vec2(30.0, 0.0), vec2(70.0, 100.0));
        assert!(place_strip(anchor, narrow, strip(), 12.0).is_none());
        let short = Rect::new(vec2(0.0, 40.0), vec2(200.0, 55.0));
        assert!(place_strip(anchor, short, strip(), 12.0).is_none());
        // And one exactly big enough still is: the refusal is a bound, not a
        // margin somebody has to guess at.
        let exact = Rect::new(vec2(0.0, 0.0), vec2(74.0, 22.0));
        let snug = Rect::new(vec2(40.0, 10.0), vec2(60.0, 20.0));
        assert!(place_strip(snug, exact, strip(), 12.0).is_some());
    }

    /// Degenerate inputs answer nothing rather than a rectangle at a NaN.
    #[test]
    fn a_view_or_a_strip_with_no_size_is_refused() {
        let anchor = Rect::new(vec2(10.0, 10.0), vec2(20.0, 20.0));
        assert!(place_strip(anchor, Rect::empty(), strip(), 12.0).is_none());
        assert!(place_strip(anchor, view(), Vec2::ZERO, 12.0).is_none());
    }
}
