//! Malformed inputs must produce errors, never panics.

mod common;

use common::chart::*;
use helm_rs::Value;
use helm_rs::chart::{self, Chart, Glob};
use helm_rs::value::Map;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn sample_tar() -> Vec<u8> {
    tar(&[
        ("c/", None),
        (
            "c/Chart.yaml",
            Some(br#"{"apiVersion": "v2", "name": "c", "version": "0.1.0"}"#),
        ),
        ("c/values.yaml", Some(br#"{"a": {"b": [1, 2]}}"#)),
        ("c/templates/x.yaml", Some(b"x: {{ .Values.a }}")),
        (
            "c/charts/s/Chart.yaml",
            Some(br#"{"apiVersion": "v2", "name": "s", "version": "0.1.0"}"#),
        ),
    ])
}

#[test]
fn mutated_archives_do_not_panic() {
    if !semver_ready() {
        return;
    }
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let tar = sample_tar();
    let tgz = gzip(&tar);
    assert!(Chart::from_tgz_with_parser(&tgz, json_yaml).is_ok());
    for _ in 0..3000 {
        // Mutate either the tar (then re-gzip) or the gzip stream itself.
        let data = if rng.below(2) == 0 {
            let mut t = tar.clone();
            for _ in 0..1 + rng.below(8) {
                let i = rng.below(t.len());
                t[i] = rng.next() as u8;
            }
            if rng.below(4) == 0 {
                t.truncate(rng.below(t.len()));
            }
            gzip(&t)
        } else {
            let mut g = tgz.clone();
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(g.len());
                g[i] = rng.next() as u8;
            }
            if rng.below(4) == 0 {
                g.truncate(rng.below(g.len()));
            }
            g
        };
        let _ = Chart::from_tgz_with_parser(&data, json_yaml);
    }
}

#[test]
fn hostile_globs_and_metadata_do_not_panic() {
    if !semver_ready() {
        return;
    }
    for p in [
        "{".repeat(10000),
        "?".repeat(100_000),
        "[".repeat(1000),
        "*".repeat(5000) + "x",
        "\\".into(),
    ] {
        let g = Glob::compile(&p);
        if let Ok(g) = g {
            let _ = g.matches(&"a".repeat(1000));
        }
    }
    let mut rng = Rng(42);
    let alphabet = br#"{}[]",:0123456789.eE-+ abcnulltrue\/"#;
    for _ in 0..3000 {
        let len = rng.below(60);
        let s: Vec<u8> = (0..len)
            .map(|_| alphabet[rng.below(alphabet.len())])
            .collect();
        let s = String::from_utf8(s).unwrap();
        let _ = Chart::from_files_with_parser(
            [("Chart.yaml", s.clone()), ("values.yaml", s.clone())],
            json_yaml,
        );
        let _ =
            chart::validate_against_schema(&helm_rs::value::MapRef::new(Map::new()), s.as_bytes());
        let _ = chart::KubeVersion::parse(&s);
        let _ = chart::split_manifests(&s);
        let _ = Value::Nil;
    }
    let deep = "[".repeat(100_000);
    assert!(
        chart::validate_against_schema(&helm_rs::value::MapRef::new(Map::new()), deep.as_bytes())
            .is_err()
    );
}
