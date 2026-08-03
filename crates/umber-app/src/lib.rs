//! Application shell: window, event loop, input translation and UI.
//!
//! This crate is the only place that knows about winit or egui. It is built as
//! a library so the desktop binary, the Android `NativeActivity` entry point
//! and the iOS host can all drive the same code.

mod about;
mod app;
mod autosave;
mod brushdrag;
mod brushlib;
mod canvasdlg;
mod colorpicker;
mod controls;
mod cputext;
mod crash;
mod dock;
/// Redrawing the pictures in `docs/images/` from the interface itself.
///
/// Public because `examples/docs-images.rs` is the thing that runs it, and an
/// example sees only what the crate exposes. It is the *only* thing exposed for
/// that: everything the generator reaches — `splash`, `theme`, `settings`,
/// `panels` — stays private behind [`docshot::generate`].
pub mod docshot;
mod editor;
mod exportdlg;
mod gesture;
/// One device for every test in this crate that wants one. See its own docs:
/// the rule is per test *binary*, and this crate is a second one.
#[cfg(test)]
mod gputest;
mod icons;
mod inputlog;
mod keylayout;
mod layerdrag;
mod localtime;
mod logo;
mod panels;
mod prefs;
mod session;
mod settings;
mod shortcuts;
mod splash;
mod syscursor;
mod tabs;
mod taskbar;
mod theme;
mod thumbs;
mod ui;
mod update;
mod updatedlg;
mod widgets;

pub use app::{UmberApp, Wake};
pub use editor::Editor;

use winit::event_loop::{ControlFlow, EventLoop};

/// Run Umber. Blocks until the window closes.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // This executable is also its own crash reporter: a panic hook writes a
    // report and spawns this binary again with `--crash-report <path>`, which
    // draws the box on a device that has not just died. So the command line is
    // read before anything else is set up, and the reporter path never touches
    // the editor, the autosave or the update check at all. See `crash`.
    if let crash::Launch::Report(path) = crash::parse_args(std::env::args()) {
        return crash::show_report(&path);
    }

    // Only on the ordinary path. Installing it in the reporter would mean a
    // crash inside the crash box spawning another crash box, for ever.
    crash::install_hook();

    // Before anything else: a Windows update leaves the binary it displaced
    // beside the new one, because a running executable cannot be deleted. This
    // is the first moment it can go.
    update::sweep_previous_binary();

    // Before the event loop, because this has to precede the first window: the
    // shell reads the application id when it creates the taskbar button, and
    // setting it afterwards does not move a button already on screen. See
    // `taskbar`.
    taskbar::claim_identity();

    // `with_user_event` rather than a plain loop: the update check answers from
    // a thread, and under `ControlFlow::Wait` there is nothing else to make the
    // loop notice. See `UmberApp::user_event`.
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    // Wait rather than Poll: a paint app should be idle when nothing is
    // happening. Redraws are requested explicitly on input.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = UmberApp::new(event_loop.create_proxy());
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Android entry point, invoked by `NativeActivity` via `android_main`.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "C" fn android_main(android_app: winit::platform::android::activity::AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    let event_loop = EventLoop::<Wake>::with_user_event()
        .with_android_app(android_app)
        .build()
        .expect("failed to build event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = UmberApp::new(event_loop.create_proxy());
    let _ = event_loop.run_app(&mut app);
}
