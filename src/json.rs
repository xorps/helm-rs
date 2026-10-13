//! JSON compatible with Go's `encoding/json` (`toJson`, `fromJson`).
//!
//! Encoding matches `json.Marshal`: map keys sorted, HTML characters escaped
//! as `<` etc. (unless using [`to_string_raw`]), U+2028/U+2029
//! escaped, Go's float formatting, `[]byte` as base64, NaN/Inf rejected.
//! Decoding matches `json.Unmarshal` into `interface{}` (numbers become
//! `float64`), including Go's syntax error messages.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::value::{Map, MapRef, Value};
use crate::yaml::strconv::{format_float, parse_float, quote};
use crate::yaml::{MAX_DEPTH, Target};

/// A JSON error, with Go's message and the byte offset (as Go's
/// `SyntaxError.Offset`: bytes read when the error was detected).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// Message as Go's `encoding/json` prints it.
    pub message: String,
    /// Byte offset of the error.
    pub offset: usize,
}

impl JsonError {
    fn new(message: String, offset: usize) -> Self {
        JsonError { message, offset }
    }
}

impl core::fmt::Display for JsonError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

impl core::error::Error for JsonError {}

// ---------------------------------------------------------------------------
// Encoding.

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Go's `appendString` (JSON string quoting).
pub(crate) fn append_string(out: &mut String, s: &str, escape_html: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' if escape_html => {
                let b = c as u8;
                out.push_str("\\u00");
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xF) as usize] as char);
            }
            c if (c as u32) < 0x20 => {
                let b = c as u8;
                out.push_str("\\u00");
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xF) as usize] as char);
            }
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Go's float encoder for `float64`.
pub(crate) fn format_json_float(f: f64) -> Result<String, JsonError> {
    if f.is_nan() || f.is_infinite() {
        return Err(JsonError::new(
            format!("json: unsupported value: {}", format_float(f, b'g', false)),
            0,
        ));
    }
    let abs = f.abs();
    let fmt = if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        b'e'
    } else {
        b'f'
    };
    let mut s = format_float(f, fmt, false);
    if fmt == b'e' {
        // clean up e-09 to e-9
        let b = s.as_bytes();
        let n = b.len();
        if n >= 4 && b[n - 4] == b'e' && b[n - 3] == b'-' && b[n - 2] == b'0' {
            let last = b[n - 1] as char;
            s.truncate(n - 2);
            s.push(last);
        }
    }
    Ok(s)
}

enum PathEnt {
    Map(MapRef),
    List(Rc<Vec<Value>>),
}

impl PathEnt {
    fn same(&self, v: &Value) -> bool {
        match (self, v) {
            (PathEnt::Map(a), Value::Map(b)) => a.ptr_eq(b),
            (PathEnt::List(a), Value::List(b)) => Rc::ptr_eq(a, b) && a.len() == b.len(),
            _ => false,
        }
    }
    fn go_type(&self) -> &'static str {
        match self {
            PathEnt::Map(_) => "map[string]interface {}",
            PathEnt::List(_) => "[]interface {}",
        }
    }
}

struct Encoder {
    out: String,
    escape_html: bool,
    /// Maps and lists on the current path (Go's `ptrLevel`/`ptrSeen`).
    path: Vec<PathEnt>,
    /// Object nesting (objects do not count as pointer levels).
    objects: usize,
}

/// Go's `startDetectingCyclesAfter`: Go only notices a cycle once the
/// nesting exceeds this, and then reports the container found at that
/// level. We detect cycles immediately and report the same container.
const START_DETECTING_CYCLES_AFTER: usize = 1000;

impl Encoder {
    /// Enters a map or list; detects cycles and excessive depth.
    fn enter(&mut self, v: &Value) -> Result<(), JsonError> {
        if let Some(s) = self.path.iter().position(|p| p.same(v)) {
            let n = self.path.len();
            let period = n - s;
            let idx = s + (START_DETECTING_CYCLES_AFTER - s) % period;
            let ty = self.path[idx].go_type();
            return Err(JsonError::new(
                format!("json: unsupported value: encountered a cycle via {ty}"),
                0,
            ));
        }
        if self.path.len() + self.objects >= MAX_DEPTH {
            return Err(JsonError::new(
                format!("json: unsupported value: exceeded max depth of {MAX_DEPTH}"),
                0,
            ));
        }
        self.path.push(match v {
            Value::Map(m) => PathEnt::Map(m.clone()),
            Value::List(l) => PathEnt::List(l.clone()),
            _ => return Ok(()),
        });
        Ok(())
    }

    #[inline(never)]
    fn scalar(&mut self, v: &Value) -> Result<(), Box<JsonError>> {
        match v {
            Value::Nil => self.out.push_str("null"),
            Value::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) | Value::Int64(i) => self.out.push_str(&format!("{i}")),
            Value::Uint64(u) => self.out.push_str(&format!("{u}")),
            Value::Float(f) => {
                let s = format_json_float(*f).map_err(Box::new)?;
                self.out.push_str(&s);
            }
            Value::String(s) => append_string(&mut self.out, s, self.escape_html),
            Value::Bytes(b) => {
                self.out.push('"');
                self.out.push_str(&crate::yaml::base64_encode(b));
                self.out.push('"');
            }
            _ => {}
        }
        Ok(())
    }

    #[inline(never)]
    fn map_entries(m: &MapRef) -> Vec<(String, Value)> {
        m.borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    #[inline(never)]
    fn key(&mut self, k: &str) {
        append_string(&mut self.out, k, self.escape_html);
        self.out.push(':');
    }

    #[inline(never)]
    fn object_enter(&mut self) -> Result<(), Box<JsonError>> {
        if self.path.len() + self.objects >= MAX_DEPTH {
            return Err(Box::new(JsonError::new(
                format!("json: unsupported value: exceeded max depth of {MAX_DEPTH}"),
                0,
            )));
        }
        self.objects += 1;
        Ok(())
    }

    // On the recursion path: keep the frame small.
    fn encode(&mut self, v: &Value) -> Result<(), Box<JsonError>> {
        match v {
            Value::List(items) => {
                self.enter(v).map_err(Box::new)?;
                self.out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.out.push(',');
                    }
                    self.encode(item)?;
                }
                self.out.push(']');
                self.path.pop();
            }
            Value::Map(m) => {
                self.enter(v).map_err(Box::new)?;
                let entries = Self::map_entries(m);
                self.out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        self.out.push(',');
                    }
                    self.key(k);
                    self.encode(v)?;
                }
                self.out.push('}');
                self.path.pop();
            }
            Value::Object(o) => {
                self.object_enter()?;
                if let Some(gs) = o.go_struct() {
                    // Struct: declaration order and json tags.
                    self.out.push('{');
                    let mut first = true;
                    for f in &gs.fields {
                        let (name, omit) = match f.json {
                            crate::value::JsonField::Skip => continue,
                            crate::value::JsonField::Name(n) => (n, false),
                            crate::value::JsonField::OmitEmpty(n) => (n, true),
                        };
                        if omit && is_empty_value(&f.value) {
                            continue;
                        }
                        if !first {
                            self.out.push(',');
                        }
                        first = false;
                        self.key(name);
                        self.encode(&f.value)?;
                    }
                    self.out.push('}');
                } else {
                    let data = o.to_data();
                    self.encode(&data)?;
                }
                self.objects -= 1;
            }
            _ => self.scalar(v)?,
        }
        Ok(())
    }
}

fn marshal(v: &Value, escape_html: bool) -> Result<String, JsonError> {
    let mut e = Encoder {
        out: String::new(),
        escape_html,
        path: Vec::new(),
        objects: 0,
    };
    e.encode(v).map_err(|e| *e)?;
    Ok(e.out)
}

/// `json.Marshal(v)`: sorted keys, HTML-escaped `<`, `>`, `&`, Go float format.
pub fn to_string(v: &Value) -> Result<String, JsonError> {
    marshal(v, true)
}

/// A `json.Encoder` with `SetEscapeHTML(false)`, without the trailing
/// newline (Sprig's `toRawJson`).
pub fn to_string_raw(v: &Value) -> Result<String, JsonError> {
    marshal(v, false)
}

/// `json.MarshalIndent(v, prefix, indent)` (Sprig's `toPrettyJson` uses
/// `"", "  "`).
pub fn to_string_indent(v: &Value, prefix: &str, indent: &str) -> Result<String, JsonError> {
    let compact = marshal(v, true)?;
    Ok(indent_json(&compact, prefix, indent))
}

/// Go's `appendIndent` applied to valid compact JSON.
fn indent_json(src: &str, prefix: &str, indent: &str) -> String {
    let mut out = String::with_capacity(src.len() * 2);
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(prefix);
        for _ in 0..depth {
            out.push_str(indent);
        }
    };
    let mut need_indent = false;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for c in src.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
            continue;
        }
        if need_indent && c != '}' && c != ']' {
            need_indent = false;
            depth += 1;
            newline(&mut out, depth);
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                need_indent = true;
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => {
                out.push(c);
                out.push(' ');
            }
            '}' | ']' => {
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
    out
}

// ---------------------------------------------------------------------------
// Decoding. Go 1.27 implements `encoding/json` on top of `encoding/json/v2`
// (`jsontext`), validating the whole input first and reporting syntax errors
// with v1-style wording (`transformSyntacticError`). This is a port of that
// validator (`consumeValue` & co. with legacy flags).

enum SynErr {
    /// `io.ErrUnexpectedEOF`.
    Eof,
    /// An `InvalidTextError` message (before legacy rewriting) and offset.
    Text(String, usize),
    /// `errMaxDepth`.
    MaxDepth(usize),
}

/// `jsonwire.QuoteRune` (the input is valid UTF-8).
fn quote_rune(b: &[u8]) -> String {
    let s = core::str::from_utf8(b).unwrap_or("");
    let c = s.chars().next().unwrap_or('\u{FFFD}');
    match c {
        '\'' => "'\\''".into(),
        '"' => "'\"'".into(),
        _ => {
            let q = quote(&String::from(c));
            format!("'{}'", &q[1..q.len() - 1])
        }
    }
}

fn first_char_len(b: &[u8]) -> usize {
    match b.first() {
        None => 0,
        Some(&c) if c < 0x80 => 1,
        Some(&c) if c >= 0xF0 => 4.min(b.len()),
        Some(&c) if c >= 0xE0 => 3.min(b.len()),
        Some(_) => 2.min(b.len()),
    }
}

fn invalid_char(b: &[u8], at: usize, where_: &str) -> SynErr {
    let rest = &b[at.min(b.len())..];
    let n = first_char_len(rest);
    SynErr::Text(
        format!("invalid character {} {}", quote_rune(&rest[..n]), where_),
        at,
    )
}

fn needs_escape(s: &str) -> bool {
    s.chars().any(|r| {
        r == '`' || r == '\u{FFFD}' || r.is_whitespace() || !crate::yaml::isprint::is_print(r)
    })
}

fn invalid_escape(what: &[u8], at: usize) -> SynErr {
    let s = crate::yaml::strconv::go_lossy_utf8(what);
    let label = if what.len() > 6 {
        "surrogate pair"
    } else {
        "escape sequence"
    };
    let shown = if s.chars().count() == 1 {
        quote_rune(s.as_bytes())
    } else if needs_escape(&s) {
        crate::yaml::strconv::quote_bytes(what)
    } else {
        format!("`{s}`")
    };
    SynErr::Text(format!("invalid {label} {shown} in string"), at)
}

fn is_ws(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\r' || c == b'\n'
}

fn skip_ws(b: &[u8], mut pos: usize) -> usize {
    while pos < b.len() && is_ws(b[pos]) {
        pos += 1;
    }
    pos
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

/// `jsonwire.hasEscapedUTF16Prefix`.
fn has_escaped_utf16_prefix(b: &[u8], lower_surrogate_half: bool) -> bool {
    for (i, &c) in b.iter().enumerate() {
        if i == 0 && c != b'\\' {
            return false;
        }
        if i == 1 && c != b'u' {
            return false;
        }
        if i == 2 && lower_surrogate_half && c != b'd' && c != b'D' {
            return false;
        }
        if i == 3
            && lower_surrogate_half
            && !(b'c'..=b'f').contains(&c)
            && !(b'C'..=b'F').contains(&c)
        {
            return false;
        }
        if (2..6).contains(&i) && !is_hex(c) {
            return false;
        }
    }
    true
}

/// `consumeString` (no UTF-8 validation, as with `AllowInvalidUTF8`).
fn consume_string(b: &[u8], pos: usize) -> Result<usize, SynErr> {
    if pos >= b.len() {
        return Err(SynErr::Eof);
    }
    if b[pos] != b'"' {
        return Err(invalid_char(b, pos, "at start of string (expecting '\"')"));
    }
    let mut n = pos + 1;
    while n < b.len() {
        let c = b[n];
        if c == b'"' {
            return Ok(n + 1);
        }
        if c >= 0x80 {
            n += first_char_len(&b[n..]).max(1);
            continue;
        }
        if c == b'\\' {
            if n + 2 > b.len() {
                return Err(SynErr::Eof);
            }
            match b[n + 1] {
                b'/' | b'"' | b'\\' | b'b' | b'f' | b'n' | b'r' | b't' => n += 2,
                b'u' => {
                    if n + 6 > b.len() {
                        if has_escaped_utf16_prefix(&b[n..], false) {
                            return Err(SynErr::Eof);
                        }
                        return Err(invalid_escape(&b[n..], n));
                    }
                    if !b[n + 2..n + 6].iter().all(|&c| is_hex(c)) {
                        return Err(invalid_escape(&b[n..n + 6], n));
                    }
                    n += 6;
                }
                _ => {
                    let end = (n + 2).min(b.len());
                    return Err(invalid_escape(&b[n..end], n));
                }
            }
            continue;
        }
        if c < b' ' {
            return Err(invalid_char(
                b,
                n,
                "in string (expecting non-control character)",
            ));
        }
        n += 1;
    }
    Err(SynErr::Eof)
}

/// `ConsumeNumberResumable` over a complete buffer.
fn consume_number(b: &[u8], pos: usize) -> Result<usize, SynErr> {
    let mut n = pos;
    if n < b.len() && b[n] == b'-' {
        n += 1;
    }
    let digit = |n: usize| n < b.len() && b[n].is_ascii_digit();
    if n >= b.len() {
        return Err(SynErr::Eof);
    }
    if b[n] == b'0' {
        n += 1;
    } else if (b'1'..=b'9').contains(&b[n]) {
        n += 1;
        while digit(n) {
            n += 1;
        }
    } else {
        return Err(invalid_char(b, n, "in number (expecting digit)"));
    }
    if n < b.len() && b[n] == b'.' {
        n += 1;
        if n >= b.len() {
            return Err(SynErr::Eof);
        }
        if !digit(n) {
            return Err(invalid_char(b, n, "in number (expecting digit)"));
        }
        while digit(n) {
            n += 1;
        }
    }
    if n < b.len() && (b[n] == b'e' || b[n] == b'E') {
        n += 1;
        if n < b.len() && (b[n] == b'-' || b[n] == b'+') {
            n += 1;
        }
        if n >= b.len() {
            return Err(SynErr::Eof);
        }
        if !digit(n) {
            return Err(invalid_char(b, n, "in number (expecting digit)"));
        }
        while digit(n) {
            n += 1;
        }
    }
    Ok(n)
}

fn consume_literal(b: &[u8], pos: usize, lit: &str) -> Result<usize, SynErr> {
    let l = lit.as_bytes();
    for i in 0..l.len() {
        if pos + i >= b.len() {
            return Err(SynErr::Eof);
        }
        if b[pos + i] != l[i] {
            let msg = format!("in literal {lit} (expecting {})", quote_rune(&l[i..i + 1]));
            return Err(invalid_char(b, pos + i, &msg));
        }
    }
    Ok(pos + l.len())
}

fn consume_value(b: &[u8], pos: usize, depth: usize) -> Result<usize, SynErr> {
    if pos >= b.len() {
        return Err(SynErr::Eof);
    }
    match b[pos] {
        b'n' => consume_literal(b, pos, "null"),
        b'f' => consume_literal(b, pos, "false"),
        b't' => consume_literal(b, pos, "true"),
        b'"' => consume_string(b, pos),
        b'-' | b'0'..=b'9' => consume_number(b, pos),
        b'{' => consume_object(b, pos, depth),
        b'[' => consume_array(b, pos, depth),
        _ => Err(invalid_char(b, pos, "at start of value")),
    }
}

fn consume_object(b: &[u8], pos: usize, depth: usize) -> Result<usize, SynErr> {
    if depth == MAX_DEPTH + 1 {
        return Err(SynErr::MaxDepth(pos));
    }
    let mut pos = skip_ws(b, pos + 1);
    if pos >= b.len() {
        return Err(SynErr::Eof);
    }
    if b[pos] == b'}' {
        return Ok(pos + 1);
    }
    let depth = depth + 1;
    loop {
        pos = skip_ws(b, pos);
        pos = consume_string(b, pos)?;
        pos = skip_ws(b, pos);
        if pos >= b.len() {
            return Err(SynErr::Eof);
        }
        if b[pos] != b':' {
            return Err(invalid_char(b, pos, "after object name (expecting ':')"));
        }
        pos = skip_ws(b, pos + 1);
        pos = consume_value(b, pos, depth)?;
        pos = skip_ws(b, pos);
        if pos >= b.len() {
            return Err(SynErr::Eof);
        }
        match b[pos] {
            b',' => pos += 1,
            b'}' => return Ok(pos + 1),
            _ => {
                return Err(invalid_char(
                    b,
                    pos,
                    "after object value (expecting ',' or '}')",
                ));
            }
        }
    }
}

fn consume_array(b: &[u8], pos: usize, depth: usize) -> Result<usize, SynErr> {
    if depth == MAX_DEPTH + 1 {
        return Err(SynErr::MaxDepth(pos));
    }
    let mut pos = skip_ws(b, pos + 1);
    if pos >= b.len() {
        return Err(SynErr::Eof);
    }
    if b[pos] == b']' {
        return Ok(pos + 1);
    }
    let depth = depth + 1;
    loop {
        pos = skip_ws(b, pos);
        pos = consume_value(b, pos, depth)?;
        pos = skip_ws(b, pos);
        if pos >= b.len() {
            return Err(SynErr::Eof);
        }
        match b[pos] {
            b',' => pos += 1,
            b']' => return Ok(pos + 1),
            _ => {
                return Err(invalid_char(
                    b,
                    pos,
                    "after array element (expecting ',' or ']')",
                ));
            }
        }
    }
}

/// `transformSyntacticError`.
fn legacy_syntax_error(e: SynErr, len: usize) -> JsonError {
    match e {
        SynErr::Eof => JsonError::new("unexpected end of JSON input".into(), len),
        SynErr::MaxDepth(at) => JsonError::new("exceeded max depth".into(), at + 1),
        SynErr::Text(mut msg, at) => {
            if let Some(i) = msg.find(" (expecting")
                && !msg.contains(" in literal")
            {
                msg.truncate(i);
            }
            let msg = msg
                .replace("object name", "object key")
                .replace("at start of value", "looking for beginning of value")
                .replace(
                    "at start of string",
                    "looking for beginning of object key string",
                )
                .replace("after object value", "after object key:value pair")
                .replace("in number", "in numeric literal");
            JsonError::new(msg, at + 1)
        }
    }
}

/// Go's `checkValid` (via `jsontext` with legacy semantics).
fn check_valid(data: &[u8]) -> Result<(), JsonError> {
    let pos = skip_ws(data, 0);
    if pos >= data.len() {
        return Err(legacy_syntax_error(SynErr::Eof, data.len()));
    }
    if data[pos] == b':' || data[pos] == b',' {
        return Err(legacy_syntax_error(
            invalid_char(data, pos, "at start of value"),
            data.len(),
        ));
    }
    let end = consume_value(data, pos, 1).map_err(|e| legacy_syntax_error(e, data.len()))?;
    let end = skip_ws(data, end);
    if end < data.len() {
        return Err(legacy_syntax_error(
            invalid_char(data, end, "after top-level value"),
            data.len(),
        ));
    }
    Ok(())
}

struct Dec<'a> {
    b: &'a [u8],
    i: usize,
    saved: Option<JsonError>,
    /// JSON pointer tokens (escaped) of the current position.
    path: Vec<String>,
}

/// JSON pointer escaping of an object name.
fn escape_pointer_token(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

impl<'a> Dec<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.b.len() && is_ws(self.b[self.i]) {
            self.i += 1;
        }
    }

    fn peek(&self) -> u8 {
        self.b.get(self.i).copied().unwrap_or(0)
    }

    /// Records a value error the way Go's legacy `UnmarshalTypeError`
    /// reports it (the first error wins).
    fn type_error(&mut self, value: &str, go_type: &str) {
        if self.saved.is_some() {
            return;
        }
        let msg = if self.path.is_empty() {
            format!("json: cannot unmarshal {value} into Go value of type {go_type}")
        } else {
            let field = self.path.join(".");
            let last = self.path.last().map(String::as_str).unwrap_or("");
            let into = if !last.is_empty() && last.bytes().all(|c| c.is_ascii_digit()) {
                ""
            } else {
                "Go struct field "
            };
            format!("json: cannot unmarshal {value} into {into}.{field} of type {go_type}")
        };
        self.saved = Some(JsonError::new(msg, self.i));
    }

    fn value(&mut self) -> Value {
        self.skip_ws();
        match self.peek() {
            b'{' => {
                self.i += 1;
                let mut m = Map::new();
                loop {
                    self.skip_ws();
                    if self.peek() == b'}' {
                        self.i += 1;
                        break;
                    }
                    let k = self.string();
                    self.skip_ws();
                    self.i += 1; // ':'
                    self.path.push(escape_pointer_token(&k));
                    let v = self.value();
                    self.path.pop();
                    m.insert(k, v);
                    self.skip_ws();
                    if self.peek() == b',' {
                        self.i += 1;
                        continue;
                    }
                    self.i += 1; // '}'
                    break;
                }
                Value::map(m)
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.peek() == b']' {
                        self.i += 1;
                        break;
                    }
                    self.path.push(format!("{}", items.len()));
                    let v = self.value();
                    self.path.pop();
                    items.push(v);
                    self.skip_ws();
                    if self.peek() == b',' {
                        self.i += 1;
                        continue;
                    }
                    self.i += 1; // ']'
                    break;
                }
                Value::list(items)
            }
            b'"' => Value::string(self.string()),
            b't' => {
                self.i += 4;
                Value::Bool(true)
            }
            b'f' => {
                self.i += 5;
                Value::Bool(false)
            }
            b'n' => {
                self.i += 4;
                Value::Nil
            }
            _ => {
                let st = self.i;
                while self.i < self.b.len()
                    && matches!(
                        self.b[self.i],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.i += 1;
                }
                let s = core::str::from_utf8(&self.b[st..self.i]).unwrap_or("0");
                match parse_float(s) {
                    Ok(f) => Value::Float(f),
                    Err(_) => {
                        let msg = format!("number {s}");
                        self.type_error(&msg, "float64");
                        // strconv.ParseFloat returns ±Inf on overflow.
                        Value::Float(if s.starts_with('-') {
                            f64::NEG_INFINITY
                        } else {
                            f64::INFINITY
                        })
                    }
                }
            }
        }
    }

    fn getu4(&self, at: usize) -> Option<u32> {
        if at + 6 > self.b.len() || self.b[at] != b'\\' || self.b[at + 1] != b'u' {
            return None;
        }
        let h = core::str::from_utf8(&self.b[at + 2..at + 6]).ok()?;
        u32::from_str_radix(h, 16).ok()
    }

    /// Reads a (valid) JSON string starting at the opening quote.
    fn string(&mut self) -> String {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        while self.i < self.b.len() {
            let c = self.b[self.i];
            match c {
                b'"' => {
                    self.i += 1;
                    break;
                }
                b'\\' => {
                    let e = self.b.get(self.i + 1).copied().unwrap_or(0);
                    match e {
                        b'u' => {
                            let mut r = self.getu4(self.i).unwrap_or(0xFFFD);
                            self.i += 6;
                            if (0xD800..0xE000).contains(&r) {
                                let mut combined = None;
                                if r < 0xDC00
                                    && let Some(r2) = self.getu4(self.i)
                                    && (0xDC00..0xE000).contains(&r2)
                                {
                                    combined = Some(0x10000 + ((r - 0xD800) << 10) + (r2 - 0xDC00));
                                }
                                match combined {
                                    Some(c) => {
                                        self.i += 6;
                                        r = c;
                                    }
                                    None => r = 0xFFFD,
                                }
                            }
                            let ch = char::from_u32(r).unwrap_or('\u{FFFD}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => {
                            out.push(match e {
                                b'b' => 0x08,
                                b'f' => 0x0C,
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                other => other,
                            });
                            self.i += 2;
                        }
                    }
                }
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        crate::yaml::strconv::go_lossy_utf8(&out)
    }
}

/// `json.Unmarshal(data, &v)` for the given target type. Like Go, decoding
/// continues after a value error (e.g. a number out of range) and the first
/// such error is returned alongside the decoded value. Syntax errors leave
/// the target empty.
pub fn unmarshal(s: &str, target: Target<'_>) -> (Value, Option<JsonError>) {
    let empty = || match target {
        Target::Any => Value::Nil,
        Target::Map(_) => Value::empty_map(),
        Target::List => Value::list(Vec::new()),
    };
    if let Err(e) = check_valid(s.as_bytes()) {
        return (empty(), Some(e));
    }
    let mut d = Dec {
        b: s.as_bytes(),
        i: 0,
        saved: None,
        path: Vec::new(),
    };
    d.skip_ws();
    let start = d.i;
    let kind = match d.peek() {
        b'{' => "object",
        b'[' => "array",
        b'"' => "string",
        b't' | b'f' => "bool",
        b'n' => "null",
        _ => "number",
    };
    match target {
        Target::Any => {}
        Target::Map(ty) => {
            if kind != "object" && kind != "null" {
                let msg = format!("json: cannot unmarshal {kind} into Go value of type {ty}");
                return (empty(), Some(JsonError::new(msg, start + 1)));
            }
        }
        Target::List => {
            if kind != "array" && kind != "null" {
                let msg =
                    format!("json: cannot unmarshal {kind} into Go value of type []interface {{}}");
                return (empty(), Some(JsonError::new(msg, start + 1)));
            }
        }
    }
    let v = d.value();
    let v = if v.is_nil() { empty() } else { v };
    (v, d.saved)
}

/// `json.Unmarshal` into `interface{}`: numbers become `float64`.
pub fn from_str(s: &str) -> Result<Value, JsonError> {
    match unmarshal(s, Target::Any) {
        (v, None) => Ok(v),
        (_, Some(e)) => Err(e),
    }
}

/// `encoding/json`'s `isEmptyValue` (for `omitempty`).
fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Nil => true,
        Value::Bool(b) => !b,
        Value::Int(i) | Value::Int64(i) => *i == 0,
        Value::Uint64(u) => *u == 0,
        Value::Float(f) => *f == 0.0,
        Value::String(s) => s.is_empty(),
        Value::Bytes(b) => b.is_empty(),
        Value::List(l) => l.is_empty(),
        Value::Map(m) => m.is_empty(),
        Value::Object(o) => {
            matches!(
                o.kind(),
                crate::value::Kind::Map | crate::value::Kind::Slice
            ) && !o.truthy()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map;

    #[test]
    fn encode() {
        let v = map! { "b" => 1.0, "a" => "<x&y>", "c" => Value::list(alloc::vec![Value::Float(1e21), Value::Float(1e-7)]) };
        assert_eq!(
            to_string(&v).unwrap(),
            "{\"a\":\"\\u003cx\\u0026y\\u003e\",\"b\":1,\"c\":[1e+21,1e-7]}"
        );
        assert_eq!(
            to_string_raw(&v).unwrap(),
            r#"{"a":"<x&y>","b":1,"c":[1e+21,1e-7]}"#
        );
        assert_eq!(
            to_string(&Value::Float(f64::NAN)).unwrap_err().message,
            "json: unsupported value: NaN"
        );
        assert_eq!(
            to_string_indent(&v, "", "  ").unwrap(),
            "{\n  \"a\": \"\\u003cx\\u0026y\\u003e\",\n  \"b\": 1,\n  \"c\": [\n    1e+21,\n    1e-7\n  ]\n}"
        );
        assert_eq!(to_string_indent(&map! {}, "", "  ").unwrap(), "{}");
    }

    #[test]
    fn decode() {
        assert_eq!(
            from_str("{\"a\": [1, 2.5, true, null, \"x\"]}").unwrap(),
            map! { "a" => Value::list(alloc::vec![Value::Float(1.0), Value::Float(2.5), Value::Bool(true), Value::Nil, Value::str("x")]) }
        );
        assert_eq!(
            from_str("x").unwrap_err().message,
            "invalid character 'x' looking for beginning of value"
        );
        assert_eq!(
            from_str("").unwrap_err().message,
            "unexpected end of JSON input"
        );
        assert_eq!(
            from_str("{\"a\" 1}").unwrap_err().message,
            "invalid character '1' after object key"
        );
        assert_eq!(
            from_str("1e400").unwrap_err().message,
            "json: cannot unmarshal number 1e400 into Go value of type float64"
        );
        assert_eq!(
            from_str("[1] x").unwrap_err().message,
            "invalid character 'x' after top-level value"
        );
        assert_eq!(from_str("\"\\ud83d\\ude00\"").unwrap(), Value::str("😀"));
    }
}
