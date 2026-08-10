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
///
/// The gap alone: the row's own height is [`tabs::BUTTON_HEIGHT`], and the
/// footer is *placed* at the bottom of the box rather than flowed after the
/// body, so there is no combined figure to keep in step with either.
const FOOTER_GAP: f32 = 10.0;

/// The least the scrolling body is ever given, however short the window is
/// dragged. Below this it stops being readable and the box may as well not have
/// opened.
const MIN_BODY: f32 = 120.0;

/// The drawing beside the heading, as **coverage** rather than as a picture.
///
/// It is the author's own sad cat, and it is stored as one channel of ink
/// because a crash box is drawn in whichever of the six themes the artist uses.
/// The original is a black drawing on an opaque white card: shown literally
/// that card is a bright block in a dark theme, and "never hard-code a colour"
/// is the rule this project keeps everywhere else. So the *ink* is kept —
/// darkness times the source alpha, which drops the card and the rounded
/// surround both — and it is tinted with a palette token at draw time. In a
/// light theme that is the drawing as it was made; in a dark one it is the same
/// cat in chalk. The token is `text_strong`, the heading's own ink, so the
/// drawing and the words read as one thing.
///
/// 256 px wide, from a 5000 px original, which is 11 KB against 3.2 MB and
/// still twice [`CAT_WIDTH`] so it stays crisp on a 2× display.
const CAT: &[u8] = include_bytes!("../../../../assets/crash-cat.png");

/// How wide the cat is drawn, in points. The height follows the picture.
const CAT_WIDTH: f32 = 128.0;

/// The warning mark's side, and the gap between it and the words.
///
/// Named because [`heading`] lays its row out by hand and has to measure the
/// block before it can place it; two literals in that arithmetic would be two
/// places for the mark's size to be stated.
const MARK: f32 = 26.0;
const MARK_GAP: f32 = 10.0;

/// The gap between the window's edge and the dialog box inside it.
///
/// The box is a `dialog_frame` floating on the window's own ground, exactly as
/// a modal floats on the interface, and this is what makes the ground visible
/// around it.
const OUTER_INSET: f32 = 10.0;

/// Space above the heading row.
///
/// The dialog frame's own inset puts the row hard under the top edge, which was
/// tight enough with one line of text and is tighter with a picture beside it.
const HEADING_TOP: f32 = 10.0;

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
    // **The window, not `available_height`.** The content has to be exactly
    // what is left once both insets are paid for, and neither reading from
    // inside the frames nor reading the root `Ui` gives that. A frame reserves
    // its top margin before its closure runs and its bottom margin after, so
    // the figure a caller sees in there is neither the whole nor the remainder;
    // and the root `Ui` `run_ui` hands over *grows with what the last pass
    // drew*, so deriving the height from it feeds back on itself — one pass
    // overflowed by three points and the next by five. `screen_rect` is the
    // window and is the same on every pass. (`viewport_rect` is egui 0.35's
    // name for it.)
    // `the_box_is_inset_equally_on_every_side` measures the four gaps.
    let room = ui.ctx().viewport_rect().height();
    let inset = egui::Frame::NONE.inner_margin(egui::Margin::same(OUTER_INSET as i8));
    inset.show(ui, |ui| {
        tabs::dialog_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Both insets and the hairline: the frame's stroke is painted
            // outside its content, so the box on screen is two points taller
            // than what is set here.
            let content =
                (room - 2.0 * OUTER_INSET - 2.0 * tabs::DIALOG_MARGIN - 2.0 * tabs::DIALOG_STROKE)
                    .max(MIN_BODY);
            ui.set_min_height(content);
            // **The cursor, not `min_rect`.** `set_min_height` inflates
            // `min_rect` on the spot, so measuring the heading against it gives
            // the whole reserved height instead — which made the body's
            // reservation negative, clamped it to `MIN_BODY`, and left the box
            // scrolling at half its height with the last two rows out of sight.
            // The cursor is where the next thing will actually go.
            let top = ui.cursor().top();

            heading(ui, p, cat_texture);
            ui.add_space(12.0);

            // The settings dialog's shape, and for its reason: a header, **one**
            // vertical `ScrollArea` claiming an explicit height, a footer. Two
            // things here are unbounded — the list of open documents and the
            // backtrace — and letting either size the window means a box whose
            // buttons are off the bottom of the screen. Expanding the details
            // used to do exactly that, which is what this replaces.
            //
            // What is left of `content` once the heading and the footer are
            // paid for. **Measured against the height that was set**, not
            // against `available_height`: a `set_min_height` is a floor rather
            // than a ceiling, so a scroll area sized from what egui thinks is
            // available grows the box past it — which is what left the bottom
            // inset five points short of the other three.
            // The footer is *placed*, at the bottom of the height that was
            // set, and the body gets what is above it. Reserving a figure and
            // letting the row flow after the scroll area cannot be made exact:
            // egui puts `item_spacing` between every pair of things, so the
            // reservation has to predict how many gaps there will be, and each
            // attempt at that arithmetic left the box a point or two past its
            // own inset. Nothing here predicts anything.
            let bottom = top + content;
            let footer_top = bottom - tabs::BUTTON_HEIGHT;
            let room = (footer_top - FOOTER_GAP - ui.cursor().top()).max(MIN_BODY);
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
            // Drawn into its own rectangle rather than flowed, which is what
            // makes the box exactly as tall as it said it would be. It
            // allocates nothing in the parent — ordinarily the thing to be
            // careful of, since a child taller than its slot paints over its
            // neighbours — and that is safe here precisely because the parent's
            // height was set first and the row is the last thing in it.
            let footer = egui::Rect::from_min_max(
                egui::pos2(ui.min_rect().left(), footer_top),
                egui::pos2(ui.min_rect().right(), bottom),
            );
            let mut footer_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(footer)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            if tabs::button(&mut footer_ui, p, "Restart Umber", true) {
                *actions.restart = true;
                *actions.close = true;
            }
            if tabs::button(&mut footer_ui, p, "Close", false) {
                *actions.close = true;
            }
        });
    });
}

/// Decode the cat into an egui texture, once.
///
/// The asset is one greyscale channel of coverage; egui wants RGBA, so the ink
/// becomes the alpha of a white image and the *colour* comes from the tint at
/// draw time. That is the whole reason it is stored that way — see [`CAT`].
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

/// The box's first row: the mark and the words, and the author's cat.
///
/// **Laid out by hand rather than as a row of widgets**, because what is wanted
/// is a *placement*: the words centred a third in from the left and the picture
/// a third in from the right, so the two read as one centred group with the gap
/// between them rather than as two things pushed against opposite edges. An
/// `ui.horizontal` can put things next to each other and cannot put them
/// anywhere in particular, and the measuring it would take to do so is most of
/// this function anyway.
///
/// Everything is measured before anything is drawn, which is also what lets the
/// words sit centred *against the picture*: their height is known, so the block
/// is placed against the taller of the two rather than hanging off its top.
///
/// **The version is deliberately not on this row**, and the box does not lose
/// it: `Report::details` writes `Umber <version>` as its first line, so it is
/// in the details block, in the report file and in whatever "Copy details"
/// puts on the clipboard. This row is the emotional beat; the facts are below
/// it and in the report.
fn heading(ui: &mut egui::Ui, p: &Palette, cat_texture: Option<&egui::TextureHandle>) {
    let title = ui.painter().layout_no_wrap(
        "Oh no, oopsy".to_owned(),
        egui::FontId::proportional(text::HEADING),
        p.text_strong,
    );
    // The mark and one line, so the block is as tall as the taller of the two
    // and both are centred on it.
    let block = egui::Vec2::new(MARK + MARK_GAP + title.size().x, title.size().y.max(MARK));
    let picture = cat_texture.map(|texture| {
        let size = texture.size_vec2();
        egui::Vec2::new(CAT_WIDTH, CAT_WIDTH * size.y / size.x.max(1.0))
    });

    // As tall as the taller of the two, so neither is clipped and both can be
    // centred against it.
    let height = picture.map_or(block.y, |cat| cat.y.max(block.y));
    ui.add_space(HEADING_TOP);
    let (row, _) = ui.allocate_exact_size(
        egui::Vec2::new(ui.available_width(), height),
        Sense::hover(),
    );

    // A third in from each end. With no picture there is nothing to balance
    // against, so the words simply take the middle.
    let (words_at, picture_at) = match picture {
        Some(_) => (
            row.left() + row.width() / 3.0,
            row.right() - row.width() / 3.0,
        ),
        None => (row.center().x, row.center().x),
    };

    let block_rect = egui::Rect::from_center_size(egui::pos2(words_at, row.center().y), block);
    // The mark is centred against the *words* rather than against the row, so a
    // long version string moving the block does not leave it behind.
    let mark = egui::Rect::from_min_size(
        egui::pos2(block_rect.left(), block_rect.center().y - MARK / 2.0),
        egui::Vec2::splat(MARK),
    );
    icons::draw(ui.painter(), mark, Icon::Alert, p.warning);

    // Centred against the mark rather than sat on its top edge, which is what
    // "centred with the triangle" means once there is only one line.
    let left = block_rect.left() + MARK + MARK_GAP;
    let top = block_rect.center().y - title.size().y / 2.0;
    ui.painter()
        .galley(egui::pos2(left, top), title, p.text_strong);

    if let (Some(texture), Some(size)) = (cat_texture, picture) {
        let at = egui::Rect::from_center_size(egui::pos2(picture_at, row.center().y), size);
        // **The heading's own ink.** The coverage carries no colour of its
        // own — that is the whole reason it is stored as ink — so this is what
        // decides it, and matching `text_strong` makes the drawing and the
        // words read as one thing rather than as a decoration beside a
        // sentence.
        egui::Image::new(texture)
            .tint(p.text_strong)
            .paint_at(ui, at);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay the box out headlessly and hand back every rectangle it filled.
    ///
    /// The window is a real one in the shipped build; here it is a context of
    /// the same size, which is enough to measure a layout — the same trick the
    /// canvas-dialog guards use to read what egui actually drew rather than
    /// restating the rule that drew it.
    fn frames_at(height: f32) -> (Vec<egui::Rect>, Vec<egui::Rect>) {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let report = Report::default();
        let mut reporter = Reporter::new(&report, Path::new("report.json"));
        let mut close = false;

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(WINDOW[0], height),
            )),
            ..Default::default()
        };
        // Twice, so the font atlas exists for the second pass and the galleys
        // are measured rather than guessed — the idiom `widgets`' own layout
        // tests keep.
        let mut rects = Vec::new();
        let mut clips: Vec<egui::Rect> = Vec::new();
        for _ in 0..2 {
            let output = ctx.run_ui(input.clone(), |ui| reporter.draw(ui, &mut close));
            rects = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Rect(rect) => Some(rect.rect),
                    _ => None,
                })
                .collect();
            clips = output.shapes.iter().map(|c| c.clip_rect).collect();
        }
        (rects, clips)
    }

    /// **The box's inset is the same on all four sides.**
    ///
    /// It was not: `dialog_frame` applies its inner margin *after* the content
    /// is measured, so `set_min_height(available_height())` ran the content to
    /// the bottom of the space and squeezed that margin off — full inset at the
    /// sides and the top, and only the outer one at the bottom. Reported from a
    /// running window, which is exactly the kind of thing a layout test can see
    /// and a person has to squint at.
    ///
    /// Measured against the window rather than asserted about the constants,
    /// because the constants were already right and the *layout* was not.
    #[test]
    fn the_box_is_inset_equally_on_every_side() {
        let height = WINDOW[1];
        let (rects, _) = frames_at(height);
        let dialog = rects
            .iter()
            .max_by(|a, b| a.area().total_cmp(&b.area()))
            .copied()
            .expect("the dialog frame is drawn");

        let left = dialog.left();
        let right = WINDOW[0] - dialog.right();
        let top = dialog.top();
        let bottom = height - dialog.bottom();

        // One point of tolerance: a frame's stroke is drawn on the boundary and
        // rounding puts an edge on either side of it.
        for (name, gap) in [("right", right), ("top", top), ("bottom", bottom)] {
            assert!(
                (gap - left).abs() <= 1.0,
                "the {name} inset is {gap:.1} against {left:.1} on the left \
                 (dialog {dialog:?} in {}x{height})",
                WINDOW[0],
            );
        }
    }

    /// **The body gets what is left of the box, not its floor.**
    ///
    /// `MIN_BODY` is the height below which the box may as well not have
    /// opened; it is a floor for a window dragged very short, and it is not
    /// what a full-size box should ever use. Sizing the body against
    /// `min_rect` did exactly that — `set_min_height` inflates `min_rect` on
    /// the spot, so the heading appeared to have used the whole box, the
    /// reservation went negative and clamped, and the box scrolled at half its
    /// height with its last two rows out of sight. Reported from a running
    /// window, again.
    ///
    /// Measured off the drawn rectangles: the picture is the heading row and
    /// the buttons are the footer, so what is between them is the body.
    #[test]
    fn the_scrolling_body_fills_what_is_left_of_the_box() {
        let (rects, clips) = frames_at(WINDOW[1]);
        // **The scroll area's own viewport**, read off the clip rectangle its
        // contents are drawn under. Measuring the gap between the heading and
        // the buttons instead proves nothing: the footer is *placed* at the
        // bottom of the box, so that distance is the same whether the body
        // fills it or is collapsed to its floor — a mutation putting the bug
        // back sailed through exactly that assertion.
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WINDOW[0], WINDOW[1]));
        let body = clips
            .iter()
            .filter(|c| c.height() < window.height() - 1.0 && c.width() > 100.0)
            .map(egui::Rect::height)
            .fold(0.0_f32, f32::max);
        let _ = rects;
        assert!(
            body > MIN_BODY * 2.0,
            "the scrolling body is {body:.0} points, which is about the              {MIN_BODY} floor rather than the room a {}x{} box leaves it",
            WINDOW[0],
            WINDOW[1],
        );
    }
}
