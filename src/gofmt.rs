//! Port of Go's `fmt` formatting (`Sprint`, `Sprintln`, `Sprintf`) and
//! `strconv.FormatFloat` for [`Value`]s.

use alloc::string::String;

use crate::value::Value;

/// `fmt.Sprint(args...)`: operands are separated by a space when neither side
/// is a string.
pub fn sprint(args: &[Value]) -> String {
    let _ = args;
    todo!("gofmt::sprint")
}

/// `fmt.Sprintln(args...)`: always space-separated, trailing newline.
pub fn sprintln(args: &[Value]) -> String {
    let _ = args;
    todo!("gofmt::sprintln")
}

/// `fmt.Sprintf(format, args...)`, including Go's `%!verb(type=value)` error
/// annotations.
pub fn sprintf(format: &str, args: &[Value]) -> String {
    let _ = (format, args);
    todo!("gofmt::sprintf")
}

/// `fmt.Sprint(v)` for a single value (`%v`).
pub fn format_value(v: &Value) -> String {
    sprint(core::slice::from_ref(v))
}

/// `strconv.FormatFloat(f, fmt, prec, 64)`; `fmt` is one of `b'e'`, `b'E'`,
/// `b'f'`, `b'g'`, `b'G'`; `prec == -1` means shortest round-trip.
pub fn format_float(f: f64, fmt: u8, prec: i32) -> String {
    let _ = (f, fmt, prec);
    todo!("gofmt::format_float")
}
