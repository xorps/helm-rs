//! Chart loading: Helm's `loader.LoadFiles`, the directory loader's
//! `.helmignore` handling, and values file parsing.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::{Error, ErrorKind, Location};
use crate::value::{MapRef, Value};
use crate::yaml::YamlError;

use super::decode::Dec;
use super::gostr::ext;
use super::ignore::Rules;
use super::metadata::{Lock, RawMetadata};
use super::values::{merge_maps, split_yaml_documents};
use super::{Chart, File};

/// YAML parser used by the loader (normally [`crate::yaml::from_str`]).
pub type YamlParser = fn(&str) -> Result<Value, YamlError>;

/// Helm's `archive.MaxDecompressedChartSize` (100 MiB).
pub const MAX_DECOMPRESSED_CHART_SIZE: usize = 100 * 1024 * 1024;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Maximum subchart nesting (guards recursion on hostile archives; Helm has
/// no limit).
const MAX_CHART_DEPTH: usize = 64;

/// A file as handed to `LoadFiles` (`archive.BufferedFile`).
#[derive(Clone, Debug)]
pub(crate) struct BufferedFile {
    pub name: String,
    pub data: Rc<[u8]>,
}

pub(crate) fn chart_err(msg: impl Into<String>) -> Error {
    Error::new(ErrorKind::Chart, msg)
}

/// Location of a 1-based line/column in `source`.
pub(crate) fn location_at(name: &str, source: &Rc<str>, line: u32, column: u32) -> Location {
    let mut off = 0usize;
    let mut cur = 1u32;
    for (i, b) in source.bytes().enumerate() {
        if cur >= line {
            break;
        }
        if b == b'\n' {
            cur += 1;
            off = i + 1;
        }
    }
    if cur < line {
        off = source.len();
    }
    let line_text = &source[off..];
    let line_text = &line_text[..line_text.find('\n').unwrap_or(line_text.len())];
    let mut col_off = 0usize;
    for (n, (i, _)) in line_text.char_indices().enumerate() {
        if n + 1 >= column as usize {
            col_off = i;
            break;
        }
        col_off = line_text.len();
    }
    if column <= 1 {
        col_off = 0;
    }
    let start = off + col_off.min(line_text.len());
    let mut loc = Location::from_span(name, source.clone(), start..start);
    loc.line = line.max(1);
    loc.column = column.max(1);
    loc
}

/// Finds the line of a top-level YAML key (best effort, for locating
/// decoding/validation errors). Falls back to line 1.
pub(crate) fn key_location(name: &str, source: &Rc<str>, key: Option<&str>) -> Location {
    if let Some(key) = key {
        let mut off = 0usize;
        for line in source.split('\n') {
            let l = line.strip_prefix('"').or_else(|| line.strip_prefix('\'')).unwrap_or(line);
            if let Some(rest) = l.strip_prefix(key) {
                let rest = rest.trim_start_matches(['"', '\'']).trim_start();
                if rest.starts_with(':') && !line.starts_with([' ', '\t']) {
                    return Location::from_span(name, source.clone(), off..off + line.len());
                }
            }
            off += line.len() + 1;
        }
    }
    Location::from_span(name, source.clone(), 0..0)
}

fn source_of(data: &[u8]) -> Rc<str> {
    Rc::from(String::from_utf8_lossy(data).as_ref())
}

fn yaml_text(data: &[u8]) -> String {
    String::from_utf8_lossy(data).into_owned()
}

/// Converts a YAML error into an [`Error`] at `line_offset` lines into `name`.
pub(crate) fn yaml_error(prefix: &str, e: &YamlError, name: &str, source: &Rc<str>, line_offset: u32) -> Error {
    let mut err = Error::new(ErrorKind::Syntax, alloc::format!("{prefix}{}", e.message));
    let (line, column) = yaml_err_pos(e);
    err = err.at(location_at(name, source, line + line_offset, column));
    err
}

/// Position of a YAML error: the line Go's message reports (`yaml: line N:
/// ...`, relative to the document), with the parser's column when it agrees.
pub(crate) fn yaml_err_pos(e: &YamlError) -> (u32, u32) {
    let msg_line = e
        .message
        .split("yaml: line ")
        .nth(1)
        .and_then(|r| r.split(':').next())
        .and_then(|n| n.parse::<u32>().ok());
    match (msg_line, e.line) {
        (Some(m), Some(l)) if m == l => (m, e.column.unwrap_or(1)),
        (Some(m), _) => (m, 1),
        (None, Some(l)) => (l, e.column.unwrap_or(1)),
        (None, None) => (1, 1),
    }
}

/// Parses `data` and decodes it into a Go struct; returns the Helm message
/// suffix (`error converting YAML...` / `error unmarshaling JSON...`).
fn decode_yaml(
    yaml: YamlParser,
    data: &[u8],
    name: &str,
    what: &str,
    f: impl FnOnce(&mut Dec, &Value),
) -> Result<(), Error> {
    let src = source_of(data);
    let v = yaml(&yaml_text(data)).map_err(|e| yaml_error(&alloc::format!("cannot load {what}: "), &e, name, &src, 0))?;
    let mut d = Dec::new();
    f(&mut d, &v);
    if let Some(msg) = d.err {
        let key = first_key_of_error(&msg);
        return Err(chart_err(alloc::format!(
            "cannot load {what}: error unmarshaling JSON: while decoding JSON: {msg}"
        ))
        .at(key_location(name, &src, key.as_deref())));
    }
    Ok(())
}

/// The top-level key in a json type error path (`... field .a.b of type`).
fn first_key_of_error(msg: &str) -> Option<String> {
    let i = msg.find(" into ")?;
    let rest = &msg[i + 6..];
    let rest = rest.strip_prefix("Go struct field ").unwrap_or(rest);
    let rest = rest.strip_prefix('.')?;
    let end = rest.find(['.', ' ']).unwrap_or(rest.len());
    Some(rest[..end].replace("~1", "/").replace("~0", "~"))
}

/// `loader.LoadValues`: multi-document values file, documents merged.
pub(crate) fn load_values(yaml: YamlParser, data: &[u8], name: &str) -> Result<MapRef, Error> {
    let mut text = yaml_text(data);
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let src: Rc<str> = Rc::from(text.as_str());
    let (docs, sep_err) = split_yaml_documents(&text);
    let mut values = MapRef::default();
    for doc in docs {
        let v = yaml(&doc.text).map_err(|e| {
            yaml_error("cannot unmarshal yaml document: ", &e, name, &src, doc.first_line - 1)
        })?;
        let cur = match v {
            Value::Nil => MapRef::default(),
            Value::Map(m) => m,
            other => {
                let mut d = Dec::new();
                d.type_err(&other, "map[string]interface {}");
                return Err(Error::new(
                    ErrorKind::Syntax,
                    alloc::format!(
                        "cannot unmarshal yaml document: error unmarshaling JSON: while decoding JSON: {}",
                        d.err.unwrap_or_default()
                    ),
                )
                .at(location_at(name, &src, doc.first_line, 1)));
            }
        };
        values = merge_maps(&values, &cur);
    }
    if let Some((m, line)) = sep_err {
        return Err(Error::new(ErrorKind::Syntax, alloc::format!("error reading yaml document: {m}"))
            .at(location_at(name, &src, line, 1)));
    }
    Ok(values)
}

/// `loader.LoadFiles`. `loc_prefix` is prepended to file names in error
/// locations (e.g. `charts/sub/`).
pub(crate) fn load_files(files: Vec<BufferedFile>, loc_prefix: &str, yaml: YamlParser) -> Result<Chart, Error> {
    if loc_prefix.matches("charts/").count() > MAX_CHART_DEPTH {
        return Err(chart_err("chart dependencies are nested too deeply"));
    }
    let mut raw: Option<RawMetadata> = None;
    let mut chart_yaml_src: Option<Rc<str>> = None;
    let loc = |n: &str| alloc::format!("{loc_prefix}{n}");
    for f in &files {
        if f.name == "Chart.yaml" {
            let md = raw.get_or_insert_with(RawMetadata::default);
            decode_yaml(yaml, &f.data, &loc(&f.name), "Chart.yaml", |d, v| md.decode(d, v))?;
            if md.md.api_version.is_empty() {
                md.md.api_version = "v1".into();
            }
            chart_yaml_src = Some(source_of(&f.data));
        }
    }
    let mut c = Chart::empty();
    let mut lock: Option<Lock> = None;
    let mut subcharts: BTreeMap<String, Vec<BufferedFile>> = BTreeMap::new();
    for f in files {
        let file = || File { name: f.name.clone(), data: f.data.clone() };
        match f.name.as_str() {
            "Chart.yaml" => {}
            "Chart.lock" | "requirements.lock" => {
                let what = f.name.clone();
                lock = Some(Lock::default());
                decode_yaml(yaml, &f.data, &loc(&f.name), &what, |d, v| Lock::decode(d, v, &mut lock))?;
                if f.name == "requirements.lock" {
                    let md = raw.get_or_insert_with(RawMetadata::default);
                    if md.md.api_version == "v1" {
                        c.files.push(file());
                    }
                }
            }
            "values.yaml" => {
                c.values = load_values(yaml, &f.data, &loc(&f.name)).map_err(|e| e.context("cannot load values.yaml"))?;
            }
            "values.schema.json" => c.schema = Some(f.data.clone()),
            "requirements.yaml" => {
                let md = raw.get_or_insert_with(RawMetadata::default);
                decode_yaml(yaml, &f.data, &loc(&f.name), "requirements.yaml", |d, v| md.decode(d, v))?;
                if md.md.api_version == "v1" {
                    c.files.push(file());
                }
            }
            n if n.starts_with("templates/") => c.templates.push(file()),
            n if n.starts_with("charts/") => {
                if ext(n) == ".prov" {
                    c.files.push(file());
                    continue;
                }
                let fname = &n["charts/".len()..];
                let cname = fname.split('/').next().unwrap_or("");
                subcharts
                    .entry(cname.to_string())
                    .or_default()
                    .push(BufferedFile { name: fname.to_string(), data: f.data.clone() });
            }
            _ => c.files.push(file()),
        }
    }
    c.lock = lock;
    let Some(raw) = raw else {
        return Err(chart_err("Chart.yaml file is missing"));
    };
    c.metadata = raw.validate().map_err(|msg| {
        let key = validation_key(&msg);
        let mut e = chart_err(msg);
        if let Some(src) = &chart_yaml_src {
            e = e.at(key_location(&loc("Chart.yaml"), src, key));
        }
        e
    })?;
    let parent = c.metadata.name.clone();
    for (n, files) in subcharts {
        if n.starts_with(['_', '.']) {
            continue;
        }
        let sc = if ext(&n) == ".tgz" {
            let file = &files[0];
            if file.name != n {
                return Err(chart_err(alloc::format!(
                    "error unpacking subchart tar in {parent}: expected {n}, got {}",
                    file.name
                )));
            }
            load_archive_bytes(&file.data, &alloc::format!("{loc_prefix}charts/{n}/"), yaml)
        } else {
            let buff: Vec<BufferedFile> = files
                .into_iter()
                .filter_map(|f| {
                    let (_, rest) = f.name.split_once('/')?;
                    Some(BufferedFile { name: rest.to_string(), data: f.data })
                })
                .collect();
            load_files(buff, &alloc::format!("{loc_prefix}charts/{n}/"), yaml)
        };
        let sc = sc.map_err(|e| e.context(alloc::format!("error unpacking subchart {n} in {parent}")))?;
        c.dependencies.push(sc);
    }
    Ok(c)
}

fn validation_key(msg: &str) -> Option<&'static str> {
    let m = msg.strip_prefix("validation: ")?;
    Some(if m.starts_with("chart.metadata.apiVersion") {
        "apiVersion"
    } else if m.starts_with("chart.metadata.name") {
        "name"
    } else if m.starts_with("chart.metadata.version") {
        "version"
    } else if m.starts_with("chart.metadata.type") {
        "type"
    } else if m.starts_with("maintainers") {
        "maintainers"
    } else {
        "dependencies"
    })
}

#[cfg(feature = "archive")]
fn load_archive_bytes(data: &[u8], loc_prefix: &str, yaml: YamlParser) -> Result<Chart, Error> {
    let files = super::archive::load_archive_files(data)?;
    load_files(files, loc_prefix, yaml)
}

#[cfg(not(feature = "archive"))]
fn load_archive_bytes(_data: &[u8], _loc_prefix: &str, _yaml: YamlParser) -> Result<Chart, Error> {
    Err(chart_err("loading .tgz subcharts requires the `archive` feature"))
}

/// Compares paths component-wise, which is the order `filepath.Walk` visits
/// files in (sorted directory entries, depth first).
fn walk_order(a: &str, b: &str) -> core::cmp::Ordering {
    a.split('/').cmp(b.split('/'))
}

/// The directory loader (`loader.LoadDir`) applied to an in-memory listing:
/// `.helmignore` rules (plus defaults), walk order, size budget, BOM removal.
pub(crate) fn load_dir_files(files: Vec<(String, Vec<u8>)>, yaml: YamlParser) -> Result<Chart, Error> {
    let mut files: Vec<(String, Vec<u8>)> = files
        .into_iter()
        .map(|(n, d)| {
            let mut n = n.replace('\\', "/");
            while let Some(s) = n.strip_prefix("./") {
                n = s.to_string();
            }
            (String::from(n.trim_start_matches('/')), d)
        })
        .filter(|(n, _)| !n.is_empty())
        .collect();
    let mut rules = match files.iter().find(|(n, _)| n == ".helmignore") {
        Some((_, d)) => Rules::parse(d).map_err(chart_err)?,
        None => Rules::empty(),
    };
    rules.add_defaults();
    files.sort_by(|a, b| walk_order(&a.0, &b.0));
    let mut remaining = MAX_DECOMPRESSED_CHART_SIZE;
    let mut out = Vec::with_capacity(files.len());
    'files: for (name, data) in files {
        let mut idx = 0;
        while let Some(i) = name[idx..].find('/') {
            let dir = &name[..idx + i];
            if rules.ignore(dir, true) {
                continue 'files;
            }
            idx += i + 1;
        }
        if rules.ignore(&name, false) {
            continue;
        }
        if data.len() > remaining {
            return Err(chart_err(alloc::format!(
                "error reading {name}: chart exceeds maximum decompressed size of {MAX_DECOMPRESSED_CHART_SIZE} bytes"
            )));
        }
        remaining -= data.len();
        let data = data.strip_prefix(UTF8_BOM).unwrap_or(&data);
        out.push(BufferedFile { name, data: Rc::from(data) });
    }
    load_files(out, "", yaml)
}
