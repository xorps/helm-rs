//! `values.schema.json` validation diffed against Helm (santhosh-tekuri/jsonschema).

mod common;

use common::chart::*;
use helm_rs::chart;
use serde_json::json;

fn check(schema: &str, values: &str) {
    let Some(want) = oracle_op(json!({"op": "schema", "schema": schema, "values": values})) else {
        return;
    };
    let v = json_yaml(values).unwrap();
    let m = v.as_map().cloned().unwrap_or_default();
    let got = chart::validate_against_schema(&m, schema.as_bytes());
    match (want, got) {
        (Ok(_), Ok(())) => {}
        (Err(w), Err(g)) => {
            // The library collects sibling keyword errors from Go maps, so
            // Helm's own order varies between runs; compare them as a set.
            fn sorted(s: &str) -> Vec<&str> {
                let mut l: Vec<&str> = s.lines().collect();
                l.sort_unstable();
                l
            }
            if sorted(&g) != sorted(&w) {
                // Helm's validator is itself nondeterministic in places (e.g.
                // metaschema error paths built from Go map iteration): accept
                // any output Helm produces for the same input.
                let helm_agrees = (0..32).any(|_| {
                    matches!(oracle_op(json!({"op": "schema", "schema": schema, "values": values})),
                        Some(Err(w2)) if sorted(&w2) == sorted(&g))
                });
                assert!(
                    helm_agrees,
                    "schema {schema}, values {values}\n ours: {g}\n helm: {w}"
                );
            }
        }
        (w, g) => panic!("schema {schema}, values {values}: oracle {w:?}, rust {g:?}"),
    }
}

#[test]
fn schema_validation_matches_helm() {
    let cases: &[(&str, &str)] = &[
        (
            r##"{"type":"object","properties":{"a":{"type":"integer"}}}"##,
            r##"{"a": "x"}"##,
        ),
        (
            r##"{"properties":{"a":{"type":"integer"}},"required":["b","c"]}"##,
            r##"{"a": 1.5}"##,
        ),
        (
            r##"{"properties":{"a":{"type":"integer"}}}"##,
            r##"{"a": 2.0}"##,
        ),
        (
            r##"{"properties":{"a":{"$ref":"#/$defs/x"}},"$defs":{"x":{"type":"string","minLength":3}}}"##,
            r##"{"a": "ab"}"##,
        ),
        (
            r##"{"properties":{"a":{"$ref":"#/$defs/x"}},"$defs":{"x":{"type":"string","minLength":3,"maxLength":1}}}"##,
            r##"{"a": "ab"}"##,
        ),
        (
            r##"{"properties":{"a":{"$ref":"#/definitions/x"}},"definitions":{"x":{"$ref":"#/definitions/y"},"y":{"type":"string"}}}"##,
            r##"{"a": 1}"##,
        ),
        (
            r##"{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"a":{"$ref":"#/definitions/x","type":"number"}},"definitions":{"x":{"type":"string"}}}"##,
            r##"{"a": 1}"##,
        ),
        (
            r##"{"properties":{"a":{"$ref":"#/definitions/x","type":"number"}},"definitions":{"x":{"type":"string"}}}"##,
            r##"{"a": true}"##,
        ),
        (
            r##"{"properties":{"a":{"$ref":"#foo"}},"$defs":{"x":{"$anchor":"foo","type":"string"}}}"##,
            r##"{"a": 1}"##,
        ),
        (
            r##"{"anyOf":[{"required":["a"]},{"required":["b"]}]}"##,
            r##"{"c": 1}"##,
        ),
        (
            r##"{"oneOf":[{"required":["a"]},{"required":["b"]}]}"##,
            r##"{"a": 1, "b": 2}"##,
        ),
        (
            r##"{"oneOf":[{"required":["a"]},{"required":["b"]}]}"##,
            r##"{"c": 1}"##,
        ),
        (
            r##"{"properties":{"num":{"minimum":5,"maximum":1,"multipleOf":0.1,"exclusiveMaximum":0}}}"##,
            r##"{"num": 3.33}"##,
        ),
        (
            r##"{"properties":{"num":{"multipleOf":0.01}}}"##,
            r##"{"num": 0.07}"##,
        ),
        (
            r##"{"properties":{"num":{"multipleOf":3}}}"##,
            r##"{"num": 1e21}"##,
        ),
        (
            r##"{"properties":{"num":{"minimum":1e3}}}"##,
            r##"{"num": 1}"##,
        ),
        (
            r##"{"properties":{"num":{"enum":["a",1,null,true]}}}"##,
            r##"{"num": 2}"##,
        ),
        (
            r##"{"properties":{"num":{"enum":["a",[1]]}}}"##,
            r##"{"num": 2}"##,
        ),
        (
            r##"{"properties":{"num":{"enum":[1.0]}}}"##,
            r##"{"num": 2}"##,
        ),
        (
            r##"{"properties":{"num":{"const":"a"}}}"##,
            r##"{"num": 2}"##,
        ),
        (
            r##"{"properties":{"num":{"const":{"a":1}}}}"##,
            r##"{"num": {"a": 1}}"##,
        ),
        (
            r##"{"properties":{"num":{"const":{"a":1}}}}"##,
            r##"{"num": {"a": 2}}"##,
        ),
        (
            r##"{"additionalProperties":false,"properties":{"x":{}}}"##,
            r##"{"a": 1, "x": 1}"##,
        ),
        (
            r##"{"additionalProperties":{"type":"string"}}"##,
            r##"{"a": 1}"##,
        ),
        (
            r##"{"properties":{"l":{"items":{"type":"string"},"minItems":3,"maxItems":1,"uniqueItems":true}}}"##,
            r##"{"l": [1, 1]}"##,
        ),
        (
            r##"{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"l":{"items":[{"type":"string"}],"additionalItems":false}}}"##,
            r##"{"l": [1, 1]}"##,
        ),
        (
            r##"{"properties":{"l":{"prefixItems":[{"type":"string"}],"items":{"type":"boolean"}}}}"##,
            r##"{"l": [1, 1]}"##,
        ),
        (
            r##"{"properties":{"l":{"contains":{"type":"string"}}}}"##,
            r##"{"l": [1, 2]}"##,
        ),
        (
            r##"{"properties":{"l":{"contains":{"type":"string"},"minContains":2,"maxContains":0}}}"##,
            r##"{"l": [1, "a"]}"##,
        ),
        (r##"{"not":{"type":"object"}}"##, r##"{"a": 1}"##),
        (
            r##"{"allOf":[{"required":["a"]},{"properties":{"b":{"type":"string"}}}]}"##,
            r##"{"b": 1}"##,
        ),
        (r##"{"$ref":"#"}"##, r##"{"a": 1}"##),
        (
            r##"{"properties":{"a":{"$ref":"#"}}}"##,
            r##"{"a": {"a": 1}}"##,
        ),
        (r##"{"type":5}"##, r##"{"a": 1}"##),
        ("{bad", r##"{"a": 1}"##),
        ("", r##"{"a": 1}"##),
        ("{} x", r##"{"a": 1}"##),
        ("[1,", r##"{"a": 1}"##),
        (r##"{"a": tru}"##, r##"{"a": 1}"##),
        (
            r##"{"properties":{"a":{"type":["string","null"]}}}"##,
            r##"{"a": 1}"##,
        ),
        (
            r##"{"properties":{"a/b~c":{"type":"string"}}}"##,
            r##"{"a/b~c": 1}"##,
        ),
        (
            r##"{"properties":{"a":{"properties":{"b":{"type":"string"}}}}}"##,
            r##"{"a": {"b": 1}}"##,
        ),
        (
            r##"{"if":{"required":["a"]},"then":{"required":["b"]},"else":{"required":["c"]}}"##,
            r##"{"a": 1}"##,
        ),
        (
            r##"{"if":{"required":["a"]},"then":{"required":["b"]},"else":{"required":["c"]}}"##,
            r##"{"x": 1}"##,
        ),
        (
            r##"{"dependentRequired":{"a":["b"]},"propertyNames":{"maxLength":1}}"##,
            r##"{"a": 1, "long": 2}"##,
        ),
        (
            r##"{"propertyNames":{"maxLength":1, "minLength": 9}}"##,
            r##"{"long": 2}"##,
        ),
        (
            r##"{"dependencies":{"a":["b", "c"], "d": {"required": ["e"]}}}"##,
            r##"{"a": 1, "d": 2}"##,
        ),
        (
            r##"{"dependentSchemas":{"a":{"required":["z"]}}}"##,
            r##"{"a": 1}"##,
        ),
        (
            r##"{"$schema":"http://json-schema.org/draft-04/schema#","properties":{"num":{"minimum":5,"exclusiveMinimum":true}}}"##,
            r##"{"num": 5}"##,
        ),
        (
            r##"{"minProperties":3,"maxProperties":0}"##,
            r##"{"num": 5}"##,
        ),
        ("false", r##"{"a": 1}"##),
        ("true", r##"{"a": 1}"##),
        (r##"{"properties":{"a":false}}"##, r##"{"a": 1}"##),
        (
            r##"{"properties":{"s":{"minLength":2}}}"##,
            r##"{"s": "é"}"##,
        ),
        (
            r##"{"properties":{"s":{"enum":["it's \"q\"\n"]}}}"##,
            r##"{"s": "x"}"##,
        ),
        (r##"{"required":["it's"]}"##, r##"{"s": "x"}"##),
    ];
    for (s, v) in cases {
        check(s, v);
    }
}

#[test]
fn schema_number_formatting_matches_helm() {
    for (max, v) in [
        ("0", "1000000"),
        ("0", "12345.5"),
        ("-1", "0.00001"),
        ("0", "1e21"),
        ("0", "123456789"),
        ("0", "1234.5678"),
        ("0", "1.5e-7"),
        ("0", "100000"),
        ("0", "999999.9"),
        ("1000.5", "2000000.25"),
        ("-5", "-0.5"),
        ("0", "1e100"),
        ("-1000000", "-12.5"),
        ("0", "0.1"),
    ] {
        check(
            &format!(r#"{{"properties":{{"num":{{"maximum":{max}}}}}}}"#),
            &format!(r#"{{"num": {v}}}"#),
        );
    }
    check(r#"{"properties":{"l":{"minItems":1234}}}"#, r#"{"l": []}"#);
}

#[test]
fn schema_patterns_match_helm() {
    let cases: &[(&str, &str)] = &[
        (r#"{"properties":{"a":{"pattern":"^z"}}}"#, r#"{"a": "ab"}"#),
        (
            r#"{"properties":{"a":{"pattern":"^a\\d+$"}}}"#,
            r#"{"a": "a12x"}"#,
        ),
        (r#"{"properties":{"a":{"pattern":"b"}}}"#, r#"{"a": "abc"}"#),
        (
            r#"{"properties":{"a":{"pattern":"it's"}}}"#,
            r#"{"a": "x"}"#,
        ),
        (
            r#"{"patternProperties":{"^x-":{"type":"integer"}, "-y$":{"type":"string"}},"additionalProperties":false}"#,
            r#"{"x-a": 1, "b": 2, "x-q": "s"}"#,
        ),
        (
            r#"{"patternProperties":{"^x-":{"type":"integer"}},"properties":{"x-a":{"type":"string"}}}"#,
            r#"{"x-a": 1}"#,
        ),
        (
            r#"{"patternProperties":{".*":{"type":"integer"}},"additionalProperties":{"type":"string"}}"#,
            r#"{"q": true}"#,
        ),
        (
            r#"{"properties":{"ports":{"type":"object","patternProperties":{"^[a-z0-9-]+$":{"properties":{"containerPort":{"type":"integer"}}}}}}}"#,
            r#"{"ports": {"jaeger-thrift": {"containerPort": "x"}}}"#,
        ),
        // Invalid regexes are caught by the metaschema (format: regex) or the compiler.
        (r#"{"pattern":"("}"#, r#"{"a": 1}"#),
        (r#"{"properties":{"a":{"pattern":"("}}}"#, r#"{"a": "ab"}"#),
        (
            r#"{"properties":{"a":{"patternProperties":{"[":{}}}}}"#,
            r#"{"a": "ab"}"#,
        ),
        (
            r#"{"$defs":{"x":{"items":{"pattern":"("}}}}"#,
            r#"{"a": 1}"#,
        ),
        (r#"{"anyOf":[{"not":{"pattern":"("}}]}"#, r#"{"a": 1}"#),
        (r#"{"definitions":{"x":{"pattern":"("}}}"#, r#"{"a": 1}"#),
        (
            r#"{"properties":{"a":{"pattern":"(", "maxLength": 1}}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"a":{"pattern":"("}}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"http://json-schema.org/draft-07/schema#","definitions":{"x":{"patternProperties":{"(":{}}}}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"http://json-schema.org/draft-07/schema#","items":{"pattern":"("}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"http://json-schema.org/draft-07/schema#","dependencies":{"a":{"pattern":"("}}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"http://json-schema.org/draft-04/schema#","properties":{"a":{"pattern":"("}}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"http://json-schema.org/draft-04/schema#","properties":{"a":{"patternProperties":{"(":{}}}}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"http://json-schema.org/draft-04/schema#","additionalProperties":{"pattern":"("}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"https://json-schema.org/draft/2019-09/schema","properties":{"a":{"pattern":"("}}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"https://json-schema.org/draft/2019-09/schema","items":{"pattern":"("}}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"http://json-schema.org/draft-06/schema#","allOf":[{"pattern":"("}]}"#,
            r#"{"a": 1}"#,
        ),
        (
            r#"{"$schema":"http://json-schema.org/draft-07/schema#","$defs":{"x":{"pattern":"("}}}"#,
            r#"{"a": 1}"#,
        ),
    ];
    for (s, v) in cases {
        check(s, v);
    }
}

#[test]
fn schema_formats_match_helm() {
    // Formats are asserted for drafts 4-7 only.
    let cases: &[(&str, &[&str])] = &[
        (
            "email",
            &[
                "x",
                "a@b.c",
                "a..b@c",
                ".a@b",
                "a.@b",
                "\"q\"@b",
                "a@[1.2.3.4]",
                "a@[IPv6:::1]",
                "a@[1.2.3]",
                "a@-b",
                "a@b_c",
                "a b@c",
                "a@[IPv6:x]",
            ],
        ),
        (
            "date",
            &[
                "2020-01-01",
                "2020-13-01",
                "2020-02-30",
                "2020-02-29",
                "2021-02-29",
                "x",
                "2020-1-01",
                "2020-01-1",
                "2020/01/01",
                "2020-01-01x",
                "202a-01-01",
                "",
                "2020-00-10",
                "2020-01-00",
            ],
        ),
        (
            "time",
            &[
                "12:00:00Z",
                "12:00:00",
                "25:00:00Z",
                "12:00:00.5+01:00",
                "12:00:00+1:00",
                "12:00:00.Z",
                "23:59:60Z",
                "12:00:60Z",
                "1a:00:00Z",
                "12-00-00Z",
                "12:00:00*01:00",
            ],
        ),
        (
            "date-time",
            &[
                "2020-01-01T25:00:00Z",
                "2020-01-01T12:00:00Z",
                "2020-01-01 12:00:00Z",
                "2020-13-01T12:00:00Z",
                "short",
            ],
        ),
        (
            "ipv4",
            &[
                "1.2.3.4",
                "1.2.x.4",
                "1.2.3",
                "01.2.3.4",
                "256.1.1.1",
                "-1.2.3.4",
                "+1.2.3.4",
                "99999999999999999999.1.1.1",
            ],
        ),
        (
            "ipv6",
            &[
                "::1",
                "1:x",
                "1.2.3.4",
                "fe80::1%eth0",
                "1::2::3",
                "12345::",
                "::ffff:1.2.3.4",
                "1:2:3:4:5:6:7:8:9",
                "1:",
                ":1",
                "::1.2.3",
            ],
        ),
        ("hostname", &["a.b", "-a", "a-", "a_b", "", "a..b", "a.b."]),
        (
            "uuid",
            &[
                "123e4567-e89b-12d3-a456-426614174000",
                "123",
                "123e4567-e89b-12d3-a456-42661417400g",
                "123e4567-e89b-12d3-a456-4266141740",
            ],
        ),
        (
            "duration",
            &[
                "P1D", "PT1H", "P", "P1W", "PW", "P1H", "PT1D", "P1D1Y", "PT", "P1", "P1DT",
            ],
        ),
        (
            "period",
            &[
                "2020-01-01T00:00:00Z/P1D",
                "P1D/2020-01-01T00:00:00Z",
                "x",
                "P1X/2020-01-01T00:00:00Z",
            ],
        ),
        (
            "uri",
            &[
                "http://a.b/c",
                "x y",
                "/rel",
                "http://[::1]:80/",
                "http://a b/",
                "http://a:x/",
                "%zz",
                "http://a/%zz",
                "://x",
                "a:b",
                "http://[1.2.3.4]/",
                "http://x@y/",
                "http://a b@c/",
            ],
        ),
        (
            "uri-reference",
            &["/a/b", "a\\b", "http://a.b", ":x", "%zz"],
        ),
        (
            "uri-template",
            &[
                "http://a/{b}",
                "http://a/{b",
                "http://a/}b{",
                "http://a/{{b}}",
            ],
        ),
        ("json-pointer", &["", "/a/b", "a", "/a~2", "/a~"]),
        ("relative-json-pointer", &["0/a", "1#", "01/a", "x", "1a"]),
        (
            "semver",
            &[
                "1.2.3",
                "1.2",
                "1.2.3-01",
                "1.2.3+",
                "01.2.3",
                "1.2.x",
                "1.2.3-a..b",
                "1.2.3+a_b",
            ],
        ),
        ("regex", &["^a$", "(", "[", "a{2,1}"]),
        ("unknown-format", &["anything"]),
    ];
    for (fmt, values) in cases {
        for v in *values {
            let schema = format!(
                r#"{{"$schema":"http://json-schema.org/draft-07/schema#","properties":{{"a":{{"format":"{fmt}"}}}}}}"#
            );
            check(&schema, &serde_json::to_string(&json!({ "a": v })).unwrap());
        }
    }
    // Not asserted in 2020-12; non-strings pass.
    check(
        r#"{"properties":{"a":{"format":"email"}}}"#,
        r#"{"a": "x"}"#,
    );
    check(
        r#"{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"a":{"format":"email"}}}"#,
        r#"{"a": 1}"#,
    );
}
