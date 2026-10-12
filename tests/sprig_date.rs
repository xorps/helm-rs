//! Differential tests for the date functions and `time.Time` formatting and
//! parsing. The oracle runs with `TZ=UTC` so that Go's `Local` matches the
//! default [`helm_rs::DeterministicEnv`] (offset 0).

mod diffkit;

use diffkit::*;
use helm_rs::{FuncMap, Value};

fn utc_oracle() {
    // The oracle process is spawned lazily by the first render; make sure it
    // inherits TZ=UTC. All tests in this binary set the same value.
    unsafe { std::env::set_var("TZ", "UTC") };
}

struct Batch {
    funcs: FuncMap,
    exprs: Vec<String>,
    ours: Vec<Result<String, String>>,
}

impl Batch {
    fn new() -> Batch {
        utc_oracle();
        Batch { funcs: helm_rs::funcs::helm(), exprs: Vec::new(), ours: Vec::new() }
    }
    fn add(&mut self, name: &str, args: Vec<Arg>) {
        let mut e = String::from(name);
        for a in &args {
            e.push(' ');
            e.push_str(&a.tmpl);
        }
        let vals: Vec<Value> = args.iter().map(|a| a.value.clone()).collect();
        let r = call(&self.funcs, name, &vals).map(|v| if v.is_nil() { String::new() } else { render_value(&v) });
        self.exprs.push(e);
        self.ours.push(r);
    }
    /// `(fn args).Method` style expressions on an object result.
    fn add_method(&mut self, name: &str, args: Vec<Arg>, method: &str, margs: Vec<Arg>) {
        let mut e = format!("({name}");
        for a in &args {
            e.push(' ');
            e.push_str(&a.tmpl);
        }
        e.push_str(&format!(").{method}"));
        for a in &margs {
            e.push(' ');
            e.push_str(&a.tmpl);
        }
        let vals: Vec<Value> = args.iter().map(|a| a.value.clone()).collect();
        let mvals: Vec<Value> = margs.iter().map(|a| a.value.clone()).collect();
        let r = call(&self.funcs, name, &vals).and_then(|v| match &v {
            Value::Object(o) => o.call_method(method, &mvals).map_err(|e| e.message),
            _ => Err("not an object".into()),
        });
        let r = r.map(|v| if v.is_nil() { String::new() } else { render_value(&v) });
        self.exprs.push(e);
        self.ours.push(r);
    }
    fn run(self, name: &str) {
        let mut bad = Vec::new();
        for chunk in (0..self.exprs.len()).collect::<Vec<_>>().chunks(500) {
            let (a, b) = (chunk[0], chunk[chunk.len() - 1] + 1);
            let Some(helm) = helm_eval_many(&self.exprs[a..b], "") else { return };
            bad.extend(compare(&self.exprs[a..b], &self.ours[a..b], &helm));
        }
        report(name, self.exprs.len(), &bad);
    }
}

fn s(x: &str) -> Arg {
    str_arg(x)
}
fn i(x: i64) -> Arg {
    int_arg(x)
}
fn i64a(x: i64) -> Arg {
    Arg::new(Value::Int64(x), format!("(int64 {})", lit(&x.to_string())))
}

/// A `time.Time` argument produced by `toDate layout value` in both worlds.
fn date_arg(layout: &str, value: &str) -> Arg {
    let funcs = helm_rs::funcs::helm();
    let v = call(&funcs, "toDate", &[Value::str(layout), Value::str(value)]).unwrap();
    Arg::new(v, format!("(toDate {} {})", lit(layout), lit(value)))
}

const LAYOUTS: &[&str] = &[
    "2006-01-02T15:04:05Z07:00",
    "2006-01-02T15:04:05.999999999Z07:00",
    "Mon Jan _2 15:04:05 2006",
    "Mon Jan _2 15:04:05 MST 2006",
    "Mon Jan 02 15:04:05 -0700 2006",
    "02 Jan 06 15:04 MST",
    "02 Jan 06 15:04 -0700",
    "Monday, 02-Jan-06 15:04:05 MST",
    "Mon, 02 Jan 2006 15:04:05 MST",
    "Mon, 02 Jan 2006 15:04:05 -0700",
    "3:04PM",
    "Jan _2 15:04:05.000000",
    "2006-01-02 15:04:05",
    "2006-01-02",
    "15:04:05",
    "January 2, 2006 3:04:05 pm",
    "06/1/2 3:4:5",
    "__2 002 Z0700 Z07 Z07:00:00 -07 -070000 -07:00:00",
    "2006-01-02T15:04:05.000Z",
    "2006-01-02 15:04:05,999",
    "20060102150405",
    "Jan Janx January Mon Monx Monday MST",
    "literal text only",
    "",
    ".0 .00 .9 .99 ,000 .000000000 .999999999",
];

#[test]
fn format_layouts() {
    let mut t = Batch::new();
    let times = [
        ("2006-01-02T15:04:05Z07:00", "2024-02-29T13:45:59Z"),
        ("2006-01-02T15:04:05Z07:00", "1999-12-31T23:59:59.123456789+05:30"),
        ("2006-01-02T15:04:05Z07:00", "2001-01-01T00:00:00-08:00"),
        ("2006-01-02T15:04:05Z07:00", "0001-01-01T00:00:00Z"),
        ("2006-01-02", "1969-07-20"),
        ("2006-01-02 15:04:05.000", "2022-10-05 07:08:09.010"),
        ("2006-01-02T15:04:05Z07:00", "2038-01-19T03:14:08.5Z"),
        ("2006-01-02T15:04:05Z07:00", "9999-12-31T23:59:59Z"),
    ];
    for (l, v) in times {
        for layout in LAYOUTS {
            t.add("date", vec![s(layout), date_arg(l, v)]);
            t.add("dateInZone", vec![s(layout), date_arg(l, v), s("UTC")]);
        }
        t.add("htmlDate", vec![date_arg(l, v)]);
        t.add("htmlDateInZone", vec![date_arg(l, v), s("UTC")]);
        t.add("unixEpoch", vec![date_arg(l, v)]);
        t.add("toString", vec![date_arg(l, v)]);
        for m in ["Year", "Month", "Day", "Hour", "Minute", "Second", "Nanosecond", "YearDay", "Weekday", "Unix", "UnixNano", "IsZero", "String", "UTC"] {
            t.add_method("toDate", vec![s(l), s(v)], m, vec![]);
        }
        t.add_method("toDate", vec![s(l), s(v)], "Format", vec![s("Jan 2 2006 3pm")]);
        t.add_method("toDate", vec![s(l), s(v)], "AddDate", vec![i(1), i(-14), i(40)]);
        for d in ["1h", "-36h", "1.5s", "abc", ""] {
            t.add("dateModify", vec![s(d), date_arg(l, v)]);
            t.add("mustDateModify", vec![s(d), date_arg(l, v)]);
        }
    }
    for n in [0i64, 1, -1, 86399, 951782400, 4102444800, -2208988800] {
        t.add("date", vec![s("2006-01-02 15:04:05 MST Mon"), i(n)]);
        t.add("date", vec![s("2006-01-02 15:04:05 MST Mon"), i64a(n)]);
        t.add("htmlDate", vec![i(n)]);
        t.add("dateInZone", vec![s("2006-01-02T15:04:05Z07:00"), i(n), s("UTC")]);
        t.add("dateInZone", vec![s("2006-01-02T15:04:05Z07:00"), i(n), s("Local")]);
        t.add("dateInZone", vec![s("2006-01-02T15:04:05Z07:00"), i(n), s("")]);
        t.add("dateInZone", vec![s("2006-01-02T15:04:05Z07:00"), i(n), s("Not/AZone")]);
    }
    t.run("format_layouts");
}

#[test]
fn parse_layouts() {
    let mut t = Batch::new();
    let cases: &[(&str, &str)] = &[
        ("2006-01-02", "2024-02-29"),
        ("2006-01-02", "2023-02-29"),
        ("2006-01-02", "2024-13-01"),
        ("2006-01-02", "24-01-01"),
        ("2006-01-02", "2024-01-01 extra"),
        ("2006-01-02", ""),
        ("2006-01-02 15:04:05", "2024-01-01 25:00:00"),
        ("2006-01-02 15:04:05", "2024-01-01 23:60:00"),
        ("2006-01-02 15:04:05", "2024-01-01 23:59:60"),
        ("2006-01-02 15:04:05", "2024-01-01 23:59:59.123456"),
        ("2006-01-02 15:04:05.000", "2024-01-01 23:59:59.12"),
        ("2006-01-02 15:04:05.999", "2024-01-01 23:59:59"),
        ("2006-01-02 15:04:05.999", "2024-01-01 23:59:59.5"),
        ("2006-01-02T15:04:05Z07:00", "2024-01-01T10:00:00+02:00"),
        ("2006-01-02T15:04:05Z07:00", "2024-01-01T10:00:00Z"),
        ("2006-01-02T15:04:05Z07:00", "2024-01-01T10:00:00+25:00"),
        ("2006-01-02T15:04:05Z07:00", "2024-01-01T10:00:00.999999999999-00:30"),
        ("2006-01-02T15:04:05Z07:00", "2024-1-01T10:00:00Z"),
        ("Jan 2 2006", "Feb 30 2024"),
        ("Jan 2 2006", "feb 3 2024"),
        ("Jan 2 2006", "FEBRUARY 3 2024"),
        ("January 2 2006", "february 3 2024"),
        ("Mon Jan 2 2006", "Tue Feb 3 2024"),
        ("Mon Jan 2 2006", "Xyz Feb 3 2024"),
        ("3:04PM", "12:30AM"),
        ("3:04PM", "12:30PM"),
        ("3:04PM", "1:30pm"),
        ("3:04pm", "1:30pm"),
        ("15:04 MST", "10:00 PST"),
        ("15:04 MST", "10:00 UTC"),
        ("15:04 MST", "10:00 GMT+3"),
        ("15:04 MST", "10:00 +03"),
        ("15:04 MST", "10:00 CEST"),
        ("15:04 MST", "10:00 X"),
        ("15:04 -0700", "10:00 +0530"),
        ("15:04 -07:00", "10:00 -0530"),
        ("15:04 -07", "10:00 +05"),
        ("15:04 Z07", "10:00 Z"),
        ("2006 002", "2024 060"),
        ("2006 002", "2023 366"),
        ("2006 __2", "2024  60"),
        ("2006-01 002", "2024-03 061"),
        ("2006-01 002", "2024-02 061"),
        ("_2 Jan", " 5 Jan"),
        ("06", "68"),
        ("06", "69"),
        ("2006", "abcd"),
        ("2006-01-02T15:04:05", "2024-01-01T10:00:00"),
        ("01/02/2006", "1/2/2006"),
        ("1/2/2006", "01/02/2006"),
        ("", ""),
        ("", "x"),
        ("literal", "literal"),
        ("literal", "litera"),
        ("2006  01", "2006 01"),
        ("2006 01", "2006  01"),
        ("2006-01-02", "２０２４-01-01"),
    ];
    for &(l, v) in cases {
        t.add("toDate", vec![s(l), s(v)]);
        t.add("mustToDate", vec![s(l), s(v)]);
        t.add_method("toDate", vec![s(l), s(v)], "Format", vec![s("2006-01-02T15:04:05.999999999Z07:00 MST")]);
    }
    t.run("parse_layouts");
}

#[test]
fn parse_random() {
    let mut t = Batch::new();
    let mut rng = Rng::new(2024);
    let pieces = ["2006", "06", "01", "1", "Jan", "January", "02", "2", "_2", "15", "3", "03", "04", "4", "05", "5", "PM", "pm", "MST", "-0700", "-07:00", "Z07:00", ".000", ".999", "Mon", "Monday", "002", "-", ":", " ", "T", "/"];
    for _ in 0..1500 {
        let n = rng.range(1, 6);
        let layout: String = (0..n).map(|_| *rng.pick(&pieces)).collect();
        // Format a random time with the layout, then parse it back.
        let secs = rng.range(-2_000_000_000, 4_000_000_000);
        let v = helm_rs::funcs::sprig::time::Time::from_unix(secs, helm_rs::funcs::sprig::time::Location::Utc).format(&layout);
        t.add("mustToDate", vec![s(&layout), s(&v)]);
        let mut garbled = v.clone();
        if !garbled.is_empty() {
            let k = rng.below(garbled.len() as u64) as usize;
            if garbled.is_char_boundary(k) && garbled.is_char_boundary(k + 1) {
                garbled.replace_range(k..k + 1, "9");
            }
        }
        t.add("mustToDate", vec![s(&layout), s(&garbled)]);
        t.add("date", vec![s(&layout), i(secs)]);
    }
    t.run("parse_random");
}

#[test]
fn durations() {
    let mut t = Batch::new();
    let inputs: Vec<Arg> = vec![
        s("1h"),
        s("1h2m3.5s"),
        s("-1.5h"),
        s("300ms"),
        s("1us"),
        s("1µs"),
        s("1μs"),
        s("2.5"),
        s(" 10 "),
        s(""),
        s("abc"),
        s("1d"),
        s("9999999999h"),
        s(".5s"),
        s("1.s"),
        s("0"),
        s("+3m"),
        s("1e3"),
        s("NaN"),
        s("Inf"),
        s("1e300"),
        s("9223372036.854775807"),
        i(0),
        i(90),
        i(-5),
        i(9_223_372_037),
        i(i64::MAX),
        i64a(42),
        fl_arg(1.5),
        fl_arg(-0.25),
        fl_arg(9.3e9),
        fl_arg(f64::NAN),
        Arg::new(Value::Uint64(5), "(semver \"5.0.0\").Major"),
        Arg::new(Value::Uint64(u64::MAX), "(semver \"18446744073709551615.0.0\").Major"),
        Arg::new(Value::Bool(true), "true"),
        Arg::new(Value::Nil, "nil"),
        list_arg(vec![]),
    ];
    for x in &inputs {
        for f in [
            "mustToDuration",
            "durationSeconds",
            "durationMilliseconds",
            "durationMicroseconds",
            "durationNanoseconds",
            "durationMinutes",
            "durationHours",
            "durationDays",
            "durationWeeks",
            "duration",
            "durationRound",
        ] {
            t.add(f, vec![x.clone()]);
        }
        for m in [s("1h"), s("15m"), s("0"), s("-1s"), s("bad"), i(7)] {
            t.add("durationRoundTo", vec![x.clone(), m.clone()]);
            t.add("durationTruncateTo", vec![x.clone(), m]);
        }
    }
    for n in [0i64, 1, 59, 60, 61, 3600, 86400, 86401, 2_592_000, 31_536_000, 31_536_001, -61, i64::MAX / 2] {
        t.add("duration", vec![i64a(n)]);
        t.add("duration", vec![s(&n.to_string())]);
        t.add("durationRound", vec![i64a(n.wrapping_mul(1_000_000_000))]);
    }
    for (d, m) in [("1h2m3.5s", "Hours"), ("1h2m3.5s", "Minutes"), ("1h2m3.5s", "Seconds"), ("1h2m3.5s", "Milliseconds"), ("-90s", "Abs"), ("1h2m3.5s", "String"), ("1h2m3.5s", "Nanoseconds")] {
        t.add_method("mustToDuration", vec![s(d)], m, vec![]);
    }
    t.add_method("mustToDuration", vec![s("1h2m3.5s")], "Round", vec![i(60_000_000_000)]);
    t.add_method("mustToDuration", vec![s("1h2m3.5s")], "Truncate", vec![i(60_000_000_000)]);
    t.run("durations");
}

fn fl_arg(f: f64) -> Arg {
    float_arg(f)
}

#[test]
fn now_and_ago_shapes() {
    // Nondeterministic in Helm: check shapes only.
    utc_oracle();
    let Some(out) = helm_eval_many(
        &["now | date \"2006\"".to_string(), "ago (now)".to_string(), "now | typeOf".to_string()],
        "",
    ) else {
        return;
    };
    let out: Vec<String> = out.into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(out[0].len(), 4);
    assert_eq!(out[1], "0s");
    assert_eq!(out[2], "time.Time");
    let funcs = helm_rs::funcs::helm();
    let now = call(&funcs, "now", &[]).unwrap();
    assert_eq!(now.type_name(), "time.Time");
    assert_eq!(render_value(&call(&funcs, "ago", &[now]).unwrap()), "0s");
}
