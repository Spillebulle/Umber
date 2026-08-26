//! The Umber mark.
//!
//! The design's brand element is the small brown box in the top-left corner of
//! the menu bar: a plain rounded square filled with the accent, carrying no
//! glyph and no outline. It is drawn at 15 px with a 3 px corner radius there,
//! which is the only proportion the mark actually has — [`CORNER_RATIO`].
//!
//! It is authored once here and rendered two ways:
//!
//! * [`draw_mark`] paints it with egui shapes, the way `icons.rs` draws
//!   everything else, so on screen it is resolution independent.
//! * [`mark_rgba`] rasterises the same shape on the CPU. Window and taskbar
//!   icons want pixels, not vectors, and no windowing system will accept a
//!   display list.
//!
//! Both read their geometry from the same constants, so the icon in the taskbar
//! and the mark in the menu bar cannot drift apart. The `regenerate_icons` test
//! at the bottom of this file writes `assets/icons/` from `mark_rgba`, which is
//! why the committed PNGs are reproducible rather than an orphan binary blob.

use crate::theme::Palette;
use egui::{Color32, CornerRadius, Painter, Rect, Vec2};

/// Corner radius as a fraction of the side.
///
/// The design draws the mark at 15 px with a 3 px radius. Expressed as a ratio
/// rather than a pixel value so a 16 px favicon and a 256 px application icon
/// are recognisably the same shape.
pub const CORNER_RATIO: f32 = 3.0 / 15.0;

/// Corner radius the design uses for the mark **on the splash**, where it is
/// drawn at 52 px with an 8 px radius.
///
/// That is 0.154, not the 0.2 of [`CORNER_RATIO`]. The design is simply
/// inconsistent between its two instances of the mark, and this records the
/// discrepancy rather than quietly averaging it away or "fixing" one to match
/// the other. Each size uses the number the design states for that size, which
/// is also the safer reading: a corner radius that looks right at 15 px is
/// usually too round when scaled to 52.
pub const SPLASH_CORNER_RATIO: f32 = 8.0 / 52.0;

/// Clear space left around the mark inside a raster icon, as a fraction of the
/// bitmap's side.
///
/// On screen the mark fills whatever rect it is given, because the surrounding
/// layout provides the breathing room. A window icon has no such layout: it is
/// dropped straight into a title bar or a taskbar button, and several platforms
/// scale or mask it. A sixteenth of the side is one pixel at 16 px — enough
/// that the corners are never clipped, small enough that the mark still reads
/// at the smallest size.
const ICON_MARGIN: f32 = 1.0 / 16.0;

/// The size the window icon is rasterised at.
///
/// Windows scales a single supplied icon rather than picking from a set, so
/// this wants to be comfortably larger than the 32 px it will usually be shown
/// at. The executable resource (see `crates/umber-desktop/build.rs`) carries
/// the full multi-resolution `.ico`; this is only the fallback the running
/// process hands the window manager.
const WINDOW_ICON: u32 = 64;

/// The size the taskbar icon is rasterised at.
///
/// Larger than [`WINDOW_ICON`] because this one is drawn large: the taskbar
/// button and the Alt-Tab switcher want 32 px at 100% scaling and up from
/// there, and winit names 256 as a good ceiling for `ICON_BIG`. The mark is a
/// rounded square whose only detail is the corner radius, so it survives the
/// downscale without needing a set of hand-tuned sizes the way a glyph would.
///
/// Windows-only, like the function that reads it: it is `ICON_BIG`, which no
/// other platform has. macOS takes its icon from the bundle and Linux from the
/// installed desktop entry, so on both this would be a rasterisation nobody
/// asks for — and under CI's `-D warnings`, a build failure.
#[cfg(target_os = "windows")]
const TASKBAR_ICON: u32 = 256;

/// Draw the mark, filling `rect`.
///
/// Everything is derived from `rect`, so the same call gives the 15 px menu-bar
/// mark and a 200 px one. A non-square rect yields a square mark centred inside
/// it rather than a stretched one — the mark is a square, and stretching a brand
/// element is worse than ignoring the extra space.
///
/// Drawn by the About dialog, which is the first thing to want the mark at a
/// size worth looking at. The splash rasterises it on the CPU instead, because
/// it runs before egui exists, and the menu bar still inlines its own
/// `rect_filled` — that call site should move here too. This is the only place
/// the mark's geometry is stated for an egui painter.
pub fn draw_mark(painter: &Painter, rect: Rect, palette: &Palette) {
    let side = rect.width().min(rect.height());
    if side <= 0.0 {
        return;
    }
    let square = Rect::from_center_size(rect.center(), Vec2::splat(side));
    // egui's corner radius is a `u8`, so this rounds rather than truncating:
    // at 15 px the design's 3 px radius is 2.999…, and truncation would give a
    // visibly squarer mark than the one specified.
    let radius = (side * CORNER_RATIO).round().clamp(0.0, 255.0) as u8;
    painter.rect_filled(square, CornerRadius::same(radius), palette.accent);
}

/// Rasterise the mark into straight-alpha RGBA8, `size` × `size`.
///
/// Antialiasing comes from the rounded box's signed distance field rather than
/// from supersampling: the distance to the outline is exact and cheap, and
/// converting it to coverage with `0.5 - d` is exact for the straight edges and
/// close enough on the corner arcs. Supersampling would need a 16× budget to
/// look as smooth at 16 px, which is where an application icon is hardest.
///
/// Straight alpha, not premultiplied — `winit::window::Icon::from_rgba` and the
/// PNG encoder both want it that way.
pub fn mark_rgba(size: u32, colour: Color32) -> Vec<u8> {
    let n = size as f32;
    // Rounded so the straight edges land on pixel boundaries. An edge halfway
    // across a pixel row would be blurred by the coverage calculation, and a
    // small icon with soft edges reads as out of focus rather than as smooth.
    let margin = (n * ICON_MARGIN).round();
    let half = (n - margin * 2.0) * 0.5;
    let radius = (half * 2.0 * CORNER_RATIO).min(half);
    let centre = n * 0.5;

    let (r, g, b) = (colour.r(), colour.g(), colour.b());
    let mut out = Vec::with_capacity((size as usize) * (size as usize) * 4);
    for y in 0..size {
        for x in 0..size {
            let coverage = rounded_box_coverage(
                x as f32 + 0.5 - centre,
                y as f32 + 0.5 - centre,
                half,
                half,
                radius,
            );
            out.extend_from_slice(&[r, g, b, (coverage * 255.0).round() as u8]);
        }
    }
    out
}

/// The icon the running process hands the window manager.
///
/// Returns `None` rather than failing: an application that will not start
/// because it could not build its own icon would be a poor trade.
pub fn window_icon() -> Option<winit::window::Icon> {
    icon_at(WINDOW_ICON)
}

/// The icon Windows draws on the **taskbar button and in Alt-Tab**.
///
/// Windows keeps two icons per window and winit sets them through two different
/// calls: [`window_icon`] goes to `with_window_icon`, which is `ICON_SMALL` and
/// reaches only the title bar, while this goes to the Windows-only
/// `with_taskbar_icon`, which is `ICON_BIG`.
///
/// Setting only the first is not a smaller version of setting both — it leaves
/// the taskbar with **nothing**. winit registers its window class with
/// `hIcon: 0`, so there is no class icon to fall back to either, and Windows
/// draws its generic application icon instead. That is what Umber shipped with
/// through 0.0.3: the right mark in the title bar and a blank page on the
/// taskbar. The executable's own icon resource does not rescue it — that one is
/// for Explorer, the Start Menu shortcut and the moment before the process
/// exists, not for a window that is already up.
#[cfg(target_os = "windows")]
pub fn taskbar_icon() -> Option<winit::window::Icon> {
    icon_at(TASKBAR_ICON)
}

fn icon_at(size: u32) -> Option<winit::window::Icon> {
    // Fixed to the default theme's accent rather than following the live
    // palette. A taskbar button that changes colour when the user switches
    // theme reads as a different application, and most window managers cache
    // the icon anyway, so the change would be unpredictable as well as unwanted.
    let rgba = mark_rgba(size, Palette::graphite().accent);
    match winit::window::Icon::from_rgba(rgba, size, size) {
        Ok(icon) => Some(icon),
        Err(e) => {
            log::warn!("could not build the {size}px window icon: {e}");
            None
        }
    }
}

/// Coverage of a rounded box at `(x, y)`, measured from its centre.
///
/// Shared with the splash, which paints the mark — and its progress bar, which
/// is the same shape at a very different aspect ratio — on the CPU, for the same
/// reason this module rasterises it: no GPU exists yet when it runs.
pub fn rounded_box_coverage(x: f32, y: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    // Distance is in pixels and signed, so how far the pixel centre sits inside
    // the shape is the coverage: exact for the straight edges, and close enough
    // on the corner arcs to be indistinguishable from a supersampled result.
    (0.5 - rounded_box_sdf(x, y, half_w, half_h, radius)).clamp(0.0, 1.0)
}

/// Signed distance from `(x, y)` to a rounded box centred on the origin, with
/// the given half-extents and corner radius. Negative inside.
///
/// `hypot` rather than `powf`: the exponent is fixed at a half, and `powf` on a
/// value that has drifted a hair below zero is NaN — a trap this codebase has
/// already been bitten by once.
fn rounded_box_sdf(x: f32, y: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    // A radius larger than the box is not a rounder box, it is a broken one:
    // the corner arcs would cross and the distance field would fold inside out.
    let radius = radius.min(half_w).min(half_h).max(0.0);
    let qx = x.abs() - half_w + radius;
    let qy = y.abs() - half_h + radius;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// The sizes Windows, Linux and macOS between them ask for.
    const SIZES: [u32; 6] = [16, 32, 48, 64, 128, 256];

    fn icon_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons")
    }

    fn brand() -> Color32 {
        Palette::graphite().accent
    }

    fn alpha_at(rgba: &[u8], size: u32, x: u32, y: u32) -> u8 {
        rgba[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn the_mark_is_solid_in_the_middle_and_clear_at_the_corners() {
        let rgba = mark_rgba(64, brand());
        assert_eq!(rgba.len(), 64 * 64 * 4);
        assert_eq!(alpha_at(&rgba, 64, 32, 32), 255);
        // Outside both the margin and the corner rounding.
        assert_eq!(alpha_at(&rgba, 64, 0, 0), 0);
        assert_eq!(alpha_at(&rgba, 64, 63, 63), 0);
    }

    #[test]
    fn the_mark_is_the_accent_wherever_it_is_opaque() {
        let accent = brand();
        let rgba = mark_rgba(32, accent);
        let centre = ((16 * 32 + 16) * 4) as usize;
        assert_eq!(
            &rgba[centre..centre + 4],
            &[accent.r(), accent.g(), accent.b(), 255]
        );
    }

    #[test]
    fn the_smallest_icon_still_has_antialiased_corners() {
        // Partial coverage somewhere is the whole reason for the distance
        // field: without it a 16 px icon is a plain square with four jagged
        // corners, which is exactly where a brand mark is judged.
        let rgba = mark_rgba(16, brand());
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] > 0 && px[3] < 255)
        );
    }

    #[test]
    fn the_mark_is_symmetric() {
        // A distance field is symmetric by construction, so this is really a
        // guard on the pixel indexing — an off-by-one in the row walk would
        // shift the mark by a pixel and show up nowhere else.
        let size = 33;
        let rgba = mark_rgba(size, brand());
        for y in 0..size {
            for x in 0..size {
                assert_eq!(
                    alpha_at(&rgba, size, x, y),
                    alpha_at(&rgba, size, size - 1 - x, size - 1 - y),
                    "asymmetry at ({x}, {y})"
                );
            }
        }
    }

    /// Encode straight-alpha RGBA8 as a PNG in memory.
    fn png(size: u32, rgba: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, size, size);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            encoder
                .write_header()
                .expect("png header")
                .write_image_data(rgba)
                .expect("png data");
        }
        buf
    }

    /// A 32-bit bottom-up DIB, with the 1-bit AND mask an `.ico` entry still
    /// requires even though a 32-bit image carries its own alpha. Windows
    /// ignores the mask when the entry is 32 bpp, but the header declares a
    /// doubled height and readers trust that, so the bytes have to be there.
    fn dib(size: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&40u32.to_le_bytes()); // biSize
        out.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
        out.extend_from_slice(&(size as i32 * 2).to_le_bytes()); // biHeight: image + mask
        out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
        out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
        out.extend_from_slice(&(size * size * 4).to_le_bytes()); // biSizeImage
        out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
        out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
        out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

        // Bottom-up rows, BGRA.
        for y in (0..size).rev() {
            for x in 0..size {
                let i = ((y * size + x) * 4) as usize;
                out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        // AND mask: one bit per pixel, rows padded to four bytes. Zero means
        // "take the colour", which the alpha channel then decides.
        let row = (size as usize).div_ceil(32) * 4;
        out.resize(out.len() + row * size as usize, 0);
        out
    }

    /// Pack the images into a Windows `.ico`.
    fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut dir = Vec::new();
        dir.extend_from_slice(&0u16.to_le_bytes()); // reserved
        dir.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon, 2 = cursor
        dir.extend_from_slice(&(images.len() as u16).to_le_bytes());

        // Directory entries are a fixed sixteen bytes, so every image's offset
        // is known before a byte of image data has been written.
        let mut offset = 6 + 16 * images.len() as u32;
        let mut blobs = Vec::with_capacity(images.len());
        for (size, rgba) in images {
            // Vista and later read a PNG straight out of an entry, which saves
            // a quarter of a megabyte on the 256 px one. Smaller entries stay
            // DIB, which every shell understands.
            let png_entry = *size >= 256;
            let blob = if png_entry {
                png(*size, rgba)
            } else {
                dib(*size, rgba)
            };
            // 256 is recorded as zero: the field is a single byte.
            let dimension = if png_entry { 0u8 } else { *size as u8 };
            dir.push(dimension); // bWidth
            dir.push(dimension); // bHeight
            dir.push(0); // bColorCount, zero for true colour
            dir.push(0); // bReserved
            dir.extend_from_slice(&1u16.to_le_bytes()); // wPlanes
            dir.extend_from_slice(&32u16.to_le_bytes()); // wBitCount
            dir.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            dir.extend_from_slice(&offset.to_le_bytes());
            offset += blob.len() as u32;
            blobs.push(blob);
        }
        for blob in blobs {
            dir.extend_from_slice(&blob);
        }
        dir
    }

    #[test]
    fn the_ico_directory_points_at_real_data() {
        let images: Vec<(u32, Vec<u8>)> =
            SIZES.iter().map(|&s| (s, mark_rgba(s, brand()))).collect();
        let bytes = ico(&images);
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 1);
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), SIZES.len() as u16);
        for i in 0..SIZES.len() {
            let entry = 6 + 16 * i;
            let len = u32::from_le_bytes(bytes[entry + 8..entry + 12].try_into().unwrap()) as usize;
            let at = u32::from_le_bytes(bytes[entry + 12..entry + 16].try_into().unwrap()) as usize;
            assert!(
                at + len <= bytes.len(),
                "entry {i} runs past the end of the file"
            );
        }
    }

    /// Regenerate `assets/icons/` from the mark above.
    ///
    /// Ignored because it writes into the source tree. Run it deliberately
    /// after changing the mark and commit what it produces:
    ///
    /// ```sh
    /// cargo test -p umber-app regenerate_icons -- --ignored
    /// ```
    ///
    /// It lives beside `draw_mark` rather than in a `tools/` binary so that it
    /// shares the geometry constants directly. The taskbar icon and the
    /// menu-bar mark are then the same shape by construction rather than by
    /// discipline, and `cargo clippy --all-targets` keeps the generator
    /// compiling as the crate moves under it.
    #[test]
    #[ignore = "writes into assets/icons; run deliberately after changing the mark"]
    fn regenerate_icons() {
        let dir = icon_dir();
        std::fs::create_dir_all(&dir).expect("create assets/icons");

        let mut images = Vec::with_capacity(SIZES.len());
        for size in SIZES {
            let rgba = mark_rgba(size, brand());
            let path = dir.join(format!("umber-{size}.png"));
            std::fs::write(&path, png(size, &rgba)).expect("write png");
            images.push((size, rgba));
        }
        std::fs::write(dir.join("umber.ico"), ico(&images)).expect("write ico");
    }
}
