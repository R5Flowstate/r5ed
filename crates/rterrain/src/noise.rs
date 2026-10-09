//! Seeded gradient noise. The field itself lives in [`rdoc::noise`] so the
//! mesh modifiers sample the same lattice the terrain does; only the
//! terrain-side combinators stay here.

pub use rdoc::noise::{fractal2, fractal3, Fractal, FractalParams, Noise, NoiseKind, Worley2};

use crate::math;

/// Domain warp: displace the sample point by a vector read from the noise
/// field itself, so features bend and swirl rather than staying axis-aligned.
pub fn domain_warp2(
    n: &Noise,
    params: &FractalParams,
    strength: f64,
    x: f64,
    y: f64,
) -> (f64, f64) {
    let qx = fractal2(Fractal::Fbm, n, params, x, y);
    let qy = fractal2(Fractal::Fbm, n, params, x + 5.2, y + 1.3);
    (x + strength * qx, y + strength * qy)
}

/// Apply domain warping `iterations` times, each reading the previous result.
pub fn domain_warp2_iter(
    n: &Noise,
    params: &FractalParams,
    strength: f64,
    iterations: u32,
    x: f64,
    y: f64,
) -> (f64, f64) {
    let (mut px, mut py) = (x, y);
    for i in 0..iterations {
        let off = i as f64 * 17.13;
        let qx = fractal2(Fractal::Fbm, n, params, px + off, py + off * 0.5);
        let qy = fractal2(Fractal::Fbm, n, params, px + 5.2 + off, py + 1.3 + off);
        px += strength * qx;
        py += strength * qy;
    }
    (px, py)
}

/// Thin networks along the zero crossings of a ridged fractal, which is how
/// drainage lines and erosion channels read on a real surface.
pub fn erosion_channels(n: &Noise, params: &FractalParams, x: f64, y: f64) -> f64 {
    let v = math::saturate(fractal2(Fractal::Ridged, n, params, x, y));
    let sq = v * v;
    sq * sq * sq
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn stats(f: impl Fn(f64, f64) -> f64) -> (f64, f64, f64) {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        let mut sum = 0.0;
        let n = 200;
        let mut count = 0.0;
        for j in 0..n {
            for i in 0..n {
                let v = f(i as f64 * 0.37, j as f64 * 0.41);
                assert!(v.is_finite(), "non-finite output {v}");
                lo = lo.min(v);
                hi = hi.max(v);
                sum += v;
                count += 1.0;
            }
        }
        (lo, hi, sum / count)
    }

    #[test]
    fn noise_is_deterministic_and_seeded() {
        let a = Noise::new(1234);
        let b = Noise::new(1234);
        let c = Noise::new(5678);
        // A sample off the lattice so no value is structurally zero.
        for i in 1..50 {
            let x = i as f64 * 0.37 + 0.11;
            let y = i as f64 * 0.11 + 0.29;
            assert_eq!(a.perlin2(x, y), b.perlin2(x, y));
            assert_eq!(a.simplex2(x, y), b.simplex2(x, y));
            assert_eq!(a.value2(x, y), b.value2(x, y));
            assert_eq!(a.perlin3(x, y, 0.5), b.perlin3(x, y, 0.5));
            assert_eq!(a.simplex3(x, y, 0.5), b.simplex3(x, y, 0.5));
            assert_eq!(a.worley2(x, y), b.worley2(x, y));
            // Different seeds must not agree.
            assert_ne!(a.perlin2(x, y), c.perlin2(x, y));
            assert_ne!(a.value2(x, y), c.value2(x, y));
            assert_ne!(a.worley2(x, y), c.worley2(x, y));
        }
        assert_eq!(a.seed(), 1234);
        assert_eq!(Noise::new(1234).perlin2(0.37, 0.11), a.perlin2(0.37, 0.11));
        assert_ne!(Noise::new(1235).perlin2(0.37, 0.11), a.perlin2(0.37, 0.11));
    }

    #[test]
    fn perlin2_is_bounded_and_correlated() {
        let n = Noise::new(7);
        let (lo, hi, mean) = stats(|x, y| n.perlin2(x, y));
        assert!(lo > -1.01 && hi < 1.01, "range {lo}..{hi}");
        assert!(mean.abs() < 0.1, "mean {mean}");
        // Nearby samples are correlated, far samples are not.
        let close = (n.perlin2(3.0, 3.0) - n.perlin2(3.05, 3.0)).abs();
        let far = (n.perlin2(3.0, 3.0) - n.perlin2(13.7, 9.1)).abs();
        assert!(close < far * 0.5, "close {close} far {far}");
    }

    #[test]
    fn perlin_is_zero_on_lattice_points() {
        let n = Noise::new(3);
        for i in -3..4 {
            for j in -3..4 {
                assert!(n.perlin2(i as f64, j as f64).abs() < 1e-12, "at {i},{j}");
            }
        }
    }

    #[test]
    fn simplex_is_bounded_and_continuous() {
        let n = Noise::new(11);
        let (lo, hi, mean) = stats(|x, y| n.simplex2(x, y));
        assert!(lo > -1.05 && hi < 1.05, "range {lo}..{hi}");
        assert!(mean.abs() < 0.12, "mean {mean}");
        // No visible jumps at lattice boundaries.
        for i in 0..40 {
            let t = i as f64 * 0.1;
            let a = n.simplex2(t, 0.5);
            let b = n.simplex2(t + 0.001, 0.5);
            assert!((a - b).abs() < 0.05, "discontinuity at {t}: {a} vs {b}");
        }
    }

    #[test]
    fn value_noise_is_bounded() {
        let n = Noise::new(5);
        let (lo, hi, mean) = stats(|x, y| n.value2(x, y));
        assert!(lo >= -1.0 && hi <= 1.0, "range {lo}..{hi}");
        assert!(mean.abs() < 0.1, "mean {mean}");
        let (lo3, hi3, _) = stats(|x, y| n.value3(x, y, 0.3));
        assert!(lo3 >= -1.0 && hi3 <= 1.0);
    }

    #[test]
    fn simplex3_is_bounded_and_nonzero() {
        let n = Noise::new(17);
        let (lo, hi, mean) = stats(|x, y| n.simplex3(x, y, 0.5));
        assert!(lo > -1.0 && hi < 1.0, "range {lo}..{hi}");
        assert!(mean.abs() < 0.15, "mean {mean}");
        // It must actually vary, not silently return a constant.
        let mut r = Rng::new(1);
        let mut distinct = std::collections::HashSet::new();
        for _ in 0..200 {
            distinct.insert(n.simplex3(r.range(-50.0, 50.0), r.range(-50.0, 50.0), r.range(-50.0, 50.0)).to_bits());
        }
        assert!(distinct.len() > 150, "only {} distinct values", distinct.len());
    }

    #[test]
    fn perlin3_is_bounded_and_zero_on_lattice() {
        let n = Noise::new(19);
        let (lo, hi, _) = stats(|x, y| n.perlin3(x, y, 0.5));
        assert!(lo > -1.05 && hi < 1.05, "range {lo}..{hi}");
        for i in -2..3 {
            for j in -2..3 {
                for k in -2..3 {
                    assert!(n.perlin3(i as f64, j as f64, k as f64).abs() < 1e-12, "at {i},{j},{k}");
                }
            }
        }
    }

    #[test]
    fn worley_distances_are_ordered() {
        let n = Noise::new(23);
        for i in 0..200 {
            let (x, y) = (i as f64 * 0.31, i as f64 * 0.17);
            let (f1, f2) = n.worley2(x, y);
            assert!(f1 >= 0.0, "negative f1 at {x},{y}");
            assert!(f2 >= f1, "f2 < f1 at {x},{y}: {f1} {f2}");
            // A feature point exists in every neighbourhood, so f1 is bounded.
            assert!(f1 < 2.0, "f1 too large at {x},{y}: {f1}");
            let w = n.worley(x, y);
            assert!((w.border() - (f2 - f1)).abs() < 1e-15);
        }
        let (a, b) = n.worley3(1.5, 2.5, 0.5);
        assert!(a >= 0.0 && b >= a && a < 2.0);
    }

    #[test]
    fn worley_families_differ() {
        let n = Noise::new(29);
        let mut distinct = [false; 3];
        for i in 0..100 {
            let (x, y) = (i as f64 * 0.13 + 0.07, i as f64 * 0.29 + 0.31);
            let w = n.worley(x, y);
            let vals = [
                NoiseKind::Worley.sample(&n, x, y),
                NoiseKind::WorleyF2.sample(&n, x, y),
                NoiseKind::WorleyBorder.sample(&n, x, y),
            ];
            assert!((vals[0] - w.f1).abs() < 1e-15);
            assert!((vals[1] - w.f2).abs() < 1e-15);
            assert!((vals[2] - w.border()).abs() < 1e-15);
            for a in 0..3 {
                for b in 0..3 {
                    if a != b && (vals[a] - vals[b]).abs() > 1e-6 {
                        distinct[a] = true;
                        distinct[b] = true;
                    }
                }
            }
        }
        assert!(distinct.iter().all(|s| *s), "families are not distinct");
    }

    #[test]
    fn noise_kind_dispatch_covers_all_families() {
        let n = Noise::new(31);
        assert_eq!(NoiseKind::Perlin.sample(&n, 1.0, 2.0), n.perlin2(1.0, 2.0));
        assert_eq!(NoiseKind::Simplex.sample(&n, 1.0, 2.0), n.simplex2(1.0, 2.0));
        assert_eq!(NoiseKind::Value.sample(&n, 1.0, 2.0), n.value2(1.0, 2.0));
        assert_eq!(NoiseKind::default(), NoiseKind::Perlin);
    }

    const ALL_FRACTALS: [Fractal; 6] = [
        Fractal::Fbm,
        Fractal::Ridged,
        Fractal::Billow,
        Fractal::Hybrid,
        Fractal::SwissTurbulence,
        Fractal::JordanTurbulence,
    ];

    #[test]
    fn fractals_are_bounded_and_deterministic() {
        let n = Noise::new(101);
        let p = FractalParams::default();
        for kind in ALL_FRACTALS {
            let (lo, hi, mean) = stats(|x, y| fractal2(kind, &n, &p, x, y));
            assert!(lo > -1.05 && hi < 1.05, "{kind:?} range {lo}..{hi}");
            assert!(mean.is_finite());
            // Same inputs, same answer, bit for bit.
            for i in 0..20 {
                let (x, y) = (i as f64 * 0.71, i as f64 * 1.31);
                assert_eq!(fractal2(kind, &n, &p, x, y), fractal2(kind, &n, &p, x, y));
                assert!(fractal3(kind, &n, &p, x, y, 0.5).is_finite());
            }
        }
    }

    #[test]
    fn fractal_shapes_have_their_own_signature() {
        let n = Noise::new(103);
        let p = FractalParams::default();
        // Ridged folds each octave, so it is never negative and its variance
        // is spread high; billow is also non-negative; plain fBm straddles 0.
        let mut negatives = [0usize; 6];
        let mut maxima = [f64::NEG_INFINITY; 6];
        for (ki, kind) in ALL_FRACTALS.iter().enumerate() {
            for j in 0..120 {
                for i in 0..120 {
                    let v = fractal2(*kind, &n, &p, i as f64 * 0.19, j as f64 * 0.23);
                    if v < 0.0 {
                        negatives[ki] += 1;
                    }
                    maxima[ki] = maxima[ki].max(v);
                }
            }
        }
        assert!(negatives[0] > 0, "fBm should straddle zero");
        // Ridged and billow are absolute-value based, so never negative.
        assert_eq!(negatives[1], 0, "ridged went negative");
        assert_eq!(negatives[2], 0, "billow went negative");
        // Jordan turbulence squares the magnitude of the whole sum, so it is
        // never positive either.
        assert!(maxima[5] <= 0.0, "jordan turbulence peaked at {}", maxima[5]);
        for (ki, kind) in ALL_FRACTALS.iter().enumerate() {
            assert!(maxima[ki].is_finite(), "{kind:?} peaked at {}", maxima[ki]);
        }
    }

    #[test]
    fn fractal_params_are_sanitized() {
        let n = Noise::new(107);
        let bad = FractalParams {
            octaves: 0,
            frequency: f64::NAN,
            lacunarity: -3.0,
            gain: f64::NAN,
            offset: f64::NAN,
            weight: f64::NAN,
        };
        let v = fractal2(Fractal::Fbm, &n, &bad, 0.3, 0.7);
        assert!(v.is_finite());
        // A huge octave count is clamped rather than looping forever.
        let big = FractalParams { octaves: u32::MAX, ..FractalParams::default() };
        assert!(fractal2(Fractal::Fbm, &n, &big, 0.3, 0.7).is_finite());
    }

    #[test]
    fn domain_warping_displaces_and_converges() {
        let n = Noise::new(109);
        let p = FractalParams::default();
        let (wx, wy) = domain_warp2(&n, &p, 0.8, 3.0, 4.0);
        assert!(wx.is_finite() && wy.is_finite());
        // Zero strength is the identity.
        let (ix, iy) = domain_warp2(&n, &p, 0.0, 3.0, 4.0);
        assert_eq!((ix, iy), (3.0, 4.0));
        // Iterating moves the point further than a single pass, and stays finite.
        let (ax, ay) = domain_warp2_iter(&n, &p, 0.5, 1, 3.0, 4.0);
        let (bx, by) = domain_warp2_iter(&n, &p, 0.5, 4, 3.0, 4.0);
        assert!(ax.is_finite() && ay.is_finite() && bx.is_finite() && by.is_finite());
        assert!((ax - bx).abs() + (ay - by).abs() > 1e-6);
        // Iteration is reproducible.
        assert_eq!(domain_warp2_iter(&n, &p, 0.5, 4, 3.0, 4.0), (bx, by));
    }

    #[test]
    fn erosion_channels_are_non_negative_and_bounded() {
        let n = Noise::new(113);
        let p = FractalParams::default();
        let mut peak = 0.0f64;
        for j in 0..80 {
            for i in 0..80 {
                let v = erosion_channels(&n, &p, i as f64 * 0.31, j as f64 * 0.17);
                assert!(v >= 0.0 && v <= 1.0 + 1e-9, "channel value {v}");
                peak = peak.max(v);
            }
        }
        assert!(peak > 0.5, "channels never peaked: {peak}");
    }

    #[test]
    fn cell_helpers_are_in_range() {
        let n = Noise::new(127);
        for i in -50..50 {
            let a = n.cell01(i, -i);
            let b = n.cell_signed(i, i);
            assert!((0.0..1.0).contains(&a));
            assert!((-1.0..1.0).contains(&b));
        }
        assert_eq!(n.cell01(3, 4), n.cell01(3, 4));
        assert_ne!(n.cell01(3, 4), n.cell01(4, 3));
    }
}
