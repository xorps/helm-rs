//! Line diffs for readable test failures, and comparison modulo lines that
//! are nondeterministic in Helm's own output (random passwords, certs, ...).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Line `a[i]` == `b[j]`.
    Eq(usize, usize),
    /// Line `a[i]` only in `a`.
    Del(usize),
    /// Line `b[j]` only in `b`.
    Ins(usize),
}

/// Splits into lines, keeping a marker for a missing final newline so that
/// "a\n" and "a" differ.
pub fn lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&str> = s.split('\n').collect();
    if s.ends_with('\n') {
        v.pop();
    } else {
        v.push("\\ No newline at end of file");
    }
    v
}

/// Myers' O(ND) diff of two line sequences. Falls back to delete-all /
/// insert-all (after trimming common prefix and suffix) when the edit
/// distance exceeds `MAX_D`, to bound time and memory.
pub fn diff<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Op> {
    const MAX_D: usize = 4000;
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut ops: Vec<Op> = (0..pre).map(|i| Op::Eq(i, i)).collect();
    let mid = myers(am, bm, MAX_D).unwrap_or_else(|| {
        let mut v: Vec<Op> = (0..am.len()).map(Op::Del).collect();
        v.extend((0..bm.len()).map(Op::Ins));
        v
    });
    ops.extend(mid.into_iter().map(|op| match op {
        Op::Eq(i, j) => Op::Eq(i + pre, j + pre),
        Op::Del(i) => Op::Del(i + pre),
        Op::Ins(j) => Op::Ins(j + pre),
    }));
    let (ao, bo) = (a.len() - suf, b.len() - suf);
    ops.extend((0..suf).map(|k| Op::Eq(ao + k, bo + k)));
    ops
}

fn myers<T: PartialEq>(a: &[T], b: &[T], max_d: usize) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    if max == 0 {
        return Some(Vec::new());
    }
    let off = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'outer: for d in 0..=(max.min(max_d) as isize) {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) { v[idx + 1] } else { v[idx - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }
    let d_end = found?;
    // Backtrack.
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..=d_end).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && v[(k - 1 + off) as usize] < v[(k + 1 + off) as usize]) { k + 1 } else { k - 1 };
        let prev_x = v[(prev_k + off) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Eq(x as usize, y as usize));
        }
        if d > 0 {
            if x == prev_x {
                y -= 1;
                ops.push(Op::Ins(y as usize));
            } else {
                x -= 1;
                ops.push(Op::Del(x as usize));
            }
        }
        x = prev_x;
        y = prev_y;
    }
    ops.reverse();
    Some(ops)
}

/// A unified diff (`--- a` / `+++ b`, 3 lines of context), at most
/// `max_lines` lines long. Empty if equal.
pub fn unified(a_name: &str, a: &str, b_name: &str, b: &str, max_lines: usize) -> String {
    let (al, bl) = (lines(a), lines(b));
    let ops = diff(&al, &bl);
    if ops.iter().all(|o| matches!(o, Op::Eq(..))) {
        return String::new();
    }
    let mut out = format!("--- {a_name}\n+++ {b_name}\n");
    const CTX: usize = 3;
    // Group into hunks.
    let changed: Vec<usize> = ops.iter().enumerate().filter(|(_, o)| !matches!(o, Op::Eq(..))).map(|(i, _)| i).collect();
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &c in &changed {
        let (s, e) = (c.saturating_sub(CTX), (c + CTX + 1).min(ops.len()));
        match hunks.last_mut() {
            Some(h) if s <= h.1 => h.1 = e,
            _ => hunks.push((s, e)),
        }
    }
    let mut written = 2;
    for (s, e) in hunks {
        let (mut a0, mut b0) = (None, None);
        let (mut ac, mut bc) = (0, 0);
        for op in &ops[s..e] {
            match *op {
                Op::Eq(i, j) => {
                    a0.get_or_insert(i);
                    b0.get_or_insert(j);
                    ac += 1;
                    bc += 1;
                }
                Op::Del(i) => {
                    a0.get_or_insert(i);
                    ac += 1;
                }
                Op::Ins(j) => {
                    b0.get_or_insert(j);
                    bc += 1;
                }
            }
        }
        let _ = writeln!(out, "@@ -{},{} +{},{} @@", a0.map_or(0, |x| x + 1), ac, b0.map_or(0, |x| x + 1), bc);
        written += 1;
        for op in &ops[s..e] {
            if written >= max_lines {
                out.push_str("... (diff truncated)\n");
                return out;
            }
            let line = match *op {
                Op::Eq(i, _) => format!(" {}", al[i]),
                Op::Del(i) => format!("-{}", al[i]),
                Op::Ins(j) => format!("+{}", bl[j]),
            };
            out.push_str(&line);
            out.push('\n');
            written += 1;
        }
    }
    out
}

/// Line indices of `a` that differ from `b` (not in their LCS): the
/// nondeterministic lines when `a` and `b` are two renders of the same input.
pub fn volatile_lines(a: &str, b: &str) -> BTreeSet<usize> {
    let (al, bl) = (lines(a), lines(b));
    diff(&al, &bl).into_iter().filter_map(|o| if let Op::Del(i) = o { Some(i) } else { None }).collect()
}

/// Whether `actual` equals `expected` except on `volatile` lines of
/// `expected`: every differing hunk must replace only volatile lines (with
/// any number of lines, since e.g. PEM blocks vary in length).
pub fn eq_modulo_volatile(expected: &str, volatile: &BTreeSet<usize>, actual: &str) -> bool {
    if expected == actual {
        return true;
    }
    // Go pointer addresses differ between runs (and we print a fixed one).
    if super::compare::normalize_addresses(expected) == super::compare::normalize_addresses(actual) {
        return true;
    }
    if volatile.is_empty() {
        return false;
    }
    let (el, al) = (lines(expected), lines(actual));
    let ops = diff(&el, &al);
    let mut i = 0;
    while i < ops.len() {
        if matches!(ops[i], Op::Eq(..)) {
            i += 1;
            continue;
        }
        let mut dels = 0;
        while i < ops.len() && !matches!(ops[i], Op::Eq(..)) {
            if let Op::Del(k) = ops[i] {
                if !volatile.contains(&k) {
                    return false;
                }
                dels += 1;
            }
            i += 1;
        }
        if dels == 0 {
            return false;
        }
    }
    true
}

/// Per-file outputs compared modulo volatile lines. Returns a readable report
/// of the first mismatch (missing/extra files are listed first), or `None`.
pub fn compare_files(
    expected: &BTreeMap<String, String>,
    expected_again: Option<&BTreeMap<String, String>>,
    actual: &BTreeMap<String, String>,
    exp_label: &str,
    act_label: &str,
) -> Option<String> {
    let ek: BTreeSet<_> = expected.keys().collect();
    let ak: BTreeSet<_> = actual.keys().collect();
    let mut report = String::new();
    let missing: Vec<_> = ek.difference(&ak).collect();
    let extra: Vec<_> = ak.difference(&ek).collect();
    if !missing.is_empty() {
        let _ = writeln!(report, "files only in {exp_label}: {missing:?}");
    }
    if !extra.is_empty() {
        let _ = writeln!(report, "files only in {act_label}: {extra:?}");
    }
    let mut ndiff = 0;
    for (name, exp) in expected {
        let Some(act) = actual.get(name) else { continue };
        let vol = expected_again.and_then(|o| o.get(name)).map(|e2| volatile_lines(exp, e2)).unwrap_or_default();
        if eq_modulo_volatile(exp, &vol, act) {
            continue;
        }
        ndiff += 1;
        if ndiff == 1 {
            let _ = writeln!(report, "first differing file: {name}");
            report.push_str(&unified(&format!("{exp_label}/{name}"), exp, &format!("{act_label}/{name}"), act, 80));
        }
    }
    if ndiff > 1 {
        let _ = writeln!(report, "({} more differing files)", ndiff - 1);
    }
    if report.is_empty() { None } else { Some(report) }
}

/// Whole-text comparison modulo volatile lines, with a unified diff report.
pub fn compare_text(expected: &str, expected_again: Option<&str>, actual: &str, exp_label: &str, act_label: &str) -> Option<String> {
    let vol = expected_again.map(|e2| volatile_lines(expected, e2)).unwrap_or_default();
    if eq_modulo_volatile(expected, &vol, actual) {
        return None;
    }
    Some(unified(exp_label, expected, act_label, actual, 80))
}

/// A template location `name:line[:col]` mentioned in a Go/Helm error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrLoc {
    pub name: String,
    pub line: u32,
    pub col: Option<u32>,
}

impl std::fmt::Display for ErrLoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.col {
            Some(c) => write!(f, "{}:{}:{}", self.name, self.line, c),
            None => write!(f, "{}:{}", self.name, self.line),
        }
    }
}

/// All `path:line[:col]` locations in a Helm error message, outermost first
/// (Go nests `include`/`tpl` errors inside the caller's message, so the last
/// one is the innermost).
pub fn error_locations(msg: &str) -> Vec<ErrLoc> {
    let mut out = Vec::new();
    for tok in msg.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '"' | '<' | '>' | ',' | '[' | ']')) {
        let tok = tok.trim_end_matches(':');
        let parts: Vec<&str> = tok.rsplitn(3, ':').collect();
        let (name, line, col) = match parts.as_slice() {
            [c, l, n] if c.parse::<u32>().is_ok() && l.parse::<u32>().is_ok() => (*n, l.parse().unwrap(), Some(c.parse().unwrap())),
            [l, rest @ ..] if l.parse::<u32>().is_ok() && !rest.is_empty() => {
                // `name:line` -- rest is reversed [name-tail, name-head?]
                let name_len = tok.len() - l.len() - 1;
                (&tok[..name_len], l.parse().unwrap(), None)
            }
            _ => continue,
        };
        if name.contains('/') && !name.contains("://") {
            out.push(ErrLoc { name: name.to_string(), line, col });
        }
    }
    out
}

/// Removes the hint `helm template` appends to render errors.
pub fn strip_cli_hint(msg: &str) -> &str {
    msg.trim_end().trim_end_matches("Use --debug flag to render out invalid YAML").trim_end()
}
