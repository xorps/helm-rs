//! End to end: charts whose values.yaml are random documents, rendered by
//! helm-rs (chart loader + template engine + encoding functions) and by real
//! Helm.
mod common;
mod encoding_support;

use std::collections::BTreeMap;

use common::rust::{Outcome, render_files};
use common::{Opts, OracleResult};
use encoding_support::*;

const TEMPLATES: &[(&str, &str)] = &[
    ("y.yaml", "{{ toYaml .Values }}"),
    ("j.yaml", "{{ toJson .Values }}"),
    ("p.yaml", "{{ toYamlPretty .Values }}"),
    ("t.yaml", "{{ .Values | toToml }}"),
    ("rt.yaml", "{{ $m := toYaml .Values | fromYaml }}{{ toJson $m }}"),
    ("jt.yaml", "{{ .Values | toJson | fromJson | toYaml }}"),
    ("ind.yaml", "data:\n{{ toYaml .Values | indent 2 }}"),
    ("each.yaml", "{{ range $k, $v := .Values }}{{ $k }}: {{ toJson $v }} {{ toYaml $v | quote }}\n{{ end }}"),
];

fn chart(values: &str) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    files.insert("Chart.yaml".to_string(), "apiVersion: v2\nname: t\nversion: 0.1.0\n".to_string());
    files.insert("values.yaml".to_string(), values.to_string());
    for (n, t) in TEMPLATES {
        files.insert(format!("templates/{n}"), t.to_string());
    }
    files
}

#[test]
fn random_values_render_like_helm() {
    let Some(_) = common::Oracle::shared() else { return };
    let mut r = Rng::new(555);
    let n: usize = std::env::var("YAML_FUZZ_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    let mut mismatches = Vec::new();
    let mut total = 0;
    for i in 0..n {
        let values = if i % 2 == 0 { gen_doc(&mut r) } else { gen_doc2(&mut r) };
        // Document splitting is the chart loader's business (and currently
        // differs: k8s' YAMLReader keeps a leading `---` line in the first
        // document, and reports a bad separator only when it reaches it);
        // keep single plain documents here.
        if values.lines().any(|l| l.starts_with("---") || l.starts_with('%')) {
            continue;
        }
        let files = chart(&values);
        let helm = common::oracle().map(|mut o| o.render_files(&files, "", &Opts::default()));
        let Some(helm) = helm else { return };
        let ours = render_files(&files, "", &Opts::default());
        total += 1;
        let same = match (&helm, &ours) {
            (OracleResult::Ok(h), Outcome::Ok(o)) => {
                h == o
                    || (h.len() == o.len()
                        && h.iter().all(|(k, v)| o.get(k).is_some_and(|w| w == v || equivalent(&values, v, w))))
            }
            (OracleResult::Err { message, .. }, Outcome::Err(e)) => {
                *message == e.helm_message() || message.contains("unsupported map key")
            }
            _ => false,
        };
        if !same {
            mismatches.push((values.clone(), format!("{helm:?}"), format!("{ours:?}")));
        }
    }
    for (v, h, o) in mismatches.iter().take(5) {
        eprintln!("---- values {v:?}\n  helm: {h}\n  ours: {o}");
    }
    assert!(mismatches.is_empty(), "{} of {total} charts differ", mismatches.len());
}

/// Outputs that may legitimately differ (Go's random map iteration order):
/// same lines in a different order, for key sets goyaml cannot sort
/// consistently.
fn equivalent(values: &str, a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let mut l: Vec<String> = s.lines().map(|l| l.replace("- ", "  ")).collect();
        l.sort();
        l
    };
    norm(a) == norm(b)
        && (has_ambiguous_order(&helm_rs::funcs::encoding::from_yaml(values))
            || helm_rs::yaml::has_key_collisions(values))
}
