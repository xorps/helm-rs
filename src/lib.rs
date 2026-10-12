//! `helm-rs`: render Helm charts without Helm.
//!
//! A `no_std` (+`alloc`) implementation of Helm's template engine: Go's
//! `text/template`, the Sprig function library, and Helm's own functions and
//! chart semantics (values coalescing, subcharts, `.Files`, `.Capabilities`).
//! Output is diff-tested against the real Helm (see `oracle/`).
//!
//! Errors carry exact file/line/column spans and the `include`/`tpl` call
//! stack; see [`Error`].
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod chart;
pub mod error;
pub mod func;
pub mod funcs;
pub mod gofmt;
pub mod json;
pub mod template;
pub mod toml;
pub mod value;
pub mod yaml;

pub use error::{Error, ErrorKind, Location};
pub use func::{CallCtx, DeterministicEnv, Env, Func, FuncError, FuncMap, Sig, Ty};
pub use value::{Kind, Map, MapRef, Object, Value};

#[doc(hidden)]
pub mod __private {
    pub use alloc::string::String;
}
