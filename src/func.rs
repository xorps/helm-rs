//! Template function definitions.
//!
//! Go's `text/template` checks and converts arguments against each function's
//! static Go signature (and number literals are typed by the parameter they are
//! passed to). [`Sig`] describes that signature so the executor can reproduce
//! Go's behavior, including its errors.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use core::fmt;

use crate::value::Value;

/// A Go parameter type, as far as argument conversion is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    /// `interface{}`: accepts anything, including nil.
    Any,
    String,
    Bool,
    /// Go `int`.
    Int,
    Int64,
    Uint64,
    Float64,
    /// `[]byte`
    Bytes,
    /// `[]interface{}`
    List,
    /// `[]string`
    Strings,
    /// `map[string]interface{}`
    Map,
    /// A named object type ([`crate::Object::type_name`]); also accepts nil.
    Object(&'static str),
}

impl Ty {
    /// Go spelling of the type, used in error messages.
    pub fn go_name(self) -> &'static str {
        match self {
            Ty::Any => "interface {}",
            Ty::String => "string",
            Ty::Bool => "bool",
            Ty::Int => "int",
            Ty::Int64 => "int64",
            Ty::Uint64 => "uint64",
            Ty::Float64 => "float64",
            Ty::Bytes => "[]uint8",
            Ty::List => "[]interface {}",
            Ty::Strings => "[]string",
            Ty::Map => "map[string]interface {}",
            Ty::Object(name) => name,
        }
    }
}

/// A Go function signature: fixed parameters plus an optional variadic tail.
/// Whether the Go function also returns an `error` does not matter here: any
/// function may fail by returning `Err`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sig {
    pub params: &'static [Ty],
    pub variadic: Option<Ty>,
}

impl Sig {
    pub const fn new(params: &'static [Ty]) -> Sig {
        Sig { params, variadic: None }
    }
    pub const fn variadic(params: &'static [Ty], rest: Ty) -> Sig {
        Sig { params, variadic: Some(rest) }
    }
}

/// An error returned by a template function. Go prints it as
/// `error calling NAME: MESSAGE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuncError {
    pub message: String,
}

impl FuncError {
    pub fn new(message: impl Into<String>) -> Self {
        FuncError { message: message.into() }
    }
}

impl fmt::Display for FuncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for FuncError {
    fn from(message: String) -> Self {
        FuncError { message }
    }
}

impl From<&str> for FuncError {
    fn from(message: &str) -> Self {
        FuncError { message: message.into() }
    }
}

/// Calendar time as seen by templates (`now`, `date`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp {
    /// Seconds since the Unix epoch.
    pub unix_seconds: i64,
    /// Nanoseconds within the second.
    pub nanos: u32,
    /// Offset of the local time zone from UTC, in seconds.
    pub utc_offset_seconds: i32,
}

/// The outside world, as far as template functions are concerned. `no_std`
/// has no clock, randomness or network, so they are supplied by the host.
///
/// The default implementation ([`DeterministicEnv`]) is fully reproducible.
pub trait Env {
    /// Current time, for `now`, `date` and friends.
    fn now(&self) -> Timestamp {
        Timestamp { unix_seconds: 0, nanos: 0, utc_offset_seconds: 0 }
    }
    /// Fills `buf` with random bytes, for `randAlphaNum`, `uuidv4`, `genCA`, ...
    fn fill_random(&mut self, buf: &mut [u8]);
    /// Kubernetes `lookup` function. Helm returns an empty map when it is not
    /// connected to a cluster, which is the default here.
    fn lookup(
        &mut self,
        _api_version: &str,
        _kind: &str,
        _namespace: &str,
        _name: &str,
    ) -> Result<Value, FuncError> {
        Ok(Value::empty_map())
    }
    /// DNS lookup for `getHostByName`. Helm returns "" unless DNS is enabled.
    fn get_host_by_name(&mut self, _name: &str) -> Result<String, FuncError> {
        Ok(String::new())
    }
}

/// A reproducible [`Env`]: fixed clock at the Unix epoch and a seeded PRNG.
#[derive(Clone, Debug)]
pub struct DeterministicEnv {
    state: u64,
}

impl DeterministicEnv {
    pub fn new(seed: u64) -> Self {
        DeterministicEnv { state: seed ^ 0x9E37_79B9_7F4A_7C15 }
    }
}

impl Default for DeterministicEnv {
    fn default() -> Self {
        DeterministicEnv::new(0)
    }
}

impl Env for DeterministicEnv {
    fn fill_random(&mut self, buf: &mut [u8]) {
        // splitmix64
        for chunk in buf.chunks_mut(8) {
            self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
    }
}

/// Context passed to every function call.
pub struct CallCtx<'a> {
    pub env: &'a mut dyn Env,
}

/// Function implementation. Arguments arrive converted per the [`Sig`].
pub type FuncImpl = Rc<dyn Fn(&mut CallCtx<'_>, &[Value]) -> Result<Value, FuncError>>;

/// A template function: signature plus implementation.
#[derive(Clone)]
pub struct Func {
    pub sig: Sig,
    pub imp: FuncImpl,
}

impl Func {
    pub fn new(
        sig: Sig,
        f: impl Fn(&mut CallCtx<'_>, &[Value]) -> Result<Value, FuncError> + 'static,
    ) -> Func {
        Func { sig, imp: Rc::new(f) }
    }
}

impl fmt::Debug for Func {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Func({:?})", self.sig)
    }
}

/// Name to function table (Go `template.FuncMap`).
#[derive(Clone, Debug, Default)]
pub struct FuncMap {
    funcs: BTreeMap<String, Func>,
}

impl FuncMap {
    pub fn new() -> Self {
        FuncMap::default()
    }

    /// Registers (or replaces) a function.
    pub fn insert(&mut self, name: impl Into<String>, func: Func) {
        self.funcs.insert(name.into(), func);
    }

    /// Registers a plain `fn` that does not need the [`CallCtx`].
    pub fn add(&mut self, name: &str, sig: Sig, f: fn(&[Value]) -> Result<Value, FuncError>) {
        self.insert(name, Func { sig, imp: Rc::new(move |_ctx: &mut CallCtx<'_>, a: &[Value]| f(a)) });
    }

    /// Registers a plain `fn` that needs the [`CallCtx`].
    pub fn add_ctx(
        &mut self,
        name: &str,
        sig: Sig,
        f: fn(&mut CallCtx<'_>, &[Value]) -> Result<Value, FuncError>,
    ) {
        self.insert(name, Func { sig, imp: Rc::new(f) });
    }

    pub fn remove(&mut self, name: &str) -> Option<Func> {
        self.funcs.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<&Func> {
        self.funcs.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.funcs.contains_key(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.funcs.keys().map(|s| s.as_str())
    }

    pub fn extend(&mut self, other: FuncMap) {
        self.funcs.extend(other.funcs);
    }
}

/// Convenience for implementing functions: a boxed error.
pub type FuncResult = Result<Value, FuncError>;
