//! Key and certificate generators (`genPrivateKey`, `genCA`,
//! `genSelfSignedCert`, `genSignedCert`, the `*WithKey` variants and
//! `buildCustomCert`), mirroring Sprig + Go's `crypto/x509`.
//!
//! Supported keys: RSA, ECDSA P-256/P-384 and Ed25519 (generation and PEM
//! parsing). Not supported (clear errors): DSA key generation and DSA/other
//! curve keys for signing. Requires the `certs` cargo feature.

#![cfg_attr(not(feature = "certs"), allow(dead_code, unused_imports))]

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::func::{FuncError, FuncMap, Sig, Ty};
use crate::value::{Kind, Object, Value};

/// Sprig's `certificate` struct (`.Cert`, `.Key`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    pub cert: String,
    pub key: String,
}

pub const CERT_TYPE: &str = "sprig.certificate";

impl Object for Certificate {
    fn type_name(&self) -> &str {
        CERT_TYPE
    }
    fn kind(&self) -> Kind {
        Kind::Struct
    }
    fn field(&self, name: &str) -> Option<Value> {
        match name {
            "Cert" => Some(Value::str(&self.cert)),
            "Key" => Some(Value::str(&self.key)),
            _ => None,
        }
    }
    fn format(&self, out: &mut String) {
        out.push('{');
        out.push_str(&self.cert);
        out.push(' ');
        out.push_str(&self.key);
        out.push('}');
    }
    fn to_data(&self) -> Value {
        crate::map! { "Cert" => self.cert.as_str(), "Key" => self.key.as_str() }
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

pub(super) fn register(f: &mut FuncMap) {
    const S: Ty = Ty::String;
    const I: Ty = Ty::Int;
    const L: Ty = Ty::List;
    const C: Ty = Ty::Object(CERT_TYPE);
    imp::register(f, S, I, L, C);
}

// ---------------------------------------------------------------------------
// PEM (encoding/pem)
// ---------------------------------------------------------------------------

/// `pem.EncodeToMemory`.
pub(crate) fn pem_encode(ty: &str, der: &[u8]) -> String {
    let b64 = super::strings::base64_encode(der);
    let mut out = format!("-----BEGIN {ty}-----\n");
    let bytes = b64.as_bytes();
    for chunk in bytes.chunks(64) {
        out.push_str(core::str::from_utf8(chunk).unwrap_or(""));
        out.push('\n');
    }
    out.push_str(&format!("-----END {ty}-----\n"));
    out
}

/// A decoded PEM block.
pub(crate) struct PemBlock {
    pub ty: String,
    pub bytes: Vec<u8>,
}

/// `pem.Decode` (first block only; headers are skipped).
pub(crate) fn pem_decode(data: &[u8]) -> Option<PemBlock> {
    let mut rest = data;
    loop {
        // Find "-----BEGIN " at the start of a line.
        let pos = find(rest, b"-----BEGIN ")?;
        if pos != 0 && rest[pos - 1] != b'\n' {
            rest = &rest[pos + 11..];
            continue;
        }
        let after = &rest[pos + 11..];
        let line_end = after.iter().position(|&c| c == b'\n').unwrap_or(after.len());
        let mut type_line = &after[..line_end];
        while type_line.last().is_some_and(|c| *c == b'\r' || *c == b' ' || *c == b'\t') {
            type_line = &type_line[..type_line.len() - 1];
        }
        let Some(ty) = type_line.strip_suffix(b"-----") else {
            rest = after;
            continue;
        };
        let ty = <String as LossyOwned>::from_utf8_lossy_owned(ty);
        let body_start = (line_end + 1).min(after.len());
        let body = &after[body_start..];
        let end_marker = format!("-----END {ty}-----");
        let Some(end) = find(body, end_marker.as_bytes()) else {
            rest = after;
            continue;
        };
        let mut content = &body[..end];
        // Skip RFC 1421 headers (lines with ':') before a blank line.
        if let Some(blank) = find(content, b"\n\n") {
            if content[..blank].contains(&b':') {
                content = &content[blank + 2..];
            }
        }
        let b64: Vec<u8> = content.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect();
        match super::strings::base64_decode(&b64) {
            Ok(bytes) => return Some(PemBlock { ty, bytes }),
            Err(_) => {
                rest = after;
                continue;
            }
        }
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| &h[i..i + n.len()] == n)
}

trait LossyOwned {
    fn from_utf8_lossy_owned(b: &[u8]) -> String;
}
impl LossyOwned for String {
    fn from_utf8_lossy_owned(b: &[u8]) -> String {
        crate::gofmt::strconv::lossy(b)
    }
}

// ---------------------------------------------------------------------------
// DER
// ---------------------------------------------------------------------------

pub(crate) mod der {
    use alloc::vec::Vec;

    pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = alloc::vec![tag];
        let n = content.len();
        if n < 0x80 {
            out.push(n as u8);
        } else {
            let bytes = (n as u64).to_be_bytes();
            let skip = bytes.iter().position(|&b| b != 0).unwrap_or(7);
            out.push(0x80 | (8 - skip) as u8);
            out.extend_from_slice(&bytes[skip..]);
        }
        out.extend_from_slice(content);
        out
    }
    pub fn seq(parts: &[&[u8]]) -> Vec<u8> {
        tlv(0x30, &parts.concat())
    }
    pub fn set(parts: &[&[u8]]) -> Vec<u8> {
        tlv(0x31, &parts.concat())
    }
    /// INTEGER from big-endian unsigned magnitude.
    pub fn uint(be: &[u8]) -> Vec<u8> {
        let mut b = be;
        while b.len() > 1 && b[0] == 0 {
            b = &b[1..];
        }
        let mut c = Vec::new();
        if b.is_empty() {
            c.push(0);
        } else {
            if b[0] & 0x80 != 0 {
                c.push(0);
            }
            c.extend_from_slice(b);
        }
        tlv(0x02, &c)
    }
    pub fn small_int(v: u8) -> Vec<u8> {
        uint(&[v])
    }
    pub fn oid(arcs: &[u64]) -> Vec<u8> {
        let mut c = Vec::new();
        let first = arcs[0] * 40 + arcs[1];
        let mut push = |mut v: u64| {
            let mut tmp = alloc::vec![(v & 0x7f) as u8];
            v >>= 7;
            while v > 0 {
                tmp.push(0x80 | (v & 0x7f) as u8);
                v >>= 7;
            }
            tmp.reverse();
            c.extend_from_slice(&tmp);
        };
        push(first);
        for &a in &arcs[2..] {
            push(a);
        }
        tlv(0x06, &c)
    }
    pub fn bitstring(b: &[u8]) -> Vec<u8> {
        let mut c = alloc::vec![0u8];
        c.extend_from_slice(b);
        tlv(0x03, &c)
    }
    pub fn octets(b: &[u8]) -> Vec<u8> {
        tlv(0x04, b)
    }
    pub fn null() -> Vec<u8> {
        alloc::vec![0x05, 0x00]
    }
    pub fn boolean(v: bool) -> Vec<u8> {
        alloc::vec![0x01, 0x01, if v { 0xff } else { 0 }]
    }

    /// A parsed TLV: (tag, content, full encoding).
    pub struct Tlv<'a> {
        pub tag: u8,
        pub content: &'a [u8],
        pub full: &'a [u8],
    }

    /// Reads one TLV from `b`; returns it and the rest.
    pub fn read(b: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
        if b.len() < 2 {
            return None;
        }
        let tag = b[0];
        if tag & 0x1f == 0x1f {
            return None;
        }
        let (len, hdr) = if b[1] < 0x80 {
            (b[1] as usize, 2)
        } else {
            let n = (b[1] & 0x7f) as usize;
            if n == 0 || n > 4 || b.len() < 2 + n {
                return None;
            }
            let mut l = 0usize;
            for &x in &b[2..2 + n] {
                l = (l << 8) | x as usize;
            }
            if l < 0x80 || (n > 1 && b[2] == 0) {
                return None; // non-minimal length
            }
            (l, 2 + n)
        };
        if b.len() < hdr + len {
            return None;
        }
        Some((Tlv { tag, content: &b[hdr..hdr + len], full: &b[..hdr + len] }, &b[hdr + len..]))
    }

    /// Reads all TLVs in a constructed content.
    pub fn read_all(mut b: &[u8]) -> Option<Vec<Tlv<'_>>> {
        let mut out = Vec::new();
        while !b.is_empty() {
            let (t, rest) = read(b)?;
            out.push(t);
            b = rest;
        }
        Some(out)
    }

}

const OID_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];
const OID_SHA256_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 11];
const OID_EC_PUBLIC_KEY: &[u64] = &[1, 2, 840, 10045, 2, 1];
const OID_P256: &[u64] = &[1, 2, 840, 10045, 3, 1, 7];
const OID_P384: &[u64] = &[1, 3, 132, 0, 34];
const OID_ECDSA_SHA256: &[u64] = &[1, 2, 840, 10045, 4, 3, 2];
const OID_ECDSA_SHA384: &[u64] = &[1, 2, 840, 10045, 4, 3, 3];
const OID_ED25519: &[u64] = &[1, 3, 101, 112];
const OID_DSA: &[u64] = &[1, 2, 840, 10040, 4, 1];

#[cfg(not(feature = "certs"))]
mod imp {
    use super::*;

    fn unsupported(_: &[Value]) -> crate::func::FuncResult {
        Err(FuncError::new("key and certificate generation requires the `certs` feature of helm-rs"))
    }

    pub(super) fn register(f: &mut FuncMap, s: Ty, i: Ty, l: Ty, c: Ty) {
        let _ = (OID_RSA, OID_SHA256_RSA, OID_EC_PUBLIC_KEY, OID_P256, OID_P384, OID_ECDSA_SHA256, OID_ECDSA_SHA384, OID_ED25519, OID_DSA);
        let sig = |p: &'static [Ty]| Sig::new(p);
        let _ = (s, i, l, c, sig);
        const S: Ty = Ty::String;
        const I: Ty = Ty::Int;
        const L: Ty = Ty::List;
        const C: Ty = Ty::Object(CERT_TYPE);
        f.add("genPrivateKey", Sig::new(&[S]), unsupported);
        f.add("buildCustomCert", Sig::new(&[S, S]), unsupported);
        f.add("genCA", Sig::new(&[S, I]), unsupported);
        f.add("genCAWithKey", Sig::new(&[S, I, S]), unsupported);
        f.add("genSelfSignedCert", Sig::new(&[S, L, L, I]), unsupported);
        f.add("genSelfSignedCertWithKey", Sig::new(&[S, L, L, I, S]), unsupported);
        f.add("genSignedCert", Sig::new(&[S, L, L, I, C]), unsupported);
        f.add("genSignedCertWithKey", Sig::new(&[S, L, L, I, C, S]), unsupported);
    }
}

#[cfg(feature = "certs")]
mod imp {
    use super::der;
    use super::*;
    use crate::func::{CallCtx, Env, FuncResult};
    use crate::funcs::sprig::{arg, iarg, sarg};
    use sha2::Digest as _;

    /// Adapts the template [`Env`] to `rand_core`.
    struct EnvRng<'a>(&'a mut dyn Env);

    impl rand_core::RngCore for EnvRng<'_> {
        fn next_u32(&mut self) -> u32 {
            let mut b = [0u8; 4];
            self.0.fill_random(&mut b);
            u32::from_le_bytes(b)
        }
        fn next_u64(&mut self) -> u64 {
            let mut b = [0u8; 8];
            self.0.fill_random(&mut b);
            u64::from_le_bytes(b)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            self.0.fill_random(dest);
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.0.fill_random(dest);
            Ok(())
        }
    }
    impl rand_core::CryptoRng for EnvRng<'_> {}

    /// A private key usable for certificates.
    #[derive(Clone)]
    enum Key {
        Rsa(rsa::RsaPrivateKey),
        P256(p256::SecretKey),
        P384(p384::SecretKey),
        Ed25519(ed25519_dalek::SigningKey),
        /// DSA keys parse but cannot sign (Go: not a crypto.Signer). Holds
        /// the DSA private key DER.
        Dsa(Vec<u8>),
    }

    impl Key {
        /// (SPKI DER, raw public key bytes).
        fn spki(&self) -> (Vec<u8>, Vec<u8>) {
            match self {
                Key::Rsa(k) => {
                    use rsa::traits::PublicKeyParts;
                    let n = k.n().to_bytes_be();
                    let e = k.e().to_bytes_be();
                    let pk = der::seq(&[&der::uint(&n), &der::uint(&e)]);
                    let alg = der::seq(&[&der::oid(OID_RSA), &der::null()]);
                    (der::seq(&[&alg, &der::bitstring(&pk)]), pk)
                }
                Key::P256(k) => {
                    use p256::elliptic_curve::sec1::ToEncodedPoint;
                    let pt = k.public_key().to_encoded_point(false).as_bytes().to_vec();
                    let alg = der::seq(&[&der::oid(OID_EC_PUBLIC_KEY), &der::oid(OID_P256)]);
                    (der::seq(&[&alg, &der::bitstring(&pt)]), pt)
                }
                Key::P384(k) => {
                    use p384::elliptic_curve::sec1::ToEncodedPoint;
                    let pt = k.public_key().to_encoded_point(false).as_bytes().to_vec();
                    let alg = der::seq(&[&der::oid(OID_EC_PUBLIC_KEY), &der::oid(OID_P384)]);
                    (der::seq(&[&alg, &der::bitstring(&pt)]), pt)
                }
                Key::Ed25519(k) => {
                    let pt = k.verifying_key().to_bytes().to_vec();
                    let alg = der::seq(&[&der::oid(OID_ED25519)]);
                    (der::seq(&[&alg, &der::bitstring(&pt)]), pt)
                }
                Key::Dsa(_) => (Vec::new(), Vec::new()),
            }
        }

        /// `pemBlockForKey`.
        fn pem(&self) -> String {
            match self {
                Key::Rsa(k) => {
                    use rsa::traits::{PrivateKeyParts, PublicKeyParts};
                    let primes = k.primes();
                    let (p, q) = (&primes[0], &primes[1]);
                    let d = k.d();
                    let one = rsa::BigUint::from(1u32);
                    let dp = d % (p - &one);
                    let dq = d % (q - &one);
                    let qinv = k.crt_coefficient().unwrap_or_else(|| one.clone());
                    let body = der::seq(&[
                        &der::small_int(0),
                        &der::uint(&k.n().to_bytes_be()),
                        &der::uint(&k.e().to_bytes_be()),
                        &der::uint(&d.to_bytes_be()),
                        &der::uint(&p.to_bytes_be()),
                        &der::uint(&q.to_bytes_be()),
                        &der::uint(&dp.to_bytes_be()),
                        &der::uint(&dq.to_bytes_be()),
                        &der::uint(&qinv.to_bytes_be()),
                    ]);
                    pem_encode("RSA PRIVATE KEY", &body)
                }
                Key::P256(k) => {
                    let (_, pt) = self.spki();
                    let body = der::seq(&[
                        &der::small_int(1),
                        &der::octets(&k.to_bytes()),
                        &der::tlv(0xa0, &der::oid(OID_P256)),
                        &der::tlv(0xa1, &der::bitstring(&pt)),
                    ]);
                    pem_encode("EC PRIVATE KEY", &body)
                }
                Key::P384(k) => {
                    let (_, pt) = self.spki();
                    let body = der::seq(&[
                        &der::small_int(1),
                        &der::octets(&k.to_bytes()),
                        &der::tlv(0xa0, &der::oid(OID_P384)),
                        &der::tlv(0xa1, &der::bitstring(&pt)),
                    ]);
                    pem_encode("EC PRIVATE KEY", &body)
                }
                Key::Ed25519(k) => {
                    let body = der::seq(&[
                        &der::small_int(0),
                        &der::seq(&[&der::oid(OID_ED25519)]),
                        &der::octets(&der::octets(&k.to_bytes())),
                    ]);
                    pem_encode("PRIVATE KEY", &body)
                }
                Key::Dsa(d) => pem_encode("DSA PRIVATE KEY", d),
            }
        }

        /// The signature AlgorithmIdentifier for this signer.
        fn alg(&self) -> Vec<u8> {
            match self {
                Key::Rsa(_) => der::seq(&[&der::oid(OID_SHA256_RSA), &der::null()]),
                Key::P256(_) => der::seq(&[&der::oid(OID_ECDSA_SHA256)]),
                Key::P384(_) => der::seq(&[&der::oid(OID_ECDSA_SHA384)]),
                Key::Ed25519(_) | Key::Dsa(_) => der::seq(&[&der::oid(OID_ED25519)]),
            }
        }

        /// (signature AlgorithmIdentifier DER, signature).
        fn sign(&self, tbs: &[u8], rng: &mut EnvRng<'_>) -> Result<(Vec<u8>, Vec<u8>), String> {
            use rsa::signature::{RandomizedSigner, SignatureEncoding, Signer};
            match self {
                Key::Rsa(k) => {
                    let sk = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(k.clone());
                    let sig = sk.try_sign(tbs).map_err(|e| format!("{e}"))?.to_vec();
                    Ok((der::seq(&[&der::oid(OID_SHA256_RSA), &der::null()]), sig))
                }
                Key::P256(k) => {
                    let sk = p256::ecdsa::SigningKey::from(k);
                    let sig: p256::ecdsa::Signature = sk.try_sign_with_rng(rng, tbs).map_err(|e| format!("{e}"))?;
                    let (r, s) = sig.split_bytes();
                    Ok((der::seq(&[&der::oid(OID_ECDSA_SHA256)]), der::seq(&[&der::uint(&r), &der::uint(&s)])))
                }
                Key::P384(k) => {
                    let sk = p384::ecdsa::SigningKey::from(k);
                    let sig: p384::ecdsa::Signature = sk.try_sign_with_rng(rng, tbs).map_err(|e| format!("{e}"))?;
                    let (r, s) = sig.split_bytes();
                    Ok((der::seq(&[&der::oid(OID_ECDSA_SHA384)]), der::seq(&[&der::uint(&r), &der::uint(&s)])))
                }
                Key::Ed25519(k) => {
                    let sig = k.sign(tbs).to_bytes().to_vec();
                    Ok((der::seq(&[&der::oid(OID_ED25519)]), sig))
                }
                Key::Dsa(_) => Err("x509: certificate private key does not implement crypto.Signer".into()),
            }
        }
    }

    fn gen_rsa(ctx: &mut CallCtx<'_>, bits: usize) -> Result<Key, String> {
        let mut rng = EnvRng(ctx.env);
        rsa::RsaPrivateKey::new(&mut rng, bits).map(Key::Rsa).map_err(|e| format!("{e}"))
    }

    // ----- PEM key parsing (sprig's parsePrivateKeyPEM) -----

    fn parse_pkcs1(b: &[u8]) -> Result<Key, String> {
        use rsa::pkcs1::DecodeRsaPrivateKey;
        rsa::RsaPrivateKey::from_pkcs1_der(b)
            .map(Key::Rsa)
            .map_err(|_| String::from("x509: failed to parse private key (use ParsePKCS8PrivateKey instead for this key format)"))
    }

    fn ec_from_scalar(curve: &[u8], scalar: &[u8]) -> Result<Key, String> {
        if curve == der::oid(OID_P256).as_slice() {
            let mut buf = [0u8; 32];
            if scalar.len() > 32 {
                return Err("x509: invalid private key length".into());
            }
            buf[32 - scalar.len()..].copy_from_slice(scalar);
            return p256::SecretKey::from_slice(&buf).map(Key::P256).map_err(|_| "x509: invalid elliptic curve private key value".into());
        }
        if curve == der::oid(OID_P384).as_slice() {
            let mut buf = [0u8; 48];
            if scalar.len() > 48 {
                return Err("x509: invalid private key length".into());
            }
            buf[48 - scalar.len()..].copy_from_slice(scalar);
            return p384::SecretKey::from_slice(&buf).map(Key::P384).map_err(|_| "x509: invalid elliptic curve private key value".into());
        }
        Err("x509: unknown elliptic curve".into())
    }

    /// SEC1 `ECPrivateKey`; `named_curve` overrides a missing curve.
    fn parse_sec1(b: &[u8], named_curve: Option<&[u8]>) -> Result<Key, String> {
        let malformed = || String::from("x509: failed to parse EC private key: asn1: structure error");
        let (outer, _) = der::read(b).ok_or_else(malformed)?;
        if outer.tag != 0x30 {
            return Err(malformed());
        }
        let items = der::read_all(outer.content).ok_or_else(malformed)?;
        if items.len() < 2 || items[0].tag != 0x02 || items[1].tag != 0x04 {
            return Err(malformed());
        }
        if items[0].content != [1] {
            return Err("x509: unknown EC private key version".into());
        }
        let mut curve: Option<Vec<u8>> = named_curve.map(|c| c.to_vec());
        for it in &items[2..] {
            if it.tag == 0xa0 {
                curve = Some(it.content.to_vec());
            }
        }
        let curve = curve.ok_or_else(|| String::from("x509: unknown elliptic curve"))?;
        ec_from_scalar(&curve, items[1].content)
    }

    fn parse_pkcs8(b: &[u8]) -> Result<Key, String> {
        let malformed = || String::from("asn1: structure error: tags don't match");
        let (outer, _) = der::read(b).ok_or_else(malformed)?;
        if outer.tag != 0x30 {
            return Err(malformed());
        }
        let items = der::read_all(outer.content).ok_or_else(malformed)?;
        if items.len() < 3 || items[0].tag != 0x02 || items[1].tag != 0x30 || items[2].tag != 0x04 {
            return Err(malformed());
        }
        let alg = der::read_all(items[1].content).ok_or_else(malformed)?;
        let Some(oid) = alg.first() else { return Err(malformed()) };
        let key = items[2].content;
        if oid.full == der::oid(OID_RSA).as_slice() {
            return parse_pkcs1(key).map_err(|_| String::from("x509: failed to parse RSA private key embedded in PKCS#8"));
        }
        if oid.full == der::oid(OID_EC_PUBLIC_KEY).as_slice() {
            let curve = alg.get(1).map(|c| c.full);
            return parse_sec1(key, curve).map_err(|e| format!("x509: failed to parse EC private key embedded in PKCS#8: {e}"));
        }
        if oid.full == der::oid(OID_ED25519).as_slice() {
            let (inner, _) = der::read(key).ok_or_else(|| String::from("x509: invalid Ed25519 private key: asn1: syntax error"))?;
            if inner.tag != 0x04 || inner.content.len() != 32 {
                return Err(format!("x509: invalid Ed25519 private key length: {}", inner.content.len()));
            }
            let mut seed = [0u8; 32];
            seed.copy_from_slice(inner.content);
            return Ok(Key::Ed25519(ed25519_dalek::SigningKey::from_bytes(&seed)));
        }
        if oid.full == der::oid(OID_DSA).as_slice() {
            return Err("x509: PKCS#8 wrapping contained private key with unknown algorithm: 1.2.840.10040.4.1".into());
        }
        Err("x509: PKCS#8 wrapping contained private key with unknown algorithm".into())
    }

    fn parse_dsa(b: &[u8]) -> Result<Key, String> {
        let malformed = || String::from("asn1: structure error: tags don't match");
        let (outer, _) = der::read(b).ok_or_else(malformed)?;
        let items = der::read_all(outer.content).ok_or_else(malformed)?;
        if outer.tag != 0x30 || items.len() != 6 || items.iter().any(|i| i.tag != 0x02) {
            return Err(malformed());
        }
        Ok(Key::Dsa(b.to_vec()))
    }

    fn parse_private_key_pem(pem: &str) -> Result<Key, String> {
        let Some(block) = pem_decode(pem.as_bytes()) else { return Err("no PEM data in input".into()) };
        if block.ty == "PRIVATE KEY" {
            return parse_pkcs8(&block.bytes).map_err(|e| format!("decoding PEM as PKCS#8: {e}"));
        }
        let Some(prefix) = block.ty.strip_suffix(" PRIVATE KEY") else {
            return Err(format!("no private key data in PEM block of type {}", block.ty));
        };
        match prefix {
            "RSA" => parse_pkcs1(&block.bytes).map_err(|e| format!("parsing RSA private key from PEM: {e}")),
            "EC" => parse_sec1(&block.bytes, None).map_err(|e| format!("parsing EC private key from PEM: {e}")),
            "DSA" => parse_dsa(&block.bytes).map_err(|e| format!("parsing DSA private key from PEM: {e}")),
            _ => Err(format!("invalid private key type {}", block.ty)),
        }
    }

    // ----- Certificate parsing (just enough of x509.ParseCertificate) -----

    struct ParsedCert {
        raw_subject: Vec<u8>,
        spki: Vec<u8>,
        subject_key_id: Vec<u8>,
    }

    fn parse_certificate(b: &[u8]) -> Result<ParsedCert, String> {
        let malformed = || String::from("x509: malformed certificate");
        let (cert, rest) = der::read(b).ok_or_else(malformed)?;
        if cert.tag != 0x30 || !rest.is_empty() {
            return Err(malformed());
        }
        let parts = der::read_all(cert.content).ok_or_else(malformed)?;
        if parts.len() != 3 || parts[0].tag != 0x30 || parts[1].tag != 0x30 || parts[2].tag != 0x03 {
            return Err(malformed());
        }
        let tbs = der::read_all(parts[0].content).ok_or_else(|| String::from("x509: malformed tbs certificate"))?;
        let mut i = 0;
        if tbs.first().is_some_and(|t| t.tag == 0xa0) {
            i += 1;
        }
        // serial, sigalg, issuer, validity, subject, spki
        if tbs.len() < i + 6 {
            return Err("x509: malformed tbs certificate".into());
        }
        if tbs[i].tag != 0x02 {
            return Err("x509: malformed serial number".into());
        }
        if tbs[i + 1].tag != 0x30 {
            return Err("x509: malformed signature algorithm identifier".into());
        }
        if tbs[i + 2].tag != 0x30 {
            return Err("x509: malformed issuer".into());
        }
        if tbs[i + 3].tag != 0x30 {
            return Err("x509: malformed validity".into());
        }
        if tbs[i + 4].tag != 0x30 {
            return Err("x509: malformed subject".into());
        }
        if tbs[i + 5].tag != 0x30 {
            return Err("x509: malformed spki".into());
        }
        let mut ski = Vec::new();
        for t in &tbs[i + 6..] {
            if t.tag == 0xa3 {
                let (exts, _) = der::read(t.content).ok_or_else(|| String::from("x509: malformed extensions"))?;
                for ext in der::read_all(exts.content).ok_or_else(|| String::from("x509: malformed extension"))? {
                    let fields = der::read_all(ext.content).ok_or_else(|| String::from("x509: malformed extension"))?;
                    if fields.first().is_some_and(|o| o.full == der::oid(&[2, 5, 29, 14]).as_slice()) {
                        if let Some(v) = fields.last() {
                            if let Some((inner, _)) = der::read(v.content) {
                                ski = inner.content.to_vec();
                            }
                        }
                    }
                }
            }
        }
        Ok(ParsedCert { raw_subject: tbs[i + 4].full.to_vec(), spki: tbs[i + 5].full.to_vec(), subject_key_id: ski })
    }

    // ----- Certificate creation -----

    struct Template {
        serial: Vec<u8>,
        subject: Vec<u8>,
        ips: Vec<Vec<u8>>,
        dns: Vec<String>,
        not_before: i64,
        not_after: i64,
        key_usage: u16,
        is_ca: bool,
    }

    const KU_DIGITAL_SIGNATURE: u16 = 1;
    const KU_KEY_ENCIPHERMENT: u16 = 1 << 2;
    const KU_CERT_SIGN: u16 = 1 << 5;

    fn is_printable(c: u8) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, b' ' | b'\'' | b'(' | b')' | b'+' | b',' | b'-' | b'.' | b'/' | b':' | b'=' | b'?')
    }

    /// `pkix.Name{CommonName: cn}` as an RDNSequence.
    fn subject(cn: &str) -> Vec<u8> {
        if cn.is_empty() {
            return der::seq(&[]);
        }
        let tag = if cn.bytes().all(is_printable) { 0x13 } else { 0x0c };
        let atv = der::seq(&[&der::oid(&[2, 5, 4, 3]), &der::tlv(tag, cn.as_bytes())]);
        der::seq(&[&der::set(&[&atv])])
    }

    fn time_der(unix: i64) -> Vec<u8> {
        let t = crate::funcs::sprig::time::Time::from_unix(unix, crate::funcs::sprig::time::Location::Utc);
        let (y, _, _) = t.ymd();
        if (1950..2050).contains(&y) {
            der::tlv(0x17, t.format("060102150405Z").as_bytes())
        } else {
            der::tlv(0x18, t.format("20060102150405Z").as_bytes())
        }
    }

    /// `net.ParseIP` (returns 4 bytes for IPv4, 16 for IPv6).
    fn parse_ip(s: &str) -> Option<Vec<u8>> {
        fn v4(s: &str) -> Option<[u8; 4]> {
            let parts: Vec<&str> = s.split('.').collect();
            if parts.len() != 4 {
                return None;
            }
            let mut out = [0u8; 4];
            for (i, p) in parts.iter().enumerate() {
                if p.is_empty() || p.len() > 3 || !p.bytes().all(|c| c.is_ascii_digit()) || (p.len() > 1 && p.starts_with('0')) {
                    return None;
                }
                let v: u32 = p.parse().ok()?;
                if v > 255 {
                    return None;
                }
                out[i] = v as u8;
            }
            Some(out)
        }
        if let Some(a) = v4(s) {
            return Some(a.to_vec());
        }
        if !s.contains(':') || s.contains('%') {
            return None;
        }
        let groups = |part: &str| -> Option<Vec<u16>> {
            if part.is_empty() {
                return Some(Vec::new());
            }
            let mut out = Vec::new();
            let pieces: Vec<&str> = part.split(':').collect();
            for (k, p) in pieces.iter().enumerate() {
                if k == pieces.len() - 1 && p.contains('.') {
                    let a = v4(p)?;
                    out.push(u16::from_be_bytes([a[0], a[1]]));
                    out.push(u16::from_be_bytes([a[2], a[3]]));
                } else {
                    if p.is_empty() || p.len() > 4 || !p.bytes().all(|c| c.is_ascii_hexdigit()) {
                        return None;
                    }
                    out.push(u16::from_str_radix(p, 16).ok()?);
                }
            }
            Some(out)
        };
        let words: Vec<u16> = match s.split_once("::") {
            Some((a, b)) => {
                if b.contains("::") {
                    return None;
                }
                let (ga, gb) = (groups(a)?, groups(b)?);
                if ga.len() + gb.len() > 7 {
                    return None;
                }
                let mut w = ga;
                w.extend(core::iter::repeat_n(0, 8 - w.len() - gb.len()));
                w.extend(gb);
                w
            }
            None => {
                let g = groups(s)?;
                if g.len() != 8 {
                    return None;
                }
                g
            }
        };
        let mut out = Vec::with_capacity(16);
        for w in words {
            out.extend_from_slice(&w.to_be_bytes());
        }
        // IPv4-mapped addresses are encoded as 4 bytes (To4).
        if out[..10].iter().all(|&b| b == 0) && out[10] == 0xff && out[11] == 0xff {
            return Some(out[12..].to_vec());
        }
        Some(out)
    }

    fn get_base_template(ctx: &mut CallCtx<'_>, cn: &str, ips: &Value, dns: &Value, days: i64) -> Result<Template, String> {
        let mut ip_bytes = Vec::new();
        if let Value::List(l) = ips {
            for ip in l.iter() {
                let Value::String(s) = ip else {
                    return Err(format!("error parsing ip: {} is not a string", crate::gofmt::format_value(ip)));
                };
                ip_bytes.push(parse_ip(s).ok_or_else(|| format!("error parsing ip: {s}"))?);
            }
        }
        let mut dns_names = Vec::new();
        if let Value::List(l) = dns {
            for d in l.iter() {
                let Value::String(s) = d else {
                    return Err(format!(
                        "error processing alternate dns name: {} is not a string",
                        crate::gofmt::format_value(d)
                    ));
                };
                dns_names.push(s.to_string());
            }
        }
        // serial: uniform in [0, 2^128)
        let mut serial = [0u8; 16];
        ctx.env.fill_random(&mut serial);
        let now = ctx.env.now().unix_seconds;
        let after = (now as i128 + (days as i128) * 86400).clamp(i64::MIN as i128 / 2, i64::MAX as i128 / 2) as i64;
        Ok(Template {
            serial: serial.to_vec(),
            subject: subject(cn),
            ips: ip_bytes,
            dns: dns_names,
            not_before: now,
            not_after: after,
            key_usage: KU_KEY_ENCIPHERMENT | KU_DIGITAL_SIGNATURE,
            is_ca: false,
        })
    }

    fn extension(oid: &[u64], critical: bool, value: &[u8]) -> Vec<u8> {
        if critical {
            der::seq(&[&der::oid(oid), &der::boolean(true), &der::octets(value)])
        } else {
            der::seq(&[&der::oid(oid), &der::octets(value)])
        }
    }

    /// `x509.CreateCertificate` + PEM encoding (`getCertAndKey`).
    fn cert_and_key(
        ctx: &mut CallCtx<'_>,
        t: &Template,
        signee: &Key,
        parent: Option<&ParsedCert>,
        signer: &Key,
    ) -> Result<Certificate, String> {
        let (spki, pub_bytes) = signee.spki();
        let issuer = match parent {
            Some(p) => p.raw_subject.clone(),
            None => t.subject.clone(),
        };
        let mut aki: Vec<u8> = Vec::new();
        if let Some(p) = parent {
            if issuer != t.subject && !p.subject_key_id.is_empty() {
                aki = p.subject_key_id.clone();
            }
        }
        let mut ski: Vec<u8> = Vec::new();
        if t.is_ca {
            ski = sha2::Sha256::digest(&pub_bytes)[..20].to_vec();
        }
        if matches!(signer, Key::Dsa(_)) {
            return Err("error creating certificate: x509: certificate private key does not implement crypto.Signer".into());
        }
        if let Some(p) = parent {
            let (signer_spki, _) = signer.spki();
            if signer_spki != p.spki {
                return Err("error creating certificate: x509: provided PrivateKey doesn't match parent's PublicKey".into());
            }
        }
        for d in &t.dns {
            if !d.is_ascii() {
                return Err(format!(
                    "error creating certificate: x509: {} cannot be encoded as an IA5String",
                    crate::gofmt::strconv::quote(d)
                ));
            }
        }
        let mut exts: Vec<Vec<u8>> = Vec::new();
        // KeyUsage (bit string, bits reversed per byte).
        let ku = t.key_usage;
        let a = [(ku as u8).reverse_bits(), ((ku >> 8) as u8).reverse_bits()];
        let l = if a[1] != 0 { 2 } else { 1 };
        let bits = &a[..l];
        let last = bits[l - 1];
        let unused = if last == 0 { 0 } else { last.trailing_zeros() as u8 };
        let mut bs = alloc::vec![unused];
        bs.extend_from_slice(bits);
        exts.push(extension(&[2, 5, 29, 15], true, &der::tlv(0x03, &bs)));
        // ExtKeyUsage: serverAuth, clientAuth.
        let eku = der::seq(&[&der::oid(&[1, 3, 6, 1, 5, 5, 7, 3, 1]), &der::oid(&[1, 3, 6, 1, 5, 5, 7, 3, 2])]);
        exts.push(extension(&[2, 5, 29, 37], false, &eku));
        // BasicConstraints.
        let bc = if t.is_ca { der::seq(&[&der::boolean(true)]) } else { der::seq(&[]) };
        exts.push(extension(&[2, 5, 29, 19], true, &bc));
        if !ski.is_empty() {
            exts.push(extension(&[2, 5, 29, 14], false, &der::octets(&ski)));
        }
        if !aki.is_empty() {
            exts.push(extension(&[2, 5, 29, 35], false, &der::seq(&[&der::tlv(0x80, &aki)])));
        }
        if !t.dns.is_empty() || !t.ips.is_empty() {
            let mut names: Vec<Vec<u8>> = t.dns.iter().map(|d| der::tlv(0x82, d.as_bytes())).collect();
            names.extend(t.ips.iter().map(|ip| der::tlv(0x87, ip)));
            let refs: Vec<&[u8]> = names.iter().map(|v| v.as_slice()).collect();
            let subject_empty = t.subject == der::seq(&[]);
            exts.push(extension(&[2, 5, 29, 17], subject_empty, &der::seq(&refs)));
        }
        let ext_refs: Vec<&[u8]> = exts.iter().map(|v| v.as_slice()).collect();
        let mut rng = EnvRng(ctx.env);
        let alg = signer.alg();
        let tbs = der::seq(&[
            &der::tlv(0xa0, &der::small_int(2)),
            &der::uint(&t.serial),
            &alg,
            &issuer,
            &der::seq(&[&time_der(t.not_before), &time_der(t.not_after)]),
            &t.subject,
            &spki,
            &der::tlv(0xa3, &der::seq(&ext_refs)),
        ]);
        let (alg, sig) = signer.sign(&tbs, &mut rng).map_err(|e| format!("error creating certificate: {e}"))?;
        let cert = der::seq(&[&tbs, &alg, &der::bitstring(&sig)]);
        Ok(Certificate { cert: pem_encode("CERTIFICATE", &cert), key: signee.pem() })
    }

    fn ok_cert(c: Certificate) -> FuncResult {
        Ok(Value::object(c))
    }

    fn gen_private_key(ctx: &mut CallCtx<'_>, a: &[Value]) -> FuncResult {
        let typ = sarg(a, 0);
        let key = match typ {
            "" | "rsa" => gen_rsa(ctx, 4096),
            "ecdsa" => Ok(Key::P256(p256::SecretKey::random(&mut EnvRng(ctx.env)))),
            "ed25519" => Ok(Key::Ed25519(ed25519_dalek::SigningKey::generate(&mut EnvRng(ctx.env)))),
            "dsa" => return Ok(Value::str("failed to generate dsa params: DSA key generation is not supported by helm-rs")),
            _ => return Ok(Value::string(format!("Unknown type {typ}"))),
        };
        Ok(Value::string(match key {
            Ok(k) => k.pem(),
            Err(e) => format!("failed to generate private key: {e}"),
        }))
    }

    fn rsa2048(ctx: &mut CallCtx<'_>) -> Result<Key, String> {
        gen_rsa(ctx, 2048).map_err(|e| format!("error generating rsa key: {e}"))
    }

    fn with_key(pem: &str) -> Result<Key, String> {
        parse_private_key_pem(pem).map_err(|e| format!("parsing private key: {e}"))
    }

    fn gen_ca(ctx: &mut CallCtx<'_>, cn: &str, days: i64, key: Key) -> FuncResult {
        let mut t = get_base_template(ctx, cn, &Value::Nil, &Value::Nil, days).map_err(FuncError::new)?;
        t.key_usage = KU_KEY_ENCIPHERMENT | KU_DIGITAL_SIGNATURE | KU_CERT_SIGN;
        t.is_ca = true;
        cert_and_key(ctx, &t, &key, None, &key).map_err(FuncError::new).and_then(ok_cert)
    }

    fn gen_self_signed(ctx: &mut CallCtx<'_>, a: &[Value], key: Key) -> FuncResult {
        let t = get_base_template(ctx, sarg(a, 0), arg(a, 1), arg(a, 2), iarg(a, 3)).map_err(FuncError::new)?;
        cert_and_key(ctx, &t, &key, None, &key).map_err(FuncError::new).and_then(ok_cert)
    }

    fn gen_signed(ctx: &mut CallCtx<'_>, a: &[Value], key: Key) -> FuncResult {
        let ca = match arg(a, 4) {
            Value::Object(o) => o.as_any().downcast_ref::<Certificate>().cloned(),
            _ => None,
        }
        .unwrap_or(Certificate { cert: String::new(), key: String::new() });
        let Some(block) = pem_decode(ca.cert.as_bytes()) else {
            return Err(FuncError::new("unable to decode certificate"));
        };
        let parent = parse_certificate(&block.bytes)
            .map_err(|e| FuncError::new(format!("error parsing certificate: decodedSignerCert.Bytes: {e}")))?;
        let signer = parse_private_key_pem(&ca.key).map_err(|e| FuncError::new(format!("error parsing private key: {e}")))?;
        let t = get_base_template(ctx, sarg(a, 0), arg(a, 1), arg(a, 2), iarg(a, 3)).map_err(FuncError::new)?;
        cert_and_key(ctx, &t, &key, Some(&parent), &signer).map_err(FuncError::new).and_then(ok_cert)
    }

    fn build_custom_cert(a: &[Value]) -> FuncResult {
        let cert = super::super::strings::base64_decode(sarg(a, 0).as_bytes())
            .map_err(|_| FuncError::new("unable to decode base64 certificate"))?;
        let key = super::super::strings::base64_decode(sarg(a, 1).as_bytes())
            .map_err(|_| FuncError::new("unable to decode base64 private key"))?;
        let Some(block) = pem_decode(&cert) else { return Err(FuncError::new("unable to decode certificate")) };
        parse_certificate(&block.bytes)
            .map_err(|e| FuncError::new(format!("error parsing certificate: decodedCert.Bytes: {e}")))?;
        let key_s = crate::gofmt::strconv::lossy(&key);
        parse_private_key_pem(&key_s).map_err(|e| FuncError::new(format!("error parsing private key: {e}")))?;
        ok_cert(Certificate { cert: crate::gofmt::strconv::lossy(&cert), key: key_s })
    }

    pub(super) fn register(f: &mut FuncMap, s: Ty, i: Ty, l: Ty, c: Ty) {
        let _ = (s, i, l, c, OID_DSA);
        const S: Ty = Ty::String;
        const I: Ty = Ty::Int;
        const L: Ty = Ty::List;
        const C: Ty = Ty::Object(CERT_TYPE);
        f.add_ctx("genPrivateKey", Sig::new(&[S]), gen_private_key);
        f.add("buildCustomCert", Sig::new(&[S, S]), build_custom_cert);
        f.add_ctx("genCA", Sig::new(&[S, I]), |ctx, a| {
            let key = rsa2048(ctx).map_err(FuncError::new)?;
            gen_ca(ctx, sarg(a, 0), iarg(a, 1), key)
        });
        f.add_ctx("genCAWithKey", Sig::new(&[S, I, S]), |ctx, a| {
            let key = with_key(sarg(a, 2)).map_err(FuncError::new)?;
            gen_ca(ctx, sarg(a, 0), iarg(a, 1), key)
        });
        f.add_ctx("genSelfSignedCert", Sig::new(&[S, L, L, I]), |ctx, a| {
            let key = rsa2048(ctx).map_err(FuncError::new)?;
            gen_self_signed(ctx, a, key)
        });
        f.add_ctx("genSelfSignedCertWithKey", Sig::new(&[S, L, L, I, S]), |ctx, a| {
            let key = with_key(sarg(a, 4)).map_err(FuncError::new)?;
            gen_self_signed(ctx, a, key)
        });
        f.add_ctx("genSignedCert", Sig::new(&[S, L, L, I, C]), |ctx, a| {
            let key = rsa2048(ctx).map_err(FuncError::new)?;
            gen_signed(ctx, a, key)
        });
        f.add_ctx("genSignedCertWithKey", Sig::new(&[S, L, L, I, C, S]), |ctx, a| {
            let key = with_key(sarg(a, 5)).map_err(FuncError::new)?;
            gen_signed(ctx, a, key)
        });
    }
}
