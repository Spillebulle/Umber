//! LZF decompression, for Krita's layer tiles.
//!
//! Krita compresses each tile with liblzf, so reading a `.kra` means having a
//! decompressor. This is forty lines of it rather than a dependency: the only
//! crate on offer, `lzf`, has not been touched since 2015 and wraps the same
//! algorithm in an API with `panic!` in it. LZF is a fixed, published format —
//! there is no upstream to track — so the cost of owning it is close to zero
//! and the benefit is that a corrupt file returns `None` instead of aborting.
//!
//! The format, from liblzf's `lzf_d.c`:
//!
//! - A control byte below 32 introduces a literal run of `ctrl + 1` bytes.
//! - Anything else is a back-reference. `ctrl >> 5` is the length, extended by
//!   a following byte when it reaches 7, and `ctrl & 0x1f` supplies the high
//!   five bits of the distance, whose low eight come from the next byte.
//!   The match is `length + 2` bytes long, starting `distance + 1` back.
//!
//! Back-references may overlap the output cursor — that is how runs are
//! encoded — so the copy has to be byte at a time rather than a slice move.

/// Decompress into a buffer of exactly `expected` bytes.
///
/// Returns `None` for any malformed stream, including one that decodes to the
/// wrong length: tiles have a size known in advance, and a stream that fills
/// less than the tile is corrupt rather than short.
pub fn decompress(input: &[u8], expected: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(expected);
    let mut ip = 0usize;

    while ip < input.len() {
        let ctrl = input[ip] as usize;
        ip += 1;

        if ctrl < 32 {
            let len = ctrl + 1;
            let end = ip.checked_add(len)?;
            if end > input.len() || out.len() + len > expected {
                return None;
            }
            out.extend_from_slice(&input[ip..end]);
            ip = end;
        } else {
            let mut len = ctrl >> 5;
            if len == 7 {
                len += *input.get(ip)? as usize;
                ip += 1;
            }
            let low = *input.get(ip)? as usize;
            ip += 1;

            let distance = ((ctrl & 0x1f) << 8) | low;
            let start = out.len().checked_sub(distance + 1)?;
            let len = len + 2;
            if out.len() + len > expected {
                return None;
            }
            // Byte at a time: a match may overlap the cursor, which is how a
            // run of one value is encoded. A slice copy would read the source
            // before the earlier bytes of the same match were written.
            for from in start..start + len {
                let byte = out[from];
                out.push(byte);
            }
        }
    }

    (out.len() == expected).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_run_is_copied_verbatim() {
        // ctrl 2 -> three literal bytes.
        assert_eq!(decompress(&[2, b'a', b'b', b'c'], 3).unwrap(), b"abc");
    }

    #[test]
    fn a_back_reference_repeats_earlier_output() {
        // Hand-assembled rather than round-tripped through a compressor of our
        // own, which would only prove the two agree with each other.
        //
        //   00 'a'        literal run of one: "a"
        //   20 00         ctrl 0x20 -> len 1+2 = 3, distance (0<<8|0)+1 = 1
        //                 so copy three bytes starting one back, overlapping.
        let out = decompress(&[0, b'a', 0x20, 0x00], 4).unwrap();
        assert_eq!(out, b"aaaa");
    }

    #[test]
    fn a_long_match_uses_the_extension_byte() {
        //   00 'x'        literal "x"
        //   E0 05 00      len 7 + 5 + 2 = 14, distance 1
        let out = decompress(&[0, b'x', 0xE0, 0x05, 0x00], 15).unwrap();
        assert_eq!(out, vec![b'x'; 15]);
    }

    #[test]
    fn a_truncated_stream_is_rejected() {
        // A literal run claiming more bytes than remain.
        assert!(decompress(&[5, b'a'], 6).is_none());
    }

    #[test]
    fn a_reference_before_the_start_is_rejected() {
        // Nothing has been emitted yet, so there is nothing to point back at —
        // this is the read that would panic if the subtraction were unchecked.
        assert!(decompress(&[0x20, 0x00], 3).is_none());
    }

    #[test]
    fn overrunning_the_tile_is_rejected() {
        // Trusting the stream's length over the tile's is how a decompressor
        // turns a corrupt file into unbounded memory use.
        assert!(decompress(&[2, b'a', b'b', b'c'], 2).is_none());
    }

    #[test]
    fn a_short_result_is_rejected() {
        assert!(decompress(&[0, b'a'], 4).is_none());
    }
}
