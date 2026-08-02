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
use crate::dock::{ColumnGeometry, DropTarget, Floating, Geometry, PanelKind, Side, limits};
use crate::editor::{Editor, Tool};
use crate::icons::{self, Icon};
use crate::layerdrag;
use crate::shortcuts::{self, Action};
use crate::theme::{Palette, metrics, text};
use crate::ui::{UiActions, icon_button};
use crate::widgets;
use egui::{
    Align, Align2, CursorIcon, FontId, Frame, Id, LayerId, Layout, Order, Pos2, Rect, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use std::f32::consts::{FRAC_PI_2, PI};
use std::time::Duration;
use umber_core::{BlendMode, EditKind, EditTarget, LayerStack, Timestamp};

/// Grab area of a splitter. Wider than the 1 px rule it draws, because a 1 px
/// target is not something anyone can hit.
const SPLITTER_GRAB: f32 = 7.0;

/// What a panel's chrome reported this frame. The caller applies these, because
/// the layout cannot be mutated while it is being iterated.
#[derive(Default)]
pub(crate) struct PanelEvents {
    /// Pointer position and the panel's rect at the moment a drag began.
    grab: Option<(Pos2, Rect)>,
    close: bool,
}

/// Draw every docked column, its panels and its splitters.
///
/// One `egui::Panel` per column, claimed in the model's own order — outermost
/// first — so what egui lays out and what [`Geometry`] predicted are the same
/// rects. Whatever is left when the loop finishes is the central panel, and
/// therefore the canvas.
pub fn sidebars(
    root: &mut Ui,
    p: &Palette,
    ed: &mut Editor,
    actions: &mut UiActions,
    geo: &Geometry,
) {
    for side in Side::ALL {
        for (column, at) in geo.columns(side).iter().enumerate() {
            let frame = Frame {
                fill: p.dock,
                ..Default::default()
            };
            // The id has to name the column as well as the side: two panels
            // sharing one would fight over the same stored state and the outer
            // one would take the inner one's width.
            let id = Id::new(("dock", side.index(), column));
            let panel = match side {
                Side::Left => egui::Panel::left(id),
                Side::Right => egui::Panel::right(id),
            };
            panel
                .exact_size(at.rect.width())
                .frame(frame)
                // `width_splitter` draws this edge itself, and lights it up in
                // the accent while it is being dragged. egui's separator lands
                // on the same pixel and is painted afterwards, so leaving it on
                // put a dim rule over the highlight and the resize affordance
                // never showed.
                .show_separator_line(false)
                // And egui's own resize handle has to go with it, or the edge
                // cannot be dragged at all. A panel is resizable by default,
                // and egui registers that handle *after* the body precisely so
                // it beats anything inside — a five-point band either side of
                // the edge, overlapping five of `width_splitter`'s seven. Since
                // `exact_size` pins the range to a point, every drag that
                // landed in the overlap was swallowed and did nothing. The
                // width is the dock model's, not egui's: it is clamped per
                // column by what the modules in it need and it is what gets
                // written to the layout file, neither of which egui knows.
                .resizable(false)
                .show(root, |ui| sidebar(ui, p, ed, actions, side, column, at));
        }
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
    column: usize,
    at: &ColumnGeometry,
) {
    let slots = &at.slots;
    // Snapshot the stack: drawing a panel can start a drag, which removes it
    // from the layout, and the loop must not be reading the Vec when it does.
    let kinds: Vec<PanelKind> = ed
        .layout
        .docked(side, column)
        .iter()
        .map(|d| d.kind)
        .collect();

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

    height_splitters(ui, p, ed, side, column, slots);
    width_splitter(ui, p, ed, side, column, at.rect);

    if let Some(kind) = closed {
        ed.layout.close(kind);
    }
    if let Some((kind, (pointer, rect))) = grabbed {
        ed.layout.begin_drag(kind, pointer, rect);
    }
}

/// The draggable boundaries between stacked panels.
fn height_splitters(
    ui: &mut Ui,
    p: &Palette,
    ed: &mut Editor,
    side: Side,
    column: usize,
    slots: &[Rect],
) {
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
                ui.id().with(("vsplit", side.index(), column, index)),
                Sense::drag(),
            )
            .on_hover_cursor(CursorIcon::ResizeVertical);
        if response.dragged() {
            ed.layout
                .resize_split(side, column, index, response.drag_delta().y, &heights);
        }
        if response.hovered() || response.dragged() {
            ui.painter()
                .hline(handle.x_range(), y, Stroke::new(2.0, p.accent));
        }
    }
}

/// The draggable inner edge that sets one column's width.
///
/// The handle sits *inside* the column rather than straddling its edge, so that
/// grabbing it counts as pointing at the panel and never at the canvas —
/// otherwise the first pixel of a resize drag would also start a stroke. That
/// is also what keeps two neighbouring columns' handles apart: each is inside
/// its own column, at the edge facing the canvas.
fn width_splitter(
    ui: &mut Ui,
    p: &Palette,
    ed: &mut Editor,
    side: Side,
    column: usize,
    rect: Rect,
) {
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
            ui.id().with(("hsplit", side.index(), column)),
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
        ed.layout.set_width(side, column, width);
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
///
/// Crate-visible rather than private because `docshot` draws a module on its own
/// for the README. It takes an explicit rect already — the dock model works
/// every rect out up front — so a caller outside the sidebar needs nothing the
/// sidebar does not already supply.
pub(crate) fn panel(
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
                    PanelKind::Tools => tools_body(ui, p, ed),
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

/// The dash pattern every dashed affordance here is drawn with. One pair, so
/// the column outline and the dock indicator cannot drift apart.
const DASH: f32 = 5.0;
const GAP: f32 = 4.0;

/// A dashed outline round a rounded rect.
///
/// egui strokes rects solid; the design's dock affordances are dashed, and a
/// dashed border is what distinguishes "this is where it will go" from a real
/// piece of chrome.
///
/// **One closed polyline, corners included.** Dashing the four straight edges
/// separately — which is what this used to do — draws a rectangle with its
/// corners missing, and it restarts the pattern four times so the dashes do not
/// line up across a corner even when one is drawn. `dashes_from_line` carries
/// its position along the whole path it is given, so handing it the corner arcs
/// as part of that path is all it takes for the pattern to run continuously the
/// whole way round.
fn dashed_rect(painter: &egui::Painter, rect: Rect, radius: f32, stroke: Stroke) {
    let points = rounded_outline(rect, radius);
    painter.extend(egui::Shape::dashed_line(&points, stroke, DASH, GAP));
}

/// The rounded rectangle as a closed polyline, starting and ending at the
/// middle of the top edge.
///
/// A closed dashed path has exactly one seam — the point where the last dash
/// meets the first — because the perimeter is not a whole number of dash-plus-
/// gap. Starting mid-edge puts it in the middle of the longest straight run
/// rather than at a corner, which is where the eye is already looking for a
/// join.
fn rounded_outline(rect: Rect, radius: f32) -> Vec<Pos2> {
    let r = radius
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5)
        .max(0.0);
    // A radius this small is a square corner: the arc would be a run of
    // coincident points, and a zero-length segment is a dash with no direction.
    if r < 0.5 {
        return vec![
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
            rect.left_top(),
        ];
    }

    // Enough segments that the chord error is well under a pixel at the radii
    // the design uses, and no more: this runs once per outline per frame.
    let steps = ((r * 0.8).ceil() as usize).clamp(2, 12);
    let start = pos2(rect.center().x, rect.top());
    let mut points = Vec::with_capacity(4 * (steps + 1) + 2);
    points.push(start);
    // Clockwise on screen, y down: top-right, bottom-right, bottom-left,
    // top-left. Each arc's first point is where the straight edge before it
    // ended, so the edges fall out of the gaps between the arcs.
    for (centre, from) in [
        (pos2(rect.right() - r, rect.top() + r), -FRAC_PI_2),
        (pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (pos2(rect.left() + r, rect.bottom() - r), FRAC_PI_2),
        (pos2(rect.left() + r, rect.top() + r), PI),
    ] {
        for i in 0..=steps {
            let angle = from + FRAC_PI_2 * (i as f32 / steps as f32);
            points.push(centre + vec2(angle.cos(), angle.sin()) * r);
        }
    }
    points.push(start);
    points
}

/// The dashed outline the design puts round a docked column while the layout is
/// being edited, so it reads as a container you can drop into.
///
/// One per column rather than one per side: two columns side by side are two
/// containers, and an outline round the pair would say the boundary between
/// them is not a place anything can go, which is exactly where a new column
/// lands.
pub fn edit_mode_outline(root: &mut Ui, p: &Palette, ed: &Editor, geo: &Geometry) {
    if !ed.layout.edit_mode() {
        return;
    }
    let painter = root.ctx().layer_painter(LayerId::new(
        Order::Foreground,
        Id::new("dock-edit-outline"),
    ));
    for side in Side::ALL {
        for column in geo.columns(side) {
            dashed_rect(
                &painter,
                column.rect.shrink(4.0),
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
        DropTarget::Dock {
            side,
            column,
            index,
        } => {
            // The design's dock indicator: a dashed accent block reading "dock
            // here". It is drawn *at the insertion point* rather than always at
            // the top of the sidebar, because unlike the design's model this
            // one can insert between two panels, and an indicator that lied
            // about where the panel lands would be worse than none.
            let zone = geo
                .columns(side)
                .get(column)
                .map_or_else(|| geo.drop_zone(side), |c| c.rect);
            let (a, b) = geo.insertion_line(side, column, index);
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
        // A column of its own, at the boundary the pointer is nearest. Full
        // height, because that is what the column will be — the block a stack
        // drop draws would say "somewhere in this list" and mean the opposite.
        DropTarget::NewColumn { side, column } => {
            let strip = geo.new_column_strip(side, column);
            painter.rect_filled(strip, 8.0, p.accent.gamma_multiply(0.09));
            dashed_rect(&painter, strip, 8.0, Stroke::new(2.0, p.accent));
            if strip.height() > 34.0 && strip.width() > 60.0 {
                painter.text(
                    strip.center(),
                    Align2::CENTER_CENTER,
                    "new column",
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

/// The Tools module: the design's tool grid, and the painting and background
/// colours under it.
///
/// This was a strip of chrome with a side of its own and its own drag handle
/// until it became a module. Two things follow from the move and are the whole
/// of what changed inside it. Its width is the column's rather than a constant,
/// so the grid **wraps** — at [`metrics::TOOL_RAIL`] that is the design's two
/// columns, and it is whatever fits at any other width, rather than two
/// columns overflowing a narrow panel. And each row is centred by hand: a
/// `horizontal` inside a centred vertical still takes the full width and lays
/// its buttons out from the left, so a column dragged wider than the grid would
/// otherwise leave them against its edge.
fn tools_body(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing = vec2(metrics::TOOL_GAP, metrics::TOOL_GAP);

        // Umber has six tools where the design shows sixteen; the rest are
        // simply not drawn, rather than shown as buttons that do nothing.
        //
        // The keys come from the binding table rather than being written in:
        // these tooltips were a second copy of it, and rebinding the brush left
        // this one still promising `B`.
        let tools = [
            (
                Tool::Brush,
                Icon::Brush,
                shortcuts::labelled("Brush", Action::BrushTool),
            ),
            (
                Tool::Eraser,
                Icon::Eraser,
                shortcuts::labelled("Eraser", Action::EraserTool),
            ),
            (
                Tool::Select,
                Icon::Select,
                shortcuts::labelled("Select", Action::SelectTool),
            ),
            (
                Tool::Transform,
                Icon::Transform,
                shortcuts::labelled("Transform", Action::TransformTool),
            ),
            (
                Tool::Pan,
                Icon::Pan,
                format!(
                    "{}, or hold Space",
                    shortcuts::labelled("Pan", Action::PanTool)
                ),
            ),
            (
                Tool::Zoom,
                Icon::Zoom,
                shortcuts::labelled("Zoom", Action::ZoomTool),
            ),
        ];

        let step = metrics::TOOL_BUTTON + metrics::TOOL_GAP;
        let per_row = (((ui.available_width() + metrics::TOOL_GAP) / step) as usize).max(1);
        let mut picked = None;
        for row in tools.chunks(per_row) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = metrics::TOOL_GAP;
                let used = row.len() as f32 * step - metrics::TOOL_GAP;
                ui.add_space(((ui.available_width() - used) * 0.5).max(0.0));
                for (tool, icon, tip) in row {
                    if widgets::tool_button(ui, p, *icon, ed.ui.tool == *tool, tip).clicked() {
                        picked = Some(*tool);
                    }
                }
            });
        }
        if let Some(tool) = picked {
            ed.set_tool(tool);
        }

        ui.add_space(6.0);
        let (line, _) =
            ui.allocate_exact_size(vec2(ui.available_width().min(44.0), 1.0), Sense::hover());
        ui.painter().rect_filled(line, 0.0, p.border);
        ui.add_space(6.0);

        // Overlapping foreground/background wells, click to swap.
        let (well, response) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
        let fg = Rect::from_min_size(well.left_top(), vec2(24.0, 24.0));
        let bg = Rect::from_min_size(well.left_top() + vec2(10.0, 10.0), vec2(24.0, 24.0));
        let to32 = |c: umber_core::Color| {
            let [r, g, b, _] = c.to_srgb_u8();
            egui::Color32::from_rgb(r, g, b)
        };
        let painter = ui.painter();
        for (rect, colour) in [(bg, ed.secondary), (fg, ed.color)] {
            painter.rect_filled(rect, metrics::RADIUS, to32(colour));
            painter.rect_stroke(
                rect,
                metrics::RADIUS,
                Stroke::new(1.0, p.popover_border),
                StrokeKind::Outside,
            );
        }
        let swap = shortcuts::labelled("Swap colours", Action::SwapColours);
        if response.on_hover_text(&swap).clicked() {
            ed.swap_colors();
        }

        ui.add_space(4.0);
        // The design writes "X swap" under the wells. The key is the bound one,
        // for the same reason the tooltips above use it — and the caption goes
        // altogether when the command has no key, rather than naming none.
        if let Some(chord) = shortcuts::first_chord(Action::SwapColours) {
            ui.label(
                egui::RichText::new(format!("{chord} swap"))
                    .size(9.0)
                    .color(p.text_dim.gamma_multiply(0.8)),
            );
        }
    });
}

fn colour_body(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut shape = ed.ui.wheel_shape;
    let mut rotates = ed.ui.wheel_rotates;
    let mut angles = ed.ui.wheel_angles;
    let changed = colorpicker::show(
        ui,
        p,
        ed.ui.picker,
        &mut shape,
        &mut rotates,
        &mut angles,
        &mut ed.hsv,
    );
    // All three are kept between runs, though their controls are here rather
    // than in the settings dialog — they are choices about the workspace, and
    // where one is set does not decide whether it should still be true tomorrow.
    //
    // Compared before and after rather than asked of the controls, because
    // `show` reports a change of *colour*: keying off its return would queue a
    // preferences write for every frame of a drag around the hue ring.
    if shape != ed.ui.wheel_shape || rotates != ed.ui.wheel_rotates || angles != ed.ui.wheel_angles
    {
        ed.ui.wheel_shape = shape;
        ed.ui.wheel_rotates = rotates;
        ed.ui.wheel_angles = angles;
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

/// The blend picker's width on the layer row it shares with the opacity slider.
///
/// Fixed, and this is one of the two places a dropdown's width is: the row has
/// exactly one thing on it that wants the spare room and it is the rail, not the
/// picker. Wide enough for "Multiply", the longest mode there is, and for the
/// one after it should the stack ever gain another.
const BLEND_WIDTH: f32 = 80.0;

/// What the ticked-layers strip was pressed for.
///
/// Collected and applied below the layout closure rather than inside it,
/// because every arm needs `ed.layers` mutably and the closure is already
/// holding it for the labels.
#[derive(Clone, Copy)]
enum Bulk {
    Visible(bool),
    Lock(bool),
    Link,
    Unlink,
    /// Tick or untick every layer at once.
    Tick(bool),
    Delete,
}

fn layers_body(ui: &mut Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    let count = ed.layers.len();
    let active = ed.layers.active_index();
    // Read once: half a dozen controls below answer to it, and a lock read
    // twice is a lock that can be read differently twice.
    let locked = ed.layers.active_is_locked();

    ui.horizontal(|ui| {
        // Whose settings these are. The blend picker and the opacity slider
        // below edit the *selected* layer — `Layer::blend` and `Layer::opacity`
        // have always been per-layer — and with nothing saying so the pair read
        // as a document-wide setting, which is the one thing they are not.
        // Typography is `controls::section`'s, so the panel gains a heading and
        // not a second heading style; it is inline rather than a call to it
        // because that helper is a block with its own spacing and this shares
        // the icon row.
        ui.label(
            egui::RichText::new("Layer settings")
                .size(text::SMALL)
                .color(p.text_dim)
                .strong(),
        )
        .on_hover_text("Blend mode and opacity apply to the selected layer");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(
                ui,
                p,
                Icon::Trash,
                // Asked of the model rather than counted here: a folder is not
                // somewhere to paint, so "more than one entry" is not the same
                // question as "something would be left to paint on", and
                // deleting a folder takes every layer inside it.
                ed.layers.can_remove(&[active]) && !locked,
                match (
                    ed.layers.can_remove(&[active]),
                    locked,
                    ed.layers.active_is_folder(),
                ) {
                    (_, true, _) => "The layer is locked — unlock it to delete it",
                    (false, _, _) => "A document needs a layer to paint on",
                    (_, _, true) => "Delete the group and everything in it — clears undo history",
                    _ => "Delete layer — clears undo history",
                },
            ) {
                actions.delete_layer = Some(active);
            }
            // Enabled by what the move would actually do, not by the index:
            // a folder steps over its whole subtree, and one at the top of a
            // group has nowhere to go without changing its nesting, which is
            // something dragging says and a chevron cannot.
            let span = ed.layers.subtree(active);
            let depth = depth_of(ed, active);
            let can_down = span.start > 0 && ed.layers.can_reorder(active, span.start - 1, depth);
            let can_up = span.end < count && ed.layers.can_reorder(active, span.end, depth);
            // The tooltip says why when it is dead, and names the gesture that
            // *can* do it — a step keeps the nesting it has, so leaving a group
            // is something only a drag can say.
            let step_tip = |can: bool, way: &'static str| {
                if can {
                    if way == "up" {
                        "Move layer up"
                    } else {
                        "Move layer down"
                    }
                } else if depth > 0 {
                    "Nowhere to step inside this group — drag it sideways to leave"
                } else {
                    "Already at the end of the stack"
                }
            };
            if icon_button(
                ui,
                p,
                Icon::ChevronDown,
                can_down,
                step_tip(can_down, "down"),
            ) {
                actions.move_layer_down = Some(active);
            }
            if icon_button(ui, p, Icon::ChevronUp, can_up, step_tip(can_up, "up")) {
                actions.move_layer_up = Some(active);
            }
            if icon_button(
                ui,
                p,
                Icon::Plus,
                count < LayerStack::MAX,
                if ed.layers.active_is_folder() {
                    "Add a layer inside the selected group"
                } else {
                    "Add a layer above the current one"
                },
            ) {
                actions.add_layer = true;
            }
            // Grouping reaches the same set every other bulk control does —
            // `LayerStack::targets`, so the ticked layers or the selected one.
            // Nothing about a folder is drawn beyond its name and its eye: a
            // pass-through folder has no opacity and no blend mode, and a slider
            // that did nothing would be exactly the control that lies. See
            // `docs/layer-folders.md`.
            // Asked of the model, like the chevrons above: a full stack and a
            // set that would nest too deep are both refusals `group` makes and
            // neither is visible from here.
            let can_group = ed.layers.can_group(&ed.layers.targets());
            if icon_button(
                ui,
                p,
                Icon::Folder,
                can_group,
                if can_group {
                    "Put the ticked layers — or the selected one — in a group"
                } else if count >= LayerStack::MAX {
                    "The stack is full — a group is an entry too"
                } else {
                    "That would nest deeper than Umber can hold"
                },
            ) {
                actions.group_layers = true;
            }
        });
    });

    ui.add_space(4.0);

    // The selected layer's flags, on one compact row of their own.
    //
    // A row of the *list* would have been the other place for them, and it is
    // the wrong one: the list is what has to stay short enough to show a stack
    // at a glance, and four more targets per row would either grow the row or
    // shrink the name to nothing. The list *shows* the flags — see
    // `widgets::layer_row` — and this is where they are changed, which is the
    // same division the blend picker and the blend label already keep.
    // **A folder has none of this.** No mask, no clipping, no blend mode and no
    // opacity of its own — a pass-through folder is its contents composited in
    // place. The controls are not drawn rather than drawn disabled: the design
    // rule allows a disabled control with an explanation where there is one
    // thing missing, and this is a whole block of them that will never apply to
    // the selected entry. Its lock *is* drawn, because a lock on a folder means
    // something and reaches everything inside it.
    let mut changed = false;
    let is_folder = ed.layers.active_is_folder();
    if !is_folder {
        ui.horizontal(|ui| {
            let has_mask = ed.layers.active_mask().is_some();
            if widgets::icon_toggle(
                ui,
                p,
                Icon::Mask,
                has_mask,
                !locked,
                match (has_mask, locked) {
                    (_, true) => "The layer is locked — unlock it to change its mask",
                    (true, _) => "Remove the layer mask — clears undo history",
                    (false, _) => "Add a layer mask, revealing everything",
                },
            ) {
                if has_mask {
                    actions.remove_mask = true;
                } else {
                    actions.add_mask = true;
                }
            }

            let layer = ed.layers.active_mut();
            if widgets::icon_toggle(
                ui,
                p,
                Icon::Clip,
                layer.clipped,
                true,
                "Clip to the layer below — the layer only shows where that one does",
            ) {
                layer.clipped = !layer.clipped;
                changed = true;
            }
            let is_locked = layer.locked;
            if widgets::icon_toggle(
                ui,
                p,
                if is_locked { Icon::Lock } else { Icon::Unlock },
                is_locked,
                true,
                if is_locked {
                    "Unlock the layer"
                } else {
                    "Lock the layer — no strokes, transforms, clearing or flipping"
                },
            ) {
                layer.locked = !is_locked;
                changed = true;
            }
            // Linking is deliberately *not* here, where the other three flags are.
            // It is the one thing on a layer that is a statement about several
            // layers at once — a group of one says nothing — so it belongs to the
            // ticked strip and there is exactly one of it. A chain on this row
            // would have to mean "link this to what?".

            // Which of the layer's two surfaces a stroke lands in. Drawn only where
            // there is a mask to paint, because a switch with one position is a
            // control that lies about there being a choice.
            if ed.layers.active_mask().is_some() {
                ui.add_space(4.0);
                let on_mask = ed.editing_mask();
                for (label, target, tip) in [
                    (
                        "Layer",
                        EditTarget::Layer,
                        "Strokes land in the layer's pixels",
                    ),
                    (
                        "Mask",
                        EditTarget::Mask,
                        "Strokes land in the mask: black hides, white reveals",
                    ),
                ] {
                    let selected = (target == EditTarget::Mask) == on_mask;
                    if ui
                        .selectable_label(
                            selected,
                            egui::RichText::new(label)
                                .size(text::SMALL)
                                .color(if selected { p.text_strong } else { p.text_dim }),
                        )
                        .on_hover_text(tip)
                        .clicked()
                    {
                        ed.edit_target = target;
                    }
                }
            }
        });
    } else {
        ui.horizontal(|ui| {
            let layer = ed.layers.active_mut();
            let is_locked = layer.locked;
            if widgets::icon_toggle(
                ui,
                p,
                if is_locked { Icon::Lock } else { Icon::Unlock },
                is_locked,
                true,
                if is_locked {
                    "Unlock the group"
                } else {
                    "Lock the group — nothing in it can be painted on or cleared"
                },
            ) {
                layer.locked = !is_locked;
                changed = true;
            }
            // **Not a label beside it.** A label in an egui horizontal layout
            // defaults to `TextWrapMode::Extend`, so a sentence here sizes the
            // strip — and with it the panel and the window — instead of being
            // sized by it. That is the exact failure `brushlib::notice_bar` and
            // `controls::banner` were written to avoid, and putting one here
            // pushed the layer list past the right edge of the window: the
            // blend labels read "Nor". Seen in a running window, which is the
            // only way this shows up.
            ui.label(
                egui::RichText::new("A group carries its layers")
                    .size(text::TINY)
                    .color(p.text_muted),
            )
            .on_hover_text(
                "A group has no blend mode and no opacity of its own — its \
                 layers composite in place. Its eye and its lock reach \
                 everything inside it.",
            );
        });
    }

    ui.add_space(4.0);

    // Blend and opacity for the selected layer, on one row.
    //
    // Both change the picture, so both have to mark the document modified —
    // otherwise the close prompt, which asks only about modified documents,
    // would let a tab holding a carefully set stack of opacities close without
    // a word. Collected and applied below the borrow rather than inside it,
    // since `mark_modified` also wants `ed`.
    if !is_folder {
        ui.horizontal(|ui| {
            let layer = ed.layers.active_mut();
            let before = (layer.blend, layer.opacity);
            // A fixed width rather than the layout's: the slider beside it is the
            // control that should take the room the row has spare.
            let label = layer.blend.label();
            widgets::dropdown(
                ui,
                p,
                widgets::Dropdown::new(label).width(widgets::DropdownWidth::Exact(BLEND_WIDTH)),
                |ui| {
                    for mode in BlendMode::ALL {
                        ui.selectable_value(&mut layer.blend, mode, mode.label());
                    }
                },
            );
            let value = layer.opacity;
            widgets::bare_slider(ui, p, &mut layer.opacity, 0.0..=1.0);
            changed |= before != (layer.blend, layer.opacity);
            ui.label(
                egui::RichText::new(format!("{:.0}", value * 100.0))
                    .monospace()
                    .size(10.0)
                    .color(p.text),
            );
        });
    }

    ui.add_space(7.0);

    // What ticking rows is *for*, drawn only once something is ticked.
    //
    // A strip that was always there would cost the list a row of height on
    // every document, most of which have three layers and no use for it; and a
    // row of controls that do nothing is the thing CLAUDE.md refuses
    // everywhere else. The count is the label, because "3 ticked" is the one
    // fact a strip like this has to tell you before you press Delete.
    if ed.layers.picked_count() > 0 {
        let picked = ed.layers.picked_count();
        // Through `effective_locked`, so a folder's lock protects what is
        // inside it — the same question `delete_layer`'s gate asks.
        let any_locked = ed
            .layers
            .targets()
            .iter()
            .any(|i| ed.layers.effective_locked(*i));
        let can_delete = ed.layers.can_remove(&ed.layers.targets());
        let mut act: Option<Bulk> = None;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{picked} ticked"))
                    .size(text::SMALL)
                    .color(p.text_muted),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(
                    ui,
                    p,
                    Icon::Trash,
                    can_delete && !any_locked,
                    match (can_delete, any_locked) {
                        (false, _) => "A document needs a layer to paint on",
                        (_, true) => "One of them is locked — unlock it to delete it",
                        _ => "Delete the ticked layers — clears undo history",
                    },
                ) {
                    act = Some(Bulk::Delete);
                }
                if icon_button(ui, p, Icon::Unlock, true, "Unlock the ticked layers") {
                    act = Some(Bulk::Lock(false));
                }
                // One chain, which links or unlinks depending on what the
                // ticked layers already are. Two buttons would be two spellings
                // of the same question, and the tooltip says which this is
                // before it is pressed.
                let already = ed.layers.shared_group(&ed.layers.targets());
                let room = ed.layers.free_group().is_some();
                if widgets::icon_toggle(
                    ui,
                    p,
                    Icon::Chain,
                    already.is_some(),
                    already.is_some() || (picked > 1 && room),
                    match (already.is_some(), picked > 1, room) {
                        (true, _, _) => "Unlink them — they stop moving together",
                        (_, false, _) => "Tick two or more layers to link them",
                        (_, _, false) => "Every link group is in use — unlink one first",
                        _ => "Link them — they move through the stack together",
                    },
                ) {
                    act = Some(match already {
                        Some(_) => Bulk::Unlink,
                        None => Bulk::Link,
                    });
                }
                if icon_button(ui, p, Icon::Lock, true, "Lock the ticked layers") {
                    act = Some(Bulk::Lock(true));
                }
                if icon_button(ui, p, Icon::EyeOff, true, "Hide the ticked layers") {
                    act = Some(Bulk::Visible(false));
                }
                if icon_button(ui, p, Icon::Eye, true, "Show the ticked layers") {
                    act = Some(Bulk::Visible(true));
                }
                // Words rather than marks, because "tick all of them" has no
                // icon anybody would recognise and `icons::Icon` gaining one
                // that has to be explained is worse than two short labels.
                for (label, tip, all) in [
                    ("None", "Untick every layer", false),
                    ("All", "Tick every layer", true),
                ] {
                    if ui
                        .selectable_label(
                            false,
                            egui::RichText::new(label)
                                .size(text::SMALL)
                                .color(p.text_dim),
                        )
                        .on_hover_text(tip)
                        .clicked()
                    {
                        act = Some(Bulk::Tick(all));
                    }
                }
            });
        });
        match act {
            // Straight onto the flags: nothing here touches the GPU or the
            // history, so there is no reason to send it round through
            // `UiActions` and back. Deleting is the one that does.
            Some(Bulk::Visible(on)) => {
                for index in ed.layers.targets() {
                    if let Some(layer) = ed.layers.get_mut(index) {
                        layer.visible = on;
                    }
                }
                changed = true;
            }
            Some(Bulk::Lock(on)) => {
                for index in ed.layers.targets() {
                    if let Some(layer) = ed.layers.get_mut(index) {
                        layer.locked = on;
                    }
                }
                changed = true;
            }
            Some(Bulk::Link) => {
                ed.layers.link(&ed.layers.targets());
                changed = true;
            }
            Some(Bulk::Unlink) => {
                ed.layers.unlink(&ed.layers.targets());
                changed = true;
            }
            Some(Bulk::Tick(on)) => ed.layers.pick_all(on),
            // Slots go back on the free list, so this clears the undo history
            // and has to happen where the GPU is. `UiActions` is `Copy` and
            // cannot carry the list; the caller reads the ticks off the editor
            // in the frame the flag was set, exactly as `new_tip` does.
            Some(Bulk::Delete) => actions.delete_picked = true,
            None => {}
        }
        ui.add_space(6.0);
    }

    // What is being carried, if anything. Kept in egui's temporary store rather
    // than on `Editor`, which is where `history_body`'s scroll memo lives and
    // for the same reason: this belongs to the list, not to the document, and a
    // tab switch has nothing to say about it.
    let mut drag: Option<layerdrag::Drag> = ui.ctx().data(|d| d.get_temp(layer_drag_id()));

    // The row a drop would land on, as `Drag::aim` left it at the end of the
    // *last* frame. One frame behind the pointer, which nobody can see in a
    // drag, and what it buys is that the mark can be handed to the row as its
    // own highlight rather than painted over the top of it — `layer_row` draws
    // its own name and blend, and a fill laid on afterwards would cover both.
    let aimed = drag.as_ref().and_then(layerdrag::Drag::destination);

    // Where the rows land, for the drag model. Collected only while a button is
    // down or something is already being carried: the list is redrawn every
    // frame and this would otherwise be a `Vec` built sixty times a second to
    // answer a question nobody is asking.
    let (pointer, origin, down, released, deciding) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.press_origin(),
            i.pointer.primary_down(),
            i.pointer.any_released(),
            i.pointer.is_decidedly_dragging(),
        )
    });
    let watching = drag.is_some() || down;
    let mut rows: Vec<layerdrag::Row> = Vec::new();

    // Stored bottom-first; shown top-first, the way it is drawn.
    let mut select = None;
    let mut toggle = None;
    let mut tick = None;
    let mut aim_at_mask = None;
    let mut fold = None;
    let editing_mask = ed.editing_mask();
    for index in (0..count).rev() {
        let Some(layer) = ed.layers.get(index) else {
            continue;
        };
        // A folded folder hides its contents. Purely a filter on what is drawn:
        // the model is untouched, so a collapsed group composites exactly as an
        // open one does and a fold can never change the picture. Rows carry
        // their stack index, which is what lets the list skip rows at all —
        // nothing downstream reads a row's place in the slice.
        if ed
            .layers
            .ancestors_of(index)
            .any(|a| ed.layers.get(a).is_some_and(|f| f.collapsed))
        {
            continue;
        }
        let target = aimed.map(|a| a.index) == Some(index);
        // Through a scope, purely to learn where the row landed: `layer_row`
        // reports what was clicked and not what it occupied, and a rect guessed
        // from the row height would be a second statement of a number
        // `widgets.rs` already owns.
        let placed = ui.scope(|ui| {
            widgets::layer_row(
                ui,
                p,
                widgets::LayerRow {
                    name: &layer.name,
                    // A slot where there is one, and something no slot can be
                    // where there is not — see `LayerRow::key`.
                    key: layer.slot().map_or(u64::MAX - index as u64, u64::from),
                    visible: layer.visible,
                    depth: layer.depth,
                    folder: layer.is_folder(),
                    collapsed: layer.collapsed,
                    hidden_by_folder: layer.visible && !ed.layers.effective_visible(index),
                    // The drop target borrows the selected row's own fill, so
                    // the mark is part of the row. The outline below is what
                    // keeps "the layer lands here" from reading as "this row is
                    // selected".
                    active: index == active || target,
                    blend: layer.blend.label(),
                    has_mask: layer.has_mask(),
                    // The edit target is per document, so only the selected row
                    // can be the one being painted into.
                    editing_mask: index == active && editing_mask,
                    clipped: layer.clipped,
                    locked: layer.locked,
                    link: layer.link,
                    thumb: layer.slot().and_then(|s| ed.thumbs.picture(s)),
                    picked: layer.picked,
                },
            )
        });
        let (row, rect) = (placed.inner, placed.response.rect);
        if target {
            ui.painter().rect_stroke(
                rect,
                metrics::RADIUS,
                Stroke::new(1.0, p.accent),
                StrokeKind::Inside,
            );
        }
        if watching {
            rows.push(layerdrag::Row {
                index,
                rect,
                depth: layer.depth,
                folder: layer.is_folder(),
            });
        }
        // A release that ends a drag is not also a click on the row it landed
        // on, nor on the eye it happened to pass over.
        if drag.is_some() {
            continue;
        }
        if row.fold_clicked {
            fold = Some(index);
        } else if row.pick_clicked {
            tick = Some(index);
        } else if row.eye_clicked {
            toggle = Some(index);
        } else if row.mask_clicked {
            // Selecting the layer as well as its mask: painting a mask on a
            // layer that is not the one being painted is not a state the
            // engine has, and clicking the chip plainly means both.
            select = Some(index);
            aim_at_mask = Some(true);
        } else if row.clicked {
            select = Some(index);
            // A click on the row proper — including its own thumbnail — is the
            // way back off the mask.
            aim_at_mask = Some(false);
        }
    }

    // Picking a layer up, aiming it and putting it down. All of it off the
    // pointer's own state rather than a `Response`, because the row belongs to
    // `widgets::layer_row` and senses clicks only — and a second widget laid
    // over the row to sense drags would be on top of the eye inside it, which
    // would leave the visibility toggle dead. egui still settles click against
    // drag: `is_decidedly_dragging` is exactly the condition under which it
    // stops calling the press a click, so a press that becomes a drag never
    // also selects and a click that never moves still does.
    if drag.is_none()
        && down
        && deciding
        && let Some(index) = origin.and_then(|at| layerdrag::row_pressed(&rows, at))
        && let Some(layer) = ed.layers.get(index)
    {
        drag = Some(layerdrag::Drag::new(index, layer.name.clone()));
    }
    if let Some(carried) = &mut drag {
        let from = carried.from;
        // The legality of a drop is `LayerStack`'s, asked rather than restated:
        // see `layerdrag`'s module docs. This is also what refuses a drop that
        // would move nothing, which used to be a comparison against `from` here.
        carried.aim(&rows, pointer, metrics::LAYER_INDENT, |to, depth| {
            ed.layers.can_reorder(from, to, depth)
        });
        drag_ghost(ui.ctx(), p, carried);
    }
    if !down && let Some(carried) = drag.take() {
        // `released` distinguishes the frame the button came up on from a drag
        // left in the store by a panel that stopped being drawn mid-gesture.
        // Without it, reopening the module with the pointer over the list would
        // resolve a drop nobody was making.
        if released
            && let Some(to) = carried.destination()
            // Reordering does not clear the undo history, and deleting a layer
            // does. The difference is `LayerStack::reorder`'s to state and it
            // states it: a `PixelPatch` names a *slot*, deleting frees one for
            // the next layer to inherit, and nothing here frees or reassigns
            // one. Stack order is the `Vec` order, so this moved no pixels —
            // and a folder holds no slot at all, so re-nesting frees none
            // either.
            && ed.layers.reorder_to(carried.from, to.index, to.depth)
        {
            changed = true;
        }
    }
    ui.ctx().data_mut(|d| match drag {
        Some(drag) => {
            d.insert_temp(layer_drag_id(), drag);
        }
        None => d.remove::<layerdrag::Drag>(layer_drag_id()),
    });

    if let Some(index) = toggle
        && let Some(layer) = ed.layers.get_mut(index)
    {
        layer.visible = !layer.visible;
        changed = true;
    }
    // A tick is not a change to the document: it says what is about to be done,
    // not what the picture holds, which is also why it is never written to the
    // file. Marking the tab modified for one would put a dot on it for a
    // gesture that changed no pixel.
    if let Some(index) = tick
        && let Some(on) = ed.layers.get(index).map(|l| !l.picked)
    {
        // Through `pick`, which cascades into a folder's contents — the half of
        // "tick a group to act on everything in it" that the model owns. The
        // other half is that `targets` was not changed at all.
        ed.layers.pick(index, on);
    }
    // Folding is not a change to the document: it says what the list shows, not
    // what the picture holds, which is also why it is never written to the file.
    if let Some(index) = fold
        && let Some(layer) = ed.layers.get_mut(index)
    {
        layer.collapsed = !layer.collapsed;
    }
    if let Some(index) = select {
        ed.layers.set_active(index);
    }
    if let Some(mask) = aim_at_mask {
        ed.edit_target = if mask {
            EditTarget::Mask
        } else {
            EditTarget::Layer
        };
    }
    if changed {
        ed.mark_modified();
    }
}

/// The nesting of one entry, for the move buttons' enablement.
fn depth_of(ed: &Editor, index: usize) -> u8 {
    ed.layers.get(index).map_or(0, |l| l.depth)
}

/// Where the layer being dragged is kept between frames.
fn layer_drag_id() -> Id {
    Id::new("layer-drag")
}

/// The label that follows the pointer while a layer is being carried.
///
/// On egui's tooltip layer, so it rides over the list rather than being clipped
/// to it, and painted rather than added as a widget: nothing about it is
/// interactive, and a widget sitting under the pointer through a drag would
/// take the hover the rows need. Same shape as the brush library's ghost, minus
/// the destination — the list says where the layer lands by lighting the row up
/// under the pointer, where the collection rail is a list of names the pointer
/// may be nowhere near.
fn drag_ghost(ctx: &egui::Context, p: &Palette, drag: &layerdrag::Drag) {
    let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) else {
        return;
    };
    ctx.set_cursor_icon(CursorIcon::Grabbing);

    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("layer-drag-ghost")));
    let galley = painter.layout_no_wrap(
        drag.name.clone(),
        FontId::proportional(text::TINY),
        p.text_strong,
    );
    let rect = Rect::from_min_size(pointer + vec2(14.0, 12.0), galley.size() + vec2(16.0, 9.0));
    painter.rect_filled(rect, metrics::RADIUS, p.popover);
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(1.0, p.popover_border),
        StrokeKind::Inside,
    );
    painter.galley(rect.min + vec2(8.0, 4.5), galley, p.text_strong);
}

/// One row of the History list, as the list has worked it out.
///
/// A struct rather than seven positional arguments, because four of them are
/// booleans and a call site that reads `true, false, true, false` is one nobody
/// can check.
struct HistoryRow {
    /// What made the mark. A row's icon is the icon of the *tool* that made it
    /// — the brush mark for a stroke, the eraser block for an erase — so the
    /// list and the tool rail name the same action the same way. There is no
    /// icon here for anything Umber cannot undo; see [`history_body`].
    icon: Icon,
    label: &'static str,
    /// How long after the previous entry this one was made. `None` at the top
    /// of the list, and for an entry either side of which has no recorded time.
    gap: Option<Duration>,
    /// When it was made, for the tooltip. `None` for an entry restored from a
    /// document written before histories carried times.
    at: Option<Timestamp>,
    applied: bool,
    current: bool,
    scroll_here: bool,
}

/// The History module: what has been painted on this document, when, and a
/// click to go back to any point in it.
///
/// What it deliberately does *not* show is anything it cannot restore. Umber's
/// history covers painting, transforms and canvas flips — adding, deleting or
/// reordering a layer is not recorded, and deleting one clears the list
/// outright — so a row appears only where the engine can actually step back
/// over the edit, and the note at the foot says so rather than leaving the gap
/// to be discovered. A list that named a structural action it could not undo
/// would be worse than one that admits its own edges. That is also why there is
/// exactly one edit icon per `EditKind` and no more: an icon set richer than
/// the enum would be a promise about what the engine records.
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
    let opened = HistoryRow {
        icon: Icon::Document,
        label: base,
        // The document as it opened is not an edit and was not timed. Nothing
        // is shown against it rather than the moment the file happened to be
        // read, which is not when any of this was painted.
        gap: None,
        at: None,
        applied: true,
        current: at_start,
        scroll_here: at_start && follow,
    };
    if history_row(ui, p, &opened).clicked() {
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
        let row = HistoryRow {
            icon: edit_icon(kind),
            label: kind.label(),
            gap: ed.history.gap_at(index),
            at: ed.history.time_at(index),
            applied,
            current,
            scroll_here: current && follow,
        };
        if history_row(ui, p, &row).clicked() {
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
            "Nothing done to this document yet. Strokes, transforms and canvas \
             flips are recorded here; layers are not."
        } else {
            "Strokes, transforms and canvas flips. Adding, deleting or \
             reordering a layer is not recorded, and deleting one clears this \
             list."
        })
        .size(9.5)
        .color(p.text_dim)
        .line_height(Some(12.0)),
    );

    // "Earlier edits discarded" is true and says nothing about why, and the why
    // is not guessable — nothing else on screen mentions that undo has a size
    // at all. An entry is the whole *rectangle* a stroke covered, so its cost
    // follows the canvas rather than the mark: on a 10000² document a stroke
    // drawn across the picture is 400 MB on its own and the second one ages the
    // first out. Without this the list reads as a bug, and it was read as one.
    //
    // Same rule as the note above it: say where the edge is rather than leave
    // it to be discovered. The figure comes off the history rather than being
    // written down here, so the two cannot drift.
    if dropped > 0 {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!(
                "The history holds {} MB of pixels. An entry is the whole \
                 rectangle a stroke covered, so on a large canvas a few broad \
                 strokes fill it.",
                ed.history.budget_bytes() / (1024 * 1024)
            ))
            .size(9.5)
            .color(p.text_dim)
            .line_height(Some(12.0)),
        );
    }
}

/// Which mark stands for an edit of this kind.
///
/// The tool's own icon, so a row and the rail agree. Exhaustive over
/// [`EditKind`] on purpose: adding a variant to that enum should not compile
/// until somebody has decided what it looks like.
fn edit_icon(kind: EditKind) -> Icon {
    match kind {
        EditKind::Paint => Icon::Brush,
        EditKind::Erase => Icon::Eraser,
        EditKind::Transform => Icon::Transform,
        // The same two marks the floating transform's own flip buttons carry,
        // so a row and the control that could have produced it agree.
        EditKind::FlipHorizontal => Icon::FlipHorizontal,
        EditKind::FlipVertical => Icon::FlipVertical,
    }
}

/// One entry in that list: a marker, what made the mark, what it was, and how
/// long after the entry before it that happened.
///
/// Nothing off screen is painted and nothing here reaches the heap that egui
/// would not have reached anyway: the elapsed figure is formatted into
/// [`umber_core::time::Brief`], which lives on the stack, and the exact date is
/// spelled out only for the one row a pointer is actually over. The list is as
/// long as the session is, and a `format!` per visible row per frame shows up
/// in a frame time before anything else about it does.
fn history_row(ui: &mut Ui, p: &Palette, row: &HistoryRow) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), metrics::HISTORY_ROW),
        Sense::click(),
    );
    if row.scroll_here {
        response.scroll_to_me(Some(Align::Center));
    }
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter();
    if row.current {
        painter.rect_filled(rect, metrics::RADIUS, p.control_active);
    } else if response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control);
    }

    // The marker is the cursor: filled and accented where the document stands,
    // hollow behind it, and hollow and dim ahead of it.
    let ink = match (row.current, row.applied) {
        (true, _) => p.accent,
        (false, true) => p.text,
        (false, false) => p.text_dim.gamma_multiply(0.55),
    };
    let dot = pos2(rect.left() + 7.0, rect.center().y);
    if row.current {
        painter.circle_filled(dot, 3.5, ink);
    } else {
        painter.circle_stroke(dot, 3.0, Stroke::new(1.0, ink));
    }

    let icon_rect = Rect::from_center_size(pos2(dot.x + 13.0, rect.center().y), vec2(12.0, 12.0));
    icons::draw(painter, icon_rect, row.icon, ink);

    // The elapsed column, right-aligned, and the first thing drawn because the
    // label is clipped to whatever it leaves behind. `Painter::text` hands back
    // the rect it used, which is also the region the date tooltip answers to —
    // measuring it a second time would be a second layout of the same glyphs.
    //
    // A row that has a time but no measurable gap — the first entry, or a pair
    // the clock put in the wrong order — draws a hyphen, so its date is still
    // reachable by hovering. A row with no time at all draws nothing: there is
    // nothing to hover for, and an empty cell is the honest report.
    let dim = ink.gamma_multiply(0.7);
    let elapsed = row.gap.map(umber_core::time::brief);
    let time_rect = match (elapsed.as_ref(), row.at) {
        (Some(brief), _) => Some(brief.as_str()),
        (None, Some(_)) => Some("-"),
        (None, None) => None,
    }
    .map(|text| {
        painter.text(
            pos2(rect.right() - 8.0, rect.center().y),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(text::TINY),
            dim,
        )
    });

    // Clipped rather than shortened: the panel can be dragged narrow and this
    // is the column that gives, but the elapsed figure must never be walked
    // over by a label that has run out of room.
    let label_left = icon_rect.right() + 4.0;
    let label_right = time_rect.map_or(rect.right(), |r| r.left()) - 5.0;
    let clip = Rect::from_min_max(
        pos2(label_left, rect.top()),
        pos2(label_right.max(label_left), rect.bottom()),
    );
    painter
        .with_clip_rect(clip.intersect(painter.clip_rect()))
        .text(
            pos2(label_left, rect.center().y),
            Align2::LEFT_CENTER,
            row.label,
            FontId::proportional(text::SMALL),
            ink,
        );

    // One tooltip or the other, decided by where the pointer is, rather than a
    // second interactive widget inside the row: an `interact` over part of a
    // clickable row would take the click with it.
    let over_time = match (time_rect, response.hover_pos()) {
        (Some(r), Some(pos)) => r.expand(4.0).contains(pos),
        _ => false,
    };
    match (over_time, row.at) {
        // In the reader's own zone, because the one thing they might be doing
        // with this is comparing it against a clock in the room. The offset is
        // asked for at *that* moment, not now, so a document spanning a
        // daylight-saving change does not gain an hour halfway through an
        // afternoon. A platform that will not say falls back to UTC, and the
        // label says which they are looking at either way.
        (true, Some(at)) => response.on_hover_text(match crate::localtime::offset_at(at) {
            Some(offset) => at.describe_at(offset),
            None => at.describe(),
        }),
        _ => response.on_hover_text(if row.applied {
            "Go back to this point"
        } else {
            "Put this back"
        }),
    }
}

/// The Colour panel's picker-type switch: a half-filled disc, the mode name,
/// and a chevron.
///
/// The look every other dropdown in the interface now takes — see
/// [`widgets::dropdown`] — and the one that keeps its leading mark, because a
/// half-filled disc genuinely says "colour picker" where none of the others has
/// a glyph to hand.
fn picker_mode_switch(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let label = ed.ui.picker.label();
    widgets::dropdown(
        ui,
        p,
        widgets::Dropdown::new(label).icon(Icon::HalfCircle),
        |ui| {
            for mode in PickerMode::ALL {
                if ui
                    .selectable_label(ed.ui.picker == mode, mode.label())
                    .clicked()
                {
                    if ed.ui.picker != mode {
                        crate::prefs::mark_dirty();
                    }
                    ed.ui.picker = mode;
                }
            }
        },
    );
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
        // The tool grid, with one button picked out, and the colour wells.
        PanelKind::Tools => {
            for k in 0..4 {
                let cell = Rect::from_min_size(
                    pos2(
                        body.left() + (k % 2) as f32 * 9.0,
                        body.top() + 2.0 + (k / 2) as f32 * 9.0,
                    ),
                    vec2(7.0, 7.0),
                );
                painter.rect_filled(cell, 1.5, if k == 0 { p.accent } else { ink });
            }
            let well = pos2(body.left() + 3.5, body.bottom() - 5.0);
            painter.circle_stroke(well, 3.5, Stroke::new(1.0, ink));
            painter.circle_filled(well + vec2(4.0, 2.0), 3.0, p.accent);
        }
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
                        "drag a panel by its header — a column re-docks it, a column's \
                         edge starts a new one, anywhere else floats · drag an edge to \
                         resize · the cross removes",
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
