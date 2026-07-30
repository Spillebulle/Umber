//! Document model.
//!
//! Single-layer for now. Layers land next; the renderer already keeps the
//! stroke scratch surface separate from layer storage so adding them is an
//! additive change rather than a rewrite.

use glam::UVec2;

#[derive(Clone, Copy, Debug)]
pub struct Document {
    pub size: UVec2,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            size: UVec2::new(2048, 2048),
        }
    }
}

impl Document {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            size: UVec2::new(width.max(1), height.max(1)),
        }
    }

    pub fn size_vec2(&self) -> glam::Vec2 {
        self.size.as_vec2()
    }

    /// Bytes one RGBA8 layer occupies.
    pub fn layer_bytes(&self) -> u64 {
        self.size.x as u64 * self.size.y as u64 * 4
    }
}
