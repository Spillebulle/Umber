//! The desktop's clipboard: pictures out of Umber and into it.
//!
//! [`umber_core::Clip`] is what a copy and a paste act on and that has not
//! changed. What this adds is the other end of it — the clipboard the rest of
//! the machine shares — and it is in `umber-app` for the reason the update
//! check is: `umber-core` and `umber-render` must not learn about the
//! platform, which is the boundary that keeps them testable without one.
//!
//! # The bytes need no conversion, and that was the claim to check
//!
//! `umber_core::clipboard` holds **straight-alpha sRGB RGBA8**, and its module
//! docs say it is held that way against the day a system clipboard arrives.
//! `arboard::ImageData` is straight-alpha sRGB RGBA8, row-major, four bytes a
//! pixel. So the claim holds: [`Board::put_image`] hands `Clip::pixels` over
//! untouched and [`Board::take_image`] is [`Clip::from_rgba`] over what came
//! back. Nothing in `umber-core` was changed for any of this.
//!
//! # Which picture a paste puts down
//!
//! [`decide`], which is a pure function of two readings — what the desktop
//! holds and what Umber holds — for the reason `install::detect` is a pure
//! function of a `Probe`: it is the whole of the rule, and no test may touch
//! the real clipboard. A CI runner may have no display server at all, and a
//! test that grabs the desktop's clipboard on somebody's machine is hostile.
//!
//! The rule: **a picture on the desktop wins, unless it is the one Umber's own
//! copy put there; and where the desktop is holding no picture at all, Umber's
//! own clip is what gets pasted.**
//!
//! Both halves are load-bearing.
//!
//! * *The desktop wins.* Copying a photograph in a browser and pasting it into
//!   Umber has to put down the photograph. Preferring the internal clip
//!   whenever there is one would make Ctrl+V put down whatever Umber last
//!   copied, for ever, which is a paste that ignores the machine it is running
//!   on.
//! * *Umber's own clip wins where the desktop holds no picture.* Copying a line
//!   of text somewhere else does not throw away the region an artist copied ten
//!   seconds ago; every painting application behaves this way, and there is no
//!   text tool for a string to be pasted into.
//! * *And where the desktop holds Umber's own bytes.* This is what keeps
//!   `a_copy_and_a_paste_are_exact_inverses` — the sibling of
//!   `saving_and_reopening_does_not_move_a_pixel` — true through a copy that
//!   also went to the desktop. It is a **check rather than a trust**: arboard
//!   round trips RGBA8 through PNG on Windows and on both Linux backends, so
//!   the bytes come back identical and the comparison is satisfied. Measured on
//!   Windows rather than only read off arboard's source: a 16×16 square
//!   carrying every alpha from 0 to 255 came back byte for byte, which also
//!   rules out the DIB path having been taken in preference to the PNG one.
//!   macOS goes out through an `NSImage` and back through its TIFF
//!   representation, which nobody working on Umber can run; if that moves a
//!   byte the comparison fails, the divergence is logged, and **what the
//!   desktop actually holds is what gets pasted**. A picture that merely
//!   resembles the one Umber copied is not evidence that it is that picture,
//!   and pasting the wrong picture is the one failure here worth avoiding at
//!   any cost.
//!
//! Where a pasted picture *goes* is not decided here. That is `Clip::place`'s,
//! in `umber-core`, and a picture off the desktop is an ordinary clip: it is
//! centred on the selection or on the view, nudged back on where it fits, and
//! centred and cropped where it is larger than the canvas — with the crop said
//! out loud, which matters more for a foreign picture than for Umber's own,
//! because a screenshot is very often larger than the canvas it is going into.
//!
//! # Nothing here is threaded, deliberately
//!
//! Reading a picture off the desktop decodes a PNG, and writing one encodes
//! it — neither is instant. Both happen on the main thread anyway, for the
//! reason `export` is not threaded: a copy and a paste are explicit commands,
//! no stroke can be live by the time either runs (both put the float down and
//! finish the stroke first), and the copy already blocks on `read_layer_rect`
//! at the same keystroke.
//!
//! A paste has a second reason of its own, and it is the decisive one: the
//! order of a copy and the paste after it has to be the order they were typed
//! in. A write handed to a thread could still be in flight when the next
//! Ctrl+V reads the desktop, and [`decide`] would then find the *previous*
//! picture there, not recognise it, and put it down — the wrong picture, from
//! the one branch that exists to make that impossible.
//!
//! One bound is worth writing down because it is not Umber's to set: on X11 a
//! read waits on the *owning* process, and arboard gives that four seconds
//! before giving up. A Ctrl+V while some other application is wedged is
//! therefore a stall of up to four seconds — bounded, on an explicit keystroke,
//! and nowhere near the drawing loop, which is the same ground the blocking
//! readbacks stand on.
//!
//! The cost is real and is stated rather than hidden: with nothing selected, a
//! copy takes the whole canvas, so on the 10000² document the Undo section uses
//! as its bound Ctrl+C hands 400 MB to a PNG encoder. That is on top of the
//! 400 MB readback the copy already paid for, and it is the same shape of cost
//! the undo budget's own note describes. A failure — an allocation refused, a
//! desktop with no clipboard at all — is logged once and carried on from, and
//! Umber's own clip is unaffected, so copy and paste inside Umber go on
//! working.
//!
//! # What the desktop cannot promise
//!
//! On X11 and on Wayland the clipboard's contents belong to the process that
//! put them there, so a picture copied out of Umber lives for as long as Umber
//! does. arboard offers what it survives on afterwards — an X11 handover to a
//! running clipboard manager, taken on the way out — and where no manager is
//! running, closing Umber empties the clipboard. That is how every X11
//! application behaves and it is not something Umber can fix from inside.
//!
//! `SetExtLinux::wait` is arboard's answer for the other shape of this problem
//! and is **refused here**: it serves clipboard requests until somebody else
//! copies something, which for a program that is going to stay open means
//! blocking the thread that draws. It is for a command-line tool that copies
//! and exits.
//!
//! On Wayland the picture goes through `wlr-data-control`/`ext-data-control`,
//! which not every compositor implements — GNOME's does not. arboard tries it
//! and **falls back to X11** where it is missing, which under XWayland is the
//! ordinary case and is bridged back to the Wayland clipboard by the compositor
//! itself, so this is not the hole it first looks like. A session with neither
//! is one where the write fails; it is logged, Umber's own copy and paste go on
//! working, and text is unaffected either way, because egui-winit reaches the
//! Wayland clipboard through `wl_data_device` rather than through data-control.
//! None of this is claimed anywhere on screen.

use umber_core::Clip;

/// What a paste should put down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Paste {
    /// Umber's own clip. Either the desktop is holding no picture, or it is
    /// holding byte for byte the one Umber's copy put there — so the exact
    /// bytes that came off the layer are the ones that go back on to it.
    Mine(Clip),
    /// A picture Umber did not put on the desktop's clipboard.
    Theirs(Clip),
    /// Neither has anything. A paste does nothing at all.
    Nothing,
}

/// Choose between the desktop's picture and Umber's own.
///
/// See the module docs for the argument. `system` is what the desktop is
/// holding, already read; `mine` is `Editor::clipboard`.
///
/// The answer is a pure function of the two readings. The one log line is
/// observation and nothing reads it — it is the only way anybody would ever
/// find out that a platform's clipboard had started moving a byte.
pub fn decide(system: Option<Clip>, mine: Option<&Clip>) -> Paste {
    match (system, mine) {
        // The desktop is still holding what Umber's own copy put there, so
        // **Umber's own bytes** are what go back on to the layer, not the ones
        // that came back through the transport. Indistinguishable today — the
        // guard is equality — and written this way round deliberately: the rule
        // is "our copy wins", and it should not quietly become "the transport's
        // copy wins" if `Clip`'s equality is ever loosened.
        (Some(theirs), Some(mine)) if theirs == *mine => Paste::Mine(mine.clone()),
        (Some(theirs), Some(mine)) => {
            // Either somebody copied something else — the ordinary case — or a
            // platform's clipboard moved a byte of Umber's own picture. The two
            // are indistinguishable from here, and only one of the two possible
            // mistakes is survivable, so the desktop is believed.
            if theirs.size() == mine.size() {
                log::debug!(
                    "the desktop holds a {} × {} picture that is not the one Umber copied",
                    theirs.size().x,
                    theirs.size().y,
                );
            }
            Paste::Theirs(theirs)
        }
        (Some(theirs), None) => Paste::Theirs(theirs),
        // No picture on the desktop: text, a file list, an image arboard could
        // not read, or an empty clipboard. Umber's own copy is not thrown away
        // by any of those.
        (None, Some(mine)) => Paste::Mine(mine.clone()),
        (None, None) => Paste::Nothing,
    }
}

/// Umber's one hold on the desktop's clipboard.
///
/// Built lazily and at most once: a machine with no clipboard at all — a
/// headless session, a Wayland compositor without the data-control protocol —
/// must still start and still paint, exactly as a machine with no tablet driver
/// does. A failure is logged where it happens and the board stays shut; every
/// call after that is a cheap `None`.
///
/// This is the *second* `arboard::Clipboard` in the process: egui-winit holds
/// one for the interface's text fields. That is safe rather than lucky — on
/// X11 arboard keeps one process-global connection and one serving thread
/// behind a `static`, so both handles are the same owner; on Windows the
/// clipboard is opened and closed around each operation; on macOS both are
/// handles on the one `NSPasteboard`.
#[derive(Default)]
pub struct Board {
    /// `None` before the first use and after a failure to open one; `tried`
    /// tells the two apart.
    #[cfg(not(target_os = "android"))]
    board: Option<arboard::Clipboard>,
    #[cfg(not(target_os = "android"))]
    tried: bool,
}

impl Board {
    /// Offer `clip` to the rest of the machine.
    ///
    /// Best effort by construction: Umber's own clipboard is written by the
    /// caller either way, so a desktop that will not take it costs the artist
    /// nothing they can see. Reported once, at the level a failed autosave is —
    /// a paint application must not raise a dialog on Ctrl+C.
    #[cfg(not(target_os = "android"))]
    pub fn put_image(&mut self, clip: &Clip) {
        let size = clip.size();
        let Some(board) = self.board() else { return };
        let image = arboard::ImageData {
            width: size.x as usize,
            height: size.y as usize,
            // Straight-alpha sRGB RGBA8 on both sides. See the module docs.
            bytes: std::borrow::Cow::Borrowed(clip.pixels()),
        };
        if let Err(e) = board.set_image(image) {
            log::warn!("the desktop's clipboard would not take the picture: {e}");
        }
    }

    /// What the desktop is holding, if it is holding a picture.
    ///
    /// `None` covers every ordinary case as well as every failure — text on the
    /// clipboard, nothing on it, a format arboard cannot read — because the
    /// caller does the same thing with all of them: falls back to Umber's own
    /// clip. Only a failure that is not "there is no picture" is logged.
    #[cfg(not(target_os = "android"))]
    pub fn take_image(&mut self) -> Option<Clip> {
        let board = self.board()?;
        let image = match board.get_image() {
            Ok(image) => image,
            Err(arboard::Error::ContentNotAvailable) => return None,
            Err(e) => {
                log::warn!("could not read the desktop's clipboard: {e}");
                return None;
            }
        };
        let width = u32::try_from(image.width).ok()?;
        let height = u32::try_from(image.height).ok()?;
        // `from_rgba` refuses a buffer that does not match its own dimensions,
        // which is the guard against a decoder that answered something other
        // than what it said it had.
        Clip::from_rgba(width, height, image.bytes.into_owned())
    }

    #[cfg(not(target_os = "android"))]
    fn board(&mut self) -> Option<&mut arboard::Clipboard> {
        if !self.tried {
            self.tried = true;
            match arboard::Clipboard::new() {
                Ok(board) => self.board = Some(board),
                Err(e) => log::warn!("no desktop clipboard on this machine: {e}"),
            }
        }
        self.board.as_mut()
    }

    /// Android has no arboard backend, so the dependency is not built there —
    /// the same gate egui-winit puts on its own. Umber's internal clipboard is
    /// untouched by this, so copy and paste inside the application work exactly
    /// as they did before any of this existed.
    #[cfg(target_os = "android")]
    pub fn put_image(&mut self, _clip: &Clip) {}

    #[cfg(target_os = "android")]
    pub fn take_image(&mut self) -> Option<Clip> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(w: u32, h: u32, px: [u8; 4]) -> Clip {
        let pixels = px
            .iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect();
        Clip::from_rgba(w, h, pixels).expect("a clip")
    }

    /// A picture somebody copied in another application is what Ctrl+V puts
    /// down, even with something already on Umber's own clipboard. Preferring
    /// the internal clip whenever there is one is the tempting simplification
    /// and makes a paste ignore the machine it is running on.
    #[test]
    fn a_picture_from_another_application_wins() {
        let theirs = clip(4, 4, [10, 20, 30, 255]);
        let mine = clip(4, 4, [200, 100, 50, 255]);
        assert_eq!(
            decide(Some(theirs.clone()), Some(&mine)),
            Paste::Theirs(theirs)
        );
    }

    /// **The exactness promise.** The desktop is holding what Umber's own copy
    /// put there, so the bytes that came off the layer are the ones that go
    /// back on to it — not the bytes that came back through the platform's
    /// clipboard. `a_copy_and_a_paste_are_exact_inverses` is the test in
    /// `umber-core` this keeps true once a copy also leaves the process.
    #[test]
    fn umbers_own_copy_comes_back_out_of_umbers_own_clip() {
        let mine = clip(3, 5, [200, 100, 50, 128]);
        // What a lossless transport hands back: the same bytes.
        let round_tripped = mine.clone();
        assert_eq!(
            decide(Some(round_tripped), Some(&mine)),
            Paste::Mine(mine.clone()),
        );
    }

    /// Copying a line of text somewhere else must not throw away the region an
    /// artist copied ten seconds ago. There is no text tool for a string to be
    /// pasted into, so "the desktop holds no picture" is the ordinary state of
    /// the machine rather than an error.
    #[test]
    fn text_on_the_desktop_leaves_umbers_own_picture_alone() {
        let mine = clip(2, 2, [1, 2, 3, 4]);
        assert_eq!(decide(None, Some(&mine)), Paste::Mine(mine));
    }

    /// Nothing anywhere. A paste has to do nothing at all rather than put down
    /// an empty rectangle and call it an edit — the same rule
    /// `copying_nothing_leaves_the_clipboard_alone` states for the other end.
    #[test]
    fn nothing_on_either_clipboard_pastes_nothing() {
        assert_eq!(decide(None, None), Paste::Nothing);
    }

    /// A picture off the desktop with nothing on Umber's own clipboard — the
    /// first paste of a fresh session, which is most of why any of this exists.
    #[test]
    fn a_first_paste_of_the_session_takes_what_the_desktop_holds() {
        let theirs = clip(8, 2, [0, 255, 0, 255]);
        assert_eq!(decide(Some(theirs.clone()), None), Paste::Theirs(theirs));
    }

    /// Same size, different pixels. Indistinguishable from Umber's own copy
    /// having been moved a byte by the platform, and the desktop is believed
    /// anyway: of the two possible mistakes only one — putting down a picture
    /// nobody copied — is unsurvivable.
    #[test]
    fn a_same_sized_picture_that_is_not_umbers_is_still_theirs() {
        let mine = clip(4, 4, [200, 100, 50, 255]);
        let theirs = clip(4, 4, [200, 100, 51, 255]);
        assert_eq!(
            decide(Some(theirs.clone()), Some(&mine)),
            Paste::Theirs(theirs)
        );
    }
}
