//! Painted controls used by the settings page.
//!
//! Same approach as `widgets.rs`, and for the same reason: egui's stock button,
//! text field and frame have a look of their own that the design does not use,
//! and restyling them fights the framework. These are drawn directly.
//!
//! What lives here rather than in `widgets.rs` is what only the settings page
//! needs — keycaps, the field that listens for a chord, and the two glyphs the
//! shared icon set does not carry yet.

use crate::icons::{self, Icon};
use crate::shortcuts::Chord;
use crate::theme::{Palette, metrics, text};
use egui::{
    Align2, Color32, FontId, Painter, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, pos2, vec2,
};
use winit::keyboard::KeyCode;

// ---------------------------------------------------------------------------
// Glyphs
// ---------------------------------------------------------------------------

/// Small symbols the settings page needs.
///
/// `Plus` and `Close` come from the shared icon set; `Revert` and `Warning` are
/// drawn here because nothing else has wanted them yet. Both are authored
/// against the same 24×24 box `icons` uses, so they can move across unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Plus,
    Close,
    /// A circular arrow: restore the default.
    Revert,
    /// A triangle with a bang: this chord is bound twice.
    Warning,
    /// A magnifier, for the design's action search field.
    Search,
}

pub fn draw_glyph(painter: &Painter, rect: Rect, glyph: Glyph, colour: Color32) {
    match glyph {
        Glyph::Plus => icons::draw(painter, rect, Icon::Plus, colour),
        Glyph::Close => icons::draw(painter, rect, Icon::Close, colour),
        Glyph::Revert => revert(painter, rect, colour),
        Glyph::Warning => warning(painter, rect, colour),
        Glyph::Search => search(painter, rect, colour),
    }
}

/// A ring with a tail, matching the design's 18×18 search icon.
fn search(painter: &Painter, rect: Rect, colour: Color32) {
    let size = rect.width().min(rect.height());
    if size <= 1.0 {
        return;
    }
    let stroke = Stroke::new((size / 9.0).max(1.0), colour);
    let c = rect.center();
    let r = size * 0.27;
    let ring = pos2(c.x - size * 0.06, c.y - size * 0.06);
    painter.circle_stroke(ring, r, stroke);
    let start = pos2(ring.x + r * 0.72, ring.y + r * 0.72);
    painter.line_segment([start, pos2(c.x + size * 0.36, c.y + size * 0.36)], stroke);
}

/// Three quarters of a circle with an arrowhead, turning anticlockwise.
fn revert(painter: &Painter, rect: Rect, colour: Color32) {
    let size = rect.width().min(rect.height());
    if size <= 1.0 {
        return;
    }
    let stroke = Stroke::new((size / 12.0).max(1.0), colour);
    let centre = rect.center();
    let radius = size * 0.34;

    // The gap sits at the top-left so the arrowhead lands somewhere legible at
    // 12 px, where a head on a diagonal turns into a smudge.
    const STEPS: usize = 20;
    let start = std::f32::consts::PI * 1.15;
    let sweep = std::f32::consts::PI * 1.6;
    let arc: Vec<Pos2> = (0..=STEPS)
        .map(|i| {
            let a = start + sweep * (i as f32 / STEPS as f32);
            pos2(centre.x + radius * a.cos(), centre.y + radius * a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(arc.clone(), stroke));

    let tip = arc[0];
    let head = size * 0.18;
    painter.add(egui::Shape::line(
        vec![
            tip + vec2(-head * 0.2, -head),
            tip,
            tip + vec2(head, -head * 0.2),
        ],
        stroke,
    ));
}

/// An equilateral triangle with a bar and a dot inside it.
fn warning(painter: &Painter, rect: Rect, colour: Color32) {
    let size = rect.width().min(rect.height());
    if size <= 1.0 {
        return;
    }
    let stroke = Stroke::new((size / 12.0).max(1.0), colour);
    let c = rect.center();
    let h = size * 0.38;
    let w = size * 0.44;

    painter.add(egui::Shape::line(
        vec![
            pos2(c.x, c.y - h),
            pos2(c.x + w, c.y + h * 0.75),
            pos2(c.x - w, c.y + h * 0.75),
            pos2(c.x, c.y - h),
        ],
        stroke,
    ));
    painter.line_segment(
        [pos2(c.x, c.y - h * 0.25), pos2(c.x, c.y + h * 0.2)],
        stroke,
    );
    painter.circle_filled(pos2(c.x, c.y + h * 0.48), stroke.width * 0.6, colour);
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// A square icon button, 20×20, for the trailing controls on a shortcut row.
///
/// A disabled one still hovers, because the tooltip explaining *why* it is
/// disabled is the whole reason to draw it rather than hide it.
pub fn icon_button(
    ui: &mut Ui,
    p: &Palette,
    glyph: Glyph,
    enabled: bool,
    tooltip: &str,
) -> Response {
    // A disabled control senses hover but not clicks. egui has no insensitive
    // state for a hand-painted widget, and letting a greyed-out button report a
    // click is how a dead control ends up doing something; the hover is kept so
    // the tooltip explaining *why* it is dead still appears.
    let (rect, response) = ui.allocate_exact_size(
        Vec2::splat(20.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );

    let painter = ui.painter();
    if enabled && response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control_hover);
    }
    let colour = match (enabled, response.hovered()) {
        (false, _) => p.text_dim.gamma_multiply(0.45),
        (true, true) => p.text_strong,
        (true, false) => p.text_muted,
    };
    draw_glyph(painter, rect.shrink(4.0), glyph, colour);

    response.on_hover_text(tooltip)
}

/// A small pill with a word in it.
///
/// `emphasis` marks the one the user most likely wants — the accent border
/// stands in for a filled primary button, which the design does not use.
pub fn text_button(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    emphasis: bool,
    enabled: bool,
) -> Response {
    let font = FontId::proportional(text::TINY);
    let width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), p.text)
        .size()
        .x
        + 18.0;
    let (rect, response) = ui.allocate_exact_size(
        vec2(width, metrics::TEXT_BUTTON),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );

    let painter = ui.painter();
    let fill = match (enabled, response.hovered()) {
        (false, _) => p.window,
        (true, true) => p.control_hover,
        (true, false) => p.control,
    };
    painter.rect_filled(rect, metrics::RADIUS, fill);
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(
            1.0,
            if emphasis && enabled {
                p.accent
            } else {
                p.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font,
        match (enabled, emphasis) {
            (false, _) => p.text_dim.gamma_multiply(0.5),
            (true, true) => p.accent,
            (true, false) => p.text,
        },
    );

    response
}

// ---------------------------------------------------------------------------
// Keycaps
// ---------------------------------------------------------------------------

/// How a keycap is being shown.
///
/// There is no "listening" cap: while a row is armed the design replaces the
/// cap entirely with the pulsing [`capture_hint`], so there is nothing left to
/// draw a cap for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapState {
    /// A bound chord.
    Bound,
    /// A bound chord that another command also holds.
    Clashing,
    /// The action has no binding at all.
    Unbound,
}

pub struct CapResponse {
    /// The cap itself was clicked — start listening.
    pub clicked: bool,
    /// The clear affordance inside it was clicked — drop this binding.
    pub cleared: bool,
}

/// A keycap-ish chip, so a chord reads as something you press.
///
/// `clearable` reserves room for the clear cross always, and the cross itself
/// only appears under the pointer. Revealing it on hover *and* widening the
/// chip at the same time would shuffle every cap to its left out from under the
/// cursor at the moment the user went to click one.
pub fn keycap(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    state: CapState,
    clearable: bool,
) -> CapResponse {
    let font = FontId::monospace(text::TINY);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), p.text)
        .size()
        .x;
    let cross = if clearable { 15.0 } else { 0.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(text_w + 16.0 + cross, 20.0), Sense::click());

    // The design gives a clashing cap its own warm palette (#2A1D18 fill,
    // #6E4034 border, #D08770 ink). `theme::Palette` carries no caution colour
    // yet, so the nearest warm tokens stand in: `control_active` is the warm
    // tinted fill and `accent` is the warm ink, in both themes.
    let (fill, border, ink) = match state {
        CapState::Bound => (p.window, p.border, p.text),
        CapState::Clashing => (p.control_active, p.accent_dim, p.accent),
        CapState::Unbound => (Color32::TRANSPARENT, p.border, p.text_dim),
    };

    let painter = ui.painter();
    if fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, 4.0, fill);
    }
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    painter.text(
        pos2(rect.left() + 8.0 + text_w * 0.5, rect.center().y),
        Align2::CENTER_CENTER,
        label,
        font,
        ink,
    );

    let mut cleared = false;
    if clearable {
        let hit = Rect::from_center_size(
            pos2(rect.right() - cross * 0.5 - 2.0, rect.center().y),
            Vec2::splat(15.0),
        );
        let clear = ui.interact(
            hit,
            ui.id().with(("clear", label, rect.left() as i32)),
            Sense::click(),
        );
        // Only under the pointer: a permanent cross on every cap is noise the
        // design does not have, and the space is reserved either way so
        // revealing it moves nothing.
        if response.hovered() || clear.hovered() {
            draw_glyph(
                ui.painter(),
                hit.shrink(3.0),
                Glyph::Close,
                if clear.hovered() {
                    p.text_strong
                } else {
                    p.text_dim.gamma_multiply(0.7)
                },
            );
        }
        cleared = clear.clicked();
    }

    CapResponse {
        // A click that landed on the cross is not a request to rebind.
        clicked: response.clicked() && !cleared,
        cleared,
    }
}

/// The design's armed-field label: "press keys" in the accent, pulsing, with
/// "esc to cancel" beside it in dim.
///
/// Saying how to get out, in the field itself, is the whole point. A capture
/// field that swallows every key press and offers no way back would let a user
/// bind over their own escape route and be stuck.
pub fn capture_hint(ui: &mut Ui, p: &Palette) {
    // 1.4 s, opacity 0.5 to 1.0 and back — `umpulse` from the design.
    const PERIOD: f64 = 1.4;
    let phase = (ui.input(|i| i.time) % PERIOD) / PERIOD;
    let alpha = 0.5 + 0.5 * (phase * std::f64::consts::TAU).sin().abs() as f32;
    // An animation only advances if frames keep coming, and Umber idles by
    // design.
    ui.ctx().request_repaint();

    let font = FontId::proportional(text::TINY);
    let armed = "press keys";
    let escape = "esc to cancel";
    let painter = ui.painter();
    let armed_w = painter
        .layout_no_wrap(armed.to_owned(), font.clone(), p.accent)
        .size()
        .x;
    let escape_w = painter
        .layout_no_wrap(escape.to_owned(), font.clone(), p.text_dim)
        .size()
        .x;

    let (rect, _) = ui.allocate_exact_size(vec2(armed_w + escape_w + 8.0, 20.0), Sense::hover());
    let painter = ui.painter();
    painter.text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        armed,
        font.clone(),
        p.accent.gamma_multiply(alpha),
    );
    painter.text(
        rect.right_center(),
        Align2::RIGHT_CENTER,
        escape,
        font,
        p.text_dim.gamma_multiply(alpha),
    );
}

/// The design's inline conflict flag: `conflicts with "Undo"`.
///
/// Warm, small and next to the name it belongs to, rather than a banner at the
/// top — a clash belongs on the row that has it.
pub fn conflict_badge(ui: &mut Ui, p: &Palette, message: &str) {
    let font = FontId::proportional(9.5);
    let width = ui
        .painter()
        .layout_no_wrap(message.to_owned(), font.clone(), p.warning)
        .size()
        .x;
    let (rect, _) = ui.allocate_exact_size(vec2(width + 12.0, 16.0), Sense::hover());
    let painter = ui.painter();
    // The warning tokens, not the accent: a clash is not a selection, and
    // borrowing the accent for it made every flagged row read as chosen.
    painter.rect_filled(rect, 3.0, p.warning_bg);
    painter.rect_stroke(
        rect,
        3.0,
        Stroke::new(1.0, p.warning_border),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        message,
        font,
        p.warning,
    );
}

/// One entry in the settings dialog's left rail.
///
/// The design marks the selected tab with an accent bar down its leading edge
/// and a heavier weight; egui's `strong()` changes colour rather than weight,
/// so the selection reads through colour and the bar.
pub fn sidebar_tab(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    selected: bool,
    enabled: bool,
    tooltip: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), 26.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );

    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, metrics::RADIUS, p.control_hover);
    } else if enabled && response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control);
    }
    if selected {
        painter.rect_filled(
            Rect::from_min_size(rect.left_top(), vec2(2.0, rect.height())),
            0.0,
            p.accent,
        );
    }
    painter.text(
        rect.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(text::SMALL),
        match (enabled, selected) {
            (false, _) => p.text_dim.gamma_multiply(0.45),
            (true, true) => p.text_strong,
            (true, false) => p.text_muted,
        },
    );

    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// The design's action search: a magnifier and a borderless field in a well.
///
/// The text field itself is egui's — text entry is caret, selection, IME and
/// clipboard, none of which is worth reimplementing to change a border. Only
/// the frame around it is painted.
pub fn search_field(ui: &mut Ui, p: &Palette, query: &mut String, hint: &str) {
    egui::Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS_LARGE)
        .inner_margin(egui::Margin::symmetric(10, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (mark, _) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::hover());
                draw_glyph(ui.painter(), mark, Glyph::Search, p.text_dim);
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(query)
                        .frame(egui::Frame::NONE)
                        .hint_text(hint)
                        .desired_width(ui.available_width())
                        .font(FontId::proportional(text::CONTROL))
                        .text_color(p.text_strong),
                );
            });
        });
}

// ---------------------------------------------------------------------------
// Capturing a chord
// ---------------------------------------------------------------------------

/// What listening for a chord produced.
pub enum Captured {
    Chord(Chord),
    /// Escape: leave the binding as it was.
    Cancelled,
    /// A key that cannot carry a shortcut, and why.
    Rejected(&'static str),
}

/// Consume key presses and turn the first usable one into a chord.
///
/// The events are *removed* from egui's queue, not merely read: while a field
/// is listening, Tab must not move focus and Enter must not activate anything.
/// Dispatch to the canvas is suspended separately, by
/// `shortcuts::set_capturing` — otherwise pressing B to bind it would also
/// select the brush.
pub fn capture(ui: &Ui) -> Option<Captured> {
    let mut result = None;
    ui.input_mut(|input| {
        input.events.retain(|event| {
            let egui::Event::Key {
                key,
                physical_key,
                pressed,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            if !*pressed {
                return false;
            }
            if result.is_some() {
                return false;
            }

            // The physical key, matching what the event loop dispatches on: a
            // binding must sit at a position on the keyboard, not on whichever
            // letter the current layout puts there.
            let key = physical_key.unwrap_or(*key);
            result = Some(match key {
                egui::Key::Escape => Captured::Cancelled,
                egui::Key::Space => {
                    Captured::Rejected("Space is reserved for panning while you draw.")
                }
                key => match key_code(key) {
                    Some(code) => Captured::Chord(Chord::new(
                        code,
                        // egui's `command` is Ctrl elsewhere and Cmd on macOS;
                        // `resolve` folds Ctrl and Super together the same way.
                        modifiers.command || modifiers.ctrl,
                        modifiers.shift,
                        modifiers.alt,
                    )),
                    None => Captured::Rejected("That key cannot carry a shortcut."),
                },
            });
            false
        });
    });
    result
}

/// egui's key back to the physical key the event loop dispatches on.
///
/// Modifier keys are absent on purpose — they are the other half of a chord,
/// never the whole of one — and so is the numpad, which egui reports as the
/// digit row, so a numpad binding would silently be a digit binding.
fn key_code(key: egui::Key) -> Option<KeyCode> {
    use egui::Key as K;
    Some(match key {
        K::A => KeyCode::KeyA,
        K::B => KeyCode::KeyB,
        K::C => KeyCode::KeyC,
        K::D => KeyCode::KeyD,
        K::E => KeyCode::KeyE,
        K::F => KeyCode::KeyF,
        K::G => KeyCode::KeyG,
        K::H => KeyCode::KeyH,
        K::I => KeyCode::KeyI,
        K::J => KeyCode::KeyJ,
        K::K => KeyCode::KeyK,
        K::L => KeyCode::KeyL,
        K::M => KeyCode::KeyM,
        K::N => KeyCode::KeyN,
        K::O => KeyCode::KeyO,
        K::P => KeyCode::KeyP,
        K::Q => KeyCode::KeyQ,
        K::R => KeyCode::KeyR,
        K::S => KeyCode::KeyS,
        K::T => KeyCode::KeyT,
        K::U => KeyCode::KeyU,
        K::V => KeyCode::KeyV,
        K::W => KeyCode::KeyW,
        K::X => KeyCode::KeyX,
        K::Y => KeyCode::KeyY,
        K::Z => KeyCode::KeyZ,
        K::Num0 => KeyCode::Digit0,
        K::Num1 => KeyCode::Digit1,
        K::Num2 => KeyCode::Digit2,
        K::Num3 => KeyCode::Digit3,
        K::Num4 => KeyCode::Digit4,
        K::Num5 => KeyCode::Digit5,
        K::Num6 => KeyCode::Digit6,
        K::Num7 => KeyCode::Digit7,
        K::Num8 => KeyCode::Digit8,
        K::Num9 => KeyCode::Digit9,
        K::F1 => KeyCode::F1,
        K::F2 => KeyCode::F2,
        K::F3 => KeyCode::F3,
        K::F4 => KeyCode::F4,
        K::F5 => KeyCode::F5,
        K::F6 => KeyCode::F6,
        K::F7 => KeyCode::F7,
        K::F8 => KeyCode::F8,
        K::F9 => KeyCode::F9,
        K::F10 => KeyCode::F10,
        K::F11 => KeyCode::F11,
        K::F12 => KeyCode::F12,
        K::Backtick => KeyCode::Backquote,
        K::Minus => KeyCode::Minus,
        K::Equals => KeyCode::Equal,
        K::OpenBracket => KeyCode::BracketLeft,
        K::CloseBracket => KeyCode::BracketRight,
        K::Backslash => KeyCode::Backslash,
        K::Semicolon => KeyCode::Semicolon,
        K::Quote => KeyCode::Quote,
        K::Comma => KeyCode::Comma,
        K::Period => KeyCode::Period,
        K::Slash => KeyCode::Slash,
        K::Tab => KeyCode::Tab,
        K::Enter => KeyCode::Enter,
        K::Backspace => KeyCode::Backspace,
        K::Delete => KeyCode::Delete,
        K::Insert => KeyCode::Insert,
        K::Home => KeyCode::Home,
        K::End => KeyCode::End,
        K::PageUp => KeyCode::PageUp,
        K::PageDown => KeyCode::PageDown,
        K::ArrowUp => KeyCode::ArrowUp,
        K::ArrowDown => KeyCode::ArrowDown,
        K::ArrowLeft => KeyCode::ArrowLeft,
        K::ArrowRight => KeyCode::ArrowRight,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// A small dim heading above a group of settings.
pub fn section(ui: &mut Ui, p: &Palette, title: &str) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(title)
            .size(text::SMALL)
            .color(p.text_dim)
            .strong(),
    );
    ui.add_space(6.0);
}

/// The sentence under a control that says what it does, or why it cannot.
pub fn note(ui: &mut Ui, p: &Palette, body: &str) {
    ui.label(
        egui::RichText::new(body)
            .size(10.0)
            .color(p.text_dim)
            .line_height(Some(13.0)),
    );
}

/// A label on the left and whatever the closure draws on the right.
pub fn row<R>(ui: &mut Ui, p: &Palette, label: &str, right: impl FnOnce(&mut Ui) -> R) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(text::SMALL).color(p.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
}

/// An inset strip carrying a warning and the buttons that answer it.
pub fn banner<R>(ui: &mut Ui, p: &Palette, message: &str, buttons: impl FnOnce(&mut Ui) -> R) {
    egui::Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.accent_dim))
        .corner_radius(metrics::RADIUS)
        .inner_margin(egui::Margin::symmetric(8, 7))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (mark, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                draw_glyph(ui.painter(), mark, Glyph::Warning, p.accent);
                ui.add_space(2.0);
                // Wrapped explicitly. A label in a horizontal layout defaults to
                // `TextWrapMode::Extend`, so a long message does not run onto a
                // second line — it makes the strip wider, and with it whatever
                // dialog the strip is in. A banner carries what went wrong, and
                // what went wrong is exactly the text nobody sized the window
                // for.
                ui.add(
                    egui::Label::new(egui::RichText::new(message).size(text::TINY).color(p.text))
                        .wrap(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), buttons);
            });
        });
}
