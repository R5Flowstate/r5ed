//! Sculpt strokes. A stroke is a polyline of samples with a radius, a
//! strength and a falloff; applying one to a heightfield returns the rect it
//! dirtied so the editor only re-meshes the affected chunks.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::erosion::{self, HydraulicParams, ThermalParams};
use crate::filters::{self, BlendMode};
use crate::heightfield::{Heightfield, Rect};
use crate::mask::Mask;
use crate::math;
use crate::noise::{FractalParams, Noise};
use crate::rng::substream;

/// The shape operator a stroke applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SculptTool {
    /// Push the surface up.
    #[default]
    Raise,
    /// Push the surface down.
    Lower,
    /// Average towards the local mean, which rounds off detail.
    Smooth,
    /// Pull the surface towards `target_height` at full weight.
    Flatten,
    /// Move the surface to `target_height` outright.
    SetHeight,
    /// Add fractal noise scaled by pressure.
    Noise,
    /// Run local hydraulic erosion inside the brush.
    Erode,
    /// Quantise into benches.
    Terrace,
    /// Taper the surface linearly between the first and last point.
    Ramp,
    /// Stamp a brush heightfield with a blend mode.
    Stamp,
}

/// One brush stroke. Points are `[x, y, pressure]` in object-space inches,
/// where x and y are world positions and pressure is 0..1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub tool: SculptTool,
    pub points: Vec<[f32; 3]>,
    /// Brush radius in inches.
    pub radius: f32,
    /// Inches of effect at full pressure for raise and lower.
    pub strength: f32,
    /// How quickly the brush fades, 0 is hard and 1 is a linear cone.
    pub falloff: f32,
    /// Height the flatten, set-height and terrace tools aim at.
    pub target_height: f32,
    /// Brush used by the stamp tool, in the same units as the target.
    pub brush: Option<Heightfield>,
    /// Clockwise rotation of the stamp brush, in radians.
    pub rotation: f32,
    /// Blend mode used by the stamp tool.
    pub blend: BlendMode,
    /// Benches per inch for the terrace tool.
    pub step: f32,
    /// Seed for the noise and erode tools.
    pub seed: u64,
    /// Noise scale in cycles per inch for the noise tool.
    pub noise_frequency: f32,
}

impl Default for Stroke {
    fn default() -> Self {
        Self {
            tool: SculptTool::Raise,
            points: Vec::new(),
            radius: 128.0,
            strength: 32.0,
            falloff: 0.5,
            target_height: 0.0,
            brush: None,
            rotation: 0.0,
            blend: BlendMode::Add,
            step: 24.0,
            seed: 0,
            noise_frequency: 0.01,
        }
    }
}

/// What a stroke did, so the editor can drive a preview and an undo entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeResult {
    /// Samples that changed, half-open on the high edges.
    pub dirty: Rect,
    /// Sum of the height change over the dirty rect, in inches.
    pub net_change: f32,
    /// Largest absolute height change inside the dirty rect, in inches.
    pub max_change: f32,
    /// Samples whose height actually moved.
    pub touched: u32,
}

impl StrokeResult {
    fn empty() -> Self {
        Self { dirty: Rect::new(0, 0, 0, 0), net_change: 0.0, max_change: 0.0, touched: 0 }
    }
}

/// Distance from a point to the stroke polyline, plus the interpolated
/// pressure at the closest point.
fn stroke_distance(points: &[[f32; 3]], x: f32, y: f32) -> (f32, f32) {
    let mut best = f32::INFINITY;
    let mut pressure = 0.0f32;
    if points.len() == 1 {
        let dx = x - points[0][0];
        let dy = y - points[0][1];
        return (math::sqrt((dx * dx + dy * dy) as f64) as f32, points[0][2]);
    }
    for seg in 0..points.len() - 1 {
        let a = points[seg];
        let b = points[seg + 1];
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            math::saturate(((x - a[0]) * dx + (y - a[1]) * dy) as f64 / len2 as f64) as f32
        } else {
            0.0
        };
        let cx = a[0] + dx * t;
        let cy = a[1] + dy * t;
        let ex = x - cx;
        let ey = y - cy;
        let d = math::sqrt((ex * ex + ey * ey) as f64) as f32;
        if d < best {
            best = d;
            pressure = a[2] + (b[2] - a[2]) * t;
        }
    }
    (best, pressure)
}

/// How strongly a sample at distance `d` from the stroke is affected, 0..1.
#[inline]
fn weight(d: f32, radius: f32, falloff: f32) -> f32 {
    if radius <= 0.0 || d >= radius {
        return 0.0;
    }
    let t = 1.0 - d / radius;
    let f = math::saturate(falloff as f64) as f32;
    // Falloff 0 is a flat disc, 1 is a cone, and anything between blends.
    (1.0 - f) + f * math::smoothstep(t as f64) as f32
}

/// The sample-space bounding box a stroke can touch.
fn stroke_bounds(hf: &Heightfield, s: &Stroke) -> Rect {
    if s.points.is_empty() || s.radius <= 0.0 {
        return Rect::new(0, 0, 0, 0);
    }
    let cell = hf.cell_size.max(1e-4);
    let mut lo = (f32::INFINITY, f32::INFINITY);
    let mut hi = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in &s.points {
        lo.0 = lo.0.min(p[0]);
        lo.1 = lo.1.min(p[1]);
        hi.0 = hi.0.max(p[0]);
        hi.1 = hi.1.max(p[1]);
    }
    // A stamp brush extends past the polyline, so pad by the brush as well.
    let extra = match (&s.tool, &s.brush) {
        (SculptTool::Stamp, Some(b)) => {
            b.extent_x().max(b.extent_y()) * 0.5 * (1.0 + s.radius / hf.extent_x().max(1.0))
        }
        _ => 0.0,
    };
    let pad = s.radius + extra;
    let (cx, cy) = hf.sample_to_world((hf.width as f32 - 1.0) * 0.5, (hf.height as f32 - 1.0) * 0.5);
    let half_w = (hf.width as f32 - 1.0) * 0.5;
    let half_h = (hf.height as f32 - 1.0) * 0.5;
    let to_sample_x = |wx: f32| ((wx - cx) / cell + half_w) as i64;
    let to_sample_y = |wy: f32| ((wy - cy) / cell + half_h) as i64;
    let x0 = to_sample_x(lo.0 - pad).clamp(0, half_w as i64) as u32;
    let y0 = to_sample_y(lo.1 - pad).clamp(0, half_h as i64) as u32;
    let x1 = (to_sample_x(hi.0 + pad) + 1).clamp(0, hf.width as i64) as u32;
    let y1 = (to_sample_y(hi.1 + pad) + 1).clamp(0, hf.height as i64) as u32;
    Rect::new(x0, y0, x1, y1)
}

fn validate(hf: &Heightfield, s: &Stroke) -> Result<()> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    if s.points.is_empty() {
        return Err(Error::BadStroke("a stroke needs at least one point"));
    }
    for p in &s.points {
        if !p[0].is_finite() || !p[1].is_finite() || !p[2].is_finite() {
            return Err(Error::BadStroke("stroke points must be finite"));
        }
    }
    if !s.radius.is_finite() || s.radius <= 0.0 {
        return Err(Error::BadStroke("stroke radius must be positive"));
    }
    if matches!(s.tool, SculptTool::Stamp) && s.brush.is_none() {
        return Err(Error::BadStroke("a stamp stroke needs a brush heightfield"));
    }
    if matches!(s.tool, SculptTool::Terrace) && s.step <= 0.0 {
        return Err(Error::BadStroke("a terrace stroke needs a positive step"));
    }
    Ok(())
}

/// Apply a stroke in place and report what changed.
///
/// The returned rect is the tight bounding box of the samples that actually
/// moved, so a stroke that only grazes the edge of the grid reports a small
/// rect rather than the whole bounding box.
pub fn apply_stroke(hf: &mut Heightfield, s: &Stroke) -> Result<StrokeResult> {
    validate(hf, s)?;
    let bounds = stroke_bounds(hf, s);
    if bounds.is_empty() {
        return Ok(StrokeResult::empty());
    }
    let before = hf.clone();
    match s.tool {
        SculptTool::Raise | SculptTool::Lower | SculptTool::Noise => {
            displace(hf, s, bounds)?;
        }
        SculptTool::Smooth => smooth_stroke(hf, s, bounds)?,
        SculptTool::Flatten | SculptTool::SetHeight => flatten(hf, s, bounds)?,
        SculptTool::Terrace => terrace_stroke(hf, s, bounds)?,
        SculptTool::Ramp => ramp(hf, s, bounds)?,
        SculptTool::Erode => {
            let sub = hf.crop(bounds.x0, bounds.y0, bounds.width(), bounds.height())?;
            let params = HydraulicParams::brush((s.radius / hf.cell_size) as u32, 1500);
            let eroded = erosion::hydraulic(&sub, &params, substream(s.seed, 0x4552_4f44))?;
            for (i, v) in hf.data.iter_mut().enumerate() {
                let (x, y) = (i % hf.width as usize, i / hf.width as usize);
                if x < bounds.x0 as usize || y < bounds.y0 as usize {
                    continue;
                }
                if x >= (bounds.x0 + bounds.width()) as usize || y >= (bounds.y0 + bounds.height()) as usize
                {
                    continue;
                }
                let k = (y - bounds.y0 as usize) * bounds.width() as usize + (x - bounds.x0 as usize);
                *v += eroded.data[k] - before.data[i];
            }
        }
        SculptTool::Stamp => {
            stamp_stroke(hf, s, bounds)?;
        }
    }
    hf.sanitize(hf.min());
    Ok(tight_dirty(hf, &before, bounds))
}

/// The bounding box of the samples that differ between the two grids.
fn tight_dirty(hf: &Heightfield, before: &Heightfield, hint: Rect) -> StrokeResult {
    let (mut x0, mut y0) = (u32::MAX, u32::MAX);
    let (mut x1, mut y1) = (0u32, 0u32);
    let (mut net, mut worst, mut touched) = (0.0f32, 0.0f32, 0u32);
    let hint = hint.clamp_to(hf.width, hf.height);
    for y in hint.y0..hint.y1 {
        for x in hint.x0..hint.x1 {
            let i = hf.index(x, y);
            let d = hf.data[i] - before.data[i];
            if d == 0.0 {
                continue;
            }
            touched += 1;
            net += d;
            worst = worst.max(d.abs());
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    if touched == 0 {
        return StrokeResult::empty();
    }
    StrokeResult { dirty: Rect::new(x0, y0, x1, y1), net_change: net, max_change: worst, touched }
}

fn displace(hf: &mut Heightfield, s: &Stroke, r: Rect) -> Result<()> {
    let noise = Noise::new(substream(s.seed, 0x4e4f_4953));
    let params = FractalParams {
        octaves: 4,
        frequency: math::max(s.noise_frequency as f64, 1e-6),
        ..FractalParams::default()
    };
    let sign = if s.tool == SculptTool::Lower { -1.0 } else { 1.0 };
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let (wx, wy) = hf.sample_to_world(x as f32, y as f32);
            let (d, pressure) = stroke_distance(&s.points, wx, wy);
            let w = weight(d, s.radius, s.falloff) * math::saturate(pressure as f64) as f32;
            if w <= 0.0 {
                continue;
            }
            let amount = if s.tool == SculptTool::Noise {
                let v = crate::noise::fractal2(crate::noise::Fractal::Fbm, &noise, &params, wx as f64, wy as f64);
                (v as f32) * s.strength * w
            } else {
                s.strength * w * sign
            };
            let i = hf.index(x, y);
            hf.data[i] += amount;
        }
    }
    Ok(())
}

fn smooth_stroke(hf: &mut Heightfield, s: &Stroke, r: Rect) -> Result<()> {
    // Padded so the blur at the edge of the window still sees real data.
    let win = r.pad(1).clamp_to(hf.width, hf.height);
    let patch = hf.crop(win.x0, win.y0, win.width(), win.height())?;
    let mut blurred = patch.clone();
    filters::smooth(&mut blurred, 1, 1);
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let (wx, wy) = hf.sample_to_world(x as f32, y as f32);
            let (d, pressure) = stroke_distance(&s.points, wx, wy);
            let w = weight(d, s.radius, s.falloff) * math::saturate(pressure as f64) as f32;
            if w <= 0.0 {
                continue;
            }
            let k = blurred.index(x - win.x0, y - win.y0);
            let i = hf.index(x, y);
            hf.data[i] += (blurred.data[k] - hf.data[i]) * w;
        }
    }
    Ok(())
}

fn flatten(hf: &mut Heightfield, s: &Stroke, r: Rect) -> Result<()> {
    let hard = s.tool == SculptTool::SetHeight;
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let (wx, wy) = hf.sample_to_world(x as f32, y as f32);
            let (d, pressure) = stroke_distance(&s.points, wx, wy);
            let w = weight(d, s.radius, s.falloff) * math::saturate(pressure as f64) as f32;
            if w <= 0.0 {
                continue;
            }
            let i = hf.index(x, y);
            hf.data[i] = if hard { s.target_height } else { hf.data[i] + (s.target_height - hf.data[i]) * w };
        }
    }
    Ok(())
}

fn terrace_stroke(hf: &mut Heightfield, s: &Stroke, r: Rect) -> Result<()> {
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let (wx, wy) = hf.sample_to_world(x as f32, y as f32);
            let (d, pressure) = stroke_distance(&s.points, wx, wy);
            let w = weight(d, s.radius, s.falloff) * math::saturate(pressure as f64) as f32;
            if w <= 0.0 {
                continue;
            }
            let i = hf.index(x, y);
            let t = hf.data[i] / s.step;
            let base = t.floor();
            let frac = t - base;
            let stepped = (base + math::smootherstep(frac as f64) as f32) * s.step;
            hf.data[i] += (stepped - hf.data[i]) * w;
        }
    }
    Ok(())
}

fn ramp(hf: &mut Heightfield, s: &Stroke, r: Rect) -> Result<()> {
    if s.points.len() < 2 {
        return Err(Error::BadStroke("a ramp stroke needs two points"));
    }
    let a = s.points[0];
    let b = s.points[s.points.len() - 1];
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    if len2 <= 0.0 {
        return Err(Error::BadStroke("a ramp stroke needs two distinct points"));
    }
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let (wx, wy) = hf.sample_to_world(x as f32, y as f32);
            let (d, pressure) = stroke_distance(&s.points, wx, wy);
            let w = weight(d, s.radius, s.falloff) * math::saturate(pressure as f64) as f32;
            if w <= 0.0 {
                continue;
            }
            let t = math::saturate(((wx - a[0]) * dx + (wy - a[1]) * dy) as f64 / len2 as f64) as f32;
            let i = hf.index(x, y);
            hf.data[i] += (s.target_height - hf.data[i]) * w * t;
        }
    }
    Ok(())
}

fn stamp_stroke(hf: &mut Heightfield, s: &Stroke, r: Rect) -> Result<()> {
    let brush = s.brush.as_ref().ok_or(Error::BadStroke("a stamp stroke needs a brush heightfield"))?;
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let (wx, wy) = hf.sample_to_world(x as f32, y as f32);
            let (d, pressure) = stroke_distance(&s.points, wx, wy);
            let w = weight(d, s.radius, s.falloff) * math::saturate(pressure as f64) as f32;
            if w <= 0.0 {
                continue;
            }
            // The brush is sampled in world space so it keeps its own scale.
            let b = brush.sample_world(wx, wy, crate::heightfield::Filter::Bilinear) * s.strength;
            let i = hf.index(x, y);
            hf.data[i] = filters::blend(hf.data[i], b, s.blend) * w + hf.data[i] * (1.0 - w);
        }
    }
    Ok(())
}

/// Combine many strokes into one dirty rect, applying them in order.
pub fn apply_strokes(hf: &mut Heightfield, strokes: &[Stroke]) -> Result<Rect> {
    let mut total = Rect::new(0, 0, 0, 0);
    let mut any = false;
    for s in strokes {
        let r = apply_stroke(hf, s)?;
        if r.touched > 0 {
            total = if any { total.union(&r.dirty) } else { r.dirty };
            any = true;
        }
    }
    Ok(total)
}

/// A radial brush mask matching a heightfield's resolution, for tools that
/// weight by a texture rather than by distance.
pub fn radial_mask(hf: &Heightfield, radius: f32, falloff: f32) -> Result<Mask> {
    if radius <= 0.0 {
        return Err(Error::BadStroke("a brush mask needs a positive radius"));
    }
    let half_w = (hf.width as f32 - 1.0) * 0.5;
    let half_h = (hf.height as f32 - 1.0) * 0.5;
    let cell = hf.cell_size.max(1e-4);
    Mask::from_fn(hf.width, hf.height, hf.cell_size, |x, y| {
        let dx = (x as f32 - half_w) * cell;
        let dy = (y as f32 - half_h) * cell;
        weight(math::sqrt((dx * dx + dy * dy) as f64) as f32, radius, falloff)
    })
}

/// Relax a whole grid towards its angle of repose, which is what a sculpt
/// session's "settle" pass runs.
pub fn settle(hf: &mut Heightfield, iterations: u32, talus_angle: f32) -> Result<Rect> {
    let before = hf.clone();
    let params = ThermalParams { iterations, talus_angle, rate: 0.5, talus: 0.6 };
    let out = erosion::thermal(hf, &params)?;
    hf.data = out.data;
    hf.sanitize(hf.min());
    let all = Rect::all(hf.width, hf.height);
    Ok(tight_dirty(hf, &before, all).dirty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::hash_f32;

    fn grid(w: u32, h: u32) -> Heightfield {
        Heightfield::filled(w, h, 8.0, 100.0).unwrap()
    }

    fn at(s: &Stroke, x: f32, y: f32) -> Stroke {
        Stroke { points: vec![[x, y, 1.0]], ..s.clone() }
    }

    #[test]
    fn raise_and_lower_move_opposite_ways() {
        let mut hf = grid(32, 32);
        let s = Stroke {
            tool: SculptTool::Raise,
            radius: 32.0,
            strength: 10.0,
            points: vec![[0.0, 0.0, 1.0]],
            ..Stroke::default()
        };
        let r = apply_stroke(&mut hf, &s).unwrap();
        assert!(r.touched > 10, "only {} samples moved", r.touched);
        assert!(r.net_change > 0.0);
        assert!(hf.get(16, 16).unwrap() > 100.0);
        let peak = hf.get(16, 16).unwrap();
        let mut down = grid(32, 32);
        let r2 = apply_stroke(&mut down, &at(&Stroke { tool: SculptTool::Lower, ..s.clone() }, 0.0, 0.0))
            .unwrap();
        assert!(r2.net_change < 0.0);
        assert!(down.get(16, 16).unwrap() < 100.0);
        // Both strokes use the same brush, so the one that lowers must move the
        // surface by the same distance the one that raised it did.
        let sunk = 100.0 - down.get(16, 16).unwrap();
        assert!((sunk - (peak - 100.0)).abs() <= 1e-4, "sank {sunk} rose {peak}");
    }

    #[test]
    fn dirty_rect_is_tight_and_local() {
        let mut hf = grid(64, 64);
        let s = Stroke {
            tool: SculptTool::Raise,
            radius: 24.0,
            strength: 5.0,
            points: vec![[-252.0, -252.0, 1.0]],
            ..Stroke::default()
        };
        let r = apply_stroke(&mut hf, &s).unwrap();
        assert!(r.touched > 0);
        // The brush sits in one corner, so the dirty rect must not span the map.
        assert!(r.dirty.width() < 16 && r.dirty.height() < 16, "{:?}", r.dirty);
        assert_eq!(r.dirty.x0, 0);
        assert_eq!(r.dirty.y0, 0);
        // Everything outside the reported rect is untouched.
        for y in 0..64u32 {
            for x in 0..64u32 {
                if !r.dirty.contains(x, y) {
                    assert_eq!(hf.get(x, y).unwrap(), 100.0, "({x},{y}) changed outside the rect");
                }
            }
        }
    }

    #[test]
    fn a_stroke_off_the_grid_changes_nothing() {
        let mut hf = grid(32, 32);
        let s = Stroke {
            tool: SculptTool::Raise,
            radius: 8.0,
            strength: 5.0,
            points: vec![[9000.0, 9000.0, 1.0]],
            ..Stroke::default()
        };
        let r = apply_stroke(&mut hf, &s).unwrap();
        assert_eq!(r.touched, 0);
        assert!(r.dirty.is_empty());
        assert!(hf.data.iter().all(|v| *v == 100.0));
    }

    #[test]
    fn pressure_and_falloff_scale_the_effect() {
        let mut soft = grid(32, 32);
        let mut hard = grid(32, 32);
        let base = Stroke { tool: SculptTool::Raise, radius: 40.0, strength: 10.0, ..Stroke::default() };
        let full = at(&base, 0.0, 0.0);
        let half = Stroke { points: vec![[0.0, 0.0, 0.5]], ..full.clone() };
        apply_stroke(&mut soft, &full).unwrap();
        apply_stroke(&mut hard, &half).unwrap();
        assert!(soft.get(16, 16).unwrap() > hard.get(16, 16).unwrap());
        // A hard falloff reaches the rim of the brush, a soft one does not.
        // A sample just inside the brush rim: a hard falloff still reaches it,
        // a soft one has already faded to nothing.
        let rim_x = (16.0 + 40.0 / 8.0 - 1.0) as u32;
        let hard_edge = Stroke { falloff: 0.0, ..base.clone() };
        let soft_edge = Stroke { falloff: 1.0, ..base.clone() };
        let mut a = grid(32, 32);
        let mut b = grid(32, 32);
        apply_stroke(&mut a, &at(&hard_edge, 0.0, 0.0)).unwrap();
        apply_stroke(&mut b, &at(&soft_edge, 0.0, 0.0)).unwrap();
        let ha = a.get(rim_x, 16).unwrap();
        let hb = b.get(rim_x, 16).unwrap();
        assert!(ha > hb, "hard falloff {ha} soft falloff {hb}");
    }

    #[test]
    fn smooth_pulls_towards_the_local_mean() {
        let mut hf = Heightfield::from_fn(32, 32, 8.0, |x, y| {
            if x == 16 && y == 16 {
                400.0
            } else {
                0.0
            }
        })
        .unwrap();
        let s = Stroke {
            tool: SculptTool::Smooth,
            radius: 48.0,
            strength: 1.0,
            points: vec![[0.0, 0.0, 1.0]],
            ..Stroke::default()
        };
        apply_stroke(&mut hf, &s).unwrap();
        assert!(hf.get(16, 16).unwrap() < 400.0);
        assert!(hf.get(16, 16).unwrap() > 0.0);
    }

    #[test]
    fn flatten_and_set_height_aim_at_the_target() {
        let mut flat = grid(32, 32);
        let mut set = grid(32, 32);
        let base = Stroke {
            tool: SculptTool::Flatten,
            radius: 32.0,
            target_height: 50.0,
            points: vec![[0.0, 0.0, 1.0]],
            ..Stroke::default()
        };
        apply_stroke(&mut flat, &base).unwrap();
        let centre = flat.get(16, 16).unwrap();
        assert!(centre < 100.0 && centre > 50.0, "flatten stopped at {centre}");
        apply_stroke(&mut set, &Stroke { tool: SculptTool::SetHeight, ..base.clone() }).unwrap();
        assert!((set.get(16, 16).unwrap() - 50.0).abs() < 1e-3);
    }

    #[test]
    fn noise_brush_adds_detail_without_a_bias() {
        let mut hf = grid(48, 48);
        let s = Stroke {
            tool: SculptTool::Noise,
            radius: 64.0,
            strength: 40.0,
            noise_frequency: 0.05,
            seed: 4,
            points: vec![[0.0, 0.0, 1.0]],
            ..Stroke::default()
        };
        let r = apply_stroke(&mut hf, &s).unwrap();
        assert!(r.touched > 100);
        let mean: f64 = hf.data.iter().map(|v| *v as f64).sum::<f64>() / hf.data.len() as f64;
        assert!((mean - 100.0).abs() < 20.0, "noise biased the field to {mean}");
        // The same seed gives the same detail.
        let mut again = grid(48, 48);
        apply_stroke(&mut again, &s).unwrap();
        assert_eq!(hash_f32(&hf.data), hash_f32(&again.data));
    }

    #[test]
    fn terrace_bench_only_inside_the_brush() {
        let mut hf = Heightfield::from_fn(32, 32, 8.0, |x, _| x as f32 * 20.0).unwrap();
        let s = Stroke {
            tool: SculptTool::Terrace,
            radius: 40.0,
            step: 32.0,
            points: vec![[0.0, 0.0, 1.0]],
            ..Stroke::default()
        };
        let r = apply_stroke(&mut hf, &s).unwrap();
        assert!(r.touched > 0);
        // Far outside the brush the ramp is untouched.
        assert_eq!(hf.get(0, 31).unwrap(), 0.0);
    }

    #[test]
    fn ramp_slopes_from_start_to_end() {
        let mut hf = grid(32, 32);
        let s = Stroke {
            tool: SculptTool::Ramp,
            radius: 400.0,
            target_height: 200.0,
            points: vec![[-128.0, 0.0, 1.0], [128.0, 0.0, 1.0]],
            ..Stroke::default()
        };
        apply_stroke(&mut hf, &s).unwrap();
        let west = hf.get(4, 16).unwrap();
        let east = hf.get(27, 16).unwrap();
        assert!(east - west > 50.0, "ramp went {west} -> {east}");
        for x in 5..27u32 {
            let a = hf.get(x - 1, 16).unwrap();
            let b = hf.get(x, 16).unwrap();
            assert!(b > a, "the ramp is not monotonic at {x}: {a} -> {b}");
        }
        assert!(hash_f32(&hf.data).ne(&0));
    }

    #[test]
    fn ramp_needs_two_distinct_points() {
        let mut hf = grid(16, 16);
        let one = Stroke { tool: SculptTool::Ramp, points: vec![[0.0, 0.0, 1.0]], ..Stroke::default() };
        assert!(matches!(apply_stroke(&mut hf, &one), Err(Error::BadStroke(_))));
        let same = Stroke {
            tool: SculptTool::Ramp,
            points: vec![[1.0, 1.0, 1.0], [1.0, 1.0, 1.0]],
            ..Stroke::default()
        };
        assert!(matches!(apply_stroke(&mut hf, &same), Err(Error::BadStroke(_))));
    }

    #[test]
    fn erode_cuts_channels_inside_the_brush() {
        // A cone gives the droplets a single clear downhill path, so the brush
        // has something real to cut.
        let mut hf = Heightfield::from_fn(64, 64, 4.0, |x, y| {
            let dx = (x as f32 - 32.0) / 8.0;
            let dy = (y as f32 - 32.0) / 8.0;
            900.0 * (1.0 - (dx * dx + dy * dy).min(1.0))
        })
        .unwrap();
        let s = Stroke {
            tool: SculptTool::Erode,
            radius: 96.0,
            seed: 9,
            points: vec![[0.0, 0.0, 1.0]],
            ..Stroke::default()
        };
        let before = hf.clone();
        let r = apply_stroke(&mut hf, &s).unwrap();
        assert!(r.touched > 50, "erode touched {} samples", r.touched);
        assert_ne!(hash_f32(&hf.data), hash_f32(&before.data));
        assert!(hf.is_finite());
    }

    #[test]
    fn stamp_uses_the_brush_and_the_blend_mode() {
        let brush = Heightfield::from_fn(9, 9, 8.0, |x, y| {
            let dx = x as f32 - 4.0;
            let dy = y as f32 - 4.0;
            if dx * dx + dy * dy <= 16.0 {
                50.0
            } else {
                0.0
            }
        })
        .unwrap();
        let mut hf = grid(48, 48);
        let s = Stroke {
            tool: SculptTool::Stamp,
            radius: 64.0,
            strength: 1.0,
            brush: Some(brush),
            blend: BlendMode::Add,
            points: vec![[0.0, 0.0, 1.0]],
            ..Stroke::default()
        };
        let r = apply_stroke(&mut hf, &s).unwrap();
        assert!(r.touched > 10);
        assert!(hf.max() > 100.0);
        // A replace blend lands on the brush value itself, and an add blend adds
        // it to the surface underneath.
        let mut replaced = grid(48, 48);
        let replace = Stroke { blend: BlendMode::Replace, falloff: 0.0, ..s.clone() };
        apply_stroke(&mut replaced, &replace).unwrap();
        let put = replaced.get(24, 24).unwrap();
        assert!((put - 50.0).abs() < 1.0, "replace left {put}");
        let mut added = grid(48, 48);
        let add = Stroke { blend: BlendMode::Add, falloff: 0.0, ..s.clone() };
        apply_stroke(&mut added, &add).unwrap();
        let sum = added.get(24, 24).unwrap();
        assert!((sum - 150.0).abs() < 1.0, "add left {sum}");
    }

    #[test]
    fn invalid_strokes_are_rejected() {
        let mut hf = grid(16, 16);
        let no_points = Stroke::default();
        assert!(matches!(apply_stroke(&mut hf, &no_points), Err(Error::BadStroke(_))));
        let nan = Stroke { points: vec![[f32::NAN, 0.0, 1.0]], ..Stroke::default() };
        assert!(matches!(apply_stroke(&mut hf, &nan), Err(Error::BadStroke(_))));
        let no_radius = Stroke { radius: 0.0, points: vec![[0.0, 0.0, 1.0]], ..Stroke::default() };
        assert!(matches!(apply_stroke(&mut hf, &no_radius), Err(Error::BadStroke(_))));
        let no_brush = Stroke { tool: SculptTool::Stamp, points: vec![[0.0, 0.0, 1.0]], ..Stroke::default() };
        assert!(matches!(apply_stroke(&mut hf, &no_brush), Err(Error::BadStroke(_))));
        let no_step = Stroke {
            tool: SculptTool::Terrace,
            step: 0.0,
            points: vec![[0.0, 0.0, 1.0]],
            ..Stroke::default()
        };
        assert!(matches!(apply_stroke(&mut hf, &no_step), Err(Error::BadStroke(_))));
        let mut empty = Heightfield { width: 1, height: 1, cell_size: 1.0, data: Vec::new() };
        assert!(apply_stroke(&mut empty, &no_radius).is_err());
    }

    #[test]
    fn apply_strokes_unions_the_dirty_rects() {
        let mut hf = grid(64, 64);
        let a = Stroke {
            tool: SculptTool::Raise,
            radius: 16.0,
            strength: 4.0,
            points: vec![[-200.0, -200.0, 1.0]],
            ..Stroke::default()
        };
        let b = Stroke {
            tool: SculptTool::Raise,
            radius: 16.0,
            strength: 4.0,
            points: vec![[200.0, 200.0, 1.0]],
            ..Stroke::default()
        };
        let r = apply_strokes(&mut hf, &[a, b]).unwrap();
        assert!(r.width() > 32, "union rect {r:?} does not span both corners");
        // No strokes at all leaves an empty rect.
        let none: Vec<Stroke> = Vec::new();
        assert!(apply_strokes(&mut hf, &none).unwrap().is_empty());
    }

    #[test]
    fn radial_mask_falls_off_from_the_centre() {
        let hf = grid(32, 32);
        // A flat falloff is a disc, so every sample inside the radius is one.
        let m = radial_mask(&hf, 64.0, 0.0).unwrap();
        assert_eq!(m.get(15, 15).unwrap(), 1.0);
        let e = m.get(0, 0).unwrap();
        assert!(e < 0.01);
        assert!(e >= 0.0);
        // A soft falloff still peaks in the middle and falls off outwards.
        let soft = radial_mask(&hf, 96.0, 1.0).unwrap();
        // Twelve cells of radius at eight inches a cell, so the disc covers
        // samples 3 to 27 along a row through the middle.
        let mut prev = soft.get(15, 16).unwrap();
        assert!(prev > 0.9, "the soft mask does not peak in the middle");
        for x in 16..28u32 {
            let v = soft.get(x, 16).unwrap();
            assert!(v <= prev, "the mask rose walking outwards at {x}: {v} after {prev}");
            prev = v;
        }
        assert_eq!(soft.get(2, 16).unwrap(), 0.0, "the soft mask reaches past its radius");
        assert_eq!(soft.get(0, 16).unwrap(), 0.0, "the soft mask reaches past its radius");
        assert!(radial_mask(&hf, 0.0, 0.5).is_err());
    }

    #[test]
    fn settle_relaxes_a_spike_and_reports_the_dirty_rect() {
        let mut hf = Heightfield::from_fn(32, 32, 4.0, |x, y| if x == 16 && y == 16 { 500.0 } else { 0.0 })
            .unwrap();
        let r = settle(&mut hf, 40, 34.0).unwrap();
        assert!(r.width() > 0 && r.height() > 0);
        assert!(hf.max() < 500.0);
        assert!(hf.is_finite());
    }

    #[test]
    fn stroke_distance_and_weight_handle_degenerate_input() {
        let one = vec![[0.0, 0.0, 1.0]];
        let (d, p) = stroke_distance(&one, 3.0, 4.0);
        assert!((d - 5.0).abs() < 1e-4);
        assert_eq!(p, 1.0);
        // A repeated point has no direction, so it is treated as a single point.
        let flat = vec![[1.0, 1.0, 0.5], [1.0, 1.0, 0.5]];
        let (d2, p2) = stroke_distance(&flat, 1.0, 1.0);
        assert_eq!(d2, 0.0);
        assert_eq!(p2, 0.5);
        assert_eq!(weight(10.0, 0.0, 0.5), 0.0);
        assert_eq!(weight(10.0, 5.0, 0.5), 0.0);
        assert!(weight(0.0, 5.0, 0.5) > 0.99);
    }

    #[test]
    fn sculpt_tools_dirty_rect() {
        let brush = Heightfield::from_fn(9, 9, 8.0, |x, y| {
            if x + y < 9 {
                40.0
            } else {
                0.0
            }
        })
        .unwrap();
        let base = Stroke {
            radius: 40.0,
            strength: 6.0,
            points: vec![[-80.0, -80.0, 1.0]],
            brush: Some(brush),
            seed: 3,
            ..Stroke::default()
        };
        let tools = [
            SculptTool::Raise,
            SculptTool::Lower,
            SculptTool::Smooth,
            SculptTool::Flatten,
            SculptTool::SetHeight,
            SculptTool::Noise,
            SculptTool::Erode,
            SculptTool::Terrace,
            SculptTool::Ramp,
            SculptTool::Stamp,
        ];
        for tool in tools {
            // A tilted plane with a spike on it, so every tool has something to
            // act on: a plane alone would leave the smoothing filters a no-op.
            let mut hf = Heightfield::from_fn(40, 40, 8.0, |x, y| {
                let spike = if (x as i32 - 9).pow(2) + (y as i32 - 9).pow(2) < 4 { 60.0 } else { 0.0 };
                100.0 + (x + y) as f32 + spike
            })
            .unwrap();
            let before = hf.clone();
            let mut s = base.clone();
            s.tool = tool;
            s.target_height = 90.0;
            s.blend = BlendMode::Add;
            if tool == SculptTool::Ramp {
                s.points = vec![[-80.0, -80.0, 1.0], [80.0, 80.0, 1.0]];
            }
            let r = apply_stroke(&mut hf, &s).unwrap_or_else(|e| panic!("{tool:?}: {e}"));
            assert!(r.touched > 0, "{tool:?} touched nothing");
            // The rect is the tight box of the samples that moved.
            let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
            let mut count = 0u32;
            for y in 0..40u32 {
                for x in 0..40u32 {
                    if hf.get(x, y).unwrap() != before.get(x, y).unwrap() {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x + 1);
                        y1 = y1.max(y + 1);
                        count += 1;
                    }
                }
            }
            assert_eq!(r.dirty, Rect::new(x0, y0, x1, y1), "{tool:?} reported a loose rect");
            assert_eq!(r.touched, count, "{tool:?} miscounted");
            // Nothing outside the rect moved. A sample inside the box can still
            // be untouched, because a box is not a disc.
            for y in 0..40u32 {
                for x in 0..40u32 {
                    if hf.get(x, y).unwrap() != before.get(x, y).unwrap() {
                        assert!(r.dirty.contains(x, y), "{tool:?} moved ({x},{y}) outside its rect");
                    }
                }
            }
            assert!(r.dirty.width() < 40 && r.dirty.height() < 40, "{tool:?} touched the whole grid");
            assert!(hf.is_finite(), "{tool:?} left a non-finite sample");
        }
    }
}
