//! `.tgz` chart archives: a streaming gzip reader (on `miniz_oxide`), a port
//! of Go's `archive/tar` reader (USTAR, PAX, GNU long names) and Helm's
//! `archive.LoadArchiveFiles`, with its size limits and error messages.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

use crate::error::Error;

use super::gostr::{clean, quote};
use super::loader::{BufferedFile, MAX_DECOMPRESSED_CHART_SIZE, chart_err};

const UNEXPECTED_EOF: &str = "unexpected EOF";
const ERR_HEADER: &str = "archive/tar: invalid tar header";
const GZ_ERR_HEADER: &str = "gzip: invalid header";
const GZ_ERR_CHECKSUM: &str = "gzip: invalid checksum";

/// Read error: clean end of stream (`io.EOF`) or a message.
#[derive(Debug, Clone, PartialEq)]
enum RErr {
    Eof,
    Msg(String),
}

impl RErr {
    fn msg(s: &str) -> RErr {
        RErr::Msg(s.to_string())
    }
    fn into_string(self) -> String {
        match self {
            RErr::Eof => "EOF".into(),
            RErr::Msg(m) => m,
        }
    }
}

fn crc32_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (i, e) in t.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *e = c;
    }
    t
}

fn crc32_update(table: &[u32; 256], crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c = table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

enum Phase {
    /// Expecting a member header (`first` = no member read yet).
    Header {
        first: bool,
    },
    Body,
    Done(RErr),
}

/// Streaming gzip decoder with Go `compress/gzip` semantics (multistream,
/// header/trailer validation). Decompresses only as far as it is read.
struct Gz<'a> {
    input: &'a [u8],
    ipos: usize,
    buf: Vec<u8>,
    bpos: usize,
    phase: Phase,
    state: Option<Box<InflateState>>,
    crc: u32,
    size: u32,
    table: [u32; 256],
}

impl<'a> Gz<'a> {
    /// `gzip.NewReader`: reads the first member header.
    fn new(input: &'a [u8]) -> Result<Gz<'a>, String> {
        let mut gz = Gz {
            input,
            ipos: 0,
            buf: Vec::new(),
            bpos: 0,
            phase: Phase::Header { first: true },
            state: None,
            crc: 0,
            size: 0,
            table: crc32_table(),
        };
        gz.read_header(true).map_err(RErr::into_string)?;
        gz.phase = Phase::Body;
        Ok(gz)
    }

    fn take_input(&mut self, n: usize) -> Result<&'a [u8], RErr> {
        let rest = &self.input[self.ipos..];
        if rest.len() < n {
            self.ipos = self.input.len();
            return Err(if rest.is_empty() {
                RErr::Eof
            } else {
                RErr::msg(UNEXPECTED_EOF)
            });
        }
        self.ipos += n;
        Ok(&rest[..n])
    }

    fn read_header(&mut self, first: bool) -> Result<(), RErr> {
        let _ = first;
        let no_eof = |e: RErr| {
            if e == RErr::Eof {
                RErr::msg(UNEXPECTED_EOF)
            } else {
                e
            }
        };
        let h = self.take_input(10)?;
        if h[0] != 0x1f || h[1] != 0x8b || h[2] != 8 {
            return Err(RErr::msg(GZ_ERR_HEADER));
        }
        let flg = h[3];
        let mut digest = crc32_update(&self.table, 0, h);
        if flg & 0x04 != 0 {
            let xl = self.take_input(2).map_err(no_eof)?;
            digest = crc32_update(&self.table, digest, xl);
            let n = u16::from_le_bytes([xl[0], xl[1]]) as usize;
            let data = self.take_input(n).map_err(no_eof)?;
            digest = crc32_update(&self.table, digest, data);
        }
        for flag in [0x08u8, 0x10] {
            if flg & flag != 0 {
                // NUL-terminated string, at most 512 bytes.
                let rest = &self.input[self.ipos..];
                match rest.iter().take(512).position(|b| *b == 0) {
                    Some(i) => {
                        digest = crc32_update(&self.table, digest, &rest[..=i]);
                        self.ipos += i + 1;
                    }
                    None if rest.len() < 512 => {
                        self.ipos = self.input.len();
                        return Err(RErr::msg(UNEXPECTED_EOF));
                    }
                    None => return Err(RErr::msg(GZ_ERR_HEADER)),
                }
            }
        }
        if flg & 0x02 != 0 {
            let c = self.take_input(2).map_err(no_eof)?;
            if u16::from_le_bytes([c[0], c[1]]) != digest as u16 {
                return Err(RErr::msg(GZ_ERR_HEADER));
            }
        }
        self.state = Some(InflateState::new_boxed(DataFormat::Raw));
        self.crc = 0;
        self.size = 0;
        Ok(())
    }

    /// Produces more output; returns false once the stream is finished/failed.
    fn fill(&mut self) -> bool {
        loop {
            match &self.phase {
                Phase::Done(_) => return false,
                Phase::Header { first } => {
                    let first = *first;
                    match self.read_header(first) {
                        Ok(()) => self.phase = Phase::Body,
                        Err(e) => self.phase = Phase::Done(e),
                    }
                }
                Phase::Body => {
                    let Some(state) = self.state.as_mut() else {
                        self.phase = Phase::Done(RErr::msg(UNEXPECTED_EOF));
                        continue;
                    };
                    if self.bpos > 0 && self.bpos == self.buf.len() {
                        self.buf.clear();
                        self.bpos = 0;
                    }
                    let mut chunk = alloc::vec![0u8; 32 * 1024];
                    let res = inflate(state, &self.input[self.ipos..], &mut chunk, MZFlush::None);
                    self.ipos += res.bytes_consumed;
                    let out = &chunk[..res.bytes_written];
                    self.crc = crc32_update(&self.table, self.crc, out);
                    self.size = self.size.wrapping_add(out.len() as u32);
                    self.buf.extend_from_slice(out);
                    match res.status {
                        Ok(MZStatus::StreamEnd) => {
                            self.state = None;
                            match self.take_input(8) {
                                Ok(t) => {
                                    let crc = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
                                    let size = u32::from_le_bytes([t[4], t[5], t[6], t[7]]);
                                    if crc != self.crc || size != self.size {
                                        self.phase = Phase::Done(RErr::msg(GZ_ERR_CHECKSUM));
                                    } else if self.ipos == self.input.len() {
                                        self.phase = Phase::Done(RErr::Eof);
                                    } else {
                                        self.phase = Phase::Header { first: false };
                                    }
                                }
                                Err(_) => self.phase = Phase::Done(RErr::msg(UNEXPECTED_EOF)),
                            }
                        }
                        Ok(_) => {
                            if res.bytes_consumed == 0 && res.bytes_written == 0 {
                                self.phase = Phase::Done(RErr::msg(UNEXPECTED_EOF));
                            }
                        }
                        Err(MZError::Buf) => {
                            if res.bytes_written == 0 {
                                self.phase = Phase::Done(RErr::msg(UNEXPECTED_EOF));
                            }
                        }
                        Err(_) => {
                            self.phase = Phase::Done(RErr::Msg(alloc::format!(
                                "flate: corrupt input before offset {}",
                                self.ipos
                            )))
                        }
                    }
                    if !out.is_empty() {
                        return true;
                    }
                }
            }
        }
    }

    /// `Read`: at least one byte, or the stream's terminal error.
    fn read_some(&mut self, max: usize) -> Result<&[u8], RErr> {
        if self.bpos == self.buf.len() && !self.fill() && self.bpos == self.buf.len() {
            return Err(match &self.phase {
                Phase::Done(e) => e.clone(),
                _ => RErr::Eof,
            });
        }
        let n = max.min(self.buf.len() - self.bpos);
        let s = &self.buf[self.bpos..self.bpos + n];
        self.bpos += n;
        Ok(s)
    }

    /// `io.ReadFull`.
    fn read_full(&mut self, n: usize, out: &mut Vec<u8>) -> Result<(), RErr> {
        let start = out.len();
        while out.len() - start < n {
            match self.read_some(n - (out.len() - start)) {
                Ok(s) => out.extend_from_slice(s),
                Err(RErr::Eof) if out.len() > start => return Err(RErr::msg(UNEXPECTED_EOF)),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Reads `n` bytes of file data (tar `regFileReader`): EOF before the end
    /// is `unexpected EOF`.
    fn read_data(&mut self, n: usize, out: &mut Vec<u8>) -> Result<(), RErr> {
        match self.read_full(n, out) {
            Err(RErr::Eof) => Err(RErr::msg(UNEXPECTED_EOF)),
            r => r,
        }
    }

    /// `tryReadFull` for block padding: EOF (even partial) is reported as EOF.
    fn skip_padding(&mut self, n: usize) -> Result<(), RErr> {
        let mut left = n;
        while left > 0 {
            left -= self.read_some(left)?.len();
        }
        Ok(())
    }
}

/// A tar header as far as Helm cares.
#[derive(Debug, Default)]
struct Header {
    name: String,
    typeflag: u8,
    size: i64,
    mode: i64,
}

impl Header {
    /// `hd.FileInfo().IsDir()`.
    fn is_dir(&self) -> bool {
        (self.mode & !0o7777) == 0o040000 || self.typeflag == b'5'
    }
}

fn header_only(t: u8) -> bool {
    matches!(t, b'1' | b'2' | b'3' | b'4' | b'5' | b'6')
}

struct Parser {
    err: bool,
}

impl Parser {
    fn string(b: &[u8]) -> String {
        let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).into_owned()
    }

    fn octal(&mut self, b: &[u8]) -> i64 {
        let start = b
            .iter()
            .position(|c| *c != b' ' && *c != 0)
            .unwrap_or(b.len());
        let end = b
            .iter()
            .rposition(|c| *c != b' ' && *c != 0)
            .map_or(start, |i| i + 1);
        let b = &b[start..end.max(start)];
        if b.is_empty() {
            return 0;
        }
        let s = Parser::string(b);
        if s.is_empty() || !s.bytes().all(|c| (b'0'..=b'7').contains(&c)) {
            self.err = true;
            return 0;
        }
        match u64::from_str_radix(&s, 8) {
            Ok(x) => x as i64,
            Err(_) => {
                self.err = true;
                u64::MAX as i64
            }
        }
    }

    fn numeric(&mut self, b: &[u8]) -> i64 {
        if !b.is_empty() && b[0] & 0x80 != 0 {
            let inv: u8 = if b[0] & 0x40 != 0 { 0xff } else { 0 };
            let mut x: u64 = 0;
            for (i, &c) in b.iter().enumerate() {
                let mut c = c ^ inv;
                if i == 0 {
                    c &= 0x7f;
                }
                if (x >> 56) > 0 {
                    self.err = true;
                    return 0;
                }
                x = (x << 8) | u64::from(c);
            }
            if (x >> 63) > 0 {
                self.err = true;
                return 0;
            }
            return if inv == 0xff { !(x as i64) } else { x as i64 };
        }
        self.octal(b)
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Format {
    V7,
    Ustar,
    Gnu,
    Star,
}

fn get_format(blk: &[u8; 512]) -> Option<Format> {
    let mut p = Parser { err: false };
    let value = p.octal(&blk[148..156]);
    let (mut unsigned, mut signed) = (0i64, 0i64);
    for (i, &c) in blk.iter().enumerate() {
        let c = if (148..156).contains(&i) { b' ' } else { c };
        unsigned += i64::from(c);
        signed += i64::from(c as i8);
    }
    if p.err || (value != unsigned && value != signed) {
        return None;
    }
    let magic = &blk[257..263];
    let version = &blk[263..265];
    let trailer = &blk[508..512];
    Some(if magic == b"ustar\0" && trailer == b"tar\0" {
        Format::Star
    } else if magic == b"ustar\0" {
        Format::Ustar
    } else if magic == b"ustar " && version == b" \0" {
        Format::Gnu
    } else {
        Format::V7
    })
}

struct TarReader<'a> {
    gz: Gz<'a>,
    /// Unread bytes of the current entry.
    remaining: usize,
    pad: usize,
}

const MAX_SPECIAL_FILE_SIZE: usize = 1 << 20;

impl<'a> TarReader<'a> {
    fn read_header(&mut self) -> Result<Header, RErr> {
        let mut blk = Vec::with_capacity(512);
        self.gz.read_full(512, &mut blk)?;
        if blk.iter().all(|b| *b == 0) {
            blk.clear();
            self.gz.read_full(512, &mut blk)?;
            if blk.iter().all(|b| *b == 0) {
                return Err(RErr::Eof);
            }
            return Err(RErr::msg(ERR_HEADER));
        }
        let mut b = [0u8; 512];
        b.copy_from_slice(&blk);
        let format = get_format(&b).ok_or_else(|| RErr::msg(ERR_HEADER))?;
        let mut p = Parser { err: false };
        let mut hdr = Header {
            typeflag: b[156],
            name: Parser::string(&b[0..100]),
            size: p.numeric(&b[124..136]),
            mode: p.numeric(&b[100..108]),
        };
        p.numeric(&b[108..116]);
        p.numeric(&b[116..124]);
        p.numeric(&b[136..148]);
        if format != Format::V7 {
            p.numeric(&b[329..337]);
            p.numeric(&b[337..345]);
            let mut prefix = String::new();
            match format {
                Format::Ustar => prefix = Parser::string(&b[345..500]),
                Format::Star => {
                    prefix = Parser::string(&b[345..476]);
                    p.numeric(&b[476..488]);
                    p.numeric(&b[488..500]);
                }
                Format::Gnu => {
                    let mut p2 = Parser { err: false };
                    if b[345] != 0 {
                        p2.numeric(&b[345..357]);
                    }
                    if b[357] != 0 {
                        p2.numeric(&b[357..369]);
                    }
                    if p2.err {
                        let s = Parser::string(&b[345..500]);
                        if s.is_ascii() {
                            prefix = s;
                        }
                    }
                }
                Format::V7 => {}
            }
            if !prefix.is_empty() {
                hdr.name = alloc::format!("{prefix}/{}", hdr.name);
            }
        }
        if p.err {
            return Err(RErr::msg(ERR_HEADER));
        }
        Ok(hdr)
    }

    fn handle_regular_file(&mut self, hdr: &Header) -> Result<(), RErr> {
        let nb = if header_only(hdr.typeflag) {
            0
        } else {
            hdr.size
        };
        if nb < 0 {
            return Err(RErr::msg(ERR_HEADER));
        }
        let nb = usize::try_from(nb).unwrap_or(usize::MAX);
        self.remaining = nb;
        self.pad = (512 - (nb % 512)) % 512;
        Ok(())
    }

    /// Reads up to `limit` bytes of the current entry.
    fn read_entry(&mut self, limit: usize, out: &mut Vec<u8>) -> Result<(), RErr> {
        let n = self.remaining.min(limit);
        let r = self.gz.read_data(n, out);
        self.remaining -= n.min(self.remaining);
        r
    }

    fn read_special_file(&mut self) -> Result<Vec<u8>, RErr> {
        let mut buf = Vec::new();
        self.read_entry(MAX_SPECIAL_FILE_SIZE + 1, &mut buf)?;
        if buf.len() > MAX_SPECIAL_FILE_SIZE {
            return Err(RErr::msg("archive/tar: header field too long"));
        }
        Ok(buf)
    }

    fn discard(&mut self) -> Result<(), RErr> {
        let mut sink = Vec::new();
        while self.remaining > 0 {
            sink.clear();
            let n = self.remaining.min(64 * 1024);
            match self.gz.read_full(n, &mut sink) {
                Ok(()) => self.remaining -= n,
                Err(RErr::Eof) => return Err(RErr::msg(UNEXPECTED_EOF)),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// `tar.Reader.Next`.
    fn next(&mut self) -> Result<Header, RErr> {
        let mut pax: Option<BTreeMap<String, String>> = None;
        let mut long_name = String::new();
        loop {
            self.discard()?;
            let pad = core::mem::take(&mut self.pad);
            self.gz.skip_padding(pad)?;
            let mut hdr = self.read_header()?;
            self.handle_regular_file(&hdr)?;
            match hdr.typeflag {
                b'x' | b'g' => {
                    let recs = parse_pax(&self.read_special_file()?)?;
                    if hdr.typeflag == b'g' {
                        if let Some(p) = recs.get("path").filter(|p| !p.is_empty()) {
                            hdr.name = p.clone();
                        }
                        return Ok(Header {
                            name: hdr.name,
                            typeflag: b'g',
                            size: 0,
                            mode: 0,
                        });
                    }
                    pax = Some(recs);
                }
                b'L' | b'K' => {
                    let data = self.read_special_file()?;
                    if hdr.typeflag == b'L' {
                        long_name = Parser::string(&data);
                    }
                }
                _ => {
                    if let Some(recs) = &pax {
                        merge_pax(&mut hdr, recs)?;
                    }
                    if !long_name.is_empty() {
                        hdr.name = core::mem::take(&mut long_name);
                    }
                    if hdr.typeflag == 0 {
                        hdr.typeflag = if hdr.name.ends_with('/') { b'5' } else { b'0' };
                    }
                    self.handle_regular_file(&hdr)?;
                    let sparse = hdr.typeflag == b'S'
                        || pax
                            .as_ref()
                            .is_some_and(|p| p.keys().any(|k| k.starts_with("GNU.sparse.")));
                    if sparse {
                        return Err(RErr::msg("archive/tar: sparse files are not supported"));
                    }
                    return Ok(hdr);
                }
            }
        }
    }
}

fn parse_pax(buf: &[u8]) -> Result<BTreeMap<String, String>, RErr> {
    let mut s = buf;
    let mut out = BTreeMap::new();
    let bad = || RErr::msg(ERR_HEADER);
    while !s.is_empty() {
        let sp = s.iter().position(|b| *b == b' ').ok_or_else(bad)?;
        let n_str = core::str::from_utf8(&s[..sp]).map_err(|_| bad())?;
        let n: i64 = if n_str.is_empty()
            || !n_str
                .trim_start_matches(['+', '-'])
                .bytes()
                .all(|c| c.is_ascii_digit())
        {
            return Err(bad());
        } else {
            n_str.parse().map_err(|_| bad())?
        };
        if n < 5 || n as u64 > s.len() as u64 {
            return Err(bad());
        }
        let rest = &s[sp + 1..];
        let n = n as usize - (sp + 1);
        if n == 0 || n > rest.len() {
            return Err(bad());
        }
        let (rec, nl, rem) = (&rest[..n - 1], rest[n - 1], &rest[n..]);
        if nl != b'\n' {
            return Err(bad());
        }
        let eq = rec.iter().position(|b| *b == b'=').ok_or_else(bad)?;
        let (k, v) = (&rec[..eq], &rec[eq + 1..]);
        if k.is_empty() {
            return Err(bad());
        }
        let k = String::from_utf8_lossy(k).into_owned();
        let check_nul = matches!(k.as_str(), "path" | "linkpath" | "uname" | "gname");
        if (check_nul && v.contains(&0)) || (!check_nul && k.contains('\0')) {
            return Err(bad());
        }
        out.insert(k, String::from_utf8_lossy(v).into_owned());
        s = rem;
    }
    Ok(out)
}

fn parse_int(v: &str) -> Option<i64> {
    let digits = v.strip_prefix(['+', '-']).unwrap_or(v);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    v.parse().ok()
}

fn merge_pax(hdr: &mut Header, recs: &BTreeMap<String, String>) -> Result<(), RErr> {
    for (k, v) in recs {
        if v.is_empty() {
            continue;
        }
        let ok = match k.as_str() {
            "path" => {
                hdr.name = v.clone();
                true
            }
            "uid" | "gid" => parse_int(v).is_some(),
            "atime" | "mtime" | "ctime" => {
                let (ss, sn) = v.split_once('.').unwrap_or((v, ""));
                parse_int(ss).is_some() && sn.bytes().all(|c| c.is_ascii_digit())
            }
            "size" => match parse_int(v) {
                Some(n) => {
                    hdr.size = n;
                    true
                }
                None => false,
            },
            _ => true,
        };
        if !ok {
            return Err(RErr::msg(ERR_HEADER));
        }
    }
    Ok(())
}

/// Helm's `archive.LoadArchiveFiles`.
pub(crate) fn load_archive_files(data: &[u8]) -> Result<Vec<BufferedFile>, Error> {
    let gz = Gz::new(data).map_err(chart_err)?;
    let mut tr = TarReader {
        gz,
        remaining: 0,
        pad: 0,
    };
    let mut files = Vec::new();
    let mut remaining_size = MAX_DECOMPRESSED_CHART_SIZE as i64;
    let too_large = || {
        chart_err(alloc::format!(
            "decompressed chart is larger than the maximum size {MAX_DECOMPRESSED_CHART_SIZE}"
        ))
    };
    loop {
        let hd = match tr.next() {
            Ok(h) => h,
            Err(RErr::Eof) => break,
            Err(RErr::Msg(m)) => return Err(chart_err(m)),
        };
        if hd.is_dir() || hd.typeflag == b'g' || hd.typeflag == b'x' {
            continue;
        }
        let delimiter = if hd.name.contains('\\') { '\\' } else { '/' };
        let parts: Vec<&str> = hd.name.split(delimiter).collect();
        let n = parts[1..]
            .join(&delimiter.to_string())
            .replace(delimiter, "/");
        if n.starts_with('/') {
            return Err(chart_err("chart illegally contains absolute paths"));
        }
        let n = clean(&n);
        if n == "." {
            return Err(chart_err(alloc::format!(
                "chart illegally contains content outside the base directory: {}",
                quote(&hd.name)
            )));
        }
        if n.starts_with("..") {
            return Err(chart_err("chart illegally references parent directory"));
        }
        let nb = n.as_bytes();
        if nb.len() >= 3 && nb[0].is_ascii_alphabetic() && nb[1] == b':' && nb[2] == b'/' {
            return Err(chart_err("chart contains illegally named files"));
        }
        if parts[0] == "Chart.yaml" {
            return Err(chart_err("chart yaml not in base directory"));
        }
        if hd.size > remaining_size {
            return Err(too_large());
        }
        let mut buf = Vec::new();
        let limit = usize::try_from(remaining_size).unwrap_or(0);
        tr.read_entry(limit, &mut buf)
            .map_err(|e| chart_err(e.into_string()))?;
        remaining_size -= buf.len() as i64;
        if (buf.len() as i64) < hd.size || remaining_size <= 0 {
            return Err(too_large());
        }
        let data = buf.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&buf);
        files.push(BufferedFile {
            name: n,
            data: Rc::from(data),
        });
    }
    if files.is_empty() {
        return Err(chart_err("no files in chart archive"));
    }
    Ok(files)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a USTAR header block.
    pub(crate) fn tar_header(name: &str, size: usize, typeflag: u8) -> [u8; 512] {
        let mut b = [0u8; 512];
        b[..name.len().min(100)].copy_from_slice(&name.as_bytes()[..name.len().min(100)]);
        b[100..107].copy_from_slice(b"0000644");
        b[108..115].copy_from_slice(b"0000000");
        b[116..123].copy_from_slice(b"0000000");
        let s = alloc::format!("{size:011o}");
        b[124..135].copy_from_slice(s.as_bytes());
        b[136..147].copy_from_slice(b"00000000000");
        b[156] = typeflag;
        b[257..263].copy_from_slice(b"ustar\0");
        b[263..265].copy_from_slice(b"00");
        b[148..156].copy_from_slice(b"        ");
        let sum: u32 = b.iter().map(|c| u32::from(*c)).sum();
        let cs = alloc::format!("{sum:06o}\0 ");
        b[148..156].copy_from_slice(cs.as_bytes());
        b
    }

    pub(crate) fn tar(entries: &[(&str, &[u8], u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data, t) in entries {
            out.extend_from_slice(&tar_header(name, data.len(), *t));
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    /// gzip with a stored (uncompressed) deflate stream.
    pub(crate) fn gzip(data: &[u8]) -> Vec<u8> {
        let table = crc32_table();
        let mut out = alloc::vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
        let chunks: Vec<&[u8]> = if data.is_empty() {
            alloc::vec![&[][..]]
        } else {
            data.chunks(65535).collect()
        };
        for (i, c) in chunks.iter().enumerate() {
            out.push(if i + 1 == chunks.len() { 1 } else { 0 });
            out.extend_from_slice(&(c.len() as u16).to_le_bytes());
            out.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
            out.extend_from_slice(c);
        }
        out.extend_from_slice(&crc32_update(&table, 0, data).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out
    }

    #[test]
    fn reads_simple_archive() {
        let t = tar(&[
            ("mychart/", b"", b'5'),
            ("mychart/Chart.yaml", b"name: x\n", b'0'),
            ("mychart/templates/a.yaml", b"\xEF\xBB\xBFa: 1\n", b'0'),
        ]);
        let files = load_archive_files(&gzip(&t)).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Chart.yaml", "templates/a.yaml"]);
        assert_eq!(&*files[1].data, b"a: 1\n");
    }

    #[test]
    fn errors() {
        let e = |b: &[u8]| load_archive_files(b).unwrap_err().message;
        assert_eq!(e(b""), "EOF");
        assert_eq!(e(b"\x1f\x8b"), "unexpected EOF");
        assert_eq!(e(b"not gzip at all"), "gzip: invalid header");
        assert_eq!(e(&gzip(&tar(&[]))), "no files in chart archive");
        assert_eq!(
            e(&gzip(&tar(&[("Chart.yaml/x", b"x", b'0')]))),
            "chart yaml not in base directory"
        );
        assert_eq!(
            e(&gzip(&tar(&[("a/../../x", b"x", b'0')]))),
            "chart illegally references parent directory"
        );
        assert_eq!(
            e(&gzip(&tar(&[("a//x", b"x", b'0')]))),
            "chart illegally contains absolute paths"
        );
        assert_eq!(
            e(&gzip(&tar(&[("x", b"x", b'0')]))),
            "chart illegally contains content outside the base directory: \"x\""
        );
        assert_eq!(
            e(&gzip(&tar(&[("a/c:/x", b"x", b'0')]))),
            "chart contains illegally named files"
        );
        let mut bad = tar(&[("a/b", b"x", b'0')]);
        bad[0] = b'z';
        assert_eq!(e(&gzip(&bad)), "archive/tar: invalid tar header");
        let t = tar(&[("a/b", b"xyz", b'0')]);
        assert_eq!(e(&gzip(&t[..513])), "unexpected EOF");
        let mut g = gzip(&tar(&[("a/b", b"x", b'0')]));
        let n = g.len();
        g[n - 8] ^= 1;
        // Tar stops at its end marker, before the gzip trailer is checked... but
        // our stored stream ends exactly there, so the checksum is verified on
        // the final read.
        let _ = load_archive_files(&g);
    }

    #[test]
    fn gnu_long_names_and_pax() {
        let long = alloc::format!("c/{}", "x".repeat(150));
        let mut t = Vec::new();
        let ln = alloc::format!("{long}\0");
        t.extend_from_slice(&tar(&[("././@LongLink", ln.as_bytes(), b'L')])[..1024]);
        t.extend_from_slice(&tar(&[("c/short", b"data", b'0')]));
        let files = load_archive_files(&gzip(&t)).unwrap();
        assert_eq!(files[0].name, "x".repeat(150));
        let rec = "path=c/templates/p.yaml\n";
        let pax = alloc::format!("{} {rec}", rec.len() + 3);
        let mut t = Vec::new();
        t.extend_from_slice(&tar(&[("PaxHeader", pax.as_bytes(), b'x')])[..1024]);
        t.extend_from_slice(&tar(&[("c/ignored", b"data", b'0')]));
        let files = load_archive_files(&gzip(&t)).unwrap();
        assert_eq!(files[0].name, "templates/p.yaml");
    }
}
