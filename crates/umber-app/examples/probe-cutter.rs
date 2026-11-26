//! Ask a Silhouette Portrait 3 who it is and how it is, and nothing else.
//!
//! ```sh
//! cargo run -p umber-app --example probe-cutter
//! ```
//!
//! Nobody working on Umber has run `cutter::UsbPrint` against a machine, so
//! this is how somebody with one finds out what it actually says. It lists every
//! Silhouette the USB printing driver can see, opens the Portrait 3, and sends
//! the three harmless commands `cutter::probe` is made of: initialise, ask for
//! the firmware string, ask for the status. **It moves nothing**: no media is
//! set up, no marks are searched for and no point is sent. Every byte written
//! and read is printed through `cutter::Logged`, one line per transfer, `>` for
//! written and `<` for read, with the byte count and the bytes as ASCII where
//! they are printable and `\xNN` where they are not. Spaces are kept, so a
//! reply can be compared byte for byte with what `cutter::status_of` and
//! `cutter::marks_found` expect.
//!
//! **Close Silhouette Studio first.** It holds the machine open, and the link
//! asks for it exclusively, so with Studio running this says the cutter is
//! busy and stops.
//!
//! Exits with failure when no Portrait 3 is found, when it is busy, or when it
//! does not answer.

use std::process::ExitCode;

use umber_app::cutter::{
    self, LinkError, Logged, PORTRAIT_3, SendError, Status, Timing, UsbPrint, escaped,
};

fn main() -> ExitCode {
    let paths = UsbPrint::paths();
    if paths.is_empty() {
        println!("No Silhouette is visible to the USB printing driver.");
    } else {
        println!("Silhouette interfaces found:");
        // Without the serial number, so the output can be pasted into a report.
        for p in &paths {
            match cutter::shown_path(p) {
                Some(shown) => println!("  {shown}"),
                None => println!("  a Silhouette (path not shown)"),
            }
        }
    }

    let link = match UsbPrint::open(PORTRAIT_3) {
        Ok(link) => link,
        Err(e) => return fail(e),
    };
    println!("Opened the Portrait 3. Sending the three harmless commands.");

    let mut link = Logged::new(link, |line| println!("  {line}"));
    match cutter::probe(&mut link, &Timing::REAL) {
        Ok((firmware, status)) => {
            println!("Firmware: {firmware}");
            match status {
                Ok(Status::Ready) => println!("Status: ready."),
                Ok(Status::Moving) => println!("Status: moving."),
                Ok(Status::NoMedia) => println!("Status: no mat or sheet loaded."),
                Err(raw) => println!(
                    "Status: an answer Umber does not recognise, \"{}\". Umber treats this as \
                     not ready.",
                    escaped(&raw)
                ),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn fail(e: LinkError) -> ExitCode {
    let sentence = SendError::Link(e).sentence();
    eprintln!("{sentence}");
    ExitCode::FAILURE
}
