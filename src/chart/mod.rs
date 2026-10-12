//! Charts: loading, values coalescing, dependencies, and rendering.
//!
//! This is a port of Helm v4.3.0's chart handling: `loader.LoadFiles` (and
//! the directory loader's `.helmignore` rules), `.tgz` archives,
//! `Chart.yaml` decoding/validation, `CoalesceValues`,
//! `ProcessDependencies`, `ToRenderValues`, the engine's per-template values
//! (`recAllTpls`), and what `helm template` prints.

// `Error` carries a full source location; boxing it would only move the cost.
#![allow(clippy::result_large_err)]

#[cfg(feature = "archive")]
mod archive;
mod capabilities;
mod coalesce;
mod decode;
mod dependencies;
mod files;
mod formats;
mod glob;
mod gostr;
mod ignore;
mod loader;
mod manifest;
mod metadata;
mod schema;
mod values;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::{Error, ErrorKind};
use crate::func::Env;
use crate::template::{Engine, Renderable};
use crate::value::{Map, MapKind, MapRef, Value};

pub use capabilities::{
    BuildInfo, CapabilitiesObject, DEFAULT_API_VERSIONS, DEFAULT_KUBE_VERSION, KubeVersion, VersionSet,
};
pub use coalesce::{coalesce_tables, coalesce_values, merge_tables, merge_values};
pub use dependencies::{is_compatible_range, process_dependencies};
pub use files::Files;
pub use glob::Glob;
pub use ignore::Rules as IgnoreRules;
pub use loader::{MAX_DECOMPRESSED_CHART_SIZE, YamlParser};
pub use manifest::{Hook, INSTALL_ORDER, Manifest, sort_manifests, split_manifests};
pub use metadata::{Dependency, Lock, Maintainer, Metadata};
pub use schema::validate_against_schema;

/// A file in a chart (`common.File`).
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    /// Path relative to the chart root, e.g. `templates/svc.yaml`.
    pub name: String,
    pub data: Rc<[u8]>,
}

/// A loaded chart (Helm `chart.Chart`, apiVersion v1/v2), with subcharts.
#[derive(Clone, Debug)]
pub struct Chart {
    pub metadata: Metadata,
    pub lock: Option<Lock>,
    /// Files under `templates/`.
    pub templates: Vec<File>,
    /// Default values (`values.yaml`).
    pub values: MapRef,
    /// `values.schema.json`, if present.
    pub schema: Option<Rc<[u8]>>,
    /// All other files (`.Files`), including `crds/`.
    pub files: Vec<File>,
    /// Subcharts (from `charts/`), in directory order.
    pub dependencies: Vec<Chart>,
}

impl Chart {
    pub(crate) fn empty() -> Chart {
        Chart {
            metadata: Metadata::default(),
            lock: None,
            templates: Vec::new(),
            values: MapRef::default(),
            schema: None,
            files: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    /// Loads a chart from in-memory files, as `helm` loads a chart
    /// directory (`loader.LoadDir`): paths are relative to the chart root
    /// (`Chart.yaml`, `templates/x.yaml`, `charts/sub/Chart.yaml`,
    /// `charts/sub-1.0.0.tgz`, ...). `.helmignore` rules are applied (plus
    /// the default `templates/.?*`), UTF-8 BOMs are stripped and Helm's
    /// 100 MiB size limit is enforced.
    pub fn from_files<I, P, D>(files: I) -> Result<Chart, Error>
    where
        I: IntoIterator<Item = (P, D)>,
        P: Into<String>,
        D: Into<Vec<u8>>,
    {
        Chart::from_files_with_parser(files, crate::yaml::from_str)
    }

    /// [`Chart::from_files`] with a custom YAML parser.
    #[doc(hidden)]
    pub fn from_files_with_parser<I, P, D>(files: I, yaml: YamlParser) -> Result<Chart, Error>
    where
        I: IntoIterator<Item = (P, D)>,
        P: Into<String>,
        D: Into<Vec<u8>>,
    {
        let files = files.into_iter().map(|(p, d)| (p.into(), d.into())).collect();
        loader::load_dir_files(files, yaml)
    }

    /// Loads a chart from a `.tgz` archive (Helm `loader.LoadArchive`).
    #[cfg(feature = "archive")]
    pub fn from_tgz(bytes: &[u8]) -> Result<Chart, Error> {
        Chart::from_tgz_with_parser(bytes, crate::yaml::from_str)
    }

    /// [`Chart::from_tgz`] with a custom YAML parser.
    #[cfg(feature = "archive")]
    #[doc(hidden)]
    pub fn from_tgz_with_parser(bytes: &[u8], yaml: YamlParser) -> Result<Chart, Error> {
        let files = archive::load_archive_files(bytes)?;
        loader::load_files(files, "", yaml)
    }

    pub fn name(&self) -> &str {
        &self.metadata.name
    }

    /// `type: library`.
    pub fn is_library(&self) -> bool {
        self.metadata.chart_type.eq_ignore_ascii_case("library")
    }

    /// What templates see as `.Chart` (without `IsRoot`).
    pub fn metadata_as_map(&self) -> Value {
        self.metadata.as_map()
    }

    /// `chart.CRDObjects()`: `(file name for output, file)` pairs for
    /// `crds/*.{yaml,yml,json}` of this chart and its subcharts.
    pub fn crd_objects(&self) -> Vec<(String, File)> {
        let mut out = Vec::new();
        self.crd_objects_rec(self.name(), &mut out);
        out
    }

    fn crd_objects_rec(&self, full_path: &str, out: &mut Vec<(String, File)>) {
        for f in &self.files {
            let e = gostr::ext(&f.name);
            let manifest_ext =
                e.eq_ignore_ascii_case(".yaml") || e.eq_ignore_ascii_case(".yml") || e.eq_ignore_ascii_case(".json");
            if f.name.starts_with("crds/") && manifest_ext {
                out.push((gostr::join(&[full_path, &f.name]), f.clone()));
            }
        }
        for d in &self.dependencies {
            d.crd_objects_rec(&alloc::format!("{full_path}/charts/{}", d.name()), out);
        }
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
    /// e.g. `v1.37.0` (parsed like `helm template --kube-version`).
    pub kube_version: String,
    /// The full list (Helm appends `--api-versions` to the defaults).
    pub api_versions: Vec<String>,
    /// e.g. `v4.3.0`.
    pub helm_version: String,
    pub git_commit: String,
    pub git_tree_state: String,
    pub go_version: String,
    pub kube_client_version: String,
}

impl Default for Capabilities {
    /// Helm v4.3.0's `common.DefaultCapabilities` (as `helm template` uses
    /// them), with the official release binary's build info.
    fn default() -> Self {
        let b = BuildInfo::default();
        Capabilities {
            kube_version: DEFAULT_KUBE_VERSION.into(),
            api_versions: DEFAULT_API_VERSIONS.iter().map(|s| s.to_string()).collect(),
            helm_version: b.version,
            git_commit: b.git_commit,
            git_tree_state: b.git_tree_state,
            go_version: b.go_version,
            kube_client_version: b.kube_client_version,
        }
    }
}

impl Capabilities {
    /// Builds the `.Capabilities` object. Errors like `helm template
    /// --kube-version` on an unparsable version.
    pub fn to_object(&self) -> Result<CapabilitiesObject, Error> {
        let kv = KubeVersion::parse(&self.kube_version).map_err(|e| {
            Error::new(ErrorKind::Chart, alloc::format!("invalid kube version '{}': {e}", self.kube_version))
        })?;
        Ok(CapabilitiesObject {
            kube_version: Rc::new(kv),
            api_versions: Rc::new(VersionSet(self.api_versions.clone())),
            helm_version: Rc::new(BuildInfo {
                version: self.helm_version.clone(),
                git_commit: self.git_commit.clone(),
                git_tree_state: self.git_tree_state.clone(),
                go_version: self.go_version.clone(),
                kube_client_version: self.kube_client_version.clone(),
            }),
        })
    }
}

/// Options for [`render`] and [`template`].
#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    pub release: ReleaseOptions,
    pub capabilities: Capabilities,
    /// `missingkey=error`.
    pub strict: bool,
    pub skip_schema_validation: bool,
    /// `helm template --include-crds`.
    pub include_crds: bool,
    /// `helm template --skip-tests`.
    pub skip_tests: bool,
}

fn user_values(values: &Value) -> MapRef {
    match values {
        Value::Map(m) => m.clone(),
        _ => MapRef::default(),
    }
}

/// `ToRenderValuesWithSchemaValidation`: the top-level template object
/// (`Chart`, `Capabilities`, `Release`, `Values`). Call after
/// [`process_dependencies`].
pub fn to_render_values(
    chart: &Chart,
    values: &Value,
    opts: &RenderOptions,
    caps: &CapabilitiesObject,
) -> Result<MapRef, Error> {
    let top = MapRef::new_values(Map::new());
    top.insert("Chart", chart.metadata.as_map());
    top.insert("Capabilities", Value::Object(Rc::new(caps.clone())));
    let mut rel = Map::new();
    rel.insert("Name".into(), Value::str(&opts.release.name));
    rel.insert("Namespace".into(), Value::str(&opts.release.namespace));
    rel.insert("IsUpgrade".into(), Value::Bool(opts.release.is_upgrade));
    rel.insert("IsInstall".into(), Value::Bool(opts.release.is_install));
    rel.insert("Revision".into(), Value::Int(opts.release.revision));
    rel.insert("Service".into(), Value::str("Helm"));
    top.insert("Release", Value::map(rel));
    let vals = coalesce_values(chart, &user_values(values))?.with_kind(MapKind::HelmValues);
    if !opts.skip_schema_validation {
        if let Err(e) = schema::validate_chart(chart, &vals) {
            return Err(Error::new(
                ErrorKind::Values,
                alloc::format!("values don't meet the specifications of the schema(s) in the following chart(s):\n{e}"),
            ));
        }
    }
    top.insert("Values", Value::Map(vals));
    Ok(top)
}

/// Builds the engine input like Helm's `allTemplates`/`recAllTpls`.
pub fn renderables(chart: &Chart, top: &MapRef) -> Vec<Renderable> {
    let mut out = Vec::new();
    rec_all_tpls(chart, &mut out, top, true, chart.name());
    out
}

fn rec_all_tpls(c: &Chart, out: &mut Vec<Renderable>, vals: &MapRef, is_root: bool, full_path: &str) -> MapRef {
    let sub_charts = MapRef::new(Map::new());
    let meta = c.metadata.as_map();
    if let Value::Map(m) = &meta {
        m.insert("IsRoot", Value::Bool(is_root));
    }
    let files = Files::new(c.files.iter().map(|f| (f.name.clone(), f.data.clone())));
    let next = MapRef::new_values(Map::new());
    next.insert("Chart", meta);
    next.insert("Files", Value::object(files));
    next.insert("Release", vals.get("Release").unwrap_or_default());
    next.insert("Capabilities", vals.get("Capabilities").unwrap_or_default());
    next.insert("Values", Value::Map(MapRef::new_values(Map::new())));
    next.insert("Subcharts", Value::Map(sub_charts.clone()));
    if is_root {
        next.insert("Values", vals.get("Values").unwrap_or_default());
    } else if let Some(vs) = values::table(vals, &alloc::format!("Values.{}", c.name())) {
        next.insert("Values", Value::Map(vs.with_kind(MapKind::HelmValues)));
    }
    for child in &c.dependencies {
        let child_path = alloc::format!("{full_path}/charts/{}", child.name());
        let sub = rec_all_tpls(child, out, &next, false, &child_path);
        // Go: `map[string]interface{}` here; `common.Values` only as `.`.
        sub_charts.insert(child.name(), Value::Map(sub.with_kind(MapKind::Plain)));
    }
    for t in &c.templates {
        if c.is_library() && !gostr::base(&t.name).starts_with('_') {
            continue;
        }
        out.push(Renderable {
            name: gostr::join(&[full_path, &t.name]),
            source: Rc::from(String::from_utf8_lossy(&t.data).as_ref()),
            vals: Value::Map(next.clone()),
            base_path: gostr::join(&[full_path, "templates"]),
        });
    }
    next
}

fn check_kube_version(chart: &Chart, caps: &CapabilitiesObject) -> Result<(), Error> {
    let kv = &chart.metadata.kube_version;
    if !kv.is_empty() && !is_compatible_range(kv, caps.kube_version.string()) {
        return Err(Error::new(
            ErrorKind::Chart,
            alloc::format!(
                "chart requires kubeVersion: {kv} which is incompatible with Kubernetes {}",
                caps.kube_version.version
            ),
        ));
    }
    Ok(())
}

/// Renders every template of `chart` (and its enabled subcharts) with the
/// user-supplied `values` merged over the chart defaults, like Helm's
/// `engine.Render` after `ProcessDependencies` and `ToRenderValues`. Keys are
/// template names such as `mychart/templates/deployment.yaml`.
pub fn render(
    chart: &Chart,
    values: &Value,
    opts: &RenderOptions,
    env: &mut dyn Env,
) -> Result<BTreeMap<String, String>, Error> {
    let engine = Engine { strict: opts.strict, ..Engine::default() };
    render_with_engine(&engine, chart, values, opts, env)
}

/// [`render`] with a custom engine (functions, lint mode, ...).
pub fn render_with_engine(
    engine: &Engine,
    chart: &Chart,
    values: &Value,
    opts: &RenderOptions,
    env: &mut dyn Env,
) -> Result<BTreeMap<String, String>, Error> {
    let caps = opts.capabilities.to_object()?;
    let mut c = chart.clone();
    process_dependencies(&mut c, &user_values(values))?;
    let top = to_render_values(&c, values, opts, &caps)?;
    check_kube_version(&c, &caps)?;
    engine.render(env, renderables(&c, &top))
}

const DEBUG_HINT: &str = "\n\nUse --debug flag to render out invalid YAML";

fn with_debug_hint(e: Error) -> Error {
    let msg = alloc::format!("{}{DEBUG_HINT}", e.helm_message());
    e.with_helm_message(msg)
}

/// Produces exactly what `helm template` prints to stdout.
pub fn template(chart: &Chart, values: &Value, opts: &RenderOptions, env: &mut dyn Env) -> Result<String, Error> {
    let engine = Engine { strict: opts.strict, ..Engine::default() };
    template_with(&engine, chart, values, opts, env, crate::yaml::from_str)
}

/// [`template`] with a custom engine and YAML parser (for manifest sorting).
#[doc(hidden)]
pub fn template_with(
    engine: &Engine,
    chart: &Chart,
    values: &Value,
    opts: &RenderOptions,
    env: &mut dyn Env,
    yaml: YamlParser,
) -> Result<String, Error> {
    let caps = opts.capabilities.to_object()?;
    match chart.metadata.chart_type.as_str() {
        "" | "application" => {}
        t => return Err(Error::new(ErrorKind::Chart, alloc::format!("{t} charts are not installable"))),
    }
    if let Some(reqs) = chart.metadata.dependencies.as_ref().filter(|r| !r.is_empty()) {
        let missing: Vec<&str> = reqs
            .iter()
            .filter(|r| !chart.dependencies.iter().any(|d| d.name() == r.name))
            .map(|r| r.name.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(Error::new(
                ErrorKind::Chart,
                alloc::format!(
                    "an error occurred while checking for chart dependencies. You may need to run 'helm dependency build' to fetch missing dependencies: found in Chart.yaml, but missing in charts/ directory: {}",
                    missing.join(", ")
                ),
            ));
        }
    }
    let mut c = chart.clone();
    process_dependencies(&mut c, &user_values(values)).map_err(|e| e.context("chart dependencies processing failed"))?;
    let top = to_render_values(&c, values, opts, &caps)?;
    check_kube_version(&c, &caps).map_err(with_debug_hint)?;
    let files = engine.render(env, renderables(&c, &top)).map_err(with_debug_hint)?;
    template_output(&c, files, opts, yaml).map_err(with_debug_hint)
}

/// The part of `helm template` after rendering: drops `NOTES.txt`, sorts
/// manifests and hooks, prepends CRDs (with `include_crds`) and formats the
/// output. `chart` must be the dependency-processed chart.
#[doc(hidden)]
pub fn template_output(
    chart: &Chart,
    mut files: BTreeMap<String, String>,
    opts: &RenderOptions,
    yaml: YamlParser,
) -> Result<String, Error> {
    files.retain(|k, _| !k.ends_with("NOTES.txt"));
    let (hooks, manifests) = sort_manifests(&files, yaml)?;
    let mut b = String::new();
    if opts.include_crds {
        for (name, f) in chart.crd_objects() {
            b.push_str(&alloc::format!("---\n# Source: {name}\n{}\n", String::from_utf8_lossy(&f.data)));
        }
    }
    for m in &manifests {
        b.push_str(&alloc::format!("---\n# Source: {}\n{}\n", m.name, m.content));
    }
    let mut out = String::from(b.trim());
    out.push('\n');
    for h in &hooks {
        if opts.skip_tests && h.events.iter().any(|e| e == "test") {
            continue;
        }
        out.push_str(&alloc::format!("---\n# Source: {}\n{}\n", h.path, h.manifest));
    }
    Ok(out)
}
