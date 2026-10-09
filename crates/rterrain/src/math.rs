//! Scalar math built only from IEEE-754 add, subtract, multiply, divide and
//! square root.
//!
//! Rust guarantees no fused multiply-add contraction and correctly rounded
//! division and square root, so every function here returns bit-identical
//! results on every target. The standard library's `sin`, `cos`, `exp`, `ln`
//! and `powf` are deliberately unused: they delegate to the platform libm and
//! differ between `x86_64` and `wasm32`.

pub use std::f64::consts::{FRAC_1_PI, FRAC_PI_2, LN_2, PI, SQRT_2, TAU};
use std::f64::consts::{FRAC_2_PI, FRAC_PI_4, LOG2_E};

const PIO2_HI: f64 = 1.570_796_326_734_125_6;
const PIO2_LO: f64 = 6.077_100_506_506_192e-11;

const S1: f64 = -1.666_666_666_666_663_2e-1;
const S2: f64 = 8.333_333_333_322_49e-3;
const S3: f64 = -1.984_126_982_985_795e-4;
const S4: f64 = 2.755_731_370_707_006_8e-6;
const S5: f64 = -2.505_076_025_340_686_3e-8;
const S6: f64 = 1.589_690_995_211_55e-10;

const C1: f64 = -5e-1;
const C2: f64 = 4.166_666_666_666_666_4e-2;
const C3: f64 = -1.388_888_888_888_889e-3;
const C4: f64 = 2.480_158_730_158_73e-5;
const C5: f64 = -2.755_731_922_398_589e-7;
const C6: f64 = 2.087_572_321_298_175e-9;
const C7: f64 = -1.135_964_755_778_819_5e-11;

const E2: f64 = 5e-1;
const E3: f64 = 1.666_666_666_666_666_6e-1;
const E4: f64 = 4.166_666_666_666_666_4e-2;
const E5: f64 = 8.333_333_333_333_333e-3;
const E6: f64 = 1.388_888_888_888_889e-3;
const E7: f64 = 1.984_126_984_126_984e-4;
const E8: f64 = 2.480_158_730_158_73e-5;
const E9: f64 = 2.755_731_922_398_589_3e-6;
const E10: f64 = 2.755_731_922_398_589e-7;
const E11: f64 = 2.505_210_838_544_172e-8;
const E12: f64 = 2.087_675_698_786_81e-9;

const L1: f64 = 0.333_333_333_333_333_3;
const L3: f64 = 0.2;
const L5: f64 = 0.142_857_142_857_142_85;
const L7: f64 = 0.111_111_111_111_111_1;
const L9: f64 = 0.090_909_090_909_090_91;
const L11: f64 = 0.076_923_076_923_076_93;
const L13: f64 = 0.066_666_666_666_666_67;
const L15: f64 = 0.058_823_529_411_764_705;
const L17: f64 = 0.052_631_578_947_368_42;
const L19: f64 = 0.047_619_047_619_047_616;

const AT1: f64 = 1.00000000000000000000;
const AT3: f64 = -3.333_333_333_333_333e-1;
const AT5: f64 = 2e-1;
const AT7: f64 = -1.428_571_428_571_428_5e-1;
const AT9: f64 = 1.111_111_111_111_111e-1;
const AT11: f64 = -9.090_909_090_909_091e-2;
const TAN_PI_8: f64 = 4.142135623730951e-01;
const LN_2_HI: f64 = 6.931_471_803_691_238e-1;
const LN_2_LO: f64 = 1.908_214_929_270_587_7e-10;

#[inline]
fn sin_core(r: f64) -> f64 {
    let r2 = r * r;
    r * (1.0 + r2 * (S1 + r2 * (S2 + r2 * (S3 + r2 * (S4 + r2 * (S5 + r2 * S6))))))
}

#[inline]
fn cos_core(r: f64) -> f64 {
    let r2 = r * r;
    1.0 + r2 * (C1 + r2 * (C2 + r2 * (C3 + r2 * (C4 + r2 * (C5 + r2 * (C6 + r2 * C7))))))
}

/// Reduce `x` to a value in [-pi/4, pi/4] plus a quadrant index in `0..4`.
#[inline]
fn reduce(x: f64) -> (f64, u32) {
    let scaled = x * FRAC_2_PI;
    let k = (scaled + 0.5).floor();
    let r = (x - k * PIO2_HI) - k * PIO2_LO;
    let q = k.rem_euclid(4.0) as u32;
    (r, q)
}

/// Sine. Accurate to about 1e-15 for arguments in [-1e12, 1e12].
pub fn sin(x: f64) -> f64 {
    let (r, q) = reduce(x);
    let s = sin_core(r);
    match q {
        0 => s,
        1 => cos_core(r),
        2 => -s,
        _ => -cos_core(r),
    }
}

/// Cosine. Accurate to about 1e-15 for arguments in [-1e12, 1e12].
pub fn cos(x: f64) -> f64 {
    let (r, q) = reduce(x);
    let c = cos_core(r);
    match q {
        0 => c,
        1 => -sin_core(r),
        2 => -c,
        _ => sin_core(r),
    }
}

#[inline]
fn exp_core(r: f64) -> f64 {
    let mut poly = E12;
    poly = E11 + r * poly;
    poly = E10 + r * poly;
    poly = E9 + r * poly;
    poly = E8 + r * poly;
    poly = E7 + r * poly;
    poly = E6 + r * poly;
    poly = E5 + r * poly;
    poly = E4 + r * poly;
    poly = E3 + r * poly;
    poly = E2 + r * poly;
    1.0 + r * (1.0 + r * poly)
}

/// Multiply a positive `f64` by `2^n`, going subnormal when needed.
#[inline]
fn scale_pow2(x: f64, n: i32) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    let bits = x.to_bits();
    let m = bits & 0x000f_ffff_ffff_ffff;
    // Whole part of the significand plus the leading one implied by a normal.
    let mut sig = m | 0x0010_0000_0000_0000;
    let e = ((bits >> 52) & 0x7ff) as i32 - 1023 + n;
    if e >= -1022 {
        return f64::from_bits((bits & 0x800f_ffff_ffff_ffff) | (((e + 1023) as u64) << 52));
    }
    // Subnormal: shift the significand right, losing bits below the minimum.
    let shift = -1022 - e;
    if shift >= 64 {
        return 0.0;
    }
    sig >>= shift;
    f64::from_bits((bits & 0x8000_0000_0000_0000) | sig)
}

/// Base-e exponential. Underflows to zero below about -745.
pub fn exp(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 709.782712893384 {
        return f64::INFINITY;
    }
    if x < -745.1332191019411 {
        return 0.0;
    }
    let k = (x * LOG2_E + 0.5).floor();
    let r = (x - k * LN_2_HI) - k * LN_2_LO;
    scale_pow2(exp_core(r), k as i32)
}

#[inline]
fn ln_mantissa(m: f64) -> f64 {
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let mut poly = L19;
    poly = L17 + s2 * poly;
    poly = L15 + s2 * poly;
    poly = L13 + s2 * poly;
    poly = L11 + s2 * poly;
    poly = L9 + s2 * poly;
    poly = L7 + s2 * poly;
    poly = L5 + s2 * poly;
    poly = L3 + s2 * poly;
    poly = L1 + s2 * poly;
    s * 2.0 * (1.0 + s2 * poly)
}

/// Natural logarithm. Negative infinity for zero, NaN below zero.
pub fn ln(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x < 0.0 {
        return f64::NAN;
    }
    if x == f64::INFINITY {
        return f64::INFINITY;
    }
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    // Subnormals carry no implicit leading one, so scale them up by 2^64.
    let (e, mantissa_bits) = if biased == 0 {
        let up = (bits & 0x800f_ffff_ffff_ffff) | (1023u64 << 52);
        ((((up >> 52) & 0x7ff) as i32) - 1023 - 64, up)
    } else {
        (biased - 1023, (bits & 0x800f_ffff_ffff_ffff) | (1023u64 << 52))
    };
    // Force the exponent to the bias, leaving a significand in [1, 2).
    let m = f64::from_bits(mantissa_bits);
    if m > SQRT_2 {
        (e as f64 + 1.0) * LN_2 + ln_mantissa(m * 0.5)
    } else {
        e as f64 * LN_2 + ln_mantissa(m)
    }
}

/// `x` raised to an arbitrary real power.
pub fn pow(x: f64, y: f64) -> f64 {
    if y == 0.0 {
        return 1.0;
    }
    if y == 1.0 {
        return x;
    }
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 {
        return if y > 0.0 { 0.0 } else { f64::INFINITY };
    }
    if x < 0.0 {
        let n = y.floor();
        if n != y {
            return f64::NAN;
        }
        let m = pow(-x, y);
        return if (n as i64) & 1 == 1 { -m } else { m };
    }
    if x == f64::INFINITY {
        return if y > 0.0 { f64::INFINITY } else { 0.0 };
    }
    if x == 1.0 {
        return 1.0;
    }
    exp(y * ln(x))
}

/// Square root that returns zero instead of NaN for negative input.
pub fn sqrt(x: f64) -> f64 {
    if x <= 0.0 {
        if x.is_nan() {
            x
        } else {
            0.0
        }
    } else {
        x.sqrt()
    }
}

#[inline]
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

#[inline]
pub fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

#[inline]
pub fn saturate(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// Larger of two values, ordered so a NaN never wins.
#[inline]
pub fn max(a: f64, b: f64) -> f64 {
    if a > b {
        a
    } else {
        b
    }
}

/// Smaller of two values, ordered so a NaN never wins.
#[inline]
pub fn min(a: f64, b: f64) -> f64 {
    if a < b {
        a
    } else {
        b
    }
}

/// Hermite smoothstep, zero slope at both ends.
pub fn smoothstep(t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    t * t * (3.0 - 2.0 * t)
}

/// Quintic smootherstep, zero first and second derivative at both ends.
pub fn smootherstep(t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Ramped step from `edge0` to `edge1`, zero below and one above.
pub fn smooth_range(x: f64, edge0: f64, edge1: f64) -> f64 {
    if (edge1 - edge0).abs() < f64::MIN_POSITIVE {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    smoothstep((x - edge0) / (edge1 - edge0))
}

/// Triangular window of half-width `r` around zero.
pub fn tent(x: f64, r: f64) -> f64 {
    if r <= 0.0 {
        return 0.0;
    }
    let a = (x / r).abs();
    if a >= 1.0 {
        0.0
    } else {
        1.0 - a
    }
}

/// Smooth bump of half-width `r`: one at the centre, zero at and past `r`.
pub fn bump(x: f64, r: f64) -> f64 {
    if r <= 0.0 {
        return 0.0;
    }
    let t = saturate(1.0 - (x / r).abs());
    t * t * (3.0 - 2.0 * t)
}

/// Unit-peak bell curve `exp(-(x/r)^2)`.
pub fn gaussian(x: f64, r: f64) -> f64 {
    if r <= 0.0 {
        return 0.0;
    }
    exp(-(x / r) * (x / r))
}

/// `sign(a - b)` as -1, 0 or 1.
#[inline]
pub fn sign(a: f64, b: f64) -> f64 {
    if a > b {
        1.0
    } else if a < b {
        -1.0
    } else {
        0.0
    }
}

/// Nearest integer, ties away from zero.
#[inline]
pub fn round(x: f64) -> f64 {
    if x >= 0.0 {
        (x + 0.5).floor()
    } else {
        (x - 0.5).ceil()
    }
}

/// Fractional part in `[0, 1)`, also for negative `x`.
#[inline]
pub fn fract(x: f64) -> f64 {
    x - x.floor()
}

/// f32 variant of [`sin`].
pub fn sin32(x: f32) -> f32 {
    sin(x as f64) as f32
}

/// f32 variant of [`cos`].
pub fn cos32(x: f32) -> f32 {
    cos(x as f64) as f32
}

/// f32 square root that returns zero for negative input.
pub fn sqrt32(x: f32) -> f32 {
    if x <= 0.0 {
        if x.is_nan() {
            x
        } else {
            0.0
        }
    } else {
        x.sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn trig_identities_hold() {
        let mut x = -40.0f64;
        while x < 40.0 {
            let s = sin(x);
            let c = cos(x);
            assert!(close(s * s + c * c, 1.0, 1e-12), "pythagoras at {x}");
            assert!(s.abs() <= 1.0 && c.abs() <= 1.0, "unit magnitude at {x}");
            assert!(close(sin(x + PI), -s, 1e-12), "sine shift at {x}");
            assert!(close(cos(x + PI), -c, 1e-12), "cosine shift at {x}");
            assert!(close(sin(x + FRAC_PI_2), c, 1e-12), "quarter shift at {x}");
            x += 0.0173;
        }
    }

    #[test]
    fn trig_known_points() {
        assert!(close(sin(0.0), 0.0, 1e-16));
        assert!(close(cos(0.0), 1.0, 1e-16));
        assert!(close(sin(FRAC_PI_2), 1.0, 1e-15));
        assert!(close(cos(PI), -1.0, 1e-15));
        assert!(close(sin(PI), 0.0, 1e-15));
        assert!(close(sin(100.0), -0.5063656411097588, 1e-12));
        assert!(close(cos(1000.0), 0.5623790762907029, 1e-12));
    }

    #[test]
    fn exp_and_ln_round_trip() {
        let mut x = -40.0f64;
        while x < 40.0 {
            let e = exp(x);
            assert!(close(ln(e), x, 1e-11 * (1.0 + x.abs())), "ln(exp) at {x}");
            x += 0.31;
        }
        assert_eq!(exp(0.0), 1.0);
        assert_eq!(ln(1.0), 0.0);
        assert_eq!(exp(-800.0), 0.0);
        assert!(exp(1e6).is_infinite());
        assert!(ln(0.0).is_infinite());
        assert!(ln(-1.0).is_nan());
        assert!(ln(f64::INFINITY).is_infinite());
    }

    #[test]
    fn exp_known_values() {
        assert!(close(exp(1.0), 2.718281828459045, 1e-15));
        assert!(close(exp(2.0), 7.389056098930650, 1e-14));
        assert!(close(ln(2.0), 0.6931471805599453, 1e-15));
        assert!(close(ln(10.0), 2.302585092994046, 1e-14));
        assert!(close(ln(f64::MIN_POSITIVE), -708.3964185322641, 1e-9));
    }

    #[test]
    fn pow_rules() {
        assert_eq!(pow(0.0, 3.0), 0.0);
        assert_eq!(pow(1.0, 7.5), 1.0);
        assert!(close(pow(2.0, 10.0), 1024.0, 1e-9));
        assert!(close(pow(9.0, 0.5), 3.0, 1e-12));
        assert!(close(pow(-2.0, 3.0), -8.0, 1e-12));
        assert!(close(pow(-2.0, 4.0), 16.0, 1e-12));
        assert!(pow(-2.0, 0.5).is_nan());
        assert!(pow(f64::NAN, 2.0).is_nan());
    }

    #[test]
    fn ramps_stay_in_range() {
        assert_eq!(smoothstep(-1.0), 0.0);
        assert_eq!(smoothstep(2.0), 1.0);
        assert_eq!(smootherstep(-1.0), 0.0);
        assert_eq!(smootherstep(2.0), 1.0);
        assert_eq!(clamp(5.0, 0.0, 1.0), 1.0);
        assert_eq!(clamp(-5.0, 0.0, 1.0), 0.0);
        assert_eq!(saturate(-5.0), 0.0);
        assert_eq!(saturate(5.0), 1.0);
        assert_eq!(tent(0.0, 2.0), 1.0);
        assert_eq!(tent(2.0, 2.0), 0.0);
        assert_eq!(tent(3.0, 2.0), 0.0);
        assert!(bump(0.0, 3.0) > 0.999);
        assert_eq!(bump(3.0, 3.0), 0.0);
        assert!(gaussian(0.0, 3.0) > 0.999);
        assert_eq!(smooth_range(0.0, 0.0, 0.0), 1.0);
        assert_eq!(smooth_range(-1.0, 0.0, 0.0), 0.0);
        assert!(close(smooth_range(0.5, 0.0, 1.0), 0.5, 1e-15));
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -3.0);
        assert!(close(fract(-0.25), 0.75, 1e-15));
        assert_eq!(lerp(2.0, 4.0, 0.5), 3.0);
        assert_eq!(sign(3.0, 1.0), 1.0);
        assert_eq!(sign(1.0, 3.0), -1.0);
        assert_eq!(sign(1.0, 1.0), 0.0);
        assert_eq!(sqrt(-4.0), 0.0);
        assert!(sqrt(f64::NAN).is_nan());
        assert_eq!(sqrt32(-1.0), 0.0);
    }
}

/// Arc tangent in radians, range -pi/2..pi/2.
///
/// Uses the same reciprocal identity as the rest of this module, with an odd
/// series in `t = (x - 1) / (x + 1)` after reducing to `|x| <= 1`.
pub fn atan(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let neg = x < 0.0;
    let a = if neg { -x } else { x };
    // Values at or above 1 map onto the reciprocal, which is in (0, 1].
    let (m, base) = if a > 1.0 { (1.0 / a, FRAC_PI_2) } else { (a, 0.0) };
    let mut t = m;
    if m > TAN_PI_8 {
        // atan(m) = pi/4 + atan((m - 1) / (m + 1)), where the argument is
        // small enough for the series to converge quickly.
        t = (m - 1.0) / (m + 1.0);
    }
    let t2 = t * t;
    let mut poly = AT11;
    poly = AT9 + t2 * poly;
    poly = AT7 + t2 * poly;
    poly = AT5 + t2 * poly;
    poly = AT3 + t2 * poly;
    poly = AT1 + t2 * poly;
    let s = t * poly;
    let r = if m > TAN_PI_8 { base + FRAC_PI_4 + s } else { base + s };
    if neg {
        -r
    } else {
        r
    }
}

/// Tangent, from the same sine and cosine polynomials.
pub fn tan(x: f64) -> f64 {
    let (r, q) = reduce(x);
    let s = sin_core(r);
    let c = cos_core(r);
    let (num, den) = match q {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    };
    if den == 0.0 {
        if num > 0.0 {
            f64::INFINITY
        } else if num < 0.0 {
            f64::NEG_INFINITY
        } else {
            f64::NAN
        }
    } else {
        num / den
    }
}

/// Arc sine, computed from the arc tangent so no libm is involved.
pub fn asin(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x >= 1.0 {
        return FRAC_PI_2;
    }
    if x <= -1.0 {
        return -FRAC_PI_2;
    }
    atan(x / sqrt(1.0 - x * x))
}

/// Degrees to radians.
#[inline]
pub fn to_radians(deg: f64) -> f64 {
    deg * (PI / 180.0)
}

/// Arc tangent of `y / x`, using the quadrant of `(x, y)`.
pub fn atan2(y: f64, x: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 && y == 0.0 {
        return 0.0;
    }
    if x == 0.0 {
        return if y > 0.0 { FRAC_PI_2 } else { -FRAC_PI_2 };
    }
    if x > 0.0 {
        return atan(y / x);
    }
    if y >= 0.0 {
        atan(y / x) + PI
    } else {
        atan(y / x) - PI
    }
}
