//! Helm's encoding functions (`pkg/engine/funcs.go`): `toYaml`, `fromYaml`,
//! `toJson`, `fromJson`, `toToml`, `fromToml` and their variants.

use alloc::string::String;
use alloc::vec;

use crate::func::{FuncError, FuncMap, Sig, Ty};
use crate::value::Value;
use crate::yaml::{GO_MAP_TYPE, Target};
use crate::{json, toml, yaml};

const ANY: Sig = Sig::new(&[Ty::Any]);
const STRING: Sig = Sig::new(&[Ty::String]);

fn trim_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
    }
    s
}

fn arg_str(args: &[Value]) -> &str {
    args.first().and_then(Value::as_str).unwrap_or("")
}

fn arg(args: &[Value]) -> Value {
    args.first().cloned().unwrap_or(Value::Nil)
}

/// `toYaml`: errors yield the empty string.
pub fn to_yaml(v: &Value) -> String {
    yaml::to_string(v).map(trim_newline).unwrap_or_default()
}

/// `mustToYaml`.
pub fn must_to_yaml(v: &Value) -> Result<String, FuncError> {
    yaml::to_string(v).map(trim_newline).map_err(|e| FuncError::new(e.message))
}

/// `toYamlPretty`: errors yield the empty string.
pub fn to_yaml_pretty(v: &Value) -> String {
    yaml::to_string_pretty(v).map(trim_newline).unwrap_or_default()
}

/// `fromYaml`: a map; on error the map holds the message under `Error`.
pub fn from_yaml(s: &str) -> Value {
    match yaml::unmarshal(s, Target::Map(GO_MAP_TYPE)) {
        Ok(v) => v,
        Err(e) => crate::map! { "Error" => e.message },
    }
}

/// `fromYamlArray`: a list; on error a one-element list with the message.
pub fn from_yaml_array(s: &str) -> Value {
    match yaml::unmarshal(s, Target::List) {
        Ok(v) => v,
        Err(e) => Value::list(vec![Value::string(e.message)]),
    }
}

/// `toJson`: errors yield the empty string.
pub fn to_json(v: &Value) -> String {
    json::to_string(v).unwrap_or_default()
}

/// `mustToJson`.
pub fn must_to_json(v: &Value) -> Result<String, FuncError> {
    json::to_string(v).map_err(|e| FuncError::new(e.message))
}

/// `fromJson`: a map; on error the message is stored under `Error` (next to
/// whatever was decoded before the error, as in Go).
pub fn from_json(s: &str) -> Value {
    let (v, err) = json::unmarshal(s, Target::Map(GO_MAP_TYPE));
    if let Some(e) = err {
        if let Value::Map(m) = &v {
            m.insert("Error", Value::string(e.message));
            return v;
        }
        return crate::map! { "Error" => e.message };
    }
    v
}

/// `fromJsonArray`: a list; on error a one-element list with the message.
pub fn from_json_array(s: &str) -> Value {
    match json::unmarshal(s, Target::List) {
        (v, None) => v,
        (_, Some(e)) => Value::list(vec![Value::string(e.message)]),
    }
}

/// `toToml`: errors yield the error message.
pub fn to_toml(v: &Value) -> String {
    toml::to_string(v).unwrap_or_else(|e| e)
}

/// `mustToToml`.
pub fn must_to_toml(v: &Value) -> Result<String, FuncError> {
    toml::to_string(v).map_err(FuncError::new)
}

/// `fromToml`: a map; on error the map holds the message under `Error`.
pub fn from_toml(s: &str) -> Value {
    match toml::from_str(s) {
        Ok(v) => v,
        Err(e) => crate::map! { "Error" => e },
    }
}

/// Registers Helm's encoding functions.
pub fn register(f: &mut FuncMap) {
    f.add("toYaml", ANY, |a| Ok(Value::string(to_yaml(&arg(a)))));
    f.add("mustToYaml", ANY, |a| must_to_yaml(&arg(a)).map(Value::string));
    f.add("toYamlPretty", ANY, |a| Ok(Value::string(to_yaml_pretty(&arg(a)))));
    f.add("fromYaml", STRING, |a| Ok(from_yaml(arg_str(a))));
    f.add("fromYamlArray", STRING, |a| Ok(from_yaml_array(arg_str(a))));
    f.add("toJson", ANY, |a| Ok(Value::string(to_json(&arg(a)))));
    f.add("mustToJson", ANY, |a| must_to_json(&arg(a)).map(Value::string));
    f.add("fromJson", STRING, |a| Ok(from_json(arg_str(a))));
    f.add("fromJsonArray", STRING, |a| Ok(from_json_array(arg_str(a))));
    f.add("toToml", ANY, |a| Ok(Value::string(to_toml(&arg(a)))));
    f.add("mustToToml", ANY, |a| must_to_toml(&arg(a)).map(Value::string));
    f.add("fromToml", STRING, |a| Ok(from_toml(arg_str(a))));
}
