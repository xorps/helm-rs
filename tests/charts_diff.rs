//! Differential tests: helm-rs vs real Helm v4.3.0 on the pinned chart corpus
//! (`testdata/charts/charts.lock`).
//!
//! * `render_matches_oracle`: `Chart::from_tgz` + `chart::render` vs the
//!   oracle's `engine.Render` output, per template file.
//! * `template_matches_helm_binary`: `chart::template` vs the stdout of the
//!   real `helm template release-name <tgz> -f <values>`.
//!
//! Errors must match too: both fail or both succeed; when both fail, the
//! message (`Error::helm_message`) and the innermost `file:line:col` Helm
//! reports must equal ours (`HELM_RS_LAX_ERRORS=1` only requires both to fail).
//!
//! Lines that differ between two Helm renders of the same input (random
//! passwords, generated certificates) are not compared.
//!
//! Setup: `scripts/build-oracle.sh`, `scripts/fetch-helm.sh`,
//! `scripts/fetch-charts.sh`. Missing pieces make the tests skip with a
//! message. `HELM_RS_CHARTS=name1,name2` selects charts.

mod common;

use std::fmt::Write;

use common::compare::{Mismatch, compare_render, compare_template};
use common::corpus::{self, ChartEntry};
use common::helm_bin::{helm_bin, helm_template};
use common::{Opts, oracle, rust};

/// `now` is fixed to the epoch in the oracle, as in `DeterministicEnv`.
fn opts() -> Opts {
    Opts {
        fixed_time: true,
        ..Opts::default()
    }
}

#[test]
fn render_matches_oracle() {
    let charts = corpus::available();
    if charts.is_empty() {
        return;
    }
    let Some(mut o) = oracle() else { return };
    let mut failures = Vec::new();
    for c in &charts {
        let values = c.values_yaml();
        let helm = o.render_chart(&c.tgz_path(), &values, &opts());
        let ours = rust::render_tgz(&c.tgz_bytes(), &values, &opts());
        let res = compare_render(
            &helm,
            || Some(o.render_chart(&c.tgz_path(), &values, &opts())),
            &ours,
        );
        record(&mut failures, c, &values, res);
    }
    finish(
        "chart::render vs oracle engine.Render",
        charts.len(),
        failures,
    );
}

#[test]
fn template_matches_helm_binary() {
    let charts = corpus::available();
    if charts.is_empty() {
        return;
    }
    let Some(helm) = helm_bin() else { return };
    let Some(mut o) = oracle() else { return };
    let mut failures = Vec::new();
    // The binary cannot fix the clock; lines using `now` differ between it and
    // the second (oracle) render and are ignored like random ones.
    let plain = Opts::default();
    for c in &charts {
        let values = c.values_yaml();
        let bin = helm_template(&helm, &c.tgz_path(), &values, &plain);
        let ours = rust::template_tgz(&c.tgz_bytes(), &values, &plain);
        let res = compare_template(
            &bin,
            || Some(o.template_chart(&c.tgz_path(), &values, &plain)),
            &ours,
        );
        record(&mut failures, c, &values, res);
    }
    finish("chart::template vs helm template", charts.len(), failures);
}

fn record(failures: &mut Vec<String>, c: &ChartEntry, values: &str, res: Option<Mismatch>) {
    match res {
        None => eprintln!("  ok    {} {}", c.name, c.version),
        Some(m) => {
            eprintln!("  FAIL  {} {} ({:?})", c.name, c.version, m.kind);
            let mut s = format!(
                "=== {} {} ({}): {:?}\n{}",
                c.name, c.version, c.file, m.kind, m.report
            );
            match &c.values {
                Some(p) => {
                    let _ = write!(s, "\nvalues ({p}):\n{values}");
                }
                None => s.push_str("\nvalues: chart defaults"),
            }
            failures.push(s);
        }
    }
}

fn finish(what: &str, total: usize, failures: Vec<String>) {
    eprintln!("{what}: {}/{total} charts match", total - failures.len());
    if failures.is_empty() {
        return;
    }
    for f in &failures {
        eprintln!("\n{f}");
    }
    panic!(
        "{what}: {} of {total} charts differ (see above)",
        failures.len()
    );
}
