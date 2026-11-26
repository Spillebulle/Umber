//! Trace outline: the form [`Editor::ui`]'s `trace` holds, drawn as a modal.
//!
//! The shape is [`crate::exportdlg`]'s, for its reasons: drawn from
//! [`crate::ui::draw`] rather than from a panel body, because the layout can
//! hide a panel and a modal that goes with one cannot then be shut, and nothing
//! here reaches the GPU. Trace *asks* (`Editor::trace_requested`) and the
//! caller does the work, because the alpha comes off the device. The dialog is
//! not closed by Trace either: `App` closes it once the paths have landed and
//! leaves it up behind the notice when the trace is refused, so the margin
//! somebody was adjusting is still in front of them when they are told there
//! is nothing to trace.
//!
//! Every rule about what a trace *is* — the threshold, the specks, the margin,
//! the fit — is `umber_core::trace`'s, with tests. This file is the drawing.

use egui::Ui;
use umber_core::PathRole;
use umber_core::trace::{MAX_MARGIN, SMOOTHING};

use crate::controls;
use crate::editor::{Editor, TraceSource};
use crate::tabs;
use crate::theme::{Palette, text};
use crate::widgets;

/// The sentence under the heading.
const ABOUT: &str = "Draws a path round each piece of artwork, standing off by the margin.";

const SOURCE_TIP: &str = "Everything traces all the layers in view. This layer traces the \
                          selected layer, or the contents of the selected folder.";

const SMOOTHING_TIP: &str =
    "How closely the path follows the edge. Lower follows more closely and uses more points.";

const KEEP_HOLES_TIP: &str = "Also trace the gaps inside a shape, such as the middle of a ring.";

/// The dialog's width, which is the export dialog's.
const WIDTH: f32 = 360.0;

/// The label a role is offered under, or `None` for one the dialog does not
/// offer.
///
/// **Exhaustive, so a fourth role has to be answered here.** Ink is a
/// brush-look stroke that nothing renders yet, and a trace is for a cutter or a
/// plotter; offering it would be a live control landing paths nobody can see.
fn offered(role: PathRole) -> Option<&'static str> {
    match role {
        PathRole::Cut => Some("Cut"),
        PathRole::Plot => Some("Plot"),
        PathRole::Ink => None,
    }
}

/// What the margin is on paper, or `None` where the document states no usable
/// resolution and a millimetre figure would be a division by nothing.
fn millimetres(margin: f32, dpi: f32) -> Option<String> {
    if !dpi.is_finite() || dpi <= 0.0 {
        return None;
    }
    let mm = margin / dpi * 25.4;
    Some(format!("{mm:.1} mm at {dpi:.0} dpi"))
}

/// Draw the trace dialog, if it is open.
pub fn show(root: &mut Ui, p: &Palette, ed: &mut Editor) {
    if !ed.ui.trace.open {
        return;
    }
    let dpi = ed.doc.dpi;
    let form = &mut ed.ui.trace;
    // A role the dialog does not offer would light neither half of the
    // switch. Nothing sets one today; this is what makes that structural.
    if offered(form.settings.role).is_none() {
        form.settings.role = PathRole::Cut;
    }
    let mut close = false;
    let mut trace = false;

    let modal = egui::Modal::new(egui::Id::new("trace-outline"))
        .frame(tabs::dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(WIDTH);
            heading(ui, p, "Trace outline");
            ui.add_space(4.0);
            caption(ui, p, ABOUT);
            ui.add_space(12.0);

            caption(ui, p, "Source").on_hover_text(SOURCE_TIP);
            ui.add_space(6.0);
            let sources = TraceSource::ALL.map(|s| (s, s.label()));
            widgets::segmented(ui, p, &mut form.source, &sources);
            ui.add_space(12.0);

            caption(ui, p, "Role");
            ui.add_space(6.0);
            // Built from `PathRole::ALL` through the exhaustive `offered`, so
            // a new role is a question this file has to answer rather than one
            // it quietly leaves out.
            let roles: Vec<(PathRole, &str)> = PathRole::ALL
                .into_iter()
                .filter_map(|r| offered(r).map(|label| (r, label)))
                .collect();
            widgets::segmented(ui, p, &mut form.settings.role, &roles);
            ui.add_space(12.0);

            widgets::typed_row(
                ui,
                p,
                &mut form.settings.margin,
                &widgets::Rail {
                    label: "Margin",
                    span: 0.0..=MAX_MARGIN,
                    limit: 0.0..=MAX_MARGIN,
                    log: false,
                    snap: 1.0,
                    deferred: false,
                    figure: widgets::Figure::new(1.0, " px", 0),
                },
            );
            // On paper, which is the unit a sticker's bleed is thought about
            // in. Read off the form every frame, so it follows a typed figure
            // and a drag alike.
            if let Some(line) = millimetres(form.settings.margin, dpi) {
                ui.add_space(4.0);
                caption(ui, p, &line);
            }
            ui.add_space(12.0);

            // The scope's response carries the tooltip over the label: the
            // rail and the field are interactive and keep their own.
            ui.scope(|ui| {
                widgets::typed_row(
                    ui,
                    p,
                    &mut form.settings.smoothing,
                    &widgets::Rail {
                        label: "Smoothing",
                        span: SMOOTHING,
                        limit: SMOOTHING,
                        log: true,
                        snap: 0.0,
                        deferred: false,
                        figure: widgets::Figure::new(1.0, " px", 1),
                    },
                );
            })
            .response
            .on_hover_text(SMOOTHING_TIP);
            ui.add_space(12.0);

            widgets::toggle_row(ui, p, "Keep holes", &mut form.settings.keep_holes)
                .on_hover_text(KEEP_HOLES_TIP);

            ui.add_space(16.0);
            // Inside a `horizontal`, for `canvasdlg`'s reason: a bare
            // right-to-left layout takes the whole of the remaining height and
            // floats the buttons in the middle of a stretched dialog.
            ui.horizontal(|ui| {
                if tabs::button(ui, p, "Cancel", false) {
                    close = true;
                }
                controls::button_row(ui, |ui| {
                    if tabs::button(ui, p, "Trace", true) {
                        trace = true;
                    }
                });
            });
        });

    // Escape and a click outside are Cancel: the form keeps what was set on
    // it, and nothing is traced. Trace does not close the dialog; see the
    // module docs.
    if close || modal.should_close() {
        ed.ui.trace.open = false;
    }
    if trace {
        ed.trace_requested = true;
    }
}

fn heading(ui: &mut Ui, p: &Palette, label: &str) {
    ui.label(
        egui::RichText::new(label)
            .size(text::CONTROL)
            .color(p.text_strong)
            .strong(),
    );
}

fn caption(ui: &mut Ui, p: &Palette, label: &str) -> egui::Response {
    ui.label(
        egui::RichText::new(label)
            .size(text::SMALL)
            .color(p.text_dim),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;
    use egui::{Pos2, Rect, pos2, vec2};

    /// What one pass drew: every galley with where it landed, and every
    /// rectangle the height of a dialog button.
    struct Shot {
        texts: Vec<(String, Rect)>,
        buttons: Vec<Rect>,
        /// Every `widgets::toggle` pill, which is 28 by 16.
        toggles: Vec<Rect>,
    }

    impl Shot {
        fn has(&self, label: &str) -> bool {
            self.texts.iter().any(|(t, _)| t == label)
        }

        /// The centre of the galley reading exactly `label`.
        fn at(&self, label: &str) -> Pos2 {
            self.texts
                .iter()
                .find(|(t, _)| t == label)
                .map(|(_, r)| r.center())
                .unwrap_or_else(|| panic!("nothing on screen reads {label:?}: {:?}", self.words()))
        }

        fn words(&self) -> Vec<&str> {
            self.texts.iter().map(|(t, _)| t.as_str()).collect()
        }
    }

    fn walk(shape: &egui::Shape, shot: &mut Shot) {
        match shape {
            egui::Shape::Text(t) => {
                let rect = t.galley.rect.translate(t.pos.to_vec2());
                shot.texts.push((t.galley.text().to_owned(), rect));
            }
            egui::Shape::Rect(r)
                if (r.rect.height() - tabs::BUTTON_HEIGHT).abs() < 0.5
                    && !shot.buttons.iter().any(|seen| {
                        seen.min.distance(r.rect.min) < 0.5 && seen.max.distance(r.rect.max) < 0.5
                    }) =>
            {
                // One rectangle per button: `tabs::button` paints a fill and a
                // stroke over the same rect, and without the dedup every gap
                // would read zero.
                shot.buttons.push(r.rect);
            }
            egui::Shape::Rect(r) if (r.rect.size() - vec2(28.0, 16.0)).length() < 0.5 => {
                shot.toggles.push(r.rect);
            }
            egui::Shape::Vec(inner) => {
                for shape in inner {
                    walk(shape, shot);
                }
            }
            _ => {}
        }
    }

    /// A context with Umber's own fonts and theme, so a button is as wide as
    /// it is in the application and the gaps measured are the ones drawn.
    struct Harness {
        ctx: egui::Context,
        palette: Palette,
        now: f64,
    }

    impl Harness {
        fn new() -> Self {
            let ctx = egui::Context::default();
            let palette = Palette::of(ThemeKind::Graphite);
            crate::theme::install_fonts(&ctx);
            crate::theme::apply(&ctx, &palette);
            Self {
                ctx,
                palette,
                now: 0.0,
            }
        }

        fn frame(&mut self, ed: &mut Editor, events: Vec<egui::Event>) -> Shot {
            self.now += 0.05;
            let output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 900.0))),
                    time: Some(self.now),
                    events,
                    ..Default::default()
                },
                |ui| show(ui, &self.palette, ed),
            );
            let mut shot = Shot {
                texts: Vec::new(),
                buttons: Vec::new(),
                toggles: Vec::new(),
            };
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut shot);
            }
            shot
        }

        /// Two passes and the second's picture: the first through a fresh
        /// context builds the font atlas, and a modal laid out against a
        /// half-built one has not settled.
        fn settled(&mut self, ed: &mut Editor) -> Shot {
            self.frame(ed, Vec::new());
            self.frame(ed, Vec::new())
        }

        /// Move, press and let go, a frame each.
        fn click(&mut self, ed: &mut Editor, at: Pos2) {
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            self.frame(ed, vec![egui::Event::PointerMoved(at)]);
            self.frame(ed, vec![button(true)]);
            self.frame(ed, vec![button(false)]);
        }

        fn press(&mut self, ed: &mut Editor, label: &str) {
            let shot = self.settled(ed);
            self.click(ed, shot.at(label));
        }
    }

    fn opened() -> Editor {
        let mut ed = Editor::default();
        ed.open_trace();
        ed
    }

    #[test]
    fn the_dialog_draws_every_control_it_names() {
        let mut ed = opened();
        let shot = Harness::new().settled(&mut ed);
        for label in [
            "Trace outline",
            ABOUT,
            "Everything",
            "This layer",
            "Cut",
            "Plot",
            "Margin",
            "Smoothing",
            "Keep holes",
            "Cancel",
            "Trace",
        ] {
            assert!(
                shot.has(label),
                "{label:?} is not on screen: {:?}",
                shot.words()
            );
        }
        // The source labels are `TraceSource`'s own, so a renamed one moves
        // this test with it rather than leaving it pinned to a stale word.
        for source in TraceSource::ALL {
            assert!(shot.has(source.label()), "{source:?}");
        }
    }

    #[test]
    fn a_closed_dialog_draws_nothing() {
        let mut ed = Editor::default();
        let shot = Harness::new().settled(&mut ed);
        assert!(!shot.has("Trace outline"), "{:?}", shot.words());
    }

    #[test]
    fn the_millimetre_line_follows_the_margin_and_the_resolution() {
        let mut h = Harness::new();
        let mut ed = opened();
        ed.doc.dpi = 300.0;
        ed.ui.trace.settings.margin = 35.0;
        let shot = h.settled(&mut ed);
        assert!(shot.has("3.0 mm at 300 dpi"), "{:?}", shot.words());

        // The line is read off the form every frame, so a new margin is a new
        // line and the old one is gone.
        ed.ui.trace.settings.margin = 70.0;
        let shot = h.settled(&mut ed);
        assert!(shot.has("5.9 mm at 300 dpi"), "{:?}", shot.words());
        assert!(!shot.has("3.0 mm at 300 dpi"), "{:?}", shot.words());

        // And the resolution is the document's: the same 70 px at 600 dpi is
        // half the paper.
        ed.doc.dpi = 600.0;
        let shot = h.settled(&mut ed);
        assert!(shot.has("3.0 mm at 600 dpi"), "{:?}", shot.words());

        // The line sits directly under the margin rail: below "Margin" and
        // above "Smoothing".
        let line = shot.at("3.0 mm at 600 dpi");
        assert!(shot.at("Margin").y < line.y && line.y < shot.at("Smoothing").y);
    }

    #[test]
    fn a_document_with_no_usable_resolution_draws_no_millimetre_line() {
        let mut h = Harness::new();
        for dpi in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let mut ed = opened();
            ed.doc.dpi = dpi;
            ed.ui.trace.settings.margin = 35.0;
            let shot = h.settled(&mut ed);
            assert!(
                !shot.texts.iter().any(|(t, _)| t.contains(" mm at ")),
                "{dpi}: {:?}",
                shot.words()
            );
            // The rest of the dialog is still there.
            assert!(shot.has("Margin"), "{dpi}");
        }
    }

    #[test]
    fn cancel_changes_nothing_and_trace_asks_once() {
        let mut h = Harness::new();
        let mut ed = opened();
        ed.ui.trace.settings.margin = 35.0;
        ed.ui.trace.settings.role = PathRole::Plot;
        ed.ui.trace.source = TraceSource::ThisLayer;
        let layers = ed.layers.len();
        let before = ed.ui.trace;

        h.press(&mut ed, "Cancel");
        assert!(!ed.ui.trace.open, "Cancel left the dialog open");
        assert!(!ed.trace_requested, "Cancel asked for a trace");
        assert_eq!(ed.layers.len(), layers);
        assert_eq!(ed.ui.trace.source, before.source);
        assert_eq!(ed.ui.trace.settings, before.settings);

        // Escape is Cancel too, and so must not ask either.
        ed.open_trace();
        h.settled(&mut ed);
        h.frame(
            &mut ed,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        assert!(!ed.ui.trace.open, "Escape left the dialog open");
        assert!(!ed.trace_requested, "Escape asked for a trace");
        assert_eq!(ed.ui.trace.settings, before.settings);

        // And so is a click outside it.
        ed.open_trace();
        h.settled(&mut ed);
        h.click(&mut ed, pos2(5.0, 5.0));
        assert!(!ed.ui.trace.open, "a click outside left the dialog open");
        assert!(!ed.trace_requested, "a click outside asked for a trace");

        // Trace asks, and leaves the dialog up: closing it is the caller's,
        // once the paths have landed.
        ed.open_trace();
        h.press(&mut ed, "Trace");
        assert!(ed.trace_requested, "Trace did not ask for a trace");
        assert!(ed.ui.trace.open, "Trace closed the dialog itself");
        assert_eq!(ed.layers.len(), layers, "the dialog did the work itself");

        // Once: the caller spends the request, and frames with no press do
        // not raise it again.
        ed.trace_requested = false;
        h.settled(&mut ed);
        assert!(!ed.trace_requested, "a frame with no press asked again");
    }

    #[test]
    fn the_role_and_source_controls_write_the_form() {
        let mut h = Harness::new();
        let mut ed = opened();
        h.press(&mut ed, "Plot");
        assert_eq!(ed.ui.trace.settings.role, PathRole::Plot);
        h.press(&mut ed, "Cut");
        assert_eq!(ed.ui.trace.settings.role, PathRole::Cut);
        h.press(&mut ed, "This layer");
        assert_eq!(ed.ui.trace.source, TraceSource::ThisLayer);
        // The pill, not the words: `toggle_row`'s label carries the tooltip
        // and the pill is the control.
        let holes = ed.ui.trace.settings.keep_holes;
        let shot = h.settled(&mut ed);
        assert_eq!(shot.toggles.len(), 1, "{:?}", shot.toggles);
        h.click(&mut ed, shot.toggles[0].center());
        assert_eq!(ed.ui.trace.settings.keep_holes, !holes);
        assert!(!ed.trace_requested);
    }

    /// No two buttons in the dialog touch, measured off the rectangles drawn —
    /// `no_two_buttons_in_the_library_browser_touch`'s method, because a guard
    /// that `button_row` sets the spacing would only agree with itself.
    #[test]
    fn no_two_buttons_in_the_trace_dialog_touch() {
        let mut ed = opened();
        let shot = Harness::new().settled(&mut ed);
        let mut buttons = shot.buttons.clone();
        assert_eq!(
            buttons.len(),
            2,
            "the dialog drew {} buttons where Cancel and Trace are two: {buttons:?}",
            buttons.len()
        );
        buttons.sort_by(|a, b| a.left().total_cmp(&b.left()));
        // Cancel first, then Trace, the order every other dialog keeps.
        assert!(buttons[0].contains(shot.at("Cancel")));
        assert!(buttons[1].contains(shot.at("Trace")));
        assert!(
            (buttons[0].top() - buttons[1].top()).abs() < 1.0,
            "one line"
        );
        let gap = buttons[1].left() - buttons[0].right();
        assert!(
            gap >= crate::theme::metrics::BUTTON_GAP - 0.5,
            "Cancel and Trace are {gap} points apart and read as one control"
        );
    }

    #[test]
    fn the_dialog_offers_no_ink_role() {
        let mut ed = opened();
        let shot = Harness::new().settled(&mut ed);
        assert!(shot.has("Cut") && shot.has("Plot"), "{:?}", shot.words());
        assert!(
            !shot.texts.iter().any(|(t, _)| t.contains("Ink")),
            "the dialog offers Ink: {:?}",
            shot.words()
        );
        // Every role is answered, and Ink is the one answered no.
        for role in PathRole::ALL {
            assert_eq!(offered(role).is_none(), role == PathRole::Ink, "{role:?}");
        }
    }

    #[test]
    fn the_millimetre_figure_is_stated_once() {
        assert_eq!(
            millimetres(35.0, 300.0).as_deref(),
            Some("3.0 mm at 300 dpi")
        );
        assert_eq!(millimetres(0.0, 72.0).as_deref(), Some("0.0 mm at 72 dpi"));
        assert_eq!(millimetres(35.0, 0.0), None);
        assert_eq!(millimetres(35.0, f32::NAN), None);
    }
}
