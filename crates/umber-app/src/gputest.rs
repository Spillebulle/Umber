//! One graphics device for this crate's tests, and one at a time.
//!
//! `umber-render`'s `gpu_pipeline.rs` has carried this rule since a device per
//! test starved a dozen concurrent Vulkan devices into a hang. **The rule is
//! per test binary, and this crate is a second binary that did not have it.**
//! It went unnoticed while `autosave`'s frame-loop test was the only thing here
//! that wanted a device; `thumbs`' arrived beside it, and two tests each
//! building and tearing down their own device — concurrently, because that is
//! what the test harness does — is the same hazard again.
//!
//! It surfaced as `STATUS_ACCESS_VIOLATION` at process exit on the ARM64
//! Windows runner: every test passed, and the binary died on the way out. A
//! crash on teardown is the worst shape of this bug, because the run *says* it
//! passed right up until the exit code, and because it is invisible on the
//! machine the tests were written on — the desktop driver here tears two
//! devices down happily.
//!
//! So: one device, created once, and a lock held for the length of each test
//! that wants it. Two tests, so the serialisation costs almost nothing; the
//! point is that a third can be added without learning this again.
//!
//! Both entry points **skip** rather than fail where there is no adapter, which
//! is the same bargain `gpu_pipeline.rs` makes: these tests have to stay
//! meaningful on a runner with no graphics at all.

use std::sync::{Mutex, MutexGuard, OnceLock};

use umber_render::Gpu;

/// The device, or `None` where this machine has no adapter.
pub fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let instance = Gpu::create_instance();
        pollster::block_on(Gpu::new(instance, None)).ok()
    })
    .as_ref()
}

/// Take the device and the right to be the only test using it.
///
/// The guard has to be held for the whole test — bind it, do not drop it on the
/// line it is taken. Poisoning is recovered from so that one failing test
/// reports its own assertion rather than turning every later one into a mutex
/// error.
pub fn lock() -> Option<(&'static Gpu, MutexGuard<'static, ()>)> {
    static SERIAL: Mutex<()> = Mutex::new(());
    let gpu = gpu()?;
    Some((gpu, SERIAL.lock().unwrap_or_else(|e| e.into_inner())))
}
