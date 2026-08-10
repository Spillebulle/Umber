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
/// The Windows installer's two bitmaps, drawn from `theme::Palette`.
///
/// Test-only, like `logo`'s icon generator: the bytes are committed under
/// `packaging/windows/` and nothing here reaches the shipped binary.
#[cfg(test)]
mod installart;
mod keylayout;
mod layerdrag;
/// Decoding a document off the drawing thread.
mod loading;
mod localtime;
mod logo;
/// The eyedropper's magnifier: what it holds and where it goes.
///
/// Public for [`syspick`]'s reason and no other — `examples/measure-screenpick.
/// rs` times the neighbourhood read at [`loupe::CELLS`] wide, and an example
/// sees only what the crate exposes. Stating that figure twice is exactly the
/// drift these measurements exist to prevent. Nothing else here is opened up:
/// the decision reaches the interface through `app.rs` and `ui.rs` alone.
pub mod loupe;
mod palettelib;
mod panels;
mod prefs;
mod recoverdlg;
mod session;
mod settings;
mod shell;
mod shortcuts;
mod splash;
mod stamplib;
mod swapchain;
mod swatchdrag;
mod sysclip;
mod syscursor;
/// Reading a pixel of the desktop, for the eyedropper's other half.
///
/// Public because `examples/measure-screenpick.rs` is what settles the numbers
/// its docs quote, and an example sees only what the crate exposes — the same
/// reason [`docshot`] and [`update`] are. Nothing else here is opened up: the
/// decision `syspick::aim` makes reaches the interface through `app.rs` alone.
pub mod syspick;
mod tabs;
mod taskbar;
mod textpanel;
mod theme;
mod themelib;
/// Umber as a command-line thumbnailer, for the freedesktop contract.
mod thumbnail;
mod thumbs;
mod tweaks;
mod ui;
/// Checking for, fetching and installing a new release.
///
/// Public because `examples/make-setup.rs` builds the setup executable with
/// `update::payload::append` — the same function the running binary reads a
/// payload back with, so the writer and the reader cannot drift.
pub mod update;
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
    // And its own installer, twice over: `--install-update` is the helper an
    // update spawns, because a running executable cannot be replaced and the
    // process that puts the package in place cannot be Umber; `--install` is
    // `umber-setup.exe`, which is this same binary with the package on its own
    // end. See `update::installer` and `update::payload`. Read before the crash
    // reporter only because the two are mutually exclusive and one of them has
    // to be first; neither parser recognises the other's flag.
    // Setup arrives with **no arguments at all**, because it is double-clicked:
    // `--install` only ever comes from something spawning this binary
    // deliberately, and nothing does. So the package on the end of the file is
    // what tells setup from Umber, and asking the command line alone left the
    // installer unreachable. Sixteen bytes off our own executable, once, before
    // any window exists; `umber.exe` carries none and pays a seek to find out.
    // Which signal wins is `installer::job`'s, not decided here.
    let carries_payload =
        std::env::current_exe().is_ok_and(|exe| update::payload::carried_by(&exe));
    if let Some(job) = update::installer::job(std::env::args(), carries_payload) {
        return update::installwin::show(job);
    }

    // A thumbnail request, which is the third program this executable is. It
    // writes one PNG and returns without ever building an event loop, so it
    // has to be read before anything that would. See `thumbnail`.
    if let Some(request) = thumbnail::job(std::env::args()) {
        return match thumbnail::run(&request) {
            Ok(()) => Ok(()),
            // Non-zero and a line on stderr, which is what the freedesktop
            // contract asks for: it stops a desktop caching the failure as
            // though it were a picture.
            Err(e) => Err(e.into()),
        };
    }

    // The command line also carries the document a file manager was asked to
    // open, which is what makes a file association more than a menu entry.
    let opening = match crash::parse_args(std::env::args()) {
        crash::Launch::Report(path) => return crash::show_report(&path),
        crash::Launch::Open(path) => Some(path),
        crash::Launch::Normal => None,
    };

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

    let mut app = UmberApp::new(event_loop.create_proxy(), opening);
    event_loop.run_app(&mut app)?;
    // The one place a shutdown is known to have been orderly, and therefore the
    // one place this is said. `run_app` returns only when the loop was told to
    // exit — by the quit prompt, once every unsaved document was accounted for,
    // or by an update handing over. A panic unwinds straight past this, a hard
    // kill never reaches it, and both are exactly the cases the next start has
    // to be able to tell apart. See `autosave::SessionMark`.
    //
    // After the `?` deliberately: a loop that ended by *failing* is not a
    // shutdown, and leaving the marker behind is the honest reading of it.
    app.ended_cleanly();
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

    let mut app = UmberApp::new(event_loop.create_proxy(), None);
    let _ = event_loop.run_app(&mut app);
    app.ended_cleanly();
}
