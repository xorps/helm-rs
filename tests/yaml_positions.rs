//! Error positions (1-based line and column) reported by the YAML decoder.
use helm_rs::yaml;

fn pos(input: &str) -> (String, Option<u32>, Option<u32>) {
    let e = yaml::from_str(input).unwrap_err();
    (e.message, e.line, e.column)
}

#[test]
fn error_positions_point_at_the_problem() {
    let cases: &[(&str, u32, u32)] = &[
        // mapping value after a plain scalar on the same line: the second ':'
        ("a: 1\nb: c: d\n", 2, 5),
        // unterminated flow sequence: reported where the input ends
        ("a: [1, 2\n", 2, 1),
        // unterminated quoted scalar
        ("a: 'x\n", 2, 1),
        // unknown escape: at the backslash
        ("a: \"x\\qy\"\n", 1, 6),
        // control character (reader error): at the character
        ("a: 1\nb: \u{1}\n", 2, 4),
        ("ab: é\u{7f}\n", 1, 6),
        // unknown anchor: at the alias
        ("a: 1\nb: *nope\n", 2, 4),
        // a block sequence where a mapping key is expected
        ("a: 1\n- b\n", 2, 1),
        // invalid !!int
        ("a: 1\nb: !!int x\n", 2, 4),
        // tab indentation
        ("a:\n\tb: 1\n", 2, 1),
        // deeper document
        ("x:\n  y:\n    z: [1, 2}\n", 3, 13),
        // NaN cannot be converted to JSON: at the value
        ("a: 1\nb:\n  c: .nan\n", 3, 6),
        // top level is not a map (for map targets): at the root node
    ];
    for &(input, line, col) in cases {
        let (msg, l, c) = pos(input);
        assert_eq!((l, c), (Some(line), Some(col)), "input {input:?}: {msg}");
    }
    let e = yaml::from_str_map("\n\n  - a\n").unwrap_err();
    assert_eq!((e.line, e.column), (Some(3), Some(3)), "{}", e.message);
}
