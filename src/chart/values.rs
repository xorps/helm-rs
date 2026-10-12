//! Helm's `common.Values` helpers (`Table`, `PathValue`) and values file
//! loading (`loader.LoadValues`, `MergeMaps`).

use alloc::string::String;
use alloc::vec::Vec;

use crate::value::{Map, MapRef, Value};

/// `Values.Table(name)`: walks dotted `name` through nested maps.
pub(crate) fn table(v: &MapRef, name: &str) -> Option<MapRef> {
    let mut cur = v.clone();
    for part in name.split('.') {
        let next = match cur.get(part) {
            Some(Value::Map(m)) => m,
            _ => return None,
        };
        cur = next;
    }
    Some(cur)
}

/// `Values.PathValue(path)`: a non-table value at a dotted path.
pub(crate) fn path_value(v: &MapRef, path: &str) -> Option<Value> {
    if path.is_empty() {
        return None;
    }
    let (parent, key) = match path.rfind('.') {
        None => (None, path),
        Some(i) => (Some(&path[..i]), &path[i + 1..]),
    };
    let t = match parent {
        None => v.clone(),
        Some(p) => table(v, p)?,
    };
    match t.get(key) {
        Some(Value::Map(_)) | None => None,
        Some(val) => Some(val),
    }
}

/// `loader.MergeMaps(a, b)`: new map with `b` merged over `a`, recursing into
/// maps present on both sides.
pub(crate) fn merge_maps(a: &MapRef, b: &MapRef) -> MapRef {
    let mut out: Map = a.borrow().clone();
    let entries: Vec<(String, Value)> = b.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (k, v) in entries {
        if let Value::Map(bm) = &v {
            if let Some(Value::Map(am)) = out.get(&k) {
                let merged = merge_maps(am, bm);
                out.insert(k, Value::Map(merged));
                continue;
            }
        }
        out.insert(k, v);
    }
    MapRef::new(out)
}

/// One document of a values file, with its 1-based first line.
pub(crate) struct Doc {
    pub text: String,
    pub first_line: u32,
}

/// Splits like `k8s.io/apimachinery/pkg/util/yaml.YAMLReader`: a line
/// starting with `---` (only spaces or a comment may follow) ends the
/// current document; when no document is pending the separator line is kept
/// as the start of the next one (as the reader does). Lines are
/// re-terminated with `\n`. Reading is lazy in Helm, so a bad separator is
/// returned after the documents preceding it, with its 1-based line.
pub(crate) fn split_yaml_documents(src: &str) -> (Vec<Doc>, Option<(String, u32)>) {
    let mut docs = Vec::new();
    let mut buf = String::new();
    let mut buf_line = 1u32;
    let mut pos = 0usize;
    let mut line_no = 1u32;
    while pos < src.len() {
        let end = src[pos..].find('\n').map_or(src.len(), |i| pos + i);
        let mut line = &src[pos..end];
        if line.ends_with('\r') {
            line = &line[..line.len() - 1];
        }
        let next = if end < src.len() { end + 1 } else { end };
        if let Some(rest) = line.strip_prefix("---") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                return (docs, Some((alloc::format!("invalid Yaml document separator: {trimmed}"), line_no)));
            }
            if !buf.is_empty() {
                docs.push(Doc { text: core::mem::take(&mut buf), first_line: buf_line });
                pos = next;
                line_no += 1;
                continue;
            }
        }
        if buf.is_empty() {
            buf_line = line_no;
        }
        buf.push_str(line);
        buf.push('\n');
        pos = next;
        line_no += 1;
    }
    if !buf.is_empty() {
        docs.push(Doc { text: buf, first_line: buf_line });
    }
    (docs, None)
}

pub(crate) fn is_table(v: &Value) -> bool {
    matches!(v, Value::Map(_))
}
