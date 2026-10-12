//! Randomized differential testing of the template engine against real Helm,
//! restricted to Go builtins and Helm's include/tpl/required.
//!
//! Knobs: `HELM_RS_FUZZ_SEED` (u64), `HELM_RS_FUZZ_ITERS` (default 300).

#![allow(clippy::result_large_err)]

mod common;
#[path = "common/values.rs"]
mod values;

use std::collections::BTreeMap;

use common::{Opts, Oracle, OracleResult};
use helm_rs::template::{Engine, Renderable};
use helm_rs::{DeterministicEnv, FuncMap, Map, Value};

const VALUES: &str = r#"{
  "s": "hello", "e": "", "n": 3, "f": 1.5, "neg": -2, "zero": 0,
  "t": true, "fl": false,
  "list": ["a", "b", "c"], "nums": [1, 2, 3], "empty": [], "emptymap": {},
  "m": {"a": 1, "b": {"c": "deep"}, "s": "x"}, "tpl": "{{ .Values.s }}!"
}"#;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
}

struct Gen {
    rng: Rng,
    vars: Vec<String>,
    depth: usize,
    in_range: bool,
}

const PATHS: &[&str] = &[
    ".Values.s",
    ".Values.e",
    ".Values.n",
    ".Values.f",
    ".Values.neg",
    ".Values.zero",
    ".Values.t",
    ".Values.fl",
    ".Values.list",
    ".Values.nums",
    ".Values.empty",
    ".Values.emptymap",
    ".Values.m",
    ".Values.m.a",
    ".Values.m.b",
    ".Values.m.b.c",
    ".Values.missing",
    ".Values.m.missing",
    ".Values.missing.x",
    ".Values.s.x",
    ".Values",
    ".Release.Name",
    ".Values.tpl",
];

impl Gen {
    fn literal(&mut self) -> String {
        match self.rng.below(9) {
            0 => self.rng.below(5).to_string(),
            1 => format!("-{}", self.rng.below(3)),
            2 => self.rng.pick(&["1.5", "0.0", "2e2", "0x10", "'a'"]).into(),
            3 => self
                .rng
                .pick(&["\"a\"", "\"hello\"", "\"\"", "`raw`", "\"b c\""])
                .into(),
            4 => self.rng.pick(&["true", "false"]).into(),
            5 => "nil".into(),
            _ => self.rng.pick(&["\"x\"", "1", "\"s\""]).into(),
        }
    }

    fn operand(&mut self) -> String {
        if self.depth > 3 {
            return self.rng.pick(PATHS).into();
        }
        match self.rng.below(10) {
            0..=3 => self.rng.pick(PATHS).into(),
            4 | 5 => self.literal(),
            6 if !self.vars.is_empty() => {
                let v = self.vars[self.rng.below(self.vars.len())].clone();
                if self.rng.chance(30) {
                    format!("{v}.{}", self.rng.pick(&["a", "s", "b", "x"]))
                } else {
                    v
                }
            }
            _ => {
                self.depth += 1;
                let p = format!("({})", self.pipeline());
                self.depth -= 1;
                p
            }
        }
    }

    fn call(&mut self) -> String {
        let f = self.rng.pick(&[
            "eq", "ne", "lt", "le", "gt", "ge", "and", "or", "not", "len", "index", "print",
            "println", "html", "js", "urlquery", "include", "tpl", "required",
        ]);
        let nargs = match f {
            "not" | "len" => 1,
            "ne" | "lt" | "le" | "gt" | "ge" | "include" | "tpl" | "required" => 2,
            "index" => 1 + self.rng.below(3),
            _ => 1 + self.rng.below(3),
        };
        // Occasionally wrong arity.
        let nargs = if self.rng.chance(5) {
            self.rng.below(4)
        } else {
            nargs
        };
        let mut s = String::from(f);
        for i in 0..nargs {
            s.push(' ');
            if f == "include" && i == 0 {
                s.push_str(self.rng.pick(&["\"d1\"", "\"d2\"", "\"nope\""]));
            } else if f == "tpl" && i == 0 {
                s.push_str(self.rng.pick(&[
                    "\"{{ .Values.s }}\"",
                    ".Values.tpl",
                    "\"{{ .x }}\"",
                    "\"{{ \"",
                ]));
            } else if f == "required" && i == 0 {
                s.push_str("\"req\"");
            } else {
                s.push_str(&self.operand());
            }
        }
        s
    }

    fn command(&mut self) -> String {
        match self.rng.below(3) {
            0 => self.operand(),
            _ => self.call(),
        }
    }

    fn pipeline(&mut self) -> String {
        let mut s = self.command();
        if self.rng.chance(30) {
            let f = self.rng.pick(&[
                "not",
                "len",
                "print",
                "html",
                "js",
                "urlquery",
                "index .Values.m",
                "eq \"x\"",
                "and 1",
                "or 0",
            ]);
            s = format!("{s} | {f}");
        }
        s
    }

    fn action(&mut self) -> String {
        let (l, r) = (
            if self.rng.chance(20) { "{{- " } else { "{{ " },
            if self.rng.chance(20) { " -}}" } else { " }}" },
        );
        if self.rng.chance(15) {
            let v = format!("$v{}", self.vars.len());
            let p = self.pipeline();
            self.vars.push(v.clone());
            return format!("{l}{v} := {p}{r}");
        }
        if !self.vars.is_empty() && self.rng.chance(5) {
            let v = self.vars[self.rng.below(self.vars.len())].clone();
            return format!("{l}{v} = {}{r}", self.pipeline());
        }
        format!("{l}{}{r}", self.pipeline())
    }

    fn block(&mut self, budget: usize) -> String {
        let mut out = String::new();
        for _ in 0..budget {
            match self.rng.below(12) {
                0..=2 => out.push_str(self.rng.pick(&["x", " ", "\n", "  y\n", "-"])),
                3..=6 => out.push_str(&self.action()),
                7 if self.depth < 3 => {
                    let kw = self.rng.pick(&["if", "with"]);
                    let saved = self.vars.len();
                    self.depth += 1;
                    let cond = self.pipeline();
                    let body = self.block(2);
                    let els = if self.rng.chance(40) {
                        if self.rng.chance(30) {
                            format!("{{{{ else {kw} {} }}}}{}", self.pipeline(), self.block(1))
                        } else {
                            format!("{{{{ else }}}}{}", self.block(1))
                        }
                    } else {
                        String::new()
                    };
                    self.depth -= 1;
                    self.vars.truncate(saved);
                    out.push_str(&format!("{{{{ {kw} {cond} }}}}{body}{els}{{{{ end }}}}"));
                }
                8 if self.depth < 3 => {
                    let saved = self.vars.len();
                    self.depth += 1;
                    let decl = match self.rng.below(3) {
                        0 => String::new(),
                        1 => {
                            let v = format!("$v{}", self.vars.len());
                            self.vars.push(v.clone());
                            format!("{v} := ")
                        }
                        _ => {
                            let (a, b) = (
                                format!("$v{}", self.vars.len()),
                                format!("$v{}", self.vars.len() + 1),
                            );
                            self.vars.push(a.clone());
                            self.vars.push(b.clone());
                            format!("{a}, {b} := ")
                        }
                    };
                    let src = self.rng.pick(&[
                        ".Values.list",
                        ".Values.nums",
                        ".Values.m",
                        ".Values.empty",
                        ".Values.missing",
                        "3",
                        ".Values.s",
                        ".Values.n",
                    ]);
                    let was = self.in_range;
                    self.in_range = true;
                    let body = self.block(2);
                    self.in_range = was;
                    let els = if self.rng.chance(30) {
                        format!("{{{{ else }}}}{}", self.block(1))
                    } else {
                        String::new()
                    };
                    self.depth -= 1;
                    self.vars.truncate(saved);
                    out.push_str(&format!(
                        "{{{{ range {decl}{src} }}}}{body}{els}{{{{ end }}}}"
                    ));
                }
                9 if self.in_range => {
                    out.push_str(self.rng.pick(&["{{ break }}", "{{ continue }}"]))
                }
                10 => out.push_str(&format!(
                    "{{{{ template {} {} }}}}",
                    self.rng.pick(&["\"d1\"", "\"d2\""]),
                    self.rng.pick(PATHS)
                )),
                _ => out.push_str(&self.action()),
            }
        }
        out
    }
}

fn defines() -> &'static str {
    "{{- define \"d1\" -}}[{{ . }}]{{- end -}}{{- define \"d2\" -}}{{ .Values.s }}{{ .x }}{{- end -}}"
}

fn render_ours(files: &[(String, String)]) -> Result<BTreeMap<String, String>, helm_rs::Error> {
    let vals: serde_json::Value = serde_json::from_str(VALUES).unwrap();
    let mut top = Map::new();
    let helm_rs::Value::Map(v) = values::json_to_value(&vals) else {
        unreachable!()
    };
    top.insert(
        "Values".into(),
        Value::Map(v.with_kind(helm_rs::MapKind::HelmValues)),
    );
    top.insert("Release".into(), helm_rs::map! { "Name" => "release-name" });
    let top = Value::Map(helm_rs::MapRef::new_values(top));
    let mut engine = Engine::new();
    engine.funcs = FuncMap::new();
    let rs = files
        .iter()
        .map(|(n, src)| Renderable {
            name: format!("t/templates/{n}"),
            source: src.as_str().into(),
            vals: top.clone(),
            base_path: "t/templates".into(),
        })
        .collect();
    engine.render(&mut DeterministicEnv::default(), rs)
}

fn render_helm(oracle: &mut Oracle, files: &[(String, String)]) -> OracleResult {
    let mut m = BTreeMap::new();
    m.insert(
        "Chart.yaml".to_string(),
        "apiVersion: v2\nname: t\nversion: 0.1.0\n".to_string(),
    );
    m.insert("values.yaml".to_string(), VALUES.to_string());
    for (n, src) in files {
        m.insert(format!("templates/{n}"), src.clone());
    }
    oracle.render_files(&m, "", &Opts::default())
}

#[test]
fn random_templates_match_helm() {
    let Some(oracle) = Oracle::shared() else {
        return;
    };
    let mut oracle = oracle.lock().unwrap_or_else(|e| e.into_inner());
    let seed: u64 = std::env::var("HELM_RS_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let iters: usize = std::env::var("HELM_RS_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let mut failures = Vec::new();
    for i in 0..iters {
        let case_seed = seed.wrapping_mul(1_000_003).wrapping_add(i as u64);
        let mut g = Gen {
            rng: Rng(case_seed),
            vars: Vec::new(),
            depth: 0,
            in_range: false,
        };
        let body = g.block(4);
        let files = vec![
            ("_helpers.tpl".to_string(), defines().to_string()),
            ("t.yaml".to_string(), body.clone()),
        ];
        let helm = render_helm(&mut oracle, &files);
        let ours = render_ours(&files);
        let ok = match (&helm, &ours) {
            (OracleResult::Ok(h), Ok(o)) => h == o,
            (OracleResult::Err { message, .. }, Err(e)) => *message == e.helm_message(),
            _ => false,
        };
        if !ok {
            failures.push(format!(
                "seed {case_seed}: {body:?}\n  helm: {:?}\n  ours: {:?}",
                match &helm {
                    OracleResult::Ok(h) =>
                        Ok(h.get("t/templates/t.yaml").cloned().unwrap_or_default()),
                    OracleResult::Err { message, .. } => Err(message.clone()),
                },
                ours.as_ref()
                    .map(|o| o.get("t/templates/t.yaml").cloned().unwrap_or_default())
                    .map_err(|e| e.helm_message())
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {iters} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
