//! Desktop entry point: Windows, macOS and Linux.

// **No console window on a release build.** A Windows executable declares which
// subsystem it wants at link time, and the default — `console` — makes the
// loader give the process a console *before* `main` runs, which is the black
// window that appeared behind Umber when it was started from Explorer, the
// Start Menu or the installer's own checkbox. There is nothing a program can do
// about that from inside: by the time any code runs the window is already up,
// and hiding it there makes it flash. It has to be declared here.
//
// Only on a release build. A debug build keeps the console subsystem so that
// `cargo run` still has somewhere to put `RUST_LOG` and — more to the point —
// so the panic hook's stderr fallback is visible to whoever is working on the
// thing that panicked. A release build gets the parent's console instead, where
// it was started from one; see [`attach_parent_console`].
//
// macOS and Linux need none of this: neither opens a terminal for a binary
// started from a launcher, and both keep the one it was started from.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    attach_parent_console();

    // `warn` by default keeps wgpu's per-frame chatter out of the way; raise it
    // with e.g. `RUST_LOG=umber_app=debug,wgpu_core=info`.
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,umber_app=info,umber_render=info"),
    )
    .init();

    if let Err(e) = umber_app::run() {
        log::error!("umber exited with an error: {e}");
        std::process::exit(1);
    }
}

/// Write to the console Umber was started from, where there is one.
///
/// The subsystem declared above stops Windows *creating* a console, which is
/// the whole point of it; it also stops a release build inheriting the one it
/// was launched from, so `RUST_LOG=… umber.exe` in a terminal would otherwise
/// print nothing at all. That is a real loss rather than a cosmetic one:
/// `RUST_LOG` and `WGPU_BACKEND` are how a driver bug gets chased, and whoever
/// is chasing one is on a release build by definition.
///
/// `AttachConsole(ATTACH_PARENT_PROCESS)` asks for the parent's console and
/// fails harmlessly where there is not one — started from Explorer, from the
/// Start Menu, or by the installer — so nothing appears in exactly the case
/// this whole change exists to fix. It must run **before** the first write to
/// either stream, because that is when the standard library resolves the
/// handle.
///
/// The known wart is that a GUI-subsystem process does not hold the shell's
/// prompt, so its output arrives after the prompt has come back. That is what
/// every Windows application doing this looks like, and it is a great deal
/// better than a build that cannot be asked what it is doing.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_parent_console() {
    // Safety: no pointer arguments, and a documented failure return for "there
    // is no console to attach to", which is the ordinary case and is ignored.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

/// Every other build already has whatever console it is going to get.
#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_parent_console() {}
