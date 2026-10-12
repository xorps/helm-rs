//! Chart loading, values and dependency processing, manifest sorting and
//! `helm template` output, diffed against real Helm (the oracle).
//!
//! Fixtures are written in JSON (valid YAML) and loaded with a JSON parser,
//! so these tests do not depend on the YAML implementation.

mod common;

use std::collections::BTreeMap;

use common::chart::*;
use helm_rs::chart::{self, Chart, RenderOptions};
use helm_rs::template::Engine;
use helm_rs::{DeterministicEnv, Value};
use serde_json::{Value as Json, json};

type Files = BTreeMap<String, Vec<u8>>;

fn files(list: &[(&str, &str)]) -> Files {
    list.iter().map(|(k, v)| (k.to_string(), v.as_bytes().to_vec())).collect()
}

#[allow(clippy::result_large_err)]
fn load(files: &Files) -> Result<Chart, helm_rs::Error> {
    Chart::from_files_with_parser(files.clone(), json_yaml)
}

fn rust_tree(c: &Chart, full_path: &str, sort: bool) -> Json {
    let mut deps: Vec<&Chart> = c.dependencies.iter().collect();
    if sort {
        deps.sort_by(|a, b| a.name().cmp(b.name()));
    }
    let deps: Vec<Json> =
        deps.iter().map(|d| rust_tree(d, &format!("{full_path}/charts/{}", d.name()), sort)).collect();
    let fdata = |fs: &[chart::File]| -> Json {
        Json::Object(fs.iter().map(|f| (f.name.clone(), json!(String::from_utf8_lossy(&f.data)))).collect())
    };
    let crds: Vec<String> = if full_path.contains('/') { vec![] } else { c.crd_objects().into_iter().map(|x| x.0).collect() };
    json!({
        "name": c.name(),
        "full_path": full_path,
        "metadata": value_to_json(&c.metadata_as_map()),
        "values": value_to_json(&Value::Map(c.values.clone())),
        "templates": c.templates.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
        "template_data": fdata(&c.templates),
        "files": c.files.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
        "file_data": fdata(&c.files),
        "schema": c.schema.as_ref().map(|s| String::from_utf8_lossy(s).into_owned()),
        "deps": deps,
        "crds": crds,
        "is_library": c.is_library(),
        "metadata_nil_deps": c.metadata.dependencies.is_none(),
    })
}

/// Oracle tree, with fields we don't model removed.
fn oracle_tree(mut j: Json, root: bool) -> Json {
    let o = j.as_object_mut().unwrap();
    o.remove("raw");
    o.remove("lock");
    if !root {
        o.insert("crds".into(), json!([]));
    }
    if let Some(Json::Array(deps)) = o.get_mut("deps") {
        for d in deps.iter_mut() {
            *d = oracle_tree(d.take(), false);
        }
    }
    // Go nil maps/values print as null; we model them as empty maps.
    if o.get("values") == Some(&Json::Null) {
        o.insert("values".into(), json!({}));
    }
    if let Some(Json::Object(md)) = o.get_mut("metadata") {
        if md.get("Annotations") == Some(&Json::Null) {
            md.insert("Annotations".into(), json!({}));
        }
    }
    norm(&j)
}

fn check_load(f: &Files) {
    let Some(want) = oracle_op(files_req("load", f)) else { return };
    let got = load(f);
    match (want, got) {
        (Ok(w), Ok(c)) => {
            let w = oracle_tree(w, true);
            let g = rust_tree(&c, c.name(), true);
            assert_eq!(g, w, "\nrust:   {}\noracle: {}", g, w);
        }
        (Err(w), Err(e)) => assert_eq!(e.helm_message(), w),
        (w, g) => panic!("mismatch: oracle {w:?}, rust {g:?}"),
    }
}

fn base_chart() -> Files {
    files(&[
        ("Chart.yaml", r#"{"apiVersion": "v2", "name": "parent", "version": "1.0.0", "appVersion": "2.0",
          "keywords": ["a", "b"], "maintainers": [{"name": "me", "email": "me@x", "url": "u"}],
          "annotations": {"k": "v"}, "kubeVersion": ">=1.20.0-0",
          "dependencies": [
            {"name": "sub1", "version": "0.1.0", "repository": "file://x", "condition": "sub1.enabled", "tags": ["t1"]},
            {"name": "sub1", "alias": "sub1copy", "version": "0.1.0", "import-values": ["data", {"child": "exp", "parent": "imported"}]},
            {"name": "sub2", "condition": "sub2.on,global.sub2", "import-values": [{"child": "deep.x", "parent": "a.b"}]}
          ]}"#),
        ("values.yaml", r#"{"global": {"g": 1, "gm": {"x": 1}}, "top": {"a": 1, "n": null}, "sub1": {"enabled": true, "v": "parent"}, "imported": {"keep": 1}}"#),
        ("templates/a.yaml", "a: {{ .Values.top.a }}"),
        ("templates/_helpers.tpl", "{{ define \"x\" }}x{{ end }}"),
        ("templates/.hidden", "hidden"),
        ("templates/NOTES.txt", "notes"),
        ("crds/crd.yaml", "kind: CustomResourceDefinition"),
        ("crds/readme.md", "x"),
        ("files/a.txt", "hello"),
        ("values.schema.json", r#"{"type": "object"}"#),
        (".helmignore", "*.ignored\nsecret/\n"),
        ("x.ignored", "no"),
        ("secret/s.txt", "no"),
        ("charts/sub1/Chart.yaml", r#"{"apiVersion": "v2", "name": "sub1", "version": "0.1.0"}"#),
        ("charts/sub1/values.yaml", r#"{"v": "sub1", "exports": {"data": {"exported": true}}, "exp": {"e": 1}, "nul": null}"#),
        ("charts/sub1/templates/s.yaml", "s: {{ .Values.v }}"),
        ("charts/sub2/Chart.yaml", r#"{"apiVersion": "v2", "name": "sub2", "version": "0.2.0", "dependencies": [{"name": "leaf", "condition": "leaf.enabled"}]}"#),
        ("charts/sub2/values.yaml", r#"{"on": true, "deep": {"x": {"y": 2}}, "leaf": {"enabled": false}}"#),
        ("charts/sub2/charts/leaf/Chart.yaml", r#"{"apiVersion": "v1", "name": "leaf", "version": "1.0.0"}"#),
        ("charts/sub2/charts/leaf/crds/leafcrd.yml", "kind: CustomResourceDefinition"),
        ("charts/sub2/charts/leaf/templates/l.yaml", r#"{"l": 1}"#),
        ("charts/sub2/charts/leaf/requirements.yaml", r#"{"dependencies": []}"#),
    ])
}

#[test]
fn load_matches_helm() {
    if !semver_ready() {
        return;
    }
    check_load(&base_chart());
}

#[test]
fn load_tgz_subchart_matches_helm() {
    if !semver_ready() {
        return;
    }
    let mut f = base_chart();
    let t = tar(&[
        ("sub3/", None),
        ("sub3/Chart.yaml", Some(br#"{"apiVersion": "v2", "name": "sub3", "version": "3.0.0", "type": "library"}"#)),
        ("sub3/templates/_lib.tpl", Some(b"{{ define \"lib\" }}lib{{ end }}")),
        ("sub3/templates/nonlib.yaml", Some(b"x: 1")),
        ("sub3/values.yaml", Some(b"\xEF\xBB\xBF{\"libv\": 1}")),
    ]);
    f.insert("charts/sub3-3.0.0.tgz".into(), gzip(&t));
    check_load(&f);
}

#[test]
fn load_archive_matches_helm() {
    if !semver_ready() {
        return;
    }
    let t = tar(&[
        ("mychart/Chart.yaml", Some(br#"{"apiVersion": "v2", "name": "mychart", "version": "0.1.0"}"#)),
        ("mychart/templates/a.yaml", Some(b"a: 1")),
        ("mychart/charts/sub/Chart.yaml", Some(br#"{"apiVersion": "v2", "name": "sub", "version": "0.1.0"}"#)),
    ]);
    let tgz = gzip(&t);
    let Some(want) = oracle_op(json!({"op": "load_archive", "files_b64": {"archive": b64(&tgz)}})) else { return };
    let c = Chart::from_tgz_with_parser(&tgz, json_yaml).unwrap();
    assert_eq!(rust_tree(&c, c.name(), true), oracle_tree(want.unwrap(), true));

    for bad in [
        tar(&[("Chart.yaml", Some(b"x"))]),
        tar(&[("a/../../x", Some(b"x"))]),
        tar(&[("a/Chart.yaml", Some(b"{}"))]),
        tar(&[]),
        t[..520].to_vec(),
        t[..600].to_vec(),
    ] {
        let tgz = gzip(&bad);
        let want = oracle_op(json!({"op": "load_archive", "files_b64": {"archive": b64(&tgz)}})).unwrap();
        match (want, Chart::from_tgz_with_parser(&tgz, json_yaml)) {
            (Ok(w), Ok(c)) => assert_eq!(rust_tree(&c, c.name(), true), oracle_tree(w, true)),
            (Err(w), Err(g)) => assert_eq!(g.helm_message(), w),
            (w, g) => panic!("mismatch: oracle {w:?}, rust {g:?}"),
        }
    }
    for raw in [&b""[..], b"\x1f\x8b\x08", b"plain text"] {
        let want = oracle_op(json!({"op": "load_archive", "files_b64": {"archive": b64(raw)}})).unwrap().unwrap_err();
        let got = Chart::from_tgz_with_parser(raw, json_yaml).unwrap_err();
        assert_eq!(got.helm_message(), want);
    }
}

#[test]
fn chart_yaml_errors_match_helm() {
    if !semver_ready() {
        return;
    }
    let cases = [
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "deprecated": "yes"}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "maintainers": "foo"}"#,
        r#"{"apiVersion": "v2", "name": {"a": 1}, "version": "0.1.0"}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": [{"name": "x", "enabled": 3}]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "annotations": {"a": [1]}}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "maintainers": [{"name": [1]}]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "maintainers": ["x"]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "keywords": [[1]]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "keywords": 3}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": {"a": 1}}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": [{"name": "x", "import-values": "a"}]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": [{"name": "x", "tags": "a"}]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": [1]}"#,
        r#"["a"]"#,
        r#""str""#,
        "",
        r#"{"name": "t", "version": 1}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": [null]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "maintainers": [null]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": [{"name": "x", "alias": "a.b"}]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": [{"name": "x", "alias": "ab"}, {"name": "ab"}]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "type": "foo"}"#,
        r#"{"apiVersion": "v2", "name": "../t", "version": "0.1.0"}"#,
        r#"{"apiVersion": "v2", "name": "..", "version": "0.1.0"}"#,
        r#"{"apiVersion": "v2", "name": "t"}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "not-semver"}"#,
        r#"{"apiVersion": "v2", "name": "t\tx\u0001y", "version": "0.1.0", "description": "a\nb"}"#,
        r#"{"APIVERSION": "v2", "Name": "t", "NAME": "u", "version": "0.1.0", "AppVersion": 1.5, "app_version": "x", "Deprecated": true}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "keywords": [1, 1.5, true, null], "annotations": {"a": 1, "b": null, "c": false}}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": [{"name": "x", "import-values": ["a", {"child": 1, "parent": "p"}, 3, null]}]}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "annotations": {"a/b~c": [1]}}"#,
        r#"{"apiVersion": "v2", "name": "t", "version": "0.1.0", "dependencies": [{"name": "x", "tags": ["a", [1]]}]}"#,
    ];
    for c in cases {
        let f = files(&[("Chart.yaml", c), ("templates/a.yaml", "a: 1")]);
        eprintln!("case {c}");
        check_load(&f);
    }
}

#[test]
fn loader_errors_match_helm() {
    if !semver_ready() {
        return;
    }
    let ok_chart = r#"{"apiVersion": "v2", "name": "p", "version": "0.1.0"}"#;
    let cases: Vec<Files> = vec![
        files(&[("templates/a.yaml", "a")]),
        files(&[("requirements.yaml", r#"{"dependencies": []}"#)]),
        files(&[("Chart.yaml", ok_chart), ("values.yaml", "[1]")]),
        files(&[("Chart.yaml", ok_chart), ("values.yaml", "\"s\"")]),
        files(&[("Chart.yaml", ok_chart), ("values.yaml", "{\"a\": 1}\n--- x\n")]),
        files(&[("Chart.yaml", ok_chart), ("values.yaml", "{\"a\": {\"b\": 1, \"c\": 2}}\n---\n{\"a\": {\"b\": 3}}\n---\n\n---  # c\nnull\n")]),
        files(&[("Chart.yaml", ok_chart), ("charts/README.md", "x")]),
        files(&[("Chart.yaml", ok_chart), ("charts/sub/Chart.yaml", r#"{"apiVersion": "v2", "name": "sub"}"#)]),
        files(&[("Chart.yaml", ok_chart), ("charts/sub.tgz/Chart.yaml", ok_chart)]),
        files(&[("Chart.yaml", ok_chart), ("charts/_skip/x", "x"), ("charts/.hidden/x", "x"), ("charts/a.prov", "p")]),
        files(&[("Chart.yaml", ok_chart), ("Chart.lock", r#"{"digest": "d", "dependencies": [{"name": "a"}]}"#)]),
        files(&[("Chart.yaml", ok_chart), ("Chart.lock", r#"{"digest": 1}"#)]),
        files(&[("Chart.yaml", r#"{"apiVersion": "v1", "name": "p", "version": "0.1.0"}"#), ("requirements.yaml", r#"{"dependencies": [{"name": "a"}]}"#), ("requirements.lock", "{}")]),
        files(&[("Chart.yaml", ok_chart), ("requirements.yaml", r#"{"dependencies": [{"name": "a"}]}"#)]),
        files(&[("Chart.yaml", ok_chart), (".helmignore", "a/**\n")]),
        files(&[("Chart.yaml", ok_chart), (".helmignore", "[\n")]),
        files(&[("Chart.yaml", ok_chart), (".helmignore", "!templates\n/Chart.yaml\n"), ("templates/a.yaml", "a"), ("b.txt", "b")]),
        files(&[("Chart.yaml", ok_chart), (".helmignore", "templates/\n!x\n"), ("templates/a.yaml", "a"), ("x", "x")]),
        files(&[("Chart.yaml", ok_chart), ("templates/sub/.x/y.yaml", "a"), ("templates/a.yaml", "\u{feff}bom"), ("crds/a-b.yaml", "1"), ("crds/a/x.yaml", "2")]),
    ];
    for f in &cases {
        eprintln!("case {:?}", f.keys().collect::<Vec<_>>());
        check_load(f);
    }
}

fn rust_values(f: &Files, user: &str) -> Result<Json, String> {
    let c = load(f).map_err(|e| e.helm_message())?;
    let user = json_yaml(user).map_err(|e| e.message)?;
    let user = if user.is_nil() { Value::empty_map() } else { user };
    let mut c2 = c.clone();
    let uv = user.as_map().unwrap().clone();
    chart::process_dependencies(&mut c2, &uv).map_err(|e| e.helm_message())?;
    let opts = RenderOptions { skip_schema_validation: true, ..Default::default() };
    let caps = opts.capabilities.to_object().unwrap();
    let top = chart::to_render_values(&c2, &user, &opts, &caps).map_err(|e| e.helm_message())?;
    Ok(json!({
        "values": value_to_json(&top.get("Values").unwrap()),
        "chart": value_to_json(&top.get("Chart").unwrap()),
        "tree": rust_tree(&c2, c2.name(), false),
    }))
}

fn check_values(f: &Files, user: &str) {
    let mut req = files_req("chart_values", f);
    req["values"] = json!(user);
    req["skip_schema"] = json!(true);
    let Some(want) = oracle_op(req) else { return };
    let got = rust_values(f, user);
    match (want, got) {
        (Ok(mut w), Ok(g)) => {
            w["tree"] = oracle_tree(w["tree"].take(), true);
            if let Some(md) = w["chart"].as_object_mut() {
                if md.get("Annotations") == Some(&Json::Null) {
                    md.insert("Annotations".into(), json!({}));
                }
            }
            let w = norm(&w);
            assert_eq!(g["values"], w["values"], "values (user {user})");
            assert_eq!(g["chart"], w["chart"], "chart (user {user})");
            assert_eq!(g["tree"], w["tree"], "tree (user {user})");
        }
        (Err(w), Err(g)) => assert_eq!(g, w),
        (w, g) => panic!("mismatch: oracle {w:?}, rust {g:?}"),
    }
}

#[test]
fn values_and_dependencies_match_helm() {
    if !semver_ready() {
        return;
    }
    let f = base_chart();
    for user in [
        "",
        r#"{"sub1": {"enabled": false}}"#,
        r#"{"tags": {"t1": false}}"#,
        r#"{"tags": {"t1": false}, "sub1": {"enabled": true}}"#,
        r#"{"sub2": {"on": false}, "global": {"sub2": true}}"#,
        r#"{"sub2": {"on": "nonbool"}, "global": {"sub2": false}}"#,
        r#"{"top": {"a": null, "b": 2}, "global": {"g": null, "new": [1, 2], "gm": {"y": 2}}}"#,
        r#"{"sub1copy": {"v": "copy", "exp": {"e": 5}}, "imported": {"keep": 2, "e": null}}"#,
        r#"{"sub2": {"leaf": {"enabled": true}, "deep": {"x": {"y": null}}}}"#,
        r#"{"sub1": null}"#,
        r#"{"sub1": "scalar"}"#,
        r#"{"global": "scalar"}"#,
        r#"{"sub2": {"global": {"g": "child"}}}"#,
    ] {
        check_values(&f, user);
    }
}

#[test]
fn coalesce_matches_helm() {
    if !semver_ready() {
        return;
    }
    let f = base_chart();
    for user in ["", r#"{"top": {"a": null}, "sub1": {"v": null, "nul": 3}, "global": {"gm": null}}"#] {
        let mut req = files_req("coalesce", &f);
        req["values"] = json!(user);
        let Some(want) = oracle_op(req) else { return };
        let c = load(&f).unwrap();
        let uv = json_yaml(user).unwrap();
        let uv = uv.as_map().cloned().unwrap_or_default();
        let got = chart::coalesce_values(&c, &uv).unwrap();
        assert_eq!(value_to_json(&Value::Map(got)), norm(&want.unwrap()), "user {user}");
    }
}

#[test]
fn manifest_sorting_matches_helm() {
    if !semver_ready() {
        return;
    }
    let mut rendered = BTreeMap::new();
    rendered.insert("c/templates/a.yaml".to_string(),
        "\n---\n{\"kind\": \"Deployment\", \"metadata\": {\"name\": \"d\"}}\n---\n{\"kind\": \"Service\"}\n---   \n\n---\n{\"kind\": \"Foo\"}\n---\t\n{\"kind\": \"Bar\", \"apiVersion\": 1}\n".to_string());
    rendered.insert("c/templates/b.yaml".to_string(),
        "{\"kind\": \"Namespace\"}\n---\n{\"kind\": \"Job\", \"metadata\": {\"name\": \"j\", \"annotations\": {\"helm.sh/hook\": \"post-install, Pre-Upgrade\", \"helm.sh/hook-weight\": \"-5\"}}}\n---\n{\"kind\": \"Pod\", \"metadata\": {\"annotations\": {\"helm.sh/hook\": \"test-success\"}}}\n---\n{\"kind\": \"Pod\", \"metadata\": {\"annotations\": {\"helm.sh/hook\": \"bogus\"}}}\n---\n{\"kind\": \"ConfigMap\", \"metadata\": {\"annotations\": {\"helm.sh/hook\": \"pre-install\"}}}\n".to_string());
    rendered.insert("c/templates/_p.tpl".to_string(), "{\"kind\": \"Secret\"}".to_string());
    rendered.insert("c/templates/empty.yaml".to_string(), "  \n".to_string());
    rendered.insert("c/charts/s/templates/x.yaml".to_string(), "{\"kind\": \"Secret\", \"metadata\": {\"annotations\": {}}}\n---\n{\"kind\": \"Foo\"}".to_string());
    rendered.insert("c/templates/z.yaml".to_string(), "{\"kind\": \"ServiceAccount\", \"metadata\": {\"annotations\": {\"x\": 1}}}".to_string());
    let Some(want) = oracle_op(json!({"op": "sort_manifests", "rendered": rendered})) else { return };
    let c = load(&files(&[("Chart.yaml", r#"{"apiVersion": "v2", "name": "c", "version": "0.1.0"}"#)])).unwrap();
    let got = chart::template_output(&c, rendered.clone(), &RenderOptions::default(), json_yaml).unwrap();
    assert_eq!(got, want.unwrap().as_str().unwrap());

    for bad in ["{\"kind\": [1]}", "{\"metadata\": \"x\"}", "{\"metadata\": {\"annotations\": {\"a\": {}}}}", "[1]"] {
        let mut r = BTreeMap::new();
        r.insert("c/templates/bad.yaml".to_string(), format!("{{\"kind\": \"A\"}}\n---\n{bad}"));
        let want = oracle_op(json!({"op": "sort_manifests", "rendered": r})).unwrap().unwrap_err();
        let got = chart::template_output(&c, r, &RenderOptions::default(), json_yaml).unwrap_err();
        assert_eq!(got.message, want, "case {bad}");
        assert_eq!(got.line(), Some(3));
    }
}

#[test]
fn glob_matches_gobwas() {
    let names: Vec<&str> = vec![
        "a.txt", "b.txt", "dir/a.txt", "dir/sub/c.yaml", "x", "X", "[a]", "a,b", "{a}", ".hidden", "dir/.h", "*",
    ];
    let mut bad = Vec::new();
    for pat in [
        "*", "**", "*.txt", "**.txt", "dir/*", "dir/**", "?", "?.txt", "[ab].txt", "[!a].txt", "[a-c].txt", "[!a-c]*",
        "{a,b}.txt", "{dir/**,x}", "{a,b", "\\[a\\]", "[", "[]", "[z-a]", "a{,.txt}", "*{a,b}*", "**/*.yaml", "dir**",
        "\\*", "{*.txt,X}", "[[]a]", "a\\,b", "{a\\,b}", "}", "{}", "[!]", "**a", "",
    ] {
        let Some(want) = oracle_op(json!({"op": "glob", "pattern": pat, "names": names})) else { return };
        let want: Vec<String> = serde_json::from_value(if want.as_ref().unwrap().is_null() { json!([]) } else { want.unwrap() }).unwrap();
        let g = chart::Glob::compile(pat).or_else(|_| chart::Glob::compile("**")).unwrap();
        let got: Vec<String> = names.iter().filter(|n| g.matches(n)).map(|s| s.to_string()).collect();
        if got != want {
            bad.push(format!("pattern {pat:?}: rust {got:?} helm {want:?}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn kube_version_parsing_matches_helm() {
    for v in ["1.30", "v1.31.2", "1.33.4-gke.1245000", " 1.2", "1", "v", "x1.2", "01.2", "1.02.3", "1.2.3.4", "1.2\n", "99999999999999999999.1"] {
        let Some(want) = oracle_op(json!({"op": "kube_version", "kube_version": v})) else { return };
        match (want, chart::KubeVersion::parse(v)) {
            (Ok(w), Ok(k)) => {
                assert_eq!(w["version"], json!(k.version), "{v:?}");
                assert_eq!(w["string"], json!(k.string()), "{v:?}");
                assert_eq!(w["major"], json!(k.major), "{v:?}");
                assert_eq!(w["minor"], json!(k.minor), "{v:?}");
            }
            (Err(w), Err(e)) => assert_eq!(e, w, "{v:?}"),
            (w, g) => panic!("{v:?}: oracle {w:?}, rust {g:?}"),
        }
    }
}

fn check_template(f: &Files, user: &str, opts: &RenderOptions) {
    let mut req = files_req("chart_template", f);
    req["values"] = json!(user);
    req["include_crds"] = json!(opts.include_crds);
    req["skip_tests"] = json!(opts.skip_tests);
    req["kube_version"] = json!(opts.capabilities.kube_version);
    req["is_upgrade"] = json!(opts.release.is_upgrade);
    req["skip_schema"] = json!(opts.skip_schema_validation);
    let Some(want) = oracle_op(req) else { return };
    let got = load(f).map_err(|e| e.helm_message()).and_then(|c| {
        let user = json_yaml(user).unwrap();
        let engine = Engine { strict: opts.strict, ..Engine::default() };
        chart::template_with(&engine, &c, &user, opts, &mut DeterministicEnv::default(), json_yaml)
            .map_err(|e| e.helm_message())
    });
    match (want, got) {
        (Ok(w), Ok(g)) => assert_eq!(g, w.as_str().unwrap()),
        (Err(w), Err(g)) => assert_eq!(g, w),
        (w, g) => panic!("mismatch: oracle {w:?}, rust {g:?}"),
    }
}

fn template_chart() -> Files {
    let mut f = base_chart();
    f.insert("templates/a.yaml".into(), br#"{"kind": "ConfigMap", "data": {"a": "{{ .Values.top.a }}", "chart": "{{ .Chart.Name }}-{{ .Chart.IsRoot }}", "rel": "{{ .Release.Name }} {{ .Release.Revision }} {{ .Release.IsUpgrade }} {{ .Release.Service }}", "kv": "{{ .Capabilities.KubeVersion }} {{ .Capabilities.KubeVersion.Major }} {{ .Capabilities.APIVersions.Has "apps/v1" }}", "hv": "{{ .Capabilities.HelmVersion.Version }}", "t": "{{ .Template.Name }} {{ .Template.BasePath }}", "files": "{{ .Files.Get "files/a.txt" }} {{ .Files.Lines "files/a.txt" }}", "subs": "{{ range $k, $v := .Subcharts }}{{ $k }}={{ $v.Chart.Name }},{{ end }}"}}"#.to_vec());
    f.insert("templates/hook.yaml".into(), br#"{"kind": "Pod", "metadata": {"name": "t", "annotations": {"helm.sh/hook": "test"}}}"#.to_vec());
    f.insert("charts/sub1/templates/s.yaml".into(), br#"{"kind": "Service", "v": "{{ .Values.v }}", "g": "{{ .Values.global.g }}", "root": "{{ .Chart.IsRoot }}", "name": "{{ .Chart.Name }}"}"#.to_vec());
    f
}

#[test]
fn template_matches_helm() {
    if !semver_ready() {
        return;
    }
    let f = template_chart();
    let opts = RenderOptions::default();
    check_template(&f, "", &opts);
    check_template(&f, r#"{"sub1": {"enabled": false}, "top": {"a": "user"}}"#, &opts);
    let opts = RenderOptions { include_crds: true, skip_tests: true, ..Default::default() };
    check_template(&f, r#"{"sub2": {"leaf": {"enabled": true}}}"#, &opts);
    let mut opts = RenderOptions::default();
    opts.capabilities.kube_version = "1.19.3-gke.1".into();
    check_template(&f, "", &opts);
    opts.capabilities.kube_version = "garbage".into();
    check_template(&f, "", &opts);
}

/// Collection-like objects (`.Files` is a Go map, `.Capabilities.APIVersions`
/// a slice) used with `range`, `len` and `index`.
#[test]
fn template_collection_objects_match_helm() {
    if !semver_ready() {
        return;
    }
    let mut f = template_chart();
    f.insert("templates/a.yaml".into(), br#"{"kind": "ConfigMap", "data": {"glob": "{{ range $k, $v := .Files.Glob "files/*" }}{{ $k }}{{ end }}", "len": "{{ len .Files }} {{ len .Capabilities.APIVersions }}", "idx": "{{ index .Capabilities.APIVersions 1 }} {{ index .Files "files/a.txt" }}", "f": "{{ .Files }}", "pf": "{{ printf "%v" .Capabilities.HelmVersion }}"}}"#.to_vec());
    check_template(&f, "", &RenderOptions::default());
}

#[test]
fn template_errors_match_helm() {
    if !semver_ready() {
        return;
    }
    let ok = r#"{"apiVersion": "v2", "name": "p", "version": "0.1.0"}"#;
    let opts = RenderOptions::default();
    check_template(&files(&[("Chart.yaml", r#"{"apiVersion": "v2", "name": "p", "version": "0.1.0", "type": "library"}"#)]), "", &opts);
    check_template(&files(&[("Chart.yaml", r#"{"apiVersion": "v2", "name": "p", "version": "0.1.0", "dependencies": [{"name": "a"}, {"name": "b"}]}"#)]), "", &opts);
    check_template(&files(&[("Chart.yaml", ok), ("templates/a.yaml", "{{ fail \"boom\" }}")]), "", &opts);
    check_template(&files(&[("Chart.yaml", ok), ("templates/a.yaml", "[1]")]), "", &opts);
    check_template(&files(&[("Chart.yaml", ok), ("templates/NOTES.txt", "only notes")]), "", &opts);
    check_template(&files(&[("Chart.yaml", ok), ("values.yaml", r#"{"sub": 1}"#), ("charts/sub/Chart.yaml", r#"{"apiVersion": "v2", "name": "sub", "version": "0.1.0"}"#)]), "", &opts);
}

#[test]
fn chart_schema_errors_match_helm() {
    if !semver_ready() {
        return;
    }
    let mut f = files(&[
        ("Chart.yaml", r#"{"apiVersion": "v2", "name": "p", "version": "0.1.0"}"#),
        ("values.schema.json", r#"{"properties": {"port": {"type": "integer"}}, "required": ["name"]}"#),
        ("values.yaml", r#"{"port": "80", "sub": {"x": 1}, "sub2": 5}"#),
        ("templates/a.yaml", r#"{"kind": "A"}"#),
        ("charts/sub/Chart.yaml", r#"{"apiVersion": "v2", "name": "sub", "version": "0.1.0"}"#),
        ("charts/sub/values.schema.json", r#"{"properties": {"x": {"type": "string"}}}"#),
        ("charts/sub2/Chart.yaml", r#"{"apiVersion": "v2", "name": "sub2", "version": "0.1.0"}"#),
        ("charts/sub2/values.schema.json", r#"{}"#),
    ]);
    // sub2's values are not a map: coalescing fails first.
    check_template(&f, "", &RenderOptions::default());
    f.insert("values.yaml".into(), br#"{"port": "80", "sub": {"x": 1}}"#.to_vec());
    check_template(&f, "", &RenderOptions::default());
    check_template(&f, r#"{"port": 1, "name": "n", "sub": {"x": "s"}}"#, &RenderOptions::default());
    check_template(&f, "", &RenderOptions { skip_schema_validation: true, ..Default::default() });
}
