//! Wall-clock stamps, and the arithmetic needed to show one.
//!
//! The undo history records *when* each edit was made, so the History module
//! can say how long passed between one mark and the next. That has to be
//! wall-clock time rather than [`std::time::Instant`]: an `Instant` is
//! meaningful only within one run of the process, and these are written into a
//! document and read back tomorrow.
//!
//! `SystemTime` is neither a GPU type nor a windowing one, so this sits inside
//! `umber-core` without troubling the crate boundary.
//!
//! # Why there is no date crate
//!
//! Turning a Unix timestamp into a calendar date is the twenty lines of
//! [`Civil::from_unix_millis`] below — Howard Hinnant's `civil_from_days`,
//! which is exact for every year including the century rules that make 1900 and
//! 2100 ordinary and 2000 a leap year. It is pinned by tests against those
//! exact dates, because an off-by-one across a leap day is precisely the bug
//! this kind of code has.
//!
//! A dependency would be a supply-chain decision taken for a tooltip, and the
//! one thing it would buy — **local** time — does not live here anyway. A zone
//! offset is a platform question, so `umber-app/src/localtime.rs` asks it and
//! hands the answer to [`Timestamp::describe_at`]; everything stored stays UTC,
//! and only the reading moves. Both forms name their zone, because an
//! unlabelled time that is two hours out looks exactly like a correct one.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A moment on the wall clock, as milliseconds since the Unix epoch.
///
/// Signed, so a clock set before 1970 is a number rather than a panic.
/// Milliseconds rather than seconds because the gaps this measures are often
/// under a second — a quick sketching hand puts three strokes in one — and a
/// resolution of seconds would report most of them as zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The clock now.
    ///
    /// Never panics. `duration_since` fails when the clock is set before the
    /// epoch, and it hands back the difference either way, so the error branch
    /// is simply the negative one.
    pub fn now() -> Self {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => Self(clamp_millis(d)),
            Err(e) => Self(-clamp_millis(e.duration())),
        }
    }

    pub const fn from_unix_millis(millis: i64) -> Self {
        Self(millis)
    }

    pub const fn unix_millis(self) -> i64 {
        self.0
    }

    /// How long from `earlier` to here, or `None` if that is not a duration.
    ///
    /// A clock is not monotonic: an NTP correction, a daylight-saving change on
    /// a machine that keeps local time in hardware, or a user typing a new date
    /// can all make an edit appear to precede the one before it. There is no
    /// honest length to report for that, and inventing one — clamping to zero,
    /// or taking the absolute value — would put a plausible number where the
    /// truth is that we do not know. The caller shows nothing instead.
    pub fn since(self, earlier: Self) -> Option<Duration> {
        let millis = self.0.checked_sub(earlier.0)?;
        (millis >= 0).then(|| Duration::from_millis(millis as u64))
    }

    pub fn civil(self) -> Civil {
        Civil::from_unix_millis(self.0)
    }

    /// The whole moment in UTC, spelled out: `1 August 2026, 14:32:07 UTC`.
    ///
    /// Day before month, so the order cannot be misread the way `08/01/2026`
    /// can. This allocates, which is why it is called only when a tooltip is
    /// actually being shown and never for a row simply being drawn.
    ///
    /// Kept separate from [`Timestamp::describe_at`] rather than delegating to
    /// it with a zero offset: this is what a reader sees when the platform
    /// could not say what zone they are in, and `UTC` states that plainly where
    /// `(UTC+00:00)` would imply somebody had established they are in it.
    pub fn describe(self) -> String {
        let c = self.civil();
        format!(
            "{} {} {}, {:02}:{:02}:{:02} UTC",
            c.day,
            c.month_name(),
            c.year,
            c.hour,
            c.minute,
            c.second
        )
    }

    /// The same, shifted into a zone `offset` seconds from UTC and labelled
    /// with it: `1 August 2026, 16:32:07 (UTC+02:00)`.
    ///
    /// The offset is the caller's to supply because finding it is a platform
    /// question and this crate has no business asking one — see
    /// `umber-app/src/localtime.rs`, which does. Everything stored stays UTC;
    /// only the reading moves.
    ///
    /// The zone is named even when it is UTC. An unlabelled time that is two
    /// hours out looks exactly like a correct one, and this is the one place a
    /// reader might be comparing against a clock on the wall.
    pub fn describe_at(self, offset: i32) -> String {
        // Shift the instant and read it as though it were UTC: a zone offset is
        // exactly that translation. Saturating rather than wrapping, so a
        // nonsense offset cannot turn a date in 2026 into one in 1907.
        let shifted = Self(self.0.saturating_add(offset as i64 * 1_000));
        let c = shifted.civil();
        let sign = if offset < 0 { '-' } else { '+' };
        let away = offset.unsigned_abs();
        format!(
            "{} {} {}, {:02}:{:02}:{:02} (UTC{sign}{:02}:{:02})",
            c.day,
            c.month_name(),
            c.year,
            c.hour,
            c.minute,
            c.second,
            away / 3600,
            (away % 3600) / 60,
        )
    }
}

/// Milliseconds of `d`, held inside `i64`. A `Duration` counts to year 5.8e11;
/// saturating is the only sane answer and cannot arise from a working clock.
fn clamp_millis(d: Duration) -> i64 {
    d.as_millis().min(i64::MAX as u128) as i64
}

/// A date and time on the proleptic Gregorian calendar, in UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    /// 1–12.
    pub month: u32,
    /// 1–31.
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

impl Civil {
    /// Howard Hinnant's `civil_from_days`, plus the time of day.
    ///
    /// The algorithm shifts the year to start in March, which puts the leap day
    /// at the *end* of a year and so removes every special case from the
    /// month-length arithmetic; `146097` is the days in a 400-year Gregorian
    /// era, which is what makes the 100/400 century rules fall out rather than
    /// being tested for. It is exact for negative days too, which is why the
    /// division is Euclidean throughout — truncating division rounds towards
    /// zero and would put every pre-1970 date one day out.
    pub fn from_unix_millis(millis: i64) -> Self {
        let seconds = millis.div_euclid(1000);
        let days = seconds.div_euclid(86_400);
        let sod = seconds.rem_euclid(86_400);

        // Shift the epoch to 0000-03-01, which is 719_468 days before
        // 1970-01-01 and the start of an era.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097); // day of era, 0..=146096
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // 0..=399
        let year = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // 0..=365, from 1 March
        let mp = (5 * doy + 2) / 153; // 0..=11, March is 0
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };

        Self {
            // January and February belong to the following calendar year.
            year: (year + i64::from(month <= 2)) as i32,
            month: month as u32,
            day: day as u32,
            hour: (sod / 3600) as u32,
            minute: (sod / 60 % 60) as u32,
            second: (sod % 60) as u32,
        }
    }

    pub fn month_name(self) -> &'static str {
        MONTHS
            .get(self.month as usize - 1)
            .copied()
            .unwrap_or("January")
    }
}

/// A short piece of text held on the stack.
///
/// The History list writes one of these for every visible row, so it must not
/// reach the heap: a panel showing forty rows would otherwise make forty small
/// allocations per frame purely to say "3s". Twenty-four bytes covers every
/// value [`brief`] can produce, including a gap of the largest duration a
/// broken clock could hand it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Brief {
    buf: [u8; 24],
    len: u8,
}

impl Brief {
    fn new() -> Self {
        Self {
            buf: [0; 24],
            len: 0,
        }
    }

    fn push(&mut self, byte: u8) {
        if let Some(slot) = self.buf.get_mut(self.len as usize) {
            *slot = byte;
            self.len += 1;
        }
    }

    fn push_str(&mut self, s: &str) {
        for byte in s.bytes() {
            self.push(byte);
        }
    }

    fn push_u64(&mut self, mut n: u64) {
        // Longest u64 is twenty digits, which fits with room for a suffix.
        let mut digits = [0u8; 20];
        let mut used = 0;
        loop {
            digits[used] = b'0' + (n % 10) as u8;
            used += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        for i in (0..used).rev() {
            self.push(digits[i]);
        }
    }

    pub fn as_str(&self) -> &str {
        // Everything written above is ASCII, so this cannot fail; `unwrap_or`
        // rather than `expect` so a future writer of non-ASCII degrades to a
        // blank column instead of taking the application down.
        std::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("")
    }
}

impl fmt::Display for Brief {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Brief {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// `gap` in the shortest form that is still true: `<1s`, `45s`, `3m`, `2h`,
/// `9d`.
///
/// One unit rather than two. The History module's time column is the one that
/// has to give when the panel is dragged narrow, and `1m 30s` is three times
/// the width of `1m` to say something the tooltip says exactly anyway. Rounding
/// is towards zero for the same reason a stopwatch's is: the number is how much
/// of the unit has *elapsed*.
///
/// Days are the last unit. A gap longer than a year is a document picked up
/// again after one, where `412d` is no less readable than `1y` and does not
/// have to pretend a year is 365 days.
pub fn brief(gap: Duration) -> Brief {
    let mut out = Brief::new();
    let secs = gap.as_secs();
    // Below a second is common — a fast hand puts three strokes in one — and
    // `0s` would read as "no time passed" rather than "less than the column
    // can say".
    if secs == 0 {
        out.push_str("<1s");
        return out;
    }
    let (value, unit) = match secs {
        0..60 => (secs, "s"),
        60..3_600 => (secs / 60, "m"),
        3_600..86_400 => (secs / 3_600, "h"),
        _ => (secs / 86_400, "d"),
    };
    out.push_u64(value);
    out.push_str(unit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn civil(secs: i64) -> Civil {
        Civil::from_unix_millis(secs * 1000)
    }

    /// The whole reason this is hand-written rather than imported: the century
    /// rules. 2000 is a leap year because it divides by 400; 2100 is not,
    /// because it divides by 100 and not by 400 — and the second of those is
    /// the one a naive `% 4` gets wrong, silently, seventy-four years from now.
    #[test]
    fn leap_years_land_on_the_right_day() {
        // 2000-02-29 exists.
        assert_eq!(
            civil(951_782_400),
            Civil {
                year: 2000,
                month: 2,
                day: 29,
                hour: 0,
                minute: 0,
                second: 0
            }
        );
        // 2024-02-29 exists.
        let c = civil(1_709_164_800);
        assert_eq!((c.year, c.month, c.day), (2024, 2, 29));
        // 2100-02-28 is followed by 1 March, not by a 29th.
        assert_eq!(
            (civil(4_107_456_000).month, civil(4_107_456_000).day),
            (2, 28)
        );
        let next = civil(4_107_456_000 + 86_400);
        assert_eq!((next.year, next.month, next.day), (2100, 3, 1));
    }

    #[test]
    fn the_epoch_and_the_day_before_it() {
        assert_eq!(
            civil(0),
            Civil {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0
            }
        );
        // A second before it. Truncating division would make this 1970-01-01,
        // which is why the arithmetic is Euclidean.
        assert_eq!(
            civil(-1),
            Civil {
                year: 1969,
                month: 12,
                day: 31,
                hour: 23,
                minute: 59,
                second: 59
            }
        );
    }

    /// Every day of forty years, checked against an independent count of the
    /// days in each month. A table-free algorithm and a table disagreeing is
    /// how an off-by-one is found.
    #[test]
    fn every_day_of_forty_years_agrees_with_a_calendar() {
        fn leap(y: i32) -> bool {
            (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
        }
        let lengths = |y: i32| {
            [
                31,
                if leap(y) { 29 } else { 28 },
                31,
                30,
                31,
                30,
                31,
                31,
                30,
                31,
                30,
                31,
            ]
        };

        // 1990-01-01, far enough back to cross 2000 on the way through.
        let mut day = 7305i64;
        for year in 1990..2030 {
            for (m, len) in lengths(year).into_iter().enumerate() {
                for d in 1..=len {
                    let c = civil(day * 86_400);
                    assert_eq!(
                        (c.year, c.month, c.day),
                        (year, m as u32 + 1, d),
                        "day {day}"
                    );
                    day += 1;
                }
            }
        }
    }

    #[test]
    fn a_time_of_day_survives_the_arithmetic() {
        // 2026-08-01 14:32:07 UTC.
        let t = Timestamp::from_unix_millis((1_785_542_400 + 14 * 3600 + 32 * 60 + 7) * 1000);
        assert_eq!(t.describe(), "1 August 2026, 14:32:07 UTC");
    }

    /// A zone offset is a translation of the instant, so the awkward cases are
    /// the ones that carry it over a boundary — and the reader has to be able
    /// to tell which zone they are looking at.
    #[test]
    fn a_moment_reads_in_the_zone_it_is_asked_for() {
        // 2026-08-01 14:32:07 UTC.
        let t = Timestamp::from_unix_millis((1_785_542_400 + 14 * 3600 + 32 * 60 + 7) * 1000);
        assert_eq!(t.describe_at(0), "1 August 2026, 14:32:07 (UTC+00:00)");
        // Norway in summer.
        assert_eq!(
            t.describe_at(2 * 3600),
            "1 August 2026, 16:32:07 (UTC+02:00)"
        );
        // Los Angeles: back over midnight into the previous day.
        assert_eq!(
            t.describe_at(-7 * 3600),
            "1 August 2026, 07:32:07 (UTC-07:00)"
        );
        // Kathmandu, to prove the minutes are not assumed to be zero.
        assert_eq!(
            t.describe_at(5 * 3600 + 45 * 60),
            "1 August 2026, 20:17:07 (UTC+05:45)"
        );

        // Just before midnight UTC, pushed into the next day and the next
        // month — the case an offset applied to the clock rather than to the
        // instant would get wrong.
        let midnight = Timestamp::from_unix_millis((1_785_542_400 + 23 * 3600 + 30 * 60) * 1000);
        assert_eq!(
            midnight.describe_at(0),
            "1 August 2026, 23:30:00 (UTC+00:00)"
        );
        assert_eq!(
            midnight.describe_at(3600),
            "2 August 2026, 00:30:00 (UTC+01:00)"
        );

        // A nonsense offset saturates rather than wrapping the year.
        let _ = t.describe_at(i32::MIN);
        let _ = t.describe_at(i32::MAX);
    }

    #[test]
    fn a_stamp_survives_its_own_round_trip() {
        let t = Timestamp::from_unix_millis(1_785_542_400_123);
        assert_eq!(Timestamp::from_unix_millis(t.unix_millis()), t);
    }

    /// A clock that has been put back makes an edit appear to precede the one
    /// before it. That is not a duration, and it must not be reported as one —
    /// nor may it panic, which a plain subtraction into `Duration` would.
    #[test]
    fn a_clock_that_goes_backwards_yields_no_duration() {
        let later = Timestamp::from_unix_millis(1_000);
        let earlier = Timestamp::from_unix_millis(5_000);
        assert_eq!(later.since(earlier), None);
        assert_eq!(earlier.since(later), Some(Duration::from_secs(4)));
        // And the extremes, where the subtraction itself would overflow.
        let big = Timestamp::from_unix_millis(i64::MAX);
        let small = Timestamp::from_unix_millis(i64::MIN);
        assert_eq!(big.since(small), None);
        assert_eq!(small.since(big), None);
    }

    #[test]
    fn a_gap_reads_in_one_unit() {
        let s = |secs| brief(Duration::from_secs(secs)).as_str().to_owned();
        assert_eq!(brief(Duration::from_millis(400)).as_str(), "<1s");
        assert_eq!(s(1), "1s");
        assert_eq!(s(59), "59s");
        assert_eq!(s(60), "1m");
        assert_eq!(s(119), "1m");
        assert_eq!(s(3_599), "59m");
        assert_eq!(s(3_600), "1h");
        assert_eq!(s(86_399), "23h");
        assert_eq!(s(86_400), "1d");
        assert_eq!(s(412 * 86_400), "412d");
    }

    /// The buffer must hold whatever a broken clock can produce rather than
    /// truncating into a number that means something else.
    #[test]
    fn the_longest_gap_still_fits() {
        let b = brief(Duration::from_secs(u64::MAX));
        assert!(b.as_str().ends_with('d'), "{b}");
        assert_eq!(b.as_str(), format!("{}d", u64::MAX / 86_400));
    }

    /// `now` reads a clock nobody controls, so the only thing worth asserting
    /// is that it does not panic and that it is ordered against itself.
    #[test]
    fn now_does_not_panic() {
        let a = Timestamp::now();
        let b = Timestamp::now();
        assert!(
            b.since(a).is_some(),
            "the clock ran backwards during a test"
        );
    }
}
