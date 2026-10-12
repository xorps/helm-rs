//! Ports of the bits of Go's `strconv` (and `fmt`) that the encoders need:
//! float formatting (`FormatFloat` with shortest precision), `ParseFloat`,
//! `ParseInt`/`ParseUint` with base prefixes, and `Quote` for single runes.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::isprint::is_print;

/// Shortest decimal representation of `|f|` (finite): ASCII digits without
/// leading/trailing zeros and the decimal point position `dp`, so that
/// `|f| = 0.d1d2... * 10^dp`. Zero yields no digits and `dp == 0`.
fn shortest_digits(f: f64, bits32: bool) -> (Vec<u8>, i32) {
    let s = if bits32 { format!("{:e}", (f as f32).abs()) } else { format!("{:e}", f.abs()) };
    let (mant, exp) = match s.split_once('e') {
        Some(p) => p,
        None => (s.as_str(), "0"),
    };
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).collect();
    while digits.last() == Some(&b'0') {
        digits.pop();
    }
    if digits.is_empty() {
        return (digits, 0);
    }
    (digits, exp + 1)
}

/// `strconv.FormatFloat(f, fmt, -1, bits)` for `fmt` in `e`, `f`, `g`.
pub(crate) fn format_float(f: f64, fmt: u8, bits32: bool) -> String {
    let f = if bits32 { f as f32 as f64 } else { f };
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    let neg = f.is_sign_negative();
    let (s, dp) = shortest_digits(f, bits32);
    let nd = s.len() as i32;
    let mut prec = match fmt {
        b'e' => (nd - 1).max(0),
        b'f' => (nd - dp).max(0),
        _ => nd,
    };
    let mut fmt = fmt;
    if fmt == b'g' {
        let eprec = 6;
        let exp = dp - 1;
        if exp < -4 || exp >= eprec {
            if prec > nd {
                prec = nd;
            }
            prec -= 1;
            fmt = b'e';
        } else {
            if prec > dp {
                prec = nd;
            }
            prec = (prec - dp).max(0);
            fmt = b'f';
        }
    }
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if fmt == b'e' {
        out.push(if nd != 0 { s[0] as char } else { '0' });
        if prec > 0 {
            out.push('.');
            let mut i = 1;
            let m = nd.min(prec + 1);
            while i < m {
                out.push(s[i as usize] as char);
                i += 1;
            }
            while i < prec + 1 {
                out.push('0');
                i += 1;
            }
        }
        out.push('e');
        let mut exp = dp - 1;
        if nd == 0 {
            exp = 0;
        }
        if exp < 0 {
            out.push('-');
            exp = -exp;
        } else {
            out.push('+');
        }
        if exp < 10 {
            out.push('0');
            out.push((b'0' + exp as u8) as char);
        } else if exp < 100 {
            out.push((b'0' + (exp / 10) as u8) as char);
            out.push((b'0' + (exp % 10) as u8) as char);
        } else {
            out.push((b'0' + (exp / 100) as u8) as char);
            out.push((b'0' + ((exp / 10) % 10) as u8) as char);
            out.push((b'0' + (exp % 10) as u8) as char);
        }
        return out;
    }
    // 'f'
    if dp > 0 {
        let m = nd.min(dp);
        for &c in &s[..m as usize] {
            out.push(c as char);
        }
        for _ in m..dp {
            out.push('0');
        }
    } else {
        out.push('0');
    }
    if prec > 0 {
        out.push('.');
        let lz = prec.min((-dp).max(0));
        let off = dp + lz;
        let m = (prec - lz).min((nd - off).max(0));
        let tz = (prec - lz - m).max(0);
        for _ in 0..lz {
            out.push('0');
        }
        for i in 0..m {
            out.push(s[(off + i) as usize] as char);
        }
        for _ in 0..tz {
            out.push('0');
        }
    }
    out
}

/// Go's `strconv.FormatFloat(f, 'g', -1, 64)`.
#[cfg(test)]
pub(crate) fn format_g(f: f64) -> String {
    format_float(f, b'g', false)
}

fn lower(c: u8) -> u8 {
    c | 0x20
}

/// Go's `underscoreOK`.
fn underscore_ok(s: &[u8]) -> bool {
    let mut saw = b'^';
    let mut i = 0;
    let mut s = s;
    if !s.is_empty() && (s[0] == b'-' || s[0] == b'+') {
        s = &s[1..];
    }
    let mut hex = false;
    if s.len() >= 2 && s[0] == b'0' && matches!(lower(s[1]), b'b' | b'o' | b'x') {
        i = 2;
        saw = b'0';
        hex = lower(s[1]) == b'x';
    }
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_digit() || hex && (b'a'..=b'f').contains(&lower(c)) {
            saw = b'0';
            i += 1;
            continue;
        }
        if c == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
            i += 1;
            continue;
        }
        if saw == b'_' {
            return false;
        }
        saw = b'!';
        i += 1;
    }
    saw != b'_'
}

/// Error kinds of Go's `strconv` parse functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumErr {
    Syntax,
    Range,
}

/// Go's `strconv.ParseFloat(s, 64)` (decimal, hex and special forms).
pub(crate) fn parse_float(s: &str) -> Result<f64, NumErr> {
    let b = s.as_bytes();
    // special(): optional sign, then "inf", "infinity" or "nan" (case-insensitive).
    {
        let (sign, rest) = match b.first() {
            Some(b'+') => (1.0, &b[1..]),
            Some(b'-') => (-1.0, &b[1..]),
            _ => (1.0, b),
        };
        let lw: Vec<u8> = rest.iter().map(|c| c.to_ascii_lowercase()).collect();
        if lw == b"infinity" || lw == b"inf" {
            return Ok(sign * f64::INFINITY);
        }
        if lw == b"nan" && rest.len() == b.len() {
            return Ok(f64::NAN);
        }
    }
    // readFloat: validate the syntax.
    let mut i = 0;
    let mut underscores = false;
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let mut hex = false;
    if i + 2 < b.len() && b[i] == b'0' && lower(b[i + 1]) == b'x' {
        hex = true;
        i += 2;
    }
    let mut sawdot = false;
    let mut sawdigits = false;
    let mut mant = String::new();
    let mut dot_pos: Option<usize> = None;
    while i < b.len() {
        let c = b[i];
        if c == b'_' {
            underscores = true;
        } else if c == b'.' {
            if sawdot {
                break;
            }
            sawdot = true;
            dot_pos = Some(mant.len());
        } else if c.is_ascii_digit() || hex && (b'a'..=b'f').contains(&lower(c)) {
            sawdigits = true;
            mant.push(c as char);
        } else {
            break;
        }
        i += 1;
    }
    if !sawdigits {
        return Err(NumErr::Syntax);
    }
    let exp_char = if hex { b'p' } else { b'e' };
    let mut exp: i64 = 0;
    if i < b.len() && lower(b[i]) == exp_char {
        i += 1;
        if i >= b.len() {
            return Err(NumErr::Syntax);
        }
        let mut esign = 1;
        if b[i] == b'+' {
            i += 1;
        } else if b[i] == b'-' {
            i += 1;
            esign = -1;
        }
        if i >= b.len() || !b[i].is_ascii_digit() {
            return Err(NumErr::Syntax);
        }
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            if b[i] == b'_' {
                underscores = true;
            } else if exp < 100_000 {
                exp = exp * 10 + (b[i] - b'0') as i64;
            }
            i += 1;
        }
        exp *= esign;
    } else if hex {
        return Err(NumErr::Syntax);
    }
    if underscores && !underscore_ok(&b[..i]) {
        return Err(NumErr::Syntax);
    }
    if i != b.len() {
        return Err(NumErr::Syntax);
    }
    let v = if hex {
        parse_hex_float(&mant, dot_pos, exp)
    } else {
        let mut t = String::with_capacity(mant.len() + 16);
        match dot_pos {
            Some(p) => {
                t.push_str(&mant[..p]);
                t.push('.');
                t.push_str(&mant[p..]);
            }
            None => t.push_str(&mant),
        }
        if t.starts_with('.') {
            t.insert(0, '0');
        }
        if t.ends_with('.') {
            t.push('0');
        }
        t.push('e');
        t.push_str(&format!("{exp}"));
        t.parse::<f64>().map_err(|_| NumErr::Syntax)?
    };
    let v = if neg { -v } else { v };
    if v.is_infinite() {
        return Err(NumErr::Range);
    }
    Ok(v)
}

fn parse_hex_float(mant: &str, dot_pos: Option<usize>, exp: i64) -> f64 {
    // Exact enough: accumulate up to 16 significant hex digits, track the
    // binary exponent, then scale (rounding may differ from Go in extreme
    // cases with >16 significant digits).
    let digits = mant.trim_start_matches('0');
    let lead_zeros = mant.len() - digits.len();
    let int_len = dot_pos.unwrap_or(mant.len()) as i64 - lead_zeros as i64;
    let mut m: u64 = 0;
    let mut used = 0i64;
    for c in digits.bytes().take(16) {
        let d = (c as char).to_digit(16).unwrap_or(0) as u64;
        m = m * 16 + d;
        used += 1;
    }
    let e2 = exp + 4 * (int_len - used);
    let mut v = m as f64;
    let mut e = e2;
    while e > 0 {
        let step = e.min(60);
        v *= (1u64 << step) as f64;
        e -= step;
        if v.is_infinite() {
            break;
        }
    }
    while e < 0 {
        let step = (-e).min(60);
        v /= (1u64 << step) as f64;
        e += step;
        if v == 0.0 {
            break;
        }
    }
    v
}

/// Go's `strconv.ParseUint(s, base, 64)` (base 0 detects prefixes).
pub(crate) fn parse_uint(s: &str, base: u32) -> Result<u64, NumErr> {
    let mut b = s.as_bytes();
    if b.is_empty() {
        return Err(NumErr::Syntax);
    }
    let base0 = base == 0;
    let s0 = b;
    let mut base = base;
    if base == 0 {
        base = 10;
        if b[0] == b'0' {
            if b.len() >= 3 && lower(b[1]) == b'b' {
                base = 2;
                b = &b[2..];
            } else if b.len() >= 3 && lower(b[1]) == b'o' {
                base = 8;
                b = &b[2..];
            } else if b.len() >= 3 && lower(b[1]) == b'x' {
                base = 16;
                b = &b[2..];
            } else {
                base = 8;
                b = &b[1..];
            }
        }
    }
    let mut n: u64 = 0;
    let mut underscores = false;
    let mut range = false;
    for &c in b {
        let d = match c {
            b'_' if base0 => {
                underscores = true;
                continue;
            }
            b'0'..=b'9' => (c - b'0') as u32,
            b'a'..=b'z' | b'A'..=b'Z' => (lower(c) - b'a') as u32 + 10,
            _ => return Err(NumErr::Syntax),
        };
        if d >= base {
            return Err(NumErr::Syntax);
        }
        if range {
            continue;
        }
        match n.checked_mul(base as u64).and_then(|x| x.checked_add(d as u64)) {
            Some(x) => n = x,
            None => range = true,
        }
    }
    if underscores && !underscore_ok(s0) {
        return Err(NumErr::Syntax);
    }
    if range {
        return Err(NumErr::Range);
    }
    Ok(n)
}

/// Go's `strconv.ParseInt(s, base, 64)`.
pub(crate) fn parse_int(s: &str, base: u32) -> Result<i64, NumErr> {
    if s.is_empty() {
        return Err(NumErr::Syntax);
    }
    let (neg, rest) = match s.as_bytes()[0] {
        b'+' => (false, &s[1..]),
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    let un = parse_uint(rest, base)?;
    let cutoff = 1u64 << 63;
    if !neg && un >= cutoff {
        return Err(NumErr::Range);
    }
    if neg && un > cutoff {
        return Err(NumErr::Range);
    }
    Ok(if neg { (un as i64).wrapping_neg() } else { un as i64 })
}

/// Go's `strconv.Quote(s)`.
pub(crate) fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x0b' => out.push_str("\\v"),
            c if is_print(c) => out.push(c),
            c if (c as u32) < 0x80 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push('"');
    out
}

/// Go's `strconv.Quote` of a byte string that may be invalid UTF-8 (invalid
/// bytes print as `\xNN`).
pub(crate) fn quote_bytes(b: &[u8]) -> String {
    let mut out = String::from("\"");
    let mut i = 0;
    while i < b.len() {
        match core::str::from_utf8(&b[i..]) {
            Ok(s) => {
                let q = quote(s);
                out.push_str(&q[1..q.len() - 1]);
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                let q = quote(core::str::from_utf8(&b[i..i + good]).unwrap_or(""));
                out.push_str(&q[1..q.len() - 1]);
                i += good;
                if i < b.len() {
                    out.push_str(&format!("\\x{:02x}", b[i]));
                    i += 1;
                }
            }
        }
    }
    out.push('"');
    out
}

/// Go's conversion of a byte string to valid UTF-8 the way `encoding/json`
/// does it: every byte that does not start a valid sequence becomes U+FFFD.
pub(crate) fn go_lossy_utf8(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match core::str::from_utf8(&b[i..]) {
            Ok(s) => {
                out.push_str(s);
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                // SAFETY-free: the prefix is valid UTF-8.
                out.push_str(core::str::from_utf8(&b[i..i + good]).unwrap_or(""));
                i += good;
                if i < b.len() {
                    out.push('\u{FFFD}');
                    i += 1;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats() {
        assert_eq!(format_g(1e6), "1e+06");
        assert_eq!(format_g(100000.0), "100000");
        assert_eq!(format_g(0.0001), "0.0001");
        assert_eq!(format_g(0.00001), "1e-05");
        assert_eq!(format_g(1.5), "1.5");
        assert_eq!(format_g(-0.0), "-0");
        assert_eq!(format_g(1e21), "1e+21");
        assert_eq!(format_g(1.7976931348623157e308), "1.7976931348623157e+308");
        assert_eq!(format_float(1234567.0, b'g', true), "1.234567e+06");
        assert_eq!(format_float(0.1, b'g', true), "0.1");
        assert_eq!(format_float(1e20, b'f', false), "100000000000000000000");
        assert_eq!(format_float(1e-7, b'e', false), "1e-07");
        assert_eq!(format_float(0.000001, b'f', false), "0.000001");
        assert_eq!(format_float(123.456, b'e', false), "1.23456e+02");
    }

    #[test]
    fn ints() {
        assert_eq!(parse_int("0755", 0), Ok(493));
        assert_eq!(parse_int("08", 0), Err(NumErr::Syntax));
        assert_eq!(parse_int("0", 0), Ok(0));
        assert_eq!(parse_int("-0x10", 0), Ok(-16));
        assert_eq!(parse_int("9223372036854775808", 0), Err(NumErr::Range));
        assert_eq!(parse_int("-9223372036854775808", 0), Ok(i64::MIN));
        assert_eq!(parse_uint("18446744073709551615", 0), Ok(u64::MAX));
        assert_eq!(parse_uint("0x", 0), Err(NumErr::Syntax));
    }

    #[test]
    fn parse_floats() {
        assert_eq!(parse_float(".5"), Ok(0.5));
        assert_eq!(parse_float("1e400"), Err(NumErr::Range));
        assert_eq!(parse_float("1e-400"), Ok(0.0));
        assert_eq!(parse_float("._5"), Err(NumErr::Syntax));
        assert_eq!(parse_float("1."), Ok(1.0));
        assert_eq!(parse_float("."), Err(NumErr::Syntax));
        assert!(parse_float("inf").unwrap().is_infinite());
    }
}
