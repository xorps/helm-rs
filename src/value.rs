//! The dynamic value model.
//!
//! Helm templates operate on Go `interface{}` values. [`Value`] mirrors the
//! subset of Go types that occur in practice, keeping the distinctions that are
//! observable from templates (e.g. `int` vs `int64` vs `float64` for `typeOf`
//! and `eq`, and map reference semantics for `set`/`unset`/`merge`).

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Ref, RefCell, RefMut};
use core::fmt;

use crate::func::{FuncError, Sig};

/// Go `map[string]interface{}` contents. Keys iterate in byte order, which is
/// the order Go's templates, `fmt` and `encoding/json` use.
pub type Map = BTreeMap<String, Value>;

/// A dynamically typed template value.
#[derive(Clone, Default)]
pub enum Value {
    /// Go `nil` (an empty `interface{}`).
    #[default]
    Nil,
    Bool(bool),
    /// Go `int`.
    Int(i64),
    /// Go `int64`.
    Int64(i64),
    /// Go `uint64`.
    Uint64(u64),
    /// Go `float64`. All numbers parsed from YAML/JSON values are floats,
    /// exactly as in Helm.
    Float(f64),
    String(Rc<str>),
    /// Go `[]byte`.
    Bytes(Rc<[u8]>),
    /// Go `[]interface{}` (also used for `[]string`).
    List(Rc<Vec<Value>>),
    /// Go `map[string]interface{}`. Maps are reference types: clones share
    /// storage, so `set` on a map is visible through every alias.
    Map(MapRef),
    /// A Go struct or other typed value with fields and methods
    /// (e.g. `.Capabilities`, `.Files`, semver versions).
    Object(Rc<dyn Object>),
}

/// Shared, mutable map with Go reference semantics.
#[derive(Clone, Default)]
pub struct MapRef(Rc<RefCell<Map>>);

impl MapRef {
    pub fn new(map: Map) -> Self {
        MapRef(Rc::new(RefCell::new(map)))
    }
    pub fn borrow(&self) -> Ref<'_, Map> {
        self.0.borrow()
    }
    pub fn borrow_mut(&self) -> RefMut<'_, Map> {
        self.0.borrow_mut()
    }
    pub fn get(&self, key: &str) -> Option<Value> {
        self.0.borrow().get(key).cloned()
    }
    pub fn insert(&self, key: impl Into<String>, value: Value) -> Option<Value> {
        self.0.borrow_mut().insert(key.into(), value)
    }
    pub fn remove(&self, key: &str) -> Option<Value> {
        self.0.borrow_mut().remove(key)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.borrow().contains_key(key)
    }
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }
    /// Same underlying storage (Go map identity).
    pub fn ptr_eq(&self, other: &MapRef) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
    /// Shallow copy into a new, independent map.
    pub fn shallow_clone(&self) -> MapRef {
        MapRef::new(self.0.borrow().clone())
    }
    /// Recursive copy: nested maps are copied too, so the result shares no
    /// mutable storage with `self`.
    pub fn deep_clone(&self) -> MapRef {
        let m = self.0.borrow();
        MapRef::new(m.iter().map(|(k, v)| (k.clone(), v.deep_clone())).collect())
    }
}

/// Go `reflect.Kind`, as reported by `kindOf` and used by `kindIs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Invalid,
    Bool,
    Int,
    Int64,
    Uint64,
    Float64,
    String,
    Slice,
    Map,
    Struct,
    Ptr,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Invalid => "invalid",
            Kind::Bool => "bool",
            Kind::Int => "int",
            Kind::Int64 => "int64",
            Kind::Uint64 => "uint64",
            Kind::Float64 => "float64",
            Kind::String => "string",
            Kind::Slice => "slice",
            Kind::Map => "map",
            Kind::Struct => "struct",
            Kind::Ptr => "ptr",
        }
    }
}

/// A Go typed value with fields and/or methods, such as `.Capabilities` or
/// `.Files`. Implement this to expose custom objects to templates.
pub trait Object: fmt::Debug {
    /// Go type name as printed by `%T` / `typeOf`, e.g. `"engine.files"`.
    fn type_name(&self) -> &str;

    /// Go kind (for `kindOf`). Most objects are structs or pointers to them.
    fn kind(&self) -> Kind {
        Kind::Struct
    }

    /// Value of an exported field. Return `None` if there is no such field.
    fn field(&self, _name: &str) -> Option<Value> {
        None
    }

    /// Signature of a method, or `None` if there is no such method.
    /// Methods take precedence over fields, as in Go.
    fn method(&self, _name: &str) -> Option<Sig> {
        None
    }

    /// Calls a method previously resolved with [`Object::method`]. Arguments
    /// have already been checked and converted according to its signature.
    fn call_method(&self, name: &str, _args: &[Value]) -> Result<Value, FuncError> {
        Err(FuncError::new(alloc::format!("no method {name}")))
    }

    /// `%v` formatting. Objects implementing Go's `fmt.Stringer` should print
    /// what `String()` returns; others print like a Go struct (`{a b}`).
    fn format(&self, out: &mut String);

    /// The value `encoding/json` would marshal this object as, used by
    /// `toJson`/`toYaml`. Defaults to `nil`.
    fn to_data(&self) -> Value {
        Value::Nil
    }

    /// Go truthiness (structs are always true; numeric-like objects such as
    /// `time.Duration` may override this).
    fn truthy(&self) -> bool {
        true
    }

    /// Lets functions downcast to concrete object types.
    fn as_any(&self) -> &dyn core::any::Any;
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::String(Rc::from(s))
    }
    pub fn string(s: String) -> Value {
        Value::String(Rc::from(s))
    }
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }
    pub fn map(map: Map) -> Value {
        Value::Map(MapRef::new(map))
    }
    pub fn empty_map() -> Value {
        Value::map(Map::new())
    }
    pub fn object(o: impl Object + 'static) -> Value {
        Value::Object(Rc::new(o))
    }

    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_map(&self) -> Option<&MapRef> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&dyn Object> {
        match self {
            Value::Object(o) => Some(&**o),
            _ => None,
        }
    }

    /// Go `reflect.Kind` of the dynamic value.
    pub fn kind(&self) -> Kind {
        match self {
            Value::Nil => Kind::Invalid,
            Value::Bool(_) => Kind::Bool,
            Value::Int(_) => Kind::Int,
            Value::Int64(_) => Kind::Int64,
            Value::Uint64(_) => Kind::Uint64,
            Value::Float(_) => Kind::Float64,
            Value::String(_) => Kind::String,
            Value::Bytes(_) | Value::List(_) => Kind::Slice,
            Value::Map(_) => Kind::Map,
            Value::Object(o) => o.kind(),
        }
    }

    /// Go type name as printed by `%T`.
    pub fn type_name(&self) -> String {
        match self {
            Value::Nil => "<nil>".into(),
            Value::Bool(_) => "bool".into(),
            Value::Int(_) => "int".into(),
            Value::Int64(_) => "int64".into(),
            Value::Uint64(_) => "uint64".into(),
            Value::Float(_) => "float64".into(),
            Value::String(_) => "string".into(),
            Value::Bytes(_) => "[]uint8".into(),
            Value::List(_) => "[]interface {}".into(),
            Value::Map(_) => "map[string]interface {}".into(),
            Value::Object(o) => o.type_name().to_string(),
        }
    }

    /// Go template truthiness (`if`, `with`, `and`, `or`, `not`): false for
    /// nil, false, zero numbers, and empty strings/slices/maps.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::Int(i) | Value::Int64(i) => *i != 0,
            Value::Uint64(u) => *u != 0,
            Value::Float(f) => *f != 0.0,
            Value::String(s) => !s.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Map(m) => !m.is_empty(),
            Value::Object(o) => o.truthy(),
        }
    }

    /// Recursive copy that shares no mutable storage with `self`.
    pub fn deep_clone(&self) -> Value {
        match self {
            Value::Map(m) => Value::Map(m.deep_clone()),
            Value::List(l) => Value::list(l.iter().map(Value::deep_clone).collect()),
            v => v.clone(),
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::str(s)
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::string(s)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}
impl From<Vec<Value>> for Value {
    fn from(l: Vec<Value>) -> Self {
        Value::list(l)
    }
}
impl From<Map> for Value {
    fn from(m: Map) -> Self {
        Value::map(m)
    }
}

/// Structural equality (maps and lists compared by content, objects by
/// identity). Number variants of different Go types are never equal, as with
/// Go's `reflect.DeepEqual`.
impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Int64(a), Value::Int64(b)) => a == b,
            (Value::Uint64(a), Value::Uint64(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a.ptr_eq(b) || *a.borrow() == *b.borrow(),
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => f.write_str("nil"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Int64(i) => write!(f, "int64({i})"),
            Value::Uint64(u) => write!(f, "uint64({u})"),
            Value::Float(x) => write!(f, "float64({x})"),
            Value::String(s) => write!(f, "{s:?}"),
            Value::Bytes(b) => write!(f, "bytes({b:?})"),
            Value::List(l) => f.debug_list().entries(l.iter()).finish(),
            Value::Map(m) => f.debug_map().entries(m.borrow().iter()).finish(),
            Value::Object(o) => write!(f, "{o:?}"),
        }
    }
}

impl fmt::Debug for MapRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.borrow().iter()).finish()
    }
}

/// Builds a [`Value::Map`] from `key => value` pairs.
#[macro_export]
macro_rules! map {
    () => { $crate::Value::empty_map() };
    ($($k:expr => $v:expr),+ $(,)?) => {{
        let mut m = $crate::value::Map::new();
        $( m.insert($crate::__private::String::from($k), $crate::Value::from($v)); )+
        $crate::Value::map(m)
    }};
}
