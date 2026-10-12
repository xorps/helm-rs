//! Port of Go's `fmt` formatting (`Sprint`, `Sprintln`, `Sprintf`) and
//! `strconv.FormatFloat` for [`Value`]s.
//!
//! TEMPORARY minimal implementation (replaced by the full port).

use alloc::format;
use alloc::string::String;

use crate::value::Value;

fn is_string(v: &Value) -> bool {
    matches!(v, Value::String(_))
}

/// `fmt.Sprint(args...)`: operands are separated by a space when neither side
/// is a string.
pub fn sprint(args: &[Value]) -> String {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 && !is_string(a) && !is_string(&args[i - 1]) {
            out.push(' ');
        }
        write_value(&mut out, a);
    }
    out
}

/// `fmt.Sprintln(args...)`: always space-separated, trailing newline.
pub fn sprintln(args: &[Value]) -> String {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        write_value(&mut out, a);
    }
    out.push('\n');
    out
}

/// `fmt.Sprintf(format, args...)` (minimal: %v %s %d %q %%).
pub fn sprintf(format: &str, args: &[Value]) -> String {
    let mut out = String::new();
    let mut it = format.chars();
    let mut ai = 0;
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('%') => out.push('%'),
            Some(v) => {
                match args.get(ai) {
                    Some(a) => match v {
                        'q' => out.push_str(&crate::template::quote::go_quote(&format_value(a))),
                        _ => write_value(&mut out, a),
                    },
                    None => out.push_str(&format!("%!{v}(MISSING)")),
                }
                ai += 1;
            }
            None => out.push_str("%!(NOVERB)"),
        }
    }
    out
}

/// `fmt.Sprint(v)` for a single value (`%v`).
pub fn format_value(v: &Value) -> String {
    let mut s = String::new();
    write_value(&mut s, v);
    s
}

/// Appends `fmt.Sprint(v)` to `out`.
pub fn write_value(out: &mut String, v: &Value) {
    write_inner(out, v, true)
}

fn write_inner(out: &mut String, v: &Value, top: bool) {
    match v {
        Value::Nil => out.push_str("<nil>"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) | Value::Int64(i) => out.push_str(&format!("{i}")),
        Value::Uint64(u) => out.push_str(&format!("{u}")),
        Value::Float(f) => out.push_str(&format_float(*f, b'g', -1)),
        Value::String(s) => out.push_str(s),
        Value::Bytes(b) => {
            out.push('[');
            for (i, x) in b.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(&format!("{x}"));
            }
            out.push(']');
        }
        Value::List(l) => {
            out.push('[');
            for (i, x) in l.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write_inner(out, x, false);
            }
            out.push(']');
        }
        Value::Map(m) => {
            out.push_str("map[");
            for (i, (k, x)) in m.borrow().iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(k);
                out.push(':');
                write_inner(out, x, false);
            }
            out.push(']');
        }
        Value::Object(o) => o.format(out),
    }
    let _ = top;
}

/// `strconv.FormatFloat(f, fmt, prec, 64)` (minimal: shortest %g).
pub fn format_float(f: f64, fmt: u8, prec: i32) -> String {
    let _ = (fmt, prec);
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let e = format!("{f:e}");
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if !(-4..21).contains(&exp) || exp >= 21 {
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mant}e{sign}{:02}", exp.abs());
    }
    let digits = mant.replace(['.', '-'], "");
    let eprec = if digits.len() > 6 { digits.len() as i32 } else { 6 };
    let _ = eprec;
    if exp < -4 || exp >= 21.min(eprec_shortest(&digits)) {
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mant}e{sign}{:02}", exp.abs());
    }
    format!("{f}")
}

fn eprec_shortest(_digits: &str) -> i32 {
    // shortest %g uses eprec = 6, but if digits > eprec and exp < digits... Go:
    // eprec = 6 when shortest; exponent form if exp < -4 || exp >= eprec,
    // except eprec is raised to digs.nd when digs.nd > eprec && digs.nd >= digs.dp.
    6
}
