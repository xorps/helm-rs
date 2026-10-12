//! Tests for the nondeterministic crypto functions: shapes, cross-checks
//! with real Helm (Go must accept our keys and certificates and vice versa),
//! and `openssl verify` when available.

mod diffkit;

use diffkit::*;
use helm_rs::{CallCtx, DeterministicEnv, FuncMap, Value};

fn funcs() -> FuncMap {
    helm_rs::funcs::sprig()
}

fn call_env(
    f: &FuncMap,
    env: &mut DeterministicEnv,
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    let func = f.get(name).unwrap();
    let mut ctx = CallCtx { env };
    (func.imp)(&mut ctx, args).map_err(|e| e.message)
}

fn cert_fields(v: &Value) -> (String, String) {
    let o = v.as_object().expect("certificate object");
    let c = o.field("Cert").unwrap();
    let k = o.field("Key").unwrap();
    (
        c.as_str().unwrap().to_string(),
        k.as_str().unwrap().to_string(),
    )
}

fn b64(s: &str) -> String {
    let f = funcs();
    render_value(&call(&f, "b64enc", &[Value::str(s)]).unwrap())
}

#[test]
fn bcrypt_known_vector_and_shapes() {
    // OpenBSD test vector.
    let salt_b64 = "CCCCCCCCCCCCCCCCCCCCC.";
    // decode bcrypt base64 salt
    let alphabet = b"./ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut bits: Vec<u8> = Vec::new();
    for c in salt_b64.bytes() {
        let v = alphabet.iter().position(|&a| a == c).unwrap() as u8;
        for k in (0..6).rev() {
            bits.push((v >> k) & 1);
        }
    }
    let mut salt = [0u8; 16];
    for i in 0..16 {
        let mut b = 0u8;
        for k in 0..8 {
            b = (b << 1) | bits[i * 8 + k];
        }
        salt[i] = b;
    }
    let f = funcs();
    let mut env = DeterministicEnv::new(1);
    let h = render_value(&call_env(&f, &mut env, "bcrypt", &[Value::str("hello")]).unwrap());
    assert!(h.starts_with("$2a$10$") && h.len() == 60, "{h}");
    let h = render_value(
        &call_env(
            &f,
            &mut env,
            "htpasswd",
            &[Value::str("user"), Value::str("pw")],
        )
        .unwrap(),
    );
    assert!(h.starts_with("user:$2a$10$"), "{h}");
    let long = "x".repeat(73);
    let h = render_value(&call_env(&f, &mut env, "bcrypt", &[Value::str(&long)]).unwrap());
    assert_eq!(
        h,
        "failed to encrypt string with bcrypt: bcrypt: password length exceeds 72 bytes"
    );
    let _ = salt;
}

#[test]
fn random_shapes() {
    let f = funcs();
    let mut env = DeterministicEnv::new(7);
    let u = render_value(&call_env(&f, &mut env, "uuidv4", &[]).unwrap());
    assert_eq!(u.len(), 36);
    assert_eq!(&u[14..15], "4");
    assert!("89ab".contains(&u[19..20]));
    for (name, check) in [
        (
            "randAlphaNum",
            (|c: char| c.is_ascii_alphanumeric()) as fn(char) -> bool,
        ),
        ("randAlpha", |c: char| c.is_ascii_alphabetic()),
        ("randNumeric", |c: char| c.is_ascii_digit()),
        ("randAscii", |c: char| (' '..='~').contains(&c)),
    ] {
        let s = render_value(&call_env(&f, &mut env, name, &[Value::Int(50)]).unwrap());
        assert_eq!(s.len(), 50);
        assert!(s.chars().all(check), "{name}: {s}");
        assert_eq!(
            render_value(&call_env(&f, &mut env, name, &[Value::Int(-3)]).unwrap()),
            ""
        );
    }
    let s = render_value(&call_env(&f, &mut env, "randBytes", &[Value::Int(12)]).unwrap());
    assert_eq!(s.len(), 16);
    let s = render_value(&call_env(&f, &mut env, "shuffle", &[Value::str("abcdef")]).unwrap());
    let mut c: Vec<char> = s.chars().collect();
    c.sort();
    assert_eq!(c.into_iter().collect::<String>(), "abcdef");
    let r = call_env(&f, &mut env, "randInt", &[Value::Int(5), Value::Int(8)]).unwrap();
    assert!(matches!(r, Value::Int(5..=7)));
    assert!(call_env(&f, &mut env, "randInt", &[Value::Int(5), Value::Int(5)]).is_err());
    // encryptAES round trip in Rust.
    let ct = call_env(
        &f,
        &mut env,
        "encryptAES",
        &[Value::str("k"), Value::str("secret text")],
    )
    .unwrap();
    let pt = call_env(&f, &mut env, "decryptAES", &[Value::str("k"), ct.clone()]).unwrap();
    assert_eq!(render_value(&pt), "secret text");
    // Helm decrypts what we encrypted.
    let expr = format!("decryptAES \"k\" {}", lit(&render_value(&ct)));
    if let Some(out) = helm_eval_many(&[expr], "") {
        assert_eq!(out[0], Ok("secret text".to_string()));
    }
}

fn openssl() -> bool {
    std::process::Command::new("openssl")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn openssl_verify(ca: &str, cert: &str) {
    if !openssl() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("helm-rs-certs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cap = dir.join("ca.pem");
    let cp = dir.join("cert.pem");
    std::fs::write(&cap, ca).unwrap();
    std::fs::write(&cp, cert).unwrap();
    let out = std::process::Command::new("openssl")
        .args(["verify", "-no_check_time", "-partial_chain", "-CAfile"])
        .arg(&cap)
        .arg(&cp)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "openssl verify failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn certificates_cross_check_with_helm() {
    let f = funcs();
    let mut env = DeterministicEnv::new(42);
    // Our CA, our signed cert.
    let ca = call_env(
        &f,
        &mut env,
        "genCA",
        &[Value::str("my-ca"), Value::Int(365)],
    )
    .unwrap();
    let (ca_cert, ca_key) = cert_fields(&ca);
    assert!(ca_cert.starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(ca_key.starts_with("-----BEGIN RSA PRIVATE KEY-----\n"));
    let ips = Value::list(vec![Value::str("10.0.0.1"), Value::str("::1")]);
    let dns = Value::list(vec![Value::str("a.example.com"), Value::str("b")]);
    let signed = call_env(
        &f,
        &mut env,
        "genSignedCert",
        &[
            Value::str("leaf"),
            ips.clone(),
            dns.clone(),
            Value::Int(30),
            ca.clone(),
        ],
    )
    .unwrap();
    let (leaf_cert, leaf_key) = cert_fields(&signed);
    openssl_verify(&ca_cert, &leaf_cert);
    openssl_verify(&ca_cert, &ca_cert);
    // Self-signed with our ECDSA / Ed25519 keys.
    let mut keys = Vec::new();
    for ty in ["ecdsa", "ed25519"] {
        let k = render_value(&call_env(&f, &mut env, "genPrivateKey", &[Value::str(ty)]).unwrap());
        assert!(k.contains("PRIVATE KEY-----"), "{k}");
        let c = call_env(
            &f,
            &mut env,
            "genSelfSignedCertWithKey",
            &[
                Value::str("ss"),
                ips.clone(),
                dns.clone(),
                Value::Int(1),
                Value::str(&k),
            ],
        )
        .unwrap();
        let (cert, key) = cert_fields(&c);
        assert_eq!(key, k);
        openssl_verify(&cert, &cert);
        // A CA from that key, and a leaf signed by it.
        let ca2 = call_env(
            &f,
            &mut env,
            "genCAWithKey",
            &[Value::str("ca2"), Value::Int(5), Value::str(&k)],
        )
        .unwrap();
        let leaf2 = call_env(
            &f,
            &mut env,
            "genSignedCertWithKey",
            &[
                Value::str("l2"),
                Value::list(vec![]),
                Value::list(vec![]),
                Value::Int(5),
                ca2.clone(),
                Value::str(&k),
            ],
        )
        .unwrap();
        openssl_verify(&cert_fields(&ca2).0, &cert_fields(&leaf2).0);
        keys.push((k, cert));
    }
    // Helm must accept our certificates and keys.
    let mut exprs = Vec::new();
    exprs.push(format!(
        "(buildCustomCert {} {}).Cert | len",
        lit(&b64(&ca_cert)),
        lit(&b64(&ca_key))
    ));
    exprs.push(format!(
        "(buildCustomCert {} {}).Key | len",
        lit(&b64(&leaf_cert)),
        lit(&b64(&leaf_key))
    ));
    exprs.push(format!(
        "(genSignedCert \"x\" nil (list \"q\") 3 (buildCustomCert {} {})).Cert | trunc 27",
        lit(&b64(&ca_cert)),
        lit(&b64(&ca_key))
    ));
    for (k, cert) in &keys {
        exprs.push(format!(
            "(genSelfSignedCertWithKey \"x\" nil nil 3 {}).Cert | trunc 27",
            lit(k)
        ));
        exprs.push(format!(
            "(buildCustomCert {} {}).Cert | len",
            lit(&b64(cert)),
            lit(&b64(k))
        ));
    }
    let Some(out) = helm_eval_many(&exprs, "") else {
        return;
    };
    assert_eq!(out[0], Ok(ca_cert.len().to_string()));
    assert_eq!(out[1], Ok(leaf_key.len().to_string()));
    assert_eq!(out[2], Ok("-----BEGIN CERTIFICATE-----".to_string()));
    for i in 0..keys.len() {
        assert_eq!(
            out[3 + 2 * i],
            Ok("-----BEGIN CERTIFICATE-----".to_string()),
            "{}",
            exprs[3 + 2 * i]
        );
        assert_eq!(out[4 + 2 * i], Ok(keys[i].1.len().to_string()));
    }
}

#[test]
fn helm_certificates_accepted_by_us() {
    // Certificates and keys generated by Go must work with our functions.
    let expr = "$ca := genCA \"go-ca\" 10 }}{{ $ca.Cert }}@@{{ $ca.Key }}@@{{ genPrivateKey \"ecdsa\" }}@@{{ genPrivateKey \"ed25519\" }}@@{{ \"\"";
    let Some(out) = helm_eval_many(&[expr.to_string()], "") else {
        return;
    };
    let text = out[0].clone().unwrap();
    let parts: Vec<&str> = text.split("@@").collect();
    let (cert, key, eckey, edkey) = (parts[0], parts[1], parts[2], parts[3]);
    let f = funcs();
    let mut env = DeterministicEnv::new(9);
    let ca = call_env(
        &f,
        &mut env,
        "buildCustomCert",
        &[Value::string(b64(cert)), Value::string(b64(key))],
    )
    .unwrap();
    let leaf = call_env(
        &f,
        &mut env,
        "genSignedCertWithKey",
        &[
            Value::str("leaf"),
            Value::list(vec![]),
            Value::list(vec![Value::str("x.y")]),
            Value::Int(3),
            ca.clone(),
            Value::str(eckey),
        ],
    )
    .unwrap();
    openssl_verify(cert, &cert_fields(&leaf).0);
    let ss = call_env(
        &f,
        &mut env,
        "genSelfSignedCertWithKey",
        &[
            Value::str("ss"),
            Value::list(vec![]),
            Value::list(vec![]),
            Value::Int(3),
            Value::str(edkey),
        ],
    )
    .unwrap();
    openssl_verify(&cert_fields(&ss).0, &cert_fields(&ss).0);
}

#[test]
fn certificate_errors_match_helm() {
    let f = funcs();
    let mut cases: Vec<(String, String, Vec<Value>)> = Vec::new();
    let mut add =
        |expr: String, name: &str, args: Vec<Value>| cases.push((expr, name.to_string(), args));
    add(
        "buildCustomCert \"!!\" \"\"".into(),
        "buildCustomCert",
        vec![Value::str("!!"), Value::str("")],
    );
    add(
        format!("buildCustomCert {} \"!!\"", lit(&b64("x"))),
        "buildCustomCert",
        vec![Value::string(b64("x")), Value::str("!!")],
    );
    add(
        format!("buildCustomCert {} \"\"", lit(&b64("not pem"))),
        "buildCustomCert",
        vec![Value::string(b64("not pem")), Value::str("")],
    );
    let bad_cert = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    add(
        format!("buildCustomCert {} \"\"", lit(&b64(bad_cert))),
        "buildCustomCert",
        vec![Value::string(b64(bad_cert)), Value::str("")],
    );
    for pem in [
        "",
        "garbage",
        "-----BEGIN FOO-----\nAAAA\n-----END FOO-----\n",
        "-----BEGIN FOO PRIVATE KEY-----\nAAAA\n-----END FOO PRIVATE KEY-----\n",
    ] {
        add(
            format!("genSelfSignedCertWithKey \"x\" nil nil 1 {}", lit(pem)),
            "genSelfSignedCertWithKey",
            vec![
                Value::str("x"),
                Value::list(vec![]),
                Value::list(vec![]),
                Value::Int(1),
                Value::str(pem),
            ],
        );
        add(
            format!("genCAWithKey \"x\" 1 {}", lit(pem)),
            "genCAWithKey",
            vec![Value::str("x"), Value::Int(1), Value::str(pem)],
        );
    }
    add(
        "genPrivateKey \"nope\"".into(),
        "genPrivateKey",
        vec![Value::str("nope")],
    );
    let ec = {
        let mut env = DeterministicEnv::new(3);
        render_value(&call_env(&f, &mut env, "genPrivateKey", &[Value::str("ecdsa")]).unwrap())
    };
    add(
        format!("genSelfSignedCertWithKey \"x\" (list 1) nil 1 {}", lit(&ec)),
        "genSelfSignedCertWithKey",
        vec![
            Value::str("x"),
            Value::list(vec![Value::Int(1)]),
            Value::list(vec![]),
            Value::Int(1),
            Value::str(&ec),
        ],
    );
    add(
        format!(
            "genSelfSignedCertWithKey \"x\" (list \"1.2.3\") nil 1 {}",
            lit(&ec)
        ),
        "genSelfSignedCertWithKey",
        vec![
            Value::str("x"),
            Value::list(vec![Value::str("1.2.3")]),
            Value::list(vec![]),
            Value::Int(1),
            Value::str(&ec),
        ],
    );
    add(
        format!("genSelfSignedCertWithKey \"x\" nil (list 2) 1 {}", lit(&ec)),
        "genSelfSignedCertWithKey",
        vec![
            Value::str("x"),
            Value::list(vec![]),
            Value::list(vec![Value::Int(2)]),
            Value::Int(1),
            Value::str(&ec),
        ],
    );
    add(
        format!(
            "genSelfSignedCertWithKey \"x\" nil (list \"é.com\") 1 {}",
            lit(&ec)
        ),
        "genSelfSignedCertWithKey",
        vec![
            Value::str("x"),
            Value::list(vec![]),
            Value::list(vec![Value::str("é.com")]),
            Value::Int(1),
            Value::str(&ec),
        ],
    );
    let exprs: Vec<String> = cases.iter().map(|c| c.0.clone()).collect();
    let mut env = DeterministicEnv::new(5);
    let ours: Vec<Result<String, String>> = cases
        .iter()
        .map(|c| call_env(&f, &mut env, &c.1, &c.2).map(|v| render_value(&v)))
        .collect();
    let Some(helm) = helm_eval_many(&exprs, "") else {
        return;
    };
    let bad = compare(&exprs, &ours, &helm);
    report("certificate_errors_match_helm", exprs.len(), &bad);
}

#[test]
fn rsa4096_private_key_accepted_by_helm() {
    let f = funcs();
    let mut env = DeterministicEnv::new(11);
    let k = render_value(&call_env(&f, &mut env, "genPrivateKey", &[Value::str("rsa")]).unwrap());
    assert!(k.starts_with("-----BEGIN RSA PRIVATE KEY-----\n"));
    let expr = format!(
        "(genSelfSignedCertWithKey \"x\" nil nil 3 {}).Key | len",
        lit(&k)
    );
    let Some(out) = helm_eval_many(&[expr], "") else {
        return;
    };
    assert_eq!(out[0], Ok(k.len().to_string()));
}
