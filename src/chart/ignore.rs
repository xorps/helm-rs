//! `.helmignore` rules (Helm `pkg/ignore`) and Go's `path.Match`.

use alloc::string::String;
use alloc::vec::Vec;

use super::gostr::{base, decode_rune};

/// Go's `path.ErrBadPattern` message.
pub(crate) const ERR_BAD_PATTERN: &str = "syntax error in pattern";

/// Go's `path.Match` (identical to `filepath.Match` on Unix).
pub(crate) fn path_match(pattern: &str, name: &str) -> Result<bool, &'static str> {
    let mut pattern = pattern.as_bytes();
    let mut name = name.as_bytes();
    'pattern: while !pattern.is_empty() {
        let (star, chunk, rest) = scan_chunk(pattern);
        pattern = rest;
        if star && chunk.is_empty() {
            return Ok(!name.contains(&b'/'));
        }
        let (t, ok, err) = match_chunk(chunk, name);
        if ok && (t.is_empty() || !pattern.is_empty()) {
            name = t;
            continue;
        }
        if let Some(e) = err {
            return Err(e);
        }
        if star {
            let mut i = 0;
            while i < name.len() && name[i] != b'/' {
                let (t, ok, err) = match_chunk(chunk, &name[i + 1..]);
                if ok {
                    if pattern.is_empty() && !t.is_empty() {
                        i += 1;
                        continue;
                    }
                    name = t;
                    continue 'pattern;
                }
                if let Some(e) = err {
                    return Err(e);
                }
                i += 1;
            }
        }
        while !pattern.is_empty() {
            let (_, chunk, rest) = scan_chunk(pattern);
            pattern = rest;
            if let (_, _, Some(e)) = match_chunk(chunk, b"") {
                return Err(e);
            }
        }
        return Ok(false);
    }
    Ok(name.is_empty())
}

fn scan_chunk(mut pattern: &[u8]) -> (bool, &[u8], &[u8]) {
    let mut star = false;
    while let [b'*', rest @ ..] = pattern {
        pattern = rest;
        star = true;
    }
    let mut inrange = false;
    let mut i = 0;
    while i < pattern.len() {
        match pattern[i] {
            b'\\' => {
                if i + 1 < pattern.len() {
                    i += 1;
                }
            }
            b'[' => inrange = true,
            b']' => inrange = false,
            b'*' if !inrange => break,
            _ => {}
        }
        i += 1;
    }
    (star, &pattern[..i], &pattern[i..])
}

fn rune_at(s: &[u8]) -> (u32, usize) {
    match decode_rune(s) {
        (Some(c), w) => (c as u32, w),
        (None, w) => (0xFFFD, w),
    }
}

fn get_esc(chunk: &[u8]) -> Result<(u32, &[u8]), &'static str> {
    if chunk.is_empty() || chunk[0] == b'-' || chunk[0] == b']' {
        return Err(ERR_BAD_PATTERN);
    }
    let mut chunk = chunk;
    if chunk[0] == b'\\' {
        chunk = &chunk[1..];
        if chunk.is_empty() {
            return Err(ERR_BAD_PATTERN);
        }
    }
    let (r, n) = decode_rune(chunk);
    let n = n.max(1);
    if r.is_none() && n == 1 {
        return Err(ERR_BAD_PATTERN);
    }
    let rest = &chunk[n..];
    if rest.is_empty() {
        return Err(ERR_BAD_PATTERN);
    }
    Ok((r.map_or(0xFFFD, |c| c as u32), rest))
}

/// Returns `(rest, ok, err)`.
fn match_chunk<'a>(mut chunk: &[u8], mut s: &'a [u8]) -> (&'a [u8], bool, Option<&'static str>) {
    let mut failed = false;
    while !chunk.is_empty() {
        if !failed && s.is_empty() {
            failed = true;
        }
        match chunk[0] {
            b'[' => {
                let mut r = 0u32;
                if !failed {
                    let (rr, n) = rune_at(s);
                    r = rr;
                    s = &s[n.max(1).min(s.len())..];
                }
                chunk = &chunk[1..];
                let mut negated = false;
                if chunk.first() == Some(&b'^') {
                    negated = true;
                    chunk = &chunk[1..];
                }
                let mut matched = false;
                let mut nrange = 0;
                loop {
                    if chunk.first() == Some(&b']') && nrange > 0 {
                        chunk = &chunk[1..];
                        break;
                    }
                    let (lo, rest) = match get_esc(chunk) {
                        Ok(x) => x,
                        Err(e) => return (b"", false, Some(e)),
                    };
                    chunk = rest;
                    let mut hi = lo;
                    if chunk[0] == b'-' {
                        match get_esc(&chunk[1..]) {
                            Ok((h, rest)) => {
                                hi = h;
                                chunk = rest;
                            }
                            Err(e) => return (b"", false, Some(e)),
                        }
                    }
                    if lo <= r && r <= hi {
                        matched = true;
                    }
                    nrange += 1;
                }
                if matched == negated {
                    failed = true;
                }
            }
            b'?' => {
                if !failed {
                    if s[0] == b'/' {
                        failed = true;
                    }
                    let (_, n) = rune_at(s);
                    s = &s[n.max(1).min(s.len())..];
                }
                chunk = &chunk[1..];
            }
            c => {
                let c = if c == b'\\' {
                    chunk = &chunk[1..];
                    if chunk.is_empty() {
                        return (b"", false, Some(ERR_BAD_PATTERN));
                    }
                    chunk[0]
                } else {
                    c
                };
                if !failed {
                    if c != s[0] {
                        failed = true;
                    }
                    s = &s[1..];
                }
                chunk = &chunk[1..];
            }
        }
    }
    if failed {
        return (b"", false, None);
    }
    (s, true, None)
}

#[derive(Clone, Debug)]
enum Kind {
    /// `/pattern`: matched against the whole path.
    Rooted,
    /// Contains `/`: matched against the whole path.
    Path,
    /// Matched against the base name.
    Base,
}

#[derive(Clone, Debug)]
struct Pattern {
    rule: String,
    kind: Kind,
    negate: bool,
    must_dir: bool,
}

/// Parsed `.helmignore` rules.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    patterns: Vec<Pattern>,
}

impl Rules {
    pub fn empty() -> Rules {
        Rules::default()
    }

    /// `ignore.Parse`: one rule per line. Errors are Go's messages.
    pub fn parse(content: &[u8]) -> Result<Rules, String> {
        let mut r = Rules::empty();
        let content = content.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(content);
        // bufio.Scanner with ScanLines: split on \n, drop a trailing \r.
        let mut lines: Vec<&[u8]> = content.split(|b| *b == b'\n').collect();
        if content.ends_with(b"\n") || content.is_empty() {
            lines.pop();
        }
        for line in lines {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = String::from_utf8_lossy(line);
            r.parse_rule(&line)?;
        }
        Ok(r)
    }

    /// `Rules.AddDefaults`: ignore hidden files in `templates/`.
    pub fn add_defaults(&mut self) {
        let _ = self.parse_rule("templates/.?*");
    }

    fn parse_rule(&mut self, rule: &str) -> Result<(), String> {
        let rule = rule.trim();
        if rule.is_empty() || rule.starts_with('#') {
            return Ok(());
        }
        if rule.contains("**") {
            return Err("double-star (**) syntax is not supported".into());
        }
        path_match(rule, "abc").map_err(String::from)?;
        let mut rule = rule;
        let mut negate = false;
        let mut must_dir = false;
        if let Some(r) = rule.strip_prefix('!') {
            negate = true;
            rule = r;
        }
        if let Some(r) = rule.strip_suffix('/') {
            must_dir = true;
            rule = r;
        }
        let (kind, rule) = if let Some(r) = rule.strip_prefix('/') {
            (Kind::Rooted, r)
        } else if rule.contains('/') {
            (Kind::Path, rule)
        } else {
            (Kind::Base, rule)
        };
        self.patterns.push(Pattern { rule: rule.into(), kind, negate, must_dir });
        Ok(())
    }

    /// `Rules.Ignore(path, fi)`.
    pub fn ignore(&self, path: &str, is_dir: bool) -> bool {
        if path.is_empty() || path == "." || path == "./" {
            return false;
        }
        for p in &self.patterns {
            let matches = |n: &str| -> bool {
                let n = match p.kind {
                    Kind::Base => base(n),
                    _ => n,
                };
                path_match(&p.rule, n).unwrap_or(false)
            };
            if p.negate {
                if p.must_dir && !is_dir {
                    return true;
                }
                if !matches(path) {
                    return true;
                }
                continue;
            }
            if p.must_dir && !is_dir {
                continue;
            }
            if matches(path) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_path_match() {
        let cases: &[(&str, &str, Result<bool, &str>)] = &[
            ("abc", "abc", Ok(true)),
            ("*", "abc", Ok(true)),
            ("*c", "abc", Ok(true)),
            ("a*", "a", Ok(true)),
            ("a*", "ab/c", Ok(false)),
            ("a*/b", "abc/b", Ok(true)),
            ("a*b*c*d*e*/f", "axbxcxdxe/f", Ok(true)),
            ("a*b?c*x", "abxbbxdbxebxczzx", Ok(true)),
            ("a*b?c*x", "abxbbxdbxebxczzy", Ok(false)),
            ("ab[c]", "abc", Ok(true)),
            ("ab[b-d]", "abc", Ok(true)),
            ("ab[^c]", "abc", Ok(false)),
            ("ab[^b-d]", "abe", Ok(true)),
            ("a\\*b", "a*b", Ok(true)),
            ("a?b", "a☺b", Ok(true)),
            ("a[^a]b", "a☺b", Ok(true)),
            ("[", "a", Err(ERR_BAD_PATTERN)),
            ("[^", "a", Err(ERR_BAD_PATTERN)),
            ("[^bc", "a", Err(ERR_BAD_PATTERN)),
            ("a[", "a", Err(ERR_BAD_PATTERN)),
            ("a[", "ab", Err(ERR_BAD_PATTERN)),
            ("a[", "x", Err(ERR_BAD_PATTERN)),
            ("a/b[", "x", Err(ERR_BAD_PATTERN)),
            ("*x", "xxx", Ok(true)),
            ("[-]", "-", Err(ERR_BAD_PATTERN)),
            ("[x-]", "x", Err(ERR_BAD_PATTERN)),
            ("\\", "a", Err(ERR_BAD_PATTERN)),
            ("[a-b-c]", "a", Err(ERR_BAD_PATTERN)),
            ("a*b*c", "a/b/c", Ok(false)),
            ("*/*", "a/b", Ok(true)),
        ];
        for (p, n, want) in cases {
            assert_eq!(path_match(p, n), *want, "Match({p:?}, {n:?})");
        }
    }

    #[test]
    fn helmignore_rules() {
        let mut r = Rules::parse(b"# comment\n*.txt\n/root.yaml\nsecrets/\ndocs/*.md\n").unwrap();
        r.add_defaults();
        assert!(r.ignore("a/b/c.txt", false));
        assert!(r.ignore("root.yaml", false));
        assert!(!r.ignore("x/root.yaml", false));
        assert!(r.ignore("secrets", true));
        assert!(!r.ignore("secrets", false));
        assert!(r.ignore("docs/a.md", false));
        assert!(r.ignore("templates/.hidden", false));
        assert!(!r.ignore("templates/x.yaml", false));
        assert_eq!(Rules::parse(b"a/**").unwrap_err(), "double-star (**) syntax is not supported");
        assert_eq!(Rules::parse(b"[").unwrap_err(), "syntax error in pattern");
    }
}
