//! Keyboard shortcuts as data.
//!
//! The bindings live in a table rather than in a `match` so the settings dialog
//! can list them: a match arm can be executed but not enumerated, printed or
//! (later) rebound. [`resolve`] turns a key press back into an [`Action`], and
//! the event loop then does exactly what it did before.

use winit::keyboard::{KeyCode, ModifiersState};

/// Something the user can bind a key to.
///
/// One variant per distinct command, *not* per binding — Redo has two default
/// bindings and is still one action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Undo,
    Redo,
    BrushTool,
    EraserTool,
    PanTool,
    ZoomTool,
    SizeDown,
    SizeUp,
    SwapColours,
    FitView,
    ActualSize,
}

impl Action {
    /// Every action, in the order the settings dialog lists them.
    ///
    /// Walking this rather than `defaults()` means an action with no binding
    /// still appears — shown as unbound — instead of silently vanishing from
    /// the list the moment someone forgets to bind it.
    pub const ALL: [Action; 11] = [
        Action::Undo,
        Action::Redo,
        Action::BrushTool,
        Action::EraserTool,
        Action::PanTool,
        Action::ZoomTool,
        Action::SizeDown,
        Action::SizeUp,
        Action::SwapColours,
        Action::FitView,
        Action::ActualSize,
    ];

    /// Human-readable, British spelling, e.g. "Swap colours".
    pub fn label(self) -> &'static str {
        match self {
            Action::Undo => "Undo",
            Action::Redo => "Redo",
            Action::BrushTool => "Brush tool",
            Action::EraserTool => "Eraser tool",
            Action::PanTool => "Pan tool",
            Action::ZoomTool => "Zoom tool",
            Action::SizeDown => "Decrease brush size",
            Action::SizeUp => "Increase brush size",
            Action::SwapColours => "Swap colours",
            Action::FitView => "Fit to view",
            Action::ActualSize => "Actual size",
        }
    }

    /// Grouping for the settings list.
    pub fn category(self) -> &'static str {
        match self {
            Action::Undo | Action::Redo => "Edit",
            Action::BrushTool | Action::EraserTool | Action::PanTool | Action::ZoomTool => "Tools",
            Action::SizeDown | Action::SizeUp => "Brush",
            Action::SwapColours => "Colour",
            Action::FitView | Action::ActualSize => "View",
        }
    }
}

/// One key press bound to one action.
///
/// The modifier flags are the *required* state of each modifier, not a mask of
/// modifiers to look for: `ctrl: false` means Ctrl must be up. See [`resolve`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub action: Action,
    pub key: KeyCode,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Binding {
    /// Display form, e.g. "Ctrl+Shift+Z", "[", "Space".
    pub fn display(&self) -> String {
        let mut out = String::new();
        // Ctrl, Shift, Alt in that order — the conventional reading order for
        // shortcuts on every desktop platform, so it is worth fixing here
        // rather than following whatever order a binding was written in.
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        out.push_str(&key_name(self.key));
        out
    }
}

/// The default set, grouped sensibly and ordered by category.
pub fn defaults() -> Vec<Binding> {
    vec![
        // Edit
        binding(Action::Undo, KeyCode::KeyZ, true, false, false),
        binding(Action::Redo, KeyCode::KeyZ, true, true, false),
        // Redo's second binding: Ctrl+Y is what Windows users reach for. The
        // list is a `Vec<Binding>`, not a map keyed by action, precisely so an
        // action can carry more than one.
        binding(Action::Redo, KeyCode::KeyY, true, false, false),
        // Tools
        binding(Action::BrushTool, KeyCode::KeyB, false, false, false),
        binding(Action::EraserTool, KeyCode::KeyE, false, false, false),
        binding(Action::PanTool, KeyCode::KeyH, false, false, false),
        binding(Action::ZoomTool, KeyCode::KeyZ, false, false, false),
        // Brush
        binding(Action::SizeDown, KeyCode::BracketLeft, false, false, false),
        binding(Action::SizeUp, KeyCode::BracketRight, false, false, false),
        // Colour
        binding(Action::SwapColours, KeyCode::KeyX, false, false, false),
        // View
        binding(Action::FitView, KeyCode::Digit0, true, false, false),
        binding(Action::ActualSize, KeyCode::Digit1, true, false, false),
    ]
}

/// Find the action bound to this key press, if any.
///
/// Every modifier is compared exactly, including the ones a binding leaves
/// `false`. Treating `false` as "don't care" would make plain Z (zoom tool)
/// fire on Ctrl+Z alongside undo, and would let Ctrl+Z match Redo's
/// Ctrl+Shift+Z. The hand-written `match` this replaces avoided that only
/// through arm ordering, which a user-reorderable table cannot rely on.
pub fn resolve(bindings: &[Binding], key: KeyCode, mods: ModifiersState) -> Option<Action> {
    // Command on macOS carries the shortcuts Ctrl carries elsewhere, and winit
    // reports it as Super, so the two are folded together here rather than
    // duplicating every Ctrl binding.
    let ctrl = mods.control_key() || mods.super_key();
    let shift = mods.shift_key();
    let alt = mods.alt_key();

    bindings
        .iter()
        .find(|b| b.key == key && b.ctrl == ctrl && b.shift == shift && b.alt == alt)
        .map(|b| b.action)
}

/// Terse constructor, so the table above reads as a table.
const fn binding(action: Action, key: KeyCode, ctrl: bool, shift: bool, alt: bool) -> Binding {
    Binding {
        action,
        key,
        ctrl,
        shift,
        alt,
    }
}

/// The printable name of a physical key.
fn key_name(key: KeyCode) -> String {
    let named = match key {
        KeyCode::Space => "Space",
        KeyCode::BracketLeft => "[",
        KeyCode::BracketRight => "]",
        KeyCode::Minus => "-",
        KeyCode::Equal => "=",
        KeyCode::Comma => ",",
        KeyCode::Period => ".",
        KeyCode::Slash => "/",
        KeyCode::Backslash => "\\",
        KeyCode::Semicolon => ";",
        KeyCode::Quote => "'",
        KeyCode::Backquote => "`",
        KeyCode::Tab => "Tab",
        KeyCode::Enter => "Enter",
        KeyCode::Escape => "Esc",
        KeyCode::Backspace => "Backspace",
        KeyCode::Delete => "Delete",
        KeyCode::ArrowUp => "Up",
        KeyCode::ArrowDown => "Down",
        KeyCode::ArrowLeft => "Left",
        KeyCode::ArrowRight => "Right",
        // The letter and digit rows debug-print as "KeyB" and "Digit0", so
        // trimming the prefix beats thirty-six explicit arms. Anything else
        // keeps winit's own name, which is at least recognisable ("F1").
        other => {
            let name = format!("{other:?}");
            return match name
                .strip_prefix("Key")
                .or_else(|| name.strip_prefix("Digit"))
            {
                Some(rest) => rest.to_string(),
                None => name,
            };
        }
    };
    named.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_a_default_binding() {
        let bindings = defaults();
        for action in Action::ALL {
            assert!(
                bindings.iter().any(|b| b.action == action),
                "{action:?} has no default binding"
            );
        }
    }
    use std::collections::HashSet;

    const CTRL: ModifiersState = ModifiersState::CONTROL;
    const SHIFT: ModifiersState = ModifiersState::SHIFT;
    const ALT: ModifiersState = ModifiersState::ALT;
    const SUPER: ModifiersState = ModifiersState::SUPER;
    const NONE: ModifiersState = ModifiersState::empty();

    fn hit(key: KeyCode, mods: ModifiersState) -> Option<Action> {
        resolve(&defaults(), key, mods)
    }

    #[test]
    fn plain_z_selects_the_zoom_tool_and_is_not_undo() {
        assert_eq!(hit(KeyCode::KeyZ, NONE), Some(Action::ZoomTool));
    }

    #[test]
    fn ctrl_z_is_undo_and_never_the_zoom_tool() {
        // The whole point of exact modifier matching: an unmodified binding
        // must not fire while Ctrl is held.
        assert_eq!(hit(KeyCode::KeyZ, CTRL), Some(Action::Undo));
    }

    #[test]
    fn ctrl_shift_z_is_redo_not_undo() {
        assert_eq!(hit(KeyCode::KeyZ, CTRL | SHIFT), Some(Action::Redo));
    }

    #[test]
    fn ctrl_y_is_the_second_redo_binding() {
        assert_eq!(hit(KeyCode::KeyY, CTRL), Some(Action::Redo));
        assert_eq!(hit(KeyCode::KeyY, NONE), None);
    }

    #[test]
    fn super_stands_in_for_ctrl() {
        // macOS: Cmd+Z must undo without a duplicate table of bindings.
        assert_eq!(hit(KeyCode::KeyZ, SUPER), Some(Action::Undo));
        assert_eq!(hit(KeyCode::KeyZ, SUPER | SHIFT), Some(Action::Redo));
        assert_eq!(hit(KeyCode::Digit0, SUPER), Some(Action::FitView));
    }

    #[test]
    fn unwanted_modifiers_block_a_match() {
        // Shift+B is a capital B, not a request to switch tools; Alt opens
        // menus on Windows and must not leak through either.
        assert_eq!(hit(KeyCode::KeyB, NONE), Some(Action::BrushTool));
        assert_eq!(hit(KeyCode::KeyB, SHIFT), None);
        assert_eq!(hit(KeyCode::KeyB, ALT), None);
        assert_eq!(hit(KeyCode::KeyZ, CTRL | ALT), None);
        assert_eq!(hit(KeyCode::KeyZ, CTRL | SHIFT | ALT), None);
    }

    #[test]
    fn tool_and_view_bindings_match_the_app() {
        assert_eq!(hit(KeyCode::KeyE, NONE), Some(Action::EraserTool));
        assert_eq!(hit(KeyCode::KeyH, NONE), Some(Action::PanTool));
        assert_eq!(hit(KeyCode::KeyX, NONE), Some(Action::SwapColours));
        assert_eq!(hit(KeyCode::BracketLeft, NONE), Some(Action::SizeDown));
        assert_eq!(hit(KeyCode::BracketRight, NONE), Some(Action::SizeUp));
        assert_eq!(hit(KeyCode::Digit0, CTRL), Some(Action::FitView));
        assert_eq!(hit(KeyCode::Digit1, CTRL), Some(Action::ActualSize));
    }

    #[test]
    fn unbound_keys_resolve_to_nothing() {
        assert_eq!(hit(KeyCode::KeyQ, NONE), None);
        assert_eq!(hit(KeyCode::Digit0, NONE), None);
        // Space is a held pan modifier, deliberately not a table entry.
        assert_eq!(hit(KeyCode::Space, NONE), None);
    }

    #[test]
    fn every_default_binding_names_a_known_action() {
        for b in defaults() {
            assert!(
                Action::ALL.contains(&b.action),
                "{:?} is missing from Action::ALL",
                b.action
            );
        }
    }

    #[test]
    fn no_two_defaults_share_a_key_combination() {
        // A collision would be resolved silently by table order, which is the
        // failure mode this whole module exists to remove.
        let mut seen = HashSet::new();
        for b in defaults() {
            assert!(
                seen.insert((b.key, b.ctrl, b.shift, b.alt)),
                "{} is bound twice",
                b.display()
            );
        }
    }

    #[test]
    fn defaults_are_grouped_by_category() {
        // The settings dialog prints a heading whenever the category changes,
        // so a category must not reappear once another has started.
        let mut finished: Vec<&str> = Vec::new();
        let mut current = "";
        for b in defaults() {
            let category = b.action.category();
            if category != current {
                assert!(
                    !finished.contains(&category),
                    "category {category} is split up"
                );
                if !current.is_empty() {
                    finished.push(current);
                }
                current = category;
            }
        }
    }

    #[test]
    fn display_spells_out_the_combination() {
        let by = |action: Action| -> String {
            defaults()
                .into_iter()
                .find(|b| b.action == action)
                .expect("action has a binding")
                .display()
        };
        assert_eq!(by(Action::Undo), "Ctrl+Z");
        assert_eq!(by(Action::Redo), "Ctrl+Shift+Z");
        assert_eq!(by(Action::SizeDown), "[");
        assert_eq!(by(Action::SizeUp), "]");
        assert_eq!(by(Action::BrushTool), "B");
        assert_eq!(by(Action::FitView), "Ctrl+0");

        let space = binding(Action::PanTool, KeyCode::Space, false, false, false);
        assert_eq!(space.display(), "Space");
        let everything = binding(Action::PanTool, KeyCode::KeyH, true, true, true);
        assert_eq!(everything.display(), "Ctrl+Shift+Alt+H");
    }

    #[test]
    fn labels_use_british_spelling() {
        assert_eq!(Action::SwapColours.label(), "Swap colours");
        for action in Action::ALL {
            assert!(!action.label().is_empty());
            assert!(!action.category().is_empty());
        }
    }
}
