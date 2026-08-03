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
//! holds, what Umber holds, and whether Umber's own copy is known to have
//! reached the desktop — for the reason `install::detect` is a pure function of
//! a `Probe`: it is the whole of the rule, and no test may touch the real
//! clipboard. A CI runner may have no display server at all, and a test that
//! grabs the desktop's clipboard on somebody's machine is hostile.
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
//! * *And where the desktop holds Umber's own bytes.* This is what keeps
//!   `a_copy_and_a_paste_are_exact_inverses` — the sibling of
//!   `saving_and_reopening_does_not_move_a_pixel` — true through a copy that
//!   also went to the desktop. It is a **check rather than a trust**: arboard
//!   round trips RGBA8 through PNG on Windows and on both Linux backends, so
//!   the bytes come back identical and the comparison is satisfied. Measured on
//!   Windows rather than only read off arboard's source: a 16×16 square
//!   carrying every alpha from 0 to 255 came back byte for byte, which also
//!   rules out the DIB path having been taken in preference to the PNG one.
//!   A picture that merely resembles the one Umber copied is not evidence that
//!   it is that picture, and pasting the wrong picture is the one failure here
//!   worth avoiding at any cost — so where the bytes differ, the desktop is
//!   believed and the divergence is logged at `warn`, loudly enough to be seen
//!   without setting `RUST_LOG`.
//!
//! **macOS is the platform this has not been run on, and it is named rather
//! than assumed sound.** arboard writes an `NSImage` built from a `CGImage`
//! with straight alpha and reads back that image's TIFF representation, and
//! Cocoa's bitmap representations conventionally carry *premultiplied* alpha
//! while `image`'s TIFF decoder does not undo it. If that is what happens, the
//! comparison above fails on every copy of anything with a soft edge, the
//! desktop is believed, and a paste straight back comes out darker at that
//! edge. Nobody working on Umber has a Mac; the `warn` is what would report it,
//! and the fix if it is ever confirmed is **not** a size heuristic — a picture
//! of the same shape is not the same picture — but an *echo*: read the desktop
//! back once immediately after a successful write and compare against that
//! instead of against the clip. It is correct on a lossy transport and has no
//! false positive, and it is not done today because it costs a second decode on
//! every copy, which on the canvases the paragraph below is about is seconds.
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
//! already paid for. That is a freeze an artist would feel, and it is stated
//! here rather than hidden. It is not gated on a size, because any threshold
//! would be a number nobody measured and the effect of crossing it would be a
//! copy that silently did not leave Umber.
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
/// holding, already read; `mine` is `Editor::clipboard`; `published` is
/// [`Board::published`] — whether the clip in `mine` is known to have reached
/// the desktop.
///
/// The answer is a pure function of the three readings. The one log line is
/// observation and nothing reads it — it is the only way anybody would ever
/// find out that a platform's clipboard had started moving a byte.
pub fn decide(system: Option<Clip>, mine: Option<&Clip>, published: bool) -> Paste {
    match (system, mine) {
        // The desktop is still holding what Umber's own copy put there, so
        // **Umber's own bytes** are what go back on to the layer, not the ones
        // that came back through the transport. Indistinguishable today — the
        // guard is equality — and written this way round deliberately: the rule
        // is "our copy wins", and it should not quietly become "the transport's
        // copy wins" if `Clip`'s equality is ever loosened.
        (Some(theirs), Some(mine)) if theirs == *mine => Paste::Mine(mine.clone()),
        // Umber's copy never reached the desktop, so what is sitting there
        // predates it as far as Umber can tell — and believing it would put
        // down a picture the artist did not copy. See the module docs: leaving
        // this case out was a bug and a silent one.
        (Some(_), Some(mine)) if !published => Paste::Mine(mine.clone()),
        (Some(theirs), Some(mine)) => {
            // Umber's copy did reach the desktop and something different is
            // there now, so the desktop moved on after Umber wrote it — the
            // ordinary case — or a platform's clipboard gave back something
            // other than what it was handed. The two are indistinguishable from
            // here and only one of the two possible mistakes is survivable, so
            // the desktop is believed and the divergence is said out loud.
            if theirs.size() == mine.size() {
                log::warn!(
                    "the desktop is holding a {} × {} picture that is not the one Umber put \
                     there; pasting the desktop's. If this happens on every copy, this \
                     platform's clipboard is not returning what it was given",
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
    /// Whether the clip the editor is holding is known to be on the desktop.
    ///
    /// Written by the two things that can make it true and by nothing else, so
    /// it cannot drift out of step with `Editor::clipboard` the way a flag
    /// beside that field at three call sites would. Read by [`Board::published`]
    /// and fed to [`decide`] — see the module docs for the bug its absence was.
    published: bool,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Board {
    /// Offer `clip` to the rest of the machine, and answer whether it got
    /// there.
    ///
    /// The answer is not decoration. Umber's own clipboard is written by the
    /// caller either way, so a refusal costs the artist nothing they can *see* —
    /// but it leaves the desktop holding an older picture, and [`decide`] would
    /// then believe that picture over the one just copied. So the outcome is
    /// remembered rather than only logged. Logged as well, at the level a failed
    /// autosave is: a paint application must not raise a dialog on Ctrl+C.
    pub fn put_image(&mut self, clip: &Clip) {
        let size = clip.size();
        self.published = false;
        let Some(board) = self.board() else { return };
        let image = arboard::ImageData {
            width: size.x as usize,
            height: size.y as usize,
            // Straight-alpha sRGB RGBA8 on both sides. See the module docs.
            bytes: std::borrow::Cow::Borrowed(clip.pixels()),
        };
        match board.set_image(image) {
            Ok(()) => self.published = true,
            Err(e) => log::warn!(
                "the desktop's clipboard would not take the picture, so it stays inside \
                 Umber: {e}"
            ),
        }
    }

    /// Note that the editor's clip came *off* the desktop, so it is there by
    /// construction.
    ///
    /// Called where a foreign picture is adopted, and only once that paste has
    /// actually happened — the same place `Editor::clipboard` is written, so
    /// the two cannot disagree.
    pub fn note_adopted(&mut self) {
        self.published = true;
    }

    /// Whether the clip the editor holds is known to have reached the desktop.
    pub fn published(&self) -> bool {
        self.published
    }

    /// What the desktop is holding, if it is holding a picture.
    ///
    /// `None` covers every ordinary case as well as every failure — text on the
    /// clipboard, nothing on it, a format arboard cannot read — because the
    /// caller does the same thing with all of them: falls back to Umber's own
    /// clip. Only a failure that is not "there is no picture" is logged.
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

/// Where arboard is not built there is no desktop clipboard to reach, and
/// `published` is therefore always false — which is exactly right: [`decide`]
/// then never prefers a picture Umber did not put there, and since
/// [`Board::take_image`] answers `None` as well, copy and paste inside Umber
/// work exactly as they did before any of this existed.
#[cfg(any(target_os = "android", target_os = "ios"))]
#[derive(Default)]
pub struct Board;

#[cfg(any(target_os = "android", target_os = "ios"))]
impl Board {
    pub fn put_image(&mut self, _clip: &Clip) {}

    pub fn note_adopted(&mut self) {}

    pub fn published(&self) -> bool {
        false
    }

    pub fn take_image(&mut self) -> Option<Clip> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Named so the third argument of every `decide` below says what it means
    /// at the call site: a bare `true` there is the reading nobody can check.
    const PUBLISHED: bool = true;

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
            decide(Some(theirs.clone()), Some(&mine), PUBLISHED),
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
            decide(Some(stale), Some(&mine), !PUBLISHED),
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

        let Paste::Mine(chosen) = decide(Some(desktop), Some(&mine), PUBLISHED) else {
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
        assert_eq!(decide(None, Some(&mine), PUBLISHED), Paste::Mine(mine));
    }

    /// Nothing anywhere. A paste has to do nothing at all rather than put down
    /// an empty rectangle and call it an edit — the same rule
    /// `copying_nothing_leaves_the_clipboard_alone` states for the other end.
    #[test]
    fn nothing_on_either_clipboard_pastes_nothing() {
        assert_eq!(decide(None, None, !PUBLISHED), Paste::Nothing);
    }

    /// A picture off the desktop with nothing on Umber's own clipboard — the
    /// first paste of a fresh session, which is most of why any of this exists.
    #[test]
    fn a_first_paste_of_the_session_takes_what_the_desktop_holds() {
        let theirs = clip(8, 2, [0, 255, 0, 255]);
        assert_eq!(
            decide(Some(theirs.clone()), None, !PUBLISHED),
            Paste::Theirs(theirs)
        );
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
            decide(Some(theirs.clone()), Some(&mine), PUBLISHED),
            Paste::Theirs(theirs)
        );
    }
}
