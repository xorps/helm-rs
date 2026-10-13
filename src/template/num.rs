//! Go `strconv.ParseInt`/`ParseUint` (base 0) and `ParseFloat` semantics.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumErr {
    Syntax,
    Range,
}

fn lower(b: u8) -> u8 {
    b | 0x20
}

/// Go's `underscoreOK`: underscores only between digits (base prefix counts
/// as a digit).
pub fn underscore_ok(s: &str) -> bool {
    let mut b = s.as_bytes();
    let mut saw = b'^';
    let mut i = 0;
    if !b.is_empty() && (b[0] == b'-' || b[0] == b'+') {
        b = &b[1..];
    }
    let mut hex = false;
    if b.len() >= 2 && b[0] == b'0' && matches!(lower(b[1]), b'b' | b'o' | b'x') {
        i = 2;
        saw = b'0';
        hex = lower(b[1]) == b'x';
    }
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_digit() || (hex && (b'a'..=b'f').contains(&lower(c))) {
            saw = b'0';
        } else if c == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
        } else {
            if saw == b'_' {
                return false;
            }
            saw = b'!';
        }
        i += 1;
    }
    saw != b'_'
}

/// `strconv.ParseUint(s, 0, 64)`.
pub fn parse_uint0(s0: &str) -> Result<u64, NumErr> {
    if s0.is_empty() {
        return Err(NumErr::Syntax);
    }
    let b = s0.as_bytes();
    let (base, digits): (u32, &[u8]) = if b[0] == b'0' {
        if b.len() >= 3 && lower(b[1]) == b'b' {
            (2, &b[2..])
        } else if b.len() >= 3 && lower(b[1]) == b'o' {
            (8, &b[2..])
        } else if b.len() >= 3 && lower(b[1]) == b'x' {
            (16, &b[2..])
        } else {
            (8, &b[1..])
        }
    } else {
        (10, b)
    };
    let mut n: u64 = 0;
    let mut underscores = false;
    for &c in digits {
        let d = match c {
            b'_' => {
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
        n = match n
            .checked_mul(base as u64)
            .and_then(|n| n.checked_add(d as u64))
        {
            Some(n) => n,
            None => {
                // Keep scanning for syntax errors first? Go returns range error
                // immediately once overflow is detected, but syntax is checked
                // per character as well; match Go: range error.
                return Err(NumErr::Range);
            }
        };
    }
    if underscores && !underscore_ok(s0) {
        return Err(NumErr::Syntax);
    }
    Ok(n)
}

/// `strconv.ParseInt(s, 0, 64)`.
pub fn parse_int0(s: &str) -> Result<i64, NumErr> {
    if s.is_empty() {
        return Err(NumErr::Syntax);
    }
    let (neg, body) = match s.as_bytes()[0] {
        b'+' => (false, &s[1..]),
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    let un = parse_uint0(body)?;
    // Underscore validity is checked against the full string (with sign) by Go.
    if body.contains('_') && !underscore_ok(s) {
        return Err(NumErr::Syntax);
    }
    let cutoff = 1u64 << 63;
    if !neg && un >= cutoff {
        return Err(NumErr::Range);
    }
    if neg && un > cutoff {
        return Err(NumErr::Range);
    }
    Ok(if neg {
        (un as i64).wrapping_neg()
    } else {
        un as i64
    })
}

/// `strconv.ParseFloat(s, 64)`.
pub fn parse_float(s: &str) -> Result<f64, NumErr> {
    let b = s.as_bytes();
    if b.is_empty() {
        return Err(NumErr::Syntax);
    }
    // Special values.
    let (neg, body) = match b[0] {
        b'+' => (false, &s[1..]),
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    let lb = body.to_ascii_lowercase();
    if lb == "inf" || lb == "infinity" {
        return Ok(if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    if lb == "nan" && body.len() == s.len() {
        return Ok(f64::NAN);
    }
    let bb = body.as_bytes();
    if bb.len() >= 2 && bb[0] == b'0' && lower(bb[1]) == b'x' {
        return parse_hex_float(&bb[2..], neg, s);
    }
    // Decimal: [digits][.digits][(e|E)[+-]digits], digits may contain '_'.
    let mut i = 0;
    let mut saw_digits = false;
    let mut underscores = false;
    let mut cleaned = alloc::string::String::with_capacity(bb.len() + 1);
    if neg {
        cleaned.push('-');
    }
    let mut saw_dot = false;
    while i < bb.len() {
        let c = bb[i];
        match c {
            b'_' => underscores = true,
            b'.' if !saw_dot => {
                saw_dot = true;
                cleaned.push('.');
            }
            b'0'..=b'9' => {
                saw_digits = true;
                cleaned.push(c as char);
            }
            _ => break,
        }
        i += 1;
    }
    if !saw_digits {
        return Err(NumErr::Syntax);
    }
    if i < bb.len() && lower(bb[i]) == b'e' {
        cleaned.push('e');
        i += 1;
        if i < bb.len() && (bb[i] == b'+' || bb[i] == b'-') {
            cleaned.push(bb[i] as char);
            i += 1;
        }
        let mut exp_digits = false;
        while i < bb.len() && (bb[i].is_ascii_digit() || bb[i] == b'_') {
            if bb[i] == b'_' {
                underscores = true;
            } else {
                exp_digits = true;
                cleaned.push(bb[i] as char);
            }
            i += 1;
        }
        if !exp_digits {
            return Err(NumErr::Syntax);
        }
    }
    if i != bb.len() {
        return Err(NumErr::Syntax);
    }
    if underscores && !underscore_ok(s) {
        return Err(NumErr::Syntax);
    }
    let f: f64 = cleaned.parse().map_err(|_| NumErr::Syntax)?;
    if f.is_infinite() {
        return Err(NumErr::Range);
    }
    Ok(f)
}

fn parse_hex_float(b: &[u8], neg: bool, full: &str) -> Result<f64, NumErr> {
    // mantissa hex digits with optional '.', then mandatory p exponent.
    let mut mant: u64 = 0;
    let mut exp: i64 = 0;
    let mut saw_digits = false;
    let mut saw_dot = false;
    let mut underscores = false;
    let mut truncated = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'_' {
            underscores = true;
        } else if c == b'.' && !saw_dot {
            saw_dot = true;
        } else if let Some(d) = (c as char).to_digit(16) {
            saw_digits = true;
            if mant >> 60 == 0 {
                mant = (mant << 4) | d as u64;
                if saw_dot {
                    exp -= 4;
                }
            } else {
                if d != 0 {
                    truncated = true;
                }
                if !saw_dot {
                    exp += 4;
                }
            }
        } else {
            break;
        }
        i += 1;
    }
    if !saw_digits || i >= b.len() || lower(b[i]) != b'p' {
        return Err(NumErr::Syntax);
    }
    i += 1;
    let mut esign = 1i64;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        if b[i] == b'-' {
            esign = -1;
        }
        i += 1;
    }
    let mut e: i64 = 0;
    let mut exp_digits = false;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        if b[i] == b'_' {
            underscores = true;
        } else {
            exp_digits = true;
            if e < 100_000 {
                e = e * 10 + (b[i] - b'0') as i64;
            }
        }
        i += 1;
    }
    if !exp_digits || i != b.len() {
        return Err(NumErr::Syntax);
    }
    if underscores && !underscore_ok(full) {
        return Err(NumErr::Syntax);
    }
    if truncated {
        mant |= 1;
    }
    let total_exp = exp + esign * e;
    let mut f = mant as f64;
    // Scale by 2^total_exp in steps to avoid intermediate overflow.
    let mut te = total_exp;
    while te > 0 {
        let step = te.min(1000);
        f *= pow2(step as i32);
        te -= step;
    }
    while te < 0 {
        let step = (-te).min(1000);
        f /= pow2(step as i32);
        te += step;
    }
    if f.is_infinite() {
        return Err(NumErr::Range);
    }
    Ok(if neg { -f } else { f })
}

fn pow2(n: i32) -> f64 {
    let mut r = 1.0f64;
    let mut base = 2.0f64;
    let mut n = n as u32;
    while n > 0 {
        if n & 1 == 1 {
            r *= base;
        }
        base *= base;
        n >>= 1;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints() {
        assert_eq!(parse_int0("0x1F"), Ok(31));
        assert_eq!(parse_int0("-0b101"), Ok(-5));
        assert_eq!(parse_int0("017"), Ok(15));
        assert_eq!(parse_int0("08"), Err(NumErr::Syntax));
        assert_eq!(parse_int0("1_000"), Ok(1000));
        assert_eq!(parse_int0("1__0"), Err(NumErr::Syntax));
        assert_eq!(parse_int0("9223372036854775808"), Err(NumErr::Range));
        assert_eq!(parse_int0("-9223372036854775808"), Ok(i64::MIN));
        assert_eq!(parse_uint0("-1"), Err(NumErr::Syntax));
    }

    #[test]
    fn floats() {
        assert_eq!(parse_float("1.5"), Ok(1.5));
        assert_eq!(parse_float(".5"), Ok(0.5));
        assert_eq!(parse_float("1e3"), Ok(1000.0));
        assert_eq!(parse_float("0x1p-2"), Ok(0.25));
        assert_eq!(parse_float("1_000.5"), Ok(1000.5));
        assert_eq!(parse_float("1e400"), Err(NumErr::Range));
        assert_eq!(parse_float("1e"), Err(NumErr::Syntax));
    }
}
