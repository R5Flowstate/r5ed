//! BC4 / BC5 unorm block compression.
//!
//! Ported from the byte-exact algorithm the map converters use, so a block
//! produced here matches the block their output carries.

use crate::error::{Error, Result};

fn palette(r0: i32, r1: i32) -> [f32; 8] {
    let p = palette_f64(r0, r1);
    std::array::from_fn(|i| p[i] as f32)
}

/// The palette in the precision the fit is decided in: a pair of interpolated
/// entries can be a float apart at f32 and exactly equal at f64, and which of
/// them a texel rounds to has to follow the wider arithmetic.
fn palette_f64(r0: i32, r1: i32) -> [f64; 8] {
    let (r0, r1) = (r0 as f64, r1 as f64);
    let mut p = [0.0f64; 8];
    p[0] = r0;
    p[1] = r1;
    if r0 > r1 {
        for (i, slot) in p.iter_mut().enumerate().skip(2).take(6) {
            let t = i as f64;
            *slot = ((8.0 - t) * r0 + (t - 1.0) * r1) / 7.0;
        }
    } else {
        for (i, slot) in p.iter_mut().enumerate().skip(2).take(4) {
            let t = i as f64;
            *slot = ((6.0 - t) * r0 + (t - 1.0) * r1) / 5.0;
        }
        p[6] = 0.0;
        p[7] = 255.0;
    }
    p
}

/// Squared error of `vals` against the palette of `(r0, r1)`, and the index
/// chosen per texel.
fn fit(vals: &[u8; 16], r0: i32, r1: i32) -> ([u8; 16], f64) {
    let pal = palette_f64(r0, r1);
    let mut idx = [0u8; 16];
    let mut d = [0.0f64; 16];
    for (i, &v) in vals.iter().enumerate() {
        let mut best = 0usize;
        let mut best_d = f64::MAX;
        for (p, &pv) in pal.iter().enumerate() {
            let e = (v as f64 - pv).powi(2);
            if e < best_d {
                best_d = e;
                best = p;
            }
        }
        idx[i] = best as u8;
        d[i] = best_d;
    }
    // The per-block error decides a descent step, and two endpoint pairs a unit
    // apart land a float apart, so the sum is accumulated the way the
    // reference's reduction accumulates it: the upper half folded into the
    // lower, then the eight lanes added in pairs.
    let mut r = [0.0f64; 8];
    for (i, &v) in d.iter().enumerate() {
        r[i % 8] += v;
    }
    let pairs = |a: f64, b: f64, c: f64, e: f64| (a + b) + (c + e);
    (idx, pairs(r[0], r[1], r[2], r[3]) + pairs(r[4], r[5], r[6], r[7]))
}

fn clamp255(v: i32) -> i32 {
    v.clamp(0, 255)
}

/// Encodes one 4x4 block of a single channel.
pub fn encode_bc4_block(vals: &[u8; 16]) -> [u8; 8] {
    let v: [i32; 16] = std::array::from_fn(|i| vals[i] as i32);
    let lo = *v.iter().min().unwrap_or(&0);
    let hi = *v.iter().max().unwrap_or(&0);
    // Endpoints derived from the extremes that carry signal: a texel at 0 or
    // 255 is a palette constant, not a value the endpoints have to reach.
    let mut ilo = 255;
    let mut ihi = 0;
    for &x in v.iter() {
        if x > 0 && x < 255 {
            ilo = ilo.min(x);
            ihi = ihi.max(x);
        }
    }
    if ilo > ihi {
        std::mem::swap(&mut ilo, &mut ihi);
    }

    let mut best_err = f64::INFINITY;
    let mut best = (0i32, 0i32, [0u8; 16]);
    let consider = |r0: i32, r1: i32, best_err: &mut f64, best: &mut (i32, i32, [u8; 16])| {
        let (idx, err) = fit(vals, r0, r1);
        if err < *best_err {
            *best_err = err;
            *best = (r0, r1, idx);
        }
    };
    for a in 0..3i32 {
        for b in 0..3i32 {
            // A family whose endpoints cross falls back to the block's own
            // extremes, which is a candidate in its own right: a block of one
            // repeated value is fitted exactly by it, and the fallback is what
            // picks that over the stretched pair.
            let r0 = clamp255(hi - a);
            let r1 = clamp255(lo + b);
            if r0 > r1 {
                consider(r0, r1, &mut best_err, &mut best);
            } else {
                consider(hi, lo, &mut best_err, &mut best);
            }
            let s0 = clamp255(ilo + a);
            let s1 = clamp255(ihi - b);
            if s0 <= s1 {
                consider(s0, s1, &mut best_err, &mut best);
            } else {
                consider(ilo, ihi, &mut best_err, &mut best);
            }
        }
    }
    if best_err.is_infinite() {
        best = (hi, lo, [0u8; 16]);
        best_err = 0.0;
    }

    // Endpoint descent; a step never changes which palette mode is in use.
    for step in [16i32, 8, 4, 2, 1, 1] {
        if best_err <= 0.0 {
            break;
        }
        for (d0, d1) in [
            (step, 0),
            (-step, 0),
            (0, step),
            (0, -step),
            (step, step),
            (-step, -step),
            (step, -step),
            (-step, step),
        ] {
            let (c0, c1) = (best.0, best.1);
            let r0 = clamp255(c0 + d0);
            let r1 = clamp255(c1 + d1);
            if (r0 > r1) != (c0 > c1) {
                continue;
            }
            let (idx, err) = fit(vals, r0, r1);
            if err < best_err {
                best_err = err;
                best = (r0, r1, idx);
            }
        }
    }

    let mut out = [0u8; 8];
    out[0] = best.0 as u8;
    out[1] = best.1 as u8;
    let mut bits: u64 = 0;
    for (i, &ix) in best.2.iter().enumerate() {
        bits |= (ix as u64) << (3 * i);
    }
    for k in 0..6 {
        out[2 + k] = ((bits >> (8 * k)) & 0xFF) as u8;
    }
    out
}

/// Decodes one BC4 block into 16 unorm values scaled by 255.
pub fn decode_bc4_block(block: &[u8; 8]) -> [f32; 16] {
    let pal = palette(block[0] as i32, block[1] as i32);
    let mut bits: u64 = 0;
    for k in 0..6 {
        bits |= (block[2 + k] as u64) << (8 * k);
    }
    std::array::from_fn(|i| pal[((bits >> (3 * i)) & 7) as usize])
}

/// Encodes a single-channel image as BC4 blocks, row-major, edge-padded to
/// whole blocks. Returns `(blocks, width_blocks, height_blocks)`.
pub fn encode_bc4_image(data: &[u8], width: u32, height: u32) -> Result<(Vec<u8>, u32, u32)> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return Err(Error::lighting("BC4 image has a zero dimension"));
    }
    if data.len() != w * h {
        return Err(Error::lighting(format!(
            "BC4 image is {} texels, {w}x{h} needs {}",
            data.len(),
            w * h
        )));
    }
    let bw = w.div_ceil(4);
    let bh = h.div_ceil(4);
    let at = |x: usize, y: usize| data[y.min(h - 1) * w + x.min(w - 1)];
    let mut out = Vec::with_capacity(bw * bh * 8);
    for by in 0..bh {
        for bx in 0..bw {
            let mut blk = [0u8; 16];
            for (k, slot) in blk.iter_mut().enumerate() {
                let tx = k % 4;
                let ty = k / 4;
                *slot = at(bx * 4 + tx, by * 4 + ty);
            }
            out.extend_from_slice(&encode_bc4_block(&blk));
        }
    }
    Ok((out, bw as u32, bh as u32))
}

/// Encodes a red/green pair as a BC5 block image. All red blocks come first,
/// then all green blocks, which is the layout the real-time-light pages use.
pub fn encode_bc5_image(red: &[u8], green: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let (red_blocks, _, _) = encode_bc4_image(red, width, height)?;
    let (green_blocks, _, _) = encode_bc4_image(green, width, height)?;
    let mut out = red_blocks;
    out.extend_from_slice(&green_blocks);
    Ok(out)
}

/// Largest absolute per-texel error a BC4 round trip can introduce, in 0..255
/// units. A block's palette is four to eight values, so a step is at most the
/// range divided by seven; anything above this is a broken codec.
pub const BC4_MAX_ERROR: f32 = 255.0 / 7.0;
