//! The real, pinned `helm` binary (`scripts/fetch-helm.sh`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Opts, repo_root};

pub const HELM_VERSION: &str = "v4.3.0";

/// `$HELM_BIN`, or `.tools/helm-v4.3.0/helm`; `None` (with a skip message)
/// if missing.
pub fn helm_bin() -> Option<PathBuf> {
    let p = std::env::var_os("HELM_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join(format!(".tools/helm-{HELM_VERSION}/helm")));
    if p.exists() {
        return Some(p);
    }
    if std::env::var_os("HELM_ORACLE_REQUIRED").is_some() {
        panic!("helm binary not found at {}; run scripts/fetch-helm.sh", p.display());
    }
    eprintln!("skipping: helm binary not found at {} (run scripts/fetch-helm.sh)", p.display());
    None
}

fn scratch_file(contents: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = repo_root().join("target/tmp/helm-bin");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("values-{}-{}.yaml", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&p, contents).unwrap();
    p
}

/// Runs `helm template <release> <chart> [-f values] ...` in a hermetic
/// environment. `Ok(stdout)` or `Err(message)` where the message is stderr
/// without the leading `Error: ` (what the oracle's `template` op returns).
pub fn helm_template(helm: &Path, chart: &Path, values_yaml: &str, opts: &Opts) -> Result<String, String> {
    let home = repo_root().join("target/tmp/helm-home");
    let mut cmd = Command::new(helm);
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("KUBECONFIG", "/dev/null")
        .env("HELM_NAMESPACE", opts.namespace.as_deref().unwrap_or("default"))
        .env("HELM_CONFIG_HOME", home.join("config"))
        .env("HELM_CACHE_HOME", home.join("cache"))
        .env("HELM_DATA_HOME", home.join("data"));
    cmd.arg("template").arg(opts.release.as_deref().unwrap_or("release-name")).arg(chart);
    let vf = (!values_yaml.is_empty()).then(|| scratch_file(values_yaml));
    if let Some(v) = &vf {
        cmd.arg("-f").arg(v);
    }
    if let Some(k) = &opts.kube_version {
        cmd.arg("--kube-version").arg(k);
    }
    for a in &opts.api_versions {
        cmd.arg("--api-versions").arg(a);
    }
    if opts.is_upgrade {
        cmd.arg("--is-upgrade");
    }
    if opts.skip_schema {
        cmd.arg("--skip-schema-validation");
    }
    if opts.include_crds {
        cmd.arg("--include-crds");
    }
    let out = cmd.output().expect("run helm");
    if let Some(v) = vf {
        let _ = std::fs::remove_file(v);
    }
    if out.status.success() {
        Ok(String::from_utf8(out.stdout).expect("helm stdout utf-8"))
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        // Warnings (e.g. deprecations) may precede the error line.
        let msg = match err.find("Error: ") {
            Some(i) => &err[i + "Error: ".len()..],
            None => &err[..],
        };
        Err(msg.trim_end_matches('\n').to_string())
    }
}
