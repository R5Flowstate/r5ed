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
    /// The sky vector image 1 stores, filtered with the colour.
    pub dir: Vec3,
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
    if strength <= 0.0 {
        return;
    }
    clamp_speckles(w, h, samples);
    for i in 0..iterations {
        filter_chart(chart, samples, strength, i);
    }
}

/// How far above its neighbourhood's median a texel may sit before it counts
/// as a speckle: one ray that found a sunlit patch the rest missed.
const SPECKLE_RATIO: f32 = 2.5;

/// Pulls each texel brighter than its 3x3 neighbourhood's median by more than
/// [`SPECKLE_RATIO`] down to that bound. A filter only spreads such a texel
/// into a blob; real light edges span more than one texel and keep their
/// neighbours' support.
fn clamp_speckles(w: usize, h: usize, samples: &mut [Sample]) {
    let lum = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    let src = samples.to_vec();
    let mut near: Vec<f32> = Vec::with_capacity(9);
    for y in 0..h {
        for x in 0..w {
            let at = y * w + x;
            if !src[at].covered {
                continue;
            }
            near.clear();
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (sx, sy) = (x as i64 + dx, y as i64 + dy);
                    if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                        continue;
                    }
                    let c = src[sy as usize * w + sx as usize];
                    if c.covered {
                        near.push(lum(c.colour));
                    }
                }
            }
            if near.len() < 5 {
                continue;
            }
            near.sort_by(f32::total_cmp);
            let bound = near[near.len() / 2] * SPECKLE_RATIO + 1e-4;
            let l = lum(src[at].colour);
            if l > bound {
                samples[at].colour = src[at].colour * (bound / l);
            }
        }
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
    const B: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let (w, h) = (chart.inner[0] as usize, chart.inner[1] as usize);
    let step = 1i64 << iteration;
    let src = samples.to_vec();
    for y in 0..h {
        for x in 0..w {
            let centre = src[y * w + x];
            if !centre.covered {
                continue;
            }
            let n0 = centre.normal.normalize_or_zero();
            let (mut acc, mut acc_dir, mut acc_len, mut wsum) = (Vec3::ZERO, Vec3::ZERO, 0.0f32, 0.0f32);
            for (j, bj) in B.iter().enumerate() {
                for (i, bi) in B.iter().enumerate() {
                    let (sx, sy) = (x as i64 + (i as i64 - 2) * step, y as i64 + (j as i64 - 2) * step);
                    if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                        continue;
                    }
                    let cand = src[sy as usize * w + sx as usize];
                    if !cand.covered {
                        continue;
                    }
                    let wn = cand.normal.normalize_or_zero().dot(n0).max(0.0);
                    let k = bi * bj * wn * wn;
                    acc += cand.colour * k;
                    acc_dir += cand.dir * k;
                    acc_len += cand.dir.length() * k;
                    wsum += k;
                }
            }
            if wsum <= 0.0 {
                continue;
            }
            let t = &mut samples[y * w + x];
            t.colour = centre.colour.lerp(acc / wsum, strength);
            // Averaging directions shortens the vector, and its length is the
            // sky's openness: the filtered direction keeps the filtered length.
            let dir = acc_dir.normalize_or_zero() * (acc_len / wsum);
            t.dir = centre.dir.lerp(dir, strength);
        }
    }
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
                samples[y * w + x] = Sample { colour: page.colour(px, py), dir: page.dir[page.width as usize * py + px], normal: t.normal, covered: true };
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
                let sun = page.sun(px, py);
                // A fixed layout's charts share rectangles: each keeps the light
                // of whichever chart lit the texel most.
                let colour = if layout.fixed { s.colour.max(page.colour(px, py)) } else { s.colour };
                page.set(px, py, colour, sun);
                if s.covered {
                    page.set_dir(px, py, s.dir);
                }
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
