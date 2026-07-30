//! Viewport transform: maps between document pixels and screen pixels.

use glam::Vec2;

/// The view onto the document.
///
/// `center` is the document-space point displayed at the middle of the
/// viewport, and `zoom` is screen pixels per document pixel. Keeping the anchor
/// at the centre (rather than a corner) means window resizes don't shift what
/// the user is looking at.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub center: Vec2,
    pub zoom: f32,
}

impl Camera {
    pub const MIN_ZOOM: f32 = 0.02;
    pub const MAX_ZOOM: f32 = 64.0;

    /// Frame the whole document with a small margin.
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

    pub fn doc_to_screen(&self, doc: Vec2, viewport: Vec2) -> Vec2 {
        (doc - self.center) * self.zoom + viewport * 0.5
    }

    pub fn screen_to_doc(&self, screen: Vec2, viewport: Vec2) -> Vec2 {
        (screen - viewport * 0.5) / self.zoom + self.center
    }

    /// Drag the canvas by a screen-space delta.
    pub fn pan_by_screen(&mut self, delta: Vec2) {
        self.center -= delta / self.zoom;
    }

    /// Zoom by `factor`, keeping the document point under `anchor` pinned.
    ///
    /// This is what makes wheel-zoom feel right: without the correction the
    /// canvas slides out from under the cursor.
    pub fn zoom_at(&mut self, anchor_screen: Vec2, factor: f32, viewport: Vec2) {
        let before = self.screen_to_doc(anchor_screen, viewport);
        self.zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        let after = self.screen_to_doc(anchor_screen, viewport);
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
        let viewport = vec2(800.0, 600.0);
        let doc = vec2(37.0, 129.0);
        let round = cam.screen_to_doc(cam.doc_to_screen(doc, viewport), viewport);
        assert!((round - doc).length() < 1e-3, "got {round:?}");
    }

    #[test]
    fn zoom_keeps_anchor_pinned() {
        let mut cam = Camera {
            center: vec2(100.0, 100.0),
            zoom: 1.0,
        };
        let viewport = vec2(800.0, 600.0);
        let anchor = vec2(200.0, 150.0);
        let doc_before = cam.screen_to_doc(anchor, viewport);
        cam.zoom_at(anchor, 1.7, viewport);
        let doc_after = cam.screen_to_doc(anchor, viewport);
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
        let viewport = vec2(800.0, 600.0);
        for _ in 0..200 {
            cam.zoom_at(Vec2::ZERO, 2.0, viewport);
        }
        assert!(cam.zoom <= Camera::MAX_ZOOM);
    }
}
