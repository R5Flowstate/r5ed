//! Deterministic pseudo-random source. SplitMix64 is used everywhere: it is
//! stateless to seed, reproducible across targets and cheap to derive a
//! substream from a coordinate.

/// SplitMix64 generator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform in `[0, 1)`. The top 53 bits are exactly representable in f64.
    #[inline]
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }

    /// Uniform in `[lo, hi)`.
    #[inline]
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// Uniform in `[lo, hi)` as `f32`.
    #[inline]
    pub fn range32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * (self.unit() as f32)
    }

    /// Uniform in `[0, n)`. Rejection sampling keeps the distribution exact.
    pub fn below(&mut self, n: u32) -> u32 {
        assert!(n > 0, "below(0) has no valid result");
        if n.is_power_of_two() {
            return self.next_u32() & (n - 1);
        }
        let limit = u32::MAX - (u32::MAX % n);
        loop {
            let v = self.next_u32();
            if v < limit {
                return v % n;
            }
        }
    }

    /// Standard normal via Box-Muller, drawing one value per call.
    pub fn normal(&mut self) -> f64 {
        let u1 = 1.0 - self.unit();
        let u2 = self.unit();
        let r = (-2.0 * crate::math::ln(u1)).sqrt();
        r * crate::math::cos(crate::math::TAU * u2)
    }

    /// Bernoulli trial with success probability `p`.
    #[inline]
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// Random unit vector in the plane.
    pub fn unit_vec2(&mut self) -> (f64, f64) {
        let a = self.range(0.0, crate::math::TAU);
        (crate::math::cos(a), crate::math::sin(a))
    }
}

/// Integer hashing for lattice noise lives in [`rdoc::noise`] now, so the mesh
/// modifiers sample the same lattice the terrain does.
pub use rdoc::noise::{hash1, hash2, hash3, mix64, substream, to_signed_unit, to_unit};

/// FNV-1a over raw bytes, used to hash generated data for golden tests.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_reproducible() {
        let a: Vec<u64> = (0..16).scan(Rng::new(42), |r, _| Some(r.next_u64())).collect();
        let b: Vec<u64> = (0..16).scan(Rng::new(42), |r, _| Some(r.next_u64())).collect();
        assert_eq!(a, b);
        let c: Vec<u64> = (0..16).scan(Rng::new(43), |r, _| Some(r.next_u64())).collect();
        assert_ne!(a, c);
    }

    #[test]
    fn unit_stays_in_range_and_spreads() {
        let mut r = Rng::new(7);
        let mut sum = 0.0;
        let mut buckets = [0u32; 8];
        for _ in 0..20000 {
            let u = r.unit();
            assert!((0.0..1.0).contains(&u));
            sum += u;
            buckets[(u * 8.0) as usize] += 1;
        }
        let mean = sum / 20000.0;
        assert!((mean - 0.5).abs() < 0.01, "mean was {mean}");
        for (i, b) in buckets.iter().enumerate() {
            assert!(*b > 2000, "bucket {i} was thin at {b}");
        }
    }

    #[test]
    fn below_is_uniform_and_valid() {
        let mut r = Rng::new(99);
        let mut counts = [0u32; 5];
        for _ in 0..5000 {
            let v = r.below(5);
            assert!(v < 5);
            counts[v as usize] += 1;
        }
        for c in counts {
            assert!((c as i32 - 1000).abs() < 150, "skewed bucket {c}");
        }
        assert_eq!(r.below(1), 0);
        assert!(r.below(1024) < 1024);
    }

    #[test]
    fn normal_has_zero_mean() {
        let mut r = Rng::new(1234);
        let mut sum = 0.0;
        let mut sum2 = 0.0;
        let n = 20000;
        for _ in 0..n {
            let v = r.normal();
            assert!(v.is_finite());
            sum += v;
            sum2 += v * v;
        }
        let mean = sum / n as f64;
        let var = sum2 / n as f64 - mean * mean;
        assert!(mean.abs() < 0.05, "mean was {mean}");
        assert!((var - 1.0).abs() < 0.1, "variance was {var}");
    }

    #[test]
    fn substreams_differ() {
        assert_ne!(substream(1, 0), substream(1, 1));
        assert_eq!(substream(1, 5), substream(1, 5));
        assert_ne!(hash2(1, 3, 4), hash2(1, 4, 3));
        assert_ne!(hash3(1, 3, 4, 5), hash3(1, 3, 4, 6));
        assert_ne!(hash1(1, 3), hash1(1, 4));
    }

    #[test]
    fn hashers_are_stable() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
        assert_eq!(hash_f32(&[1.0, -2.0]), hash_f32(&[1.0, -2.0]));
        assert_ne!(hash_f32(&[1.0]), hash_f32(&[1.0, 0.0]));
    }

    #[test]
    fn unit_from_hash_is_bounded() {
        for i in 0..1000i32 {
            let u = to_unit(hash1(5, i));
            assert!((0.0..1.0).contains(&u));
            let s = to_signed_unit(hash1(5, i));
            assert!((-1.0..1.0).contains(&s));
        }
    }
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hash a float array by its exact bit pattern, so equal data always agrees.
pub fn hash_f32(values: &[f32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in values {
        for b in v.to_bits().to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}
