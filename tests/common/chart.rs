//! Helpers for chart tests: JSON <-> `Value` conversion, a JSON-only "YAML"
//! parser (JSON is YAML, so fixtures written in JSON load with both Helm and
//! the Rust loader even before the YAML parser is available), tar/gzip
//! builders and chart-level oracle requests.

use std::collections::BTreeMap;

use helm_rs::value::{Map, Value};
use helm_rs::yaml::YamlError;
use serde_json::{Value as Json, json};

use super::Oracle;

pub fn json_to_value(j: &Json) -> Value {
    match j {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Float(n.as_f64().unwrap()),
        Json::String(s) => Value::str(s),
        Json::Array(a) => Value::list(a.iter().map(json_to_value).collect()),
        Json::Object(o) => {
            let m: Map = o.iter().map(|(k, v)| (k.clone(), json_to_value(v))).collect();
            Value::map(m)
        }
    }
}

/// Converts a `Value` to JSON like Go's `encoding/json` would see it.
pub fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Nil => Json::Null,
        Value::Bool(b) => json!(b),
        Value::Int(i) | Value::Int64(i) => json!(i),
        Value::Uint64(u) => json!(u),
        Value::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                json!(*f as i64)
            } else {
                json!(f)
            }
        }
        Value::String(s) => json!(&**s),
        Value::Bytes(b) => json!(String::from_utf8_lossy(b)),
        Value::List(l) => Json::Array(l.iter().map(value_to_json).collect()),
        Value::Map(m) => Json::Object(m.borrow().iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect()),
        Value::Object(o) => value_to_json(&o.to_data()),
    }
}

/// Normalizes oracle JSON numbers (floats that are integral become ints).
pub fn norm(j: &Json) -> Json {
    match j {
        Json::Number(n) => {
            let f = n.as_f64().unwrap();
            if f.fract() == 0.0 && f.abs() < 1e15 { json!(f as i64) } else { j.clone() }
        }
        Json::Array(a) => Json::Array(a.iter().map(norm).collect()),
        Json::Object(o) => Json::Object(o.iter().map(|(k, v)| (k.clone(), norm(v))).collect()),
        _ => j.clone(),
    }
}

/// "YAML" parser accepting JSON only (empty input is null).
pub fn json_yaml(s: &str) -> Result<Value, YamlError> {
    if s.trim().is_empty() {
        return Ok(Value::Nil);
    }
    serde_json::from_str::<Json>(s)
        .map(|j| json_to_value(&j))
        .map_err(|e| YamlError { message: format!("json fixture: {e}"), line: Some(e.line() as u32), column: Some(e.column() as u32) })
}

/// A tar archive (USTAR) from entries; `None` data means a directory.
pub fn tar(entries: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in entries {
        let mut b = [0u8; 512];
        b[..name.len()].copy_from_slice(name.as_bytes());
        b[100..107].copy_from_slice(if data.is_some() { b"0000644" } else { b"0000755" });
        b[108..115].copy_from_slice(b"0000000");
        b[116..123].copy_from_slice(b"0000000");
        let size = data.map_or(0, |d| d.len());
        b[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        b[136..147].copy_from_slice(b"00000000000");
        b[156] = if data.is_some() { b'0' } else { b'5' };
        b[257..263].copy_from_slice(b"ustar\0");
        b[263..265].copy_from_slice(b"00");
        b[148..156].copy_from_slice(b"        ");
        let sum: u32 = b.iter().map(|c| u32::from(*c)).sum();
        b[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&b);
        if let Some(d) = data {
            out.extend_from_slice(d);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
    }
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// gzip with stored deflate blocks.
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(65535).collect() };
    for (i, c) in chunks.iter().enumerate() {
        out.push(u8::from(i + 1 == chunks.len()));
        out.extend_from_slice(&(c.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

pub fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Oracle response for chart ops: `Ok(json or output)` / `Err(message)`.
pub fn oracle_op(req: Json) -> Option<Result<Json, String>> {
    let oracle = Oracle::shared()?;
    let mut o = oracle.lock().unwrap_or_else(|e| e.into_inner());
    let resp = o.request(&req);
    if let Some(e) = resp.get("error").and_then(Json::as_str) {
        return Some(Err(e.to_string()));
    }
    if let Some(out) = resp.get("output") {
        return Some(Ok(out.clone()));
    }
    Some(Ok(resp.get("json").cloned().unwrap_or(Json::Null)))
}

/// Whether `helm_rs::semver` (implemented separately; chart loading needs it)
/// is available. Tests print a skip message and return early otherwise.
pub fn semver_ready() -> bool {
    static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *READY.get_or_init(|| {
        std::panic::catch_unwind(|| helm_rs::semver::Version::parse("1.0.0").is_ok()).unwrap_or(false)
    });
    if !ok {
        eprintln!("skipping: helm_rs::semver is not implemented yet");
    }
    ok
}

/// Chart files for requests: text files in `files`, binary ones in `files_b64`.
pub fn files_req(op: &str, files: &BTreeMap<String, Vec<u8>>) -> Json {
    let mut text = serde_json::Map::new();
    let mut bin = serde_json::Map::new();
    for (k, v) in files {
        match std::str::from_utf8(v) {
            Ok(s) => text.insert(k.clone(), json!(s)),
            Err(_) => bin.insert(k.clone(), json!(b64(v))),
        };
    }
    json!({"op": op, "files": text, "files_b64": bin})
}
