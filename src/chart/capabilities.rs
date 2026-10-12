//! `.Capabilities` (Helm `common.Capabilities`) as template objects.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::Any;

use crate::func::{FuncError, Sig, Ty};
use crate::value::{Kind, Map, Object, Value};

/// `common.DefaultVersionSet` of Helm v4.3.0 (client-go v0.37 scheme), in
/// Helm's order.
pub const DEFAULT_API_VERSIONS: &[&str] = &[
    "v1",
    "admissionregistration.k8s.io/v1",
    "admissionregistration.k8s.io/v1alpha1",
    "admissionregistration.k8s.io/v1beta1",
    "internal.apiserver.k8s.io/v1alpha1",
    "apps/v1",
    "apps/v1beta1",
    "apps/v1beta2",
    "authentication.k8s.io/v1",
    "authentication.k8s.io/v1alpha1",
    "authentication.k8s.io/v1beta1",
    "authorization.k8s.io/v1",
    "authorization.k8s.io/v1beta1",
    "autoscaling/v1",
    "autoscaling/v2",
    "batch/v1",
    "batch/v1beta1",
    "certificates.k8s.io/v1",
    "certificates.k8s.io/v1beta1",
    "certificates.k8s.io/v1alpha1",
    "coordination.k8s.io/v1alpha2",
    "coordination.k8s.io/v1beta1",
    "coordination.k8s.io/v1",
    "discovery.k8s.io/v1",
    "discovery.k8s.io/v1beta1",
    "events.k8s.io/v1",
    "events.k8s.io/v1beta1",
    "extensions/v1beta1",
    "flowcontrol.apiserver.k8s.io/v1",
    "flowcontrol.apiserver.k8s.io/v1beta1",
    "flowcontrol.apiserver.k8s.io/v1beta2",
    "flowcontrol.apiserver.k8s.io/v1beta3",
    "lifecycle.k8s.io/v1alpha1",
    "networking.k8s.io/v1",
    "networking.k8s.io/v1beta1",
    "node.k8s.io/v1",
    "node.k8s.io/v1alpha1",
    "node.k8s.io/v1beta1",
    "policy/v1",
    "policy/v1beta1",
    "rbac.authorization.k8s.io/v1",
    "rbac.authorization.k8s.io/v1beta1",
    "rbac.authorization.k8s.io/v1alpha1",
    "resource.k8s.io/v1",
    "resource.k8s.io/v1beta2",
    "resource.k8s.io/v1beta1",
    "resource.k8s.io/v1alpha3",
    "scheduling.k8s.io/v1alpha3",
    "scheduling.k8s.io/v1beta1",
    "scheduling.k8s.io/v1",
    "storage.k8s.io/v1beta1",
    "storage.k8s.io/v1",
    "storage.k8s.io/v1alpha1",
    "storagemigration.k8s.io/v1",
    "storagemigration.k8s.io/v1beta1",
    "apiextensions.k8s.io/v1beta1",
    "apiextensions.k8s.io/v1",
];

/// Default Kubernetes version of Helm v4.3.0 (`helm template` without
/// `--kube-version`).
pub const DEFAULT_KUBE_VERSION: &str = "v1.37.0";

/// `common.KubeVersion`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubeVersion {
    /// Full version as given, with a `v` prefix (`GitVersion()`).
    pub version: String,
    /// Normalized (`v` + numeric components), what `String()` returns.
    pub normalized: String,
    pub major: String,
    pub minor: String,
}

impl KubeVersion {
    /// `common.ParseKubeVersion` (apimachinery `version.ParseGeneric`).
    /// Errors are Go's messages.
    pub fn parse(version: &str) -> Result<KubeVersion, String> {
        // ^\s*v?([0-9]+(?:\.[0-9]+)*)(.*)*$
        let s = version.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let s = s.strip_prefix('v').unwrap_or(s);
        let num_end = {
            let b = s.as_bytes();
            let mut i = 0;
            let mut last_good = 0;
            loop {
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    break;
                }
                last_good = i;
                if i < b.len() && b[i] == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
                    i += 1;
                } else {
                    break;
                }
            }
            last_good
        };
        let quoted = super::gostr::quote(version);
        if num_end == 0 || s[num_end..].contains('\n') {
            return Err(alloc::format!("could not parse {quoted} as version"));
        }
        let numbers = &s[..num_end];
        let comps: Vec<&str> = numbers.split('.').collect();
        if comps.len() < 2 {
            return Err(alloc::format!("illegal version string {quoted}"));
        }
        let mut parsed = Vec::new();
        for (i, c) in comps.iter().enumerate() {
            if i == 0 && c.starts_with('0') && *c != "0" {
                return Err(alloc::format!(
                    "illegal zero-prefixed version component {} in {quoted}",
                    super::gostr::quote(c)
                ));
            }
            match c.parse::<u64>() {
                Ok(n) => parsed.push(n),
                Err(_) => {
                    return Err(alloc::format!(
                        "illegal non-numeric version component {} in {quoted}: strconv.ParseUint: parsing {}: value out of range",
                        super::gostr::quote(c),
                        super::gostr::quote(c)
                    ));
                }
            }
        }
        let normalized = alloc::format!(
            "v{}",
            parsed.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(".")
        );
        let git = if version.starts_with('v') { version.to_string() } else { alloc::format!("v{version}") };
        Ok(KubeVersion {
            version: git,
            normalized,
            major: parsed[0].to_string(),
            minor: parsed[1].to_string(),
        })
    }
}

impl Object for KubeVersion {
    fn type_name(&self) -> &str {
        "common.KubeVersion"
    }
    fn field(&self, name: &str) -> Option<Value> {
        match name {
            "Version" => Some(Value::str(&self.version)),
            "Major" => Some(Value::str(&self.major)),
            "Minor" => Some(Value::str(&self.minor)),
            _ => None,
        }
    }
    fn method(&self, name: &str) -> Option<Sig> {
        matches!(name, "String" | "GitVersion").then_some(Sig::new(&[]))
    }
    fn call_method(&self, name: &str, _args: &[Value]) -> Result<Value, FuncError> {
        match name {
            "String" => Ok(Value::str(self.string())),
            "GitVersion" => Ok(Value::str(&self.version)),
            _ => Err(FuncError::new(alloc::format!("no method {name}"))),
        }
    }
    /// Templates print the (addressable) field through its `String()` method.
    fn format(&self, out: &mut String) {
        out.push_str(self.string());
    }
    fn to_data(&self) -> Value {
        let mut m = Map::new();
        m.insert("Version".into(), Value::str(&self.version));
        m.insert("Major".into(), Value::str(&self.major));
        m.insert("Minor".into(), Value::str(&self.minor));
        Value::map(m)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl KubeVersion {
    /// `KubeVersion.String()`.
    pub fn string(&self) -> &str {
        if self.normalized.is_empty() { &self.version } else { &self.normalized }
    }

    /// Go `%v` of the struct value (`{Version normalized Major Minor}`).
    pub fn go_struct_string(&self) -> String {
        alloc::format!("{{{} {} {} {}}}", self.version, self.normalized, self.major, self.minor)
    }
}

/// `common.VersionSet`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionSet(pub Vec<String>);

impl Object for VersionSet {
    fn type_name(&self) -> &str {
        "common.VersionSet"
    }
    fn kind(&self) -> Kind {
        Kind::Slice
    }
    fn method(&self, name: &str) -> Option<Sig> {
        (name == "Has").then_some(Sig::new(&[Ty::String]))
    }
    fn call_method(&self, name: &str, args: &[Value]) -> Result<Value, FuncError> {
        match (name, args) {
            ("Has", [Value::String(s)]) => Ok(Value::Bool(self.0.iter().any(|v| **v == **s))),
            _ => Err(FuncError::new(alloc::format!("no method {name}"))),
        }
    }
    fn format(&self, out: &mut String) {
        out.push('[');
        out.push_str(&self.0.join(" "));
        out.push(']');
    }
    /// The list of strings (also what `range`/`index`/`len` operate on).
    fn to_data(&self) -> Value {
        Value::list(self.0.iter().map(|s| Value::str(s)).collect())
    }
    fn truthy(&self) -> bool {
        !self.0.is_empty()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `version.BuildInfo` (`.Capabilities.HelmVersion`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    pub git_commit: String,
    pub git_tree_state: String,
    pub go_version: String,
    pub kube_client_version: String,
}

impl Default for BuildInfo {
    /// What the official Helm v4.3.0 release binary reports.
    fn default() -> Self {
        BuildInfo {
            version: "v4.3.0".into(),
            git_commit: "bec5b06ed841fe5269972d864d5177944fd5970f".into(),
            git_tree_state: "clean".into(),
            go_version: "go1.27.1".into(),
            kube_client_version: "v1.37".into(),
        }
    }
}

impl Object for BuildInfo {
    fn type_name(&self) -> &str {
        "version.BuildInfo"
    }
    fn field(&self, name: &str) -> Option<Value> {
        match name {
            "Version" => Some(Value::str(&self.version)),
            "GitCommit" => Some(Value::str(&self.git_commit)),
            "GitTreeState" => Some(Value::str(&self.git_tree_state)),
            "GoVersion" => Some(Value::str(&self.go_version)),
            "KubeClientVersion" => Some(Value::str(&self.kube_client_version)),
            _ => None,
        }
    }
    fn format(&self, out: &mut String) {
        out.push_str(&alloc::format!(
            "{{{} {} {} {} {}}}",
            self.version, self.git_commit, self.git_tree_state, self.go_version, self.kube_client_version
        ));
    }
    fn to_data(&self) -> Value {
        let mut m = Map::new();
        for (k, v) in [
            ("version", &self.version),
            ("git_commit", &self.git_commit),
            ("git_tree_state", &self.git_tree_state),
            ("go_version", &self.go_version),
        ] {
            if !v.is_empty() {
                m.insert(k.into(), Value::str(v));
            }
        }
        m.insert("kube_client_version".into(), Value::str(&self.kube_client_version));
        Value::map(m)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `*common.Capabilities` as seen by templates.
#[derive(Clone, Debug)]
pub struct CapabilitiesObject {
    pub kube_version: Rc<KubeVersion>,
    pub api_versions: Rc<VersionSet>,
    pub helm_version: Rc<BuildInfo>,
}

impl Object for CapabilitiesObject {
    fn type_name(&self) -> &str {
        "*common.Capabilities"
    }
    fn kind(&self) -> Kind {
        Kind::Ptr
    }
    fn field(&self, name: &str) -> Option<Value> {
        match name {
            "KubeVersion" => Some(Value::Object(self.kube_version.clone())),
            "APIVersions" => Some(Value::Object(self.api_versions.clone())),
            "HelmVersion" => Some(Value::Object(self.helm_version.clone())),
            _ => None,
        }
    }
    fn format(&self, out: &mut String) {
        out.push_str("&{");
        out.push_str(&self.kube_version.go_struct_string());
        out.push(' ');
        self.api_versions.format(out);
        out.push(' ');
        self.helm_version.format(out);
        out.push('}');
    }
    fn to_data(&self) -> Value {
        let mut m = Map::new();
        m.insert("KubeVersion".into(), self.kube_version.to_data());
        m.insert("APIVersions".into(), self.api_versions.to_data());
        m.insert("HelmVersion".into(), self.helm_version.to_data());
        Value::map(m)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kube_version() {
        let kv = KubeVersion::parse("1.33.4-gke.1245000").unwrap();
        assert_eq!(kv.version, "v1.33.4-gke.1245000");
        assert_eq!(kv.normalized, "v1.33.4");
        assert_eq!((kv.major.as_str(), kv.minor.as_str()), ("1", "33"));
        assert_eq!(KubeVersion::parse("v1.2").unwrap().normalized, "v1.2");
        assert!(KubeVersion::parse("1").is_err());
        assert!(KubeVersion::parse("abc").is_err());
    }
}
