//! Live telemetry of the pointer stream, for Settings → Input & pen.
//!
//! Umber cannot test a tablet where it is written — nobody working on it has a
//! pen — so the two pen fixes this exists to verify were made from the
//! documentation and shipped unproven. This is how somebody holding the
//! hardware settles them from inside the running application: which route the
//! events arrive by, what the device actually reported, and what
//! [`PressureModel::resolve`] made of it.
//!
//! Everything here is **observation**. Nothing on the stroke path reads it, and
//! a stroke must behave identically whether the pane is open or not. Two rules
//! keep that true:
//!
//! - **The resolved figure is recorded, never recomputed.** `resolve` mutates
//!   the model — it carries the simulated value forward and latches whether the
//!   device has been heard from this stroke — so calling it a second time to
//!   have a number to draw would corrupt the model driving the real stroke.
//!   [`InputLog::note_resolved`] takes what the one real call answered.
//! - **Nothing here allocates.** [`Ring`] is a fixed array written once per
//!   pointer event, which is the drawing path.
//!
//! The one thing that *is* resolved here is the test strip's own mark, and it
//! runs through [`InputLog::probe`] — a private copy of the model, reset on
//! every press. A copy because the strip is dragged while no stroke exists, so
//! there is no real call to record; private because a diagnostic must not be
//! able to reach the live model at all.

use glam::Vec2;
use umber_core::input::PressureModel;
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};

/// Which route through the window system an event arrived by.
///
/// The single most useful thing on the page. On Windows a pen reaches winit as
/// `WindowEvent::Touch`, through `WM_POINTER`, and produces no `CursorMoved` at
/// all; a mouse — and a tablet driver left in "mouse mode", which is the usual
/// reason a pen feels dead — produces only `CursorMoved` and `MouseInput`. So
/// this distinguishes "the driver is not sending pen events" from "the pen is
/// arriving and something later is wrong" without any guesswork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `CursorMoved` / `MouseInput` — a mouse, or a pen being reported as one.
    Mouse,
    /// `WindowEvent::Touch` — a finger, or a pen on Windows Ink.
    Touch,
}

impl Route {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mouse => "Mouse",
            Self::Touch => "Touch / pen",
        }
    }
}

/// What the pointer was doing when the event was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Down,
    Moved,
    Up,
    /// A touch update for a contact that never started: a pen in range and off
    /// the glass. Worth naming, because it is the state that proves the tablet
    /// is talking to Umber before anything has been drawn at all.
    Hovering,
    Cancelled,
}

impl Motion {
    pub fn label(self) -> &'static str {
        match self {
            Self::Down => "pressed",
            Self::Moved => "moved",
            Self::Up => "released",
            Self::Hovering => "hovering",
            Self::Cancelled => "cancelled",
        }
    }

    /// How winit's touch phases map onto the above.
    ///
    /// `contact` is whether this id is one that has already `Started`. The rule
    /// mirrors the stroke path's — a `Moved` for an unknown id is a hover, not
    /// a contact — deliberately rather than by sharing code: this is a *reading*
    /// of the event stream, and it must go on describing what arrives even if
    /// the stroke path's handling of it is what turns out to be wrong.
    fn of(phase: TouchPhase, contact: bool) -> Self {
        match phase {
            TouchPhase::Started => Self::Down,
            TouchPhase::Moved if contact => Self::Moved,
            TouchPhase::Moved => Self::Hovering,
            TouchPhase::Ended => Self::Up,
            TouchPhase::Cancelled => Self::Cancelled,
        }
    }
}

/// The shape winit reported a touch's force in, which is a platform tell.
///
/// Windows Ink arrives `Normalized`; `Calibrated` is iOS's, and is the only one
/// that carries a stylus altitude. Recorded because "no tilt" and "tilt this
/// build cannot receive" are different answers and the page has to give the
/// right one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForceKind {
    /// The event carried no force field at all.
    Absent,
    Normalised,
    Calibrated,
}

/// One pointer event, exactly as it arrived.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// Seconds since application start.
    pub at: f64,
    /// Physical window pixels.
    pub pos: Vec2,
    /// What the device reported, and `None` when it reported nothing.
    ///
    /// The distinction is the whole subject of the pen fix: winit's Windows
    /// path runs the raw reading through a normaliser that accepts `1..=1024`
    /// and answers `None` for everything else, so a genuine zero — a pen a
    /// hair off the glass — is indistinguishable from a mouse. The page must
    /// therefore never print an absent reading as `0.00`.
    pub reported: Option<f32>,
    /// What the real [`PressureModel::resolve`] call answered for this sample,
    /// or `None` where nothing resolved it — a hover, a pan, or a move while no
    /// stroke was running.
    pub resolved: Option<f32>,
    /// Stylus altitude in radians, when the platform supplied one. Only ever
    /// present inside a `Force::Calibrated`, which is iOS's form.
    pub altitude: Option<f32>,
    pub route: Route,
    pub motion: Motion,
    pub force_kind: ForceKind,
    /// Which gesture the real [`gesture::press`] call resolved this press to,
    /// and `None` for everything that is not a press.
    ///
    /// The reason this is worth a column: three gestures — the Alt-drag brush
    /// resize, the Pan tool and the Zoom tool — were decided in the mouse arm of
    /// `window_event` alone, so a pen fell through them and painted. "Which
    /// gesture did that press become" is the one reading that would have shown
    /// it, from inside the running application, on the machine that has the
    /// tablet. Recorded rather than recomputed, like [`Sample::resolved`]:
    /// `press` is pure, but a second call here would be a second opinion, and
    /// the page has to show what actually ran.
    pub gesture: Option<crate::gesture::Press>,
}

impl Sample {
    const EMPTY: Self = Self {
        at: 0.0,
        pos: Vec2::ZERO,
        reported: None,
        resolved: None,
        altitude: None,
        route: Route::Mouse,
        motion: Motion::Moved,
        force_kind: ForceKind::Absent,
        gesture: None,
    };
}

/// The most recent [`Sample`]s, oldest first.
///
/// Fixed capacity because it is written once per pointer event, which is the
/// drawing path — a `Vec` would allocate mid-stroke, and one that grew for as
/// long as the application stayed open would be a slow leak on a diagnostic
/// most people will never open.
pub struct Ring {
    items: [Sample; Self::CAP],
    /// Where the next sample goes.
    head: usize,
    /// How many of `items` have ever been written, saturating at `CAP`.
    len: usize,
}

impl Ring {
    /// Enough to hold a whole gesture at a tablet's report rate — a pen sends
    /// on the order of 130 points a second, so this is a second or so of one,
    /// which is what it takes to see a lift-off fall to zero.
    pub const CAP: usize = 192;

    fn new() -> Self {
        Self {
            items: [Sample::EMPTY; Self::CAP],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, sample: Sample) {
        self.items[self.head] = sample;
        self.head = (self.head + 1) % Self::CAP;
        self.len = (self.len + 1).min(Self::CAP);
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Every sample held, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Sample> + '_ {
        let start = (self.head + Self::CAP - self.len) % Self::CAP;
        (0..self.len).map(move |i| &self.items[(start + i) % Self::CAP])
    }

    /// The newest `n`, oldest first. Fewer if that is all there is.
    pub fn recent(&self, n: usize) -> impl Iterator<Item = &Sample> + '_ {
        self.iter().skip(self.len.saturating_sub(n))
    }

    pub fn newest(&self) -> Option<&Sample> {
        (self.len > 0).then(|| &self.items[(self.head + Self::CAP - 1) % Self::CAP])
    }

    fn newest_mut(&mut self) -> Option<&mut Sample> {
        (self.len > 0).then(|| &mut self.items[(self.head + Self::CAP - 1) % Self::CAP])
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

/// Half-width of the test strip's mark, in points, at a given pressure.
///
/// The floor is not decoration. A mark that vanishes completely at zero
/// pressure cannot be told from a mark that never arrived, and telling those
/// two apart is exactly what somebody looking at this page is trying to do.
pub fn nib_half_width(pressure: f32, max: f32) -> f32 {
    const MIN: f32 = 0.4;
    let max = max.max(MIN);
    MIN + (max - MIN) * pressure.clamp(0.0, 1.0)
}

/// Everything the Input & pen pane reads.
pub struct InputLog {
    pub ring: Ring,
    /// Cumulative counts for the session, so "has a pen ever reached this
    /// window" stays answerable after the mouse has been moved since. A pen
    /// that has never sent a single touch is the failure this catches.
    pub mouse_events: u32,
    pub touch_events: u32,
    /// How many touch events carried a force reading at all. A tablet whose
    /// touches all arrive with none is a driver problem, not a Umber one.
    pub with_force: u32,
    /// What the last painted frame asked the window system for: `Some(true)`
    /// where the interface wanted no cursor at all, `Some(false)` where it
    /// wanted an ordinary one, `None` before any frame has been painted.
    ///
    /// The reason this is worth a row of its own: "the arrow is still there
    /// under my pen" has two completely different causes, and the other columns
    /// on this page distinguish neither. Either Umber never asked — the pen was
    /// not recognised, or `Editor::cursor` was somewhere else and the canvas
    /// decided the pointer was over a panel — or Umber asked and the platform
    /// did not carry it out, which is a real thing that happens here and is
    /// what `syscursor` exists for. This says which.
    ///
    /// An `Option` because "no frame has been painted yet" is not the same
    /// answer as "an ordinary cursor", the same rule [`Sample::reported`] lives
    /// by. Recorded, never recomputed: it is the very bool the frame acted on.
    pub cursor_hidden: Option<bool>,
    /// Touch id currently in contact, for [`Motion::of`]. One rather than a set
    /// because it only has to tell a contact from a hover, and a second finger
    /// is a pinch the stroke path already refuses.
    contact: Option<u64>,
    /// Last position seen, so an event carrying none — `MouseInput` — still
    /// records where it happened.
    pos: Vec2,

    /// The test strip's own pressure model. See the module docs: a copy,
    /// because there is no real stroke to record while the modal is up, and
    /// private so nothing can mistake it for the live one.
    probe: PressureModel,
    probing: bool,
    probe_at: f64,
    probe_pos: Vec2,
    /// When the current drag of the strip began, so the strip draws that drag
    /// and not the one before it.
    pub probe_started: f64,
}

impl Default for InputLog {
    fn default() -> Self {
        Self {
            ring: Ring::new(),
            mouse_events: 0,
            touch_events: 0,
            with_force: 0,
            cursor_hidden: None,
            contact: None,
            pos: Vec2::ZERO,
            probe: PressureModel::default(),
            probing: false,
            probe_at: 0.0,
            probe_pos: Vec2::ZERO,
            probe_started: f64::MAX,
        }
    }
}

impl InputLog {
    /// Record a window event. Called for **every** one, so it returns as early
    /// as it can and does no work at all for the ones that are not pointer
    /// events.
    pub fn note(&mut self, event: &WindowEvent, now: f64) {
        let mut sample = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_events = self.mouse_events.saturating_add(1);
                self.pos = Vec2::new(position.x as f32, position.y as f32);
                Sample {
                    at: now,
                    pos: self.pos,
                    route: Route::Mouse,
                    motion: Motion::Moved,
                    ..Sample::EMPTY
                }
            }
            // The left button only. Middle pans and right opens nothing, and a
            // readout that flickered on a middle click would be reporting
            // something the stroke path never sees.
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.mouse_events = self.mouse_events.saturating_add(1);
                Sample {
                    at: now,
                    pos: self.pos,
                    route: Route::Mouse,
                    motion: if *state == ElementState::Pressed {
                        Motion::Down
                    } else {
                        Motion::Up
                    },
                    ..Sample::EMPTY
                }
            }
            WindowEvent::Touch(touch) => {
                self.touch_events = self.touch_events.saturating_add(1);
                self.pos = Vec2::new(touch.location.x as f32, touch.location.y as f32);
                let motion = Motion::of(touch.phase, self.contact == Some(touch.id));
                match motion {
                    Motion::Down => self.contact = Some(touch.id),
                    Motion::Up | Motion::Cancelled => self.contact = None,
                    _ => {}
                }
                let (force_kind, altitude) = match touch.force {
                    None => (ForceKind::Absent, None),
                    Some(winit::event::Force::Normalized(_)) => (ForceKind::Normalised, None),
                    Some(winit::event::Force::Calibrated { altitude_angle, .. }) => {
                        (ForceKind::Calibrated, altitude_angle.map(|a| a as f32))
                    }
                };
                if touch.force.is_some() {
                    self.with_force = self.with_force.saturating_add(1);
                }
                Sample {
                    at: now,
                    pos: self.pos,
                    // Exactly what the stroke path is handed — same call, same
                    // normalisation — so the number on the page is the number
                    // the brush engine sees and not a second reading of it.
                    reported: touch.force.map(|f| f.normalized() as f32),
                    altitude,
                    route: Route::Touch,
                    motion,
                    force_kind,
                    ..Sample::EMPTY
                }
            }
            _ => return,
        };

        if self.probing {
            sample.resolved = Some(self.resolve_probe(&sample, now));
        }
        self.ring.push(sample);
    }

    /// Record what the one real [`PressureModel::resolve`] call answered.
    ///
    /// Called from `Editor::sample`, immediately after that call and against
    /// the sample [`Self::note`] has just pushed for the same event — which is
    /// why `note` runs before the event is dispatched. Never resolves anything
    /// itself; see the module docs.
    pub fn note_resolved(&mut self, pressure: f32) {
        if let Some(newest) = self.ring.newest_mut() {
            newest.resolved = Some(pressure);
        }
    }

    /// Record which gesture the one real `gesture::press` call resolved a press
    /// to.
    ///
    /// **Only ever lands on a sample that is itself a press.** `note` records
    /// the left button and touches and skips everything else, so a middle-click
    /// — which resolves to a pan and is a press the page never saw — would
    /// otherwise back-fill its answer onto whatever motion happened to be
    /// newest. Same shape as [`Self::note_resolved`], and the same reason it is
    /// called immediately after the real decision rather than deciding again.
    pub fn note_gesture(&mut self, gesture: crate::gesture::Press) {
        if let Some(newest) = self.ring.newest_mut()
            && newest.motion == Motion::Down
        {
            newest.gesture = Some(gesture);
        }
    }

    /// Record which cursor the frame just painted asked the window system for.
    ///
    /// Takes the answer the frame acted on rather than asking `Editor::pen_dot`
    /// again, for the reason [`Self::note_resolved`] and [`Self::note_gesture`]
    /// do: the page has to show what actually ran. Frame-driven rather than
    /// event-driven, and deliberately — it is a statement about a *frame*, and
    /// the frames between two pointer events are exactly where an arrow that
    /// should not be there is sitting.
    pub fn note_cursor(&mut self, hidden: bool) {
        self.cursor_hidden = Some(hidden);
    }

    /// The most recent press that resolved to a gesture, for the pane's readout.
    ///
    /// Walks the ring backwards, which is fine: only the Input & pen page calls
    /// it, once per frame it is open, and never while painting.
    pub fn last_gesture(&self) -> Option<crate::gesture::Press> {
        self.ring.iter().filter_map(|s| s.gesture).last()
    }

    /// Start the test strip's own resolution, from a copy of the live model.
    pub fn begin_probe(&mut self, model: PressureModel, now: f64) {
        self.probe = model;
        self.probe.reset();
        self.probing = true;
        self.probe_at = now;
        self.probe_pos = self.pos;
        self.probe_started = now;
    }

    pub fn end_probe(&mut self) {
        self.probing = false;
    }

    pub fn probing(&self) -> bool {
        self.probing
    }

    /// Resolve one sample through the strip's own model.
    ///
    /// A press restarts it, which is what makes the strip exercise the per-
    /// stroke latch rather than a session-wide one: the same `reset` a real
    /// stroke gets at `begin_stroke`.
    fn resolve_probe(&mut self, sample: &Sample, now: f64) -> f32 {
        if sample.motion == Motion::Down {
            self.probe.reset();
        }
        // Screen pixels, where a stroke measures document pixels. It only
        // reaches the speed model, and the strip says so.
        let distance = (sample.pos - self.probe_pos).length();
        let dt = (now - self.probe_at).max(0.0);
        self.probe_pos = sample.pos;
        self.probe_at = now;
        self.probe.resolve(sample.reported, distance, dt)
    }

    /// Throw away everything recorded so far.
    pub fn clear(&mut self) {
        self.ring.clear();
        self.mouse_events = 0;
        self.touch_events = 0;
        self.with_force = 0;
        // Back to "nothing yet" like everything else here. The next painted
        // frame writes it again immediately, which is right: it describes that
        // frame rather than the session.
        self.cursor_hidden = None;
        self.probe_started = f64::MAX;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: f64) -> Sample {
        Sample {
            at,
            ..Sample::EMPTY
        }
    }

    #[test]
    fn a_ring_keeps_the_newest_samples_in_order() {
        let mut ring = Ring::new();
        for i in 0..(Ring::CAP + 7) {
            ring.push(sample(i as f64));
        }
        assert_eq!(ring.len(), Ring::CAP, "the ring must not grow");
        let times: Vec<f64> = ring.iter().map(|s| s.at).collect();
        assert_eq!(times.first().copied(), Some(7.0), "oldest first, wrapped");
        assert_eq!(
            times.last().copied(),
            Some((Ring::CAP + 6) as f64),
            "newest last"
        );
        assert!(
            times.windows(2).all(|w| w[1] > w[0]),
            "wrapping must not reorder the samples"
        );
    }

    #[test]
    fn an_empty_ring_yields_nothing() {
        let ring = Ring::new();
        assert!(ring.is_empty());
        assert_eq!(ring.iter().count(), 0);
        assert!(ring.newest().is_none());
    }

    #[test]
    fn recent_takes_from_the_new_end() {
        let mut ring = Ring::new();
        for i in 0..10 {
            ring.push(sample(i as f64));
        }
        let times: Vec<f64> = ring.recent(3).map(|s| s.at).collect();
        assert_eq!(times, vec![7.0, 8.0, 9.0]);
        assert_eq!(
            ring.recent(50).count(),
            10,
            "asking for more than is held gives what there is"
        );
    }

    #[test]
    fn the_resolved_figure_lands_on_the_sample_that_produced_it() {
        // The order the two halves run in is what makes this work: `note`
        // pushes the event, then the stroke path resolves it and amends.
        let mut log = InputLog::default();
        log.ring.push(sample(1.0));
        log.ring.push(sample(2.0));
        log.note_resolved(0.42);
        assert_eq!(log.ring.newest().unwrap().resolved, Some(0.42));
        assert_eq!(
            log.ring.iter().next().unwrap().resolved,
            None,
            "an earlier sample must not be back-filled"
        );
    }

    #[test]
    fn resolving_nothing_is_harmless() {
        // A pointer event that never reaches `Editor::sample` — a hover, a pan
        // — leaves the column empty rather than reading as a zero.
        let mut log = InputLog::default();
        log.note_resolved(1.0);
        assert!(log.ring.is_empty());
    }

    #[test]
    fn a_touch_that_never_started_reads_as_a_hover() {
        // A pen in range and off the glass. Calling it a contact is what put a
        // phantom finger in the stroke path's touch map and turned the next
        // press into a pinch.
        assert_eq!(Motion::of(TouchPhase::Moved, false), Motion::Hovering);
        assert_eq!(Motion::of(TouchPhase::Moved, true), Motion::Moved);
        assert_eq!(Motion::of(TouchPhase::Started, false), Motion::Down);
        assert_eq!(Motion::of(TouchPhase::Ended, true), Motion::Up);
        assert_eq!(Motion::of(TouchPhase::Cancelled, true), Motion::Cancelled);
    }

    fn pen(phase: TouchPhase, x: f64, force: Option<f32>) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, 0.0),
            force: force.map(|f| winit::event::Force::Normalized(f as f64)),
            // Windows issues a fresh pointer id per contact session, so a real
            // one changes between strokes; within one it does not.
            id: 7,
        })
    }

    #[test]
    fn a_pen_reads_as_touch_and_keeps_its_absent_readings_absent() {
        let mut log = InputLog::default();
        // In range and off the glass, then down, along, and lifted — and the
        // lift is the sample winit reports no pressure for, which is the whole
        // reason this page exists.
        log.note(&pen(TouchPhase::Moved, 10.0, Some(0.2)), 0.0);
        log.note(&pen(TouchPhase::Started, 10.0, Some(0.4)), 0.1);
        log.note(&pen(TouchPhase::Moved, 20.0, Some(0.6)), 0.2);
        log.note(&pen(TouchPhase::Ended, 30.0, None), 0.3);

        assert_eq!(log.touch_events, 4);
        assert_eq!(log.mouse_events, 0, "a pen must not be counted as a mouse");
        assert_eq!(log.with_force, 3);

        let motions: Vec<Motion> = log.ring.iter().map(|s| s.motion).collect();
        assert_eq!(
            motions,
            vec![Motion::Hovering, Motion::Down, Motion::Moved, Motion::Up]
        );
        assert!(log.ring.iter().all(|s| s.route == Route::Touch));
        assert_eq!(
            log.ring.newest().unwrap().reported,
            None,
            "an absent reading must stay absent, not become a zero"
        );
        assert_eq!(
            log.ring.newest().unwrap().force_kind,
            ForceKind::Absent,
            "and the page has to be able to say which kind of silence it was"
        );
    }

    #[test]
    fn a_mouse_reads_as_a_mouse_and_carries_no_pressure_field() {
        let mut log = InputLog::default();
        log.note(
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(4.0, 5.0),
            },
            0.0,
        );
        assert_eq!(log.mouse_events, 1);
        assert_eq!(log.touch_events, 0);
        let newest = log.ring.newest().unwrap();
        assert_eq!(newest.route, Route::Mouse);
        assert_eq!(newest.pos, Vec2::new(4.0, 5.0));
        assert_eq!(
            newest.reported, None,
            "mouse events have no pressure field at all"
        );
    }

    #[test]
    fn a_press_records_the_gesture_it_was_resolved_to() {
        // The reading this column exists for: a pen press that became a stroke
        // when the user meant to resize the brush is invisible in "route" and
        // "motion", and obvious here.
        let mut log = InputLog::default();
        log.note(&pen(TouchPhase::Started, 10.0, Some(0.4)), 0.0);
        log.note_gesture(crate::gesture::Press::ResizeBrush);
        assert_eq!(
            log.ring.newest().unwrap().gesture,
            Some(crate::gesture::Press::ResizeBrush)
        );
        assert_eq!(log.last_gesture(), Some(crate::gesture::Press::ResizeBrush));

        // A move is not a press and must not be given one. `note` skips the
        // middle button entirely, so without this guard a middle-click's pan
        // would be back-filled onto whatever motion happened to be newest.
        log.note(&pen(TouchPhase::Moved, 20.0, Some(0.6)), 0.1);
        log.note_gesture(crate::gesture::Press::Pan);
        assert_eq!(
            log.ring.newest().unwrap().gesture,
            None,
            "only a press carries a gesture"
        );
        assert_eq!(
            log.last_gesture(),
            Some(crate::gesture::Press::ResizeBrush),
            "and the readout still names the last real press"
        );
    }

    #[test]
    fn a_gesture_with_nothing_to_hang_it_on_is_harmless() {
        let mut log = InputLog::default();
        log.note_gesture(crate::gesture::Press::Paint);
        assert!(log.ring.is_empty());
        assert_eq!(log.last_gesture(), None);
    }

    #[test]
    fn a_nib_at_no_pressure_is_still_visible() {
        // Zero pressure has to draw *something*, or "the pen reported zero" and
        // "the pen sent nothing" look identical — which is the one distinction
        // the strip exists to make.
        assert!(nib_half_width(0.0, 9.0) > 0.0);
        assert!(nib_half_width(1.0, 9.0) > nib_half_width(0.5, 9.0));
        assert!(nib_half_width(0.5, 9.0) > nib_half_width(0.0, 9.0));
        assert_eq!(nib_half_width(1.0, 9.0), 9.0, "full pressure is the peak");
        assert_eq!(
            nib_half_width(2.0, 9.0),
            nib_half_width(1.0, 9.0),
            "a device reporting past its own maximum must not widen the mark"
        );
    }
}
