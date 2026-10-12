//! `Chart::from_tgz` on arbitrary bytes must not panic; charts it accepts are
//! rendered (errors are fine, panics are bugs). Seed the corpus with real
//! charts: `cp testdata/charts/cache/*.tgz fuzz/corpus/chart_from_tgz/`.
#![no_main]

use helm_rs::chart::{self, Capabilities, Chart, ReleaseOptions, RenderOptions};
use helm_rs::{DeterministicEnv, Value};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(chart) = Chart::from_tgz(data) else {
        return;
    };
    let opts = RenderOptions {
        release: ReleaseOptions::default(),
        capabilities: Capabilities {
            kube_version: "v1.37.0".into(),
            api_versions: Vec::new(),
            helm_version: "v4.3.0".into(),
        },
        strict: false,
        skip_schema_validation: false,
    };
    let _ = chart::render(
        &chart,
        &Value::empty_map(),
        &opts,
        &mut DeterministicEnv::default(),
    );
});
