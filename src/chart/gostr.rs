//! Small ports of Go standard library string/path helpers used by the chart
//! code (`strconv.Quote`, `path.Clean`, `filepath.Base`, `unicode.IsPrint`).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// Approximation of Go's `unicode.IsPrint`: letters, marks, numbers,
/// punctuation, symbols and the ASCII space. Without Unicode category tables
/// we exclude control, format, private-use, surrogate and other whitespace
/// characters, which covers what occurs in practice.
pub(crate) fn is_print(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    let u = c as u32;
    !matches!(u,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF
        | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F
        | 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
        | 0xFDD0..=0xFDEF | 0xFFFE | 0xFFFF)
}

/// Go's `unicode.IsSpace`.
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace()
}

/// Go's `strconv.Quote` (`%q` of a string).
pub(crate) fn quote(s: &str) -> String {
    quote_bytes(s.as_bytes())
}

/// `strconv.Quote` applied to possibly invalid UTF-8.
pub(crate) fn quote_bytes(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() + 2);
    out.push('"');
    let mut i = 0;
    while i < b.len() {
        let (c, w) = decode_rune(&b[i..]);
        if c.is_none() {
            let _ = write!(out, "\\x{:02x}", b[i]);
            i += 1;
            continue;
        }
        let c = c.unwrap_or('\u{FFFD}');
        i += w;
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x0B' => out.push_str("\\v"),
            c if is_print(c) => out.push(c),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if (c as u32) < 0x10000 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => {
                let _ = write!(out, "\\U{:08x}", c as u32);
            }
        }
    }
    out.push('"');
    out
}

/// Decodes one UTF-8 rune; `None` for an invalid encoding (width 1).
pub(crate) fn decode_rune(b: &[u8]) -> (Option<char>, usize) {
    if b.is_empty() {
        return (None, 0);
    }
    let w = match b[0] {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return (None, 1),
    };
    if b.len() < w {
        return (None, 1);
    }
    match core::str::from_utf8(&b[..w]) {
        Ok(s) => (s.chars().next(), w),
        Err(_) => (None, 1),
    }
}

/// Go's `path.Clean`.
pub(crate) fn clean(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let rooted = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if !rooted {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let mut out = String::new();
    if rooted {
        out.push('/');
    }
    out.push_str(&parts.join("/"));
    if out.is_empty() {
        return ".".into();
    }
    out
}

/// Go's `path.Join` (empty elements ignored, result cleaned).
pub(crate) fn join(parts: &[&str]) -> String {
    let nonempty: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
    if nonempty.is_empty() {
        return String::new();
    }
    clean(&nonempty.join("/"))
}

/// Go's `path.Base` / `filepath.Base` (Unix).
pub(crate) fn base(p: &str) -> &str {
    if p.is_empty() {
        return ".";
    }
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return "/";
    }
    match t.rfind('/') {
        Some(i) => &t[i + 1..],
        None => t,
    }
}

/// Go's `path.Ext` / `filepath.Ext`.
pub(crate) fn ext(p: &str) -> &str {
    let b = p.as_bytes();
    let mut i = b.len();
    while i > 0 && b[i - 1] != b'/' {
        i -= 1;
        if b[i] == b'.' {
            return &p[i..];
        }
    }
    ""
}

/// `strconv.FormatFloat(f, 'g', -1, 32)`: how `sigs.k8s.io/yaml` turns a YAML
/// float into a string when the target field is a string.
pub(crate) fn format_float32_g(f: f64) -> String {
    let x = f as f32;
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    // Shortest round-trip digits via Rust's `{:e}`, then Go's %g layout.
    let e = alloc::format!("{:e}", x);
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if !(-4..6).contains(&exp) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        let a = exp.unsigned_abs();
        if a < 10 {
            out.push('0');
        }
        let _ = write!(out, "{a}");
    } else if exp < 0 {
        out.push_str("0.");
        for _ in 0..(-exp - 1) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let point = exp as usize + 1;
        if digits.len() <= point {
            out.push_str(&digits);
            for _ in digits.len()..point {
                out.push('0');
            }
        } else {
            out.push_str(&digits[..point]);
            out.push('.');
            out.push_str(&digits[point..]);
        }
    }
    out
}

/// How `sigs.k8s.io/yaml` stringifies a YAML number for a string target.
/// YAML integers become `strconv.FormatInt`, floats `FormatFloat(.., 'g', -1, 32)`.
/// Our parsed values are all `float64`, so integral values are treated as
/// YAML integers (the common case: `version: 1`).
pub(crate) fn yaml_number_to_string(f: f64) -> String {
    if f.is_finite() && f.abs() < 9.2e18 && f == (f as i64) as f64 {
        return alloc::format!("{}", f as i64);
    }
    format_float32_g(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float32_g() {
        assert_eq!(format_float32_g(1.1), "1.1");
        assert_eq!(format_float32_g(1.10), "1.1");
        assert_eq!(format_float32_g(1.23456789), "1.2345679");
        assert_eq!(format_float32_g(1e6), "1e+06");
        assert_eq!(format_float32_g(123456.0), "123456");
        assert_eq!(format_float32_g(0.0001), "0.0001");
        assert_eq!(format_float32_g(0.00001), "1e-05");
        assert_eq!(format_float32_g(-2.5), "-2.5");
        assert_eq!(yaml_number_to_string(1000.0), "1000");
        assert_eq!(yaml_number_to_string(12345678901.0), "12345678901");
    }

    #[test]
    fn paths() {
        assert_eq!(clean("a/../b/./c/"), "b/c");
        assert_eq!(clean("../a"), "../a");
        assert_eq!(clean(""), ".");
        assert_eq!(clean("/../a"), "/a");
        assert_eq!(base("a/b/"), "b");
        assert_eq!(base(""), ".");
        assert_eq!(base("/"), "/");
        assert_eq!(ext("a/b.tar.gz"), ".gz");
        assert_eq!(ext("a.b/c"), "");
        assert_eq!(join(&["a", "", "b/../c"]), "a/c");
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("a\"b\n\x01é"), "\"a\\\"b\\n\\x01é\"");
        assert_eq!(quote_bytes(b"\xff"), "\"\\xff\"");
    }
}
