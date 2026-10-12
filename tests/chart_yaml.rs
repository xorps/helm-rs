//! End-to-end chart tests with real YAML (Chart.yaml, values, manifests),
//! diffed against `helm template`.
#![allow(clippy::result_large_err)]

mod common;

use std::collections::BTreeMap;

use common::chart::*;
use helm_rs::chart::{self, Chart, RenderOptions};
use helm_rs::{DeterministicEnv, Value};
use serde_json::json;

fn files(list: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
    list.iter().map(|(k, v)| (k.to_string(), v.as_bytes().to_vec())).collect()
}

fn user_values(yaml: &str) -> Value {
    helm_rs::yaml::from_str(yaml).unwrap()
}

fn check_template(f: &BTreeMap<String, Vec<u8>>, values: &str, opts: &RenderOptions) {
    let mut req = files_req("chart_template", f);
    req["values"] = json!(values);
    req["include_crds"] = json!(opts.include_crds);
    req["skip_tests"] = json!(opts.skip_tests);
    req["kube_version"] = json!(opts.capabilities.kube_version);
    req["release"] = json!(opts.release.name);
    req["namespace"] = json!(opts.release.namespace);
    req["is_upgrade"] = json!(opts.release.is_upgrade);
    let Some(want) = oracle_op(req) else { return };
    let got = Chart::from_files(f.clone())
        .and_then(|c| chart::template(&c, &user_values(values), opts, &mut DeterministicEnv::default()))
        .map_err(|e| e.helm_message());
    match (want, got) {
        (Ok(w), Ok(g)) => assert_eq!(g, w.as_str().unwrap()),
        (Err(w), Err(g)) => assert_eq!(g, w),
        (w, g) => panic!("mismatch: oracle {w:?}, rust {g:?}"),
    }
}

fn app_chart() -> BTreeMap<String, Vec<u8>> {
    files(&[
        (
            "Chart.yaml",
            "apiVersion: v2\nname: app\nversion: 1.10\nappVersion: 1.20\ndescription: 1e3\nkeywords: [1, true, x]\nannotations:\n  num: 5\n  flag: yes\ndependencies:\n  - name: db\n    version: \"~0.1\"\n    condition: db.enabled\n    import-values:\n      - child: info\n        parent: dbinfo\n",
        ),
        (
            "values.yaml",
            "# defaults\nreplicas: 2\nimage:\n  repo: nginx\n  tag: \"1.25\"\nglobal:\n  env: prod\nempty: ~\n---\nextra: from-second-doc\n",
        ),
        (
            "templates/deploy.yaml",
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: {{ .Release.Name }}-{{ .Chart.Name }}\n  labels:\n    chart: \"{{ .Chart.Name }}-{{ .Chart.Version }}\"\n    app-version: {{ .Chart.AppVersion | quote }}\nspec:\n  replicas: {{ .Values.replicas }}\n  template:\n    spec:\n      containers:\n        - image: \"{{ .Values.image.repo }}:{{ .Values.image.tag }}\"\n          env:\n            - name: EXTRA\n              value: {{ .Values.extra }}\n            - name: DB\n              value: {{ .Values.dbinfo.host | default \"none\" }}\n",
        ),
        ("templates/svc.yaml", "apiVersion: v1\nkind: Service\nmetadata:\n  name: svc\n---\napiVersion: v1\nkind: ServiceAccount\nmetadata:\n  name: sa\n"),
        ("templates/cm.yaml", "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\ndata:\n{{ (.Files.Glob \"conf/*\").AsConfig | indent 2 }}\n"),
        ("templates/hook.yaml", "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: hook\n  annotations:\n    \"helm.sh/hook\": pre-install,post-upgrade\n    \"helm.sh/hook-weight\": \"5\"\n"),
        ("templates/tests/t.yaml", "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test\n  annotations:\n    helm.sh/hook: test\n"),
        ("templates/NOTES.txt", "Installed {{ .Release.Name }}\n"),
        ("templates/_helpers.tpl", "{{ define \"x\" }}x{{ end }}"),
        ("conf/a.conf", "a=1\n"),
        ("conf/b.conf", "b=2\n"),
        ("crds/crd.yaml", "apiVersion: apiextensions.k8s.io/v1\nkind: CustomResourceDefinition\nmetadata:\n  name: x\n"),
        ("charts/db/Chart.yaml", "apiVersion: v2\nname: db\nversion: 0.1.5\n"),
        ("charts/db/values.yaml", "enabled: true\ninfo:\n  host: db.local\n"),
        ("charts/db/templates/db.yaml", "apiVersion: v1\nkind: Secret\nmetadata:\n  name: db\n  labels:\n    env: {{ .Values.global.env }}\n"),
    ])
}

#[test]
fn app_chart_template_matches_helm() {
    if !semver_ready() {
        return;
    }
    let f = app_chart();
    check_template(&f, "", &RenderOptions::default());
    check_template(&f, "replicas: 5\ndb:\n  enabled: false\n", &RenderOptions::default());
    let mut opts = RenderOptions { include_crds: true, skip_tests: true, ..Default::default() };
    opts.release.name = "rel".into();
    opts.release.namespace = "ns".into();
    check_template(&f, "image:\n  tag: null\n", &opts);
}

#[test]
fn chart_yaml_scalars_are_stringified_like_helm() {
    if !semver_ready() {
        return;
    }
    let f = app_chart();
    let Some(want) = oracle_op(files_req("load", &f)) else { return };
    let c = Chart::from_files(f).unwrap();
    let md = value_to_json(&c.metadata_as_map());
    let want = &want.unwrap()["metadata"];
    for k in ["Version", "AppVersion", "Description", "Keywords", "Annotations"] {
        assert_eq!(md[k], want[k], "{k}");
    }
}

#[test]
fn yaml_errors_have_helm_messages_and_locations() {
    if !semver_ready() {
        return;
    }
    // Chart.yaml syntax error.
    let f = files(&[("Chart.yaml", "apiVersion: v2\nname: [x\n")]);
    let want = oracle_op(files_req("load", &f)).map(|r| r.unwrap_err());
    let e = Chart::from_files(f).unwrap_err();
    if let Some(w) = want {
        assert_eq!(e.helm_message(), w);
    }
    assert_eq!(e.location.as_ref().unwrap().name, "Chart.yaml");

    // Type error: located at the offending key.
    let f = files(&[("Chart.yaml", "apiVersion: v2\nname: x\nversion: 0.1.0\ndeprecated: maybe\n")]);
    let want = oracle_op(files_req("load", &f)).map(|r| r.unwrap_err());
    let e = Chart::from_files(f).unwrap_err();
    if let Some(w) = want {
        assert_eq!(e.helm_message(), w);
    }
    assert_eq!(e.line(), Some(4));

    // Validation error: located at the key.
    let f = files(&[("Chart.yaml", "apiVersion: v2\nname: x\nversion: one\n")]);
    let e = Chart::from_files(f).unwrap_err();
    assert_eq!(e.helm_message(), "validation: chart.metadata.version \"one\" is invalid");
    assert_eq!(e.line(), Some(3));

    // values.yaml: error in the second document, reported at the file line.
    let f = files(&[
        ("Chart.yaml", "apiVersion: v2\nname: x\nversion: 0.1.0\n"),
        ("values.yaml", "a: 1\n---\nb: 2\nc: [\n"),
    ]);
    let want = oracle_op(files_req("load", &f)).map(|r| r.unwrap_err());
    let e = Chart::from_files(f).unwrap_err();
    if let Some(w) = want {
        assert_eq!(e.helm_message(), w);
    }
    let loc = e.location.unwrap();
    assert_eq!(loc.name, "values.yaml");
    assert!(loc.line >= 4, "{loc:?}");

    // Subchart errors are wrapped and located in the subchart's file.
    let f = files(&[
        ("Chart.yaml", "apiVersion: v2\nname: x\nversion: 0.1.0\n"),
        ("charts/sub/Chart.yaml", "apiVersion: v2\nname: sub\nversion: [\n"),
    ]);
    let e = Chart::from_files(f).unwrap_err();
    assert!(e.helm_message().starts_with("error unpacking subchart sub in x: cannot load Chart.yaml: "));
    assert_eq!(e.location.unwrap().name, "charts/sub/Chart.yaml");
}

#[test]
fn manifest_yaml_errors_match_helm() {
    if !semver_ready() {
        return;
    }
    let f = files(&[
        ("Chart.yaml", "apiVersion: v2\nname: x\nversion: 0.1.0\n"),
        ("templates/a.yaml", "kind: A\n---\nkind: B\nmetadata: [\n"),
    ]);
    check_template(&f, "", &RenderOptions::default());
    let c = Chart::from_files(f).unwrap();
    let e = chart::template(&c, &Value::Nil, &RenderOptions::default(), &mut DeterministicEnv::default()).unwrap_err();
    assert_eq!(e.location.as_ref().unwrap().name, "x/templates/a.yaml");
}

/// values.yaml documents are split like k8s' YAMLReader: a leading `---` is
/// part of the first document, bad separators are reported lazily.
#[test]
fn values_documents_match_helm() {
    if !semver_ready() {
        return;
    }
    // (values.yaml, expected 1-based error line in values.yaml, if an error)
    let cases: &[(&str, Option<u32>)] = &[
        ("---\na: 1\nb: [\n", Some(3)),
        ("a: 1\n---\nb: [\n", Some(3)),
        ("%YAML 1.1\n---\na: 1\n--- [\n", None),
        ("a: 1\n--- x\n", Some(2)),
        ("a: {\n--- x\n", None),
        ("---\n---\na: 1\n", None),
        ("a: 1\n---\n---\nb: 2\n", None),
        ("a:\n  b: 1\n---\na:\n  c: 2\n--- # comment\nd: 3", None),
        ("---", None),
        ("----\na: 1\n", Some(1)),
        ("a: 1\r\n---\r\nb: 2\r\n", None),
        ("a: [1]\n---\n- x\n", Some(3)),
    ];
    for (values, line) in cases {
        let f = files(&[
            ("Chart.yaml", "apiVersion: v2\nname: x\nversion: 0.1.0\n"),
            ("values.yaml", values),
        ]);
        let Some(want) = oracle_op(files_req("load", &f)) else {
            return;
        };
        match (want, Chart::from_files(f)) {
            (Ok(w), Ok(c)) => {
                assert_eq!(
                    value_to_json(&Value::Map(c.values.clone())),
                    norm(&w["values"]),
                    "{values:?}"
                )
            }
            (Err(w), Err(e)) => {
                assert_eq!(e.helm_message(), w, "{values:?}");
                let loc = e.location.as_ref().expect("location");
                assert_eq!(loc.name, "values.yaml");
                if let Some(l) = line {
                    assert_eq!(loc.line, *l, "{values:?} {}", e.helm_message());
                }
            }
            (w, g) => panic!("{values:?}: oracle {w:?}, rust {g:?}"),
        }
    }
}

/// Go types of what templates see (`typeOf`, `%v`, `toJson`).
#[test]
fn template_object_types_match_helm() {
    if !semver_ready() {
        return;
    }
    let tpl = r#"ann={{ typeOf .Chart.Annotations }} {{ .Chart.Annotations }} {{ toJson .Chart.Annotations }}
kw={{ typeOf .Chart.Keywords }} mt={{ typeOf .Chart.Maintainers }} deps={{ typeOf .Chart.Dependencies }}
iv={{ range .Chart.Dependencies }}{{ typeOf .ImportValues }}{{ range .ImportValues }} {{ typeOf . }} {{ . }}{{ end }} {{ typeOf .Tags }} {{ typeOf .Enabled }};{{ end }}
lines={{ typeOf (.Files.Lines "a.txt") }} {{ .Files.Lines "a.txt" }}
glob={{ typeOf (.Files.Glob "*.txt") }} {{ len (.Files.Glob "*.txt") }}
cfg={{ typeOf (.Files.Glob "*.txt").AsConfig }} get={{ typeOf (.Files.Get "a.txt") }} bytes={{ typeOf (.Files.GetBytes "a.txt") }}
caps={{ typeOf .Capabilities.APIVersions }} {{ typeOf .Capabilities.KubeVersion }} {{ typeOf .Capabilities.HelmVersion }}
rel={{ typeOf .Release }} {{ typeOf .Release.Revision }} {{ typeOf .Subcharts }} {{ typeOf .Values }} {{ typeOf .Chart }} {{ typeOf .Chart.IsRoot }} {{ typeOf .Files }}
chart={{ toJson .Chart }}
sub={{ range $k, $v := .Subcharts }}{{ $k }} {{ typeOf $v }} {{ typeOf $v.Values }} {{ toJson $v.Chart.Annotations }}{{ end }}
"#;
    let mut f = files(&[
        (
            "Chart.yaml",
            "apiVersion: v2\nname: p\nversion: 0.1.0\nkeywords: [a]\nmaintainers: [{name: m}]\ndependencies:\n  - name: s\n    tags: [t]\n    import-values: [data, {child: c, parent: p}]\n",
        ),
        ("a.txt", "x\ny\n"),
        ("charts/s/Chart.yaml", "apiVersion: v2\nname: s\nversion: 0.1.0\nannotations: {k: v}\n"),
        ("charts/s/values.yaml", "exports: {data: {d: 1}}\nc: {e: 2}\n"),
    ]);
    f.insert("templates/t.yaml".into(), tpl.as_bytes().to_vec());
    let Some(mut o) = common::oracle() else { return };
    let text: BTreeMap<String, String> =
        f.iter().map(|(k, v)| (k.clone(), String::from_utf8(v.clone()).unwrap())).collect();
    let want = o.render_files(&text, "", &common::Opts::default());
    drop(o);
    let common::OracleResult::Ok(want) = want else { panic!("oracle failed: {want:?}") };
    let c = Chart::from_files(f).unwrap();
    let got = chart::render(&c, &Value::Nil, &RenderOptions::default(), &mut DeterministicEnv::default()).unwrap();
    assert_eq!(got["p/templates/t.yaml"], want["p/templates/t.yaml"]);
}
