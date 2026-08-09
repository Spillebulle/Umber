//! The block stream Clip Studio stores a bitmap in.
//!
//! Two quite different files in this crate reach the same structure, which is
//! why it lives here rather than in either of them:
//!
//! - [`crate::brushimport::csmaterial`] reads a brush tip or a paper out of a
//!   material's `data/material_0.layer`;
//! - [`crate::docimport::clipstudio`] reads a layer's pixels out of a `.clip`
//!   document's external chunks.
//!
//! What they share is everything below the container: a record stream framed as
//! `[u32 be size][u32 be name length][utf-16be name][payload]`, an `Attribute`
//! blob whose `Parameter` field states the picture's size and its block grid,
//! and one zlib stream per 256-square block. A second copy of that would be the
//! drift this codebase refuses everywhere — the same rule `docformat` states as
//! "there must never be a second ORA reader".
//!
//! # Everything here reads a file a stranger wrote
//!
//! Every length is bounds-checked, every allocation is bounded by a figure the
//! *caller* states rather than by one the file does, and a decompressed block is
//! `take`n at the size the block itself declared — so a zip bomb costs one
//! block. Nothing here panics on a malformed input; it answers `None` and the
//! caller decides whether that costs a layer or the document.

use std::io::Read;

/// Blocks are always this square. It is in the file too, and checked against.
pub(crate) const BLOCK: usize = 256;

/// One 256-square plane of one byte per pixel.
pub(crate) const PLANE: usize = BLOCK * BLOCK;

/// `[u32 17]["BlockDataEndChunk"]`, which closes every block record.
pub(crate) const END_MARKER: usize = 4 + 17 * 2;

/// A bitmap of more channels than this is not one either reader knows.
///
/// Five is the widest shape Clip Studio writes: one alpha plane, then four
/// interleaved bytes of which three are colour.
const MAX_CHANNELS: usize = 5;

pub(crate) fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at.checked_add(4)?)
        .map(|b| u32::from_be_bytes(b.try_into().expect("four bytes")))
}

pub(crate) fn utf16be(bytes: &[u8]) -> String {
    String::from_utf16_lossy(
        &bytes
            .chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect::<Vec<_>>(),
    )
}

/// A record of the `[u32 size][u32 name length][utf-16be name][payload]` form.
pub(crate) struct Record<'a> {
    pub(crate) name: String,
    pub(crate) payload: &'a [u8],
    /// Where the record after this one starts.
    pub(crate) end: usize,
}

pub(crate) fn record(blob: &[u8], at: usize) -> Option<Record<'_>> {
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

/// A `[u32 name length][utf-16be name]` label, and everything after it.
///
/// `Attribute`'s fields are framed this way — no length of their own, so a
/// reader either knows how long each is or, as here, only reads the first.
pub(crate) fn field(blob: &[u8], at: usize) -> Option<(String, &[u8])> {
    let units = be32(blob, at)? as usize;
    if units == 0 || units > 64 {
        return None;
    }
    let body = at.checked_add(4)?.checked_add(units.checked_mul(2)?)?;
    let name = utf16be(blob.get(at + 4..body)?);
    Some((name, blob.get(body..)?))
}

/// What an `Offscreen` row's `Attribute` blob says about its picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bitmap {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The block grid, in blocks.
    pub(crate) columns: usize,
    pub(crate) rows: usize,
    /// How the bytes of a decompressed block are laid out, where the file said.
    ///
    /// `None` where the `Parameter` field is too short to hold the counts,
    /// which the fixtures in [`crate::brushimport::csmaterial`] deliberately
    /// are: that reader derives the channel count from the block's own declared
    /// size instead, and this is the field the *document* reader needs.
    pub(crate) packing: Option<Packing>,
    /// What an absent block holds.
    pub(crate) fill: Fill,
}

/// The two halves of a block's bytes.
///
/// A block is `first` **planes** of one byte per pixel, then `second`
/// **interleaved** bytes per pixel. Clip Studio writes `(1, 4)` for colour —
/// an alpha plane, then BGRX four bytes at a time — `(1, 1)` for greyscale and
/// `(1, 0)` for a mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Packing {
    pub(crate) first: usize,
    pub(crate) second: usize,
}

impl Packing {
    /// Bytes one whole block decompresses to.
    pub(crate) fn block_len(self) -> usize {
        (self.first + self.second) * PLANE
    }
}

/// What a block Clip Studio did not store holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fill {
    /// Nothing: the state of every ordinary raster layer, whose untouched
    /// corners are transparent.
    Empty,
    /// The file states a fill, and this is its **first channel** as a byte.
    ///
    /// That is the whole of what a mask needs — one channel, and `255` is
    /// exactly the "reveal everything" a Clip Studio mask starts as. A
    /// *colour* bitmap's fill needs four more values whose meaning has never
    /// been checked against a file that paints with one, so a caller reading
    /// colour must refuse this rather than invent something plausible.
    Stated(u8),
    /// The `InitColor` section could not be located. Older files and the
    /// material fixtures both land here.
    Unknown,
}

/// Read an `Offscreen` row's `Attribute` blob.
///
/// `max_side` is the caller's bound on how large a picture it will believe —
/// **not** the file's, which is exactly the number a hostile file would like to
/// choose. Without it two consistent but enormous dimensions multiply to
/// something a 64-bit `usize` holds happily, and the allocation that follows
/// takes the application down.
pub(crate) fn parse_attribute(attribute: &[u8], max_side: u32) -> Option<Bitmap> {
    // `[u32 header length][u32 Parameter section][u32 InitColor section]
    //  [u32 BlockSize section]`, and the sections follow in that order.
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
    if width > max_side || height > max_side {
        return None;
    }
    // A grid that does not cover the picture — or covers far more of it than it
    // needs to — is a parse that has gone wrong, not a bitmap.
    if columns != (width as usize).div_ceil(BLOCK) || rows != (height as usize).div_ceil(BLOCK) {
        return None;
    }

    // `Parameter`'s remaining sixteen integers describe the pixel packing. The
    // first is the channel order, then the two halves and their sum.
    //
    // **Three numbers that disagree make the packing absent, not the bitmap
    // unreadable**, and the difference belongs to the *other* caller.
    // `csmaterial` never read these words at all — it takes its channel count
    // off each block's own declared size — so refusing here would silently
    // tighten a brush importer over a field it does not use, on real material
    // files nothing in this repository can test against. The document reader
    // refuses the absent packing itself, one line from where it would have.
    let packing = match (
        be32(parameter, 20),
        be32(parameter, 24),
        be32(parameter, 28),
    ) {
        (Some(first), Some(second), Some(total)) => {
            let (first, second) = (first as usize, second as usize);
            (first + second == total as usize && first + second <= MAX_CHANNELS && first != 0)
                .then_some(Packing { first, second })
        }
        _ => None,
    };

    Some(Bitmap {
        width,
        height,
        columns,
        rows,
        packing,
        fill: init_fill(attribute, head),
    })
}

/// Whether the `InitColor` section says an absent block is anything but empty.
///
/// The section sits immediately after `Parameter`, at an offset the header's
/// own three lengths give — which is the only way to reach it, because the
/// fields inside an `Attribute` carry a name and no length of their own.
fn init_fill(attribute: &[u8], head: usize) -> Fill {
    let (Some(info), Some(extra), Some(sizes)) =
        (be32(attribute, 4), be32(attribute, 8), be32(attribute, 12))
    else {
        return Fill::Unknown;
    };
    // **The four lengths have to account for the blob exactly**, which is the
    // only evidence available that this *is* the layout being read: the fields
    // inside an `Attribute` carry a name and no length, so a header that does
    // not add up leaves no way to know where the second one starts. It holds on
    // every `Offscreen` row of every sample file. Anything else is
    // [`Fill::Unknown`] rather than a guess.
    let total = [head, info as usize, extra as usize, sizes as usize]
        .into_iter()
        .try_fold(0usize, |a, b| a.checked_add(b));
    if total != Some(attribute.len()) {
        return Fill::Unknown;
    }
    let at = head + info as usize;
    let Some((name, body)) = field(attribute, at) else {
        return Fill::Unknown;
    };
    if name != "InitColor" {
        return Fill::Unknown;
    }
    // `[u32][u32 flag][u32 first channel][u32 second channel count][u32]
    //  [u32 second channels…]`. The two shapes seen: `[20, 0, 0, 0, 4]` on an
    // ordinary raster layer, whose absent blocks are transparent, and
    // `[20, 1, 0xFFFFFFFF, 0, 4]` on a **mask**, whose absent blocks reveal
    // everything — which is what a Clip Studio mask starts as, and the reason
    // this section is read at all rather than assumed away.
    //
    // The channel is taken as its **top byte**. Only `0` and all-ones have ever
    // been seen, and both read the same under any scaling; a value between them
    // has not been observed and is not something to be confident about.
    match (be32(body, 4), be32(body, 8)) {
        (Some(0), _) => Fill::Empty,
        (Some(_), Some(first)) => Fill::Stated((first >> 24) as u8),
        _ => Fill::Unknown,
    }
}

/// Walk a `BlockData` blob, handing each **stored** block to `f` in grid order.
///
/// A callback rather than a `Vec` of blocks, and that is a bound rather than a
/// style: a whole grid of decompressed blocks is `columns × rows × 320 KB`,
/// which on the largest canvas an import will accept is 1.3 GB — and every one
/// of those can be a hundred-byte zlib stream, so a 400 KB file would ask for
/// all of it. One block is live at a time here, so the amplification a hostile
/// file can reach is one block.
///
/// `expected` is how many blocks the `Attribute`'s grid says there are.
/// `None` is returned for a blob that does not hold exactly that many, or one
/// whose block this reader could not make sense of — which takes the whole
/// bitmap rather than leaving a hole somebody would read as the artist's own
/// transparency.
pub(crate) fn for_each_block(
    blob: &[u8],
    packing: Packing,
    expected: usize,
    mut f: impl FnMut(usize, &[u8]),
) -> Option<()> {
    let mut seen = 0usize;
    let mut at = 0usize;
    while let Some(chunk) = record(blob, at) {
        at = chunk.end;
        if chunk.name != "BlockDataBeginChunk" {
            continue;
        }
        // Counted before the decode, so a blob naming a million blocks costs
        // one inflate rather than a million.
        if seen >= expected {
            return None;
        }
        if let Some(block) = decode_block(chunk.payload, Some(packing))? {
            f(seen, &block);
        }
        seen += 1;
    }
    (seen == expected).then_some(())
}

/// `[u32 index][u32 uncompressed bytes][u32 block width][u32 block height]
/// [u32 present]`, then — only where it is present —
/// `[u32 length + 4][u32 le length][zlib stream]`.
///
/// `Some(None)` is a block that is simply not there, which is the ordinary
/// state of a mipmap level Clip Studio has not built and of any corner of a
/// layer nobody painted on; `None` is a block this reader could not parse.
///
/// The two lengths disagree by exactly the four bytes of the second, in every
/// block of every sample file. Neither is trusted for the *end* of the stream:
/// the record's own size is, minus the nested `BlockDataEndChunk` marker that
/// closes it, because that is the one bound the container itself guarantees.
///
/// `take` stops the decoder at the size the block declared, which bounds what a
/// hostile stream can cost to one block. `expect` is the packing the caller
/// already read out of the `Attribute`, where it has one: a block whose
/// declared size disagrees with it is refused rather than sliced by whichever
/// number happens to be smaller.
pub(crate) fn decode_block(payload: &[u8], expect: Option<Packing>) -> Option<Option<Vec<u8>>> {
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
    if declared < PLANE || !declared.is_multiple_of(PLANE) || declared > MAX_CHANNELS * PLANE {
        return None;
    }
    if expect.is_some_and(|p| p.block_len() != declared) {
        return None;
    }

    // The stream runs from here to the end of the record, less the nested
    // marker. The caller has already trimmed the payload to the record, so the
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

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The record framing and the `Attribute` blob, written.
///
/// The bargain `crate::sqlite::fixture` records applies here too: a generated
/// fixture tests this module against *this file's* understanding of the format
/// rather than against Clip Studio. What offsets it is that the writer is the
/// inverse of the reader rather than a copy of it, and that every layout below
/// was measured off real `.clip` files before it was written — the section
/// lengths in [`fixture::attribute`] in particular, which is how the
/// `InitColor` section is reached at all.
#[cfg(test)]
pub(crate) mod fixture {
    use super::{BLOCK, END_MARKER, PLANE, Packing};
    use std::io::Write;

    pub(crate) fn utf16be(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
    }

    /// `[u32 size][u32 name length][utf-16be name][payload]`.
    pub(crate) fn record(name: &str, payload: &[u8]) -> Vec<u8> {
        let name = utf16be(name);
        let size = 8 + name.len() + payload.len();
        let mut out = (size as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&((name.len() / 2) as u32).to_be_bytes());
        out.extend_from_slice(&name);
        out.extend_from_slice(payload);
        out
    }

    /// `[u32 name length][utf-16be name][payload]` — an `Attribute` field,
    /// which carries no length of its own.
    fn field(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = (name.chars().count() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&utf16be(name));
        out.extend_from_slice(payload);
        out
    }

    fn ints(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// A whole `Attribute` blob: the four section lengths, then `Parameter`,
    /// `InitColor` and `BlockSize` in that order.
    ///
    /// `fill` is the value an absent block holds, `None` for transparent —
    /// which is the difference between the two `InitColor` shapes real files
    /// carry.
    pub(crate) fn attribute(
        width: u32,
        height: u32,
        packing: Packing,
        fill: Option<u8>,
    ) -> Vec<u8> {
        let columns = (width as usize).div_ceil(BLOCK) as u32;
        let rows = (height as usize).div_ceil(BLOCK) as u32;
        let (first, second) = (packing.first as u32, packing.second as u32);
        let parameter = field(
            "Parameter",
            &ints(&[
                width,
                height,
                columns,
                rows,
                33,
                first,
                second,
                first + second,
                PLANE as u32,
                second,
                second * BLOCK as u32,
                1,
                BLOCK as u32,
                PLANE as u32,
                BLOCK as u32,
                BLOCK as u32,
                8,
                8,
                0,
                0,
            ]),
        );
        let init = match fill {
            None => ints(&[20, 0, 0, 0, 4]),
            Some(v) => {
                let mut words = vec![20u32, 1, (u32::from(v) << 24) | 0x00ff_ffff, second, 4];
                words.extend(std::iter::repeat_n(0xffff_ffffu32, packing.second));
                ints(&words)
            }
        };
        let init = field("InitColor", &init);
        let mut sizes = ints(&[12, columns * rows, 4]);
        sizes.extend(std::iter::repeat_n(0u8, (columns * rows) as usize * 4));
        let sizes = field("BlockSize", &sizes);

        let mut out = ints(&[
            16,
            parameter.len() as u32,
            init.len() as u32,
            sizes.len() as u32,
        ]);
        out.extend_from_slice(&parameter);
        out.extend_from_slice(&init);
        out.extend_from_slice(&sizes);
        out
    }

    /// One block record, present or absent.
    pub(crate) fn block(pixels: Option<&[u8]>, packing: Packing) -> Vec<u8> {
        let declared = packing.block_len() as u32;
        let mut head = Vec::new();
        for v in [0u32, declared, BLOCK as u32, BLOCK as u32] {
            head.extend_from_slice(&v.to_be_bytes());
        }
        let Some(pixels) = pixels else {
            head.extend_from_slice(&0u32.to_be_bytes());
            head.extend_from_slice(&record("BlockDataEndChunk", &[])[4..]);
            return record("BlockDataBeginChunk", &head);
        };
        head.extend_from_slice(&1u32.to_be_bytes());

        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(pixels).expect("deflate");
        let stream = encoder.finish().expect("deflate");
        // The pair of lengths real files carry: big-endian counting the second
        // field, then little-endian counting the stream alone.
        head.extend_from_slice(&(stream.len() as u32 + 4).to_be_bytes());
        head.extend_from_slice(&(stream.len() as u32).to_le_bytes());
        head.extend_from_slice(&stream);
        head.extend_from_slice(&record("BlockDataEndChunk", &[])[4..]);
        debug_assert_eq!(END_MARKER, record("BlockDataEndChunk", &[]).len() - 4);
        record("BlockDataBeginChunk", &head)
    }

    /// A whole `BlockData` blob: one record per block, then `BlockStatus`.
    pub(crate) fn block_data(blocks: &[Option<Vec<u8>>], packing: Packing) -> Vec<u8> {
        let mut out = Vec::new();
        for b in blocks {
            out.extend_from_slice(&block(b.as_deref(), packing));
        }
        out.extend_from_slice(&record("BlockStatus", &[0u8; 8]));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASK: Packing = Packing {
        first: 1,
        second: 0,
    };
    const COLOUR: Packing = Packing {
        first: 1,
        second: 4,
    };

    /// The three numbers a bitmap's packing is stated by have to agree with
    /// each other, because everything downstream slices by them.
    #[test]
    fn a_packing_whose_own_numbers_disagree_is_refused() {
        // Built by hand rather than through the fixture, which cannot state a
        // sum that disagrees with its own parts — which is the point here.
        let build = |first: u32, second: u32, total: u32| {
            let mut parameter: Vec<u8> = Vec::new();
            for v in [512u32, 512, 2, 2, 33, first, second, total] {
                parameter.extend_from_slice(&v.to_be_bytes());
            }
            let mut out = 16u32.to_be_bytes().to_vec();
            for v in [0u32, 0, 0] {
                out.extend_from_slice(&v.to_be_bytes());
            }
            out.extend_from_slice(&9u32.to_be_bytes());
            out.extend_from_slice(&fixture::utf16be("Parameter"));
            out.extend_from_slice(&parameter);
            out
        };

        let ok = parse_attribute(&build(1, 4, 5), 16384).expect("a legal packing");
        assert_eq!(ok.packing, Some(COLOUR));
        assert_eq!(COLOUR.block_len(), 5 * PLANE);

        // A packing that does not add up is **no packing**, and deliberately
        // not a refused bitmap: `csmaterial` reads the same blob without
        // reading these words, so a refusal here would tighten a brush
        // importer over a field it ignores. The three shapes are a sum that
        // disagrees with its parts, more channels than Clip Studio writes, and
        // no first plane at all.
        for bad in [build(1, 4, 6), build(2, 4, 6), build(0, 4, 4)] {
            assert_eq!(
                parse_attribute(&bad, 16384)
                    .expect("the bitmap still reads")
                    .packing,
                None
            );
        }
    }

    /// **An absent block is not always empty**, and the file says which.
    ///
    /// A mask's `InitColor` states all-ones, because a Clip Studio mask starts
    /// revealing everything; a raster layer's states nothing, because its
    /// untouched corners are transparent. Reading the second rule for both
    /// would blank every masked layer in the document, silently.
    #[test]
    fn the_fill_an_absent_block_carries_is_read_and_not_assumed() {
        assert_eq!(
            parse_attribute(&fixture::attribute(300, 300, MASK, Some(255)), 16384)
                .expect("a mask")
                .fill,
            Fill::Stated(255)
        );
        assert_eq!(
            parse_attribute(&fixture::attribute(300, 300, COLOUR, None), 16384)
                .expect("a layer")
                .fill,
            Fill::Empty
        );
        // An `Attribute` whose sections do not add up is not one this layout
        // can be read out of, and saying so is not the same as saying "empty".
        let mut short = fixture::attribute(300, 300, COLOUR, None);
        short.truncate(short.len() - 4);
        assert_eq!(
            parse_attribute(&short, 16384)
                .expect("the Parameter is intact")
                .fill,
            Fill::Unknown
        );
    }

    /// The grid, the packing and the blocks come back off a blob the fixture
    /// wrote, in order, with the absent one still absent — and a blob whose
    /// block count does not match the grid takes the whole bitmap.
    #[test]
    fn a_block_data_blob_reads_back_block_for_block() {
        let bitmap = parse_attribute(&fixture::attribute(300, 300, MASK, None), 16384)
            .expect("an attribute");
        assert_eq!((bitmap.columns, bitmap.rows), (2, 2));
        let packing = bitmap.packing.expect("a packing");

        let written: Vec<Option<Vec<u8>>> = (0..4)
            .map(|i| (i != 2).then(|| vec![(i * 17) as u8; packing.block_len()]))
            .collect();
        let blob = fixture::block_data(&written, packing);

        let mut read: Vec<Option<Vec<u8>>> = vec![None; 4];
        for_each_block(&blob, packing, 4, |i, block| read[i] = Some(block.to_vec()))
            .expect("the blocks");
        assert_eq!(read, written);

        // The grid and the blob have to agree about how many there are.
        assert!(for_each_block(&blob, packing, 3, |_, _| {}).is_none());
        assert!(for_each_block(&blob, packing, 5, |_, _| {}).is_none());
    }

    /// A block whose declared size disagrees with the bitmap's stated packing
    /// is refused rather than sliced by whichever number is smaller.
    #[test]
    fn a_block_that_disagrees_with_the_packing_is_refused() {
        let payload = fixture::block(Some(&vec![7u8; COLOUR.block_len()]), COLOUR);
        let chunk = record(&payload, 0).expect("a record");
        assert!(decode_block(chunk.payload, Some(COLOUR)).is_some());
        assert!(decode_block(chunk.payload, Some(MASK)).is_none());
    }

    /// A picture whose two dimensions are individually plausible and whose
    /// product is not. `checked_mul` waves this through; the bound is what
    /// stops it, and the bound belongs to the caller rather than to the file.
    #[test]
    fn a_bitmap_larger_than_the_caller_allows_is_refused_before_it_is_believed() {
        let (w, h) = (2_000_000_000u32, 2_000_000_000u32);
        let mut parameter: Vec<u8> = Vec::new();
        for v in [w, h, w.div_ceil(256), h.div_ceil(256)] {
            parameter.extend_from_slice(&v.to_be_bytes());
        }
        let mut out = 16u32.to_be_bytes().to_vec();
        for v in [0u32, 0, 0] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&9u32.to_be_bytes());
        out.extend_from_slice(&fixture::utf16be("Parameter"));
        out.extend_from_slice(&parameter);
        assert!(parse_attribute(&out, 16384).is_none());
    }

    /// Nothing here may panic on rubbish, whatever the length.
    #[test]
    fn short_and_random_input_is_refused_rather_than_read_past() {
        for len in 0..64usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 % 251) as u8).collect();
            assert!(parse_attribute(&bytes, 16384).is_none());
            // Called for the panic rather than the answer: these are the four
            // doors a stranger's bytes come through.
            let _ = record(&bytes, 0);
            let _ = decode_block(&bytes, None);
            let _ = field(&bytes, 0);
            let _ = for_each_block(&bytes, COLOUR, 4, |_, _| {});
        }
    }
}
