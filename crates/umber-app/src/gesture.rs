//! What a press on the canvas begins — one decision, for every kind of pointer.
//!
//! **This exists because a pen is not a mouse to winit.** On Windows a pen
//! arrives as `WindowEvent::Touch`, through `WM_POINTER`; winit consumes those
//! messages, so Windows never promotes them to legacy mouse ones and a pen
//! produces no `CursorMoved` and no `MouseInput` at all. Anything decided in the
//! mouse arm of `window_event` is therefore invisible to a tablet — which is how
//! the Alt-drag brush resize, the Pan tool and the Zoom tool all came to work
//! under a mouse and do nothing under a pen, while a press that should have
//! resized the brush fell through to painting instead.
//!
//! The fix that lasts is not a second copy of the decision in the touch arm. It
//! is this: the two event families each say what they *observed* — which tool is
//! in hand, which modifiers are down, whether the interface owns the position,
//! whether the press is a button or a contact — and this module says what that
//! means. `app.rs` then acts on the answer once. A gesture added here reaches
//! both pointers or neither.
//!
//! It is a pure function of a [`Pointer`], with no winit and no egui in it, for
//! the reason `install::detect` takes an injected `Probe` and `keylayout::
//! name_for` an injected reading: nobody working on Umber has a tablet, so the
//! only evidence that a pen press resolves the way a mouse press does is a test
//! that can state both without a window.

use crate::editor::Tool;
use winit::event::TouchPhase;

/// How far a contact may travel and still be read as a tap rather than a drag,
/// in physical window pixels.
///
/// Same size and the same reasoning as `app::PUT_DOWN_SLOP`, which settles the
/// other click-or-drag question in Umber: it is the hand's wobble as the nib
/// touches and leaves the glass, not a movement anybody meant.
pub const TAP_SLOP: f32 = 6.0;

/// Whether a contact that travelled `distance` physical pixels was a tap.
pub fn is_tap(distance: f32) -> bool {
    distance <= TAP_SLOP
}

/// Everything the decision below is allowed to read.
///
/// Deliberately a snapshot of observations rather than a borrow of the editor:
/// it is what makes the whole matrix — six tools times two pointers times the
/// modifiers — statable in a test.
#[derive(Clone, Copy, Debug)]
pub struct Pointer {
    pub tool: Tool,
    /// The interface, rather than the document, owns the position pressed.
    /// Computed by `app::ui_owns_pointer` from the *event's* position, never
    /// from `Editor::cursor` — see there.
    pub ui_owns: bool,
    /// Alt is held. A keyboard modifier reaches every pointer alike, which is
    /// exactly why it has to be consulted on the touch path too; it was not.
    pub alt: bool,
    /// Space is held — the temporary pan modifier.
    pub space: bool,
    /// The middle button, which only a mouse has.
    pub pan_button: bool,
    /// This press is a contact on the glass rather than a button going down: a
    /// pen or a finger.
    pub contact: bool,
    /// A brush-size drag is armed, because Alt went down over the canvas with
    /// nothing else happening.
    pub resizing: bool,
}

/// What a press turned out to mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// The interface has it; the canvas sees nothing.
    Ignored,
    /// Pan the view, whatever tool is in hand.
    Pan,
    /// Zoom about where the press landed.
    Zoom,
    Paint,
    Select,
    Transform,
    /// Take the colour under the pointer.
    Eyedropper,
    /// Carry on the brush-size drag under this contact.
    ResizeBrush,
}

impl Press {
    /// Every variant.
    ///
    /// Anything reasoning over the whole enum iterates this rather than a list
    /// written out where it is used, which is exactly what a variant added
    /// later does not appear in — a test walking a hand-written array is a
    /// test that silently stops covering the thing it names.
    pub const ALL: [Press; 8] = [
        Press::Ignored,
        Press::Pan,
        Press::Zoom,
        Press::Paint,
        Press::Select,
        Press::Transform,
        Press::Eyedropper,
        Press::ResizeBrush,
    ];

    /// A short name for Settings → Input & pen, which records which gesture a
    /// press was resolved to.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ignored => "interface",
            Self::Pan => "pan",
            Self::Zoom => "zoom",
            Self::Paint => "paint",
            Self::Select => "select",
            Self::Transform => "transform",
            Self::Eyedropper => "eyedropper",
            Self::ResizeBrush => "brush size",
        }
    }
}

/// Decide what a press begins.
///
/// The order of the tests is the whole of the rule and each one is here for a
/// reason:
///
/// 1. **The pan overrides come first, before the interface is even consulted.**
///    A middle-drag or a space-drag pans whatever it started over — muscle
///    memory should not depend on where the panels happen to be, and that is
///    what the mouse path has always done.
/// 2. **Then the interface.** A press on a panel, a menu or a scrollbar is not
///    the canvas's.
/// 3. **Then the brush-size drag, but only for a contact.** This is the one
///    place a pen genuinely cannot copy the mouse. The mouse's rule is "Alt
///    with a button is the eyedropper and Alt without one is the resize" — and
///    a pen has no button-less drag *on the glass* to spell the second with, so
///    reading Alt-plus-contact as the eyedropper would leave a tablet with no
///    way to resize a brush at all. It goes the other way instead, the way
///    Krita and Photoshop spell it on a tablet: Alt with the nib down and
///    moving is the resize, and Alt with the nib down and *still* is the
///    eyedropper, settled at the release by [`is_tap`]. A mouse press is never
///    a contact, so nothing about the mouse changes.
/// 4. **Then Alt**, which is the eyedropper in every paint application.
/// 5. **Then the tool.**
pub fn press(p: Pointer) -> Press {
    if p.pan_button || p.space {
        return Press::Pan;
    }
    if p.ui_owns {
        return Press::Ignored;
    }
    if p.contact && p.resizing {
        return Press::ResizeBrush;
    }
    if p.alt {
        return Press::Eyedropper;
    }
    match p.tool {
        Tool::Brush | Tool::Eraser => Press::Paint,
        Tool::Select => Press::Select,
        Tool::Transform => Press::Transform,
        // The same answer Alt gives above, deliberately — one gesture, reached
        // two ways. A second `Press` variant for the tool would be a second
        // thing `app.rs` had to route to `pick_colour`, which is exactly the
        // duplicate path the composite pass's single reader exists to prevent.
        Tool::Eyedropper => Press::Eyedropper,
        Tool::Pan => Press::Pan,
        Tool::Zoom => Press::Zoom,
    }
}

/// Whether a press that resolved to `decision` must end a stroke already in
/// flight.
///
/// **A second button going down mid-stroke used to strand the stroke in a state
/// nothing ended.** Hold the left button and draw, then press the middle one:
/// [`press`] answers [`Press::Pan`] and `pointer_pressed` sets
/// `Interaction::Panning` over the top of `Interaction::Drawing`. The left
/// button coming up then falls through `pointer_released`'s `_` arm — it
/// dispatches on the interaction, which is no longer `Drawing` — so
/// `finish_stroke` is never called at all. Three things follow, and the last is
/// the worst:
///
/// * The dabs already stamped stay in the scratch texture and go on being
///   composited, so a half-stroke hangs on the canvas that is in no layer. Save
///   or export in that state and the file disagrees with the screen.
/// * `render`'s `quiet` test requires `!stroke.is_active()`, and the builder
///   still is, so **the autosave stops for the rest of the session** until some
///   later stroke happens to begin and end properly.
/// * The next `start_stroke` clears the scratch, so the hanging mark silently
///   vanishes rather than being baked in — which is the one mercy here, and is
///   why this is a lost stroke rather than a wrong-coloured one.
///
/// The answer is **finish, not cancel**, and the asymmetry with
/// [`Contact::Pinch`] is deliberate rather than an oversight: a second finger
/// means the first contact was never a stroke, so those dabs must never reach
/// the canvas, where a second *button* arrives after the artist has drawn a
/// visible mark with the first. Every other "something else is happening now"
/// path in `app.rs` commits too — `switch_document`, `apply_canvas`,
/// `close_document`, `float_a_clip` and `take_region` all call `finish_stroke`.
/// Do not unify the two.
///
/// **[`Press::Paint`] supersedes too, and the reason is a second lost stroke.**
/// It looks like the one press that should be excluded — it *is* a stroke, so
/// ending one to begin another reads like churn — and that reasoning was wrong,
/// because it assumed a `Paint` press cannot arrive while a stroke runs. It
/// can. `Editor::touches` is written only by the touch arm, so it is empty
/// while a **mouse** stroke is live, and a pen coming down then answers
/// [`Contact::Press`] rather than [`Contact::Pinch`] and resolves to `Paint`.
/// What followed was `start_stroke` on an already-active builder, whose
/// unconditional `clear_stroke` **discarded the mouse stroke with no history
/// entry** — silently, and not undoable. Finishing it instead commits it with
/// its entry and *then* begins the new one, which is strictly better than
/// losing it.
///
/// The ordinary one-pointer case is untouched, and what guarantees that is the
/// call site's `&& self.editor.stroke.is_active()`: a press arrives once per
/// gesture and no stroke is live when it does. This does not double-finish
/// anything.
///
/// [`Press::Ignored`] is the one exclusion left, and it is excluded for what it
/// *is* rather than for being unreachable — it is reachable by the same
/// crossing. A press the canvas never sees must not end a stroke, and acting on
/// one was a real bug: see the [`Contact::Pinch`] arm in `app.rs`, "one finger
/// on a panel is not a gesture, and cancelling the stroke there threw away a
/// stroke the other hand was in the middle of". That is exactly this case.
///
/// The honest fix for the whole class is still owed: `pointer_released` should
/// end a stroke on `stroke.is_active()` rather than on `Interaction`, which is
/// where the authority actually lives. With that in place no future `Press`
/// variant and no new writer of `Interaction` could strand a stroke at all.
///
/// The `match` is exhaustive and has **no wildcard arm**, which is the point of
/// it: a `_ => true` would silently answer for a variant nobody had thought
/// about, which is what a negative `matches!` did here and is the reason this
/// was rewritten. [`Press::ALL`] is the other half — the compiler catches a new
/// variant not answering, and the test walking `ALL` catches one that answers
/// but was never exercised.
pub fn supersedes_stroke(decision: Press) -> bool {
    match decision {
        // A press the canvas never sees. The one exclusion.
        Press::Ignored => false,
        // Paint included: a pen landing during a mouse stroke is a `Paint`
        // press with a stroke running, and discarding it is a lost mark.
        Press::Paint
        | Press::Pan
        | Press::Zoom
        | Press::Select
        | Press::Transform
        | Press::Eyedropper
        | Press::ResizeBrush => true,
    }
}

/// What a `WindowEvent::Touch` turns out to be.
///
/// The routing half of this module: `press` says what a press *means*, and this
/// says which touch events are presses at all. Both of the rules below were
/// bugs, and both are the sort that cannot be reproduced without hardware —
/// which is exactly why they are a function rather than a chain of `if`s inside
/// the event arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contact {
    /// A first contact going down: a press, to be put through [`press`].
    Press,
    /// A second contact: the gesture is a pinch, and whatever the first one was
    /// making is abandoned.
    Pinch,
    /// An update for an id that never went down — a pen in range and off the
    /// glass. Not a contact, and must never be recorded as one.
    Hover,
    /// The contact that owns the gesture, moving.
    Drag,
    /// Some other contact moving: a pinch in progress.
    Pinching,
    /// The owning contact leaving the glass.
    Release,
    /// Some other contact leaving.
    Lift,
}

/// Decide what a touch event is.
///
/// * `down` is how many contacts are on the glass *including* this one, for a
///   `Started`.
/// * `known` is whether this id has already been seen going down.
/// * `owner` is whether this id is the one carrying the current gesture.
///
/// The two rules worth stating:
///
/// **A `Moved` for an id that never `Started` is a hover, not a contact.**
/// Windows reports a pen in range as a pointer update with no down flag, and a
/// pen carried out of range sends no "up" — so recording it as a contact leaves
/// an entry that never goes away, and the next real press (Windows issues a
/// fresh pointer id per contact session) counts as a second finger and is taken
/// for a pinch. A finger always starts before it moves, so nothing is lost by
/// requiring it.
///
/// **A second contact is a pinch and supersedes everything.** A hand landing on
/// the glass must not half-finish a stroke, a selection or a transform drag.
pub fn contact(phase: TouchPhase, down: usize, known: bool, owner: bool) -> Contact {
    match phase {
        TouchPhase::Started if down > 1 => Contact::Pinch,
        TouchPhase::Started => Contact::Press,
        TouchPhase::Moved if !known => Contact::Hover,
        TouchPhase::Moved if owner => Contact::Drag,
        TouchPhase::Moved => Contact::Pinching,
        TouchPhase::Ended | TouchPhase::Cancelled if owner => Contact::Release,
        TouchPhase::Ended | TouchPhase::Cancelled => Contact::Lift,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mouse's left button, with nothing held.
    fn mouse(tool: Tool) -> Pointer {
        Pointer {
            tool,
            ui_owns: false,
            alt: false,
            space: false,
            pan_button: false,
            contact: false,
            resizing: false,
        }
    }

    /// The same press by a pen or a finger: a contact, no button.
    fn pen(tool: Tool) -> Pointer {
        Pointer {
            contact: true,
            ..mouse(tool)
        }
    }

    /// Every tool.
    ///
    /// Written out here rather than iterated off a list in `editor.rs`, and
    /// therefore checked by `every_tool_is_in_this_modules_own_list` below —
    /// which is the shape CLAUDE.md's rule for an `ALL` array prescribes: a
    /// hand-written array is exactly the thing a new variant does not appear
    /// in, so something has to fail the build when one is added.
    const TOOLS: [Tool; 7] = [
        Tool::Brush,
        Tool::Eraser,
        Tool::Select,
        Tool::Transform,
        Tool::Eyedropper,
        Tool::Pan,
        Tool::Zoom,
    ];

    #[test]
    fn every_tool_is_in_this_modules_own_list() {
        // The exhaustive `match` is what the *compiler* catches: a tool added
        // to the enum cannot get past this without an arm. The arm indexing
        // `TOOLS` is what catches the array being left short, which the
        // compiler cannot see — a fixed-size array of the right length
        // compiles whatever is in it. Neither half covers the other, and the
        // known hole is an arm that indexes somebody else's position; that is
        // what the equality is for.
        for (i, tool) in TOOLS.iter().enumerate() {
            let expected = match tool {
                Tool::Brush => 0,
                Tool::Eraser => 1,
                Tool::Select => 2,
                Tool::Transform => 3,
                Tool::Eyedropper => 4,
                Tool::Pan => 5,
                Tool::Zoom => 6,
            };
            assert_eq!(i, expected, "{tool:?} is filed in the wrong place");
        }
    }

    #[test]
    fn the_eyedropper_tool_and_alt_are_one_gesture() {
        // Two ways in, one answer — which is what keeps `pick_colour` the
        // single route from a pixel to a colour. If these ever differ, `app.rs`
        // has grown a second path and the canvas read and the desktop read can
        // start disagreeing about what a pick is.
        assert_eq!(press(mouse(Tool::Eyedropper)), Press::Eyedropper);
        assert_eq!(press(pen(Tool::Eyedropper)), Press::Eyedropper);
        assert_eq!(
            press(Pointer {
                alt: true,
                ..mouse(Tool::Brush)
            }),
            press(mouse(Tool::Eyedropper)),
        );
    }

    #[test]
    fn the_eyedropper_tool_still_gives_way_to_the_pan_overrides_and_the_interface() {
        // A tool that swallowed Space or a middle-drag would be the one place
        // in the rail where navigation stopped working, and one that picked a
        // colour out of a panel would be reading the theme's own ink.
        assert_eq!(
            press(Pointer {
                space: true,
                ..mouse(Tool::Eyedropper)
            }),
            Press::Pan
        );
        assert_eq!(
            press(Pointer {
                pan_button: true,
                ..mouse(Tool::Eyedropper)
            }),
            Press::Pan
        );
        assert_eq!(
            press(Pointer {
                ui_owns: true,
                ..mouse(Tool::Eyedropper)
            }),
            Press::Ignored
        );
    }

    #[test]
    fn the_eyedropper_tool_does_not_take_the_brush_resize_off_a_tablet() {
        // Alt with the nib down and moving is the resize, whatever tool is in
        // hand — which has to keep being true for this one, or a pen user who
        // reached for the eyedropper would find Alt-drag had stopped resizing
        // the brush there and only there.
        assert_eq!(
            press(Pointer {
                alt: true,
                resizing: true,
                ..pen(Tool::Eyedropper)
            }),
            Press::ResizeBrush
        );
    }

    #[test]
    fn a_pen_press_resolves_to_what_a_mouse_press_would() {
        // The whole point of the module. Every tool, and every modifier state
        // that is not the one place the two genuinely differ (Alt with the
        // resize armed, below).
        for tool in TOOLS {
            for alt in [false, true] {
                for space in [false, true] {
                    let m = Pointer {
                        alt,
                        space,
                        ..mouse(tool)
                    };
                    let p = Pointer {
                        alt,
                        space,
                        ..pen(tool)
                    };
                    assert_eq!(
                        press(m),
                        press(p),
                        "a pen and a mouse must agree for {tool:?} alt={alt} space={space}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_navigation_tools_are_reachable_by_pen() {
        // The reported bug: "Same with the move tool and zoom tool. Nothing
        // happens with pen." The touch arm used to answer neither, because
        // panning and zooming by touch were the two-finger gesture and one
        // finger navigated nothing — which is right for a hand that has landed
        // on the glass with a brush in hand, and wrong for somebody who went
        // to the rail and *chose* the Pan tool.
        assert_eq!(press(pen(Tool::Pan)), Press::Pan);
        assert_eq!(press(pen(Tool::Zoom)), Press::Zoom);
    }

    #[test]
    fn alt_with_a_contact_carries_the_brush_resize_on() {
        // The other half of the report: "Holding alt makes the circle come up,
        // but dragging with a pen just draws." The circle is armed by
        // `ModifiersChanged`, which a pen never interfered with; what was
        // missing is that the contact then had to *continue* the gesture
        // instead of starting a stroke.
        let armed = Pointer {
            alt: true,
            resizing: true,
            ..pen(Tool::Brush)
        };
        assert_eq!(press(armed), Press::ResizeBrush);

        // And a mouse press in the same state is still the eyedropper, because
        // a button is not a contact. This is the one asymmetry in the module
        // and it must not leak the other way.
        let same_by_mouse = Pointer {
            contact: false,
            ..armed
        };
        assert_eq!(press(same_by_mouse), Press::Eyedropper);
    }

    #[test]
    fn alt_without_the_resize_armed_is_the_eyedropper_for_both() {
        // Alt is a keyboard modifier and reaches a pen user exactly as it
        // reaches a mouse user. The touch arm used to consult it not at all, so
        // Alt and a pen painted.
        for tool in TOOLS {
            let m = Pointer {
                alt: true,
                ..mouse(tool)
            };
            let p = Pointer {
                alt: true,
                ..pen(tool)
            };
            assert_eq!(press(m), Press::Eyedropper);
            assert_eq!(press(p), Press::Eyedropper);
        }
    }

    #[test]
    fn space_pans_under_a_pen_too() {
        for tool in TOOLS {
            let p = Pointer {
                space: true,
                ..pen(tool)
            };
            assert_eq!(
                press(p),
                Press::Pan,
                "space is the pan override for {tool:?}"
            );
        }
    }

    #[test]
    fn a_pan_override_beats_the_interface_and_the_interface_beats_the_tool() {
        // A middle-drag begun over a panel still pans, which is what the mouse
        // path has always done; everything else a panel swallows.
        let over_ui = Pointer {
            ui_owns: true,
            ..mouse(Tool::Brush)
        };
        assert_eq!(press(over_ui), Press::Ignored);
        assert_eq!(
            press(Pointer {
                pan_button: true,
                ..over_ui
            }),
            Press::Pan
        );
        assert_eq!(
            press(Pointer {
                alt: true,
                ..over_ui
            }),
            Press::Ignored,
            "Alt must not pick a colour out of a panel"
        );
        assert_eq!(
            press(Pointer {
                contact: true,
                resizing: true,
                ..over_ui
            }),
            Press::Ignored,
            "nor may a contact on a panel resize the brush"
        );
    }

    #[test]
    fn every_press_but_a_stroke_of_its_own_ends_the_stroke_it_interrupts() {
        // The reported sequence: hold left and draw, then press middle. The
        // pan takes `Interaction` over, and `pointer_released` dispatches on
        // `Interaction` — so without this the left button coming up never
        // reaches `finish_stroke`, the dabs hang in the scratch, and the
        // autosave stops for the rest of the session.
        assert!(supersedes_stroke(press(Pointer {
            pan_button: true,
            ..mouse(Tool::Brush)
        })));

        // Over `Press::ALL`, never a list written out here: a variant added
        // later does not appear in a hand-written array, so such a test goes on
        // passing while quietly covering less than its name claims.
        //
        // Two halves guard this, and neither covers the other. The exhaustive
        // `match` in `supersedes_stroke` is what the *compiler* catches — a new
        // variant cannot be added without answering. The length assertion below
        // is what catches `ALL` being left short, which the compiler cannot see
        // because a fixed-size array with the right number of elements in it
        // compiles whatever those elements are. That is the whole of what the
        // two claim between them.
        assert_eq!(Press::ALL.len(), 8, "a variant is missing from Press::ALL");
        for decision in Press::ALL {
            let superseded = supersedes_stroke(decision);
            match decision {
                // The only press the canvas never sees, and the only one that
                // may leave a stroke running. See `supersedes_stroke`.
                Press::Ignored => assert!(!superseded, "a press on a panel ended a stroke"),
                _ => assert!(superseded, "{decision:?} left a stroke running"),
            }
        }
    }

    #[test]
    fn a_pen_landing_during_a_mouse_stroke_does_not_discard_it() {
        // `Paint` looks like the one press that should be excluded — it is a
        // stroke, so ending one to begin another reads like churn — and that
        // reasoning assumed a `Paint` press cannot arrive with a stroke
        // already running. It can, and this is the sequence.
        //
        // `Editor::touches` is written only by the touch arm, so it is empty
        // while a *mouse* stroke is live. A pen coming down is therefore the
        // first contact, which `contact` reads as a press and not a pinch...
        assert_eq!(
            contact(TouchPhase::Started, 1, false, false),
            Contact::Press,
            "a mouse stroke leaves `touches` empty, so the pen is contact one"
        );
        // ...and `press` resolves that to `Paint`, with the brush in hand.
        assert_eq!(press(pen(Tool::Brush)), Press::Paint);
        // Which must end the mouse stroke rather than let `start_stroke` run
        // on an already-active builder: its unconditional `clear_stroke`
        // discarded that stroke with no history entry, silently and not
        // undoably.
        assert!(
            supersedes_stroke(Press::Paint),
            "a pen landing mid-stroke discards the stroke it interrupts"
        );
    }

    #[test]
    fn a_tap_is_short_and_a_drag_is_not() {
        assert!(is_tap(0.0));
        assert!(is_tap(TAP_SLOP));
        assert!(!is_tap(TAP_SLOP + 0.01));
    }

    #[test]
    fn a_pen_in_range_and_off_the_glass_is_never_a_contact() {
        // `known` false is the whole test: an id that never went down.
        assert_eq!(contact(TouchPhase::Moved, 0, false, false), Contact::Hover);
        // Even if something has left `drawing_touch` pointing at it, which is
        // the state that would turn a hover into a stroke.
        assert_eq!(contact(TouchPhase::Moved, 0, false, true), Contact::Hover);
    }

    #[test]
    fn a_press_after_a_hover_is_a_press_and_not_a_pinch() {
        // Windows issues a fresh pointer id per contact session, so a pen that
        // hovered as id 7 comes down as id 8. If the hover had been recorded as
        // a contact, `down` would be 2 here and the press would be read as a
        // second finger — the pinch that ate every pen stroke.
        assert_eq!(
            contact(TouchPhase::Started, 1, false, false),
            Contact::Press
        );
    }

    #[test]
    fn a_second_finger_is_a_pinch_and_the_first_one_stops_owning_the_gesture() {
        assert_eq!(
            contact(TouchPhase::Started, 2, false, false),
            Contact::Pinch
        );
        // While it runs, the contact that is not the owner drives the pinch.
        assert_eq!(
            contact(TouchPhase::Moved, 2, true, false),
            Contact::Pinching
        );
    }

    #[test]
    fn only_the_owning_contact_drives_and_ends_the_gesture() {
        assert_eq!(contact(TouchPhase::Moved, 1, true, true), Contact::Drag);
        assert_eq!(contact(TouchPhase::Ended, 1, true, true), Contact::Release);
        assert_eq!(
            contact(TouchPhase::Cancelled, 1, true, true),
            Contact::Release
        );
        assert_eq!(contact(TouchPhase::Ended, 2, true, false), Contact::Lift);
    }

    #[test]
    fn a_whole_pen_stroke_routes_the_way_a_mouse_drag_does() {
        // The sequence a tablet actually produces on Windows: hovering in,
        // touching down, drawing, lifting off. Read as a press, a drag and a
        // release, which is exactly what a mouse's button-down, move and
        // button-up are — and the press then goes through `press`, which the
        // tests above pin against the mouse's answer for every tool.
        let mut down = 0usize;
        let mut known = false;

        assert_eq!(
            contact(TouchPhase::Moved, down, known, false),
            Contact::Hover,
            "in range, off the glass"
        );
        down += 1;
        assert_eq!(
            contact(TouchPhase::Started, down, known, false),
            Contact::Press
        );
        known = true;
        assert_eq!(contact(TouchPhase::Moved, down, known, true), Contact::Drag);
        assert_eq!(
            contact(TouchPhase::Ended, down, known, true),
            Contact::Release
        );
    }
}
