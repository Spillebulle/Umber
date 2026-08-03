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
//! [`decide`], which is a pure function of three readings — what the desktop
//! holds, what Umber holds, and [`OnDesktop`]: what the desktop *should* be
//! handing back for Umber's own clip. That is `install::detect`'s shape and it
//! is for `install::detect`'s reason: it is the whole of the rule, and **no
//! test may touch the real clipboard.** A CI runner may have no display server
//! at all, and a test that grabs the desktop's clipboard on somebody's machine
//! is hostile.
//!
//! The rule: **a picture on the desktop wins, unless it is the one Umber's own
//! copy put there, or Umber's own copy never got there; and where the desktop
//! is holding no picture at all, Umber's own clip is what gets pasted.**
//!
//! Every clause is load-bearing.
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
//! * *And where the copy never reached the desktop.* **This clause exists
//!   because leaving it out was a bug**, and a silent one. A write can fail —
//!   a Windows global allocation refused a 400 MB picture is the realistic
//!   case — and when it does the desktop goes on holding whatever it held
//!   *before* the copy. Without this clause the next Ctrl+V compares that older
//!   picture against Umber's new clip, finds them different, believes the
//!   desktop and **puts down something the artist did not copy**. There is no
//!   ordering to appeal to: Umber cannot know when the desktop's picture was
//!   put there, only whether its own reached it. So where it did not, the copy
//!   Umber knows about is the most recent thing Umber knows about, and it wins.
//!   The cost is stated rather than hidden — in a session where the write keeps
//!   failing, a picture copied in *another* application will not paste in —
//!   and it is the far smaller of the two, because that case is visible (the
//!   wrong picture arrives as a float, and Escape throws it away) where the
//!   other is a copy and a paste that quietly disagree.
//! * *And where the desktop holds Umber's own copy.* This is what keeps
//!   `a_copy_and_a_paste_are_exact_inverses` — the sibling of
//!   `saving_and_reopening_does_not_move_a_pixel` — true through a copy that
//!   also went to the desktop. The bytes that go back on to the layer are the
//!   ones that came off it, never the ones that came back through the machine.
//!
//! # Recognising Umber's own copy, on a clipboard that changes it
//!
//! The third reading is a **picture, not a flag**, and that is the whole of
//! this section. "Is the desktop still holding what Umber put there" cannot be
//! answered by comparing against `Editor::clipboard`, because a platform whose
//! clipboard does not hand back the bytes it was given is holding something
//! that is *not* that clip and is nonetheless Umber's copy. Comparing against
//! the clip would fail to recognise it, believe the desktop, and paste the
//! mangled bytes — a copy and a paste straight back coming out different, which
//! is wrong pixels, silently, and a **regression** on any platform it happens
//! on, since an internal clipboard was exact before there was a desktop one.
//!
//! So [`Board::put_image`] keeps the **echo**: the picture read straight back
//! after a successful write, which is by construction what the desktop will
//! hand back next time. [`decide`] compares against that. It is correct on a
//! lossy transport and, unlike a size or a shape test, it has no false
//! positive — a picture of the same shape is not the same picture, and
//! `an_echo_does_not_make_another_applications_picture_lose` pins that the echo
//! does not swallow a photograph somebody copied in a browser.
//!
//! **The echo is only taken where the transport is not known to be exact**, and
//! [`TRANSPORT_IS_EXACT`] is where that is decided and evidenced. Windows was
//! measured — every alpha from 0 to 255, byte for byte — and arboard's X11 and
//! Wayland backends encode and decode `image/png` from and to RGBA8, which is
//! lossless by construction. Those two pay nothing at all: no second read, and
//! no second copy of the picture in memory, because the bytes to compare
//! against are the clip's own ([`OnDesktop::TheClipItself`]). Everything else
//! pays one extra decode per copy to be correct rather than fast, which is the
//! direction this project takes that trade everywhere.
//!
//! **Nobody working on Umber has a Mac, and no part of the macOS clipboard path
//! has ever been run** — the same statement the pen and the mobile targets are
//! held to, and it is why the gate is a `const` and not a `cfg`: the echo
//! compiles on every platform, which is the only check on it anybody here can
//! perform. What is *suspected* there is that arboard writes an `NSImage` built
//! from a `CGImage` with straight alpha and reads back that image's TIFF
//! representation, and that Cocoa's bitmap representations carry premultiplied
//! alpha where `image`'s TIFF decoder does not undo it. If that is right the
//! echo is what makes it harmless; if it is wrong, macOS pays one read per copy
//! for nothing, which is the cheap way to be wrong. Note that a premultiply is
//! the exact identity on anything fully opaque, so an echo that agrees on one
//! picture says nothing about the next — which is why a first agreeing echo
//! does **not** promote the platform to exact.
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
//! **The cost was measured, because guessing it from `measure-history.rs`'s
//! 1.6 ms/MB would have been wrong by five times.** That figure is PNG at
//! `Compression::Fast`; arboard encodes at `image`'s default level, and on
//! Windows it writes a *second* copy as an uncompressed `CF_DIBV5` beside the
//! PNG because some applications only read one of the two. Timed on this
//! machine over 4, 16 and 64 MB pictures, in release: **about 8 ms per megabyte
//! each way**, both `set_image` and `get_image`. So an ordinary selection is
//! imperceptible, a 2048² region is about a sixth of a second, and Ctrl+C with
//! nothing selected on the 10000² document the Undo section uses as its bound
//! is **roughly three seconds** — on top of the 400 MB readback that copy
//! already paid for. On a platform that needs the echo, add the read to that.
//! That is a freeze an artist would feel, and it is stated here rather than
//! hidden. It is not gated on a size, because any threshold would be a number
//! nobody measured and the effect of crossing it would be a copy that silently
//! did not leave Umber.
//!
//! ### And nothing on screen says so, which was checked rather than assumed
//!
//! This project's rule is that a control which lies is worse than one that is
//! not drawn, and a three-second freeze with nothing on screen reads as a hang.
//! Three ways to say something were looked at and all three were refused, so
//! the conclusion is written down rather than left to be re-derived:
//!
//! * **A progress bar is impossible and would be the lying control.** The whole
//!   copy is one blocking call sequence — `read_layer_rect`, then arboard's
//!   encode — and neither reports progress, so the bar could only animate over
//!   something it does not know. `Stage::progress` returning `Option` and
//!   drawing an empty track is the same refusal in the update dialog.
//! * **A wait cursor cannot be relied on.** Setting it before the block means
//!   the OS re-asserting it through a message pump that, by definition, is not
//!   running — so whether it appears is a platform question nobody here can
//!   answer, and a cursor that changes on one platform and not another is worse
//!   than one that never does.
//! * **A notice *before* the work is the one that would actually work**, and it
//!   is a real change rather than a line: the copy would have to become two
//!   phases — a frame that draws and submits "Copying…", then the blocking work
//!   on the next — which is a pending-copy state on `UmberApp`, a banner to
//!   clear afterwards, and a decision about when the word appears. Showing it
//!   on *every* copy is a flicker on the ordinary small ones; showing it only
//!   on large ones is the size threshold refused above, though in a much weaker
//!   form, since being wrong about it costs a word rather than a copy. It is
//!   the thing to build if this is ever worth building; it is not a comment.
//!
//! Two more bounds are not Umber's to set. On X11 a read waits on the *owning*
//! process and arboard gives that four seconds, so a Ctrl+V while another
//! application is wedged stalls for that long. And a paste makes **two** such
//! round trips, not one: egui-winit sees the keystroke first and reads the
//! desktop's *text* for its own `Event::Paste` whether or not a text field has
//! the keyboard, and then [`Board::take_image`] reads the picture. All of it is
//! bounded, on an explicit keystroke, and nowhere near the drawing loop, which
//! is the same ground the blocking readbacks stand on.
//!
//! A failure — an allocation refused, a desktop with no clipboard at all — is
//! logged and carried on from, and Umber's own clip is unaffected, so copy and
//! paste inside Umber go on working. It is also *remembered*; see the third
//! clause of the rule above for why that is not optional.
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
    /// Umber's own clip: the desktop is holding no picture, or it is holding
    /// byte for byte the one Umber's copy put there, or Umber's copy never
    /// reached it. In all three the exact bytes that came off the layer are the
    /// ones that go back on to it.
    Mine(Clip),
    /// A picture Umber did not put on the desktop's clipboard.
    Theirs(Clip),
    /// Neither has anything. A paste does nothing at all.
    Nothing,
}

/// What the desktop should be handing back for Umber's own clip, if it is
/// still holding it.
///
/// This is the third reading [`decide`] takes, and it is a `Clip` rather than a
/// boolean because **the bytes a lossy transport gives back for our own picture
/// are not our own picture**. Comparing what the desktop holds against
/// `Editor::clipboard` only recognises our copy where the round trip is exact;
/// comparing it against what the transport actually *echoed* recognises it
/// everywhere, and still pastes the exact bytes off the layer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OnDesktop {
    /// Nothing of Umber's is there: no copy has been made, or the write was
    /// refused.
    #[default]
    Nothing,
    /// Umber's clip is there, and this platform hands back what it was given —
    /// so the bytes to compare against are `Editor::clipboard`'s own and no
    /// second copy of them is held.
    TheClipItself,
    /// Umber's clip is there, and this is what the desktop hands back for it.
    /// Only reached where [`TRANSPORT_IS_EXACT`] is false.
    Echo(Clip),
}

/// Choose between the desktop's picture and Umber's own.
///
/// See the module docs for the argument. `system` is what the desktop is
/// holding, already read; `mine` is `Editor::clipboard`; `on_desktop` is
/// [`Board::on_desktop`].
///
/// The answer is a pure function of the three readings — no clipboard, no
/// display server, no platform. The one log line is observation and nothing
/// reads it.
pub fn decide(system: Option<Clip>, mine: Option<&Clip>, on_desktop: &OnDesktop) -> Paste {
    // What the desktop would be handing back if it were still holding Umber's
    // copy. `None` means it is not — never written, or the write was refused.
    let expected = match on_desktop {
        OnDesktop::Nothing => None,
        OnDesktop::TheClipItself => mine,
        OnDesktop::Echo(echo) => Some(echo),
    };
    match (system, mine) {
        // The desktop is still holding what Umber's own copy put there, so
        // **Umber's own bytes** are what go back on to the layer, not the ones
        // that came back through the transport. On an exact platform those are
        // the same bytes; on one that moves a byte they are not, and that
        // difference is the whole reason `expected` is a picture rather than a
        // flag.
        (Some(theirs), Some(mine)) if expected == Some(&theirs) => Paste::Mine(mine.clone()),
        // Umber's copy never reached the desktop, so what is sitting there
        // predates it as far as Umber can tell — and believing it would put
        // down a picture the artist did not copy. See the module docs: leaving
        // this case out was a bug and a silent one.
        (Some(_), Some(mine)) if expected.is_none() => Paste::Mine(mine.clone()),
        (Some(theirs), Some(_)) => {
            // Umber's copy did reach the desktop and something else is there
            // now, so somebody copied something else. That is the whole of what
            // this branch means now: with the echo in hand, "the transport
            // moved a byte" is no longer one of the readings it could be, which
            // is why what used to be a `warn` about that is a `debug` about
            // this.
            log::debug!(
                "the desktop holds a {} × {} picture that is not the one Umber put there",
                theirs.size().x,
                theirs.size().y,
            );
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
/// **The gate is Android *and* iOS**, which is what egui-winit's own *code*
/// gates on even though its manifest names only Android. arboard has no iOS
/// backend either: its `cfg` for the X11/Wayland one is `all(unix, not(any(
/// macos, android, emscripten)))`, so iOS falls into it and an iOS build would
/// go looking for an X server. Matching the manifest rather than the code is
/// how "architecturally prepared" quietly stops being true.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
pub struct Board {
    /// `None` before the first use and after a failure to open one; `tried`
    /// tells the two apart.
    board: Option<arboard::Clipboard>,
    tried: bool,
    /// What the desktop should hand back for the clip the editor is holding.
    ///
    /// Written by the two things that can put a picture there and by nothing
    /// else, so it cannot drift out of step with `Editor::clipboard` the way a
    /// flag beside that field at three call sites would. Read by
    /// [`Board::on_desktop`] and fed to [`decide`].
    on_desktop: OnDesktop,
}

/// Whether this platform's clipboard is known to hand back exactly the bytes it
/// was given.
///
/// Where it is false, [`Board::put_image`] reads the picture straight back and
/// keeps that **echo** to compare against later — one extra decode per copy,
/// paid to be right rather than fast. Where it is true nothing extra happens at
/// all.
///
/// * **Windows: measured.** A 16×16 square carrying every alpha from 0 to 255
///   through `set_image` and `get_image` came back byte for byte. arboard
///   writes a PNG *and* a `CF_DIBV5` and reads the PNG back first, so the
///   measurement also rules out the DIB path having been preferred.
/// * **X11 and Wayland: read off arboard's source, which is airtight here** —
///   both backends encode `image/png` from RGBA8 and decode it back to RGBA8,
///   and PNG is lossless. Nobody working on Umber has run Linux either, so this
///   is the weaker of the two claims; it is included because the argument does
///   not depend on anything a platform could vary.
/// * **Everything else, macOS included: not known.** See the module docs for
///   what is suspected there and why guessing in the other direction would be
///   wrong pixels rather than a slow copy.
///
/// A `const` rather than a `cfg` deliberately: the echo then **compiles on
/// every platform**, which — since nobody here can run macOS — is the only
/// check on it anybody working on Umber can actually perform. The branch is a
/// constant, so the platforms that do not need it pay nothing at run time.
/// The second arm is arboard's own `cfg` for its X11/Wayland module, plus iOS —
/// which arboard does *not* exclude, and which is why [`Board`] gates it out
/// itself. Spelled as the set the evidence is about rather than as "not macOS",
/// so a platform arboard grows a new backend for lands in the unknown group
/// and pays for the echo until somebody checks it.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const TRANSPORT_IS_EXACT: bool = cfg!(any(
    target_os = "windows",
    all(
        unix,
        not(any(
            target_os = "macos",
            target_os = "android",
            target_os = "ios",
            target_os = "emscripten"
        ))
    ),
));

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Board {
    /// Offer `clip` to the rest of the machine, remembering what the desktop
    /// should hand back for it.
    ///
    /// Remembered rather than returned, so the caller cannot forget to carry
    /// the answer: it is read back through [`Board::on_desktop`] at the one
    /// place that needs it. And it is not decoration. Umber's own clipboard is
    /// written by the caller either way, so a refusal costs the artist nothing
    /// they can *see* — but it leaves the desktop holding an older picture, and
    /// [`decide`] would otherwise believe that picture over the one just
    /// copied. Logged as well, at the level a failed autosave is: a paint
    /// application must not raise a dialog on Ctrl+C.
    ///
    /// **The echo.** Where [`TRANSPORT_IS_EXACT`] is false the picture is read
    /// straight back and *that* is what is kept, because on a platform whose
    /// clipboard does not return what it was given, what it returns for our own
    /// picture is the only thing a later paste can recognise it by. An echo
    /// that cannot be read leaves [`OnDesktop::Nothing`] — the same state a
    /// refused write leaves, and for the same reason: not knowing what is there
    /// must fall towards pasting Umber's own copy, never towards pasting
    /// something else's.
    pub fn put_image(&mut self, clip: &Clip) {
        let size = clip.size();
        self.on_desktop = OnDesktop::Nothing;
        let Some(board) = self.board() else { return };
        let image = arboard::ImageData {
            width: size.x as usize,
            height: size.y as usize,
            // Straight-alpha sRGB RGBA8 on both sides. See the module docs.
            bytes: std::borrow::Cow::Borrowed(clip.pixels()),
        };
        if let Err(e) = board.set_image(image) {
            log::warn!(
                "the desktop's clipboard would not take the picture, so it stays inside \
                 Umber: {e}"
            );
            return;
        }
        self.on_desktop = if TRANSPORT_IS_EXACT {
            OnDesktop::TheClipItself
        } else {
            match self.read_image() {
                Some(echo) => {
                    if echo == *clip {
                        // The suspicion about this platform was unfounded for
                        // this picture at least. Kept as an echo anyway rather
                        // than promoted to `TheClipItself`: the transport that
                        // is suspected here is a premultiply, which is the
                        // exact identity on anything fully opaque, so one
                        // agreeing picture is no evidence at all about the next
                        // one. It costs a `Clip` of memory and nothing else.
                        log::debug!("the clipboard echoed this picture unchanged");
                    } else {
                        log::info!(
                            "this platform's clipboard did not hand back the picture it was \
                             given, so the copy is recognised by its echo instead"
                        );
                    }
                    OnDesktop::Echo(echo)
                }
                None => {
                    log::warn!(
                        "the picture was put on the desktop's clipboard but could not be read \
                         back, so a later paste will prefer Umber's own copy"
                    );
                    OnDesktop::Nothing
                }
            }
        };
    }

    /// Note that the editor's clip came *off* the desktop.
    ///
    /// It is [`OnDesktop::TheClipItself`] on **every** platform, exact
    /// transport or not, and that is not an oversight: the clip was obtained by
    /// reading the desktop, so the bytes the desktop hands back for it are — by
    /// construction — the bytes in hand. There is nothing to echo.
    ///
    /// Called where a foreign picture is adopted, and only once that paste has
    /// actually happened — the same place `Editor::clipboard` is written, so
    /// the two cannot disagree.
    pub fn note_adopted(&mut self) {
        self.on_desktop = OnDesktop::TheClipItself;
    }

    /// What the desktop should hand back for the clip the editor holds.
    pub fn on_desktop(&self) -> &OnDesktop {
        &self.on_desktop
    }

    /// What the desktop is holding, if it is holding a picture.
    ///
    /// `None` covers every ordinary case as well as every failure — text on the
    /// clipboard, nothing on it, a format arboard cannot read — because the
    /// caller does the same thing with all of them: falls back to Umber's own
    /// clip. Only a failure that is not "there is no picture" is logged.
    pub fn take_image(&mut self) -> Option<Clip> {
        self.read_image()
    }

    /// The one read. [`Board::take_image`] is a paste asking; the echo in
    /// [`Board::put_image`] is a copy asking, and neither may have its own
    /// notion of what a picture off the clipboard is.
    fn read_image(&mut self) -> Option<Clip> {
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
}

/// Where arboard is not built there is no desktop clipboard to reach, so
/// nothing of Umber's is ever on one — which is exactly right: [`decide`] then
/// never prefers a picture Umber did not put there, and since
/// [`Board::take_image`] answers `None` as well, copy and paste inside Umber
/// work exactly as they did before any of this existed.
#[cfg(any(target_os = "android", target_os = "ios"))]
#[derive(Default)]
pub struct Board {
    nothing: OnDesktop,
}

#[cfg(any(target_os = "android", target_os = "ios"))]
impl Board {
    pub fn put_image(&mut self, _clip: &Clip) {}

    pub fn note_adopted(&mut self) {}

    pub fn on_desktop(&self) -> &OnDesktop {
        &self.nothing
    }

    pub fn take_image(&mut self) -> Option<Clip> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a copy on an exact platform leaves behind, named so the third
    /// argument of a `decide` below says what it means at the call site.
    const PUT_THERE: OnDesktop = OnDesktop::TheClipItself;
    /// And what a copy that never reached the desktop leaves behind.
    const NOT_THERE: OnDesktop = OnDesktop::Nothing;

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
            decide(Some(theirs.clone()), Some(&mine), &PUT_THERE),
            Paste::Theirs(theirs)
        );
    }

    /// **The bug the third reading exists for.** The desktop was holding
    /// something, Umber copied a region, and the write to the desktop failed —
    /// a Windows global allocation refusing a large picture is the realistic
    /// way. The desktop is therefore still holding the *older* picture, and
    /// believing it means a copy and the paste straight after it put down
    /// different pictures with nothing said. Umber's own copy is the most
    /// recent thing Umber knows happened, so it is what lands.
    #[test]
    fn a_copy_the_desktop_refused_is_still_what_a_paste_puts_down() {
        let stale = clip(4, 4, [10, 20, 30, 255]);
        let mine = clip(6, 2, [200, 100, 50, 255]);
        assert_eq!(
            decide(Some(stale), Some(&mine), &NOT_THERE),
            Paste::Mine(mine),
            "a copy the desktop would not take was overruled by what was there before it"
        );
    }

    /// **The exactness promise, end to end.** A region taken off a layer,
    /// handed to a desktop that gives it back unchanged, and pasted: the bytes
    /// that reach the layer have to be the bytes that came off it. That is
    /// `a_copy_and_a_paste_are_exact_inverses` in `umber-core` with the machine
    /// in the middle of it — which is exactly what this module put there, and
    /// the only reason the "it is already ours" branch exists at all.
    ///
    /// The transport is modelled as what arboard measurably does: `put_image`
    /// hands over `Clip::pixels` and `take_image` is `Clip::from_rgba` over
    /// what came back, so a lossless one returns the same bytes.
    #[test]
    fn a_copy_out_to_the_desktop_and_a_paste_back_move_no_pixel() {
        // Layer-texture form: premultiplied, so no component may exceed alpha.
        let layer: Vec<u8> = vec![
            200, 100, 50, 255, // opaque
            0, 0, 0, 0, // clear
            90, 40, 20, 128, // half covered
            33, 33, 33, 200,
        ];
        let rect = umber_core::PixelRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        };
        let mine = Clip::from_layer(rect, &layer, None).expect("a clip");
        let desktop = Clip::from_rgba(mine.size().x, mine.size().y, mine.pixels().to_vec())
            .expect("what a lossless desktop hands back");

        let Paste::Mine(chosen) = decide(Some(desktop), Some(&mine), &PUT_THERE) else {
            panic!("Umber's own copy was not recognised on the desktop's clipboard");
        };
        let placed = chosen
            .place(glam::UVec2::splat(64), glam::vec2(32.0, 32.0))
            .expect("somewhere to go");
        assert_eq!(
            placed.pixels, layer,
            "a copy out to the machine and a paste back moved a pixel"
        );
    }

    /// Copying a line of text somewhere else must not throw away the region an
    /// artist copied ten seconds ago. There is no text tool for a string to be
    /// pasted into, so "the desktop holds no picture" is the ordinary state of
    /// the machine rather than an error.
    #[test]
    fn text_on_the_desktop_leaves_umbers_own_picture_alone() {
        let mine = clip(2, 2, [1, 2, 3, 4]);
        assert_eq!(decide(None, Some(&mine), &PUT_THERE), Paste::Mine(mine));
    }

    /// Nothing anywhere. A paste has to do nothing at all rather than put down
    /// an empty rectangle and call it an edit — the same rule
    /// `copying_nothing_leaves_the_clipboard_alone` states for the other end.
    #[test]
    fn nothing_on_either_clipboard_pastes_nothing() {
        assert_eq!(decide(None, None, &NOT_THERE), Paste::Nothing);
    }

    /// A picture off the desktop with nothing on Umber's own clipboard — the
    /// first paste of a fresh session, which is most of why any of this exists.
    #[test]
    fn a_first_paste_of_the_session_takes_what_the_desktop_holds() {
        let theirs = clip(8, 2, [0, 255, 0, 255]);
        assert_eq!(
            decide(Some(theirs.clone()), None, &NOT_THERE),
            Paste::Theirs(theirs)
        );
    }

    /// Same size, different pixels, on a platform whose transport returns what
    /// it was given. Somebody copied something else, and the desktop is
    /// believed — a picture of the same shape is not the same picture, which is
    /// exactly why the echo below is a `Clip` and not a size.
    #[test]
    fn a_same_sized_picture_that_is_not_umbers_is_still_theirs() {
        let mine = clip(4, 4, [200, 100, 50, 255]);
        let theirs = clip(4, 4, [200, 100, 51, 255]);
        assert_eq!(
            decide(Some(theirs.clone()), Some(&mine), &PUT_THERE),
            Paste::Theirs(theirs)
        );
    }

    /// **The echo, and the reason it exists.** On a platform whose clipboard
    /// does not hand back the bytes it was given — macOS is suspected of
    /// exactly this, through `NSImage` and a TIFF representation that
    /// premultiplies — the desktop is holding something that is *not* Umber's
    /// clip and is nonetheless Umber's copy. Comparing against the clip would
    /// fail to recognise it, believe the desktop, and paste the transport's
    /// mangled bytes: a copy and a paste straight back that comes out darker at
    /// every soft edge, silently, on a whole supported platform.
    ///
    /// Comparing against the echo recognises it, and what lands is still the
    /// exact bytes off the layer. Driven end to end — through `Clip::from_layer`
    /// and `Clip::place` — because the promise is about pixels reaching a layer
    /// rather than about which enum variant came back.
    #[test]
    fn a_lossy_clipboard_still_pastes_the_bytes_that_came_off_the_layer() {
        // Layer-texture form: premultiplied, so no component may exceed alpha.
        let layer: Vec<u8> = vec![
            200, 100, 50, 255, // opaque
            0, 0, 0, 0, // clear
            90, 40, 20, 128, // a soft edge, which is where the loss would be
            33, 33, 33, 200,
        ];
        let rect = umber_core::PixelRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        };
        let mine = Clip::from_layer(rect, &layer, None).expect("a clip");

        // What a transport that moved a byte hands back for it. Nothing here
        // models Cocoa; all that matters is that it is not `mine`.
        let mut mangled = mine.pixels().to_vec();
        mangled[6] = mangled[6].wrapping_add(3);
        let echo = Clip::from_rgba(mine.size().x, mine.size().y, mangled).expect("an echo");
        assert_ne!(echo, mine, "the fixture is not modelling a lossy transport");

        let Paste::Mine(chosen) = decide(Some(echo.clone()), Some(&mine), &OnDesktop::Echo(echo))
        else {
            panic!("the copy was not recognised by its echo, so the transport's bytes would land");
        };
        let placed = chosen
            .place(glam::UVec2::splat(64), glam::vec2(32.0, 32.0))
            .expect("somewhere to go");
        assert_eq!(
            placed.pixels, layer,
            "a paste on a lossy platform did not restore the bytes the copy took"
        );
    }

    /// And the echo must not swallow a genuine foreign picture. A platform that
    /// needs one is still a platform somebody copies a photograph on.
    #[test]
    fn an_echo_does_not_make_another_applications_picture_lose() {
        let mine = clip(4, 4, [200, 100, 50, 255]);
        let echo = clip(4, 4, [200, 100, 50, 254]);
        let theirs = clip(4, 4, [10, 20, 30, 255]);
        assert_eq!(
            decide(Some(theirs.clone()), Some(&mine), &OnDesktop::Echo(echo)),
            Paste::Theirs(theirs)
        );
    }

    /// A picture adopted *off* the desktop is recorded as being there, on every
    /// platform and with no echo taken, because it was obtained by reading the
    /// desktop — so the bytes it hands back for it are the bytes in hand.
    ///
    /// **What that is worth is not the paste straight afterwards**, which lands
    /// the same picture whatever the state says, so asserting it would be a
    /// test that passes against the bug. It is the *next* copy somebody makes
    /// in another application: leaving the adoption unrecorded fires the
    /// refused-write clause where no write was refused, and Ctrl+V then puts
    /// down the picture from last time instead of the one just copied.
    #[test]
    fn adopting_a_picture_does_not_make_the_next_foreign_copy_lose() {
        let adopted = clip(3, 3, [10, 20, 30, 255]);
        let newer = clip(5, 1, [1, 2, 3, 255]);
        assert_eq!(
            decide(Some(newer.clone()), Some(&adopted), &PUT_THERE),
            Paste::Theirs(newer.clone()),
            "the picture adopted last time was preferred over the one just copied"
        );
        // And this is the failure `note_adopted` exists to prevent, stated so
        // the assertion above cannot be read as testing nothing.
        assert_eq!(
            decide(Some(newer), Some(&adopted), &NOT_THERE),
            Paste::Mine(adopted),
            "the refused-write clause has stopped preferring Umber's own copy"
        );
    }
}
