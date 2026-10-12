//! `values.schema.json` validation (Helm `ValidateAgainstSchema`, on
//! `santhosh-tekuri/jsonschema/v6`): drafts 4, 6, 7, 2019-09 and 2020-12,
//! with that library's error tree and messages.
//!
//! Supported: `type`, `enum`, `const`, `$ref` (local JSON pointers and
//! anchors), `properties`, `additionalProperties`, `required`,
//! `dependencies`, `dependentRequired`, `dependentSchemas`, `propertyNames`,
//! `min/maxProperties`, `items`/`additionalItems`/`prefixItems`,
//! `contains`/`minContains`/`maxContains`, `min/maxItems`, `uniqueItems`,
//! `min/maxLength`, numeric bounds and `multipleOf`, `allOf`/`anyOf`/
//! `oneOf`/`not`, `if`/`then`/`else`, boolean schemas.
//! Not supported: `pattern`/`patternProperties` (no regex engine yet),
//! `format`, `unevaluated*`, `$dynamicRef`/`$recursiveRef`, remote refs and
//! metaschema validation of the schema itself.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use crate::funcs::sprig::goregex::{self, GoRegex};
use crate::value::{MapRef, Value};

use super::Chart;

const ROOT_URL: &str = "file:///values.schema.json";

// ---------------------------------------------------------------- JSON --

#[derive(Clone, Debug, PartialEq)]
enum J {
    Null,
    Bool(bool),
    /// Number with its source text (Go `json.Number`).
    Num(String, f64),
    Str(String),
    Arr(Vec<J>),
    Obj(BTreeMap<String, J>),
}

struct JParser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

fn quote_char(c: u8) -> String {
    match c {
        b'\'' => "'\\''".into(),
        b'"' => "'\"'".into(),
        c if c.is_ascii_graphic() || c == b' ' => alloc::format!("'{}'", c as char),
        c => alloc::format!("'\\x{c:02x}'"),
    }
}

impl JParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn err_at(&self, ctx: &str) -> String {
        match self.s.get(self.i) {
            None => "unexpected EOF".into(),
            Some(c) => alloc::format!("invalid character {} {ctx}", quote_char(*c)),
        }
    }

    fn value(&mut self) -> Result<J, String> {
        self.ws();
        self.depth += 1;
        if self.depth > 512 {
            return Err("exceeded max depth".into());
        }
        let r = match self.s.get(self.i) {
            None => Err("unexpected EOF".into()),
            Some(b'{') => {
                self.i += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        if self.s.get(self.i) != Some(&b'"') {
                            return Err(self.err_at("looking for beginning of object key string"));
                        }
                        let k = self.string()?;
                        self.ws();
                        if self.s.get(self.i) != Some(&b':') {
                            return Err(self.err_at("after object key"));
                        }
                        self.i += 1;
                        let v = self.value()?;
                        m.insert(k, v);
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err_at("after object key:value pair")),
                        }
                    }
                }
                Ok(J::Obj(m))
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        a.push(self.value()?);
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err_at("after array element")),
                        }
                    }
                }
                Ok(J::Arr(a))
            }
            Some(b'"') => self.string().map(J::Str),
            Some(b't') => self.lit("true", J::Bool(true)),
            Some(b'f') => self.lit("false", J::Bool(false)),
            Some(b'n') => self.lit("null", J::Null),
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => Err(self.err_at("looking for beginning of value")),
        };
        self.depth -= 1;
        r
    }

    fn lit(&mut self, word: &str, v: J) -> Result<J, String> {
        for (k, &b) in word.as_bytes().iter().enumerate() {
            match self.s.get(self.i) {
                Some(c) if *c == b => self.i += 1,
                None => return Err("unexpected EOF".into()),
                Some(_) => {
                    return Err(self.err_at(&alloc::format!("in literal {word} (expecting {})", quote_char(word.as_bytes()[k]))));
                }
            }
        }
        Ok(v)
    }

    fn number(&mut self) -> Result<J, String> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.i < p.s.len() && p.s[p.i].is_ascii_digit() {
                p.i += 1;
            }
            p.i > s
        };
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        if self.s.get(self.i) == Some(&b'0') {
            self.i += 1;
        } else if !digits(self) {
            return Err(self.err_at("in numeric literal"));
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return Err(self.err_at("after decimal point in numeric literal"));
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(self.err_at("in exponent of numeric literal"));
            }
        }
        let text = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        let f = text.parse::<f64>().unwrap_or(f64::NAN);
        Ok(J::Num(text, f))
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else { return Err("unexpected EOF".into()) };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                b'\\' => {
                    self.i += 1;
                    let Some(&e) = self.s.get(self.i) else { return Err("unexpected EOF".into()) };
                    self.i += 1;
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let hex = |p: &mut Self| -> Option<u32> {
                                let h = p.s.get(p.i..p.i + 4)?;
                                let v = u32::from_str_radix(core::str::from_utf8(h).ok()?, 16).ok()?;
                                p.i += 4;
                                Some(v)
                            };
                            let Some(mut cp) = hex(self) else {
                                return Err(self.err_at("in \\u hexadecimal character escape"));
                            };
                            if (0xD800..0xDC00).contains(&cp) && self.s.get(self.i..self.i + 2) == Some(b"\\u") {
                                let save = self.i;
                                self.i += 2;
                                match hex(self) {
                                    Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    }
                                    _ => self.i = save,
                                }
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => {
                            self.i -= 1;
                            return Err(self.err_at("in string escape code"));
                        }
                    }
                }
                c if c < 0x20 => return Err(self.err_at("in string literal")),
                c => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }
}

/// `jsonschema.UnmarshalJSON`.
fn parse_json(s: &[u8]) -> Result<J, String> {
    let mut p = JParser { s, i: 0, depth: 0 };
    p.ws();
    if p.i == s.len() {
        return Err("EOF".into());
    }
    let v = p.value()?;
    p.ws();
    if p.i != s.len() {
        return Err("invalid character after top-level value".into());
    }
    Ok(v)
}

// -------------------------------------------------------------- schema --

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Ty {
    Null,
    Boolean,
    Number,
    Integer,
    String,
    Array,
    Object,
}

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::Null => "null",
            Ty::Boolean => "boolean",
            Ty::Number => "number",
            Ty::Integer => "integer",
            Ty::String => "string",
            Ty::Array => "array",
            Ty::Object => "object",
        }
    }
    fn parse(s: &str) -> Option<Ty> {
        Some(match s {
            "null" => Ty::Null,
            "boolean" => Ty::Boolean,
            "number" => Ty::Number,
            "integer" => Ty::Integer,
            "string" => Ty::String,
            "array" => Ty::Array,
            "object" => Ty::Object,
            _ => return None,
        })
    }
}

/// A JSON number kept exactly (Go `json.Number` / `big.Rat`).
#[derive(Clone, Debug)]
struct Num {
    text: String,
    f: f64,
}

#[derive(Clone)]
enum Items {
    One(usize),
    Many(Vec<usize>),
}

#[derive(Clone)]
enum Dep {
    Props(Vec<String>),
    Schema(usize),
}

#[derive(Clone, Default)]
struct Node {
    location: String,
    boolean: Option<bool>,
    draft: u32,
    types: Option<Vec<Ty>>,
    konst: Option<J>,
    enumv: Option<Vec<J>>,
    reference: Option<usize>,
    properties: Vec<(String, usize)>,
    pattern_properties: Vec<(Rc<GoRegex>, usize)>,
    pattern: Option<(String, Rc<GoRegex>)>,
    format: Option<String>,
    additional_props: Option<Result<bool, usize>>,
    required: Vec<String>,
    dependencies: Vec<(String, Dep)>,
    dependent_required: Vec<(String, Vec<String>)>,
    dependent_schemas: Vec<(String, usize)>,
    property_names: Option<usize>,
    min_props: Option<usize>,
    max_props: Option<usize>,
    items: Option<Items>,
    additional_items: Option<Result<bool, usize>>,
    prefix_items: Vec<usize>,
    items2020: Option<usize>,
    contains: Option<usize>,
    min_contains: Option<usize>,
    max_contains: Option<usize>,
    min_items: Option<usize>,
    max_items: Option<usize>,
    unique_items: bool,
    min_length: Option<usize>,
    max_length: Option<usize>,
    minimum: Option<Num>,
    maximum: Option<Num>,
    exclusive_minimum: Option<Num>,
    exclusive_maximum: Option<Num>,
    multiple_of: Option<Num>,
    not: Option<usize>,
    all_of: Vec<usize>,
    any_of: Vec<usize>,
    one_of: Vec<usize>,
    if_: Option<usize>,
    then: Option<usize>,
    else_: Option<usize>,
}

struct Compiler<'a> {
    root: &'a J,
    draft: u32,
    nodes: Vec<Node>,
    by_ptr: BTreeMap<String, usize>,
    anchors: BTreeMap<String, String>,
}

fn escape_tok(t: &str) -> String {
    t.replace('~', "~0").replace('/', "~1")
}

fn unescape_tok(t: &str) -> String {
    t.replace("~1", "/").replace("~0", "~")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(core::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn lookup<'j>(root: &'j J, ptr: &str) -> Option<&'j J> {
    let mut cur = root;
    if ptr.is_empty() {
        return Some(cur);
    }
    for tok in ptr.strip_prefix('/')?.split('/') {
        let tok = unescape_tok(tok);
        cur = match cur {
            J::Obj(m) => m.get(&tok)?,
            J::Arr(a) => a.get(tok.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn draft_of(root: &J) -> u32 {
    let J::Obj(m) = root else { return 2020 };
    match m.get("$schema") {
        Some(J::Str(s)) if s.contains("draft-04") => 4,
        Some(J::Str(s)) if s.contains("draft-06") => 6,
        Some(J::Str(s)) if s.contains("draft-07") => 7,
        Some(J::Str(s)) if s.contains("2019-09") => 2019,
        _ => 2020,
    }
}

fn collect_anchors(j: &J, ptr: &str, draft: u32, out: &mut BTreeMap<String, String>) {
    match j {
        J::Obj(m) => {
            if let Some(J::Str(a)) = m.get("$anchor") {
                out.insert(a.clone(), ptr.into());
            }
            if draft <= 7 {
                if let Some(J::Str(id)) = m.get(if draft == 4 { "id" } else { "$id" }) {
                    if let Some(a) = id.strip_prefix('#') {
                        out.insert(a.into(), ptr.into());
                    }
                }
            }
            for (k, v) in m {
                if matches!(k.as_str(), "enum" | "const" | "default" | "examples") {
                    continue;
                }
                collect_anchors(v, &alloc::format!("{ptr}/{}", escape_tok(k)), draft, out);
            }
        }
        J::Arr(a) => {
            for (i, v) in a.iter().enumerate() {
                collect_anchors(v, &alloc::format!("{ptr}/{i}"), draft, out);
            }
        }
        _ => {}
    }
}

fn to_usize(j: Option<&J>) -> Option<usize> {
    match j {
        Some(J::Num(_, f)) if *f >= 0.0 && int_f64(*f) => Some(*f as usize),
        _ => None,
    }
}

fn num(j: Option<&J>) -> Option<Num> {
    match j {
        Some(J::Num(t, f)) => Some(Num { text: t.clone(), f: *f }),
        _ => None,
    }
}

fn strings(j: Option<&J>) -> Vec<String> {
    match j {
        Some(J::Arr(a)) => a.iter().filter_map(|v| if let J::Str(s) = v { Some(s.clone()) } else { None }).collect(),
        _ => Vec::new(),
    }
}

impl Compiler<'_> {
    fn compile(&mut self, ptr: &str) -> Result<usize, String> {
        if let Some(&i) = self.by_ptr.get(ptr) {
            return Ok(i);
        }
        let Some(j) = lookup(self.root, ptr) else {
            return Err(alloc::format!("JSON pointer {ptr:?} not found"));
        };
        let idx = self.nodes.len();
        self.nodes.push(Node::default());
        self.by_ptr.insert(ptr.into(), idx);
        let mut n = Node { location: alloc::format!("{ROOT_URL}#{ptr}"), draft: self.draft, ..Node::default() };
        let m = match j {
            J::Bool(b) => {
                n.boolean = Some(*b);
                self.nodes[idx] = n;
                return Ok(idx);
            }
            J::Obj(m) => m,
            _ => return Err(alloc::format!("{:?} is not a valid schema", n.location)),
        };
        let draft = self.draft;
        let sub = |c: &mut Self, kw: &str| -> Result<Option<usize>, String> {
            if m.contains_key(kw) {
                c.compile(&alloc::format!("{ptr}/{}", escape_tok(kw))).map(Some)
            } else {
                Ok(None)
            }
        };
        let arr = |c: &mut Self, kw: &str| -> Result<Vec<usize>, String> {
            match m.get(kw) {
                Some(J::Arr(a)) => {
                    (0..a.len()).map(|i| c.compile(&alloc::format!("{ptr}/{}/{i}", escape_tok(kw)))).collect()
                }
                _ => Ok(Vec::new()),
            }
        };
        let map = |c: &mut Self, kw: &str| -> Result<Vec<(String, usize)>, String> {
            match m.get(kw) {
                Some(J::Obj(o)) => o
                    .keys()
                    .map(|k| {
                        c.compile(&alloc::format!("{ptr}/{}/{}", escape_tok(kw), escape_tok(k))).map(|i| (k.clone(), i))
                    })
                    .collect(),
                _ => Ok(Vec::new()),
            }
        };
        let additional = |c: &mut Self, kw: &str| -> Result<Option<Result<bool, usize>>, String> {
            match m.get(kw) {
                None => Ok(None),
                Some(J::Bool(b)) => Ok(Some(Ok(*b))),
                Some(_) => c.compile(&alloc::format!("{ptr}/{kw}")).map(|i| Some(Err(i))),
            }
        };
        if let Some(J::Str(r)) = m.get("$ref") {
            n.reference = Some(self.resolve_ref(r)?);
            if draft < 2019 {
                self.nodes[idx] = n;
                return Ok(idx);
            }
        }
        // applicator
        n.all_of = arr(self, "allOf")?;
        n.any_of = arr(self, "anyOf")?;
        n.one_of = arr(self, "oneOf")?;
        n.not = sub(self, "not")?;
        if draft < 2020 {
            match m.get("items") {
                Some(J::Arr(_)) => {
                    n.items = Some(Items::Many(arr(self, "items")?));
                    n.additional_items = additional(self, "additionalItems")?;
                }
                Some(_) => n.items = sub(self, "items")?.map(Items::One),
                None => {}
            }
        }
        n.properties = map(self, "properties")?;
        if let Some(J::Obj(pp)) = m.get("patternProperties") {
            for k in pp.keys() {
                let idx = self.compile(&alloc::format!("{ptr}/patternProperties/{}", escape_tok(k)))?;
                let re = goregex::compile(k).map_err(|e| {
                    alloc::format!("invalid regex {} at {}: {e}", super::gostr::quote(k), super::gostr::quote(&alloc::format!("{ROOT_URL}#{ptr}/patternProperties")))
                })?;
                n.pattern_properties.push((Rc::new(re), idx));
            }
        }
        n.additional_props = additional(self, "additionalProperties")?;
        if let Some(J::Obj(deps)) = m.get("dependencies") {
            for (k, v) in deps {
                let d = match v {
                    J::Arr(_) => Dep::Props(strings(Some(v))),
                    _ => Dep::Schema(self.compile(&alloc::format!("{ptr}/dependencies/{}", escape_tok(k)))?),
                };
                n.dependencies.push((k.clone(), d));
            }
        }
        // validation
        match m.get("type") {
            Some(J::Str(t)) => n.types = Some(Ty::parse(t).into_iter().collect()),
            Some(J::Arr(a)) => {
                n.types = Some(a.iter().filter_map(|t| if let J::Str(t) = t { Ty::parse(t) } else { None }).collect())
            }
            Some(_) => {
                return Err(alloc::format!(
                    "\"{ROOT_URL}#\" is not valid against metaschema: jsonschema validation failed with 'https://json-schema.org/draft/2020-12/schema#'\n- at '': 'allOf' failed\n  - at '{}/type': 'anyOf' failed\n    - at '{}/type': value must be one of 'array', 'boolean', 'integer', 'null', 'number', 'object', 'string'\n    - at '{}/type': got {}, want array",
                    ptr, ptr, ptr, json_type_name(m.get("type"))
                ));
            }
            None => {}
        }
        if let Some(J::Arr(a)) = m.get("enum") {
            n.enumv = Some(a.clone());
        }
        n.multiple_of = num(m.get("multipleOf"));
        n.maximum = num(m.get("maximum"));
        if matches!(m.get("exclusiveMaximum"), Some(J::Bool(true))) {
            n.exclusive_maximum = n.maximum.take();
        } else {
            n.exclusive_maximum = num(m.get("exclusiveMaximum"));
        }
        n.minimum = num(m.get("minimum"));
        if matches!(m.get("exclusiveMinimum"), Some(J::Bool(true))) {
            n.exclusive_minimum = n.minimum.take();
        } else {
            n.exclusive_minimum = num(m.get("exclusiveMinimum"));
        }
        n.min_length = to_usize(m.get("minLength"));
        if let Some(J::Str(pat)) = m.get("pattern") {
            let re = goregex::compile(pat).map_err(|e| {
                alloc::format!("invalid regex {} at {}: {e}", super::gostr::quote(pat), super::gostr::quote(&alloc::format!("{ROOT_URL}#{ptr}/pattern")))
            })?;
            n.pattern = Some((pat.clone(), Rc::new(re)));
        }
        if draft < 2019 {
            if let Some(J::Str(f)) = m.get("format") {
                n.format = Some(f.clone());
            }
        }
        n.max_length = to_usize(m.get("maxLength"));
        n.min_items = to_usize(m.get("minItems"));
        n.max_items = to_usize(m.get("maxItems"));
        n.unique_items = matches!(m.get("uniqueItems"), Some(J::Bool(true)));
        n.max_props = to_usize(m.get("maxProperties"));
        n.min_props = to_usize(m.get("minProperties"));
        n.required = strings(m.get("required"));
        if draft >= 6 {
            n.contains = sub(self, "contains")?;
            n.property_names = sub(self, "propertyNames")?;
            if let Some(v) = m.get("const") {
                n.konst = Some(v.clone());
            }
        }
        if draft >= 7 {
            n.if_ = sub(self, "if")?;
            if n.if_.is_some() {
                let b = match m.get("if") {
                    Some(J::Bool(b)) => Some(*b),
                    _ => None,
                };
                if b != Some(false) {
                    n.then = sub(self, "then")?;
                }
                if b != Some(true) {
                    n.else_ = sub(self, "else")?;
                }
            }
        }
        if draft >= 2019 {
            if n.contains.is_some() {
                n.min_contains = to_usize(m.get("minContains"));
                n.max_contains = to_usize(m.get("maxContains"));
            }
            if let Some(J::Obj(o)) = m.get("dependentRequired") {
                for (k, v) in o {
                    if let J::Arr(_) = v {
                        n.dependent_required.push((k.clone(), strings(Some(v))));
                    }
                }
            }
            n.dependent_schemas = map(self, "dependentSchemas")?;
        }
        if draft >= 2020 {
            n.prefix_items = arr(self, "prefixItems")?;
            n.items2020 = sub(self, "items")?;
        }
        self.nodes[idx] = n;
        Ok(idx)
    }

    fn resolve_ref(&mut self, r: &str) -> Result<usize, String> {
        let frag = r.strip_prefix(ROOT_URL).unwrap_or(r);
        let Some(frag) = frag.strip_prefix('#') else {
            if frag.is_empty() {
                return self.compile("");
            }
            if frag.starts_with("urn:") {
                return self.compile_true();
            }
            return Err(alloc::format!("failing loading {r:?}: unsupported reference"));
        };
        let frag = percent_decode(frag);
        if frag.is_empty() || frag.starts_with('/') {
            return self.compile(&frag);
        }
        match self.anchors.get(&frag).cloned() {
            Some(p) => self.compile(&p),
            None => Err(alloc::format!("anchor {frag:?} not found in {ROOT_URL:?}")),
        }
    }

    fn compile_true(&mut self) -> Result<usize, String> {
        let idx = self.nodes.len();
        self.nodes.push(Node { boolean: Some(true), location: "urn".into(), ..Node::default() });
        Ok(idx)
    }
}

fn json_type_name(j: Option<&J>) -> &'static str {
    match j {
        Some(J::Null) | None => "null",
        Some(J::Bool(_)) => "boolean",
        Some(J::Num(..)) => "number",
        Some(J::Str(_)) => "string",
        Some(J::Arr(_)) => "array",
        Some(J::Obj(_)) => "object",
    }
}

// ------------------------------------------------------------ validate --

enum Kind {
    Schema,
    Group,
    Reference,
    Msg(String),
}

struct VErr {
    loc: Vec<String>,
    kind: Kind,
    causes: Vec<VErr>,
}

impl VErr {
    fn skip(&self) -> bool {
        self.causes.len() == 1 && matches!(self.kind, Kind::Reference)
    }

    fn display(&self, sb: &mut String, mut indent: usize) {
        if !self.skip() {
            if indent > 0 {
                sb.push('\n');
                for _ in 0..indent - 1 {
                    sb.push_str("  ");
                }
                sb.push_str("- ");
            }
            indent += 1;
            let msg = match &self.kind {
                Kind::Schema => alloc::format!("jsonschema validation failed with {}", squote(&alloc::format!("{ROOT_URL}#"))),
                Kind::Group | Kind::Reference => "validation failed".into(),
                Kind::Msg(m) => m.clone(),
            };
            if matches!(self.kind, Kind::Schema) {
                sb.push_str(&msg);
            } else {
                let ptr: String = self.loc.iter().map(|t| alloc::format!("/{}", escape_tok(t))).collect();
                let _ = write!(sb, "at {}: {msg}", squote(&ptr));
            }
        }
        for c in &self.causes {
            c.display(sb, indent);
        }
    }
}

/// The library's `quote`: Go `%q` with single quotes.
fn squote(s: &str) -> String {
    let q = super::gostr::quote(s);
    let inner = &q[1..q.len() - 1];
    alloc::format!("'{}'", inner.replace("\\\"", "\"").replace('\'', "\\'"))
}

fn display_j(j: &J) -> String {
    match j {
        J::Str(s) => squote(s),
        J::Arr(_) | J::Obj(_) => "value".into(),
        J::Null => "<nil>".into(),
        J::Bool(b) => alloc::format!("{b}"),
        J::Num(t, _) => t.clone(),
    }
}

/// `%d` through `golang.org/x/text/message` (English): digit grouping.
fn g(n: usize) -> String {
    group(&n.to_string())
}

fn group(digits: &str) -> String {
    let mut out = String::new();
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `%v` of a float64 through `golang.org/x/text/message` (English):
/// shortest digits, grouped, or `d.ddd×10ⁿⁿ` when the exponent is < -4 or >= 6.
fn fmt_f64(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "∞".into() } else { "-∞".into() };
    }
    let neg = f.is_sign_negative() && f != 0.0;
    let e = alloc::format!("{:e}", if neg { -f } else { f });
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if f != 0.0 && !(-4..6).contains(&exp) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push_str("×10");
        if exp < 0 {
            out.push('⁻');
        }
        let a = alloc::format!("{:02}", exp.unsigned_abs());
        for c in a.chars() {
            out.push(match c {
                '0' => '⁰',
                '1' => '¹',
                '2' => '²',
                '3' => '³',
                '4' => '⁴',
                '5' => '⁵',
                '6' => '⁶',
                '7' => '⁷',
                '8' => '⁸',
                _ => '⁹',
            });
        }
    } else if exp < 0 {
        out.push_str("0.");
        for _ in 0..(-exp - 1) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let point = exp as usize + 1;
        let (int, frac) = if digits.len() <= point {
            (alloc::format!("{digits}{}", "0".repeat(point - digits.len())), String::new())
        } else {
            (digits[..point].to_string(), digits[point..].to_string())
        };
        out.push_str(&group(&int));
        if !frac.is_empty() {
            out.push('.');
            out.push_str(&frac);
        }
    }
    out
}

fn type_of(v: &Value) -> Option<Ty> {
    Some(match v {
        Value::Nil => Ty::Null,
        Value::Bool(_) => Ty::Boolean,
        Value::Float(f) if !f.is_finite() => return None,
        Value::Int(_) | Value::Int64(_) | Value::Uint64(_) | Value::Float(_) => Ty::Number,
        Value::String(_) => Ty::String,
        Value::List(_) => Ty::Array,
        Value::Map(_) => Ty::Object,
        _ => return None,
    })
}

fn int_f64(f: f64) -> bool {
    f.is_finite() && (f.abs() >= 9.007_199_254_740_992e15 || f == (f as i64) as f64)
}

fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Float(f) => *f,
        Value::Int(i) | Value::Int64(i) => *i as f64,
        Value::Uint64(u) => *u as f64,
        _ => f64::NAN,
    }
}

fn is_integer(v: &Value) -> bool {
    match v {
        Value::Int(_) | Value::Int64(_) | Value::Uint64(_) => true,
        Value::Float(f) => int_f64(*f),
        _ => false,
    }
}

fn equals(v: &Value, j: &J) -> bool {
    match (v, j) {
        (Value::Nil, J::Null) => true,
        (Value::Bool(a), J::Bool(b)) => a == b,
        (Value::String(a), J::Str(b)) => **a == **b,
        (Value::List(a), J::Arr(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| equals(x, y)),
        (Value::Map(a), J::Obj(b)) => {
            let a = a.borrow();
            a.len() == b.len() && a.iter().all(|(k, x)| b.get(k).is_some_and(|y| equals(x, y)))
        }
        (v, J::Num(_, f)) if type_of(v) == Some(Ty::Number) => as_f64(v) == *f,
        _ => false,
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::List(x), Value::List(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| values_equal(p, q)),
        (Value::Map(x), Value::Map(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| values_equal(v, w)))
        }
        _ if type_of(a) == Some(Ty::Number) && type_of(b) == Some(Ty::Number) => as_f64(a) == as_f64(b),
        _ => a == b,
    }
}

/// Exact decimal `(mantissa, exponent10)` of a number's text.
fn decimal(text: &str) -> Option<(i128, i32)> {
    let t = text.trim_start_matches('-');
    let (mant, exp) = match t.find(['e', 'E']) {
        Some(i) => (&t[..i], t[i + 1..].parse::<i32>().ok()?),
        None => (t, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let digits = alloc::format!("{int}{frac}");
    let digits = digits.trim_start_matches('0');
    let m: i128 = if digits.is_empty() { 0 } else { digits.parse().ok()? };
    Some((m, exp - frac.len() as i32))
}

fn is_multiple(v: f64, of: &Num) -> bool {
    let vt = alloc::format!("{v:e}");
    if let (Some((a, ea)), Some((b, eb))) = (decimal(&vt), decimal(&of.text)) {
        if b == 0 {
            return false;
        }
        let pow = |e: i32| -> Option<i128> { 10i128.checked_pow(u32::try_from(e).ok()?) };
        if ea >= eb {
            if let Some(scaled) = pow(ea - eb).and_then(|p| a.checked_mul(p)) {
                return scaled % b == 0;
            }
        } else if let Some(den) = pow(eb - ea).and_then(|p| b.checked_mul(p)) {
            return a % den == 0;
        }
    }
    let q = v / of.f;
    int_f64(q)
}

#[derive(Clone)]
struct Scope<'a> {
    node: usize,
    ref_kw: &'static str,
    vid: usize,
    parent: Option<&'a Scope<'a>>,
}

impl Scope<'_> {
    fn kw_loc(&self, nodes: &[Node]) -> String {
        let mut loc = String::new();
        let mut sc = self;
        while let Some(p) = sc.parent {
            if !sc.ref_kw.is_empty() {
                loc = alloc::format!("/{}{loc}", sc.ref_kw);
            } else {
                let cur = &nodes[sc.node].location;
                let par = &nodes[p.node].location;
                loc = alloc::format!("{}{loc}", cur.strip_prefix(par.as_str()).unwrap_or(cur));
            }
            sc = p;
        }
        loc
    }
}

struct Validator<'a> {
    nodes: &'a [Node],
    steps: usize,
}

const MAX_STEPS: usize = 2_000_000;

impl Validator<'_> {
    fn err(loc: &[String], kind: Kind) -> VErr {
        VErr { loc: loc.to_vec(), kind, causes: Vec::new() }
    }

    fn msg(loc: &[String], m: String) -> VErr {
        Self::err(loc, Kind::Msg(m))
    }

    fn validate(&mut self, v: &Value, loc: &mut Vec<String>, scope: &Scope<'_>) -> Result<(), VErr> {
        self.steps += 1;
        if self.steps > MAX_STEPS || loc.len() > 256 {
            return Err(Self::msg(loc, "validation too complex".into()));
        }
        let s = &self.nodes[scope.node];
        if let Some(b) = s.boolean {
            return if b { Ok(()) } else { Err(Self::msg(loc, "false schema".into())) };
        }
        // cycle check
        let mut p = scope.parent;
        while let Some(sc) = p {
            if sc.vid != scope.vid {
                break;
            }
            if sc.node == scope.node {
                return Err(Self::msg(
                    loc,
                    alloc::format!(
                        "both {} and {} resolve to {} causing reference cycle",
                        scope.kw_loc(self.nodes),
                        sc.kw_loc(self.nodes),
                        super::gostr::quote(&s.location)
                    ),
                ));
            }
            p = sc.parent;
        }
        let Some(t) = type_of(v) else {
            return Err(Self::msg(loc, alloc::format!("invalid jsonType {}", v.type_name())));
        };
        if let Some(types) = &s.types {
            if !types.is_empty() {
                let matched = types.contains(&t) || (types.contains(&Ty::Integer) && t == Ty::Number && is_integer(v));
                if !matched {
                    let mut want = types.clone();
                    want.sort();
                    want.dedup();
                    let want: Vec<&str> = want.iter().map(|t| t.name()).collect();
                    return Err(Self::msg(loc, alloc::format!("got {}, want {}", t.name(), want.join(" or "))));
                }
            }
        }
        if let Some(c) = &s.konst {
            if !equals(v, c) {
                let m = match c {
                    J::Arr(_) | J::Obj(_) => "'const' failed".into(),
                    c => alloc::format!("value must be {}", display_j(c)),
                };
                return Err(Self::msg(loc, m));
            }
        }
        if let Some(e) = &s.enumv {
            if !e.iter().any(|item| equals(v, item)) {
                let all_prim = e.iter().all(|i| !matches!(i, J::Arr(_) | J::Obj(_)));
                let m = if !all_prim {
                    "'enum' failed".into()
                } else if e.len() == 1 {
                    alloc::format!("value must be {}", display_j(&e[0]))
                } else {
                    let w: Vec<String> = e.iter().map(display_j).collect();
                    alloc::format!("value must be one of {}", w.join(", "))
                };
                return Err(Self::msg(loc, m));
            }
        }
        if let (Some(f), Value::String(st)) = (&s.format, v) {
            if let Err(e) = super::formats::check(f, st) {
                return Err(Self::msg(loc, alloc::format!("{} is not valid {f}: {e}", squote(st))));
            }
        }
        let mut errors: Vec<VErr> = Vec::new();
        if let Some(r) = s.reference {
            let res = self.validate_ref(v, loc, scope, r, "$ref");
            if s.draft < 2019 {
                return res;
            }
            if let Err(e) = res {
                errors.push(e);
            }
        }
        match v {
            Value::Map(m) => self.obj_validate(s, m, loc, scope, &mut errors),
            Value::List(l) => self.arr_validate(s, l, loc, scope, &mut errors),
            Value::String(st) => {
                let n = st.chars().count();
                if let Some(min) = s.min_length {
                    if n < min {
                        errors.push(Self::msg(loc, alloc::format!("minLength: got {}, want {}", g(n), g(min))));
                    }
                }
                if let Some(max) = s.max_length {
                    if n > max {
                        errors.push(Self::msg(loc, alloc::format!("maxLength: got {}, want {}", g(n), g(max))));
                    }
                }
                if let Some((src, re)) = &s.pattern {
                    if !re.is_match(st) {
                        errors.push(Self::msg(loc, alloc::format!("{} does not match pattern {}", squote(st), squote(src))));
                    }
                }
            }
            _ if t == Ty::Number => {
                let x = as_f64(v);
                let fx = fmt_f64(x);
                let mut check = |bound: &Option<Num>, name: &str, bad: fn(f64, f64) -> bool| {
                    if let Some(b) = bound {
                        if bad(x, b.f) {
                            errors.push(Self::msg(loc, alloc::format!("{name}: got {fx}, want {}", fmt_f64(b.f))));
                        }
                    }
                };
                check(&s.minimum, "minimum", |x, b| x < b);
                check(&s.maximum, "maximum", |x, b| x > b);
                check(&s.exclusive_minimum, "exclusiveMinimum", |x, b| x <= b);
                check(&s.exclusive_maximum, "exclusiveMaximum", |x, b| x >= b);
                if let Some(mo) = &s.multiple_of {
                    if !is_multiple(x, mo) {
                        errors.push(Self::msg(loc, alloc::format!("multipleOf: got {fx}, want {}", fmt_f64(mo.f))));
                    }
                }
            }
            _ => {}
        }
        self.cond_validate(s, v, loc, scope, &mut errors);
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors.pop().unwrap_or_else(|| Self::err(loc, Kind::Group))),
            _ => {
                let mut g = Self::err(loc, Kind::Group);
                g.causes = errors;
                Err(g)
            }
        }
    }

    fn validate_self(&mut self, v: &Value, loc: &mut Vec<String>, scope: &Scope<'_>, node: usize, kw: &'static str) -> Result<(), VErr> {
        let child = Scope { node, ref_kw: kw, vid: scope.vid, parent: Some(scope) };
        self.validate(v, loc, &child)
    }

    fn validate_val(&mut self, v: &Value, loc: &mut Vec<String>, scope: &Scope<'_>, node: usize, tok: String) -> Result<(), VErr> {
        let child = Scope { node, ref_kw: "", vid: scope.vid + 1, parent: Some(scope) };
        loc.push(tok);
        let r = self.validate(v, loc, &child);
        loc.pop();
        r
    }

    fn validate_ref(&mut self, v: &Value, loc: &mut Vec<String>, scope: &Scope<'_>, node: usize, kw: &'static str) -> Result<(), VErr> {
        match self.validate_self(v, loc, scope, node, kw) {
            Ok(()) => Ok(()),
            Err(e) => {
                let mut r = Self::err(loc, Kind::Reference);
                if matches!(e.kind, Kind::Group) {
                    r.causes = e.causes;
                } else {
                    r.causes.push(e);
                }
                Err(r)
            }
        }
    }

    fn missing(obj: &MapRef, reqd: &[String]) -> Vec<String> {
        reqd.iter().filter(|p| !obj.contains_key(p)).cloned().collect()
    }

    fn obj_validate(&mut self, s: &Node, obj: &MapRef, loc: &mut Vec<String>, scope: &Scope<'_>, errors: &mut Vec<VErr>) {
        let len = obj.len();
        if let Some(min) = s.min_props {
            if len < min {
                errors.push(Self::msg(loc, alloc::format!("minProperties: got {}, want {}", g(len), g(min))));
            }
        }
        if let Some(max) = s.max_props {
            if len > max {
                errors.push(Self::msg(loc, alloc::format!("maxProperties: got {}, want {}", g(len), g(max))));
            }
        }
        if !s.required.is_empty() {
            let missing = Self::missing(obj, &s.required);
            if !missing.is_empty() {
                errors.push(Self::msg(loc, required_msg(&missing)));
            }
        }
        for (pname, dep) in &s.dependencies {
            if obj.contains_key(pname) {
                match dep {
                    Dep::Props(p) => {
                        let missing = Self::missing(obj, p);
                        if !missing.is_empty() {
                            errors.push(Self::msg(loc, alloc::format!(
                                "properties {} required, if {} exists",
                                join_quoted(&missing),
                                squote(pname)
                            )));
                        }
                    }
                    Dep::Schema(n) => {
                        if let Err(e) = self.validate_self(&Value::Map(obj.clone()), loc, scope, *n, "") {
                            errors.push(e);
                        }
                    }
                }
            }
        }
        let entries: Vec<(String, Value)> = obj.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut additional = Vec::new();
        for (pname, pvalue) in &entries {
            let mut evaluated = false;
            if let Some((_, n)) = s.properties.iter().find(|(k, _)| k == pname) {
                evaluated = true;
                if let Err(e) = self.validate_val(pvalue, loc, scope, *n, pname.clone()) {
                    errors.push(e);
                }
            }
            for (re, n) in &s.pattern_properties {
                if re.is_match(pname) {
                    evaluated = true;
                    if let Err(e) = self.validate_val(pvalue, loc, scope, *n, pname.clone()) {
                        errors.push(e);
                    }
                }
            }
            if !evaluated {
                match &s.additional_props {
                    Some(Ok(false)) => additional.push(pname.clone()),
                    Some(Err(n)) => {
                        if let Err(e) = self.validate_val(pvalue, loc, scope, *n, pname.clone()) {
                            errors.push(e);
                        }
                    }
                    _ => {}
                }
            }
        }
        if !additional.is_empty() {
            errors.push(Self::msg(loc, alloc::format!("additional properties {} not allowed", join_quoted(&additional))));
        }
        if s.draft == 4 {
            return;
        }
        if let Some(pn) = s.property_names {
            for (pname, _) in &entries {
                // Validated as a separate instance (fresh location and scope).
                let child = Scope { node: pn, ref_kw: "", vid: 0, parent: None };
                if let Err(e) = self.validate(&Value::str(pname), &mut Vec::new(), &child) {
                    let causes = if matches!(e.kind, Kind::Group) { e.causes } else { alloc::vec![e] };
                    let mut pe = Self::msg(loc, alloc::format!("invalid propertyName {}", squote(pname)));
                    pe.causes = causes;
                    errors.push(pe);
                }
            }
        }
        if s.draft == 6 {
            return;
        }
        for (pname, n) in &s.dependent_schemas {
            if obj.contains_key(pname) {
                if let Err(e) = self.validate_self(&Value::Map(obj.clone()), loc, scope, *n, "") {
                    errors.push(e);
                }
            }
        }
        for (pname, reqd) in &s.dependent_required {
            if obj.contains_key(pname) {
                let missing = Self::missing(obj, reqd);
                if !missing.is_empty() {
                    errors.push(Self::msg(loc, alloc::format!(
                        "properties {} required, if {} exists",
                        join_quoted(&missing),
                        squote(pname)
                    )));
                }
            }
        }
    }

    fn arr_validate(&mut self, s: &Node, arr: &[Value], loc: &mut Vec<String>, scope: &Scope<'_>, errors: &mut Vec<VErr>) {
        let len = arr.len();
        if let Some(min) = s.min_items {
            if len < min {
                errors.push(Self::msg(loc, alloc::format!("minItems: got {}, want {}", g(len), g(min))));
            }
        }
        if let Some(max) = s.max_items {
            if len > max {
                errors.push(Self::msg(loc, alloc::format!("maxItems: got {}, want {}", g(len), g(max))));
            }
        }
        if s.unique_items && len > 1 {
            'outer: for i in 1..len {
                for j in 0..i {
                    if values_equal(&arr[i], &arr[j]) {
                        errors.push(Self::msg(loc, alloc::format!("items at {} and {} are equal", g(j), g(i))));
                        break 'outer;
                    }
                }
            }
        }
        if s.draft < 2020 {
            let mut evaluated = 0;
            match &s.items {
                Some(Items::One(n)) => {
                    for (i, item) in arr.iter().enumerate() {
                        if let Err(e) = self.validate_val(item, loc, scope, *n, i.to_string()) {
                            errors.push(e);
                        }
                    }
                    evaluated = len;
                }
                Some(Items::Many(ns)) => {
                    let min = len.min(ns.len());
                    for (i, item) in arr[..min].iter().enumerate() {
                        if let Err(e) = self.validate_val(item, loc, scope, ns[i], i.to_string()) {
                            errors.push(e);
                        }
                    }
                    evaluated = min;
                }
                None => {}
            }
            match &s.additional_items {
                Some(Ok(false)) if evaluated != len => {
                    errors.push(Self::msg(loc, alloc::format!("last {} additionalItem(s) not allowed", g(len - evaluated))));
                }
                Some(Err(n)) => {
                    for (i, item) in arr[evaluated..].iter().enumerate() {
                        if let Err(e) = self.validate_val(item, loc, scope, *n, (evaluated + i).to_string()) {
                            errors.push(e);
                        }
                    }
                }
                _ => {}
            }
        } else {
            let evaluated = s.prefix_items.len().min(len);
            for (i, item) in arr[..evaluated].iter().enumerate() {
                if let Err(e) = self.validate_val(item, loc, scope, s.prefix_items[i], i.to_string()) {
                    errors.push(e);
                }
            }
            if let Some(n) = s.items2020 {
                for (i, item) in arr[evaluated..].iter().enumerate() {
                    if let Err(e) = self.validate_val(item, loc, scope, n, (evaluated + i).to_string()) {
                        errors.push(e);
                    }
                }
            }
        }
        if let Some(c) = s.contains {
            let mut errs = Vec::new();
            let mut matched = Vec::new();
            for (i, item) in arr.iter().enumerate() {
                match self.validate_val(item, loc, scope, c, i.to_string()) {
                    Err(e) => errs.push(e),
                    Ok(()) => matched.push(i),
                }
            }
            let list = |m: &[usize]| m.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(" ");
            if let Some(min) = s.min_contains {
                if matched.len() < min {
                    let msg = if matched.is_empty() {
                        alloc::format!("min {} items required to match contains schema, but none matched", g(min))
                    } else {
                        alloc::format!(
                            "min {} items required to match contains schema, but matched {} items at {}",
                            g(min),
                            g(matched.len()),
                            list(&matched)
                        )
                    };
                    let mut e = Self::msg(loc, msg);
                    e.causes = errs;
                    errors.push(e);
                }
            } else if matched.is_empty() {
                let mut e = Self::msg(loc, "no items match contains schema".into());
                e.causes = errs;
                errors.push(e);
            }
            if let Some(max) = s.max_contains {
                if matched.len() > max {
                    errors.push(Self::msg(loc, alloc::format!(
                        "max {} items required to match contains schema, but matched {} items at {}",
                        g(max),
                        g(matched.len()),
                        list(&matched)
                    )));
                }
            }
        }
    }

    fn cond_validate(&mut self, s: &Node, v: &Value, loc: &mut Vec<String>, scope: &Scope<'_>, errors: &mut Vec<VErr>) {
        if let Some(n) = s.not {
            if self.validate_self(v, loc, scope, n, "").is_ok() {
                errors.push(Self::msg(loc, "'not' failed".into()));
            }
        }
        if !s.all_of.is_empty() {
            let errs: Vec<VErr> = s.all_of.iter().filter_map(|n| self.validate_self(v, loc, scope, *n, "").err()).collect();
            if !errs.is_empty() {
                let mut e = Self::msg(loc, "'allOf' failed".into());
                e.causes = errs;
                errors.push(e);
            }
        }
        if !s.any_of.is_empty() {
            let mut errs = Vec::new();
            let mut matched = false;
            for n in &s.any_of {
                match self.validate_self(v, loc, scope, *n, "") {
                    Err(e) => errs.push(e),
                    Ok(()) => {
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                let mut e = Self::msg(loc, "'anyOf' failed".into());
                e.causes = errs;
                errors.push(e);
            }
        }
        if !s.one_of.is_empty() {
            let mut matched: Option<usize> = None;
            let mut errs = Vec::new();
            for (i, n) in s.one_of.iter().enumerate() {
                match self.validate_self(v, loc, scope, *n, "") {
                    Err(e) => {
                        if matched.is_none() {
                            errs.push(e);
                        }
                    }
                    Ok(()) => match matched {
                        None => matched = Some(i),
                        Some(m) => {
                            errors.push(Self::msg(loc, alloc::format!("'oneOf' failed, subschemas {m}, {i} matched")));
                            break;
                        }
                    },
                }
            }
            if matched.is_none() {
                let mut e = Self::msg(loc, "'oneOf' failed, none matched".into());
                e.causes = errs;
                errors.push(e);
            }
        }
        if let Some(i) = s.if_ {
            if self.validate_self(v, loc, scope, i, "").is_ok() {
                if let Some(t) = s.then {
                    if let Err(e) = self.validate_self(v, loc, scope, t, "") {
                        errors.push(e);
                    }
                }
            } else if let Some(e) = s.else_ {
                if let Err(e) = self.validate_self(v, loc, scope, e, "") {
                    errors.push(e);
                }
            }
        }
    }
}

fn required_msg(missing: &[String]) -> String {
    if missing.len() == 1 {
        alloc::format!("missing property {}", squote(&missing[0]))
    } else {
        alloc::format!("missing properties {}", join_quoted(missing))
    }
}

fn join_quoted(a: &[String]) -> String {
    a.iter().map(|s| squote(s)).collect::<Vec<_>>().join(", ")
}

// ------------------------------------------------------ metaschema --
//
// The library validates a schema against its draft's metaschema before
// compiling it. We model only the checks that can fail on otherwise
// well-formed schemas in practice: `pattern` and `patternProperties` keys
// must be valid regexes (`format: regex`). The error tree mirrors the
// metaschema's structure (vocabulary `allOf` in 2019-09/2020-12, `$ref`
// recursion, `anyOf` branches) so messages match.

fn meta_url(draft: u32) -> &'static str {
    match draft {
        4 => "http://json-schema.org/draft-04/schema#",
        6 => "http://json-schema.org/draft-06/schema#",
        7 => "http://json-schema.org/draft-07/schema#",
        2019 => "https://json-schema.org/draft/2019-09/schema#",
        _ => "https://json-schema.org/draft/2020-12/schema#",
    }
}

fn tok_loc(ptr: &str) -> Vec<String> {
    if ptr.is_empty() {
        return Vec::new();
    }
    ptr[1..].split('/').map(unescape_tok).collect()
}

fn combine(loc: &str, mut errs: Vec<VErr>) -> Option<VErr> {
    match errs.len() {
        0 => None,
        1 => errs.pop(),
        _ => Some(VErr { loc: tok_loc(loc), kind: Kind::Group, causes: errs }),
    }
}

fn reference(loc: &str, e: VErr) -> VErr {
    let causes = if matches!(e.kind, Kind::Group) { e.causes } else { alloc::vec![e] };
    VErr { loc: tok_loc(loc), kind: Kind::Reference, causes }
}

fn mmsg(loc: &str, m: String) -> VErr {
    VErr { loc: tok_loc(loc), kind: Kind::Msg(m), causes: Vec::new() }
}

fn regex_err(s: &str) -> Option<String> {
    goregex::compile(s).err().map(|e| alloc::format!("{} is not valid regex: {e}", squote(s)))
}

fn json_kind_name(j: &J) -> &'static str {
    json_type_name(Some(j))
}

/// Result of validating a schema value at `loc` against the metaschema
/// (`{"$ref": "#"}` / `{"$dynamicRef": "#meta"}` target).
fn meta_schema(j: &J, loc: &str, draft: u32) -> Option<VErr> {
    let J::Obj(m) = j else { return None };
    let child = |kw: &str| alloc::format!("{loc}/{}", escape_tok(kw));
    // A `$ref`/`$dynamicRef` to the metaschema from `at`.
    let sub = |v: &J, at: &str| meta_schema(v, at, draft).map(|e| reference(at, e));
    // `{type: object, additionalProperties: <schema ref>}` values.
    let schema_map = |kw: &str, extra: &mut Vec<VErr>| -> Option<VErr> {
        let at = child(kw);
        let J::Obj(o) = m.get(kw)? else { return None };
        let mut errs: Vec<VErr> = o
            .iter()
            .filter_map(|(k, v)| sub(v, &alloc::format!("{at}/{}", escape_tok(k))))
            .collect();
        errs.append(extra);
        combine(&at, errs)
    };
    let schema_array = |kw: &str| -> Option<VErr> {
        let at = child(kw);
        let J::Arr(a) = m.get(kw)? else { return None };
        let errs = a.iter().enumerate().filter_map(|(i, v)| sub(v, &alloc::format!("{at}/{i}"))).collect();
        combine(&at, errs)
    };
    let single = |kw: &str| -> Option<VErr> { sub(m.get(kw)?, &child(kw)) };
    let pattern = || -> Option<VErr> {
        let J::Str(p) = m.get("pattern")? else { return None };
        regex_err(p).map(|e| mmsg(&child("pattern"), e))
    };
    let prop_names = || -> Vec<VErr> {
        let Some(J::Obj(o)) = m.get("patternProperties") else { return Vec::new() };
        if draft == 4 {
            return Vec::new();
        }
        o.keys()
            .filter_map(|k| {
                regex_err(k).map(|e| VErr {
                    loc: tok_loc(&child("patternProperties")),
                    kind: Kind::Msg(alloc::format!("invalid propertyName {}", squote(k))),
                    causes: alloc::vec![mmsg("", e)],
                })
            })
            .collect()
    };
    // `anyOf: [<schema ref>, <array>]` style keywords.
    let any_of_schema_or_array = |kw: &str, arr_ok: bool| -> Option<VErr> {
        let at = child(kw);
        let v = m.get(kw)?;
        let (branch1, branch2) = match v {
            J::Arr(a) if arr_ok => {
                let want = if draft == 4 { "object" } else { "object or boolean" };
                let t = mmsg(&at, alloc::format!("got array, want {want}"));
                let errs: Vec<VErr> =
                    a.iter().enumerate().filter_map(|(i, x)| sub(x, &alloc::format!("{at}/{i}"))).collect();
                if errs.is_empty() {
                    return None;
                }
                (Some(t), combine(&at, errs))
            }
            J::Arr(_) => return None,
            other => {
                let e = sub(other, &at)?;
                (Some(e), Some(mmsg(&at, alloc::format!("got {}, want array", json_kind_name(other)))))
            }
        };
        Some(VErr {
            loc: tok_loc(&at),
            kind: Kind::Msg("'anyOf' failed".into()),
            causes: branch1.into_iter().chain(branch2).collect(),
        })
    };
    let dependencies = || -> Option<VErr> {
        let at = child("dependencies");
        let J::Obj(o) = m.get("dependencies")? else { return None };
        let errs = o
            .iter()
            .filter_map(|(k, v)| {
                let kat = alloc::format!("{at}/{}", escape_tok(k));
                if matches!(v, J::Arr(_)) {
                    return None;
                }
                let e = sub(v, &kat)?;
                Some(VErr {
                    loc: tok_loc(&kat),
                    kind: Kind::Msg("'anyOf' failed".into()),
                    causes: alloc::vec![e, mmsg(&kat, alloc::format!("got {}, want array", json_kind_name(v)))],
                })
            })
            .collect();
        combine(&at, errs)
    };
    let mut errs: Vec<VErr> = Vec::new();
    if draft < 2019 {
        // One monolithic metaschema; keyword errors in key order.
        for kw in m.keys() {
            let e = match kw.as_str() {
                "properties" | "definitions" => schema_map(kw, &mut Vec::new()),
                "patternProperties" => schema_map(kw, &mut prop_names()),
                "additionalProperties" | "additionalItems" if draft == 4 => {
                    let v = &m[kw];
                    if matches!(v, J::Bool(_)) {
                        None
                    } else {
                        sub(v, &child(kw)).map(|e| VErr {
                            loc: tok_loc(&child(kw)),
                            kind: Kind::Msg("'anyOf' failed".into()),
                            causes: alloc::vec![mmsg(&child(kw), alloc::format!("got {}, want boolean", json_kind_name(v))), e],
                        })
                    }
                }
                "additionalProperties" | "additionalItems" | "not" | "contains" | "propertyNames" | "if" | "then"
                | "else" => single(kw),
                "items" => any_of_schema_or_array(kw, true),
                "allOf" | "anyOf" | "oneOf" => schema_array(kw),
                "dependencies" => dependencies(),
                "pattern" => pattern(),
                _ => None,
            };
            errs.extend(e);
        }
        return combine(loc, errs);
    }
    // 2019-09 / 2020-12: top-level compat keywords, then the vocabularies.
    for kw in m.keys() {
        match kw.as_str() {
            "definitions" => errs.extend(schema_map(kw, &mut Vec::new())),
            "dependencies" => errs.extend(dependencies()),
            _ => {}
        }
    }
    let mut core = Vec::new();
    let mut applicator = Vec::new();
    let mut unevaluated = Vec::new();
    let mut validation = Vec::new();
    for kw in m.keys() {
        match kw.as_str() {
            "$defs" => core.extend(schema_map(kw, &mut Vec::new())),
            "properties" | "dependentSchemas" => applicator.extend(schema_map(kw, &mut Vec::new())),
            "patternProperties" => applicator.extend(schema_map(kw, &mut prop_names())),
            "items" if draft == 2019 => applicator.extend(any_of_schema_or_array(kw, true)),
            "additionalProperties" | "additionalItems" | "items" | "contains" | "propertyNames" | "if" | "then"
            | "else" | "not" => applicator.extend(single(kw)),
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => applicator.extend(schema_array(kw)),
            "unevaluatedItems" | "unevaluatedProperties" => unevaluated.extend(single(kw)),
            "pattern" => validation.extend(pattern()),
            _ => {}
        }
    }
    let vocab_errs: Vec<VErr> = [core, applicator, unevaluated, validation]
        .into_iter()
        .filter_map(|v| combine(loc, v).map(|e| reference(loc, e)))
        .collect();
    if !vocab_errs.is_empty() {
        errs.push(VErr { loc: tok_loc(loc), kind: Kind::Msg("'allOf' failed".into()), causes: vocab_errs });
    }
    combine(loc, errs)
}

/// The metaschema check, with the library's error text.
fn meta_check(root: &J, draft: u32) -> Result<(), String> {
    let Some(e) = meta_schema(root, "", draft) else { return Ok(()) };
    let causes = if matches!(e.kind, Kind::Group) { e.causes } else { alloc::vec![e] };
    let mut s = alloc::format!("jsonschema validation failed with {}", squote(meta_url(draft)));
    for c in &causes {
        c.display(&mut s, 1);
    }
    Err(alloc::format!("\"{ROOT_URL}#\" is not valid against metaschema: {s}"))
}

/// Validates `values` against a single JSON schema document
/// (`ValidateAgainstSingleSchema`). Returns Helm's error text.
pub fn validate_against_schema(values: &MapRef, schema: &[u8]) -> Result<(), String> {
    let root = parse_json(schema)?;
    let draft = draft_of(&root);
    meta_check(&root, draft)?;
    let mut anchors = BTreeMap::new();
    collect_anchors(&root, "", draft, &mut anchors);
    let mut c = Compiler { root: &root, draft, nodes: Vec::new(), by_ptr: BTreeMap::new(), anchors };
    let root_idx = c.compile("")?;
    let nodes = c.nodes;
    let mut vd = Validator { nodes: &nodes, steps: 0 };
    let scope = Scope { node: root_idx, ref_kw: "", vid: 0, parent: None };
    let v = Value::Map(values.clone());
    if let Err(e) = vd.validate(&v, &mut Vec::new(), &scope) {
        let causes = if matches!(e.kind, Kind::Group) { e.causes } else { alloc::vec![e] };
        let root_err = VErr { loc: Vec::new(), kind: Kind::Schema, causes };
        let mut s = String::new();
        root_err.display(&mut s, 0);
        let prefix = alloc::format!("jsonschema validation failed with {}\n", squote(&alloc::format!("{ROOT_URL}#")));
        let s = s.strip_prefix(prefix.as_str()).unwrap_or(&s).to_string();
        return Err(alloc::format!("{s}\n"));
    }
    Ok(())
}

/// `ValidateAgainstSchema`: the chart's schema and its subcharts'.
pub(crate) fn validate_chart(chart: &Chart, values: &MapRef) -> Result<(), String> {
    let mut sb = String::new();
    if let Some(schema) = &chart.schema {
        if let Err(e) = validate_against_schema(values, schema) {
            let _ = write!(sb, "{}:\n{e}", chart.name());
        }
    }
    for sub in &chart.dependencies {
        match values.get(sub.name()) {
            None | Some(Value::Nil) => {}
            Some(Value::Map(m)) => {
                if let Err(e) = validate_chart(sub, &m) {
                    sb.push_str(&e);
                }
            }
            Some(other) => {
                let _ = write!(
                    sb,
                    "{}:\ninvalid type for values: expected object (map), got {}\n",
                    sub.name(),
                    other.type_name()
                );
            }
        }
    }
    if sb.is_empty() { Ok(()) } else { Err(sb) }
}
