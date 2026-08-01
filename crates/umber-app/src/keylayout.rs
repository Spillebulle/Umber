//! What the keyboard in front of the user actually prints.
//!
//! Umber's bindings are physical positions: winit names a key by where a *US*
//! layout has that legend, whatever the keyboard says. `shortcuts::key_for_text`
//! already makes a punctuation press follow the legend rather than the position,
//! so Ctrl and the key marked `+` zooms in wherever a layout has put it. The
//! label did not follow — `shortcuts::us_key_name` spells `KeyCode::Equal` as
//! "=" on every machine — so a Norwegian keyboard was told to press a key it
//! does not have.
//!
//! Only the *label* moves. `Chord::id` is untouched and must stay so: the stored
//! form is ASCII-identifier-only and identical on every platform, which is what
//! lets a preferences file be copied between machines. There is precedent —
//! `Chord::display` already prints "Cmd" where the stored form says "Ctrl".
//!
//! ## Shape
//!
//! [`Legends`] is a *reading* — one snapshot of what each physical key prints —
//! and [`name_for`] is a pure function of it, in the style of
//! `update::install::detect`. That is what lets the Norwegian, German and
//! Cyrillic answers be tested on a machine with none of those keyboards, which
//! is the only way they are tested at all.
//!
//! ## Where a reading comes from
//!
//! Windows only. `MapVirtualKeyW` turns a scancode into the virtual key *this*
//! layout puts there and then into the character it prints, which is exactly the
//! question being asked; the scancode is winit's own, so the position table is
//! not written down a second time. macOS and Linux answer "cannot say" and fall
//! back to the US name — never to an empty label.
//!
//! ## Why it is a cache, and what ends its life
//!
//! A reading costs a pair of platform calls per key, and `Chord::display` runs
//! from tooltips while the interface is painting. So the reading is taken once
//! and kept. A layout can be switched while the app runs, and [`forget_if_changed`]
//! is what notices — called from the event loop when a key arrives or the window
//! takes focus, which is the *input* path deliberately. The cost of that choice
//! is that a layout switched with nothing pressed and no click on the window
//! leaves a label stale until the next of either; the cost of the alternative is
//! a platform call every frame a tooltip is up.

use crate::shortcuts;
use std::sync::RwLock;
use winit::keyboard::KeyCode;

/// The letter block. Its own list rather than a range, because `KeyCode` is not
/// ordered and the letters are the half that names itself from its position.
const LETTERS: [KeyCode; 26] = [
    KeyCode::KeyA,
    KeyCode::KeyB,
    KeyCode::KeyC,
    KeyCode::KeyD,
    KeyCode::KeyE,
    KeyCode::KeyF,
    KeyCode::KeyG,
    KeyCode::KeyH,
    KeyCode::KeyI,
    KeyCode::KeyJ,
    KeyCode::KeyK,
    KeyCode::KeyL,
    KeyCode::KeyM,
    KeyCode::KeyN,
    KeyCode::KeyO,
    KeyCode::KeyP,
    KeyCode::KeyQ,
    KeyCode::KeyR,
    KeyCode::KeyS,
    KeyCode::KeyT,
    KeyCode::KeyU,
    KeyCode::KeyV,
    KeyCode::KeyW,
    KeyCode::KeyX,
    KeyCode::KeyY,
    KeyCode::KeyZ,
];

/// A snapshot of what one keyboard layout prints, unshifted, on each key whose
/// label could move.
///
/// Sparse on purpose: a position the platform will not answer for is simply
/// absent, and [`name_for`] falls back for it rather than inventing a legend.
#[derive(Default)]
pub struct Legends {
    printed: Vec<(KeyCode, char)>,
}

impl Legends {
    /// Take a reading, one position at a time.
    ///
    /// The reader is injected rather than called directly, which is what makes
    /// every layout testable off the platform that could answer for it.
    pub fn read(ask: impl Fn(KeyCode) -> Option<char>) -> Legends {
        let positions = LETTERS
            .into_iter()
            .chain(shortcuts::PUNCTUATION.iter().map(|(key, _)| *key));
        Legends {
            printed: positions.filter_map(|key| Some((key, ask(key)?))).collect(),
        }
    }

    /// What this layout prints on one position.
    fn of(&self, key: KeyCode) -> Option<char> {
        self.printed
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, printed)| *printed)
    }

    /// Whether `wanted` is on a punctuation key somewhere — anywhere. Which
    /// position it landed on does not matter, because the dispatcher folds a
    /// punctuation press onto the legend rather than the position.
    fn punctuation_prints(&self, wanted: char) -> bool {
        shortcuts::PUNCTUATION
            .iter()
            .any(|(key, _)| self.of(*key) == Some(wanted))
    }
}

/// The name to print for a physical key on the keyboard this reading came from,
/// or `None` where the layout says nothing that should move the label.
///
/// The two halves answer to the two dispatch rules, which is why they differ.
pub fn name_for(key: KeyCode, legends: &Legends) -> Option<String> {
    if LETTERS.contains(&key) {
        // A letter dispatches on the *position* — B is a place the hand goes —
        // so the key the user presses is this one, and its keycap is what to
        // print. On QWERTZ that means Undo reads "Ctrl+Y", which is the truth:
        // the key marked Y is physically Z. See the commit that added the fold.
        //
        // Only an ASCII letter is taken. A Cyrillic or Greek layout prints one
        // Archivo has no glyph for, and those keycaps carry the Latin letter as
        // a second legend anyway — so the US name is both drawable and right.
        let printed = legends.of(key)?;
        return printed
            .is_ascii_alphabetic()
            .then(|| printed.to_ascii_uppercase().to_string());
    }

    // Punctuation dispatches on the *legend* instead, so the key to name is
    // whichever one prints one of this key's two characters, wherever the layout
    // has moved it to. Only those two can reach this binding at all, so the
    // whole question is which of them the keyboard prints without a modifier.
    //
    // The unshifted-on-US one is preferred, which makes a US keyboard the exact
    // identity: nothing moves for the people the old labels already suited.
    let (_, legends_of_key) = shortcuts::PUNCTUATION.iter().find(|(k, _)| *k == key)?;
    legends_of_key
        .iter()
        .find(|c| legends.punctuation_prints(**c))
        .map(|c| c.to_string())
}

/// The layout's name for a physical key, or `None` where it cannot say.
///
/// Reads the cached reading, taking one if there is none. See the module docs
/// for what drops it.
pub fn key_name(key: KeyCode) -> Option<String> {
    if let Some(reading) = read_held().as_ref() {
        return name_for(key, &reading.legends);
    }
    let fresh = Reading {
        layout: platform::current_layout(),
        legends: platform::read_legends(),
    };
    let mut held = write_held();
    name_for(key, &held.insert(fresh).legends)
}

/// Drop the reading if the keyboard layout has changed since it was taken.
///
/// Cheap — one question to the platform, and no reading unless the answer
/// differs. Called from the event loop; see the module docs for why it is there
/// and not on the drawing path.
pub fn forget_if_changed() {
    let now = platform::current_layout();
    if read_held().as_ref().is_some_and(|r| r.layout != now) {
        *write_held() = None;
    }
}

/// One reading and the layout it was taken from.
struct Reading {
    /// Opaque: compared, never interpreted.
    layout: usize,
    legends: Legends,
}

static HELD: RwLock<Option<Reading>> = RwLock::new(None);

// A poisoned lock means a previous panic happened while the reading was
// borrowed. It is a snapshot that is only ever replaced whole, so it cannot be
// left half-written; taking the inner value keeps the labels drawing rather than
// panicking the paint app a second time. Same reasoning as `shortcuts::LIVE`.
fn read_held() -> std::sync::RwLockReadGuard<'static, Option<Reading>> {
    HELD.read().unwrap_or_else(|e| e.into_inner())
}

fn write_held() -> std::sync::RwLockWriteGuard<'static, Option<Reading>> {
    HELD.write().unwrap_or_else(|e| e.into_inner())
}

#[cfg(windows)]
mod platform {
    use super::Legends;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyboardLayout, MAPVK_VK_TO_CHAR, MAPVK_VSC_TO_VK_EX, MapVirtualKeyW,
    };
    use winit::keyboard::KeyCode;
    use winit::platform::scancode::PhysicalKeyExtScancode;

    /// Which layout is in force, as a number to compare against.
    ///
    /// `GetKeyboardLayout(0)` is the calling thread's, which is the window's:
    /// Windows keeps a layout per thread and switches the foreground one.
    pub fn current_layout() -> usize {
        // SAFETY: no pointers and no memory involved. The handle is compared,
        // never dereferenced.
        (unsafe { GetKeyboardLayout(0) }) as usize
    }

    pub fn read_legends() -> Legends {
        Legends::read(printed_on)
    }

    /// The character this layout prints on a physical key, unshifted.
    ///
    /// Two steps, both against the layout in force now: the scancode becomes
    /// the virtual key this layout puts at that position, and the virtual key
    /// becomes the character it prints. The scancode comes from winit, so the
    /// `KeyCode`-to-position table is not written down a second time here.
    ///
    /// `MAPVK_VSC_TO_VK_EX` rather than `MAPVK_VSC_TO_VK`: the latter collapses
    /// the left and right modifiers onto one virtual key, and a reading that
    /// answered the same thing for two different positions would be wrong for
    /// both of them.
    fn printed_on(key: KeyCode) -> Option<char> {
        let scancode = key.to_scancode()?;
        // SAFETY: `MapVirtualKeyW` takes and returns plain integers.
        let virtual_key = unsafe { MapVirtualKeyW(scancode, MAPVK_VSC_TO_VK_EX) };
        if virtual_key == 0 {
            return None;
        }
        // SAFETY: as above.
        let printed = unsafe { MapVirtualKeyW(virtual_key, MAPVK_VK_TO_CHAR) };
        // The top bit marks a dead key — the `´` a Nordic layout puts where US
        // has `=`, which prints nothing on its own. Taking one as a legend would
        // put an accent in a label that no single keypress ever produces.
        if printed == 0 || printed & 0x8000_0000 != 0 {
            return None;
        }
        char::from_u32(printed & 0xFFFF)
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Legends;

    /// One layout, for ever. Nothing here can tell two apart, so nothing here
    /// may claim they changed.
    pub fn current_layout() -> usize {
        0
    }

    /// macOS has `UCKeyTranslate` and X11 and Wayland have xkb, and neither is
    /// reachable from winit's handle without taking a dependency on the platform
    /// crate underneath it. Until one of them is, a label on those platforms
    /// keeps the US name — which is what every label showed before this existed,
    /// so nothing there gets worse.
    pub fn read_legends() -> Legends {
        Legends::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts::{BINDABLE, us_key_name};

    /// A reading built from a table of `(position, what it prints unshifted)`.
    /// Anything absent from the table is a position the platform would not
    /// answer for.
    fn reading(rows: &[(KeyCode, char)]) -> Legends {
        Legends::read(|key| {
            rows.iter()
                .find(|(at, _)| *at == key)
                .map(|(_, printed)| *printed)
        })
    }

    /// The layout winit's key *names* are written for: every position prints
    /// exactly what that position is called. `us_key_name` answers with one
    /// character for the letters, digits and punctuation and with a word for
    /// everything else, which is precisely the set a legend exists for.
    fn american() -> Legends {
        Legends::read(|key| {
            let name = us_key_name(key);
            let mut chars = name.chars();
            let first = chars.next()?;
            chars.next().is_none().then_some(first)
        })
    }

    /// The Norwegian layout. The letter block is US's, so only the keys that
    /// actually move are written out:
    ///
    /// ```text
    /// Norwegian:  | 1 2 3 4 5 6 7 8 9 0 + \
    /// US:         ` 1 2 3 4 5 6 7 8 9 0 - =
    /// ```
    ///
    /// `[`, `]`, `{`, `}` and `` ` `` are AltGr'd on this keyboard and so print
    /// nothing unshifted at any position.
    fn norwegian() -> Legends {
        let mut rows: Vec<(KeyCode, char)> = LETTERS
            .into_iter()
            .map(|key| (key, us_key_name(key).chars().next().expect("a letter")))
            .collect();
        rows.extend([
            (KeyCode::Backquote, '|'),
            (KeyCode::Minus, '+'),
            (KeyCode::Equal, '\\'),
            (KeyCode::BracketLeft, 'å'),
            (KeyCode::BracketRight, '¨'),
            (KeyCode::Semicolon, 'ø'),
            (KeyCode::Quote, 'æ'),
            (KeyCode::Backslash, '\''),
            (KeyCode::Comma, ','),
            (KeyCode::Period, '.'),
            (KeyCode::Slash, '-'),
        ]);
        reading(&rows)
    }

    #[test]
    fn a_us_keyboard_moves_no_label_at_all() {
        // The whole design rests on this: the people the old labels suited must
        // see exactly what they saw before.
        let us = american();
        for key in BINDABLE.iter().copied() {
            if let Some(name) = name_for(key, &us) {
                assert_eq!(name, us_key_name(key), "{key:?} moved on a US keyboard");
            }
        }
    }

    #[test]
    fn a_nordic_label_names_the_key_the_keyboard_has() {
        let no = norwegian();
        // The report this was built for: Zoom in read "Ctrl+=" on a keyboard
        // with no `=` on it. The key that zooms in is the one marked `+`.
        assert_eq!(name_for(KeyCode::Equal, &no).as_deref(), Some("+"));
        // Zoom out is still `-`, but it is a different key from the one US
        // calls Minus — it is where US has `/`. Same legend, so same label.
        assert_eq!(name_for(KeyCode::Minus, &no).as_deref(), Some("-"));
        // `\` moved to where US has `=` and keeps its name.
        assert_eq!(name_for(KeyCode::Backslash, &no).as_deref(), Some("\\"));
        assert_eq!(name_for(KeyCode::Quote, &no).as_deref(), Some("'"));
        // The letter block is untouched, so the tool shortcuts read as before.
        assert_eq!(name_for(KeyCode::KeyB, &no).as_deref(), Some("B"));
        assert_eq!(name_for(KeyCode::KeyZ, &no).as_deref(), Some("Z"));
    }

    #[test]
    fn a_legend_the_layout_does_not_print_falls_back() {
        let no = norwegian();
        // Brush size is `[` and `]`, which a Nordic keyboard puts on AltGr and
        // therefore prints nowhere unshifted. There is no honest layout answer,
        // so there is no answer: the label keeps the US name it always had.
        assert_eq!(name_for(KeyCode::BracketLeft, &no), None);
        assert_eq!(name_for(KeyCode::BracketRight, &no), None);
        // `;` is Shift+`,` there, and `:` is Shift+`.` — neither is unshifted.
        assert_eq!(name_for(KeyCode::Semicolon, &no), None);
        // Backquote's own position prints `|`, which belongs to Backslash.
        assert_eq!(name_for(KeyCode::Backquote, &no), None);
    }

    #[test]
    fn a_digit_and_a_named_key_are_never_renamed() {
        // Deliberate. Dispatch on the digit row is positional, and every Latin
        // layout marks that key with its digit even where the digit is shifted
        // — AZERTY prints `à` on the key it also calls 0. Naming it `à` would
        // be less recognisable, not more honest. Named keys have no legend at
        // all, and a localised one would sit oddly among English labels.
        for reading in [american(), norwegian()] {
            assert_eq!(name_for(KeyCode::Digit0, &reading), None);
            assert_eq!(name_for(KeyCode::Digit1, &reading), None);
            assert_eq!(name_for(KeyCode::F1, &reading), None);
            assert_eq!(name_for(KeyCode::Enter, &reading), None);
            assert_eq!(name_for(KeyCode::ArrowLeft, &reading), None);
            assert_eq!(name_for(KeyCode::Space, &reading), None);
        }
    }

    #[test]
    fn a_qwertz_letter_names_the_key_it_really_is() {
        // German swaps Y and Z. Umber dispatches letters on the position, so
        // Ctrl and the key marked Y is Undo there — which the label used to
        // deny by printing "Ctrl+Z". It now says what actually happens; the
        // fact that it reads oddly is the wart the fold's commit recorded, made
        // visible rather than hidden.
        let mut rows: Vec<(KeyCode, char)> = LETTERS
            .into_iter()
            .map(|key| (key, us_key_name(key).chars().next().expect("a letter")))
            .collect();
        for (position, printed) in [(KeyCode::KeyZ, 'y'), (KeyCode::KeyY, 'z')] {
            let row = rows.iter_mut().find(|(at, _)| *at == position);
            row.expect("in the letter block").1 = printed;
        }
        let de = reading(&rows);
        assert_eq!(name_for(KeyCode::KeyZ, &de).as_deref(), Some("Y"));
        assert_eq!(name_for(KeyCode::KeyY, &de).as_deref(), Some("Z"));
        // Lower case in, upper case out: a keycap is upper case and so is every
        // other shortcut label.
        assert_eq!(name_for(KeyCode::KeyB, &de).as_deref(), Some("B"));
    }

    #[test]
    fn a_non_latin_legend_is_refused() {
        // Archivo carries no Cyrillic, so a label built from one would be a row
        // of empty boxes — and the keycap has the Latin letter on it anyway.
        let ru = reading(&[(KeyCode::KeyZ, 'я'), (KeyCode::KeyB, 'и')]);
        assert_eq!(name_for(KeyCode::KeyZ, &ru), None);
        assert_eq!(name_for(KeyCode::KeyB, &ru), None);
    }

    #[test]
    fn a_layout_nobody_can_read_leaves_every_label_alone() {
        // macOS and Linux, and any Windows key the platform will not answer
        // for. Never an empty label — always no answer, so the US name stands.
        let silent = Legends::default();
        for key in BINDABLE.iter().copied() {
            assert_eq!(name_for(key, &silent), None, "{key:?}");
        }
    }

    #[test]
    fn a_name_from_the_live_platform_is_never_empty() {
        // Whatever this machine's keyboard is, an answer has to be printable —
        // a blank label would be worse than the wrong key.
        for key in BINDABLE.iter().copied() {
            if let Some(name) = key_name(key) {
                assert!(!name.is_empty(), "{key:?} named itself nothing");
                assert!(
                    name.chars().all(|c| !c.is_control()),
                    "{key:?} named itself {name:?}"
                );
            }
        }
        // Asking twice must agree: the second call reads the cache the first
        // one filled.
        assert_eq!(key_name(KeyCode::KeyB), key_name(KeyCode::KeyB));
        forget_if_changed();
        assert_eq!(key_name(KeyCode::KeyB), key_name(KeyCode::KeyB));
    }
}
