//! Port of the parts of Go's `strconv` (and `unicode/utf8`) that `fmt` and the
//! template functions rely on: float formatting, quoting, number parsing.

#![allow(clippy::needless_range_loop, clippy::collapsible_match, clippy::too_many_arguments, clippy::type_complexity)]

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use super::isprint::{IS_GRAPHIC, IS_NOT_PRINT_16, IS_NOT_PRINT_32, IS_PRINT_16, IS_PRINT_32};

const LOWERHEX: &[u8; 16] = b"0123456789abcdef";
const UPPERHEX: &[u8; 16] = b"0123456789ABCDEF";

/// `utf8.RuneError`.
pub const RUNE_ERROR: u32 = 0xFFFD;
/// `utf8.MaxRune`.
pub const MAX_RUNE: u32 = 0x10FFFF;

// ---------------------------------------------------------------------------
// UTF-8 (Go semantics: one RuneError per invalid byte)
// ---------------------------------------------------------------------------

/// `utf8.DecodeRune`: returns `(RuneError, 1)` for invalid encodings and
/// `(RuneError, 0)` for empty input.
pub fn decode_rune(s: &[u8]) -> (u32, usize) {
    let Some(&b0) = s.first() else { return (RUNE_ERROR, 0) };
    if b0 < 0x80 {
        return (b0 as u32, 1);
    }
    let (len, min, init) = match b0 {
        0xC2..=0xDF => (2, 0x80, (b0 & 0x1F) as u32),
        0xE0..=0xEF => (3, 0x800, (b0 & 0x0F) as u32),
        0xF0..=0xF4 => (4, 0x10000, (b0 & 0x07) as u32),
        _ => return (RUNE_ERROR, 1),
    };
    if s.len() < len {
        return (RUNE_ERROR, 1);
    }
    let mut r = init;
    for &b in &s[1..len] {
        if b & 0xC0 != 0x80 {
            return (RUNE_ERROR, 1);
        }
        r = (r << 6) | (b & 0x3F) as u32;
    }
    if r < min || r > MAX_RUNE || (0xD800..=0xDFFF).contains(&r) {
        return (RUNE_ERROR, 1);
    }
    (r, len)
}

/// `utf8.ValidRune`.
pub fn valid_rune(r: u32) -> bool {
    r <= MAX_RUNE && !(0xD800..=0xDFFF).contains(&r)
}

/// `utf8.AppendRune` (invalid runes become U+FFFD).
pub fn push_rune(out: &mut Vec<u8>, r: u32) {
    let c = char::from_u32(r).unwrap_or('\u{FFFD}');
    let mut b = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
}

/// `utf8.RuneCount`.
pub fn rune_count(mut s: &[u8]) -> usize {
    let mut n = 0;
    while !s.is_empty() {
        let (_, w) = decode_rune(s);
        s = &s[w..];
        n += 1;
    }
    n
}

/// Converts bytes to a `String` the way Go's text output would end up after
/// a round trip through `encoding/json`: every invalid byte becomes U+FFFD.
pub fn lossy(s: &[u8]) -> String {
    match core::str::from_utf8(s) {
        Ok(v) => String::from(v),
        Err(_) => {
            let mut out = String::with_capacity(s.len());
            let mut rest = s;
            while !rest.is_empty() {
                let (r, w) = decode_rune(rest);
                out.push(char::from_u32(r).unwrap_or('\u{FFFD}'));
                rest = &rest[w..];
            }
            out
        }
    }
}

/// [`lossy`] taking ownership.
pub fn lossy_vec(s: Vec<u8>) -> String {
    match String::from_utf8(s) {
        Ok(v) => v,
        Err(e) => lossy(e.as_bytes()),
    }
}

// ---------------------------------------------------------------------------
// IsPrint / IsGraphic
// ---------------------------------------------------------------------------

fn bsearch16(s: &[u16], v: u16) -> (usize, bool) {
    let (mut i, mut j) = (0, s.len());
    while i < j {
        let h = i + (j - i) / 2;
        if s[h] < v {
            i = h + 1;
        } else {
            j = h;
        }
    }
    (i, i < s.len() && s[i] == v)
}

fn bsearch32(s: &[u32], v: u32) -> (usize, bool) {
    let (mut i, mut j) = (0, s.len());
    while i < j {
        let h = i + (j - i) / 2;
        if s[h] < v {
            i = h + 1;
        } else {
            j = h;
        }
    }
    (i, i < s.len() && s[i] == v)
}

/// `strconv.IsPrint`.
pub fn is_print(r: u32) -> bool {
    if r <= 0xFF {
        if (0x20..=0x7E).contains(&r) {
            return true;
        }
        if (0xA1..=0xFF).contains(&r) {
            return r != 0xAD;
        }
        return false;
    }
    if r < 1 << 16 {
        let rr = r as u16;
        let (i, _) = bsearch16(&IS_PRINT_16, rr);
        if i >= IS_PRINT_16.len() || rr < IS_PRINT_16[i & !1] || IS_PRINT_16[i | 1] < rr {
            return false;
        }
        let (_, found) = bsearch16(&IS_NOT_PRINT_16, rr);
        return !found;
    }
    let (i, _) = bsearch32(&IS_PRINT_32, r);
    if i >= IS_PRINT_32.len() || r < IS_PRINT_32[i & !1] || IS_PRINT_32[i | 1] < r {
        return false;
    }
    if r >= 0x20000 {
        return true;
    }
    let (_, found) = bsearch16(&IS_NOT_PRINT_32, (r - 0x10000) as u16);
    !found
}

/// `strconv.IsGraphic`.
pub fn is_graphic(r: u32) -> bool {
    is_print(r) || (r <= 0xFFFF && bsearch16(&IS_GRAPHIC, r as u16).1)
}

// ---------------------------------------------------------------------------
// Quoting
// ---------------------------------------------------------------------------

fn append_escaped_rune(buf: &mut Vec<u8>, r: u32, quote: u8, ascii_only: bool) {
    if r == quote as u32 || r == '\\' as u32 {
        buf.push(b'\\');
        buf.push(r as u8);
        return;
    }
    if ascii_only {
        if r < 0x80 && is_print(r) {
            buf.push(r as u8);
            return;
        }
    } else if is_print(r) {
        push_rune(buf, r);
        return;
    }
    match r {
        0x07 => buf.extend_from_slice(b"\\a"),
        0x08 => buf.extend_from_slice(b"\\b"),
        0x0C => buf.extend_from_slice(b"\\f"),
        0x0A => buf.extend_from_slice(b"\\n"),
        0x0D => buf.extend_from_slice(b"\\r"),
        0x09 => buf.extend_from_slice(b"\\t"),
        0x0B => buf.extend_from_slice(b"\\v"),
        _ => {
            if r < b' ' as u32 || r == 0x7F {
                buf.extend_from_slice(b"\\x");
                buf.push(LOWERHEX[((r as u8) >> 4) as usize]);
                buf.push(LOWERHEX[((r as u8) & 0xF) as usize]);
            } else {
                let r = if valid_rune(r) { r } else { 0xFFFD };
                if r < 0x10000 {
                    buf.extend_from_slice(b"\\u");
                    for s in (0..=12).rev().step_by(4) {
                        buf.push(LOWERHEX[((r >> s) & 0xF) as usize]);
                    }
                } else {
                    buf.extend_from_slice(b"\\U");
                    for s in (0..=28).rev().step_by(4) {
                        buf.push(LOWERHEX[((r >> s) & 0xF) as usize]);
                    }
                }
            }
        }
    }
}

/// `strconv.AppendQuote` / `AppendQuoteToASCII` on raw bytes.
pub fn append_quoted(buf: &mut Vec<u8>, mut s: &[u8], quote: u8, ascii_only: bool) {
    buf.push(quote);
    while !s.is_empty() {
        let (r, w) = decode_rune(s);
        if w == 1 && r == RUNE_ERROR {
            buf.extend_from_slice(b"\\x");
            buf.push(LOWERHEX[(s[0] >> 4) as usize]);
            buf.push(LOWERHEX[(s[0] & 0xF) as usize]);
            s = &s[1..];
            continue;
        }
        append_escaped_rune(buf, r, quote, ascii_only);
        s = &s[w..];
    }
    buf.push(quote);
}

/// `strconv.AppendQuoteRune` / `AppendQuoteRuneToASCII`.
pub fn append_quoted_rune(buf: &mut Vec<u8>, r: u32, ascii_only: bool) {
    buf.push(b'\'');
    let r = if valid_rune(r) { r } else { RUNE_ERROR };
    append_escaped_rune(buf, r, b'\'', ascii_only);
    buf.push(b'\'');
}

/// `strconv.Quote`.
pub fn quote(s: &str) -> String {
    let mut b = Vec::with_capacity(s.len() + 2);
    append_quoted(&mut b, s.as_bytes(), b'"', false);
    lossy_vec(b)
}

/// `strconv.Quote` of raw bytes (invalid UTF-8 becomes `\xNN`).
pub fn quote_bytes(s: &[u8]) -> String {
    let mut b = Vec::with_capacity(s.len() + 2);
    append_quoted(&mut b, s, b'"', false);
    lossy_vec(b)
}

/// `strconv.CanBackquote`.
pub fn can_backquote(mut s: &[u8]) -> bool {
    while !s.is_empty() {
        let (r, w) = decode_rune(s);
        s = &s[w..];
        if w > 1 {
            if r == 0xFEFF {
                return false;
            }
            continue;
        }
        if r == RUNE_ERROR {
            return false;
        }
        if (r < b' ' as u32 && r != b'\t' as u32) || r == b'`' as u32 || r == 0x7F {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Integers
// ---------------------------------------------------------------------------

/// Error kinds of `strconv` parse functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumErr {
    Syntax,
    Range,
}

impl NumErr {
    pub fn text(self) -> &'static str {
        match self {
            NumErr::Syntax => "invalid syntax",
            NumErr::Range => "value out of range",
        }
    }
}

/// `(*NumError).Error()`, e.g. `strconv.ParseInt: parsing "x": invalid syntax`.
pub fn num_error(func: &str, input: &str, e: NumErr) -> String {
    alloc::format!("strconv.{func}: parsing {}: {}", quote(input), e.text())
}

fn lower(c: u8) -> u8 {
    c | (b'x' - b'X')
}

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
        if c.is_ascii_digit() || (hex && (b'a'..=b'f').contains(&lower(c))) {
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

/// `strconv.ParseUint(s, base, bitSize)` (base 0 or 2..=36; bitSize 0 means 64).
pub fn parse_uint(s: &str, base: u32, bit_size: u32) -> Result<u64, NumErr> {
    let sb = s.as_bytes();
    if sb.is_empty() {
        return Err(NumErr::Syntax);
    }
    let base0 = base == 0;
    let mut base = base;
    let mut digits = sb;
    if base == 0 {
        base = 10;
        if sb[0] == b'0' {
            if sb.len() >= 3 && lower(sb[1]) == b'b' {
                base = 2;
                digits = &sb[2..];
            } else if sb.len() >= 3 && lower(sb[1]) == b'o' {
                base = 8;
                digits = &sb[2..];
            } else if sb.len() >= 3 && lower(sb[1]) == b'x' {
                base = 16;
                digits = &sb[2..];
            } else {
                base = 8;
                digits = &sb[1..];
            }
        }
    } else if !(2..=36).contains(&base) {
        return Err(NumErr::Syntax);
    }
    let bit_size = if bit_size == 0 { 64 } else { bit_size.min(64) };
    let cutoff = u64::MAX / base as u64 + 1;
    let max_val = if bit_size == 64 { u64::MAX } else { (1u64 << bit_size) - 1 };
    let mut underscores = false;
    let mut n: u64 = 0;
    for &c in digits {
        let d = if c == b'_' && base0 {
            underscores = true;
            continue;
        } else if c.is_ascii_digit() {
            c - b'0'
        } else if lower(c).is_ascii_lowercase() {
            lower(c) - b'a' + 10
        } else {
            return Err(NumErr::Syntax);
        };
        if d as u32 >= base {
            return Err(NumErr::Syntax);
        }
        if n >= cutoff {
            return Err(NumErr::Range);
        }
        n *= base as u64;
        let n1 = n.wrapping_add(d as u64);
        if n1 < n || n1 > max_val {
            return Err(NumErr::Range);
        }
        n = n1;
    }
    if underscores && !underscore_ok(sb) {
        return Err(NumErr::Syntax);
    }
    Ok(n)
}

/// `strconv.ParseInt(s, base, bitSize)`. On range errors Go also returns the
/// clamped value; that is reported in the `Err` variant's payload.
pub fn parse_int(s: &str, base: u32, bit_size: u32) -> Result<i64, (NumErr, i64)> {
    if s.is_empty() {
        return Err((NumErr::Syntax, 0));
    }
    let (neg, rest) = match s.as_bytes()[0] {
        b'+' => (false, &s[1..]),
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    let bit_size = if bit_size == 0 { 64 } else { bit_size.min(64) };
    let un = match parse_uint(rest, base, bit_size) {
        Ok(v) => v,
        Err(NumErr::Range) => u64::MAX,
        Err(e) => return Err((e, 0)),
    };
    let cutoff = 1u64 << (bit_size - 1);
    if !neg && un >= cutoff {
        return Err((NumErr::Range, (cutoff - 1) as i64));
    }
    if neg && un > cutoff {
        return Err((NumErr::Range, (cutoff as i64).wrapping_neg()));
    }
    let n = un as i64;
    Ok(if neg { n.wrapping_neg() } else { n })
}

/// `strconv.Atoi`.
pub fn atoi(s: &str) -> Result<i64, (NumErr, i64)> {
    parse_int(s, 10, 64)
}

// ---------------------------------------------------------------------------
// Float parsing
// ---------------------------------------------------------------------------

fn special(s: &[u8]) -> Option<(f64, usize)> {
    if s.is_empty() {
        return None;
    }
    fn prefix_len(s: &[u8], p: &[u8]) -> usize {
        let n = s.len().min(p.len());
        for i in 0..n {
            if s[i].to_ascii_lowercase() != p[i] {
                return i;
            }
        }
        n
    }
    let mut sign = 1.0;
    let mut nsign = 0;
    let mut t = s;
    match s[0] {
        b'+' | b'-' | b'i' | b'I' => {
            if s[0] == b'+' || s[0] == b'-' {
                if s[0] == b'-' {
                    sign = -1.0;
                }
                nsign = 1;
                t = &s[1..];
            }
            let mut n = prefix_len(t, b"infinity");
            if 3 < n && n < 8 {
                n = 3;
            }
            if n == 3 || n == 8 {
                return Some((sign * f64::INFINITY, nsign + n));
            }
        }
        b'n' | b'N' => {
            if prefix_len(s, b"nan") == 3 {
                return Some((f64::NAN, 3));
            }
        }
        _ => {}
    }
    None
}

struct ReadFloat {
    mantissa: u64,
    exp: i64,
    neg: bool,
    trunc: bool,
    hex: bool,
    n: usize,
}

fn read_float(s: &[u8]) -> Option<ReadFloat> {
    let mut underscores = false;
    let mut i = 0;
    let mut neg = false;
    let mut mantissa: u64 = 0;
    let mut trunc = false;
    let mut hex = false;
    if s.is_empty() {
        return None;
    }
    match s[0] {
        b'+' => i += 1,
        b'-' => {
            i += 1;
            neg = true
        }
        _ => {}
    }
    let mut base: u64 = 10;
    let mut max_mant_digits = 19;
    let mut exp_char = b'e';
    if i + 2 < s.len() && s[i] == b'0' && lower(s[i + 1]) == b'x' {
        base = 16;
        max_mant_digits = 16;
        i += 2;
        exp_char = b'p';
        hex = true;
    }
    let mut sawdot = false;
    let mut sawdigits = false;
    let mut nd: i64 = 0;
    let mut nd_mant: i64 = 0;
    let mut dp: i64 = 0;
    while i < s.len() {
        let c = s[i];
        if c == b'_' {
            underscores = true;
        } else if c == b'.' {
            if sawdot {
                break;
            }
            sawdot = true;
            dp = nd;
        } else if c.is_ascii_digit() {
            sawdigits = true;
            if c == b'0' && nd == 0 {
                dp -= 1;
                i += 1;
                continue;
            }
            nd += 1;
            if nd_mant < max_mant_digits {
                mantissa = mantissa * base + (c - b'0') as u64;
                nd_mant += 1;
            } else if c != b'0' {
                trunc = true;
            }
        } else if base == 16 && (b'a'..=b'f').contains(&lower(c)) {
            sawdigits = true;
            nd += 1;
            if nd_mant < max_mant_digits {
                mantissa = mantissa * 16 + (lower(c) - b'a' + 10) as u64;
                nd_mant += 1;
            } else {
                trunc = true;
            }
        } else {
            break;
        }
        i += 1;
    }
    if !sawdigits {
        return None;
    }
    if !sawdot {
        dp = nd;
    }
    if base == 16 {
        dp *= 4;
        nd_mant *= 4;
    }
    if i < s.len() && lower(s[i]) == exp_char {
        i += 1;
        if i >= s.len() {
            return None;
        }
        let mut esign = 1;
        if s[i] == b'+' {
            i += 1;
        } else if s[i] == b'-' {
            i += 1;
            esign = -1;
        }
        if i >= s.len() || !s[i].is_ascii_digit() {
            return None;
        }
        let mut e: i64 = 0;
        while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'_') {
            if s[i] == b'_' {
                underscores = true;
                i += 1;
                continue;
            }
            if e < 10000 {
                e = e * 10 + (s[i] - b'0') as i64;
            }
            i += 1;
        }
        dp += e * esign;
    } else if base == 16 {
        return None;
    }
    let exp = if mantissa != 0 { dp - nd_mant } else { 0 };
    if underscores && !underscore_ok(&s[..i]) {
        return None;
    }
    Some(ReadFloat { mantissa, exp, neg, trunc, hex, n: i })
}

fn atof_hex(mut mantissa: u64, exp: i64, neg: bool, trunc: bool) -> Result<f64, (NumErr, f64)> {
    const MANTBITS: i64 = 52;
    const EXPBITS: i64 = 11;
    const BIAS: i64 = -1023;
    let max_exp = (1 << EXPBITS) + BIAS - 2;
    let min_exp = BIAS + 1;
    let mut exp = exp + MANTBITS;
    while mantissa != 0 && mantissa >> (MANTBITS + 2) == 0 {
        mantissa <<= 1;
        exp -= 1;
    }
    if trunc {
        mantissa |= 1;
    }
    while mantissa >> (1 + MANTBITS + 2) != 0 {
        mantissa = (mantissa >> 1) | (mantissa & 1);
        exp += 1;
    }
    while mantissa > 1 && exp < min_exp - 2 {
        mantissa = (mantissa >> 1) | (mantissa & 1);
        exp += 1;
    }
    let mut round = mantissa & 3;
    mantissa >>= 2;
    round |= mantissa & 1;
    exp += 2;
    if round == 3 {
        mantissa += 1;
        if mantissa == 1 << (1 + MANTBITS) {
            mantissa >>= 1;
            exp += 1;
        }
    }
    if mantissa >> MANTBITS == 0 {
        exp = BIAS;
    }
    let mut err = false;
    if exp > max_exp {
        mantissa = 1 << MANTBITS;
        exp = max_exp + 1;
        err = true;
    }
    let mut bits = mantissa & ((1 << MANTBITS) - 1);
    bits |= (((exp - BIAS) & ((1 << EXPBITS) - 1)) as u64) << MANTBITS;
    if neg {
        bits |= 1 << 63;
    }
    let f = f64::from_bits(bits);
    if err { Err((NumErr::Range, f)) } else { Ok(f) }
}

/// `strconv.ParseFloat(s, 64)`. On a range error Go returns ±Inf alongside
/// the error; it is given in the `Err` payload.
pub fn parse_float(s: &str) -> Result<f64, (NumErr, f64)> {
    let b = s.as_bytes();
    if let Some((v, n)) = special(b) {
        if n == b.len() {
            return Ok(v);
        }
        return Err((NumErr::Syntax, 0.0));
    }
    let Some(rf) = read_float(b) else { return Err((NumErr::Syntax, 0.0)) };
    if rf.n != b.len() {
        return Err((NumErr::Syntax, 0.0));
    }
    if rf.hex {
        return atof_hex(rf.mantissa, rf.exp, rf.neg, rf.trunc);
    }
    // Decimal: Rust's parser is correctly rounded, as is Go's.
    let clean: String = s.chars().filter(|&c| c != '_').collect();
    match clean.parse::<f64>() {
        Ok(v) if v.is_infinite() => Err((NumErr::Range, v)),
        Ok(v) => Ok(v),
        Err(_) => Err((NumErr::Syntax, 0.0)),
    }
}

// ---------------------------------------------------------------------------
// Float formatting
// ---------------------------------------------------------------------------

/// Multiprecision decimal (Go's `strconv.decimal`), used for exact
/// fixed-precision formatting.
struct Decimal {
    d: Vec<u8>,
    nd: usize,
    dp: i64,
    trunc: bool,
}

const DEC_CAP: usize = 800;
const MAX_SHIFT: u32 = 60;

static LEFTCHEATS: [(i64, &str); 61] = [
    (0, ""),
    (1, "5"),
    (1, "25"),
    (1, "125"),
    (2, "625"),
    (2, "3125"),
    (2, "15625"),
    (3, "78125"),
    (3, "390625"),
    (3, "1953125"),
    (4, "9765625"),
    (4, "48828125"),
    (4, "244140625"),
    (4, "1220703125"),
    (5, "6103515625"),
    (5, "30517578125"),
    (5, "152587890625"),
    (6, "762939453125"),
    (6, "3814697265625"),
    (6, "19073486328125"),
    (7, "95367431640625"),
    (7, "476837158203125"),
    (7, "2384185791015625"),
    (7, "11920928955078125"),
    (8, "59604644775390625"),
    (8, "298023223876953125"),
    (8, "1490116119384765625"),
    (9, "7450580596923828125"),
    (9, "37252902984619140625"),
    (9, "186264514923095703125"),
    (10, "931322574615478515625"),
    (10, "4656612873077392578125"),
    (10, "23283064365386962890625"),
    (10, "116415321826934814453125"),
    (11, "582076609134674072265625"),
    (11, "2910383045673370361328125"),
    (11, "14551915228366851806640625"),
    (12, "72759576141834259033203125"),
    (12, "363797880709171295166015625"),
    (12, "1818989403545856475830078125"),
    (13, "9094947017729282379150390625"),
    (13, "45474735088646411895751953125"),
    (13, "227373675443232059478759765625"),
    (13, "1136868377216160297393798828125"),
    (14, "5684341886080801486968994140625"),
    (14, "28421709430404007434844970703125"),
    (14, "142108547152020037174224853515625"),
    (15, "710542735760100185871124267578125"),
    (15, "3552713678800500929355621337890625"),
    (15, "17763568394002504646778106689453125"),
    (16, "88817841970012523233890533447265625"),
    (16, "444089209850062616169452667236328125"),
    (16, "2220446049250313080847263336181640625"),
    (16, "11102230246251565404236316680908203125"),
    (17, "55511151231257827021181583404541015625"),
    (17, "277555756156289135105907917022705078125"),
    (17, "1387778780781445675529539585113525390625"),
    (18, "6938893903907228377647697925567626953125"),
    (18, "34694469519536141888238489627838134765625"),
    (18, "173472347597680709441192448139190673828125"),
    (19, "867361737988403547205962240695953369140625"),
];

impl Decimal {
    fn new(v: u64) -> Decimal {
        let mut d = Decimal { d: alloc::vec![0u8; DEC_CAP], nd: 0, dp: 0, trunc: false };
        let mut buf = [0u8; 24];
        let mut n = 0;
        let mut v = v;
        while v > 0 {
            let v1 = v / 10;
            buf[n] = (v - 10 * v1) as u8 + b'0';
            n += 1;
            v = v1;
        }
        while n > 0 {
            n -= 1;
            d.d[d.nd] = buf[n];
            d.nd += 1;
        }
        d.dp = d.nd as i64;
        d.trim();
        d
    }

    fn trim(&mut self) {
        while self.nd > 0 && self.d[self.nd - 1] == b'0' {
            self.nd -= 1;
        }
        if self.nd == 0 {
            self.dp = 0;
        }
    }

    fn right_shift(&mut self, k: u32) {
        let mut r = 0usize;
        let mut w = 0usize;
        let mut n: u64 = 0;
        while n >> k == 0 {
            if r >= self.nd {
                if n == 0 {
                    self.nd = 0;
                    return;
                }
                while n >> k == 0 {
                    n *= 10;
                    r += 1;
                }
                break;
            }
            let c = self.d[r] as u64;
            n = n * 10 + c - b'0' as u64;
            r += 1;
        }
        self.dp -= r as i64 - 1;
        let mask: u64 = (1 << k) - 1;
        while r < self.nd {
            let c = self.d[r] as u64;
            let dig = n >> k;
            n &= mask;
            self.d[w] = (dig as u8) + b'0';
            w += 1;
            n = n * 10 + c - b'0' as u64;
            r += 1;
        }
        while n > 0 {
            let dig = n >> k;
            n &= mask;
            if w < DEC_CAP {
                self.d[w] = (dig as u8) + b'0';
                w += 1;
            } else if dig > 0 {
                self.trunc = true;
            }
            n *= 10;
        }
        self.nd = w;
        self.trim();
    }

    fn left_shift(&mut self, k: u32) {
        let (mut delta, cutoff) = LEFTCHEATS[k as usize];
        // prefixIsLessThan
        let less = {
            let b = &self.d[..self.nd];
            let s = cutoff.as_bytes();
            let mut res = false;
            for i in 0..s.len() {
                if i >= b.len() {
                    res = true;
                    break;
                }
                if b[i] != s[i] {
                    res = b[i] < s[i];
                    break;
                }
            }
            res
        };
        if less {
            delta -= 1;
        }
        let mut r = self.nd as i64;
        let mut w = self.nd as i64 + delta;
        let mut n: u64 = 0;
        r -= 1;
        while r >= 0 {
            n += ((self.d[r as usize] - b'0') as u64) << k;
            let quo = n / 10;
            let rem = n - 10 * quo;
            w -= 1;
            if (w as usize) < DEC_CAP {
                self.d[w as usize] = rem as u8 + b'0';
            } else if rem != 0 {
                self.trunc = true;
            }
            n = quo;
            r -= 1;
        }
        while n > 0 {
            let quo = n / 10;
            let rem = n - 10 * quo;
            w -= 1;
            if (w as usize) < DEC_CAP {
                self.d[w as usize] = rem as u8 + b'0';
            } else if rem != 0 {
                self.trunc = true;
            }
            n = quo;
        }
        self.nd = (self.nd as i64 + delta) as usize;
        if self.nd >= DEC_CAP {
            self.nd = DEC_CAP;
        }
        self.dp += delta;
        self.trim();
    }

    fn shift(&mut self, mut k: i64) {
        if self.nd == 0 {
            return;
        }
        if k > 0 {
            while k > MAX_SHIFT as i64 {
                self.left_shift(MAX_SHIFT);
                k -= MAX_SHIFT as i64;
            }
            self.left_shift(k as u32);
        } else if k < 0 {
            while k < -(MAX_SHIFT as i64) {
                self.right_shift(MAX_SHIFT);
                k += MAX_SHIFT as i64;
            }
            self.right_shift((-k) as u32);
        }
    }

    fn should_round_up(&self, nd: i64) -> bool {
        if nd < 0 || nd as usize >= self.nd {
            return false;
        }
        let nd = nd as usize;
        if self.d[nd] == b'5' && nd + 1 == self.nd {
            if self.trunc {
                return true;
            }
            return nd > 0 && (self.d[nd - 1] - b'0') % 2 != 0;
        }
        self.d[nd] >= b'5'
    }

    fn round(&mut self, nd: i64) {
        if nd < 0 || nd as usize >= self.nd {
            return;
        }
        if self.should_round_up(nd) {
            self.round_up(nd as usize);
        } else {
            self.nd = nd as usize;
            self.trim();
        }
    }

    fn round_up(&mut self, nd: usize) {
        let mut i = nd;
        while i > 0 {
            i -= 1;
            if self.d[i] < b'9' {
                self.d[i] += 1;
                self.nd = i + 1;
                return;
            }
        }
        self.d[0] = b'1';
        self.nd = 1;
        self.dp += 1;
    }
}

/// Shortest decimal digits that round-trip, via Rust's formatter (which,
/// like Go's, picks the closest of the shortest candidates).
fn shortest_digits(f: f64) -> (Vec<u8>, i64) {
    let mut s = String::new();
    let _ = write!(s, "{:e}", f.abs());
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).collect();
    (digits, exp + 1)
}

fn fmt_efg(dst: &mut Vec<u8>, neg: bool, s: &[u8], dp: i64, nd: i64, prec: i64, fmt: u8, shortest: bool) {
    let mut prec = prec;
    let mut fmt = fmt;
    if fmt == b'g' || fmt == b'G' {
        let mut eprec = prec;
        if eprec > nd && nd >= dp {
            eprec = nd;
        }
        if shortest {
            eprec = 6;
        }
        let exp = dp - 1;
        if exp < -4 || exp >= eprec {
            if prec > nd {
                prec = nd;
            }
            prec -= 1;
            fmt = fmt + b'e' - b'g';
        } else {
            if prec > dp {
                prec = nd;
            }
            prec = (prec - dp).max(0);
            fmt = b'f';
        }
    }
    match fmt {
        b'e' | b'E' => {
            if neg {
                dst.push(b'-');
            }
            dst.push(if nd != 0 { s[0] } else { b'0' });
            if prec > 0 {
                dst.push(b'.');
                let mut i = 1i64;
                let m = nd.min(prec + 1);
                if i < m {
                    dst.extend_from_slice(&s[i as usize..m as usize]);
                    i = m;
                }
                for _ in 0..(prec + 1 - i).max(0) {
                    dst.push(b'0');
                }
            }
            dst.push(fmt);
            let mut exp = dp - 1;
            if nd == 0 {
                exp = 0;
            }
            if exp < 0 {
                dst.push(b'-');
                exp = -exp;
            } else {
                dst.push(b'+');
            }
            if exp < 10 {
                dst.push(b'0');
                dst.push(exp as u8 + b'0');
            } else if exp < 100 {
                dst.push((exp / 10) as u8 + b'0');
                dst.push((exp % 10) as u8 + b'0');
            } else {
                dst.push((exp / 100) as u8 + b'0');
                dst.push(((exp / 10) % 10) as u8 + b'0');
                dst.push((exp % 10) as u8 + b'0');
            }
        }
        b'f' => {
            if neg {
                dst.push(b'-');
            }
            if dp > 0 {
                let m = nd.min(dp);
                dst.extend_from_slice(&s[..m as usize]);
                for _ in 0..(dp - m) {
                    dst.push(b'0');
                }
            } else {
                dst.push(b'0');
            }
            if prec > 0 {
                dst.push(b'.');
                let lz = prec.min((-dp).max(0));
                let off = dp + lz;
                let m = (prec - lz).min((nd - off).max(0));
                let tz = (prec - lz - m).max(0);
                for _ in 0..lz {
                    dst.push(b'0');
                }
                for i in 0..m {
                    dst.push(s[(off + i) as usize]);
                }
                for _ in 0..tz {
                    dst.push(b'0');
                }
            }
        }
        _ => {
            dst.push(b'%');
            dst.push(fmt);
        }
    }
}

fn fmt_b(dst: &mut Vec<u8>, neg: bool, mant: u64, exp: i64) {
    if neg {
        dst.push(b'-');
    }
    let _ = write_u64(dst, mant);
    dst.push(b'p');
    if exp >= 0 {
        dst.push(b'+');
    }
    let mut s = String::new();
    let _ = write!(s, "{exp}");
    dst.extend_from_slice(s.as_bytes());
}

fn write_u64(dst: &mut Vec<u8>, v: u64) -> core::fmt::Result {
    let mut s = String::new();
    write!(s, "{v}")?;
    dst.extend_from_slice(s.as_bytes());
    Ok(())
}

fn fmt_x(dst: &mut Vec<u8>, prec: i64, fmt: u8, neg: bool, mut mant: u64, mut exp: i64, mantbits: i64) {
    if mant == 0 {
        exp = 0;
    }
    mant <<= 60 - mantbits;
    while mant != 0 && mant & (1 << 60) == 0 {
        mant <<= 1;
        exp -= 1;
    }
    if (0..15).contains(&prec) {
        let shift = (prec * 4) as u32;
        let extra = (mant << shift) & ((1 << 60) - 1);
        mant >>= 60 - shift;
        if extra | (mant & 1) > 1 << 59 {
            mant += 1;
        }
        mant <<= 60 - shift;
        if mant & (1 << 61) != 0 {
            mant >>= 1;
            exp += 1;
        }
    }
    let hex = if fmt == b'X' { UPPERHEX } else { LOWERHEX };
    if neg {
        dst.push(b'-');
    }
    dst.push(b'0');
    dst.push(fmt);
    dst.push(b'0' + ((mant >> 60) & 1) as u8);
    mant <<= 4;
    if prec < 0 && mant != 0 {
        dst.push(b'.');
        while mant != 0 {
            dst.push(hex[((mant >> 60) & 15) as usize]);
            mant <<= 4;
        }
    } else if prec > 0 {
        dst.push(b'.');
        for _ in 0..prec {
            dst.push(hex[((mant >> 60) & 15) as usize]);
            mant <<= 4;
        }
    }
    dst.push(if fmt == b'x' { b'p' } else { b'P' });
    if exp < 0 {
        dst.push(b'-');
        exp = -exp;
    } else {
        dst.push(b'+');
    }
    if exp < 100 {
        dst.push((exp / 10) as u8 + b'0');
        dst.push((exp % 10) as u8 + b'0');
    } else if exp < 1000 {
        dst.push((exp / 100) as u8 + b'0');
        dst.push(((exp / 10) % 10) as u8 + b'0');
        dst.push((exp % 10) as u8 + b'0');
    } else {
        dst.push((exp / 1000) as u8 + b'0');
        dst.push(((exp / 100) % 10) as u8 + b'0');
        dst.push(((exp / 10) % 10) as u8 + b'0');
        dst.push((exp % 10) as u8 + b'0');
    }
}

/// `strconv.AppendFloat(dst, f, fmt, prec, bitSize)`.
pub fn append_float(dst: &mut Vec<u8>, val: f64, fmt: u8, prec: i64, bit_size: u32) {
    let (bits, expbits, mantbits, bias): (u64, i64, i64, i64) = if bit_size == 32 {
        ((val as f32).to_bits() as u64, 8, 23, -127)
    } else {
        (val.to_bits(), 11, 52, -1023)
    };
    let neg = bits >> (expbits + mantbits) != 0;
    let mut exp = ((bits >> mantbits) & ((1 << expbits) - 1)) as i64;
    let mut mant = bits & ((1u64 << mantbits) - 1);
    if exp == (1 << expbits) - 1 {
        if mant != 0 {
            dst.extend_from_slice(b"NaN");
        } else if neg {
            dst.extend_from_slice(b"-Inf");
        } else {
            dst.extend_from_slice(b"+Inf");
        }
        return;
    }
    if exp == 0 {
        exp += 1;
    } else {
        mant |= 1 << mantbits;
    }
    exp += bias;
    if fmt == b'b' {
        fmt_b(dst, neg, mant, exp - mantbits);
        return;
    }
    if fmt == b'x' || fmt == b'X' {
        fmt_x(dst, prec, fmt, neg, mant, exp, mantbits);
        return;
    }
    if !matches!(fmt, b'e' | b'E' | b'f' | b'g' | b'G') {
        dst.push(b'%');
        dst.push(fmt);
        return;
    }
    if mant == 0 {
        fmt_efg(dst, neg, &[], 0, 0, prec, fmt, prec < 0);
        return;
    }
    if prec < 0 {
        let (digits, dp) = if bit_size == 32 {
            let mut s = String::new();
            let _ = write!(s, "{:e}", (val as f32).abs());
            let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
            let e: i64 = e.parse().unwrap_or(0);
            (m.bytes().filter(|b| b.is_ascii_digit()).collect::<Vec<u8>>(), e + 1)
        } else {
            shortest_digits(val)
        };
        let nd = digits.len() as i64;
        let prec = match fmt {
            b'e' | b'E' => (nd - 1).max(0),
            b'f' => (nd - dp).max(0),
            _ => nd,
        };
        fmt_efg(dst, neg, &digits, dp, nd, prec, fmt, true);
        return;
    }
    let mut d = Decimal::new(mant);
    d.shift(exp - mantbits);
    let mut prec = prec;
    match fmt {
        b'e' | b'E' => d.round(prec.saturating_add(1)),
        b'f' => d.round(d.dp.saturating_add(prec)),
        _ => {
            if prec == 0 {
                prec = 1;
            }
            d.round(prec)
        }
    }
    let nd = d.nd as i64;
    fmt_efg(dst, neg, &d.d[..d.nd], d.dp, nd, prec, fmt, false);
}

/// `strconv.FormatFloat(f, fmt, prec, 64)`.
pub fn format_float(f: f64, fmt: u8, prec: i32) -> String {
    let mut v = Vec::new();
    append_float(&mut v, f, fmt, prec as i64, 64);
    lossy_vec(v)
}

/// `strconv.FormatInt(i, base)` (base 2..=36).
pub fn format_int(i: i64, base: u32) -> String {
    let mut out = String::new();
    if i < 0 {
        out.push('-');
    }
    out.push_str(&format_uint(i.unsigned_abs(), base));
    out
}

/// `strconv.FormatUint(u, base)` (base 2..=36).
pub fn format_uint(mut u: u64, base: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let base = base.clamp(2, 36) as u64;
    let mut buf = [0u8; 65];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = DIGITS[(u % base) as usize];
        u /= base;
        if u == 0 {
            break;
        }
    }
    String::from(core::str::from_utf8(&buf[i..]).unwrap_or(""))
}
