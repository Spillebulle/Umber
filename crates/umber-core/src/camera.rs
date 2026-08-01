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

impl Camera {
    pub const MIN_ZOOM: f32 = 0.02;
    pub const MAX_ZOOM: f32 = 64.0;

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

    /// True when any of the document is outside the view — which is exactly
    /// when a bar is worth drawing, and covers both "too big to fit" and
    /// "fits, but has been pushed off the side".
    pub fn overflows(&self) -> bool {
        let half = self.extent * 0.5;
        // A hair of tolerance: a fitted document sits a rounding error from its
        // own edges, and a bar that flickered on and off as the camera drifted
        // would be worse than one that appears a pixel late.
        self.centre - half > 0.5 || self.centre + half < self.doc - 0.5
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
    pub fn thumb(&self) -> (f32, f32) {
        let length = (self.extent / self.travel()).clamp(0.02, 1.0);
        let start = (self.centre / self.travel()).clamp(0.0, 1.0 - length);
        (start, length)
    }

    /// How far the camera moves for a drag of `fraction` of the bar's length.
    ///
    /// The inverse of [`ScrollSpan::thumb`], and a plain multiply because the
    /// travel does not depend on where the camera is.
    pub fn pan_by(&self, fraction: f32) -> f32 {
        fraction * self.travel()
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

    // --- scrollbars --------------------------------------------------------

    /// A 1000-pixel document under a 500-pixel viewport at zoom 1, centred.
    fn half_shown() -> ScrollSpan {
        ScrollSpan::new(1000.0, 500.0, 1.0, 500.0)
    }

    #[test]
    fn a_document_that_fits_needs_no_bar() {
        // The whole document inside the view, with room to spare: nothing is
        // hidden, so nothing is drawn.
        let span = ScrollSpan::new(1000.0, 2000.0, 1.0, 500.0);
        assert!(!span.overflows());
    }

    #[test]
    fn a_document_pushed_off_the_side_needs_one_even_though_it_fits() {
        // The case the bars are actually for: the document is smaller than the
        // window and has been panned until half of it is under a panel.
        let span = ScrollSpan::new(1000.0, 2000.0, 1.0, 1800.0);
        assert!(span.overflows());
    }

    #[test]
    fn a_document_larger_than_the_view_needs_one() {
        assert!(half_shown().overflows());
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
