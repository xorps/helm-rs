//! Unit tests for the test harness helpers in `tests/common`.

mod common;

use common::diff::{self, ErrLoc, Op};
use common::rng::Rng;

#[test]
fn myers_diff_is_minimal_and_valid() {
    let mut rng = Rng::new(1);
    for _ in 0..300 {
        let a: Vec<u8> = (0..rng.below(30)).map(|_| rng.below(4) as u8).collect();
        let b: Vec<u8> = (0..rng.below(30)).map(|_| rng.below(4) as u8).collect();
        let ops = diff::diff(&a, &b);
        // Replaying the script turns a into b.
        let (mut ra, mut rb) = (Vec::new(), Vec::new());
        for op in &ops {
            match *op {
                Op::Eq(i, j) => {
                    assert_eq!(a[i], b[j]);
                    ra.push(a[i]);
                    rb.push(b[j]);
                }
                Op::Del(i) => ra.push(a[i]),
                Op::Ins(j) => rb.push(b[j]),
            }
        }
        assert_eq!(ra, a);
        assert_eq!(rb, b);
        // Minimal: number of equal lines == LCS length.
        let eq = ops.iter().filter(|o| matches!(o, Op::Eq(..))).count();
        assert_eq!(eq, lcs(&a, &b), "{a:?} {b:?}");
    }
}

fn lcs(a: &[u8], b: &[u8]) -> usize {
    let mut t = vec![vec![0; b.len() + 1]; a.len() + 1];
    for i in 0..a.len() {
        for j in 0..b.len() {
            t[i + 1][j + 1] = if a[i] == b[j] { t[i][j] + 1 } else { t[i][j + 1].max(t[i + 1][j]) };
        }
    }
    t[a.len()][b.len()]
}

#[test]
fn unified_diff_format() {
    let d = diff::unified("a", "x\ny\nz\n", "b", "x\nY\nz\n", 100);
    assert_eq!(d, "--- a\n+++ b\n@@ -1,3 +1,3 @@\n x\n-y\n+Y\n z\n");
    assert_eq!(diff::unified("a", "same\n", "b", "same\n", 100), "");
    assert!(diff::unified("a", "x", "b", "x\n", 100).contains("No newline"));
}

#[test]
fn volatile_lines_are_masked() {
    let a = "kind: Secret\npassword: AAAA\nname: x\n";
    let b = "kind: Secret\npassword: BBBB\nname: x\n";
    let vol = diff::volatile_lines(a, b);
    assert_eq!(vol.into_iter().collect::<Vec<_>>(), vec![1]);
    let vol = diff::volatile_lines(a, b);
    assert!(diff::eq_modulo_volatile(a, &vol, "kind: Secret\npassword: CCCC\nname: x\n"));
    assert!(diff::eq_modulo_volatile(a, &vol, "kind: Secret\npassword: C\nC\nname: x\n"));
    assert!(!diff::eq_modulo_volatile(a, &vol, "kind: Secret\npassword: CCCC\nname: y\n"));
    assert!(!diff::eq_modulo_volatile(a, &vol, "extra: 1\nkind: Secret\npassword: CCCC\nname: x\n"));
    assert!(!diff::eq_modulo_volatile(a, &Default::default(), b));
}

#[test]
fn error_locations_are_extracted() {
    let msg = r#"template: t/templates/t.yaml:3:14: executing "t/templates/t.yaml" at <include "x" .>: error calling include: template: t/templates/_h.tpl:2:5: executing "x" at <.a.b>: nil pointer evaluating interface {}.b"#;
    let locs = diff::error_locations(msg);
    assert_eq!(
        locs,
        vec![
            ErrLoc { name: "t/templates/t.yaml".into(), line: 3, col: Some(14) },
            ErrLoc { name: "t/templates/_h.tpl".into(), line: 2, col: Some(5) },
        ]
    );
    let locs = diff::error_locations("parse error at (t/templates/t.yaml:7): unexpected EOF");
    assert_eq!(locs, vec![ErrLoc { name: "t/templates/t.yaml".into(), line: 7, col: None }]);
    let locs = diff::error_locations("execution error at (loki/templates/validate.yaml:40:4): boom");
    assert_eq!(locs[0].to_string(), "loki/templates/validate.yaml:40:4");
    assert!(diff::error_locations("see https://example.com:443/x").is_empty());
}

#[test]
fn rng_is_reproducible() {
    let a: Vec<u64> = { let mut r = Rng::new(42); (0..5).map(|_| r.next_u64()).collect() };
    let b: Vec<u64> = { let mut r = Rng::new(42); (0..5).map(|_| r.next_u64()).collect() };
    assert_eq!(a, b);
    assert_ne!(Rng::fork(1, "a").next_u64(), Rng::fork(1, "b").next_u64());
}
