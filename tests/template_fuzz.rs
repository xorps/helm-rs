//! Template fuzzing: random Go templates rendered by helm-rs and by real Helm
//! (the oracle's `files` op, batched); both must produce the same output or
//! both fail, with the same message and the same `template:line:col`.
//!
//! Two sources of templates:
//! * a grammar of plausible templates: text, actions, pipelines, variables
//!   (declare/assign), if/else if/with/range (incl. `$i, $v :=`, else,
//!   break/continue), define/template/block, include/tpl, trim markers,
//!   comments, literals of every kind (ints, hex, octal, runes, floats,
//!   strings, raw strings, bools, nil), field/method chains, Sprig/Helm
//!   function calls with random (also wrong-typed / wrong-arity) arguments
//!   and nested parens, rendered over random values;
//! * seeds harvested from the corpus charts' templates, mutated at the
//!   action/token/character level and rendered together with the chart's
//!   helpers and default values.
//!
//! Knobs: `HELM_RS_FUZZ_SEED`, `HELM_RS_FUZZ_ITERS` (cases per source,
//! default 400), `HELM_RS_FUZZ_THREADS`, `HELM_RS_CHARTS` (seed charts),
//! `HELM_RS_LAX_ERRORS=1`.

mod common;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::compare::{Kind, Mismatch, compare_render};
use common::corpus;
use common::diff;
use common::rng::{Rng, fuzz_iters, fuzz_seed};
use common::{OracleResult, Opts, Oracle, parse_render, rust, with_opts};
use serde_json::json;

const CHART_YAML: &str = "apiVersion: v2\nname: t\nversion: 0.1.0\nappVersion: \"1.0\"\n";
const MAIN: &str = "templates/t.yaml";

fn opts() -> Opts {
    Opts { fixed_time: true, ..Opts::default() }
}

// ---------------------------------------------------------------------------
// Grammar

/// Functions that are nondeterministic (random, wall clock, network) and
/// therefore never generated.
const NONDETERMINISTIC: &[&str] = &[
    "shuffle", "randAlpha", "randAlphaNum", "randNumeric", "randAscii", "randInt", "randBytes", "uuidv4", "genCA",
    "genCAWithKey", "genPrivateKey", "genSelfSignedCert", "genSelfSignedCertWithKey", "genSignedCert",
    "genSignedCertWithKey", "encryptAES", "bcrypt", "htpasswd", "ago", "getHostByName",
];

#[derive(Clone, Copy)]
enum A {
    S,
    I,
    F,
    B,
    L,
    M,
    X,
}

/// (name, parameter kinds, variadic tail kind)
const FUNCS: &[(&str, &[A], Option<A>)] = {
    use A::*;
    &[
        ("trim", &[S], None), ("trimAll", &[S, S], None), ("trimPrefix", &[S, S], None), ("trimSuffix", &[S, S], None),
        ("upper", &[S], None), ("lower", &[S], None), ("title", &[S], None), ("untitle", &[S], None),
        ("repeat", &[I, S], None), ("substr", &[I, I, S], None), ("nospace", &[S], None), ("trunc", &[I, S], None),
        ("abbrev", &[I, S], None), ("abbrevboth", &[I, I, S], None), ("initials", &[S], None), ("wrap", &[I, S], None),
        ("wrapWith", &[I, S, S], None), ("contains", &[S, S], None), ("hasPrefix", &[S, S], None), ("hasSuffix", &[S, S], None),
        ("quote", &[], Some(X)), ("squote", &[], Some(X)), ("cat", &[], Some(X)), ("indent", &[I, S], None), ("nindent", &[I, S], None),
        ("replace", &[S, S, S], None), ("plural", &[S, S, I], None), ("snakecase", &[S], None), ("camelcase", &[S], None),
        ("kebabcase", &[S], None), ("swapcase", &[S], None), ("regexMatch", &[S, S], None), ("mustRegexMatch", &[S, S], None),
        ("regexFindAll", &[S, S, I], None), ("regexFind", &[S, S], None), ("regexReplaceAll", &[S, S, S], None),
        ("regexReplaceAllLiteral", &[S, S, S], None), ("regexSplit", &[S, S, I], None), ("regexQuoteMeta", &[S], None),
        ("toString", &[X], None), ("toStrings", &[X], None), ("atoi", &[S], None), ("int", &[X], None), ("int64", &[X], None),
        ("float64", &[X], None), ("toDecimal", &[X], None), ("seq", &[], Some(I)), ("until", &[I], None), ("untilStep", &[I, I, I], None),
        ("splitList", &[S, S], None), ("split", &[S, S], None), ("splitn", &[S, I, S], None), ("join", &[S, L], None),
        ("sortAlpha", &[L], None), ("b64enc", &[S], None), ("b64dec", &[S], None), ("b32enc", &[S], None), ("b32dec", &[S], None),
        ("sha1sum", &[S], None), ("sha256sum", &[S], None), ("sha512sum", &[S], None), ("adler32sum", &[S], None),
        ("toJson", &[X], None), ("mustToJson", &[X], None), ("toPrettyJson", &[X], None), ("toRawJson", &[X], None),
        ("fromJson", &[S], None), ("fromJsonArray", &[S], None), ("toYaml", &[X], None), ("mustToYaml", &[X], None),
        ("toYamlPretty", &[X], None), ("fromYaml", &[S], None), ("fromYamlArray", &[S], None), ("toToml", &[X], None),
        ("fromToml", &[S], None), ("default", &[X, X], None), ("empty", &[X], None), ("coalesce", &[], Some(X)),
        ("all", &[], Some(X)), ("any", &[], Some(X)), ("ternary", &[X, X, B], None), ("compact", &[L], None),
        ("first", &[L], None), ("last", &[L], None), ("rest", &[L], None), ("initial", &[L], None), ("append", &[L, X], None),
        ("prepend", &[L, X], None), ("concat", &[], Some(L)), ("reverse", &[L], None), ("uniq", &[L], None),
        ("without", &[L], Some(X)), ("has", &[X, L], None), ("slice", &[L], Some(I)), ("chunk", &[I, L], None),
        ("dict", &[], Some(X)), ("get", &[M, S], None), ("set", &[M, S, X], None), ("unset", &[M, S], None),
        ("hasKey", &[M, S], None), ("pluck", &[S], Some(M)), ("keys", &[], Some(M)), ("pick", &[M], Some(S)),
        ("omit", &[M], Some(S)), ("merge", &[M], Some(X)), ("mergeOverwrite", &[M], Some(X)), ("values", &[M], None),
        ("deepCopy", &[X], None), ("dig", &[S, X, M], None), ("list", &[], Some(X)), ("add", &[], Some(X)), ("add1", &[X], None),
        ("sub", &[X, X], None), ("mul", &[X], Some(X)), ("div", &[X, X], None), ("mod", &[X, X], None), ("max", &[X], Some(X)),
        ("min", &[X], Some(X)), ("floor", &[F], None), ("ceil", &[F], None), ("round", &[F, I], None), ("addf", &[], Some(X)),
        ("subf", &[X], Some(X)), ("mulf", &[X], Some(X)), ("divf", &[X], Some(X)), ("maxf", &[X], Some(X)), ("minf", &[X], Some(X)),
        ("len", &[X], None), ("index", &[L], Some(I)), ("printf", &[S], Some(X)), ("print", &[], Some(X)), ("println", &[], Some(X)),
        ("eq", &[X], Some(X)), ("ne", &[X, X], None), ("lt", &[X, X], None), ("le", &[X, X], None), ("gt", &[X, X], None),
        ("ge", &[X, X], None), ("and", &[X], Some(X)), ("or", &[X], Some(X)), ("not", &[X], None), ("html", &[], Some(X)),
        ("js", &[], Some(X)), ("urlquery", &[], Some(X)), ("semver", &[S], None), ("semverCompare", &[S, S], None),
        ("typeOf", &[X], None), ("kindOf", &[X], None), ("typeIs", &[S, X], None), ("kindIs", &[S, X], None),
        ("deepEqual", &[X, X], None), ("required", &[S, X], None), ("fail", &[S], None), ("toDate", &[S, S], None),
        ("date", &[S, X], None), ("dateInZone", &[S, X, S], None), ("dateModify", &[S, X], None), ("duration", &[X], None),
        ("durationRound", &[X], None), ("htmlDate", &[X], None), ("unixEpoch", &[X], None), ("now", &[], None),
        ("base", &[S], None), ("dir", &[S], None), ("ext", &[S], None), ("clean", &[S], None), ("isAbs", &[S], None),
        ("urlParse", &[S], None), ("urlJoin", &[M], None), ("lookup", &[S, S, S, S], None), ("derivePassword", &[I, S, S, S, S], None),
        ("decryptAES", &[S, S], None), ("mustToDuration", &[X], None), ("durationSeconds", &[X], None),
        ("durationMinutes", &[X], None), ("durationRoundTo", &[X, X], None), ("durationTruncateTo", &[X, X], None),

        ("mustFirst", &[L], None), ("mustAppend", &[L, X], None), ("mustMerge", &[M], Some(X)), ("mustDateModify", &[S, X], None),
        ("mustFromJson", &[S], None), ("sortAlpha", &[X], None),
    ]
};

/// Functions Helm removes from Sprig, and an unknown one: rarely generated.
const UNDEFINED_FUNCS: &[(&str, &[A], Option<A>)] = &[("env", &[A::S], None), ("expandenv", &[A::S], None), ("nonexistentFunc", &[A::X], None)];

const VALUE_KEYS: &[&str] = &["a", "b", "str", "num", "int", "float", "bool", "nil", "list", "map", "nested", "empty", "tpl", "Missing"];

struct Gen<'a> {
    rng: &'a mut Rng,
    /// Variables in scope.
    vars: Vec<String>,
    in_range: usize,
    depth: usize,
    defines: &'a [&'a str],
    /// Hostile mode: malformed snippets, wrong arity and argument types
    /// are much more likely (error-path coverage).
    noise: bool,
}

impl Gen<'_> {
    fn chance(&mut self, num: u32, den: u32) -> bool {
        self.rng.chance(num, den)
    }

    fn template(&mut self, max_items: usize) -> String {
        let n = 1 + self.rng.below(max_items);
        (0..n).map(|_| self.item()).collect()
    }

    fn item(&mut self) -> String {
        let deep = self.depth >= 4;
        match self.rng.weighted(&[6, 10, 1, if deep { 0 } else { 4 }, if self.noise { 2 } else { 0 }]) {
            0 => self.text(),
            1 => self.action(),
            2 => self.comment(),
            3 => self.control(),
            _ => self.raw_noise(),
        }
    }

    fn text(&mut self) -> String {
        self.rng
            .pick(&["foo", "\n", "  ", "key: ", "value\n", "- item\n", "{ ", "}", "-", "$", "ünï ✓", " x ", "\t", ":\n  ", "\"q\"", "}}"])
            .to_string()
    }

    fn raw_noise(&mut self) -> String {
        // Malformed delimiters and actions, for parse error coverage.
        self.rng
            .pick(&["{{", "{{}}", "{{ }}", "{{-}}", "{{-x}}", "{{ .", "{{ end }}", "{{ else }}", "{{ (", "{{ ) }}", "{{ \"unterminated }}", "{{ 'ab' }}", "{{ $undefined }}", "{{ break }}", "{{ continue }}", "{{ .a | }}", "{{ | .a }}", "{{ .a.  b }}", "{{ 1 2 }}", "{{ define \"x\" }}{{ end }}", "{{/* unterminated", "{{ := 1 }}", "{{ $x := }}"])
            .to_string()
    }

    fn comment(&mut self) -> String {
        let (l, r) = self.trims();
        let body = self.rng.pick(&["/* c */", "/* multi\nline */", "/**/"]);
        format!("{{{{{l}{body}{r}}}}}")
    }

    fn trims(&mut self) -> (&'static str, &'static str) {
        let l = if self.chance(1, 3) { "- " } else { "" };
        let r = if self.chance(1, 3) { " -" } else { "" };
        (l, r)
    }

    fn open(&mut self) -> String {
        let (l, _) = self.trims();
        if l.is_empty() { "{{ ".into() } else { "{{- ".into() }
    }

    fn close(&mut self) -> String {
        if self.chance(1, 3) { " -}}".into() } else { " }}".into() }
    }

    fn action(&mut self) -> String {
        let (o, c) = (self.open(), self.close());
        let p = if self.chance(1, 5) {
            let v = format!("${}", self.rng.pick(&["x", "y", "val", "i"]));
            let assign = self.vars.contains(&v) && self.chance(1, 2);
            let p = self.pipeline();
            if !self.vars.contains(&v) {
                self.vars.push(v.clone());
            }
            format!("{v} {} {p}", if assign { "=" } else { ":=" })
        } else if self.in_range > 0 && self.chance(1, 12) {
            self.rng.pick(&["break", "continue"]).to_string()
        } else {
            self.pipeline()
        };
        format!("{o}{p}{c}")
    }

    fn control(&mut self) -> String {
        self.depth += 1;
        let saved_vars = self.vars.len();
        let out = match self.rng.weighted(&[5, 3, 4, 2, 2, 2, 2]) {
            0 => {
                let mut s = format!("{}if {}{}", self.open(), self.pipeline(), self.close());
                s += &self.body(3);
                while self.chance(1, 3) {
                    s += &format!("{}else if {}{}", self.open(), self.pipeline(), self.close());
                    s += &self.body(2);
                }
                if self.chance(1, 2) {
                    s += &format!("{}else{}", self.open(), self.close());
                    s += &self.body(2);
                }
                s + &format!("{}end{}", self.open(), self.close())
            }
            1 => {
                let mut s = format!("{}with {}{}", self.open(), self.pipeline(), self.close());
                s += &self.body(3);
                if self.chance(1, 3) {
                    s += &format!("{}else{}", self.open(), self.close());
                    s += &self.body(2);
                }
                s + &format!("{}end{}", self.open(), self.close())
            }
            2 => {
                let decl = match self.rng.below(3) {
                    0 => {
                        self.vars.push("$i".into());
                        self.vars.push("$v".into());
                        "$i, $v := "
                    }
                    1 => {
                        self.vars.push("$v".into());
                        "$v := "
                    }
                    _ => "",
                };
                let over = match self.rng.below(6) {
                    0 => format!("until {}", self.rng.range(-1, 4)),
                    1 => self.pipeline(),
                    _ => self.rng.pick(&[".Values.list", ".Values.map", "$.Values.nested.list", ".Values.empty", "(list 1 2 3)", "(dict \"b\" 2 \"a\" 1)", ".Values.nil", ".Values.b"]).to_string(),
                };
                let mut s = format!("{}range {decl}{over}{}", self.open(), self.close());
                self.in_range += 1;
                s += &self.body(3);
                self.in_range -= 1;
                if self.chance(1, 3) {
                    s += &format!("{}else{}", self.open(), self.close());
                    s += &self.body(2);
                }
                s + &format!("{}end{}", self.open(), self.close())
            }
            3 => {
                let name = *self.rng.pick(self.defines);
                format!("{}template {:?} {}{}", self.open(), name, self.operand(), self.close())
            }
            4 => {
                let name = *self.rng.pick(self.defines);
                format!("{}include {:?} {}{}", self.open(), name, self.operand(), self.close())
            }
            5 => {
                let inner = Gen { rng: self.rng, vars: Vec::new(), in_range: 0, depth: self.depth + 2, defines: self.defines, noise: self.noise }.template_owned(2);
                format!("{}tpl {} {}{}", self.open(), go_quote(&inner, self.rng), self.rng.pick(&["$", ".", "(dict)", "(dict \"Template\" $.Template \"Values\" $.Values)"]), self.close())
            }
            _ => {
                let name = self.rng.pick(&["blk", "blk2"]).to_string();
                let mut s = format!("{}block {:?} {}{}", self.open(), name, self.operand(), self.close());
                s += &self.body(2);
                s + &format!("{}end{}", self.open(), self.close())
            }
        };
        self.vars.truncate(saved_vars);
        self.depth -= 1;
        out
    }

    /// A branch body: variables declared inside are not visible after it.
    fn body(&mut self, n: usize) -> String {
        let saved = self.vars.len();
        let out = self.template(n);
        self.vars.truncate(saved);
        out
    }

    fn template_owned(mut self, n: usize) -> String {
        self.template(n)
    }

    fn pipeline(&mut self) -> String {
        let mut s = self.command();
        let mut n = 0;
        while n < 3 && self.chance(1, 3) {
            s += " | ";
            s += &self.func_call(true);
            n += 1;
        }
        s
    }

    fn command(&mut self) -> String {
        if self.depth > 6 {
            return self.literal();
        }
        match self.rng.weighted(&[5, 6, 1]) {
            0 => self.operand(),
            1 => self.func_call(false),
            _ => format!("({})", {
                self.depth += 1;
                let p = self.pipeline();
                self.depth -= 1;
                p
            }),
        }
    }

    fn func_call(&mut self, piped: bool) -> String {
        let candidates: Vec<_> = FUNCS.iter().filter(|f| !NONDETERMINISTIC.contains(&f.0)).collect();
        let (name, params, rest) = if self.chance(1, 400) { *self.rng.pick(UNDEFINED_FUNCS) } else { **self.rng.pick(&candidates) };
        let mut kinds: Vec<A> = params.to_vec();
        if let Some(r) = rest {
            for _ in 0..self.rng.below(3) {
                kinds.push(r);
            }
        }
        if piped && !kinds.is_empty() {
            kinds.pop(); // the pipeline supplies the last argument
        }
        // Wrong arity sometimes.
        match self.rng.below(if self.noise { 10 } else { 80 }) {
            0 if !kinds.is_empty() => {
                kinds.pop();
            }
            1 => kinds.push(A::X),
            _ => {}
        }
        let mut s = name.to_string();
        for k in kinds {
            s.push(' ');
            let wrong = self.chance(1, if self.noise { 6 } else { 40 });
            s += &if wrong { self.arg(A::X) } else { self.arg(k) };
        }
        s
    }

    fn arg(&mut self, k: A) -> String {
        if self.depth < 6 && self.chance(1, 6) {
            self.depth += 1;
            let p = format!("({})", self.pipeline());
            self.depth -= 1;
            return p;
        }
        match k {
            A::S => {
                if self.chance(1, 3) {
                    self.rng.pick(&[".Values.a", ".Values.str", ".Release.Name", ".Chart.Name", ".Values.b.c.d", ".Values.map.k", ".Values.tpl", "$.Values.a"]).to_string()
                } else {
                    go_quote(self.rng.pick(&["", "a", "abc", "a,b,c", "1", "-", "x.y", "{{ .Values.a }}", "hello world", "1.2.3", ">=1.0.0", "2006-01-02", "1h30m", "%s-%d", "%v", "a\\nb", "[a-z]+", "Ünï", "true", "{\"k\": [1, 2]}", "- a\n- b", "k: v", "k = \"v\""]), self.rng)
                }
            }
            A::I => self.rng.pick(&["0", "1", "2", "-1", "3", "10", "0x10", "1e2", "255", "1.5", "'a'"]).to_string(),
            A::F => self.rng.pick(&["1.5", "0.0", "-2.25", "1e3", "3"]).to_string(),
            A::B => self.rng.pick(&["true", "false", ".Values.bool", "(eq 1 1)"]).to_string(),
            A::L => self.rng.pick(&[".Values.list", "(list 1 \"a\" nil)", "(list)", "(splitList \",\" \"a,b\")", ".Values.empty"]).to_string(),
            A::M => self.rng.pick(&[".Values.map", "(dict \"a\" 1)", "(dict)", ".Values", "$.Values.nested", ".Values.nil"]).to_string(),
            A::X => self.operand(),
        }
    }

    fn operand(&mut self) -> String {
        match self.rng.weighted(&[6, 3, 4, 1, 1]) {
            0 => self.field(),
            1 if !self.vars.is_empty() => {
                let v = self.rng.pick(&self.vars).clone();
                if self.chance(1, 3) { format!("{v}.{}", self.rng.pick(VALUE_KEYS)) } else { v }
            }
            2 => self.literal(),
            3 => ".".into(),
            _ => "$".into(),
        }
    }

    fn field(&mut self) -> String {
        match self.rng.weighted(&[10, 2, 1, 1, 1, 1]) {
            0 => {
                let mut s = String::from(if self.chance(1, 4) { "$.Values" } else { ".Values" });
                match self.rng.weighted(&[7, 2, 1]) {
                    0 => {
                        s.push('.');
                        s.push_str(self.rng.pick(VALUE_KEYS));
                    }
                    1 => s.push_str(self.rng.pick(&[".b.c.d", ".b.c", ".map.k", ".map.z", ".nested.a.b.c", ".nested.list", ""])),
                    _ => {
                        for _ in 0..1 + self.rng.below(3) {
                            s.push('.');
                            s.push_str(self.rng.pick(VALUE_KEYS));
                        }
                    }
                }
                s
            }
            1 => self.rng.pick(&[".Release.Name", ".Release.Namespace", ".Release.IsInstall", ".Release.Revision", ".Release.Service"]).to_string(),
            2 => self.rng.pick(&[".Chart.Name", ".Chart.Version", ".Chart.AppVersion", ".Chart.Annotations", ".Chart.IsRoot"]).to_string(),
            3 => self.rng.pick(&[".Capabilities.KubeVersion.Version", ".Capabilities.KubeVersion.Major", ".Capabilities.APIVersions.Has \"apps/v1\"", ".Capabilities.HelmVersion.Version", ".Capabilities.KubeVersion"]).to_string(),
            4 => self.rng.pick(&[".Template.Name", ".Template.BasePath", ".Files.Get \"x.txt\"", ".Files.Glob \"*.txt\"", "(.Files.Get \"x.txt\")", ".Files.Lines \"x.txt\"", ".Files.AsConfig"]).to_string(),
            _ => self.rng.pick(&[".Nope", ".Values.a.b.c.d", ".Values.list.x", "(index .Values.list 0)", "(index .Values.map \"k\")", ".Values.map.k"]).to_string(),
        }
    }

    fn literal(&mut self) -> String {
        match self.rng.below(5) {
            0 => self.rng.pick(&["0", "1", "-1", "42", "0x1F", "0o17", "017", "0b101", "1_000", "9223372036854775807", "9223372036854775808", "-9223372036854775808", "'a'", "'\\n'", "'ü'"]).to_string(),
            1 => self.rng.pick(&["1.5", "-0.5", "1e3", "1E-2", ".5", "0x1p-2", "1.0", "3.", "1e10"]).to_string(),
            2 => go_quote(self.rng.pick(&["", "x", "with space", "quote\"inside", "tab\there", "uni ✓", "{{ x }}", "\\", "line\nbreak"]), self.rng),
            3 => self.rng.pick(&["true", "false", "nil"]).to_string(),
            _ => self.rng.pick(&["`raw`", "`multi\nline`", "``", "\"\\u00e9\"", "\"\\x41\"", "\"\\101\""]).to_string(),
        }
    }
}

/// Go string literal (sometimes a raw string).
fn go_quote(s: &str, rng: &mut Rng) -> String {
    if !s.contains('`') && rng.chance(1, 5) {
        return format!("`{s}`");
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn random_values(rng: &mut Rng) -> serde_json::Value {
    let mut v = json!({
        "a": "A", "b": {"c": {"d": "deep"}}, "str": "hello", "num": 1000000, "int": 3, "float": 1.5,
        "bool": true, "nil": null, "list": ["x", 2, false, {"k": "v"}], "map": {"k": "v", "n": 2, "z": [1]},
        "nested": {"a": {"b": {"c": "d"}}, "list": [[1, 2], []]}, "empty": [], "tpl": "{{ .Values.str }}-{{ .Release.Name }}",
    });
    let alts = [
        json!(null), json!(""), json!(0), json!(-1), json!(1.5), json!(1e21), json!(true), json!("true"), json!("1"),
        json!([]), json!({}), json!(["a", "b"]), json!({"k": null}), json!("{{ fail \"tpl\" }}"), json!("ünï"),
    ];
    let keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    for k in keys {
        if rng.chance(1, 6) {
            v[&k] = rng.pick(&alts).clone();
        }
    }
    v
}

fn helpers(rng: &mut Rng, defines: &[&str]) -> String {
    let mut s = String::new();
    for name in defines {
        if rng.chance(1, 8) {
            continue; // leave some undefined
        }
        let body = Gen { rng, vars: Vec::new(), in_range: 0, depth: 2, defines: &["h1", "h2"], noise: false }.template_owned(3);
        s += &format!("{{{{- define {name:?} -}}}}\n{body}\n{{{{- end -}}}}\n");
    }
    s
}

/// A complete fuzz case: chart files (Chart.yaml, values.yaml, templates).
fn grammar_case(rng: &mut Rng) -> BTreeMap<String, String> {
    let defines = ["h1", "h2", "h3", "t.name"];
    let mut files = BTreeMap::new();
    files.insert("Chart.yaml".into(), CHART_YAML.into());
    files.insert("values.yaml".into(), serde_json::to_string_pretty(&random_values(rng)).unwrap());
    files.insert("x.txt".into(), "file contents\nline 2\n".into());
    let noise = rng.chance(1, 4);
    let main = Gen { rng, vars: Vec::new(), in_range: 0, depth: 0, defines: &defines, noise }.template_owned(6);
    files.insert(MAIN.into(), main);
    files.insert("templates/_helpers.tpl".into(), helpers(rng, &defines));
    files
}

// ---------------------------------------------------------------------------
// Seed mutation

struct Seed {
    chart: String,
    /// Template path within the chart, e.g. `templates/deployment.yaml`.
    name: String,
    text: String,
    /// Shared context: Chart.yaml, values.yaml, helpers (by chart).
    ctx: std::sync::Arc<BTreeMap<String, String>>,
}

/// Harvests templates from the corpus charts, with each chart's helper
/// templates (including its subcharts', flattened) and coalesced values.
fn harvest_seeds(o: &mut Oracle) -> Vec<Seed> {
    let mut seeds = Vec::new();
    for c in corpus::available() {
        let root = c.extracted();
        let values = o.chart_values(Some(&c.tgz_path()), &c.values_yaml()).unwrap_or(json!({}));
        let mut ctx = BTreeMap::new();
        ctx.insert("Chart.yaml".to_string(), format!("apiVersion: v2\nname: {}\nversion: 0.1.0\n", c.chart));
        ctx.insert("values.yaml".to_string(), serde_json::to_string(&values).unwrap());
        let mut templates = Vec::new();
        for (path, bytes) in corpus::walk_files(&root) {
            let Ok(text) = String::from_utf8(bytes) else { continue };
            let file = path.rsplit('/').next().unwrap();
            let in_templates = path.contains("templates/");
            if !in_templates {
                continue;
            }
            if file.starts_with('_') {
                // Helpers of subcharts are flattened into the root chart.
                let flat = format!("templates/_{}", path.replace('/', "_"));
                ctx.insert(if path.starts_with("templates/") { path.clone() } else { flat }, text);
            } else if path.starts_with("templates/") && !path.contains("/tests/") {
                templates.push((path, text));
            }
        }
        let ctx = std::sync::Arc::new(ctx);
        for (name, text) in templates {
            seeds.push(Seed { chart: c.name.clone(), name, text, ctx: ctx.clone() });
        }
    }
    seeds
}

/// Spans of `{{ ... }}` actions in `s`.
fn actions(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(a) = s[i..].find("{{") {
        let start = i + a;
        match s[start + 2..].find("}}") {
            Some(b) => {
                let end = start + 2 + b + 2;
                out.push((start, end));
                i = end;
            }
            None => break,
        }
    }
    out
}

fn mutate(text: &str, rng: &mut Rng) -> (String, &'static str) {
    let acts = actions(text);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let pick_act = |rng: &mut Rng| acts[rng.below(acts.len())];
    let mut s = text.to_string();
    let what = match rng.below(12) {
        0 | 1 if !acts.is_empty() => {
            let (a, b) = pick_act(rng);
            s.replace_range(a..b, "");
            "delete action"
        }
        2 if !acts.is_empty() => {
            let (a, b) = pick_act(rng);
            let dup = s[a..b].to_string();
            let at = pick_act(rng).0;
            s.insert_str(at, &dup);
            "duplicate action"
        }
        3 if acts.len() >= 2 => {
            let (x, y) = (pick_act(rng), pick_act(rng));
            let (x, y) = if x.0 < y.0 { (x, y) } else { (y, x) };
            if x == y {
                return mutate(text, rng);
            }
            let (ax, ay) = (s[x.0..x.1].to_string(), s[y.0..y.1].to_string());
            s.replace_range(y.0..y.1, &ax);
            s.replace_range(x.0..x.1, &ay);
            "swap actions"
        }
        4 if !acts.is_empty() => {
            // Toggle trim markers.
            let (a, b) = pick_act(rng);
            let inner = &text[a + 2..b - 2];
            let inner = if let Some(r) = inner.strip_prefix("- ") { r.to_string() } else { format!("- {inner}") };
            let inner = if let Some(r) = inner.strip_suffix(" -") { r.to_string() } else { format!("{inner} -") };
            s.replace_range(a..b, &format!("{{{{{inner}}}}}"));
            "toggle trim markers"
        }
        5 | 6 if !acts.is_empty() => {
            // Replace an identifier inside an action.
            let (a, b) = pick_act(rng);
            let inner = &text[a..b];
            let words: Vec<(usize, usize)> = word_spans(inner);
            if words.is_empty() {
                return mutate(text, rng);
            }
            let (wa, wb) = words[rng.below(words.len())];
            let repl = match rng.below(3) {
                0 => rng.pick(FUNCS).0.to_string(),
                1 => rng.pick(&["Values", "Release", "Chart", "nope", "end", "else", "if", "range", "nil", "true", "$", "."]).to_string(),
                _ => rng.pick(VALUE_KEYS).to_string(),
            };
            s.replace_range(a + wa..a + wb, &repl);
            "replace identifier"
        }
        7 if !acts.is_empty() => {
            // Insert a punctuation char inside an action.
            let (a, b) = pick_act(rng);
            let at = (a + 2..b - 1).filter(|&i| s.is_char_boundary(i)).nth(rng.below((b - a - 3).max(1))).unwrap_or(a + 2);
            s.insert(at, *rng.pick(&['(', ')', '|', '$', '.', '"', '`', ' ', '-', ',', '=', ':']));
            "insert char in action"
        }
        8 => {
            let at = chars.get(rng.below(chars.len().max(1))).map_or(0, |c| c.0);
            s.truncate(at);
            "truncate"
        }
        9 => {
            // Insert a generated action.
            let defines = ["h1"];
            let snippet = Gen { rng, vars: Vec::new(), in_range: 0, depth: 3, defines: &defines, noise: false }.template_owned(2);
            let at = chars.get(rng.below(chars.len().max(1))).map_or(0, |c| c.0);
            s.insert_str(at, &snippet);
            "insert generated snippet"
        }
        _ if !chars.is_empty() => {
            let (i, c) = chars[rng.below(chars.len())];
            match rng.below(3) {
                0 => {
                    s.replace_range(i..i + c.len_utf8(), "");
                    "delete char"
                }
                1 => {
                    s.insert(i, *rng.pick(&['{', '}', '\n', ' ', '"', 'x', '-', '\t']));
                    "insert char"
                }
                _ => {
                    s.replace_range(i..i + c.len_utf8(), rng.pick(&["{{", "}}", "\n", "$", "ü"]));
                    "replace char"
                }
            }
        }
        _ => "unchanged",
    };
    (s, what)
}

fn word_spans(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices().chain(std::iter::once((s.len(), ' '))) {
        let w = c.is_alphanumeric() || c == '_';
        match (start, w) {
            (None, true) => start = Some(i),
            (Some(st), false) => {
                out.push((st, i));
                start = None;
            }
            _ => {}
        }
    }
    out
}

fn seed_case(seed: &Seed, rng: &mut Rng) -> (BTreeMap<String, String>, String) {
    let mut text = seed.text.clone();
    let mut whats = Vec::new();
    for _ in 0..1 + rng.weighted(&[6, 3, 1]) {
        let (t, w) = mutate(&text, rng);
        text = t;
        whats.push(w);
    }
    let mut files = (*seed.ctx).clone();
    files.insert(seed.name.clone(), text);
    (files, format!("{}:{} mutated by [{}]", seed.chart, seed.name, whats.join(", ")))
}

// ---------------------------------------------------------------------------
// Driver

struct Case {
    id: String,
    files: BTreeMap<String, String>,
    /// For seed mutations: the original template (for the report).
    original: Option<(String, String)>,
}

fn helm_batch(o: &mut Oracle, cases: &[&BTreeMap<String, String>]) -> Vec<OracleResult> {
    let reqs = cases.iter().map(|f| with_opts(json!({"op": "files", "files": f}), "", &opts())).collect();
    o.batch(reqs).iter().map(parse_render).collect()
}

fn check(o: &mut Oracle, files: &BTreeMap<String, String>, helm: Option<OracleResult>) -> Option<Mismatch> {
    let helm = helm.unwrap_or_else(|| o.render_files(files, "", &opts()));
    let ours = rust::render_files(files, "", &opts());
    compare_render(&helm, || Some(o.render_files(files, "", &opts())), &ours)
}

/// Removes `{{ }}` actions / text chunks of the main template while the same
/// kind of mismatch persists.
fn minimize(o: &mut Oracle, files: &BTreeMap<String, String>, main: &str, kind: Kind) -> (BTreeMap<String, String>, Mismatch) {
    let mut cur = files.clone();
    let mut budget = 60;
    'outer: while budget > 0 {
        let text = cur[main].clone();
        let mut spans = actions(&text);
        // Also try lines.
        let mut pos = 0;
        for l in text.split_inclusive('\n') {
            spans.push((pos, pos + l.len()));
            pos += l.len();
        }
        spans.sort_by_key(|s| std::cmp::Reverse(s.1 - s.0));
        for (a, b) in spans {
            if budget == 0 {
                break 'outer;
            }
            budget -= 1;
            let mut t = text.clone();
            t.replace_range(a..b, "");
            let mut cand = cur.clone();
            cand.insert(main.to_string(), t);
            if check(o, &cand, None).is_some_and(|m| m.kind == kind) {
                cur = cand;
                continue 'outer;
            }
        }
        break;
    }
    let m = check(o, &cur, None).unwrap_or(Mismatch { kind, report: "(mismatch vanished while minimizing: flaky?)".into() });
    (cur, m)
}

fn report_case(o: &mut Oracle, case: &Case, m: Mismatch, seed: u64) -> String {
    let main = case.files.keys().find(|k| case.original.as_ref().is_some_and(|(n, _)| n == *k)).cloned().unwrap_or_else(|| MAIN.to_string());
    let (min, m2) = minimize(o, &case.files, &main, m.kind);
    let mut s = format!("=== {} (HELM_RS_FUZZ_SEED={seed}): {:?}\n{}\n", case.id, m2.kind, m2.report);
    if let Some((name, orig)) = &case.original {
        s += &format!("mutation (original -> fuzzed, before minimization):\n{}", diff::unified(name, orig, name, &case.files[name], 40));
    }
    s += &format!("minimized {main}:\n{}\n", min[&main]);
    for (k, v) in &min {
        if k != &main && k.ends_with(".tpl") && case.original.is_none() {
            s += &format!("{k}:\n{v}\n");
        }
        if k == "values.yaml" && case.original.is_none() {
            s += &format!("values.yaml:\n{v}\n");
        }
    }
    s
}

#[derive(Default)]
struct Stats {
    cases: usize,
    helm_ok: usize,
    helm_err: usize,
}

fn run_cases(cases: Vec<Case>, seed: u64, what: &str) {
    if cases.is_empty() {
        return;
    }
    // If helm-rs cannot render a trivial chart yet, say so once instead of
    // reporting every case.
    let trivial: BTreeMap<String, String> = [("Chart.yaml".to_string(), CHART_YAML.to_string()), (MAIN.to_string(), "ok {{ 1 }}".to_string())].into();
    if let rust::Outcome::Panic(p) = rust::render_files(&trivial, "", &opts()) {
        panic!("{what}: helm-rs panics on a trivial chart ({p}); not fuzzing {} cases", cases.len());
    }
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let stats = Mutex::new(Stats::default());
    const BATCH: usize = 32;
    let max_failures = 10;
    std::thread::scope(|sc| {
        for _ in 0..threads() {
            sc.spawn(|| {
                let mut o = Oracle::spawn().expect("spawn oracle");
                loop {
                    if failures.lock().unwrap().len() >= max_failures {
                        break;
                    }
                    let start = next.fetch_add(BATCH, Ordering::Relaxed);
                    if start >= cases.len() {
                        break;
                    }
                    let chunk = &cases[start..(start + BATCH).min(cases.len())];
                    let helm = helm_batch(&mut o, &chunk.iter().map(|c| &c.files).collect::<Vec<_>>());
                    for (case, h) in chunk.iter().zip(helm) {
                        {
                            let mut s = stats.lock().unwrap();
                            s.cases += 1;
                            if matches!(h, OracleResult::Ok(_)) { s.helm_ok += 1 } else { s.helm_err += 1 }
                        }
                        if let Some(m) = check(&mut o, &case.files, Some(h)) {
                            let r = report_case(&mut o, case, m, seed);
                            let mut f = failures.lock().unwrap();
                            if f.len() < max_failures {
                                f.push(r);
                            }
                        }
                    }
                }
            });
        }
    });
    let s = stats.into_inner().unwrap();
    eprintln!("{what}: {} cases ({} render in helm, {} fail in helm), seed {seed}", s.cases, s.helm_ok, s.helm_err);
    let failures = failures.into_inner().unwrap();
    if !failures.is_empty() {
        for f in &failures {
            eprintln!("\n{f}");
        }
        panic!("{what}: {} failing cases shown (stopped at {max_failures}); HELM_RS_FUZZ_SEED={seed}", failures.len());
    }
}

fn threads() -> usize {
    std::env::var("HELM_RS_FUZZ_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| (std::thread::available_parallelism().map_or(2, |n| n.get()) / 2).clamp(1, 8))
}

fn grammar_cases(seed: u64, n: usize) -> Vec<Case> {
    let mut rng = Rng::fork(seed, "grammar");
    (0..n).map(|i| Case { id: format!("grammar case {i}"), files: grammar_case(&mut rng), original: None }).collect()
}

fn seed_cases(o: &mut Oracle, seed: u64, n: usize) -> Vec<Case> {
    let seeds = harvest_seeds(o);
    if seeds.is_empty() {
        return Vec::new();
    }
    let mut rng = Rng::fork(seed, "seeds");
    (0..n)
        .map(|i| {
            let sd = &seeds[rng.below(seeds.len())];
            let (files, id) = seed_case(sd, &mut rng);
            Case { id: format!("seed case {i}: {id}"), files, original: Some((sd.name.clone(), sd.text.clone())) }
        })
        .collect()
}

#[test]
fn grammar_templates_match_helm() {
    if Oracle::shared().is_none() {
        return;
    }
    let seed = fuzz_seed("grammar_templates_match_helm");
    run_cases(grammar_cases(seed, fuzz_iters(400)), seed, "template grammar fuzz");
}

#[test]
fn mutated_corpus_templates_match_helm() {
    let Some(mut o) = common::oracle() else { return };
    let seed = fuzz_seed("mutated_corpus_templates_match_helm");
    let cases = seed_cases(&mut o, seed, fuzz_iters(400));
    drop(o);
    run_cases(cases, seed, "corpus template mutation fuzz");
}

/// Checks the generators against the oracle alone: reproducible, a useful
/// mix of successes, parse errors and execution errors, and Helm's errors
/// carry locations the comparison can use.
#[test]
fn template_generators_produce_useful_cases() {
    let Some(mut o) = common::oracle() else { return };
    let a: Vec<_> = grammar_cases(99, 300).into_iter().map(|c| c.files).collect();
    let b: Vec<_> = grammar_cases(99, 300).into_iter().map(|c| c.files).collect();
    assert_eq!(a, b, "grammar generator not reproducible");
    let seeded: Vec<_> = seed_cases(&mut o, 99, 200).into_iter().map(|c| c.files).collect();
    for (what, cases) in [("grammar", a), ("corpus seeds", seeded)] {
        if cases.is_empty() {
            continue;
        }
        let (mut ok, mut parse, mut exec, mut located) = (0, 0, 0, 0);
        for chunk in cases.chunks(50) {
            for r in helm_batch(&mut o, &chunk.iter().collect::<Vec<_>>()) {
                match r {
                    OracleResult::Ok(_) => ok += 1,
                    OracleResult::Err { message, .. } => {
                        if std::env::var_os("HELM_RS_FUZZ_VERBOSE").is_some() {
                            eprintln!("helm error ({what}): {:?}", message.chars().take(400).collect::<String>());
                        }
                        if message.contains("executing") || message.contains("execution error") {
                            exec += 1;
                        } else {
                            parse += 1;
                        }
                        if !diff::error_locations(&message).is_empty() {
                            located += 1;
                        }
                    }
                }
            }
        }
        let total = cases.len();
        eprintln!("{what}: {total} cases: {ok} render, {parse} parse errors, {exec} execution errors, {located} errors with a location");
        assert!(ok * 4 >= total, "{what}: too few successful renders: {ok}/{total}");
        assert!(parse * 20 >= total && exec * 20 >= total, "{what}: error paths underrepresented");
        assert!(located * 10 >= (parse + exec) * 8, "{what}: too few errors with locations: {located}/{}", parse + exec);
    }
}
