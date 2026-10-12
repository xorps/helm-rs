//! `dict.go`: dictionary functions, including `mergo`-compatible merging.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{arg, sarg, str_list, strval};
use crate::func::{FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::value::{Kind, Map, MapRef, Value};

const A: Ty = Ty::Any;
const M: Ty = Ty::Map;
const S: Ty = Ty::String;

fn marg(a: &[Value], i: usize) -> MapRef {
    match a.get(i) {
        Some(Value::Map(m)) => m.clone(),
        _ => MapRef::default(),
    }
}

pub(super) fn register(f: &mut FuncMap) {
    f.add("dict", Sig::variadic(&[], A), |a| {
        let mut m = Map::new();
        let mut i = 0;
        while i < a.len() {
            let key = strval(&a[i]);
            let v = if i + 1 >= a.len() {
                Value::str("")
            } else {
                a[i + 1].clone()
            };
            m.insert(key, v);
            i += 2;
        }
        Ok(Value::map(m))
    });
    f.add("get", Sig::new(&[M, S]), |a| {
        Ok(marg(a, 0).get(sarg(a, 1)).unwrap_or_else(|| Value::str("")))
    });
    f.add("set", Sig::new(&[M, S, A]), |a| {
        let m = marg(a, 0);
        m.insert(sarg(a, 1), arg(a, 2).clone());
        Ok(Value::Map(m))
    });
    f.add("unset", Sig::new(&[M, S]), |a| {
        let m = marg(a, 0);
        m.remove(sarg(a, 1));
        Ok(Value::Map(m))
    });
    f.add("hasKey", Sig::new(&[M, S]), |a| {
        Ok(Value::Bool(marg(a, 0).contains_key(sarg(a, 1))))
    });
    f.add("pluck", Sig::variadic(&[S], M), |a| {
        let key = sarg(a, 0);
        let mut res = Vec::new();
        for i in 1..a.len() {
            if let Some(v) = marg(a, i).get(key) {
                res.push(v);
            }
        }
        Ok(Value::list(res))
    });
    f.add("keys", Sig::variadic(&[], M), |a| {
        let mut k: Vec<String> = Vec::new();
        for i in 0..a.len() {
            k.extend(marg(a, i).borrow().keys().cloned());
        }
        Ok(str_list(k))
    });
    f.add("pick", Sig::variadic(&[M], S), |a| {
        let d = marg(a, 0);
        let mut res = Map::new();
        for i in 1..a.len() {
            let k = sarg(a, i);
            if let Some(v) = d.get(k) {
                res.insert(k.into(), v);
            }
        }
        Ok(Value::map(res))
    });
    f.add("omit", Sig::variadic(&[M], S), |a| {
        let d = marg(a, 0);
        let omit: Vec<&str> = (1..a.len()).map(|i| sarg(a, i)).collect();
        let res: Map = d
            .borrow()
            .iter()
            .filter(|(k, _)| !omit.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Ok(Value::map(res))
    });
    f.add("merge", Sig::variadic(&[M], M), |a| {
        merge(a, false).or_else(|_| Ok(Value::str("")))
    });
    f.add("mustMerge", Sig::variadic(&[M], M), |a| merge(a, false));
    f.add("mergeOverwrite", Sig::variadic(&[M], M), |a| {
        merge(a, true).or_else(|_| Ok(Value::str("")))
    });
    f.add("mustMergeOverwrite", Sig::variadic(&[M], M), |a| {
        merge(a, true)
    });
    f.add("values", Sig::new(&[M]), |a| {
        Ok(Value::list(marg(a, 0).borrow().values().cloned().collect()))
    });
    f.add("dig", Sig::variadic(&[], A), dig);
}

fn interface_conversion(v: &Value, want: &str) -> FuncError {
    let have = if v.is_nil() {
        String::from("nil")
    } else {
        v.type_name()
    };
    FuncError::new(format!(
        "interface conversion: interface {{}} is {have}, not {want}"
    ))
}

fn dig(ps: &[Value]) -> FuncResult {
    if ps.len() < 3 {
        return Err(FuncError::new("dig needs at least three arguments"));
    }
    // A type assertion: Helm's named `common.Values` does not satisfy it.
    let dict = match &ps[ps.len() - 1] {
        Value::Map(d) if d.kind() == crate::value::MapKind::Plain => d,
        other => return Err(interface_conversion(other, "map[string]interface {}")),
    };
    let def = &ps[ps.len() - 2];
    let mut ks = Vec::new();
    for p in &ps[..ps.len() - 2] {
        match p {
            Value::String(s) => ks.push(s.clone()),
            _ => return Err(interface_conversion(p, "string")),
        }
    }
    let mut dict = dict.clone();
    let mut i = 0;
    loop {
        let Some(step) = dict.get(&ks[i]) else {
            return Ok(def.clone());
        };
        if i + 1 == ks.len() {
            return Ok(step);
        }
        match step {
            Value::Map(m) => dict = m,
            other => return Err(interface_conversion(&other, "map[string]interface {}")),
        }
        i += 1;
    }
}

/// mergo's `isEmptyValue` (dereferencing).
fn mergo_empty(v: &Value) -> bool {
    match v {
        Value::Nil => true,
        Value::Object(o) => match o.kind() {
            Kind::Struct | Kind::Ptr => false,
            _ => !o.truthy(),
        },
        _ => !v.truthy(),
    }
}

fn merge(a: &[Value], overwrite: bool) -> FuncResult {
    let dst = marg(a, 0);
    for i in 1..a.len() {
        let src = marg(a, i);
        deep_merge_map(&dst, &src, overwrite, 0)?;
    }
    Ok(Value::Map(dst))
}

/// mergo `deepMerge` for `map[string]interface{}` destinations.
fn deep_merge_map(
    dst: &MapRef,
    src: &MapRef,
    overwrite: bool,
    depth: usize,
) -> Result<(), FuncError> {
    if depth > 1000 {
        return Err(FuncError::new("mergo: structure too deep (cyclic?)"));
    }
    let src_entries: Vec<(String, Value)> = src
        .borrow()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (key, src_el) in src_entries {
        let dst_el = dst.get(&key);
        if src_el.is_nil() {
            if overwrite {
                dst.insert(key, Value::Nil);
            }
            continue;
        }
        match &src_el {
            Value::Map(sm) => {
                if let Some(Value::Map(dm)) = &dst_el {
                    deep_merge_map(dm, sm, overwrite, depth + 1)?;
                }
            }
            Value::List(_) | Value::Bytes(_) => {
                // dstSlice selection; mergo always stores it back.
                let dst_slice = if overwrite || dst.is_empty() {
                    src_el.clone()
                } else {
                    match &dst_el {
                        None | Some(Value::Nil) => Value::list(Vec::new()),
                        Some(v) => v.clone(),
                    }
                };
                dst.insert(key.clone(), dst_slice);
            }
            _ => {}
        }
        if let Some(d) = &dst_el
            && !mergo_empty(d)
        {
            if matches!(src_el, Value::List(_) | Value::Bytes(_)) {
                continue;
            }
            if matches!(src_el, Value::Map(_)) && matches!(d, Value::Map(_)) {
                continue;
            }
        }
        if overwrite || dst_el.is_none() || dst_el.as_ref().is_some_and(mergo_empty) {
            dst.insert(key, src_el);
        }
    }
    Ok(())
}
