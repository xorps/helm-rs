//! Go's `path` (and, on Unix, identical `path/filepath`) functions.

use alloc::string::String;
use alloc::vec::Vec;

use super::sarg;
use crate::func::{FuncMap, Sig, Ty};
use crate::gofmt::strconv;
use crate::value::Value;

pub(super) fn register(f: &mut FuncMap) {
    const S1: Sig = Sig::new(&[Ty::String]);
    for (name, os) in [("base", "osBase"), ("dir", "osDir"), ("clean", "osClean"), ("ext", "osExt"), ("isAbs", "osIsAbs")] {
        let imp: fn(&[Value]) -> crate::func::FuncResult = match name {
            "base" => |a| Ok(Value::string(base(sarg(a, 0)))),
            "dir" => |a| Ok(Value::string(dir(sarg(a, 0)))),
            "clean" => |a| Ok(Value::string(clean(sarg(a, 0)))),
            "ext" => |a| Ok(Value::str(ext(sarg(a, 0)))),
            _ => |a| Ok(Value::Bool(sarg(a, 0).starts_with('/'))),
        };
        f.add(name, S1, imp);
        f.add(os, S1, imp);
    }
}

/// `path.Clean`.
pub(crate) fn clean(path: &str) -> String {
    if path.is_empty() {
        return String::from(".");
    }
    let p = path.as_bytes();
    let rooted = p[0] == b'/';
    let n = p.len();
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let (mut r, mut dotdot) = (0usize, 0usize);
    if rooted {
        out.push(b'/');
        r = 1;
        dotdot = 1;
    }
    while r < n {
        if p[r] == b'/' {
            r += 1;
        } else if p[r] == b'.' && (r + 1 == n || p[r + 1] == b'/') {
            r += 1;
        } else if p[r] == b'.' && p[r + 1] == b'.' && (r + 2 == n || p[r + 2] == b'/') {
            r += 2;
            if out.len() > dotdot {
                let mut last = out.pop();
                while out.len() > dotdot && last != Some(b'/') {
                    last = out.pop();
                }
            } else if !rooted {
                if !out.is_empty() {
                    out.push(b'/');
                }
                out.extend_from_slice(b"..");
                dotdot = out.len();
            }
        } else {
            if (rooted && out.len() != 1) || (!rooted && !out.is_empty()) {
                out.push(b'/');
            }
            while r < n && p[r] != b'/' {
                out.push(p[r]);
                r += 1;
            }
        }
    }
    if out.is_empty() {
        return String::from(".");
    }
    strconv::lossy_vec(out)
}

/// `path.Base`.
pub(crate) fn base(path: &str) -> String {
    if path.is_empty() {
        return String::from(".");
    }
    let p = path.trim_end_matches('/');
    let p = match p.rfind('/') {
        Some(i) => &p[i + 1..],
        None => p,
    };
    if p.is_empty() {
        return String::from("/");
    }
    String::from(p)
}

/// `path.Ext`.
pub(crate) fn ext(path: &str) -> &str {
    let b = path.as_bytes();
    let mut i = b.len();
    while i > 0 && b[i - 1] != b'/' {
        i -= 1;
        if b[i] == b'.' {
            return &path[i..];
        }
    }
    ""
}

/// `path.Dir`.
pub(crate) fn dir(path: &str) -> String {
    let d = match path.rfind('/') {
        Some(i) => &path[..i + 1],
        None => "",
    };
    clean(d)
}
