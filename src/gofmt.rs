//! Port of Go's `fmt` formatting (`Sprint`, `Sprintln`, `Sprintf`) and
//! `strconv.FormatFloat` for [`Value`]s.
//!
//! The printer follows `fmt/print.go` and `fmt/format.go` (Go 1.27) closely,
//! including the error annotations (`%!d(string=x)`, `%!v(MISSING)`,
//! `%!(EXTRA ...)`, `%!(BADWIDTH)`, ...). Values are treated as the Go types
//! they model: `Value::List` is `[]interface {}`, `Value::Map` is
//! `map[string]interface {}`, `Value::Bytes` is `[]byte`, and so on.
//!
//! Known gaps: `%p` prints a fixed fake address (Go prints a real pointer),
//! and objects ([`crate::Object`]) are treated as `fmt.Stringer`s whose
//! `String()` is [`crate::Object::format`]; other verbs on structs cannot be
//! reproduced field by field.

#![allow(
    clippy::needless_range_loop,
    clippy::collapsible_match,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

use alloc::string::String;
use alloc::vec::Vec;

use crate::value::{Kind, Value};

mod isprint;
pub mod strconv;

/// `fmt.Sprint(args...)`: operands are separated by a space when neither side
/// is a string.
pub fn sprint(args: &[Value]) -> String {
    let mut p = Printer::new();
    let mut prev_string = false;
    for (i, arg) in args.iter().enumerate() {
        let is_string = matches!(arg, Value::String(_))
            || (matches!(arg, Value::Object(o) if o.kind() == Kind::String));
        if i > 0 && !is_string && !prev_string {
            p.buf.push(b' ');
        }
        p.print_arg(arg, 'v');
        prev_string = is_string;
    }
    p.finish()
}

/// `fmt.Sprintln(args...)`: always space-separated, trailing newline.
pub fn sprintln(args: &[Value]) -> String {
    let mut p = Printer::new();
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            p.buf.push(b' ');
        }
        p.print_arg(arg, 'v');
    }
    p.buf.push(b'\n');
    p.finish()
}

/// `fmt.Sprintf(format, args...)`, including Go's `%!verb(type=value)` error
/// annotations.
pub fn sprintf(format: &str, args: &[Value]) -> String {
    let mut p = Printer::new();
    p.do_printf(format, args);
    p.finish()
}

/// `fmt.Sprint(v)` for a single value (`%v`).
pub fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => String::from(&**s),
        _ => sprint(core::slice::from_ref(v)),
    }
}

/// Appends `fmt.Sprint(v)` (`%v` of a single value) to `out`.
pub fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::String(s) => out.push_str(s),
        _ => out.push_str(&sprint(core::slice::from_ref(v))),
    }
}

/// `strconv.FormatFloat(f, fmt, prec, 64)`; `fmt` is one of `b'e'`, `b'E'`,
/// `b'f'`, `b'g'`, `b'G'` (also `b'b'`, `b'x'`, `b'X'`); `prec == -1` means
/// shortest round-trip.
pub fn format_float(f: f64, fmt: u8, prec: i32) -> String {
    strconv::format_float(f, fmt, prec)
}

/// Go `%T` of a value (`reflect.TypeOf(v).String()`).
pub fn type_string(v: &Value) -> String {
    v.type_name()
}

const LDIGITS: &[u8; 17] = b"0123456789abcdefx";
const UDIGITS: &[u8; 17] = b"0123456789ABCDEFX";

#[derive(Clone, Copy, Default)]
struct Flags {
    wid_present: bool,
    prec_present: bool,
    minus: bool,
    plus: bool,
    sharp: bool,
    space: bool,
    zero: bool,
    plus_v: bool,
    sharp_v: bool,
}

/// The `time.Duration` downcast hook: durations print as integers for
/// numeric verbs.
/// An integer-kind object: `time.Duration` (signed) or `uint8` (unsigned).
fn int_object(v: &Value) -> Option<(u64, bool)> {
    if let Some(n) = duration_nanos(v) {
        return Some((n as u64, true));
    }
    match v {
        Value::Object(o) => o
            .as_any()
            .downcast_ref::<crate::value::GoUint8>()
            .map(|b| (b.0 as u64, false)),
        _ => None,
    }
}

fn duration_nanos(v: &Value) -> Option<i64> {
    match v {
        Value::Object(o) => o
            .as_any()
            .downcast_ref::<crate::funcs::duration::Duration>()
            .map(|d| d.0),
        _ => None,
    }
}

/// The Go struct behind a known object, for `%#v`, `%+v` and non-string
/// verbs (fields are printed like Go's `printValue` does).
struct StructView {
    ptr: bool,
    type_name: &'static str,
    fields: Vec<(&'static str, Value)>,
}

fn struct_view(v: &Value) -> Option<StructView> {
    let Value::Object(o) = v else { return None };
    if let Some(vo) = o.as_any().downcast_ref::<crate::semver::VersionObject>() {
        let ver = &vo.version;
        return Some(StructView {
            ptr: !vo.by_value,
            type_name: "semver.Version",
            fields: alloc::vec![
                ("major", Value::Uint64(ver.major)),
                ("minor", Value::Uint64(ver.minor)),
                ("patch", Value::Uint64(ver.patch)),
                ("pre", Value::str(&ver.pre)),
                ("metadata", Value::str(&ver.metadata)),
                ("original", Value::str(&ver.original)),
            ],
        });
    }
    if let Some(c) = o
        .as_any()
        .downcast_ref::<crate::funcs::sprig::certs::Certificate>()
    {
        return Some(StructView {
            ptr: false,
            type_name: "sprig.certificate",
            fields: alloc::vec![("Cert", Value::str(&c.cert)), ("Key", Value::str(&c.key))],
        });
    }
    let gs = o.go_struct()?;
    Some(StructView {
        ptr: gs.ptr,
        type_name: gs.type_name,
        fields: gs.fields.into_iter().map(|f| (f.name, f.value)).collect(),
    })
}

/// Whether an object implements `fmt.Stringer` (unknown objects are assumed
/// to, since [`crate::Object::format`] is their `%v`).
fn is_stringer(v: &Value) -> bool {
    match v {
        Value::Object(o) => {
            !o.as_any().is::<crate::funcs::sprig::certs::Certificate>()
                && !matches!(o.kind(), Kind::Map | Kind::Slice)
                && match o.stringer() {
                    crate::value::Stringer::None => false,
                    crate::value::Stringer::Value => true,
                    crate::value::Stringer::Pointer => o.go_struct().is_some_and(|g| g.ptr),
                }
        }
        _ => false,
    }
}

struct Printer {
    buf: Vec<u8>,
    f: Flags,
    wid: i64,
    prec: i64,
    reordered: bool,
    good_arg_num: bool,
    erroring: bool,
}

fn too_large(x: i64) -> bool {
    const MAX: i64 = 1_000_000;
    !(-MAX..=MAX).contains(&x)
}

/// `parsenum`: returns (num, isnum, newi).
fn parsenum(s: &[u8], start: usize, end: usize) -> (i64, bool, usize) {
    if start >= end {
        return (0, false, end);
    }
    let mut num = 0i64;
    let mut isnum = false;
    let mut newi = start;
    while newi < end && s[newi].is_ascii_digit() {
        if too_large(num) {
            return (0, false, end);
        }
        num = num * 10 + (s[newi] - b'0') as i64;
        isnum = true;
        newi += 1;
    }
    (num, isnum, newi)
}

fn int_from_arg(a: &[Value], arg_num: usize) -> (i64, bool, usize) {
    let mut new_arg_num = arg_num;
    let mut num = 0;
    let mut is_int = false;
    if arg_num < a.len() {
        match &a[arg_num] {
            Value::Int(n) | Value::Int64(n) => {
                num = *n;
                is_int = true;
            }
            Value::Uint64(n) => {
                if *n <= i64::MAX as u64 {
                    num = *n as i64;
                    is_int = true;
                }
            }
            v => {
                if let Some((n, _)) = int_object(v) {
                    num = n as i64;
                    is_int = true;
                }
            }
        }
        new_arg_num = arg_num + 1;
        if too_large(num) {
            num = 0;
            is_int = false;
        }
    }
    (num, is_int, new_arg_num)
}

/// `parseArgNumber`: (index, wid, ok).
fn parse_arg_number(format: &[u8]) -> (i64, usize, bool) {
    if format.len() < 3 {
        return (0, 1, false);
    }
    for i in 1..format.len() {
        if format[i] == b']' {
            let (width, ok, newi) = parsenum(format, 1, i);
            if !ok || newi != i {
                return (0, i + 1, false);
            }
            return (width - 1, i + 1, true);
        }
    }
    (0, 1, false)
}

impl Printer {
    fn new() -> Printer {
        Printer {
            buf: Vec::new(),
            f: Flags::default(),
            wid: 0,
            prec: 0,
            reordered: false,
            good_arg_num: true,
            erroring: false,
        }
    }

    fn finish(self) -> String {
        strconv::lossy_vec(self.buf)
    }

    fn clearflags(&mut self) {
        self.f = Flags::default();
        self.wid = 0;
        self.prec = 0;
    }

    fn write_rune(&mut self, r: char) {
        let mut b = [0u8; 4];
        self.buf.extend_from_slice(r.encode_utf8(&mut b).as_bytes());
    }

    // ----- format.go -----

    fn write_padding(&mut self, n: i64) {
        if n <= 0 {
            return;
        }
        let pad = if self.f.zero && !self.f.minus {
            b'0'
        } else {
            b' '
        };
        self.buf.resize(self.buf.len() + n as usize, pad);
    }

    fn pad(&mut self, b: &[u8]) {
        if !self.f.wid_present || self.wid == 0 {
            self.buf.extend_from_slice(b);
            return;
        }
        let width = self.wid - strconv::rune_count(b) as i64;
        if !self.f.minus {
            self.write_padding(width);
            self.buf.extend_from_slice(b);
        } else {
            self.buf.extend_from_slice(b);
            self.write_padding(width);
        }
    }

    fn pad_string(&mut self, s: &str) {
        self.pad(s.as_bytes())
    }

    fn fmt_boolean(&mut self, v: bool) {
        self.pad_string(if v { "true" } else { "false" });
    }

    fn fmt_unicode(&mut self, u: u64) {
        let mut prec: i64 = 4;
        if self.f.prec_present && self.prec > 4 {
            prec = self.prec;
        }
        // Build right-to-left.
        let mut rev: Vec<u8> = Vec::new();
        if self.f.sharp && u <= strconv::MAX_RUNE as u64 && strconv::is_print(u as u32) {
            rev.push(b'\'');
            let mut tmp = Vec::new();
            strconv::push_rune(&mut tmp, u as u32);
            rev.extend(tmp.iter().rev());
            rev.push(b'\'');
            rev.push(b' ');
        }
        let mut u = u;
        while u >= 16 {
            rev.push(UDIGITS[(u & 0xF) as usize]);
            prec -= 1;
            u >>= 4;
        }
        rev.push(UDIGITS[u as usize]);
        prec -= 1;
        while prec > 0 {
            rev.push(b'0');
            prec -= 1;
        }
        rev.push(b'+');
        rev.push(b'U');
        rev.reverse();
        let old_zero = self.f.zero;
        self.f.zero = false;
        self.pad(&rev);
        self.f.zero = old_zero;
    }

    fn fmt_integer(&mut self, u: u64, base: u32, is_signed: bool, verb: char, digits: &[u8; 17]) {
        let negative = is_signed && (u as i64) < 0;
        let mut u = if negative {
            (u as i64).wrapping_neg() as u64
        } else {
            u
        };
        let mut prec: i64 = 0;
        if self.f.prec_present {
            prec = self.prec;
            if prec == 0 && u == 0 {
                let old_zero = self.f.zero;
                self.f.zero = false;
                let w = self.wid;
                self.write_padding(w);
                self.f.zero = old_zero;
                return;
            }
        } else if self.f.zero && !self.f.minus && self.f.wid_present {
            prec = self.wid;
            if negative || self.f.plus || self.f.space {
                prec -= 1;
            }
        }
        let mut rev: Vec<u8> = Vec::with_capacity(24);
        match base {
            10 => {
                while u >= 10 {
                    let next = u / 10;
                    rev.push(b'0' + (u - next * 10) as u8);
                    u = next;
                }
            }
            16 => {
                while u >= 16 {
                    rev.push(digits[(u & 0xF) as usize]);
                    u >>= 4;
                }
            }
            8 => {
                while u >= 8 {
                    rev.push(b'0' + (u & 7) as u8);
                    u >>= 3;
                }
            }
            _ => {
                while u >= 2 {
                    rev.push(b'0' + (u & 1) as u8);
                    u >>= 1;
                }
            }
        }
        rev.push(digits[u as usize]);
        while prec > rev.len() as i64 {
            rev.push(b'0');
        }
        if self.f.sharp {
            match base {
                2 => {
                    rev.push(b'b');
                    rev.push(b'0');
                }
                8 => {
                    if *rev.last().unwrap_or(&b'0') != b'0' {
                        rev.push(b'0');
                    }
                }
                16 => {
                    rev.push(digits[16]);
                    rev.push(b'0');
                }
                _ => {}
            }
        }
        if verb == 'O' {
            rev.push(b'o');
            rev.push(b'0');
        }
        if negative {
            rev.push(b'-');
        } else if self.f.plus {
            rev.push(b'+');
        } else if self.f.space {
            rev.push(b' ');
        }
        rev.reverse();
        let old_zero = self.f.zero;
        self.f.zero = false;
        self.pad(&rev);
        self.f.zero = old_zero;
    }

    fn truncate<'a>(&self, b: &'a [u8]) -> &'a [u8] {
        if self.f.prec_present {
            let mut n = self.prec;
            let mut i = 0;
            while i < b.len() {
                n -= 1;
                if n < 0 {
                    return &b[..i];
                }
                let (_, w) = strconv::decode_rune(&b[i..]);
                i += w;
            }
        }
        b
    }

    fn fmt_s(&mut self, s: &[u8]) {
        let s = self.truncate(s);
        self.pad(s);
    }

    fn fmt_sbx(&mut self, b: &[u8], digits: &[u8; 17]) {
        let mut length = b.len() as i64;
        if self.f.prec_present && self.prec < length {
            length = self.prec;
        }
        let mut width = 2 * length;
        if width > 0 {
            if self.f.space {
                if self.f.sharp {
                    width *= 2;
                }
                width += length - 1;
            } else if self.f.sharp {
                width += 2;
            }
        } else {
            if self.f.wid_present {
                let w = self.wid;
                self.write_padding(w);
            }
            return;
        }
        if self.f.wid_present && self.wid > width && !self.f.minus {
            let w = self.wid - width;
            self.write_padding(w);
        }
        if self.f.sharp {
            self.buf.push(b'0');
            self.buf.push(digits[16]);
        }
        for i in 0..length as usize {
            if self.f.space && i > 0 {
                self.buf.push(b' ');
                if self.f.sharp {
                    self.buf.push(b'0');
                    self.buf.push(digits[16]);
                }
            }
            let c = b[i];
            self.buf.push(digits[(c >> 4) as usize]);
            self.buf.push(digits[(c & 0xF) as usize]);
        }
        if self.f.wid_present && self.wid > width && self.f.minus {
            let w = self.wid - width;
            self.write_padding(w);
        }
    }

    fn fmt_q(&mut self, s: &[u8]) {
        let s = self.truncate(s);
        if self.f.sharp && strconv::can_backquote(s) {
            let mut v = Vec::with_capacity(s.len() + 2);
            v.push(b'`');
            v.extend_from_slice(s);
            v.push(b'`');
            self.pad(&v);
            return;
        }
        let mut v = Vec::with_capacity(s.len() + 2);
        strconv::append_quoted(&mut v, s, b'"', self.f.plus);
        self.pad(&v);
    }

    fn fmt_c(&mut self, c: u64) {
        let r = if c > strconv::MAX_RUNE as u64 {
            strconv::RUNE_ERROR
        } else {
            c as u32
        };
        let mut v = Vec::new();
        strconv::push_rune(&mut v, r);
        self.pad(&v);
    }

    fn fmt_qc(&mut self, c: u64) {
        let r = if c > strconv::MAX_RUNE as u64 {
            strconv::RUNE_ERROR
        } else {
            c as u32
        };
        let mut v = Vec::new();
        strconv::append_quoted_rune(&mut v, r, self.f.plus);
        self.pad(&v);
    }

    fn fmt_float_raw(&mut self, v: f64, size: u32, verb: u8, prec: i64) {
        let prec = if self.f.prec_present { self.prec } else { prec };
        let mut num: Vec<u8> = alloc::vec![b'+'];
        strconv::append_float(&mut num, v, verb, prec, size);
        if num[1] == b'-' || num[1] == b'+' {
            num.remove(0);
        } else {
            num[0] = b'+';
        }
        if self.f.space && num[0] == b'+' && !self.f.plus {
            num[0] = b' ';
        }
        if num[1] == b'I' || num[1] == b'N' {
            let old_zero = self.f.zero;
            self.f.zero = false;
            if num[1] == b'N' && !self.f.space && !self.f.plus {
                num.remove(0);
            }
            self.pad(&num);
            self.f.zero = old_zero;
            return;
        }
        if self.f.sharp && verb != b'b' {
            let mut digits: i64 = 0;
            if matches!(verb, b'v' | b'g' | b'G' | b'x') {
                digits = prec;
                if digits == -1 {
                    digits = 6;
                }
            }
            let mut tail: Vec<u8> = Vec::new();
            let mut has_decimal_point = false;
            let mut saw_nonzero_digit = false;
            let mut i = 1;
            while i < num.len() {
                match num[i] {
                    b'.' => has_decimal_point = true,
                    b'p' | b'P' => {
                        tail.extend_from_slice(&num[i..]);
                        num.truncate(i);
                        break;
                    }
                    b'e' | b'E' if verb != b'x' && verb != b'X' => {
                        tail.extend_from_slice(&num[i..]);
                        num.truncate(i);
                        break;
                    }
                    c => {
                        if c != b'0' {
                            saw_nonzero_digit = true;
                        }
                        if saw_nonzero_digit {
                            digits -= 1;
                        }
                    }
                }
                i += 1;
            }
            if !has_decimal_point {
                if num.len() == 2 && num[1] == b'0' {
                    digits -= 1;
                }
                num.push(b'.');
            }
            while digits > 0 {
                num.push(b'0');
                digits -= 1;
            }
            num.extend_from_slice(&tail);
        }
        if self.f.plus || num[0] != b'+' {
            if self.f.zero && !self.f.minus && self.f.wid_present && self.wid > num.len() as i64 {
                self.buf.push(num[0]);
                let w = self.wid - num.len() as i64;
                self.write_padding(w);
                self.buf.extend_from_slice(&num[1..]);
                return;
            }
            self.pad(&num);
            return;
        }
        self.pad(&num[1..]);
    }

    // ----- print.go -----

    /// `badVerb`. `arg` is the top-level argument (if any); `value` is the
    /// reflect value being printed (if any). Either determines the type.
    fn bad_verb(&mut self, arg: Option<&Value>, value: Option<&Value>, verb: char) {
        self.erroring = true;
        self.buf.extend_from_slice(b"%!");
        self.write_rune(verb);
        self.buf.push(b'(');
        match (arg, value) {
            (Some(a), _) if !a.is_nil() => {
                self.buf.extend_from_slice(a.type_name().as_bytes());
                self.buf.push(b'=');
                self.print_arg(a, 'v');
            }
            (_, Some(v)) if !v.is_nil() => {
                self.buf.extend_from_slice(v.type_name().as_bytes());
                self.buf.push(b'=');
                self.print_value(v, 'v', 0);
            }
            _ => self.buf.extend_from_slice(b"<nil>"),
        }
        self.buf.push(b')');
        self.erroring = false;
    }

    /// `badVerb` for a `uint8` element of a byte slice.
    fn bad_verb_u8(&mut self, b: u8, verb: char) {
        self.erroring = true;
        self.buf.extend_from_slice(b"%!");
        self.write_rune(verb);
        self.buf.extend_from_slice(b"(uint8=");
        self.fmt_integer(b as u64, 10, false, 'v', LDIGITS);
        self.buf.push(b')');
        self.erroring = false;
    }

    fn fmt_bool(&mut self, arg: Option<&Value>, value: Option<&Value>, v: bool, verb: char) {
        match verb {
            't' | 'v' => self.fmt_boolean(v),
            _ => self.bad_verb(arg, value, verb),
        }
    }

    fn fmt_0x64(&mut self, v: u64, leading0x: bool) {
        let sharp = self.f.sharp;
        self.f.sharp = leading0x;
        self.fmt_integer(v, 16, false, 'v', LDIGITS);
        self.f.sharp = sharp;
    }

    /// Returns false if the verb is bad (caller reports).
    fn fmt_integer_verb(&mut self, v: u64, is_signed: bool, verb: char) -> bool {
        match verb {
            'v' => {
                if self.f.sharp_v && !is_signed {
                    self.fmt_0x64(v, true);
                } else {
                    self.fmt_integer(v, 10, is_signed, verb, LDIGITS);
                }
            }
            'd' => self.fmt_integer(v, 10, is_signed, verb, LDIGITS),
            'b' => self.fmt_integer(v, 2, is_signed, verb, LDIGITS),
            'o' | 'O' => self.fmt_integer(v, 8, is_signed, verb, LDIGITS),
            'x' => self.fmt_integer(v, 16, is_signed, verb, LDIGITS),
            'X' => self.fmt_integer(v, 16, is_signed, verb, UDIGITS),
            'c' => self.fmt_c(v),
            'q' => self.fmt_qc(v),
            'U' => self.fmt_unicode(v),
            _ => return false,
        }
        true
    }

    fn fmt_int(
        &mut self,
        arg: Option<&Value>,
        value: Option<&Value>,
        v: u64,
        is_signed: bool,
        verb: char,
    ) {
        if !self.fmt_integer_verb(v, is_signed, verb) {
            self.bad_verb(arg, value, verb);
        }
    }

    fn fmt_float(
        &mut self,
        arg: Option<&Value>,
        value: Option<&Value>,
        v: f64,
        size: u32,
        verb: char,
    ) {
        match verb {
            'v' => self.fmt_float_raw(v, size, b'g', -1),
            'b' | 'g' | 'G' | 'x' | 'X' => self.fmt_float_raw(v, size, verb as u8, -1),
            'f' | 'e' | 'E' => self.fmt_float_raw(v, size, verb as u8, 6),
            'F' => self.fmt_float_raw(v, size, b'f', 6),
            _ => self.bad_verb(arg, value, verb),
        }
    }

    fn fmt_string(&mut self, arg: Option<&Value>, value: Option<&Value>, v: &[u8], verb: char) {
        match verb {
            'v' => {
                if self.f.sharp_v {
                    self.fmt_q(v)
                } else {
                    self.fmt_s(v)
                }
            }
            's' => self.fmt_s(v),
            'x' => self.fmt_sbx(v, LDIGITS),
            'X' => self.fmt_sbx(v, UDIGITS),
            'q' => self.fmt_q(v),
            _ => self.bad_verb(arg, value, verb),
        }
    }

    /// `fmtBytes`.
    fn fmt_bytes(&mut self, v: &[u8], verb: char, type_string: &str) {
        match verb {
            'v' | 'd' => {
                if self.f.sharp_v {
                    self.buf.extend_from_slice(type_string.as_bytes());
                    self.buf.push(b'{');
                    for (i, &c) in v.iter().enumerate() {
                        if i > 0 {
                            self.buf.extend_from_slice(b", ");
                        }
                        self.fmt_0x64(c as u64, true);
                    }
                    self.buf.push(b'}');
                } else {
                    self.buf.push(b'[');
                    for (i, &c) in v.iter().enumerate() {
                        if i > 0 {
                            self.buf.push(b' ');
                        }
                        self.fmt_integer(c as u64, 10, false, verb, LDIGITS);
                    }
                    self.buf.push(b']');
                }
            }
            's' => self.fmt_s(v),
            'x' => self.fmt_sbx(v, LDIGITS),
            'X' => self.fmt_sbx(v, UDIGITS),
            'q' => self.fmt_q(v),
            _ => self.print_byte_slice_value(v, verb),
        }
    }

    /// `printValue` of a `[]uint8` for verbs other than s/q/x/X.
    fn print_byte_slice_value(&mut self, v: &[u8], verb: char) {
        if self.f.sharp_v {
            self.buf.extend_from_slice(b"[]uint8{");
            for (i, &c) in v.iter().enumerate() {
                if i > 0 {
                    self.buf.extend_from_slice(b", ");
                }
                self.print_u8(c, verb);
            }
            self.buf.push(b'}');
        } else {
            self.buf.push(b'[');
            for (i, &c) in v.iter().enumerate() {
                if i > 0 {
                    self.buf.push(b' ');
                }
                self.print_u8(c, verb);
            }
            self.buf.push(b']');
        }
    }

    fn print_u8(&mut self, c: u8, verb: char) {
        if !self.fmt_integer_verb(c as u64, false, verb) {
            self.bad_verb_u8(c, verb);
        }
    }

    fn fmt_pointer(&mut self, arg: Option<&Value>, value: &Value, verb: char) {
        // Only reference kinds have addresses; we cannot print real ones.
        let is_ref = matches!(value, Value::Map(_) | Value::List(_) | Value::Bytes(_))
            || matches!(value, Value::Object(o) if o.kind() == Kind::Ptr);
        if !is_ref {
            self.bad_verb(arg, Some(value), verb);
            return;
        }
        let u: u64 = 0xc000010000;
        match verb {
            'v' => {
                if self.f.sharp_v {
                    self.buf.push(b'(');
                    self.buf.extend_from_slice(value.type_name().as_bytes());
                    self.buf.extend_from_slice(b")(");
                    self.fmt_0x64(u, true);
                    self.buf.push(b')');
                } else {
                    let sharp = !self.f.sharp;
                    self.fmt_0x64(u, sharp);
                }
            }
            'p' => {
                let sharp = !self.f.sharp;
                self.fmt_0x64(u, sharp);
            }
            'b' | 'o' | 'd' | 'x' | 'X' => self.fmt_int(arg, Some(value), u, false, verb),
            _ => self.bad_verb(arg, Some(value), verb),
        }
    }

    /// `handleMethods`: objects are treated as `fmt.Stringer`s.
    fn handle_methods(&mut self, arg: &Value, verb: char) -> bool {
        if self.erroring {
            return false;
        }
        if verb == 'w' {
            // Sprintf never wraps errors: %w is always a bad verb.
            self.bad_verb(Some(arg), None, verb);
            return true;
        }
        let Value::Object(o) = arg else { return false };
        if self.f.sharp_v {
            if int_object(arg).is_some() || struct_view(arg).is_some() {
                return false;
            }
            // GoStringer (time.Time) or best effort.
            let s = match o.as_any().downcast_ref::<crate::funcs::sprig::time::Time>() {
                Some(t) => t.go_syntax(),
                None => {
                    let mut s = String::new();
                    o.format(&mut s);
                    s
                }
            };
            self.fmt_s(s.as_bytes());
            return true;
        }
        if !is_stringer(arg) {
            return false;
        }
        match verb {
            'v' | 's' | 'x' | 'X' | 'q' => {
                let mut s = String::new();
                o.format(&mut s);
                self.fmt_string(Some(arg), None, s.as_bytes(), verb);
                true
            }
            _ => false,
        }
    }

    fn print_arg(&mut self, arg: &Value, verb: char) {
        if arg.is_nil() {
            match verb {
                'T' | 'v' => self.pad_string("<nil>"),
                _ => self.bad_verb(None, None, verb),
            }
            return;
        }
        match verb {
            'T' => {
                let t = arg.type_name();
                self.fmt_s(t.as_bytes());
                return;
            }
            'p' => {
                self.fmt_pointer(Some(arg), arg, 'p');
                return;
            }
            _ => {}
        }
        match arg {
            Value::Nil => {}
            Value::Bool(b) => self.fmt_bool(Some(arg), None, *b, verb),
            Value::Float(f) => self.fmt_float(Some(arg), None, *f, 64, verb),
            Value::Int(i) | Value::Int64(i) => self.fmt_int(Some(arg), None, *i as u64, true, verb),
            Value::Uint64(u) => self.fmt_int(Some(arg), None, *u, false, verb),
            Value::String(s) => self.fmt_string(Some(arg), None, s.as_bytes(), verb),
            Value::Bytes(b) => self.fmt_bytes(b, verb, "[]byte"),
            Value::List(_) | Value::Map(_) | Value::Object(_) => {
                if !self.handle_methods(arg, verb) {
                    self.print_value(arg, verb, 0);
                }
            }
        }
    }

    /// Prints an element of a container (an `interface{}` slot).
    fn print_elem(&mut self, v: &Value, verb: char, depth: usize) {
        if v.is_nil() {
            if self.f.sharp_v {
                self.buf.extend_from_slice(b"interface {}(nil)");
            } else {
                self.buf.extend_from_slice(b"<nil>");
            }
        } else {
            self.print_value(v, verb, depth + 1);
        }
    }

    fn print_value(&mut self, value: &Value, verb: char, depth: usize) {
        if depth > 200 {
            // Self-referential maps; Go would overflow its stack.
            self.buf.extend_from_slice(b"...");
            return;
        }
        if depth > 0 && self.handle_methods(value, verb) {
            return;
        }
        match value {
            Value::Nil => {
                if depth == 0 {
                    self.buf.extend_from_slice(b"<invalid reflect.Value>");
                } else if verb == 'v' {
                    self.buf.extend_from_slice(b"<nil>");
                } else {
                    self.bad_verb(None, None, verb);
                }
            }
            Value::Bool(b) => self.fmt_bool(None, Some(value), *b, verb),
            Value::Int(i) | Value::Int64(i) => {
                self.fmt_int(None, Some(value), *i as u64, true, verb)
            }
            Value::Uint64(u) => self.fmt_int(None, Some(value), *u, false, verb),
            Value::Float(f) => self.fmt_float(None, Some(value), *f, 64, verb),
            Value::String(s) => self.fmt_string(None, Some(value), s.as_bytes(), verb),
            Value::Bytes(b) => match verb {
                's' | 'q' | 'x' | 'X' => self.fmt_bytes(b, verb, "[]uint8"),
                _ => self.print_byte_slice_value(b, verb),
            },
            Value::List(l) => {
                if self.f.sharp_v {
                    self.buf.extend_from_slice(b"[]interface {}{");
                    for (i, e) in l.iter().enumerate() {
                        if i > 0 {
                            self.buf.extend_from_slice(b", ");
                        }
                        self.print_elem(e, verb, depth);
                    }
                    self.buf.push(b'}');
                } else {
                    self.buf.push(b'[');
                    for (i, e) in l.iter().enumerate() {
                        if i > 0 {
                            self.buf.push(b' ');
                        }
                        self.print_elem(e, verb, depth);
                    }
                    self.buf.push(b']');
                }
            }
            Value::Map(m) => {
                let entries: Vec<(String, Value)> = m
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if self.f.sharp_v {
                    self.buf.extend_from_slice(b"map[string]interface {}{");
                } else {
                    self.buf.extend_from_slice(b"map[");
                }
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        if self.f.sharp_v {
                            self.buf.extend_from_slice(b", ");
                        } else {
                            self.buf.push(b' ');
                        }
                    }
                    let key = Value::str(k);
                    self.print_value(&key, verb, depth + 1);
                    self.buf.push(b':');
                    self.print_elem(v, verb, depth);
                }
                if self.f.sharp_v {
                    self.buf.push(b'}');
                } else {
                    self.buf.push(b']');
                }
            }
            Value::Object(o) => {
                if let Some((n, signed)) = int_object(value) {
                    self.fmt_int(None, Some(value), n, signed, verb);
                    return;
                }
                if let Some(sv) = struct_view(value) {
                    if sv.ptr {
                        if depth > 0 {
                            // Go's fmtPointer: the address for %v, a bad-verb
                            // annotation (`%!s(*T=&{...})`) otherwise.
                            self.fmt_pointer(Some(value), value, verb);
                            return;
                        }
                        self.buf.push(b'&');
                    }
                    if self.f.sharp_v {
                        self.buf.extend_from_slice(sv.type_name.as_bytes());
                    }
                    self.buf.push(b'{');
                    for (i, (name, fv)) in sv.fields.iter().enumerate() {
                        if i > 0 {
                            if self.f.sharp_v {
                                self.buf.extend_from_slice(b", ");
                            } else {
                                self.buf.push(b' ');
                            }
                        }
                        if self.f.plus_v || self.f.sharp_v {
                            self.buf.extend_from_slice(name.as_bytes());
                            self.buf.push(b':');
                        }
                        self.print_value(fv, verb, depth + 1);
                    }
                    self.buf.push(b'}');
                    return;
                }
                if matches!(o.kind(), Kind::Map | Kind::Slice) {
                    // Named map/slice types (`.Files`, `[]string`, ...)
                    // format like their underlying data, verb included.
                    let data = match o.to_data() {
                        Value::Nil if o.kind() == Kind::Map => Value::empty_map(),
                        d => d,
                    };
                    self.print_value(&data, verb, depth);
                    return;
                }
                if verb == 'v' {
                    let mut s = String::new();
                    o.format(&mut s);
                    self.buf.extend_from_slice(s.as_bytes());
                } else {
                    self.bad_verb(None, Some(value), verb);
                }
            }
        }
    }

    fn arg_number(
        &mut self,
        arg_num: usize,
        format: &[u8],
        i: usize,
        num_args: usize,
    ) -> (usize, usize, bool) {
        if format.len() <= i || format[i] != b'[' {
            return (arg_num, i, false);
        }
        self.reordered = true;
        let (index, wid, ok) = parse_arg_number(&format[i..]);
        if ok && 0 <= index && (index as usize) < num_args {
            return (index as usize, i + wid, true);
        }
        self.good_arg_num = false;
        (arg_num, i + wid, ok)
    }

    fn do_printf(&mut self, format_str: &str, a: &[Value]) {
        let format = format_str.as_bytes();
        let end = format.len();
        let mut arg_num: usize = 0;
        let mut after_index;
        self.reordered = false;
        let mut i = 0;
        'format_loop: while i < end {
            self.good_arg_num = true;
            let lasti = i;
            while i < end && format[i] != b'%' {
                i += 1;
            }
            if i > lasti {
                self.buf.extend_from_slice(&format[lasti..i]);
            }
            if i >= end {
                break;
            }
            i += 1;
            self.clearflags();
            'simple: while i < end {
                let c = format[i];
                match c {
                    b'#' => self.f.sharp = true,
                    b'0' => self.f.zero = true,
                    b'+' => self.f.plus = true,
                    b'-' => self.f.minus = true,
                    b' ' => self.f.space = true,
                    _ => {
                        if c.is_ascii_lowercase() && arg_num < a.len() {
                            if c == b'w' || c == b'v' {
                                self.f.sharp_v = self.f.sharp;
                                self.f.sharp = false;
                                self.f.plus_v = self.f.plus;
                                self.f.plus = false;
                            }
                            self.print_arg(&a[arg_num], c as char);
                            arg_num += 1;
                            i += 1;
                            continue 'format_loop;
                        }
                        break 'simple;
                    }
                }
                i += 1;
            }

            (arg_num, i, after_index) = self.arg_number(arg_num, format, i, a.len());

            if i < end && format[i] == b'*' {
                i += 1;
                let (w, ok, n) = int_from_arg(a, arg_num);
                self.wid = w;
                self.f.wid_present = ok;
                arg_num = n;
                if !self.f.wid_present {
                    self.buf.extend_from_slice(b"%!(BADWIDTH)");
                }
                if self.wid < 0 {
                    self.wid = -self.wid;
                    self.f.minus = true;
                    self.f.zero = false;
                }
                after_index = false;
            } else {
                let (w, ok, ni) = parsenum(format, i, end);
                self.wid = w;
                self.f.wid_present = ok;
                i = ni;
                if after_index && self.f.wid_present {
                    self.good_arg_num = false;
                }
            }

            if i + 1 < end && format[i] == b'.' {
                i += 1;
                if after_index {
                    self.good_arg_num = false;
                }
                (arg_num, i, after_index) = self.arg_number(arg_num, format, i, a.len());
                if i < end && format[i] == b'*' {
                    i += 1;
                    let (p, ok, n) = int_from_arg(a, arg_num);
                    self.prec = p;
                    self.f.prec_present = ok;
                    arg_num = n;
                    if self.prec < 0 {
                        self.prec = 0;
                        self.f.prec_present = false;
                    }
                    if !self.f.prec_present {
                        self.buf.extend_from_slice(b"%!(BADPREC)");
                    }
                    after_index = false;
                } else {
                    let (p, ok, ni) = parsenum(format, i, end);
                    self.prec = p;
                    self.f.prec_present = ok;
                    i = ni;
                    if !self.f.prec_present {
                        self.prec = 0;
                        self.f.prec_present = true;
                    }
                }
            }

            if !after_index {
                (arg_num, i, after_index) = self.arg_number(arg_num, format, i, a.len());
            }
            let _ = after_index;

            if i >= end {
                self.buf.extend_from_slice(b"%!(NOVERB)");
                break;
            }

            let verb = format_str[i..].chars().next().unwrap_or('\u{FFFD}');
            i += verb.len_utf8();

            if verb == '%' {
                self.buf.push(b'%');
            } else if !self.good_arg_num {
                self.buf.extend_from_slice(b"%!");
                self.write_rune(verb);
                self.buf.extend_from_slice(b"(BADINDEX)");
            } else if arg_num >= a.len() {
                self.buf.extend_from_slice(b"%!");
                self.write_rune(verb);
                self.buf.extend_from_slice(b"(MISSING)");
            } else {
                if verb == 'w' || verb == 'v' {
                    self.f.sharp_v = self.f.sharp;
                    self.f.sharp = false;
                    self.f.plus_v = self.f.plus;
                    self.f.plus = false;
                }
                self.print_arg(&a[arg_num], verb);
                arg_num += 1;
            }
        }

        if !self.reordered && arg_num < a.len() {
            self.clearflags();
            self.buf.extend_from_slice(b"%!(EXTRA ");
            for (i, arg) in a[arg_num..].iter().enumerate() {
                if i > 0 {
                    self.buf.extend_from_slice(b", ");
                }
                if arg.is_nil() {
                    self.buf.extend_from_slice(b"<nil>");
                } else {
                    self.buf.extend_from_slice(arg.type_name().as_bytes());
                    self.buf.push(b'=');
                    self.print_arg(arg, 'v');
                }
            }
            self.buf.push(b')');
        }
    }
}
