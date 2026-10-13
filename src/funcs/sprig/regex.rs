//! `regex.go`.

use super::goregex::{GoRegex, compile, must_compile_message, quote_meta};
use super::{iarg, sarg, str_list};
use crate::func::{FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::value::Value;

const S: Ty = Ty::String;
const I: Ty = Ty::Int;

fn must(expr: &str) -> Result<GoRegex, FuncError> {
    compile(expr).map_err(|e| FuncError::new(must_compile_message(expr, &e)))
}

fn checked(expr: &str) -> Result<GoRegex, FuncError> {
    compile(expr).map_err(FuncError::new)
}

fn find_all(re: GoRegex, a: &[Value]) -> FuncResult {
    Ok(str_list(re.find_all_string(sarg(a, 1), iarg(a, 2))))
}

fn split(re: GoRegex, a: &[Value]) -> FuncResult {
    Ok(str_list(re.split(sarg(a, 1), iarg(a, 2))))
}

pub(super) fn register(f: &mut FuncMap) {
    f.add("regexMatch", Sig::new(&[S, S]), |a| {
        Ok(Value::Bool(
            compile(sarg(a, 0))
                .map(|re| re.is_match(sarg(a, 1)))
                .unwrap_or(false),
        ))
    });
    f.add("mustRegexMatch", Sig::new(&[S, S]), |a| {
        Ok(Value::Bool(checked(sarg(a, 0))?.is_match(sarg(a, 1))))
    });
    f.add("regexFindAll", Sig::new(&[S, S, I]), |a| {
        find_all(must(sarg(a, 0))?, a)
    });
    f.add("mustRegexFindAll", Sig::new(&[S, S, I]), |a| {
        find_all(checked(sarg(a, 0))?, a)
    });
    f.add("regexFind", Sig::new(&[S, S]), |a| {
        Ok(Value::str(must(sarg(a, 0))?.find_string(sarg(a, 1))))
    });
    f.add("mustRegexFind", Sig::new(&[S, S]), |a| {
        Ok(Value::str(checked(sarg(a, 0))?.find_string(sarg(a, 1))))
    });
    f.add("regexReplaceAll", Sig::new(&[S, S, S]), |a| {
        Ok(Value::string(must(sarg(a, 0))?.replace_all(
            sarg(a, 1),
            sarg(a, 2),
            false,
        )))
    });
    f.add("mustRegexReplaceAll", Sig::new(&[S, S, S]), |a| {
        Ok(Value::string(checked(sarg(a, 0))?.replace_all(
            sarg(a, 1),
            sarg(a, 2),
            false,
        )))
    });
    f.add("regexReplaceAllLiteral", Sig::new(&[S, S, S]), |a| {
        Ok(Value::string(must(sarg(a, 0))?.replace_all(
            sarg(a, 1),
            sarg(a, 2),
            true,
        )))
    });
    f.add("mustRegexReplaceAllLiteral", Sig::new(&[S, S, S]), |a| {
        Ok(Value::string(checked(sarg(a, 0))?.replace_all(
            sarg(a, 1),
            sarg(a, 2),
            true,
        )))
    });
    f.add("regexSplit", Sig::new(&[S, S, I]), |a| {
        split(must(sarg(a, 0))?, a)
    });
    f.add("mustRegexSplit", Sig::new(&[S, S, I]), |a| {
        split(checked(sarg(a, 0))?, a)
    });
    f.add("regexQuoteMeta", Sig::new(&[S]), |a| {
        Ok(Value::string(quote_meta(sarg(a, 0))))
    });
}
