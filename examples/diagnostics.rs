use helm_rs::template::{Engine, Renderable};
use helm_rs::{DeterministicEnv, FuncMap, Value};

fn render(files: &[(&str, &str)], values: Value) -> helm_rs::Error {
    let top = helm_rs::map! { "Values" => values };
    let mut e = Engine::new();
    e.funcs = FuncMap::new();
    let rs = files.iter().map(|(n, s)| Renderable { name: format!("mychart/templates/{n}"), source: (*s).into(), vals: top.clone(), base_path: "mychart/templates".into() }).collect();
    e.render(&mut DeterministicEnv::default(), rs).unwrap_err()
}

//! Shows how errors are reported. Run: `cargo run --example diagnostics`

fn main() {
    let helpers = "{{- define \"mychart.image\" -}}\n{{ .Values.image.repository }}:{{ .Values.image.tag.major }}\n{{- end }}";
    let deploy = "apiVersion: apps/v1\nkind: Deployment\nspec:\n  template:\n    spec:\n      containers:\n        - name: app\n          image: {{ include \"mychart.image\" . }}\n";
    let e = render(&[("_helpers.tpl", helpers), ("deployment.yaml", deploy)], helm_rs::map! { "image" => helm_rs::map! { "repository" => "nginx" } });
    println!("--- ours ---\n{e}\n--- helm ---\n{}\n", e.helm_message());
    let e = render(&[("svc.yaml", "kind: Service\nspec:\n  ports:\n  {{- range .Values.ports }}\n    - port: {{ .port }\n  {{- end }}\n")], Value::Nil);
    println!("--- ours ---\n{e}\n--- helm ---\n{}\n", e.helm_message());
    let e = render(&[("cm.yaml", "data:\n  x: {{ tpl \"{{ .Values.a.b }}\" . }}\n")], helm_rs::map!{});
    println!("--- ours ---\n{e}\n--- helm ---\n{}\n", e.helm_message());
    let e = render(&[("cm.yaml", "data:\n  x: {{ required \"database.password is required\" .Values.database.password }}\n")], helm_rs::map!{"database" => helm_rs::map!{}});
    println!("--- ours ---\n{e}\n--- helm ---\n{}\n", e.helm_message());
}
