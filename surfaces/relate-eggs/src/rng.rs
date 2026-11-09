//! A tiny xorshift generator for the eggs: toys need noise, not quality,
//! and the crate takes no dependency for it. Seeded from the clock and the
//! process id.

pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn from_clock() -> Rng {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Rng::seeded(nanos ^ (u64::from(std::process::id()) << 32))
    }

    pub(crate) fn seeded(seed: u64) -> Rng {
        // Zero is xorshift's one fixed point.
        Rng(seed | 1)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n`; `n` must be non-zero.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "Rng::below(0)");
        (self.next_u64() >> 11) as usize % n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_stays_in_range_and_varies() {
        let mut r = Rng::seeded(0);
        let xs: Vec<usize> = (0..200).map(|_| r.below(7)).collect();
        assert!(xs.iter().all(|&x| x < 7));
        assert!(xs.iter().any(|&x| x != xs[0]));
    }
}
