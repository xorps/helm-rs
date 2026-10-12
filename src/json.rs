//! JSON compatible with Go's `encoding/json` (`toJson`, `fromJson`).

use alloc::string::String;

use crate::value::Value;

/// A JSON syntax error, with Go's message and the 1-based position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// Message as Go's `encoding/json` prints it.
    pub message: String,
    /// Byte offset of the error.
    pub offset: usize,
}

/// `json.Marshal(v)`: sorted keys, HTML-escaped `<`, `>`, `&`, Go float format.
pub fn to_string(v: &Value) -> Result<String, JsonError> {
    let _ = v;
    todo!("json::to_string")
}

/// `json.Unmarshal` into `interface{}`: numbers become `float64`.
pub fn from_str(s: &str) -> Result<Value, JsonError> {
    let _ = s;
    todo!("json::from_str")
}
