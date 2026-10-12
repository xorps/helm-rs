//! Shared helpers for differential tests of `gofmt` and the Sprig functions
//! against real Helm (via the Go oracle).
#![allow(dead_code)]

use std::collections::BTreeMap;

use helm_rs::gofmt::strconv;
use helm_rs::value::Map;
use helm_rs::{CallCtx, DeterministicEnv, FuncMap, Value};

#[path = "../common/mod.rs"]
pub mod common;

pub const SEP: &str = "\n<<<SEP>>>\n";

/// Small deterministic PRNG (splitmix64).
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x5DEE_CE66_D1CE_4E5B)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

/// A template argument: the Rust value plus a template expression producing
/// the same Go value.
#[derive(Clone, Debug)]
pub struct Arg {
    pub value: Value,
    pub tmpl: String,
}

impl Arg {
    pub fn new(value: Value, tmpl: impl Into<String>) -> Arg {
        Arg {
            value,
            tmpl: tmpl.into(),
        }
    }
}

/// A Go string literal for templates.
pub fn lit(s: &str) -> String {
    strconv::quote(s)
}

pub fn str_arg(s: &str) -> Arg {
    Arg::new(Value::str(s), lit(s))
}

pub fn int_arg(i: i64) -> Arg {
    Arg::new(Value::Int(i), format!("{i}"))
}

pub fn float_arg(f: f64) -> Arg {
    if f.is_finite() {
        Arg::new(Value::Float(f), format!("{f:e}"))
    } else {
        let s = if f.is_nan() {
            "NaN"
        } else if f > 0.0 {
            "+Inf"
        } else {
            "-Inf"
        };
        Arg::new(Value::Float(f), format!("(float64 {})", lit(s)))
    }
}

pub fn list_arg(items: Vec<Arg>) -> Arg {
    let tmpl = if items.is_empty() {
        "(list)".to_string()
    } else {
        format!(
            "(list {})",
            items
                .iter()
                .map(|a| a.tmpl.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    Arg::new(
        Value::list(items.into_iter().map(|a| a.value).collect()),
        tmpl,
    )
}

pub fn dict_arg(items: Vec<(String, Arg)>) -> Arg {
    let mut m = Map::new();
    let mut parts = Vec::new();
    for (k, v) in items {
        parts.push(format!("{} {}", lit(&k), v.tmpl));
        m.insert(k, v.value);
    }
    let tmpl = if parts.is_empty() {
        "(dict)".to_string()
    } else {
        format!("(dict {})", parts.join(" "))
    };
    Arg::new(Value::map(m), tmpl)
}

const STR_ALPHABET: &[&str] = &[
    "a", "b", "z", "A", "Z", "0", "9", " ", "  ", "\t", "\n", "-", "_", ".", ",", "/", "\\", "\"",
    "'", "`", "%", "{", "}", "é", "ß", "ǅ", "日本", "€", "😀", "\u{0}", "\u{7f}", "\u{ad}",
    "\u{feff}", "\u{2028}", "\u{a0}", "İ", "x", "y", "hello", "World", "foo_bar", "fooBar", "FOO",
    "http", "://", "=", "+", "*", "?", "(", ")", "[", "]", "|", "^", "$", "\u{1F80}", "ǆ", "Ⅻ",
    "١", "µ", "ﬀ",
];

pub fn rand_string(rng: &mut Rng, max_parts: u64) -> String {
    let n = rng.below(max_parts + 1);
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(rng.pick(STR_ALPHABET));
    }
    s
}

pub fn rand_float(rng: &mut Rng) -> f64 {
    match rng.below(10) {
        0 => 0.0,
        1 => -0.0,
        2 => rng.range(-1000, 1000) as f64,
        3 => rng.range(-1000, 1000) as f64 / 8.0,
        4 => f64::from_bits(rng.next()),
        5 => {
            let f = f64::from_bits(rng.next());
            if f.is_finite() { f } else { 1.5 }
        }
        6 => rng.range(-100000, 100000) as f64 / 1000.0,
        7 => 10f64.powi(rng.range(-30, 30) as i32) * (rng.range(1, 99) as f64),
        8 => *rng.pick(&[
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e21,
            1e20,
            123456789.0,
            0.1,
            0.000001,
            1e-7,
        ]),
        _ => rng.range(-5, 5) as f64 + 0.5,
    }
}

pub fn rand_int(rng: &mut Rng) -> i64 {
    match rng.below(6) {
        0 => 0,
        1 => rng.range(-20, 20),
        2 => rng.range(-100000, 100000),
        3 => rng.next() as i64,
        4 => *rng.pick(&[
            i64::MAX,
            i64::MIN + 1,
            255,
            256,
            65,
            0x1F600,
            0x10FFFF,
            0x110000,
            -1,
        ]),
        _ => rng.range(0, 1000),
    }
}

/// A random argument of any kind; `depth` limits nesting.
pub fn rand_arg(rng: &mut Rng, depth: u32) -> Arg {
    let k = if depth == 0 {
        rng.below(8)
    } else {
        rng.below(11)
    };
    match k {
        0 | 1 => int_arg(rand_int(rng)),
        2 | 3 => float_arg(rand_float(rng)),
        4 | 5 => str_arg(&rand_string(rng, 4)),
        6 => {
            let b = rng.chance(1, 2);
            Arg::new(Value::Bool(b), if b { "true" } else { "false" })
        }
        7 => {
            if rng.chance(1, 2) {
                Arg::new(Value::Nil, "nil")
            } else {
                let i = rand_int(rng);
                Arg::new(Value::Int64(i), format!("(int64 {})", lit(&i.to_string())))
            }
        }
        8 => {
            let n = rng.below(4);
            list_arg((0..n).map(|_| rand_arg(rng, depth - 1)).collect())
        }
        9 => {
            let n = rng.below(4);
            dict_arg(
                (0..n)
                    .map(|_| (rand_string(rng, 2), rand_arg(rng, depth - 1)))
                    .collect(),
            )
        }
        _ => {
            let u = if rng.chance(1, 2) {
                rng.next()
            } else {
                rng.below(300)
            };
            Arg::new(
                Value::Uint64(u),
                format!("(semver {}).Major", lit(&format!("{u}.0.0"))),
            )
        }
    }
}

/// Extracts the function error message from a Helm template error, i.e. the
/// part after `error calling NAME: `.
pub fn func_error_message(err: &str) -> String {
    if let Some(i) = err.find("error calling ") {
        let rest = &err[i + "error calling ".len()..];
        if let Some(j) = rest.find(": ") {
            return rest[j + 2..].to_string();
        }
    }
    err.to_string()
}

/// Renders many expressions in one Helm render, one `{{ expr }}` each.
/// Expressions that fail are isolated by bisection and reported as `Err`.
/// Returns `None` when the oracle is unavailable.
pub fn helm_eval_many(exprs: &[String], values_yaml: &str) -> Option<Vec<Result<String, String>>> {
    let tmpl: String = exprs
        .iter()
        .map(|e| format!("{{{{ {e} }}}}{SEP}"))
        .collect();
    let out = common::helm_render_one(&tmpl, values_yaml)?;
    match out {
        Ok(text) => {
            let mut parts: Vec<Result<String, String>> =
                text.split(SEP).map(|s| Ok(s.to_string())).collect();
            parts.pop(); // trailing empty piece
            if parts.len() != exprs.len() {
                // Some output contained the separator; fall back to one by one.
                if exprs.len() == 1 {
                    return Some(vec![Ok(text.trim_end_matches(SEP).to_string())]);
                }
                let mid = exprs.len() / 2;
                let mut a = helm_eval_many(&exprs[..mid], values_yaml)?;
                a.extend(helm_eval_many(&exprs[mid..], values_yaml)?);
                return Some(a);
            }
            Some(parts)
        }
        Err(e) => {
            if exprs.len() == 1 {
                return Some(vec![Err(e)]);
            }
            let mid = exprs.len() / 2;
            let mut a = helm_eval_many(&exprs[..mid], values_yaml)?;
            a.extend(helm_eval_many(&exprs[mid..], values_yaml)?);
            Some(a)
        }
    }
}

/// Calls a registered function directly.
pub fn call(funcs: &FuncMap, name: &str, args: &[Value]) -> Result<Value, String> {
    let f = funcs
        .get(name)
        .unwrap_or_else(|| panic!("no function {name}"));
    let mut env = DeterministicEnv::default();
    let mut ctx = CallCtx { env: &mut env };
    (f.imp)(&mut ctx, args).map_err(|e| e.message)
}

/// How text/template prints a pipeline value (`fmt.Fprint` semantics).
pub fn render_value(v: &Value) -> String {
    helm_rs::gofmt::format_value(v)
}

/// Compares Rust results against Helm results; returns a list of mismatch
/// descriptions (empty on success).
pub fn compare(
    labels: &[String],
    ours: &[Result<String, String>],
    helm: &[Result<String, String>],
) -> Vec<String> {
    let mut bad = Vec::new();
    for i in 0..labels.len() {
        let h = match &helm[i] {
            Ok(s) => Ok(s.clone()),
            Err(e) => Err(func_error_message(e)),
        };
        if ours[i] != h {
            bad.push(format!(
                "{}\n   ours: {:?}\n   helm: {:?}",
                labels[i], ours[i], h
            ));
        }
    }
    bad
}

pub fn report(name: &str, total: usize, bad: &[String]) {
    eprintln!("{name}: {} cases, {} mismatches", total, bad.len());
    if !bad.is_empty() {
        let shown: Vec<&String> = bad.iter().take(40).collect();
        panic!(
            "{name}: {} of {} cases differ from Helm:\n{}",
            bad.len(),
            total,
            shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

pub fn map_of(pairs: &[(&str, Value)]) -> Value {
    let mut m = BTreeMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v.clone());
    }
    Value::map(m)
}
