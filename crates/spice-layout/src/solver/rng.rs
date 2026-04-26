//! Tiny seeded PRNG (xoshiro256**), for SA acceptance and FR jitter.
//!
//! We deliberately do not pull `rand` as a runtime dependency: the SA
//! pass only needs uniform `f64`/`u64` and a deterministic seed. A
//! ~30-line PRNG keeps the dep graph small and the determinism story
//! explicit.

#[derive(Debug, Clone)]
pub(super) struct Rng {
    s: [u64; 4],
}

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        // SplitMix64 to expand a u64 seed into four state words.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut next = || {
            z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^ (x >> 31)
        };
        let s = [next(), next(), next(), next()];
        // Avoid the all-zero state, which xoshiro cannot leave.
        let mut out = Self { s };
        if out.s == [0; 4] {
            out.s[0] = 1;
        }
        out
    }

    pub(super) fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform `f64` in `[0, 1)`.
    pub(super) fn next_f64(&mut self) -> f64 {
        // 53 high bits → fraction in [0, 1). Bits fits in 53 so the
        // u64→f64 cast is exact; the divisor is a power-of-two f64.
        let bits = self.next_u64() >> 11;
        #[allow(clippy::cast_precision_loss)]
        let v = bits as f64;
        #[allow(clippy::cast_precision_loss)]
        let denom = (1u64 << 53) as f64;
        v / denom
    }

    /// Uniform integer in `0..n` (n must be > 0).
    pub(super) fn next_below(&mut self, n: usize) -> usize {
        // Modulo bias is irrelevant for the small `n` we feed in.
        // u64→usize is at most 64 bits on 64-bit hosts; the project's
        // MSRV does not target 32-bit, so the truncation lint is
        // suppressed rather than chased.
        #[allow(clippy::cast_possible_truncation)]
        let r = self.next_u64() as usize;
        r % n
    }
}
