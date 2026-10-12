//! Differential tests of JSON decoding/encoding against real Helm.
mod common;
mod encoding_support;

use encoding_support::*;
use helm_rs::funcs::encoding as enc;
use helm_rs::json;

fn json_probe_rust(input: &str) -> String {
    let m = enc::from_json(input);
    let j = enc::to_json(&m);
    if j.is_empty() {
        return "J:".into();
    }
    format!("J:{j}\nR:{}", json::to_string_raw(&m).unwrap_or_default())
}

/// Go returns a nil map/slice for a top-level JSON `null` (printed as
/// `null` by `toJson`); we return an empty one (a documented divergence).
fn without_null(inputs: Vec<String>) -> Vec<String> {
    inputs
        .into_iter()
        .filter(|s| s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r' || c == '\n') != "null")
        .collect()
}

fn json_array_probe_rust(input: &str) -> String {
    format!("J:{}", enc::to_json(&enc::from_json_array(input)))
}

fn corpus() -> Vec<String> {
    let cases: &[&str] = &[
        "{}",
        "[]",
        "null",
        "true",
        "false",
        "1",
        "\"x\"",
        "{\"a\":1}",
        " {\"a\" : [1, 2.5, -0, 1e3, 1E-3, 0.1] } ",
        "{\"a\":1,\"a\":2}",
        "{\"b\":1,\"a\":{\"d\":[],\"c\":{}}}",
        "[1, [2, [3]], {\"a\": null}]",
        "{\"a\":\"<&>\"}",
        "{\"a\":\"\\u003c\\u2028\\u2029\\ud83d\\ude00\"}",
        "{\"a\":\"\\ud800\"}",
        "{\"a\":\"\\ud800\\u0041\"}",
        "{\"a\":\"\\udc00\\ud800\"}",
        "{\"a\":\"\\b\\f\\n\\r\\t\\/\\\\\\\"\"}",
        "{\"a\":\"\\u0000\\u001f\\u007f\"}",
        "{\"a\":\"é日本😀\"}",
        "{\"a\":1e400}",
        "{\"a\":-1e400,\"b\":2}",
        "{\"a\":1e-400}",
        "[1e400, 2]",
        "{\"a\":12345678901234567890}",
        "{\"a\":9007199254740993}",
        "{\"a\":0.1e1}",
        "{\"a\":1.7976931348623157e308}",
        "{\"a\":5e-324}",
        "{\"a\":123456789.123456789}",
        "{\"a\":1e21}",
        "{\"a\":1e20}",
        "{\"a\":0.000001}",
        "{\"a\":0.0000001}",
        "{\"a\":-0.0}",
        "{\"a\":100}",
        // syntax errors
        "",
        " ",
        "x",
        "{",
        "[",
        "{\"a\"",
        "{\"a\":",
        "{\"a\":1",
        "{\"a\":1,",
        "{\"a\" 1}",
        "{\"a\":1 \"b\":2}",
        "[1 2]",
        "[1,]",
        "{\"a\":1,}",
        "{a:1}",
        "{'a':1}",
        "[01]",
        "[-]",
        "[1.]",
        "[.5]",
        "[1e]",
        "[1e+]",
        "[+1]",
        "[-a]",
        "[tru]",
        "[trux]",
        "[nul]",
        "[fals]",
        "[\"a\\x\"]",
        "[\"\\u12\"]",
        "[\"\\u12g4\"]",
        "[\"a\nb\"]",
        "[\"a\tb\"]",
        "[1] x",
        "[1] [2]",
        "{}}",
        "[]]",
        "\u{feff}{}",
        "{\"a\":NaN}",
        "{\"a\":Infinity}",
        "[\"\\u00e9\"]",
        "{\"\":1}",
        "[\"\u{1}\"]",
        "[\u{1}]",
        "{\"a\"\u{a0}:1}",
        "nulls",
        "truex",
        "1x",
        "{\"a\":[1,{\"b\":[2,{\"c\":3}]}]}",
        "[[[[[[[[[[]]]]]]]]]]",
        "{\"a\":\"\\uDFFF\"}",
        "\"\\u\"",
        "[1,\n2,\r\n3\t]",
        "{\"a\":true,\"b\":false,\"c\":null}",
        "  [  ]  ",
        "{\"a\": 1e309}",
    ];
    cases.iter().map(|s| s.to_string()).collect()
}

#[test]
fn from_json_corpus_matches_helm() {
    let inputs = without_null(corpus());
    let Some(m) = diff_inputs(&inputs, &from_json_probe, &json_probe_rust) else { return };
    report(&m, inputs.len());
}

#[test]
fn from_json_array_corpus_matches_helm() {
    let inputs = without_null(corpus());
    let Some(m) = diff_inputs(&inputs, &from_json_array_probe, &json_array_probe_rust) else { return };
    report(&m, inputs.len());
}

fn gen_json(r: &mut Rng, depth: usize) -> String {
    let ws = |r: &mut Rng| -> &'static str { r.pick(&["", "", " ", "\n", "\t", "  ", "\r\n"]) };
    match if depth > 4 { r.below(5) } else { r.below(8) } {
        0 => r
            .pick(&[
                "0",
                "-0",
                "1",
                "-1",
                "1.5",
                "1e3",
                "1E+3",
                "1e-7",
                "0.000001",
                "123456789012345678901234",
                "1e21",
                "1e20",
                "3.14159265358979",
                "2.5e-8",
                "1e308",
                "5e-324",
                "-1.0e-0",
                "100000",
            ])
            .to_string(),
        1 => r.pick(&["true", "false", "null"]).to_string(),
        2..=4 => {
            let s = rand_string(r);
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c if r.chance(5) && (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04X}", c as u32)),
                    c => out.push(c),
                }
            }
            if r.chance(5) {
                out.push_str(r.pick(&["\\ud83d\\ude00", "\\ud800", "\\udc00", "\\/", "\\b\\f"]));
            }
            out.push('"');
            out
        }
        5 | 6 => {
            let n = r.below(5);
            let items: Vec<String> = (0..n)
                .map(|i| {
                    let key =
                        if r.chance(70) { format!("k{}", r.below(6) + i) } else { format!("{:?}", rand_string(r)) };
                    let key = if key.starts_with('"') { key } else { format!("\"{key}\"") };
                    format!("{}{}{}:{}{}", ws(r), key, ws(r), ws(r), gen_json(r, depth + 1))
                })
                .collect();
            format!("{{{}{}}}", items.join(","), ws(r))
        }
        _ => {
            let n = r.below(5);
            let items: Vec<String> = (0..n).map(|_| format!("{}{}", ws(r), gen_json(r, depth + 1))).collect();
            format!("[{}{}]", items.join(","), ws(r))
        }
    }
}

#[test]
fn random_json_matches_helm() {
    let mut r = Rng::new(99);
    let n: usize = std::env::var("YAML_FUZZ_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(3000);
    let inputs: Vec<String> = (0..n)
        .map(|i| {
            let doc = if r.chance(80) {
                let n = r.below(4);
                let items: Vec<String> = (0..n).map(|j| format!("\"k{j}\": {}", gen_json(&mut r, 1))).collect();
                format!("{{{}}}", items.join(", "))
            } else {
                gen_json(&mut r, 0)
            };
            if i % 3 == 0 { mutate(&mut r, &doc) } else { doc }
        })
        .collect();
    let inputs = without_null(inputs);
    let Some(m) = diff_inputs(&inputs, &from_json_probe, &json_probe_rust) else { return };
    report(&m, inputs.len());
    // The same documents parsed as YAML (JSON is YAML).
    let Some(m) = diff_inputs(&inputs, &from_yaml_probe, &from_yaml_probe_rust) else { return };
    report(&m, inputs.len());
}
