//! The pinned chart corpus (`testdata/charts/charts.lock`), fetched into
//! `testdata/charts/cache/` by `scripts/fetch-charts.sh`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value as Json;

use super::repo_root;

#[derive(Debug, Clone)]
pub struct ChartEntry {
    /// Corpus name (unique), e.g. `bitnami-redis`.
    pub name: String,
    /// Helm repo URL or `oci://` reference.
    pub repo: String,
    /// Chart name inside the repo (= top directory in the .tgz).
    pub chart: String,
    pub version: String,
    /// File name in the cache directory.
    pub file: String,
    pub sha256: String,
    /// Values file relative to `testdata/charts/`, if the chart needs one.
    pub values: Option<String>,
    /// Whether the pinned `helm template` rendered it when the lock was written.
    pub helm_template_ok: bool,
    /// The error `helm template` printed, if it failed.
    pub error: String,
}

pub fn charts_dir() -> PathBuf {
    repo_root().join("testdata/charts")
}

pub fn cache_dir() -> PathBuf {
    charts_dir().join("cache")
}

pub fn lock_path() -> PathBuf {
    charts_dir().join("charts.lock")
}

/// The parsed lock file.
pub fn lock_json() -> Json {
    let text = std::fs::read_to_string(lock_path()).expect("read testdata/charts/charts.lock");
    serde_json::from_str(&text).expect("parse charts.lock")
}

/// Every chart in the lock.
pub fn lock() -> Vec<ChartEntry> {
    let j = lock_json();
    j["charts"]
        .as_array()
        .expect("charts array")
        .iter()
        .map(|c| {
            let s = |k: &str| c.get(k).and_then(Json::as_str).unwrap_or("").to_string();
            ChartEntry {
                name: s("name"),
                repo: s("repo"),
                chart: s("chart"),
                version: s("version"),
                file: s("file"),
                sha256: s("sha256"),
                values: Some(s("values")).filter(|v| !v.is_empty()),
                helm_template_ok: s("helm_template") == "ok",
                error: s("error"),
            }
        })
        .collect()
}

/// Charts selected by `HELM_RS_CHARTS=name1,name2` (all if unset).
pub fn selected() -> Vec<ChartEntry> {
    let filter: Option<Vec<String>> = std::env::var("HELM_RS_CHARTS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    let all = lock();
    if let Some(f) = &filter {
        for name in f {
            assert!(
                all.iter().any(|c| &c.name == name),
                "HELM_RS_CHARTS: unknown chart {name:?}"
            );
        }
    }
    all.into_iter()
        .filter(|c| filter.as_ref().is_none_or(|f| f.contains(&c.name)))
        .collect()
}

/// Selected charts whose .tgz is in the cache. Prints a skip message for
/// missing ones (or panics if `HELM_RS_CHARTS_REQUIRED` is set).
pub fn available() -> Vec<ChartEntry> {
    let sel = selected();
    let (have, missing): (Vec<_>, Vec<_>) = sel.into_iter().partition(|c| c.tgz_path().exists());
    if !missing.is_empty() {
        let names: Vec<_> = missing.iter().map(|c| c.name.as_str()).collect();
        let msg = format!(
            "charts not in cache (run scripts/fetch-charts.sh): {}",
            names.join(", ")
        );
        if std::env::var_os("HELM_RS_CHARTS_REQUIRED").is_some() {
            panic!("{msg}");
        }
        eprintln!("skipping: {msg}");
    }
    have
}

impl ChartEntry {
    pub fn tgz_path(&self) -> PathBuf {
        cache_dir().join(&self.file)
    }

    pub fn tgz_bytes(&self) -> Vec<u8> {
        std::fs::read(self.tgz_path())
            .unwrap_or_else(|e| panic!("read {}: {e}", self.tgz_path().display()))
    }

    pub fn values_path(&self) -> Option<PathBuf> {
        self.values.as_ref().map(|v| charts_dir().join(v))
    }

    /// The user values the corpus renders this chart with ("" if none).
    pub fn values_yaml(&self) -> String {
        self.values_path()
            .map(|p| {
                std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
            })
            .unwrap_or_default()
    }

    /// Extracts the chart (and any packaged subcharts in `charts/*.tgz`) into
    /// a scratch directory, once; returns the chart root directory.
    pub fn extracted(&self) -> PathBuf {
        let base = repo_root()
            .join("target/tmp/charts-extracted")
            .join(format!("{}-{}", self.name, self.version));
        let root = base.join(&self.chart);
        let marker = base.join(".done");
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if marker.exists() {
            return root;
        }
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        untar(&self.tgz_path(), &base);
        extract_subcharts(&root);
        std::fs::write(&marker, "").unwrap();
        root
    }
}

fn untar(tgz: &Path, into: &Path) {
    let st = Command::new("tar")
        .arg("-xzf")
        .arg(tgz)
        .arg("-C")
        .arg(into)
        .status()
        .expect("run tar");
    assert!(st.success(), "tar -xzf {} failed", tgz.display());
}

fn extract_subcharts(chart: &Path) {
    let charts = chart.join("charts");
    let Ok(rd) = std::fs::read_dir(&charts) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.extension().is_some_and(|e| e == "tgz") {
            untar(&p, &charts);
        }
    }
    if let Ok(rd) = std::fs::read_dir(&charts) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                extract_subcharts(&e.path());
            }
        }
    }
}

/// All regular files under `dir`, as (relative path with `/`, bytes), sorted.
pub fn walk_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn go(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                go(base, &p, out);
            } else if p.is_file() {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    go(dir, dir, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// sha256 of a file via the system tool (`sha256sum` or `shasum -a 256`).
pub fn sha256_file(path: &Path) -> Option<String> {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .or_else(|| {
            Command::new("shasum")
                .args(["-a", "256"])
                .arg(path)
                .output()
                .ok()
                .filter(|o| o.status.success())
        })?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
}
