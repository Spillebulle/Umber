//! The print sheet: a one-page PDF of the flattened picture at its physical
//! size, with the registration marks drawn where [`crate::sheet`] says they are.
//!
//! A PDF rather than a PNG because a PNG has no page. Its resolution field is a
//! hint every print dialog is free to ignore, and most of them scale a picture
//! to fit the paper, which moves every mark and makes the cut land somewhere
//! else. A PDF page *is* a size in points, so printing it at 100% puts a
//! millimetre on the paper for every millimetre in the file.
//!
//! The writer is small on purpose: one page, one image, one content stream of
//! filled rectangles, and a cross-reference table. No crate is pulled in for it,
//! because everything a PDF library would add (fonts, text, annotations) is
//! something this page must not carry.
//!
//! The picture is matted onto white **in linear light** through
//! [`crate::export`]'s own table, so a soft edge prints exactly as the flattened
//! export of the same document would show it over white. Rows go through the
//! compressor one at a time, so the writer holds one row of RGB beside the
//! picture rather than a second canvas.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use glam::UVec2;

use crate::docformat::SaveError;
use crate::document::sane_dpi;
use crate::sheet::{Marks, RectMm};

/// Points in an inch, which is what a PDF measures in.
const POINTS_PER_INCH: f64 = 72.0;
/// Millimetres in an inch.
const MM_PER_INCH: f64 = 25.4;

/// What goes on the page.
pub struct Sheet<'a> {
    /// The picture's size in pixels.
    pub size: UVec2,
    /// Pixels per inch, which with `size` is the page's physical size.
    pub dpi: f32,
    /// Straight-alpha sRGB RGBA8, `size.x * size.y * 4` bytes, as
    /// `CanvasRenderer::export_rgba` hands it over.
    pub rgba: &'a [u8],
    /// The registration marks, or `None` for a sheet that only prints.
    pub marks: Option<Marks>,
}

/// Why a sheet was not written.
#[derive(Debug)]
pub enum SheetError {
    /// A buffer that is not the picture's size. A caller bug, refused rather
    /// than written as a sheared image.
    WrongSize {
        found: usize,
        expected: usize,
    },
    Io(io::Error),
}

impl std::fmt::Display for SheetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SheetError::WrongSize { found, expected } => write!(
                f,
                "The picture was {found} bytes where {expected} were expected."
            ),
            SheetError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SheetError {}

impl From<io::Error> for SheetError {
    fn from(e: io::Error) -> Self {
        SheetError::Io(e)
    }
}

/// So a sheet can be written through `docformat::write_with`, the one
/// temp-and-rename a file Umber writes goes through.
impl From<SheetError> for SaveError {
    fn from(e: SheetError) -> Self {
        match e {
            SheetError::WrongSize { found, expected } => SaveError::WrongSize {
                what: "print sheet".to_string(),
                found,
                expected,
            },
            SheetError::Io(e) => SaveError::Io(e),
        }
    }
}

/// The name to suggest for the sheet of a document titled `title`.
pub fn default_file_name(title: &str) -> String {
    format!("{} print sheet.pdf", crate::export::title_stem(title))
}

/// `path` with `.pdf` appended unless it already ends in it.
///
/// Appended, never substituted, for the reason `export::target` gives: not every
/// save dialog adds the filter's extension, and substituting would rename a
/// deliberate `study.v2`.
pub fn with_pdf_extension(path: &Path) -> PathBuf {
    let is_pdf = path
        .extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("pdf"));
    if is_pdf {
        path.to_path_buf()
    } else {
        let mut name = path.as_os_str().to_os_string();
        name.push(".pdf");
        PathBuf::from(name)
    }
}

/// Counts what passes through, because the cross-reference table is a list of
/// byte offsets.
struct Counting<W> {
    inner: W,
    at: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.at += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Points from millimetres.
fn pt(mm: f32) -> f64 {
    mm as f64 * POINTS_PER_INCH / MM_PER_INCH
}

/// The content stream: the picture filling the page, then the marks in black.
fn content(page: (f64, f64), marks: Option<&Marks>) -> String {
    let (w, h) = page;
    let mut s = format!("q {w:.3} 0 0 {h:.3} 0 0 cm /Im0 Do Q\n");
    if let Some(marks) = marks {
        s.push_str("0 g\n");
        for RectMm { min, max } in marks.fills() {
            // PDF's origin is the page's bottom left; the model's is the top left.
            let x = pt(min.x);
            let y = h - pt(max.y);
            let rw = pt(max.x - min.x);
            let rh = pt(max.y - min.y);
            s.push_str(&format!("{x:.3} {y:.3} {rw:.3} {rh:.3} re f\n"));
        }
    }
    s
}

/// Write `sheet` to `out` as a one-page PDF 1.4.
pub fn write(out: &mut impl Write, sheet: &Sheet<'_>) -> Result<(), SheetError> {
    let (width, height) = (sheet.size.x as usize, sheet.size.y as usize);
    let expected = width * height * 4;
    if sheet.rgba.len() != expected {
        return Err(SheetError::WrongSize {
            found: sheet.rgba.len(),
            expected,
        });
    }

    let dpi = sane_dpi(sheet.dpi) as f64;
    let page = (
        width as f64 * POINTS_PER_INCH / dpi,
        height as f64 * POINTS_PER_INCH / dpi,
    );

    let mut out = Counting { inner: out, at: 0 };
    // Object n's offset is at index n; object 0 is the free list's head.
    let mut offsets = [0u64; 7];

    // The second line is four bytes over 127, which is how a PDF tells a
    // transfer program it is binary.
    out.write_all(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")?;

    offsets[1] = out.at;
    out.write_all(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")?;

    offsets[2] = out.at;
    out.write_all(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n")?;

    offsets[3] = out.at;
    write!(
        out,
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.3} {:.3}] \
         /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>\nendobj\n",
        page.0, page.1
    )?;

    // The image's length is not known until it has been compressed, so it is
    // an indirect object written after the stream.
    offsets[4] = out.at;
    write!(
        out,
        "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
         /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length 6 0 R >>\nstream\n"
    )?;
    let start = out.at;
    {
        let white = crate::export::matte_table(255);
        let mut encoder = ZlibEncoder::new(&mut out, Compression::default());
        let mut row = vec![0u8; width * 3];
        for line in sheet.rgba.chunks_exact(width.max(1) * 4).take(height) {
            for (px, dst) in line
                .as_chunks::<4>()
                .0
                .iter()
                .zip(row.as_chunks_mut::<3>().0.iter_mut())
            {
                let a = px[3] as usize * 256;
                for c in 0..3 {
                    dst[c] = white[a + px[c] as usize];
                }
            }
            encoder.write_all(&row)?;
        }
        encoder.finish()?;
    }
    let length = out.at - start;
    out.write_all(b"\nendstream\nendobj\n")?;

    offsets[6] = out.at;
    write!(out, "6 0 obj\n{length}\nendobj\n")?;

    let stream = content(page, sheet.marks.as_ref());
    offsets[5] = out.at;
    write!(
        out,
        "5 0 obj\n<< /Length {} >>\nstream\n{stream}endstream\nendobj\n",
        stream.len()
    )?;

    let xref = out.at;
    out.write_all(b"xref\n0 7\n0000000000 65535 f \n")?;
    for offset in &offsets[1..] {
        write!(out, "{offset:010} 00000 n \n")?;
    }
    write!(
        out,
        "trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    )?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
        hay[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    }

    fn pdf(sheet: &Sheet<'_>) -> Vec<u8> {
        let mut out = Vec::new();
        write(&mut out, sheet).expect("a sheet writes");
        out
    }

    fn opaque(size: UVec2, colour: [u8; 4]) -> Vec<u8> {
        colour.repeat((size.x * size.y) as usize)
    }

    /// The numbers between the brackets of `/MediaBox [ ... ]`.
    fn media_box(bytes: &[u8]) -> Vec<f64> {
        let at = find(bytes, b"/MediaBox [", 0).expect("a MediaBox") + 11;
        let end = find(bytes, b"]", at).unwrap();
        std::str::from_utf8(&bytes[at..end])
            .unwrap()
            .split_whitespace()
            .map(|n| n.parse().unwrap())
            .collect()
    }

    /// The body of object `n`'s stream.
    fn stream_of(bytes: &[u8], n: u32) -> &[u8] {
        let obj = find(bytes, format!("\n{n} 0 obj").as_bytes(), 0).expect("the object");
        let start = find(bytes, b"stream\n", obj).unwrap() + 7;
        &bytes[start..]
    }

    fn content_stream(bytes: &[u8]) -> String {
        let body = stream_of(bytes, 5);
        let end = find(body, b"endstream", 0).unwrap();
        String::from_utf8(body[..end].to_vec()).unwrap()
    }

    fn picture(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(stream_of(bytes, 4))
            .read_to_end(&mut out)
            .expect("the image inflates");
        out
    }

    #[test]
    fn the_page_is_the_documents_physical_size() {
        let size = UVec2::new(2480, 3508);
        let rgba = opaque(size, [255, 255, 255, 255]);
        let bytes = pdf(&Sheet {
            size,
            dpi: 300.0,
            rgba: &rgba,
            marks: None,
        });
        let mb = media_box(&bytes);
        assert_eq!(mb.len(), 4);
        assert_eq!((mb[0], mb[1]), (0.0, 0.0));
        assert!((mb[2] - 595.2).abs() < 0.01, "{mb:?}");
        assert!((mb[3] - 841.92).abs() < 0.01, "{mb:?}");
        assert!(bytes.starts_with(b"%PDF-1.4\n"));
    }

    #[test]
    fn the_cross_reference_table_points_at_every_object() {
        let size = UVec2::new(210, 297);
        let rgba = opaque(size, [40, 90, 200, 255]);
        let bytes = pdf(&Sheet {
            size,
            dpi: 25.4,
            rgba: &rgba,
            marks: Some(Marks::for_page(210.0, 297.0).unwrap()),
        });
        let sx = find(&bytes, b"startxref\n", 0).expect("startxref") + 10;
        let end = find(&bytes, b"\n", sx).unwrap();
        let xref: usize = std::str::from_utf8(&bytes[sx..end])
            .unwrap()
            .parse()
            .unwrap();
        assert!(bytes[xref..].starts_with(b"xref\n0 "));
        let header_end = find(&bytes, b"\n", xref + 5).unwrap() + 1;
        let count: usize = std::str::from_utf8(&bytes[xref + 7..header_end - 1])
            .unwrap()
            .parse()
            .unwrap();
        assert!(count >= 6, "{count}");
        for n in 0..count {
            let entry = &bytes[header_end + n * 20..header_end + (n + 1) * 20];
            let entry = std::str::from_utf8(entry).unwrap();
            assert!(
                entry.ends_with(" \n") || entry.ends_with("\r\n"),
                "{entry:?}"
            );
            if n == 0 {
                assert!(entry.starts_with("0000000000 65535 f"), "{entry:?}");
                continue;
            }
            assert_eq!(&entry[17..18], "n", "{entry:?}");
            let offset: usize = entry[..10].parse().unwrap();
            let want = format!("{n} 0 obj");
            assert!(
                bytes[offset..].starts_with(want.as_bytes()),
                "object {n} is not at {offset}"
            );
        }
        let trailer = find(&bytes, b"trailer", 0).unwrap();
        assert!(find(&bytes[trailer..], format!("/Size {count}").as_bytes(), 0).is_some());
    }

    #[test]
    fn the_picture_survives_the_round_trip() {
        let size = UVec2::new(3, 2);
        let colours: [[u8; 3]; 6] = [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [10, 20, 30],
            [200, 150, 100],
            [0, 0, 0],
        ];
        let rgba: Vec<u8> = colours
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect();
        let bytes = pdf(&Sheet {
            size,
            dpi: 72.0,
            rgba: &rgba,
            marks: None,
        });
        let want: Vec<u8> = colours.iter().flatten().copied().collect();
        assert_eq!(picture(&bytes), want);
        assert!(find(&bytes, b"/Width 3 /Height 2", 0).is_some());
        assert!(
            find(
                &bytes,
                b"/DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
                0
            )
            .is_some()
        );
    }

    #[test]
    fn transparency_mattes_onto_white_in_linear_light() {
        let size = UVec2::new(2, 1);
        let rgba = [90, 30, 200, 0, 0, 0, 0, 128];
        let bytes = pdf(&Sheet {
            size,
            dpi: 72.0,
            rgba: &rgba,
            marks: None,
        });
        let rgb = picture(&bytes);
        assert_eq!(&rgb[..3], &[255, 255, 255]);
        for c in &rgb[3..6] {
            assert!((*c as i32 - 188).abs() <= 1, "{rgb:?}");
        }
    }

    #[test]
    fn marks_are_drawn_where_the_model_puts_them() {
        // 210 x 297 pixels at 25.4 dpi is A4 exactly: one pixel a millimetre.
        let size = UVec2::new(210, 297);
        let rgba = opaque(size, [255, 255, 255, 255]);
        let bytes = pdf(&Sheet {
            size,
            dpi: 25.4,
            rgba: &rgba,
            marks: Some(Marks::for_page(210.0, 297.0).unwrap()),
        });
        let content = content_stream(&bytes);
        let tokens: Vec<&str> = content.split_whitespace().collect();
        let res: Vec<usize> = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| **t == "re")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(res.len(), 5, "{content}");
        let first: Vec<f64> = tokens[res[0] - 4..res[0]]
            .iter()
            .map(|n| n.parse().unwrap())
            .collect();
        let want = [28.346, 841.89 - 42.52, 14.173, 14.173];
        for (got, want) in first.iter().zip(want) {
            assert!((got - want).abs() < 0.01, "{first:?}");
        }
        // Filled in black, after the picture so the picture cannot cover them.
        let black = content.find("0 g").expect("black fill");
        assert!(content.find("Do").unwrap() < black);
        assert!(black < content.find(" re").unwrap());
    }

    #[test]
    fn a_sheet_without_marks_draws_none() {
        let size = UVec2::new(210, 297);
        let rgba = opaque(size, [255, 255, 255, 255]);
        let bytes = pdf(&Sheet {
            size,
            dpi: 25.4,
            rgba: &rgba,
            marks: None,
        });
        let content = content_stream(&bytes);
        assert!(content.contains("/Im0 Do"), "{content}");
        assert!(!content.split_whitespace().any(|t| t == "re"), "{content}");
    }

    #[test]
    fn a_buffer_of_the_wrong_length_is_refused() {
        let mut out = Vec::new();
        let rgba = [0u8; 23];
        let result = write(
            &mut out,
            &Sheet {
                size: UVec2::new(3, 2),
                dpi: 72.0,
                rgba: &rgba,
                marks: None,
            },
        );
        match result {
            Err(SheetError::WrongSize { found, expected }) => {
                assert_eq!((found, expected), (23, 24));
            }
            other => panic!("{other:?}"),
        }
        assert!(out.is_empty(), "nothing is written for a refused buffer");
    }

    #[test]
    fn the_suggested_name_drops_a_known_extension() {
        assert_eq!(default_file_name("sticker.ora"), "sticker print sheet.pdf");
        assert_eq!(default_file_name("study.v2"), "study.v2 print sheet.pdf");
        assert_eq!(default_file_name("  "), "untitled print sheet.pdf");
    }

    #[test]
    fn a_picked_name_gains_pdf_only_where_it_lacks_it() {
        assert_eq!(
            with_pdf_extension(Path::new("a/sheet")),
            PathBuf::from("a/sheet.pdf")
        );
        assert_eq!(
            with_pdf_extension(Path::new("a/sheet.PDF")),
            PathBuf::from("a/sheet.PDF")
        );
        assert_eq!(
            with_pdf_extension(Path::new("a/study.v2")),
            PathBuf::from("a/study.v2.pdf")
        );
    }
}
