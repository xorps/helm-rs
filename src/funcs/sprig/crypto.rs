//! `crypto.go`: bcrypt/htpasswd, derivePassword, AES, random bytes, UUIDs,
//! and the key/certificate generators.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use hmac::Mac as _;

use super::strings::{base64_decode, base64_encode};
use super::{iarg, sarg};
use crate::func::{CallCtx, FuncError, FuncMap, FuncResult, Sig, Ty};
use crate::gofmt::strconv;
use crate::value::Value;

const S: Ty = Ty::String;
const I: Ty = Ty::Int;

pub(super) fn register(f: &mut FuncMap) {
    f.add_ctx("bcrypt", Sig::new(&[S]), |c, a| {
        Ok(Value::string(bcrypt_string(c, sarg(a, 0))))
    });
    f.add_ctx("htpasswd", Sig::new(&[S, S]), |c, a| {
        let user = sarg(a, 0);
        if user.contains(':') {
            return Ok(Value::string(format!("invalid username: {user}")));
        }
        Ok(Value::string(format!(
            "{user}:{}",
            bcrypt_string(c, sarg(a, 1))
        )))
    });
    f.add("derivePassword", Sig::new(&[Ty::Uint32, S, S, S, S]), |a| {
        let counter = match a.first() {
            Some(Value::Uint64(n)) => *n as u32,
            Some(Value::Int(n)) | Some(Value::Int64(n)) => *n as u32,
            _ => 0,
        };
        Ok(Value::string(derive_password(
            counter,
            sarg(a, 1),
            sarg(a, 2),
            sarg(a, 3),
            sarg(a, 4),
        )))
    });
    f.add_ctx("encryptAES", Sig::new(&[S, S]), encrypt_aes);
    f.add("decryptAES", Sig::new(&[S, S]), decrypt_aes);
    f.add_ctx("randBytes", Sig::new(&[I]), |c, a| {
        let n = iarg(a, 0);
        if n < 0 {
            return Err(FuncError::new("runtime error: makeslice: len out of range"));
        }
        if n as u128 > super::MAX_OUTPUT as u128 {
            return Err(super::too_large("randBytes"));
        }
        let mut buf = alloc::vec![0u8; n as usize];
        c.env.fill_random(&mut buf);
        Ok(Value::string(base64_encode(&buf)))
    });
    f.add_ctx("uuidv4", Sig::new(&[]), |c, _| Ok(Value::string(uuidv4(c))));
    f.add_ctx("getHostByName", Sig::new(&[S]), |c, a| {
        c.env.get_host_by_name(sarg(a, 0)).map(Value::string)
    });
    super::certs::register(f);
}

// ---------------------------------------------------------------------------
// bcrypt (golang.org/x/crypto/bcrypt, cost 10, version 2a)
// ---------------------------------------------------------------------------

const BCRYPT_ALPHABET: &[u8; 64] =
    b"./ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

fn bcrypt_b64(src: &[u8]) -> String {
    // Standard base64 bit grouping with the bcrypt alphabet, no padding.
    let mut out = String::new();
    for chunk in src.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let n = chunk.len() + 1;
        for i in 0..n {
            out.push(BCRYPT_ALPHABET[((v >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// bcrypt hash of `password` with `salt` (16 bytes) and `cost`.
pub(crate) fn bcrypt_hash(password: &[u8], cost: u32, salt: &[u8; 16]) -> String {
    let mut key = password.to_vec();
    key.push(0);
    let mut state = blowfish::Blowfish::bc_init_state();
    state.salted_expand_key(salt, &key);
    for _ in 0..(1u64 << cost) {
        state.bc_expand_key(&key);
        state.bc_expand_key(salt);
    }
    let magic = b"OrpheanBeholderScryDoubt";
    let mut ctext = [0u32; 6];
    for i in 0..6 {
        ctext[i] = u32::from_be_bytes([
            magic[4 * i],
            magic[4 * i + 1],
            magic[4 * i + 2],
            magic[4 * i + 3],
        ]);
    }
    for i in (0..6).step_by(2) {
        for _ in 0..64 {
            let [l, r] = state.bc_encrypt([ctext[i], ctext[i + 1]]);
            ctext[i] = l;
            ctext[i + 1] = r;
        }
    }
    let mut out = Vec::with_capacity(24);
    for w in ctext {
        out.extend_from_slice(&w.to_be_bytes());
    }
    format!(
        "$2a${cost:02}${}{}",
        bcrypt_b64(salt),
        bcrypt_b64(&out[..23])
    )
}

fn bcrypt_string(ctx: &mut CallCtx<'_>, input: &str) -> String {
    if input.len() > 72 {
        return String::from(
            "failed to encrypt string with bcrypt: bcrypt: password length exceeds 72 bytes",
        );
    }
    let mut salt = [0u8; 16];
    ctx.env.fill_random(&mut salt);
    bcrypt_hash(input.as_bytes(), 10, &salt)
}

// ---------------------------------------------------------------------------
// derivePassword (Master Password algorithm)
// ---------------------------------------------------------------------------

fn password_templates(t: &str) -> Option<&'static [&'static str]> {
    Some(match t {
        "maximum" => &["anoxxxxxxxxxxxxxxxxx", "axxxxxxxxxxxxxxxxxno"],
        "long" => &[
            "CvcvnoCvcvCvcv",
            "CvcvCvcvnoCvcv",
            "CvcvCvcvCvcvno",
            "CvccnoCvcvCvcv",
            "CvccCvcvnoCvcv",
            "CvccCvcvCvcvno",
            "CvcvnoCvccCvcv",
            "CvcvCvccnoCvcv",
            "CvcvCvccCvcvno",
            "CvcvnoCvcvCvcc",
            "CvcvCvcvnoCvcc",
            "CvcvCvcvCvccno",
            "CvccnoCvccCvcv",
            "CvccCvccnoCvcv",
            "CvccCvccCvcvno",
            "CvcvnoCvccCvcc",
            "CvcvCvccnoCvcc",
            "CvcvCvccCvccno",
            "CvccnoCvcvCvcc",
            "CvccCvcvnoCvcc",
            "CvccCvcvCvccno",
        ],
        "medium" => &["CvcnoCvc", "CvcCvcno"],
        "short" => &["Cvcn"],
        "basic" => &["aaanaaan", "aannaaan", "aaannaaa"],
        "pin" => &["nnnn"],
        _ => return None,
    })
}

fn template_chars(c: u8) -> &'static [u8] {
    match c {
        b'V' => b"AEIOU",
        b'C' => b"BCDFGHJKLMNPQRSTVWXYZ",
        b'v' => b"aeiou",
        b'c' => b"bcdfghjklmnpqrstvwxyz",
        b'A' => b"AEIOUBCDFGHJKLMNPQRSTVWXYZ",
        b'a' => b"AEIOUaeiouBCDFGHJKLMNPQRSTVWXYZbcdfghjklmnpqrstvwxyz",
        b'n' => b"0123456789",
        b'o' => b"@&%?,=[]_:-+*$#!'^~;()/.",
        b'x' => b"AEIOUaeiouBCDFGHJKLMNPQRSTVWXYZbcdfghjklmnpqrstvwxyz0123456789!@#$%^&*()",
        _ => b"",
    }
}

fn derive_password(
    counter: u32,
    password_type: &str,
    password: &str,
    user: &str,
    site: &str,
) -> String {
    let Some(templates) = password_templates(password_type) else {
        return format!("cannot find password template {password_type}");
    };
    let seed = b"com.lyndir.masterpassword";
    let mut salt = seed.to_vec();
    salt.extend_from_slice(&(user.len() as u32).to_be_bytes());
    salt.extend_from_slice(user.as_bytes());
    let mut key = [0u8; 64];
    let Ok(params) = scrypt::Params::new(15, 8, 2, 64) else {
        return String::from("failed to derive password: invalid parameters");
    };
    if scrypt::scrypt(password.as_bytes(), &salt, &params, &mut key).is_err() {
        return String::from("failed to derive password: scrypt failed");
    }
    let mut buf = seed.to_vec();
    buf.extend_from_slice(&(site.len() as u32).to_be_bytes());
    buf.extend_from_slice(site.as_bytes());
    buf.extend_from_slice(&counter.to_be_bytes());
    let Ok(mut mac) = hmac::Hmac::<sha2::Sha256>::new_from_slice(&key) else {
        return String::new();
    };
    mac.update(&buf);
    let seed = mac.finalize().into_bytes();
    let temp = templates[seed[0] as usize % templates.len()].as_bytes();
    let mut out = String::new();
    for (i, &element) in temp.iter().enumerate() {
        let chars = template_chars(element);
        out.push(chars[seed[i + 1] as usize % chars.len()] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// AES-256-CBC
// ---------------------------------------------------------------------------

fn aes_key(password: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    let p = password.as_bytes();
    let n = p.len().min(32);
    key[..n].copy_from_slice(&p[..n]);
    key
}

fn encrypt_aes(ctx: &mut CallCtx<'_>, a: &[Value]) -> FuncResult {
    use aes::cipher::{BlockEncrypt, KeyInit};
    let (password, plaintext) = (sarg(a, 0), sarg(a, 1));
    if plaintext.is_empty() {
        return Ok(Value::str(""));
    }
    let key = aes_key(password);
    let cipher = aes::Aes256::new(&key.into());
    let mut content = plaintext.as_bytes().to_vec();
    let padding = 16 - content.len() % 16;
    content.extend(core::iter::repeat_n(padding as u8, padding));
    let mut iv = [0u8; 16];
    ctx.env.fill_random(&mut iv);
    let mut out = iv.to_vec();
    let mut prev = iv;
    for chunk in content.chunks(16) {
        let mut b = aes::Block::default();
        for i in 0..16 {
            b[i] = chunk[i] ^ prev[i];
        }
        cipher.encrypt_block(&mut b);
        prev.copy_from_slice(&b);
        out.extend_from_slice(&b);
    }
    Ok(Value::string(base64_encode(&out)))
}

fn decrypt_aes(a: &[Value]) -> FuncResult {
    use aes::cipher::{BlockDecrypt, KeyInit};
    let (password, crypt64) = (sarg(a, 0), sarg(a, 1));
    if crypt64.is_empty() {
        return Ok(Value::str(""));
    }
    let key = aes_key(password);
    let crypt = base64_decode(crypt64.as_bytes()).map_err(FuncError::new)?;
    if crypt.len() < 16 {
        // Go slices the decode buffer, whose capacity is DecodedLen(len(s)).
        return Err(FuncError::new(format!(
            "runtime error: slice bounds out of range [:16] with capacity {}",
            crypt64.len() / 4 * 3
        )));
    }
    let (iv, body) = crypt.split_at(16);
    if body.len() % 16 != 0 {
        return Err(FuncError::new("crypto/cipher: input not full blocks"));
    }
    let cipher = aes::Aes256::new(&key.into());
    let mut pt = Vec::with_capacity(body.len());
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);
    for chunk in body.chunks(16) {
        let mut b = aes::Block::clone_from_slice(chunk);
        cipher.decrypt_block(&mut b);
        for i in 0..16 {
            pt.push(b[i] ^ prev[i]);
        }
        prev.copy_from_slice(chunk);
    }
    let Some(&last) = pt.last() else {
        return Err(FuncError::new("runtime error: index out of range [-1]"));
    };
    let end = pt.len() as i64 - last as i64;
    if end < 0 {
        return Err(FuncError::new(format!(
            "runtime error: slice bounds out of range [:{end}]"
        )));
    }
    Ok(Value::string(strconv::lossy(&pt[..end as usize])))
}

// ---------------------------------------------------------------------------
// UUID v4
// ---------------------------------------------------------------------------

fn uuidv4(ctx: &mut CallCtx<'_>) -> String {
    let mut b = [0u8; 16];
    ctx.env.fill_random(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = super::strings::hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bcrypt_b64_decode(s: &str) -> Vec<u8> {
        let mut bits = Vec::new();
        for c in s.bytes() {
            let v = BCRYPT_ALPHABET.iter().position(|&a| a == c).unwrap_or(0) as u8;
            for k in (0..6).rev() {
                bits.push((v >> k) & 1);
            }
        }
        bits.chunks(8)
            .filter(|c| c.len() == 8)
            .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b))
            .collect()
    }

    #[test]
    fn bcrypt_openbsd_vectors() {
        for (pw, salt, cost, want) in [
            (
                "U*U",
                "CCCCCCCCCCCCCCCCCCCCC.",
                5,
                "$2a$05$CCCCCCCCCCCCCCCCCCCCC.E5YPO9kmyuRGyh0XouQYb4YMJKvyOeW",
            ),
            (
                "",
                "CCCCCCCCCCCCCCCCCCCCC.",
                5,
                "$2a$05$CCCCCCCCCCCCCCCCCCCCC.7uG0VCzI2bS7j6ymqJi9CdcdxiRTWNy",
            ),
            (
                "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789chars after 72 are ignored",
                "abcdefghijklmnopqrstuu",
                5,
                "$2a$05$abcdefghijklmnopqrstu.5s2v8.iXieOjg/.AySBTTZIIVFJeBui",
            ),
        ] {
            let s = bcrypt_b64_decode(salt);
            let mut salt16 = [0u8; 16];
            salt16.copy_from_slice(&s[..16]);
            let pw = &pw.as_bytes()[..pw.len().min(72)];
            // (The third vector's salt is non-canonical; compare the hash.)
            assert_eq!(bcrypt_hash(pw, cost, &salt16)[29..], want[29..]);
        }
    }

    #[test]
    fn derive_password_shape() {
        let p = derive_password(1, "long", "pass", "user", "example.com");
        assert_eq!(p.len(), 14);
        assert_eq!(
            derive_password(1, "nope", "a", "b", "c"),
            "cannot find password template nope"
        );
    }
}
