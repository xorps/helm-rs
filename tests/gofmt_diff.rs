//! Differential tests: `helm_rs::gofmt` vs Go's `fmt` (through Helm's
//! `printf`/`print`/`println` template builtins).

mod diffkit;

use diffkit::*;
use helm_rs::Value;
use helm_rs::gofmt;

fn check_printf(cases: &[(String, Vec<Arg>)], name: &str) {
    let mut exprs = Vec::new();
    let mut labels = Vec::new();
    let mut ours = Vec::new();
    for (f, args) in cases {
        let mut e = format!("printf {}", lit(f));
        for a in args {
            e.push(' ');
            e.push_str(&a.tmpl);
        }
        let vals: Vec<Value> = args.iter().map(|a| a.value.clone()).collect();
        ours.push(Ok(gofmt::sprintf(f, &vals)));
        labels.push(e.clone());
        exprs.push(e);
    }
    let mut bad = Vec::new();
    for chunk in (0..exprs.len()).collect::<Vec<_>>().chunks(400) {
        let (a, b) = (chunk[0], chunk[chunk.len() - 1] + 1);
        let Some(helm) = helm_eval_many(&exprs[a..b], "") else { return };
        bad.extend(compare(&labels[a..b], &ours[a..b], &helm));
    }
    report(name, exprs.len(), &bad);
}

#[test]
fn printf_fixed_cases() {
    let fl = |f: f64| float_arg(f);
    let s = |x: &str| str_arg(x);
    let i = |x: i64| int_arg(x);
    let mut cases: Vec<(String, Vec<Arg>)> = Vec::new();
    let mut add = |f: &str, a: Vec<Arg>| cases.push((f.to_string(), a));
    add("%v %v %v", vec![fl(1e6), fl(1.5e-7), fl(123456789.0)]);
    add("%v|%v|%v|%v", vec![fl(0.1), fl(1e21), fl(1e20), fl(-0.0)]);
    add("%g %G %e %E %f %F", vec![fl(1234.5678), fl(1234.5678), fl(1234.5678), fl(1234.5678), fl(1234.5678), fl(1234.5678)]);
    add("%.2f %.0f %.3e %.10g %8.3f %-8.3f| %+.1f %08.3f % f", vec![fl(0.125), fl(2.5), fl(1.0005), fl(1.0/3.0), fl(3.14159), fl(3.14159), fl(2.0), fl(-3.5), fl(1.0)]);
    add("%#v %#g %#.3g %#e %#f %#x", vec![fl(1.0), fl(1.0), fl(2.0), fl(1.0), fl(1.0), fl(1.0)]);
    add("%x %X %b %.3x", vec![fl(1.0), fl(-3.75), fl(1.0), fl(1.0/3.0)]);
    add("%d %5d %-5d| %05d %+d % d %x %X %o %O %b %#x %#o %#b %c %q %U %#U", vec![i(42), i(42), i(42), i(-42), i(42), i(42), i(255), i(255), i(8), i(8), i(5), i(255), i(8), i(5), i(0x1F600), i(65), i(0x1F600), i(0x1F600)]);
    add("%.3d %.0d|%5.0d|%08.3d", vec![i(5), i(0), i(0), i(7)]);
    add("%s %q %x %X % x %#x %# x %10s|%-10s|%.2s %5.1q", vec![s("hi"), s("a\"b\n"), s("hi"), s("hi"), s("hi"), s("hi"), s("hi"), s("hi"), s("hi"), s("héllo"), s("xyz")]);
    add("%#q %+q %q", vec![s("back`tick"), s("日本"), s("\u{7f}\u{0}")]);
    add("%d %s %v", vec![s("x"), i(3), Arg::new(Value::Nil, "nil")]);
    add("%t %t %v", vec![Arg::new(Value::Bool(true), "true"), i(1), Arg::new(Value::Bool(false), "false")]);
    add("%v %d %s", vec![list_arg(vec![i(1), s("a"), Arg::new(Value::Nil, "nil")]), list_arg(vec![i(1), s("a")]), list_arg(vec![i(1), s("a")])]);
    add("%v %+v %#v %x", vec![
        dict_arg(vec![("b".into(), i(2)), ("a".into(), list_arg(vec![s("x"), s("y")]))]),
        dict_arg(vec![("a".into(), i(1))]),
        dict_arg(vec![("a".into(), i(1)), ("n".into(), Arg::new(Value::Nil, "nil"))]),
        dict_arg(vec![("ab".into(), s("cd"))]),
    ]);
    add("%#v %#v %#v %#v", vec![list_arg(vec![i(1), s("a")]), s("q"), i(3), fl(2.5)]);
    add("%T %T %T %T %T %T %T", vec![i(1), fl(1.0), s("a"), list_arg(vec![]), dict_arg(vec![]), Arg::new(Value::Nil, "nil"), Arg::new(Value::Bool(true), "true")]);
    add("%d %d", vec![i(1)]);
    add("%d", vec![i(1), i(2), s("x")]);
    add("%[2]d %[1]d", vec![i(1), i(2)]);
    add("%[3]d %[0]d %[x]d", vec![i(1), i(2)]);
    add("%[2]*[1]d|%-*d|%*d", vec![i(12), i(5), i(-5), i(1), s("x"), i(2)]);
    add("%.*f|%.*f", vec![i(2), fl(3.14159), i(-1), fl(1.0)]);
    add("%", vec![]);
    add("%!|%z|%-", vec![i(1), i(2)]);
    add("%v %v", vec![Arg::new(Value::Int64(-5), "(int64 -5)"), Arg::new(Value::Uint64(7), "(semver \"7.0.0\").Major")]);
    add("%#v %x %v", vec![Arg::new(Value::Uint64(7), "(semver \"7.0.0\").Major"), Arg::new(Value::Uint64(255), "(semver \"255.0.0\").Major"), fl(f64::NAN)]);
    add("%5v|%-6v|%05v|%+v|% v", vec![fl(f64::INFINITY), fl(f64::NEG_INFINITY), fl(f64::NAN), fl(f64::NAN), fl(f64::NAN)]);
    add("%010.3f|%-010.3f|%+010.3f|%x", vec![fl(-1.5), fl(-1.5), fl(1.5), fl(f64::INFINITY)]);
    add("%v %s %d", vec![
        Arg::new(Value::object(helm_rs::funcs::duration::Duration(3_723_500_000_000)), "(mustToDuration \"1h2m3.5s\")"),
        Arg::new(Value::object(helm_rs::funcs::duration::Duration(1500)), "(mustToDuration \"1.5us\")"),
        Arg::new(Value::object(helm_rs::funcs::duration::Duration(1500)), "(mustToDuration \"1.5us\")"),
    ]);
    add("%t|%v", vec![
        Arg::new(Value::object(helm_rs::funcs::duration::Duration(-1)), "(mustToDuration \"-1ns\")"),
        list_arg(vec![Arg::new(Value::object(helm_rs::funcs::duration::Duration(60_000_000_000)), "(mustToDuration \"1m\")")]),
    ]);
    add("%6.2v|%.1v|%10.4v", vec![s("hello"), list_arg(vec![s("abc"), fl(1.25)]), fl(3.14159265)]);
    add("%e %e %e", vec![fl(1e100), fl(1e-100), fl(5e-324)]);
    add("%.30f|%.20e|%.40g", vec![fl(0.1), fl(1.0/3.0), fl(2.0f64.sqrt())]);
    add("%v %v %v %v", vec![fl(1e-5), fl(1e-4), fl(123456.0), fl(1234567.0)]);
    add("%v %v", vec![fl(f64::MAX), fl(f64::MIN_POSITIVE)]);
    add("%c|%c|%q|%U", vec![i(-1), i(0x110000), i(0xD800), i(-1)]);
    add("%x|%X|% X|%#X", vec![s(""), s("\u{ff}"), s("ab"), s("ab")]);
    add("%10x|%-10x|%010x", vec![s("ab"), s("ab"), s("ab")]);
    add("%s|%10.3s|%-4s|%04s", vec![s("日本語日本語"), s("日本語日本語"), s("é"), s("ab")]);
    check_printf(&cases, "printf_fixed_cases");
}

fn rand_format(rng: &mut Rng, nargs: usize) -> String {
    let mut f = String::new();
    let n = rng.range(1, 4);
    for _ in 0..n {
        if rng.chance(1, 3) {
            f.push_str(rng.pick(&["a", " ", "x=", "-", "日", "[", "]", "*", "."]));
        }
        f.push('%');
        let nflags = rng.below(3);
        for _ in 0..nflags {
            f.push(*rng.pick(&['#', '0', '+', '-', ' ']));
        }
        if rng.chance(1, 8) {
            f.push_str(&format!("[{}]", rng.below(nargs as u64 + 2)));
        }
        match rng.below(6) {
            0 => f.push('*'),
            1 | 2 => f.push_str(&rng.range(0, 20).to_string()),
            _ => {}
        }
        if rng.chance(1, 3) {
            f.push('.');
            match rng.below(4) {
                0 => f.push('*'),
                1 => {}
                _ => f.push_str(&rng.range(0, 25).to_string()),
            }
        }
        if rng.chance(1, 10) {
            f.push_str(&format!("[{}]", rng.below(nargs as u64 + 2)));
        }
        if rng.chance(1, 40) {
            break; // NOVERB
        }
        f.push(*rng.pick(&[
            'v', 'v', 'v', 'd', 'd', 's', 's', 'q', 'x', 'X', 'f', 'e', 'E', 'g', 'G', 'F', 'b', 'o', 'O', 'c', 'U', 't', 'T',
            '%', 'z', 'w', 'é',
        ]));
    }
    f
}

#[test]
fn printf_random() {
    let mut rng = Rng::new(20240611);
    let mut cases = Vec::new();
    for _ in 0..4000 {
        let nargs = rng.below(4) as usize;
        let args: Vec<Arg> = (0..nargs).map(|_| rand_arg(&mut rng, 2)).collect();
        let f = rand_format(&mut rng, nargs);
        cases.push((f, args));
    }
    check_printf(&cases, "printf_random");
}

#[test]
fn printf_random_floats() {
    let mut rng = Rng::new(77);
    let mut cases = Vec::new();
    for _ in 0..4000 {
        let f = rand_float(&mut rng);
        let verb = *rng.pick(&['v', 'e', 'E', 'f', 'g', 'G', 'x', 'b']);
        let fmt = match rng.below(4) {
            0 => format!("%{verb}"),
            1 => format!("%.{}{verb}", rng.range(0, 25)),
            2 => format!("%#{}.{}{verb}", rng.range(0, 12), rng.range(0, 25)),
            _ => format!("%+0{}{verb}", rng.range(0, 30)),
        };
        cases.push((fmt, vec![float_arg(f)]));
    }
    check_printf(&cases, "printf_random_floats");
}

#[test]
fn print_and_println_random() {
    let mut rng = Rng::new(99);
    let mut exprs = Vec::new();
    let mut labels = Vec::new();
    let mut ours = Vec::new();
    for _ in 0..1500 {
        let nargs = rng.below(5) as usize;
        let args: Vec<Arg> = (0..nargs).map(|_| rand_arg(&mut rng, 2)).collect();
        let vals: Vec<Value> = args.iter().map(|a| a.value.clone()).collect();
        let ln = rng.chance(1, 2);
        let mut e = String::from(if ln { "println" } else { "print" });
        for a in &args {
            e.push(' ');
            e.push_str(&a.tmpl);
        }
        ours.push(Ok(if ln { gofmt::sprintln(&vals) } else { gofmt::sprint(&vals) }));
        labels.push(e.clone());
        exprs.push(e);
    }
    let Some(helm) = helm_eval_many(&exprs, "") else { return };
    let bad = compare(&labels, &ours, &helm);
    report("print_and_println_random", exprs.len(), &bad);
}

#[test]
fn format_float_matches_strconv() {
    // A few direct checks of strconv.FormatFloat.
    assert_eq!(gofmt::format_float(1e6, b'g', -1), "1e+06");
    assert_eq!(gofmt::format_float(1.5e-7, b'g', -1), "1.5e-07");
    assert_eq!(gofmt::format_float(100.0, b'f', -1), "100");
    assert_eq!(gofmt::format_float(0.125, b'f', 2), "0.12");
    assert_eq!(gofmt::format_float(f64::NAN, b'g', -1), "NaN");
    assert_eq!(gofmt::format_float(f64::INFINITY, b'g', -1), "+Inf");
    assert_eq!(gofmt::format_float(-0.0, b'g', -1), "-0");
}
