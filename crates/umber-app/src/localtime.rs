//! What the clock on the wall says, as an offset from UTC.
//!
//! `umber-core` stores every timestamp in UTC and knows how to spell one out
//! given an offset — see `umber_core::time`. Finding that offset is the part
//! that needs the operating system's time-zone database, so it lives here,
//! where platform code belongs, rather than in the engine.
//!
//! ## Why not a crate
//!
//! `time`'s `local-offset` refuses outright in a multi-threaded process, and
//! Umber is one several times over: wgpu, the preferences writer and the update
//! check all spawn. `chrono` would work, and carries a great deal besides. What
//! is actually needed is one number, and both platforms hand it over directly —
//! `libc` and `windows-sys` are already in the dependency tree underneath winit
//! and wgpu, so this adds no crate to the build at all.
//!
//! ## Why the offset is asked for per instant
//!
//! Not "the offset here" but "the offset here, **then**". Daylight saving means
//! a summer stroke and a winter one in the same document are two different
//! offsets, and a history spanning October would otherwise put an hour's jump
//! in the middle of an afternoon's work. Both platform calls take the moment.
//!
//! A failure is `None` and the caller falls back to UTC, labelled as such. A
//! tooltip is not worth a panic, and a time that says which zone it is in is
//! never wrong — only less convenient.

/// Seconds to add to UTC to get local time at `at`, or `None` if the platform
/// will not say.
pub fn offset_at(at: umber_core::time::Timestamp) -> Option<i32> {
    platform::offset_at(at.unix_millis().div_euclid(1000))
}

#[cfg(unix)]
mod platform {
    /// `localtime_r` fills a `tm` whose `tm_gmtoff` is the offset that applied
    /// at that instant, daylight saving included. It is thread-safe — the
    /// reentrant form exists for exactly that — and it is the same call every
    /// C program on the system makes.
    pub fn offset_at(unix_seconds: i64) -> Option<i32> {
        let time = libc::time_t::try_from(unix_seconds).ok()?;
        // SAFETY: `tm` is POD and fully written by `localtime_r`, which is given
        // a valid pointer to it and to `time`. A null return means the platform
        // could not answer, which is the `None` below rather than a failure.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&time, &mut tm).is_null() {
                return None;
            }
            i32::try_from(tm.tm_gmtoff).ok()
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime};

    /// Between the Windows epoch (1601-01-01) and the Unix one, in the 100 ns
    /// ticks a `FILETIME` counts.
    const EPOCH_TICKS: i64 = 116_444_736_000_000_000;

    /// Windows has no `tm_gmtoff`, so the offset is measured rather than read:
    /// convert the instant to local time — `SystemTimeToTzSpecificLocalTime`
    /// applies whichever daylight rule was in force *then*, which is the whole
    /// reason for passing the moment — and take the difference.
    pub fn offset_at(unix_seconds: i64) -> Option<i32> {
        let ticks = unix_seconds
            .checked_mul(10_000_000)?
            .checked_add(EPOCH_TICKS)?;
        if ticks < 0 {
            return None;
        }
        let utc = system_time(ticks)?;

        // SAFETY: both structures are POD; `local` is fully written on success,
        // and a zero return means the call failed and it is not read.
        let local = unsafe {
            let mut local: SYSTEMTIME = std::mem::zeroed();
            if SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
                return None;
            }
            local
        };

        let local_ticks = file_time(&local)?;
        i32::try_from((local_ticks - ticks) / 10_000_000).ok()
    }

    /// A `FILETIME` back to a `SYSTEMTIME`, via the round trip Windows offers.
    fn system_time(ticks: i64) -> Option<SYSTEMTIME> {
        use windows_sys::Win32::System::Time::FileTimeToSystemTime;
        let ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        // SAFETY: `st` is POD and written by the call; a zero return means it
        // was not, and it is not read in that case.
        unsafe {
            let mut st: SYSTEMTIME = std::mem::zeroed();
            (FileTimeToSystemTime(&ft, &mut st) != 0).then_some(st)
        }
    }

    fn file_time(st: &SYSTEMTIME) -> Option<i64> {
        // SAFETY: as above, in the other direction.
        unsafe {
            let mut ft: FILETIME = std::mem::zeroed();
            if SystemTimeToFileTime(st, &mut ft) == 0 {
                return None;
            }
            Some(((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64)
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    pub fn offset_at(_unix_seconds: i64) -> Option<i32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use umber_core::time::Timestamp;

    /// Whatever zone the machine running this is in, the answer has to be one
    /// a zone can actually be. The half-hour and three-quarter-hour zones are
    /// real — Kathmandu is +05:45 — so the test is a range, not a multiple.
    #[test]
    fn the_offset_is_a_plausible_one() {
        let Some(offset) = offset_at(Timestamp::now()) else {
            return; // A platform that will not say is allowed to not say.
        };
        assert!(
            (-12 * 3600..=14 * 3600).contains(&offset),
            "{offset} seconds is not a time zone"
        );
        assert_eq!(offset % 60, 0, "no zone is offset by seconds");
    }

    /// The offset is asked for per instant so that daylight saving is right,
    /// which means January and July may legitimately differ — but neither may
    /// fail when the other does not, and both must stay plausible.
    #[test]
    fn midwinter_and_midsummer_both_answer() {
        let january = Timestamp::from_unix_millis(1_767_225_600_000); // 2026-01-01
        let july = Timestamp::from_unix_millis(1_782_950_400_000); // 2026-07-01
        match (offset_at(january), offset_at(july)) {
            (Some(w), Some(s)) => {
                assert!((-12 * 3600..=14 * 3600).contains(&w));
                assert!((-12 * 3600..=14 * 3600).contains(&s));
                // Daylight saving is at most an hour or two either way.
                assert!((s - w).abs() <= 2 * 3600, "winter {w}, summer {s}");
            }
            (None, None) => {}
            (w, s) => panic!("answered for one and not the other: {w:?}, {s:?}"),
        }
    }

    /// A timestamp far outside anything a file system or a person will produce
    /// must come back as "cannot say" rather than as a wrong number.
    #[test]
    fn an_impossible_instant_does_not_panic() {
        let _ = offset_at(Timestamp::from_unix_millis(i64::MIN));
        let _ = offset_at(Timestamp::from_unix_millis(i64::MAX));
        let _ = offset_at(Timestamp::from_unix_millis(0));
    }
}
