//! Platform-agnostic painting engine.
//!
//! This crate knows nothing about windows, GPUs or event loops. It turns a
//! stream of [`InputPoint`]s into a stream of [`Dab`]s, and owns the document
//! model, camera and undo history. Everything here is deterministic and
//! testable without a GPU.

pub mod brush;
pub mod brushimport;
pub mod camera;
pub mod canvassize;
pub mod clipboard;
pub mod color;
mod csblocks;
pub mod curve;
pub mod damage;
pub mod docformat;
pub mod docimport;
pub mod document;
pub mod dynamics;
pub mod effect;
pub mod export;
pub mod fonts;
pub mod geom;
pub mod harmony;
pub mod history;
pub mod input;
pub mod layer;
pub mod overlay;
pub mod palette;
pub mod palimport;
mod pattern_table;
pub mod preset;
pub mod preview;
pub mod selection;
// **Public for `examples/survey-clip-schema.rs` and for nothing else.** Nothing
// in the application reaches it: the readers that need it (`docimport` and
// `brushimport`) are inside this crate and take the same route they always did.
//
// The alternative was to put the schema walk *in* the library behind one `pub`
// function, which is two hundred lines of diagnostic shipped in the binary to
// avoid publishing a reader that is already an API internally — every item this
// module exposes was already `pub` within it. A reader must still never walk the
// schema to decide what to do: a reader whose behaviour follows a table it was
// never written against is exactly what `Database::table_names` says it is not
// for.
//
// **The same example takes the opposite decision about the chunk walk, and the
// two are not in conflict.** `survey-clip-schema` copies `clipstudio::split`'s
// thirty lines of container framing rather than have that widened, because what
// would have to be published there is a *reader's* internals — a private type
// holding borrowed slices, whose shape is the reader's to change. What is
// published here is a general-purpose SQLite reader that was already designed
// as one. The rule both follow is the same: do not widen a reader for a
// diagnostic. Neither is a precedent for widening the other.
pub mod sqlite;
pub mod stroke;
pub mod style;
pub mod text;
pub mod textobj;
pub mod thumbnail;
pub mod tile;
pub mod time;
pub mod tip;
mod tip_table;
pub mod transform;

pub use brush::{Brush, BrushMode, GrainPattern};
pub use camera::{Camera, ScrollSpan};
// `canvassize` is deliberately not re-exported here. Its one consumer imports
// the module, and a partial re-export would have to pick four of its ten public
// items — claiming `Orientation`, a broad name, at a root that already carries
// `Anchor` and `Unit`, and leaving the next caller to wonder why `Chosen` and
// `LockedShape` are not beside them. `palette::Palette` is kept out for a
// related reason and says so above.
pub use clipboard::{Clip, Cut};
pub use color::{Color, Hsv};
pub use curve::ResponseCurve;
pub use damage::TileMask;
pub use docformat::{SaveDocument, SaveError, SaveLayer, SaveWarning};
pub use docimport::{ImportError, ImportedDocument, ImportedLayer};
pub use document::{Anchor, Background, CanvasCopy, Document, Unit};
pub use dynamics::{DabInput, DabTarget, Modulation, Modulations};
// `Outline`, never `Stroke` — see `effect`'s module docs. The interface's word
// for it is `EffectKind::label`'s and lives nowhere else.
pub use effect::{Effect, EffectKind, OutlinePosition};
pub use export::{ExportError, ExportFormat, ExportLoss, ExportOptions};
pub use fonts::{Face, FontLibrary};
pub use geom::{FlipAxis, PixelRect, Rect};
pub use harmony::Harmony;
pub use history::{Edit, EditBody, EditKind, History, Jump, PixelPatch};
pub use input::InputPoint;
pub use layer::{BlendMode, EditRefusal, EditTarget, Layer, LayerStack, SlotRoom, StackShape};
pub use overlay::{Side, Strip};
// `palette::Palette` is deliberately **not** re-exported at the root. The app
// crate's own `theme::Palette` is the interface's colour tokens and is in scope
// in nearly every file that draws; a second `Palette` at `umber_core::Palette`
// would be one `use` away from the two being told apart by which import came
// last. Callers say `palette::Palette` and alias it, which reads as what it is.
pub use palette::{PaletteError, PaletteLibrary, Swatch};
pub use preset::{BrushPreset, Credit, PresetError, UserLibrary};
pub use selection::{ModeSetting, Selection, SelectionDraft, SelectionMode, SelectionOp};
pub use stroke::{Dab, StrokeBuilder};
pub use text::{Align, Setting, TextBlock, TextError};
pub use textobj::{Placement, TextFace, TextObject};
pub use time::Timestamp;
pub use tip::{StrokeCoverage, TipMask, TipReading, TipTarget, stroke_coverage};
pub use transform::{Affine, Handle, Transform};
