//! `github.com/gobwas/glob` (v0.2.3) with `/` as the only separator, as used
//! by `.Files.Glob`.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Text(Vec<char>),
    /// `*`: any run of non-separators.
    Any,
    /// `**`: anything.
    Super,
    /// `?`: one non-separator.
    Single,
    List {
        chars: Vec<char>,
        not: bool,
    },
    Range {
        lo: char,
        hi: char,
        not: bool,
    },
    /// `{a,b}`: alternatives (each a pattern).
    AnyOf(Vec<Vec<Node>>),
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Eof,
    Text(String),
    Any,
    Super,
    Single,
    Not,
    Separator,
    RangeOpen,
    RangeClose,
    RangeLo(char),
    RangeHi(char),
    RangeBetween,
    TermsOpen,
    TermsClose,
}

struct Lexer<'a> {
    data: &'a str,
    pos: usize,
    terms_level: i32,
    tokens: Vec<Tok>,
    last_size: usize,
    err: Option<&'static str>,
}

const EOF: char = '\0';

impl Lexer<'_> {
    fn peek(&mut self) -> (char, usize) {
        match self.data[self.pos..].chars().next() {
            None => (EOF, 0),
            Some('\u{FFFD}') => {
                self.err = Some("could not read rune");
                (EOF, 0)
            }
            Some(c) => (c, c.len_utf8()),
        }
    }
    fn read(&mut self) -> char {
        let (c, w) = self.peek();
        self.pos += w;
        self.last_size = w;
        c
    }
    fn unread(&mut self) {
        self.pos -= self.last_size;
        self.last_size = 0;
    }

    fn next(&mut self) -> Result<Tok, &'static str> {
        loop {
            if let Some(e) = self.err {
                return Err(e);
            }
            if !self.tokens.is_empty() {
                return Ok(self.tokens.remove(0));
            }
            self.fetch_item();
        }
    }

    fn fetch_item(&mut self) {
        let r = self.read();
        match r {
            EOF => self.tokens.push(Tok::Eof),
            '{' => {
                self.terms_level += 1;
                if self.terms_level > 64 {
                    self.err = Some("pattern too complex");
                }
                self.tokens.push(Tok::TermsOpen);
            }
            ',' if self.terms_level > 0 => self.tokens.push(Tok::Separator),
            '}' if self.terms_level > 0 => {
                self.tokens.push(Tok::TermsClose);
                self.terms_level -= 1;
            }
            '[' => {
                self.tokens.push(Tok::RangeOpen);
                self.fetch_range();
            }
            '?' => self.tokens.push(Tok::Single),
            '*' => {
                if self.read() == '*' {
                    self.tokens.push(Tok::Super);
                } else {
                    self.unread();
                    self.tokens.push(Tok::Any);
                }
            }
            _ => {
                self.unread();
                let breakers: &[char] = if self.terms_level > 0 {
                    &['?', '*', '[', '{', '}', ',']
                } else {
                    &['?', '*', '[', '{']
                };
                self.fetch_text(breakers);
            }
        }
    }

    fn fetch_range(&mut self) {
        let mut want_hi = false;
        let mut want_close = false;
        let mut seen_not = false;
        loop {
            let r = self.read();
            if r == EOF {
                if self.err.is_none() {
                    self.err = Some("unexpected end of input");
                }
                return;
            }
            if want_close {
                if r != ']' {
                    self.err = Some("expected close range character");
                } else {
                    self.tokens.push(Tok::RangeClose);
                }
                return;
            }
            if want_hi {
                self.tokens.push(Tok::RangeHi(r));
                want_close = true;
                continue;
            }
            if !seen_not && r == '!' {
                self.tokens.push(Tok::Not);
                seen_not = true;
                continue;
            }
            let (n, w) = self.peek();
            if n == '-' {
                self.pos += w;
                self.tokens.push(Tok::RangeLo(r));
                self.tokens.push(Tok::RangeBetween);
                want_hi = true;
                continue;
            }
            self.unread();
            self.fetch_text(&[']']);
            want_close = true;
        }
    }

    fn fetch_text(&mut self, breakers: &[char]) {
        let mut data = String::new();
        let mut escaped = false;
        loop {
            let r = self.read();
            if r == EOF {
                break;
            }
            if !escaped {
                if r == '\\' {
                    escaped = true;
                    continue;
                }
                if breakers.contains(&r) {
                    self.unread();
                    break;
                }
            }
            escaped = false;
            data.push(r);
        }
        if !data.is_empty() {
            self.tokens.push(Tok::Text(data));
        }
    }
}

/// A compiled glob.
#[derive(Clone, Debug)]
pub struct Glob {
    nodes: Vec<Node>,
}

/// Tree under construction: a stack of AnyOf groups.
struct Frame {
    alts: Vec<Vec<Node>>,
}

impl Glob {
    /// `glob.Compile(pattern, '/')`.
    pub fn compile(pattern: &str) -> Result<Glob, &'static str> {
        let mut lx = Lexer {
            data: pattern,
            pos: 0,
            terms_level: 0,
            tokens: Vec::new(),
            last_size: 0,
            err: None,
        };
        // root pattern + stack of open AnyOf frames
        let mut root: Vec<Node> = Vec::new();
        let mut stack: Vec<Frame> = Vec::new();
        fn cur<'a>(root: &'a mut Vec<Node>, stack: &'a mut [Frame]) -> &'a mut Vec<Node> {
            match stack.last_mut() {
                Some(f) => f.alts.last_mut().expect("frame has an alternative"),
                None => root,
            }
        }
        loop {
            match lx.next()? {
                Tok::Eof => break,
                Tok::Text(t) => cur(&mut root, &mut stack).push(Node::Text(t.chars().collect())),
                Tok::Any => cur(&mut root, &mut stack).push(Node::Any),
                Tok::Super => cur(&mut root, &mut stack).push(Node::Super),
                Tok::Single => cur(&mut root, &mut stack).push(Node::Single),
                Tok::RangeOpen => {
                    let node = parse_range(&mut lx)?;
                    cur(&mut root, &mut stack).push(node);
                }
                Tok::TermsOpen => stack.push(Frame {
                    alts: alloc::vec![Vec::new()],
                }),
                Tok::Separator => match stack.last_mut() {
                    Some(f) => f.alts.push(Vec::new()),
                    None => return Err("unexpected token"),
                },
                Tok::TermsClose => match stack.pop() {
                    Some(f) => cur(&mut root, &mut stack).push(Node::AnyOf(f.alts)),
                    None => return Err("unexpected token"),
                },
                _ => return Err("unexpected token"),
            }
        }
        // Unclosed terms are still inserted into the tree.
        while let Some(f) = stack.pop() {
            cur(&mut root, &mut stack).push(Node::AnyOf(f.alts));
        }
        if count_nodes(&root) > MAX_NODES {
            return Err("pattern too complex");
        }
        Ok(Glob { nodes: root })
    }

    /// `Glob.Match(s)`.
    pub fn matches(&self, s: &str) -> bool {
        let chars: Vec<char> = s.chars().collect();
        let mut budget = 1_000_000u32;
        match_seq(&self.nodes, &chars, 0, &mut budget, &mut |p, _| {
            p == chars.len()
        })
    }
}

/// Matching recurses per node; bound it so hostile patterns cannot exhaust
/// the stack (such patterns fall back to `**` in `.Files.Glob`).
const MAX_NODES: usize = 1000;

fn count_nodes(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            Node::AnyOf(alts) => 1 + alts.iter().map(|a| count_nodes(a)).sum::<usize>(),
            _ => 1,
        })
        .sum()
}

fn parse_range(lx: &mut Lexer<'_>) -> Result<Node, &'static str> {
    let mut not = false;
    let mut lo = '\0';
    let mut hi = '\0';
    let mut chars = String::new();
    loop {
        match lx.next()? {
            Tok::Eof => return Err("unexpected end"),
            Tok::Not => not = true,
            Tok::RangeLo(c) => lo = c,
            Tok::RangeBetween => {}
            Tok::RangeHi(c) => {
                hi = c;
                if hi < lo {
                    return Err("hi character should be greater than lo");
                }
            }
            Tok::Text(t) => chars = t,
            Tok::RangeClose => {
                let is_range = lo != '\0' && hi != '\0';
                let is_chars = !chars.is_empty();
                if is_chars == is_range {
                    return Err("could not parse range");
                }
                return Ok(if is_range {
                    Node::Range { lo, hi, not }
                } else {
                    Node::List {
                        chars: chars.chars().collect(),
                        not,
                    }
                });
            }
            _ => {}
        }
    }
}

/// Continuation-passing backtracking matcher.
fn match_seq(
    nodes: &[Node],
    s: &[char],
    pos: usize,
    budget: &mut u32,
    k: &mut dyn FnMut(usize, &mut u32) -> bool,
) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    let Some((first, rest)) = nodes.split_first() else {
        return k(pos, budget);
    };
    match first {
        Node::Text(t) => {
            if s.len() >= pos + t.len() && s[pos..pos + t.len()] == t[..] {
                match_seq(rest, s, pos + t.len(), budget, k)
            } else {
                false
            }
        }
        Node::Single => {
            if pos < s.len() && s[pos] != '/' {
                match_seq(rest, s, pos + 1, budget, k)
            } else {
                false
            }
        }
        Node::List { chars, not } => {
            if pos < s.len() && chars.contains(&s[pos]) != *not {
                match_seq(rest, s, pos + 1, budget, k)
            } else {
                false
            }
        }
        Node::Range { lo, hi, not } => {
            if pos < s.len() && (*lo <= s[pos] && s[pos] <= *hi) != *not {
                match_seq(rest, s, pos + 1, budget, k)
            } else {
                false
            }
        }
        Node::Any | Node::Super => {
            let mut end = pos;
            loop {
                if match_seq(rest, s, end, budget, k) {
                    return true;
                }
                if end >= s.len() || (matches!(first, Node::Any) && s[end] == '/') || *budget == 0 {
                    return false;
                }
                end += 1;
            }
        }
        Node::AnyOf(alts) => {
            for alt in alts {
                let mut cont = |p: usize, b: &mut u32| match_seq(rest, s, p, b, k);
                if match_seq(alt, s, pos, budget, &mut cont) {
                    return true;
                }
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, s: &str) -> bool {
        Glob::compile(p).unwrap().matches(s)
    }

    #[test]
    fn basics() {
        assert!(m("*.txt", "a.txt"));
        assert!(!m("*.txt", "d/a.txt"));
        assert!(m("**.txt", "d/a.txt"));
        assert!(m("d/**", "d/a/b"));
        assert!(m("?", "a"));
        assert!(!m("?", "/"));
        assert!(m("[a-c]x", "bx"));
        assert!(m("[!a-c]x", "dx"));
        assert!(m("[abc]", "b"));
        assert!(m("{a,b}*", "bzz"));
        assert!(m("{a,b/**}", "b/z/z"));
        assert!(m("a{,b}", "a"));
        assert!(m("a\\*", "a*"));
        assert!(m("}", "}"));
        assert!(Glob::compile("[").is_err());
        assert!(Glob::compile("[]").is_err());
        assert!(Glob::compile("[z-a]").is_err());
    }
}
