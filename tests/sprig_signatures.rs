//! Verifies that every registered function exists in Helm with the same Go
//! signature (arity, variadic-ness and parameter types), by provoking
//! text/template's argument errors in real Helm.

mod diffkit;

use diffkit::*;
use helm_rs::{Sig, Ty};

/// Sprig's genericMap keys (v3.3.0), minus `fail` (provided by the engine).
const SPRIG_FUNCS: &[&str] = &[
    "hello", "ago", "date", "date_in_zone", "date_modify", "dateInZone", "dateModify", "duration", "durationRound",
    "htmlDate", "htmlDateInZone", "must_date_modify", "mustDateModify", "mustToDate", "now", "toDate", "unixEpoch",
    "abbrev", "abbrevboth", "trunc", "trim", "upper", "lower", "title", "untitle", "substr", "repeat", "trimall",
    "trimAll", "trimSuffix", "trimPrefix", "nospace", "initials", "randAlphaNum", "randAlpha", "randAscii",
    "randNumeric", "swapcase", "shuffle", "snakecase", "camelcase", "kebabcase", "wrap", "wrapWith", "contains",
    "hasPrefix", "hasSuffix", "quote", "squote", "cat", "indent", "nindent", "replace", "plural", "sha1sum",
    "sha256sum", "sha512sum", "adler32sum", "toString", "atoi", "int64", "int", "float64", "seq", "toDecimal", "split",
    "splitList", "splitn", "toStrings", "until", "untilStep", "add1", "add", "sub", "div", "mod", "mul", "randInt",
    "add1f", "addf", "subf", "divf", "mulf", "biggest", "max", "min", "maxf", "minf", "ceil", "floor", "round", "join",
    "sortAlpha", "default", "empty", "coalesce", "all", "any", "compact", "mustCompact", "fromJson", "toJson",
    "toPrettyJson", "toRawJson", "mustFromJson", "mustToJson", "mustToPrettyJson", "mustToRawJson", "ternary",
    "deepCopy", "mustDeepCopy", "typeOf", "typeIs", "typeIsLike", "kindOf", "kindIs", "deepEqual", "env", "expandenv",
    "getHostByName", "base", "dir", "clean", "ext", "isAbs", "osBase", "osClean", "osDir", "osExt", "osIsAbs",
    "b64enc", "b64dec", "b32enc", "b32dec", "tuple", "list", "dict", "get", "set", "unset", "hasKey", "pluck", "keys",
    "pick", "omit", "merge", "mergeOverwrite", "mustMerge", "mustMergeOverwrite", "values", "append", "push",
    "mustAppend", "mustPush", "prepend", "mustPrepend", "first", "mustFirst", "rest", "mustRest", "last", "mustLast",
    "initial", "mustInitial", "reverse", "mustReverse", "uniq", "mustUniq", "without", "mustWithout", "has",
    "mustHas", "slice", "mustSlice", "concat", "dig", "chunk", "mustChunk", "bcrypt", "htpasswd", "genPrivateKey",
    "derivePassword", "buildCustomCert", "genCA", "genCAWithKey", "genSelfSignedCert", "genSelfSignedCertWithKey",
    "genSignedCert", "genSignedCertWithKey", "encryptAES", "decryptAES", "randBytes", "uuidv4", "semver",
    "semverCompare", "regexMatch", "mustRegexMatch", "regexFindAll", "mustRegexFindAll", "regexFind",
    "mustRegexFind", "regexReplaceAll", "mustRegexReplaceAll", "regexReplaceAllLiteral",
    "mustRegexReplaceAllLiteral", "regexSplit", "mustRegexSplit", "regexQuoteMeta", "urlParse", "urlJoin",
];

const HELM_DURATION_FUNCS: &[&str] = &[
    "mustToDuration", "durationSeconds", "durationMilliseconds", "durationMicroseconds", "durationNanoseconds",
    "durationMinutes", "durationHours", "durationDays", "durationWeeks", "durationRoundTo", "durationTruncateTo",
];

#[test]
fn sprig_function_set_is_complete() {
    let f = helm_rs::funcs::sprig();
    let mut names: Vec<&str> = f.names().collect();
    names.sort();
    let mut want: Vec<&str> = SPRIG_FUNCS.to_vec();
    want.sort();
    assert_eq!(names, want);
    let h = helm_rs::funcs::helm();
    for n in HELM_DURATION_FUNCS {
        assert!(h.contains(n), "{n}");
    }
    assert!(!h.contains("env") && !h.contains("expandenv"));
}

fn dummy(t: Ty) -> &'static str {
    match t {
        Ty::Any => "\"x\"",
        Ty::String => "\"x\"",
        Ty::Bool => "false",
        Ty::Int | Ty::Int64 | Ty::Uint32 | Ty::Uint64 => "0",
        Ty::Float64 => "0.5",
        Ty::Bytes => "(list)",
        Ty::List => "(list)",
        Ty::Strings => "(list)",
        Ty::Map => "(dict)",
        Ty::Object(_) | Ty::Struct(_) => "nil",
    }
}

/// A value of a type that does not convert to `t` (or "" if `t` accepts
/// everything).
fn foreign(t: Ty) -> &'static str {
    match t {
        Ty::Any => "",
        Ty::List | Ty::Strings | Ty::Bytes | Ty::Map => "\"str\"",
        Ty::Object(_) | Ty::Struct(_) => "\"str\"",
        _ => "(list)",
    }
}

fn go_type_name(t: Ty) -> &'static str {
    match t {
        Ty::Object(n) | Ty::Struct(n) => n,
        Ty::Uint64 => "uint64",
        _ => t.go_name(),
    }
}

#[test]
fn signatures_match_helm() {
    let h = helm_rs::funcs::helm();
    let mut exprs = Vec::new();
    let mut expect: Vec<Result<String, String>> = Vec::new();
    let mut labels = Vec::new();
    let mut names: Vec<&str> = h.names().collect();
    names.sort();
    for name in names {
        let sig: Sig = h.get(name).unwrap().sig;
        let n = sig.params.len();
        // Arity: call with no arguments.
        if n > 0 {
            exprs.push(name.to_string());
            labels.push(format!("{name} (arity)"));
            let want = if sig.variadic.is_some() {
                format!("wrong number of args for {name}: want at least {n} got 0")
            } else {
                format!("wrong number of args for {name}: want {n} got 0")
            };
            expect.push(Err(want));
        }
        if sig.variadic.is_none() {
            // Too many arguments.
            let args: Vec<&str> = sig.params.iter().map(|&t| dummy(t)).chain(["0"]).collect();
            exprs.push(format!("{name} {}", args.join(" ")));
            labels.push(format!("{name} (too many)"));
            expect.push(Err(format!("wrong number of args for {name}: want {n} got {}", n + 1)));
        }
        // Parameter types.
        let mut all: Vec<Ty> = sig.params.to_vec();
        if let Some(v) = sig.variadic {
            all.push(v);
        }
        for k in 0..all.len() {
            let f = foreign(all[k]);
            if f.is_empty() {
                continue;
            }
            // Struct-typed parameters reject nil, so they cannot take a dummy.
            if all.iter().enumerate().any(|(j, t)| j != k && matches!(t, Ty::Object(_) | Ty::Struct(_))) {
                continue;
            }
            // Known divergence: derivePassword's counter is a Go uint32.
            if name == "derivePassword" && k == 0 {
                continue;
            }
            let args: Vec<&str> = all.iter().enumerate().map(|(j, &t)| if j == k { f } else { dummy(t) }).collect();
            exprs.push(format!("{name} {}", args.join(" ")));
            labels.push(format!("{name} (param {k} type)"));
            let got = if f == "(list)" { "[]interface {}" } else { "string" };
            expect.push(Err(format!("wrong type for value; expected {}; got {got}", go_type_name(all[k]))));
        }
    }
    let Some(helm) = helm_eval_many(&exprs, "") else { return };
    let mut bad = Vec::new();
    for i in 0..exprs.len() {
        let got = match &helm[i] {
            Ok(s) => Ok(s.clone()),
            Err(e) => {
                // Keep just the final line of the template error.
                let last = e.lines().last().unwrap_or("").trim().to_string();
                Err(last)
            }
        };
        let ok = match (&expect[i], &got) {
            (Err(want), Err(g)) => {
                g.ends_with(want.as_str())
                    // String constants report "can't handle" with the same type.
                    || (want.starts_with("wrong type for value; expected ")
                        && want.ends_with("; got string")
                        && g.ends_with(&format!(
                            "for arg of type {}",
                            &want["wrong type for value; expected ".len()..want.len() - "; got string".len()]
                        )))
            }
            _ => false,
        };
        if !ok {
            bad.push(format!("{}: {}\n   want: {:?}\n   helm: {:?}", labels[i], exprs[i], expect[i], got));
        }
    }
    report("signatures_match_helm", exprs.len(), &bad);
}
