//! What every adapter on this machine will actually hold.
//!
//! ```sh
//! cargo run --release -p umber-render --example measure-limits
//! ```
//!
//! Written to settle "can the canvas ceiling be raised?", which is not Umber's
//! question to answer: `Document::MAX_EDGE` may say anything, but a texture
//! past `max_texture_dimension_2d` is a validation error, and a validation
//! error is fatal. `CanvasLimit::of_device` already clamps what the dialogs
//! offer to this number, so raising the constant above it changes nothing an
//! artist can reach.
//!
//! It reports every backend separately, because the answer differs by API on
//! the same card — and a build that forces one with `WGPU_BACKEND` is getting
//! that backend's ceiling rather than the machine's best.

fn main() {
    let instance = umber_render::Gpu::create_instance();

    println!(
        "{:<34} {:<10} {:>12} {:>14} {:>12}",
        "adapter", "backend", "max 2D", "max buffer", "max layers"
    );
    println!("{}", "-".repeat(88));

    let mut best = 0u32;
    for adapter in pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())) {
        let info = adapter.get_info();
        let limits = adapter.limits();
        best = best.max(limits.max_texture_dimension_2d);
        let name: String = info.name.chars().take(33).collect();
        println!(
            "{name:<34} {:<10} {:>12} {:>13}M {:>12}",
            format!("{:?}", info.backend),
            limits.max_texture_dimension_2d,
            limits.max_buffer_size / (1024 * 1024),
            limits.max_texture_array_layers,
        );
    }

    println!();
    println!("largest square texture any adapter here will make: {best} px");

    // The constant permitting a size and the device making one are different
    // claims, and only the second is worth anything. This asks the adapter
    // Umber would actually pick for a real layer array at the new ceiling.
    let edge = umber_core::document::Document::MAX_EDGE;
    match pollster::block_on(umber_render::Gpu::new(
        umber_render::Gpu::create_instance(),
        None,
    )) {
        Ok(gpu) => {
            let allowed = gpu.device.limits().max_texture_dimension_2d;
            println!(
                "
the adapter Umber picks allows {allowed} px; Document::MAX_EDGE is {edge}"
            );
            if allowed >= edge {
                let made = gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("ceiling probe"),
                    size: wgpu::Extent3d {
                        width: edge,
                        height: edge,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                println!(
                    "  a {edge}² layer was created: {:.1} GB",
                    (edge as f64 * edge as f64 * 4.0) / 1e9
                );
                drop(made);
            } else {
                println!("  this device caps a canvas below the format's ceiling");
            }
        }
        Err(e) => println!(
            "
no device: {e}"
        ),
    }
    // What that costs, which is the other half of whether a ceiling is usable.
    for edge in [16384u64, 32768] {
        let one = edge * edge * 4;
        println!(
            "  a {edge}² layer is {:.1} GB; sixty-four of them is {:.0} GB",
            one as f64 / 1e9,
            (one * 64) as f64 / 1e9,
        );
    }
}
