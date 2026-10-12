mod common;

#[test]
fn oracle_renders_float_values_like_helm() {
    let Some(out) =
        common::helm_render_one("{{ .Values.a }} {{ typeOf .Values.a }}", "a: 1000000\n")
    else {
        return;
    };
    assert_eq!(out.unwrap(), "1e+06 float64");
}
