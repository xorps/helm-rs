//! `values.schema.json` validation diffed against Helm (santhosh-tekuri/jsonschema).

mod common;

use common::chart::*;
use helm_rs::chart;
use serde_json::json;

fn check(schema: &str, values: &str) {
    let Some(want) = oracle_op(json!({"op": "schema", "schema": schema, "values": values})) else { return };
    let v = json_yaml(values).unwrap();
    let m = v.as_map().cloned().unwrap_or_default();
    let got = chart::validate_against_schema(&m, schema.as_bytes());
    match (want, got) {
        (Ok(_), Ok(())) => {}
        (Err(w), Err(g)) => assert_eq!(g, w, "schema {schema}, values {values}"),
        (w, g) => panic!("schema {schema}, values {values}: oracle {w:?}, rust {g:?}"),
    }
}

#[test]
fn schema_validation_matches_helm() {
    let cases: &[(&str, &str)] = &[
        (r##"{"type":"object","properties":{"a":{"type":"integer"}}}"##, r##"{"a": "x"}"##),
        (r##"{"properties":{"a":{"type":"integer"}},"required":["b","c"]}"##, r##"{"a": 1.5}"##),
        (r##"{"properties":{"a":{"type":"integer"}}}"##, r##"{"a": 2.0}"##),
        (r##"{"properties":{"a":{"$ref":"#/$defs/x"}},"$defs":{"x":{"type":"string","minLength":3}}}"##, r##"{"a": "ab"}"##),
        (r##"{"properties":{"a":{"$ref":"#/$defs/x"}},"$defs":{"x":{"type":"string","minLength":3,"maxLength":1}}}"##, r##"{"a": "ab"}"##),
        (r##"{"properties":{"a":{"$ref":"#/definitions/x"}},"definitions":{"x":{"$ref":"#/definitions/y"},"y":{"type":"string"}}}"##, r##"{"a": 1}"##),
        (r##"{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"a":{"$ref":"#/definitions/x","type":"number"}},"definitions":{"x":{"type":"string"}}}"##, r##"{"a": 1}"##),
        (r##"{"properties":{"a":{"$ref":"#/definitions/x","type":"number"}},"definitions":{"x":{"type":"string"}}}"##, r##"{"a": true}"##),
        (r##"{"properties":{"a":{"$ref":"#foo"}},"$defs":{"x":{"$anchor":"foo","type":"string"}}}"##, r##"{"a": 1}"##),
        (r##"{"anyOf":[{"required":["a"]},{"required":["b"]}]}"##, r##"{"c": 1}"##),
        (r##"{"oneOf":[{"required":["a"]},{"required":["b"]}]}"##, r##"{"a": 1, "b": 2}"##),
        (r##"{"oneOf":[{"required":["a"]},{"required":["b"]}]}"##, r##"{"c": 1}"##),
        (r##"{"properties":{"num":{"minimum":5,"maximum":1,"multipleOf":0.1,"exclusiveMaximum":0}}}"##, r##"{"num": 3.33}"##),
        (r##"{"properties":{"num":{"multipleOf":0.01}}}"##, r##"{"num": 0.07}"##),
        (r##"{"properties":{"num":{"multipleOf":3}}}"##, r##"{"num": 1e21}"##),
        (r##"{"properties":{"num":{"minimum":1e3}}}"##, r##"{"num": 1}"##),
        (r##"{"properties":{"num":{"enum":["a",1,null,true]}}}"##, r##"{"num": 2}"##),
        (r##"{"properties":{"num":{"enum":["a",[1]]}}}"##, r##"{"num": 2}"##),
        (r##"{"properties":{"num":{"enum":[1.0]}}}"##, r##"{"num": 2}"##),
        (r##"{"properties":{"num":{"const":"a"}}}"##, r##"{"num": 2}"##),
        (r##"{"properties":{"num":{"const":{"a":1}}}}"##, r##"{"num": {"a": 1}}"##),
        (r##"{"properties":{"num":{"const":{"a":1}}}}"##, r##"{"num": {"a": 2}}"##),
        (r##"{"additionalProperties":false,"properties":{"x":{}}}"##, r##"{"a": 1, "x": 1}"##),
        (r##"{"additionalProperties":{"type":"string"}}"##, r##"{"a": 1}"##),
        (r##"{"properties":{"l":{"items":{"type":"string"},"minItems":3,"maxItems":1,"uniqueItems":true}}}"##, r##"{"l": [1, 1]}"##),
        (r##"{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"l":{"items":[{"type":"string"}],"additionalItems":false}}}"##, r##"{"l": [1, 1]}"##),
        (r##"{"properties":{"l":{"prefixItems":[{"type":"string"}],"items":{"type":"boolean"}}}}"##, r##"{"l": [1, 1]}"##),
        (r##"{"properties":{"l":{"contains":{"type":"string"}}}}"##, r##"{"l": [1, 2]}"##),
        (r##"{"properties":{"l":{"contains":{"type":"string"},"minContains":2,"maxContains":0}}}"##, r##"{"l": [1, "a"]}"##),
        (r##"{"not":{"type":"object"}}"##, r##"{"a": 1}"##),
        (r##"{"allOf":[{"required":["a"]},{"properties":{"b":{"type":"string"}}}]}"##, r##"{"b": 1}"##),
        (r##"{"$ref":"#"}"##, r##"{"a": 1}"##),
        (r##"{"properties":{"a":{"$ref":"#"}}}"##, r##"{"a": {"a": 1}}"##),
        (r##"{"type":5}"##, r##"{"a": 1}"##),
        ("{bad", r##"{"a": 1}"##),
        ("", r##"{"a": 1}"##),
        ("{} x", r##"{"a": 1}"##),
        ("[1,", r##"{"a": 1}"##),
        (r##"{"a": tru}"##, r##"{"a": 1}"##),
        (r##"{"properties":{"a":{"type":["string","null"]}}}"##, r##"{"a": 1}"##),
        (r##"{"properties":{"a/b~c":{"type":"string"}}}"##, r##"{"a/b~c": 1}"##),
        (r##"{"properties":{"a":{"properties":{"b":{"type":"string"}}}}}"##, r##"{"a": {"b": 1}}"##),
        (r##"{"if":{"required":["a"]},"then":{"required":["b"]},"else":{"required":["c"]}}"##, r##"{"a": 1}"##),
        (r##"{"if":{"required":["a"]},"then":{"required":["b"]},"else":{"required":["c"]}}"##, r##"{"x": 1}"##),
        (r##"{"dependentRequired":{"a":["b"]},"propertyNames":{"maxLength":1}}"##, r##"{"a": 1, "long": 2}"##),
        (r##"{"propertyNames":{"maxLength":1, "minLength": 9}}"##, r##"{"long": 2}"##),
        (r##"{"dependencies":{"a":["b", "c"], "d": {"required": ["e"]}}}"##, r##"{"a": 1, "d": 2}"##),
        (r##"{"dependentSchemas":{"a":{"required":["z"]}}}"##, r##"{"a": 1}"##),
        (r##"{"$schema":"http://json-schema.org/draft-04/schema#","properties":{"num":{"minimum":5,"exclusiveMinimum":true}}}"##, r##"{"num": 5}"##),
        (r##"{"minProperties":3,"maxProperties":0}"##, r##"{"num": 5}"##),
        ("false", r##"{"a": 1}"##),
        ("true", r##"{"a": 1}"##),
        (r##"{"properties":{"a":false}}"##, r##"{"a": 1}"##),
        (r##"{"properties":{"s":{"minLength":2}}}"##, r##"{"s": "é"}"##),
        (r##"{"properties":{"s":{"enum":["it's \"q\"\n"]}}}"##, r##"{"s": "x"}"##),
        (r##"{"required":["it's"]}"##, r##"{"s": "x"}"##),
    ];
    for (s, v) in cases {
        check(s, v);
    }
}

#[test]
fn schema_number_formatting_matches_helm() {
    for (max, v) in [
        ("0", "1000000"), ("0", "12345.5"), ("-1", "0.00001"), ("0", "1e21"), ("0", "123456789"), ("0", "1234.5678"),
        ("0", "1.5e-7"), ("0", "100000"), ("0", "999999.9"), ("1000.5", "2000000.25"), ("-5", "-0.5"), ("0", "1e100"),
        ("-1000000", "-12.5"), ("0", "0.1"),
    ] {
        check(&format!(r#"{{"properties":{{"num":{{"maximum":{max}}}}}}}"#), &format!(r#"{{"num": {v}}}"#));
    }
    check(r#"{"properties":{"l":{"minItems":1234}}}"#, r#"{"l": []}"#);
}
