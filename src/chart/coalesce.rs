//! Values coalescing (Helm `pkg/chart/common/util/coalesce.go`), ported
//! literally, including Go map aliasing (via [`MapRef`]).

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, ErrorKind};
use crate::value::{MapRef, Value};

use super::Chart;
use super::values::is_table;

const GLOBAL_KEY: &str = "global";

fn concat_prefix(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.into()
    } else {
        alloc::format!("{a}.{b}")
    }
}

fn entries(m: &MapRef) -> Vec<(String, Value)> {
    m.borrow()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `CoalesceValues(chart, vals)`: user values over chart defaults, nulls
/// delete keys, globals propagated, subchart values coalesced.
pub fn coalesce_values(chart: &Chart, vals: &MapRef) -> Result<MapRef, Error> {
    let copy = vals.deep_clone();
    coalesce(chart, &copy, "", false)?;
    Ok(copy)
}

/// `MergeValues(chart, vals)`: like [`coalesce_values`] but keeps nulls.
pub fn merge_values(chart: &Chart, vals: &MapRef) -> Result<MapRef, Error> {
    let copy = vals.deep_clone();
    coalesce(chart, &copy, "", true)?;
    Ok(copy)
}

fn coalesce(ch: &Chart, dest: &MapRef, prefix: &str, merge: bool) -> Result<(), Error> {
    coalesce_chart_values(ch, dest, prefix, merge);
    coalesce_deps(ch, dest, prefix, merge)
}

fn coalesce_deps(ch: &Chart, dest: &MapRef, prefix: &str, merge: bool) -> Result<(), Error> {
    for sub in &ch.dependencies {
        let name = sub.name();
        match dest.get(name) {
            None => {
                dest.insert(name, Value::empty_map());
            }
            Some(c) if !is_table(&c) => {
                return Err(Error::new(
                    ErrorKind::Values,
                    alloc::format!("type mismatch on {name}: {}", go_percent_t(&c)),
                ));
            }
            Some(_) => {}
        }
        if let Some(Value::Map(dvmap)) = dest.get(name) {
            let sub_prefix = concat_prefix(prefix, ch.name());
            coalesce_globals(&dvmap, dest, &sub_prefix);
            coalesce(sub, &dvmap, &sub_prefix, merge)?;
            dest.insert(name, Value::Map(dvmap));
        }
    }
    Ok(())
}

fn coalesce_globals(dest: &MapRef, src: &MapRef, prefix: &str) {
    let dg = match dest.get(GLOBAL_KEY) {
        None => MapRef::default(),
        Some(Value::Map(m)) => m,
        Some(_) => return, // warning: destination global is not a table
    };
    let sg = match src.get(GLOBAL_KEY) {
        None => MapRef::default(),
        Some(Value::Map(m)) => m,
        Some(_) => return,
    };
    for (key, val) in entries(&sg) {
        if let Value::Map(vm) = &val {
            let vv = vm.shallow_clone();
            match dg.get(&key) {
                None => {
                    dg.insert(key, Value::Map(vv));
                }
                Some(Value::Map(destvmap)) => {
                    let sub_prefix = concat_prefix(prefix, &key);
                    coalesce_tables_full_key(&vv, &destvmap, &sub_prefix, true);
                    dg.insert(key, Value::Map(vv));
                }
                Some(_) => {} // Conflict: cannot merge map onto non-map
            }
        } else if dg.get(&key).is_some_and(|dv| is_table(&dv)) {
            // key is table. Skipping
        } else {
            dg.insert(key, val);
        }
    }
    dest.insert(GLOBAL_KEY, Value::Map(dg));
}

fn coalesce_chart_values(c: &Chart, v: &MapRef, prefix: &str, merge: bool) {
    let _sub_prefix = concat_prefix(prefix, c.name());
    let vc = c.values.deep_clone();
    for (key, val) in entries(&vc) {
        if let Some(value) = v.get(&key) {
            if value.is_nil() && !merge {
                v.remove(&key);
            } else if let Value::Map(dest) = value {
                if let Value::Map(src) = &val {
                    let merge = child_chart_merge_true(c, &key, merge);
                    if !merge {
                        clean_nil_values(src);
                    }
                    coalesce_tables_full_key(&dest, src, &concat_prefix(&_sub_prefix, &key), merge);
                }
                // else: warning: skipped value: Not a table.
            }
        } else {
            if !merge {
                if val.is_nil() {
                    continue;
                }
                if let Value::Map(sub) = &val {
                    clean_nil_values(sub);
                }
            }
            v.insert(key, val);
        }
    }
}

fn child_chart_merge_true(c: &Chart, key: &str, merge: bool) -> bool {
    if c.dependencies.iter().any(|s| s.name() == key) {
        true
    } else {
        merge
    }
}

/// `CoalesceTables(dst, src)`: `dst` is authoritative.
pub fn coalesce_tables(dst: &MapRef, src: &MapRef) -> MapRef {
    coalesce_tables_full_key(dst, src, "", false);
    dst.clone()
}

/// `MergeTables(dst, src)`.
pub fn merge_tables(dst: &MapRef, src: &MapRef) -> MapRef {
    coalesce_tables_full_key(dst, src, "", true);
    dst.clone()
}

fn coalesce_tables_full_key(dst: &MapRef, src: &MapRef, prefix: &str, merge: bool) {
    let src_original_non_nil: Vec<String> = src
        .borrow()
        .iter()
        .filter(|(_, v)| !v.is_nil())
        .map(|(k, _)| k.clone())
        .collect();
    for (key, val) in entries(dst) {
        if val.is_nil() {
            src.insert(key, Value::Nil);
        }
    }
    for (key, val) in entries(src) {
        let fullkey = concat_prefix(prefix, &key);
        let dv = dst.get(&key);
        match dv {
            Some(Value::Nil) if !merge && src_original_non_nil.contains(&key) => {
                dst.remove(&key);
            }
            None => {
                dst.insert(key, val);
            }
            Some(dv) => {
                if let Value::Map(vm) = &val {
                    if let Value::Map(dm) = &dv {
                        coalesce_tables_full_key(dm, vm, &fullkey, merge);
                    }
                    // else: warning: cannot overwrite table with non table
                }
                // else if dv is a table and val != nil: warning, ignored
            }
        }
    }
}

/// Recursively removes nil entries in place.
fn clean_nil_values(m: &MapRef) {
    for (key, val) in entries(m) {
        match val {
            Value::Nil => {
                m.remove(&key);
            }
            Value::Map(sub) => clean_nil_values(&sub),
            _ => {}
        }
    }
}

/// `chartutil.trimNilValues`: deep copy without nil entries.
pub(crate) fn trim_nil_values(vals: &MapRef) -> MapRef {
    let copy = vals.deep_clone();
    fn trim(m: &MapRef) {
        for (key, val) in entries(m) {
            match val {
                Value::Nil => {
                    m.remove(&key);
                }
                Value::Map(sub) => trim(&sub),
                _ => {}
            }
        }
    }
    trim(&copy);
    copy
}

/// Go `fmt.Sprintf("%t", v)`.
fn go_percent_t(v: &Value) -> String {
    match v {
        Value::Bool(b) => alloc::format!("{b}"),
        Value::Nil => "%!t(<nil>)".into(),
        v => alloc::format!("%!t({}={})", v.type_name(), crate::gofmt::format_value(v)),
    }
}
