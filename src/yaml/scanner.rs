//! Port of the libyaml reader and scanner as found in `go.yaml.in/yaml/v2`
//! (v2.4.4, used by `sigs.k8s.io/yaml`). The structure follows the Go code
//! closely so that accepted input, error messages and error positions match.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mark {
    /// Character index.
    pub index: usize,
    /// 0-based line.
    pub line: usize,
    /// 0-based column (characters).
    pub column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum ScalarStyle {
    #[default]
    Any,
    Plain,
    SingleQuoted,
    DoubleQuoted,
    Literal,
    Folded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum TokenType {
    #[default]
    NoToken,
    StreamStart,
    StreamEnd,
    VersionDirective,
    TagDirective,
    DocumentStart,
    DocumentEnd,
    BlockSequenceStart,
    BlockMappingStart,
    BlockEnd,
    FlowSequenceStart,
    FlowSequenceEnd,
    FlowMappingStart,
    FlowMappingEnd,
    BlockEntry,
    FlowEntry,
    Key,
    Value,
    Alias,
    Anchor,
    Tag,
    Scalar,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Token {
    pub typ: TokenType,
    pub start_mark: Mark,
    pub end_mark: Mark,
    /// Alias/anchor name, scalar value, tag handle or %TAG handle.
    pub value: Vec<u8>,
    /// Tag suffix.
    pub suffix: Vec<u8>,
    /// %TAG prefix.
    pub prefix: Vec<u8>,
    pub style: ScalarStyle,
    pub major: i8,
    pub minor: i8,
}

#[derive(Clone, Copy, Debug, Default)]
struct SimpleKey {
    possible: bool,
    required: bool,
    token_number: usize,
    mark: Mark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum ErrorKind {
    #[default]
    None,
    Reader,
    Scanner,
    Parser,
}

/// Error state shared by the scanner and the parser (Go's fields on
/// `yaml_parser_t`).
#[derive(Clone, Debug, Default)]
pub(crate) struct ErrorState {
    pub kind: ErrorKind,
    pub problem: String,
    pub problem_mark: Mark,
    pub problem_offset: usize,
    pub context: String,
    pub context_mark: Mark,
}

const RAW_BUFFER_SIZE: usize = 512;
const MAX_FLOW_LEVEL: usize = 10000;
const MAX_INDENTS: usize = 10000;

pub(crate) struct Scanner<'a> {
    input: &'a [u8],
    input_pos: usize,
    raw: Vec<u8>,
    raw_pos: usize,
    eof: bool,
    encoding_known: bool,
    offset: usize,

    buffer: Vec<u8>,
    buffer_pos: usize,
    unread: usize,

    pub mark: Mark,
    pub err: ErrorState,

    pub stream_start_produced: bool,
    pub stream_end_produced: bool,
    flow_level: usize,
    pub tokens: Vec<Token>,
    pub tokens_head: usize,
    pub tokens_parsed: usize,
    pub token_available: bool,
    indent: i64,
    indents: Vec<i64>,
    simple_key_allowed: bool,
    simple_keys: Vec<SimpleKey>,
    simple_keys_by_tok: BTreeMap<usize, usize>,
}

#[inline]
fn width(b: u8) -> usize {
    if b & 0x80 == 0 {
        1
    } else if b & 0xE0 == 0xC0 {
        2
    } else if b & 0xF0 == 0xE0 {
        3
    } else if b & 0xF8 == 0xF0 {
        4
    } else {
        0
    }
}

fn is_alpha_b(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

impl<'a> Scanner<'a> {
    pub fn new(input: &'a [u8]) -> Scanner<'a> {
        Scanner {
            input,
            input_pos: 0,
            raw: Vec::with_capacity(RAW_BUFFER_SIZE),
            raw_pos: 0,
            eof: false,
            encoding_known: false,
            offset: 0,
            buffer: Vec::new(),
            buffer_pos: 0,
            unread: 0,
            mark: Mark::default(),
            err: ErrorState::default(),
            stream_start_produced: false,
            stream_end_produced: false,
            flow_level: 0,
            tokens: Vec::new(),
            tokens_head: 0,
            tokens_parsed: 0,
            token_available: false,
            indent: 0,
            indents: Vec::new(),
            simple_key_allowed: false,
            simple_keys: Vec::new(),
            simple_keys_by_tok: BTreeMap::new(),
        }
    }

    // ---------------------------------------------------------------
    // Character access helpers (out of range reads yield NUL).

    #[inline]
    fn at(&self, k: usize) -> u8 {
        self.buffer.get(self.buffer_pos + k).copied().unwrap_or(0)
    }
    #[inline]
    fn ch(&self) -> u8 {
        self.at(0)
    }
    fn is_z(&self, k: usize) -> bool {
        self.at(k) == 0
    }
    fn is_blank(&self, k: usize) -> bool {
        let c = self.at(k);
        c == b' ' || c == b'\t'
    }
    fn is_space(&self, k: usize) -> bool {
        self.at(k) == b' '
    }
    fn is_tab(&self, k: usize) -> bool {
        self.at(k) == b'\t'
    }
    fn is_break(&self, k: usize) -> bool {
        let c = self.at(k);
        c == b'\r'
            || c == b'\n'
            || c == 0xC2 && self.at(k + 1) == 0x85
            || c == 0xE2
                && self.at(k + 1) == 0x80
                && (self.at(k + 2) == 0xA8 || self.at(k + 2) == 0xA9)
    }
    fn is_breakz(&self, k: usize) -> bool {
        self.is_break(k) || self.is_z(k)
    }
    fn is_blankz(&self, k: usize) -> bool {
        self.is_blank(k) || self.is_breakz(k)
    }
    fn is_crlf(&self, k: usize) -> bool {
        self.at(k) == b'\r' && self.at(k + 1) == b'\n'
    }
    fn is_alpha(&self, k: usize) -> bool {
        is_alpha_b(self.at(k))
    }
    fn is_digit(&self, k: usize) -> bool {
        self.at(k).is_ascii_digit()
    }
    fn is_hex(&self, k: usize) -> bool {
        self.at(k).is_ascii_hexdigit()
    }
    fn as_hex(&self, k: usize) -> u32 {
        let c = self.at(k);
        match c {
            b'A'..=b'F' => (c - b'A' + 10) as u32,
            b'a'..=b'f' => (c - b'a' + 10) as u32,
            _ => c.wrapping_sub(b'0') as u32,
        }
    }
    /// Go's `is_bom(buffer, pos)` which, quirkily, looks at the start of the
    /// buffer rather than at `pos`.
    fn is_bom_quirk(&self) -> bool {
        self.buffer.first() == Some(&0xEF)
            && self.buffer.get(1) == Some(&0xBB)
            && self.buffer.get(2) == Some(&0xBF)
    }

    #[inline]
    fn cache(&mut self, n: usize) -> bool {
        self.unread >= n || self.update_buffer(n)
    }

    fn skip(&mut self) {
        self.mark.index += 1;
        self.mark.column += 1;
        self.unread = self.unread.saturating_sub(1);
        let w = width(self.ch()).max(1);
        self.buffer_pos += w;
    }

    fn skip_line(&mut self) {
        if self.is_crlf(0) {
            self.mark.index += 2;
            self.mark.column = 0;
            self.mark.line += 1;
            self.unread = self.unread.saturating_sub(2);
            self.buffer_pos += 2;
        } else if self.is_break(0) {
            self.mark.index += 1;
            self.mark.column = 0;
            self.mark.line += 1;
            self.unread = self.unread.saturating_sub(1);
            self.buffer_pos += width(self.ch()).max(1);
        }
    }

    fn read(&mut self, s: &mut Vec<u8>) {
        let w = width(self.ch()).max(1);
        for k in 0..w {
            s.push(self.at(k));
        }
        self.buffer_pos += w;
        self.mark.index += 1;
        self.mark.column += 1;
        self.unread = self.unread.saturating_sub(1);
    }

    fn read_line(&mut self, s: &mut Vec<u8>) {
        let (c0, c1, c2) = (self.at(0), self.at(1), self.at(2));
        if c0 == b'\r' && c1 == b'\n' {
            s.push(b'\n');
            self.buffer_pos += 2;
            self.mark.index += 1;
            self.unread = self.unread.saturating_sub(1);
        } else if c0 == b'\r' || c0 == b'\n' {
            s.push(b'\n');
            self.buffer_pos += 1;
        } else if c0 == 0xC2 && c1 == 0x85 {
            s.push(b'\n');
            self.buffer_pos += 2;
        } else if c0 == 0xE2 && c1 == 0x80 && (c2 == 0xA8 || c2 == 0xA9) {
            s.extend_from_slice(&[c0, c1, c2]);
            self.buffer_pos += 3;
        } else {
            return;
        }
        self.mark.index += 1;
        self.mark.column = 0;
        self.mark.line += 1;
        self.unread = self.unread.saturating_sub(1);
    }

    // ---------------------------------------------------------------
    // Reader.

    fn set_reader_error(&mut self, problem: &str, offset: usize) -> bool {
        self.err.kind = ErrorKind::Reader;
        self.err.problem = problem.into();
        self.err.problem_offset = offset;
        false
    }

    fn update_raw_buffer(&mut self) {
        if self.raw_pos == 0 && self.raw.len() == RAW_BUFFER_SIZE {
            return;
        }
        if self.eof {
            return;
        }
        if self.raw_pos > 0 {
            self.raw.drain(..self.raw_pos.min(self.raw.len()));
            self.raw_pos = 0;
        }
        if self.input_pos == self.input.len() {
            self.eof = true;
            return;
        }
        let n = (RAW_BUFFER_SIZE - self.raw.len()).min(self.input.len() - self.input_pos);
        self.raw
            .extend_from_slice(&self.input[self.input_pos..self.input_pos + n]);
        self.input_pos += n;
    }

    fn determine_encoding(&mut self) {
        while !self.eof && self.raw.len() - self.raw_pos < 3 {
            self.update_raw_buffer();
        }
        let avail = self.raw.len() - self.raw_pos;
        if avail >= 3 && self.raw[self.raw_pos..self.raw_pos + 3] == [0xEF, 0xBB, 0xBF] {
            self.raw_pos += 3;
            self.offset += 3;
        }
        self.encoding_known = true;
    }

    fn update_buffer(&mut self, length: usize) -> bool {
        if self.unread >= length {
            return true;
        }
        if !self.encoding_known {
            self.determine_encoding();
        }
        // Move the unread characters to the beginning of the buffer.
        if self.buffer_pos > 0 {
            let p = self.buffer_pos.min(self.buffer.len());
            self.buffer.drain(..p);
            self.buffer_pos = 0;
        }
        let mut first = true;
        while self.unread < length {
            if !first || self.raw_pos == self.raw.len() {
                self.update_raw_buffer();
            }
            first = false;
            while self.raw_pos != self.raw.len() {
                let raw_unread = self.raw.len() - self.raw_pos;
                let octet = self.raw[self.raw_pos];
                let w = width(octet);
                if w == 0 {
                    return self.set_reader_error("invalid leading UTF-8 octet", self.offset);
                }
                if w > raw_unread {
                    if self.eof {
                        return self
                            .set_reader_error("incomplete UTF-8 octet sequence", self.offset);
                    }
                    break;
                }
                let mut value: u32 = match w {
                    1 => (octet & 0x7F) as u32,
                    2 => (octet & 0x1F) as u32,
                    3 => (octet & 0x0F) as u32,
                    _ => (octet & 0x07) as u32,
                };
                for k in 1..w {
                    let o = self.raw[self.raw_pos + k];
                    if o & 0xC0 != 0x80 {
                        return self
                            .set_reader_error("invalid trailing UTF-8 octet", self.offset + k);
                    }
                    value = (value << 6) + (o & 0x3F) as u32;
                }
                let ok_len = match w {
                    1 => true,
                    2 => value >= 0x80,
                    3 => value >= 0x800,
                    _ => value >= 0x10000,
                };
                if !ok_len {
                    return self
                        .set_reader_error("invalid length of a UTF-8 sequence", self.offset);
                }
                if (0xD800..=0xDFFF).contains(&value) || value > 0x10FFFF {
                    return self.set_reader_error("invalid Unicode character", self.offset);
                }
                let allowed = value == 0x09
                    || value == 0x0A
                    || value == 0x0D
                    || (0x20..=0x7E).contains(&value)
                    || value == 0x85
                    || (0xA0..=0xD7FF).contains(&value)
                    || (0xE000..=0xFFFD).contains(&value)
                    || (0x10000..=0x10FFFF).contains(&value);
                if !allowed {
                    return self
                        .set_reader_error("control characters are not allowed", self.offset);
                }
                self.buffer
                    .extend_from_slice(&self.raw[self.raw_pos..self.raw_pos + w]);
                self.raw_pos += w;
                self.offset += w;
                self.unread += 1;
            }
            if self.eof {
                self.buffer.push(0);
                self.unread += 1;
                break;
            }
        }
        true
    }

    // ---------------------------------------------------------------
    // Token queue.

    fn insert_token(&mut self, pos: Option<usize>, token: Token) {
        if self.tokens_head > 0 && self.tokens_head == self.tokens.len() {
            self.tokens.clear();
            self.tokens_head = 0;
        } else if self.tokens_head > 64 {
            self.tokens.drain(..self.tokens_head);
            self.tokens_head = 0;
        }
        match pos {
            None => self.tokens.push(token),
            Some(p) => {
                let at = (self.tokens_head + p).min(self.tokens.len());
                self.tokens.insert(at, token);
            }
        }
    }

    fn set_scanner_error(&mut self, context: &str, context_mark: Mark, problem: &str) -> bool {
        self.err.kind = ErrorKind::Scanner;
        self.err.context = context.into();
        self.err.context_mark = context_mark;
        self.err.problem = problem.into();
        self.err.problem_mark = self.mark;
        false
    }

    fn set_scanner_tag_error(
        &mut self,
        directive: bool,
        context_mark: Mark,
        problem: &str,
    ) -> bool {
        let context = if directive {
            "while parsing a %TAG directive"
        } else {
            "while parsing a tag"
        };
        self.set_scanner_error(context, context_mark, problem)
    }

    /// Makes sure at least one token is available at `tokens[tokens_head]`.
    pub fn fetch_more_tokens(&mut self) -> bool {
        loop {
            if self.tokens_head != self.tokens.len() {
                match self.simple_keys_by_tok.get(&self.tokens_parsed).copied() {
                    None => break,
                    Some(idx) => match self.simple_key_is_valid(idx) {
                        None => return false,
                        Some(false) => break,
                        Some(true) => {}
                    },
                }
            }
            if !self.fetch_next_token() {
                return false;
            }
        }
        self.token_available = true;
        true
    }

    fn fetch_next_token(&mut self) -> bool {
        if !self.cache(1) {
            return false;
        }
        if !self.stream_start_produced {
            self.fetch_stream_start();
            return true;
        }
        if !self.scan_to_next_token() {
            return false;
        }
        self.unroll_indent(self.mark.column as i64);
        if !self.cache(4) {
            return false;
        }
        if self.is_z(0) {
            return self.fetch_stream_end();
        }
        let c = self.ch();
        if self.mark.column == 0 && c == b'%' {
            return self.fetch_directive();
        }
        if self.mark.column == 0
            && c == b'-'
            && self.at(1) == b'-'
            && self.at(2) == b'-'
            && self.is_blankz(3)
        {
            return self.fetch_document_indicator(TokenType::DocumentStart);
        }
        if self.mark.column == 0
            && c == b'.'
            && self.at(1) == b'.'
            && self.at(2) == b'.'
            && self.is_blankz(3)
        {
            return self.fetch_document_indicator(TokenType::DocumentEnd);
        }
        match c {
            b'[' => return self.fetch_flow_collection_start(TokenType::FlowSequenceStart),
            b'{' => return self.fetch_flow_collection_start(TokenType::FlowMappingStart),
            b']' => return self.fetch_flow_collection_end(TokenType::FlowSequenceEnd),
            b'}' => return self.fetch_flow_collection_end(TokenType::FlowMappingEnd),
            b',' => return self.fetch_flow_entry(),
            _ => {}
        }
        if c == b'-' && self.is_blankz(1) {
            return self.fetch_block_entry();
        }
        if c == b'?' && (self.flow_level > 0 || self.is_blankz(1)) {
            return self.fetch_key();
        }
        if c == b':' && (self.flow_level > 0 || self.is_blankz(1)) {
            return self.fetch_value();
        }
        match c {
            b'*' => return self.fetch_anchor(TokenType::Alias),
            b'&' => return self.fetch_anchor(TokenType::Anchor),
            b'!' => return self.fetch_tag(),
            b'|' if self.flow_level == 0 => return self.fetch_block_scalar(true),
            b'>' if self.flow_level == 0 => return self.fetch_block_scalar(false),
            b'\'' => return self.fetch_flow_scalar(true),
            b'"' => return self.fetch_flow_scalar(false),
            _ => {}
        }
        if !(self.is_blankz(0)
            || matches!(
                c,
                b'-' | b'?'
                    | b':'
                    | b','
                    | b'['
                    | b']'
                    | b'{'
                    | b'}'
                    | b'#'
                    | b'&'
                    | b'*'
                    | b'!'
                    | b'|'
                    | b'>'
                    | b'\''
                    | b'"'
                    | b'%'
                    | b'@'
                    | b'`'
            ))
            || (c == b'-' && !self.is_blank(1))
            || (self.flow_level == 0 && (c == b'?' || c == b':') && !self.is_blankz(1))
        {
            return self.fetch_plain_scalar();
        }
        let m = self.mark;
        self.set_scanner_error(
            "while scanning for the next token",
            m,
            "found character that cannot start any token",
        )
    }

    /// `None` on error, otherwise whether the key is still valid.
    fn simple_key_is_valid(&mut self, idx: usize) -> Option<bool> {
        let key = match self.simple_keys.get(idx) {
            Some(k) => *k,
            None => return Some(false),
        };
        if !key.possible {
            return Some(false);
        }
        if key.mark.line < self.mark.line || key.mark.index + 1024 < self.mark.index {
            if key.required {
                self.set_scanner_error(
                    "while scanning a simple key",
                    key.mark,
                    "could not find expected ':'",
                );
                return None;
            }
            self.simple_keys[idx].possible = false;
            return Some(false);
        }
        Some(true)
    }

    fn save_simple_key(&mut self) -> bool {
        let required = self.flow_level == 0 && self.indent == self.mark.column as i64;
        if self.simple_key_allowed {
            let key = SimpleKey {
                possible: true,
                required,
                token_number: self.tokens_parsed + (self.tokens.len() - self.tokens_head),
                mark: self.mark,
            };
            if !self.remove_simple_key() {
                return false;
            }
            let last = self.simple_keys.len() - 1;
            self.simple_keys[last] = key;
            self.simple_keys_by_tok.insert(key.token_number, last);
        }
        true
    }

    fn remove_simple_key(&mut self) -> bool {
        let i = self.simple_keys.len() - 1;
        if self.simple_keys[i].possible {
            if self.simple_keys[i].required {
                let m = self.simple_keys[i].mark;
                return self.set_scanner_error(
                    "while scanning a simple key",
                    m,
                    "could not find expected ':'",
                );
            }
            self.simple_keys[i].possible = false;
            let tn = self.simple_keys[i].token_number;
            self.simple_keys_by_tok.remove(&tn);
        }
        true
    }

    fn increase_flow_level(&mut self) -> bool {
        self.simple_keys.push(SimpleKey {
            possible: false,
            required: false,
            token_number: self.tokens_parsed + (self.tokens.len() - self.tokens_head),
            mark: self.mark,
        });
        self.flow_level += 1;
        if self.flow_level > MAX_FLOW_LEVEL {
            let m = self.simple_keys[self.simple_keys.len() - 1].mark;
            return self.set_scanner_error(
                "while increasing flow level",
                m,
                &format!("exceeded max depth of {MAX_FLOW_LEVEL}"),
            );
        }
        true
    }

    fn decrease_flow_level(&mut self) {
        if self.flow_level > 0 {
            self.flow_level -= 1;
            if let Some(last) = self.simple_keys.pop() {
                self.simple_keys_by_tok.remove(&last.token_number);
            }
        }
    }

    fn roll_indent(
        &mut self,
        column: i64,
        number: Option<usize>,
        typ: TokenType,
        mark: Mark,
    ) -> bool {
        if self.flow_level > 0 {
            return true;
        }
        if self.indent < column {
            self.indents.push(self.indent);
            self.indent = column;
            if self.indents.len() > MAX_INDENTS {
                let m = self.simple_keys[self.simple_keys.len() - 1].mark;
                return self.set_scanner_error(
                    "while increasing indent level",
                    m,
                    &format!("exceeded max depth of {MAX_INDENTS}"),
                );
            }
            let token = Token {
                typ,
                start_mark: mark,
                end_mark: mark,
                ..Default::default()
            };
            let pos = number.map(|n| n.saturating_sub(self.tokens_parsed));
            self.insert_token(pos, token);
        }
        true
    }

    fn unroll_indent(&mut self, column: i64) {
        if self.flow_level > 0 {
            return;
        }
        while self.indent > column {
            let token = Token {
                typ: TokenType::BlockEnd,
                start_mark: self.mark,
                end_mark: self.mark,
                ..Default::default()
            };
            self.insert_token(None, token);
            self.indent = self.indents.pop().unwrap_or(-1);
        }
    }

    fn fetch_stream_start(&mut self) {
        self.indent = -1;
        self.simple_keys.push(SimpleKey::default());
        self.simple_keys_by_tok.clear();
        self.simple_key_allowed = true;
        self.stream_start_produced = true;
        let token = Token {
            typ: TokenType::StreamStart,
            start_mark: self.mark,
            end_mark: self.mark,
            ..Default::default()
        };
        self.insert_token(None, token);
    }

    fn fetch_stream_end(&mut self) -> bool {
        if self.mark.column != 0 {
            self.mark.column = 0;
            self.mark.line += 1;
        }
        self.unroll_indent(-1);
        if !self.remove_simple_key() {
            return false;
        }
        self.simple_key_allowed = false;
        let token = Token {
            typ: TokenType::StreamEnd,
            start_mark: self.mark,
            end_mark: self.mark,
            ..Default::default()
        };
        self.insert_token(None, token);
        true
    }

    fn fetch_directive(&mut self) -> bool {
        self.unroll_indent(-1);
        if !self.remove_simple_key() {
            return false;
        }
        self.simple_key_allowed = false;
        let mut token = Token::default();
        if !self.scan_directive(&mut token) {
            return false;
        }
        self.insert_token(None, token);
        true
    }

    fn fetch_document_indicator(&mut self, typ: TokenType) -> bool {
        self.unroll_indent(-1);
        if !self.remove_simple_key() {
            return false;
        }
        self.simple_key_allowed = false;
        let start_mark = self.mark;
        self.skip();
        self.skip();
        self.skip();
        let end_mark = self.mark;
        self.insert_token(
            None,
            Token {
                typ,
                start_mark,
                end_mark,
                ..Default::default()
            },
        );
        true
    }

    fn fetch_flow_collection_start(&mut self, typ: TokenType) -> bool {
        if !self.save_simple_key() {
            return false;
        }
        if !self.increase_flow_level() {
            return false;
        }
        self.simple_key_allowed = true;
        let start_mark = self.mark;
        self.skip();
        let end_mark = self.mark;
        self.insert_token(
            None,
            Token {
                typ,
                start_mark,
                end_mark,
                ..Default::default()
            },
        );
        true
    }

    fn fetch_flow_collection_end(&mut self, typ: TokenType) -> bool {
        if !self.remove_simple_key() {
            return false;
        }
        self.decrease_flow_level();
        self.simple_key_allowed = false;
        let start_mark = self.mark;
        self.skip();
        let end_mark = self.mark;
        self.insert_token(
            None,
            Token {
                typ,
                start_mark,
                end_mark,
                ..Default::default()
            },
        );
        true
    }

    fn fetch_flow_entry(&mut self) -> bool {
        if !self.remove_simple_key() {
            return false;
        }
        self.simple_key_allowed = true;
        let start_mark = self.mark;
        self.skip();
        let end_mark = self.mark;
        self.insert_token(
            None,
            Token {
                typ: TokenType::FlowEntry,
                start_mark,
                end_mark,
                ..Default::default()
            },
        );
        true
    }

    fn fetch_block_entry(&mut self) -> bool {
        if self.flow_level == 0 {
            if !self.simple_key_allowed {
                let m = self.mark;
                return self.set_scanner_error(
                    "",
                    m,
                    "block sequence entries are not allowed in this context",
                );
            }
            let (col, m) = (self.mark.column as i64, self.mark);
            if !self.roll_indent(col, None, TokenType::BlockSequenceStart, m) {
                return false;
            }
        }
        if !self.remove_simple_key() {
            return false;
        }
        self.simple_key_allowed = true;
        let start_mark = self.mark;
        self.skip();
        let end_mark = self.mark;
        self.insert_token(
            None,
            Token {
                typ: TokenType::BlockEntry,
                start_mark,
                end_mark,
                ..Default::default()
            },
        );
        true
    }

    fn fetch_key(&mut self) -> bool {
        if self.flow_level == 0 {
            if !self.simple_key_allowed {
                let m = self.mark;
                return self.set_scanner_error(
                    "",
                    m,
                    "mapping keys are not allowed in this context",
                );
            }
            let (col, m) = (self.mark.column as i64, self.mark);
            if !self.roll_indent(col, None, TokenType::BlockMappingStart, m) {
                return false;
            }
        }
        if !self.remove_simple_key() {
            return false;
        }
        self.simple_key_allowed = self.flow_level == 0;
        let start_mark = self.mark;
        self.skip();
        let end_mark = self.mark;
        self.insert_token(
            None,
            Token {
                typ: TokenType::Key,
                start_mark,
                end_mark,
                ..Default::default()
            },
        );
        true
    }

    fn fetch_value(&mut self) -> bool {
        let idx = self.simple_keys.len() - 1;
        match self.simple_key_is_valid(idx) {
            None => return false,
            Some(true) => {
                let sk = self.simple_keys[idx];
                let token = Token {
                    typ: TokenType::Key,
                    start_mark: sk.mark,
                    end_mark: sk.mark,
                    ..Default::default()
                };
                self.insert_token(
                    Some(sk.token_number.saturating_sub(self.tokens_parsed)),
                    token,
                );
                if !self.roll_indent(
                    sk.mark.column as i64,
                    Some(sk.token_number),
                    TokenType::BlockMappingStart,
                    sk.mark,
                ) {
                    return false;
                }
                self.simple_keys[idx].possible = false;
                self.simple_keys_by_tok.remove(&sk.token_number);
                self.simple_key_allowed = false;
            }
            Some(false) => {
                if self.flow_level == 0 {
                    if !self.simple_key_allowed {
                        let m = self.mark;
                        return self.set_scanner_error(
                            "",
                            m,
                            "mapping values are not allowed in this context",
                        );
                    }
                    let (col, m) = (self.mark.column as i64, self.mark);
                    if !self.roll_indent(col, None, TokenType::BlockMappingStart, m) {
                        return false;
                    }
                }
                self.simple_key_allowed = self.flow_level == 0;
            }
        }
        let start_mark = self.mark;
        self.skip();
        let end_mark = self.mark;
        self.insert_token(
            None,
            Token {
                typ: TokenType::Value,
                start_mark,
                end_mark,
                ..Default::default()
            },
        );
        true
    }

    fn fetch_anchor(&mut self, typ: TokenType) -> bool {
        if !self.save_simple_key() {
            return false;
        }
        self.simple_key_allowed = false;
        let mut token = Token::default();
        if !self.scan_anchor(&mut token, typ) {
            return false;
        }
        self.insert_token(None, token);
        true
    }

    fn fetch_tag(&mut self) -> bool {
        if !self.save_simple_key() {
            return false;
        }
        self.simple_key_allowed = false;
        let mut token = Token::default();
        if !self.scan_tag(&mut token) {
            return false;
        }
        self.insert_token(None, token);
        true
    }

    fn fetch_block_scalar(&mut self, literal: bool) -> bool {
        if !self.remove_simple_key() {
            return false;
        }
        self.simple_key_allowed = true;
        let mut token = Token::default();
        if !self.scan_block_scalar(&mut token, literal) {
            return false;
        }
        self.insert_token(None, token);
        true
    }

    fn fetch_flow_scalar(&mut self, single: bool) -> bool {
        if !self.save_simple_key() {
            return false;
        }
        self.simple_key_allowed = false;
        let mut token = Token::default();
        if !self.scan_flow_scalar(&mut token, single) {
            return false;
        }
        self.insert_token(None, token);
        true
    }

    fn fetch_plain_scalar(&mut self) -> bool {
        if !self.save_simple_key() {
            return false;
        }
        self.simple_key_allowed = false;
        let mut token = Token::default();
        if !self.scan_plain_scalar(&mut token) {
            return false;
        }
        self.insert_token(None, token);
        true
    }

    fn scan_to_next_token(&mut self) -> bool {
        loop {
            if !self.cache(1) {
                return false;
            }
            if self.mark.column == 0 && self.is_bom_quirk() {
                self.skip();
            }
            if !self.cache(1) {
                return false;
            }
            while self.ch() == b' '
                || ((self.flow_level > 0 || !self.simple_key_allowed) && self.ch() == b'\t')
            {
                self.skip();
                if !self.cache(1) {
                    return false;
                }
            }
            if self.ch() == b'#' {
                while !self.is_breakz(0) {
                    self.skip();
                    if !self.cache(1) {
                        return false;
                    }
                }
            }
            if self.is_break(0) {
                if !self.cache(2) {
                    return false;
                }
                self.skip_line();
                if self.flow_level == 0 {
                    self.simple_key_allowed = true;
                }
            } else {
                break;
            }
        }
        true
    }

    fn scan_directive(&mut self, token: &mut Token) -> bool {
        let start_mark = self.mark;
        self.skip();
        let mut name = Vec::new();
        if !self.scan_directive_name(start_mark, &mut name) {
            return false;
        }
        if name == b"YAML" {
            let (mut major, mut minor) = (0i8, 0i8);
            if !self.scan_version_directive_value(start_mark, &mut major, &mut minor) {
                return false;
            }
            *token = Token {
                typ: TokenType::VersionDirective,
                start_mark,
                end_mark: self.mark,
                major,
                minor,
                ..Default::default()
            };
        } else if name == b"TAG" {
            let (mut handle, mut prefix) = (Vec::new(), Vec::new());
            if !self.scan_tag_directive_value(start_mark, &mut handle, &mut prefix) {
                return false;
            }
            *token = Token {
                typ: TokenType::TagDirective,
                start_mark,
                end_mark: self.mark,
                value: handle,
                prefix,
                ..Default::default()
            };
        } else {
            return self.set_scanner_error(
                "while scanning a directive",
                start_mark,
                "found unknown directive name",
            );
        }
        if !self.cache(1) {
            return false;
        }
        while self.is_blank(0) {
            self.skip();
            if !self.cache(1) {
                return false;
            }
        }
        if self.ch() == b'#' {
            while !self.is_breakz(0) {
                self.skip();
                if !self.cache(1) {
                    return false;
                }
            }
        }
        if !self.is_breakz(0) {
            return self.set_scanner_error(
                "while scanning a directive",
                start_mark,
                "did not find expected comment or line break",
            );
        }
        if self.is_break(0) {
            if !self.cache(2) {
                return false;
            }
            self.skip_line();
        }
        true
    }

    fn scan_directive_name(&mut self, start_mark: Mark, name: &mut Vec<u8>) -> bool {
        if !self.cache(1) {
            return false;
        }
        let mut s = Vec::new();
        while self.is_alpha(0) {
            self.read(&mut s);
            if !self.cache(1) {
                return false;
            }
        }
        if s.is_empty() {
            return self.set_scanner_error(
                "while scanning a directive",
                start_mark,
                "could not find expected directive name",
            );
        }
        if !self.is_blankz(0) {
            return self.set_scanner_error(
                "while scanning a directive",
                start_mark,
                "found unexpected non-alphabetical character",
            );
        }
        *name = s;
        true
    }

    fn scan_version_directive_value(
        &mut self,
        start_mark: Mark,
        major: &mut i8,
        minor: &mut i8,
    ) -> bool {
        if !self.cache(1) {
            return false;
        }
        while self.is_blank(0) {
            self.skip();
            if !self.cache(1) {
                return false;
            }
        }
        if !self.scan_version_directive_number(start_mark, major) {
            return false;
        }
        if self.ch() != b'.' {
            return self.set_scanner_error(
                "while scanning a %YAML directive",
                start_mark,
                "did not find expected digit or '.' character",
            );
        }
        self.skip();
        self.scan_version_directive_number(start_mark, minor)
    }

    fn scan_version_directive_number(&mut self, start_mark: Mark, number: &mut i8) -> bool {
        if !self.cache(1) {
            return false;
        }
        let (mut value, mut length) = (0i8, 0);
        while self.is_digit(0) {
            length += 1;
            if length > 2 {
                return self.set_scanner_error(
                    "while scanning a %YAML directive",
                    start_mark,
                    "found extremely long version number",
                );
            }
            value = value
                .wrapping_mul(10)
                .wrapping_add((self.ch() - b'0') as i8);
            self.skip();
            if !self.cache(1) {
                return false;
            }
        }
        if length == 0 {
            return self.set_scanner_error(
                "while scanning a %YAML directive",
                start_mark,
                "did not find expected version number",
            );
        }
        *number = value;
        true
    }

    fn scan_tag_directive_value(
        &mut self,
        start_mark: Mark,
        handle: &mut Vec<u8>,
        prefix: &mut Vec<u8>,
    ) -> bool {
        if !self.cache(1) {
            return false;
        }
        while self.is_blank(0) {
            self.skip();
            if !self.cache(1) {
                return false;
            }
        }
        let mut handle_value = Vec::new();
        if !self.scan_tag_handle(true, start_mark, &mut handle_value) {
            return false;
        }
        if !self.cache(1) {
            return false;
        }
        if !self.is_blank(0) {
            return self.set_scanner_error(
                "while scanning a %TAG directive",
                start_mark,
                "did not find expected whitespace",
            );
        }
        while self.is_blank(0) {
            self.skip();
            if !self.cache(1) {
                return false;
            }
        }
        let mut prefix_value = Vec::new();
        if !self.scan_tag_uri(true, None, start_mark, &mut prefix_value) {
            return false;
        }
        if !self.cache(1) {
            return false;
        }
        if !self.is_blankz(0) {
            return self.set_scanner_error(
                "while scanning a %TAG directive",
                start_mark,
                "did not find expected whitespace or line break",
            );
        }
        *handle = handle_value;
        *prefix = prefix_value;
        true
    }

    fn scan_anchor(&mut self, token: &mut Token, typ: TokenType) -> bool {
        let mut s = Vec::new();
        let start_mark = self.mark;
        self.skip();
        if !self.cache(1) {
            return false;
        }
        while self.is_alpha(0) {
            self.read(&mut s);
            if !self.cache(1) {
                return false;
            }
        }
        let end_mark = self.mark;
        let c = self.ch();
        if s.is_empty()
            || !(self.is_blankz(0)
                || matches!(c, b'?' | b':' | b',' | b']' | b'}' | b'%' | b'@' | b'`'))
        {
            let context = if typ == TokenType::Anchor {
                "while scanning an anchor"
            } else {
                "while scanning an alias"
            };
            return self.set_scanner_error(
                context,
                start_mark,
                "did not find expected alphabetic or numeric character",
            );
        }
        *token = Token {
            typ,
            start_mark,
            end_mark,
            value: s,
            ..Default::default()
        };
        true
    }

    fn scan_tag(&mut self, token: &mut Token) -> bool {
        let start_mark = self.mark;
        let mut handle = Vec::new();
        let mut suffix = Vec::new();
        if !self.cache(2) {
            return false;
        }
        if self.at(1) == b'<' {
            self.skip();
            self.skip();
            if !self.scan_tag_uri(false, None, start_mark, &mut suffix) {
                return false;
            }
            if self.ch() != b'>' {
                return self.set_scanner_error(
                    "while scanning a tag",
                    start_mark,
                    "did not find the expected '>'",
                );
            }
            self.skip();
        } else {
            if !self.scan_tag_handle(false, start_mark, &mut handle) {
                return false;
            }
            if handle.first() == Some(&b'!') && handle.len() > 1 && handle.last() == Some(&b'!') {
                if !self.scan_tag_uri(false, None, start_mark, &mut suffix) {
                    return false;
                }
            } else {
                let head = handle.clone();
                if !self.scan_tag_uri(false, Some(&head), start_mark, &mut suffix) {
                    return false;
                }
                handle = alloc::vec![b'!'];
                if suffix.is_empty() {
                    core::mem::swap(&mut handle, &mut suffix);
                }
            }
        }
        if !self.cache(1) {
            return false;
        }
        if !self.is_blankz(0) {
            return self.set_scanner_error(
                "while scanning a tag",
                start_mark,
                "did not find expected whitespace or line break",
            );
        }
        *token = Token {
            typ: TokenType::Tag,
            start_mark,
            end_mark: self.mark,
            value: handle,
            suffix,
            ..Default::default()
        };
        true
    }

    fn scan_tag_handle(&mut self, directive: bool, start_mark: Mark, handle: &mut Vec<u8>) -> bool {
        if !self.cache(1) {
            return false;
        }
        if self.ch() != b'!' {
            return self.set_scanner_tag_error(directive, start_mark, "did not find expected '!'");
        }
        let mut s = Vec::new();
        self.read(&mut s);
        if !self.cache(1) {
            return false;
        }
        while self.is_alpha(0) {
            self.read(&mut s);
            if !self.cache(1) {
                return false;
            }
        }
        if self.ch() == b'!' {
            self.read(&mut s);
        } else if directive && s != b"!" {
            return self.set_scanner_tag_error(directive, start_mark, "did not find expected '!'");
        }
        *handle = s;
        true
    }

    fn scan_tag_uri(
        &mut self,
        directive: bool,
        head: Option<&[u8]>,
        start_mark: Mark,
        uri: &mut Vec<u8>,
    ) -> bool {
        let mut s = Vec::new();
        let head = head.unwrap_or(&[]);
        let mut has_tag = !head.is_empty();
        if head.len() > 1 {
            s.extend_from_slice(&head[1..]);
        }
        if !self.cache(1) {
            return false;
        }
        loop {
            let c = self.ch();
            let ok = self.is_alpha(0)
                || matches!(
                    c,
                    b';' | b'/'
                        | b'?'
                        | b':'
                        | b'@'
                        | b'&'
                        | b'='
                        | b'+'
                        | b'$'
                        | b','
                        | b'.'
                        | b'!'
                        | b'~'
                        | b'*'
                        | b'\''
                        | b'('
                        | b')'
                        | b'['
                        | b']'
                        | b'%'
                );
            if !ok {
                break;
            }
            if c == b'%' {
                if !self.scan_uri_escapes(directive, start_mark, &mut s) {
                    return false;
                }
            } else {
                self.read(&mut s);
            }
            if !self.cache(1) {
                return false;
            }
            has_tag = true;
        }
        if !has_tag {
            return self.set_scanner_tag_error(
                directive,
                start_mark,
                "did not find expected tag URI",
            );
        }
        *uri = s;
        true
    }

    fn scan_uri_escapes(&mut self, directive: bool, start_mark: Mark, s: &mut Vec<u8>) -> bool {
        let mut w: usize = 1024;
        while w > 0 {
            if !self.cache(3) {
                return false;
            }
            if !(self.ch() == b'%' && self.is_hex(1) && self.is_hex(2)) {
                return self.set_scanner_tag_error(
                    directive,
                    start_mark,
                    "did not find URI escaped octet",
                );
            }
            let octet = ((self.as_hex(1) << 4) + self.as_hex(2)) as u8;
            if w == 1024 {
                w = width(octet);
                if w == 0 {
                    return self.set_scanner_tag_error(
                        directive,
                        start_mark,
                        "found an incorrect leading UTF-8 octet",
                    );
                }
            } else if octet & 0xC0 != 0x80 {
                return self.set_scanner_tag_error(
                    directive,
                    start_mark,
                    "found an incorrect trailing UTF-8 octet",
                );
            }
            s.push(octet);
            self.skip();
            self.skip();
            self.skip();
            w -= 1;
        }
        true
    }

    fn scan_block_scalar(&mut self, token: &mut Token, literal: bool) -> bool {
        let start_mark = self.mark;
        self.skip();
        if !self.cache(1) {
            return false;
        }
        let mut chomping = 0i32;
        let mut increment = 0i64;
        if self.ch() == b'+' || self.ch() == b'-' {
            chomping = if self.ch() == b'+' { 1 } else { -1 };
            self.skip();
            if !self.cache(1) {
                return false;
            }
            if self.is_digit(0) {
                if self.ch() == b'0' {
                    return self.set_scanner_error(
                        "while scanning a block scalar",
                        start_mark,
                        "found an indentation indicator equal to 0",
                    );
                }
                increment = (self.ch() - b'0') as i64;
                self.skip();
            }
        } else if self.is_digit(0) {
            if self.ch() == b'0' {
                return self.set_scanner_error(
                    "while scanning a block scalar",
                    start_mark,
                    "found an indentation indicator equal to 0",
                );
            }
            increment = (self.ch() - b'0') as i64;
            self.skip();
            if !self.cache(1) {
                return false;
            }
            if self.ch() == b'+' || self.ch() == b'-' {
                chomping = if self.ch() == b'+' { 1 } else { -1 };
                self.skip();
            }
        }
        if !self.cache(1) {
            return false;
        }
        while self.is_blank(0) {
            self.skip();
            if !self.cache(1) {
                return false;
            }
        }
        if self.ch() == b'#' {
            while !self.is_breakz(0) {
                self.skip();
                if !self.cache(1) {
                    return false;
                }
            }
        }
        if !self.is_breakz(0) {
            return self.set_scanner_error(
                "while scanning a block scalar",
                start_mark,
                "did not find expected comment or line break",
            );
        }
        if self.is_break(0) {
            if !self.cache(2) {
                return false;
            }
            self.skip_line();
        }
        let mut end_mark = self.mark;
        let mut indent: i64 = 0;
        if increment > 0 {
            indent = if self.indent >= 0 {
                self.indent + increment
            } else {
                increment
            };
        }
        let mut s = Vec::new();
        let mut leading_break = Vec::new();
        let mut trailing_breaks = Vec::new();
        if !self.scan_block_scalar_breaks(
            &mut indent,
            &mut trailing_breaks,
            start_mark,
            &mut end_mark,
        ) {
            return false;
        }
        if !self.cache(1) {
            return false;
        }
        let mut leading_blank = false;
        while self.mark.column as i64 == indent && !self.is_z(0) {
            let trailing_blank = self.is_blank(0);
            if !literal
                && !leading_blank
                && !trailing_blank
                && leading_break.first() == Some(&b'\n')
            {
                if trailing_breaks.is_empty() {
                    s.push(b' ');
                }
            } else {
                s.extend_from_slice(&leading_break);
            }
            leading_break.clear();
            s.extend_from_slice(&trailing_breaks);
            trailing_breaks.clear();
            leading_blank = self.is_blank(0);
            while !self.is_breakz(0) {
                self.read(&mut s);
                if !self.cache(1) {
                    return false;
                }
            }
            if !self.cache(2) {
                return false;
            }
            self.read_line(&mut leading_break);
            if !self.scan_block_scalar_breaks(
                &mut indent,
                &mut trailing_breaks,
                start_mark,
                &mut end_mark,
            ) {
                return false;
            }
        }
        if chomping != -1 {
            s.extend_from_slice(&leading_break);
        }
        if chomping == 1 {
            s.extend_from_slice(&trailing_breaks);
        }
        *token = Token {
            typ: TokenType::Scalar,
            start_mark,
            end_mark,
            value: s,
            style: if literal {
                ScalarStyle::Literal
            } else {
                ScalarStyle::Folded
            },
            ..Default::default()
        };
        true
    }

    fn scan_block_scalar_breaks(
        &mut self,
        indent: &mut i64,
        breaks: &mut Vec<u8>,
        start_mark: Mark,
        end_mark: &mut Mark,
    ) -> bool {
        *end_mark = self.mark;
        let mut max_indent: i64 = 0;
        loop {
            if !self.cache(1) {
                return false;
            }
            while (*indent == 0 || (self.mark.column as i64) < *indent) && self.is_space(0) {
                self.skip();
                if !self.cache(1) {
                    return false;
                }
            }
            if self.mark.column as i64 > max_indent {
                max_indent = self.mark.column as i64;
            }
            if (*indent == 0 || (self.mark.column as i64) < *indent) && self.is_tab(0) {
                return self.set_scanner_error(
                    "while scanning a block scalar",
                    start_mark,
                    "found a tab character where an indentation space is expected",
                );
            }
            if !self.is_break(0) {
                break;
            }
            if !self.cache(2) {
                return false;
            }
            self.read_line(breaks);
            *end_mark = self.mark;
        }
        if *indent == 0 {
            *indent = max_indent;
            if *indent < self.indent + 1 {
                *indent = self.indent + 1;
            }
            if *indent < 1 {
                *indent = 1;
            }
        }
        true
    }

    fn scan_flow_scalar(&mut self, token: &mut Token, single: bool) -> bool {
        let start_mark = self.mark;
        self.skip();
        let mut s = Vec::new();
        let mut leading_break = Vec::new();
        let mut trailing_breaks = Vec::new();
        let mut whitespaces = Vec::new();
        loop {
            if !self.cache(4) {
                return false;
            }
            if self.mark.column == 0
                && ((self.at(0) == b'-' && self.at(1) == b'-' && self.at(2) == b'-')
                    || (self.at(0) == b'.' && self.at(1) == b'.' && self.at(2) == b'.'))
                && self.is_blankz(3)
            {
                return self.set_scanner_error(
                    "while scanning a quoted scalar",
                    start_mark,
                    "found unexpected document indicator",
                );
            }
            if self.is_z(0) {
                return self.set_scanner_error(
                    "while scanning a quoted scalar",
                    start_mark,
                    "found unexpected end of stream",
                );
            }
            let mut leading_blanks = false;
            while !self.is_blankz(0) {
                let c = self.ch();
                if single && c == b'\'' && self.at(1) == b'\'' {
                    s.push(b'\'');
                    self.skip();
                    self.skip();
                } else if (single && c == b'\'') || (!single && c == b'"') {
                    break;
                } else if !single && c == b'\\' && self.is_break(1) {
                    if !self.cache(3) {
                        return false;
                    }
                    self.skip();
                    self.skip_line();
                    leading_blanks = true;
                    break;
                } else if !single && c == b'\\' {
                    let mut code_length = 0;
                    match self.at(1) {
                        b'0' => s.push(0),
                        b'a' => s.push(0x07),
                        b'b' => s.push(0x08),
                        b't' | b'\t' => s.push(0x09),
                        b'n' => s.push(0x0A),
                        b'v' => s.push(0x0B),
                        b'f' => s.push(0x0C),
                        b'r' => s.push(0x0D),
                        b'e' => s.push(0x1B),
                        b' ' => s.push(0x20),
                        b'"' => s.push(b'"'),
                        b'\'' => s.push(b'\''),
                        b'\\' => s.push(b'\\'),
                        b'N' => s.extend_from_slice(&[0xC2, 0x85]),
                        b'_' => s.extend_from_slice(&[0xC2, 0xA0]),
                        b'L' => s.extend_from_slice(&[0xE2, 0x80, 0xA8]),
                        b'P' => s.extend_from_slice(&[0xE2, 0x80, 0xA9]),
                        b'x' => code_length = 2,
                        b'u' => code_length = 4,
                        b'U' => code_length = 8,
                        _ => {
                            return self.set_scanner_error(
                                "while parsing a quoted scalar",
                                start_mark,
                                "found unknown escape character",
                            );
                        }
                    }
                    self.skip();
                    self.skip();
                    if code_length > 0 {
                        if !self.cache(code_length) {
                            return false;
                        }
                        let mut value: u32 = 0;
                        for k in 0..code_length {
                            if !self.is_hex(k) {
                                return self.set_scanner_error(
                                    "while parsing a quoted scalar",
                                    start_mark,
                                    "did not find expected hexdecimal number",
                                );
                            }
                            value = (value << 4) + self.as_hex(k);
                        }
                        if (0xD800..=0xDFFF).contains(&value) || value > 0x10FFFF {
                            return self.set_scanner_error(
                                "while parsing a quoted scalar",
                                start_mark,
                                "found invalid Unicode character escape code",
                            );
                        }
                        push_utf8(&mut s, value);
                        for _ in 0..code_length {
                            self.skip();
                        }
                    }
                } else {
                    self.read(&mut s);
                }
                if !self.cache(2) {
                    return false;
                }
            }
            if !self.cache(1) {
                return false;
            }
            if single {
                if self.ch() == b'\'' {
                    break;
                }
            } else if self.ch() == b'"' {
                break;
            }
            while self.is_blank(0) || self.is_break(0) {
                if self.is_blank(0) {
                    if !leading_blanks {
                        self.read(&mut whitespaces);
                    } else {
                        self.skip();
                    }
                } else {
                    if !self.cache(2) {
                        return false;
                    }
                    if !leading_blanks {
                        whitespaces.clear();
                        self.read_line(&mut leading_break);
                        leading_blanks = true;
                    } else {
                        self.read_line(&mut trailing_breaks);
                    }
                }
                if !self.cache(1) {
                    return false;
                }
            }
            if leading_blanks {
                if leading_break.first() == Some(&b'\n') {
                    if trailing_breaks.is_empty() {
                        s.push(b' ');
                    } else {
                        s.extend_from_slice(&trailing_breaks);
                    }
                } else {
                    s.extend_from_slice(&leading_break);
                    s.extend_from_slice(&trailing_breaks);
                }
                trailing_breaks.clear();
                leading_break.clear();
            } else {
                s.extend_from_slice(&whitespaces);
                whitespaces.clear();
            }
        }
        self.skip();
        *token = Token {
            typ: TokenType::Scalar,
            start_mark,
            end_mark: self.mark,
            value: s,
            style: if single {
                ScalarStyle::SingleQuoted
            } else {
                ScalarStyle::DoubleQuoted
            },
            ..Default::default()
        };
        true
    }

    fn scan_plain_scalar(&mut self, token: &mut Token) -> bool {
        let mut s = Vec::new();
        let mut leading_break = Vec::new();
        let mut trailing_breaks = Vec::new();
        let mut whitespaces = Vec::new();
        let mut leading_blanks = false;
        let indent = self.indent + 1;
        let start_mark = self.mark;
        let mut end_mark = self.mark;
        loop {
            if !self.cache(4) {
                return false;
            }
            if self.mark.column == 0
                && ((self.at(0) == b'-' && self.at(1) == b'-' && self.at(2) == b'-')
                    || (self.at(0) == b'.' && self.at(1) == b'.' && self.at(2) == b'.'))
                && self.is_blankz(3)
            {
                break;
            }
            if self.ch() == b'#' {
                break;
            }
            while !self.is_blankz(0) {
                let c = self.ch();
                if (c == b':' && self.is_blankz(1))
                    || (self.flow_level > 0 && matches!(c, b',' | b'?' | b'[' | b']' | b'{' | b'}'))
                {
                    break;
                }
                if leading_blanks || !whitespaces.is_empty() {
                    if leading_blanks {
                        if leading_break.first() == Some(&b'\n') {
                            if trailing_breaks.is_empty() {
                                s.push(b' ');
                            } else {
                                s.extend_from_slice(&trailing_breaks);
                            }
                        } else {
                            s.extend_from_slice(&leading_break);
                            s.extend_from_slice(&trailing_breaks);
                        }
                        trailing_breaks.clear();
                        leading_break.clear();
                        leading_blanks = false;
                    } else {
                        s.extend_from_slice(&whitespaces);
                        whitespaces.clear();
                    }
                }
                self.read(&mut s);
                end_mark = self.mark;
                if !self.cache(2) {
                    return false;
                }
            }
            if !(self.is_blank(0) || self.is_break(0)) {
                break;
            }
            if !self.cache(1) {
                return false;
            }
            while self.is_blank(0) || self.is_break(0) {
                if self.is_blank(0) {
                    if leading_blanks && (self.mark.column as i64) < indent && self.is_tab(0) {
                        return self.set_scanner_error(
                            "while scanning a plain scalar",
                            start_mark,
                            "found a tab character that violates indentation",
                        );
                    }
                    if !leading_blanks {
                        self.read(&mut whitespaces);
                    } else {
                        self.skip();
                    }
                } else {
                    if !self.cache(2) {
                        return false;
                    }
                    if !leading_blanks {
                        whitespaces.clear();
                        self.read_line(&mut leading_break);
                        leading_blanks = true;
                    } else {
                        self.read_line(&mut trailing_breaks);
                    }
                }
                if !self.cache(1) {
                    return false;
                }
            }
            if self.flow_level == 0 && (self.mark.column as i64) < indent {
                break;
            }
        }
        *token = Token {
            typ: TokenType::Scalar,
            start_mark,
            end_mark,
            value: s,
            style: ScalarStyle::Plain,
            ..Default::default()
        };
        if leading_blanks {
            self.simple_key_allowed = true;
        }
        true
    }
}

pub(crate) fn push_utf8(s: &mut Vec<u8>, value: u32) {
    if value <= 0x7F {
        s.push(value as u8);
    } else if value <= 0x7FF {
        s.push((0xC0 + (value >> 6)) as u8);
        s.push((0x80 + (value & 0x3F)) as u8);
    } else if value <= 0xFFFF {
        s.push((0xE0 + (value >> 12)) as u8);
        s.push((0x80 + ((value >> 6) & 0x3F)) as u8);
        s.push((0x80 + (value & 0x3F)) as u8);
    } else {
        s.push((0xF0 + (value >> 18)) as u8);
        s.push((0x80 + ((value >> 12) & 0x3F)) as u8);
        s.push((0x80 + ((value >> 6) & 0x3F)) as u8);
        s.push((0x80 + (value & 0x3F)) as u8);
    }
}
