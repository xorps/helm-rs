//! JSON schema `format` assertions of `santhosh-tekuri/jsonschema/v6`
//! (asserted for drafts 4, 6 and 7 only, as Helm configures the library),
//! with the library's error messages (including the Go standard library
//! errors it wraps: `time.Parse`, `strconv.Atoi`, `netip.ParseAddr`,
//! `url.Parse`, `regexp.Compile`).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use super::gostr::{is_print, quote, quote_bytes};

type R = Result<(), String>;

/// Validates `s` against the named format. Unknown formats pass.
pub(crate) fn check(name: &str, s: &str) -> R {
    match name {
        "json-pointer" => json_pointer(s),
        "relative-json-pointer" => relative_json_pointer(s),
        "uuid" => uuid(s),
        "duration" => duration(s),
        "period" => period(s),
        "ipv4" => ipv4(s),
        "ipv6" => ipv6(s),
        "hostname" => hostname(s),
        "email" => email(s),
        "date" => date(s.as_bytes()),
        "time" => time(s.as_bytes()),
        "date-time" => date_time(s.as_bytes()),
        "uri" | "iri" => uri(s),
        "uri-reference" | "iri-reference" => uri_reference(s),
        "uri-template" => uri_template(s),
        "semver" => semver(s),
        "regex" => crate::funcs::sprig::goregex::compile(s).map(|_| ()),
        _ => Ok(()),
    }
}

/// Go `%q` of a rune.
fn qr(c: char) -> String {
    match c {
        '\'' => "'\\''".into(),
        '\\' => "'\\\\'".into(),
        '\x07' => "'\\a'".into(),
        '\x08' => "'\\b'".into(),
        '\x0C' => "'\\f'".into(),
        '\n' => "'\\n'".into(),
        '\r' => "'\\r'".into(),
        '\t' => "'\\t'".into(),
        '\x0B' => "'\\v'".into(),
        c if is_print(c) => alloc::format!("'{c}'"),
        c if (c as u32) < 0x80 => alloc::format!("'\\x{:02x}'", c as u32),
        c if (c as u32) < 0x10000 => alloc::format!("'\\u{:04x}'", c as u32),
        c => alloc::format!("'\\U{:08x}'", c as u32),
    }
}

fn err(m: impl Into<String>) -> R {
    Err(m.into())
}

fn json_pointer(s: &str) -> R {
    if s.is_empty() {
        return Ok(());
    }
    if !s.starts_with('/') {
        return err("not starting with /");
    }
    for tok in s.split('/').skip(1) {
        let mut escape = false;
        for ch in tok.chars() {
            if escape {
                escape = false;
                if ch != '0' && ch != '1' {
                    return err("~ must be followed by 0 or 1");
                }
                continue;
            }
            if ch == '~' {
                escape = true;
                continue;
            }
            let c = ch as u32;
            if !(c <= 0x2E || (0x30..=0x7D).contains(&c) || c >= 0x7F) {
                return err(alloc::format!("invalid character {}", qr(ch)));
            }
        }
        if escape {
            return err("~ must be followed by 0 or 1");
        }
    }
    Ok(())
}

fn relative_json_pointer(s: &str) -> R {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 {
        return err("must start with non-negative integer");
    }
    if n > 1 && s.starts_with('0') {
        return err("starts with zero");
    }
    let rest = &s[n..];
    if rest == "#" {
        return Ok(());
    }
    json_pointer(rest)
}

fn uuid(s: &str) -> R {
    let lens = [8, 4, 4, 4, 12];
    let groups: Vec<&str> = s.split('-').collect();
    if groups.len() != lens.len() {
        return err("must have 5 elements");
    }
    for (i, g) in groups.iter().enumerate() {
        if g.len() != lens[i] {
            return err(alloc::format!("element {} must be {} characters long", i + 1, lens[i]));
        }
        for ch in g.chars() {
            if !ch.is_ascii_hexdigit() {
                return err(alloc::format!("non-hex character {}", qr(ch)));
            }
        }
    }
    Ok(())
}

fn duration(s: &str) -> R {
    let Some(s) = s.strip_prefix('P') else { return err("must start with P") };
    if s.is_empty() {
        return err("nothing after P");
    }
    if let Some(w) = s.strip_suffix('W') {
        if w.is_empty() {
            return err("no number in week");
        }
        if !w.bytes().all(|c| c.is_ascii_digit()) {
            return err("invalid week");
        }
        return Ok(());
    }
    let all_units = ["YMD", "HMS"];
    for (i, part) in s.split('T').enumerate() {
        if i != 0 && part.is_empty() {
            return err("no time elements");
        }
        if i >= all_units.len() {
            return err("more than one T");
        }
        let mut units = all_units[i];
        let mut p = part;
        while !p.is_empty() {
            let digits = p.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return err("missing number");
            }
            p = &p[digits..];
            let Some(unit) = p.chars().next() else { return err("missing unit") };
            match units.find(unit) {
                Some(j) => units = &units[j + 1..],
                None => {
                    // Go %q of a byte (uint8) prints it as a character.
                    let b = p.as_bytes()[0] as char;
                    if all_units[i].contains(unit) {
                        return err(alloc::format!("unit {} out of order", qr(b)));
                    }
                    return err(alloc::format!("invalid unit {}", qr(b)));
                }
            }
            p = &p[unit.len_utf8()..];
        }
    }
    Ok(())
}

fn period(s: &str) -> R {
    let Some(slash) = s.find('/') else { return err("missing slash") };
    let (start, end) = (&s[..slash], &s[slash + 1..]);
    if start.starts_with('P') {
        duration(start).map_err(|e| alloc::format!("invalid start duration: {e}"))?;
        date_time(end.as_bytes()).map_err(|e| alloc::format!("invalid end date-time: {e}"))?;
    } else {
        date_time(start.as_bytes()).map_err(|e| alloc::format!("invalid start date-time: {e}"))?;
        if end.starts_with('P') {
            duration(end).map_err(|e| alloc::format!("invalid end duration: {e}"))?;
        } else {
            date_time(end.as_bytes()).map_err(|e| alloc::format!("invalid end date-time: {e}"))?;
        }
    }
    Ok(())
}

/// `strconv.Atoi`.
fn atoi(s: &str) -> Result<i64, String> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err(alloc::format!("strconv.Atoi: parsing {}: invalid syntax", quote(s)));
    }
    s.parse::<i64>().map_err(|_| alloc::format!("strconv.Atoi: parsing {}: value out of range", quote(s)))
}

fn ipv4(s: &str) -> R {
    let groups: Vec<&str> = s.split('.').collect();
    if groups.len() != 4 {
        return err("expected four decimals");
    }
    for g in groups {
        if g.len() > 1 && g.starts_with('0') {
            return err("leading zeros");
        }
        let n = atoi(g)?;
        if !(0..=255).contains(&n) {
            return err("decimal must be between 0 and 255");
        }
    }
    Ok(())
}

fn addr_err(input: &str, msg: &str, at: &str) -> String {
    if at.is_empty() {
        alloc::format!("ParseAddr({}): {msg}", quote(input))
    } else {
        alloc::format!("ParseAddr({}): {msg} (at {})", quote(input), quote(at))
    }
}

fn parse_ipv4_fields(input: &str, s: &[u8]) -> Result<(), String> {
    let (mut val, mut pos, mut dig_len) = (0u32, 0, 0);
    for i in 0..s.len() {
        let c = s[i];
        if c.is_ascii_digit() {
            if dig_len == 1 && val == 0 {
                return Err(addr_err(input, "IPv4 field has octet with leading zero", ""));
            }
            val = val * 10 + u32::from(c - b'0');
            dig_len += 1;
            if val > 255 {
                return Err(addr_err(input, "IPv4 field has value >255", ""));
            }
        } else if c == b'.' {
            if i == 0 || i == s.len() - 1 || s[i - 1] == b'.' {
                return Err(addr_err(input, "IPv4 field must have at least one digit", &String::from_utf8_lossy(&s[i..])));
            }
            if pos == 3 {
                return Err(addr_err(input, "IPv4 address too long", ""));
            }
            pos += 1;
            val = 0;
            dig_len = 0;
        } else {
            return Err(addr_err(input, "unexpected character", &String::from_utf8_lossy(&s[i..])));
        }
    }
    if pos < 3 {
        return Err(addr_err(input, "IPv4 address too short", ""));
    }
    Ok(())
}

/// `netip.ParseAddr`; returns the zone (empty if none) and whether it is IPv4.
fn parse_addr(input: &str) -> Result<(String, bool), String> {
    for c in input.bytes() {
        match c {
            b'.' => return parse_ipv4_fields(input, input.as_bytes()).map(|_| (String::new(), true)),
            b':' => return parse_ipv6(input).map(|z| (z, false)),
            b'%' => return Err(addr_err(input, "missing IPv6 address", "")),
            _ => {}
        }
    }
    Err(addr_err(input, "unable to parse IP", ""))
}

fn parse_ipv6(input: &str) -> Result<String, String> {
    let mut s = input.as_bytes();
    let mut zone = String::new();
    if let Some(i) = s.iter().position(|c| *c == b'%') {
        zone = String::from_utf8_lossy(&s[i + 1..]).into_owned();
        s = &s[..i];
        if zone.is_empty() {
            return Err(addr_err(input, "zone must be a non-empty string", ""));
        }
    }
    let at = |s: &[u8]| String::from_utf8_lossy(s).into_owned();
    let mut ellipsis: i32 = -1;
    if s.len() >= 2 && s[0] == b':' && s[1] == b':' {
        ellipsis = 0;
        s = &s[2..];
        if s.is_empty() {
            return Ok(zone);
        }
    }
    let mut i = 0;
    while i < 16 {
        let mut off = 0;
        let mut acc: u32 = 0;
        while off < s.len() {
            let c = s[off];
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => break,
            };
            acc = (acc << 4) + u32::from(d);
            if off > 3 {
                return Err(addr_err(input, "each group must have 4 or less digits", &at(s)));
            }
            if acc > 0xFFFF {
                return Err(addr_err(input, "IPv6 field has value >=2^16", &at(s)));
            }
            off += 1;
        }
        if off == 0 {
            return Err(addr_err(input, "each colon-separated field must have at least one digit", &at(s)));
        }
        if off < s.len() && s[off] == b'.' {
            if ellipsis < 0 && i != 12 {
                return Err(addr_err(input, "embedded IPv4 address must replace the final 2 fields of the address", &at(s)));
            }
            if i + 4 > 16 {
                return Err(addr_err(
                    input,
                    "too many hex fields to fit an embedded IPv4 at the end of the address",
                    &at(s),
                ));
            }
            // Errors quote the whole input, as Go does.
            let mut fields = Vec::new();
            fields.extend_from_slice(s);
            parse_ipv4_fields(input, &fields)?;
            s = &[];
            i += 4;
            break;
        }
        i += 2;
        s = &s[off..];
        if s.is_empty() {
            break;
        }
        if s[0] != b':' {
            return Err(addr_err(input, "unexpected character, want colon", &at(s)));
        } else if s.len() == 1 {
            return Err(addr_err(input, "colon must be followed by more characters", &at(s)));
        }
        s = &s[1..];
        if s[0] == b':' {
            if ellipsis >= 0 {
                return Err(addr_err(input, "multiple :: in address", &at(s)));
            }
            ellipsis = i as i32;
            s = &s[1..];
            if s.is_empty() {
                break;
            }
        }
    }
    if !s.is_empty() {
        return Err(addr_err(input, "trailing garbage after address", &at(s)));
    }
    if i < 16 {
        if ellipsis < 0 {
            return Err(addr_err(input, "address string too short", ""));
        }
    } else if ellipsis >= 0 {
        return Err(addr_err(input, "the :: must expand to at least one field of zeros", ""));
    }
    Ok(zone)
}

fn ipv6(s: &str) -> R {
    if !s.contains(':') {
        return err("missing colon");
    }
    let (zone, _) = parse_addr(s)?;
    if !zone.is_empty() {
        return err("zone id is not a part of ipv6 address");
    }
    Ok(())
}

fn hostname(s: &str) -> R {
    let s = s.strip_suffix('.').unwrap_or(s);
    if s.len() > 253 {
        return err("more than 253 characters long");
    }
    for label in s.split('.') {
        if label.is_empty() || label.len() > 63 {
            return err("label must be 1 to 63 characters long");
        }
        if label.starts_with('-') {
            return err("label starts with hyphen");
        }
        if label.ends_with('-') {
            return err("label ends with hyphen");
        }
        for ch in label.chars() {
            if !(ch.is_ascii_alphanumeric() || ch == '-') {
                return err(alloc::format!("invalid character {}", qr(ch)));
            }
        }
    }
    Ok(())
}

fn email(s: &str) -> R {
    if s.len() > 254 {
        return err("more than 255 characters long");
    }
    let Some(at) = s.rfind('@') else { return err("missing @") };
    let (local, domain) = (&s[..at], &s[at + 1..]);
    if local.len() > 64 {
        return err("local part more than 64 characters long");
    }
    if local.len() > 1 && local.starts_with('"') && local.ends_with('"') {
        let inner = &local[1..local.len() - 1];
        if inner.contains('\\') || inner.contains('"') {
            return err("backslash and quote are not allowed within quoted local part");
        }
    } else {
        if local.starts_with('.') {
            return err("starts with dot");
        }
        if local.ends_with('.') {
            return err("ends with dot");
        }
        if local.contains("..") {
            return err("consecutive dots");
        }
        for ch in local.chars() {
            if !(ch.is_ascii_alphanumeric() || ".!#$%&'*+-/=?^_`{|}~".contains(ch)) {
                return err(alloc::format!("invalid character {}", qr(ch)));
            }
        }
    }
    if domain.len() >= 2 && domain.starts_with('[') && domain.ends_with(']') {
        let d = &domain[1..domain.len() - 1];
        if let Some(rem) = d.strip_prefix("IPv6:") {
            return ipv6(rem).map_err(|e| alloc::format!("invalid ipv6 address: {e}"));
        }
        return ipv4(d).map_err(|e| alloc::format!("invalid ipv4 address: {e}"));
    }
    hostname(domain).map_err(|e| alloc::format!("invalid domain: {e}"))
}

/// Go `time` package's `quote`.
fn tquote(s: &[u8]) -> String {
    let mut out = String::from("\"");
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c >= 0x80 || c < b' ' {
            let width = if c < 0x80 {
                1
            } else {
                match super::gostr::decode_rune(&s[i..]) {
                    (Some(ch), w) if ch != '\u{FFFD}' => w,
                    (_, _) if s[i..].starts_with("\u{FFFD}".as_bytes()) => 3,
                    _ => 1,
                }
            };
            for j in 0..width.min(s.len() - i) {
                let _ = write!(out, "\\x{:02x}", s[i + j]);
            }
            i += width;
        } else {
            if c == b'"' || c == b'\\' {
                out.push('\\');
            }
            out.push(c as char);
            i += 1;
        }
    }
    out.push('"');
    out
}

fn days_in(month: u32, year: i64) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `time.Parse("2006-01-02", s)`.
fn date(s: &[u8]) -> R {
    let layout = "2006-01-02";
    let cannot = |value: &[u8], elem: &str| {
        Err(alloc::format!(
            "parsing time {} as {}: cannot parse {} as {}",
            tquote(s),
            tquote(layout.as_bytes()),
            tquote(value),
            tquote(elem.as_bytes())
        ))
    };
    let digit = |v: &[u8], i: usize| v.get(i).is_some_and(u8::is_ascii_digit);
    let mut v = s;
    // 2006
    if v.len() < 4 || !digit(v, 0) {
        return cannot(v, "2006");
    }
    if !v[..4].iter().all(u8::is_ascii_digit) {
        return cannot(v, "2006");
    }
    let year: i64 = core::str::from_utf8(&v[..4]).ok().and_then(|y| y.parse().ok()).unwrap_or(0);
    v = &v[4..];
    let num2 = |v: &[u8]| -> Option<(u32, usize)> {
        if !digit(v, 0) || !digit(v, 1) {
            return None;
        }
        Some((u32::from(v[0] - b'0') * 10 + u32::from(v[1] - b'0'), 2))
    };
    // -01
    if v.first() != Some(&b'-') {
        return cannot(v, "-");
    }
    v = &v[1..];
    let Some((month, n)) = num2(v) else { return cannot(v, "01") };
    v = &v[n..];
    if !(1..=12).contains(&month) {
        return Err(alloc::format!("parsing time {}: month out of range", tquote(s)));
    }
    // -02
    if v.first() != Some(&b'-') {
        return cannot(v, "-");
    }
    v = &v[1..];
    let Some((day, n)) = num2(v) else { return cannot(v, "02") };
    v = &v[n..];
    if !v.is_empty() {
        return Err(alloc::format!("parsing time {}: extra text: {}", tquote(s), tquote(v)));
    }
    if day < 1 || day > days_in(month, year) {
        return Err(alloc::format!("parsing time {}: day out of range", tquote(s)));
    }
    Ok(())
}

fn atoi_bytes(b: &[u8]) -> Option<i64> {
    let s = core::str::from_utf8(b).ok()?;
    atoi(s).ok()
}

fn time(s: &[u8]) -> R {
    if s.len() < 9 {
        return err("less than 9 characters long");
    }
    if s[2] != b':' || s[5] != b':' {
        return err("missing colon in correct place");
    }
    let mut hms = Vec::new();
    for tok in s[..8].splitn(3, |c| *c == b':') {
        let Some(i) = atoi_bytes(tok) else { return err("invalid hour/min/sec") };
        if i < 0 {
            return err("non-positive hour/min/sec");
        }
        hms.push(i);
    }
    if hms.len() != 3 {
        return err("missing hour/min/sec");
    }
    let (mut h, mut m, sec) = (hms[0], hms[1], hms[2]);
    if h > 23 || m > 59 || sec > 60 {
        return err("hour/min/sec out of range");
    }
    let mut str = &s[8..];
    if let Some(rem) = str.strip_prefix(b".") {
        let n = rem.iter().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return err("no digits in second fraction");
        }
        str = &rem[n..];
    }
    if str != b"z" && str != b"Z" {
        if str.len() != 6 {
            return err("offset must be 6 characters long");
        }
        let sign: i64 = match str[0] {
            b'+' => -1,
            b'-' => 1,
            _ => return err("offset must begin with plus/minus"),
        };
        let str = &str[1..];
        if str[2] != b':' {
            return err("missing colon in offset in correct place");
        }
        let mut zhm = Vec::new();
        for tok in str.splitn(2, |c| *c == b':') {
            let Some(i) = atoi_bytes(tok) else { return err("invalid hour/min in offset") };
            if i < 0 {
                return err("non-positive hour/min in offset");
            }
            zhm.push(i);
        }
        let (zh, zm) = (zhm[0], zhm[1]);
        if zh > 23 || zm > 59 {
            return err("hour/min in offset out of range");
        }
        let mut hm = (h * 60 + m) + sign * (zh * 60 + zm);
        if hm < 0 {
            hm += 24 * 60;
        }
        h = hm / 60;
        m = hm % 60;
    }
    if sec >= 60 && (h != 23 || m != 59) {
        return err("invalid leap second");
    }
    Ok(())
}

fn date_time(s: &[u8]) -> R {
    if s.len() < 20 {
        return err("less than 20 characters long");
    }
    if s[10] != b't' && s[10] != b'T' {
        return err("11th character must be t or T");
    }
    date(&s[..10]).map_err(|e| alloc::format!("invalid date element: {e}"))?;
    time(&s[11..]).map_err(|e| alloc::format!("invalid time element: {e}"))
}

// ------------------------------------------------------------- url --

fn ishex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Host,
    Zone,
    Path,
    Fragment,
    User,
}

fn should_escape_host(c: u8) -> bool {
    if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
        return false;
    }
    !matches!(c, b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':' | b'[' | b']' | b'<' | b'>' | b'"')
}

/// `url.unescape` error checks only.
fn unescape(s: &[u8], mode: Mode) -> Result<(), String> {
    let mut i = 0;
    let mut n = 0;
    while i < s.len() {
        match s[i] {
            b'%' => {
                n += 1;
                if i + 2 >= s.len() || !ishex(s[i + 1]) || !ishex(s[i + 2]) {
                    let e = &s[i..s.len().min(i + 3)];
                    return Err(alloc::format!("invalid URL escape {}", quote_bytes(e)));
                }
                let hi = (s[i + 1] as char).to_digit(16).unwrap_or(0);
                if mode == Mode::Host && hi < 8 && &s[i..i + 3] != b"%25" {
                    return Err(alloc::format!("invalid URL escape {}", quote_bytes(&s[i..i + 3])));
                }
                i += 3;
            }
            c => {
                if (mode == Mode::Host || mode == Mode::Zone) && c < 0x80 && should_escape_host(c) {
                    return Err(alloc::format!("invalid character {} in host name", quote_bytes(&s[i..i + 1])));
                }
                i += 1;
            }
        }
    }
    let _ = n;
    Ok(())
}

fn valid_optional_port(p: &[u8]) -> bool {
    p.is_empty() || (p[0] == b':' && p[1..].iter().all(u8::is_ascii_digit))
}

fn parse_host(scheme: &str, host: &[u8]) -> Result<String, String> {
    let last_open = host.iter().rposition(|c| *c == b'[');
    match last_open {
        Some(i) if i > 0 => return Err("invalid IP-literal".into()),
        Some(_) => {
            let Some(close) = host.iter().rposition(|c| *c == b']') else {
                return Err("missing ']' in host".into());
            };
            let colon_port = &host[close + 1..];
            if !valid_optional_port(colon_port) {
                return Err(alloc::format!("invalid port {} after host", quote_bytes(colon_port)));
            }
            let hostname = &host[1..close.max(1)];
            let zi = hostname.windows(3).position(|w| w == b"%25");
            match zi {
                Some(z) => {
                    unescape(&hostname[..z], Mode::Host)?;
                    unescape(&hostname[z..], Mode::Zone)?;
                }
                None => unescape(hostname, Mode::Host)?,
            }
            let h = percent_decode(hostname);
            match parse_addr(&h) {
                Err(e) => return Err(alloc::format!("invalid host: {e}")),
                Ok((_, true)) => return Err("invalid IP-literal".into()),
                Ok(_) => {}
            }
            return Ok(alloc::format!("[{h}]{}", percent_decode(colon_port)));
        }
        None => {
            if let Some(mut i) = host.iter().position(|c| *c == b':') {
                let last = host.iter().rposition(|c| *c == b':').unwrap_or(i);
                if last != i && scheme != "http" && scheme != "https" {
                    i = last;
                }
                let colon_port = &host[i..];
                if !valid_optional_port(colon_port) {
                    return Err(alloc::format!("invalid port {} after host", quote_bytes(colon_port)));
                }
            }
        }
    }
    unescape(host, Mode::Host)?;
    Ok(percent_decode(host))
}

fn percent_decode(s: &[u8]) -> String {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() && ishex(s[i + 1]) && ishex(s[i + 2]) {
            let v = (s[i + 1] as char).to_digit(16).unwrap_or(0) * 16 + (s[i + 2] as char).to_digit(16).unwrap_or(0);
            out.push(v as u8);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Url {
    scheme: String,
    host: String,
    raw_path: String,
}

/// `url.Parse` (validation subset), errors as `parse "<url>": <msg>`.
fn url_parse(raw: &str) -> Result<Url, String> {
    let (u, frag) = match raw.find('#') {
        Some(i) => (&raw[..i], Some(&raw[i + 1..])),
        None => (raw, None),
    };
    let perr = |url: &str, e: String| alloc::format!("parse {}: {e}", quote(url));
    let url = url_parse_inner(u).map_err(|e| perr(u, e))?;
    if let Some(f) = frag {
        unescape(f.as_bytes(), Mode::Fragment).map_err(|e| perr(raw, e))?;
    }
    Ok(url)
}

fn url_parse_inner(raw: &str) -> Result<Url, String> {
    if raw.bytes().any(|c| c < 0x20 || c == 0x7f) {
        return Err("net/url: invalid control character in URL".into());
    }
    let mut url = Url { scheme: String::new(), host: String::new(), raw_path: String::new() };
    if raw == "*" {
        return Ok(url);
    }
    // getScheme
    let mut rest = raw;
    for (i, c) in raw.bytes().enumerate() {
        match c {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'0'..=b'9' | b'+' | b'-' | b'.' => {
                if i == 0 {
                    break;
                }
            }
            b':' => {
                if i == 0 {
                    return Err("missing protocol scheme".into());
                }
                url.scheme = raw[..i].to_ascii_lowercase();
                rest = &raw[i + 1..];
                break;
            }
            _ => break,
        }
    }
    if rest.ends_with('?') && rest.matches('?').count() == 1 {
        rest = &rest[..rest.len() - 1];
    } else if let Some(i) = rest.find('?') {
        rest = &rest[..i];
    }
    if !rest.starts_with('/') {
        if !url.scheme.is_empty() {
            return Ok(url);
        }
        let seg = rest.split('/').next().unwrap_or("");
        if seg.contains(':') {
            return Err("first path segment in URL cannot contain colon".into());
        }
    }
    if (!url.scheme.is_empty() || !rest.starts_with("///")) && rest.starts_with("//") {
        let mut authority = &rest[2..];
        rest = "";
        if let Some(i) = authority.find('/') {
            rest = &authority[i..];
            authority = &authority[..i];
        }
        let ab = authority.as_bytes();
        let at = ab.iter().rposition(|c| *c == b'@');
        let host = match at {
            None => ab,
            Some(i) => &ab[i + 1..],
        };
        url.host = parse_host(&url.scheme, host)?;
        if let Some(i) = at {
            let userinfo = &ab[..i];
            let valid = userinfo.iter().all(|c| {
                c.is_ascii_alphanumeric() || b"-._:~!$&'()*+,;=%@".contains(c)
            });
            if !valid {
                return Err("net/url: invalid userinfo".into());
            }
            unescape(userinfo, Mode::User)?;
        }
    }
    unescape(rest.as_bytes(), Mode::Path)?;
    url.raw_path = rest.to_string();
    Ok(url)
}

/// santhosh's `parseURL`: also checks bracketed IPv6 hosts.
fn parse_url(s: &str) -> Result<Url, String> {
    let u = url_parse(s)?;
    // u.Hostname(): host without port and brackets.
    let h = &u.host;
    let hostname = if let Some(rest) = h.strip_prefix('[') {
        rest.find(']').map_or(rest, |i| &rest[..i]).to_string()
    } else {
        match h.rfind(':') {
            Some(i) if h[i + 1..].bytes().all(|c| c.is_ascii_digit()) => h[..i].to_string(),
            _ => h.clone(),
        }
    };
    if hostname.contains(':') {
        if !u.host.contains('[') || !u.host.contains(']') {
            return Err("ipv6 address not enclosed in brackets".into());
        }
        ipv6(&hostname).map_err(|e| alloc::format!("invalid ipv6 address: {e}"))?;
    }
    Ok(u)
}

fn uri(s: &str) -> R {
    let u = parse_url(s)?;
    if u.scheme.is_empty() {
        return err("relative url");
    }
    Ok(())
}

fn uri_reference(s: &str) -> R {
    if s.contains('\\') {
        return err("contains \\");
    }
    parse_url(s).map(|_| ())
}

fn uri_template(s: &str) -> R {
    let u = parse_url(s)?;
    for tok in u.raw_path.split('/') {
        let tok = percent_decode(tok.as_bytes());
        let mut want = true;
        for ch in tok.chars() {
            let got = match ch {
                '{' => true,
                '}' => false,
                _ => continue,
            };
            if got != want {
                return err("nested curly braces");
            }
            want = !want;
        }
        if !want {
            return err("no matching closing brace");
        }
    }
    Ok(())
}

fn semver(s: &str) -> R {
    let mut s = s;
    if let Some(i) = s.find('+') {
        let build = &s[i + 1..];
        if build.is_empty() {
            return err("build is empty");
        }
        for id in build.split('.') {
            if id.is_empty() {
                return err("build identifier is empty");
            }
            for ch in id.chars() {
                if !(ch.is_ascii_alphanumeric() || ch == '-') {
                    return err(alloc::format!("invalid character {} in build identifier", qr(ch)));
                }
            }
        }
        s = &s[..i];
    }
    if let Some(i) = s.find('-') {
        for id in s[i + 1..].split('.') {
            if id.is_empty() {
                return err("pre-release identifier is empty");
            }
            let mut all_digits = true;
            for ch in id.chars() {
                if ch.is_ascii_digit() {
                } else if ch.is_ascii_alphabetic() || ch == '-' {
                    all_digits = false;
                } else {
                    return err(alloc::format!("invalid character {} in pre-release identifier", qr(ch)));
                }
            }
            if all_digits && id.len() > 1 && id.starts_with('0') {
                return err("pre-release numeric identifier starts with zero");
            }
        }
        s = &s[..i];
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return err("versionCore must have 3 numbers separated by dot");
    }
    for (i, v) in parts.iter().enumerate() {
        let name = ["major", "minor", "patch"][i];
        if v.is_empty() {
            return err(alloc::format!("{name} is empty"));
        }
        if v.len() > 1 && v.starts_with('0') {
            return err(alloc::format!("{name} starts with zero"));
        }
        if !v.bytes().all(|c| c.is_ascii_digit()) {
            return err(alloc::format!("{name} contains non-digit"));
        }
    }
    Ok(())
}
