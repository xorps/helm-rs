//! String functions (`strings.go` and the goutils/xstrings helpers).

#![allow(
    clippy::needless_range_loop,
    clippy::collapsible_match,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use sha1::Digest as _;

use super::gounicode::{self as gu, is_space};
use super::{MAX_OUTPUT, arg, iarg, sarg, str_list, strval, too_large};
use crate::func::{CallCtx, FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::gofmt::strconv;
use crate::value::{Map, Value};

const S: Ty = Ty::String;
const I: Ty = Ty::Int;
const A: Ty = Ty::Any;

pub(super) fn register(f: &mut FuncMap) {
    f.add("hello", Sig::new(&[]), |_| Ok(Value::str("Hello!")));
    f.add("abbrev", Sig::new(&[I, S]), |a| {
        Ok(Value::string(abbrev(iarg(a, 0), sarg(a, 1))))
    });
    f.add("abbrevboth", Sig::new(&[I, I, S]), |a| {
        Ok(Value::string(abbrevboth(
            iarg(a, 0),
            iarg(a, 1),
            sarg(a, 2),
        )))
    });
    f.add("trunc", Sig::new(&[I, S]), |a| {
        Ok(Value::string(trunc(iarg(a, 0), sarg(a, 1))))
    });
    f.add("trim", Sig::new(&[S]), |a| {
        Ok(Value::str(trim_space(sarg(a, 0))))
    });
    f.add("upper", Sig::new(&[S]), |a| {
        Ok(Value::string(
            sarg(a, 0).chars().map(gu::to_upper).collect(),
        ))
    });
    f.add("lower", Sig::new(&[S]), |a| {
        Ok(Value::string(
            sarg(a, 0).chars().map(gu::to_lower).collect(),
        ))
    });
    f.add("title", Sig::new(&[S]), |a| {
        Ok(Value::string(title(sarg(a, 0))))
    });
    f.add("untitle", Sig::new(&[S]), |a| {
        Ok(Value::string(uncapitalize(sarg(a, 0))))
    });
    f.add("substr", Sig::new(&[I, I, S]), |a| {
        // Byte slicing can split a UTF-8 sequence; Go keeps the raw bytes.
        substring(iarg(a, 0), iarg(a, 1), sarg(a, 2)).map(|b| Value::go_string(&b))
    });
    f.add("repeat", Sig::new(&[I, S]), |a| {
        repeat(sarg(a, 1), iarg(a, 0)).map(Value::string)
    });
    f.add("trimall", Sig::new(&[S, S]), |a| {
        Ok(Value::str(trim_cutset(sarg(a, 1), sarg(a, 0))))
    });
    f.add("trimAll", Sig::new(&[S, S]), |a| {
        Ok(Value::str(trim_cutset(sarg(a, 1), sarg(a, 0))))
    });
    f.add("trimSuffix", Sig::new(&[S, S]), |a| {
        let (suf, s) = (sarg(a, 0), sarg(a, 1));
        Ok(Value::str(s.strip_suffix(suf).unwrap_or(s)))
    });
    f.add("trimPrefix", Sig::new(&[S, S]), |a| {
        let (pre, s) = (sarg(a, 0), sarg(a, 1));
        Ok(Value::str(s.strip_prefix(pre).unwrap_or(s)))
    });
    f.add("nospace", Sig::new(&[S]), |a| {
        Ok(Value::string(delete_white_space(sarg(a, 0))))
    });
    f.add("initials", Sig::new(&[S]), |a| {
        Ok(Value::string(initials(sarg(a, 0))))
    });
    f.add_ctx("randAlphaNum", Sig::new(&[I]), |c, a| {
        crypto_random(c, iarg(a, 0), 0, 0, true, true)
    });
    f.add_ctx("randAlpha", Sig::new(&[I]), |c, a| {
        crypto_random(c, iarg(a, 0), 0, 0, true, false)
    });
    f.add_ctx("randAscii", Sig::new(&[I]), |c, a| {
        crypto_random(c, iarg(a, 0), 32, 127, false, false)
    });
    f.add_ctx("randNumeric", Sig::new(&[I]), |c, a| {
        crypto_random(c, iarg(a, 0), 0, 0, false, true)
    });
    f.add("swapcase", Sig::new(&[S]), |a| {
        Ok(Value::string(swap_case(sarg(a, 0))))
    });
    f.add_ctx("shuffle", Sig::new(&[S]), shuffle);
    f.add("snakecase", Sig::new(&[S]), |a| {
        Ok(Value::string(camel_case_to_lower_case(sarg(a, 0), '_')))
    });
    f.add("camelcase", Sig::new(&[S]), |a| {
        Ok(Value::string(to_pascal_case(sarg(a, 0))))
    });
    f.add("kebabcase", Sig::new(&[S]), |a| {
        Ok(Value::string(camel_case_to_lower_case(sarg(a, 0), '-')))
    });
    f.add("wrap", Sig::new(&[I, S]), |a| {
        Ok(Value::string(wrap_custom(
            sarg(a, 1),
            iarg(a, 0),
            "",
            false,
        )))
    });
    f.add("wrapWith", Sig::new(&[I, S, S]), |a| {
        Ok(Value::string(wrap_custom(
            sarg(a, 2),
            iarg(a, 0),
            sarg(a, 1),
            true,
        )))
    });
    f.add("contains", Sig::new(&[S, S]), |a| {
        Ok(Value::Bool(sarg(a, 1).contains(sarg(a, 0))))
    });
    f.add("hasPrefix", Sig::new(&[S, S]), |a| {
        Ok(Value::Bool(sarg(a, 1).starts_with(sarg(a, 0))))
    });
    f.add("hasSuffix", Sig::new(&[S, S]), |a| {
        Ok(Value::Bool(sarg(a, 1).ends_with(sarg(a, 0))))
    });
    f.add("quote", Sig::variadic(&[], A), |a| {
        Ok(Value::string(quote(a)))
    });
    f.add("squote", Sig::variadic(&[], A), |a| {
        Ok(Value::string(squote(a)))
    });
    f.add("cat", Sig::variadic(&[], A), |a| {
        let parts: Vec<String> = a
            .iter()
            .filter(|v| !v.is_nil())
            .map(crate::gofmt::format_value)
            .collect();
        Ok(Value::string(parts.join(" ")))
    });
    f.add("indent", Sig::new(&[I, S]), |a| {
        indent(iarg(a, 0), sarg(a, 1)).map(Value::string)
    });
    f.add("nindent", Sig::new(&[I, S]), |a| {
        indent(iarg(a, 0), sarg(a, 1)).map(|s| Value::string(format!("\n{s}")))
    });
    f.add("replace", Sig::new(&[S, S, S]), |a| {
        replace(sarg(a, 2), sarg(a, 0), sarg(a, 1)).map(Value::string)
    });
    f.add("plural", Sig::new(&[S, S, I]), |a| {
        Ok(Value::str(if iarg(a, 2) == 1 {
            sarg(a, 0)
        } else {
            sarg(a, 1)
        }))
    });
    f.add("sha1sum", Sig::new(&[S]), |a| {
        Ok(Value::string(hex(&sha1::Sha1::digest(sbytes(a, 0)))))
    });
    f.add("sha256sum", Sig::new(&[S]), |a| {
        Ok(Value::string(hex(&sha2::Sha256::digest(sbytes(a, 0)))))
    });
    f.add("sha512sum", Sig::new(&[S]), |a| {
        Ok(Value::string(hex(&sha2::Sha512::digest(sbytes(a, 0)))))
    });
    f.add("adler32sum", Sig::new(&[S]), |a| {
        Ok(Value::string(adler32(&sbytes(a, 0)).to_string()))
    });
    f.add("toString", Sig::new(&[A]), |a| {
        Ok(Value::string(strval(arg(a, 0))))
    });
    f.add("split", Sig::new(&[S, S]), |a| {
        Ok(parts_map(go_split(sarg(a, 1), sarg(a, 0), -1)))
    });
    f.add("splitList", Sig::new(&[S, S]), |a| {
        Ok(str_list(go_split(sarg(a, 1), sarg(a, 0), -1)))
    });
    f.add("splitn", Sig::new(&[S, I, S]), |a| {
        Ok(parts_map(go_split(sarg(a, 2), sarg(a, 0), iarg(a, 1))))
    });
    f.add("toStrings", Sig::new(&[A]), |a| {
        Ok(str_list(strslice(arg(a, 0))))
    });
    f.add("join", Sig::new(&[S, A]), |a| {
        Ok(Value::string(strslice(arg(a, 1)).join(sarg(a, 0))))
    });
    f.add("sortAlpha", Sig::new(&[A]), |a| {
        let v = arg(a, 0);
        let mut items = match v {
            Value::List(_) | Value::Bytes(_) => strslice(v),
            _ => alloc::vec![strval(v)],
        };
        if matches!(v, Value::List(_) | Value::Bytes(_)) {
            items.sort();
        }
        Ok(str_list(items))
    });
    f.add("b64enc", Sig::new(&[S]), |a| {
        Ok(Value::string(base64_encode(&sbytes(a, 0))))
    });
    f.add("b64dec", Sig::new(&[S]), |a| {
        Ok(Value::string(match base64_decode(sarg(a, 0).as_bytes()) {
            Ok(b) => strconv::lossy_vec(b),
            Err(e) => e,
        }))
    });
    f.add("b32enc", Sig::new(&[S]), |a| {
        Ok(Value::string(base32_encode(&sbytes(a, 0))))
    });
    f.add("b32dec", Sig::new(&[S]), |a| {
        Ok(Value::string(match base32_decode(sarg(a, 0).as_bytes()) {
            Ok(b) => strconv::lossy_vec(b),
            Err(e) => e,
        }))
    });
    f.add("env", Sig::new(&[S]), |_| Ok(Value::str("")));
    f.add("expandenv", Sig::new(&[S]), |a| {
        Ok(Value::string(expand_env(sarg(a, 0))))
    });
}

pub(crate) fn hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// `strings.TrimSpace`.
pub(crate) fn trim_space(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `strings.Trim(s, cutset)`.
fn trim_cutset<'a>(s: &'a str, cutset: &str) -> &'a str {
    if s.is_empty() || cutset.is_empty() {
        return s;
    }
    s.trim_matches(|c: char| cutset.contains(c))
}

/// `strings.Repeat`.
pub(crate) fn repeat(s: &str, count: i64) -> Result<String, FuncError> {
    if count < 0 {
        return Err(FuncError::new("strings: negative Repeat count"));
    }
    if s.is_empty() || count == 0 {
        return Ok(String::new());
    }
    let total = (s.len() as u128) * (count as u128);
    if total > i64::MAX as u128 {
        return Err(FuncError::new("strings: Repeat output length overflow"));
    }
    if total > MAX_OUTPUT as u128 {
        return Err(too_large("strings.Repeat"));
    }
    Ok(s.repeat(count as usize))
}

fn indent(spaces: i64, v: &str) -> Result<String, FuncError> {
    let pad = repeat(" ", spaces)?;
    let nl = v.matches('\n').count();
    if (pad.len() as u128) * (nl as u128 + 1) + v.len() as u128 > MAX_OUTPUT as u128 {
        return Err(too_large("indent"));
    }
    let mut out = pad.clone();
    out.push_str(&v.replace('\n', &format!("\n{pad}")));
    Ok(out)
}

fn replace(src: &str, old: &str, new: &str) -> Result<String, FuncError> {
    let n = if old.is_empty() {
        src.chars().count() + 1
    } else {
        src.matches(old).count()
    };
    if (n as u128) * (new.len() as u128) + src.len() as u128 > MAX_OUTPUT as u128 {
        return Err(too_large("strings.Replace"));
    }
    Ok(src.replace(old, new))
}

fn trunc(c: i64, s: &str) -> String {
    let b = s.as_bytes();
    let len = b.len() as i64;
    if c < 0 && len + c > 0 {
        return strconv::lossy(&b[(len + c) as usize..]);
    }
    if c >= 0 && len > c {
        return strconv::lossy(&b[..c as usize]);
    }
    s.to_string()
}

/// goutils `AbbreviateFull` (byte based). `Err` carries goutils' error.
fn abbreviate_full(s: &[u8], offset: i64, max_width: i64) -> Result<Vec<u8>, ()> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    if max_width < 4 {
        return Err(());
    }
    let len = s.len() as i64;
    if len <= max_width {
        return Ok(s.to_vec());
    }
    let mut offset = offset;
    if offset > len {
        offset = len;
    }
    if len.wrapping_sub(offset) < max_width.wrapping_sub(3) {
        offset = len.wrapping_sub(max_width.wrapping_sub(3));
    }
    let marker = b"...";
    if offset <= 4 {
        let mut out = s[..(max_width - 3) as usize].to_vec();
        out.extend_from_slice(marker);
        return Ok(out);
    }
    if max_width < 7 {
        return Err(());
    }
    if offset.wrapping_add(max_width).wrapping_sub(3) < len {
        let inner = abbreviate_full(&s[offset as usize..], 0, max_width - 3).unwrap_or_default();
        let mut out = marker.to_vec();
        out.extend_from_slice(&inner);
        return Ok(out);
    }
    let mut out = marker.to_vec();
    out.extend_from_slice(&s[(len - (max_width - 3)) as usize..]);
    Ok(out)
}

fn abbrev(width: i64, s: &str) -> String {
    if width < 4 {
        return s.to_string();
    }
    strconv::lossy_vec(abbreviate_full(s.as_bytes(), 0, width).unwrap_or_default())
}

fn abbrevboth(left: i64, right: i64, s: &str) -> String {
    if right < 4 || (left > 0 && right < 7) {
        return s.to_string();
    }
    strconv::lossy_vec(abbreviate_full(s.as_bytes(), left, right).unwrap_or_default())
}

fn slice_err_alen(x: i64, len: usize) -> FuncError {
    if x < 0 {
        FuncError::new(format!("runtime error: slice bounds out of range [:{x}]"))
    } else {
        FuncError::new(format!(
            "runtime error: slice bounds out of range [:{x}] with length {len}"
        ))
    }
}

fn slice_err_b(x: i64, y: i64) -> FuncError {
    if x < 0 {
        FuncError::new(format!("runtime error: slice bounds out of range [{x}:]"))
    } else {
        FuncError::new(format!(
            "runtime error: slice bounds out of range [{x}:{y}]"
        ))
    }
}

fn substring(start: i64, end: i64, s: &str) -> Result<alloc::vec::Vec<u8>, FuncError> {
    let b = s.as_bytes();
    let len = b.len() as i64;
    if start < 0 {
        if end < 0 || end > len {
            return Err(slice_err_alen(end, b.len()));
        }
        return Ok(b[..end as usize].to_vec());
    }
    if end < 0 || end > len {
        if start > len {
            return Err(slice_err_b(start, len));
        }
        return Ok(b[start as usize..].to_vec());
    }
    if start > end {
        return Err(slice_err_b(start, end));
    }
    Ok(b[start as usize..end as usize].to_vec())
}

/// `strings.Title`.
fn title(s: &str) -> String {
    let is_sep = |r: char| -> bool {
        if (r as u32) <= 0x7F {
            return !(r.is_ascii_alphanumeric() || r == '_');
        }
        if gu::is_letter(r) || gu::is_digit(r) {
            return false;
        }
        is_space(r)
    };
    let mut prev = ' ';
    s.chars()
        .map(|r| {
            let out = if is_sep(prev) { gu::to_title(r) } else { r };
            prev = r;
            out
        })
        .collect()
}

/// goutils `Uncapitalize` (whitespace delimiters).
fn uncapitalize(s: &str) -> String {
    let mut next = true;
    s.chars()
        .map(|c| {
            if is_space(c) {
                next = true;
                c
            } else if next {
                next = false;
                gu::to_lower(c)
            } else {
                c
            }
        })
        .collect()
}

/// goutils `SwapCase`.
fn swap_case(s: &str) -> String {
    let mut whitespace = true;
    s.chars()
        .map(|c| {
            if gu::is_upper(c) || gu::is_title(c) {
                whitespace = false;
                gu::to_lower(c)
            } else if gu::is_lower(c) {
                if whitespace {
                    whitespace = false;
                    gu::to_title(c)
                } else {
                    gu::to_upper(c)
                }
            } else {
                whitespace = is_space(c);
                c
            }
        })
        .collect()
}

/// goutils `DeleteWhiteSpace`: byte-wise, re-encoding bytes as runes.
fn delete_white_space(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut count = 0;
    for &c in b {
        let ch = gu::byte_rune(c);
        if !is_space(ch) {
            out.push(ch);
            count += 1;
        }
    }
    if count == b.len() {
        return s.to_string();
    }
    out
}

/// goutils `Initials`: byte-wise, re-encoding bytes as runes.
fn initials(s: &str) -> String {
    let mut out = String::new();
    let mut last_was_gap = true;
    for &c in s.as_bytes() {
        let ch = gu::byte_rune(c);
        if is_space(ch) {
            last_was_gap = true;
        } else if last_was_gap {
            out.push(ch);
            last_was_gap = false;
        }
    }
    out
}

/// goutils `WrapCustom` (byte based).
fn wrap_custom(s: &str, wrap_length: i64, new_line: &str, wrap_long_words: bool) -> String {
    if s.is_empty() {
        return String::new();
    }
    let new_line = if new_line.is_empty() { "\n" } else { new_line };
    let wrap_length = if wrap_length < 1 { 1 } else { wrap_length } as usize;
    let b = s.as_bytes();
    let input_len = b.len();
    let mut offset = 0usize;
    let mut out: Vec<u8> = Vec::new();
    while input_len - offset > wrap_length {
        if b[offset] == b' ' {
            offset += 1;
            continue;
        }
        let end = wrap_length + offset + 1;
        let space = b[offset..end].iter().rposition(|&c| c == b' ');
        if let Some(sp) = space {
            let sp = sp + offset;
            out.extend_from_slice(&b[offset..sp]);
            out.extend_from_slice(new_line.as_bytes());
            offset = sp + 1;
        } else if wrap_long_words {
            let end = wrap_length + offset;
            out.extend_from_slice(&b[offset..end]);
            out.extend_from_slice(new_line.as_bytes());
            offset += wrap_length;
        } else {
            let end = wrap_length + offset;
            match b[end..].iter().position(|&c| c == b' ') {
                None => {
                    out.extend_from_slice(&b[offset..]);
                    offset = input_len;
                }
                Some(i) => {
                    let sp = i + end;
                    out.extend_from_slice(&b[offset..sp]);
                    out.extend_from_slice(new_line.as_bytes());
                    offset = sp + 1;
                }
            }
        }
    }
    out.extend_from_slice(&b[offset..]);
    strconv::lossy_vec(out)
}

fn quote(args: &[Value]) -> String {
    let mut out: Vec<String> = Vec::new();
    for v in args {
        if v.is_nil() {
            continue;
        }
        let bytes: Vec<u8> = match v {
            Value::Bytes(b) => b.to_vec(),
            _ => strval(v).into_bytes(),
        };
        let mut q = Vec::new();
        strconv::append_quoted(&mut q, &bytes, b'"', false);
        out.push(strconv::lossy_vec(q));
    }
    out.join(" ")
}

fn squote(args: &[Value]) -> String {
    let parts: Vec<String> = args
        .iter()
        .filter(|v| !v.is_nil())
        .map(|v| format!("'{}'", crate::gofmt::format_value(v)))
        .collect();
    parts.join(" ")
}

/// Go's `strings.genSplit(s, sep, 0, n)` (`Split`/`SplitN`).
pub(crate) fn go_split(s: &str, sep: &str, n: i64) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    if sep.is_empty() {
        // explode
        let l = s.chars().count() as i64;
        let n = if n < 0 || n > l { l } else { n };
        let mut out = Vec::new();
        let mut rest = s;
        for _ in 0..n.saturating_sub(1) {
            let c = rest.chars().next().unwrap_or('\u{FFFD}');
            out.push(c.to_string());
            rest = &rest[c.len_utf8()..];
        }
        if n > 0 {
            out.push(rest.to_string());
        }
        return out;
    }
    if n < 0 {
        return s.split(sep).map(String::from).collect();
    }
    s.splitn(n as usize, sep).map(String::from).collect()
}

fn parts_map(parts: Vec<String>) -> Value {
    let mut m = Map::new();
    for (i, p) in parts.into_iter().enumerate() {
        m.insert(format!("_{i}"), Value::string(p));
    }
    // Go: map[string]string
    Value::string_map(m)
}

/// Sprig's `strslice`.
pub(crate) fn strslice(v: &Value) -> Vec<String> {
    match v {
        Value::List(l) => l.iter().filter(|x| !x.is_nil()).map(strval).collect(),
        Value::Bytes(b) => b.iter().map(|x| x.to_string()).collect(),
        Value::Nil => Vec::new(),
        _ => alloc::vec![strval(v)],
    }
}

fn crypto_random(
    ctx: &mut CallCtx<'_>,
    count: i64,
    start: u32,
    end: u32,
    letters: bool,
    numbers: bool,
) -> FuncResult {
    if count <= 0 {
        return Ok(Value::str(""));
    }
    if count as u128 * 4 > MAX_OUTPUT as u128 {
        return Err(too_large("randString"));
    }
    let (start, end) = if start == 0 && end == 0 {
        (b' ' as u32, b'z' as u32 + 1)
    } else {
        (start, end)
    };
    let gap = (end - start) as u64;
    let mut out = String::with_capacity(count as usize);
    let mut n = 0;
    while n < count {
        let ch = (super::rand_below(ctx, gap) as u32 + start) as u8 as char;
        if (letters && ch.is_ascii_alphabetic())
            || (numbers && ch.is_ascii_digit())
            || (!letters && !numbers)
        {
            out.push(ch);
            n += 1;
        }
    }
    Ok(Value::string(out))
}

fn shuffle(ctx: &mut CallCtx<'_>, a: &[Value]) -> FuncResult {
    let mut runes: Vec<char> = sarg(a, 0).chars().collect();
    let mut i = runes.len();
    while i > 1 {
        i -= 1;
        let j = super::rand_below(ctx, i as u64 + 1) as usize;
        runes.swap(i, j);
    }
    Ok(Value::string(runes.into_iter().collect()))
}

// ---------------------------------------------------------------------------
// xstrings case conversion
// ---------------------------------------------------------------------------

fn is_connector(r: char) -> bool {
    r == '-' || r == '_' || is_space(r)
}

const MIN_CJK: char = '\u{3400}';

/// xstrings `isAlphabet`.
fn is_alphabet(r: char) -> bool {
    if !gu::is_letter(r) {
        return false;
    }
    let u = r as u32;
    if r < MIN_CJK {
        return true;
    }
    !matches!(u, 0x4E00..=0x9FCC | 0x3400..=0x4D85 | 0x20000..=0x2B81D)
}

/// xstrings `ToPascalCase` (Sprig's `camelcase`).
fn to_pascal_case(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut buf = String::new();
    let mut chars = s.chars();
    let mut is_first_rune_upper = false;
    let mut r0 = '\0';
    for c in chars.by_ref() {
        r0 = c;
        if !is_connector(c) {
            is_first_rune_upper = gu::is_upper(c);
            r0 = gu::to_upper(c);
            break;
        }
        buf.push(c);
    }
    let rest: Vec<char> = chars.collect();
    if rest.is_empty() {
        // Go writes the last rune read, even if it was a connector.
        buf.push(r0);
        return buf;
    }
    for r in rest {
        let r1 = r0;
        r0 = r;
        if is_connector(r0) && is_connector(r1) {
            buf.push(r1);
            continue;
        }
        if is_connector(r1) {
            is_first_rune_upper = gu::is_upper(r0);
            r0 = gu::to_upper(r0);
        } else {
            if is_first_rune_upper {
                if gu::is_upper(r0) {
                    r0 = gu::to_lower(r0);
                } else {
                    is_first_rune_upper = false;
                }
            }
            buf.push(r1);
        }
    }
    buf.push(r0);
    buf
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WordType {
    Invalid,
    Number,
    UpperCase,
    Alphabet,
    Connector,
    Punct,
    Other,
}

/// xstrings `nextValidRune`: skips U+FFFD (Go's `RuneError`).
fn next_valid_rune(s: &str, prev: char) -> (char, usize) {
    let mut size = 0;
    for c in s.chars() {
        size += c.len_utf8();
        if c != '\u{FFFD}' {
            return (c, size);
        }
    }
    (prev, size)
}

fn next_word(s: &str) -> (WordType, &str, &str) {
    if s.is_empty() {
        return (WordType::Invalid, "", "");
    }
    let mut offset;
    let mut remaining = s;
    let (mut r, mut size) = next_valid_rune(remaining, '\u{FFFD}');
    offset = size;
    if r == '\u{FFFD}' {
        return (WordType::Invalid, &s[..offset], &s[offset..]);
    }
    let wt;
    if is_connector(r) {
        wt = WordType::Connector;
        remaining = &remaining[size..];
        while !remaining.is_empty() {
            (r, size) = next_valid_rune(remaining, r);
            if !is_connector(r) {
                break;
            }
            offset += size;
            remaining = &remaining[size..];
        }
    } else if gu::is_punct(r) {
        wt = WordType::Punct;
        remaining = &remaining[size..];
        while !remaining.is_empty() {
            (r, size) = next_valid_rune(remaining, r);
            if !gu::is_punct(r) {
                break;
            }
            offset += size;
            remaining = &remaining[size..];
        }
    } else if gu::is_upper(r) {
        wt = WordType::UpperCase;
        remaining = &remaining[size..];
        if !remaining.is_empty() {
            (r, size) = next_valid_rune(remaining, r);
            if gu::is_upper(r) {
                let mut prev_size = size;
                offset += size;
                remaining = &remaining[size..];
                while !remaining.is_empty() {
                    (r, size) = next_valid_rune(remaining, r);
                    if !gu::is_upper(r) {
                        break;
                    }
                    prev_size = size;
                    offset += size;
                    remaining = &remaining[size..];
                }
                if !remaining.is_empty() && is_alphabet(r) {
                    offset -= prev_size;
                    remaining = &s[offset..];
                }
            } else if is_alphabet(r) {
                offset += size;
                remaining = &remaining[size..];
                while !remaining.is_empty() {
                    (r, size) = next_valid_rune(remaining, r);
                    if !is_alphabet(r) || gu::is_upper(r) {
                        break;
                    }
                    offset += size;
                    remaining = &remaining[size..];
                }
            }
        }
    } else if is_alphabet(r) {
        wt = WordType::Alphabet;
        remaining = &remaining[size..];
        while !remaining.is_empty() {
            (r, size) = next_valid_rune(remaining, r);
            if !is_alphabet(r) || gu::is_upper(r) {
                break;
            }
            offset += size;
            remaining = &remaining[size..];
        }
    } else if gu::is_number(r) {
        wt = WordType::Number;
        remaining = &remaining[size..];
        while !remaining.is_empty() {
            (r, size) = next_valid_rune(remaining, r);
            if !gu::is_number(r) {
                break;
            }
            offset += size;
            remaining = &remaining[size..];
        }
    } else {
        wt = WordType::Other;
        remaining = &remaining[size..];
        while !remaining.is_empty() {
            (r, size) = next_valid_rune(remaining, r);
            if size == 0 || is_connector(r) || is_alphabet(r) || gu::is_number(r) || gu::is_punct(r)
            {
                break;
            }
            offset += size;
            remaining = &remaining[size..];
        }
    }
    (wt, &s[..offset], remaining)
}

fn to_lower_word(buf: &mut String, wt: WordType, s: &str, connector: char) {
    if wt != WordType::UpperCase && wt != WordType::Connector {
        buf.push_str(s);
        return;
    }
    for r in s.chars() {
        if is_connector(r) {
            buf.push(connector);
        } else if gu::is_upper(r) {
            buf.push(gu::to_lower(r));
        } else {
            buf.push(r);
        }
    }
}

/// xstrings `camelCaseToLowerCase` (`ToSnakeCase`/`ToKebabCase`).
fn camel_case_to_lower_case(s: &str, connector: char) -> String {
    use WordType::*;
    if s.is_empty() {
        return String::new();
    }
    let mut buf = String::new();
    let (mut wt, mut word, mut remaining) = next_word(s);
    while !remaining.is_empty() {
        if wt != Connector {
            to_lower_word(&mut buf, wt, word, connector);
        }
        let prev = wt;
        let last = word;
        (wt, word, remaining) = next_word(remaining);
        match prev {
            Number => {
                while wt == Alphabet || wt == Number {
                    to_lower_word(&mut buf, wt, word, connector);
                    (wt, word, remaining) = next_word(remaining);
                }
                if wt != Invalid && wt != Punct && wt != Connector {
                    buf.push(connector);
                }
            }
            Connector => to_lower_word(&mut buf, prev, last, connector),
            Punct => {}
            _ => {
                if wt != Number {
                    if wt != Connector && wt != Punct {
                        buf.push(connector);
                    }
                    continue;
                }
                if remaining.is_empty() {
                    continue;
                }
                let last = word;
                (wt, word, remaining) = next_word(remaining);
                if wt != Alphabet {
                    to_lower_word(&mut buf, Number, last, connector);
                    if wt != Connector && wt != Punct {
                        buf.push(connector);
                    }
                    continue;
                }
                buf.push(connector);
                to_lower_word(&mut buf, Number, last, connector);
                while wt == Alphabet || wt == Number {
                    to_lower_word(&mut buf, wt, word, connector);
                    (wt, word, remaining) = next_word(remaining);
                }
                if wt != Invalid && wt != Connector && wt != Punct {
                    buf.push(connector);
                }
            }
        }
    }
    to_lower_word(&mut buf, wt, word, connector);
    buf
}

// ---------------------------------------------------------------------------
// base64 / base32 (Go's encoding packages, standard alphabets)
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub(crate) fn base64_encode(src: &[u8]) -> String {
    let mut out = String::with_capacity(src.len().div_ceil(3) * 4);
    for chunk in src.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(v >> 18) as usize & 63] as char);
        out.push(B64[(v >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(v >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[v as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn b64_val(c: u8) -> u8 {
    match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => 0xFF,
    }
}

fn corrupt(kind: &str, at: usize) -> String {
    format!("illegal {kind} data at input byte {at}")
}

/// `base64.StdEncoding.DecodeString`.
pub(crate) fn base64_decode(src: &[u8]) -> Result<Vec<u8>, String> {
    let mut dst = Vec::with_capacity(src.len() / 4 * 3);
    let mut si = 0usize;
    while si < src.len() {
        let mut dbuf = [0u8; 4];
        let mut dlen = 4;
        let mut err: Option<String> = None;
        let mut j = 0;
        while j < 4 {
            if src.len() == si {
                if j == 0 {
                    return Ok(dst);
                }
                return Err(corrupt("base64", si - j));
            }
            let c = src[si];
            si += 1;
            let out = b64_val(c);
            if out != 0xFF {
                dbuf[j] = out;
                j += 1;
                continue;
            }
            if c == b'\n' || c == b'\r' {
                continue;
            }
            if c != b'=' {
                return Err(corrupt("base64", si - 1));
            }
            match j {
                0 | 1 => return Err(corrupt("base64", si - 1)),
                2 => {
                    while si < src.len() && (src[si] == b'\n' || src[si] == b'\r') {
                        si += 1;
                    }
                    if si == src.len() {
                        return Err(corrupt("base64", src.len()));
                    }
                    if src[si] != b'=' {
                        return Err(corrupt("base64", si - 1));
                    }
                    si += 1;
                }
                _ => {}
            }
            while si < src.len() && (src[si] == b'\n' || src[si] == b'\r') {
                si += 1;
            }
            if si < src.len() {
                err = Some(corrupt("base64", si));
            }
            dlen = j;
            break;
        }
        let val = ((dbuf[0] as u32) << 18)
            | ((dbuf[1] as u32) << 12)
            | ((dbuf[2] as u32) << 6)
            | dbuf[3] as u32;
        let bytes = [(val >> 16) as u8, (val >> 8) as u8, val as u8];
        dst.extend_from_slice(&bytes[..dlen - 1]);
        if let Some(e) = err {
            return Err(e);
        }
    }
    Ok(dst)
}

fn base32_encode(src: &[u8]) -> String {
    let mut out = String::with_capacity(src.len().div_ceil(5) * 8);
    for chunk in src.chunks(5) {
        let mut b = [0u8; 5];
        b[..chunk.len()].copy_from_slice(chunk);
        let v = ((b[0] as u64) << 32)
            | ((b[1] as u64) << 24)
            | ((b[2] as u64) << 16)
            | ((b[3] as u64) << 8)
            | b[4] as u64;
        let nchars = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for i in 0..8 {
            if i < nchars {
                out.push(B32[((v >> (35 - 5 * i)) & 31) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `base32.StdEncoding.DecodeString`.
fn base32_decode(src: &[u8]) -> Result<Vec<u8>, String> {
    let src: Vec<u8> = src
        .iter()
        .copied()
        .filter(|&c| c != b'\n' && c != b'\r')
        .collect();
    let olen = src.len();
    let mut s = &src[..];
    let mut dst = Vec::new();
    let mut end = false;
    let val = |c: u8| -> u8 {
        match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => 0xFF,
        }
    };
    while !s.is_empty() && !end {
        let mut dbuf = [0u8; 8];
        let mut dlen = 8;
        let mut j = 0;
        while j < 8 {
            if s.is_empty() {
                return Err(corrupt("base32", olen - s.len() - j));
            }
            let c = s[0];
            s = &s[1..];
            if c == b'=' && j >= 2 && s.len() < 8 {
                if s.len() + j < 8 - 1 {
                    return Err(corrupt("base32", olen));
                }
                for k in 0..8 - 1 - j {
                    if s.len() > k && s[k] != b'=' {
                        return Err(corrupt("base32", olen - s.len() + k - 1));
                    }
                }
                dlen = j;
                end = true;
                if dlen == 1 || dlen == 3 || dlen == 6 {
                    return Err(corrupt("base32", olen - s.len() - 1));
                }
                break;
            }
            dbuf[j] = val(c);
            if dbuf[j] == 0xFF {
                return Err(corrupt("base32", olen - s.len() - 1));
            }
            j += 1;
        }
        let mut out = [0u8; 5];
        out[4] = dbuf[6] << 5 | dbuf[7];
        out[3] = dbuf[4] << 7 | dbuf[5] << 2 | dbuf[6] >> 3;
        out[2] = dbuf[3] << 4 | dbuf[4] >> 1;
        out[1] = dbuf[1] << 6 | dbuf[2] << 1 | dbuf[3] >> 4;
        out[0] = dbuf[0] << 3 | dbuf[1] >> 2;
        let n = match dlen {
            8 => 5,
            7 => 4,
            5 => 3,
            4 => 2,
            2 => 1,
            _ => 0,
        };
        dst.extend_from_slice(&out[..n]);
    }
    Ok(dst)
}

// ---------------------------------------------------------------------------
// os.ExpandEnv with an empty environment
// ---------------------------------------------------------------------------

fn expand_env(s: &str) -> String {
    let b = s.as_bytes();
    let special = |c: u8| {
        matches!(
            c,
            b'*' | b'#' | b'$' | b'@' | b'!' | b'?' | b'-' | b'0'..=b'9'
        )
    };
    let alnum = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    let mut buf: Option<Vec<u8>> = None;
    let mut i = 0;
    let mut j = 0;
    while j < b.len() {
        if b[j] == b'$' && j + 1 < b.len() {
            let out = buf.get_or_insert_with(Vec::new);
            out.extend_from_slice(&b[i..j]);
            let t = &b[j + 1..];
            let (name_len, w): (usize, usize) = if t[0] == b'{' {
                if t.len() > 2 && special(t[1]) && t[2] == b'}' {
                    (1, 3)
                } else {
                    let mut r = (0, 1);
                    for k in 1..t.len() {
                        if t[k] == b'}' {
                            r = if k == 1 { (0, 2) } else { (k - 1, k + 1) };
                            break;
                        }
                    }
                    r
                }
            } else if special(t[0]) {
                (1, 1)
            } else {
                let mut k = 0;
                while k < t.len() && alnum(t[k]) {
                    k += 1;
                }
                (k, k)
            };
            if name_len == 0 && w == 0 {
                out.push(b'$');
            }
            // Names (and bad syntax) expand to "" with no environment.
            j += w;
            i = j + 1;
        }
        j += 1;
    }
    match buf {
        None => s.to_string(),
        Some(mut out) => {
            if i < b.len() {
                out.extend_from_slice(&b[i..]);
            }
            strconv::lossy_vec(out)
        }
    }
}

/// A string argument as bytes; byte-carrying Go strings arrive as `Bytes`.
fn sbytes(a: &[Value], i: usize) -> alloc::vec::Vec<u8> {
    match a.get(i) {
        Some(Value::Bytes(b)) => b.to_vec(),
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        _ => alloc::vec::Vec::new(),
    }
}
