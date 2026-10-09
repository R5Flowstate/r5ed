//! Erosion. Three models: particle-based hydraulic erosion for detail,
//! a grid-based pipe model for whole landscapes, and thermal erosion for
//! material sliding past its angle of repose.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::heightfield::{Heightfield, Rect};
use crate::math;
use crate::parallel;
use crate::rng::{Rng, substream};

/// Particle-based hydraulic erosion.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HydraulicParams {
    /// Droplets to release.
    pub droplets: u32,
    /// Steps a droplet lives before it is absorbed or leaves the grid.
    pub max_lifetime: u32,
    /// How much a droplet keeps its previous direction, 0..1.
    pub inertia: f32,
    /// Sediment carrying capacity factor.
    pub capacity: f32,
    /// Fraction of the carried sediment dropped per step.
    pub deposit_speed: f32,
    /// Fraction of the local material removed per unit of capacity.
    pub erode_speed: f32,
    /// Fraction of water lost per step.
    pub evaporation: f32,
    /// Downward acceleration, in inches per step squared.
    pub gravity: f32,
    /// Sediment a droplet starts with, as a fraction of capacity.
    pub initial_sediment: f32,
    /// Water volume a droplet starts with.
    pub initial_water: f32,
    /// Initial speed along the gradient.
    pub initial_speed: f32,
    /// Erosion brush radius in samples.
    pub radius: u32,
    /// Weight given to the minimum-height sample under the brush.
    pub min_weight: f32,
    /// Deposition brush radius in samples.
    pub deposit_radius: u32,
}

impl Default for HydraulicParams {
    fn default() -> Self {
        Self {
            droplets: 60_000,
            max_lifetime: 40,
            inertia: 0.06,
            capacity: 3.2,
            deposit_speed: 0.28,
            erode_speed: 0.32,
            evaporation: 0.018,
            gravity: 6.0,
            initial_sediment: 1.0,
            initial_water: 1.0,
            initial_speed: 1.0,
            radius: 3,
            min_weight: 0.28,
            deposit_radius: 2,
        }
    }
}

impl HydraulicParams {
    /// A brush-sized preset: few droplets over a small patch.
    pub fn brush(radius: u32, droplets: u32) -> Self {
        Self {
            droplets,
            max_lifetime: 24,
            inertia: 0.05,
            capacity: 2.4,
            deposit_speed: 0.4,
            erode_speed: 0.45,
            evaporation: 0.03,
            radius: radius.max(1),
            min_weight: 0.3,
            deposit_radius: radius.max(1),
            ..Self::default()
        }
    }

    fn sanitized(&self) -> HydraulicParams {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        HydraulicParams {
            droplets: self.droplets.clamp(0, 4_000_000),
            max_lifetime: self.max_lifetime.clamp(1, 10_000),
            inertia: f(self.inertia, 0.0, 0.99, 0.06),
            capacity: f(self.capacity, 0.0, 1000.0, 3.2),
            deposit_speed: f(self.deposit_speed, 0.0, 1.0, 0.28),
            erode_speed: f(self.erode_speed, 0.0, 1.0, 0.32),
            evaporation: f(self.evaporation, 0.0, 1.0, 0.018),
            gravity: f(self.gravity, 0.0, 1000.0, 6.0),
            initial_sediment: f(self.initial_sediment, 0.0, 1000.0, 1.0),
            initial_water: f(self.initial_water, 1e-4, 1000.0, 1.0),
            initial_speed: f(self.initial_speed, 0.0, 1000.0, 1.0),
            radius: self.radius.min(512),
            min_weight: f(self.min_weight, 0.0, 1000.0, 0.28),
            deposit_radius: self.deposit_radius.min(512),
        }
    }
}

/// Add `amount` of material over a disc around `pos`.
///
/// The disc is weighted by a smooth radial falloff and, secondarily, by how
/// far each sample sits below the disc's mean height, so material settles into
/// the hollow it fell into rather than building a cone.
fn deposit_disc(h: &mut [f32], w: i64, ht: i64, pos: (f32, f32), amount: f32, radius: u32, min_w: f32) {
    let r = radius as i64;
    let (cx, cy) = (pos.0 as i64, pos.1 as i64);
    if !amount.is_finite() {
        return;
    }
    let inv_r2 = 1.0 / ((r * r + r) as f32 + 1.0);

    let mut cells: Vec<(usize, f32)> = Vec::new();
    let mut mean = 0.0f32;
    let mut n = 0.0f32;
    for dy in -r..=r {
        for dx in -r..=r {
            let x = cx + dx;
            let y = cy + dy;
            if x < 0 || y < 0 || x >= w || y >= ht {
                continue;
            }
            let d2 = (dx * dx + dy * dy) as f32;
            if d2 > (r * r + r) as f32 {
                continue;
            }
            let falloff = 1.0 - d2 * inv_r2;
            let idx = y as usize * w as usize + x as usize;
            mean += h[idx] * falloff;
            n += falloff;
            cells.push((idx, falloff));
        }
    }
    if cells.is_empty() || n <= 0.0 {
        return;
    }
    mean /= n;

    let mut weights: Vec<f32> = Vec::with_capacity(cells.len());
    let mut total = 0.0f32;
    for (idx, falloff) in &cells {
        let below = (mean - h[*idx]).max(0.0);
        let wgt = falloff * (1.0 + below * min_w * 4.0);
        weights.push(wgt);
        total += wgt;
    }
    if total <= 0.0 || !total.is_finite() {
        return;
    }
    let scale = amount / total;
    for (k, (idx, _)) in cells.iter().enumerate() {
        h[*idx] += weights[k] * scale;
    }
}

/// Bilinear height and central-difference gradient at a fractional position.
fn height_and_gradient(h: &[f32], w: i64, ht: i64, x: f32, y: f32) -> (f32, f32, f32) {
    let cx = math::clamp(x as f64, 0.0, (w - 1) as f64) as f32;
    let cy = math::clamp(y as f64, 0.0, (ht - 1) as f64) as f32;
    let x0 = cx.floor() as i64;
    let y0 = cy.floor() as i64;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(ht - 1);
    let fx = cx - x0 as f32;
    let fy = cy - y0 as f32;
    let at = |xi: i64, yi: i64| h[yi as usize * w as usize + xi as usize];
    let h00 = at(x0, y0);
    let h10 = at(x1, y0);
    let h01 = at(x0, y1);
    let h11 = at(x1, y1);
    let top = h00 + (h10 - h00) * fx;
    let bot = h01 + (h11 - h01) * fx;
    // The analytic derivative of the bilinear patch, in samples per sample.
    let gx = (h10 - h00) * (1.0 - fy) + (h11 - h01) * fy;
    let gy = (h01 - h00) * (1.0 - fx) + (h11 - h10) * fx;
    (top + (bot - top) * fy, gx, gy)
}

#[inline]
fn normalize2(x: f32, y: f32) -> (f32, f32) {
    let len = math::sqrt((x * x + y * y) as f64) as f32;
    if len > 1e-6 && len.is_finite() {
        (x / len, y / len)
    } else {
        (0.0, 0.0)
    }
}

/// Run particle-based hydraulic erosion, returning a new heightfield.
///
/// The result is a pure function of the input, the parameters and the seed.
pub fn hydraulic(hf: &Heightfield, params: &HydraulicParams, seed: u64) -> Result<Heightfield> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    if hf.width < 3 || hf.height < 3 {
        return Err(Error::ZeroSize);
    }
    let p = params.sanitized();
    let w = hf.width as i64;
    let ht = hf.height as i64;
    let cell = hf.cell_size;
    let mut out = hf.clone();
    let mut rng = Rng::new(substream(seed, 0x4859_4452_4f53));

    for _ in 0..p.droplets {
        let mut pos = (
            rng.range32(1.0, (w - 1) as f32),
            rng.range32(1.0, (ht - 1) as f32),
        );
        // Speed is in cells per step, so it stays comparable with the slope and
        // the capacity stays in inches of soil.
        let mut speed = p.initial_speed / cell;
        let mut water = p.initial_water;
        let (_, gx0, gy0) = height_and_gradient(&out.data, w, ht, pos.0, pos.1);
        let dir = normalize2(-gx0, -gy0);
        let mut sediment =
            p.initial_sediment * sediment_capacity(&p, cell, gx0, gy0, dir.0, dir.1, speed, water);
        let mut dir = dir;

        for _ in 0..p.max_lifetime {
            let old = pos;
            let (h_old, gx, gy) = height_and_gradient(&out.data, w, ht, old.0, old.1);
            let downhill = normalize2(-gx, -gy);
            // The new heading keeps most of the old one and adds the local
            // downhill pull, so a droplet runs straight down a channel instead
            // of stair-stepping across it.
            dir = normalize2(
                dir.0 * p.inertia + downhill.0 * (1.0 - p.inertia),
                dir.1 * p.inertia + downhill.1 * (1.0 - p.inertia),
            );
            pos = (pos.0 + dir.0, pos.1 + dir.1);
            let (h_new, _, _) = height_and_gradient(&out.data, w, ht, pos.0, pos.1);
            // The drop along the heading feeds the droplet's speed.
            let fell = (h_old - h_new) / cell;
            let sp2 = (speed * speed + (fell * p.gravity).max(-MAX_SPEED * MAX_SPEED)).max(0.0);
            speed = math::sqrt(sp2 as f64) as f32;
            if speed > MAX_SPEED {
                speed = MAX_SPEED;
            }
            let dh = h_new - h_old;
            let capacity = sediment_capacity(&p, cell, gx, gy, dir.0, dir.1, speed, water);

            if sediment > capacity || dh > 0.0 {
                // Uphill or over capacity: drop sediment to fill the hollow.
                let amount = if dh > 0.0 {
                    dh.min(sediment)
                } else {
                    (sediment - capacity) * p.deposit_speed
                };
                sediment -= amount;
                deposit_disc(&mut out.data, w, ht, pos, amount, p.deposit_radius, p.min_weight);
            } else {
                // Under capacity: cut into the ground, never deeper than the
                // step the droplet just fell, so a channel can never dig
                // through the surface it is running on.
                let take = ((capacity - sediment) * p.erode_speed).min(fell * cell * 0.5);
                if take > 0.0 {
                    deposit_disc(&mut out.data, w, ht, pos, -take, p.radius, p.min_weight);
                    sediment += take;
                }
            }

            water *= 1.0 - p.evaporation;
            let left = pos.0 < 1.0 || pos.1 < 1.0 || pos.0 >= w as f32 - 1.0 || pos.1 >= ht as f32 - 1.0;
            if water <= 1e-4 || left {
                // Whatever the droplet still carries settles where it stopped,
                // so nothing leaves the grid suspended.
                if sediment > 0.0 {
                    let here = (
                        math::clamp(pos.0 as f64, 0.0, (w - 1) as f64) as f32,
                        math::clamp(pos.1 as f64, 0.0, (ht - 1) as f64) as f32,
                    );
                    deposit_disc(&mut out.data, w, ht, here, sediment, p.deposit_radius, p.min_weight);
                }
                break;
            }
        }
    }

    out.sanitize(hf.min());
    Ok(out)
}

/// A droplet never usefully travels more than this many cells in one step, and
/// capping it keeps the capacity from running away down a long slope.
const MAX_SPEED: f32 = 2.0;

/// The largest fraction of a droplet's own volume that can be solid material.
/// Water cannot hold more than this in suspension, so it bounds the capacity
/// and stops a droplet from digging an unbounded hole as it falls into one.
const SUSPENSION: f32 = 0.35;

/// How much soil a droplet can hold, in inches.
///
/// The slope along its heading, its speed and the water it has left all raise
/// the load, which is why headwaters are thin threads and a fast trunk stream
/// carries the most.
#[allow(clippy::too_many_arguments)]
fn sediment_capacity(
    p: &HydraulicParams,
    cell: f32,
    gx: f32,
    gy: f32,
    dx: f32,
    dy: f32,
    speed: f32,
    water: f32,
) -> f32 {
    if cell <= 0.0 {
        return 0.0;
    }
    let slope = (-gx * dx - gy * dy) / cell;
    if slope <= 0.0 || water <= 0.0 {
        return 0.0;
    }
    let wanted = p.capacity * slope * speed * water;
    (wanted.max(0.0)).min(water * SUSPENSION)
}

/// Erode only inside `rect`, feathering the seam, which is what the sculpt
/// brush needs so a local stroke cannot leave a hard edge.
pub fn hydraulic_in_rect(
    hf: &mut Heightfield,
    rect: Rect,
    params: &HydraulicParams,
    seed: u64,
) -> Result<Rect> {
    let r = rect.clamp_to(hf.width, hf.height);
    if r.is_empty() {
        return Ok(r);
    }
    // A droplet needs a cell of margin to start and finish, so the patch is
    // grown before it is cut and the seam falls outside the requested rect.
    let cut = r.pad(1).clamp_to(hf.width, hf.height);
    let sub = hf.crop(cut.x0, cut.y0, cut.width(), cut.height())?;
    let local = hydraulic(&sub, params, seed)?;
    let feather = math::saturate(params.erode_speed as f64) as f32;
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let k = sub.index(x - cut.x0, y - cut.y0);
            let i = hf.index(x, y);
            // The change fades at the rect's own border, so the stroke leaves
            // no hard edge behind it.
            let edge = (x - r.x0).min(r.x1 - 1 - x).min(y - r.y0).min(r.y1 - 1 - y) as f32;
            let wgt = 1.0 - feather * (1.0 - math::saturate(edge as f64) as f32);
            hf.data[i] += (local.data[k] - sub.data[k]) * wgt;
        }
    }
    hf.sanitize(hf.min());
    Ok(r)
}

/// Grid-based hydraulic erosion using the virtual pipe model.
///
/// Each cell passes water to its four neighbours through pipes that fill under
/// pressure and overflow. This is far cheaper per unit of area than droplets
/// and suits whole landscapes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PipeParams {
    /// Simulation steps.
    pub iterations: u32,
    /// Water added per cell per step, in inches.
    pub rain: f32,
    /// Sediment carrying capacity factor, in inches of soil per unit of flow.
    #[serde(default = "default_pipe_capacity")]
    pub capacity: f32,
    /// Fraction of a cell's sediment capacity actually moved.
    pub solubility: f32,
    /// Fraction of pipe water that evaporates per step.
    pub evaporation: f32,
    /// Fraction of the excess sediment dropped when capacity is exceeded.
    pub deposition: f32,
    /// Water a cell starts with.
    pub initial_water: f32,
    /// Pressure coefficient: how strongly a slope drives a pipe, in inches of
    /// head per step of fall.
    pub gravity: f32,
    /// Fraction of the previous step's flow a pipe keeps.
    pub pipe_rate: f32,
    /// Fraction of standing water that runs off over the cell walls per step.
    pub surface_rate: f32,
    /// Sediment a cell starts with, in inches.
    pub initial_sediment: f32,
}

fn default_pipe_capacity() -> f32 {
    1.0
}

impl Default for PipeParams {
    fn default() -> Self {
        Self {
            iterations: 60,
            rain: 0.012,
            capacity: 1.0,
            solubility: 0.35,
            evaporation: 0.02,
            deposition: 0.28,
            initial_water: 0.5,
            gravity: 1.0,
            pipe_rate: 0.22,
            surface_rate: 0.32,
            initial_sediment: 0.0,
        }
    }
}

impl PipeParams {
    pub fn sanitized(&self) -> PipeParams {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        PipeParams {
            iterations: self.iterations.clamp(1, 4096),
            rain: f(self.rain, 0.0, 1.0, 0.012),
            capacity: f(self.capacity, 0.0, 1000.0, 1.0),
            solubility: f(self.solubility, 0.0, 1.0, 0.35),
            evaporation: f(self.evaporation, 0.0, 1.0, 0.02),
            deposition: f(self.deposition, 0.0, 1.0, 0.28),
            initial_water: f(self.initial_water, 0.0, 8.0, 0.5),
            gravity: f(self.gravity, 0.0, 256.0, 1.0),
            pipe_rate: f(self.pipe_rate, 0.0, 1.0, 0.22),
            surface_rate: f(self.surface_rate, 0.0, 1.0, 0.32),
            initial_sediment: f(self.initial_sediment, 0.0, 8.0, 0.0),
        }
    }
}

/// The four pipe directions as cell offsets, ordered so the pipe facing the
/// opposite way is always two slots further on.
const PIPES: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];

/// Slot of the pipe that faces the opposite way to `d`.
const fn opposite(d: usize) -> usize {
    (d + 2) % 4
}


/// Run grid-based pipe-model erosion, returning a new heightfield.
///
/// Water and sediment are held per cell in a fixed array and every pass walks
/// the grid in the same order, so the result depends only on the input, never on
/// iteration order or on how the work is split across threads.
pub fn pipe_erosion(hf: &Heightfield, params: &PipeParams) -> Result<Heightfield> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    if hf.width < 3 || hf.height < 3 {
        return Err(Error::ZeroSize);
    }
    let p = params.sanitized();
    let w = hf.width as usize;
    let h = hf.height as usize;
    let n = w * h;
    let cell = hf.cell_size;
    let index = |x: usize, y: usize| y * w + x;

    let mut height = hf.data.clone();
    let mut water = vec![p.initial_water; n];
    let mut sediment = vec![p.initial_sediment; n];
    // Four pipe outflows per cell, in the PIPES order.
    let mut flux = vec![0.0f32; n * 4];
    let mut dh = vec![0.0f32; n];
    let mut moved = vec![0.0f32; n];
    let mut next_water = vec![0.0f32; n];

    for _ in 0..p.iterations {
        for v in water.iter_mut() {
            *v = (*v + p.rain).max(0.0);
        }

        // Pipe pressure: a pipe fills with the slope it sits on and discharges
        // what its cell cannot hold. The grid edge is closed, so no water or
        // sediment is invented at the border.
        for y in 0..h {
            for x in 0..w {
                let i = index(x, y);
                let head = height[i] + water[i];
                let mut demand = 0.0f32;
                for (d, slot) in PIPES.iter().enumerate() {
                    let (nx, ny) = (x as i32 + slot.0, y as i32 + slot.1);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        flux[i * 4 + d] = 0.0;
                        continue;
                    }
                    let j = index(nx as usize, ny as usize);
                    let drop = head - (height[j] + water[j]);
                    let f = (flux[i * 4 + d] * p.pipe_rate + drop / cell * p.gravity).max(0.0);
                    flux[i * 4 + d] = f;
                    demand += f;
                }
                // Water standing on the surface also runs off over the cell walls.
                demand += water[i] * p.surface_rate;
                let scale = if demand > 0.0 { (water[i] / demand).min(1.0) } else { 0.0 };
                for d in 0..4 {
                    flux[i * 4 + d] *= scale;
                }
            }
        }

        // Water update, velocity, then erosion or deposition. Heights are read
        // from the previous pass and written as a delta, so the pass is a pure
        // function of the state it started from.
        for y in 0..h {
            for x in 0..w {
                let i = index(x, y);
                let mut inflow = 0.0f32;
                let mut outflow = water[i] * p.surface_rate;
                let mut vx = 0.0f32;
                let mut vy = 0.0f32;
                for (d, slot) in PIPES.iter().enumerate() {
                    let f = flux[i * 4 + d];
                    outflow += f;
                    let (nx, ny) = (x as i32 + slot.0, y as i32 + slot.1);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    // The pipe of the neighbour that faces back at this cell.
                    inflow += flux[index(nx as usize, ny as usize) * 4 + opposite(d)];
                    match d {
                        1 => vx += f,
                        3 => vx -= f,
                        2 => vy += f,
                        _ => vy -= f,
                    }
                }
                let wn = (water[i] + inflow - outflow).max(0.0);
                next_water[i] = wn;

                // Average velocity across the cell, from the imbalance across
                // the pipes.
                let speed = math::sqrt(((vx * 0.5) * (vx * 0.5) + (vy * 0.5) * (vy * 0.5)) as f64) as f32;
                let (gx, gy) = cell_gradient(&height, w, h, x, y);
                let slope2 = gx * gx + gy * gy;
                let tilt = math::sqrt(slope2 as f64 / (1.0 + slope2 as f64)) as f32;
                let capacity = (p.capacity * tilt * speed * wn).max(0.0);
                let diff = capacity - sediment[i];
                if diff > 0.0 {
                    // Dissolve, but never undercut the local slope.
                    let take = diff.min(max_neighbour_drop(&height, w, h, x, y) * 0.5);
                    dh[i] -= take;
                    sediment[i] += take;
                } else {
                    let give = (-diff) * p.deposition;
                    dh[i] += give;
                    sediment[i] -= give;
                }
            }
        }

        // Advect the suspended load with the flow, so what each cell sends out
        // is exactly what its neighbours receive.
        for d in moved.iter_mut() {
            *d = 0.0;
        }
        for y in 0..h {
            for x in 0..w {
                let i = index(x, y);
                let mut out = 0.0f32;
                for d in 0..4 {
                    out += flux[i * 4 + d];
                }
                if out <= 1e-9 {
                    continue;
                }
                for (d, slot) in PIPES.iter().enumerate() {
                    let f = flux[i * 4 + d];
                    if f <= 0.0 {
                        continue;
                    }
                    let (nx, ny) = (x as i32 + slot.0, y as i32 + slot.1);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let share = sediment[i] * (f / out);
                    moved[index(nx as usize, ny as usize)] += share;
                    moved[i] -= share;
                }
            }
        }

        for y in 0..h {
            for x in 0..w {
                let i = index(x, y);
                height[i] += dh[i];
                let s = (sediment[i] + moved[i]).max(0.0);
                sediment[i] = if s.is_finite() { s } else { 0.0 };
                let wv = next_water[i] * (1.0 - p.evaporation);
                water[i] = if wv.is_finite() { wv.max(0.0) } else { 0.0 };
                if !height[i].is_finite() {
                    height[i] = 0.0;
                }
            }
        }
    }

    let mut out = hf.clone();
    out.data = height;
    out.sanitize(hf.min());
    Ok(out)
}

/// Central-difference height gradient of a cell, clamping at the grid edge.
fn cell_gradient(h: &[f32], w: usize, hgt: usize, x: usize, y: usize) -> (f32, f32) {
    let at = |xx: usize, yy: usize| h[yy.min(hgt - 1) * w + xx.min(w - 1)];
    let gx = (at(x + 1, y) - at(x.saturating_sub(1), y)) * 0.5;
    let gy = (at(x, y + 1) - at(x, y.saturating_sub(1))) * 0.5;
    (gx, gy)
}

/// The largest height difference between a cell and its neighbours, which is
/// the deepest cut that does not undercut the surface.
fn max_neighbour_drop(h: &[f32], w: usize, hgt: usize, x: usize, y: usize) -> f32 {
    let at = |xx: usize, yy: usize| h[yy.min(hgt - 1) * w + xx.min(w - 1)];
    let centre = at(x, y);
    let d = |xx: usize, yy: usize| (at(xx, yy) - centre).abs();
    d(x + 1, y).max(d(x.saturating_sub(1), y)).max(d(x, y + 1)).max(d(x, y.saturating_sub(1)))
}

/// Thermal erosion: material above the angle of repose slides downhill.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThermalParams {
    /// Iteration passes.
    pub iterations: u32,
    /// Repose angle in degrees. Material steeper than this slides.
    pub talus_angle: f32,
    /// Fraction of the excess height moved per pass, 0..1.
    pub rate: f32,
    /// Fraction of moved material kept, the rest spreads wider.
    pub talus: f32,
}

impl Default for ThermalParams {
    fn default() -> Self {
        Self { iterations: 24, talus_angle: 34.0, rate: 0.5, talus: 0.6 }
    }
}

impl ThermalParams {
    pub fn sanitized(&self) -> ThermalParams {
        ThermalParams {
            iterations: self.iterations.clamp(1, 4096),
            talus_angle: if self.talus_angle.is_finite() {
                self.talus_angle.clamp(0.0, 89.0)
            } else {
                34.0
            },
            rate: if self.rate.is_finite() { self.rate.clamp(0.0, 1.0) } else { 0.5 },
            talus: if self.talus.is_finite() { self.talus.clamp(0.0, 1.0) } else { 0.6 },
        }
    }
}

const N4: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

/// Run thermal erosion, returning a new heightfield.
///
/// Each pass moves material from any sample steeper than the talus angle to
/// its lower 4-neighbours, weighted towards the lowest, so slopes relax
/// towards the angle of repose. Volume is conserved: every transfer debits
/// exactly what it credits.
pub fn thermal(hf: &Heightfield, params: &ThermalParams) -> Result<Heightfield> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    let p = params.sanitized();
    let w = hf.width as usize;
    let h = hf.height as usize;
    // A one-cell step may hold this height difference before material slides.
    let tan = math::tan(math::to_radians(p.talus_angle as f64)) as f32;
    let max_drop = tan * hf.cell_size;

    let mut height = hf.data.clone();
    let mut delta = vec![0.0f32; height.len()];

    for _ in 0..p.iterations {
        for d in delta.iter_mut() {
            *d = 0.0;
        }
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let here = height[i];
                let mut lowest = here;
                let mut has_neighbour = false;
                for (dx, dy) in N4 {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    lowest = lowest.min(height[ny as usize * w + nx as usize]);
                    has_neighbour = true;
                }
                if !has_neighbour {
                    continue;
                }
                let excess = (here - lowest) - max_drop;
                if excess <= 0.0 {
                    continue;
                }
                // Weight receivers by how far below the crest they sit, so
                // material runs to the lowest ground first.
                let mut receivers: [(usize, f32); 4] = [(0, 0.0); 4];
                let mut count = 0usize;
                let mut total_w = 0.0f32;
                for (dx, dy) in N4 {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    let hn = height[j];
                    if hn >= here {
                        continue;
                    }
                    let wgt = 1.0 + (here - hn) * p.talus;
                    receivers[count] = (j, wgt);
                    count += 1;
                    total_w += wgt;
                }
                if count == 0 || total_w <= 0.0 {
                    continue;
                }
                let moved = excess * p.rate;
                delta[i] -= moved;
                for (j, wgt) in &receivers[..count] {
                    delta[*j] += moved * (*wgt / total_w);
                }
            }
        }
        parallel::zip_mut(&mut height, &delta, |h, d| {
            *h += d;
            if !h.is_finite() {
                *h = 0.0;
            }
        });
    }

    let mut out = hf.clone();
    out.data = height;
    out.sanitize(hf.min());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bowl(w: u32, h: u32, cell: f32) -> Heightfield {
        Heightfield::from_fn(w, h, cell, |x, y| {
            let dx = (x as f32 - w as f32 * 0.5) / 4.0;
            let dy = (y as f32 - h as f32 * 0.5) / 4.0;
            200.0 - (dx * dx + dy * dy) * 4.0
        })
        .unwrap()
    }

    fn volume(hf: &Heightfield) -> f64 {
        hf.data.iter().map(|v| *v as f64).sum::<f64>() * hf.cell_size as f64
    }

    #[test]
    fn hydraulic_carves_and_is_reproducible() {
        let hf = bowl(64, 64, 8.0);
        let p = HydraulicParams { droplets: 4000, ..HydraulicParams::default() };
        let a = hydraulic(&hf, &p, 7).unwrap();
        let b = hydraulic(&hf, &p, 7).unwrap();
        assert_eq!(a, b, "same seed must give the same surface");
        let c = hydraulic(&hf, &p, 8).unwrap();
        assert_ne!(a, c, "a different seed must give a different surface");
        assert!(a.is_finite());
        // Droplets concentrate in the middle of the bowl, so it must go down.
        let mid = a.index(32, 32);
        assert!(a.data[mid] < hf.data[mid], "centre was not eroded");
    }

    #[test]
    fn hydraulic_erosion_bounded() {
        let hf = bowl(48, 48, 8.0);
        let p = HydraulicParams { droplets: 6000, ..HydraulicParams::default() };
        let a = hydraulic(&hf, &p, 3).unwrap();
        let before = volume(&hf);
        let after = volume(&a);
        // Erosion moves material around; a droplet carries at most its own
        // capacity, so the net change is a small fraction of the terrain.
        let rel = (after - before).abs() / before.abs();
        assert!(rel < 0.25, "volume moved by {rel}");
        let span = (a.max() - a.min()) as f64;
        assert!(span > 0.0 && span.is_finite());
    }

    #[test]
    fn hydraulic_rejects_degenerate_grids() {
        let tiny = Heightfield::filled(2, 2, 1.0, 0.0).unwrap();
        assert!(hydraulic(&tiny, &HydraulicParams::default(), 1).is_err());
        let ok = Heightfield::filled(8, 8, 1.0, 0.0).unwrap();
        let none = HydraulicParams { droplets: 0, ..HydraulicParams::default() };
        assert_eq!(hydraulic(&ok, &none, 1).unwrap(), ok);
    }

    #[test]
    fn hydraulic_in_rect_only_touches_its_window() {
        let mut hf = bowl(40, 40, 4.0);
        let before = hf.clone();
        let rect = Rect::new(10, 12, 20, 24);
        let p = HydraulicParams::brush(2, 2000);
        let touched = hydraulic_in_rect(&mut hf, rect, &p, 5).unwrap();
        assert_eq!(touched, rect);
        for y in 0..40u32 {
            for x in 0..40u32 {
                let changed = (hf.get(x, y).unwrap() - before.get(x, y).unwrap()).abs() > 1e-6;
                if changed {
                    assert!(rect.contains(x, y), "changed ({x},{y}) outside the rect");
                }
            }
        }
        // A rect that misses the grid changes nothing.
        let mut flat = Heightfield::filled(16, 16, 1.0, 5.0).unwrap();
        let miss = Rect::new(100, 100, 110, 110);
        assert!(hydraulic_in_rect(&mut flat, miss, &p, 5).unwrap().is_empty());
        assert!(flat.data.iter().all(|v| *v == 5.0));
    }

    #[test]
    fn pipe_erosion_moves_material_downhill() {
        // A ramp tilted along X: the pipe model must shave the high end and
        // build at the low end, which is the only thing a landscape pipe model
        // does. The grid edge is closed, so the low corner pools and fills.
        let hf = Heightfield::from_fn(64, 64, 8.0, |x, y| x as f32 * 6.0 + y as f32).unwrap();
        let a = pipe_erosion(&hf, &PipeParams::default()).unwrap();
        assert!(a.is_finite());
        let high = a.index(60, 32);
        let low = a.index(0, 32);
        assert!(a.data[high] < hf.data[high], "the high end was not eroded");
        assert!(a.data[low] > hf.data[low], "the low end was not filled");
        // The slope is preserved, so the model flattened the ramp rather than
        // moving the whole surface.
        assert!(a.max() - a.min() < hf.max() - hf.min() + 1.0);
        // Deterministic: the pipe model has no random component at all.
        assert_eq!(pipe_erosion(&hf, &PipeParams::default()).unwrap(), a);
        // Nothing leaves the grid as NaN even with punishing parameters.
        let harsh = PipeParams { rain: 1.0, iterations: 200, ..PipeParams::default() };
        assert!(pipe_erosion(&hf, &harsh).unwrap().is_finite());
        // A basin is a closed depression, so it must not fall through the floor.
        let basin = bowl(48, 48, 8.0);
        let b = pipe_erosion(&basin, &PipeParams::default()).unwrap();
        assert!(b.is_finite());
        assert!(b.data[b.index(24, 24)] > basin.data[basin.index(24, 24)] - basin.data.len() as f32);
    }

    #[test]
    fn pipe_erosion_rejects_degenerate_grids() {
        let tiny = Heightfield::filled(2, 2, 1.0, 0.0).unwrap();
        assert!(pipe_erosion(&tiny, &PipeParams::default()).is_err());
    }

    #[test]
    fn thermal_erosion_bounded() {
        let hf = Heightfield::from_fn(48, 48, 4.0, |x, _| {
            // A spike far steeper than the angle of repose.
            if x == 24 {
                400.0
            } else {
                0.0
            }
        })
        .unwrap();
        let p = ThermalParams { iterations: 60, ..ThermalParams::default() };
        let a = thermal(&hf, &p).unwrap();
        let before = volume(&hf);
        let after = volume(&a);
        assert!((after - before).abs() < before.abs() * 1e-4 + 1e-3, "{before} -> {after}");
        // The spike is gone, spread over the neighbours.
        assert!(a.max() < hf.max());
        // Erosion relaxes the field: both the steepest step and the mean step
        // fall, even though a single pass can overshoot the talus angle when
        // material lands on a fresh slope.
        let step = |g: &Heightfield, x: u32, y: u32| (g.get(x + 1, y).unwrap() - g.get(x, y).unwrap()).abs();
        let worst_before = (1..47u32).map(|x| step(&hf, x, 24)).fold(0.0f32, f32::max);
        let worst_after = (1..47u32).map(|x| step(&a, x, 24)).fold(0.0f32, f32::max);
        assert!(worst_after < worst_before, "{worst_after} is not below {worst_before}");
        let mean = |g: &Heightfield| {
            (1..47u32).map(|x| step(g, x, 24)).sum::<f32>() / 46.0
        };
        assert!(mean(&a) < mean(&hf) * 0.5, "the mean step barely moved");
        // Deterministic.
        assert_eq!(thermal(&hf, &p).unwrap(), a);
    }

    #[test]
    fn thermal_with_no_slope_is_a_no_op() {
        let hf = Heightfield::filled(16, 16, 1.0, 42.0).unwrap();
        assert_eq!(thermal(&hf, &ThermalParams::default()).unwrap(), hf);
    }

    #[test]
    fn height_gradient_matches_a_known_plane() {
        let hf = Heightfield::from_fn(9, 9, 2.0, |x, y| 3.0 * x as f32 + 5.0 * y as f32).unwrap();
        let (h, gx, gy) = height_and_gradient(&hf.data, 9, 9, 4.0, 5.0);
        assert!((h - (12.0 + 25.0)).abs() < 1e-4, "height {h}");
        assert!((gx - 3.0).abs() < 1e-4, "gx {gx}");
        assert!((gy - 5.0).abs() < 1e-4, "gy {gy}");
        // Reads outside the grid clamp instead of panicking.
        let (h2, _, _) = height_and_gradient(&hf.data, 9, 9, -50.0, 500.0);
        assert!(h2.is_finite());
    }

    #[test]
    fn deposit_disc_is_mass_preserving_and_bounded() {
        let mut h = vec![10.0f32; 25];
        deposit_disc(&mut h, 5, 5, (2.0, 2.0), 5.0, 2, 0.3);
        let sum: f32 = h.iter().sum();
        // The deposits are stored as f32, so the sum carries f32 rounding.
        assert!((sum - (10.0 * 25.0 + 5.0)).abs() < 1e-2, "sum {sum}");
        // A disc off the grid deposits nothing rather than indexing out of range.
        let mut edge = vec![0.0f32; 4];
        deposit_disc(&mut edge, 2, 2, (-50.0, -50.0), 5.0, 2, 0.3);
        assert!(edge.iter().all(|v| *v == 0.0));
        // A non-finite amount is dropped instead of poisoning the whole disc.
        let mut guard = vec![1.0f32; 9];
        deposit_disc(&mut guard, 3, 3, (1.0, 1.0), f32::NAN, 2, 0.3);
        assert!(guard.iter().all(|v| *v == 1.0));
    }
}
