//! Go's `unicode` predicates and simple case mappings (tables generated from
//! Go 1.27), so string functions agree with Go rune for rune.

use super::unicode_tables::{CASE_RANGES, LETTER, LOWER, NUMBER, PUNCT, TITLE, UPPER, UPPER_LOWER};

fn in_table(t: &[(u32, u32)], r: u32) -> bool {
    let (mut lo, mut hi) = (0usize, t.len());
    while lo < hi {
        let m = (lo + hi) / 2;
        let (a, b) = t[m];
        if r < a {
            hi = m;
        } else if r > b {
            lo = m + 1;
        } else {
            return true;
        }
    }
    false
}

pub fn is_upper(r: char) -> bool {
    let r = r as u32;
    if r < 0x80 {
        return (b'A' as u32..=b'Z' as u32).contains(&r);
    }
    in_table(&UPPER, r)
}

pub fn is_lower(r: char) -> bool {
    let r = r as u32;
    if r < 0x80 {
        return (b'a' as u32..=b'z' as u32).contains(&r);
    }
    in_table(&LOWER, r)
}

pub fn is_title(r: char) -> bool {
    in_table(&TITLE, r as u32)
}

pub fn is_letter(r: char) -> bool {
    let r = r as u32;
    if r < 0x80 {
        let c = r as u8;
        return c.is_ascii_alphabetic();
    }
    in_table(&LETTER, r)
}

pub fn is_number(r: char) -> bool {
    in_table(&NUMBER, r as u32)
}

pub fn is_punct(r: char) -> bool {
    in_table(&PUNCT, r as u32)
}

/// `unicode.IsSpace`.
pub fn is_space(r: char) -> bool {
    match r as u32 {
        0x09..=0x0D | 0x20 | 0x85 | 0xA0 => true,
        r if r < 0x100 => false,
        0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 => true,
        _ => false,
    }
}

const UPPER_CASE: usize = 0;
const LOWER_CASE: usize = 1;
const TITLE_CASE: usize = 2;

fn to(case: usize, r: char) -> char {
    let ru = r as u32;
    let (mut lo, mut hi) = (0usize, CASE_RANGES.len());
    while lo < hi {
        let m = (lo + hi) / 2;
        let (clo, chi, delta) = CASE_RANGES[m];
        if clo <= ru && ru <= chi {
            let d = delta[case];
            if d == UPPER_LOWER {
                let v = clo + (((ru - clo) & !1) | (case as u32 & 1));
                return char::from_u32(v).unwrap_or(r);
            }
            return char::from_u32((ru as i64 + d as i64) as u32).unwrap_or(r);
        }
        if ru < clo {
            hi = m;
        } else {
            lo = m + 1;
        }
    }
    r
}

pub fn to_upper(r: char) -> char {
    if r.is_ascii() {
        return r.to_ascii_uppercase();
    }
    to(UPPER_CASE, r)
}

pub fn to_lower(r: char) -> char {
    if r.is_ascii() {
        return r.to_ascii_lowercase();
    }
    to(LOWER_CASE, r)
}

pub fn to_title(r: char) -> char {
    if r.is_ascii() {
        return r.to_ascii_uppercase();
    }
    to(TITLE_CASE, r)
}

/// A byte reinterpreted as a rune (Go's `rune(str[i])`).
pub fn byte_rune(b: u8) -> char {
    b as char
}

/// `unicode.IsDigit` (category Nd).
pub fn is_digit(r: char) -> bool {
    if r.is_ascii() {
        return r.is_ascii_digit();
    }
    in_table(&super::unicode_tables::DIGIT, r as u32)
}
