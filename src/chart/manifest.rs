//! Manifest splitting and ordering (Helm `pkg/release/v1/util`):
//! `SplitManifests`, `SortManifests` with `InstallOrder`, hooks.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::{Error, ErrorKind};
use crate::value::Value;

use super::decode::{Dec, match_field};
use super::gostr::base;
use super::loader::{YamlParser, location_at};

/// Helm's `releaseutil.InstallOrder`.
pub const INSTALL_ORDER: &[&str] = &[
    "PriorityClass",
    "Namespace",
    "NetworkPolicy",
    "ResourceQuota",
    "LimitRange",
    "PodSecurityPolicy",
    "PodDisruptionBudget",
    "ServiceAccount",
    "Secret",
    "SecretList",
    "ConfigMap",
    "StorageClass",
    "PersistentVolume",
    "PersistentVolumeClaim",
    "CustomResourceDefinition",
    "ClusterRole",
    "ClusterRoleList",
    "ClusterRoleBinding",
    "ClusterRoleBindingList",
    "Role",
    "RoleList",
    "RoleBinding",
    "RoleBindingList",
    "Service",
    "DaemonSet",
    "Pod",
    "ReplicationController",
    "ReplicaSet",
    "Deployment",
    "HorizontalPodAutoscaler",
    "StatefulSet",
    "Job",
    "CronJob",
    "IngressClass",
    "Ingress",
    "APIService",
    "MutatingWebhookConfiguration",
    "ValidatingWebhookConfiguration",
];

const HOOK_EVENTS: &[&str] = &[
    "pre-install",
    "post-install",
    "pre-delete",
    "post-delete",
    "pre-upgrade",
    "post-upgrade",
    "pre-rollback",
    "post-rollback",
    "test",
    "test-success",
];

/// A manifest document from a rendered template.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    /// Template path, e.g. `mychart/templates/svc.yaml`.
    pub name: String,
    pub content: String,
    pub kind: String,
    pub api_version: String,
}

/// A hook document (`helm.sh/hook` annotation).
#[derive(Clone, Debug, PartialEq)]
pub struct Hook {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub manifest: String,
    /// Normalized event names (`test-success` becomes `test`).
    pub events: Vec<String>,
    pub weight: i64,
    pub delete_policies: Vec<String>,
}

/// `SplitManifests`: splits on lines starting with `---`, returning
/// `(byte offset in the input, document)` pairs, in order.
pub fn split_manifests(big: &str) -> Vec<(usize, &str)> {
    let lead = big.len() - big.trim_start().len();
    let s = &big[lead..];
    // (?m)^---[ \t]*
    let mut seps: Vec<(usize, usize)> = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if (i == 0 || b[i - 1] == b'\n') && b[i..].starts_with(b"---") {
            let mut j = i + 3;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            seps.push((i, j));
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    let mut docs = Vec::new();
    let mut start = 0;
    let mut push = |from: usize, to: usize| {
        let d = &s[from..to];
        if d.trim().is_empty() {
            return;
        }
        let t = d.trim_start();
        docs.push((lead + to - t.len(), t));
    };
    for (a, z) in seps {
        push(start, a);
        start = z;
    }
    push(start, s.len());
    docs
}

/// `SimpleHead` of a manifest document.
#[derive(Default)]
struct Head {
    api_version: String,
    kind: String,
    metadata: Option<(String, Option<BTreeMap<String, String>>)>,
}

fn decode_head(d: &mut Dec, v: &Value) -> Head {
    let mut h = Head::default();
    let m = match v {
        Value::Nil => return h,
        Value::Map(m) => m.borrow(),
        other => {
            d.type_err(other, "util.SimpleHead");
            return h;
        }
    };
    for (k, v) in m.iter() {
        let Some(f) = match_field(&["apiVersion", "kind", "metadata"], k) else { continue };
        d.push(k);
        match f {
            "apiVersion" => d.string(v, &mut h.api_version),
            "kind" => d.string(v, &mut h.kind),
            _ => match v {
                Value::Nil => h.metadata = None,
                Value::Map(mm) => {
                    let md = h.metadata.get_or_insert_with(Default::default);
                    for (k2, v2) in mm.borrow().iter() {
                        let Some(f2) = match_field(&["name", "annotations"], k2) else { continue };
                        d.push(k2);
                        if f2 == "name" {
                            d.string(v2, &mut md.0);
                        } else {
                            d.string_map(v2, &mut md.1);
                        }
                        d.pop();
                    }
                }
                other => d.type_err(
                    other,
                    "struct { Name string \"json:\\\"name\\\"\"; Annotations map[string]string \"json:\\\"annotations\\\"\" }",
                ),
            },
        }
        d.pop();
    }
    h
}

fn yaml_parse_error(path: &str, msg: &str, content: &Rc<str>, offset: usize, line: Option<u32>, col: Option<u32>) -> Error {
    let base_line = content[..offset.min(content.len())].matches('\n').count() as u32;
    let loc = location_at(path, content, base_line + line.unwrap_or(1), col.unwrap_or(1));
    Error::new(ErrorKind::Syntax, alloc::format!("YAML parse error on {path}: {msg}")).at(loc)
}

/// `SortManifests(files, nil, InstallOrder)`: hooks and manifests, sorted by
/// kind; partials (`_*`) and empty files are skipped.
pub fn sort_manifests(files: &BTreeMap<String, String>, yaml: YamlParser) -> Result<(Vec<Hook>, Vec<Manifest>), Error> {
    let mut hooks = Vec::new();
    let mut generic = Vec::new();
    for (path, content) in files {
        if base(path).starts_with('_') || content.trim().is_empty() {
            continue;
        }
        let src: Rc<str> = Rc::from(content.as_str());
        for (offset, m) in split_manifests(content) {
            let v = yaml(m).map_err(|e| yaml_parse_error(path, &e.message, &src, offset, e.line, e.column))?;
            let mut d = Dec::new();
            let head = decode_head(&mut d, &v);
            if let Some(e) = d.err {
                return Err(yaml_parse_error(
                    path,
                    &alloc::format!("error unmarshaling JSON: while decoding JSON: {e}"),
                    &src,
                    offset,
                    None,
                    None,
                ));
            }
            let annotations = head.metadata.as_ref().and_then(|m| m.1.as_ref()).filter(|a| !a.is_empty());
            let hook_types = annotations.and_then(|a| a.get("helm.sh/hook"));
            let Some(hook_types) = hook_types else {
                generic.push(Manifest {
                    name: path.clone(),
                    content: m.to_string(),
                    kind: head.kind,
                    api_version: head.api_version,
                });
                continue;
            };
            let a = annotations.cloned().unwrap_or_default();
            let weight = a.get("helm.sh/hook-weight").and_then(|w| parse_go_atoi(w)).unwrap_or(0);
            let mut events = Vec::new();
            let mut unknown = false;
            for t in hook_types.split(',') {
                let t = t.trim().to_lowercase();
                if !HOOK_EVENTS.contains(&t.as_str()) {
                    unknown = true;
                    break;
                }
                events.push(if t == "test-success" { "test".to_string() } else { t });
            }
            if unknown {
                continue;
            }
            let delete_policies = a
                .get("helm.sh/hook-delete-policy")
                .map(|p| p.split(',').map(|s| s.trim().to_lowercase()).collect())
                .unwrap_or_default();
            hooks.push(Hook {
                name: head.metadata.map(|m| m.0).unwrap_or_default(),
                kind: head.kind,
                path: path.clone(),
                manifest: m.to_string(),
                events,
                weight,
                delete_policies,
            });
        }
    }
    hooks.sort_by(|a, b| kind_cmp(&a.kind, &b.kind));
    generic.sort_by(|a, b| kind_cmp(&a.kind, &b.kind));
    Ok((hooks, generic))
}

fn parse_go_atoi(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `lessByKind` as a total order for a stable sort.
fn kind_cmp(a: &str, b: &str) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    let pa = INSTALL_ORDER.iter().position(|k| *k == a);
    let pb = INSTALL_ORDER.iter().position(|k| *k == b);
    match (pa, pb) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split() {
        let s = "\n\n---\na: 1\n---  \nb: 2\n----\nc\n---x\n";
        let docs: Vec<&str> = split_manifests(s).into_iter().map(|d| d.1).collect();
        assert_eq!(docs, ["a: 1\n", "b: 2\n", "-\nc\n", "x\n"]);
        for (off, d) in split_manifests(s) {
            assert_eq!(&s[off..off + d.len()], d);
        }
    }
}
