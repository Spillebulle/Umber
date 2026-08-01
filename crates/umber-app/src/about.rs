//! About, and the two dialogs the update check needs.
//!
//! Three modals, in the order a user meets them:
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
//! * [`dialog`] — Help, About. The mark, the version, the repository, the
//!   licence, how this copy was installed, and a check-for-updates button that
//!   reports its own outcome.
//!
//! All three use `tabs::dialog_frame` and `tabs::button`, so they are the same
//! object as the close prompt and the import notice rather than a second family
//! of dialogs.

use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::logo;
use crate::tabs;
use crate::theme::{Palette, text};
use crate::update::{self, Applied, Status, Version};
use egui::{Align2, FontId, Sense, vec2};

/// Draw whichever of the three is due. Called once per frame from `ui::draw`.
pub fn show(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    first_run_notice(root, p, ed);
    update_prompt(root, p, ed);
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

/// The update half of the About dialog: a button, whatever the last check said,
/// and the honest limit of what Umber can promise about a download.
fn update_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    let status = ed.updates.status().clone();
    let obstacle = ed.updates.kind().cannot_update();
    let mut check = false;
    let mut install = false;
    let mut quit = false;

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
            // A second check while one is running would start a second request
            // for the same answer, so the button goes dead for as long as it
            // takes — which on a slow connection is the only feedback there is.
            if tabs::button(ui, p, "Check now", false) && !ed.updates.busy() {
                check = true;
            }
        });
    });
    ui.add_space(8.0);

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
        Status::Downloading => note(ui, p, "Downloading…"),
        Status::Applied(Applied::Restart) => note(
            ui,
            p,
            "The new version is in place. It runs the next time you start Umber.",
        ),
        Status::Applied(Applied::Installer) => {
            note(
                ui,
                p,
                "The Windows installer is running. It needs Umber to close before \
                 it can replace the program.",
            );
            ui.add_space(6.0);
            if tabs::button(ui, p, "Close Umber", true) {
                quit = true;
            }
        }
        Status::Failed(message) => {
            note(ui, p, message);
        }
        Status::Available(release) => {
            ui.label(
                egui::RichText::new(format!("Umber {} is available.", release.version))
                    .size(text::SMALL)
                    .color(p.text_strong),
            );
            match obstacle.as_deref() {
                Some(obstacle) => {
                    ui.add_space(6.0);
                    note(ui, p, obstacle);
                }
                None if ed.updates.installable(release).is_none() => {
                    ui.add_space(6.0);
                    note(
                        ui,
                        p,
                        "That release carries no build for this machine. The \
                         releases page has everything that was published.",
                    );
                }
                None => {
                    ui.add_space(6.0);
                    if tabs::button(ui, p, "Download and install", true) {
                        install = true;
                    }
                }
            }
        }
    }

    ui.add_space(10.0);
    // Said here rather than left to be inferred. The download is fetched over
    // TLS from an address the release API gave, and checked against the length
    // that API reported — and that is the whole of it. Release signing is not
    // built; see CLAUDE.md.
    ui.label(
        egui::RichText::new(
            "Umber does not sign its releases. A download is fetched over HTTPS \
             from GitHub and checked against the size GitHub reports, which is \
             not the same as a signature.",
        )
        .size(9.5)
        .color(p.text_dim.gamma_multiply(0.85))
        .line_height(Some(12.5)),
    );

    if check {
        ed.updates.check();
    }
    if install {
        ed.updates.install_available();
    }
    if quit {
        ed.updates.request_quit();
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

/// A label and its value, on one line.
fn fact(ui: &mut egui::Ui, p: &Palette, label: &str, value: &str) {
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
fn rule(ui: &mut egui::Ui, p: &Palette) {
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, p.border);
}
