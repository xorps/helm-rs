//! Function libraries: Sprig and Helm's additions.

use crate::func::FuncMap;

pub mod duration;
pub mod encoding;
pub mod sprig;

/// Sprig's `TxtFuncMap()` (Masterminds/sprig v3, as pinned by Helm).
pub fn sprig() -> FuncMap {
    let mut f = FuncMap::new();
    sprig::register(&mut f);
    f
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
    duration::register(&mut f);
    encoding::register(&mut f);
    f
}
