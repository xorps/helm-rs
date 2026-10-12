//! Go `text/template`, as used by Helm.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Error;
use crate::func::{Env, FuncMap};
use crate::value::Value;

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
}

impl Default for Engine {
    fn default() -> Self {
        Engine { funcs: crate::funcs::helm(), strict: false, lint_mode: false }
    }
}

impl Engine {
    pub fn new() -> Engine {
        Engine::default()
    }

    /// Parses all templates into one set (so `define`s are shared) and
    /// executes every template whose base name does not start with `_`.
    /// Returns rendered output keyed by template name.
    pub fn render(
        &self,
        env: &mut dyn Env,
        templates: Vec<Renderable>,
    ) -> Result<BTreeMap<String, String>, Error> {
        let _ = (env, templates);
        todo!("Engine::render")
    }
}
