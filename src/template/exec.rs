//! Executor: a port of Go's `text/template/exec.go` (and the builtins in
//! `funcs.go`), extended with Helm's `include`/`tpl`/`required`/`fail`.
//!
//! Go distinguishes three states of a `reflect.Value` that are observable
//! from templates; [`Ev`] models them:
//! - `Invalid`: no value at all (e.g. the result of a pipeline that produced
//!   nil, or `.` when a template is invoked without data). Field access on it
//!   yields nothing, silently.
//! - `Iface(v)`: a value whose static type is `interface{}` — map entries and
//!   list elements. `Iface(Nil)` is a nil interface: field access on it is
//!   the classic "nil pointer evaluating interface {}.x" error.
//! - `Val(v)`: a value with a concrete static type.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use super::ast::*;
use super::quote::go_quote;
use super::{GoErr, Set, TemplateFile};
use crate::error::{Error, ErrorKind, Frame, Location};
use crate::func::{CallCtx, Env, Func, FuncError, FuncMap, Ty};
use crate::gofmt;
use crate::value::{Kind, Map, MapKind, MapRef, Value};

/// An evaluated value with its Go static-type state.
#[derive(Clone, Debug)]
pub(crate) enum Ev {
    Invalid,
    Iface(Value),
    Val(Value),
}

impl Ev {
    fn of(v: Value) -> Ev {
        if v.is_nil() { Ev::Invalid } else { Ev::Val(v) }
    }

    /// `indirectInterface`.
    fn indirect_iface(self) -> Ev {
        match self {
            Ev::Iface(Value::Nil) => Ev::Invalid,
            Ev::Iface(v) => Ev::Val(v),
            other => other,
        }
    }

    /// The underlying value (nil for Invalid).
    fn value(&self) -> Value {
        match self {
            Ev::Invalid => Value::Nil,
            Ev::Iface(v) | Ev::Val(v) => v.clone(),
        }
    }

    fn is_valid(&self) -> bool {
        !matches!(self, Ev::Invalid)
    }

    /// Go's `Type()` string for error messages.
    fn type_string(&self) -> String {
        match self {
            Ev::Invalid => "<nil>".into(),
            Ev::Iface(_) => "interface {}".into(),
            Ev::Val(v) => v.type_name(),
        }
    }
}

/// `missingVal`: the absent final pipeline argument.
type Final = Option<Ev>;

/// Control flow out of `walk`.
pub(crate) enum Flow {
    Fail(Box<Fail>),
    Break,
    Continue,
}

/// An execution error: Go's error chain (for Helm-identical messages) plus
/// our rich diagnostic.
pub(crate) struct Fail {
    pub go: GoErr,
    pub diag: Error,
}

type R<T> = Result<T, Flow>;

fn fail_flow(go: GoErr, diag: Error) -> Flow {
    Flow::Fail(Box::new(Fail { go, diag }))
}

/// Shared, per-render execution context.
pub(crate) struct Ctx<'e> {
    pub funcs: &'e FuncMap,
    pub env: &'e mut dyn Env,
    pub strict: bool,
    pub lint_mode: bool,
    pub enable_dns: bool,
    /// Helm's `includedNames` recursion counters.
    pub included: BTreeMap<String, usize>,
    /// Native nesting depth of template invocations (stack guard).
    pub depth: usize,
    pub max_depth: usize,
}

/// The node Go would report an error at ("s.at(node)"): its position and
/// kind. The node's text and span are only computed if an error occurs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct At {
    pos: usize,
    kind: AtKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtKind {
    Action,
    Range,
    Template,
    Pipe,
    Command,
    Field,
    Chain,
    Identifier,
    Variable,
    Dot,
    Nil,
    Bool,
    Number,
    String,
}

fn arg_kind(a: &Arg) -> AtKind {
    match a {
        Arg::Field(_) => AtKind::Field,
        Arg::Chain(_) => AtKind::Chain,
        Arg::Identifier { .. } => AtKind::Identifier,
        Arg::Pipe(_) => AtKind::Pipe,
        Arg::Variable(_) => AtKind::Variable,
        Arg::Dot { .. } => AtKind::Dot,
        Arg::Nil { .. } => AtKind::Nil,
        Arg::Bool { .. } => AtKind::Bool,
        Arg::Number(_) => AtKind::Number,
        Arg::String { .. } => AtKind::String,
    }
}

/// Finds the node identified by `at` in `list` and returns Go's `String()`
/// of it and its span.
fn find_node(list: &ListNode, at: At, text: &str) -> Option<(String, Span)> {
    for n in &list.nodes {
        let found = match n {
            Node::Text { .. } | Node::Break { .. } | Node::Continue { .. } => None,
            Node::Action(a) => {
                if at.kind == AtKind::Action && a.pos == at.pos {
                    return Some((format!("{{{{{}}}}}", a.pipe), a.pipe.span.clone()));
                }
                find_in_pipe(&a.pipe, at)
            }
            Node::If(b) | Node::With(b) => find_in_pipe(&b.pipe, at)
                .or_else(|| find_node(&b.list, at, text))
                .or_else(|| b.else_list.as_ref().and_then(|l| find_node(l, at, text))),
            Node::Range(b) => {
                if at.kind == AtKind::Range && b.pos == at.pos {
                    return Some((
                        NodeDisplay { node: n, text }.to_string(),
                        b.pipe.span.clone(),
                    ));
                }
                find_in_pipe(&b.pipe, at)
                    .or_else(|| find_node(&b.list, at, text))
                    .or_else(|| b.else_list.as_ref().and_then(|l| find_node(l, at, text)))
            }
            Node::Template(t) => {
                if at.kind == AtKind::Template && t.pos == at.pos {
                    return Some((NodeDisplay { node: n, text }.to_string(), t.span.clone()));
                }
                t.pipe.as_ref().and_then(|p| find_in_pipe(p, at))
            }
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

fn find_in_pipe(p: &PipeNode, at: At) -> Option<(String, Span)> {
    if at.kind == AtKind::Pipe && p.pos == at.pos {
        return Some((p.to_string(), p.span.clone()));
    }
    for c in &p.cmds {
        if at.kind == AtKind::Command && c.pos == at.pos {
            return Some((c.to_string(), c.span.clone()));
        }
        for a in &c.args {
            if let Some(f) = find_in_arg(a, at) {
                return Some(f);
            }
        }
    }
    None
}

fn find_in_arg(a: &Arg, at: At) -> Option<(String, Span)> {
    if arg_kind(a) == at.kind && a.pos() == at.pos {
        return Some((a.to_string(), a.span()));
    }
    match a {
        Arg::Pipe(p) => find_in_pipe(p, at),
        Arg::Chain(c) => find_in_arg(&c.node, at),
        _ => None,
    }
}

struct Var {
    name: Rc<str>,
    value: Ev,
}

pub(crate) struct State<'s, 'e> {
    ctx: &'s mut Ctx<'e>,
    set: Rc<Set>,
    /// Name of the executing template (Go `s.tmpl.Name()`).
    name: Rc<str>,
    tree: Rc<Tree>,
    vars: Vec<Var>,
    at: Option<At>,
    /// Go's template-call depth for `{{template}}` (maxExecDepth).
    depth: usize,
}

/// Go's `maxExecDepth`.
const MAX_EXEC_DEPTH: usize = 100_000;
/// Helm's `recursionMaxNums`.
const RECURSION_MAX_NUMS: usize = 1000;

impl<'s, 'e> State<'s, 'e> {
    pub fn new(ctx: &'s mut Ctx<'e>, set: Rc<Set>, name: Rc<str>, tree: Rc<Tree>, dot: Ev) -> Self {
        State {
            ctx,
            set,
            name,
            tree,
            vars: vec![Var {
                name: Rc::from("$"),
                value: dot,
            }],
            at: None,
            depth: 0,
        }
    }

    // ---------------------------------------------------------------
    // Errors

    #[inline]
    fn set_at(&mut self, pos: usize, kind: AtKind) {
        self.at = Some(At { pos, kind });
    }

    #[inline]
    fn at_arg(&mut self, a: &Arg) {
        self.set_at(a.pos(), arg_kind(a));
    }

    /// Context string and span of the current node (error path only).
    fn at_info(&self, at: At) -> (String, Span) {
        find_node(&self.tree.root, at, &self.tree.text)
            .unwrap_or_else(|| (String::new(), at.pos..at.pos))
    }

    /// Go's `ErrorContext` location string `NAME:LINE:COL` (byte column,
    /// 0-based on the first line... exactly as Go computes it).
    fn go_location(&self, pos: usize) -> String {
        let text: &str = &self.tree.text;
        let pos = pos.min(text.len());
        let before = &text.as_bytes()[..pos];
        let line_num = 1 + before.iter().filter(|&&b| b == b'\n').count();
        let byte_num = match before.iter().rposition(|&b| b == b'\n') {
            None => pos,
            Some(i) => pos - (i + 1),
        };
        format!("{}:{}:{}", self.tree.parse_name, line_num, byte_num)
    }

    fn location(&self, span: Span) -> Location {
        Location::from_span(self.tree.parse_name.clone(), self.tree.text.clone(), span)
    }

    /// Go's `s.errorf`: the exec error prefix plus `msg`.
    fn go_prefix(&self) -> String {
        match &self.at {
            None => format!("template: {}: ", self.name),
            Some(at) => {
                let loc = self.go_location(at.pos);
                let (context, _) = self.at_info(*at);
                format!(
                    "template: {}: executing {} at <{}>: ",
                    loc,
                    go_quote(&self.name),
                    context
                )
            }
        }
    }

    #[inline(never)]
    fn errorf<T>(&self, msg: impl Into<String>) -> R<T> {
        Err(self.error(msg.into(), None))
    }

    #[cold]
    #[inline(never)]
    fn error(&self, msg: String, help: Option<String>) -> Flow {
        let go = GoErr::Exec {
            msg: format!("{}{}", self.go_prefix(), msg),
            wrapped: None,
        };
        let mut diag = Error::new(ErrorKind::Exec, msg);
        if let Some(at) = self.at {
            diag.location = Some(self.location(self.at_info(at).1));
        }
        diag.help = help;
        fail_flow(go, diag)
    }

    // ---------------------------------------------------------------
    // Variables

    fn mark(&self) -> usize {
        self.vars.len()
    }

    fn pop(&mut self, mark: usize) {
        self.vars.truncate(mark);
    }

    fn push(&mut self, name: Rc<str>, value: Ev) {
        self.vars.push(Var { name, value });
    }

    fn set_var(&mut self, name: &str, value: Ev) -> R<()> {
        for v in self.vars.iter_mut().rev() {
            if &*v.name == name {
                v.value = value;
                return Ok(());
            }
        }
        self.errorf(format!("undefined variable: {name}"))
    }

    fn set_top_var(&mut self, n: usize, value: Ev) {
        let len = self.vars.len();
        self.vars[len - n].value = value;
    }

    fn var_value(&self, name: &str) -> R<Ev> {
        for v in self.vars.iter().rev() {
            if &*v.name == name {
                return Ok(v.value.clone());
            }
        }
        self.errorf(format!("undefined variable: {name}"))
    }

    // ---------------------------------------------------------------
    // Walking

    pub fn walk_list(&mut self, dot: &Ev, list: &ListNode, out: &mut String) -> R<()> {
        for n in &list.nodes {
            self.walk(dot, n, out)?;
        }
        Ok(())
    }

    fn walk(&mut self, dot: &Ev, node: &Node, out: &mut String) -> R<()> {
        match node {
            Node::Text { pos, end } => {
                out.push_str(&self.tree.text[*pos..*end]);
                Ok(())
            }
            Node::Action(a) => {
                self.set_at(a.pos, AtKind::Action);
                let val = self.eval_pipeline(dot, &a.pipe)?;
                if a.pipe.decl.is_empty() {
                    self.print_value(&val, out);
                }
                Ok(())
            }
            Node::Break { .. } => Err(Flow::Break),
            Node::Continue { .. } => Err(Flow::Continue),
            Node::If(b) => self.walk_if_or_with(false, dot, b, out),
            Node::With(b) => self.walk_if_or_with(true, dot, b, out),
            Node::Range(b) => self.walk_range(dot, b, out),
            Node::Template(t) => self.walk_template(dot, t, out),
        }
    }

    fn walk_if_or_with(
        &mut self,
        is_with: bool,
        dot: &Ev,
        b: &BranchNode,
        out: &mut String,
    ) -> R<()> {
        let mark = self.mark();
        let r = (|| {
            let val = self.eval_pipeline(dot, &b.pipe)?;
            let truth = is_true(&val.clone().indirect_iface());
            if truth {
                if is_with {
                    self.walk_list(&val, &b.list, out)
                } else {
                    self.walk_list(dot, &b.list, out)
                }
            } else if let Some(el) = &b.else_list {
                self.walk_list(dot, el, out)
            } else {
                Ok(())
            }
        })();
        self.pop(mark);
        r
    }

    fn walk_range(&mut self, dot: &Ev, r: &BranchNode, out: &mut String) -> R<()> {
        self.set_at(r.pos, AtKind::Range);
        let mark0 = self.mark();
        let result = self.walk_range_inner(dot, r, out);
        self.pop(mark0);
        match result {
            Err(Flow::Break) => Ok(()),
            other => other,
        }
    }

    fn walk_range_inner(&mut self, dot: &Ev, r: &BranchNode, out: &mut String) -> R<()> {
        let mut val = self.eval_pipeline(dot, &r.pipe)?.indirect_iface();
        let mut concrete = false;
        if let Some(c) = coll_of(&val) {
            concrete = c.concrete;
            val = Ev::Val(c.data);
        }
        let mark = self.mark();
        let decl = &r.pipe.decl;
        let one = |s: &mut Self, index: Ev, elem: Ev, out: &mut String| -> R<()> {
            if !decl.is_empty() {
                if r.pipe.is_assign {
                    if decl.len() > 1 {
                        s.set_var(&decl[0].ident[0].name, index.clone())?;
                    } else {
                        s.set_var(&decl[0].ident[0].name, elem.clone())?;
                    }
                } else {
                    s.set_top_var(1, elem.clone());
                }
            }
            if decl.len() > 1 {
                if r.pipe.is_assign {
                    s.set_var(&decl[1].ident[0].name, elem.clone())?;
                } else {
                    s.set_top_var(2, index);
                }
            }
            let res = s.walk_list(&elem, &r.list, out);
            s.pop(mark);
            match res {
                Err(Flow::Continue) => Ok(()),
                other => other,
            }
        };
        let mut ran = false;
        match &val {
            Ev::Val(v @ (Value::Int(_) | Value::Int64(_) | Value::Uint64(_))) => {
                if decl.len() > 1 {
                    let msg = format!(
                        "can't use {} to iterate over more than one variable",
                        gofmt::format_value(v)
                    );
                    return self.errorf(msg);
                }
                let n: i128 = match v {
                    Value::Int(i) | Value::Int64(i) => *i as i128,
                    Value::Uint64(u) => *u as i128,
                    _ => 0,
                };
                let mut i: i128 = 0;
                while i < n {
                    ran = true;
                    let e = match v {
                        Value::Int(_) => Value::Int(i as i64),
                        Value::Int64(_) => Value::Int64(i as i64),
                        _ => Value::Uint64(i as u64),
                    };
                    one(self, Ev::Invalid, Ev::Val(e), out)?;
                    i += 1;
                }
            }
            Ev::Val(Value::List(l)) => {
                let l = l.clone();
                for (i, e) in l.iter().enumerate() {
                    ran = true;
                    one(
                        self,
                        Ev::Val(Value::Int(i as i64)),
                        elem(concrete, e.clone()),
                        out,
                    )?;
                }
            }
            Ev::Val(Value::Bytes(b)) => {
                let b = b.clone();
                for (i, e) in b.iter().enumerate() {
                    ran = true;
                    one(
                        self,
                        Ev::Val(Value::Int(i as i64)),
                        Ev::Val(Value::Int(*e as i64)),
                        out,
                    )?;
                }
            }
            Ev::Val(Value::Map(m)) => {
                let entries: Vec<(String, Value)> = m
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                for (k, v) in entries {
                    ran = true;
                    one(self, Ev::Val(Value::string(k)), elem(concrete, v), out)?;
                }
            }
            Ev::Invalid => {}
            Ev::Val(v) => {
                let msg = format!("range can't iterate over {}", gofmt::format_value(v));
                return self.errorf(msg);
            }
            Ev::Iface(_) => unreachable!("indirected"),
        }
        if !ran && let Some(el) = &r.else_list {
            return self.walk_list(dot, el, out);
        }
        Ok(())
    }

    fn walk_template(&mut self, dot: &Ev, t: &TemplateNode, out: &mut String) -> R<()> {
        self.set_at(t.pos, AtKind::Template);
        let Some(tmpl) = self.set.lookup(&t.name) else {
            let msg = format!("template {} not defined", go_quote(&t.name));
            return self.errorf(msg);
        };
        if self.depth == MAX_EXEC_DEPTH {
            return self.errorf(format!(
                "exceeded maximum template depth ({MAX_EXEC_DEPTH})"
            ));
        }
        let new_dot = match &t.pipe {
            Some(p) => self.eval_pipeline(dot, p)?,
            None => Ev::Invalid,
        };
        if self.ctx.depth >= self.ctx.max_depth {
            return self.errorf(format!(
                "exceeded maximum template depth ({})",
                self.ctx.max_depth
            ));
        }
        let call_site = self.location(t.span.clone());
        self.ctx.depth += 1;
        let mut sub = State {
            ctx: &mut *self.ctx,
            set: self.set.clone(),
            name: Rc::from(t.name.as_str()),
            tree: tmpl.clone(),
            vars: vec![Var {
                name: Rc::from("$"),
                value: new_dot.clone(),
            }],
            at: None,
            depth: self.depth + 1,
        };
        let r = sub.walk_list(&new_dot, &tmpl.root, out);
        self.ctx.depth -= 1;
        r.map_err(|f| match f {
            Flow::Fail(mut fail) => {
                fail.diag.frames.push(Frame {
                    location: call_site,
                    call: format!("template {}", go_quote(&t.name)),
                });
                Flow::Fail(fail)
            }
            other => other,
        })
    }

    // ---------------------------------------------------------------
    // Pipelines and commands

    fn eval_pipeline(&mut self, dot: &Ev, pipe: &PipeNode) -> R<Ev> {
        self.set_at(pipe.pos, AtKind::Pipe);
        let mut value: Final = None;
        for cmd in &pipe.cmds {
            let v = self.eval_command(dot, cmd, value.take())?;
            // Unwrap interface{} values.
            value = Some(match v {
                Ev::Iface(Value::Nil) => Ev::Invalid,
                Ev::Iface(v) => Ev::Val(v),
                other => other,
            });
        }
        let value = value.unwrap_or(Ev::Invalid);
        for var in &pipe.decl {
            if pipe.is_assign {
                self.set_var(&var.ident[0].name, value.clone())?;
            } else {
                self.push(var.ident[0].name.clone(), value.clone());
            }
        }
        Ok(value)
    }

    fn not_a_function(&mut self, args: &[Arg], fin: &Final) -> R<()> {
        if args.len() > 1 || fin.is_some() {
            return self.errorf(format!("can't give argument to non-function {}", args[0]));
        }
        Ok(())
    }

    fn eval_command(&mut self, dot: &Ev, cmd: &CommandNode, fin: Final) -> R<Ev> {
        let first = &cmd.args[0];
        match first {
            Arg::Field(f) => return self.eval_field_node(dot, f, &cmd.args, fin),
            Arg::Chain(c) => return self.eval_chain_node(dot, c, &cmd.args, fin),
            Arg::Identifier { pos, name, span } => {
                return self.eval_function(dot, *pos, name, span, Some(cmd), &cmd.args, fin);
            }
            Arg::Pipe(p) => {
                self.not_a_function(&cmd.args, &fin)?;
                return self.eval_pipeline(dot, p);
            }
            Arg::Variable(v) => return self.eval_variable_node(dot, v, &cmd.args, fin),
            _ => {}
        }
        self.at_arg(first);
        self.not_a_function(&cmd.args, &fin)?;
        match first {
            Arg::Bool { val, .. } => Ok(Ev::Val(Value::Bool(*val))),
            Arg::Dot { .. } => Ok(dot.clone()),
            Arg::Nil { .. } => self.errorf("nil is not a command"),
            Arg::Number(n) => self.ideal_constant(n),
            Arg::String { text, .. } => Ok(Ev::Val(Value::String(text.clone()))),
            _ => self.errorf(format!(
                "can't evaluate command {}",
                go_quote(&first.to_string())
            )),
        }
    }

    fn ideal_constant(&mut self, n: &NumberNode) -> R<Ev> {
        self.set_at(n.pos, AtKind::Number);
        let t: &str = &n.text;
        if n.is_complex {
            return self.errorf(format!("complex constant {t} is not supported"));
        }
        if n.is_float
            && !is_hex_int(t)
            && !t.starts_with('\'')
            && t.contains(['.', 'e', 'E', 'p', 'P'])
        {
            return Ok(Ev::Val(Value::Float(n.float64)));
        }
        if n.is_int {
            return Ok(Ev::Val(Value::Int(n.int64)));
        }
        if n.is_uint {
            return self.errorf(format!("{t} overflows int"));
        }
        Ok(Ev::Invalid)
    }

    fn eval_field_node(&mut self, dot: &Ev, f: &FieldNode, args: &[Arg], fin: Final) -> R<Ev> {
        self.set_at(f.pos, AtKind::Field);
        self.eval_field_chain(dot, dot.clone(), FieldCtx::Field(f), &f.ident, args, fin)
    }

    fn eval_chain_node(&mut self, dot: &Ev, c: &ChainNode, args: &[Arg], fin: Final) -> R<Ev> {
        self.set_at(c.pos, AtKind::Chain);
        if matches!(c.node, Arg::Nil { .. }) {
            return self.errorf(format!(
                "indirection through explicit nil in {}",
                Arg::Chain(Box::new(c.clone()))
            ));
        }
        let pipe = self.eval_arg(dot, None, &c.node)?;
        self.eval_field_chain(dot, pipe, FieldCtx::Chain(c), &c.field, args, fin)
    }

    fn eval_variable_node(
        &mut self,
        dot: &Ev,
        v: &VariableNode,
        args: &[Arg],
        fin: Final,
    ) -> R<Ev> {
        self.set_at(v.pos, AtKind::Variable);
        let value = self.var_value(&v.ident[0].name)?;
        if v.ident.len() == 1 {
            self.not_a_function(args, &fin)?;
            return Ok(value);
        }
        self.eval_field_chain(dot, value, FieldCtx::Var(v), &v.ident[1..], args, fin)
    }

    fn eval_field_chain(
        &mut self,
        dot: &Ev,
        mut receiver: Ev,
        node: FieldCtx<'_>,
        ident: &[Ident],
        args: &[Arg],
        fin: Final,
    ) -> R<Ev> {
        let n = ident.len();
        for i in 0..n - 1 {
            receiver = self.eval_field(dot, ident, i, &node, &[], None, receiver)?;
        }
        self.eval_field(dot, ident, n - 1, &node, args, fin, receiver)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_field(
        &mut self,
        dot: &Ev,
        ident: &[Ident],
        idx: usize,
        node: &FieldCtx<'_>,
        args: &[Arg],
        fin: Final,
        receiver: Ev,
    ) -> R<Ev> {
        let field_name: &str = &ident[idx].name;
        if !receiver.is_valid() {
            if self.ctx.strict {
                let msg = format!("nil data; no entry for key {}", go_quote(field_name));
                return Err(self.error(msg, None));
            }
            return Ok(Ev::Invalid);
        }
        let typ = receiver.type_string();
        let value = match &receiver {
            Ev::Iface(Value::Nil) => {
                let msg = format!("nil pointer evaluating {typ}.{field_name}");
                let help = Some(format!("`{}` is nil or missing", node.prefix(ident, idx)));
                return Err(self.error_at_segment(msg, help, node, ident, idx));
            }
            Ev::Iface(v) | Ev::Val(v) => v.clone(),
            Ev::Invalid => unreachable!(),
        };
        // Methods first.
        if let Value::Map(m) = &value
            && m.kind() == MapKind::HelmValues
            && let Some(meth) = ValuesMethod::from_name(field_name)
        {
            let call_args = if args.is_empty() { None } else { Some(args) };
            let name = field_name.to_string();
            return self.eval_call(
                dot,
                CallTarget::Values(m.clone(), meth),
                None,
                node.at(),
                &name,
                call_args,
                fin,
            );
        }
        if let Value::Object(o) = &value
            && let Some(sig) = o.method(field_name)
        {
            let o = o.clone();
            let name = field_name.to_string();
            let call_args = if args.is_empty() { None } else { Some(args) };
            return self.eval_call(
                dot,
                CallTarget::Method(&*o, sig, &name),
                None,
                node.at(),
                &name,
                call_args,
                fin,
            );
        }
        let has_args = args.len() > 1 || fin.is_some();
        match &value {
            Value::Object(o) => {
                if let Some(f) = o.field(field_name) {
                    if has_args {
                        return self.errorf(format!(
                            "{field_name} has arguments but cannot be invoked as function"
                        ));
                    }
                    return Ok(Ev::of(f));
                }
                if o.kind() == Kind::Map
                    && let Some(c) = collection(&**o)
                    && let Value::Map(m) = &c.data
                {
                    if has_args {
                        return self
                            .errorf(format!("{field_name} is not a method but has arguments"));
                    }
                    return Ok(match m.get(field_name) {
                        Some(v) => elem(c.concrete, v),
                        None => c.missing,
                    });
                }
            }
            Value::Map(m) => {
                if has_args {
                    return self.errorf(format!("{field_name} is not a method but has arguments"));
                }
                return match m.get(field_name) {
                    Some(v) => Ok(Ev::Iface(v)),
                    None => {
                        if self.ctx.strict {
                            let msg = format!("map has no entry for key {}", go_quote(field_name));
                            Err(self.error_at_segment(msg, None, node, ident, idx))
                        } else {
                            Ok(Ev::Iface(Value::Nil))
                        }
                    }
                };
            }
            _ => {}
        }
        let msg = format!("can't evaluate field {field_name} in type {typ}");
        let help = Some(format!(
            "`{}` is a {}, not a map or object",
            node.prefix(ident, idx),
            value.type_name()
        ));
        Err(self.error_at_segment(msg, help, node, ident, idx))
    }

    /// An error whose Go context is the current node, but whose diagnostic
    /// span is the failing `.field` segment.
    fn error_at_segment(
        &self,
        msg: String,
        help: Option<String>,
        node: &FieldCtx<'_>,
        ident: &[Ident],
        idx: usize,
    ) -> Flow {
        let mut f = self.error(msg, help);
        if let Flow::Fail(fail) = &mut f {
            let start = ident[idx].start;
            let end = start + 1 + ident[idx].name.len();
            let _ = node;
            fail.diag.location = Some(self.location(start..end));
        }
        f
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_function(
        &mut self,
        dot: &Ev,
        pos: usize,
        name: &Rc<str>,
        span: &Span,
        cmd: Option<&CommandNode>,
        args: &[Arg],
        fin: Final,
    ) -> R<Ev> {
        self.set_at(pos, AtKind::Identifier);
        let target = if let Some(f) = self.ctx.funcs.get(name) {
            CallTarget::Func(f.clone())
        } else if let Some(b) = Builtin::from_name(name) {
            CallTarget::Builtin(b)
        } else if let Some(h) = HelmFn::from_name(name) {
            CallTarget::Helm(h)
        } else {
            return self.errorf(format!("{} is not a defined function", go_quote(name)));
        };
        let at = match cmd {
            Some(c) => At {
                pos: c.pos,
                kind: AtKind::Command,
            },
            None => At {
                pos,
                kind: AtKind::Identifier,
            },
        };
        let _ = span;
        self.eval_call(dot, target, cmd, at, name, Some(args), fin)
    }

    /// Go's `evalCall`. `args` includes the function name/field node at [0].
    #[allow(clippy::too_many_arguments)]
    fn eval_call(
        &mut self,
        dot: &Ev,
        target: CallTarget<'_>,
        _cmd: Option<&CommandNode>,
        node_at: At,
        name: &str,
        args: Option<&[Arg]>,
        fin: Final,
    ) -> R<Ev> {
        let args: &[Arg] = match args {
            Some(a) if !a.is_empty() => &a[1..],
            _ => &[],
        };
        let sig = target.sig();
        let num_in_params = sig.fixed.len() + usize::from(sig.variadic.is_some());
        let num_in = args.len() + usize::from(fin.is_some());
        let mut num_fixed = args.len();
        if sig.variadic.is_some() {
            num_fixed = num_in_params - 1;
            if num_in < num_fixed {
                return Err(self.arity_error(name, true, num_in_params - 1, args.len()));
            }
        } else if num_in != num_in_params {
            return Err(self.arity_error(name, false, num_in_params, num_in));
        }

        if let CallTarget::Builtin(b @ (Builtin::And | Builtin::Or)) = target {
            let is_or = b == Builtin::Or;
            let mut v = Ev::Invalid;
            for arg in args {
                v = self.eval_arg(dot, Some(PTy::Rv), arg)?;
                if truth(&v) == is_or {
                    return Ok(v);
                }
            }
            if let Some(f) = fin {
                v = f;
            }
            return Ok(v);
        }

        let mut argv: Vec<Ev> = Vec::with_capacity(num_in);
        let mut i = 0;
        while i < num_fixed && i < args.len() {
            let ty = sig.param(i);
            argv.push(self.eval_arg(dot, Some(ty), &args[i])?);
            i += 1;
        }
        if let Some(vt) = sig.variadic {
            while i < args.len() {
                argv.push(self.eval_arg(dot, Some(vt), &args[i])?);
                i += 1;
            }
        }
        if let Some(f) = fin {
            let t = match sig.variadic {
                Some(vt) if num_in > num_fixed => vt,
                _ => sig.param(num_in - 1),
            };
            argv.push(self.validate_type(f, Some(t))?);
        }

        let result = if let CallTarget::Builtin(Builtin::Call) = target {
            let callee = match args.first() {
                Some(a) => a.to_string(),
                None => String::from("<invalid Value>"),
            };
            builtin_call(&callee, &argv)
        } else {
            self.dispatch(target, &argv, name)
        };
        match result {
            Ok(v) => Ok(v),
            Err(err) => {
                self.at = Some(node_at);
                let call_span = self.at_info(node_at).1;
                Err(self.call_error(name, err, call_span))
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn arity_error(&self, name: &str, variadic: bool, want: usize, got: usize) -> Flow {
        let msg = if variadic {
            format!("wrong number of args for {name}: want at least {want} got {got}")
        } else {
            format!("wrong number of args for {name}: want {want} got {got}")
        };
        self.error(msg, None)
    }

    #[inline(never)]
    fn dispatch(&mut self, target: CallTarget<'_>, argv: &[Ev], name: &str) -> Result<Ev, CallErr> {
        // Reflection-based functions see Go's named type; everything else
        // sees a map/slice-kind object (e.g. `VersionSet`) as its data, as
        // Go's reflect.Value.Len/Index/MapKeys would.
        let keeps_objects = matches!(name, "typeOf" | "typeIs" | "typeIsLike" | "dig")
            || name.starts_with("duration")
            || name == "mustToDuration";
        match target {
            CallTarget::Builtin(b) => self.call_builtin(b, argv),
            CallTarget::Func(f) => {
                let mut vals = values(argv);
                // Sprig's slice (reflect.Value.Slice) returns the input's slice type;
                // rest/initial build a new []interface{}.
                let keep_type: Option<&'static str> = if matches!(name, "slice" | "mustSlice") {
                    vals.first()
                        .and_then(|v| v.as_object())
                        .and_then(|o| o.as_any().downcast_ref::<crate::value::TypedList>())
                        .map(|t| t.go_type)
                } else {
                    None
                };
                let byte_func = matches!(
                    name,
                    "b64enc"
                        | "b32enc"
                        | "sha1sum"
                        | "sha256sum"
                        | "sha512sum"
                        | "adler32sum"
                        | "fromYaml"
                        | "fromYamlArray"
                );
                for v in &mut vals {
                    if let Value::Object(o) = v
                        && let Some(b) = o.as_any().downcast_ref::<crate::value::GoBytesString>()
                    {
                        *v = if byte_func {
                            Value::Bytes(b.0.clone())
                        } else {
                            o.to_data()
                        };
                    }
                }
                if !keeps_objects {
                    for v in &mut vals {
                        if let Value::Object(o) = v
                            && matches!(o.kind(), Kind::Map | Kind::Slice)
                        {
                            *v = o.to_data();
                        }
                    }
                }
                let mut cc = CallCtx {
                    env: &mut *self.ctx.env,
                };
                let r = (f.imp)(&mut cc, &vals);
                let r = match (r, keep_type) {
                    (Ok(Value::List(l)), Some(t)) => {
                        Ok(Value::Object(Rc::new(crate::value::TypedList {
                            go_type: t,
                            items: l,
                        })))
                    }
                    (r, _) => r,
                };
                r.map(Ev::of_result).map_err(CallErr::Plain)
            }
            CallTarget::Method(o, _, mname) => {
                let vals = values(argv);
                o.call_method(mname, &vals)
                    .map(Ev::of_result)
                    .map_err(CallErr::Plain)
            }
            CallTarget::Helm(h) => {
                let vals = values(argv);
                self.call_helm(h, &vals)
            }
            CallTarget::Values(m, meth) => meth.call(&m, &values(argv)).map_err(CallErr::Plain),
        }
    }

    /// `error calling NAME: %w`.
    #[cold]
    #[inline(never)]
    fn call_error(&self, name: &str, err: CallErr, call_span: Span) -> Flow {
        let prefix = self.go_prefix();
        match err {
            CallErr::Plain(e) => {
                let inner = GoErr::Plain {
                    msg: e.message.clone(),
                    wrapped: None,
                };
                let msg = format!("error calling {name}: {}", e.message);
                let go = GoErr::Exec {
                    msg: format!("{prefix}{msg}"),
                    wrapped: Some(Box::new(inner)),
                };
                let mut diag = Error::new(ErrorKind::Exec, msg);
                diag.location = Some(self.location(call_span));
                fail_flow(go, diag)
            }
            CallErr::Helm(he) => {
                let HelmErr {
                    go: inner,
                    mut diag,
                    frame,
                } = *he;
                let msg = format!("error calling {name}: {}", inner.message());
                let go = GoErr::Exec {
                    msg: format!("{prefix}{msg}"),
                    wrapped: Some(Box::new(inner)),
                };
                match frame {
                    Some(call) => diag.frames.push(Frame {
                        location: self.location(call_span),
                        call,
                    }),
                    None => {
                        if diag.location.is_none() {
                            diag.location = Some(self.location(call_span));
                        }
                    }
                }
                fail_flow(go, diag)
            }
        }
    }

    // ---------------------------------------------------------------
    // Arguments

    /// Go's `validateType`. `typ == None` is Go's nil type.
    fn validate_type(&self, value: Ev, typ: Option<PTy>) -> R<Ev> {
        let Some(typ) = typ else { return Ok(value) };
        let ty = match typ {
            PTy::Rv => return Ok(value),
            PTy::Ty(t) => t,
        };
        match value {
            Ev::Invalid => match ty {
                Ty::Any | Ty::Object(_) => Ok(Ev::Iface(Value::Nil)),
                Ty::Map => Ok(Ev::Val(Value::empty_map())),
                Ty::List | Ty::Strings => Ok(Ev::Val(Value::list(Vec::new()))),
                Ty::Bytes => Ok(Ev::Val(Value::Bytes(Rc::from(&[][..])))),
                _ => self.errorf(format!("invalid value; expected {}", ty.go_name())),
            },
            Ev::Iface(v) => {
                if ty == Ty::Any {
                    return Ok(Ev::Iface(v));
                }
                if v.is_nil() {
                    return self.errorf(format!(
                        "wrong type for value; expected {}; got interface {{}}",
                        ty.go_name()
                    ));
                }
                if assignable(&v, ty) {
                    return Ok(Ev::Val(to_param_type(v, ty)));
                }
                self.errorf(format!(
                    "wrong type for value; expected {}; got {}",
                    ty.go_name(),
                    v.type_name()
                ))
            }
            Ev::Val(v) => {
                if ty == Ty::Any || assignable(&v, ty) {
                    return Ok(Ev::Val(to_param_type(v, ty)));
                }
                self.errorf(format!(
                    "wrong type for value; expected {}; got {}",
                    ty.go_name(),
                    v.type_name()
                ))
            }
        }
    }

    fn eval_arg(&mut self, dot: &Ev, typ: Option<PTy>, n: &Arg) -> R<Ev> {
        self.at_arg(n);
        match n {
            Arg::Dot { .. } => return self.validate_type(dot.clone(), typ),
            Arg::Nil { .. } => {
                return match typ {
                    Some(PTy::Rv) => Ok(Ev::Invalid),
                    Some(PTy::Ty(Ty::Any | Ty::Object(_))) => Ok(Ev::Iface(Value::Nil)),
                    Some(PTy::Ty(Ty::Map)) => Ok(Ev::Val(Value::empty_map())),
                    Some(PTy::Ty(Ty::List | Ty::Strings)) => Ok(Ev::Val(Value::list(Vec::new()))),
                    Some(PTy::Ty(Ty::Bytes)) => Ok(Ev::Val(Value::Bytes(Rc::from(&[][..])))),
                    Some(PTy::Ty(t)) => {
                        self.errorf(format!("cannot assign nil to {}", t.go_name()))
                    }
                    None => self.errorf("cannot assign nil to <nil>"),
                };
            }
            Arg::Field(f) => {
                let v = self.eval_field_node(dot, f, core::slice::from_ref(n), None)?;
                return self.validate_type(v, typ);
            }
            Arg::Variable(v) => {
                let r = self.eval_variable_node(dot, v, &[], None)?;
                return self.validate_type(r, typ);
            }
            Arg::Pipe(p) => {
                let r = self.eval_pipeline(dot, p)?;
                return self.validate_type(r, typ);
            }
            Arg::Identifier { pos, name, span } => {
                let r = self.eval_function(
                    dot,
                    *pos,
                    name,
                    span,
                    None,
                    core::slice::from_ref(n),
                    None,
                )?;
                return self.validate_type(r, typ);
            }
            Arg::Chain(c) => {
                let r = self.eval_chain_node(dot, c, &[], None)?;
                return self.validate_type(r, typ);
            }
            _ => {}
        }
        let ty = match typ {
            Some(PTy::Rv) | Some(PTy::Ty(Ty::Any)) => return self.eval_empty_interface(dot, n),
            Some(PTy::Ty(t)) => t,
            None => return self.errorf(format!("can't handle {n} for arg of type <nil>")),
        };
        match ty {
            Ty::Bool => match n {
                Arg::Bool { val, .. } => Ok(Ev::Val(Value::Bool(*val))),
                _ => self.errorf(format!("expected bool; found {n}")),
            },
            Ty::Float64 => match n {
                Arg::Number(num) if num.is_float => Ok(Ev::Val(Value::Float(num.float64))),
                _ => self.errorf(format!("expected float; found {n}")),
            },
            Ty::Int | Ty::Int64 => match n {
                Arg::Number(num) if num.is_int => Ok(Ev::Val(if ty == Ty::Int {
                    Value::Int(num.int64)
                } else {
                    Value::Int64(num.int64)
                })),
                _ => self.errorf(format!("expected integer; found {n}")),
            },
            Ty::Uint64 | Ty::Uint32 => match n {
                Arg::Number(num) if num.is_uint => {
                    Ok(Ev::Val(Value::Uint64(if ty == Ty::Uint32 {
                        num.uint64 as u32 as u64
                    } else {
                        num.uint64
                    })))
                }
                _ => self.errorf(format!("expected unsigned integer; found {n}")),
            },
            Ty::String => match n {
                Arg::String { text, .. } => Ok(Ev::Val(Value::String(text.clone()))),
                _ => self.errorf(format!("expected string; found {n}")),
            },
            other => self.errorf(format!(
                "can't handle {n} for arg of type {}",
                other.go_name()
            )),
        }
    }

    fn eval_empty_interface(&mut self, dot: &Ev, n: &Arg) -> R<Ev> {
        self.at_arg(n);
        match n {
            Arg::Bool { val, .. } => Ok(Ev::Val(Value::Bool(*val))),
            Arg::Dot { .. } => Ok(dot.clone()),
            Arg::Number(num) => self.ideal_constant(num),
            Arg::String { text, .. } => Ok(Ev::Val(Value::String(text.clone()))),
            _ => self.errorf(format!(
                "can't handle assignment of {n} to empty interface argument"
            )),
        }
    }

    // ---------------------------------------------------------------
    // Output

    fn print_value(&mut self, v: &Ev, out: &mut String) {
        match v {
            Ev::Invalid => out.push_str("<no value>"),
            // An addressable struct field prints through a pointer-receiver
            // String() (Go's printableValue).
            Ev::Iface(Value::Object(o)) | Ev::Val(Value::Object(o))
                if o.stringer() == crate::value::Stringer::Pointer =>
            {
                o.format(out)
            }
            Ev::Iface(v) | Ev::Val(v) => gofmt::write_value(out, v),
        }
    }
}

/// How a map/slice-kind object (`.Files`, `VersionSet`, `[]string`,
/// `map[string]string`, nil maps) behaves as a collection.
struct Coll {
    /// A `List` or `Map` (a nil map reads as an empty map).
    data: Value,
    /// Elements have a concrete static type (not `interface {}`).
    concrete: bool,
    /// Value of a missing map key.
    missing: Ev,
}

fn collection(o: &dyn crate::value::Object) -> Option<Coll> {
    if !matches!(o.kind(), Kind::Map | Kind::Slice) {
        return None;
    }
    let any = o.as_any();
    if let Some(t) = any.downcast_ref::<crate::value::TypedMap>() {
        return Some(Coll {
            data: Value::Map(t.map.clone().unwrap_or_default()),
            concrete: t.elem_zero.is_some(),
            missing: t.elem_zero.clone().map_or(Ev::Iface(Value::Nil), Ev::Val),
        });
    }
    if let Some(t) = any.downcast_ref::<crate::value::TypedList>() {
        return Some(Coll {
            data: Value::List(t.items.clone()),
            concrete: t.go_type != "[]interface {}",
            missing: Ev::Invalid,
        });
    }
    let data = match o.to_data() {
        Value::Nil if o.kind() == Kind::Map => Value::empty_map(),
        Value::Nil => Value::list(Vec::new()),
        d => d,
    };
    // Helm's own collection types: `engine.files` (map[string][]byte) and
    // `common.VersionSet` ([]string).
    Some(Coll {
        data,
        concrete: true,
        missing: Ev::Val(Value::Bytes(Rc::from(&[][..]))),
    })
}

/// Returns the collection view of `v`, if it is a map/slice-kind object.
fn coll_of(v: &Ev) -> Option<Coll> {
    match v {
        Ev::Val(Value::Object(o)) | Ev::Iface(Value::Object(o)) => collection(&**o),
        _ => None,
    }
}

/// Wraps an element according to its collection's static element type.
fn elem(concrete: bool, v: Value) -> Ev {
    if concrete { Ev::Val(v) } else { Ev::Iface(v) }
}

impl Ev {
    /// A function's return value: nil becomes a nil interface.
    fn of_result(v: Value) -> Ev {
        if v.is_nil() {
            Ev::Iface(Value::Nil)
        } else {
            Ev::Val(v)
        }
    }
}

/// Which node a field chain came from (for diagnostics).
enum FieldCtx<'a> {
    Field(&'a FieldNode),
    Chain(&'a ChainNode),
    Var(&'a VariableNode),
}

impl FieldCtx<'_> {
    /// Text of the chain up to (excluding) segment `idx`, e.g. `.Values.a`.
    fn prefix(&self, ident: &[Ident], idx: usize) -> String {
        let mut s = match self {
            FieldCtx::Field(_) => String::new(),
            FieldCtx::Chain(c) => match &c.node {
                Arg::Pipe(p) => format!("({p})"),
                other => other.to_string(),
            },
            FieldCtx::Var(v) => v.ident[0].name.to_string(),
        };
        for id in &ident[..idx] {
            s.push('.');
            s.push_str(&id.name);
        }
        if s.is_empty() {
            s.push('.');
        }
        s
    }

    fn at(&self) -> At {
        match self {
            FieldCtx::Field(f) => At {
                pos: f.pos,
                kind: AtKind::Field,
            },
            FieldCtx::Chain(c) => At {
                pos: c.pos,
                kind: AtKind::Chain,
            },
            FieldCtx::Var(v) => At {
                pos: v.pos,
                kind: AtKind::Variable,
            },
        }
    }
}

fn is_hex_int(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() > 2 && b[0] == b'0' && (b[1] == b'x' || b[1] == b'X') && !s.contains(['p', 'P'])
}

/// Assigning a named map type (`common.Values`) to a
/// `map[string]interface{}` parameter converts it to the unnamed type.
fn to_param_type(v: Value, ty: Ty) -> Value {
    match (ty, v) {
        (Ty::Map, Value::Map(m)) if m.kind() != MapKind::Plain => {
            Value::Map(m.with_kind(MapKind::Plain))
        }
        (_, v) => v,
    }
}

/// Go assignability of a concrete value to a parameter type.
fn assignable(v: &Value, ty: Ty) -> bool {
    match (ty, v) {
        (Ty::Any, _) => true,
        (Ty::String, Value::String(_)) => true,
        (Ty::Bool, Value::Bool(_)) => true,
        (Ty::Int, Value::Int(_)) => true,
        (Ty::Int64, Value::Int64(_)) => true,
        (Ty::Uint64, Value::Uint64(_)) => true,
        (Ty::Float64, Value::Float(_)) => true,
        (Ty::Bytes, Value::Bytes(_)) => true,
        (Ty::List, Value::List(_)) => true,
        (Ty::Strings, Value::List(l)) => l.iter().all(|v| matches!(v, Value::String(_))),
        (Ty::Map, Value::Map(_)) => true,
        (Ty::Strings, Value::Object(o)) => o.type_name() == "[]string",
        (Ty::String, Value::Object(o)) => o.type_name() == "string",
        (Ty::Object(name) | Ty::Struct(name), Value::Object(o)) => o.type_name() == name,
        _ => false,
    }
}

/// Go's `isTrue` on an indirected value.
fn is_true(v: &Ev) -> bool {
    match v {
        Ev::Invalid => false,
        Ev::Iface(v) => !v.is_nil(),
        Ev::Val(v) => v.truthy(),
    }
}

/// Go's `truth`.
fn truth(v: &Ev) -> bool {
    is_true(&v.clone().indirect_iface())
}

/// Parameter type including `reflect.Value` (builtins).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PTy {
    Rv,
    Ty(Ty),
}

struct CallSig {
    fixed: Vec<PTy>,
    variadic: Option<PTy>,
}

impl CallSig {
    fn param(&self, i: usize) -> PTy {
        if i < self.fixed.len() {
            self.fixed[i]
        } else {
            self.variadic.unwrap_or(PTy::Ty(Ty::Any))
        }
    }
}

enum CallTarget<'a> {
    Builtin(Builtin),
    Values(MapRef, ValuesMethod),
    Helm(HelmFn),
    Func(Func),
    Method(&'a dyn crate::value::Object, crate::func::Sig, &'a str),
}

impl CallTarget<'_> {
    fn sig(&self) -> CallSig {
        match self {
            CallTarget::Builtin(b) => b.sig(),
            CallTarget::Helm(h) => h.sig(),
            CallTarget::Values(_, m) => m.sig(),
            CallTarget::Func(f) => CallSig {
                fixed: f.sig.params.iter().map(|t| PTy::Ty(*t)).collect(),
                variadic: f.sig.variadic.map(PTy::Ty),
            },
            CallTarget::Method(_, s, _) => CallSig {
                fixed: s.params.iter().map(|t| PTy::Ty(*t)).collect(),
                variadic: s.variadic.map(PTy::Ty),
            },
        }
    }
}

/// Error returned by a function call.
pub(crate) enum CallErr {
    /// An ordinary function error.
    Plain(FuncError),
    /// An error from `include`/`tpl`/`required`/`fail` carrying a Go error
    /// chain and a diagnostic. `frame` names the call for the stack trace.
    Helm(Box<HelmErr>),
}

pub(crate) struct HelmErr {
    go: GoErr,
    diag: Error,
    frame: Option<String>,
}

fn helm_err(go: GoErr, diag: Error, frame: Option<String>) -> CallErr {
    CallErr::Helm(Box::new(HelmErr { go, diag, frame }))
}

impl From<FuncError> for CallErr {
    fn from(e: FuncError) -> Self {
        CallErr::Plain(e)
    }
}

// -------------------------------------------------------------------
// Go builtins

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    And,
    Or,
    Not,
    Len,
    Index,
    Slice,
    Print,
    Printf,
    Println,
    Html,
    Js,
    Urlquery,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Call,
}

impl Builtin {
    pub fn from_name(name: &str) -> Option<Builtin> {
        Some(match name {
            "and" => Builtin::And,
            "or" => Builtin::Or,
            "not" => Builtin::Not,
            "len" => Builtin::Len,
            "index" => Builtin::Index,
            "slice" => Builtin::Slice,
            "print" => Builtin::Print,
            "printf" => Builtin::Printf,
            "println" => Builtin::Println,
            "html" => Builtin::Html,
            "js" => Builtin::Js,
            "urlquery" => Builtin::Urlquery,
            "eq" => Builtin::Eq,
            "ne" => Builtin::Ne,
            "lt" => Builtin::Lt,
            "le" => Builtin::Le,
            "gt" => Builtin::Gt,
            "ge" => Builtin::Ge,
            "call" => Builtin::Call,
            _ => return None,
        })
    }

    fn sig(self) -> CallSig {
        use PTy::*;
        let any = Ty(crate::func::Ty::Any);
        let (fixed, variadic): (Vec<PTy>, Option<PTy>) = match self {
            Builtin::And
            | Builtin::Or
            | Builtin::Index
            | Builtin::Slice
            | Builtin::Eq
            | Builtin::Call => (vec![Rv], Some(Rv)),
            Builtin::Not | Builtin::Len => (vec![Rv], None),
            Builtin::Ne | Builtin::Lt | Builtin::Le | Builtin::Gt | Builtin::Ge => {
                (vec![Rv, Rv], None)
            }
            Builtin::Print | Builtin::Println | Builtin::Html | Builtin::Js | Builtin::Urlquery => {
                (vec![], Some(any))
            }
            Builtin::Printf => (vec![Ty(crate::func::Ty::String)], Some(any)),
        };
        CallSig { fixed, variadic }
    }
}

impl State<'_, '_> {
    fn call_builtin(&mut self, b: Builtin, argv: &[Ev]) -> Result<Ev, CallErr> {
        let plain = |s: String| CallErr::Plain(FuncError::new(s));
        match b {
            Builtin::And | Builtin::Or => unreachable!(),
            Builtin::Not => Ok(Ev::Val(Value::Bool(!truth(&argv[0])))),
            Builtin::Len => {
                let (item, is_nil) = indirect(&argv[0]);
                let item = match coll_of(&item) {
                    Some(c) => Ev::Val(c.data),
                    None => item,
                };
                if is_nil {
                    return Err(plain("len of nil pointer".into()));
                }
                match item {
                    Ev::Invalid => Err(plain(
                        "reflect: call of reflect.Value.Type on zero Value".into(),
                    )),
                    Ev::Val(Value::String(s)) => Ok(Ev::Val(Value::Int(s.len() as i64))),
                    Ev::Val(Value::List(l)) => Ok(Ev::Val(Value::Int(l.len() as i64))),
                    Ev::Val(Value::Bytes(b)) => Ok(Ev::Val(Value::Int(b.len() as i64))),
                    Ev::Val(Value::Map(m)) => Ok(Ev::Val(Value::Int(m.len() as i64))),
                    Ev::Val(Value::Object(o)) if o.as_any().is::<crate::value::GoBytesString>() => {
                        let b = o
                            .as_any()
                            .downcast_ref::<crate::value::GoBytesString>()
                            .map_or(0, |b| b.0.len());
                        Ok(Ev::Val(Value::Int(b as i64)))
                    }
                    other => Err(plain(format!("len of type {}", other.type_string()))),
                }
            }
            Builtin::Index => builtin_index(&argv[0], &argv[1..]).map_err(plain),
            Builtin::Slice => builtin_slice(&argv[0], &argv[1..]).map_err(plain),
            Builtin::Print => Ok(Ev::Val(Value::string(gofmt::sprint(&values(argv))))),
            Builtin::Println => Ok(Ev::Val(Value::string(gofmt::sprintln(&values(argv))))),
            Builtin::Printf => {
                let fmt = argv[0].value();
                let fmt = fmt.as_str().unwrap_or("");
                Ok(Ev::Val(Value::string(gofmt::sprintf(
                    fmt,
                    &values(&argv[1..]),
                ))))
            }
            Builtin::Html => Ok(Ev::Val(Value::string(html_escape(&eval_args(argv))))),
            Builtin::Js => Ok(Ev::Val(Value::string(js_escape(&eval_args(argv))))),
            Builtin::Urlquery => Ok(Ev::Val(Value::string(query_escape(&eval_args(argv))))),
            Builtin::Eq => builtin_eq(&argv[0], &argv[1..])
                .map(|b| Ev::Val(Value::Bool(b)))
                .map_err(plain),
            Builtin::Ne => builtin_eq(&argv[0], &argv[1..2])
                .map(|b| Ev::Val(Value::Bool(!b)))
                .map_err(plain),
            Builtin::Lt => builtin_lt(&argv[0], &argv[1])
                .map(|b| Ev::Val(Value::Bool(b)))
                .map_err(plain),
            Builtin::Le => builtin_le(&argv[0], &argv[1])
                .map(|b| Ev::Val(Value::Bool(b)))
                .map_err(plain),
            Builtin::Gt => builtin_le(&argv[0], &argv[1])
                .map(|b| Ev::Val(Value::Bool(!b)))
                .map_err(plain),
            Builtin::Ge => builtin_lt(&argv[0], &argv[1])
                .map(|b| Ev::Val(Value::Bool(!b)))
                .map_err(plain),
            Builtin::Call => builtin_call("", argv),
        }
    }
}

/// Go's `call`: templates have no function values, so this always fails.
fn builtin_call(callee: &str, argv: &[Ev]) -> Result<Ev, CallErr> {
    let msg = match argv[0].clone().indirect_iface() {
        Ev::Invalid => String::from("call of nil"),
        other => format!("non-function {callee} of type {}", other.type_string()),
    };
    Err(CallErr::Plain(FuncError::new(msg)))
}

fn values(argv: &[Ev]) -> Vec<Value> {
    argv.iter().map(Ev::value).collect()
}

/// Go's `indirect`: (value, isNil).
fn indirect(v: &Ev) -> (Ev, bool) {
    match v {
        Ev::Iface(Value::Nil) => (v.clone(), true),
        Ev::Iface(x) => (Ev::Val(x.clone()), false),
        other => (other.clone(), false),
    }
}

/// `evalArgs` for html/js/urlquery.
fn eval_args(argv: &[Ev]) -> String {
    if argv.len() == 1
        && let Some(s) = argv[0].value().as_str()
    {
        return s.to_string();
    }
    let vals: Vec<Value> = argv
        .iter()
        .map(|a| match a.value() {
            Value::Nil => Value::str("<no value>"),
            v => v,
        })
        .collect();
    gofmt::sprint(&vals)
}

/// The integer value of an int-kind named type (`time.Duration`).
fn obj_int(v: &Ev) -> Option<i64> {
    match v {
        Ev::Val(Value::Object(o)) => o
            .as_any()
            .downcast_ref::<crate::funcs::duration::Duration>()
            .map(|d| d.0),
        _ => None,
    }
}

fn index_arg(index: &Ev, cap: usize) -> Result<usize, String> {
    let x: i64 = match index {
        v if obj_int(v).is_some() => obj_int(v).unwrap_or(0),
        Ev::Val(Value::Int(i)) | Ev::Val(Value::Int64(i)) => *i,
        Ev::Val(Value::Uint64(u)) => *u as i64,
        Ev::Invalid => return Err("cannot index slice/array with nil".into()),
        other => {
            return Err(format!(
                "cannot index slice/array with type {}",
                other.type_string()
            ));
        }
    };
    if x < 0 || x as u64 > cap as u64 {
        return Err(format!("index out of range: {x}"));
    }
    Ok(x as usize)
}

fn builtin_index(item: &Ev, indexes: &[Ev]) -> Result<Ev, String> {
    let mut item = item.clone().indirect_iface();
    if !item.is_valid() {
        return Err("index of untyped nil".into());
    }
    for index in indexes {
        let index = index.clone().indirect_iface();
        let (it, is_nil) = indirect(&item);
        let (it, concrete, missing) = match coll_of(&it) {
            Some(c) => (Ev::Val(c.data), c.concrete, c.missing),
            None => (it, false, Ev::Iface(Value::Nil)),
        };
        if is_nil {
            return Err("index of nil pointer".into());
        }
        item = match &it {
            Ev::Val(Value::List(l)) => {
                let x = index_arg(&index, l.len())?;
                match l.get(x) {
                    Some(v) => elem(concrete, v.clone()),
                    None => return Err("reflect: slice index out of range".into()),
                }
            }
            Ev::Val(Value::Bytes(b)) => {
                let x = index_arg(&index, b.len())?;
                match b.get(x) {
                    Some(v) => Ev::Val(Value::Int(*v as i64)),
                    None => return Err("reflect: slice index out of range".into()),
                }
            }
            Ev::Val(Value::String(s)) => {
                let x = index_arg(&index, s.len())?;
                match s.as_bytes().get(x) {
                    Some(v) => Ev::Val(Value::Int(*v as i64)),
                    None => return Err("reflect: string index out of range".into()),
                }
            }
            Ev::Val(Value::Map(m)) => {
                let key = match &index {
                    Ev::Invalid => return Err("value is nil; should be of type string".into()),
                    Ev::Val(Value::String(s)) => s.clone(),
                    other => {
                        return Err(format!(
                            "value has type {}; should be string",
                            other.type_string()
                        ));
                    }
                };
                match m.get(&key) {
                    Some(v) => elem(concrete, v),
                    None => missing,
                }
            }
            Ev::Invalid => unreachable!(),
            other => return Err(format!("can't index item of type {}", other.type_string())),
        };
    }
    Ok(item)
}

fn builtin_slice(item: &Ev, indexes: &[Ev]) -> Result<Ev, String> {
    let item = item.clone().indirect_iface();
    if !item.is_valid() {
        return Err("slice of untyped nil".into());
    }
    if indexes.len() > 3 {
        return Err(format!("too many slice indexes: {}", indexes.len()));
    }
    let (len, is_string) = match &item {
        Ev::Val(Value::String(s)) => {
            if indexes.len() == 3 {
                return Err("cannot 3-index slice a string".into());
            }
            (s.len(), true)
        }
        Ev::Val(Value::List(l)) => (l.len(), false),
        Ev::Val(Value::Bytes(b)) => (b.len(), false),
        other => return Err(format!("can't slice item of type {}", other.type_string())),
    };
    let mut idx = [0usize, len, 0];
    for (i, index) in indexes.iter().enumerate() {
        idx[i] = index_arg(&index.clone().indirect_iface(), len)?;
    }
    if idx[0] > idx[1] {
        return Err(format!("invalid slice index: {} > {}", idx[0], idx[1]));
    }
    if indexes.len() == 3 && idx[1] > idx[2] {
        return Err(format!("invalid slice index: {} > {}", idx[1], idx[2]));
    }
    let _ = is_string;
    Ok(match &item {
        Ev::Val(Value::String(s)) => {
            let bytes = &s.as_bytes()[idx[0]..idx[1]];
            Ev::Val(Value::string(String::from_utf8_lossy(bytes).into_owned()))
        }
        Ev::Val(Value::List(l)) => Ev::Val(Value::list(l[idx[0]..idx[1]].to_vec())),
        Ev::Val(Value::Bytes(b)) => Ev::Val(Value::Bytes(Rc::from(&b[idx[0]..idx[1]]))),
        _ => unreachable!(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BasicKind {
    Invalid,
    Bool,
    Int,
    Float,
    String,
    Uint,
}

fn basic_kind(v: &Ev) -> BasicKind {
    match v {
        Ev::Val(Value::Bool(_)) => BasicKind::Bool,
        Ev::Val(Value::Int(_) | Value::Int64(_)) => BasicKind::Int,
        Ev::Val(Value::Uint64(_)) => BasicKind::Uint,
        Ev::Val(Value::Float(_)) => BasicKind::Float,
        Ev::Val(Value::String(_)) => BasicKind::String,
        v if obj_int(v).is_some() => BasicKind::Int,
        _ => BasicKind::Invalid,
    }
}

fn as_i(v: &Ev) -> i64 {
    match v {
        Ev::Val(Value::Int(i) | Value::Int64(i)) => *i,
        v if obj_int(v).is_some() => obj_int(v).unwrap_or(0),
        _ => 0,
    }
}
fn as_u(v: &Ev) -> u64 {
    match v {
        Ev::Val(Value::Uint64(u)) => *u,
        _ => 0,
    }
}
fn as_f(v: &Ev) -> f64 {
    match v {
        Ev::Val(Value::Float(f)) => *f,
        _ => 0.0,
    }
}
fn as_b(v: &Ev) -> bool {
    matches!(v, Ev::Val(Value::Bool(true)))
}
fn as_s(v: &Ev) -> Rc<str> {
    match v {
        Ev::Val(Value::String(s)) => s.clone(),
        _ => Rc::from(""),
    }
}

fn go_kind(v: &Ev) -> &'static str {
    match v {
        Ev::Invalid => "invalid",
        Ev::Iface(_) => "interface",
        Ev::Val(v) => v.kind().name(),
    }
}

fn builtin_eq(arg1: &Ev, arg2: &[Ev]) -> Result<bool, String> {
    let arg1 = arg1.clone().indirect_iface();
    if arg2.is_empty() {
        return Err("missing argument for comparison".into());
    }
    let k1 = basic_kind(&arg1);
    for arg in arg2 {
        let arg = arg.clone().indirect_iface();
        let k2 = basic_kind(&arg);
        let truth = if k1 != k2 {
            match (k1, k2) {
                (BasicKind::Int, BasicKind::Uint) => {
                    as_i(&arg1) >= 0 && as_i(&arg1) as u64 == as_u(&arg)
                }
                (BasicKind::Uint, BasicKind::Int) => {
                    as_i(&arg) >= 0 && as_u(&arg1) == as_i(&arg) as u64
                }
                _ => {
                    if arg1.is_valid() && arg.is_valid() {
                        return Err(format!(
                            "incompatible types for comparison: {} and {}",
                            arg1.type_string(),
                            arg.type_string()
                        ));
                    }
                    false
                }
            }
        } else {
            match k1 {
                BasicKind::Bool => as_b(&arg1) == as_b(&arg),
                BasicKind::Float => as_f(&arg1) == as_f(&arg),
                BasicKind::Int => as_i(&arg1) == as_i(&arg),
                BasicKind::String => as_s(&arg1) == as_s(&arg),
                BasicKind::Uint => as_u(&arg1) == as_u(&arg),
                BasicKind::Invalid => {
                    // canCompare: same kind or either invalid.
                    let can =
                        go_kind(&arg1) == go_kind(&arg) || !arg1.is_valid() || !arg.is_valid();
                    if !can {
                        return Err(gofmt::sprintf(
                            "non-comparable types %s: %v, %s: %v",
                            &[
                                arg1.value(),
                                Value::string(arg1.type_string()),
                                Value::string(arg.type_string()),
                                arg.value(),
                            ],
                        ));
                    }
                    if !arg1.is_valid() || !arg.is_valid() {
                        !arg1.is_valid() && !arg.is_valid()
                    } else {
                        match &arg {
                            Ev::Val(Value::Map(_) | Value::List(_) | Value::Bytes(_)) => {
                                return Err(gofmt::sprintf(
                                    "non-comparable type %s: %v",
                                    &[arg.value(), Value::string(arg.type_string())],
                                ));
                            }
                            Ev::Val(Value::Object(o))
                                if matches!(o.kind(), Kind::Map | Kind::Slice) =>
                            {
                                return Err(gofmt::sprintf(
                                    "non-comparable type %s: %v",
                                    &[arg.value(), Value::string(arg.type_string())],
                                ));
                            }
                            _ => match (&arg1, &arg) {
                                (Ev::Val(Value::Object(a)), Ev::Val(Value::Object(b))) => {
                                    Rc::ptr_eq(a, b)
                                        || gofmt::format_value(&arg1.value())
                                            == gofmt::format_value(&arg.value())
                                }
                                _ => false,
                            },
                        }
                    }
                }
            }
        };
        if truth {
            return Ok(true);
        }
    }
    Ok(false)
}

fn builtin_lt(arg1: &Ev, arg2: &Ev) -> Result<bool, String> {
    let bad = || String::from("invalid type for comparison");
    let arg1 = arg1.clone().indirect_iface();
    let k1 = basic_kind(&arg1);
    if k1 == BasicKind::Invalid {
        return Err(bad());
    }
    let arg2 = arg2.clone().indirect_iface();
    let k2 = basic_kind(&arg2);
    if k2 == BasicKind::Invalid {
        return Err(bad());
    }
    if k1 != k2 {
        return match (k1, k2) {
            (BasicKind::Int, BasicKind::Uint) => {
                Ok(as_i(&arg1) < 0 || (as_i(&arg1) as u64) < as_u(&arg2))
            }
            (BasicKind::Uint, BasicKind::Int) => {
                Ok(as_i(&arg2) >= 0 && as_u(&arg1) < as_i(&arg2) as u64)
            }
            _ => Err(format!(
                "incompatible types for comparison: {} and {}",
                arg1.type_string(),
                arg2.type_string()
            )),
        };
    }
    Ok(match k1 {
        BasicKind::Bool => return Err(bad()),
        BasicKind::Float => as_f(&arg1) < as_f(&arg2),
        BasicKind::Int => as_i(&arg1) < as_i(&arg2),
        BasicKind::String => as_s(&arg1) < as_s(&arg2),
        BasicKind::Uint => as_u(&arg1) < as_u(&arg2),
        BasicKind::Invalid => unreachable!(),
    })
}

fn builtin_le(arg1: &Ev, arg2: &Ev) -> Result<bool, String> {
    let lt = builtin_lt(arg1, arg2)?;
    if lt {
        return Ok(true);
    }
    builtin_eq(arg1, core::slice::from_ref(arg2))
}

/// `template.HTMLEscapeString`.
pub(crate) fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\0' => out.push('\u{FFFD}'),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// `template.JSEscapeString`.
pub(crate) fn js_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '<' => out.push_str("\\u003C"),
            '>' => out.push_str("\\u003E"),
            '&' => out.push_str("\\u0026"),
            '=' => out.push_str("\\u003D"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u00{:02X}", c as u32));
            }
            c if (c as u32) < 0x80 => out.push(c),
            c => {
                if super::unicode::is_print(c) {
                    out.push(c);
                } else {
                    out.push_str(&format!("\\u{:04X}", c as u32));
                }
            }
        }
    }
    out
}

/// `url.QueryEscape`.
pub(crate) fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// -------------------------------------------------------------------
// Helm's context-dependent functions

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HelmFn {
    Include,
    Tpl,
    Required,
    Fail,
    Lookup,
    GetHostByName,
}

impl HelmFn {
    pub fn from_name(name: &str) -> Option<HelmFn> {
        Some(match name {
            "include" => HelmFn::Include,
            "tpl" => HelmFn::Tpl,
            "required" => HelmFn::Required,
            "fail" => HelmFn::Fail,
            "lookup" => HelmFn::Lookup,
            "getHostByName" => HelmFn::GetHostByName,
            _ => return None,
        })
    }

    fn sig(self) -> CallSig {
        use crate::func::Ty::*;
        let t = |v: &[Ty]| v.iter().map(|t| PTy::Ty(*t)).collect::<Vec<_>>();
        let fixed = match self {
            HelmFn::Include | HelmFn::Tpl | HelmFn::Required => t(&[String, Any]),
            HelmFn::Fail | HelmFn::GetHostByName => t(&[String]),
            HelmFn::Lookup => t(&[String, String, String, String]),
        };
        CallSig {
            fixed,
            variadic: None,
        }
    }
}

const WARN_START: &str = "HELM_ERR_START";
const WARN_END: &str = "HELM_ERR_END";

impl State<'_, '_> {
    fn call_helm(&mut self, h: HelmFn, args: &[Value]) -> Result<Ev, CallErr> {
        match h {
            HelmFn::Include => {
                let name = args[0].as_str().unwrap_or("");
                self.include(name, args[1].clone())
                    .map(|out| Ev::Val(Value::string(out)))
            }
            HelmFn::Tpl => {
                let text = args[0].as_str().unwrap_or("");
                self.tpl(text, args[1].clone())
                    .map(|out| Ev::Val(Value::string(out)))
            }
            _ => self.call_helm_other(h, args),
        }
    }

    #[inline(never)]
    fn call_helm_other(&mut self, h: HelmFn, args: &[Value]) -> Result<Ev, CallErr> {
        let s = |i: usize| -> String { args[i].as_str().unwrap_or("").to_string() };
        match h {
            HelmFn::Include | HelmFn::Tpl => unreachable!(),
            HelmFn::Required => {
                let warn = s(0);
                let val = &args[1];
                let missing = match val {
                    Value::Nil => true,
                    Value::String(s) => s.is_empty(),
                    _ => false,
                };
                if missing {
                    if self.ctx.lint_mode {
                        return Ok(Ev::Val(Value::str("")));
                    }
                    return Err(self.fail_error(&warn, ErrorKind::Fail, "required value missing"));
                }
                Ok(Ev::of_result(val.clone()))
            }
            HelmFn::Fail => {
                let msg = s(0);
                if self.ctx.lint_mode {
                    return Ok(Ev::Val(Value::str("")));
                }
                Err(self.fail_error(&msg, ErrorKind::Fail, "fail called"))
            }
            HelmFn::Lookup => {
                let (a, b, c, d) = (s(0), s(1), s(2), s(3));
                self.ctx
                    .env
                    .lookup(&a, &b, &c, &d)
                    .map(Ev::of_result)
                    .map_err(CallErr::Plain)
            }
            HelmFn::GetHostByName => {
                if !self.ctx.enable_dns {
                    return Ok(Ev::Val(Value::str("")));
                }
                let name = s(0);
                self.ctx
                    .env
                    .get_host_by_name(&name)
                    .map(|v| Ev::Val(Value::string(v)))
                    .map_err(CallErr::Plain)
            }
        }
    }

    fn fail_error(&self, msg: &str, kind: ErrorKind, _what: &str) -> CallErr {
        let go = GoErr::Plain {
            msg: format!("{WARN_START}{msg}{WARN_END}"),
            wrapped: None,
        };
        let diag = Error::new(kind, msg.to_string());
        helm_err(go, diag, None)
    }

    /// Helm's `include`.
    fn include(&mut self, name: &str, data: Value) -> Result<String, CallErr> {
        let count = self.ctx.included.get(name).copied();
        if let Some(v) = count {
            if v > RECURSION_MAX_NUMS {
                return Err(recursion_error(name));
            }
            self.ctx.included.insert(name.to_string(), v + 1);
        } else {
            self.ctx.included.insert(name.to_string(), 1);
        }
        let r = self.execute_template(name, data);
        if let Some(c) = self.ctx.included.get_mut(name) {
            *c -= 1;
        }
        r.map_err(|e| include_error(name, e))
    }

    /// Go's `Template.ExecuteTemplate` on the current set.
    fn execute_template(&mut self, name: &str, data: Value) -> Result<String, ExecTemplateErr> {
        let Some(tree) = self.set.lookup(name) else {
            return Err(not_found_error(name));
        };
        if self.ctx.depth >= self.ctx.max_depth {
            return Err(depth_error(name, self.ctx.max_depth));
        }
        self.ctx.depth += 1;
        let dot = Ev::of(data);
        let mut out = String::new();
        let r = {
            let mut sub = State::new(
                &mut *self.ctx,
                self.set.clone(),
                Rc::from(name),
                tree.clone(),
                dot.clone(),
            );
            sub.walk_list(&dot, &tree.root, &mut out)
        };
        self.ctx.depth -= 1;
        match r {
            Ok(()) => Ok(out),
            Err(Flow::Fail(f)) => Err(ExecTemplateErr::Fail(f)),
            Err(_) => Ok(out),
        }
    }

    /// Helm's `tpl`.
    fn tpl(&mut self, text: &str, vals: Value) -> Result<String, CallErr> {
        let source: Rc<str> = Rc::from(text);
        let funcs = self.ctx.funcs;
        let has = |n: &str| {
            funcs.contains(n) || Builtin::from_name(n).is_some() || HelmFn::from_name(n).is_some()
        };
        let trees = match super::parse::parse("gotpl", source.clone(), &has) {
            Ok(t) => t,
            Err(pe) => {
                let inner = GoErr::Plain {
                    msg: pe.go_message.clone(),
                    wrapped: None,
                };
                let msg = format!(
                    "cannot parse template {}: {}",
                    go_quote(text),
                    pe.go_message
                );
                let go = GoErr::Plain {
                    msg,
                    wrapped: Some(Box::new(inner)),
                };
                let diag = Error::new(ErrorKind::Parse, pe.message.clone())
                    .at(Location::from_span(
                        "gotpl",
                        source.clone(),
                        pe.span.clone(),
                    ))
                    .with_help("in the template string passed to `tpl`".to_string());
                return Err(helm_err(go, diag, Some("tpl".into())));
            }
        };
        let main = trees.get("gotpl").cloned();
        let child = Rc::new(Set::child(self.set.clone(), trees));
        let Some(main) = main else {
            return Ok(String::new());
        };
        if self.ctx.depth >= self.ctx.max_depth {
            let msg = format!("exceeded maximum template depth ({})", self.ctx.max_depth);
            let go = GoErr::Plain {
                msg: msg.clone(),
                wrapped: None,
            };
            return Err(helm_err(go, Error::new(ErrorKind::Exec, msg), None));
        }
        self.ctx.depth += 1;
        let dot = Ev::of(vals);
        let mut out = String::new();
        let r = {
            let mut sub = State::new(
                &mut *self.ctx,
                child,
                Rc::from("gotpl"),
                main.clone(),
                dot.clone(),
            );
            sub.walk_list(&dot, &main.root, &mut out)
        };
        self.ctx.depth -= 1;
        match r {
            Ok(()) | Err(Flow::Break) | Err(Flow::Continue) => Ok(out.replace("<no value>", "")),
            Err(Flow::Fail(f)) => {
                let msg = format!(
                    "error during tpl function execution for {}: {}",
                    go_quote(text),
                    f.go.message()
                );
                let go = GoErr::Plain {
                    msg,
                    wrapped: Some(Box::new(f.go)),
                };
                Err(helm_err(go, f.diag, Some("tpl".into())))
            }
        }
    }
}

#[cold]
#[inline(never)]
fn recursion_error(name: &str) -> CallErr {
    let inner = GoErr::Plain {
        msg: "unable to execute template".into(),
        wrapped: None,
    };
    let msg = format!(
        "rendering template has a nested reference name: {name}: unable to execute template"
    );
    let go = GoErr::Plain {
        msg: msg.clone(),
        wrapped: Some(Box::new(inner)),
    };
    let diag = Error::new(ErrorKind::Exec, msg);
    helm_err(go, diag, None)
}

#[cold]
#[inline(never)]
fn include_error(name: &str, e: ExecTemplateErr) -> CallErr {
    match e {
        ExecTemplateErr::NotFound(go) => {
            let diag = Error::new(ErrorKind::Exec, go.message())
                .with_help(format!("no template named {} is defined", go_quote(name)));
            helm_err(go, diag, None)
        }
        ExecTemplateErr::Fail(f) => {
            helm_err(f.go, f.diag, Some(format!("include {}", go_quote(name))))
        }
    }
}

#[cold]
#[inline(never)]
fn not_found_error(name: &str) -> ExecTemplateErr {
    let msg = format!(
        "template: no template {} associated with template {}",
        go_quote(name),
        go_quote("gotpl")
    );
    ExecTemplateErr::NotFound(GoErr::Plain { msg, wrapped: None })
}

#[cold]
#[inline(never)]
fn depth_error(name: &str, max: usize) -> ExecTemplateErr {
    let msg = format!("exceeded maximum template depth ({max})");
    let go = GoErr::Exec {
        msg: format!("template: {name}: {msg}"),
        wrapped: None,
    };
    ExecTemplateErr::Fail(Box::new(Fail {
        go,
        diag: Error::new(ErrorKind::Exec, msg),
    }))
}

enum ExecTemplateErr {
    NotFound(GoErr),
    Fail(Box<Fail>),
}

/// Entry point used by the engine: executes `tree` (named `name`) with `dot`.
pub(crate) fn execute(
    ctx: &mut Ctx<'_>,
    set: Rc<Set>,
    file: &TemplateFile,
    dot: Value,
) -> Result<String, Box<Fail>> {
    let dot = Ev::of(dot);
    let mut out = String::new();
    let mut st = State::new(
        ctx,
        set,
        Rc::from(file.name.as_str()),
        file.tree.clone(),
        dot.clone(),
    );
    match st.walk_list(&dot, &file.tree.root, &mut out) {
        Ok(()) | Err(Flow::Break) | Err(Flow::Continue) => Ok(out),
        Err(Flow::Fail(f)) => Err(f),
    }
}

/// Methods of Helm's `common.Values` map type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValuesMethod {
    Yaml,
    Table,
    AsMap,
    PathValue,
    Encode,
}

impl ValuesMethod {
    fn from_name(name: &str) -> Option<ValuesMethod> {
        Some(match name {
            "YAML" => ValuesMethod::Yaml,
            "Table" => ValuesMethod::Table,
            "AsMap" => ValuesMethod::AsMap,
            "PathValue" => ValuesMethod::PathValue,
            "Encode" => ValuesMethod::Encode,
            _ => return None,
        })
    }

    fn sig(self) -> CallSig {
        let fixed = match self {
            ValuesMethod::Yaml | ValuesMethod::AsMap => vec![],
            ValuesMethod::Table | ValuesMethod::PathValue => vec![PTy::Ty(Ty::String)],
            ValuesMethod::Encode => vec![PTy::Ty(Ty::Object("io.Writer"))],
        };
        CallSig {
            fixed,
            variadic: None,
        }
    }

    fn call(self, m: &MapRef, args: &[Value]) -> Result<Ev, FuncError> {
        match self {
            ValuesMethod::Yaml => {
                let s = crate::yaml::to_string(&Value::Map(m.clone()))
                    .map_err(|e| FuncError::new(e.message))?;
                Ok(Ev::Val(Value::string(s)))
            }
            ValuesMethod::AsMap => {
                if m.is_empty() {
                    return Ok(Ev::Val(Value::empty_map()));
                }
                Ok(Ev::Val(Value::Map(m.with_kind(MapKind::Plain))))
            }
            ValuesMethod::Table => {
                let name = args[0].as_str().unwrap_or("");
                values_table(m, name).map(|t| Ev::Val(Value::Map(t)))
            }
            ValuesMethod::PathValue => {
                let path = args[0].as_str().unwrap_or("");
                if path.is_empty() {
                    return Err(FuncError::new("YAML path cannot be empty"));
                }
                let parts: Vec<&str> = path.split('.').collect();
                let no_value = |k: &str| FuncError::new(format!("{} is not a value", go_quote(k)));
                let is_table = |v: &Value| matches!(v, Value::Map(_));
                if parts.len() == 1 {
                    return match m.get(parts[0]) {
                        Some(v) if !is_table(&v) => Ok(Ev::of_result(v)),
                        _ => Err(no_value(parts[0])),
                    };
                }
                let key = parts[parts.len() - 1];
                let table = values_table(m, &parts[..parts.len() - 1].join("."))
                    .map_err(|_| no_value(key))?;
                match table.get(key) {
                    Some(v) if !is_table(&v) => Ok(Ev::of_result(v)),
                    _ => Err(no_value(key)),
                }
            }
            ValuesMethod::Encode => Err(FuncError::new(
                "runtime error: invalid memory address or nil pointer dereference",
            )),
        }
    }
}

/// `common.Values.Table`.
fn values_table(m: &MapRef, name: &str) -> Result<MapRef, FuncError> {
    let mut table = m.clone();
    for n in name.split('.') {
        let next = match table.get(n) {
            None => return Err(FuncError::new(format!("{} is not a table", go_quote(n)))),
            Some(Value::Map(t)) => t.with_kind(MapKind::HelmValues),
            Some(_) => return Err(FuncError::new(format!("{} is not a table", go_quote(n)))),
        };
        table = next;
    }
    let _ = Map::new;
    Ok(table)
}
