//! Port of the libyaml event parser from `go.yaml.in/yaml/v2`.

use alloc::vec::Vec;

use super::scanner::{ErrorKind, Mark, ScalarStyle, Scanner, TokenType};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum EventType {
    #[default]
    NoEvent,
    StreamStart,
    StreamEnd,
    DocumentStart,
    DocumentEnd,
    Alias,
    Scalar,
    SequenceStart,
    SequenceEnd,
    MappingStart,
    MappingEnd,
}

impl EventType {
    /// Go's `yaml_event_type_t.String()`.
    pub fn go_name(self) -> &'static str {
        match self {
            EventType::NoEvent => "none",
            EventType::StreamStart => "stream start",
            EventType::StreamEnd => "stream end",
            EventType::DocumentStart => "document start",
            EventType::DocumentEnd => "document end",
            EventType::Alias => "alias",
            EventType::Scalar => "scalar",
            EventType::SequenceStart => "sequence start",
            EventType::SequenceEnd => "sequence end",
            EventType::MappingStart => "mapping start",
            EventType::MappingEnd => "mapping end",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Event {
    pub typ: EventType,
    pub start_mark: Mark,
    #[allow(dead_code)]
    pub end_mark: Mark,
    pub anchor: Vec<u8>,
    pub tag: Vec<u8>,
    pub value: Vec<u8>,
    pub implicit: bool,
    pub quoted_implicit: bool,
    pub style: ScalarStyle,
    /// Flow style for collections.
    pub flow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    StreamStart,
    ImplicitDocumentStart,
    DocumentStart,
    DocumentContent,
    DocumentEnd,
    BlockNode,
    BlockSequenceFirstEntry,
    BlockSequenceEntry,
    IndentlessSequenceEntry,
    BlockMappingFirstKey,
    BlockMappingKey,
    BlockMappingValue,
    FlowSequenceFirstEntry,
    FlowSequenceEntry,
    FlowSequenceEntryMappingKey,
    FlowSequenceEntryMappingValue,
    FlowSequenceEntryMappingEnd,
    FlowMappingFirstKey,
    FlowMappingKey,
    FlowMappingValue,
    FlowMappingEmptyValue,
    End,
}

#[derive(Clone, Debug)]
struct TagDirective {
    handle: Vec<u8>,
    prefix: Vec<u8>,
}

pub(crate) struct Parser<'a> {
    pub sc: Scanner<'a>,
    state: State,
    states: Vec<State>,
    marks: Vec<Mark>,
    tag_directives: Vec<TagDirective>,
}

fn default_tag_directives() -> [TagDirective; 2] {
    [
        TagDirective {
            handle: b"!".to_vec(),
            prefix: b"!".to_vec(),
        },
        TagDirective {
            handle: b"!!".to_vec(),
            prefix: b"tag:yaml.org,2002:".to_vec(),
        },
    ]
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a [u8]) -> Parser<'a> {
        Parser {
            sc: Scanner::new(input),
            state: State::StreamStart,
            states: Vec::new(),
            marks: Vec::new(),
            tag_directives: Vec::new(),
        }
    }

    fn peek(&mut self) -> Option<TokenType> {
        if self.sc.token_available || self.sc.fetch_more_tokens() {
            self.sc.tokens.get(self.sc.tokens_head).map(|t| t.typ)
        } else {
            None
        }
    }

    fn tok(&self) -> &super::scanner::Token {
        &self.sc.tokens[self.sc.tokens_head]
    }

    fn skip_token(&mut self) {
        self.sc.token_available = false;
        self.sc.tokens_parsed += 1;
        self.sc.stream_end_produced =
            self.sc.tokens.get(self.sc.tokens_head).map(|t| t.typ) == Some(TokenType::StreamEnd);
        self.sc.tokens_head += 1;
    }

    fn pop_state(&mut self) {
        self.state = self.states.pop().unwrap_or(State::End);
    }

    fn set_parser_error(&mut self, problem: &str, problem_mark: Mark) -> bool {
        self.sc.err.kind = ErrorKind::Parser;
        self.sc.err.problem = problem.into();
        self.sc.err.problem_mark = problem_mark;
        false
    }

    fn set_parser_error_context(
        &mut self,
        context: &str,
        context_mark: Mark,
        problem: &str,
        problem_mark: Mark,
    ) -> bool {
        self.sc.err.kind = ErrorKind::Parser;
        self.sc.err.context = context.into();
        self.sc.err.context_mark = context_mark;
        self.sc.err.problem = problem.into();
        self.sc.err.problem_mark = problem_mark;
        false
    }

    /// `yaml_parser_parse`. Returns false on error (see `sc.err`).
    pub fn parse(&mut self, event: &mut Event) -> bool {
        *event = Event::default();
        if self.sc.stream_end_produced
            || self.sc.err.kind != ErrorKind::None
            || self.state == State::End
        {
            return true;
        }
        match self.state {
            State::StreamStart => self.parse_stream_start(event),
            State::ImplicitDocumentStart => self.parse_document_start(event, true),
            State::DocumentStart => self.parse_document_start(event, false),
            State::DocumentContent => self.parse_document_content(event),
            State::DocumentEnd => self.parse_document_end(event),
            State::BlockNode => self.parse_node(event, true, false),
            State::BlockSequenceFirstEntry => self.parse_block_sequence_entry(event, true),
            State::BlockSequenceEntry => self.parse_block_sequence_entry(event, false),
            State::IndentlessSequenceEntry => self.parse_indentless_sequence_entry(event),
            State::BlockMappingFirstKey => self.parse_block_mapping_key(event, true),
            State::BlockMappingKey => self.parse_block_mapping_key(event, false),
            State::BlockMappingValue => self.parse_block_mapping_value(event),
            State::FlowSequenceFirstEntry => self.parse_flow_sequence_entry(event, true),
            State::FlowSequenceEntry => self.parse_flow_sequence_entry(event, false),
            State::FlowSequenceEntryMappingKey => self.parse_flow_sequence_entry_mapping_key(event),
            State::FlowSequenceEntryMappingValue => {
                self.parse_flow_sequence_entry_mapping_value(event)
            }
            State::FlowSequenceEntryMappingEnd => self.parse_flow_sequence_entry_mapping_end(event),
            State::FlowMappingFirstKey => self.parse_flow_mapping_key(event, true),
            State::FlowMappingKey => self.parse_flow_mapping_key(event, false),
            State::FlowMappingValue => self.parse_flow_mapping_value(event, false),
            State::FlowMappingEmptyValue => self.parse_flow_mapping_value(event, true),
            State::End => true,
        }
    }

    fn parse_stream_start(&mut self, event: &mut Event) -> bool {
        let Some(typ) = self.peek() else { return false };
        let t = self.tok();
        if typ != TokenType::StreamStart {
            let m = t.start_mark;
            return self.set_parser_error("did not find expected <stream-start>", m);
        }
        *event = Event {
            typ: EventType::StreamStart,
            start_mark: t.start_mark,
            end_mark: t.end_mark,
            ..Default::default()
        };
        self.state = State::ImplicitDocumentStart;
        self.skip_token();
        true
    }

    fn parse_document_start(&mut self, event: &mut Event, implicit: bool) -> bool {
        let Some(mut typ) = self.peek() else {
            return false;
        };
        if !implicit {
            while typ == TokenType::DocumentEnd {
                self.skip_token();
                match self.peek() {
                    Some(t) => typ = t,
                    None => return false,
                }
            }
        }
        if implicit
            && typ != TokenType::VersionDirective
            && typ != TokenType::TagDirective
            && typ != TokenType::DocumentStart
            && typ != TokenType::StreamEnd
        {
            if !self.process_directives() {
                return false;
            }
            self.states.push(State::DocumentEnd);
            self.state = State::BlockNode;
            let t = self.tok();
            *event = Event {
                typ: EventType::DocumentStart,
                start_mark: t.start_mark,
                end_mark: t.end_mark,
                ..Default::default()
            };
        } else if typ != TokenType::StreamEnd {
            let start_mark = self.tok().start_mark;
            if !self.process_directives() {
                return false;
            }
            let Some(typ) = self.peek() else { return false };
            if typ != TokenType::DocumentStart {
                let m = self.tok().start_mark;
                return self.set_parser_error("did not find expected <document start>", m);
            }
            self.states.push(State::DocumentEnd);
            self.state = State::DocumentContent;
            let end_mark = self.tok().end_mark;
            *event = Event {
                typ: EventType::DocumentStart,
                start_mark,
                end_mark,
                implicit: false,
                ..Default::default()
            };
            self.skip_token();
        } else {
            self.state = State::End;
            let t = self.tok();
            *event = Event {
                typ: EventType::StreamEnd,
                start_mark: t.start_mark,
                end_mark: t.end_mark,
                ..Default::default()
            };
            self.skip_token();
        }
        true
    }

    fn parse_document_content(&mut self, event: &mut Event) -> bool {
        let Some(typ) = self.peek() else { return false };
        if matches!(
            typ,
            TokenType::VersionDirective
                | TokenType::TagDirective
                | TokenType::DocumentStart
                | TokenType::DocumentEnd
                | TokenType::StreamEnd
        ) {
            self.pop_state();
            let m = self.tok().start_mark;
            return self.process_empty_scalar(event, m);
        }
        self.parse_node(event, true, false)
    }

    fn parse_document_end(&mut self, event: &mut Event) -> bool {
        let Some(typ) = self.peek() else { return false };
        let start_mark = self.tok().start_mark;
        let mut end_mark = start_mark;
        let mut implicit = true;
        if typ == TokenType::DocumentEnd {
            end_mark = self.tok().end_mark;
            self.skip_token();
            implicit = false;
        }
        self.tag_directives.clear();
        self.state = State::DocumentStart;
        *event = Event {
            typ: EventType::DocumentEnd,
            start_mark,
            end_mark,
            implicit,
            ..Default::default()
        };
        true
    }

    fn parse_node(&mut self, event: &mut Event, block: bool, indentless_sequence: bool) -> bool {
        let Some(mut typ) = self.peek() else {
            return false;
        };
        if typ == TokenType::Alias {
            self.pop_state();
            let t = self.tok();
            *event = Event {
                typ: EventType::Alias,
                start_mark: t.start_mark,
                end_mark: t.end_mark,
                anchor: t.value.clone(),
                ..Default::default()
            };
            self.skip_token();
            return true;
        }
        let mut start_mark = self.tok().start_mark;
        let mut end_mark = start_mark;
        let mut tag_token = false;
        let mut tag_handle = Vec::new();
        let mut tag_suffix = Vec::new();
        let mut anchor = Vec::new();
        let mut tag_mark = Mark::default();
        if typ == TokenType::Anchor {
            let t = self.tok();
            anchor = t.value.clone();
            start_mark = t.start_mark;
            end_mark = t.end_mark;
            self.skip_token();
            match self.peek() {
                Some(t) => typ = t,
                None => return false,
            }
            if typ == TokenType::Tag {
                let t = self.tok();
                tag_token = true;
                tag_handle = t.value.clone();
                tag_suffix = t.suffix.clone();
                tag_mark = t.start_mark;
                end_mark = t.end_mark;
                self.skip_token();
                match self.peek() {
                    Some(t) => typ = t,
                    None => return false,
                }
            }
        } else if typ == TokenType::Tag {
            let t = self.tok();
            tag_token = true;
            tag_handle = t.value.clone();
            tag_suffix = t.suffix.clone();
            start_mark = t.start_mark;
            tag_mark = t.start_mark;
            end_mark = t.end_mark;
            self.skip_token();
            match self.peek() {
                Some(t) => typ = t,
                None => return false,
            }
            if typ == TokenType::Anchor {
                let t = self.tok();
                anchor = t.value.clone();
                end_mark = t.end_mark;
                self.skip_token();
                match self.peek() {
                    Some(t) => typ = t,
                    None => return false,
                }
            }
        }
        let mut tag = Vec::new();
        if tag_token {
            if tag_handle.is_empty() {
                tag = core::mem::take(&mut tag_suffix);
            } else {
                for td in &self.tag_directives {
                    if td.handle == tag_handle {
                        tag = td.prefix.clone();
                        tag.extend_from_slice(&tag_suffix);
                        break;
                    }
                }
                if tag.is_empty() {
                    return self.set_parser_error_context(
                        "while parsing a node",
                        start_mark,
                        "found undefined tag handle",
                        tag_mark,
                    );
                }
            }
        }
        let implicit = tag.is_empty();
        if indentless_sequence && typ == TokenType::BlockEntry {
            end_mark = self.tok().end_mark;
            self.state = State::IndentlessSequenceEntry;
            *event = Event {
                typ: EventType::SequenceStart,
                start_mark,
                end_mark,
                anchor,
                tag,
                implicit,
                flow: false,
                ..Default::default()
            };
            return true;
        }
        if typ == TokenType::Scalar {
            let (t_end, t_style) = {
                let t = self.tok();
                (t.end_mark, t.style)
            };
            end_mark = t_end;
            let mut plain_implicit = false;
            let mut quoted_implicit = false;
            if (tag.is_empty() && t_style == ScalarStyle::Plain) || tag == b"!" {
                plain_implicit = true;
            } else if tag.is_empty() {
                quoted_implicit = true;
            }
            *event = Event {
                typ: EventType::Scalar,
                start_mark,
                end_mark,
                anchor,
                tag,
                value: core::mem::take(&mut self.sc.tokens[self.sc.tokens_head].value),
                implicit: plain_implicit,
                quoted_implicit,
                style: t_style,
                flow: false,
            };
            self.pop_state();
            self.skip_token();
            return true;
        }
        if typ == TokenType::FlowSequenceStart {
            end_mark = self.tok().end_mark;
            self.state = State::FlowSequenceFirstEntry;
            *event = Event {
                typ: EventType::SequenceStart,
                start_mark,
                end_mark,
                anchor,
                tag,
                implicit,
                flow: true,
                ..Default::default()
            };
            return true;
        }
        if typ == TokenType::FlowMappingStart {
            end_mark = self.tok().end_mark;
            self.state = State::FlowMappingFirstKey;
            *event = Event {
                typ: EventType::MappingStart,
                start_mark,
                end_mark,
                anchor,
                tag,
                implicit,
                flow: true,
                ..Default::default()
            };
            return true;
        }
        if block && typ == TokenType::BlockSequenceStart {
            end_mark = self.tok().end_mark;
            self.state = State::BlockSequenceFirstEntry;
            *event = Event {
                typ: EventType::SequenceStart,
                start_mark,
                end_mark,
                anchor,
                tag,
                implicit,
                flow: false,
                ..Default::default()
            };
            return true;
        }
        if block && typ == TokenType::BlockMappingStart {
            end_mark = self.tok().end_mark;
            self.state = State::BlockMappingFirstKey;
            *event = Event {
                typ: EventType::MappingStart,
                start_mark,
                end_mark,
                anchor,
                tag,
                implicit,
                flow: false,
                ..Default::default()
            };
            return true;
        }
        if !anchor.is_empty() || !tag.is_empty() {
            self.pop_state();
            *event = Event {
                typ: EventType::Scalar,
                start_mark,
                end_mark,
                anchor,
                tag,
                implicit,
                quoted_implicit: false,
                style: ScalarStyle::Plain,
                ..Default::default()
            };
            return true;
        }
        let context = if block {
            "while parsing a block node"
        } else {
            "while parsing a flow node"
        };
        let m = self.tok().start_mark;
        self.set_parser_error_context(context, start_mark, "did not find expected node content", m)
    }

    fn parse_block_sequence_entry(&mut self, event: &mut Event, first: bool) -> bool {
        if first {
            if self.peek().is_none() {
                return false;
            }
            self.marks.push(self.tok().start_mark);
            self.skip_token();
        }
        let Some(typ) = self.peek() else { return false };
        if typ == TokenType::BlockEntry {
            let mark = self.tok().end_mark;
            self.skip_token();
            let Some(typ) = self.peek() else { return false };
            if typ != TokenType::BlockEntry && typ != TokenType::BlockEnd {
                self.states.push(State::BlockSequenceEntry);
                return self.parse_node(event, true, false);
            }
            self.state = State::BlockSequenceEntry;
            return self.process_empty_scalar(event, mark);
        }
        if typ == TokenType::BlockEnd {
            self.pop_state();
            self.marks.pop();
            let t = self.tok();
            *event = Event {
                typ: EventType::SequenceEnd,
                start_mark: t.start_mark,
                end_mark: t.end_mark,
                ..Default::default()
            };
            self.skip_token();
            return true;
        }
        let context_mark = self.marks.pop().unwrap_or_default();
        let m = self.tok().start_mark;
        self.set_parser_error_context(
            "while parsing a block collection",
            context_mark,
            "did not find expected '-' indicator",
            m,
        )
    }

    fn parse_indentless_sequence_entry(&mut self, event: &mut Event) -> bool {
        let Some(typ) = self.peek() else { return false };
        if typ == TokenType::BlockEntry {
            let mark = self.tok().end_mark;
            self.skip_token();
            let Some(typ) = self.peek() else { return false };
            if typ != TokenType::BlockEntry
                && typ != TokenType::Key
                && typ != TokenType::Value
                && typ != TokenType::BlockEnd
            {
                self.states.push(State::IndentlessSequenceEntry);
                return self.parse_node(event, true, false);
            }
            self.state = State::IndentlessSequenceEntry;
            return self.process_empty_scalar(event, mark);
        }
        self.pop_state();
        let t = self.tok();
        *event = Event {
            typ: EventType::SequenceEnd,
            start_mark: t.start_mark,
            end_mark: t.start_mark,
            ..Default::default()
        };
        true
    }

    fn parse_block_mapping_key(&mut self, event: &mut Event, first: bool) -> bool {
        if first {
            if self.peek().is_none() {
                return false;
            }
            self.marks.push(self.tok().start_mark);
            self.skip_token();
        }
        let Some(typ) = self.peek() else { return false };
        if typ == TokenType::Key {
            let mark = self.tok().end_mark;
            self.skip_token();
            let Some(typ) = self.peek() else { return false };
            if typ != TokenType::Key && typ != TokenType::Value && typ != TokenType::BlockEnd {
                self.states.push(State::BlockMappingValue);
                return self.parse_node(event, true, true);
            }
            self.state = State::BlockMappingValue;
            return self.process_empty_scalar(event, mark);
        } else if typ == TokenType::BlockEnd {
            self.pop_state();
            self.marks.pop();
            let t = self.tok();
            *event = Event {
                typ: EventType::MappingEnd,
                start_mark: t.start_mark,
                end_mark: t.end_mark,
                ..Default::default()
            };
            self.skip_token();
            return true;
        }
        let context_mark = self.marks.pop().unwrap_or_default();
        let m = self.tok().start_mark;
        self.set_parser_error_context(
            "while parsing a block mapping",
            context_mark,
            "did not find expected key",
            m,
        )
    }

    fn parse_block_mapping_value(&mut self, event: &mut Event) -> bool {
        let Some(typ) = self.peek() else { return false };
        if typ == TokenType::Value {
            let mark = self.tok().end_mark;
            self.skip_token();
            let Some(typ) = self.peek() else { return false };
            if typ != TokenType::Key && typ != TokenType::Value && typ != TokenType::BlockEnd {
                self.states.push(State::BlockMappingKey);
                return self.parse_node(event, true, true);
            }
            self.state = State::BlockMappingKey;
            return self.process_empty_scalar(event, mark);
        }
        self.state = State::BlockMappingKey;
        let m = self.tok().start_mark;
        self.process_empty_scalar(event, m)
    }

    fn parse_flow_sequence_entry(&mut self, event: &mut Event, first: bool) -> bool {
        if first {
            if self.peek().is_none() {
                return false;
            }
            self.marks.push(self.tok().start_mark);
            self.skip_token();
        }
        let Some(mut typ) = self.peek() else {
            return false;
        };
        if typ != TokenType::FlowSequenceEnd {
            if !first {
                if typ == TokenType::FlowEntry {
                    self.skip_token();
                    match self.peek() {
                        Some(t) => typ = t,
                        None => return false,
                    }
                } else {
                    let context_mark = self.marks.pop().unwrap_or_default();
                    let m = self.tok().start_mark;
                    return self.set_parser_error_context(
                        "while parsing a flow sequence",
                        context_mark,
                        "did not find expected ',' or ']'",
                        m,
                    );
                }
            }
            if typ == TokenType::Key {
                self.state = State::FlowSequenceEntryMappingKey;
                let t = self.tok();
                *event = Event {
                    typ: EventType::MappingStart,
                    start_mark: t.start_mark,
                    end_mark: t.end_mark,
                    implicit: true,
                    flow: true,
                    ..Default::default()
                };
                self.skip_token();
                return true;
            } else if typ != TokenType::FlowSequenceEnd {
                self.states.push(State::FlowSequenceEntry);
                return self.parse_node(event, false, false);
            }
        }
        self.pop_state();
        self.marks.pop();
        let t = self.tok();
        *event = Event {
            typ: EventType::SequenceEnd,
            start_mark: t.start_mark,
            end_mark: t.end_mark,
            ..Default::default()
        };
        self.skip_token();
        true
    }

    fn parse_flow_sequence_entry_mapping_key(&mut self, event: &mut Event) -> bool {
        let Some(typ) = self.peek() else { return false };
        if typ != TokenType::Value
            && typ != TokenType::FlowEntry
            && typ != TokenType::FlowSequenceEnd
        {
            self.states.push(State::FlowSequenceEntryMappingValue);
            return self.parse_node(event, false, false);
        }
        let mark = self.tok().end_mark;
        self.skip_token();
        self.state = State::FlowSequenceEntryMappingValue;
        self.process_empty_scalar(event, mark)
    }

    fn parse_flow_sequence_entry_mapping_value(&mut self, event: &mut Event) -> bool {
        let Some(typ) = self.peek() else { return false };
        if typ == TokenType::Value {
            self.skip_token();
            let Some(typ) = self.peek() else { return false };
            if typ != TokenType::FlowEntry && typ != TokenType::FlowSequenceEnd {
                self.states.push(State::FlowSequenceEntryMappingEnd);
                return self.parse_node(event, false, false);
            }
        }
        self.state = State::FlowSequenceEntryMappingEnd;
        let m = self.tok().start_mark;
        self.process_empty_scalar(event, m)
    }

    fn parse_flow_sequence_entry_mapping_end(&mut self, event: &mut Event) -> bool {
        if self.peek().is_none() {
            return false;
        }
        self.state = State::FlowSequenceEntry;
        let m = self.tok().start_mark;
        *event = Event {
            typ: EventType::MappingEnd,
            start_mark: m,
            end_mark: m,
            ..Default::default()
        };
        true
    }

    fn parse_flow_mapping_key(&mut self, event: &mut Event, first: bool) -> bool {
        if first {
            if self.peek().is_none() {
                return false;
            }
            self.marks.push(self.tok().start_mark);
            self.skip_token();
        }
        let Some(mut typ) = self.peek() else {
            return false;
        };
        if typ != TokenType::FlowMappingEnd {
            if !first {
                if typ == TokenType::FlowEntry {
                    self.skip_token();
                    match self.peek() {
                        Some(t) => typ = t,
                        None => return false,
                    }
                } else {
                    let context_mark = self.marks.pop().unwrap_or_default();
                    let m = self.tok().start_mark;
                    return self.set_parser_error_context(
                        "while parsing a flow mapping",
                        context_mark,
                        "did not find expected ',' or '}'",
                        m,
                    );
                }
            }
            if typ == TokenType::Key {
                self.skip_token();
                let Some(typ) = self.peek() else { return false };
                if typ != TokenType::Value
                    && typ != TokenType::FlowEntry
                    && typ != TokenType::FlowMappingEnd
                {
                    self.states.push(State::FlowMappingValue);
                    return self.parse_node(event, false, false);
                }
                self.state = State::FlowMappingValue;
                let m = self.tok().start_mark;
                return self.process_empty_scalar(event, m);
            } else if typ != TokenType::FlowMappingEnd {
                self.states.push(State::FlowMappingEmptyValue);
                return self.parse_node(event, false, false);
            }
        }
        self.pop_state();
        self.marks.pop();
        let t = self.tok();
        *event = Event {
            typ: EventType::MappingEnd,
            start_mark: t.start_mark,
            end_mark: t.end_mark,
            ..Default::default()
        };
        self.skip_token();
        true
    }

    fn parse_flow_mapping_value(&mut self, event: &mut Event, empty: bool) -> bool {
        let Some(typ) = self.peek() else { return false };
        if empty {
            self.state = State::FlowMappingKey;
            let m = self.tok().start_mark;
            return self.process_empty_scalar(event, m);
        }
        if typ == TokenType::Value {
            self.skip_token();
            let Some(typ) = self.peek() else { return false };
            if typ != TokenType::FlowEntry && typ != TokenType::FlowMappingEnd {
                self.states.push(State::FlowMappingKey);
                return self.parse_node(event, false, false);
            }
        }
        self.state = State::FlowMappingKey;
        let m = self.tok().start_mark;
        self.process_empty_scalar(event, m)
    }

    fn process_empty_scalar(&mut self, event: &mut Event, mark: Mark) -> bool {
        *event = Event {
            typ: EventType::Scalar,
            start_mark: mark,
            end_mark: mark,
            implicit: true,
            style: ScalarStyle::Plain,
            ..Default::default()
        };
        true
    }

    fn process_directives(&mut self) -> bool {
        let Some(mut typ) = self.peek() else {
            return false;
        };
        let mut has_version = false;
        while typ == TokenType::VersionDirective || typ == TokenType::TagDirective {
            let t = self.tok().clone();
            if typ == TokenType::VersionDirective {
                if has_version {
                    return self.set_parser_error("found duplicate %YAML directive", t.start_mark);
                }
                if t.major != 1 || t.minor != 1 {
                    return self.set_parser_error("found incompatible YAML document", t.start_mark);
                }
                has_version = true;
            } else {
                let value = TagDirective {
                    handle: t.value.clone(),
                    prefix: t.prefix.clone(),
                };
                if !self.append_tag_directive(value, false, t.start_mark) {
                    return false;
                }
            }
            self.skip_token();
            match self.peek() {
                Some(t) => typ = t,
                None => return false,
            }
        }
        let m = self.tok().start_mark;
        for td in default_tag_directives() {
            if !self.append_tag_directive(td, true, m) {
                return false;
            }
        }
        true
    }

    fn append_tag_directive(
        &mut self,
        value: TagDirective,
        allow_duplicates: bool,
        mark: Mark,
    ) -> bool {
        if self.tag_directives.iter().any(|t| t.handle == value.handle) {
            if allow_duplicates {
                return true;
            }
            return self.set_parser_error("found duplicate %TAG directive", mark);
        }
        self.tag_directives.push(value);
        true
    }
}
