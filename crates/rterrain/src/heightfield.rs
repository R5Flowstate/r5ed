//! Heightfields: row-major `f32` heights in Source inches, sampled every
//! `cell_size` along X and Y. Sample `(x, y)` is at index `y * width + x`, so
//! X is the fast axis.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::math;

/// Rectangular sample range, half-open on the high edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    pub fn new(x0: u32, y0: u32, x1: u32, y1: u32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    /// Whole-grid rect for a grid of the given size.
    pub fn all(w: u32, h: u32) -> Self {
        Self { x0: 0, y0: 0, x1: w, y1: h }
    }

    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    pub fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    /// Smallest rect containing both.
    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// Clip to a `w` by `h` grid, giving an empty rect when disjoint.
    pub fn clamp_to(&self, w: u32, h: u32) -> Rect {
        Rect { x0: self.x0.min(w), y0: self.y0.min(h), x1: self.x1.min(w), y1: self.y1.min(h) }
    }

    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// Iterate the samples in row-major order.
    pub fn iter(&self) -> RectIter {
        RectIter { rect: *self, x: self.x0, y: self.y0 }
    }
}

impl Rect {
    /// Grow the rect by `pad` samples on every side, clipped to the grid.
    pub fn pad(&self, pad: u32) -> Rect {
        Rect {
            x0: self.x0.saturating_sub(pad),
            y0: self.y0.saturating_sub(pad),
            x1: self.x1.saturating_add(pad),
            y1: self.y1.saturating_add(pad),
        }
    }
}

/// Row-major iterator over the samples of a [`Rect`].
pub struct RectIter {
    rect: Rect,
    x: u32,
    y: u32,
}

impl Iterator for RectIter {
    type Item = (u32, u32);

    fn next(&mut self) -> Option<(u32, u32)> {
        if self.y >= self.rect.y1 {
            return None;
        }
        let cur = (self.x, self.y);
        self.x += 1;
        if self.x >= self.rect.x1 {
            self.x = self.rect.x0;
            self.y += 1;
        }
        Some(cur)
    }
}

fn validate_dims(width: u32, height: u32, cell_size: f32) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(Error::ZeroSize);
    }
    if !cell_size.is_finite() || cell_size <= 0.0 {
        return Err(Error::InvalidCellSize(cell_size));
    }
    // Guard the usize product on 32-bit wasm as well as 64-bit native.
    if (width as usize).checked_mul(height as usize).is_none() {
        return Err(Error::SizeMismatch(width, height, usize::MAX));
    }
    Ok(())
}

/// A regular grid of heights, row-major, indexed `y * width + x`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Heightfield {
    pub width: u32,
    pub height: u32,
    pub cell_size: f32,
    pub data: Vec<f32>,
}

impl Heightfield {
    /// Construct from raw data, checking the length against the extent.
    pub fn new(width: u32, height: u32, cell_size: f32, data: Vec<f32>) -> Result<Self> {
        validate_dims(width, height, cell_size)?;
        let n = width as usize * height as usize;
        if data.len() != n {
            return Err(Error::SizeMismatch(width, height, data.len()));
        }
        Ok(Self { width, height, cell_size, data })
    }

    /// Every sample at the same height.
    pub fn filled(width: u32, height: u32, cell_size: f32, value: f32) -> Result<Self> {
        validate_dims(width, height, cell_size)?;
        let n = width as usize * height as usize;
        Ok(Self { width, height, cell_size, data: vec![value; n] })
    }

    /// Build from a closure over integer sample coordinates.
    pub fn from_fn<F>(width: u32, height: u32, cell_size: f32, f: F) -> Result<Self>
    where
        F: Fn(u32, u32) -> f32,
    {
        validate_dims(width, height, cell_size)?;
        let n = width as usize * height as usize;
        let mut data = Vec::with_capacity(n);
        for y in 0..height {
            for x in 0..width {
                data.push(f(x, y));
            }
        }
        Ok(Self { width, height, cell_size, data })
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// World extent along X, which spans `width - 1` gaps.
    #[inline]
    pub fn extent_x(&self) -> f32 {
        (self.width as f32 - 1.0) * self.cell_size
    }

    /// World extent along Y, which spans `height - 1` gaps.
    #[inline]
    pub fn extent_y(&self) -> f32 {
        (self.height as f32 - 1.0) * self.cell_size
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }

    /// Bounds-checked sample fetch.
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> Result<f32> {
        if x >= self.width || y >= self.height {
            return Err(Error::OutOfBounds(x as i64, y as i64, self.width, self.height));
        }
        Ok(self.data[self.index(x, y)])
    }

    /// Sample fetch that clamps the coordinate to the edge.
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

    /// Lowest sample, or positive infinity for an empty grid.
    pub fn min(&self) -> f32 {
        self.data.iter().copied().fold(f32::INFINITY, f32::min)
    }

    /// Highest sample, or negative infinity for an empty grid.
    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(f32::NEG_INFINITY, f32::max)
    }

    /// Inclusive `lo..=hi` sample bounds.
    pub fn range(&self) -> (f32, f32) {
        (self.min(), self.max())
    }

    /// True when no sample is NaN or infinite.
    pub fn is_finite(&self) -> bool {
        self.data.iter().all(|v| v.is_finite())
    }

    /// Index of the first non-finite sample, if any.
    pub fn first_non_finite(&self) -> Option<usize> {
        self.data.iter().position(|v| !v.is_finite())
    }

    /// Replace any NaN or infinite sample with `fallback`.
    pub fn sanitize(&mut self, fallback: f32) {
        for v in self.data.iter_mut() {
            if !v.is_finite() {
                *v = fallback;
            }
        }
    }

    /// Add a constant to every sample.
    pub fn offset(&mut self, delta: f32) {
        for v in self.data.iter_mut() {
            *v += delta;
        }
    }

    /// Multiply every sample.
    pub fn scale_values(&mut self, factor: f32) {
        for v in self.data.iter_mut() {
            *v *= factor;
        }
    }

    /// Rescale so the sample range maps onto `lo..=hi`. A flat grid maps to `lo`.
    pub fn normalize_to(&mut self, lo: f32, hi: f32) -> Result<()> {
        if !lo.is_finite() || !hi.is_finite() || hi < lo {
            return Err(Error::BadRange(lo, hi));
        }
        let (mn, mx) = self.range();
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

    /// Rescale to `0..=1`.
    pub fn normalize_unit(&mut self) -> Result<()> {
        self.normalize_to(0.0, 1.0)
    }
}

/// How to interpolate between samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    /// Nearest sample.
    Nearest,
    /// Bilinear over the surrounding cell.
    #[default]
    Bilinear,
    /// Catmull-Rom cubic over the surrounding 4x4 neighbourhood.
    Bicubic,
}

/// Catmull-Rom interpolation for offset `t` in `0..=1` between `p1` and `p2`.
#[inline]
fn catmull_rom(t: f32, p0: f32, p1: f32, p2: f32, p3: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

/// Normalize a 3-vector, returning `+Z` for a zero-length input.
pub fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = math::sqrt((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]) as f64) as f32;
    if len > 0.0 && len.is_finite() {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

impl Heightfield {
    /// Sample at fractional sample coordinates, clamping to the grid.
    ///
    /// `u` and `v` are in sample units: `0.0` is the first column and row,
    /// `width - 1` is the last.
    pub fn sample(&self, u: f32, v: f32, filter: Filter) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        match filter {
            Filter::Nearest => self.get_clamped(u.round() as i64, v.round() as i64),
            Filter::Bilinear => self.sample_bilinear(u, v),
            Filter::Bicubic => self.sample_bicubic(u, v),
        }
    }

    /// Bilinear interpolation at fractional sample coordinates.
    pub fn sample_bilinear(&self, u: f32, v: f32) -> f32 {
        let w = self.width as i64;
        let h = self.height as i64;
        let x = math::clamp(u as f64, 0.0, (w - 1) as f64);
        let y = math::clamp(v as f64, 0.0, (h - 1) as f64);
        let x0 = x.floor() as i64;
        let y0 = y.floor() as i64;
        let fx = (x - x0 as f64) as f32;
        let fy = (y - y0 as f64) as f32;
        let h00 = self.get_clamped(x0, y0);
        let h10 = self.get_clamped(x0 + 1, y0);
        let h01 = self.get_clamped(x0, y0 + 1);
        let h11 = self.get_clamped(x0 + 1, y0 + 1);
        let a = h00 + (h10 - h00) * fx;
        let b = h01 + (h11 - h01) * fx;
        a + (b - a) * fy
    }

    /// Bicubic (Catmull-Rom) interpolation at fractional sample coordinates.
    pub fn sample_bicubic(&self, u: f32, v: f32) -> f32 {
        let w = self.width as i64;
        let h = self.height as i64;
        let x = math::clamp(u as f64, 0.0, (w - 1) as f64);
        let y = math::clamp(v as f64, 0.0, (h - 1) as f64);
        let x0 = x.floor() as i64;
        let y0 = y.floor() as i64;
        let fx = (x - x0 as f64) as f32;
        let fy = (y - y0 as f64) as f32;
        let mut col = [0.0f32; 4];
        for (k, slot) in col.iter_mut().enumerate() {
            let yk = y0 - 1 + k as i64;
            *slot = catmull_rom(
                fx,
                self.get_clamped(x0 - 1, yk),
                self.get_clamped(x0, yk),
                self.get_clamped(x0 + 1, yk),
                self.get_clamped(x0 + 2, yk),
            );
        }
        catmull_rom(fy, col[0], col[1], col[2], col[3])
    }

    /// Sample by world position in object space, where the origin is the
    /// centre of the grid footprint and Z is up.
    pub fn sample_world(&self, x: f32, y: f32, filter: Filter) -> f32 {
        let (u, v) = self.world_to_sample(x, y);
        self.sample(u, v, filter)
    }

    /// Convert a world position to fractional sample coordinates.
    pub fn world_to_sample(&self, x: f32, y: f32) -> (f32, f32) {
        (
            x / self.cell_size + (self.width as f32 - 1.0) * 0.5,
            y / self.cell_size + (self.height as f32 - 1.0) * 0.5,
        )
    }

    /// Convert fractional sample coordinates to a world position.
    pub fn sample_to_world(&self, u: f32, v: f32) -> (f32, f32) {
        (
            (u - (self.width as f32 - 1.0) * 0.5) * self.cell_size,
            (v - (self.height as f32 - 1.0) * 0.5) * self.cell_size,
        )
    }
}

impl Heightfield {
    /// Unit surface normal at an integer sample, from central differences.
    pub fn normal(&self, x: u32, y: u32) -> [f32; 3] {
        let (gx, gy) = self.gradient(x, y);
        normalize3([-gx, -gy, 1.0])
    }

    /// Per-sample unit normals, row-major.
    pub fn normals(&self) -> Vec<[f32; 3]> {
        let mut out = Vec::with_capacity(self.data.len());
        for y in 0..self.height {
            for x in 0..self.width {
                out.push(self.normal(x, y));
            }
        }
        out
    }

    /// Height gradient `(dh/dx, dh/dy)` at an integer sample.
    pub fn gradient(&self, x: u32, y: u32) -> (f32, f32) {
        let c = self.cell_size;
        let gx = (self.get_clamped(x as i64 + 1, y as i64) - self.get_clamped(x as i64 - 1, y as i64)) / (2.0 * c);
        let gy = (self.get_clamped(x as i64, y as i64 + 1) - self.get_clamped(x as i64, y as i64 - 1)) / (2.0 * c);
        (gx, gy)
    }

    /// Per-sample slope in radians: zero is flat, `pi/2` is vertical.
    pub fn slope(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.data.len());
        for y in 0..self.height {
            for x in 0..self.width {
                let (gx, gy) = self.gradient(x, y);
                let g = math::sqrt((gx * gx + gy * gy) as f64) as f32;
                out.push(math::atan(g as f64) as f32);
            }
        }
        out
    }

    /// Per-sample slope in degrees.
    pub fn slope_deg(&self) -> Vec<f32> {
        self.slope().into_iter().map(|s| s * (180.0 / std::f32::consts::PI)).collect()
    }

    /// Per-sample discrete curvature, the Laplacian of height, so positive on
    /// ridges and negative in hollows. Units are inches per cell squared.
    pub fn curvature(&self) -> Vec<f32> {
        let c2 = self.cell_size * self.cell_size;
        if c2 <= 0.0 {
            return vec![0.0; self.data.len()];
        }
        let mut out = Vec::with_capacity(self.data.len());
        for y in 0..self.height {
            for x in 0..self.width {
                let centre = self.data[self.index(x, y)];
                let sum = self.get_clamped(x as i64 - 1, y as i64)
                    + self.get_clamped(x as i64 + 1, y as i64)
                    + self.get_clamped(x as i64, y as i64 - 1)
                    + self.get_clamped(x as i64, y as i64 + 1);
                out.push((sum - 4.0 * centre) / c2);
            }
        }
        out
    }

    /// Resample to a new sample count, preserving the world footprint by
    /// scaling `cell_size` to match.
    pub fn resample(&self, width: u32, height: u32, filter: Filter) -> Result<Heightfield> {
        validate_dims(width, height, self.cell_size)?;
        if width == self.width && height == self.height {
            return Ok(self.clone());
        }
        let cell_x = if width > 1 { self.extent_x() / (width as f32 - 1.0) } else { self.cell_size };
        let cell_y = if height > 1 { self.extent_y() / (height as f32 - 1.0) } else { self.cell_size };
        let cell = if cell_x > 0.0 && cell_y > 0.0 { (cell_x + cell_y) * 0.5 } else { self.cell_size };
        let mut out = Heightfield::filled(width, height, cell, 0.0)?;
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
                out.data[i] = self.sample(u as f32, v as f32, filter);
            }
        }
        Ok(out)
    }
}

impl Heightfield {
    /// Copy a sub-rectangle of samples.
    pub fn crop(&self, x0: u32, y0: u32, width: u32, height: u32) -> Result<Heightfield> {
        if width == 0 || height == 0 {
            return Err(Error::ZeroSize);
        }
        let x1 = x0 as u64 + width as u64;
        let y1 = y0 as u64 + height as u64;
        if x1 > self.width as u64 || y1 > self.height as u64 {
            return Err(Error::CropOutOfBounds(
                x0 as i64,
                y0 as i64,
                width,
                height,
                self.width,
                self.height,
            ));
        }
        let mut data = Vec::with_capacity(width as usize * height as usize);
        for y in y0..y0 + height {
            let row = y as usize * self.width as usize;
            data.extend_from_slice(&self.data[row + x0 as usize..row + x1 as usize]);
        }
        Ok(Heightfield { width, height, cell_size: self.cell_size, data })
    }

    /// Grow the grid by `pad` samples on every side, repeating edge values.
    pub fn pad(&self, pad: u32) -> Result<Heightfield> {
        let twice = pad.checked_mul(2).ok_or(Error::ZeroSize)?;
        let w = self.width.checked_add(twice).ok_or(Error::ZeroSize)?;
        let h = self.height.checked_add(twice).ok_or(Error::ZeroSize)?;
        validate_dims(w, h, self.cell_size)?;
        let mut out = Heightfield::filled(w, h, self.cell_size, 0.0)?;
        for y in 0..h {
            // Clamp the source coordinate into the original grid.
            let sy = y.clamp(pad, pad + self.height - 1) - pad;
            for x in 0..w {
                let sx = x.clamp(pad, pad + self.width - 1) - pad;
                let di = out.index(x, y);
                let si = self.index(sx, sy);
                out.data[di] = self.data[si];
            }
        }
        Ok(out)
    }

    /// Serialise to the `.f32` sidecar format: raw row-major little-endian
    /// `f32` values with no header.
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() * 4);
        for v in &self.data {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Parse the `.f32` sidecar format, checking the byte count first.
    pub fn from_le_bytes(bytes: &[u8], width: u32, height: u32, cell_size: f32) -> Result<Self> {
        validate_dims(width, height, cell_size)?;
        let want = width as usize * height as usize * 4;
        if bytes.len() != want {
            return Err(Error::BadByteLength(width as usize * height as usize, width, height, bytes.len()));
        }
        let mut data = Vec::with_capacity(want / 4);
        for chunk in bytes.chunks_exact(4) {
            data.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Ok(Heightfield { width, height, cell_size, data })
    }
}

/// Where water flows and how much of it gathers there.
#[derive(Clone, Debug, PartialEq)]
pub struct Flow {
    /// Catchment area per sample, in world units squared.
    pub area: Vec<f32>,
    /// Downslope direction per sample, unit length, zero where there is none.
    pub direction: Vec<[f32; 2]>,
    /// Visit order, highest first, one grid index per sample.
    pub order: Vec<u32>,
    /// Heights with every depression filled to its spill level, so a lake
    /// surface is flat from one end of a basin to the other.
    pub filled: Vec<f32>,
}

impl Flow {
    /// Largest catchment area in the grid.
    pub fn max_area(&self) -> f32 {
        self.area.iter().copied().fold(0.0f32, f32::max)
    }
}

/// 8-connected neighbour offsets in a fixed order.
const N8: [(i64, i64); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

/// Total order on `f32` for binary heaps, so heap output is deterministic.
#[derive(Clone, Copy, PartialEq)]
struct OrderedF32(f32);

impl Eq for OrderedF32 {}

impl PartialOrd for OrderedF32 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedF32 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Heightfield {
    /// Accumulate drainage area using a priority-flood depression fill.
    ///
    /// Every sample drains to its steepest lower 8-neighbour after single-cell
    /// pits are filled, so closed basins still route water off the map. The
    /// traversal order follows only heights and index order, so the result is
    /// reproducible without a seed.
    pub fn flow_accumulation(&self) -> Flow {
        let w = self.width as i64;
        let h = self.height as i64;
        let n = self.data.len();
        let cell_area = self.cell_size * self.cell_size;
        let mut area = vec![cell_area; n];
        let mut direction = vec![[0.0f32, 0.0f32]; n];
        let mut filled = self.data.clone();
        let mut visited = vec![false; n];

        // Seed a min-heap with the border, which drains off the map.
        let mut heap: BinaryHeap<Reverse<(OrderedF32, u32)>> = BinaryHeap::with_capacity(n);
        for y in 0..h {
            for x in 0..w {
                if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                    let i = self.index(x as u32, y as u32);
                    visited[i] = true;
                    heap.push(Reverse((OrderedF32(filled[i]), i as u32)));
                }
            }
        }

        let mut order: Vec<u32> = Vec::with_capacity(n);
        while let Some(Reverse((OrderedF32(level), i))) = heap.pop() {
            let (x, y) = (i as i64 % w, i as i64 / w);
            order.push(i);
            for (dx, dy) in N8 {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let ni = self.index(nx as u32, ny as u32);
                if visited[ni] {
                    continue;
                }
                visited[ni] = true;
                // Raise the cell just enough to escape, with a tiny tilt so
                // ties still yield a strictly descending path.
                let needed = level + 1e-4 * (1.0 + level.abs());
                if filled[ni] < needed {
                    filled[ni] = needed;
                }
                heap.push(Reverse((OrderedF32(filled[ni]), ni as u32)));
            }
        }

        // Route each cell downhill, walking the fill order from the top down
        // so that upstream area has already arrived.
        for idx in order.iter().rev() {
            let i = *idx as usize;
            let (x, y) = (i as i64 % w, i as i64 / w);
            let here = filled[i];
            let mut best: i64 = -1;
            let mut best_drop = 0.0f32;
            for (dx, dy) in N8 {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let ni = self.index(nx as u32, ny as u32);
                let dist = if dx != 0 && dy != 0 {
                    self.cell_size * std::f32::consts::SQRT_2
                } else {
                    self.cell_size
                };
                let drop = (here - filled[ni]) / dist;
                if drop > best_drop {
                    best_drop = drop;
                    best = ni as i64;
                }
            }
            if best >= 0 {
                let b = best as usize;
                area[b] += area[i];
                let (dx, dy) = ((b as i64 % w) - x, (b as i64 / w) - y);
                let len = math::sqrt((dx * dx + dy * dy) as f64) as f32;
                if len > 0.0 {
                    direction[i] = [dx as f32 / len, dy as f32 / len];
                }
            }
        }

        order.reverse();
        Flow { area, direction, order, filled }
    }

    /// Raise every depression to the level of its lowest outlet, which is the
    /// surface a lake would settle on. Samples outside a basin are untouched.
    pub fn fill_depressions(&self) -> Vec<f32> {
        self.flow_accumulation().filled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: u32, h: u32, cell: f32) -> Heightfield {
        Heightfield::from_fn(w, h, cell, |x, y| x as f32 + y as f32 * 10.0).unwrap()
    }

    #[test]
    fn construction_validates_input() {
        assert!(matches!(Heightfield::new(0, 4, 1.0, vec![]), Err(Error::ZeroSize)));
        assert!(matches!(Heightfield::new(4, 0, 1.0, vec![]), Err(Error::ZeroSize)));
        assert!(matches!(Heightfield::new(2, 2, 0.0, vec![0.0; 4]), Err(Error::InvalidCellSize(_))));
        assert!(matches!(Heightfield::new(2, 2, f32::NAN, vec![0.0; 4]), Err(Error::InvalidCellSize(_))));
        assert!(matches!(Heightfield::new(2, 2, 1.0, vec![0.0; 3]), Err(Error::SizeMismatch(2, 2, 3))));
        let ok = Heightfield::new(2, 3, 1.0, vec![0.0; 6]).unwrap();
        assert_eq!(ok.len(), 6);
        assert_eq!(ok.index(1, 2), 5);
    }

    #[test]
    fn accessors_bounds_check() {
        let hf = ramp(4, 3, 2.0);
        assert!(hf.get(3, 2).is_ok());
        assert!(hf.get(4, 2).is_err());
        assert!(hf.get(0, 3).is_err());
        // Clamped reads never fail, even far outside the grid.
        assert_eq!(hf.get_clamped(-99, -99), hf.get(0, 0).unwrap());
        assert_eq!(hf.get_clamped(99, 99), hf.get(3, 2).unwrap());
    }

    #[test]
    fn sampling_matches_known_plane() {
        let hf = Heightfield::from_fn(5, 5, 1.0, |x, y| (x * 2 + y * 3) as f32).unwrap();
        // A plane is reproduced exactly by both filters, except at the border
        // where bicubic clamps its outer taps.
        for (u, v) in [(0.0, 0.0), (1.5, 2.25), (2.0, 4.0), (3.9, 0.1)] {
            let want = u * 2.0 + v * 3.0;
            assert!((hf.sample_bilinear(u, v) - want).abs() < 1e-4, "bilinear at {u},{v}");
        }
        for (u, v) in [(1.5, 2.25), (2.0, 2.0), (2.5, 1.5)] {
            let want = u * 2.0 + v * 3.0;
            assert!((hf.sample_bicubic(u, v) - want).abs() < 1e-3, "bicubic at {u},{v}");
        }
        // Border taps are clamped, so the result stays finite and close.
        for (u, v) in [(0.0, 0.0), (4.0, 0.0), (0.0, 4.0), (4.0, 4.0)] {
            assert!((hf.sample_bicubic(u, v) - (u * 2.0 + v * 3.0)).abs() < 0.5);
        }
        // Sampling clamps outside the grid rather than reading rubbish.
        assert_eq!(hf.sample_bilinear(-5.0, -5.0), hf.sample_bilinear(0.0, 0.0));
        assert_eq!(hf.sample_bilinear(50.0, 50.0), hf.sample_bilinear(4.0, 4.0));
        assert_eq!(hf.sample(2.0, 1.0, Filter::Nearest), 7.0);
    }

    #[test]
    fn world_and_sample_coords_round_trip() {
        let hf = Heightfield::from_fn(9, 5, 4.0, |_, _| 0.0).unwrap();
        for (u, v) in [(0.0, 0.0), (3.5, 2.25), (8.0, 4.0)] {
            let (x, y) = hf.sample_to_world(u, v);
            let (u2, v2) = hf.world_to_sample(x, y);
            assert!((u - u2).abs() < 1e-4 && (v - v2).abs() < 1e-4);
        }
        // The footprint is centred on the origin.
        assert_eq!(hf.sample_to_world(4.0, 2.0), (0.0, 0.0));
        assert_eq!(hf.extent_x(), 32.0);
        assert_eq!(hf.extent_y(), 16.0);
    }

    #[test]
    fn normals_and_slope_track_the_surface() {
        // A constant-slope ramp rising in X only.
        let hf = Heightfield::from_fn(6, 6, 1.0, |x, _| x as f32).unwrap();
        let n = hf.normal(3, 3);
        assert!((n[0] + std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5, "normal x was {}", n[0]);
        assert!(n[1].abs() < 1e-5, "normal y was {}", n[1]);
        assert!((n[2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5, "normal z was {}", n[2]);
        let flat = Heightfield::filled(4, 4, 1.0, 7.0).unwrap();
        assert_eq!(flat.normal(1, 1), [0.0, 0.0, 1.0]);
        // Slope of a 45 degree ramp is pi/4.
        let s = hf.slope();
        assert!((s[hf.index(3, 3)] - std::f32::consts::FRAC_PI_4).abs() < 1e-4);
        let sd = hf.slope_deg();
        assert!((sd[hf.index(3, 3)] - 45.0).abs() < 0.01);
        assert!(flat.slope().iter().all(|v| *v == 0.0));
    }

    #[test]
    fn curvature_finds_a_ridge_and_a_hollow() {
        // A downward parabola is a ridge: the Laplacian of height is negative
        // at its crest and positive away from it.
        let ridge = Heightfield::from_fn(9, 9, 1.0, |x, _| -((x as f32) - 4.0).powi(2)).unwrap();
        let c = ridge.curvature();
        assert!(c[ridge.index(4, 4)] < 0.0, "crest curvature {}", c[ridge.index(4, 4)]);
        assert!(c[ridge.index(0, 4)] > 0.0, "flank curvature {}", c[ridge.index(0, 4)]);
        // An upward parabola is a hollow, so the sign flips.
        let hollow = Heightfield::from_fn(9, 9, 1.0, |x, _| ((x as f32) - 4.0).powi(2)).unwrap();
        assert!(hollow.curvature()[hollow.index(4, 4)] > 0.0);
        let flat = Heightfield::filled(5, 5, 1.0, 2.0).unwrap();
        assert!(flat.curvature().iter().all(|v| v.abs() < 1e-6));
    }

    #[test]
    fn min_max_normalize() {
        let mut hf = ramp(4, 4, 1.0);
        assert_eq!(hf.min(), 0.0);
        assert_eq!(hf.max(), 33.0);
        assert_eq!(hf.range(), (0.0, 33.0));
        hf.normalize_to(-100.0, 100.0).unwrap();
        assert!((hf.min() + 100.0).abs() < 1e-3);
        assert!((hf.max() - 100.0).abs() < 1e-3);
        hf.normalize_unit().unwrap();
        assert!((hf.min() - 0.0).abs() < 1e-5);
        assert!((hf.max() - 1.0).abs() < 1e-5);
        // A flat grid collapses to the low end rather than dividing by zero.
        let mut flat = Heightfield::filled(3, 3, 1.0, 5.0).unwrap();
        flat.normalize_to(2.0, 9.0).unwrap();
        assert!(flat.data.iter().all(|v| *v == 2.0));
        assert!(matches!(hf.normalize_to(1.0, 0.0), Err(Error::BadRange(_, _))));
    }

    #[test]
    fn offset_and_scale() {
        let mut hf = ramp(3, 3, 1.0);
        hf.offset(10.0);
        assert_eq!(hf.min(), 10.0);
        hf.scale_values(2.0);
        assert_eq!(hf.min(), 20.0);
    }

    #[test]
    fn non_finite_detection_and_repair() {
        let mut hf = ramp(4, 4, 1.0);
        assert!(hf.is_finite());
        hf.data[5] = f32::NAN;
        assert!(!hf.is_finite());
        assert_eq!(hf.first_non_finite(), Some(5));
        hf.sanitize(0.0);
        assert!(hf.is_finite());
        assert_eq!(hf.data[5], 0.0);
    }
}

#[cfg(test)]
mod grid_tests {
    use super::*;

    #[test]
    fn resample_preserves_the_surface() {
        let src = Heightfield::from_fn(9, 5, 2.0, |x, y| (x * x + y) as f32).unwrap();
        let up = src.resample(17, 9, Filter::Bilinear).unwrap();
        assert_eq!((up.width, up.height), (17, 9));
        // The footprint is preserved, so cell size scales with the sample count.
        assert!((up.extent_x() - src.extent_x()).abs() < 1e-3);
        assert!((up.extent_y() - src.extent_y()).abs() < 1e-3);
        for y in 0..9u32 {
            for x in 0..17u32 {
                let (u, v) = (x as f32 * 8.0 / 16.0, y as f32 * 4.0 / 8.0);
                let want = src.sample_bilinear(u, v);
                let got = up.get(x, y).unwrap();
                assert!((got - want).abs() < 1e-2, "at {x},{y}: {got} vs {want}");
            }
        }
        // Resampling to the same size is an exact identity.
        let same = src.resample(9, 5, Filter::Bilinear).unwrap();
        assert_eq!(same.data, src.data);
    }

    #[test]
    fn resample_down_keeps_peaks() {
        let src = Heightfield::from_fn(33, 33, 1.0, |x, y| {
            if x == 16 && y == 16 {
                100.0
            } else {
                0.0
            }
        })
        .unwrap();
        let small = src.resample(9, 9, Filter::Bilinear).unwrap();
        assert_eq!(small.max(), 100.0);
    }

    #[test]
    fn crop_extracts_the_window() {
        let hf = Heightfield::from_fn(6, 5, 1.0, |x, y| (y * 6 + x) as f32).unwrap();
        let sub = hf.crop(2, 1, 3, 2).unwrap();
        assert_eq!((sub.width, sub.height), (3, 2));
        assert_eq!(sub.data[0], 8.0);
        assert_eq!(sub.data[sub.index(2, 1)], 16.0);
        // A window that hangs off the edge is rejected, not truncated.
        assert!(matches!(hf.crop(4, 0, 3, 2), Err(Error::CropOutOfBounds(..))));
        assert!(hf.crop(0, 0, 0, 2).is_err());
        let full = hf.crop(0, 0, hf.width, hf.height).unwrap();
        assert_eq!(full.data, hf.data);
    }

    #[test]
    fn pad_repeats_edges() {
        let hf = Heightfield::from_fn(3, 3, 1.0, |x, y| (x + y * 3) as f32).unwrap();
        let p = hf.pad(1).unwrap();
        assert_eq!((p.width, p.height), (5, 5));
        assert_eq!(p.get(0, 0).unwrap(), hf.get(0, 0).unwrap());
        assert_eq!(p.get(2, 2).unwrap(), hf.get(1, 1).unwrap());
        assert_eq!(p.get(4, 4).unwrap(), hf.get(2, 2).unwrap());
        assert_eq!(p.get(1, 1).unwrap(), hf.get(0, 0).unwrap());
        let p2 = hf.pad(2).unwrap();
        assert_eq!((p2.width, p2.height), (7, 7));
    }

    #[test]
    fn sidecar_bytes_round_trip() {
        let hf = Heightfield::from_fn(3, 2, 1.5, |x, y| (x as f32) * 0.25 - (y as f32) * 100.0).unwrap();
        let bytes = hf.to_le_bytes();
        assert_eq!(bytes.len(), 6 * 4);
        let back = Heightfield::from_le_bytes(&bytes, 3, 2, 1.5).unwrap();
        assert_eq!(back, hf);
        // Wrong byte counts are errors, not partial reads.
        assert!(matches!(Heightfield::from_le_bytes(&bytes[..20], 3, 2, 1.5), Err(Error::BadByteLength(..))));
        assert!(matches!(Heightfield::from_le_bytes(&bytes, 6, 2, 1.5), Err(Error::BadByteLength(..))));
    }

    #[test]
    fn flow_accumulates_downhill() {
        // A plane tilted along X only, so rows are identical and no cross-row
        // drop exists: every row drains independently towards low X.
        let hf = Heightfield::from_fn(11, 11, 1.0, |x, _| (10 - x) as f32).unwrap();
        let flow = hf.flow_accumulation();
        assert_eq!(flow.area.len(), hf.data.len());
        assert_eq!(flow.order.len(), hf.data.len());
        let cell = hf.cell_size * hf.cell_size;
        // Along a row, area grows as it moves downhill.
        for y in 0..11u32 {
            assert!((flow.area[hf.index(0, y)] - cell).abs() < 1e-4);
            for x in 1..11u32 {
                let here = flow.area[hf.index(x, y)];
                let up = flow.area[hf.index(x - 1, y)];
                assert!(here > up, "area did not grow at {x},{y}: {here} vs {up}");
            }
            // The whole row reaches the low edge and leaves the map there.
            assert!((flow.area[hf.index(10, y)] - cell * 11.0).abs() < 1e-3);
        }
        assert!((flow.max_area() - cell * 11.0).abs() < 1e-3);
        // Every sample appears exactly once in the order.
        let mut seen = vec![false; hf.data.len()];
        for i in &flow.order {
            assert!(!seen[*i as usize]);
            seen[*i as usize] = true;
        }
        // Flow is reproducible.
        assert_eq!(hf.flow_accumulation().area, flow.area);
    }

    #[test]
    fn flow_fills_a_closed_bowl() {
        // A bowl is a depression with no outlet, so the fill must still route
        // every sample off the map instead of trapping it at the centre.
        let hf = Heightfield::from_fn(11, 11, 1.0, |x, y| {
            let dx = x as f32 - 5.0;
            let dy = y as f32 - 5.0;
            dx * dx + dy * dy
        })
        .unwrap();
        let flow = hf.flow_accumulation();
        assert_eq!(flow.order.len(), hf.data.len());
        // Some sample routes off the grid, and the centre is not a dead end
        // holding only its own cell.
        let rim = flow.area[hf.index(0, 5)];
        assert!(rim > 1.0, "rim sample accumulated nothing: {rim}");
        let max = flow.max_area();
        assert!(max > hf.cell_size * hf.cell_size * 10.0, "nothing drained: {max}");
    }

    #[test]
    fn flow_directions_point_downhill() {
        let hf = Heightfield::from_fn(9, 9, 1.0, |x, y| -((x + y) as f32) * 0.5).unwrap();
        let flow = hf.flow_accumulation();
        for y in 1..8u32 {
            for x in 1..8u32 {
                let d = flow.direction[hf.index(x, y)];
                if d == [0.0, 0.0] {
                    continue;
                }
                // Stepping along the direction must reach strictly lower ground.
                let nx = (x as f32 + d[0] * 0.7).round() as i64;
                let ny = (y as f32 + d[1] * 0.7).round() as i64;
                let here = hf.get_clamped(x as i64, y as i64);
                let there = hf.get_clamped(nx, ny);
                assert!(there <= here + 1e-3, "not downhill at {x},{y}: {here} -> {there}");
            }
        }
    }

    #[test]
    fn rect_algebra() {
        let a = Rect::new(0, 0, 4, 4);
        let b = Rect::new(2, 2, 8, 8);
        let u = a.union(&b);
        assert_eq!((u.x0, u.y0, u.x1, u.y1), (0, 0, 8, 8));
        assert_eq!(a.width(), 4);
        assert_eq!(a.height(), 4);
        assert!(a.contains(3, 3));
        assert!(!a.contains(4, 3));
        assert!(!a.is_empty());
        assert!(Rect::new(2, 2, 2, 5).is_empty());
        let cl = Rect::new(0, 0, 100, 100).clamp_to(10, 10);
        assert_eq!((cl.x0, cl.y0, cl.x1, cl.y1), (0, 0, 10, 10));
        let disjoint = Rect::new(20, 20, 30, 30).clamp_to(10, 10);
        assert!(disjoint.is_empty());
        let n = Rect::new(1, 1, 3, 3).iter().count();
        assert_eq!(n, 4);
        let visited: Vec<(u32, u32)> = Rect::new(0, 0, 2, 2).iter().collect();
        assert_eq!(visited, vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
        assert_eq!(Rect::all(3, 4), Rect::new(0, 0, 3, 4));
    }
}
