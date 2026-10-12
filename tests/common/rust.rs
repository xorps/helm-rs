//! Calls into helm-rs, with panics (e.g. unimplemented `todo!()`s) caught and
//! reported as outcomes instead of aborting the whole test.
#![allow(clippy::result_large_err)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Once, OnceLock};

use helm_rs::chart::{self, Capabilities, Chart, ReleaseOptions, RenderOptions};
use helm_rs::{DeterministicEnv, Error, ErrorKind, Value};

use super::{Opts, Oracle};

/// Result of a helm-rs call.
#[derive(Debug)]
pub enum Outcome<T> {
    Ok(T),
    Err(Error),
    /// helm-rs panicked (message and location).
    Panic(String),
}

impl<T> Outcome<T> {
    pub fn is_ok(&self) -> bool {
        matches!(self, Outcome::Ok(_))
    }

    /// A one-line description of a failure (for reports).
    pub fn describe_failure(&self) -> String {
        match self {
            Outcome::Ok(_) => "ok".into(),
            Outcome::Err(e) => format!("error: {}", e.helm_message()),
            Outcome::Panic(p) => format!("PANIC: {p}"),
        }
    }
}

thread_local! {
    static QUIET: Cell<bool> = const { Cell::new(false) };
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn install_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if QUIET.with(Cell::get) {
                let msg = info
                    .payload()
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| info.payload().downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic>".into());
                let loc = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
                LAST_PANIC.with(|p| *p.borrow_mut() = Some(format!("{msg}{loc}")));
            } else {
                prev(info)
            }
        }));
    });
}

/// Runs `f`, turning a panic into `Outcome::Panic` without printing it.
pub fn catch<T>(f: impl FnOnce() -> Result<T, Error>) -> Outcome<T> {
    install_hook();
    QUIET.with(|q| q.set(true));
    let r = catch_unwind(AssertUnwindSafe(f));
    QUIET.with(|q| q.set(false));
    match r {
        Ok(Ok(v)) => Outcome::Ok(v),
        Ok(Err(e)) => Outcome::Err(e),
        Err(_) => Outcome::Panic(LAST_PANIC.with(|p| p.borrow_mut().take()).unwrap_or_else(|| "panic".into())),
    }
}

/// Default capabilities as reported by the oracle (so both sides agree), or
/// Helm v4.3.0's defaults when the oracle is unavailable.
pub fn default_capabilities() -> Capabilities {
    static CAPS: OnceLock<Capabilities> = OnceLock::new();
    CAPS.get_or_init(|| {
        let info = Oracle::shared().map(|o| o.lock().unwrap_or_else(|e| e.into_inner()).version_info());
        let get = |k: &str| info.as_ref().and_then(|i| i.get(k)).and_then(|v| v.as_str()).map(str::to_string);
        Capabilities {
            kube_version: get("kube_version").unwrap_or_else(|| "v1.37.0".into()),
            api_versions: get("api_versions")
                .map(|s| s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect())
                .unwrap_or_else(|| Capabilities::default().api_versions),
            helm_version: get("helm_version").unwrap_or_else(|| "v4.3.0".into()),
            ..Capabilities::default()
        }
    })
    .clone()
}

/// [`RenderOptions`] equivalent to an oracle request with `opts`.
pub fn render_options(opts: &Opts) -> RenderOptions {
    let mut caps = default_capabilities();
    if let Some(k) = &opts.kube_version {
        caps.kube_version = if k.starts_with('v') { k.clone() } else { format!("v{k}") };
    }
    caps.api_versions.extend(opts.api_versions.iter().cloned());
    RenderOptions {
        release: ReleaseOptions {
            name: opts.release.clone().unwrap_or_else(|| "release-name".into()),
            namespace: opts.namespace.clone().unwrap_or_else(|| "default".into()),
            revision: 1,
            is_install: !opts.is_upgrade,
            is_upgrade: opts.is_upgrade,
        },
        capabilities: caps,
        strict: opts.strict,
        skip_schema_validation: opts.skip_schema,
        include_crds: opts.include_crds,
        skip_tests: false,
    }
}

/// User values YAML -> [`Value`] (empty document -> empty map).
pub fn parse_values(yaml: &str) -> Result<Value, Error> {
    match helm_rs::yaml::from_str(yaml) {
        Ok(Value::Nil) => Ok(Value::empty_map()),
        Ok(v) => Ok(v),
        Err(e) => Err(Error::new(ErrorKind::Syntax, e.message)),
    }
}

pub fn render_tgz(tgz: &[u8], values_yaml: &str, opts: &Opts) -> Outcome<BTreeMap<String, String>> {
    catch(|| {
        let chart = Chart::from_tgz(tgz)?;
        let values = parse_values(values_yaml)?;
        chart::render(&chart, &values, &render_options(opts), &mut DeterministicEnv::default())
    })
}

pub fn template_tgz(tgz: &[u8], values_yaml: &str, opts: &Opts) -> Outcome<String> {
    catch(|| {
        let chart = Chart::from_tgz(tgz)?;
        let values = parse_values(values_yaml)?;
        chart::template(&chart, &values, &render_options(opts), &mut DeterministicEnv::default())
    })
}

pub fn render_files(files: &BTreeMap<String, String>, values_yaml: &str, opts: &Opts) -> Outcome<BTreeMap<String, String>> {
    catch(|| {
        let chart = Chart::from_files(files.iter().map(|(k, v)| (k.clone(), v.as_bytes().to_vec())))?;
        let values = parse_values(values_yaml)?;
        chart::render(&chart, &values, &render_options(opts), &mut DeterministicEnv::default())
    })
}
