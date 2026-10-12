//! Key and certificate generators (`genPrivateKey`, `genCA`, ...).

use alloc::string::String;

use crate::func::{FuncError, FuncMap, Sig, Ty};
use crate::value::{Kind, Object, Value};

/// Sprig's `certificate` struct (`.Cert`, `.Key`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    pub cert: String,
    pub key: String,
}

pub const CERT_TYPE: &str = "sprig.certificate";

impl Object for Certificate {
    fn type_name(&self) -> &str {
        CERT_TYPE
    }
    fn kind(&self) -> Kind {
        Kind::Struct
    }
    fn field(&self, name: &str) -> Option<Value> {
        match name {
            "Cert" => Some(Value::str(&self.cert)),
            "Key" => Some(Value::str(&self.key)),
            _ => None,
        }
    }
    fn format(&self, out: &mut String) {
        out.push('{');
        out.push_str(&self.cert);
        out.push(' ');
        out.push_str(&self.key);
        out.push('}');
    }
    fn to_data(&self) -> Value {
        crate::map! { "Cert" => self.cert.as_str(), "Key" => self.key.as_str() }
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

fn unsupported(_: &[Value]) -> crate::func::FuncResult {
    Err(FuncError::new("key and certificate generation is not supported by helm-rs"))
}

pub(super) fn register(f: &mut FuncMap) {
    const S: Ty = Ty::String;
    const I: Ty = Ty::Int;
    const L: Ty = Ty::List;
    const C: Ty = Ty::Object(CERT_TYPE);
    f.add("genPrivateKey", Sig::new(&[S]), unsupported);
    f.add("buildCustomCert", Sig::new(&[S, S]), unsupported);
    f.add("genCA", Sig::new(&[S, I]), unsupported);
    f.add("genCAWithKey", Sig::new(&[S, I, S]), unsupported);
    f.add("genSelfSignedCert", Sig::new(&[S, L, L, I]), unsupported);
    f.add("genSelfSignedCertWithKey", Sig::new(&[S, L, L, I, S]), unsupported);
    f.add("genSignedCert", Sig::new(&[S, L, L, I, C]), unsupported);
    f.add("genSignedCertWithKey", Sig::new(&[S, L, L, I, C, S]), unsupported);
}
