//! Where the time goes when a document is opened.
//!
//! ```sh
//! cargo run --release -p umber-core --example measure-open -- big.clip
//! ```
//!
//! Written because the application freezes while a large document loads and the
//! obvious fix — a progress bar — cannot be designed without knowing which of
//! the three phases is the wait, and whether any of them can report progress at
//! all. A bar over a phase that reports nothing is the lying control
//! `Stage::progress`'s `Option` already refuses.
//!
//! The three are: reading the file off disk, decoding it into an
//! [`ImportedDocument`], and turning that into a [`LayerStack`] with its
//! uploads. Only the last touches anything the GPU will want, and only the
//! first two are candidates for a worker thread.

use std::path::PathBuf;
use std::time::Instant;

use umber_core::docimport;

fn main() {
    let paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: measure-open <document>...");
        std::process::exit(2);
    }

    println!(
        "{:<40} {:>9} {:>9} {:>9} {:>9}  layers",
        "file", "read", "decode", "open", "total"
    );
    println!("{}", "-".repeat(96));

    for path in &paths {
        let name: String = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .chars()
            .take(39)
            .collect();

        let t0 = Instant::now();
        let Ok(bytes) = std::fs::read(path) else {
            println!("{name:<40}  could not be read");
            continue;
        };
        let read = t0.elapsed();

        // `import` reads the file itself, so this times the whole of it and
        // subtracts the read to get the decode. Close enough for a phase
        // breakdown, and it uses the same public path the application does.
        let t1 = Instant::now();
        let doc = match docimport::import(path) {
            Ok(doc) => doc,
            Err(e) => {
                println!("{name:<40}  refused: {e}");
                continue;
            }
        };
        let decode = t1.elapsed().saturating_sub(read);
        let layers = doc.layers.len();

        let t2 = Instant::now();
        let opened = doc.open();
        let open = t2.elapsed();
        let uploads = opened.uploads.len();

        let total = read + decode + open;
        println!(
            "{name:<40} {:>8.0}ms {:>8.0}ms {:>8.0}ms {:>8.0}ms  {layers} layers, {uploads} uploads",
            read.as_secs_f64() * 1000.0,
            decode.as_secs_f64() * 1000.0,
            open.as_secs_f64() * 1000.0,
            total.as_secs_f64() * 1000.0,
        );
        drop(bytes);
    }
}
