//! Port of the libyaml emitter (`emitterc.go`) of goyaml v2 and v3. The two
//! versions differ in a handful of places, selected by [`Emitter::v3`].
//! Comments are not supported (they never occur when encoding values).

use alloc::string::String;
use alloc::vec::Vec;

use super::parser::{Event, EventType};
use super::scanner::ScalarStyle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    StreamStart,
    FirstDocumentStart,
    DocumentStart,
    DocumentContent,
    DocumentEnd,
    FlowSequenceFirstItem,
    FlowSequenceItem,
    FlowMappingFirstKey,
    FlowMappingKey,
    FlowMappingSimpleValue,
    FlowMappingValue,
    BlockSequenceFirstItem,
    BlockSequenceItem,
    BlockMappingFirstKey,
    BlockMappingKey,
    BlockMappingSimpleValue,
    BlockMappingValue,
    End,
}

#[derive(Default)]
struct ScalarData {
    value: Vec<u8>,
    multiline: bool,
    flow_plain_allowed: bool,
    block_plain_allowed: bool,
    single_quoted_allowed: bool,
    block_allowed: bool,
    style: ScalarStyle,
}

pub(crate) struct Emitter {
    pub v3: bool,
    pub out: Vec<u8>,
    pub best_indent: i64,
    pub best_width: i64,
    unicode: bool,
    canonical: bool,

    state: State,
    states: Vec<State>,
    events: Vec<Event>,
    events_head: usize,
    indents: Vec<i64>,
    tag_directives: Vec<(Vec<u8>, Vec<u8>)>,
    indent: i64,
    flow_level: i64,

    root_context: bool,
    sequence_context: bool,
    mapping_context: bool,
    simple_key_context: bool,

    line: i64,
    column: i64,
    whitespace: bool,
    indention: bool,
    pub open_ended: bool,

    anchor: Option<Vec<u8>>,
    anchor_alias: bool,
    tag_handle: Vec<u8>,
    tag_suffix: Vec<u8>,
    scalar: ScalarData,

    pub problem: String,
}

// Byte-class helpers on slices (out-of-range reads yield NUL).
#[inline]
fn at(b: &[u8], i: usize) -> u8 {
    b.get(i).copied().unwrap_or(0)
}
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
fn is_alpha(b: &[u8], i: usize) -> bool {
    let c = at(b, i);
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}
fn is_ascii(b: &[u8], i: usize) -> bool {
    at(b, i) <= 0x7F
}
fn is_printable(b: &[u8], i: usize) -> bool {
    let c = at(b, i);
    let c1 = at(b, i + 1);
    let c2 = at(b, i + 2);
    c == 0x0A
        || (0x20..=0x7E).contains(&c)
        || (c == 0xC2 && c1 >= 0xA0)
        || (c > 0xC2 && c < 0xED)
        || (c == 0xED && c1 < 0xA0)
        || c == 0xEE
        || (c == 0xEF && !(c1 == 0xBB && c2 == 0xBF) && !(c1 == 0xBF && (c2 == 0xBE || c2 == 0xBF)))
}
fn is_bom_quirk(b: &[u8]) -> bool {
    at(b, 0) == 0xEF && at(b, 1) == 0xBB && at(b, 2) == 0xBF
}
fn is_space(b: &[u8], i: usize) -> bool {
    at(b, i) == b' '
}
fn is_blank(b: &[u8], i: usize) -> bool {
    let c = at(b, i);
    c == b' ' || c == b'\t'
}
fn is_break(b: &[u8], i: usize) -> bool {
    let c = at(b, i);
    c == b'\r'
        || c == b'\n'
        || c == 0xC2 && at(b, i + 1) == 0x85
        || c == 0xE2 && at(b, i + 1) == 0x80 && (at(b, i + 2) == 0xA8 || at(b, i + 2) == 0xA9)
}
fn is_blankz(b: &[u8], i: usize) -> bool {
    is_blank(b, i) || is_break(b, i) || at(b, i) == 0
}

const DEFAULT_TAG_DIRECTIVES: [(&[u8], &[u8]); 2] = [(b"!", b"!"), (b"!!", b"tag:yaml.org,2002:")];

impl Emitter {
    pub fn new(v3: bool) -> Emitter {
        Emitter {
            v3,
            out: Vec::new(),
            best_indent: 0,
            best_width: if v3 { -1 } else { 0 },
            unicode: true,
            canonical: false,
            state: State::StreamStart,
            states: Vec::new(),
            events: Vec::new(),
            events_head: 0,
            indents: Vec::new(),
            tag_directives: Vec::new(),
            indent: 0,
            flow_level: 0,
            root_context: false,
            sequence_context: false,
            mapping_context: false,
            simple_key_context: false,
            line: 0,
            column: 0,
            whitespace: false,
            indention: false,
            open_ended: false,
            anchor: None,
            anchor_alias: false,
            tag_handle: Vec::new(),
            tag_suffix: Vec::new(),
            scalar: ScalarData::default(),
            problem: String::new(),
        }
    }

    fn error(&mut self, problem: &str) -> bool {
        self.problem = problem.into();
        false
    }

    // ---------------------------------------------------------------
    // Output primitives.

    fn put(&mut self, c: u8) -> bool {
        self.out.push(c);
        self.column += 1;
        true
    }

    fn put_break(&mut self) -> bool {
        self.out.push(b'\n');
        self.column = 0;
        self.line += 1;
        if self.v3 {
            self.indention = true;
        }
        true
    }

    fn write(&mut self, s: &[u8], i: &mut usize) -> bool {
        let w = width(at(s, *i)).max(1);
        for k in 0..w {
            self.out.push(at(s, *i + k));
        }
        self.column += 1;
        *i += w;
        true
    }

    fn write_all(&mut self, s: &[u8]) -> bool {
        let mut i = 0;
        while i < s.len() {
            self.write(s, &mut i);
        }
        true
    }

    fn write_break(&mut self, s: &[u8], i: &mut usize) -> bool {
        if at(s, *i) == b'\n' {
            self.put_break();
            *i += 1;
        } else {
            self.write(s, i);
            self.column = 0;
            self.line += 1;
            if self.v3 {
                self.indention = true;
            }
        }
        true
    }

    // ---------------------------------------------------------------

    /// `yaml_emitter_emit`.
    pub fn emit(&mut self, event: Event) -> bool {
        self.events.push(event);
        while !self.need_more_events() {
            let ev = core::mem::take(&mut self.events[self.events_head]);
            self.events[self.events_head].typ = ev.typ;
            if !self.analyze_event(&ev) {
                return false;
            }
            if !self.state_machine(&ev) {
                return false;
            }
            self.events_head += 1;
        }
        if self.events_head == self.events.len() {
            self.events.clear();
            self.events_head = 0;
        }
        true
    }

    fn need_more_events(&self) -> bool {
        if self.events_head == self.events.len() {
            return true;
        }
        let accumulate = match self.events[self.events_head].typ {
            EventType::DocumentStart => 1,
            EventType::SequenceStart => 2,
            EventType::MappingStart => 3,
            _ => return false,
        };
        if self.events.len() - self.events_head > accumulate {
            return false;
        }
        let mut level: i64 = 0;
        for e in &self.events[self.events_head..] {
            match e.typ {
                EventType::StreamStart
                | EventType::DocumentStart
                | EventType::SequenceStart
                | EventType::MappingStart => level += 1,
                EventType::StreamEnd | EventType::DocumentEnd | EventType::SequenceEnd | EventType::MappingEnd => {
                    level -= 1
                }
                _ => {}
            }
            if level == 0 {
                return false;
            }
        }
        true
    }

    fn append_tag_directive(&mut self, handle: &[u8], prefix: &[u8], allow_duplicates: bool) -> bool {
        if self.tag_directives.iter().any(|(h, _)| h == handle) {
            if allow_duplicates {
                return true;
            }
            return self.error("duplicate %TAG directive");
        }
        self.tag_directives.push((handle.to_vec(), prefix.to_vec()));
        true
    }

    fn increase_indent(&mut self, flow: bool, indentless: bool) -> bool {
        self.indents.push(self.indent);
        if self.indent < 0 {
            self.indent = if flow { self.best_indent } else { 0 };
        } else if !indentless {
            if self.v3 {
                if self.states.last() == Some(&State::BlockSequenceItem) {
                    self.indent += 2;
                } else {
                    self.indent = self.best_indent * ((self.indent + self.best_indent) / self.best_indent);
                }
            } else {
                self.indent += self.best_indent;
            }
        }
        true
    }

    fn pop_indent(&mut self) {
        self.indent = self.indents.pop().unwrap_or(-1);
    }

    fn pop_state(&mut self) {
        self.state = self.states.pop().unwrap_or(State::End);
    }

    fn state_machine(&mut self, ev: &Event) -> bool {
        match self.state {
            State::StreamStart => self.emit_stream_start(ev),
            State::FirstDocumentStart => self.emit_document_start(ev, true),
            State::DocumentStart => self.emit_document_start(ev, false),
            State::DocumentContent => self.emit_document_content(ev),
            State::DocumentEnd => self.emit_document_end(ev),
            State::FlowSequenceFirstItem => self.emit_flow_sequence_item(ev, true),
            State::FlowSequenceItem => self.emit_flow_sequence_item(ev, false),
            State::FlowMappingFirstKey => self.emit_flow_mapping_key(ev, true),
            State::FlowMappingKey => self.emit_flow_mapping_key(ev, false),
            State::FlowMappingSimpleValue => self.emit_flow_mapping_value(ev, true),
            State::FlowMappingValue => self.emit_flow_mapping_value(ev, false),
            State::BlockSequenceFirstItem => self.emit_block_sequence_item(ev, true),
            State::BlockSequenceItem => self.emit_block_sequence_item(ev, false),
            State::BlockMappingFirstKey => self.emit_block_mapping_key(ev, true),
            State::BlockMappingKey => self.emit_block_mapping_key(ev, false),
            State::BlockMappingSimpleValue => self.emit_block_mapping_value(ev, true),
            State::BlockMappingValue => self.emit_block_mapping_value(ev, false),
            State::End => self.error("expected nothing after STREAM-END"),
        }
    }

    fn emit_stream_start(&mut self, ev: &Event) -> bool {
        if ev.typ != EventType::StreamStart {
            return self.error("expected STREAM-START");
        }
        if self.best_indent < 2 || self.best_indent > 9 {
            self.best_indent = 2;
        }
        if self.best_width >= 0 && self.best_width <= self.best_indent * 2 {
            self.best_width = 80;
        }
        if self.best_width < 0 {
            self.best_width = (1 << 31) - 1;
        }
        self.indent = -1;
        self.line = 0;
        self.column = 0;
        self.whitespace = true;
        self.indention = true;
        self.state = State::FirstDocumentStart;
        true
    }

    fn emit_document_start(&mut self, ev: &Event, first: bool) -> bool {
        if ev.typ == EventType::DocumentStart {
            for (h, p) in DEFAULT_TAG_DIRECTIVES {
                if !self.append_tag_directive(h, p, true) {
                    return false;
                }
            }
            let mut implicit = ev.implicit;
            if !first || self.canonical {
                implicit = false;
            }
            if !implicit {
                self.write_indent();
                self.write_indicator(b"---", true, false, false);
                if self.canonical || self.v3 {
                    self.write_indent();
                }
            }
            self.state = State::DocumentContent;
            return true;
        }
        if ev.typ == EventType::StreamEnd {
            if self.open_ended {
                self.write_indicator(b"...", true, false, false);
                self.write_indent();
            }
            self.state = State::End;
            return true;
        }
        self.error("expected DOCUMENT-START or STREAM-END")
    }

    fn emit_document_content(&mut self, ev: &Event) -> bool {
        self.states.push(State::DocumentEnd);
        self.emit_node(ev, true, false, false, false)
    }

    fn emit_document_end(&mut self, ev: &Event) -> bool {
        if ev.typ != EventType::DocumentEnd {
            return self.error("expected DOCUMENT-END");
        }
        self.write_indent();
        if !ev.implicit {
            self.write_indicator(b"...", true, false, false);
            self.write_indent();
        }
        self.state = State::DocumentStart;
        self.tag_directives.clear();
        true
    }

    fn emit_flow_sequence_item(&mut self, ev: &Event, first: bool) -> bool {
        if first {
            self.write_indicator(b"[", true, true, false);
            self.increase_indent(true, false);
            self.flow_level += 1;
        }
        if ev.typ == EventType::SequenceEnd {
            self.flow_level -= 1;
            self.pop_indent();
            if self.v3 {
                if self.column == 0 || self.canonical && !first {
                    self.write_indent();
                }
            } else if self.canonical && !first {
                self.write_indicator(b",", false, false, false);
                self.write_indent();
            }
            self.write_indicator(b"]", false, false, false);
            self.pop_state();
            return true;
        }
        if !first {
            self.write_indicator(b",", false, false, false);
        }
        if self.v3 && self.column == 0 {
            self.write_indent();
        }
        if self.canonical || self.column > self.best_width {
            self.write_indent();
        }
        self.states.push(State::FlowSequenceItem);
        self.emit_node(ev, false, true, false, false)
    }

    fn emit_flow_mapping_key(&mut self, ev: &Event, first: bool) -> bool {
        if first {
            self.write_indicator(b"{", true, true, false);
            self.increase_indent(true, false);
            self.flow_level += 1;
        }
        if ev.typ == EventType::MappingEnd {
            self.flow_level -= 1;
            self.pop_indent();
            if self.canonical && !first {
                if !self.v3 {
                    self.write_indicator(b",", false, false, false);
                }
                self.write_indent();
            }
            self.write_indicator(b"}", false, false, false);
            self.pop_state();
            return true;
        }
        if !first {
            self.write_indicator(b",", false, false, false);
        }
        if self.v3 && self.column == 0 {
            self.write_indent();
        }
        if self.canonical || self.column > self.best_width {
            self.write_indent();
        }
        if !self.canonical && self.check_simple_key() {
            self.states.push(State::FlowMappingSimpleValue);
            return self.emit_node(ev, false, false, true, true);
        }
        self.write_indicator(b"?", true, false, false);
        self.states.push(State::FlowMappingValue);
        self.emit_node(ev, false, false, true, false)
    }

    fn emit_flow_mapping_value(&mut self, ev: &Event, simple: bool) -> bool {
        if simple {
            self.write_indicator(b":", false, false, false);
        } else {
            if self.canonical || self.column > self.best_width {
                self.write_indent();
            }
            self.write_indicator(b":", true, false, false);
        }
        self.states.push(State::FlowMappingKey);
        self.emit_node(ev, false, false, true, false)
    }

    fn emit_block_sequence_item(&mut self, ev: &Event, first: bool) -> bool {
        if first {
            if self.v3 {
                self.increase_indent(false, false);
            } else {
                let indentless = self.mapping_context && !self.indention;
                self.increase_indent(false, indentless);
            }
        }
        if ev.typ == EventType::SequenceEnd {
            self.pop_indent();
            self.pop_state();
            return true;
        }
        self.write_indent();
        self.write_indicator(b"-", true, false, true);
        self.states.push(State::BlockSequenceItem);
        self.emit_node(ev, false, true, false, false)
    }

    fn emit_block_mapping_key(&mut self, ev: &Event, first: bool) -> bool {
        if first {
            self.increase_indent(false, false);
        }
        if ev.typ == EventType::MappingEnd {
            self.pop_indent();
            self.pop_state();
            return true;
        }
        self.write_indent();
        if self.check_simple_key() {
            self.states.push(State::BlockMappingSimpleValue);
            return self.emit_node(ev, false, false, true, true);
        }
        self.write_indicator(b"?", true, false, true);
        self.states.push(State::BlockMappingValue);
        self.emit_node(ev, false, false, true, false)
    }

    fn emit_block_mapping_value(&mut self, ev: &Event, simple: bool) -> bool {
        if simple {
            self.write_indicator(b":", false, false, false);
        } else {
            self.write_indent();
            self.write_indicator(b":", true, false, true);
        }
        self.states.push(State::BlockMappingKey);
        self.emit_node(ev, false, false, true, false)
    }

    fn emit_node(&mut self, ev: &Event, root: bool, sequence: bool, mapping: bool, simple_key: bool) -> bool {
        self.root_context = root;
        self.sequence_context = sequence;
        self.mapping_context = mapping;
        self.simple_key_context = simple_key;
        match ev.typ {
            EventType::Alias => self.emit_alias(),
            EventType::Scalar => self.emit_scalar(ev),
            EventType::SequenceStart => self.emit_sequence_start(ev),
            EventType::MappingStart => self.emit_mapping_start(ev),
            _ => self.error("expected SCALAR, SEQUENCE-START, MAPPING-START, or ALIAS"),
        }
    }

    fn emit_alias(&mut self) -> bool {
        self.process_anchor();
        self.pop_state();
        true
    }

    fn emit_scalar(&mut self, ev: &Event) -> bool {
        if !self.select_scalar_style(ev) {
            return false;
        }
        self.process_anchor();
        self.process_tag();
        self.increase_indent(true, false);
        self.process_scalar();
        self.pop_indent();
        self.pop_state();
        true
    }

    fn emit_sequence_start(&mut self, ev: &Event) -> bool {
        self.process_anchor();
        self.process_tag();
        if self.flow_level > 0 || self.canonical || ev.flow || self.check_empty_sequence() {
            self.state = State::FlowSequenceFirstItem;
        } else {
            self.state = State::BlockSequenceFirstItem;
        }
        true
    }

    fn emit_mapping_start(&mut self, ev: &Event) -> bool {
        self.process_anchor();
        self.process_tag();
        if self.flow_level > 0 || self.canonical || ev.flow || self.check_empty_mapping() {
            self.state = State::FlowMappingFirstKey;
        } else {
            self.state = State::BlockMappingFirstKey;
        }
        true
    }

    fn check_empty_sequence(&self) -> bool {
        if self.events.len() - self.events_head < 2 {
            return false;
        }
        self.events[self.events_head].typ == EventType::SequenceStart
            && self.events[self.events_head + 1].typ == EventType::SequenceEnd
    }

    fn check_empty_mapping(&self) -> bool {
        if self.events.len() - self.events_head < 2 {
            return false;
        }
        self.events[self.events_head].typ == EventType::MappingStart
            && self.events[self.events_head + 1].typ == EventType::MappingEnd
    }

    fn check_simple_key(&self) -> bool {
        let anchor_len = self.anchor.as_ref().map_or(0, |a| a.len());
        let length = match self.events[self.events_head].typ {
            EventType::Alias => anchor_len,
            EventType::Scalar => {
                if self.scalar.multiline {
                    return false;
                }
                anchor_len + self.tag_handle.len() + self.tag_suffix.len() + self.scalar.value.len()
            }
            EventType::SequenceStart => {
                if !self.check_empty_sequence() {
                    return false;
                }
                anchor_len + self.tag_handle.len() + self.tag_suffix.len()
            }
            EventType::MappingStart => {
                if !self.check_empty_mapping() {
                    return false;
                }
                anchor_len + self.tag_handle.len() + self.tag_suffix.len()
            }
            _ => return false,
        };
        length <= 128
    }

    fn select_scalar_style(&mut self, ev: &Event) -> bool {
        let no_tag = self.tag_handle.is_empty() && self.tag_suffix.is_empty();
        if no_tag && !ev.implicit && !ev.quoted_implicit {
            return self.error("neither tag nor implicit flags are specified");
        }
        let mut style = ev.style;
        if style == ScalarStyle::Any {
            style = ScalarStyle::Plain;
        }
        if self.canonical {
            style = ScalarStyle::DoubleQuoted;
        }
        if self.simple_key_context && self.scalar.multiline {
            style = ScalarStyle::DoubleQuoted;
        }
        if style == ScalarStyle::Plain {
            if self.flow_level > 0 && !self.scalar.flow_plain_allowed
                || self.flow_level == 0 && !self.scalar.block_plain_allowed
            {
                style = ScalarStyle::SingleQuoted;
            }
            if self.scalar.value.is_empty() && (self.flow_level > 0 || self.simple_key_context) {
                style = ScalarStyle::SingleQuoted;
            }
            if no_tag && !ev.implicit {
                style = ScalarStyle::SingleQuoted;
            }
        }
        if style == ScalarStyle::SingleQuoted && !self.scalar.single_quoted_allowed {
            style = ScalarStyle::DoubleQuoted;
        }
        if (style == ScalarStyle::Literal || style == ScalarStyle::Folded)
            && (!self.scalar.block_allowed || self.flow_level > 0 || self.simple_key_context)
        {
            style = ScalarStyle::DoubleQuoted;
        }
        if no_tag && !ev.quoted_implicit && style != ScalarStyle::Plain {
            self.tag_handle = b"!".to_vec();
        }
        self.scalar.style = style;
        true
    }

    fn process_anchor(&mut self) {
        let Some(anchor) = self.anchor.clone() else { return };
        let c: &[u8] = if self.anchor_alias { b"*" } else { b"&" };
        self.write_indicator(c, true, false, false);
        self.write_all(&anchor);
        self.whitespace = false;
        self.indention = false;
    }

    fn process_tag(&mut self) {
        if self.tag_handle.is_empty() && self.tag_suffix.is_empty() {
            return;
        }
        if !self.tag_handle.is_empty() {
            let h = self.tag_handle.clone();
            self.write_tag_handle(&h);
            if !self.tag_suffix.is_empty() {
                let s = self.tag_suffix.clone();
                self.write_tag_content(&s, false);
            }
        } else {
            self.write_indicator(b"!<", true, false, false);
            let s = self.tag_suffix.clone();
            self.write_tag_content(&s, false);
            self.write_indicator(b">", false, false, false);
        }
    }

    fn process_scalar(&mut self) {
        let value = core::mem::take(&mut self.scalar.value);
        let allow_breaks = !self.simple_key_context;
        match self.scalar.style {
            ScalarStyle::Plain | ScalarStyle::Any => self.write_plain_scalar(&value, allow_breaks),
            ScalarStyle::SingleQuoted => self.write_single_quoted_scalar(&value, allow_breaks),
            ScalarStyle::DoubleQuoted => self.write_double_quoted_scalar(&value, allow_breaks),
            ScalarStyle::Literal => self.write_literal_scalar(&value),
            ScalarStyle::Folded => self.write_folded_scalar(&value),
        }
        self.scalar.value = value;
    }

    fn analyze_anchor(&mut self, anchor: &[u8], alias: bool) -> bool {
        if anchor.is_empty() {
            return self.error(if alias { "alias value must not be empty" } else { "anchor value must not be empty" });
        }
        let mut i = 0;
        while i < anchor.len() {
            if !is_alpha(anchor, i) {
                return self.error(if alias {
                    "alias value must contain alphanumerical characters only"
                } else {
                    "anchor value must contain alphanumerical characters only"
                });
            }
            i += width(anchor[i]).max(1);
        }
        self.anchor = Some(anchor.to_vec());
        self.anchor_alias = alias;
        true
    }

    fn analyze_tag(&mut self, tag: &[u8]) -> bool {
        if tag.is_empty() {
            return self.error("tag value must not be empty");
        }
        for (h, p) in &self.tag_directives {
            if tag.starts_with(p) {
                self.tag_handle = h.clone();
                self.tag_suffix = tag[p.len()..].to_vec();
                return true;
            }
        }
        self.tag_suffix = tag.to_vec();
        true
    }

    fn analyze_scalar(&mut self, value: &[u8]) {
        let mut block_indicators = false;
        let mut flow_indicators = false;
        let mut line_breaks = false;
        let mut special_characters = false;
        let mut tab_characters = false;
        let mut leading_space = false;
        let mut leading_break = false;
        let mut trailing_space = false;
        let mut trailing_break = false;
        let mut break_space = false;
        let mut space_break = false;
        let mut previous_space = false;
        let mut previous_break = false;

        self.scalar.value = value.to_vec();
        if value.is_empty() {
            self.scalar.multiline = false;
            self.scalar.flow_plain_allowed = false;
            self.scalar.block_plain_allowed = true;
            self.scalar.single_quoted_allowed = true;
            self.scalar.block_allowed = false;
            return;
        }
        if value.len() >= 3 && (value.starts_with(b"---") || value.starts_with(b"...")) {
            block_indicators = true;
            flow_indicators = true;
        }
        let mut preceded_by_whitespace = true;
        let mut i = 0;
        while i < value.len() {
            let w = width(value[i]).max(1);
            let followed_by_whitespace = i + w >= value.len() || is_blank(value, i + w);
            if i == 0 {
                match value[i] {
                    b'#' | b',' | b'[' | b']' | b'{' | b'}' | b'&' | b'*' | b'!' | b'|' | b'>' | b'\'' | b'"'
                    | b'%' | b'@' | b'`' => {
                        flow_indicators = true;
                        block_indicators = true;
                    }
                    b'?' | b':' => {
                        flow_indicators = true;
                        if followed_by_whitespace {
                            block_indicators = true;
                        }
                    }
                    b'-' if followed_by_whitespace => {
                        flow_indicators = true;
                        block_indicators = true;
                    }
                    _ => {}
                }
            } else {
                match value[i] {
                    b',' | b'?' | b'[' | b']' | b'{' | b'}' => flow_indicators = true,
                    b':' => {
                        flow_indicators = true;
                        if followed_by_whitespace {
                            block_indicators = true;
                        }
                    }
                    b'#' if preceded_by_whitespace => {
                        flow_indicators = true;
                        block_indicators = true;
                    }
                    _ => {}
                }
            }
            if self.v3 && value[i] == b'\t' {
                tab_characters = true;
            } else if !is_printable(value, i) || !is_ascii(value, i) && !self.unicode {
                special_characters = true;
            }
            if is_space(value, i) {
                if i == 0 {
                    leading_space = true;
                }
                if i + w == value.len() {
                    trailing_space = true;
                }
                if previous_break {
                    break_space = true;
                }
                previous_space = true;
                previous_break = false;
            } else if is_break(value, i) {
                line_breaks = true;
                if i == 0 {
                    leading_break = true;
                }
                if i + w == value.len() {
                    trailing_break = true;
                }
                if previous_space {
                    space_break = true;
                }
                previous_space = false;
                previous_break = true;
            } else {
                previous_space = false;
                previous_break = false;
            }
            preceded_by_whitespace = is_blankz(value, i);
            i += w;
        }
        let sd = &mut self.scalar;
        sd.multiline = line_breaks;
        sd.flow_plain_allowed = true;
        sd.block_plain_allowed = true;
        sd.single_quoted_allowed = true;
        sd.block_allowed = true;
        if leading_space || leading_break || trailing_space || trailing_break {
            sd.flow_plain_allowed = false;
            sd.block_plain_allowed = false;
        }
        if trailing_space {
            sd.block_allowed = false;
        }
        if break_space {
            sd.flow_plain_allowed = false;
            sd.block_plain_allowed = false;
            sd.single_quoted_allowed = false;
        }
        if space_break || tab_characters || special_characters {
            sd.flow_plain_allowed = false;
            sd.block_plain_allowed = false;
            sd.single_quoted_allowed = false;
        }
        if space_break || special_characters {
            sd.block_allowed = false;
        }
        if line_breaks {
            sd.flow_plain_allowed = false;
            sd.block_plain_allowed = false;
        }
        if flow_indicators {
            sd.flow_plain_allowed = false;
        }
        if block_indicators {
            sd.block_plain_allowed = false;
        }
    }

    fn analyze_event(&mut self, ev: &Event) -> bool {
        self.anchor = None;
        self.tag_handle.clear();
        self.tag_suffix.clear();
        self.scalar.value.clear();
        match ev.typ {
            EventType::Alias => {
                if !self.analyze_anchor(&ev.anchor, true) {
                    return false;
                }
            }
            EventType::Scalar => {
                if !ev.anchor.is_empty() && !self.analyze_anchor(&ev.anchor, false) {
                    return false;
                }
                if !ev.tag.is_empty()
                    && (self.canonical || (!ev.implicit && !ev.quoted_implicit))
                    && !self.analyze_tag(&ev.tag)
                {
                    return false;
                }
                self.analyze_scalar(&ev.value);
            }
            EventType::SequenceStart | EventType::MappingStart => {
                if !ev.anchor.is_empty() && !self.analyze_anchor(&ev.anchor, false) {
                    return false;
                }
                if !ev.tag.is_empty() && (self.canonical || !ev.implicit) && !self.analyze_tag(&ev.tag) {
                    return false;
                }
            }
            _ => {}
        }
        true
    }

    fn write_indent(&mut self) -> bool {
        let indent = self.indent.max(0);
        if !self.indention || self.column > indent || (self.column == indent && !self.whitespace) {
            self.put_break();
        }
        while self.column < indent {
            self.put(b' ');
        }
        self.whitespace = true;
        if !self.v3 {
            self.indention = true;
        }
        true
    }

    fn write_indicator(
        &mut self,
        indicator: &[u8],
        need_whitespace: bool,
        is_whitespace: bool,
        is_indention: bool,
    ) -> bool {
        if need_whitespace && !self.whitespace {
            self.put(b' ');
        }
        self.write_all(indicator);
        self.whitespace = is_whitespace;
        self.indention = self.indention && is_indention;
        self.open_ended = false;
        true
    }

    fn write_tag_handle(&mut self, value: &[u8]) {
        if !self.whitespace {
            self.put(b' ');
        }
        self.write_all(value);
        self.whitespace = false;
        self.indention = false;
    }

    fn write_tag_content(&mut self, value: &[u8], need_whitespace: bool) {
        if need_whitespace && !self.whitespace {
            self.put(b' ');
        }
        let mut i = 0;
        while i < value.len() {
            let c = value[i];
            let must_write = matches!(
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
                    | b'_'
                    | b'.'
                    | b'~'
                    | b'*'
                    | b'\''
                    | b'('
                    | b')'
                    | b'['
                    | b']'
            ) || is_alpha(value, i);
            if must_write {
                self.write(value, &mut i);
            } else {
                let w = width(c).max(1);
                for _ in 0..w {
                    let octet = at(value, i);
                    i += 1;
                    self.put(b'%');
                    let hex = |d: u8| if d < 10 { b'0' + d } else { b'A' + d - 10 };
                    self.put(hex(octet >> 4));
                    self.put(hex(octet & 0x0f));
                }
            }
        }
        self.whitespace = false;
        self.indention = false;
    }

    fn write_plain_scalar(&mut self, value: &[u8], allow_breaks: bool) {
        if (!self.v3 || !value.is_empty()) && !self.whitespace {
            self.put(b' ');
        }
        let mut spaces = false;
        let mut breaks = false;
        let mut i = 0;
        while i < value.len() {
            if is_space(value, i) {
                if allow_breaks && !spaces && self.column > self.best_width && !is_space(value, i + 1) {
                    self.write_indent();
                    i += width(value[i]).max(1);
                } else {
                    self.write(value, &mut i);
                }
                spaces = true;
            } else if is_break(value, i) {
                if !breaks && value[i] == b'\n' {
                    self.put_break();
                }
                self.write_break(value, &mut i);
                if !self.v3 {
                    self.indention = true;
                }
                breaks = true;
            } else {
                if breaks {
                    self.write_indent();
                }
                self.write(value, &mut i);
                self.indention = false;
                spaces = false;
                breaks = false;
            }
        }
        if !self.v3 || !value.is_empty() {
            self.whitespace = false;
        }
        self.indention = false;
        if self.root_context {
            self.open_ended = true;
        }
    }

    fn write_single_quoted_scalar(&mut self, value: &[u8], allow_breaks: bool) {
        self.write_indicator(b"'", true, false, false);
        let mut spaces = false;
        let mut breaks = false;
        let mut i = 0;
        while i < value.len() {
            if is_space(value, i) {
                if allow_breaks
                    && !spaces
                    && self.column > self.best_width
                    && i > 0
                    && i < value.len() - 1
                    && !is_space(value, i + 1)
                {
                    self.write_indent();
                    i += width(value[i]).max(1);
                } else {
                    self.write(value, &mut i);
                }
                spaces = true;
            } else if is_break(value, i) {
                if !breaks && value[i] == b'\n' {
                    self.put_break();
                }
                self.write_break(value, &mut i);
                if !self.v3 {
                    self.indention = true;
                }
                breaks = true;
            } else {
                if breaks {
                    self.write_indent();
                }
                if value[i] == b'\'' {
                    self.put(b'\'');
                }
                self.write(value, &mut i);
                self.indention = false;
                spaces = false;
                breaks = false;
            }
        }
        self.write_indicator(b"'", false, false, false);
        self.whitespace = false;
        self.indention = false;
    }

    fn write_double_quoted_scalar(&mut self, value: &[u8], allow_breaks: bool) {
        let mut spaces = false;
        self.write_indicator(b"\"", true, false, false);
        let mut i = 0;
        while i < value.len() {
            if !is_printable(value, i)
                || (!self.unicode && !is_ascii(value, i))
                || is_bom_quirk(value)
                || is_break(value, i)
                || value[i] == b'"'
                || value[i] == b'\\'
            {
                let octet = value[i];
                let (mut w, mut v): (usize, u32) = if octet & 0x80 == 0 {
                    (1, (octet & 0x7F) as u32)
                } else if octet & 0xE0 == 0xC0 {
                    (2, (octet & 0x1F) as u32)
                } else if octet & 0xF0 == 0xE0 {
                    (3, (octet & 0x0F) as u32)
                } else if octet & 0xF8 == 0xF0 {
                    (4, (octet & 0x07) as u32)
                } else {
                    (1, 0)
                };
                for k in 1..w {
                    v = (v << 6) + (at(value, i + k) & 0x3F) as u32;
                }
                i += w.max(1);
                self.put(b'\\');
                match v {
                    0x00 => {
                        self.put(b'0');
                    }
                    0x07 => {
                        self.put(b'a');
                    }
                    0x08 => {
                        self.put(b'b');
                    }
                    0x09 => {
                        self.put(b't');
                    }
                    0x0A => {
                        self.put(b'n');
                    }
                    0x0B => {
                        self.put(b'v');
                    }
                    0x0C => {
                        self.put(b'f');
                    }
                    0x0D => {
                        self.put(b'r');
                    }
                    0x1B => {
                        self.put(b'e');
                    }
                    0x22 => {
                        self.put(b'"');
                    }
                    0x5C => {
                        self.put(b'\\');
                    }
                    0x85 => {
                        self.put(b'N');
                    }
                    0xA0 => {
                        self.put(b'_');
                    }
                    0x2028 => {
                        self.put(b'L');
                    }
                    0x2029 => {
                        self.put(b'P');
                    }
                    _ => {
                        if v <= 0xFF {
                            self.put(b'x');
                            w = 2;
                        } else if v <= 0xFFFF {
                            self.put(b'u');
                            w = 4;
                        } else {
                            self.put(b'U');
                            w = 8;
                        }
                        let mut k = (w as i32 - 1) * 4;
                        while k >= 0 {
                            let digit = ((v >> k) & 0x0F) as u8;
                            self.put(if digit < 10 { digit + b'0' } else { digit + b'A' - 10 });
                            k -= 4;
                        }
                    }
                }
                spaces = false;
            } else if is_space(value, i) {
                if allow_breaks && !spaces && self.column > self.best_width && i > 0 && i < value.len() - 1 {
                    self.write_indent();
                    if is_space(value, i + 1) {
                        self.put(b'\\');
                    }
                    i += width(value[i]).max(1);
                } else {
                    self.write(value, &mut i);
                }
                spaces = true;
            } else {
                self.write(value, &mut i);
                spaces = false;
            }
        }
        self.write_indicator(b"\"", false, false, false);
        self.whitespace = false;
        self.indention = false;
    }

    fn write_block_scalar_hints(&mut self, value: &[u8]) {
        if is_space(value, 0) || is_break(value, 0) {
            let hint = [b'0' + self.best_indent as u8];
            self.write_indicator(&hint, false, false, false);
        }
        self.open_ended = false;
        let mut chomp: Option<u8> = None;
        if value.is_empty() {
            chomp = Some(b'-');
        } else {
            let mut i = value.len() - 1;
            while i > 0 && value[i] & 0xC0 == 0x80 {
                i -= 1;
            }
            if !is_break(value, i) {
                chomp = Some(b'-');
            } else if i == 0 {
                chomp = Some(b'+');
                self.open_ended = true;
            } else {
                i -= 1;
                while i > 0 && value[i] & 0xC0 == 0x80 {
                    i -= 1;
                }
                if is_break(value, i) {
                    chomp = Some(b'+');
                    self.open_ended = true;
                }
            }
        }
        if let Some(c) = chomp {
            self.write_indicator(&[c], false, false, false);
        }
    }

    fn write_literal_scalar(&mut self, value: &[u8]) {
        self.write_indicator(b"|", true, false, false);
        self.write_block_scalar_hints(value);
        self.put_break();
        self.indention = true;
        self.whitespace = true;
        let mut breaks = true;
        let mut i = 0;
        while i < value.len() {
            if is_break(value, i) {
                self.write_break(value, &mut i);
                if !self.v3 {
                    self.indention = true;
                }
                breaks = true;
            } else {
                if breaks {
                    self.write_indent();
                }
                self.write(value, &mut i);
                self.indention = false;
                breaks = false;
            }
        }
    }

    fn write_folded_scalar(&mut self, value: &[u8]) {
        self.write_indicator(b">", true, false, false);
        self.write_block_scalar_hints(value);
        self.put_break();
        self.indention = true;
        self.whitespace = true;
        let mut breaks = true;
        let mut leading_spaces = true;
        let mut i = 0;
        while i < value.len() {
            if is_break(value, i) {
                if !breaks && !leading_spaces && value[i] == b'\n' {
                    let mut k = 0;
                    while is_break(value, k) {
                        k += width(value[k]).max(1);
                    }
                    if !is_blankz(value, k) {
                        self.put_break();
                    }
                }
                self.write_break(value, &mut i);
                if !self.v3 {
                    self.indention = true;
                }
                breaks = true;
            } else {
                if breaks {
                    self.write_indent();
                    leading_spaces = is_blank(value, i);
                }
                if !breaks && is_space(value, i) && !is_space(value, i + 1) && self.column > self.best_width {
                    self.write_indent();
                    i += width(value[i]).max(1);
                } else {
                    self.write(value, &mut i);
                }
                self.indention = false;
                breaks = false;
            }
        }
    }
}
