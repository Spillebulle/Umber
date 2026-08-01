//! Bitmap brush tips.
//!
//! A tip is an 8-bit coverage mask stamped in place of the procedural round
//! falloff. It **modulates coverage** and nothing else: the dab pass still
//! writes a single channel into the stroke scratch with a `max` blend, so the
//! wet-layer invariant in `CLAUDE.md` is untouched — a tipped stroke saturates
//! at 1.0 under overlap exactly as a round one does, and stroke opacity is
//! still applied once at commit.
//!
//! Plain bytes, no GPU types: `umber-render` uploads one of these to an
//! `R8Unorm` texture, and the engine stays testable without a device.
//!
//! On disk a tip is an 8-bit greyscale PNG in the brush library's `tips/`
//! directory — see [`crate::preset::UserLibrary`] for why the library is a
//! directory rather than the single RON file it used to be.

use crate::preset::PresetError;

/// An 8-bit coverage mask, row-major from the top-left.
///
/// `0` is no paint and `255` is full paint. That is the same convention GIMP's
/// `.gbr` uses, which is why the importer needs no inversion — worth stating,
/// because half the brush formats in the world use the opposite one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipMask {
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

impl TipMask {
    /// Refuse anything bigger than this in either axis.
    ///
    /// Device limits are `downlevel_defaults`, which guarantees only 2048, and
    /// a tip is stamped once per dab — a 4096² tip would be both unbindable on
    /// a mobile GPU and far larger than any brush needs.
    pub const MAX_SIZE: u32 = 2048;

    /// `coverage` must hold exactly `width * height` bytes.
    pub fn new(width: u32, height: u32, coverage: Vec<u8>) -> Result<Self, PresetError> {
        if width == 0 || height == 0 {
            return Err(PresetError::Malformed(
                None,
                "a brush tip cannot be empty".to_string(),
            ));
        }
        if width > Self::MAX_SIZE || height > Self::MAX_SIZE {
            return Err(PresetError::Malformed(
                None,
                format!(
                    "brush tip is {width}x{height}, larger than {}",
                    Self::MAX_SIZE
                ),
            ));
        }
        let expected = width as usize * height as usize;
        if coverage.len() != expected {
            return Err(PresetError::Malformed(
                None,
                format!(
                    "brush tip has {} bytes, expected {expected}",
                    coverage.len()
                ),
            ));
        }
        Ok(Self {
            width,
            height,
            coverage,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Row-major coverage, `width * height` bytes.
    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    /// Coverage at one texel. Out of range reads as no paint rather than
    /// panicking — a tip is data from a file somebody else wrote.
    pub fn at(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.coverage[(y * self.width + x) as usize]
    }

    /// The same mask centred — to the nearest texel — in a square of empty
    /// coverage.
    ///
    /// The dab pass stretches a tip over the dab's bounding box, so a mask that
    /// is not square comes out squashed. Padding is the fix rather than
    /// [`crate::Brush::dab_ratio`], for two reasons: the ratio's long axis is
    /// the dab's *x* axis, so a portrait mask would additionally have to be
    /// rotated by a quarter turn and rotated back by `dab_angle`, and the ratio
    /// is the user's setting — spending it on the file's proportions would mean
    /// a stamp brush could never be deliberately squashed. The cost is shading
    /// an empty margin, which for the near-square masks brush packs actually
    /// contain is a few per cent.
    ///
    /// Already-square masks are returned unchanged.
    pub fn padded_to_square(self) -> Self {
        let side = self.width.max(self.height);
        if self.width == side && self.height == side {
            return self;
        }
        let x0 = (side - self.width) / 2;
        let y0 = (side - self.height) / 2;
        let mut coverage = vec![0u8; side as usize * side as usize];
        for y in 0..self.height {
            let src = (y * self.width) as usize;
            let dst = ((y + y0) * side + x0) as usize;
            coverage[dst..dst + self.width as usize]
                .copy_from_slice(&self.coverage[src..src + self.width as usize]);
        }
        Self {
            width: side,
            height: side,
            coverage,
        }
    }

    /// Encode as an 8-bit greyscale PNG — how a tip is stored in the library.
    ///
    /// PNG rather than the raw bytes because it carries its own dimensions and
    /// because the files are then ordinary images: a tip can be looked at,
    /// replaced or copied between machines with anything that opens a picture,
    /// which matters for a format whose whole content is somebody else's
    /// bitmap. `png` is already a dependency of this crate for document import,
    /// so it costs nothing to build.
    pub fn to_png(&self) -> Result<Vec<u8>, PresetError> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|e| malformed(format!("the brush tip could not be written ({e})")))?;
            writer
                .write_image_data(&self.coverage)
                .map_err(|e| malformed(format!("the brush tip could not be written ({e})")))?;
        }
        Ok(out)
    }

    /// Decode a tip written by [`TipMask::to_png`].
    ///
    /// Only greyscale is accepted. A tip *is* a coverage mask, and for a
    /// colour image there is no answer to "which channel is the coverage" that
    /// is right for every file — guessing produces a brush that is quietly the
    /// wrong shape, which is worse than a sentence saying what was expected.
    pub fn from_png(bytes: &[u8]) -> Result<Self, PresetError> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        // Expands a 1/2/4-bit or 16-bit greyscale file to the 8 bits the mask
        // holds, so a tip dropped in by hand does not have to be exactly what
        // Umber writes.
        decoder.set_transformations(png::Transformations::normalize_to_color8());

        let mut reader = decoder.read_info().map_err(|e| {
            malformed(format!(
                "the brush tip's PNG header could not be read ({e})"
            ))
        })?;
        let size = reader
            .output_buffer_size()
            .ok_or_else(|| malformed("the brush tip is too large to decode".to_string()))?;
        let mut buf = vec![0u8; size];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| malformed(format!("the brush tip could not be decoded ({e})")))?;

        let texels = info.width as usize * info.height as usize;
        let coverage = match info.color_type {
            png::ColorType::Grayscale => {
                buf.truncate(texels);
                buf
            }
            // Written by some editors when the image has an alpha channel it
            // does not use. The grey is still the coverage.
            png::ColorType::GrayscaleAlpha => {
                buf[..texels * 2].chunks_exact(2).map(|px| px[0]).collect()
            }
            other => {
                return Err(malformed(format!(
                    "a brush tip must be an 8-bit greyscale PNG, and this one is {other:?}"
                )));
            }
        };
        Self::new(info.width, info.height, coverage)
    }
}

fn malformed(message: String) -> PresetError {
    PresetError::Malformed(None, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_reads_back_row_major() {
        let tip = TipMask::new(3, 2, vec![1, 2, 3, 4, 5, 6]).expect("build");
        assert_eq!(tip.at(0, 0), 1);
        assert_eq!(tip.at(2, 0), 3);
        assert_eq!(tip.at(0, 1), 4);
        assert_eq!(tip.at(2, 1), 6);
    }

    #[test]
    fn out_of_range_is_empty_rather_than_a_panic() {
        let tip = TipMask::new(2, 2, vec![255; 4]).expect("build");
        assert_eq!(tip.at(2, 0), 0);
        assert_eq!(tip.at(0, 2), 0);
        assert_eq!(tip.at(u32::MAX, u32::MAX), 0);
    }

    #[test]
    fn a_mismatched_buffer_is_refused() {
        assert!(TipMask::new(4, 4, vec![0; 15]).is_err());
        assert!(TipMask::new(4, 4, vec![0; 17]).is_err());
        assert!(TipMask::new(0, 4, vec![]).is_err());
        assert!(TipMask::new(4096, 1, vec![0; 4096]).is_err());
    }

    /// The whole point of storing tips as files: what comes back has to be the
    /// mask that went in, to the byte. A lossy step here would change the shape
    /// of every stamp brush every time the library was reloaded.
    #[test]
    fn a_mask_survives_a_png_round_trip_byte_for_byte() {
        let coverage: Vec<u8> = (0..(17 * 5)).map(|i| (i * 7 % 256) as u8).collect();
        let tip = TipMask::new(17, 5, coverage.clone()).expect("build");
        let png = tip.to_png().expect("encode");
        let back = TipMask::from_png(&png).expect("decode");

        assert_eq!(back.width(), 17);
        assert_eq!(back.height(), 5);
        assert_eq!(back.coverage(), coverage);
        assert_eq!(back, tip);
    }

    #[test]
    fn a_colour_png_is_refused_rather_than_guessed_at() {
        // Which channel would the coverage be? There is no answer that is right
        // for every file, and a wrong guess is a brush that is quietly the
        // wrong shape.
        let mut rgb = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut rgb, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[10, 20, 30]).expect("data");
        }
        let err = TipMask::from_png(&rgb).expect_err("colour is refused");
        assert!(err.to_string().contains("greyscale"), "{err}");

        assert!(TipMask::from_png(b"not a png").is_err());
    }

    #[test]
    fn padding_centres_a_mask_without_stretching_it() {
        // A 2x4 mask becomes 4x4 with its two columns in the middle — a
        // portrait or landscape stamp has to keep its proportions, and the dab
        // stretches whatever it is given over a square.
        let tall = TipMask::new(2, 4, vec![9; 8]).expect("build");
        let square = tall.padded_to_square();
        assert_eq!((square.width(), square.height()), (4, 4));
        #[rustfmt::skip]
        let expected = [
            0, 9, 9, 0,
            0, 9, 9, 0,
            0, 9, 9, 0,
            0, 9, 9, 0,
        ];
        assert_eq!(square.coverage(), expected);

        // Landscape is the same rule turned on its side.
        let wide = TipMask::new(4, 2, vec![9; 8]).expect("build");
        let square = wide.padded_to_square();
        assert_eq!((square.width(), square.height()), (4, 4));
        assert_eq!(&square.coverage()[..4], [0, 0, 0, 0]);
        assert_eq!(&square.coverage()[4..12], [9; 8]);

        // An already-square mask is left exactly alone.
        let round = TipMask::new(2, 2, vec![1, 2, 3, 4]).expect("build");
        assert_eq!(round.clone().padded_to_square(), round);
    }
}
