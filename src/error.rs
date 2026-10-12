//! Errors with precise source locations.
//!
//! Every error that originates in a template or a YAML document carries the
//! file name, 1-based line and column, and byte span of the offending
//! construct, plus the chain of `include`/`template`/`tpl` calls that led
//! there. [`Error`]'s `Display` renders a compiler-style diagnostic with the
//! source line and a caret underline; [`Error::helm_message`] gives the
//! message Helm itself would print.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

/// What went wrong, broadly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Template syntax error.
    Parse,
    /// Error while executing a template (bad field access, function error...).
    Exec,
    /// `required` or `fail` was called.
    Fail,
    /// Malformed YAML/JSON/TOML input (values files, `Chart.yaml`, ...).
    Syntax,
    /// Invalid chart structure, metadata, or dependencies.
    Chart,
    /// Values failed validation (e.g. `values.schema.json`).
    Values,
}

/// A position in a source file.
#[derive(Clone, PartialEq, Eq)]
pub struct Location {
    /// Template or file name, e.g. `mychart/templates/deployment.yaml`.
    pub name: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column, counted in characters.
    pub column: u32,
    /// Byte range of the offending construct within `source`.
    pub span: Range<usize>,
    /// The full source text, for rendering snippets.
    pub source: Option<Rc<str>>,
}

impl Location {
    /// Builds a location from a byte span in `source`, computing line/column.
    pub fn from_span(name: impl Into<String>, source: Rc<str>, span: Range<usize>) -> Location {
        let (line, column) = line_col(&source, span.start);
        Location { name: name.into(), line, column, span, source: Some(source) }
    }

    /// Builds a location without source text.
    pub fn new(name: impl Into<String>, line: u32, column: u32) -> Location {
        Location { name: name.into(), line, column, span: 0..0, source: None }
    }

    /// The source line containing the start of the span, without newline.
    pub fn source_line(&self) -> Option<&str> {
        let src = self.source.as_deref()?;
        let start = self.span.start.min(src.len());
        let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = src[start..].find('\n').map_or(src.len(), |i| start + i);
        Some(&src[line_start..line_end])
    }
}

impl fmt::Debug for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.name, self.line, self.column)
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.name, self.line, self.column)
    }
}

/// 1-based line and character column of byte offset `pos` in `src`.
pub fn line_col(src: &str, pos: usize) -> (u32, u32) {
    let pos = floor_char_boundary(src, pos.min(src.len()));
    let before = &src[..pos];
    let line = before.matches('\n').count() as u32 + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = src[line_start..pos].chars().count() as u32 + 1;
    (line, column)
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// One step of the template call stack: where an `include`, `template` or
/// `tpl` call was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The call site.
    pub location: Location,
    /// What was called, e.g. `include "common.labels"` or `tpl`.
    pub call: String,
}

/// A rendering error.
#[derive(Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    /// The error message, without location information.
    pub message: String,
    /// Where the error happened (innermost).
    pub location: Option<Location>,
    /// Call stack from the innermost caller outwards.
    pub frames: Vec<Frame>,
    /// The message Helm would produce for this error, when known.
    pub helm_message: Option<String>,
    /// An extra hint, e.g. which part of a field chain was nil.
    pub help: Option<String>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Error {
        Error { kind, message: message.into(), location: None, frames: Vec::new(), helm_message: None, help: None }
    }

    pub fn at(mut self, location: Location) -> Error {
        self.location = Some(location);
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Error {
        self.help = Some(help.into());
        self
    }

    pub fn with_helm_message(mut self, msg: impl Into<String>) -> Error {
        self.helm_message = Some(msg.into());
        self
    }

    /// Wraps the message with context, keeping location information.
    pub fn context(mut self, ctx: impl fmt::Display) -> Error {
        self.message = alloc::format!("{ctx}: {}", self.message);
        self
    }

    /// The message Helm would print for this error (best effort).
    pub fn helm_message(&self) -> String {
        match &self.helm_message {
            Some(m) => m.clone(),
            None => self.message.clone(),
        }
    }

    /// Line of the innermost location, if any.
    pub fn line(&self) -> Option<u32> {
        self.location.as_ref().map(|l| l.line)
    }

    /// Column of the innermost location, if any.
    pub fn column(&self) -> Option<u32> {
        self.location.as_ref().map(|l| l.column)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Renders a diagnostic like:
///
/// ```text
/// error: nil pointer evaluating interface {}.tag
///   --> mychart/templates/deployment.yaml:12:24
///    |
/// 12 |         image: {{ .Values.image.tag }}
///    |                   ^^^^^^^^^^^^^^^^^
///    = note: in "mychart.image" included from mychart/templates/deployment.yaml:30:8
/// ```
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(loc) = &self.location {
            write!(f, "\n  --> {loc}")?;
            write_snippet(f, loc)?;
        }
        if let Some(help) = &self.help {
            write!(f, "\n   = help: {help}")?;
        }
        for frame in &self.frames {
            write!(f, "\n   = note: from `{}` at {}", frame.call, frame.location)?;
        }
        Ok(())
    }
}

fn write_snippet(f: &mut fmt::Formatter<'_>, loc: &Location) -> fmt::Result {
    let Some(line) = loc.source_line() else { return Ok(()) };
    let num = loc.line.to_string();
    let pad = " ".repeat(num.len());
    let col = loc.column.saturating_sub(1) as usize;
    let span_text_len = loc
        .source
        .as_deref()
        .and_then(|s| s.get(loc.span.clone()))
        .map(|t| t.split('\n').next().unwrap_or("").chars().count())
        .unwrap_or(1);
    let avail = line.chars().count().saturating_sub(col);
    let width = span_text_len.clamp(1, avail.max(1));
    // Reproduce tabs so the caret lines up.
    let lead: String = line.chars().take(col).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
    write!(f, "\n {pad} |\n {num} | {line}\n {pad} | {lead}{}", "^".repeat(width))
}

impl core::error::Error for Error {}

impl core::error::Error for crate::func::FuncError {}
