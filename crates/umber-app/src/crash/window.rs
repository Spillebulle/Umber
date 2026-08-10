//! The crash box: a window of its own, in a process of its own.
//!
//! This runs after Umber has died, started by [`super::spawn_reporter`] with
//! `--crash-report <path>`. It is the same executable, so it has `theme`,
//! `widgets`, `icons` and `tabs::dialog_frame` — the box is built out of the
//! same pieces as the settings dialog and the module library, and looks like
//! them because it *is* them. What it does not share is the device: this
//! process asks for its own adapter, its own surface and its own egui context,
//! which is the whole reason the reporter is a second process at all. See the
//! module docs on [`super`].
//!
//! There is deliberately **no canvas**. `CanvasRenderer` is never built, no
//! document is loaded, and nothing here touches `umber-core` beyond what the
//! report already holds — so the crash box cannot be stopped by the same thing
//! that stopped Umber.
//!
//! ### The "Copy details" button
//!
//! There used to be no such button, and the reason was real: `egui-winit` was
//! built with `default-features = false` and its `clipboard` feature was
//! therefore not compiled in, so `Context::copy_text` fell through to a
//! `String` held in the process — a button that would have copied the backtrace
//! into nothing an issue tracker could see. A control that lies is worse than
//! one that is not drawn.
//!
//! The feature is on now, because Umber's canvas clipboard needed the same
//! crate (`sysclip`), so `arboard` and the packaging declarations it was worth
//! avoiding for one button had to be paid for anyway. `Context::copy_text`
//! reaches the desktop, and this is exactly the window where somebody needs it:
//! the whole point of the report is that it goes into a bug report.
//!
//! What did not change is the rest of the route out. The report is still a
//! file, the box still names its path and offers to open the folder, and the
//! details are still a read-only `TextEdit` that can be selected by hand —
//! because this window runs after a crash, and the one control it offers for
//! getting the report out must not be the only one.
//!
//! `about::link_row` still paints its own hyperlink: that is the `links`
//! feature, which is a different one and is still off.

use crate::icons::{self, Icon};
use crate::prefs;
use crate::shell::{self, Page};
use crate::tabs;
use crate::theme::{Palette, metrics, text};
use egui::{Sense, vec2};
use std::path::{Path, PathBuf};

use super::report::Report;

/// The window, in logical points.
///
/// Fixed rather than derived from the content, for the reason
/// `metrics::BRUSH_LIBRARY` is fixed: the details block is a backtrace, whose
/// length is unbounded, and a window that grew to fit one would be taller than
/// the screen with its buttons out of reach. The details scroll instead.
const WINDOW: [f32; 2] = [560.0, 520.0];

/// What the footer takes: one row of buttons and the space above it.
const FOOTER: f32 = 36.0;

/// The least the scrolling body is ever given, however short the window is
/// dragged. Below this it stops being readable and the box may as well not have
/// opened.
const MIN_BODY: f32 = 120.0;

/// The drawing above the heading, as **coverage** rather than as a picture.
///
/// It is the author's own sad cat, and it is stored as one channel of ink
/// because a crash box is drawn in whichever of the six themes the artist uses.
/// The original is a black drawing on an opaque white card: shown literally
/// that card is a bright block in a dark theme, and "never hard-code a colour"
/// is the rule this project keeps everywhere else. So the *ink* is kept —
/// darkness times the source alpha, which drops the card and the rounded
/// surround both — and it is tinted with a palette token at draw time. In a
/// light theme that is the drawing as it was made; in a dark one it is the same
/// cat in chalk.
///
/// 256 px wide, from a 5000 px original, which is 11 KB against 3.2 MB and
/// still twice [`CAT_WIDTH`] so it stays crisp on a 2× display.
const CAT: &[u8] = include_bytes!("../../../../assets/crash-cat.png");

/// How wide the cat is drawn, in points. The height follows the picture.
const CAT_WIDTH: f32 = 128.0;

/// Show the report. Returns once the window has closed.
pub fn show(report: &Report, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut reporter = Reporter::new(report, path);
    shell::run(&mut reporter)?;
    if reporter.restart {
        super::restart();
    }
    Ok(())
}

/// The crash box as a [`Page`]. Everything below is what this window *says*;
/// the window itself is `shell`'s, shared with the update installer, because
/// two copies of a winit-and-wgpu host is exactly the drift this project
/// refuses where the blend maths is concerned.
struct Reporter<'a> {
    report: &'a Report,
    /// Built once. It is the same text the file holds and the same text the
    /// details block shows — assembled in `report.rs` so those cannot drift.
    details: String,
    report_path: PathBuf,
    palette: Palette,
    /// "Technical details", closed to begin with. A crash box that opens on a
    /// page of frame addresses buries the sentence about somebody's work.
    expanded: bool,
    /// Whether "Copy details" has been used, which is the whole of the
    /// confirmation. Latched rather than timed: a message that fades needs a
    /// repaint scheduled for a moment nothing else would ask for, and this
    /// window sits under `ControlFlow::Wait` so that a box being read costs
    /// nothing. A line that simply stays says the same thing and costs one
    /// frame — the one the click already caused.
    copied: bool,
    restart: bool,
    /// The cat, uploaded once.
    ///
    /// Held rather than loaded per frame: `load_texture` uploads, and a crash
    /// box sits open under `ControlFlow::Wait` for as long as somebody reads
    /// it. `None` where the asset would not decode, which draws nothing at all
    /// — a crash reporter that fails to open over its own decoration would be
    /// an absurd way to lose a report.
    cat: Option<egui::TextureHandle>,
}

impl<'a> Reporter<'a> {
    fn new(report: &'a Report, path: &Path) -> Self {
        // The user's own theme, read from their preferences file. A crash box
        // in the wrong theme is a small thing and an avoidable one — and this
        // process has no editor to take it from, which is exactly why prefs are
        // a file rather than a field.
        let prefs = prefs::load();
        Self {
            report,
            details: report.details(),
            report_path: path.to_path_buf(),
            // Through `themelib::resolve` rather than straight to
            // `Palette::with_accent`, so a theme the user made reaches the
            // crash box too. It is the same door `prefs::apply` goes through
            // and it reads the library only when one is named.
            palette: crate::themelib::resolve(
                prefs.theme,
                prefs.accent,
                prefs.custom_theme.as_deref(),
            ),
            expanded: false,
            copied: false,
            restart: false,
            cat: None,
        }
    }
}

impl Page for Reporter<'_> {
    fn title(&self) -> String {
        "Umber crash report".to_string()
    }

    fn size(&self) -> [f32; 2] {
        WINDOW
    }

    fn palette(&self) -> Palette {
        self.palette
    }

    fn draw(&mut self, ui: &mut egui::Ui, close: &mut bool) {
        // Uploaded on the first frame rather than in `new`, because there is no
        // context until the window exists.
        if self.cat.is_none() {
            self.cat = load_cat(ui.ctx());
        }
        body(
            ui,
            &self.palette,
            self.report,
            &self.details,
            &self.report_path,
            self.cat.as_ref(),
            Actions {
                expanded: &mut self.expanded,
                copied: &mut self.copied,
                restart: &mut self.restart,
                close,
            },
        );
    }
}

/// What the body may change. A struct rather than four `&mut` parameters
/// because a run of bare booleans in a signature is how the wrong one gets set.
struct Actions<'a> {
    expanded: &'a mut bool,
    copied: &'a mut bool,
    restart: &'a mut bool,
    close: &'a mut bool,
}

/// The box itself.
fn body(
    ui: &mut egui::Ui,
    p: &Palette,
    report: &Report,
    details: &str,
    report_path: &Path,
    cat_texture: Option<&egui::TextureHandle>,
    actions: Actions<'_>,
) {
    // The dialog frame every modal in the application uses, inset far enough
    // for the backdrop to show around it — so this reads as the same object as
    // the settings dialog and the module library rather than as a page of text
    // that happens to be in a window.
    let inset = egui::Frame::NONE.inner_margin(egui::Margin::same(10));
    inset.show(ui, |ui| {
        tabs::dialog_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(ui.available_height());

            heading(ui, p, report, cat_texture);
            ui.add_space(12.0);

            // The settings dialog's shape, and for its reason: a header, **one**
            // vertical `ScrollArea` claiming an explicit height, a footer. Two
            // things here are unbounded — the list of open documents and the
            // backtrace — and letting either size the window means a box whose
            // buttons are off the bottom of the screen. Expanding the details
            // used to do exactly that, which is what this replaces.
            //
            // The height is what is left after the footer, taken before the
            // body is drawn rather than after, so the buttons cannot be pushed
            // anywhere by what goes above them.
            let room = (ui.available_height() - FOOTER).max(MIN_BODY);
            egui::ScrollArea::vertical()
                .max_height(room)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // What happened, in plain words. The heading is the
                    // greeting; this is the part somebody has to act on.
                    paragraph(
                        ui,
                        p,
                        "Umber ran into a problem it could not carry on from and \
                         had to stop. Nothing about this has left your machine: \
                         the report below is a file on this computer and stays \
                         there unless you send it.",
                    );

                    ui.add_space(14.0);
                    work(ui, p, report);

                    ui.add_space(14.0);
                    technical(ui, p, details, actions.expanded, actions.copied);

                    ui.add_space(12.0);
                    where_the_report_is(ui, p, report_path);
                });

            // The footer, outside the scroll area, so the buttons stay where
            // they are however long the details run — a control that walks out
            // from under the pointer as the thing above it grows is how a Close
            // becomes a Restart.
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if tabs::button(ui, p, "Restart Umber", true) {
                    *actions.restart = true;
                    *actions.close = true;
                }
                if tabs::button(ui, p, "Close", false) {
                    *actions.close = true;
                }
            });
        });
    });
}

/// The mark, the heading and the one line of fact under it.
///
/// The heading is the application's own voice and is exactly what it says. The
/// line under it carries the version, because a crash box that does not say
/// which build broke is a crash box nobody can act on.
fn heading(
    ui: &mut egui::Ui,
    p: &Palette,
    report: &Report,
    cat_texture: Option<&egui::TextureHandle>,
) {
    // The mark and the words on the left, the drawing on the right, on one
    // row — so the box opens on what happened and what it is about at once,
    // and the picture is beside the sentence rather than above it pushing the
    // sentence down.
    ui.horizontal(|ui| {
        // The cat is placed first from the right, so the words take whatever is
        // left rather than the picture being squeezed by them. `right_to_left`
        // inside the row is how this file's own footer already does it.
        let cat_width = cat_texture.map_or(0.0, |_| CAT_WIDTH);
        let words = (ui.available_width() - cat_width - 12.0).max(80.0);

        ui.vertical(|ui| {
            ui.set_width(words);
            ui.horizontal(|ui| {
                let (mark, _) = ui.allocate_exact_size(egui::Vec2::splat(26.0), Sense::hover());
                icons::draw(ui.painter(), mark, Icon::Alert, p.warning);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("Oh no, oopsy")
                            .size(text::HEADING)
                            .color(p.text_strong)
                            .strong(),
                    );
                    let version = if report.version.is_empty() {
                        "Umber stopped unexpectedly".to_string()
                    } else {
                        format!("Umber {} stopped unexpectedly", report.version)
                    };
                    ui.label(
                        egui::RichText::new(version)
                            .size(text::SMALL)
                            .color(p.text_muted),
                    );
                });
            });
        });

        if let Some(texture) = cat_texture {
            cat(ui, p, texture);
        }
    });
}

/// Decode the cat into an egui texture, once.
///
/// The asset is one greyscale channel of coverage; egui wants RGBA, so the ink
/// becomes the alpha of a white image and the *colour* comes from the tint at
/// draw time. That is the whole reason it is stored this way — see [`CAT`].
fn load_cat(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(CAT));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    // The asset is written as `L8` and nothing else is expected; anything else
    // is a rebuild that changed it, and drawing nothing beats drawing noise.
    if info.color_type != png::ColorType::Grayscale {
        return None;
    }
    let pixels: Vec<egui::Color32> = buf[..info.buffer_size()]
        .iter()
        .map(|ink| egui::Color32::from_rgba_unmultiplied(255, 255, 255, *ink))
        .collect();
    let image = egui::ColorImage {
        size: [info.width as usize, info.height as usize],
        pixels,
        source_size: egui::Vec2::new(info.width as f32, info.height as f32),
    };
    Some(ctx.load_texture("crash-cat", image, egui::TextureOptions::LINEAR))
}

/// Draw the cat, centred, above the heading.
fn cat(ui: &mut egui::Ui, p: &Palette, texture: &egui::TextureHandle) {
    let size = texture.size_vec2();
    let height = CAT_WIDTH * size.y / size.x.max(1.0);
    // Centred by allocating the whole line and putting the picture in the
    // middle of it, rather than by a layout — the box is one column and this is
    // the only thing on its row.
    let (rect, _) = ui.allocate_exact_size(
        egui::Vec2::new(ui.available_width(), height),
        Sense::hover(),
    );
    let at = egui::Rect::from_center_size(rect.center(), egui::Vec2::new(CAT_WIDTH, height));
    // `text` rather than `text_strong`: it is a decoration beside a heading,
    // and the heading is what should be loudest. The tint is what makes the
    // coverage a colour at all.
    egui::Image::new(texture).tint(p.text).paint_at(ui, at);
}

/// What happened to the artist's work — the single most useful thing this box
/// can contain, and the one that must never overstate.
///
/// Every sentence comes from `report.rs`, where the rule about a copy being
/// complete is decided and tested. Nothing is phrased here.
fn work(ui: &mut egui::Ui, p: &Palette, report: &Report) {
    let rescued = report.rescued();
    let at_risk = report.at_risk();
    if rescued.is_empty() && at_risk.is_empty() {
        // Not silence, and not a claim either. "Nothing was open with unsaved
        // work" is a fact; "your work is safe" would be a promise about files
        // this process has not looked at.
        section(ui, p, "Your work");
        note(ui, p, "Nothing was open with unsaved changes.");
        return;
    }

    section(ui, p, "Your work");
    for rescue in &rescued {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(&rescue.title)
                .size(text::SMALL)
                .color(p.text_strong),
        );
        path_line(ui, p, &rescue.path);
        note(ui, p, &rescue.note());
    }
    for title in &at_risk {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(title)
                .size(text::SMALL)
                .color(p.text_strong),
        );
        note(ui, p, "Umber had no copy of this one.");
    }
}

/// The collapsed technical section.
///
/// The header is allocated unconditionally and hit-tested by geometry, which is
/// the rule "a widget revealed on hover must not be what decides the hover"
/// exists for — although here it is a plain click target, so the only thing at
/// stake is a chevron that does not flicker.
fn technical(
    ui: &mut egui::Ui,
    p: &Palette,
    details: &str,
    expanded: &mut bool,
    copied: &mut bool,
) {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), metrics::DROPDOWN),
        Sense::click(),
    );
    let painter = ui.painter();
    let chevron = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 7.0, rect.center().y),
        egui::Vec2::splat(12.0),
    );
    let ink = if response.hovered() {
        p.text_strong
    } else {
        p.text_muted
    };
    icons::draw(
        painter,
        chevron,
        if *expanded {
            Icon::ChevronUp
        } else {
            Icon::ChevronDown
        },
        ink,
    );
    painter.text(
        egui::pos2(rect.left() + 18.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Technical details",
        egui::FontId::proportional(text::SMALL),
        ink,
    );
    if response.clicked() {
        *expanded = !*expanded;
    }
    if !*expanded {
        return;
    }

    ui.add_space(6.0);
    // **Above the text, not below it.** The block underneath is a backtrace and
    // its length is bounded only by `report::BACKTRACE_LIMIT`, so a control
    // after it is one somebody has to scroll a page of frame addresses to
    // reach — the failure the brush editor's Edit mark is in the *header* to
    // avoid, and the one the footer twelve lines further down is outside the
    // scroll area to avoid. It copies exactly what is drawn below it:
    // `Report::details`, called once in `Reporter::new` and handed to both, so
    // what is pasted into an issue cannot differ from what the box showed.
    ui.horizontal(|ui| {
        if tabs::button(ui, p, "Copy details", false) {
            ui.ctx().copy_text(details.to_string());
            *copied = true;
        }
        if *copied {
            ui.add_space(8.0);
            // Says what is true rather than "Copied": on X11 and Wayland what
            // is copied is served by *this* process, so closing the box before
            // pasting can empty the clipboard again unless a clipboard manager
            // took it on the way out. That is every X11 application's story and
            // not something a button can fix — it is the other half of why the
            // path to the file is printed below and why the text stays
            // selectable.
            ui.label(
                egui::RichText::new("Copied. Paste it before closing this window.")
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
        }
    });
    ui.add_space(6.0);
    // Deliberately **no scroll area of its own**. This sits inside the body's
    // one `ScrollArea`, and a second one nested in it would make the wheel mean
    // two things depending on where the pointer happened to be — the rule the
    // settings dialog's panes live by. The text is bounded instead, by
    // `report::BACKTRACE_LIMIT` where it is captured, so "as tall as it needs"
    // has a ceiling.
    //
    // Read-only, and therefore genuinely selectable: `&str` is a `TextBuffer`
    // that reports itself immutable, so egui draws a real text field that
    // cannot be edited.
    let mut text = details;
    ui.add(
        egui::TextEdit::multiline(&mut text)
            .font(egui::FontId::monospace(text::TINY))
            .desired_width(f32::INFINITY)
            .text_color(p.text_muted),
    );
}

/// Where the file is, and the one control that gets it out of this window.
///
/// The path is on a line of its own, **wrapped explicitly**. A report path is
/// the longest string in this window — the data directory plus a timestamped
/// name — and an egui label defaults to `TextWrapMode::Extend`, which is what
/// put the brush browser wider than the screen. Beside a button in a
/// right-to-left row it does not widen the window, because the window is fixed;
/// it runs off the *left* edge instead and the start of the path is lost, which
/// is worse. `Label::wrap` is the fix, not `set_max_width`.
///
/// The button sits under it and to the left, with the dialog's own actions far
/// away at the bottom right: this one belongs to the path above it, not to the
/// question the box is asking.
fn where_the_report_is(ui: &mut egui::Ui, p: &Palette, path: &Path) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(format!("Saved to {}", path.display()))
                .size(9.5)
                .color(p.text_dim)
                .line_height(Some(12.5)),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    if let Some(dir) = path.parent()
        && tabs::button(ui, p, "Show the report", false)
    {
        // The same opener the settings dialog uses for the autosave directory.
        // Best effort by construction — there is no portable way to ask — which
        // is why the path is printed above it rather than only linked.
        if let Err(e) = crate::autosave::reveal(dir) {
            log::warn!("could not open {}: {e}", dir.display());
        }
    }
}

// ---------------------------------------------------------------------------
// Small pieces, following `about.rs`'s
// ---------------------------------------------------------------------------

fn section(ui: &mut egui::Ui, p: &Palette, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .size(text::SMALL)
            .color(p.text_dim)
            .strong(),
    );
}

fn paragraph(ui: &mut egui::Ui, p: &Palette, message: &str) {
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

/// A path, in the muted ink the rest of the interface gives one.
fn path_line(ui: &mut egui::Ui, p: &Palette, path: &str) {
    ui.label(
        egui::RichText::new(path)
            .size(10.0)
            .color(p.accent)
            .line_height(Some(13.5)),
    );
}
