//! Does the eyedropper's outside-the-window drag actually happen?
//!
//! ```sh
//! cargo run --release -p umber-app --example probe-outside-drag
//! ```
//!
//! `syspick`'s module docs assert that the gesture needs no grab of Umber's
//! own, because **winit takes the mouse capture on button-down** — so a window
//! goes on receiving `CursorMoved` with client coordinates past its own size,
//! and receives the button-up wherever that happens. That claim was reasoned
//! from winit's source and never run, and the artist's report ("the eye dropper
//! only works inside the canvas") is exactly the shape a false claim there
//! would take. This runs it.
//!
//! It also settles a second thing the loupe rests on: **the pointer itself is
//! not in a screen read.** `syspick::sample` reads the pixel under the cursor,
//! so if GDI's screen surface included the cursor bitmap every pick would take
//! the arrow's own ink. Nothing in Umber would look obviously wrong; the colour
//! would simply be somebody's cursor.
//!
//! # It moves the pointer, so it contains what the pointer can reach
//!
//! Two windows, both this process's. A **backdrop** covering the whole virtual
//! screen, always on top, painted one flat known colour; and a small
//! **harness** above it. The drag runs from inside the harness out over the
//! backdrop and releases there, so every press and release in the sequence
//! lands on a window this example made. A synthetic click that went astray
//! because the capture does *not* hold would land on the backdrop rather than
//! on whatever the user had open — which matters, because an agent driving a
//! window it did not create once painted into somebody's unsaved document.
//!
//! The pointer is put back where it was found.
//!
//! **This is an example and not a test** for `measure-screenpick.rs`'s reason
//! and one more: a CI runner has no desktop, and no test may synthesise input.

fn main() {
    #[cfg(windows)]
    windows::run();
    #[cfg(not(windows))]
    {
        println!("The capture this probes is Windows' and winit's Windows path.");
        println!("On X11 the equivalent is the protocol's implicit passive grab,");
        println!("which is not built and could not be run here anyway.");
    }
}

#[cfg(windows)]
mod windows {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use umber_app::syspick;
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, SendInput,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN, SetCursorPos,
    };
    use winit::application::ApplicationHandler;
    use winit::dpi::{PhysicalPosition, PhysicalSize};
    use winit::event::{ElementState, MouseButton, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::{Window, WindowId, WindowLevel};

    /// The backdrop's fill, in softbuffer's `0RGB`. A colour nothing else on a
    /// desktop is likely to be, so "the sample read the backdrop" is not a
    /// coincidence.
    const BACKDROP: u32 = 0x00_2E_8B_57;
    const BACKDROP_RGB: [u8; 3] = [0x2E, 0x8B, 0x57];

    /// One step of the script per this long, so winit has frames to deliver the
    /// events in between.
    const BEAT: Duration = Duration::from_millis(70);

    struct Painted {
        window: Arc<Window>,
        surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
        _context: Option<softbuffer::Context<Arc<Window>>>,
    }

    impl Painted {
        fn new(window: Arc<Window>) -> Self {
            let context = softbuffer::Context::new(window.clone()).ok();
            let surface = context
                .as_ref()
                .and_then(|c| softbuffer::Surface::new(c, window.clone()).ok());
            Self {
                window,
                surface,
                _context: context,
            }
        }

        fn fill(&mut self, colour: u32) {
            let size = self.window.inner_size();
            let (Some(w), Some(h)) = (
                std::num::NonZeroU32::new(size.width),
                std::num::NonZeroU32::new(size.height),
            ) else {
                return;
            };
            let Some(surface) = self.surface.as_mut() else {
                return;
            };
            if surface.resize(w, h).is_err() {
                return;
            }
            let Ok(mut buffer) = surface.buffer_mut() else {
                return;
            };
            buffer.fill(colour);
            let _ = buffer.present();
        }
    }

    /// What the script does, in order. Every position is a *virtual screen*
    /// pixel and every one of them is over a window this process owns.
    enum Beat {
        MoveTo(i32, i32),
        Press,
        Release,
        /// Read the screen at the pointer and say what came back, which is the
        /// cursor-exclusion half.
        Sample,
        Done,
    }

    struct Probe {
        backdrop: Option<Painted>,
        harness: Option<Painted>,
        harness_id: Option<WindowId>,
        harness_origin: (i32, i32),
        harness_size: (u32, u32),
        /// The virtual screen: origin and size, which is what the backdrop is
        /// stretched over.
        backdrop_rect: (i32, i32, i32, i32),
        script: Vec<Beat>,
        step: usize,
        next: Instant,
        /// Every `CursorMoved` the harness saw while the button was held, in
        /// its own client coordinates.
        dragged: Vec<(f64, f64)>,
        held: bool,
        press_seen: bool,
        release_seen: Option<(f64, f64)>,
        sampled: Option<Option<[u8; 3]>>,
    }

    pub fn run() {
        // SAFETY: `GetSystemMetrics` takes an index and returns an `i32`;
        // `GetCursorPos` fills a `POINT` this owns.
        let (vx, vy, vw, vh, restore) = unsafe {
            let mut at = POINT { x: 0, y: 0 };
            GetCursorPos(&mut at);
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
                (at.x, at.y),
            )
        };
        println!("virtual screen: origin ({vx}, {vy}), {vw} x {vh}");
        println!("This covers the screen for about two seconds and moves the");
        println!("pointer. Both windows belong to this process, so a click that");
        println!("goes astray cannot reach anything of yours. The pointer is put");
        println!("back where it was found.");
        println!();

        // The harness sits well inside the primary monitor, so there is room on
        // every side of it to drag out into.
        let harness_origin = (vx + vw / 2 - 200, vy + vh / 2 - 150);
        let harness_size = (400u32, 300u32);
        let inside = (harness_origin.0 + 200, harness_origin.1 + 150);
        // Out to the right of the harness and well clear of it, which on a
        // multi-monitor desktop is still over the backdrop.
        let outside = (harness_origin.0 + 700, harness_origin.1 + 150);

        let script = vec![
            Beat::MoveTo(inside.0, inside.1),
            Beat::Press,
            Beat::MoveTo(inside.0 + 100, inside.1),
            Beat::MoveTo(outside.0 - 100, outside.1),
            Beat::MoveTo(outside.0, outside.1),
            Beat::Sample,
            Beat::Release,
            Beat::Done,
        ];

        let event_loop = EventLoop::new().expect("an event loop");
        event_loop.set_control_flow(ControlFlow::Poll);
        let mut probe = Probe {
            backdrop: None,
            harness: None,
            harness_id: None,
            harness_origin,
            harness_size,
            backdrop_rect: (vx, vy, vw, vh),
            script,
            step: 0,
            next: Instant::now() + Duration::from_millis(400),
            dragged: Vec::new(),
            held: false,
            press_seen: false,
            release_seen: None,
            sampled: None,
        };
        let _ = event_loop.run_app(&mut probe);

        // SAFETY: two coordinates, no pointers.
        unsafe { SetCursorPos(restore.0, restore.1) };
        probe.report();
    }

    impl Probe {
        fn report(&self) {
            println!();
            println!("press seen by the harness:   {}", self.press_seen);
            println!("moves while held:            {}", self.dragged.len());
            let outside_client = self
                .dragged
                .iter()
                .filter(|(x, y)| {
                    *x < 0.0
                        || *y < 0.0
                        || *x >= f64::from(self.harness_size.0)
                        || *y >= f64::from(self.harness_size.1)
                })
                .count();
            println!("  of those, past the client:  {outside_client}");
            if let Some(last) = self.dragged.last() {
                println!("  last position:              {last:?}");
            }
            match self.release_seen {
                Some(at) => println!("release seen by the harness: yes, at {at:?}"),
                None => println!("release seen by the harness: NO"),
            }
            println!();
            match self.sampled {
                Some(Some(rgb)) => {
                    println!("screen read under the pointer: {rgb:?}");
                    if rgb == BACKDROP_RGB {
                        println!("  which is the backdrop's own colour, so the read is of");
                        println!("  the window under the pointer and NOT of the cursor.");
                    } else {
                        println!("  expected {BACKDROP_RGB:?}. Either the backdrop had not");
                        println!("  painted, or the screen read is picking up something");
                        println!("  else — the cursor bitmap is the one worth chasing.");
                    }
                }
                Some(None) => {
                    println!("screen read under the pointer: nothing (off every monitor?)")
                }
                None => println!("screen read under the pointer: never taken"),
            }
            println!();
            if self.press_seen && outside_client > 0 && self.release_seen.is_some() {
                println!("THE CAPTURE HOLDS. A window that has taken a button press goes");
                println!("on receiving moves with client coordinates past its own size,");
                println!("and receives the release out there. That is exactly what the");
                println!("eyedropper's drag off the window rests on.");
            } else {
                println!("THE CAPTURE DOES NOT HOLD as `syspick` describes it. The");
                println!("eyedropper cannot reach the desktop by dragging, and that");
                println!("module's docs need rewriting rather than its code.");
            }
        }

        fn beat(&mut self, event_loop: &ActiveEventLoop) {
            let Some(beat) = self.script.get(self.step) else {
                event_loop.exit();
                return;
            };
            match beat {
                Beat::MoveTo(x, y) => move_to(*x, *y),
                Beat::Press => click(MOUSEEVENTF_LEFTDOWN),
                Beat::Release => click(MOUSEEVENTF_LEFTUP),
                Beat::Sample => {
                    // SAFETY: fills a `POINT` this owns.
                    let mut at = POINT { x: 0, y: 0 };
                    unsafe { GetCursorPos(&mut at) };
                    self.sampled = Some(syspick::sample(at.x, at.y));
                }
                Beat::Done => {
                    event_loop.exit();
                    return;
                }
            }
            self.step += 1;
            self.next = Instant::now() + BEAT;
        }
    }

    fn move_to(x: i32, y: i32) {
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        // `MOUSEEVENTF_ABSOLUTE` is 0..=65535 across whatever
        // `MOUSEEVENTF_VIRTUALDESK` says the desk is, so the mapping is off the
        // virtual screen's own origin and size rather than the primary
        // monitor's — which on a desktop with a screen left of the primary one
        // is the difference between landing on the window and landing on
        // nothing.
        let nx = ((x - vx) as f64 * 65535.0 / f64::from(vw.max(1))).round() as i32;
        let ny = ((y - vy) as f64 * 65535.0 / f64::from(vh.max(1))).round() as i32;
        send(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            nx,
            ny,
        );
    }

    fn click(flags: u32) {
        send(flags, 0, 0);
    }

    fn send(flags: u32, dx: i32, dy: i32) {
        // SAFETY: one fully initialised `INPUT` of the size `SendInput` is
        // told, passed by pointer to a call that reads it and returns.
        unsafe {
            let mut input: INPUT = std::mem::zeroed();
            input.r#type = INPUT_MOUSE;
            input.Anonymous.mi.dx = dx;
            input.Anonymous.mi.dy = dy;
            input.Anonymous.mi.dwFlags = flags;
            SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
        }
    }

    impl ApplicationHandler for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.harness.is_some() {
                return;
            }
            let (vx, vy, vw, vh) = self.backdrop_rect;
            let backdrop = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("probe backdrop")
                            .with_decorations(false)
                            .with_window_level(WindowLevel::AlwaysOnTop)
                            .with_position(PhysicalPosition::new(vx, vy))
                            .with_inner_size(PhysicalSize::new(vw.max(1) as u32, vh.max(1) as u32)),
                    )
                    .expect("a backdrop window"),
            );
            let mut backdrop = Painted::new(backdrop);
            backdrop.fill(BACKDROP);

            let harness = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("probe harness")
                            .with_decorations(false)
                            .with_window_level(WindowLevel::AlwaysOnTop)
                            .with_position(PhysicalPosition::new(
                                self.harness_origin.0,
                                self.harness_origin.1,
                            ))
                            .with_inner_size(PhysicalSize::new(
                                self.harness_size.0,
                                self.harness_size.1,
                            )),
                    )
                    .expect("a harness window"),
            );
            self.harness_id = Some(harness.id());
            let mut harness = Painted::new(harness);
            harness.fill(0x00_C0_50_20);
            harness.window.focus_window();

            self.backdrop = Some(backdrop);
            self.harness = Some(harness);
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            if Some(id) != self.harness_id {
                return;
            }
            match event {
                WindowEvent::RedrawRequested => {
                    if let Some(h) = self.harness.as_mut() {
                        h.fill(0x00_C0_50_20);
                    }
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => match state {
                    ElementState::Pressed => {
                        self.held = true;
                        self.press_seen = true;
                    }
                    ElementState::Released => {
                        self.held = false;
                        self.release_seen =
                            Some(self.dragged.last().copied().unwrap_or((f64::NAN, f64::NAN)));
                    }
                },
                WindowEvent::CursorMoved { position, .. } => {
                    if self.held {
                        self.dragged.push((position.x, position.y));
                    }
                }
                WindowEvent::CloseRequested => event_loop.exit(),
                _ => {}
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.harness.is_none() {
                return;
            }
            if Instant::now() >= self.next {
                self.beat(event_loop);
            }
        }
    }
}
