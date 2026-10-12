//! Values fuzzing on the chart corpus: mutated value sets derived from each
//! chart's (coalesced) default values are rendered by helm-rs and by real Helm
//! (the oracle); both must succeed or fail together, with identical output
//! (or identical error message and location).
//!
//! Mutations: flipping booleans (especially `*.enabled`), toggling all
//! feature flags under a subtree together, numbers (0, negative, huge,
//! floats), tricky strings (empty, numeric-looking, "true", multi-line,
//! unicode, YAML-special, template syntax, very long), YAML 1.1 plain
//! scalars (`yes`, `0x1F`, `~`, `.inf`, ...), deleting keys / null, unknown
//! keys, scalar <-> list/map swaps, list edits, and targeted knobs
//! (`image.tag`, `replicaCount`, `resources`, `ingress`, `persistence`,
//! `autoscaling`, `serviceMonitor`, `global.*`, ...).
//!
//! Failures are minimized (greedy removal of mutations) and reported with the
//! seed, the mutations and the resulting values file.
//!
//! Knobs:
//! * `HELM_RS_FUZZ_SEED`: seed (printed on every run).
//! * `HELM_RS_FUZZ_ITERS`: cases per chart (default 16).
//! * `HELM_RS_FUZZ_THREADS`: parallel oracles (default: half the CPUs, max 8).
//! * `HELM_RS_CHARTS=a,b`: chart subset. `HELM_RS_LAX_ERRORS=1`.

mod common;

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::compare::{Kind, Mismatch, compare_render};
use common::corpus::{self, ChartEntry};
use common::rng::{Rng, fuzz_iters, fuzz_seed};
use common::{Opts, Oracle, OracleResult, rust};
use serde_json::Value as Json;

// ---------------------------------------------------------------------------
// Value trees

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Null,
    Bool(bool),
    /// Number, as written.
    Num(String),
    Str(String),
    /// A plain (unquoted) YAML scalar, emitted verbatim.
    Raw(String),
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

#[derive(Clone, Debug, PartialEq)]
enum Seg {
    Key(String),
    Idx(usize),
}

type Path = Vec<Seg>;

fn path_str(p: &[Seg]) -> String {
    let mut s = String::new();
    for seg in p {
        match seg {
            Seg::Key(k)
                if k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    && !k.is_empty() =>
            {
                if !s.is_empty() {
                    s.push('.');
                }
                s.push_str(k);
            }
            Seg::Key(k) => s.push_str(&format!("[{k:?}]")),
            Seg::Idx(i) => s.push_str(&format!("[{i}]")),
        }
    }
    if s.is_empty() { "<root>".into() } else { s }
}

impl Node {
    fn from_json(j: &Json) -> Node {
        match j {
            Json::Null => Node::Null,
            Json::Bool(b) => Node::Bool(*b),
            Json::Number(n) => Node::Num(n.to_string()),
            Json::String(s) => Node::Str(s.clone()),
            Json::Array(a) => Node::List(a.iter().map(Node::from_json).collect()),
            Json::Object(m) => Node::Map(
                m.iter()
                    .map(|(k, v)| (k.clone(), Node::from_json(v)))
                    .collect(),
            ),
        }
    }

    fn get(&self, p: &[Seg]) -> Option<&Node> {
        let Some((first, rest)) = p.split_first() else {
            return Some(self);
        };
        match (self, first) {
            (Node::Map(m), Seg::Key(k)) => m.iter().find(|(kk, _)| kk == k)?.1.get(rest),
            (Node::List(l), Seg::Idx(i)) => l.get(*i)?.get(rest),
            _ => None,
        }
    }

    /// Sets the value at `p`, creating missing map entries (and replacing
    /// non-map intermediates with maps when `force`). Returns false if the
    /// path does not fit.
    fn set(&mut self, p: &[Seg], v: Node, force: bool) -> bool {
        let Some((first, rest)) = p.split_first() else {
            *self = v;
            return true;
        };
        if force
            && !matches!(
                (&*self, first),
                (Node::Map(_), Seg::Key(_)) | (Node::List(_), Seg::Idx(_))
            )
        {
            *self = Node::Map(Vec::new());
        }
        match (self, first) {
            (Node::Map(m), Seg::Key(k)) => {
                if let Some(e) = m.iter_mut().find(|(kk, _)| kk == k) {
                    return e.1.set(rest, v, force);
                }
                if !rest.is_empty() && !force {
                    return false;
                }
                let mut child = Node::Map(Vec::new());
                if !child.set(rest, v, true) {
                    return false;
                }
                m.push((k.clone(), child));
                true
            }
            (Node::List(l), Seg::Idx(i)) => match l.get_mut(*i) {
                Some(e) => e.set(rest, v, force),
                None => false,
            },
            _ => false,
        }
    }

    fn remove(&mut self, p: &[Seg]) -> bool {
        let Some((last, parent)) = p.split_last() else {
            return false;
        };
        let mut cur = self;
        for seg in parent {
            cur = match (cur, seg) {
                (Node::Map(m), Seg::Key(k)) => match m.iter_mut().find(|(kk, _)| kk == k) {
                    Some(e) => &mut e.1,
                    None => return false,
                },
                (Node::List(l), Seg::Idx(i)) => match l.get_mut(*i) {
                    Some(e) => e,
                    None => return false,
                },
                _ => return false,
            };
        }
        match (cur, last) {
            (Node::Map(m), Seg::Key(k)) => {
                let n = m.len();
                m.retain(|(kk, _)| kk != k);
                m.len() != n
            }
            (Node::List(l), Seg::Idx(i)) if *i < l.len() => {
                l.remove(*i);
                true
            }
            _ => false,
        }
    }

    /// Emits as YAML (JSON-compatible flow style, pretty printed).
    fn emit(&self, out: &mut String, indent: usize) {
        let pad = |n: usize| "  ".repeat(n);
        match self {
            Node::Null => out.push_str("null"),
            Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Node::Num(n) | Node::Raw(n) => out.push_str(n),
            Node::Str(s) => out.push_str(&serde_json::to_string(s).unwrap()),
            Node::List(l) if l.is_empty() => out.push_str("[]"),
            Node::Map(m) if m.is_empty() => out.push_str("{}"),
            Node::List(l) => {
                out.push_str("[\n");
                for (i, v) in l.iter().enumerate() {
                    out.push_str(&pad(indent + 1));
                    v.emit(out, indent + 1);
                    out.push_str(if i + 1 < l.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(indent));
                out.push(']');
            }
            Node::Map(m) => {
                out.push_str("{\n");
                for (i, (k, v)) in m.iter().enumerate() {
                    out.push_str(&pad(indent + 1));
                    out.push_str(&serde_json::to_string(k).unwrap());
                    out.push_str(": ");
                    v.emit(out, indent + 1);
                    out.push_str(if i + 1 < m.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(indent));
                out.push('}');
            }
        }
    }

    fn to_yaml(&self) -> String {
        let mut s = String::new();
        self.emit(&mut s, 0);
        s.push('\n');
        s
    }

    fn brief(&self) -> String {
        let mut s = String::new();
        match self {
            Node::List(l) if !l.is_empty() => s.push_str(&format!("[...{} items]", l.len())),
            Node::Map(m) if !m.is_empty() => s.push_str(&format!("{{...{} keys}}", m.len())),
            _ => self.emit(&mut s, 0),
        }
        if s.len() > 80 {
            let cut = (0..=77).rev().find(|&i| s.is_char_boundary(i)).unwrap();
            s.truncate(cut);
            s.push_str("...");
        }
        s
    }
}

/// Every path in `n` (maps fully, the first few list items), with depth limit.
fn collect_paths(n: &Node, prefix: &mut Path, out: &mut Vec<Path>) {
    if prefix.len() > 14 {
        return;
    }
    match n {
        Node::Map(m) => {
            for (k, v) in m {
                prefix.push(Seg::Key(k.clone()));
                out.push(prefix.clone());
                collect_paths(v, prefix, out);
                prefix.pop();
            }
        }
        Node::List(l) => {
            for (i, v) in l.iter().enumerate().take(3) {
                prefix.push(Seg::Idx(i));
                out.push(prefix.clone());
                collect_paths(v, prefix, out);
                prefix.pop();
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Mutations

#[derive(Clone, Debug)]
enum Op {
    Set(Node),
    Delete,
}

#[derive(Clone, Debug)]
struct Mutation {
    path: Path,
    op: Op,
    why: &'static str,
}

impl std::fmt::Display for Mutation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.op {
            Op::Set(v) => write!(
                f,
                "{} = {}  ({})",
                path_str(&self.path),
                v.brief(),
                self.why
            ),
            Op::Delete => write!(f, "delete {}  ({})", path_str(&self.path), self.why),
        }
    }
}

const TRICKY_STRINGS: &[&str] = &[
    "",
    " ",
    "0",
    "1",
    "-1",
    "1.0",
    "1e3",
    "0x1F",
    "true",
    "false",
    "yes",
    "no",
    "null",
    "~",
    "nil",
    "<nil>",
    "line1\nline2",
    "trailing newline\n",
    "\ttab",
    "unicode: ✓ ünïcødé 日本語 🚀",
    "key: value",
    "- item",
    "# comment",
    "{a: 1}",
    "[1, 2]",
    "'single' \"double\"",
    "back\\slash",
    "&anchor *alias !tag %percent @at `tick`",
    "{{ .Release.Name }}",
    "{{ fail \"injected\" }}",
    "$(echo hi)",
    "a,b,c",
    "http://example.com:8080/path?q=1#frag",
    "registry.example.com/org/image:1.2.3@sha256:0000000000000000000000000000000000000000000000000000000000000000",
    "100m",
    "1Gi",
    "  leading and trailing  ",
    "UPPER_case-Mixed.123",
];

const RAW_SCALARS: &[&str] = &[
    "yes",
    "no",
    "on",
    "off",
    "y",
    "n",
    "True",
    "FALSE",
    "~",
    "null",
    "Null",
    "0x1F",
    "0o17",
    "017",
    "08",
    ".inf",
    "-.inf",
    ".nan",
    "1e3",
    "1_000",
    "1:30",
    "2024-01-01",
    "2024-01-01T10:00:00Z",
    "0.",
    ".5",
    "+1",
    "-0",
    "1.0e+3",
];

const NUMBERS: &[&str] = &[
    "0",
    "1",
    "-1",
    "2",
    "3",
    "65535",
    "65536",
    "-2147483649",
    "1.5",
    "0.1",
    "1e6",
    "1000000",
    "1e21",
    "9007199254740993",
    "18446744073709551616",
    "-0.0",
    "3.14159",
    "100",
    "1e-7",
    "123456789012",
];

fn random_scalar(rng: &mut Rng) -> Node {
    match rng.below(6) {
        0 => Node::Bool(rng.chance(1, 2)),
        1 => Node::Num(rng.pick(NUMBERS).to_string()),
        2 => Node::Raw(rng.pick(RAW_SCALARS).to_string()),
        3 => Node::Null,
        4 => Node::Str("x".repeat(rng.pick(&[1usize, 63, 64, 253, 254, 5000]).to_owned())),
        _ => Node::Str(rng.pick(TRICKY_STRINGS).to_string()),
    }
}

fn random_tree(rng: &mut Rng, depth: usize) -> Node {
    if depth == 0 || rng.chance(1, 2) {
        return random_scalar(rng);
    }
    if rng.chance(1, 2) {
        Node::List(
            (0..rng.below(3))
                .map(|_| random_tree(rng, depth - 1))
                .collect(),
        )
    } else {
        let keys = [
            "a", "name", "value", "enabled", "key", "x-y", "1", "true", "nested",
        ];
        let mut m: Vec<(String, Node)> = Vec::new();
        for _ in 0..rng.below(4) {
            let k = rng.pick(&keys).to_string();
            if !m.iter().any(|(kk, _)| *kk == k) {
                m.push((k, random_tree(rng, depth - 1)));
            }
        }
        Node::Map(m)
    }
}

fn last_key(p: &[Seg]) -> Option<&str> {
    match p.last() {
        Some(Seg::Key(k)) => Some(k),
        _ => None,
    }
}

fn s(x: &str) -> Node {
    Node::Str(x.into())
}

fn map(kv: &[(&str, Node)]) -> Node {
    Node::Map(kv.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

fn child(p: &[Seg], k: &str) -> Path {
    let mut c = p.to_vec();
    c.push(Seg::Key(k.into()));
    c
}

struct Fuzzer<'a> {
    base: &'a Node,
    paths: Vec<Path>,
}

impl Fuzzer<'_> {
    fn pick_path(&self, rng: &mut Rng, pred: impl Fn(&Path, &Node) -> bool) -> Option<Path> {
        let cands: Vec<&Path> = self
            .paths
            .iter()
            .filter(|p| self.base.get(p).is_some_and(|n| pred(p, n)))
            .collect();
        if cands.is_empty() {
            None
        } else {
            Some(rng.pick(&cands).to_vec())
        }
    }

    fn generate(&self, rng: &mut Rng) -> Vec<Mutation> {
        let n = 1 + rng.weighted(&[5, 3, 2, 1, 1]);
        let mut muts = Vec::new();
        for _ in 0..n {
            for _attempt in 0..5 {
                let before = muts.len();
                self.gen_one(rng, &mut muts);
                if muts.len() > before {
                    break;
                }
            }
        }
        muts
    }

    fn gen_one(&self, rng: &mut Rng, out: &mut Vec<Mutation>) {
        let set = |path: Path, v: Node, why: &'static str| Mutation {
            path,
            op: Op::Set(v),
            why,
        };
        let strat = rng.weighted(&[16, 6, 6, 8, 6, 4, 6, 14, 4, 3]);
        match strat {
            0 => {
                // Flip a boolean, preferring `enabled` flags.
                let prefer = rng.chance(2, 3);
                let p = self
                    .pick_path(rng, |p, n| {
                        matches!(n, Node::Bool(_)) && (!prefer || last_key(p) == Some("enabled"))
                    })
                    .or_else(|| self.pick_path(rng, |_, n| matches!(n, Node::Bool(_))));
                if let Some(p) = p {
                    let Some(Node::Bool(b)) = self.base.get(&p) else {
                        return;
                    };
                    out.push(set(p, Node::Bool(!b), "flip bool"));
                }
            }
            1 => {
                // Toggle every `enabled` under a subtree together.
                let Some(root) = self.pick_path(rng, |_, n| matches!(n, Node::Map(_))) else {
                    return;
                };
                let on = rng.chance(1, 2);
                let flags: Vec<Path> = self
                    .paths
                    .iter()
                    .filter(|p| {
                        p.starts_with(&root)
                            && last_key(p) == Some("enabled")
                            && matches!(self.base.get(p), Some(Node::Bool(_)))
                    })
                    .take(40)
                    .cloned()
                    .collect();
                for p in flags {
                    out.push(set(p, Node::Bool(on), "toggle feature group"));
                }
            }
            2 => {
                if let Some(p) = self.pick_path(rng, |_, n| matches!(n, Node::Num(_))) {
                    let v = if rng.chance(1, 4) {
                        Node::Str(rng.pick(NUMBERS).to_string())
                    } else {
                        Node::Num(rng.pick(NUMBERS).to_string())
                    };
                    out.push(set(p, v, "change number"));
                }
            }
            3 => {
                if let Some(p) = self.pick_path(rng, |_, n| matches!(n, Node::Str(_))) {
                    let v = match rng.below(4) {
                        0 => Node::Raw(rng.pick(RAW_SCALARS).to_string()),
                        1 => Node::Str("x".repeat(*rng.pick(&[64usize, 254, 10000]))),
                        _ => Node::Str(rng.pick(TRICKY_STRINGS).to_string()),
                    };
                    out.push(set(p, v, "tricky string"));
                }
            }
            4 => {
                if let Some(p) = self.pick_path(rng, |_, _| true) {
                    out.push(Mutation {
                        path: p,
                        op: Op::Delete,
                        why: "delete key",
                    });
                }
            }
            5 => {
                if let Some(p) = self.pick_path(rng, |_, _| true) {
                    out.push(set(p, Node::Null, "set null"));
                }
            }
            6 => {
                // Unknown key.
                let p = self
                    .pick_path(rng, |_, n| matches!(n, Node::Map(_)))
                    .unwrap_or_default();
                let k = rng
                    .pick(&[
                        "fuzzUnknown",
                        "enabled",
                        "extra",
                        "UPPER",
                        "with space",
                        "dotted.key",
                        "0",
                    ])
                    .to_string();
                out.push(set(child(&p, &k), random_tree(rng, 2), "unknown key"));
            }
            7 => self.targeted(rng, out),
            8 => {
                // Swap scalar <-> list/map.
                if let Some(p) = self.pick_path(rng, |_, _| true) {
                    let cur = self.base.get(&p).unwrap();
                    let v = match cur {
                        Node::List(_) | Node::Map(_) => random_scalar(rng),
                        _ => rng
                            .pick(&[
                                Node::List(vec![]),
                                Node::Map(vec![]),
                                Node::List(vec![s("a")]),
                                map(&[("a", Node::Num("1".into()))]),
                            ])
                            .clone(),
                    }
                    .clone();
                    out.push(set(p, v, "type swap"));
                }
            }
            _ => {
                // List edits.
                if let Some(p) = self.pick_path(rng, |_, n| matches!(n, Node::List(_))) {
                    let Some(Node::List(l)) = self.base.get(&p) else {
                        return;
                    };
                    let mut l = l.clone();
                    match rng.below(3) {
                        0 => l.clear(),
                        1 if !l.is_empty() => l.push(l[0].clone()),
                        _ => l.push(random_tree(rng, 2)),
                    }
                    out.push(set(p, Node::List(l), "list edit"));
                }
            }
        }
    }

    /// Mutations of well-known knobs.
    fn targeted(&self, rng: &mut Rng, out: &mut Vec<Mutation>) {
        let set = |path: Path, v: Node, why: &'static str| Mutation {
            path,
            op: Op::Set(v),
            why,
        };
        let knobs = [
            "tag",
            "replicaCount",
            "replicas",
            "resources",
            "ingress",
            "persistence",
            "autoscaling",
            "serviceMonitor",
            "global",
            "annotations",
            "podAnnotations",
            "nodeSelector",
            "tolerations",
            "extraEnv",
            "image",
            "service",
        ];
        let knob = *rng.pick(&knobs);
        let p = self.pick_path(rng, |p, _| last_key(p) == Some(knob));
        let p = match (p, knob) {
            (Some(p), _) => p,
            (None, "global") => vec![Seg::Key("global".into())],
            (None, _) => return,
        };
        let cur = self.base.get(&p).cloned().unwrap_or(Node::Null);
        let has = |k: &str| matches!(&cur, Node::Map(m) if m.iter().any(|(kk, _)| kk == k));
        match knob {
            "tag" => {
                let v = rng
                    .pick(&[
                        Node::Num("1.10".into()),
                        Node::Num("1.0".into()),
                        Node::Num("2".into()),
                        s(""),
                        s("v1.2.3"),
                        s("latest"),
                        Node::Raw("1.20".into()),
                        Node::Null,
                    ])
                    .clone();
                out.push(set(p, v, "image tag"));
            }
            "replicaCount" | "replicas" => {
                let v = rng
                    .pick(&[
                        Node::Num("0".into()),
                        Node::Num("3".into()),
                        Node::Num("-1".into()),
                        s("2"),
                        Node::Num("1.5".into()),
                        Node::Raw("1e3".into()),
                    ])
                    .clone();
                out.push(set(p, v, "replicas"));
            }
            "resources" => {
                let v = match rng.below(3) {
                    0 => map(&[
                        ("limits", map(&[("cpu", s("100m")), ("memory", s("128Mi"))])),
                        (
                            "requests",
                            map(&[("cpu", Node::Num("0.5".into())), ("memory", s("64Mi"))]),
                        ),
                    ]),
                    1 => Node::Map(vec![]),
                    _ => map(&[("requests", map(&[("cpu", Node::Num("1".into()))]))]),
                };
                out.push(set(p, v, "resources"));
            }
            "ingress" if matches!(cur, Node::Map(_)) => {
                out.push(set(child(&p, "enabled"), Node::Bool(true), "ingress on"));
                if has("hosts") {
                    let host = if rng.chance(1, 2) {
                        map(&[
                            ("host", s("chart.example.com")),
                            (
                                "paths",
                                Node::List(vec![map(&[
                                    ("path", s("/")),
                                    ("pathType", s("Prefix")),
                                ])]),
                            ),
                        ])
                    } else {
                        s("chart.example.com")
                    };
                    out.push(set(
                        child(&p, "hosts"),
                        Node::List(vec![host]),
                        "ingress hosts",
                    ));
                }
                if has("hostname") {
                    out.push(set(
                        child(&p, "hostname"),
                        s("chart.example.com"),
                        "ingress hostname",
                    ));
                }
                if has("tls") && rng.chance(1, 2) {
                    let v = if matches!(cur.get(&[Seg::Key("tls".into())]), Some(Node::Bool(_))) {
                        Node::Bool(true)
                    } else {
                        Node::List(vec![map(&[
                            ("secretName", s("tls-secret")),
                            ("hosts", Node::List(vec![s("chart.example.com")])),
                        ])])
                    };
                    out.push(set(child(&p, "tls"), v, "ingress tls"));
                }
                if has("ingressClassName") {
                    out.push(set(
                        child(&p, "ingressClassName"),
                        s("nginx"),
                        "ingress class",
                    ));
                }
            }
            "persistence" if matches!(cur, Node::Map(_)) => {
                out.push(set(
                    child(&p, "enabled"),
                    Node::Bool(rng.chance(1, 2)),
                    "persistence",
                ));
                if has("size") {
                    out.push(set(
                        child(&p, "size"),
                        rng.pick(&[s("1Gi"), s(""), Node::Num("10".into())]).clone(),
                        "persistence size",
                    ));
                }
                if has("storageClass") {
                    out.push(set(
                        child(&p, "storageClass"),
                        rng.pick(&[s("-"), s(""), s("fast")]).clone(),
                        "storage class",
                    ));
                }
            }
            "autoscaling" if matches!(cur, Node::Map(_)) => {
                out.push(set(
                    child(&p, "enabled"),
                    Node::Bool(true),
                    "autoscaling on",
                ));
                for (k, v) in [
                    ("minReplicas", "2"),
                    ("maxReplicas", "5"),
                    ("targetCPUUtilizationPercentage", "80"),
                    ("targetCPU", "80"),
                ] {
                    if has(k) {
                        out.push(set(child(&p, k), Node::Num(v.into()), "autoscaling"));
                    }
                }
            }
            "serviceMonitor" if matches!(cur, Node::Map(_)) => {
                out.push(set(
                    child(&p, "enabled"),
                    Node::Bool(true),
                    "serviceMonitor on",
                ));
                if has("interval") {
                    out.push(set(
                        child(&p, "interval"),
                        s("15s"),
                        "serviceMonitor interval",
                    ));
                }
                if has("labels") || has("additionalLabels") {
                    let k = if has("labels") {
                        "labels"
                    } else {
                        "additionalLabels"
                    };
                    out.push(set(
                        child(&p, k),
                        map(&[("release", s("prom"))]),
                        "serviceMonitor labels",
                    ));
                }
            }
            "global" => {
                let (k, v) = rng
                    .pick(&[
                        ("imageRegistry", s("registry.example.com")),
                        ("imagePullSecrets", Node::List(vec![s("regcred")])),
                        (
                            "imagePullSecrets",
                            Node::List(vec![map(&[("name", s("regcred"))])]),
                        ),
                        ("storageClass", s("fast")),
                        ("defaultStorageClass", s("fast")),
                        (
                            "security",
                            map(&[("allowInsecureImages", Node::Bool(true))]),
                        ),
                        (
                            "compatibility",
                            map(&[("openshift", map(&[("adaptSecurityContext", s("force"))]))]),
                        ),
                        ("labels", map(&[("team", s("x"))])),
                        ("commonLabels", map(&[("team", s("x"))])),
                    ])
                    .clone();
                out.push(set(child(&p, k), v, "global"));
            }
            "annotations" | "podAnnotations" | "nodeSelector" => {
                let v = rng
                    .pick(&[
                        map(&[("example.com/key", s("value"))]),
                        map(&[("a", Node::Num("1".into())), ("b", Node::Bool(true))]),
                        Node::Map(vec![]),
                    ])
                    .clone();
                out.push(set(p, v, "string map"));
            }
            "tolerations" => {
                out.push(set(
                    p,
                    Node::List(vec![map(&[
                        ("key", s("k")),
                        ("operator", s("Exists")),
                        ("effect", s("NoSchedule")),
                    ])]),
                    "tolerations",
                ));
            }
            "extraEnv" => {
                out.push(set(
                    p,
                    Node::List(vec![map(&[("name", s("FOO")), ("value", s("1"))])]),
                    "extra env",
                ));
            }
            "image" if matches!(cur, Node::Map(_)) => {
                for (k, v) in [
                    ("registry", s("registry.example.com")),
                    ("repository", s("org/img")),
                    (
                        "digest",
                        s(
                            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                        ),
                    ),
                    ("pullPolicy", s("Always")),
                ] {
                    if has(k) && rng.chance(1, 2) {
                        out.push(set(child(&p, k), v, "image"));
                    }
                }
            }
            "service" if matches!(cur, Node::Map(_)) => {
                if has("type") {
                    out.push(set(
                        child(&p, "type"),
                        rng.pick(&[s("NodePort"), s("LoadBalancer"), s("ClusterIP")])
                            .clone(),
                        "service type",
                    ));
                }
                if has("port") {
                    out.push(set(
                        child(&p, "port"),
                        rng.pick(&[Node::Num("8080".into()), s("8080")]).clone(),
                        "service port",
                    ));
                }
            }
            _ => {}
        }
    }
}

/// User values for a case: the corpus values with each mutated subtree
/// overridden. A mutation inside a list overrides the whole list (Helm does
/// not merge lists); deleting a map key is a `null` override.
fn build_values(corpus_values: &Node, effective_base: &Node, muts: &[Mutation]) -> Node {
    let mut eff = effective_base.clone();
    let mut prefixes: Vec<Path> = Vec::new();
    for m in muts {
        let list_at = m.path.iter().position(|s| matches!(s, Seg::Idx(_)));
        let prefix: Path = m.path[..list_at.unwrap_or(m.path.len())].to_vec();
        let applied = match &m.op {
            Op::Set(v) => eff.set(&m.path, v.clone(), false),
            Op::Delete if list_at.is_some() => eff.remove(&m.path),
            Op::Delete => eff.set(&m.path, Node::Null, false),
        };
        if applied && !prefix.is_empty() {
            prefixes.push(prefix);
        }
    }
    let mut out = match corpus_values {
        Node::Map(_) => corpus_values.clone(),
        _ => Node::Map(Vec::new()),
    };
    for p in &prefixes {
        if prefixes
            .iter()
            .any(|q| q.len() < p.len() && p.starts_with(q))
        {
            continue; // covered by an ancestor override
        }
        let v = eff.get(p).cloned().unwrap_or(Node::Null);
        out.set(p, v, true);
    }
    out
}

// ---------------------------------------------------------------------------
// Driver

struct ChartCtx {
    entry: ChartEntry,
    tgz: Vec<u8>,
    corpus_values: Node,
    base: Node,
}

fn opts() -> Opts {
    Opts {
        fixed_time: true,
        ..Opts::default()
    }
}

fn prepare(o: &mut Oracle, c: &ChartEntry) -> Result<ChartCtx, String> {
    let values = c.values_yaml();
    let corpus_values = Node::from_json(&o.chart_values(None, &values)?);
    let base = Node::from_json(&o.chart_values(Some(&c.tgz_path()), &values)?);
    Ok(ChartCtx {
        entry: c.clone(),
        tgz: c.tgz_bytes(),
        corpus_values,
        base,
    })
}

fn check(o: &mut Oracle, cx: &ChartCtx, values_yaml: &str) -> (OracleResult, Option<Mismatch>) {
    let path = cx.entry.tgz_path();
    let helm = o.render_chart(&path, values_yaml, &opts());
    let ours = rust::render_tgz(&cx.tgz, values_yaml, &opts());
    let m = compare_render(
        &helm,
        || Some(o.render_chart(&path, values_yaml, &opts())),
        &ours,
    );
    (helm, m)
}

/// Greedily drops mutations while the same kind of mismatch persists.
fn minimize(
    o: &mut Oracle,
    cx: &ChartCtx,
    muts: Vec<Mutation>,
    kind: Kind,
) -> (Vec<Mutation>, String, Mismatch) {
    let mut cur = muts;
    let mut budget = 40;
    'outer: loop {
        for i in 0..cur.len() {
            if budget == 0 {
                break 'outer;
            }
            budget -= 1;
            let mut cand = cur.clone();
            cand.remove(i);
            let y = build_values(&cx.corpus_values, &cx.base, &cand).to_yaml();
            if let (_, Some(m)) = check(o, cx, &y)
                && m.kind == kind
            {
                cur = cand;
                continue 'outer;
            }
        }
        break;
    }
    let y = build_values(&cx.corpus_values, &cx.base, &cur).to_yaml();
    let m = check(o, cx, &y).1.unwrap_or(Mismatch {
        kind,
        report: "(mismatch vanished while minimizing: flaky?)".into(),
    });
    (cur, y, m)
}

#[derive(Default)]
struct Stats {
    cases: usize,
    helm_ok: usize,
    helm_err: usize,
}

fn fuzz_chart(
    o: &mut Oracle,
    c: &ChartEntry,
    seed: u64,
    iters: usize,
    stats: &Mutex<Stats>,
) -> Vec<String> {
    let cx = match prepare(o, c) {
        Ok(cx) => cx,
        Err(e) => {
            return vec![format!(
                "=== {}: cannot get default values from the oracle: {e}",
                c.name
            )];
        }
    };
    // Baseline first: if the unmutated chart already differs, fuzzing it is
    // noise (charts_diff reports the details).
    let baseline = build_values(&cx.corpus_values, &cx.base, &[]).to_yaml();
    if let (_, Some(m)) = check(o, &cx, &baseline) {
        return vec![format!(
            "=== {} {}: baseline (no mutations) already differs ({:?}); not fuzzed. See tests/charts_diff.rs.\n{}",
            c.name,
            c.version,
            m.kind,
            first_lines(&m.report, 6)
        )];
    }
    let mut paths = Vec::new();
    collect_paths(&cx.base, &mut Vec::new(), &mut paths);
    let fz = Fuzzer {
        base: &cx.base,
        paths,
    };
    let mut rng = Rng::fork(seed, &c.name);
    let mut failures = Vec::new();
    for iter in 0..iters {
        let muts = fz.generate(&mut rng);
        let values = build_values(&cx.corpus_values, &cx.base, &muts).to_yaml();
        let (helm, m) = check(o, &cx, &values);
        {
            let mut s = stats.lock().unwrap();
            s.cases += 1;
            if matches!(helm, OracleResult::Ok(_)) {
                s.helm_ok += 1
            } else {
                s.helm_err += 1
            }
        }
        let Some(m) = m else { continue };
        let (min, yaml, m) = minimize(o, &cx, muts, m.kind);
        let list: Vec<String> = min.iter().map(|m| format!("  {m}")).collect();
        failures.push(format!(
            "=== {} {} iteration {iter} (HELM_RS_FUZZ_SEED={seed} HELM_RS_CHARTS={}): {:?}\nminimized mutations:\n{}\n{}\nvalues:\n{}",
            c.name,
            c.version,
            c.name,
            m.kind,
            list.join("\n"),
            m.report,
            first_lines(&yaml, 200)
        ));
        if failures.len() >= 3 {
            break; // enough for one chart
        }
    }
    failures
}

fn first_lines(s: &str, n: usize) -> String {
    let mut v: Vec<&str> = s.lines().take(n + 1).collect();
    if v.len() > n {
        v.truncate(n);
        v.push("...");
    }
    v.join("\n")
}

fn threads() -> usize {
    std::env::var("HELM_RS_FUZZ_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            (std::thread::available_parallelism().map_or(2, |n| n.get()) / 2).clamp(1, 8)
        })
}

#[test]
fn values_fuzz_matches_helm() {
    let charts = corpus::available();
    if charts.is_empty() || Oracle::shared().is_none() {
        return;
    }
    let seed = fuzz_seed("values_fuzz_matches_helm");
    let iters = fuzz_iters(16);
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let stats = Mutex::new(Stats::default());
    std::thread::scope(|sc| {
        for _ in 0..threads().min(charts.len()) {
            sc.spawn(|| {
                let mut o = Oracle::spawn().expect("spawn oracle");
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(c) = charts.get(i) else { break };
                    let f = fuzz_chart(&mut o, c, seed, iters, &stats);
                    eprintln!(
                        "  {:<30} {}",
                        c.name,
                        if f.is_empty() { "ok" } else { "FAIL" }
                    );
                    failures.lock().unwrap().extend(f);
                }
            });
        }
    });
    let s = stats.into_inner().unwrap();
    eprintln!(
        "values fuzz: {} mutated cases ({} render in helm, {} fail in helm), seed {seed}",
        s.cases, s.helm_ok, s.helm_err
    );
    let failures = failures.into_inner().unwrap();
    if !failures.is_empty() {
        for f in &failures {
            eprintln!("\n{f}");
        }
        panic!(
            "values fuzz: {} failures (HELM_RS_FUZZ_SEED={seed})",
            failures.len()
        );
    }
}

/// Exercises the generator against the oracle alone (no helm-rs): the
/// generated values files must parse in Helm, be reproducible from the seed,
/// and a reasonable share must still render (otherwise the fuzzer only tests
/// error paths).
#[test]
fn values_fuzz_generator_produces_useful_cases() {
    let charts = corpus::available();
    let Some(mut o) = common::oracle() else {
        return;
    };
    let picks: Vec<&ChartEntry> = charts
        .iter()
        .filter(|c| {
            ["ingress-nginx", "bitnami-redis", "grafana", "cert-manager"].contains(&c.name.as_str())
        })
        .collect();
    let (mut ok, mut total) = (0, 0);
    for c in picks {
        let cx = prepare(&mut o, c).unwrap();
        let mut paths = Vec::new();
        collect_paths(&cx.base, &mut Vec::new(), &mut paths);
        let fz = Fuzzer {
            base: &cx.base,
            paths,
        };
        let mut r1 = Rng::fork(7, &c.name);
        let mut r2 = Rng::fork(7, &c.name);
        for _ in 0..12 {
            let m1 = fz.generate(&mut r1);
            let m2 = fz.generate(&mut r2);
            let y1 = build_values(&cx.corpus_values, &cx.base, &m1).to_yaml();
            assert_eq!(
                y1,
                build_values(&cx.corpus_values, &cx.base, &m2).to_yaml(),
                "not reproducible"
            );
            // Parses as YAML in Helm, unless it deliberately contains a
            // non-finite float (which JSON cannot represent).
            if let Err(e) = o.chart_values(None, &y1) {
                assert!(
                    y1.contains(": .inf")
                        || y1.contains(": -.inf")
                        || y1.contains(": .nan")
                        || y1.contains(".inf\n")
                        || y1.contains(".nan\n"),
                    "{}: generated values do not parse: {e}\n{y1}",
                    c.name
                );
            }
            total += 1;
            if let OracleResult::Ok(_) = o.render_chart(&c.tgz_path(), &y1, &opts()) {
                ok += 1;
            }
        }
    }
    if total > 0 {
        eprintln!("{ok}/{total} fuzzed value sets render in helm");
        assert!(
            ok * 4 >= total,
            "too few fuzzed cases render successfully: {ok}/{total}"
        );
        assert!(
            ok < total,
            "no fuzzed case fails in helm: mutations too timid?"
        );
    }
}

#[test]
fn build_values_semantics() {
    let base = Node::from_json(&serde_json::json!({"a": {"b": 1, "c": [ {"d": true} ]}, "e": "x"}));
    let corpus = Node::from_json(&serde_json::json!({"e": "y"}));
    let k = |s: &str| Seg::Key(s.into());
    let muts = vec![
        Mutation {
            path: vec![k("a"), k("c"), Seg::Idx(0), k("d")],
            op: Op::Set(Node::Bool(false)),
            why: "",
        },
        Mutation {
            path: vec![k("a"), k("b")],
            op: Op::Delete,
            why: "",
        },
    ];
    let v = build_values(&corpus, &base, &muts);
    let expect =
        Node::from_json(&serde_json::json!({"e": "y", "a": {"c": [ {"d": false} ], "b": null}}));
    fn sorted(n: &Node) -> Node {
        match n {
            Node::Map(m) => {
                let mut m: Vec<_> = m.iter().map(|(k, v)| (k.clone(), sorted(v))).collect();
                m.sort_by(|a, b| a.0.cmp(&b.0));
                Node::Map(m)
            }
            Node::List(l) => Node::List(l.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    assert_eq!(sorted(&v), expect, "{}", v.to_yaml());
}
