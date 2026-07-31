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
