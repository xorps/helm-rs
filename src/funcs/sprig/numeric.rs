//! Numeric functions (`numeric.go` and the arithmetic in `functions.go`).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::decimal::Decimal;
use super::{arg, iarg, to_float64, to_int64};
use crate::func::{CallCtx, FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::gofmt::strconv;
use crate::value::Value;

const A: Ty = Ty::Any;
const I: Ty = Ty::Int;

/// Max elements produced by `until`/`untilStep`/`seq`.
const MAX_ITEMS: u128 = 1 << 24;

pub(super) fn register(f: &mut FuncMap) {
    f.add("atoi", Sig::new(&[Ty::String]), |a| {
        let s = super::sarg(a, 0);
        Ok(Value::Int(match strconv::atoi(s) {
            Ok(n) => n,
            Err((strconv::NumErr::Range, n)) => n,
            Err(_) => 0,
        }))
    });
    f.add("int64", Sig::new(&[A]), |a| Ok(Value::Int64(to_int64(arg(a, 0)))));
    f.add("int", Sig::new(&[A]), |a| Ok(Value::Int(to_int64(arg(a, 0)))));
    f.add("float64", Sig::new(&[A]), |a| Ok(Value::Float(to_float64(arg(a, 0)))));
    f.add("seq", Sig::variadic(&[], I), seq);
    f.add("toDecimal", Sig::new(&[A]), |a| {
        let s = crate::gofmt::format_value(arg(a, 0));
        Ok(Value::Int64(strconv::parse_int(&s, 8, 64).unwrap_or(0)))
    });
    f.add("until", Sig::new(&[I]), |a| {
        let n = iarg(a, 0);
        until_step(0, n, if n < 0 { -1 } else { 1 })
    });
    f.add("untilStep", Sig::new(&[I, I, I]), |a| until_step(iarg(a, 0), iarg(a, 1), iarg(a, 2)));
    f.add("add1", Sig::new(&[A]), |a| Ok(Value::Int64(to_int64(arg(a, 0)).wrapping_add(1))));
    f.add("add", Sig::variadic(&[], A), |a| Ok(Value::Int64(a.iter().fold(0i64, |s, v| s.wrapping_add(to_int64(v))))));
    f.add("sub", Sig::new(&[A, A]), |a| Ok(Value::Int64(to_int64(arg(a, 0)).wrapping_sub(to_int64(arg(a, 1))))));
    f.add("div", Sig::new(&[A, A]), |a| {
        let (x, y) = (to_int64(arg(a, 0)), to_int64(arg(a, 1)));
        if y == 0 {
            return Err(FuncError::new("runtime error: integer divide by zero"));
        }
        Ok(Value::Int64(x.wrapping_div(y)))
    });
    f.add("mod", Sig::new(&[A, A]), |a| {
        let (x, y) = (to_int64(arg(a, 0)), to_int64(arg(a, 1)));
        if y == 0 {
            return Err(FuncError::new("runtime error: integer divide by zero"));
        }
        Ok(Value::Int64(x.wrapping_rem(y)))
    });
    f.add("mul", Sig::variadic(&[A], A), |a| {
        Ok(Value::Int64(a[1..].iter().fold(to_int64(arg(a, 0)), |s, v| s.wrapping_mul(to_int64(v)))))
    });
    f.add_ctx("randInt", Sig::new(&[I, I]), rand_int);
    f.add("add1f", Sig::new(&[A]), |a| decimal_op(arg(a, 0), &[Value::Int(1)], Op::Add));
    f.add("addf", Sig::variadic(&[], A), |a| decimal_op(&Value::Float(0.0), a, Op::Add));
    f.add("subf", Sig::variadic(&[A], A), |a| decimal_op(arg(a, 0), &a[1..], Op::Sub));
    f.add("divf", Sig::variadic(&[A], A), |a| decimal_op(arg(a, 0), &a[1..], Op::Div));
    f.add("mulf", Sig::variadic(&[A], A), |a| decimal_op(arg(a, 0), &a[1..], Op::Mul));
    let max: fn(&[Value]) -> FuncResult = |a| Ok(Value::Int64(a[1..].iter().fold(to_int64(arg(a, 0)), |m, v| m.max(to_int64(v)))));
    f.add("biggest", Sig::variadic(&[A], A), max);
    f.add("max", Sig::variadic(&[A], A), max);
    f.add("min", Sig::variadic(&[A], A), |a| {
        Ok(Value::Int64(a[1..].iter().fold(to_int64(arg(a, 0)), |m, v| m.min(to_int64(v)))))
    });
    f.add("maxf", Sig::variadic(&[A], A), |a| {
        Ok(Value::Float(a[1..].iter().fold(to_float64(arg(a, 0)), |m, v| go_max(m, to_float64(v)))))
    });
    f.add("minf", Sig::variadic(&[A], A), |a| {
        Ok(Value::Float(a[1..].iter().fold(to_float64(arg(a, 0)), |m, v| go_min(m, to_float64(v)))))
    });
    f.add("ceil", Sig::new(&[A]), |a| Ok(Value::Float(libm_ceil(to_float64(arg(a, 0))))));
    f.add("floor", Sig::new(&[A]), |a| Ok(Value::Float(libm_floor(to_float64(arg(a, 0))))));
    f.add("round", Sig::variadic(&[A, I], Ty::Float64), round);
}

pub(crate) fn libm_floor(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    if x.abs() >= 4_503_599_627_370_496.0 {
        return x;
    }
    let t = x as i64 as f64;
    let r = if t > x { t - 1.0 } else { t };
    if r == 0.0 && x < 0.0 { -0.0 } else { r }
}

fn libm_ceil(x: f64) -> f64 {
    -libm_floor(-x)
}

/// `math.Max`.
pub(crate) fn go_max(x: f64, y: f64) -> f64 {
    if x.is_infinite() && x > 0.0 || y.is_infinite() && y > 0.0 {
        return f64::INFINITY;
    }
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 && x == y {
        if x.is_sign_negative() {
            return y;
        }
        return x;
    }
    if x > y { x } else { y }
}

/// `math.Min`.
pub(crate) fn go_min(x: f64, y: f64) -> f64 {
    if x.is_infinite() && x < 0.0 || y.is_infinite() && y < 0.0 {
        return f64::NEG_INFINITY;
    }
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 && x == y {
        if x.is_sign_negative() {
            return x;
        }
        return y;
    }
    if x < y { x } else { y }
}

fn until_step(start: i64, stop: i64, step: i64) -> FuncResult {
    let count: u128 = if stop < start {
        if step >= 0 {
            0
        } else {
            ((start as i128 - stop as i128 + (-(step as i128)) - 1) / -(step as i128)) as u128
        }
    } else if step <= 0 {
        0
    } else {
        ((stop as i128 - start as i128 + step as i128 - 1) / step as i128) as u128
    };
    if count > MAX_ITEMS {
        return Err(super::too_large("untilStep"));
    }
    let mut v = Vec::with_capacity(count as usize);
    let mut i = start;
    for _ in 0..count {
        v.push(Value::Int(i));
        i = i.wrapping_add(step);
    }
    Ok(Value::list(v))
}

fn until_step_ints(start: i64, stop: i64, step: i64) -> Result<Vec<i64>, FuncError> {
    match until_step(start, stop, step)? {
        Value::List(l) => Ok(l.iter().map(|v| if let Value::Int(n) = v { *n } else { 0 }).collect()),
        _ => Ok(Vec::new()),
    }
}

fn seq(a: &[Value]) -> FuncResult {
    let p: Vec<i64> = (0..a.len()).map(|i| iarg(a, i)).collect();
    let ints = match p.len() {
        1 => {
            let (start, end) = (1i64, p[0]);
            let inc = if end < start { -1 } else { 1 };
            until_step_ints(start, end.wrapping_add(inc), inc)?
        }
        3 => {
            let (start, step, end) = (p[0], p[1], p[2]);
            let mut inc = 1;
            if end < start {
                inc = -1;
                if step > 0 {
                    return Ok(Value::str(""));
                }
            }
            until_step_ints(start, end.wrapping_add(inc), step)?
        }
        2 => {
            let (start, end) = (p[0], p[1]);
            let step = if end < start { -1 } else { 1 };
            until_step_ints(start, end.wrapping_add(step), step)?
        }
        _ => return Ok(Value::str("")),
    };
    let s: Vec<String> = ints.iter().map(|n| n.to_string()).collect();
    Ok(Value::string(s.join(" ")))
}

fn rand_int(ctx: &mut CallCtx<'_>, a: &[Value]) -> FuncResult {
    let (min, max) = (iarg(a, 0), iarg(a, 1));
    let n = max.wrapping_sub(min);
    if n <= 0 {
        return Err(FuncError::new("invalid argument to Intn"));
    }
    Ok(Value::Int((super::rand_below(ctx, n as u64) as i64).wrapping_add(min)))
}

#[derive(Clone, Copy)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

fn decimal_op(a: &Value, b: &[Value], op: Op) -> FuncResult {
    let mut prt = Decimal::from_float(to_float64(a))?;
    for x in b {
        let dx = Decimal::from_float(to_float64(x))?;
        prt = match op {
            Op::Add => prt.add(&dx, false)?,
            Op::Sub => prt.add(&dx, true)?,
            Op::Mul => prt.mul(&dx)?,
            Op::Div => prt.div(&dx)?,
        };
    }
    Ok(Value::Float(prt.to_f64()))
}

fn round(a: &[Value]) -> FuncResult {
    let round_on = match a.get(2) {
        Some(Value::Float(f)) => *f,
        _ => 0.5,
    };
    let val = to_float64(arg(a, 0));
    let places = iarg(a, 1) as f64;
    let pow = go_pow10(places);
    let digit = pow * val;
    let div = modf_frac(digit);
    let r = if div >= round_on { libm_ceil(digit) } else { libm_floor(digit) };
    Ok(Value::Float(r / pow))
}

/// `math.Modf(f)` fractional part.
fn modf_frac(f: f64) -> f64 {
    if f.is_infinite() {
        return f64::NAN;
    }
    if f.is_nan() {
        return f;
    }
    let int = if f < 0.0 { -libm_floor(-f) } else { libm_floor(f) };
    let frac = f - int;
    if frac == 0.0 && f.is_sign_negative() { -0.0 } else { frac }
}

/// `math.Pow(10, y)` for integral `y`.
fn go_pow10(y: f64) -> f64 {
    if y == 0.0 {
        return 1.0;
    }
    if y == 1.0 {
        return 10.0;
    }
    let yi = y.abs();
    if yi >= 9.223_372_036_854_775_808e18 {
        return if y > 0.0 { f64::INFINITY } else { 0.0 };
    }
    let mut a1 = 1.0f64;
    let mut ae: i64 = 0;
    // Frexp(10) = 0.625 * 2^4
    let (mut x1, mut xe) = (0.625f64, 4i64);
    let mut i = yi as i64;
    while i != 0 {
        if !(-(1 << 12)..=(1 << 12)).contains(&xe) {
            ae += xe;
            break;
        }
        if i & 1 == 1 {
            a1 *= x1;
            ae += xe;
        }
        x1 *= x1;
        xe <<= 1;
        if x1 < 0.5 {
            x1 += x1;
            xe -= 1;
        }
        i >>= 1;
    }
    if y < 0.0 {
        a1 = 1.0 / a1;
        ae = -ae;
    }
    ldexp(a1, ae)
}

/// `math.Ldexp`.
fn ldexp(frac: f64, exp: i64) -> f64 {
    if frac == 0.0 || frac.is_infinite() || frac.is_nan() {
        return frac;
    }
    // normalize
    let (mut frac, mut exp) = (frac, exp);
    if frac.abs() < f64::MIN_POSITIVE {
        frac *= (1u64 << 52) as f64;
        exp -= 52;
    }
    let mut x = frac.to_bits();
    exp += ((x >> 52) & 0x7FF) as i64 - 1023;
    if exp < -1075 {
        return if frac < 0.0 { -0.0 } else { 0.0 };
    }
    if exp > 1023 {
        return if frac < 0.0 { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let mut m = 1.0;
    if exp < -1022 {
        exp += 53;
        m = 1.0 / (1u64 << 53) as f64;
    }
    x &= !(0x7FFu64 << 52);
    x |= ((exp + 1023) as u64) << 52;
    m * f64::from_bits(x)
}
