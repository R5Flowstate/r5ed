//! Edge-aware a-trous denoising on the lightmap chart domain.
//!
//! The guide is the surface normal: a tap only contributes when it faces the
//! same way as the centre, so light does not bleed across a chart's edge or a
//! corner.

use glam::Vec3;

use crate::charts::{Chart, ChartLayout};
use crate::light::BakedPage;

/// One texel's worth of shading input.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub colour: Vec3,
    pub normal: Vec3,
    /// True when a triangle covered the texel.
    pub covered: bool,
}

/// Filters one chart's texels, in place. Only the chart's own texels take part,
/// so nothing crosses a chart boundary.
pub fn denoise_chart(chart: &Chart, samples: &mut [Sample], iterations: u32, strength: f32) {
    let (w, h) = (chart.inner[0] as usize, chart.inner[1] as usize);
    if w == 0 || h == 0 || samples.len() != w * h {
        return;
    }
    for i in 0..iterations {
        if strength <= 0.0 {
            return;
        }
        filter_chart(chart, samples, strength, i);
    }
}

/// A dilated copy of a chart's texels, the largest empty neighbourhood
/// available at every texel.
pub fn dilate_chart(chart: &Chart, samples: &mut [Sample], radius: u32) {
    let (w, h) = (chart.inner[0] as usize, chart.inner[1] as usize);
    if w == 0 || h == 0 || samples.len() != w * h {
        return;
    }
    let src = samples.to_vec();
    for step in 1..=radius {
        for y in 0..h {
            for x in 0..w {
                let at = y * w + x;
                if src[at].covered {
                    continue;
                }
                let mut best: Option<Sample> = None;
                for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                    let (nx, ny) = (x as i64 + dx * step as i64, y as i64 + dy * step as i64);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let cand = src[ny as usize * w + nx as usize];
                    if cand.covered {
                        best = Some(cand);
                        break;
                    }
                }
                if let Some(c) = best {
                    samples[at] = Sample { covered: false, ..c };
                }
            }
        }
    }
}

fn filter_chart(chart: &Chart, samples: &mut [Sample], strength: f32, iteration: u32) {
    let (w, h) = (chart.inner[0] as usize, chart.inner[1] as usize);
    let kernel = a_trous_kernel(iteration);
    let src = samples.to_vec();
    for y in 0..h {
        for x in 0..w {
            let centre = src[y * w + x];
            if !centre.covered {
                continue;
            }
            let mut acc = centre.colour * kernel[0][0];
            let mut wsum: f32 = kernel[0][0];
            for (dy, row) in kernel.iter().enumerate() {
                for (dx, kw) in row.iter().enumerate() {
                    if *kw <= 0.0 || (dx == 1 && dy == 1) {
                        continue;
                    }
                    let (sx, sy) = (x as i64 + dx as i64 - 1, y as i64 + dy as i64 - 1);
                    if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                        continue;
                    }
                    let cand = src[sy as usize * w + sx as usize];
                    if !cand.covered {
                        continue;
                    }
                    let wn = cand.normal.normalize_or_zero().dot(centre.normal.normalize_or_zero()).max(0.0);
                    let wn = wn * wn;
                    let k = *kw * wn;
                    acc += cand.colour * k;
                    wsum += k;
                }
            }
            if wsum <= 0.0 {
                continue;
            }
            let filtered = acc / wsum;
            samples[y * w + x].colour = centre.colour.mul_add(Vec3::splat(1.0 - strength), filtered * strength);
        }
    }
}

/// The 3x3 B-spline kernel a-trous filtering uses, widened by the step.
pub fn a_trous_kernel(step: u32) -> [[f32; 3]; 3] {
    let s = 1u32 << step;
    let b = [1.0f32 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let mut k = [[0.0f32; 3]; 3];
    for (j, row) in k.iter_mut().enumerate() {
        for (i, slot) in row.iter_mut().enumerate() {
            *slot = b[j] * b[i] * (s * s) as f32;
        }
    }
    let total: f32 = k.iter().flatten().sum();
    for row in k.iter_mut() {
        for slot in row.iter_mut() {
            *slot /= total;
        }
    }
    k
}

/// Filter every chart of a page and write the result back.
pub fn denoise_page(scene: &rir::BakeScene, layout: &ChartLayout, page: &mut BakedPage, iterations: u32, strength: f32) {
    if iterations == 0 || strength <= 0.0 {
        return;
    }
    let page_index = page.index;
    for chart in layout.charts.iter().filter(|c| c.page == page_index) {
        let (w, h) = (chart.inner[0] as usize, chart.inner[1] as usize);
        if w == 0 || h == 0 {
            continue;
        }
        // Only texels a triangle covered take part, each guided by its own
        // normal, so a chart holding several faces never filters across them.
        let texels = match scene.surfaces.get(chart.surface) {
            Some(s) => crate::light::rasterize(chart, s),
            None => Vec::new(),
        };
        let mut samples = vec![Sample::default(); w * h];
        for y in 0..h {
            for x in 0..w {
                let px = chart.origin[0] as usize + x;
                let py = chart.origin[1] as usize + y;
                if px >= page.width as usize || py >= page.height as usize {
                    continue;
                }
                let Some(Some(t)) = texels.get(y * w + x) else { continue };
                samples[y * w + x] = Sample { colour: page.colour(px, py), normal: t.normal, covered: true };
            }
        }
        dilate_chart(chart, &mut samples, 2);
        denoise_chart(chart, &mut samples, iterations, strength);
        for y in 0..h {
            for x in 0..w {
                let px = chart.origin[0] as usize + x;
                let py = chart.origin[1] as usize + y;
                if px >= page.width as usize || py >= page.height as usize {
                    continue;
                }
                let s = samples[y * w + x];
                // Charts laid out by the map share their rectangles, so a
                // texel only this chart's triangles reached is all it writes.
                if layout.fixed && !s.covered {
                    continue;
                }
                let prev = page.colour(px, py);
                let sun = page.sun(px, py);
                page.set(px, py, s.colour.max(prev), sun);
            }
        }
    }
}


/// Gives every texel no triangle reached the values of its nearest covered
/// neighbour, out to `radius` texels, so a bilinear tap at a chart's edge reads
/// the chart and not black.
pub fn fill_uncovered(page: &mut BakedPage, covered: &[bool], radius: u32) {
    let (w, h) = (page.width as usize, page.height as usize);
    if covered.len() != w * h {
        return;
    }
    let mut known = covered.to_vec();
    for _ in 0..radius {
        let before = known.clone();
        for y in 0..h {
            for x in 0..w {
                let at = y * w + x;
                if before[at] {
                    continue;
                }
                let from = [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)].into_iter().find_map(|(dx, dy)| {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        return None;
                    }
                    let n = ny as usize * w + nx as usize;
                    before[n].then_some(n)
                });
                if let Some(n) = from {
                    page.light[at] = page.light[n];
                    page.dir[at] = page.dir[n];
                    page.sun_vis[at] = page.sun_vis[n];
                    known[at] = true;
                }
            }
        }
    }
}
