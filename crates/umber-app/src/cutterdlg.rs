//! Send to cutter: the form [`Editor::cutter`] holds, drawn as a modal.
//!
//! The shape is [`crate::tracedlg`]'s. The dialog *asks*
//! (`Editor::cutter_request`) and `App` does the work, because a job has to
//! settle a stroke, a float and a Path tool draft before the paths are read,
//! and only `App` can do that. What the form does own is the [`Worker`] once
//! one is running, and [`CutterForm::pump`] is how its events become the
//! stage line, the log and the outcome, so all of that is testable here with
//! a scripted machine and no window.
//!
//! **Nobody here has a Silhouette.** The log is what the artist who does will
//! copy and send back, which is why it records every transfer and why it has a
//! Copy log button above it rather than at the end of two thousand lines.
//!
//! Every rule about what a job *is* is `umber_core::cutjob`'s, and every rule
//! about the conversation is `crate::cutter`'s. This file is the drawing.

use egui::Ui;
use umber_core::PathRole;
use umber_core::cutjob::{self, Job, JobRefusal, JobSettings, Page};
use umber_core::document::Document;
use umber_core::layer::LayerStack;

use crate::controls;
use crate::cutter::{Event, Opener, Stage, Timing, Work, Worker};
use crate::editor::Editor;
use crate::tabs;
use crate::theme::{Palette, text};
use crate::widgets;

/// The dialog's width.
const WIDTH: f32 = 420.0;
/// The log's height, fixed so a growing log scrolls rather than growing the
/// dialog.
const LOG_HEIGHT: f32 = 160.0;
/// The most lines the log keeps. A long job writes several lines a block, and
/// what matters in a report is usually the end.
pub const MAX_LOG_LINES: usize = 2000;

/// Whether this platform has a link at all.
const SUPPORTED: bool = cfg!(windows);

const FIRST_TEST: &str = "For a first test, put a pen in the holder and use plain paper.";
const WINDOWS_ONLY: &str = "Sending to a cutter works on Windows only for now.";
const NO_MARKS: &str = "This document has no registration marks. Switch them on in the File menu.";
const MARKS_DO_NOT_FIT: &str = "This page cannot hold registration marks.";

/// What the dialog has asked `App` to do, spent once after the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutterRequest {
    Check,
    Send,
    Stop,
}

/// What the form is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutterPhase {
    Idle,
    Checking,
    Sending(Stage),
    /// A stop was asked for and the worker has not answered yet.
    Stopping,
}

/// What the summary line was worked out from. Anything that changes the plan
/// is in here, so a frame where none of it moved reuses the line.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SummaryKey {
    revision: u64,
    settings: JobSettings,
    size: glam::UVec2,
    dpi_bits: u32,
    marks: bool,
}

struct Summary {
    key: SummaryKey,
    line: String,
    refused: bool,
}

/// The Send to cutter form. Above the `--- documents ---` line on
/// [`Editor`]: it describes the machine on the desk, not a document, and a
/// job that is running belongs to no tab.
///
/// **One set of settings per role, for the session.** A blade and a pen want
/// different pressures, so switching role shows that role's own settings, as
/// the artist last left them, rather than resetting either.
pub struct CutterForm {
    pub open: bool,
    /// Cut or Plot. Never Ink: the dialog does not offer it.
    pub role: PathRole,
    pub cut: JobSettings,
    pub plot: JobSettings,
    pub phase: CutterPhase,
    pub log: Vec<String>,
    /// `log` joined, kept beside it so the read-only text field and Copy log
    /// do not join two thousand lines every frame.
    log_text: String,
    /// How the last piece of work ended: the line to show, or why it failed.
    pub outcome: Option<Result<String, String>>,
    /// The work in flight. Only one at a time.
    pub worker: Option<Worker>,
    summary: Option<Summary>,
    /// How many times the summary has been planned, which is what a test
    /// reads to say it is not planned every frame.
    planned: usize,
}

impl Default for CutterForm {
    fn default() -> Self {
        Self {
            open: false,
            role: PathRole::Cut,
            cut: JobSettings::new(PathRole::Cut),
            plot: JobSettings::new(PathRole::Plot),
            phase: CutterPhase::Idle,
            log: Vec::new(),
            log_text: String::new(),
            outcome: None,
            worker: None,
            summary: None,
            planned: 0,
        }
    }
}

/// The label a role is offered under, or `None` for one the dialog does not
/// offer. Exhaustive, so a fourth role has to be answered here.
fn offered(role: PathRole) -> Option<&'static str> {
    match role {
        PathRole::Cut => Some("Cut"),
        PathRole::Plot => Some("Plot"),
        PathRole::Ink => None,
    }
}

impl CutterForm {
    /// The settings of the role in front.
    pub fn settings(&self) -> JobSettings {
        match self.role {
            PathRole::Cut | PathRole::Ink => self.cut,
            PathRole::Plot => self.plot,
        }
    }

    fn settings_mut(&mut self) -> &mut JobSettings {
        match self.role {
            PathRole::Cut | PathRole::Ink => &mut self.cut,
            PathRole::Plot => &mut self.plot,
        }
    }

    /// The settings a job is planned with: marks only where this page can
    /// hold them, which is what the switch shows when it is disabled.
    fn effective(&self, page: &Page) -> JobSettings {
        let s = self.settings();
        JobSettings {
            role: self.role,
            use_marks: s.use_marks && page.marks.is_some(),
            ..s
        }
    }

    /// The job the dialog would send, planned exactly as `App` plans the one
    /// it does send.
    pub fn plan_job(&self, layers: &LayerStack, doc: &Document) -> Result<Job, JobRefusal> {
        let page = Page::of(doc);
        let settings = self.effective(&page);
        cutjob::plan(&cutjob::paths_of(layers, settings.role), &settings, &page)
    }

    /// Whether work is in flight. The dialog cannot be closed while it is.
    pub fn busy(&self) -> bool {
        let working = match self.phase {
            CutterPhase::Idle => false,
            CutterPhase::Checking | CutterPhase::Sending(_) | CutterPhase::Stopping => true,
        };
        working || self.worker.is_some()
    }

    /// The summary line and whether it is a refusal, planned again only when
    /// something it depends on has changed.
    fn summary(&mut self, layers: &LayerStack, doc: &Document) -> (&str, bool) {
        let key = SummaryKey {
            revision: layers.vector_revision(),
            settings: self.effective(&Page::of(doc)),
            size: doc.size,
            dpi_bits: doc.dpi.to_bits(),
            marks: doc.marks,
        };
        if self.summary.as_ref().is_none_or(|s| s.key != key) {
            self.planned += 1;
            let (line, refused) = match self.plan_job(layers, doc) {
                Ok(job) => (summary_line(&job, self.role), false),
                Err(why) => (why.sentence(), true),
            };
            self.summary = Some(Summary { key, line, refused });
        }
        let s = self.summary.as_ref().expect("planned above");
        (&s.line, s.refused)
    }

    /// Start a piece of work, unless one is already running.
    pub fn begin(
        &mut self,
        work: Work,
        open: Opener,
        timing: Timing,
        wake: impl Fn() + Send + 'static,
    ) {
        if self.busy() {
            return;
        }
        if self.log.is_empty() {
            self.push_line(format!("Umber {}", env!("CARGO_PKG_VERSION")));
        }
        self.phase = match &work {
            Work::Check => {
                self.push_line("Check connection".to_string());
                CutterPhase::Checking
            }
            Work::Send(job) => {
                let s = job.settings();
                self.push_line(format!(
                    "Send: {}, pressure {}, speed {}, passes {}, registration marks {}, {} paths, {:.1} mm",
                    offered(s.role).unwrap_or("Cut"),
                    s.pressure,
                    s.speed,
                    s.passes,
                    if s.use_marks { "on" } else { "off" },
                    job.path_count(),
                    job.length_mm()
                ));
                CutterPhase::Sending(Stage::Connecting)
            }
        };
        self.trim_log();
        self.outcome = None;
        self.worker = Some(Worker::start(work, open, timing, wake));
    }

    /// Ask a running job to stop. A check is three harmless commands and is
    /// left to finish.
    pub fn stop(&mut self) {
        match self.phase {
            CutterPhase::Sending(_) => {
                if let Some(worker) = &self.worker {
                    worker.stop();
                    self.phase = CutterPhase::Stopping;
                }
            }
            CutterPhase::Idle | CutterPhase::Checking | CutterPhase::Stopping => {}
        }
    }

    /// Collect what the worker has said since the last frame.
    pub fn pump(&mut self) {
        let Some(worker) = self.worker.as_mut() else {
            return;
        };
        let events = worker.poll();
        self.absorb(events);
    }

    /// Fold events into the form: lines into the log, a stage into the phase,
    /// and the final event into the outcome, after which the worker is gone.
    pub fn absorb(&mut self, events: Vec<Event>) {
        for event in events {
            match event {
                Event::Line(line) => self.push_line(line),
                Event::Stage(stage) => match self.phase {
                    // A stop is said until the worker answers it.
                    CutterPhase::Stopping | CutterPhase::Checking => {}
                    CutterPhase::Idle | CutterPhase::Sending(_) => {
                        self.phase = CutterPhase::Sending(stage);
                    }
                },
                Event::Checked(outcome) | Event::Sent(outcome) => {
                    self.push_line(match &outcome {
                        Ok(line) | Err(line) => line.clone(),
                    });
                    self.outcome = Some(outcome);
                    self.phase = CutterPhase::Idle;
                    self.worker = None;
                }
            }
        }
        self.trim_log();
    }

    fn push_line(&mut self, line: String) {
        self.log_text.push_str(&line);
        self.log_text.push('\n');
        self.log.push(line);
    }

    /// Drop the oldest lines past [`MAX_LOG_LINES`], once per batch.
    fn trim_log(&mut self) {
        if self.log.len() <= MAX_LOG_LINES {
            return;
        }
        let excess = self.log.len() - MAX_LOG_LINES;
        self.log.drain(..excess);
        self.log_text.clear();
        for line in &self.log {
            self.log_text.push_str(line);
            self.log_text.push('\n');
        }
    }

    /// The whole log as text, one line each.
    pub fn log_text(&self) -> &str {
        &self.log_text
    }
}

/// "3 paths, 412 mm of cutting".
fn summary_line(job: &Job, role: PathRole) -> String {
    let n = job.path_count();
    let paths = if n == 1 { "path" } else { "paths" };
    let doing = match role {
        PathRole::Cut | PathRole::Ink => "cutting",
        PathRole::Plot => "drawing",
    };
    format!("{n} {paths}, {:.0} mm of {doing}", job.length_mm())
}

/// The line naming what a running job is doing.
fn stage_line(phase: CutterPhase) -> Option<String> {
    match phase {
        CutterPhase::Idle => None,
        CutterPhase::Checking | CutterPhase::Sending(Stage::Connecting) => {
            Some("Looking for the cutter".to_string())
        }
        CutterPhase::Sending(Stage::FindingMarks) => {
            Some("Finding the registration marks".to_string())
        }
        CutterPhase::Sending(Stage::Cutting { block, of }) => {
            Some(format!("Cutting, block {} of {of}", block + 1))
        }
        CutterPhase::Sending(Stage::Finishing) => Some("Finishing".to_string()),
        CutterPhase::Stopping => Some("Stopping".to_string()),
    }
}

/// A rail for a whole number between `lo` and `hi`.
fn whole_row(ui: &mut Ui, p: &Palette, label: &str, value: &mut u8, lo: u8, hi: u8) {
    let span = f32::from(lo)..=f32::from(hi);
    let mut v = f32::from(*value);
    widgets::typed_row(
        ui,
        p,
        &mut v,
        &widgets::Rail {
            label,
            span: span.clone(),
            limit: span,
            log: false,
            snap: 1.0,
            deferred: false,
            figure: widgets::Figure::new(1.0, "", 0),
        },
    );
    *value = v.round().clamp(f32::from(lo), f32::from(hi)) as u8;
}

/// Draw the Send to cutter dialog, if it is open.
pub fn show(root: &mut Ui, p: &Palette, ed: &mut Editor) {
    if !ed.cutter.open {
        return;
    }
    if offered(ed.cutter.role).is_none() {
        ed.cutter.role = PathRole::Cut;
    }
    let page = Page::of(&ed.doc);
    let marks_fit = page.marks.is_some();
    let doc_marks = ed.doc.marks;
    let busy = ed.cutter.busy();
    let phase = ed.cutter.phase;
    let layers = &ed.layers;
    let doc = &ed.doc;
    let form = &mut ed.cutter;
    let mut close = false;
    let mut request = None;
    // Whether the summary is a refusal, which is what Send is drawn from.
    let mut refused = true;

    let modal = egui::Modal::new(egui::Id::new("send-to-cutter"))
        .frame(tabs::dialog_frame(p))
        .show(root.ctx(), |ui| {
            ui.set_width(WIDTH);
            heading(ui, p, "Send to cutter");
            ui.add_space(12.0);

            // Nothing on the form changes a job that is running, so it is
            // not offered as though it might.
            ui.add_enabled_ui(!busy, |ui| {
                caption(ui, p, "Role");
                ui.add_space(6.0);
                let roles: Vec<(PathRole, &str)> = PathRole::ALL
                    .into_iter()
                    .filter_map(|r| offered(r).map(|label| (r, label)))
                    .collect();
                widgets::segmented(ui, p, &mut form.role, &roles);
                ui.add_space(12.0);

                if marks_fit {
                    widgets::toggle_row(
                        ui,
                        p,
                        "Use registration marks",
                        &mut form.settings_mut().use_marks,
                    );
                } else {
                    // Shown off, and the artist's own choice is left as it was
                    // for the day the page can hold marks.
                    let mut off = false;
                    ui.add_enabled_ui(false, |ui| {
                        widgets::toggle_row(ui, p, "Use registration marks", &mut off);
                    });
                    ui.add_space(4.0);
                    caption(
                        ui,
                        p,
                        if doc_marks {
                            MARKS_DO_NOT_FIT
                        } else {
                            NO_MARKS
                        },
                    );
                }
                ui.add_space(12.0);

                let s = form.settings_mut();
                whole_row(ui, p, "Pressure", &mut s.pressure, 1, 33);
                ui.add_space(8.0);
                whole_row(ui, p, "Speed", &mut s.speed, 1, 10);
                ui.add_space(8.0);
                whole_row(ui, p, "Passes", &mut s.passes, 1, 5);
            });
            ui.add_space(12.0);

            // After the controls, so a role or a figure changed this frame is
            // summarised this frame rather than on the next one.
            let (summary, is_refusal) = form.summary(layers, doc);
            refused = is_refusal;
            line(ui, p, summary);
            ui.add_space(6.0);
            caption(ui, p, FIRST_TEST);
            if !SUPPORTED {
                ui.add_space(4.0);
                caption(ui, p, WINDOWS_ONLY);
            }
            ui.add_space(12.0);

            if let Some(stage) = stage_line(phase) {
                line(ui, p, &stage);
            } else if let Some(outcome) = &form.outcome {
                match outcome {
                    Ok(said) | Err(said) => line(ui, p, said),
                }
            }

            if !form.log.is_empty() {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if tabs::button(ui, p, "Copy log", false) {
                        ui.ctx().copy_text(form.log_text().to_owned());
                    }
                });
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .id_salt("cutter-log")
                    .max_height(LOG_HEIGHT)
                    .min_scrolled_height(LOG_HEIGHT)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        // A `&str` is a read-only buffer, so this is a real,
                        // selectable text field that cannot be edited.
                        let mut text = form.log_text();
                        ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .font(egui::FontId::monospace(text::TINY))
                                .desired_width(f32::INFINITY)
                                .text_color(p.text_muted),
                        );
                    });
            }

            ui.add_space(16.0);
            // Inside a `horizontal`, for `canvasdlg`'s reason.
            ui.horizontal(|ui| match phase {
                CutterPhase::Idle => {
                    if tabs::button(ui, p, "Close", false) {
                        close = true;
                    }
                    controls::button_row(ui, |ui| {
                        let send = ui
                            .add_enabled_ui(SUPPORTED && !refused, |ui| {
                                tabs::button(ui, p, "Send", true)
                            })
                            .inner;
                        if send {
                            request = Some(CutterRequest::Send);
                        }
                        let check = ui
                            .add_enabled_ui(SUPPORTED, |ui| {
                                tabs::button(ui, p, "Check connection", false)
                            })
                            .inner;
                        if check {
                            request = Some(CutterRequest::Check);
                        }
                    });
                }
                CutterPhase::Sending(_) => {
                    controls::button_row(ui, |ui| {
                        if tabs::button(ui, p, "Stop", false) {
                            request = Some(CutterRequest::Stop);
                        }
                    });
                }
                // A check is three harmless commands and a stop is already
                // asked: there is nothing to press but to wait.
                CutterPhase::Checking | CutterPhase::Stopping => {}
            });
        });

    // Escape, a click outside and Close all close the dialog, but never under
    // a running job: the dialog is the only thing on screen that can stop it.
    if (close || modal.should_close()) && !busy {
        ed.cutter.open = false;
    }
    if request.is_some() {
        ed.cutter_request = request;
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

fn line(ui: &mut Ui, p: &Palette, label: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(label)
                .size(text::SMALL)
                .color(p.text_strong),
        )
        .wrap(),
    );
}

fn caption(ui: &mut Ui, p: &Palette, label: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(label)
                .size(text::SMALL)
                .color(p.text_dim),
        )
        .wrap(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cutter::{Link, LinkError};
    use crate::theme::ThemeKind;
    use egui::{Pos2, Rect, pos2, vec2};
    use glam::Vec2;
    use umber_core::{Node, Path, VectorObject};

    struct Shot {
        texts: Vec<(String, Rect)>,
        buttons: Vec<Rect>,
        toggles: Vec<Rect>,
        /// Every rectangle the height of `widgets::progress_bar`.
        bars: Vec<Rect>,
        copied: Vec<String>,
    }

    impl Shot {
        fn has(&self, label: &str) -> bool {
            self.texts.iter().any(|(t, _)| t == label)
        }

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
                shot.buttons.push(r.rect);
            }
            egui::Shape::Rect(r) if (r.rect.size() - vec2(28.0, 16.0)).length() < 0.5 => {
                shot.toggles.push(r.rect);
            }
            egui::Shape::Rect(r)
                if (r.rect.height() - crate::theme::metrics::PROGRESS_BAR).abs() < 0.25
                    && r.rect.width() > 40.0 =>
            {
                shot.bars.push(r.rect);
            }
            egui::Shape::Vec(inner) => {
                for shape in inner {
                    walk(shape, shot);
                }
            }
            _ => {}
        }
    }

    fn shot_of(output: &egui::FullOutput) -> Shot {
        let mut shot = Shot {
            texts: Vec::new(),
            buttons: Vec::new(),
            toggles: Vec::new(),
            bars: Vec::new(),
            copied: Vec::new(),
        };
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut shot);
        }
        for command in &output.platform_output.commands {
            if let egui::OutputCommand::CopyText(t) = command {
                shot.copied.push(t.clone());
            }
        }
        shot
    }

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
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 1000.0))),
                    time: Some(self.now),
                    events,
                    ..Default::default()
                },
                |ui| show(ui, &self.palette, ed),
            );
            shot_of(&output)
        }

        fn settled(&mut self, ed: &mut Editor) -> Shot {
            self.frame(ed, Vec::new());
            self.frame(ed, Vec::new())
        }

        /// Move, press and let go, a frame each. Answers the last frame.
        fn click(&mut self, ed: &mut Editor, at: Pos2) -> Shot {
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            self.frame(ed, vec![egui::Event::PointerMoved(at)]);
            self.frame(ed, vec![button(true)]);
            self.frame(ed, vec![button(false)])
        }

        fn press(&mut self, ed: &mut Editor, label: &str) -> Shot {
            let shot = self.settled(ed);
            self.click(ed, shot.at(label))
        }
    }

    fn escape() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn square(role: PathRole, lo: f32, hi: f32) -> Path {
        Path {
            role,
            closed: true,
            nodes: [(lo, lo), (hi, lo), (hi, hi), (lo, hi)]
                .into_iter()
                .map(|(x, y)| Node::corner(Vec2::new(x, y)))
                .collect(),
            ink: None,
        }
    }

    /// A4 at 300 dpi with marks, and one cut and one plot square well inside
    /// them.
    fn opened() -> Editor {
        let mut ed = Editor::default();
        let mut doc = Document::new(2480, 3508).with_marks(true);
        doc.dpi = 300.0;
        ed.doc = doc;
        ed.layers
            .add_vector_layer(
                "paths",
                VectorObject::Paths(vec![
                    square(PathRole::Cut, 600.0, 1200.0),
                    square(PathRole::Plot, 700.0, 900.0),
                ]),
            )
            .unwrap();
        ed.open_cutter();
        ed
    }

    #[test]
    fn the_dialog_summarises_the_job_it_would_send() {
        let mut h = Harness::new();
        let mut ed = opened();
        let cut = ed.cutter.plan_job(&ed.layers, &ed.doc).unwrap();
        let shot = h.settled(&mut ed);
        let line = format!("1 path, {:.0} mm of cutting", cut.length_mm());
        assert!(shot.has(&line), "{line:?} not in {:?}", shot.words());
        for label in [
            "Send to cutter",
            "Cut",
            "Plot",
            "Use registration marks",
            "Pressure",
            "Speed",
            "Passes",
            FIRST_TEST,
            "Check connection",
            "Send",
            "Close",
        ] {
            assert!(shot.has(label), "{label:?}: {:?}", shot.words());
        }
        assert_eq!(shot.has(WINDOWS_ONLY), !SUPPORTED);
        // Planned once, however many frames it is drawn.
        assert_eq!(ed.cutter.planned, 1);

        // Plot is the other square and says drawing, with its own settings.
        let shot = h.press(&mut ed, "Plot");
        assert_eq!(ed.cutter.role, PathRole::Plot);
        let plot = ed.cutter.plan_job(&ed.layers, &ed.doc).unwrap();
        let line = format!("1 path, {:.0} mm of drawing", plot.length_mm());
        assert!(shot.has(&line), "{line:?} not in {:?}", shot.words());
        assert_ne!(cut.length_mm(), plot.length_mm());
        assert_eq!(ed.cutter.settings(), JobSettings::new(PathRole::Plot));

        // A changed setting on one role is kept for that role.
        ed.cutter.plot.pressure = 20;
        h.press(&mut ed, "Cut");
        assert_eq!(ed.cutter.settings(), JobSettings::new(PathRole::Cut));
        h.press(&mut ed, "Plot");
        assert_eq!(ed.cutter.settings().pressure, 20);

        // A new path is a new summary.
        let before = ed.cutter.planned;
        ed.layers
            .add_vector_layer(
                "more",
                VectorObject::Paths(vec![square(PathRole::Plot, 1300.0, 1400.0)]),
            )
            .unwrap();
        let shot = h.settled(&mut ed);
        assert!(
            shot.texts.iter().any(|(t, _)| t.starts_with("2 paths, ")),
            "{:?}",
            shot.words()
        );
        assert_eq!(ed.cutter.planned, before + 1);
    }

    #[test]
    fn a_refusal_is_shown_and_send_is_disabled() {
        let mut h = Harness::new();
        let mut ed = Editor::default();
        ed.open_cutter();
        let sentence = JobRefusal::NoPaths(PathRole::Cut).sentence();
        let shot = h.settled(&mut ed);
        assert!(shot.has(&sentence), "{:?}", shot.words());
        h.press(&mut ed, "Send");
        assert_eq!(ed.cutter_request, None, "a refused job could be sent");
        assert!(ed.cutter.worker.is_none());
    }

    #[test]
    fn marks_cannot_be_asked_for_on_a_document_without_them() {
        let mut h = Harness::new();
        let mut ed = opened();
        ed.doc.marks = false;
        assert!(ed.cutter.settings().use_marks, "the default asks for marks");
        let shot = h.settled(&mut ed);
        assert!(shot.has(NO_MARKS), "{:?}", shot.words());
        // Not the refusal a marked job would meet: the job is planned without.
        assert!(!shot.has(&JobRefusal::NoMarks.sentence()));
        assert!(
            shot.texts.iter().any(|(t, _)| t.starts_with("1 path, ")),
            "{:?}",
            shot.words()
        );
        // The pill is drawn and pressing it changes nothing.
        assert_eq!(shot.toggles.len(), 1, "{:?}", shot.toggles);
        let pill = shot.toggles[0].center();
        h.click(&mut ed, pill);
        assert!(ed.cutter.settings().use_marks);
        assert!(
            !ed.cutter
                .plan_job(&ed.layers, &ed.doc)
                .unwrap()
                .settings()
                .use_marks
        );

        // With marks the pill is live and the caption gone.
        ed.doc.marks = true;
        let shot = h.settled(&mut ed);
        assert!(!shot.has(NO_MARKS), "{:?}", shot.words());
        h.click(&mut ed, shot.toggles[0].center());
        assert!(!ed.cutter.settings().use_marks, "a live switch did nothing");
    }

    #[test]
    fn while_sending_only_stop_is_offered_and_no_bar_is_drawn() {
        // The detector can see a bar where there is one.
        let ctx = egui::Context::default();
        let p = Palette::of(ThemeKind::Graphite);
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            widgets::progress_bar(ui, &p, Some(0.5));
        });
        assert!(
            !shot_of(&output).bars.is_empty(),
            "the bar detector is blind"
        );

        let mut h = Harness::new();
        let mut ed = opened();
        ed.cutter.phase = CutterPhase::Sending(Stage::Cutting { block: 2, of: 12 });
        let shot = h.settled(&mut ed);
        assert!(shot.has("Cutting, block 3 of 12"), "{:?}", shot.words());
        assert_eq!(shot.buttons.len(), 1, "{:?}", shot.words());
        assert!(shot.buttons[0].contains(shot.at("Stop")));
        for gone in ["Send", "Check connection", "Close"] {
            assert!(!shot.has(gone), "{gone} offered while sending");
        }
        assert!(shot.bars.is_empty(), "a bar was drawn: {:?}", shot.bars);

        h.press(&mut ed, "Stop");
        assert_eq!(ed.cutter_request, Some(CutterRequest::Stop));

        // Neither a check nor a stop already asked offers anything.
        for phase in [CutterPhase::Checking, CutterPhase::Stopping] {
            ed.cutter.phase = phase;
            let shot = h.settled(&mut ed);
            assert!(shot.buttons.is_empty(), "{phase:?}: {:?}", shot.words());
            assert!(shot.bars.is_empty());
        }
        assert!(h.settled(&mut ed).has("Stopping"));
        ed.cutter.phase = CutterPhase::Checking;
        assert!(h.settled(&mut ed).has("Looking for the cutter"));
    }

    #[test]
    fn the_dialog_cannot_be_closed_while_a_job_runs() {
        let mut h = Harness::new();
        let mut ed = opened();
        ed.cutter.phase = CutterPhase::Sending(Stage::FindingMarks);
        h.settled(&mut ed);
        h.frame(&mut ed, vec![escape()]);
        assert!(ed.cutter.open, "Escape closed the dialog under a job");
        h.settled(&mut ed);
        h.click(&mut ed, pos2(5.0, 5.0));
        assert!(
            ed.cutter.open,
            "a click outside closed the dialog under a job"
        );

        // Once it is over, Escape closes it.
        ed.cutter.phase = CutterPhase::Idle;
        h.settled(&mut ed);
        h.frame(&mut ed, vec![escape()]);
        assert!(!ed.cutter.open);
        ed.open_cutter();
        h.press(&mut ed, "Close");
        assert!(!ed.cutter.open, "Close left the dialog open");
    }

    #[test]
    fn the_log_is_drawn_and_can_be_copied() {
        let mut h = Harness::new();
        let mut ed = opened();
        let shot = h.settled(&mut ed);
        assert!(
            !shot.has("Copy log"),
            "a log with nothing in it was offered"
        );
        ed.cutter.absorb(vec![
            Event::Line("> 2 \\x1b\\x04".to_string()),
            Event::Line("< 16 CAMEO V1.10    \\x03".to_string()),
            Event::Checked(Ok(
                "Connected. Firmware CAMEO V1.10. The cutter is ready.".to_string()
            )),
        ]);
        let shot = h.settled(&mut ed);
        assert!(
            shot.has("Connected. Firmware CAMEO V1.10. The cutter is ready."),
            "{:?}",
            shot.words()
        );
        let log = ed.cutter.log_text().to_owned();
        assert!(
            shot.texts
                .iter()
                .any(|(t, _)| t.contains("CAMEO V1.10    \\x03")),
            "the log text is not drawn: {:?}",
            shot.words()
        );
        let shot = h.press(&mut ed, "Copy log");
        assert_eq!(shot.copied, vec![log.clone()]);
        assert_eq!(
            log,
            "> 2 \\x1b\\x04\n< 16 CAMEO V1.10    \\x03\nConnected. Firmware CAMEO V1.10. \
             The cutter is ready.\n"
        );
    }

    #[test]
    fn the_log_keeps_the_newest_lines() {
        let mut form = CutterForm::default();
        form.absorb(
            (0..2500)
                .map(|i| Event::Line(format!("line {i}")))
                .collect(),
        );
        assert_eq!(form.log.len(), MAX_LOG_LINES);
        assert_eq!(form.log[0], "line 500");
        assert_eq!(form.log.last().unwrap(), "line 2499");
        assert_eq!(form.log_text(), form.log.join("\n") + "\n");
    }

    #[test]
    fn no_two_buttons_in_the_dialog_touch() {
        let mut ed = opened();
        let shot = Harness::new().settled(&mut ed);
        let mut buttons = shot.buttons.clone();
        assert_eq!(buttons.len(), 3, "{:?}", shot.words());
        buttons.sort_by(|a, b| a.left().total_cmp(&b.left()));
        assert!(buttons[0].contains(shot.at("Close")));
        assert!(buttons[1].contains(shot.at("Check connection")));
        assert!(buttons[2].contains(shot.at("Send")));
        for pair in buttons.windows(2) {
            assert!((pair[0].top() - pair[1].top()).abs() < 1.0, "one line");
            let gap = pair[1].left() - pair[0].right();
            assert!(
                gap >= crate::theme::metrics::BUTTON_GAP - 0.5,
                "two buttons are {gap} points apart"
            );
        }
    }

    #[test]
    fn pressing_send_asks_for_a_send_and_starts_nothing_itself() {
        let mut h = Harness::new();
        let mut ed = opened();
        h.press(&mut ed, "Send");
        if cfg!(windows) {
            assert_eq!(ed.cutter_request, Some(CutterRequest::Send));
        } else {
            assert_eq!(ed.cutter_request, None, "Send is live off Windows");
            assert!(h.settled(&mut ed).has(WINDOWS_ONLY));
        }
        assert!(ed.cutter.worker.is_none(), "the dialog started a worker");
        assert_eq!(ed.cutter.phase, CutterPhase::Idle);

        ed.cutter_request = None;
        h.press(&mut ed, "Check connection");
        if cfg!(windows) {
            assert_eq!(ed.cutter_request, Some(CutterRequest::Check));
        } else {
            assert_eq!(ed.cutter_request, None);
        }
        assert!(ed.cutter.worker.is_none());
    }

    /// The whole route from a started worker to an outcome on the form, with
    /// a machine that answers from a script and no window.
    #[test]
    fn a_check_runs_to_an_outcome_on_the_form() {
        struct Ready(Vec<Vec<u8>>);
        impl Link for Ready {
            fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
                match bytes {
                    b"FG\x03" => self.0.push(b"CAMEO V1.10\x03".to_vec()),
                    b"\x1b\x05" => self.0.push(b"0\x03".to_vec()),
                    _ => {}
                }
                Ok(())
            }
            fn read_reply(&mut self, _: std::time::Duration) -> Result<Vec<u8>, LinkError> {
                if self.0.is_empty() {
                    Err(LinkError::Timeout)
                } else {
                    Ok(self.0.remove(0))
                }
            }
            fn discard(&mut self, _: std::time::Duration) -> Result<Vec<u8>, LinkError> {
                Ok(self.0.drain(..).flatten().collect())
            }
        }
        let quick = Timing {
            poll: std::time::Duration::from_millis(1),
            ready: std::time::Duration::from_millis(50),
            marks: std::time::Duration::from_millis(5),
            reply: std::time::Duration::from_millis(5),
            settle: std::time::Duration::from_millis(1),
            drain: std::time::Duration::from_millis(1),
        };
        let mut form = CutterForm::default();
        form.begin(
            Work::Check,
            Box::new(|_log| Ok(Box::new(Ready(Vec::new())) as Box<dyn Link + Send>)),
            quick,
            || {},
        );
        assert_eq!(form.phase, CutterPhase::Checking);
        assert!(form.busy());
        // A second begin while one runs starts nothing.
        form.begin(
            Work::Check,
            Box::new(|_log| panic!("a second worker was started")),
            quick,
            || {},
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while form.worker.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "the check never ended"
            );
            form.pump();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(form.phase, CutterPhase::Idle);
        assert_eq!(
            form.outcome,
            Some(Ok(
                "Connected. Firmware CAMEO V1.10. The cutter is ready.".to_string()
            ))
        );
        assert!(form.log[0].starts_with("Umber "));
        assert_eq!(form.log[1], "Check connection");
        assert!(
            form.log.iter().any(|l| l == "> 3 FG\\x03"),
            "{:?}",
            form.log
        );
        assert!(!form.busy());
    }
}
