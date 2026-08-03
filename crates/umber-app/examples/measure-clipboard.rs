//! Re-derive the two figures `sysclip` quotes, and settle the macOS question.
//!
//! ```sh
//! cargo run --release -p umber-app --example measure-clipboard
//! ```
//!
//! `measure-history.rs`, `measure-undo.rs` and `measure-pressure.rs` exist so
//! that a number in a comment can be checked rather than believed, and CLAUDE.md
//! says to re-run them before rebuilding an argument from memory. `sysclip`
//! quotes two, and both decide something:
//!
//! * **Is the round trip exact?** [`sysclip::TRANSPORT_IS_EXACT`] gates whether
//!   the echo is taken at all, so getting it wrong on a platform means an
//!   artist's own copy can come back changed. This is the check.
//! * **What does a copy cost?** About 8 ms per megabyte each way is what the
//!   module says a copy pays on top of its readback, and it is the reason the
//!   docs admit a three-second freeze on a very large canvas.
//!
//! **This touches the real clipboard, which is exactly why it is an example and
//! not a test.** No test in Umber may: a CI runner may have no display server,
//! and grabbing somebody's clipboard while they work is hostile. Running this
//! replaces whatever is on the clipboard — deliberately, and only when asked.
//!
//! # Running it on a Mac is the point
//!
//! Nobody working on Umber has one, so the macOS clipboard path has never been
//! executed. `sysclip` *suspects* that arboard's `NSImage` write and TIFF read
//! carry premultiplied alpha that `image` does not undo, and the echo exists to
//! make that harmless either way. One run of this on a Mac says which it is.
//! The exactness sweep below covers every alpha from 0 to 255, so a
//! premultiply — the identity on anything opaque — cannot hide in it.

use std::borrow::Cow;
use std::time::Instant;

fn main() {
    println!("Umber clipboard measurements. This overwrites the system clipboard.\n");
    match arboard::Clipboard::new() {
        Ok(mut board) => {
            exactness(&mut board);
            println!();
            cost(&mut board);
        }
        Err(e) => {
            eprintln!("no clipboard on this machine: {e}");
            eprintln!("On Linux that is usually no display server, or a Wayland");
            eprintln!("compositor without wlr-data-control and no XWayland behind it.");
            std::process::exit(1);
        }
    }
}

/// Does `get_image` hand back what `set_image` was given?
///
/// Every alpha from 0 to 255 at a colour that is not grey, so a premultiply, a
/// channel swap and a row flip all show up. The colours are **not** scaled by
/// alpha: `Clip` holds straight alpha, which is what Umber hands over.
fn exactness(board: &mut arboard::Clipboard) {
    let (w, h) = (16usize, 16usize);
    let mut bytes = Vec::with_capacity(w * h * 4);
    for i in 0..(w * h) {
        let a = (i * 255 / (w * h - 1)) as u8;
        bytes.extend_from_slice(&[200, 90, 30, a]);
    }

    if let Err(e) = board.set_image(arboard::ImageData {
        width: w,
        height: h,
        bytes: Cow::Borrowed(&bytes),
    }) {
        println!("exactness: the clipboard would not take the picture: {e}");
        return;
    }
    let back = match board.get_image() {
        Ok(back) => back,
        Err(e) => {
            println!("exactness: written, but could not be read back: {e}");
            return;
        }
    };

    if back.width != w || back.height != h {
        println!(
            "exactness: NOT EXACT — {}x{} went out and {}x{} came back",
            w, h, back.width, back.height
        );
        return;
    }
    let mut worst = 0i32;
    let mut differing = 0usize;
    let mut first: Option<(usize, u8, u8)> = None;
    for (i, (out, back)) in bytes.iter().zip(back.bytes.iter()).enumerate() {
        let d = (*out as i32 - *back as i32).abs();
        if d != 0 {
            differing += 1;
            worst = worst.max(d);
            first.get_or_insert((i, *out, *back));
        }
    }
    if differing == 0 {
        println!("exactness: EXACT — every byte of every alpha survived.");
        println!("           TRANSPORT_IS_EXACT may be true for this platform.");
        return;
    }
    println!("exactness: NOT EXACT — {differing} bytes differ, worst by {worst}.");
    if let Some((i, out, back)) = first {
        let px = i / 4;
        println!(
            "           first at pixel {px} channel {} : {out} -> {back} (alpha there was {})",
            i % 4,
            bytes[px * 4 + 3],
        );
    }
    println!("           TRANSPORT_IS_EXACT must be false here, and the echo is");
    println!("           what keeps a copy and a paste straight back exact.");
}

/// What `set_image` and `get_image` cost per megabyte.
///
/// Three sizes, because the per-megabyte figure is not flat: the fixed cost of
/// opening the clipboard dominates a small picture. Content that is neither
/// flat nor noise, since a painting is somewhere between and PNG's compressor
/// is what is being timed.
fn cost(board: &mut arboard::Clipboard) {
    println!("cost (release build; a debug build is several times slower):");
    for side in [1024usize, 2048, 4096] {
        let mut bytes = Vec::with_capacity(side * side * 4);
        for y in 0..side {
            for x in 0..side {
                let r = ((x * 255) / side) as u8;
                let g = ((y * 255) / side) as u8;
                let b = (((x ^ y) & 0xff) as u8).wrapping_mul(3);
                bytes.extend_from_slice(&[r, g, b, 255]);
            }
        }
        let mb = bytes.len() as f64 / (1024.0 * 1024.0);

        let t = Instant::now();
        let wrote = board.set_image(arboard::ImageData {
            width: side,
            height: side,
            bytes: Cow::Borrowed(&bytes),
        });
        let set = t.elapsed();
        if let Err(e) = wrote {
            println!("  {side}x{side} ({mb:.0} MB): refused — {e}");
            continue;
        }

        let t = Instant::now();
        let read = board.get_image();
        let get = t.elapsed();
        if let Err(e) = read {
            println!("  {side}x{side} ({mb:.0} MB): written, unreadable — {e}");
            continue;
        }

        println!(
            "  {side}x{side} ({mb:>3.0} MB): set {:>6.0} ms ({:>5.2} ms/MB), \
             get {:>6.0} ms ({:>5.2} ms/MB)",
            set.as_secs_f64() * 1000.0,
            set.as_secs_f64() * 1000.0 / mb,
            get.as_secs_f64() * 1000.0,
            get.as_secs_f64() * 1000.0 / mb,
        );
    }
    println!();
    println!("A copy with nothing selected takes the whole canvas, so multiply the");
    println!("set figure by the canvas in megabytes — and double it where the echo");
    println!("is taken. That is what the three seconds in `sysclip`'s docs is.");
}
