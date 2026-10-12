//! Charts: loading, values coalescing, dependencies, and rendering.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Error;
use crate::func::Env;
use crate::value::Value;

/// A loaded chart (Helm `chart.Chart`, apiVersion v1/v2), with subcharts.
#[derive(Clone, Debug)]
pub struct Chart {
    _private: (),
}

impl Chart {
    /// Loads a chart from in-memory files (Helm `loader.LoadFiles`). Paths are
    /// relative to the chart root (`Chart.yaml`, `templates/x.yaml`,
    /// `charts/sub/Chart.yaml`, `charts/sub-1.0.0.tgz`, ...).
    pub fn from_files<I, P, D>(files: I) -> Result<Chart, Error>
    where
        I: IntoIterator<Item = (P, D)>,
        P: Into<String>,
        D: Into<Vec<u8>>,
    {
        let _ = files;
        todo!("Chart::from_files")
    }

    /// Loads a chart from a `.tgz` archive (Helm `loader.LoadArchive`).
    #[cfg(feature = "archive")]
    pub fn from_tgz(bytes: &[u8]) -> Result<Chart, Error> {
        let _ = bytes;
        todo!("Chart::from_tgz")
    }

    pub fn name(&self) -> &str {
        todo!("Chart::name")
    }
}

/// Release information exposed as `.Release`.
#[derive(Clone, Debug)]
pub struct ReleaseOptions {
    pub name: String,
    pub namespace: String,
    pub revision: i64,
    pub is_install: bool,
    pub is_upgrade: bool,
}

impl Default for ReleaseOptions {
    /// What `helm template` uses: `release-name` in `default`, revision 1.
    fn default() -> Self {
        ReleaseOptions {
            name: "release-name".into(),
            namespace: "default".into(),
            revision: 1,
            is_install: true,
            is_upgrade: false,
        }
    }
}

/// Cluster capabilities exposed as `.Capabilities`.
#[derive(Clone, Debug)]
pub struct Capabilities {
    /// e.g. `v1.37.0`.
    pub kube_version: String,
    pub api_versions: Vec<String>,
    /// e.g. `v4.3.0`.
    pub helm_version: String,
}

/// Options for [`render`] and [`template`].
#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub release: ReleaseOptions,
    pub capabilities: Capabilities,
    /// `missingkey=error`.
    pub strict: bool,
    pub skip_schema_validation: bool,
}

/// Renders every template of `chart` (and its enabled subcharts) with the
/// user-supplied `values` merged over the chart defaults, like Helm's
/// `engine.Render`. Keys are template names such as
/// `mychart/templates/deployment.yaml`.
pub fn render(
    chart: &Chart,
    values: &Value,
    opts: &RenderOptions,
    env: &mut dyn Env,
) -> Result<BTreeMap<String, String>, Error> {
    let _ = (chart, values, opts, env);
    todo!("chart::render")
}

/// Produces exactly what `helm template` prints to stdout.
pub fn template(chart: &Chart, values: &Value, opts: &RenderOptions, env: &mut dyn Env) -> Result<String, Error> {
    let _ = (chart, values, opts, env);
    todo!("chart::template")
}
