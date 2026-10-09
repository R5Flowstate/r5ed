//! BC6H_UF16 block compression, the encoding the map cubemap texture uses.
//!
//! Every block is written in mode 11: one region, two RGB endpoints of 10 bits
//! per channel stored as-is, and a 4-bit index per texel (3 bits for texel 0,
//! whose top bit is implied zero). The layout is
//!
//! ```text
//! bits   0..4    mode, 0b00011
//! bits   5..34   endpoint 0: red, green, blue, 10 bits each
//! bits  35..64   endpoint 1: red, green, blue
//! bits  65..67   texel 0 index
//! bits  68..127  texels 1..15, 4 bits each
//! ```
//!
//! Endpoints and the palette live in the format's unquantised domain, where a
//! value `u` decodes to the half-float bit pattern `u * 31 / 64`, so the ramp
//! is linear in half-float bits rather than in light.

use glam::Vec3;

use crate::error::{Error, Result};

const MODE11: u64 = 0b00011;
const ENDPOINT_BITS: u32 = 10;
const INDEX_START: u32 = 65;
const WEIGHTS: [i32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];
/// The largest finite half float, the brightest value a texel can hold.
pub const BC6H_MAX: f32 = 65504.0;

/// Encodes 16 linear RGB texels as one BC6H_UF16 block.
pub fn encode_bc6h_block(texels: &[Vec3; 16]) -> [u8; 16] {
    let target: [[i32; 3]; 16] = std::array::from_fn(|i| {
        let t = texels[i];
        [t.x, t.y, t.z].map(|c| unquantised_of_half(half_bits(c)))
    });
    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    for t in &target {
        for c in 0..3 {
            lo[c] = lo[c].min(t[c]);
            hi[c] = hi[c].max(t[c]);
        }
    }
    let mut e0 = lo.map(quantise_endpoint);
    let mut e1 = hi.map(quantise_endpoint);
    let palette = palette_of(e0, e1);
    let mut idx = [0u8; 16];
    for (i, t) in target.iter().enumerate() {
        let mut best = (i64::MAX, 0u8);
        for (k, p) in palette.iter().enumerate() {
            let err: i64 = (0..3).map(|c| ((t[c] - p[c]) as i64).pow(2)).sum();
            if err < best.0 {
                best = (err, k as u8);
            }
        }
        idx[i] = best.1;
    }
    // Texel 0's index has only three bits; the weights are symmetric, so
    // swapping the endpoints and mirroring every index is exact.
    if idx[0] >= 8 {
        std::mem::swap(&mut e0, &mut e1);
        for v in &mut idx {
            *v = 15 - *v;
        }
    }
    let mut bits = BitWriter::default();
    bits.put(MODE11, 5);
    for e in e0.iter().chain(e1.iter()) {
        bits.put(*e as u64, ENDPOINT_BITS);
    }
    bits.put(idx[0] as u64, 3);
    for v in &idx[1..] {
        bits.put(*v as u64, 4);
    }
    bits.bytes()
}

/// Decodes a mode-11 block to linear RGB; any other mode decodes to black.
pub fn decode_bc6h_block(block: &[u8; 16]) -> [Vec3; 16] {
    let word = u128::from_le_bytes(*block);
    let field = |at: u32, n: u32| ((word >> at) & ((1u128 << n) - 1)) as i32;
    if field(0, 5) as u64 != MODE11 {
        return [Vec3::ZERO; 16];
    }
    let e0: [i32; 3] = std::array::from_fn(|c| field(5 + 10 * c as u32, 10));
    let e1: [i32; 3] = std::array::from_fn(|c| field(35 + 10 * c as u32, 10));
    let palette = palette_of(e0, e1);
    std::array::from_fn(|i| {
        let k = if i == 0 { field(INDEX_START, 3) } else { field(INDEX_START + 3 + 4 * (i as u32 - 1), 4) };
        let p = palette[k as usize];
        Vec3::new(half_value(finish(p[0])), half_value(finish(p[1])), half_value(finish(p[2])))
    })
}

/// One face of any size, one block per 4x4 area: a level smaller than a block
/// (the 2x2 and 1x1 tail of a mip chain) repeats its edge texels to fill it.
pub fn encode_bc6h_face(face: &[Vec3], width: u32, height: u32) -> Result<Vec<u8>> {
    let (w, h) = (width.max(1) as usize, height.max(1) as usize);
    if face.len() != w * h {
        return Err(Error::lighting(format!("a {w}x{h} face holds {} samples", face.len())));
    }
    let mut out = Vec::with_capacity(w.div_ceil(4) * h.div_ceil(4) * 16);
    for by in 0..h.div_ceil(4) {
        for bx in 0..w.div_ceil(4) {
            let blk: [Vec3; 16] = std::array::from_fn(|k| {
                let x = (bx * 4 + k % 4).min(w - 1);
                let y = (by * 4 + k / 4).min(h - 1);
                face[y * w + x]
            });
            out.extend_from_slice(&encode_bc6h_block(&blk));
        }
    }
    Ok(out)
}

/// Bytes one mip level of a BC6H image takes.
pub fn bc6h_level_bytes(w: u32, h: u32) -> usize {
    (w.max(1) as usize).div_ceil(4) * (h.max(1) as usize).div_ceil(4) * 16
}

fn palette_of(e0: [i32; 3], e1: [i32; 3]) -> [[i32; 3]; 16] {
    let a = e0.map(unquantise);
    let b = e1.map(unquantise);
    std::array::from_fn(|k| std::array::from_fn(|c| (a[c] * (64 - WEIGHTS[k]) + b[c] * WEIGHTS[k] + 32) >> 6))
}

fn unquantise(e: i32) -> i32 {
    match e {
        0 => 0,
        1023 => 0xFFFF,
        _ => ((e << 16) + 0x8000) >> ENDPOINT_BITS,
    }
}

/// The 10-bit endpoint whose unquantised value lies nearest `u`.
fn quantise_endpoint(u: i32) -> i32 {
    let guess = ((u - 32) as f32 / 64.0).round() as i32;
    (guess - 1..=guess + 1)
        .map(|e| e.clamp(0, 1023))
        .min_by_key(|&e| (unquantise(e) - u).abs())
        .unwrap_or(0)
}

fn finish(u: i32) -> u16 {
    ((u * 31) >> 6) as u16
}

fn unquantised_of_half(h: u16) -> i32 {
    ((h as i32 * 64) + 15) / 31
}

/// The half-float bit pattern of a non-negative value, rounded to nearest and
/// clamped to the largest finite half.
pub fn half_bits(v: f32) -> u16 {
    if v <= 0.0 {
        return 0;
    }
    if v >= BC6H_MAX {
        return 0x7BFF;
    }
    let bits = v.to_bits();
    let exp = ((bits >> 23) & 0xFF) as i32 - 127 + 15;
    let mant = bits & 0x7F_FFFF;
    if exp <= 0 {
        if exp < -10 {
            return 0;
        }
        let m = mant | 0x80_0000;
        let shift = (14 - exp) as u32;
        return ((m + (1 << (shift - 1))) >> shift) as u16;
    }
    let h = ((exp as u32) << 10) | (mant >> 13);
    let h = h + ((mant >> 12) & 1);
    h.min(0x7BFF) as u16
}

/// The value of a half-float bit pattern.
pub fn half_value(h: u16) -> f32 {
    let exp = ((h >> 10) & 0x1F) as i32;
    let m = (h & 0x3FF) as f32;
    if exp == 0 {
        m * 2f32.powi(-24)
    } else {
        (1.0 + m / 1024.0) * 2f32.powi(exp - 15)
    }
}

#[derive(Default)]
struct BitWriter {
    word: u128,
    at: u32,
}

impl BitWriter {
    fn put(&mut self, v: u64, n: u32) {
        self.word |= ((v as u128) & ((1u128 << n) - 1)) << self.at;
        self.at += n;
    }

    fn bytes(&self) -> [u8; 16] {
        self.word.to_le_bytes()
    }
}
