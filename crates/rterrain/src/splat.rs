//! Splat painting and scatter. Layer weights are `u8` maps that sum to 255 per
//! sample, and scatter places models with Poisson-disk sampling filtered by the
//! terrain and by a layer's weight map.

use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::heightfield::{Heightfield, Rect};
use crate::math;
use crate::mask::Mask;
use crate::noise::{FractalParams, Noise};
use crate::rng::{Rng, substream};

/// Which property of the surface a layer keys off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LayerDriver {
    /// Constant weight everywhere the mask allows.
    #[default]
    Constant,
    /// Ramp over a height window, in inches.
    Height,
    /// Ramp over a slope window, in degrees.
    Slope,
    /// Ramp over a curvature window, in inches per cell squared.
    Curvature,
    /// Ramp over normalised flow accumulation.
    Flow,
    /// Ramp over a fractal noise field, which is what breaks up hard edges.
    Noise,
}

/// One splat layer's auto-paint rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerRule {
    /// Layer name, carried through so a caller can match rules to names.
    pub name: String,
    pub driver: LayerDriver,
    /// Lower edge of the window for the chosen driver.
    pub range_lo: f32,
    /// Upper edge of the window.
    pub range_hi: f32,
    /// Weight before normalisation, 0..1.
    pub weight: f32,
    /// Noise seed, used when `driver` is `Noise`.
    pub seed: u64,
    /// Noise frequency in cycles per inch.
    pub frequency: f32,
    /// Blur radius applied to the finished weight map, in cells.
    pub blur: u32,
    /// Raise the rule's score to this power before normalisation, which sharpens
    /// or softens the boundary against its neighbours.
    pub contrast: f32,
}

impl Default for LayerRule {
    fn default() -> Self {
        Self {
            name: String::new(),
            driver: LayerDriver::Constant,
            range_lo: 0.0,
            range_hi: 1.0,
            weight: 1.0,
            seed: 0,
            frequency: 0.01,
            blur: 0,
            contrast: 1.0,
        }
    }
}

impl LayerRule {
    /// Score in 0..1 for one sample, before any per-layer contrast.
    fn score(&self, ctx: &SampleContext, i: usize) -> f32 {
        let raw = match self.driver {
            LayerDriver::Constant => 1.0,
            LayerDriver::Height => ramp(ctx.height[i], self.range_lo, self.range_hi),
            LayerDriver::Slope => ramp(ctx.slope_deg[i], self.range_lo, self.range_hi),
            LayerDriver::Curvature => ramp(ctx.curvature[i], self.range_lo, self.range_hi),
            LayerDriver::Flow => ramp(ctx.flow_norm[i], self.range_lo, self.range_hi),
            LayerDriver::Noise => {
                let cell = ctx.cell_size;
                let n = Noise::new(substream(self.seed, 0x4e4f_4953));
                let f = math::max(self.frequency as f64, 1e-6);
                let v = crate::noise::fractal2(
                    crate::noise::Fractal::Fbm,
                    &n,
                    &FractalParams { octaves: 4, frequency: f, ..FractalParams::default() },
                    (i as f64 % ctx.width as f64) * cell as f64,
                    (i as f64 / ctx.width as f64) * cell as f64,
                );
                ramp(v as f32, self.range_lo, self.range_hi)
            }
        };
        let w = math::saturate(self.weight as f64) as f32 * math::saturate(raw as f64) as f32;
        let k = if self.contrast > 0.0 { self.contrast } else { 1.0 };
        math::pow(w as f64, k as f64) as f32
    }
}

struct SampleContext {
    width: u32,
    cell_size: f32,
    height: Vec<f32>,
    slope_deg: Vec<f32>,
    curvature: Vec<f32>,
    flow_norm: Vec<f32>,
}

/// Smooth 0..1 window across `lo..hi`; either edge may be above the other, which
/// inverts the window so a rule can carve out a band.
fn ramp(v: f32, lo: f32, hi: f32) -> f32 {
    if (hi - lo).abs() < f32::MIN_POSITIVE {
        return if v >= hi { 1.0 } else { 0.0 };
    }
    math::smooth_range(v as f64, lo as f64, hi as f64) as f32
}

/// A stack of per-layer `u8` weight maps, one per layer, each `width` by
/// `height`, and every sample's layer weights summing to exactly 255.
#[derive(Clone, Debug, PartialEq)]
pub struct SplatMaps {
    pub width: u32,
    pub height: u32,
    pub cell_size: f32,
    /// One `u8` map per layer, row-major.
    pub layers: Vec<Vec<u8>>,
}

impl SplatMaps {
    /// Number of layers.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Read one layer's weight, or zero outside the map.
    pub fn weight(&self, layer: usize, x: u32, y: u32) -> u8 {
        if layer >= self.layers.len() || x >= self.width || y >= self.height {
            return 0;
        }
        self.layers[layer][y as usize * self.width as usize + x as usize]
    }

    /// Total weight of a sample across every layer.
    pub fn total(&self, x: u32, y: u32) -> u32 {
        (0..self.layers.len()).map(|l| self.weight(l, x, y) as u32).sum()
    }

    /// The four highest-weight layers at a sample, as `(layer, weight)` pairs
    /// sorted by descending weight then ascending index.
    pub fn top_layers(&self, x: u32, y: u32) -> Vec<(usize, u8)> {
        let mut all: Vec<(usize, u8)> =
            (0..self.layers.len()).map(|l| (l, self.weight(l, x, y))).collect();
        all.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        all.truncate(4);
        all
    }

    /// Layer indices a chunk of samples touches, ascending.
    pub fn layers_in(&self, rect: Rect) -> Vec<usize> {
        let r = rect.clamp_to(self.width, self.height);
        let mut used: Vec<usize> = (0..self.layers.len()).collect();
        used.retain(|l| {
            for y in r.y0..r.y1 {
                for x in r.x0..r.x1 {
                    if self.weight(*l, x, y) > 0 {
                        return true;
                    }
                }
            }
            false
        });
        used
    }

    /// Serialise to raw little-endian `u8`, one map after another.
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.layers.len() * (self.width as usize * self.height as usize));
        for l in &self.layers {
            out.extend_from_slice(l);
        }
        out
    }

    /// Parse raw little-endian `u8` maps.
    pub fn from_le_bytes(
        bytes: &[u8],
        layers: usize,
        width: u32,
        height: u32,
        cell_size: f32,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::ZeroSize);
        }
        let n = width as usize * height as usize;
        if layers == 0 || bytes.len() != n * layers {
            return Err(Error::BadByteLengthU8(n * layers, width, height, bytes.len()));
        }
        let mut maps = Vec::with_capacity(layers);
        for l in 0..layers {
            maps.push(bytes[l * n..(l + 1) * n].to_vec());
        }
        Ok(Self { width, height, cell_size, layers: maps })
    }
}

/// Where a scatter rule's output goes in world space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ObjectTransform {
    /// Object-space origin in world coordinates.
    pub origin: DVec3,
    /// Object-space Z rotation in degrees.
    pub yaw_deg: f64,
}

impl ObjectTransform {
    /// Apply the transform to an object-space position.
    pub fn to_world(&self, p: DVec3) -> DVec3 {
        let r = math::to_radians(self.yaw_deg);
        let (s, c) = (math::sin(r), math::cos(r));
        DVec3::new(p.x * c - p.y * s + self.origin.x, p.x * s + p.y * c + self.origin.y, p.z + self.origin.z)
    }

    /// Apply the transform to an object-space direction.
    pub fn dir_to_world(&self, d: DVec3) -> DVec3 {
        let r = math::to_radians(self.yaw_deg);
        let (s, c) = (math::sin(r), math::cos(r));
        DVec3::new(d.x * c - d.y * s, d.x * s + d.y * c, d.z)
    }
}

/// One placed instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    /// Model path, taken from the rule that produced it.
    pub model: String,
    /// World-space position.
    pub position: DVec3,
    /// World-space orientation.
    pub rotation: DQuat,
    /// Uniform scale.
    pub scale: f32,
    /// Emit as a static prop rather than as a dynamic entity.
    pub static_prop: bool,
}

/// Auto-paint a stack of layer weight maps from a set of rules.
///
/// Weights are normalised so every sample sums to exactly 255: the floor of
/// each share is taken, then the leftover is handed out one at a time to the
/// layers with the largest fractional part, lowest index first. That makes the
/// sum exact and the tie-break deterministic.
pub fn autopaint(hf: &Heightfield, rules: &[LayerRule]) -> Result<SplatMaps> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    if rules.is_empty() {
        return Err(Error::EmptyBlend);
    }
    if rules.len() > 16 {
        return Err(Error::BadParams("at most 16 splat layers are supported"));
    }
    let n = hf.data.len();
    let slope_deg = hf.slope_deg();
    let curvature = hf.curvature();
    let flow = hf.flow_accumulation();
    let max_area = flow.max_area().max(1e-6);
    let ctx = SampleContext {
        width: hf.width,
        cell_size: hf.cell_size,
        height: hf.data.clone(),
        slope_deg,
        curvature,
        flow_norm: flow.area.iter().map(|a| (a / max_area).clamp(0.0, 1.0)).collect(),
    };

    let mut raw = vec![0.0f32; rules.len() * n];
    for (li, rule) in rules.iter().enumerate() {
        for i in 0..n {
            raw[li * n + i] = rule.score(&ctx, i).max(0.0);
        }
    }
    // A soft blur per layer breaks up the hard window edges.
    for (li, rule) in rules.iter().enumerate() {
        if rule.blur > 0 {
            let m = Mask::new(hf.width, hf.height, hf.cell_size, raw[li * n..(li + 1) * n].to_vec())?;
            let blurred = m.blur(rule.blur, 2);
            raw[li * n..(li + 1) * n].copy_from_slice(&blurred.data);
        }
    }

    let mut maps = vec![vec![0u8; n]; rules.len()];
    for i in 0..n {
        let mut total = 0.0f32;
        for li in 0..rules.len() {
            total += raw[li * n + i];
        }
        if total <= 0.0 || !total.is_finite() {
            // No rule wants this sample, so the last layer owns it outright.
            maps[rules.len() - 1][i] = 255;
            continue;
        }
        // Largest remainder first, so the shares always total 255 exactly.
        let mut order: Vec<usize> = (0..rules.len()).collect();
        order.sort_by(|a, b| {
            let fa = raw[b * n + i] / total;
            let fb = raw[a * n + i] / total;
            fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(b))
        });
        let mut assigned = 0u32;
        for li in &order {
            let share = f64::from(raw[li * n + i] / total) * 255.0;
            let q = share.floor();
            let byte = math::clamp(q, 0.0, 255.0) as u32;
            maps[*li][i] = byte.min(255 - assigned) as u8;
            assigned += maps[*li][i] as u32;
        }
        // Hand out the remainder one at a time, largest remainder first.
        let mut guard = rules.len();
        while assigned < 255 && guard > 0 {
            for li in &order {
                if assigned >= 255 {
                    break;
                }
                if maps[*li][i] < 255 {
                    maps[*li][i] += 1;
                    assigned += 1;
                }
            }
            guard -= 1;
        }
    }

    Ok(SplatMaps { width: hf.width, height: hf.height, cell_size: hf.cell_size, layers: maps })
}

/// All the filters a scatter rule applies, named so the UI can show them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ScatterFilters {
    /// Ignore samples whose layer weight is below this fraction.
    pub min_weight: f32,
    /// Allowed slope window, in degrees.
    pub slope_deg: [f32; 2],
    /// Allowed height window, in inches.
    pub height: [f64; 2],
    /// Allowed scale range.
    pub scale: [f32; 2],
    /// 0 keeps models upright, 1 lays them flat on the surface.
    pub align_to_normal: f32,
    /// Instances per 1000x1000 inches of footprint at full weight.
    pub density: f64,
    pub seed: u64,
    pub static_prop: bool,
}

/// How hard to try before giving up on filling the requested density.
const MAX_ATTEMPTS_PER_POINT: u32 = 30;

/// Place models on a heightfield using Poisson-disk sampling.
///
/// Points are accepted only where every filter passes, and the disk radius
/// comes from the rule's density, so the same rule always produces the same
/// placement count for the same terrain. Returns an error only when the
/// requested density is impossible, which is a disk packing limit rather than a
/// bad input.
pub fn scatter(
    hf: &Heightfield,
    splat: Option<&SplatMaps>,
    layer: usize,
    models: &[String],
    filters: &ScatterFilters,
    transform: &ObjectTransform,
) -> Result<Vec<Placement>> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    if models.is_empty() {
        return Err(Error::BadParams("a scatter rule needs at least one model"));
    }
    if filters.density <= 0.0 || !filters.density.is_finite() {
        return Err(Error::BadParams("scatter density must be positive"));
    }
    if let Some(s) = splat {
        if layer >= s.layer_count() {
            return Err(Error::MissingWeightMap(layer));
        }
        if s.width != hf.width || s.height != hf.height {
            return Err(Error::WeightMapSize(
                layer,
                s.width,
                s.height,
                hf.width,
                hf.height,
            ));
        }
    }
    let slope = hf.slope_deg();
    let min_slope = filters.slope_deg[0].min(filters.slope_deg[1]);
    let max_slope = filters.slope_deg[0].max(filters.slope_deg[1]);
    let min_h = filters.height[0].min(filters.height[1]) as f32;
    let max_h = filters.height[0].max(filters.height[1]) as f32;
    let lo_scale = filters.scale[0].min(filters.scale[1]);
    let hi_scale = filters.scale[0].max(filters.scale[1]);
    if hi_scale <= 0.0 {
        return Err(Error::BadParams("scatter scale range must be positive"));
    }

    // The disk radius follows from the density: one point per this much area.
    let cell = hf.cell_size as f64;
    let per_point = 1_000_000.0 / filters.density;
    let radius = (per_point / std::f64::consts::PI).sqrt().max(cell);
    let (fw, fh) = (hf.extent_x() as f64, hf.extent_y() as f64);
    if fw <= 0.0 || fh <= 0.0 {
        return Err(Error::ZeroSize);
    }
    let area = fw * fh;
    // A density below one per footprint still asks for one placement, because a
    // request that rounds to nothing would report an error for a rule the
    // caller wrote down deliberately.
    let target = ((area / per_point).max(1.0)).round() as u32;

    let cell_cols = ((fw / radius).ceil() as usize).max(1) + 2;
    let cell_rows = ((fh / radius).ceil() as usize).max(1) + 2;
    let mut grid: Vec<Vec<(f64, f64)>> = vec![Vec::new(); cell_cols * cell_rows];
    let mut accepted: Vec<(f64, f64, [f32; 3])> = Vec::new();
    let mut rng = Rng::new(substream(filters.seed, 0x5343_4154));
    let mut attempts = 0u32;
    let mut budget = target.saturating_mul(MAX_ATTEMPTS_PER_POINT).max(1024);

    // Bridson's algorithm: candidates are drawn around the points already
    // placed, so the set stays a proper Poisson disk.
    while accepted.len() < target as usize && budget > 0 {
        let (cx, cy) = if accepted.is_empty() {
            (rng.range(0.0, fw), rng.range(0.0, fh))
        } else {
            let k = rng.below(accepted.len() as u32) as usize;
            let (px, py, _) = accepted[k];
            let ang = rng.range(0.0, math::TAU);
            let r = radius * rng.range(1.0, 2.0);
            (px + r * math::cos(ang), py + r * math::sin(ang))
        };
        if !(cx >= 0.0 && cy >= 0.0 && cx < fw && cy < fh) {
            budget = budget.saturating_sub(1);
            continue;
        }
        attempts += 1;
        budget = budget.saturating_sub(1);
        if !poisson_ok(&grid, cell_cols, cell_rows, cx, cy, radius) {
            continue;
        }
        // Filters, in cheapest-first order.
        let (u, v) = hf.world_to_sample(cx as f32, cy as f32);
        let ix = u.round().clamp(0.0, hf.width as f32 - 1.0) as u32;
        let iy = v.round().clamp(0.0, hf.height as f32 - 1.0) as u32;
        let h = hf.get(ix, iy)?;
        if h < min_h || h > max_h {
            continue;
        }
        if slope[hf.index(ix, iy)] < min_slope || slope[hf.index(ix, iy)] > max_slope {
            continue;
        }
        if let Some(s) = splat {
            let w = s.weight(layer, ix, iy) as f32 / 255.0;
            if w < math::saturate(filters.min_weight as f64) as f32 {
                continue;
            }
        }
        poisson_insert(&mut grid, cell_cols, cell_rows, cx, cy, radius);
        accepted.push((cx, cy, hf.normal(ix, iy)));
    }

    if accepted.is_empty() {
        return Err(Error::ScatterStalled(attempts, budget));
    }

    let mut out = Vec::with_capacity(accepted.len());
    for (i, (x, y, n)) in accepted.iter().enumerate() {
        // A stable per-placement draw keeps the same rule reproducible.
        let mut r = Rng::new(substream(filters.seed, 0x504c_434d + i as u64));
        let model = models[r.below(models.len() as u32) as usize].clone();
        let scale = r.range32(lo_scale, hi_scale);
        let align = math::saturate(filters.align_to_normal as f64);
        let nrm = DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64).normalize_or(DVec3::Z);
        // Two small rotations tip the model towards the normal by `align`.
        let tilt = DQuat::from_rotation_x(nrm.y.atan2(nrm.z) * align)
            .mul_quat(DQuat::from_rotation_z(-nrm.x.atan2(nrm.z) * align));
        let spin = DQuat::from_rotation_z(r.range(0.0, math::TAU));
        let wx = *x - fw * 0.5;
        let wy = *y - fh * 0.5;
        let wz = hf.sample_world(wx as f32, wy as f32, crate::heightfield::Filter::Bilinear) as f64;
        out.push(Placement {
            model,
            position: transform.to_world(DVec3::new(wx, wy, wz)),
            rotation: spin * tilt,
            scale: scale.max(0.0),
            static_prop: filters.static_prop,
        });
    }
    Ok(out)
}

fn grid_slot(cols: usize, rows: usize, x: f64, y: f64, radius: f64) -> (usize, usize) {
    let c = (x / radius).floor();
    let r = (y / radius).floor();
    let c = if c.is_finite() && c > 0.0 { c as usize } else { 0 };
    let r = if r.is_finite() && r > 0.0 { r as usize } else { 0 };
    (c.min(cols.saturating_sub(1)), r.min(rows.saturating_sub(1)))
}

fn poisson_ok(
    grid: &[Vec<(f64, f64)>],
    cols: usize,
    rows: usize,
    x: f64,
    y: f64,
    radius: f64,
) -> bool {
    let (c, r) = grid_slot(cols, rows, x, y, radius);
    let span = (radius * 2.0 / radius).ceil() as i64;
    for rr in (r as i64 - span).max(0)..=(r as i64 + span).min(rows as i64 - 1) {
        for cc in (c as i64 - span).max(0)..=(c as i64 + span).min(cols as i64 - 1) {
            for (px, py) in &grid[rr as usize * cols + cc as usize] {
                let dx = px - x;
                let dy = py - y;
                if dx * dx + dy * dy < radius * radius {
                    return false;
                }
            }
        }
    }
    true
}

fn poisson_insert(
    grid: &mut [Vec<(f64, f64)>],
    cols: usize,
    rows: usize,
    x: f64,
    y: f64,
    radius: f64,
) {
    let (c, r) = grid_slot(cols, rows, x, y, radius);
    grid[r * cols + c].push((x, y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heightfield::Rect;

    fn flat(w: u32, h: u32) -> Heightfield {
        Heightfield::filled(w, h, 8.0, 100.0).unwrap()
    }

    fn rules() -> Vec<LayerRule> {
        vec![
            LayerRule {
                name: "rock".into(),
                driver: LayerDriver::Slope,
                range_lo: 25.0,
                range_hi: 45.0,
                weight: 1.0,
                ..LayerRule::default()
            },
            LayerRule {
                name: "grass".into(),
                driver: LayerDriver::Slope,
                range_lo: 20.0,
                range_hi: 0.0,
                weight: 1.0,
                ..LayerRule::default()
            },
            LayerRule { name: "dirt".into(), ..LayerRule::default() },
        ]
    }

    #[test]
    fn autopaint_weights_sum_255() {
        let hf = crate::generators::Generator::Mountains(crate::generators::MountainsParams::default())
            .generate(64, 64, 8.0)
            .unwrap();
        let mut rs = rules();
        rs[0].blur = 1;
        rs[1].contrast = 2.5;
        rs[2].driver = LayerDriver::Flow;
        let maps = autopaint(&hf, &rs).unwrap();
        assert_eq!(maps.layer_count(), 3);
        for y in 0..64u32 {
            for x in 0..64u32 {
                assert_eq!(maps.total(x, y), 255, "({x},{y}) did not sum to 255");
            }
        }
    }

    #[test]
    fn autopaint_is_sensitive_to_every_driver() {
        let hf = crate::generators::Generator::Alpine(crate::generators::AlpineParams::default())
            .generate(48, 48, 8.0)
            .unwrap();
        // Every driver, with a window that matches the units it reads and the
        // spread the terrain actually has.
        let drivers = [
            (LayerDriver::Constant, 0.0, 1.0),
            (LayerDriver::Height, 2500.0, 4000.0),
            (LayerDriver::Slope, 0.0, 60.0),
            (LayerDriver::Curvature, -1.0, 1.0),
            (LayerDriver::Flow, 0.2, 0.9),
            (LayerDriver::Noise, -1.0, 1.0),
        ];
        let mut hashes = Vec::new();
        for (driver, lo, hi) in drivers {
            // The second rule is the flat catch-all, so every share the driver
            // takes is visible as a difference from the catch-all map.
            let r = vec![
                LayerRule {
                    name: "a".into(),
                    driver,
                    range_lo: lo,
                    range_hi: hi,
                    weight: 1.0,
                    ..LayerRule::default()
                },
                LayerRule { name: "b".into(), weight: 1.0, ..LayerRule::default() },
            ];
            let maps = autopaint(&hf, &r).unwrap();
            let painted = maps.layers[0].iter().filter(|b| **b > 0).count();
            assert!(painted > 64, "driver {driver:?} painted only {painted} samples");
            let second = maps.layers[1].iter().filter(|b| **b > 0).count();
            assert!(second > 64, "driver {driver:?} starved the catch-all rule");
            hashes.push(crate::rng::hash_f32(
                &maps.layers[0].iter().map(|b| *b as f32).collect::<Vec<f32>>(),
            ));
        }
        for a in 0..hashes.len() {
            for b in a + 1..hashes.len() {
                assert_ne!(hashes[a], hashes[b], "drivers {a} and {b} produced the same map");
            }
        }
    }

    #[test]
    fn autopaint_rejects_bad_input() {
        let hf = flat(16, 16);
        assert!(matches!(autopaint(&hf, &[]), Err(Error::EmptyBlend)));
        let too_many = vec![LayerRule::default(); 20];
        assert!(matches!(autopaint(&hf, &too_many), Err(Error::BadParams(_))));
        let empty = Heightfield { width: 1, height: 1, cell_size: 1.0, data: Vec::new() };
        assert!(autopaint(&empty, &[LayerRule::default()]).is_err());
    }

    #[test]
    fn layer_picks_the_land_where_slope_is_low() {
        let hf = Heightfield::from_fn(32, 32, 8.0, |x, _| x as f32 * x as f32).unwrap();
        let maps = autopaint(&hf, &rules()).unwrap();
        // Flat ground at x=0 should be grass, the steep x=31 end should be rock.
        assert!(maps.weight(1, 0, 16) > maps.weight(0, 0, 16), "flat ground is not grass");
        assert!(maps.weight(0, 31, 16) > maps.weight(1, 31, 16), "steep ground is not rock");
    }

    #[test]
    fn splat_maps_serialise_and_report_layers() {
        let hf = flat(8, 8);
        let maps = autopaint(&hf, &rules()).unwrap();
        let bytes = maps.to_le_bytes();
        assert_eq!(bytes.len(), 3 * 64);
        let back = SplatMaps::from_le_bytes(&bytes, 3, 8, 8, 8.0).unwrap();
        assert_eq!(back.layers, maps.layers);
        assert_eq!(back.cell_size, 8.0);
        assert!(matches!(SplatMaps::from_le_bytes(&bytes, 0, 8, 8, 8.0), Err(Error::BadByteLengthU8(..))));
        assert!(matches!(SplatMaps::from_le_bytes(&bytes[..10], 3, 8, 8, 8.0), Err(Error::BadByteLengthU8(..))));
        assert!(matches!(SplatMaps::from_le_bytes(&bytes, 3, 0, 8, 8.0), Err(Error::ZeroSize)));
        // The used-layer report only names layers present in the window.
        let used = maps.layers_in(Rect::all(8, 8));
        assert!(!used.is_empty());
        assert!(maps.layers_in(Rect::new(100, 100, 110, 110)).is_empty());
        // The top-four list is sorted, capped, and reads zero outside the map.
        let top = maps.top_layers(0, 0);
        assert!(top.len() <= 4);
        for w in top.windows(2) {
            assert!(w[0].1 >= w[1].1);
        }
        assert!(maps.top_layers(99, 99).iter().all(|(_, w)| *w == 0));
        assert_eq!(maps.weight(9, 0, 0), 0);
    }

    fn models() -> Vec<String> {
        vec!["mdl/rocks/rock_a.mdl".to_string(), "mdl/rocks/rock_b.mdl".to_string()]
    }

    fn base_filters() -> ScatterFilters {
        ScatterFilters {
            min_weight: 0.0,
            slope_deg: [0.0, 90.0],
            height: [f64::NEG_INFINITY, f64::INFINITY],
            scale: [0.8, 1.2],
            align_to_normal: 0.0,
            density: 0.02,
            seed: 7,
            static_prop: true,
        }
    }

    #[test]
    fn scatter_respects_filters() {
        let hf = Heightfield::from_fn(64, 64, 8.0, |x, _| x as f32 * 12.0).unwrap();
        let identity = ObjectTransform::default();
        let all = scatter(&hf, None, 0, &models(), &base_filters(), &identity).unwrap();
        assert!(!all.is_empty());
        assert!(all.iter().all(|p| p.scale >= 0.8 && p.scale <= 1.2));
        assert!(all.iter().all(|p| p.static_prop));
        assert!(all.iter().all(|p| !p.model.is_empty()));

        // The height window rejects everything.
        let high = ScatterFilters { height: [9000.0, 9999.0], ..base_filters() };
        assert!(matches!(
            scatter(&hf, None, 0, &models(), &high, &identity),
            Err(Error::ScatterStalled(..))
        ));
        // The slope window rejects everything on a ramp.
        let flat_slope = ScatterFilters { slope_deg: [0.0, 0.0001], ..base_filters() };
        assert!(matches!(
            scatter(&hf, None, 0, &models(), &flat_slope, &identity),
            Err(Error::ScatterStalled(..))
        ));
    }

    #[test]
    fn scatter_honours_the_layer_weight() {
        let hf = flat(64, 64);
        let maps = autopaint(&hf, &rules()).unwrap();
        let identity = ObjectTransform::default();
        // On flat ground the grass rule takes the whole sample, so nothing
        // reaches the catch-all layer and its weight sits around half.
        let mid = maps.weight(2, 32, 32) as f32 / 255.0;
        assert!(mid > 0.2 && mid < 0.8, "catch-all weight was {mid}");
        let all = scatter(&hf, Some(&maps), 2, &models(), &base_filters(), &identity).unwrap();
        assert!(!all.is_empty());
        // A weight threshold above the map's own share rejects everything.
        let strict = ScatterFilters { min_weight: 0.9, ..base_filters() };
        assert!(matches!(
            scatter(&hf, Some(&maps), 2, &models(), &strict, &identity),
            Err(Error::ScatterStalled(..))
        ));
        // A missing layer index is an error, not a silent empty result.
        assert!(matches!(
            scatter(&hf, Some(&maps), 9, &models(), &base_filters(), &identity),
            Err(Error::MissingWeightMap(9))
        ));
    }

    #[test]
    fn scatter_density_is_within_tolerance() {
        let hf = flat(128, 128);
        let identity = ObjectTransform::default();
        // cell 8 inches over 127 gaps is 1016 units on a side, and the density
        // is instances per million square inches.
        let area = (1016.0 * 1016.0) as f64;
        for density in [1.0, 4.0, 10.0] {
            let f = ScatterFilters { density, ..base_filters() };
            let got = scatter(&hf, None, 0, &models(), &f, &identity).unwrap().len() as f64;
            let want = area * density / 1_000_000.0;
            // Poisson-disk packing never reaches the ideal density, but it
            // must be within a factor of two of it.
            assert!(got > want * 0.4 && got < want * 1.5, "density {density}: {got} vs {want}");
        }
    }

    #[test]
    fn scatter_is_reproducible_and_seed_sensitive() {
        let hf = crate::generators::Generator::RollingHills(
            crate::generators::RollingHillsParams::default(),
        )
        .generate(64, 64, 8.0)
        .unwrap();
        let identity = ObjectTransform::default();
        let a = scatter(&hf, None, 0, &models(), &base_filters(), &identity).unwrap();
        let b = scatter(&hf, None, 0, &models(), &base_filters(), &identity).unwrap();
        assert_eq!(a, b);
        let other = ScatterFilters { seed: 8, ..base_filters() };
        let c = scatter(&hf, None, 0, &models(), &other, &identity).unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn scatter_applies_the_object_transform() {
        let hf = flat(32, 32);
        let t = ObjectTransform { origin: DVec3::new(100.0, -50.0, 7.0), yaw_deg: 90.0 };
        let placed = scatter(&hf, None, 0, &models(), &base_filters(), &t).unwrap();
        assert!(!placed.is_empty());
        for p in &placed {
            // Object space is centred on the grid, so a 90 degree yaw plus the
            // origin must land every point inside the shifted footprint.
            let local = t.to_world(DVec3::new(0.0, 0.0, 0.0));
            assert_eq!(local, DVec3::new(100.0, -50.0, 7.0));
            assert!(p.position.x > 100.0 - 130.0 && p.position.x < 100.0 + 130.0);
            assert!(p.position.y > -50.0 - 130.0 && p.position.y < -50.0 + 130.0);
        }
    }

    #[test]
    fn scatter_aligns_to_the_surface_normal() {
        let hf = Heightfield::from_fn(64, 64, 8.0, |x, _| x as f32 * 4.0).unwrap();
        let identity = ObjectTransform::default();
        let flat = ScatterFilters { align_to_normal: 0.0, ..base_filters() };
        let tilted = ScatterFilters { align_to_normal: 1.0, ..base_filters() };
        let a = scatter(&hf, None, 0, &models(), &flat, &identity).unwrap();
        let b = scatter(&hf, None, 0, &models(), &tilted, &identity).unwrap();
        // Positions are the same either way; only the orientation differs.
        assert_eq!(a.len(), b.len());
        assert_ne!(a[0].rotation, b[0].rotation);
    }

    #[test]
    fn scatter_rejects_bad_parameters() {
        let hf = flat(32, 32);
        let identity = ObjectTransform::default();
        assert!(scatter(&hf, None, 0, &[], &base_filters(), &identity).is_err());
        let zero = ScatterFilters { density: 0.0, ..base_filters() };
        assert!(scatter(&hf, None, 0, &models(), &zero, &identity).is_err());
        let nan = ScatterFilters { density: f64::NAN, ..base_filters() };
        assert!(scatter(&hf, None, 0, &models(), &nan, &identity).is_err());
        let bad_scale = ScatterFilters { scale: [0.0, -1.0], ..base_filters() };
        assert!(scatter(&hf, None, 0, &models(), &bad_scale, &identity).is_err());
        // A single sample grid has no extent to scatter over.
        let tiny = Heightfield::filled(1, 1, 1.0, 0.0).unwrap();
        assert!(scatter(&tiny, None, 0, &models(), &base_filters(), &identity).is_err());
    }

    #[test]
    fn poisson_disk_has_no_close_pairs() {
        let hf = flat(96, 96);
        let identity = ObjectTransform::default();
        let f = ScatterFilters { density: 0.05, ..base_filters() };
        let placed = scatter(&hf, None, 0, &models(), &f, &identity).unwrap();
        // The disk radius implied by the density is the minimum separation.
        let per_point = 1_000_000.0 / 0.05;
        let radius = (per_point / std::f64::consts::PI).sqrt();
        for a in 0..placed.len() {
            for b in a + 1..placed.len() {
                let d = placed[a].position.distance(placed[b].position);
                assert!(d >= radius * 0.999, "points {a} and {b} are only {d} apart");
            }
        }
    }

    #[test]
    fn ramp_handles_inverted_and_degenerate_windows() {
        assert_eq!(ramp(5.0, 0.0, 10.0), 0.5);
        assert_eq!(ramp(15.0, 0.0, 10.0), 1.0);
        assert_eq!(ramp(-5.0, 0.0, 10.0), 0.0);
        assert_eq!(ramp(5.0, 10.0, 0.0), 0.5);
        assert_eq!(ramp(5.0, 3.0, 3.0), 1.0);
        assert_eq!(ramp(1.0, 3.0, 3.0), 0.0);
    }
}
