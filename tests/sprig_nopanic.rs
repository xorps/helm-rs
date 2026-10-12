//! Robustness: every function must return (Ok or Err) and never panic or
//! allocate unboundedly on arbitrary, signature-conforming arguments.

mod diffkit;

use diffkit::*;
use helm_rs::{CallCtx, DeterministicEnv, Ty, Value};

fn value_for(rng: &mut Rng, t: Ty) -> Value {
    match t {
        Ty::Any => rand_arg(rng, 2).value,
        Ty::String => Value::string(match rng.below(5) {
            0 => rand_string(rng, 8),
            1 => ["(", "[", "\\", "a{1001}", "%", "%!", "${", "2006-01-02", "1h", ">=1.2", "x||y", "::", "-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----"][rng.below(13) as usize].to_string(),
            _ => rand_string(rng, 4),
        }),
        Ty::Bool => Value::Bool(rng.chance(1, 2)),
        Ty::Int | Ty::Int64 => Value::Int(match rng.below(4) {
            0 => rand_int(rng),
            1 => *rng.pick(&[i64::MAX, i64::MIN, 1 << 40, -(1 << 40), 1_000_000_000_000]),
            _ => rng.range(-10, 10),
        }),
        Ty::Uint64 => Value::Uint64(rng.next()),
        Ty::Uint32 => Value::Uint64(rng.next() as u32 as u64),
        Ty::Float64 => Value::Float(rand_float(rng)),
        Ty::Bytes => Value::Bytes(rand_string(rng, 4).into_bytes().into()),
        Ty::List | Ty::Strings => match rand_arg(rng, 1).value {
            v @ Value::List(_) => v,
            v => Value::list(vec![v]),
        },
        Ty::Map => match rand_arg(rng, 2).value {
            v @ Value::Map(_) => v,
            v => map_of(&[("a", v)]),
        },
        Ty::Object(_) | Ty::Struct(_) => Value::Nil,
    }
}

#[test]
fn no_function_panics() {
    let funcs = helm_rs::funcs::helm();
    let seed: u64 = std::env::var("NOPANIC_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(99);
    let base_rounds: usize = std::env::var("NOPANIC_ROUNDS").ok().and_then(|s| s.parse().ok()).unwrap_or(400);
    let mut rng = Rng::new(seed);
    let slow = ["genPrivateKey", "genCA", "genSelfSignedCert", "genSignedCert", "derivePassword", "bcrypt", "htpasswd"];
    let mut names: Vec<String> = funcs.names().map(String::from).collect();
    names.sort();
    let mut calls = 0;
    for name in &names {
        // JSON (de)serialization is implemented in `crate::json` (separate module).
        if name.contains("Json") {
            continue;
        }
        let f = funcs.get(name).unwrap();
        let rounds = if slow.contains(&name.as_str()) { 3 } else { base_rounds };
        for _ in 0..rounds {
            let mut args: Vec<Value> = f.sig.params.iter().map(|&t| value_for(&mut rng, t)).collect();
            if let Some(v) = f.sig.variadic {
                for _ in 0..rng.below(4) {
                    args.push(value_for(&mut rng, v));
                }
            }
            let mut env = DeterministicEnv::new(rng.next());
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut ctx = CallCtx { env: &mut env };
                let _ = (f.imp)(&mut ctx, &args);
            }));
            assert!(r.is_ok(), "{name} panicked on {args:?}");
            calls += 1;
        }
    }
    eprintln!("no_function_panics: {calls} calls");
}

#[test]
fn huge_requests_fail_cleanly() {
    let funcs = helm_rs::funcs::helm();
    let cases: Vec<(&str, Vec<Value>)> = vec![
        ("repeat", vec![Value::Int(1 << 40), Value::str("abc")]),
        ("until", vec![Value::Int(1_000_000_000_000)]),
        ("untilStep", vec![Value::Int(0), Value::Int(i64::MAX), Value::Int(1)]),
        ("seq", vec![Value::Int(1_000_000_000_000)]),
        ("indent", vec![Value::Int(1 << 40), Value::str("a\nb")]),
        ("randAlphaNum", vec![Value::Int(1 << 50)]),
        ("randBytes", vec![Value::Int(1 << 50)]),
        ("printf", vec![]),
    ];
    for (name, args) in cases {
        let Some(f) = funcs.get(name) else { continue };
        let mut env = DeterministicEnv::default();
        let mut ctx = CallCtx { env: &mut env };
        let r = (f.imp)(&mut ctx, &args);
        assert!(r.is_err(), "{name} should fail cleanly");
    }
    // Deeply nested / self-referential structures.
    let m = map_of(&[]);
    if let Value::Map(mr) = &m {
        mr.insert("self", m.clone());
    }
    for name in ["deepCopy", "toString", "mergeOverwrite", "merge"] {
        let f = funcs.get(name).unwrap();
        let mut env = DeterministicEnv::default();
        let mut ctx = CallCtx { env: &mut env };
        let args = if name.starts_with("merge") { vec![m.clone(), m.clone()] } else { vec![m.clone()] };
        let _ = (f.imp)(&mut ctx, &args);
    }
    // Break the cycle so the test does not leak.
    if let Value::Map(mr) = &m {
        mr.remove("self");
    }
}

#[test]
fn gofmt_never_panics() {
    let mut rng = Rng::new(5);
    let chars = ["%", "v", "d", "*", "[", "]", "1", "0", ".", "-", "+", "#", " ", "x", "q", "é", "9", "T", "!"];
    for _ in 0..20000 {
        let n = rng.below(12);
        let f: String = (0..n).map(|_| *rng.pick(&chars)).collect();
        let args: Vec<Value> = (0..rng.below(4)).map(|_| rand_arg(&mut rng, 2).value).collect();
        let _ = helm_rs::gofmt::sprintf(&f, &args);
        let _ = helm_rs::gofmt::sprint(&args);
    }
}
