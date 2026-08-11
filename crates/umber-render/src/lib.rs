//! GPU rendering for Umber.
//!
//! # Frame structure
//!
//! 1. **Dab pass** — new dabs since the last frame are stamped into a scratch
//!    coverage texture with `max` blending, or — for a brush that asks to build
//!    up, which a sparse texture stamp must — with each dab compositing over
//!    the last.
//! 2. **Composite pass** — the whole layer stack and the scratch are combined
//!    and drawn to the surface under the camera transform.
//! 3. **Commit** (at pointer-up only) — the scratch is baked into the active
//!    layer over the stroke's damaged rectangle, then cleared.
//!
//! Steps 1 and 2 are the per-frame cost, and both are a single draw call —
//! layers live in a texture array that the composite shader walks in one pass,
//! so an extra layer costs a loop iteration rather than a whole render pass.
//! Frame time is dominated by the composite's fullscreen fetch rather than by
//! how much the user drew.

pub mod canvas;
pub mod gpu;

pub use canvas::{
    BakedStack, CanvasRenderer, CaptureSlice, CompositeParams, DabStyle, DocumentCapture,
    EffectFrame, FloatParams, FloatSource, LayerDraw, LayerEffects, ProbeParams, StrokeStyle,
    Thumbnail, Vram, effect_marks_nothing, text_reset_is_live,
};
pub use gpu::{Choice, Gpu};
