//! Semantic versions and constraints compatible with
//! `github.com/Masterminds/semver/v3` v3.5.0 (as pinned by Helm v4.3.0).
//!
//! [`Version::parse`] is `semver.NewVersion` (lenient "coerce" mode: `v1.2`,
//! `1`), [`Constraints::parse`] is `semver.NewConstraint`, and
//! [`Constraints::check`] is `Constraints.Check`, including the rule that
//! prerelease versions only satisfy constraints that mention a prerelease
//! (`>=1.21.0-0`). Error messages match Go's.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::Ordering;

use regex_automata::meta::Regex;
use regex_automata::util::captures::Captures;

use crate::func::{FuncError, Sig, Ty};
use crate::gofmt::strconv;
use crate::value::{Kind, Object, Value};

/// Maximum version string length (`semver.MaxVersionLen`).
pub const MAX_VERSION_LEN: usize = 256;
/// Maximum constraint string length (`semver.MaxConstraintLen`).
pub const MAX_CONSTRAINT_LEN: usize = 512;
/// Maximum number of `||` groups (`semver.MaxConstraintGroups`).
pub const MAX_CONSTRAINT_GROUPS: usize = 32;

pub const ERR_INVALID_SEMVER: &str = "invalid semantic version";
pub const ERR_INVALID_METADATA: &str = "invalid metadata string";
pub const ERR_INVALID_PRERELEASE: &str = "invalid prerelease string";
pub const ERR_SEGMENT_STARTS_ZERO: &str = "version segment starts with 0";
pub const ERR_INVALID_CHARACTERS: &str = "invalid characters in version";
pub const ERR_EMPTY_STRING: &str = "version string empty";

/// A parsed version (`*semver.Version`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: String,
    pub metadata: String,
    pub original: String,
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

/// Matches `X+(\.X+)*` with X = `[0-9A-Za-z-]`.
fn dotted_idents(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(|p| !p.is_empty() && p.bytes().all(is_ident_char))
}

fn contains_only(s: &str, allowed: fn(u8) -> bool) -> bool {
    s.bytes().all(allowed)
}

fn validate_prerelease(p: &str) -> Result<(), String> {
    for part in p.split('.') {
        if part.is_empty() {
            return Err(ERR_INVALID_PRERELEASE.into());
        } else if contains_only(part, |c| c.is_ascii_digit()) {
            if part.len() > 1 && part.as_bytes()[0] == b'0' {
                return Err(ERR_SEGMENT_STARTS_ZERO.into());
            }
        } else if !contains_only(part, is_ident_char) {
            return Err(ERR_INVALID_PRERELEASE.into());
        }
    }
    Ok(())
}

fn validate_metadata(m: &str) -> Result<(), String> {
    for part in m.split('.') {
        if part.is_empty() || !contains_only(part, is_ident_char) {
            return Err(ERR_INVALID_METADATA.into());
        }
    }
    Ok(())
}

fn parse_segment(s: &str) -> Result<u64, String> {
    strconv::parse_uint(s, 10, 64)
        .map_err(|e| format!("error parsing version segment: {}", strconv::num_error("ParseUint", s, e)))
}

/// Matches Masterminds' loose version regex; returns
/// (major, minor-with-dot, patch-with-dot, pre, metadata).
fn match_loose(v: &str) -> Option<(&str, &str, &str, &str, &str)> {
    let b = v.as_bytes();
    let mut i = 0;
    if i < b.len() && b[i] == b'v' {
        i += 1;
    }
    let digits = |i: usize| {
        let mut j = i;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        j
    };
    let j = digits(i);
    if j == i {
        return None;
    }
    let major = &v[i..j];
    i = j;
    let mut parts = ["", ""];
    for p in parts.iter_mut() {
        if i < b.len() && b[i] == b'.' {
            let j = digits(i + 1);
            if j > i + 1 {
                *p = &v[i..j];
                i = j;
            }
        }
    }
    let mut pre = "";
    if i < b.len() && b[i] == b'-' {
        let start = i + 1;
        let mut j = start;
        while j < b.len() && (is_ident_char(b[j]) || b[j] == b'.') {
            j += 1;
        }
        let cand = &v[start..j];
        if !dotted_idents(cand) {
            return None;
        }
        pre = cand;
        i = j;
    }
    let mut meta = "";
    if i < b.len() && b[i] == b'+' {
        let cand = &v[i + 1..];
        if !dotted_idents(cand) {
            return None;
        }
        meta = cand;
        i = b.len();
    }
    if i != b.len() {
        return None;
    }
    Some((major, parts[0], parts[1], pre, meta))
}

impl Version {
    /// `semver.NewVersion` (coercing mode, Helm's default).
    pub fn parse(v: &str) -> Result<Version, String> {
        if v.len() > MAX_VERSION_LEN {
            return Err(format!("version string is too long (max {MAX_VERSION_LEN} bytes)"));
        }
        let Some((major, minor, patch, pre, meta)) = match_loose(v) else {
            return Err(ERR_INVALID_SEMVER.into());
        };
        let mut sv = Version {
            major: parse_segment(major)?,
            minor: 0,
            patch: 0,
            pre: pre.into(),
            metadata: meta.into(),
            original: v.into(),
        };
        if !minor.is_empty() {
            sv.minor = parse_segment(&minor[1..])?;
        }
        if !patch.is_empty() {
            sv.patch = parse_segment(&patch[1..])?;
        }
        if !sv.pre.is_empty() {
            validate_prerelease(&sv.pre)?;
        }
        if !sv.metadata.is_empty() {
            validate_metadata(&sv.metadata)?;
        }
        Ok(sv)
    }

    /// `semver.StrictNewVersion`.
    pub fn parse_strict(v: &str) -> Result<Version, String> {
        if v.is_empty() {
            return Err(ERR_EMPTY_STRING.into());
        }
        if v.len() > MAX_VERSION_LEN {
            return Err(format!("version string is too long (max {MAX_VERSION_LEN} bytes)"));
        }
        let parts: Vec<&str> = v.splitn(3, '.').collect();
        if parts.len() != 3 {
            return Err(ERR_INVALID_SEMVER.into());
        }
        let mut sv = Version { major: 0, minor: 0, patch: 0, pre: String::new(), metadata: String::new(), original: v.into() };
        let mut p2 = parts[2];
        if let Some((a, m)) = p2.split_once('+') {
            sv.metadata = m.into();
            p2 = a;
            validate_metadata(m)?;
        }
        if let Some((a, p)) = p2.split_once('-') {
            sv.pre = p.into();
            p2 = a;
            validate_prerelease(p)?;
        }
        for p in [parts[0], parts[1], p2] {
            if !contains_only(p, |c| c.is_ascii_digit()) {
                return Err(ERR_INVALID_CHARACTERS.into());
            }
            if p.len() > 1 && p.as_bytes()[0] == b'0' {
                return Err(ERR_SEGMENT_STARTS_ZERO.into());
            }
        }
        let pu = |s: &str| strconv::parse_uint(s, 10, 64).map_err(|e| strconv::num_error("ParseUint", s, e));
        sv.major = pu(parts[0])?;
        sv.minor = pu(parts[1])?;
        sv.patch = pu(p2)?;
        Ok(sv)
    }

    /// `semver.New(major, minor, patch, pre, metadata)`.
    pub fn new(major: u64, minor: u64, patch: u64, pre: &str, metadata: &str) -> Version {
        let mut v = Version { major, minor, patch, pre: pre.into(), metadata: metadata.into(), original: String::new() };
        v.original = v.to_string();
        v
    }

    pub fn original(&self) -> &str {
        &self.original
    }

    fn original_v_prefix(&self) -> &str {
        if self.original.starts_with('v') { "v" } else { "" }
    }

    /// `Version.IncPatch()`; errors on overflow (Go panics).
    pub fn inc_patch(&self) -> Result<Version, String> {
        let mut n = self.clone();
        n.metadata.clear();
        if !self.pre.is_empty() {
            n.pre.clear();
        } else {
            n.pre.clear();
            if self.patch == u64::MAX {
                return Err("patch version increment would overflow uint64".into());
            }
            n.patch = self.patch + 1;
        }
        n.original = format!("{}{}", self.original_v_prefix(), n);
        Ok(n)
    }

    /// `Version.IncMinor()`.
    pub fn inc_minor(&self) -> Result<Version, String> {
        let mut n = self.clone();
        n.metadata.clear();
        n.pre.clear();
        n.patch = 0;
        if self.minor == u64::MAX {
            return Err("minor version increment would overflow uint64".into());
        }
        n.minor = self.minor + 1;
        n.original = format!("{}{}", self.original_v_prefix(), n);
        Ok(n)
    }

    /// `Version.IncMajor()`.
    pub fn inc_major(&self) -> Result<Version, String> {
        let mut n = self.clone();
        n.metadata.clear();
        n.pre.clear();
        n.patch = 0;
        n.minor = 0;
        if self.major == u64::MAX {
            return Err("major version increment would overflow uint64".into());
        }
        n.major = self.major + 1;
        n.original = format!("{}{}", self.original_v_prefix(), n);
        Ok(n)
    }

    /// `Version.SetPrerelease(p)`.
    pub fn set_prerelease(&self, p: &str) -> Result<Version, String> {
        let mut n = self.clone();
        if !p.is_empty() {
            validate_prerelease(p)?;
        }
        n.pre = p.into();
        n.original = format!("{}{}", self.original_v_prefix(), n);
        Ok(n)
    }

    /// `Version.SetMetadata(m)`.
    pub fn set_metadata(&self, m: &str) -> Result<Version, String> {
        let mut n = self.clone();
        if !m.is_empty() {
            validate_metadata(m)?;
        }
        n.metadata = m.into();
        n.original = format!("{}{}", self.original_v_prefix(), n);
        Ok(n)
    }

    /// `Version.Compare(o)`: -1, 0 or 1 (metadata is ignored).
    pub fn compare(&self, o: &Version) -> i32 {
        for (a, b) in [(self.major, o.major), (self.minor, o.minor), (self.patch, o.patch)] {
            match a.cmp(&b) {
                Ordering::Less => return -1,
                Ordering::Greater => return 1,
                Ordering::Equal => {}
            }
        }
        let (ps, po) = (self.pre.as_str(), o.pre.as_str());
        if ps.is_empty() && po.is_empty() {
            return 0;
        }
        if ps.is_empty() {
            return 1;
        }
        if po.is_empty() {
            return -1;
        }
        compare_prerelease(ps, po)
    }

    pub fn less_than(&self, o: &Version) -> bool {
        self.compare(o) < 0
    }
    pub fn equal(&self, o: &Version) -> bool {
        self.compare(o) == 0
    }
}

impl core::fmt::Display for Version {
    /// `Version.String()`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        if !self.metadata.is_empty() {
            write!(f, "+{}", self.metadata)?;
        }
        Ok(())
    }
}

fn compare_prerelease(v: &str, o: &str) -> i32 {
    let sparts: Vec<&str> = v.split('.').collect();
    let oparts: Vec<&str> = o.split('.').collect();
    let l = sparts.len().max(oparts.len());
    for i in 0..l {
        let s = sparts.get(i).copied().unwrap_or("");
        let t = oparts.get(i).copied().unwrap_or("");
        let d = compare_pre_part(s, t);
        if d != 0 {
            return d;
        }
    }
    0
}

fn compare_pre_part(s: &str, o: &str) -> i32 {
    if s == o {
        return 0;
    }
    if s.is_empty() {
        return if !o.is_empty() { -1 } else { 1 };
    }
    if o.is_empty() {
        return if !s.is_empty() { 1 } else { -1 };
    }
    let oi = strconv::parse_uint(o, 10, 64);
    let si = strconv::parse_uint(s, 10, 64);
    match (oi, si) {
        (Err(_), Err(_)) => {
            if s > o {
                1
            } else {
                -1
            }
        }
        (Err(_), Ok(_)) => -1,
        (Ok(_), Err(_)) => 1,
        (Ok(oi), Ok(si)) => {
            if si > oi {
                1
            } else {
                -1
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Constraints
// ---------------------------------------------------------------------------

/// Go's `\s` (ASCII whitespace without `\v`).
const WS: &str = r"[\t\n\f\r ]";
const OPS: &str = r"=||!=|>|<|>=|=>|<=|=<|~|~>|\^";
const CV: &str = r"v?([0-9|x|X|\*]+)(\.[0-9|x|X|\*]+)?(\.[0-9|x|X|\*]+)?(-([0-9A-Za-z\-]+(\.[0-9A-Za-z\-]+)*))?(\+([0-9A-Za-z\-]+(\.[0-9A-Za-z\-]+)*))?";

fn build(pat: &str) -> Regex {
    // The patterns are constants; building cannot fail.
    Regex::new(pat).unwrap_or_else(|_| Regex::new("$^").expect("trivial regex"))
}

struct Regexes {
    constraint: Regex,
    range: Regex,
    find: Regex,
    valid: Regex,
}

impl Regexes {
    fn new() -> Regexes {
        Regexes {
            constraint: build(&format!(r"^{WS}*({OPS}){WS}*({CV}){WS}*$")),
            range: build(&format!(r"{WS}*({CV}){WS}+-{WS}+({CV}){WS}*")),
            find: build(&format!(r"({OPS}){WS}*({CV})")),
            valid: build(&format!(r"^({WS}*({OPS}){WS}*({CV}){WS}*)((?:{WS}+|,{WS}*)({OPS}){WS}*({CV}){WS}*)*$")),
        }
    }
}

fn group<'h>(caps: &Captures, h: &'h str, i: usize) -> &'h str {
    caps.get_group(i).map(|s| &h[s.range()]).unwrap_or("")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    TildeOrEqual,
    NotEqual,
    GreaterThan,
    LessThan,
    GreaterThanEqual,
    LessThanEqual,
    Tilde,
    Caret,
}

/// A single constraint, e.g. `>=1.2`.
#[derive(Clone, Debug)]
struct Constraint {
    con: Version,
    orig: String,
    origfunc: String,
    op: Op,
    minor_dirty: bool,
    dirty: bool,
    patch_dirty: bool,
}

fn is_x(x: &str) -> bool {
    matches!(x, "x" | "*" | "X")
}

fn parse_constraint(re: &Regexes, c: &str) -> Result<Constraint, String> {
    if c.is_empty() {
        return Ok(Constraint {
            con: Version::new(0, 0, 0, "", ""),
            orig: String::new(),
            origfunc: String::new(),
            op: Op::TildeOrEqual,
            minor_dirty: false,
            patch_dirty: false,
            dirty: true,
        });
    }
    let mut caps = re.constraint.create_captures();
    re.constraint.captures(c, &mut caps);
    if !caps.is_match() {
        return Err(format!("improper constraint: {}", strconv::quote(c)));
    }
    let m = |i| group(&caps, c, i);
    let origfunc = m(1);
    let op = match origfunc {
        "" | "=" => Op::TildeOrEqual,
        "!=" => Op::NotEqual,
        ">" => Op::GreaterThan,
        "<" => Op::LessThan,
        ">=" | "=>" => Op::GreaterThanEqual,
        "<=" | "=<" => Op::LessThanEqual,
        "~" | "~>" => Op::Tilde,
        _ => Op::Caret,
    };
    let mut ver = m(2).to_string();
    let (mut minor_dirty, mut patch_dirty, mut dirty) = (false, false, false);
    let trim = |s: &'_ str| -> String { s.strip_prefix('.').unwrap_or(s).to_string() };
    if is_x(m(3)) || m(3).is_empty() {
        ver = format!("0.0.0{}", m(6));
        dirty = true;
    } else if is_x(&trim(m(4))) || m(4).is_empty() {
        minor_dirty = true;
        dirty = true;
        ver = format!("{}.0.0{}", m(3), m(6));
    } else if is_x(&trim(m(5))) || m(5).is_empty() {
        dirty = true;
        patch_dirty = true;
        ver = format!("{}{}.0{}", m(3), m(4), m(6));
    }
    let con = Version::parse(&ver).map_err(|_| String::from("constraint parser error"))?;
    Ok(Constraint { con, orig: m(2).to_string(), origfunc: origfunc.to_string(), op, minor_dirty, dirty, patch_dirty })
}

fn q(v: &Version) -> String {
    strconv::quote(&v.to_string())
}

impl Constraint {
    fn check(&self, v: &Version, include_pre: bool) -> Result<(), String> {
        let c = self;
        let orig = strconv::quote(&c.orig);
        if !v.pre.is_empty() && !include_pre {
            return Err(format!(
                "{} is a prerelease version and the constraint is only looking for release versions",
                q(v)
            ));
        }
        match c.op {
            Op::NotEqual => {
                if c.dirty {
                    if c.con.major != v.major {
                        return Ok(());
                    }
                    if c.con.minor != v.minor && !c.minor_dirty {
                        return Ok(());
                    } else if c.minor_dirty {
                        return Err(format!("{} is equal to {orig}", q(v)));
                    } else if c.con.patch != v.patch && !c.patch_dirty {
                        return Ok(());
                    } else if c.patch_dirty {
                        if !v.pre.is_empty() || !c.con.pre.is_empty() {
                            if compare_prerelease(&v.pre, &c.con.pre) != 0 {
                                return Ok(());
                            }
                            return Err(format!("{} is equal to {orig}", q(v)));
                        }
                        return Err(format!("{} is equal to {orig}", q(v)));
                    }
                }
                if v.equal(&c.con) {
                    return Err(format!("{} is equal to {orig}", q(v)));
                }
                Ok(())
            }
            Op::GreaterThan => {
                let err = || Err(format!("{} is less than or equal to {orig}", q(v)));
                if !c.dirty {
                    return if v.compare(&c.con) == 1 { Ok(()) } else { err() };
                }
                if v.major > c.con.major {
                    Ok(())
                } else if v.major < c.con.major || c.minor_dirty {
                    err()
                } else if c.patch_dirty {
                    if v.minor > c.con.minor { Ok(()) } else { err() }
                } else if v.compare(&c.con) == 1 {
                    Ok(())
                } else {
                    err()
                }
            }
            Op::LessThan => {
                if v.compare(&c.con) < 0 {
                    Ok(())
                } else {
                    Err(format!("{} is greater than or equal to {orig}", q(v)))
                }
            }
            Op::GreaterThanEqual => {
                if v.compare(&c.con) >= 0 {
                    Ok(())
                } else {
                    Err(format!("{} is less than {orig}", q(v)))
                }
            }
            Op::LessThanEqual => {
                let err = || Err(format!("{} is greater than {orig}", q(v)));
                if !c.dirty {
                    return if v.compare(&c.con) <= 0 { Ok(()) } else { err() };
                }
                if v.major > c.con.major || (v.major == c.con.major && v.minor > c.con.minor && !c.minor_dirty) {
                    return err();
                }
                Ok(())
            }
            Op::Tilde => c.tilde(v, &orig),
            Op::TildeOrEqual => {
                if c.dirty {
                    return c.tilde(v, &orig);
                }
                if v.equal(&c.con) {
                    Ok(())
                } else {
                    Err(format!("{} is not equal to {orig}", q(v)))
                }
            }
            Op::Caret => {
                if v.less_than(&c.con) {
                    return Err(format!("{} is less than {orig}", q(v)));
                }
                if c.con.major > 0 || c.minor_dirty {
                    if v.major == c.con.major {
                        return Ok(());
                    }
                    return Err(format!("{} does not have same major version as {orig}", q(v)));
                }
                if c.con.major == 0 && v.major > 0 {
                    return Err(format!("{} does not have same major version as {orig}", q(v)));
                }
                if c.con.minor > 0 || c.patch_dirty {
                    if v.minor == c.con.minor {
                        return Ok(());
                    }
                    return Err(format!(
                        "{} does not have same minor version as {orig}. Expected minor versions to match when constraint major version is 0",
                        q(v)
                    ));
                }
                if c.con.minor == 0 && v.minor > 0 {
                    return Err(format!("{} does not have same minor version as {orig}", q(v)));
                }
                if c.con.patch == v.patch {
                    return Ok(());
                }
                Err(format!(
                    "{} does not equal {orig}. Expect version and constraint to equal when major and minor versions are 0",
                    q(v)
                ))
            }
        }
    }

    fn tilde(&self, v: &Version, orig: &str) -> Result<(), String> {
        let c = self;
        if v.less_than(&c.con) {
            return Err(format!("{} is less than {orig}", q(v)));
        }
        if c.con.major == 0 && c.con.minor == 0 && c.con.patch == 0 && !c.minor_dirty && !c.patch_dirty {
            return Ok(());
        }
        if v.major != c.con.major {
            return Err(format!("{} does not have same major version as {orig}", q(v)));
        }
        if v.minor != c.con.minor && !c.minor_dirty {
            return Err(format!("{} does not have same major and minor version as {orig}", q(v)));
        }
        Ok(())
    }
}

/// A parsed constraint set (`semver.NewConstraint`), e.g. `>=1.21.0-0`.
#[derive(Clone, Debug)]
pub struct Constraints {
    constraints: Vec<Vec<Constraint>>,
    contains_pre: Vec<bool>,
    /// `Constraints.IncludePrerelease`.
    pub include_prerelease: bool,
}

fn rewrite_range(re: &Regexes, i: &str) -> String {
    let mut o = i.to_string();
    let mut found: Vec<(String, String, String)> = Vec::new();
    for c in re.range.captures_iter(i) {
        let all = group(&c, i, 0).to_string();
        found.push((all, group(&c, i, 1).to_string(), group(&c, i, 11).to_string()));
    }
    for (all, a, b) in found {
        let t = format!(">= {a}, <= {b} ");
        if let Some(pos) = o.find(&all) {
            o.replace_range(pos..pos + all.len(), &t);
        }
    }
    o
}

impl Constraints {
    /// `semver.NewConstraint(c)`.
    pub fn parse(c: &str) -> Result<Constraints, String> {
        if c.len() > MAX_CONSTRAINT_LEN {
            return Err(format!("constraint string is too long (max {MAX_CONSTRAINT_LEN} bytes)"));
        }
        let re = Regexes::new();
        let c = rewrite_range(&re, c);
        let ors: Vec<&str> = c.split("||").collect();
        if ors.len() > MAX_CONSTRAINT_GROUPS {
            return Err(format!("too many constraint groups (max {MAX_CONSTRAINT_GROUPS})"));
        }
        let mut or = Vec::with_capacity(ors.len());
        let mut has_pre = Vec::with_capacity(ors.len());
        for v in ors {
            if !re.valid.is_match(v) {
                return Err(format!("improper constraint: {}", strconv::quote(v)));
            }
            let mut cs: Vec<&str> = re.find.find_iter(v).map(|m| &v[m.range()]).collect();
            if cs.is_empty() {
                cs.push(v);
            }
            let mut result = Vec::with_capacity(cs.len());
            let mut pre = false;
            for s in cs {
                let pc = parse_constraint(&re, s)?;
                if !pc.con.pre.is_empty() {
                    pre = true;
                }
                result.push(pc);
            }
            or.push(result);
            has_pre.push(pre);
        }
        Ok(Constraints { constraints: or, contains_pre: has_pre, include_prerelease: false })
    }

    /// `Constraints.Check(v)`.
    pub fn check(&self, v: &Version) -> bool {
        for (i, o) in self.constraints.iter().enumerate() {
            let include = self.include_prerelease || self.contains_pre[i];
            if o.iter().all(|c| c.check(v, include).is_ok()) {
                return true;
            }
        }
        false
    }

    /// `Constraints.Validate(v)`: whether `v` satisfies the constraints, and
    /// the reasons why not.
    pub fn validate(&self, v: &Version) -> (bool, Vec<String>) {
        let mut e = Vec::new();
        let mut prerelease = false;
        for (i, o) in self.constraints.iter().enumerate() {
            let mut joy = true;
            let include = self.include_prerelease || self.contains_pre[i];
            for c in o {
                if !include && !v.pre.is_empty() {
                    if !prerelease {
                        e.push(format!(
                            "{} is a prerelease version and the constraint is only looking for release versions",
                            q(v)
                        ));
                        prerelease = true;
                    }
                    joy = false;
                } else if let Err(err) = c.check(v, include) {
                    e.push(err);
                    joy = false;
                }
            }
            if joy {
                return (true, Vec::new());
            }
        }
        (false, e)
    }
}

impl core::fmt::Display for Constraints {
    /// `Constraints.String()`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let groups: Vec<String> = self
            .constraints
            .iter()
            .map(|g| g.iter().map(|c| format!("{}{}", c.origfunc, c.orig)).collect::<Vec<_>>().join(" "))
            .collect();
        f.write_str(&groups.join(" || "))
    }
}

// ---------------------------------------------------------------------------
// Template object
// ---------------------------------------------------------------------------

/// Type name of a `*semver.Version` for template signatures.
pub const VERSION_PTR_TYPE: &str = "*semver.Version";

/// A `*semver.Version` (or, for results of `IncMajor` & co., a
/// `semver.Version` value) as seen by templates.
#[derive(Clone, Debug)]
pub struct VersionObject {
    pub version: Version,
    /// True for a `semver.Version` value (only value-receiver methods).
    pub by_value: bool,
}

impl VersionObject {
    pub fn ptr(version: Version) -> Value {
        Value::object(VersionObject { version, by_value: false })
    }
}

fn version_arg(v: &Value) -> Result<&Version, FuncError> {
    match v {
        Value::Object(o) => match o.as_any().downcast_ref::<VersionObject>() {
            Some(vo) => Ok(&vo.version),
            None => Err(FuncError::new("invalid memory address or nil pointer dereference")),
        },
        _ => Err(FuncError::new("runtime error: invalid memory address or nil pointer dereference")),
    }
}

impl Object for VersionObject {
    fn type_name(&self) -> &str {
        if self.by_value { "semver.Version" } else { VERSION_PTR_TYPE }
    }
    fn kind(&self) -> Kind {
        if self.by_value { Kind::Struct } else { Kind::Ptr }
    }
    fn method(&self, name: &str) -> Option<Sig> {
        const NONE: Sig = Sig::new(&[]);
        const VER: Sig = Sig::new(&[Ty::Object(VERSION_PTR_TYPE)]);
        const STR: Sig = Sig::new(&[Ty::String]);
        let value_method = match name {
            "String" | "Major" | "Minor" | "Patch" | "Prerelease" | "Metadata" | "IncPatch" | "IncMinor"
            | "IncMajor" | "MarshalJSON" | "MarshalText" | "Value" => Some(NONE),
            "SetPrerelease" | "SetMetadata" => Some(STR),
            _ => None,
        };
        if value_method.is_some() || self.by_value {
            return value_method;
        }
        match name {
            "Original" => Some(NONE),
            "LessThan" | "LessThanEqual" | "GreaterThan" | "GreaterThanEqual" | "Equal" | "Compare" => Some(VER),
            _ => None,
        }
    }
    fn call_method(&self, name: &str, args: &[Value]) -> Result<Value, FuncError> {
        let v = &self.version;
        let val = |r: Result<Version, String>| -> Result<Value, FuncError> {
            r.map(|version| Value::object(VersionObject { version, by_value: true })).map_err(FuncError::new)
        };
        let s = |i: usize| args.get(i).and_then(Value::as_str).unwrap_or("");
        Ok(match name {
            "String" => Value::string(v.to_string()),
            "Original" => Value::str(&v.original),
            "Major" => Value::Uint64(v.major),
            "Minor" => Value::Uint64(v.minor),
            "Patch" => Value::Uint64(v.patch),
            "Prerelease" => Value::str(&v.pre),
            "Metadata" => Value::str(&v.metadata),
            "IncPatch" => return val(v.inc_patch()),
            "IncMinor" => return val(v.inc_minor()),
            "IncMajor" => return val(v.inc_major()),
            "SetPrerelease" => return val(v.set_prerelease(s(0))),
            "SetMetadata" => return val(v.set_metadata(s(0))),
            "MarshalJSON" => Value::Bytes(strconv::quote(&v.to_string()).into_bytes().into()),
            "MarshalText" => Value::Bytes(v.to_string().into_bytes().into()),
            "Value" => Value::string(v.to_string()),
            "LessThan" | "LessThanEqual" | "GreaterThan" | "GreaterThanEqual" | "Equal" | "Compare" => {
                let o = version_arg(args.first().unwrap_or(&Value::Nil))?;
                let c = v.compare(o);
                match name {
                    "LessThan" => Value::Bool(c < 0),
                    "LessThanEqual" => Value::Bool(c <= 0),
                    "GreaterThan" => Value::Bool(c > 0),
                    "GreaterThanEqual" => Value::Bool(c >= 0),
                    "Equal" => Value::Bool(c == 0),
                    _ => Value::Int(c as i64),
                }
            }
            _ => return Err(FuncError::new(format!("can't call method {name} on {}", self.type_name()))),
        })
    }
    fn format(&self, out: &mut String) {
        out.push_str(&self.version.to_string());
    }
    fn to_data(&self) -> Value {
        Value::string(self.version.to_string())
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
