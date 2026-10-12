//! Parser: a port of Go's `text/template/parse/parse.go`.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use super::ast::*;
use super::lex::{Item, ItemType, Lexer};
use super::num::{self, NumErr};
use super::quote::{self, UnquotedChar};

/// Maximum nesting of parenthesized pipelines (Go: 10000). Kept lower to stay
/// well within the native stack.
const MAX_STACK_DEPTH: usize = 300;

/// Maximum nesting of control structures (`if`/`range`/`with`/`block`). Go
/// has no limit; this guards the native stack.
const MAX_NESTING: usize = 300;

/// A template syntax error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The bare message, e.g. `unexpected "}" in operand`.
    pub message: String,
    /// Go's full message: `template: NAME:LINE: MESSAGE`.
    pub go_message: String,
    /// Template (file) name.
    pub name: String,
    /// 1-based line (as Go reports it).
    pub line: usize,
    /// Byte span of the offending token, for diagnostics.
    pub span: Span,
    /// The template text.
    pub source: Option<Rc<str>>,
}

struct Unwind;

type PResult<T> = Result<T, Unwind>;

/// Parses `text` into a set of trees: the main tree named `name` plus one per
/// `define`/`block`. `has_function` reports whether a function is defined.
pub fn parse(
    name: &str,
    text: Rc<str>,
    has_function: &dyn Fn(&str) -> bool,
) -> Result<BTreeMap<String, Rc<Tree>>, ParseError> {
    let src: &str = &text;
    let mut lexer = Lexer::new(src);
    lexer.break_ok = !has_function("break");
    lexer.continue_ok = !has_function("continue");
    let mut p = Parser {
        lex: lexer,
        token: [dummy_item(), dummy_item(), dummy_item()],
        peek_count: 0,
        vars: vec![String::from("$")],
        tree_set: BTreeMap::new(),
        action_line: 0,
        action_pos: 0,
        range_depth: 0,
        stack_depth: 0,
        nesting: 0,
        parse_name: name.to_string(),
        text: text.clone(),
        has_function,
        error: None,
    };
    match p.parse_top(name) {
        Ok(()) => Ok(p.tree_set),
        Err(Unwind) => Err(p.error.take().expect("parse error recorded")),
    }
}

fn dummy_item() -> Item {
    Item { typ: ItemType::Eof, pos: 0, val: String::new(), line: 0, end: 0 }
}

struct Parser<'a> {
    lex: Lexer<'a>,
    token: [Item; 3],
    peek_count: usize,
    vars: Vec<String>,
    tree_set: BTreeMap<String, Rc<Tree>>,
    action_line: usize,
    action_pos: usize,
    range_depth: usize,
    stack_depth: usize,
    nesting: usize,
    parse_name: String,
    text: Rc<str>,
    has_function: &'a dyn Fn(&str) -> bool,
    error: Option<ParseError>,
}

enum Parsed {
    Node(Node),
    End,
    Else(Pos),
}

impl<'a> Parser<'a> {
    fn next(&mut self) -> Item {
        if self.peek_count > 0 {
            self.peek_count -= 1;
        } else {
            self.token[0] = self.lex.next_item();
        }
        self.token[self.peek_count].clone()
    }

    fn backup(&mut self) {
        self.peek_count += 1;
    }

    fn backup2(&mut self, t1: Item) {
        self.token[1] = t1;
        self.peek_count = 2;
    }

    fn backup3(&mut self, t2: Item, t1: Item) {
        self.token[1] = t1;
        self.token[2] = t2;
        self.peek_count = 3;
    }

    fn peek(&mut self) -> Item {
        if self.peek_count > 0 {
            return self.token[self.peek_count - 1].clone();
        }
        self.peek_count = 1;
        self.token[0] = self.lex.next_item();
        self.token[0].clone()
    }

    fn next_non_space(&mut self) -> Item {
        loop {
            let t = self.next();
            if t.typ != ItemType::Space {
                return t;
            }
        }
    }

    fn peek_non_space(&mut self) -> Item {
        let t = self.next_non_space();
        self.backup();
        t
    }

    /// Records an error at the most recently lexed token (Go uses
    /// `token[0].line`).
    fn errorf<T>(&mut self, msg: String) -> PResult<T> {
        let tok = &self.token[0];
        let span = tok.pos..tok.end.max(tok.pos);
        self.fail(msg, span)
    }

    fn fail<T>(&mut self, msg: String, span: Span) -> PResult<T> {
        let line = self.token[0].line;
        let go_message = format!("template: {}:{}: {}", self.parse_name, line, msg);
        if self.error.is_none() {
            self.error = Some(ParseError {
                message: msg,
                go_message,
                name: self.parse_name.clone(),
                line,
                span,
                source: Some(self.text.clone()),
            });
        }
        Err(Unwind)
    }

    fn expect(&mut self, expected: ItemType, context: &str) -> PResult<Item> {
        let token = self.next_non_space();
        if token.typ != expected {
            return self.unexpected(&token, context);
        }
        Ok(token)
    }

    fn expect_one_of(&mut self, e1: ItemType, e2: ItemType, context: &str) -> PResult<Item> {
        let token = self.next_non_space();
        if token.typ != e1 && token.typ != e2 {
            return self.unexpected(&token, context);
        }
        Ok(token)
    }

    fn unexpected<T>(&mut self, token: &Item, context: &str) -> PResult<T> {
        if token.typ == ItemType::Error {
            let mut extra = String::new();
            if self.action_line != 0 && self.action_line != token.line {
                extra = format!(" in action started at {}:{}", self.parse_name, self.action_line);
                if token.val.ends_with(" action") {
                    extra = extra[" in action".len()..].to_string();
                }
            }
            // For unterminated constructs, point at where the action began.
            let span = if token.val == "unclosed action" || token.val == "unclosed left paren" {
                self.action_pos..self.action_pos + 2
            } else {
                token.pos..token.end.max(token.pos + 1)
            };
            return self.fail(format!("{}{}", token.describe(), extra), span);
        }
        let span = token.pos..token.end.max(token.pos);
        self.fail(format!("unexpected {} in {}", token.describe(), context), span)
    }

    fn new_tree(&self, name: String, root: ListNode) -> Tree {
        Tree { name, parse_name: self.parse_name.clone(), text: self.text.clone(), root }
    }

    /// Go's `Tree.add`.
    fn add(&mut self, tree: Tree) -> PResult<()> {
        let empty_new = is_empty_list(&tree.root, &self.text);
        if let Some(existing) = self.tree_set.get(&tree.name)
            && !is_empty_list(&existing.root, &existing.text)
        {
            if !empty_new {
                let msg = format!("template: multiple definition of template {}", quote::go_quote(&tree.name));
                return self.errorf(msg);
            }
            return Ok(());
        }
        self.tree_set.insert(tree.name.clone(), Rc::new(tree));
        Ok(())
    }

    fn parse_top(&mut self, name: &str) -> PResult<()> {
        let pos = self.peek().pos;
        let mut root = ListNode { pos, nodes: Vec::new() };
        while self.peek().typ != ItemType::Eof {
            if self.peek().typ == ItemType::LeftDelim {
                let delim = self.next();
                if self.next_non_space().typ == ItemType::Define {
                    // A new tree shares the lexer; vars are reset.
                    let saved_vars = core::mem::replace(&mut self.vars, vec![String::from("$")]);
                    let saved_depth = self.stack_depth;
                    self.stack_depth = 0;
                    let r = self.parse_definition();
                    self.vars = saved_vars;
                    self.stack_depth = saved_depth;
                    r?;
                    continue;
                }
                self.backup2(delim);
            }
            match self.text_or_action()? {
                Parsed::End => return self.errorf("unexpected {{end}}".into()),
                Parsed::Else(_) => return self.errorf("unexpected {{else}}".into()),
                Parsed::Node(n) => root.nodes.push(n),
            }
        }
        let tree = self.new_tree(name.to_string(), root);
        self.add(tree)
    }

    fn parse_definition(&mut self) -> PResult<()> {
        let context = "define clause";
        let name_tok = self.expect_one_of(ItemType::String, ItemType::RawString, context)?;
        let name = match quote::unquote(&name_tok.val) {
            Ok(n) => n,
            Err(e) => return self.errorf(e.into()),
        };
        self.expect(ItemType::RightDelim, context)?;
        let (root, end) = self.item_list()?;
        if !matches!(end, Parsed::End) {
            let s = parsed_string(&end);
            return self.errorf(format!("unexpected {s} in {context}"));
        }
        let tree = self.new_tree(name, root);
        self.add(tree)
    }

    fn item_list(&mut self) -> PResult<(ListNode, Parsed)> {
        let pos = self.peek_non_space().pos;
        let mut list = ListNode { pos, nodes: Vec::new() };
        while self.peek_non_space().typ != ItemType::Eof {
            match self.text_or_action()? {
                Parsed::Node(n) => list.nodes.push(n),
                other => return Ok((list, other)),
            }
        }
        self.errorf("unexpected EOF".into())
    }

    fn text_or_action(&mut self) -> PResult<Parsed> {
        let token = self.next_non_space();
        match token.typ {
            ItemType::Text => Ok(Parsed::Node(Node::Text { pos: token.pos, end: token.end })),
            ItemType::LeftDelim => {
                let saved = (self.action_line, self.action_pos);
                self.action_line = token.line;
                self.action_pos = token.pos;
                let r = self.action();
                // Go clears actionLine (deferred) after the action.
                self.action_line = 0;
                let _ = saved;
                r
            }
            _ => self.unexpected(&token, "input"),
        }
    }

    fn action(&mut self) -> PResult<Parsed> {
        let token = self.next_non_space();
        match token.typ {
            ItemType::Block => return self.block_control().map(Parsed::Node),
            ItemType::Break => return self.break_control(token.pos).map(Parsed::Node),
            ItemType::Continue => return self.continue_control(token.pos).map(Parsed::Node),
            ItemType::Else => return self.else_control(),
            ItemType::End => return self.end_control(),
            ItemType::If => return self.if_control().map(Parsed::Node),
            ItemType::Range => return self.range_control().map(Parsed::Node),
            ItemType::Template => return self.template_control().map(Parsed::Node),
            ItemType::With => return self.with_control().map(Parsed::Node),
            _ => {}
        }
        self.backup();
        let token = self.peek();
        let pipe = self.pipeline("command", ItemType::RightDelim)?;
        Ok(Parsed::Node(Node::Action(ActionNode { pos: token.pos, pipe })))
    }

    fn break_control(&mut self, pos: Pos) -> PResult<Node> {
        let token = self.next_non_space();
        if token.typ != ItemType::RightDelim {
            return self.unexpected(&token, "{{break}}");
        }
        if self.range_depth == 0 {
            return self.errorf("{{break}} outside {{range}}".into());
        }
        Ok(Node::Break { pos, span: pos..token.end })
    }

    fn continue_control(&mut self, pos: Pos) -> PResult<Node> {
        let token = self.next_non_space();
        if token.typ != ItemType::RightDelim {
            return self.unexpected(&token, "{{continue}}");
        }
        if self.range_depth == 0 {
            return self.errorf("{{continue}} outside {{range}}".into());
        }
        Ok(Node::Continue { pos, span: pos..token.end })
    }

    fn pipeline(&mut self, context: &str, end: ItemType) -> PResult<PipeNode> {
        let token = self.peek_non_space();
        let mut pipe = PipeNode { pos: token.pos, is_assign: false, decl: Vec::new(), cmds: Vec::new(), span: token.pos..token.pos };
        // decls:
        loop {
            let v = self.peek_non_space();
            if v.typ != ItemType::Variable {
                break;
            }
            self.next();
            let token_after_variable = self.peek();
            let next = self.peek_non_space();
            if next.typ == ItemType::Assign || next.typ == ItemType::Declare {
                pipe.is_assign = next.typ == ItemType::Assign;
                self.next_non_space();
                pipe.decl.push(new_variable(v.pos, &v.val));
                self.vars.push(v.val.clone());
                break;
            } else if next.typ == ItemType::Char && next.val == "," {
                self.next_non_space();
                pipe.decl.push(new_variable(v.pos, &v.val));
                self.vars.push(v.val.clone());
                if context == "range" && pipe.decl.len() < 2 {
                    match self.peek_non_space().typ {
                        ItemType::Variable | ItemType::RightDelim | ItemType::RightParen => continue,
                        _ => return self.errorf("range can only initialize variables".into()),
                    }
                }
                return self.errorf(format!("too many declarations in {context}"));
            } else if token_after_variable.typ == ItemType::Space {
                self.backup3(v, token_after_variable);
                break;
            } else {
                self.backup2(v);
                break;
            }
        }
        loop {
            let token = self.next_non_space();
            if token.typ == end {
                pipe.span.end = token.pos;
                self.check_pipeline(&pipe, context)?;
                return Ok(pipe);
            }
            match token.typ {
                ItemType::Bool
                | ItemType::CharConstant
                | ItemType::Complex
                | ItemType::Dot
                | ItemType::Field
                | ItemType::Identifier
                | ItemType::Number
                | ItemType::Nil
                | ItemType::RawString
                | ItemType::String
                | ItemType::Variable
                | ItemType::LeftParen => {
                    self.backup();
                    let cmd = self.command()?;
                    pipe.cmds.push(cmd);
                }
                _ => return self.unexpected(&token, context),
            }
        }
    }

    fn check_pipeline(&mut self, pipe: &PipeNode, context: &str) -> PResult<()> {
        if pipe.cmds.is_empty() {
            return self.errorf(format!("missing value for {context}"));
        }
        for (i, c) in pipe.cmds.iter().enumerate().skip(1) {
            if matches!(c.args[0], Arg::Bool { .. } | Arg::Dot { .. } | Arg::Nil { .. } | Arg::Number(_) | Arg::String { .. }) {
                let span = c.span.clone();
                return self.fail(format!("non executable command in pipeline stage {}", i + 1), span);
            }
        }
        Ok(())
    }

    fn parse_control(&mut self, context: &str) -> PResult<BranchNode> {
        if self.nesting >= MAX_NESTING {
            return self.errorf("max nesting depth exceeded".into());
        }
        self.nesting += 1;
        let nvars = self.vars.len();
        let r = self.parse_control_inner(context);
        self.vars.truncate(nvars);
        self.nesting -= 1;
        r
    }

    fn parse_control_inner(&mut self, context: &str) -> PResult<BranchNode> {
        let pipe = self.pipeline(context, ItemType::RightDelim)?;
        if context == "range" {
            self.range_depth += 1;
        }
        let r = self.item_list();
        if context == "range" {
            self.range_depth -= 1;
        }
        let (list, next) = r?;
        let mut else_list = None;
        match next {
            Parsed::End => {}
            Parsed::Else(epos) => {
                if context == "if" && self.peek().typ == ItemType::If {
                    self.next();
                    let n = self.if_control()?;
                    else_list = Some(ListNode { pos: epos, nodes: vec![n] });
                } else if context == "with" && self.peek().typ == ItemType::With {
                    self.next();
                    let n = self.with_control()?;
                    else_list = Some(ListNode { pos: epos, nodes: vec![n] });
                } else {
                    let (el, next) = self.item_list()?;
                    if !matches!(next, Parsed::End) {
                        let s = parsed_string(&next);
                        return self.errorf(format!("expected end; found {s}"));
                    }
                    else_list = Some(el);
                }
            }
            Parsed::Node(_) => unreachable!(),
        }
        Ok(BranchNode { pos: pipe.pos, pipe, list, else_list })
    }

    fn if_control(&mut self) -> PResult<Node> {
        self.parse_control("if").map(Node::If)
    }

    fn range_control(&mut self) -> PResult<Node> {
        self.parse_control("range").map(Node::Range)
    }

    fn with_control(&mut self) -> PResult<Node> {
        self.parse_control("with").map(Node::With)
    }

    fn end_control(&mut self) -> PResult<Parsed> {
        self.expect(ItemType::RightDelim, "end")?;
        Ok(Parsed::End)
    }

    fn else_control(&mut self) -> PResult<Parsed> {
        let peek = self.peek_non_space();
        if peek.typ == ItemType::If || peek.typ == ItemType::With {
            return Ok(Parsed::Else(peek.pos));
        }
        let token = self.expect(ItemType::RightDelim, "else")?;
        Ok(Parsed::Else(token.pos))
    }

    fn block_control(&mut self) -> PResult<Node> {
        let context = "block clause";
        let token = self.next_non_space();
        let name = self.parse_template_name(&token, context)?;
        let pipe = self.pipeline(context, ItemType::RightDelim)?;

        let saved_vars = core::mem::replace(&mut self.vars, vec![String::from("$")]);
        let saved_range = core::mem::replace(&mut self.range_depth, 0);
        let r = self.item_list();
        self.vars = saved_vars;
        self.range_depth = saved_range;
        let (root, end) = r?;
        if !matches!(end, Parsed::End) {
            let s = parsed_string(&end);
            return self.errorf(format!("unexpected {s} in {context}"));
        }
        let tree = self.new_tree(name.clone(), root);
        self.add(tree)?;
        Ok(Node::Template(TemplateNode { pos: token.pos, name, pipe: Some(pipe), span: token.pos..token.end }))
    }

    fn template_control(&mut self) -> PResult<Node> {
        let context = "template clause";
        let token = self.next_non_space();
        let name = self.parse_template_name(&token, context)?;
        let mut pipe = None;
        if self.next_non_space().typ != ItemType::RightDelim {
            self.backup();
            pipe = Some(self.pipeline(context, ItemType::RightDelim)?);
        }
        Ok(Node::Template(TemplateNode { pos: token.pos, name, pipe, span: token.pos..token.end }))
    }

    fn parse_template_name(&mut self, token: &Item, context: &str) -> PResult<String> {
        match token.typ {
            ItemType::String | ItemType::RawString => match quote::unquote(&token.val) {
                Ok(s) => Ok(s),
                Err(e) => self.errorf(e.into()),
            },
            _ => self.unexpected(token, context),
        }
    }

    fn command(&mut self) -> PResult<CommandNode> {
        let pos = self.peek_non_space().pos;
        let mut cmd = CommandNode { pos, args: Vec::new(), span: pos..pos };
        loop {
            self.peek_non_space();
            if let Some(op) = self.operand()? {
                cmd.span.end = op.span().end;
                cmd.args.push(op);
            }
            let token = self.next();
            match token.typ {
                ItemType::Space => continue,
                ItemType::RightDelim | ItemType::RightParen => self.backup(),
                ItemType::Pipe => {}
                _ => return self.unexpected(&token, "operand"),
            }
            break;
        }
        if cmd.args.is_empty() {
            return self.errorf("empty command".into());
        }
        Ok(cmd)
    }

    fn operand(&mut self) -> PResult<Option<Arg>> {
        let Some(node) = self.term()? else { return Ok(None) };
        if self.peek().typ == ItemType::Field {
            let chain_pos = self.peek().pos;
            let mut fields = Vec::new();
            let mut end = node.span().end;
            while self.peek().typ == ItemType::Field {
                let t = self.next();
                end = t.end;
                fields.push(Ident { name: Rc::from(&t.val[1..]), start: t.pos });
            }
            let start = node.span().start;
            let node = match node {
                Arg::Field(mut f) => {
                    f.ident.extend(fields);
                    Arg::Field(FieldNode { pos: chain_pos, ident: f.ident, span: start..end })
                }
                Arg::Variable(mut v) => {
                    v.ident.extend(fields);
                    Arg::Variable(VariableNode { pos: chain_pos, ident: v.ident, span: start..end })
                }
                Arg::Bool { .. } | Arg::String { .. } | Arg::Number(_) | Arg::Nil { .. } | Arg::Dot { .. } => {
                    let s = node.to_string();
                    return self.errorf(format!("unexpected . after term {}", quote::go_quote(&s)));
                }
                other => Arg::Chain(Box::new(ChainNode { pos: chain_pos, node: other, field: fields, span: start..end })),
            };
            return Ok(Some(node));
        }
        Ok(Some(node))
    }

    fn term(&mut self) -> PResult<Option<Arg>> {
        let token = self.next_non_space();
        let span = token.pos..token.end;
        let arg = match token.typ {
            ItemType::Identifier => {
                if !(self.has_function)(&token.val) {
                    return self.fail(format!("function {} not defined", quote::go_quote(&token.val)), span);
                }
                Arg::Identifier { pos: token.pos, name: Rc::from(token.val.as_str()), span }
            }
            ItemType::Dot => Arg::Dot { pos: token.pos },
            ItemType::Nil => Arg::Nil { pos: token.pos },
            ItemType::Variable => self.use_var(token.pos, &token.val, span)?,
            ItemType::Field => Arg::Field(FieldNode {
                pos: token.pos,
                ident: split_idents(token.pos, &token.val[1..], 1),
                span,
            }),
            ItemType::Bool => Arg::Bool { pos: token.pos, val: token.val == "true" },
            ItemType::CharConstant | ItemType::Complex | ItemType::Number => match new_number(&token) {
                Ok(n) => Arg::Number(Box::new(n)),
                Err(msg) => return self.fail(msg, span),
            },
            ItemType::LeftParen => {
                if self.stack_depth >= MAX_STACK_DEPTH {
                    return self.errorf("max expression depth exceeded".into());
                }
                self.stack_depth += 1;
                let r = self.pipeline("parenthesized pipeline", ItemType::RightParen);
                self.stack_depth -= 1;
                let mut p = r?;
                p.span = token.pos..self.token[0].end.max(p.span.end);
                Arg::Pipe(Box::new(p))
            }
            ItemType::String | ItemType::RawString => match quote::unquote(&token.val) {
                Ok(s) => Arg::String { pos: token.pos, quoted: Rc::from(token.val.as_str()), text: Rc::from(s) },
                Err(e) => return self.fail(e.into(), span),
            },
            _ => {
                self.backup();
                return Ok(None);
            }
        };
        Ok(Some(arg))
    }

    fn use_var(&mut self, pos: Pos, name: &str, span: Span) -> PResult<Arg> {
        let v = new_variable(pos, name);
        let first = &v.ident[0].name;
        if self.vars.iter().any(|n| **n == **first) {
            return Ok(Arg::Variable(VariableNode { span, ..v }));
        }
        let msg = format!("undefined variable {}", quote::go_quote(first));
        self.fail(msg, span)
    }
}

fn parsed_string(p: &Parsed) -> &'static str {
    match p {
        Parsed::End => "{{end}}",
        Parsed::Else(_) => "{{else}}",
        Parsed::Node(_) => "node",
    }
}

fn split_idents(start: usize, s: &str, prefix_len: usize) -> Vec<Ident> {
    let mut out = Vec::new();
    let mut off = start;
    for part in s.split('.') {
        out.push(Ident { name: Rc::from(part), start: off });
        off += part.len() + prefix_len;
    }
    out
}

fn new_variable(pos: Pos, ident: &str) -> VariableNode {
    // "$x.a.b": ident[0] = "$x" at pos, then fields.
    let mut out = Vec::new();
    let mut off = pos;
    for (i, part) in ident.split('.').enumerate() {
        let start = if i == 0 { off } else { off };
        out.push(Ident { name: Rc::from(part), start });
        off += part.len() + 1;
    }
    VariableNode { pos, ident: out, span: pos..pos + ident.len() }
}

/// Go's `Tree.newNumber`.
fn new_number(token: &Item) -> Result<NumberNode, String> {
    let text = token.val.as_str();
    let mut n = NumberNode {
        pos: token.pos,
        is_int: false,
        is_uint: false,
        is_float: false,
        is_complex: false,
        int64: 0,
        uint64: 0,
        float64: 0.0,
        complex: (0.0, 0.0),
        text: Rc::from(text),
        span: token.pos..token.end,
    };
    match token.typ {
        ItemType::CharConstant => {
            let inner = &text[1..];
            let (c, tail) = quote::unquote_char(inner, b'\'').map_err(String::from)?;
            if tail != "'" {
                return Err(format!("malformed character constant: {text}"));
            }
            let r = match c {
                UnquotedChar::Char(ch) => ch as u32,
                UnquotedChar::Byte(b) => b as u32,
            };
            n.int64 = r as i64;
            n.is_int = true;
            n.uint64 = r as u64;
            n.is_uint = true;
            n.float64 = r as f64;
            n.is_float = true;
            return Ok(n);
        }
        ItemType::Complex => {
            // "a+bi" / "a-bi"
            let body = &text[..text.len() - 1];
            let split = body
                .char_indices()
                .skip(1)
                .filter(|&(i, c)| (c == '+' || c == '-') && !matches!(body.as_bytes()[i - 1], b'e' | b'E' | b'p' | b'P'))
                .map(|(i, _)| i)
                .last()
                .ok_or_else(|| String::from("expected integer"))?;
            let re = num::parse_float(&body[..split]).map_err(|_| String::from("expected integer"))?;
            let im = num::parse_float(&body[split..]).map_err(|_| String::from("expected integer"))?;
            n.is_complex = true;
            n.complex = (re, im);
            simplify_complex(&mut n);
            return Ok(n);
        }
        _ => {}
    }
    if let Some(stripped) = text.strip_suffix('i')
        && let Ok(f) = num::parse_float(stripped)
    {
        n.is_complex = true;
        n.complex = (0.0, f);
        simplify_complex(&mut n);
        return Ok(n);
    }
    let u = num::parse_uint0(text);
    if let Ok(u) = u {
        n.is_uint = true;
        n.uint64 = u;
    }
    if let Ok(i) = num::parse_int0(text) {
        n.is_int = true;
        n.int64 = i;
        if i == 0 {
            n.is_uint = true;
            n.uint64 = u.unwrap_or(0);
        }
    }
    if n.is_int {
        n.is_float = true;
        n.float64 = n.int64 as f64;
    } else if n.is_uint {
        n.is_float = true;
        n.float64 = n.uint64 as f64;
    } else if let Ok(f) = num::parse_float(text) {
        if !text.contains(['.', 'e', 'E', 'p', 'P']) {
            return Err(format!("integer overflow: {}", quote::go_quote(text)));
        }
        n.is_float = true;
        n.float64 = f;
        if !n.is_int && f_as_int(f).is_some() {
            n.is_int = true;
            n.int64 = f as i64;
        }
        if !n.is_uint && f_as_uint(f).is_some() {
            n.is_uint = true;
            n.uint64 = f as u64;
        }
    } else if num::parse_float(text) == Err(NumErr::Range) {
        // Fallthrough to illegal syntax, as in Go.
    }
    if !n.is_int && !n.is_uint && !n.is_float {
        return Err(format!("illegal number syntax: {}", quote::go_quote(text)));
    }
    Ok(n)
}

fn simplify_complex(n: &mut NumberNode) {
    n.is_float = n.complex.1 == 0.0;
    if n.is_float {
        let f = n.complex.0;
        n.float64 = f;
        n.is_int = f_as_int(f).is_some();
        if n.is_int {
            n.int64 = f as i64;
        }
        n.is_uint = f_as_uint(f).is_some();
        if n.is_uint {
            n.uint64 = f as u64;
        }
    }
}

/// `float64(int64(f)) == f`, for values in int64 range.
fn f_as_int(f: f64) -> Option<i64> {
    if (-9.223_372_036_854_775_808e18..9.223_372_036_854_775_808e18).contains(&f) && (f as i64) as f64 == f {
        Some(f as i64)
    } else {
        None
    }
}

/// `float64(uint64(f)) == f`, for values in uint64 range.
fn f_as_uint(f: f64) -> Option<u64> {
    if (0.0..1.844_674_407_370_955_161_6e19).contains(&f) && (f as u64) as f64 == f {
        Some(f as u64)
    } else {
        None
    }
}
