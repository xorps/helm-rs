//! Value encoders: goyaml v2's `Marshal` of decoded `interface{}` values (the
//! second half of `sigs.k8s.io/yaml.Marshal`) and goyaml v3's encoder for
//! Helm's `toYamlPretty`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::decode::GoVal;
use super::emitter::Emitter;
use super::parser::{Event, EventType};
use super::resolve::{self, STR_TAG};
use super::scanner::ScalarStyle;
use super::strconv::{format_float, go_lossy_utf8};
use super::unicode_tables::{is_digit, is_letter};
use crate::value::Value;

/// Maximum nesting depth when encoding (guards against cycles and stack
/// overflow).
const MAX_ENCODE_DEPTH: usize = super::MAX_DEPTH;

// ---------------------------------------------------------------------------
// Key ordering (`sorter.go`), for string keys.

/// `keyList.Less` for two string keys (v2 or v3 flavor).
pub(crate) fn key_less(a: &str, b: &str, v3: bool) -> bool {
    if a.is_ascii() && b.is_ascii() {
        // No allocation for the common case: bytes are runes.
        let ar: &[u8] = a.as_bytes();
        let br: &[u8] = b.as_bytes();
        key_less_runes(ar.len(), br.len(), |i| ar[i] as char, |i| br[i] as char, v3)
    } else {
        let ar: Vec<char> = a.chars().collect();
        let br: Vec<char> = b.chars().collect();
        key_less_runes(ar.len(), br.len(), |i| ar[i], |i| br[i], v3)
    }
}

fn key_less_runes(
    alen: usize,
    blen: usize,
    ar: impl Fn(usize) -> char,
    br: impl Fn(usize) -> char,
    v3: bool,
) -> bool {
    let mut digits = false;
    let mut i = 0;
    while i < alen && i < blen {
        let (ac, bc) = (ar(i), br(i));
        if ac == bc {
            if v3 {
                digits = is_digit(ac);
            }
            i += 1;
            continue;
        }
        let al = is_letter(ac);
        let bl = is_letter(bc);
        if al && bl {
            return ac < bc;
        }
        if al || bl {
            if v3 && digits {
                return al;
            }
            return bl;
        }
        let mut an: i64 = 0;
        let mut bn: i64 = 0;
        if ac == '0' || bc == '0' {
            let mut j = i;
            while j > 0 && is_digit(ar(j - 1)) {
                if ar(j - 1) != '0' {
                    an = 1;
                    bn = 1;
                    break;
                }
                j -= 1;
            }
        }
        let mut ai = i;
        while ai < alen && is_digit(ar(ai)) {
            an = an
                .wrapping_mul(10)
                .wrapping_add((ar(ai) as i64).wrapping_sub('0' as i64));
            ai += 1;
        }
        let mut bi = i;
        while bi < blen && is_digit(br(bi)) {
            bn = bn
                .wrapping_mul(10)
                .wrapping_add((br(bi) as i64).wrapping_sub('0' as i64));
            bi += 1;
        }
        if an != bn {
            return an < bn;
        }
        if ai != bi {
            return ai < bi;
        }
        return ac < bc;
    }
    alen < blen
}

/// Sorts with a possibly inconsistent `less` without panicking (stable
/// merge sort on indices).
pub(crate) fn sort_by_less<T>(v: &mut Vec<T>, less: &dyn Fn(&T, &T) -> bool) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut idx: Vec<usize> = (0..n).collect();
    let mut buf: Vec<usize> = idx.clone();
    merge_sort_idx(&mut idx, &mut buf, &|a, b| less(&v[a], &v[b]));
    let mut slots: Vec<Option<T>> = v.drain(..).map(Some).collect();
    for i in idx {
        if let Some(x) = slots.get_mut(i).and_then(Option::take) {
            v.push(x);
        }
    }
}

fn merge_sort_idx(a: &mut [usize], buf: &mut [usize], less: &dyn Fn(usize, usize) -> bool) {
    let n = a.len();
    if n <= 12 {
        for i in 1..n {
            let mut j = i;
            while j > 0 && less(a[j], a[j - 1]) {
                a.swap(j, j - 1);
                j -= 1;
            }
        }
        return;
    }
    let mid = n / 2;
    {
        let (l, r) = a.split_at_mut(mid);
        let (bl, br) = buf.split_at_mut(mid);
        merge_sort_idx(l, bl, less);
        merge_sort_idx(r, br, less);
    }
    let (mut i, mut j, mut k) = (0, mid, 0);
    while i < mid && j < n {
        if less(a[j], a[i]) {
            buf[k] = a[j];
            j += 1;
        } else {
            buf[k] = a[i];
            i += 1;
        }
        k += 1;
    }
    while i < mid {
        buf[k] = a[i];
        i += 1;
        k += 1;
    }
    while j < n {
        buf[k] = a[j];
        j += 1;
        k += 1;
    }
    a.copy_from_slice(&buf[..n]);
}

// ---------------------------------------------------------------------------
// Event helpers.

struct Enc {
    em: Emitter,
    v3: bool,
    depth: usize,
}

type EncResult = Result<(), String>;

impl Enc {
    fn new(v3: bool) -> Enc {
        let mut em = Emitter::new(v3);
        if v3 {
            em.best_indent = 2;
        }
        Enc { em, v3, depth: 0 }
    }

    fn emit(&mut self, ev: Event) -> EncResult {
        if self.em.emit(ev) {
            Ok(())
        } else {
            let p = if self.em.problem.is_empty() {
                String::from("unknown problem generating YAML content")
            } else {
                self.em.problem.clone()
            };
            Err(format!("yaml: {p}"))
        }
    }

    fn start(&mut self) -> EncResult {
        self.emit(Event {
            typ: EventType::StreamStart,
            ..Default::default()
        })?;
        self.emit(Event {
            typ: EventType::DocumentStart,
            implicit: true,
            ..Default::default()
        })
    }

    fn end_doc(&mut self) -> EncResult {
        self.emit(Event {
            typ: EventType::DocumentEnd,
            implicit: true,
            ..Default::default()
        })
    }

    fn finish(&mut self) -> EncResult {
        self.em.open_ended = false;
        self.emit(Event {
            typ: EventType::StreamEnd,
            ..Default::default()
        })
    }

    fn scalar(&mut self, value: &[u8], style: ScalarStyle) -> EncResult {
        self.emit(Event {
            typ: EventType::Scalar,
            value: value.to_vec(),
            implicit: true,
            quoted_implicit: true,
            style,
            ..Default::default()
        })
    }

    fn nilv(&mut self) -> EncResult {
        self.scalar(b"null", ScalarStyle::Plain)
    }

    fn floatv(&mut self, f: f64) -> EncResult {
        let s = format_float(f, b'g', false);
        let s = match s.as_str() {
            "+Inf" => ".inf".into(),
            "-Inf" => "-.inf".into(),
            "NaN" => ".nan".into(),
            _ => s,
        };
        self.scalar(s.as_bytes(), ScalarStyle::Plain)
    }

    fn stringv(&mut self, s: &str) -> EncResult {
        let (rtag, _) =
            resolve::resolve("", s, !self.v3).unwrap_or((STR_TAG, resolve::Resolved::Str));
        let mut can_use_plain = rtag == STR_TAG && !resolve::is_base60_float(s);
        if self.v3 && resolve::is_old_bool(s) {
            can_use_plain = false;
        }
        let style = if s.contains('\n') {
            ScalarStyle::Literal
        } else if can_use_plain {
            ScalarStyle::Plain
        } else {
            ScalarStyle::DoubleQuoted
        };
        self.scalar(s.as_bytes(), style)
    }

    fn mapping_start(&mut self) -> EncResult {
        self.emit(Event {
            typ: EventType::MappingStart,
            implicit: true,
            flow: false,
            ..Default::default()
        })
    }
    fn mapping_end(&mut self) -> EncResult {
        self.emit(Event {
            typ: EventType::MappingEnd,
            ..Default::default()
        })
    }
    fn sequence_start(&mut self) -> EncResult {
        self.emit(Event {
            typ: EventType::SequenceStart,
            implicit: true,
            flow: false,
            ..Default::default()
        })
    }
    fn sequence_end(&mut self) -> EncResult {
        self.emit(Event {
            typ: EventType::SequenceEnd,
            ..Default::default()
        })
    }

    #[inline(never)]
    fn enter(&mut self) -> EncResult {
        self.depth += 1;
        if self.depth > MAX_ENCODE_DEPTH {
            return Err(format!("yaml: exceeded max depth of {MAX_ENCODE_DEPTH}"));
        }
        Ok(())
    }

    // The recursive functions below keep their frames small; scalar work
    // happens in non-inlined helpers.

    #[inline(never)]
    fn go_scalar(&mut self, v: &GoVal) -> EncResult {
        match v {
            GoVal::Nil => self.nilv(),
            GoVal::Bool(b) => self.scalar(if *b { b"true" } else { b"false" }, ScalarStyle::Plain),
            GoVal::Int(i) => self.scalar(format!("{i}").as_bytes(), ScalarStyle::Plain),
            GoVal::Uint(u) => self.scalar(format!("{u}").as_bytes(), ScalarStyle::Plain),
            GoVal::Float(f) => self.floatv(*f),
            GoVal::Str(s) => self.stringv(&go_lossy_utf8(s)),
            GoVal::Seq(_) | GoVal::Map(_) => Ok(()),
        }
    }

    #[inline(never)]
    fn sorted_go_entries<'v>(
        &self,
        entries: &'v [(GoVal, GoVal, (super::scanner::Mark, super::scanner::Mark))],
    ) -> Vec<&'v (GoVal, GoVal, (super::scanner::Mark, super::scanner::Mark))> {
        let mut sorted: Vec<&(GoVal, GoVal, (super::scanner::Mark, super::scanner::Mark))> =
            entries.iter().collect();
        let v3 = self.v3;
        sort_by_less(&mut sorted, &|a, b| go_key_less(&a.0, &b.0, v3));
        sorted
    }

    // v2: values decoded from JSON.
    fn marshal_go(&mut self, v: &GoVal) -> EncResult {
        match v {
            GoVal::Seq(items) => {
                self.enter()?;
                self.sequence_start()?;
                for (item, _) in items {
                    self.marshal_go(item)?;
                }
                self.sequence_end()?;
                self.depth -= 1;
                Ok(())
            }
            GoVal::Map(entries) => {
                self.enter()?;
                let sorted = self.sorted_go_entries(entries);
                self.mapping_start()?;
                for (k, v, _) in sorted {
                    self.marshal_go(k)?;
                    self.marshal_go(v)?;
                }
                self.mapping_end()?;
                self.depth -= 1;
                Ok(())
            }
            _ => self.go_scalar(v),
        }
    }

    #[inline(never)]
    fn value_scalar(&mut self, v: &Value) -> EncResult {
        match v {
            Value::Nil => self.nilv(),
            Value::Bool(b) => self.scalar(if *b { b"true" } else { b"false" }, ScalarStyle::Plain),
            Value::Int(i) | Value::Int64(i) => {
                self.scalar(format!("{i}").as_bytes(), ScalarStyle::Plain)
            }
            Value::Uint64(u) => self.scalar(format!("{u}").as_bytes(), ScalarStyle::Plain),
            Value::Float(f) => self.floatv(*f),
            Value::String(s) => self.stringv(s),
            Value::Bytes(b) => {
                self.enter()?;
                self.sequence_start()?;
                for byte in b.iter() {
                    self.scalar(format!("{byte}").as_bytes(), ScalarStyle::Plain)?;
                }
                self.sequence_end()?;
                self.depth -= 1;
                Ok(())
            }
            Value::Object(o) => {
                // goyaml.v3's `timev`: RFC 3339, always plain.
                let data = o.to_data();
                self.scalar(data.as_str().unwrap_or("").as_bytes(), ScalarStyle::Plain)
            }
            Value::List(_) | Value::Map(_) => Ok(()),
        }
    }

    #[inline(never)]
    fn sorted_value_entries(&self, m: &crate::value::MapRef) -> Vec<(String, Value)> {
        let mut entries: Vec<(String, Value)> = m
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let v3 = self.v3;
        sort_by_less(&mut entries, &|a, b| key_less(&a.0, &b.0, v3));
        entries
    }

    // v3: template values.
    fn marshal_value(&mut self, v: &Value) -> EncResult {
        match v {
            Value::List(items) => {
                self.enter()?;
                self.sequence_start()?;
                for item in items.iter() {
                    self.marshal_value(item)?;
                }
                self.sequence_end()?;
                self.depth -= 1;
                Ok(())
            }
            Value::Map(m) => {
                self.enter()?;
                let sorted = self.sorted_value_entries(m);
                self.mapping_start()?;
                for (k, v) in &sorted {
                    self.stringv(k)?;
                    self.marshal_value(v)?;
                }
                self.mapping_end()?;
                self.depth -= 1;
                Ok(())
            }
            Value::Object(o) if o.type_name() != "time.Time" => {
                self.enter()?;
                let r = if let Some(gs) = o.go_struct() {
                    // goyaml.v3 structs: exported fields in declaration
                    // order, keyed by the lowercased field name.
                    (|| {
                        self.mapping_start()?;
                        for f in gs
                            .fields
                            .iter()
                            .filter(|f| f.name.starts_with(|c: char| c.is_ascii_uppercase()))
                        {
                            self.stringv(&f.name.to_ascii_lowercase())?;
                            self.marshal_value(&f.value)?;
                        }
                        self.mapping_end()
                    })()
                } else {
                    match o.to_data() {
                        // A nil map encodes as `{}` (only nil pointers are null).
                        Value::Nil if o.kind() == crate::value::Kind::Map => {
                            self.marshal_value(&Value::empty_map())
                        }
                        data => self.marshal_value(&data),
                    }
                };
                self.depth -= 1;
                r
            }
            _ => self.value_scalar(v),
        }
    }
}

/// `keyList.Less` for decoded values (keys are strings in practice).
fn go_key_less(a: &GoVal, b: &GoVal, v3: bool) -> bool {
    fn num(v: &GoVal) -> Option<f64> {
        match v {
            GoVal::Int(i) => Some(*i as f64),
            GoVal::Uint(u) => Some(*u as f64),
            GoVal::Float(f) => Some(*f),
            GoVal::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
    fn kind(v: &GoVal) -> u8 {
        // reflect.Kind ordering.
        match v {
            GoVal::Nil => 20,
            GoVal::Bool(_) => 1,
            GoVal::Int(_) => 2,
            GoVal::Uint(_) => 11,
            GoVal::Float(_) => 14,
            GoVal::Seq(_) => 23,
            GoVal::Map(_) => 21,
            GoVal::Str(_) => 24,
        }
    }
    match (num(a), num(b)) {
        (Some(af), Some(bf)) => {
            if af != bf {
                return af < bf;
            }
            if kind(a) != kind(b) {
                return kind(a) < kind(b);
            }
            match (a, b) {
                (GoVal::Int(x), GoVal::Int(y)) => x < y,
                (GoVal::Uint(x), GoVal::Uint(y)) => x < y,
                (GoVal::Float(x), GoVal::Float(y)) => x < y,
                (GoVal::Bool(x), GoVal::Bool(y)) => !x & y,
                _ => false,
            }
        }
        _ => match (a, b) {
            (GoVal::Str(x), GoVal::Str(y)) => key_less(&go_lossy_utf8(x), &go_lossy_utf8(y), v3),
            _ => kind(a) < kind(b),
        },
    }
}

/// goyaml v2 `Marshal` of a decoded value.
pub(crate) fn marshal_v2(v: &GoVal) -> Result<String, String> {
    let mut e = Enc::new(false);
    e.start()?;
    e.marshal_go(v)?;
    e.end_doc()?;
    e.finish()?;
    Ok(go_lossy_utf8(&e.em.out))
}

/// goyaml v3 `Encoder` with `SetIndent(2)`, one `Encode(v)` call, not closed.
pub(crate) fn encode_v3(v: &Value) -> Result<String, String> {
    let mut e = Enc::new(true);
    e.start()?;
    e.marshal_value(v)?;
    e.end_doc()?;
    Ok(go_lossy_utf8(&e.em.out))
}
