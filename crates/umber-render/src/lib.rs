//! GPU rendering for Umber.
//!
//! # Frame structure
//!
//! 1. **Dab pass** — new dabs since the last frame are stamped into a scratch
//!    coverage texture with `max` blending.
//! 2. **Composite pass** — layer and scratch are combined and drawn to the
//!    surface under the camera transform.
//! 3. **Commit** (at pointer-up only) — the scratch is baked into the layer
//!    over the stroke's damaged rectangle, then cleared.
//!
//! Steps 1 and 2 are the per-frame cost. Both are a single draw call, so
//! frame time is dominated by the composite pass's fullscreen fetch rather
//! than by how much the user drew.

pub mod canvas;
pub mod gpu;

pub use canvas::CanvasRenderer;
pub use gpu::Gpu;
