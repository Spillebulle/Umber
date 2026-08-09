//! Reading a pixel of the desktop, for the eyedropper's other half.
//!
//! In `umber-app` for the reason [`sysclip`](crate::sysclip) and
//! [`syscursor`](crate::syscursor) are: `umber-core` and `umber-render` may not
//! learn about the platform, which is the boundary that keeps them testable
//! without one. Nothing below this module knows the desktop exists, and the
//! canvas half of the eyedropper is untouched — it still goes through
//! `CanvasRenderer::pick_colour`, which reuses the screen composite pass, so
//! there is exactly one path from a pixel to a colour inside the document and
//! this adds a second one only for pixels that are outside it.
//!
//! **The decision is pure and the platform call is the thin part**, which is
//! `sysclip::decide`'s shape and is the only reason any of this is testable.
//! [`aim`] is a function of four readings — where the pointer is, how big the
//! client area is, where that area sits on the desktop, and whether this build
//! can read the desktop at all — and it is what says whether a sample belongs
//! to the canvas, to the desktop, or to nothing. [`sample`] is nine lines of
//! GDI under a `cfg`. No test touches the screen.
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
//! **There is no loupe, and that is what the overlay would have been for.** A
//! magnifier has to be drawn at the pointer, the pointer is on somebody else's
//! window, so it is an always-on-top borderless window moved once per event —
//! which is then either occluding the pixel it exists to magnify or offset from
//! it by a hand-tuned margin, and either way is a second wgpu surface and a
//! second render pass. It is the right thing to build eventually and it is not
//! built. What stands in for it is that the sample is applied *live*: the
//! colour under the pointer is the painting colour for as long as the drag
//! lasts, so the Colour module's swatch is the readout.
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
//! [`Aim::Unreachable`] the moment the pointer leaves the window, and the tool
//! options strip says why. A control that is live where it cannot work is the
//! thing this project refuses everywhere else.
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
//!   one call per pointer event is a visible flicker across every window on the
//!   machine.
//! * **Multi-monitor is right, and only because the process is DPI aware.**
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

/// What the tool options strip says where the desktop cannot be read.
///
/// Short, because it shares one unwrapped row with the sentence above it and a
/// strip does not reflow. The *why* is [`unreadable_detail`], on hover, which
/// is the split every long explanation in this interface makes.
pub const fn unreadable_reason() -> &'static str {
    if cfg!(windows) {
        // Unreachable in a shipped build — the strip draws the other sentence
        // entirely. Stated anyway so the function is total and nobody has to
        // wonder what happens if it is called.
        "Drag off the window to take one from anywhere on the screen."
    } else if cfg!(target_os = "macos") {
        "Picking outside the window is not built for macOS yet."
    } else {
        "Picking outside the window is not built for this system yet."
    }
}

/// Why not, in one hover.
///
/// Per platform rather than "not supported here", because the two reasons are
/// genuinely different and only one of them is ever likely to change: on
/// Wayland it is a deliberate property of the display server, and on macOS it
/// is a permission nobody here has the hardware to test.
pub const fn unreadable_detail() -> &'static str {
    if cfg!(windows) {
        "The colour under the pointer becomes the painting colour as you drag, \
         so the Colour module's swatch is the readout."
    } else if cfg!(target_os = "macos") {
        "It needs the Screen Recording permission, which is a prompt and an \
         entitlement, and nobody working on Umber has a Mac to test it on. \
         Picking inside the window works as it always has."
    } else {
        "On Wayland a colour off the desktop has to come from the compositor's \
         own picker, and the X11 route has not been written. Picking inside \
         the window works as it always has."
    }
}

/// Where a sample taken at some pointer position belongs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aim {
    /// Inside the window's own client area. The canvas answers, through the
    /// composite pass, exactly as an Alt-click has always done — and where the
    /// point is off the document that read declines and the colour is left
    /// alone. Nothing about Umber's own interface is ever sampled: a pick over
    /// a panel is a pick over a point that is not in the document.
    Canvas,
    /// Outside it, at these virtual-screen physical pixels.
    Desktop(i32, i32),
    /// Outside it, and this build cannot read the desktop.
    Unreachable,
}

/// Decide where a pick lands.
///
/// `pointer` and `client` are in physical window pixels — winit's unit, and
/// what `Editor::cursor` holds. `origin` is `Window::inner_position`, the
/// client area's top-left in desktop physical pixels, and it is an `Option`
/// because winit's is: a platform that cannot say answers `None`, and a
/// position that cannot be placed on the desktop is one that cannot be read
/// off it either.
///
/// `desktop_readable` is injected rather than read from [`DESKTOP_READABLE`]
/// for the reason `install::detect` takes a `Probe`: the answer this gives on a
/// machine that cannot read the desktop is the half that has to be tested on
/// the machine that can.
///
/// **The test is the client area and not the document.** Inside the window and
/// off the canvas — over a panel, over the tab strip, over the margin — is
/// [`Aim::Canvas`], which the canvas read then declines. Making it
/// [`Aim::Desktop`] instead would sample Umber's own interface off the screen
/// surface, which is a colour the palette can already provide and is a second,
/// worse route to it: the pixels there are the *theme's*, already composited
/// with whatever egui drew over them.
pub fn aim(pointer: Vec2, client: Vec2, origin: Option<(i32, i32)>, desktop_readable: bool) -> Aim {
    let inside = pointer.x >= 0.0
        && pointer.y >= 0.0
        && pointer.x < client.x.max(0.0)
        && pointer.y < client.y.max(0.0);
    if inside {
        return Aim::Canvas;
    }
    if !desktop_readable {
        return Aim::Unreachable;
    }
    let Some((ox, oy)) = origin else {
        return Aim::Unreachable;
    };
    // `floor` rather than `as i32`, which truncates towards zero and would
    // therefore round the wrong way for every position left of or above the
    // window — the exact half of the range this branch exists for.
    Aim::Desktop(ox + pointer.x.floor() as i32, oy + pointer.y.floor() as i32)
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

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: Vec2 = Vec2::new(1280.0, 800.0);
    const ORIGIN: Option<(i32, i32)> = Some((100, 50));

    #[test]
    fn inside_the_client_area_is_the_canvas() {
        for at in [
            Vec2::new(0.0, 0.0),
            Vec2::new(640.0, 400.0),
            Vec2::new(1279.0, 799.0),
        ] {
            assert_eq!(aim(at, CLIENT, ORIGIN, true), Aim::Canvas, "at {at:?}");
        }
    }

    #[test]
    fn a_position_over_a_panel_is_still_the_canvas_read_to_refuse() {
        // The rule this module could most plausibly have got the other way
        // round. Umber's own interface is inside the client area, so it is
        // `Canvas` and the composite read then declines it for being off the
        // document — rather than being read off the screen surface, which
        // would hand back the theme's own ink already composited with whatever
        // egui had drawn over it.
        assert_eq!(aim(Vec2::new(4.0, 4.0), CLIENT, ORIGIN, true), Aim::Canvas);
    }

    #[test]
    fn outside_it_lands_on_the_desktop_at_the_windows_own_offset() {
        assert_eq!(
            aim(Vec2::new(1280.0, 400.0), CLIENT, ORIGIN, true),
            Aim::Desktop(1380, 450)
        );
        assert_eq!(
            aim(Vec2::new(10.0, 800.0), CLIENT, ORIGIN, true),
            Aim::Desktop(110, 850)
        );
    }

    #[test]
    fn a_pointer_left_of_or_above_the_window_rounds_the_way_the_others_do() {
        // `as i32` truncates towards zero, so -0.5 would come back as 0 and
        // every position in the leftmost column of the drag would be read one
        // pixel to the right of where the pointer was. This is the whole
        // reason `aim` floors.
        assert_eq!(
            aim(Vec2::new(-0.5, 400.0), CLIENT, ORIGIN, true),
            Aim::Desktop(99, 450)
        );
        assert_eq!(
            aim(Vec2::new(-1.5, -1.5), CLIENT, ORIGIN, true),
            Aim::Desktop(98, 48)
        );
        // And the positive side keeps agreeing with it, so the two halves of
        // the drag are one rule rather than two.
        assert_eq!(
            aim(Vec2::new(1280.5, 800.5), CLIENT, ORIGIN, true),
            Aim::Desktop(1380, 850)
        );
    }

    #[test]
    fn a_monitor_left_of_the_primary_one_is_a_negative_coordinate() {
        // Windows puts the virtual screen's origin at the primary monitor's
        // top-left, so a second screen to the left is at negative x — and a
        // window on it has a negative `inner_position`. Nothing clamps.
        assert_eq!(
            aim(Vec2::new(10.0, 10.0), CLIENT, Some((-1920, -120)), true),
            Aim::Canvas
        );
        assert_eq!(
            aim(Vec2::new(-10.0, -10.0), CLIENT, Some((-1920, -120)), true),
            Aim::Desktop(-1930, -130)
        );
    }

    #[test]
    fn a_build_that_cannot_read_the_desktop_says_so_rather_than_guessing() {
        // The macOS and Linux answer, tested on the machine that can. Inside
        // the window is unchanged — the canvas half works everywhere.
        assert_eq!(aim(Vec2::new(4.0, 4.0), CLIENT, ORIGIN, false), Aim::Canvas);
        assert_eq!(
            aim(Vec2::new(-4.0, 4.0), CLIENT, ORIGIN, false),
            Aim::Unreachable
        );
    }

    #[test]
    fn a_window_that_cannot_say_where_it_is_reads_nothing_off_the_desktop() {
        assert_eq!(
            aim(Vec2::new(-4.0, 4.0), CLIENT, None, true),
            Aim::Unreachable
        );
    }

    #[test]
    fn a_zero_sized_client_area_is_all_outside() {
        // Minimised, or the frame between a resize and the first paint. Every
        // position is outside, and none of them is `Canvas` — which matters
        // because `Canvas` would send a coordinate into the composite read for
        // a surface that has no pixels.
        assert_eq!(
            aim(Vec2::ZERO, Vec2::ZERO, ORIGIN, true),
            Aim::Desktop(100, 50)
        );
    }

    #[test]
    fn the_strip_says_what_this_platform_can_actually_do() {
        // The string is the only thing a user of an unsupported platform ever
        // sees of this module, so it must never be empty and must never be the
        // Windows arm on a platform that is not Windows — a live-sounding
        // sentence over a gesture that does nothing is precisely the control
        // that lies.
        let reason = unreadable_reason();
        assert!(!reason.is_empty());
        assert!(!unreadable_detail().is_empty());
        assert_eq!(
            reason.starts_with("Drag off the window"),
            DESKTOP_READABLE,
            "only the platform that can do it may say it can"
        );
        assert_eq!(
            reason.contains("not built"),
            !DESKTOP_READABLE,
            "and only one that cannot may say it is not built"
        );
    }

    #[test]
    fn neither_sentence_uses_an_em_dash() {
        // This project's rule for text the interface draws, and these two are
        // the only strings in this module that reach a person.
        for line in [unreadable_reason(), unreadable_detail()] {
            assert!(!line.contains('—'), "an em dash in {line:?}");
        }
    }
}
