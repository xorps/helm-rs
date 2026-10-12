//! Sprig v3.3.0 (`github.com/Masterminds/sprig/v3`) `TxtFuncMap`.
//!
//! Every function is registered with its Go signature (see
//! [`crate::func::Sig`]) and reproduces the Go implementation, including the
//! behavior of its dependencies (`spf13/cast`, `Masterminds/goutils`,
//! `huandu/xstrings`, `shopspring/decimal`, `dario.cat/mergo`, ...). Where
//! Go would panic, the function returns a [`FuncError`] carrying Go's panic
//! message.
//!
//! Known divergences (no_std / model limitations):
//! - `split`/`splitn` return `map[string]interface {}` and `splitList`,
//!   `toStrings`, `keys`, `sortAlpha`, `regexFindAll`, `regexSplit` return
//!   `[]interface {}` (Go: `map[string]string` / `[]string`), visible only
//!   through `typeOf`/`%T`.
//! - `keys`/`values` iterate in sorted key order (Go: random order).
//! - `env` returns "" and `expandenv` expands every variable to "" (there is
//!   no process environment); Helm removes both anyway.
//! - Time zones other than `UTC` and `Local` (the [`crate::Env`] offset)
//!   fall back to UTC, as Go does when a zone cannot be loaded.
//! - Elements of `[]byte` seen through list functions are `uint64` values.

use alloc::string::String;
use alloc::vec::Vec;

use crate::func::{FuncError, FuncMap};
use crate::gofmt;
use crate::gofmt::strconv;
use crate::value::{Kind, Value};

mod certs;
mod crypto;
mod date;
mod decimal;
mod defaults;
mod dict;
pub mod goregex;
pub mod gounicode;
mod list;
mod numeric;
mod paths;
mod reflect;
mod regex;
mod regex_names;
mod strings;
pub mod time;
mod unicode_tables;
mod url;

/// Registers all of Sprig's text-template functions.
pub fn register(f: &mut FuncMap) {
    strings::register(f);
    numeric::register(f);
    defaults::register(f);
    dict::register(f);
    list::register(f);
    reflect::register(f);
    regex::register(f);
    paths::register(f);
    url::register(f);
    crypto::register(f);
    date::register(f);
}

/// Upper bound on the size of generated strings/lists (Go would try and
/// usually die; we return an error instead).
pub(crate) const MAX_OUTPUT: usize = 1 << 28;

pub(crate) fn too_large(what: &str) -> FuncError {
    FuncError::new(alloc::format!("{what}: output too large"))
}

/// Argument `i` (missing arguments read as nil).
pub(crate) fn arg(a: &[Value], i: usize) -> &Value {
    a.get(i).unwrap_or(&Value::Nil)
}

/// String argument `i` (a `Ty::String` parameter).
pub(crate) fn sarg(a: &[Value], i: usize) -> &str {
    match a.get(i) {
        Some(Value::String(s)) => s,
        _ => "",
    }
}

/// Integer argument `i` (a `Ty::Int`/`Ty::Int64` parameter).
pub(crate) fn iarg(a: &[Value], i: usize) -> i64 {
    match a.get(i) {
        Some(Value::Int(n)) | Some(Value::Int64(n)) => *n,
        Some(Value::Uint64(n)) => *n as i64,
        Some(Value::Float(f)) => *f as i64,
        _ => 0,
    }
}

/// Bool argument `i`.
pub(crate) fn barg(a: &[Value], i: usize) -> bool {
    matches!(a.get(i), Some(Value::Bool(true)))
}

/// A list of strings as a template value.
pub(crate) fn str_list<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> Value {
    Value::list(items.into_iter().map(|s| Value::str(s.as_ref())).collect())
}

/// Sprig's `strval`: strings as-is, `[]byte` as a string, `Stringer`s via
/// `String()`, everything else via `fmt.Sprintf("%v")`.
pub(crate) fn strval(v: &Value) -> String {
    match v {
        Value::String(s) => String::from(&**s),
        Value::Bytes(b) => strconv::lossy(b),
        _ => gofmt::format_value(v),
    }
}

/// Sprig's `empty` (reflection-based zero check).
pub(crate) fn empty(v: &Value) -> bool {
    match v {
        Value::Nil => true,
        Value::Object(o) => match o.kind() {
            Kind::Struct => false,
            Kind::Ptr => false,
            _ => !o.truthy(),
        },
        _ => !v.truthy(),
    }
}

/// `reflect.TypeOf(v).Kind().String()` (panics on nil in Go).
pub(crate) fn kind_name(v: &Value) -> &'static str {
    v.kind().name()
}

pub(crate) const NIL_DEREF: &str = "runtime error: invalid memory address or nil pointer dereference";

/// Elements of a slice-kinded value, as `reflect.Value.Index(i).Interface()`.
pub(crate) fn slice_items(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::List(l) => Some(l.to_vec()),
        Value::Bytes(b) => Some(b.iter().map(|&x| Value::Uint64(x as u64)).collect()),
        _ => None,
    }
}

/// Go `reflect.DeepEqual`.
pub(crate) fn deep_equal(a: &Value, b: &Value) -> bool {
    deep_equal_depth(a, b, 0)
}

fn deep_equal_depth(a: &Value, b: &Value, depth: usize) -> bool {
    if depth > 1000 {
        return false;
    }
    match (a, b) {
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| deep_equal_depth(p, q, depth + 1))
        }
        (Value::Map(x), Value::Map(y)) => {
            if x.ptr_eq(y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len()
                && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| deep_equal_depth(v, w, depth + 1)))
        }
        (Value::Object(x), Value::Object(y)) => {
            if alloc::rc::Rc::ptr_eq(x, y) {
                return true;
            }
            if x.type_name() != y.type_name() {
                return false;
            }
            if let (Some(p), Some(q)) = (
                x.as_any().downcast_ref::<crate::funcs::duration::Duration>(),
                y.as_any().downcast_ref::<crate::funcs::duration::Duration>(),
            ) {
                return p == q;
            }
            if let (Some(p), Some(q)) = (
                x.as_any().downcast_ref::<crate::semver::VersionObject>(),
                y.as_any().downcast_ref::<crate::semver::VersionObject>(),
            ) {
                return p.version == q.version;
            }
            if let (Some(p), Some(q)) = (x.as_any().downcast_ref::<time::Time>(), y.as_any().downcast_ref::<time::Time>()) {
                return p == q;
            }
            false
        }
        _ => a == b,
    }
}

/// `cast.ToInt64` (errors become 0).
pub(crate) fn to_int64(v: &Value) -> i64 {
    match v {
        Value::Int(n) | Value::Int64(n) => *n,
        Value::Uint64(n) => *n as i64,
        Value::Float(f) => crate::funcs::duration::f64_to_i64(*f),
        Value::String(s) => strconv::parse_int(trim_zero_decimal(s), 0, 64).unwrap_or(0),
        Value::Bool(b) => *b as i64,
        _ => 0,
    }
}

/// `cast.ToFloat64` (errors become 0).
pub(crate) fn to_float64(v: &Value) -> f64 {
    match v {
        Value::Int(n) | Value::Int64(n) => *n as f64,
        Value::Uint64(n) => *n as f64,
        Value::Float(f) => *f,
        Value::String(s) => strconv::parse_float(s).unwrap_or(0.0),
        Value::Bool(b) => *b as i64 as f64,
        _ => 0.0,
    }
}

/// cast's `trimZeroDecimal`: "1.0" -> "1".
pub(crate) fn trim_zero_decimal(s: &str) -> &str {
    let b = s.as_bytes();
    let mut found_zero = false;
    let mut i = b.len();
    while i > 0 {
        match b[i - 1] {
            b'.' => {
                if found_zero {
                    return &s[..i - 1];
                }
            }
            b'0' => found_zero = true,
            _ => return s,
        }
        i -= 1;
    }
    s
}

/// Draws a uniformly random integer in `0..n` from the environment
/// (`rand.Intn`).
pub(crate) fn rand_below(ctx: &mut crate::func::CallCtx<'_>, n: u64) -> u64 {
    if n <= 1 {
        return 0;
    }
    let zone = u64::MAX - (u64::MAX % n);
    loop {
        let mut b = [0u8; 8];
        ctx.env.fill_random(&mut b);
        let v = u64::from_le_bytes(b);
        if v < zone {
            return v % n;
        }
    }
}
