//! Go `text/template`, as used by Helm.
//!
//! [`Engine::render`] reproduces Helm's `engine.Render`: all chart templates
//! are parsed into one set (so `define`s are shared), then every template
//! whose base name does not start with `_` is executed.
//!
//! [`TemplateSet`] is the lower-level API for rendering arbitrary templates.

mod ast;
mod exec;
mod lex;
mod num;
mod parse;
pub(crate) mod quote;
pub(crate) mod unicode;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub use ast::Tree;
pub use parse::ParseError;

use crate::error::{Error, ErrorKind, Location};
use crate::func::{Env, FuncMap};
use crate::value::{Map, Value};

/// A template file to render, with the values it sees (Go `renderable`).
#[derive(Clone, Debug)]
pub struct Renderable {
    /// Full template name, e.g. `mychart/charts/sub/templates/svc.yaml`.
    pub name: String,
    pub source: Rc<str>,
    /// The top-level object (`.`): `Values`, `Release`, `Chart`, ... The
    /// engine adds `Template` before executing.
    pub vals: Value,
    /// e.g. `mychart/templates`.
    pub base_path: String,
}

/// Helm's template engine (`pkg/engine.Engine`).
#[derive(Clone, Debug)]
pub struct Engine {
    pub funcs: FuncMap,
    /// `missingkey=error` instead of `missingkey=zero`.
    pub strict: bool,
    /// `required`/`fail` do not fail (as in `helm lint`).
    pub lint_mode: bool,
    /// Allow `getHostByName` to call [`Env::get_host_by_name`].
    pub enable_dns: bool,
    /// Maximum nesting of `include`/`template`/`tpl` calls. Helm allows
    /// ~1000 nested `include`s of one name, but native stack space is finite
    /// (each level needs a few KiB, more in debug builds); exceeding this is
    /// a clean error instead of a stack overflow. Raise it if your templates
    /// recurse deeply and your thread has the stack for it.
    pub max_depth: usize,
}

impl Default for Engine {
    fn default() -> Self {
        Engine {
            funcs: crate::funcs::helm(),
            strict: false,
            lint_mode: false,
            enable_dns: false,
            max_depth: 128,
        }
    }
}

/// A Go error as a chain of `Error()` strings, so Helm's message rewriting
/// (`reformatExecErrorMsg`) can be reproduced exactly.
#[derive(Clone, Debug)]
pub(crate) enum GoErr {
    /// `template.ExecError` whose message optionally wraps (`%w`) another.
    Exec {
        msg: String,
        wrapped: Option<Box<GoErr>>,
    },
    /// Any other error, optionally wrapping another.
    Plain {
        msg: String,
        wrapped: Option<Box<GoErr>>,
    },
}

impl GoErr {
    pub fn message(&self) -> String {
        match self {
            GoErr::Exec { msg, .. } | GoErr::Plain { msg, .. } => msg.clone(),
        }
    }

    /// `errors.Unwrap` chain: (Error() string, is ExecError).
    fn chain(&self) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let mut cur = Some(self);
        while let Some(e) = cur {
            match e {
                GoErr::Exec { msg, wrapped } => {
                    out.push((msg.clone(), true));
                    // ExecError.Unwrap() is the fmt.Errorf error, same text.
                    out.push((msg.clone(), false));
                    cur = wrapped.as_deref();
                }
                GoErr::Plain { msg, wrapped } => {
                    out.push((msg.clone(), false));
                    cur = wrapped.as_deref();
                }
            }
        }
        out
    }
}

/// A template namespace with Go `Clone` semantics for `tpl`: a child layer
/// sees its parent's templates, and its own definitions shadow them.
#[derive(Debug, Default)]
pub(crate) struct Set {
    parent: Option<Rc<Set>>,
    map: BTreeMap<String, Rc<Tree>>,
}

impl Set {
    fn lookup(&self, name: &str) -> Option<Rc<Tree>> {
        match self.map.get(name) {
            Some(t) => Some(t.clone()),
            None => self.parent.as_ref().and_then(|p| p.lookup(name)),
        }
    }

    /// Go's `associate`: an empty tree never replaces an existing template.
    fn associate(&mut self, name: String, tree: Rc<Tree>) {
        if ast::is_empty_list(&tree.root, &tree.text) && self.lookup(&name).is_some() {
            return;
        }
        self.map.insert(name, tree);
    }

    fn child(parent: Rc<Set>, trees: BTreeMap<String, Rc<Tree>>) -> Set {
        let mut s = Set {
            parent: Some(parent),
            map: BTreeMap::new(),
        };
        for (name, tree) in trees {
            s.associate(name, tree);
        }
        s
    }
}

/// A parsed template file.
pub(crate) struct TemplateFile {
    pub name: String,
    pub tree: Rc<Tree>,
}

/// A set of parsed templates sharing one namespace of `define`s, like a Go
/// `*template.Template` with associated templates.
#[derive(Debug)]
pub struct TemplateSet {
    funcs: FuncMap,
    set: Set,
    /// Main tree of each parsed file, by name.
    files: BTreeMap<String, Rc<Tree>>,
    strict: bool,
    lint_mode: bool,
    enable_dns: bool,
    max_depth: usize,
}

impl TemplateSet {
    /// An empty set using the given engine's functions and options.
    pub fn new(engine: &Engine) -> TemplateSet {
        TemplateSet {
            funcs: engine.funcs.clone(),
            set: Set::default(),
            files: BTreeMap::new(),
            strict: engine.strict,
            lint_mode: engine.lint_mode,
            enable_dns: engine.enable_dns,
            max_depth: engine.max_depth,
        }
    }

    fn has_function(&self, name: &str) -> bool {
        self.funcs.contains(name)
            || exec::Builtin::from_name(name).is_some()
            || exec::HelmFn::from_name(name).is_some()
    }

    /// Parses `text` as template `name` (Go `t.New(name).Parse(text)`).
    pub fn add(&mut self, name: &str, text: &str) -> Result<(), Error> {
        self.add_rc(name, Rc::from(text))
            .map_err(|pe| parse_error(&pe, None))
    }

    fn add_rc(&mut self, name: &str, text: Rc<str>) -> Result<(), ParseError> {
        let trees = {
            let has = |n: &str| self.has_function(n);
            parse::parse(name, text, &has)?
        };
        if let Some(main) = trees.get(name) {
            self.files.insert(name.to_string(), main.clone());
        }
        for (n, t) in trees {
            self.set.associate(n, t);
        }
        Ok(())
    }

    /// Names of all defined templates.
    pub fn template_names(&self) -> impl Iterator<Item = &str> {
        self.set.map.keys().map(|s| s.as_str())
    }

    /// Executes template `name` with `data` as `.` (Go `ExecuteTemplate`).
    pub fn execute(&self, name: &str, data: &Value, env: &mut dyn Env) -> Result<String, Error> {
        let set = Rc::new(Set {
            parent: None,
            map: self.set.map.clone(),
        });
        let Some(tree) = set.lookup(name) else {
            let msg = format!(
                "template: no template {} associated with template {}",
                quote::go_quote(name),
                quote::go_quote("gotpl")
            );
            return Err(Error::new(ErrorKind::Exec, msg));
        };
        let mut ctx = self.ctx(env);
        let file = TemplateFile {
            name: name.to_string(),
            tree,
        };
        exec::execute(&mut ctx, set, &file, data.clone()).map_err(|f| finish_exec_error(name, *f))
    }

    fn ctx<'a>(&'a self, env: &'a mut dyn Env) -> exec::Ctx<'a> {
        exec::Ctx {
            funcs: &self.funcs,
            env,
            strict: self.strict,
            lint_mode: self.lint_mode,
            enable_dns: self.enable_dns,
            included: BTreeMap::new(),
            depth: 0,
            max_depth: self.max_depth,
        }
    }
}

impl Engine {
    pub fn new() -> Engine {
        Engine::default()
    }

    /// Renders a single template string with `data` as `.`, with Helm's
    /// `<no value>` cleanup. Convenience for tests and simple uses.
    pub fn render_str(
        &self,
        name: &str,
        text: &str,
        data: &Value,
        env: &mut dyn Env,
    ) -> Result<String, Error> {
        let mut set = TemplateSet::new(self);
        set.add_rc(name, Rc::from(text))
            .map_err(|pe| parse_error(&pe, Some(name)))?;
        let out = set.execute(name, data, env)?;
        Ok(out.replace("<no value>", ""))
    }

    /// Parses all templates into one set (so `define`s are shared) and
    /// executes every template whose base name does not start with `_`.
    /// Returns rendered output keyed by template name.
    pub fn render(
        &self,
        env: &mut dyn Env,
        templates: Vec<Renderable>,
    ) -> Result<BTreeMap<String, String>, Error> {
        let mut by_name: BTreeMap<String, Renderable> = BTreeMap::new();
        for t in templates {
            by_name.insert(t.name.clone(), t);
        }
        let keys = sort_templates(by_name.keys().cloned().collect());

        let mut set = TemplateSet::new(self);
        for name in &keys {
            let r = &by_name[name];
            if let Err(pe) = set.add_rc(name, r.source.clone()) {
                return Err(parse_error(&pe, Some(name)));
            }
        }

        let shared = Rc::new(Set {
            parent: None,
            map: set.set.map.clone(),
        });
        let mut rendered = BTreeMap::new();
        let mut ctx = set.ctx(env);
        for name in &keys {
            let base = name.rsplit('/').next().unwrap_or(name);
            if base.starts_with('_') {
                continue;
            }
            let r = &by_name[name];
            if let Value::Map(m) = &r.vals {
                let mut t = Map::new();
                t.insert("Name".into(), Value::str(name));
                t.insert("BasePath".into(), Value::str(&r.base_path));
                m.insert("Template", Value::map(t));
            }
            let Some(tree) = set.files.get(name) else {
                continue;
            };
            let file = TemplateFile {
                name: name.clone(),
                tree: tree.clone(),
            };
            match exec::execute(&mut ctx, shared.clone(), &file, r.vals.clone()) {
                Ok(out) => {
                    rendered.insert(name.clone(), out.replace("<no value>", ""));
                }
                Err(f) => return Err(finish_exec_error(name, *f)),
            }
        }
        Ok(rendered)
    }
}

/// Helm's `sortTemplates`: deepest paths first, then reverse lexical.
fn sort_templates(mut keys: Vec<String>) -> Vec<String> {
    keys.sort_by(|a, b| {
        let ca = a.matches('/').count();
        let cb = b.matches('/').count();
        // sort.Reverse(byPathLen): reverse of (count asc, then a < b).
        cb.cmp(&ca).then_with(|| b.cmp(a))
    });
    keys
}

/// Builds the public error for a parse failure, with Helm's message
/// (`cleanupParseError`).
fn parse_error(pe: &ParseError, helm_file: Option<&str>) -> Error {
    let mut e = Error::new(ErrorKind::Parse, pe.message.clone());
    e.location = Some(Location::new(pe.name.clone(), pe.line as u32, 0));
    if let Some(src) = pe_source(pe) {
        e.location = Some(Location::from_span(pe.name.clone(), src, pe.span.clone()));
    }
    let helm = match helm_file {
        Some(file) => cleanup_parse_error(file, &pe.go_message),
        None => pe.go_message.clone(),
    };
    e.helm_message = Some(helm);
    e
}

fn pe_source(pe: &ParseError) -> Option<Rc<str>> {
    pe.source.clone()
}

/// Helm's `cleanupParseError`.
fn cleanup_parse_error(filename: &str, go_msg: &str) -> String {
    let tokens: Vec<&str> = go_msg.split(": ").collect();
    if tokens.len() == 1 {
        return format!("parse error in ({filename}): {go_msg}");
    }
    let location = tokens[1];
    let err_msg = tokens[tokens.len() - 1];
    format!("parse error at ({location}): {err_msg}")
}

fn finish_exec_error(filename: &str, f: exec::Fail) -> Error {
    let mut diag = f.diag;
    diag.helm_message = Some(reformat_exec_error_msg(filename, &f.go));
    diag
}

#[derive(Clone, PartialEq, Eq, Default)]
struct TraceableError {
    location: String,
    message: String,
    executed_function: String,
}

impl TraceableError {
    fn render(&self) -> String {
        let mut s = String::new();
        if !self.location.is_empty() {
            s.push_str(&self.location);
            s.push_str("\n  ");
        }
        if !self.executed_function.is_empty() {
            s.push_str(&self.executed_function);
            s.push_str("\n    ");
        }
        if !self.message.is_empty() {
            s.push_str(&self.message);
            s.push('\n');
        }
        s
    }
}

fn cut_template_suffix(msg: &str) -> &str {
    match msg.find(" template:") {
        Some(i) => &msg[..i],
        None => msg,
    }
}

/// Helm's `parseTemplateExecErrorString`.
fn parse_template_exec_error_string(s: &str) -> Option<TraceableError> {
    let remainder = s.strip_prefix("template: ")?;
    if remainder.starts_with("no template ") {
        return Some(TraceableError {
            message: s.to_string(),
            ..Default::default()
        });
    }
    if let Some(i) = remainder.find(": executing ") {
        let template_name = &remainder[..i];
        let after = &remainder[i + ": executing ".len()..];
        let ok = (|| {
            if !after.starts_with('"') {
                return None;
            }
            let end_quote = after[1..].find('"')? + 1;
            let function_name = &after[1..end_quote];
            let after_func = &after[end_quote + 1..];
            let after_at = after_func.strip_prefix(" at <")?;
            let j = after_at.find(">: ")?;
            let location_name = &after_at[..j];
            let err_msg = cut_template_suffix(&after_at[j + 3..]);
            Some(TraceableError {
                location: template_name.to_string(),
                message: err_msg.to_string(),
                executed_function: format!("executing \"{function_name}\" at <{location_name}>:"),
            })
        })();
        if ok.is_some() {
            return ok;
        }
    }
    if let Some(sep) = remainder.rfind(": ") {
        let template_name = &remainder[..sep];
        let err_msg = cut_template_suffix(&remainder[sep + 2..]);
        return Some(TraceableError {
            location: template_name.to_string(),
            message: err_msg.to_string(),
            ..Default::default()
        });
    }
    None
}

/// Helm's `reformatExecErrorMsg`.
fn reformat_exec_error_msg(filename: &str, err: &GoErr) -> String {
    let chain = err.chain();
    let full = err.message();
    if !chain.iter().any(|(_, is_exec)| *is_exec) {
        return full;
    }
    let tokens: Vec<&str> = full.splitn(3, ": ").collect();
    if tokens.len() != 3 {
        return format!("execution error in ({filename}): {full}");
    }
    let location = tokens[1];
    if let Some(start) = tokens[2].find("HELM_ERR_START")
        && let Some(end) = tokens[2].rfind("HELM_ERR_END")
        && end >= start + "HELM_ERR_START".len()
    {
        let inner = &tokens[2][start + "HELM_ERR_START".len()..end];
        return format!("execution error at ({location}): {inner}");
    }
    let mut locs: Vec<TraceableError> = Vec::new();
    for (s, _) in &chain {
        match parse_template_exec_error_string(s) {
            Some(tr) => {
                if locs.last() != Some(&tr) {
                    locs.push(tr);
                }
            }
            None => return full,
        }
    }
    let mut out = String::new();
    for l in &locs {
        out.push_str(&l.render());
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::func::DeterministicEnv;

    fn render(t: &str, data: Value) -> Result<String, Error> {
        let mut e = Engine::new();
        e.funcs = FuncMap::new();
        e.render_str("t", t, &data, &mut DeterministicEnv::default())
    }

    #[test]
    fn basic() {
        let data = crate::map! { "a" => "x", "b" => crate::map!{ "c" => true } };
        assert_eq!(
            render("hi {{ .a }} {{ if .b.c }}yes{{ end }}", data.clone()).unwrap(),
            "hi x yes"
        );
        assert_eq!(
            render(
                "{{- range $k, $v := . }}{{ $k }}={{ $v }};{{ end }}",
                crate::map! {"x" => "1", "y" => "2"}
            )
            .unwrap(),
            "x=1;y=2;"
        );
        assert_eq!(render("{{ .missing }}", data.clone()).unwrap(), "");
        let e = render("line1\n {{ .a.b.c }}", crate::map! {}).unwrap_err();
        assert_eq!(e.message, "nil pointer evaluating interface {}.b");
        assert_eq!(
            e.helm_message.as_deref(),
            Some(
                "t:2:6\n  executing \"t\" at <.a.b.c>:\n    nil pointer evaluating interface {}.b"
            )
        );
        assert_eq!((e.line(), e.column()), (Some(2), Some(7)));
    }

    #[test]
    fn parse_errors() {
        let e = render("a\n{{ .x ", Value::Nil).unwrap_err();
        assert_eq!(
            e.helm_message.as_deref(),
            Some("parse error at (t:2): unclosed action")
        );
        assert_eq!((e.line(), e.column()), (Some(2), Some(1)));
    }
}
