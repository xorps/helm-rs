//! Large inputs must be handled in reasonable time.
use std::time::Instant;

use helm_rs::funcs::encoding as enc;
use helm_rs::{json, yaml};

#[test]
fn large_documents_are_fast() {
    let mut doc = String::new();
    for i in 0..5000 {
        doc.push_str(&format!(
            "key{i}:\n  name: \"value {i}\"\n  list: [1, 2, {i}]\n  nested: {{a: b, c: [x, y]}}\n"
        ));
    }
    if let Some(n) = std::env::var("PERF_PARSE_LOOPS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    {
        for _ in 0..n {
            let _ = yaml::from_str(&doc).unwrap();
        }
    }
    let t = Instant::now();
    let v = yaml::from_str(&doc).unwrap();
    let parse = t.elapsed();
    let t = Instant::now();
    let y = yaml::to_string(&v).unwrap();
    let emit = t.elapsed();
    let t = Instant::now();
    let p = yaml::to_string_pretty(&v).unwrap();
    let pretty = t.elapsed();
    let t = Instant::now();
    let j = json::to_string(&v).unwrap();
    let back = json::from_str(&j).unwrap();
    let js = t.elapsed();
    let t = Instant::now();
    let tm = enc::to_toml(&v);
    let tb = enc::from_toml(&tm);
    let toml = t.elapsed();
    assert_eq!(back, v);
    assert!(tb.as_map().is_some());
    eprintln!(
        "{} bytes: parse {parse:?}, toYaml {emit:?} ({} bytes), toYamlPretty {pretty:?} ({} bytes), json {js:?}, toml {toml:?}",
        doc.len(),
        y.len(),
        p.len()
    );
    assert!(
        parse.as_secs() < 10
            && emit.as_secs() < 10
            && pretty.as_secs() < 10
            && js.as_secs() < 10
            && toml.as_secs() < 10
    );
}
