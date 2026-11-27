//! [`Rng`]: the environment's random numbers (sticky actions, no-op starts).

/// SplitMix64 (Steele, Lea and Flood 2014): a 64-bit state, one multiply-xorshift per draw.
/// It passes BigCrush, needs no dependency, and gives the same stream on every platform, so
/// a seed fixes an episode's sticky actions and no-ops everywhere. Not for cryptography.
///
/// One per environment, seeded from the env seed (`RetroEnv.reset(seed=...)`); it runs on
/// from one episode to the next until it is seeded again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng { state: seed }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A uniform number in [0, 1), with 53 random bits.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// A uniform integer in `0..n`, without modulo bias (Lemire 2019: multiply, and reject
    /// the few draws that would favour low values). `n == 0` panics.
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "Rng::below(0)");
        let threshold = n.wrapping_neg() % n;
        loop {
            let product = u128::from(self.next_u64()) * u128::from(n);
            if (product as u64) >= threshold {
                return (product >> 64) as u64;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_reference_splitmix64() {
        // The reference implementation's (Vigna's splitmix64.c) first outputs from seed 0.
        let mut rng = Rng::new(0);
        assert_eq!(rng.next_u64(), 0xe220_a839_7b1d_cdaf);
        assert_eq!(rng.next_u64(), 0x6e78_9e6a_a1b9_65f4);
        assert_eq!(rng.next_u64(), 0x06c4_5d18_8009_454f);
    }

    #[test]
    fn next_f64_is_in_the_unit_interval() {
        let mut rng = Rng::new(7);
        let draws: Vec<f64> = (0..10_000).map(|_| rng.next_f64()).collect();
        assert!(draws.iter().all(|&x| (0.0..1.0).contains(&x)));
        let mean = draws.iter().sum::<f64>() / draws.len() as f64;
        // The mean of 10,000 uniforms has a standard deviation of 0.0029: 5 sigma.
        assert!((mean - 0.5).abs() < 0.015, "mean {mean}");
    }

    #[test]
    fn below_covers_its_range_uniformly() {
        let mut rng = Rng::new(1);
        let mut counts = [0u32; 7];
        for _ in 0..70_000 {
            counts[rng.below(7) as usize] += 1;
        }
        // 10,000 expected per value, standard deviation 93: 5 sigma.
        for (value, &count) in counts.iter().enumerate() {
            assert!(count.abs_diff(10_000) < 465, "{value}: {count}");
        }
        assert_eq!(Rng::new(3).below(1), 0);
        let mut rng = Rng::new(3);
        assert!((0..1000).all(|_| rng.below(u64::MAX) < u64::MAX));
    }

    #[test]
    fn a_seed_fixes_the_stream() {
        let (mut a, mut b) = (Rng::new(42), Rng::new(42));
        assert!((0..100).all(|_| a.next_u64() == b.next_u64()));
        assert_ne!(Rng::new(42).next_u64(), Rng::new(43).next_u64());
    }
}
