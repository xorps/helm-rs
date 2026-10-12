//! Test support: a client for the Go oracle (`oracle/`, real Helm pinned in
//! `oracle/go.mod`). Build it with `scripts/build-oracle.sh`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value as Json, json};

pub mod chart;
pub mod compare;
pub mod corpus;
pub mod diff;
pub mod helm_bin;
pub mod rng;
pub mod rust;

/// Locks the shared oracle, or `None` (after printing a skip message) if it
/// is not built.
pub fn oracle() -> Option<std::sync::MutexGuard<'static, Oracle>> {
    Some(Oracle::shared()?.lock().unwrap_or_else(|e| e.into_inner()))
}

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn oracle_path() -> PathBuf {
    std::env::var_os("HELM_ORACLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/helm-oracle"))
}

/// Result of rendering with real Helm.
#[derive(Debug, Clone, PartialEq)]
pub enum OracleResult {
    Ok(BTreeMap<String, String>),
    Err { stage: String, message: String },
}

/// Extra request fields (release name, kube version, ...).
#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub release: Option<String>,
    pub namespace: Option<String>,
    pub kube_version: Option<String>,
    pub api_versions: Vec<String>,
    pub is_upgrade: bool,
    pub strict: bool,
    pub skip_schema: bool,
    /// `now` returns the Unix epoch (UTC), like `DeterministicEnv`.
    pub fixed_time: bool,
    /// `helm template --include-crds` (template op only).
    pub include_crds: bool,
}

pub struct Oracle {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Oracle {
    pub fn spawn() -> Option<Oracle> {
        let path = oracle_path();
        if !path.exists() {
            if std::env::var_os("HELM_ORACLE_REQUIRED").is_some() {
                panic!(
                    "oracle not found at {}; run scripts/build-oracle.sh",
                    path.display()
                );
            }
            eprintln!(
                "skipping: oracle not found at {} (run scripts/build-oracle.sh)",
                path.display()
            );
            return None;
        }
        let mut child = Command::new(path)
            // "Local" time must match DeterministicEnv (UTC).
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn oracle");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Some(Oracle {
            _child: child,
            stdin,
            stdout,
        })
    }

    /// Process-wide shared oracle (tests run in parallel threads).
    pub fn shared() -> Option<&'static Mutex<Oracle>> {
        static ORACLE: OnceLock<Option<Mutex<Oracle>>> = OnceLock::new();
        ORACLE
            .get_or_init(|| Oracle::spawn().map(Mutex::new))
            .as_ref()
    }

    pub fn request(&mut self, req: &Json) -> Json {
        let mut line = serde_json::to_string(req).unwrap();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
        let mut resp = String::new();
        self.stdout.read_line(&mut resp).unwrap();
        serde_json::from_str(&resp).unwrap_or_else(|e| panic!("bad oracle response {resp:?}: {e}"))
    }

    fn finish(&mut self, req: Json, values: &str, opts: &Opts) -> OracleResult {
        let req = with_opts(req, values, opts);
        let resp = self.request(&req);
        parse_render(&resp)
    }

    /// Runs many requests (built with [`with_opts`]) in one round trip.
    pub fn batch(&mut self, reqs: Vec<Json>) -> Vec<Json> {
        let n = reqs.len();
        let resp = self.request(&json!({"op": "batch", "requests": reqs}));
        let results = resp
            .get("results")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(results.len(), n, "bad batch response: {resp}");
        results
    }

    /// `helm template` stdout for a chart directory or .tgz on disk, computed
    /// in-process by the oracle (same code path as the CLI).
    pub fn template_chart(
        &mut self,
        path: &std::path::Path,
        values: &str,
        opts: &Opts,
    ) -> TemplateResult {
        let resp = self.request(&with_opts(
            json!({"op": "template", "path": path}),
            values,
            opts,
        ));
        parse_template(&resp)
    }

    /// `helm template` stdout for an in-memory chart.
    pub fn template_files(
        &mut self,
        files: &BTreeMap<String, String>,
        values: &str,
        opts: &Opts,
    ) -> TemplateResult {
        let resp = self.request(&with_opts(
            json!({"op": "template", "files": files}),
            values,
            opts,
        ));
        parse_template(&resp)
    }

    /// The chart's default values coalesced with `values` and all subchart
    /// defaults, as JSON. Without a chart: `values` parsed by Helm, as JSON.
    pub fn chart_values(
        &mut self,
        path: Option<&std::path::Path>,
        values: &str,
    ) -> Result<Json, String> {
        let mut req = json!({"op": "values", "values": values});
        if let Some(p) = path {
            req["path"] = json!(p);
        }
        let resp = self.request(&req);
        if let Some(err) = resp.get("error").and_then(Json::as_str) {
            return Err(err.to_string());
        }
        Ok(resp.get("json").cloned().unwrap_or(Json::Null))
    }
}

/// Result of the oracle's `template` op: stdout, or the CLI error message.
pub type TemplateResult = Result<String, String>;

/// Adds `values` and `opts` to a request object.
pub fn with_opts(mut req: Json, values: &str, opts: &Opts) -> Json {
    {
        let o = req.as_object_mut().unwrap();
        o.insert("values".into(), json!(values));
        if let Some(r) = &opts.release {
            o.insert("release".into(), json!(r));
        }
        if let Some(n) = &opts.namespace {
            o.insert("namespace".into(), json!(n));
        }
        if let Some(k) = &opts.kube_version {
            o.insert("kube_version".into(), json!(k));
        }
        o.insert("api_versions".into(), json!(opts.api_versions));
        o.insert("is_upgrade".into(), json!(opts.is_upgrade));
        o.insert("strict".into(), json!(opts.strict));
        o.insert("skip_schema".into(), json!(opts.skip_schema));
        o.insert("fixed_time".into(), json!(opts.fixed_time));
        o.insert("include_crds".into(), json!(opts.include_crds));
    }
    req
}

/// Parses a `chart`/`files` op response.
pub fn parse_render(resp: &Json) -> OracleResult {
    if let Some(err) = resp.get("error").and_then(Json::as_str) {
        return OracleResult::Err {
            stage: resp
                .get("stage")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string(),
            message: err.to_string(),
        };
    }
    let files = resp
        .get("files")
        .and_then(Json::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                .collect()
        })
        .unwrap_or_default();
    OracleResult::Ok(files)
}

/// Parses a `template` op response.
pub fn parse_template(resp: &Json) -> TemplateResult {
    if let Some(err) = resp.get("error").and_then(Json::as_str) {
        return Err(err.to_string());
    }
    Ok(resp
        .get("output")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string())
}

impl Oracle {
    /// Renders an in-memory chart (`path -> content`, e.g. `Chart.yaml`,
    /// `templates/x.yaml`) with user values (YAML).
    pub fn render_files(
        &mut self,
        files: &BTreeMap<String, String>,
        values: &str,
        opts: &Opts,
    ) -> OracleResult {
        self.finish(json!({"op": "files", "files": files}), values, opts)
    }

    /// Renders a chart directory or .tgz on disk.
    pub fn render_chart(
        &mut self,
        path: &std::path::Path,
        values: &str,
        opts: &Opts,
    ) -> OracleResult {
        self.finish(json!({"op": "chart", "path": path}), values, opts)
    }

    pub fn version_info(&mut self) -> Json {
        self.request(&json!({"op": "version"}))["info"].clone()
    }
}

/// Minimal chart around a single template `templates/t.yaml`, with chart
/// default values `values_yaml`. The rendered output is keyed
/// `t/templates/t.yaml`.
pub fn single_template_chart(template: &str, values_yaml: &str) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    files.insert(
        "Chart.yaml".to_string(),
        "apiVersion: v2\nname: t\nversion: 0.1.0\n".to_string(),
    );
    files.insert("values.yaml".to_string(), values_yaml.to_string());
    files.insert("templates/t.yaml".to_string(), template.to_string());
    files
}

/// Renders a single template with real Helm. `Ok(output)` or `Err(message)`.
/// Returns `None` if the oracle is not built.
pub fn helm_render_one(template: &str, values_yaml: &str) -> Option<Result<String, String>> {
    let oracle = Oracle::shared()?;
    let mut o = oracle.lock().unwrap_or_else(|e| e.into_inner());
    Some(
        match o.render_files(
            &single_template_chart(template, values_yaml),
            "",
            &Opts::default(),
        ) {
            OracleResult::Ok(files) => {
                Ok(files.get("t/templates/t.yaml").cloned().unwrap_or_default())
            }
            OracleResult::Err { message, .. } => Err(message),
        },
    )
}
