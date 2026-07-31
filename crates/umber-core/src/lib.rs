//! Platform-agnostic painting engine.
//!
//! This crate knows nothing about windows, GPUs or event loops. It turns a
//! stream of [`InputPoint`]s into a stream of [`Dab`]s, and owns the document
//! model, camera and undo history. Everything here is deterministic and
//! testable without a GPU.

pub mod brush;
pub mod camera;
pub mod color;
pub mod document;
pub mod geom;
pub mod history;
pub mod input;
pub mod layer;
pub mod stroke;

pub use brush::{Brush, BrushMode, BrushPreset};
pub use camera::Camera;
pub use color::{Color, Hsv};
pub use document::Document;
pub use geom::{PixelRect, Rect};
pub use history::{History, PixelPatch};
pub use input::InputPoint;
pub use layer::{BlendMode, Layer, LayerStack};
pub use stroke::{Dab, StrokeBuilder};
