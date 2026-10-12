//! Differential tests of the template engine against real Helm, using only
//! Go builtins and Helm's own functions (include, tpl, required, fail).

mod common;
#[path = "common/values.rs"]
mod values;

use std::collections::BTreeMap;

use common::{Oracle, OracleResult, Opts};
use helm_rs::template::{Engine, Renderable};
use helm_rs::{DeterministicEnv, FuncMap, Map, Value};

const VALUES: &str = r#"{
  "s": "hello", "e": "", "n": 3, "f": 1.5, "big": 1000000, "neg": -2, "zero": 0,
  "t": true, "fl": false, "nul": null,
  "list": ["a", "b", "c"], "nums": [1, 2, 3], "empty": [], "emptymap": {},
  "m": {"a": 1, "b": {"c": "deep"}, "z": null},
  "pct": "100%", "html": "<a href='x'>&</a>", "uni": "héllo wörld ✓"
}"#;

/// Renders `templates` (file name → source) with real Helm and with us.
fn both(templates: &[(&str, &str)]) -> Option<(OracleResult, Result<BTreeMap<String, String>, helm_rs::Error>)> {
    let oracle = Oracle::shared()?;
    let mut files = BTreeMap::new();
    files.insert("Chart.yaml".to_string(), "apiVersion: v2\nname: t\nversion: 0.1.0\n".to_string());
    files.insert("values.yaml".to_string(), VALUES.to_string());
    for (n, src) in templates {
        files.insert(format!("templates/{n}"), src.to_string());
    }
    let helm = oracle.lock().unwrap_or_else(|e| e.into_inner()).render_files(&files, "", &Opts::default());

    let vals: serde_json::Value = serde_json::from_str(VALUES).unwrap();
    let mut top = Map::new();
    let helm_rs::Value::Map(v) = values::json_to_value(&vals) else { unreachable!() };
    top.insert("Values".into(), Value::Map(v.with_kind(helm_rs::MapKind::HelmValues)));
    top.insert("Release".into(), helm_rs::map! { "Name" => "release-name", "Namespace" => "default" });
    let top = Value::Map(helm_rs::MapRef::new_values(top));
    let mut engine = Engine::new();
    engine.funcs = FuncMap::new();
    let renderables = templates
        .iter()
        .map(|(n, src)| Renderable {
            name: format!("t/templates/{n}"),
            source: (*src).into(),
            vals: top.clone(),
            base_path: "t/templates".into(),
        })
        .collect();
    let ours = engine.render(&mut DeterministicEnv::default(), renderables);
    Some((helm, ours))
}

fn check(cases: &[&str]) {
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let Some((helm, ours)) = both(&[("t.yaml", case)]) else { return };
        let ok = match (&helm, &ours) {
            (OracleResult::Ok(h), Ok(o)) => h == o,
            (OracleResult::Err { message, .. }, Err(e)) => e.helm_message() == *message,
            _ => false,
        };
        if !ok {
            let o = match &ours {
                Ok(m) => format!("Ok({:?})", m.get("t/templates/t.yaml")),
                Err(e) => format!("Err(helm_message={:?})\n{e}", e.helm_message()),
            };
            let h = match &helm {
                OracleResult::Ok(m) => format!("Ok({:?})", m.get("t/templates/t.yaml")),
                OracleResult::Err { message, .. } => format!("Err({message:?})"),
            };
            failures.push(format!("case {i}: {case:?}\n  helm: {h}\n  ours: {o}"));
        }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

#[test]
fn fields_and_missing_values() {
    check(&[
        "{{ .Values.s }}|{{ .Values.missing }}|{{ .Values.nul }}|{{ .Values.m.z }}",
        "{{ .Values.m.b.c }}",
        "{{ .Values.missing.x }}",
        "{{ .Values.m.missing.x }}",
        "{{ .Values.s.x }}",
        "{{ .Values.n.x }}",
        "{{ .Values.list.x }}",
        "{{ $x := .Values.missing }}{{ $x.foo }}",
        "{{ (.Values.missing).foo }}",
        "{{ .Values.m.a }} {{ .Values.f }} {{ .Values.big }} {{ .Values.neg }} {{ .Values.t }}",
        "{{ .Values.list }} {{ .Values.m }} {{ .Values.empty }} {{ .Values.emptymap }}",
        "{{ .Values }}",
        "{{ .Release.Name }}",
        "{{ .Values.s 1 }}",
        "{{ .Values.m.a.b.c }}",
        "{{ $.Values.s }}",
        "{{ .Values.m.b.c.d }}",
    ]);
}

#[test]
fn control_flow() {
    check(&[
        "{{ if .Values.t }}y{{ else }}n{{ end }}{{ if .Values.fl }}y{{ else if .Values.n }}n3{{ end }}",
        "{{ if .Values.e }}y{{ else }}n{{ end }}{{ if .Values.empty }}y{{ else }}n{{ end }}{{ if .Values.emptymap }}y{{ else }}n{{ end }}{{ if .Values.zero }}y{{ else }}n{{ end }}",
        "{{ with .Values.m }}{{ .a }}{{ end }}{{ with .Values.missing }}x{{ else }}none{{ end }}",
        "{{ with .Values.missing }}x{{ else with .Values.s }}{{ . }}{{ end }}",
        "{{ range .Values.list }}[{{ . }}]{{ end }}",
        "{{ range $i, $v := .Values.list }}{{ $i }}={{ $v }} {{ end }}",
        "{{ range $k, $v := .Values.m }}{{ $k }}:{{ $v }};{{ end }}",
        "{{ range .Values.empty }}x{{ else }}empty{{ end }}",
        "{{ range .Values.missing }}x{{ else }}empty{{ end }}",
        "{{ range .Values.s }}x{{ end }}",
        "{{ range .Values.n }}{{ . }}{{ end }}",
        "{{ range 3 }}{{ . }}{{ end }}",
        "{{ range $i, $x := 3 }}{{ . }}{{ end }}",
        "{{ range .Values.list }}{{ if eq . \"b\" }}{{ break }}{{ end }}{{ . }}{{ end }}",
        "{{ range .Values.list }}{{ if eq . \"b\" }}{{ continue }}{{ end }}{{ . }}{{ end }}",
        "{{ $x := 1 }}{{ range .Values.list }}{{ $x = . }}{{ end }}{{ $x }}",
        "{{ $x := 1 }}{{ if true }}{{ $x := 2 }}{{ $x }}{{ end }}{{ $x }}",
        "{{- range .Values.list -}}\n  {{ . }}\n{{- end }}",
        "a  {{- 1 -}}  b {{- /* comment */ -}} c",
        "{{ range $k, $v := .Values.m }}{{ range $v }}{{ end }}{{ end }}",
        "{{ range $i, $e = .Values.list }}{{ end }}",
        "{{ $i := 0 }}{{ $e := 0 }}{{ range $i, $e = .Values.list }}{{ end }}{{ $i }}{{ $e }}",
    ]);
}

#[test]
fn builtins() {
    check(&[
        "{{ eq .Values.s \"hello\" }} {{ eq .Values.n 3 }} {{ eq .Values.n 3.0 }} {{ ne .Values.s \"x\" }}",
        "{{ eq 1 2 3 1 }} {{ eq \"a\" \"b\" }} {{ eq .Values.missing nil }}",
        "{{ eq .Values.missing \"x\" }} {{ eq .Values.missing .Values.nul }}",
        "{{ eq .Values.list .Values.list }}",
        "{{ eq .Values.m .Values.list }}",
        "{{ eq .Values.t true }} {{ eq .Values.s 1 }}",
        "{{ lt 1 2 }} {{ lt .Values.n 4.0 }} {{ le 2 2 }} {{ gt \"b\" \"a\" }} {{ ge 1.5 1.5 }}",
        "{{ lt .Values.n 4 }}",
        "{{ lt true false }}",
        "{{ lt .Values.missing 1 }}",
        "{{ and 1 0 2 }} {{ and 1 2 }} {{ or 0 \"\" \"x\" }} {{ or 0 \"\" }} {{ and }}",
        "{{ not .Values.t }} {{ not .Values.missing }} {{ not 0 }}",
        "{{ len .Values.list }} {{ len .Values.s }} {{ len .Values.m }} {{ len .Values.uni }}",
        "{{ len .Values.n }}",
        "{{ len .Values.missing }}",
        "{{ .Values.missing | len }}",
        "{{ index .Values.list 1 }} {{ index .Values.m \"a\" }} {{ index .Values.m \"b\" \"c\" }} {{ index .Values.s 0 }}",
        "{{ index .Values.list 3 }}",
        "{{ index .Values.list 5 }}",
        "{{ index .Values.list -1 }}",
        "{{ index .Values.list .Values.n }}",
        "{{ index .Values.m 1 }}",
        "{{ index .Values.missing 1 }}",
        "{{ index .Values.m \"missing\" }}|{{ index .Values.m.missing \"x\" }}",
        "{{ print 1 2 \"a\" \"b\" 3 .Values.nul }} {{ println 1 \"a\" }}",
        "{{ printf \"%v-%s-%d\" .Values.list .Values.s 3 }}",
        "{{ printf \"%q\" .Values.s }}",
        "{{ html .Values.html }} {{ js .Values.html }} {{ urlquery .Values.html .Values.uni }}",
        "{{ html .Values.missing }}",
        "{{ js \"a\\u0001b\" }}",
        "{{ .Values.s | printf \"%s!\" }}",
        "{{ call .Values.s }}",
        "{{ 1 | not }}",
    ]);
}

#[test]
fn literals_and_variables() {
    check(&[
        "{{ 1 }} {{ 1.5 }} {{ 0x10 }} {{ 1e3 }} {{ 'a' }} {{ 0o17 }} {{ 0b101 }} {{ 1_000 }} {{ -5 }} {{ +5 }}",
        "{{ \"a\\tb\" }} {{ `raw\\n` }} {{ true }} {{ nil }}",
        "{{ 9223372036854775807 }}",
        "{{ 9223372036854775808 }}",
        "{{ 18446744073709551616 }}",
        "{{ 1e400 }}",
        "{{ 0x1p4 }} {{ .5 }}",
        "{{ $x := \"a\" }}{{ $y := $x }}{{ $y }}{{ $x = \"b\" }}{{ $x }}{{ $y }}",
        "{{ $x := .Values.m }}{{ $x.b.c }}",
        "{{ $x := 1 }}{{ $x.foo }}",
        "{{ (1) }} {{ (\"a\") }} {{ (.Values.s) }}",
        "{{ $x := . }}{{ $x.Values.s }}",
        "{{ 'ab' }}",
        "{{ '\\x41' }} {{ '\\u00e9' }}",
    ]);
}

#[test]
fn parse_errors() {
    check(&[
        "{{ .Values.s ",
        "{{ if .Values.t }}",
        "{{ end }}",
        "{{ else }}",
        "{{ range }}{{ end }}",
        "{{ $x }}",
        "{{ nofunc }}",
        "{{ .Values.s | 1 }}",
        "{{ (1 }}",
        "{{ 1) }}",
        "{{ \"abc }}",
        "{{ `abc }}",
        "{{ 'ab }}",
        "{{ /* unclosed }}",
        "{{ /* a */ x }}",
        "{{ break }}",
        "{{ define \"x\" }}{{ end }}{{ define \"x\" }}y{{ end }}{{ define \"x\" }}z{{ end }}",
        "{{ template }}",
        "{{ template \"x\" }}",
        "{{ 1.2.3 }}",
        "{{ .a.b. }}",
        "{{ $x := 1 2 }}",
        "{{ a b c }}",
        "{{ 3x }}",
        "{{ .Values.€ }}",
        "{{ @ }}",
        "\n\n{{ if true }}\n{{ else }}\n{{ else }}\n{{ end }}",
        "{{ with $x, $y := . }}{{ end }}",
        "{{ range $a, $b, $c := . }}{{ end }}",
        "{{ $x = 1 }}",
        "{{ true.x }}",
        "{{- -}}",
        "{{ \"a\" | \"b\" }}",
        "{{ := }}",
        "{{ $x := }}",
        "{{ (.Values.s }}",
        "{{ define \"a\" }}{{ define \"b\" }}{{ end }}{{ end }}",
        "{{ else if }}",
        "{{ if 1 }}{{ else if }}{{ end }}",
    ]);
}

#[test]
fn templates_and_helm_functions() {
    check(&[
        "{{ define \"x\" }}X{{ .Values.s }}{{ end }}{{ template \"x\" . }}|{{ template \"x\" }}|{{ include \"x\" . }}",
        "{{ block \"b\" . }}default {{ .Values.s }}{{ end }}",
        "{{ template \"nope\" . }}",
        "{{ include \"nope\" . }}",
        "{{ define \"err\" }}\n\n  {{ .Values.m.missing.x }}{{ end }}a{{ include \"err\" . }}",
        "{{ tpl \"{{ .Values.s }}-{{ .Values.missing }}\" . }}",
        "{{ tpl \"{{ .Values.m.missing.x }}\" . }}",
        "{{ tpl \"{{ .Values.s \" . }}",
        "{{ tpl \"{{ define \\\"z\\\" }}Z{{ end }}{{ include \\\"z\\\" . }}\" . }}",
        "{{ required \"need s\" .Values.s }} {{ required \"need n\" .Values.n }}",
        "{{ required \"need missing\" .Values.missing }}",
        "{{ required \"need e\" .Values.e }}",
        "{{ fail \"boom\" }}",
        "{{ define \"f\" }}{{ fail \"inner boom\" }}{{ end }}{{ include \"f\" . }}",
        "{{ lookup \"v1\" \"Pod\" \"ns\" \"x\" }}{{ lookup \"v1\" \"Pod\" \"ns\" \"x\" | len }}",
        "{{ include \"x\" }}",
        "{{ include 1 . }}",
        "{{ include .Values.missing . }}",
        "{{ tpl .Values.s . }} {{ tpl \"{{ . }}\" 5 }}",
        "{{ define \"d\" }}{{ . }}{{ end }}{{ include \"d\" .Values.missing }}|{{ include \"d\" nil }}",
        "{{ define \"p\" }}{{ .x }}{{ end }}{{ include \"p\" .Values.missing }}",
        "{{ .Template.Name }} {{ .Template.BasePath }}",
    ]);
}

#[test]
fn infinite_recursion_fails_cleanly() {
    let Some((helm, ours)) = both(&[("t.yaml", "{{ define \"r\" }}{{ include \"r\" . }}{{ end }}{{ include \"r\" . }}")]) else { return };
    assert!(matches!(helm, OracleResult::Err { .. }));
    let e = ours.unwrap_err();
    assert!(e.message.contains("exceeded maximum template depth"), "{e}");
}
