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
