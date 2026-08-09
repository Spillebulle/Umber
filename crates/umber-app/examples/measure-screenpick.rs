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
//! * **What does a *block* cost, and may it answer on its own?** The loupe
//!   needs a neighbourhood, and a neighbourhood read with `GetPixel` is N²
//!   display refreshes — 569 ms for an 11×11 here, which is not a control. One
//!   `BitBlt` of the block is the only candidate, and it measured the same as
//!   one pixel: the wait is the display's rather than the pixels'.
//!
//!   The second half decided the design. Taking `GetPixel` for the colour and
//!   the block for the picture cost **9.0 ms** against **4.6** for one read, so
//!   the second call of a frame waits again and the loupe would have doubled
//!   the cost of a gesture that already existed. So the block's middle texel is
//!   the colour, and the last table here is what says that is safe: the two
//!   routes are driven against each other over the corners, one pixel off each
//!   edge and far off every monitor, **at the real block size** so the centring
//!   is what is under test rather than a size of one where every offset is
//!   zero.
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
        println!();

        // The loupe. Three figures decide whether a magnified neighbourhood is
        // affordable at all, and the first is the one that rules out the naive
        // route before anything is built.
        const BLOCK: u32 = umber_app::loupe::CELLS;
        println!("The loupe's neighbourhood, {BLOCK} x {BLOCK}:");
        println!(
            "  by GetPixel, one per texel:                {:.1} us",
            per * f64::from(BLOCK * BLOCK)
        );
        println!("  (predicted from the figure above, not run: at that rate a frame");
        println!("   of the drag would take most of a second)");

        // Warm up the block route as well: the first `CreateCompatibleBitmap`
        // of a size is not the one that matters.
        for _ in 0..20 {
            let _ = syspick::sample_patch(cx, cy, BLOCK);
        }
        const BLOCKS: u32 = 500;
        let t = Instant::now();
        for _ in 0..BLOCKS {
            std::hint::black_box(syspick::sample_patch(cx, cy, BLOCK));
        }
        let per_block = t.elapsed().as_secs_f64() * 1e6 / f64::from(BLOCKS);
        println!("  by one BitBlt (syspick::sample_patch):     {per_block:.1} us");

        // How much of that is the per-texel `MonitorFromPoint` sweep rather
        // than the blit, which is what says whether "a texel on no monitor is
        // nothing rather than black" costs anything worth having a second
        // opinion about.
        let t = Instant::now();
        for _ in 0..BLOCKS {
            std::hint::black_box(via_bitblt(cx, cy));
        }
        let per_one = t.elapsed().as_secs_f64() * 1e6 / f64::from(BLOCKS);
        println!("  one pixel by the same route, for scale:    {per_one:.1} us");

        // What a frame of the drag actually pays: the colour comes from
        // `GetPixel` because it is the one route that answers "nothing" off
        // every monitor, and the picture from the block. If the wait is the
        // compositor's, the second read of a frame should be nearly free; if
        // it is not, the loupe costs a whole second refresh and that is a
        // figure somebody has to be told.
        let t = Instant::now();
        for _ in 0..BLOCKS {
            std::hint::black_box(syspick::sample(cx, cy));
            std::hint::black_box(syspick::sample_patch(cx, cy, BLOCK));
        }
        let per_pair = t.elapsed().as_secs_f64() * 1e6 / f64::from(BLOCKS);
        println!("  the pair a frame of the drag takes:        {per_pair:.1} us");
        println!();
        println!("If the block costs what one pixel costs, the wait is the display's");
        println!("and not the pixels', which is what makes a magnifier free to read.");
        println!("If the pair costs about one read, the second call of a frame does");
        println!("not wait again; if it costs two, the loupe is a second refresh.");
        println!();

        // **Could the block answer on its own?** That is the question the pair's
        // cost raises: if the middle texel of `sample_patch` said everything
        // `sample` says — including "nothing" for a position on no monitor,
        // which is what `GetPixel`'s CLR_INVALID is used for — a frame of the
        // drag would be one refresh instead of two, and the colour taken and
        // the picture shown would come from one instant instead of two four
        // milliseconds apart.
        //
        // `sample_patch` decides that per texel with `MonitorFromPoint`, which
        // answers the question directly where CLR_INVALID answers it by
        // accident. This is whether the two actually agree, on the corners
        // (static, so a mismatch is real) and off the edges (where the whole
        // distinction lives).
        //
        // At the **real** block size, not at one, because the centring is
        // where an off-by-one would hide: `sample_patch` blits from `(x, y)`
        // minus half the block and calls texel `size / 2` the middle, and a
        // size of one makes both of those zero and tests nothing.
        println!(
            "{:<26} {:>14} {:>14}  agree",
            "at", "GetPixel", "block middle"
        );
        let mut split = 0u32;
        let mid = (BLOCK * BLOCK / 2) as usize;
        for (name, x, y) in &probes {
            let a = syspick::sample(*x, *y);
            let b = syspick::sample_patch(*x, *y, BLOCK).and_then(|t| t[mid]);
            let show = |v: Option<[u8; 3]>| match v {
                Some([r, g, b]) => format!("{r:3} {g:3} {b:3}"),
                None => "      -".to_string(),
            };
            if a != b {
                split += 1;
            }
            println!(
                "{name:<26} {:>14} {:>14}  {}",
                show(a),
                show(b),
                if a == b { "yes" } else { "NO" }
            );
        }
        println!(
            "far off every monitor: GetPixel {:?}, block middle {:?}",
            syspick::sample(far.0, far.1),
            syspick::sample_patch(far.0, far.1, BLOCK).and_then(|t| t[mid])
        );
        // The centring, said a second way: the block's middle must move with
        // the position rather than being fixed at its top-left. One pixel to
        // the right must read what `GetPixel` reads one pixel to the right.
        let (px, py) = (vx + vw / 2 + 1, vy + vh / 2);
        println!(
            "one pixel right of centre: GetPixel {:?}, block middle {:?}",
            syspick::sample(px, py),
            syspick::sample_patch(px, py, BLOCK).and_then(|t| t[mid])
        );
        println!();
        println!("{split} disagreement(s). A live desktop repaints between two reads");
        println!("four milliseconds apart, so a mismatch on a pixel that is changing");
        println!("proves nothing; one on a static corner, or on the presence of a");
        println!("colour at all, is the finding.");
    }
}
