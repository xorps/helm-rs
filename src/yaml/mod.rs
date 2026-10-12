//! YAML compatible with `sigs.k8s.io/yaml` as used by Helm.
//!
//! Parsing follows `yaml.Unmarshal` into `interface{}`: YAML is converted to
//! JSON first, so all numbers become `float64` and all keys strings.
//! Emission follows `yaml.Marshal` (`toYaml`) and the `goyaml.v3` encoder with
//! indent 2 (`toYamlPretty`).
//!
//! The scanner, parser and emitter are ports of the libyaml-derived code in
//! `go.yaml.in/yaml/v2` (and `v3` for the pretty encoder), so accepted input,
//! output formatting and error messages match Helm exactly. Errors also carry
//! the precise 1-based line and column of the problem (Go's messages only
//! give a line, and for parser errors it is off by one).
//!
//! Differences from Go, by design:
//! - nesting deeper than 256 levels is rejected with an `exceeded max depth`
//!   error (Go accepts up to 10000 levels);
//! - Go picks the result at random where its own behavior depends on map
//!   iteration order (several unsupported map keys, keys colliding after
//!   conversion to strings, key sets that goyaml's "natural" ordering cannot
//!   sort consistently); we return one of Go's possible results.

use alloc::format;
use alloc::string::String;

use crate::value::Value;

mod decode;
mod emitter;
mod encode;
pub(crate) mod isprint;
mod parser;
mod resolve;
mod scanner;
pub(crate) mod strconv;
mod unicode_tables;

pub(crate) use resolve::base64_encode;

/// Maximum nesting depth of collections accepted by the parsers and
/// encoders in this crate (Go allows 10000; we stop earlier to stay well
/// within the stack).
pub(crate) const MAX_DEPTH: usize = 256;

/// A YAML error with Go's message and the 1-based position (when known).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YamlError {
    /// Message as `sigs.k8s.io/yaml` prints it, e.g.
    /// `error converting YAML to JSON: yaml: line 3: mapping values are not allowed in this context`.
    pub message: String,
    /// 1-based line, if known.
    pub line: Option<u32>,
    /// 1-based column, if known.
    pub column: Option<u32>,
}

impl core::fmt::Display for YamlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

impl core::error::Error for YamlError {}

/// The Go type `yaml.Unmarshal` (or `json.Unmarshal`) decodes into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    /// `interface{}`.
    Any,
    /// A map type; the string is the Go type name used in error messages:
    /// [`GO_MAP_TYPE`] for `fromYaml`/`fromJson` and values files inside a
    /// chart, `"common.Values"` for user-supplied values (`helm -f`). A
    /// top-level `null` yields an empty map.
    Map(&'a str),
    /// `[]interface {}` (`fromYamlArray`, `fromJsonArray`). A top-level
    /// `null` yields an empty list.
    List,
}

/// The Go type name of `map[string]interface{}`.
pub const GO_MAP_TYPE: &str = "map[string]interface {}";

impl YamlError {
    fn new(message: String, src: &str, err: &decode::DecodeError) -> YamlError {
        let (line, column) = if let Some(off) = err.offset {
            let (l, c) = crate::error::line_col(src, off);
            (Some(l), Some(c))
        } else if let Some(m) = err.mark {
            (Some(m.line as u32 + 1), Some(m.column as u32 + 1))
        } else {
            (None, None)
        };
        YamlError {
            message,
            line,
            column,
        }
    }

    fn plain(message: String) -> YamlError {
        YamlError {
            message,
            line: None,
            column: None,
        }
    }
}

/// `yaml.Unmarshal(data, &v)` with `v interface{}`. Empty input yields `Nil`.
pub fn from_str(s: &str) -> Result<Value, YamlError> {
    unmarshal(s, Target::Any)
}

/// `yaml.Unmarshal(data, &m)` with `m map[string]interface{}` (as used by
/// `fromYaml` and values files). Always returns a [`Value::Map`] on success.
pub fn from_str_map(s: &str) -> Result<Value, YamlError> {
    unmarshal(s, Target::Map(GO_MAP_TYPE))
}

/// `sigs.k8s.io/yaml.Unmarshal(data, &out)` for the given target type.
pub fn unmarshal(s: &str, target: Target<'_>) -> Result<Value, YamlError> {
    unmarshal_impl(s.as_bytes(), s, target)
}

/// [`unmarshal`] on raw bytes, which may not be valid UTF-8 (Go strings can
/// hold any bytes; the YAML reader then reports Go's encoding errors).
pub fn unmarshal_bytes(data: &[u8], target: Target<'_>) -> Result<Value, YamlError> {
    match core::str::from_utf8(data) {
        Ok(s) => unmarshal(s, target),
        Err(_) => {
            // Positions are computed against the lossily decoded text.
            let lossy = alloc::string::String::from_utf8_lossy(data);
            unmarshal_impl(data, &lossy, target)
        }
    }
}

fn unmarshal_impl(data: &[u8], s: &str, target: Target<'_>) -> Result<Value, YamlError> {
    const CONV: &str = "error converting YAML to JSON: ";
    let (nodes, root) = decode::compose(data)
        .map_err(|e| YamlError::new(format!("{CONV}yaml: {}", e.message), s, &e))?;
    let (go, root_mark) = match root {
        Some(r) => {
            let mut d = decode::Decoder::new(&nodes);
            let v = d
                .decode(r)
                .map_err(|e| YamlError::new(format!("{CONV}yaml: {}", e.message), s, &e))?;
            let mark = nodes[r]
                .children
                .first()
                .map_or(nodes[r].mark, |&c| nodes[c].mark);
            (v, mark)
        }
        None => (decode::GoVal::Nil, scanner::Mark::default()),
    };
    drop(nodes);
    let j = decode::to_jsonable(go, root_mark)
        .map_err(|e| YamlError::new(format!("{CONV}{}", e.message), s, &e))?;
    let v = decode::json_roundtrip(j)
        .map_err(|e| YamlError::new(format!("{CONV}{}", e.message), s, &e))?;
    let kind_err = |kind: &str, ty: &str| YamlError {
        message: format!(
            "error unmarshaling JSON: while decoding JSON: json: cannot unmarshal {kind} into Go value of type {ty}"
        ),
        line: Some(root_mark.line as u32 + 1),
        column: Some(root_mark.column as u32 + 1),
    };
    match target {
        Target::Any => Ok(v),
        Target::Map(ty) => match v {
            Value::Map(_) => Ok(v),
            Value::Nil => Ok(Value::empty_map()),
            other => Err(kind_err(json_kind(&other), ty)),
        },
        Target::List => match v {
            Value::List(_) => Ok(v),
            Value::Nil => Ok(Value::list(alloc::vec::Vec::new())),
            other => Err(kind_err(json_kind(&other), "[]interface {}")),
        },
    }
}

/// The JSON kind name `encoding/json` uses in `UnmarshalTypeError`s.
pub(crate) fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Map(_) => "object",
        Value::List(_) => "array",
        Value::String(_) => "string",
        Value::Bool(_) => "bool",
        Value::Nil => "null",
        _ => "number",
    }
}

/// Whether decoding `s` maps two different YAML keys to the same JSON key,
/// in which case Go's result depends on map iteration order. For tests.
#[doc(hidden)]
pub fn has_key_collisions(s: &str) -> bool {
    let Ok((nodes, Some(root))) = decode::compose(s.as_bytes()) else {
        return false;
    };
    let mut d = decode::Decoder::new(&nodes);
    d.decode(root)
        .map(|v| decode::has_key_collisions(&v))
        .unwrap_or(false)
}

/// goyaml's map key ordering (`keyList.Less`) for string keys; `v3` selects
/// the goyaml.v3 variant. Exposed for tests.
#[doc(hidden)]
pub fn key_less(a: &str, b: &str, v3: bool) -> bool {
    encode::key_less(a, b, v3)
}

/// `yaml.Marshal(v)` (Helm's `toYaml`, before Helm trims the final newline):
/// `json.Marshal`, then goyaml v2's `Unmarshal` + `Marshal` of the JSON.
pub fn to_string(v: &Value) -> Result<String, YamlError> {
    let json = crate::json::to_string(v)
        .map_err(|e| YamlError::plain(format!("error marshaling into JSON: {}", e.message)))?;
    json_to_yaml(&json)
}

/// `sigs.k8s.io/yaml.JSONToYAML`.
pub fn json_to_yaml(json: &str) -> Result<String, YamlError> {
    let (nodes, root) = decode::compose(json.as_bytes())
        .map_err(|e| YamlError::new(format!("yaml: {}", e.message), json, &e))?;
    let go = match root {
        Some(r) => {
            let mut d = decode::Decoder::new(&nodes);
            d.decode(r)
                .map_err(|e| YamlError::new(format!("yaml: {}", e.message), json, &e))?
        }
        None => decode::GoVal::Nil,
    };
    encode::marshal_v2(&go).map_err(YamlError::plain)
}

/// goyaml.v3 encoder with `SetIndent(2)` (Helm's `toYamlPretty`, before Helm
/// trims the final newline).
pub fn to_string_pretty(v: &Value) -> Result<String, YamlError> {
    encode::encode_v3(v).map_err(YamlError::plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn rt(s: &str) -> String {
        match from_str(s) {
            Ok(v) => to_string(&v).unwrap_or_else(|e| e.message),
            Err(e) => e.message,
        }
    }

    #[test]
    fn basics() {
        assert_eq!(rt("a: 1\nb: [x, z]\n"), "a: 1\nb:\n- x\n- z\n");
        assert_eq!(rt("a: '1'\n"), "a: \"1\"\n");
        assert_eq!(rt("a: |\n  x\n  y\n"), "a: |\n  x\n  y\n");
        assert_eq!(rt("a: 1e6\n"), "a: 1000000\n");
        assert_eq!(rt("a: 1.5\n"), "a: 1.5\n");
        assert_eq!(rt("a: {}\nb: []\n"), "a: {}\nb: []\n");
        assert_eq!(rt("a: yes\nb: 'yes'\n"), "a: true\nb: \"yes\"\n");
        assert_eq!(
            rt("a: b: c\n"),
            "error converting YAML to JSON: yaml: mapping values are not allowed in this context"
        );
        assert_eq!(rt(""), "null\n");
    }

    #[test]
    fn pretty() {
        let v = from_str("a: 1000000\nb:\n- x\n- {c: d}\n").unwrap();
        assert_eq!(
            to_string_pretty(&v).unwrap(),
            "a: 1e+06\nb:\n  - x\n  - c: d\n"
        );
    }

    #[test]
    fn error_positions() {
        let e = from_str("a: 1\nb: c: d\n").unwrap_err();
        assert_eq!(e.line, Some(2));
        assert_eq!(e.column, Some(5));
        let e = from_str("a: 1\n\u{1}").unwrap_err();
        assert_eq!(
            e.message,
            "error converting YAML to JSON: yaml: control characters are not allowed"
        );
        assert_eq!((e.line, e.column), (Some(2), Some(1)));
        let _ = e.to_string();
    }
}
