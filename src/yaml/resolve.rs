//! Implicit tag resolution (`resolve.go` of goyaml v2/v3) and helpers.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::strconv::{parse_float, parse_int, parse_uint};

pub(crate) const NULL_TAG: &str = "tag:yaml.org,2002:null";
pub(crate) const BOOL_TAG: &str = "tag:yaml.org,2002:bool";
pub(crate) const STR_TAG: &str = "tag:yaml.org,2002:str";
pub(crate) const INT_TAG: &str = "tag:yaml.org,2002:int";
pub(crate) const FLOAT_TAG: &str = "tag:yaml.org,2002:float";
pub(crate) const TIMESTAMP_TAG: &str = "tag:yaml.org,2002:timestamp";
pub(crate) const BINARY_TAG: &str = "tag:yaml.org,2002:binary";
pub(crate) const MERGE_TAG: &str = "tag:yaml.org,2002:merge";

const LONG_TAG_PREFIX: &str = "tag:yaml.org,2002:";

/// Go's `shortTag`.
pub(crate) fn short_tag(tag: &str) -> String {
    match tag.strip_prefix(LONG_TAG_PREFIX) {
        Some(rest) => format!("!!{rest}"),
        None => tag.into(),
    }
}

/// A resolved scalar value (Go's `interface{}` results of `resolve`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Resolved {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    /// The input string itself.
    Str,
    /// Looks like a timestamp (decoded as the original string).
    Timestamp,
    /// The merge key `<<`.
    Merge,
}

fn hint(in_: &str) -> u8 {
    match in_.as_bytes().first() {
        None => b'N',
        Some(b'+') | Some(b'-') => b'S',
        Some(b'0'..=b'9') => b'D',
        Some(b'y' | b'Y' | b'n' | b'N' | b't' | b'T' | b'f' | b'F' | b'o' | b'O' | b'~') => b'M',
        Some(b'.') => b'.',
        _ => 0,
    }
}

fn resolve_map(s: &str, v2: bool) -> Option<(&'static str, Resolved)> {
    let r = match s {
        "true" | "True" | "TRUE" => (BOOL_TAG, Resolved::Bool(true)),
        "false" | "False" | "FALSE" => (BOOL_TAG, Resolved::Bool(false)),
        "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" if v2 => {
            (BOOL_TAG, Resolved::Bool(true))
        }
        "n" | "N" | "no" | "No" | "NO" | "off" | "Off" | "OFF" if v2 => {
            (BOOL_TAG, Resolved::Bool(false))
        }
        "" | "~" | "null" | "Null" | "NULL" => (NULL_TAG, Resolved::Null),
        ".nan" | ".NaN" | ".NAN" => (FLOAT_TAG, Resolved::Float(f64::NAN)),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            (FLOAT_TAG, Resolved::Float(f64::INFINITY))
        }
        "-.inf" | "-.Inf" | "-.INF" => (FLOAT_TAG, Resolved::Float(f64::NEG_INFINITY)),
        "<<" => (MERGE_TAG, Resolved::Merge),
        _ => return None,
    };
    Some(r)
}

/// `^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$`
pub(crate) fn yaml_style_float(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let st = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - st
    };
    if i < b.len() && b[i] == b'.' {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    } else {
        if digits(&mut i) == 0 {
            return false;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            digits(&mut i);
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

/// `^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+(?:\.[0-9_]*)?$`
pub(crate) fn is_base60_float(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() {
        return false;
    }
    let c = b[0];
    if !(c == b'+' || c == b'-' || c.is_ascii_digit()) || !b.contains(&b':') {
        return false;
    }
    let mut i = 0;
    if b[i] == b'+' || b[i] == b'-' {
        i += 1;
    }
    if i >= b.len() || !b[i].is_ascii_digit() {
        return false;
    }
    i += 1;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    // (?::[0-5]?[0-9])+ -- try all ways greedily with backtracking on the
    // optional digit.
    fn groups(b: &[u8], i: usize) -> bool {
        // need at least one group starting at i
        if i >= b.len() || b[i] != b':' {
            return false;
        }
        let j = i + 1;
        let mut cands = Vec::new();
        if j + 1 < b.len() && (b'0'..=b'5').contains(&b[j]) && b[j + 1].is_ascii_digit() {
            cands.push(j + 2);
        }
        if j < b.len() && b[j].is_ascii_digit() {
            cands.push(j + 1);
        }
        for k in cands {
            if rest(b, k) || groups(b, k) {
                return true;
            }
        }
        false
    }
    fn rest(b: &[u8], i: usize) -> bool {
        if i == b.len() {
            return true;
        }
        if b[i] != b'.' {
            return false;
        }
        b[i + 1..].iter().all(|c| c.is_ascii_digit() || *c == b'_')
    }
    groups(b, i)
}

/// v3's `isOldBool`.
pub(crate) fn is_old_bool(s: &str) -> bool {
    matches!(
        s,
        "y" | "Y"
            | "yes"
            | "Yes"
            | "YES"
            | "on"
            | "On"
            | "ON"
            | "n"
            | "N"
            | "no"
            | "No"
            | "NO"
            | "off"
            | "Off"
            | "OFF"
    )
}

pub(crate) fn resolvable_tag(tag: &str) -> bool {
    matches!(
        tag,
        "" | STR_TAG | BOOL_TAG | INT_TAG | FLOAT_TAG | NULL_TAG | TIMESTAMP_TAG
    )
}

/// Go's `resolve(tag, in)`. `v2` selects the YAML 1.1 boolean table (v2)
/// vs. the v3 one (v3 also understands `0o` octal). Returns the resolved
/// (long) tag and value, or the `failf` message (without the `yaml: `
/// prefix).
pub(crate) fn resolve(tag: &str, in_: &str, v2: bool) -> Result<(&'static str, Resolved), String> {
    if !resolvable_tag(tag) {
        // Callers handle unresolvable tags themselves (value is the input).
        return Ok(("", Resolved::Str));
    }
    let (rtag, out) = resolve_inner(tag, in_, v2);
    // The deferred check.
    if tag.is_empty() || tag == rtag || tag == STR_TAG || tag == BINARY_TAG {
        return Ok((rtag, out));
    }
    if tag == FLOAT_TAG
        && rtag == INT_TAG
        && let Resolved::Int(i) = out
    {
        return Ok((FLOAT_TAG, Resolved::Float(i as f64)));
    }
    Err(format!(
        "cannot decode {} `{}` as a {}",
        short_tag(rtag),
        in_,
        short_tag(tag)
    ))
}

fn int_result(i: i64) -> (&'static str, Resolved) {
    (INT_TAG, Resolved::Int(i))
}

fn resolve_inner(tag: &str, in_: &str, v2: bool) -> (&'static str, Resolved) {
    let h = hint(in_);
    if h != 0 && tag != STR_TAG && tag != BINARY_TAG {
        if let Some(r) = resolve_map(in_, v2) {
            return r;
        }
        match h {
            b'M' => {}
            b'.' => {
                if let Ok(f) = parse_float(in_) {
                    return (FLOAT_TAG, Resolved::Float(f));
                }
            }
            b'D' | b'S' => {
                if (tag.is_empty() || tag == TIMESTAMP_TAG) && parse_timestamp(in_) {
                    return (TIMESTAMP_TAG, Resolved::Timestamp);
                }
                let plain: String = in_.chars().filter(|&c| c != '_').collect();
                if let Ok(i) = parse_int(&plain, 0) {
                    return int_result(i);
                }
                if let Ok(u) = parse_uint(&plain, 0) {
                    return (INT_TAG, Resolved::Uint(u));
                }
                if yaml_style_float(&plain)
                    && let Ok(f) = parse_float(&plain)
                {
                    return (FLOAT_TAG, Resolved::Float(f));
                }
                if let Some(rest) = plain.strip_prefix("0b") {
                    if let Ok(i) = parse_int(rest, 2) {
                        return int_result(i);
                    }
                    if let Ok(u) = parse_uint(rest, 2) {
                        return (INT_TAG, Resolved::Uint(u));
                    }
                } else if let Some(rest) = plain.strip_prefix("-0b")
                    && let Ok(i) = parse_int(&format!("-{rest}"), 2)
                {
                    return int_result(i);
                }
                if !v2 {
                    if let Some(rest) = plain.strip_prefix("0o") {
                        if let Ok(i) = parse_int(rest, 8) {
                            return int_result(i);
                        }
                        if let Ok(u) = parse_uint(rest, 8) {
                            return (INT_TAG, Resolved::Uint(u));
                        }
                    } else if let Some(rest) = plain.strip_prefix("-0o")
                        && let Ok(i) = parse_int(&format!("-{rest}"), 8)
                    {
                        return int_result(i);
                    }
                }
            }
            _ => {}
        }
    }
    (STR_TAG, Resolved::Str)
}

// ---------------------------------------------------------------------------
// time.Parse for the four layouts goyaml accepts.

fn days_in(month: u32, year: i64) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Go's `getnum(s, fixed)`.
fn getnum(s: &[u8], fixed: bool) -> Option<(u32, &[u8])> {
    let d0 = *s.first()?;
    if !d0.is_ascii_digit() {
        return None;
    }
    match s.get(1) {
        Some(d1) if d1.is_ascii_digit() => {
            Some(((d0 - b'0') as u32 * 10 + (d1 - b'0') as u32, &s[2..]))
        }
        _ => {
            if fixed {
                None
            } else {
                Some(((d0 - b'0') as u32, &s[1..]))
            }
        }
    }
}

fn cutspace(mut s: &[u8]) -> &[u8] {
    while let Some(b' ') = s.first() {
        s = &s[1..];
    }
    s
}

#[derive(Clone, Copy)]
enum Layout {
    T,
    LowerT,
    Space,
    DateOnly,
}

fn parse_layout(s: &[u8], layout: Layout) -> bool {
    // "2006"
    if s.len() < 4 || !s[..4].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let year: i64 = s[..4].iter().fold(0i64, |a, c| a * 10 + (c - b'0') as i64);
    let mut v = &s[4..];
    // "-"
    if v.first() != Some(&b'-') {
        return false;
    }
    v = &v[1..];
    let Some((month, r)) = getnum(v, false) else {
        return false;
    };
    v = r;
    if month == 0 || month > 12 {
        return false;
    }
    if v.first() != Some(&b'-') {
        return false;
    }
    v = &v[1..];
    let Some((day, r)) = getnum(v, false) else {
        return false;
    };
    v = r;
    match layout {
        Layout::DateOnly => {}
        Layout::T | Layout::LowerT => {
            let sep = if matches!(layout, Layout::T) {
                b'T'
            } else {
                b't'
            };
            if v.first() != Some(&sep) {
                return false;
            }
            v = &v[1..];
        }
        Layout::Space => {
            if !v.is_empty() && v[0] != b' ' {
                return false;
            }
            v = cutspace(v);
        }
    }
    if !matches!(layout, Layout::DateOnly) {
        let Some((hour, r)) = getnum(v, false) else {
            return false;
        };
        v = r;
        if hour >= 24 {
            return false;
        }
        if v.first() != Some(&b':') {
            return false;
        }
        v = &v[1..];
        let Some((min, r)) = getnum(v, false) else {
            return false;
        };
        v = r;
        if min >= 60 {
            return false;
        }
        if v.first() != Some(&b':') {
            return false;
        }
        v = &v[1..];
        let Some((sec, r)) = getnum(v, false) else {
            return false;
        };
        v = r;
        if sec >= 60 {
            return false;
        }
        // .999999999 (optional)
        if v.len() >= 2 && (v[0] == b'.' || v[0] == b',') && v[1].is_ascii_digit() {
            let mut i = 0;
            while i + 1 < v.len() && v[i + 1].is_ascii_digit() {
                i += 1;
            }
            v = &v[1 + i..];
        }
        if matches!(layout, Layout::T | Layout::LowerT) {
            // Z07:00
            if v.first() == Some(&b'Z') {
                v = &v[1..];
            } else {
                if v.len() < 6 || v[3] != b':' {
                    return false;
                }
                let (sign, hh, mm) = (v[0], &v[1..3], &v[4..6]);
                let Some((hr, _)) = getnum(hh, true) else {
                    return false;
                };
                let Some((mn, _)) = getnum(mm, true) else {
                    return false;
                };
                if hr > 24 || mn > 60 {
                    return false;
                }
                if sign != b'+' && sign != b'-' {
                    return false;
                }
                v = &v[6..];
            }
        }
    }
    if !v.is_empty() {
        return false;
    }
    day >= 1 && day <= days_in(month, year)
}

/// goyaml's `parseTimestamp`.
pub(crate) fn parse_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i != 4 || i == b.len() || b[i] != b'-' {
        return false;
    }
    [Layout::T, Layout::LowerT, Layout::Space, Layout::DateOnly]
        .iter()
        .any(|l| parse_layout(b, *l))
}

/// Go's `base64.StdEncoding.DecodeString` (newlines and carriage returns are
/// ignored).
pub(crate) fn base64_decode(s: &[u8]) -> Option<Vec<u8>> {
    let clean: Vec<u8> = s
        .iter()
        .copied()
        .filter(|&c| c != b'\n' && c != b'\r')
        .collect();
    if !clean.len().is_multiple_of(4) {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    let n = clean.len() / 4;
    for (q, chunk) in clean.chunks(4).enumerate() {
        let last = q + 1 == n;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut acc = 0u32;
        for &c in &chunk[..4 - pad] {
            acc = (acc << 6) | val(c)?;
        }
        match pad {
            0 => {
                out.push((acc >> 16) as u8);
                out.push((acc >> 8) as u8);
                out.push(acc as u8);
            }
            1 => {
                acc <<= 6;
                out.push((acc >> 16) as u8);
                out.push((acc >> 8) as u8);
            }
            _ => {
                acc <<= 12;
                out.push((acc >> 16) as u8);
            }
        }
    }
    Some(out)
}

/// `base64.StdEncoding.EncodeToString`.
pub(crate) fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert!(parse_timestamp("2001-12-14"));
        assert!(parse_timestamp("2001-1-4"));
        assert!(parse_timestamp("2001-12-14T21:59:43.10Z"));
        assert!(parse_timestamp("2001-12-14t21:59:43.10-05:00"));
        assert!(parse_timestamp("2001-12-14 21:59:43.10"));
        assert!(!parse_timestamp("2001-12-14 21:59:43.10 -5"));
        assert!(!parse_timestamp("2001-02-30"));
        assert!(parse_timestamp("2000-02-29"));
        assert!(!parse_timestamp("1900-02-29"));
    }

    #[test]
    fn base60() {
        assert!(is_base60_float("1:20"));
        assert!(is_base60_float("190:20:30.15"));
        assert!(!is_base60_float("1:2:"));
        assert!(!is_base60_float("1:60"));
        assert!(is_base60_float("1:59"));
        assert!(!is_base60_float("1:600"));
    }

    #[test]
    fn resolves() {
        assert_eq!(resolve("", "0x10", true).unwrap().1, Resolved::Int(16));
        assert_eq!(resolve("", "08", true).unwrap().1, Resolved::Float(8.0));
        assert_eq!(resolve("", "1_000", true).unwrap().1, Resolved::Int(1000));
        assert_eq!(resolve("", "yes", true).unwrap().1, Resolved::Bool(true));
        assert_eq!(resolve("", "yes", false).unwrap().1, Resolved::Str);
        assert_eq!(resolve("", "0o17", true).unwrap().1, Resolved::Int(15));
        assert_eq!(resolve("", ".5", true).unwrap().1, Resolved::Float(0.5));
        assert!(resolve(INT_TAG, "abc", true).is_err());
    }
}
