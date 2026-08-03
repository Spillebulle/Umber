//! Taking the arrow off the screen when the canvas is drawing its own pointer.
//!
//! [`ui::pen_cursor`](crate::ui) asks egui for `CursorIcon::None`, and that is
//! still the request — this module is only what carries it out on the one
//! platform where winit cannot. The layering is unchanged: egui re-derives the
//! cursor from what the interface asked for on every frame, `app::render` reads
//! that answer out of the frame's `PlatformOutput`, and this is called from
//! there. There is no state here and nothing latches.
//!
//! **Why winit's own hide does not reach a pen on Windows.** `CursorIcon::None`
//! becomes `Window::set_cursor_visible(false)`, and winit's Windows backend
//! implements that by setting `CursorFlags::HIDDEN` and then calling
//! `refresh_os_cursor`, which reads:
//!
//! ```text
//! let cursor_in_client = self.contains(CursorFlags::IN_WINDOW);
//! if cursor_in_client { util::set_cursor_hidden(self.contains(CursorFlags::HIDDEN)) }
//! else                { util::set_cursor_hidden(false) }
//! ```
//!
//! `IN_WINDOW` is set in exactly one place: winit's `WM_MOUSEMOVE` handler. A
//! pen on Windows Ink arrives as `WM_POINTERUPDATE`, and winit's handler for
//! that ends in `ProcResult::Value(0)` — it never calls `DefWindowProc`, and it
//! calls `SkipPointerFrameMessages` — so Windows never promotes the pointer
//! messages to legacy mouse ones. That is the same fact as "a pen produces no
//! `CursorMoved`", which CLAUDE.md already records and
//! [`Editor::pen_pointer`](crate::Editor) is built on; read from this side it
//! means `IN_WINDOW` is *never set at all* in a session driven only by a pen,
//! so the hide takes the `else` branch and is silently dropped. egui-winit then
//! dedupes on its own `current_cursor_icon`, so the request is made once, lost
//! once, and never repeated: the arrow stays on screen for the rest of the
//! session.
//!
//! **`SetCursor(NULL)` rather than `ShowCursor`.** `ShowCursor` is a per-thread
//! counter — a latch, and the same latch `pen_cursor`'s docs refuse, with the
//! added hazard that winit keeps a counter of its own and the two would fight.
//! `SetCursor` sets the shape and nothing else, `NULL` is the documented way to
//! spell "no cursor", and it is cheap enough to reissue every frame the answer
//! is still "none" — which is what makes this re-derived rather than remembered.
//! Putting the arrow *back* is deliberately not here: egui asks for a real
//! `CursorIcon` on the very frame `pen_cursor` stops asking, and egui-winit's
//! own `set_cursor` call carries it out.
//!
//! Nothing is needed on macOS or Linux: `set_cursor_visible` there is not gated
//! on anything a pen fails to produce. Neither platform has a pen path at all
//! yet, so this would be untested code on top of untestable code.

/// Draw no cursor at all, for as long as the caller keeps asking.
///
/// Called once per frame in which the interface asked for
/// `egui::CursorIcon::None`, and never otherwise.
#[cfg(windows)]
pub fn hide_now() {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetCursor;

    // SAFETY: `SetCursor` takes a cursor handle, and a null one is the
    // documented request for no cursor. It affects only the calling thread's
    // idea of the cursor shape, and this runs on the thread that owns the
    // window — `app::render` is called from the winit event loop.
    unsafe {
        SetCursor(std::ptr::null_mut());
    }
}

#[cfg(not(windows))]
pub fn hide_now() {}
