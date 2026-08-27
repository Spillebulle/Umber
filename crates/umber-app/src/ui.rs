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
use crate::loupe;
use crate::panels;
use crate::shortcuts::{self, Action};
use crate::syspick;
use crate::tabs;
use crate::theme::contrast::{self, Ink};
use crate::theme::{Palette, metrics, text};
use crate::tweaks::Tweak;
use crate::widgets;
use egui::{Align2, FontId, Frame, Margin, Rect, Sense, Stroke, pos2, vec2};
use umber_core::{
    BlendMode, Brush, DabInput, DabTarget, GrainPattern, Modulation, ResponseCurve, ScrollSpan,
    Selection, SelectionMode, SelectionOp, input::PressureSource,
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
    /// Set the selected text layer again from what the Text panel is showing:
    /// re-render in place, over the union of where the text was and where it is
    /// going. The caller's for [`UiActions::place_text`]'s reasons and one more
    /// — it reads the layer back off the GPU for the undo patch.
    ///
    /// **Not a float.** The record already says where the text goes, so putting
    /// a box up would make "a float exists only with the transform tool in hand"
    /// learn a second tool at each of the three places it is checked; moving and
    /// turning stays the transform tool's job.
    pub update_text: bool,
    /// Take the record off the selected layer, leaving every pixel. It changes
    /// no pixel, so there is no undo entry and nothing for the GPU to do — but
    /// it goes through the caller anyway, because the panel may not reach past
    /// the editor to reset what it is editing.
    pub convert_text_to_paint: bool,
    /// Set the text being edited in the palette's colour rather than the one the
    /// record holds, at the next Update. Through the caller because
    /// `TextState::editing` is written in one place per frame, exactly as the
    /// layer ticks are.
    pub take_text_colour: bool,
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
    // After the notice, so a document that failed shows why rather than the
    // bar of the next one over the top of it.
    tabs::loading(root, &p, ed);

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
            // Before `pen_cursor` and not after it. Neither paints the other's
            // cursor — they are exclusive on `Editor::pen_pointer` — but
            // egui's cursor is whichever was asked for *last*, so putting the
            // pen's request second means that if the exclusivity ever broke,
            // the pen's own dot would still win over a crosshair rather than
            // the window ending up with both.
            aiming_cursor(ui, ed);
            pen_cursor(ui, &p, ed);
            rect
        })
        .inner;

    panels::floats(root, &p, ed, &mut actions);
    panels::edit_mode_outline(root, &p, ed, &geo);
    // Last, so the drop it resolves is tested against a frame in which every
    // panel has already had its say.
    panels::drag_overlay(root, &p, ed, &geo);

    // The eyedropper's magnifier, over everything and part of nothing. Drawn
    // from here rather than from the central panel because a pick over the
    // *interface* is now a real read, so the circle has to be able to sit over
    // a docked panel — and a central panel's painter is clipped to its own
    // rect. It is a bare layer painter and not an `Area`, which is what keeps
    // it out of `layer_id_at` and therefore out of every gate that asks
    // whether egui owns the pointer.
    loupe_overlay(root, &p, ed);
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

/// The outline's width, in points, on a display at `pixels_per_point`.
///
/// **A whole number of device pixels, never one point.** egui already snaps an
/// axis-aligned line's *position* to the pixel grid — `tessellate_line_segment`
/// does it, and the dashes are `Shape::LineSegment`s so they get it too — but it
/// can only snap what it is given, and a one-point stroke on a display at 150%
/// is one and a half device pixels. epaint draws a line that wide as a ridge
/// whose fully opaque core is half a pixel across, straddling a pixel boundary,
/// so no pixel centre is ever fully covered: the ants come out two device pixels
/// wide at three quarters opacity each.
///
/// **It bites at 150% and at 175% and nowhere else**, which is worth being exact
/// about rather than saying "any fractional scale". The core is under-covered
/// only where the nearest whole number of device pixels is *even* while the
/// scale is not, and that is the interval from 150% up to 200%; 125% rounds to
/// one, lands on a pixel centre and was already crisp. So it is invisible at
/// 100% and 200%, which are the two scales anybody developing would be at.
/// Measured over white paper:
/// the darkest pixel of the dark half is 0 at 100%, 125% and 200%, and **64** at
/// 150%, where the accent reads 160 against its own 192.
///
/// Rounding the *width* to whole device pixels puts the opaque core back on a
/// covered pixel at every scale, because epaint then rounds the position to a
/// centre for an odd count and to a boundary for an even one, and the two agree.
/// It changes nothing at 100% or 200%, where one point already is a whole number
/// of pixels; at 125% it narrows 1.25 device pixels to 1, dropping a faint
/// fringe rather than fixing a softness.
///
/// It is asked of the *context* and not of `Editor::pixels_per_point`, which is
/// written after the frame that used it and so is one frame stale — a marquee a
/// frame behind the scale is exactly the soft line this exists to prevent, on
/// the frame somebody drags Umber onto their second monitor.
///
/// **Two things this does not reach, and both are worth knowing before anybody
/// reads it as settled.** It only helps a line egui *snapped*, which is one
/// that is axis-aligned: `tessellate_line_segment` tests `a.x == b.x` and
/// `a.y == b.y` and does nothing otherwise, so a lasso's marquee is as soft as
/// it ever was and nothing here can change that. And the same one-point stroke
/// is drawn by `transform_box` — its 2-point underlay is three device pixels at
/// 150%, an odd count, so that half stays crisp while the accent on top is
/// exactly the ridge described above. It has not been changed, because an
/// appearance is not something to alter without looking at it and there is no
/// preview for the float.
fn ant_width(pixels_per_point: f32) -> f32 {
    let ppp = pixels_per_point.max(1e-3);
    ppp.round().max(1.0) / ppp
}

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
/// and a black one. Neither colour is a literal, and the under-pass is
/// [`Palette::accent_underlay`] rather than `backdrop`: that token was chosen
/// because "`backdrop` and `accent` are each dark in one theme and light in the
/// other", and Krita's canvas surround is a 50% grey, so the two halves came
/// within 1.60:1 of each other and the dark line read 1.00:1 on mid-grey paint.
/// The underlay is the far end of the lightness axis from the accent, which is
/// what that sentence was reaching for. **Only the accent dashes move.** The
/// line under them stays solid, so the pair still reads on any artwork at every
/// instant of the animation rather than only when a dash happens to be over a
/// dark pixel.
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
    // See `ant_width`: one *point* is a soft, two-pixel line at 150% and 175%
    // scaling, which are two of the settings Windows offers and neither of them
    // is a scale anybody developing this would have been looking at.
    let width = ant_width(ui.ctx().pixels_per_point());
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
            painter.line_segment([pair[0], pair[1]], Stroke::new(width, p.accent_underlay()));
        }
        dashes.clear();
        egui::Shape::dashed_line_many_with_offset(
            screen,
            Stroke::new(width, p.accent),
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
    // Two passes, dark then light, so the box reads over both a white canvas
    // and a black one — the same trick the selection outline uses, and the
    // under-pass is the same `accent_underlay`. It was `backdrop`; see there.
    let under = p.accent_underlay();
    for i in 0..4 {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        painter.line_segment([a, b], Stroke::new(2.0, under));
        painter.line_segment([a, b], Stroke::new(1.0, p.accent));
    }

    for handle in umber_core::Handle::BOX {
        let at = to_screen(float.xf.handle_at(handle));
        painter.circle_filled(at, 4.0, under);
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
    // artwork and neither colour can be assumed to read against it. The
    // under-pass is `accent_underlay` for the reason the outline's is.
    icons::draw(painter, at.expand(1.0), Icon::Rotate, p.accent_underlay());
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
    // Derived from the pit, at the rank `text_dim` used to hold. That token was
    // chosen for being the one ink that is a mid-grey in *every* theme — the
    // surfaces invert between the light themes and the dark ones and most of
    // the ink with them, so a fixed strong ink would be black on one and white
    // on the other, over artwork that is neither. The flaw is the same reading
    // read the other way: on a mid-grey pit a mid-grey dot is 1.34:1, which is
    // a cursor nobody can find. `theme::contrast` has the argument, and
    // `a_mark_on_the_canvas_pit_reads_in_every_theme` is the bound.
    ui.painter().circle_filled(
        ed.to_points(at),
        metrics::PEN_DOT,
        contrast::ink_on(p.backdrop, Ink::Dim),
    );
}

/// A crosshair over the canvas while the eyedropper is in hand.
///
/// **The one tool that gets a cursor of its own, and that is deliberate rather
/// than the start of a set.** Every other tool paints a mark whose own size is
/// what the hand is aiming with — a brush has a stroke, a selection has a
/// marquee, a transform has a box — and the arrow is a fine pointer at all of
/// them. A pick has no mark at all and its target is exactly one pixel, so the
/// arrow, whose hotspot is its tip and whose body covers the pixels below and
/// to the right, is the worst possible thing to aim it with. That is why every
/// application draws a crosshair here and none of them draws one for a brush.
///
/// **Never with a pen**, which draws its own dot: a crosshair and a dot would
/// be two pointers. That is `Editor::pen_pointer` below, so the two are
/// exclusive by construction and not by ordering — but this is called *before*
/// [`pen_cursor`] anyway, because egui takes whichever cursor was asked for
/// last and being second is what a safeguard would need to be.
///
/// It is a per-frame request like every other cursor in this interface, so
/// nothing has to remember to put the arrow back: change tool, cross onto a
/// panel, open a dialog, and it is gone on the next frame. See
/// [`Editor::pen_dot`](crate::Editor::pen_dot) for why that shape was chosen
/// over a latch.
fn aiming_cursor(ui: &egui::Ui, ed: &Editor) {
    if ed.pen_pointer {
        return;
    }
    // The same two readings `pen_cursor` hands to `Editor::pen_dot`, and for
    // the same reasons: over a panel or a modal the ordinary cursor is the
    // right one, and asking for a shape while another application has the
    // keyboard would set it across the whole desktop.
    //
    // The rule they feed is `Editor::aiming_pick`, shared with `App::
    // pick_aimed` — so the crosshair and the loupe cannot end up promising a
    // colour in different places.
    let around = editor::Surroundings {
        over_area: editor::over_egui_area(ed, ui.ctx(), ed.cursor),
        focused: ui.ctx().input(|i| i.focused),
    };
    if !ed.aiming_pick(around) {
        return;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
}

/// The eyedropper's magnifier: a circle of screen or document pixels, beside
/// the pointer, with the one a release would take marked.
///
/// **The picture is read, never invented.** Every cell is a texel
/// `App::pick_this_frame` actually sampled, and a texel it could not read draws
/// nothing at all — the rim shows through, which is what a position off every
/// monitor or off the canvas looks like. A loupe that filled those in with
/// black would be a control that lies, which is the standard everything else
/// here is held to, and it is why [`loupe::Patch`] carries an `Option` per
/// texel rather than a colour.
///
/// **The middle cell is drawn from [`loupe::Loupe::taken`] and not from the
/// block.** Today those are the same value wherever there is a block — the
/// colour *is* the middle texel — so this reads as a distinction without a
/// difference and is not one: `taken` is what a release keeps, the two have
/// separate fallbacks the other cannot supply, and the cell under the mark is
/// the one thing here that must be the colour rather than the picture. Drawing
/// it from the block would be right by coincidence.
///
/// Where the circle goes is [`loupe::place`]'s, in a model with no drawing in
/// it, for the reason `overlay::place_strip` and `ScrollSpan` are — and it
/// carries a rule the painter cannot state: the circle never comes within half
/// a block of the pointer, because over the interface and the desktop the block
/// is read off the same screen the circle is drawn on. It is handed
/// [`loupe::OUTER`], the radius of the *rim*, and not the grid's own — the
/// guard has to measure the shape that is drawn, so the rim is a figure of that
/// module's rather than a number here.
///
/// The picture inside it is drawn to fill the disc and is then *covered* at the
/// edge by the rim, so the boundary the eye sees is the rim's own feathered
/// inner edge rather than the corner of whichever texel got there last. See
/// [`loupe_cells`] and [`loupe_glass`]; before that it was a staircase of
/// six-point steps, which is what a grid clipped cell by cell to a circle looks
/// like. A consequence worth stating rather than discovering: the block is
/// square and the window is round, so its corners are read and not shown. The
/// block is that wide because a `BitBlt` of it costs what one pixel costs, not
/// because every texel is on screen.
fn loupe_overlay(root: &egui::Ui, p: &Palette, ed: &Editor) {
    let Some(seen) = ed.loupe.as_ref() else {
        return;
    };
    let ctx = root.ctx();
    let pointer = ed.to_points(seen.at);
    // `content_rect` and not `viewport_rect`: on a platform with a notch or a
    // status bar the second includes the part of the window nothing may be
    // drawn in, and a loupe clamped into *that* would sit under it.
    let view = ctx.content_rect();
    let Some(centre) = loupe::place(
        glam::Vec2::new(pointer.x, pointer.y),
        loupe::View {
            min: glam::Vec2::new(view.min.x, view.min.y),
            max: glam::Vec2::new(view.max.x, view.max.y),
        },
        loupe::OUTER,
    ) else {
        return;
    };
    let centre = pos2(centre.x, centre.y);

    // A bare layer painter, not an `Area`: an `Area` registers a rectangle that
    // `Memory::layer_id_at` then answers with, which would make the loupe the
    // thing `ui_owns_pointer` and `Editor::pen_dot` see — a magnifier that
    // refused the press it exists to aim.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("umber-loupe"),
    ));

    // The body, under everything. It is what shows through where a texel could
    // not be read, and its own edge is what gives the silhouette its
    // antialiasing: a mesh has none, so the rim's shading above is laid over a
    // feathered disc of the same colour rather than being asked to be the
    // outline itself.
    painter.circle_filled(centre, loupe::OUTER, p.popover);

    let taken = seen.taken.map(|c| {
        let [r, g, b, _] = c.to_srgb_u8();
        egui::Color32::from_rgb(r, g, b)
    });
    let mark = match seen.patch.as_ref() {
        Some(patch) => loupe_cells(&painter, patch, centre, taken),
        // Only the one pixel could be read, so it is shown large rather than
        // magnified into a grid it never had. The alternative — a fake
        // neighbourhood — is the one thing this control may not do.
        None => {
            if let Some(fill) = taken {
                painter.circle_filled(centre, loupe::RADIUS, fill);
            }
            None
        }
    };

    loupe_glass(&painter, p, centre);

    if let Some(rect) = mark {
        // Over the glass rather than under it, because this is the one mark
        // here that is aimed by rather than looked at, and the thickness wash
        // would dim it. It sits in the middle of the disc where that wash is
        // nothing anyway, so the order costs the look nothing either.
        //
        // Derived from the colour it sits on, so it reads on a white swatch and
        // on a black one — the rule `theme::contrast` exists for, and the only
        // way a fixed ink could work here would be if the loupe were never
        // aimed at the theme's own extremes, which is exactly what it is for.
        let on = taken.unwrap_or(p.popover);
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.5, contrast::ink_on(on, Ink::Strong)),
            egui::StrokeKind::Outside,
        );
    }
}

/// How many segments the lens and its rim are built from.
///
/// The bands are meshes, so nothing in them is antialiased by egui and this
/// figure *is* the smoothness of every arc in the loupe. Ninety-six is under
/// three points a segment at [`loupe::OUTER`], which is a fraction of a device
/// pixel of sag at any scale a window runs at — 0.02 points, which is 0.07 of a
/// device pixel even at 300%. What it costs is three meshes and 1,152 triangles,
/// allocated per frame, on the frames of a session where somebody is holding the
/// eyedropper down. That is an allocation on a drawing path and it is stated
/// rather than hidden: the loupe exists only during a pick, where the frame
/// already carries a screen read that waits on the compositor.
const GLASS_SEGMENTS: usize = 96;

/// Where the light falls on the lens, as an angle in the painter's y-down frame.
///
/// Up and to the left, which is the direction every drawing tradition and every
/// interface toolkit lights a raised thing from. It is **not** a convention this
/// interface already had: the lens is the only shaded surface in Umber, so this
/// sets one rather than following one, and anything raised added later should
/// agree with it.
///
/// **Fixed rather than following the pointer**: a highlight that swung round as
/// the hand moved would read as the picture sliding about under the glass rather
/// than as a lens being carried over it.
const GLASS_LIGHT: f32 = -3.0 * std::f32::consts::FRAC_PI_4;

/// How dark the glass goes at its own edge, as a fraction of [`contrast::SHADE`].
///
/// This is the thickness of the lens: there is more glass to look through at the
/// perimeter than at the middle. It has to stay low, because what is under it is
/// the picture somebody is picking a colour out of — the wash reaches the outer
/// texels and not the middle one, which is the one a release takes.
const GLASS_THICKNESS: f32 = 0.17;

/// The hairline the boundary catches: all the way round, facing the light, and
/// on the far side.
///
/// Never zero on the away side: a real edge picks up something all the way
/// round, and a highlight that stopped dead halfway would draw a seam across the
/// lens instead of turning it.
///
/// **The third figure is what tells glass from plastic.** Light entering the top
/// left of a lens leaves at the bottom right, so a real one is bright on the
/// side *away* from the source as well as towards it — a single highlight with a
/// dead opposite edge is how a moulded button reads. It is tighter than the main
/// lobe as well as weaker, which is why they are raised to different powers in
/// [`loupe_glass`].
const GLASS_EDGE: (f32, f32, f32) = (0.10, 0.60, 0.38);

/// How wide that hairline is, in points.
const GLASS_EDGE_WIDTH: f32 = 2.2;

/// How hard the rim turns from the light, at its inner edge, its middle and its
/// outer edge.
///
/// Peaked in the middle, which is what makes it read as a bead rather than as a
/// bevel: a band brightest against the picture is a chamfer, and one brightest
/// in the middle is round.
const GLASS_RIM: (f32, f32, f32) = (0.22, 0.66, 0.30);

/// How far past [`loupe::OUTER`] the rim's shading fades out, in points.
///
/// A mesh has no feathering of its own, so without this the shading would stop
/// at a polygon edge and put a stepped silhouette back on the one shape the body
/// disc underneath is antialiased for. Half a point is under a device pixel
/// everywhere and is enough to hand the edge back to the disc.
///
/// It also covers a mismatch nothing else would: epaint tessellates a circle of
/// this size with sixty-four segments and [`GLASS_SEGMENTS`] is ninety-six, so
/// the band is the *rounder* of the two and can sit 0.03 of a point outside the
/// disc it is laid on. That is nothing while the outermost stop is transparent
/// and the disc's own feather reaches past [`loupe::OUTER`]; it stops being
/// nothing the day somebody removes this fade.
const GLASS_FADE: f32 = 0.5;

/// A band of triangles between a run of radii, coloured per corner.
///
/// The one shape the lens needs that a `Painter` has no method for: every part
/// of it is a gradient across a band, and egui's circles and strokes are flat. A
/// mesh also lets two bands abut with no seam, which two feathered fills laid
/// against each other cannot — the same reason the cells below are `rect_filled`
/// rather than polygons clipped to the circle.
///
/// `at(angle, stop)` is asked for every corner, so one call carries both the
/// sweep of the highlight around the rim and the falloff across it.
fn glass_band(
    centre: egui::Pos2,
    radii: &[f32],
    at: impl Fn(f32, usize) -> egui::Color32,
) -> egui::Shape {
    let stops = radii.len();
    let mut mesh = egui::Mesh::default();
    mesh.reserve_vertices(stops * (GLASS_SEGMENTS + 1));
    mesh.reserve_triangles(2 * (stops - 1) * GLASS_SEGMENTS);
    for i in 0..=GLASS_SEGMENTS {
        let angle = i as f32 / GLASS_SEGMENTS as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        for (stop, r) in radii.iter().enumerate() {
            mesh.colored_vertex(centre + vec2(cos, sin) * *r, at(angle, stop));
        }
    }
    for i in 0..GLASS_SEGMENTS as u32 {
        let (here, next) = (i * stops as u32, (i + 1) * stops as u32);
        for stop in 0..stops as u32 - 1 {
            let (a, b) = (here + stop, next + stop);
            mesh.add_triangle(a, b, a + 1);
            mesh.add_triangle(b, b + 1, a + 1);
        }
    }
    egui::Shape::Mesh(mesh.into())
}

/// One end of the lightness axis at a fraction of full.
///
/// Both ends rather than an ink chosen against a surface, because what the lens
/// is drawn over is the artist's own picture — [`contrast::LIT`] carries the
/// argument, which is `Palette::accent_underlay`'s landing the same way.
fn glass_ink(lit: bool, strength: f32) -> egui::Color32 {
    let c = if lit { contrast::LIT } else { contrast::SHADE };
    let a = (strength.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// The shading at a point on the rim facing `angle`, at `strength` of full.
///
/// The cosine of the angle to [`GLASS_LIGHT`] decides both which end of the axis
/// is wanted and how much of it, which is Lambert's law for a bead lit from one
/// side — so one number carries the highlight and the shadow opposite it, and
/// the two cannot end up out of step. **Squared**, because a linear falloff
/// reads as two flat halves with a seam down the middle, which is a painted ring
/// rather than a turned surface.
fn glass_facing(angle: f32, strength: f32) -> egui::Color32 {
    let facing = (angle - GLASS_LIGHT).cos();
    glass_ink(facing >= 0.0, facing * facing * strength)
}

/// The lens: what turns a grid of texels into something that reads as glass.
///
/// Four passes over the picture and the rim, and every one of them is a gradient
/// rather than a line:
///
/// * **The thickness**, a wash of the dark end from the middle of the glass out
///   to its edge. There is more glass to look through at the perimeter than at
///   the centre, and a lens with no such falloff is a hole.
/// * **The rim**, an opaque band in `popover` covering the cell overhang.
/// * **The catch-light**, a hairline of the light end right at the boundary,
///   brightest where the edge faces [`GLASS_LIGHT`] and with a second, tighter
///   lobe on the side away from it. This is the thing that says at a glance that
///   there is a surface here, and the second lobe is what says the surface is
///   glass — see [`GLASS_EDGE`].
/// * **The rim's shading**, turning from the light end at the top left to the
///   dark end at the bottom right, so the band reads as round.
///
/// **There is no black outline and deliberately no stroke of any kind that means
/// one.** A hairline in `popover_border` used to be what separated the loupe
/// from the canvas, and a hard dark ring is the one thing glass does not have;
/// what separates it now is the rim's own shading, which is a surface rather
/// than a border. The opaque band *is* drawn with `circle_stroke` — that is how
/// egui spells a feathered annulus, and its inner feather is what antialiases
/// the boundary of the picture — but nothing about it reads as a line.
///
/// **The magnification stays uniform to the very edge, and that is a refusal
/// rather than an omission.** A real lens compresses what it shows at the
/// perimeter. Copying that would make the outer cells narrower than the ones
/// beside them while the block behind them is still read on a uniform grid, so a
/// cell would stop standing for one screen pixel — which is the whole of what
/// this control is aimed by. `loupe::CELLS`' own comment makes the same trade
/// the other way: read more than is shown, never show more than was read.
fn loupe_glass(painter: &egui::Painter, p: &Palette, centre: egui::Pos2) {
    let (r, o) = (loupe::RADIUS, loupe::OUTER);

    // The thickness. It starts well inside the boundary so the falloff is a
    // gradient across half the lens rather than a ring somebody could point at,
    // and it is bent at the middle stop rather than run straight: a linear ramp
    // over that distance has a visible start, which is the ring this is trying
    // not to be.
    painter.add(glass_band(centre, &[r * 0.42, r * 0.78, r], |_, stop| {
        let strength = match stop {
            0 => 0.0,
            1 => GLASS_THICKNESS * 0.22,
            _ => GLASS_THICKNESS,
        };
        glass_ink(false, strength)
    }));

    // The rim, opaque, hiding the cell overhang. A `circle_stroke` rather than a
    // mesh because epaint feathers a thick closed stroke on *both* edges, and
    // the inner one is the boundary of the picture — the one edge in this whole
    // control that has to be a smooth circle.
    //
    // **egui strokes a circle on the *outside* of its radius**, not centred on
    // it: `tessellate_circle` calls `PathStroke::outside()`. So this band runs
    // from `r` to `r + RIM` and the radius asked for is the boundary itself.
    // Handing it the mid-radius, which is what a centred stroke would want, put
    // the band at 37.5 to 46.5 and left the picture spilling four points past
    // the lens.
    painter.circle_stroke(centre, r, Stroke::new(o - r, p.popover));

    // The catch-light, last of the things inside the glass, so it sits over the
    // thickness rather than under it.
    painter.add(glass_band(
        centre,
        &[r - GLASS_EDGE_WIDTH, r],
        |angle, stop| {
            if stop == 0 {
                return glass_ink(true, 0.0);
            }
            let facing = (angle - GLASS_LIGHT).cos();
            let towards = facing.max(0.0).powi(2);
            let away = (-facing).max(0.0).powi(4);
            glass_ink(
                true,
                GLASS_EDGE.0 + GLASS_EDGE.1 * towards + GLASS_EDGE.2 * away,
            )
        },
    ));

    // The rim's shading, peaked across the band and faded out past the
    // silhouette so the edge pixel comes from the disc underneath.
    painter.add(glass_band(
        centre,
        &[r, 0.5 * (r + o), o, o + GLASS_FADE],
        |angle, stop| {
            let strength = match stop {
                0 => GLASS_RIM.0,
                1 => GLASS_RIM.1,
                2 => GLASS_RIM.2,
                _ => 0.0,
            };
            glass_facing(angle, strength)
        },
    ));
}

/// The grid inside the circle, and where the mark on the cell a release would
/// take belongs.
///
/// **The cells are clipped one whole cell past [`loupe::RADIUS`], not to it.**
/// Clipping to the radius leaves the staircase *inside* the circle — the top row
/// of a grid stepping by six points reaches nowhere near the top of the disc —
/// and that is the stepped mosaic this used to draw. A generous clip covers the
/// disc entirely and overhangs it by at most one cell, which [`loupe_glass`]'s
/// opaque rim then hides. **The overhang is vertical only**: `half` is clamped
/// by the grid's own width, which stops at [`loupe::RADIUS`], so along the
/// middle row the picture meets the circle exactly and with no slack at all —
/// covered because `Rect::contains` is inclusive, and covered *visually*
/// because the rim's feather straddles the boundary either way. One cell is the
/// exact figure rather than a margin:
/// the worst gap between a staircase of that pitch and its own circle is
/// `sqrt(R² + 2Rc − c²) − R`, which at eleven cells is 5.1 of the 6 available.
/// The cells past the boundary are drawn and covered, which costs **26** more
/// rectangles on a frame where somebody is picking a colour: 87 before, 113
/// now, counted by walking this loop rather than estimated beside it.
///
/// The reach is derived from *this patch's* cell and not from [`loupe::CELL`],
/// so the coverage argument above holds for whatever size arrives. What the
/// constant is for is the other half, which this function cannot enforce: the
/// rim has to be at least a cell wide to hide the overhang, and `loupe.rs`
/// asserts that against `CELL`. The two agree for the `loupe::CELLS`-wide patch
/// both producers make.
///
/// It returns the mark rather than drawing it, so [`loupe_overlay`] can put it
/// over the glass. See there for why.
fn loupe_cells(
    painter: &egui::Painter,
    patch: &loupe::Patch,
    centre: egui::Pos2,
    taken: Option<egui::Color32>,
) -> Option<Rect> {
    let size = patch.size();
    let cell = 2.0 * loupe::RADIUS / size as f32;
    let reach = loupe::RADIUS + cell;
    let top_left = centre - vec2(loupe::RADIUS, loupe::RADIUS);
    let middle = patch.middle();
    let mut mark = None;

    for row in 0..size {
        let y0 = top_left.y + row as f32 * cell;
        let y1 = y0 + cell;
        // The row edge *further* from the centre, so the half-width is the
        // smaller of the two the row could claim and the cells stay strictly
        // inside `reach`. Reading it at the row's centre instead would let the
        // top and bottom rows overhang by half a cell more than the rim is
        // wide, and the overhang would then show past it.
        let dy = (y0 - centre.y).abs().max((y1 - centre.y).abs());
        if dy >= reach {
            continue;
        }
        let half = (reach * reach - dy * dy).sqrt();
        for col in 0..size {
            let x0 = (top_left.x + col as f32 * cell).max(centre.x - half);
            let x1 = (top_left.x + (col + 1) as f32 * cell).min(centre.x + half);
            if x1 <= x0 {
                continue;
            }
            let rect = Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
            if col == middle && row == middle {
                // Unclipped: the middle of an odd grid is nowhere near the
                // boundary, and the mark has to sit on the whole cell.
                mark = Some(Rect::from_min_size(
                    pos2(top_left.x + col as f32 * cell, y0),
                    vec2(cell, cell),
                ));
                if let Some(fill) = taken {
                    painter.rect_filled(rect, 0.0, fill);
                }
                continue;
            }
            if let Some([r, g, b]) = patch.at(col, row) {
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(r, g, b));
            }
        }
    }

    mark
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
    // The same reading the Edit menu's Undo and Redo rows take, and the same
    // one `App::mirror_document` gates on. See `Editor::flip_refused_by_lock`.
    let flip_locked = ed.flip_refused_by_lock();
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
            // The reason is `editor::FLIP_LOCKED_REASON`, shared with the Edit
            // menu's two history rows and with the notice a refused keystroke
            // raises. Three hand-written near-copies of one sentence is the
            // drift `flip_refused_by_lock` was introduced to stop for the
            // *reading*, left standing for the wording.
            .on_disabled_hover_text(format!(
                "A layer is locked. {} first.",
                editor::FLIP_LOCKED_REASON
            ))
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
    // "in the history" and not "to this document": clearing a layer and
    // resizing the canvas both empty the list, so a document twenty strokes
    // deep can reach this line, and a sentence claiming nothing had been done
    // to it would be flatly false in exactly the case the paragraph above
    // names.
    // **A step over a canvas flip is refused while any layer is locked**, in
    // both directions, and these two rows are disabled to match — exactly as
    // the Image menu's flip rows already are, from the same reading. Without
    // this the menu offers a command that raises a box instead of doing
    // anything, which is the control-that-lies failure; with it, the notice
    // `App::settle_step` raises is left catching the keystroke alone. Each
    // reason gets its own sentence, because "nothing to undo" over a document
    // twenty strokes deep sends somebody looking for the wrong problem.
    // `StepGate::refuses` and never `== StepGate::FlipLocked`: a control asks
    // whether it may offer the command, which is a question about the *set* of
    // refusing answers, and an equality test answers it only while that set has
    // one member. An equality test is what stood here first, and it is
    // `matches!` wearing an operator — a fourth variant would have been a
    // compile error in `App::settle_step` and a silent `false` here.
    //
    // **The gate is read before `App::undo` runs `finish_transform`**, so with
    // a float standing this can disable a row a click would have made live: the
    // float goes down as an `EditKind::Transform`, which is then the entry on
    // top and is not a flip. The row is dead and its sentence names a flip that
    // would no longer have been next. Left as it is deliberately — the error is
    // always towards refusing, never towards offering what the model declines,
    // which is the direction that costs a document nothing — and drawing the
    // menu from a speculative `finish_transform` would mean predicting an edit
    // in order to describe it.
    let undo_locked = ed.undo_gate().refuses();
    if menu_item(ui, Action::Undo, ed.history.can_undo() && !undo_locked)
        .on_disabled_hover_text(if undo_locked {
            format!(
                "The next step back is a canvas flip, and a layer is locked. {} first.",
                editor::FLIP_LOCKED_REASON
            )
        } else {
            "Nothing in the history to undo.".to_owned()
        })
        .clicked()
    {
        actions.undo = true;
        ui.close();
    }
    let redo_locked = ed.redo_gate().refuses();
    if menu_item(ui, Action::Redo, ed.history.can_redo() && !redo_locked)
        .on_disabled_hover_text(if redo_locked {
            format!(
                "The next step forward is a canvas flip, and a layer is locked. {} first.",
                editor::FLIP_LOCKED_REASON
            )
        } else {
            "Nothing undone to put back.".to_owned()
        })
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
    // Cut, Copy, Paste, in that order, because that is the order every desktop
    // draws them and the order the keys sit in on the bottom row.
    //
    // **Both say what they do with nothing selected, and Cut needs to.**
    // `take_region` falls back to the whole layer, so a Cut here with no
    // marquee standing clears the picture — which was reachable before only by
    // Ctrl+X, since the canvas strip that carries the other Cut is drawn only
    // while a selection is live. A new way in to a destructive command is the
    // wrong place to say less than the old one did.
    //
    // Gated on the lock, matching that strip's Cut button and the gate inside
    // `App::cut_selection`: a cut takes pixels off the layer, so a locked one
    // refuses it, and the row says so before the click rather than answering
    // with a notice.
    if menu_item(ui, Action::Cut, !ed.layers.active_is_locked())
        .on_hover_text(
            "Takes the selection onto the clipboard. Cuts the whole layer if nothing is selected.",
        )
        .on_disabled_hover_text("Unlock the layer to cut from it.")
        .clicked()
    {
        actions.cut_selection = true;
        ui.close();
    }
    // Never disabled: it writes nothing.
    if menu_item(ui, Action::Copy, true)
        .on_hover_text("Copies the selection. Copies the whole layer if nothing is selected.")
        .clicked()
    {
        actions.copy_selection = true;
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
    /// now, so it costs what one costs — the 90 point rail, the *field*, and a
    /// label three characters longer than "Opacity"'s. The figure is a field
    /// rather than a painted readout since it became typable, which means it is
    /// as wide as the widest figure the rail can show rather than as wide as
    /// the one showing; all three of these were re-measured against that by
    /// `every_brush_rail_fits_the_budget_that_lets_it_be_drawn`.
    pub const STABILISER: f32 = 190.0;
    /// The flow rail: what one dab lays down, as against `OPACITY`, which
    /// caps the finished stroke. A fourth `inline_slider` — the 90 point rail,
    /// the field reserving its widest figure ("100"), and a label three
    /// characters shorter than "Opacity"'s.
    ///
    /// **Bisected against the guard rather than derived**, which is what the
    /// derivation was worth: the first figure here was 175, reasoned from
    /// `OPACITY` less ten points for three fewer characters, and that
    /// disagreed with `STABILISER`'s own reasoning (five points for three
    /// *more*) in the same breath. Measured,
    /// `every_rail_on_the_strip_fits_the_budget_that_lets_it_be_drawn` fails at
    /// 140 and passes at 145, so what the rail needs is in `(140, 145]`. 150
    /// is that with a font metric's worth of room on top — the margin a
    /// glyph-width change moves by — and 175 was simply thirty points of
    /// nothing, dropping the rail earlier than it had to on a narrow window.
    ///
    /// **It is drawn last and therefore dropped first**, which is a claim
    /// about what a painter is stuck without rather than about what matters.
    /// Size and opacity are reached for constantly; the stabiliser is the one
    /// setting adjusted *while* a line is being drawn. Flow is a statement
    /// about the brush's character, so it is the one of the four that can
    /// wait for the brush editor on a narrow window.
    pub const FLOW: f32 = 150.0;
    /// The line naming the modifiers that add to, subtract from and intersect a
    /// selection, and say what the feather applies to.
    pub const COMBINE: f32 = 320.0;
    /// The four marks that say what a new shape does to the selection.
    pub const SELECT_OP: f32 = 105.0;
    /// The feather rail, its label and its figure.
    ///
    /// 165 while the figure was a painted label as wide as the value showing —
    /// one character at the default of 0. It is a field now, reserving the
    /// widest figure the rail can produce ("250" plus a digit's room for the
    /// caret), which is about twenty points more. Widened by hand rather than
    /// measured, because the Select strip is the one
    /// `every_brush_rail_fits_the_budget_that_lets_it_be_drawn` declines to
    /// sweep and says why.
    pub const FEATHER: f32 = 185.0;
    /// The eyedropper's second sentence: what a drag off the window does, or
    /// why it does nothing here. Both readings are within a few characters of
    /// each other, so unlike the navigation pair one figure covers them.
    ///
    /// **Bisected against its own guard rather than estimated**, which is what
    /// the estimate was worth: 305 came from `ZOOM`'s points-per-character and
    /// `the_eyedroppers_hint_does_not_overrun_the_strip_it_is_drawn_on` fails
    /// at it, by four points — which is exactly the margin a font metric moves
    /// by. It also fails at 295 and passes at 320, so what the sentence
    /// actually needs is somewhere in `(305, 320]` and the `ui::add_space(6.0)`
    /// before the label is part of it.
    pub const EYEDROPPER: f32 = 320.0;
    // Pan's and Zoom's second sentence are budgeted per tool, on
    // `navigate_hint`'s own third field, because the two lines are a third
    // apart in width and one figure for both drops Pan's while it still fits.
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
/// reasoning as [`combine_hint`], which is also why this is a `const fn`
/// returning `&'static str` rather than a `format!` per frame: the strip is
/// painted every frame and neither line can change at run time, because
/// neither gesture is bindable.
///
/// **The modifier is spelled out here rather than taken from
/// [`shortcuts::primary_modifier_name`], and that is a `const fn` limitation
/// rather than a preference.** A `const fn` cannot concatenate, so the choice
/// was between writing "Cmd"/"Ctrl" under the same `cfg!` that function uses,
/// or building the line at run time on the drawing path. [`combine_hint`] made
/// the same trade and says so; this is the third place that spelling lives, and
/// if it ever becomes a fourth it should be a `concat!` under `#[cfg]` off one
/// constant instead.
///
/// The third field is what the second sentence costs, so the strip can drop it
/// on its own terms: it is one unwrapped row, and a sentence that does not fit
/// runs off the end rather than reflowing. Per tool rather than one figure for
/// both, because Zoom's line is a third longer than Pan's and a shared budget
/// takes Pan's away at a width that would have held it comfortably. Measured
/// off `options_strip_preview`'s shots and pinned by
/// `neither_navigation_hint_overruns_the_strip_it_is_drawn_on`.
///
/// **Exhaustive over [`Tool`] rather than a wildcard**, for the reason
/// `panels::edit_icon` is exhaustive over `EditKind`: nine of the design's
/// sixteen tools are not built, and the first navigation tool added would
/// otherwise silently draw Pan's two sentences. The five that never reach this
/// branch are named so that `Tool` growing is a compile error and not a wrong
/// sentence — `options_strip` handles them above, and if one ever fell through
/// to here saying nothing is better than saying something false.
const fn navigate_hint(tool: Tool) -> (&'static str, &'static str, f32) {
    match tool {
        Tool::Brush | Tool::Eraser | Tool::Select | Tool::Transform | Tool::Eyedropper => {
            ("", "", f32::INFINITY)
        }
        Tool::Zoom => (
            "Drag right or up to zoom in, left or down to zoom out.",
            if cfg!(target_os = "macos") {
                "Hold Cmd and roll the wheel to zoom at the pointer with any tool in hand."
            } else {
                "Hold Ctrl and roll the wheel to zoom at the pointer with any tool in hand."
            },
            370.0,
        ),
        Tool::Pan => (
            "Drag on the canvas to move the picture.",
            "Hold Space to do the same with any tool in hand.",
            250.0,
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

/// The strip's brush size rail.
///
/// A function rather than a struct literal at the call site, for the reason
/// `settings::scale_row` is one: a test that restated these numbers would go on
/// passing while the control it stands for was changed underneath it. This one
/// carries the whole point of the rail — that its `span` stops at
/// [`Tweak::span`]'s 1000 while a *typed* figure is held to [`Tweak::range`]'s
/// `Brush::MAX_SIZE` — so a test of `widgets::typed_value` that built its own
/// `Rail` would prove the widget respects a limit and not that this rail passes
/// one. `widgets`' tests read it.
pub(crate) fn strip_size_rail() -> widgets::Rail<'static> {
    widgets::Rail {
        label: "Size",
        // The rail stops at `tweaks::SIZE_RAIL_TOP` and a size does not: type
        // 1500 and the brush is 1500 px across. Shared with the brush editor's
        // own size rail so the two cannot stop in different places.
        span: Tweak::Size.span(),
        limit: Tweak::Size.range(),
        log: true,
        snap: 0.0,
        deferred: false,
        // Bare, not `Tweak::Size.figure()`'s " px": the strip is one unwrapped
        // row of three rails and a unit on each is nine points it cannot spare.
        // The label is directly beside it and says which setting it is.
        figure: widgets::Figure::new(1.0, "", 0),
    }
}

/// The strip's opacity rail, and the stabiliser's beside it.
///
/// One function for the pair because they are the same control with two names
/// and two bounds — a percentage of something, dragged and typed the same way.
/// Stated here rather than at the call site for [`strip_size_rail`]'s reason.
///
/// The stabiliser's range is the brush editor's own — 0.0..=0.95, where 1.0
/// would be a stroke that never reaches the pen — so the two controls cannot
/// disagree about what full stabilisation is, and it is the typed limit as
/// well: a percentage rail whose hundred is not reachable is one whose hundred
/// is not real.
pub(crate) fn strip_percent_rail(label: &'static str, top: f32) -> widgets::Rail<'static> {
    widgets::Rail {
        label,
        span: 0.0..=top,
        limit: 0.0..=top,
        log: false,
        snap: 0.0,
        deferred: false,
        figure: widgets::Figure::new(100.0, "", 0),
    }
}

/// The brush's flow rail.
///
/// [`strip_percent_rail`] is not reusable here: its span starts at zero and
/// flow's starts at [`Brush::MIN_FLOW`], because a flow of zero paints nothing
/// and the decade under the bound is a dab the `R8Unorm` scratch rounds away.
/// `limit` matches `span`, so a typed figure is held to the same floor the drag
/// is — the one place a rail may legitimately differ is brush size, and it says
/// so.
pub(crate) fn strip_flow_rail() -> widgets::Rail<'static> {
    widgets::Rail {
        label: "Flow",
        span: Brush::MIN_FLOW..=1.0,
        limit: Brush::MIN_FLOW..=1.0,
        log: false,
        snap: 0.0,
        deferred: false,
        figure: widgets::Figure::new(100.0, "", 0),
    }
}

/// The Select tool's feather rail.
///
/// Its figure can be typed exactly as the brush rails' can — `inline_slider` is
/// one control — and what has not changed is that it sets what the *next* shape
/// will be rather than softening the one standing. [`combine_hint`] is where
/// the strip says so.
pub(crate) fn strip_feather_rail() -> widgets::Rail<'static> {
    widgets::Rail {
        label: "Feather",
        span: 0.0..=Selection::MAX_FEATHER,
        limit: 0.0..=Selection::MAX_FEATHER,
        log: false,
        snap: 0.0,
        deferred: false,
        figure: widgets::Figure::new(1.0, "", 0),
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
            Tool::Eyedropper => (Icon::Eyedropper, "Eyedropper"),
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
                widgets::inline_slider(ui, p, &mut ed.brush.size, &strip_size_rail());
            }
            if room >= strip_budget::SIZE + strip_budget::OPACITY {
                widgets::inline_slider(
                    ui,
                    p,
                    &mut ed.brush.opacity,
                    &strip_percent_rail("Opacity", 1.0),
                );
            }
            if room >= strip_budget::SIZE + strip_budget::OPACITY + strip_budget::STABILISER {
                // The third rail, and the same control as the two beside it.
                // It used to be a `widgets::chip` — a reading, with a tooltip
                // saying to go to the brush editor to change it — which put the
                // one setting a painter adjusts *while* drawing a line behind
                // two clicks and a tab.
                widgets::inline_slider(
                    ui,
                    p,
                    &mut ed.brush.stabilization,
                    &strip_percent_rail("Stabiliser", Brush::MAX_STABILIZATION),
                );
            }
            if room
                >= strip_budget::SIZE
                    + strip_budget::OPACITY
                    + strip_budget::STABILISER
                    + strip_budget::FLOW
            {
                // Beside Opacity because that is the pair a painter reasons
                // about together, and the two are genuinely different numbers:
                // opacity caps the finished stroke once, flow meters what each
                // dab lays down on the way to it. Photoshop puts them side by
                // side on its own options bar for the same reason.
                //
                // The rail bottoms out at `Brush::MIN_FLOW` rather than at zero,
                // unlike Opacity's: a flow of zero is a brush that paints
                // nothing, and the decade below the bound is a dab too faint for
                // the scratch to store at all — a control whose bottom end is
                // indistinguishable from a broken one.
                widgets::inline_slider(ui, p, &mut ed.brush.flow, &strip_flow_rail());
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
                widgets::inline_slider(ui, p, &mut ed.ui.selection_feather, &strip_feather_rail());
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
        } else if ed.ui.tool == Tool::Eyedropper {
            // Two sentences and no controls, in the Transform hint's register
            // and for the same reason: there is nothing here to set, the whole
            // gesture is the pointer's.
            //
            // The second one is the only place in the interface that says a
            // colour can be taken from outside the window — and on a platform
            // where it cannot, it is the only place that says so. The tool
            // itself is *not* disabled there: picking inside the window works
            // everywhere, so disabling it would take away the half that does
            // work. `syspick::outside_line` is where the two readings
            // live, so the strip cannot say one thing and the module do
            // another.
            ui.label(
                egui::RichText::new("Press on the canvas to take the colour under the pointer.")
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
            // Read afresh rather than against the room measured earlier, for
            // the reason the navigation and selection hints do: the sentence
            // above is drawn unconditionally and is in no budget.
            if ui.available_width() >= strip_budget::EYEDROPPER {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(syspick::outside_line())
                        .size(text::SMALL)
                        .color(p.text_dim),
                )
                .on_hover_text(syspick::outside_detail());
            }
        } else {
            // Pan and Zoom. Two sentences in the Transform hint's register
            // rather than a control, because like Transform there is nothing
            // here to set: the whole gesture is the pointer's.
            let (does, instead, budget) = navigate_hint(ed.ui.tool);
            ui.label(
                egui::RichText::new(does)
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
            // Read afresh rather than against the room measured earlier, for
            // the reason the selection tool's combine line reads it afresh: the
            // sentence above is drawn unconditionally and is in no budget.
            if ui.available_width() >= budget {
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

/// The side of an [`icon_button`], in points, and of the mark inside it.
///
/// This widget's own geometry, beside it rather than in `theme::metrics`, the
/// arrangement `widgets::ICON_TOGGLE` and `PICK_AT`/`PICK_HIT`/`PICK_MARK`
/// already keep. `pub` because `panels::remove_button` is the same square with
/// a warning fill behind it and has to be drawn to the same size, and because
/// the module header lays a strip of these out before its title and therefore
/// has to know what one costs.
///
/// **These were 18 and 18 — the mark filling its whole hit target — and that
/// is what made a module header's marks read as oversized.** The reading that
/// carries it is the *close mark beside them*: `panels::remove_button` was a
/// bare 18 square with `shrink(3.0)`, so one strip drew four marks at 18 and a
/// fifth at 12. That is the inconsistency somebody sees before they can name
/// it, and the mark is 12 now because 12 is what the close mark always was.
///
/// The supporting reading is the header's padding, and it is worth stating
/// exactly rather than generously. [`metrics::PANEL_HEADER`] is 32 and its own
/// doc describes it as the design's 8 px padding around an 11 px line — which
/// sums to 27, not 32, so that sentence does not describe the constant and
/// only its padding half is usable. Taking that half, a control drawn edge to
/// edge in a 32-point header wants to sit inside 16. Nothing in the code
/// enforces such a box: `panels::panel` builds the header at the full 32 and
/// lays the strip into `header.top()..header.bottom()` with no vertical
/// padding at all. So this is an argument from the design's prose about what
/// *ought* to fit, not a bound the program keeps.
///
/// The tool rail is untouched and is not an inconsistency with this: its mark
/// is 18 inside `metrics::TOOL_BUTTON`'s 32-point button, so it has a button's
/// worth of air around it. A header mark has none, which is exactly why it
/// cannot be the same size.
///
/// **The argument is header-shaped and the constant is not**, which is the
/// thing to know before moving it again. Roughly twenty-seven call sites take
/// it, and most are nowhere near a header: the library and stamp browsers'
/// close marks, the brush rename row's tick and cross, the menu bar's cog, the
/// Layers ticked strip. At 16 this is the **smallest interactive target in the
/// interface**, under `widgets::PICK_HIT`'s 18 and `widgets::ICON_TOGGLE`'s 20,
/// which sit in the same panel column. That is deliberate — these are marks
/// with no chrome behind them, where those two are boxes — but it is a floor
/// rather than somewhere with room beneath it.
///
/// **12 is also exactly where the stroke stops thinning.** `icons::draw` takes
/// the weight as `(2 * size / 24).max(1.0)`, so an 18-point mark is stroked at
/// 1.5 and a 12-point one at exactly 1.0: on the floor, not clamped by it.
/// Anything smaller keeps the 1.0 and blots. Worth knowing beside
/// `Palette::active_ink`'s note that this widget reads 1.43:1 on
/// `control_active` in MediaBog — the mark is a third thinner than it was on a
/// control already recorded as marginal there, which is a reason not to take it
/// down again rather than a reason to put it back.
pub const ICON_BUTTON: f32 = 16.0;

/// The mark inside an [`ICON_BUTTON`]. See there.
pub const ICON_BUTTON_MARK: f32 = 12.0;

/// A bare icon that acts as a button, [`ICON_BUTTON`] square. Shared with
/// `panels.rs`.
///
/// A disabled one still hovers, and still shows its tooltip — matching
/// [`crate::controls::icon_button`], and for the same reason. Several callers
/// pass the *reason* it is dead as the tooltip (the brush library's `＋` hands
/// over whatever went wrong with the library file), and while the hover was
/// skipped along with the click, none of those explanations ever reached the
/// screen: what was left was a greyed mark with nothing to say for itself.
pub fn icon_button(ui: &mut egui::Ui, p: &Palette, icon: Icon, enabled: bool, tip: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ICON_BUTTON, ICON_BUTTON),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hovered = enabled && response.hovered();
    icons::draw(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(ICON_BUTTON_MARK, ICON_BUTTON_MARK)),
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
            // `Tweak::span`, shared with the tool options strip's size rail, so
            // the two cannot stop in different places. It is not
            // `Tweak::range`: that is what a size may *be*, and the difference
            // is the whole of `tweaks::SIZE_RAIL_TOP`'s note.
            Tweak::Size.span(),
            true,
            |v| Tweak::Size.format(v),
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
        // Beside Opacity, because the two are the pair people confuse and the
        // only way to learn the difference is to see them together. The floor is
        // `Brush::MIN_FLOW` and not zero: a flow of zero paints nothing, and
        // under the bound a dab is fainter than one level of the coverage
        // scratch, which does not move the accumulator at all.
        widgets::slider_row(
            &mut c[1],
            p,
            "Flow",
            &mut ed.brush.flow,
            Brush::MIN_FLOW..=1.0,
            false,
            percent,
        );
    });
    // Said out loud because the control is otherwise indistinguishable from a
    // second opacity, and reads as one until somebody paints a stroke back over
    // itself. The two sentences are the two states rather than one sentence
    // hedging, so the row at rest says plainly that it is doing nothing.
    caption(
        ui,
        p,
        if ed.brush.flow < 1.0 {
            "Each dab lays down less than the finished mark, so the stroke \
             builds towards it and darkens where it crosses itself. Opacity \
             still caps the whole stroke, once."
        } else {
            "Every dab carries the full mark, so the stroke is as strong at \
             its first dab as anywhere else and crossing it changes nothing. \
             Lower this to make a stroke build."
        },
    );
    ui.add_space(4.0);
    ui.columns(2, |c| {
        widgets::slider_row(
            &mut c[0],
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
                // Both labels step up a rank on the selected row, because that
                // row is the one drawn on `control_active` and the fills the
                // preset themes take from their own applications are bright:
                // `text` on MediaBog's selection blue is 2.60:1 and `text_dim`
                // is 1.43:1. Every other selected row in this interface already
                // inks its primary line `text_strong`; this one did not.
                let (primary, secondary) = if selected {
                    (p.text_strong, p.text)
                } else {
                    (p.text, p.text_dim)
                };
                ui.horizontal(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        egui::RichText::new(format!(
                            "{} \u{2190} {}",
                            entry.target.label(),
                            entry.input.label()
                        ))
                        .size(text::TINY)
                        .color(primary),
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
                            .color(secondary),
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
    // **The mark is already accumulating whenever flow is under 1.0**, because
    // `Brush::builds` is an `||` over the two and nothing downstream reads
    // `build_up` on its own. So the toggle genuinely does nothing here, and the
    // "off" sentence below — which says a stroke is as even where it crosses
    // itself as anywhere else — would be flatly contradicted by the Flow caption
    // two rows up on the same screen. The toggle stays *live* rather than being
    // disabled: it is still the field that will be saved, and it is what the
    // brush goes back to accumulating by if flow is returned to 1.0. What
    // changes is the sentence, which is the part that was lying.
    caption(
        ui,
        p,
        if ed.brush.flow < 1.0 {
            "Flow is already below 100%, so this brush accumulates whichever \
             way this is set. It decides what happens when flow goes back to \
             100%."
        } else if ed.brush.build_up {
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
/// Cached through [`widgets::tip_texture`], which is where the rule lives:
/// validated by the tile's `Arc` identity **and by the ink it was drawn in**.
/// The modal redraws every frame and this would otherwise upload a texture on
/// each of them, while a cache that forgot the colour would keep a tile drawn in
/// the old theme's ink until the paper itself changed. `"brush-paper"` is this
/// square's alone — the name is what the slot is derived from, and the browser
/// draws the same tiles under names of its own, because two consumers sharing a
/// slot evict each other's live texture every frame.
fn paper_preview(ui: &mut egui::Ui, p: &Palette, ed: &Editor) {
    let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
    ui.painter().rect_filled(rect, metrics::RADIUS, p.chrome);

    let Some(tile) = ed.paper_tile() else {
        // A name with nothing behind it. Left as the empty well rather than
        // filled with one of the shipped tiles, because painting flat is
        // exactly what the brush is about to do.
        return;
    };
    let texture = widgets::tip_texture(
        ui.ctx(),
        "brush-paper",
        &tile,
        p.text_strong,
        PAPER_PREVIEW_TEXELS,
    );

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
             same modes a layer has, and the same maths. Applied once, \
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

    /// A piece of text the interface drew, and the rectangle it occupies.
    ///
    /// The rectangle rather than only a point, because two tests want different
    /// things from it: the menu tests aim a click at its centre and match rows
    /// by its baseline, and the strip test asks whether its right edge is still
    /// on the strip.
    #[derive(Clone, Debug)]
    struct Drawn {
        text: String,
        rect: egui::Rect,
    }

    impl Drawn {
        /// Where a click on this string lands.
        fn at(&self) -> egui::Pos2 {
            self.rect.center()
        }
    }

    /// Every string a shape tree paints, with the rectangle it paints into.
    ///
    /// A `Shape::Vec` is what a widget's own painting comes back as, so this
    /// recurses rather than reading the top level.
    fn strings_in(shape: &egui::Shape, out: &mut Vec<Drawn>) {
        match shape {
            egui::Shape::Text(text) => out.push(Drawn {
                text: text.galley.text().to_owned(),
                rect: egui::Rect::from_min_size(text.pos, text.galley.size()),
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
                        .at(),
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
        let mut demanded = 0;
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
                demanded += 1;
            }
        }
        // A floor, because the *whole* selling point of this test is that it
        // reads the set off `Action::category` rather than listing it — and
        // `Menu::of_category` answers `None` for a string it does not know, so
        // a renamed category would quietly stop being demanded and leave this
        // green over an empty loop. Fifteen rows are demanded today.
        // `no_command_category_is_skipped_by_accident` catches the same decay
        // from the other side; this catches it if that list is widened
        // carelessly.
        assert!(
            demanded >= 15,
            "only {demanded} rows were demanded, so a category has stopped being routed"
        );
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
    ///
    /// **The label has to be on the chord's own row**, matched by the shared
    /// baseline `menu_item` draws both on. Asserting only that the label
    /// appears *somewhere* in the menu was the first draft and a review found
    /// it hollow: swapping two rows' labels leaves every label and every chord
    /// present, so Copy wearing Cut's name and Cut wearing Copy's would have
    /// passed. `mutating_a_menu_row_is_caught_by_the_label_test` drives that
    /// exact swap through the same comparison.
    #[test]
    fn a_menu_row_standing_for_a_command_carries_the_commands_own_name() {
        for menu in [Menu::File, Menu::Edit, Menu::View] {
            let mut ed = Editor::default();
            let drawn = menu_strings(menu, &mut ed);
            let checked = check_rows_are_named_for_their_chords(menu, &drawn)
                .unwrap_or_else(|complaint| panic!("{complaint}"));
            // Otherwise a menu that drew no chords at all would pass in
            // silence. Counted per menu rather than over all three, so one
            // going quiet cannot be made up for by the other two.
            assert!(
                checked >= 2,
                "only {checked} rows of the {menu:?} menu carried a chord to check"
            );
        }
    }

    /// The label test's rule, as a function, so the mutation it exists to catch
    /// can be driven through it rather than described in a comment.
    ///
    /// `Ok(n)` having checked `n` rows, `Err` naming the first row whose label
    /// is not its chord's.
    fn check_rows_are_named_for_their_chords(menu: Menu, drawn: &[Drawn]) -> Result<usize, String> {
        let bound: Vec<(Action, String)> = Action::ALL
            .into_iter()
            .filter_map(|a| shortcuts::first_chord(a).map(|c| (a, c)))
            .collect();
        let mut checked = 0;
        for (action, chord) in &bound {
            if bound.iter().filter(|(_, c)| c == chord).count() != 1 {
                continue;
            }
            let Some(row) = drawn.iter().find(|d| &d.text == chord) else {
                continue;
            };
            // A row's label and its chord are drawn on one baseline, so the
            // label is whichever string shares this chord's y. Half a line of
            // slack, because the two galleys are different sizes and their
            // centres need not agree to the last bit of an f32.
            let named = drawn
                .iter()
                .any(|d| d.text == action.label() && (d.at().y - row.at().y).abs() < 6.0);
            if !named {
                return Err(format!(
                    "the {menu:?} menu draws {chord} on a row that is not called {:?}; \
                     it draws {:?}",
                    action.label(),
                    drawn.iter().map(|d| &d.text).collect::<Vec<_>>()
                ));
            }
            checked += 1;
        }
        Ok(checked)
    }

    /// Two rows wearing each other's names is caught.
    ///
    /// The mutation a review found the first draft of the label test blind to:
    /// every label and every chord is still present, so an assertion that only
    /// asks whether the label appears in the menu passes. This builds that
    /// state directly — the same strings the Edit menu draws, with Cut's and
    /// Copy's labels swapped onto each other's baselines — and requires the
    /// rule to refuse it.
    #[test]
    fn mutating_a_menu_row_is_caught_by_the_label_test() {
        let mut ed = Editor::default();
        let honest = menu_strings(Menu::Edit, &mut ed);
        check_rows_are_named_for_their_chords(Menu::Edit, &honest)
            .expect("the Edit menu as drawn is honest");

        // Each label keeps its own text and moves onto the other's row, which
        // is what a call site handing `menu_item` the wrong label would draw.
        let row_of = |label: &str| {
            honest
                .iter()
                .find(|d| d.text == label)
                .unwrap_or_else(|| panic!("the Edit menu draws no {label:?}"))
                .rect
        };
        let (cut, copy) = (row_of(Action::Cut.label()), row_of(Action::Copy.label()));
        let swapped: Vec<Drawn> = honest
            .iter()
            .map(|d| {
                if d.text == Action::Cut.label() {
                    Drawn {
                        text: Action::Copy.label().to_owned(),
                        rect: cut,
                    }
                } else if d.text == Action::Copy.label() {
                    Drawn {
                        text: Action::Cut.label().to_owned(),
                        rect: copy,
                    }
                } else {
                    d.clone()
                }
            })
            .collect();
        assert!(
            check_rows_are_named_for_their_chords(Menu::Edit, &swapped).is_err(),
            "two rows wearing each other's names went unnoticed"
        );
    }

    /// Neither navigation hint runs off the strip it is drawn on.
    ///
    /// The strip is a single unwrapped row, so a sentence too long for it does
    /// not reflow — it carries on past the right edge, off the window. That is
    /// what `navigate_hint`'s budget exists to prevent, and the budget is a
    /// hand-measured number, which is exactly the kind that goes stale when
    /// somebody edits a sentence.
    ///
    /// **Asserted rather than left to the picture test.** The first version of
    /// this said an assertion about a string's width would be a claim about a
    /// font; it is a claim about *Archivo*, which Umber ships and
    /// `theme::install_fonts` installs, and three tests in this crate already
    /// make it — `a_number_row_is_no_wider_than_the_column_it_is_drawn_in` is
    /// the pattern. `options_strip_preview` still shoots 740 and 720 because a
    /// picture answers a different question: whether the line reads well, not
    /// whether it fits.
    ///
    /// Swept rather than sampled, because a budget is wrong over a *range* of
    /// widths: too small a figure draws the sentence at every width between the
    /// budget and what the sentence costs, and a two-point sample can miss the
    /// whole band. This walks every 5 points from far too narrow to far too
    /// wide and requires the content to stay inside the strip at each.
    #[test]
    fn neither_navigation_hint_overruns_the_strip_it_is_drawn_on() {
        use crate::editor::Tool;

        for tool in [Tool::Pan, Tool::Zoom] {
            let ctx = egui::Context::default();
            let palette = Palette::of(ThemeKind::Graphite);
            crate::theme::install_fonts(&ctx);
            crate::theme::apply(&ctx, &palette);
            let mut widest_overrun: Option<(f32, f32)> = None;
            let mut ever_drew_the_second = false;
            for step in 0..140 {
                let width = 200.0 + step as f32 * 5.0;
                let mut ed = Editor::default();
                ed.ui.tool = tool;
                // Two passes: the first through a fresh context builds the font
                // atlas, and text measured against a half-built one is not the
                // width it settles at.
                let mut drawn = Vec::new();
                for _ in 0..2 {
                    drawn.clear();
                    let input = egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            pos2(0.0, 0.0),
                            vec2(width, metrics::OPTIONS_STRIP),
                        )),
                        ..Default::default()
                    };
                    let output = ctx.run_ui(input, |ui| {
                        egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(metrics::STRIP_PAD, 0))
                            .show(ui, |ui| {
                                ui.set_height(metrics::OPTIONS_STRIP);
                                super::options_strip(ui, &palette, &mut ed);
                            });
                    });
                    for clipped in &output.shapes {
                        strings_in(&clipped.shape, &mut drawn);
                    }
                }
                let (_, instead, _) = super::navigate_hint(tool);
                if let Some(second) = drawn.iter().find(|d| d.text == instead) {
                    ever_drew_the_second = true;
                    // The galley's own right edge, read back off the shape
                    // rather than computed from a character count, which is the
                    // whole point of measuring instead of estimating.
                    if second.rect.right() > width {
                        widest_overrun = Some((width, second.rect.right()));
                    }
                }
            }
            assert!(
                ever_drew_the_second,
                "{tool:?}'s second sentence was never drawn at any width, so \
                 this test proved nothing"
            );
            assert!(
                widest_overrun.is_none(),
                "{tool:?}'s second sentence runs to {:.0} points on a {:.0} point strip",
                widest_overrun.unwrap().1,
                widest_overrun.unwrap().0
            );
        }
    }

    /// Every rail the strip draws fits the room its budget claims.
    ///
    /// [`strip_budget`]'s figures are hand-measured, and the strip is a single
    /// unwrapped row — so a budget a few points short does not reflow, it draws
    /// the rail off the right edge of the window. That went from a theoretical
    /// risk to a live one when the readouts became fields: a painted label is
    /// as wide as the figure showing, and a field is as wide as the *widest*
    /// figure its rail can produce, so all four grew at once.
    ///
    /// **What is measured is the width the strip *claimed*, not where its
    /// glyphs landed**, and the first draft got that wrong. A field reserves
    /// `widgets::figure_width` and paints its galley at one end of that box —
    /// the left end, on this shape — so up to three characters of allocated
    /// width carry no shape at all, and the group whose reserve hangs off the
    /// edge is exactly the last one drawn. The frame's own rect is what every
    /// allocation adds up to, which is the reading that cannot miss an empty
    /// reserve; the shapes are read *as well*, because a label in a horizontal
    /// layout extends rather than wrapping and can therefore draw past what it
    /// claimed.
    ///
    /// Swept rather than sampled, for `neither_navigation_hint_overruns_the_
    /// strip_it_is_drawn_on`'s reason: a budget is wrong over a *band* of
    /// widths — from the budget up to what the group actually costs — and the
    /// slack here is a point or two, so the step is one point rather than the
    /// five the first draft used.
    ///
    /// **It sweeps the brush strip and deliberately not the Select one**, and
    /// that is worth stating because the Feather is a rail like the other three
    /// and leaving it out is how a fourth budget goes unmeasured in the very
    /// commit that widened it. Two readings were tried on Select and neither is
    /// an assertion worth shipping. The whole strip's right edge is exceeded
    /// from 200 points up by the **mode hint**, which is drawn unconditionally
    /// and is in no budget at all — `SelectionMode::Polygon`'s is eighty-four
    /// characters — so that reading fails on prose this change did not touch.
    /// Measuring where the rails end instead, by the left edge of the sentence
    /// that follows them, gives a figure 69 points *larger* at 430..445 than at
    /// 446 and above **with the identical groups drawn**, which is an anomaly in
    /// that strip's layout that predates this change and that nobody has
    /// explained. Asserting over a reading nobody understands is worse than not
    /// asserting: it fails on the next unrelated edit and gets silenced rather
    /// than diagnosed. The numbers are here for whoever picks it up, and
    /// [`strip_budget::FEATHER`] was widened by hand instead.
    #[test]
    fn every_rail_on_the_strip_fits_the_budget_that_lets_it_be_drawn() {
        use crate::editor::Tool;
        use std::cell::Cell;

        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx, &palette);

        for (tool, last) in [(Tool::Brush, "Flow")] {
            let mut worst: Option<(f32, f32)> = None;
            let mut ever_drew_the_last = false;
            for step in 0..760 {
                let width = 200.0 + step as f32;
                let mut ed = Editor::default();
                ed.ui.tool = tool;
                let after_the_rails = umber_core::SelectionMode::default().hint();
                // Two passes: the first through a fresh context builds the font
                // atlas, and a field measured against a half-built one is not
                // the width it settles at.
                let claimed = Cell::new(f32::NEG_INFINITY);
                let mut reached = f32::NEG_INFINITY;
                let mut drew_the_last = false;
                for _ in 0..2 {
                    let input = egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            pos2(0.0, 0.0),
                            vec2(width, metrics::OPTIONS_STRIP),
                        )),
                        ..Default::default()
                    };
                    let output = ctx.run_ui(input, |ui| {
                        let frame = egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(metrics::STRIP_PAD, 0))
                            .show(ui, |ui| {
                                ui.set_height(metrics::OPTIONS_STRIP);
                                super::options_strip(ui, &palette, &mut ed);
                            });
                        claimed.set(frame.response.rect.right());
                    });
                    let mut drawn = Vec::new();
                    for clipped in &output.shapes {
                        strings_in(&clipped.shape, &mut drawn);
                    }
                    drew_the_last = drawn.iter().any(|d| d.text == last);
                    reached = match drawn.iter().find(|d| d.text == after_the_rails) {
                        // Where the rails finished: the sentence that follows
                        // them starts there, reserves and all.
                        Some(prose) => prose.rect.left(),
                        // Nothing follows them, so what they claimed *is* the
                        // strip's own width. Read off the frame rather than off
                        // the shapes, because a field reserves the widest figure
                        // its rail can show and paints its galley at one end of
                        // that box — up to three characters of allocated width
                        // carry no shape at all, and the group whose reserve
                        // hangs off the edge is exactly the last one drawn.
                        None => claimed.get(),
                    };
                }
                ever_drew_the_last |= drew_the_last;
                // A rail ending exactly on the strip's own right margin is
                // inside it. Half a point of slack for the frame's rounding.
                if reached.is_finite() && reached > width + 0.5 {
                    worst = Some((width, reached));
                }
            }
            assert!(
                ever_drew_the_last,
                "{tool:?}'s {last} rail was never drawn at any width, so this proved nothing"
            );
            assert!(
                worst.is_none(),
                "{tool:?}'s rails reach {:.1} points on a {:.0} point strip",
                worst.unwrap().1,
                worst.unwrap().0
            );
        }
    }

    /// The eyedropper's second sentence does not run off the strip either.
    ///
    /// The same failure `neither_navigation_hint_overruns_the_strip_it_is_
    /// drawn_on` guards and the same sweep, against [`strip_budget::EYEDROPPER`]
    /// rather than a per-tool figure. A separate test because the sentence does
    /// not come from `navigate_hint` — it comes from `syspick`, and *which* of
    /// its two readings is drawn depends on the platform. Both are within a few
    /// characters of each other, so one budget covers them; what this asserts
    /// is that whichever one this build carries fits.
    #[test]
    fn the_eyedroppers_hint_does_not_overrun_the_strip_it_is_drawn_on() {
        use crate::editor::Tool;

        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx, &palette);

        let sentence = crate::syspick::outside_line();
        let mut widest_overrun: Option<(f32, f32)> = None;
        let mut ever_drew_it = false;
        for step in 0..140 {
            let width = 200.0 + step as f32 * 5.0;
            let mut ed = Editor::default();
            ed.ui.tool = Tool::Eyedropper;
            // Two passes, for the reason the navigation sweep takes two: text
            // measured against a half-built font atlas is not the width it
            // settles at.
            let mut drawn = Vec::new();
            for _ in 0..2 {
                drawn.clear();
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        pos2(0.0, 0.0),
                        vec2(width, metrics::OPTIONS_STRIP),
                    )),
                    ..Default::default()
                };
                let output = ctx.run_ui(input, |ui| {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(metrics::STRIP_PAD, 0))
                        .show(ui, |ui| {
                            ui.set_height(metrics::OPTIONS_STRIP);
                            super::options_strip(ui, &palette, &mut ed);
                        });
                });
                for clipped in &output.shapes {
                    strings_in(&clipped.shape, &mut drawn);
                }
            }
            if let Some(second) = drawn.iter().find(|d| d.text == sentence) {
                ever_drew_it = true;
                if second.rect.right() > width {
                    widest_overrun = Some((width, second.rect.right()));
                }
            }
        }
        assert!(
            ever_drew_it,
            "the eyedropper's second sentence was never drawn at any width, so \
             this test proved nothing"
        );
        assert!(
            widest_overrun.is_none(),
            "it runs to {:.0} points on a {:.0} point strip",
            widest_overrun.unwrap().1,
            widest_overrun.unwrap().0
        );
    }

    /// Every category [`Action::category`] can answer with is either a menu
    /// this test knows or a category deliberately not on the menu bar.
    ///
    /// [`Menu::of_category`] answers `None` for anything it does not recognise,
    /// and `None` means "needs no row" — the unsafe default. Without this, a
    /// command filed under a new category would be skipped by the coverage test
    /// in silence, which is the failure that test exists to prevent.
    #[test]
    fn no_command_category_is_skipped_by_accident() {
        // Tools, Brush and Colour are reached from the rail, the brush editor
        // and the colour panel. They are deliberately not menu bar commands,
        // and a category leaving this list has to be decided about rather than
        // fall through `of_category`'s wildcard.
        const OFF_THE_MENU_BAR: [&str; 3] = ["Tools", "Brush", "Colour"];
        for action in Action::ALL {
            let category = action.category();
            assert!(
                Menu::of_category(category).is_some() || OFF_THE_MENU_BAR.contains(&category),
                "{:?} is filed under {category:?}, which is neither a menu nor \
                 named as off the menu bar",
                action.label()
            );
        }
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
    ///
    /// **Cut is clicked on an unlocked layer as well as a locked one**, and the
    /// first draft was not. A review pointed out that asserting only the
    /// refusal is satisfied by a Cut that is dead in every state — mutating the
    /// gate to a plain `false` left it green — so what it proved was "Cut is
    /// dead when locked" rather than "Cut answers to the lock". It also makes
    /// the negative half mean something: a click that missed the row entirely
    /// would satisfy `!cut_selection` for the wrong reason, and the unlocked
    /// case is what says the aim is good.
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

        let mut ed = Editor::default();
        assert!(
            !ed.layers.active_is_locked(),
            "a fresh editor's layer is supposed to be unlocked"
        );
        let actions = click_menu_row(Menu::Edit, &mut ed, Action::Cut.label());
        assert!(
            actions.cut_selection,
            "Cut was dead on an unlocked layer, so the row answers to nothing"
        );
    }

    /// The Edit menu's Undo and Redo rows go dead when a locked layer refuses
    /// the canvas flip they would step over, and stay live for anything else.
    ///
    /// **This is the panel half, and it is the half a model test cannot
    /// reach.** `Editor::undo_gate`'s own guard measures the rule and cannot
    /// see whether `edit_menu` calls it — reverting that one call site leaves
    /// it green, which is the failure CLAUDE.md records for
    /// `is_bold_anchor`. So this clicks the row `click_menu_row` finds, and a
    /// disabled row swallows the click.
    ///
    /// **Three states, not two**, and the third is what makes it a test of the
    /// rule rather than of the lock. A locked layer over a *paint* entry must
    /// leave Undo live: a gate that refused every kind while anything was
    /// locked would make Ctrl+Z inert for a painter who had locked a reference
    /// layer, and asserting only the flip case would pass under that rule too.
    /// The unlocked flip case is what says the aim is good, exactly as
    /// `cut_answers_to_the_lock_and_copy_does_not` argues.
    ///
    /// The redo side is driven by pushing the entry across, which is what
    /// `App::undo` does to it.
    #[test]
    fn the_edit_menus_history_rows_go_dead_when_a_lock_refuses_the_flip() {
        use umber_core::{Edit, EditBody, EditKind};

        /// An editor holding one entry of `kind`, with the layer locked or not.
        fn ready(kind: EditKind, locked: bool, redo: bool) -> Editor {
            let mut ed = Editor::default();
            ed.history.record(Edit::new(kind, EditBody::Flip));
            if redo {
                let edit = ed.history.take_undo().expect("the entry just recorded");
                ed.history.push_redo(edit);
            }
            ed.layers.active_mut().locked = locked;
            ed
        }

        // A flip with nothing locked: both rows live, so the aim is good and
        // the fixture reaches the rows at all.
        let mut ed = ready(EditKind::FlipHorizontal, false, false);
        assert!(
            click_menu_row(Menu::Edit, &mut ed, Action::Undo.label()).undo,
            "Undo was dead over an unlocked flip"
        );
        let mut ed = ready(EditKind::FlipHorizontal, false, true);
        assert!(
            click_menu_row(Menu::Edit, &mut ed, Action::Redo.label()).redo,
            "Redo was dead over an unlocked flip"
        );

        // The same flip with a layer locked: both rows dead.
        let mut ed = ready(EditKind::FlipHorizontal, true, false);
        assert!(
            !click_menu_row(Menu::Edit, &mut ed, Action::Undo.label()).undo,
            "Undo was live over a flip a lock refuses, so the menu offers what \
             `App::settle_step` will then decline"
        );
        let mut ed = ready(EditKind::FlipVertical, true, true);
        assert!(
            !click_menu_row(Menu::Edit, &mut ed, Action::Redo.label()).redo,
            "Redo was live over a flip a lock refuses"
        );

        // And a *paint* entry with the same lock: still live, both ways. This
        // is the assertion that fails if the gate is widened into "a lock
        // refuses the history".
        let mut ed = ready(EditKind::Paint, true, false);
        assert!(
            click_menu_row(Menu::Edit, &mut ed, Action::Undo.label()).undo,
            "a locked layer killed Undo over a stroke, which nothing refuses"
        );
        let mut ed = ready(EditKind::Paint, true, true);
        assert!(
            click_menu_row(Menu::Edit, &mut ed, Action::Redo.label()).redo,
            "a locked layer killed Redo over a stroke, which nothing refuses"
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
    /// `chrome` in Graphite alone is how an ink at 1.06:1 in Paper survived
    /// being looked at. (1.07 here and in `widgets.rs` until `contrast::ratio`
    /// was asked; `rail` on Paper's pit is 1.0589.)
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
            // Krita's pit is the 50% grey, which is the surface the token this
            // thumb used to be inked with could say nothing on. A shot of the
            // two extremes says nothing about the middle, which is exactly the
            // mistake recorded above one level up.
            (ThemeKind::Krita, "krita"),
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

    // -----------------------------------------------------------------------
    // The eyedropper's loupe
    // -----------------------------------------------------------------------

    /// Every flat-filled rectangle a run of the interface produced, with its
    /// colour, recursing through `Shape::Vec` for `strings_in`'s reason.
    fn rects_in(shape: &egui::Shape, out: &mut Vec<(egui::Rect, egui::Color32)>) {
        match shape {
            egui::Shape::Rect(r) => out.push((r.rect, r.fill)),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    rects_in(shape, out);
                }
            }
            _ => {}
        }
    }

    /// The same for filled circles, which is what the rim and the
    /// single-colour fallback are drawn as.
    fn circles_in(shape: &egui::Shape, out: &mut Vec<(f32, egui::Color32)>) {
        match shape {
            egui::Shape::Circle(c) => out.push((c.radius, c.fill)),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    circles_in(shape, out);
                }
            }
            _ => {}
        }
    }

    /// Draw one loupe into a fresh context and hand back what it painted and
    /// the context it painted into.
    fn draw_loupe(seen: crate::loupe::Loupe) -> (egui::Context, Vec<(egui::Rect, egui::Color32)>) {
        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx, &palette);
        let mut ed = Editor::default();
        ed.ui.tool = crate::editor::Tool::Eyedropper;
        ed.cursor = seen.at;
        ed.loupe = Some(seen);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0))),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            super::loupe_overlay(ui, &palette, &ed);
        });
        let mut rects = Vec::new();
        for clipped in &output.shapes {
            rects_in(&clipped.shape, &mut rects);
        }
        (ctx, rects)
    }

    /// A full patch of one colour, with the middle overridden.
    fn patch_of(all: [u8; 3], middle: Option<[u8; 3]>) -> crate::loupe::Patch {
        let cells = crate::loupe::CELLS;
        let mut texels = vec![Some(all); (cells * cells) as usize];
        let m = (cells / 2) as usize;
        texels[m * cells as usize + m] = middle;
        crate::loupe::Patch::new(cells, texels).expect("a patch")
    }

    #[test]
    fn the_loupe_draws_the_colour_a_release_would_take_and_not_the_block_it_read() {
        // **Two readings that disagree**, which is the only way to test which
        // of them the middle cell is drawn from. Off the screen they can
        // genuinely differ — the colour is the block's middle texel, but a
        // `BitBlt` that failed falls back to `syspick::sample` and there is no
        // rule saying the fallback and a stale patch agree — and over the
        // canvas they never do, so a fixture where both are the same value
        // would pass under either reading. That is CLAUDE.md's rule about a
        // two-state reading applied to a painter.
        let taken = umber_core::Color::from_srgb_u8(220, 30, 140, 255);
        let (_, rects) = draw_loupe(crate::loupe::Loupe {
            at: glam::Vec2::new(600.0, 400.0),
            taken: Some(taken),
            patch: Some(patch_of([12, 200, 60], Some([9, 9, 9]))),
        });
        let wanted = egui::Color32::from_rgb(220, 30, 140);
        let block = egui::Color32::from_rgb(9, 9, 9);
        assert!(
            rects.iter().any(|(_, fill)| *fill == wanted),
            "the colour a release takes is drawn"
        );
        assert!(
            !rects.iter().any(|(_, fill)| *fill == block),
            "and the block's own middle texel is not"
        );
        // The rest of the block is what it read, so the loupe is a picture and
        // not a swatch.
        assert!(
            rects
                .iter()
                .any(|(_, fill)| *fill == egui::Color32::from_rgb(12, 200, 60)),
            "the neighbours are the block's"
        );
    }

    #[test]
    fn a_texel_the_loupe_could_not_read_is_left_blank_rather_than_filled() {
        // Off every monitor, off the canvas, or fully transparent. Drawing
        // black there would be a control that lies about what is under the
        // pointer, so exactly one fewer cell is painted.
        let at = glam::Vec2::new(600.0, 400.0);
        let taken = umber_core::Color::from_srgb_u8(10, 10, 10, 255);
        let full = patch_of([80, 80, 80], Some([80, 80, 80]));
        let mut texels =
            vec![Some([80u8, 80, 80]); (crate::loupe::CELLS * crate::loupe::CELLS) as usize];
        // A texel next to the middle, so it is nowhere near the circle's
        // boundary and cannot have been dropped by the clipping instead.
        let m = (crate::loupe::CELLS / 2) as usize;
        texels[m * crate::loupe::CELLS as usize + m + 1] = None;
        let holed = crate::loupe::Patch::new(crate::loupe::CELLS, texels).expect("a patch");

        let (_, with_all) = draw_loupe(crate::loupe::Loupe {
            at,
            taken: Some(taken),
            patch: Some(full),
        });
        let (_, with_hole) = draw_loupe(crate::loupe::Loupe {
            at,
            taken: Some(taken),
            patch: Some(holed),
        });
        assert_eq!(
            with_all.len(),
            with_hole.len() + 1,
            "exactly the one unread texel goes unpainted"
        );
    }

    #[test]
    fn the_loupe_claims_no_pointer() {
        // It is a bare layer painter and not an `Area`, which is what keeps it
        // out of `Memory::layer_id_at` — the reading `ui_owns_pointer` and
        // `Editor::pen_dot` both go through. An `Area` here would be a
        // magnifier that refused the press it exists to aim, and the loupe
        // sits directly beside the pointer, so it is the one overlay in this
        // interface that would certainly be under it.
        let at = glam::Vec2::new(600.0, 400.0);
        let (ctx, _) = draw_loupe(crate::loupe::Loupe {
            at,
            taken: Some(umber_core::Color::from_srgb_u8(10, 10, 10, 255)),
            patch: Some(patch_of([80, 80, 80], Some([80, 80, 80]))),
        });
        // Where `loupe::place` puts it for a pointer with room above.
        let centre = pos2(at.x, at.y - crate::loupe::OUTER - crate::loupe::CLEARANCE);
        let layer = ctx.layer_id_at(centre);
        assert!(
            layer.is_none_or(|l| l.order == egui::Order::Background),
            "the loupe registered an area at {centre:?}: {layer:?}"
        );
    }

    #[test]
    fn a_loupe_with_no_neighbourhood_shows_the_one_colour_it_has() {
        // The `BitBlt` failed but the pixel was read. A fake grid would be the
        // thing this control may not do, so the whole disc is the colour — and
        // that is a decision rather than a gap, which is why it is pinned.
        //
        // **Both halves**, because "no grid" alone is what a loupe that drew
        // nothing at all would also satisfy: the grid is rectangles and the
        // disc is a circle, so an assertion about rectangles cannot see it.
        let at = glam::Vec2::new(600.0, 400.0);
        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx, &palette);
        let mut ed = Editor::default();
        ed.ui.tool = crate::editor::Tool::Eyedropper;
        ed.cursor = at;
        ed.loupe = Some(crate::loupe::Loupe {
            at,
            taken: Some(umber_core::Color::from_srgb_u8(7, 130, 240, 255)),
            patch: None,
        });
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0))),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| super::loupe_overlay(ui, &palette, &ed));

        let (mut rects, mut circles) = (Vec::new(), Vec::new());
        for clipped in &output.shapes {
            rects_in(&clipped.shape, &mut rects);
            circles_in(&clipped.shape, &mut circles);
        }
        assert!(
            rects.is_empty(),
            "no grid is drawn where none was read: {rects:?}"
        );
        assert!(
            circles
                .iter()
                .any(|(r, fill)| *fill == egui::Color32::from_rgb(7, 130, 240)
                    && (*r - crate::loupe::RADIUS).abs() < 0.01),
            "the disc is the colour it did read: {circles:?}"
        );
    }

    #[test]
    #[ignore = "wants a GPU and `Stage` is not `gputest`'s device; run deliberately"]
    #[cfg(debug_assertions)]
    fn the_cpu_frame_sampler_agrees_with_the_gpu() {
        // **Validating the instrument.** Three guards quote figures out of
        // `frame_pixel`, and a CPU rasteriser that quietly disagreed with the
        // one that draws would make all three arguments about nothing. So the
        // same frame goes through both and every pixel of the loupe is
        // compared.
        //
        // It is `#[ignore]`d, and that is a real limit rather than a
        // preference: it wants a device, and `docshot::Stage` builds its own
        // rather than taking `gputest::lock()`, so it may not sit in a suite the
        // harness runs on parallel threads. The previews beside it are ignored
        // for the same reason.
        //
        // The loupe is the fixture because it is *static* — the marquee asks
        // egui's clock for its dash phase, and `Stage::shoot` runs frames until
        // nothing is animating, so the two would be a sixtieth of a second
        // apart in the pattern. Measured over 160,000 device pixels: 156,763
        // agree exactly, 2,923 are a level out, 278 two and 36 three, and the
        // worst of them sit on the silhouette where the disc's feather is. That
        // is egui's optional dithering and the rounding either side of it, and
        // the bound below is what that reading actually is rather than a round
        // number beside it.
        use crate::docshot;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let palette = Palette::of(ThemeKind::Graphite);
        let field = vec2(200.0, 200.0);
        let scale = 2.0;
        let at = glam::Vec2::new(100.0, 170.0);
        let cells = crate::loupe::CELLS;
        let mut texels = Vec::new();
        for row in 0..cells {
            for col in 0..cells {
                texels.push(
                    (col + row < cells)
                        .then_some([32u8, 44, 70])
                        .or(Some([224, 196, 120])),
                );
            }
        }
        let patch = crate::loupe::Patch::new(cells, texels).expect("a patch");
        let body = |ui: &mut egui::Ui| {
            let mut ed = Editor::default();
            ed.ui.tool = crate::editor::Tool::Eyedropper;
            ed.cursor = at;
            ed.loupe = Some(crate::loupe::Loupe {
                at,
                taken: Some(umber_core::Color::from_srgb_u8(224, 196, 120, 255)),
                patch: Some(patch.clone()),
            });
            super::loupe_overlay(ui, &palette, &ed);
        };

        // Geometry on both sides, for the reason `frame_pixel` gives: a
        // pre-rasterised disc is a textured quad this cannot read, and the
        // option changes nothing else about the picture.
        stage
            .ctx
            .tessellation_options_mut(|o| o.prerasterized_discs = false);
        let image = stage.shoot(field, scale, &palette, palette.backdrop, body);
        let prims = frame_at_geometric(&palette, field, scale, body);

        let (w, h) = (
            (field.x * scale).round() as u32,
            (field.y * scale).round() as u32,
        );
        let (mut worst, mut worst_at) = (0i32, (0u32, 0u32));
        let mut histogram = [0u32; 8];
        for y in 0..h {
            for x in 0..w {
                let sample = pos2((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale);
                let cpu = frame_pixel(&prims, palette.backdrop, sample);
                let gpu = image.pixel(x, y);
                let off = [
                    (cpu.r() as i32 - gpu.r() as i32).abs(),
                    (cpu.g() as i32 - gpu.g() as i32).abs(),
                    (cpu.b() as i32 - gpu.b() as i32).abs(),
                ]
                .into_iter()
                .max()
                .unwrap_or(0);
                histogram[(off as usize).min(7)] += 1;
                if off > worst {
                    worst = off;
                    worst_at = (x, y);
                }
            }
        }
        println!("worst deviation {worst} of 255, at {worst_at:?}; by level {histogram:?}");
        assert!(
            worst <= 3,
            "the CPU sampler is {worst} levels from the GPU at {worst_at:?}"
        );
    }

    #[test]
    fn the_loupes_picture_fills_its_own_circle() {
        // **The stepped edge, measured.** What made the old loupe read as a
        // mosaic is that a grid of six-point cells clipped to its own circle
        // covers a staircase strictly *inside* that circle — the top row of an
        // eleven-cell grid reaches nowhere near the top of the disc — so the
        // outline the eye followed was the corner of whichever cell got
        // furthest. Now the cells are clipped a cell wider than the disc and
        // the rim covers the overhang, so the boundary is a circle.
        //
        // The property that says so is coverage: every point of the disc has to
        // be inside some drawn cell. That is a reading of the rectangles that
        // were actually painted, not of the clip radius — reverting `reach` to
        // `loupe::RADIUS` fails it at the top of the disc, which is where the
        // old picture was worst.
        let at = glam::Vec2::new(600.0, 400.0);
        let (_, rects) = draw_loupe(crate::loupe::Loupe {
            at,
            taken: Some(umber_core::Color::from_srgb_u8(80, 80, 80, 255)),
            patch: Some(patch_of([80, 80, 80], Some([80, 80, 80]))),
        });
        let centre = pos2(at.x, at.y - crate::loupe::OUTER - crate::loupe::CLEARANCE);
        // Just inside the boundary, because a point exactly on it is a question
        // about the last bit of floating point rather than about the picture.
        let r = crate::loupe::RADIUS - 0.05;
        let mut missed = Vec::new();
        for step in 0..720 {
            let angle = step as f32 / 720.0 * std::f32::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            for reach in [0.999f32, 0.9, 0.5] {
                let p = centre + vec2(cos, sin) * (r * reach);
                if !rects.iter().any(|(rect, _)| rect.contains(p)) {
                    missed.push(p);
                }
            }
        }
        assert!(
            missed.is_empty(),
            "{} points of the lens are not covered by any cell, e.g. {:?}",
            missed.len(),
            &missed[..missed.len().min(3)]
        );
    }

    #[test]
    fn the_loupe_draws_nothing_in_popover_border() {
        // What the artist asked for, and it is checkable because the old
        // outline was a token: `popover_border` is the hairline that used to
        // ring the loupe, and a lens does not have one. Everything that
        // separates it from the canvas now is shading — the rim turning from
        // the light — which is the guard below.
        //
        // Read off the shapes rather than off the source, so a stroke reaching
        // that colour by another route is caught too.
        //
        // **The name is what it checks and not the general property**, which is
        // the honest half: a dark ring hand-mixed, or taken from `p.border`, or
        // drawn in `contrast::SHADE` at full strength would pass this. It was
        // called `the_loupe_draws_no_outline`, which promised the general
        // property, and a name that overstates is what the next reader trusts
        // instead of looking. Testing the general property means measuring
        // whether the boundary has a *ring* in it, which is the shading guard's
        // job from the other side.
        let palette = Palette::of(ThemeKind::Graphite);
        let at = glam::Vec2::new(600.0, 400.0);
        let prims = frame_at(&palette, vec2(1280.0, 800.0), 2.0, |ui| {
            let mut ed = Editor::default();
            ed.ui.tool = crate::editor::Tool::Eyedropper;
            ed.cursor = at;
            ed.loupe = Some(crate::loupe::Loupe {
                at,
                taken: Some(umber_core::Color::from_srgb_u8(80, 80, 80, 255)),
                patch: Some(patch_of([80, 80, 80], Some([80, 80, 80]))),
            });
            super::loupe_overlay(ui, &palette, &ed);
        });
        let border = palette.popover_border;
        for prim in &prims {
            let egui::epaint::Primitive::Mesh(mesh) = &prim.primitive else {
                continue;
            };
            assert!(
                !mesh.vertices.iter().any(|v| v.color == border),
                "the loupe still draws something in `popover_border`"
            );
        }
    }

    #[test]
    fn the_loupes_glass_darkens_towards_its_edge_and_catches_the_light_there() {
        // **The other three quarters of `loupe_glass`.** The rim guard beside
        // this one samples at `RIM * 0.37`, which is in the rim's shading band
        // and in none of the passes that lie on the *picture* — so the
        // thickness and the catch-light could both have been deleted with every
        // guard here still green, and `GLASS_EDGE.2`, the counter-lobe the
        // comments call the thing that tells glass from a moulded button, had
        // no measurement at all. That was found by a critic reading the radii
        // rather than by anything failing.
        //
        // The fixture is one flat colour across the whole block deliberately:
        // every sample below then differs *only* by what the glass put on it,
        // so a difference between two of them cannot be the picture underneath.
        //
        // Radii and angles are off `glass_band`'s stops and off its segment
        // boundaries — a segment turns through 3.75 degrees. That is belt and
        // braces rather than load-bearing now that `triangle_at` has a fill
        // rule; before it, a sample on a stop composited its wash twice.
        let palette = Palette::of(ThemeKind::Graphite);
        let at = glam::Vec2::new(600.0, 400.0);
        let prims = frame_at(&palette, vec2(1280.0, 800.0), 2.0, |ui| {
            let mut ed = Editor::default();
            ed.ui.tool = crate::editor::Tool::Eyedropper;
            ed.cursor = at;
            ed.loupe = Some(crate::loupe::Loupe {
                at,
                taken: Some(umber_core::Color::from_srgb_u8(80, 80, 80, 255)),
                patch: Some(patch_of([80, 80, 80], Some([80, 80, 80]))),
            });
            super::loupe_overlay(ui, &palette, &ed);
        });
        let centre = pos2(at.x, at.y - crate::loupe::OUTER - crate::loupe::CLEARANCE);
        let r = crate::loupe::RADIUS;
        let seen = |radius: f32, degrees: f32| {
            let a = degrees.to_radians();
            let p = centre + vec2(a.cos(), a.sin()) * radius;
            crate::theme::contrast::luminance(frame_pixel(&prims, palette.backdrop, p))
        };

        // The thickness: further out is darker. Both radii are inside the
        // thickness band and outside the catch-light's, so nothing else is on
        // them.
        let middle = seen(r * 0.25, 97.3);
        let deep = seen(r * 0.85, 97.3);
        assert!(
            deep < middle,
            "the glass does not darken towards its edge: {deep:.4} at 0.85 of \
             the radius against {middle:.4} at 0.25"
        );

        // The catch-light: at the boundary, facing the light, the glass is
        // *lighter* than the thickness alone left it further in. Take the
        // thickness band's own outer wash away and the rim goes darker instead.
        let rim_lit = seen(r - 1.1, 227.3);
        assert!(
            rim_lit > deep,
            "the boundary does not catch the light: {rim_lit:.4} against \
             {deep:.4} where there is thickness and no catch-light"
        );

        // The counter-lobe: the far side of the boundary is brighter than the
        // quarter turn between the two lobes, where only `GLASS_EDGE.0` is
        // left. Setting `GLASS_EDGE.2` to zero makes these two equal.
        let rim_away = seen(r - 1.1, 47.3);
        let rim_across = seen(r - 1.1, 137.3);
        assert!(
            rim_away > rim_across,
            "the far side of the boundary does not carry the counter-lobe: \
             {rim_away:.4} against {rim_across:.4} across the light"
        );
        // And it is the weaker of the two, or it is not a counter-lobe.
        assert!(
            rim_away < rim_lit,
            "the counter-lobe is not weaker than the highlight: {rim_away:.4} \
             against {rim_lit:.4}"
        );
    }

    #[test]
    fn the_loupes_rim_turns_from_the_light() {
        // **The whole of "it reads as glass rather than as a disc"**, and it is
        // a statement about pixels rather than about the constants that produce
        // them: the rim towards `GLASS_LIGHT` is at least as light as the body
        // it is made of, and the rim opposite is darker — "at least", because
        // one theme's body is already at the end of the axis, which is the
        // second half of this test. Delete either half of
        // `loupe_glass`'s shading and this fails; change the light's direction
        // and it fails the other way round, which is what a guard reading the
        // constant back could not do.
        //
        // Driven in **both** themes deliberately. `contrast::LIT` and
        // `contrast::SHADE` are ends of the axis rather than inks chosen
        // against a surface, so the claim is that they still straddle a rim
        // whose own colour is near-black in one theme and white in the other —
        // and a shading pass written as one ink plus `ink_on` would pass in
        // Graphite and be a flat wash in Paper.
        for kind in [ThemeKind::Graphite, ThemeKind::Paper] {
            let palette = Palette::of(kind);
            let at = glam::Vec2::new(600.0, 400.0);
            let prims = frame_at(&palette, vec2(1280.0, 800.0), 2.0, |ui| {
                let mut ed = Editor::default();
                ed.ui.tool = crate::editor::Tool::Eyedropper;
                ed.cursor = at;
                ed.loupe = Some(crate::loupe::Loupe {
                    at,
                    taken: Some(umber_core::Color::from_srgb_u8(80, 80, 80, 255)),
                    patch: Some(patch_of([80, 80, 80], Some([80, 80, 80]))),
                });
                super::loupe_overlay(ui, &palette, &ed);
            });
            let centre = pos2(at.x, at.y - crate::loupe::OUTER - crate::loupe::CLEARANCE);
            // The middle of the rim band, where its shading is peaked.
            // Off the stops and off the segment boundaries: `RIM * 0.37` is
            // between the shading band's inner and middle stops — short of the
            // peak at the middle one, deliberately, so the reading is of the
            // band rather than of its brightest ring — and 227.3 degrees is not
            // a multiple of the 3.75 a segment turns through.
            //
            // That is belt and braces rather than load-bearing. Before
            // `triangle_at` had a fill rule a sample on a stop composited its
            // wash twice and read as an extreme, which is how this radius was
            // chosen; the fill rule fixed the cause, and this stays because a
            // sample that cannot land on a seam is one fewer thing to reason
            // about.
            let band = crate::loupe::RADIUS + crate::loupe::RIM * 0.37;
            let sample = |degrees: f32| {
                let a = degrees.to_radians();
                let p = centre + vec2(a.cos(), a.sin()) * band;
                let lit = frame_pixel(&prims, palette.backdrop, p);
                crate::theme::contrast::luminance(lit)
            };
            let body = crate::theme::contrast::luminance(palette.popover);
            let towards = sample(227.3);
            let away = sample(47.3);
            assert!(
                towards >= body && away < body,
                "{kind:?}: the rim reads {towards:.4} towards the light and {away:.4} away from it, against a body of {body:.4}"
            );
            // **Only one half of the pair does any work in Paper**, and that is
            // stated rather than hidden: its `popover` is pure white, so
            // nothing can be lighter than it and the whole of what turns the
            // rim there is the shadow. Same shape as `accent_underlay`'s own
            // admission that a pair of one extreme and one mid tone cannot
            // reach its target on every artwork. So what is held to a floor is
            // the *separation* between the two sides, which is what an eye
            // reads as roundness — and the floor is what ships, Paper's 4.67
            // against Graphite's 6.73, not a round number chosen beside it.
            let turn = (towards.max(away) + 0.05) / (towards.min(away) + 0.05);
            assert!(
                turn >= 4.6,
                "{kind:?}: the rim only turns by {turn:.2}:1 from one side to the other"
            );
        }
    }

    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn loupe_preview() {
        use crate::docshot;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/loupe");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        // A neighbourhood with structure in it, so the grid is legible as a
        // grid rather than as a disc: a diagonal edge, a couple of holes where
        // nothing could be read, and a middle that is neither.
        let cells = crate::loupe::CELLS;
        let patch = |hole: bool| {
            let mut texels = Vec::new();
            for row in 0..cells {
                for col in 0..cells {
                    let dark = col + row < cells;
                    let missing = hole && col + 2 >= cells;
                    texels.push(if missing {
                        None
                    } else if dark {
                        Some([32, 44, 70])
                    } else {
                        Some([224, 196, 120])
                    });
                }
            }
            crate::loupe::Patch::new(cells, texels).expect("a patch")
        };

        let field = vec2(420.0, 320.0);
        let mut written = 0;
        for (theme, ink) in [
            (ThemeKind::Graphite, "graphite"),
            (ThemeKind::Paper, "paper"),
        ] {
            let palette = Palette::of(theme);
            for (name, at, taken, held) in [
                (
                    "1-middle",
                    vec2(210.0, 200.0),
                    Some([224, 196, 120]),
                    Some(patch(false)),
                ),
                (
                    "2-against-the-top",
                    vec2(210.0, 12.0),
                    Some([32, 44, 70]),
                    Some(patch(false)),
                ),
                (
                    "3-with-holes",
                    vec2(120.0, 200.0),
                    Some([224, 196, 120]),
                    Some(patch(true)),
                ),
                (
                    "4-outside-the-window",
                    vec2(900.0, 160.0),
                    Some([200, 40, 40]),
                    Some(patch(false)),
                ),
                (
                    "5-one-colour",
                    vec2(300.0, 200.0),
                    Some([60, 170, 120]),
                    None,
                ),
                ("6-nothing-there", vec2(300.0, 200.0), None, None),
            ] {
                let mut ed = Editor::default();
                ed.ui.theme = theme;
                ed.ui.tool = crate::editor::Tool::Eyedropper;
                ed.cursor = glam::Vec2::new(at.x, at.y);
                ed.loupe = Some(crate::loupe::Loupe {
                    at: glam::Vec2::new(at.x, at.y),
                    taken: taken.map(|[r, g, b]| umber_core::Color::from_srgb_u8(r, g, b, 255)),
                    patch: held,
                });
                let image = stage.shoot(field, 2.0, &palette, palette.backdrop, |ui| {
                    // A crosshair stand-in where the pointer is, so the gap the
                    // clearance buys can be judged rather than taken on trust.
                    if at.x < field.x {
                        let p = pos2(at.x, at.y);
                        ui.painter().line_segment(
                            [p - vec2(6.0, 0.0), p + vec2(6.0, 0.0)],
                            egui::Stroke::new(1.0, palette.text),
                        );
                        ui.painter().line_segment(
                            [p - vec2(0.0, 6.0), p + vec2(0.0, 6.0)],
                            egui::Stroke::new(1.0, palette.text),
                        );
                    }
                    super::loupe_overlay(ui, &palette, &ed);
                });
                docshot::write_png(&dir.join(format!("{ink}-{name}.png")), &image)
                    .expect("write the png");
                written += 1;
            }
        }
        println!("wrote {written} shots to {}", dir.display());
    }

    // -----------------------------------------------------------------------
    // Reading a finished frame back without a device
    // -----------------------------------------------------------------------

    /// The colour a triangle carries at `at`, or `None` where it does not cover
    /// it.
    ///
    /// Barycentric, with a **fill rule**, and the fill rule is not decoration:
    /// without one a point on the seam between two triangles of one band is
    /// covered by both, so a translucent wash composites twice. The loupe's
    /// glass is bands of triangles about a centre, its fixture has a diagonal
    /// running at exactly forty-five degrees, and a segment boundary sits there
    /// — so the seam case is the common case rather than a curiosity, and it
    /// read eight levels out against the GPU until this was written.
    ///
    /// A point within a thousandth of a point of an edge is treated as *on* it
    /// and claimed by the triangle whose winding makes that edge a top or left
    /// one. Two triangles sharing an edge traverse it in opposite directions,
    /// and `e.x - s.x` and `s.x - e.x` are exact negatives in IEEE, so exactly
    /// one of them claims it however the arithmetic rounds — which is the part
    /// a plain `== 0.0` test would get wrong, since the two edge functions are
    /// evaluated from different vertices and need not both land on zero.
    fn triangle_at(v: [&egui::epaint::Vertex; 3], at: egui::Pos2) -> Option<egui::Color32> {
        let cross = |a: egui::Vec2, b: egui::Vec2| a.x * b.y - a.y * b.x;
        let area = cross(v[1].pos - v[0].pos, v[2].pos - v[0].pos);
        if area.abs() < 1e-9 {
            return None;
        }
        // Wound so the interior is where every edge function is positive.
        let order = if area > 0.0 { [0, 1, 2] } else { [0, 2, 1] };
        let mut weight = [0.0f32; 3];
        for k in 0..3 {
            let (s, e) = (v[order[(k + 1) % 3]].pos, v[order[(k + 2) % 3]].pos);
            let along = e - s;
            let side = cross(along, at - s);
            let tol = 1e-3 * along.length().max(1e-6);
            if side < -tol {
                return None;
            }
            if side <= tol {
                // On the edge: only the triangle that traverses it downwards,
                // or leftwards along a horizontal, may claim it.
                let claims = along.y > 0.0 || (along.y == 0.0 && along.x < 0.0);
                if !claims {
                    return None;
                }
            }
            weight[k] = side / area.abs();
        }
        let mix = |f: fn(&egui::Color32) -> u8| {
            (0..3)
                .map(|k| weight[k] * f(&v[order[k]].color) as f32)
                .sum::<f32>()
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Some(egui::Color32::from_rgba_premultiplied(
            mix(|c| c.r()),
            mix(|c| c.g()),
            mix(|c| c.b()),
            mix(|c| c.a()),
        ))
    }

    /// What the renderer would put at one device pixel of a finished frame,
    /// over `under`.
    ///
    /// **The only way to ask "what colour came out" with no device**, which is
    /// what these two features need: an appearance change that cannot be
    /// measured can only be argued about. It is egui's own tessellation and
    /// then a point sample of every triangle in order, which is what a
    /// rasteriser does at a pixel centre.
    ///
    /// It is exact rather than approximate over most of a frame, and where it is
    /// not is worth knowing before trusting a figure out of it.
    /// `fs_main_gamma_framebuffer` — the entry point a non-sRGB target picks,
    /// which is the one Umber's surface and `docshot` both use — multiplies the
    /// interpolated vertex colour by the texture and writes it through, and the
    /// interpolation is over `unpack_color`'s bytes, so it happens in the same
    /// gamma space the bytes are already in. So a fragment's colour *is* its
    /// vertex colour wherever the texel is white, which is every shape these
    /// guards read.
    ///
    /// **The exception is a filled circle**, and it is not obvious:
    /// `TessellationOptions::prerasterized_discs` is on by default, so
    /// `circle_filled` is not geometry at all but one textured quad lifted out
    /// of the font atlas — this reads its corners as filled where the atlas has
    /// them empty. It is why the loupe's rim is sampled well inside the body
    /// disc, where the atlas is opaque and the flat reading is right, and why
    /// `the_cpu_frame_sampler_agrees_with_the_gpu` turns the option off on both
    /// sides. A stroke is unaffected: that path is skipped for a transparent
    /// fill. The other thing left out is egui's optional dithering, worth a
    /// level.
    fn frame_pixel(
        prims: &[egui::ClippedPrimitive],
        under: egui::Color32,
        at: egui::Pos2,
    ) -> egui::Color32 {
        let mut out = [under.r() as f32, under.g() as f32, under.b() as f32];
        for prim in prims {
            if !prim.clip_rect.contains(at) {
                continue;
            }
            let egui::epaint::Primitive::Mesh(mesh) = &prim.primitive else {
                continue;
            };
            for tri in mesh.indices.as_chunks::<3>().0 {
                let v = [
                    &mesh.vertices[tri[0] as usize],
                    &mesh.vertices[tri[1] as usize],
                    &mesh.vertices[tri[2] as usize],
                ];
                let Some(c) = triangle_at(v, at) else {
                    continue;
                };
                let keep = 1.0 - c.a() as f32 / 255.0;
                out = [
                    c.r() as f32 + out[0] * keep,
                    c.g() as f32 + out[1] * keep,
                    c.b() as f32 + out[2] * keep,
                ];
            }
        }
        egui::Color32::from_rgb(
            out[0].round().clamp(0.0, 255.0) as u8,
            out[1].round().clamp(0.0, 255.0) as u8,
            out[2].round().clamp(0.0, 255.0) as u8,
        )
    }

    /// Draw `body` into a fresh context at `ppp` and tessellate it, which is
    /// every frame these guards read.
    fn frame_at(
        palette: &Palette,
        size: egui::Vec2,
        ppp: f32,
        body: impl FnOnce(&mut egui::Ui),
    ) -> Vec<egui::ClippedPrimitive> {
        frame_into(&fresh_context(palette), size, ppp, body)
    }

    /// [`frame_at`] with pre-rasterised discs turned off, which is the one
    /// difference [`frame_pixel`] cannot see. Only the cross-check wants it.
    fn frame_at_geometric(
        palette: &Palette,
        size: egui::Vec2,
        ppp: f32,
        body: impl FnOnce(&mut egui::Ui),
    ) -> Vec<egui::ClippedPrimitive> {
        let ctx = fresh_context(palette);
        ctx.tessellation_options_mut(|o| o.prerasterized_discs = false);
        frame_into(&ctx, size, ppp, body)
    }

    fn fresh_context(palette: &Palette) -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx, palette);
        ctx
    }

    fn frame_into(
        ctx: &egui::Context,
        size: egui::Vec2,
        ppp: f32,
        body: impl FnOnce(&mut egui::Ui),
    ) -> Vec<egui::ClippedPrimitive> {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), size)),
            time: Some(0.0),
            ..Default::default()
        };
        input
            .viewports
            .entry(input.viewport_id)
            .or_default()
            .native_pixels_per_point = Some(ppp);
        let mut body = Some(body);
        let output = ctx.run_ui(input, |ui| {
            if let Some(body) = body.take() {
                body(ui);
            }
        });
        ctx.tessellate(output.shapes, output.pixels_per_point)
    }

    // -----------------------------------------------------------------------
    // The selection marquee
    // -----------------------------------------------------------------------

    /// Set an editor up with one selection over a document the size of the
    /// field, at zoom 1 and centred.
    fn editor_with_selection(doc: glam::UVec2, rings: Vec<Vec<glam::Vec2>>, ppp: f32) -> Editor {
        let mut ed = Editor::default();
        ed.pixels_per_point = ppp;
        // A zoom of `ppp` puts one document pixel on one point, so every scale
        // draws the same picture; the fractional centre is what stops every
        // edge landing on a whole device pixel for free, which is the case a
        // snap has to earn rather than inherit.
        ed.camera = umber_core::Camera {
            zoom: ppp,
            center: glam::Vec2::new(doc.x as f32 * 0.5 + 0.37, doc.y as f32 * 0.5 + 0.21),
        };
        ed.selection = Some(std::sync::Arc::new(
            umber_core::selection::Selection::from_rings(rings, doc).expect("a selection"),
        ));
        ed
    }

    /// One upright edge of a rectangle selection, and where it lands.
    const MARQUEE_DOC: glam::UVec2 = glam::UVec2::new(320, 220);
    const MARQUEE_RING: [glam::Vec2; 4] = [
        glam::Vec2::new(40.0, 40.0),
        glam::Vec2::new(160.0, 40.0),
        glam::Vec2::new(160.0, 140.0),
        glam::Vec2::new(40.0, 140.0),
    ];

    #[test]
    fn the_marquee_lands_a_whole_device_pixel_at_every_scale() {
        // **The reported defect, measured.** "Feathered and washed out" is a
        // statement about a device pixel: a marquee that pops has pixels
        // carrying its two inks *exactly*, and one that does not has the ink
        // spread over two at three quarters each. So this reads the frame back
        // and looks for the exact bytes, rather than asserting anything about
        // the width `ant_width` returns — which is the guard that would agree
        // with itself.
        //
        // The scales are the ones Windows offers. 125, 150 and 175 are the
        // three that are not whole numbers of device pixels per point, and 150
        // is the one most laptops ship set to; at a one-point stroke it reads
        // 64 for the dark half against white paper and 160 for the accent,
        // where every other scale here reads 0 and 192. That is the mutation
        // this was written against.
        let palette = Palette::of(ThemeKind::Graphite);
        let field = vec2(320.0, 220.0);
        let paper = egui::Color32::WHITE;
        for ppp in [1.0f32, 1.25, 1.5, 1.75, 2.0, 3.0] {
            let mut ed = editor_with_selection(MARQUEE_DOC, vec![MARQUEE_RING.to_vec()], ppp);
            let prims = frame_at(&palette, field, ppp, |ui| {
                let rect = ui.max_rect();
                ui.painter().rect_filled(rect, 0.0, paper);
                super::selection_outline(ui, &palette, &mut ed, rect);
            });

            // Down the left-hand edge, one device pixel at a time, taking the
            // darkest and the most saturated thing on each row's little scan
            // across the edge. Both inks have to turn up somewhere: the dashes
            // are the accent and the gaps between them are the underlay.
            let (mut solid_under, mut solid_accent) = (0, 0);
            for row in int_range(46.0, 134.0, ppp) {
                let y = (row as f32 + 0.5) / ppp;
                for col in int_range(30.0, 52.0, ppp) {
                    let at = pos2((col as f32 + 0.5) / ppp, y);
                    match frame_pixel(&prims, paper, at) {
                        c if c == palette.accent_underlay() => solid_under += 1,
                        c if c == palette.accent => solid_accent += 1,
                        _ => {}
                    }
                }
            }
            assert!(
                solid_under > 0 && solid_accent > 0,
                "at {ppp}x the outline never fully covers a device pixel: \
                 {solid_under} of the underlay, {solid_accent} of the accent"
            );
        }
    }

    /// Device pixel indices whose *centres* lie inside a span given in points.
    ///
    /// The guard above samples at `(index + 0.5) / ppp`, which is where a
    /// rasteriser takes its one sample, so the sweep has to be over indices and
    /// not over points — a loop stepping by a point would miss whole rows at
    /// 300% and take the same row twice at 50%.
    fn int_range(from: f32, to: f32, ppp: f32) -> std::ops::Range<u32> {
        (from * ppp).ceil() as u32..(to * ppp).floor() as u32
    }

    /// The darkest device pixel in the picture, and how many are within one
    /// level of it.
    fn darkest(image: &crate::docshot::Image, w: u32, h: u32) -> (u8, u32) {
        let mut best = 255u8;
        for y in 0..h {
            for x in 0..w {
                let p = image.pixel(x, y);
                best = best.min(p.r().max(p.g()).max(p.b()));
            }
        }
        let mut count = 0;
        for y in 0..h {
            for x in 0..w {
                let p = image.pixel(x, y);
                if p.r().max(p.g()).max(p.b()) <= best + 1 {
                    count += 1;
                }
            }
        }
        (best, count)
    }

    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn marquee_preview() {
        use crate::docshot;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/marquee");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        let field = vec2(320.0, 220.0);
        // A rectangle, and a lasso-ish blob beside it. The rectangle is the case
        // the artist meets; the blob is the staircase a traced mask produces,
        // where nothing can be snapped and the softness is the truth.
        let blob: Vec<glam::Vec2> = (0..24)
            .map(|i| {
                let a = i as f32 / 24.0 * std::f32::consts::TAU;
                glam::Vec2::new(240.0 + 52.0 * a.cos(), 110.0 + 46.0 * a.sin())
            })
            .collect();

        let mut written = 0;
        for (theme, ink) in [
            (ThemeKind::Graphite, "graphite"),
            (ThemeKind::Paper, "paper"),
        ] {
            let palette = Palette::of(theme);
            // White paper is the case reported, and black is its opposite: the
            // pair has to read on both, which is what `accent_underlay` is for.
            for (paper, paper_name) in [
                (egui::Color32::WHITE, "white"),
                (egui::Color32::from_gray(128), "grey"),
                (egui::Color32::BLACK, "black"),
            ] {
                // 1.5 is the case the reported softness lives in: a Windows
                // display at 150%, which is what most laptops ship set to.
                for ppp in [1.0f32, 1.5, 2.0] {
                    let mut ed = editor_with_selection(
                        MARQUEE_DOC,
                        vec![MARQUEE_RING.to_vec(), blob.clone()],
                        ppp,
                    );
                    ed.ui.theme = theme;
                    let image = stage.shoot(field, ppp, &palette, paper, |ui| {
                        let rect = ui.max_rect();
                        ui.painter().rect_filled(rect, 0.0, paper);
                        super::selection_outline(ui, &palette, &mut ed, rect);
                    });
                    let (w, h) = (
                        (field.x * ppp).round() as u32,
                        (field.y * ppp).round() as u32,
                    );
                    let (dark, n) = darkest(&image, w, h);
                    println!("{ink}/{paper_name}/@{ppp}: darkest {dark}, {n} pixels at it");
                    docshot::write_png(&dir.join(format!("{ink}-{paper_name}-{ppp}x.png")), &image)
                        .expect("write the png");
                    written += 1;
                }
            }
        }
        println!("wrote {written} shots to {}", dir.display());
    }
}
