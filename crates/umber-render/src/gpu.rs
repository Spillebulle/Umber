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

/// The descriptor [`Gpu::create_instance`] builds, split out so the one thing
/// that is easy to get wrong about it can be tested.
///
/// **Umber sets no `memory_budget_thresholds`, and 0.1.3 setting
/// `for_resource_creation` to 90 is what made it refuse to start on an ordinary
/// card.** The whole of that argument is retracted below rather than deleted,
/// because the reasoning was good and only its premise about the hardware was
/// false — somebody will reach for it again.
///
/// The argument was: without a threshold a driver may satisfy an allocation it
/// has no room for by paging to system memory — the canvas becomes a slideshow
/// with nothing said — where with one, the allocation comes back as an ordinary
/// `Error::OutOfMemory` on the call that asked, which [`super::canvas`]'s
/// `try_reserve` catches in an error scope and turns into a sentence. That is
/// still true of what the threshold *does*.
///
/// **What it rests on is that wgpu can tell which heap an allocation will land
/// on, and on Vulkan it cannot.**
/// `wgpu-hal`'s `error_if_would_oom_on_resource_allocation` (29.0.4,
/// `src/vulkan/device.rs`) collects **every** heap that has any memory type with
/// the relevant flag — `DEVICE_LOCAL` for a texture, `HOST_VISIBLE` for a
/// buffer — and refuses if *any* of them is over the threshold, because
/// `gpu-alloc` gives it no way to ask where the resource is going. So a heap the
/// texture will never occupy can refuse it, and the check's own comment is what
/// says how much that was thought to matter: "there is usually only one heap on
/// integrated GPUs and two on dedicated GPUs".
///
/// **What is measured and what is inferred**, because the difference decides
/// what a re-enable has to establish first. Measured: 0.1.3 refuses to start on
/// an RTX 3070 under Windows on Vulkan, reporting `OutOfMemory` from
/// `crash::device_error` with no document open and about 1 GB of 8 in use — and
/// the threshold is the only thing in Umber that can produce an `OutOfMemory`
/// the driver did not itself give, and shipped for the first time in that
/// release. Measured on the development machine: two heaps, the device-local one
/// covering the whole 10 GB and host-visible, which is a Resizable BAR layout
/// and exactly the case wgpu's comment assumes. Inferred, and **not yet read off
/// the affected machine**: that it publishes NVIDIA's non-BAR layout, where the
/// ~246 MiB aperture is a *third* heap flagged `DEVICE_LOCAL`, so every texture
/// is measured against 90% of 246 MiB instead of against the card. A heap
/// reporting a budget of zero would produce the same symptom by the same route.
/// The remedy does not turn on which it is; a re-enable does.
///
/// **The refusal path survives this and that is why turning it off is
/// affordable.** A driver that genuinely cannot allocate still answers
/// `VK_ERROR_OUT_OF_DEVICE_MEMORY`, wgpu still maps it through
/// `handle_hal_error_with_nonfatal_oom`, and `try_reserve`'s scope still catches
/// it and still produces `umber-app::vram`'s sentence. What is given up is the
/// *early* refusal — the case where the driver would rather page than fail — and
/// that costs a slow canvas where the threshold cost a dead application. Metal
/// and GL never supported the threshold at all, so this is the behaviour macOS
/// has always had rather than a new one.
///
/// **Two things to fix before re-enabling it**, in this order. The heap layout
/// has to be read before the threshold is chosen — which means `Adapter::as_hal`
/// and a direct `ash` dependency, the cost `Vram`'s docs already declined once —
/// and `for_resource_creation` has to be written **after**
/// `new_without_display_handle_from_env()`, because
/// `InstanceDescriptor::with_env` silently rebuilds the field with
/// `MemoryBudgetThresholds::default()` and that call *is*
/// `new_without_display_handle()` followed by `with_env()`. Writing it before is
/// a threshold that compiles, reads back correctly at the call site, and is
/// inert.
///
/// **`for_device_loss` was never set and must never be.** It makes the backend
/// deliberately *lose* the device on the next poll under memory pressure, which
/// is the unrecoverable outcome the refusal existed to avoid — and it would have
/// turned the bug above into a lost device rather than a catchable error.
fn instance_descriptor() -> wgpu::InstanceDescriptor {
    wgpu::InstanceDescriptor::new_without_display_handle_from_env()
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

    /// No allocation is refused against a budget the driver reported.
    ///
    /// **This one pins a decision rather than measuring a mechanism, and saying
    /// which it is matters**, because the default for both fields is already
    /// `None` — so it cannot fail for the reason a guard usually does, and
    /// nothing here demonstrates by mutation. What it does is fail the build for
    /// anybody who sets `for_resource_creation` again, and send them to
    /// [`instance_descriptor`]'s docs for the two things that have to be true
    /// first. 0.1.3 shipped it at 90 and would not start on an RTX 3070 with
    /// 7 GB free; the reasoning that put it there was sound against a heap
    /// layout that half the NVIDIA machines in the world do not have.
    ///
    /// It needs no adapter and no device: `InstanceDescriptor` is plain data
    /// until `Instance::new` is handed it.
    #[test]
    fn no_allocation_is_refused_against_a_reported_budget() {
        let desc = instance_descriptor();
        assert_eq!(
            desc.memory_budget_thresholds.for_resource_creation, None,
            "wgpu's Vulkan check measures every heap carrying the flag, including NVIDIA's \
             246 MiB BAR aperture — see `instance_descriptor` before setting this again"
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
    /// the examples all route through it — so what this holds is that there is
    /// one statement of the descriptor and every real instance is built from it.
    ///
    /// **It was written to protect a threshold that is now deliberately unset,
    /// and it is kept rather than retired for the same reason
    /// [`instance_descriptor`] is kept as a function.** That is where the
    /// argument about `memory_budget_thresholds` lives and where a second
    /// `Instance::new` would have to be reconciled with it; a call site built
    /// from a descriptor of its own is one that would not be. Its sibling
    /// records the failure this shape exists for: a guard on the descriptor is
    /// not a guard on the instance.
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
