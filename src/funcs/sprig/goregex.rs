//! Go `regexp` (RE2 syntax) on top of `regex-automata`.
//!
//! Patterns are parsed with a port of Go's `regexp/syntax` parser (Perl
//! flags, as `regexp.Compile` uses), which reproduces Go's syntax errors, and
//! then re-emitted in an unambiguous `regex-syntax` form (every literal
//! escaped, Go's ASCII-only `\d`/`\s`/`\w`/`\b` spelled out). Matching uses
//! leftmost-first semantics, like Go. The `Find*`/`ReplaceAll*`/`Split`
//! iteration rules (empty matches, `$name` expansion) follow `regexp.go`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;

use regex_automata::meta::Regex;
use regex_automata::util::captures::Captures;
use regex_automata::Input;

use crate::gofmt::strconv;

use super::gounicode;

const MAX_RUNE: u32 = 0x10FFFF;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Flags {
    fold: bool,
    one_line: bool,
    dot_nl: bool,
    non_greedy: bool,
}

/// A class item before emission.
#[derive(Clone, Debug)]
enum ClassItem {
    Range(u32, u32),
    /// `\p{Name}` (true = negated).
    Unicode(String, bool),
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    NoMatch,
    Lit(char, bool),
    AnyNotNl,
    Any,
    BeginLine,
    EndLine,
    BeginText,
    EndText,
    WordB,
    NoWordB,
    Class { items: Vec<ClassItem>, negated: bool, fold: bool },
    Capture(usize, Box<Node>),
    Repeat { min: i64, max: i64, greedy: bool, sub: Box<Node> },
    Concat(Vec<Node>),
    Alt(Vec<Node>),
}

enum Item {
    Node(Node),
    LeftParen { cap: usize, name: Option<String>, flags: Flags },
    Bar,
}

/// A Go regexp syntax error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub code: &'static str,
    pub expr: String,
}

impl SyntaxError {
    fn new(code: &'static str, expr: &str) -> SyntaxError {
        SyntaxError { code, expr: expr.to_string() }
    }
    /// `(*syntax.Error).Error()`.
    pub fn message(&self) -> String {
        format!("error parsing regexp: {}: `{}`", self.code, self.expr)
    }
}

const ERR_INVALID_CHAR_RANGE: &str = "invalid character class range";
const ERR_INVALID_ESCAPE: &str = "invalid escape sequence";
const ERR_INVALID_NAMED_CAPTURE: &str = "invalid named capture";
const ERR_INVALID_PERL_OP: &str = "invalid or unsupported Perl syntax";
const ERR_INVALID_REPEAT_OP: &str = "invalid nested repetition operator";
const ERR_INVALID_REPEAT_SIZE: &str = "invalid repeat count";
const ERR_MISSING_BRACKET: &str = "missing closing ]";
const ERR_MISSING_PAREN: &str = "missing closing )";
const ERR_MISSING_REPEAT_ARGUMENT: &str = "missing argument to repetition operator";
const ERR_TRAILING_BACKSLASH: &str = "trailing backslash at end of expression";
const ERR_UNEXPECTED_PAREN: &str = "unexpected )";
const ERR_NESTING_DEPTH: &str = "expression nests too deeply";
const ERR_LARGE: &str = "expression too large";

const PERL_D: &[(u32, u32)] = &[(0x30, 0x39)];
const PERL_S: &[(u32, u32)] = &[(0x9, 0xa), (0xc, 0xd), (0x20, 0x20)];
const PERL_W: &[(u32, u32)] = &[(0x30, 0x39), (0x41, 0x5a), (0x5f, 0x5f), (0x61, 0x7a)];

fn posix_group(name: &str) -> Option<(bool, &'static [(u32, u32)])> {
    let (neg, base) = match name.strip_prefix("[:^") {
        Some(rest) => (true, rest),
        None => (false, name.strip_prefix("[:")?),
    };
    let class: &'static [(u32, u32)] = match base {
        "alnum:]" => &[(0x30, 0x39), (0x41, 0x5a), (0x61, 0x7a)],
        "alpha:]" => &[(0x41, 0x5a), (0x61, 0x7a)],
        "ascii:]" => &[(0x0, 0x7f)],
        "blank:]" => &[(0x9, 0x9), (0x20, 0x20)],
        "cntrl:]" => &[(0x0, 0x1f), (0x7f, 0x7f)],
        "digit:]" => &[(0x30, 0x39)],
        "graph:]" => &[(0x21, 0x7e)],
        "lower:]" => &[(0x61, 0x7a)],
        "print:]" => &[(0x20, 0x7e)],
        "punct:]" => &[(0x21, 0x2f), (0x3a, 0x40), (0x5b, 0x60), (0x7b, 0x7e)],
        "space:]" => &[(0x9, 0xd), (0x20, 0x20)],
        "upper:]" => &[(0x41, 0x5a)],
        "word:]" => &[(0x30, 0x39), (0x41, 0x5a), (0x5f, 0x5f), (0x61, 0x7a)],
        "xdigit:]" => &[(0x30, 0x39), (0x41, 0x46), (0x61, 0x66)],
        _ => return None,
    };
    Some((neg, class))
}

fn negate_ranges(x: &[(u32, u32)]) -> Vec<ClassItem> {
    let mut out = Vec::new();
    let mut next_lo = 0u32;
    for &(lo, hi) in x {
        if lo > next_lo {
            out.push(ClassItem::Range(next_lo, lo - 1));
        }
        next_lo = hi + 1;
    }
    if next_lo <= MAX_RUNE {
        out.push(ClassItem::Range(next_lo, MAX_RUNE));
    }
    out
}

/// Go's `regexp/syntax` canonicalization of `\p{...}` names.
fn canonical_name(name: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for &c in name.as_bytes() {
        let c = match c {
            b'_' | b'-' | b' ' => continue,
            c if first => {
                first = false;
                c.to_ascii_uppercase()
            }
            c => c.to_ascii_lowercase(),
        };
        out.push(c as char);
    }
    out
}

/// Resolves a Unicode class name as Go does; returns the name to use in
/// `regex-syntax` and whether Go inverts it (`Assigned`).
fn unicode_table(name: &str) -> Option<(String, bool)> {
    let canon = canonical_name(name);
    match canon.as_str() {
        "Any" => return Some(("Any".into(), false)),
        "Assigned" => return Some(("Cn".into(), true)),
        "Ascii" => return Some(("ASCII".into(), false)),
        "Lc" => return Some(("LC".into(), false)),
        _ => {}
    }
    for &n in super::regex_names::CATEGORIES {
        if n == canon {
            return Some((n.into(), false));
        }
    }
    for &n in super::regex_names::SCRIPTS {
        if n == canon {
            return Some((n.into(), false));
        }
    }
    for &(alias, actual) in super::regex_names::CATEGORY_ALIASES {
        if canonical_name(alias) == canon {
            return Some((actual.into(), false));
        }
    }
    for &n in super::regex_names::SCRIPTS {
        if canonical_name(n) == canon {
            return Some((n.into(), false));
        }
    }
    None
}

fn is_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn unhex(c: char) -> i64 {
    match c {
        '0'..='9' => c as i64 - '0' as i64,
        'a'..='f' => c as i64 - 'a' as i64 + 10,
        'A'..='F' => c as i64 - 'A' as i64 + 10,
        _ => -1,
    }
}

fn next_char(s: &str) -> (char, &str) {
    match s.chars().next() {
        Some(c) => (c, &s[c.len_utf8()..]),
        None => ('\u{FFFD}', s),
    }
}

/// `parseEscape`: returns (rune, rest).
fn parse_escape(s: &str) -> Result<(char, &str), SyntaxError> {
    let t = &s[1..];
    if t.is_empty() {
        return Err(SyntaxError::new(ERR_TRAILING_BACKSLASH, ""));
    }
    let (c, mut t) = next_char(t);
    let bad = |t: &str| Err(SyntaxError::new(ERR_INVALID_ESCAPE, &s[..s.len() - t.len()]));
    match c {
        '1'..='7' if t.is_empty() || !(b'0'..=b'7').contains(&t.as_bytes()[0]) => bad(t),
        '0'..='7' => {
            let mut r = c as u32 - '0' as u32;
            for _ in 1..3 {
                if t.is_empty() || !(b'0'..=b'7').contains(&t.as_bytes()[0]) {
                    break;
                }
                r = r * 8 + (t.as_bytes()[0] - b'0') as u32;
                t = &t[1..];
            }
            Ok((char::from_u32(r).unwrap_or('\u{FFFD}'), t))
        }
        'x' => {
            if t.is_empty() {
                return bad(t);
            }
            let (c, t2) = next_char(t);
            t = t2;
            if c == '{' {
                let mut nhex = 0;
                let mut r: i64 = 0;
                loop {
                    if t.is_empty() {
                        return bad(t);
                    }
                    let (c, t2) = next_char(t);
                    t = t2;
                    if c == '}' {
                        break;
                    }
                    let v = unhex(c);
                    if v < 0 {
                        return bad(t);
                    }
                    r = r * 16 + v;
                    if r > MAX_RUNE as i64 {
                        return bad(t);
                    }
                    nhex += 1;
                }
                if nhex == 0 {
                    return bad(t);
                }
                // Surrogates are not chars; Go would accept them as runes
                // that never match valid UTF-8.
                return Ok((char::from_u32(r as u32).unwrap_or('\u{FFFD}'), t));
            }
            let x = unhex(c);
            let (c, t2) = if t.is_empty() { ('\u{FFFD}', t) } else { next_char(t) };
            t = t2;
            let y = unhex(c);
            if x < 0 || y < 0 {
                return bad(t);
            }
            Ok((char::from_u32((x * 16 + y) as u32).unwrap_or('\u{FFFD}'), t))
        }
        'a' => Ok(('\u{7}', t)),
        'f' => Ok(('\u{c}', t)),
        'n' => Ok(('\n', t)),
        'r' => Ok(('\r', t)),
        't' => Ok(('\t', t)),
        'v' => Ok(('\u{b}', t)),
        c if (c as u32) < 0x80 && !is_alnum(c) => Ok((c, t)),
        _ => bad(t),
    }
}

struct Parser<'a> {
    flags: Flags,
    stack: Vec<Item>,
    num_cap: usize,
    names: Vec<Option<String>>,
    whole: &'a str,
}

fn perl_group(s: &str) -> Option<(bool, &'static [(u32, u32)])> {
    let b = s.as_bytes();
    if b.len() < 2 || b[0] != b'\\' {
        return None;
    }
    match b[1] {
        b'd' => Some((false, PERL_D)),
        b'D' => Some((true, PERL_D)),
        b's' => Some((false, PERL_S)),
        b'S' => Some((true, PERL_S)),
        b'w' => Some((false, PERL_W)),
        b'W' => Some((true, PERL_W)),
        _ => None,
    }
}

fn group_items(neg: bool, class: &[(u32, u32)], fold: bool) -> Vec<ClassItem> {
    if neg && !fold {
        return negate_ranges(class);
    }
    if neg {
        // Fold, then negate: emitted as a nested negated class.
        return alloc::vec![ClassItem::Unicode(ranges_class(class), true)];
    }
    class.iter().map(|&(a, b)| ClassItem::Range(a, b)).collect()
}

/// A nested class for `group_items` (stored in the Unicode variant's name
/// slot with a `[` marker).
fn ranges_class(class: &[(u32, u32)]) -> String {
    let mut s = String::from("[");
    for &(a, b) in class {
        push_range(&mut s, a, b);
    }
    s.push(']');
    s
}

fn push_char_escape(s: &mut String, c: u32) {
    let _ = write!(s, "\\x{{{c:X}}}");
}

fn push_range(s: &mut String, a: u32, b: u32) {
    push_char_escape(s, a);
    if b != a {
        s.push('-');
        push_char_escape(s, b);
    }
}

impl<'a> Parser<'a> {
    fn push(&mut self, n: Node) {
        self.stack.push(Item::Node(n));
    }

    fn literal(&mut self, c: char) {
        let fold = self.flags.fold;
        self.push(Node::Lit(c, fold));
    }

    fn top_is_pseudo_or_empty(&self) -> bool {
        !matches!(self.stack.last(), Some(Item::Node(_)))
    }

    fn concat(&mut self) {
        let mut i = self.stack.len();
        while i > 0 && matches!(self.stack[i - 1], Item::Node(_)) {
            i -= 1;
        }
        let subs: Vec<Node> = self
            .stack
            .drain(i..)
            .filter_map(|it| if let Item::Node(n) = it { Some(n) } else { None })
            .collect();
        let n = match subs.len() {
            0 => Node::Empty,
            1 => subs.into_iter().next().unwrap_or(Node::Empty),
            _ => {
                let mut flat = Vec::new();
                for s in subs {
                    match s {
                        Node::Concat(v) => flat.extend(v),
                        s => flat.push(s),
                    }
                }
                Node::Concat(flat)
            }
        };
        self.push(n);
    }

    fn alternate(&mut self) {
        let mut i = self.stack.len();
        while i > 0 && matches!(self.stack[i - 1], Item::Node(_)) {
            i -= 1;
        }
        let subs: Vec<Node> = self
            .stack
            .drain(i..)
            .filter_map(|it| if let Item::Node(n) = it { Some(n) } else { None })
            .collect();
        let n = match subs.len() {
            0 => Node::NoMatch,
            1 => subs.into_iter().next().unwrap_or(Node::NoMatch),
            _ => {
                let mut flat = Vec::new();
                for s in subs {
                    match s {
                        Node::Alt(v) => flat.extend(v),
                        s => flat.push(s),
                    }
                }
                Node::Alt(flat)
            }
        };
        self.push(n);
    }

    /// `swapVerticalBar` without the char-class merging optimization.
    fn swap_vertical_bar(&mut self) -> bool {
        let n = self.stack.len();
        if n >= 2 && matches!(self.stack[n - 2], Item::Bar) && matches!(self.stack[n - 1], Item::Node(_)) {
            self.stack.swap(n - 2, n - 1);
            return true;
        }
        false
    }

    fn parse_vertical_bar(&mut self) {
        self.concat();
        if !self.swap_vertical_bar() {
            self.stack.push(Item::Bar);
        }
    }

    fn parse_right_paren(&mut self) -> Result<(), SyntaxError> {
        self.concat();
        if self.swap_vertical_bar() {
            self.stack.pop();
        }
        self.alternate();
        let n = self.stack.len();
        if n < 2 {
            return Err(SyntaxError::new(ERR_UNEXPECTED_PAREN, self.whole));
        }
        let re1 = self.stack.pop();
        let re2 = self.stack.pop();
        match (re1, re2) {
            (Some(Item::Node(node)), Some(Item::LeftParen { cap, name: _, flags })) => {
                self.flags = flags;
                if cap == 0 {
                    self.push(node);
                } else {
                    self.push(Node::Capture(cap, Box::new(node)));
                }
                Ok(())
            }
            _ => Err(SyntaxError::new(ERR_UNEXPECTED_PAREN, self.whole)),
        }
    }

    fn repeat(
        &mut self,
        min: i64,
        max: i64,
        before: &'a str,
        after: &'a str,
        last_repeat: &'a str,
    ) -> Result<&'a str, SyntaxError> {
        let mut after = after;
        let mut non_greedy = self.flags.non_greedy;
        if after.as_bytes().first() == Some(&b'?') {
            after = &after[1..];
            non_greedy = !non_greedy;
        }
        if !last_repeat.is_empty() {
            return Err(SyntaxError::new(ERR_INVALID_REPEAT_OP, &last_repeat[..last_repeat.len() - after.len()]));
        }
        if self.top_is_pseudo_or_empty() {
            return Err(SyntaxError::new(ERR_MISSING_REPEAT_ARGUMENT, &before[..before.len() - after.len()]));
        }
        let Some(Item::Node(sub)) = self.stack.pop() else {
            return Err(SyntaxError::new(ERR_MISSING_REPEAT_ARGUMENT, &before[..before.len() - after.len()]));
        };
        let node = Node::Repeat { min, max, greedy: !non_greedy, sub: Box::new(sub) };
        if before.starts_with('{') && (min >= 2 || max >= 2) && !repeat_is_valid(&node, 1000) {
            return Err(SyntaxError::new(ERR_INVALID_REPEAT_SIZE, &before[..before.len() - after.len()]));
        }
        self.push(node);
        Ok(after)
    }

    fn parse_perl_flags(&mut self, s: &'a str) -> Result<&'a str, SyntaxError> {
        let t = s;
        let b = t.as_bytes();
        let starts_with_p = b.len() > 4 && b[2] == b'P' && b[3] == b'<';
        let starts_with_name = b.len() > 3 && b[2] == b'<';
        if starts_with_p || starts_with_name {
            let expr_start = if starts_with_name { 3 } else { 4 };
            let Some(end) = t.find('>') else {
                return Err(SyntaxError::new(ERR_INVALID_NAMED_CAPTURE, s));
            };
            let capture = &t[..end + 1];
            let name = if expr_start <= end { &t[expr_start..end] } else { "" };
            if name.is_empty() || !name.chars().all(|c| c == '_' || is_alnum(c)) {
                return Err(SyntaxError::new(ERR_INVALID_NAMED_CAPTURE, capture));
            }
            self.num_cap += 1;
            self.names.push(Some(name.to_string()));
            self.stack.push(Item::LeftParen { cap: self.num_cap, name: Some(name.to_string()), flags: self.flags });
            return Ok(&t[end + 1..]);
        }
        let mut t = &t[2..];
        let mut flags = self.flags;
        let mut sign = 1;
        let mut saw_flag = false;
        // With sign < 0, flags are inverted; we track that by toggling.
        let mut inverted = false;
        let set = |flags: &mut Flags, inverted: bool, c: char| {
            // Setting a flag in inverted mode clears it in the result.
            match c {
                'i' => flags.fold = !inverted,
                'm' => flags.one_line = inverted,
                's' => flags.dot_nl = !inverted,
                'U' => flags.non_greedy = !inverted,
                _ => {}
            }
        };
        while !t.is_empty() {
            let (c, rest) = next_char(t);
            t = rest;
            match c {
                'i' | 'm' | 's' | 'U' => {
                    set(&mut flags, inverted, c);
                    saw_flag = true;
                }
                '-' => {
                    if sign < 0 {
                        break;
                    }
                    sign = -1;
                    inverted = true;
                    saw_flag = false;
                }
                ':' | ')' => {
                    if sign < 0 && !saw_flag {
                        break;
                    }
                    if c == ':' {
                        self.stack.push(Item::LeftParen { cap: 0, name: None, flags: self.flags });
                    }
                    self.flags = flags;
                    return Ok(t);
                }
                _ => break,
            }
        }
        Err(SyntaxError::new(ERR_INVALID_PERL_OP, &s[..s.len() - t.len()]))
    }

    /// `parseUnicodeClass`: Ok(None) if `s` is not a `\p`/`\P` escape.
    fn parse_unicode_class(&self, s: &'a str) -> Result<Option<(ClassItem, &'a str)>, SyntaxError> {
        let b = s.as_bytes();
        if b.len() < 2 || b[0] != b'\\' || (b[1] != b'p' && b[1] != b'P') {
            return Ok(None);
        }
        let mut negated = b[1] == b'P';
        let t = &s[2..];
        let (c, t) = next_char(t);
        let (seq, mut name, rest);
        if c != '{' {
            seq = &s[..s.len() - t.len()];
            name = &seq[2..];
            rest = t;
        } else {
            let Some(end) = s.find('}') else {
                return Err(SyntaxError::new(ERR_INVALID_CHAR_RANGE, s));
            };
            seq = &s[..end + 1];
            rest = &s[end + 1..];
            name = &s[3..end];
        }
        if let Some(n) = name.strip_prefix('^') {
            negated = !negated;
            name = n;
        }
        let Some((rname, inv)) = unicode_table(name) else {
            return Err(SyntaxError::new(ERR_INVALID_CHAR_RANGE, seq));
        };
        if inv {
            negated = !negated;
        }
        Ok(Some((ClassItem::Unicode(rname, negated), rest)))
    }

    fn parse_class_char(&self, s: &'a str, whole: &str) -> Result<(char, &'a str), SyntaxError> {
        if s.is_empty() {
            return Err(SyntaxError::new(ERR_MISSING_BRACKET, whole));
        }
        if s.as_bytes()[0] == b'\\' {
            return parse_escape(s);
        }
        Ok(next_char(s))
    }

    fn parse_class(&mut self, s: &'a str) -> Result<&'a str, SyntaxError> {
        let mut t = &s[1..];
        let fold = self.flags.fold;
        let mut items: Vec<ClassItem> = Vec::new();
        let mut negated = false;
        if t.as_bytes().first() == Some(&b'^') {
            negated = true;
            t = &t[1..];
        }
        let mut first = true;
        while t.is_empty() || t.as_bytes()[0] != b']' || first {
            first = false;
            if t.len() > 2 && t.as_bytes()[0] == b'[' && t.as_bytes()[1] == b':' {
                if let Some(i) = t[2..].find(":]") {
                    let i = i + 2;
                    let name = &t[..i + 2];
                    match posix_group(name) {
                        Some((neg, class)) => {
                            items.extend(group_items(neg, class, fold));
                            t = &t[i + 2..];
                            continue;
                        }
                        None => return Err(SyntaxError::new(ERR_INVALID_CHAR_RANGE, name)),
                    }
                }
            }
            if let Some((item, rest)) = self.parse_unicode_class(t)? {
                items.push(item);
                t = rest;
                continue;
            }
            if let Some((neg, class)) = perl_group(t) {
                items.extend(group_items(neg, class, fold));
                t = &t[2..];
                continue;
            }
            let rng = t;
            let (lo, rest) = self.parse_class_char(t, s)?;
            t = rest;
            let mut hi = lo;
            if t.len() >= 2 && t.as_bytes()[0] == b'-' && t.as_bytes()[1] != b']' {
                t = &t[1..];
                let (h, rest) = self.parse_class_char(t, s)?;
                t = rest;
                hi = h;
                if hi < lo {
                    return Err(SyntaxError::new(ERR_INVALID_CHAR_RANGE, &rng[..rng.len() - t.len()]));
                }
            }
            items.push(ClassItem::Range(lo as u32, hi as u32));
        }
        t = &t[1..];
        self.push(Node::Class { items, negated, fold });
        Ok(t)
    }
}

fn parse_repeat(s: &str) -> Option<(i64, i64, &str)> {
    fn parse_int(s: &str) -> Option<(i64, &str)> {
        let b = s.as_bytes();
        if b.is_empty() || !b[0].is_ascii_digit() {
            return None;
        }
        if b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_digit() {
            return None;
        }
        let mut i = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let mut n: i64 = 0;
        for &c in &b[..i] {
            if n >= 100_000_000 {
                n = -1;
                break;
            }
            n = n * 10 + (c - b'0') as i64;
        }
        Some((n, &s[i..]))
    }
    let s = s.strip_prefix('{')?;
    let (min, mut s) = parse_int(s)?;
    let max;
    let mut min = min;
    if s.is_empty() {
        return None;
    }
    if s.as_bytes()[0] != b',' {
        max = min;
    } else {
        s = &s[1..];
        if s.is_empty() {
            return None;
        }
        if s.as_bytes()[0] == b'}' {
            max = -1;
        } else {
            let (m, rest) = parse_int(s)?;
            s = rest;
            max = m;
            if max < 0 {
                min = -1;
            }
        }
    }
    let rest = s.strip_prefix('}')?;
    Some((min, max, rest))
}

fn repeat_is_valid(re: &Node, n: i64) -> bool {
    let mut n = n;
    if let Node::Repeat { min, max, .. } = re {
        let mut m = *max;
        if m == 0 {
            return true;
        }
        if m < 0 {
            m = *min;
        }
        if m > n {
            return false;
        }
        if m > 0 {
            n /= m;
        }
    }
    match re {
        Node::Capture(_, s) => repeat_is_valid(s, n),
        Node::Repeat { sub, .. } => repeat_is_valid(sub, n),
        Node::Concat(v) | Node::Alt(v) => v.iter().all(|s| repeat_is_valid(s, n)),
        _ => true,
    }
}

fn height(n: &Node) -> usize {
    match n {
        Node::Capture(_, s) => 1 + height(s),
        Node::Repeat { sub, .. } => 1 + height(sub),
        Node::Concat(v) | Node::Alt(v) => 1 + v.iter().map(height).max().unwrap_or(0),
        _ => 1,
    }
}

/// Approximate compiled size (Go's `calcSize`), saturating.
fn size(n: &Node) -> i64 {
    let s = match n {
        Node::Lit(..) => 1,
        Node::Capture(_, s) => 2i64.saturating_add(size(s)),
        Node::Concat(v) => v.iter().fold(0i64, |a, s| a.saturating_add(size(s))),
        Node::Alt(v) => v.iter().fold(v.len() as i64 - 1, |a, s| a.saturating_add(size(s))),
        Node::Repeat { min, max, sub, .. } => {
            let sub = size(sub);
            if *max == -1 {
                if *min == 0 { 2i64.saturating_add(sub) } else { 1i64.saturating_add(min.saturating_mul(sub)) }
            } else {
                max.saturating_mul(sub).saturating_add(max - min)
            }
        }
        _ => 1,
    };
    s.max(1)
}

/// Parses a Go regexp (with `syntax.Perl` flags) into an AST.
fn parse(s: &str) -> Result<(Node, Vec<Option<String>>), SyntaxError> {
    let mut p = Parser {
        flags: Flags { fold: false, one_line: true, dot_nl: false, non_greedy: false },
        stack: Vec::new(),
        num_cap: 0,
        names: alloc::vec![None],
        whole: s,
    };
    let mut t = s;
    let mut last_repeat: &str = "";
    while !t.is_empty() {
        let mut repeat: &str = "";
        let b = t.as_bytes();
        match b[0] {
            b'(' => {
                if b.len() >= 2 && b[1] == b'?' {
                    t = p.parse_perl_flags(t)?;
                } else {
                    p.num_cap += 1;
                    p.names.push(None);
                    p.stack.push(Item::LeftParen { cap: p.num_cap, name: None, flags: p.flags });
                    t = &t[1..];
                }
            }
            b'|' => {
                p.parse_vertical_bar();
                t = &t[1..];
            }
            b')' => {
                p.parse_right_paren()?;
                t = &t[1..];
            }
            b'^' => {
                p.push(if p.flags.one_line { Node::BeginText } else { Node::BeginLine });
                t = &t[1..];
            }
            b'$' => {
                p.push(if p.flags.one_line { Node::EndText } else { Node::EndLine });
                t = &t[1..];
            }
            b'.' => {
                p.push(if p.flags.dot_nl { Node::Any } else { Node::AnyNotNl });
                t = &t[1..];
            }
            b'[' => {
                t = p.parse_class(t)?;
            }
            b'*' | b'+' | b'?' => {
                let before = t;
                let (min, max) = match b[0] {
                    b'*' => (0, -1),
                    b'+' => (1, -1),
                    _ => (0, 1),
                };
                let after = &t[1..];
                t = p.repeat(min, max, before, after, last_repeat)?;
                repeat = before;
            }
            b'{' => {
                let before = t;
                match parse_repeat(t) {
                    None => {
                        p.literal('{');
                        t = &t[1..];
                    }
                    Some((min, max, after)) => {
                        if min < 0 || min > 1000 || max > 1000 || (max >= 0 && min > max) {
                            return Err(SyntaxError::new(ERR_INVALID_REPEAT_SIZE, &before[..before.len() - after.len()]));
                        }
                        t = p.repeat(min, max, before, after, last_repeat)?;
                        repeat = before;
                    }
                }
            }
            b'\\' => {
                let mut handled = true;
                if b.len() >= 2 {
                    match b[1] {
                        b'A' => {
                            p.push(Node::BeginText);
                            t = &t[2..];
                        }
                        b'b' => {
                            p.push(Node::WordB);
                            t = &t[2..];
                        }
                        b'B' => {
                            p.push(Node::NoWordB);
                            t = &t[2..];
                        }
                        b'C' => return Err(SyntaxError::new(ERR_INVALID_ESCAPE, &t[..2])),
                        b'Q' => {
                            let rest = &t[2..];
                            let (lit, after) = match rest.find("\\E") {
                                Some(i) => (&rest[..i], &rest[i + 2..]),
                                None => (rest, ""),
                            };
                            for c in lit.chars() {
                                p.literal(c);
                            }
                            t = after;
                        }
                        b'z' => {
                            p.push(Node::EndText);
                            t = &t[2..];
                        }
                        _ => handled = false,
                    }
                } else {
                    handled = false;
                }
                if !handled {
                    let fold = p.flags.fold;
                    if let Some((item, rest)) = p.parse_unicode_class(t)? {
                        p.push(Node::Class { items: alloc::vec![item], negated: false, fold });
                        t = rest;
                    } else if let Some((neg, class)) = perl_group(t) {
                        let items = group_items(neg, class, fold);
                        p.push(Node::Class { items, negated: false, fold });
                        t = &t[2..];
                    } else {
                        let (c, rest) = parse_escape(t)?;
                        p.literal(c);
                        t = rest;
                    }
                }
            }
            _ => {
                let (c, rest) = next_char(t);
                p.literal(c);
                t = rest;
            }
        }
        last_repeat = repeat;
    }
    p.concat();
    if p.swap_vertical_bar() {
        p.stack.pop();
    }
    p.alternate();
    if p.stack.len() != 1 {
        return Err(SyntaxError::new(ERR_MISSING_PAREN, s));
    }
    let Some(Item::Node(node)) = p.stack.pop() else {
        return Err(SyntaxError::new(ERR_MISSING_PAREN, s));
    };
    if height(&node) > 1000 {
        return Err(SyntaxError::new(ERR_NESTING_DEPTH, s));
    }
    if size(&node) > (128 << 20) / 40 {
        return Err(SyntaxError::new(ERR_LARGE, s));
    }
    Ok((node, p.names))
}

fn emit(n: &Node, out: &mut String) {
    match n {
        Node::Empty => out.push_str("(?:)"),
        Node::NoMatch => out.push_str("[a&&b]"),
        Node::Lit(c, fold) => {
            if *fold {
                out.push_str("(?i:");
            }
            if c.is_ascii_alphanumeric() {
                out.push(*c);
            } else {
                push_char_escape(out, *c as u32);
            }
            if *fold {
                out.push(')');
            }
        }
        Node::AnyNotNl => out.push_str("[^\\n]"),
        Node::Any => out.push_str("(?s:.)"),
        Node::BeginLine => out.push_str("(?m:^)"),
        Node::EndLine => out.push_str("(?m:$)"),
        Node::BeginText => out.push_str("\\A"),
        Node::EndText => out.push_str("\\z"),
        Node::WordB => out.push_str("(?-u:\\b)"),
        Node::NoWordB => out.push_str("(?-u:\\B)"),
        Node::Class { items, negated, fold } => {
            out.push_str(if *fold { "(?i:[" } else { "(?:[" });
            if *negated {
                out.push('^');
            }
            if items.is_empty() {
                // Empty class: matches nothing (or anything if negated).
                out.push_str("\\x{0}&&\\x{1}");
            }
            for it in items {
                match it {
                    ClassItem::Range(a, b) => push_range(out, *a, *b),
                    ClassItem::Unicode(name, neg) => {
                        if name.starts_with('[') {
                            // Nested (pre-folded) negated class.
                            out.push_str("[^");
                            out.push_str(&name[1..]);
                        } else {
                            out.push_str(if *neg { "\\P{" } else { "\\p{" });
                            out.push_str(name);
                            out.push('}');
                        }
                    }
                }
            }
            out.push_str("])");
        }
        Node::Capture(_, s) => {
            out.push('(');
            emit(s, out);
            out.push(')');
        }
        Node::Repeat { min, max, greedy, sub } => {
            out.push_str("(?:");
            emit(sub, out);
            out.push(')');
            match (*min, *max) {
                (0, -1) => out.push('*'),
                (1, -1) => out.push('+'),
                (0, 1) => out.push('?'),
                (m, -1) => {
                    let _ = write!(out, "{{{m},}}");
                }
                (m, x) if m == x => {
                    let _ = write!(out, "{{{m}}}");
                }
                (m, x) => {
                    let _ = write!(out, "{{{m},{x}}}");
                }
            }
            if !greedy {
                out.push('?');
            }
        }
        Node::Concat(v) => {
            out.push_str("(?:");
            for s in v {
                emit(s, out);
            }
            out.push(')');
        }
        Node::Alt(v) => {
            out.push_str("(?:");
            for (i, s) in v.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                emit(s, out);
            }
            out.push(')');
        }
    }
}

/// A compiled Go regexp.
pub struct GoRegex {
    re: Regex,
    names: Vec<Option<String>>,
    expr_empty: bool,
}

/// `regexp.Compile` error text, or a size-limit error.
pub fn compile(expr: &str) -> Result<GoRegex, String> {
    let (node, names) = parse(expr).map_err(|e| e.message())?;
    let mut pat = String::new();
    emit(&node, &mut pat);
    let re = Regex::builder()
        .configure(Regex::config().nfa_size_limit(Some(256 << 20)).hybrid_cache_capacity(8 << 20))
        .build(&pat)
        .map_err(|_| SyntaxError::new(ERR_LARGE, expr).message())?;
    Ok(GoRegex { re, names, expr_empty: expr.is_empty() })
}

/// `regexp.MustCompile`'s panic message for a bad pattern.
pub fn must_compile_message(expr: &str, err: &str) -> String {
    let q = if strconv::can_backquote(expr.as_bytes()) { format!("`{expr}`") } else { strconv::quote(expr) };
    format!("regexp: Compile({q}): {err}")
}

/// `regexp.QuoteMeta`.
pub fn quote_meta(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl GoRegex {
    fn exec(&self, s: &str, pos: usize, caps: &mut Captures) -> bool {
        let input = Input::new(s).span(pos..s.len());
        self.re.search_captures(&input, caps);
        caps.is_match()
    }

    pub fn is_match(&self, s: &str) -> bool {
        self.re.is_match(s)
    }

    /// `FindString`.
    pub fn find_string<'h>(&self, s: &'h str) -> &'h str {
        match self.re.find(s) {
            Some(m) => &s[m.range()],
            None => "",
        }
    }

    /// `FindAllStringIndex(s, n)` (spans of whole matches).
    pub fn find_all_index(&self, s: &str, n: i64) -> Vec<(usize, usize)> {
        let n = if n < 0 { s.len() as i64 + 1 } else { n };
        let mut out = Vec::new();
        let mut pos = 0usize;
        let mut prev_match_end: i64 = -1;
        let end = s.len();
        let mut i = 0;
        while i < n && pos <= end {
            let Some(m) = self.re.search(&Input::new(s).span(pos..end)) else { break };
            let (a, b) = (m.start(), m.end());
            let mut accept = true;
            if b == pos {
                if a as i64 == prev_match_end {
                    accept = false;
                }
                pos += s[pos..].chars().next().map_or(1, char::len_utf8);
            } else {
                pos = b;
            }
            prev_match_end = b as i64;
            if accept {
                out.push((a, b));
                i += 1;
            }
        }
        out
    }

    /// `FindAllString(s, n)`.
    pub fn find_all_string<'h>(&self, s: &'h str, n: i64) -> Vec<&'h str> {
        self.find_all_index(s, n).into_iter().map(|(a, b)| &s[a..b]).collect()
    }

    /// `Split(s, n)`.
    pub fn split<'h>(&self, s: &'h str, n: i64) -> Vec<&'h str> {
        if n == 0 {
            return Vec::new();
        }
        if !self.expr_empty && s.is_empty() {
            return alloc::vec![""];
        }
        let matches = self.find_all_index(s, n);
        let mut out = Vec::new();
        let (mut beg, mut end) = (0usize, 0usize);
        for (a, b) in matches {
            if n > 0 && out.len() as i64 == n - 1 {
                break;
            }
            end = a;
            if b != 0 {
                out.push(&s[beg..end]);
            }
            beg = b;
        }
        if end != s.len() {
            out.push(&s[beg..]);
        }
        out
    }

    /// `ReplaceAllString` (with `$` expansion) or `ReplaceAllLiteralString`.
    pub fn replace_all(&self, src: &str, repl: &str, literal: bool) -> String {
        let mut buf = String::new();
        let mut last_match_end = 0usize;
        let mut search_pos = 0usize;
        let end = src.len();
        let mut caps = self.re.create_captures();
        while search_pos <= end {
            if !self.exec(src, search_pos, &mut caps) {
                break;
            }
            let Some(m) = caps.get_match() else { break };
            let (a0, a1) = (m.start(), m.end());
            buf.push_str(&src[last_match_end..a0]);
            if a1 > last_match_end || a0 == 0 {
                if literal {
                    buf.push_str(repl);
                } else {
                    self.expand(&mut buf, repl, src, &caps);
                }
            }
            last_match_end = a1;
            let width = src[search_pos..].chars().next().map_or(0, char::len_utf8);
            if search_pos + width > a1 {
                search_pos += width;
            } else if search_pos + 1 > a1 {
                search_pos += 1;
            } else {
                search_pos = a1;
            }
        }
        buf.push_str(&src[last_match_end..]);
        buf
    }

    fn group<'h>(&self, src: &'h str, caps: &Captures, i: usize) -> Option<&'h str> {
        caps.get_group(i).map(|sp| &src[sp.range()])
    }

    fn expand(&self, dst: &mut String, template: &str, src: &str, caps: &Captures) {
        let mut template = template;
        while !template.is_empty() {
            let Some(i) = template.find('$') else { break };
            dst.push_str(&template[..i]);
            template = &template[i + 1..];
            if template.as_bytes().first() == Some(&b'$') {
                dst.push('$');
                template = &template[1..];
                continue;
            }
            let Some((name, num, rest)) = extract(template) else {
                dst.push('$');
                continue;
            };
            template = rest;
            if num >= 0 {
                if let Some(g) = self.group(src, caps, num as usize) {
                    dst.push_str(g);
                }
            } else {
                for (i, n) in self.names.iter().enumerate() {
                    if n.as_deref() == Some(name) {
                        if let Some(g) = self.group(src, caps, i) {
                            dst.push_str(g);
                            break;
                        }
                    }
                }
            }
        }
        dst.push_str(template);
    }
}

fn extract(s: &str) -> Option<(&str, i64, &str)> {
    if s.is_empty() {
        return None;
    }
    let mut str_ = s;
    let brace = str_.starts_with('{');
    if brace {
        str_ = &str_[1..];
    }
    let mut i = 0;
    for c in str_.chars() {
        if !gounicode::is_letter(c) && !gounicode::is_digit(c) && c != '_' {
            break;
        }
        i += c.len_utf8();
    }
    if i == 0 {
        return None;
    }
    let name = &str_[..i];
    if brace {
        if i >= str_.len() || str_.as_bytes()[i] != b'}' {
            return None;
        }
        i += 1;
    }
    let mut num: i64 = 0;
    for &c in name.as_bytes() {
        if !c.is_ascii_digit() || num >= 100_000_000 {
            num = -1;
            break;
        }
        num = num * 10 + (c - b'0') as i64;
    }
    if name.as_bytes()[0] == b'0' && name.len() > 1 {
        num = -1;
    }
    Some((name, num, &str_[i..]))
}
