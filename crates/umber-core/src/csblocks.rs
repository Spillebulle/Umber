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
    /// Nothing: the ordinary case, and every raster layer in every sample.
    Empty,
    /// A colour the file states. Read as a *flag* and never as a colour —
    /// what the section holds beyond that has not been established against a
    /// real file, so a caller must refuse rather than paint something plausible.
    Stated,
    /// The `InitColor` section could not be located. Older files and the
    /// material fixtures both land here; both are read as [`Fill::Empty`],
    /// which is what every reader of this format does.
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
    let packing = match (be32(parameter, 20), be32(parameter, 24), be32(parameter, 28)) {
        (Some(first), Some(second), Some(total)) => {
            let (first, second) = (first as usize, second as usize);
            // Refused rather than repaired: a file whose own three numbers
            // disagree is not one to guess the layout of.
            if first + second != total as usize || first + second > MAX_CHANNELS || first == 0 {
                return None;
            }
            Some(Packing { first, second })
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
    let (Some(info), Some(extra)) = (be32(attribute, 4), be32(attribute, 8)) else {
        return Fill::Unknown;
    };
    let Some(at) = head.checked_add(info as usize) else {
        return Fill::Unknown;
    };
    // The section has to end inside the blob, or this is not the layout being
    // read and nothing below it means anything. `checked_add` first, because an
    // overflowing sum would otherwise compare as small.
    match at.checked_add(extra as usize) {
        Some(end) if end <= attribute.len() => {}
        _ => return Fill::Unknown,
    }
    let Some((name, body)) = field(attribute, at) else {
        return Fill::Unknown;
    };
    if name != "InitColor" {
        return Fill::Unknown;
    }
    // `[u32][u32 flag]…`. Only the flag is read: what follows it is a colour
    // whose form has never been checked against a file that uses one.
    match be32(body, 4) {
        Some(0) => Fill::Empty,
        Some(_) => Fill::Stated,
        None => Fill::Unknown,
    }
}

/// Every block of a `BlockData` blob, in grid order.
///
/// An inner `None` is a block Clip Studio did not store — the ordinary state of
/// an untouched corner of a layer. An outer `None` is a block this reader could
/// not make sense of, and takes the whole bitmap with it rather than leaving a
/// hole somebody would read as the artist's own transparency.
pub(crate) fn blocks(blob: &[u8], packing: Packing) -> Option<Vec<Option<Vec<u8>>>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(chunk) = record(blob, at) {
        at = chunk.end;
        if chunk.name != "BlockDataBeginChunk" {
            continue;
        }
        out.push(decode_block(chunk.payload, Some(packing))?);
    }
    Some(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The three numbers a bitmap's packing is stated by have to agree with
    /// each other, because everything downstream slices by them.
    #[test]
    fn a_packing_whose_own_numbers_disagree_is_refused() {
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
            out.extend_from_slice(
                &"Parameter"
                    .encode_utf16()
                    .flat_map(u16::to_be_bytes)
                    .collect::<Vec<u8>>(),
            );
            out.extend_from_slice(&parameter);
            out
        };

        let ok = parse_attribute(&build(1, 4, 5), 16384).expect("a legal packing");
        assert_eq!(ok.packing, Some(Packing { first: 1, second: 4 }));
        assert_eq!(ok.packing.expect("packing").block_len(), 5 * PLANE);

        // The sum is what every slice downstream is taken against.
        assert!(parse_attribute(&build(1, 4, 6), 16384).is_none());
        // Wider than anything Clip Studio writes.
        assert!(parse_attribute(&build(2, 4, 6), 16384).is_none());
        // No alpha plane at all is not a shape this reader knows.
        assert!(parse_attribute(&build(0, 4, 4), 16384).is_none());
    }

    /// A picture whose two dimensions are individually plausible and whose
    /// product is not. `checked_mul` waves this through; the bound is what
    /// stops it, and the bound belongs to the caller rather than to the file.
    #[test]
    fn a_bitmap_larger_than_the_caller_allows_is_refused_before_it_is_believed() {
        let mut parameter: Vec<u8> = Vec::new();
        let (w, h) = (2_000_000_000u32, 2_000_000_000u32);
        for v in [w, h, w.div_ceil(256), h.div_ceil(256)] {
            parameter.extend_from_slice(&v.to_be_bytes());
        }
        let mut out = 16u32.to_be_bytes().to_vec();
        for v in [0u32, 0, 0] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&9u32.to_be_bytes());
        out.extend_from_slice(
            &"Parameter"
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<u8>>(),
        );
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
            let _ = blocks(&bytes, Packing { first: 1, second: 4 });
        }
    }
}
