//! Go's `time.Duration` and Helm's duration helpers (`mustToDuration`,
//! `durationSeconds`, ..., `durationRoundTo`, `durationTruncateTo`), from
//! `helm.sh/helm/v4/pkg/engine/funcs.go`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::func::{FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::gofmt::strconv;
use crate::value::{Kind, Object, Value};

pub const NANOSECOND: i64 = 1;
pub const MICROSECOND: i64 = 1000;
pub const MILLISECOND: i64 = 1_000_000;
pub const SECOND: i64 = 1_000_000_000;
pub const MINUTE: i64 = 60 * SECOND;
pub const HOUR: i64 = 60 * MINUTE;

/// A Go `time.Duration` (nanoseconds). `%v` prints like Go (`1h2m3.5s`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Duration(pub i64);

impl Duration {
    /// `Duration.String()`.
    pub fn go_string(self) -> String {
        let mut buf = [0u8; 32];
        let mut w = buf.len();
        let d = self.0;
        let mut u = d as u64;
        let neg = d < 0;
        if neg {
            u = u.wrapping_neg();
        }
        fn fmt_frac(buf: &mut [u8], mut w: usize, mut v: u64, prec: usize) -> (usize, u64) {
            let mut print = false;
            for _ in 0..prec {
                let digit = v % 10;
                print = print || digit != 0;
                if print {
                    w -= 1;
                    buf[w] = digit as u8 + b'0';
                }
                v /= 10;
            }
            if print {
                w -= 1;
                buf[w] = b'.';
            }
            (w, v)
        }
        fn fmt_int(buf: &mut [u8], mut w: usize, mut v: u64) -> usize {
            if v == 0 {
                w -= 1;
                buf[w] = b'0';
            } else {
                while v > 0 {
                    w -= 1;
                    buf[w] = (v % 10) as u8 + b'0';
                    v /= 10;
                }
            }
            w
        }
        if u < SECOND as u64 {
            let prec;
            w -= 1;
            buf[w] = b's';
            w -= 1;
            if u == 0 {
                return String::from("0s");
            } else if u < MICROSECOND as u64 {
                prec = 0;
                buf[w] = b'n';
            } else if u < MILLISECOND as u64 {
                prec = 3;
                w -= 1;
                buf[w] = 0xC2;
                buf[w + 1] = 0xB5;
            } else {
                prec = 6;
                buf[w] = b'm';
            }
            let (nw, nu) = fmt_frac(&mut buf, w, u, prec);
            w = fmt_int(&mut buf, nw, nu);
        } else {
            w -= 1;
            buf[w] = b's';
            let (nw, nu) = fmt_frac(&mut buf, w, u, 9);
            w = fmt_int(&mut buf, nw, nu % 60);
            u = nu / 60;
            if u > 0 {
                w -= 1;
                buf[w] = b'm';
                w = fmt_int(&mut buf, w, u % 60);
                u /= 60;
                if u > 0 {
                    w -= 1;
                    buf[w] = b'h';
                    w = fmt_int(&mut buf, w, u);
                }
            }
        }
        if neg {
            w -= 1;
            buf[w] = b'-';
        }
        strconv::lossy(&buf[w..])
    }

    pub fn seconds(self) -> f64 {
        let sec = self.0 / SECOND;
        let nsec = self.0 % SECOND;
        sec as f64 + nsec as f64 / 1e9
    }
    pub fn minutes(self) -> f64 {
        let min = self.0 / MINUTE;
        let nsec = self.0 % MINUTE;
        min as f64 + nsec as f64 / (60.0 * 1e9)
    }
    pub fn hours(self) -> f64 {
        let hour = self.0 / HOUR;
        let nsec = self.0 % HOUR;
        hour as f64 + nsec as f64 / (60.0 * 60.0 * 1e9)
    }

    /// `Duration.Truncate(m)`.
    pub fn truncate(self, m: Duration) -> Duration {
        if m.0 <= 0 {
            return self;
        }
        Duration(self.0 - self.0 % m.0)
    }

    /// `Duration.Round(m)`.
    pub fn round(self, m: Duration) -> Duration {
        if m.0 <= 0 {
            return self;
        }
        let d = self.0;
        let m = m.0;
        let less_than_half = |x: i64, y: i64| (x as u64).wrapping_add(x as u64) < y as u64;
        let mut r = d % m;
        if d < 0 {
            r = r.wrapping_neg();
            if less_than_half(r, m) {
                return Duration(d.wrapping_add(r));
            }
            let d1 = d.wrapping_sub(m).wrapping_add(r);
            if d1 < d {
                return Duration(d1);
            }
            return Duration(i64::MIN);
        }
        if less_than_half(r, m) {
            return Duration(d - r);
        }
        let d1 = d.wrapping_add(m).wrapping_sub(r);
        if d1 > d {
            return Duration(d1);
        }
        Duration(i64::MAX)
    }

    pub fn abs(self) -> Duration {
        if self.0 >= 0 {
            self
        } else if self.0 == i64::MIN {
            Duration(i64::MAX)
        } else {
            Duration(-self.0)
        }
    }
}

impl Object for Duration {
    fn type_name(&self) -> &str {
        "time.Duration"
    }
    fn kind(&self) -> Kind {
        Kind::Int64
    }
    fn method(&self, name: &str) -> Option<Sig> {
        match name {
            "String" | "Hours" | "Minutes" | "Seconds" | "Milliseconds" | "Microseconds"
            | "Nanoseconds" | "Abs" => Some(Sig::new(&[])),
            "Round" | "Truncate" => Some(Sig::new(&[Ty::Int64])),
            _ => None,
        }
    }
    fn call_method(&self, name: &str, args: &[Value]) -> Result<Value, FuncError> {
        let d = *self;
        Ok(match name {
            "String" => Value::string(d.go_string()),
            "Hours" => Value::Float(d.hours()),
            "Minutes" => Value::Float(d.minutes()),
            "Seconds" => Value::Float(d.seconds()),
            "Milliseconds" => Value::Int64(d.0 / MILLISECOND),
            "Microseconds" => Value::Int64(d.0 / MICROSECOND),
            "Nanoseconds" => Value::Int64(d.0),
            "Abs" => Value::object(d.abs()),
            "Round" | "Truncate" => {
                let m = match args.first() {
                    Some(Value::Int64(n)) | Some(Value::Int(n)) => Duration(*n),
                    Some(v) => match as_duration_object(v) {
                        Some(m) => m,
                        None => return Err(FuncError::new("wrong argument type")),
                    },
                    None => Duration(0),
                };
                Value::object(if name == "Round" {
                    d.round(m)
                } else {
                    d.truncate(m)
                })
            }
            _ => return Err(FuncError::new(format!("no method {name}"))),
        })
    }
    fn format(&self, out: &mut String) {
        out.push_str(&self.go_string());
    }
    fn to_data(&self) -> Value {
        Value::Int64(self.0)
    }
    fn truthy(&self) -> bool {
        self.0 != 0
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// The duration inside a `time.Duration` object value, if it is one.
pub fn as_duration_object(v: &Value) -> Option<Duration> {
    match v {
        Value::Object(o) => o.as_any().downcast_ref::<Duration>().copied(),
        _ => None,
    }
}

/// Go's `time` package `quote` (used in its error messages).
pub fn time_quote(s: &str) -> String {
    const LOWERHEX: &[u8] = b"0123456789abcdef";
    let mut buf: Vec<u8> = Vec::with_capacity(s.len() + 2);
    buf.push(b'"');
    let bytes = s.as_bytes();
    for (i, c) in s.char_indices() {
        if c as u32 >= 0x80 || (c as u32) < 0x20 {
            let width = c.len_utf8();
            for j in 0..width {
                buf.extend_from_slice(b"\\x");
                buf.push(LOWERHEX[(bytes[i + j] >> 4) as usize]);
                buf.push(LOWERHEX[(bytes[i + j] & 0xF) as usize]);
            }
        } else {
            if c == '"' || c == '\\' {
                buf.push(b'\\');
            }
            buf.push(c as u8);
        }
    }
    buf.push(b'"');
    strconv::lossy_vec(buf)
}

fn leading_int(s: &[u8]) -> Result<(u64, &[u8]), ()> {
    let mut i = 0;
    let mut x: u64 = 0;
    while i < s.len() {
        let c = s[i];
        if !c.is_ascii_digit() {
            break;
        }
        if x > (1u64 << 63) / 10 {
            return Err(());
        }
        x = x * 10 + (c - b'0') as u64;
        if x > 1 << 63 {
            return Err(());
        }
        i += 1;
    }
    Ok((x, &s[i..]))
}

fn leading_fraction(s: &[u8]) -> (u64, f64, &[u8]) {
    let mut i = 0;
    let mut x: u64 = 0;
    let mut scale = 1.0;
    let mut overflow = false;
    while i < s.len() {
        let c = s[i];
        if !c.is_ascii_digit() {
            break;
        }
        i += 1;
        if overflow {
            continue;
        }
        if x > ((1u64 << 63) - 1) / 10 {
            overflow = true;
            continue;
        }
        let y = x * 10 + (c - b'0') as u64;
        if y > 1 << 63 {
            overflow = true;
            continue;
        }
        x = y;
        scale *= 10.0;
    }
    (x, scale, &s[i..])
}

/// `time.ParseDuration`, with Go's error messages.
pub fn parse_duration(orig: &str) -> Result<Duration, String> {
    let invalid = || format!("time: invalid duration {}", time_quote(orig));
    let mut s = orig.as_bytes();
    let mut d: u64 = 0;
    let mut neg = false;
    if !s.is_empty() {
        let c = s[0];
        if c == b'-' || c == b'+' {
            neg = c == b'-';
            s = &s[1..];
        }
    }
    if s == b"0" {
        return Ok(Duration(0));
    }
    if s.is_empty() {
        return Err(invalid());
    }
    while !s.is_empty() {
        let mut f: u64 = 0;
        let mut scale = 1.0;
        if !(s[0] == b'.' || s[0].is_ascii_digit()) {
            return Err(invalid());
        }
        let pl = s.len();
        let (mut v, rest) = leading_int(s).map_err(|_| invalid())?;
        s = rest;
        let pre = pl != s.len();
        let mut post = false;
        if !s.is_empty() && s[0] == b'.' {
            s = &s[1..];
            let pl = s.len();
            let (ff, sc, rest) = leading_fraction(s);
            f = ff;
            scale = sc;
            s = rest;
            post = pl != s.len();
        }
        if !pre && !post {
            return Err(invalid());
        }
        let mut i = 0;
        while i < s.len() {
            let c = s[i];
            if c == b'.' || c.is_ascii_digit() {
                break;
            }
            i += 1;
        }
        if i == 0 {
            return Err(format!(
                "time: missing unit in duration {}",
                time_quote(orig)
            ));
        }
        let u = &s[..i];
        s = &s[i..];
        let unit: u64 = match u {
            b"ns" => 1,
            b"us" => 1000,
            b"\xC2\xB5s" => 1000,
            b"\xCE\xBCs" => 1000,
            b"ms" => 1_000_000,
            b"s" => 1_000_000_000,
            b"m" => 60_000_000_000,
            b"h" => 3_600_000_000_000,
            _ => {
                let us = strconv::lossy(u);
                return Err(format!(
                    "time: unknown unit {} in duration {}",
                    time_quote(&us),
                    time_quote(orig)
                ));
            }
        };
        if v > (1u64 << 63) / unit {
            return Err(invalid());
        }
        v *= unit;
        if f > 0 {
            v = v.wrapping_add((f as f64 * (unit as f64 / scale)) as u64);
            if v > 1 << 63 {
                return Err(invalid());
            }
        }
        d = d.wrapping_add(v);
        if d > 1 << 63 {
            return Err(invalid());
        }
    }
    if neg {
        return Ok(Duration((d as i64).wrapping_neg()));
    }
    if d > (1u64 << 63) - 1 {
        return Err(invalid());
    }
    Ok(Duration(d as i64))
}

/// Go's (amd64) `int64(f)` conversion: out-of-range and NaN give MinInt64.
pub fn f64_to_i64(f: f64) -> i64 {
    if f.is_nan() || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&f) {
        i64::MIN
    } else {
        f as i64
    }
}

const MAX_DURATION_SECONDS: i64 = i64::MAX / SECOND;
const MIN_DURATION_SECONDS: i64 = i64::MIN / SECOND;
const MAX_DURATION_SECONDS_FLOAT: f64 = i64::MAX as f64 / SECOND as f64;
const MIN_DURATION_SECONDS_FLOAT: f64 = i64::MIN as f64 / SECOND as f64;

fn from_seconds_float(seconds: f64) -> Result<Duration, String> {
    if seconds.is_nan() || seconds.is_infinite() {
        return Err(format!(
            "invalid duration seconds: {}",
            crate::gofmt::format_value(&Value::Float(seconds))
        ));
    }
    if !(MIN_DURATION_SECONDS_FLOAT..=MAX_DURATION_SECONDS_FLOAT).contains(&seconds) {
        return Err(format!(
            "duration seconds overflow: {}",
            crate::gofmt::format_value(&Value::Float(seconds))
        ));
    }
    let nanos = seconds * SECOND as f64;
    if nanos > i64::MAX as f64 || nanos < i64::MIN as f64 {
        return Err(format!(
            "duration nanoseconds overflow: {}",
            crate::gofmt::format_value(&Value::Float(nanos))
        ));
    }
    Ok(Duration(f64_to_i64(nanos)))
}

/// Helm's `asDuration`.
pub fn as_duration(v: &Value) -> Result<Duration, String> {
    if let Some(d) = as_duration_object(v) {
        return Ok(d);
    }
    match v {
        Value::String(x) => {
            let s = trim_space(x);
            if s.is_empty() {
                return Err(String::from("empty duration"));
            }
            if let Ok(d) = parse_duration(s) {
                return Ok(d);
            }
            if let Ok(f) = strconv::parse_float(s) {
                return from_seconds_float(f);
            }
            Err(format!("could not parse duration {}", strconv::quote(x)))
        }
        Value::Nil => Err(String::from("invalid duration")),
        Value::Int(n) | Value::Int64(n) => {
            if *n > MAX_DURATION_SECONDS || *n < MIN_DURATION_SECONDS {
                return Err(format!("duration seconds overflow: {n}"));
            }
            Ok(Duration(n * SECOND))
        }
        Value::Uint64(n) => {
            if *n > MAX_DURATION_SECONDS as u64 {
                return Err(format!("duration seconds overflow: {n}"));
            }
            Ok(Duration(*n as i64 * SECOND))
        }
        Value::Float(f) => from_seconds_float(*f),
        Value::Object(o) if o.kind() == Kind::Int64 || o.kind() == Kind::Int => {
            // Other integer-kinded objects: use their data value.
            match o.to_data() {
                Value::Int(n) | Value::Int64(n) => as_duration(&Value::Int64(n)),
                _ => Err(format!("unsupported duration type {}", v.type_name())),
            }
        }
        _ => Err(format!("unsupported duration type {}", v.type_name())),
    }
}

/// `strings.TrimSpace`.
pub fn trim_space(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

fn arg0(a: &[Value]) -> &Value {
    a.first().unwrap_or(&Value::Nil)
}

fn must_to_duration(a: &[Value]) -> FuncResult {
    as_duration(arg0(a))
        .map(Value::object)
        .map_err(FuncError::new)
}

fn float_or_zero(a: &[Value], f: fn(Duration) -> f64) -> FuncResult {
    Ok(Value::Float(as_duration(arg0(a)).map(f).unwrap_or(0.0)))
}

fn int_or_zero(a: &[Value], f: fn(Duration) -> i64) -> FuncResult {
    Ok(Value::Int64(as_duration(arg0(a)).map(f).unwrap_or(0)))
}

fn round_to(a: &[Value], truncate: bool) -> FuncResult {
    let Ok(d) = as_duration(arg0(a)) else {
        return Ok(Value::object(Duration(0)));
    };
    let Ok(m) = as_duration(a.get(1).unwrap_or(&Value::Nil)) else {
        return Ok(Value::object(d));
    };
    Ok(Value::object(if truncate {
        d.truncate(m)
    } else {
        d.round(m)
    }))
}

/// Registers Helm's duration helpers.
pub fn register(f: &mut FuncMap) {
    const ANY1: Sig = Sig::new(&[Ty::Any]);
    const ANY2: Sig = Sig::new(&[Ty::Any, Ty::Any]);
    f.add("mustToDuration", ANY1, must_to_duration);
    f.add("durationSeconds", ANY1, |a| {
        float_or_zero(a, Duration::seconds)
    });
    f.add("durationMilliseconds", ANY1, |a| {
        int_or_zero(a, |d| d.0 / MILLISECOND)
    });
    f.add("durationMicroseconds", ANY1, |a| {
        int_or_zero(a, |d| d.0 / MICROSECOND)
    });
    f.add("durationNanoseconds", ANY1, |a| int_or_zero(a, |d| d.0));
    f.add("durationMinutes", ANY1, |a| {
        float_or_zero(a, Duration::minutes)
    });
    f.add("durationHours", ANY1, |a| float_or_zero(a, Duration::hours));
    f.add("durationDays", ANY1, |a| {
        float_or_zero(a, |d| d.hours() / 24.0)
    });
    f.add("durationWeeks", ANY1, |a| {
        float_or_zero(a, |d| d.hours() / 24.0 / 7.0)
    });
    f.add("durationRoundTo", ANY2, |a| round_to(a, false));
    f.add("durationTruncateTo", ANY2, |a| round_to(a, true));
}
