//! Differential tests of fromToml/toToml against real Helm.
mod common;
mod encoding_support;

use encoding_support::*;
use helm_rs::funcs::encoding as enc;

fn probe(name: &str) -> String {
    format!(
        "{{{{- $m := .Files.Get \"data/{name}\" | fromToml -}}}}{{{{- $j := toJson $m -}}}}J:{{{{ $j }}}}\nT:{{{{ toToml $m }}}}\nY:{{{{ toYaml $m }}}}\nP:{{{{ toYamlPretty $m }}}}"
    )
}

fn probe_rust(input: &str) -> String {
    let m = enc::from_toml(input);
    format!(
        "J:{}\nT:{}\nY:{}\nP:{}",
        enc::to_json(&m),
        enc::to_toml(&m),
        enc::to_yaml(&m),
        enc::to_yaml_pretty(&m)
    )
}

fn yaml_to_toml_probe(name: &str) -> String {
    format!("{{{{- $m := .Files.Get \"data/{name}\" | fromYaml -}}}}T:{{{{ toToml $m }}}}")
}

fn yaml_to_toml_rust(input: &str) -> String {
    format!("T:{}", enc::to_toml(&enc::from_yaml(input)))
}

fn corpus() -> Vec<String> {
    let cases: &[&str] = &[
        "",
        "a = 1",
        "a = 1\n",
        "a = -1\nb = +2\nc = 0\nd = 1_000\ne = 0x1F\nf = 0o17\ng = 0b101\n",
        "a = 1.5\nb = 1e6\nc = -0.0\nd = 1E-7\ne = 3.14159265358979\nf = inf\ng = -inf\nh = nan\ni = 1_0.5\n",
        "a = \"x\"\nb = 'lit\\n'\nc = \"\"\"\nmulti\nline\"\"\"\nd = '''\nraw\n'''\n",
        "a = \"esc \\t \\n \\\" \\\\ \\u00e9 \\U0001F600 \\e \\x41\"\n",
        "a = \"line \\\n   continued\"\n",
        "a = \"\"\"x \\\n\n   y\"\"\"\n",
        "a = true\nb = false\n",
        "a = [1, 2, 3]\nb = [\"x\", 'y']\nc = []\nd = [[1], [2, 3]]\ne = [1, \"mixed\"]\n",
        "a = [\n  1,\n  2, # comment\n]\n",
        "a = {b = 1, c = \"x\"}\n",
        "a = {b = {c = 1}}\n",
        "a = {}\n",
        "a = {b = 1,}\n",
        "a = {\nb = 1\n}\n",
        "[t]\na = 1\n[t.u]\nb = 2\n",
        "[a.b.c]\nd = 1\n",
        "[[arr]]\nx = 1\n[[arr]]\nx = 2\n",
        "[[a.b]]\nc = 1\n[a]\nd = 2\n",
        "a.b.c = 1\na.b.d = 2\n",
        "\"quoted key\" = 1\n'lit key' = 2\n\"\" = 3\n",
        "[\"t q\".'u']\nx = 1\n",
        "# comment\na = 1 # trailing\n",
        "a = 1\r\nb = 2\r\n",
        "\u{feff}a = 1\n",
        "a = 1979-05-27T07:32:00Z\n",
        "a = 1979-05-27T00:32:00.999999-07:00\n",
        "a = 1979-05-27 07:32:00Z\n",
        "a = 1979-05-27\n",
        "a = 07:32:00\n",
        "a = 1979-05-27T07:32:00\n",
        "a = 1979-05-27T07:32Z\n",
        "a = 1979-5-27\n",
        "a = 1979-02-30\n",
        // errors
        "a",
        "a =",
        "a = ",
        "= 1",
        "a = 1\na = 2\n",
        "[t]\n[t]\n",
        "a = 1\n[a]\n",
        "a = [1,,2]\n",
        "a = [1 2]\n",
        "a = {b = 1\n",
        "a = \"unterminated\n",
        "a = 'x\n",
        "a = \"\"\"x",
        "a = 01\n",
        "a = 1__0\n",
        "a = _1\n",
        "a = 1_\n",
        "a = 1.\n",
        "a = .5\n",
        "a = 1.e5\n",
        "a = 9223372036854775808\n",
        "a = 1e400\n",
        "a = +0x1\n",
        "a = 0x\n",
        "a = tru\n",
        "a = \"\\q\"\n",
        "a = \"\\u12\"\n",
        "a = \"\\uD800\"\n",
        "[a\n",
        "[]\n",
        "[.a]\n",
        "[a.]\n",
        "[[a]\n",
        "a.b = 1\na = 2\n",
        "a = 1 b = 2\n",
        "a = 1\n\u{1}\n",
        "a = \"\u{7f}\"\n",
        "a=\"x\"\ra=1\n",
        "a = [1]\n[[a]]\n",
        "[[a]]\n[a]\n",
        "a = {b = 1}\na.c = 2\n",
        "a.b = 1\n[a]\nc = 2\n",
        "a = inf\nb = -nan\nc = +nan\n",
        "a = 0b2\n",
        "a = 0o8\n",
        "a = -.5\n",
        "a = 1e\n",
        "a = \"x\" # c\n# end",
        "x.\"y\" . z = 1\n",
        "a = [ ]\n",
        "a = [\n# only comment\n]\n",
        "a = [1, [2, {b = 3}]]\n",
        "a = {b.c = 1, b.d = 2}\n",
        "a = {b = 1, b = 2}\n",
        "a = 'it''s'\n",
        "a = '''it's'''\n",
        "a = \"\"\"\"quoted\"\"\"\"\n",
        "a = \"\"\"\"\"\"\"\n",
        "a = ''''''''\n",
    ];
    cases.iter().map(|s| s.to_string()).collect()
}

fn set_utc() {
    // Local date/times depend on the oracle's time zone.
    // SAFETY: tests in this binary do not read the environment concurrently
    // before the oracle is spawned.
    unsafe { std::env::set_var("TZ", "UTC") };
}

#[test]
fn from_toml_corpus_matches_helm() {
    set_utc();
    let inputs = corpus();
    let Some(m) = diff_inputs(&inputs, &probe, &probe_rust) else {
        return;
    };
    report(&m, inputs.len());
}

#[test]
fn to_toml_of_yaml_values_matches_helm() {
    set_utc();
    let mut r = Rng::new(77);
    let mut inputs: Vec<String> = [
        "a: 1\nb: x\nc: [1, 2]\nd: {e: f}\n",
        "a: [{b: 1}, {c: 2}]\n",
        "a: [{b: 1}, 2]\n",
        "a: [1, null]\n",
        "a: null\nb: 1\n",
        "a: {b: {c: {d: 1}}}\n",
        "\"k y\": 1\n\"\": 2\n\"x.y\": 3\n",
        "a: []\nb: {}\n",
        "a: [[{b: 1}]]\n",
        "a: {b: null, c: 1}\n",
        "a: [{}]\n",
        "a: 1e21\nb: 1.5\nc: 100\nd: -0.0\n",
        "a: \"tab\\there\\u007f\\u0001\"\n",
        "a: [{b: [{c: 1}]}]\n",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let n: usize = std::env::var("YAML_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    inputs.extend((0..n).map(|_| gen_doc(&mut r)));
    let Some(m) = diff_inputs(&inputs, &yaml_to_toml_probe, &yaml_to_toml_rust) else {
        return;
    };
    report(&m, inputs.len());
}

#[test]
fn mutated_toml_matches_helm() {
    set_utc();
    let mut r = Rng::new(3);
    let base = corpus();
    let n: usize = std::env::var("YAML_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);
    let inputs: Vec<String> = (0..n)
        .map(|_| {
            let i = r.below(base.len());
            mutate(&mut r, &base[i])
        })
        .collect();
    let Some(m) = diff_inputs(&inputs, &probe, &probe_rust) else {
        return;
    };
    report(&m, inputs.len());
}
