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

use crate::editor::{self, BrushTab, Editor, Tool};
use crate::icons::{self, Icon};
use crate::panels;
use crate::shortcuts::{self, Action};
use crate::tabs;
use crate::theme::{Palette, metrics, text};
use crate::widgets;
use egui::{Align2, FontId, Frame, Margin, Rect, Sense, Stroke, pos2, vec2};
use std::sync::Arc;
use umber_core::{
    BlendMode, Brush, DabInput, DabTarget, GrainPattern, Modulation, ResponseCurve, ScrollSpan,
    Selection, SelectionMode, SelectionOp, TipMask, input::PressureSource,
};

/// Requests the UI makes that need GPU access, handled by the caller.
#[derive(Default, Clone, Copy)]
pub struct UiActions {
    pub clear: bool,
    /// Take the selection onto Umber's clipboard and on to the desktop's, and —
    /// for a cut — off the layer. The caller's, because both block on a
    /// readback *and* on the encode that puts the picture on the machine's
    /// clipboard, and a cut records an undo entry. On a very large region that
    /// is about a second, which `examples/measure-clipboard.rs` measures and
    /// this button therefore shares. Raised by the selection's overlay strip
    /// and by the Edit menu; the keyboard reaches the same two methods directly
    /// rather than through here.
    pub copy_selection: bool,
    pub cut_selection: bool,
    /// Put whatever is on the clipboard down as a floating transform. The
    /// caller's, because it uploads a texture and puts any float already up
    /// down first — and because `sysclip::decide` reads the *desktop's*
    /// clipboard, which blocks.
    ///
    /// Raised by the Edit menu, which is the only control for it: the
    /// selection's canvas strip offers Deselect, Copy and Cut, and is drawn
    /// only while a selection is live, so a picture copied in another
    /// application had nowhere in the interface to be pasted from.
    pub paste: bool,
    /// Put the Export dialog up. Nothing is written by this: it only asks the
    /// format, and the answer comes back as `export`.
    pub open_export: bool,
    /// Ask for a file and write the flattened document into it, encoded like
    /// this. The caller's, because the file dialog blocks and the pixels come
    /// off the GPU. See [`crate::exportdlg`].
    pub export: Option<umber_core::ExportOptions>,
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
    /// Mirror the whole document about this axis. The pixels are the
    /// renderer's, so like every other entry here it is the caller's to carry
    /// out.
    pub flip_canvas: Option<umber_core::FlipAxis>,
    /// Move the document to this position in the history — a click on a row of
    /// the History module. Carried out by the caller as that many undo or redo
    /// steps, since each one reads and writes a rect on the GPU.
    pub history_jump: Option<usize>,
    pub fit_view: bool,
    pub reset_zoom: bool,
    /// Step the zoom in or out about the middle of the view, exactly as
    /// [`Action::ZoomIn`] and [`Action::ZoomOut`] do from the keyboard. Routed
    /// through here rather than written on the editor where the View menu draws
    /// them, so all four of that menu's rows are carried out in one place.
    pub zoom_in: bool,
    pub zoom_out: bool,
    /// Set the Text module's block and float it over the canvas. The caller's,
    /// because it blocks on a font file and on the rasteriser and then does
    /// exactly what a paste does — `Clip::place`, `begin_float`, and the
    /// transform tool in hand.
    ///
    /// A `bool` rather than the pixels, because [`UiActions`] is `Copy`: the
    /// caller reads the block off the editor in the frame the flag was set,
    /// which is the frame the button was clicked in. Same arrangement, and the
    /// same reason, as [`UiActions::delete_picked`].
    pub place_text: bool,
    pub add_layer: bool,
    /// Put the ticked layers — or the selected one — into a new folder. The
    /// caller's, because it commits a float first; nothing else about it
    /// touches the GPU, since a folder holds no slice.
    pub group_layers: bool,
    pub delete_layer: Option<usize>,
    /// Delete every ticked layer. A `bool` rather than the list, because
    /// [`UiActions`] is `Copy` — the caller reads the ticks off the editor in
    /// the frame the flag was set, which is the frame the request was made in.
    /// Same arrangement, and the same reason, as [`UiActions::new_tip`].
    pub delete_picked: bool,
    pub move_layer_up: Option<usize>,
    pub move_layer_down: Option<usize>,
    /// Give the selected layer a mask, or take its mask off. The caller's,
    /// because a new mask has to be filled white on the GPU and both have to be
    /// recorded in the history.
    pub add_mask: bool,
    pub remove_mask: bool,
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
    /// Open the autosave copies the recovery offer was asked for.
    ///
    /// A `bool` rather than the list, because [`UiActions`] is `Copy` — the
    /// caller reads them off `Editor::recovery` in the frame the flag was set,
    /// which is the frame the request was made in. Same arrangement, and the
    /// same reason, as [`UiActions::delete_picked`].
    pub recover: bool,
    /// The recovery offer has been answered. Takes the dialog down and forgets
    /// the marker it came from; it deletes no copy, which is the whole of why
    /// saying "not now" is safe. See [`crate::recoverdlg`].
    pub dismiss_recovery: bool,
    /// Open a canvas to draw a bitmap tip for the brush currently in hand.
    ///
    /// A `bool` rather than the brush it is for, because `UiActions` is `Copy`
    /// and a preset id is a `String`. The caller reads the brush off the editor
    /// in the same frame the flag was set, which is the frame the request was
    /// made in — see [`crate::brushlib::take_draw_request`].
    pub new_tip: bool,
    /// Turn the tip document in front into the stamp it was opened for. The
    /// caller's, because the pixels come off the GPU.
    pub commit_tip: bool,
    /// Put this tab's canvas on the brush it was drawn for and then close it.
    ///
    /// The tab index rather than a `bool`, for the reason
    /// [`UiActions::save_and_close`] carries one: the prompt can be raised on a
    /// tab that is not in front, and committing one tab's canvas while closing
    /// another would lose the work in the most confusing way available. Closed
    /// only if the stamp reached a brush — a mask that was refused leaves the
    /// tab open, still holding what it was about to lose.
    pub use_tip_and_close: Option<usize>,
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
    let p = ed.palette();
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

    // The same shape, for a document that is a brush stamp rather than a
    // picture. Both are claimed before the workspace is measured, so the canvas
    // shrinks under them rather than being covered — which is what keeps the
    // camera pivot honest.
    actions.commit_tip = crate::brushlib::tip_bar(root, &p, ed);

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
    // The Tip section's "Draw a tip…", answered here because opening a document
    // needs GPU storage — the same division every other entry in `UiActions`
    // keeps.
    actions.new_tip = crate::brushlib::take_draw_request(root.ctx());
    crate::settings::show(root, &p, ed, &mut actions);
    // About, the first-run notice about the update check, and the prompt the
    // check raises. Drawn from here rather than from the Help menu, for the
    // same reason the brush library's modals are drawn from `panels`: a menu
    // closes the moment it is clicked, and a dialog owned by something that is
    // no longer on screen cannot be shut.
    crate::about::show(root, &p, ed);
    // After About, so it is the modal on top: an update in progress is the more
    // urgent of the two, and About's own update section defers to it.
    crate::updatedlg::show(root, &p, ed);

    // Drawn here rather than from a panel body, for the same reason the brush
    // library's modals are: the layout can hide a panel, and a modal that goes
    // with one cannot then be shut or reopened.
    let mut canvas = crate::canvasdlg::Outcome::default();
    crate::canvasdlg::show(root, &p, ed, &mut canvas);
    actions.create_document = canvas.create;
    actions.canvas_change = canvas.change;

    // Here for the same reason, and it answers with an encoding rather than
    // doing one: the file dialog it leads to blocks, and the pixels are the
    // GPU's.
    let mut exporting = crate::exportdlg::Outcome::default();
    crate::exportdlg::show(root, &p, ed, &mut exporting);
    actions.export = exporting.export;

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
        Some(tabs::CloseChoice::Export) => actions.open_export = true,
        // Same rule as Save: the tab closes only if the stamp actually reached
        // a brush. See `UiActions::use_tip_and_close`.
        Some(tabs::CloseChoice::UseAsTip) => actions.use_tip_and_close = ed.ui.close_prompt.take(),
        Some(tabs::CloseChoice::Cancel) | None => {}
    }

    // What a session that stopped left behind. Drawn from here rather than from
    // a panel body, for the reason the brush library's modals and the canvas
    // dialogs are, and placed here rather than anywhere else in this run of
    // dialogs for two reasons of its own. *After* the quit prompt, because that
    // one is the answer to "the window is closing" and supersedes everything —
    // though the two cannot in practice be on screen together, since this is
    // only ever raised on the first frame. *Before* the notice below, because
    // recovering a document can raise one, and a warning about what an import
    // dropped has to land on top of the offer that produced it.
    crate::recoverdlg::show(root, &p, ed, &mut actions);

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
            // Beside the outline it belongs to rather than beside the
            // transform box, which is the other thing drawn here and is what
            // takes its place: `selection_buttons` returns early while a float
            // is up, so the two can never both be on screen and the order of
            // these two calls does not matter.
            selection_buttons(ui, &p, ed, rect, &mut actions);
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

/// Side of a button drawn over the canvas, the clearance between a strip of
/// them and the thing it acts on, and the gap between two of them.
///
/// One set of numbers for both strips — the floating transform's flip pair and
/// the selection's Deselect / Copy / Cut — because they are the same control in
/// the same kind of place, and two sets would let them drift apart on screen
/// for no reason anybody could state.
const CANVAS_BUTTON: f32 = 22.0;
const CANVAS_BUTTON_GAP: f32 = 12.0;
const CANVAS_BUTTON_SPACING: f32 = 4.0;

/// The width a strip of `n` canvas buttons takes.
fn strip_width(n: usize) -> f32 {
    n as f32 * CANVAS_BUTTON + (n.saturating_sub(1)) as f32 * CANVAS_BUTTON_SPACING
}

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
    let width = strip_width(2);
    let top = bounds.top() - CANVAS_BUTTON_GAP - CANVAS_BUTTON;
    let strip = Rect::from_min_size(
        pos2(bounds.center().x - width * 0.5, top),
        vec2(width, CANVAS_BUTTON),
    );
    // A box dragged up under the strips takes its buttons off the top of the
    // canvas region with it. Drawing them clipped would leave live targets
    // nobody can see, so they are simply not offered — the box can be dragged
    // back down, and Enter still puts it down from anywhere.
    //
    // Deliberately *not* the selection strip's rule, which pulls itself back on
    // screen instead: a floating transform can be moved and a selection cannot,
    // so there the artist would have no way of reaching the controls at all.
    //
    // Against what the scrollbars have left rather than the whole region, for
    // the reason the selection strip is placed there: the bars are drawn after
    // this and win a tie, so a flip button in the strip would be a button that
    // scrolls. Here that means declining a fraction earlier, which is the rule
    // this already follows.
    let free = canvas_free_of_scrollbars(ed, rect);
    if !free.contains_rect(strip) {
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
            pos2(
                strip.left() + i as f32 * (CANVAS_BUTTON + CANVAS_BUTTON_SPACING),
                strip.top(),
            ),
            egui::Vec2::splat(CANVAS_BUTTON),
        );
        ed.transform_buttons[i] = Some(at);
        let button = CanvasButton {
            at,
            clip: free,
            id: ("float-flip", i),
            icon,
            enabled: true,
        };
        if canvas_button(ui, p, button, || tip.to_string())
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

/// The live selection's own controls: Deselect, Copy and Cut, in a strip beside
/// the marquee.
///
/// **Real buttons over the canvas**, exactly as the flip pair above are, so
/// their rectangles go into `Editor::selection_buttons` and through
/// `canvas_overlay_owns_pointer` — otherwise a press on one is also a press on
/// the canvas, which with a brush in hand is a dab painted under the button
/// that was clicked, inside the very selection somebody was about to copy. That
/// test is consulted on the pen's path as well as the mouse's, through
/// `pointer_over_canvas`, so this cannot be a control that works with a mouse
/// and paints with a pen.
///
/// **Gone the moment the pixels are picked up.** A float has the transform
/// tool's own strip in that place, and a Copy beside it would be a control
/// about a selection that is no longer what an edit acts on.
///
/// Where the strip goes is `umber_core::overlay`'s, with the reasoning there:
/// the marquee can be scrolled half off the view, pushed under a docked panel
/// or drawn round the whole canvas, and unlike a floating transform it cannot
/// be dragged back into reach — so the strip comes to the pointer rather than
/// declining to appear.
fn selection_buttons(
    ui: &mut egui::Ui,
    p: &Palette,
    ed: &mut Editor,
    rect: Rect,
    actions: &mut UiActions,
) {
    ed.selection_buttons = [None, None, None];
    if ed.float.is_some() {
        return;
    }
    let Some(bounds) = ed.selection.as_ref().map(|s| s.bounds()) else {
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
        glam::Vec2::new(s.x / scale, s.y / scale)
    };

    let anchor = umber_core::Rect::new(
        to_screen(glam::Vec2::new(bounds.x as f32, bounds.y as f32)),
        to_screen(glam::Vec2::new(
            (bounds.x + bounds.width) as f32,
            (bounds.y + bounds.height) as f32,
        )),
    );
    // Placed inside what the scrollbars have left, not the whole region: they
    // are drawn after this and win a tie, so a Cut button flush against the
    // bottom edge would be a button that scrolls. `canvas_free_of_scrollbars`
    // has the argument. The *pivot* above is still the full rect's centre,
    // because that is where the picture is.
    //
    // With the whole region as the fallback, and that is not belt and braces.
    // `place_strip` begins by intersecting the marquee with the view, so a
    // selection whose visible sliver lies *entirely* inside the eleven-point
    // band has no overlap with the inset region and gets no strip at all —
    // Deselect, Copy and Cut simply gone, for a selection with a perfectly good
    // place above it. Losing the commands is far worse than a button sharing an
    // edge with a scrollbar, and "the strip comes to the pointer rather than
    // declining to appear" is the rule that module exists to hold.
    let free = canvas_free_of_scrollbars(ed, rect);
    let as_view = |r: Rect| {
        umber_core::Rect::new(
            glam::Vec2::new(r.left(), r.top()),
            glam::Vec2::new(r.right(), r.bottom()),
        )
    };
    let size = glam::Vec2::new(strip_width(3), CANVAS_BUTTON);
    let place =
        |r: Rect| umber_core::overlay::place_strip(anchor, as_view(r), size, CANVAS_BUTTON_GAP);
    let Some((clip, strip)) = place(free)
        .map(|s| (free, s))
        .or_else(|| place(rect).map(|s| (rect, s)))
    else {
        return;
    };

    // Cut writes to the layer, so a locked one refuses it — and the control
    // says so before the click rather than answering with a dialog, which is
    // the rule "Clear layer" already follows against its own menu item. Copy
    // and Deselect write nothing and are never refused.
    let locked = ed.layers.active_is_locked();
    let mut deselect = false;
    for (i, (icon, enabled)) in [
        (Icon::Deselect, true),
        (Icon::Copy, true),
        (Icon::Cut, !locked),
    ]
    .into_iter()
    .enumerate()
    {
        let at = Rect::from_min_size(
            pos2(
                strip.rect.min.x + i as f32 * (CANVAS_BUTTON + CANVAS_BUTTON_SPACING),
                strip.rect.min.y,
            ),
            egui::Vec2::splat(CANVAS_BUTTON),
        );
        // Recorded whatever happens next — including for a *disabled* button,
        // which still must not let a press through to the canvas underneath it.
        ed.selection_buttons[i] = Some(at);
        // The tooltip is built inside the closure, which egui calls only while
        // the pointer is actually on the button. `shortcuts::labelled` formats
        // a string and reads the binding table behind a lock, and this runs
        // several times a second for as long as a selection is open — the same
        // reason `Editor::selection_screen` exists.
        let button = CanvasButton {
            at,
            clip,
            id: ("selection-strip", i),
            icon,
            enabled,
        };
        let clicked = canvas_button(ui, p, button, || match i {
            0 => shortcuts::labelled("Deselect", Action::Deselect),
            1 => shortcuts::labelled("Copy the selection", Action::Copy),
            _ if locked => "The layer is locked, so nothing can be cut out of it. \
                            Unlock it in the Layers panel."
                .to_string(),
            _ => shortcuts::labelled("Cut the selection", Action::Cut),
        });
        if clicked {
            match i {
                0 => deselect = true,
                1 => actions.copy_selection = true,
                _ => actions.cut_selection = true,
            }
        }
    }
    // After the loop, because it takes the selection this frame's rectangles
    // were computed from.
    //
    // The rectangles are deliberately **left standing** for the rest of this
    // frame. The buttons have already been painted into it, so clearing them
    // would leave three marks on screen that the canvas owns — and a press
    // there would paint a dab under a button the artist can still see. The
    // opposite window costs a swallowed press over open canvas and no pixels,
    // which is the cheaper of the two; the repaint below closes it next frame
    // rather than waiting for egui to volunteer one.
    if deselect {
        ed.deselect();
        ui.ctx().request_repaint();
    }
}

/// One button of a strip drawn over the canvas.
///
/// Shared by the two strips so they cannot look like different controls. The
/// caller records `at` before calling: whether the click is acted on is the
/// caller's, but whether a *press* there belongs to the canvas is not, and a
/// button whose rectangle was only recorded on the frame it happened to be
/// clicked would paint underneath itself on every other one.
///
/// A disabled one still hovers and still shows its tooltip, matching
/// [`icon_button`] and for the same reason: the tooltip is usually the
/// *explanation*, and skipping the hover along with the click leaves a dead
/// mark with nothing to say for itself.
///
/// `tip` is a closure because egui calls it only while the pointer is on the
/// button. Building the text unconditionally would allocate on the drawing
/// path, and a canvas overlay is drawn for as long as the thing it belongs to
/// is on screen.
struct CanvasButton {
    /// Where it goes, in points.
    at: Rect,
    /// The canvas region. Painting is clipped to it, so a strip that reaches
    /// the edge does not draw over a panel.
    clip: Rect,
    /// Unique within the frame; the strip and the index within it.
    id: (&'static str, usize),
    icon: Icon,
    enabled: bool,
}

fn canvas_button(
    ui: &mut egui::Ui,
    p: &Palette,
    button: CanvasButton,
    tip: impl FnOnce() -> String,
) -> bool {
    let CanvasButton {
        at,
        clip,
        id,
        icon,
        enabled,
    } = button;
    let response = ui.interact(
        at,
        ui.id().with(id),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let lit = enabled && response.hovered();
    let painter = ui.painter().with_clip_rect(clip);
    painter.rect_filled(
        at,
        metrics::RADIUS,
        if lit { p.control_hover } else { p.control },
    );
    painter.rect_stroke(
        at,
        metrics::RADIUS,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
    let ink = match (enabled, lit) {
        (false, _) => p.text_dim,
        (_, true) => p.text_strong,
        _ => p.text,
    };
    icons::draw(&painter, at.shrink(3.0), icon, ink);
    response
        .on_hover_ui(|ui| {
            ui.label(tip());
        })
        .clicked()
}

/// The canvas scrollbars, along the bottom and the right of the document
/// region — the right being the left edge of whatever is docked there.
///
/// Drawn on both axes of every document, and [`ScrollSpan`]'s own docs have the
/// argument. Short version: these used to be drawn only where part of the
/// picture was off the view, which reads as the honest rule and hid travel the
/// camera already had — zoom out until the whole canvas fits and the only way
/// to shift it off centre was to zoom back in. The tell, from a running window,
/// was that a single notch of the wheel made both bars appear.
///
/// The geometry is [`ScrollSpan`]'s, in `umber-core`, so what the thumb says
/// and where the camera is cannot drift apart — the same division `dock.rs` and
/// `panels.rs` keep.
///
/// Recording the rectangles in [`Editor::scroll_bars`] carries more weight than
/// it used to rather than less: the bars are now a live target over the canvas
/// on *every* frame, and that record is the only thing between a press on one
/// and a dab under it. Both pointers reach it through
/// `Editor::pointer_over_canvas`, so a pen cannot paint through a bar a mouse
/// is refused by.
fn canvas_scrollbars(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, rect: Rect) {
    let bars = scrollbars(ed, rect);
    ed.scroll_bars = bars.at;

    // Space is the pan gesture, and `gesture::press` gives it the canvas
    // *before* the interface is consulted — "a space-drag pans whatever it
    // started over" is the rule, and it deliberately does not read
    // `Editor::scroll_bars`. So while space is held the bar must not take a
    // drag of its own, or a press in the strip drives the camera twice: the pan
    // moves the picture with the hand and the bar moves it against, and the
    // second is the larger, so the canvas slides *backwards* under the pointer.
    // Painting the thumbs is still right — they go on reporting where the
    // picture is while it is dragged.
    let live = !ed.space_down;

    if let Some(at) = bars.at[1]
        && let Some(by) = widgets::canvas_scrollbar(ui, p, at, bars.down, true, live)
    {
        ed.camera.center.y += by;
    }
    if let Some(at) = bars.at[0]
        && let Some(by) = widgets::canvas_scrollbar(ui, p, at, bars.across, false, live)
    {
        ed.camera.center.x += by;
    }
}

/// Where the two bars go and what they are showing.
///
/// Separated from the painting because two other things need the answer before
/// the bars are drawn: the selection strip and the transform's flip pair are
/// placed inside what is *left* of the canvas region. See
/// [`canvas_free_of_scrollbars`].
struct Scrollbars {
    across: ScrollSpan,
    down: ScrollSpan,
    /// Horizontal then vertical, matching [`Editor::scroll_bars`].
    at: [Option<Rect>; 2],
}

/// A pure function of the editor and this frame's canvas rect, so the geometry
/// is stated once however many callers want it.
fn scrollbars(ed: &Editor, rect: Rect) -> Scrollbars {
    // The viewport in *document* units, so the spans are worked out from the
    // region actually being laid out this frame rather than from last frame's
    // `canvas_size`.
    let scale = ed.pixels_per_point.max(1e-3);
    let doc = ed.doc.size_vec2();
    let zoom = ed.camera.zoom;
    let across = ScrollSpan::new(doc.x, rect.width() * scale, zoom, ed.camera.center.x);
    let down = ScrollSpan::new(doc.y, rect.height() * scale, zoom, ed.camera.center.y);

    let bar = metrics::SCROLLBAR;
    // A track too short to hold a thumb is refused outright rather than drawn
    // and left undraggable — it would be a strip of canvas that swallows every
    // press for nothing, which is worse than no bar. Tested against the length
    // the bar gets when the *other* one is also drawn, which is the shorter of
    // the two cases and therefore the safe one to judge on.
    let show_x = across.scrollable() && rect.width() - bar > widgets::MIN_TRACK;
    let show_y = down.scrollable() && rect.height() - bar > widgets::MIN_TRACK;

    // Neither bar runs under the other: a thumb sliding into the corner where
    // they cross would be under the one on top of it for its last few pixels.
    let corner_x = rect.right() - if show_y { bar } else { 0.0 };
    let corner_y = rect.bottom() - if show_x { bar } else { 0.0 };

    let at = [
        show_x.then(|| {
            Rect::from_min_max(
                pos2(rect.left(), rect.bottom() - bar),
                pos2(corner_x, rect.bottom()),
            )
        }),
        show_y.then(|| {
            Rect::from_min_max(
                pos2(rect.right() - bar, rect.top()),
                pos2(rect.right(), corner_y),
            )
        }),
    ];
    Scrollbars { across, down, at }
}

/// The part of the canvas region a control may be placed in.
///
/// The bars occupy a strip along the bottom and the right of *every* frame now,
/// and they are drawn **after** the selection strip and the flip pair — so
/// egui breaks a tie in the bar's favour, and a Cut button half under a
/// scrollbar is a button that scrolls when it is clicked. Placing those two
/// inside what is left is the fix rather than reordering the draws: reordering
/// only swaps which of the two controls is unreachable.
///
/// It was already possible before the bars became permanent, on any document
/// larger than its view, which is most of a painting session — so this is a
/// standing bug the change made universal rather than one it introduced.
///
/// Only for *placing*. The marquee and the transform box are drawn over the
/// whole region, because they are pictures of where the pixels are rather than
/// things to press, and the camera pivot is the full rect's centre.
fn canvas_free_of_scrollbars(ed: &Editor, rect: Rect) -> Rect {
    let at = scrollbars(ed, rect).at;
    let mut free = rect;
    if at[0].is_some() {
        free.max.y -= metrics::SCROLLBAR;
    }
    if at[1].is_some() {
        free.max.x -= metrics::SCROLLBAR;
    }
    free
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
/// That request is still the whole of what this function makes. On Windows it
/// is not the whole of what *happens*: winit's `set_cursor_visible` — which is
/// what egui-winit turns `CursorIcon::None` into — cannot reach a pen, and
/// `syscursor` is the one line that finishes the job, from the same per-frame
/// answer rather than from a state of its own. Its module docs have the
/// argument.
///
/// Where the dot goes is [`Editor::pen_dot`]'s, in a pure function, because it
/// is the half of this that can be tested on a machine with no tablet.
///
/// The dot is in points, which is egui's unit and already scaled — so it is
/// the same size on the screen whatever the display's density, exactly as the
/// panels and the type are.
fn pen_cursor(ui: &egui::Ui, p: &Palette, ed: &Editor) {
    // Over a panel, a menu, a modal or a scrollbar the ordinary cursor is the
    // right one: those are things to point at, and a workspace whose pointer
    // vanished at the edge of the canvas — or the moment a dialog opened —
    // would be unusable. Both readings have to be asked of egui, so they are
    // taken here and handed to the rule rather than fetched inside it.
    //
    // Focus belongs in the *request*, not beside the platform call that
    // carries it out: asking for "none" while another application has the
    // keyboard leaves egui-winit's dedupe holding a blank cursor it will never
    // be prompted to replace. `Editor::pen_dot`'s docs have that in full.
    //
    // `input.focused` and deliberately not `input.viewport().focused`, which
    // looks like the more direct reading of the same thing and here is always
    // `None`: egui-winit fills that in from `update_viewport_info`, which the
    // caller has to invoke and `app::render` does not. `focused` is written
    // from the `WindowEvent::Focused` egui-winit sees in `on_window_event`,
    // which every window event goes through.
    let around = editor::Surroundings {
        over_area: editor::over_egui_area(ed, ui.ctx(), ed.cursor),
        focused: ui.ctx().input(|i| i.focused),
    };
    let Some(at) = ed.pen_dot(around) else {
        return;
    };
    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
    // `text_dim` is the palette's recessive ink, and it is the one token that
    // is a mid-grey in *both* themes — the surfaces invert between Graphite and
    // Paper and most of the ink with them, so anything stronger would be black
    // on one and white on the other, over artwork that is neither.
    ui.painter()
        .circle_filled(ed.to_points(at), metrics::PEN_DOT, p.text_dim);
}

fn menu_bar(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    ui.horizontal_centered(|ui| {
        let (mark, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
        ui.painter().rect_filled(mark, 3.0, p.accent);
        ui.add_space(6.0);

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| file_menu(ui, ed, actions));
            ui.menu_button("Edit", |ui| edit_menu(ui, ed, actions));
            ui.menu_button("View", |ui| view_menu(ui, actions));

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
                update_rehearsal(ui, ed);
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
                "{} · {} × {}",
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

/// The File menu's rows.
///
/// The three menus that carry [`Action`]s are functions of their own rather
/// than closures inside [`menu_bar`], so a test can draw one into a plain `Ui`
/// and read back what it says. Opening a real popup headlessly means
/// synthesising a click at a position nothing reports, and what the test is
/// after is the rows rather than the popup.
fn file_menu(ui: &mut egui::Ui, ed: &mut Editor, actions: &mut UiActions) {
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
    // Beside Canvas settings rather than under a new Image menu:
    // these are the same kind of thing — a change to the document
    // rather than to the artwork on it — and the menu bar is the
    // design's, not something to add a heading to for two rows.
    //
    // Unlike a resize, a flip keeps the undo history: the canvas
    // size does not change and the flip is its own inverse, so it
    // goes in the history as an entry that stores no pixels.
    // A locked layer refuses the flip *whole* — a picture with some
    // layers mirrored and some not was never on screen, and a flip
    // that half happened cannot be undone by flipping again. Said
    // here rather than only refused in `mirror_document`, so the
    // menu does not offer what it will not do.
    let flip_locked = ed.layers.any_locked();
    for (axis, hint) in [
        (
            umber_core::FlipAxis::Horizontal,
            "Mirror every layer left to right. The canvas size is unchanged.",
        ),
        (
            umber_core::FlipAxis::Vertical,
            "Mirror every layer top to bottom. The canvas size is unchanged.",
        ),
    ] {
        let action = match axis {
            umber_core::FlipAxis::Horizontal => Action::FlipCanvasHorizontal,
            umber_core::FlipAxis::Vertical => Action::FlipCanvasVertical,
        };
        if menu_item(ui, action, !flip_locked)
            .on_hover_text(hint)
            .on_disabled_hover_text(
                "A layer is locked. A flip mirrors every layer at once, so it \
                             cannot skip one. Unlock it first.",
            )
            .clicked()
        {
            actions.flip_canvas = Some(axis);
            ui.close();
        }
    }
    ui.separator();
    if menu_item(ui, Action::Save, true).clicked() {
        actions.save = true;
        ui.close();
    }
    if menu_item(ui, Action::SaveAs, true).clicked() {
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
    // Said before the click, like removing a mask and like the
    // canvas dialog's own line: clearing is the last command that
    // is not an undoable edit, so it takes the history with it and
    // there is no way back afterwards.
    if ui
        .add_enabled(
            !ed.layers.active_is_locked(),
            egui::Button::new("Clear layer"),
        )
        .on_hover_text("Empties the layer, and clears the undo history with it.")
        .on_disabled_hover_text("Unlock the layer to clear it.")
        .clicked()
    {
        actions.clear = true;
        ui.close();
    }
    if menu_item(ui, Action::Export, true)
        .on_hover_text(
            "One flattened image for showing people: PNG, JPEG, TIFF, GIF or \
                         BMP. Save keeps the layers.",
        )
        .clicked()
    {
        actions.open_export = true;
        ui.close();
    }
}

/// The Edit menu's rows: the two history commands, the four that move pixels
/// on and off the clipboard, and Settings.
fn edit_menu(ui: &mut egui::Ui, ed: &mut Editor, actions: &mut UiActions) {
    // The history covers strokes, transforms, canvas flips and
    // changes to the layer stack, which is what the History panel's
    // own note says. It used to cover painting alone, and these two
    // lines went on saying so long after the six structural edits
    // were recorded — a menu contradicting a panel about the same
    // list. Clearing a layer and resizing the canvas are still
    // outside it, and both still clear the list.
    if menu_item(ui, Action::Undo, ed.history.can_undo())
        .on_disabled_hover_text("Nothing has been done to this document yet.")
        .clicked()
    {
        actions.undo = true;
        ui.close();
    }
    if menu_item(ui, Action::Redo, ed.history.can_redo())
        .on_disabled_hover_text("Nothing undone to put back.")
        .clicked()
    {
        actions.redo = true;
        ui.close();
    }
    ui.separator();
    // Deselect, Copy, Cut and Paste. All four are bound and all four
    // are dispatched; only Copy and Cut had a control, on the
    // selection's canvas strip, which is drawn only while a
    // selection is live. So Paste had nowhere in the interface to be
    // found at all: somebody who copied a picture in another
    // application had a keystroke and no menu row.
    if menu_item(ui, Action::Deselect, ed.selection.is_some())
        .on_hover_text("Let edits reach the whole layer again.")
        .on_disabled_hover_text("Nothing is selected, so edits already reach the whole layer.")
        .clicked()
    {
        ed.deselect();
        ui.close();
    }
    // Never disabled. It writes nothing, and with nothing selected
    // it copies the whole layer.
    if menu_item(ui, Action::Copy, true).clicked() {
        actions.copy_selection = true;
        ui.close();
    }
    // Gated on the lock, matching the canvas strip's Cut button and
    // the gate inside `App::cut_selection`: a cut takes pixels off
    // the layer, so a locked one refuses it, and the row says so
    // before the click rather than answering with a notice.
    if menu_item(ui, Action::Cut, !ed.layers.active_is_locked())
        .on_disabled_hover_text("Unlock the layer to cut from it.")
        .clicked()
    {
        actions.cut_selection = true;
        ui.close();
    }
    // **Never disabled, and `ed.clipboard.is_none()` is the wrong
    // test to reach for.** What a paste puts down is
    // `sysclip::decide`'s answer, read at paste time from the
    // *desktop's* clipboard as well as Umber's own — so a screenshot
    // taken in another application pastes perfectly well with
    // Umber's clip empty, and a row disabled on that field would
    // refuse it. Reading the desktop's clipboard every frame to find
    // out is not an option either: it blocks.
    if menu_item(ui, Action::Paste, true).clicked() {
        actions.paste = true;
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
}

/// The View menu's rows.
///
/// It takes no [`Editor`], which is the shape of the whole menu: all four rows
/// are requests the caller carries out, so there is nothing here to write.
fn view_menu(ui: &mut egui::Ui, actions: &mut UiActions) {
    // Four rows for the four bound View actions. Zoom in and Zoom
    // out had none, and the other two showed no key and named
    // themselves: "Fit to window" here against "Fit to view" on the
    // Shortcuts page, for one command. Every label is now
    // `Action::label`'s.
    if menu_item(ui, Action::FitView, true).clicked() {
        actions.fit_view = true;
        ui.close();
    }
    if menu_item(ui, Action::ActualSize, true).clicked() {
        actions.reset_zoom = true;
        ui.close();
    }
    ui.separator();
    if menu_item(ui, Action::ZoomIn, true).clicked() {
        actions.zoom_in = true;
        ui.close();
    }
    if menu_item(ui, Action::ZoomOut, true).clicked() {
        actions.zoom_out = true;
        ui.close();
    }
}

/// A way to look at each of the update dialog's screens without a release to
/// install.
///
/// Nobody working on Umber can run a real update against a real release — it
/// would mean cutting one — so every screen of `updatedlg.rs` would otherwise
/// ship having been reasoned about and never seen. This walks the model to each
/// of them with a release that does not exist.
///
/// **It is `debug_assertions` only, and so is everything it reaches.** A live
/// control that fabricates a release has no business in a build somebody paints
/// with, and the reason it is a compile-time gate rather than a hidden flag is
/// that a hidden flag is a thing somebody finds.
#[cfg(debug_assertions)]
fn update_rehearsal(ui: &mut egui::Ui, ed: &mut Editor) {
    use crate::update::{Applied, Phase, Stage, flow::Countdown};

    ui.separator();
    ui.menu_button("Update dialog (debug)…", |ui| {
        let screens: [(&str, Phase); 7] = [
            ("The offer", Phase::Offer),
            (
                "Downloading",
                Phase::Working(Stage::Downloading {
                    received: 12 * 1024 * 1024,
                    total: 31 * 1024 * 1024,
                }),
            ),
            ("Unpacking", Phase::Working(Stage::Unpacking)),
            ("Installing on Windows", Phase::Working(Stage::HandingOver)),
            (
                "Done, restart",
                Phase::Done {
                    outcome: Applied::Restart,
                    countdown: Countdown::stopped(),
                },
            ),
            (
                "Done, installer",
                Phase::Done {
                    outcome: Applied::Installer,
                    countdown: Countdown::stopped(),
                },
            ),
            (
                "Failed",
                Phase::Failed(
                    "Umber could not download umber-0.0.5-x64.msi: connection reset \
                     by peer."
                        .to_string(),
                ),
            ),
        ];
        for (label, phase) in screens {
            if ui.button(label).clicked() {
                // The countdown is the real one — started here, not faked — so
                // what is on screen is the drawing a real update produces.
                ed.updates.demo(phase, std::time::Instant::now());
                ui.close();
            }
        }
    });
}

/// Nothing in a release build.
#[cfg(not(debug_assertions))]
fn update_rehearsal(_ui: &mut egui::Ui, _ed: &mut Editor) {}

/// A menu entry standing for one bindable command: its own name, and the chord
/// currently bound to it.
///
/// **The label comes from the action, never from the call site.** A menu row and
/// the Shortcuts page are two views of one command, and typing the name here as
/// well made them disagree: `Action::FitView` is "Fit to view" in the settings
/// list and was drawn as "Fit to window" in the View menu, which is two names
/// for one thing in an interface that has a search field for the other one. It
/// is the rule `shortcuts` already states about the bindings themselves —
/// enumerable data rather than a `match` — applied to the names as well. A row
/// that is *not* a command (New…, Open…, Canvas settings…, Close document,
/// Clear layer, Settings…, About) has no action to take a name from and is
/// still written out where it is drawn.
///
/// The chord is read out of the live binding table rather than typed next to the
/// label, so a rebind in the settings dialog reaches the menu as well — and an
/// action left unbound shows no chord instead of a stale one. `published` clones
/// the table, which is only ever paid while a menu is open.
///
/// `enabled` is a parameter rather than an `add_enabled_ui` around the call, so
/// a row can be both dead and labelled with its key. Undo and Redo were built as
/// bare buttons for exactly that reason, which left `Ctrl+Z` named nowhere in
/// the interface outside the Shortcuts page.
fn menu_item(ui: &mut egui::Ui, action: shortcuts::Action, enabled: bool) -> egui::Response {
    let chord = shortcuts::published()
        .iter()
        .find(|b| b.action == action)
        .map(|b| b.chord().display())
        .unwrap_or_default();
    ui.add_enabled(
        enabled,
        egui::Button::new(action.label()).shortcut_text(chord),
    )
}

/// What each optional group on the tool options strip costs, in points.
///
/// The strip is a single unwrapped row, so a window narrow enough to overrun it
/// does not reflow — the controls simply carry on past the right edge. These
/// budgets decide which groups are drawn, in reverse order of how constantly a
/// painter reaches for them: the stabiliser goes first, then opacity, then
/// size.
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
    /// The stabiliser rail. It used to be 110 for a `widgets::chip`, which is a
    /// label and a small pill; it is a third [`crate::widgets::inline_slider`]
    /// now, so it costs what one costs — the 90 point rail, the readout, and a
    /// label three characters longer than "Opacity"'s.
    pub const STABILISER: f32 = 190.0;
    /// The line naming the modifiers that add to, subtract from and intersect a
    /// selection, and say what the feather applies to.
    pub const COMBINE: f32 = 320.0;
    /// The four marks that say what a new shape does to the selection.
    pub const SELECT_OP: f32 = 105.0;
    /// The feather rail, its label and its readout.
    pub const FEATHER: f32 = 165.0;
    /// The second sentence for the Pan and Zoom tools — the one naming the
    /// gesture that reaches the same thing with any tool in hand. Measured
    /// against what is *left* after the first sentence, exactly as [`COMBINE`]
    /// is, because the first is drawn unconditionally and is in no budget.
    /// Wide enough for the longer of the two, which is Zoom's — measured off
    /// `options_strip_preview`'s shots rather than guessed, because this one is
    /// a whole sentence and a budget below its real cost draws it half off the
    /// edge instead of not drawing it. 280 was that mistake: it let Zoom's line
    /// start at a strip width of about 630, where it needs 365.
    pub const NAVIGATE_MORE: f32 = 370.0;
}

/// What the Pan and Zoom tools do, and how to reach the same thing without
/// putting the brush down.
///
/// Both used to draw the words "drag on the canvas", which is four lowercase
/// words saying nothing a painter had not already guessed from the icon — and
/// neither named the gesture that actually matters. Pan and Zoom are the two
/// tools most people never select, because holding Space and rolling the wheel
/// under the primary modifier do the same job mid-stroke; the strip is where
/// that is worth saying, since a held modifier is part of a gesture rather than
/// a command and is therefore deliberately not in the rebindable table. Same
/// reasoning as [`combine_hint`], and it is why these are `const fn`s over
/// [`shortcuts::primary_modifier_name`] rather than `format!` per frame: the
/// modifier is named for the platform, and neither line can change at run time
/// because neither gesture is bindable.
///
/// The second half of each is behind [`strip_budget::NAVIGATE_MORE`]: the strip
/// is one unwrapped row, so a sentence that does not fit runs off the end of it
/// rather than reflowing.
const fn navigate_hint(tool: Tool) -> (&'static str, &'static str) {
    match tool {
        Tool::Zoom => (
            "Drag right or up to zoom in, left or down to zoom out.",
            if cfg!(target_os = "macos") {
                "Hold Cmd and roll the wheel to zoom at the pointer with any tool in hand."
            } else {
                "Hold Ctrl and roll the wheel to zoom at the pointer with any tool in hand."
            },
        ),
        // Pan's, and every tool that is not Zoom: the branch this sits in is
        // reached by Pan and Zoom alone, and a wildcard keeps the function
        // total without a panic nobody could ever see.
        _ => (
            "Drag on the canvas to move the picture.",
            "Hold Space to do the same with any tool in hand.",
        ),
    }
}

/// How to add to, subtract from and intersect a selection, named for this
/// platform — and **what the feather applies to**.
///
/// A held modifier is part of a gesture rather than a command, so it is
/// deliberately not in the rebindable table — which leaves the options strip as
/// the only place the user can find out. It is a *second* way in rather than
/// the only one now: the four marks beside it are the same four operations, for
/// the reason `App::selection_op` gives.
///
/// The feather's half is here because [`widgets::inline_slider`] has no
/// tooltip — it is the strip's rail and every other user of it sets something
/// that takes effect at once — and a rail that reads `0…250` while a selection
/// is standing and changes nothing about it is a control that lies unless the
/// strip says which selection it means. Saying it in the label ("Feather next")
/// was the alternative and reads as a typo; this is where the other thing the
/// strip has to explain about a *gesture* already lives.
///
/// `const` rather than `format!` with `shortcuts::primary_modifier_name`,
/// because the strip is painted every frame and a string built per frame for a
/// line that never changes is exactly what the rest of this interface avoids.
const fn combine_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Hold Shift to add, Cmd to subtract, both to intersect. Feather softens \
         the next shape you draw."
    } else {
        "Hold Shift to add, Ctrl to subtract, both to intersect. Feather softens \
         the next shape you draw."
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
                // The third rail, and the same control as the two beside it.
                // It used to be a `widgets::chip` — a reading, with a tooltip
                // saying to go to the brush editor to change it — which put the
                // one setting a painter adjusts *while* drawing a line behind
                // two clicks and a tab. `metrics::OPTIONS_STRIP` is 36 points,
                // so `widgets::number_row`'s two stacked rows do not fit here
                // and this figure cannot be typed; that is the strip's own
                // trade and is why `inline_slider` is what the strip uses.
                //
                // The range is the brush editor's own — 0.0..=0.95, where 1.0
                // would be a stroke that never reaches the pen — so the two
                // controls cannot disagree about what full stabilisation is.
                widgets::inline_slider(
                    ui,
                    p,
                    "Stabiliser",
                    &mut ed.brush.stabilization,
                    0.0..=Brush::MAX_STABILIZATION,
                    false,
                    |v| format!("{:.0}", v * 100.0),
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
            // One reading of the room and cumulative budgets, exactly as the
            // brush's three groups above do it, in reverse order of how badly
            // somebody is stuck without them. The controls come before the
            // prose because a control that is not drawn cannot be reached at
            // all, where a sentence has usually already been read — and the
            // feather goes before the marks because the four operations have a
            // second way in (the modifiers, which the line at the end names)
            // and the feather has none.
            let room = ui.available_width();
            if room >= strip_budget::SELECT_OP {
                divider(ui, p);
                selection_op_switch(ui, p, ed);
            }
            if room >= strip_budget::SELECT_OP + strip_budget::FEATHER {
                divider(ui, p);
                widgets::inline_slider(
                    ui,
                    p,
                    "Feather",
                    &mut ed.ui.selection_feather,
                    0.0..=Selection::MAX_FEATHER,
                    false,
                    |v| format!("{v:.0}"),
                );
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(ed.ui.selection_mode.hint())
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
            // Dropped first when the window is narrow: the gesture the mode
            // needs is the line somebody is stuck without, and this one is
            // about a gesture they have not reached for yet.
            //
            // **Measured afresh, not against `room`.** The mode hint above it is
            // drawn unconditionally and is in no budget — `SelectionMode::
            // Polygon`'s is eighty-four characters — so a budget taken before it
            // would put this sentence hundreds of points past the right edge of
            // a strip that does not reflow. Reading what is actually left is
            // what the line has always done, and it accounts for the hint by
            // construction rather than by a number that would have to be kept
            // in step with three sentences.
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
            // Pan and Zoom. Two sentences in the Transform hint's register
            // rather than a control, because like Transform there is nothing
            // here to set: the whole gesture is the pointer's.
            let (does, instead) = navigate_hint(ed.ui.tool);
            ui.label(
                egui::RichText::new(does)
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
            // Read afresh rather than against the room measured earlier, for
            // the reason the selection tool's combine line reads it afresh: the
            // sentence above is drawn unconditionally and is in no budget.
            if ui.available_width() >= strip_budget::NAVIGATE_MORE {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(instead)
                        .size(text::SMALL)
                        .color(p.text_dim),
                );
            }
        }
    });
}

/// The selection tool's mode switch: the mode name and a chevron, opening a
/// list of the three.
///
/// [`widgets::dropdown`], like every other dropdown in the interface. It used
/// to paint a filled `p.control` pill behind itself so it would read as a
/// control against the strip, and it no longer does — deliberately. A fill on
/// this strip meant the opposite of "a control": [`widgets::chip`] is a
/// *reading*, and the stabiliser was one until it became a rail. That is why
/// there is still no filled dropdown — the fill would have to be learnt as
/// meaning one thing here and another in Settings, where the chips remain.
/// What says this opens is the chevron, which is what says it everywhere else
/// too.
fn selection_mode_switch(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    let label = ed.ui.selection_mode.label();
    widgets::dropdown(ui, p, widgets::Dropdown::new(label), |ui| {
        for mode in SelectionMode::ALL {
            if ui
                .selectable_label(ed.ui.selection_mode == mode, mode.label())
                .clicked()
            {
                ed.ui.selection_mode = mode;
                // A half-drawn outline belongs to the mode that was drawing it,
                // and a polygon left open under the lasso would take its next
                // click as a vertex.
                ed.cancel_selection_draft();
            }
        }
    });
}

/// What a new shape does to the selection already standing: four marks, of
/// which exactly one is on.
///
/// **Four [`widgets::icon_toggle`]s rather than a [`widgets::segmented`] or a
/// fifth [`widgets::dropdown`].** The segmented picker takes the whole of the
/// width it is given, which on a single unwrapped row is everything left, and
/// it is a stacked control that belongs in a panel. A dropdown would be a
/// second one on this strip, beside the mode's, with nothing but its own word
/// to say which question it answers. Four marks laid out side by side, one lit,
/// is what Photoshop, GIMP, Krita and Affinity all draw for exactly this — and
/// it is the one spelling that fits a 36-point row and says the whole set at a
/// glance rather than one member of it.
///
/// The toggles are read-only about each other: clicking the one already on
/// leaves it on, because these are four answers to one question and there is no
/// state with none of them chosen. That is the difference between this and the
/// layer flags, which are the same widget and are genuinely independent.
fn selection_op_switch(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    for op in SelectionOp::ALL {
        let mark = match op {
            SelectionOp::Replace => Icon::SelectReplace,
            SelectionOp::Add => Icon::SelectAdd,
            SelectionOp::Subtract => Icon::SelectSubtract,
            SelectionOp::Intersect => Icon::SelectIntersect,
        };
        // The tooltip carries the name as well as what it does: the mark is a
        // picture of the *result*, which is legible once you know the four and
        // guessable at best before that.
        let tip = format!("{} · {}", op.label(), op.hint());
        if widgets::icon_toggle(ui, p, mark, ed.ui.selection_op == op, true, &tip) {
            ed.ui.selection_op = op;
        }
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
                "layout edit · nothing you draw changes; panels are the only \
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
                    "{where_it_lives}{} · panels locked · Window, Customise layout",
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

/// Height the brush editor's footer claims at the bottom of the dialog.
///
/// Named because two places have to agree on it: [`brush_editor`], which hands
/// the body whatever is left over, and `brushlib::save_row`, which must not
/// draw more than this — the two transient things that used to make it taller,
/// the notice and the "Save this brush as" field, are `brushlib::save_bar`'s
/// now and sit above the body instead.
///
/// **Two** of `theme::apply`'s 6 points of item spacing — egui appends one
/// after every allocated rect, including the zero-height one that pushes the
/// footer down and the one-point hairline — plus that hairline, the ten points
/// under it, and one 22-point line of buttons. 12 + 1 + 10 + 22.
///
/// `the_brush_editor_is_one_size_whatever_section_is_in_front` is what holds
/// this to what the footer actually costs, which is the half of this that is
/// easy to get wrong: a reserve one point short is a footer that overruns the
/// frame, and the dialog grows by that point rather than the row being clipped.
/// That test therefore has to install Umber's **own** style — this was 39 while
/// it did not, because egui's default `item_spacing.y` is 3 where Umber's is 6,
/// and the dialog was six points taller in a running window than in the test
/// that was supposed to be pinning it.
const BRUSH_EDITOR_FOOTER: f32 = 45.0;

/// Breathing space between the body and the footer's hairline.
const BRUSH_EDITOR_GAP: f32 = 14.0;

/// The brush editor, matching the design's dialog.
///
/// Holds every brush parameter that is not on the options strip, so the strip
/// can stay short. Edits apply live — there is no OK or Cancel, because a paint
/// app should let you see a change as you make it.
///
/// **One size, whatever section is in front**, which is the settings dialog's
/// rule and was arrived at the same way: each section used to size the modal,
/// so moving from Tip to Inputs grew it by a third and moving back shrank it —
/// with the tab strip you had just clicked sliding out from under the pointer,
/// because a modal is centred and a modal that changes height moves both edges.
/// A header, one vertical `ScrollArea` with `auto_shrink([false, false])` and
/// an explicit max height, a footer. The scroll area claiming its space
/// whatever is in it is the whole of the fix; being vertical, it also cannot
/// grow a horizontal bar out of a section that overruns.
///
/// **Sections must not add scroll areas of their own** — nested scrolling makes
/// the wheel mean two things — and must not be given the dialog's height to
/// size themselves against, or this comes straight back.
///
/// `pub(crate)` because `stamplib` opens it: a stamp imported into the library
/// is one somebody is about to put on a brush.
pub(crate) fn brush_editor(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    if !ed.ui.brush_editor_open {
        return;
    }

    let name = ed
        .active_preset
        .and_then(|i| ed.presets.get(i))
        .map(|preset| preset.name.clone())
        .unwrap_or_else(|| "Brush".to_string());

    // Clamped to the window, because a modal taller than the screen has its
    // footer — the only way to keep an edit — off the bottom of it. The clamp
    // reads the window and never the section, so it cannot reintroduce the
    // thing above.
    let available = root.ctx().content_rect().size();
    let [full_width, full_height] = metrics::BRUSH_EDITOR;
    let width = full_width.min(available.x - 48.0).max(420.0);
    let height = full_height.min(available.y - 48.0).max(320.0);

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
            ui.set_width(width);
            ui.set_height(height);

            ui.horizontal(|ui| {
                // The close mark first, then the title painted into what it
                // leaves — `status_bar`'s order, and the same reason the
                // footer's is that way round. The title carries the brush's
                // *name*, which is text somebody else typed and therefore the
                // one thing on this line with no length anybody can promise; as
                // a label it would extend, and a long enough name would be
                // drawn straight over the close mark.
                let band = ui.max_rect();
                // The close mark's own height, so the row is the same whatever
                // the title elides to.
                ui.allocate_exact_size(vec2(0.0, 18.0), Sense::hover());
                let used = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, p, Icon::Close, true, "Close") {
                            ed.ui.brush_editor_open = false;
                        }
                    })
                    .response
                    .rect
                    .left();
                let painter = ui.painter();
                painter.text(
                    pos2(band.left(), band.top() + 9.0),
                    Align2::LEFT_CENTER,
                    widgets::elide(
                        painter,
                        &format!("Edit brush · {name}"),
                        text::CONTROL,
                        used - band.left() - 8.0,
                    ),
                    FontId::proportional(text::CONTROL),
                    p.text_strong,
                );
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

            // Above the body, so the space they take comes off what scrolls
            // rather than off the dialog's height. See `brushlib::save_bar`.
            crate::brushlib::save_bar(ui, p, ed);

            let body = (ui.available_height() - BRUSH_EDITOR_FOOTER - BRUSH_EDITOR_GAP).max(0.0);
            egui::ScrollArea::vertical()
                // A scroll position per section. One shared position would
                // carry Inputs' offset onto Tip, which is short enough to be
                // left showing nothing.
                .id_salt(("brush-editor", ed.ui.brush_tab))
                .max_height(body)
                .auto_shrink([false, false])
                .show(ui, |ui| match ed.ui.brush_tab {
                    BrushTab::Tip => brush_editor_tip(ui, p, ed),
                    BrushTab::Dynamics => brush_editor_dynamics(ui, p, ed),
                    BrushTab::Inputs => brush_editor_inputs(ui, p, ed),
                    BrushTab::Scatter => brush_editor_scatter(ui, p, ed),
                    BrushTab::Texture => brush_editor_texture(ui, p, ed),
                    BrushTab::Blending => brush_editor_blending(ui, p, ed),
                });

            // The design's footer: name what you have made, or write it back
            // over the brush you started from. Pushed to the bottom by whatever
            // is left, exactly as the settings dialog's is.
            let left = (ui.available_height() - BRUSH_EDITOR_FOOTER).max(0.0);
            ui.allocate_space(vec2(0.0, left));
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
///
/// `pub(crate)` because the Brush tweaks module asks the same question about
/// the same rail. A second copy of the OR would be two readings that can
/// disagree — a rail live in one panel and dead in the other.
pub(crate) fn has_angle(ed: &Editor) -> bool {
    ed.brush.dab_has_angle() || ed.tip.is_some()
}

/// A percentage readout, which most of these sliders share.
fn percent(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

/// A caption under a control, explaining why it is off or what it does.
///
/// `pub(crate)` for the reason [`has_angle`] is: the Brush tweaks module draws
/// the same sentence under the same disabled rails, and a second `RichText`
/// with the same size and colour written out is a caption that drifts.
pub(crate) fn caption(ui: &mut egui::Ui, p: &Palette, line: &str) {
    ui.label(egui::RichText::new(line).size(10.0).color(p.text_dim));
}

/// The design's Tip section: a two-column grid of the dab's own properties.
fn brush_editor_tip(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;
    // The stamp itself is `brushlib`'s: every way of changing it — the list of
    // masks, the file dialog, the canvas to draw one on — is a reach into the
    // user's library, and this file paints the sliders.
    let stamped = crate::brushlib::tip_row(ui, p, ed);
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
        // The same range the tool options strip's rail uses, from the one
        // constant, so the two controls cannot disagree about what full
        // stabilisation is.
        widgets::slider_row(
            &mut c[1],
            p,
            "Stabilisation",
            &mut ed.brush.stabilization,
            0.0..=Brush::MAX_STABILIZATION,
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
            "Angle needs an elliptical dab or a bitmap tip. Lower Roundness \
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
            &mut ed.brush.pressure_size,
            &mut ed.brush.size_curve,
            Some(("Min size", &mut ed.brush.min_size_ratio)),
        );
        curve_column(
            &mut c[1],
            p,
            "Pressure → opacity",
            &mut ed.brush.pressure_opacity,
            &mut ed.brush.opacity_curve,
            None,
        );
        curve_column(
            &mut c[2],
            p,
            "Pressure → hardness",
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
            "Nothing but pressure drives this brush. That is the fast path: no \
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
        // No leading mark on either of these two: "Drives" and "Driven by" name
        // an abstraction, and a glyph invented for one would have to be learnt
        // before it said anything.
        let label = edited.target.label();
        widgets::dropdown(
            &mut c[0],
            p,
            widgets::Dropdown::new(label).width(widgets::DropdownWidth::Fill),
            |ui| {
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
            },
        );

        c[1].label(
            egui::RichText::new("Driven by")
                .size(text::SMALL)
                .color(p.text_dim),
        );
        let label = edited.input.label();
        widgets::dropdown(
            &mut c[1],
            p,
            widgets::Dropdown::new(label).width(widgets::DropdownWidth::Fill),
            |ui| {
                for input in DabInput::ALL {
                    if ui
                        .selectable_label(input == edited.input, input.label())
                        .clicked()
                    {
                        edited.input = input;
                    }
                }
            },
        );
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
        // As wide as the curve panel above it rather than as wide as the
        // column: the two are one control read downwards, and a picker running
        // past the square it belongs to would break that.
        widgets::dropdown(
            &mut c[0],
            p,
            widgets::Dropdown::new(current).width(widgets::DropdownWidth::Exact(size)),
            |ui| {
                for (name, preset) in ResponseCurve::PRESETS {
                    if ui
                        .selectable_label(edited.curve.preset_name() == Some(name), name)
                        .clicked()
                    {
                        edited.curve = preset;
                    }
                }
            },
        );

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
            "How fast the pointer is moving right now. It reacts within a \
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
            "Which way the stroke is heading, over half a turn. A line pulled \
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
         size. Angle jitter needs an elliptical dab to show.\n\nSpeed lead \
         throws each dab along the direction of travel, a tenth of a second's \
         worth of it per unit, so a fast stroke runs ahead of the cursor and a \
         slow one sits on it.",
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

        paper_picker(ui, p, ed);

        ui.horizontal(|ui| {
            paper_preview(ui, p, ed);
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

/// Which paper the brush bites through: Umber's three, then the user's own,
/// then the way into the library.
///
/// **One dropdown where this used to be a segmented control**, and that is the
/// change a texture library forces rather than a restyling. A segmented control
/// is a row of every choice there is, which is right for a closed set of three
/// and cannot be right for a list that grows — and offering the three as
/// segments *and* the user's own as a second control would be two spellings of
/// one question, which is the rule `widgets::dropdown` exists to keep. The
/// shipped three still come first, still in `GrainPattern::ALL`'s order.
fn paper_picker(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    // `Brush::grain_pattern` is what a `None` name means, so the label has to
    // read the same two-step `Editor::paper_tile` does.
    let label = match ed.paper_name.as_deref() {
        Some(name) => name.to_owned(),
        None => ed.brush.grain_pattern.label().to_owned(),
    };
    let mut chosen: Option<Option<String>> = None;
    let mut browse = false;
    widgets::dropdown(
        ui,
        p,
        widgets::Dropdown::new(&label).width(widgets::DropdownWidth::Fill),
        |ui| {
            for pattern in GrainPattern::ALL {
                let selected = ed.paper_name.is_none() && ed.brush.grain_pattern == pattern;
                if ui.selectable_label(selected, pattern.label()).clicked() {
                    ed.brush.grain_pattern = pattern;
                    chosen = Some(None);
                }
            }
            if !ed.papers.is_empty() {
                ui.separator();
            }
            for name in ed.papers.keys() {
                let selected = ed.paper_name.as_deref() == Some(name.as_str());
                if ui.selectable_label(selected, name).clicked() {
                    chosen = Some(Some(name.clone()));
                }
            }
            // A name the brush carries that this list has not already drawn.
            // Two of them, and they must not be told apart by anything but the
            // **resolver**: `Editor::paper_tile` reads the user's library and
            // then the shipped table, so a name is missing only when *it*
            // answers nothing. Testing `ed.papers` alone said "not in your
            // library" about a shipped tile the brush was visibly painting
            // through — three modules with three definitions of "resolves",
            // two of them on screen together.
            if let Some(name) = ed
                .paper_name
                .as_deref()
                .filter(|name| !ed.papers.contains_key(*name))
            {
                ui.separator();
                let _ = match ed.paper_tile() {
                    // One Umber ships, named rather than chosen through the
                    // enum — an imported preset can do that.
                    Some(_) => ui.selectable_label(true, format!("{name} · shipped with Umber")),
                    // Nothing behind it at all: a library copied without its
                    // `papers/` directory. Said out loud rather than left out,
                    // or the picker would name one of Umber's three for a brush
                    // that is painting flat. See `BrushPreset::paper`.
                    None => ui.selectable_label(true, format!("{name} · not in your library")),
                };
            }
            ui.separator();
            if ui.selectable_label(false, "Browse papers…").clicked() {
                browse = true;
            }
        },
    );
    if let Some(name) = chosen {
        ed.set_paper(name);
    }
    if browse {
        crate::stamplib::open(ui.ctx(), crate::stamplib::Kind::Papers);
    }
}

/// Widest a tile is downsampled to for the 56-point thumbnail. A paper is at
/// most a few hundred texels and this is more than the square can show.
const PAPER_PREVIEW_TEXELS: u32 = 96;

/// A thumbnail of the paper in hand, drawn **tiled**.
///
/// Two copies across and two down, which is the one thing a single square
/// cannot show: the grain is anchored to the document and wraps across it, so a
/// tile whose edges do not meet draws a grid over the canvas — invisible in one
/// copy and unmissable the moment it meets itself. Umber's own three join by
/// construction; a picture somebody imported may not.
///
/// Cached in egui's temporary store and validated by `Arc` identity, exactly as
/// `brushlib`'s tip preview is: the modal redraws every frame and this would
/// otherwise upload a texture on each of them. Its own slot and its own id —
/// the browser's rows draw the same tiles through a cache of their own, because
/// two consumers of a one-slot cache evict each other's live texture.
fn paper_preview(ui: &mut egui::Ui, p: &Palette, ed: &Editor) {
    let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
    ui.painter().rect_filled(rect, metrics::RADIUS, p.chrome);

    let Some(tile) = ed.paper_tile() else {
        // A name with nothing behind it. Left as the empty well rather than
        // filled with one of the shipped tiles, because painting flat is
        // exactly what the brush is about to do.
        return;
    };
    let id = egui::Id::new("brush-paper-preview");
    let cached: Option<(Arc<TipMask>, egui::TextureHandle)> = ui.ctx().data(|d| d.get_temp(id));
    let texture = match cached {
        Some((held, texture)) if Arc::ptr_eq(&held, &tile) => texture,
        _ => {
            let texture = ui.ctx().load_texture(
                "brush-paper",
                widgets::tip_image(&tile, p.text_strong, PAPER_PREVIEW_TEXELS),
                egui::TextureOptions::LINEAR,
            );
            ui.ctx()
                .data_mut(|d| d.insert_temp(id, (Arc::clone(&tile), texture.clone())));
            texture
        }
    };

    // Four separate draws, not one with a uv range past 1: egui's textures are
    // clamped rather than repeating, so the wide uv would magnify the top-left
    // quarter and smear its edge row over the rest — which is the one thing a
    // square that exists to show a join must not do.
    crate::stamplib::tiled(
        ui,
        texture.id(),
        rect.shrink(2.0),
        crate::stamplib::Kind::Papers.repeats(),
    );
}

/// Colour pickup — a brush that carries what it finds on the canvas.
fn brush_editor_blending(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;

    // How the stroke combines with the layer it lands on.
    //
    // **Not drawn for an eraser**, rather than drawn and disabled. A blend mode
    // combines a colour with what is under it and an eraser deposits none, so
    // there is no setting here that is merely switched off — there is nothing
    // for the control to be about. That is `Brush::blend_applies`, and it is
    // the same reading the Tip section's Angle slider takes of a round dab.
    //
    // `widgets::dropdown`, and the same `BlendMode::ALL` the layer picker
    // walks: one gesture, one list, and the same arithmetic behind both.
    if ed.brush.blend_applies() {
        ui.label(
            egui::RichText::new("Blend mode")
                .size(text::SMALL)
                .color(p.text_dim),
        );
        // Alone on its line, so it fills it — `DropdownWidth`'s own rule.
        let label = ed.brush.blend.label();
        widgets::dropdown(
            ui,
            p,
            widgets::Dropdown::new(label).width(widgets::DropdownWidth::Fill),
            |ui| {
                for mode in BlendMode::ALL {
                    ui.selectable_value(&mut ed.brush.blend, mode, mode.label());
                }
            },
        );
        caption(
            ui,
            p,
            "How the finished stroke combines with the layer under it: the \
             same five modes a layer has, and the same maths. Applied once, \
             when the stroke is put down, so a mark that crosses itself \
             multiplies with the paint beneath it rather than with itself.",
        );
    }

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
///
/// Took a `salt` for the preset picker's id while that was an `egui::ComboBox`,
/// which needs one given to it. [`widgets::dropdown`] is allocated out of the
/// `Ui` it is drawn in, so it takes its id from where it lands — and each of
/// these columns is its own `Ui`.
fn curve_column(
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
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
        // As wide as the curve panel above it, for the reason the Inputs tab's
        // copy of this gives.
        widgets::dropdown(
            ui,
            p,
            widgets::Dropdown::new(current).width(widgets::DropdownWidth::Exact(size)),
            |ui| {
                for (name, preset) in ResponseCurve::PRESETS {
                    if ui
                        .selectable_label(curve.preset_name() == Some(name), name)
                        .clicked()
                    {
                        *curve = preset;
                    }
                }
            },
        );

        if let Some((label, value)) = min {
            ui.add_space(8.0);
            widgets::slider_row(ui, p, label, value, 0.0..=1.0, false, percent);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::UiActions;
    use crate::brushlib;
    use crate::editor::{BrushTab, Editor};
    use crate::shortcuts::{self, Action};
    use crate::theme::{Palette, ThemeKind, metrics};
    use egui::{Rect, pos2, vec2};

    /// Which of the three menus that carry commands is being drawn.
    ///
    /// The bodies are drawn straight into a plain `Ui` rather than through a
    /// real popup: opening one headlessly means synthesising a click on a menu
    /// button whose rectangle nothing reports, and the rows are what these
    /// tests are about. It is the same function the menu bar calls, so nothing
    /// stands in for anything.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Menu {
        File,
        Edit,
        View,
    }

    impl Menu {
        /// The menu a command of this category belongs in.
        ///
        /// Read off [`Action::category`] rather than listed here, so an action
        /// added to a category is an action these tests immediately demand a
        /// row for. "Image" is the two canvas flips, which sit in File beside
        /// Canvas settings rather than under an Image menu of their own.
        fn of_category(category: &str) -> Option<Menu> {
            match category {
                "File" | "Image" => Some(Menu::File),
                "Edit" => Some(Menu::Edit),
                "View" => Some(Menu::View),
                _ => None,
            }
        }

        fn draw(self, ui: &mut egui::Ui, ed: &mut Editor, actions: &mut UiActions) {
            match self {
                Menu::File => super::file_menu(ui, ed, actions),
                Menu::Edit => super::edit_menu(ui, ed, actions),
                Menu::View => super::view_menu(ui, actions),
            }
        }
    }

    /// A piece of text a menu drew, and where it drew it.
    #[derive(Clone, Debug)]
    struct Drawn {
        text: String,
        at: egui::Pos2,
    }

    /// Every string a shape tree paints, with its centre.
    ///
    /// A `Shape::Vec` is what a widget's own painting comes back as, so this
    /// recurses rather than reading the top level.
    fn strings_in(shape: &egui::Shape, out: &mut Vec<Drawn>) {
        match shape {
            egui::Shape::Text(text) => out.push(Drawn {
                text: text.galley.text().to_owned(),
                at: text.pos + text.galley.size() * 0.5,
            }),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    strings_in(shape, out);
                }
            }
            _ => {}
        }
    }

    /// A context with Umber's own font and spacing, which is what decides how
    /// wide a row is and therefore whether its shortcut text is drawn at all.
    fn menu_context() -> (egui::Context, Palette) {
        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx, &palette);
        (ctx, palette)
    }

    fn menu_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 700.0))),
            events,
            ..Default::default()
        }
    }

    /// Draw a menu's rows and report every string on them.
    ///
    /// Twice, because the first pass through a fresh context builds the font
    /// atlas and text measured against a half-built one is not the text that
    /// settles.
    fn menu_strings(menu: Menu, ed: &mut Editor) -> Vec<Drawn> {
        let (ctx, _) = menu_context();
        let mut drawn = Vec::new();
        for _ in 0..2 {
            drawn.clear();
            let mut actions = UiActions::default();
            let output = ctx.run_ui(menu_input(Vec::new()), |ui| {
                // A narrow column laid out top-down, which is the shape a menu
                // popup gives its contents. A row's width decides whether egui
                // finds room for the shortcut text beside the label.
                ui.vertical(|ui| {
                    ui.set_max_width(260.0);
                    menu.draw(ui, ed, &mut actions);
                });
            });
            for clipped in &output.shapes {
                strings_in(&clipped.shape, &mut drawn);
            }
        }
        drawn
    }

    /// Press and release on the row carrying `label`, and report what the menu
    /// asked the caller for.
    ///
    /// A disabled row swallows the click, so this is also how "that row is
    /// live" is asserted without reading a colour off a shape.
    fn click_menu_row(menu: Menu, ed: &mut Editor, label: &str) -> UiActions {
        let (ctx, _) = menu_context();
        let mut actions = UiActions::default();
        let mut at = None;
        // Lay out, aim, press, release. The press and the release are separate
        // frames because that is what a hand does, and egui settles a click on
        // the release.
        for frame in 0..4 {
            let events = match (frame, at) {
                (2, Some(at)) => vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                (3, Some(at)) => vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                }],
                _ => Vec::new(),
            };
            actions = UiActions::default();
            let output = ctx.run_ui(menu_input(events), |ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(260.0);
                    menu.draw(ui, ed, &mut actions);
                });
            });
            let mut drawn = Vec::new();
            for clipped in &output.shapes {
                strings_in(&clipped.shape, &mut drawn);
            }
            if frame == 1 {
                at = Some(
                    drawn
                        .iter()
                        .find(|d| d.text == label)
                        .unwrap_or_else(|| {
                            panic!("the {menu:?} menu draws no row called {label:?}")
                        })
                        .at,
                );
            }
        }
        actions
    }

    /// Every command filed under File, Image, Edit or View has a menu row.
    ///
    /// Six did not. `Deselect`, `Copy`, `Cut` and `Paste` were bound and
    /// dispatched and appeared in no menu — Paste in no control anywhere, since
    /// the selection's canvas strip is drawn only while a selection is live, so
    /// a picture copied in another application could only be pasted by somebody
    /// who already knew the key. `ZoomIn` and `ZoomOut` were bound with no row
    /// either.
    ///
    /// The set is read off [`Action::category`] rather than written out here,
    /// which is what makes this a test rather than a second copy of the menu:
    /// filing a new command under one of those four categories fails this until
    /// it has somewhere to be clicked.
    #[test]
    fn every_file_edit_and_view_command_has_a_menu_row() {
        for menu in [Menu::File, Menu::Edit, Menu::View] {
            let mut ed = Editor::default();
            let drawn = menu_strings(menu, &mut ed);
            for action in Action::ALL {
                if Menu::of_category(action.category()) != Some(menu) {
                    continue;
                }
                assert!(
                    drawn.iter().any(|d| d.text == action.label()),
                    "the {menu:?} menu has no row for {:?}; it draws {:?}",
                    action.label(),
                    drawn.iter().map(|d| &d.text).collect::<Vec<_>>()
                );
            }
        }
    }

    /// A menu row standing for a command is named as the command is named.
    ///
    /// `Action::FitView` is "Fit to view" on the Shortcuts page and the View
    /// menu drew it as "Fit to window": one command with two names, in an
    /// interface whose other view of it has a search field. The label now comes
    /// from the action and the call site cannot supply one.
    ///
    /// A row is found by the **chord** it draws rather than by its label, which
    /// is the whole point — the chord is put there by `menu_item` from the
    /// action, so it says a row for that command exists without assuming
    /// anything about what the row is called. Chords shared by two commands are
    /// skipped, since either label would satisfy them.
    #[test]
    fn a_menu_row_standing_for_a_command_carries_the_commands_own_name() {
        let bound: Vec<(Action, String)> = Action::ALL
            .into_iter()
            .filter_map(|a| shortcuts::first_chord(a).map(|c| (a, c)))
            .collect();
        let mut checked = 0;
        for menu in [Menu::File, Menu::Edit, Menu::View] {
            let mut ed = Editor::default();
            let drawn = menu_strings(menu, &mut ed);
            for (action, chord) in &bound {
                if bound.iter().filter(|(_, c)| c == chord).count() != 1 {
                    continue;
                }
                if !drawn.iter().any(|d| &d.text == chord) {
                    continue;
                }
                assert!(
                    drawn.iter().any(|d| d.text == action.label()),
                    "the {menu:?} menu draws {chord} on a row that is not called {:?}; \
                     it draws {:?}",
                    action.label(),
                    drawn.iter().map(|d| &d.text).collect::<Vec<_>>()
                );
                checked += 1;
            }
        }
        // Otherwise a menu that drew no chords at all would pass in silence.
        assert!(checked >= 8, "only {checked} rows carried a chord to check");
    }

    /// Paste is offered with Umber's own clipboard empty.
    ///
    /// **`Editor::clipboard` being `None` is not "nothing to paste", and
    /// disabling the row on it is the mistake this guards.** What a paste puts
    /// down is `sysclip::decide`'s answer, and that reads the *desktop's*
    /// clipboard as well: a screenshot taken in another application pastes
    /// perfectly well while Umber holds nothing. Nor could the row ask — reading
    /// the desktop's clipboard blocks, and this is drawn every frame the menu is
    /// open.
    ///
    /// Asserted by clicking the row rather than by reading a colour off it: a
    /// disabled row swallows the click, so the request coming back is the proof.
    #[test]
    fn paste_is_offered_even_with_umbers_own_clipboard_empty() {
        let mut ed = Editor::default();
        assert!(
            ed.clipboard.is_none(),
            "a fresh editor is supposed to hold no clip"
        );
        let actions = click_menu_row(Menu::Edit, &mut ed, Action::Paste.label());
        assert!(
            actions.paste,
            "clicking Paste with an empty clip asked for nothing"
        );
    }

    /// Copy is never refused and Cut is refused by a locked layer.
    ///
    /// The pair the selection's canvas strip already draws that way, and the
    /// gate `App::cut_selection` already keeps: a cut takes pixels off the
    /// layer, a copy writes nothing. Two rows in one test because the risk is
    /// getting them the same way round.
    #[test]
    fn cut_answers_to_the_lock_and_copy_does_not() {
        let mut ed = Editor::default();
        ed.layers.active_mut().locked = true;
        let actions = click_menu_row(Menu::Edit, &mut ed, Action::Cut.label());
        assert!(
            !actions.cut_selection,
            "Cut was live on a locked layer, which `cut_selection` would then refuse"
        );
        let actions = click_menu_row(Menu::Edit, &mut ed, Action::Copy.label());
        assert!(
            actions.copy_selection,
            "Copy was refused by a lock, though it writes nothing"
        );
    }

    /// What the footer is asked to hold, beyond the six sections.
    ///
    /// The two that are not the ordinary case are the ones that used to make it
    /// taller or wider: a name field armed takes the buttons off the row, and a
    /// brush of the user's own replaces one button with two — the second
    /// carrying a name somebody else chose, which is the width nobody here can
    /// bound.
    #[derive(Clone, Copy, Debug)]
    enum Footer {
        /// A shipped brush in hand: one "Save brush…" button.
        Shipped,
        /// One of the user's own, with a name long enough to be a problem:
        /// `Update "<name>"` and `Save as new…` on one line.
        Yours,
        /// The "Save this brush as" field up, which takes the buttons away.
        Naming,
    }

    /// Every section of the brush editor in every footer state, and how big the
    /// dialog came out.
    fn section_sizes() -> Vec<(BrushTab, Footer, egui::Vec2)> {
        let input = egui::RawInput {
            // Comfortably larger than the dialog in both directions, so the
            // clamp to the window is not what is being measured.
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1400.0, 900.0))),
            ..Default::default()
        };
        let palette = Palette::of(ThemeKind::Graphite);

        let mut out = Vec::new();
        for tab in [
            BrushTab::Tip,
            BrushTab::Dynamics,
            BrushTab::Inputs,
            BrushTab::Scatter,
            BrushTab::Texture,
            BrushTab::Blending,
        ] {
            for footer in [Footer::Shipped, Footer::Yours, Footer::Naming] {
                // A context per measurement, not one shared. egui remembers an
                // area's rect between frames, and a modal that has already been
                // laid out taller stays taller — so a shared context can report
                // every case agreeing when what they agree on is the largest.
                let ctx = egui::Context::default();
                // Umber's own font and spacing, not egui's defaults. This is
                // what `docshot::Stage::shoot` does and what `ui::draw` does,
                // and without it the whole measurement is of a style Umber
                // never draws with: egui's `item_spacing.y` is 3 where
                // `theme::apply` sets 6, so a footer reserve calibrated here
                // would be six points short in a running window — which is
                // exactly the "reserve that does not match what the footer
                // costs" this test exists to prevent.
                crate::theme::install_fonts(&ctx);
                crate::theme::apply(&ctx, &palette);
                let mut ed = Editor::default();
                ed.ui.brush_editor_open = true;
                ed.ui.brush_tab = tab;
                if matches!(footer, Footer::Yours) {
                    // Past the shipped library, which is what `Index::is_user`
                    // reads — so the footer offers Update as well as Save as
                    // new. The name is deliberately absurd: it is the one piece
                    // of text in this dialog that Umber did not write.
                    let mut mine = ed.presets[0].clone();
                    mine.id = "user/long".to_owned();
                    mine.name = "A stupendously long name somebody typed into the field".to_owned();
                    ed.presets.push(mine);
                    ed.active_preset = Some(ed.presets.len() - 1);
                }
                let naming = matches!(footer, Footer::Naming)
                    .then_some("Another name of quite unreasonable length");
                let mut size = egui::Vec2::ZERO;
                // Three passes and the last is the one read: the first through
                // a fresh context builds the font atlas, and a modal lays
                // itself out against the previous frame's screen — text
                // measured against a half-built atlas is not the height it
                // settles at.
                for _ in 0..3 {
                    // Seeded rather than loaded, so this never touches the
                    // brush library of whoever is running it.
                    brushlib::seed_broken_library(&ctx, &ed, "no library", naming);
                    let _ = ctx.run_ui(input.clone(), |ui| {
                        super::brush_editor(ui, &palette, &mut ed);
                    });
                    size = ctx
                        .memory(|m| m.area_rect(egui::Id::new("brush-editor")))
                        .expect("the brush editor draws an area")
                        .size();
                }
                out.push((tab, footer, size));
            }
        }
        out
    }

    /// The brush editor is one size, whatever section is in front.
    ///
    /// Each section used to size the modal: Inputs is a list of arbitrary
    /// length where Tip is a fixed grid, so moving between them grew and shrank
    /// the dialog — and a modal is centred, so a change of height moves *both*
    /// edges and takes the tab strip out from under the pointer that has just
    /// clicked it. It is now a header, one vertical `ScrollArea` with
    /// `auto_shrink([false, false])` and an explicit max height, and a footer,
    /// which is the settings dialog's rule applied here.
    ///
    /// The absolute size is asserted as well as the agreement between the six,
    /// because the two are different failures: six sections that agree on the
    /// wrong number is a header or a footer overrunning what the frame reserved
    /// for it, and that is the same on every tab — so equality alone would pass
    /// straight over it.
    #[test]
    fn the_brush_editor_is_one_size_whatever_section_is_in_front() {
        let sizes = section_sizes();
        let [width, height] = metrics::BRUSH_EDITOR;
        // The frame's `Margin::same(18)` and its one-point border, on all four
        // sides: 18 + 1 either way.
        let expected = vec2(width + 38.0, height + 38.0);
        for (tab, footer, size) in &sizes {
            // Within a tenth of a point rather than exactly. The height is the
            // frame's own arithmetic over a sum of `add_space`s and row
            // heights, so the last bit of the f32 differs between sections; a
            // tenth of a point is far below anything that could be a layout
            // changing size and far above that.
            let off = (*size - expected).abs();
            assert!(
                off.x < 0.1 && off.y < 0.1,
                "the brush editor is {:.4} × {:.4} on {tab:?} with {footer:?}, \
                 not {:.4} × {:.4}",
                size.x,
                size.y,
                expected.x,
                expected.y
            );
        }
    }

    /// The brush editor, on each of its six sections.
    ///
    /// Written rather than asserted for the reason `layers_panel_preview` is:
    /// the test above says the frame is one size, and only a picture says
    /// whether the section inside it is laid out sensibly at that size — how
    /// much of the frame a short section leaves empty, and whether a long one
    /// scrolls where it should.
    ///
    /// ```sh
    /// cargo test -p umber-app brush_editor_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn brush_editor_preview() {
        use crate::docshot;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/brush-editor");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        // The dialog is 560 × 600 and clamps to `available - 48`, so a field 48
        // larger either way holds it at full size with an even margin of dimmed
        // backdrop around it.
        let [w, h] = metrics::BRUSH_EDITOR;
        let field = vec2(w + 48.0, h + 48.0);
        // The six sections, then the footer state the assertion above cannot
        // speak for. `ui.set_width` bounds the dialog, so a footer whose
        // buttons are wider than the room left beside the note does not grow
        // the modal — it is drawn over the note, and only a picture says so.
        // A brush of the user's own is the case: it replaces one button with
        // two, and the second carries a name Umber did not write.
        let sections = [
            BrushTab::Tip,
            BrushTab::Dynamics,
            BrushTab::Inputs,
            BrushTab::Scatter,
            BrushTab::Texture,
            BrushTab::Blending,
        ];
        for (n, (tab, long_name)) in sections
            .into_iter()
            .map(|tab| (tab, false))
            .chain(std::iter::once((BrushTab::Tip, true)))
            .enumerate()
        {
            let mut ed = Editor::default();
            ed.layout = crate::dock::Layout::default();
            ed.ui.brush_editor_open = true;
            ed.ui.brush_tab = tab;
            if long_name {
                let mut mine = ed.presets[0].clone();
                mine.id = "user/long".to_owned();
                mine.name = "A stupendously long name somebody typed into the field".to_owned();
                ed.presets.push(mine);
                ed.active_preset = Some(ed.presets.len() - 1);
            }
            let palette = Palette::with_accent(ed.ui.theme, ed.ui.accent);
            let image = stage.shoot(field, 1.5, &palette, palette.backdrop, |root| {
                brushlib::seed_broken_library(root.ctx(), &ed, "no library", None);
                super::brush_editor(root, &palette, &mut ed);
            });
            let name = if long_name {
                "7-footer-long-name.png".to_owned()
            } else {
                format!("{}-{tab:?}.png", n + 1).to_lowercase()
            };
            docshot::write_png(&dir.join(name), &image).expect("write the png");
        }
        println!("wrote 7 shots to {}", dir.display());
    }

    /// The tool options strip, at three widths, with the brush in hand and then
    /// with each of the two tools whose strip is a sentence.
    ///
    /// The stabiliser is a third `widgets::inline_slider` beside size and
    /// opacity rather than the `widgets::chip` it was, and the two things worth
    /// looking at are whether three rails on a 36-point strip read as three
    /// controls and whether the budget that drops them one at a time drops them
    /// where it says it does.
    ///
    /// **Pan and Zoom are shot for a different reason**: the strip is a single
    /// unwrapped row, so a sentence too long for it does not reflow, it runs off
    /// the right edge. Their second half is behind
    /// [`strip_budget::NAVIGATE_MORE`], and only a picture says whether the
    /// budget is the right size — an assertion about a string's length would be
    /// a claim about a font.
    ///
    /// ```sh
    /// cargo test -p umber-app options_strip_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn options_strip_preview() {
        use crate::docshot;
        use crate::editor::Tool;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/brush-editor");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        let mut written = 0;
        for tool in [Tool::Brush, Tool::Pan, Tool::Zoom] {
            // The three rail widths, plus the pair either side of where Zoom's
            // second sentence has to come off: 740 is the narrowest strip that
            // holds it whole and 720 is the widest that drops it, so a budget
            // edited to a number that overruns shows up as text running off the
            // right of one of these two rather than as nothing at all.
            for (n, width) in [900.0_f32, 740.0, 720.0, 560.0, 380.0]
                .into_iter()
                .enumerate()
            {
                let mut ed = Editor::default();
                ed.layout = crate::dock::Layout::default();
                ed.ui.tool = tool;
                let palette = Palette::with_accent(ed.ui.theme, ed.ui.accent);
                let field = vec2(width, metrics::OPTIONS_STRIP);
                let image = stage.shoot(field, 2.0, &palette, palette.chrome, |root| {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(metrics::STRIP_PAD, 0))
                        .show(root, |ui| {
                            ui.set_height(metrics::OPTIONS_STRIP);
                            super::options_strip(ui, &palette, &mut ed);
                        });
                });
                let name = format!("strip-{tool:?}-{}-{width:.0}.png", n + 1).to_lowercase();
                docshot::write_png(&dir.join(name), &image).expect("write the png");
                written += 1;
            }
        }
        println!("wrote {written} strips to {}", dir.display());
    }

    /// The canvas scrollbars in the three states that matter, over a canvas
    /// region the size a real one is.
    ///
    /// Written rather than asserted for the reason `layers_panel_preview` is.
    /// The question drawing the bars always raises is not a geometry one —
    /// [`ScrollSpan`]'s own tests settle that — it is whether two thumbs sitting
    /// permanently along the edges of somebody's picture read as furniture, and
    /// no assertion about widgets can answer that. `docshot::Stage` is the only
    /// thing in the crate that can look at a piece of interface.
    ///
    /// The middle shot is the one to judge: that is the fitted document, which
    /// used to draw no bars at all and is the whole of what changed.
    ///
    /// **Both themes, on `backdrop`.** Not one of the two, and not on `chrome`,
    /// and both of those were wrong here rather than approximations: the thumb
    /// lies over the *canvas*, which is what the composite pass paints
    /// `backdrop` with, and the surfaces invert between Graphite and Paper so a
    /// shot of one says nothing about the other. Shooting the idle thumb on
    /// `chrome` in Graphite alone is how an ink at 1.07:1 in Paper survived
    /// being looked at.
    ///
    /// It takes **no `gputest::lock()`**, and that is decided rather than
    /// skipped — see the note in the body.
    ///
    /// ```sh
    /// cargo test -p umber-app canvas_scrollbar_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn canvas_scrollbar_preview() {
        use crate::docshot;
        use crate::editor::Editor;
        use crate::theme::{Palette, ThemeKind};
        use egui::{Pos2, Rect, vec2};
        use glam::Vec2;

        // No `gputest::lock()`, against this crate's stated rule, and the
        // reason is that taking it here would make the thing the rule guards
        // *worse*. `lock` builds the shared device on the way past, and
        // `docshot::Stage` builds one of its own regardless — so the guard buys
        // two live devices where there was one, which is the configuration
        // blamed for the `STATUS_ACCESS_VIOLATION` at process exit on the ARM64
        // runner. The six other `#[ignore]`d previews take no guard either, so
        // it would not serialise them; the faithful fix is for `Stage` to take
        // the shared device, and it belongs in `docshot` rather than here.
        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/canvas-scrollbars");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        // The zoom at which the whole document just fits: 720x480 points at two
        // pixels a point is 1440x960 physical, and what bounds it is the
        // smaller *ratio* rather than the shorter side — the same rule
        // `Camera::fit` keeps, minus its margin. Restating the document's own
        // size would make the middle shot quietly stop being the fitted case
        // the moment `Document::default` moved.
        let field = vec2(720.0, 480.0);
        let doc = Editor::default().doc.size_vec2();
        let fits = (1440.0 / doc.x).min(960.0 / doc.y);
        let middle = doc * 0.5;
        let mut written = 0;
        for (theme, ink) in [
            (ThemeKind::Graphite, "graphite"),
            (ThemeKind::Paper, "paper"),
        ] {
            for (name, zoom, centre) in [
                ("1-zoomed-in", 1.0, middle),
                ("2-fits", fits, middle),
                // Pushed towards the bottom-right corner, so both thumbs are
                // off their middles and in opposite directions.
                ("3-pushed-off", fits, doc * Vec2::new(0.88, 0.15)),
            ] {
                let mut ed = Editor::default();
                ed.ui.theme = theme;
                ed.pixels_per_point = 2.0;
                ed.camera.zoom = zoom;
                ed.camera.center = centre;
                let palette = Palette::with_accent(theme, ed.ui.accent);
                let rect = Rect::from_min_size(Pos2::ZERO, field);
                let image = stage.shoot(field, 2.0, &palette, palette.backdrop, |ui| {
                    super::canvas_scrollbars(ui, &palette, &mut ed, rect);
                });
                docshot::write_png(&dir.join(format!("{ink}-{name}.png")), &image)
                    .expect("write the png");
                written += 1;
            }
        }
        println!("wrote {written} shots to {}", dir.display());
    }
}
