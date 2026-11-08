//! The one seeded PRNG the workspace's differential tests share
//! (LABS-934): deterministic, dependency-free, so a seed pins the run.

/// A seeded xorshift64*: deterministic, dependency-free.
pub struct Rng(pub u64);

impl Rng {
    /// The next word of the stream (named off `next` so it is never
    /// confused for `Iterator::next`).
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A draw from `0..n`, advancing the stream by one word. Zero policy:
    /// `below(0)` is 0 by contract (the word is still drawn), so callers
    /// never need a `max(1)` guard of their own.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// A draw from the unit interval `[0, 1)`, advancing the stream by one
    /// word: 53 bits of mantissa, so the step is the f64 resolution.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}
