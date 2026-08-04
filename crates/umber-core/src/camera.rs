//! Viewport transform: maps between document pixels and screen pixels.

use glam::Vec2;

/// The view onto the document.
///
/// `center` is the document-space point displayed at `pivot`, and `zoom` is
/// screen pixels per document pixel.
///
/// `pivot` is passed in rather than derived from the window size because the
/// canvas does not occupy the whole window — tool rails and panels take a bite
/// out of it, and the document should sit in the middle of what is left, not
/// the middle of the window.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub center: Vec2,
    pub zoom: f32,
}

/// What one screen pixel of drag along the zoom axis multiplies the zoom by.
///
/// Small because it is spent per pixel of a continuous gesture, where
/// `ZOOM_KEY_STEP` in `app.rs` is spent per keypress: about 90 pixels doubles
/// the zoom, which is a short sweep of the hand.
const ZOOM_DRAG_RATE: f32 = 1.008;

impl Camera {
    pub const MIN_ZOOM: f32 = 0.02;
    pub const MAX_ZOOM: f32 = 64.0;

    /// What a zoom-tool drag of `delta` screen pixels multiplies the zoom by.
    ///
    /// Right and up zoom in, left and down zoom out. How the two axes resolve
    /// into one signed distance is [`crate::geom::drag_towards_more`]'s, which
    /// the Alt-held brush resize asks the same question of; the rate that
    /// distance is spent at is this gesture's alone.
    pub fn zoom_drag_factor(delta: Vec2) -> f32 {
        ZOOM_DRAG_RATE.powf(crate::geom::drag_towards_more(delta))
    }

    /// Frame the whole document inside a region of `viewport` size.
    pub fn fit(doc_size: Vec2, viewport: Vec2) -> Self {
        let zoom = if doc_size.x > 0.0 && doc_size.y > 0.0 {
            (viewport.x / doc_size.x)
                .min(viewport.y / doc_size.y)
                .clamp(Self::MIN_ZOOM, Self::MAX_ZOOM)
                * 0.9
        } else {
            1.0
        };
        Self {
            center: doc_size * 0.5,
            zoom,
        }
    }

    pub fn doc_to_screen(&self, doc: Vec2, pivot: Vec2) -> Vec2 {
        (doc - self.center) * self.zoom + pivot
    }

    pub fn screen_to_doc(&self, screen: Vec2, pivot: Vec2) -> Vec2 {
        (screen - pivot) / self.zoom + self.center
    }

    /// Drag the canvas by a screen-space delta.
    pub fn pan_by_screen(&mut self, delta: Vec2) {
        self.center -= delta / self.zoom;
    }

    /// Zoom by `factor`, keeping the document point under `anchor` pinned.
    ///
    /// This is what makes wheel-zoom feel right: without the correction the
    /// canvas slides out from under the cursor.
    pub fn zoom_at(&mut self, anchor_screen: Vec2, factor: f32, pivot: Vec2) {
        let before = self.screen_to_doc(anchor_screen, pivot);
        self.zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        let after = self.screen_to_doc(anchor_screen, pivot);
        self.center += before - after;
    }
}

/// One axis of the canvas's scroll state, in document units.
///
/// The model behind the scrollbars: what fraction of the bar the thumb covers,
/// where it sits, and what a drag along the bar is worth. Here rather than in
/// the painting code for the reason `CanvasCopy::plan` is here — it is geometry,
/// it decides where the picture goes, and it is testable without a window.
///
/// The scrollable travel is the document plus one viewport, half a viewport
/// hanging off each end, so the camera centre ranges over exactly the document.
/// That is deliberately **not** "the document, or the view, whichever reaches
/// further": a span that grew as the view left the document would change under
/// the pointer mid-drag, and the thumb would accelerate away from the hand
/// holding it. This one depends on the zoom and the document, and not on where
/// the camera is — so a drag maps to a distance linearly and stays put.
///
/// **A bar exists wherever a document does**, and not only where part of the
/// picture is off the view. It used to be drawn on exactly that test, which
/// reads as the honest rule and is not: the travel above is the document plus a
/// viewport *at every zoom*, so a picture that fits in the window is still one
/// the camera can be moved a whole document's width across — the bar was hiding
/// travel it already had, and zooming out was the only way to a canvas that
/// could not be shifted off centre. What gave it away, from a running window,
/// is that one notch of the wheel made both bars appear out of nowhere: panning
/// is the moment the old test starts answering yes. A control offered only once
/// you have found another way to do the thing is not a control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollSpan {
    /// The document's size along this axis.
    pub doc: f32,
    /// How much of the document the view shows, in document units.
    pub extent: f32,
    /// The document coordinate the middle of the view sits on.
    pub centre: f32,
}

impl ScrollSpan {
    /// `viewport` is the visible size in *screen* pixels; `zoom` converts it.
    pub fn new(doc: f32, viewport: f32, zoom: f32, centre: f32) -> Self {
        Self {
            doc,
            extent: viewport / zoom.max(1e-6),
            centre,
        }
    }

    /// True when this bar has somewhere to go, and therefore when it is worth
    /// drawing — which, the travel being the document plus a viewport, is
    /// wherever there is a document at all.
    ///
    /// It reads a *property of the document*, never one of the camera: a rule
    /// that turned the bars on and off as the picture was pushed about would
    /// take the canvas region's last eleven points away and give them back
    /// under a hand that is painting in them.
    ///
    /// The consequence worth knowing is that [`ScrollSpan::thumb`] is then
    /// shorter than its own track, so this does not draw a full-length thumb
    /// meaning "nothing to scroll" — which was the other way this could have
    /// been spelled, and would have been a bar that says one thing and does
    /// another. It is shorter by the document's share of the travel, so at the
    /// smallest documents and the lowest zoom it is only *just* shorter — a
    /// 128-pixel canvas at [`Camera::MIN_ZOOM`] leaves two tenths of a percent
    /// of the track to drag in. That is honest rather than a gap: the document
    /// is under three screen pixels across and there is nowhere to go.
    pub fn scrollable(&self) -> bool {
        self.doc > 0.0
    }

    /// The total distance a full bar's worth of dragging covers.
    pub fn travel(&self) -> f32 {
        (self.doc + self.extent).max(1e-6)
    }

    /// The thumb as `(start, length)`, both fractions of the bar.
    ///
    /// Clamped into the bar. The camera can be dragged well outside the travel
    /// by other means — a space-drag has no such limit — and a thumb drawn
    /// outside its own track would be a worse answer than one pinned at the end
    /// that says "further that way".
    ///
    /// **The start maps the camera's range onto the track the thumb leaves
    /// free**, rather than onto the travel directly, and the difference is only
    /// visible once the `0.02` length floor bites — which is `extent < doc/49`,
    /// a large canvas at a working zoom rather than a curiosity. There
    /// `extent / travel` is *not* what the thumb is drawn at, so a start of
    /// `centre / travel` saturated while the camera still had a fiftieth of the
    /// document to go: the last screenfuls of a big canvas moved the picture
    /// under a thumb standing still, and the bar could not describe where you
    /// were. Against the free track the two ends line up by construction —
    /// `centre = 0` is `start = 0` and `centre = doc` is `start = 1 - length`,
    /// at every zoom. Wherever the floor does not bite `1 - length` is exactly
    /// `doc / travel`, so this is the same number the simpler form gave and
    /// nothing about the ordinary case changed.
    pub fn thumb(&self) -> (f32, f32) {
        let length = (self.extent / self.travel()).clamp(0.02, 1.0);
        let free = 1.0 - length;
        let start = if self.doc > 0.0 {
            (self.centre / self.doc * free).clamp(0.0, free)
        } else {
            0.0
        };
        (start, length)
    }

    /// How far the camera moves for a drag of `fraction` of the bar's length.
    ///
    /// The inverse of [`ScrollSpan::thumb`], and a plain multiply because the
    /// travel does not depend on where the camera is — that is the whole of the
    /// rule the type exists to hold, and the clamp below does not touch it.
    ///
    /// What the clamp does is stop a drag banking distance past the end of the
    /// track. `thumb` pins its start inside the bar, so without this a drag
    /// carried past an end goes on moving the *camera* under a thumb that has
    /// stopped, and every bit of that has to be spent again before the thumb
    /// moves on the way back: the picture slides while the hand is on a thumb
    /// standing still, which is the complaint the fixed travel exists to
    /// prevent, arrived at from the other side. It matters far more now the
    /// bars are drawn on a document that fits, because then the ends are a
    /// short drag away rather than somewhere nobody goes.
    ///
    /// The distance is the document spread over the track the thumb leaves
    /// free — the exact inverse of [`ScrollSpan::thumb`]'s start, so a drag of
    /// a tenth of the bar moves the thumb a tenth of the bar, at every zoom.
    /// Where the length floor does not bite, `doc / (1 - length)` is exactly
    /// `travel`, which is the plain multiply this used to be and the figure the
    /// type's own docs describe.
    ///
    /// The **bound is the document**, both ends. That is where the thumb's
    /// range now lines up, so neither end can move the picture under a thumb
    /// that has stopped. Bounding it short of the document was tried and was
    /// worse in the other direction: it put the last fiftieth of a large canvas
    /// out of the bar's reach entirely, which on a 16384 canvas at full zoom is
    /// thirteen screenfuls of the edge that could be seen and not scrolled to.
    ///
    /// A camera already outside that range — a space-drag has no limit, and
    /// neither does zooming in on a corner — is left where it is rather than
    /// snapped back in, so the bounds are widened to wherever it stands. The
    /// bar can then only ever improve matters, and a hair of a drag can never
    /// teleport a picture that was pushed a long way out. The widening does
    /// ratchet: a camera nudged inwards cannot be put back where it was by the
    /// bar, only by whatever took it out there.
    pub fn pan_by(&self, fraction: f32) -> f32 {
        let (_, length) = self.thumb();
        let free = 1.0 - length;
        if free <= 0.0 {
            return 0.0;
        }
        let lo = self.centre.min(0.0);
        let hi = self.centre.max(self.doc);
        (self.centre + fraction * self.doc / free).clamp(lo, hi) - self.centre
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    #[test]
    fn screen_and_doc_are_inverses() {
        let cam = Camera {
            center: vec2(100.0, 50.0),
            zoom: 2.5,
        };
        let pivot = vec2(400.0, 300.0);
        let doc = vec2(37.0, 129.0);
        let round = cam.screen_to_doc(cam.doc_to_screen(doc, pivot), pivot);
        assert!((round - doc).length() < 1e-3, "got {round:?}");
    }

    #[test]
    fn the_camera_centre_lands_on_the_pivot() {
        // The pivot is what puts the document in the middle of the *canvas
        // region* rather than the middle of the window.
        let cam = Camera {
            center: vec2(1024.0, 1024.0),
            zoom: 0.4,
        };
        // A viewport with a 48px rail on the left and a 264px panel on the
        // right: the free region's centre is not the window's centre.
        let pivot = vec2(48.0 + (1280.0 - 48.0 - 264.0) * 0.5, 360.0);
        let on_screen = cam.doc_to_screen(cam.center, pivot);
        assert!((on_screen - pivot).length() < 1e-3, "got {on_screen:?}");
    }

    #[test]
    fn zoom_keeps_anchor_pinned() {
        let mut cam = Camera {
            center: vec2(100.0, 100.0),
            zoom: 1.0,
        };
        let pivot = vec2(400.0, 300.0);
        let anchor = vec2(200.0, 150.0);
        let doc_before = cam.screen_to_doc(anchor, pivot);
        cam.zoom_at(anchor, 1.7, pivot);
        let doc_after = cam.screen_to_doc(anchor, pivot);
        assert!(
            (doc_before - doc_after).length() < 1e-3,
            "anchor drifted: {doc_before:?} -> {doc_after:?}"
        );
    }

    // --- the zoom tool's drag ----------------------------------------------

    #[test]
    fn dragging_right_or_up_zooms_in_and_the_other_way_out() {
        // The whole of what the gesture promises, in one place: two directions
        // in, two out, and nothing at all for a drag that has not moved.
        assert!(Camera::zoom_drag_factor(vec2(20.0, 0.0)) > 1.0);
        assert!(Camera::zoom_drag_factor(vec2(0.0, -20.0)) > 1.0, "up is in");
        assert!(Camera::zoom_drag_factor(vec2(-20.0, 0.0)) < 1.0);
        assert!(
            Camera::zoom_drag_factor(vec2(0.0, 20.0)) < 1.0,
            "screen y is down-positive, so down is out"
        );
        assert_eq!(Camera::zoom_drag_factor(Vec2::ZERO), 1.0);
    }

    #[test]
    fn the_two_axes_are_worth_the_same() {
        // Neither axis is the "real" one with the other bolted on.
        let across = Camera::zoom_drag_factor(vec2(17.0, 0.0));
        let up = Camera::zoom_drag_factor(vec2(0.0, -17.0));
        assert!((across - up).abs() < 1e-5, "{across} vs {up}");
    }

    #[test]
    fn a_diagonal_drag_is_worth_its_own_length_and_not_the_sum_of_its_axes() {
        // The reason the axes are not simply added. A 45° drag of 40 pixels
        // each way is 56 pixels of hand movement; adding would make it 80, so
        // the same sweep would zoom half again as fast for being made
        // diagonally.
        let diagonal = Camera::zoom_drag_factor(vec2(40.0, -40.0));
        let same_distance = Camera::zoom_drag_factor(vec2(40.0 * 2f32.sqrt(), 0.0));
        assert!(
            (diagonal - same_distance).abs() < 1e-4,
            "{diagonal} vs {same_distance}"
        );
        assert!(diagonal < Camera::zoom_drag_factor(vec2(80.0, 0.0)));
    }

    #[test]
    fn a_drag_along_the_neutral_diagonal_does_nothing() {
        // Down-right and up-left are exactly between "in" and "out", and a
        // gesture between two answers must not pick one.
        assert_eq!(Camera::zoom_drag_factor(vec2(30.0, 30.0)), 1.0);
        assert_eq!(Camera::zoom_drag_factor(vec2(-30.0, -30.0)), 1.0);
    }

    #[test]
    fn no_drag_is_worth_more_than_the_distance_the_hand_moved() {
        // What stops any direction from being a fast lane. The bound is the
        // pure horizontal drag, which is what the gesture was before the
        // vertical axis was taken in — so nothing got faster.
        let bound = Camera::zoom_drag_factor(vec2(50.0, 0.0));
        for step in 0..64 {
            let angle = step as f32 * std::f32::consts::TAU / 64.0;
            let factor = Camera::zoom_drag_factor(vec2(angle.cos(), angle.sin()) * 50.0);
            assert!(factor <= bound + 1e-4, "{angle} gave {factor}");
            assert!(factor >= 1.0 / bound - 1e-4, "{angle} gave {factor}");
        }
    }

    #[test]
    fn a_horizontal_drag_keeps_the_rate_it_has_always_had() {
        // Pinned so the two axes cannot be combined by quietly slowing the one
        // that was already there.
        assert!((Camera::zoom_drag_factor(vec2(12.0, 0.0)) - 1.008f32.powf(12.0)).abs() < 1e-5);
    }

    // --- scrollbars --------------------------------------------------------

    /// A 1000-pixel document under a 500-pixel viewport at zoom 1, centred.
    fn half_shown() -> ScrollSpan {
        ScrollSpan::new(1000.0, 500.0, 1.0, 500.0)
    }

    /// The same document at a zoom that puts the whole of it on screen twice
    /// over — the case the bars used to refuse to draw at all.
    fn zoomed_out() -> ScrollSpan {
        ScrollSpan::new(1000.0, 2000.0, 1.0, 500.0)
    }

    #[test]
    fn a_document_that_fits_still_gets_a_bar() {
        // The report this was changed for: the whole picture is in the window,
        // so nothing is hidden — and the camera can still be moved a whole
        // document across, which is travel the old rule hid.
        assert!(zoomed_out().scrollable());
        assert!(half_shown().scrollable());
    }

    #[test]
    fn a_bar_that_is_drawn_can_always_be_dragged_somewhere() {
        // The pair the drawing rule turns on: a bar is offered exactly where
        // there is somewhere to go, so it can never be a control that does
        // nothing. Both directions are checked, because "moves" has to hold
        // whichever end the camera starts nearer.
        for span in [half_shown(), zoomed_out()] {
            assert!(span.scrollable());
            assert!(span.pan_by(0.1) > 0.0, "{span:?} would not move forwards");
            assert!(span.pan_by(-0.1) < 0.0, "{span:?} would not move back");
        }
    }

    #[test]
    fn the_thumb_never_fills_a_bar_that_is_drawn() {
        // What makes "always drawn" honest rather than a full-length thumb
        // sitting there meaning nothing. Swept over the zoom range a canvas
        // this size is actually looked at through.
        for zoom in [0.02f32, 0.1, 0.25, 1.0, 4.0, 64.0] {
            let span = ScrollSpan::new(4000.0, 1600.0, zoom, 2000.0);
            assert!(span.scrollable());
            let (_, length) = span.thumb();
            assert!(length < 0.999, "at zoom {zoom} the thumb was {length}");
        }
    }

    #[test]
    fn a_zoomed_out_bar_still_reaches_the_whole_document() {
        // Dragging the thumb from one end of its track to the other has to put
        // the corner of a fitted picture in the middle of the view — otherwise
        // "the bars are always there" would be true and useless.
        let span = zoomed_out();
        let (start, length) = span.thumb();
        let reach = span.pan_by(1.0 - length - start);
        assert!(
            (span.centre + reach - span.doc).abs() < 1e-3,
            "reached {} of {}",
            span.centre + reach,
            span.doc
        );
    }

    #[test]
    fn a_document_pushed_off_the_side_keeps_its_bar() {
        // The document is smaller than the window and has been panned until
        // half of it is under a panel. This was the case the old rule existed
        // for, and it must go on working.
        let span = ScrollSpan::new(1000.0, 2000.0, 1.0, 1800.0);
        assert!(span.scrollable());
        let (start, length) = span.thumb();
        assert!((start + length - 1.0).abs() < 1e-4, "{start} + {length}");
    }

    #[test]
    fn the_thumb_covers_the_share_of_the_document_on_screen() {
        // 500 of a 1500-unit travel — the document plus one viewport.
        let (start, length) = half_shown().thumb();
        assert!((length - 1.0 / 3.0).abs() < 1e-4, "got {length}");
        // Centred on the middle of the document is centred on the bar.
        assert!((start - (0.5 - length / 2.0)).abs() < 1e-4, "got {start}");
    }

    #[test]
    fn the_thumb_reaches_each_end_and_stops() {
        // Centre at 0 is the top-left corner of the document in the middle of
        // the view — the far end of the travel, and as far as the bar goes.
        let mut span = half_shown();
        span.centre = 0.0;
        assert_eq!(span.thumb().0, 0.0);

        span.centre = 1000.0;
        let (start, length) = span.thumb();
        assert!(
            (start + length - 1.0).abs() < 1e-4,
            "got {start} + {length}"
        );

        // And beyond, where a space-drag can put it: pinned, never outside.
        span.centre = 100_000.0;
        let (start, length) = span.thumb();
        assert!(start >= 0.0 && start + length <= 1.0 + 1e-4);
    }

    #[test]
    fn a_drag_along_the_whole_bar_covers_the_whole_travel() {
        // What makes the thumb follow the pointer instead of drifting from it.
        let span = half_shown();
        let (before, _) = span.thumb();
        let moved = ScrollSpan {
            centre: span.centre + span.pan_by(0.1),
            ..span
        };
        assert!((moved.thumb().0 - (before + 0.1)).abs() < 1e-4);
    }

    #[test]
    fn the_travel_does_not_move_as_the_camera_does() {
        // The reason the span is the document plus a viewport rather than the
        // union of the document and the view: a travel that grew as the camera
        // left the document would change under the pointer mid-drag, and the
        // thumb would accelerate away from the hand holding it.
        let span = half_shown();
        let far = ScrollSpan {
            centre: -5000.0,
            ..span
        };
        assert_eq!(span.travel(), far.travel());

        // And the same on a document that fits, which is where the bars are
        // now drawn and where the temptation to make the span "the document or
        // the view, whichever reaches further" is strongest.
        let fitted = zoomed_out();
        let pushed = ScrollSpan {
            centre: -5000.0,
            ..fitted
        };
        assert_eq!(fitted.travel(), pushed.travel());
    }

    #[test]
    fn a_drag_stops_at_the_end_of_the_track_instead_of_banking_distance() {
        // Without this the camera goes on moving under a thumb that has
        // stopped, and the overshoot has to be paid back before the picture
        // moves at all on the way home — the pointer travelling while the thumb
        // stands still. Reachable in a couple of centimetres now the bars are
        // drawn on a document that fits.
        let span = zoomed_out();
        let hard = span.pan_by(10.0);
        assert!(
            (span.centre + hard - span.doc).abs() < 1e-3,
            "landed at {}",
            span.centre + hard
        );
        let back = span.pan_by(-10.0);
        assert!(
            (span.centre + back).abs() < 1e-3,
            "landed at {}",
            span.centre + back
        );
    }

    #[test]
    fn the_thumb_and_the_camera_reach_their_ends_together_at_the_length_floor() {
        // The case the `0.02` length floor creates, and it caught two opposite
        // bugs on the way to this, so it asserts against both ends.
        //
        // These are not extreme numbers: 16384 square is `Document::MAX_EDGE`
        // and 1440 physical pixels at zoom 64 is somebody looking closely at a
        // large canvas. Draw the thumb's start off the *travel* and it
        // saturates a fiftieth of the document early, so the last thirteen
        // screenfuls move the picture under a thumb standing still. Bound the
        // drag short to match, and those thirteen screenfuls become unreachable
        // by the bar at all. Against the free track both ends line up.
        let span = ScrollSpan::new(16384.0, 1440.0, 64.0, 8192.0);
        let (_, length) = span.thumb();
        assert_eq!(length, 0.02, "this test needs the floored case to bite");

        let far = ScrollSpan {
            centre: span.centre + span.pan_by(10.0),
            ..span
        };
        // The bar reaches the far edge of the document — not a fraction short.
        assert!(
            (far.centre - far.doc).abs() < 1e-2,
            "the bar reached {} of {}",
            far.centre,
            far.doc
        );
        // And the thumb is at its own end there rather than having got there
        // first: the two agree.
        assert!(
            (far.thumb().0 - (1.0 - length)).abs() < 1e-4,
            "the thumb was at {} where its end is {}",
            far.thumb().0,
            1.0 - length
        );
        // Nothing was banked on the way out, so the smallest drag home moves
        // the thumb at once.
        let back = ScrollSpan {
            centre: far.centre + far.pan_by(-0.001),
            ..far
        };
        assert!(
            back.thumb().0 < far.thumb().0,
            "the thumb did not follow the hand home"
        );
    }

    #[test]
    fn the_thumb_describes_a_camera_no_gesture_but_the_bar_put_there() {
        // The bar is not the only thing that moves the camera — `zoom_at` pulls
        // the centre towards the anchor, and the wheel pan clamps nothing — so
        // the thumb has to be able to say where *anywhere* in the document is.
        // A start taken off the travel could not, in exactly the band the floor
        // creates, and the failure looked like a frozen bar rather than like an
        // arithmetic mistake.
        let span = ScrollSpan::new(16384.0, 1440.0, 64.0, 0.0);
        let (_, length) = span.thumb();
        let mut last = -1.0;
        for step in 0..=20 {
            let at = ScrollSpan {
                centre: span.doc * step as f32 / 20.0,
                ..span
            };
            let start = at.thumb().0;
            assert!(start > last, "the thumb stalled at centre {}", at.centre);
            last = start;
        }
        assert!((last - (1.0 - length)).abs() < 1e-4, "ended at {last}");
    }

    #[test]
    fn a_camera_already_outside_the_document_is_not_snapped_back_by_the_bar() {
        // A space-drag has no limit, so the camera can stand well outside the
        // travel. Clamping to the document would make a hair of a drag teleport
        // the picture; the bounds widen to wherever it is instead, so the bar
        // can only ever improve matters.
        let far = ScrollSpan {
            centre: -5000.0,
            ..zoomed_out()
        };
        // Away from the document: refused outright rather than made worse.
        assert_eq!(far.pan_by(-0.5), 0.0);
        // Towards it: an ordinary drag, worth exactly what it is worth.
        assert!((far.pan_by(0.1) - 0.1 * far.travel()).abs() < 1e-3);
    }

    #[test]
    fn zooming_out_grows_the_thumb() {
        let close = ScrollSpan::new(1000.0, 500.0, 2.0, 500.0);
        let far = ScrollSpan::new(1000.0, 500.0, 0.5, 500.0);
        assert!(close.thumb().1 < far.thumb().1);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut cam = Camera {
            center: Vec2::ZERO,
            zoom: 1.0,
        };
        let pivot = vec2(400.0, 300.0);
        for _ in 0..200 {
            cam.zoom_at(Vec2::ZERO, 2.0, pivot);
        }
        assert!(cam.zoom <= Camera::MAX_ZOOM);
    }
}
