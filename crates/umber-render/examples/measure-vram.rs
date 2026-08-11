//! Whether a real document's layers actually fit on this card, by putting them
//! there.
//!
//! ```sh
//! cargo run --release -p umber-render --example measure-vram -- "~/Desktop/Valorants magical bitches.clip"
//! ```
//!
//! `measure-atlas` in `umber-core` computes what the reservation *would* be from
//! the piece set, with no device in it. This is the other half: it creates the
//! real [`CanvasRenderer`] at that page count and runs the real upload loop
//! `App::install_import` runs, so the answer is "the card took it" rather than
//! "the arithmetic says it should".
//!
//! Written to settle one claim, which is that the 20000×5000 Clip Studio
//! document Umber refused at 19.7 GB opens once a layer costs what it covers.
//! Two readings come back and both matter:
//!
//! - **pages**, which is what the atlas actually allocated. This grows past the
//!   reservation if the residency the pieces reach is larger than the estimate;
//!   it cannot be smaller.
//! - **tiles backed**, against the tiles a dense store would have taken. That
//!   is the occupancy the whole design turns on, measured on the GPU side rather
//!   than from the file.
//!
//! It is an example rather than a test for the reason `measure-effects` is: it
//! wants gigabytes of a real card and a document nobody but the artist has.

use std::path::PathBuf;

use glam::UVec2;
use umber_core::docimport;
use umber_core::tile::Grid;
use umber_render::CanvasRenderer;
use umber_render::gpu::{Choice, Gpu};

fn gigabytes(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.2} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / (1u64 << 20) as f64)
    }
}

fn main() {
    let mut choice = Choice::Best;
    let mut path: Option<PathBuf> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--fallback" => choice = Choice::Fallback,
            _ => path = Some(PathBuf::from(arg)),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: measure-vram [--fallback] <document>");
        std::process::exit(2);
    };

    let gpu = pollster::block_on(Gpu::with_adapter(Gpu::create_instance(), None, choice))
        .unwrap_or_else(|e| panic!("{e}"));
    let info = gpu.adapter.get_info();
    println!(
        "adapter: {} ({:?}, {:?}), max texture dimension {}",
        info.name,
        info.device_type,
        info.backend,
        gpu.adapter.limits().max_texture_dimension_2d
    );

    let imported = match docimport::import(&path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("could not read it: {e}");
            std::process::exit(1);
        }
    };
    let opened = imported.open();
    let size = opened.document.size;
    let grid = Grid::new(UVec2::new(size.x, size.y));
    let page = grid.page_size();
    let page_bytes = u64::from(page.x) * u64::from(page.y) * 4;

    // Exactly `App::install_import`'s reservation.
    let mut tiles = 0usize;
    for upload in &opened.uploads {
        let mut seen: Vec<(u32, u32)> = upload
            .pieces
            .iter()
            .flat_map(|p| grid.tiles_over(p.rect))
            .collect();
        seen.sort_unstable();
        seen.dedup();
        tiles += seen.len();
    }
    let pages = (tiles as u32).div_ceil(grid.tiles_per_page().max(1));
    println!(
        "{} × {}, {} slice(s), page {} × {} ({} each)",
        size.x,
        size.y,
        opened.uploads.len(),
        page.x,
        page.y,
        gigabytes(page_bytes),
    );
    println!(
        "reserving {pages} page(s) = {} against a dense {}",
        gigabytes(u64::from(pages) * page_bytes),
        gigabytes(opened.uploads.len() as u64 * u64::from(size.x) * u64::from(size.y) * 4),
    );

    let mut canvas = match CanvasRenderer::try_new(
        &gpu.device,
        &gpu.queue,
        UVec2::new(size.x, size.y),
        wgpu::TextureFormat::Bgra8Unorm,
        pages,
    ) {
        Ok(c) => c,
        Err(refused) => {
            println!(
                "REFUSED: the card would not hold {} page(s), {}",
                refused.slices,
                gigabytes(refused.peak_bytes())
            );
            std::process::exit(1);
        }
    };
    canvas.clear_all_layers(&gpu.queue);

    let started = std::time::Instant::now();
    for upload in &opened.uploads {
        for piece in &upload.pieces {
            canvas.write_layer_rect(
                &gpu.device,
                &gpu.queue,
                upload.slot,
                piece.rect,
                &piece.bytes,
            );
        }
    }
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    let took = started.elapsed();

    let backed: usize = opened
        .uploads
        .iter()
        .map(|u| canvas.backed_tiles(u.slot))
        .sum();
    let dense_tiles = opened.uploads.len() * grid.tiles_per_page() as usize;
    println!(
        "uploaded in {:.1?}: {} page(s) allocated = {}",
        took,
        canvas.page_count(),
        gigabytes(u64::from(canvas.page_count()) * page_bytes),
    );
    println!(
        "{backed} tile(s) backed of a dense {dense_tiles} ({:.1}%), {} free in the pool",
        backed as f64 / dense_tiles.max(1) as f64 * 100.0,
        canvas.free_tiles(),
    );
    println!("it opened.");
}
