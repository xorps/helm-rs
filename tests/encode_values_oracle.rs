//! Differential tests of toYaml/toYamlPretty/toJson on values built by
//! templates (Go ints, int64, floats, []byte, arbitrary strings) against
//! real Helm.
mod common;
mod encoding_support;

use std::collections::BTreeMap;

use encoding_support::*;
use helm_rs::funcs::encoding as enc;
use helm_rs::value::Map;
use helm_rs::{Value, json};

/// A Go string literal for `s`.
fn go_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c if (c as u32) < 0x80 => out.push(c),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push('"');
    out
}

fn probe(expr: &str) -> String {
    format!(
        "{{{{- $v := {expr} -}}}}{{{{- $j := toJson $v -}}}}J:{{{{ $j }}}}{{{{ if $j }}}}\nR:{{{{ toRawJson $v }}}}\nI:{{{{ toPrettyJson $v }}}}{{{{ end }}}}\nY:{{{{ toYaml $v }}}}\nP:{{{{ toYamlPretty $v }}}}"
    )
}

fn rust_probe(v: &Value) -> String {
    let j = enc::to_json(v);
    let mut s = format!("J:{j}");
    if !j.is_empty() {
        s.push_str(&format!(
            "\nR:{}\nI:{}",
            json::to_string_raw(v).unwrap_or_default(),
            json::to_string_indent(v, "", "  ").unwrap_or_default()
        ));
    }
    s.push_str(&format!(
        "\nY:{}\nP:{}",
        enc::to_yaml(v),
        enc::to_yaml_pretty(v)
    ));
    s
}

fn run(cases: &[(String, Value)], files: &BTreeMap<String, String>) {
    let mut mismatches = Vec::new();
    for chunk in cases.chunks(200) {
        let mut templates = BTreeMap::new();
        for (i, (expr, _)) in chunk.iter().enumerate() {
            templates.insert(format!("v{i:04}.yaml"), probe(expr));
        }
        let Some(out) = render_batch(&templates, files, "") else {
            return;
        };
        let out = out.unwrap_or_else(|e| panic!("render failed: {e}"));
        for (i, (expr, v)) in chunk.iter().enumerate() {
            let want = out
                .get(&format!("v{i:04}.yaml"))
                .cloned()
                .unwrap_or_default();
            let got = rust_probe(v);
            if std::env::var_os("ENCODING_DUMP").is_some() {
                eprintln!("== {expr}\n{want}");
            }
            let same_lines = || {
                let norm = |s: &str| {
                    let mut l: Vec<String> = s
                        .replace("\nP:", "\n")
                        .replace("\nY:", "\n")
                        .lines()
                        .map(|l| l.replace("- ", "  "))
                        .collect();
                    l.sort();
                    l
                };
                norm(&want) == norm(&got)
            };
            if want != got && !(same_lines() && has_ambiguous_order(v)) {
                mismatches.push((expr.clone(), want, got));
            }
        }
    }
    report(&mismatches, cases.len());
}

fn s(x: &str) -> Value {
    Value::str(x)
}

fn map(entries: Vec<(&str, Value)>) -> Value {
    Value::map(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<Map>(),
    )
}

#[test]
fn template_values_match_helm() {
    let mut files = BTreeMap::new();
    files.insert("data/b".to_string(), "hello\u{1}world".to_string());
    files.insert("data/e".to_string(), String::new());
    let cases: Vec<(&str, Value)> = vec![
        ("1", Value::Int(1)),
        ("-7", Value::Int(-7)),
        ("(int64 5)", Value::Int64(5)),
        ("(int 9223372036854775807)", Value::Int(i64::MAX)),
        ("1.5", Value::Float(1.5)),
        ("(float64 1000000)", Value::Float(1e6)),
        ("(float64 100000)", Value::Float(1e5)),
        ("1e21", Value::Float(1e21)),
        ("1e20", Value::Float(1e20)),
        ("0.000001", Value::Float(1e-6)),
        ("0.0000001", Value::Float(1e-7)),
        ("123456789.0", Value::Float(123456789.0)),
        (
            "(float64 9007199254740993)",
            Value::Float(9007199254740993.0),
        ),
        ("true", Value::Bool(true)),
        (".Values.missing", Value::Nil),
        ("\"\"", s("")),
        ("(list)", Value::list(vec![])),
        ("(dict)", Value::empty_map()),
        (
            "(list 1 2 (int64 3) 4.5 \"x\" true nil (list) (dict))",
            Value::list(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int64(3),
                Value::Float(4.5),
                s("x"),
                Value::Bool(true),
                Value::Nil,
                Value::list(vec![]),
                Value::empty_map(),
            ]),
        ),
        (
            "(dict \"b\" 1 \"a\" (list \"x\" 2.5))",
            map(vec![
                ("b", Value::Int(1)),
                ("a", Value::list(vec![s("x"), Value::Float(2.5)])),
            ]),
        ),
        (
            "(dict \"yes\" 1 \"1\" 2 \"\" 3 \"a b\" 4 \"-\" 5 \"#\" 6 \"y\" 7 \"null\" 8 \"<<\" 9)",
            map(vec![
                ("yes", Value::Int(1)),
                ("1", Value::Int(2)),
                ("", Value::Int(3)),
                ("a b", Value::Int(4)),
                ("-", Value::Int(5)),
                ("#", Value::Int(6)),
                ("y", Value::Int(7)),
                ("null", Value::Int(8)),
                ("<<", Value::Int(9)),
            ]),
        ),
        (
            "(dict \"a\" (dict \"b\" (dict \"c\" (list (list 1 (list 2)) (dict \"d\" (list))))))",
            map(vec![(
                "a",
                map(vec![(
                    "b",
                    map(vec![(
                        "c",
                        Value::list(vec![
                            Value::list(vec![Value::Int(1), Value::list(vec![Value::Int(2)])]),
                            map(vec![("d", Value::list(vec![]))]),
                        ]),
                    )]),
                )]),
            )]),
        ),
        (
            ".Files.GetBytes \"data/b\"",
            Value::Bytes(b"hello\x01world".to_vec().into()),
        ),
        (
            ".Files.GetBytes \"data/e\"",
            Value::Bytes(Vec::new().into()),
        ),
        (
            "(list (.Files.GetBytes \"data/b\"))",
            Value::list(vec![Value::Bytes(b"hello\x01world".to_vec().into())]),
        ),
        (
            "(dict \"k10\" 1 \"k9\" 2 \"k09\" 3 \"K1\" 4 \"k\" 5 \"k1a\" 6 \"k1A\" 7 \"_k\" 8 \"9\" 9 \"10\" 10 \"é\" 11)",
            map(vec![
                ("k10", Value::Int(1)),
                ("k9", Value::Int(2)),
                ("k09", Value::Int(3)),
                ("K1", Value::Int(4)),
                ("k", Value::Int(5)),
                ("k1a", Value::Int(6)),
                ("k1A", Value::Int(7)),
                ("_k", Value::Int(8)),
                ("9", Value::Int(9)),
                ("10", Value::Int(10)),
                ("é", Value::Int(11)),
            ]),
        ),
    ];
    let mut all: Vec<(String, Value)> =
        cases.into_iter().map(|(e, v)| (e.to_string(), v)).collect();
    // Strings of every flavor, as values and as keys.
    let mut r = Rng::new(5);
    let n: usize = std::env::var("YAML_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    for i in 0..n {
        let st = rand_string(&mut r);
        let st = if i % 7 == 0 {
            format!("{st}{}", rand_string(&mut r).repeat(1 + r.below(6)))
        } else {
            st
        };
        if i % 2 == 0 {
            all.push((go_quote(&st), Value::string(st)));
        } else {
            let k = rand_string(&mut r);
            let expr = format!("(dict {} (list {} 1))", go_quote(&k), go_quote(&st));
            all.push((
                expr,
                map(vec![(
                    k.as_str(),
                    Value::list(vec![Value::string(st), Value::Int(1)]),
                )]),
            ));
        }
    }
    run(&all, &files);
}

/// Key ordering (goyaml's "natural" sort) with digits and letters from many
/// scripts.
#[test]
fn unicode_key_order_matches_helm() {
    let alphabet: Vec<char> =
        "aZ09_-.é\u{663}\u{ff15}\u{301}\u{915}\u{93e}\u{2167}ⅻ\u{1d7d8}Ωж中\u{2028}é1 "
            .chars()
            .collect();
    let mut r = Rng::new(11);
    let n: usize = std::env::var("YAML_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);
    let mut cases = Vec::new();
    for _ in 0..n {
        let nk = 2 + r.below(5);
        let mut m = Map::new();
        let mut expr = String::from("(dict");
        for i in 0..nk {
            let len = 1 + r.below(4);
            let k: String = (0..len)
                .map(|_| alphabet[r.below(alphabet.len())])
                .collect();
            expr.push_str(&format!(" {} {}", go_quote(&k), i));
            m.insert(k, Value::Int(i as i64));
        }
        expr.push(')');
        cases.push((expr, Value::map(m)));
    }
    run(&cases, &BTreeMap::new());
}
