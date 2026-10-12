//! Decoding parsed YAML into Go structs the way `sigs.k8s.io/yaml` does:
//! YAML is converted to JSON (numbers/bools become strings where the target
//! field is a string), then `encoding/json` decodes into the struct with
//! case-insensitive field matching. Type errors are reported with the message
//! Go 1.27's `encoding/json` produces, and decoding continues (the first error
//! wins), exactly like `json.Unmarshal`.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::value::Value;

use super::gostr::yaml_number_to_string;

/// Decoder state: current JSON path and the first type error.
#[derive(Default)]
pub(crate) struct Dec {
    path: Vec<String>,
    pub(crate) err: Option<String>,
}

pub(crate) fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Nil => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) | Value::Int64(_) | Value::Uint64(_) | Value::Float(_) => "number",
        Value::String(_) | Value::Bytes(_) => "string",
        Value::List(_) => "array",
        Value::Map(_) | Value::Object(_) => "object",
    }
}

impl Dec {
    pub(crate) fn new() -> Dec {
        Dec::default()
    }

    /// Records a type error (`json.UnmarshalTypeError`) at the current path.
    pub(crate) fn type_err(&mut self, v: &Value, go_type: &str) {
        if self.err.is_some() {
            return;
        }
        let kind = json_kind(v);
        let msg = if self.path.is_empty() {
            alloc::format!("json: cannot unmarshal {kind} into Go value of type {go_type}")
        } else {
            let field = self.path.join(".");
            let last = self.path.last().map(String::as_str).unwrap_or("");
            let into = if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
                ""
            } else {
                "Go struct field "
            };
            alloc::format!("json: cannot unmarshal {kind} into {into}.{field} of type {go_type}")
        };
        self.err = Some(msg);
    }

    pub(crate) fn push(&mut self, tok: &str) {
        // JSON pointer escaping, as Go reports paths from a JSON pointer.
        self.path.push(tok.replace('~', "~0").replace('/', "~1"));
    }

    pub(crate) fn push_index(&mut self, i: usize) {
        self.path.push(i.to_string());
    }

    pub(crate) fn pop(&mut self) {
        self.path.pop();
    }

    /// Go `string` field (with YAML scalar stringification).
    pub(crate) fn string(&mut self, v: &Value, out: &mut String) {
        match v {
            Value::Nil => {}
            Value::String(s) => *out = s.to_string(),
            Value::Bool(b) => *out = if *b { "true".into() } else { "false".into() },
            Value::Float(f) => *out = yaml_number_to_string(*f),
            Value::Int(i) | Value::Int64(i) => *out = i.to_string(),
            Value::Uint64(u) => *out = u.to_string(),
            _ => self.type_err(v, "string"),
        }
    }

    /// Go `bool` field.
    pub(crate) fn bool(&mut self, v: &Value, out: &mut bool) {
        match v {
            Value::Nil => {}
            Value::Bool(b) => *out = *b,
            _ => self.type_err(v, "bool"),
        }
    }

    /// Go `[]string` field.
    pub(crate) fn strings(&mut self, v: &Value, out: &mut Option<Vec<String>>) {
        match v {
            Value::Nil => *out = None,
            Value::List(l) => {
                let vec = out.get_or_insert_with(Vec::new);
                vec.resize(l.len(), String::new());
                for (i, item) in l.iter().enumerate() {
                    self.push_index(i);
                    self.string(item, &mut vec[i]);
                    self.pop();
                }
            }
            _ => self.type_err(v, "[]string"),
        }
    }

    /// Go `map[string]string` field.
    pub(crate) fn string_map(&mut self, v: &Value, out: &mut Option<BTreeMap<String, String>>) {
        match v {
            Value::Nil => *out = None,
            Value::Map(m) => {
                let map = out.get_or_insert_with(BTreeMap::new);
                for (k, item) in m.borrow().iter() {
                    self.push(k);
                    let mut s = String::new();
                    self.string(item, &mut s);
                    if !matches!(item, Value::List(_) | Value::Map(_)) {
                        map.insert(k.clone(), s);
                    }
                    self.pop();
                }
            }
            _ => self.type_err(v, "map[string]string"),
        }
    }

    /// Go `[]interface{}` field.
    pub(crate) fn any_list(&mut self, v: &Value, out: &mut Option<Vec<Value>>) {
        match v {
            Value::Nil => *out = None,
            Value::List(l) => *out = Some(l.iter().map(Value::deep_clone).collect()),
            _ => self.type_err(v, "[]interface {}"),
        }
    }

    /// Go `[]*T` field where `T` is a struct decoded by `f`.
    pub(crate) fn struct_ptr_list<T: Default>(
        &mut self,
        v: &Value,
        out: &mut Option<Vec<Option<T>>>,
        list_type: &str,
        elem_type: &str,
        mut f: impl FnMut(&mut Dec, &BTreeMap<String, Value>, &mut T),
    ) {
        match v {
            Value::Nil => *out = None,
            Value::List(l) => {
                let vec = out.get_or_insert_with(Vec::new);
                vec.truncate(l.len());
                while vec.len() < l.len() {
                    vec.push(None);
                }
                for (i, item) in l.iter().enumerate() {
                    self.push_index(i);
                    match item {
                        Value::Nil => vec[i] = None,
                        Value::Map(m) => {
                            let slot = vec[i].get_or_insert_with(T::default);
                            f(self, &m.borrow(), slot);
                        }
                        other => self.type_err(other, elem_type),
                    }
                    self.pop();
                }
            }
            _ => self.type_err(v, list_type),
        }
    }
}

/// Finds the struct field a JSON key maps to: an exact match, else the
/// first case-insensitive match (Go `encoding/json` rules).
pub(crate) fn match_field<'a>(fields: &[&'a str], key: &str) -> Option<&'a str> {
    if let Some(f) = fields.iter().find(|f| **f == key) {
        return Some(f);
    }
    fields.iter().copied().find(|f| f.eq_ignore_ascii_case(key) && f.len() == key.len())
}
