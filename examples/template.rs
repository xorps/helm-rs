//! A minimal `helm template`: renders a chart archive with optional values.
//!
//! ```sh
//! cargo run --example template -- mychart-1.0.0.tgz [values.yaml]
//! ```

use std::process::ExitCode;

use helm_rs::chart::{self, Chart, RenderOptions};
use helm_rs::{DeterministicEnv, Value, yaml};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: template <chart.tgz> [values.yaml]");
        return ExitCode::from(2);
    };
    match run(path, args.get(1)) {
        Ok(manifest) => {
            print!("{manifest}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(chart_path: &str, values_path: Option<&String>) -> Result<String, String> {
    let bytes = std::fs::read(chart_path).map_err(|e| format!("{chart_path}: {e}"))?;
    let chart = Chart::from_tgz(&bytes).map_err(|e| e.to_string())?;

    // User values (`-f values.yaml`), parsed exactly like Helm does.
    let values = match values_path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
            yaml::unmarshal(&text, yaml::Target::Map("common.Values")).map_err(|e| format!("{p}: {}", e.message))?
        }
        None => Value::empty_map(),
    };

    // Defaults match `helm template`: release "release-name" in "default",
    // Kubernetes v1.37.0, Helm v4.3.0 capabilities.
    let opts = RenderOptions::default();
    // Clock, randomness and `lookup` come from the environment; this one is
    // deterministic (fixed clock at the Unix epoch, seeded randomness).
    let mut env = DeterministicEnv::default();

    // The error's Display is a full diagnostic (file:line:col, source line
    // with a caret, include/tpl call stack).
    chart::template(&chart, &values, &opts, &mut env).map_err(|e| e.to_string())
}
