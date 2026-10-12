//! YAML compatible with `sigs.k8s.io/yaml` as used by Helm.
//!
//! Parsing follows `yaml.Unmarshal` into `interface{}`: YAML is converted to
//! JSON first, so all numbers become `float64` and all keys strings.
//! Emission follows `yaml.Marshal` (`toYaml`) and the `goyaml.v3` encoder with
//! indent 2 (`toYamlPretty`).

use alloc::string::String;

use crate::value::Value;

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

/// `yaml.Unmarshal(data, &v)` with `v interface{}`. Empty input yields `Nil`.
pub fn from_str(s: &str) -> Result<Value, YamlError> {
    let _ = s;
    todo!("yaml::from_str")
}

/// `yaml.Marshal(v)` (Helm's `toYaml`, before Helm trims the final newline).
pub fn to_string(v: &Value) -> Result<String, YamlError> {
    let _ = v;
    todo!("yaml::to_string")
}

/// goyaml.v3 encoder with `SetIndent(2)` (Helm's `toYamlPretty`).
pub fn to_string_pretty(v: &Value) -> Result<String, YamlError> {
    let _ = v;
    todo!("yaml::to_string_pretty")
}
