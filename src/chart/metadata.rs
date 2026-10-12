//! `Chart.yaml` metadata (Helm `chart.Metadata`, `chart.Dependency`,
//! `chart.Lock`): decoding, validation and the `.Chart` map.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::value::{Map, Value};

use super::decode::{Dec, match_field};
use super::gostr::{base, is_print, is_space, quote};

/// A chart maintainer (`chart.Maintainer`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Maintainer {
    pub name: String,
    pub email: String,
    pub url: String,
}

/// A chart dependency (`chart.Dependency`), from `Chart.yaml`,
/// `requirements.yaml` or a lock file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub version: String,
    pub repository: String,
    pub condition: String,
    pub tags: Option<Vec<String>>,
    pub enabled: bool,
    /// `import-values`: strings or `{child, parent}` maps.
    pub import_values: Option<Vec<Value>>,
    pub alias: String,
}

/// Chart metadata (`chart.Metadata`), i.e. `Chart.yaml`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    pub name: String,
    pub home: String,
    pub sources: Option<Vec<String>>,
    pub version: String,
    pub description: String,
    pub keywords: Option<Vec<String>>,
    pub maintainers: Option<Vec<Maintainer>>,
    pub icon: String,
    pub api_version: String,
    pub condition: String,
    pub tags: String,
    pub app_version: String,
    pub deprecated: bool,
    pub annotations: Option<BTreeMap<String, String>>,
    pub kube_version: String,
    /// `None` when absent (Go nil): dependency processing is skipped.
    pub dependencies: Option<Vec<Dependency>>,
    /// `type`: `application`, `library` or empty.
    pub chart_type: String,
}

/// `Chart.lock` / `requirements.lock` (`chart.Lock`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lock {
    pub generated: String,
    pub digest: String,
    pub dependencies: Option<Vec<Dependency>>,
}

/// Metadata as decoded, before validation (null list entries allowed).
#[derive(Clone, Debug, Default)]
pub(crate) struct RawMetadata {
    pub md: Metadata,
    pub maintainers: Option<Vec<Option<Maintainer>>>,
    pub dependencies: Option<Vec<Option<Dependency>>>,
}

const METADATA_FIELDS: &[&str] = &[
    "name",
    "home",
    "sources",
    "version",
    "description",
    "keywords",
    "maintainers",
    "icon",
    "apiVersion",
    "condition",
    "tags",
    "appVersion",
    "deprecated",
    "annotations",
    "kubeVersion",
    "dependencies",
    "type",
];

const DEPENDENCY_FIELDS: &[&str] = &[
    "name",
    "version",
    "repository",
    "condition",
    "tags",
    "enabled",
    "import-values",
    "alias",
];

const MAINTAINER_FIELDS: &[&str] = &["name", "email", "url"];

pub(crate) fn decode_maintainer(d: &mut Dec, m: &Map, out: &mut Maintainer) {
    for (k, v) in m.iter() {
        let Some(f) = match_field(MAINTAINER_FIELDS, k) else {
            continue;
        };
        d.push(k);
        match f {
            "name" => d.string(v, &mut out.name),
            "email" => d.string(v, &mut out.email),
            _ => d.string(v, &mut out.url),
        }
        d.pop();
    }
}

pub(crate) fn decode_dependency(d: &mut Dec, m: &Map, out: &mut Dependency) {
    for (k, v) in m.iter() {
        let Some(f) = match_field(DEPENDENCY_FIELDS, k) else {
            continue;
        };
        d.push(k);
        match f {
            "name" => d.string(v, &mut out.name),
            "version" => d.string(v, &mut out.version),
            "repository" => d.string(v, &mut out.repository),
            "condition" => d.string(v, &mut out.condition),
            "tags" => d.strings(v, &mut out.tags),
            "enabled" => d.bool(v, &mut out.enabled),
            "import-values" => d.any_list(v, &mut out.import_values),
            _ => d.string(v, &mut out.alias),
        }
        d.pop();
    }
}

fn decode_dependencies(d: &mut Dec, v: &Value, out: &mut Option<Vec<Option<Dependency>>>) {
    d.struct_ptr_list(
        v,
        out,
        "[]*v2.Dependency",
        "v2.Dependency",
        decode_dependency,
    );
}

impl RawMetadata {
    /// `json.Unmarshal` of the YAML document into the (existing) metadata.
    pub(crate) fn decode(&mut self, d: &mut Dec, v: &Value) {
        let m = match v {
            Value::Nil => return,
            Value::Map(m) => m.borrow(),
            other => return d.type_err(other, "v2.Metadata"),
        };
        let md = &mut self.md;
        for (k, v) in m.iter() {
            let Some(f) = match_field(METADATA_FIELDS, k) else {
                continue;
            };
            d.push(k);
            match f {
                "name" => d.string(v, &mut md.name),
                "home" => d.string(v, &mut md.home),
                "sources" => d.strings(v, &mut md.sources),
                "version" => d.string(v, &mut md.version),
                "description" => d.string(v, &mut md.description),
                "keywords" => d.strings(v, &mut md.keywords),
                "maintainers" => d.struct_ptr_list(
                    v,
                    &mut self.maintainers,
                    "[]*v2.Maintainer",
                    "v2.Maintainer",
                    decode_maintainer,
                ),
                "icon" => d.string(v, &mut md.icon),
                "apiVersion" => d.string(v, &mut md.api_version),
                "condition" => d.string(v, &mut md.condition),
                "tags" => d.string(v, &mut md.tags),
                "appVersion" => d.string(v, &mut md.app_version),
                "deprecated" => d.bool(v, &mut md.deprecated),
                "annotations" => d.string_map(v, &mut md.annotations),
                "kubeVersion" => d.string(v, &mut md.kube_version),
                "dependencies" => decode_dependencies(d, v, &mut self.dependencies),
                _ => d.string(v, &mut md.chart_type),
            }
            d.pop();
        }
    }

    /// `Metadata.Validate()`: sanitizes strings and checks required fields.
    /// Returns the Helm error message (`validation: ...`) on failure.
    pub(crate) fn validate(mut self) -> Result<Metadata, String> {
        let md = &mut self.md;
        for s in [
            &mut md.name,
            &mut md.description,
            &mut md.home,
            &mut md.icon,
            &mut md.condition,
            &mut md.tags,
            &mut md.app_version,
            &mut md.kube_version,
        ] {
            *s = sanitize(s);
        }
        for list in [&mut md.sources, &mut md.keywords].into_iter().flatten() {
            for s in list.iter_mut() {
                *s = sanitize(s);
            }
        }
        let verr = |m: String| alloc::format!("validation: {m}");
        if md.api_version.is_empty() {
            return Err(verr("chart.metadata.apiVersion is required".into()));
        }
        if md.name.is_empty() {
            return Err(verr("chart.metadata.name is required".into()));
        }
        if md.name == "." || md.name == ".." {
            return Err(verr(alloc::format!(
                "chart.metadata.name {} is not allowed",
                quote(&md.name)
            )));
        }
        if md.name != base(&md.name) {
            return Err(verr(alloc::format!(
                "chart.metadata.name {} is invalid",
                quote(&md.name)
            )));
        }
        if md.version.is_empty() {
            return Err(verr("chart.metadata.version is required".into()));
        }
        if !is_valid_semver(&md.version) {
            return Err(verr(alloc::format!(
                "chart.metadata.version {} is invalid",
                quote(&md.version)
            )));
        }
        if !matches!(md.chart_type.as_str(), "" | "application" | "library") {
            return Err(verr(
                "chart.metadata.type must be application or library".into(),
            ));
        }
        if let Some(ms) = self.maintainers.take() {
            let mut out = Vec::with_capacity(ms.len());
            for m in ms {
                let Some(mut m) = m else {
                    return Err(verr(
                        "maintainers must not contain empty or null nodes".into(),
                    ));
                };
                m.name = sanitize(&m.name);
                m.email = sanitize(&m.email);
                m.url = sanitize(&m.url);
                out.push(m);
            }
            md.maintainers = Some(out);
        }
        if let Some(deps) = self.dependencies.take() {
            let mut out: Vec<Dependency> = Vec::with_capacity(deps.len());
            for dep in deps {
                let Some(mut dep) = dep else {
                    return Err(verr(
                        "dependencies must not contain empty or null nodes".into(),
                    ));
                };
                validate_dependency(&mut dep)?;
                let key = if dep.alias.is_empty() {
                    &dep.name
                } else {
                    &dep.alias
                };
                if out.iter().any(|o| {
                    (if o.alias.is_empty() {
                        &o.name
                    } else {
                        &o.alias
                    }) == key
                }) {
                    return Err(verr(alloc::format!(
                        "more than one dependency with name or alias {}",
                        quote(key)
                    )));
                }
                out.push(dep);
            }
            md.dependencies = Some(out);
        }
        Ok(self.md)
    }
}

fn validate_dependency(d: &mut Dependency) -> Result<(), String> {
    d.name = sanitize(&d.name);
    d.version = sanitize(&d.version);
    d.repository = sanitize(&d.repository);
    d.condition = sanitize(&d.condition);
    if let Some(tags) = &mut d.tags {
        for t in tags.iter_mut() {
            *t = sanitize(t);
        }
    }
    if !d.alias.is_empty()
        && !d
            .alias
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(alloc::format!(
            "validation: dependency {} has disallowed characters in the alias",
            quote(&d.name)
        ));
    }
    Ok(())
}

fn is_valid_semver(v: &str) -> bool {
    crate::semver::Version::parse(v).is_ok()
}

/// Helm's `sanitizeString`: whitespace becomes a space, non-printable
/// characters are dropped.
pub(crate) fn sanitize(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            if is_space(c) {
                Some(' ')
            } else if is_print(c) {
                Some(c)
            } else {
                None
            }
        })
        .collect()
}

impl Lock {
    pub(crate) fn decode(d: &mut Dec, v: &Value, out: &mut Option<Lock>) {
        let m = match v {
            Value::Nil => {
                *out = None;
                return;
            }
            Value::Map(m) => m.borrow(),
            other => return d.type_err(other, "v2.Lock"),
        };
        let lock = out.get_or_insert_with(Lock::default);
        let mut deps: Option<Vec<Option<Dependency>>> = lock
            .dependencies
            .take()
            .map(|v| v.into_iter().map(Some).collect());
        for (k, v) in m.iter() {
            let Some(f) = match_field(&["generated", "digest", "dependencies"], k) else {
                continue;
            };
            d.push(k);
            match f {
                "generated" => match v {
                    Value::Nil => {}
                    Value::String(s) => lock.generated = s.to_string(),
                    other => d.type_err(other, "time.Time"),
                },
                "digest" => d.string(v, &mut lock.digest),
                _ => decode_dependencies(d, v, &mut deps),
            }
            d.pop();
        }
        lock.dependencies = deps.map(|v| v.into_iter().map(Option::unwrap_or_default).collect());
    }
}

fn str_list(l: &Option<Vec<String>>) -> Value {
    Value::list(l.iter().flatten().map(|s| Value::str(s)).collect())
}

impl Dependency {
    /// Go `structToMap` of a dependency.
    pub fn as_map(&self) -> Value {
        let mut m = Map::new();
        m.insert("Name".into(), Value::str(&self.name));
        m.insert("Version".into(), Value::str(&self.version));
        m.insert("Repository".into(), Value::str(&self.repository));
        m.insert("Condition".into(), Value::str(&self.condition));
        m.insert("Tags".into(), str_list(&self.tags));
        m.insert("Enabled".into(), Value::Bool(self.enabled));
        m.insert(
            "ImportValues".into(),
            Value::list(self.import_values.iter().flatten().cloned().collect()),
        );
        m.insert("Alias".into(), Value::str(&self.alias));
        Value::map(m)
    }
}

impl Metadata {
    /// What templates see as `.Chart` (Helm's `MetadataAsMap`, i.e.
    /// `structToMap`): Go field names as keys, every field present.
    pub fn as_map(&self) -> Value {
        let mut m = Map::new();
        m.insert("Name".into(), Value::str(&self.name));
        m.insert("Home".into(), Value::str(&self.home));
        m.insert("Sources".into(), str_list(&self.sources));
        m.insert("Version".into(), Value::str(&self.version));
        m.insert("Description".into(), Value::str(&self.description));
        m.insert("Keywords".into(), str_list(&self.keywords));
        let maint = self
            .maintainers
            .iter()
            .flatten()
            .map(|mt| {
                let mut mm = Map::new();
                mm.insert("Name".into(), Value::str(&mt.name));
                mm.insert("Email".into(), Value::str(&mt.email));
                mm.insert("URL".into(), Value::str(&mt.url));
                Value::map(mm)
            })
            .collect();
        m.insert("Maintainers".into(), Value::list(maint));
        m.insert("Icon".into(), Value::str(&self.icon));
        m.insert("APIVersion".into(), Value::str(&self.api_version));
        m.insert("Condition".into(), Value::str(&self.condition));
        m.insert("Tags".into(), Value::str(&self.tags));
        m.insert("AppVersion".into(), Value::str(&self.app_version));
        m.insert("Deprecated".into(), Value::Bool(self.deprecated));
        // Go `map[string]string`; nil when absent.
        let ann = match &self.annotations {
            Some(a) => {
                Value::string_map(a.iter().map(|(k, v)| (k.clone(), Value::str(v))).collect())
            }
            None => Value::nil_map("map[string]string", Some(Value::str(""))),
        };
        m.insert("Annotations".into(), ann);
        m.insert("KubeVersion".into(), Value::str(&self.kube_version));
        m.insert(
            "Dependencies".into(),
            Value::list(
                self.dependencies
                    .iter()
                    .flatten()
                    .map(Dependency::as_map)
                    .collect(),
            ),
        );
        m.insert("Type".into(), Value::str(&self.chart_type));
        Value::map(m)
    }

    /// Parses a `Chart.yaml` document that was already parsed into a
    /// [`Value`] (YAML → JSON → struct, like `sigs.k8s.io/yaml`) and
    /// validates it. Errors are Helm's messages.
    pub fn from_value(v: &Value) -> Result<Metadata, String> {
        let mut raw = RawMetadata::default();
        let mut d = Dec::new();
        raw.decode(&mut d, v);
        if let Some(e) = d.err {
            return Err(alloc::format!(
                "error unmarshaling JSON: while decoding JSON: {e}"
            ));
        }
        if raw.md.api_version.is_empty() {
            raw.md.api_version = "v1".to_string();
        }
        raw.validate()
    }
}
