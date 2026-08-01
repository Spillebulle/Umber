//! Drawing the dockable modules: sidebars, floating panels, splitters and the
//! drag-and-drop affordances that move panels between them.
//!
//! The model this drives lives in [`crate::dock`]; nothing here decides *where*
//! anything goes, it only paints what the model says and reports interactions
//! back to it. Keeping the two apart is what lets the fiddly parts — insertion
//! indices, minimum sizes, the config round trip — be tested without a window.
//!
//! Two things here are easy to get wrong and are commented at their call sites:
//! the hit-test ordering that lets a close button live inside a drag handle,
//! and the fact that a floating panel is an [`egui::Area`] rather than a
//! [`egui::Panel`] — an Area does not claim space, so the canvas region, and
//! therefore the camera pivot, is unaffected by a panel hovering over it.

use crate::brushlib;
use crate::colorpicker::{self, PickerMode};
use crate::dock::{DropTarget, Floating, Geometry, PanelKind, Side, limits};
use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::theme::{Palette, metrics, text};
use crate::ui::{UiActions, icon_button};
use crate::widgets;
use egui::{
    Align, Align2, CursorIcon, FontId, Frame, Id, LayerId, Layout, Order, Pos2, Rect, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use umber_core::{BlendMode, LayerStack};

/// Grab area of a splitter. Wider than the 1 px rule it draws, because a 1 px
/// target is not something anyone can hit.
const SPLITTER_GRAB: f32 = 7.0;

/// What a panel's chrome reported this frame. The caller applies these, because
/// the layout cannot be mutated while it is being iterated.
#[derive(Default)]
struct PanelEvents {
    /// Pointer position and the panel's rect at the moment a drag began.
    grab: Option<(Pos2, Rect)>,
    close: bool,
}

/// Draw both sidebars, their panels and their splitters.
pub fn sidebars(
    root: &mut Ui,
    p: &Palette,
    ed: &mut Editor,
    actions: &mut UiActions,
    geo: &Geometry,
) {
    for side in Side::ALL {
        let Some(rect) = geo.sidebar[side.index()] else {
            continue;
        };
        let frame = Frame {
            fill: p.dock,
            ..Default::default()
        };
        let panel = match side {
            Side::Left => egui::Panel::left("dock-left"),
            Side::Right => egui::Panel::right("dock-right"),
        };
        panel
            .exact_size(rect.width())
            .frame(frame)
            // `width_splitter` draws this edge itself, and lights it up in the
            // accent while it is being dragged. egui's separator lands on the
            // same pixel and is painted afterwards, so leaving it on put a dim
            // rule over the highlight and the resize affordance never showed.
            .show_separator_line(false)
            .show(root, |ui| sidebar(ui, p, ed, actions, side, geo));
    }

    // The library's browser and its dialogs. Drawn here rather than from inside
    // the Brushes panel, which the layout is free to hide — a modal that goes
    // with its panel cannot be shut and cannot be reopened.
    brushlib::dialogs(root, p, ed);
    // The module library, for the same reason and one more: it is how a module
    // that has been removed from the layout comes back, so tying it to a panel
    // would tie the way back to the thing that has gone.
    module_library(root, p, ed);
}

fn sidebar(
    ui: &mut Ui,
    p: &Palette,
    ed: &mut Editor,
    actions: &mut UiActions,
    side: Side,
    geo: &Geometry,
) {
    let slots = &geo.slots[side.index()];
    // Snapshot the stack: drawing a panel can start a drag, which removes it
    // from the layout, and the loop must not be reading the Vec when it does.
    let kinds: Vec<PanelKind> = ed.layout.docked(side).iter().map(|d| d.kind).collect();

    let mut grabbed = None;
    let mut closed = None;
    for (index, kind) in kinds.iter().copied().enumerate() {
        let Some(slot) = slots.get(index).copied() else {
            continue;
        };
        let events = panel(ui, p, ed, actions, kind, slot);
        if let Some(grab) = events.grab {
            grabbed = Some((kind, grab));
        }
        if events.close {
            closed = Some(kind);
        }
        if index + 1 < kinds.len() {
            ui.painter()
                .hline(slot.x_range(), slot.bottom(), Stroke::new(1.0, p.border));
        }
    }

    height_splitters(ui, p, ed, side, slots);
    width_splitter(ui, p, ed, side, geo);

    if let Some(kind) = closed {
        ed.layout.close(kind);
    }
    if let Some((kind, (pointer, rect))) = grabbed {
        ed.layout.begin_drag(kind, pointer, rect);
    }
}

/// The draggable boundaries between stacked panels.
fn height_splitters(ui: &mut Ui, p: &Palette, ed: &mut Editor, side: Side, slots: &[Rect]) {
    let heights: Vec<f32> = slots.iter().map(|s| s.height()).collect();
    // The last slot has nothing below it to push against, so it has no handle.
    let boundaries = slots.len().saturating_sub(1);
    for (index, slot) in slots.iter().enumerate().take(boundaries) {
        let y = slot.bottom();
        let handle = Rect::from_min_max(
            pos2(slot.left(), y - SPLITTER_GRAB * 0.5),
            pos2(slot.right(), y + SPLITTER_GRAB * 0.5),
        );
        let response = ui
            .interact(
                handle,
                ui.id().with(("vsplit", side.index(), index)),
                Sense::drag(),
            )
            .on_hover_cursor(CursorIcon::ResizeVertical);
        if response.dragged() {
            ed.layout
                .resize_split(side, index, response.drag_delta().y, &heights);
        }
        if response.hovered() || response.dragged() {
            ui.painter()
                .hline(handle.x_range(), y, Stroke::new(2.0, p.accent));
        }
    }
}

/// The draggable inner edge that sets the sidebar's width.
///
/// The handle sits *inside* the sidebar rather than straddling its edge, so
/// that grabbing it counts as pointing at the panel and never at the canvas —
/// otherwise the first pixel of a resize drag would also start a stroke.
fn width_splitter(ui: &mut Ui, p: &Palette, ed: &mut Editor, side: Side, geo: &Geometry) {
    let Some(rect) = geo.sidebar[side.index()] else {
        return;
    };
    let handle = match side {
        Side::Left => Rect::from_min_max(
            pos2(rect.right() - SPLITTER_GRAB, rect.top()),
            rect.right_bottom(),
        ),
        Side::Right => Rect::from_min_max(
            rect.left_top(),
            pos2(rect.left() + SPLITTER_GRAB, rect.bottom()),
        ),
    };
    let response = ui
        .interact(
            handle,
            ui.id().with(("hsplit", side.index())),
            Sense::drag(),
        )
        .on_hover_cursor(CursorIcon::ResizeHorizontal);

    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let width = match side {
            Side::Left => pointer.x - rect.left(),
            Side::Right => rect.right() - pointer.x,
        };
        ed.layout.set_width(side, width);
    }

    let x = match side {
        Side::Left => rect.right() - 0.5,
        Side::Right => rect.left() + 0.5,
    };
    let colour = if response.hovered() || response.dragged() {
        p.accent
    } else {
        p.border
    };
    let weight = if response.hovered() || response.dragged() {
        2.0
    } else {
        1.0
    };
    ui.painter()
        .vline(x, rect.y_range(), Stroke::new(weight, colour));
}

/// Draw every floating panel, back to front.
pub fn floats(root: &mut Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    let list: Vec<Floating> = ed.layout.floating().to_vec();
    let ctx = root.ctx().clone();

    let mut grabbed = None;
    let mut closed = None;
    let mut raised = None;
    let mut resized = None;

    for f in list {
        // An Area, not a Panel: an Area floats over everything and claims no
        // space, which is exactly what "hovers over the canvas" has to mean.
        // A Panel would shrink the central region and move the camera pivot
        // with it, and strokes would land away from the cursor.
        let area = egui::Area::new(Id::new(("float-panel", f.kind)))
            .order(Order::Middle)
            .fixed_pos(f.rect.min)
            .movable(false)
            .constrain(false);

        let inner = area.show(&ctx, |ui| {
            // Claim the whole rect so the Area's own bounds match the panel.
            // Without this the Area is only as big as whatever was allocated,
            // and egui's layer hit testing lets clicks through the panel.
            let backdrop = ui.allocate_rect(f.rect, Sense::hover());

            let painter = ui.painter();
            painter.rect_filled(f.rect, metrics::RADIUS_LARGE, p.popover);
            painter.rect_stroke(
                f.rect,
                metrics::RADIUS_LARGE,
                Stroke::new(1.0, p.popover_border),
                StrokeKind::Inside,
            );

            let events = panel(ui, p, ed, actions, f.kind, f.rect);

            // Corner grip. Registered after the body, so it wins the hit test
            // against whatever the panel happens to put underneath it.
            let grip = Rect::from_min_max(
                f.rect.right_bottom() - vec2(16.0, 16.0),
                f.rect.right_bottom(),
            );
            let handle = ui
                .interact(grip, ui.id().with(("resize", f.kind)), Sense::drag())
                .on_hover_cursor(CursorIcon::ResizeNwSe);
            if handle.dragged() {
                let size = (f.rect.size() + handle.drag_delta()).clamp(
                    vec2(limits::FLOAT_MIN_WIDTH, limits::FLOAT_MIN_HEIGHT),
                    vec2(limits::FLOAT_MAX_WIDTH, limits::FLOAT_MAX_HEIGHT),
                );
                resized = Some((f.kind, Rect::from_min_size(f.rect.min, size)));
            }
            icons::draw(
                ui.painter(),
                grip.shrink(3.0),
                Icon::Corner,
                if handle.hovered() || handle.dragged() {
                    p.text_strong
                } else {
                    p.text_dim
                },
            );

            (events, backdrop)
        });

        let (events, backdrop) = inner.inner;
        if let Some(grab) = events.grab {
            grabbed = Some((f.kind, grab));
        }
        if events.close {
            closed = Some(f.kind);
        }
        // Any press inside the panel raises it. Checking the whole rect rather
        // than a click on the backdrop means clicking a slider raises the panel
        // too, which is what a user expects of overlapping windows.
        if backdrop.contains_pointer() && ctx.input(|i| i.pointer.any_pressed()) {
            raised = Some((f.kind, inner.response.layer_id));
        }
    }

    if let Some((kind, rect)) = resized {
        ed.layout.set_float_rect(kind, rect);
    }
    if let Some((kind, layer)) = raised {
        ed.layout.raise(kind);
        ctx.move_to_top(layer);
    }
    if let Some(kind) = closed {
        ed.layout.close(kind);
    }
    if let Some((kind, (pointer, rect))) = grabbed {
        ed.layout.begin_drag(kind, pointer, rect);
    }
}

/// One panel: header strip, then its body, drawn into `rect`.
fn panel(
    ui: &mut Ui,
    p: &Palette,
    ed: &mut Editor,
    actions: &mut UiActions,
    kind: PanelKind,
    rect: Rect,
) -> PanelEvents {
    let mut events = PanelEvents::default();
    let pad = f32::from(metrics::PANEL_PAD);

    // `Layout::geometry` gives a sidebar no width at all rather than let the two
    // of them eat a narrow window's canvas, and the header's own controls are
    // laid out from `rect.center().x` to `rect.right() - pad` — which on a rect
    // this narrow is a rectangle with its right edge left of its left one.
    // Nothing useful can be drawn in it either way.
    if rect.width() < pad * 2.0 + 8.0 {
        return events;
    }

    let header = Rect::from_min_size(rect.min, vec2(rect.width(), metrics::PANEL_HEADER));

    // The whole header is the drag handle — but only in edit mode, as the
    // design has it. Outside it the header is inert, so reaching for a slider
    // can never tear its panel off mid-stroke.
    //
    // It is registered *first* on purpose: egui breaks hit-test ties in favour
    // of the last widget added, so the close button and the picker-mode switch
    // placed below still take their own clicks even though they sit inside
    // this rect.
    let editing = ed.layout.edit_mode();
    let grip = ui.interact(
        header,
        ui.id().with(("panel-header", kind)),
        if editing {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let grip = if editing {
        grip.on_hover_cursor(if ed.layout.is_dragging() {
            CursorIcon::Grabbing
        } else {
            CursorIcon::Grab
        })
    } else {
        grip
    };
    if grip.drag_started()
        && let Some(pointer) = grip.interact_pointer_pos()
    {
        events.grab = Some((pointer, rect));
    }

    let painter = ui.painter();
    if editing && grip.hovered() && !ed.layout.is_dragging() {
        painter.rect_filled(header, 0.0, p.control.gamma_multiply(0.5));
    }
    let dots = Rect::from_center_size(
        pos2(rect.left() + pad + 4.0, header.center().y),
        vec2(10.0, 14.0),
    );
    // The grip lights up in edit mode and recedes outside it, which is the
    // design's cue that the panel has become movable.
    icons::draw(
        painter,
        dots,
        Icon::Grip,
        if editing {
            p.accent
        } else {
            p.text_dim.gamma_multiply(0.5)
        },
    );
    painter.text(
        pos2(dots.right() + 6.0, header.center().y),
        Align2::LEFT_CENTER,
        kind.title(),
        FontId::proportional(text::SMALL),
        p.text_strong,
    );

    // Header controls, right-aligned. Added after the drag handle, so they win.
    let controls = Rect::from_min_max(
        pos2(rect.center().x, header.top()),
        pos2(rect.right() - pad, header.bottom()),
    );
    ui.scope_builder(
        UiBuilder::new()
            .id_salt(("panel-controls", kind))
            .max_rect(controls)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            // Removing a module is an edit-mode action, so a stray click on the
            // corner of a panel cannot make it vanish mid-painting.
            if editing && remove_button(ui, p) {
                events.close = true;
            }
            if kind == PanelKind::Colour {
                picker_mode_switch(ui, p, ed);
            }
            if kind == PanelKind::Brushes {
                brushlib::header_controls(ui, p, ed);
            }
        },
    );

    // Body. Clipped and scrollable, because a panel dragged down to its minimum
    // height still has to show something rather than spilling over its
    // neighbour.
    let body = Rect::from_min_max(
        pos2(rect.left() + pad, header.bottom()),
        pos2(rect.right() - pad, rect.bottom() - 6.0),
    );
    if body.height() < 8.0 || body.width() < 8.0 {
        return events;
    }
    ui.scope_builder(
        UiBuilder::new()
            .id_salt(("panel-body", kind))
            .max_rect(body),
        |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(body));
            egui::ScrollArea::vertical()
                .id_salt(("panel-scroll", kind))
                .auto_shrink([false, false])
                .show(ui, |ui| match kind {
                    PanelKind::Colour => colour_body(ui, p, ed),
                    PanelKind::Brushes => brushlib::panel(ui, p, ed),
                    PanelKind::Layers => layers_body(ui, p, ed, actions),
                    PanelKind::History => history_body(ui, p, ed, actions),
                });
        },
    );

    events
}

/// The edit-mode control that takes a module out of the layout.
///
/// Its own function rather than [`icon_button`] because it is the one control
/// in a header that destroys something, and it says so by lighting up in the
/// warning colour instead of in the ordinary hover ink. It is nothing worse
/// than reversible — the module library puts any module back — which is why the
/// tooltip names the way back rather than asking for a confirmation.
fn remove_button(ui: &mut Ui, p: &Palette) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, metrics::RADIUS, p.warning_bg);
    }
    icons::draw(
        ui.painter(),
        rect.shrink(3.0),
        Icon::Close,
        if hovered { p.warning } else { p.text_dim },
    );
    // No arrow glyph in the tooltip: Archivo carries none, and a blank box
    // pointing at the way back would be worse than spelling out the menu.
    response
        .on_hover_text("Remove this module from the layout — Window, Modules puts it back")
        .clicked()
}

/// A dashed outline round a rounded rect.
///
/// egui strokes rects solid; the design's dock affordances are dashed, and a
/// dashed border is what distinguishes "this is where it will go" from a real
/// piece of chrome. Corners are approximated by insetting the four runs, which
/// at these radii is not visible.
fn dashed_rect(painter: &egui::Painter, rect: Rect, radius: f32, stroke: Stroke) {
    let r = radius.min(rect.width() * 0.5).min(rect.height() * 0.5);
    let runs = [
        [
            pos2(rect.left() + r, rect.top()),
            pos2(rect.right() - r, rect.top()),
        ],
        [
            pos2(rect.right(), rect.top() + r),
            pos2(rect.right(), rect.bottom() - r),
        ],
        [
            pos2(rect.right() - r, rect.bottom()),
            pos2(rect.left() + r, rect.bottom()),
        ],
        [
            pos2(rect.left(), rect.bottom() - r),
            pos2(rect.left(), rect.top() + r),
        ],
    ];
    for run in runs {
        painter.extend(egui::Shape::dashed_line(&run, stroke, 5.0, 4.0));
    }
}

/// The dashed outline the design puts round a sidebar while the layout is being
/// edited, so it reads as a container you can drop into.
pub fn edit_mode_outline(root: &mut Ui, p: &Palette, ed: &Editor, geo: &Geometry) {
    if !ed.layout.edit_mode() {
        return;
    }
    let painter = root.ctx().layer_painter(LayerId::new(
        Order::Foreground,
        Id::new("dock-edit-outline"),
    ));
    for side in Side::ALL {
        if let Some(rect) = geo.sidebar[side.index()] {
            dashed_rect(
                &painter,
                rect.shrink(4.0),
                8.0,
                Stroke::new(1.5, p.accent_dim),
            );
        }
    }
}

/// The overlay a drag draws, and the point at which a drop is resolved.
///
/// Called last, after every panel has had its chance to interact, so the
/// geometry the drop is tested against is this frame's.
pub fn drag_overlay(root: &mut Ui, p: &Palette, ed: &mut Editor, geo: &Geometry) {
    let ctx = root.ctx().clone();
    if !ed.layout.is_dragging() {
        return;
    }

    if let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) {
        ed.layout.drag_to(pointer);
    }
    let Some(drag) = ed.layout.drag() else { return };
    let target = geo.drop_target(drag.pointer);

    let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("dock-drag")));
    match target {
        DropTarget::Dock { side, index } => {
            // The design's dock indicator: a dashed accent block reading "dock
            // here". It is drawn *at the insertion point* rather than always at
            // the top of the sidebar, because unlike the design's model this
            // one can insert between two panels, and an indicator that lied
            // about where the panel lands would be worse than none.
            let zone = geo.drop_zone(side);
            let (a, b) = geo.insertion_line(side, index);
            let block = Rect::from_min_max(
                pos2(zone.left() + 8.0, a.y.min(zone.bottom() - 40.0)),
                pos2(b.x - 8.0, (a.y + 110.0).min(zone.bottom() - 8.0)),
            );
            painter.rect_filled(block, 8.0, p.accent.gamma_multiply(0.09));
            dashed_rect(&painter, block, 8.0, Stroke::new(2.0, p.accent));
            painter.line_segment([a, b], Stroke::new(2.0, p.accent));
            if block.height() > 34.0 {
                painter.text(
                    block.center(),
                    Align2::CENTER_CENTER,
                    "dock here",
                    FontId::proportional(text::TINY),
                    p.accent,
                );
            }
        }
        DropTarget::Float => {
            let rect = drag.float_rect();
            painter.rect_filled(rect, metrics::RADIUS_LARGE, p.popover.gamma_multiply(0.85));
            painter.rect_stroke(
                rect,
                metrics::RADIUS_LARGE,
                Stroke::new(1.0, p.accent),
                StrokeKind::Inside,
            );
        }
    }

    // The panel itself is not drawn while it is in the air — only its header,
    // as a label under the cursor. Re-running a whole panel body into a moving
    // rect would re-enter every widget's id in a new place each frame, which
    // egui reasonably objects to.
    let tab = Rect::from_min_size(
        drag.pointer - drag.grab,
        vec2(drag.float_size.x, metrics::PANEL_HEADER),
    );
    painter.rect_filled(tab, metrics::RADIUS_LARGE, p.popover);
    painter.rect_stroke(
        tab,
        metrics::RADIUS_LARGE,
        Stroke::new(1.0, p.accent),
        StrokeKind::Inside,
    );
    painter.text(
        pos2(tab.left() + f32::from(metrics::PANEL_PAD), tab.center().y),
        Align2::LEFT_CENTER,
        drag.kind.title(),
        FontId::proportional(text::SMALL),
        p.text_strong,
    );
    // A module picked up from the library is not being held down, so it has to
    // say what will put it down. Without this the only cue is that it follows
    // the pointer, which reads as a stuck drag rather than as a placement.
    if drag.sticky {
        painter.text(
            pos2(tab.right() - f32::from(metrics::PANEL_PAD), tab.center().y),
            Align2::RIGHT_CENTER,
            "click to place",
            FontId::proportional(text::TINY),
            p.accent,
        );
    }

    ctx.set_cursor_icon(CursorIcon::Grabbing);

    // Which pointer event ends the drag is the model's to decide: one begun by
    // a press ends on the release, one begun by a click on the next press. See
    // `Layout::drag_should_drop`.
    let (down, pressed) = ctx.input(|i| (i.pointer.any_down(), i.pointer.any_pressed()));
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        ed.layout.cancel_drag();
    } else if ed.layout.drag_should_drop(down, pressed) {
        ed.layout.end_drag(target);
    }
    ctx.request_repaint();
}

// --- panel bodies ---------------------------------------------------------

fn colour_body(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut shape = ed.ui.wheel_shape;
    let mut rotates = ed.ui.wheel_rotates;
    let changed = colorpicker::show(ui, p, ed.ui.picker, &mut shape, &mut rotates, &mut ed.hsv);
    // Both are kept between runs, though their controls are here rather than in
    // the settings dialog — they are choices about the workspace, and where one
    // is set does not decide whether it should still be true tomorrow.
    //
    // Compared before and after rather than asked of the controls, because
    // `show` reports a change of *colour*: keying off its return would queue a
    // preferences write for every frame of a drag around the hue ring.
    if shape != ed.ui.wheel_shape || rotates != ed.ui.wheel_rotates {
        ed.ui.wheel_shape = shape;
        ed.ui.wheel_rotates = rotates;
        crate::prefs::mark_dirty();
    }
    if changed {
        ed.commit_picker();
    }

    ui.add_space(9.0);
    ui.horizontal(|ui| {
        let [r, g, b, _] = ed.color.to_srgb_u8();
        let (chip, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
        ui.painter()
            .rect_filled(chip, metrics::RADIUS, egui::Color32::from_rgb(r, g, b));
        ui.label(
            egui::RichText::new(format!("#{r:02X}{g:02X}{b:02X}"))
                .monospace()
                .size(text::TINY)
                .color(p.text),
        );
    });
}

fn layers_body(ui: &mut Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    let count = ed.layers.len();
    let active = ed.layers.active_index();

    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(
                ui,
                p,
                Icon::Trash,
                count > 1,
                "Delete layer — clears undo history",
            ) {
                actions.delete_layer = Some(active);
            }
            if icon_button(ui, p, Icon::ChevronDown, active > 0, "Move layer down") {
                actions.move_layer_down = Some(active);
            }
            if icon_button(ui, p, Icon::ChevronUp, active + 1 < count, "Move layer up") {
                actions.move_layer_up = Some(active);
            }
            if icon_button(
                ui,
                p,
                Icon::Plus,
                count < LayerStack::MAX,
                "Add a layer above the current one",
            ) {
                actions.add_layer = true;
            }
        });
    });

    ui.add_space(4.0);

    // Blend and opacity for the selected layer, on one row.
    //
    // Both change the picture, so both have to mark the document modified —
    // otherwise the close prompt, which asks only about modified documents,
    // would let a tab holding a carefully set stack of opacities close without
    // a word. Collected and applied below the borrow rather than inside it,
    // since `mark_modified` also wants `ed`.
    let mut changed = false;
    ui.horizontal(|ui| {
        let layer = ed.layers.active_mut();
        let before = (layer.blend, layer.opacity);
        egui::ComboBox::from_id_salt("layer-blend")
            .selected_text(
                egui::RichText::new(layer.blend.label())
                    .size(text::TINY)
                    .color(p.text),
            )
            .width(80.0)
            .show_ui(ui, |ui| {
                for mode in BlendMode::ALL {
                    ui.selectable_value(&mut layer.blend, mode, mode.label());
                }
            });
        let value = layer.opacity;
        widgets::bare_slider(ui, p, &mut layer.opacity, 0.0..=1.0);
        changed = before != (layer.blend, layer.opacity);
        ui.label(
            egui::RichText::new(format!("{:.0}", value * 100.0))
                .monospace()
                .size(10.0)
                .color(p.text),
        );
    });

    ui.add_space(7.0);

    // Stored bottom-first; shown top-first, the way it is drawn.
    let mut select = None;
    let mut toggle = None;
    for index in (0..count).rev() {
        let Some(layer) = ed.layers.get(index) else {
            continue;
        };
        let row = widgets::layer_row(
            ui,
            p,
            &layer.name,
            layer.slot(),
            layer.visible,
            index == active,
            layer.blend.label(),
        );
        if row.eye_clicked {
            toggle = Some(index);
        } else if row.clicked {
            select = Some(index);
        }
    }
    if let Some(index) = toggle
        && let Some(layer) = ed.layers.get_mut(index)
    {
        layer.visible = !layer.visible;
        changed = true;
    }
    if let Some(index) = select {
        ed.layers.set_active(index);
    }
    if changed {
        ed.mark_modified();
    }
}

/// The History module: what has been painted on this document, and a click to
/// go back to any point in it.
///
/// What it deliberately does *not* show is anything it cannot restore. Umber's
/// history covers painting only — adding, deleting or reordering a layer is not
/// recorded, and deleting one clears the list outright — so a row appears only
/// where a patch was captured, and the note at the foot says so rather than
/// leaving the gap to be discovered. A list that named a structural action it
/// could not undo would be worse than one that admits its own edges.
fn history_body(ui: &mut Ui, p: &Palette, ed: &Editor, actions: &mut UiActions) {
    let position = ed.history.position();
    let count = ed.history.len();

    // Keep the current entry in view when the position moves under the list —
    // a jump of eight entries otherwise scrolls nothing and appears to have
    // done nothing. Held in egui's temporary store rather than in `UiState`,
    // which is per-application where this is per-list.
    let memo = Id::new("history-follow");
    let follow = ui.ctx().data(|d| d.get_temp::<usize>(memo)) != Some(position);

    let mut jump = None;

    // Row zero is the document with none of the edits applied, and it is the
    // only way back to a blank start, so it is a row like any other rather than
    // a caption. Once the budget has aged entries out it is no longer the
    // beginning of the document, and it says so — see `History::dropped`.
    let dropped = ed.history.dropped();
    let base = if dropped > 0 {
        "Earlier edits discarded"
    } else {
        "Opened"
    };
    let at_start = position == 0;
    if history_row(ui, p, base, true, at_start, at_start && follow).clicked() {
        jump = Some(0);
    }

    for index in 0..count {
        let Some(kind) = ed.history.kind_at(index) else {
            continue;
        };
        // Applied entries read as ink, undone ones as the ghost they are: a
        // click on one of those is a redo.
        let applied = index < position;
        let current = position == index + 1;
        if history_row(ui, p, kind.label(), applied, current, current && follow).clicked() {
            jump = Some(index + 1);
        }
    }

    if let Some(target) = jump {
        actions.history_jump = Some(target);
    }
    ui.ctx().data_mut(|d| d.insert_temp(memo, position));

    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(if count == 0 {
            "Nothing painted on this document yet. Strokes are recorded here; \
             layers are not."
        } else {
            "Strokes only. Adding, deleting or reordering a layer is not \
             recorded, and deleting one clears this list."
        })
        .size(9.5)
        .color(p.text_dim)
        .line_height(Some(12.0)),
    );
}

/// One entry in that list: a marker, then what the edit was.
///
/// Nothing here allocates and nothing off screen is painted. The list is as
/// long as the session is, and both of those show up in a frame time before
/// anything else about it does.
fn history_row(
    ui: &mut Ui,
    p: &Palette,
    label: &'static str,
    applied: bool,
    current: bool,
    scroll_here: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), metrics::HISTORY_ROW),
        Sense::click(),
    );
    if scroll_here {
        response.scroll_to_me(Some(Align::Center));
    }
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter();
    if current {
        painter.rect_filled(rect, metrics::RADIUS, p.control_active);
    } else if response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control);
    }

    // The marker is the cursor: filled and accented where the document stands,
    // hollow behind it, and hollow and dim ahead of it.
    let ink = match (current, applied) {
        (true, _) => p.accent,
        (false, true) => p.text,
        (false, false) => p.text_dim.gamma_multiply(0.55),
    };
    let dot = pos2(rect.left() + 8.0, rect.center().y);
    if current {
        painter.circle_filled(dot, 3.5, ink);
    } else {
        painter.circle_stroke(dot, 3.0, Stroke::new(1.0, ink));
    }
    painter.text(
        pos2(dot.x + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(text::SMALL),
        ink,
    );

    response.on_hover_text(if applied {
        "Go back to this point"
    } else {
        "Put this back"
    })
}

/// The Colour panel's picker-type switch: a half-filled disc, the mode name,
/// and a chevron.
fn picker_mode_switch(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let label = ed.ui.picker.label();
    let font = FontId::proportional(9.5);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), p.text_dim)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(vec2(text_w + 26.0, 16.0), Sense::click());
    let colour = if response.hovered() {
        p.text_strong
    } else {
        p.text_dim
    };
    let painter = ui.painter();
    icons::draw(
        painter,
        Rect::from_min_size(rect.left_top(), vec2(12.0, 16.0)),
        Icon::HalfCircle,
        colour,
    );
    painter.text(
        rect.left_center() + vec2(15.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        font,
        colour,
    );
    icons::draw(
        painter,
        Rect::from_min_size(rect.right_top() - vec2(11.0, 0.0), vec2(11.0, 16.0)),
        Icon::ChevronDown,
        colour,
    );

    if response.clicked() {
        ed.ui.picker_menu_open = !ed.ui.picker_menu_open;
    }
    let popup = egui::Popup::from_response(&response)
        .open(ed.ui.picker_menu_open)
        .show(|ui| {
            for mode in PickerMode::ALL {
                if ui
                    .selectable_label(ed.ui.picker == mode, mode.label())
                    .clicked()
                {
                    if ed.ui.picker != mode {
                        crate::prefs::mark_dirty();
                    }
                    ed.ui.picker = mode;
                    ed.ui.picker_menu_open = false;
                }
            }
        });
    if popup.is_none() {
        ed.ui.picker_menu_open = false;
    }
}

// --- the module library ----------------------------------------------------

/// Every module there is: a picture of it, what it is for, and a way to put it
/// into the layout.
///
/// Drawn from [`sidebars`], not from a panel body — the layout can hide any
/// panel, and this is precisely the dialog that brings a hidden one back, so
/// tying it to one would tie the way back to the thing that has gone.
///
/// Adding does not place the module: it puts it in the pointer's hand, in
/// layout edit mode, and lets the drop decide where it goes. That is the same
/// gesture that moves a module already in the layout, so there is one way to
/// say where a panel lives rather than two, and Escape abandons the add exactly
/// as it abandons a move.
fn module_library(root: &mut Ui, p: &Palette, ed: &mut Editor) {
    if !ed.ui.module_library_open {
        return;
    }

    let mut picked = None;
    let response = egui::Modal::new(Id::new("module-library"))
        .frame(
            Frame::NONE
                .fill(p.popover)
                .stroke(Stroke::new(1.0, p.popover_border))
                .corner_radius(8)
                .inner_margin(egui::Margin::same(18)),
        )
        .show(root.ctx(), |ui| {
            ui.set_width(metrics::MODULE_LIBRARY_WIDTH);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Modules")
                        .size(text::CONTROL)
                        .color(p.text_strong)
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, p, Icon::Close, true, "Close") {
                        ed.ui.module_library_open = false;
                    }
                });
            });
            crate::controls::note(
                ui,
                p,
                "The panels the workspace is made of. Adding one hands it to \
                 the pointer — drop it in a sidebar to dock it, or anywhere \
                 else to leave it floating over the canvas.",
            );
            ui.add_space(10.0);

            for kind in PanelKind::ALL {
                if module_card(ui, p, ed, kind) {
                    picked = Some(kind);
                }
                ui.add_space(6.0);
            }
        });

    if response.should_close() {
        ed.ui.module_library_open = false;
    }

    if let Some(kind) = picked {
        // Under the cursor, which is where the user is looking. With no pointer
        // — a keyboard-driven click — the middle of the window is the only
        // honest answer, and the module is visibly in hand there too.
        let pointer = root
            .ctx()
            .pointer_hover_pos()
            .unwrap_or_else(|| root.ctx().viewport_rect().center());
        if ed.layout.add_dragging(kind, pointer) {
            // Out of the way, or the module would be dragged across the dialog
            // that produced it.
            ed.ui.module_library_open = false;
        }
    }
}

/// One module's card. Returns true when the user asked for it.
fn module_card(ui: &mut Ui, p: &Palette, ed: &Editor, kind: PanelKind) -> bool {
    let open = ed.layout.is_open(kind);
    let mut add = false;
    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS_LARGE)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let [w, h] = metrics::MODULE_PREVIEW;
                let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
                module_preview(ui.painter(), p, rect, kind);

                ui.add_space(12.0);
                ui.vertical(|ui| {
                    // Room kept for the button, so a long description does not
                    // push it off the card.
                    ui.set_width((ui.available_width() - 76.0).max(80.0));
                    ui.label(
                        egui::RichText::new(kind.title())
                            .size(text::SMALL)
                            .color(p.text_strong)
                            .strong(),
                    );
                    crate::controls::note(ui, p, kind.description());
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // An open module is not offered again — there is exactly
                    // one of each, which is what makes the kind its identity.
                    // The control says why it is dead rather than vanishing,
                    // so the card does not change shape as panels come and go.
                    if crate::controls::text_button(ui, p, "Add", !open, !open)
                        .on_hover_text(if open {
                            "Already in your layout. Drag it by its header in \
                             layout edit mode to move it."
                        } else {
                            "Pick it up, then click where you want it"
                        })
                        .clicked()
                    {
                        add = true;
                    }
                });
            });
        });
    add
}

/// A schematic of one module, painted rather than screenshotted.
///
/// This project paints its widgets rather than shipping pictures of them, and a
/// bitmap of a panel would be a second thing to keep in step with the panel —
/// stale the first time the Colour module gains a control, and wrong in the
/// other theme immediately. A schematic in the palette's own tokens is never
/// either: it says which module this is by its *shape*, which is what the eye
/// is matching against the sidebar anyway.
fn module_preview(painter: &egui::Painter, p: &Palette, rect: Rect, kind: PanelKind) {
    painter.rect_filled(rect, metrics::RADIUS, p.chrome);
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );

    // Every module wears the same header, because in the dock every module
    // does: it is the strip they are all dragged by.
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), 9.0));
    painter.rect_filled(header, 0.0, p.control);
    painter.hline(rect.x_range(), header.bottom(), Stroke::new(1.0, p.border));
    for k in 0..2 {
        painter.circle_filled(
            pos2(rect.left() + 5.0 + k as f32 * 3.0, header.center().y),
            0.8,
            p.accent,
        );
    }
    painter.rect_filled(
        Rect::from_min_size(
            pos2(rect.left() + 13.0, header.center().y - 1.0),
            vec2(18.0, 2.0),
        ),
        1.0,
        p.text_dim,
    );

    let body = Rect::from_min_max(pos2(rect.left() + 6.0, header.bottom() + 5.0), rect.max)
        .shrink2(vec2(0.0, 5.0));
    if body.width() < 8.0 || body.height() < 8.0 {
        return;
    }
    let ink = p.text_dim;
    let bar = |y: f32, from: f32, to: f32, colour: egui::Color32| {
        painter.rect_filled(
            Rect::from_min_max(
                pos2(body.left() + from, y - 1.0),
                pos2(body.left() + to, y + 1.0),
            ),
            1.0,
            colour,
        );
    };

    match kind {
        // The hue ring with its square centre, and the swatch under it.
        PanelKind::Colour => {
            let r = (body.height() * 0.42).min(body.width() * 0.28);
            let centre = pos2(body.left() + r + 2.0, body.top() + r + 1.0);
            painter.circle_stroke(centre, r, Stroke::new(2.5, p.accent));
            painter.rect_filled(
                Rect::from_center_size(centre, vec2(r, r)),
                1.0,
                p.text_dim.gamma_multiply(0.7),
            );
            for k in 0..3 {
                bar(
                    body.bottom() - 2.0,
                    0.0 + k as f32 * 14.0,
                    10.0 + k as f32 * 14.0,
                    ink,
                );
            }
        }
        // A short list of brushes, each a stroke sample beside a name.
        PanelKind::Brushes => {
            for k in 0..3 {
                let y = body.top() + 5.0 + k as f32 * 10.0;
                painter.rect_filled(
                    Rect::from_min_size(pos2(body.left(), y - 2.0), vec2(16.0, 4.0)),
                    2.0,
                    if k == 0 { p.accent } else { ink },
                );
                bar(y, 20.0, 20.0 + 26.0 - k as f32 * 5.0, ink);
            }
        }
        // A stack, with a thumbnail on every row and one of them selected.
        PanelKind::Layers => {
            for k in 0..3 {
                let y = body.top() + 5.0 + k as f32 * 10.0;
                let cell = Rect::from_min_size(pos2(body.left(), y - 3.5), vec2(7.0, 7.0));
                painter.rect_stroke(
                    cell,
                    1.0,
                    Stroke::new(1.0, if k == 1 { p.accent } else { ink }),
                    StrokeKind::Inside,
                );
                bar(y, 11.0, 11.0 + 30.0 - k as f32 * 6.0, ink);
            }
        }
        // A timeline: a marker per entry, filled where the document stands.
        PanelKind::History => {
            for k in 0..4 {
                let y = body.top() + 4.0 + k as f32 * 8.0;
                let dot = pos2(body.left() + 3.0, y);
                if k == 2 {
                    painter.circle_filled(dot, 2.5, p.accent);
                } else {
                    painter.circle_stroke(dot, 2.0, Stroke::new(1.0, ink));
                }
                bar(y, 9.0, if k == 2 { 40.0 } else { 30.0 }, ink);
            }
        }
    }
}

/// The tool rail's own drag handle, shown only in layout edit mode.
///
/// Dragging it past the middle of the window moves the rail to that edge. This
/// is all that survives of the left-handed mirror, and deliberately as a drag
/// rather than a setting: a "which side" flag is that mirror wearing a
/// different label.
pub fn rail_grip(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    if !ed.layout.edit_mode() {
        return;
    }
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::drag());
    let response = response
        .on_hover_cursor(CursorIcon::Grab)
        .on_hover_text("Drag the rail to the other side of the window");

    icons::draw(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(14.0, 14.0)),
        Icon::Grip,
        if response.dragged() {
            p.text_strong
        } else {
            p.accent
        },
    );

    if response.drag_stopped()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let middle = ui.ctx().viewport_rect().center().x;
        let side = if pointer.x < middle {
            Side::Left
        } else {
            Side::Right
        };
        ed.layout.set_rail_side(side);
    }
    ui.add_space(4.0);
}

/// The design's layout-edit strip: what the mode is, and the way out of it.
///
/// Sits under the options strip, where the design places it, and exists only
/// while the mode is on — a permanent bar explaining a mode you are not in
/// would be noise.
pub fn edit_bar(root: &mut Ui, p: &Palette, ed: &mut Editor) {
    if !ed.layout.edit_mode() {
        return;
    }
    let frame = Frame {
        fill: p.control_active,
        stroke: Stroke::new(1.0, p.accent_dim),
        inner_margin: egui::Margin::symmetric(12, 0),
        ..Default::default()
    };
    egui::Panel::top("layout-edit-bar")
        .exact_size(metrics::EDIT_BAR)
        .frame(frame)
        .show(root, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(
                    egui::RichText::new("LAYOUT EDIT")
                        .size(text::TINY)
                        .color(p.accent)
                        .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "drag a panel by its header — a sidebar re-docks it, anywhere \
                         else floats · drag an edge to resize · the cross removes",
                    )
                    .size(text::TINY)
                    .color(p.text_dim),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if edit_bar_link(ui, p.text, "Back to painting")
                        .on_hover_text("Leave layout edit mode")
                        .clicked()
                    {
                        ed.layout.set_edit_mode(false);
                    }
                    ui.add_space(12.0);
                    // The way back from having removed one, next to the mode
                    // that removes them. The Window menu has it too, but that
                    // is two clicks away from the bar that explains the mode.
                    if edit_bar_link(ui, p.text_dim, "Add a module")
                        .on_hover_text("Every module there is, and what each one does")
                        .clicked()
                    {
                        ed.ui.module_library_open = true;
                    }
                    ui.add_space(12.0);
                    if edit_bar_link(ui, p.text_dim, "Reset layout")
                        .on_hover_text("Put every panel back where it started")
                        .clicked()
                    {
                        ed.layout.reset();
                    }
                });
            });
        });
}

fn edit_bar_link(ui: &mut Ui, colour: egui::Color32, label: &str) -> egui::Response {
    ui.add(
        egui::Label::new(egui::RichText::new(label).size(text::TINY).color(colour))
            .sense(Sense::click()),
    )
}

/// The Window menu's layout section: the edit mode, which panels are shown,
/// which side the rail is on, and the way back from a wrecked arrangement.
pub fn window_menu(ui: &mut Ui, ed: &mut Editor) {
    let mut editing = ed.layout.edit_mode();
    if ui
        .checkbox(&mut editing, "Customise layout…")
        .on_hover_text("Makes the panels draggable. The canvas is paused meanwhile.")
        .clicked()
    {
        ed.layout.set_edit_mode(editing);
        ui.close();
    }

    // The one way in. There used to be a "Panels" submenu of checkboxes beside
    // this, from before the library existed; two lists of the same modules, one
    // of which showed a picture and a description and the other a tick, is a
    // choice nobody should have to make. The library shows what each module is
    // and hands it to the pointer; the cross in a module's header, in layout
    // edit mode, takes it back out.
    if ui
        .button("Modules…")
        .on_hover_text("Every module there is, what each one does, and a way to add it")
        .clicked()
    {
        ed.ui.module_library_open = true;
        ui.close();
    }

    if ui
        .button("Reset layout")
        .on_hover_text("Put every panel back where it started")
        .clicked()
    {
        ed.layout.reset();
        ui.close();
    }
}
