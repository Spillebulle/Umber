//! The Windows installer's two bitmaps, drawn from the palette.
//!
//! WiX's stock dialog sets take exactly two pictures — a 493×58 banner across
//! the top of every working page, and a 493×312 field behind the welcome and
//! exit pages — and nothing else about their look is themeable without
//! hand-writing the whole dialog set. So this is the whole of "make the
//! installer look like Umber", and it is generated rather than drawn by hand
//! for `docshot`'s reason: an asset somebody exported once goes stale in
//! silence, and an installer is precisely where nobody looks for the drift.
//! Change `theme::Palette` and re-run the regenerator below.
//!
//! ### The one constraint that decides the layout
//!
//! MSI draws each dialog's title and description as **transparent text
//! controls over the bitmap**, in the system dialog colour, which is black.
//! There is no per-dialog way to change that: the text styles WixUI defines are
//! shared with pages whose background is plain white, so lightening them would
//! make those unreadable instead. A splash-dark bitmap under black text is
//! therefore not a style choice that is available.
//!
//! What is available is putting the dark where the text is not. The banner is
//! light where MSI writes and carries the splash's own brand group — mark,
//! gap, wordmark, on the theme's backdrop — in a block on the right; the
//! welcome and exit field is a graphite sidebar down the left, ending five
//! pixels short of where the first control begins. [`BANNER_SPLIT`] and
//! [`DIALOG_SIDEBAR`] are those measurements and the tests below check that the
//! committed pictures actually honour them, because the failure is an installer
//! nobody can read rather than one that looks wrong.
//!
//! Test-only, like the icon generator in `logo.rs`: the bytes are committed
//! under `packaging/windows/`, so nothing here reaches the shipped binary.

use crate::cputext::Font;
use crate::logo;
use crate::splash;
use crate::theme::Palette;
use egui::Color32;

/// The banner across the top of the licence and progress pages.
///
/// 493×58 is WiX's figure for `WixUIBannerBmp`: the control is 370×44 dialog
/// units and MSI's units come out at four thirds of a pixel here.
pub const BANNER: (u32, u32) = (493, 58);

/// The field behind the welcome and exit pages, `WixUIDialogBmp`. 370×234
/// dialog units by the same conversion.
pub const DIALOG: (u32, u32) = (493, 312);

/// Where the banner stops being light.
///
/// The banner is `ProgressDlg`'s alone in this dialog set, and that dialog's
/// transparent title is a control 330 dialog units wide starting at 20 — which
/// is 27..466 px, nearly the whole strip. The control is left-aligned and holds
/// "Installing [ProductName]", so the glyphs stop well short of a third of it;
/// 300 is where the picture may safely go dark. Stated as a judgement rather
/// than as a measurement of the control, because it is one: the failure if a
/// title ever did run that far is text over the mark, which is ugly, and the
/// sidebar's figure below is the one that would be unreadable.
const BANNER_SPLIT: u32 = 300;

/// Where the welcome and exit field stops being dark.
///
/// This is the load-bearing number. `WelcomeEulaDlg` — the whole of
/// `WixUI_Minimal`'s first page — draws its transparent title at dialog x
/// **130**, which is 173 px, and its licence and its accept box start at the
/// same column; `ExitDialog`, `FatalError` and `UserExit` use 135, or 180 px.
/// 168 clears the tighter of the two by five pixels. It was 176 first, taken
/// off the exit dialog's 135 alone, which put the first page's title three
/// pixels into a near-black ground — the one place a heading is drawn largest.
const DIALOG_SIDEBAR: u32 = 168;

/// Margin around the brand group inside the banner's dark block.
const BLOCK_MARGIN: f32 = 12.0;

/// The mark's side in the sidebar, and the gap under it before the wordmark.
///
/// The splash lays the mark and the wordmark out as a *row*, and that row does
/// not fit a 176 px column at any size worth reading — at the scale that fits,
/// the mark is 29 px in a field 312 px tall. So the sidebar stacks them. It is
/// the only second arrangement of the brand group in Umber and it is stated
/// here, once, beside the only thing that draws it.
const SIDEBAR_MARK: f32 = 88.0;
const SIDEBAR_GAP: f32 = 22.0;

/// The theme the installer wears.
///
/// Graphite for the brand block, Paper for the ground MSI writes on. Both are
/// Umber's own tables rather than two colours picked to look like them, which
/// is what lets the tests below detect a picture generated before the palette
/// last moved.
fn ink() -> Palette {
    Palette::graphite()
}

fn ground() -> Palette {
    Palette::paper()
}

/// The banner, as a 24-bit BMP.
pub fn banner_bmp() -> Vec<u8> {
    bmp(BANNER.0, BANNER.1, &banner_pixels())
}

/// The welcome and exit field, as a 24-bit BMP.
pub fn dialog_bmp() -> Vec<u8> {
    bmp(DIALOG.0, DIALOG.1, &dialog_pixels())
}

fn banner_pixels() -> Vec<u32> {
    let (w, h) = BANNER;
    let mut canvas = vec![pack(ground().chrome); (w * h) as usize];

    // The block is the splash's own brand group, at whatever scale fits it —
    // `splash::banner` fills its field with the backdrop and centres the group,
    // which is exactly what a block of this shape wants. Reusing it rather than
    // re-laying it out is what stops the installer and the start-up screen
    // drifting apart about how wide "UMBER" is.
    let block_w = w - BANNER_SPLIT;
    let (row_w, row_h) = splash::row_extent(1.0);
    let scale = ((block_w as f32 - BLOCK_MARGIN * 2.0) / row_w)
        .min((h as f32 - BLOCK_MARGIN * 2.0) / row_h)
        .max(0.05);
    let block = splash::banner(block_w as usize, h as usize, scale, &ink());
    blit(&mut canvas, w, &block, block_w, BANNER_SPLIT, 0);

    canvas
}

fn dialog_pixels() -> Vec<u32> {
    let (w, h) = DIALOG;
    let ink = ink();
    let mut canvas = vec![pack(ground().chrome); (w * h) as usize];

    for y in 0..h {
        for x in 0..DIALOG_SIDEBAR {
            canvas[(y * w + x) as usize] = pack(ink.backdrop);
        }
    }

    let column = DIALOG_SIDEBAR as f32;
    let wordmark = fitted_wordmark(column - BLOCK_MARGIN * 4.0);
    let word_h = wordmark.as_ref().map_or(0.0, |f| f.cap_height());
    let group_h = SIDEBAR_MARK
        + if word_h > 0.0 {
            SIDEBAR_GAP + word_h
        } else {
            0.0
        };
    let top = (h as f32 - group_h) * 0.5;

    let mark = logo::mark_rgba(SIDEBAR_MARK as u32, ink.accent);
    stamp(
        &mut canvas,
        w,
        &mark,
        SIDEBAR_MARK as u32,
        ((column - SIDEBAR_MARK) * 0.5).round() as i32,
        top.round() as i32,
    );

    if let Some(font) = &wordmark {
        let baseline = top + SIDEBAR_MARK + SIDEBAR_GAP + word_h;
        let x = (column - font.width("UMBER")) * 0.5;
        font.draw("UMBER", x, baseline, |px, py, coverage| {
            blend(&mut canvas, w, h, px, py, ink.text_strong, coverage);
        });
    }

    canvas
}

/// The wordmark at the largest size that fits `width`.
///
/// Two constructions rather than a table of sizes: the width of "UMBER" at
/// weight 900 is Archivo's to decide, and a number written down here would be
/// wrong the day the font is updated.
fn fitted_wordmark(width: f32) -> Option<Font> {
    const PROBE: f32 = 40.0;
    let probe = Font::new(PROBE, 900.0, -PROBE / 32.0)?;
    let measured = probe.width("UMBER");
    if measured <= 0.0 {
        return Some(probe);
    }
    let size = PROBE * (width / measured);
    Font::new(size, 900.0, -size / 32.0)
}

/// Copy a `0RGB` block into the canvas at `(at_x, at_y)`. Opaque — the block
/// carries its own backdrop.
fn blit(canvas: &mut [u32], canvas_w: u32, block: &[u32], block_w: u32, at_x: u32, at_y: u32) {
    let rows = block.len() as u32 / block_w.max(1);
    for y in 0..rows {
        for x in 0..block_w {
            let to = ((at_y + y) * canvas_w + at_x + x) as usize;
            if to < canvas.len() {
                canvas[to] = block[(y * block_w + x) as usize];
            }
        }
    }
}

/// Composite straight-alpha RGBA over the canvas.
fn stamp(canvas: &mut [u32], canvas_w: u32, rgba: &[u8], side: u32, at_x: i32, at_y: i32) {
    let height = canvas.len() as u32 / canvas_w.max(1);
    for y in 0..side {
        for x in 0..side {
            let i = ((y * side + x) * 4) as usize;
            let colour = Color32::from_rgb(rgba[i], rgba[i + 1], rgba[i + 2]);
            let coverage = rgba[i + 3] as f32 / 255.0;
            blend(
                canvas,
                canvas_w,
                height,
                at_x + x as i32,
                at_y + y as i32,
                colour,
                coverage,
            );
        }
    }
}

/// Blend in sRGB, exactly as `splash::Buffer::blend` does and for the reason it
/// gives: this is antialiasing coverage over flat interface colours.
fn blend(canvas: &mut [u32], w: u32, h: u32, x: i32, y: i32, colour: Color32, coverage: f32) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 || coverage <= 0.0 {
        return;
    }
    let coverage = coverage.clamp(0.0, 1.0);
    let i = y as usize * w as usize + x as usize;
    let dst = canvas[i];
    let mix = |shift: u32, src: u8| {
        let d = ((dst >> shift) & 0xFF) as f32;
        let v = d + (src as f32 - d) * coverage;
        (v.round().clamp(0.0, 255.0) as u32) << shift
    };
    canvas[i] = mix(16, colour.r()) | mix(8, colour.g()) | mix(0, colour.b());
}

fn pack(colour: Color32) -> u32 {
    ((colour.r() as u32) << 16) | ((colour.g() as u32) << 8) | colour.b() as u32
}

/// Pack `0RGB` pixels into a 24-bit uncompressed Windows bitmap.
///
/// 24-bit and `BI_RGB`, deliberately. MSI loads these through `LoadImage`,
/// which reads a 32-bit bitmap's fourth channel as padding rather than as
/// alpha on some shells and as alpha on others — a picture that is opaque
/// everywhere has no use for the ambiguity.
fn bmp(width: u32, height: u32, pixels: &[u32]) -> Vec<u8> {
    let stride = ((width * 3).div_ceil(4)) * 4;
    let image = (stride * height) as usize;
    let mut out = Vec::with_capacity(54 + image);

    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + image as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&54u32.to_le_bytes()); // offset to the pixels

    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes());
    // Positive: rows run bottom-up, which is the layout every reader accepts.
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(image as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    for y in (0..height).rev() {
        let row = out.len();
        for x in 0..width {
            let p = pixels[(y * width + x) as usize];
            out.push(p as u8); // blue
            out.push((p >> 8) as u8); // green
            out.push((p >> 16) as u8); // red
        }
        out.resize(row + stride as usize, 0);
    }
    out
}

/// Read a committed bitmap back: `(width, height, 0RGB pixels, top-down)`.
///
/// Deliberately its own parser rather than the `image` crate's. `image` is a
/// dependency of `umber-core` with only the formats an export needs turned on,
/// and BMP is not one of them; a header this short is not worth widening it
/// for, and reading the file with a second implementation is what makes the
/// checks below evidence about the *file* rather than about the writer.
fn read_bmp(bytes: &[u8]) -> Option<(u32, u32, Vec<u32>)> {
    if bytes.len() < 54 || &bytes[0..2] != b"BM" {
        return None;
    }
    let at = u32::from_le_bytes(bytes[10..14].try_into().ok()?) as usize;
    let width = i32::from_le_bytes(bytes[18..22].try_into().ok()?);
    let height = i32::from_le_bytes(bytes[22..26].try_into().ok()?);
    let bits = u16::from_le_bytes(bytes[28..30].try_into().ok()?);
    if width <= 0 || height <= 0 || bits != 24 {
        return None;
    }
    let (w, h) = (width as u32, height as u32);
    let stride = ((w * 3).div_ceil(4)) * 4;

    let mut pixels = vec![0u32; (w * h) as usize];
    for y in 0..h {
        let row = at + ((h - 1 - y) * stride) as usize;
        for x in 0..w {
            let i = row + (x * 3) as usize;
            let (b, g, r) = (*bytes.get(i)?, *bytes.get(i + 1)?, *bytes.get(i + 2)?);
            pixels[(y * w + x) as usize] = ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
        }
    }
    Some((w, h, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn art_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows")
    }

    fn committed(name: &str) -> (u32, u32, Vec<u32>) {
        let path = art_dir().join(name);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("no installer art at {}: {e}", path.display()));
        read_bmp(&bytes).expect("a 24-bit uncompressed BMP is what WiX is handed")
    }

    /// Relative luminance, near enough for "will black text read on this".
    fn luma(p: u32) -> f32 {
        let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
        (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
    }

    fn darkest(pixels: &[u32], w: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
        let mut worst = 1.0f32;
        for y in y0..y1 {
            for x in x0..x1 {
                worst = worst.min(luma(pixels[(y * w + x) as usize]));
            }
        }
        worst
    }

    #[test]
    fn the_art_is_the_size_wix_hands_to_the_dialogs() {
        // WiX scales neither bitmap: a banner of the wrong size is drawn at its
        // own size in the corner of the control, with the dialog's grey showing
        // through the rest.
        let (w, h, _) = committed("banner.bmp");
        assert_eq!((w, h), BANNER);
        let (w, h, _) = committed("dialog.bmp");
        assert_eq!((w, h), DIALOG);
    }

    /// The one that matters. MSI writes each dialog's title over the bitmap in
    /// black, and nothing in a stock dialog set can change that colour for one
    /// page — so a picture that is dark where the text lands is an installer
    /// whose headings cannot be read.
    #[test]
    fn black_text_reads_everywhere_wix_writes_it() {
        let (w, _, banner) = committed("banner.bmp");
        assert!(
            darkest(&banner, w, 0, 0, BANNER_SPLIT, BANNER.1) > 0.6,
            "the banner is dark where MSI draws the page title"
        );

        let (w, _, dialog) = committed("dialog.bmp");
        assert!(
            darkest(&dialog, w, DIALOG_SIDEBAR, 0, DIALOG.0, DIALOG.1) > 0.6,
            "the welcome and exit field is dark where MSI draws its text"
        );
    }

    /// A picture generated before the palette last moved is one nobody would
    /// notice for a release or two. Both distinctive colours are checked: the
    /// ground MSI writes on, and the accent the mark is filled with.
    #[test]
    fn the_committed_art_is_drawn_in_this_palettes_colours() {
        for name in ["banner.bmp", "dialog.bmp"] {
            let (_, _, pixels) = committed(name);
            assert!(
                pixels.contains(&pack(ground().chrome)),
                "{name} is not on this Paper's chrome"
            );
            assert!(
                pixels.contains(&pack(ink().accent)),
                "{name} carries no mark in this Graphite's accent"
            );
            assert!(
                pixels.contains(&pack(ink().backdrop)),
                "{name} has no block on this Graphite's backdrop"
            );
        }
    }

    /// The generator has to satisfy the same rule the committed files are held
    /// to, or regenerating them is how the installer becomes unreadable.
    #[test]
    fn what_the_generator_draws_would_pass_the_same_checks() {
        let (w, h, banner) = read_bmp(&banner_bmp()).expect("the writer's own output parses");
        assert_eq!((w, h), BANNER);
        assert!(darkest(&banner, w, 0, 0, BANNER_SPLIT, h) > 0.6);

        let (w, h, dialog) = read_bmp(&dialog_bmp()).expect("the writer's own output parses");
        assert_eq!((w, h), DIALOG);
        assert!(darkest(&dialog, w, DIALOG_SIDEBAR, 0, w, h) > 0.6);
        assert!(dialog.contains(&pack(ink().accent)), "the mark is missing");
    }

    /// Regenerate `packaging/windows/*.bmp`.
    ///
    /// Ignored because it writes into the source tree, exactly like
    /// `logo::tests::regenerate_icons`. Run it after changing the palette or
    /// the mark and commit what it produces:
    ///
    /// ```sh
    /// cargo test -p umber-app regenerate_installer_art -- --ignored
    /// ```
    #[test]
    #[ignore = "writes into packaging/windows; run deliberately after changing the palette"]
    fn regenerate_installer_art() {
        let dir = art_dir();
        std::fs::write(dir.join("banner.bmp"), banner_bmp()).expect("write banner.bmp");
        std::fs::write(dir.join("dialog.bmp"), dialog_bmp()).expect("write dialog.bmp");
    }
}
