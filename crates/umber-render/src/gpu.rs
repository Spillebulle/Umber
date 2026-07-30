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

impl Gpu {
    /// Create a device suitable for the given surface.
    pub async fn new(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'static>>,
    ) -> Result<Self, String> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface,
                force_fallback_adapter: false,
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
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
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
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);

        wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        }
    }
}
