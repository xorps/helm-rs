//! Comparing helm-rs results with Helm's.

use std::collections::BTreeMap;
use std::fmt::Write;

use super::OracleResult;
use super::diff::{self, error_locations};
use super::rng::lax_errors;
use super::rust::Outcome;

/// How helm-rs disagreed with Helm (used to keep minimization on target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Both succeed, different output.
    Output,
    /// Helm succeeds, helm-rs returns an error.
    HelmOkOursErr,
    /// Helm fails, helm-rs succeeds.
    HelmErrOursOk,
    /// Both fail with different messages.
    ErrMessage,
    /// Both fail, the reported location differs.
    ErrLocation,
    /// helm-rs panicked.
    Panic,
}

#[derive(Debug, Clone)]
pub struct Mismatch {
    pub kind: Kind,
    pub report: String,
}

impl Mismatch {
    fn new(kind: Kind, report: String) -> Mismatch {
        Mismatch { kind, report }
    }
}

/// Compares per-template render results. `again` is a second Helm render of
/// the same input, used to ignore nondeterministic lines; it is only
/// computed when the first comparison fails.
pub fn compare_render(
    helm: &OracleResult,
    again: impl FnOnce() -> Option<OracleResult>,
    ours: &Outcome<BTreeMap<String, String>>,
) -> Option<Mismatch> {
    match (helm, ours) {
        (_, Outcome::Panic(p)) => {
            let h = match helm {
                OracleResult::Ok(_) => "helm succeeds".to_string(),
                OracleResult::Err { message, .. } => format!("helm fails: {message}"),
            };
            Some(Mismatch::new(Kind::Panic, format!("{h}\nhelm-rs PANIC: {p}")))
        }
        (OracleResult::Ok(fa), Outcome::Ok(fr)) => {
            diff::compare_files(fa, None, fr, "helm", "helm-rs")?;
            let fb = match again() {
                Some(OracleResult::Ok(fb)) => Some(fb),
                _ => None,
            };
            diff::compare_files(fa, fb.as_ref(), fr, "helm", "helm-rs").map(|r| Mismatch::new(Kind::Output, r))
        }
        (OracleResult::Err { stage, message }, Outcome::Err(e)) => compare_errors(&format!("({stage}) {message}"), message, e),
        (OracleResult::Ok(_), Outcome::Err(e)) => {
            Some(Mismatch::new(Kind::HelmOkOursErr, format!("helm succeeds, helm-rs fails:\n{e}")))
        }
        (OracleResult::Err { stage, message }, Outcome::Ok(_)) => {
            Some(Mismatch::new(Kind::HelmErrOursOk, format!("helm fails ({stage}): {message}\nhelm-rs succeeds")))
        }
    }
}

/// Compares `helm template` stdout (or CLI error) with `chart::template`.
pub fn compare_template(
    helm: &Result<String, String>,
    again: impl FnOnce() -> Option<Result<String, String>>,
    ours: &Outcome<String>,
) -> Option<Mismatch> {
    match (helm, ours) {
        (_, Outcome::Panic(p)) => {
            let h = match helm {
                Ok(_) => "helm template succeeds".to_string(),
                Err(e) => format!("helm template fails: {e}"),
            };
            Some(Mismatch::new(Kind::Panic, format!("{h}\nhelm-rs PANIC: {p}")))
        }
        (Ok(out), Outcome::Ok(mine)) => {
            diff::compare_text(out, None, mine, "helm template", "helm-rs template")?;
            let b = again().and_then(Result::ok);
            diff::compare_text(out, b.as_deref(), mine, "helm template", "helm-rs template").map(|r| Mismatch::new(Kind::Output, r))
        }
        (Err(be), Outcome::Err(e)) => compare_errors(be, diff::strip_cli_hint(be), e),
        (Ok(_), Outcome::Err(e)) => Some(Mismatch::new(Kind::HelmOkOursErr, format!("helm template succeeds, helm-rs fails:\n{e}"))),
        (Err(be), Outcome::Ok(_)) => Some(Mismatch::new(Kind::HelmErrOursOk, format!("helm template fails: {be}\nhelm-rs succeeds"))),
    }
}

/// Compares a Helm error with ours: `helm_message()` must equal Helm's
/// message, and our innermost location must equal the innermost
/// `file:line[:col]` in Helm's message. Skipped with `HELM_RS_LAX_ERRORS=1`.
pub fn compare_errors(display: &str, helm_msg: &str, ours: &helm_rs::Error) -> Option<Mismatch> {
    if lax_errors() {
        return None;
    }
    let mine = ours.helm_message();
    let mut kind = None;
    let mut report = String::new();
    if diff::strip_cli_hint(&mine) != diff::strip_cli_hint(helm_msg) {
        kind = Some(Kind::ErrMessage);
        let _ = writeln!(report, "both fail, with different messages:\n  helm:    {display}\n  helm-rs: {mine}");
    }
    if let Some(hl) = error_locations(helm_msg).pop() {
        let ok = matches!(&ours.location, Some(l) if l.name == hl.name && l.line == hl.line && hl.col.is_none_or(|c| c == l.column));
        if !ok {
            kind.get_or_insert(Kind::ErrLocation);
            let mine = ours.location.as_ref().map_or("none".to_string(), |l| format!("{}:{}:{}", l.name, l.line, l.column));
            let _ = writeln!(report, "error location differs: helm {hl}, helm-rs {mine}");
        }
    }
    let kind = kind?;
    let _ = write!(report, "helm-rs diagnostic:\n{ours}");
    Some(Mismatch::new(kind, report))
}
