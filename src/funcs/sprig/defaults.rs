//! `defaults.go`: default/empty/coalesce/all/any/ternary, JSON and deepCopy.

use alloc::string::String;
use alloc::vec::Vec;

use super::{arg, barg, empty, sarg};
use crate::func::{FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::value::Value;

const A: Ty = Ty::Any;

pub(super) fn register(f: &mut FuncMap) {
    f.add("default", Sig::variadic(&[A], A), |a| {
        let given = &a[1.min(a.len())..];
        if given.is_empty() || empty(&given[0]) {
            return Ok(arg(a, 0).clone());
        }
        Ok(given[0].clone())
    });
    f.add("empty", Sig::new(&[A]), |a| Ok(Value::Bool(empty(arg(a, 0)))));
    f.add("coalesce", Sig::variadic(&[], A), |a| Ok(a.iter().find(|v| !empty(v)).cloned().unwrap_or(Value::Nil)));
    f.add("all", Sig::variadic(&[], A), |a| Ok(Value::Bool(a.iter().all(|v| !empty(v)))));
    f.add("any", Sig::variadic(&[], A), |a| Ok(Value::Bool(a.iter().any(|v| !empty(v)))));
    f.add("ternary", Sig::new(&[A, A, Ty::Bool]), |a| Ok(if barg(a, 2) { arg(a, 0).clone() } else { arg(a, 1).clone() }));
    f.add("fromJson", Sig::new(&[Ty::String]), |a| Ok(crate::json::from_str(sarg(a, 0)).unwrap_or(Value::Nil)));
    f.add("mustFromJson", Sig::new(&[Ty::String]), |a| crate::json::from_str(sarg(a, 0)).map_err(|e| FuncError::new(e.message)));
    f.add("toJson", Sig::new(&[A]), |a| Ok(Value::string(crate::json::to_string(arg(a, 0)).unwrap_or_default())));
    f.add("mustToJson", Sig::new(&[A]), |a| to_json(arg(a, 0)).map(Value::string));
    f.add("toPrettyJson", Sig::new(&[A]), |a| Ok(Value::string(to_json(arg(a, 0)).map(|s| json_indent(&s)).unwrap_or_default())));
    f.add("mustToPrettyJson", Sig::new(&[A]), |a| to_json(arg(a, 0)).map(|s| Value::string(json_indent(&s))));
    f.add("toRawJson", Sig::new(&[A]), |a| to_json(arg(a, 0)).map(|s| Value::string(unescape_html(&s))));
    f.add("mustToRawJson", Sig::new(&[A]), |a| to_json(arg(a, 0)).map(|s| Value::string(unescape_html(&s))));
    f.add("deepCopy", Sig::new(&[A]), |a| {
        deep_copy(arg(a, 0), 0).map_err(|e| if e.message.starts_with("reflect:") { e } else { FuncError::new(alloc::format!("deepCopy error: {}", e.message)) })
    });
    f.add("mustDeepCopy", Sig::new(&[A]), |a| deep_copy(arg(a, 0), 0));
}

fn to_json(v: &Value) -> Result<String, FuncError> {
    crate::json::to_string(v).map_err(|e| FuncError::new(e.message))
}

/// `copystructure.Copy`.
fn deep_copy(v: &Value, depth: usize) -> FuncResult {
    if depth > 1000 {
        return Err(FuncError::new("copystructure: structure too deep (cyclic?)"));
    }
    Ok(match v {
        Value::Nil if depth == 0 => {
            return Err(FuncError::new("reflect: call of reflect.Value.Type on zero Value"));
        }
        // copystructure copies exported struct fields only; a semver
        // version has none, so its copy is the zero version.
        Value::Object(o) if o.as_any().is::<crate::semver::VersionObject>() => {
            let by_value = o.as_any().downcast_ref::<crate::semver::VersionObject>().is_some_and(|x| x.by_value);
            let zero = crate::semver::Version {
                major: 0,
                minor: 0,
                patch: 0,
                pre: String::new(),
                metadata: String::new(),
                original: String::new(),
            };
            Value::object(crate::semver::VersionObject { version: zero, by_value })
        }
        Value::Map(m) => {
            let entries: Vec<(String, Value)> = m.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let mut out = crate::value::Map::new();
            for (k, x) in entries {
                out.insert(k, deep_copy(&x, depth + 1)?);
            }
            Value::map(out)
        }
        Value::List(l) => Value::list(l.iter().map(|x| deep_copy(x, depth + 1)).collect::<Result<Vec<_>, _>>()?),
        Value::Bytes(b) => Value::Bytes(b.to_vec().into()),
        x => x.clone(),
    })
}

/// Undoes `encoding/json`'s HTML escaping (`SetEscapeHTML(false)`):
/// `<`, `>`, `&` back to `<`, `>`, `&` (only real escapes,
/// i.e. preceded by an even number of backslashes).
pub(crate) fn unescape_html(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            if b[i + 1] == b'u' && i + 6 <= b.len() {
                match &s[i + 2..i + 6] {
                    "003c" => {
                        out.push('<');
                        i += 6;
                        continue;
                    }
                    "003e" => {
                        out.push('>');
                        i += 6;
                        continue;
                    }
                    "0026" => {
                        out.push('&');
                        i += 6;
                        continue;
                    }
                    _ => {}
                }
            }
            // Copy the escape pair verbatim.
            out.push_str(&s[i..i + 2]);
            i += 2;
            continue;
        }
        let c = s[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// `json.Indent(dst, src, "", "  ")` on compact JSON.
pub(crate) fn json_indent(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(src.len() * 2);
    let mut depth = 0usize;
    let mut need_indent = false;
    let mut in_string = false;
    let mut escaped = false;
    let newline = |out: &mut Vec<u8>, depth: usize| {
        out.push(b'\n');
        for _ in 0..depth {
            out.extend_from_slice(b"  ");
        }
    };
    for &c in b {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            continue;
        }
        if need_indent && c != b']' && c != b'}' {
            need_indent = false;
            depth += 1;
            newline(&mut out, depth);
        }
        match c {
            b'"' => {
                in_string = true;
                out.push(c);
            }
            b'{' | b'[' => {
                need_indent = true;
                out.push(c);
            }
            b',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            b':' => {
                out.push(c);
                out.push(b' ');
            }
            b'}' | b']' => {
                if need_indent {
                    need_indent = false;
                } else {
                    depth = depth.saturating_sub(1);
                    newline(&mut out, depth);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    crate::gofmt::strconv::lossy_vec(out)
}
