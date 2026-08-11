//! Test files, built in memory.
//!
//! Every fixture here is generated rather than committed. A repository of
//! binary sample documents rots: nobody can review a diff to a `.psd`, nobody
//! remembers which application wrote it, and it is dead weight in every clone
//! forever. Writing the bytes in Rust means the fixture is readable, and the
//! test says out loud what it believes the format to be.
//!
//! That cuts both ways and is worth being honest about: a generated fixture
//! tests the reader against *this file's* understanding of the format, not
//! against Photoshop. Where that understanding came from real files, the
//! builder says so at the point it matters — see [`psd`].

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::csblocks::{self, Packing};
use crate::sqlite::Value;
use crate::sqlite::fixture::TableSpec;

// ---------------------------------------------------------------- PNG

fn encode_png(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("fixture png header");
        writer.write_image_data(data).expect("fixture png data");
    }
    out
}

pub fn png_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    encode_png(width, height, png::ColorType::Rgba, rgba)
}

pub fn png_rgb(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    encode_png(width, height, png::ColorType::Rgb, rgb)
}

pub fn png_grey(width: u32, height: u32, grey: &[u8]) -> Vec<u8> {
    encode_png(width, height, png::ColorType::Grayscale, grey)
}

/// `width * height` copies of one pixel.
fn solid(width: u32, height: u32, pixel: &[u8; 4]) -> Vec<u8> {
    pixel
        .iter()
        .copied()
        .cycle()
        .take(width as usize * height as usize * 4)
        .collect()
}

// ---------------------------------------------------------------- ZIP

struct Archive(ZipWriter<Cursor<Vec<u8>>>);

impl Archive {
    fn new(mimetype: &str) -> Self {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        // The specification requires `mimetype` first and uncompressed, and
        // real readers check it, so the fixtures do it properly.
        zip.start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(mimetype.as_bytes()).unwrap();
        Self(zip)
    }

    fn add(&mut self, name: &str, bytes: &[u8]) -> &mut Self {
        self.0
            .start_file(
                name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .unwrap();
        self.0.write_all(bytes).unwrap();
        self
    }

    fn finish(self) -> Vec<u8> {
        self.0.finish().unwrap().into_inner()
    }
}

/// A ZIP whose mimetype belongs to something else entirely.
pub fn wrong_mimetype_zip() -> Vec<u8> {
    let mut a = Archive::new("application/zip");
    a.add("stack.xml", b"<image w=\"1\" h=\"1\"><stack/></image>");
    a.finish()
}

// ---------------------------------------------------------------- ORA

/// One layer of an ORA fixture, in the order the specification wants them:
/// uppermost first.
pub struct OraLayer {
    name: String,
    width: u32,
    height: u32,
    pixel: [u8; 4],
    x: i64,
    y: i64,
    opacity: f32,
    visible: bool,
    op: String,
    /// The body of the `umber/effects/<i>.ron` entry, when there is to be one.
    effects_record: Option<String>,
    /// Write `umber-effects` on the `<layer>`.
    ///
    /// Separate from the record so a fixture can name one that is not in the
    /// archive — the case a reader has to survive and cannot produce for
    /// itself.
    effects_named: bool,
}

impl OraLayer {
    /// A solid rectangle of one colour, placed at (1,1) when it is smaller
    /// than the canvas so that offset handling is always under test.
    pub fn new(name: &str, width: u32, height: u32, pixel: &[u8; 4]) -> Self {
        Self {
            name: name.to_string(),
            width,
            height,
            pixel: *pixel,
            x: 1,
            y: 1,
            opacity: 1.0,
            visible: true,
            op: "svg:src-over".to_string(),
            effects_record: None,
            effects_named: false,
        }
    }

    pub fn op(mut self, op: &str) -> Self {
        self.op = op.to_string();
        self
    }

    /// Carry `umber-effects`, pointing at a record holding this exact text.
    ///
    /// The text rather than a `Vec<Effect>`, because the cases worth testing
    /// are the ones a serialiser cannot produce: a record with no `kind`, a
    /// kind this build has never heard of, bytes that are not RON at all.
    pub fn effects(mut self, record: &str) -> Self {
        self.effects_record = Some(record.to_string());
        self.effects_named = true;
        self
    }

    /// Carry `umber-effects` naming a record that is not in the archive.
    pub fn effects_named_but_absent(mut self) -> Self {
        self.effects_named = true;
        self
    }

    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    fn xml(&self, src: &str, effects_src: Option<&str>) -> String {
        let effects = match effects_src {
            Some(path) => format!(" umber-effects=\"{path}\""),
            None => String::new(),
        };
        format!(
            "<layer name=\"{}\" src=\"{src}\" x=\"{}\" y=\"{}\" opacity=\"{}\" visibility=\"{}\" composite-op=\"{}\"{effects}/>",
            self.name,
            self.x,
            self.y,
            self.opacity,
            if self.visible { "visible" } else { "hidden" },
            self.op,
        )
    }
}

pub fn ora(width: u32, height: u32, layers: &[OraLayer]) -> Vec<u8> {
    let mut archive = Archive::new("image/openraster");
    let mut body = String::new();
    for (i, layer) in layers.iter().enumerate() {
        let src = format!("data/layer{i}.png");
        // The writer's own numbering, so a fixture and a saved document name
        // the record the same way.
        let effects_src = format!("umber/effects/{i:03}.ron");
        if let Some(record) = &layer.effects_record {
            archive.add(&effects_src, record.as_bytes());
        }
        body += &layer.xml(&src, layer.effects_named.then_some(effects_src.as_str()));
        let png = png_rgba(
            layer.width,
            layer.height,
            &solid(layer.width, layer.height, &layer.pixel),
        );
        archive.add(&src, &png);
    }
    let xml = format!("<image w=\"{width}\" h=\"{height}\"><stack>{body}</stack></image>");
    archive.add("stack.xml", xml.as_bytes());
    archive.add(
        "mergedimage.png",
        &png_rgba(width, height, &solid(width, height, &[9, 9, 9, 255])),
    );
    archive.finish()
}

/// Two layers inside a hidden group at half opacity.
pub fn ora_with_group() -> Vec<u8> {
    let mut archive = Archive::new("image/openraster");
    archive.add("data/a.png", &png_rgba(1, 1, &[255, 255, 255, 255]));
    archive.add("data/b.png", &png_rgba(1, 1, &[0, 0, 0, 255]));
    let xml = "<image w=\"2\" h=\"2\"><stack>\
        <stack name=\"Ink\" opacity=\"0.5\" visibility=\"hidden\">\
        <layer name=\"A\" src=\"data/a.png\"/>\
        <layer name=\"B\" src=\"data/b.png\"/>\
        </stack></stack></image>";
    archive.add("stack.xml", xml.as_bytes());
    archive.finish()
}

/// An ORA whose first element is a **self-closing** empty group.
///
/// `<stack/>` never produces an `End` event, so the reader's depth bookkeeping
/// has a branch of its own for it. Umber's writer does not emit that form —
/// which is exactly why this exists: it arrives from other applications, and
/// nothing else in the suite reaches that branch.
pub fn ora_with_empty_group() -> Vec<u8> {
    let mut archive = Archive::new("image/openraster");
    archive.add("data/a.png", &png_rgba(1, 1, &[255, 0, 0, 255]));
    let xml = "<image w=\"1\" h=\"1\"><stack>        <stack name=\"Empty\"/>        <layer name=\"After\" src=\"data/a.png\"/>        </stack></image>";
    archive.add("stack.xml", xml.as_bytes());
    archive.finish()
}

// ---------------------------------------------------------------- KRA

/// One mask of a Krita fixture, hanging off a [`KraLayer`].
///
/// Written from Krita's own `kis_kra_savexml_visitor.cpp` and
/// `kis_kra_save_visitor.cpp`: the element is `<mask>` inside `<masks>`, the
/// kind is in `nodetype` (**not** `type`), and the binary data of a
/// transparency mask is its *selection*, under `<filename>.pixelselection`
/// with the byte outside the tiles in `<filename>.pixelselection.defaultpixel`.
pub struct KraMask {
    name: String,
    node_type: &'static str,
    visible: bool,
    x: i64,
    y: i64,
    /// Coverage bytes to store, as `(x, y, value)`; everywhere else takes
    /// `default`.
    pixels: Vec<(usize, usize, u8)>,
    default: u8,
    /// Whether the `.pixelselection` entry is written at all. Krita omits it
    /// for a mask whose selection is empty.
    data: bool,
    /// Whether the element carries a `filename` attribute.
    named: bool,
}

impl KraMask {
    /// A visible transparency mask that reveals nothing but the pixels it is
    /// given — Krita's own default for a pixel selection.
    pub fn transparency(name: &str) -> Self {
        Self {
            name: name.to_string(),
            node_type: "transparencymask",
            visible: true,
            x: 0,
            y: 0,
            pixels: Vec::new(),
            default: 0,
            data: true,
            named: true,
        }
    }

    pub fn kind(mut self, node_type: &'static str) -> Self {
        self.node_type = node_type;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn at(mut self, x: i64, y: i64) -> Self {
        self.x = x;
        self.y = y;
        self
    }

    pub fn coverage(mut self, x: usize, y: usize, value: u8) -> Self {
        self.pixels.push((x, y, value));
        self
    }

    pub fn default_coverage(mut self, value: u8) -> Self {
        self.default = value;
        self
    }

    /// A mask whose `.pixelselection` entry is not in the archive.
    pub fn without_data(mut self) -> Self {
        self.data = false;
        self
    }

    /// A mask carrying no `filename` attribute, so it names no pixel data.
    pub fn unnamed(mut self) -> Self {
        self.named = false;
        self.data = false;
        self
    }

    fn xml(&self, filename: &str) -> String {
        let filename = if self.named {
            format!(" filename=\"{filename}\"")
        } else {
            String::new()
        };
        format!(
            "<mask name=\"{}\"{filename} nodetype=\"{}\" visible=\"{}\" \
             locked=\"0\" x=\"{}\" y=\"{}\"/>",
            self.name,
            self.node_type,
            if self.visible { 1 } else { 0 },
            self.x,
            self.y,
        )
    }

    /// One 64×64 tile at the origin, a single plane of coverage.
    fn tile_file(&self) -> Vec<u8> {
        let mut plane = vec![0u8; 64 * 64];
        for (x, y, value) in &self.pixels {
            plane[y * 64 + x] = *value;
        }
        tile_file(1, &plane, false)
    }
}

/// One layer of a Krita fixture. Uppermost first, as `maindoc.xml` orders them.
pub struct KraLayer {
    name: String,
    node_type: &'static str,
    opacity: u8,
    visible: bool,
    op: String,
    x: i64,
    y: i64,
    pixels: Vec<(usize, usize, [u8; 4])>,
    compress: bool,
    masks: Vec<KraMask>,
}

impl KraLayer {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            node_type: "paintlayer",
            opacity: 255,
            visible: true,
            op: "normal".to_string(),
            x: 0,
            y: 0,
            pixels: Vec::new(),
            compress: false,
            masks: Vec::new(),
        }
    }

    pub fn vector(mut self) -> Self {
        self.node_type = "vectorlayer";
        self
    }

    pub fn pixel(mut self, x: usize, y: usize, rgba: [u8; 4]) -> Self {
        self.pixels.push((x, y, rgba));
        self
    }

    pub fn at(mut self, x: i64, y: i64) -> Self {
        self.x = x;
        self.y = y;
        self
    }

    pub fn opacity(mut self, opacity: u8) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn op(mut self, op: &str) -> Self {
        self.op = op.to_string();
        self
    }

    pub fn compressed(mut self) -> Self {
        self.compress = true;
        self
    }

    /// Attach a mask, uppermost first — the order Krita writes them in.
    pub fn mask(mut self, mask: KraMask) -> Self {
        self.masks.push(mask);
        self
    }

    /// An ordinary visible transparency mask revealing one pixel.
    pub fn masked(self) -> Self {
        self.mask(KraMask::transparency("Mask").coverage(0, 0, 255))
    }

    fn group(name: &str) -> Self {
        let mut layer = Self::new(name);
        layer.node_type = "grouplayer";
        layer
    }

    /// One 64×64 tile at the origin, planar BGRA, optionally LZF-compressed.
    fn tile_file(&self) -> Vec<u8> {
        const N: usize = 64 * 64;
        let mut planes = vec![0u8; N * 4];
        for (x, y, rgba) in &self.pixels {
            let i = y * 64 + x;
            planes[i] = rgba[2]; // blue plane first
            planes[N + i] = rgba[1];
            planes[2 * N + i] = rgba[0];
            planes[3 * N + i] = rgba[3];
        }
        tile_file(4, &planes, self.compress)
    }

    fn xml(&self, filename: &str, children: &str) -> String {
        let open = format!(
            "<layer name=\"{}\" filename=\"{filename}\" nodetype=\"{}\" x=\"{}\" y=\"{}\" opacity=\"{}\" visible=\"{}\" compositeop=\"{}\" colorspacename=\"RGBA\"",
            self.name,
            self.node_type,
            self.x,
            self.y,
            self.opacity,
            if self.visible { 1 } else { 0 },
            self.op,
        );
        let mut inner = String::new();
        if !self.masks.is_empty() {
            inner += "<masks>";
            for (i, mask) in self.masks.iter().enumerate() {
                inner += &mask.xml(&mask_filename(filename, i));
            }
            inner += "</masks>";
        }
        inner += children;
        if inner.is_empty() {
            format!("{open}/>")
        } else {
            format!("{open}>{inner}</layer>")
        }
    }

    /// Write every mask's binary data under the layer directory.
    fn add_masks(&self, archive: &mut Archive, document: &str, filename: &str) {
        for (i, mask) in self.masks.iter().enumerate() {
            if !mask.data {
                continue;
            }
            let path = format!(
                "{document}/layers/{}.pixelselection",
                mask_filename(filename, i)
            );
            archive.add(&path, &mask.tile_file());
            archive.add(&format!("{path}.defaultpixel"), &[mask.default]);
        }
    }
}

/// Krita numbers every node in one sequence; the fixtures only need the names
/// to be unique and to differ from the layers'.
fn mask_filename(layer: &str, index: usize) -> String {
    format!("{layer}mask{index}")
}

/// A bare Krita tile file, for a test that drives the tile reader directly.
pub fn kra_tile_file(pixel_size: u32, planes: &[u8]) -> Vec<u8> {
    tile_file(pixel_size, planes, false)
}

/// A Krita tile file: the five-line header, then one tile at the origin.
///
/// `pixel_size` is what the header declares and what the reader is required to
/// agree with — 4 for a layer's BGRA planes, 1 for a mask's selection.
fn tile_file(pixel_size: u32, planes: &[u8], compress: bool) -> Vec<u8> {
    let body = if compress {
        lzf_compress(planes)
    } else {
        planes.to_vec()
    };
    let flag: u8 = if compress { 1 } else { 0 };

    let mut out =
        format!("VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE {pixel_size}\nDATA 1\n")
            .into_bytes();
    // The declared size counts the flag byte as well as the payload.
    out.extend_from_slice(format!("0,0,LZF,{}\n", body.len() + 1).as_bytes());
    out.push(flag);
    out.extend_from_slice(&body);
    out
}

/// A valid LZF stream, using back-references for runs.
///
/// Only used to build fixtures, but deliberately not a literals-only encoder:
/// a tile that decodes correctly without ever taking the back-reference path
/// would leave the interesting half of the decompressor untested.
fn lzf_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut literals: Vec<u8> = Vec::new();
    let mut i = 0;

    fn flush(out: &mut Vec<u8>, literals: &mut Vec<u8>) {
        for chunk in literals.chunks(32) {
            out.push(chunk.len() as u8 - 1);
            out.extend_from_slice(chunk);
        }
        literals.clear();
    }

    while i < data.len() {
        // A run of the byte just emitted encodes as a back-reference one byte
        // behind, which the decompressor copies over itself.
        let run = if i > 0 {
            data[i..].iter().take_while(|&&b| b == data[i - 1]).count()
        } else {
            0
        };
        if run >= 3 {
            flush(&mut out, &mut literals);
            let mut left = run;
            while left >= 3 {
                let take = left.min(264);
                let l = take - 2;
                if l < 7 {
                    out.push((l as u8) << 5);
                } else {
                    out.push(7u8 << 5);
                    out.push((l - 7) as u8);
                }
                out.push(0); // distance - 1 = 0, i.e. one byte back
                left -= take;
            }
            literals.extend_from_slice(&data[i + run - left..i + run]);
            i += run;
        } else {
            literals.push(data[i]);
            i += 1;
        }
    }
    flush(&mut out, &mut literals);
    out
}

fn kra_archive(width: u32, height: u32, colourspace: &str, layers: &[KraLayer]) -> Vec<u8> {
    let mut archive = Archive::new("application/x-krita");
    let mut body = String::new();
    for (i, layer) in layers.iter().enumerate() {
        let filename = format!("layer{i}");
        body += &layer.xml(&filename, "");
        if layer.node_type == "paintlayer" {
            archive.add(&format!("Fixture/layers/{filename}"), &layer.tile_file());
        }
        layer.add_masks(&mut archive, "Fixture", &filename);
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <DOC xmlns=\"http://www.calligra.org/DTD/krita\" syntaxVersion=\"2\">\
         <IMAGE name=\"Fixture\" mime=\"application/x-kra\" width=\"{width}\" height=\"{height}\" \
         colorspacename=\"{colourspace}\" profile=\"sRGB-elle-V2-srgbtrc.icc\">\
         <layers>{body}</layers></IMAGE></DOC>"
    );
    archive.add("maindoc.xml", xml.as_bytes());
    archive.add(
        "mergedimage.png",
        &png_rgba(width, height, &solid(width, height, &[7, 7, 7, 255])),
    );
    archive.finish()
}

pub fn kra(width: u32, height: u32, layers: &[KraLayer]) -> Vec<u8> {
    kra_archive(width, height, "RGBA", layers)
}

/// A document Krita wrote in a colour space this importer will not read.
pub fn kra_in_colourspace(colourspace: &str) -> Vec<u8> {
    kra_archive(64, 64, colourspace, &[KraLayer::new("Paint")])
}

/// A hidden, half-opaque group holding two paint layers, one of them masked.
///
/// Also the only fixture whose `<layer>` elements are not self-closing, which
/// is the code path every group layer in a real document takes.
pub fn kra_with_group() -> Vec<u8> {
    let group = KraLayer::group("Ink").opacity(128).hidden();
    let inside = [
        KraLayer::new("Lines").pixel(0, 0, [1, 1, 1, 255]).masked(),
        KraLayer::new("Fills").pixel(1, 0, [2, 2, 2, 255]),
    ];
    let outside = KraLayer::new("Paper").pixel(2, 0, [3, 3, 3, 255]);

    let mut archive = Archive::new("application/x-krita");
    let mut children = String::new();
    for (i, layer) in inside.iter().enumerate() {
        let filename = format!("inner{i}");
        children += &layer.xml(&filename, "");
        archive.add(&format!("Fixture/layers/{filename}"), &layer.tile_file());
        layer.add_masks(&mut archive, "Fixture", &filename);
    }
    let mut body = group.xml("group0", &format!("<layers>{children}</layers>"));
    body += &outside.xml("outer0", "");
    archive.add("Fixture/layers/outer0", &outside.tile_file());

    let xml = format!(
        "<DOC xmlns=\"http://www.calligra.org/DTD/krita\" syntaxVersion=\"2\">         <IMAGE name=\"Fixture\" mime=\"application/x-kra\" width=\"64\" height=\"64\"          colorspacename=\"RGBA\" profile=\"sRGB-elle-V2-srgbtrc.icc\">         <layers>{body}</layers></IMAGE></DOC>"
    );
    archive.add("maindoc.xml", xml.as_bytes());
    archive.add(
        "mergedimage.png",
        &png_rgba(64, 64, &solid(64, 64, &[7, 7, 7, 255])),
    );
    archive.finish()
}

/// A transparency mask on the **group** rather than on a layer.
///
/// Groups are flattened away by this reader, so a mask on one has nowhere to
/// go — and unlike the layer case that is a real change to the picture.
pub fn kra_with_masked_group() -> Vec<u8> {
    let group = KraLayer::group("Ink").mask(KraMask::transparency("Group mask"));
    let inside = KraLayer::new("Lines").pixel(0, 0, [1, 1, 1, 255]);

    let mut archive = Archive::new("application/x-krita");
    let children = inside.xml("inner0", "");
    archive.add("Fixture/layers/inner0", &inside.tile_file());
    let body = group.xml("group0", &format!("<layers>{children}</layers>"));
    group.add_masks(&mut archive, "Fixture", "group0");

    let xml = format!(
        "<DOC xmlns=\"http://www.calligra.org/DTD/krita\" syntaxVersion=\"2\">\
         <IMAGE name=\"Fixture\" mime=\"application/x-kra\" width=\"64\" height=\"64\" \
         colorspacename=\"RGBA\" profile=\"sRGB-elle-V2-srgbtrc.icc\">\
         <layers>{body}</layers></IMAGE></DOC>"
    );
    archive.add("maindoc.xml", xml.as_bytes());
    archive.add(
        "mergedimage.png",
        &png_rgba(64, 64, &solid(64, 64, &[7, 7, 7, 255])),
    );
    archive.finish()
}

/// A `<masks>` element holding a child this reader has no arm for at all.
///
/// Stands in for the reading of the format being wrong about the *element*
/// rather than about an attribute: if Krita named its mask elements anything
/// but `<mask>`, this is the shape the reader would meet, and nothing here
/// would fire. The layer must still report that it had a mask.
pub fn kra_with_unreadable_mask_element() -> Vec<u8> {
    let layer = KraLayer::new("Lines").pixel(0, 0, [1, 1, 1, 255]);
    let mut archive = Archive::new("application/x-krita");
    archive.add("Fixture/layers/layer0", &layer.tile_file());

    let body = "<layer name=\"Lines\" filename=\"layer0\" nodetype=\"paintlayer\" x=\"0\" y=\"0\" \
                opacity=\"255\" visible=\"1\" compositeop=\"normal\" colorspacename=\"RGBA\">\
                <masks><transparencymask name=\"Mask\" filename=\"mask1\"/></masks></layer>";
    let xml = format!(
        "<DOC xmlns=\"http://www.calligra.org/DTD/krita\" syntaxVersion=\"2\">\
         <IMAGE name=\"Fixture\" mime=\"application/x-kra\" width=\"64\" height=\"64\" \
         colorspacename=\"RGBA\" profile=\"sRGB-elle-V2-srgbtrc.icc\">\
         <layers>{body}</layers></IMAGE></DOC>"
    );
    archive.add("maindoc.xml", xml.as_bytes());
    archive.add(
        "mergedimage.png",
        &png_rgba(64, 64, &solid(64, 64, &[7, 7, 7, 255])),
    );
    archive.finish()
}

pub fn kra_with_vector_layer() -> Vec<u8> {
    kra(
        64,
        64,
        &[
            KraLayer::new("Text").vector(),
            KraLayer::new("Paint").pixel(0, 0, [1, 2, 3, 255]),
        ],
    )
}

// ---------------------------------------------------------------- PSD
//
// Written from Adobe's layer-record layout, and calibrated against the real
// Photoshop files the `psd` crate ships as test fixtures: the flag bit that
// means "hidden", the clipping byte, and the bottom-to-top record order were
// all read back out of those files before being written here. A fixture that
// merely agreed with our own reader would prove nothing.

/// A layer mask, as the "Layer mask / adjustment layer data" block describes
/// it.
///
/// Adobe's 20-byte form: the mask's own rectangle, the byte outside it, a flags
/// byte, and two of padding. The mask's pixels then arrive as channel `-2`,
/// alongside the layer's own three colours and its alpha — **and in the mask's
/// rectangle, not the layer's**, which is exactly why that block has to be read
/// before the bytes mean anything.
pub struct PsdMask {
    /// top, left, bottom, right; the last two exclusive.
    rect: (i32, i32, i32, i32),
    /// Outside the rectangle.
    default: u8,
    /// Bit 1 set means Photoshop has switched the mask off.
    flags: u8,
    /// A flat coverage for the whole rectangle.
    value: u8,
    /// Store the channel PackBits-compressed rather than raw.
    rle: bool,
}

impl PsdMask {
    pub fn new(rect: (i32, i32, i32, i32), value: u8) -> Self {
        Self {
            rect,
            default: 0,
            flags: 0,
            value,
            rle: false,
        }
    }

    /// A mask Photoshop has switched off, which bounds nothing there.
    pub fn disabled(mut self) -> Self {
        self.flags |= 0b10;
        self
    }

    /// Store the mask channel RLE-compressed, as Photoshop ordinarily does.
    ///
    /// The bytes written are a per-scanline length table over the **mask's**
    /// rows followed by one PackBits run each — the real layout, which is the
    /// point: it is shorter than the table `psd` 0.3.5 skips, and that is what
    /// makes the file undecodable there. See [`super::photoshop`]'s docs.
    pub fn compressed(mut self) -> Self {
        self.rle = true;
        self
    }

    fn rows(&self) -> usize {
        let (top, _, bottom, _) = self.rect;
        (bottom - top) as usize
    }

    fn columns(&self) -> usize {
        let (_, left, _, right) = self.rect;
        (right - left) as usize
    }

    /// The channel plane, in whichever form [`Self::rle`] asks for.
    ///
    /// PackBits: a two-byte length per scanline, then the runs. A row of one
    /// repeated byte is `(1 - columns) as i8` followed by that byte, which is
    /// two bytes a row — comfortably shorter than the `2 * layer_height` table
    /// `psd` 0.3.5 skips past.
    fn plane(&self) -> Vec<u8> {
        if !self.rle {
            return std::iter::repeat_n(self.value, self.rows() * self.columns()).collect();
        }
        let run = [(1i32 - self.columns() as i32) as i8 as u8, self.value];
        let mut out = Vec::new();
        for _ in 0..self.rows() {
            out.extend_from_slice(&(run.len() as u16).to_be_bytes());
        }
        for _ in 0..self.rows() {
            out.extend_from_slice(&run);
        }
        out
    }

    /// The compression marker that goes in front of the plane.
    fn compression(&self) -> u16 {
        u16::from(self.rle)
    }
}

/// One layer of a PSD fixture. Bottom first, the order the file stores them in.
pub struct PsdLayerSpec {
    name: String,
    pixel: [u8; 4],
    opacity: u8,
    visible: bool,
    clipped: bool,
    blend: [u8; 4],
    mask: Option<PsdMask>,
}

impl PsdLayerSpec {
    pub fn new(name: &str, pixel: [u8; 4]) -> Self {
        Self {
            name: name.to_string(),
            pixel,
            opacity: 255,
            visible: true,
            clipped: false,
            blend: *b"norm",
            mask: None,
        }
    }

    pub fn mask(mut self, mask: PsdMask) -> Self {
        self.mask = Some(mask);
        self
    }

    pub fn opacity(mut self, opacity: u8) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn clipped(mut self) -> Self {
        self.clipped = true;
        self
    }

    pub fn blend(mut self, key: [u8; 4]) -> Self {
        self.blend = key;
        self
    }
}

fn psd_header(width: u32, height: u32, channels: u16, depth: u16, colour_mode: u16) -> Vec<u8> {
    let mut out = b"8BPS".to_vec();
    out.extend_from_slice(&1u16.to_be_bytes()); // version 1; PSB is 2
    out.extend_from_slice(&[0; 6]); // reserved
    out.extend_from_slice(&channels.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&depth.to_be_bytes());
    out.extend_from_slice(&colour_mode.to_be_bytes());
    out
}

/// A layered 8-bit RGB PSD, every layer a solid colour covering the canvas.
pub fn psd(width: u32, height: u32, layers: &[PsdLayerSpec]) -> Vec<u8> {
    let pixels = width as usize * height as usize;

    let mut records = Vec::new();
    let mut channel_data = Vec::new();
    for layer in layers {
        // Rectangle: top, left, bottom, right — the last two exclusive.
        records.extend_from_slice(&0i32.to_be_bytes());
        records.extend_from_slice(&0i32.to_be_bytes());
        records.extend_from_slice(&(height as i32).to_be_bytes());
        records.extend_from_slice(&(width as i32).to_be_bytes());
        // A layer mask arrives as one more channel, `-2`, and its plane is the
        // size of the *mask's* rectangle rather than the layer's.
        let channels: &[i16] = match layer.mask {
            Some(_) => &[0, 1, 2, -1, -2],
            None => &[0, 1, 2, -1],
        };
        records.extend_from_slice(&(channels.len() as u16).to_be_bytes()); // channel count
        for id in channels {
            records.extend_from_slice(&id.to_be_bytes());
            let plane = match (*id, &layer.mask) {
                (-2, Some(mask)) => mask.plane().len(),
                _ => pixels,
            };
            // Two bytes of compression marker plus the plane itself.
            records.extend_from_slice(&(plane as u32 + 2).to_be_bytes());
        }
        records.extend_from_slice(b"8BIM");
        records.extend_from_slice(&layer.blend);
        records.push(layer.opacity);
        records.push(u8::from(layer.clipped)); // 0 = base, 1 = clipped to below
        records.push(if layer.visible { 0 } else { 0b10 }); // bit 1 set = hidden
        records.push(0); // filler

        let mut extra = Vec::new();
        match &layer.mask {
            None => extra.extend_from_slice(&0u32.to_be_bytes()), // no mask data
            Some(mask) => {
                // Adobe's 20-byte form of the layer mask block.
                extra.extend_from_slice(&20u32.to_be_bytes());
                let (top, left, bottom, right) = mask.rect;
                for edge in [top, left, bottom, right] {
                    extra.extend_from_slice(&edge.to_be_bytes());
                }
                extra.push(mask.default);
                extra.push(mask.flags);
                extra.extend_from_slice(&[0, 0]); // padding to twenty
            }
        }
        extra.extend_from_slice(&0u32.to_be_bytes()); // no blending ranges
        let name = layer.name.as_bytes();
        extra.push(name.len() as u8);
        extra.extend_from_slice(name);
        while (extra.len() - 8) % 4 != 0 {
            extra.push(0); // the Pascal string pads to a multiple of four
        }
        records.extend_from_slice(&(extra.len() as u32).to_be_bytes());
        records.extend_from_slice(&extra);

        for component in [0usize, 1, 2, 3] {
            channel_data.extend_from_slice(&0u16.to_be_bytes()); // raw
            channel_data.extend(std::iter::repeat_n(layer.pixel[component], pixels));
        }
        if let Some(mask) = &layer.mask {
            channel_data.extend_from_slice(&mask.compression().to_be_bytes());
            channel_data.extend_from_slice(&mask.plane());
        }
    }

    let mut layer_info = (layers.len() as i16).to_be_bytes().to_vec();
    layer_info.extend_from_slice(&records);
    layer_info.extend_from_slice(&channel_data);

    let mut section = (layer_info.len() as u32).to_be_bytes().to_vec();
    section.extend_from_slice(&layer_info);

    let mut out = psd_header(width, height, 4, 8, 3);
    out.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    out.extend_from_slice(&0u32.to_be_bytes()); // image resources
    out.extend_from_slice(&(section.len() as u32).to_be_bytes());
    out.extend_from_slice(&section);
    // Image data section: the flattened composite, planar, one plane per
    // channel. Whatever is on top wins, which is enough for a fixture.
    out.extend_from_slice(&0u16.to_be_bytes());
    let top = layers.last().map(|l| l.pixel).unwrap_or([0, 0, 0, 0]);
    for component in [0usize, 1, 2, 3] {
        out.extend(std::iter::repeat_n(top[component], pixels));
    }
    out
}

/// A PSD with no layer records at all — a flattened save.
pub fn psd_flattened(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let pixels = width as usize * height as usize;
    let mut out = psd_header(width, height, 3, 8, 3);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // empty layer and mask section
    out.extend_from_slice(&0u16.to_be_bytes()); // raw
    for component in 0..3 {
        for i in 0..pixels {
            out.push(rgb[i * 3 + component]);
        }
    }
    out
}

/// A well-formed PSD at a bit depth this importer refuses.
pub fn psd_with_depth(depth: u16) -> Vec<u8> {
    let mut out = psd_header(1, 1, 4, depth, 3);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&[0; 8]);
    out
}

/// A CMYK document, which is not a colour space Umber paints in.
pub fn psd_in_cmyk() -> Vec<u8> {
    let mut out = psd_header(1, 1, 4, 8, 4);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&[0; 4]);
    out
}

/// A PSB — the large-document format, which is header version 2.
pub fn psb() -> Vec<u8> {
    let mut out = psd(1, 1, &[PsdLayerSpec::new("Layer", [0, 0, 0, 255])]);
    out[4..6].copy_from_slice(&2u16.to_be_bytes());
    out
}

/// A `stack.xml` naming a layer file that is not in the archive.
pub fn ora_missing_layer_data() -> Vec<u8> {
    let mut archive = Archive::new("image/openraster");
    archive.add(
        "stack.xml",
        b"<image w=\"2\" h=\"2\"><stack><layer name=\"Gone\" src=\"data/gone.png\"/></stack></image>",
    );
    archive.add(
        "mergedimage.png",
        &png_rgba(2, 2, &solid(2, 2, &[1, 2, 3, 255])),
    );
    archive.finish()
}

// ---------------------------------------------------------- Clip Studio

/// One layer of a `.clip` fixture.
///
/// The tree is built from these bottom first, which is the order Clip Studio's
/// own `LayerNextIndex` chain runs in — see [`super::clipstudio`]'s docs for how
/// that direction was established, since building the fixture the other way
/// round would make every test here agree with a reader that inverts documents.
pub struct ClipLayer {
    pub name: &'static str,
    /// `LayerType`. `1` is an ordinary raster layer.
    pub kind: i64,
    pub folder: bool,
    pub visible: bool,
    pub locked: bool,
    pub clipped: bool,
    /// `LayerOpacity`, out of 256.
    pub opacity: i64,
    pub composite: i64,
    /// Canvas-sized straight-alpha RGBA. `None` gives the layer no bitmap at
    /// all, which is what a folder has.
    pub pixels: Option<Vec<u8>>,
    /// Canvas-sized coverage, `0` hiding and `255` revealing.
    pub mask: Option<Vec<u8>>,
    /// What an absent mask block holds. `Some(255)` is what Clip Studio writes,
    /// because a mask starts revealing everything.
    pub mask_fill: Option<u8>,
    /// What an absent *colour* block holds. `None` is what every raster layer
    /// in every real file states; a `Some` is the shape the reader refuses.
    pub pixel_fill: Option<u8>,
    /// The bitmap's own size, `None` for the canvas's. Smaller is the case
    /// every real sample happens not to have.
    pub bitmap_size: Option<(u32, u32)>,
    /// Where the bitmap's top-left corner sits on the canvas. Split across the
    /// three column pairs a real file splits it across.
    pub offset: (i64, i64),
    /// Whether the mask's own eye is on — `LayerVisibility`'s second bit,
    /// which a real file only sets on a layer that has a mask.
    pub mask_visible: bool,
    /// `SpecialRenderType`. 20 marks the Paper layer; everything else is 0.
    pub special_render: i64,
    /// Write the mipmap bookkeeping and withhold the pixels, which is what a
    /// vector layer in a real `.clip` looks like.
    pub withhold_chunk: bool,
    /// The **source** picture of a placed image, written as a second mipmap
    /// chain named by `ResizableOriginalMipmap`.
    ///
    /// Its chunk is **present**, and that asymmetry is the whole point: the
    /// real document this was built from has a full render chain whose every
    /// level's chunk is absent, beside a resizable-original chain whose base
    /// level's chunk holds 11 MB of the artist's picture. A fixture that
    /// withheld both would be a vector layer wearing a different column, and
    /// would pass a reader that never looked at the column at all.
    pub resizable_pixels: Option<Vec<u8>>,
    /// The flat colour a Paper layer draws, as ordinary sRGB bytes. Stored
    /// scaled up to `u32` by [`scale_channel`], which is what a real file does.
    pub draw_colour: [u8; 3],
    pub children: Vec<ClipLayer>,
}

impl ClipLayer {
    /// A raster layer of one flat colour.
    pub fn flat(name: &'static str, width: u32, height: u32, rgba: [u8; 4]) -> Self {
        Self {
            name,
            kind: 1,
            folder: false,
            visible: true,
            locked: false,
            clipped: false,
            opacity: 256,
            composite: 0,
            pixels: Some(
                std::iter::repeat_n(rgba, width as usize * height as usize)
                    .flatten()
                    .collect(),
            ),
            mask: None,
            mask_fill: Some(255),
            pixel_fill: None,
            bitmap_size: None,
            offset: (0, 0),
            mask_visible: true,
            special_render: 0,
            withhold_chunk: false,
            resizable_pixels: None,
            draw_colour: [0, 0, 0],
            children: Vec::new(),
        }
    }

    /// A **vector** layer: `LayerType` 0, and no pixels in the file.
    ///
    /// That is what a real one is. Clip Studio keeps the strokes and rasterises
    /// them on demand, so it writes the whole mipmap chain and never fills the
    /// external chunks the chain points at — which is `withhold_chunk`, and is
    /// why the layer fails at the reader's **second** drop site rather than its
    /// first. That distinction is the whole point of the fixture: guarding only
    /// the first site left every real vector layer still reporting damage.
    ///
    /// **This comment used to say "no pixels anywhere … which is exactly what
    /// `pixels: None` produces here", and both halves were false**: `flat`
    /// gives it a buffer, and it is the chunk rather than the bitmap that is
    /// withheld. The property it misdescribed is precisely the one the fixture
    /// exists to have, so the wrong sentence was worse than none.
    /// [`Self::no_render_bitmap`] is the other half of the pair.
    pub fn vector(name: &'static str, width: u32, height: u32) -> Self {
        let mut layer = Self::flat(name, width, height, [0, 0, 0, 0]);
        layer.kind = 0;
        layer.withhold_chunk = true;
        layer
    }

    /// A layer naming **no render mipmap at all**, which is the reader's
    /// *first* drop site.
    ///
    /// Every other fixture here writes a chain and withholds its chunk, so all
    /// of them fail at the second site — which left the first one reachable by
    /// no test at all. Demonstrated by mutation: replacing that site's whole
    /// sentence with a marker left 1,120 tests green.
    ///
    /// A real file does this for a layer Clip Studio has never rendered.
    /// `pixels: None` is what produces it: [`ClipBuild::chain`] writes
    /// `LayerRenderMipmap` 0 when there is no bitmap to build a chain around.
    pub fn no_render_bitmap(name: &'static str, kind: i64) -> Self {
        let mut layer = Self::flat(name, 1, 1, [0, 0, 0, 0]);
        layer.kind = kind;
        layer.pixels = None;
        layer
    }

    /// A **placed image**: an image imported into the document and left
    /// resizable rather than rasterised.
    ///
    /// Built to the shape of the real document that provoked it, because a
    /// convenient fixture would have proved nothing. `LayerType` is 0, exactly
    /// as a vector layer's is — that collision is the bug — and the render
    /// chain is written whole with its chunk **withheld**, which is what makes
    /// the layer fail at the second drop site rather than the first, as a real
    /// one does. What distinguishes it is `ResizableOriginalMipmap`, naming a
    /// second chain whose pixels are really there.
    ///
    /// Those pixels are deliberately **not** the picture the layer would show:
    /// the source is placed by a transform this reader does not read, so a test
    /// that expected them on the canvas would be asserting the wrong thing.
    pub fn placed_image(name: &'static str, width: u32, height: u32) -> Self {
        let mut layer = Self::vector(name, width, height);
        layer.resizable_pixels = Some(
            std::iter::repeat_n([200u8, 40, 60, 255], width as usize * height as usize)
                .flatten()
                .collect(),
        );
        layer
    }

    /// The **Paper** layer: the flat sheet Clip Studio puts under a new
    /// document.
    ///
    /// It carries a colour and **no bitmap at all**, which is the whole reason
    /// it needs its own constructor: `flat` would give it pixels, and a paper
    /// with pixels is not the thing that was going wrong. A real one names a
    /// `LayerRenderMipmap` whose offscreen is absent, which is exactly what
    /// `pixels: None` produces here.
    pub fn paper(rgb: [u8; 3]) -> Self {
        let mut layer = Self::folder("Paper", Vec::new());
        layer.folder = false;
        layer.kind = 1584;
        layer.special_render = 20;
        layer.draw_colour = rgb;
        layer
    }

    pub fn folder(name: &'static str, children: Vec<ClipLayer>) -> Self {
        Self {
            name,
            kind: 0,
            folder: true,
            visible: true,
            locked: false,
            clipped: false,
            opacity: 256,
            composite: 0,
            pixels: None,
            mask: None,
            mask_fill: None,
            pixel_fill: None,
            bitmap_size: None,
            offset: (0, 0),
            mask_visible: true,
            special_render: 0,
            withhold_chunk: false,
            resizable_pixels: None,
            draw_colour: [0, 0, 0],
            children,
        }
    }

    pub fn kind(mut self, kind: i64) -> Self {
        self.kind = kind;
        self
    }

    pub fn composite(mut self, composite: i64) -> Self {
        self.composite = composite;
        self
    }

    pub fn opacity(mut self, opacity: i64) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn locked(mut self) -> Self {
        self.locked = true;
        self
    }

    pub fn clipped(mut self) -> Self {
        self.clipped = true;
        self
    }

    pub fn mask(mut self, coverage: Vec<u8>) -> Self {
        self.mask = Some(coverage);
        self
    }

    /// A mask Clip Studio has switched off — its eye clear in
    /// `LayerVisibility`, with the mask itself still in the file.
    pub fn mask_hidden(mut self) -> Self {
        self.mask_visible = false;
        self
    }

    pub fn mask_fill(mut self, fill: Option<u8>) -> Self {
        self.mask_fill = fill;
        self
    }

    /// State a colour fill for the blocks this layer does not store, which is
    /// what a Clip Studio *fill* layer carries and what the reader refuses.
    pub fn pixel_fill(mut self, fill: u8) -> Self {
        self.pixel_fill = Some(fill);
        self
    }

    /// A bitmap smaller than the canvas, placed at `offset`.
    ///
    /// `pixels` and `mask` are then that size rather than the canvas's, which
    /// is what a Clip Studio layer drawn in one corner actually holds.
    pub fn placed(mut self, size: (u32, u32), offset: (i64, i64)) -> Self {
        self.bitmap_size = Some(size);
        self.offset = offset;
        self
    }
}

/// Everything the tables need while the tree is being walked.
struct ClipBuild {
    width: u32,
    height: u32,
    layers: Vec<Vec<Value>>,
    mipmaps: Vec<Vec<Value>>,
    infos: Vec<Vec<Value>>,
    offscreens: Vec<Vec<Value>>,
    external: Vec<(Vec<u8>, Vec<u8>)>,
    next_id: i64,
    next_chunk: usize,
}

const CLIP_COLOUR: Packing = Packing {
    first: 1,
    second: 4,
};
const CLIP_MASK: Packing = Packing {
    first: 1,
    second: 0,
};

impl ClipBuild {
    fn id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// A forty-character `extrnlid…` name, which is the shape a real one has.
    fn chunk_name(&mut self) -> Vec<u8> {
        self.next_chunk += 1;
        format!("extrnlid{:032}", self.next_chunk).into_bytes()
    }

    /// Cut a `size`-sized buffer into 256-square blocks and register the
    /// `Offscreen` row, the mipmap chain and the external chunk for it.
    ///
    /// `size` is the **bitmap's** own size, which is the canvas's for an
    /// ordinary layer and smaller for one drawn in a corner — the case that
    /// decides whether the reader clips its blits against the bitmap or only
    /// against the canvas, and the case every real sample happens not to have.
    ///
    /// A block every byte of which equals `fill` is **not stored**, which is
    /// what a real writer does and what makes the `InitColor` path reachable.
    /// The padding outside the bitmap in a partly used block is deliberately
    /// *not* the fill: a real writer leaves whatever was there, and a reader
    /// that trusts it writes rubbish over the canvas.
    fn bitmap(
        &mut self,
        source: &[u8],
        size: (u32, u32),
        packing: Packing,
        fill: Option<u8>,
        // `withhold_chunk` writes the `Offscreen` row but **not** the external
        // chunk it names. That is what a real vector layer looks like: Clip
        // Studio writes the whole mipmap chain for one and never rasterises the
        // strokes into a block, so the row is there and the chunk it points at
        // is not. A different failure from having no chain at all, and the one
        // the artist's own files actually take.
        withhold_chunk: bool,
    ) -> i64 {
        let (width, height) = (size.0 as usize, size.1 as usize);
        let columns = width.div_ceil(256);
        let rows = height.div_ceil(256);
        let mut blocks: Vec<Option<Vec<u8>>> = Vec::with_capacity(columns * rows);
        for row in 0..rows {
            for column in 0..columns {
                // `PADDING` rather than zero, so a reader that copies a block's
                // out-of-bitmap corner onto the canvas is caught rather than
                // flattered.
                const PADDING: u8 = 0x5a;
                let mut block = vec![PADDING; packing.block_len()];
                let mut painted = false;
                for y in 0..256 {
                    for x in 0..256 {
                        let (sx, sy) = (column * 256 + x, row * 256 + y);
                        if sx >= width || sy >= height {
                            continue;
                        }
                        painted = true;
                        let i = y * 256 + x;
                        if packing.second == 0 {
                            block[i] = source[sy * width + sx];
                        } else {
                            // Straight RGBA in, `[alpha plane][BGRX]` out.
                            let px = (sy * width + sx) * 4;
                            block[i] = source[px + 3];
                            let at = 256 * 256 + i * packing.second;
                            block[at] = source[px + 2];
                            block[at + 1] = source[px + 1];
                            block[at + 2] = source[px];
                            block[at + 3] = 0;
                        }
                    }
                }
                // Only a block that lies wholly inside the bitmap can be left
                // out, because only then is every byte of it the artist's.
                let whole = (column + 1) * 256 <= width && (row + 1) * 256 <= height;
                let uniform = |v: u8| block.iter().all(|b| *b == v);
                let absent = whole && (fill.is_some_and(uniform) || (fill.is_none() && uniform(0)));
                blocks.push((!absent && painted).then_some(block));
            }
        }

        let name = self.chunk_name();
        if !withhold_chunk {
            self.external.push((
                name.clone(),
                csblocks::fixture::block_data(&blocks, packing),
            ));
        }

        let offscreen = self.id();
        self.offscreens.push(vec![
            Value::Integer(offscreen),
            Value::Blob(csblocks::fixture::attribute(size.0, size.1, packing, fill)),
            Value::Blob(name),
        ]);
        let info = self.id();
        self.infos
            .push(vec![Value::Integer(info), Value::Integer(offscreen)]);
        let mipmap = self.id();
        self.mipmaps
            .push(vec![Value::Integer(mipmap), Value::Integer(info)]);
        mipmap
    }

    /// One `LayerNextIndex` chain, bottom first, returning the first id.
    fn chain(&mut self, layers: &[ClipLayer]) -> i64 {
        let ids: Vec<i64> = layers.iter().map(|_| self.id()).collect();
        for (i, layer) in layers.iter().enumerate() {
            let first_child = if layer.children.is_empty() {
                0
            } else {
                self.chain(&layer.children)
            };
            let size = layer.bitmap_size.unwrap_or((self.width, self.height));
            let render = match &layer.pixels {
                Some(pixels) => self.bitmap(
                    pixels,
                    size,
                    CLIP_COLOUR,
                    layer.pixel_fill,
                    layer.withhold_chunk,
                ),
                None => 0,
            };
            let mask = match &layer.mask {
                Some(coverage) => self.bitmap(coverage, size, CLIP_MASK, layer.mask_fill, false),
                None => 0,
            };
            // `withhold_chunk: false` — the source picture of a placed image is
            // in the file, which is exactly what makes it different from a
            // vector layer and what the real document shows.
            let resizable = match &layer.resizable_pixels {
                Some(pixels) => self.bitmap(pixels, size, CLIP_COLOUR, layer.pixel_fill, false),
                None => 0,
            };
            self.layers.push(vec![
                Value::Integer(ids[i]),
                Value::Text(layer.name.to_string()),
                Value::Integer(layer.kind),
                Value::Integer(i64::from(layer.folder)),
                // `LayerVisibility`: bit 0 the layer's eye, bit 1 its mask's.
                // A real file only sets the second on a layer that has one.
                Value::Integer(
                    i64::from(layer.visible)
                        | (i64::from(layer.mask.is_some() && layer.mask_visible) * 2),
                ),
                Value::Integer(i64::from(layer.locked)),
                Value::Integer(i64::from(layer.clipped)),
                Value::Integer(layer.opacity),
                Value::Integer(layer.composite),
                Value::Integer(ids.get(i + 1).copied().unwrap_or(0)),
                Value::Integer(first_child),
                Value::Integer(render),
                Value::Integer(mask),
                // The six offsets. A real file splits a placement between
                // `LayerOffset*` and the two `Offscr*` pairs, and the reader
                // sums them, so the fixture puts half in each — a reader that
                // read only one of them would land at half the offset.
                Value::Integer(layer.offset.0 - layer.offset.0 / 2),
                Value::Integer(layer.offset.1 - layer.offset.1 / 2),
                Value::Integer(layer.offset.0 / 2),
                Value::Integer(layer.offset.1 / 2),
                Value::Integer(layer.offset.0 / 2),
                Value::Integer(layer.offset.1 / 2),
                Value::Integer(0),
                Value::Integer(0),
                Value::Integer(layer.special_render),
                // Clip Studio states each channel over the whole of `u32`
                // rather than as a byte, which is the thing a reader is most
                // likely to get wrong by a factor of 16 million.
                Value::Integer(scale_channel(layer.draw_colour[0])),
                Value::Integer(scale_channel(layer.draw_colour[1])),
                Value::Integer(scale_channel(layer.draw_colour[2])),
                Value::Integer(resizable),
            ]);
        }
        ids.first().copied().unwrap_or(0)
    }
}

/// A byte as Clip Studio stores a colour channel: spread over the whole of
/// `u32`, so 255 is `0xFFFFFFFF` exactly.
fn scale_channel(byte: u8) -> i64 {
    (i64::from(byte) * i64::from(u32::MAX)) / 255
}

/// The size of the `CanvasPreview` every `.clip` fixture carries, and the one
/// colour in it. Both are chosen to be unlike anything a layer holds, so a
/// thumbnail that came from anywhere else is visibly wrong rather than
/// accidentally right.
pub const CLIP_PREVIEW: (u32, u32) = (12, 6);
pub const CLIP_PREVIEW_PIXEL: [u8; 4] = [7, 200, 111, 255];

const CLIP_LAYER_COLUMNS: [&str; 26] = [
    "MainId",
    "LayerName",
    "LayerType",
    "LayerFolder",
    "LayerVisibility",
    "LayerLock",
    "LayerClip",
    "LayerOpacity",
    "LayerComposite",
    "LayerNextIndex",
    "LayerFirstChildIndex",
    "LayerRenderMipmap",
    "LayerLayerMaskMipmap",
    "LayerOffsetX",
    "LayerOffsetY",
    "LayerRenderOffscrOffsetX",
    "LayerRenderOffscrOffsetY",
    "LayerMaskOffsetX",
    "LayerMaskOffsetY",
    "LayerMaskOffscrOffsetX",
    "LayerMaskOffscrOffsetY",
    // The Paper layer's four: what marks it, and the flat colour it draws.
    "SpecialRenderType",
    "DrawColorMainRed",
    "DrawColorMainGreen",
    "DrawColorMainBlue",
    // What tells a placed image from a vector layer, both of which carry
    // `LayerType` 0 and neither of which has a rendered bitmap.
    "ResizableOriginalMipmap",
];

/// A whole `.clip`: the database, its external chunks and the chunk stream.
pub fn clip(width: u32, height: u32, layers: &[ClipLayer]) -> Vec<u8> {
    clip_with(width, height, layers, |db| db)
}

/// How a fixture's `Canvas` row states its size.
///
/// A real `.clip` may measure its canvas in physical units and leave the
/// resolution to turn it into pixels, which is a thing the reader has to be
/// driven through rather than reasoned about — the file that exposed it opened
/// at 21×29 instead of 4961×7016.
pub struct CanvasSize {
    /// `CanvasWidth`/`CanvasHeight`, in whatever `CanvasUnit` says.
    pub measure: (f64, f64),
    /// `CanvasUnit`. 0 is pixels and 1 is centimetres; anything else is a unit
    /// this build refuses.
    pub unit: i64,
    /// `CanvasResolution`, or `None` to leave it out entirely.
    pub dpi: Option<f64>,
}

impl CanvasSize {
    /// Pixels at the usual resolution, which is what most files hold.
    pub fn pixels(width: u32, height: u32) -> Self {
        Self {
            measure: (f64::from(width), f64::from(height)),
            unit: 0,
            dpi: Some(350.0),
        }
    }

    /// A canvas stated as a physical measurement.
    pub fn measured(width: f64, height: f64, unit: i64, dpi: Option<f64>) -> Self {
        Self {
            measure: (width, height),
            unit,
            dpi,
        }
    }
}

/// A `.clip` whose canvas is stated in `size`'s own terms.
pub fn clip_sized(size: CanvasSize, layers: &[ClipLayer]) -> Vec<u8> {
    // The layers still need a pixel canvas to be built against, so the bitmaps
    // are made at whatever the measurement comes to. A fixture whose unit this
    // build refuses never reaches a layer, so any workable figure will do.
    let (w, h) = match size.unit {
        1 => {
            let dpi = size.dpi.unwrap_or(350.0);
            (
                (size.measure.0 * dpi / 2.54).round() as u32,
                (size.measure.1 * dpi / 2.54).round() as u32,
            )
        }
        _ => (size.measure.0 as u32, size.measure.1 as u32),
    };
    clip_inner(w.max(1), h.max(1), Some(size), layers, |db| db)
}

/// The same, with one hand on the finished database — for a test that has to
/// damage it.
pub fn clip_with(
    width: u32,
    height: u32,
    layers: &[ClipLayer],
    damage: impl FnOnce(Vec<u8>) -> Vec<u8>,
) -> Vec<u8> {
    clip_inner(width, height, None, layers, damage)
}

/// The builder both entry points share.
///
/// `stated` is how the `Canvas` row should describe itself; `width` and
/// `height` are always the real pixel canvas the layers are built against, so
/// a fixture measured in centimetres still gets bitmaps of the right size.
fn clip_inner(
    width: u32,
    height: u32,
    stated: Option<CanvasSize>,
    layers: &[ClipLayer],
    damage: impl FnOnce(Vec<u8>) -> Vec<u8>,
) -> Vec<u8> {
    let mut build = ClipBuild {
        width,
        height,
        layers: Vec::new(),
        mipmaps: Vec::new(),
        infos: Vec::new(),
        offscreens: Vec::new(),
        external: Vec::new(),
        // Clip Studio numbers its own root folder 2 and Umber's reader does not
        // care what the number is, only that `CanvasRootFolder` names it.
        next_id: 1,
        next_chunk: 0,
    };
    let root = build.id();
    let first_child = build.chain(layers);
    build.layers.push(vec![
        Value::Integer(root),
        Value::Text(String::new()),
        Value::Integer(256),
        Value::Integer(1),
        Value::Integer(1),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(256),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(first_child),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(0),
        Value::Integer(0),
    ]);

    // The flattened picture Clip Studio keeps beside the layers, which is what
    // a thumbnail reads. Deliberately a *different* size from the canvas and a
    // colour no layer in any fixture uses, so a reader that produced the
    // thumbnail by any other route — compositing, or taking the first layer —
    // fails rather than coincidentally agreeing.
    let preview_png = png_rgba(
        CLIP_PREVIEW.0,
        CLIP_PREVIEW.1,
        &solid(CLIP_PREVIEW.0, CLIP_PREVIEW.1, &CLIP_PREVIEW_PIXEL),
    );
    let canvas_preview = TableSpec::new(
        "CanvasPreview",
        &[
            "MainId",
            "ImageType",
            "ImageWidth",
            "ImageHeight",
            "ImageData",
        ],
    )
    .row(vec![
        Value::Integer(1),
        Value::Integer(1),
        Value::Integer(i64::from(CLIP_PREVIEW.0)),
        Value::Integer(i64::from(CLIP_PREVIEW.1)),
        Value::Blob(preview_png),
    ]);

    let stated = stated.unwrap_or_else(|| CanvasSize::pixels(width, height));
    let mut canvas = TableSpec::new(
        "Canvas",
        &[
            "MainId",
            "CanvasWidth",
            "CanvasHeight",
            "CanvasResolution",
            "CanvasUnit",
            "CanvasRootFolder",
        ],
    );
    canvas = canvas.row(vec![
        Value::Integer(1),
        Value::Real(stated.measure.0),
        Value::Real(stated.measure.1),
        // `Null` rather than a zero, because "states no resolution" and "states
        // nought" are different files and the reader treats them the same way
        // only by accident today.
        stated.dpi.map_or(Value::Null, Value::Real),
        Value::Integer(stated.unit),
        Value::Integer(root),
    ]);

    let mut layer_table = TableSpec::new("Layer", &CLIP_LAYER_COLUMNS);
    for row in build.layers {
        layer_table = layer_table.row(row);
    }
    let mut mipmap = TableSpec::new("Mipmap", &["MainId", "BaseMipmapInfo"]);
    for row in build.mipmaps {
        mipmap = mipmap.row(row);
    }
    let mut info = TableSpec::new("MipmapInfo", &["MainId", "Offscreen"]);
    for row in build.infos {
        info = info.row(row);
    }
    let mut offscreen = TableSpec::new("Offscreen", &["MainId", "Attribute", "BlockData"]);
    for row in build.offscreens {
        offscreen = offscreen.row(row);
    }

    let database = damage(crate::sqlite::fixture::database(&[
        canvas,
        canvas_preview,
        layer_table,
        mipmap,
        info,
        offscreen,
    ]));
    clip_container(&build.external, &database)
}

/// Wrap the pieces in the `CSFCHUNK` stream.
pub fn clip_container(external: &[(Vec<u8>, Vec<u8>)], database: &[u8]) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::new();
    let chunk = |tag: &[u8; 8], payload: &[u8]| {
        let mut out = tag.to_vec();
        out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        out.extend_from_slice(payload);
        out
    };
    body.extend_from_slice(&chunk(b"CHNKHead", &[0u8; 40]));
    for (name, data) in external {
        let mut payload = (name.len() as u64).to_be_bytes().to_vec();
        payload.extend_from_slice(name);
        payload.extend_from_slice(&(data.len() as u64).to_be_bytes());
        payload.extend_from_slice(data);
        body.extend_from_slice(&chunk(b"CHNKExta", &payload));
    }
    body.extend_from_slice(&chunk(b"CHNKSQLi", database));
    body.extend_from_slice(&chunk(b"CHNKFoot", &[]));

    let mut out = b"CSFCHUNK".to_vec();
    out.extend_from_slice(&((body.len() + 24) as u64).to_be_bytes());
    out.extend_from_slice(&24u64.to_be_bytes());
    out.extend_from_slice(&body);
    out
}
