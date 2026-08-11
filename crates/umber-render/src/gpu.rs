//! Device setup.

use std::sync::Arc;

/// Owns the wgpu device and queue.
///
/// Deliberately free of any surface or window reference so the same instance
/// can outlive a surface — on Android the window is destroyed and recreated
/// whenever the app is backgrounded, and rebuilding the device there would mean
/// re-uploading the entire document.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
}

/// The share of the driver's reported memory budget past which a texture or a
/// buffer is refused rather than attempted.
///
/// **This is what makes an out-of-memory allocation reportable at all.** Without
/// it a Vulkan or D3D12 driver may satisfy an allocation it has no room for by
/// paging to system memory — the canvas becomes a slideshow with nothing said —
/// or fail in a way that arrives too late to act on. With it, an allocation the
/// device cannot comfortably hold comes back as an ordinary `Error::OutOfMemory`
/// on the exact call that asked, which `canvas::try_reserve` catches in an error
/// scope and turns into a sentence.
///
/// Three things to know before changing it:
///
/// * **It is best effort and never a guarantee.** Vulkan honours it only where
///   `VK_EXT_memory_budget` is present and returns `Ok` where it is not; Metal
///   and GL support none of it, and D3D12 applies it unconditionally through its
///   sub-allocator. So a refusal proves the card said no; the absence of one
///   proves nothing.
/// * **It cuts both ways, and the other edge is sharp.** The threshold is
///   charged on **buffers as well as textures** — `create_buffer` asks it too —
///   and `Queue::write_texture`'s staging buffer is allocated through wgpu's
///   *fatal* error path, which calls `lose` on an out-of-memory. So this makes
///   an upload that would previously have been attempted refusable, and a
///   refused staging allocation is the lost device rather than a sentence. The
///   window is narrow — `write_layer_rect` bands and waits, so a band is at most
///   `readback_limit`, and a budget too full for that is one the array
///   reservation would already have been refused at — but it is a real trade and
///   not a free win. **`docs/perf/` §4.4's submit fix is what bounds it; nothing
///   makes it catchable.**
/// * **Ninety rather than ninety-nine**, for headroom against everything else a
///   frame allocates beside the array: the stroke scratch, an effect working
///   set, a blended commit's backdrop, and the staging above. An earlier draft
///   of this comment gave the reason as `create_texture`'s internal clear
///   views — that is **wrong** and is recorded rather than deleted, because it
///   is the plausible-sounding reason somebody will reach for again.
///   `vkCreateImageView` allocates no device memory through the sub-allocator
///   and is not budget-checked at all; those views are fatal on failure and this
///   threshold has no bearing on them.
/// * **It is charged on every allocation**, including the per-frame ones — the
///   smudge probe's target, the thumbnail's uniform buffer, a blended commit's
///   backdrop. Each becomes one `vkGetPhysicalDeviceMemoryProperties2`. Believed
///   to be noise and **not measured**.
const MEMORY_BUDGET_PERCENT: u8 = 90;

/// The descriptor [`Gpu::create_instance`] builds, split out so the one thing
/// that is easy to get wrong about it can be tested.
///
/// **`InstanceDescriptor::with_env` silently discards
/// `memory_budget_thresholds`** — it rebuilds the struct with
/// `MemoryBudgetThresholds::default()` — and
/// `new_without_display_handle_from_env()` *is* `new_without_display_handle()`
/// followed by `with_env()`. So the threshold has to be written after that call
/// and not before it, or the whole refusal path above is inert with nothing
/// saying so. `the_memory_budget_threshold_survives_the_environment` is the
/// guard.
///
/// **`for_device_loss` stays unset, and that is the more important half.**
/// Setting it makes the backend deliberately *lose* the device on the next poll
/// under memory pressure, which is precisely the unrecoverable outcome the
/// refusal exists to avoid: a refused allocation is a sentence, a lost device is
/// the crash box.
fn instance_descriptor() -> wgpu::InstanceDescriptor {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    desc.memory_budget_thresholds = wgpu::MemoryBudgetThresholds {
        for_resource_creation: Some(MEMORY_BUDGET_PERCENT),
        for_device_loss: None,
    };
    desc
}

/// Which adapter to ask the instance for.
///
/// Umber itself always wants [`Choice::Best`]. [`Choice::Fallback`] exists for
/// the GPU tests and is the answer to a real problem: CI runners have no
/// graphics card, so `gpu_pipeline.rs` runs there on the driver's software
/// rasteriser — WARP on Windows, lavapipe on Linux — while a developer's
/// machine runs it on hardware. The two do not round identically, so a test
/// asserting an exact byte passes here and fails there, and the failure is
/// found *after* a tag has been pushed. Being able to ask for the same
/// rasteriser CI will use is what turns that into something reproducible
/// before it is pushed. See `shared_gpu` in the tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Choice {
    /// Whatever the machine has, preferring a discrete card.
    #[default]
    Best,
    /// The software rasteriser, even where there is a card.
    Fallback,
}

impl Gpu {
    /// Create a device suitable for the given surface.
    pub async fn new(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'static>>,
    ) -> Result<Self, String> {
        Self::with_adapter(instance, compatible_surface, Choice::Best).await
    }

    /// As [`Gpu::new`], choosing the adapter. See [`Choice`] for why.
    pub async fn with_adapter(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'static>>,
        choice: Choice,
    ) -> Result<Self, String> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface,
                force_fallback_adapter: choice == Choice::Fallback,
            })
            .await
            .map_err(|e| format!("no suitable GPU adapter: {e}"))?;

        let info = adapter.get_info();
        log::info!(
            "GPU: {} ({:?}, {:?})",
            info.name,
            info.device_type,
            info.backend
        );

        // Downlevel defaults keep us inside what mobile GPUs guarantee, so a
        // desktop build cannot silently start depending on limits an Android
        // or iOS device will refuse at startup.
        let limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("umber-device"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                experimental_features: Default::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| format!("failed to create device: {e}"))?;

        Ok(Self {
            instance,
            adapter,
            device: Arc::new(device),
            queue: Arc::new(queue),
        })
    }

    /// Default instance, honouring `WGPU_BACKEND` so a backend can be forced
    /// when debugging driver-specific behaviour.
    ///
    /// Uses the no-display-handle constructor, which is correct for the
    /// Vulkan/D3D12/Metal backends we actually run on. A Wayland + GLES
    /// fallback would need the display-handle variant instead.
    pub fn create_instance() -> wgpu::Instance {
        wgpu::Instance::new(instance_descriptor())
    }

    /// Pick a surface configuration.
    ///
    /// Deliberately picks a **non**-sRGB format. The engine works in linear
    /// throughout, so an sRGB surface would be the obvious choice — but egui
    /// emits colours that are already gamma-encoded, and the hardware would
    /// encode them a second time, washing the whole UI out. Taking a linear
    /// surface and doing the encode explicitly at the end of `composite.wgsl`
    /// keeps both the canvas and the UI correct.
    pub fn surface_config(
        &self,
        surface: &wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> wgpu::SurfaceConfiguration {
        let caps = surface.get_capabilities(&self.adapter);
        // Indexed nowhere: a surface the adapter cannot present to reports no
        // formats at all, and `caps.formats[0]` would then panic during
        // start-up instead of failing at `configure` with something a bug
        // report can act on.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
        if format.is_srgb() {
            // Survivable, but worth saying out loud: `composite.wgsl` does the
            // gamma encode itself, so an sRGB surface encodes it twice and the
            // whole canvas comes out washed out.
            log::warn!("surface offers only sRGB formats ({format:?}); colours will be light");
        }

        wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The threshold must survive the environment sweep, not be reset by it.
    ///
    /// This measures the descriptor Umber actually builds rather than restating
    /// the rule: writing the field and *then* calling `with_env` compiles, reads
    /// correctly, and leaves `for_resource_creation` at `None` — at which point
    /// nothing refuses an allocation, `try_reserve` catches nothing, and the
    /// artist gets the crash box the whole path exists to replace. Demonstrated
    /// by mutation: build the descriptor as
    /// `new_without_display_handle()` + field + `.with_env()` and this fails.
    ///
    /// It needs no adapter and no device: `InstanceDescriptor` is plain data
    /// until `Instance::new` is handed it.
    #[test]
    fn the_memory_budget_threshold_survives_the_environment() {
        let desc = instance_descriptor();
        assert_eq!(
            desc.memory_budget_thresholds.for_resource_creation,
            Some(MEMORY_BUDGET_PERCENT),
            "with_env resets memory_budget_thresholds; the write has to come after it"
        );
        assert_eq!(
            desc.memory_budget_thresholds.for_device_loss, None,
            "setting this loses the device under pressure, which a refusal exists to avoid"
        );
    }

    /// **A guard on the descriptor is not a guard on the instance**, and this is
    /// the half that decides whether any of it runs.
    ///
    /// [`Gpu::create_instance`] is the only `Instance::new` in the workspace —
    /// `app.rs`, `docshot.rs`, `gputest.rs`, `shell.rs`, `gpu_pipeline.rs` and
    /// the examples all route through it — so reverting that one line to
    /// `wgpu::Instance::new(InstanceDescriptor::new_without_display_handle_from_env())`
    /// leaves every threshold `None` on every real instance, refuses no
    /// allocation early, and `the_memory_budget_threshold_survives_the_
    /// environment` stays green while the whole refusal path is inert. That is
    /// the "a guard on a model is not a guard on the panel" failure this project
    /// records three times over; the remedy it prescribes is to enumerate the
    /// call site rather than the rule.
    ///
    /// The reading is the source, because an `Instance` exposes nothing about
    /// the descriptor it was built from and building one needs a backend. What
    /// it therefore cannot see: whether some *other* module grows a second
    /// `Instance::new`. Demonstrated by mutation.
    #[test]
    fn the_instance_is_built_from_that_descriptor() {
        const SRC: &str = include_str!("gpu.rs");
        // The sentinel is split so this scan does not match its own source, the
        // trap the sibling guard in `canvas.rs` records at length.
        const NEEDLE: &str = concat!("pub fn ", "create_instance()");
        let body: String = SRC
            .lines()
            .skip_while(|l| !l.contains(NEEDLE))
            .skip(1)
            .take_while(|l| !l.contains("    }"))
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("instance_descriptor()"),
            "`create_instance` builds its own descriptor, so the threshold never reaches a real \
             instance: {body}"
        );
    }
}
