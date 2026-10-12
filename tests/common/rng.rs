//! Small seeded PRNG and the fuzzing environment knobs.

/// SplitMix64: tiny, fast, good enough for test-case generation, and
/// reproducible across platforms.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x5DEE_CE66_D1CE_4E5B)
    }

    /// An independent stream derived from this seed and a label.
    pub fn fork(seed: u64, label: &str) -> Rng {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in label.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
        Rng::new(seed ^ h.rotate_left(17))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0);
        (self.next_u64() % n as u64) as usize
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % ((hi - lo) as u64 + 1)) as i64
    }

    /// True with probability `num/den`.
    pub fn chance(&mut self, num: u32, den: u32) -> bool {
        (self.next_u64() % den as u64) < num as u64
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    /// Picks an index according to `weights`.
    pub fn weighted(&mut self, weights: &[u32]) -> usize {
        let total: u32 = weights.iter().sum();
        let mut r = (self.next_u64() % total as u64) as u32;
        for (i, &w) in weights.iter().enumerate() {
            if r < w {
                return i;
            }
            r -= w;
        }
        weights.len() - 1
    }
}

/// `HELM_RS_FUZZ_SEED` (decimal or 0x-hex), or a fresh random seed. The seed
/// is always printed so failures can be reproduced.
pub fn fuzz_seed(test: &str) -> u64 {
    let seed = match std::env::var("HELM_RS_FUZZ_SEED") {
        Ok(s) => {
            let s = s.trim();
            s.strip_prefix("0x")
                .map(|h| u64::from_str_radix(h, 16))
                .unwrap_or_else(|| s.parse())
                .unwrap_or_else(|e| panic!("bad HELM_RS_FUZZ_SEED {s:?}: {e}"))
        }
        Err(_) => {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            (t.as_nanos() as u64) ^ ((std::process::id() as u64) << 32)
        }
    };
    eprintln!("{test}: HELM_RS_FUZZ_SEED={seed}");
    seed
}

/// `HELM_RS_FUZZ_ITERS`, or `default`.
pub fn fuzz_iters(default: usize) -> usize {
    std::env::var("HELM_RS_FUZZ_ITERS")
        .ok()
        .map(|s| s.trim().parse().unwrap_or_else(|e| panic!("bad HELM_RS_FUZZ_ITERS {s:?}: {e}")))
        .unwrap_or(default)
}

/// `HELM_RS_LAX_ERRORS=1`: when both sides fail, don't require identical
/// messages/locations.
pub fn lax_errors() -> bool {
    std::env::var("HELM_RS_LAX_ERRORS").is_ok_and(|v| !v.is_empty() && v != "0")
}
