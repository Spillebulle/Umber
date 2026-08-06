//! About, and the notice the update check needs before its first request.
//!
//! Two modals, in the order a user meets them:
//!
//! * [`first_run_notice`] — shown once, before the first check goes out, saying
//!   that Umber asks GitHub for the release list when it starts. The check is
//!   on by default (see [`crate::update::Updates::check_on_startup`] for why),
//!   and this is what makes that defensible: nothing leaves the machine until
//!   the user has read the sentence and answered it.
//! * [`dialog`] — Help, About. The mark, the version, the repository, the
//!   licence, how this copy was installed, and a check-for-updates button that
//!   reports its own outcome.
//!
//! The third — the offer, and the work it leads to — is `updatedlg.rs`. It used
//! to live here as a one-screen prompt that started a download and then said
//! nothing until the installer appeared; it is now two screens with a state
//! machine behind it, which is more than this module should be about.
//!
//! All of them use `tabs::dialog_frame` and `tabs::button`, so they are the same
//! object as the close prompt and the import notice rather than a second family
//! of dialogs — which is also why the small text pieces at the foot of this file
//! are `pub(crate)`.

use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::logo;
use crate::tabs;
use crate::theme::{Palette, text};
use crate::update::{self, Status, Version};
use egui::{Align2, FontId, Sense, vec2};

/// Draw whichever of the two is due. Called once per frame from `ui::draw`.
pub fn show(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    first_run_notice(root, p, ed);
    dialog(root, p, ed);
}

// ---------------------------------------------------------------------------
// The first run
// ---------------------------------------------------------------------------

/// Say what the startup check does, once, before it has done it.
///
/// Deliberately answerable in one click either way, and deliberately not
/// dismissable by clicking outside: an unanswered notice would leave the check
/// switched on and unmentioned, which is the exact arrangement this exists to
/// avoid. Escape is left as "not now" — it comes back next start, and the check
/// stays held until it is answered.
fn first_run_notice(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    if ed.updates.notice_seen {
        return;
    }
    // Not while the splash's work is still landing: this is the first thing a
    // new user sees, and it should sit on a drawn workspace rather than on an
    // empty one.
    let mut answered: Option<bool> = None;

    egui::Modal::new(egui::Id::new("update-first-run"))
        .frame(tabs::dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(420.0);
            heading(ui, p, "Umber checks for new versions");
            ui.add_space(10.0);
            body(
                ui,
                p,
                "When Umber starts, it asks GitHub which release is newest and \
                 tells you if there is one. The request carries nothing about you \
                 or your work. No document, no identifier, not even a count \
                 of how often you run it.",
            );
            ui.add_space(8.0);
            body(
                ui,
                p,
                "You can change this at any time in Settings, General.",
            );
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if tabs::button(ui, p, "Check for updates", true) {
                    answered = Some(true);
                }
                if tabs::button(ui, p, "Don't check", false) {
                    answered = Some(false);
                }
            });
        });

    if let Some(wanted) = answered {
        ed.updates.notice_seen = true;
        ed.updates.check_on_startup = wanted;
        crate::prefs::mark_dirty();
    }
}

// ---------------------------------------------------------------------------
// About
// ---------------------------------------------------------------------------

/// The design has no About screen, so this follows the shape of the dialogs it
/// does have: the mark, a heading, rows of fact, and the actions on the right.
fn dialog(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    if !ed.ui.about_open {
        return;
    }
    let mut close = false;

    let modal = egui::Modal::new(egui::Id::new("about"))
        .frame(tabs::dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(430.0);

            ui.horizontal(|ui| {
                let (mark, _) = ui.allocate_exact_size(egui::Vec2::splat(44.0), Sense::hover());
                // The one place the mark's geometry is stated for an egui
                // painter, rather than a fifth `rect_filled` with its own
                // corner radius.
                logo::draw_mark(ui.painter(), mark, p);
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("Umber")
                            .size(text::HEADING)
                            .color(p.text_strong)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(format!("Version {}", Version::current()))
                            .size(text::SMALL)
                            .color(p.text_muted),
                    );
                });
            });

            ui.add_space(12.0);
            body(
                ui,
                p,
                "A GPU-accelerated painting application, written for the shortest \
                 possible path between a pen moving and pixels changing.",
            );

            ui.add_space(14.0);
            fact(ui, p, "Licence", "GPL-3.0-or-later");
            fact(ui, p, "Installed as", &ed.updates.kind().label());
            if link_row(ui, p, "Repository", "github.com/Spillebulle/umber") {
                update::open_in_browser(update::REPOSITORY);
            }

            ui.add_space(14.0);
            rule(ui, p);
            ui.add_space(12.0);
            update_section(ui, p, ed);

            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if tabs::button(ui, p, "Close", true) {
                    close = true;
                }
                if tabs::button(ui, p, "Releases", false) {
                    update::open_in_browser(update::RELEASES_PAGE);
                }
            });
        });

    if close || modal.should_close() {
        ed.ui.about_open = false;
    }
}

/// The honest limit of what Umber can promise about a download, said out loud.
///
/// Said here rather than left to be inferred. The download is fetched over TLS
/// from an address the release API gave, and checked against the length that
/// API reported, and that is the whole of it. Release signing is not built; see
/// CLAUDE.md.
///
/// **A `const` rather than a literal at the call site so a test can read it.**
/// `update::flow` and `update::installer` each fail the build when a stage or a
/// step label claims a check Umber does not perform; this is the third place
/// Umber speaks to the user about an update and it had no such guard. A
/// sentence that is correct with nothing holding it correct is one refactor
/// from being a security claim Umber cannot support — and unlike the other two,
/// what this paragraph must not lose is the *denial*.
const UNSIGNED_NOTE: &str = "Umber does not sign its releases. A download is fetched over HTTPS \
     from GitHub and checked against the size GitHub reports, which is not the same as a \
     signature.";

/// The update half of the About dialog: a button, whatever the last check said,
/// and the honest limit of what Umber can promise about a download.
///
/// It reports and it does not *do*. An update itself — the offer, the notes,
/// the bar, the countdown — is `updatedlg.rs`, and this hands over to it rather
/// than growing a second, smaller version of the same thing inside a section of
/// About.
fn update_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    let status = ed.updates.status().clone();
    // An installation that never asks GitHub at all — the Flatpak, whose
    // sandbox has no network and whose updates are Flatpak's own job.
    let unavailable = ed.updates.check_unavailable();
    let working = ed.updates.flow().and_then(|flow| match flow.phase() {
        crate::update::Phase::Working(stage) => Some(stage.label()),
        _ => None,
    });
    let mut check = false;
    let mut show = false;

    ui.horizontal(|ui| {
        let (mark, _) = ui.allocate_exact_size(egui::Vec2::splat(15.0), Sense::hover());
        icons::draw(ui.painter(), mark, Icon::Download, p.text_dim);
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Updates")
                .size(text::SMALL)
                .color(p.text_dim)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // No button where there is nothing behind it. A "Check now" that
            // could only ever time out is the live control that lies.
            if unavailable.is_none() {
                // A second check while one is running would start a second
                // request for the same answer, so the button goes dead for as
                // long as it takes — which on a slow connection is the only
                // feedback there is.
                if tabs::button(ui, p, "Check now", false) && !ed.updates.busy() {
                    check = true;
                }
            }
        });
    });
    ui.add_space(8.0);

    if let Some(reason) = unavailable {
        note(ui, p, reason);
        return;
    }

    // An update already under way outranks whatever the check last said: it is
    // the newer fact, and the dialog carrying it is on top of this one anyway.
    if let Some(stage) = working {
        note(ui, p, &format!("An update is in progress: {stage}."));
        return;
    }

    match &status {
        Status::Idle => note(
            ui,
            p,
            if ed.updates.check_on_startup {
                "Umber checks when it starts. Ask now if you would rather not wait."
            } else {
                "The check on start-up is switched off, in Settings, General."
            },
        ),
        Status::Checking => note(ui, p, "Asking GitHub…"),
        Status::UpToDate => note(
            ui,
            p,
            &format!("Umber {} is the newest release.", Version::current()),
        ),
        Status::Failed(message) => {
            note(ui, p, message);
        }
        Status::Available(release) => {
            ui.label(
                egui::RichText::new(format!("Umber {} is available.", release.version))
                    .size(text::SMALL)
                    .color(p.text_strong),
            );
            ui.add_space(6.0);
            // What that release carries, whether this copy may take it, and the
            // notes themselves are all the update dialog's — stated once, where
            // the buttons that act on them are.
            if tabs::button(ui, p, "Show the update…", true) {
                show = true;
            }
        }
    }

    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(UNSIGNED_NOTE)
            .size(9.5)
            .color(p.text_dim.gamma_multiply(0.85))
            .line_height(Some(12.5)),
    );

    if check {
        ed.updates.check();
    }
    if show {
        ed.updates.open_offer();
    }
}

// ---------------------------------------------------------------------------
// Small pieces
// ---------------------------------------------------------------------------

pub(crate) fn heading(ui: &mut egui::Ui, p: &Palette, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .size(text::CONTROL)
            .color(p.text_strong)
            .strong(),
    );
}

pub(crate) fn body(ui: &mut egui::Ui, p: &Palette, message: &str) {
    ui.label(
        egui::RichText::new(message)
            .size(text::SMALL)
            .color(p.text)
            .line_height(Some(15.0)),
    );
}

pub(crate) fn note(ui: &mut egui::Ui, p: &Palette, message: &str) {
    ui.label(
        egui::RichText::new(message)
            .size(10.0)
            .color(p.text_dim)
            .line_height(Some(13.5)),
    );
}

/// A label and its value, on one line.
pub(crate) fn fact(ui: &mut egui::Ui, p: &Palette, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.scope(|ui| {
            ui.set_width(90.0);
            ui.label(
                egui::RichText::new(label)
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
        });
        ui.label(
            egui::RichText::new(value)
                .size(text::SMALL)
                .color(p.text_strong),
        );
    });
}

/// A [`fact`] whose value opens somewhere outside Umber. Returns true on click.
///
/// Painted rather than an `egui::Hyperlink`: `egui-winit` is built without
/// default features, so its link handling is not compiled in and egui's own
/// hyperlink would look like a link and do nothing.
fn link_row(ui: &mut egui::Ui, p: &Palette, label: &str, value: &str) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.scope(|ui| {
            ui.set_width(90.0);
            ui.label(
                egui::RichText::new(label)
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
        });
        let font = FontId::proportional(text::SMALL);
        let width = ui
            .painter()
            .layout_no_wrap(value.to_owned(), font.clone(), p.accent)
            .size()
            .x;
        let (rect, response) = ui.allocate_exact_size(vec2(width + 18.0, 16.0), Sense::click());
        let painter = ui.painter();
        painter.text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            value,
            font,
            if response.hovered() {
                p.accent
            } else {
                p.text_strong
            },
        );
        icons::draw(
            painter,
            egui::Rect::from_center_size(
                egui::pos2(rect.right() - 7.0, rect.center().y),
                egui::Vec2::splat(11.0),
            ),
            Icon::Link,
            p.text_dim,
        );
        clicked = response.clicked();
    });
    clicked
}

/// The hairline the design puts between sections of a dialog.
pub(crate) fn rule(ui: &mut egui::Ui, p: &Palette) {
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, p.border);
}

#[cfg(test)]
mod tests {
    use super::UNSIGNED_NOTE;

    /// Umber does not sign its releases, and nothing it draws may imply
    /// otherwise. `update::flow::no_stage_calls_anything_verified` and
    /// `update::installer::no_step_calls_anything_verified` fail the build over
    /// it; About is the third place Umber speaks to the user about an update
    /// and it had no guard at all.
    ///
    /// **The word list here is deliberately not the other two's, and copying
    /// theirs would delete the disclaimer.** They ban "signed" and "signature"
    /// outright, which is right for what they cover: a stage label reading
    /// "Installing…" has no business mentioning signing, so any occurrence is a
    /// claim. This paragraph has to name a signature in order to *deny* one, so
    /// the same list would fail on the correct sentence. What is banned here is
    /// the vocabulary of a claim; what is required is the denial itself.
    ///
    /// That asymmetry is also the answer to whether the three should share one
    /// scanner: they should not. A shared helper would either force About to
    /// stop mentioning signatures or weaken the two that can afford the
    /// stricter rule.
    #[test]
    fn the_about_box_makes_no_claim_it_cannot_keep_and_keeps_its_denial() {
        let said = UNSIGNED_NOTE.to_lowercase();

        // A word a reader would take for a check Umber does not perform.
        for word in ["verif", "authentic", "secure"] {
            assert!(
                !said.contains(word),
                "the About box's update note says {word:?}: {UNSIGNED_NOTE:?}"
            );
        }

        // And the denial, which is the whole reason the paragraph exists.
        // Losing it is the likelier failure: trimmed to "fetched over HTTPS and
        // checked against the size GitHub reports", it states two facts and
        // lets the reader infer a third.
        assert!(
            said.contains("does not sign"),
            "the About box no longer denies that Umber signs its releases: {UNSIGNED_NOTE:?}"
        );
    }
}
