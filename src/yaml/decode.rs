//! Node composition and decoding into Go `interface{}` values, following
//! `decode.go` of goyaml v2, plus the YAML→JSON conversion done by
//! `sigs.k8s.io/yaml`.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::MAX_DEPTH;
use super::parser::{Event, EventType, Parser};
use super::resolve::{self, BINARY_TAG, MERGE_TAG, Resolved, TIMESTAMP_TAG};
use super::scanner::{ErrorKind, Mark};
use super::strconv::{format_float, go_lossy_utf8, quote};

/// An error raised while decoding (Go's `failf`), message without the
/// `yaml: ` prefix, with the position it relates to (0-based mark).
#[derive(Clone, Debug)]
pub(crate) struct DecodeError {
    pub message: String,
    pub mark: Option<Mark>,
    /// Byte offset (reader errors).
    pub offset: Option<usize>,
}

impl DecodeError {
    fn new(message: String, mark: Option<Mark>) -> Self {
        DecodeError {
            message,
            mark,
            offset: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Document,
    Mapping,
    Sequence,
    Scalar,
    Alias,
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub kind: NodeKind,
    pub mark: Mark,
    pub tag: String,
    pub value: String,
    pub implicit: bool,
    pub children: Vec<usize>,
    pub alias: Option<usize>,
}

/// Parses the first document of `input` into a node arena. Returns the
/// arena and the root (document) node, or `None` for an empty stream.
pub(crate) fn compose(input: &[u8]) -> Result<(Vec<Node>, Option<usize>), DecodeError> {
    let input: &[u8] = if input.is_empty() { b"\n" } else { input };
    let mut p = Parser::new(input);
    let mut nodes: Vec<Node> = Vec::new();
    let mut anchors: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut ev = Event::default();
    let mut started = false;
    let mut root: Option<usize> = None;
    loop {
        if !p.parse(&mut ev) {
            return Err(parser_error(&p));
        }
        if !started {
            // init(): expect STREAM-START.
            if ev.typ != EventType::StreamStart {
                return Err(expect_error(&p, EventType::StreamStart, ev.typ));
            }
            started = true;
            continue;
        }
        let new_node = |kind: NodeKind, ev: &Event| Node {
            kind,
            mark: ev.start_mark,
            tag: String::new(),
            value: String::new(),
            implicit: false,
            children: Vec::new(),
            alias: None,
        };
        let id = nodes.len();
        match ev.typ {
            EventType::StreamEnd => {
                if stack.is_empty() {
                    return Ok((nodes, root));
                }
                return Err(DecodeError::new(
                    "attempted to go past the end of stream; corrupted value?".into(),
                    Some(ev.start_mark),
                ));
            }
            EventType::DocumentStart => {
                if !stack.is_empty() {
                    return Err(expect_error(&p, EventType::DocumentEnd, ev.typ));
                }
                nodes.push(new_node(NodeKind::Document, &ev));
                anchors.clear();
                stack.push(id);
                root = Some(id);
                continue;
            }
            EventType::DocumentEnd => {
                // The document is complete once its end event is consumed.
                return Ok((nodes, root));
            }
            EventType::Scalar => {
                let mut n = new_node(NodeKind::Scalar, &ev);
                n.value = go_lossy_utf8(&ev.value);
                n.tag = go_lossy_utf8(&ev.tag);
                n.implicit = ev.implicit;
                nodes.push(n);
                if !ev.anchor.is_empty() {
                    anchors.insert(ev.anchor.clone(), id);
                }
            }
            EventType::Alias => {
                let mut n = new_node(NodeKind::Alias, &ev);
                n.value = go_lossy_utf8(&ev.anchor);
                match anchors.get(&ev.anchor) {
                    Some(&t) => n.alias = Some(t),
                    None => {
                        return Err(DecodeError::new(
                            format!("unknown anchor '{}' referenced", n.value),
                            Some(ev.start_mark),
                        ));
                    }
                }
                nodes.push(n);
            }
            EventType::SequenceStart | EventType::MappingStart => {
                let kind = if ev.typ == EventType::SequenceStart {
                    NodeKind::Sequence
                } else {
                    NodeKind::Mapping
                };
                let mut n = new_node(kind, &ev);
                n.tag = go_lossy_utf8(&ev.tag);
                nodes.push(n);
                if !ev.anchor.is_empty() {
                    anchors.insert(ev.anchor.clone(), id);
                }
            }
            EventType::SequenceEnd | EventType::MappingEnd => {
                stack.pop();
                continue;
            }
            EventType::StreamStart | EventType::NoEvent => {
                return Err(expect_error(&p, EventType::DocumentStart, ev.typ));
            }
        }
        if let Some(&parent) = stack.last() {
            nodes[parent].children.push(id);
        }
        if matches!(ev.typ, EventType::SequenceStart | EventType::MappingStart) {
            stack.push(id);
            if stack.len() > MAX_DEPTH + 1 {
                return Err(DecodeError::new(
                    format!(
                        "line {}: exceeded max depth of {}",
                        ev.start_mark.line + 1,
                        MAX_DEPTH
                    ),
                    Some(ev.start_mark),
                ));
            }
        }
    }
}

fn expect_error(p: &Parser<'_>, want: EventType, got: EventType) -> DecodeError {
    let _ = p;
    DecodeError::new(
        format!(
            "expected {} event but got {}",
            want.go_name(),
            got.go_name()
        ),
        None,
    )
}

/// Go's `(*parser).fail()`.
fn parser_error(p: &Parser<'_>) -> DecodeError {
    let e = &p.sc.err;
    let mut line = 0;
    if e.problem_mark.line != 0 {
        line = e.problem_mark.line;
        if e.kind == ErrorKind::Scanner {
            line += 1;
        }
    } else if e.context_mark.line != 0 {
        line = e.context_mark.line;
    }
    let where_ = if line != 0 {
        format!("line {line}: ")
    } else {
        String::new()
    };
    let msg = if e.problem.is_empty() {
        "unknown problem parsing YAML content"
    } else {
        e.problem.as_str()
    };
    let mut err = DecodeError::new(format!("{where_}{msg}"), None);
    if e.kind == ErrorKind::Reader {
        err.offset = Some(e.problem_offset);
    } else {
        err.mark = Some(e.problem_mark);
    }
    err
}

// ---------------------------------------------------------------------------
// Go values produced by goyaml v2 when decoding into `interface{}`.

#[derive(Clone, Debug)]
pub(crate) enum GoVal {
    Nil,
    Bool(bool),
    /// Go `int`.
    Int(i64),
    Uint(u64),
    Float(f64),
    /// Go string (may be invalid UTF-8 after `!!binary`).
    Str(Vec<u8>),
    Seq(Vec<(GoVal, Mark)>),
    /// `map[interface{}]interface{}`: (key, value, key position).
    Map(Vec<(GoVal, GoVal, (Mark, Mark))>),
}

impl GoVal {
    fn is_collection(&self) -> bool {
        matches!(self, GoVal::Seq(_) | GoVal::Map(_))
    }

    /// Go type name (`reflect.TypeOf(v).String()`).
    fn type_name(&self) -> &'static str {
        match self {
            GoVal::Nil => "%!s(<nil>)",
            GoVal::Bool(_) => "bool",
            GoVal::Int(_) => "int",
            GoVal::Uint(_) => "uint64",
            GoVal::Float(_) => "float64",
            GoVal::Str(_) => "string",
            GoVal::Seq(_) => "[]interface {}",
            GoVal::Map(_) => "map[interface {}]interface {}",
        }
    }

    /// `fmt.Sprintf("%#v", v)` (top level: nil prints as `<nil>`).
    pub fn go_syntax(&self, top: bool) -> String {
        match self {
            GoVal::Nil => {
                if top {
                    "<nil>".into()
                } else {
                    "interface {}(nil)".into()
                }
            }
            GoVal::Bool(b) => b.to_string(),
            GoVal::Int(i) => i.to_string(),
            GoVal::Uint(u) => format!("0x{u:x}"),
            GoVal::Float(f) => format_float(*f, b'g', false),
            GoVal::Str(s) => quote(&go_lossy_utf8(s)),
            GoVal::Seq(items) => {
                let parts: Vec<String> = items.iter().map(|(v, _)| v.go_syntax(false)).collect();
                format!("[]interface {{}}{{{}}}", parts.join(", "))
            }
            GoVal::Map(entries) => {
                let mut sorted: Vec<&(GoVal, GoVal, (Mark, Mark))> = entries.iter().collect();
                sorted.sort_by(|a, b| a.0.fmt_cmp(&b.0));
                let parts: Vec<String> = sorted
                    .into_iter()
                    .map(|(k, v, _)| format!("{}:{}", k.go_syntax(false), v.go_syntax(false)))
                    .collect();
                format!("map[interface {{}}]interface {{}}{{{}}}", parts.join(", "))
            }
        }
    }
}

impl GoVal {
    fn type_rank(&self) -> u8 {
        match self {
            // fmtsort orders interface keys by the address of their type
            // descriptor; this is the order observed in Helm's binary.
            GoVal::Nil => 0,
            GoVal::Str(_) => 1,
            GoVal::Uint(_) => 2,
            GoVal::Int(_) => 3,
            GoVal::Float(_) => 4,
            GoVal::Bool(_) => 5,
            GoVal::Seq(_) => 6,
            GoVal::Map(_) => 7,
        }
    }

    /// Ordering used by `fmt` for map keys (`internal/fmtsort`).
    fn fmt_cmp(&self, other: &GoVal) -> core::cmp::Ordering {
        use core::cmp::Ordering;
        match (self, other) {
            (GoVal::Bool(a), GoVal::Bool(b)) => a.cmp(b),
            (GoVal::Int(a), GoVal::Int(b)) => a.cmp(b),
            (GoVal::Uint(a), GoVal::Uint(b)) => a.cmp(b),
            (GoVal::Float(a), GoVal::Float(b)) => a.partial_cmp(b).unwrap_or_else(|| {
                if a.is_nan() && !b.is_nan() {
                    Ordering::Less
                } else if !a.is_nan() && b.is_nan() {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            }),
            (GoVal::Str(a), GoVal::Str(b)) => a.cmp(b),
            _ => self.type_rank().cmp(&other.type_rank()),
        }
    }
}

/// Totally ordered stand-in for Go's `==` on scalar map keys.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum KeyRepr {
    Nil,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(u64),
    Str(Vec<u8>),
}

impl KeyRepr {
    /// `None` for keys that never compare equal (NaN) or are not scalars.
    fn of(v: &GoVal) -> Option<KeyRepr> {
        Some(match v {
            GoVal::Nil => KeyRepr::Nil,
            GoVal::Bool(b) => KeyRepr::Bool(*b),
            GoVal::Int(i) => KeyRepr::Int(*i),
            GoVal::Uint(u) => KeyRepr::Uint(*u),
            GoVal::Float(f) if f.is_nan() => return None,
            GoVal::Float(f) => KeyRepr::Float(if *f == 0.0 { 0 } else { f.to_bits() }),
            GoVal::Str(s) => KeyRepr::Str(s.clone()),
            GoVal::Seq(_) | GoVal::Map(_) => return None,
        })
    }
}

/// A Go map under construction: entries in insertion order plus an index.
#[derive(Default)]
pub(crate) struct IndexedMap {
    entries: Vec<(GoVal, GoVal, (Mark, Mark))>,
    index: BTreeMap<KeyRepr, usize>,
}

impl IndexedMap {
    fn set(&mut self, k: GoVal, v: GoVal, mark: (Mark, Mark)) {
        match KeyRepr::of(&k) {
            Some(repr) => match self.index.get(&repr) {
                Some(&i) => {
                    self.entries[i].1 = v;
                    self.entries[i].2 = mark;
                }
                None => {
                    self.index.insert(repr, self.entries.len());
                    self.entries.push((k, v, mark));
                }
            },
            None => self.entries.push((k, v, mark)),
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
    }
}

const ALIAS_RATIO_RANGE_LOW: f64 = 400000.0;
const ALIAS_RATIO_RANGE_HIGH: f64 = 4000000.0;

fn allowed_alias_ratio(decode_count: usize) -> f64 {
    let dc = decode_count as f64;
    if dc <= ALIAS_RATIO_RANGE_LOW {
        0.99
    } else if dc >= ALIAS_RATIO_RANGE_HIGH {
        0.10
    } else {
        0.99 - 0.89
            * ((dc - ALIAS_RATIO_RANGE_LOW) / (ALIAS_RATIO_RANGE_HIGH - ALIAS_RATIO_RANGE_LOW))
    }
}

pub(crate) struct Decoder<'n> {
    nodes: &'n [Node],
    aliases: Vec<usize>,
    decode_count: usize,
    alias_count: usize,
    alias_depth: usize,
    depth: usize,
}

/// What the decoder writes into: an `interface{}` or an existing map (merge).
enum Out<'a> {
    Iface(&'a mut GoVal),
    Map(&'a mut IndexedMap),
}

impl<'n> Decoder<'n> {
    pub fn new(nodes: &'n [Node]) -> Self {
        Decoder {
            nodes,
            aliases: Vec::new(),
            decode_count: 0,
            alias_count: 0,
            alias_depth: 0,
            depth: 0,
        }
    }

    #[inline(never)]
    fn fail(&self, msg: String, n: usize) -> Box<DecodeError> {
        Box::new(DecodeError::new(msg, Some(self.nodes[n].mark)))
    }

    /// `d.unmarshal(node, out)` with `out` an `interface{}`.
    pub fn decode(&mut self, n: usize) -> Result<GoVal, Box<DecodeError>> {
        let mut v = GoVal::Nil;
        self.unmarshal(n, Out::Iface(&mut v))?;
        Ok(v)
    }

    /// Bookkeeping done on every `unmarshal` call (alias ratio, depth).
    #[inline(never)]
    fn enter(&mut self, n: usize) -> Result<(), Box<DecodeError>> {
        self.decode_count += 1;
        if self.alias_depth > 0 {
            self.alias_count += 1;
        }
        if self.alias_count > 100
            && self.decode_count > 1000
            && (self.alias_count as f64) / (self.decode_count as f64)
                > allowed_alias_ratio(self.decode_count)
        {
            return Err(self.fail("document contains excessive aliasing".into(), n));
        }
        self.depth += 1;
        if self.depth > MAX_DEPTH + 2 {
            let m = self.nodes[n].mark;
            return Err(self.fail(
                format!("line {}: exceeded max depth of {}", m.line + 1, MAX_DEPTH),
                n,
            ));
        }
        Ok(())
    }

    // The functions on the recursion path keep their frames small (the
    // stack must not overflow on deeply nested input, even in debug builds).

    fn unmarshal(&mut self, n: usize, out: Out<'_>) -> Result<(), Box<DecodeError>> {
        self.enter(n)?;
        let r = match self.nodes[n].kind {
            NodeKind::Document => match self.nodes[n].children.as_slice() {
                [c] => self.unmarshal(*c, out),
                _ => Ok(()),
            },
            NodeKind::Alias => self.alias(n, out),
            NodeKind::Scalar => self.scalar_into(n, out),
            NodeKind::Sequence => self.sequence(n, out),
            NodeKind::Mapping => match out {
                Out::Iface(o) => {
                    let mut entries = IndexedMap::default();
                    let r = self.mapping_into(n, &mut entries);
                    *o = GoVal::Map(entries.entries);
                    r
                }
                Out::Map(m) => self.mapping_into(n, m),
            },
        };
        self.depth -= 1;
        r
    }

    fn alias(&mut self, n: usize, out: Out<'_>) -> Result<(), Box<DecodeError>> {
        let target = self.alias_enter(n)?;
        let Some(target) = target else { return Ok(()) };
        let r = self.unmarshal(target, out);
        self.alias_depth -= 1;
        self.aliases.retain(|&a| a != n);
        r
    }

    #[inline(never)]
    fn alias_enter(&mut self, n: usize) -> Result<Option<usize>, Box<DecodeError>> {
        let node = &self.nodes[n];
        if self.aliases.contains(&n) {
            return Err(self.fail(format!("anchor '{}' value contains itself", node.value), n));
        }
        let Some(target) = node.alias else {
            return Ok(None);
        };
        self.aliases.push(n);
        self.alias_depth += 1;
        Ok(Some(target))
    }

    #[inline(never)]
    fn scalar_into(&mut self, n: usize, out: Out<'_>) -> Result<(), Box<DecodeError>> {
        let v = self.scalar(n)?;
        match out {
            Out::Iface(o) => *o = v,
            Out::Map(m) => {
                // Decoding a scalar into a map (via merge): null resets the
                // map; anything else is a type error that Go records and
                // ignores here because merge targets are maps.
                if matches!(v, GoVal::Nil) {
                    m.clear();
                }
            }
        }
        Ok(())
    }

    fn sequence(&mut self, n: usize, out: Out<'_>) -> Result<(), Box<DecodeError>> {
        let len = self.nodes[n].children.len();
        let mut items = Vec::with_capacity(len);
        for i in 0..len {
            let c = self.nodes[n].children[i];
            let mut e = GoVal::Nil;
            self.unmarshal(c, Out::Iface(&mut e))?;
            items.push((e, self.nodes[c].mark));
        }
        if let Out::Iface(o) = out {
            *o = GoVal::Seq(items);
        }
        Ok(())
    }

    fn mapping_into(&mut self, n: usize, out: &mut IndexedMap) -> Result<(), Box<DecodeError>> {
        let len = self.nodes[n].children.len();
        let mut i = 0;
        while i + 1 < len {
            let kn = self.nodes[n].children[i];
            let vn = self.nodes[n].children[i + 1];
            i += 2;
            if self.is_merge(kn) {
                self.merge(vn, out)?;
                continue;
            }
            let mut k = GoVal::Nil;
            self.unmarshal(kn, Out::Iface(&mut k))?;
            if k.is_collection() {
                return Err(self.invalid_key(&k, kn));
            }
            let mut v = GoVal::Nil;
            self.unmarshal(vn, Out::Iface(&mut v))?;
            out.set(k, v, (self.nodes[kn].mark, self.nodes[vn].mark));
        }
        Ok(())
    }

    #[inline(never)]
    fn invalid_key(&self, k: &GoVal, kn: usize) -> Box<DecodeError> {
        self.fail(format!("invalid map key: {}", k.go_syntax(true)), kn)
    }

    fn is_merge(&self, n: usize) -> bool {
        let node = &self.nodes[n];
        node.kind == NodeKind::Scalar
            && node.value == "<<"
            && (node.implicit || node.tag == MERGE_TAG)
    }

    #[inline(never)]
    fn want_map(&self, n: usize) -> Box<DecodeError> {
        self.fail(
            "map merge requires map or sequence of maps as the value".into(),
            n,
        )
    }

    fn merge(&mut self, n: usize, out: &mut IndexedMap) -> Result<(), Box<DecodeError>> {
        match self.nodes[n].kind {
            NodeKind::Mapping => self.unmarshal(n, Out::Map(out)),
            NodeKind::Alias => {
                if let Some(t) = self.nodes[n].alias
                    && self.nodes[t].kind != NodeKind::Mapping
                {
                    return Err(self.want_map(n));
                }
                self.unmarshal(n, Out::Map(out))
            }
            NodeKind::Sequence => {
                let len = self.nodes[n].children.len();
                for i in (0..len).rev() {
                    let c = self.nodes[n].children[i];
                    let cn = &self.nodes[c];
                    if cn.kind == NodeKind::Alias {
                        if let Some(t) = cn.alias
                            && self.nodes[t].kind != NodeKind::Mapping
                        {
                            return Err(self.want_map(n));
                        }
                    } else if cn.kind != NodeKind::Mapping {
                        return Err(self.want_map(n));
                    }
                    self.unmarshal(c, Out::Map(out))?;
                }
                Ok(())
            }
            _ => Err(self.want_map(n)),
        }
    }

    #[inline(never)]
    fn scalar(&mut self, n: usize) -> Result<GoVal, Box<DecodeError>> {
        let node = &self.nodes[n];
        if node.tag.is_empty() && !node.implicit {
            return Ok(GoVal::Str(node.value.as_bytes().to_vec()));
        }
        if !resolve::resolvable_tag(&node.tag) {
            if node.tag == BINARY_TAG {
                return match resolve::base64_decode(node.value.as_bytes()) {
                    Some(data) => Ok(GoVal::Str(data)),
                    None => Err(self.fail("!!binary value contains invalid base64 data".into(), n)),
                };
            }
            return Ok(GoVal::Str(node.value.as_bytes().to_vec()));
        }
        let (rtag, r) = match resolve::resolve(&node.tag, &node.value, true) {
            Ok(x) => x,
            Err(msg) => return Err(self.fail(msg, n)),
        };
        if rtag == TIMESTAMP_TAG {
            return Ok(GoVal::Str(node.value.as_bytes().to_vec()));
        }
        Ok(match r {
            Resolved::Null => GoVal::Nil,
            Resolved::Bool(b) => GoVal::Bool(b),
            Resolved::Int(i) => GoVal::Int(i),
            Resolved::Uint(u) => GoVal::Uint(u),
            Resolved::Float(f) => GoVal::Float(f),
            Resolved::Str | Resolved::Timestamp | Resolved::Merge => {
                GoVal::Str(node.value.as_bytes().to_vec())
            }
        })
    }
}

// ---------------------------------------------------------------------------
// sigs.k8s.io/yaml: YAML object -> JSON-able object -> json.Marshal ->
// json.Unmarshal.

/// JSON-able value (string keys, sorted like `encoding/json` does).
pub(crate) enum JVal {
    Nil,
    Bool(bool),
    /// A number that `json.Marshal` can encode (or NaN/Inf, an error).
    Num(f64, Mark),
    Str(Vec<u8>),
    Seq(Vec<JVal>),
    Map(BTreeMap<Vec<u8>, JVal>),
}

/// The JSON key a YAML map key converts to (`convertToJSONableObject`), or
/// the "unsupported map key" error.
#[inline(never)]
fn json_key(k: &GoVal, v: &GoVal, m: Mark) -> Result<Vec<u8>, Box<DecodeError>> {
    Ok(match k {
        GoVal::Str(s) => s.clone(),
        GoVal::Int(i) => i.to_string().into_bytes(),
        GoVal::Float(f) => {
            let s = format_float(*f, b'g', true);
            match s.as_str() {
                "+Inf" => b".inf".to_vec(),
                "-Inf" => b"-.inf".to_vec(),
                "NaN" => b".nan".to_vec(),
                _ => s.into_bytes(),
            }
        }
        GoVal::Bool(b) => {
            if *b {
                b"true".to_vec()
            } else {
                b"false".to_vec()
            }
        }
        _ => {
            return Err(Box::new(DecodeError::new(
                format!(
                    "unsupported map key of type: {}, key: {}, value: {}",
                    k.type_name(),
                    k.go_syntax(true),
                    v.go_syntax(true)
                ),
                Some(m),
            )));
        }
    })
}

/// `convertToJSONableObject` (with no typed JSON target).
pub(crate) fn to_jsonable(v: GoVal, mark: Mark) -> Result<JVal, Box<DecodeError>> {
    Ok(match v {
        GoVal::Nil => JVal::Nil,
        GoVal::Bool(b) => JVal::Bool(b),
        GoVal::Int(i) => JVal::Num(i as f64, mark),
        GoVal::Uint(u) => JVal::Num(u as f64, mark),
        GoVal::Float(f) => JVal::Num(f, mark),
        GoVal::Str(s) => JVal::Str(s),
        GoVal::Seq(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (v, m) in items {
                out.push(to_jsonable(v, m)?);
            }
            JVal::Seq(out)
        }
        GoVal::Map(entries) => {
            let mut out = BTreeMap::new();
            for (k, v, m) in entries {
                let key = json_key(&k, &v, m.0)?;
                let jv = to_jsonable(v, m.1)?;
                out.insert(key, jv);
            }
            JVal::Map(out)
        }
    })
}

/// Whether two keys of some map convert to the same JSON key (Go then keeps
/// whichever it happens to visit last).
pub(crate) fn has_key_collisions(v: &GoVal) -> bool {
    match v {
        GoVal::Seq(items) => items.iter().any(|(v, _)| has_key_collisions(v)),
        GoVal::Map(entries) => {
            let mut seen = alloc::collections::BTreeSet::new();
            for (k, v, m) in entries {
                if let Ok(key) = json_key(k, &GoVal::Nil, m.0)
                    && !seen.insert(key)
                {
                    return true;
                }
                if has_key_collisions(v) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

#[inline(never)]
fn unsupported_float(f: f64, m: Mark) -> Box<DecodeError> {
    Box::new(DecodeError::new(
        format!("json: unsupported value: {}", format_float(f, b'g', false)),
        Some(m),
    ))
}

/// `json.Marshal` followed by `json.Unmarshal` into `interface{}`.
pub(crate) fn json_roundtrip(v: JVal) -> Result<crate::Value, Box<DecodeError>> {
    use crate::Value;
    Ok(match v {
        JVal::Nil => Value::Nil,
        JVal::Bool(b) => Value::Bool(b),
        JVal::Num(f, m) => {
            if f.is_nan() || f.is_infinite() {
                return Err(unsupported_float(f, m));
            }
            Value::Float(f)
        }
        JVal::Str(s) => Value::string(go_lossy_utf8(&s)),
        JVal::Seq(items) => {
            let mut out = Vec::with_capacity(items.len());
            for v in items {
                out.push(json_roundtrip(v)?);
            }
            Value::list(out)
        }
        JVal::Map(m) => {
            let mut out = crate::value::Map::new();
            for (k, v) in m {
                let v = json_roundtrip(v)?;
                out.insert(go_lossy_utf8(&k), v);
            }
            Value::map(out)
        }
    })
}
