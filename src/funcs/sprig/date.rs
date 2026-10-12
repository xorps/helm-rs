//! `date.go`: date functions.

use alloc::string::{String, ToString};

use super::time::{Location, TIME_TYPE, Time, as_time, parse_in_location};
use super::{arg, sarg};
use crate::func::{CallCtx, FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::funcs::duration::{Duration, HOUR, MINUTE, SECOND, parse_duration};
use crate::gofmt::strconv;
use crate::value::Value;

const A: Ty = Ty::Any;
const S: Ty = Ty::String;
const T: Ty = Ty::Object(TIME_TYPE);

fn local(ctx: &mut CallCtx<'_>) -> Location {
    Location::Local(ctx.env.now().utc_offset_seconds)
}

/// `time.Now()` in the local zone.
pub(crate) fn now(ctx: &mut CallCtx<'_>) -> Time {
    let ts = ctx.env.now();
    Time { unix: ts.unix_seconds, nsec: ts.nanos.min(999_999_999), loc: Location::Local(ts.utc_offset_seconds) }
}

/// `time.LoadLocation(name)`, falling back to UTC like Sprig does on error.
fn load_location(ctx: &mut CallCtx<'_>, zone: &str) -> Location {
    match zone {
        "" | "UTC" => Location::Utc,
        "Local" => local(ctx),
        // No tz database in no_std: Go would load the zone here.
        _ => Location::Utc,
    }
}

fn time_arg(a: &[Value], i: usize) -> Time {
    a.get(i).and_then(as_time).unwrap_or_else(Time::zero)
}

/// The `date interface{}` conversion of `dateInZone`.
fn to_time(ctx: &mut CallCtx<'_>, v: &Value, int32_ok: bool) -> Time {
    let _ = int32_ok;
    if let Some(t) = as_time(v) {
        return t;
    }
    match v {
        Value::Int(n) | Value::Int64(n) => Time::from_unix(*n, local(ctx)),
        _ => now(ctx),
    }
}

fn date_in_zone(ctx: &mut CallCtx<'_>, fmt: &str, date: &Value, zone: &str) -> String {
    let t = to_time(ctx, date, true);
    let loc = load_location(ctx, zone);
    t.in_loc(loc).format(fmt)
}

pub(super) fn register(f: &mut FuncMap) {
    f.add_ctx("now", Sig::new(&[]), |c, _| Ok(Value::object(now(c))));
    f.add_ctx("date", Sig::new(&[S, A]), |c, a| Ok(Value::string(date_in_zone(c, sarg(a, 0), arg(a, 1), "Local"))));
    for name in ["dateInZone", "date_in_zone"] {
        f.add_ctx(name, Sig::new(&[S, A, S]), |c, a| {
            Ok(Value::string(date_in_zone(c, sarg(a, 0), arg(a, 1), sarg(a, 2))))
        });
    }
    f.add_ctx("htmlDate", Sig::new(&[A]), |c, a| Ok(Value::string(date_in_zone(c, "2006-01-02", arg(a, 0), "Local"))));
    f.add_ctx("htmlDateInZone", Sig::new(&[A, S]), |c, a| {
        Ok(Value::string(date_in_zone(c, "2006-01-02", arg(a, 0), sarg(a, 1))))
    });
    for name in ["dateModify", "date_modify"] {
        f.add(name, Sig::new(&[S, T]), |a| {
            let t = time_arg(a, 1);
            Ok(Value::object(match parse_duration(sarg(a, 0)) {
                Ok(d) => t.add(d),
                Err(_) => t,
            }))
        });
    }
    for name in ["mustDateModify", "must_date_modify"] {
        f.add(name, Sig::new(&[S, T]), |a| {
            let t = time_arg(a, 1);
            parse_duration(sarg(a, 0)).map(|d| Value::object(t.add(d))).map_err(FuncError::new)
        });
    }
    f.add_ctx("ago", Sig::new(&[A]), |c, a| {
        let t = match arg(a, 0) {
            v if as_time(v).is_some() => as_time(v).unwrap_or_else(Time::zero),
            Value::Int(n) | Value::Int64(n) => Time::from_unix(*n, local(c)),
            _ => now(c),
        };
        let d = now(c).sub(&t).round(Duration(SECOND));
        Ok(Value::string(d.go_string()))
    });
    f.add("duration", Sig::new(&[A]), |a| {
        let n = match arg(a, 0) {
            Value::String(s) => match strconv::parse_int(s, 10, 64) {
                Ok(n) => n,
                Err((_, n)) => n,
            },
            Value::Int64(n) => *n,
            _ => 0,
        };
        Ok(Value::string(Duration(n.wrapping_mul(SECOND)).go_string()))
    });
    f.add_ctx("durationRound", Sig::new(&[A]), duration_round);
    f.add_ctx("toDate", Sig::new(&[S, S]), |c, a| {
        let loc = local(c);
        Ok(Value::object(parse_in_location(sarg(a, 0), sarg(a, 1), &loc, &loc).unwrap_or_else(|_| Time::zero())))
    });
    f.add_ctx("mustToDate", Sig::new(&[S, S]), |c, a| {
        let loc = local(c);
        parse_in_location(sarg(a, 0), sarg(a, 1), &loc, &loc).map(Value::object).map_err(FuncError::new)
    });
    f.add("unixEpoch", Sig::new(&[T]), |a| Ok(Value::string(time_arg(a, 0).unix.to_string())));
}

fn duration_round(ctx: &mut CallCtx<'_>, a: &[Value]) -> FuncResult {
    let v = arg(a, 0);
    let d: i64 = if let Some(t) = as_time(v) {
        now(ctx).sub(&t).0
    } else {
        match v {
            Value::String(s) => parse_duration(s).map(|d| d.0).unwrap_or(0),
            Value::Int64(n) => *n,
            _ => 0,
        }
    };
    let mut u = d as u64;
    if d < 0 {
        u = u.wrapping_neg();
    }
    let year = HOUR as u64 * 24 * 365;
    let month = HOUR as u64 * 24 * 30;
    let day = HOUR as u64 * 24;
    let hour = HOUR as u64;
    let minute = MINUTE as u64;
    let second = SECOND as u64;
    let s = if u > year {
        alloc::format!("{}y", u / year)
    } else if u > month {
        alloc::format!("{}mo", u / month)
    } else if u > day {
        alloc::format!("{}d", u / day)
    } else if u > hour {
        alloc::format!("{}h", u / hour)
    } else if u > minute {
        alloc::format!("{}m", u / minute)
    } else if u > second {
        alloc::format!("{}s", u / second)
    } else {
        String::from("0s")
    };
    Ok(Value::string(s))
}
