//! Platform-agnostic painting engine.
//!
//! This crate knows nothing about windows, GPUs or event loops. It turns a
//! stream of [`InputPoint`]s into a stream of [`Dab`]s, and owns the document
//! model, camera and undo history. Everything here is deterministic and
//! testable without a GPU.

pub mod brush;
pub mod brushimport;
pub mod camera;
pub mod clipboard;
pub mod color;
pub mod curve;
pub mod damage;
pub mod docformat;
pub mod docimport;
pub mod document;
pub mod dynamics;
pub mod export;
pub mod geom;
pub mod harmony;
pub mod history;
pub mod input;
pub mod layer;
pub mod overlay;
pub mod palette;
mod pattern_table;
pub mod preset;
pub mod preview;
pub mod selection;
mod sqlite;
pub mod stroke;
pub mod style;
pub mod thumbnail;
pub mod time;
pub mod tip;
mod tip_table;
pub mod transform;

pub use brush::{Brush, BrushMode, GrainPattern};
pub use camera::{Camera, ScrollSpan};
pub use clipboard::{Clip, Cut};
pub use color::{Color, Hsv};
pub use curve::ResponseCurve;
pub use damage::TileMask;
pub use docformat::{SaveDocument, SaveError, SaveLayer, SaveWarning};
pub use docimport::{ImportError, ImportedDocument, ImportedLayer};
pub use document::{Anchor, Background, CanvasCopy, Document, Unit};
pub use dynamics::{DabInput, DabTarget, Modulation, Modulations};
pub use export::{ExportError, ExportFormat, ExportLoss, ExportOptions};
pub use geom::{FlipAxis, PixelRect, Rect};
pub use harmony::Harmony;
pub use history::{Edit, EditBody, EditKind, History, Jump, PixelPatch};
pub use input::InputPoint;
pub use layer::{BlendMode, EditTarget, Layer, LayerStack};
pub use overlay::{Side, Strip};
// `palette::Palette` is deliberately **not** re-exported at the root. The app
// crate's own `theme::Palette` is the interface's colour tokens and is in scope
// in nearly every file that draws; a second `Palette` at `umber_core::Palette`
// would be one `use` away from the two being told apart by which import came
// last. Callers say `palette::Palette` and alias it, which reads as what it is.
pub use palette::{PaletteError, PaletteLibrary, Swatch};
pub use preset::{BrushPreset, Credit, PresetError, UserLibrary};
pub use selection::{Selection, SelectionDraft, SelectionMode, SelectionOp};
pub use stroke::{Dab, StrokeBuilder};
pub use time::Timestamp;
pub use tip::{StrokeCoverage, TipMask, TipReading, TipTarget, stroke_coverage};
pub use transform::{Affine, Handle, Transform};
