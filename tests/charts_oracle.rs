//! Validates the test infrastructure itself against the real, pinned Helm:
//! the chart corpus (lock file, cache checksums) and the Go oracle (its
//! `template` op must reproduce the `helm` binary byte for byte, and its
//! per-template `chart` op must agree with what the binary prints).
//!
//! These tests do not exercise helm-rs; they make sure the references the
//! differential tests compare against are trustworthy.
//!
//! Knobs: `HELM_RS_CHARTS=a,b` (subset), `HELM_ORACLE_REQUIRED=1` /
//! `HELM_RS_CHARTS_REQUIRED=1` (fail instead of skip when tools are missing).

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::corpus::{self, ChartEntry};
use common::diff;
use common::helm_bin::{helm_bin, helm_template};
use common::{OracleResult, Opts, oracle};

#[test]
fn lock_is_consistent() {
    let lock = corpus::lock_json();
    assert_eq!(lock["helm_version"], "v4.3.0", "lock written with a different helm");
    let charts = corpus::lock();
    assert!(charts.len() >= 25, "corpus too small: {}", charts.len());
    let mut names = BTreeSet::new();
    for c in &charts {
        assert!(names.insert(c.name.clone()), "duplicate chart {}", c.name);
        assert!(!c.version.is_empty(), "{}: no version", c.name);
        assert_eq!(c.file, format!("{}-{}.tgz", c.name, c.version), "{}: file name", c.name);
        assert!(c.sha256.len() == 64 && c.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{}: bad sha256", c.name);
        if let Some(p) = c.values_path() {
            assert!(p.exists(), "{}: values file {} missing", c.name, p.display());
        }
        assert!(c.helm_template_ok, "{}: helm template failed when the lock was written: {}", c.name, c.error);
    }
    // Every values file belongs to a chart.
    for e in std::fs::read_dir(corpus::charts_dir().join("values")).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().trim_end_matches(".yaml").to_string();
        assert!(names.contains(&n), "values/{n}.yaml does not match any chart");
    }
    // sources.txt and the lock list the same charts.
    let sources = std::fs::read_to_string(corpus::charts_dir().join("sources.txt")).unwrap();
    let src_names: BTreeSet<String> = sources
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .collect();
    assert_eq!(src_names, names, "sources.txt and charts.lock disagree; run scripts/update-charts.sh");
}

#[test]
fn cache_checksums_match_lock() {
    for c in corpus::available() {
        let Some(sum) = corpus::sha256_file(&c.tgz_path()) else {
            eprintln!("skipping: no sha256sum/shasum");
            return;
        };
        assert_eq!(sum, c.sha256, "{}: cached {} does not match the lock", c.name, c.file);
    }
}

/// Renders `c` with the default corpus options.
fn opts() -> Opts {
    Opts::default()
}

/// The oracle's `template` op is byte-identical to `helm template`
/// (modulo lines that differ between two renders of the same input, i.e.
/// random passwords/certs), and fails with the same message.
#[test]
fn oracle_template_matches_helm_binary() {
    let Some(helm) = helm_bin() else { return };
    let charts = corpus::available();
    let Some(mut o) = oracle() else { return };
    let mut failures = Vec::new();
    for c in &charts {
        let values = c.values_yaml();
        let bin = helm_template(&helm, &c.tgz_path(), &values, &opts());
        let a = o.template_chart(&c.tgz_path(), &values, &opts());
        let b = o.template_chart(&c.tgz_path(), &values, &opts());
        if let Some(f) = compare_template(c, &bin, &a, &b) {
            failures.push(f);
        }
        if bin.is_ok() != c.helm_template_ok {
            failures.push(format!("{}: helm template ok={} but the lock says ok={}", c.name, bin.is_ok(), c.helm_template_ok));
        }
    }
    report(&charts, failures);
}

/// Error paths: charts that need a values file fail without it, identically
/// in the binary and the oracle (schema validation, `required`, `fail`).
#[test]
fn oracle_template_errors_match_helm_binary() {
    let Some(helm) = helm_bin() else { return };
    let charts: Vec<_> = corpus::available().into_iter().filter(|c| c.values.is_some()).collect();
    let Some(mut o) = oracle() else { return };
    let mut failures = Vec::new();
    for c in &charts {
        let bin = helm_template(&helm, &c.tgz_path(), "", &opts());
        let a = o.template_chart(&c.tgz_path(), "", &opts());
        assert!(bin.is_err(), "{}: expected helm template to fail without values", c.name);
        if let Some(f) = compare_template(c, &bin, &a, &a) {
            failures.push(f);
        }
    }
    // Malformed user values and a too-old kube version fail identically too.
    if let Some(c) = charts.first() {
        let bad = Opts { kube_version: Some("v1.2.3".into()), ..opts() };
        for (values, o2) in [("a: [1\n", opts()), ("- 1\n", opts()), ("", bad)] {
            let bin = helm_template(&helm, &c.tgz_path(), values, &o2);
            let a = o.template_chart(&c.tgz_path(), values, &o2);
            if let (Err(be), Err(ae)) = (&bin, &a) {
                // The CLI names the values file; the oracle says values.yaml.
                let norm = |s: &str| s.split(": ").skip(1).collect::<Vec<_>>().join(": ");
                if be.starts_with("failed to parse ") && ae.starts_with("failed to parse ") && norm(be) == norm(ae) {
                    continue;
                }
            }
            if let Some(f) = compare_template(c, &bin, &a, &a) {
                failures.push(format!("values {values:?}: {f}"));
            }
        }
    }
    report(&charts, failures);
}

fn compare_template(c: &ChartEntry, bin: &Result<String, String>, a: &Result<String, String>, b: &Result<String, String>) -> Option<String> {
    match (bin, a) {
        (Ok(bin), Ok(a)) => diff::compare_text(a, b.as_ref().ok().map(|s| s.as_str()), bin, "oracle", "helm")
            .map(|d| format!("{}: oracle template output differs from helm binary:\n{d}", c.name)),
        // The CLI prints the message followed by a newline, so a trailing
        // newline in the error itself is not observable.
        (Err(be), Err(ae)) if be.trim_end() == ae.trim_end() => None,
        (Err(be), Err(ae)) => Some(format!("{}: both fail, differently:\n  helm:   {be}\n  oracle: {ae}", c.name)),
        (Ok(_), Err(ae)) => Some(format!("{}: helm succeeds but oracle fails: {ae}", c.name)),
        (Err(be), Ok(_)) => Some(format!("{}: oracle succeeds but helm fails: {be}", c.name)),
    }
}

/// Splits a rendered file into YAML documents like Helm's
/// `releaseutil.SplitManifests` (separator `(?:^|\s*\n)---\s*`, documents
/// trimmed, empty ones dropped).
fn split_manifests(s: &str) -> Vec<String> {
    let mut docs = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if &bytes[i..i + 3] == b"---" && (i == 0 || bytes[i - 1] == b'\n') {
            docs.push(&s[start..i]);
            start = i + 3;
            i += 3;
        } else {
            i += 1;
        }
    }
    docs.push(&s[start..]);
    docs.into_iter().map(str::trim).filter(|d| !d.is_empty()).map(str::to_string).collect()
}

/// The oracle's per-template engine output (what `chart::render` is compared
/// against) is consistent with the binary's `helm template` stdout: every
/// document of every rendered file is printed under its `# Source:` header,
/// and nothing else is printed.
#[test]
fn oracle_render_consistent_with_helm_binary() {
    let Some(helm) = helm_bin() else { return };
    let charts = corpus::available();
    let Some(mut o) = oracle() else { return };
    let mut failures = Vec::new();
    for c in &charts {
        let values = c.values_yaml();
        let bin = helm_template(&helm, &c.tgz_path(), &values, &opts());
        let a = o.render_chart(&c.tgz_path(), &values, &opts());
        let b = o.render_chart(&c.tgz_path(), &values, &opts());
        match (&bin, &a, &b) {
            (Ok(out), OracleResult::Ok(fa), OracleResult::Ok(fb)) => {
                if let Some(f) = check_consistent(out, fa, fb) {
                    failures.push(format!("{}: {f}", c.name));
                }
            }
            (Err(_), OracleResult::Err { .. }, _) => {}
            (bin, a, _) => failures.push(format!("{}: helm template: {:?}\n  oracle render: {:?}", c.name, bin.as_ref().map(|_| "ok"), a_status(a))),
        }
    }
    report(&charts, failures);
}

fn a_status(r: &OracleResult) -> String {
    match r {
        OracleResult::Ok(_) => "ok".into(),
        OracleResult::Err { stage, message } => format!("{stage}: {message}"),
    }
}

fn check_consistent(out: &str, fa: &BTreeMap<String, String>, fb: &BTreeMap<String, String>) -> Option<String> {
    let mut ndocs = 0;
    for (name, content) in fa {
        if name.ends_with("NOTES.txt") {
            continue;
        }
        let docs = split_manifests(content);
        let docs_b = split_manifests(fb.get(name).map(String::as_str).unwrap_or(""));
        ndocs += docs.len();
        for (k, d) in docs.iter().enumerate() {
            if docs_b.get(k) != Some(d) {
                continue; // nondeterministic document
            }
            let needle = format!("# Source: {name}\n{d}\n");
            if !out.contains(&needle) {
                return Some(format!("document {k} of {name} not found in helm template output:\n{d}"));
            }
        }
    }
    let printed = out.matches("# Source: ").count();
    if printed != ndocs {
        return Some(format!("helm template printed {printed} documents, the oracle rendered {ndocs}"));
    }
    None
}

fn report(charts: &[ChartEntry], failures: Vec<String>) {
    eprintln!("checked {} charts", charts.len());
    if !failures.is_empty() {
        for f in &failures {
            eprintln!("\n{f}");
        }
        panic!("{} failures", failures.len());
    }
}

#[test]
fn split_manifests_like_helm() {
    assert_eq!(split_manifests("a: 1\n---\nb: 2\n"), vec!["a: 1", "b: 2"]);
    assert_eq!(split_manifests("---\n# c\na: 1\n--- \n\n"), vec!["# c\na: 1"]);
    assert!(split_manifests("\n\n").is_empty());
}
