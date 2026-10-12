//! A port of the parts of Go's `time.Time` that templates use: layout-based
//! formatting (`Format`) and parsing (`Parse`/`ParseInLocation`), calendar
//! arithmetic, and the template-visible methods.
//!
//! Locations: `no_std` has no time zone database, so only UTC, fixed zones
//! (from parsed offsets) and `Local` exist. `Local` uses the offset from
//! [`crate::Env::now`]; its abbreviation is `UTC` when the offset is zero and
//! empty (printed as `+hhmm`) otherwise.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::func::{FuncError, Sig, Ty};
use crate::funcs::duration::{Duration, as_duration_object, time_quote};
use crate::value::{Kind, Object, Value};

/// Seconds from 0001-01-01 to 1970-01-01.
const UNIX_TO_INTERNAL: i64 = 62_135_596_800;

/// A location (time zone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    Utc,
    /// The process-local zone, with its (fixed) UTC offset in seconds.
    Local(i32),
    /// `time.FixedZone(name, offset)`.
    Fixed(String, i32),
}

impl Location {
    fn name_offset(&self) -> (String, i32) {
        match self {
            Location::Utc => ("UTC".into(), 0),
            Location::Local(off) => (if *off == 0 { "UTC".into() } else { String::new() }, *off),
            Location::Fixed(n, o) => (n.clone(), *o),
        }
    }
    /// `Location.String()`.
    pub fn name(&self) -> String {
        match self {
            Location::Utc => "UTC".into(),
            Location::Local(_) => "Local".into(),
            Location::Fixed(n, _) => n.clone(),
        }
    }
}

/// A Go `time.Time` (no monotonic clock reading).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Time {
    /// Seconds since the Unix epoch.
    pub unix: i64,
    /// Nanoseconds within the second, `0..1e9`.
    pub nsec: u32,
    pub loc: Location,
}

pub const LONG_DAY_NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
pub const LONG_MONTH_NAMES: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];

/// Days since 1970-01-01 for a proleptic Gregorian date (month 1..=12).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// (year, month 1..=12, day) for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in(month: i64, year: i64) -> i64 {
    match month {
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

const DAYS_BEFORE: [i64; 13] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365];

/// Go's `norm`.
fn norm(hi: i64, lo: i64, base: i64) -> (i64, i64) {
    let (mut hi, mut lo) = (hi, lo);
    if lo < 0 {
        let n = (-lo - 1) / base + 1;
        hi -= n;
        lo += n * base;
    }
    if lo >= base {
        let n = lo / base;
        hi += n;
        lo -= n * base;
    }
    (hi, lo)
}

impl Time {
    /// The zero `time.Time{}` (0001-01-01 00:00:00 UTC).
    pub fn zero() -> Time {
        Time { unix: -UNIX_TO_INTERNAL, nsec: 0, loc: Location::Utc }
    }

    /// `time.Unix(sec, 0)` in `loc`.
    pub fn from_unix(sec: i64, loc: Location) -> Time {
        Time { unix: sec, nsec: 0, loc }
    }

    /// `time.Date(...)` (normalizing out-of-range fields, like Go).
    pub fn date(year: i64, month: i64, day: i64, hour: i64, min: i64, sec: i64, nsec: i64, loc: Location) -> Time {
        let (year, m) = norm(year, month - 1, 12);
        let month = m + 1;
        let (sec, nsec) = norm(sec, nsec, 1_000_000_000);
        let (min, sec) = norm(min, sec, 60);
        let (hour, min) = norm(hour, min, 60);
        let (day, hour) = norm(day, hour, 24);
        let days = days_from_civil(year, month, 1) + day - 1;
        let unix = days
            .saturating_mul(86400)
            .saturating_add(hour * 3600 + min * 60 + sec);
        let (_, off) = loc.name_offset();
        Time { unix: unix.saturating_sub(off as i64), nsec: nsec as u32, loc }
    }

    fn local_secs(&self) -> i64 {
        let (_, off) = self.loc.name_offset();
        self.unix.saturating_add(off as i64)
    }

    /// (year, month, day) in the time's location.
    pub fn ymd(&self) -> (i64, i64, i64) {
        civil_from_days(self.local_secs().div_euclid(86400))
    }

    /// (hour, minute, second).
    pub fn clock(&self) -> (i64, i64, i64) {
        let s = self.local_secs().rem_euclid(86400);
        (s / 3600, s % 3600 / 60, s % 60)
    }

    /// 0 = Sunday.
    pub fn weekday(&self) -> i64 {
        (self.local_secs().div_euclid(86400) + 4).rem_euclid(7)
    }

    /// Day of the year, 1-based.
    pub fn yday(&self) -> i64 {
        let (y, _, _) = self.ymd();
        self.local_secs().div_euclid(86400) - days_from_civil(y, 1, 1) + 1
    }

    pub fn in_loc(&self, loc: Location) -> Time {
        Time { unix: self.unix, nsec: self.nsec, loc }
    }

    /// `t.Add(d)`.
    pub fn add(&self, d: Duration) -> Time {
        let mut dsec = d.0 / 1_000_000_000;
        let mut nsec = self.nsec as i64 + d.0 % 1_000_000_000;
        if nsec >= 1_000_000_000 {
            dsec += 1;
            nsec -= 1_000_000_000;
        } else if nsec < 0 {
            dsec -= 1;
            nsec += 1_000_000_000;
        }
        Time { unix: self.unix.wrapping_add(dsec), nsec: nsec as u32, loc: self.loc.clone() }
    }

    fn total_nanos(&self) -> i128 {
        self.unix as i128 * 1_000_000_000 + self.nsec as i128
    }

    /// `t.Sub(u)`, saturating like Go.
    pub fn sub(&self, u: &Time) -> Duration {
        let d = self.total_nanos() - u.total_nanos();
        if d > i64::MAX as i128 {
            Duration(i64::MAX)
        } else if d < i64::MIN as i128 {
            Duration(i64::MIN)
        } else {
            Duration(d as i64)
        }
    }

    fn rem(&self, d: Duration) -> i64 {
        let total = (self.unix as i128 + UNIX_TO_INTERNAL as i128) * 1_000_000_000 + self.nsec as i128;
        total.rem_euclid(d.0 as i128) as i64
    }

    pub fn truncate(&self, d: Duration) -> Time {
        if d.0 <= 0 {
            return self.clone();
        }
        self.add(Duration(-self.rem(d)))
    }

    pub fn round(&self, d: Duration) -> Time {
        if d.0 <= 0 {
            return self.clone();
        }
        let r = self.rem(d);
        if (r as u64).wrapping_add(r as u64) < d.0 as u64 {
            self.add(Duration(-r))
        } else {
            self.add(Duration(d.0 - r))
        }
    }

    /// `t.Format(layout)`.
    pub fn format(&self, layout: &str) -> String {
        if layout == "2006-01-02T15:04:05Z07:00" || layout == "2006-01-02T15:04:05.999999999Z07:00" {
            return self.format_rfc3339(layout.len() > 25);
        }
        let (name, offset) = self.loc.name_offset();
        let (year, month, day) = self.ymd();
        let (hour, min, sec) = self.clock();
        let yday = self.yday();
        let mut b: Vec<u8> = Vec::with_capacity(layout.len() + 10);
        let mut layout = layout;
        while !layout.is_empty() {
            let (prefix, std, suffix) = next_std_chunk(layout);
            b.extend_from_slice(prefix.as_bytes());
            if std == 0 {
                break;
            }
            layout = suffix;
            match std & STD_MASK {
                STD_YEAR => append_int(&mut b, year.abs() % 100, 2),
                STD_LONG_YEAR => append_int(&mut b, year, 4),
                STD_MONTH => b.extend_from_slice(&LONG_MONTH_NAMES[(month - 1) as usize].as_bytes()[..3]),
                STD_LONG_MONTH => b.extend_from_slice(LONG_MONTH_NAMES[(month - 1) as usize].as_bytes()),
                STD_NUM_MONTH => append_int(&mut b, month, 0),
                STD_ZERO_MONTH => append_int(&mut b, month, 2),
                STD_WEEK_DAY => b.extend_from_slice(&LONG_DAY_NAMES[self.weekday() as usize].as_bytes()[..3]),
                STD_LONG_WEEK_DAY => b.extend_from_slice(LONG_DAY_NAMES[self.weekday() as usize].as_bytes()),
                STD_DAY => append_int(&mut b, day, 0),
                STD_UNDER_DAY => {
                    if day < 10 {
                        b.push(b' ');
                    }
                    append_int(&mut b, day, 0);
                }
                STD_ZERO_DAY => append_int(&mut b, day, 2),
                STD_UNDER_YEAR_DAY => {
                    if yday < 100 {
                        b.push(b' ');
                        if yday < 10 {
                            b.push(b' ');
                        }
                    }
                    append_int(&mut b, yday, 0);
                }
                STD_ZERO_YEAR_DAY => append_int(&mut b, yday, 3),
                STD_HOUR => append_int(&mut b, hour, 2),
                STD_HOUR12 => append_int(&mut b, if hour % 12 == 0 { 12 } else { hour % 12 }, 0),
                STD_ZERO_HOUR12 => append_int(&mut b, if hour % 12 == 0 { 12 } else { hour % 12 }, 2),
                STD_MINUTE => append_int(&mut b, min, 0),
                STD_ZERO_MINUTE => append_int(&mut b, min, 2),
                STD_SECOND => append_int(&mut b, sec, 0),
                STD_ZERO_SECOND => append_int(&mut b, sec, 2),
                STD_PM => b.extend_from_slice(if hour >= 12 { b"PM" } else { b"AM" }),
                STD_PM_LOWER => b.extend_from_slice(if hour >= 12 { b"pm" } else { b"am" }),
                s @ (STD_ISO8601_TZ | STD_ISO8601_COLON_TZ | STD_ISO8601_SECONDS_TZ | STD_ISO8601_SHORT_TZ
                | STD_ISO8601_COLON_SECONDS_TZ | STD_NUM_TZ | STD_NUM_COLON_TZ | STD_NUM_SECONDS_TZ
                | STD_NUM_SHORT_TZ | STD_NUM_COLON_SECONDS_TZ) => {
                    let iso = matches!(
                        s,
                        STD_ISO8601_TZ
                            | STD_ISO8601_COLON_TZ
                            | STD_ISO8601_SECONDS_TZ
                            | STD_ISO8601_SHORT_TZ
                            | STD_ISO8601_COLON_SECONDS_TZ
                    );
                    if offset == 0 && iso {
                        b.push(b'Z');
                        continue;
                    }
                    let mut zone = offset as i64 / 60;
                    let mut absoffset = offset as i64;
                    if zone < 0 {
                        b.push(b'-');
                        zone = -zone;
                        absoffset = -absoffset;
                    } else {
                        b.push(b'+');
                    }
                    append_int(&mut b, zone / 60, 2);
                    if matches!(s, STD_ISO8601_COLON_TZ | STD_NUM_COLON_TZ | STD_ISO8601_COLON_SECONDS_TZ | STD_NUM_COLON_SECONDS_TZ) {
                        b.push(b':');
                    }
                    if s != STD_NUM_SHORT_TZ && s != STD_ISO8601_SHORT_TZ {
                        append_int(&mut b, zone % 60, 2);
                    }
                    if matches!(s, STD_ISO8601_SECONDS_TZ | STD_NUM_SECONDS_TZ | STD_NUM_COLON_SECONDS_TZ | STD_ISO8601_COLON_SECONDS_TZ) {
                        if s == STD_NUM_COLON_SECONDS_TZ || s == STD_ISO8601_COLON_SECONDS_TZ {
                            b.push(b':');
                        }
                        append_int(&mut b, absoffset % 60, 2);
                    }
                }
                STD_TZ => {
                    if !name.is_empty() {
                        b.extend_from_slice(name.as_bytes());
                        continue;
                    }
                    let mut zone = offset as i64 / 60;
                    if zone < 0 {
                        b.push(b'-');
                        zone = -zone;
                    } else {
                        b.push(b'+');
                    }
                    append_int(&mut b, zone / 60, 2);
                    append_int(&mut b, zone % 60, 2);
                }
                STD_FRAC_SECOND0 | STD_FRAC_SECOND9 => append_nano(&mut b, self.nsec as i64, std),
                _ => {}
            }
        }
        crate::gofmt::strconv::lossy_vec(b)
    }

    fn format_rfc3339(&self, nanos: bool) -> String {
        let (_, offset) = self.loc.name_offset();
        let (year, month, day) = self.ymd();
        let (hour, min, sec) = self.clock();
        let mut b = Vec::new();
        append_int(&mut b, year, 4);
        b.push(b'-');
        append_int(&mut b, month, 2);
        b.push(b'-');
        append_int(&mut b, day, 2);
        b.push(b'T');
        append_int(&mut b, hour, 2);
        b.push(b':');
        append_int(&mut b, min, 2);
        b.push(b':');
        append_int(&mut b, sec, 2);
        if nanos {
            append_nano(&mut b, self.nsec as i64, std_frac_second(STD_FRAC_SECOND9, 9, b'.'));
        }
        if offset == 0 {
            b.push(b'Z');
        } else {
            let mut zone = offset as i64 / 60;
            if zone < 0 {
                b.push(b'-');
                zone = -zone;
            } else {
                b.push(b'+');
            }
            append_int(&mut b, zone / 60, 2);
            b.push(b':');
            append_int(&mut b, zone % 60, 2);
        }
        crate::gofmt::strconv::lossy_vec(b)
    }

    /// `Time.String()` (without a monotonic clock reading).
    pub fn go_string(&self) -> String {
        self.format("2006-01-02 15:04:05.999999999 -0700 MST")
    }

    pub fn is_zero(&self) -> bool {
        self.unix == -UNIX_TO_INTERNAL && self.nsec == 0
    }
}

// ---------------------------------------------------------------------------
// Layout chunks
// ---------------------------------------------------------------------------

const STD_NEED_DATE: i64 = 1 << 8;
const STD_NEED_YDAY: i64 = 1 << 9;
const STD_NEED_CLOCK: i64 = 1 << 10;
const STD_ARG_SHIFT: i64 = 16;
const STD_SEPARATOR_SHIFT: i64 = 28;
const STD_MASK: i64 = (1 << STD_ARG_SHIFT) - 1;

const STD_LONG_MONTH: i64 = 1 + STD_NEED_DATE;
const STD_MONTH: i64 = 2 + STD_NEED_DATE;
const STD_NUM_MONTH: i64 = 3 + STD_NEED_DATE;
const STD_ZERO_MONTH: i64 = 4 + STD_NEED_DATE;
const STD_LONG_WEEK_DAY: i64 = 5 + STD_NEED_DATE;
const STD_WEEK_DAY: i64 = 6 + STD_NEED_DATE;
const STD_DAY: i64 = 7 + STD_NEED_DATE;
const STD_UNDER_DAY: i64 = 8 + STD_NEED_DATE;
const STD_ZERO_DAY: i64 = 9 + STD_NEED_DATE;
const STD_UNDER_YEAR_DAY: i64 = 10 + STD_NEED_YDAY;
const STD_ZERO_YEAR_DAY: i64 = 11 + STD_NEED_YDAY;
const STD_HOUR: i64 = 12 + STD_NEED_CLOCK;
const STD_HOUR12: i64 = 13 + STD_NEED_CLOCK;
const STD_ZERO_HOUR12: i64 = 14 + STD_NEED_CLOCK;
const STD_MINUTE: i64 = 15 + STD_NEED_CLOCK;
const STD_ZERO_MINUTE: i64 = 16 + STD_NEED_CLOCK;
const STD_SECOND: i64 = 17 + STD_NEED_CLOCK;
const STD_ZERO_SECOND: i64 = 18 + STD_NEED_CLOCK;
const STD_LONG_YEAR: i64 = 19 + STD_NEED_DATE;
const STD_YEAR: i64 = 20 + STD_NEED_DATE;
const STD_PM: i64 = 21 + STD_NEED_CLOCK;
const STD_PM_LOWER: i64 = 22 + STD_NEED_CLOCK;
const STD_TZ: i64 = 23;
const STD_ISO8601_TZ: i64 = 24;
const STD_ISO8601_SECONDS_TZ: i64 = 25;
const STD_ISO8601_SHORT_TZ: i64 = 26;
const STD_ISO8601_COLON_TZ: i64 = 27;
const STD_ISO8601_COLON_SECONDS_TZ: i64 = 28;
const STD_NUM_TZ: i64 = 29;
const STD_NUM_SECONDS_TZ: i64 = 30;
const STD_NUM_SHORT_TZ: i64 = 31;
const STD_NUM_COLON_TZ: i64 = 32;
const STD_NUM_COLON_SECONDS_TZ: i64 = 33;
const STD_FRAC_SECOND0: i64 = 34;
const STD_FRAC_SECOND9: i64 = 35;

const STD0X: [i64; 6] = [STD_ZERO_MONTH, STD_ZERO_DAY, STD_ZERO_HOUR12, STD_ZERO_MINUTE, STD_ZERO_SECOND, STD_YEAR];

fn std_frac_second(code: i64, n: i64, c: u8) -> i64 {
    let v = code | ((n & 0xfff) << STD_ARG_SHIFT);
    if c == b'.' { v } else { v | (1 << STD_SEPARATOR_SHIFT) }
}

fn digits_len(std: i64) -> i64 {
    (std >> STD_ARG_SHIFT) & 0xfff
}

fn starts_with_lower(s: &[u8]) -> bool {
    s.first().is_some_and(|c| c.is_ascii_lowercase())
}

fn is_digit_at(s: &[u8], i: usize) -> bool {
    s.get(i).is_some_and(|c| c.is_ascii_digit())
}

/// Go's `nextStdChunk`.
fn next_std_chunk(layout: &str) -> (&str, i64, &str) {
    let l = layout.as_bytes();
    let n = l.len();
    let has = |i: usize, s: &str| n >= i + s.len() && &l[i..i + s.len()] == s.as_bytes();
    for i in 0..n {
        let c = l[i];
        match c {
            b'J' => {
                if has(i, "Jan") {
                    if has(i, "January") {
                        return (&layout[..i], STD_LONG_MONTH, &layout[i + 7..]);
                    }
                    if !starts_with_lower(&l[i + 3..]) {
                        return (&layout[..i], STD_MONTH, &layout[i + 3..]);
                    }
                }
            }
            b'M' => {
                if n >= i + 3 {
                    if has(i, "Mon") {
                        if has(i, "Monday") {
                            return (&layout[..i], STD_LONG_WEEK_DAY, &layout[i + 6..]);
                        }
                        if !starts_with_lower(&l[i + 3..]) {
                            return (&layout[..i], STD_WEEK_DAY, &layout[i + 3..]);
                        }
                    }
                    if has(i, "MST") {
                        return (&layout[..i], STD_TZ, &layout[i + 3..]);
                    }
                }
            }
            b'0' => {
                if n >= i + 2 && (b'1'..=b'6').contains(&l[i + 1]) {
                    return (&layout[..i], STD0X[(l[i + 1] - b'1') as usize], &layout[i + 2..]);
                }
                if n >= i + 3 && l[i + 1] == b'0' && l[i + 2] == b'2' {
                    return (&layout[..i], STD_ZERO_YEAR_DAY, &layout[i + 3..]);
                }
            }
            b'1' => {
                if n >= i + 2 && l[i + 1] == b'5' {
                    return (&layout[..i], STD_HOUR, &layout[i + 2..]);
                }
                return (&layout[..i], STD_NUM_MONTH, &layout[i + 1..]);
            }
            b'2' => {
                if has(i, "2006") {
                    return (&layout[..i], STD_LONG_YEAR, &layout[i + 4..]);
                }
                return (&layout[..i], STD_DAY, &layout[i + 1..]);
            }
            b'_' => {
                if n >= i + 2 && l[i + 1] == b'2' {
                    if n >= i + 5 && &l[i + 1..i + 5] == b"2006" {
                        return (&layout[..i + 1], STD_LONG_YEAR, &layout[i + 5..]);
                    }
                    return (&layout[..i], STD_UNDER_DAY, &layout[i + 2..]);
                }
                if n >= i + 3 && l[i + 1] == b'_' && l[i + 2] == b'2' {
                    return (&layout[..i], STD_UNDER_YEAR_DAY, &layout[i + 3..]);
                }
            }
            b'3' => return (&layout[..i], STD_HOUR12, &layout[i + 1..]),
            b'4' => return (&layout[..i], STD_MINUTE, &layout[i + 1..]),
            b'5' => return (&layout[..i], STD_SECOND, &layout[i + 1..]),
            b'P' => {
                if n >= i + 2 && l[i + 1] == b'M' {
                    return (&layout[..i], STD_PM, &layout[i + 2..]);
                }
            }
            b'p' => {
                if n >= i + 2 && l[i + 1] == b'm' {
                    return (&layout[..i], STD_PM_LOWER, &layout[i + 2..]);
                }
            }
            b'-' => {
                for (pat, code) in [
                    ("-070000", STD_NUM_SECONDS_TZ),
                    ("-07:00:00", STD_NUM_COLON_SECONDS_TZ),
                    ("-0700", STD_NUM_TZ),
                    ("-07:00", STD_NUM_COLON_TZ),
                    ("-07", STD_NUM_SHORT_TZ),
                ] {
                    if has(i, pat) {
                        return (&layout[..i], code, &layout[i + pat.len()..]);
                    }
                }
            }
            b'Z' => {
                for (pat, code) in [
                    ("Z070000", STD_ISO8601_SECONDS_TZ),
                    ("Z07:00:00", STD_ISO8601_COLON_SECONDS_TZ),
                    ("Z0700", STD_ISO8601_TZ),
                    ("Z07:00", STD_ISO8601_COLON_TZ),
                    ("Z07", STD_ISO8601_SHORT_TZ),
                ] {
                    if has(i, pat) {
                        return (&layout[..i], code, &layout[i + pat.len()..]);
                    }
                }
            }
            b'.' | b',' => {
                if i + 1 < n && (l[i + 1] == b'0' || l[i + 1] == b'9') {
                    let ch = l[i + 1];
                    let mut j = i + 1;
                    while j < n && l[j] == ch {
                        j += 1;
                    }
                    if !is_digit_at(l, j) {
                        let code = if l[i + 1] == b'9' { STD_FRAC_SECOND9 } else { STD_FRAC_SECOND0 };
                        let std = std_frac_second(code, (j - (i + 1)) as i64, c);
                        return (&layout[..i], std, &layout[j..]);
                    }
                }
            }
            _ => {}
        }
    }
    (layout, 0, "")
}

/// Go's `appendInt`.
fn append_int(b: &mut Vec<u8>, x: i64, width: usize) {
    let mut u = x.unsigned_abs();
    if x < 0 {
        b.push(b'-');
    }
    let mut digits = Vec::new();
    loop {
        digits.push(b'0' + (u % 10) as u8);
        u /= 10;
        if u == 0 {
            break;
        }
    }
    for _ in digits.len()..width {
        b.push(b'0');
    }
    digits.reverse();
    b.extend_from_slice(&digits);
}

fn append_nano(b: &mut Vec<u8>, nanosec: i64, std: i64) {
    let trim = std & STD_MASK == STD_FRAC_SECOND9;
    let n = digits_len(std) as usize;
    if trim && (n == 0 || nanosec == 0) {
        return;
    }
    let dot = if (std >> STD_SEPARATOR_SHIFT) == 0 { b'.' } else { b',' };
    b.push(dot);
    let start = b.len();
    append_int(b, nanosec, 9);
    if n < 9 {
        b.truncate(start + n);
    }
    if trim {
        while b.last() == Some(&b'0') {
            b.pop();
        }
        if b.last() == Some(&dot) {
            b.pop();
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn parse_error(layout: &str, value: &str, layout_elem: &str, value_elem: &str, message: &str) -> String {
    if message.is_empty() {
        format!(
            "parsing time {} as {}: cannot parse {} as {}",
            time_quote(value),
            time_quote(layout),
            time_quote(value_elem),
            time_quote(layout_elem)
        )
    } else {
        format!("parsing time {}{message}", time_quote(value))
    }
}

fn getnum(s: &str, fixed: bool) -> Result<(i64, &str), ()> {
    let b = s.as_bytes();
    if !is_digit_at(b, 0) {
        return Err(());
    }
    if !is_digit_at(b, 1) {
        if fixed {
            return Err(());
        }
        return Ok(((b[0] - b'0') as i64, &s[1..]));
    }
    Ok((((b[0] - b'0') * 10 + (b[1] - b'0')) as i64, &s[2..]))
}

fn getnum3(s: &str, fixed: bool) -> Result<(i64, &str), ()> {
    let b = s.as_bytes();
    let mut n = 0i64;
    let mut i = 0;
    while i < 3 && is_digit_at(b, i) {
        n = n * 10 + (b[i] - b'0') as i64;
        i += 1;
    }
    if i == 0 || (fixed && i != 3) {
        return Err(());
    }
    Ok((n, &s[i..]))
}

fn skip<'a>(value: &'a str, prefix: &str) -> Result<&'a str, &'a str> {
    let mut v = value;
    let mut p = prefix;
    while !p.is_empty() {
        if p.as_bytes()[0] == b' ' {
            if !v.is_empty() && v.as_bytes()[0] != b' ' {
                return Err(v);
            }
            p = p.trim_start_matches(' ');
            v = v.trim_start_matches(' ');
            continue;
        }
        if v.is_empty() || v.as_bytes()[0] != p.as_bytes()[0] {
            return Err(v);
        }
        // Advance by one byte; both are ASCII-or-equal prefixes.
        let w = p.chars().next().map_or(1, char::len_utf8);
        if v.len() < w || v.as_bytes()[..w] != p.as_bytes()[..w] {
            return Err(v);
        }
        p = &p[w..];
        v = &v[w..];
    }
    Ok(v)
}

fn lookup<'a>(tab: &[&str], val: &'a str, short: bool) -> Result<(i64, &'a str), ()> {
    for (i, v) in tab.iter().enumerate() {
        let v = if short { &v[..3] } else { v };
        if val.len() >= v.len() {
            let a = &val.as_bytes()[..v.len()];
            let ok = a.iter().zip(v.as_bytes()).all(|(&c1, &c2)| {
                if c1 == c2 {
                    return true;
                }
                let (l1, l2) = (c1 | 0x20, c2 | 0x20);
                l1 == l2 && l1.is_ascii_lowercase()
            });
            if ok {
                return Ok((i as i64, &val[v.len()..]));
            }
        }
    }
    Err(())
}

fn leading_int(s: &str) -> Result<(u64, &str), ()> {
    let b = s.as_bytes();
    let mut x: u64 = 0;
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        if x > (1 << 63) / 10 {
            return Err(());
        }
        x = x * 10 + (b[i] - b'0') as u64;
        if x > 1 << 63 {
            return Err(());
        }
        i += 1;
    }
    Ok((x, &s[i..]))
}

fn atoi(s: &str) -> Result<i64, ()> {
    let (neg, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (q, rem) = leading_int(s)?;
    if !rem.is_empty() {
        return Err(());
    }
    let x = q as i64;
    Ok(if neg { x.wrapping_neg() } else { x })
}

fn parse_nanoseconds(value: &str, nbytes: usize) -> Result<(i64, &'static str), ()> {
    let b = value.as_bytes();
    if b[0] != b'.' && b[0] != b',' {
        return Err(());
    }
    let nbytes = nbytes.min(10);
    let ns = atoi(&value[1..nbytes])?;
    if ns < 0 {
        return Ok((0, "fractional second"));
    }
    let mut ns = ns;
    for _ in 0..(10 - nbytes) {
        ns *= 10;
    }
    Ok((ns, ""))
}

fn parse_signed_offset(value: &str) -> usize {
    let b = value.as_bytes();
    if b[0] != b'-' && b[0] != b'+' {
        return 0;
    }
    match leading_int(&value[1..]) {
        Ok((x, rem)) if rem.len() != value.len() - 1 && x <= 23 => value.len() - rem.len(),
        _ => 0,
    }
}

fn parse_time_zone(value: &str) -> Option<usize> {
    let b = value.as_bytes();
    if b.len() < 3 {
        return None;
    }
    if b.len() >= 4 && (&b[..4] == b"ChST" || &b[..4] == b"MeST") {
        return Some(4);
    }
    if &b[..3] == b"GMT" {
        let rest = &value[3..];
        if rest.is_empty() {
            return Some(3);
        }
        return Some(3 + parse_signed_offset(rest));
    }
    if b[0] == b'+' || b[0] == b'-' {
        let l = parse_signed_offset(value);
        return if l > 0 { Some(l) } else { None };
    }
    let mut n_upper = 0;
    while n_upper < 6 && n_upper < b.len() && b[n_upper].is_ascii_uppercase() {
        n_upper += 1;
    }
    match n_upper {
        5 if b[4] == b'T' => Some(5),
        4 if b[3] == b'T' || &b[..4] == b"WITA" => Some(4),
        3 => Some(3),
        _ => None,
    }
}

fn parse_rfc3339(s: &str, local: &Location) -> Option<Time> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let mut ok = true;
    let mut parse_uint = |s: &[u8], min: i64, max: i64| -> i64 {
        let mut x = 0i64;
        for &c in s {
            if !c.is_ascii_digit() {
                ok = false;
                return min;
            }
            x = x * 10 + (c - b'0') as i64;
        }
        if x < min || max < x {
            ok = false;
            return min;
        }
        x
    };
    let year = parse_uint(&b[0..4], 0, 9999);
    let month = parse_uint(&b[5..7], 1, 12);
    let day = parse_uint(&b[8..10], 1, days_in(month, year));
    let hour = parse_uint(&b[11..13], 0, 23);
    let min = parse_uint(&b[14..16], 0, 59);
    let sec = parse_uint(&b[17..19], 0, 59);
    if !ok || !(b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' && b[16] == b':') {
        return None;
    }
    let mut rest = &s[19..];
    let mut nsec = 0;
    let rb = rest.as_bytes();
    if rb.len() >= 2 && rb[0] == b'.' && rb[1].is_ascii_digit() {
        let mut n = 2;
        while n < rb.len() && rb[n].is_ascii_digit() {
            n += 1;
        }
        nsec = parse_nanoseconds(rest, n).map(|x| x.0).unwrap_or(0);
        rest = &rest[n..];
    }
    let mut t = Time::date(year, month, day, hour, min, sec, nsec, Location::Utc);
    if rest != "Z" {
        let rb = rest.as_bytes();
        if rb.len() != 6 {
            return None;
        }
        let mut ok2 = true;
        let mut pu = |s: &[u8], min: i64, max: i64| -> i64 {
            let mut x = 0i64;
            for &c in s {
                if !c.is_ascii_digit() {
                    ok2 = false;
                    return min;
                }
                x = x * 10 + (c - b'0') as i64;
            }
            if x < min || max < x {
                ok2 = false;
                return min;
            }
            x
        };
        let hr = pu(&rb[1..3], 0, 23);
        let mm = pu(&rb[4..6], 0, 59);
        if !ok2 || !((rb[0] == b'-' || rb[0] == b'+') && rb[3] == b':') {
            return None;
        }
        let mut zone_offset = (hr * 60 + mm) * 60;
        if rb[0] == b'-' {
            zone_offset = -zone_offset;
        }
        t.unix -= zone_offset;
        let (_, local_off) = local.name_offset();
        t.loc = if local_off as i64 == zone_offset { local.clone() } else { Location::Fixed(String::new(), zone_offset as i32) };
    }
    Some(t)
}

/// `time.ParseInLocation(layout, value, loc)` (`time.Parse` uses UTC as the
/// default location and `local` for zone lookups).
pub fn parse_in_location(layout: &str, value: &str, default_loc: &Location, local: &Location) -> Result<Time, String> {
    if layout == "2006-01-02T15:04:05Z07:00" || layout == "2006-01-02T15:04:05.999999999Z07:00" {
        if let Some(t) = parse_rfc3339(value, local) {
            return Ok(t);
        }
    }
    let (alayout, avalue) = (layout, value);
    let mut layout = layout;
    let mut value = value;
    let mut range_err = "";
    let (mut am_set, mut pm_set) = (false, false);
    let (mut year, mut month, mut day, mut yday) = (0i64, -1i64, -1i64, -1i64);
    let (mut hour, mut min, mut sec, mut nsec) = (0i64, 0i64, 0i64, 0i64);
    let mut z: Option<Location> = None;
    let mut zone_offset: i64 = -1;
    let mut zone_name = String::new();
    loop {
        let (prefix, std, suffix) = next_std_chunk(layout);
        let stdstr = &layout[prefix.len()..layout.len() - suffix.len()];
        value = match skip(value, prefix) {
            Ok(v) => v,
            Err(v) => return Err(parse_error(alayout, avalue, prefix, v, "")),
        };
        if std == 0 {
            if !value.is_empty() {
                return Err(parse_error(alayout, avalue, "", value, &format!(": extra text: {}", time_quote(value))));
            }
            break;
        }
        layout = suffix;
        let hold = value;
        let mut err = false;
        match std & STD_MASK {
            STD_YEAR => {
                if value.len() < 2 || !value.is_char_boundary(2) {
                    err = true;
                } else {
                    let p = &value[..2];
                    value = &value[2..];
                    match atoi(p) {
                        Ok(y) => year = if y >= 69 { y + 1900 } else { y + 2000 },
                        Err(_) => err = true,
                    }
                }
            }
            STD_LONG_YEAR => {
                if value.len() < 4 || !is_digit_at(value.as_bytes(), 0) || !value.is_char_boundary(4) {
                    err = true;
                } else {
                    let p = &value[..4];
                    value = &value[4..];
                    match atoi(p) {
                        Ok(y) => year = y,
                        Err(_) => err = true,
                    }
                }
            }
            STD_MONTH | STD_LONG_MONTH => match lookup(&LONG_MONTH_NAMES, value, std & STD_MASK == STD_MONTH) {
                Ok((m, rest)) => {
                    month = m + 1;
                    value = rest;
                }
                Err(_) => {
                    month = 0;
                    err = true;
                }
            },
            STD_NUM_MONTH | STD_ZERO_MONTH => match getnum(value, std == STD_ZERO_MONTH) {
                Ok((m, rest)) => {
                    month = m;
                    value = rest;
                    if month <= 0 || 12 < month {
                        range_err = "month";
                    }
                }
                Err(_) => {
                    month = 0;
                    err = true;
                }
            },
            STD_WEEK_DAY | STD_LONG_WEEK_DAY => match lookup(&LONG_DAY_NAMES, value, std & STD_MASK == STD_WEEK_DAY) {
                Ok((_, rest)) => value = rest,
                Err(_) => err = true,
            },
            STD_DAY | STD_UNDER_DAY | STD_ZERO_DAY => {
                if std == STD_UNDER_DAY && value.starts_with(' ') {
                    value = &value[1..];
                }
                match getnum(value, std == STD_ZERO_DAY) {
                    Ok((d, rest)) => {
                        day = d;
                        value = rest;
                    }
                    Err(_) => {
                        day = 0;
                        err = true;
                    }
                }
            }
            STD_UNDER_YEAR_DAY | STD_ZERO_YEAR_DAY => {
                for _ in 0..2 {
                    if std == STD_UNDER_YEAR_DAY && value.starts_with(' ') {
                        value = &value[1..];
                    }
                }
                match getnum3(value, std == STD_ZERO_YEAR_DAY) {
                    Ok((d, rest)) => {
                        yday = d;
                        value = rest;
                    }
                    Err(_) => {
                        yday = 0;
                        err = true;
                    }
                }
            }
            STD_HOUR => match getnum(value, false) {
                Ok((h, rest)) => {
                    hour = h;
                    value = rest;
                    if !(0..24).contains(&hour) {
                        range_err = "hour";
                    }
                }
                Err(_) => {
                    hour = 0;
                    err = true;
                }
            },
            STD_HOUR12 | STD_ZERO_HOUR12 => match getnum(value, std == STD_ZERO_HOUR12) {
                Ok((h, rest)) => {
                    hour = h;
                    value = rest;
                    if !(0..=12).contains(&hour) {
                        range_err = "hour";
                    }
                }
                Err(_) => {
                    hour = 0;
                    err = true;
                }
            },
            STD_MINUTE | STD_ZERO_MINUTE => match getnum(value, std == STD_ZERO_MINUTE) {
                Ok((m, rest)) => {
                    min = m;
                    value = rest;
                    if !(0..60).contains(&min) {
                        range_err = "minute";
                    }
                }
                Err(_) => {
                    min = 0;
                    err = true;
                }
            },
            STD_SECOND | STD_ZERO_SECOND => match getnum(value, std == STD_ZERO_SECOND) {
                Ok((s, rest)) => {
                    sec = s;
                    value = rest;
                    if !(0..60).contains(&sec) {
                        range_err = "second";
                    } else {
                        let vb = value.as_bytes();
                        if vb.len() >= 2 && (vb[0] == b'.' || vb[0] == b',') && vb[1].is_ascii_digit() {
                            let (_, nstd, _) = next_std_chunk(layout);
                            let nstd = nstd & STD_MASK;
                            if nstd != STD_FRAC_SECOND0 && nstd != STD_FRAC_SECOND9 {
                                let mut n = 2;
                                while n < vb.len() && vb[n].is_ascii_digit() {
                                    n += 1;
                                }
                                match parse_nanoseconds(value, n) {
                                    Ok((ns, re)) => {
                                        nsec = ns;
                                        range_err = re;
                                    }
                                    Err(_) => err = true,
                                }
                                value = &value[n..];
                            }
                        }
                    }
                }
                Err(_) => {
                    sec = 0;
                    err = true;
                }
            },
            STD_PM | STD_PM_LOWER => {
                if value.len() < 2 || !value.is_char_boundary(2) {
                    err = true;
                } else {
                    let p = &value[..2];
                    value = &value[2..];
                    let (pm, am) = if std == STD_PM { ("PM", "AM") } else { ("pm", "am") };
                    if p == pm {
                        pm_set = true;
                    } else if p == am {
                        am_set = true;
                    } else {
                        err = true;
                    }
                }
            }
            s @ (STD_ISO8601_TZ | STD_ISO8601_SHORT_TZ | STD_ISO8601_COLON_TZ | STD_ISO8601_SECONDS_TZ
            | STD_ISO8601_COLON_SECONDS_TZ | STD_NUM_TZ | STD_NUM_SHORT_TZ | STD_NUM_COLON_TZ
            | STD_NUM_SECONDS_TZ | STD_NUM_COLON_SECONDS_TZ) => {
                let iso = matches!(
                    s,
                    STD_ISO8601_TZ | STD_ISO8601_SHORT_TZ | STD_ISO8601_COLON_TZ | STD_ISO8601_SECONDS_TZ | STD_ISO8601_COLON_SECONDS_TZ
                );
                if iso && value.starts_with('Z') {
                    value = &value[1..];
                    z = Some(Location::Utc);
                } else {
                    let vb = value.as_bytes();
                    let parts: Option<(&[u8], &[u8], &[u8], &[u8], usize)> =
                        if s == STD_ISO8601_COLON_TZ || s == STD_NUM_COLON_TZ {
                            if vb.len() < 6 || vb[3] != b':' {
                                None
                            } else {
                                Some((&vb[0..1], &vb[1..3], &vb[4..6], b"00", 6))
                            }
                        } else if s == STD_NUM_SHORT_TZ || s == STD_ISO8601_SHORT_TZ {
                            if vb.len() < 3 { None } else { Some((&vb[0..1], &vb[1..3], b"00", b"00", 3)) }
                        } else if s == STD_ISO8601_COLON_SECONDS_TZ || s == STD_NUM_COLON_SECONDS_TZ {
                            if vb.len() < 9 || vb[3] != b':' || vb[6] != b':' {
                                None
                            } else {
                                Some((&vb[0..1], &vb[1..3], &vb[4..6], &vb[7..9], 9))
                            }
                        } else if s == STD_ISO8601_SECONDS_TZ || s == STD_NUM_SECONDS_TZ {
                            if vb.len() < 7 { None } else { Some((&vb[0..1], &vb[1..3], &vb[3..5], &vb[5..7], 7)) }
                        } else if vb.len() < 5 {
                            None
                        } else {
                            Some((&vb[0..1], &vb[1..3], &vb[3..5], b"00", 5))
                        };
                    match parts {
                        None => err = true,
                        Some((sign, h, m, sc, n)) => {
                            if !value.is_char_boundary(n) {
                                err = true;
                            } else {
                                value = &value[n..];
                                let num = |x: &[u8]| -> Option<i64> {
                                    if x.len() == 2 && x[0].is_ascii_digit() && x[1].is_ascii_digit() {
                                        Some(((x[0] - b'0') * 10 + (x[1] - b'0')) as i64)
                                    } else {
                                        None
                                    }
                                };
                                let (hr, mm, ss) = match num(h) {
                                    None => {
                                        err = true;
                                        (0, 0, 0)
                                    }
                                    Some(hr) => match num(m) {
                                        None => {
                                            err = true;
                                            (hr, 0, 0)
                                        }
                                        Some(mm) => match num(sc) {
                                            None => {
                                                err = true;
                                                (hr, mm, 0)
                                            }
                                            Some(ss) => (hr, mm, ss),
                                        },
                                    },
                                };
                                if hr > 24 {
                                    range_err = "time zone offset hour";
                                }
                                if mm > 60 {
                                    range_err = "time zone offset minute";
                                }
                                if ss > 60 {
                                    range_err = "time zone offset second";
                                }
                                zone_offset = (hr * 60 + mm) * 60 + ss;
                                match sign[0] {
                                    b'+' => {}
                                    b'-' => zone_offset = -zone_offset,
                                    _ => err = true,
                                }
                            }
                        }
                    }
                }
            }
            STD_TZ => {
                if value.starts_with("UTC") {
                    z = Some(Location::Utc);
                    value = &value[3..];
                } else {
                    match parse_time_zone(value) {
                        Some(n) if value.is_char_boundary(n) => {
                            zone_name = value[..n].to_string();
                            value = &value[n..];
                        }
                        _ => err = true,
                    }
                }
            }
            STD_FRAC_SECOND0 => {
                let ndigit = 1 + digits_len(std) as usize;
                if value.len() < ndigit || !value.is_char_boundary(ndigit) {
                    err = true;
                } else {
                    match parse_nanoseconds(value, ndigit) {
                        Ok((ns, re)) => {
                            nsec = ns;
                            range_err = re;
                        }
                        Err(_) => err = true,
                    }
                    value = &value[ndigit..];
                }
            }
            STD_FRAC_SECOND9 => {
                let vb = value.as_bytes();
                if vb.len() >= 2 && (vb[0] == b'.' || vb[0] == b',') && vb[1].is_ascii_digit() {
                    let mut i = 0;
                    while i + 1 < vb.len() && vb[i + 1].is_ascii_digit() {
                        i += 1;
                    }
                    match parse_nanoseconds(value, 1 + i) {
                        Ok((ns, re)) => {
                            nsec = ns;
                            range_err = re;
                        }
                        Err(_) => err = true,
                    }
                    value = &value[1 + i..];
                }
            }
            _ => {}
        }
        if !range_err.is_empty() {
            return Err(parse_error(alayout, avalue, stdstr, value, &format!(": {range_err} out of range")));
        }
        if err {
            return Err(parse_error(alayout, avalue, stdstr, hold, ""));
        }
    }
    if pm_set && hour < 12 {
        hour += 12;
    } else if am_set && hour == 12 {
        hour = 0;
    }
    if yday >= 0 {
        let mut d = 0;
        let mut m = 0;
        if is_leap(year) {
            if yday == 31 + 29 {
                m = 2;
                d = 29;
            } else if yday > 31 + 29 {
                yday -= 1;
            }
        }
        if !(1..=365).contains(&yday) {
            return Err(parse_error(alayout, avalue, "", value, ": day-of-year out of range"));
        }
        if m == 0 {
            m = (yday - 1) / 31 + 1;
            if DAYS_BEFORE[m as usize] < yday {
                m += 1;
            }
            d = yday - DAYS_BEFORE[(m - 1) as usize];
        }
        if month >= 0 && month != m {
            return Err(parse_error(alayout, avalue, "", value, ": day-of-year does not match month"));
        }
        month = m;
        if day >= 0 && day != d {
            return Err(parse_error(alayout, avalue, "", value, ": day-of-year does not match day"));
        }
        day = d;
    } else {
        if month < 0 {
            month = 1;
        }
        if day < 0 {
            day = 1;
        }
    }
    if day < 1 || day > days_in(month, year) {
        return Err(parse_error(alayout, avalue, "", value, ": day out of range"));
    }
    if let Some(z) = z {
        return Ok(Time::date(year, month, day, hour, min, sec, nsec, z));
    }
    if zone_offset != -1 {
        let mut t = Time::date(year, month, day, hour, min, sec, nsec, Location::Utc);
        t.unix -= zone_offset;
        let (lname, loff) = local.name_offset();
        if loff as i64 == zone_offset && (zone_name.is_empty() || lname == zone_name) {
            t.loc = local.clone();
            return Ok(t);
        }
        t.loc = Location::Fixed(zone_name, zone_offset as i32);
        return Ok(t);
    }
    if !zone_name.is_empty() {
        let mut t = Time::date(year, month, day, hour, min, sec, nsec, Location::Utc);
        let (lname, loff) = local.name_offset();
        if lname == zone_name {
            t.unix -= loff as i64;
            t.loc = local.clone();
            return Ok(t);
        }
        let mut offset = 0i64;
        if zone_name.len() > 3 && zone_name.starts_with("GMT") {
            offset = atoi(&zone_name[3..]).unwrap_or(0) * 3600;
        }
        t.loc = Location::Fixed(zone_name, offset as i32);
        return Ok(t);
    }
    Ok(Time::date(year, month, day, hour, min, sec, nsec, default_loc.clone()))
}

// ---------------------------------------------------------------------------
// Template object
// ---------------------------------------------------------------------------

pub const TIME_TYPE: &str = "time.Time";

/// The `time.Time` inside an object value.
pub fn as_time(v: &Value) -> Option<Time> {
    match v {
        Value::Object(o) => o.as_any().downcast_ref::<Time>().cloned(),
        _ => None,
    }
}

/// A `time.Month` value (prints as its English name).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Month(pub i64);

impl Object for Month {
    fn type_name(&self) -> &str {
        "time.Month"
    }
    fn kind(&self) -> Kind {
        Kind::Int
    }
    fn method(&self, name: &str) -> Option<Sig> {
        (name == "String").then(|| Sig::new(&[]))
    }
    fn call_method(&self, _name: &str, _args: &[Value]) -> Result<Value, FuncError> {
        let mut s = String::new();
        self.format(&mut s);
        Ok(Value::string(s))
    }
    fn format(&self, out: &mut String) {
        if (1..=12).contains(&self.0) {
            out.push_str(LONG_MONTH_NAMES[(self.0 - 1) as usize]);
        } else {
            out.push_str(&format!("%!Month({})", self.0));
        }
    }
    fn to_data(&self) -> Value {
        Value::Int(self.0)
    }
    fn truthy(&self) -> bool {
        self.0 != 0
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// A `time.Weekday` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Weekday(pub i64);

impl Object for Weekday {
    fn type_name(&self) -> &str {
        "time.Weekday"
    }
    fn kind(&self) -> Kind {
        Kind::Int
    }
    fn method(&self, name: &str) -> Option<Sig> {
        (name == "String").then(|| Sig::new(&[]))
    }
    fn call_method(&self, _name: &str, _args: &[Value]) -> Result<Value, FuncError> {
        let mut s = String::new();
        self.format(&mut s);
        Ok(Value::string(s))
    }
    fn format(&self, out: &mut String) {
        if (0..7).contains(&self.0) {
            out.push_str(LONG_DAY_NAMES[self.0 as usize]);
        } else {
            out.push_str(&format!("%!Weekday({})", self.0));
        }
    }
    fn to_data(&self) -> Value {
        Value::Int(self.0)
    }
    fn truthy(&self) -> bool {
        self.0 != 0
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

impl Object for Time {
    fn type_name(&self) -> &str {
        TIME_TYPE
    }
    fn kind(&self) -> Kind {
        Kind::Struct
    }
    fn method(&self, name: &str) -> Option<Sig> {
        const NONE: Sig = Sig::new(&[]);
        Some(match name {
            "Year" | "Month" | "Day" | "Hour" | "Minute" | "Second" | "Nanosecond" | "YearDay" | "Weekday" | "Unix"
            | "UnixNano" | "UnixMilli" | "UnixMicro" | "UTC" | "Local" | "IsZero" | "String" | "GoString"
            | "MarshalJSON" | "MarshalText" => NONE,
            "Format" => Sig::new(&[Ty::String]),
            "Add" | "Truncate" | "Round" => Sig::new(&[Ty::Int64]),
            "AddDate" => Sig::new(&[Ty::Int, Ty::Int, Ty::Int]),
            "Sub" | "Before" | "After" | "Equal" | "Compare" => Sig::new(&[Ty::Object(TIME_TYPE)]),
            _ => return None,
        })
    }
    fn call_method(&self, name: &str, args: &[Value]) -> Result<Value, FuncError> {
        let int_arg = |i: usize| -> i64 {
            match args.get(i) {
                Some(Value::Int(n)) | Some(Value::Int64(n)) => *n,
                Some(v) => as_duration_object(v).map(|d| d.0).unwrap_or(0),
                None => 0,
            }
        };
        let time_arg = || -> Time { args.first().and_then(as_time).unwrap_or_else(Time::zero) };
        let (y, mo, d) = self.ymd();
        let (h, mi, s) = self.clock();
        Ok(match name {
            "Year" => Value::Int(y),
            "Month" => Value::object(Month(mo)),
            "Day" => Value::Int(d),
            "Hour" => Value::Int(h),
            "Minute" => Value::Int(mi),
            "Second" => Value::Int(s),
            "Nanosecond" => Value::Int(self.nsec as i64),
            "YearDay" => Value::Int(self.yday()),
            "Weekday" => Value::object(Weekday(self.weekday())),
            "Unix" => Value::Int64(self.unix),
            "UnixNano" => Value::Int64(self.unix.wrapping_mul(1_000_000_000).wrapping_add(self.nsec as i64)),
            "UnixMilli" => Value::Int64(self.unix.wrapping_mul(1000).wrapping_add(self.nsec as i64 / 1_000_000)),
            "UnixMicro" => Value::Int64(self.unix.wrapping_mul(1_000_000).wrapping_add(self.nsec as i64 / 1000)),
            "UTC" => Value::object(self.in_loc(Location::Utc)),
            "Local" => {
                let off = match &self.loc {
                    Location::Local(o) => *o,
                    _ => 0,
                };
                Value::object(self.in_loc(Location::Local(off)))
            }
            "IsZero" => Value::Bool(self.is_zero()),
            "String" => Value::string(self.go_string()),
            "GoString" => Value::string(self.go_string()),
            "MarshalJSON" | "MarshalText" => {
                let s = self.format("2006-01-02T15:04:05.999999999Z07:00");
                let s = if name == "MarshalJSON" { format!("\"{s}\"") } else { s };
                Value::Bytes(s.into_bytes().into())
            }
            "Format" => Value::string(self.format(args.first().and_then(Value::as_str).unwrap_or(""))),
            "Add" => Value::object(self.add(Duration(int_arg(0)))),
            "Truncate" => Value::object(self.truncate(Duration(int_arg(0)))),
            "Round" => Value::object(self.round(Duration(int_arg(0)))),
            "AddDate" => Value::object(Time::date(
                y.wrapping_add(int_arg(0)),
                mo.wrapping_add(int_arg(1)),
                d.wrapping_add(int_arg(2)),
                h,
                mi,
                s,
                self.nsec as i64,
                self.loc.clone(),
            )),
            "Sub" => Value::object(self.sub(&time_arg())),
            "Before" => Value::Bool(self.total_nanos() < time_arg().total_nanos()),
            "After" => Value::Bool(self.total_nanos() > time_arg().total_nanos()),
            "Equal" => Value::Bool(self.total_nanos() == time_arg().total_nanos()),
            "Compare" => Value::Int(match self.total_nanos().cmp(&time_arg().total_nanos()) {
                core::cmp::Ordering::Less => -1,
                core::cmp::Ordering::Equal => 0,
                core::cmp::Ordering::Greater => 1,
            }),
            _ => return Err(FuncError::new(format!("can't call method {name} on time.Time"))),
        })
    }
    fn format(&self, out: &mut String) {
        out.push_str(&self.go_string());
    }
    fn to_data(&self) -> Value {
        Value::string(self.format("2006-01-02T15:04:05.999999999Z07:00"))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
