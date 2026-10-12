//! Differential tests: Sprig functions (`helm_rs::funcs::sprig()`) vs real
//! Helm. Each case calls a function directly with the argument values a
//! template would pass, and compares with Helm rendering `{{ fn args }}`.

mod diffkit;

use diffkit::*;
use helm_rs::{FuncMap, Value};

struct Batch {
    funcs: FuncMap,
    exprs: Vec<String>,
    ours: Vec<Result<String, String>>,
}

/// How text/template prints a pipeline result.
fn print_result(v: &Value) -> String {
    if v.is_nil() { String::new() } else { render_value(v) }
}

impl Batch {
    fn new() -> Batch {
        Batch { funcs: helm_rs::funcs::sprig(), exprs: Vec::new(), ours: Vec::new() }
    }
    fn add(&mut self, name: &str, args: Vec<Arg>) {
        let mut e = String::from(name);
        for a in &args {
            e.push(' ');
            e.push_str(&a.tmpl);
        }
        let vals: Vec<Value> = args.iter().map(|a| a.value.deep_clone()).collect();
        let r = call(&self.funcs, name, &vals).map(|v| print_result(&v));
        self.exprs.push(e);
        self.ours.push(r);
    }
    /// Adds an arbitrary template expression with a precomputed result.
    fn add_raw(&mut self, expr: &str, ours: Result<String, String>) {
        self.exprs.push(expr.to_string());
        self.ours.push(ours);
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
fn fl(x: f64) -> Arg {
    float_arg(x)
}
fn nil() -> Arg {
    Arg::new(Value::Nil, "nil")
}
fn b(x: bool) -> Arg {
    Arg::new(Value::Bool(x), if x { "true" } else { "false" })
}
fn l(items: Vec<Arg>) -> Arg {
    list_arg(items)
}
fn d(items: Vec<(&str, Arg)>) -> Arg {
    dict_arg(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}
fn i64a(x: i64) -> Arg {
    Arg::new(Value::Int64(x), format!("(int64 {})", lit(&x.to_string())))
}

const SAMPLE_STRINGS: &[&str] = &[
    "",
    " ",
    "a",
    "hello world",
    "  padded  ",
    "Hello World",
    "foo_bar_baz",
    "FooBarBaz",
    "fooBarBaz",
    "foo-bar",
    "HTTPServer",
    "XMLHttpRequest",
    "a1b2c3",
    "123abc",
    "abc123def",
    "version2Release10",
    "ID",
    "userID",
    "日本語",
    "ǅemal ǆungla",
    "ß straße",
    "İstanbul",
    "éàü ÉÀÜ",
    "tab\there",
    "line1\nline2\nline3",
    "trailing\n",
    "\u{a0}nbsp\u{85}",
    "a.b.c",
    "a,b,,c",
    "x--y__z",
    "--leading",
    "trailing__",
    "Ⅻ roman",
    "١٢٣ arabic digits",
    "emoji 😀 here",
    "MiXeD cAsE wOrDs",
    "o'neil mc-donald",
    "what?!",
    "snake_Case_Mixed",
    "ALLCAPS",
    "a  b   c",
    "The quick brown fox jumps over the lazy dog",
    "http://example.com/path?q=1#frag",
    "\u{FFFD}bad",
    "ǈ title",
    "ᾈ greek",
];

#[test]
fn string_functions() {
    let mut t = Batch::new();
    for &x in SAMPLE_STRINGS {
        for f in [
            "trim", "upper", "lower", "title", "untitle", "nospace", "initials", "swapcase", "snakecase", "camelcase",
            "kebabcase", "quote", "squote", "sha1sum", "sha256sum", "sha512sum", "adler32sum", "toString", "b64enc",
            "b32enc", "b64dec", "b32dec", "base", "dir", "clean", "ext", "isAbs", "osBase", "regexQuoteMeta", "atoi",
            "int", "int64", "float64", "toDecimal", "toStrings",
        ] {
            t.add(f, vec![s(x)]);
        }
        for n in [-5i64, -1, 0, 1, 3, 4, 5, 7, 10, 100] {
            t.add("trunc", vec![i(n), s(x)]);
            t.add("abbrev", vec![i(n), s(x)]);
            t.add("wrap", vec![i(n), s(x)]);
            t.add("wrapWith", vec![i(n), s("<br>"), s(x)]);
            t.add("indent", vec![i(n.max(0)), s(x)]);
            t.add("nindent", vec![i(n.max(0)), s(x)]);
            t.add("repeat", vec![i(n.max(0) % 4), s(x)]);
            t.add("plural", vec![s("one"), s("many"), i(n)]);
            t.add("splitn", vec![s(" "), i(n), s(x)]);
            t.add("splitn", vec![s(""), i(n), s(x)]);
        }
        for (a, bb) in [(0i64, 4i64), (0, 7), (5, 7), (5, 10), (10, 20), (-1, 10), (3, 4)] {
            t.add("abbrevboth", vec![i(a), i(bb), s(x)]);
        }
        for (a, bb) in [(0i64, 0i64), (0, 3), (1, 2), (2, 1), (-1, 2), (1, -1), (0, 100), (-1, -1), (3, 3), (50, 60)] {
            t.add("substr", vec![i(a), i(bb), s(x)]);
        }
        for sep in ["", " ", ",", "o", "ll"] {
            t.add("split", vec![s(sep), s(x)]);
            t.add("splitList", vec![s(sep), s(x)]);
            t.add("join", vec![s(sep), l(vec![s(x), s("b"), i(1), nil()])]);
            t.add("replace", vec![s(sep), s("_"), s(x)]);
            t.add("contains", vec![s(sep), s(x)]);
            t.add("hasPrefix", vec![s(sep), s(x)]);
            t.add("hasSuffix", vec![s(sep), s(x)]);
            t.add("trimAll", vec![s(sep), s(x)]);
            t.add("trimall", vec![s(sep), s(x)]);
            t.add("trimPrefix", vec![s(sep), s(x)]);
            t.add("trimSuffix", vec![s(sep), s(x)]);
        }
        t.add("cat", vec![s(x), nil(), i(1), fl(2.5), l(vec![s("a")])]);
        t.add("quote", vec![s(x), nil(), i(5), b(true)]);
        t.add("squote", vec![s(x), nil(), i(5), l(vec![s("q")])]);
        t.add("regexMatch", vec![s("^[a-z]+$"), s(x)]);
        t.add("regexFind", vec![s("[A-Z][a-z]*"), s(x)]);
        t.add("regexFindAll", vec![s("[a-z]"), s(x), i(-1)]);
        t.add("regexFindAll", vec![s("o*"), s(x), i(3)]);
        t.add("regexReplaceAll", vec![s("(\\w)(\\w*)"), s(x), s("$2$1-${1}x$$")]);
        t.add("regexReplaceAllLiteral", vec![s("\\s+"), s(x), s("$1")]);
        t.add("regexSplit", vec![s("[ ,.]+"), s(x), i(-1)]);
        t.add("regexSplit", vec![s(""), s(x), i(3)]);
    }
    t.add("hello", vec![]);
    t.add("sortAlpha", vec![l(vec![s("b"), s("a"), s("C"), i(1), nil()])]);
    t.add("sortAlpha", vec![s("x")]);
    t.add("sortAlpha", vec![nil()]);
    t.add("toStrings", vec![l(vec![i(1), fl(2.5), nil(), b(true), s("x")])]);
    t.add("toStrings", vec![i(5)]);
    t.add("toStrings", vec![nil()]);
    t.add("repeat", vec![i(-1), s("x")]);
    t.add("indent", vec![i(-1), s("x")]);
    t.add("b64dec", vec![s("aGVsbG8=")]);
    t.add("b64dec", vec![s("aGVsbG8")]);
    t.add("b64dec", vec![s("aGVs\nbG8=")]);
    t.add("b64dec", vec![s("aGVsbG8=x")]);
    t.add("b64dec", vec![s("a===")]);
    t.add("b32dec", vec![s("NBSWY3DP")]);
    t.add("b32dec", vec![s("NBSWY3D")]);
    t.add("b32dec", vec![s("NBSWY===")]);
    t.add("b32dec", vec![s("NB======")]);
    t.add("b32dec", vec![s("N=======")]);
    t.run("string_functions");
}

#[test]
fn numeric_functions() {
    let mut t = Batch::new();
    let nums = vec![
        i(0),
        i(1),
        i(-7),
        i(42),
        fl(2.5),
        fl(-2.5),
        fl(1e20),
        fl(0.1),
        fl(f64::NAN),
        fl(f64::INFINITY),
        s("12"),
        s("1.0"),
        s("1.5"),
        s("0x1F"),
        s("017"),
        s("1_000"),
        s("abc"),
        s(" 3"),
        s("1e3"),
        s("9223372036854775808"),
        b(true),
        nil(),
        i64a(-3),
        Arg::new(Value::Uint64(u64::MAX), "(semver \"18446744073709551615.0.0\").Major"),
    ];
    for x in &nums {
        for f in ["int", "int64", "float64", "add1", "add1f", "floor", "ceil", "toDecimal", "empty", "toString"] {
            t.add(f, vec![x.clone()]);
        }
        for y in &nums {
            for f in ["add", "sub", "mul", "div", "mod", "max", "min", "maxf", "minf", "addf", "subf", "mulf", "divf"] {
                t.add(f, vec![x.clone(), y.clone()]);
            }
        }
        for p in [0i64, 1, 2, -1] {
            t.add("round", vec![x.clone(), i(p)]);
            t.add("round", vec![x.clone(), i(p), fl(0.3)]);
        }
    }
    t.add("add", vec![]);
    t.add("addf", vec![]);
    t.add("mul", vec![i(3)]);
    t.add("biggest", vec![i(1), i(5), i(3)]);
    t.add("addf", vec![fl(0.1), fl(0.2), fl(0.3)]);
    t.add("mulf", vec![fl(1.1), fl(1.1), fl(1.1)]);
    t.add("divf", vec![i(1), i(3)]);
    t.add("divf", vec![i(-2), i(3)]);
    t.add("divf", vec![i(2), i(-3)]);
    t.add("divf", vec![fl(1e300), fl(1e-300)]);
    t.add("subf", vec![fl(1e20), fl(1e-20)]);
    for n in [-5i64, -1, 0, 1, 5] {
        t.add("until", vec![i(n)]);
        t.add("seq", vec![i(n)]);
        for m in [-3i64, 0, 2, 7] {
            t.add("seq", vec![i(n), i(m)]);
            for st in [-2i64, -1, 0, 1, 3] {
                t.add("untilStep", vec![i(n), i(m), i(st)]);
                t.add("seq", vec![i(n), i(st), i(m)]);
            }
        }
    }
    t.add("seq", vec![]);
    t.add("seq", vec![i(1), i(2), i(3), i(4)]);
    for x in ["12", "-12", "+5", "", "-", "1.5", "99999999999999999999", "-99999999999999999999", "0x10"] {
        t.add("atoi", vec![s(x)]);
    }
    t.run("numeric_functions");
}

#[test]
fn defaults_and_reflection() {
    let mut t = Batch::new();
    let vals = vec![
        nil(),
        i(0),
        i(1),
        fl(0.0),
        fl(-0.0),
        fl(f64::NAN),
        s(""),
        s("x"),
        b(false),
        b(true),
        l(vec![]),
        l(vec![nil()]),
        d(vec![]),
        d(vec![("a", i(1))]),
        i64a(0),
        Arg::new(Value::Uint64(0), "(semver \"0.0.0\").Major"),
        Arg::new(Value::Nil, "(semver \"1.2.3\")").with_ours_value(),
        Arg::new(Value::Nil, "(mustToDuration \"0s\")").with_ours_value(),
        Arg::new(Value::Nil, "(mustToDuration \"1s\")").with_ours_value(),
    ];
    for x in &vals {
        for f in ["empty", "typeOf", "kindOf", "toString", "deepCopy", "toJson", "toPrettyJson", "toRawJson"] {
            if f.contains("Json") {
                continue; // JSON is implemented by another module.
            }
            t.add(f, vec![x.clone()]);
        }
        t.add("default", vec![s("dflt"), x.clone()]);
        t.add("default", vec![s("dflt")]);
        t.add("ternary", vec![s("yes"), s("no"), b(!x.value.is_nil())]);
        t.add("coalesce", vec![nil(), x.clone(), s("z")]);
        t.add("all", vec![i(1), x.clone()]);
        t.add("any", vec![i(0), x.clone()]);
        for k in ["int", "string", "map", "slice", "invalid", "float64", "ptr", "struct", "int64"] {
            t.add("kindIs", vec![s(k), x.clone()]);
        }
        for ty in ["int", "string", "[]interface {}", "map[string]interface {}", "semver.Version", "*semver.Version", "<nil>"] {
            t.add("typeIs", vec![s(ty), x.clone()]);
            t.add("typeIsLike", vec![s(ty), x.clone()]);
        }
        for y in &vals {
            t.add("deepEqual", vec![x.clone(), y.clone()]);
        }
    }
    t.add("all", vec![]);
    t.add("any", vec![]);
    t.add("coalesce", vec![]);
    t.run("defaults_and_reflection");
}

#[test]
fn list_functions() {
    let mut t = Batch::new();
    let lists = vec![
        l(vec![]),
        l(vec![i(1)]),
        l(vec![i(1), i(2), i(3), i(4), i(5)]),
        l(vec![s("a"), nil(), i(0), s(""), l(vec![]), s("a"), i(1), fl(1.0), i(1)]),
        l(vec![d(vec![("a", i(1))]), d(vec![("a", i(1))]), d(vec![("a", i(2))])]),
        s("notalist"),
        i(5),
        nil(),
        d(vec![("k", s("v"))]),
    ];
    for x in &lists {
        for f in [
            "first", "mustFirst", "last", "rest", "initial", "reverse", "compact", "uniq", "mustUniq", "mustReverse",
            "mustRest", "mustLast", "mustInitial", "mustCompact",
        ] {
            t.add(f, vec![x.clone()]);
        }
        t.add("append", vec![x.clone(), s("z")]);
        t.add("mustPush", vec![x.clone(), nil()]);
        t.add("prepend", vec![x.clone(), i(0)]);
        t.add("without", vec![x.clone(), i(1), s("a")]);
        t.add("has", vec![i(1), x.clone()]);
        t.add("has", vec![s("a"), x.clone()]);
        t.add("mustHas", vec![nil(), x.clone()]);
        t.add("concat", vec![x.clone(), l(vec![s("x")])]);
        t.add("slice", vec![x.clone()]);
        for (a, bb) in [(0i64, 1i64), (1, 3), (2, 2), (3, 1), (-1, 2), (0, 9)] {
            t.add("slice", vec![x.clone(), i(a), i(bb)]);
            t.add("slice", vec![x.clone(), i(a)]);
        }
        t.add("slice", vec![x.clone(), s("1"), fl(2.0)]);
        for n in [-2i64, -1, 0, 1, 2, 3, 10] {
            t.add("chunk", vec![i(n), x.clone()]);
        }
        t.add("join", vec![s("-"), x.clone()]);
        t.add("sortAlpha", vec![x.clone()]);
    }
    t.add("list", vec![]);
    t.add("tuple", vec![i(1), s("a"), nil()]);
    t.add("concat", vec![]);
    t.add("concat", vec![l(vec![]), l(vec![])]);
    t.run("list_functions");
}

#[test]
fn dict_functions() {
    let mut t = Batch::new();
    let dd = || d(vec![("a", i(1)), ("b", d(vec![("c", s("x")), ("e", l(vec![i(1)]))])), ("n", nil()), ("z", s(""))]);
    t.add("dict", vec![]);
    t.add("dict", vec![s("a")]);
    t.add("dict", vec![s("a"), i(1), i(2), s("x"), nil(), b(true), s("a"), s("dup")]);
    t.add("dict", vec![l(vec![i(1)]), i(1), fl(1.5), i(2)]);
    for k in ["a", "b", "n", "missing", ""] {
        t.add("get", vec![dd(), s(k)]);
        t.add("hasKey", vec![dd(), s(k)]);
        t.add("set", vec![dd(), s(k), s("new")]);
        t.add("unset", vec![dd(), s(k)]);
        t.add("pluck", vec![s(k), dd(), d(vec![(k, s("second"))]), d(vec![])]);
        t.add("pick", vec![dd(), s(k), s("a")]);
        t.add("omit", vec![dd(), s(k), s("a")]);
        t.add("dig", vec![s(k), s("default"), dd()]);
        t.add("dig", vec![s("b"), s(k), s("default"), dd()]);
        t.add("dig", vec![s(k), s("c"), s("default"), dd()]);
    }
    t.add("dig", vec![s("a"), dd()]);
    t.add("dig", vec![s("a"), s("d"), s("notamap")]);
    t.add("dig", vec![i(1), s("d"), dd()]);
    t.add("keys", vec![d(vec![("q", i(1))]), d(vec![("a", i(2))])]);
    t.add_raw("sortAlpha (keys (dict \"b\" 1 \"a\" 2) (dict \"c\" 1))", Ok("[a b c]".into()));
    t.add("keys", vec![]);
    t.add("values", vec![d(vec![("k", s("v"))])]);
    let srcs: Vec<Arg> = vec![
        d(vec![]),
        d(vec![("a", i(2)), ("new", s("n"))]),
        d(vec![("a", i(0)), ("z", s("filled")), ("n", s("was nil"))]),
        d(vec![("b", d(vec![("c", s("y")), ("d", s("added")), ("e", l(vec![i(2), i(3)]))]))]),
        d(vec![("b", s("scalar")), ("a", nil())]),
        d(vec![("b", l(vec![])), ("e", l(vec![i(9)]))]),
        d(vec![("b", d(vec![]))]),
        d(vec![("a", d(vec![("x", i(1))])), ("z", d(vec![("x", i(1))]))]),
        d(vec![("z", l(vec![i(1)])), ("a", l(vec![]))]),
    ];
    for f in ["merge", "mergeOverwrite", "mustMerge", "mustMergeOverwrite"] {
        for s1 in &srcs {
            t.add(f, vec![dd(), s1.clone()]);
            for s2 in &srcs {
                t.add(f, vec![dd(), s1.clone(), s2.clone()]);
                t.add(f, vec![s1.clone(), s2.clone()]);
            }
        }
        t.add(f, vec![dd()]);
    }
    t.run("dict_functions");
}

#[test]
fn semver_functions() {
    let mut t = Batch::new();
    let versions = [
        "1.2.3", "v1.2.3", "1.2", "1", "v1", "1.2.3-alpha", "1.2.3-alpha.1", "1.2.3-0", "1.2.3+build", "1.2.3-beta+exp.sha",
        "01.2.3", "1.02.3", "1.2.3-01", "1.2.3-", "1.2.3+", "1.2.3.4", "x.y.z", "", "v", "1.2.3-a..b", "1.2.3+a..b",
        "99999999999999999999.0.0", "1.21.0-0", "1.21.0", "1.20.15", "0.0.1", "0.1.0", "2.0.0-rc.1", "1.2.3-alpha.beta.10",
        "1.2.3-alpha.beta.9", "1.2.3-x-y-z.-", "1.2.3+b.0001",
    ];
    let constraints = [
        ">=1.2.3", ">1.2", "<2", "<=1.2.3", "=1.2.3", "1.2.3", "!=1.2.3", "~1.2.3", "~1.2", "~1", "^1.2.3", "^0.2.3",
        "^0.0.3", "^0", "^0.0", "1.2.x", "1.x", "*", "x", ">=1.21.0-0", ">=1.21.0", ">= 1.2, < 2.0", ">=1.2 <2.0",
        "1.2 - 1.4.5", "1.2.3 - 2", ">1 || <0.5", "~>1.2", "=>1.2", "=<1.2", "", " ", ">=a", "1.2.3-alpha", ">1.2.3-alpha",
        "!=1.2", "!=1.2.x", "<1.2.x", ">1.2.x", "<=1.x", ">1.x", "~0", "~0.0", "^1.x", ">=1.2.3,<1.3", "1 || 2 || 3",
        "|| 1", ">= 1.2.3 ||", "1.2.3 -", "v1.2.3", ">=v1.2", "1.2.*", "1.X", ">1.2.3+build",
    ];
    for v in versions {
        t.add("semver", vec![s(v)]);
        for f in ["Major", "Minor", "Patch", "Prerelease", "Metadata", "Original", "String"] {
            let expr = format!("(semver {}).{f}", lit(v));
            let ours = match helm_rs::semver::Version::parse(v) {
                Ok(ver) => Ok(match f {
                    "Major" => ver.major.to_string(),
                    "Minor" => ver.minor.to_string(),
                    "Patch" => ver.patch.to_string(),
                    "Prerelease" => ver.pre.clone(),
                    "Metadata" => ver.metadata.clone(),
                    "Original" => ver.original.clone(),
                    _ => ver.to_string(),
                }),
                Err(e) => Err(e),
            };
            t.add_raw(&expr, ours);
        }
        for c in constraints {
            t.add("semverCompare", vec![s(c), s(v)]);
        }
    }
    t.run("semver_functions");
}

#[test]
fn semver_random() {
    let mut rng = Rng::new(4242);
    let mut t = Batch::new();
    let parts = ["0", "1", "2", "10", "x", "*", "X", "01"];
    let pres = ["", "-0", "-alpha", "-alpha.1", "-1", "-beta.2", "-rc.10", "-0.3.7", "-x.7.z.92"];
    let ops = ["", "=", "!=", ">", "<", ">=", "=>", "<=", "=<", "~", "~>", "^"];
    let gen_ver = |rng: &mut Rng, wild: bool| -> String {
        let n = rng.range(1, 3);
        let mut v = String::new();
        if rng.chance(1, 6) {
            v.push('v');
        }
        for k in 0..n {
            if k > 0 {
                v.push('.');
            }
            let p = if wild { *rng.pick(&parts) } else { *rng.pick(&parts[..4]) };
            v.push_str(p);
        }
        if rng.chance(1, 3) {
            v.push_str(rng.pick(&pres));
        }
        if rng.chance(1, 10) {
            v.push_str("+meta.1");
        }
        v
    };
    for _ in 0..3000 {
        let mut c = String::new();
        let groups = rng.range(1, 2);
        for g in 0..groups {
            if g > 0 {
                c.push_str(" || ");
            }
            let n = rng.range(1, 2);
            for k in 0..n {
                if k > 0 {
                    c.push_str(if rng.chance(1, 2) { ", " } else { " " });
                }
                if rng.chance(1, 10) {
                    c.push_str(&format!("{} - {}", gen_ver(&mut rng, true), gen_ver(&mut rng, true)));
                } else {
                    c.push_str(rng.pick(&ops));
                    if rng.chance(1, 4) {
                        c.push(' ');
                    }
                    c.push_str(&gen_ver(&mut rng, true));
                }
            }
        }
        let v = gen_ver(&mut rng, false);
        t.add("semverCompare", vec![s(&c), s(&v)]);
    }
    t.run("semver_random");
}

#[test]
fn regex_functions() {
    let mut t = Batch::new();
    let patterns = [
        "a+", "^a", "a$", "(?i)hello", "\\d+", "\\D", "\\s+", "\\w+", "\\W", "\\bfoo\\b", "[[:alpha:]]+", "[^a-z]",
        "(a)(b)?", "(?P<first>\\w+) (?P<last>\\w+)", "(?<x>.)", "a*?", "a{2,3}", "a{2}", "a{,3}", "x{1001}", "(a{1000}){2}",
        "\\pL+", "\\p{Greek}", "\\PL", "\\p{^Greek}", "\\p{greek}", "\\p{Foo}", "[\\d\\s]", "[\\D]", "(?i)[^k]", "(?s).",
        "(?m)^x$", "(?U)a+", "a**", "a+*", "*a", "(", ")", "[a", "[z-a]", "\\", "\\Z", "\\C", "\\Q.*\\E", "\\x41", "\\x{263a}",
        "\\101", "\\8", "(?x)a", "(?i-i)A", "(?-)", "(?P<>a)", "(?P<a!>a)", "a|*", "()", "(|a)", "x|", "^*", "$+", "\\b*",
        "[]a]", "[a-]", "[-a]", "[\\-]", "[\\]]", "[[:word:]]", "[[:foo:]]", "\\pN", "\\p", "\\p{", "[\\p{Lu}]", ".*",
        "", "é+", "(?i)é", "(?i)ſ", "(?i)k", "[[:^digit:]]", "a{1001}", "a{1000}", "(?:a)", "(?i:A)b", "\\.", "\\<", "\\>",
        "\\&", "\\ ", "\\_", "\\v\\f\\a", "[\\a]", "\\z", "\\A", "(a|ab)(c|bcd)(d*)", "a\\E", "\\Qab", "x*?y+?z??",
    ];
    let inputs = ["", "a", "aaa", "hello world foo", "Hello HELLO", "foo123bar 456", "ab\nax\nx", "k K \u{212a}", "Ω αβγ", "abcd"];
    for p in patterns {
        t.add("regexMatch", vec![s(p), s("aa")]);
        t.add("mustRegexMatch", vec![s(p), s("hello")]);
        t.add("regexQuoteMeta", vec![s(p)]);
        for x in inputs {
            t.add("mustRegexFindAll", vec![s(p), s(x), i(-1)]);
            t.add("mustRegexFind", vec![s(p), s(x)]);
            t.add("mustRegexReplaceAll", vec![s(p), s(x), s("<$0|$1|${first}|$last|$2x|$$>")]);
            t.add("mustRegexReplaceAllLiteral", vec![s(p), s(x), s("[$1]")]);
            t.add("mustRegexSplit", vec![s(p), s(x), i(-1)]);
            t.add("mustRegexSplit", vec![s(p), s(x), i(2)]);
            t.add("mustRegexFindAll", vec![s(p), s(x), i(1)]);
        }
        t.add("regexFind", vec![s(p), s("abc")]);
        t.add("regexFindAll", vec![s(p), s("abc"), i(0)]);
        t.add("regexSplit", vec![s(p), s("abc"), i(0)]);
        t.add("regexReplaceAll", vec![s(p), s("abc"), s("x")]);
    }
    t.run("regex_functions");
}

fn rand_regex(rng: &mut Rng, depth: u32) -> String {
    let atoms = [
        "a", "b", "ab", ".", "\\d", "\\w", "\\s", "[a-c]", "[^a]", "x", "é", "\\.", "(?i)a", "\\b", "^", "$", "[[:digit:]]",
        "\\pL", "k", "\\x41",
    ];
    let mut s = String::new();
    let n = rng.range(1, 4);
    for _ in 0..n {
        let atom = if depth > 0 && rng.chance(1, 4) {
            let inner = rand_regex(rng, depth - 1);
            match rng.below(4) {
                0 => format!("({inner})"),
                1 => format!("(?:{inner})"),
                2 => format!("({inner}|{})", rand_regex(rng, depth - 1)),
                _ => format!("(?P<n{}>{inner})", rng.below(3)),
            }
        } else {
            rng.pick(&atoms).to_string()
        };
        s.push_str(&atom);
        match rng.below(8) {
            0 => s.push('*'),
            1 => s.push('+'),
            2 => s.push('?'),
            3 => s.push_str("*?"),
            4 => s.push_str(&format!("{{{},{}}}", rng.below(3), rng.range(2, 4))),
            _ => {}
        }
    }
    s
}

#[test]
fn regex_random() {
    let mut rng = Rng::new(31337);
    let mut t = Batch::new();
    for _ in 0..1500 {
        let p = rand_regex(&mut rng, 2);
        let x = rand_string(&mut rng, 6);
        t.add("mustRegexFindAll", vec![s(&p), s(&x), i(-1)]);
        t.add("mustRegexReplaceAll", vec![s(&p), s(&x), s("<$1|${n0}>")]);
        t.add("mustRegexSplit", vec![s(&p), s(&x), i(-1)]);
    }
    t.run("regex_random");
}

#[test]
fn strings_random() {
    let mut rng = Rng::new(777);
    let mut t = Batch::new();
    for _ in 0..1500 {
        let x = rand_string(&mut rng, 6);
        for f in ["snakecase", "kebabcase", "camelcase", "title", "untitle", "swapcase", "initials", "nospace", "upper", "lower", "trim", "quote", "b64enc", "b32enc"] {
            t.add(f, vec![s(&x)]);
        }
        let n = rng.range(-3, 12);
        t.add("trunc", vec![i(n), s(&x)]);
        t.add("abbrev", vec![i(n), s(&x)]);
        t.add("wrap", vec![i(n), s(&x)]);
        t.add("wrapWith", vec![i(n), s("|"), s(&x)]);
        t.add("substr", vec![i(rng.range(-1, 5)), i(rng.range(-1, 12)), s(&x)]);
        t.add("b64dec", vec![s(&x)]);
        t.add("b32dec", vec![s(&x)]);
        t.add("clean", vec![s(&x)]);
        t.add("dir", vec![s(&x)]);
        t.add("base", vec![s(&x)]);
        t.add("ext", vec![s(&x)]);
    }
    t.run("strings_random");
}

#[test]
fn printf_like_and_misc() {
    let mut t = Batch::new();
    for x in ["1h", "1h2m3.5s", "-1.5h", "300ms", "1us", "1µs", "2.5", " 10 ", "", "abc", "1d", "9999999999h", ".5s", "1.s", "0", "+3m"] {
        t.add("duration", vec![s(x)]);
        t.add("durationRound", vec![s(x)]);
    }
    for n in [0i64, 1, 59, 60, 61, 3600, 86400, 86401, 2_592_000, 31_536_000, 31_536_001, -61, i64::MAX / 2] {
        t.add("duration", vec![i64a(n)]);
        t.add("duration", vec![i(n)]);
        t.add("durationRound", vec![i64a(n.wrapping_mul(1_000_000_000))]);
    }
    t.run("printf_like_and_misc");
}

#[test]
fn url_functions() {
    let mut t = Batch::new();
    let urls = [
        "http://example.com",
        "https://user:pass@example.com:8080/a/b?q=1&r=2#frag",
        "mailto:joe@example.com",
        "/relative/path",
        "relative/path",
        "a:b",
        ":nope",
        "http://[::1]:80/x",
        "http://[fe80::1%25en0]/",
        "http://a b.com/",
        "http://host/%zz",
        "http://host/a%20b?x=%20#%41",
        "//proto-relative/x",
        "http://user@host",
        "http://us%20er:p@ss@host/",
        "http://host:port/",
        "http://host:1:2/",
        "file:///etc/passwd",
        "*",
        "",
        "?query",
        "#frag",
        "HTTP://EXAMPLE.COM/",
        "http://[::1/",
        "http://x/\u{7f}",
    ];
    for u in urls {
        t.add("urlParse", vec![s(u)]);
    }
    let joins = vec![
        d(vec![("scheme", s("https")), ("host", s("example.com")), ("path", s("/a b")), ("query", s("x=1")), ("fragment", s("f g"))]),
        d(vec![("host", s("h")), ("path", s("rel"))]),
        d(vec![("scheme", s("mailto")), ("opaque", s("joe@x.com"))]),
        d(vec![("scheme", s("http")), ("host", s("h")), ("userinfo", s("user:pa ss"))]),
        d(vec![("scheme", s("http")), ("host", s("h")), ("userinfo", s("user"))]),
        d(vec![("path", s("a:b"))]),
        d(vec![("scheme", s("http")), ("path", s("//x"))]),
        d(vec![("host", i(5))]),
        d(vec![("host", nil())]),
        d(vec![]),
    ];
    for j in joins {
        t.add("urlJoin", vec![j]);
    }
    t.run("url_functions");
}

#[test]
fn crypto_deterministic() {
    let mut t = Batch::new();
    for (pt, pw) in [("", "pw"), ("hello", "secret"), ("x", ""), ("long text here", "a-very-long-password-longer-than-32-bytes!!")] {
        // decryptAES(encryptAES) round trip through Helm.
        let expr = format!("decryptAES {} (encryptAES {} {})", lit(pw), lit(pw), lit(pt));
        t.add_raw(&expr, Ok(pt.to_string()));
    }
    for bad in ["", "AAAA", "AAAAAAAAAAAAAAAAAAAAAA==", "!!!!", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="] {
        t.add("decryptAES", vec![s("pw"), s(bad)]);
    }
    for (c, ty, pw, user, site) in [(1u64, "long", "pass", "user", "example.com"), (3, "maximum", "", "", ""), (0, "pin", "é", "ü", "x"), (7, "nope", "a", "b", "c")] {
        let expr = format!("derivePassword {c} {} {} {} {}", lit(ty), lit(pw), lit(user), lit(site));
        let funcs = helm_rs::funcs::sprig();
        let ours = call(&funcs, "derivePassword", &[Value::Uint64(c), Value::str(ty), Value::str(pw), Value::str(user), Value::str(site)])
            .map(|v| render_value(&v));
        t.add_raw(&expr, ours);
    }
    t.add("htpasswd", vec![s("bad:user"), s("pw")]);
    t.run("crypto_deterministic");
}

impl ArgExt for Arg {}
trait ArgExt {
    /// Marks an argument whose Rust value must be computed by evaluating its
    /// template expression with our own functions (simple `(fn "lit" ...)`).
    fn with_ours_value(self) -> Arg
    where
        Self: Sized + Into<Arg> + Clone,
    {
        let a: Arg = self.into();
        let v = eval_simple(&a.tmpl);
        Arg::new(v, a.tmpl)
    }
}

/// Evaluates `(fn "s1" "s2")` expressions using our implementations.
fn eval_simple(expr: &str) -> Value {
    let inner = expr.trim_start_matches('(').trim_end_matches(')');
    let (name, rest) = inner.split_once(' ').unwrap();
    let mut args = Vec::new();
    let mut r = rest.trim();
    while !r.is_empty() {
        assert!(r.starts_with('"'), "unsupported: {expr}");
        let end = r[1..].find('"').unwrap() + 1;
        args.push(Value::str(&r[1..end]));
        r = r[end + 1..].trim_start();
    }
    let funcs = helm_rs::funcs::helm();
    call(&funcs, name, &args).unwrap()
}
