//! The Umber workspace.
//!
//! Layout follows the "Umber app" screen of the design project: menu bar, tool
//! options strip, the canvas, and the modules — the tool rail, Colour, Brushes
//! and Layers — in columns either side of it.
//!
//! Where those modules sit is no longer fixed. They can be dragged between the
//! two edges, stacked in a column, put in a column of their own, torn off to
//! float over the canvas, and closed; the columns and the panels within them
//! resize. That machinery lives in [`crate::dock`] (the model) and
//! [`crate::panels`] (the drawing) rather than here, because this file was
//! already long enough — including the tool rail, which used to be a strip of
//! chrome laid out here and is now a module like any other.
//!
//! There used to be a global "left-handed" flag that mirrored the whole
//! workspace. It is gone, and so is the tool rail's own side setting that
//! outlived it: both were worse versions of "put the panels where you want
//! them".

use crate::editor::{BrushTab, Editor, Tool};
use crate::icons::{self, Icon};
use crate::panels;
use crate::shortcuts::{self, Action};
use crate::tabs;
use crate::theme::{Palette, metrics, text};
use crate::widgets;
use egui::{Align2, FontId, Frame, Margin, Rect, Sense, Stroke, pos2, vec2};
use std::sync::Arc;
use umber_core::{
    Brush, DabInput, DabTarget, GrainPattern, Modulation, ResponseCurve, ScrollSpan, SelectionMode,
    TipMask, input::PressureSource,
};

/// Requests the UI makes that need GPU access, handled by the caller.
#[derive(Default, Clone, Copy)]
pub struct UiActions {
    pub clear: bool,
    pub export: bool,
    /// Write the document to the file it came from, asking for one if it has
    /// none yet.
    pub save: bool,
    /// Always ask for a file, even when the document already has one.
    pub save_as: bool,
    /// Save, and close this document if — and only if — the save succeeds.
    /// Cancelling the file dialog therefore leaves the tab open, which is the
    /// only safe reading of "Save" on a prompt about losing work.
    pub save_and_close: Option<usize>,
    pub undo: bool,
    pub redo: bool,
    /// Move the document to this position in the history — a click on a row of
    /// the History module. Carried out by the caller as that many undo or redo
    /// steps, since each one reads and writes a rect on the GPU.
    pub history_jump: Option<usize>,
    pub fit_view: bool,
    pub reset_zoom: bool,
    pub add_layer: bool,
    pub delete_layer: Option<usize>,
    pub move_layer_up: Option<usize>,
    pub move_layer_down: Option<usize>,
    /// Make this document active. Every document has GPU storage of its own,
    /// so the switch is the caller's to carry out.
    pub pick_tab: Option<usize>,
    /// Close this document, having already been confirmed if it holds work.
    pub close_tab: Option<usize>,
    /// Open a blank document with exactly these settings — the New document
    /// dialog's answer. Separate from `new_document`, which is the tab strip's
    /// `+` and inherits the document in front.
    pub create_document: Option<umber_core::Document>,
    /// Change the live document's canvas. See [`crate::canvasdlg`].
    pub canvas_change: Option<crate::canvasdlg::CanvasChange>,
    pub new_document: bool,
    pub open_file: bool,
    /// Close the window, every document with unsaved work having been accounted
    /// for. See [`crate::tabs::quit_prompt`].
    pub quit: bool,
    /// Write every document that holds work and then quit — but only if all of
    /// them are actually written. A cancelled file dialog is not permission to
    /// discard the rest, exactly as it is not for one tab.
    pub save_all_and_quit: bool,
    /// Open the internal autosave location in the system file manager.
    pub reveal_autosaves: bool,
}

pub struct UiOutput {
    pub actions: UiActions,
    /// Region left for the document, in egui points.
    pub canvas_rect: Rect,
}

/// egui 0.35 merged `SidePanel`/`TopBottomPanel` into one `Panel` type that
/// nests inside a `Ui` rather than attaching to the `Context`, which is why
/// this takes a `&mut Ui`.
pub fn draw(root: &mut egui::Ui, ed: &mut Editor) -> UiOutput {
    let p = Palette::with_accent(ed.ui.theme, ed.ui.accent);
    let mut actions = UiActions::default();

    // The design gives every chrome strip a hairline along the edge it meets
    // the next one at. egui's own panel separator is exactly that — it uses
    // `noninteractive.bg_stroke`, which `theme::apply` sets to the design's
    // border colour — so these are left with it switched on and draw none of
    // their own.
    let pad = Margin::symmetric(metrics::STRIP_PAD, 0);
    let chrome = Frame {
        fill: p.chrome,
        ..Default::default()
    };

    egui::Panel::top("menu-bar")
        .exact_size(metrics::MENU_BAR)
        .frame(chrome.inner_margin(pad))
        .show(root, |ui| menu_bar(ui, &p, ed, &mut actions));

    // Between the menu bar and the tool options, where the design draws it.
    // It takes its 30 points out of the window like any other panel, so the
    // canvas region — and with it the camera pivot every dab is placed against
    // — shrinks to match without anything here having to say so.
    let tab_strip = Frame {
        fill: p.dock,
        ..Default::default()
    };
    let mut tab_actions = tabs::TabActions::default();
    egui::Panel::top("doc-tabs")
        .exact_size(metrics::TAB_STRIP)
        .frame(tab_strip)
        // The one strip that draws its own rule, because the active tab has to
        // break through it to join the surface below. egui's separator goes on
        // top of the panel's own contents, so leaving it on drew a line straight
        // across the bottom of the selected tab.
        .show_separator_line(false)
        .show(root, |ui| tab_actions = tabs::strip(ui, &p, ed));

    egui::Panel::top("options-strip")
        .exact_size(metrics::OPTIONS_STRIP)
        .frame(chrome.inner_margin(pad))
        .show(root, |ui| options_strip(ui, &p, ed));

    egui::Panel::bottom("status-bar")
        .exact_size(metrics::STATUS_BAR)
        .frame(chrome.inner_margin(pad))
        .show(root, |ui| status_bar(ui, &p, ed, &mut actions));

    // Only present in layout edit mode, and claimed before the workspace is
    // measured so the sidebars sit under it rather than behind it.
    panels::edit_bar(root, &p, ed);

    // Everything below the strips and above the status bar is the layout's to
    // divide up. Measuring it here, before any of it is claimed, is what lets
    // the dock model compute every rect up front — so the drop indicator and
    // the panels it predicts cannot disagree.
    let workspace = root.available_rect_before_wrap();
    ed.layout.clamp_floating(workspace);
    let geo = ed.layout.geometry(workspace);

    panels::sidebars(root, &p, ed, &mut actions, &geo);

    // The strip only reports; acting on it is the caller's, because every
    // document owns GPU storage that has to be created, switched or freed.
    actions.pick_tab = tab_actions.pick;
    actions.new_document |= tab_actions.new_document;
    if let Some(index) = tab_actions.close {
        if ed.session.tabs().get(index).is_some_and(|tab| tab.modified) {
            // Show the document before asking about it: a prompt that offers to
            // export a canvas you cannot see is asking about the wrong one.
            actions.pick_tab = Some(index);
            ed.ui.close_prompt = Some(index);
        } else {
            actions.close_tab = Some(index);
        }
    }

    brush_editor(root, &p, ed);
    crate::settings::show(root, &p, ed, &mut actions);
    // About, the first-run notice about the update check, and the prompt the
    // check raises. Drawn from here rather than from the Help menu, for the
    // same reason the brush library's modals are drawn from `panels`: a menu
    // closes the moment it is clicked, and a dialog owned by something that is
    // no longer on screen cannot be shut.
    crate::about::show(root, &p, ed);

    // Drawn here rather than from a panel body, for the same reason the brush
    // library's modals are: the layout can hide a panel, and a modal that goes
    // with one cannot then be shut or reopened.
    let mut canvas = crate::canvasdlg::Outcome::default();
    crate::canvasdlg::show(root, &p, ed, &mut canvas);
    actions.create_document = canvas.create;
    actions.canvas_change = canvas.change;

    // Before the close prompt, and above it: this one is the answer to "the
    // window is closing", which supersedes any question about a single tab.
    match tabs::quit_prompt(root, &p, ed) {
        Some(tabs::QuitChoice::Discard) => actions.quit = true,
        Some(tabs::QuitChoice::SaveAll) => actions.save_all_and_quit = true,
        Some(tabs::QuitChoice::Cancel) | None => {}
    }

    match tabs::close_prompt(root, &p, ed) {
        Some(tabs::CloseChoice::Close) => actions.close_tab = ed.ui.close_prompt.take(),
        // The prompt closes now, but the tab only closes if the save succeeds —
        // a cancelled file dialog must not be a silent discard. See
        // `UiActions::save_and_close`.
        Some(tabs::CloseChoice::Save) => actions.save_and_close = ed.ui.close_prompt.take(),
        // Export keeps a copy of the picture but is not an answer to "close
        // this?", so the prompt stays open behind it.
        Some(tabs::CloseChoice::Export) => actions.export = true,
        Some(tabs::CloseChoice::Cancel) | None => {}
    }
    tabs::notice(root, &p, ed);

    // Whatever is left is the document's. The canvas is drawn by the GPU
    // beneath egui, so this panel only reports its rect and stays transparent.
    //
    // Floating panels are added *after* this deliberately. They are egui
    // `Area`s, which claim no space, so the canvas rect — and therefore
    // `Editor::canvas_pivot` and `CompositeParams::pivot` — is the same whether
    // a panel hovers over the canvas or not. Making them panels instead would
    // shrink this rect, move the pivot, and land every dab away from the
    // cursor.
    let canvas_rect = egui::CentralPanel::default()
        .frame(Frame::NONE)
        .show(root, |ui| {
            let rect = ui.max_rect();
            selection_outline(ui, &p, ed, rect);
            transform_box(ui, &p, ed, rect);
            canvas_scrollbars(ui, &p, ed, rect);
            brush_size_preview(ui, &p, ed);
            // After the preview, so the pen sits on top of the ring rather
            // than under it. Both are drawn when Alt is held with a pen, and
            // deliberately: they answer different questions — the ring is a
            // measurement anchored where the gesture began, the dot is where
            // the nib is now, and watching the second cross the first is how
            // the size is judged.
            pen_cursor(ui, &p, ed);
            rect
        })
        .inner;

    panels::floats(root, &p, ed, &mut actions);
    panels::edit_mode_outline(root, &p, ed, &geo);
    // Last, so the drop it resolves is tested against a frame in which every
    // panel has already had its say.
    panels::drag_overlay(root, &p, ed, &geo);
    ed.layout.save_if_dirty();

    // Keys are read off the winit event before egui is asked, so a field with
    // the keyboard has to say so or every letter typed into it also fires a
    // tool shortcut. Asked here, once, for the whole interface: every text
    // field is drawn by the time this runs, and a per-module version only ever
    // covers the fields that module knows about — which is how the settings
    // dialog's search box came to be the one nobody had.
    //
    // `text_edit_focused` rather than `egui_wants_keyboard_input`: the latter
    // is true for anything focusable, so tabbing onto a button would leave the
    // canvas deaf to every shortcut until the focus was dropped again.
    shortcuts::set_typing(root.ctx().text_edit_focused());

    UiOutput {
        actions,
        canvas_rect,
    }
}

/// Length of one dash and the gap after it, in points.
const ANT_DASH: f32 = 4.0;

/// How fast the dashes travel along the outline, in points per second.
///
/// One full dash-and-gap period a second. Faster reads as a shimmer rather than
/// a direction; slower and it is hard to be sure the line is moving at all,
/// which is the whole point of the animation.
const ANT_SPEED: f32 = 2.0 * ANT_DASH;

/// How often a frame is asked for while a selection is on screen.
///
/// **This is the cost of the animation and it is the only thing paid for it.**
/// Marching ants at the display's rate would mean a frame every 16 ms for as
/// long as a document is open with something selected — the fifth of a core
/// `render`'s `repaint_at` exists to stop being spent on a picture nobody is
/// touching. Sixteen frames a second is a sixth of that: below about ten the
/// dashes visibly hop rather than slide, and above about twenty nothing is
/// gained that anybody can see in a four-point dash. At this rate the pattern
/// advances half a point per frame, which reads as movement.
///
/// Asked for only where a selection or a gesture actually exists, so a document
/// with nothing selected is exactly as idle as it was before.
const ANT_FRAME_MS: u64 = 60;

/// The selection, and the outline being drawn if one is.
///
/// Drawn whatever tool is in hand, because it is how the artist knows their
/// painting is being clipped.
///
/// Two passes, dark then light, so the outline reads over both a white canvas
/// and a black one. Neither colour is a literal: `backdrop` and `accent` are
/// each dark in one theme and light in the other, which is what makes the pair
/// work on any artwork. **Only the accent dashes move.** The dark line under
/// them stays solid, so the pair still reads on any artwork at every instant of
/// the animation rather than only when a dash happens to be over a dark pixel.
fn selection_outline(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, rect: Rect) {
    if ed.selection.is_none() && ed.selection_draft.is_none() {
        return;
    }

    // The pivot from *this* frame's canvas rect rather than `Editor::
    // canvas_pivot`, which is written after this runs and is therefore last
    // frame's. It is the same number the composite pass will be given, so the
    // outline and the pixels it describes cannot be a frame apart while the
    // panels are being dragged.
    let scale = ed.pixels_per_point.max(1e-3);
    let pivot = glam::Vec2::new(rect.center().x, rect.center().y) * scale;
    let camera = ed.camera;
    let to_screen = |doc: glam::Vec2| {
        let s = camera.doc_to_screen(doc, pivot);
        pos2(s.x / scale, s.y / scale)
    };

    // egui's own clock, so the ants keep step with the frames actually painted
    // rather than with how many of them there were. A dropped frame slides the
    // pattern further; it does not make it fall behind.
    //
    // The offset counts *backwards* through the period so it stays positive:
    // `dashes_from_line` starts its walk at `dash_offset` along the path, so a
    // negative one would place the first dash before the path begins and draw
    // it hanging off the end.
    //
    // Wrapped in f64 and only then narrowed: egui's clock counts from when the
    // application started, and an f32 holding a day's worth of seconds has
    // steps coarser than the dash is long — the ants would end a long session
    // hopping between two positions.
    let period = f64::from(2.0 * ANT_DASH);
    let travelled = ui.input(|i| i.time) * f64::from(ANT_SPEED);
    let phase = (period - travelled.rem_euclid(period)) as f32;
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(ANT_FRAME_MS));

    // Clipped to the canvas region: a selection scrolled under a panel must
    // not draw its outline across it.
    let painter = ui.painter().with_clip_rect(rect);
    // Field by field, so the buffers can be borrowed while the selection and
    // the draft are read. They are the editor's for the reason given there:
    // this path now runs several times a second for as long as the document is
    // open, so anything it allocates it allocates for ever.
    let Editor {
        selection,
        selection_draft,
        selection_outline,
        selection_screen: screen,
        selection_dashes: dashes,
        ..
    } = ed;
    let mut draw_ring = |ring: &[glam::Vec2], closed: bool| {
        if ring.len() < 2 {
            return;
        }
        screen.clear();
        screen.extend(ring.iter().copied().map(to_screen));
        if closed {
            screen.push(screen[0]);
        }
        // Segment by segment rather than one `Shape::line`, which would want
        // the points by value and so a copy of them per ring per frame.
        for pair in screen.windows(2) {
            painter.line_segment([pair[0], pair[1]], Stroke::new(1.0, p.backdrop));
        }
        dashes.clear();
        egui::Shape::dashed_line_many_with_offset(
            screen,
            Stroke::new(1.0, p.accent),
            &[ANT_DASH],
            &[ANT_DASH],
            phase,
            dashes,
        );
        painter.extend(dashes.drain(..));
    };

    if let Some(selection) = selection.as_ref() {
        for ring in selection.rings() {
            draw_ring(ring, true);
        }
    }
    if let Some(draft) = selection_draft.as_ref() {
        // Into the editor's own buffer rather than a fresh one: this is the
        // one part of the selection path that runs every frame.
        draft.outline_into(selection_outline);
        // Only the rectangle is closed while it is being drawn: its four
        // corners *are* the shape. A lasso mid-drag and a polygon two clicks in
        // are paths, and drawing the edge back to the start would promise a
        // shape the next moment is going to change.
        draw_ring(selection_outline, draft.mode() == SelectionMode::Rectangle);
    }
}

/// How far outside the box's right-hand edge the rotation mark sits, in points:
/// where the dotted leader starts, and where the icon's near edge begins.
///
/// Clear of the edge handle's own 4-point disc and of the tolerance a press is
/// tested against, so the mark never sits on top of a handle it is not.
const ROTATE_LEADER: (f32, f32) = (9.0, 20.0);

/// Side of the rotation mark, in points. The same 18 the interface's other bare
/// icons are drawn at.
const ROTATE_MARK: f32 = 18.0;

/// Side of a flip button, and the gap between the pair and the top of the box.
const FLIP_BUTTON: f32 = 22.0;
const FLIP_GAP: f32 = 12.0;

/// The box round a floating transform, and the controls that act on it.
///
/// Every one of these handles does something: the four corners scale both axes,
/// the four edges scale one, and anywhere outside turns the box.
/// `Transform::grab` is what decides which, so what is drawn here and what the
/// pointer takes hold of are the same positions read out of the same function —
/// a handle painted somewhere the hit test did not agree with would be the
/// worst kind of control that lies. The rotation mark holds to the same rule:
/// it is not a button, it is a *label* for the gesture, and it is placed off
/// `handle_at`'s own answers so it cannot end up somewhere a press would mean
/// something else.
///
/// A solid line, not the selection's dashes. The two are often on screen
/// together and they mean different things: the dashes are where an edit may
/// land, and this is a picture that has not been put down yet.
fn transform_box(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, rect: Rect) {
    ed.transform_buttons = [None, None];
    let Some(float) = ed.float else {
        return;
    };

    // This frame's canvas rect, for the reason `selection_outline` gives:
    // `Editor::canvas_pivot` is written after this runs and is a frame behind
    // while the panels are being dragged.
    let scale = ed.pixels_per_point.max(1e-3);
    let pivot = glam::Vec2::new(rect.center().x, rect.center().y) * scale;
    let camera = ed.camera;
    let to_screen = |doc: glam::Vec2| {
        let s = camera.doc_to_screen(doc, pivot);
        pos2(s.x / scale, s.y / scale)
    };

    let painter = ui.painter().with_clip_rect(rect);
    let quad = float.xf.quad();
    let corners: Vec<egui::Pos2> = quad.iter().copied().map(to_screen).collect();
    for i in 0..4 {
        // Two passes, dark then light, so the box reads over both a white
        // canvas and a black one — the same trick the selection outline uses,
        // and neither colour is a literal.
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        painter.line_segment([a, b], Stroke::new(2.0, p.backdrop));
        painter.line_segment([a, b], Stroke::new(1.0, p.accent));
    }

    for handle in umber_core::Handle::BOX {
        let at = to_screen(float.xf.handle_at(handle));
        painter.circle_filled(at, 4.0, p.backdrop);
        painter.circle_filled(at, 3.0, p.accent);
    }

    rotate_mark(&painter, p, &float.xf, &to_screen);
    flip_buttons(ui, p, ed, rect, &corners);
}

/// The rotation affordance: a small mark just outside the box's right-hand
/// edge, joined to it by a dotted leader.
///
/// Instructive rather than interactive, and it has to be — `Handle::Rotate` is
/// reached by pressing *anywhere* outside the box, so a button here would only
/// be a smaller version of a target that is already the whole canvas. What the
/// mark buys is that the gesture is discoverable at all, which is exactly what
/// the ring-outside-a-corner it replaced never was.
///
/// Its position comes out of `handle_at`, not out of the screen rectangle the
/// box happens to occupy: the outward direction is the box's own local +x,
/// which is the middle-right handle minus the middle-left one. So it turns with
/// the box, and it swaps sides when the picture is flipped — which is right,
/// because it marks the box's right-hand side rather than the screen's.
fn rotate_mark(
    painter: &egui::Painter,
    p: &Palette,
    xf: &umber_core::Transform,
    to_screen: &impl Fn(glam::Vec2) -> egui::Pos2,
) {
    use umber_core::Handle::Scale;
    let edge = to_screen(xf.handle_at(Scale { local: (1, 0) }));
    let inward = to_screen(xf.handle_at(Scale { local: (-1, 0) }));
    let away = edge - inward;
    // A box scaled to nothing on this axis has no outward direction to speak
    // of. Nothing is drawn rather than something placed by a normalised zero.
    if away.length_sq() < 1.0 {
        return;
    }
    let away = away.normalized();

    let (from, to) = ROTATE_LEADER;
    painter.extend(egui::Shape::dotted_line(
        &[edge + away * from, edge + away * to],
        p.accent,
        3.0,
        0.8,
    ));
    let centre = edge + away * (to + ROTATE_MARK * 0.5);
    let at = Rect::from_center_size(centre, vec2(ROTATE_MARK, ROTATE_MARK));
    // Dark under light, as the box's own outline is: the mark lies over the
    // artwork and neither colour can be assumed to read against it.
    icons::draw(painter, at.expand(1.0), Icon::Rotate, p.backdrop);
    icons::draw(painter, at, Icon::Rotate, p.accent);
}

/// The two flip buttons, above the box.
///
/// Real buttons, unlike the rotation mark: negating a scale is not a drag and
/// there is no gesture for it to label. They therefore sit *over the canvas*
/// where a press would otherwise start something, so their rectangles go into
/// `Editor::transform_buttons` — the same answer `Editor::scroll_bars` is to
/// the same problem, and for the same reason: these are painted into egui's
/// background layer inside the canvas region, where neither `pointer_over_
/// canvas` nor `app.rs`'s `layer_id_at` check would otherwise see them.
///
/// Placed above the box's screen bounding rectangle rather than on its top
/// edge. They are buttons, so their hit rectangles are axis-aligned whatever
/// the box is doing; a pair that turned with the box would have targets that
/// disagreed with the marks drawn in them.
fn flip_buttons(
    ui: &mut egui::Ui,
    p: &Palette,
    ed: &mut Editor,
    rect: Rect,
    corners: &[egui::Pos2],
) {
    let bounds = Rect::from_points(corners);
    let gap = 4.0;
    let width = FLIP_BUTTON * 2.0 + gap;
    let top = bounds.top() - FLIP_GAP - FLIP_BUTTON;
    let strip = Rect::from_min_size(
        pos2(bounds.center().x - width * 0.5, top),
        vec2(width, FLIP_BUTTON),
    );
    // A box dragged up under the strips takes its buttons off the top of the
    // canvas region with it. Drawing them clipped would leave live targets
    // nobody can see, so they are simply not offered — the box can be dragged
    // back down, and Enter still puts it down from anywhere.
    if !rect.contains_rect(strip) {
        return;
    }

    for (i, (icon, tip, flip)) in [
        (
            Icon::FlipHorizontal,
            "Mirror the floating picture left to right",
            true,
        ),
        (
            Icon::FlipVertical,
            "Mirror the floating picture top to bottom",
            false,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let at = Rect::from_min_size(
            pos2(strip.left() + i as f32 * (FLIP_BUTTON + gap), strip.top()),
            egui::Vec2::splat(FLIP_BUTTON),
        );
        ed.transform_buttons[i] = Some(at);
        let response = ui.interact(at, ui.id().with(("float-flip", i)), Sense::click());
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(
            at,
            metrics::RADIUS,
            if response.hovered() {
                p.control_hover
            } else {
                p.control
            },
        );
        painter.rect_stroke(
            at,
            metrics::RADIUS,
            Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        icons::draw(
            &painter,
            at.shrink(3.0),
            icon,
            if response.hovered() {
                p.text_strong
            } else {
                p.text
            },
        );
        if response.on_hover_text(tip).clicked()
            && let Some(float) = ed.float.as_mut()
        {
            if flip {
                float.xf.flip_x();
            } else {
                float.xf.flip_y();
            }
        }
    }
}

/// The canvas scrollbars, along the bottom and the right of the document
/// region — the right being the left edge of whatever is docked there.
///
/// Drawn only where the document actually runs off the view, on the axis it
/// runs off. That covers both "larger than the window" and "small enough to
/// fit, but pushed under a panel", which are the same complaint: part of the
/// picture is somewhere the artist cannot see it.
///
/// The geometry is [`ScrollSpan`]'s, in `umber-core`, so what the thumb says
/// and where the camera is cannot drift apart — the same division `dock.rs` and
/// `panels.rs` keep.
fn canvas_scrollbars(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, rect: Rect) {
    // The viewport in *document* units, so the spans are worked out from the
    // region actually being laid out this frame rather than from last frame's
    // `canvas_size`.
    let scale = ed.pixels_per_point.max(1e-3);
    let doc = ed.doc.size_vec2();
    let zoom = ed.camera.zoom;
    let across = ScrollSpan::new(doc.x, rect.width() * scale, zoom, ed.camera.center.x);
    let down = ScrollSpan::new(doc.y, rect.height() * scale, zoom, ed.camera.center.y);

    let (show_x, show_y) = (across.overflows(), down.overflows());
    ed.scroll_bars = [None, None];
    if !show_x && !show_y {
        return;
    }

    // Neither bar runs under the other: a thumb sliding into the corner where
    // they cross would be under the one on top of it for its last few pixels.
    let bar = metrics::SCROLLBAR;
    let corner_x = rect.right() - if show_y { bar } else { 0.0 };
    let corner_y = rect.bottom() - if show_x { bar } else { 0.0 };

    if show_y {
        let at = Rect::from_min_max(
            pos2(rect.right() - bar, rect.top()),
            pos2(rect.right(), corner_y),
        );
        ed.scroll_bars[1] = Some(at);
        if let Some(by) = widgets::canvas_scrollbar(ui, p, at, down, true) {
            ed.camera.center.y += by;
        }
    }
    if show_x {
        let at = Rect::from_min_max(
            pos2(rect.left(), rect.bottom() - bar),
            pos2(corner_x, rect.bottom()),
        );
        ed.scroll_bars[0] = Some(at);
        if let Some(by) = widgets::canvas_scrollbar(ui, p, at, across, false) {
            ed.camera.center.x += by;
        }
    }
}

/// The circle the Alt-held resize draws, showing the size the brush has been
/// dragged to.
///
/// Centred on where the pointer was when Alt went down, not on the pointer:
/// the drag *is* the size, so a ring that ran along with the hand would be
/// something to chase rather than something to measure against — and the
/// canvas underneath the anchor is what the artist is judging the size against.
///
/// **Document space scaled to the screen**, because that is what a size is:
/// [`Brush::size`] is a diameter in document pixels, so a 50-pixel brush at
/// 200% draws a hundred pixels across — the width of the mark it will actually
/// leave, at any zoom.
///
/// A circle rather than the ellipse an elliptical brush stamps. `size` is the
/// *long* axis and is the one number this gesture moves; the dab's own outline
/// turns with `dab_angle`, rolls with the jitter, narrows under pressure and
/// scatters off the line, so an ellipse here would be a picture of one dab
/// rather than of the number under the hand.
fn brush_size_preview(ui: &egui::Ui, p: &Palette, ed: &Editor) {
    let Some(resize) = ed.brush_resize else {
        return;
    };
    let at = ed.to_points(resize.origin);
    // Document pixels → physical pixels is the zoom; physical → points is
    // egui's, and the same conversion every other canvas boundary makes.
    let radius = ed.brush.size * 0.5 * ed.camera.zoom / ed.pixels_per_point.max(1e-3);
    let painter = ui.painter();
    painter.circle_stroke(at, radius, Stroke::new(1.0, p.accent));
    // The anchor the drag is measured from — and the only thing on screen at
    // all when a one-pixel brush is being sized at a low zoom.
    painter.circle_filled(at, 1.0, p.accent);
}

/// The pen's own pointer: a small grey dot where the nib is, instead of the
/// arrow.
///
/// A pen that is being aimed at a canvas is a drawing instrument, and an arrow
/// pointing up and to the left says nothing about where the mark will start.
/// Only for a pen — [`Editor::pen_pointer`] is what the last pointer event was
/// driven by, not a preference — so a mouse keeps the cursor the rest of the
/// desktop gave it.
///
/// **`CursorIcon::None` rather than winit's `set_cursor_visible`**, and the
/// difference is which way the state runs. egui's cursor is re-derived from
/// what the interface asked for on *every* frame, so this hides the arrow only
/// for as long as this function keeps asking: the pen goes away, the pointer
/// crosses onto a panel, another window takes focus, a widget claims a cursor
/// of its own — and the arrow is back with nothing having to remember to put
/// it back. `set_cursor_visible(false)` is the opposite, a latch whose failure
/// mode is a window with no pointer in it and no way to say so.
///
/// The dot is in points, which is egui's unit and already scaled — so it is
/// the same size on the screen whatever the display's density, exactly as the
/// panels and the type are.
fn pen_cursor(ui: &egui::Ui, p: &Palette, ed: &Editor) {
    // Over a panel, a menu or a scrollbar the ordinary cursor is the right
    // one: those are things to point at, and a workspace whose pointer
    // vanished at the edge of the canvas would be unusable.
    if !ed.pen_pointer || !ed.pointer_over_canvas(ed.cursor) {
        return;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
    // `text_dim` is the palette's recessive ink, and it is the one token that
    // is a mid-grey in *both* themes — the surfaces invert between Graphite and
    // Paper and most of the ink with them, so anything stronger would be black
    // on one and white on the other, over artwork that is neither.
    ui.painter()
        .circle_filled(ed.to_points(ed.cursor), metrics::PEN_DOT, p.text_dim);
}

fn menu_bar(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    ui.horizontal_centered(|ui| {
        let (mark, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
        ui.painter().rect_filled(mark, 3.0, p.accent);
        ui.add_space(6.0);

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New…").clicked() {
                    let doc = ed.doc;
                    ed.canvas_form.open(crate::canvasdlg::Dialog::New, doc);
                    ui.close();
                }
                if ui.button("Open…").clicked() {
                    actions.open_file = true;
                    ui.close();
                }
                if ui
                    .button("Canvas settings…")
                    .on_hover_text("Size, background and resolution of the document in front.")
                    .clicked()
                {
                    let doc = ed.doc;
                    ed.canvas_form.open(crate::canvasdlg::Dialog::Settings, doc);
                    ui.close();
                }
                ui.separator();
                if menu_item(ui, "Save", Action::Save).clicked() {
                    actions.save = true;
                    ui.close();
                }
                if menu_item(ui, "Save as…", Action::SaveAs).clicked() {
                    actions.save_as = true;
                    ui.close();
                }
                ui.separator();
                // Only offered while there is another document to fall back to;
                // Umber has nowhere to go with nothing open.
                if ui
                    .add_enabled(ed.session.len() > 1, egui::Button::new("Close document"))
                    .on_disabled_hover_text(
                        "This is the only document open, and Umber has nothing to \
                         show in its place.",
                    )
                    .clicked()
                {
                    let index = ed.session.active_index();
                    if ed.session.active_tab().modified {
                        ed.ui.close_prompt = Some(index);
                    } else {
                        actions.close_tab = Some(index);
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("Clear layer").clicked() {
                    actions.clear = true;
                    ui.close();
                }
                if ui
                    .button("Export flat PNG…")
                    .on_hover_text(
                        "One flattened image, for showing people. Save keeps the layers.",
                    )
                    .clicked()
                {
                    actions.export = true;
                    ui.close();
                }
            });

            ui.menu_button("Edit", |ui| {
                // The history covers painting only, which is why these can be
                // dead on a document that plainly has layers in it.
                if ui
                    .add_enabled(ed.history.can_undo(), egui::Button::new("Undo"))
                    .on_disabled_hover_text("Nothing painted on this document to undo.")
                    .clicked()
                {
                    actions.undo = true;
                    ui.close();
                }
                if ui
                    .add_enabled(ed.history.can_redo(), egui::Button::new("Redo"))
                    .on_disabled_hover_text("Nothing undone to put back.")
                    .clicked()
                {
                    actions.redo = true;
                    ui.close();
                }
                ui.separator();
                // Under Edit rather than Window, which is where Windows and
                // most Linux desktops put preferences. Window is about the
                // arrangement of the workspace; these are settings for the
                // application.
                if ui.button("Settings…").clicked() {
                    ed.ui.settings_open = true;
                    ui.close();
                }
            });

            ui.menu_button("View", |ui| {
                if ui.button("Fit to window").clicked() {
                    actions.fit_view = true;
                    ui.close();
                }
                if ui.button("Actual size").clicked() {
                    actions.reset_zoom = true;
                    ui.close();
                }
            });

            ui.menu_button("Window", |ui| {
                panels::window_menu(ui, ed);
            });

            ui.menu_button("Help", |ui| {
                if ui.button("Check for updates…").clicked() {
                    ed.updates.check();
                    // Opened alongside, because that is where the answer
                    // appears: a check whose result had nowhere to land would
                    // be a menu item that does nothing visible.
                    ed.ui.about_open = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("About Umber").clicked() {
                    ed.ui.about_open = true;
                    ui.close();
                }
            });
        });

        // Where the menus finished, before the right-hand group moves the
        // cursor to the other end of the bar.
        let menus_right = ui.cursor().min.x;

        let right = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, p, Icon::Gear, true, "Settings") {
                    ed.ui.settings_open = true;
                }
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("{:.0} fps", ed.average_fps()))
                        .size(text::TINY)
                        .color(p.text_dim),
                );
            })
            .response
            .rect;

        // Document title, centred on the band left over between the menus and
        // the frame counter rather than on the whole bar. Centring on the bar
        // reads better only while the bar is wide: below about 900 points the
        // title crossed the menu labels, and below 600 it sat under the gear.
        // Drawn after both, so the band is measured rather than guessed.
        let band = Rect::from_min_max(
            pos2(menus_right + 12.0, ui.max_rect().top()),
            pos2(right.left() - 12.0, ui.max_rect().bottom()),
        );
        if band.width() >= 40.0 {
            let title = format!(
                "{} — {} × {}",
                ed.session.active_title(),
                ed.doc.size.x,
                ed.doc.size.y
            );
            let painter = ui.painter();
            painter.text(
                band.center(),
                Align2::CENTER_CENTER,
                widgets::elide(painter, &title, text::CONTROL, band.width()),
                FontId::proportional(text::CONTROL),
                p.text_dim,
            );
        }
    });
}

/// A menu entry that shows the chord currently bound to it.
///
/// Read out of the live binding table rather than typed next to the label, so a
/// rebind in the settings dialog reaches the menu as well — and an action left
/// unbound shows no chord instead of a stale one. `published` clones the table,
/// which is only ever paid while a menu is open.
fn menu_item(ui: &mut egui::Ui, label: &str, action: shortcuts::Action) -> egui::Response {
    let chord = shortcuts::published()
        .iter()
        .find(|b| b.action == action)
        .map(|b| b.chord().display())
        .unwrap_or_default();
    ui.add(egui::Button::new(label).shortcut_text(chord))
}

/// What each optional group on the tool options strip costs, in points.
///
/// The strip is a single unwrapped row, so a window narrow enough to overrun it
/// does not reflow — the controls simply carry on past the right edge. These
/// budgets decide which groups are drawn, in reverse order of how constantly a
/// painter reaches for them: the stabiliser readout goes first, then opacity,
/// then size.
///
/// They are the design's own widths (a 90 point rail, a 24 point readout) plus
/// the labels and egui's item spacing, rather than anything measured. Measuring
/// would mean laying the strip out twice to find out whether to lay it out, and
/// these only decide *whether* a group appears, never where it lands.
///
/// There used to be a fourth: 92 points held clear at the right for an "Edit
/// brush…" link. The way into the brush editor is the pencil in the Brushes
/// panel *header* now, so the reserve went with the link and every group here
/// is measured against the whole strip again.
mod strip_budget {
    pub const SIZE: f32 = 160.0;
    pub const OPACITY: f32 = 185.0;
    pub const STABILISER: f32 = 110.0;
    /// The line naming the modifiers that add to and subtract from a selection.
    pub const COMBINE: f32 = 175.0;
}

/// How to add to and subtract from a selection, named for this platform.
///
/// A held modifier is part of a gesture rather than a command, so it is
/// deliberately not in the rebindable table — which leaves the options strip as
/// the only place the user can find out. `const` rather than `format!` with
/// `shortcuts::primary_modifier_name`, because the strip is painted every frame
/// and a string built per frame for a line that never changes is exactly what
/// the rest of this interface avoids.
const fn combine_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Hold Shift to add, Cmd to subtract."
    } else {
        "Hold Shift to add, Ctrl to subtract."
    }
}

/// The horizontal strip of settings for the current tool.
///
/// Size and opacity live here as well as further down the dock because they are
/// the two a painter reaches for constantly.
fn options_strip(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.horizontal_centered(|ui| {
        let (icon, name) = match ed.ui.tool {
            Tool::Brush => (Icon::Brush, "Brush"),
            Tool::Eraser => (Icon::Eraser, "Eraser"),
            Tool::Select => (Icon::Select, "Select"),
            Tool::Transform => (Icon::Transform, "Transform"),
            Tool::Pan => (Icon::Pan, "Pan"),
            Tool::Zoom => (Icon::Zoom, "Zoom"),
        };
        let (glyph_rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
        icons::draw(ui.painter(), glyph_rect, icon, p.accent);
        ui.label(
            egui::RichText::new(name)
                .size(text::SMALL)
                .color(p.text_strong)
                .strong(),
        );

        divider(ui, p);

        if ed.ui.tool.paints() {
            let room = ui.available_width();
            if room >= strip_budget::SIZE {
                widgets::inline_slider(
                    ui,
                    p,
                    "Size",
                    &mut ed.brush.size,
                    Brush::MIN_SIZE..=400.0,
                    true,
                    |v| format!("{v:.0}"),
                );
            }
            if room >= strip_budget::SIZE + strip_budget::OPACITY {
                widgets::inline_slider(
                    ui,
                    p,
                    "Opacity",
                    &mut ed.brush.opacity,
                    0.0..=1.0,
                    false,
                    |v| format!("{:.0}", v * 100.0),
                );
            }
            if room >= strip_budget::SIZE + strip_budget::OPACITY + strip_budget::STABILISER {
                divider(ui, p);

                // Read-only, unlike the design's, which has a chevron and opens
                // a menu. Stabilisation is set in the brush editor; the tooltip
                // says so rather than leaving a pill that looks like a control
                // and answers to nothing.
                widgets::chip(
                    ui,
                    p,
                    "Stabiliser",
                    &format!("{:.0}", ed.brush.stabilization * 100.0),
                    "How much this brush smooths the stroke. Change it in the \
                     brush editor — the pencil in the Brushes panel header — \
                     on the Tip tab.",
                );
            }
        } else if ed.ui.tool == Tool::Transform {
            // What the tool actually does, said plainly, because none of it is
            // discoverable from a box with dots on it. Two sentences rather
            // than a row of controls: there is nothing here to set — the whole
            // gesture is the pointer's.
            let hint = if ed.float.is_some() {
                "Drag inside the box to move it, a handle to scale, or \
                 anywhere outside to turn it. Shift keeps the proportions. \
                 Enter or a click outside puts it down, Escape throws the \
                 move away."
            } else if ed.selection.is_some() {
                "Press inside the selection to pick it up."
            } else {
                "Press on the canvas to pick the whole layer up. Select \
                 something first to move only part of it."
            };
            ui.label(
                egui::RichText::new(hint)
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
        } else if ed.ui.tool == Tool::Select {
            selection_mode_switch(ui, p, ed);
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(ed.ui.selection_mode.hint())
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
            // Dropped first when the window is narrow: the gesture the mode
            // needs is the line somebody is stuck without, and this one is
            // about a gesture they have not reached for yet.
            if ui.available_width() >= strip_budget::COMBINE {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(combine_hint())
                        .size(text::SMALL)
                        .color(p.text_dim),
                );
            }
            if ed.selection.is_some() {
                divider(ui, p);
                // Only offered where there is something to clear. A live
                // control that answers to nothing is the thing this interface
                // does not do.
                if status_link(
                    ui,
                    p,
                    &shortcuts::labelled("Deselect", Action::Deselect),
                    "Let edits reach the whole layer again.",
                ) {
                    ed.deselect();
                }
            }
        } else {
            ui.label(
                egui::RichText::new("drag on the canvas")
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
        }
    });
}

/// The selection tool's mode switch: the mode name and a chevron, opening a
/// list of the three.
///
/// The trigger is painted, like every other control the design specifies;
/// the list itself is a popup of `selectable_label`s, exactly as the Colour
/// panel's picker-mode switch is. One dropdown pattern rather than two.
fn selection_mode_switch(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    let label = ed.ui.selection_mode.label();
    let font = FontId::proportional(text::SMALL);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), p.text_dim)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(vec2(text_w + 16.0, 18.0), Sense::click());
    let colour = if response.hovered() {
        p.text_strong
    } else {
        p.text
    };
    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, p.control);
    painter.text(
        rect.left_center() + vec2(6.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        font,
        colour,
    );
    icons::draw(
        painter,
        Rect::from_min_size(rect.right_top() - vec2(12.0, 0.0), vec2(12.0, 18.0)),
        Icon::ChevronDown,
        colour,
    );

    if response.clicked() {
        ed.ui.selection_menu_open = !ed.ui.selection_menu_open;
    }
    let popup = egui::Popup::from_response(&response)
        .open(ed.ui.selection_menu_open)
        .show(|ui| {
            for mode in SelectionMode::ALL {
                if ui
                    .selectable_label(ed.ui.selection_mode == mode, mode.label())
                    .clicked()
                {
                    ed.ui.selection_mode = mode;
                    // A half-drawn outline belongs to the mode that was
                    // drawing it, and a polygon left open under the lasso
                    // would take its next click as a vertex.
                    ed.cancel_selection_draft();
                    ed.ui.selection_menu_open = false;
                }
            }
        });
    if popup.is_none() {
        ed.ui.selection_menu_open = false;
    }
}

fn divider(ui: &mut egui::Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 16.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.border);
}

/// A bare 18×18 icon that acts as a button. Shared with `panels.rs`.
///
/// A disabled one still hovers, and still shows its tooltip — matching
/// [`crate::controls::icon_button`], and for the same reason. Several callers
/// pass the *reason* it is dead as the tooltip (the brush library's `＋` hands
/// over whatever went wrong with the library file), and while the hover was
/// skipped along with the click, none of those explanations ever reached the
/// screen: what was left was a greyed mark with nothing to say for itself.
pub fn icon_button(ui: &mut egui::Ui, p: &Palette, icon: Icon, enabled: bool, tip: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        vec2(18.0, 18.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hovered = enabled && response.hovered();
    icons::draw(
        ui.painter(),
        rect,
        icon,
        if !enabled {
            p.text_dim.gamma_multiply(0.4)
        } else if hovered {
            p.text_strong
        } else {
            p.text_dim
        },
    );
    response.on_hover_text(tip).clicked()
}

/// An icon followed by a label, behaving as one clickable unit.
fn text_icon_link(ui: &mut egui::Ui, p: &Palette, icon: Icon, label: &str) -> egui::Response {
    let font = FontId::proportional(text::SMALL);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), p.text_dim)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(vec2(text_w + 20.0, 18.0), Sense::click());
    let colour = if response.hovered() {
        p.text_strong
    } else {
        p.text_dim
    };
    icons::draw(
        ui.painter(),
        Rect::from_min_size(rect.left_top(), vec2(16.0, 18.0)),
        icon,
        colour,
    );
    ui.painter().text(
        rect.right_center(),
        Align2::RIGHT_CENTER,
        label,
        font,
        colour,
    );
    response
}

fn status_bar(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    ui.horizontal_centered(|ui| {
        // The right-hand group goes on first, even though it reads last. It is
        // the half that must never be lost — the zoom, and the two links that
        // put the view back — whereas the left is a running commentary. Placing
        // it first is what lets the left side be measured against what is
        // actually left, instead of overrunning it on a narrow window.
        let right = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} × {} · {:.0}% · {} layer{} · undo {:.0} MB",
                        ed.doc.size.x,
                        ed.doc.size.y,
                        ed.camera.zoom * 100.0,
                        ed.layers.len(),
                        if ed.layers.len() == 1 { "" } else { "s" },
                        ed.history.used_bytes() as f32 / (1024.0 * 1024.0),
                    ))
                    .size(text::TINY)
                    .color(p.text_dim),
                );

                // Two words with no icon and no border, so what they do is
                // worth spelling out — with the key that also does it.
                ui.add_space(8.0);
                if status_link(
                    ui,
                    p,
                    "100%",
                    &shortcuts::labelled("Show the document at actual size", Action::ActualSize),
                ) {
                    actions.reset_zoom = true;
                }
                if status_link(
                    ui,
                    p,
                    "Fit",
                    &shortcuts::labelled("Fit the whole document in the window", Action::FitView),
                ) {
                    actions.fit_view = true;
                }
            })
            .response
            .rect;

        // The design swaps the whole status line while the layout is being
        // edited. Saying so here is what makes a paused canvas legible rather
        // than a bug.
        let (line, ink) = if ed.layout.edit_mode() {
            (
                "layout edit — nothing you draw changes; panels are the only \
                 thing that moves"
                    .to_string(),
                p.accent,
            )
        } else {
            // The file the document lives in, named in full: the tab strip only
            // has room for the file name, and knowing *which* sketch.ora is
            // being painted on is exactly what a status bar is for. A document
            // with no file yet says so rather than pretending a path.
            let tab = ed.session.active_tab();
            let where_it_lives = match &tab.path {
                Some(path) => path.display().to_string(),
                None => format!("{} · not saved yet", tab.title),
            };
            (
                format!(
                    "{where_it_lives}{} · panels locked — Window, Customise layout",
                    if tab.modified { " · unsaved" } else { "" },
                ),
                p.text_dim,
            )
        };

        // Painted rather than laid out, so it can be cut to the room the right
        // half left. An `egui::Label` in a horizontal layout extends instead of
        // wrapping, and would have run straight under the zoom readout.
        let bar = ui.max_rect();
        let room = right.left() - 12.0 - bar.left();
        if room > 24.0 {
            let painter = ui.painter();
            painter.text(
                bar.left_center(),
                Align2::LEFT_CENTER,
                widgets::elide(painter, &line, text::TINY, room),
                FontId::proportional(text::TINY),
                ink,
            );
        }
    });
}

fn status_link(ui: &mut egui::Ui, p: &Palette, label: &str, tip: &str) -> bool {
    ui.add(
        egui::Label::new(
            egui::RichText::new(label)
                .size(text::TINY)
                .color(p.text_dim),
        )
        .sense(Sense::click()),
    )
    .on_hover_text(tip)
    .clicked()
}

/// The brush editor, matching the design's dialog.
///
/// Holds every brush parameter that is not on the options strip, so the strip
/// can stay short. Edits apply live — there is no OK or Cancel, because a paint
/// app should let you see a change as you make it.
fn brush_editor(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    if !ed.ui.brush_editor_open {
        return;
    }

    let name = ed
        .active_preset
        .and_then(|i| ed.presets.get(i))
        .map(|preset| preset.name.clone())
        .unwrap_or_else(|| "Brush".to_string());

    let response = egui::Modal::new(egui::Id::new("brush-editor"))
        .frame(
            Frame::NONE
                .fill(p.popover)
                .stroke(Stroke::new(1.0, p.popover_border))
                .corner_radius(8)
                .inner_margin(Margin::same(18)),
        )
        .show(root.ctx(), |ui| {
            // Wider than the other modals because the Tip section is the
            // design's two-column grid and the Dynamics section is three curves
            // side by side. At 430 px either would have to stack, and a brush
            // editor you have to scroll is one you stop reaching for.
            ui.set_width(metrics::BRUSH_EDITOR_WIDTH);

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("Edit brush — {name}"))
                        .size(text::CONTROL)
                        .color(p.text_strong)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, p, Icon::Close, true, "Close") {
                        ed.ui.brush_editor_open = false;
                    }
                });
            });

            ui.add_space(10.0);
            widgets::segmented(
                ui,
                p,
                &mut ed.ui.brush_tab,
                &[
                    (BrushTab::Tip, "Tip"),
                    (BrushTab::Dynamics, "Dynamics"),
                    (BrushTab::Inputs, "Inputs"),
                    (BrushTab::Scatter, "Scatter"),
                    (BrushTab::Texture, "Texture"),
                    (BrushTab::Blending, "Blending"),
                ],
            );
            ui.add_space(12.0);

            match ed.ui.brush_tab {
                BrushTab::Tip => brush_editor_tip(ui, p, ed),
                BrushTab::Dynamics => brush_editor_dynamics(ui, p, ed),
                BrushTab::Inputs => brush_editor_inputs(ui, p, ed),
                BrushTab::Scatter => brush_editor_scatter(ui, p, ed),
                BrushTab::Texture => brush_editor_texture(ui, p, ed),
                BrushTab::Blending => brush_editor_blending(ui, p, ed),
            }

            // The design's footer: name what you have made, or write it back
            // over the brush you started from.
            crate::brushlib::save_row(ui, p, ed);
        });

    // Clicking the backdrop or pressing Escape dismisses it.
    if response.should_close() {
        ed.ui.brush_editor_open = false;
    }
}

/// Whether turning the dab would change anything.
///
/// An ellipse has an angle; so does a stamp, whatever its roundness, because a
/// bitmap is not rotationally symmetric. [`Brush::dab_has_angle`] can only
/// answer the first half — `BrushPreset::tip` is a name the editor resolves —
/// so the two are combined here rather than in the engine.
fn has_angle(ed: &Editor) -> bool {
    ed.brush.dab_has_angle() || ed.tip.is_some()
}

/// A percentage readout, which most of these sliders share.
fn percent(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

/// A caption under a control, explaining why it is off or what it does.
fn caption(ui: &mut egui::Ui, p: &Palette, line: &str) {
    ui.label(egui::RichText::new(line).size(10.0).color(p.text_dim));
}

/// The design's Tip section: a two-column grid of the dab's own properties.
fn brush_editor_tip(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;
    let stamped = bitmap_tip_row(ui, p, ed);
    ui.columns(2, |c| {
        widgets::slider_row(
            &mut c[0],
            p,
            "Size",
            &mut ed.brush.size,
            Brush::MIN_SIZE..=400.0,
            true,
            |v| format!("{v:.0} px"),
        );
        // A tip *replaces* the procedural falloff rather than being multiplied
        // into it, so hardness has nothing left to shape. Drawn dead with the
        // reason underneath rather than removed: a control that disappears when
        // you pick a brush reads as a bug.
        let column = &mut c[1];
        column.scope(|ui| {
            if stamped {
                ui.disable();
            }
            widgets::slider_row(
                ui,
                p,
                "Hardness",
                &mut ed.brush.hardness,
                0.0..=1.0,
                false,
                percent,
            );
        });
        if stamped {
            caption(column, p, "The stamp decides this brush's edge.");
        }
    });
    ui.columns(2, |c| {
        widgets::slider_row(
            &mut c[0],
            p,
            "Opacity",
            &mut ed.brush.opacity,
            0.0..=1.0,
            false,
            percent,
        );
        widgets::slider_row(
            &mut c[1],
            p,
            "Spacing",
            &mut ed.brush.spacing,
            0.01..=0.5,
            true,
            percent,
        );
    });
    ui.columns(2, |c| {
        // Roundness rather than the engine's aspect ratio, because that is the
        // word the design uses and the word every other paint application uses.
        // `dab_ratio` is long-over-short, so the two are reciprocals; 5% is the
        // floor because a 20:1 chisel is already thinner than any real bristle.
        let mut roundness = 1.0 / ed.brush.dab_ratio.max(1.0);
        if widgets::slider_row(
            &mut c[0],
            p,
            "Roundness",
            &mut roundness,
            0.05..=1.0,
            false,
            percent,
        ) {
            ed.brush.dab_ratio = 1.0 / roundness.clamp(0.05, 1.0);
        }
        widgets::slider_row(
            &mut c[1],
            p,
            "Airbrush rate",
            &mut ed.brush.dabs_per_second,
            0.0..=100.0,
            false,
            |v| {
                if v <= 0.0 {
                    "off".to_string()
                } else {
                    format!("{v:.0}/s")
                }
            },
        );
    });
    ui.columns(2, |c| {
        // A circle has no angle. Rather than let the slider lie, it is disabled
        // until the dab is elliptical and says why — but a *stamp* has an angle
        // whatever its roundness, because a bitmap is not rotationally
        // symmetric. `Brush` cannot answer that: the tip is a name it resolves
        // through the library, so the question is the editor's to ask.
        let round = !has_angle(ed);
        c[0].scope(|ui| {
            if round {
                ui.disable();
            }
            widgets::slider_row(
                ui,
                p,
                "Angle",
                &mut ed.brush.dab_angle,
                0.0..=359.0,
                false,
                |v| format!("{v:.0}°"),
            );
        });
        widgets::slider_row(
            &mut c[1],
            p,
            "Stabilisation",
            &mut ed.brush.stabilization,
            0.0..=0.95,
            false,
            percent,
        );
    });

    ui.scope(|ui| {
        if !has_angle(ed) {
            ui.disable();
        }
        widgets::toggle_row(
            ui,
            p,
            "Angle follows the stroke",
            &mut ed.brush.dab_angle_follows_stroke,
        );
    });
    caption(
        ui,
        p,
        if has_angle(ed) {
            "A rake keeps its bristles across the line of travel; a broad nib \
             holds one angle through a curve."
        } else {
            "Angle needs an elliptical dab or a bitmap tip — lower Roundness \
             first."
        },
    );
    ui.add_space(2.0);
    caption(
        ui,
        p,
        "Airbrush rate keeps depositing paint while the pen is held still. \
         Spacing alone stops when you do.",
    );
}

/// The bitmap tip, when the brush has one. Returns whether it does.
///
/// Only drawn for a stamp brush. Almost every brush is round, and a permanent
/// row saying so would be a control that never does anything — the way in is
/// **Import brushes…** in the Brushes panel, which reads `.gbr`.
fn bitmap_tip_row(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) -> bool {
    let Some(mask) = ed.tip.clone() else {
        return false;
    };

    let mut cleared = false;
    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS)
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                tip_preview(ui, p, &mask);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("Bitmap tip")
                            .size(text::SMALL)
                            .color(p.text_strong),
                    );
                    ui.label(
                        egui::RichText::new(format!("{} × {} px", mask.width(), mask.height()))
                            .size(text::TINY)
                            .color(p.text_dim),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::controls::text_button(ui, p, "Use a round tip", false, true)
                        .on_hover_text(
                            "Paint with the procedural round dab instead. \
                             Save the brush to keep the change.",
                        )
                        .clicked()
                    {
                        cleared = true;
                    }
                });
            });
        });

    if cleared {
        ed.clear_tip();
    }
    !cleared
}

/// Widest a mask is downsampled to for the editor's 48-point thumbnail.
///
/// A stamp can be 2048 texels across, so it is box-averaged down first —
/// nearest sampling would show a sparse spatter tip as an empty square about
/// half the time.
const TIP_PREVIEW_TEXELS: u32 = 96;

fn tip_preview(ui: &mut egui::Ui, p: &Palette, mask: &Arc<TipMask>) {
    // Kept in egui's temporary store and compared by `Arc` identity, so
    // switching brush rebuilds it and holding the editor open does not. The
    // naive version uploads a texture on every one of the modal's frames.
    let id = egui::Id::new("brush-tip-preview");
    let cached: Option<(Arc<TipMask>, egui::TextureHandle)> = ui.ctx().data(|d| d.get_temp(id));
    let texture = match cached {
        Some((held, texture)) if Arc::ptr_eq(&held, mask) => texture,
        _ => {
            let texture = ui.ctx().load_texture(
                "brush-tip",
                widgets::tip_image(mask, p.text_strong, TIP_PREVIEW_TEXELS),
                egui::TextureOptions::LINEAR,
            );
            ui.ctx()
                .data_mut(|d| d.insert_temp(id, (Arc::clone(mask), texture.clone())));
            texture
        }
    };

    let (rect, _) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, p.chrome);
    painter.image(
        texture.id(),
        rect.shrink(2.0),
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

fn brush_editor_dynamics(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 10.0;

    ui.label(
        egui::RichText::new("Pressure source")
            .size(text::SMALL)
            .color(p.text_dim),
    );
    widgets::segmented(
        ui,
        p,
        &mut ed.pressure.source,
        &[
            (PressureSource::Device, "Device"),
            (PressureSource::Simulated, "Speed"),
            (PressureSource::Constant, "Off"),
        ],
    );
    if ed.pressure.source == PressureSource::Device {
        caption(
            ui,
            p,
            "Touch screens and Windows pens report real pressure. A mouse, and a \
             pen on macOS or Linux, falls back to full pressure.",
        );
    }

    ui.add_space(4.0);

    // Three curves rather than the design's two. Hardness is the most used
    // pressure dynamic in the shipped library after size and opacity — 69 of
    // its 196 brushes ask for it — and a light stroke that thins without also
    // softening does not read as a pencil.
    ui.columns(3, |c| {
        curve_column(
            &mut c[0],
            p,
            "Pressure → size",
            "size",
            &mut ed.brush.pressure_size,
            &mut ed.brush.size_curve,
            Some(("Min size", &mut ed.brush.min_size_ratio)),
        );
        curve_column(
            &mut c[1],
            p,
            "Pressure → opacity",
            "opacity",
            &mut ed.brush.pressure_opacity,
            &mut ed.brush.opacity_curve,
            None,
        );
        curve_column(
            &mut c[2],
            p,
            "Pressure → hardness",
            "hardness",
            &mut ed.brush.pressure_hardness,
            &mut ed.brush.hardness_curve,
            Some(("Min hardness", &mut ed.brush.min_hardness_ratio)),
        );
    });
}

/// Everything that drives the brush and is not pressure.
///
/// A fifth section rather than a fourth column on Dynamics. Dynamics is three
/// curves that all answer "what does pressing harder do"; this is a *list* of
/// arbitrary length, and no amount of column arithmetic makes those the same
/// shape. `docs/brushes.md` records the naming.
fn brush_editor_inputs(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 8.0;

    caption(
        ui,
        p,
        "Speed, stroke position, direction and chance can each drive the brush, \
         on top of whatever pressure is doing. This is where an imported \
         MyPaint brush keeps the rest of its character.",
    );

    let count = ed.brush.modulations.len();
    ed.ui.modulation = ed.ui.modulation.min(count.saturating_sub(1));

    let mut remove = None;
    for i in 0..count {
        let entry = ed.brush.modulations.as_slice()[i];
        let selected = i == ed.ui.modulation;
        let row = Frame::NONE
            .fill(if selected {
                p.control_active
            } else {
                p.control
            })
            .stroke(Stroke::new(1.0, if selected { p.accent } else { p.border }))
            .corner_radius(6)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        egui::RichText::new(format!(
                            "{} \u{2190} {}",
                            entry.target.label(),
                            entry.input.label()
                        ))
                        .size(text::TINY)
                        .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, p, Icon::Trash, true, "Remove this input") {
                            remove = Some(i);
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "{} … {}",
                                entry.target.format(entry.low),
                                entry.target.format(entry.high)
                            ))
                            .size(text::TINY)
                            .color(p.text_dim),
                        );
                    });
                });
            });
        // The whole row selects, not just the label — a 6 px target is not one.
        if row
            .response
            .interact(Sense::click())
            .on_hover_text("Edit this input")
            .clicked()
        {
            ed.ui.modulation = i;
        }
    }
    if let Some(i) = remove {
        ed.brush.modulations.remove(i);
    }

    let full = ed.brush.modulations.is_full();
    ui.scope(|ui| {
        if full {
            ui.disable();
        }
        if text_icon_link(ui, p, Icon::Plus, "Add an input").clicked() {
            // Speed onto size is the pack's most common non-pressure mapping
            // by a wide margin, so it is the least surprising thing to land on.
            let added = ed.brush.modulations.insert(Modulation {
                target: DabTarget::Size,
                input: DabInput::Speed,
                low: 0.0,
                high: 0.0,
                curve: ResponseCurve::LINEAR,
            });
            if added {
                ed.ui.modulation = ed.brush.modulations.len() - 1;
            }
        }
    });
    if full {
        caption(ui, p, "A brush holds twelve inputs at most.");
    }

    let Some(entry) = ed.brush.modulations.get_mut(ed.ui.modulation).map(|m| *m) else {
        ui.add_space(6.0);
        caption(
            ui,
            p,
            "Nothing but pressure drives this brush. That is the fast path — no \
             per-dab evaluation and no random draws at all.",
        );
        return;
    };
    let mut edited = entry;

    ui.add_space(6.0);
    divider(ui, p);
    ui.add_space(6.0);

    ui.columns(2, |c| {
        c[0].label(
            egui::RichText::new("Drives")
                .size(text::SMALL)
                .color(p.text_dim),
        );
        egui::ComboBox::from_id_salt("mod-target")
            .selected_text(
                egui::RichText::new(edited.target.label())
                    .size(text::TINY)
                    .color(p.text),
            )
            .width(c[0].available_width())
            .show_ui(&mut c[0], |ui| {
                for target in DabTarget::ALL {
                    if ui
                        .selectable_label(target == edited.target, target.label())
                        .clicked()
                    {
                        edited.target = target;
                        // The range is stated in the target's own unit, so it
                        // means something different the moment the target
                        // changes. Clearing it is honest; carrying a 180-degree
                        // angle over onto hue is not.
                        edited.low = 0.0;
                        edited.high = 0.0;
                    }
                }
            });

        c[1].label(
            egui::RichText::new("Driven by")
                .size(text::SMALL)
                .color(p.text_dim),
        );
        egui::ComboBox::from_id_salt("mod-input")
            .selected_text(
                egui::RichText::new(edited.input.label())
                    .size(text::TINY)
                    .color(p.text),
            )
            .width(c[1].available_width())
            .show_ui(&mut c[1], |ui| {
                for input in DabInput::ALL {
                    if ui
                        .selectable_label(input == edited.input, input.label())
                        .clicked()
                    {
                        edited.input = input;
                    }
                }
            });
    });

    ui.add_space(8.0);
    let range = edited.target.range();
    let target = edited.target;
    ui.columns(2, |c| {
        widgets::slider_row(
            &mut c[0],
            p,
            "At the low end",
            &mut edited.low,
            range.clone(),
            false,
            move |v| target.format(v),
        );
        widgets::slider_row(
            &mut c[1],
            p,
            "At the high end",
            &mut edited.high,
            range,
            false,
            move |v| target.format(v),
        );
    });

    ui.add_space(8.0);
    ui.columns(2, |c| {
        c[0].label(
            egui::RichText::new("Shape")
                .size(text::SMALL)
                .color(p.text_dim),
        );
        let size = c[0].available_width().min(metrics::CURVE_PANEL);
        widgets::curve_editor(&mut c[0], p, &mut edited.curve, size);
        c[0].add_space(6.0);
        let current = edited.curve.preset_name().unwrap_or("Custom");
        egui::ComboBox::from_id_salt("mod-curve")
            .selected_text(egui::RichText::new(current).size(text::TINY).color(p.text))
            .width(size)
            .show_ui(&mut c[0], |ui| {
                for (name, preset) in ResponseCurve::PRESETS {
                    if ui
                        .selectable_label(edited.curve.preset_name() == Some(name), name)
                        .clicked()
                    {
                        edited.curve = preset;
                    }
                }
            });

        caption(&mut c[1], p, input_note(edited.input));
    });

    if let Some(slot) = ed.brush.modulations.get_mut(ed.ui.modulation) {
        *slot = edited;
    }

    // The stroke ramp is a property of the brush rather than of one entry, and
    // it means nothing at all unless something reads it — so it is drawn dead,
    // with the reason, rather than hidden or left live and inert.
    ui.add_space(8.0);
    divider(ui, p);
    ui.add_space(8.0);
    let uses_stroke = ed.brush.uses_stroke_position();
    ui.scope(|ui| {
        if !uses_stroke {
            ui.disable();
        }
        ui.columns(2, |c| {
            widgets::slider_row(
                &mut c[0],
                p,
                "Stroke ramp",
                &mut ed.brush.stroke_span,
                1.0..=500.0,
                true,
                |v| format!("{v:.0} radii"),
            );
            widgets::slider_row(
                &mut c[1],
                p,
                "Then hold for",
                &mut ed.brush.stroke_hold,
                0.0..=10.0,
                false,
                |v| format!("{v:.1}×"),
            );
        });
    });
    caption(
        ui,
        p,
        if uses_stroke {
            "Stroke position climbs from 0 to 1 over this much travel, measured \
             in dab radii so the brush behaves the same at any size, then holds \
             and starts again."
        } else {
            "Only used once something above is driven by stroke position."
        },
    );
}

/// One line about what an input actually measures, shown beside the curve.
fn input_note(input: DabInput) -> &'static str {
    match input {
        DabInput::Pressure => {
            "How hard the pen is pressed. Size, opacity, hardness and scatter \
             have their own pressure curves on the Dynamics and Scatter tabs; \
             use this for the rest."
        }
        DabInput::Speed => {
            "How fast the pointer is moving right now — it reacts within a \
             flick, so it is the one that makes a stroke thin as it is thrown."
        }
        DabInput::SlowSpeed => {
            "The same measurement smoothed over most of a second, so it \
             describes the pace of the whole gesture rather than the moment."
        }
        DabInput::Stroke => {
            "How far into the mark you are, from the ramp below. Good for paint \
             running out, or colour drifting along a stroke."
        }
        DabInput::Direction => {
            "Which way the stroke is heading, over half a turn — a line pulled \
             left and the same line pulled right read the same."
        }
        DabInput::Random => {
            "A fresh throw of the dice for every dab. One throw is shared by \
             every random input on the brush, so two of them move together."
        }
    }
}

/// The design's Scatter section: everything the dab does at random.
fn brush_editor_scatter(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;

    ui.columns(2, |c| {
        // Stated in dab radii, so a brush sprays the same way at any size.
        widgets::slider_row(
            &mut c[0],
            p,
            "Scatter",
            &mut ed.brush.scatter,
            0.0..=8.0,
            false,
            |v| format!("{v:.2}×"),
        );
        widgets::slider_row(
            &mut c[1],
            p,
            "Size jitter",
            &mut ed.brush.radius_jitter,
            0.0..=2.0,
            false,
            |v| format!("{v:.2}"),
        );
    });

    ui.columns(2, |c| {
        let round = !has_angle(ed);
        c[0].scope(|ui| {
            if round {
                ui.disable();
            }
            widgets::slider_row(
                ui,
                p,
                "Angle jitter",
                &mut ed.brush.dab_angle_jitter,
                0.0..=360.0,
                false,
                |v| format!("±{:.0}°", v * 0.5),
            );
        });
        // A curve rather than a fourth column on Dynamics: pressure-driven
        // scatter is a property of the scatter, and it is unreadable next to
        // three curves that are all about the mark rather than its randomness.
        curve_column(
            &mut c[1],
            p,
            "Pressure → scatter",
            "scatter",
            &mut ed.brush.pressure_scatter,
            &mut ed.brush.scatter_curve,
            Some(("Min scatter", &mut ed.brush.min_scatter_ratio)),
        );
    });

    // A *directed* offset, so it belongs here with the other things that move
    // a dab off the line rather than on Inputs with the modulations — and it is
    // deliberately not spelled as scatter, because a lead trails and a spray
    // does not.
    widgets::slider_row(
        ui,
        p,
        "Speed lead",
        &mut ed.brush.speed_offset,
        -3.0..=3.0,
        false,
        |v| format!("{v:+.2}"),
    );

    caption(
        ui,
        p,
        "Scatter is measured in dab radii, so a spray looks like itself at any \
         size. Angle jitter needs an elliptical dab to show. Speed lead throws \
         each dab along the direction of travel — a tenth of a second's worth \
         of it per unit — so a fast stroke runs ahead of the cursor and a slow \
         one sits on it.",
    );
}

/// The design's Texture section: the paper, and whether the mark builds up.
///
/// Two settings that look unrelated and belong together. Both are about a mark
/// made of many faint stamps rather than one solid one: grain is what makes it
/// faint, and build-up is what lets going over it again make it darker. A
/// textured brush without build-up paints one pass and then stops responding,
/// which is the surprise this section exists to avoid.
fn brush_editor_texture(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;

    widgets::toggle_row(ui, p, "Build up", &mut ed.brush.build_up);
    caption(
        ui,
        p,
        if ed.brush.build_up {
            "Each dab composites over the last, so a stroke deepens where it \
             overlaps itself and a faint stamp builds to solid. This is how \
             GIMP and Krita paint, and what a texture stamp needs."
        } else {
            "Overlapping dabs saturate instead of accumulating, so a stroke is \
             as even where it crosses itself as anywhere else. Right for a \
             solid dab; a faint stamp can never paint stronger than its own \
             brightest texel."
        },
    );

    ui.add_space(4.0);
    widgets::slider_row(
        ui,
        p,
        "Paper",
        &mut ed.brush.grain,
        0.0..=1.0,
        false,
        percent,
    );

    // The tile and its size only mean anything once the paper is biting, and
    // `has_grain()` is the same threshold the renderer uses to decide whether to
    // bind a tile at all — so a live control here is one whose effect is really
    // rendered.
    let grained = ed.brush.has_grain();
    ui.scope(|ui| {
        if !grained {
            ui.disable();
        }
        ui.spacing_mut().item_spacing.y = 12.0;

        let mut pattern = ed.brush.grain_pattern;
        let options: Vec<(GrainPattern, &str)> =
            GrainPattern::ALL.iter().map(|g| (*g, g.label())).collect();
        if widgets::segmented(ui, p, &mut pattern, &options) {
            ed.brush.grain_pattern = pattern;
        }

        ui.horizontal(|ui| {
            paper_preview(ui, p, ed.brush.grain_pattern);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                widgets::slider_row(
                    ui,
                    p,
                    "Tile size",
                    &mut ed.brush.grain_scale,
                    Brush::MIN_GRAIN_SCALE..=Brush::MAX_GRAIN_SCALE,
                    true,
                    |v| format!("{v:.0} px"),
                );
            });
        });
    });

    caption(
        ui,
        p,
        if grained {
            "The paper is fixed to the document, not to the brush, so a second \
             stroke lands in the same pits as the first. Tile size is in \
             document pixels: paper does not get coarser when you pick up a \
             bigger pencil."
        } else {
            "Raise Paper to let the texture bite into the mark. At zero the dab \
             is exactly what it would be with no paper at all."
        },
    );
}

/// A thumbnail of one paper tile.
///
/// Cached in egui's temporary store and keyed by the pattern, exactly as
/// [`tip_preview`] is: the modal redraws every frame and this would otherwise
/// upload a texture on each of them.
fn paper_preview(ui: &mut egui::Ui, p: &Palette, pattern: GrainPattern) {
    let Some(tile) = umber_core::tip::pattern(pattern.key()) else {
        return;
    };
    let id = egui::Id::new("brush-paper-preview");
    let cached: Option<(GrainPattern, egui::TextureHandle)> = ui.ctx().data(|d| d.get_temp(id));
    let texture = match cached {
        Some((held, texture)) if held == pattern => texture,
        _ => {
            let texture = ui.ctx().load_texture(
                "brush-paper",
                widgets::tip_image(tile, p.text_strong, TIP_PREVIEW_TEXELS),
                egui::TextureOptions::LINEAR,
            );
            ui.ctx()
                .data_mut(|d| d.insert_temp(id, (pattern, texture.clone())));
            texture
        }
    };

    let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, p.chrome);
    painter.image(
        texture.id(),
        rect.shrink(2.0),
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

/// Colour pickup — a brush that carries what it finds on the canvas.
fn brush_editor_blending(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;

    widgets::slider_row(
        ui,
        p,
        "Colour pickup",
        &mut ed.brush.smudge,
        0.0..=1.0,
        false,
        percent,
    );

    // The other two only mean anything once something is being picked up, and
    // `smudges()` is the same threshold the renderer uses to decide whether to
    // run a canvas probe at all — so a control that is live here is a control
    // whose effect is actually rendered.
    let blending = ed.brush.smudges();
    ui.scope(|ui| {
        if !blending {
            ui.disable();
        }
        ui.spacing_mut().item_spacing.y = 12.0;
        ui.columns(2, |c| {
            widgets::slider_row(
                &mut c[0],
                p,
                "Smear length",
                &mut ed.brush.smudge_length,
                0.0..=0.99,
                false,
                percent,
            );
            widgets::slider_row(
                &mut c[1],
                p,
                "Pickup radius",
                &mut ed.brush.smudge_radius,
                0.25..=8.0,
                true,
                |v| format!("{v:.2}×"),
            );
        });
    });

    caption(
        ui,
        p,
        if blending {
            "Colour pickup mixes what is under the brush into what it deposits; \
             at 100% it deposits only what it found. Smear length is how long \
             that colour survives, pickup radius how wide a patch it averages."
        } else {
            "Raise colour pickup to turn this into a blender. The canvas is \
             sampled once a frame while a stroke is live, so it costs nothing \
             until you do."
        },
    );
}

/// One dynamics column: an on/off toggle, the curve, its presets, and — where
/// the parameter has a floor rather than falling to zero — that floor.
fn curve_column(
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
    salt: &str,
    enabled: &mut bool,
    curve: &mut ResponseCurve,
    min: Option<(&str, &mut f32)>,
) {
    widgets::toggle_row(ui, p, label, enabled);

    ui.add_space(6.0);

    // The curve stays visible when the mapping is off, but disabled, so its
    // shape is not a surprise the moment it is switched back on.
    ui.scope(|ui| {
        if !*enabled {
            ui.disable();
        }
        let size = ui.available_width().min(metrics::CURVE_PANEL);
        widgets::curve_editor(ui, p, curve, size);

        ui.add_space(6.0);
        let current = curve.preset_name().unwrap_or("Custom");
        egui::ComboBox::from_id_salt(("curve-preset", salt))
            .selected_text(egui::RichText::new(current).size(text::TINY).color(p.text))
            .width(size)
            .show_ui(ui, |ui| {
                for (name, preset) in ResponseCurve::PRESETS {
                    if ui
                        .selectable_label(curve.preset_name() == Some(name), name)
                        .clicked()
                    {
                        *curve = preset;
                    }
                }
            });

        if let Some((label, value)) = min {
            ui.add_space(8.0);
            widgets::slider_row(ui, p, label, value, 0.0..=1.0, false, percent);
        }
    });
}
