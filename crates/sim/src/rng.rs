//! Deterministic PCG32 (XSH-RR) pseudo-random number generator.
//!
//! Two `Rng`s constructed with the same seed and driven with the same
//! sequence of calls always produce the same output, which is what makes
//! the whole simulation reproducible from a single `u64` seed.

const MULTIPLIER: u64 = 6364136223846793005;

pub struct Rng {
    state: u64,
    inc: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut rng = Rng {
            state: 0,
            inc: (seed << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    pub fn next_u32(&mut self) -> u32 {
        let old_state = self.state;
        self.state = old_state.wrapping_mul(MULTIPLIER).wrapping_add(self.inc);
        let xorshifted = (((old_state >> 18) ^ old_state) >> 27) as u32;
        let rot = (old_state >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform float in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / (1u32 << 24) as f32)
    }

    /// Uniform float in `[lo, hi)`.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::new(123);
        let mut b = Rng::new(123);
        for _ in 0..64 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn unit_stays_in_range() {
        let mut rng = Rng::new(7);
        for _ in 0..1000 {
            let v = rng.unit();
            assert!((0.0..1.0).contains(&v));
        }
    }
}
