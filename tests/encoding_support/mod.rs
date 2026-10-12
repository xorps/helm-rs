//! Shared helpers for the encoding differential tests: batching templates
//! through the oracle, a seeded PRNG and random YAML generators.
#![allow(dead_code)]

use std::collections::BTreeMap;

use helm_rs::Value;
use helm_rs::funcs::encoding as enc;

use crate::common::{Opts, Oracle, OracleResult};

/// Renders many templates (name -> template) with extra chart files in one
/// oracle request. Returns `None` if the oracle is unavailable.
pub fn render_batch(
    templates: &BTreeMap<String, String>,
    files: &BTreeMap<String, String>,
    values: &str,
) -> Option<Result<BTreeMap<String, String>, String>> {
    let oracle = Oracle::shared()?;
    let mut o = oracle.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = BTreeMap::new();
    all.insert("Chart.yaml".to_string(), "apiVersion: v2\nname: t\nversion: 0.1.0\n".to_string());
    all.insert("values.yaml".to_string(), values.to_string());
    for (k, v) in files {
        all.insert(k.clone(), v.clone());
    }
    for (k, v) in templates {
        all.insert(format!("templates/{k}"), v.clone());
    }
    Some(match o.render_files(&all, "", &Opts { skip_schema: true, ..Default::default() }) {
        OracleResult::Ok(out) => Ok(out
            .into_iter()
            .filter_map(|(k, v)| k.strip_prefix("t/templates/").map(|k| (k.to_string(), v)))
            .collect()),
        OracleResult::Err { stage, message } => Err(format!("{stage}: {message}")),
    })
}

/// The template used to probe `fromYaml` on file `data/{name}`.
pub fn from_yaml_probe(name: &str) -> String {
    format!(
        concat!(
            "{{{{- $m := .Files.Get \"data/{0}\" | fromYaml -}}}}",
            "{{{{- if hasKey $m \"Error\" }}}}ERR:{{{{ $m.Error }}}}",
            "{{{{- else }}}}J:{{{{ toJson $m }}}}\nY:{{{{ toYaml $m }}}}\nP:{{{{ toYamlPretty $m }}}}{{{{ end -}}}}"
        ),
        name
    )
}

/// The Rust equivalent of [`from_yaml_probe`].
pub fn from_yaml_probe_rust(input: &str) -> String {
    let m = enc::from_yaml(input);
    if let Some(Value::String(s)) = m.as_map().and_then(|m| m.get("Error")) {
        return format!("ERR:{s}");
    }
    format!("J:{}\nY:{}\nP:{}", enc::to_json(&m), enc::to_yaml(&m), enc::to_yaml_pretty(&m))
}

/// `fromYamlArray` probe.
pub fn from_yaml_array_probe(name: &str) -> String {
    format!(
        "{{{{- $a := .Files.Get \"data/{name}\" | fromYamlArray -}}}}J:{{{{ toJson $a }}}}\nY:{{{{ toYaml $a }}}}\nP:{{{{ toYamlPretty $a }}}}"
    )
}

pub fn from_yaml_array_probe_rust(input: &str) -> String {
    let a = enc::from_yaml_array(input);
    format!("J:{}\nY:{}\nP:{}", enc::to_json(&a), enc::to_yaml(&a), enc::to_yaml_pretty(&a))
}

/// `fromJson` probe.
pub fn from_json_probe(name: &str) -> String {
    format!(
        "{{{{- $m := .Files.Get \"data/{name}\" | fromJson -}}}}{{{{- $j := toJson $m -}}}}J:{{{{ $j }}}}{{{{ if $j }}}}\nR:{{{{ toRawJson $m }}}}{{{{ end }}}}"
    )
}

pub fn from_json_array_probe(name: &str) -> String {
    format!("{{{{- $m := .Files.Get \"data/{name}\" | fromJsonArray -}}}}J:{{{{ toJson $m }}}}")
}

/// Compares Rust results with the oracle for many inputs at once. `probe`
/// builds the template for a data file name; `rust` computes the expected
/// output from the input. Returns the mismatches.
pub fn diff_inputs(
    inputs: &[String],
    probe: &dyn Fn(&str) -> String,
    rust: &dyn Fn(&str) -> String,
) -> Option<Vec<(String, String, String)>> {
    let mut mismatches = Vec::new();
    for chunk in inputs.chunks(200) {
        let mut templates = BTreeMap::new();
        let mut files = BTreeMap::new();
        for (i, input) in chunk.iter().enumerate() {
            let name = format!("i{i:04}");
            files.insert(format!("data/{name}"), input.clone());
            templates.insert(format!("{name}.yaml"), probe(&name));
        }
        let out = match render_batch(&templates, &files, "")? {
            Ok(o) => o,
            Err(e) => panic!("oracle render failed: {e}"),
        };
        for (i, input) in chunk.iter().enumerate() {
            let name = format!("i{i:04}.yaml");
            let want = out.get(&name).cloned().unwrap_or_default();
            let got = rust(input);
            if std::env::var_os("ENCODING_DUMP").is_some() {
                eprintln!("== {input:?}\n{want}");
            }
            if want != got && !nondeterministic_equivalent(input, &want, &got) {
                mismatches.push((input.clone(), want, got));
            }
        }
    }
    Some(mismatches)
}

/// Whether Go's (random map iteration order dependent) result may
/// legitimately differ from ours.
fn nondeterministic_equivalent(input: &str, want: &str, got: &str) -> bool {
    // Several unsupported keys: Go reports whichever it meets first while
    // iterating a map.
    const UNSUPPORTED: &str = "error converting YAML to JSON: unsupported map key";
    if want.contains(UNSUPPORTED) && got.contains(UNSUPPORTED) {
        return true;
    }
    if helm_rs::yaml::has_key_collisions(input) {
        return true;
    }
    // Key orderings that are not total: Go's sort result depends on the
    // (random) map iteration order.
    let sections = |s: &str| -> Option<(String, Vec<String>)> {
        let j = s.strip_prefix("J:")?;
        let (j, rest) = j.split_once("\nY:")?;
        let rest = rest.replace("\nP:", "\n");
        let mut lines: Vec<String> = rest.lines().map(|l| l.replace("- ", "  ")).collect();
        lines.sort();
        Some((j.to_string(), lines))
    };
    match (sections(want), sections(got)) {
        (Some(a), Some(b)) if a == b => has_ambiguous_order(&enc::from_yaml(input)),
        _ => false,
    }
}

pub fn has_ambiguous_order(v: &Value) -> bool {
    match v {
        Value::Map(m) => {
            let keys: Vec<String> = m.borrow().keys().cloned().collect();
            for v3 in [false, true] {
                let less = |a: &str, b: &str| helm_rs::yaml::key_less(a, b, v3);
                for a in &keys {
                    for b in &keys {
                        if a != b && less(a, b) == less(b, a) {
                            return true;
                        }
                        for c in &keys {
                            if less(a, b) && less(b, c) && less(c, a) {
                                return true;
                            }
                        }
                    }
                }
            }
            m.borrow().values().any(has_ambiguous_order)
        }
        Value::List(l) => l.iter().any(has_ambiguous_order),
        _ => false,
    }
}

pub fn report(mismatches: &[(String, String, String)], total: usize) {
    for (input, want, got) in mismatches.iter().take(15) {
        eprintln!("---- input: {input:?}\n  helm: {want:?}\n  rust: {got:?}");
    }
    assert!(mismatches.is_empty(), "{} of {} cases differ from Helm", mismatches.len(), total);
    eprintln!("{total} cases match Helm");
}

// ---------------------------------------------------------------------------
// PRNG.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

// ---------------------------------------------------------------------------
// Random YAML generation.

const SCALAR_ATOMS: &[&str] = &[
    "a",
    "b",
    "foo",
    "bar baz",
    "x-y",
    "x_y",
    "1",
    "0",
    "-1",
    "+1",
    "1.5",
    "-0.0",
    "1e3",
    "1E-3",
    ".5",
    "5.",
    "0x1F",
    "0o17",
    "017",
    "08",
    "0b101",
    "1_000",
    "1__0",
    "_1",
    "1:30",
    "190:20:30",
    "12345678901234567890",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775809",
    "1e400",
    "1e-400",
    ".inf",
    "-.Inf",
    "+.INF",
    ".nan",
    ".NaN",
    "true",
    "True",
    "TRUE",
    "false",
    "yes",
    "No",
    "on",
    "OFF",
    "y",
    "n",
    "Y",
    "null",
    "Null",
    "~",
    "",
    "2001-12-14",
    "2001-12-14t21:59:43.10-05:00",
    "2001-12-14 21:59:43.10",
    "2001-12-14 21:59:43.10 -5",
    "2002-12-14T00:00:00Z",
    "2001-02-30",
    "<<",
    "=",
    "-",
    "--",
    "---",
    "...",
    "a: b",
    "a #b",
    "a#b",
    "#a",
    "@a",
    "`a",
    "!a",
    "&a",
    "*a",
    "%a",
    "|",
    ">",
    "[a]",
    "{a}",
    "a,b",
    "a, b",
    "?",
    "? a",
    "a?",
    ":a",
    "a:",
    "a::b",
    "'",
    "\"",
    "\\",
    "\\n",
    "é",
    "日本",
    "😀",
    "\u{a0}",
    "\u{85}",
    "\u{2028}",
    "\u{feff}x",
    "\t",
    " a",
    "a ",
    "a  b",
    "0.1",
    "3.14159265358979",
    "100000",
    "1000000",
    "1e21",
    "1e20",
    "123456789012",
    "-.5",
    "+.5",
    "0.",
    "00",
    "0x",
    "0xg",
    "1e",
    "e1",
    "1.2.3",
    "1,000",
    "1 000",
    "NaN",
    "Infinity",
    "-inf",
    "nan",
];

fn rand_words(r: &mut Rng) -> String {
    let words = [
        "lorem",
        "ipsum",
        "a",
        "dolor",
        "sit:",
        "amet,",
        "#x",
        "-",
        "é",
        "日本語",
        "x\ty",
        "  ",
        "verylongwordwithoutanyspacesatallxxxxxxxxxxxxxxx",
        "k: v",
        "'q'",
        "\"d\"",
    ];
    let n = 1 + r.below(40);
    let mut s = String::new();
    for i in 0..n {
        if i > 0 {
            s.push(if r.chance(90) { ' ' } else { '\n' });
        }
        s.push_str(r.pick(&words));
    }
    if r.chance(20) {
        s.insert(0, ' ');
    }
    if r.chance(20) {
        s.push(' ');
    }
    if r.chance(10) {
        s.push('\n');
    }
    s
}

fn rand_chars(r: &mut Rng) -> String {
    let chars = [
        'a',
        'b',
        ' ',
        ' ',
        '\n',
        '\t',
        '\u{85}',
        '\u{2028}',
        '\u{2029}',
        '\u{feff}',
        '\u{a0}',
        '\u{1}',
        '\u{7f}',
        '\u{9f}',
        '\u{fffe}',
        '\u{1F600}',
        'é',
        '\\',
        '"',
        '\'',
        '\r',
        '#',
        ':',
        '-',
        '\u{e000}',
        '\u{d7ff}',
        '\u{1b}',
    ];
    let n = 1 + r.below(12);
    (0..n).map(|_| *r.pick(&chars)).collect()
}

/// A random string value (may contain anything; only usable quoted).
pub fn rand_string(r: &mut Rng) -> String {
    match r.below(3) {
        0 => rand_words(r),
        1 => rand_chars(r),
        _ => rand_atom(r),
    }
}

fn rand_atom(r: &mut Rng) -> String {
    if r.chance(15) {
        // random printable soup
        let chars = [
            'a', 'Z', '0', '9', ' ', '-', ':', '#', ',', '[', ']', '{', '}', '\'', '"', '\\', '.', '!', '&', '*', '?',
            '|', '>', '%', '@', '`', 'é', '\t', '~', '<', '=', '_', '/',
        ];
        let n = 1 + r.below(8);
        return (0..n).map(|_| *r.pick(&chars)).collect();
    }
    r.pick(SCALAR_ATOMS).to_string()
}

/// Quotes a string in YAML double-quoted style, escaping as needed.
pub fn dq(s: &str, r: &mut Rng) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str(if r.chance(50) { "\\t" } else { "\t" }),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if r.chance(3) && (c as u32) > 0x7f && (c as u32) < 0x10000 => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn scalar_text(r: &mut Rng, s: &str) -> String {
    match r.below(10) {
        0..=5 => s.to_string(),
        6 | 7 => dq(s, r),
        _ => sq(s),
    }
}

fn indent_str(n: usize) -> String {
    " ".repeat(n)
}

/// Generates a random YAML node at the given indentation, as text that
/// starts right after "key:" or "-" (i.e. either " scalar" inline or a
/// newline followed by an indented block).
fn gen_node(r: &mut Rng, ind: usize, depth: usize, anchors: &mut Vec<String>) -> String {
    let mut prefix = String::new();
    if r.chance(6) {
        let name = format!("a{}", r.below(5));
        prefix.push_str(&format!(" &{name}"));
        anchors.push(name);
    }
    if r.chance(5) {
        let tags = [
            "!!str",
            "!!int",
            "!!float",
            "!!bool",
            "!!null",
            "!!binary",
            "!foo",
            "!",
            "!!map",
            "!!seq",
            "!<tag:yaml.org,2002:str>",
            "!!timestamp",
        ];
        prefix.push(' ');
        prefix.push_str(r.pick(&tags));
    }
    if !anchors.is_empty() && r.chance(5) {
        return format!(" *{}", r.pick(anchors));
    }
    let choice = if depth > 4 { 0 } else { r.below(10) };
    match choice {
        0..=3 => {
            let a = rand_atom(r);
            format!("{prefix} {}", scalar_text(r, &a))
        }
        4 => {
            let s = rand_string(r);
            format!("{prefix} {}", dq(&s, r))
        }
        5 => {
            // block scalar
            let style = *r.pick(&["|", ">", "|-", ">-", "|+", ">+", "|2", ">1-", "|+2"]);
            let mut s = format!("{prefix} {style}\n");
            let n = r.below(4);
            for _ in 0..n {
                let extra = if r.chance(20) { "  " } else { "" };
                if r.chance(15) {
                    s.push('\n');
                    continue;
                }
                s.push_str(&format!("{}{}{}\n", indent_str(ind + 2), extra, rand_atom(r).replace('\n', " ")));
            }
            s.pop();
            s
        }
        6 => {
            // flow collection
            let mut s = format!("{prefix} ");
            s.push_str(&gen_flow(r, depth));
            s
        }
        7 | 8 => {
            // block mapping
            let n = 1 + r.below(4);
            let mut s = prefix.clone();
            let child_ind = ind + if r.chance(80) { 2 } else { 1 + r.below(4) };
            for i in 0..n {
                let key = if r.chance(10) { "<<".to_string() } else { rand_key(r, i) };
                s.push('\n');
                s.push_str(&indent_str(child_ind));
                s.push_str(&key);
                s.push(':');
                s.push_str(&gen_node(r, child_ind, depth + 1, anchors));
            }
            s
        }
        _ => {
            // block sequence (indented or not)
            let n = 1 + r.below(4);
            let mut s = prefix.clone();
            let child_ind = if r.chance(50) { ind } else { ind + 2 };
            for _ in 0..n {
                s.push('\n');
                s.push_str(&indent_str(child_ind));
                s.push('-');
                let node = gen_node(r, child_ind + 2, depth + 1, anchors);
                s.push_str(&node);
            }
            s
        }
    }
}

fn rand_key(r: &mut Rng, i: usize) -> String {
    match r.below(14) {
        12 => {
            let s = rand_string(r);
            dq(&s, r)
        }
        13 => {
            let n = *r.pick(&[60usize, 120, 125, 126, 127, 128, 129, 130, 1000, 1030]);
            format!("{}{}", "k".repeat(n), i)
        }
        0 => format!("{}", r.below(100)),
        1 => sq(&rand_atom(r)),
        2 => {
            let a = rand_atom(r);
            dq(&a, r)
        }
        3 => r
            .pick(&[
                "true",
                "yes",
                "~",
                "null",
                "1.5",
                "0x10",
                "1e3",
                "-1",
                ".inf",
                "18446744073709551615",
                "2001-01-01",
            ])
            .to_string(),
        _ => format!("k{}{}", i, r.pick(&["", "a", "_b", "-c", "10", "2", "B", "é"])),
    }
}

fn gen_flow(r: &mut Rng, depth: usize) -> String {
    let n = r.below(4);
    if depth > 4 || r.chance(50) {
        let items: Vec<String> = (0..n)
            .map(|_| {
                if depth < 4 && r.chance(25) {
                    gen_flow(r, depth + 1)
                } else {
                    let a = rand_atom(r);
                    match r.below(3) {
                        0 => a,
                        1 => dq(&a, r),
                        _ => sq(&a),
                    }
                }
            })
            .collect();
        format!("[{}]", items.join(if r.chance(50) { ", " } else { "," }))
    } else {
        let items: Vec<String> = (0..n)
            .map(|i| {
                let v = if depth < 4 && r.chance(25) { gen_flow(r, depth + 1) } else { dq(&rand_atom(r), r) };
                format!("{}: {}", rand_key(r, i), v)
            })
            .collect();
        format!("{{{}}}", items.join(", "))
    }
}

/// A random YAML document whose top level is a mapping.
pub fn gen_doc(r: &mut Rng) -> String {
    let mut anchors = Vec::new();
    let n = 1 + r.below(5);
    let mut s = String::new();
    if r.chance(10) {
        s.push_str("---\n");
    }
    if r.chance(5) {
        s.push_str("# comment\n");
    }
    for i in 0..n {
        let key = rand_key(r, i);
        s.push_str(&key);
        s.push(':');
        s.push_str(&gen_node(r, 0, 1, &mut anchors));
        if r.chance(10) {
            s.push_str(" # trailing");
        }
        s.push('\n');
    }
    if r.chance(5) {
        s.push_str("...\n");
    }
    if r.chance(5) {
        s = s.replace('\n', "\r\n");
    }
    s
}

/// A second, more syntax-oriented generator: multi-line scalars, explicit
/// keys, compact sequences, multi-line flow collections, directives,
/// comments in odd places, several documents.
pub fn gen_doc2(r: &mut Rng) -> String {
    let mut s = String::new();
    if r.chance(10) {
        s.push_str(r.pick(&[
            "%YAML 1.1\n---\n",
            "%TAG !e! tag:e.com,2000:\n---\n",
            "--- !!map\n",
            "--- # c\n",
            "---\n",
        ]));
    }
    let mut anchors: Vec<String> = Vec::new();
    let n = 1 + r.below(5);
    for i in 0..n {
        gen_entry2(r, &mut s, 0, i, 0, &mut anchors);
    }
    if r.chance(10) {
        s.push_str(r.pick(&["...\n", "---\nx: 1\n", "--- [\n", "...\n---\n- a\n", "# end"]));
    }
    s
}

fn gen_key2(r: &mut Rng, i: usize) -> String {
    match r.below(10) {
        0 => format!("&k{} key{i}", r.below(3)),
        1 => format!("\"quoted {i}\""),
        2 => format!("'single {i}'"),
        3 => format!("{}", r.below(20)),
        _ => format!("key{i}{}", r.pick(&["", "_x", "-y", "Z"])),
    }
}

fn gen_entry2(r: &mut Rng, s: &mut String, ind: usize, i: usize, depth: usize, anchors: &mut Vec<String>) {
    let pad = " ".repeat(ind);
    if r.chance(5) {
        s.push_str(&format!("{pad}{}# comment {i}\n", " ".repeat(r.below(4))));
    }
    if r.chance(8) {
        // explicit key
        s.push_str(&format!("{pad}? {}\n{pad}:", gen_key2(r, i)));
    } else {
        s.push_str(&format!("{pad}{}:", gen_key2(r, i)));
    }
    gen_value2(r, s, ind, depth, anchors);
}

fn gen_value2(r: &mut Rng, s: &mut String, ind: usize, depth: usize, anchors: &mut Vec<String>) {
    let pad = " ".repeat(ind);
    let choice = if depth > 3 { r.below(5) } else { r.below(11) };
    match choice {
        0 => s.push_str(&format!(" {}\n", if r.chance(50) { rand_word() } else { rand_atom(r) })),
        1 => {
            // multi-line plain scalar
            s.push_str(&format!(" {}\n", rand_word()));
            for _ in 0..1 + r.below(3) {
                if r.chance(30) {
                    s.push('\n');
                }
                s.push_str(&format!("{pad}{}{}\n", " ".repeat(1 + r.below(3)), rand_word()));
            }
        }
        2 => {
            // multi-line quoted scalar
            let q = if r.chance(50) { '"' } else { '\'' };
            let esc = |w: String| {
                if q == '"' { w.replace('\\', "\\\\").replace('"', "\\\"") } else { w.replace('\'', "''") }
            };
            s.push_str(&format!(" {q}{}", esc(rand_word())));
            for _ in 0..1 + r.below(3) {
                s.push('\n');
                if r.chance(30) {
                    s.push('\n');
                }
                s.push_str(&format!("{pad}{}{}", " ".repeat(r.below(4)), esc(rand_word())));
                if q == '"' && r.chance(20) {
                    s.push('\\');
                }
            }
            s.push_str(&format!("{q}\n"));
        }
        3 => {
            // block scalar with indentation indicator / chomping
            let ind_ind = r.below(3);
            let chomp = *r.pick(&["", "-", "+"]);
            let header = if ind_ind > 0 { format!("{ind_ind}{chomp}") } else { chomp.to_string() };
            let style = if r.chance(50) { '|' } else { '>' };
            s.push_str(&format!(" {style}{header}\n"));
            let base = ind + if ind_ind > 0 { ind_ind } else { 2 };
            for _ in 0..r.below(4) {
                let extra = if r.chance(25) { 2 } else { 0 };
                if r.chance(20) {
                    s.push('\n');
                } else {
                    s.push_str(&format!("{}{}\n", " ".repeat(base + extra), rand_word()));
                }
            }
        }
        4 => s.push_str(&format!(" {}\n", gen_flow_multiline(r, ind, 0))),
        5 | 6 => {
            // nested block mapping
            s.push('\n');
            let child = ind + *r.pick(&[1usize, 2, 2, 2, 4]);
            for i in 0..1 + r.below(3) {
                gen_entry2(r, s, child, i, depth + 1, anchors);
            }
        }
        7 | 8 => {
            // block sequence (indentless or indented), maybe compact
            s.push('\n');
            let child = if r.chance(50) { ind } else { ind + 2 };
            let cpad = " ".repeat(child);
            for i in 0..1 + r.below(3) {
                match r.below(4) {
                    0 => {
                        // compact nested sequence
                        s.push_str(&format!("{cpad}- - {}\n{cpad}  - {}\n", rand_word(), rand_word()));
                    }
                    1 => {
                        // sequence of mappings
                        s.push_str(&format!("{cpad}- {}:", gen_key2(r, i)));
                        gen_value2(r, s, child + 2, depth + 1, anchors);
                        if r.chance(50) {
                            s.push_str(&format!("{cpad}  other{i}: {}\n", rand_atom(r)));
                        }
                    }
                    _ => {
                        s.push_str(&format!("{cpad}-"));
                        gen_value2(r, s, child + 2, depth + 1, anchors);
                    }
                }
            }
        }
        9 => {
            // anchor + tag on a collection
            let name = format!("n{}", r.below(3));
            s.push_str(&format!(" &{name} {}\n", r.pick(&["!!map", "!!map", "!custom", ""])));
            let child = ind + 2;
            for i in 0..1 + r.below(2) {
                gen_entry2(r, s, child, i, depth + 1, anchors);
            }
            anchors.push(name);
        }
        _ => {
            if anchors.is_empty() || r.chance(5) {
                s.push_str(&format!(" {}\n", rand_atom(r)));
            } else {
                let a = r.pick(anchors).clone();
                s.push_str(&format!(" *{a}\n"));
            }
        }
    }
}

fn rand_word() -> String {
    // deterministic per call site is fine; vary via a simple counter
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let words = [
        "alpha",
        "beta gamma",
        "delta",
        "x y z",
        "1.5",
        "yes",
        "é",
        "alpha",
        "beta",
        "gamma delta",
        "x: y",
        "#not",
        "a #c",
        "-dash",
        "tab\there",
        "~",
        "'q'",
        "\"d\"",
    ];
    words[N.fetch_add(1, Ordering::Relaxed) % words.len()].to_string()
}

fn gen_flow_multiline(r: &mut Rng, ind: usize, depth: usize) -> String {
    let pad = " ".repeat(ind + 1);
    let sep = if r.chance(40) { format!(",\n{pad}") } else { ", ".to_string() };
    let n = r.below(4);
    if r.chance(50) {
        let items: Vec<String> = (0..n)
            .map(|_| if depth < 3 && r.chance(25) { gen_flow_multiline(r, ind + 1, depth + 1) } else { rand_atom(r) })
            .collect();
        format!("[{}{}]", items.join(&sep), if r.chance(20) { "," } else { "" })
    } else {
        let items: Vec<String> = (0..n)
            .map(|i| {
                let v = if depth < 3 && r.chance(25) {
                    gen_flow_multiline(r, ind + 1, depth + 1)
                } else {
                    dq(&rand_atom(r), r)
                };
                if r.chance(15) { format!("? k{i} : {v}") } else { format!("k{i}: {v}") }
            })
            .collect();
        format!("{{{}}}", items.join(&sep))
    }
}

/// Randomly corrupts a document (to exercise error paths).
pub fn mutate(r: &mut Rng, s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    let edits = 1 + r.below(3);
    for _ in 0..edits {
        if chars.is_empty() {
            break;
        }
        let pos = r.below(chars.len());
        let ins = [
            '\t', ' ', ':', '-', '"', '\'', '[', ']', '{', '}', '\n', '#', '&', '*', '!', '|', '>', '?', ',', '%',
            '\u{1}', '\u{7f}', '\u{feff}', '\r', '@',
        ];
        match r.below(3) {
            0 => {
                chars.remove(pos);
            }
            1 => chars.insert(pos, *r.pick(&ins)),
            _ => chars[pos] = *r.pick(&ins),
        }
    }
    chars.into_iter().collect()
}
