//! Go `strconv.Quote` / `strconv.Unquote` / `strconv.UnquoteChar`.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use super::unicode::is_print;

/// `strconv.Quote(s)`.
pub fn go_quote(s: &str) -> String {
    quote_with(s, '"', false)
}

/// `strconv.QuoteToASCII(s)`.
pub fn go_quote_ascii(s: &str) -> String {
    quote_with(s, '"', true)
}

fn quote_with(s: &str, quote: char, ascii_only: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        append_escaped_rune(&mut out, c, quote, ascii_only);
    }
    out.push(quote);
    out
}

/// Quotes bytes that may not be valid UTF-8 (invalid bytes become `\xNN`).
pub fn go_quote_bytes(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() + 2);
    out.push('"');
    let mut rest = b;
    while !rest.is_empty() {
        match core::str::from_utf8(rest) {
            Ok(s) => {
                for c in s.chars() {
                    append_escaped_rune(&mut out, c, '"', false);
                }
                break;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                for c in core::str::from_utf8(good).unwrap().chars() {
                    append_escaped_rune(&mut out, c, '"', false);
                }
                // Go emits one \x escape per invalid byte.
                let _ = write!(out, "\\x{:02x}", bad[0]);
                rest = &bad[1..];
            }
        }
    }
    out.push('"');
    out
}

/// `strconv.QuoteRune(r)`.
pub fn go_quote_rune(c: char) -> String {
    let mut out = String::new();
    out.push('\'');
    append_escaped_rune(&mut out, c, '\'', false);
    out.push('\'');
    out
}

fn append_escaped_rune(out: &mut String, c: char, quote: char, ascii_only: bool) {
    if c == quote || c == '\\' {
        out.push('\\');
        out.push(c);
        return;
    }
    if ascii_only {
        if (c as u32) < 0x80 && is_print(c) {
            out.push(c);
            return;
        }
    } else if is_print(c) {
        out.push(c);
        return;
    }
    match c {
        '\u{7}' => out.push_str("\\a"),
        '\u{8}' => out.push_str("\\b"),
        '\u{c}' => out.push_str("\\f"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{b}' => out.push_str("\\v"),
        _ => {
            let r = c as u32;
            if r < ' ' as u32 || r == 0x7f {
                let _ = write!(out, "\\x{r:02x}");
            } else if r < 0x10000 {
                let _ = write!(out, "\\u{r:04x}");
            } else {
                let _ = write!(out, "\\U{r:08x}");
            }
        }
    }
}

/// Error from unquoting; Go reports `invalid syntax`.
pub const ERR_SYNTAX: &str = "invalid syntax";

/// `strconv.UnquoteChar(s, quote)`: decodes the first character or escape
/// sequence. Returns the value (a char, or a raw byte for `\x`/octal escapes
/// in strings), and the remaining input.
pub fn unquote_char(s: &str, quote: u8) -> Result<(UnquotedChar, &str), &'static str> {
    let bytes = s.as_bytes();
    let Some(&c) = bytes.first() else { return Err(ERR_SYNTAX) };
    if c == quote && (quote == b'\'' || quote == b'"') {
        return Err(ERR_SYNTAX);
    }
    if c >= 0x80 {
        let ch = s.chars().next().unwrap();
        return Ok((UnquotedChar::Char(ch), &s[ch.len_utf8()..]));
    }
    if c != b'\\' {
        return Ok((UnquotedChar::Char(c as char), &s[1..]));
    }
    if bytes.len() <= 1 {
        return Err(ERR_SYNTAX);
    }
    let c = bytes[1];
    let mut rest = &s[2..];
    let v = match c {
        b'a' => UnquotedChar::Char('\u{7}'),
        b'b' => UnquotedChar::Char('\u{8}'),
        b'f' => UnquotedChar::Char('\u{c}'),
        b'n' => UnquotedChar::Char('\n'),
        b'r' => UnquotedChar::Char('\r'),
        b't' => UnquotedChar::Char('\t'),
        b'v' => UnquotedChar::Char('\u{b}'),
        b'x' | b'u' | b'U' => {
            let n = match c {
                b'x' => 2,
                b'u' => 4,
                _ => 8,
            };
            if rest.len() < n {
                return Err(ERR_SYNTAX);
            }
            let mut v: u32 = 0;
            for &d in &rest.as_bytes()[..n] {
                let x = (d as char).to_digit(16).ok_or(ERR_SYNTAX)?;
                v = (v << 4) | x;
            }
            rest = &rest[n..];
            if c == b'x' {
                UnquotedChar::Byte(v as u8)
            } else {
                match char::from_u32(v) {
                    Some(ch) => UnquotedChar::Char(ch),
                    None => return Err(ERR_SYNTAX),
                }
            }
        }
        b'0'..=b'7' => {
            let mut v = (c - b'0') as u32;
            if rest.len() < 2 {
                return Err(ERR_SYNTAX);
            }
            for &d in &rest.as_bytes()[..2] {
                if !(b'0'..=b'7').contains(&d) {
                    return Err(ERR_SYNTAX);
                }
                v = (v << 3) | (d - b'0') as u32;
            }
            rest = &rest[2..];
            if v > 255 {
                return Err(ERR_SYNTAX);
            }
            UnquotedChar::Byte(v as u8)
        }
        b'\\' => UnquotedChar::Char('\\'),
        b'\'' | b'"' => {
            if c != quote {
                return Err(ERR_SYNTAX);
            }
            UnquotedChar::Char(c as char)
        }
        _ => return Err(ERR_SYNTAX),
    };
    Ok((v, rest))
}

/// A decoded character or a raw byte (from `\xNN` / octal escapes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnquotedChar {
    Char(char),
    Byte(u8),
}

/// `strconv.Unquote(s)` for `"..."`, `` `...` `` and `'...'` literals. Byte
/// escapes that do not form valid UTF-8 are replaced with U+FFFD.
pub fn unquote(s: &str) -> Result<String, &'static str> {
    let b = s.as_bytes();
    if b.len() < 2 {
        return Err(ERR_SYNTAX);
    }
    let quote = b[0];
    if quote != b[b.len() - 1] {
        return Err(ERR_SYNTAX);
    }
    let inner = &s[1..s.len() - 1];
    match quote {
        b'`' => {
            if inner.contains('`') {
                return Err(ERR_SYNTAX);
            }
            Ok(inner.replace('\r', ""))
        }
        b'"' | b'\'' => {
            if inner.contains('\n') {
                return Err(ERR_SYNTAX);
            }
            let mut buf: Vec<u8> = Vec::with_capacity(inner.len());
            let mut rest = inner;
            let mut count = 0;
            while !rest.is_empty() {
                let (c, r) = unquote_char(rest, quote)?;
                rest = r;
                count += 1;
                match c {
                    UnquotedChar::Char(ch) => {
                        let mut tmp = [0u8; 4];
                        buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                    }
                    UnquotedChar::Byte(byte) => {
                        if quote == b'\'' {
                            // In a rune literal, \xNN is a code point.
                            let mut tmp = [0u8; 4];
                            buf.extend_from_slice((byte as char).encode_utf8(&mut tmp).as_bytes());
                        } else {
                            buf.push(byte);
                        }
                    }
                }
                if quote == b'\'' && count > 1 {
                    return Err(ERR_SYNTAX);
                }
            }
            if quote == b'\'' && count != 1 {
                return Err(ERR_SYNTAX);
            }
            Ok(match String::from_utf8(buf) {
                Ok(s) => s,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            })
        }
        _ => Err(ERR_SYNTAX),
    }
}
