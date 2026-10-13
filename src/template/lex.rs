//! Lexer: a port of Go's `text/template/parse/lex.go`.
//!
//! Positions are byte offsets into the template text; lines are 1-based.

use alloc::format;
use alloc::string::String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemType {
    Error,
    Bool,
    Char,
    CharConstant,
    Comment,
    Complex,
    Assign,
    Declare,
    Eof,
    Field,
    Identifier,
    LeftDelim,
    LeftParen,
    Number,
    Pipe,
    RawString,
    RightDelim,
    RightParen,
    Space,
    String,
    Text,
    Variable,
    // Keywords.
    Block,
    Break,
    Continue,
    Dot,
    Define,
    Else,
    End,
    If,
    Nil,
    Range,
    Template,
    With,
}

impl ItemType {
    fn is_keyword(self) -> bool {
        matches!(
            self,
            ItemType::Block
                | ItemType::Break
                | ItemType::Continue
                | ItemType::Dot
                | ItemType::Define
                | ItemType::Else
                | ItemType::End
                | ItemType::If
                | ItemType::Nil
                | ItemType::Range
                | ItemType::Template
                | ItemType::With
        )
    }
}

fn keyword(word: &str) -> Option<ItemType> {
    Some(match word {
        "." => ItemType::Dot,
        "block" => ItemType::Block,
        "break" => ItemType::Break,
        "continue" => ItemType::Continue,
        "define" => ItemType::Define,
        "else" => ItemType::Else,
        "end" => ItemType::End,
        "if" => ItemType::If,
        "range" => ItemType::Range,
        "nil" => ItemType::Nil,
        "template" => ItemType::Template,
        "with" => ItemType::With,
        _ => return None,
    })
}

/// A token. For errors, `val` holds the message.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub typ: ItemType,
    pub pos: usize,
    pub val: String,
    pub line: usize,
    /// End byte offset (exclusive), for diagnostics.
    pub end: usize,
}

impl Item {
    /// Go's `item.String()`, used in "unexpected X in Y" messages.
    pub fn describe(&self) -> String {
        match self.typ {
            ItemType::Eof => "EOF".into(),
            ItemType::Error => self.val.clone(),
            t if t.is_keyword() => format!("<{}>", self.val),
            _ => {
                if self.val.chars().count() > 10 {
                    // %.10q: quote the first 10 runes.
                    let head: String = self.val.chars().take(10).collect();
                    format!("{}...", crate::template::quote::go_quote(&head))
                } else {
                    crate::template::quote::go_quote(&self.val)
                }
            }
        }
    }
}

const LEFT_DELIM: &str = "{{";
const RIGHT_DELIM: &str = "}}";
const LEFT_COMMENT: &str = "/*";
const RIGHT_COMMENT: &str = "*/";
const TRIM_MARKER_LEN: usize = 2;

#[derive(Clone, Copy)]
enum State {
    Text,
    LeftDelim,
    Comment,
    RightDelim,
    InsideAction,
    Space,
    Identifier,
    Field,
    Variable,
    Char,
    Number,
    Quote,
    RawQuote,
}

pub(crate) struct Lexer<'a> {
    input: &'a str,
    /// Current length of the input; set to 0 on error, like Go.
    limit: usize,
    pos: usize,
    start: usize,
    at_eof: bool,
    paren_depth: i32,
    line: usize,
    start_line: usize,
    item: Option<Item>,
    inside_action: bool,
    pub break_ok: bool,
    pub continue_ok: bool,
}

const EOF: i32 = -1;

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Lexer<'a> {
        Lexer {
            input,
            limit: input.len(),
            pos: 0,
            start: 0,
            at_eof: false,
            paren_depth: 0,
            line: 1,
            start_line: 1,
            item: None,
            inside_action: false,
            break_ok: true,
            continue_ok: true,
        }
    }

    fn rest(&self) -> &'a str {
        &self.input[self.pos.min(self.limit)..self.limit]
    }

    fn next(&mut self) -> i32 {
        if self.pos >= self.limit {
            self.at_eof = true;
            return EOF;
        }
        let c = self.input[self.pos..self.limit].chars().next().unwrap();
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
        }
        c as i32
    }

    fn peek(&mut self) -> i32 {
        let r = self.next();
        self.backup();
        r
    }

    fn backup(&mut self) {
        if !self.at_eof && self.pos > 0 {
            let c = self.input[..self.pos].chars().next_back().unwrap();
            self.pos -= c.len_utf8();
            if c == '\n' {
                self.line -= 1;
            }
        }
    }

    fn this_item(&mut self, typ: ItemType) -> Item {
        let i = Item {
            typ,
            pos: self.start,
            val: String::from(&self.input[self.start..self.pos]),
            line: self.start_line,
            end: self.pos,
        };
        self.start = self.pos;
        self.start_line = self.line;
        i
    }

    fn emit(&mut self, typ: ItemType) -> Option<State> {
        let i = self.this_item(typ);
        self.item = Some(i);
        None
    }

    fn emit_item(&mut self, i: Item) -> Option<State> {
        self.item = Some(i);
        None
    }

    fn ignore(&mut self) {
        self.line += count_newlines(&self.input[self.start..self.pos]);
        self.start = self.pos;
        self.start_line = self.line;
    }

    fn accept(&mut self, valid: &str) -> bool {
        let r = self.next();
        if r >= 0 && valid.contains(char::from_u32(r as u32).unwrap_or('\u{0}')) && r != 0 {
            return true;
        }
        self.backup();
        false
    }

    fn accept_run(&mut self, valid: &str) {
        loop {
            let r = self.next();
            if !(r > 0 && valid.contains(char::from_u32(r as u32).unwrap_or('\u{0}'))) {
                break;
            }
        }
        self.backup();
    }

    fn errorf(&mut self, msg: String) -> Option<State> {
        self.item = Some(Item {
            typ: ItemType::Error,
            pos: self.start,
            val: msg,
            line: self.start_line,
            end: self.pos,
        });
        self.start = 0;
        self.pos = 0;
        self.limit = 0;
        None
    }

    /// Returns the next item.
    pub fn next_item(&mut self) -> Item {
        self.item = Some(Item {
            typ: ItemType::Eof,
            pos: self.pos,
            val: "EOF".into(),
            line: self.start_line,
            end: self.pos,
        });
        let mut state = if self.inside_action {
            State::InsideAction
        } else {
            State::Text
        };
        loop {
            let next = match state {
                State::Text => self.lex_text(),
                State::LeftDelim => self.lex_left_delim(),
                State::Comment => self.lex_comment(),
                State::RightDelim => self.lex_right_delim(),
                State::InsideAction => self.lex_inside_action(),
                State::Space => self.lex_space(),
                State::Identifier => self.lex_identifier(),
                State::Field => self.lex_field_or_variable(ItemType::Field),
                State::Variable => self.lex_variable(),
                State::Char => self.lex_char(),
                State::Number => self.lex_number(),
                State::Quote => self.lex_quote(),
                State::RawQuote => self.lex_raw_quote(),
            };
            match next {
                Some(s) => state = s,
                None => return self.item.take().unwrap(),
            }
        }
    }

    fn lex_text(&mut self) -> Option<State> {
        if let Some(x) = self.rest().find(LEFT_DELIM) {
            if x > 0 {
                self.pos += x;
                let mut trim_length = 0;
                let delim_end = self.pos + LEFT_DELIM.len();
                if has_left_trim_marker(&self.input[delim_end..self.limit]) {
                    trim_length = right_trim_length(&self.input[self.start..self.pos]);
                }
                self.pos -= trim_length;
                self.line += count_newlines(&self.input[self.start..self.pos]);
                let i = self.this_item(ItemType::Text);
                self.pos += trim_length;
                self.ignore();
                if !i.val.is_empty() {
                    return self.emit_item(i);
                }
            }
            return Some(State::LeftDelim);
        }
        self.pos = self.limit;
        if self.pos > self.start {
            self.line += count_newlines(&self.input[self.start..self.pos]);
            return self.emit(ItemType::Text);
        }
        self.emit(ItemType::Eof)
    }

    fn at_right_delim(&self) -> (bool, bool) {
        let rest = self.rest();
        if has_right_trim_marker(rest) && rest[TRIM_MARKER_LEN..].starts_with(RIGHT_DELIM) {
            return (true, true);
        }
        if rest.starts_with(RIGHT_DELIM) {
            return (true, false);
        }
        (false, false)
    }

    fn lex_left_delim(&mut self) -> Option<State> {
        self.pos += LEFT_DELIM.len();
        let trim_space = has_left_trim_marker(self.rest());
        let after_marker = if trim_space { TRIM_MARKER_LEN } else { 0 };
        if self.input[self.pos + after_marker..self.limit].starts_with(LEFT_COMMENT) {
            self.pos += after_marker;
            self.ignore();
            return Some(State::Comment);
        }
        let i = self.this_item(ItemType::LeftDelim);
        self.inside_action = true;
        self.pos += after_marker;
        self.ignore();
        self.paren_depth = 0;
        self.emit_item(i)
    }

    fn lex_comment(&mut self) -> Option<State> {
        self.pos += LEFT_COMMENT.len();
        let Some(x) = self.rest().find(RIGHT_COMMENT) else {
            return self.errorf("unclosed comment".into());
        };
        self.pos += x + RIGHT_COMMENT.len();
        let (delim, trim_space) = self.at_right_delim();
        if !delim {
            return self.errorf("comment ends before closing delimiter".into());
        }
        self.line += count_newlines(&self.input[self.start..self.pos]);
        let _i = self.this_item(ItemType::Comment);
        if trim_space {
            self.pos += TRIM_MARKER_LEN;
        }
        self.pos += RIGHT_DELIM.len();
        if trim_space {
            self.pos += left_trim_length(self.rest());
        }
        self.ignore();
        Some(State::Text)
    }

    fn lex_right_delim(&mut self) -> Option<State> {
        let (_, trim_space) = self.at_right_delim();
        if trim_space {
            self.pos += TRIM_MARKER_LEN;
            self.ignore();
        }
        self.pos += RIGHT_DELIM.len();
        let i = self.this_item(ItemType::RightDelim);
        if trim_space {
            self.pos += left_trim_length(self.rest());
            self.ignore();
        }
        self.inside_action = false;
        self.emit_item(i)
    }

    fn lex_inside_action(&mut self) -> Option<State> {
        let (delim, _) = self.at_right_delim();
        if delim {
            if self.paren_depth == 0 {
                return Some(State::RightDelim);
            }
            return self.errorf("unclosed left paren".into());
        }
        let r = self.next();
        if r == EOF {
            return self.errorf("unclosed action".into());
        }
        let c = char::from_u32(r as u32).unwrap();
        match c {
            _ if is_space(c) => {
                self.backup();
                Some(State::Space)
            }
            '=' => self.emit(ItemType::Assign),
            ':' => {
                if self.next() != '=' as i32 {
                    return self.errorf("expected :=".into());
                }
                self.emit(ItemType::Declare)
            }
            '|' => self.emit(ItemType::Pipe),
            '"' => Some(State::Quote),
            '`' => Some(State::RawQuote),
            '$' => Some(State::Variable),
            '\'' => Some(State::Char),
            '.' => {
                if self.pos < self.limit {
                    let b = self.input.as_bytes()[self.pos];
                    if !b.is_ascii_digit() {
                        return Some(State::Field);
                    }
                }
                self.backup();
                Some(State::Number)
            }
            '+' | '-' | '0'..='9' => {
                self.backup();
                Some(State::Number)
            }
            _ if is_alpha_numeric(c) => {
                self.backup();
                Some(State::Identifier)
            }
            '(' => {
                self.paren_depth += 1;
                self.emit(ItemType::LeftParen)
            }
            ')' => {
                self.paren_depth -= 1;
                if self.paren_depth < 0 {
                    return self.errorf("unexpected right paren".into());
                }
                self.emit(ItemType::RightParen)
            }
            _ if (c as u32) <= 0x7f && is_ascii_print(c) => self.emit(ItemType::Char),
            _ => self.errorf(format!(
                "unrecognized character in action: {}",
                go_sharp_u(c)
            )),
        }
    }

    fn lex_space(&mut self) -> Option<State> {
        let mut num_spaces = 0;
        loop {
            let r = self.peek();
            if !(r >= 0 && is_space(char::from_u32(r as u32).unwrap())) {
                break;
            }
            self.next();
            num_spaces += 1;
        }
        let s = &self.input[self.pos - 1..self.limit];
        if has_right_trim_marker(s) && s[TRIM_MARKER_LEN..].starts_with(RIGHT_DELIM) {
            self.backup();
            if num_spaces == 1 {
                return Some(State::RightDelim);
            }
        }
        self.emit(ItemType::Space)
    }

    fn lex_identifier(&mut self) -> Option<State> {
        loop {
            let r = self.next();
            if r >= 0 && is_alpha_numeric(char::from_u32(r as u32).unwrap()) {
                continue;
            }
            self.backup();
            let word = &self.input[self.start..self.pos];
            if !self.at_terminator() {
                return self.errorf(format!("bad character {}", go_sharp_u_rune(r)));
            }
            if let Some(kw) = keyword(word) {
                if (kw == ItemType::Break && !self.break_ok)
                    || (kw == ItemType::Continue && !self.continue_ok)
                {
                    return self.emit(ItemType::Identifier);
                }
                return self.emit(kw);
            }
            if word.starts_with('.') {
                return self.emit(ItemType::Field);
            }
            if word == "true" || word == "false" {
                return self.emit(ItemType::Bool);
            }
            return self.emit(ItemType::Identifier);
        }
    }

    fn lex_variable(&mut self) -> Option<State> {
        if self.at_terminator() {
            return self.emit(ItemType::Variable);
        }
        self.lex_field_or_variable(ItemType::Variable)
    }

    fn lex_field_or_variable(&mut self, typ: ItemType) -> Option<State> {
        if self.at_terminator() {
            if typ == ItemType::Variable {
                return self.emit(ItemType::Variable);
            }
            return self.emit(ItemType::Dot);
        }
        let mut r;
        loop {
            r = self.next();
            if !(r >= 0 && is_alpha_numeric(char::from_u32(r as u32).unwrap())) {
                self.backup();
                break;
            }
        }
        if !self.at_terminator() {
            return self.errorf(format!("bad character {}", go_sharp_u_rune(r)));
        }
        self.emit(typ)
    }

    fn at_terminator(&mut self) -> bool {
        let r = self.peek();
        if r >= 0 && is_space(char::from_u32(r as u32).unwrap()) {
            return true;
        }
        if r == EOF {
            return true;
        }
        if let Some(c) = char::from_u32(r as u32)
            && matches!(c, '.' | ',' | '|' | ':' | ')' | '(')
        {
            return true;
        }
        self.rest().starts_with(RIGHT_DELIM)
    }

    fn lex_char(&mut self) -> Option<State> {
        loop {
            let r = self.next();
            if r == '\\' as i32 {
                let r2 = self.next();
                if r2 != EOF && r2 != '\n' as i32 {
                    continue;
                }
                return self.errorf("unterminated character constant".into());
            }
            if r == EOF || r == '\n' as i32 {
                return self.errorf("unterminated character constant".into());
            }
            if r == '\'' as i32 {
                break;
            }
        }
        self.emit(ItemType::CharConstant)
    }

    fn lex_number(&mut self) -> Option<State> {
        if !self.scan_number() {
            let s = String::from(&self.input[self.start..self.pos]);
            return self.errorf(format!(
                "bad number syntax: {}",
                crate::template::quote::go_quote(&s)
            ));
        }
        let sign = self.peek();
        if sign == '+' as i32 || sign == '-' as i32 {
            if !self.scan_number() || self.input.as_bytes()[self.pos - 1] != b'i' {
                let s = String::from(&self.input[self.start..self.pos]);
                return self.errorf(format!(
                    "bad number syntax: {}",
                    crate::template::quote::go_quote(&s)
                ));
            }
            return self.emit(ItemType::Complex);
        }
        self.emit(ItemType::Number)
    }

    fn scan_number(&mut self) -> bool {
        self.accept("+-");
        let mut digits = "0123456789_";
        if self.accept("0") {
            if self.accept("xX") {
                digits = "0123456789abcdefABCDEF_";
            } else if self.accept("oO") {
                digits = "01234567_";
            } else if self.accept("bB") {
                digits = "01_";
            }
        }
        self.accept_run(digits);
        if self.accept(".") {
            self.accept_run(digits);
        }
        if digits.len() == 10 + 1 && self.accept("eE") {
            self.accept("+-");
            self.accept_run("0123456789_");
        }
        if digits.len() == 16 + 6 + 1 && self.accept("pP") {
            self.accept("+-");
            self.accept_run("0123456789_");
        }
        self.accept("i");
        let r = self.peek();
        if r >= 0 && is_alpha_numeric(char::from_u32(r as u32).unwrap()) {
            self.next();
            return false;
        }
        true
    }

    fn lex_quote(&mut self) -> Option<State> {
        loop {
            let r = self.next();
            if r == '\\' as i32 {
                let r2 = self.next();
                if r2 != EOF && r2 != '\n' as i32 {
                    continue;
                }
                return self.errorf("unterminated quoted string".into());
            }
            if r == EOF || r == '\n' as i32 {
                return self.errorf("unterminated quoted string".into());
            }
            if r == '"' as i32 {
                break;
            }
        }
        self.emit(ItemType::String)
    }

    fn lex_raw_quote(&mut self) -> Option<State> {
        loop {
            let r = self.next();
            if r == EOF {
                return self.errorf("unterminated raw quoted string".into());
            }
            if r == '`' as i32 {
                break;
            }
        }
        self.emit(ItemType::RawString)
    }
}

fn count_newlines(s: &str) -> usize {
    s.bytes().filter(|&b| b == b'\n').count()
}

pub(crate) fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// Go `unicode.IsLetter(r) || unicode.IsDigit(r) || r == '_'`.
pub(crate) fn is_alpha_numeric(c: char) -> bool {
    c == '_' || crate::template::unicode::is_letter(c) || crate::template::unicode::is_digit(c)
}

fn is_ascii_print(c: char) -> bool {
    (' '..='~').contains(&c)
}

fn has_left_trim_marker(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0] == b'-' && is_space(b[1] as char)
}

fn has_right_trim_marker(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && is_space(b[0] as char) && b[1] == b'-'
}

fn right_trim_length(s: &str) -> usize {
    s.len() - s.trim_end_matches(is_space).len()
}

fn left_trim_length(s: &str) -> usize {
    s.len() - s.trim_start_matches(is_space).len()
}

/// Go's `%#U` formatting of a rune: `U+0041 'A'`.
pub(crate) fn go_sharp_u(c: char) -> String {
    if crate::template::unicode::is_print(c) {
        format!("U+{:04X} '{}'", c as u32, c)
    } else {
        format!("U+{:04X}", c as u32)
    }
}

fn go_sharp_u_rune(r: i32) -> String {
    if r < 0 {
        // Go formats eof (-1) with %#U as "U+FFFFFFFFFFFFFFFF".
        return "U+FFFFFFFFFFFFFFFF".into();
    }
    go_sharp_u(char::from_u32(r as u32).unwrap_or('\u{FFFD}'))
}
