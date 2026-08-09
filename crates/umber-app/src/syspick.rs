//! Reading a pixel of the desktop, for the eyedropper's other half.
//!
//! In `umber-app` for the reason `sysclip` and `syscursor` are: `umber-core`
//! and `umber-render` may not learn about the platform, which is the boundary
//! that keeps them testable without one. Those two are named rather than linked
//! because they are private modules and this one is `pub` — rustdoc refuses a
//! link out of a public item into a private one, and a broken link is worse
//! than a name. Nothing below this module knows the desktop exists, and the
//! canvas half of the eyedropper is untouched — it still goes through
//! `CanvasRenderer::pick_patch`, which reuses the screen composite pass, so
//! there is exactly one path from a pixel to a colour inside the document and
//! this adds a second one only for pixels that are not in it.
//!
//! **The decision is pure and the platform call is the thin part**, which is
//! `sysclip::decide`'s shape and is the only reason any of this is testable.
//! [`aim`] is a function of five readings — where the pointer is, whether that
//! is over the picture, how big the client area is, where it sits on the
//! desktop, and whether this build can read the desktop at all — and it is what
//! says whether a sample belongs to the canvas, to the screen, or to nothing at
//! all. [`sample`] is nine lines of GDI under a `cfg` and [`sample_patch`] is
//! the same call over a block. No test touches the screen.
//!
//! # How the pointer gets outside the window at all
//!
//! It does not leave. **winit takes the mouse capture on button-down**, on
//! Windows in `capture_mouse` from its `WM_LBUTTONDOWN` handler, and on X11 by
//! the implicit passive grab the protocol performs for a button press. While a
//! button is held the window therefore goes on receiving `WM_MOUSEMOVE` — and
//! so `WindowEvent::CursorMoved` — with client-relative coordinates that are
//! negative or past the client size, and it receives the button-up wherever
//! that happens. winit's move handler forwards those positions unchanged; it
//! filters only on the position having *changed*.
//!
//! So the gesture is press inside, drag out, release over the target, and it
//! needs no grab of our own, no global hook and no full-screen overlay. That
//! matters: a hook is a privilege this application has no business asking for,
//! and an overlay is a second window that would be *under the pointer* and
//! therefore the thing any screen read would read.
//!
//! **The pen has not been tried and nothing here rests on it.** winit's
//! `WM_POINTERDOWN`/`UPDATE`/`UP` handler does *not* call `capture_mouse` — the
//! capture above is the mouse arm's alone — so a pen relies on Windows' own
//! implicit pointer capture, which the documentation says holds a contact to
//! the window that received the down until the up. If that holds, a pen drag
//! reaches the desktop by the same route with nothing added; if it does not,
//! the pen simply picks inside the window and stops at its edge. Nobody working
//! on Umber has a tablet, which is why this is stated rather than claimed, and
//! Settings → Input & pen is where somebody with one would see what the press
//! actually resolved to.
//!
//! **There is a loupe now, and it lives inside Umber's window.** The first
//! draft of this module argued there could not be one: a magnifier has to be
//! drawn at the pointer, the pointer is on somebody else's window, so it is an
//! always-on-top borderless window moved once per event, occluding the pixel it
//! exists to magnify or offset from it by a hand-tuned margin, plus a second
//! wgpu surface and a second render pass. **Every clause of that is true and
//! the conclusion does not follow**, because a magnifier does not have to be
//! *at* the pointer to be useful. `loupe::place` keeps it in Umber's own view —
//! beside the pointer while the pointer is in the window, clamped to the edge
//! once the pointer has left — so it is an ordinary egui overlay in a
//! foreground layer, with no window and no surface of its own. See `loupe.rs`
//! for where it goes and [`sample_patch`] for where its pixels come from.
//!
//! The colour also follows the pointer as the drag moves, so the Colour
//! module's swatch is a second readout. Neither is *live* — `App::pick_this_
//! frame` skips a frame the pointer did not move on, so a pixel that changes
//! underneath a hand held still (a video, another window repainting) is not
//! re-read. That is the throttle earning its keep and it is worth saying,
//! because somebody will hold still over something that moves.
//!
//! # What is only true on Windows
//!
//! Windows is the one platform this was built and run on.
//!
//! * **X11** would work the same way — `xcb_get_image` against the root window,
//!   and the implicit grab above is already in the protocol. It is not built,
//!   because it cannot be run here.
//! * **Wayland** has no screen read at all by design; a compositor hands one
//!   out through `org.freedesktop.portal.Screenshot`, whose `PickColor` method
//!   is exactly this gesture performed by the *compositor* rather than by us.
//!   That is a different interaction — the portal draws its own picker and
//!   returns one colour — so it is not a backend for [`sample`] but a second
//!   spelling of the whole feature.
//! * **macOS** needs the Screen Recording permission since Catalina, which is a
//!   prompt, an entitlement and a trip to System Settings for the user; and
//!   nobody working on Umber has a Mac. `CGDisplayCreateImage` is the call.
//!
//! On all three [`DESKTOP_READABLE`] is false, [`aim`] answers
//! [`Aim::Unreachable`] the moment the pointer leaves the *canvas*, and the
//! tool options strip says why. A control that is live where it cannot work is
//! the thing this project refuses everywhere else. The boundary used to be the
//! window rather than the canvas, and moved when Umber's own chrome became a
//! screen read: there is one reason a platform cannot answer, so there is one
//! variant for it.
//!
//! # A read costs a display refresh, so it is once per frame
//!
//! **Every figure in this section comes from one run** of
//! `examples/measure-screenpick.rs`, and that is deliberate rather than tidy:
//! a page carrying 7 ms from one afternoon beside 4.6 from another is a page
//! that cannot be argued against. The run is a two-monitor desktop, virtual
//! origin `(-2560, 0)`, with nothing else building.
//!
//! One `GetPixel` against the screen DC is **4.7 ms**, and `GetDC` plus
//! `ReleaseDC` around it is **9 µs**. So there is no handle worth caching and
//! nothing about the call to make cheaper. The read waits for the compositor
//! rather than computing anything, so it is one refresh of whatever display it
//! is asked about — the figure is the display's, not the code's, and an earlier
//! run of this on a busier machine read 7 ms. The `BitBlt` route measured the
//! same, which is the other half of the same observation.
//!
//! That decides where the sample goes. Pointer events arrive far faster than a
//! refresh, so a read per event puts the event loop minutes behind a drag;
//! `App::picked_at` is the throttle and `App::render` is where the one sample
//! per frame is taken. **Re-run the example before changing any of that**, and
//! expect a different number: a 60 Hz panel should read about 16 ms.
//!
//! It also settles the loupe, and this is measured rather than predicted. A
//! `BitBlt` of an 11×11 block costs **4.6 ms**, which is what a `BitBlt` of one
//! pixel costs, because the wait is the wait rather than the pixels. Reading
//! the same neighbourhood with `GetPixel` would be 121 refreshes — 569 ms a
//! frame on the run these figures come from, which is not a control — so
//! [`sample_patch`] is one `BitBlt` and there is no second candidate.
//!
//! **And a frame pays one read, not two.** The first draft took `GetPixel` for
//! the colour and the block for the picture, which measured **9.0 ms** against
//! 4.6: the second call of a frame waits again, so the loupe would have doubled
//! the cost of a gesture that already existed, and the colour kept would have
//! come from an instant four milliseconds from the picture around it. The
//! middle texel of the block is the colour instead. See [`sample_patch`] for
//! why that is safe, which is the one place the "off every monitor" rule below
//! had to be restated rather than repeated.
//!
//! # What is subtly wrong even on Windows
//!
//! Say these rather than discover them. `examples/measure-screenpick.rs` is
//! what they were measured with.
//!
//! * **Wide-gamut and HDR displays hand back a number in the display's own
//!   space, and it is read as sRGB.** GDI's screen surface is the SDR
//!   composition, so on an HDR display what comes back is the tone-mapped
//!   version of what is on screen rather than the colour the pixel actually is,
//!   and on a Display-P3 or Adobe RGB panel with a matching profile the byte is
//!   in *that* space and Umber will treat it as sRGB. There is no fix short of
//!   reading the monitor's ICC profile and converting, which is a colour
//!   management story Umber does not have anywhere yet — the canvas is sRGB end
//!   to end. Picking off a photograph on a wide-gamut screen therefore gives a
//!   colour that is close and is not the same.
//! * **Hardware overlays are not in it.** Video played through an overlay plane
//!   and some full-screen exclusive games are composed by the display
//!   controller rather than into the surface GDI reads, so a pick over one of
//!   those reads whatever is behind it — usually black. `CAPTUREBLT` is the
//!   flag that widens a `BitBlt` to include layered windows and it is
//!   deliberately not used: it forces a repaint of the whole desktop, which at
//!   one call per frame of a drag is a visible flicker across every window on
//!   the machine.
//! * **Off every monitor must read as nothing and not as black**, and that is
//!   why [`sample`] is `GetPixel` rather than the `BitBlt` route. Measured:
//!   outside the virtual screen `GetPixel` answers `CLR_INVALID` where a
//!   `BitBlt` succeeds against nothing and hands back `[0, 0, 0]`. Two monitors
//!   of different heights leave a real gap that a drag crosses, and a picker
//!   that silently took black there would be worse than one that took nothing.
//!
//!   **[`sample_patch`] does not rest on that and must not be read as
//!   contradicting it.** A block has to answer the question per texel and
//!   `CLR_INVALID` cannot, so it asks `MonitorFromPoint` with
//!   `MONITOR_DEFAULTTONULL` — which answers "is there a screen here" directly
//!   where `GetPixel`'s refusal answers it by accident. That is what lets one
//!   read serve the picture and the colour both. The example drives the two
//!   against each other over the virtual screen's corners, one pixel off each
//!   edge and far off every monitor, at the real block size so the centring is
//!   what is under test: they agree everywhere, including on refusing.
//! * **Multi-monitor is right, and only because the process is DPI aware.**
//!   The example was run on a desktop whose virtual screen origin is
//!   `(-2560, 0)` — a second monitor left of the primary one — and the reads at
//!   its corners answered, negative coordinates included.
//!   winit calls `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` before
//!   any window exists, so `Window::inner_position` is in true virtual-screen
//!   physical pixels and the screen DC's coordinate space is the same one —
//!   including the negative coordinates a monitor left of or above the primary
//!   one has. Were the process DPI-unaware, Windows would virtualise both and
//!   the two would disagree by the scale factor of whichever monitor the
//!   pointer was over. Nothing here compensates for that, because nothing has
//!   to; it is written down because the failure would look like a picker that
//!   is accurate on one screen and off by a few pixels on the next.

use glam::Vec2;

/// Whether this build can read a pixel that is not its own.
///
/// A `const` rather than a `cfg!` at each site, so that the interface's
/// disabled control and the decision below cannot end up gated on different
/// spellings of the same question.
pub const DESKTOP_READABLE: bool = cfg!(windows);

/// What the tool options strip says about picking *beyond the canvas*.
///
/// One line per platform and every one of them is drawn — this is not a refusal
/// message, it is the sentence, and on Windows it says what the gesture does.
/// That is deliberate: one function means the strip cannot describe a
/// capability the module does not have, which is the failure a separate
/// "supported" string and "unsupported" string invite.
///
/// It used to say *outside the window*, which was the boundary while Umber's
/// own chrome read nothing. The boundary is now the canvas: past it the screen
/// answers, whether what is there is a Layers panel or somebody else's window.
///
/// Short, because it shares one unwrapped row with the sentence beside it and a
/// strip does not reflow. Everything longer is [`outside_detail`], on hover,
/// which is the split every long explanation in this interface makes.
pub const fn outside_line() -> &'static str {
    if cfg!(windows) {
        "Drag anywhere on the screen to take the colour under the pointer."
    } else if cfg!(target_os = "macos") {
        "Picking beyond the canvas is not built for macOS yet."
    } else {
        "Picking beyond the canvas is not built for this system yet."
    }
}

/// The rest of it, in one hover.
///
/// Per platform rather than one sentence for all three, because what there is
/// to say genuinely differs and only one of the three is likely to change: on
/// Wayland it is a deliberate property of the display server, on macOS it is a
/// permission nobody here has the hardware to test, and on Windows there is no
/// obstacle to explain so it says where to look for the answer instead.
pub const fn outside_detail() -> &'static str {
    if cfg!(windows) {
        "The loupe shows what a release would take, and the colour under the \
         pointer becomes the painting colour as you drag."
    } else if cfg!(target_os = "macos") {
        "It needs the Screen Recording permission, which is a prompt and an \
         entitlement, and nobody working on Umber has a Mac to test it on. \
         Picking on the canvas works as it always has."
    } else {
        "On Wayland a colour off the screen has to come from the compositor's \
         own picker, and the X11 route has not been written. Picking on the \
         canvas works as it always has."
    }
}

/// Where a sample taken at some pointer position belongs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aim {
    /// On the document. The canvas answers, through the composite pass,
    /// exactly as an Alt-click has always done — and where the point is
    /// outside the picture that read declines and the colour is left alone.
    Canvas,
    /// Inside Umber's own window and not on the document: a panel, the tab
    /// strip, a scrollbar, the selection's strip, the margin round a canvas
    /// smaller than the view. Read **off the screen**, at these virtual-screen
    /// physical pixels — the same reading [`Aim::Desktop`] takes, from the same
    /// arithmetic, one line above.
    ///
    /// It exists because the first draft did not have it. `Editor::cursor` is
    /// not clipped to the canvas region and `screen_to_doc` is a plain camera
    /// transform, so a point over the Layers panel maps to a real document
    /// pixel whenever the picture reaches under the dock — which it does at any
    /// zoom that fills the window. A drag that left the canvas therefore went
    /// on changing the painting colour to colours the artist could not see.
    /// The press was already safe (`ui_owns_pointer` refuses it); the drag was
    /// not, because nothing after the press asked again.
    ///
    /// **It then read nothing at all, and that was wrong for a second
    /// reason.** The stated argument was that reading a panel off the screen
    /// surface hands back the theme's own ink already composited with whatever
    /// egui drew over it. That is exactly what an eyedropper is for: it takes
    /// the colour you can *see*. Umber's chrome is on the screen precisely as
    /// another application's window is, so a picker that read Photoshop's title
    /// bar and refused Umber's own swatch grid was incoherent from the side
    /// that matters — and it is what the artist reported. The variant survives
    /// the correction because the *canvas* still answers through the document
    /// (`pick_colour`, the composite before the interface is drawn over it,
    /// which is exact at any zoom); only this one changed instrument.
    Interface(i32, i32),
    /// Outside the window, at these virtual-screen physical pixels.
    Desktop(i32, i32),
    /// Nothing here can be read: outside the window, or over the interface, on
    /// a build with no screen read.
    ///
    /// It used to mean the first of those alone. Once the interface became a
    /// screen read it had to cover the second, because the two now fail for one
    /// reason — [`DESKTOP_READABLE`] — and giving them separate answers would
    /// be two variants meaning "this platform cannot".
    Unreachable,
}

/// Decide where a pick lands.
///
/// Four injected readings and no state, which is `sysclip::decide`'s shape and
/// the only reason any of this is testable — the macOS and Linux answers are
/// checked on a Windows machine, and the panel case is checked with no window
/// at all.
///
/// * `pointer` and `client` are physical window pixels, winit's unit and what
///   `Editor::cursor` holds.
/// * `over_canvas` is `Editor::pointer_over_canvas`, which is the *canvas
///   region* minus the panels, the scrollbars and the canvas's own overlay
///   controls. Injected rather than derived here, because that reading belongs
///   to the layout and this module may not learn about panels.
/// * `origin` is `Window::inner_position`, the client area's top-left in
///   desktop physical pixels, and it is an `Option` because winit's is.
/// * `desktop_readable` is injected rather than read from [`DESKTOP_READABLE`]
///   for the reason `install::detect` takes a `Probe`.
///
/// **The order is canvas, then window, then desktop.** The canvas is first
/// because the document is a better instrument than the screen for the pixels
/// it owns — it is the composite *before* the interface is drawn over it, so it
/// is exact at any zoom, where a screen read of a canvas at 37% would hand back
/// whatever the sampler resolved several document pixels into. The other two
/// differ only in which side of the client rectangle the pointer is on, and
/// they take the identical reading: **there is one statement of the
/// screen-coordinate arithmetic below and both answers are built from it**, so
/// the interface half cannot drift from the desktop half by a pixel.
///
/// **Umber's title bar and window borders fall in the second group**, and it is
/// stated rather than fixed: they are outside the *client* area, so a drag onto
/// them is an [`Aim::Desktop`] rather than an [`Aim::Interface`]. Both read the
/// screen, so nothing observable turns on it; excluding them would mean reading
/// `outer_position` and `outer_size` as well and having two rectangles to keep
/// in step.
pub fn aim(
    pointer: Vec2,
    over_canvas: bool,
    client: Vec2,
    origin: Option<(i32, i32)>,
    desktop_readable: bool,
) -> Aim {
    if over_canvas {
        return Aim::Canvas;
    }
    let inside = pointer.x >= 0.0
        && pointer.y >= 0.0
        && pointer.x < client.x.max(0.0)
        && pointer.y < client.y.max(0.0);
    if !desktop_readable {
        return Aim::Unreachable;
    }
    let Some((ox, oy)) = origin else {
        return Aim::Unreachable;
    };
    // **One statement, two answers.** The interface and the desktop differ in
    // which variant they wear and in nothing else, so a second copy of these
    // two lines — which is what putting the interface case in its own branch
    // would have meant — is a pixel of drift waiting to be introduced by
    // whichever of the two somebody edits next.
    //
    // `floor` rather than `as i32`, which truncates towards zero and would
    // therefore round the wrong way for every position left of or above the
    // window — the exact half of the range this branch exists for.
    //
    // `saturating_add` because the sum is the only arithmetic in this module
    // and `as i32` saturates rather than wrapping: a nonsense pointer position
    // would give `i32::MAX`, and `origin + i32::MAX` is a panic in a debug
    // build. winit's Windows path bounds these to a `i16` in practice, so this
    // is not reachable today and costs nothing to make unreachable by
    // construction.
    let x = ox.saturating_add(pointer.x.floor() as i32);
    let y = oy.saturating_add(pointer.y.floor() as i32);
    if inside {
        Aim::Interface(x, y)
    } else {
        Aim::Desktop(x, y)
    }
}

/// Read one pixel of the desktop, at virtual-screen physical pixels.
///
/// `None` where the position is off every monitor, or where GDI declines. The
/// bytes are the desktop's own sRGB and go through `Color::from_srgb_u8` at the
/// call site — never a second `powf`, which is the rule the clipboard and the
/// palette already keep.
#[cfg(windows)]
pub fn sample(x: i32, y: i32) -> Option<[u8; 3]> {
    use windows_sys::Win32::Graphics::Gdi::{CLR_INVALID, GetDC, GetPixel, ReleaseDC};

    // SAFETY: `GetDC(NULL)` asks for the screen's device context, which is the
    // documented way to spell "the whole virtual desktop"; it returns null on
    // failure and that is checked. `GetPixel` takes that handle and two
    // coordinates and dereferences nothing of ours. `ReleaseDC` is paired with
    // it on every path, including the early return, because a screen DC is a
    // process-wide resource and leaking one leaks it for the session.
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        if dc.is_null() {
            return None;
        }
        let colour = GetPixel(dc, x, y);
        ReleaseDC(std::ptr::null_mut(), dc);
        if colour == CLR_INVALID {
            // Off every monitor, which is an ordinary thing for a pointer
            // dragged into the gap between two screens of different heights.
            return None;
        }
        // A COLORREF is 0x00bbggrr, which is why this is not a byte cast.
        Some([
            (colour & 0xff) as u8,
            ((colour >> 8) & 0xff) as u8,
            ((colour >> 16) & 0xff) as u8,
        ])
    }
}

/// The same, on a platform with no screen read.
///
/// Never called: [`aim`] answers [`Aim::Unreachable`] wherever
/// [`DESKTOP_READABLE`] is false, so the refusal is one gate above this rather
/// than here. It exists so the call site compiles everywhere without a `cfg`
/// of its own, which is the same reason `sysclip`'s echo is a `const` and not a
/// `cfg`.
#[cfg(not(windows))]
pub fn sample(_x: i32, _y: i32) -> Option<[u8; 3]> {
    None
}

/// Read a `size`×`size` neighbourhood of the screen, centred on `(x, y)`.
///
/// Row-major, `size * size` entries, top-left first. A texel that is on **no
/// monitor** is `None` rather than black, which is the loupe's whole reason for
/// taking this shape: a patch at the edge of a screen, or in the gap two
/// monitors of different heights leave, is mostly desktop and partly nothing,
/// and a black band would read as a black window. That is the same distinction
/// [`sample`] makes with `CLR_INVALID`, applied per texel.
///
/// **One `BitBlt`, and the alternative is not close.** `GetPixel` waits for a
/// display refresh, so an 11×11 neighbourhood read that way is 121 refreshes —
/// **569 ms** a frame on the run the module docs quote, which is not a control.
/// `examples/measure-screenpick.rs` times this against the single pixel; the
/// block came out at 4.6 ms against the pixel's 4.7, because the wait is the
/// wait rather than the pixels.
///
/// **It does decide what a click takes**, through its middle texel, and that
/// took overturning the rule above. The first draft called [`sample`] beside
/// this on every frame, because `GetPixel` is the route that answers "nothing"
/// off every monitor where a `BitBlt` succeeds against nothing and hands back
/// black. That rule is about the *blit*; the `MonitorFromPoint` sweep below is
/// a different and more direct answer to the same question, so it does not
/// apply here. Measured, the pair cost 9.0 ms a frame against 4.6 for one — the
/// second read of a frame waits again — and the two answers were four
/// milliseconds apart on a live desktop. The example drives them against each
/// other at this size and they agree, refusals included. [`sample`] remains the
/// fallback for a blit that failed outright, which is the one thing a block
/// cannot report.
///
/// `None` for a GDI failure or a `size` of zero. No `CAPTUREBLT`, for the
/// reason the module docs give: it repaints the whole desktop.
#[cfg(windows)]
pub fn sample_patch(x: i32, y: i32, size: u32) -> Option<Vec<Option<[u8; 3]>>> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, MONITOR_DEFAULTTONULL,
        MonitorFromPoint, ReleaseDC, SRCCOPY, SelectObject,
    };

    if size == 0 {
        return None;
    }
    let n = i32::try_from(size).ok()?;
    // The centre texel is the pixel `sample` would read, so the top-left is
    // half a block up and to the left. Integer division, so an even `size`
    // puts the centre one past the middle — which is why the loupe's own
    // constant is odd and says so.
    let half = n / 2;
    let (left, top) = (x.saturating_sub(half), y.saturating_sub(half));

    // SAFETY: as `sample` for the screen DC. Every handle created here is
    // checked for null before use and destroyed on every path, including the
    // early ones; the bitmap is deselected before `GetDIBits` because MSDN
    // requires it not to be selected into a DC when that is called; and the
    // destination buffer is `n * n` 32-bit pixels, which is exactly what the
    // `BITMAPINFOHEADER` beside it describes.
    let raw: Option<Vec<[u8; 3]>> = unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, n, n);
        let mut out = None;
        if !mem.is_null() && !bmp.is_null() {
            let old = SelectObject(mem, bmp as _);
            let blitted = BitBlt(mem, 0, 0, n, n, screen, left, top, SRCCOPY) != 0;
            SelectObject(mem, old);
            if blitted {
                let mut info: BITMAPINFO = std::mem::zeroed();
                info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                info.bmiHeader.biWidth = n;
                // Negative height asks for a top-down bitmap, so the first row
                // of the buffer is the top row of the block and the caller's
                // row-major order needs no flip.
                info.bmiHeader.biHeight = -n;
                info.bmiHeader.biPlanes = 1;
                info.bmiHeader.biBitCount = 32;
                info.bmiHeader.biCompression = BI_RGB;
                let count = (size as usize) * (size as usize);
                let mut px = vec![0u8; count * 4];
                if GetDIBits(
                    mem,
                    bmp,
                    0,
                    size,
                    px.as_mut_ptr().cast(),
                    &mut info,
                    DIB_RGB_COLORS,
                ) != 0
                {
                    // A 32-bit DIB is BGRA in memory.
                    out = Some(
                        px.chunks_exact(4)
                            .map(|c| [c[2], c[1], c[0]])
                            .collect::<Vec<_>>(),
                    );
                }
            }
        }
        if !bmp.is_null() {
            DeleteObject(bmp as _);
        }
        if !mem.is_null() {
            DeleteDC(mem);
        }
        ReleaseDC(std::ptr::null_mut(), screen);
        out
    };

    let raw = raw?;
    // Which texels exist. `MonitorFromPoint` with `MONITOR_DEFAULTTONULL` is
    // the only reading that answers this: the gap between two screens of
    // different heights is *inside* the virtual screen's bounding rectangle
    // and on no monitor, so no arithmetic over `SM_*VIRTUALSCREEN` can find
    // it. It computes rather than waiting on the compositor, which is why 121
    // of them beside one `BitBlt` is not a second refresh.
    Some(
        raw.into_iter()
            .enumerate()
            .map(|(i, rgb)| {
                let px = left + (i % size as usize) as i32;
                let py = top + (i / size as usize) as i32;
                // SAFETY: takes a `POINT` by value and a flag, and returns a
                // handle this never dereferences.
                let monitor =
                    unsafe { MonitorFromPoint(POINT { x: px, y: py }, MONITOR_DEFAULTTONULL) };
                (!monitor.is_null()).then_some(rgb)
            })
            .collect(),
    )
}

/// The same, on a platform with no screen read.
///
/// Never called, for [`sample`]'s reason and gated at the same place.
#[cfg(not(windows))]
pub fn sample_patch(_x: i32, _y: i32, _size: u32) -> Option<Vec<Option<[u8; 3]>>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: Vec2 = Vec2::new(1280.0, 800.0);
    const ORIGIN: Option<(i32, i32)> = Some((100, 50));

    /// On the canvas: what `Editor::pointer_over_canvas` says over the picture.
    fn on_canvas(at: Vec2) -> Aim {
        aim(at, true, CLIENT, ORIGIN, true)
    }

    /// Inside the window and not on the canvas: over a panel, a scrollbar, the
    /// tab strip, or outside the window altogether.
    fn off_canvas(at: Vec2) -> Aim {
        aim(at, false, CLIENT, ORIGIN, true)
    }

    #[test]
    fn over_the_picture_is_the_canvas() {
        for at in [
            Vec2::new(0.0, 0.0),
            Vec2::new(640.0, 400.0),
            Vec2::new(1279.0, 799.0),
        ] {
            assert_eq!(on_canvas(at), Aim::Canvas, "at {at:?}");
        }
    }

    #[test]
    fn a_drag_onto_a_panel_reads_the_screen_where_the_panel_is() {
        // **Two defects, one variant, and the second was this module's own
        // decision.** The first: the press is refused by `ui_owns_pointer`, but
        // the *drag* asked nothing after it, and `screen_to_doc` is a plain
        // camera transform with no clip to the canvas region — so at any zoom
        // that fills the window a point over the Layers panel mapped to a real
        // document pixel, and the painting colour went on changing to colours
        // the artist could not see. The second: the repair read nothing at all,
        // on the argument that a panel off the screen surface is the theme's
        // ink composited with whatever egui drew over it. That is what an
        // eyedropper is *for*, and the artist reported it as the picker not
        // working on Umber's own interface.
        //
        // So the answer is the screen, at the same coordinates the desktop
        // branch computes — the panel at window (4, 4) with the window at
        // (100, 50) is screen (104, 54), whatever is drawn there.
        assert_eq!(off_canvas(Vec2::new(4.0, 4.0)), Aim::Interface(104, 54));
        assert_eq!(
            off_canvas(Vec2::new(1279.0, 400.0)),
            Aim::Interface(1379, 450)
        );
        assert_eq!(
            off_canvas(Vec2::new(640.0, 799.0)),
            Aim::Interface(740, 849)
        );
    }

    #[test]
    fn the_interface_and_the_desktop_are_one_piece_of_arithmetic() {
        // The pair either side of the client edge. `client.x` is 1280, so 1279
        // is the last column inside the window and 1280 is the first outside
        // it — and the two answers must be consecutive screen pixels, or the
        // colour under the pointer would jump by however much the two copies of
        // the arithmetic had drifted. There is one copy, and this is what says
        // so from the outside.
        let last_in = off_canvas(Vec2::new(1279.0, 400.0));
        let first_out = off_canvas(Vec2::new(1280.0, 400.0));
        assert_eq!(last_in, Aim::Interface(1379, 450));
        assert_eq!(first_out, Aim::Desktop(1380, 450));
        // And down the other edge, where the origin's y is what is added.
        assert_eq!(off_canvas(Vec2::new(40.0, 799.0)), Aim::Interface(140, 849));
        assert_eq!(off_canvas(Vec2::new(40.0, 800.0)), Aim::Desktop(140, 850));
    }

    #[test]
    fn outside_the_window_lands_on_the_desktop_at_its_own_offset() {
        assert_eq!(
            off_canvas(Vec2::new(1280.0, 400.0)),
            Aim::Desktop(1380, 450)
        );
        assert_eq!(off_canvas(Vec2::new(10.0, 800.0)), Aim::Desktop(110, 850));
    }

    #[test]
    fn the_canvas_wins_over_the_window_test_and_never_the_other_way() {
        // `over_canvas` is asked first, so a canvas region that somehow
        // reported true outside the client area would still read as the canvas
        // rather than as the desktop. That is the safe direction: the canvas
        // read declines a point off the document, where a desktop read would
        // hand back a pixel of somebody else's window for a position Umber
        // believed was its own.
        assert_eq!(
            aim(Vec2::new(-4.0, -4.0), true, CLIENT, ORIGIN, true),
            Aim::Canvas
        );
    }

    #[test]
    fn a_pointer_left_of_or_above_the_window_rounds_the_way_the_others_do() {
        // `as i32` truncates towards zero, so -0.5 would come back as 0 and
        // every position in the leftmost column of the drag would be read one
        // pixel to the right of where the pointer was. This is the whole
        // reason `aim` floors.
        assert_eq!(off_canvas(Vec2::new(-0.5, 400.0)), Aim::Desktop(99, 450));
        assert_eq!(off_canvas(Vec2::new(-1.5, -1.5)), Aim::Desktop(98, 48));
        // And the positive side keeps agreeing with it, so the two halves of
        // the drag are one rule rather than two.
        assert_eq!(
            off_canvas(Vec2::new(1280.5, 800.5)),
            Aim::Desktop(1380, 850)
        );
    }

    #[test]
    fn a_monitor_left_of_the_primary_one_is_a_negative_coordinate() {
        // Windows puts the virtual screen's origin at the primary monitor's
        // top-left, so a second screen to the left is at negative x — and a
        // window on it has a negative `inner_position`. Nothing clamps.
        let far_left = Some((-1920, -120));
        assert_eq!(
            aim(Vec2::new(10.0, 10.0), true, CLIENT, far_left, true),
            Aim::Canvas
        );
        assert_eq!(
            aim(Vec2::new(-10.0, -10.0), false, CLIENT, far_left, true),
            Aim::Desktop(-1930, -130)
        );
    }

    #[test]
    fn a_nonsense_position_saturates_rather_than_overflowing() {
        // `as i32` saturates, so a pointer at `f32::MAX` becomes `i32::MAX` and
        // a plain `+` on the origin is an overflow — which in a debug build is
        // a panic and in a release build is a wrap to a coordinate on the far
        // side of the desktop. Not reachable through winit today, and free to
        // make unreachable by construction.
        //
        // **Both ends, and both with an origin that pushes it over rather than
        // one that happens to absorb it.** Against `ORIGIN`'s (100, 50) the
        // negative case does not overflow at all, so a test written only that
        // way passes under a plain `+` and proves nothing.
        assert_eq!(
            aim(
                Vec2::new(f32::MAX, f32::MAX),
                false,
                CLIENT,
                Some((1, 1)),
                true
            ),
            Aim::Desktop(i32::MAX, i32::MAX)
        );
        assert_eq!(
            aim(
                Vec2::new(f32::MIN, f32::MIN),
                false,
                CLIENT,
                Some((-1, -1)),
                true
            ),
            Aim::Desktop(i32::MIN, i32::MIN)
        );
        // And an ordinary origin with a saturated pointer still lands
        // somewhere, rather than being clamped to the extreme by a second
        // saturation nobody asked for.
        assert_eq!(
            aim(Vec2::new(f32::MIN, f32::MIN), false, CLIENT, ORIGIN, true),
            Aim::Desktop(i32::MIN + 100, i32::MIN + 50)
        );
    }

    #[test]
    fn a_build_that_cannot_read_the_desktop_says_so_rather_than_guessing() {
        // The macOS and Linux answer, tested on the machine that can. The
        // canvas half works everywhere — that is the document and no platform
        // is involved — and **everything past it is now one refusal**, because
        // the interface and the desktop fail there for the same single reason.
        // Before the interface became a screen read this line asserted
        // `Aim::Interface` for the panel, which meant "read nothing" and read
        // the same as this does; what changed is that the variant now carries a
        // reading, so a platform that has none must not wear it.
        assert_eq!(
            aim(Vec2::new(4.0, 4.0), true, CLIENT, ORIGIN, false),
            Aim::Canvas
        );
        assert_eq!(
            aim(Vec2::new(4.0, 4.0), false, CLIENT, ORIGIN, false),
            Aim::Unreachable
        );
        assert_eq!(
            aim(Vec2::new(-4.0, 4.0), false, CLIENT, ORIGIN, false),
            Aim::Unreachable
        );
    }

    #[test]
    fn a_window_that_cannot_say_where_it_is_reads_nothing_off_the_desktop() {
        // Defensive rather than live: `aim` tests `desktop_readable` first, and
        // the only platform where that is true is the one whose
        // `inner_position` never fails. Kept because the `Option` is winit's
        // and a platform gaining a screen read before it gains a position is a
        // combination nothing else would catch.
        assert_eq!(
            aim(Vec2::new(-4.0, 4.0), false, CLIENT, None, true),
            Aim::Unreachable
        );
    }

    #[test]
    fn a_zero_sized_client_area_is_all_outside() {
        // Minimised, or the frame between a resize and the first paint. Every
        // position is outside the window, so nothing reads as `Interface` and
        // the desktop answers — which is right: the pointer genuinely is over
        // whatever is behind a window with no pixels.
        assert_eq!(
            aim(Vec2::ZERO, false, Vec2::ZERO, ORIGIN, true),
            Aim::Desktop(100, 50)
        );
    }

    #[test]
    fn the_strip_says_what_this_platform_can_actually_do() {
        // The line is the only thing a user of an unsupported platform ever
        // sees of this module, so it must never be empty and must never be the
        // Windows arm on a platform that is not Windows — a live-sounding
        // sentence over a gesture that does nothing is precisely the control
        // that lies. Both directions are asserted: the second is what catches
        // an arm that has stopped saying "not built" while still being the
        // arm a platform without the feature draws.
        let line = outside_line();
        assert!(!line.is_empty());
        assert!(!outside_detail().is_empty());
        assert_eq!(
            line.starts_with("Drag anywhere on the screen"),
            DESKTOP_READABLE,
            "only the platform that can do it may say it can"
        );
        assert_eq!(
            line.contains("not built"),
            !DESKTOP_READABLE,
            "and only one that cannot may say it is not built"
        );
    }

    #[test]
    fn neither_sentence_uses_an_em_dash() {
        // This project's rule for text the interface draws, and these two are
        // the only strings in this module that reach a person.
        for line in [outside_line(), outside_detail()] {
            assert!(!line.contains('—'), "an em dash in {line:?}");
        }
    }
}
