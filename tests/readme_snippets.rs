//! Keeps the README examples compiling and correct.
#![allow(clippy::result_large_err)]

#[test]
fn render_single_template() -> Result<(), helm_rs::Error> {
    use helm_rs::template::Engine;
    use helm_rs::{DeterministicEnv, map};

    let data = map! { "Values" => map! { "name" => "world" } };
    let out = Engine::new().render_str(
        "greeting",
        "hello {{ .Values.name | upper }}",
        &data,
        &mut DeterministicEnv::default(),
    )?;
    assert_eq!(out, "hello WORLD");
    Ok(())
}

#[test]
fn custom_function() -> Result<(), helm_rs::Error> {
    use helm_rs::template::Engine;
    use helm_rs::{DeterministicEnv, Sig, Ty, Value};

    let mut engine = Engine::new();
    engine.funcs.add("shout", Sig::new(&[Ty::String]), |args| {
        Ok(Value::string(
            args[0].as_str().unwrap_or("").to_uppercase() + "!",
        ))
    });
    let out = engine.render_str(
        "t",
        "{{ shout \"hi\" }}",
        &Value::Nil,
        &mut DeterministicEnv::default(),
    )?;
    assert_eq!(out, "HI!");
    Ok(())
}

#[test]
fn render_chart_from_files() -> Result<(), Box<dyn std::error::Error>> {
    use helm_rs::chart::{self, Chart, RenderOptions};
    use helm_rs::{DeterministicEnv, yaml};

    let chart = Chart::from_files([
        ("Chart.yaml", "apiVersion: v2\nname: demo\nversion: 0.1.0\n"),
        ("values.yaml", "replicas: 1\n"),
        (
            "templates/cm.yaml",
            "kind: ConfigMap\ndata:\n  replicas: {{ .Values.replicas | quote }}\n",
        ),
    ])?;
    let values = yaml::unmarshal("replicas: 3\n", yaml::Target::Map("common.Values"))
        .map_err(|e| e.message)?;
    let manifest = chart::template(
        &chart,
        &values,
        &RenderOptions::default(),
        &mut DeterministicEnv::default(),
    )?;
    assert_eq!(
        manifest,
        "---\n# Source: demo/templates/cm.yaml\nkind: ConfigMap\ndata:\n  replicas: \"3\"\n"
    );
    Ok(())
}
