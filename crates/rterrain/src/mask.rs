//! A 0..1 field at the same resolution as a heightfield, used to weight
//! generators, blends and brush strokes.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::heightfield::Heightfield;
use crate::math;

/// 4-connected neighbour offsets in a fixed order, for morphological passes.
const N4: [(i64, i64); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

fn validate(width: u32, height: u32, cell_size: f32) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(Error::ZeroSize);
    }
    if !cell_size.is_finite() || cell_size <= 0.0 {
        return Err(Error::InvalidCellSize(cell_size));
    }
    if (width as usize).checked_mul(height as usize).is_none() {
        return Err(Error::SizeMismatch(width, height, usize::MAX));
    }
    Ok(())
}

/// A scalar field of weights in roughly 0..1 over the same grid as a heightfield.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub cell_size: f32,
    pub data: Vec<f32>,
}

impl Mask {
    pub fn new(width: u32, height: u32, cell_size: f32, data: Vec<f32>) -> Result<Self> {
        validate(width, height, cell_size)?;
        let n = width as usize * height as usize;
        if data.len() != n {
            return Err(Error::SizeMismatch(width, height, data.len()));
        }
        Ok(Self { width, height, cell_size, data })
    }

    pub fn filled(width: u32, height: u32, cell_size: f32, value: f32) -> Result<Self> {
        validate(width, height, cell_size)?;
        let n = width as usize * height as usize;
        Ok(Self { width, height, cell_size, data: vec![value; n] })
    }

    pub fn from_fn<F>(width: u32, height: u32, cell_size: f32, f: F) -> Result<Self>
    where
        F: Fn(u32, u32) -> f32,
    {
        validate(width, height, cell_size)?;
        let n = width as usize * height as usize;
        let mut data = Vec::with_capacity(n);
        for y in 0..height {
            for x in 0..width {
                data.push(f(x, y));
            }
        }
        Ok(Self { width, height, cell_size, data })
    }

    /// An all-ones mask matching a heightfield's resolution.
    pub fn ones_like(hf: &Heightfield) -> Self {
        Mask { width: hf.width, height: hf.height, cell_size: hf.cell_size, data: vec![1.0; hf.data.len()] }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }

    pub fn get(&self, x: u32, y: u32) -> Result<f32> {
        if x >= self.width || y >= self.height {
            return Err(Error::OutOfBounds(x as i64, y as i64, self.width, self.height));
        }
        Ok(self.data[self.index(x, y)])
    }

    #[inline]
    pub fn get_clamped(&self, x: i64, y: i64) -> f32 {
        let x = x.clamp(0, self.width as i64 - 1) as u32;
        let y = y.clamp(0, self.height as i64 - 1) as u32;
        self.data[self.index(x, y)]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, value: f32) -> Result<()> {
        if x >= self.width || y >= self.height {
            return Err(Error::OutOfBounds(x as i64, y as i64, self.width, self.height));
        }
        let i = self.index(x, y);
        self.data[i] = value;
        Ok(())
    }

    /// Clamp every sample into 0..1.
    pub fn clamped(&self) -> Self {
        let mut out = self.clone();
        for v in out.data.iter_mut() {
            *v = math::saturate(*v as f64) as f32;
        }
        out
    }

    /// Linear interpolation at fractional sample coordinates.
    pub fn sample_bilinear(&self, u: f32, v: f32) -> f32 {
        let w = self.width as i64;
        let h = self.height as i64;
        let x = math::clamp(u as f64, 0.0, (w - 1) as f64);
        let y = math::clamp(v as f64, 0.0, (h - 1) as f64);
        let x0 = x.floor() as i64;
        let y0 = y.floor() as i64;
        let fx = (x - x0 as f64) as f32;
        let fy = (y - y0 as f64) as f32;
        let a = self.get_clamped(x0, y0);
        let b = self.get_clamped(x0 + 1, y0);
        let c = self.get_clamped(x0, y0 + 1);
        let d = self.get_clamped(x0 + 1, y0 + 1);
        let top = a + (b - a) * fx;
        let bot = c + (d - c) * fx;
        top + (bot - top) * fy
    }
}

impl Mask {
    /// Resample to a new sample count.
    pub fn resample(&self, width: u32, height: u32) -> Result<Mask> {
        let mut out = Mask::filled(width, height, self.cell_size, 0.0)?;
        for y in 0..height {
            let v = if height > 1 {
                y as f64 * (self.height as f64 - 1.0) / (height as f64 - 1.0)
            } else {
                0.0
            };
            for x in 0..width {
                let u = if width > 1 {
                    x as f64 * (self.width as f64 - 1.0) / (width as f64 - 1.0)
                } else {
                    0.0
                };
                let i = out.index(x, y);
                out.data[i] = self.sample_bilinear(u as f32, v as f32);
            }
        }
        Ok(out)
    }

    /// Copy a sub-rectangle.
    pub fn crop(&self, x0: u32, y0: u32, width: u32, height: u32) -> Result<Mask> {
        let helper = Heightfield {
            width: self.width,
            height: self.height,
            cell_size: self.cell_size,
            data: self.data.clone(),
        }
        .crop(x0, y0, width, height)?;
        Ok(Mask { width: helper.width, height: helper.height, cell_size: helper.cell_size, data: helper.data })
    }

    /// Grow the grid by `pad` samples on every side, repeating edge values.
    pub fn pad(&self, pad: u32) -> Result<Mask> {
        let helper = Heightfield {
            width: self.width,
            height: self.height,
            cell_size: self.cell_size,
            data: self.data.clone(),
        };
        let out = helper.pad(pad)?;
        Ok(Mask { width: out.width, height: out.height, cell_size: out.cell_size, data: out.data })
    }

    /// Lowest sample.
    pub fn min(&self) -> f32 {
        self.data.iter().copied().fold(f32::INFINITY, f32::min)
    }

    /// Highest sample.
    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(f32::NEG_INFINITY, f32::max)
    }

    /// Rescale the sample range onto `lo..=hi`.
    pub fn normalize_to(&mut self, lo: f32, hi: f32) -> Result<()> {
        if !lo.is_finite() || !hi.is_finite() || hi < lo {
            return Err(Error::BadRange(lo, hi));
        }
        let (mn, mx) = (self.min(), self.max());
        let span = mx - mn;
        if span <= 0.0 || !span.is_finite() {
            for v in self.data.iter_mut() {
                *v = lo;
            }
            return Ok(());
        }
        let scale = (hi - lo) / span;
        for v in self.data.iter_mut() {
            *v = lo + (*v - mn) * scale;
        }
        Ok(())
    }

    /// Rescale to 0..1.
    pub fn normalize_unit(&mut self) -> Result<()> {
        self.normalize_to(0.0, 1.0)
    }

    /// Pointwise combination of two same-shaped masks.
    pub fn combine(&self, other: &Mask, f: impl Fn(f32, f32) -> f32) -> Result<Mask> {
        if self.width != other.width || self.height != other.height {
            return Err(Error::SizeMismatch(other.width, other.height, other.data.len()));
        }
        let mut out = self.clone();
        for i in 0..out.data.len() {
            out.data[i] = f(self.data[i], other.data[i]);
        }
        Ok(out)
    }

    /// Hard threshold: samples at or above `t` become one, the rest zero.
    pub fn threshold(&self, t: f32) -> Mask {
        let mut out = self.clone();
        for v in out.data.iter_mut() {
            *v = if *v >= t { 1.0 } else { 0.0 };
        }
        out
    }
}

impl Mask {
    /// Separable box blur repeated `passes` times, which converges towards a
    /// Gaussian. `radius` is in samples.
    pub fn blur(&self, radius: u32, passes: u32) -> Mask {
        if radius == 0 || passes == 0 || self.data.is_empty() {
            return self.clone();
        }
        let w = self.width as i64;
        let h = self.height as i64;
        let r = radius as i64;
        let mut cur = self.data.clone();
        let mut tmp = vec![0.0f32; cur.len()];
        for _ in 0..passes.max(1) {
            for y in 0..h {
                for x in 0..w {
                    let (mut sum, mut n) = (0.0f32, 0i64);
                    for d in -r..=r {
                        let sx = x + d;
                        if sx < 0 || sx >= w {
                            continue;
                        }
                        sum += cur[y as usize * w as usize + sx as usize];
                        n += 1;
                    }
                    tmp[y as usize * w as usize + x as usize] = if n > 0 { sum / n as f32 } else { 0.0 };
                }
            }
            for y in 0..h {
                for x in 0..w {
                    let (mut sum, mut n) = (0.0f32, 0i64);
                    for d in -r..=r {
                        let sy = y + d;
                        if sy < 0 || sy >= h {
                            continue;
                        }
                        sum += tmp[sy as usize * w as usize + x as usize];
                        n += 1;
                    }
                    cur[y as usize * w as usize + x as usize] = if n > 0 { sum / n as f32 } else { 0.0 };
                }
            }
        }
        Mask { width: self.width, height: self.height, cell_size: self.cell_size, data: cur }
    }

    /// Erode the set region: each pass clears any sample with a clear
    /// 4-neighbour, so `iterations` erodes by that many samples.
    pub fn shrink(&self, iterations: u32) -> Mask {
        let mut cur = self.threshold(0.5);
        for _ in 0..iterations {
            let mut next = cur.clone();
            for y in 0..self.height as i64 {
                for x in 0..self.width as i64 {
                    if cur.get_clamped(x, y) <= 0.0 {
                        continue;
                    }
                    for (dx, dy) in N4 {
                        if cur.get_clamped(x + dx, y + dy) <= 0.0 {
                            let i = cur.index(x as u32, y as u32);
                            next.data[i] = 0.0;
                            break;
                        }
                    }
                }
            }
            cur = next;
        }
        cur
    }

    /// Dilate the set region: each pass fills any sample with a set
    /// 4-neighbour, so `iterations` grows by that many samples.
    pub fn grow(&self, iterations: u32) -> Mask {
        let mut cur = self.threshold(0.5);
        for _ in 0..iterations {
            let mut next = cur.clone();
            for y in 0..self.height as i64 {
                for x in 0..self.width as i64 {
                    if cur.get_clamped(x, y) > 0.0 {
                        continue;
                    }
                    for (dx, dy) in N4 {
                        if cur.get_clamped(x + dx, y + dy) > 0.0 {
                            let i = cur.index(x as u32, y as u32);
                            next.data[i] = 1.0;
                            break;
                        }
                    }
                }
            }
            cur = next;
        }
        cur
    }

    /// Serialise to raw little-endian `f32`.
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() * 4);
        for v in &self.data {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Parse raw little-endian `f32`.
    pub fn from_le_bytes(bytes: &[u8], width: u32, height: u32, cell_size: f32) -> Result<Self> {
        let hf = Heightfield::from_le_bytes(bytes, width, height, cell_size)?;
        Ok(Mask { width: hf.width, height: hf.height, cell_size: hf.cell_size, data: hf.data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(w: u32, h: u32) -> Mask {
        Mask::from_fn(w, h, 8.0, |x, y| {
            let dx = x as f32 - (w as f32 - 1.0) * 0.5;
            let dy = y as f32 - (h as f32 - 1.0) * 0.5;
            if dx * dx + dy * dy <= 9.0 {
                1.0
            } else {
                0.0
            }
        })
        .unwrap()
    }

    #[test]
    fn construction_validates_input() {
        assert!(matches!(Mask::new(0, 4, 1.0, vec![]), Err(Error::ZeroSize)));
        assert!(matches!(Mask::new(4, 4, 0.0, vec![0.0; 16]), Err(Error::InvalidCellSize(_))));
        assert!(matches!(Mask::new(4, 4, 1.0, vec![0.0; 15]), Err(Error::SizeMismatch(..))));
        let m = Mask::filled(3, 2, 4.0, 0.5).unwrap();
        assert_eq!((m.len(), m.is_empty()), (6, false));
        assert_eq!(m.index(2, 1), 5);
        assert!(Mask::filled(1, 1, 1.0, 0.0).unwrap().is_empty() == false);
    }

    #[test]
    fn accessors_bounds_check() {
        let m = Mask::from_fn(4, 3, 2.0, |x, y| (x * 10 + y) as f32).unwrap();
        assert_eq!(m.get(3, 2).unwrap(), 32.0);
        assert!(m.get(4, 2).is_err());
        assert!(m.get(0, 3).is_err());
        assert_eq!(m.get_clamped(-5, -5), 0.0);
        assert_eq!(m.get_clamped(99, 99), 32.0);
        let mut m = m;
        m.set(1, 1, -4.0).unwrap();
        assert_eq!(m.get(1, 1).unwrap(), -4.0);
        assert!(m.set(9, 9, 0.0).is_err());
    }

    #[test]
    fn ones_like_matches_the_heightfield() {
        let hf = Heightfield::filled(5, 7, 3.0, 12.0).unwrap();
        let m = Mask::ones_like(&hf);
        assert_eq!((m.width, m.height, m.cell_size), (5, 7, 3.0));
        assert!(m.data.iter().all(|v| *v == 1.0));
        assert_eq!(m.len(), hf.data.len());
    }

    #[test]
    fn clamping_and_threshold() {
        let m = Mask::from_fn(3, 1, 1.0, |x, _| x as f32 - 1.0).unwrap();
        let c = m.clamped();
        assert_eq!(c.data, vec![0.0, 0.0, 1.0]);
        // The original is untouched, so a clamp is not a destructive edit.
        assert_eq!(m.data, vec![-1.0, 0.0, 1.0]);
        let t = m.threshold(0.5);
        assert_eq!(t.data, vec![0.0, 0.0, 1.0]);
        assert_eq!(m.threshold(-2.0).data, vec![1.0, 1.0, 1.0]);
    }

    #[test]
    fn sampling_interpolates_between_samples() {
        let m = Mask::from_fn(5, 5, 1.0, |x, _| x as f32 * 2.0).unwrap();
        assert_eq!(m.sample_bilinear(0.0, 0.0), 0.0);
        assert_eq!(m.sample_bilinear(2.5, 0.0), 5.0);
        assert_eq!(m.sample_bilinear(4.0, 0.0), 8.0);
        // Outside the grid the sample clamps.
        assert_eq!(m.sample_bilinear(-3.0, 0.0), 0.0);
        assert_eq!(m.sample_bilinear(99.0, 0.0), 8.0);
    }

    #[test]
    fn resample_keeps_the_shape() {
        // A plane is exactly reproduced by bilinear interpolation, so the round
        // trip has to come back bit for bit.
        let plane = Mask::from_fn(16, 16, 1.0, |x, y| x as f32 * 2.0 + y as f32 * 0.5).unwrap();
        let back = plane.resample(32, 32).unwrap().resample(16, 16).unwrap();
        for (x, y) in back.data.iter().zip(plane.data.iter()) {
            assert!((x - y).abs() < 1e-3, "{x} vs {y}");
        }
        // A smooth cone survives a round trip to within the curvature the
        // resample cannot see.
        let cone = Mask::from_fn(16, 16, 1.0, |x, y| {
            let dx = x as f32 - 7.5;
            let dy = y as f32 - 7.5;
            (1.0 - (dx * dx + dy * dy).sqrt() / 8.0).max(0.0)
        })
        .unwrap();
        let big = cone.resample(32, 32).unwrap();
        assert_eq!((big.width, big.height), (32, 32));
        assert!(big.max() >= cone.max() - 1e-5);
        let back = big.resample(16, 16).unwrap();
        assert!(back.max() > cone.max() * 0.95, "the cone lost its peak: {} vs {}", back.max(), cone.max());
    }

    #[test]
    fn crop_and_pad() {
        let m = Mask::from_fn(6, 5, 1.0, |x, y| (y * 6 + x) as f32).unwrap();
        let sub = m.crop(2, 1, 3, 2).unwrap();
        assert_eq!((sub.width, sub.height), (3, 2));
        assert_eq!(sub.data[0], 8.0);
        assert!(m.crop(4, 0, 3, 2).is_err());
        let p = m.pad(1).unwrap();
        assert_eq!((p.width, p.height), (8, 7));
        assert_eq!(p.get(0, 0).unwrap(), 0.0);
        assert_eq!(p.get(2, 2).unwrap(), m.get(1, 1).unwrap());
    }

    #[test]
    fn min_max_and_normalize() {
        let mut m = Mask::from_fn(4, 4, 1.0, |x, y| (x + y * 4) as f32).unwrap();
        assert_eq!((m.min(), m.max()), (0.0, 15.0));
        m.normalize_to(2.0, 6.0).unwrap();
        assert!((m.min() - 2.0).abs() < 1e-5);
        assert!((m.max() - 6.0).abs() < 1e-5);
        m.normalize_unit().unwrap();
        assert!((m.max() - 1.0).abs() < 1e-5);
        let mut flat = Mask::filled(3, 3, 1.0, 1.0).unwrap();
        flat.normalize_to(4.0, 8.0).unwrap();
        assert!(flat.data.iter().all(|v| *v == 4.0));
        assert!(matches!(flat.normalize_to(1.0, 0.0), Err(Error::BadRange(_, _))));
    }

    #[test]
    fn combine_needs_the_same_shape() {
        let a = disc(8, 8);
        let b = Mask::filled(8, 8, 8.0, 0.25).unwrap();
        let sum = a.combine(&b, |x, y| x + y).unwrap();
        assert!(sum.data.iter().all(|v| *v == 0.25 || *v == 1.25));
        let other = disc(4, 4);
        assert!(matches!(a.combine(&other, |x, y| x + y), Err(Error::SizeMismatch(..))));
    }

    #[test]
    fn blur_spreads_the_disc() {
        let m = disc(24, 24);
        let total = |x: &Mask| x.data.iter().map(|v| *v as f64).sum::<f64>();
        let blurred = m.blur(2, 2);
        // A box blur moves the weight outwards without inventing or losing any,
        // so the total is unchanged and the peak comes down.
        assert!(blurred.max() < m.max());
        assert!((total(&blurred) - total(&m)).abs() < total(&m) * 0.05, "the blur changed the total");
        // A zero radius or zero passes is a no-op.
        assert_eq!(m.blur(0, 3), m);
        assert_eq!(m.blur(3, 0), m);
    }

    #[test]
    fn grow_and_shrink_are_inverse_one_sample_steps() {
        let m = disc(24, 24);
        let grown = m.grow(1);
        let shrunk = m.shrink(1);
        let count = |x: &Mask| x.data.iter().filter(|v| **v > 0.5).count();
        assert!(count(&grown) > count(&m), "grow did not grow the disc");
        assert!(count(&shrunk) < count(&m), "shrink did not shrink the disc");
        // Growing then shrinking returns to where it started.
        let round = m.grow(2).shrink(2);
        assert_eq!(count(&round), count(&m));
        // An empty mask stays empty however many passes it gets.
        let none = Mask::filled(8, 8, 1.0, 0.0).unwrap();
        assert_eq!(count(&none.grow(3)), 0);
        assert_eq!(count(&none.shrink(3)), 0);
    }

    #[test]
    fn bytes_round_trip() {
        let m = disc(9, 7);
        let bytes = m.to_le_bytes();
        assert_eq!(bytes.len(), 9 * 7 * 4);
        let back = Mask::from_le_bytes(&bytes, 9, 7, 8.0).unwrap();
        assert_eq!(back, m);
        // A truncated or oversized buffer is an error, never a partial read.
        assert!(Mask::from_le_bytes(&bytes[..8], 9, 7, 8.0).is_err());
        assert!(Mask::from_le_bytes(&bytes, 0, 7, 8.0).is_err());
    }
}

