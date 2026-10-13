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
            Some(Mismatch::new(
                Kind::Panic,
                format!("{h}\nhelm-rs PANIC: {p}"),
            ))
        }
        (OracleResult::Ok(fa), Outcome::Ok(fr)) => {
            diff::compare_files(fa, None, fr, "helm", "helm-rs")?;
            let fb = match again() {
                Some(OracleResult::Ok(fb)) => Some(fb),
                _ => None,
            };
            diff::compare_files(fa, fb.as_ref(), fr, "helm", "helm-rs")
                .map(|r| Mismatch::new(Kind::Output, r))
        }
        // Helm crashed outright; failing with an error is the better outcome.
        (OracleResult::Err { stage, .. }, Outcome::Err(_)) if stage == super::CRASH_STAGE => None,
        (OracleResult::Err { stage, message }, Outcome::Err(e)) => {
            compare_errors(&format!("({stage}) {message}"), message, e)
        }
        (OracleResult::Ok(_), Outcome::Err(e)) => Some(Mismatch::new(
            Kind::HelmOkOursErr,
            format!("helm succeeds, helm-rs fails:\n{e}"),
        )),
        (OracleResult::Err { stage, message }, Outcome::Ok(_)) => Some(Mismatch::new(
            Kind::HelmErrOursOk,
            format!("helm fails ({stage}): {message}\nhelm-rs succeeds"),
        )),
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
            Some(Mismatch::new(
                Kind::Panic,
                format!("{h}\nhelm-rs PANIC: {p}"),
            ))
        }
        (Ok(out), Outcome::Ok(mine)) => {
            diff::compare_text(out, None, mine, "helm template", "helm-rs template")?;
            let b = again().and_then(Result::ok);
            diff::compare_text(out, b.as_deref(), mine, "helm template", "helm-rs template")
                .map(|r| Mismatch::new(Kind::Output, r))
        }
        (Err(be), Outcome::Err(e)) => compare_errors(be, diff::strip_cli_hint(be), e),
        (Ok(_), Outcome::Err(e)) => Some(Mismatch::new(
            Kind::HelmOkOursErr,
            format!("helm template succeeds, helm-rs fails:\n{e}"),
        )),
        (Err(be), Outcome::Ok(_)) => Some(Mismatch::new(
            Kind::HelmErrOursOk,
            format!("helm template fails: {be}\nhelm-rs succeeds"),
        )),
    }
}

/// Compares a Helm error with ours: `helm_message()` must equal Helm's
/// message exactly (which pins Go's line:col), and our innermost diagnostic
/// location must be on the same file and line as Helm's innermost location.
/// Our column is intentionally different: it is 1-based and points at the
/// failing segment, where Go reports a 0-based byte offset of the node.
/// Skipped with `HELM_RS_LAX_ERRORS=1`.
pub fn compare_errors(display: &str, helm_msg: &str, ours: &helm_rs::Error) -> Option<Mismatch> {
    if lax_errors() {
        return None;
    }
    let mine = ours.helm_message();
    let mut kind = None;
    let mut report = String::new();
    // Unbounded recursion: Helm stops at ~1000 nested includes, we stop at
    // Engine::max_depth with a different message. Both failing is a match.
    if mine.contains("exceeded maximum template depth")
        && (helm_msg.contains("nested reference name")
            || helm_msg.contains("exceeded maximum template depth"))
    {
        return None;
    }
    let (mine_n, helm_n) = (normalize_addresses(&mine), normalize_addresses(helm_msg));
    if diff::strip_cli_hint(&mine_n) != diff::strip_cli_hint(&helm_n)
        && !same_tokens_reordered(&mine_n, &helm_n)
    {
        kind = Some(Kind::ErrMessage);
        let _ = writeln!(
            report,
            "both fail, with different messages:\n  helm:    {display}\n  helm-rs: {mine}"
        );
    }
    // Helm messages may mention several locations (call chains, "action
    // started at X"); ours must agree with one of them on file and line.
    let all_locs = error_locations(helm_msg);
    if let Some(hl) = all_locs.last().cloned() {
        // Go's parser reports the line of the last token it read, which can
        // be past the offending one; our parse locations point at the token.
        let is_parse = helm_msg.contains("parse error");
        let any_match = all_locs.iter().any(|hl| {
            let same = |l: &helm_rs::Location| {
                l.name == hl.name && (l.line == hl.line || (is_parse && l.line < hl.line))
            };
            ours.location().is_some_and(same) || ours.frames().iter().any(|f| same(&f.location))
        });
        // Helm reports `fail`/`required` at the outermost template; we report
        // where the call happened and list the callers as frames.
        let same = |l: &helm_rs::Location| l.name == hl.name && l.line == hl.line;
        let ok = any_match
            || ours.location().is_some_and(same)
            || ours.frames().iter().any(|f| same(&f.location));
        if !ok {
            kind.get_or_insert(Kind::ErrLocation);
            let mine = ours.location().map_or("none".to_string(), |l| {
                format!("{}:{}:{}", l.name, l.line, l.column)
            });
            let _ = writeln!(report, "error location differs: helm {hl}, helm-rs {mine}");
        }
    }
    let kind = kind?;
    let _ = write!(report, "helm-rs diagnostic:\n{ours}");
    Some(Mismatch::new(kind, report))
}

/// True when two messages contain the same tokens in a different order. Go
/// iterates maps in random order (e.g. `keys $m | join ","`), so Helm itself
/// produces such variations between runs.
fn same_tokens_reordered(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let toks = |s: &str| {
        let mut v: Vec<String> = s
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        v.sort();
        v
    };
    toks(a) == toks(b)
}

/// Replaces Go pointer addresses (`0xc000123456`), which differ per run.
pub fn normalize_addresses(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'0'
            && i + 1 < b.len()
            && b[i + 1] == b'x'
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
        {
            let mut j = i + 2;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j - i - 2 >= 8 {
                out.extend_from_slice(b"0xADDR");
                i = j;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).expect("only ASCII was replaced")
}
