//! `url.go` (`urlParse`, `urlJoin`) on a port of Go's `net/url`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::sarg;
use crate::func::{FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::gofmt::strconv;
use crate::value::{Map, Value};

const ENCODE_PATH: u8 = 1;
const ENCODE_PATH_SEGMENT: u8 = 2;
const ENCODE_HOST: u8 = 4;
const ENCODE_ZONE: u8 = 8;
const ENCODE_USER_PASSWORD: u8 = 16;
const ENCODE_QUERY_COMPONENT: u8 = 32;
const ENCODE_FRAGMENT: u8 = 64;
const HEX_CHAR: u8 = 128;

// Generated from Go's net/url/encoding_table.go.
static URL_TABLE: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 76, 12, 0, 95, 0, 95, 12, 76, 76, 76, 95, 93, 127, 127, 65, //
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 79, 93, 12, 95, 12, 64, //
    67, 255, 255, 255, 255, 255, 255, 127, 127, 127, 127, 127, 127, 127, 127, 127, //
    127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 12, 0, 12, 0, 127, //
    0, 255, 255, 255, 255, 255, 255, 127, 127, 127, 127, 127, 127, 127, 127, 127, //
    127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 127, 0, 0, 0, 127, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
];

fn ishex(c: u8) -> bool {
    URL_TABLE[c as usize] & HEX_CHAR != 0
}

fn unhex(c: u8) -> u8 {
    9u8.wrapping_mul(c >> 6).wrapping_add(c & 15)
}

fn should_escape(c: u8, mode: u8) -> bool {
    URL_TABLE[c as usize] & mode == 0
}

fn unescape(s: &str, mode: u8) -> Result<String, String> {
    let b = s.as_bytes();
    let mut n = 0;
    let mut has_plus = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                n += 1;
                if i + 2 >= b.len() || !ishex(b[i + 1]) || !ishex(b[i + 2]) {
                    let rest = &b[i..];
                    let rest = &rest[..rest.len().min(3)];
                    return Err(format!("invalid URL escape {}", strconv::quote(&strconv::lossy(rest))));
                }
                if mode == ENCODE_HOST && unhex(b[i + 1]) < 8 && &b[i..i + 3] != b"%25" {
                    return Err(format!("invalid URL escape {}", strconv::quote(&strconv::lossy(&b[i..i + 3]))));
                }
                if mode == ENCODE_ZONE {
                    let v = (unhex(b[i + 1]) << 4) | unhex(b[i + 2]);
                    if &b[i..i + 3] != b"%25" && v != b' ' && should_escape(v, ENCODE_HOST) {
                        return Err(format!("invalid URL escape {}", strconv::quote(&strconv::lossy(&b[i..i + 3]))));
                    }
                }
                i += 3;
            }
            b'+' => {
                has_plus = mode == ENCODE_QUERY_COMPONENT;
                i += 1;
            }
            c => {
                if (mode == ENCODE_HOST || mode == ENCODE_ZONE) && c < 0x80 && should_escape(c, mode) {
                    return Err(format!(
                        "invalid character {} in host name",
                        strconv::quote(&strconv::lossy(&b[i..i + 1]))
                    ));
                }
                i += 1;
            }
        }
    }
    if n == 0 && !has_plus {
        return Ok(s.to_string());
    }
    let plus = if mode == ENCODE_QUERY_COMPONENT { b' ' } else { b'+' };
    let mut t: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                t.push((unhex(b[i + 1]) << 4) | unhex(b[i + 2]));
                i += 2;
            }
            b'+' => t.push(plus),
            c => t.push(c),
        }
        i += 1;
    }
    Ok(strconv::lossy_vec(t))
}

fn escape(s: &str, mode: u8) -> String {
    const UPPERHEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut t: Vec<u8> = Vec::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c == b' ' && mode == ENCODE_QUERY_COMPONENT {
            t.push(b'+');
        } else if should_escape(c, mode) {
            t.push(b'%');
            t.push(UPPERHEX[(c >> 4) as usize]);
            t.push(UPPERHEX[(c & 15) as usize]);
        } else {
            t.push(c);
        }
    }
    strconv::lossy_vec(t)
}

#[derive(Clone, Debug, Default)]
struct Userinfo {
    username: String,
    password: String,
    password_set: bool,
}

impl Userinfo {
    fn string(&self) -> String {
        let mut s = escape(&self.username, ENCODE_USER_PASSWORD);
        if self.password_set {
            s.push(':');
            s.push_str(&escape(&self.password, ENCODE_USER_PASSWORD));
        }
        s
    }
}

#[derive(Clone, Debug, Default)]
struct Url {
    scheme: String,
    opaque: String,
    user: Option<Userinfo>,
    host: String,
    path: String,
    raw_path: String,
    omit_host: bool,
    force_query: bool,
    raw_query: String,
    fragment: String,
    raw_fragment: String,
}

fn get_scheme(raw: &str) -> Result<(String, &str), String> {
    for (i, c) in raw.bytes().enumerate() {
        match c {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'0'..=b'9' | b'+' | b'-' | b'.' => {
                if i == 0 {
                    return Ok((String::new(), raw));
                }
            }
            b':' => {
                if i == 0 {
                    return Err("missing protocol scheme".into());
                }
                return Ok((raw[..i].to_string(), &raw[i + 1..]));
            }
            _ => return Ok((String::new(), raw)),
        }
    }
    Ok((String::new(), raw))
}

fn valid_optional_port(port: &str) -> bool {
    if port.is_empty() {
        return true;
    }
    let Some(rest) = port.strip_prefix(':') else { return false };
    rest.bytes().all(|b| b.is_ascii_digit())
}

fn valid_userinfo(s: &str) -> bool {
    s.chars().all(|r| {
        r.is_ascii_alphanumeric()
            || matches!(r, '-' | '.' | '_' | ':' | '~' | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' | '%' | '@')
    })
}

/// Minimal `netip.ParseAddr` check for bracketed hosts: returns whether it
/// is a valid IPv6 address (with optional zone).
fn parse_ipv6(s: &str) -> Result<(), String> {
    let (addr, zone) = match s.find('%') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    if zone == Some("") {
        return Err(format!("ParseAddr({}): zone must be a non-empty string", strconv::quote(s)));
    }
    let bad = || Err(format!("ParseAddr({}): unable to parse IP", strconv::quote(s)));
    if addr.is_empty() {
        return Err(format!("ParseAddr({}): unable to parse IP", strconv::quote(s)));
    }
    if !addr.contains(':') {
        // IPv4 (rejected by the caller) or garbage.
        let parts: Vec<&str> = addr.split('.').collect();
        if parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|c| c.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|v| v <= 255)) {
            return Ok(());
        }
        return bad();
    }
    // IPv6: at most one "::", hex groups, optional trailing IPv4.
    let groups_of = |s: &str| -> Option<usize> {
        if s.is_empty() {
            return Some(0);
        }
        let parts: Vec<&str> = s.split(':').collect();
        let mut n = 0;
        for (k, p) in parts.iter().enumerate() {
            if k == parts.len() - 1 && p.contains('.') {
                let q: Vec<&str> = p.split('.').collect();
                if q.len() != 4 || !q.iter().all(|x| !x.is_empty() && x.len() <= 3 && x.parse::<u16>().is_ok_and(|v| v <= 255)) {
                    return None;
                }
                n += 2;
            } else if p.is_empty() || p.len() > 4 || !p.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            } else {
                n += 1;
            }
        }
        Some(n)
    };
    match addr.split_once("::") {
        Some((a, b)) => {
            if b.contains("::") {
                return bad();
            }
            match (groups_of(a), groups_of(b)) {
                (Some(x), Some(y)) if x + y < 8 && !a.contains('.') => Ok(()),
                _ => bad(),
            }
        }
        None => match groups_of(addr) {
            Some(8) => Ok(()),
            _ => bad(),
        },
    }
}

fn parse_host(scheme: &str, host: &str) -> Result<String, String> {
    if let Some(open) = host.rfind('[') {
        if open > 0 {
            return Err("invalid IP-literal".into());
        }
        let Some(close) = host.rfind(']') else { return Err("missing ']' in host".into()) };
        let colon_port = &host[close + 1..];
        if !valid_optional_port(colon_port) {
            return Err(format!("invalid port {} after host", strconv::quote(colon_port)));
        }
        let unescaped_port = unescape(colon_port, ENCODE_HOST)?;
        if close < 1 {
            return Err("invalid IP-literal".into());
        }
        let hostname = &host[1..close];
        let unescaped = match hostname.find("%25") {
            Some(z) => {
                let hp = unescape(&hostname[..z], ENCODE_HOST)?;
                let zp = unescape(&hostname[z..], ENCODE_ZONE)?;
                hp + &zp
            }
            None => unescape(hostname, ENCODE_HOST)?,
        };
        parse_ipv6(&unescaped).map_err(|e| format!("invalid host: {e}"))?;
        if !unescaped.contains(':') {
            return Err("invalid IP-literal".into());
        }
        return Ok(format!("[{unescaped}]{unescaped_port}"));
    }
    if let Some(mut i) = host.find(':') {
        let last = host.rfind(':').unwrap_or(i);
        if last != i && scheme != "http" && scheme != "https" {
            i = last;
        }
        let colon_port = &host[i..];
        if !valid_optional_port(colon_port) {
            return Err(format!("invalid port {} after host", strconv::quote(colon_port)));
        }
    }
    unescape(host, ENCODE_HOST)
}

fn parse_authority(scheme: &str, authority: &str) -> Result<(Option<Userinfo>, String), String> {
    let i = authority.rfind('@');
    let host = match i {
        None => parse_host(scheme, authority)?,
        Some(i) => parse_host(scheme, &authority[i + 1..])?,
    };
    let Some(i) = i else { return Ok((None, host)) };
    let userinfo = &authority[..i];
    if !valid_userinfo(userinfo) {
        return Err("net/url: invalid userinfo".into());
    }
    let user = match userinfo.split_once(':') {
        None => Userinfo { username: unescape(userinfo, ENCODE_USER_PASSWORD)?, ..Default::default() },
        Some((u, p)) => Userinfo {
            username: unescape(u, ENCODE_USER_PASSWORD)?,
            password: unescape(p, ENCODE_USER_PASSWORD)?,
            password_set: true,
        },
    };
    Ok((Some(user), host))
}

impl Url {
    fn set_path(&mut self, p: &str) -> Result<(), String> {
        let path = unescape(p, ENCODE_PATH)?;
        self.raw_path = if escape(&path, ENCODE_PATH) == p { String::new() } else { p.to_string() };
        self.path = path;
        Ok(())
    }

    fn set_fragment(&mut self, f: &str) -> Result<(), String> {
        let frag = unescape(f, ENCODE_FRAGMENT)?;
        self.raw_fragment = if escape(&frag, ENCODE_FRAGMENT) == f { String::new() } else { f.to_string() };
        self.fragment = frag;
        Ok(())
    }

    fn escaped_path(&self) -> String {
        if !self.raw_path.is_empty() && valid_encoded(&self.raw_path, ENCODE_PATH) {
            if let Ok(p) = unescape(&self.raw_path, ENCODE_PATH) {
                if p == self.path {
                    return self.raw_path.clone();
                }
            }
        }
        if self.path == "*" {
            return "*".into();
        }
        escape(&self.path, ENCODE_PATH)
    }

    fn escaped_fragment(&self) -> String {
        if !self.raw_fragment.is_empty() && valid_encoded(&self.raw_fragment, ENCODE_FRAGMENT) {
            if let Ok(f) = unescape(&self.raw_fragment, ENCODE_FRAGMENT) {
                if f == self.fragment {
                    return self.raw_fragment.clone();
                }
            }
        }
        escape(&self.fragment, ENCODE_FRAGMENT)
    }

    fn hostname(&self) -> String {
        let mut host = self.host.as_str();
        if let Some(colon) = host.rfind(':') {
            if valid_optional_port(&host[colon..]) {
                host = &host[..colon];
            }
        }
        if host.starts_with('[') && host.ends_with(']') && host.len() >= 2 {
            host = &host[1..host.len() - 1];
        }
        host.to_string()
    }

    fn string(&self) -> String {
        let mut buf = String::new();
        if !self.scheme.is_empty() {
            buf.push_str(&self.scheme);
            buf.push(':');
        }
        if !self.opaque.is_empty() {
            buf.push_str(&self.opaque);
        } else {
            if (!self.scheme.is_empty() || !self.host.is_empty() || self.user.is_some())
                && !(self.omit_host && self.host.is_empty() && self.user.is_none())
            {
                if !self.host.is_empty() || !self.path.is_empty() || self.user.is_some() {
                    buf.push_str("//");
                }
                if let Some(ui) = &self.user {
                    buf.push_str(&ui.string());
                    buf.push('@');
                }
                if !self.host.is_empty() {
                    buf.push_str(&escape(&self.host, ENCODE_HOST));
                }
            }
            let mut path = self.escaped_path();
            if self.omit_host && self.host.is_empty() && self.user.is_none() && path.starts_with("//") {
                buf.push_str("%2F");
                path = path[1..].to_string();
            }
            if !path.is_empty() && !path.starts_with('/') && !self.host.is_empty() {
                buf.push('/');
            }
            if buf.is_empty() {
                let segment = path.split('/').next().unwrap_or("");
                if segment.contains(':') {
                    buf.push_str("./");
                }
            }
            buf.push_str(&path);
        }
        if self.force_query || !self.raw_query.is_empty() {
            buf.push('?');
            buf.push_str(&self.raw_query);
        }
        if !self.fragment.is_empty() {
            buf.push('#');
            buf.push_str(&self.escaped_fragment());
        }
        buf
    }
}

fn valid_encoded(s: &str, mode: u8) -> bool {
    s.bytes().all(|c| {
        matches!(c, b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':' | b'@' | b'[' | b']' | b'%')
            || !should_escape(c, mode)
    })
}

fn parse_inner(raw: &str) -> Result<Url, String> {
    if raw.bytes().any(|b| b < b' ' || b == 0x7f) {
        return Err("net/url: invalid control character in URL".into());
    }
    let mut url = Url::default();
    if raw == "*" {
        url.path = "*".into();
        return Ok(url);
    }
    let (scheme, mut rest) = get_scheme(raw)?;
    url.scheme = scheme.to_ascii_lowercase();
    if rest.ends_with('?') && rest.matches('?').count() == 1 {
        url.force_query = true;
        rest = &rest[..rest.len() - 1];
    } else if let Some((r, q)) = rest.split_once('?') {
        rest = r;
        url.raw_query = q.to_string();
    }
    if !rest.starts_with('/') {
        if !url.scheme.is_empty() {
            url.opaque = rest.to_string();
            return Ok(url);
        }
        let segment = rest.split('/').next().unwrap_or("");
        if segment.contains(':') {
            return Err("first path segment in URL cannot contain colon".into());
        }
    }
    let mut rest = rest.to_string();
    if (!url.scheme.is_empty() || !rest.starts_with("///")) && rest.starts_with("//") {
        let mut authority = rest[2..].to_string();
        rest = String::new();
        if let Some(i) = authority.find('/') {
            rest = authority[i..].to_string();
            authority.truncate(i);
        }
        let (user, host) = parse_authority(&url.scheme, &authority)?;
        url.user = user;
        url.host = host;
    } else if !url.scheme.is_empty() && rest.starts_with('/') {
        url.omit_host = true;
    }
    url.set_path(&rest)?;
    Ok(url)
}

/// `url.Parse`, with Go's `*url.Error` message on failure.
fn parse(raw: &str) -> Result<Url, String> {
    let (u, frag) = match raw.split_once('#') {
        Some((u, f)) => (u, f),
        None => (raw, ""),
    };
    let mut url = parse_inner(u).map_err(|e| format!("parse {}: {e}", strconv::quote(u)))?;
    if frag.is_empty() {
        return Ok(url);
    }
    url.set_fragment(frag).map_err(|e| format!("parse {}: {e}", strconv::quote(raw)))?;
    Ok(url)
}

fn url_parse(a: &[Value]) -> FuncResult {
    let u = parse(sarg(a, 0)).map_err(|e| FuncError::new(format!("unable to parse url: {e}")))?;
    let mut m = Map::new();
    m.insert("scheme".into(), Value::str(&u.scheme));
    m.insert("host".into(), Value::str(&u.host));
    m.insert("hostname".into(), Value::string(u.hostname()));
    m.insert("path".into(), Value::str(&u.path));
    m.insert("query".into(), Value::str(&u.raw_query));
    m.insert("opaque".into(), Value::str(&u.opaque));
    m.insert("fragment".into(), Value::str(&u.fragment));
    m.insert("userinfo".into(), Value::string(u.user.as_ref().map(Userinfo::string).unwrap_or_default()));
    Ok(Value::map(m))
}

fn dict_get_or_empty(d: &crate::value::MapRef, key: &str) -> Result<String, FuncError> {
    match d.get(key) {
        None => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.to_string()),
        Some(Value::Nil) => Err(FuncError::new(super::NIL_DEREF)),
        Some(v) => Err(FuncError::new(format!(
            "unable to parse {key} key, must be of type string, but {} found",
            v.kind().name()
        ))),
    }
}

fn url_join(a: &[Value]) -> FuncResult {
    let d = match a.first() {
        Some(Value::Map(m)) => m.clone(),
        _ => crate::value::MapRef::default(),
    };
    let mut u = Url {
        scheme: dict_get_or_empty(&d, "scheme")?,
        host: dict_get_or_empty(&d, "host")?,
        path: dict_get_or_empty(&d, "path")?,
        raw_query: dict_get_or_empty(&d, "query")?,
        opaque: dict_get_or_empty(&d, "opaque")?,
        fragment: dict_get_or_empty(&d, "fragment")?,
        ..Default::default()
    };
    let userinfo = dict_get_or_empty(&d, "userinfo")?;
    if !userinfo.is_empty() {
        let t = parse(&format!("proto://{userinfo}@host"))
            .map_err(|e| FuncError::new(format!("unable to parse userinfo in dict: {e}")))?;
        u.user = t.user;
    }
    Ok(Value::string(u.string()))
}

pub(super) fn register(f: &mut FuncMap) {
    f.add("urlParse", Sig::new(&[Ty::String]), url_parse);
    f.add("urlJoin", Sig::new(&[Ty::Map]), url_join);
}

#[allow(dead_code)]
const UNUSED: [u8; 2] = [ENCODE_PATH_SEGMENT, ENCODE_FRAGMENT];
