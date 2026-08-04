//! The Text module: set a line of text and place it on the canvas.
//!
//! `umber_core::text` is the model — shaping, layout, rasterisation — and
//! `umber_core::fonts` is where the faces come from. Neither draws anything.
//! This module holds what the artist has typed, finds the faces on a worker
//! thread, and paints the panel. Same division `dock.rs` keeps against
//! `panels.rs`.
//!
//! # Why the typing happens in a panel and not on the canvas
//!
//! Because a caret on the canvas is a much larger feature than it looks, and
//! this one is useful without it. Key dispatch happens at the **winit** level,
//! before egui sees a keystroke, so an on-canvas caret needs `shortcuts`
//! suspended for it, characters taken from `KeyEvent::text` rather than the
//! physical key, `Enter` and `Escape` prised away from the float commands they
//! already mean — and, the one that decides it, **an IME**. Umber never calls
//! `set_ime_allowed` and never handles `WindowEvent::Ime`, so a canvas caret
//! could not type Chinese, Japanese or Korean at all, and nobody working on
//! Umber can test that it does once it is added.
//!
//! A real `egui::TextEdit` gets all of that for nothing: the caret, the
//! selection, the IME and — since the clipboard feature went on for `sysclip` —
//! Ctrl+C, Ctrl+X and Ctrl+V out to the rest of the machine. Pasting a
//! paragraph in from somewhere else is most of what somebody wants a text panel
//! for, and here it already works.
//!
//! # Placing is a paste
//!
//! [`crate::app::UmberApp::place_text`] turns the coverage into a `Clip` and
//! hands it to the same two calls `paste` uses — `Clip::place` and
//! `begin_float` — then switches to the transform tool. So the box has handles,
//! Escape abandons it, a click outside puts it down, the undo entry is a
//! `Transform` like a paste's, and the preview is byte for byte what commits.
//! Nothing new reaches the GPU and nothing new reaches the file.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{Sense, Ui, vec2};

use umber_core::Background;
use umber_core::fonts::{self, Face, FontLibrary};
use umber_core::text::{self, Align, TextBlock, TextError};

use crate::controls;
use crate::editor::Editor;
use crate::icons::Icon;
use crate::theme::{Palette, metrics, text as texttokens};
use crate::ui::UiActions;
use crate::widgets::{self, DropdownWidth, NumberRow};

/// What the built-in face is registered as. A name rather than a path, because
/// it is the thing a preference would record and it must not be a file that is
/// not there.
pub const BUILTIN: &str = "archivo";

/// The em size the panel's preview is rasterised at.
///
/// Fixed rather than fitted, and the picture is scaled to the panel afterwards.
/// Fitting means rasterising once to measure and again to draw, on every
/// keystroke, and the second pass buys sharpness in a thumbnail nobody paints
/// with — what the artist is checking here is the face, the weight and the line
/// breaks, all of which survive a scale.
const PREVIEW_EM: f32 = 26.0;

/// The tallest the preview may be drawn, in points. Past this it is scaled
/// down: a panel is not where somebody reads six lines of a poem.
const PREVIEW_MAX_HEIGHT: f32 = 110.0;

/// Every face Umber can set text in, and how far along finding them is.
///
/// The built-in face is there from the first frame, so the panel is usable
/// before the scan lands and a machine whose scan finds nothing is still usable
/// afterwards. See `fonts`'s module docs for what the scan costs and why it is
/// therefore not on the drawing path.
pub struct Fonts {
    library: FontLibrary,
    /// The worker, while one is running. `None` before it starts and after it
    /// has landed.
    pending: Option<Receiver<FontLibrary>>,
    /// Whether a scan has been asked for. Separate from `pending` so a scan
    /// that finished is not started again every frame.
    started: bool,
}

impl Default for Fonts {
    fn default() -> Self {
        let mut library = FontLibrary::default();
        library.add_builtin(BUILTIN, crate::cputext::ARCHIVO);
        Self {
            library,
            pending: None,
            started: false,
        }
    }
}

impl Fonts {
    pub fn library(&self) -> &FontLibrary {
        &self.library
    }

    pub fn scanning(&self) -> bool {
        self.pending.is_some()
    }

    /// Look for the machine's fonts, once, on a worker thread.
    ///
    /// On a thread because a scan is several hundred file reads — see `fonts` —
    /// and this is called from a panel body, which is the drawing path. The
    /// panel asks for a repaint while one is in flight rather than the thread
    /// waking the loop: it is bounded, it ends, and it is only ever running
    /// while the Text module is on screen. The update check's `EventLoopProxy`
    /// wake exists because *that* answer may arrive with nothing on screen
    /// waiting for it.
    pub fn start(&mut self, folder: Option<PathBuf>) {
        if self.started {
            return;
        }
        self.started = true;
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("umber-fonts".to_string())
            .spawn(move || {
                let mut library = FontLibrary::default();
                library.add_builtin(BUILTIN, crate::cputext::ARCHIVO);
                // The user's own folder **first**, so their explicit choice
                // beats a copy of the same family in `/usr/share/fonts`:
                // `FontLibrary::insert` keeps the first of a duplicate, and
                // somebody who pointed Umber at a directory meant that
                // directory.
                let mut roots = Vec::new();
                roots.extend(folder);
                roots.extend(fonts::search_roots(&fonts::Probe::here()));
                library.scan(&roots);
                let _ = tx.send(library);
            })
            .is_ok();
        if spawned {
            self.pending = Some(rx);
        } else {
            // A machine that will not give us a thread keeps the built-in face
            // and says nothing. There is no useful second attempt, and a
            // painting application must not refuse to draw a panel over it.
            log::warn!("could not start the font scan; only the built-in face is available");
        }
    }

    /// Take the scan's answer if it has arrived. True when it just did.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = self.pending.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok(library) => {
                log::info!(
                    "found {} faces in {} font files ({} unreadable)",
                    library.faces().len(),
                    library.scanned,
                    library.unreadable
                );
                self.library = library;
                self.pending = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            // The worker died. The built-in face stands.
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                false
            }
        }
    }

    /// Throw the scan away so the next open of the panel looks again.
    ///
    /// What the folder preference calls when it changes: a library that still
    /// held the old folder's faces would offer faces the artist has just
    /// pointed Umber away from.
    pub fn forget(&mut self) {
        self.pending = None;
        self.started = false;
        let mut library = FontLibrary::default();
        library.add_builtin(BUILTIN, crate::cputext::ARCHIVO);
        self.library = library;
    }
}

/// What the artist has typed, and which face it is in.
///
/// Above the `--- documents ---` line in [`Editor`], and deliberately: a block
/// of text being composed belongs to the person rather than to the picture, in
/// the same way the brush in hand and the clipboard do. Switching tabs and
/// placing the same caption on a second document is the ordinary thing to want.
pub struct TextState {
    pub block: TextBlock,
    /// The family and style **by name**, never a `Face`. A `Face` holds a path,
    /// and the scan can replace the whole library underneath this — see
    /// `FontLibrary::resolve`, which is total for exactly this reason.
    pub family: String,
    pub style: String,
    pub fonts: Fonts,
    /// Filter for the family menu. Several hundred faces is a list nobody
    /// scrolls.
    pub search: String,
    /// The panel's picture of the block, and what it was made from.
    ///
    /// Keyed by everything that changes the picture **including the colour**,
    /// so a preview cannot outlive the thing it is a preview of. Single
    /// consumer — the panel body, drawn once per pass — which is the property
    /// that makes one cache entry safe; see `brushlib::tip_preview` and the
    /// rule in CLAUDE.md about a cache with two call sites.
    preview: Option<(u64, egui::TextureHandle)>,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            block: TextBlock::default(),
            family: "Archivo".to_string(),
            style: "Regular".to_string(),
            fonts: Fonts::default(),
            search: String::new(),
            preview: None,
        }
    }
}

impl TextState {
    /// The face the current family and style name, resolved against whatever
    /// the library holds now.
    pub fn face(&self) -> Option<&Face> {
        self.fonts.library().resolve(&self.family, &self.style)
    }

    /// Rasterise the block at its real size, ready to be placed.
    ///
    /// Blocking on a file read and on the rasteriser, which is what an explicit
    /// click may do. Nothing on the drawing path calls this — the preview has
    /// its own, smaller one.
    pub fn set(&self) -> Result<umber_core::text::Setting, TextError> {
        let face = self.face().ok_or(TextError::Unreadable)?;
        let data = face.load().ok_or(TextError::Unreadable)?;
        text::set(face, &data, &self.block)
    }
}

/// The Text panel body.
pub fn panel(ui: &mut Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    // The scan starts the first time somebody opens this module, not at
    // start-up: it is several hundred file reads for a feature most sessions
    // never reach.
    let folder = ed.font_folder.clone();
    ed.text.fonts.start(folder);
    if ed.text.fonts.poll() {
        ui.ctx().request_repaint();
    }
    if ed.text.fonts.scanning() {
        // Bounded, and only while this panel is on screen — the cost
        // `render`'s `repaint_at` exists to avoid is a *perpetual* request, and
        // this one ends when the worker does.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(150));
    }

    // A real `TextEdit`, for everything in the module docs: the caret, the
    // selection, the IME and the system clipboard, none of which is worth
    // reimplementing. `ui::draw` already calls `shortcuts::set_typing` from
    // `text_edit_focused`, so typing "brush" in here does not also pick the
    // brush.
    let edit = egui::TextEdit::multiline(&mut ed.text.block.text)
        .desired_rows(3)
        .desired_width(ui.available_width())
        .hint_text("Type something");
    ui.add(edit);
    ui.add_space(8.0);

    font_picker(ui, p, ed);
    ui.add_space(2.0);
    style_picker(ui, p, ed);
    ui.add_space(6.0);

    let mut block = ed.text.block.clone();
    widgets::number_row(
        ui,
        p,
        &mut block.size,
        NumberRow {
            label: "Size",
            range: text::MIN_SIZE..=text::MAX_SIZE,
            snap: 1.0,
            per_unit: 1.0,
            suffix: " px",
            decimals: 0,
            deferred: false,
        },
    );
    widgets::number_row(
        ui,
        p,
        &mut block.line_spacing,
        NumberRow {
            label: "Line spacing",
            range: 0.5..=3.0,
            snap: 0.05,
            per_unit: 100.0,
            suffix: "%",
            decimals: 0,
            deferred: false,
        },
    );
    widgets::number_row(
        ui,
        p,
        &mut block.tracking,
        NumberRow {
            label: "Tracking",
            range: -20.0..=40.0,
            snap: 0.5,
            per_unit: 1.0,
            suffix: " px",
            decimals: 1,
            deferred: false,
        },
    );
    ui.add_space(4.0);
    let mut align = block.align;
    widgets::segmented(
        ui,
        p,
        &mut align,
        &[
            (Align::Left, Align::Left.label()),
            (Align::Centre, Align::Centre.label()),
            (Align::Right, Align::Right.label()),
        ],
    );
    block.align = align;
    ed.text.block = block;

    ui.add_space(10.0);
    preview(ui, p, ed);

    ui.add_space(8.0);
    place_row(ui, p, ed, actions);
}

/// Which family. A dropdown, because there is one dropdown in this interface,
/// with a search field above the list — several hundred families is not a list
/// anybody scrolls.
fn font_picker(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let label = ed.text.family.clone();
    let count = ed.text.fonts.library().faces().len().to_string();
    let mut chosen: Option<String> = None;
    widgets::dropdown(
        ui,
        p,
        widgets::Dropdown::new(&label)
            .icon(Icon::Text)
            .trailing(&count)
            .width(DropdownWidth::Fill),
        |ui| {
            controls::search_field(ui, p, &mut ed.text.search, "Search fonts");
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(280.0)
                .show(ui, |ui| {
                    let query = ed.text.search.to_lowercase();
                    for family in ed.text.fonts.library().families() {
                        if !query.is_empty() && !family.to_lowercase().contains(&query) {
                            continue;
                        }
                        if ui
                            .selectable_label(family == ed.text.family, family)
                            .clicked()
                        {
                            chosen = Some(family.to_string());
                        }
                    }
                });
        },
    );
    if let Some(family) = chosen {
        ed.text.family = family;
        // The style is a name within a family, so it cannot survive the family
        // changing. Landing on whatever `resolve` picks — nearest to regular,
        // upright — keeps the panel showing the face it is actually going to
        // set rather than one it will substitute for silently.
        ed.text.style = ed
            .text
            .fonts
            .library()
            .resolve(&ed.text.family, "Regular")
            .map(|f| f.style.clone())
            .unwrap_or_else(|| "Regular".to_string());
    }
}

/// Which style within the family.
fn style_picker(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let label = ed.text.style.clone();
    let styles: Vec<String> = ed
        .text
        .fonts
        .library()
        .family(&ed.text.family)
        .iter()
        .map(|f| f.style.clone())
        .collect();
    let mut chosen = None;
    widgets::dropdown(
        ui,
        p,
        widgets::Dropdown::new(&label).width(DropdownWidth::Fill),
        |ui| {
            for style in &styles {
                if ui
                    .selectable_label(*style == ed.text.style, style)
                    .clicked()
                {
                    chosen = Some(style.clone());
                }
            }
        },
    );
    if let Some(style) = chosen {
        ed.text.style = style;
    }
}

/// A fingerprint of everything that changes the picture.
///
/// A hash rather than a stored copy of the block: the block holds a `String`
/// that can be a paragraph, and cloning it every frame to compare would be an
/// allocation on the drawing path.
fn preview_key(ed: &Editor) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let b = &ed.text.block;
    b.text.hash(&mut h);
    b.size.to_bits().hash(&mut h);
    b.line_spacing.to_bits().hash(&mut h);
    b.tracking.to_bits().hash(&mut h);
    b.align.hash(&mut h);
    ed.text.family.hash(&mut h);
    ed.text.style.hash(&mut h);
    ed.color.to_srgb_u8().hash(&mut h);
    h.finish()
}

/// What the block will look like, in the face it will be set in.
///
/// Rasterised at [`PREVIEW_EM`] rather than at the real size — a caption for a
/// 4000-pixel canvas is not a picture that fits in a 264-point panel — and
/// scaled to fit. The figure underneath is what says how big it really is.
fn preview(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    if ed.text.block.text.trim().is_empty() {
        controls::note(
            ui,
            p,
            "Type something above, then Place it on the canvas to move, scale \
             and turn it before it is put down.",
        );
        return;
    }

    let key = preview_key(ed);
    let stale = ed.text.preview.as_ref().map(|(k, _)| *k) != Some(key);
    // What the real block measures, which is what the caption reports. Taken
    // from the same call that makes the picture, so the two cannot disagree.
    let mut measured = None;
    let mut missing: Vec<char> = Vec::new();
    let mut mixed = false;

    if stale {
        let mut small = ed.text.block.clone();
        let ratio = PREVIEW_EM / ed.text.block.size.max(text::MIN_SIZE);
        small.size = PREVIEW_EM;
        small.tracking *= ratio;
        let made = ed
            .text
            .face()
            .and_then(|face| face.load().map(|data| (face.clone(), data)))
            .map(|(face, data)| text::set(&face, &data, &small));
        match made {
            Some(Ok(setting)) => {
                let [r, g, b, _] = ed.color.to_srgb_u8();
                let pixels: Vec<egui::Color32> = setting
                    .coverage
                    .iter()
                    .map(|&c| egui::Color32::from_rgba_unmultiplied(r, g, b, c))
                    .collect();
                let image = egui::ColorImage {
                    size: [setting.width as usize, setting.height as usize],
                    pixels,
                    source_size: vec2(setting.width as f32, setting.height as f32),
                };
                let handle =
                    ui.ctx()
                        .load_texture("text-preview", image, egui::TextureOptions::LINEAR);
                ed.text.preview = Some((key, handle));
            }
            _ => ed.text.preview = None,
        }
    }

    // The real block, for the figure and the notices. Measured rather than
    // guessed from the preview's ratio, because trimming to the ink is not
    // linear in the size.
    if let Ok(setting) = ed.text.set() {
        measured = Some((setting.width, setting.height));
        missing = setting.missing;
        mixed = setting.mixed_directions;
    }

    if let Some((_, handle)) = &ed.text.preview {
        let [w, h] = handle.size();
        let (w, h) = (w as f32, h as f32);
        let full = ui.available_width();
        let scale = (full / w).min(PREVIEW_MAX_HEIGHT / h).min(1.0);
        let size = vec2(w * scale, h * scale);
        // The plate is the panel's full width whatever the text measures, so a
        // long caption and a short one are the same control rather than a strip
        // that changes size as somebody types.
        let (behind, _) = ui.allocate_exact_size(vec2(full, size.y + 12.0), Sense::hover());
        plate(ui, p, behind, ed.doc.background);
        let rect = egui::Rect::from_center_size(behind.center(), size);
        ui.painter().image(
            handle.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        ui.add_space(4.0);
    }

    if let Some((w, h)) = measured {
        ui.label(
            egui::RichText::new(format!("{w} × {h} px on the canvas"))
                .size(texttokens::TINY)
                .color(p.text_dim),
        );
    }

    // Both of these are the import rule applied here: an operation that loses
    // something says so, in a finished sentence, rather than leaving it to be
    // discovered on the canvas.
    if !missing.is_empty() {
        // **By codepoint, never by drawing the characters.** The interface is
        // set in Archivo, so a character *this* face has no glyph for is one
        // Archivo very likely has none for either — printing it here would draw
        // the blank box the "no Unicode symbols in the UI" rule exists to
        // prevent, in the one sentence whose whole job is to say which
        // character is missing. `U+5B57` always renders and is what somebody
        // would search for.
        let list = missing
            .iter()
            .take(6)
            .map(|c| format!("U+{:04X}", *c as u32))
            .collect::<Vec<_>>()
            .join(", ");
        let more = if missing.len() > 6 {
            format!(" and {} more", missing.len() - 6)
        } else {
            String::new()
        };
        controls::note(
            ui,
            p,
            &format!(
                "This face has no glyph for {list}{more} — they are left blank \
                 rather than drawn as a box. Choose another font, or remove them."
            ),
        );
    }
    if mixed {
        controls::note(
            ui,
            p,
            "This line mixes left-to-right and right-to-left writing. Umber \
             does not reorder the two yet, so they may come out in the wrong \
             order.",
        );
    }
}

/// What the preview sits on: **this document's own background**.
///
/// Not the panel's fill and not a plate chosen to make the preview legible.
/// The question the preview answers is "what will this look like when I put it
/// down", and the honest answer to that is the paint over the canvas it is
/// going onto — so a white-backed document shows dark text on white, exactly
/// as it will land, and a transparent one shows the checker, which is what the
/// canvas itself draws and what a layer thumbnail draws.
///
/// The alternative — brightening the plate, or the ink, so that the preview
/// always reads — would be showing a colour nobody chose. The Colour panel is
/// where the colour is and where to change it.
///
/// The checker is drawn on the transparent case rather than a flat fill for the
/// reason the layer thumbnail gives: what is being previewed is pixels *with
/// alpha*, and a flat fill would say the text was opaque where it is
/// antialiased.
fn plate(ui: &Ui, p: &Palette, rect: egui::Rect, background: Background) {
    const CELL: f32 = 6.0;
    let painter = ui.painter();
    if let Background::Colour(colour) = background {
        let [r, g, b, _] = colour.to_srgb_u8();
        painter.rect_filled(rect, metrics::RADIUS, egui::Color32::from_rgb(r, g, b));
        return;
    }
    painter.rect_filled(rect, metrics::RADIUS, p.window);
    let cols = (rect.width() / CELL).ceil() as usize;
    let rows = (rect.height() / CELL).ceil() as usize;
    for i in 0..cols {
        for j in 0..rows {
            if (i + j) % 2 == 0 {
                continue;
            }
            let cell = egui::Rect::from_min_size(
                rect.left_top() + vec2(i as f32 * CELL, j as f32 * CELL),
                vec2(CELL, CELL),
            );
            painter.rect_filled(cell.intersect(rect), 0.0, p.control_hover);
        }
    }
}

/// The Place button, and why it might be refused.
///
/// Disabled rather than live-and-then-a-dialog where the answer is already
/// known — the rule "Clear layer" and the selection's Cut both follow. The lock
/// and the folder are still gated for real in `begin_float`, so a shortcut
/// cannot go round this.
fn place_row(ui: &mut Ui, p: &Palette, ed: &Editor, actions: &mut UiActions) {
    let locked = ed.layers.active_is_locked();
    let folder = ed.layers.active_slot().is_none();
    let empty = ed.text.block.text.trim().is_empty();
    let enabled = !locked && !folder && !empty;
    let tooltip = if locked {
        "The layer is locked. Unlock it in the Layers panel, or select another."
    } else if folder {
        "A folder is selected. A folder holds no pixels — select a layer."
    } else if empty {
        "Type something first."
    } else {
        "Put the text on the canvas, where the transform tool can move, scale \
         and turn it before it is committed"
    };
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let response = controls::text_button(ui, p, "Place", true, enabled);
            if response.clicked() {
                actions.place_text = true;
            }
            response.on_hover_text(tooltip);
        });
    });
    ui.add_space(2.0);
    controls::note(
        ui,
        p,
        "Text is painted into the layer when it is put down — it is pixels \
         afterwards, not something that can be re-typed.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    /// The panel is usable on the first frame and on a machine whose scan finds
    /// nothing: the interface's own face is in the library before any thread
    /// has run, and the default family and style name it.
    #[test]
    fn the_panel_can_set_text_before_any_scan_has_run() {
        let ed = Editor::default();
        assert!(!ed.text.fonts.scanning());
        assert!(!ed.text.fonts.library().is_empty());
        let face = ed.text.face().expect("a face before the scan");
        assert_eq!(face.family, ed.text.family);
    }

    /// What Place puts down is an ordinary clip in the artist's own colour with
    /// the coverage as its alpha — which is exactly what `float_a_clip` hands
    /// to `Clip::place` for a paste, and the whole of why nothing new reaches
    /// the GPU.
    #[test]
    fn what_place_would_put_down_is_an_ordinary_clip() {
        let mut ed = Editor::default();
        ed.text.block.text = "Umber".to_string();
        ed.text.block.size = 64.0;
        let setting = ed.text.set().expect("ink");
        let clip = setting.clip(ed.color).expect("a clip");
        assert_eq!(clip.size().x, setting.width);
        assert_eq!(clip.size().y, setting.height);
        assert!(clip.pixels().iter().skip(3).step_by(4).any(|&a| a > 200));
    }

    /// Nothing typed is refused rather than producing an empty float, and it is
    /// told apart from a line of spaces — the notice has different sentences
    /// for the two.
    #[test]
    fn an_empty_block_places_nothing() {
        let mut ed = Editor::default();
        assert_eq!(ed.text.set().err(), Some(TextError::Empty));
        ed.text.block.text = "   ".to_string();
        assert_eq!(ed.text.set().err(), Some(TextError::NoInk));
    }

    /// A family the machine does not have still resolves, because a preference
    /// records names and the machine it is read back on may have neither. The
    /// panel would otherwise be a picker that cannot draw anything until
    /// somebody works out why.
    #[test]
    fn a_face_that_is_not_here_still_sets_something() {
        let mut ed = Editor::default();
        ed.text.family = "A Foundry Face Nobody Has".to_string();
        ed.text.style = "Ultra Condensed Black Italic".to_string();
        ed.text.block.text = "Umber".to_string();
        assert!(ed.text.set().is_ok());
    }

    /// The preview is keyed by everything that changes the picture — including
    /// the colour, which is the one to forget, because it is not in the block.
    /// A key that missed one would draw a preview of a caption somebody had
    /// already changed, which is the control that lies at its smallest.
    #[test]
    fn the_preview_key_moves_with_everything_that_changes_the_picture() {
        let typed = |f: fn(&mut Editor)| {
            let mut ed = Editor::default();
            ed.text.block.text = "Umber".to_string();
            f(&mut ed);
            preview_key(&ed)
        };
        let base = typed(|_| {});
        assert_eq!(base, typed(|_| {}), "the key is not stable");

        for (what, f) in [
            (
                "text",
                (|ed: &mut Editor| ed.text.block.text.push('s')) as fn(&mut Editor),
            ),
            ("size", |ed| ed.text.block.size += 1.0),
            ("line spacing", |ed| ed.text.block.line_spacing += 0.1),
            ("tracking", |ed| ed.text.block.tracking += 1.0),
            ("align", |ed| ed.text.block.align = Align::Right),
            ("family", |ed| ed.text.family = "Other".to_string()),
            ("style", |ed| ed.text.style = "Bold".to_string()),
            ("colour", |ed| {
                ed.color = umber_core::Color::from_srgb_u8(200, 30, 30, 255)
            }),
        ] {
            assert_ne!(typed(f), base, "changing the {what} did not move the key");
        }
    }

    /// The Text module at the panel's real width, in the states it can be in.
    ///
    /// Written rather than asserted for the reason `layers_panel_preview` is:
    /// what goes wrong in a panel body is a *layout*, and no assertion about
    /// widgets catches two controls drawn over each other at `metrics::PANEL`'s
    /// real 264 points. This one has a preview image, three rails, two
    /// dropdowns and a segmented picker to fit into that width, which is
    /// exactly the shape that fits in the abstract and does not on screen.
    ///
    /// ```sh
    /// cargo test -p umber-app text_panel_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn text_panel_preview() {
        use crate::dock::{Layout, PanelKind};
        use crate::docshot;
        use crate::theme::metrics;
        use egui::{Pos2, Rect, vec2};

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/text-panel");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        // The fourth is the one worth looking at: two CJK ideographs Archivo
        // has no glyph for, so the notice that names them is on screen beside
        // the preview that does not show them.
        for (name, text, align) in [
            ("1-empty", "", Align::Left),
            ("2-a-caption", "Umber", Align::Left),
            (
                "3-several-lines",
                "Painted in Umber\non a Tuesday\nafternoon",
                Align::Centre,
            ),
            (
                "4-a-face-cannot-show-it",
                "Umber \u{5b57}\u{4f53}",
                Align::Left,
            ),
        ] {
            let mut ed = Editor::default();
            ed.layout = Layout::default();
            ed.text.block.text = text.to_string();
            ed.text.block.align = align;
            let palette = crate::theme::Palette::with_accent(ed.ui.theme, ed.ui.accent);
            let field = vec2(metrics::PANEL, 520.0);
            let rect = Rect::from_min_size(Pos2::ZERO, field);
            let image = stage.shoot(field, 2.0, &palette, palette.dock, |root| {
                let mut actions = UiActions::default();
                crate::panels::panel(root, &palette, &mut ed, &mut actions, PanelKind::Text, rect);
            });
            docshot::write_png(&dir.join(format!("{name}.png")), &image).expect("write the png");
        }
        println!("wrote 4 shots to {}", dir.display());
    }
}
