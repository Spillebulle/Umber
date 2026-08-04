//! The full-resolution pixels of a Clip Studio material.
//!
//! A brush tip and a paper texture are both *materials*, and a material is a
//! USTAR archive stored in `MaterialFile.FileData` — see
//! [`super::clipstudio`]. Beside `thumbnail/thumbnail.png` sits
//! `data/material_0.layer` (or `data/material.layer`), which is where the
//! picture the artist actually drew lives. This module is the route to it.
//!
//! It is separate from [`super::clipstudio`] because it is a *file format*
//! rather than a brush conversion: four nested containers, none of which knows
//! anything about a `Brush`.
//!
//! # The four layers, outermost first
//!
//! 1. **A C2F chunk stream.** Magic `\x89C2F\r\n\x1a\n`, then chunks of
//!    `[u32 le size][4-byte tag][payload][u32 checksum]` tagged `HEAD`, `dATA`
//!    and `TAIL`. Each `dATA` payload opens with a `u16` flag: the one with
//!    flag 1 is a fixed 5128 bytes with no structure left in it, and the one
//!    with **flag 0** is everything below.
//!
//! 2. **A headerless SQLite database.** Page size 1024, and the file header and
//!    the first five pages are gone, so the payload's first page is page
//!    **six** — [`crate::sqlite::Database::headerless`]. `docs/brushes.md` said
//!    seven and it is six; the number is not a matter of taste, because an
//!    overflow chain names absolute page numbers and the picture is exactly the
//!    blob large enough to need one. Every claim in that note was re-derived
//!    from the two sample files before any of this was written, and this is the
//!    one that did not survive.
//!
//!    There is no `sqlite_master` to find a table with — it lived on page 1 —
//!    so the way in is [`crate::sqlite::Database::scan`] and a row is
//!    recognised by what is *in* it. The table wanted is `Offscreen`
//!    (`_PW_ID`, `MainId`, `CanvasId`, `LayerId`, `Attribute`, `BlockData`),
//!    and a material holds **three** of its rows: two empty mipmap levels and
//!    one carrying the pixels. Which is which is decided by the data — a row
//!    whose blocks are all absent has no picture in it — rather than by
//!    `MainId`, which is a number this reader has no key to.
//!
//! 3. **A record stream**, in both `Attribute` and `BlockData`:
//!    `[u32 be size][u32 be name length][utf-16be name][payload]`, the size
//!    covering the whole record. `Attribute`'s first record is `Parameter`,
//!    whose payload opens with the material's true width and height as plain
//!    integers, then the block grid. `BlockData` is one
//!    `BlockDataBeginChunk` record per block, each ending with a nested
//!    `BlockDataEndChunk` marker, then a `BlockStatus` record.
//!
//! 4. **A plain zlib stream per block** — `78 01`, nothing exotic. Blocks are
//!    256×256, row-major over `ceil(width / 256)` columns, and their channels
//!    are **planar**: with two channels the first 65536 bytes of a block are
//!    channel 0 and the second 65536 are channel 1.
//!
//!    Channel 0 is the coverage and nothing else is read — `offscreen` has the
//!    measurement, and the one material shape this reader refuses rather than
//!    guesses at.
//!
//! # Refusing rather than guessing
//!
//! Every length here comes out of somebody else's file and every one is
//! bounds-checked; a material that does not parse answers `None` and the
//! caller falls back to the thumbnail. That fallback is not a formality: of
//! the six materials in the two sample files, one is refused for its plane
//! layout and one because its `dATA` payload is not a headerless database at
//! all — not at the page size and first page below, and not at any other the
//! numbers were swept over by hand while this was written.
//!
//! **Decompression is bounded by the size the block declares**, so a zip bomb
//! costs one block's worth of memory; and the *picture* is bounded separately
//! by [`MAX_SIDE`], because a block's declared size says nothing about the
//! dimensions the material claims and the allocation follows those.

use std::io::Read;

use crate::sqlite::{Database, Value};

/// Blocks are always this square. It is in the file too, and checked against.
const BLOCK: usize = 256;

/// The page a material's embedded database starts at.
///
/// Not one: the file header and the pages before this are simply absent, and a
/// page number `n` therefore names the byte range `(n - 6) * 1024`. Verified
/// against both sample files by sweeping the number and taking the one whose
/// overflow chains all resolve — at every other offset the largest blob in the
/// database is 254 bytes, which is the *empty* mipmap level.
const FIRST_PAGE: u32 = 6;

/// The page size that database uses.
const PAGE_SIZE: usize = 1024;

/// The largest side a material may claim before this reader stops believing it.
///
/// Four times the largest mask the engine can bind and seven times the largest
/// material in the sample files, so nothing real is refused. What it bounds is
/// not the picture but the **allocation** a corrupt or hostile file can ask
/// for: 64 MB at this figure, and unbounded without it, because two consistent
/// but enormous dimensions multiply to something a 64-bit `usize` holds and
/// `checked_mul` then waves through.
const MAX_SIDE: u32 = 8_192;

/// A material's own pixels, as coverage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Material {
    pub width: u32,
    pub height: u32,
    /// `width * height` bytes, row-major, `0` no paint and `255` full — which
    /// is [`crate::tip::TipMask`]'s convention, so it goes straight into one.
    pub coverage: Vec<u8>,
}

/// Read the full-resolution pixels out of a material's tar archive.
///
/// `None` for anything this reader cannot make sense of — a material Clip
/// Studio left out of the file, a container shape it has never seen, a block
/// that will not inflate. The caller keeps the thumbnail in that case, which
/// is what it did before this module existed.
pub fn from_archive(archive: &[u8]) -> Option<Material> {
    let layer = ["data/material_0.layer", "data/material.layer"]
        .into_iter()
        .find_map(|name| super::clipstudio::tar_member(archive, name))?;
    from_layer(&layer)
}

/// Read a `material_0.layer` — the C2F stream and everything under it.
pub fn from_layer(layer: &[u8]) -> Option<Material> {
    let payload = database_chunk(layer)?;
    let db = Database::headerless(payload, PAGE_SIZE, FIRST_PAGE).ok()?;
    // Three `Offscreen` rows, two of them empty mipmap levels — but **the
    // biggest picture rather than the first**, because "the others are empty"
    // is a property of the sample files and not of the format. Taking the
    // first would let page order decide a brush's resolution, silently, the
    // day a material arrives with a populated mipmap. `MainId` orders them in
    // the sample files and is a number with no key to it, so it is not used.
    db.scan()
        .iter()
        .filter_map(offscreen)
        .max_by_key(|m| u64::from(m.width) * u64::from(m.height))
}

/// The `dATA` chunk whose flag is zero, without the flag.
///
/// Bounded by the declared size at every step, so a truncated or hostile
/// stream ends the walk rather than reading past it.
fn database_chunk(layer: &[u8]) -> Option<&[u8]> {
    const MAGIC: &[u8; 8] = b"\x89C2F\r\n\x1a\n";
    if !layer.starts_with(MAGIC) {
        return None;
    }
    let mut at = MAGIC.len();
    while at + 8 <= layer.len() {
        let size = u32::from_le_bytes(layer[at..at + 4].try_into().ok()?) as usize;
        let tag = &layer[at + 4..at + 8];
        let body = at + 8;
        let end = body.checked_add(size)?;
        if end > layer.len() {
            return None;
        }
        if tag == b"dATA" && size >= 2 && layer[body] == 0 && layer[body + 1] == 0 {
            return layer.get(body + 2..end);
        }
        if tag == b"TAIL" {
            return None;
        }
        // Four more for the checksum that follows every payload.
        at = end.checked_add(4)?;
    }
    None
}

/// One `Offscreen` row, decoded.
fn offscreen(row: &crate::sqlite::Row) -> Option<Material> {
    // `_PW_ID`, `MainId`, `CanvasId`, `LayerId`, `Attribute`, `BlockData`.
    let values = row.values();
    if values.len() != 6 {
        return None;
    }
    let (Value::Blob(attribute), Value::Blob(blocks)) = (&values[4], &values[5]) else {
        return None;
    };

    // `Attribute` opens with a short header whose first word is its own
    // length, and the first named field follows it.
    let head = be32(attribute, 0)? as usize;
    if !(8..=64).contains(&head) {
        return None;
    }
    let (name, parameter) = field(attribute, head)?;
    if name != "Parameter" {
        return None;
    }
    let width = be32(parameter, 0)?;
    let height = be32(parameter, 4)?;
    let columns = be32(parameter, 8)? as usize;
    let rows = be32(parameter, 12)? as usize;
    if width == 0 || height == 0 || columns == 0 || rows == 0 {
        return None;
    }
    // **The size is refused before it is believed**, and `checked_mul` below is
    // not enough on its own: a consistent pair of very large numbers multiplies
    // to something a 64-bit `usize` holds happily and then asks the allocator
    // for it, so a file a stranger wrote could take the application down by
    // naming a picture nobody drew. The bound is generous — every material in
    // the sample files is under 1200 on a side, and Clip Studio's own canvas
    // does not reach this — and the cost of exceeding it is a fall back to the
    // thumbnail rather than a refusal to import.
    if width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    // A grid that does not cover the picture — or covers far more of it than
    // it needs to — is a parse that has gone wrong, not a material.
    if columns != (width as usize).div_ceil(BLOCK) || rows != (height as usize).div_ceil(BLOCK) {
        return None;
    }

    let texels = (width as usize).checked_mul(height as usize)?;
    let mut coverage = vec![0u8; texels];
    let mut found = false;
    let mut at = 0usize;
    let mut index = 0usize;
    while let Some(chunk) = record(blocks, at) {
        at = chunk.end;
        if chunk.name != "BlockDataBeginChunk" {
            continue;
        }
        let (column, row) = (index % columns, index / columns);
        index += 1;
        if row >= rows {
            return None;
        }
        // An absent block is a block of nothing, and the mask already holds
        // nothing — so it costs no inflate and no copy. That is also what makes
        // the two empty mipmap levels free: neither yields a single block, so
        // `from_layer` finds no picture in them and moves on.
        let Some(block) = decode_block(chunk.payload)? else {
            continue;
        };
        // **One or two planes, and the first is the mask.** Both readings are
        // measured rather than assumed: decoded, downscaled to the material's
        // own thumbnail and compared against the coverage `clipstudio` computes
        // off that thumbnail, plane 0 differs by a mean absolute 0.0002,
        // 0.0397, 0.0612 and 0.0680 of a level over the four materials in the
        // sample files — the resampling and nothing else. The second plane,
        // where there is one, is **all zeroes** in every material sampled, so
        // reading it as an alpha and multiplying would give a mask that paints
        // nothing.
        //
        // Anything else answers `None` and the caller keeps the thumbnail. The
        // shape that exists and is refused is five planes: an alpha that is
        // solid, then four that look like colour. `alpha × (1 - luma)` over the
        // first three is the obvious reading, it was tried, and it lands 0.13
        // of a level away from that material's own thumbnail — twice the worst
        // above, and unexplained. A mask that is *plausibly* a paper is exactly
        // the quietly-wrong picture this project refuses; the thumbnail is a
        // smaller picture of the right one.
        //
        // Refused **here** rather than after the walk, so a colour material
        // costs one block's inflate rather than a plane per channel of a
        // canvas-sized picture. Only plane 0 is ever kept, for the same reason.
        // "All zeroes" is **checked, not assumed**. If a material ever carries
        // a live alpha there, plane 0 alone paints where the material is
        // transparent — a plausible-looking, wrong mask, which is the exact
        // thing the five-plane refusal exists to prevent. One scan per block,
        // and the answer is the same fall back to the thumbnail.
        let planes = block.len() / (BLOCK * BLOCK);
        let usable = match planes {
            1 => true,
            2 => block[BLOCK * BLOCK..].iter().all(|v| *v == 0),
            _ => false,
        };
        if !usable {
            return None;
        }
        blit(
            &mut coverage,
            width,
            height,
            column,
            row,
            &block[..BLOCK * BLOCK],
        );
        found = true;
    }
    found.then_some(Material {
        width,
        height,
        coverage,
    })
}

/// `[u32 index][u32 uncompressed bytes][u32 block width][u32 block height]
/// [u32 present]`, then — only where it is present —
/// `[u32 length + 4][u32 le length][zlib stream]`.
///
/// `Ok(None)` is a block that is simply not there, which is the ordinary state
/// of a mipmap level Clip Studio has not built; `None` is a block this reader
/// could not parse, and takes the whole material with it.
///
/// The two lengths disagree by exactly the four bytes of the second, in both
/// sample files and in every block of both. Neither is trusted for the *end*
/// of the stream: the record's own size is, minus the nested
/// `BlockDataEndChunk` marker that closes it, because that is the one bound
/// the container itself guarantees.
///
/// `take` stops the decoder at the size the block declared, which bounds what
/// a hostile stream can cost to one block.
fn decode_block(payload: &[u8]) -> Option<Option<Vec<u8>>> {
    const PLANE: usize = BLOCK * BLOCK;
    /// A material of more than this many planes is not one this reader knows.
    /// Five is the most any sample has — an alpha, three colours, and one more
    /// that nothing reads.
    const MAX_PLANES: usize = 5;

    let declared = be32(payload, 4)? as usize;
    let block_width = be32(payload, 8)? as usize;
    let block_height = be32(payload, 12)? as usize;
    let present = be32(payload, 16)?;
    if block_width != BLOCK || block_height != BLOCK {
        return None;
    }
    if present == 0 {
        return Some(None);
    }
    // A block is a whole number of 256-square planes and nothing else. Checked
    // only where the block is real: an absent one carries a stale figure, and
    // one of the sample materials leaves five channels' worth there.
    if declared < PLANE || !declared.is_multiple_of(PLANE) || declared > MAX_PLANES * PLANE {
        return None;
    }

    // The stream runs from here to the end of the record, less the nested
    // marker. `record` has already trimmed the payload to the record, so the
    // marker is what is left over at the end.
    let stream = payload.get(28..payload.len().checked_sub(END_MARKER)?)?;
    let mut pixels = Vec::with_capacity(declared);
    flate2::read::ZlibDecoder::new(stream)
        .take(declared as u64)
        .read_to_end(&mut pixels)
        .ok()?;
    if pixels.len() != declared {
        return None;
    }
    Some(Some(pixels))
}

/// `[u32 17]["BlockDataEndChunk"]`, which closes every block record.
const END_MARKER: usize = 4 + 17 * 2;

/// A `[u32 name length][utf-16be name]` label, and everything after it.
///
/// `Attribute`'s fields are framed this way — no length, so a reader has to
/// know how long each is. Only the first, `Parameter`, is read, and its own
/// fields are fixed-position, so nothing here needs to walk to the second.
fn field(blob: &[u8], at: usize) -> Option<(String, &[u8])> {
    let units = be32(blob, at)? as usize;
    if units == 0 || units > 64 {
        return None;
    }
    let body = at.checked_add(4)?.checked_add(units * 2)?;
    let name = utf16be(blob.get(at + 4..body)?);
    Some((name, blob.get(body..)?))
}

/// A record of the `[u32 size][u32 name length][utf-16be name][payload]` form.
struct Record<'a> {
    name: String,
    payload: &'a [u8],
    /// Where the record after this one starts.
    end: usize,
}

fn record(blob: &[u8], at: usize) -> Option<Record<'_>> {
    let size = be32(blob, at)? as usize;
    let units = be32(blob, at + 4)? as usize;
    // A name is a handful of ASCII words; anything else is not this format.
    if units == 0 || units > 64 {
        return None;
    }
    let head = 8 + units * 2;
    if size < head {
        return None;
    }
    let end = at.checked_add(size)?;
    let body = blob.get(at..end)?;
    Some(Record {
        name: utf16be(&body[8..head]),
        payload: &body[head..],
        end,
    })
}

fn utf16be(bytes: &[u8]) -> String {
    String::from_utf16_lossy(
        &bytes
            .chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect::<Vec<_>>(),
    )
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_be_bytes(b.try_into().expect("four bytes")))
}

/// Copy one 256-square block into its place in the mask, clipped to the
/// picture.
///
/// The last column and the last row are partly outside it — 267 pixels is two
/// blocks of which the second contributes eleven — so the copy is per row and
/// bounded by the mask rather than by the block.
fn blit(plane: &mut [u8], width: u32, height: u32, column: usize, row: usize, block: &[u8]) {
    let (width, height) = (width as usize, height as usize);
    let x0 = column * BLOCK;
    let y0 = row * BLOCK;
    let take = BLOCK.min(width.saturating_sub(x0));
    for y in 0..BLOCK.min(height.saturating_sub(y0)) {
        let from = y * BLOCK;
        let to = (y0 + y) * width + x0;
        plane[to..to + take].copy_from_slice(&block[from..from + take]);
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A material layer built byte by byte, the way every other importer's tests
/// build theirs.
///
/// Nothing vendored: the sample `.sut` files this reader was written against
/// are somebody's own downloads of unknown licence, so what the tests check is
/// this module's understanding of the four containers rather than those files
/// — exactly the bargain `crate::sqlite::fixture` records. What offsets that is
/// that the writer here is the *inverse* of the reader rather than a copy of
/// it, and that every layout it lays down was measured off the real files
/// first.
#[cfg(test)]
pub(crate) mod fixture {
    use super::{BLOCK, END_MARKER, FIRST_PAGE, PAGE_SIZE};
    use crate::sqlite::{Value, fixture::headerless};
    use std::io::Write;

    fn utf16be(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
    }

    /// `[u32 name length][utf-16be name]`, the way `Attribute` frames a field.
    fn field(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = (name.chars().count() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&utf16be(name));
        out.extend_from_slice(payload);
        out
    }

    /// `[u32 size][u32 name length][utf-16be name][payload]`, the way
    /// `BlockData` frames a record.
    fn record(name: &str, payload: &[u8]) -> Vec<u8> {
        let name = utf16be(name);
        let size = 8 + name.len() + payload.len();
        let mut out = (size as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&((name.len() / 2) as u32).to_be_bytes());
        out.extend_from_slice(&name);
        out.extend_from_slice(payload);
        out
    }

    /// An `Offscreen` row's `Attribute`: a short header, then `Parameter`.
    fn attribute(width: u32, height: u32) -> Vec<u8> {
        let mut parameter = Vec::new();
        for v in [
            width,
            height,
            width.div_ceil(BLOCK as u32),
            height.div_ceil(BLOCK as u32),
        ] {
            parameter.extend_from_slice(&v.to_be_bytes());
        }
        let body = field("Parameter", &parameter);
        // `[u32 header length][u32 first field length][u32][u32]`.
        let mut out = 16u32.to_be_bytes().to_vec();
        for v in [body.len() as u32, 42, 42] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&body);
        out
    }

    /// One block record, present or absent.
    fn block(pixels: Option<&[u8]>, planes: usize) -> Vec<u8> {
        let declared = (planes * BLOCK * BLOCK) as u32;
        let mut head = Vec::new();
        for v in [0u32, declared, BLOCK as u32, BLOCK as u32] {
            head.extend_from_slice(&v.to_be_bytes());
        }
        let Some(pixels) = pixels else {
            head.extend_from_slice(&0u32.to_be_bytes());
            head.extend_from_slice(&record("BlockDataEndChunk", &[]).as_slice()[4..]);
            return record("BlockDataBeginChunk", &head);
        };
        head.extend_from_slice(&1u32.to_be_bytes());

        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(pixels).expect("deflate");
        let stream = encoder.finish().expect("deflate");
        // The pair of lengths the real files carry: big-endian counting the
        // second field, then little-endian counting the stream alone.
        head.extend_from_slice(&(stream.len() as u32 + 4).to_be_bytes());
        head.extend_from_slice(&(stream.len() as u32).to_le_bytes());
        head.extend_from_slice(&stream);
        // The nested marker `decode_block` measures the stream's end against.
        head.extend_from_slice(&record("BlockDataEndChunk", &[]).as_slice()[4..]);
        debug_assert_eq!(END_MARKER, record("BlockDataEndChunk", &[]).len() - 4);
        record("BlockDataBeginChunk", &head)
    }

    /// A whole `data/material_0.layer`: the C2F stream, the headerless
    /// database, one `Offscreen` row and its blocks.
    ///
    /// `coverage` is the picture, `width * height` bytes; `planes` says how
    /// many channels each block declares, the extra ones written as zeroes.
    pub fn material_layer(width: u32, height: u32, coverage: &[u8], planes: usize) -> Vec<u8> {
        assert_eq!(coverage.len(), width as usize * height as usize);
        let columns = (width as usize).div_ceil(BLOCK);
        let rows = (height as usize).div_ceil(BLOCK);

        let mut blocks = Vec::new();
        for row in 0..rows {
            for column in 0..columns {
                let mut pixels = vec![0u8; planes * BLOCK * BLOCK];
                for y in 0..BLOCK.min(height as usize - row * BLOCK) {
                    for x in 0..BLOCK.min(width as usize - column * BLOCK) {
                        pixels[y * BLOCK + x] =
                            coverage[(row * BLOCK + y) * width as usize + column * BLOCK + x];
                    }
                }
                blocks.extend_from_slice(&block(Some(&pixels), planes));
            }
        }
        blocks.extend_from_slice(&record("BlockStatus", &[0u8; 8]));

        // Two empty mipmap levels first, exactly as a real material has, so a
        // reader that took the first row it found would fail the test.
        let empty: Vec<u8> = (0..columns * rows)
            .flat_map(|_| block(None, planes))
            .collect();
        let offscreen = |attr: Vec<u8>, data: Vec<u8>| {
            vec![
                Value::Null,
                Value::Integer(3),
                Value::Integer(0),
                Value::Integer(2),
                Value::Blob(attr),
                Value::Blob(data),
            ]
        };
        let db = headerless(
            &[
                offscreen(attribute(width, height), empty.clone()),
                offscreen(attribute(width, height), empty),
                offscreen(attribute(width, height), blocks),
            ],
            PAGE_SIZE,
            FIRST_PAGE,
        );

        let mut payload = vec![0u8, 0];
        payload.extend_from_slice(&db);
        let mut out = b"\x89C2F\r\n\x1a\n".to_vec();
        for (tag, body) in [
            (b"HEAD", Vec::new()),
            (b"dATA", vec![1u8, 0]),
            (b"dATA", payload),
            (b"TAIL", Vec::new()),
        ] {
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(tag);
            out.extend_from_slice(&body);
            out.extend_from_slice(&0u32.to_be_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picture a material carries has to come back whole — across the C2F
    /// stream, the headerless database, the record framing and the zlib.
    #[test]
    fn a_materials_own_pixels_come_back_exactly() {
        let coverage: Vec<u8> = (0..100 * 60).map(|i| (i * 7 % 256) as u8).collect();
        let layer = fixture::material_layer(100, 60, &coverage, 1);

        let material = from_layer(&layer).expect("read");
        assert_eq!((material.width, material.height), (100, 60));
        assert_eq!(material.coverage, coverage);
    }

    /// The picture is bigger than one block and bigger than one page, so the
    /// grid has to be walked and the record has to come back off an **overflow
    /// chain** — whose page numbers are absolute, which is the whole reason
    /// `FIRST_PAGE` is a number rather than a guess.
    #[test]
    fn a_material_of_several_blocks_is_assembled_in_the_right_order() {
        let (w, h) = (300u32, 260u32);
        // A gradient in x and a step in y, so a block placed in the wrong
        // column *or* the wrong row shows up.
        let coverage: Vec<u8> = (0..h)
            .flat_map(|y| (0..w).map(move |x| ((x * 255 / (w - 1)) as u8) ^ ((y / 130) as u8 * 15)))
            .collect();
        let layer = fixture::material_layer(w, h, &coverage, 1);
        assert!(layer.len() > 4 * PAGE_SIZE, "the fixture must spill");

        let material = from_layer(&layer).expect("read");
        assert_eq!((material.width, material.height), (w, h));
        assert_eq!(material.coverage, coverage);
    }

    /// The second plane is all zeroes in every material sampled, so reading it
    /// as an alpha and multiplying would give a mask that paints nothing. This
    /// is the guard on that.
    #[test]
    fn a_second_plane_does_not_dim_the_mask() {
        let coverage = vec![255u8; 40 * 40];
        let one = from_layer(&fixture::material_layer(40, 40, &coverage, 1)).expect("one plane");
        let two = from_layer(&fixture::material_layer(40, 40, &coverage, 2)).expect("two planes");
        assert_eq!(one.coverage, two.coverage);
        assert!(two.coverage.iter().all(|v| *v == 255));
    }

    /// Five planes is the colour shape, and it is refused rather than guessed
    /// at — the caller keeps the thumbnail, which is a smaller picture of the
    /// right thing rather than a plausible picture of the wrong one.
    #[test]
    fn a_material_this_reader_cannot_read_answers_nothing_rather_than_a_guess() {
        let coverage = vec![128u8; 40 * 40];
        assert!(from_layer(&fixture::material_layer(40, 40, &coverage, 5)).is_none());
        // And so does anything that is not a material layer at all.
        assert!(from_layer(b"not a C2F stream").is_none());
        assert!(from_layer(&[]).is_none());
    }

    /// Blocks of a material that disagree about how many channels they carry.
    ///
    /// A real file does not do this and a corrupt one can. It used to gather a
    /// plane per channel and then slice every block by the *widest* count any
    /// block had declared, so a two-plane block followed by a one-plane block
    /// sliced past the end and **panicked** — which takes the application down
    /// with every unsaved document in it, because somebody imported a brush.
    #[test]
    fn blocks_that_disagree_about_their_channels_are_refused_rather_than_sliced_past() {
        let (w, h) = (400u32, 200u32);
        let coverage = vec![200u8; (w * h) as usize];
        // Two blocks across. Splice a one-plane second block into a two-plane
        // material by building both and taking one record from each.
        let wide = fixture::material_layer(w, h, &coverage, 2);
        let narrow = fixture::material_layer(w, h, &coverage, 1);
        assert!(from_layer(&wide).is_some());
        assert!(from_layer(&narrow).is_some());

        // The channel count is the block's own declared size, so flipping one
        // block's figure is enough to make the two disagree.
        let mut mixed = wide.clone();
        let two = (2 * BLOCK * BLOCK).to_be_bytes();
        let one = (BLOCK * BLOCK).to_be_bytes();
        let (mut seen, mut flipped) = (0, false);
        for at in 0..mixed.len() - 8 {
            if mixed[at..at + 8] == two[..] {
                seen += 1;
                // The second one, so the first block still says two.
                if seen == 2 {
                    mixed[at..at + 8].copy_from_slice(&one);
                    flipped = true;
                    break;
                }
            }
        }
        assert!(flipped, "the fixture must declare its channel count twice");
        // Whatever it decides, it must decide it rather than unwind.
        let verdict = std::panic::catch_unwind(|| from_layer(&mixed).is_some());
        assert!(verdict.is_ok(), "a mixed-channel material must not panic");
    }

    /// A size out of a stranger's file is refused before it is believed.
    /// `checked_mul` alone is not enough: two large numbers that are consistent
    /// with each other multiply to something a 64-bit `usize` holds, and then
    /// the allocator is asked for it.
    #[test]
    fn a_material_that_claims_an_enormous_size_is_refused_rather_than_allocated() {
        let coverage = vec![255u8; 40 * 40];
        let mut layer = fixture::material_layer(40, 40, &coverage, 1);

        // Overwrite every `Parameter` payload in the stream with a width and a
        // height whose product is 4 x 10^18 — under `usize::MAX`, and far past
        // any picture — leaving the block grid consistent with them.
        let (w, h) = (2_000_000_000u32, 2_000_000_000u32);
        let mut header = Vec::new();
        header.extend_from_slice(&9u32.to_be_bytes());
        header.extend_from_slice(
            &"Parameter"
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<u8>>(),
        );
        let mut wanted = header.clone();
        for v in [w, h, w.div_ceil(BLOCK as u32), h.div_ceil(BLOCK as u32)] {
            wanted.extend_from_slice(&v.to_be_bytes());
        }
        let mut replaced = 0;
        for at in 0..layer.len().saturating_sub(wanted.len()) {
            if layer[at..at + header.len()] == header[..] {
                layer[at..at + wanted.len()].copy_from_slice(&wanted);
                replaced += 1;
            }
        }
        assert!(replaced > 0, "the fixture must carry a Parameter field");
        assert!(from_layer(&layer).is_none());
    }

    /// These are files a stranger wrote, and every offset in them is read out
    /// of the file itself. A refusal is fine; a panic takes the application
    /// down with every unsaved document in it.
    #[test]
    fn a_corrupt_material_is_refused_and_never_panics() {
        // Several blocks and two planes, so the grid walk and the plane
        // arithmetic are both reachable — and the corruption runs over the
        // **whole** file rather than the first kilobyte. `clipstudio`'s own
        // fuzz test concentrates on the header, on the reasoning that a wrong
        // number does most damage there; that reasoning is wrong here, because
        // the block records live past the first page and they are where
        // `decode_block` and `blit` read.
        let (w, h) = (600u32, 300u32);
        let coverage: Vec<u8> = (0..w * h).map(|i| (i % 256) as u8).collect();
        let good = fixture::material_layer(w, h, &coverage, 2);

        let mut cases: Vec<Vec<u8>> = (0..64)
            .map(|i| good[..good.len() * i / 64].to_vec())
            .collect();
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..512 {
            let mut c = good.clone();
            for _ in 0..6 {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let at = (seed >> 33) as usize % c.len();
                c[at] ^= ((seed >> 11) & 0xff) as u8;
            }
            cases.push(c);
        }
        for (i, case) in cases.iter().enumerate() {
            let verdict = std::panic::catch_unwind(|| from_layer(case).is_some());
            assert!(
                verdict.is_ok(),
                "case {i} panicked instead of being refused"
            );
        }
    }
}
