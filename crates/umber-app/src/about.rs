//! The two dialogs the update check needs.
//!
//! * [`first_run_notice`] — shown once, before the first check goes out, saying
//!   that Umber asks GitHub for the release list when it starts. The check is
//!   on by default (see [`crate::update::Updates::check_on_startup`] for why),
//!   and this is what makes that defensible: nothing leaves the machine until
//!   the user has read the sentence and answered it.
//! * [`update_prompt`] — raised by the automatic check when a newer release
//!   exists. It offers to install where that is legitimate, and **where it is
//!   not it still says so and points at the releases page**: an installation
//!   Umber may not replace is not a reason to leave somebody on an old build
//!   without telling them.
//!
//! Both use `tabs::dialog_frame` and `tabs::button`, so they are the same object
//! as the close prompt and the import notice rather than a second family of
//! dialogs.

use crate::editor::Editor;
use crate::tabs;
use crate::theme::{Palette, text};
use crate::update::{self, Status, Version};

/// Draw whichever of the two is due. Called once per frame from `ui::draw`.
pub fn show(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    first_run_notice(root, p, ed);
    update_prompt(root, p, ed);
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
                 or your work — no document, no identifier, not even a count of \
                 how often you run it.",
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
// A newer release
// ---------------------------------------------------------------------------

/// The prompt the automatic check raises.
fn update_prompt(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    if !ed.updates.prompt_open {
        return;
    }
    let Status::Available(release) = ed.updates.status().clone() else {
        ed.updates.prompt_open = false;
        return;
    };
    let installable = ed.updates.installable(&release).is_some();
    let obstacle = ed.updates.kind().cannot_update();
    let mut install = false;
    let mut open_page = false;
    let mut dismiss = false;

    let modal = egui::Modal::new(egui::Id::new("update-available"))
        .frame(tabs::dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(460.0);
            heading(ui, p, &format!("Umber {} is available", release.version));
            ui.add_space(4.0);
            note(ui, p, &format!("You are running {}.", Version::current()));

            if !release.notes.trim().is_empty() {
                ui.add_space(10.0);
                // The release notes are `CHANGELOG.md`'s own section, published
                // verbatim by the workflow — so this is the repository's text,
                // not a second wording of it.
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        body(ui, p, release.notes.trim());
                    });
            }

            // Where Umber may not do the update itself, the prompt still
            // appears and still says where the build is. Being unable to
            // install something is not a reason to leave somebody unaware that
            // it exists.
            if let Some(obstacle) = obstacle.as_deref() {
                ui.add_space(12.0);
                note(ui, p, obstacle);
            } else if !installable {
                ui.add_space(12.0);
                note(
                    ui,
                    p,
                    "This release does not carry a build for this machine. The \
                     releases page has everything that was published.",
                );
            }

            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if installable && obstacle.is_none() {
                    if tabs::button(ui, p, "Update now", true) {
                        install = true;
                    }
                } else if tabs::button(ui, p, "Open the releases page", true) {
                    open_page = true;
                }
                if tabs::button(ui, p, "Not now", false) {
                    dismiss = true;
                }
            });
        });

    if install {
        ed.updates.install_available();
    }
    if open_page {
        update::open_in_browser(&release.page);
        dismiss = true;
    }
    if dismiss || modal.should_close() {
        ed.updates.prompt_open = false;
    }
}

// ---------------------------------------------------------------------------
// Small pieces
// ---------------------------------------------------------------------------

fn heading(ui: &mut egui::Ui, p: &Palette, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .size(text::CONTROL)
            .color(p.text_strong)
            .strong(),
    );
}

fn body(ui: &mut egui::Ui, p: &Palette, message: &str) {
    ui.label(
        egui::RichText::new(message)
            .size(text::SMALL)
            .color(p.text)
            .line_height(Some(15.0)),
    );
}

fn note(ui: &mut egui::Ui, p: &Palette, message: &str) {
    ui.label(
        egui::RichText::new(message)
            .size(10.0)
            .color(p.text_dim)
            .line_height(Some(13.5)),
    );
}
