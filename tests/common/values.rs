//! Converting test fixtures to `Value`s.
#![allow(dead_code)]

use helm_rs::{Map, Value};
use serde_json::Value as Json;

/// JSON → Value with Helm's number semantics (all numbers are float64).
pub fn json_to_value(j: &Json) -> Value {
    match j {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Float(n.as_f64().unwrap()),
        Json::String(s) => Value::str(s),
        Json::Array(a) => Value::list(a.iter().map(json_to_value).collect()),
        Json::Object(o) => {
            let mut m = Map::new();
            for (k, v) in o {
                // Helm's coalescing drops null entries from chart defaults.
                if !v.is_null() {
                    m.insert(k.clone(), json_to_value(v));
                }
            }
            Value::map(m)
        }
    }
}
