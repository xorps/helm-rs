//! `reflect.go` (typeOf & co.), `deepEqual`, and `semver.go`.

use super::{arg, deep_equal, sarg};
use crate::func::{FuncError, FuncMap, Sig, Ty};
use crate::semver::{Constraints, Version, VersionObject};
use crate::value::Value;

const A: Ty = Ty::Any;
const S: Ty = Ty::String;

pub(super) fn register(f: &mut FuncMap) {
    f.add("typeOf", Sig::new(&[A]), |a| Ok(Value::string(arg(a, 0).type_name())));
    f.add("typeIs", Sig::new(&[S, A]), |a| Ok(Value::Bool(sarg(a, 0) == arg(a, 1).type_name())));
    f.add("typeIsLike", Sig::new(&[S, A]), |a| {
        let t = arg(a, 1).type_name();
        let target = sarg(a, 0);
        Ok(Value::Bool(target == t || t.strip_prefix('*') == Some(target)))
    });
    f.add("kindOf", Sig::new(&[A]), |a| Ok(Value::str(arg(a, 0).kind().name())));
    f.add("kindIs", Sig::new(&[S, A]), |a| Ok(Value::Bool(sarg(a, 0) == arg(a, 1).kind().name())));
    f.add("deepEqual", Sig::new(&[A, A]), |a| Ok(Value::Bool(deep_equal(arg(a, 0), arg(a, 1)))));
    f.add("semver", Sig::new(&[S]), |a| Version::parse(sarg(a, 0)).map(VersionObject::ptr).map_err(FuncError::new));
    f.add("semverCompare", Sig::new(&[S, S]), |a| {
        let c = Constraints::parse(sarg(a, 0)).map_err(FuncError::new)?;
        let v = Version::parse(sarg(a, 1)).map_err(FuncError::new)?;
        Ok(Value::Bool(c.check(&v)))
    });
}
