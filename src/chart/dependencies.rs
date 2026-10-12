//! Dependency processing (Helm `chartutil.ProcessDependencies`): enabling
//! subcharts by condition/tags, alias copies, and `import-values`.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Error;
use crate::value::{Map, MapRef, Value};

use super::coalesce::{coalesce_tables, coalesce_values, merge_tables, merge_values, trim_nil_values};
use super::metadata::Dependency;
use super::values::{path_value, table};
use super::Chart;

/// `util.IsCompatibleRange(constraint, version)`.
pub fn is_compatible_range(constraint: &str, ver: &str) -> bool {
    let Ok(v) = crate::semver::Version::parse(ver) else { return false };
    let Ok(c) = crate::semver::Constraints::parse(constraint) else { return false };
    c.check(&v)
}

/// `ProcessDependencies(chart, vals)`: mutates `chart` like Helm does.
pub fn process_dependencies(c: &mut Chart, v: &MapRef) -> Result<(), Error> {
    process_dependency_enabled(c, v, "")?;
    process_dependency_import_values(c, true)
}

fn process_dependency_conditions(reqs: &mut [Dependency], cvals: &MapRef, cpath: &str) {
    for r in reqs.iter_mut() {
        for c in r.condition.trim().split(',') {
            if c.is_empty() {
                continue;
            }
            if let Some(Value::Bool(b)) = path_value(cvals, &alloc::format!("{cpath}{c}")) {
                r.enabled = b;
                break;
            }
        }
    }
}

fn process_dependency_tags(reqs: &mut [Dependency], cvals: &MapRef) {
    let Some(vt) = table(cvals, "tags") else { return };
    for r in reqs.iter_mut() {
        let (mut has_true, mut has_false) = (false, false);
        for k in r.tags.iter().flatten() {
            if let Some(Value::Bool(b)) = vt.get(k) {
                if b {
                    has_true = true;
                } else {
                    has_false = true;
                }
            }
        }
        if !has_true && has_false {
            r.enabled = false;
        } else if has_true || !has_false {
            r.enabled = true;
        }
    }
}

/// Finds the chart for a dependency and copies it (with alias applied).
fn get_alias_dependency(charts: &[Chart], dep: &Dependency) -> Option<Chart> {
    for c in charts {
        if c.name() != dep.name {
            continue;
        }
        if !is_compatible_range(&dep.version, &c.metadata.version) {
            continue;
        }
        let mut out = c.clone();
        if !dep.alias.is_empty() {
            out.metadata.name = dep.alias.clone();
        }
        return Some(out);
    }
    None
}

fn process_dependency_enabled(c: &mut Chart, v: &MapRef, path: &str) -> Result<(), Error> {
    let Some(reqs) = c.metadata.dependencies.clone() else { return Ok(()) };
    let mut chart_dependencies: Vec<Chart> = Vec::new();
    for existing in &c.dependencies {
        let listed = reqs
            .iter()
            .any(|req| existing.name() == req.name && is_compatible_range(&req.version, &existing.metadata.version));
        if !listed {
            chart_dependencies.push(existing.clone());
        }
    }
    let mut reqs = reqs;
    for req in reqs.iter_mut() {
        if let Some(cd) = get_alias_dependency(&c.dependencies, req) {
            chart_dependencies.push(cd);
        }
        if !req.alias.is_empty() {
            req.name = req.alias.clone();
        }
    }
    c.dependencies = chart_dependencies;
    for r in reqs.iter_mut() {
        r.enabled = true;
    }
    c.metadata.dependencies = Some(reqs);
    let cvals = coalesce_values(c, v)?;
    let reqs = c.metadata.dependencies.as_deref_mut().unwrap_or_default();
    process_dependency_tags(reqs, &cvals);
    process_dependency_conditions(reqs, &cvals, path);
    let rm: BTreeSet<String> = reqs.iter().filter(|r| !r.enabled).map(|r| r.name.clone()).collect();
    let mut cd: Vec<Chart> =
        core::mem::take(&mut c.dependencies).into_iter().filter(|n| !rm.contains(n.name())).collect();
    let cd_metadata: Vec<Dependency> = reqs.iter().filter(|n| !rm.contains(&n.name)).cloned().collect();
    for t in cd.iter_mut() {
        let subpath = alloc::format!("{path}{}.", t.name());
        process_dependency_enabled(t, &cvals, &subpath)?;
    }
    // Go: `append(nil, cdMetadata...)` stays nil when nothing is kept.
    c.metadata.dependencies = if cd_metadata.is_empty() { None } else { Some(cd_metadata) };
    c.dependencies = cd;
    Ok(())
}

fn path_to_map(path: &str, data: MapRef) -> MapRef {
    if path == "." {
        return data;
    }
    let mut cur = data;
    for p in path.split('.').rev() {
        let mut m = Map::new();
        m.insert(p.into(), Value::Map(cur));
        cur = MapRef::new(m);
    }
    cur
}

/// `Values.AsMap()` of a table: an empty table becomes a fresh map.
fn as_map(m: MapRef) -> MapRef {
    if m.is_empty() { MapRef::default() } else { m }
}

fn process_import_values(c: &mut Chart, merge: bool) -> Result<(), Error> {
    if c.metadata.dependencies.is_none() {
        return Ok(());
    }
    let empty = MapRef::default();
    let cvals = if merge { merge_values(c, &empty)? } else { coalesce_values(c, &empty)? };
    let mut b = MapRef::default();
    let deps = c.metadata.dependencies.as_deref_mut().unwrap_or_default();
    for r in deps.iter_mut() {
        let mut outiv: Vec<Value> = Vec::new();
        for riv in r.import_values.iter().flatten() {
            match riv {
                Value::Map(iv) => {
                    let child = crate::gofmt::format_value(&iv.get("child").unwrap_or(Value::Nil));
                    let parent = crate::gofmt::format_value(&iv.get("parent").unwrap_or(Value::Nil));
                    let mut m = Map::new();
                    m.insert("child".into(), Value::str(&child));
                    m.insert("parent".into(), Value::str(&parent));
                    outiv.push(Value::string_map(m)); // Go map[string]string
                    let Some(vv) = table(&cvals, &alloc::format!("{}.{child}", r.name)) else { continue };
                    let src = path_to_map(&parent, as_map(vv));
                    b = if merge { merge_tables(&b, &src) } else { coalesce_tables(&b, &src) };
                }
                Value::String(iv) => {
                    let child = alloc::format!("exports.{iv}");
                    let mut m = Map::new();
                    m.insert("child".into(), Value::str(&child));
                    m.insert("parent".into(), Value::str("."));
                    outiv.push(Value::string_map(m)); // Go map[string]string
                    let Some(vm) = table(&cvals, &alloc::format!("{}.{child}", r.name)) else { continue };
                    let src = as_map(vm);
                    b = if merge { merge_tables(&b, &src) } else { coalesce_tables(&b, &src) };
                }
                _ => {}
            }
        }
        r.import_values = if outiv.is_empty() { None } else { Some(outiv) };
    }
    c.values = if merge {
        let cvals = cvals.deep_clone();
        merge_tables(&cvals, &b)
    } else {
        let cvals = trim_nil_values(&cvals);
        coalesce_tables(&cvals, &b)
    };
    Ok(())
}

fn process_dependency_import_values(c: &mut Chart, merge: bool) -> Result<(), Error> {
    for d in c.dependencies.iter_mut() {
        process_dependency_import_values(d, merge)?;
    }
    process_import_values(c, merge)
}
