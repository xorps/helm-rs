//! `list.go`: list functions over Go slices (via reflection in Sprig).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{NIL_DEREF, arg, deep_equal, empty, iarg, kind_name, slice_items, to_int64};
use crate::funcs::duration::f64_to_i64;
use crate::func::{FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::value::Value;

const A: Ty = Ty::Any;

/// Gets the slice elements of `list`, or the Go error for non-slices.
/// `what` is the verb in Sprig's message ("Cannot push on type %s").
fn items(list: &Value, msg: impl Fn(&str) -> String) -> Result<Vec<Value>, FuncError> {
    if list.is_nil() {
        return Err(FuncError::new(NIL_DEREF));
    }
    slice_items(list).ok_or_else(|| FuncError::new(msg(kind_name(list))))
}

macro_rules! both {
    ($f:expr, $name:literal, $must:literal, $sig:expr, $body:expr) => {
        $f.add($name, $sig, $body);
        $f.add($must, $sig, $body);
    };
}

pub(super) fn register(f: &mut FuncMap) {
    f.add("list", Sig::variadic(&[], A), |a| Ok(Value::list(a.to_vec())));
    f.add("tuple", Sig::variadic(&[], A), |a| Ok(Value::list(a.to_vec())));
    for n in ["append", "mustAppend", "push", "mustPush"] {
        f.add(n, Sig::new(&[A, A]), push);
    }
    both!(f, "prepend", "mustPrepend", Sig::new(&[A, A]), |a| {
        let mut l = items(arg(a, 0), |t| format!("Cannot prepend on type {t}"))?;
        l.insert(0, arg(a, 1).clone());
        Ok(Value::list(l))
    });
    both!(f, "first", "mustFirst", Sig::new(&[A]), |a| {
        let l = items(arg(a, 0), |t| format!("Cannot find first on type {t}"))?;
        Ok(l.into_iter().next().unwrap_or(Value::Nil))
    });
    both!(f, "last", "mustLast", Sig::new(&[A]), |a| {
        let l = items(arg(a, 0), |t| format!("Cannot find last on type {t}"))?;
        Ok(l.into_iter().last().unwrap_or(Value::Nil))
    });
    both!(f, "rest", "mustRest", Sig::new(&[A]), |a| {
        let l = items(arg(a, 0), |t| format!("Cannot find rest on type {t}"))?;
        Ok(Value::list(l.into_iter().skip(1).collect()))
    });
    both!(f, "initial", "mustInitial", Sig::new(&[A]), |a| {
        let mut l = items(arg(a, 0), |t| format!("Cannot find initial on type {t}"))?;
        l.pop();
        Ok(Value::list(l))
    });
    both!(f, "reverse", "mustReverse", Sig::new(&[A]), |a| {
        let mut l = items(arg(a, 0), |t| format!("Cannot find reverse on type {t}"))?;
        l.reverse();
        Ok(Value::list(l))
    });
    both!(f, "compact", "mustCompact", Sig::new(&[A]), |a| {
        let l = items(arg(a, 0), |t| format!("Cannot compact on type {t}"))?;
        Ok(Value::list(l.into_iter().filter(|v| !empty(v)).collect()))
    });
    both!(f, "uniq", "mustUniq", Sig::new(&[A]), |a| {
        let l = items(arg(a, 0), |t| format!("Cannot find uniq on type {t}"))?;
        let mut dest: Vec<Value> = Vec::new();
        for v in l {
            if !dest.iter().any(|d| deep_equal(&v, d)) {
                dest.push(v);
            }
        }
        Ok(Value::list(dest))
    });
    both!(f, "without", "mustWithout", Sig::variadic(&[A], A), |a| {
        let l = items(arg(a, 0), |t| format!("Cannot find without on type {t}"))?;
        let omit = &a[1.min(a.len())..];
        Ok(Value::list(l.into_iter().filter(|v| !omit.iter().any(|o| deep_equal(v, o))).collect()))
    });
    both!(f, "has", "mustHas", Sig::new(&[A, A]), |a| {
        let hay = arg(a, 1);
        if hay.is_nil() {
            return Ok(Value::Bool(false));
        }
        let l = items(hay, |t| format!("Cannot find has on type {t}"))?;
        let needle = arg(a, 0);
        Ok(Value::Bool(l.iter().any(|v| deep_equal(needle, v))))
    });
    both!(f, "slice", "mustSlice", Sig::variadic(&[A], A), slice);
    f.add("concat", Sig::variadic(&[], A), |a| {
        let mut res = Vec::new();
        for l in a {
            res.extend(items(l, |t| format!("Cannot concat type {t} as list"))?);
        }
        Ok(Value::list(res))
    });
    both!(f, "chunk", "mustChunk", Sig::new(&[Ty::Int, A]), chunk);
}

fn push(a: &[Value]) -> FuncResult {
    let mut l = items(arg(a, 0), |t| format!("Cannot push on type {t}"))?;
    l.push(arg(a, 1).clone());
    Ok(Value::list(l))
}

fn slice(a: &[Value]) -> FuncResult {
    let list = arg(a, 0);
    let l = items(list, |t| format!("list should be type of slice or array but {t}"))?;
    let n = l.len() as i64;
    if n == 0 {
        return Ok(Value::Nil);
    }
    let start = if a.len() > 1 { to_int64(&a[1]) } else { 0 };
    let end = if a.len() > 2 { to_int64(&a[2]) } else { n };
    if start < 0 || end < start || end > n {
        return Err(FuncError::new("reflect.Value.Slice: slice index out of bounds"));
    }
    match list {
        Value::Bytes(b) => Ok(Value::Bytes(b[start as usize..end as usize].to_vec().into())),
        _ => Ok(Value::list(l[start as usize..end as usize].to_vec())),
    }
}

fn chunk(a: &[Value]) -> FuncResult {
    let size = iarg(a, 0);
    let l = items(arg(a, 1), |t| format!("Cannot chunk type {t}"))?;
    let len = l.len() as i64;
    let makeslice = || FuncError::new("runtime error: makeslice: len out of range");
    let cs = f64_to_i64(super::numeric::libm_floor((len - 1) as f64 / size as f64) + 1.0);
    if cs < 0 || cs as u128 > l.len() as u128 + 1 {
        return Err(makeslice());
    }
    let mut out = Vec::with_capacity(cs as usize);
    for i in 0..cs {
        let mut clen = size;
        if i == cs - 1 {
            let m = (len as f64) % (size as f64);
            clen = f64_to_i64(super::numeric::libm_floor(m));
            if clen == 0 {
                clen = size;
            }
        }
        if clen < 0 {
            return Err(makeslice());
        }
        let mut c = Vec::with_capacity(clen.min(len) as usize);
        for j in 0..clen {
            let ix = i * size + j;
            match l.get(ix as usize) {
                Some(v) if ix >= 0 => c.push(v.clone()),
                _ => return Err(FuncError::new("reflect: slice index out of range")),
            }
        }
        out.push(Value::list(c));
    }
    // Go: [][]interface{}
    Ok(Value::typed_list("[][]interface {}", out))
}
