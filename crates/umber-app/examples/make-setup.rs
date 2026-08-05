//! Build `umber-setup.exe` from a binary and a package.
//!
//! ```sh
//! cargo run -p umber-app --example make-setup -- \
//!     target/release/umber.exe umber-0.0.8-x64.msi umber-setup-0.0.8-x64.exe
//! ```
//!
//! The setup executable is Umber's own binary with the MSI concatenated onto it
//! and a footer saying how long the MSI is — `umber_app::update::payload` has
//! the format and the reasoning. Run with `--install` it lifts the package back
//! out and installs it through Umber's own window.
//!
//! **A Rust example rather than a shell script**, unlike the rest of
//! `packaging/`, and for one reason: it calls the same `payload::append` the
//! running binary reads with, so the writer and the reader cannot drift. Two
//! scripts — one for `pwsh` and one for `sh`, which is what `tools/` keeps —
//! would be two more statements of a byte layout.
//!
//! The *name* matters and is the caller's: `unpack_payload` reads the version
//! out of the file's own stem, so `umber-setup-0.0.8-x64.exe` is what puts
//! "Install Umber 0.0.8" at the top of the window.

use umber_app::update::payload;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [executable, package, out] = args.as_slice() else {
        eprintln!("usage: make-setup <executable> <package.msi> <out.exe>");
        return std::process::ExitCode::FAILURE;
    };

    let exe = match std::fs::read(executable) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("could not read {executable}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let msi = match std::fs::read(package) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("could not read {package}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let setup = payload::append(&exe, &msi);
    // Read back before it is written, so a build that produced something the
    // installer cannot open fails here rather than on somebody's machine. It
    // costs one comparison of a ten-megabyte slice.
    match payload::read(&setup) {
        Some(back) if back == msi.as_slice() => {}
        _ => {
            eprintln!("the package did not read back out of the setup binary");
            return std::process::ExitCode::FAILURE;
        }
    }

    if let Err(e) = std::fs::write(out, &setup) {
        eprintln!("could not write {out}: {e}");
        return std::process::ExitCode::FAILURE;
    }
    println!(
        "{out}: {} bytes ({} of program, {} of package)",
        setup.len(),
        exe.len(),
        msi.len()
    );
    std::process::ExitCode::SUCCESS
}
