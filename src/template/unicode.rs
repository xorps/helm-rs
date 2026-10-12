//! Approximations of Go's `unicode` predicates using `core`'s tables.
//!
//! Exact for ASCII and the vast majority of real-world input. Differences
//! from Go: `is_letter` also accepts letter numbers (Nl) and other-alphabetic
//! marks; `is_digit` also accepts other numbers (No, Nl).

/// Go `unicode.IsLetter`.
pub fn is_letter(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphabetic();
    }
    c.is_alphabetic()
}

/// Go `unicode.IsDigit`.
pub fn is_digit(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    c.is_numeric()
}

/// Go `unicode.IsPrint`: letters, marks, numbers, punctuation, symbols and
/// the ASCII space.
pub fn is_print(c: char) -> bool {
    if c.is_ascii() {
        return (' '..='~').contains(&c);
    }
    let r = c as u32;
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    // Format characters (Cf), surrogates/private use (Co), noncharacters.
    let cf = matches!(r,
        0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x890..=0x891 | 0x8E2 | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F);
    let private = matches!(r, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD);
    let nonchar = (r & 0xFFFE) == 0xFFFE || (0xFDD0..=0xFDEF).contains(&r);
    !(cf || private || nonchar)
}
