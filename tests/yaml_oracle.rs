//! Differential tests of YAML parsing/emission against real Helm.
mod common;
mod encoding_support;

use encoding_support::*;

fn corpus() -> Vec<String> {
    let cases: &[&str] = &[
        // scalars & resolution
        "a: 1\n",
        "a: -1\n",
        "a: +1\n",
        "a: 1.0\n",
        "a: 1e6\n",
        "a: 1e21\n",
        "a: 1e20\n",
        "a: 1.5e300\n",
        "a: 0.000001\n",
        "a: 0.0000001\n",
        "a: 123456789\n",
        "a: 12345678901234567890\n",
        "a: 9007199254740993\n",
        "a: -9223372036854775808\n",
        "a: -9223372036854775809\n",
        "a: 0x1F\n",
        "a: 0o17\n",
        "a: 017\n",
        "a: 08\n",
        "a: 0b1010\n",
        "a: -0b11\n",
        "a: 1_000\n",
        "a: 1:30\n",
        "a: 190:20:30.15\n",
        "a: .5\n",
        "a: -.5\n",
        "a: 5.\n",
        "a: 1e400\n",
        "a: .inf\n",
        "a: -.inf\n",
        "a: .nan\n",
        "a: .NaN\n",
        "a: NaN\n",
        "a: inf\n",
        "a: true\n",
        "a: True\n",
        "a: yes\n",
        "a: No\n",
        "a: on\n",
        "a: Off\n",
        "a: y\n",
        "a: n\n",
        "a: Y\n",
        "a: null\n",
        "a: ~\n",
        "a:\n",
        "a: Null\n",
        "a: NULL\n",
        "a: nULL\n",
        "a: 2001-12-14\n",
        "a: 2001-12-14t21:59:43.10-05:00\n",
        "a: 2001-12-14 21:59:43.10\n",
        "a: 2001-12-14 21:59:43.10 -5\n",
        "a: 2001-02-30\n",
        "a: <<\n",
        "a: '1'\n",
        "a: \"true\"\n",
        "a: 'yes'\n",
        "a: 'y'\n",
        "a: '~'\n",
        "a: ''\n",
        "a: \"\"\n",
        "a: '2001-12-14'\n",
        "a: '1:30'\n",
        "a: '0x10'\n",
        "a: '1e3'\n",
        "a: '.inf'\n",
        "a: '1_000'\n",
        "a: '-'\n",
        "a: '---'\n",
        "a: '...'\n",
        "a: '# x'\n",
        "a: 'x #y'\n",
        "a: 'x: y'\n",
        "a: ': x'\n",
        "a: '- x'\n",
        "a: '? x'\n",
        "a: '[x]'\n",
        "a: '{x}'\n",
        "a: '@x'\n",
        "a: '`x'\n",
        "a: '!x'\n",
        "a: '&x'\n",
        "a: '*x'\n",
        "a: '%x'\n",
        "a: '|'\n",
        "a: '>'\n",
        "a: ' x'\n",
        "a: 'x '\n",
        "a: \"\\t\"\n",
        "a: \"x\\ty\"\n",
        "a: \"\\0\\a\\b\\v\\f\\r\\e\"\n",
        "a: \"\\x41\\u00e9\\U0001F600\"\n",
        "a: \"\\N\\_\\L\\P\"\n",
        "a: \"\\/\"\n",
        "a: \"\\q\"\n",
        "a: \"\\x4\"\n",
        "a: \"\\uD800\"\n",
        "a: \"é日本😀\"\n",
        "a: \"\\u00a0\"\n",
        "a: \"\\u0085\"\n",
        "a: \"\\ufeffx\"\n",
        "a: \"\\ufeff\"\n",
        "a: \"\\u2028\"\n",
        "a: \"\\x7f\"\n",
        "a: \"\\x80\"\n",
        "a: \"\\x9f\"\n",
        "a: \"\\uFFFE\"\n",
        "a: \"<&>\"\n",
        "a: \"\\u2029x\"\n",
        "a: 'it''s'\n",
        "a: \"say \\\"hi\\\"\"\n",
        "a: \"back\\\\slash\"\n",
        // multi-line scalars
        "a: |\n  line1\n  line2\n",
        "a: |-\n  x\n",
        "a: |+\n  x\n\n",
        "a: >\n  folded\n  text\n\n  para\n",
        "a: >-\n  x\n   y\n  z\n",
        "a: |2\n   x\n",
        "a: |\n\n  x\n",
        "a: |\n  \n  x\n",
        "a: |\n x\n",
        "a: |1\n  x\n",
        "a: |0\n x\n",
        "a: |\n  x\n y\n",
        "a: >\n\n\n",
        "a: |\n",
        "a: |-\n",
        "a: >+\n\n\n",
        "a: plain\n  continued\n",
        "a: plain\n\n  para\n",
        "a: 'single\n  line'\n",
        "a: \"double\n  line\"\n",
        "a: \"x\\\n  y\"\n",
        "a: \"x \\\n  y\"\n",
        "a: 'a\n\n  b'\n",
        "a: \"trailing   \n  x\"\n",
        "a: \"x\n\n\n  y\"\n",
        "a: |\n  trailing space \n",
        "a: |\n  \ttab\n",
        "a: \"\\n\"\n",
        "a: \"\\n\\n\"\n",
        "a: \"x\\n\"\n",
        "a: \"x\\n\\n\"\n",
        "a: \" x\\ny\"\n",
        "a: \"x \\ny\"\n",
        "a: \"x\\n y\"\n",
        "a: \"\\nx\"\n",
        "a: \"x\\r\\ny\"\n",
        "a: \"x\\ry\"\n",
        // long strings (folding at width 80)
        "a: aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddddd eeeeeeeeee ffffffffff gggggggggg hhhhhhhhhh iiiiiiiiii jjjjjjjjjj\n",
        "a: \"aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddddd eeeeeeeeee ffffffffff gggggggggg hhhhhhhhhh: iiiiiiiiii jjjjjjjjjj\"\n",
        "a: \"aaaaaaaaaa  bbbbbbbbbb cccccccccc dddddddddd eeeeeeeeee ffffffffff gggggggggg hhhhhhhhhh\\tiiiiiiiiii jjjjjjjjjj\"\n",
        "a: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        "k: [aaaaaaaaaa, bbbbbbbbbb, cccccccccc, dddddddddd, eeeeeeeeee, ffffffffff, gggggggggg, hhhhhhhhhh, iiiiiiiiii]\n",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa: 1\n",
        // collections
        "a: []\nb: {}\n",
        "a: [1, [2, [3]], {b: c}]\n",
        "a:\n- 1\n- - 2\n  - 3\n- b: c\n  d: e\n",
        "a:\n  - x\n  -\n  - y\n",
        "a: {b: , c}\n",
        "a: {? b}\n",
        "a: [b: c, d]\n",
        "a: [? b : c]\n",
        "a: {b: [1,2], c: {d: e}}\n",
        "a: [a,b,]\n",
        "a: {a: b,}\n",
        "a: [,]\n",
        "? a\n: b\n",
        "? |\n  x\n: y\n",
        "? [a, b]\n: c\n",
        "? {a: b}\n: c\n",
        "a: {[a]: b}\n",
        "1: a\n2: b\n10: c\n",
        "true: a\nfalse: b\n",
        "~: a\n",
        "null: a\n",
        "1.5: a\n",
        "0.1: a\n",
        "3.14159265358979: a\n",
        "1e10: a\n",
        "100000: a\n",
        "1234567: a\n",
        ".inf: a\n",
        "-.inf: a\n",
        ".nan: a\n",
        "18446744073709551615: a\n",
        "0x10: a\n",
        "yes: a\nno: b\n",
        "'1': a\n1: b\n",
        "a: 1\na: 2\n",
        "2001-01-01: a\n",
        "b: 1\na: 2\nc: 3\n",
        "a10: 1\na2: 2\na1: 3\nA: 4\n_: 5\n-: 6\n",
        "x01: 1\nx1: 2\nx001: 3\nx10: 4\n",
        "a: 1\nB: 2\nc: 3\nD: 4\n",
        "é: 1\ne: 2\nz: 3\n",
        "1a: 1\na1: 2\n11: 3\n2: 4\n",
        "k: {}\nl: []\nm: {n: {}}\n",
        "a:\n  b:\n    c:\n      d: e\n",
        // anchors, aliases, merge
        "a: &x 1\nb: *x\n",
        "a: &x [1, 2]\nb: *x\n",
        "base: &b {x: 1, y: 2}\nc:\n  <<: *b\n  y: 3\n",
        "base: &b {x: 1, y: 2}\nc:\n  y: 3\n  <<: *b\n",
        "a: &a {x: 1}\nb: &b {x: 2, y: 2}\nc:\n  <<: [*a, *b]\n",
        "c:\n  <<: {x: 1}\n  x: 2\n",
        "c:\n  <<: 1\n",
        "c:\n  <<: [1]\n",
        "a: &a [1]\nc:\n  <<: *a\n",
        "c:\n  '<<': {x: 1}\n",
        "c:\n  !!merge <<: {x: 1}\n",
        "a: *unknown\n",
        "a: &a [*a]\n",
        "a: &a\n  b: *a\n",
        "a: &x 1\na2: &x 2\nb: *x\n",
        "&a a: b\nc: *a\n",
        "a: &x\nb: *x\n",
        "a: &x !!str 1\nb: *x\n",
        // tags
        "a: !!str 1\n",
        "a: !!int '1'\n",
        "a: !!int x\n",
        "a: !!float 1\n",
        "a: !!float x\n",
        "a: !!bool yes\n",
        "a: !!bool 1\n",
        "a: !!null ''\n",
        "a: !!null x\n",
        "a: !!binary aGVsbG8=\n",
        "a: !!binary |\n  aGVs\n  bG8=\n",
        "a: !!binary x\n",
        "a: !!binary /w==\n",
        "a: !foo bar\n",
        "a: !foo 12\n",
        "a: ! 12\n",
        "a: !<tag:yaml.org,2002:str> 12\n",
        "a: !!timestamp 2001-01-01\n",
        "a: !!timestamp x\n",
        "a: !!map {b: c}\n",
        "a: !!seq [b]\n",
        "a: !!str\n",
        "a: !!int\n",
        "%TAG !e! tag:example.com,2000:\n---\na: !e!foo x\n",
        "a: !e!foo x\n",
        "a: !!int 0x10\n",
        "a: !!float .inf\n",
        "a: !!str [a]\n",
        "!!map\na: b\n",
        // documents & directives
        "---\na: 1\n",
        "---\na: 1\n...\n",
        "a: 1\n---\nb: 2\n",
        "a: 1\n--- [\n",
        "%YAML 1.1\n---\na: 1\n",
        "%YAML 1.2\n---\na: 1\n",
        "%YAML 1.1\n%YAML 1.1\n---\na: 1\n",
        "%FOO bar\n---\na: 1\n",
        "---\n",
        "...\n",
        "",
        "\n",
        "# only a comment\n",
        "--- # c\na: 1\n",
        "a: 1 # c\n",
        "a: 1# c\n",
        "a: b #c\n  d\n",
        // whitespace/encodings
        "a: 1\r\nb: 2\r\n",
        "a: 1\rb: 2\r",
        "\u{feff}a: 1\n",
        "a: 1\n\u{feff}b: 2\n",
        "a:\t1\n",
        "\ta: 1\n",
        "a: 1\n\tb: 2\n",
        "a:\n\t- 1\n",
        "a: [1,\t2]\n",
        "a: \"x\ty\"\n",
        "a: x\u{85}y\n",
        "a: x\u{2028}y\n",
        "a: \u{1}\n",
        "a: \u{7f}\n",
        "a: \"\u{1}\"\n",
        "a: 1\n\u{1}\n",
        "a: 1",
        "a: |\n  x",
        "a: \"x",
        // errors
        "a: b: c\n",
        "a:\n  b\n c\n",
        "- a\nb: c\n",
        "a: 1\n- b\n",
        "a: [1, 2\n",
        "a: {b: c\n",
        "a: ]\n",
        "a: }\n",
        "[a\n",
        "a: 'x\n",
        "a: \"x\n",
        "a: @x\n",
        "a: `x\n",
        "a: %x\n",
        "a: &\n",
        "a: *\n",
        "a: !<x\n",
        "a: !!\n",
        "a:\n  - b\n  c: d\n",
        "a: |\n  x\n y\n",
        "key with: colon: here\n",
        "a: b\n c: d\n",
        "a:\n b: c\n  d: e\n",
        "  a: 1\nb: 2\n",
        "a: 1\n  b: 2\n",
        ": a\n",
        "? a\n? b\n",
        "a: [b: c: d]\n",
        "a: {b: c: d}\n",
        "{a: 1}: b\n",
        "a: >2\n x\n",
        "a: |9\n  x\n",
        "a: |x\n",
        "a: \"\\u12\"\n",
        "a: \"\\xZZ\"\n",
        "a: - b\n",
        "a: ? b\n",
        "- - - a\n",
        "a: [- b]\n",
        "- a\n - b\n",
        "a\n",
        "a b c\n",
        "- a\n",
        "1\n",
        "\"x\"\n",
        "[1, 2]\n",
        "null\n",
        "true\n",
        "{}\n",
        "[]\n",
        "a: 1\n...\nb\n",
        "a: 1\n--- x\n",
        "a: !!int 99999999999999999999\n",
        "a:\n  - 1\n  - 2\n b: 3\n",
        "a:\n- 1\n - 2\n",
        "foo:\n  bar: 1\n baz: 2\n",
        "a: \"\\\n\"\n",
        "a: 'x'y\n",
        "a: \"x\"y\n",
        "a: [x]y\n",
        "a: {x: y}z\n",
    ];
    cases.iter().map(|s| s.to_string()).collect()
}

#[test]
fn from_yaml_corpus_matches_helm() {
    let inputs = corpus();
    let Some(m) = diff_inputs(&inputs, &from_yaml_probe, &from_yaml_probe_rust) else {
        return;
    };
    report(&m, inputs.len());
}

#[test]
fn random_documents2_match_helm() {
    let mut r = Rng::new(2024);
    let n: usize = std::env::var("YAML_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let inputs: Vec<String> = (0..n)
        .map(|i| {
            let d = gen_doc2(&mut r);
            if i % 3 == 0 { mutate(&mut r, &d) } else { d }
        })
        .collect();
    let Some(m) = diff_inputs(&inputs, &from_yaml_probe, &from_yaml_probe_rust) else {
        return;
    };
    report(&m, inputs.len());
}

/// The reader validates input in 512-byte chunks, so which error is
/// reported (a syntax error or an invalid character later on) depends on
/// chunk boundaries.
#[test]
fn reader_chunk_boundaries_match_helm() {
    let mut inputs = Vec::new();
    for bad in ["\u{1}", "\u{7f}", "\u{feff}", "\u{85}", "é", "\t"] {
        for pos in (490..540).chain(1010..1040).chain(1530..1545) {
            for prefix in ["a: b: c\n", "a: 1\n", "a: [1\n", "\u{feff}a: 1\n"] {
                let mut s = String::from(prefix);
                while s.len() < pos {
                    s.push_str(if s.len() % 37 == 0 { "\n" } else { "x" });
                }
                s.push_str(bad);
                s.push_str("\nz: 1\n");
                inputs.push(format!("k: |\n  {s}"));
                inputs.push(s);
            }
        }
    }
    let Some(m) = diff_inputs(&inputs, &from_yaml_probe, &from_yaml_probe_rust) else {
        return;
    };
    report(&m, inputs.len());
}

#[test]
fn from_yaml_array_corpus_matches_helm() {
    let mut inputs = corpus();
    inputs.extend(
        [
            "- 1\n- a\n",
            "[1, 2]\n",
            "- a: 1\n- [b]\n",
            "",
            "null\n",
            "- ~\n",
            "x\n",
            "1\n",
            "{a: 1}\n",
            "- \"\\ud800\"\n",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    let Some(m) = diff_inputs(&inputs, &from_yaml_array_probe, &from_yaml_array_probe_rust) else {
        return;
    };
    report(&m, inputs.len());
}

#[test]
fn random_documents_match_helm() {
    let mut r = Rng::new(42);
    let n: usize = std::env::var("YAML_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let inputs: Vec<String> = (0..n).map(|_| gen_doc(&mut r)).collect();
    let Some(m) = diff_inputs(&inputs, &from_yaml_probe, &from_yaml_probe_rust) else {
        return;
    };
    report(&m, inputs.len());
}

#[test]
fn mutated_documents_match_helm() {
    let mut r = Rng::new(7);
    let n: usize = std::env::var("YAML_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let base = corpus();
    let inputs: Vec<String> = (0..n)
        .map(|i| {
            let doc = if i % 2 == 0 {
                gen_doc(&mut r)
            } else {
                base[r.below(base.len())].clone()
            };
            mutate(&mut r, &doc)
        })
        .collect();
    let Some(m) = diff_inputs(&inputs, &from_yaml_probe, &from_yaml_probe_rust) else {
        return;
    };
    report(&m, inputs.len());
}
