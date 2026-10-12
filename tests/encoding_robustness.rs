//! The encoders/decoders must never panic or overflow the stack, whatever
//! the input. Deep-nesting cases run on a deliberately small stack.
mod common;
mod encoding_support;

use encoding_support::*;
use helm_rs::funcs::encoding as enc;
use helm_rs::value::Map;
use helm_rs::{Value, json, yaml};

/// The nesting limit of the encoders/decoders.
const LIMIT: usize = 256;

fn exercise(input: &str) {
    let _ = yaml::from_str(input);
    let m = enc::from_yaml(input);
    let _ = enc::to_yaml(&m);
    let _ = enc::to_yaml_pretty(&m);
    let _ = enc::to_toml(&m);
    let _ = enc::from_yaml_array(input);
    let j = enc::from_json(input);
    let _ = enc::to_json(&j);
    let _ = json::to_string_indent(&j, "", "  ");
    let _ = enc::from_json_array(input);
    let _ = yaml::json_to_yaml(input);
    let t = enc::from_toml(input);
    let _ = enc::to_toml(&t);
}

/// Runs `f` on a thread with a 1 MiB stack.
fn small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn random_bytes_do_not_panic() {
    let mut r = Rng::new(1234);
    let alphabet: Vec<char> =
        "ab01 \t\n\r:-?,[]{}#&*!|>'\"%@`.~<=+_\\/xyz\u{feff}\u{85}\u{2028}\u{1}\u{7f}é😀"
            .chars()
            .collect();
    for _ in 0..20000 {
        let n = r.below(60);
        let s: String = (0..n).map(|_| alphabet[r.below(alphabet.len())]).collect();
        exercise(&s);
    }
    for _ in 0..3000 {
        let doc = gen_doc(&mut r);
        exercise(&mutate(&mut r, &doc));
    }
}

#[test]
fn deep_nesting_is_rejected_cleanly() {
    small_stack(|| {
        for depth in [LIMIT - 2, LIMIT - 1, LIMIT, LIMIT + 1, 1000, 20000, 200_000] {
            let flow = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
            exercise(&flow);
            let obj = format!("{}1{}", "{\"a\":".repeat(depth), "}".repeat(depth));
            exercise(&obj);
            let flow_map = format!("a: {}{}", "{a: ".repeat(depth), "}".repeat(depth));
            exercise(&flow_map);
            let toml_arr = format!("a = {}{}", "[".repeat(depth), "]".repeat(depth));
            exercise(&toml_arr);
            let toml_tab = format!("a = {}{}", "{b = ".repeat(depth), "}".repeat(depth));
            exercise(&toml_tab);
        }
        let ok = format!("a: {}{}", "[".repeat(LIMIT - 2), "]".repeat(LIMIT - 2));
        assert!(yaml::from_str(&ok).is_ok());
        let e =
            yaml::from_str(&format!("a: {}{}", "[".repeat(5000), "]".repeat(5000))).unwrap_err();
        assert!(e.message.contains("exceeded max depth"), "{}", e.message);
        // Block nesting.
        let mut block = String::new();
        for i in 0..3000 {
            block.push_str(&" ".repeat(i));
            block.push_str("a:\n");
        }
        exercise(&block);
        let mut seq = String::new();
        for i in 0..3000 {
            seq.push_str(&" ".repeat(i));
            seq.push_str("-\n");
        }
        exercise(&seq);
        // Alias chains create deep values from shallow documents.
        let mut chain = String::from("a0: &a0 [x]\n");
        for i in 1..3000 {
            chain.push_str(&format!("a{i}: &a{i} [*a{}]\n", i - 1));
        }
        exercise(&chain);
    });
}

#[test]
fn billion_laughs_is_rejected() {
    let mut doc = String::from(
        "a: &a [\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\"]\n",
    );
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    for w in names.windows(2) {
        let refs = vec![format!("*{}", w[0]); 9].join(",");
        doc.push_str(&format!("{}: &{} [{refs}]\n", w[1], w[1]));
    }
    let e = yaml::from_str(&doc).unwrap_err();
    assert_eq!(
        e.message,
        "error converting YAML to JSON: yaml: document contains excessive aliasing"
    );
}

#[test]
fn deep_and_cyclic_values_do_not_overflow() {
    small_stack(|| {
        for depth in [LIMIT - 2, LIMIT + 1, 100_000] {
            let mut v = Value::Nil;
            for _ in 0..depth {
                v = Value::list(vec![v]);
            }
            let m = helm_rs::map! { "a" => v.clone() };
            if depth > LIMIT {
                assert!(json::to_string(&v).is_err());
                assert!(yaml::to_string(&v).is_err());
                assert!(yaml::to_string_pretty(&v).is_err());
            }
            let _ = enc::to_toml(&m);
            let _ = enc::to_yaml(&m);
            drop(m);
            // Unwrap the nesting to avoid a deep recursive drop.
            let mut cur = v;
            while let Value::List(l) = cur {
                cur = std::rc::Rc::try_unwrap(l)
                    .map(|mut x| x.pop().unwrap_or(Value::Nil))
                    .unwrap_or(Value::Nil);
            }
        }

        let m = Value::map(Map::new());
        if let Value::Map(r) = &m {
            r.insert("self", m.clone());
        }
        let e = json::to_string(&m).unwrap_err();
        assert_eq!(
            e.message,
            "json: unsupported value: encountered a cycle via map[string]interface {}"
        );
        assert_eq!(enc::to_yaml(&m), "");
        assert!(enc::must_to_yaml(&m).is_err());
        assert_eq!(enc::to_yaml_pretty(&m), "");
        assert!(enc::must_to_toml(&m).is_err());
        // A cycle through a list: Go reports the container met at nesting
        // level 1001.
        let l = Value::list(vec![m.clone()]);
        if let Value::Map(r) = &m {
            r.insert("self", l.clone());
        }
        let e = json::to_string(&m).unwrap_err();
        assert_eq!(
            e.message,
            "json: unsupported value: encountered a cycle via map[string]interface {}"
        );
        let e = json::to_string(&l).unwrap_err();
        assert_eq!(
            e.message,
            "json: unsupported value: encountered a cycle via []interface {}"
        );
        if let Value::Map(r) = &m {
            r.remove("self");
        }
    });
}
