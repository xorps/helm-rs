//! `.Files` (Helm `engine.files`): a map of file name to contents with
//! `Get`, `GetBytes`, `Glob`, `Lines`, `AsConfig` and `AsSecrets`.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt::Write;

use crate::func::{FuncError, Sig, Ty};
use crate::value::{Kind, Map, Object, Value};

use super::glob::Glob;
use super::gostr::base;

/// `engine.files`.
#[derive(Clone, Debug, Default)]
pub struct Files {
    pub files: BTreeMap<String, Rc<[u8]>>,
}

impl Files {
    pub fn new(files: impl IntoIterator<Item = (String, Rc<[u8]>)>) -> Files {
        Files { files: files.into_iter().collect() }
    }

    /// `files.GetBytes`.
    pub fn get_bytes(&self, name: &str) -> Rc<[u8]> {
        self.files.get(name).cloned().unwrap_or_else(|| Rc::from(&[][..]))
    }

    /// `files.Glob`: invalid patterns match everything (Helm falls back to `**`).
    pub fn glob(&self, pattern: &str) -> Files {
        let g = Glob::compile(pattern).or_else(|_| Glob::compile("**"));
        let files = match g {
            Ok(g) => self.files.iter().filter(|(n, _)| g.matches(n)).map(|(n, d)| (n.clone(), d.clone())).collect(),
            Err(_) => self.files.clone(),
        };
        Files { files }
    }

    /// `files.Lines`.
    pub fn lines(&self, path: &str) -> Vec<String> {
        let Some(data) = self.files.get(path).filter(|d| !d.is_empty()) else {
            return Vec::new();
        };
        let mut s = String::from_utf8_lossy(data).into_owned();
        if s.ends_with('\n') {
            s.pop();
        }
        s.split('\n').map(String::from).collect()
    }

    fn as_yaml(&self, f: impl Fn(&[u8]) -> String) -> String {
        let m: Map = self.files.iter().map(|(k, v)| (String::from(base(k)), Value::string(f(v)))).collect();
        match crate::yaml::to_string(&Value::map(m)) {
            Ok(s) => String::from(s.strip_suffix('\n').unwrap_or(&s)),
            Err(_) => String::new(),
        }
    }

    /// `files.AsConfig`.
    pub fn as_config(&self) -> String {
        self.as_yaml(|v| String::from_utf8_lossy(v).into_owned())
    }

    /// `files.AsSecrets`.
    pub fn as_secrets(&self) -> String {
        self.as_yaml(base64_std)
    }
}

/// Standard base64 with padding.
pub(crate) fn base64_std(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

impl Object for Files {
    fn type_name(&self) -> &str {
        "engine.files"
    }
    fn kind(&self) -> Kind {
        Kind::Map
    }
    /// Map key lookup (`.Files.name` / `index .Files "name"`); a missing key
    /// yields the zero value (empty `[]byte`).
    fn field(&self, name: &str) -> Option<Value> {
        Some(Value::Bytes(self.get_bytes(name)))
    }
    fn method(&self, name: &str) -> Option<Sig> {
        match name {
            "Get" | "GetBytes" | "Glob" | "Lines" => Some(Sig::new(&[Ty::String])),
            "AsConfig" | "AsSecrets" => Some(Sig::new(&[])),
            _ => None,
        }
    }
    fn call_method(&self, name: &str, args: &[Value]) -> Result<Value, FuncError> {
        let arg = || args.first().and_then(Value::as_str).unwrap_or("");
        match name {
            "Get" => Ok(Value::string(String::from_utf8_lossy(&self.get_bytes(arg())).into_owned())),
            "GetBytes" => Ok(Value::Bytes(self.get_bytes(arg()))),
            "Glob" => Ok(Value::object(self.glob(arg()))),
            "Lines" => Ok(Value::list(self.lines(arg()).into_iter().map(Value::string).collect())),
            "AsConfig" => Ok(Value::string(self.as_config())),
            "AsSecrets" => Ok(Value::string(self.as_secrets())),
            _ => Err(FuncError::new(alloc::format!("no method {name}"))),
        }
    }
    /// Go `%v` of `map[string][]byte`.
    fn format(&self, out: &mut String) {
        out.push_str("map[");
        for (i, (k, v)) in self.files.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(k);
            out.push_str(":[");
            for (j, b) in v.iter().enumerate() {
                if j > 0 {
                    out.push(' ');
                }
                let _ = write!(out, "{b}");
            }
            out.push(']');
        }
        out.push(']');
    }
    /// The map of file names to bytes (what `range`, `len` and `toJson` see).
    fn to_data(&self) -> Value {
        Value::map(self.files.iter().map(|(k, v)| (k.clone(), Value::Bytes(v.clone()))).collect())
    }
    fn truthy(&self) -> bool {
        !self.files.is_empty()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_b64() {
        let f = Files::new([("a.txt".into(), Rc::from(&b"x\ny\n"[..])), ("b/c".into(), Rc::from(&b""[..]))]);
        assert_eq!(f.lines("a.txt"), ["x", "y"]);
        assert!(f.lines("b/c").is_empty());
        assert_eq!(base64_std(b"hello"), "aGVsbG8=");
        assert_eq!(base64_std(b"hi"), "aGk=");
        assert_eq!(f.glob("*.txt").files.len(), 1);
        assert_eq!(f.glob("[").files.len(), 2);
        let mut s = String::new();
        f.format(&mut s);
        assert_eq!(s, "map[a.txt:[120 10 121 10] b/c:[]]");
    }
}
