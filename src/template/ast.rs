//! Parse tree, mirroring Go's `text/template/parse/node.go`.
//!
//! Every node keeps Go's `Pos` (byte offset used for Go-compatible error
//! messages) and a `span` (byte range of the whole construct) used for our
//! own diagnostics.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

pub(crate) type Pos = usize;
pub(crate) type Span = Range<usize>;

/// A parsed template.
#[derive(Debug)]
pub struct Tree {
    /// Template name.
    pub name: String,
    /// Name of the top-level template being parsed (for error messages).
    pub parse_name: String,
    /// Full text that was parsed (shared by all trees from one parse).
    pub text: Rc<str>,
    pub root: ListNode,
}

#[derive(Debug, Clone)]
pub struct ListNode {
    pub pos: Pos,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone)]
pub enum Node {
    Text { pos: Pos, end: usize },
    Action(ActionNode),
    If(BranchNode),
    Range(BranchNode),
    With(BranchNode),
    Template(TemplateNode),
    Break { pos: Pos, span: Span },
    Continue { pos: Pos, span: Span },
}

impl Node {
    pub fn pos(&self) -> Pos {
        match self {
            Node::Text { pos, .. } | Node::Break { pos, .. } | Node::Continue { pos, .. } => *pos,
            Node::Action(a) => a.pos,
            Node::If(b) | Node::Range(b) | Node::With(b) => b.pos,
            Node::Template(t) => t.pos,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActionNode {
    pub pos: Pos,
    pub pipe: PipeNode,
}

#[derive(Debug, Clone)]
pub struct BranchNode {
    pub pos: Pos,
    pub pipe: PipeNode,
    pub list: ListNode,
    pub else_list: Option<ListNode>,
}

#[derive(Debug, Clone)]
pub struct TemplateNode {
    pub pos: Pos,
    pub name: String,
    pub pipe: Option<PipeNode>,
    /// Span of the quoted name.
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct PipeNode {
    pub pos: Pos,
    pub is_assign: bool,
    pub decl: Vec<VariableNode>,
    pub cmds: Vec<CommandNode>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct CommandNode {
    pub pos: Pos,
    pub args: Vec<Arg>,
    pub span: Span,
}

/// One `.name` segment of a field chain, with its byte offset.
#[derive(Debug, Clone)]
pub struct Ident {
    pub name: Rc<str>,
    /// Offset of the segment (its leading `.` or `$`).
    pub start: usize,
}

#[derive(Debug, Clone)]
pub struct FieldNode {
    pub pos: Pos,
    pub ident: Vec<Ident>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct VariableNode {
    pub pos: Pos,
    /// `ident[0]` is the variable name (`$x`), the rest are fields.
    pub ident: Vec<Ident>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ChainNode {
    pub pos: Pos,
    pub node: Arg,
    pub field: Vec<Ident>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct NumberNode {
    pub pos: Pos,
    pub is_int: bool,
    pub is_uint: bool,
    pub is_float: bool,
    pub is_complex: bool,
    pub int64: i64,
    pub uint64: u64,
    pub float64: f64,
    pub complex: (f64, f64),
    pub text: Rc<str>,
    pub span: Span,
}

/// An operand: an element of a command.
#[derive(Debug, Clone)]
pub enum Arg {
    Field(FieldNode),
    Chain(Box<ChainNode>),
    Identifier { pos: Pos, name: Rc<str>, span: Span },
    Pipe(Box<PipeNode>),
    Variable(VariableNode),
    Dot { pos: Pos },
    Nil { pos: Pos },
    Bool { pos: Pos, val: bool },
    Number(Box<NumberNode>),
    String { pos: Pos, quoted: Rc<str>, text: Rc<str> },
}

impl Arg {
    pub fn pos(&self) -> Pos {
        match self {
            Arg::Field(f) => f.pos,
            Arg::Chain(c) => c.pos,
            Arg::Identifier { pos, .. }
            | Arg::Dot { pos }
            | Arg::Nil { pos }
            | Arg::Bool { pos, .. }
            | Arg::String { pos, .. } => *pos,
            Arg::Pipe(p) => p.pos,
            Arg::Variable(v) => v.pos,
            Arg::Number(n) => n.pos,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Arg::Field(f) => f.span.clone(),
            Arg::Chain(c) => c.span.clone(),
            Arg::Identifier { span, .. } => span.clone(),
            Arg::Pipe(p) => p.span.clone(),
            Arg::Variable(v) => v.span.clone(),
            Arg::Dot { pos } | Arg::Nil { pos } => *pos..*pos + if matches!(self, Arg::Dot { .. }) { 1 } else { 3 },
            Arg::Bool { pos, val } => *pos..*pos + if *val { 4 } else { 5 },
            Arg::Number(n) => n.span.clone(),
            Arg::String { pos, quoted, .. } => *pos..*pos + quoted.len(),
        }
    }
}

// ---- Go-compatible String() renderings, used in error contexts. ----

impl fmt::Display for Arg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Arg::Field(n) => {
                for id in &n.ident {
                    write!(f, ".{}", id.name)?;
                }
                Ok(())
            }
            Arg::Chain(c) => {
                if let Arg::Pipe(p) = &c.node {
                    write!(f, "({p})")?;
                } else {
                    write!(f, "{}", c.node)?;
                }
                for id in &c.field {
                    write!(f, ".{}", id.name)?;
                }
                Ok(())
            }
            Arg::Identifier { name, .. } => f.write_str(name),
            Arg::Pipe(p) => write!(f, "{p}"),
            Arg::Variable(v) => {
                for (i, id) in v.ident.iter().enumerate() {
                    if i > 0 {
                        f.write_str(".")?;
                    }
                    f.write_str(&id.name)?;
                }
                Ok(())
            }
            Arg::Dot { .. } => f.write_str("."),
            Arg::Nil { .. } => f.write_str("nil"),
            Arg::Bool { val, .. } => f.write_str(if *val { "true" } else { "false" }),
            Arg::Number(n) => f.write_str(&n.text),
            Arg::String { quoted, .. } => f.write_str(quoted),
        }
    }
}

impl fmt::Display for VariableNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, id) in self.ident.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            f.write_str(&id.name)?;
        }
        Ok(())
    }
}

impl fmt::Display for CommandNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, arg) in self.args.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            if let Arg::Pipe(p) = arg {
                write!(f, "({p})")?;
            } else {
                write!(f, "{arg}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for PipeNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.decl.is_empty() {
            for (i, v) in self.decl.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{v}")?;
            }
            f.write_str(if self.is_assign { " = " } else { " := " })?;
        }
        for (i, c) in self.cmds.iter().enumerate() {
            if i > 0 {
                f.write_str(" | ")?;
            }
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

/// Renders a node like Go's `Node.String()`; needs the source for text nodes.
pub(crate) struct NodeDisplay<'a> {
    pub node: &'a Node,
    pub text: &'a str,
}

impl fmt::Display for NodeDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.text;
        match self.node {
            Node::Text { pos, end } => f.write_str(&text[*pos..*end]),
            Node::Action(a) => write!(f, "{{{{{}}}}}", a.pipe),
            Node::If(b) => write_branch(f, "if", b, text),
            Node::Range(b) => write_branch(f, "range", b, text),
            Node::With(b) => write_branch(f, "with", b, text),
            Node::Template(t) => {
                write!(f, "{{{{template {}", super::quote::go_quote(&t.name))?;
                if let Some(p) = &t.pipe {
                    write!(f, " {p}")?;
                }
                f.write_str("}}")
            }
            Node::Break { .. } => f.write_str("{{break}}"),
            Node::Continue { .. } => f.write_str("{{continue}}"),
        }
    }
}

fn write_list(f: &mut fmt::Formatter<'_>, l: &ListNode, text: &str) -> fmt::Result {
    for n in &l.nodes {
        write!(f, "{}", NodeDisplay { node: n, text })?;
    }
    Ok(())
}

fn write_branch(f: &mut fmt::Formatter<'_>, name: &str, b: &BranchNode, text: &str) -> fmt::Result {
    write!(f, "{{{{{name} {}}}}}", b.pipe)?;
    write_list(f, &b.list, text)?;
    if let Some(e) = &b.else_list {
        f.write_str("{{else}}")?;
        write_list(f, e, text)?;
    }
    f.write_str("{{end}}")
}

/// Go's `parse.IsEmptyTree`.
pub(crate) fn is_empty_list(l: &ListNode, text: &str) -> bool {
    l.nodes.iter().all(|n| match n {
        Node::Text { pos, end } => text[*pos..*end].trim_matches(is_go_space).is_empty(),
        _ => false,
    })
}

/// `bytes.TrimSpace` whitespace (Unicode White_Space).
fn is_go_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{85}' | '\u{a0}') || (c as u32 > 0xff && c.is_whitespace())
}
