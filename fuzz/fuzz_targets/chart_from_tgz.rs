//! `Chart::from_tgz` on arbitrary bytes must not panic; charts it accepts are
//! rendered (errors are fine, panics are bugs). Seed the corpus with real
//! charts: `cp testdata/charts/cache/*.tgz fuzz/corpus/chart_from_tgz/`.
#![no_main]

use helm_rs::chart::{self, Chart, RenderOptions};
use helm_rs::{DeterministicEnv, Value};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(chart) = Chart::from_tgz(data) else {
        return;
    };
    // `helm template` defaults (Kubernetes v1.37.0, Helm v4.3.0).
    let opts = RenderOptions::default();
    let _ = chart::render(
        &chart,
        &Value::empty_map(),
        &opts,
        &mut DeterministicEnv::default(),
    );
});
