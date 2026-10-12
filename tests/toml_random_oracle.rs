//! Random TOML documents: fromToml/toToml/toJson against real Helm.
mod common;
mod encoding_support;

use encoding_support::*;
use helm_rs::funcs::encoding as enc;

fn probe(name: &str) -> String {
    format!(
        "{{{{- $m := .Files.Get \"data/{name}\" | fromToml -}}}}J:{{{{ toJson $m }}}}\nT:{{{{ toToml $m }}}}\nP:{{{{ toYamlPretty $m }}}}"
    )
}

fn probe_rust(input: &str) -> String {
    let m = enc::from_toml(input);
    format!("J:{}\nT:{}\nP:{}", enc::to_json(&m), enc::to_toml(&m), enc::to_yaml_pretty(&m))
}

fn toml_key(r: &mut Rng) -> String {
    match r.below(8) {
        0 => format!("\"q {}\"", r.below(4)),
        1 => format!("'lit{}'", r.below(4)),
        2 => format!("d{}.e{}", r.below(3), r.below(3)),
        3 => "\"\"".to_string(),
        _ => format!("k{}", r.below(6)),
    }
}

fn toml_string(r: &mut Rng) -> String {
    let s = rand_string(r);
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str(if r.chance(50) { "\\t" } else { "\t" }),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn toml_value(r: &mut Rng, depth: usize) -> String {
    match if depth > 2 { r.below(8) } else { r.below(11) } {
        0 => r
            .pick(&["1", "-17", "+3", "0", "1_000", "0xdead_beef", "0o755", "0b1101", "9223372036854775807"])
            .to_string(),
        1 => {
            r.pick(&["1.5", "-0.0", "6.02e23", "1e-7", "inf", "-inf", "nan", "3.1415_9265", "1E6", "100.0"]).to_string()
        }
        2 => r.pick(&["true", "false"]).to_string(),
        3 => toml_string(r),
        4 => r
            .pick(&[
                "\"plain\"",
                "'lit \\ eral'",
                "\"esc \\t\\n\\\\ \\\" \\u00e9\"",
                "\"\"\"\nmulti\n  line\"\"\"",
                "'''\nraw\n'''",
                "\"\"",
                "\"\"\"a \\\n   b\"\"\"",
            ])
            .to_string(),
        5 => r
            .pick(&[
                "1979-05-27T07:32:00Z",
                "1979-05-27T00:32:00.999-07:00",
                "1979-05-27",
                "07:32:00",
                "1979-05-27 07:32:00",
                "1979-05-27T07:32:00",
            ])
            .to_string(),
        6 | 7 => {
            let n = r.below(4);
            let items: Vec<String> = (0..n).map(|_| toml_value(r, depth + 1)).collect();
            format!("[{}]", items.join(if r.chance(30) { ",\n  " } else { ", " }))
        }
        _ => {
            let n = r.below(3);
            let items: Vec<String> = (0..n).map(|i| format!("i{i} = {}", toml_value(r, depth + 1))).collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

fn gen_toml(r: &mut Rng) -> String {
    let mut s = String::new();
    for _ in 0..r.below(4) {
        s.push_str(&format!("{} = {}\n", toml_key(r), toml_value(r, 0)));
    }
    for t in 0..r.below(4) {
        if r.chance(30) {
            s.push_str(&format!("[[arr{}]]\n", r.below(2)));
        } else {
            s.push_str(&format!("[{}{}]\n", r.pick(&["t", "\"quoted t\"", "a.b", "t.sub"]), t % 2));
        }
        if r.chance(10) {
            s.push_str("# comment\n");
        }
        for _ in 0..r.below(4) {
            let c = if r.chance(10) { " # c" } else { "" };
            s.push_str(&format!("{} = {}{c}\n", toml_key(r), toml_value(r, 0)));
        }
    }
    s
}

#[test]
fn random_toml_matches_helm() {
    // Local date/times depend on the oracle's time zone.
    // SAFETY: nothing reads the environment concurrently at this point.
    unsafe { std::env::set_var("TZ", "UTC") };
    let mut r = Rng::new(31);
    let n: usize = std::env::var("YAML_FUZZ_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(2000);
    let inputs: Vec<String> = (0..n)
        .map(|i| {
            let d = gen_toml(&mut r);
            if i % 4 == 0 { mutate(&mut r, &d) } else { d }
        })
        .collect();
    let Some(m) = diff_inputs(&inputs, &probe, &probe_rust) else { return };
    report(&m, inputs.len());
}
