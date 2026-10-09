//! Seeded gradient noise shared by terrain recipes and mesh modifiers.
//!
//! Moved down from `rterrain` so both use one implementation: `rterrain`
//! re-exports this module. Every function is a pure function of its inputs
//! and the seed, using only IEEE add, subtract, multiply, divide and square
//! root, so results are identical on every target.

use serde::{Deserialize, Serialize};

/// Mix a seed with a stream index so independent substreams never correlate.
#[inline]
pub fn substream(seed: u64, stream: u64) -> u64 {
    let mut z = seed ^ stream.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Hash two integers with a seed, for lattice noise lookups.
#[inline]
pub fn hash2(seed: u64, x: i32, y: i32) -> u64 {
    let mut h = seed ^ (x as u64).wrapping_mul(0x8da6_b343_5d8f_d1b3);
    h = h.rotate_left(31).wrapping_mul(0xd6e8_feb8_6659_fd93);
    h ^= (y as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
    mix64(h)
}

/// Hash three integers with a seed.
#[inline]
pub fn hash3(seed: u64, x: i32, y: i32, z: i32) -> u64 {
    let mut h = hash2(seed, x, y);
    h ^= (z as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    mix64(h)
}

/// Hash one integer with a seed.
#[inline]
pub fn hash1(seed: u64, x: i32) -> u64 {
    mix64(seed ^ (x as u64).wrapping_mul(0x8da6_b343_5d8f_d1b3))
}

/// Finalizer of the MurmurHash3 mixer.
#[inline]
pub fn mix64(mut z: u64) -> u64 {
    z ^= z >> 33;
    z = z.wrapping_mul(0xff51_afd7_ed55_8ccd);
    z ^= z >> 33;
    z = z.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    z ^ (z >> 33)
}

/// Hash a 64-bit value to a uniform f64 in `[0, 1)`.
#[inline]
pub fn to_unit(h: u64) -> f64 {
    (h >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
}

/// Hash a 64-bit value to a uniform f64 in `[-1, 1)`.
#[inline]
pub fn to_signed_unit(h: u64) -> f64 {
    to_unit(h) * 2.0 - 1.0
}

/// The noise family to evaluate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NoiseKind {
    #[default]
    Perlin,
    Simplex,
    Value,
    Worley,
    WorleyF2,
    WorleyBorder,
}

impl NoiseKind {
    /// Evaluate this family at a point in noise space.
    pub fn sample(&self, n: &Noise, x: f64, y: f64) -> f64 {
        match self {
            NoiseKind::Perlin => n.perlin2(x, y),
            NoiseKind::Simplex => n.simplex2(x, y),
            NoiseKind::Value => n.value2(x, y),
            NoiseKind::Worley => n.worley2(x, y).0,
            NoiseKind::WorleyF2 => n.worley2(x, y).1,
            NoiseKind::WorleyBorder => n.worley2(x, y).1 - n.worley2(x, y).0,
        }
    }

    /// Evaluate this family at a 3D point in noise space.
    pub fn sample3(&self, n: &Noise, x: f64, y: f64, z: f64) -> f64 {
        match self {
            NoiseKind::Perlin => n.perlin3(x, y, z),
            NoiseKind::Simplex => n.simplex3(x, y, z),
            NoiseKind::Value => n.value3(x, y, z),
            NoiseKind::Worley => n.worley3(x, y, z).0,
            NoiseKind::WorleyF2 => n.worley3(x, y, z).1,
            NoiseKind::WorleyBorder => {
                let (f1, f2) = n.worley3(x, y, z);
                f2 - f1
            }
        }
    }
}

/// A seeded noise field. Holds a permutation table, which is all Perlin and
/// simplex need; value and Worley hash coordinates directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Noise {
    seed: u64,
    perm: [u8; 512],
}

const F32_FRAC_1_SQRT_2: f32 = std::f64::consts::FRAC_1_SQRT_2 as f32;

const GRAD2: [[f32; 2]; 8] = [
    [1.0, 0.0],
    [-1.0, 0.0],
    [0.0, 1.0],
    [0.0, -1.0],
    [F32_FRAC_1_SQRT_2, F32_FRAC_1_SQRT_2],
    [-F32_FRAC_1_SQRT_2, F32_FRAC_1_SQRT_2],
    [F32_FRAC_1_SQRT_2, -F32_FRAC_1_SQRT_2],
    [-F32_FRAC_1_SQRT_2, -F32_FRAC_1_SQRT_2],
];

const GRAD3: [[f32; 3]; 12] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
];

#[allow(clippy::excessive_precision)]
const SQRT_3: f64 = 1.732_050_807_568_877_2;
const F2: f64 = 0.5 * (SQRT_3 - 1.0);
const G2: f64 = (3.0 - SQRT_3) / 6.0;
const F3: f64 = 1.0 / 3.0;
const G3: f64 = 1.0 / 6.0;

#[allow(clippy::excessive_precision)]
const SIMPLEX2_CORNER_PEAK: f64 = 0.050_596_442_562_694_071;
#[allow(clippy::excessive_precision)]
const SIMPLEX3_CORNER_PEAK: f64 = 0.021_662_076_611_633_046;
const SIMPLEX2_SCALE: f64 = 1.0 / (3.0 * SIMPLEX2_CORNER_PEAK);
const SIMPLEX3_SCALE: f64 = 1.0 / (4.0 * SIMPLEX3_CORNER_PEAK);

impl Noise {
    /// Build a permutation table from a seed using SplitMix64.
    pub fn new(seed: u64) -> Self {
        let mut p = [0u8; 256];
        for (i, slot) in p.iter_mut().enumerate() {
            *slot = i as u8;
        }
        let mut state = seed ^ 0x243f_6a88_85a3_08d3;
        for i in (1..256).rev() {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            let j = (z % (i as u64 + 1)) as usize;
            p.swap(i, j);
        }
        let mut perm = [0u8; 512];
        for i in 0..512 {
            perm[i] = p[i & 255];
        }
        Self { seed, perm }
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    #[inline]
    fn at(&self, x: i32, y: i32) -> u8 {
        self.perm[(self.perm[x as usize & 255] as usize + y as usize) & 255]
    }

    #[inline]
    fn at3(&self, x: i32, y: i32, z: i32) -> u8 {
        self.perm[(self.perm[(self.perm[x as usize & 255] as usize + y as usize) & 255] as usize + z as usize) & 255]
    }
}

#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

impl Noise {
    pub fn perlin2(&self, x: f64, y: f64) -> f64 {
        let (xi, yi) = (x.floor() as i64, y.floor() as i64);
        let (xf, yf) = (x - xi as f64, y - yi as f64);
        let (u, v) = (fade(xf), fade(yf));
        let (x0, y0) = (wrap_i32(xi), wrap_i32(yi));
        let (x1, y1) = (wrap_i32(xi + 1), wrap_i32(yi + 1));
        let g00 = self.grad2(x0, y0, xf, yf);
        let g10 = self.grad2(x1, y0, xf - 1.0, yf);
        let g01 = self.grad2(x0, y1, xf, yf - 1.0);
        let g11 = self.grad2(x1, y1, xf - 1.0, yf - 1.0);
        let a = g00 + u * (g10 - g00);
        let b = g01 + u * (g11 - g01);
        a + v * (b - a)
    }

    #[inline]
    fn grad2(&self, ix: i32, iy: i32, dx: f64, dy: f64) -> f64 {
        let g = GRAD2[(self.at(ix, iy) & 7) as usize];
        g[0] as f64 * dx + g[1] as f64 * dy
    }

    #[inline]
    fn corner(&self, dx: f64, dy: f64, ix: i32, iy: i32) -> f64 {
        let r = 0.5 - dx * dx - dy * dy;
        if r <= 0.0 {
            return 0.0;
        }
        r * r * self.grad2(ix, iy, dx, dy)
    }

    pub fn simplex2(&self, x: f64, y: f64) -> f64 {
        let s = (x + y) * F2;
        let (i, j) = ((x + s).floor(), (y + s).floor());
        let t = (i + j) * G2;
        let (x0, y0) = (x - (i - t), y - (j - t));
        let (i1, j1) = if t > G2 { (1.0f64, 0.0f64) } else { (0.0f64, 1.0f64) };
        let (x1, y1) = (x0 - i1 + G2, y0 - j1 + G2);
        let (x2, y2) = (x0 - 1.0 + 2.0 * G2, y0 - 1.0 + 2.0 * G2);
        let (ci, cj) = (wrap_i32(i as i64), wrap_i32(j as i64));
        let (ci1, cj1) = (wrap_i32(i as i64 + i1 as i64), wrap_i32(j as i64 + j1 as i64));
        let (ci2, cj2) = (wrap_i32(i as i64 + 1), wrap_i32(j as i64 + 1));
        SIMPLEX2_SCALE
            * (self.corner(x0, y0, ci, cj)
                + self.corner(x1, y1, ci1, cj1)
                + self.corner(x2, y2, ci2, cj2))
    }

    pub fn value2(&self, x: f64, y: f64) -> f64 {
        let (xi, yi) = (x.floor() as i64, y.floor() as i64);
        let (xf, yf) = (x - xi as f64, y - yi as f64);
        let (u, v) = (fade(xf), fade(yf));
        let (x0, y0) = (wrap_i32(xi), wrap_i32(yi));
        let (x1, y1) = (wrap_i32(xi + 1), wrap_i32(yi + 1));
        let a = to_signed_unit(hash2(self.seed, x0, y0));
        let b = to_signed_unit(hash2(self.seed, x1, y0));
        let c = to_signed_unit(hash2(self.seed, x0, y1));
        let d = to_signed_unit(hash2(self.seed, x1, y1));
        let ab = a + u * (b - a);
        let cd = c + u * (d - c);
        ab + v * (cd - ab)
    }

    pub fn perlin3(&self, x: f64, y: f64, z: f64) -> f64 {
        let (xi, yi, zi) = (x.floor() as i64, y.floor() as i64, z.floor() as i64);
        let (xf, yf, zf) = (x - xi as f64, y - yi as f64, z - zi as f64);
        let (u, v, w) = (fade(xf), fade(yf), fade(zf));
        let (x0, y0, z0) = (wrap_i32(xi), wrap_i32(yi), wrap_i32(zi));
        let (x1, y1) = (wrap_i32(xi + 1), wrap_i32(yi + 1));
        let z1 = wrap_i32(zi + 1);
        let c000 = self.grad3(x0, y0, z0, xf, yf, zf);
        let c100 = self.grad3(x1, y0, z0, xf - 1.0, yf, zf);
        let c010 = self.grad3(x0, y1, z0, xf, yf - 1.0, zf);
        let c110 = self.grad3(x1, y1, z0, xf - 1.0, yf - 1.0, zf);
        let c001 = self.grad3(x0, y0, z1, xf, yf, zf - 1.0);
        let c101 = self.grad3(x1, y0, z1, xf - 1.0, yf, zf - 1.0);
        let c011 = self.grad3(x0, y1, z1, xf, yf - 1.0, zf - 1.0);
        let c111 = self.grad3(x1, y1, z1, xf - 1.0, yf - 1.0, zf - 1.0);
        let x00 = c000 + u * (c100 - c000);
        let x10 = c010 + u * (c110 - c010);
        let x01 = c001 + u * (c101 - c001);
        let x11 = c011 + u * (c111 - c011);
        let y0v = x00 + v * (x10 - x00);
        let y1v = x01 + v * (x11 - x01);
        y0v + w * (y1v - y0v)
    }

    #[inline]
    fn grad3(&self, ix: i32, iy: i32, iz: i32, dx: f64, dy: f64, dz: f64) -> f64 {
        let g = GRAD3[(self.at3(ix, iy, iz) % 12) as usize];
        g[0] as f64 * dx + g[1] as f64 * dy + g[2] as f64 * dz
    }

    pub fn simplex3(&self, x: f64, y: f64, z: f64) -> f64 {
        let s = (x + y + z) * F3;
        let (i, j, k) = ((x + s).floor() as i64, (y + s).floor() as i64, (z + s).floor() as i64);
        let t = (i + j + k) as f64 * G3;
        let (x0, y0, z0) = (x - (i as f64 - t), y - (j as f64 - t), z - (k as f64 - t));
        let (i1, j1, k1, i2, j2, k2) = if x0 >= y0 {
            if y0 >= z0 {
                (1, 0, 0, 1, 1, 0)
            } else if x0 >= z0 {
                (1, 0, 0, 1, 0, 1)
            } else {
                (0, 0, 1, 1, 0, 1)
            }
        } else if y0 < z0 {
            (0, 0, 1, 0, 1, 1)
        } else if x0 < z0 {
            (0, 1, 0, 0, 1, 1)
        } else {
            (0, 1, 0, 1, 1, 0)
        };
        let (x1, y1, z1) = (x0 - i1 as f64 + G3, y0 - j1 as f64 + G3, z0 - k1 as f64 + G3);
        let (x2, y2, z2) = (x0 - i2 as f64 + 2.0 * G3, y0 - j2 as f64 + 2.0 * G3, z0 - k2 as f64 + 2.0 * G3);
        let (x3, y3, z3) = (x0 - 1.0 + 3.0 * G3, y0 - 1.0 + 3.0 * G3, z0 - 1.0 + 3.0 * G3);
        let (ci, cj, ck) = (wrap_i32(i), wrap_i32(j), wrap_i32(k));
        let (ci1, cj1, ck1) = (wrap_i32(i + i1 as i64), wrap_i32(j + j1 as i64), wrap_i32(k + k1 as i64));
        let (ci2, cj2, ck2) = (wrap_i32(i + i2 as i64), wrap_i32(j + j2 as i64), wrap_i32(k + k2 as i64));
        let (ci3, cj3, ck3) = (wrap_i32(i + 1), wrap_i32(j + 1), wrap_i32(k + 1));
        let corner = |dx: f64, dy: f64, dz: f64, ix: i32, iy: i32, iz: i32| -> f64 {
            let r = 0.6 - dx * dx - dy * dy - dz * dz;
            if r <= 0.0 {
                return 0.0;
            }
            let r2 = r * r;
            r2 * r2 * self.grad3(ix, iy, iz, dx, dy, dz)
        };
        SIMPLEX3_SCALE
            * (corner(x0, y0, z0, ci, cj, ck)
                + corner(x1, y1, z1, ci1, cj1, ck1)
                + corner(x2, y2, z2, ci2, cj2, ck2)
                + corner(x3, y3, z3, ci3, cj3, ck3))
    }

    pub fn value3(&self, x: f64, y: f64, z: f64) -> f64 {
        let (xi, yi, zi) = (x.floor() as i64, y.floor() as i64, z.floor() as i64);
        let (xf, yf, zf) = (x - xi as f64, y - yi as f64, z - zi as f64);
        let (u, v, w) = (fade(xf), fade(yf), fade(zf));
        let (x0, y0, z0) = (wrap_i32(xi), wrap_i32(yi), wrap_i32(zi));
        let (x1, y1) = (wrap_i32(xi + 1), wrap_i32(yi + 1));
        let z1 = wrap_i32(zi + 1);
        let l = |a: f64, b: f64, t: f64| a + t * (b - a);
        l(
            l(
                l(to_signed_unit(hash3(self.seed, x0, y0, z0)), to_signed_unit(hash3(self.seed, x1, y0, z0)), u),
                l(to_signed_unit(hash3(self.seed, x0, y1, z0)), to_signed_unit(hash3(self.seed, x1, y1, z0)), u),
                v,
            ),
            l(
                l(to_signed_unit(hash3(self.seed, x0, y0, z1)), to_signed_unit(hash3(self.seed, x1, y0, z1)), u),
                l(to_signed_unit(hash3(self.seed, x0, y1, z1)), to_signed_unit(hash3(self.seed, x1, y1, z1)), u),
                v,
            ),
            w,
        )
    }
}

#[inline]
fn wrap_i32(v: i64) -> i32 {
    (v & 0xff) as i32
}

/// Distances to the two nearest feature points in a 2D Worley cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Worley2 {
    pub f1: f64,
    pub f2: f64,
}

impl Worley2 {
    pub fn border(&self) -> f64 {
        self.f2 - self.f1
    }
}

#[inline]
fn feature3(seed: u64, cx: i32, cy: i32, cz: i32) -> (f64, f64, f64) {
    let h = hash3(seed, cx, cy, cz);
    let a = to_unit(h);
    let b = to_unit(mix64(h));
    let c = to_unit(mix64(mix64(h)));
    (cx as f64 + a, cy as f64 + b, cz as f64 + c)
}

impl Noise {
    pub fn worley2(&self, x: f64, y: f64) -> (f64, f64) {
        let (cx, cy) = (x.floor() as i64, y.floor() as i64);
        let (fx, fy) = (x - cx as f64, y - cy as f64);
        let mut f1 = f64::INFINITY;
        let mut f2 = f64::INFINITY;
        for j in -1..=1 {
            for i in -1..=1 {
                let (ix, iy) = (wrap_i32(cx + i), wrap_i32(cy + j));
                let h = hash2(self.seed, ix, iy);
                let px = i as f64 + to_unit(h);
                let py = j as f64 + to_unit(mix64(h));
                let dx = px - fx;
                let dy = py - fy;
                let d = (dx * dx + dy * dy).sqrt();
                if d < f1 {
                    f2 = f1;
                    f1 = d;
                } else if d < f2 {
                    f2 = d;
                }
            }
        }
        (f1, f2)
    }

    pub fn worley(&self, x: f64, y: f64) -> Worley2 {
        let (f1, f2) = self.worley2(x, y);
        Worley2 { f1, f2 }
    }

    pub fn worley3(&self, x: f64, y: f64, z: f64) -> (f64, f64) {
        let (cx, cy, cz) = (x.floor() as i64, y.floor() as i64, z.floor() as i64);
        let (fx, fy, fz) = (x - cx as f64, y - cy as f64, z - cz as f64);
        let mut f1 = f64::INFINITY;
        let mut f2 = f64::INFINITY;
        for k in -1..=1i64 {
            for j in -1..=1i64 {
                for i in -1..=1i64 {
                    let (px, py, pz) = feature3(self.seed, wrap_i32(cx + i), wrap_i32(cy + j), wrap_i32(cz + k));
                    let dx = px - (cx + i) as f64 - fx;
                    let dy = py - (cy + j) as f64 - fy;
                    let dz = pz - (cz + k) as f64 - fz;
                    let d = (dx * dx + dy * dy + dz * dz).sqrt();
                    if d < f1 {
                        f2 = f1;
                        f1 = d;
                    } else if d < f2 {
                        f2 = d;
                    }
                }
            }
        }
        (f1, f2)
    }

    pub fn cell01(&self, x: i32, y: i32) -> f64 {
        to_unit(hash2(self.seed, x, y))
    }

    pub fn cell_signed(&self, x: i32, y: i32) -> f64 {
        to_signed_unit(hash2(self.seed, x, y))
    }
}

/// How octave amplitudes are combined across a fractal sum.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Fractal {
    #[default]
    Fbm,
    Ridged,
    Billow,
    Hybrid,
    SwissTurbulence,
    JordanTurbulence,
}

impl Fractal {
    #[inline]
    fn shape(&self, v: f64) -> f64 {
        match self {
            Fractal::Fbm | Fractal::Hybrid => v,
            Fractal::Ridged => 1.0 - v.abs(),
            Fractal::Billow | Fractal::SwissTurbulence => v.abs(),
            Fractal::JordanTurbulence => 0.0,
        }
    }
}

/// Parameters for a fractal sum over a base noise field.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FractalParams {
    pub octaves: u32,
    pub frequency: f64,
    pub lacunarity: f64,
    pub gain: f64,
    pub offset: f64,
    pub weight: f64,
}

impl Default for FractalParams {
    fn default() -> Self {
        Self { octaves: 5, frequency: 1.0, lacunarity: 2.0, gain: 0.5, offset: 1.0, weight: 1.0 }
    }
}

impl FractalParams {
    fn sanitized(&self) -> FractalParams {
        FractalParams {
            octaves: self.octaves.clamp(1, 24),
            frequency: if self.frequency.is_finite() { self.frequency.abs().max(1e-6) } else { 1.0 },
            lacunarity: if self.lacunarity.is_finite() { self.lacunarity.abs().max(1.0) } else { 2.0 },
            gain: if self.gain.is_finite() { self.gain.clamp(0.0, 1.0) } else { 0.5 },
            offset: if self.offset.is_finite() { self.offset.abs() } else { 1.0 },
            weight: if self.weight.is_finite() { self.weight.abs() } else { 1.0 },
        }
    }
}

/// Evaluate a fractal sum of `kind` over a 2D base field.
pub fn fractal2(kind: Fractal, n: &Noise, params: &FractalParams, x: f64, y: f64) -> f64 {
    let p = params.sanitized();
    let mut freq = p.frequency;
    let mut amp = 1.0f64;
    let mut sum = 0.0f64;
    let mut norm = 0.0f64;
    let mut carried = p.weight;
    for _ in 0..p.octaves {
        let sample = n.perlin2(x * freq, y * freq);
        let shaped = kind.shape(sample);
        if kind == Fractal::Hybrid {
            let w = carried.clamp(0.0, 1.0);
            sum += shaped * w * amp;
            carried = (carried * shaped.abs() + p.offset) * 0.5;
        } else if kind == Fractal::SwissTurbulence {
            sum += shaped * shaped * amp;
        } else {
            sum += shaped * amp;
        }
        norm += amp;
        freq *= p.lacunarity;
        amp *= p.gain;
    }
    if kind == Fractal::JordanTurbulence {
        let r = sum.abs();
        return -r * r;
    }
    if norm > 0.0 { sum / norm } else { 0.0 }
}

/// Evaluate a fractal sum of `kind` over a 3D base field.
pub fn fractal3(kind: Fractal, n: &Noise, params: &FractalParams, x: f64, y: f64, z: f64) -> f64 {
    let p = params.sanitized();
    let mut freq = p.frequency;
    let mut amp = 1.0f64;
    let mut sum = 0.0f64;
    let mut norm = 0.0f64;
    let mut carried = p.weight;
    for _ in 0..p.octaves {
        let sample = n.perlin3(x * freq, y * freq, z * freq);
        let shaped = kind.shape(sample);
        match kind {
            Fractal::Hybrid => {
                let w = carried.clamp(0.0, 1.0);
                sum += shaped * w * amp;
                norm += amp;
                carried = (carried * shaped.abs() + p.offset) * 0.5;
            }
            Fractal::SwissTurbulence => {
                let a = shaped * shaped;
                sum += a * amp;
                norm += amp;
            }
            Fractal::JordanTurbulence => {
                sum += shaped * amp;
                norm += amp;
            }
            _ => {
                sum += shaped * amp;
                norm += amp;
            }
        }
        freq *= p.lacunarity;
        amp *= p.gain;
    }
    if kind == Fractal::JordanTurbulence {
        let r = sum.abs();
        return -r * r;
    }
    if norm > 0.0 { sum / norm } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_fields_are_seeded_and_bounded() {
        let a = Noise::new(1234);
        let b = Noise::new(1234);
        for i in 1..20 {
            let (x, y, z) = (i as f64 * 0.37 + 0.11, i as f64 * 0.11 + 0.29, 0.5);
            assert_eq!(a.perlin3(x, y, z), b.perlin3(x, y, z));
            assert_eq!(a.value3(x, y, z), b.value3(x, y, z));
            assert_eq!(a.simplex3(x, y, z), b.simplex3(x, y, z));
            assert_eq!(a.worley3(x, y, z), b.worley3(x, y, z));
            for v in [a.perlin3(x, y, z), a.value3(x, y, z), a.simplex3(x, y, z)] {
                assert!(v > -1.05 && v < 1.05, "out of range {v}");
            }
            let (f1, f2) = a.worley3(x, y, z);
            assert!(f1 >= 0.0 && f2 >= f1 && f1 < 2.0);
        }
        assert_eq!(NoiseKind::Perlin.sample3(&a, 1.0, 2.0, 3.0), a.perlin3(1.0, 2.0, 3.0));
        assert_eq!(NoiseKind::Value.sample(&a, 1.0, 2.0), a.value2(1.0, 2.0));
        let p = FractalParams::default();
        assert!(fractal3(Fractal::Fbm, &a, &p, 0.3, 0.7, 0.1).is_finite());
        assert_eq!(fractal3(Fractal::Fbm, &a, &p, 0.3, 0.7, 0.1), fractal3(Fractal::Fbm, &b, &p, 0.3, 0.7, 0.1));
    }
}
