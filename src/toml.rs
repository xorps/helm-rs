//! TOML compatible with `github.com/BurntSushi/toml` v1.6 (`toToml`,
//! `fromToml`).
//!
//! The lexer and parser are ports of BurntSushi's, so accepted input and
//! error messages (`toml: line N (last key "k"): ...`) match. Datetimes are
//! returned as RFC 3339 strings (what `toJson` would print for the Go
//! `time.Time`), assuming a UTC local time zone for local date/times.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::value::{Map, Value};
use crate::yaml::strconv::{format_float, parse_float, parse_int, quote};

/// Maximum nesting of arrays/inline tables.
const MAX_DEPTH: usize = crate::yaml::MAX_DEPTH;

// ---------------------------------------------------------------------------
// Encoding.

/// `dblQuotedReplacer`.
fn dbl_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

fn is_bare_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn maybe_quoted(k: &str) -> String {
    if k.is_empty() {
        return "\"\"".into();
    }
    if k.chars().all(is_bare_key_char) {
        return k.into();
    }
    format!("\"{}\"", dbl_quoted(k))
}

fn key_string(key: &[String]) -> String {
    let parts: Vec<String> = key.iter().map(|k| maybe_quoted(k)).collect();
    parts.join(".")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TType {
    Integer,
    Float,
    Datetime,
    Str,
    Bool,
    Array,
    Hash,
    ArrayHash,
}

struct Enc {
    out: String,
    has_written: bool,
    depth: usize,
}

type EncResult = Result<(), String>;

fn is_time(v: &Value) -> bool {
    matches!(v, Value::Object(o) if o.type_name() == "time.Time")
}

/// Go structs keep their identity (field order); other objects encode as
/// their data.
fn data_of(v: &Value) -> Value {
    match v {
        Value::Object(o) if !is_time(v) && o.go_struct().is_none() => o.to_data(),
        _ => v.clone(),
    }
}

/// A struct's exported fields, in declaration order, keyed by Go field name
/// (BurntSushi/toml ignores `json` tags).
fn struct_entries(v: &Value) -> Option<Vec<(String, Value)>> {
    let Value::Object(o) = v else { return None };
    let gs = o.go_struct()?;
    Some(
        gs.fields
            .into_iter()
            .filter(|f| f.name.starts_with(|c: char| c.is_ascii_uppercase()))
            .map(|f| (String::from(f.name), data_of(&f.value)))
            .collect(),
    )
}

impl Enc {
    fn write(&mut self, s: &str) {
        self.out.push_str(s);
        self.has_written = true;
    }

    fn newline(&mut self) {
        if self.has_written {
            self.write("\n");
        }
    }

    fn indent_str(key: &[String]) -> String {
        "  ".repeat(key.len().saturating_sub(1))
    }

    fn enter(&mut self) -> EncResult {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(format!("toml: exceeded max depth of {MAX_DEPTH}"));
        }
        Ok(())
    }

    /// `tomlTypeOfGo` (None for nil).
    fn type_of(&mut self, v: &Value) -> Result<Option<TType>, String> {
        Ok(Some(match v {
            Value::Nil => return Ok(None),
            Value::Bool(_) => TType::Bool,
            Value::Int(_) | Value::Int64(_) | Value::Uint64(_) => TType::Integer,
            Value::Float(_) => TType::Float,
            Value::String(_) => TType::Str,
            Value::Bytes(_) => TType::Array,
            Value::Map(_) => TType::Hash,
            Value::List(items) => {
                if self.is_table_array(items)? {
                    TType::ArrayHash
                } else {
                    TType::Array
                }
            }
            Value::Object(_) if is_time(v) => TType::Datetime,
            Value::Object(o) if o.go_struct().is_some() => TType::Hash,
            Value::Object(o) => {
                let d = o.to_data();
                self.enter()?;
                let t = self.type_of(&d)?;
                self.depth -= 1;
                return Ok(t);
            }
        }))
    }

    fn is_table_array(&mut self, items: &[Value]) -> Result<bool, String> {
        if items.is_empty() {
            return Ok(false);
        }
        self.enter()?;
        let mut ret = true;
        for it in items {
            let tt = self.type_of(it)?;
            match tt {
                None => return Err("toml: cannot encode array with nil element".into()),
                Some(t) => {
                    if ret && t != TType::Hash {
                        ret = false;
                    }
                }
            }
        }
        self.depth -= 1;
        Ok(ret)
    }

    fn encode(&mut self, key: &[String], v: &Value) -> EncResult {
        match v {
            Value::Nil => Ok(()),
            Value::Bool(_)
            | Value::Int(_)
            | Value::Int64(_)
            | Value::Uint64(_)
            | Value::Float(_)
            | Value::String(_) => self.write_key_value(key, v, false),
            Value::List(_) | Value::Bytes(_) => {
                if self.type_of(v)? == Some(TType::ArrayHash) {
                    self.array_of_tables(key, v)
                } else {
                    self.write_key_value(key, v, false)
                }
            }
            Value::Map(_) => self.table(key, v),
            Value::Object(_) if is_time(v) => self.write_key_value(key, v, false),
            Value::Object(o) if o.go_struct().is_some() => self.table(key, v),
            Value::Object(o) => {
                let d = o.to_data();
                self.enter()?;
                self.encode(key, &d)?;
                self.depth -= 1;
                Ok(())
            }
        }
    }

    fn write_key_value(&mut self, key: &[String], v: &Value, inline: bool) -> EncResult {
        if key.is_empty() {
            return self.element(v);
        }
        let k = maybe_quoted(&key[key.len() - 1]);
        self.write(&format!("{}{} = ", Self::indent_str(key), k));
        self.element(v)?;
        if !inline {
            self.newline();
        }
        Ok(())
    }

    fn element(&mut self, v: &Value) -> EncResult {
        match v {
            Value::String(s) => {
                let q = format!("\"{}\"", dbl_quoted(s));
                self.write(&q);
            }
            Value::Bool(b) => self.write(if *b { "true" } else { "false" }),
            Value::Int(i) | Value::Int64(i) => self.write(&format!("{i}")),
            Value::Uint64(u) => self.write(&format!("{u}")),
            Value::Float(f) => {
                let f = *f;
                if f.is_nan() {
                    if f.is_sign_negative() {
                        self.write("-");
                    }
                    self.write("nan");
                } else if f.is_infinite() {
                    if f.is_sign_negative() {
                        self.write("-");
                    }
                    self.write("inf");
                } else {
                    let s = format_float(f, b'g', false);
                    let s = if s.contains('e') || s.contains('.') {
                        s
                    } else {
                        format!("{s}.0")
                    };
                    self.write(&s);
                }
            }
            Value::List(items) => {
                self.enter()?;
                self.write("[");
                for (i, it) in items.iter().enumerate() {
                    self.element(it)?;
                    if i != items.len() - 1 {
                        self.write(", ");
                    }
                }
                self.write("]");
                self.depth -= 1;
            }
            Value::Bytes(b) => {
                self.write("[");
                for (i, byte) in b.iter().enumerate() {
                    self.write(&format!("{byte}"));
                    if i != b.len() - 1 {
                        self.write(", ");
                    }
                }
                self.write("]");
            }
            Value::Map(_) => {
                self.enter()?;
                self.map(&[], v, true)?;
                self.depth -= 1;
            }
            Value::Object(o) if is_time(v) => {
                let s = match o.as_any().downcast_ref::<TomlDatetime>() {
                    Some(t) => t.toml.clone(),
                    None => o.to_data().as_str().unwrap_or("").to_string(),
                };
                self.write(&s);
            }
            Value::Object(o) => {
                let d = o.to_data();
                self.enter()?;
                self.element(&d)?;
                self.depth -= 1;
            }
            Value::Nil => {
                return Err("reflect: call of reflect.Value.Interface on zero Value".into());
            }
        }
        Ok(())
    }

    fn array_of_tables(&mut self, key: &[String], v: &Value) -> EncResult {
        if key.is_empty() {
            return Err("toml: top-level values must be Go maps or structs".into());
        }
        let Value::List(items) = v else { return Ok(()) };
        for it in items.iter() {
            let it = data_of(it);
            if it.is_nil() {
                continue;
            }
            self.newline();
            self.write(&format!("{}[[{}]]", Self::indent_str(key), key_string(key)));
            self.newline();
            self.enter()?;
            self.map(key, &it, false)?;
            self.depth -= 1;
        }
        Ok(())
    }

    fn table(&mut self, key: &[String], v: &Value) -> EncResult {
        if key.len() == 1 {
            self.newline();
        }
        if !key.is_empty() {
            self.write(&format!("{}[{}]", Self::indent_str(key), key_string(key)));
            self.newline();
        }
        self.enter()?;
        self.map(key, v, false)?;
        self.depth -= 1;
        Ok(())
    }

    fn map(&mut self, key: &[String], v: &Value, inline: bool) -> EncResult {
        let entries: Vec<(String, Value)> = match v {
            Value::Map(m) => m
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), data_of(v)))
                .collect(),
            _ => match struct_entries(v) {
                Some(e) => e,
                None => return Ok(()),
            },
        };
        let mut direct = Vec::new();
        let mut sub = Vec::new();
        for (k, val) in entries {
            match self.type_of(&val)? {
                Some(TType::Hash) | Some(TType::ArrayHash) => sub.push((k, val)),
                _ => direct.push((k, val)),
            }
        }
        if inline {
            self.write("{");
        }
        let trail = !sub.is_empty();
        self.write_map_keys(key, &direct, trail, inline)?;
        self.write_map_keys(key, &sub, false, inline)?;
        if inline {
            self.write("}");
        }
        Ok(())
    }

    fn write_map_keys(
        &mut self,
        key: &[String],
        keys: &[(String, Value)],
        trail: bool,
        inline: bool,
    ) -> EncResult {
        for (i, (k, val)) in keys.iter().enumerate() {
            if val.is_nil() {
                continue;
            }
            if inline {
                self.write_key_value(core::slice::from_ref(k), val, true)?;
                if trail || i != keys.len() - 1 {
                    self.write(", ");
                }
            } else {
                let mut sub_key = key.to_vec();
                sub_key.push(k.clone());
                self.encode(&sub_key, val)?;
            }
        }
        Ok(())
    }
}

/// `toml.NewEncoder(...).Encode(v)`.
pub fn to_string(v: &Value) -> Result<String, String> {
    let mut e = Enc {
        out: String::new(),
        has_written: false,
        depth: 0,
    };
    let v = data_of(v);
    e.encode(&[], &v)?;
    Ok(e.out)
}

// ---------------------------------------------------------------------------
// Lexer (port of lex.go).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ItemType {
    Error,
    Eof,
    Text,
    Str,
    StrEsc,
    RawStr,
    MultilineStr,
    RawMultilineStr,
    Bool,
    Integer,
    Float,
    Datetime,
    Array,
    ArrayEnd,
    TableStart,
    TableEnd,
    ArrayTableStart,
    ArrayTableEnd,
    KeyStart,
    KeyEnd,
    CommentStart,
    InlineTableStart,
    InlineTableEnd,
}

#[derive(Clone, Copy, Debug, Default)]
struct Pos {
    line: usize,
    start: usize,
    len: usize,
}

#[derive(Clone, Debug)]
struct Item {
    typ: ItemType,
    val: String,
    err: Option<String>,
    pos: Pos,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Top,
    TopEnd,
    TableStart,
    TableEnd,
    ArrayTableEnd,
    TableNameStart,
    TableNameEnd,
    BareName,
    QuotedName,
    KeyStart,
    KeyNameStart,
    KeyEnd,
    Value,
    ArrayValue,
    ArrayValueEnd,
    ArrayEnd,
    InlineTableValue,
    InlineTableValueEnd,
    InlineTableEnd,
    Str,
    MultilineStr,
    RawStr,
    MultilineRawStr,
    MultilineStrEscape,
    StrEscape,
    HexEscape,
    ShortUnicodeEscape,
    LongUnicodeEscape,
    NumberOrDateStart,
    NumberOrDate,
    Datetime,
    HexInteger,
    OctalInteger,
    BinaryInteger,
    DecimalNumber,
    DecimalNumberStart,
    BaseNumberOrDate,
    Float,
    Bool,
    CommentStart,
    Comment,
}

const EOF: char = '\0';
/// Go's `utf8.RuneError` returned by `next` on errors.
const RUNE_ERROR: char = '\u{FFFD}';

struct Lexer<'a> {
    input: &'a str,
    start: usize,
    pos: usize,
    line: usize,
    state: Option<St>,
    items: VecDeque<Item>,
    esc: bool,
    prev_widths: [usize; 4],
    nprev: usize,
    at_eof: bool,
    stack: Vec<St>,
}

fn is_whitespace(r: char) -> bool {
    r == '\t' || r == ' '
}
fn is_nl(r: char) -> bool {
    r == '\n' || r == '\r'
}
fn is_control(r: char) -> bool {
    match r {
        '\t' | '\r' | '\n' => false,
        _ => (r as u32) <= 0x1f || r as u32 == 0x7f,
    }
}
fn is_digit(r: char) -> bool {
    r.is_ascii_digit()
}
fn is_binary(r: char) -> bool {
    r == '0' || r == '1'
}
fn is_octal(r: char) -> bool {
    ('0'..='7').contains(&r)
}
fn is_hex(r: char) -> bool {
    r.is_ascii_hexdigit()
}

/// Go's `%q` for a rune.
fn q_rune(r: char) -> String {
    match r {
        '\'' => "'\\''".into(),
        '"' => "'\"'".into(),
        _ => {
            let q = quote(&String::from(r));
            format!("'{}'", &q[1..q.len() - 1])
        }
    }
}

fn rune_or_eof(r: char) -> String {
    if r == EOF {
        return "end of file".into();
    }
    format!("'{r}'")
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Lexer {
            input,
            start: 0,
            pos: 0,
            line: 1,
            state: Some(St::Top),
            items: VecDeque::new(),
            esc: false,
            prev_widths: [0; 4],
            nprev: 0,
            at_eof: false,
            stack: Vec::new(),
        }
    }

    fn next_item(&mut self) -> Item {
        loop {
            if let Some(it) = self.items.pop_front() {
                return it;
            }
            match self.state {
                Some(st) => self.state = self.step(st),
                None => {
                    return Item {
                        typ: ItemType::Error,
                        val: String::new(),
                        err: Some("unexpected EOF".into()),
                        pos: self.get_pos(),
                    };
                }
            }
        }
    }

    fn push(&mut self, s: St) {
        self.stack.push(s);
    }

    fn pop(&mut self) -> Option<St> {
        self.stack.pop()
    }

    fn current(&self) -> &'a str {
        self.input.get(self.start..self.pos).unwrap_or("")
    }

    fn get_pos(&self) -> Pos {
        let len = self.pos.saturating_sub(self.start);
        Pos {
            line: self.line,
            start: self.start,
            len: if len == 0 { 1 } else { len },
        }
    }

    fn emit(&mut self, typ: ItemType) {
        if self.start > self.pos {
            let b = self.input.as_bytes().get(self.pos).copied().unwrap_or(0);
            self.error(format!("invalid UTF-8 byte: 0x{b:02x}"));
            return;
        }
        let it = Item {
            typ,
            val: self.current().to_string(),
            err: None,
            pos: self.get_pos(),
        };
        self.items.push_back(it);
        self.start = self.pos;
    }

    fn emit_trim(&mut self, typ: ItemType) {
        let it = Item {
            typ,
            val: self
                .current()
                .trim_matches(|c: char| c.is_whitespace())
                .to_string(),
            err: None,
            pos: self.get_pos(),
        };
        self.items.push_back(it);
        self.start = self.pos;
    }

    fn next(&mut self) -> char {
        if self.pos >= self.input.len() {
            self.at_eof = true;
            return EOF;
        }
        let b = self.input.as_bytes();
        if b[self.pos] == b'\n' {
            self.line += 1;
        }
        self.prev_widths[3] = self.prev_widths[2];
        self.prev_widths[2] = self.prev_widths[1];
        self.prev_widths[1] = self.prev_widths[0];
        if self.nprev < 4 {
            self.nprev += 1;
        }
        let r = self.input[self.pos..].chars().next().unwrap_or(EOF);
        let w = r.len_utf8();
        if is_control(r)
            || (r == '\r'
                && (self.input.len() - 1 == self.pos || b.get(self.pos + 1) != Some(&b'\n')))
        {
            self.error_pos(
                self.pos.wrapping_sub(1),
                1,
                format!(
                    "TOML files cannot contain control characters: '0x{:02x}'",
                    r as u32
                ),
            );
            return RUNE_ERROR;
        }
        self.prev_widths[0] = w;
        self.pos += w;
        r
    }

    fn ignore(&mut self) {
        self.start = self.pos;
    }

    fn backup(&mut self) {
        if self.at_eof {
            self.at_eof = false;
            return;
        }
        if self.nprev < 1 {
            return;
        }
        let w = self.prev_widths[0];
        self.prev_widths[0] = self.prev_widths[1];
        self.prev_widths[1] = self.prev_widths[2];
        self.prev_widths[2] = self.prev_widths[3];
        self.nprev -= 1;
        self.pos = self.pos.saturating_sub(w);
        if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'\n' {
            self.line -= 1;
        }
    }

    fn accept(&mut self, valid: char) -> bool {
        if self.next() == valid {
            return true;
        }
        self.backup();
        false
    }

    fn peek(&mut self) -> char {
        let r = self.next();
        self.backup();
        r
    }

    fn skip(&mut self, pred: fn(char) -> bool) {
        loop {
            let r = self.next();
            if pred(r) {
                continue;
            }
            self.backup();
            self.ignore();
            return;
        }
    }

    fn error(&mut self, msg: String) -> Option<St> {
        if self.at_eof {
            return self.error_prev_line(msg);
        }
        let pos = self.get_pos();
        self.items.push_back(Item {
            typ: ItemType::Error,
            val: String::new(),
            err: Some(msg),
            pos,
        });
        None
    }

    fn error_prev_line(&mut self, msg: String) -> Option<St> {
        let mut pos = self.get_pos();
        pos.line = pos.line.saturating_sub(1);
        pos.len = 1;
        pos.start = self.pos.saturating_sub(1);
        self.items.push_back(Item {
            typ: ItemType::Error,
            val: String::new(),
            err: Some(msg),
            pos,
        });
        None
    }

    fn error_pos(&mut self, start: usize, len: usize, msg: String) -> Option<St> {
        let mut pos = self.get_pos();
        pos.start = start;
        pos.len = len;
        self.items.push_back(Item {
            typ: ItemType::Error,
            val: String::new(),
            err: Some(msg),
            pos,
        });
        None
    }

    fn errorf(&mut self, msg: String) -> Option<St> {
        if self.at_eof {
            let mut pos = self.get_pos();
            if self.pos >= 1 && self.input.as_bytes().get(self.pos - 1) == Some(&b'\n') {
                pos.line = pos.line.saturating_sub(1);
            }
            pos.len = 1;
            pos.start = self.pos.saturating_sub(1);
            self.items.push_back(Item {
                typ: ItemType::Error,
                val: String::new(),
                err: Some(msg),
                pos,
            });
            return None;
        }
        let pos = self.get_pos();
        self.items.push_back(Item {
            typ: ItemType::Error,
            val: String::new(),
            err: Some(msg),
            pos,
        });
        None
    }

    fn step(&mut self, st: St) -> Option<St> {
        match st {
            St::Top => {
                let r = self.next();
                if is_whitespace(r) || is_nl(r) {
                    self.ignore();
                    return Some(St::Top);
                }
                match r {
                    '#' => {
                        self.push(St::Top);
                        return Some(St::CommentStart);
                    }
                    '[' => return Some(St::TableStart),
                    EOF => {
                        if self.pos > self.start {
                            return self.errorf("unexpected EOF".into());
                        }
                        self.emit(ItemType::Eof);
                        return None;
                    }
                    _ => {}
                }
                self.backup();
                self.push(St::TopEnd);
                Some(St::KeyStart)
            }
            St::TopEnd => {
                let r = self.next();
                if r == '#' {
                    self.push(St::Top);
                    return Some(St::CommentStart);
                }
                if is_whitespace(r) {
                    return Some(St::TopEnd);
                }
                if is_nl(r) {
                    self.ignore();
                    return Some(St::Top);
                }
                if r == EOF {
                    self.emit(ItemType::Eof);
                    return None;
                }
                self.errorf(format!(
                    "expected a top-level item to end with a newline, comment, or EOF, but got {} instead",
                    q_rune(r)
                ))
            }
            St::TableStart => {
                if self.peek() == '[' {
                    self.next();
                    self.emit(ItemType::ArrayTableStart);
                    self.push(St::ArrayTableEnd);
                } else {
                    self.emit(ItemType::TableStart);
                    self.push(St::TableEnd);
                }
                Some(St::TableNameStart)
            }
            St::TableEnd => {
                self.emit(ItemType::TableEnd);
                Some(St::TopEnd)
            }
            St::ArrayTableEnd => {
                let r = self.next();
                if r != ']' {
                    return self.errorf(format!(
                        "expected end of table array name delimiter ']', but got {} instead",
                        q_rune(r)
                    ));
                }
                self.emit(ItemType::ArrayTableEnd);
                Some(St::TopEnd)
            }
            St::TableNameStart => {
                self.skip(is_whitespace);
                let r = self.peek();
                if r == ']' || r == EOF {
                    return self.errorf(
                        "unexpected end of table name (table names cannot be empty)".into(),
                    );
                }
                if r == '.' {
                    return self
                        .errorf("unexpected table separator (table names cannot be empty)".into());
                }
                self.push(St::TableNameEnd);
                if r == '"' || r == '\'' {
                    self.ignore();
                    return Some(St::QuotedName);
                }
                Some(St::BareName)
            }
            St::TableNameEnd => {
                self.skip(is_whitespace);
                let r = self.next();
                if r == '.' {
                    self.ignore();
                    return Some(St::TableNameStart);
                }
                if r == ']' {
                    return self.pop();
                }
                self.errorf(format!(
                    "expected '.' or ']' to end table name, but got {} instead",
                    q_rune(r)
                ))
            }
            St::BareName => {
                let r = self.next();
                if is_bare_key_char(r) {
                    return Some(St::BareName);
                }
                self.backup();
                self.emit(ItemType::Text);
                self.pop()
            }
            St::QuotedName => {
                let r = self.next();
                if r == '"' {
                    self.ignore();
                    return Some(St::Str);
                }
                if r == '\'' {
                    self.ignore();
                    return Some(St::RawStr);
                }
                if is_whitespace(r) {
                    self.ignore();
                    return Some(St::Value);
                }
                if r == EOF {
                    return self.errorf("unexpected EOF; expected value".into());
                }
                self.errorf(format!("expected value but found {} instead", q_rune(r)))
            }
            St::KeyStart => {
                self.skip(is_whitespace);
                let r = self.peek();
                if r == '=' || r == EOF {
                    return self.errorf("unexpected '=': key name appears blank".into());
                }
                if r == '.' {
                    return self.errorf("unexpected '.': keys cannot start with a '.'".into());
                }
                if r == '"' || r == '\'' {
                    self.ignore();
                }
                self.emit(ItemType::KeyStart);
                Some(St::KeyNameStart)
            }
            St::KeyNameStart => {
                self.skip(is_whitespace);
                let r = self.peek();
                if r == '"' || r == '\'' {
                    self.ignore();
                    self.push(St::KeyEnd);
                    return Some(St::QuotedName);
                }
                if r == '=' || r == EOF {
                    return self.errorf("unexpected '='".into());
                }
                if r == '.' {
                    return self.errorf("unexpected '.'".into());
                }
                self.push(St::KeyEnd);
                Some(St::BareName)
            }
            St::KeyEnd => {
                self.skip(is_whitespace);
                let r = self.next();
                if is_whitespace(r) {
                    self.ignore();
                    return Some(St::KeyEnd);
                }
                if r == EOF {
                    return self.errorf("unexpected EOF; expected key separator '='".into());
                }
                if r == '.' {
                    self.ignore();
                    return Some(St::KeyNameStart);
                }
                if r == '=' {
                    self.emit(ItemType::KeyEnd);
                    self.ignore();
                    return Some(St::Value);
                }
                if r == '\n' {
                    return self.error_prev_line(format!(
                        "expected '.' or '=', but got {} instead",
                        q_rune(r)
                    ));
                }
                self.errorf(format!(
                    "expected '.' or '=', but got {} instead",
                    q_rune(r)
                ))
            }
            St::Value => self.lex_value(),
            St::ArrayValue => {
                let r = self.next();
                if is_whitespace(r) || is_nl(r) {
                    self.ignore();
                    return Some(St::ArrayValue);
                }
                if r == '#' {
                    self.push(St::ArrayValue);
                    return Some(St::CommentStart);
                }
                if r == ',' {
                    return self.errorf("unexpected comma".into());
                }
                if r == ']' {
                    return Some(St::ArrayEnd);
                }
                self.backup();
                self.push(St::ArrayValueEnd);
                Some(St::Value)
            }
            St::ArrayValueEnd => {
                let r = self.next();
                if is_whitespace(r) || is_nl(r) {
                    self.ignore();
                    return Some(St::ArrayValueEnd);
                }
                match r {
                    '#' => {
                        self.push(St::ArrayValueEnd);
                        Some(St::CommentStart)
                    }
                    ',' => {
                        self.ignore();
                        Some(St::ArrayValue)
                    }
                    ']' => Some(St::ArrayEnd),
                    _ => self.errorf(format!(
                        "expected a comma (',') or array terminator (']'), but got {}",
                        rune_or_eof(r)
                    )),
                }
            }
            St::ArrayEnd => {
                self.ignore();
                self.emit(ItemType::ArrayEnd);
                self.pop()
            }
            St::InlineTableValue => {
                let r = self.next();
                if is_whitespace(r) || is_nl(r) {
                    self.ignore();
                    return Some(St::InlineTableValue);
                }
                match r {
                    '#' => {
                        self.push(St::InlineTableValue);
                        return Some(St::CommentStart);
                    }
                    ',' => return self.errorf("unexpected comma".into()),
                    '}' => return Some(St::InlineTableEnd),
                    _ => {}
                }
                self.backup();
                self.push(St::InlineTableValueEnd);
                Some(St::KeyStart)
            }
            St::InlineTableValueEnd => {
                let r = self.next();
                if is_whitespace(r) || is_nl(r) {
                    self.ignore();
                    return Some(St::InlineTableValueEnd);
                }
                match r {
                    '#' => {
                        self.push(St::InlineTableValueEnd);
                        Some(St::CommentStart)
                    }
                    ',' => {
                        self.ignore();
                        self.skip(is_whitespace);
                        if self.peek() == '}' {
                            return Some(St::InlineTableValueEnd);
                        }
                        Some(St::InlineTableValue)
                    }
                    '}' => Some(St::InlineTableEnd),
                    _ => self.errorf(format!(
                        "expected a comma or an inline table terminator '}}', but got {} instead",
                        rune_or_eof(r)
                    )),
                }
            }
            St::InlineTableEnd => {
                self.ignore();
                self.emit(ItemType::InlineTableEnd);
                self.pop()
            }
            St::Str => {
                let r = self.next();
                if r == EOF {
                    return self.errorf("unexpected EOF; expected '\"'".into());
                }
                if is_nl(r) {
                    return self.error_prev_line("strings cannot contain newlines".into());
                }
                if r == '\\' {
                    self.push(St::Str);
                    return Some(St::StrEscape);
                }
                if r == '"' {
                    self.backup();
                    if self.esc {
                        self.esc = false;
                        self.emit(ItemType::StrEsc);
                    } else {
                        self.emit(ItemType::Str);
                    }
                    self.next();
                    self.ignore();
                    return self.pop();
                }
                Some(St::Str)
            }
            St::MultilineStr => {
                let r = self.next();
                match r {
                    EOF => self.errorf("unexpected EOF; expected '\"\"\"'".into()),
                    '\\' => Some(St::MultilineStrEscape),
                    '"' => {
                        if self.accept('"') {
                            if self.accept('"') {
                                if self.peek() == '"' {
                                    let cur = self.current();
                                    if cur.ends_with("\"\"\"\"\"") && !cur.ends_with("\\\"\"\"\"\"")
                                    {
                                        return self.errorf("unexpected '\"\"\"\"\"\"'".into());
                                    }
                                    self.backup();
                                    self.backup();
                                    return Some(St::MultilineStr);
                                }
                                self.backup();
                                self.backup();
                                self.backup();
                                self.esc = false;
                                self.emit(ItemType::MultilineStr);
                                self.next();
                                self.next();
                                self.next();
                                self.ignore();
                                return self.pop();
                            }
                            self.backup();
                        }
                        Some(St::MultilineStr)
                    }
                    _ => Some(St::MultilineStr),
                }
            }
            St::RawStr => {
                let r = self.next();
                if r == EOF {
                    return self.errorf("unexpected EOF; expected \"'\"".into());
                }
                if is_nl(r) {
                    return self.error_prev_line("strings cannot contain newlines".into());
                }
                if r == '\'' {
                    self.backup();
                    self.emit(ItemType::RawStr);
                    self.next();
                    self.ignore();
                    return self.pop();
                }
                Some(St::RawStr)
            }
            St::MultilineRawStr => {
                let r = self.next();
                match r {
                    EOF => self.errorf("unexpected EOF; expected \"'''\"".into()),
                    '\'' => {
                        if self.accept('\'') {
                            if self.accept('\'') {
                                if self.peek() == '\'' {
                                    if self.current().ends_with("'''''") {
                                        return self.errorf("unexpected \"''''''\"".into());
                                    }
                                    self.backup();
                                    self.backup();
                                    return Some(St::MultilineRawStr);
                                }
                                self.backup();
                                self.backup();
                                self.backup();
                                self.emit(ItemType::RawMultilineStr);
                                self.next();
                                self.next();
                                self.next();
                                self.ignore();
                                return self.pop();
                            }
                            self.backup();
                        }
                        Some(St::MultilineRawStr)
                    }
                    _ => Some(St::MultilineRawStr),
                }
            }
            St::MultilineStrEscape => {
                if is_nl(self.next()) {
                    return Some(St::MultilineStr);
                }
                self.backup();
                self.push(St::MultilineStr);
                self.step(St::StrEscape)
            }
            St::StrEscape => {
                self.esc = true;
                let r = self.next();
                match r {
                    'e' | 'b' | 't' | 'n' | 'f' | 'r' | '"' | ' ' | '\t' | '\\' => self.pop(),
                    'x' => Some(St::HexEscape),
                    'u' => Some(St::ShortUnicodeEscape),
                    'U' => Some(St::LongUnicodeEscape),
                    _ => self.error(format!("invalid escape in string '\\{r}'")),
                }
            }
            St::HexEscape => self.hex_escape(
                2,
                "expected two hexadecimal digits after '\\x', but got {} instead",
            ),
            St::ShortUnicodeEscape => self.hex_escape(
                4,
                "expected four hexadecimal digits after '\\u', but got {} instead",
            ),
            St::LongUnicodeEscape => self.hex_escape(
                8,
                "expected eight hexadecimal digits after '\\U', but got {} instead",
            ),
            St::NumberOrDateStart => {
                if self.next() == '0' {
                    return Some(St::BaseNumberOrDate);
                }
                Some(St::NumberOrDate)
            }
            St::NumberOrDate => {
                let r = self.next();
                if is_digit(r) {
                    return Some(St::NumberOrDate);
                }
                match r {
                    '-' | ':' => Some(St::Datetime),
                    '_' => Some(St::DecimalNumber),
                    '.' | 'e' | 'E' => Some(St::Float),
                    _ => {
                        self.backup();
                        self.emit(ItemType::Integer);
                        self.pop()
                    }
                }
            }
            St::Datetime => {
                let r = self.next();
                if is_digit(r) || matches!(r, '-' | ':' | 'T' | 't' | ' ' | '.' | 'Z' | 'z' | '+') {
                    return Some(St::Datetime);
                }
                self.backup();
                self.emit_trim(ItemType::Datetime);
                self.pop()
            }
            St::HexInteger | St::OctalInteger | St::BinaryInteger => {
                let r = self.next();
                let ok = match st {
                    St::HexInteger => is_hex(r),
                    St::OctalInteger => is_octal(r),
                    _ => is_binary(r),
                };
                if ok || r == '_' {
                    return Some(st);
                }
                self.backup();
                self.emit(ItemType::Integer);
                self.pop()
            }
            St::DecimalNumber => {
                let r = self.next();
                if is_digit(r) {
                    return Some(St::DecimalNumber);
                }
                match r {
                    '.' | 'e' | 'E' => Some(St::Float),
                    '_' => Some(St::DecimalNumber),
                    _ => {
                        self.backup();
                        self.emit(ItemType::Integer);
                        self.pop()
                    }
                }
            }
            St::DecimalNumberStart => {
                let r = self.next();
                match r {
                    'i' => {
                        if !self.accept('n') || !self.accept('f') {
                            let c = self.current().to_string();
                            return self.errorf(format!("invalid float: '{c}'"));
                        }
                        self.emit(ItemType::Float);
                        return self.pop();
                    }
                    'n' => {
                        if !self.accept('a') || !self.accept('n') {
                            let c = self.current().to_string();
                            return self.errorf(format!("invalid float: '{c}'"));
                        }
                        self.emit(ItemType::Float);
                        return self.pop();
                    }
                    '0' => {
                        let p = self.peek();
                        if matches!(p, 'b' | 'o' | 'x') {
                            let c = self.current().to_string();
                            return self.errorf(format!(
                                "cannot use sign with non-decimal numbers: '{c}{p}'"
                            ));
                        }
                    }
                    '.' => return self.errorf("floats must start with a digit, not '.'".into()),
                    _ => {}
                }
                if is_digit(r) {
                    return Some(St::DecimalNumber);
                }
                self.errorf(format!("expected a digit but got {}", q_rune(r)))
            }
            St::BaseNumberOrDate => {
                let r = self.next();
                if is_digit(r) {
                    return Some(St::NumberOrDate);
                }
                match r {
                    '_' => Some(St::DecimalNumber),
                    '.' | 'e' | 'E' => Some(St::Float),
                    'b' => {
                        let p = self.peek();
                        if !is_binary(p) {
                            let c = self.current().to_string();
                            self.errorf(format!("not a binary number: '{c}{p}'"));
                        }
                        Some(St::BinaryInteger)
                    }
                    'o' => {
                        let p = self.peek();
                        if !is_octal(p) {
                            let c = self.current().to_string();
                            self.errorf(format!("not an octal number: '{c}{p}'"));
                        }
                        Some(St::OctalInteger)
                    }
                    'x' => {
                        let p = self.peek();
                        if !is_hex(p) {
                            let c = self.current().to_string();
                            self.errorf(format!("not a hexadecimal number: '{c}{p}'"));
                        }
                        Some(St::HexInteger)
                    }
                    _ => {
                        self.backup();
                        self.emit(ItemType::Integer);
                        self.pop()
                    }
                }
            }
            St::Float => {
                let r = self.next();
                if is_digit(r) || matches!(r, '_' | '.' | '-' | '+' | 'e' | 'E') {
                    return Some(St::Float);
                }
                self.backup();
                self.emit(ItemType::Float);
                self.pop()
            }
            St::Bool => {
                let mut rs = String::new();
                loop {
                    let r = self.next();
                    if !(r.is_alphabetic() && r != RUNE_ERROR) {
                        self.backup();
                        break;
                    }
                    rs.push(r);
                }
                if rs == "true" || rs == "false" {
                    self.emit(ItemType::Bool);
                    return self.pop();
                }
                self.errorf(format!("expected value but found {} instead", quote(&rs)))
            }
            St::CommentStart => {
                self.ignore();
                self.emit(ItemType::CommentStart);
                Some(St::Comment)
            }
            St::Comment => {
                let r = self.next();
                if is_nl(r) || r == EOF {
                    self.backup();
                    self.emit(ItemType::Text);
                    return self.pop();
                }
                Some(St::Comment)
            }
        }
    }

    fn hex_escape(&mut self, n: usize, msg: &str) -> Option<St> {
        for _ in 0..n {
            let r = self.next();
            if !is_hex(r) {
                let c = quote(self.current());
                return self.errorf(msg.replace("{}", &c));
            }
        }
        self.pop()
    }

    fn lex_value(&mut self) -> Option<St> {
        let r = self.next();
        if is_whitespace(r) {
            self.ignore();
            return Some(St::Value);
        }
        if is_digit(r) {
            self.backup();
            return Some(St::NumberOrDateStart);
        }
        match r {
            '[' => {
                self.ignore();
                self.emit(ItemType::Array);
                return Some(St::ArrayValue);
            }
            '{' => {
                self.ignore();
                self.emit(ItemType::InlineTableStart);
                return Some(St::InlineTableValue);
            }
            '"' => {
                if self.accept('"') {
                    if self.accept('"') {
                        self.ignore();
                        return Some(St::MultilineStr);
                    }
                    self.backup();
                }
                self.ignore();
                return Some(St::Str);
            }
            '\'' => {
                if self.accept('\'') {
                    if self.accept('\'') {
                        self.ignore();
                        return Some(St::MultilineRawStr);
                    }
                    self.backup();
                }
                self.ignore();
                return Some(St::RawStr);
            }
            '.' => return self.errorf("floats must start with a digit, not '.'".into()),
            'i' | 'n' => {
                if (self.accept('n') && self.accept('f')) || (self.accept('a') && self.accept('n'))
                {
                    self.emit(ItemType::Float);
                    return self.pop();
                }
            }
            '-' | '+' => return Some(St::DecimalNumberStart),
            _ => {}
        }
        if r.is_alphabetic() && r != RUNE_ERROR {
            self.backup();
            return Some(St::Bool);
        }
        if r == EOF {
            return self.errorf("unexpected EOF; expected value".into());
        }
        if r == '\n' {
            return self.error_prev_line(format!("expected value but found {} instead", q_rune(r)));
        }
        self.errorf(format!("expected value but found {} instead", q_rune(r)))
    }
}

// ---------------------------------------------------------------------------
// Parser (port of parse.go).

#[derive(Clone, Debug)]
enum TVal {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Datetime(TomlDatetime),
    Array(Vec<TVal>),
    Table(BTreeMap<String, TVal>),
    ArrayTables(Vec<BTreeMap<String, TVal>>),
}

struct ParseError {
    message: String,
    line: usize,
    last_key: String,
}

impl ParseError {
    fn render(&self) -> String {
        if self.last_key.is_empty() {
            format!("toml: line {}: {}", self.line, self.message)
        } else {
            format!(
                "toml: line {} (last key {}): {}",
                self.line,
                quote(&self.last_key),
                self.message
            )
        }
    }
}

type PResult<T> = Result<T, Box<ParseError>>;

/// State of an inline table being parsed (boxed to keep recursive frames
/// small).
struct InlineState {
    top: BTreeMap<String, TVal>,
    outer_context: Vec<String>,
    outer_key: String,
    prev_context: Vec<String>,
}

struct Parser<'a> {
    lx: Lexer<'a>,
    context: Vec<String>,
    current_key: String,
    pos: Pos,
    key_info: BTreeMap<String, TType>,
    mapping: BTreeMap<String, TVal>,
    implicits: BTreeSet<String>,
    depth: usize,
}

fn key_str(key: &[String]) -> String {
    key_string(key)
}

impl<'a> Parser<'a> {
    fn current(&self) -> String {
        if self.current_key.is_empty() {
            return key_str(&self.context);
        }
        if self.context.is_empty() {
            return self.current_key.clone();
        }
        format!("{}.{}", key_str(&self.context), self.current_key)
    }

    fn err_at(&self, pos: Pos, message: String) -> Box<ParseError> {
        Box::new(ParseError {
            message,
            line: pos.line,
            last_key: self.current(),
        })
    }

    fn panicf(&self, message: String) -> Box<ParseError> {
        self.err_at(self.pos, message)
    }

    fn bug(&self, message: String) -> Box<ParseError> {
        Box::new(ParseError {
            message: format!("BUG: {message}"),
            line: self.pos.line,
            last_key: String::new(),
        })
    }

    fn next(&mut self) -> PResult<Item> {
        let it = self.lx.next_item();
        if it.typ == ItemType::Error {
            let msg = it.err.clone().unwrap_or_else(|| it.val.clone());
            return Err(self.err_at(it.pos, msg));
        }
        Ok(it)
    }

    fn next_pos(&mut self) -> PResult<Item> {
        let it = self.next()?;
        self.pos = it.pos;
        Ok(it)
    }

    fn expect(&mut self, typ: ItemType) -> PResult<Item> {
        let it = self.next()?;
        if it.typ != typ {
            return Err(self.bug(format!("Expected '{typ:?}' but got '{:?}'.", it.typ)));
        }
        Ok(it)
    }

    fn top_level(&mut self, item: Item) -> PResult<()> {
        match item.typ {
            ItemType::CommentStart => {
                self.expect(ItemType::Text)?;
            }
            ItemType::TableStart | ItemType::ArrayTableStart => {
                let array = item.typ == ItemType::ArrayTableStart;
                let end = if array {
                    ItemType::ArrayTableEnd
                } else {
                    ItemType::TableEnd
                };
                let mut name = self.next_pos()?;
                let mut key = Vec::new();
                while name.typ != end && name.typ != ItemType::Eof {
                    key.push(self.key_string(&name)?);
                    name = self.next()?;
                }
                if name.typ != end {
                    return Err(self.bug(format!("Expected '{end:?}' but got '{:?}'.", name.typ)));
                }
                self.add_context(&key, array)?;
                self.set_type("", if array { TType::ArrayHash } else { TType::Hash });
            }
            ItemType::KeyStart => {
                let outer = self.context.clone();
                let mut k = self.next_pos()?;
                let mut key = Vec::new();
                while k.typ != ItemType::KeyEnd && k.typ != ItemType::Eof {
                    key.push(self.key_string(&k)?);
                    k = self.next()?;
                }
                if k.typ != ItemType::KeyEnd {
                    return Err(self.bug(format!("Expected 'KeyEnd' but got '{:?}'.", k.typ)));
                }
                self.current_key = key.last().cloned().unwrap_or_default();
                let parent = key[..key.len().saturating_sub(1)].to_vec();
                for p in &parent {
                    let mut c = self.context.clone();
                    c.push(p.clone());
                    self.add_implicit_context(&c)?;
                }
                let v_item = self.next()?;
                let (val, typ) = self.value(&v_item, false)?;
                let ck = self.current_key.clone();
                self.set_value(&ck, val)?;
                self.set_type(&ck, typ);
                self.context = outer;
                self.current_key.clear();
            }
            other => return Err(self.bug(format!("Unexpected type at top level: {other:?}"))),
        }
        Ok(())
    }

    fn key_string(&mut self, it: &Item) -> PResult<String> {
        match it.typ {
            ItemType::Text => Ok(it.val.clone()),
            ItemType::Str
            | ItemType::StrEsc
            | ItemType::MultilineStr
            | ItemType::RawStr
            | ItemType::RawMultilineStr => match self.value(it, false)?.0 {
                TVal::Str(s) => Ok(s),
                _ => Err(self.bug("Unexpected key type".into())),
            },
            other => Err(self.bug(format!("Unexpected key type: {other:?}"))),
        }
    }

    fn value(&mut self, it: &Item, parent_is_array: bool) -> PResult<(TVal, TType)> {
        // Keep this frame small: it is on the recursion path.
        match it.typ {
            ItemType::Array | ItemType::InlineTableStart => self.value_nested(it, parent_is_array),
            _ => self.value_scalar(it),
        }
    }

    fn value_nested(&mut self, it: &Item, parent_is_array: bool) -> PResult<(TVal, TType)> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.depth_error(it));
        }
        let r = if it.typ == ItemType::Array {
            self.value_array(it)
        } else {
            self.value_inline_table(it, parent_is_array)
        };
        self.depth -= 1;
        r
    }

    #[inline(never)]
    fn depth_error(&self, it: &Item) -> Box<ParseError> {
        self.err_at(it.pos, format!("exceeded max depth of {MAX_DEPTH}"))
    }

    #[inline(never)]
    fn value_scalar(&mut self, it: &Item) -> PResult<(TVal, TType)> {
        Ok(match it.typ {
            ItemType::Str | ItemType::RawStr => (TVal::Str(it.val.clone()), TType::Str),
            ItemType::StrEsc => (TVal::Str(self.replace_escapes(it, &it.val)?), TType::Str),
            ItemType::MultilineStr => {
                let s = strip_escaped_newlines(strip_first_newline(&it.val));
                (TVal::Str(self.replace_escapes(it, &s)?), TType::Str)
            }
            ItemType::RawMultilineStr => (
                TVal::Str(strip_first_newline(&it.val).to_string()),
                TType::Str,
            ),
            ItemType::Integer => (self.value_integer(it)?, TType::Integer),
            ItemType::Float => (self.value_float(it)?, TType::Float),
            ItemType::Bool => match it.val.as_str() {
                "true" => (TVal::Bool(true), TType::Bool),
                "false" => (TVal::Bool(false), TType::Bool),
                _ => return Err(self.bug(format!("Expected boolean value, but got '{}'.", it.val))),
            },
            ItemType::Datetime => (self.value_datetime(it)?, TType::Datetime),
            other => return Err(self.bug(format!("Unexpected value type: {other:?}"))),
        })
    }

    fn value_integer(&mut self, it: &Item) -> PResult<TVal> {
        if !num_underscores_ok(&it.val) {
            return Err(self.err_at(
                it.pos,
                format!(
                    "Invalid integer {}: underscores must be surrounded by digits",
                    quote(&it.val)
                ),
            ));
        }
        if num_has_leading_zero(&it.val) {
            return Err(self.err_at(
                it.pos,
                format!(
                    "Invalid integer {}: cannot have leading zeroes",
                    quote(&it.val)
                ),
            ));
        }
        match parse_int(&it.val, 0) {
            Ok(n) => Ok(TVal::Int(n)),
            Err(crate::yaml::strconv::NumErr::Range) => {
                Err(self.err_at(it.pos, format!("{} is out of range for int64", it.val)))
            }
            Err(_) => Err(self.bug(format!("Expected integer value, but got '{}'.", it.val))),
        }
    }

    fn value_float(&mut self, it: &Item) -> PResult<TVal> {
        let parts: Vec<&str> = it
            .val
            .split(['.', 'e', 'E'])
            .filter(|p| !p.is_empty())
            .collect();
        for part in &parts {
            if !num_underscores_ok(part) {
                return Err(self.err_at(
                    it.pos,
                    format!(
                        "Invalid float {}: underscores must be surrounded by digits",
                        quote(&it.val)
                    ),
                ));
            }
        }
        if !parts.is_empty() && num_has_leading_zero(parts[0]) {
            return Err(self.err_at(
                it.pos,
                format!(
                    "Invalid float {}: cannot have leading zeroes",
                    quote(&it.val)
                ),
            ));
        }
        if !num_periods_ok(&it.val) {
            return Err(self.err_at(
                it.pos,
                format!(
                    "Invalid float {}: '.' must be followed by one or more digits",
                    quote(&it.val)
                ),
            ));
        }
        let mut val: String = it.val.chars().filter(|&c| c != '_').collect();
        let mut signbit = false;
        if val == "+nan" || val == "-nan" {
            signbit = val == "-nan";
            val = "nan".into();
        }
        let num = match parse_float(&val) {
            Ok(f) => f,
            Err(crate::yaml::strconv::NumErr::Range) => {
                return Err(self.err_at(it.pos, format!("{} is out of range for float64", it.val)));
            }
            Err(_) => {
                return Err(self.err_at(it.pos, format!("Invalid float value: {}", quote(&it.val))));
            }
        };
        Ok(TVal::Float(if signbit { -num.abs() } else { num }))
    }

    fn value_datetime(&mut self, it: &Item) -> PResult<TVal> {
        let v: String = it
            .val
            .chars()
            .map(|c| match c {
                'z' => 'Z',
                't' | ' ' => 'T',
                c => c,
            })
            .collect();
        match parse_toml_datetime(&v) {
            Some(s) => Ok(TVal::Datetime(s)),
            None => Err(self.err_at(it.pos, format!("invalid datetime: {}", quote(&v)))),
        }
    }

    fn value_array(&mut self, _it: &Item) -> PResult<(TVal, TType)> {
        self.array_begin();
        let mut array = Vec::new();
        while let Some(it) = self.array_next()? {
            let (v, _) = self.value(&it, true)?;
            array.push(v);
        }
        Ok((TVal::Array(array), TType::Array))
    }

    #[inline(never)]
    fn array_begin(&mut self) {
        let ck = self.current_key.clone();
        self.set_type(&ck, TType::Array);
    }

    /// The next array element item (skipping comments); `None` at the end.
    #[inline(never)]
    fn array_next(&mut self) -> PResult<Option<Box<Item>>> {
        loop {
            let it = self.next()?;
            if it.typ == ItemType::ArrayEnd {
                return Ok(None);
            }
            if it.typ == ItemType::CommentStart {
                self.expect(ItemType::Text)?;
                continue;
            }
            return Ok(Some(Box::new(it)));
        }
    }

    fn value_inline_table(&mut self, _it: &Item, parent_is_array: bool) -> PResult<(TVal, TType)> {
        // Small frame: the bulky work happens in non-inlined helpers.
        let mut st = self.inline_begin(parent_is_array)?;
        while let Some(parent) = self.inline_key()? {
            let vit = self.next()?;
            let (val, typ) = self.value(&vit, false)?;
            self.inline_store(&mut st, parent, val, typ)?;
        }
        Ok((self.inline_end(st), TType::Hash))
    }

    #[inline(never)]
    fn inline_begin(&mut self, parent_is_array: bool) -> PResult<Box<InlineState>> {
        let st = Box::new(InlineState {
            top: BTreeMap::new(),
            outer_context: self.context.clone(),
            outer_key: self.current_key.clone(),
            prev_context: Vec::new(),
        });
        self.context.push(self.current_key.clone());
        let mut st = st;
        st.prev_context = self.context.clone();
        self.current_key.clear();
        let c = self.context.clone();
        self.add_implicit(&c);
        self.add_context(&c, parent_is_array)?;
        Ok(st)
    }

    /// Reads the next key of an inline table (skipping comments); `None` at
    /// the end of the table. Returns the dotted parent of the key.
    #[inline(never)]
    fn inline_key(&mut self) -> PResult<Option<Vec<String>>> {
        loop {
            let it = self.next()?;
            if it.typ == ItemType::InlineTableEnd {
                return Ok(None);
            }
            if it.typ == ItemType::CommentStart {
                self.expect(ItemType::Text)?;
                continue;
            }
            break;
        }
        let mut k = self.next_pos()?;
        let mut key = Vec::new();
        while k.typ != ItemType::KeyEnd && k.typ != ItemType::Eof {
            key.push(self.key_string(&k)?);
            k = self.next()?;
        }
        if k.typ != ItemType::KeyEnd {
            return Err(self.bug(format!("Expected 'KeyEnd' but got '{:?}'.", k.typ)));
        }
        self.current_key = key.last().cloned().unwrap_or_default();
        let parent = key[..key.len().saturating_sub(1)].to_vec();
        for p in &parent {
            let mut c = self.context.clone();
            c.push(p.clone());
            self.add_implicit_context(&c)?;
        }
        Ok(Some(parent))
    }

    #[inline(never)]
    fn inline_store(
        &mut self,
        st: &mut InlineState,
        parent: Vec<String>,
        val: TVal,
        typ: TType,
    ) -> PResult<()> {
        let ck = self.current_key.clone();
        self.set_value(&ck, val.clone())?;
        self.set_type(&ck, typ);
        let mut hash = &mut st.top;
        for c in &parent {
            let entry = hash
                .entry(c.clone())
                .or_insert_with(|| TVal::Table(BTreeMap::new()));
            match entry {
                TVal::Table(t) => hash = t,
                _ => {
                    return Err(
                        self.panicf(format!("{} is not a table", quote(&key_str(&self.context))))
                    );
                }
            }
        }
        hash.insert(ck, val);
        self.context = st.prev_context.clone();
        Ok(())
    }

    #[inline(never)]
    #[allow(clippy::boxed_local)]
    fn inline_end(&mut self, st: Box<InlineState>) -> TVal {
        let st = *st;
        self.context = st.outer_context;
        self.current_key = st.outer_key;
        TVal::Table(st.top)
    }

    /// Navigates `mapping` along `path` (following the last element of
    /// arrays of tables); `None` if a step is missing or not a table.
    fn table_at(&mut self, path: &[String]) -> Result<&mut BTreeMap<String, TVal>, usize> {
        let mut hash = &mut self.mapping;
        for (i, k) in path.iter().enumerate() {
            match hash.get_mut(k) {
                Some(TVal::Table(t)) => hash = t,
                Some(TVal::ArrayTables(ts)) => match ts.last_mut() {
                    Some(t) => hash = t,
                    None => return Err(i),
                },
                Some(_) => return Err(i),
                None => return Err(usize::MAX - i),
            }
        }
        Ok(hash)
    }

    fn add_context(&mut self, key: &[String], array: bool) -> PResult<()> {
        let parent = &key[..key.len().saturating_sub(1)];
        let mut key_context: Vec<String> = Vec::new();
        for k in parent {
            key_context.push(k.clone());
            let kc = key_context.clone();
            let exists = matches!(self.table_at(&kc[..kc.len() - 1]), Ok(h) if h.contains_key(k));
            if !exists {
                self.add_implicit(&kc);
                if let Ok(h) = self.table_at(&kc[..kc.len() - 1]) {
                    h.insert(k.clone(), TVal::Table(BTreeMap::new()));
                }
            }
            match self.table_at(&kc) {
                Ok(_) => {}
                Err(_) => {
                    return Err(self.panicf(format!(
                        "Key '{}' was already created as a hash.",
                        key_str(&kc)
                    )));
                }
            }
        }
        self.context = key_context.clone();
        let last = key.last().cloned().unwrap_or_default();
        if array {
            let kc = key_context.clone();
            let h = match self.table_at(&kc) {
                Ok(h) => h,
                Err(_) => return Err(self.bug("Context for key has not been established.".into())),
            };
            let entry = h
                .entry(last.clone())
                .or_insert_with(|| TVal::ArrayTables(Vec::new()));
            match entry {
                TVal::ArrayTables(ts) => ts.push(BTreeMap::new()),
                _ => {
                    return Err(self.panicf(format!(
                        "Key '{}' was already created and cannot be used as an array.",
                        key_str(key)
                    )));
                }
            }
        } else {
            self.set_value(&last, TVal::Table(BTreeMap::new()))?;
        }
        self.context.push(last);
        Ok(())
    }

    fn set_value(&mut self, key: &str, value: TVal) -> PResult<()> {
        let ctx = self.context.clone();
        if let Err(i) = self.table_at(&ctx) {
            if i > usize::MAX / 2 {
                let n = (usize::MAX - i + 1).min(ctx.len());
                return Err(self.bug(format!(
                    "Context for key '{}' has not been established.",
                    key_str(&ctx[..n])
                )));
            }
            return Err(self.panicf(format!(
                "Key '{}' has already been defined.",
                key_str(&ctx[..=i])
            )));
        }
        let mut key_context = ctx.clone();
        key_context.push(key.to_string());
        let exists = matches!(self.table_at(&ctx), Ok(h) if h.contains_key(key));
        if exists {
            if self.is_array(&key_context) {
                if !self.is_implicit(&key_context) {
                    return Err(self.panicf(format!(
                        "Key '{}' has already been defined.",
                        key_str(&key_context)
                    )));
                }
                self.remove_implicit(&key_context);
                if let Ok(h) = self.table_at(&ctx) {
                    h.insert(key.to_string(), value);
                }
                return Ok(());
            }
            if self.is_implicit(&key_context) {
                self.remove_implicit(&key_context);
                return Ok(());
            }
            return Err(self.panicf(format!(
                "Key '{}' has already been defined.",
                key_str(&key_context)
            )));
        }
        if let Ok(h) = self.table_at(&ctx) {
            h.insert(key.to_string(), value);
        }
        Ok(())
    }

    fn set_type(&mut self, key: &str, typ: TType) {
        let mut kc = self.context.clone();
        if !key.is_empty() {
            kc.push(key.to_string());
        }
        if kc.is_empty() {
            kc.push(String::new());
        }
        self.key_info.insert(key_str(&kc), typ);
    }

    fn add_implicit(&mut self, key: &[String]) {
        self.implicits.insert(key_str(key));
    }
    fn remove_implicit(&mut self, key: &[String]) {
        self.implicits.remove(&key_str(key));
    }
    fn is_implicit(&self, key: &[String]) -> bool {
        self.implicits.contains(&key_str(key))
    }
    fn is_array(&self, key: &[String]) -> bool {
        self.key_info.get(&key_str(key)) == Some(&TType::Array)
    }
    fn add_implicit_context(&mut self, key: &[String]) -> PResult<()> {
        self.add_implicit(key);
        self.add_context(key, false)
    }

    fn replace_escapes(&mut self, it: &Item, s: &str) -> PResult<String> {
        let mut out = String::with_capacity(s.len());
        let b = s.as_bytes();
        let mut skip = 0usize;
        for (i, c) in s.char_indices() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let e = b.get(i + 1).copied().unwrap_or(0);
            match e {
                b' ' | b'\t' => {
                    return Err(self.err_at(it.pos, format!("invalid escape: '\\{}'", e as char)));
                }
                b'b' => out.push('\u{8}'),
                b't' => out.push('\t'),
                b'n' => out.push('\n'),
                b'f' => out.push('\u{c}'),
                b'r' => out.push('\r'),
                b'e' => out.push('\u{1b}'),
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'x' | b'u' | b'U' => {
                    let n = match e {
                        b'x' => 2,
                        b'u' => 4,
                        _ => 8,
                    };
                    let hex = s.get(i + 2..i + 2 + n).unwrap_or("");
                    let v = u32::from_str_radix(&hex.to_ascii_lowercase(), 16).unwrap_or(u32::MAX);
                    match char::from_u32(v) {
                        Some(ch) => out.push(ch),
                        None => {
                            return Err(self.err_at(
                                it.pos,
                                format!("Escaped character '\\u{hex}' is not valid UTF-8."),
                            ));
                        }
                    }
                    skip = n + 1;
                    continue;
                }
                other => {
                    return Err(self.bug(format!(
                        "Expected valid escape code after \\, but got {}.",
                        q_rune(other as char)
                    )));
                }
            }
            skip = 1;
        }
        Ok(out)
    }
}

fn strip_first_newline(s: &str) -> &str {
    if let Some(r) = s.strip_prefix('\n') {
        return r;
    }
    if let Some(r) = s.strip_prefix("\r\n") {
        return r;
    }
    s
}

fn strip_escaped_newlines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut s = s.to_string();
    let mut i = 0usize;
    loop {
        let Some(ix) = s[i..].find('\\') else {
            out.push_str(&s);
            return out;
        };
        i += ix;
        let b = s.as_bytes();
        if b.len() > i + 1 && b[i + 1] == b'\\' {
            i += 2;
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && matches!(b[j], b' ' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        if j == i + 1 {
            i += 1;
            continue;
        }
        if !s[i..j].contains('\n') {
            i += 1;
            continue;
        }
        out.push_str(&s[..i]);
        s = s[j..].to_string();
        i = 0;
    }
}

fn num_has_leading_zero(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() > 1 && b[0] == b'0' && !(b[1] == b'b' || b[1] == b'o' || b[1] == b'x') {
        return true;
    }
    b.len() > 2 && (b[0] == b'-' || b[0] == b'+') && b[1] == b'0'
}

fn num_underscores_ok(s: &str) -> bool {
    if matches!(s, "nan" | "+nan" | "-nan" | "inf" | "-inf" | "+inf") {
        return true;
    }
    let mut accept = false;
    for r in s.chars() {
        if r == '_' && !accept {
            return false;
        }
        accept = is_hex(r);
    }
    accept
}

fn num_periods_ok(s: &str) -> bool {
    let mut period = false;
    for r in s.chars() {
        if period && !is_digit(r) {
            return false;
        }
        period = r == '.';
    }
    !period
}

// ---------------------------------------------------------------------------
// Datetimes: time.ParseInLocation with BurntSushi's layouts.

#[derive(Default)]
struct DateTime {
    date: Option<(i64, u32, u32)>,
    time: Option<(u32, u32, u32, String)>,
    offset: Option<i32>,
}

fn digits(s: &[u8], n: usize) -> Option<(u32, &[u8])> {
    if s.len() < n || !s[..n].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((
        s[..n].iter().fold(0u32, |a, c| a * 10 + (c - b'0') as u32),
        &s[n..],
    ))
}

fn lit(s: &[u8], c: u8) -> Option<&[u8]> {
    if s.first() == Some(&c) {
        Some(&s[1..])
    } else {
        None
    }
}

/// Parses one layout; `with_seconds`/`frac` select the variants.
fn parse_dt(v: &str, date: bool, time: bool, seconds: bool, zone: bool) -> Option<DateTime> {
    let mut s = v.as_bytes();
    let mut dt = DateTime::default();
    if date {
        let (y, r) = digits(s, 4)?;
        let r = lit(r, b'-')?;
        let (m, r) = digits(r, 2)?;
        let r = lit(r, b'-')?;
        let (d, r) = digits(r, 2)?;
        if !(1..=12).contains(&m) {
            return None;
        }
        let dim = match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            _ => {
                let y = y as i64;
                if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                    29
                } else {
                    28
                }
            }
        };
        if d < 1 || d > dim {
            return None;
        }
        dt.date = Some((y as i64, m, d));
        s = r;
        if time {
            s = lit(s, b'T')?;
        }
    }
    if time {
        // "15" accepts one or two digits.
        let (h, r) = match digits(s, 2) {
            Some(x) => x,
            None => digits(s, 1)?,
        };
        if h >= 24 {
            return None;
        }
        let r = lit(r, b':')?;
        let (mi, r) = digits(r, 2)?;
        if mi >= 60 {
            return None;
        }
        let mut r = r;
        let mut sec = 0;
        let mut frac = String::new();
        if seconds {
            r = lit(r, b':')?;
            let (sv, rr) = digits(r, 2)?;
            if sv >= 60 {
                return None;
            }
            sec = sv;
            r = rr;
            if r.len() >= 2 && (r[0] == b'.' || r[0] == b',') && r[1].is_ascii_digit() {
                let mut i = 1;
                while i < r.len() && r[i].is_ascii_digit() {
                    i += 1;
                }
                frac = core::str::from_utf8(&r[1..i]).unwrap_or("").to_string();
                r = &r[i..];
            }
        }
        dt.time = Some((h, mi, sec, frac));
        s = r;
    }
    if zone {
        if let Some(r) = lit(s, b'Z') {
            dt.offset = Some(0);
            s = r;
        } else {
            if s.len() < 6 || s[3] != b':' {
                return None;
            }
            let sign = s[0];
            let (hh, _) = digits(&s[1..3], 2)?;
            let (mm, _) = digits(&s[4..6], 2)?;
            if hh > 24 || mm > 60 {
                return None;
            }
            let off = (hh * 60 + mm) as i32 * 60;
            dt.offset = Some(match sign {
                b'+' => off,
                b'-' => -off,
                _ => return None,
            });
            s = &s[6..];
        }
    }
    if !s.is_empty() {
        return None;
    }
    Some(dt)
}

/// `missingLeadingZero(d, layout)` for the layout prefix (separators must
/// line up with the zero-padded layout).
fn missing_leading_zero(d: &str, layout: &str) -> bool {
    let d = d.as_bytes();
    for (i, &c) in layout.as_bytes().iter().enumerate() {
        if c == b'.' || c == b'Z' {
            return false;
        }
        if !c.is_ascii_digit() && d.get(i) != Some(&c) {
            return true;
        }
    }
    false
}

/// A TOML datetime as decoded by BurntSushi (a Go `time.Time`), assuming
/// the local time zone is UTC.
#[derive(Clone, Debug)]
pub struct TomlDatetime {
    /// RFC 3339 (`time.RFC3339Nano`), as `encoding/json` prints it.
    json: String,
    /// The form BurntSushi's encoder writes back.
    toml: String,
    /// Go's `time.Time.String()`.
    display: String,
}

impl crate::value::Object for TomlDatetime {
    fn type_name(&self) -> &str {
        "time.Time"
    }
    fn format(&self, out: &mut String) {
        out.push_str(&self.display);
    }
    fn to_data(&self) -> Value {
        Value::str(&self.json)
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

fn parse_toml_datetime(v: &str) -> Option<TomlDatetime> {
    // (layout, date, time, seconds, zone)
    const LAYOUTS: [(&str, bool, bool, bool, bool); 7] = [
        (
            "2006-01-02T15:04:05.999999999Z07:00",
            true,
            true,
            true,
            true,
        ),
        ("2006-01-02T15:04:05.999999999", true, true, true, false),
        ("2006-01-02", true, false, false, false),
        ("15:04:05.999999999", false, true, true, false),
        ("2006-01-02T15:04Z07:00", true, true, false, true),
        ("2006-01-02T15:04", true, true, false, false),
        ("15:04", false, true, false, false),
    ];
    for (layout, date, time, seconds, zone) in LAYOUTS {
        if let Some(dt) = parse_dt(v, date, time, seconds, zone) {
            if missing_leading_zero(v, layout) {
                return None;
            }
            let (y, mo, d) = dt.date.unwrap_or((0, 1, 1));
            let (h, mi, s, frac) = dt.time.unwrap_or((0, 0, 0, String::new()));
            let mut frac = frac;
            frac.truncate(9);
            while frac.ends_with('0') {
                frac.pop();
            }
            let frac = if frac.is_empty() {
                String::new()
            } else {
                format!(".{frac}")
            };
            let date_s = format!("{y:04}-{mo:02}-{d:02}");
            let time_s = format!("{h:02}:{mi:02}:{s:02}{frac}");
            let off = dt.offset.unwrap_or(0);
            let (zone_rfc, zone_num) = if off == 0 {
                ("Z".to_string(), "+0000".to_string())
            } else {
                let sign = if off < 0 { '-' } else { '+' };
                let a = off.unsigned_abs();
                (
                    format!("{sign}{:02}:{:02}", a / 3600, (a / 60) % 60),
                    format!("{sign}{:02}{:02}", a / 3600, (a / 60) % 60),
                )
            };
            let json = format!("{date_s}T{time_s}{zone_rfc}");
            let (toml, zone_name) = match (date, time, zone) {
                (true, true, true) => (
                    json.clone(),
                    if off == 0 {
                        "UTC".to_string()
                    } else {
                        zone_num.clone()
                    },
                ),
                (true, true, false) => (format!("{date_s}T{time_s}"), "datetime-local".to_string()),
                (true, false, _) => (date_s.clone(), "date-local".to_string()),
                _ => (time_s.clone(), "time-local".to_string()),
            };
            let display = format!("{date_s} {time_s} {zone_num} {zone_name}");
            return Some(TomlDatetime {
                json,
                toml,
                display,
            });
        }
    }
    None
}
// ---------------------------------------------------------------------------

fn to_value(v: TVal) -> Value {
    match v {
        TVal::Str(s) => Value::string(s),
        TVal::Datetime(d) => Value::object(d),
        TVal::Int(i) => Value::Int64(i),
        TVal::Float(f) => Value::Float(f),
        TVal::Bool(b) => Value::Bool(b),
        TVal::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(to_value(it));
            }
            Value::list(out)
        }
        TVal::Table(t) => table_value(t),
        TVal::ArrayTables(ts) => {
            let mut out = Vec::with_capacity(ts.len());
            for t in ts {
                out.push(table_value(t));
            }
            Value::list(out)
        }
    }
}

fn table_value(t: BTreeMap<String, TVal>) -> Value {
    let mut m = Map::new();
    for (k, v) in t {
        m.insert(k, to_value(v));
    }
    Value::map(m)
}

/// `toml.Unmarshal` into `map[string]interface{}`.
pub fn from_str(s: &str) -> Result<Value, String> {
    let mut data = s;
    if let Some(r) = data.strip_prefix('\u{feff}') {
        data = r;
    }
    let ex = data.len().min(6);
    if data.as_bytes()[..ex].contains(&0) {
        return Err(
            "toml: line 1: files cannot contain NULL bytes; probably using UTF-16; TOML files must be UTF-8".into()
        );
    }
    let mut p = Parser {
        lx: Lexer::new(data),
        context: Vec::new(),
        current_key: String::new(),
        pos: Pos::default(),
        key_info: BTreeMap::new(),
        mapping: BTreeMap::new(),
        implicits: BTreeSet::new(),
        depth: 0,
    };
    loop {
        let item = p.next().map_err(|e| e.render())?;
        if item.typ == ItemType::Eof {
            break;
        }
        p.top_level(item).map_err(|e| e.render())?;
    }
    Ok(table_value(core::mem::take(&mut p.mapping)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map;

    #[test]
    fn roundtrip() {
        let v = from_str("a = 1\nb = \"x\"\n[c]\nd = [1, 2.5]\n[[e]]\nf = true\n").unwrap();
        assert_eq!(
            v,
            map! {
                "a" => Value::Int64(1),
                "b" => "x",
                "c" => map! { "d" => Value::list(alloc::vec![Value::Int64(1), Value::Float(2.5)]) },
                "e" => Value::list(alloc::vec![map! { "f" => true }])
            }
        );
        assert_eq!(
            to_string(&v).unwrap(),
            "a = 1\nb = \"x\"\n\n[c]\n  d = [1, 2.5]\n\n[[e]]\n  f = true\n"
        );
        assert_eq!(
            from_str("a = ").unwrap_err(),
            "toml: line 1 (last key \"a\"): unexpected EOF; expected value"
        );
    }
}
