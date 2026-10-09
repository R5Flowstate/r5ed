//! One generator per terrain type. Every generator takes a parameter struct
//! with sane defaults, declares the height range it will produce, and is a
//! pure function of its parameters, the seed and the grid size.
//!
//! All the shape maths runs in `f64` and only the final sample is narrowed to
//! `f32`, so the same parameters give bit-identical output on every target.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::filters;
use crate::heightfield::Heightfield;
use crate::heightmap::HeightmapSource;
use crate::math;
use crate::noise::{self, Fractal, FractalParams, Noise};
use crate::rng::{Rng, substream};

/// Largest sample count a generator will produce, so a hostile recipe cannot
/// ask for a grid no machine can hold.
pub const MAX_SAMPLES: usize = 1 << 28;

/// How a dune field is laid out relative to the wind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DunePattern {
    /// Long crests perpendicular to the wind, the common transverse dune.
    #[default]
    Transverse,
    /// Isolated crescents with two horns pointing downwind.
    Barchan,
    /// Radiating arms around a deflation centre, as on a star dune.
    Star,
}

/// Which noise family drives a generator's base shape.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeParams {
    /// Octave count, frequency, gain and the rest.
    pub fractal: FractalParams,
    /// Which fractal combination to sum.
    pub kind: Fractal,
    /// Cross-term bias in -1..1, where positive favours X and negative Y.
    pub anisotropy: f32,
    /// Coordinate rotation in degrees, applied before sampling.
    pub rotation_deg: f32,
    /// Displacement of the sample point by the noise field, in cells.
    pub warp: f32,
    /// Seed for the warp field, taken from the generator seed when zero.
    pub warp_seed: u64,
}

impl Default for ShapeParams {
    fn default() -> Self {
        Self {
            fractal: FractalParams { octaves: 6, frequency: 1.6, ..FractalParams::default() },
            kind: Fractal::Fbm,
            anisotropy: 0.0,
            rotation_deg: 0.0,
            warp: 0.0,
            warp_seed: 0,
        }
    }
}

impl ShapeParams {
    /// Sample the shape at a cell coordinate, returning roughly -1..1.
    fn sample(&self, n: &Noise, warp_noise: &Noise, x: f64, y: f64) -> f64 {
        let rad = math::to_radians(self.rotation_deg as f64);
        let (mut px, mut py) = (x, y);
        if self.warp > 0.0 {
            let s = self.warp as f64;
            let (wx, wy) = noise::domain_warp2_iter(warp_noise, &self.fractal, s, 2, x, y);
            px = wx;
            py = wy;
        }
        let (rx, ry) = (px * math::cos(rad) - py * math::sin(rad), px * math::sin(rad) + py * math::cos(rad));
        let a = self.anisotropy as f64;
        let (sx, sy) = (rx * (1.0 + a), ry * (1.0 - a));
        noise::fractal2(self.kind, n, &self.fractal, sx, sy)
    }
}

/// Rolling hills: soft, rounded, low relief.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RollingHillsParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    pub shape: ShapeParams,
}

impl Default for RollingHillsParams {
    fn default() -> Self {
        Self {
            base: 64.0,
            amplitude: 320.0,
            seed: 1,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 5, frequency: 1.1, gain: 0.45, ..FractalParams::default() },
                kind: Fractal::Fbm,
                ..ShapeParams::default()
            },
        }
    }
}

/// Mountains: ridged crests blended with fBm shoulders.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MountainsParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Fraction of the crest that comes from the ridged fractal.
    pub ridged_mix: f32,
    /// Height of the broad massif the crests sit on, relative to amplitude.
    pub massif: f32,
    pub shape: ShapeParams,
}

impl Default for MountainsParams {
    fn default() -> Self {
        Self {
            base: 0.0,
            amplitude: 2400.0,
            seed: 2,
            ridged_mix: 0.7,
            massif: 0.45,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 7, frequency: 1.4, gain: 0.5, ..FractalParams::default() },
                kind: Fractal::Ridged,
                ..ShapeParams::default()
            },
        }
    }
}

/// Alpine terrain: mountains run through thermal and hydraulic erosion so the
/// slopes settle near their angle of repose and the valleys collect sediment.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AlpineParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    pub ridged_mix: f32,
    pub massif: f32,
    /// Thermal erosion passes and the repose angle they relax towards.
    pub thermal_iterations: u32,
    pub talus_angle: f32,
    /// Droplets of hydraulic erosion run after the thermal passes.
    pub erosion_droplets: u32,
    pub shape: ShapeParams,
}

impl Default for AlpineParams {
    fn default() -> Self {
        Self {
            base: 0.0,
            amplitude: 3000.0,
            seed: 3,
            ridged_mix: 0.75,
            massif: 0.5,
            thermal_iterations: 12,
            talus_angle: 38.0,
            erosion_droplets: 20_000,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 7, frequency: 1.5, gain: 0.5, ..FractalParams::default() },
                kind: Fractal::Ridged,
                ..ShapeParams::default()
            },
        }
    }
}

/// Plateau or mesa country: a flat top with a hard cliff at its edge.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlateauParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Benches per height unit of relief.
    pub steps: f32,
    /// 0 is a smooth step, 1 is a square cliff.
    pub sharpness: f32,
    /// Regional tilt, in inches per cell, so mesas are not all level.
    pub tilt: f32,
    pub shape: ShapeParams,
}

impl Default for PlateauParams {
    fn default() -> Self {
        Self {
            base: 200.0,
            amplitude: 900.0,
            seed: 4,
            steps: 5.0,
            sharpness: 0.85,
            tilt: 0.6,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 5, frequency: 1.2, gain: 0.5, ..FractalParams::default() },
                kind: Fractal::Fbm,
                ..ShapeParams::default()
            },
        }
    }
}

/// Canyon country: a plateau cut by a meandering river and stepped by erosion.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanyonParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Number of river channels carved into the plateau.
    pub channels: u32,
    /// Half-width of a channel, in cells.
    pub channel_width: f32,
    /// Depth of a channel relative to the plateau top.
    pub channel_depth: f32,
    /// How much the channel meanders, in cells.
    pub meander: f32,
    /// Benches cut into the channel walls.
    pub steps: f32,
    pub shape: ShapeParams,
}

impl Default for CanyonParams {
    fn default() -> Self {
        Self {
            base: 400.0,
            amplitude: 1200.0,
            seed: 5,
            channels: 2,
            channel_width: 6.0,
            channel_depth: 0.55,
            meander: 3.0,
            steps: 7.0,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 5, frequency: 1.3, gain: 0.5, ..FractalParams::default() },
                kind: Fractal::Fbm,
                ..ShapeParams::default()
            },
        }
    }
}

/// Badlands: deeply dissected slopes with dense branching gullies.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BadlandsParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Sharpness of the drainage network, higher is more incised.
    pub drainage: f32,
    /// Benches on the dissected slopes.
    pub steps: f32,
    pub shape: ShapeParams,
}

impl Default for BadlandsParams {
    fn default() -> Self {
        Self {
            base: 100.0,
            amplitude: 1000.0,
            seed: 6,
            drainage: 3.0,
            steps: 9.0,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 7, frequency: 1.7, gain: 0.55, ..FractalParams::default() },
                kind: Fractal::Ridged,
                ..ShapeParams::default()
            },
        }
    }
}

/// Desert dunes: directional wind ridges in one of three patterns.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DunesParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    pub pattern: DunePattern,
    /// Wind bearing in degrees; crests run across it.
    pub wind_deg: f32,
    /// Wavelength between crests, in cells.
    pub wavelength: f32,
    /// Crest sharpness, 0 is a sine, 1 is a knife edge.
    pub crest_sharpness: f32,
    pub shape: ShapeParams,
}

impl Default for DunesParams {
    fn default() -> Self {
        Self {
            base: 0.0,
            amplitude: 260.0,
            seed: 7,
            pattern: DunePattern::Transverse,
            wind_deg: 35.0,
            wavelength: 24.0,
            crest_sharpness: 0.6,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 4, frequency: 1.0, gain: 0.5, ..FractalParams::default() },
                kind: Fractal::Fbm,
                warp: 0.35,
                ..ShapeParams::default()
            },
        }
    }
}

/// Islands and archipelagos: radial landmasses with a beach shelf.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct IslandsParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Number of island centres; one is a single island.
    pub centres: u32,
    /// Radius of each island's landmass, in cells.
    pub radius: f32,
    /// Width of the shallow shelf, in cells.
    pub shelf: f32,
    /// Sea level relative to base.
    pub water_level: f32,
    pub shape: ShapeParams,
}

impl Default for IslandsParams {
    fn default() -> Self {
        Self {
            base: 0.0,
            amplitude: 700.0,
            seed: 8,
            centres: 3,
            radius: 34.0,
            shelf: 18.0,
            water_level: 0.18,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 6, frequency: 2.2, gain: 0.5, ..FractalParams::default() },
                kind: Fractal::Fbm,
                warp: 0.5,
                ..ShapeParams::default()
            },
        }
    }
}

/// Volcanic terrain: a cone with a caldera and radiating lava channels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolcanicParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Radius of the cone's foot, in cells.
    pub radius: f32,
    /// Radius of the caldera, in cells.
    pub caldera: f32,
    /// Depth of the caldera below the rim, as a fraction of amplitude.
    pub caldera_depth: f32,
    /// Number of lava channels running down the flanks.
    pub flows: u32,
    /// Roughness of the cone's flanks, 0 is a clean cone.
    pub flank_noise: f32,
}

impl Default for VolcanicParams {
    fn default() -> Self {
        Self {
            base: 0.0,
            amplitude: 1800.0,
            seed: 9,
            radius: 40.0,
            caldera: 12.0,
            caldera_depth: 0.35,
            flows: 5,
            flank_noise: 0.25,
        }
    }
}

/// Craters: an impact field with a power-law size distribution.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CratersParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    pub count: u32,
    /// Largest crater radius, in cells.
    pub max_radius: f32,
    /// Ratio between successive crater sizes; small means many small craters.
    pub size_decay: f32,
    /// Floor depth relative to radius.
    pub depth: f32,
    /// Rim height relative to radius.
    pub rim: f32,
    /// Relief of the ground between craters.
    pub background: f32,
}

impl Default for CratersParams {
    fn default() -> Self {
        Self {
            base: 100.0,
            amplitude: 0.0,
            seed: 10,
            count: 40,
            max_radius: 22.0,
            size_decay: 0.55,
            depth: 0.35,
            rim: 0.22,
            background: 120.0,
        }
    }
}

/// Glacial valley: a U-shaped trough carved along a spline.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GlacialParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Ridge crest either side of the trough.
    pub ridge_height: f32,
    /// Half-width of the trough floor, in cells.
    pub floor_width: f32,
    /// Width of the trough walls above the floor, in cells.
    pub wall_width: f32,
    /// Passes of thermal erosion that widen the trough into a U.
    pub smoothing: u32,
    pub shape: ShapeParams,
}

impl Default for GlacialParams {
    fn default() -> Self {
        Self {
            base: 200.0,
            amplitude: 900.0,
            seed: 11,
            ridge_height: 0.6,
            floor_width: 5.0,
            wall_width: 12.0,
            smoothing: 4,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 6, frequency: 1.4, gain: 0.5, ..FractalParams::default() },
                kind: Fractal::Ridged,
                ..ShapeParams::default()
            },
        }
    }
}

/// River network and lakes carved by following the terrain's own flow.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RiversParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Depth of a channel relative to the local flow accumulation.
    pub incision: f32,
    /// Half-width of a channel, in cells.
    pub width: f32,
    /// Fill basins whose catchment exceeds this fraction of the largest.
    pub lakes: f32,
    /// Extra smoothing of the carved network.
    pub smoothing: u32,
    pub shape: ShapeParams,
}

impl Default for RiversParams {
    fn default() -> Self {
        Self {
            base: 300.0,
            amplitude: 1000.0,
            seed: 12,
            incision: 0.5,
            width: 3.0,
            lakes: 0.12,
            smoothing: 2,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 6, frequency: 1.5, gain: 0.5, ..FractalParams::default() },
                kind: Fractal::Fbm,
                warp: 0.4,
                ..ShapeParams::default()
            },
        }
    }
}

/// Coastline: open sea on one side, land rising to cliffs on the other.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoastlineParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Wind bearing of the shoreline in degrees; land is downwind of it.
    pub bearing_deg: f32,
    /// How much the shoreline meanders, in cells.
    pub meander: f32,
    /// Sea level relative to base.
    pub water_level: f32,
    /// Steepness of the cliff band at the shoreline.
    pub cliff: f32,
    pub shape: ShapeParams,
}

impl Default for CoastlineParams {
    fn default() -> Self {
        Self {
            base: 0.0,
            amplitude: 700.0,
            seed: 13,
            bearing_deg: 20.0,
            meander: 5.0,
            water_level: 0.22,
            cliff: 0.8,
            shape: ShapeParams {
                fractal: FractalParams { octaves: 6, frequency: 1.6, gain: 0.5, ..FractalParams::default() },
                kind: Fractal::Fbm,
                ..ShapeParams::default()
            },
        }
    }
}

/// Terraced farmland: gentle steps with a bund on each riser.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FarmlandParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Height of one terrace, in inches.
    pub step: f32,
    /// Flatness of each terrace, 0 is a smooth ramp and 1 is dead level.
    pub flatness: f32,
    /// Height of the bund lip on the uphill edge of a terrace.
    pub bund: f32,
}

impl Default for FarmlandParams {
    fn default() -> Self {
        Self {
            base: 100.0,
            amplitude: 400.0,
            seed: 14,
            step: 24.0,
            flatness: 0.9,
            bund: 4.0,
        }
    }
}

/// A flat arena with a raised berm around the rim so players cannot fall out.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArenaParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    /// Fraction of the half-extent at which the berm starts to rise.
    pub rim_start: f32,
    /// Height of the berm above the floor.
    pub berm: f32,
    /// How abruptly the berm rises, 0 is a long ramp and 1 is a wall.
    pub berm_sharpness: f32,
    /// Relief inside the arena, relative to amplitude.
    pub floor_relief: f32,
}

impl Default for ArenaParams {
    fn default() -> Self {
        Self {
            base: 0.0,
            amplitude: 500.0,
            seed: 15,
            rim_start: 0.78,
            berm: 260.0,
            berm_sharpness: 0.7,
            floor_relief: 0.08,
        }
    }
}

/// A field of holes, as if the ground dropped out in blobs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PitsParams {
    pub base: f32,
    pub amplitude: f32,
    pub seed: u64,
    pub count: u32,
    pub max_radius: f32,
    pub depth: f32,
    pub lip: f32,
    /// Cell size the pit radii are expressed in, kept for authoring clarity.
    pub radius_in_cells: bool,
}

impl Default for PitsParams {
    fn default() -> Self {
        Self {
            base: 100.0,
            amplitude: 500.0,
            seed: 16,
            count: 24,
            max_radius: 18.0,
            depth: 0.7,
            lip: 0.1,
            radius_in_cells: true,
        }
    }
}

/// Every generator the recipe graph can build a node from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Generator {
    RollingHills(RollingHillsParams),
    Mountains(MountainsParams),
    Alpine(AlpineParams),
    Plateau(PlateauParams),
    Canyon(CanyonParams),
    Badlands(BadlandsParams),
    Dunes(DunesParams),
    Islands(IslandsParams),
    Volcanic(VolcanicParams),
    Craters(CratersParams),
    Glacial(GlacialParams),
    Rivers(RiversParams),
    Coastline(CoastlineParams),
    Farmland(FarmlandParams),
    Arena(ArenaParams),
    Pits(PitsParams),
    Heightmap(HeightmapSource),
}

impl Generator {
    /// The seed the generator's noise fields are built from.
    pub fn seed(&self) -> u64 {
        match self {
            Generator::RollingHills(p) => p.seed,
            Generator::Mountains(p) => p.seed,
            Generator::Alpine(p) => p.seed,
            Generator::Plateau(p) => p.seed,
            Generator::Canyon(p) => p.seed,
            Generator::Badlands(p) => p.seed,
            Generator::Dunes(p) => p.seed,
            Generator::Islands(p) => p.seed,
            Generator::Volcanic(p) => p.seed,
            Generator::Craters(p) => p.seed,
            Generator::Glacial(p) => p.seed,
            Generator::Rivers(p) => p.seed,
            Generator::Coastline(p) => p.seed,
            Generator::Farmland(p) => p.seed,
            Generator::Arena(p) => p.seed,
            Generator::Pits(p) => p.seed,
            Generator::Heightmap(s) => s.seed,
        }
    }

    /// Lowest and highest sample this generator can emit, before any filter is
    /// applied downstream.
    pub fn height_range(&self) -> (f32, f32) {
        match self {
            Generator::RollingHills(p) => (p.base - p.amplitude.abs(), p.base + p.amplitude.abs()),
            Generator::Mountains(p) => (p.base, p.base + p.amplitude.abs() * (1.0 + p.massif.abs())),
            Generator::Alpine(p) => (p.base, p.base + p.amplitude.abs() * (1.0 + p.massif.abs())),
            Generator::Plateau(p) => (p.base - p.amplitude.abs(), p.base + p.amplitude.abs()),
            Generator::Canyon(p) => (p.base - p.amplitude.abs() * (1.0 + p.channel_depth.abs()), p.base + p.amplitude.abs()),
            Generator::Badlands(p) => (p.base - p.amplitude.abs(), p.base + p.amplitude.abs()),
            Generator::Dunes(p) => {
                // The profile runs 0..1 and the secondary relief adds a quarter
                // of the amplitude either side, and the star pattern stacks a
                // radial system on top of the two crossing crest systems.
                let lo = p.base - p.amplitude.abs() * 0.25;
                let hi = p.base + p.amplitude.abs() * 1.9;
                (lo, hi)
            }

            Generator::Islands(p) => (p.base, p.base + p.amplitude.abs()),
            Generator::Volcanic(p) => (p.base, p.base + p.amplitude.abs()),
            Generator::Craters(p) => {
                (p.base - p.background.abs() - p.amplitude.abs(), p.base + p.background.abs() + p.amplitude.abs())
            }
            Generator::Glacial(p) => (
                p.base - p.amplitude.abs() * p.ridge_height.abs(),
                p.base + p.amplitude.abs() * (1.0 + p.ridge_height.abs()),
            ),

            Generator::Rivers(p) => (
                p.base - p.amplitude.abs() * p.incision.abs(),
                p.base + p.amplitude.abs(),
            ),
            Generator::Coastline(p) => (p.base, p.base + p.amplitude.abs()),
            Generator::Farmland(p) => (
                p.base - p.amplitude.abs() - p.bund.abs(),
                p.base + p.amplitude.abs() + p.bund.abs(),
            ),
            Generator::Arena(p) => (
                p.base - p.amplitude.abs() * p.floor_relief.abs(),
                p.base + p.amplitude.abs() * p.floor_relief.abs() + p.berm.abs(),
            ),
            Generator::Pits(p) => (p.base - p.depth.abs() * p.amplitude.abs(), p.base + p.lip.abs() * p.amplitude.abs()),
            Generator::Heightmap(s) => (s.min_height(), s.max_height()),
        }
    }

    /// Build the heightfield on a `width` by `height` grid.
    pub fn generate(&self, width: u32, height: u32, cell_size: f32) -> Result<Heightfield> {
        let n = width as usize * height as usize;
        if n == 0 {
            return Err(Error::ZeroSize);
        }
        if n > MAX_SAMPLES {
            return Err(Error::HeightmapTooLarge(width, height));
        }
        let out = match self {
            Generator::RollingHills(p) => rolling_hills(p, width, height, cell_size)?,
            Generator::Mountains(p) => mountains(p, width, height, cell_size)?,
            Generator::Alpine(p) => alpine(p, width, height, cell_size)?,
            Generator::Plateau(p) => plateau(p, width, height, cell_size)?,
            Generator::Canyon(p) => canyon(p, width, height, cell_size)?,
            Generator::Badlands(p) => badlands(p, width, height, cell_size)?,
            Generator::Dunes(p) => dunes(p, width, height, cell_size)?,
            Generator::Islands(p) => islands(p, width, height, cell_size)?,
            Generator::Volcanic(p) => volcanic(p, width, height, cell_size)?,
            Generator::Craters(p) => craters(p, width, height, cell_size)?,
            Generator::Glacial(p) => glacial(p, width, height, cell_size)?,
            Generator::Rivers(p) => rivers(p, width, height, cell_size)?,
            Generator::Coastline(p) => coastline(p, width, height, cell_size)?,
            Generator::Farmland(p) => farmland(p, width, height, cell_size)?,
            Generator::Arena(p) => arena(p, width, height, cell_size)?,
            Generator::Pits(p) => pits(p, width, height, cell_size)?,
            Generator::Heightmap(s) => s.to_heightfield(cell_size)?,
        };
        if out.width != width || out.height != height {
            return Err(Error::SizeMismatch(width, height, out.data.len()));
        }
        let mut out = out;
        if !out.is_finite() {
            out.sanitize(self.height_range().0);
        }
        Ok(out)
    }
}

fn noise_for(seed: u64, stream: u64) -> Noise {
    Noise::new(substream(seed, stream))
}

fn warp_noise(shape: &ShapeParams, seed: u64) -> Noise {
    if shape.warp_seed == 0 {
        noise_for(seed, 0x5741_5250)
    } else {
        Noise::new(shape.warp_seed)
    }
}

fn check(cell_size: f32, width: u32, height: u32) -> Result<()> {
    if width < 2 || height < 2 {
        return Err(Error::ZeroSize);
    }
    if !cell_size.is_finite() || cell_size <= 0.0 {
        return Err(Error::InvalidCellSize(cell_size));
    }
    Ok(())
}

/// Smooth 0..1 ramp across a band, used for every falloff in this module.
#[inline]
fn band(x: f64, edge0: f64, edge1: f64) -> f64 {
    math::smooth_range(x, edge0, edge1)
}

/// Rotate a cell coordinate into a frame whose X axis points along `deg`.
#[inline]
fn along(deg: f32, x: f64, y: f64) -> (f64, f64) {
    let r = math::to_radians(deg as f64);
    (x * math::cos(r) + y * math::sin(r), -x * math::sin(r) + y * math::cos(r))
}

fn rolling_hills(p: &RollingHillsParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let amp = p.amplitude as f64;
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    for y in 0..h {
        for x in 0..w {
            let v = p.shape.sample(&n, &wn, x as f64, y as f64);
            let i = out.index(x, y);
            out.data[i] = (p.base as f64 + amp * v) as f32;
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn mountain_core(
    ridged_mix: f32,
    massif: f32,
    shape: &ShapeParams,
    n: &Noise,
    wn: &Noise,
    base: f64,
    amp: f64,
    w: u32,
    h: u32,
) -> Vec<f64> {
    let mut out = vec![0.0f64; (w as usize) * (h as usize)];
    let mix = math::saturate(ridged_mix as f64);
    let diag = math::sqrt(w as f64 * w as f64 + h as f64 * h as f64);
    let mut ridged = *shape;
    ridged.kind = Fractal::Ridged;
    let mut smooth_shape = *shape;
    smooth_shape.kind = Fractal::Fbm;
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f64, y as f64);
            let r = ridged.sample(n, wn, px, py);
            let f = smooth_shape.sample(n, wn, px, py);
            // The massif is a broad, always-positive dome so the range has
            // somewhere to sit; the crest field rides on top of it.
            let d = math::sqrt(px * px + py * py) / diag;
            let dome = 1.0 - band(d, 0.15, 1.05);
            let crest = math::saturate(mix * r + (1.0 - mix) * (f * 0.5 + 0.5));
            out[y as usize * w as usize + x as usize] = base + amp * (crest + massif as f64 * dome);
        }
    }
    out
}

fn mountains(p: &MountainsParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let data = mountain_core(
        p.ridged_mix,
        p.massif,
        &p.shape,
        &n,
        &wn,
        p.base as f64,
        p.amplitude as f64,
        w,
        h,
    );
    Heightfield::new(w, h, cell, data.into_iter().map(|v| v as f32).collect())
}

fn alpine(p: &AlpineParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let data = mountain_core(
        p.ridged_mix,
        p.massif,
        &p.shape,
        &n,
        &wn,
        p.base as f64,
        p.amplitude as f64,
        w,
        h,
    );
    let mut hf = Heightfield::new(w, h, cell, data.into_iter().map(|v| v as f32).collect())?;
    let thermal = crate::erosion::ThermalParams {
        iterations: p.thermal_iterations,
        talus_angle: p.talus_angle,
        rate: 0.5,
        talus: 0.6,
    };
    hf = crate::erosion::thermal(&hf, &thermal)?;
    let hydraulic = crate::erosion::HydraulicParams {
        droplets: p.erosion_droplets,
        ..crate::erosion::HydraulicParams::default()
    };
    crate::erosion::hydraulic(&hf, &hydraulic, substream(p.seed, 0x414c_5049))
}

fn plateau(p: &PlateauParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    for y in 0..h {
        for x in 0..w {
            let v = p.shape.sample(&n, &wn, x as f64, y as f64);
            let i = out.index(x, y);
            // Tilt the whole block so the benches do not all sit level.
            let tilt = p.tilt as f64 * (x as f64 - w as f64 * 0.5) * 0.25;
            out.data[i] = (p.base as f64 + p.amplitude as f64 * v + tilt) as f32;
        }
    }
    terrace_field(&mut out, p.steps as f64, p.sharpness as f64);
    Ok(out)
}

/// Quantise into flat benches separated by a riser of the given sharpness.
fn terrace_field(hf: &mut Heightfield, steps: f64, sharpness: f64) {
    if steps <= 0.0 || !steps.is_finite() {
        return;
    }
    let s = math::clamp(sharpness, 0.0, 1.0);
    // A bench never rises above the highest point it was cut from, so the
    // original range is the clamp the result must respect.
    let (lo, hi) = hf.range();
    for v in hf.data.iter_mut() {
        let t = *v as f64 / steps;
        let base = t.floor();
        let frac = t - base;
        // A hard terrace holds its level then jumps; a soft one is a smoothstep.
        let shaped = if s >= 0.999 {
            if frac < 1.0 - s_threshold() {
                0.0
            } else {
                1.0
            }
        } else {
            let k = s * s * (3.0 - 2.0 * s);
            math::smootherstep(frac * (1.0 + k * 8.0))
        };
        *v = math::clamp((base + shaped) * steps, lo as f64, hi as f64) as f32;
    }
}

fn s_threshold() -> f64 {
    1.0 - 1e-3
}

/// Distance from a point to a polyline, in cells, used by every channel carve.
fn polyline_distance(pts: &[(f64, f64)], x: f64, y: f64) -> f64 {
    if pts.len() == 1 {
        let dx = x - pts[0].0;
        let dy = y - pts[0].1;
        return math::sqrt(dx * dx + dy * dy);
    }
    let mut best = f64::INFINITY;
    for seg in 0..pts.len().saturating_sub(1) {
        let (ax, ay) = pts[seg];
        let (bx, by) = pts[seg + 1];
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 { math::saturate(((x - ax) * dx + (y - ay) * dy) / len2) } else { 0.0 };
        let (cx, cy) = (ax + dx * t, ay + dy * t);
        let d = math::sqrt((x - cx) * (x - cx) + (y - cy) * (y - cy));
        best = best.min(d);
    }
    if best.is_finite() {
        best
    } else {
        f64::INFINITY
    }
}

/// A meandering channel path that crosses the grid from one edge to the other.
fn channel_path(seed: u64, stream: u64, w: u32, h: u32, meander: f64) -> Vec<(f64, f64)> {
    let n = noise_for(seed, stream);
    let mut pts = Vec::with_capacity(33);
    let (span_x, span_y) = (w as f64, h as f64);
    for k in 0..=32 {
        let t = k as f64 / 32.0;
        let y = t * span_y;
        let wobble = n.perlin2(t * 3.0, 11.5) * meander;
        let x = t * span_x + wobble - meander * 0.5;
        pts.push((x, y));
    }
    pts
}

fn canyon(p: &CanyonParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    for y in 0..h {
        for x in 0..w {
            let v = p.shape.sample(&n, &wn, x as f64, y as f64);
            let i = out.index(x, y);
            out.data[i] = (p.base as f64 + p.amplitude as f64 * v) as f32;
        }
    }
    let channels = p.channels.clamp(1, 16);
    let width = p.width_or_default();
    for c in 0..channels {
        let path = channel_path(p.seed, 0x4348_4e00 + c as u64, w, h, p.meander as f64);
        let floor_level = p.base as f64 - p.amplitude as f64 * p.channel_depth.max(0.0) as f64;
        carve_channel(&mut out, &path, width, floor_level);
    }
    if p.steps > 0.0 {
        terrace_field(&mut out, p.amplitude as f64 / f64::from(p.steps.max(1.0)), 0.35);
    }
    Ok(out)
}

impl CanyonParams {
    fn width_or_default(&self) -> f64 {
        if self.channel_width > 0.0 {
            self.channel_width as f64
        } else {
            4.0
        }
    }
}

/// Cut a flat-bottomed channel of half-width `half` at `floor` along `path`.
fn carve_channel(hf: &mut Heightfield, path: &[(f64, f64)], half: f64, floor: f64) {
    if half <= 0.0 {
        return;
    }
    for y in 0..hf.height {
        for x in 0..hf.width {
            let d = polyline_distance(path, x as f64, y as f64);
            if !d.is_finite() || d > half * 2.5 {
                continue;
            }
            // Flat floor inside `half`, then a wall that climbs to the rim.
            let t = math::saturate((d - half) / (half * 1.5));
            let floor_h = floor as f32;
            let wall = (1.0 - math::smoothstep(t)) as f32;
            let i = hf.index(x, y);
            hf.data[i] = floor_h + (hf.data[i] - floor_h) * wall;
        }
    }
}

fn badlands(p: &BadlandsParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    let d = math::max(p.drainage as f64, 0.01);
    for y in 0..h {
        for x in 0..w {
            let v = p.shape.sample(&n, &wn, x as f64, y as f64);
            // Raising a ridged field to a high power incises the valleys and
            // leaves the interfluves broad and flat, which is badlands.
            let t = math::saturate(v);
            let cut = math::pow(t, d);
            let i = out.index(x, y);
            out.data[i] = (p.base as f64 + p.amplitude as f64 * (cut - 0.5)) as f32;
        }
    }
    if p.steps > 0.0 {
        terrace_field(&mut out, p.amplitude as f64 / f64::from(p.steps.max(1.0)), 0.3);
    }
    Ok(out)
}

fn dunes(p: &DunesParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let lambda = if p.wavelength > 0.0 { p.wavelength as f64 } else { 16.0 };
    let sharp = math::saturate(p.crest_sharpness as f64);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    let cx = w as f64 * 0.5;
    let cy = h as f64 * 0.5;
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f64, y as f64);
            // `along` puts the downwind axis on X, so crests run across it.
            let (ax, ay) = along(p.wind_deg, px, py);
            let i = out.index(x, y);
            let relief = p.shape.sample(&n, &wn, px * 0.5, py * 0.5) as f64;
            let v = match p.pattern {
                DunePattern::Transverse => {
                    let phase = math::fract(ax / lambda);
                    dune_profile(phase, sharp)
                }
                DunePattern::Barchan => {
                    // Horns point downwind, so the amplitude grows upwind.
                    let phase = math::fract(ax / (lambda * 2.0));
                    let d = math::sqrt((px - cx) * (px - cx) + (py - cy) * (py - cy));
                    let spread = math::bump(d, lambda * 2.5);
                    dune_profile(phase, sharp) * spread
                }
                DunePattern::Star => {
                    // Two crossing crest systems radiate from a deflation centre.
                    let d = math::sqrt((px - cx) * (px - cx) + (py - cy) * (py - cy));
                    let radial = math::sin(math::atan2(py - cy, px - cx) * 7.0 + d / lambda * math::TAU);
                    let radial = 0.5 + 0.5 * radial;
                    dune_profile(math::fract(ax / lambda), sharp) * 0.5
                        + dune_profile(math::fract(ay / lambda), sharp) * 0.5
                        + radial * math::bump(d, lambda * 3.0) * 0.6
                }
            };
            out.data[i] = (p.base as f64 + p.amplitude as f64 * (v + 0.25 * relief)) as f32;
        }
    }
    Ok(out)
}

/// One dune cross-section in 0..1, with a sharp crest and a soft slip face.
fn dune_profile(phase: f64, sharpness: f64) -> f64 {
    let s = math::sin(phase * math::TAU);
    let shaped = if sharpness <= 0.0 {
        0.5 + 0.5 * s
    } else {
        // Pull the crest up and the trough down as sharpness rises.
        let t = math::saturate(0.5 + 0.5 * s);
        let g = math::pow(t, 1.0 + sharpness * 5.0) / math::pow(0.5, sharpness * 5.0);
        math::saturate(g)
    };
    math::saturate(shaped)
}

fn islands(p: &IslandsParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    // Centres sit on a jittered grid, so an archipelago spreads out instead of
    // clumping and no centre falls off the edge of the map.
    let centres = p.centres.clamp(1, 64) as usize;
    let grid = (centres as f64).sqrt().ceil().max(1.0) as usize;
    let mut rng = Rng::new(substream(p.seed, 0x4345_4e54));
    let radius = if p.radius > 0.0 { p.radius as f64 } else { 24.0 };
    let mut spots: Vec<(f64, f64, f64)> = Vec::with_capacity(centres);
    for k in 0..centres {
        let (gx, gy) = (k % grid, k / grid);
        spots.push((
            (gx as f64 + rng.range(0.25, 0.75)) * w as f64 / grid as f64,
            (gy as f64 + rng.range(0.25, 0.75)) * h as f64 / grid as f64,
            radius * rng.range(0.75, 1.25),
        ));
    }
    let shelf = math::max(p.shelf as f64, 0.0);
    // The shelf is a fraction of the radius, so it stays proportional when the
    // radius changes. Inside it the ground is the shallow beach, and the land
    // climbs from the shoreline inwards to the middle of the island.
    let shelf_frac = math::saturate(shelf / radius);
    let sea = math::saturate(p.water_level as f64);
    // An island never runs off the edge of the map, whatever radius it was
    // given, so the land fades back to sea level over the outer cells.
    let edge = math::max(math::min(w as f64, h as f64) * 0.12, 2.0);
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f64, y as f64);
            // Distance to the nearest centre as a fraction of that centre's
            // radius, so islands of different sizes blend into one coastline.
            let mut nearest = f64::INFINITY;
            for (sx, sy, sr) in &spots {
                let d = math::sqrt((px - sx) * (px - sx) + (py - sy) * (py - sy)) / math::max(*sr, 1.0);
                nearest = nearest.min(d);
            }
            let detail = p.shape.sample(&n, &wn, px, py) * 0.28;
            let r = nearest + detail;
            // `d` is zero in the middle of an island and one at its shoreline,
            // so the beach is the inner ring and the land rises inwards.
            let beach = 1.0 - math::smoothstep(math::saturate(r / math::max(1.0 - shelf_frac, 1e-3)));
            let land = 1.0 - math::smoothstep(math::saturate((r - (1.0 - shelf_frac)) / 0.5));
            let fx = math::smoothstep(px / edge) * math::smoothstep((w as f64 - 1.0 - px) / edge);
            let fy = math::smoothstep(py / edge) * math::smoothstep((h as f64 - 1.0 - py) / edge);
            let inside = fx * fy;
            let i = out.index(x, y);
            out.data[i] = (p.base as f64
                + p.amplitude as f64 * (sea + (1.0 - sea) * (beach * 0.25 + land * 0.75) * inside))
                as f32;
        }
    }
    Ok(out)
}

fn volcanic(p: &VolcanicParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    let cx = w as f64 * 0.5;
    let cy = h as f64 * 0.5;
    let radius = math::max(p.radius as f64, 1.0);
    let caldera = math::max(p.caldera as f64, 0.0);
    let amp = p.amplitude as f64;
    let rough = math::saturate(p.flank_noise as f64);
    let flows = p.flows.clamp(0, 32);
    let mut rng = Rng::new(substream(p.seed, 0x464c_4f57));
    let mut flow_dirs: Vec<f64> = Vec::with_capacity(flows as usize);
    for _ in 0..flows {
        flow_dirs.push(rng.range(0.0, math::TAU));
    }
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f64 - cx, y as f64 - cy);
            let d = math::sqrt(px * px + py * py);
            // A cone: height falls linearly with radius, so the flank angle is
            // constant all the way round and the caldera cuts a clean bowl.
            let cone = 1.0 - math::saturate(d / radius);
            let mut height = amp * cone;
            // The caldera is a bowl subtracted from the summit.
            if caldera > 0.0 {
                let bowl = 1.0 - math::smoothstep(math::saturate(d / caldera));
                height -= amp * math::saturate(p.caldera_depth as f64) * bowl;
            }
            if rough > 0.0 {
                height += amp * 0.12 * rough * n.perlin2(px * 0.08, py * 0.08) * cone;
            }
            // Lava channels: narrow notches from just outside the caldera to
            // the foot of the cone, so they follow the steepest line.
            for dir in &flow_dirs {
                let along = px * math::cos(*dir) + py * math::sin(*dir);
                let across = -px * math::sin(*dir) + py * math::cos(*dir);
                if along < caldera * 0.5 || along > radius {
                    continue;
                }
                let width = 1.0 + along * 0.06;
                let notch = 1.0 - math::smoothstep(math::saturate((across.abs() - width) / width));
                let reach = 1.0 - math::saturate((along - radius * 0.5) / (radius * 0.5));
                height -= amp * 0.16 * notch * reach;
            }
            let i = out.index(x, y);
            out.data[i] = (p.base as f64 + height) as f32;
        }
    }
    Ok(out)
}

fn craters(p: &CratersParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    Ok(crater_field(p, w, h, cell))
}

fn pits(p: &PitsParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let proxy = CratersParams {
        base: p.base,
        amplitude: p.amplitude,
        seed: p.seed,
        count: p.count,
        max_radius: p.max_radius,
        size_decay: 0.5,
        depth: p.depth,
        rim: p.lip,
        background: 0.0,
    };
    Ok(crater_field(&proxy, w, h, cell))
}

/// A background surface with a field of round hollows stamped on it. A crater
/// and a pit are the same profile with different depth and lip ratios, so both
/// come from this one builder.
fn crater_field(p: &CratersParams, w: u32, h: u32, cell: f32) -> Heightfield {
    let mut hf = Heightfield::filled(w, h, cell, p.base).expect("checked above");
    let n = Noise::new(p.seed);
    let mut rng = Rng::new(substream(p.seed, 0x494d_5041_4354));
    for y in 0..h {
        for x in 0..w {
            let i = hf.index(x, y);
            let v = n.perlin2(x as f64 * 0.05, y as f64 * 0.05) as f32;
            hf.data[i] = p.base + p.background * v;
        }
    }
    let count = p.count.clamp(0, 4096);
    let decay = math::clamp(p.size_decay as f64, 0.05, 1.0);
    for _ in 0..count {
        let cx = rng.range(0.0, w as f64);
        let cy = rng.range(0.0, h as f64);
        // A power law on the size gives many small craters and few big ones.
        let u = rng.unit();
        let radius = p.max_radius as f64 * math::pow(decay, u * 6.0);
        if radius < 0.5 {
            continue;
        }
        let extent = (radius * 1.6).ceil() as i64;
        let depth = p.depth as f64 * radius;
        let rim = p.rim as f64 * radius;
        for dy in -extent..=extent {
            for dx in -extent..=extent {
                let x = cx as i64 + dx;
                let y = cy as i64 + dy;
                if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                    continue;
                }
                let d = math::sqrt((dx * dx + dy * dy) as f64);
                let t = d / radius;
                // Bowl inside the rim, an ejecta lip just outside it, then flat.
                let bowl = 1.0 - math::smootherstep(math::saturate(t / 0.85));
                let lip = band(t, 0.85, 1.25) * (1.0 - math::saturate((t - 1.0) / 0.6));
                let i = hf.index(x as u32, y as u32);
                hf.data[i] += (rim * lip - depth * bowl) as f32;
            }
        }
    }
    hf
}

fn glacial(p: &GlacialParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    let path = channel_path(p.seed, 0x474c_4143, w, h, 2.5);
    let floor = p.floor_width.max(0.5) as f64;
    let wall = math::max(p.wall_width as f64, 0.5);
    for y in 0..h {
        for x in 0..w {
            let v = p.shape.sample(&n, &wn, x as f64, y as f64);
            let i = out.index(x, y);
            let rough = (p.base as f64 + p.amplitude as f64 * v) as f32;
            let d = polyline_distance(&path, x as f64, y as f64);
            // A U profile: flat floor, then a wall that eases into the ridge.
            let profile = if d <= floor {
                0.0
            } else {
                let t = math::saturate((d - floor) / wall);
                math::smootherstep(t)
            };
            let ridge = p.ridge_height as f64 * p.amplitude as f64;
            out.data[i] = (rough as f64 - profile * ridge) as f32;
        }
    }
    if p.smoothing > 0 {
        crate::erosion::thermal(
            &out,
            &crate::erosion::ThermalParams {
                iterations: p.smoothing,
                talus_angle: 40.0,
                rate: 0.5,
                talus: 0.7,
            },
        )?;
    }
    Ok(out)
}

fn rivers(p: &RiversParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    for y in 0..h {
        for x in 0..w {
            let v = p.shape.sample(&n, &wn, x as f64, y as f64);
            let i = out.index(x, y);
            out.data[i] = (p.base as f64 + p.amplitude as f64 * v) as f32;
        }
    }
    let flow = out.flow_accumulation();
    let max_area = flow.max_area() as f64;
    let width = math::max(p.width as f64, 0.5);
    let incision = p.incision as f64;
    let lake_threshold = (math::saturate(p.lakes as f64) * max_area) as f32;
    for i in 0..out.data.len() {
        // Channel depth follows the logarithm of the catchment, so headwaters
        // are shallow threads and trunk rivers are deep.
        let a = flow.area[i] as f64 / max_area.max(1.0);
        let t = math::saturate(ln1p(a) / ln1p(1.0));
        out.data[i] -= (incision * p.amplitude as f64 * t) as f32;
    }
    // A basin whose catchment is large enough fills to its spill height, which
    // is the level its lowest outlet sits at.
    if lake_threshold > 0.0 {
        for i in 0..out.data.len() {
            if flow.area[i] >= lake_threshold && flow.filled[i] > out.data[i] {
                out.data[i] = flow.filled[i];
            }
        }
    }
    // A light blur widens the threads into valleys without moving the trunk.
    for _ in 0..p.smoothing {
        filters::smooth(&mut out, width.ceil() as u32, 1);
    }
    Ok(out)
}

/// `ln(1 + x)`, which keeps flow accumulation well behaved near zero.
fn ln1p(x: f64) -> f64 {
    if x <= 0.0 {
        0.0
    } else {
        math::ln(1.0 + x)
    }
}

fn coastline(p: &CoastlineParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let wn = warp_noise(&p.shape, p.seed);
    let half_diag = (w as f64 + h as f64) * 0.25;
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    let sea = math::saturate(p.water_level as f64);
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f64 - w as f64 * 0.5, y as f64 - h as f64 * 0.5);
            // Signed distance from the shoreline, negative out to sea.
            let shore = n.perlin2(px * 0.05, py * 0.05) * p.meander as f64;
            let d = shore - along(p.bearing_deg, px, py).0;
            let cliff = math::clamp(p.cliff as f64, 0.0, 1.0);
            // A soft band on the sea side, a sharp rise on the land side.
            let land = math::smoothstep(math::saturate(d / (half_diag * 0.5 + 1.0)));
            let stepped = math::saturate(1.0 - (1.0 - land) * (1.0 + cliff * 3.0));
            let rough = p.shape.sample(&n, &wn, px, py);
            let v = sea + (1.0 - sea) * stepped + 0.15 * rough * stepped;
            let i = out.index(x, y);
            out.data[i] = (p.base as f64 + p.amplitude as f64 * math::saturate(v)) as f32;
        }
    }
    Ok(out)
}

fn farmland(p: &FarmlandParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    for y in 0..h {
        for x in 0..w {
            let v = n.perlin2(x as f64 * 0.02, y as f64 * 0.02);
            let i = out.index(x, y);
            out.data[i] = (p.base as f64 + p.amplitude as f64 * v) as f32;
        }
    }
    let step = if p.step > 0.0 { p.step as f64 } else { 24.0 };
    let flat = math::saturate(p.flatness as f64);
    for v in out.data.iter_mut() {
        let t = *v as f64 / step;
        let base = t.floor();
        let frac = t - base;
        // Flatness pulls each field towards the level of its own terrace.
        let eased = math::lerp(frac, 0.5, flat);
        *v = ((base + eased) * step) as f32;
    }
    // A bund lip on the uphill edge keeps water on the terrace.
    let bund = p.bund as f64;
    if bund != 0.0 {
        let src = out.clone();
        for y in 0..h {
            for x in 0..w {
                let uphill = src.get_clamped(x as i64, y as i64 - 1);
                let here = src.get_clamped(x as i64, y as i64);
                let drop = here - uphill;
                let i = out.index(x, y);
                if drop < 0.0 {
                    out.data[i] += (bund * math::saturate(-drop as f64 / step)) as f32;
                }
            }
        }
    }
    Ok(out)
}

fn arena(p: &ArenaParams, w: u32, h: u32, cell: f32) -> Result<Heightfield> {
    check(cell, w, h)?;
    let n = Noise::new(p.seed);
    let mut out = Heightfield::filled(w, h, cell, p.base)?;
    let start = math::saturate(p.rim_start as f64);
    let sharp = math::saturate(p.berm_sharpness as f64);
    let relief = math::saturate(p.floor_relief as f64);
    let berm = p.berm as f64;
    for y in 0..h {
        for x in 0..w {
            // Chebyshev distance in cells, so the arena is a square floor.
            let dx = (x as f64 / (w as f64 - 1.0) - 0.5).abs() * 2.0;
            let dy = (y as f64 / (h as f64 - 1.0) - 0.5).abs() * 2.0;
            let t = math::saturate((dx.max(dy) - start) / math::max(1.0 - start, 1e-3));
            let wall = if sharp >= 0.999 {
                if t > 0.0 { 1.0 } else { 0.0 }
            } else {
                math::smootherstep(math::saturate(t / math::max(sharp, 0.05)))
            };
            let ripple = n.perlin2(x as f64 * 0.03, y as f64 * 0.03);
            let i = out.index(x, y);
            out.data[i] = (p.base as f64
                + p.amplitude as f64 * relief * ripple * (1.0 - wall)
                + berm * wall) as f32;
        }
    }
    Ok(out)
}

/// Names of every generator, for error messages and the editor's palette.
pub const KIND_NAMES: [&str; 16] = [
    "rolling_hills",
    "mountains",
    "alpine",
    "plateau",
    "canyon",
    "badlands",
    "dunes",
    "islands",
    "volcanic",
    "craters",
    "glacial",
    "rivers",
    "coastline",
    "farmland",
    "arena",
    "pits",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<(&'static str, Generator)> {
        vec![
            ("rolling_hills", Generator::RollingHills(RollingHillsParams::default())),
            ("mountains", Generator::Mountains(MountainsParams::default())),
            ("alpine", Generator::Alpine(AlpineParams::default())),
            ("plateau", Generator::Plateau(PlateauParams::default())),
            ("canyon", Generator::Canyon(CanyonParams::default())),
            ("badlands", Generator::Badlands(BadlandsParams::default())),
            ("dunes", Generator::Dunes(DunesParams::default())),
            ("islands", Generator::Islands(IslandsParams::default())),
            ("volcanic", Generator::Volcanic(VolcanicParams::default())),
            ("craters", Generator::Craters(CratersParams::default())),
            ("glacial", Generator::Glacial(GlacialParams::default())),
            ("rivers", Generator::Rivers(RiversParams::default())),
            ("coastline", Generator::Coastline(CoastlineParams::default())),
            ("farmland", Generator::Farmland(FarmlandParams::default())),
            ("arena", Generator::Arena(ArenaParams::default())),
            ("pits", Generator::Pits(PitsParams::default())),
        ]
    }

    #[test]
    fn every_generator_statistics() {
        for (name, g) in all() {
            let hf = g.generate(48, 40, 8.0).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((hf.width, hf.height), (48, 40));
            assert!(hf.is_finite(), "{name} produced a non-finite sample");
            let (lo, hi) = g.height_range();
            assert!(lo.is_finite() && hi.is_finite() && hi >= lo, "{name} range {lo}..{hi}");
            let (mn, mx) = hf.range();
            assert!(mn >= lo - 1e-3, "{name} min {mn} below {lo}");
            assert!(mx <= hi + 1e-3, "{name} max {mx} above {hi}");
            assert!(mx > mn, "{name} is completely flat");
            // A real surface is not a plane, so its normals must not all agree.
            let ns = hf.normals();
            let spread = ns
                .iter()
                .map(|n| (n[0] * n[0] + n[1] * n[1]).sqrt() as f64)
                .fold(0.0f64, f64::max);
            assert!(spread > 1e-3, "{name} is flat everywhere");
            assert!(ns.iter().all(|n| (n[0] * n[0] + n[1] * n[1] + n[2] * n[2] - 1.0).abs() < 1e-5));
        }
        assert_eq!(KIND_NAMES.len(), all().len());

        // Statistics that only hold for the type they belong to.
        let dunes = Generator::Dunes(DunesParams::default()).generate(96, 96, 8.0).unwrap();
        let slope = dunes.slope_deg();
        assert!(
            slope.iter().filter(|s| **s > 3.0).count() > 96 * 96 / 4,
            "a dune field is not made of slopes"
        );
        let islands = Generator::Islands(IslandsParams::default()).generate(96, 96, 8.0).unwrap();
        let sea = IslandsParams::default().base + IslandsParams::default().amplitude * 0.18;
        for x in 0..96u32 {
            assert!(islands.get(x, 0).unwrap() <= sea + 1e-3);
            assert!(islands.get(x, 95).unwrap() <= sea + 1e-3);
        }
        assert!(islands.max() > sea + 100.0, "the archipelago has no dry land");
        let canyon = Generator::Canyon(CanyonParams::default()).generate(96, 96, 8.0).unwrap();
        let mut sorted = canyon.data.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(
            sorted[sorted.len() / 10] < sorted[sorted.len() * 9 / 10] - 100.0,
            "the canyon floor is not below the plateau"
        );
        let arena = Generator::Arena(ArenaParams::default()).generate(96, 96, 8.0).unwrap();
        let centre = arena.get(48, 48).unwrap();
        let mut rim = f32::NEG_INFINITY;
        for x in 0..96u32 {
            rim = rim.max(arena.get(x, 0).unwrap());
        }
        assert!(rim > centre + ArenaParams::default().berm * 0.8, "the arena has no berm");
        // Full flatness puts every sample on the middle of its own terrace, and
        // no flatness at all spreads them back across the whole step.
        let step = FarmlandParams::default().step;
        let off_centre = |p: &FarmlandParams| {
            let hf = Generator::Farmland(*p).generate(64, 64, 8.0).unwrap();
            hf.data
                .iter()
                .map(|v| (v - (v.div_euclid(step) + 0.5) * step).abs())
                .fold(0.0f32, f32::max)
        };
        let level = off_centre(&FarmlandParams { flatness: 1.0, bund: 0.0, ..FarmlandParams::default() });
        let ramped = off_centre(&FarmlandParams { flatness: 0.0, bund: 0.0, ..FarmlandParams::default() });
        assert!(level < 1e-2, "the flat terraces are not level: {level}");
        assert!(ramped > step * 0.3, "a flatness of zero still levelled the field: {ramped}");
    }

    #[test]
    fn every_generator_is_deterministic() {
        for (name, g) in all() {
            let a = g.generate(32, 32, 4.0).unwrap();
            let b = g.generate(32, 32, 4.0).unwrap();
            assert_eq!(a, b, "{name} is not deterministic");
        }
    }

    #[test]
    fn a_different_seed_gives_a_different_surface() {
        // A resolution where every generator's default parameters leave room to
        // see structure, so a saturated field cannot hide a missing seed.
        for (name, g) in all() {
            let a = g.generate(96, 96, 4.0).unwrap();
            let b = reseed(&g).generate(96, 96, 4.0).unwrap();
            assert_ne!(a.data, b.data, "{name} ignored its seed");
        }
    }

    fn reseed(g: &Generator) -> Generator {
        let s = g.seed().wrapping_add(977);
        let mut out = g.clone();
        match &mut out {
            Generator::RollingHills(p) => p.seed = s,
            Generator::Mountains(p) => p.seed = s,
            Generator::Alpine(p) => p.seed = s,
            Generator::Plateau(p) => p.seed = s,
            Generator::Canyon(p) => p.seed = s,
            Generator::Badlands(p) => p.seed = s,
            Generator::Dunes(p) => p.seed = s,
            Generator::Islands(p) => p.seed = s,
            Generator::Volcanic(p) => p.seed = s,
            Generator::Craters(p) => p.seed = s,
            Generator::Glacial(p) => p.seed = s,
            Generator::Rivers(p) => p.seed = s,
            Generator::Coastline(p) => p.seed = s,
            Generator::Farmland(p) => p.seed = s,
            Generator::Arena(p) => p.seed = s,
            Generator::Pits(p) => p.seed = s,
            Generator::Heightmap(h) => h.seed = s,
        }
        out
    }

    #[test]
    fn generate_rejects_bad_grids() {
        let g = Generator::RollingHills(RollingHillsParams::default());
        assert!(g.generate(0, 8, 1.0).is_err());
        assert!(g.generate(1, 8, 1.0).is_err());
        assert!(g.generate(8, 8, 0.0).is_err());
        assert!(g.generate(8, 8, f32::NAN).is_err());
        // Absurd sizes are refused rather than attempted.
        assert!(matches!(
            g.generate(1 << 20, 1 << 20, 1.0),
            Err(Error::HeightmapTooLarge(..))
        ));
    }

    #[test]
    fn dunes_have_a_dominant_gradient_direction() {
        for pattern in [DunePattern::Transverse, DunePattern::Barchan, DunePattern::Star] {
            let p = DunesParams { pattern, ..DunesParams::default() };
            let hf = Generator::Dunes(p).generate(160, 160, 4.0).unwrap();
            // The mean gradient of a periodic field cancels, so the direction
            // comes from the structure tensor: the gradient of a dune field
            // varies along the wind and is flat across the crests.
            let (mut xx, mut xy, mut yy) = (0.0f64, 0.0f64, 0.0f64);
            for y in 2..158u32 {
                for x in 2..158u32 {
                    let (gx, gy) = hf.gradient(x, y);
                    let (gx, gy) = (gx as f64, gy as f64);
                    xx += gx * gx;
                    xy += gx * gy;
                    yy += gy * gy;
                }
            }
            let trace = xx + yy;
            let det = xx * yy - xy * xy;
            // Larger eigenvalue of the 2x2 tensor is the dominant gradient axis.
            let disc = math::sqrt(math::max(trace * trace * 0.25 - det, 0.0));
            let major = trace * 0.5 + disc;
            assert!(major > 0.0, "{pattern:?} has no gradient at all");
            // The dominant axis angle, from the eigenvector of that eigenvalue.
            let (mut vx, mut vy) = (major - yy, xy);
            if math::sqrt(vx * vx + vy * vy) < 1e-12 {
                vx = xy;
                vy = major - xx;
            }
            let angle = math::atan2(vy, vx);
            let wind = math::to_radians(p.wind_deg as f64);
            // The wind axis and the dominant gradient axis are the same line,
            // so the angle between them is at most a right turn.
            let diff = math::atan2(math::sin(angle - wind), math::cos(angle - wind)).abs();
            assert!(diff < math::FRAC_PI_2 + 1e-6, "{pattern:?} dominant gradient is {diff} rad off the wind");
        }
    }

    #[test]
    fn craters_have_a_rim_above_the_centre() {
        let p = CratersParams { count: 8, max_radius: 20.0, background: 0.0, ..CratersParams::default() };
        let hf = Generator::Craters(p).generate(96, 96, 4.0).unwrap();
        // The bowl of a crater is its lowest point, so the global minimum is
        // inside one; the ejecta lip is a ring of local maxima further out.
        let mut floor = f32::INFINITY;
        for x in 0..96u32 {
            for y in 0..96u32 {
                floor = floor.min(hf.get(x, y).unwrap());
            }
        }
        assert!(floor < p.base - 1.0, "no bowl was cut: {floor}");
        // Somewhere in the annulus there is a local maximum well above the floor.
        let mut rims = 0;
        for y in 3..93u32 {
            for x in 3..93u32 {
                let here = hf.get(x, y).unwrap();
                let mut peak = true;
                for (dx, dy) in [(-3i64, 0i64), (3, 0), (0, -3), (0, 3), (-3, -3), (3, 3)] {
                    if hf.get_clamped(x as i64 + dx, y as i64 + dy) > here {
                        peak = false;
                    }
                }
                if peak && here > floor + 1.0 {
                    rims += 1;
                }
            }
        }
        assert!(rims > 4, "no crater rim stands above the floor: {rims}");
    }

    #[test]
    fn islands_fall_away_to_the_water_at_the_border() {
        // A radius far larger than the map still has to leave open water at the
        // edge, so the border check holds for any parameters.
        for p in [
            IslandsParams { centres: 2, radius: 26.0, ..IslandsParams::default() },
            IslandsParams { centres: 1, radius: 400.0, ..IslandsParams::default() },
        ] {
            let hf = Generator::Islands(p).generate(128, 128, 4.0).unwrap();
            let sea = p.base + p.amplitude * p.water_level;
            for y in 0..128u32 {
                for x in 0..128u32 {
                    if x == 0 || y == 0 || x == 127 || y == 127 {
                        let v = hf.get(x, y).unwrap();
                        assert!(v <= sea + 1.0, "border ({x},{y}) at {v} is above sea level {sea}");
                    }
                }
            }
            // At least one sample must be dry land.
            assert!(hf.max() > sea + p.amplitude * 0.2, "no dry land at all");
        }
    }

    #[test]
    fn canyons_cut_a_floor_below_the_plateau() {
        let hf = Generator::Canyon(CanyonParams::default()).generate(96, 96, 4.0).unwrap();
        // The lowest tenth of the field is the canyon floor; everything above
        // is the plateau it was cut into.
        let mut sorted = hf.data.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let floor = sorted[sorted.len() / 10];
        let rim = sorted[sorted.len() - sorted.len() / 10];
        assert!(floor < rim - 1.0, "floor {floor} is not below the rim {rim}");
    }

    #[test]
    fn plateau_terraces_into_flat_benches() {
        let p = PlateauParams { sharpness: 0.95, steps: 6.0, ..PlateauParams::default() };
        let hf = Generator::Plateau(p).generate(64, 64, 4.0).unwrap();
        let step = p.amplitude / p.steps.max(1.0);
        // Most of the surface sits within a small fraction of a step of a
        // bench level rather than on the smooth ramp between them.
        let mut near = 0;
        for v in &hf.data {
            let frac = (v / step).abs() % 1.0;
            let d = if frac > 0.5 { 1.0 - frac } else { frac };
            if d < 0.18 {
                near += 1;
            }
        }
        let frac = near as f64 / hf.data.len() as f64;
        assert!(frac > 0.3, "only {frac} of the surface is on a bench");
    }

    #[test]
    fn arena_has_a_raised_border_and_a_flat_floor() {
        let p = ArenaParams::default();
        let hf = Generator::Arena(p).generate(96, 96, 4.0).unwrap();
        let centre = hf.get(48, 48).unwrap();
        let corner = hf.get(0, 0).unwrap();
        assert!(corner > centre + p.berm * 0.8, "corner {corner} centre {centre}");
        // The floor is nearly level.
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for y in 30..66u32 {
            for x in 30..66u32 {
                let v = hf.get(x, y).unwrap();
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        assert!((hi - lo) < p.amplitude * p.floor_relief * 6.0 + 1.0, "floor span {lo}..{hi}");
    }

    #[test]
    fn volcanic_has_a_caldera_below_the_rim() {
        let p = VolcanicParams::default();
        let hf = Generator::Volcanic(p).generate(96, 96, 4.0).unwrap();
        // The centre is the caldera floor; the rim is the lip where the bowl
        // meets the cone, one caldera radius out, and it is the summit.
        let centre = hf.get(48, 48).unwrap();
        let rim = hf.get(48 + (p.caldera * 0.95) as u32, 48).unwrap();
        assert!(rim > centre + 1.0, "the caldera rim {rim} is not above the floor {centre}");
        // The cone falls away from the rim to its foot and never leaves the
        // declared range.
        let foot = hf.get(48 + p.radius as u32, 48).unwrap();
        assert!(foot < rim, "the cone did not fall away to its foot: {foot} vs {rim}");
        assert!(hf.max() <= p.base + p.amplitude + 1.0, "the cone grew past the declared range");
    }

    #[test]
    fn farmland_makes_level_terraces() {
        let p = FarmlandParams { step: 32.0, flatness: 0.95, bund: 6.0, ..FarmlandParams::default() };
        let hf = Generator::Farmland(p).generate(64, 64, 4.0).unwrap();
        // A bund exists: some samples sit above their neighbours' terrace level.
        let mut rises = 0;
        for y in 1..63u32 {
            for x in 0..64u32 {
                if hf.get(x, y).unwrap() > hf.get(x, y - 1).unwrap() + 1.0 {
                    rises += 1;
                }
            }
        }
        assert!(rises > 20, "no bunds found");
    }

    #[test]
    fn glacial_carves_a_flat_floor_along_the_spline() {
        let hf = Generator::Glacial(GlacialParams::default()).generate(96, 96, 4.0).unwrap();
        // The trench is a long low corridor: its mean is well below the field's
        // mean, and it runs the full height of the map rather than sitting in
        // one corner.
        let mean: f64 = hf.data.iter().map(|v| *v as f64).sum::<f64>() / hf.data.len() as f64;
        let low = hf.data.iter().filter(|v| (**v as f64) < mean - 100.0).count();
        assert!(low > 200, "trench is too small: {low} samples");
        let mut per_row = 0;
        for y in 0..96u32 {
            let below = (0..96u32).filter(|x| (hf.get(*x, y).unwrap() as f64) < mean - 100.0).count();
            if below > 3 {
                per_row += 1;
            }
        }
        assert!(per_row > 80, "the trench only crosses {per_row} of 96 rows");
    }

    #[test]
    fn rivers_are_cut_along_the_flow_lines() {
        let p = RiversParams::default();
        let before = Heightfield::from_fn(96, 96, 4.0, |x, y| {
            let n = Noise::new(p.seed);
            let w = Noise::new(substream(p.seed, 0x5741_5250));
            p.shape
                .sample(&n, &w, x as f64, y as f64)
                .mul_add(p.amplitude as f64, p.base as f64) as f32
        })
        .unwrap();
        let after = Generator::Rivers(p).generate(96, 96, 4.0).unwrap();
        // Incision follows the flow lines, so the ground under the biggest
        // catchments is cut much deeper than the ground between them. The flow
        // is measured on the surface the rivers were cut into, and a drainage
        // network is far too skewed for equal-width buckets, so the samples are
        // ranked instead.
        let flow = before.flow_accumulation();
        let mut ranked: Vec<usize> = (0..before.data.len()).collect();
        ranked.sort_by(|a, b| {
            flow.area[*a].total_cmp(&flow.area[*b]).then(a.cmp(b))
        });
        let mut cut = [0.0f64; 4];
        let mut seen = [0u32; 4];
        for (rank, i) in ranked.iter().enumerate() {
            let bucket = (rank * 4) / ranked.len().max(1);
            let bucket = if bucket < 4 { bucket } else { 3 };
            cut[bucket] += (before.data[*i] - after.data[*i]) as f64;
            seen[bucket] += 1;
        }
        assert!(seen.iter().all(|c| *c > 100), "the catchment quartiles are uneven: {seen:?}");
        let mean: Vec<f64> = (0..4).map(|b| cut[b] / seen[b] as f64).collect();
        assert!(mean[3] < 0.0, "the wettest ground was not cut: {mean:?}");
        assert!(
            mean[3] - mean[0] < -50.0,
            "the cut does not grow with the catchment: {mean:?}"
        );
        // Trunk channels exist where the catchment is large.
        let after_flow = after.flow_accumulation();
        assert!(
            after.data.iter().zip(before.data.iter()).any(|(a, b)| *a < *b),
            "no sample was cut at all"
        );
        let mut trunk = 0;
        for i in 0..after.data.len() {
            if after_flow.area[i] > after_flow.max_area() * 0.4 && after.data[i] < before.data[i] - 5.0 {
                trunk += 1;
            }
        }
        assert!(trunk > 20, "no trunk channels cut: {trunk}");
    }

    #[test]
    fn coastline_puts_deep_water_on_one_side() {
        let p = CoastlineParams::default();
        let hf = Generator::Coastline(p).generate(96, 96, 4.0).unwrap();
        let sea = p.base + p.amplitude * p.water_level;
        let half: Vec<f32> = (0..48).map(|i| hf.get(i, 48).unwrap()).collect();
        let first = half[0];
        let last = half[47];
        assert!((first - last).abs() > p.amplitude * 0.3, "no shoreline gradient across the row");
        assert!(first.min(last) <= sea + p.amplitude * 0.05);
    }

    #[test]
    fn badlands_are_dissected() {
        let hf = Generator::Badlands(BadlandsParams::default()).generate(96, 96, 4.0).unwrap();
        // A dissected field has far more slope than a smooth dome of the same
        // relief, and its curvature is strongly negative in the gullies.
        let slope: f64 = hf.slope().iter().map(|s| *s as f64).sum::<f64>() / hf.data.len() as f64;
        assert!(slope > 0.05, "mean slope {slope} is too gentle to be badlands");
        let curv = hf.curvature();
        let neg = curv.iter().filter(|c| **c < 0.0).count();
        assert!(neg > hf.data.len() / 4, "not enough incised gullies: {neg}");
    }

    #[test]
    fn alpine_runs_erosion_after_the_shape() {
        let raw = Generator::Mountains(MountainsParams::default()).generate(64, 64, 8.0).unwrap();
        let eroded = Generator::Alpine(AlpineParams::default()).generate(64, 64, 8.0).unwrap();
        assert!(eroded.is_finite());
        let talus = math::tan(math::to_radians(38.0)) as f32 * 8.0;
        let steep = |g: &Heightfield| {
            (1..63u32)
                .map(|x| (g.get(x + 1, 32).unwrap() - g.get(x, 32).unwrap()).abs())
                .filter(|d| *d > talus)
                .count()
        };
        // Thermal and hydraulic erosion together leave far fewer over-talus
        // steps than the raw ridged field has.
        assert!(steep(&eroded) < steep(&raw), "alpine {} vs raw {}", steep(&eroded), steep(&raw));
        // Erosion also takes the fine detail off, so the surface is smoother
        // than the raw field even where deposition has built new ground up.
        let roughness = |g: &Heightfield| {
            let c = g.curvature();
            c.iter().map(|v| (*v as f64).abs()).sum::<f64>() / c.len() as f64
        };
        assert!(
            roughness(&eroded) < roughness(&raw),
            "alpine {} vs raw {}",
            roughness(&eroded),
            roughness(&raw)
        );
    }

    #[test]
    fn pits_are_the_inverse_of_craters() {
        let a = Generator::Craters(CratersParams {
            count: 8,
            max_radius: 20.0,
            background: 0.0,
            rim: 0.0,
            ..CratersParams::default()
        })
        .generate(64, 64, 4.0)
        .unwrap();
        let b = Generator::Pits(PitsParams { count: 8, max_radius: 20.0, ..PitsParams::default() })
            .generate(64, 64, 4.0)
            .unwrap();
        let (alo, ahi) = a.range();
        let (blo, bhi) = b.range();
        assert!(alo < bhi, "craters {alo}..{ahi} and pits {blo}..{bhi} do not overlap");
    }

    #[test]
    fn generators_respect_resolution_changes() {
        for (_, g) in all() {
            let small = g.generate(24, 24, 8.0).unwrap();
            let large = g.generate(48, 48, 8.0).unwrap();
            assert!(small.is_finite() && large.is_finite());
            assert_eq!(small.extent_x(), 23.0 * 8.0);
            assert_eq!(large.extent_x(), 47.0 * 8.0);
        }
    }

    #[test]
    fn generators_survive_hostile_parameters() {
        let cases = vec![
            Generator::Dunes(DunesParams { wavelength: 0.0, crest_sharpness: f32::NAN, ..Default::default() }),
            Generator::Dunes(DunesParams { wavelength: -5.0, wind_deg: 1e9, ..Default::default() }),
            Generator::Islands(IslandsParams { centres: 0, radius: 0.0, shelf: -3.0, ..Default::default() }),
            Generator::Volcanic(VolcanicParams { radius: 0.0, caldera: -1.0, flows: 999, ..Default::default() }),
            Generator::Craters(CratersParams { count: 3, max_radius: 0.0, size_decay: 0.0, ..Default::default() }),
            Generator::Canyon(CanyonParams { channels: 0, channel_width: 0.0, steps: -1.0, ..Default::default() }),
            Generator::Plateau(PlateauParams { steps: 0.0, sharpness: 5.0, tilt: f32::INFINITY, ..Default::default() }),
            Generator::Farmland(FarmlandParams { step: 0.0, flatness: 9.0, ..Default::default() }),
            Generator::Rivers(RiversParams { width: 0.0, lakes: 4.0, incision: -1.0, ..Default::default() }),
            Generator::Pits(PitsParams { count: 0, max_radius: 0.0, ..Default::default() }),
        ];
        for g in cases {
            let hf = g.generate(40, 40, 4.0).unwrap();
            assert!(hf.is_finite());
        }
    }

    #[test]
    fn terrace_field_helpers_behave() {
        let mut hf = Heightfield::from_fn(16, 16, 1.0, |x, _| x as f32 * 10.0).unwrap();
        let (lo, hi) = hf.range();
        terrace_field(&mut hf, 25.0, 1.0);
        // A hard terrace lands every sample on a step and never leaves the
        // range it was cut from.
        for v in &hf.data {
            let q = (*v / 25.0).round();
            assert!((*v / 25.0 - q).abs() < 1e-3, "{v} is off the step grid");
            assert!(*v >= lo && *v <= hi, "{v} left the range {lo}..{hi}");
        }
        // A soft terrace keeps the same range but rounds its risers.
        let mut soft = Heightfield::from_fn(16, 16, 1.0, |x, _| x as f32 * 10.0).unwrap();
        terrace_field(&mut soft, 25.0, 0.0);
        assert!((soft.max() - hi).abs() < 1e-3);
        assert!((soft.min() - lo).abs() < 1e-3);
        assert_ne!(soft.data, hf.data);
        // A zero step count is a no-op, not a division by zero.
        let before = hf.clone();
        terrace_field(&mut hf, 0.0, 1.0);
        assert_eq!(hf, before);
    }

    #[test]
    fn polyline_and_channel_helpers() {
        let path = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert!((polyline_distance(&path, 5.0, 0.0) - 0.0).abs() < 1e-9);
        assert!((polyline_distance(&path, 5.0, 3.0) - 3.0).abs() < 1e-9);
        // A single point still gives a finite distance.
        let one = vec![(2.0, 2.0)];
        assert!(polyline_distance(&one, 5.0, 5.0).is_finite());
        // An empty path cannot carve anything.
        let mut hf = Heightfield::filled(16, 16, 1.0, 100.0).unwrap();
        carve_channel(&mut hf, &[], 4.0, 0.0);
        assert!(hf.data.iter().all(|v| *v == 100.0));
        carve_channel(&mut hf, &path, 0.0, 0.0);
        assert!(hf.data.iter().all(|v| *v == 100.0));
    }

    #[test]
    fn dune_profile_spans_its_range() {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for k in 0..1000 {
            let t = k as f64 / 1000.0;
            let v = dune_profile(t, 0.6);
            assert!((0.0..=1.0).contains(&v), "profile out of range: {v}");
            lo = lo.min(v);
            hi = hi.max(v);
        }
        assert!(lo < 0.05 && hi > 0.95, "profile range {lo}..{hi}");
    }

    #[test]
    fn shape_params_apply_rotation_anisotropy_and_warp() {
        let p = ShapeParams {
            anisotropy: 0.5,
            rotation_deg: 33.0,
            warp: 0.4,
            ..ShapeParams::default()
        };
        let n = Noise::new(1);
        let wn = Noise::new(2);
        for x in 0..8u32 {
            for y in 0..8u32 {
                let v = p.sample(&n, &wn, x as f64, y as f64);
                assert!(v.is_finite() && v.abs() <= 1.001, "shape sample {v}");
            }
        }
        // A zero warp leaves the sample point where it was.
        let flat = ShapeParams { warp: 0.0, ..p.clone() };
        assert!((flat.sample(&n, &wn, 3.0, 4.0) - flat.sample(&n, &wn, 3.0, 4.0)).abs() < 1e-15);
    }
}
