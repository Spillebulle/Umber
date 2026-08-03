//! Taking the arrow off the screen when the canvas is drawing its own pointer.
//!
//! `ui::pen_cursor` asks egui for `CursorIcon::None`, and that is
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
//! **Only while the window has focus — and that guard is not here.** The cursor
//! shape is a shared, global thing rather than this thread's; MSDN's rule is
//! that a window sets it only over its own client area, which is why winit does
//! this from a `WM_SETCURSOR` handler gated on `HTCLIENT`. Umber has no such
//! handler to sit in and repaints for reasons of its own — an autosave notice,
//! an update answer, an egui animation — so a pen left hovering over the canvas
//! while the user works elsewhere would blank the cursor wherever it now is,
//! once per repaint.
//!
//! The guard was tried *here*, beside the call, and that was wrong in a way
//! worth writing down: the **request** then still said "none", so egui-winit
//! deduped it against `current_cursor_icon`, never called `set_cursor`, and the
//! blank shape this module had already installed stayed in force across the
//! whole desktop — with no later frame able to take it back, because nothing
//! had changed as far as egui was concerned. Alt-Tab away by keyboard with the
//! pen hovering and that is the state. It is exactly "a window with no pointer
//! in it and no way to say so", which is what `pen_cursor` rejects
//! `set_cursor_visible` for, arrived at from the other side.
//!
//! So focus lives in [`Editor::pen_dot`](crate::Editor::pen_dot), in what the
//! interface asks for. Unfocused, egui asks for a real `CursorIcon`, the dedupe
//! passes, winit's `set_cursor` restores the arrow on the spot, and this
//! function is not called at all because `cursor_icon` is no longer `None`. One
//! condition, and the two halves cannot disagree.
//!
//! Nothing is needed on macOS or Linux: `set_cursor_visible` there is not gated
//! on anything a pen fails to produce. Neither platform has a pen path at all
//! yet, so this would be untested code on top of untestable code.
//!
//! **Nothing in the test suite covers this file.** Delete it and every test
//! still passes: what they pin is that Umber *asks* for no cursor in the right
//! circumstances, which is `Editor::pen_dot` and is a rule. Whether the ask
//! reaches the screen is a property of Windows, of a driver and of a tablet
//! nobody working on Umber has — so the evidence for it is the reading in
//! Settings → Input & pen, and that is why the reading exists.

/// Draw no cursor at all, for as long as the caller keeps asking.
///
/// Called once per frame in which the interface asked for
/// `egui::CursorIcon::None`, and never otherwise. Focus is already folded into
/// that request — see the module docs — so there is nothing to re-test here.
#[cfg(windows)]
pub fn hide_now() {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetCursor;

    // SAFETY: `SetCursor` takes a cursor handle and a null one is the
    // documented request for no cursor; there is no pointer to dereference and
    // no failure to check. It must be called from the thread that owns the
    // window, which this is — `app::render` runs on the winit event loop — and
    // only while the cursor is ours to set, which `pen_dot`'s focus test is.
    unsafe {
        SetCursor(std::ptr::null_mut());
    }
}

#[cfg(not(windows))]
pub fn hide_now() {}
