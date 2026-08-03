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
//! means the hide takes the `else` branch and is silently dropped. egui-winit
//! then dedupes on its own `current_cursor_icon`, so the request is made once,
//! lost once, and never repeated: the arrow stays on screen for the rest of the
//! session.
//!
//! **Do not disprove this with a mouse plugged in.** In a pen-only session
//! `IN_WINDOW` is never set at all, but the ordinary case is a mixed one, and
//! there the flag is a *one-way* latch: `WM_MOUSELEAVE` clears it, and the
//! leave is tracked against the *cursor position*, which a pen does move. So a
//! mouse sets it, the pen then carries the cursor out of the window and clears
//! it, and nothing can set it again without a real mouse move. Hiding
//! therefore works right up until the first time somebody picks up the pen,
//! which is the one moment nobody tests.
//!
//! **And a second gate above that one**, which is why this reads
//! `PlatformOutput` rather than reaching into egui-winit to make it retry.
//! `apply_cursor` returns immediately when its own `pointer_pos_in_points` is
//! `None`, and `on_touch` clears that on `TouchPhase::Ended` — so from the
//! moment a pen leaves the glass until it is put down again, the icon is not
//! applied at all, whatever the flag above says. A hovering pen does keep it
//! set (a hover is a `Moved`, which goes through `on_cursor_moved`), so the
//! `IN_WINDOW` gate is the operative one *during* a hover and this one is the
//! operative one just after a stroke. Reading what the frame *asked for*, out
//! of the `PlatformOutput` itself, is upstream of both.
//!
//! **`SetCursor(NULL)` rather than `ShowCursor`.** `ShowCursor` is a *counter*,
//! and winit keeps a `static AtomicBool` beside its own use of it
//! (`util::set_cursor_hidden`) to avoid double-counting — so a second caller
//! desynchronises that latch permanently, which is the same latch
//! `pen_cursor`'s docs refuse plus a shared one to corrupt. `SetCursor` sets
//! the shape and nothing else, `NULL` is the documented way to spell "no
//! cursor", and it is cheap enough to reissue every frame the answer is still
//! "none" — which is what makes this re-derived rather than remembered.
//! Putting the arrow *back* is deliberately not here: egui asks for a real
//! `CursorIcon` on the very frame `pen_cursor` stops asking, and winit's
//! `set_cursor` calls `SetCursor` on the spot rather than waiting for a
//! `WM_SETCURSOR` that, by the argument above, a pen never provokes.
//!
//! **Only while the window has focus.** The cursor shape is a shared, global
//! thing, not this thread's — MSDN's rule is that a window sets it only over
//! its own client area, which is why winit does this from a `WM_SETCURSOR`
//! handler gated on `HTCLIENT`. Umber has no such handler to sit in, and it
//! repaints for reasons of its own (an autosave notice, an update answer, an
//! egui animation). Without the guard, a pen left hovering over the canvas
//! while the user works in another application blanks the cursor wherever it
//! now is on the desktop, once per repaint. Focus is not the same test as
//! `HTCLIENT` and is deliberately the cheaper one: `pen_dot` has already said
//! the pointer is over the canvas, so focus is the only part left.
//!
//! Nothing is needed on macOS or Linux: `set_cursor_visible` there is not gated
//! on anything a pen fails to produce. Neither platform has a pen path at all
//! yet, so this would be untested code on top of untestable code.

/// Draw no cursor at all, for as long as the caller keeps asking.
///
/// Called once per frame in which the interface asked for
/// `egui::CursorIcon::None` *and* the window had focus, and never otherwise.
#[cfg(windows)]
pub fn hide_now() {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetCursor;

    // SAFETY: `SetCursor` takes a cursor handle and a null one is the
    // documented request for no cursor; there is no pointer to dereference and
    // no failure to check. It must be called from the thread that owns the
    // window, which this is — `app::render` runs on the winit event loop — and
    // only while the cursor belongs to us, which is the caller's focus guard.
    unsafe {
        SetCursor(std::ptr::null_mut());
    }
}

#[cfg(not(windows))]
pub fn hide_now() {}
