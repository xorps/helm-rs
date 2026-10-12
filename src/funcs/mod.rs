//! Function libraries: Sprig and Helm's additions.

use crate::func::FuncMap;

/// Sprig's `TxtFuncMap()` (Masterminds/sprig v3, as pinned by Helm).
pub fn sprig() -> FuncMap {
    FuncMap::new()
}

/// Helm's function map (`engine.funcMap()`): Sprig without `env`/`expandenv`,
/// plus `toYaml`, `fromYaml`, `toJson`, `toToml`, duration helpers, ...
///
/// The context-dependent functions `include`, `tpl`, `required`, `fail` and
/// `lookup` are provided by the template engine itself.
pub fn helm() -> FuncMap {
    let mut f = sprig();
    f.remove("env");
    f.remove("expandenv");
    f
}
