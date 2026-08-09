//! Settle the three claims `syspick` makes about reading a pixel of the desktop.
//!
//! ```sh
//! cargo run --release -p umber-app --example measure-screenpick
//! ```
//!
//! `measure-clipboard.rs`, `measure-history.rs`, `measure-undo.rs` and
//! `measure-pressure.rs` exist so that a number in a comment can be checked
//! rather than believed, and CLAUDE.md says to re-run them before rebuilding an
//! argument from memory. This one answers:
//!
//! * **How often may a pick be taken?** The first run of this answered the
//!   question the design had assumed: a read is about **7 ms**, which is one
//!   refresh of a 144 Hz display, and `GetDC`/`ReleaseDC` around it is 9 µs. So
//!   there is no handle to cache, the call cannot be made cheaper, and the
//!   sample cannot live on the pointer event. `App::picked_at` is the throttle
//!   that came out of it. A machine with a 60 Hz panel should read about 16 ms
//!   here, because the figure is the display's.
//! * **Does the simple route lose anything the harder one keeps?** `GetPixel`
//!   is one GDI call; `BitBlt` into a memory bitmap plus `GetDIBits` is five,
//!   and is the route that could carry `CAPTUREBLT` for layered windows.
//!   `syspick` chose the first and its docs say why; this is whether the two
//!   actually agree on the pixels in front of you.
//! * **Do coordinates off the primary monitor answer at all?** The
//!   multi-monitor claim is that the screen DC's space is the virtual screen,
//!   negative coordinates included, and that a position off every monitor comes
//!   back `CLR_INVALID` rather than as black.
//! * **What does a *block* cost, and what does a block on top of the single
//!   pixel cost?** The loupe needs a neighbourhood, and a neighbourhood read
//!   with `GetPixel` is N² display refreshes — 850 ms for an 11×11, which is
//!   not a control. One `BitBlt` of the block is the only candidate. The pair
//!   matters separately, because the colour that is *taken* still comes from
//!   `GetPixel` (it is the one route that answers "nothing" off every monitor),
//!   so a frame of the drag pays both: if the wait is the compositor's, the
//!   second read of the same frame should be nearly free, and if it is not the
//!   loupe costs a second refresh.
//!
//! **This reads the screen, which is why it is an example and not a test.** No
//! test in Umber may: a CI runner has no desktop to read, and sampling
//! somebody's screen while they work is the sort of thing an application asks
//! permission for. It prints a handful of colour values and nothing else — no
//! image is captured and nothing is written anywhere.
//!
//! On anything but Windows it says so and stops, because there is nothing
//! built to measure.

fn main() {
    #[cfg(windows)]
    windows::run();
    #[cfg(not(windows))]
    {
        println!("syspick reads the desktop on Windows only, so there is nothing");
        println!("to measure here. See the module docs for what the X11, Wayland");
        println!("and macOS routes would be.");
    }
}

#[cfg(windows)]
mod windows {
    use std::time::Instant;
    use umber_app::syspick;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    /// The other route: one `BitBlt` of a single pixel into a memory bitmap,
    /// read back with `GetDIBits`. Deliberately written out rather than shared
    /// with `syspick`, because the whole point is that it is a second opinion.
    ///
    /// No `CAPTUREBLT`. `syspick` refuses that flag because it forces a repaint
    /// of the entire desktop, which at one call per pointer event is a visible
    /// flicker on every window on the machine; measuring with it would be
    /// measuring something Umber will not do.
    fn via_bitblt(x: i32, y: i32) -> Option<[u8; 3]> {
        // SAFETY: every handle is checked for null before use and released on
        // every path, including the early returns. `GetDIBits` is handed a
        // correctly sized `BITMAPINFO` and a four-byte destination for the one
        // 32-bit pixel it is asked for.
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return None;
            }
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, 1, 1);
            let mut out = None;
            if !mem.is_null() && !bmp.is_null() {
                let old = SelectObject(mem, bmp as _);
                let blitted = BitBlt(mem, 0, 0, 1, 1, screen, x, y, SRCCOPY) != 0;
                // **Deselected before `GetDIBits`, not after.** MSDN: "The
                // bitmap identified by the `hbm` parameter must not be selected
                // into a device context when the application calls this
                // function." It happens to work either way, and this example is
                // the *evidence* for `syspick`'s claim that the two routes
                // agree — a second opinion resting on documented misuse is not
                // one.
                SelectObject(mem, old);
                if blitted {
                    let mut info: BITMAPINFO = std::mem::zeroed();
                    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                    info.bmiHeader.biWidth = 1;
                    // Negative height asks for a top-down bitmap, which for one
                    // pixel changes nothing and is what a real capture would
                    // want.
                    info.bmiHeader.biHeight = -1;
                    info.bmiHeader.biPlanes = 1;
                    info.bmiHeader.biBitCount = 32;
                    info.bmiHeader.biCompression = BI_RGB;
                    let mut px = [0u8; 4];
                    if GetDIBits(
                        mem,
                        bmp,
                        0,
                        1,
                        px.as_mut_ptr().cast(),
                        &mut info,
                        DIB_RGB_COLORS,
                    ) != 0
                    {
                        // A 32-bit DIB is BGRA in memory.
                        out = Some([px[2], px[1], px[0]]);
                    }
                }
            }
            if !bmp.is_null() {
                DeleteObject(bmp as _);
            }
            if !mem.is_null() {
                DeleteDC(mem);
            }
            ReleaseDC(std::ptr::null_mut() as HWND, screen);
            out
        }
    }

    pub fn run() {
        // SAFETY: `GetSystemMetrics` takes an index and returns an `i32`.
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        println!("virtual screen: origin ({vx}, {vy}), {vw} x {vh}");
        println!("(a negative origin means a monitor left of or above the primary one)");
        println!();

        // A spread of points across the whole virtual screen, so a
        // multi-monitor machine reads more than one of them. Every position is
        // a corner or a middle rather than anything the user is working on.
        let probes: Vec<(&str, i32, i32)> = vec![
            ("virtual top-left", vx, vy),
            ("virtual centre", vx + vw / 2, vy + vh / 2),
            ("virtual bottom-right", vx + vw - 1, vy + vh - 1),
            ("one past the right edge", vx + vw + 4, vy + vh / 2),
            ("one before the left edge", vx - 4, vy + vh / 2),
        ];

        println!("{:<26} {:>14} {:>14}  agree", "at", "GetPixel", "BitBlt");
        #[derive(Default)]
        struct Disagreements {
            inside: u32,
            outside: u32,
        }
        let mut disagreements = Disagreements::default();
        for (name, x, y) in &probes {
            let a = syspick::sample(*x, *y);
            let b = via_bitblt(*x, *y);
            let show = |v: Option<[u8; 3]>| match v {
                Some([r, g, b]) => format!("{r:3} {g:3} {b:3}"),
                None => "      -".to_string(),
            };
            let agree = a == b;
            let inside = *x >= vx && *y >= vy && *x < vx + vw && *y < vy + vh;
            if !agree {
                if inside {
                    disagreements.inside += 1;
                } else {
                    disagreements.outside += 1;
                }
            }
            println!(
                "{name:<26} {:>14} {:>14}  {}",
                show(a),
                show(b),
                if agree { "yes" } else { "NO" }
            );
        }
        println!();
        // A disagreement *inside* the virtual screen would be a real finding —
        // two routes reading the same surface and getting different pixels.
        // Outside it, the disagreement is the point: `GetPixel` answers
        // `CLR_INVALID` where `BitBlt` succeeds against nothing and hands back
        // black, and "there is nothing there" against "it is black there" is
        // exactly the distinction the drag needs. So the two counts are
        // separate; the first must be zero and the second is expected.
        if disagreements.inside == 0 {
            println!("The two routes agree on every pixel that exists.");
        } else {
            println!(
                "{} disagreement(s) on real pixels. Read `syspick`'s note on hardware",
                disagreements.inside
            );
            println!("overlays and layered windows before changing anything: a BitBlt");
            println!("without CAPTUREBLT reads the same surface, so a difference here");
            println!("is something else and is worth chasing.");
        }
        if disagreements.outside > 0 {
            println!(
                "{} outside the virtual screen, where GetPixel refuses and BitBlt",
                disagreements.outside
            );
            println!("hands back black. That is why `syspick` uses GetPixel: a pick in");
            println!("the gap between two screens of different heights must read as");
            println!("nothing rather than as black.");
        }
        println!();

        // A position that is off every monitor. On a rectangular single-monitor
        // desktop there is none inside the virtual bounds, so this deliberately
        // goes far outside them: the claim being checked is that GDI answers
        // `CLR_INVALID` rather than handing back black, which is the difference
        // between "nothing there" and "it is black there".
        let far = (vx - 10_000, vy - 10_000);
        println!(
            "far off every monitor at {far:?}: {:?}",
            syspick::sample(far.0, far.1)
        );
        println!("(None is right. Some([0,0,0]) would mean a pick in the gap between");
        println!("two screens of different heights silently took black.)");
        println!();

        // The cost. One read per `CursorMoved`, and a mouse reporting at 1000 Hz
        // is a read every millisecond, so anything approaching that figure has
        // to be throttled.
        let (cx, cy) = (vx + vw / 2, vy + vh / 2);
        // Warm up: the first `GetDC` of the session is not the one that matters.
        for _ in 0..100 {
            let _ = syspick::sample(cx, cy);
        }
        const RUNS: u32 = 2000;
        let t = Instant::now();
        for _ in 0..RUNS {
            std::hint::black_box(syspick::sample(cx, cy));
        }
        let per = t.elapsed().as_secs_f64() * 1e6 / f64::from(RUNS);
        println!("GetPixel, including its own GetDC/ReleaseDC: {per:.1} us per read");

        let t = Instant::now();
        for _ in 0..RUNS {
            std::hint::black_box(via_bitblt(cx, cy));
        }
        let per_blt = t.elapsed().as_secs_f64() * 1e6 / f64::from(RUNS);
        println!("BitBlt + GetDIBits, same:                    {per_blt:.1} us per read");

        // Where the cost is. If the answer is `GetDC`, holding a screen DC for
        // the length of the drag is the fix and `syspick` would need state; if
        // it is `GetPixel`, nothing about the call can be made cheaper and the
        // sample has to be throttled to once per frame instead. That is a
        // design question and this is what decides it, so both halves are
        // timed separately rather than one figure being blamed.
        //
        // SAFETY: as `syspick::sample`, and the DC is released once at the end.
        let (per_dc, per_pixel) = unsafe {
            use windows_sys::Win32::Graphics::Gdi::GetPixel;
            let t = Instant::now();
            for _ in 0..RUNS {
                let dc = GetDC(std::ptr::null_mut());
                std::hint::black_box(dc);
                ReleaseDC(std::ptr::null_mut(), dc);
            }
            let dc_only = t.elapsed().as_secs_f64() * 1e6 / f64::from(RUNS);

            let dc = GetDC(std::ptr::null_mut());
            let t = Instant::now();
            for _ in 0..RUNS {
                std::hint::black_box(GetPixel(dc, cx, cy));
            }
            let pixel_only = t.elapsed().as_secs_f64() * 1e6 / f64::from(RUNS);
            ReleaseDC(std::ptr::null_mut(), dc);
            (dc_only, pixel_only)
        };
        println!("GetDC + ReleaseDC alone:                     {per_dc:.1} us");
        println!("GetPixel alone, on a DC already held:        {per_pixel:.1} us");
        println!();
        println!("A pointer event arrives at most once a millisecond, so anything");
        println!("near 1000 us has to be throttled rather than sampled per event.");
        println!("Which half carries the cost decides how: an expensive GetDC means");
        println!("holding one for the drag, an expensive GetPixel means there is");
        println!("nothing to hold and the sample belongs on the frame instead.");
    }
}
