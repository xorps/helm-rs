//! TOML compatible with `github.com/BurntSushi/toml` (`toToml`, `fromToml`).

use alloc::string::String;

use crate::value::Value;

/// `toml.NewEncoder(...).Encode(v)`.
pub fn to_string(v: &Value) -> Result<String, String> {
    let _ = v;
    todo!("toml::to_string")
}

/// `toml.Unmarshal` into `map[string]interface{}`.
pub fn from_str(s: &str) -> Result<Value, String> {
    let _ = s;
    todo!("toml::from_str")
}
