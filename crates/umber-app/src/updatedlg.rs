//! The update dialog: the offer, and the work it leads to.
//!
//! Two screens in one modal, which is `update::flow`'s [`Phase`] painted. There
//! is no drawing in the model and no decisions here — the same division
//! `panels.rs` keeps against `dock.rs`, and for the same reason: a state machine
//! with a socket at one end and a window at the other is otherwise testable at
//! neither.
//!
//! * **The offer.** Which release, what version this copy is on, and the
//!   release's own notes in a box of their own. Three actions: update now, not
//!   now, and never ask again — the last of which writes the *existing*
//!   preference, so Settings, General shows it and can undo it.
//! * **The work.** A bar in the splash's style, labelled with the stage the
//!   update is actually in, and then a completion screen with a five-second
//!   countdown, a "restart now" and a cancel.
//!
//! Three rules it is written to, all of them CLAUDE.md's:
//!
//! * **Nothing here says "verified".** Umber does not sign its releases. The
//!   footnote states exactly what was checked — HTTPS, an address from the API,
//!   and a length — and the stage that does the checking is called what it is.
//! * **"Update now" is never drawn on an installation Umber does not own.**
//!   `flow::actions` decides that, and a package manager's copy gets the
//!   manager's own command instead.
//! * **A release note is text nobody here wrote.** It goes in one vertical
//!   `ScrollArea` with `auto_shrink([false, false])` inside a box of a fixed
//!   size, and every label in a horizontal layout is explicitly wrapped —
//!   `TextWrapMode::Extend` is what once put the brush importer's notices wider
//!   than the screen.

use crate::about;
use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::tabs;
use crate::theme::{Palette, metrics, text};
use crate::update::{self, Applied, Exit, Flow, Phase, Version};
use crate::widgets;
use egui::{Sense, Stroke};
use std::time::{Duration, Instant};

/// What a click asked for. Collected while drawing and carried out afterwards,
/// because the modal's closure holds the editor and the actions need it too.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    /// Start the download.
    Install,
    /// Shut the dialog and leave the check as it is.
    NotNow,
    /// Shut it and switch the startup check off.
    Never,
    OpenPage,
    /// Stop a download in flight.
    Stop,
    /// Start over after a stop or a failure.
    Retry,
    /// Act on the countdown now.
    Finish,
    /// Stop the countdown and carry on painting.
    KeepRunning,
}

/// Draw the dialog if one is up. Called once per frame from `ui::draw`.
///
/// From there rather than from a panel body, exactly as the canvas dialogs and
/// the brush library's modals are: the layout can hide a panel, and a modal
/// owned by one that has gone cannot be shut.
pub fn show(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    let Some(flow) = ed.updates.flow().cloned() else {
        return;
    };
    // Clocked once for the whole frame, so the countdown's figure and the
    // decision to act on it cannot come from two different instants.
    let now = Instant::now();
    let actions = ed.updates.actions(&flow.release);
    let mut act: Option<Act> = None;

    let modal = egui::Modal::new(egui::Id::new("update"))
        .frame(tabs::dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(metrics::UPDATE_DIALOG_WIDTH);
            match flow.phase() {
                Phase::Offer => offer(ui, p, &flow, &actions, &mut act),
                Phase::Working(stage) => working(
                    ui,
                    p,
                    &flow,
                    stage.progress(),
                    stage.label(),
                    flow.can_stop(),
                    &mut act,
                ),
                // The stop has been asked for and the worker has not answered.
                // The bar keeps the reading it had rather than emptying — the
                // download is still running until it says otherwise — and the
                // line says what is being waited for.
                Phase::Stopping(stage) => working(
                    ui,
                    p,
                    &flow,
                    stage.progress(),
                    "stopping…".to_string(),
                    false,
                    &mut act,
                ),
                Phase::Stopped => stopped(ui, p, &mut act),
                Phase::Done { outcome, countdown } => {
                    done(ui, p, &flow, *outcome, *countdown, now, &mut act);
                }
                Phase::Failed(message) => failed(ui, p, message, &mut act),
            }
        });

    // Escape and a click outside are "not now" — but only where that is a
    // thing to be. A modal that vanished mid-download would leave a thread
    // running with nothing on screen to stop it.
    if modal.should_close() && !flow.holds_work() {
        act = act.or(Some(Act::NotNow));
    }

    match act {
        Some(Act::Install) => ed.updates.install_offered(),
        Some(Act::NotNow) => ed.updates.dismiss(),
        Some(Act::Never) => {
            ed.updates.never_ask_again();
            // The switch lives in the preferences file, like the one in
            // Settings, General that shows it.
            crate::prefs::mark_dirty();
        }
        Some(Act::OpenPage) => {
            // The release's own page, taken from the API reply, never built
            // here — and `open_in_browser` refuses anything that is not https.
            update::open_in_browser(&flow.release.page);
            ed.updates.dismiss();
        }
        Some(Act::Stop) => ed.updates.stop_update(),
        Some(Act::Retry) => ed.updates.retry(),
        Some(Act::Finish) => ed.updates.request_exit(exit_for(&flow)),
        Some(Act::KeepRunning) => ed.updates.cancel_countdown(),
        None => {}
    }

    // The countdown running out is the same act as the button under it, so it
    // goes through the same request.
    if act.is_none() && flow.due(now).is_some() {
        ed.updates.request_exit(exit_for(&flow));
    }
}

/// What the completion screen's button, and its countdown, actually do.
///
/// A copy Umber replaced itself can be started again from here. The Windows
/// installer cannot: it needs Umber *gone* before it may touch the files, and
/// it offers to start the new version itself when it has finished.
fn exit_for(flow: &Flow) -> Exit {
    match flow.phase() {
        Phase::Done {
            outcome: Applied::Restart,
            ..
        } => Exit::Restart,
        _ => Exit::Quit,
    }
}

// ---------------------------------------------------------------------------
// Screen one: the offer
// ---------------------------------------------------------------------------

fn offer(
    ui: &mut egui::Ui,
    p: &Palette,
    flow: &Flow,
    actions: &update::flow::Actions,
    act: &mut Option<Act>,
) {
    let release = &flow.release;
    title(ui, p, &format!("Umber {} is available", release.version));
    ui.add_space(12.0);

    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            // Sized off the dialog rather than left to the layout: the notes
            // box is fixed, so the column beside it has to be too, or a long
            // release title decides how wide the versions are.
            ui.set_width((ui.available_width() - metrics::UPDATE_NOTES[0] - 16.0).max(120.0));
            about::fact(ui, p, "Installed", &Version::current().to_string());
            about::fact(ui, p, "Available", &release.version.to_string());

            ui.add_space(10.0);
            // Where Umber may not do the update itself, the offer still appears
            // and still says where the build is: being unable to install
            // something is not a reason to leave somebody unaware that it
            // exists.
            if let Some(obstacle) = actions.obstacle.as_deref() {
                about::note(ui, p, obstacle);
            } else if actions.no_build {
                about::note(
                    ui,
                    p,
                    "This release carries no build for this machine. The releases \
                     page has everything that was published.",
                );
            }
        });
        ui.add_space(16.0);
        notes_box(ui, p, release.notes.trim());
    });

    ui.add_space(14.0);
    unsigned_footnote(ui, p);
    ui.add_space(12.0);
    actions_row(ui, |ui| {
        // Right to left, so the action that carries the dialog is drawn first
        // and lands rightmost.
        if actions.update_now {
            if tabs::button(ui, p, "Update now", true) {
                *act = Some(Act::Install);
            }
        } else if actions.open_page && tabs::button(ui, p, "Open the releases page", true) {
            *act = Some(Act::OpenPage);
        }
        if tabs::button(ui, p, "Not now", false) {
            *act = Some(Act::NotNow);
        }
        // Writes the preference Settings, General shows, rather than a second
        // switch of its own that could disagree with it.
        if tabs::button(ui, p, "Never ask again", false) {
            *act = Some(Act::Never);
        }
        ui.add_space(6.0);
        // Said beside the button rather than in a tooltip, because it is the
        // one button here with a consequence outside this dialog.
        ui.add(
            egui::Label::new(
                egui::RichText::new("Never ask again switches the start-up check off.")
                    .size(9.5)
                    .color(p.text_dim.gamma_multiply(0.85)),
            )
            .wrap(),
        );
    });
}

/// The release's own notes, in a box of their own.
///
/// `CHANGELOG.md`'s section for that version, published verbatim by the release
/// workflow and read back out of the API reply — so this is the repository's
/// text rather than a second wording of it, and it describes the build being
/// *offered*. The changelog compiled into this binary describes the build
/// already running and would be exactly the wrong thing to show.
fn notes_box(ui: &mut egui::Ui, p: &Palette, notes: &str) {
    egui::Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(metrics::UPDATE_NOTES[0]);
            ui.set_height(metrics::UPDATE_NOTES[1]);
            // Explicitly vertical. A `Frame` takes the layout of the `Ui` it is
            // shown in, and this one is shown inside the offer's two-column
            // `horizontal_top` — so without this the heading and the notes sit
            // side by side rather than one above the other.
            ui.vertical(|ui| {
                notes_body(ui, p, notes);
            });
        });
}

/// The box's contents: a heading and the notes under it.
fn notes_body(ui: &mut egui::Ui, p: &Palette, notes: &str) {
    ui.label(
        egui::RichText::new("Release notes")
            .size(text::TINY)
            .color(p.text_dim)
            .strong(),
    );
    ui.add_space(6.0);
    // One vertical scroll area, claiming its space whatever is in it. Sized by
    // the box rather than sizing it: notes are text nobody here wrote, and a
    // box that grew to fit them would take the modal and then the window with
    // it.
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("update-notes")
        .show(ui, |ui| {
            let text = if notes.is_empty() {
                "This release was published without notes. The releases page has \
                 what there is."
            } else {
                notes
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .size(text::SMALL)
                        .color(p.text)
                        .line_height(Some(15.5)),
                )
                .wrap(),
            );
        });
}

// ---------------------------------------------------------------------------
// Screen two: the work
// ---------------------------------------------------------------------------

/// The bar, the line under it, and a cancel while stopping costs nothing.
///
/// Handed a fraction and a line rather than a [`update::Stage`], because the
/// two do not always come from the same place: a stop waiting on the worker
/// keeps the *stage's* reading on the bar while the line says "stopping…".
fn working(
    ui: &mut egui::Ui,
    p: &Palette,
    flow: &Flow,
    progress: Option<f32>,
    line: String,
    can_stop: bool,
    act: &mut Option<Act>,
) {
    title(ui, p, &format!("Installing Umber {}", flow.release.version));
    ui.add_space(16.0);
    widgets::progress_bar(ui, p, progress);
    ui.add_space(10.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(line)
                .size(text::SMALL)
                .color(p.text_dim),
        )
        .wrap(),
    );

    ui.add_space(16.0);
    actions_row(ui, |ui| {
        // Only while stopping is free. From the unpack onwards a stop could
        // land mid-swap, and a half-replaced binary is the one outcome that
        // costs somebody their installation — so the control comes off the
        // screen rather than being drawn and refused.
        if can_stop && tabs::button(ui, p, "Cancel", false) {
            *act = Some(Act::Stop);
        }
    });
}

/// The completion screen, with the countdown.
fn done(
    ui: &mut egui::Ui,
    p: &Palette,
    flow: &Flow,
    outcome: Applied,
    countdown: update::flow::Countdown,
    now: Instant,
    act: &mut Option<Act>,
) {
    let (heading, waiting, settled, button) = match outcome {
        Applied::Restart => (
            format!("Umber {} is installed", flow.release.version),
            "Umber will restart in",
            "The new version is in place. It runs the next time you start Umber.",
            "Restart now",
        ),
        // Windows owns the installation from the moment `msiexec` has the
        // package. This is the honest limit of what Umber can report, and it is
        // said in words rather than drawn as a bar that knows nothing.
        Applied::Installer => (
            "The Windows installer is ready".to_string(),
            "Umber will close in",
            "The installer is open and waiting. It cannot replace Umber while \
             Umber is running, so close Umber when you are ready.",
            "Close Umber now",
        ),
    };
    title(ui, p, &heading);
    ui.add_space(10.0);

    match countdown.seconds_left(now) {
        Some(seconds) => {
            about::body(
                ui,
                p,
                &format!(
                    "{waiting} {seconds} second{}.{}",
                    if seconds == 1 { "" } else { "s" },
                    match outcome {
                        Applied::Restart => "",
                        Applied::Installer =>
                            " Windows shows its own progress from there, and offers \
                             to start the new version when it has finished.",
                    },
                ),
            );
            // A bar that empties as the wait runs down, from the same numbers
            // the figure above is read from — so the two cannot disagree.
            ui.add_space(12.0);
            let left = countdown.remaining(now).unwrap_or_default().as_secs_f32();
            widgets::progress_bar(
                ui,
                p,
                Some(left / update::flow::RESTART_DELAY.as_secs_f32()),
            );
            // Asked for at the next whole second rather than on a fixed timer:
            // the loop sleeps in `ControlFlow::Wait`, so a countdown nobody
            // asks to be redrawn simply stops moving.
            ui.ctx().request_repaint_after(next_tick(left));
        }
        // Cancelled. The update happened either way, and the screen says what
        // is true rather than what the countdown was going to do.
        None => about::body(ui, p, settled),
    }

    ui.add_space(16.0);
    actions_row(ui, |ui| {
        if countdown.running() {
            if tabs::button(ui, p, button, true) {
                *act = Some(Act::Finish);
            }
            if tabs::button(ui, p, "Cancel", false) {
                *act = Some(Act::KeepRunning);
            }
        } else {
            if tabs::button(ui, p, button, false) {
                *act = Some(Act::Finish);
            }
            if tabs::button(ui, p, "Close", true) {
                *act = Some(Act::NotNow);
            }
        }
    });
}

/// How long until the printed figure changes.
fn next_tick(seconds_left: f32) -> Duration {
    let fraction = seconds_left - seconds_left.floor();
    if fraction <= 0.001 {
        // Effectively on the boundary; a short delay rather than a whole second
        // so the last frame of the countdown is not skipped.
        Duration::from_millis(50)
    } else {
        Duration::from_secs_f32(fraction)
    }
}

/// The user stopped the download.
fn stopped(ui: &mut egui::Ui, p: &Palette, act: &mut Option<Act>) {
    title(ui, p, "The update was stopped");
    ui.add_space(10.0);
    about::body(
        ui,
        p,
        "Nothing was downloaded to disk and nothing was changed. This copy of \
         Umber is exactly as it was.",
    );
    ui.add_space(16.0);
    actions_row(ui, |ui| {
        if tabs::button(ui, p, "Close", true) {
            *act = Some(Act::NotNow);
        }
        if tabs::button(ui, p, "Try again", false) {
            *act = Some(Act::Retry);
        }
    });
}

/// It went wrong, in the sentence the worker wrote.
fn failed(ui: &mut egui::Ui, p: &Palette, message: &str, act: &mut Option<Act>) {
    title(ui, p, "The update did not finish");
    ui.add_space(10.0);
    // Wrapped, and it has to be: these carry an operating system's own error
    // text, which is the longest string in the interface and the one nobody
    // sized a window for.
    ui.add(
        egui::Label::new(
            egui::RichText::new(message)
                .size(text::SMALL)
                .color(p.text)
                .line_height(Some(15.0)),
        )
        .wrap(),
    );
    ui.add_space(16.0);
    actions_row(ui, |ui| {
        if tabs::button(ui, p, "Close", true) {
            *act = Some(Act::NotNow);
        }
        if tabs::button(ui, p, "Try again", false) {
            *act = Some(Act::Retry);
        }
        if tabs::button(ui, p, "Open the releases page", false) {
            *act = Some(Act::OpenPage);
        }
    });
}

// ---------------------------------------------------------------------------
// Small pieces
// ---------------------------------------------------------------------------

/// The strip of buttons at the foot of a screen, laid out right to left so the
/// action that carries the dialog lands rightmost.
///
/// Wrapped in a `horizontal`, which is the idiom `canvasdlg.rs` uses and is not
/// decoration: a bare `right_to_left` layout with a centred cross axis takes the
/// *whole* of the modal's remaining height, which on the short screens here
/// stretched the dialog to the height of the window and left the buttons
/// floating in the middle of it.
fn actions_row(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), body);
    });
}

/// The dialog's heading, with the mark every update screen carries.
fn title(ui: &mut egui::Ui, p: &Palette, line: &str) {
    ui.horizontal(|ui| {
        let (mark, _) = ui.allocate_exact_size(egui::Vec2::splat(16.0), Sense::hover());
        // Drawn, not a glyph: Archivo carries no symbols, so a Unicode arrow
        // would be a blank box.
        icons::draw(ui.painter(), mark, Icon::Download, p.accent);
        ui.add_space(6.0);
        about::heading(ui, p, line);
    });
}

/// What Umber can and cannot promise about a download.
///
/// Stated on the screen where the decision is made rather than left to About.
/// The words are chosen: "checked against the size GitHub reports" is true, and
/// "verified" would be a claim about a signature that does not exist.
fn unsigned_footnote(ui: &mut egui::Ui, p: &Palette) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "Umber does not sign its releases. The download is fetched over \
                 HTTPS from an address GitHub's API gave, and checked against \
                 the size GitHub reports. That is not the same as a signature.",
            )
            .size(9.5)
            .color(p.text_dim.gamma_multiply(0.85))
            .line_height(Some(12.5)),
        )
        .wrap(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::{Release, Stage};

    /// A release that does not exist, built here rather than borrowed from the
    /// debug-only helper: these tests run in either profile.
    fn release() -> Release {
        Release {
            version: Version::parse("9.9.9").expect("parses"),
            tag: "v9.9.9".into(),
            page: update::RELEASES_PAGE.into(),
            notes: String::new(),
            assets: Vec::new(),
        }
    }

    /// Write every screen of this dialog out as a PNG so it can be looked at.
    ///
    /// Ignored, and run by hand — the same arrangement `splash`'s
    /// `splash_preview` uses, and here it is the *only* way any of this is ever
    /// seen: a real update needs a release that is newer than this build, which
    /// means cutting one. It draws through `updatedlg::show` itself, offscreen,
    /// so what lands in the file is the interface rather than a picture of it.
    ///
    /// ```sh
    /// cargo test -p umber-app update_dialog_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn update_dialog_preview() {
        use crate::docshot;
        use crate::editor::Editor;

        use crate::update::{Applied, Phase, flow::Countdown};
        use egui::vec2;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/update-dlg");
        std::fs::create_dir_all(&dir).expect("create the preview directory");
        let now = Instant::now();

        let screens: [(&str, Phase); 8] = [
            ("1-offer", Phase::Offer),
            (
                "2-downloading",
                Phase::Working(Stage::Downloading {
                    received: 12 * 1024 * 1024,
                    total: 31 * 1024 * 1024,
                }),
            ),
            ("3-unpacking", Phase::Working(Stage::Unpacking)),
            ("4-handing-over", Phase::Working(Stage::HandingOver)),
            (
                "5-stopping",
                Phase::Stopping(Stage::Downloading {
                    received: 24 * 1024 * 1024,
                    total: 31 * 1024 * 1024,
                }),
            ),
            (
                "6-done-restart",
                Phase::Done {
                    outcome: Applied::Restart,
                    countdown: Countdown::stopped(),
                },
            ),
            (
                "7-done-installer",
                Phase::Done {
                    outcome: Applied::Installer,
                    countdown: Countdown::stopped(),
                },
            ),
            (
                "8-failed",
                Phase::Failed(
                    "Umber could not download umber-0.0.5-x64.msi: connection reset by \
                     peer.\n\nNothing was changed."
                        .to_string(),
                ),
            ),
        ];

        let count = screens.len();
        for (name, phase) in screens {
            let mut ed = Editor::default();
            ed.updates.demo(phase, now);
            let palette = ed.palette();
            let image = stage.shoot(
                // Wide enough that the modal sits in a margin of dimmed
                // backdrop rather than against the edge of the picture.
                vec2(metrics::UPDATE_DIALOG_WIDTH + 120.0, 460.0),
                1.5,
                &palette,
                palette.backdrop,
                |ui| super::show(ui, &palette, &mut ed),
            );
            docshot::write_png(&dir.join(format!("{name}.png")), &image).expect("write the png");
        }
        println!("wrote {count} screens to {}", dir.display());
    }

    #[test]
    fn the_countdowns_next_frame_lands_on_the_second_boundary() {
        // 4.37 s left reads "5"; it becomes "4" in 0.37 s, and that is when the
        // frame is worth asking for. Anything shorter is a redraw of the whole
        // interface for a figure that has not changed.
        assert!((next_tick(4.37).as_secs_f32() - 0.37).abs() < 0.001);
        assert!((next_tick(0.5).as_secs_f32() - 0.5).abs() < 0.001);
        // On the boundary itself, soon rather than a second away, so the last
        // frame of the countdown is not skipped.
        assert!(next_tick(2.0) <= Duration::from_millis(60));
        assert!(next_tick(0.0) <= Duration::from_millis(60));
    }

    #[test]
    fn the_completion_screens_button_matches_what_was_installed() {
        let t0 = Instant::now();
        let mut flow = Flow::offering(release());
        flow.begin();
        flow.stage(Stage::Installing);
        flow.finished(Applied::Restart, t0);
        assert_eq!(exit_for(&flow), Exit::Restart);

        let mut flow = Flow::offering(release());
        flow.begin();
        flow.stage(Stage::HandingOver);
        flow.finished(Applied::Installer, t0);
        assert_eq!(
            exit_for(&flow),
            Exit::Quit,
            "the Windows installer needs Umber gone, and starts the new build itself",
        );

        // And anywhere else, closing is the most that can be meant.
        let flow = Flow::offering(release());
        assert_eq!(exit_for(&flow), Exit::Quit);
    }
}
