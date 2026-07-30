//! Editor state — everything that is not a GPU resource or a window.

use glam::Vec2;
use std::collections::HashMap;
use std::time::Instant;
use umber_core::{
    Brush, Camera, Color, Document, History, InputPoint, LayerStack, StrokeBuilder,
    input::{PressureModel, PressureSource},
};
use umber_render::{LayerDraw, StrokeStyle};

/// What the pointer is currently doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interaction {
    Idle,
    Drawing,
    Panning,
}

pub struct Editor {
    pub doc: Document,
    pub camera: Camera,
    pub brush: Brush,
    pub color: Color,
    pub layers: LayerStack,

    pub stroke: StrokeBuilder,
    pub history: History,
    pub pressure: PressureModel,

    pub interaction: Interaction,
    /// Cursor in physical window pixels.
    pub cursor: Vec2,
    pub last_cursor: Vec2,
    /// Space held — temporary pan modifier.
    pub space_down: bool,

    /// Brush settings captured at stroke start. The user can change the colour
    /// mid-stroke via the UI; the stroke must still commit with what it began
    /// with, or the preview and the committed result disagree.
    pub stroke_style: StrokeStyle,
    /// Layer slot the stroke started on. Captured because the user can select a
    /// different layer mid-stroke, and the stroke must land where it began.
    pub stroke_slot: u32,

    /// Touch points currently down, for pinch handling.
    pub touches: HashMap<u64, Vec2>,
    /// The touch that owns the current stroke.
    pub drawing_touch: Option<u64>,
    /// Pinch state: distance and midpoint at the previous sample.
    pub pinch: Option<(f32, Vec2)>,

    start: Instant,
    last_sample_time: f64,
    pub frame_times: [f32; 60],
    pub frame_cursor: usize,
}

impl Default for Editor {
    fn default() -> Self {
        let doc = Document::default();
        Self {
            doc,
            camera: Camera {
                center: doc.size_vec2() * 0.5,
                zoom: 1.0,
            },
            brush: Brush::default(),
            color: Color::from_srgb_u8(20, 20, 24, 255),
            layers: LayerStack::new(),
            stroke: StrokeBuilder::new(),
            history: History::default(),
            pressure: PressureModel::default(),
            interaction: Interaction::Idle,
            cursor: Vec2::ZERO,
            last_cursor: Vec2::ZERO,
            space_down: false,
            stroke_style: StrokeStyle::default(),
            stroke_slot: 0,
            touches: HashMap::new(),
            drawing_touch: None,
            pinch: None,
            start: Instant::now(),
            last_sample_time: 0.0,
            frame_times: [0.0; 60],
            frame_cursor: 0,
        }
    }
}

impl Editor {
    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn screen_to_doc(&self, screen: Vec2, viewport: Vec2) -> Vec2 {
        self.camera.screen_to_doc(screen, viewport)
    }

    /// Build an input sample, resolving pressure through the current model.
    pub fn sample(&mut self, screen: Vec2, viewport: Vec2, reported: Option<f32>) -> InputPoint {
        let now = self.now();
        let dt = (now - self.last_sample_time).max(0.0);
        self.last_sample_time = now;

        let doc = self.screen_to_doc(screen, viewport);
        // Speed is measured in document pixels so simulated pressure behaves
        // the same at every zoom level.
        let distance = (doc - self.screen_to_doc(self.last_cursor, viewport)).length();
        let pressure = self.pressure.resolve(reported, distance, dt);

        InputPoint::new(doc, pressure, now)
    }

    pub fn begin_stroke(&mut self, point: InputPoint) {
        // Snapshot the brush: the user can change colour, opacity or layer via
        // the panel mid-stroke, but the stroke must finish as it started.
        self.stroke_style = StrokeStyle {
            color: self.color,
            opacity: self.brush.opacity,
            mode: self.brush.mode,
        };
        self.stroke_slot = self.layers.active_slot();
        self.pressure.reset();
        self.stroke.begin(self.brush, point);
        self.interaction = Interaction::Drawing;
    }

    /// Flatten the layer stack into what the composite pass consumes.
    ///
    /// Bottom-to-top, matching the shader's iteration order.
    pub fn layer_draws(&self) -> Vec<LayerDraw> {
        self.layers
            .layers()
            .iter()
            .map(|l| LayerDraw {
                slot: l.slot(),
                opacity: l.opacity,
                blend: l.blend.index(),
                visible: l.visible,
            })
            .collect()
    }

    pub fn record_frame_time(&mut self, dt: f32) {
        self.frame_times[self.frame_cursor] = dt;
        self.frame_cursor = (self.frame_cursor + 1) % self.frame_times.len();
    }

    pub fn average_fps(&self) -> f32 {
        let sum: f32 = self.frame_times.iter().sum();
        let n = self.frame_times.iter().filter(|t| **t > 0.0).count();
        if n == 0 || sum <= 0.0 {
            0.0
        } else {
            n as f32 / sum
        }
    }

    /// True when pressure will be a flat 1.0, which is worth telling the user
    /// about rather than leaving them wondering why the pen feels dead.
    pub fn pressure_is_flat(&self) -> bool {
        matches!(self.pressure.source, PressureSource::Constant)
    }
}
