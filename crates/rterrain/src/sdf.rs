//! Volumetric terrain: a signed distance field on a regular grid, evaluated
//! from a list of operations, and meshed with surface nets.
//!
//! A negative distance is inside solid. Because the field is a true distance,
//! a plain union, subtraction or intersection is exact even where the operands
//! do not overlap, which is what keeps chunk borders watertight.

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::heightfield::Heightfield;
use crate::math;
use crate::noise::Noise;

/// Ceiling on voxels, so a hostile operation list cannot ask for a grid no
/// machine can hold.
pub const MAX_VOXELS: u64 = 1 << 27;

/// A closed shape the field can be built from. Every one is a true distance
/// function in inches, so the combinators behave as authored.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum SdfShape {
    Sphere {
        center: DVec3,
        radius: f64,
    },
    /// A swept circle from `a` to `b`.
    Capsule {
        a: DVec3,
        b: DVec3,
        radius: f64,
    },
    /// Axis-aligned box rotated by pitch, yaw and roll in degrees.
    Box {
        center: DVec3,
        half: DVec3,
        rotation: [f64; 3],
    },
    /// A capped cylinder standing on `base`, so its axis is +Z.
    Cylinder {
        base: DVec3,
        height: f64,
        radius: f64,
    },
    /// A capped cone standing on `base`, so its axis is +Z.
    Cone {
        base: DVec3,
        height: f64,
        radius: f64,
    },
    /// A torus in the XY plane.
    Torus {
        center: DVec3,
        major: f64,
        minor: f64,
    },
    /// Everything on the side the normal points away from is solid.
    HalfSpace {
        point: DVec3,
        normal: DVec3,
    },
}

impl SdfShape {
    /// Distance from `p` to the shape's surface, negative inside.
    pub fn distance(&self, p: DVec3) -> f64 {
        match *self {
            SdfShape::Sphere { center, radius } => (p - center).length() - radius.abs(),
            SdfShape::Capsule { a, b, radius } => {
                capsule_distance(p, a, b, radius.abs())
            }
            SdfShape::Box { center, half, rotation } => {
                box_distance(p, center, half, rotation)
            }
            SdfShape::Cylinder { base, height, radius } => {
                cylinder_distance(p, base, height.abs(), radius.abs())
            }
            SdfShape::Cone { base, height, radius } => {
                cone_distance(p, base, height.abs(), radius.abs())
            }
            SdfShape::Torus { center, major, minor } => {
                let q = p - center;
                let ring = math::sqrt(q.x * q.x + q.y * q.y) - major.abs();
                let m = minor.abs();
                math::sqrt(ring * ring + q.z * q.z) - m
            }
            SdfShape::HalfSpace { point, normal } => {
                let n = normal.normalize_or(DVec3::Z);
                (p - point).dot(n)
            }
        }
    }

    /// Conservative bound on the distance, used to skip work when far away.
    pub fn reach(&self) -> f64 {
        match *self {
            SdfShape::Sphere { radius, .. } => radius.abs() * 2.0,
            SdfShape::Capsule { a, b, radius } => (b - a).length() + radius.abs() * 2.0,
            SdfShape::Box { half, .. } => half.length() * 2.0,
            SdfShape::Cylinder { height, radius, base } => {
                height.abs() + radius.abs() * 2.0 + (base.length())
            }
            SdfShape::Cone { height, radius, .. } => height.abs() + radius.abs() * 2.0,
            SdfShape::Torus { major, minor, .. } => major.abs() * 2.0 + minor.abs() * 2.0,
            SdfShape::HalfSpace { .. } => 1.0e30,
        }
    }
}

fn capsule_distance(p: DVec3, a: DVec3, b: DVec3, r: f64) -> f64 {
    let ba = b - a;
    let pa = p - a;
    let denom = ba.dot(ba);
    let t = if denom > 0.0 { math::saturate(pa.dot(ba) / denom) } else { 0.0 };
    (pa - ba * t).length() - r
}

/// Inverse-rotate `p` into the box's frame, then take a box distance.
fn box_distance(p: DVec3, center: DVec3, half: DVec3, rotation: [f64; 3]) -> f64 {
    let (pitch, yaw, roll) =
        (math::to_radians(rotation[0]), math::to_radians(rotation[1]), math::to_radians(rotation[2]));
    let (sp, cp) = (math::sin(pitch), math::cos(pitch));
    let (sy, cy) = (math::sin(yaw), math::cos(yaw));
    let (sr, cr) = (math::sin(roll), math::cos(roll));
    // Undo the rotation in the order it was applied.
    let mut q = p - center;
    let (y0, z0) = (q.y * cr + q.z * sr, -q.y * sr + q.z * cr);
    q.y = y0;
    q.z = z0;
    let (x1, z1) = (q.x * cp + q.z * sp, -q.x * sp + q.z * cp);
    q.x = x1;
    q.z = z1;
    let (x2, y2) = (q.x * cy - q.y * sy, q.x * sy + q.y * cy);
    q.x = x2;
    q.y = y2;
    let d = DVec3::new(q.x.abs() - half.x.abs(), q.y.abs() - half.y.abs(), q.z.abs() - half.z.abs());
    let outside = DVec3::new(d.x.max(0.0), d.y.max(0.0), d.z.max(0.0)).length();
    outside + d.x.max(d.y).max(d.z).min(0.0)
}

fn cylinder_distance(p: DVec3, base: DVec3, height: f64, radius: f64) -> f64 {
    let q = p - base;
    let radial = DVec3::new(q.x, q.y, 0.0).length() - radius;
    let axial = (q.z - height * 0.5).abs() - height * 0.5;
    let outside = math::sqrt(radial.max(0.0) * radial.max(0.0) + axial.max(0.0) * axial.max(0.0));
    outside + radial.max(axial).min(0.0)
}

/// Distance to a capped cone standing on `base` with its axis along +Z.
///
/// The surface is the base disc, the ruled side and the apex, which in the
/// (radius, height) half-plane is a disc plus a common tangent. The result is
/// exact outside and a lower bound inside, which is all a mesher needs and is
/// what every distance function of this family does.
fn cone_distance(p: DVec3, base: DVec3, height: f64, radius: f64) -> f64 {
    let q = p - base;
    // The half-plane runs from the base disc at -h to the apex at +h.
    let h = height * 0.5;
    let qx = math::sqrt(q.x * q.x + q.y * q.y);
    let qy = q.z - h;
    let (r1, r2) = (radius, 0.0);
    let k1 = (r2, h);
    let k2 = (r2 - r1, 2.0 * h);
    let cap = if qy < 0.0 { r1 } else { r2 };
    let ca = (qx - math::min(qx, cap), qy.abs() - h);
    let denom = k2.0 * k2.0 + k2.1 * k2.1;
    let along = if denom > 0.0 {
        math::saturate(((k1.0 - qx) * k2.0 + (k1.1 - qy) * k2.1) / denom)
    } else {
        0.0
    };
    let cb = (qx - k1.0 + k2.0 * along, qy - k1.1 + k2.1 * along);
    let d2 = math::min(ca.0 * ca.0 + ca.1 * ca.1, cb.0 * cb.0 + cb.1 * cb.1);
    let sign = if cb.0 < 0.0 && ca.1 < 0.0 { -1.0 } else { 1.0 };
    sign * math::sqrt(d2)
}

/// One step of a field's construction. The list is applied in order, so the
/// first entry must produce a solid and every later entry modifies it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SdfOp {
    /// Replace the field with the shape.
    Add { shape: SdfShape },
    /// Grow the current solid by the shape.
    Union {
        shape: SdfShape,
        /// Blend radius; zero is a hard union.
        k: f64,
    },
    /// Cut the shape out of the current solid.
    Subtract {
        shape: SdfShape,
        k: f64,
    },
    /// Keep only what both the current solid and the shape contain.
    Intersect {
        shape: SdfShape,
        k: f64,
    },
    /// Push the surface in and out by a noise field.
    Noise {
        amplitude: f64,
        frequency: f64,
        seed: u64,
        kind: NoiseKind3,
    },
    /// Move the current field by an offset, which repositions a whole solid.
    Shift {
        offset: DVec3,
    },
    /// Scale the whole volume about the object origin, dilating or shrinking it.
    ///
    /// Every shape in the list is evaluated in the scaled space, so a subtraction
    /// authored after the scale moves with the solid it cuts. Scaling the
    /// distances themselves would leave the surface exactly where it was.
    Scale {
        factor: f64,
    },
    /// Carve a heightfield slab out of the solid, which is how a cave is dug
    /// into a heightfield terrain.
    Field {
        field: VolumeField,
        /// Fillet radius on the cut; zero is a hard carve.
        k: f64,
    },
}

/// A heightfield carried inside a field, as a slab of the given thickness whose
/// top face follows the samples. Subtracting one leaves a landscape with a cave
/// running through it, and a field with a ridge standing above the rest.
///
/// The samples travel with the op list so a recipe stays pure data, which is what
/// lets the browser and the command line build the same volume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolumeField {
    /// Object-space position of sample `(0, 0)`.
    pub origin: DVec3,
    /// World size of one sample, in inches.
    pub cell: f64,
    /// Row-major heights, `width * height` of them.
    pub width: u32,
    pub height: u32,
    pub samples: Vec<f32>,
    /// Depth of the slab below its surface, in inches.
    pub thickness: f64,
}

impl VolumeField {
    /// Build a field from a heightfield, offset so sample `(0, 0)` sits at
    /// `origin`.
    pub fn from_heightfield(hf: &Heightfield, origin: DVec3, cell: f64, thickness: f64) -> Result<Self> {
        if hf.data.is_empty() {
            return Err(Error::ZeroSize);
        }
        if !cell.is_finite() || cell <= 0.0 {
            return Err(Error::InvalidVoxelSize(cell));
        }
        if !thickness.is_finite() || thickness <= 0.0 {
            return Err(Error::BadParams("a heightfield slab needs a positive thickness"));
        }
        if !origin.is_finite() {
            return Err(Error::BadParams("sdf origin must be finite"));
        }
        Ok(Self {
            origin,
            cell,
            width: hf.width,
            height: hf.height,
            samples: hf.data.clone(),
            thickness,
        })
    }

    /// Surface height at a point, bilinear between the four nearest samples.
    fn surface(&self, p: DVec3) -> f64 {
        let gx = (p.x - self.origin.x) / self.cell;
        let gy = (p.y - self.origin.y) / self.cell;
        let x = math::clamp(gx, 0.0, (self.width - 1) as f64);
        let y = math::clamp(gy, 0.0, (self.height - 1) as f64);
        let x0 = x.floor() as u32;
        let y0 = y.floor() as u32;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;
        let at = |ix: u32, iy: u32| f64::from(self.samples[iy as usize * self.width as usize + ix as usize]);
        let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
        let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
        top + (bot - top) * fy + self.origin.z
    }

    /// Distance to the slab, negative inside it.
    ///
    /// A heightfield is exact in x and y and varies only in z, so the slab is the
    /// intersection of six half-spaces and its distance is the largest of their
    /// plane distances.
    pub fn distance(&self, p: DVec3) -> f64 {
        let top = self.surface(p);
        let bottom = top - self.thickness;
        let gx = (p.x - self.origin.x) / self.cell;
        let gy = (p.y - self.origin.y) / self.cell;
        let vertical = math::max(p.z - top, bottom - p.z);
        let lateral = math::max(math::max(-gx, gx - (self.width as f64 - 1.0)), math::max(-gy, gy - (self.height as f64 - 1.0)))
            * self.cell;
        math::max(vertical, lateral)
    }

    /// Object-space bounds of the slab, padded by `pad` inches.
    pub fn bounds(&self, pad: f64) -> (DVec3, DVec3) {
        let lowest = self
            .samples
            .iter()
            .copied()
            .fold(f64::INFINITY, |a, b| a.min(f64::from(b)))
            + self.origin.z;
        let lo = DVec3::new(self.origin.x, self.origin.y, lowest - self.thickness - pad);
        let hi = DVec3::new(
            self.origin.x + (self.width as f64 - 1.0) * self.cell,
            self.origin.y + (self.height as f64 - 1.0) * self.cell,
            self.max_height() + pad,
        );
        (lo, hi)
    }

    /// Highest sample in the field, for building a bounding volume around it.
    pub fn max_height(&self) -> f64 {
        self.samples.iter().copied().fold(f64::NEG_INFINITY, |a, b| a.max(f64::from(b))) + self.origin.z
    }
}

/// Which noise family displaces an SDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NoiseKind3 {
    #[default]
    Perlin,
    Simplex,
    Value,
    /// Keeps the displacement centred on zero, so it neither dilates nor erodes.
    Symmetric,
}

impl NoiseKind3 {
    /// Evaluate in -1..1 at an object-space point.
    pub fn sample(&self, n: &Noise, x: f64, y: f64, z: f64) -> f64 {
        match self {
            NoiseKind3::Perlin => n.perlin3(x, y, z),
            NoiseKind3::Simplex => n.simplex3(x, y, z),
            NoiseKind3::Value => n.value3(x, y, z),
            NoiseKind3::Symmetric => n.perlin3(x, y, z) * n.value3(x + 3.1, y + 7.7, z + 1.3),
        }
    }
}

/// A signed distance field on a regular grid, with the recipe that made it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolumeTerrain {
    /// Field resolution in voxels per axis.
    pub chunks: [u32; 3],
    /// World size of one voxel, in inches.
    pub voxel_size: f64,
    /// Object-space low corner of the field.
    pub origin: DVec3,
    /// Operations applied in order.
    pub ops: Vec<SdfOp>,
}

impl VolumeTerrain {
    /// Voxel count of the field.
    pub fn voxel_count(&self) -> u64 {
        self.chunks[0] as u64 * self.chunks[1] as u64 * self.chunks[2] as u64
    }

    /// Validate the field's size and operation list.
    pub fn validate(&self) -> Result<()> {
        if self.chunks.iter().any(|c| *c < 2) {
            return Err(Error::SdfTooSmall(self.chunks[0], self.chunks[1], self.chunks[2]));
        }
        if self.voxel_count() > MAX_VOXELS {
            return Err(Error::SdfTooLarge(self.chunks[0], self.chunks[1], self.chunks[2]));
        }
        if !self.voxel_size.is_finite() || self.voxel_size <= 0.0 {
            return Err(Error::InvalidVoxelSize(self.voxel_size));
        }
        if self.ops.is_empty() {
            return Err(Error::BadParams("a volume terrain needs at least one operation"));
        }
        if !matches!(self.ops[0], SdfOp::Add { .. }) {
            return Err(Error::BadParams("the first sdf operation must be add"));
        }
        if !self.origin.is_finite() {
            return Err(Error::BadParams("sdf origin must be finite"));
        }
        Ok(())
    }

    /// The voxel resolution for a field spanning `min..max`: one sample per
    /// voxel corner on each axis, at least the two `validate` asks for, and
    /// halved until the whole grid fits in `MAX_VOXELS`.
    pub fn resolution_for(min: DVec3, max: DVec3, voxel_size: f64) -> [u32; 3] {
        let size = if voxel_size.is_finite() && voxel_size > 0.0 { voxel_size } else { 1.0 };
        let span = (max - min).abs();
        let mut out = [2u32; 3];
        for (axis, s) in [span.x, span.y, span.z].into_iter().enumerate() {
            if s.is_finite() {
                out[axis] = ((s / size).ceil() as u64 + 1).clamp(2, 1 << 16) as u32;
            }
        }
        while out.iter().map(|n| *n as u64).product::<u64>() > MAX_VOXELS {
            for n in out.iter_mut() {
                *n = (*n / 2).max(2);
            }
        }
        out
    }

    /// Voxels per chunk edge used by the mesher.
    pub const CHUNK_VOXELS: u32 = 8;

    /// Chunk grid size, per axis.
    pub fn chunk_grid(&self) -> [u32; 3] {
        let cv = Self::CHUNK_VOXELS;
        [
            self.chunks[0].div_ceil(cv).max(1),
            self.chunks[1].div_ceil(cv).max(1),
            self.chunks[2].div_ceil(cv).max(1),
        ]
    }

    /// Object-space position of voxel corner `(i, j, k)`.
    pub fn voxel_origin(&self, i: u32, j: u32, k: u32) -> DVec3 {
        self.origin + DVec3::new(
            i as f64 * self.voxel_size,
            j as f64 * self.voxel_size,
            k as f64 * self.voxel_size,
        )
    }

    /// Evaluate the field, returning `chunks[0] * chunks[1] * chunks[2]` signed
    /// distances in X-fastest order.
    ///
    /// The result is a pure function of the terrain, so two runs on two targets
    /// produce bit-identical distances. Every step reads the object-space point
    /// it is given, so a shift is folded into the sampling position and the
    /// whole list is evaluated once.
    pub fn evaluate(&self) -> Result<Vec<f32>> {
        self.validate()?;
        let (nx, ny, nz) = (self.chunks[0], self.chunks[1], self.chunks[2]);
        let n = (nx as usize) * (ny as usize) * (nz as usize);
        let mut shift = DVec3::ZERO;
        for op in &self.ops {
            if let SdfOp::Shift { offset } = op {
                if !offset.is_finite() {
                    return Err(Error::BadParams("sdf shift must be finite"));
                }
                shift += offset;
            }
        }
        let origin = self.origin - shift;
        if !origin.is_finite() {
            return Err(Error::BadParams("sdf origin must be finite"));
        }

        let mut field = vec![f32::INFINITY; n];
        // Every scale operation folds into the point each shape is sampled at,
        // so the whole list is still evaluated once, in order, at one point per
        // voxel. A distance multiplied by a factor has the same zero set, which
        // is why the scale goes into the sample point rather than the result.
        let mut scale = 1.0f64;
        for op in &self.ops {
            if let SdfOp::Scale { factor } = *op {
                if !factor.is_finite() || factor <= 0.0 {
                    return Err(Error::BadParams("sdf scale must be positive"));
                }
                scale *= factor;
                if !scale.is_finite() || scale <= 0.0 {
                    return Err(Error::BadParams("sdf scale overflowed"));
                }
            }
        }
        run_ops(&mut field, &self.ops, nx, ny, nz, self.voxel_size, origin, scale)?;
        for v in field.iter_mut() {
            if !v.is_finite() {
                *v = f32::MAX;
            }
        }
        Ok(field)
    }
}

/// Apply an operation list to a field, in order, one point per voxel.
///
/// Shapes are sampled at `origin + index * voxel_size` divided by `scale`,
/// which is what makes a scale operation dilate the solid.
#[allow(clippy::too_many_arguments)]
fn run_ops(
    field: &mut [f32],
    ops: &[SdfOp],
    nx: u32,
    ny: u32,
    nz: u32,
    vs: f64,
    origin: DVec3,
    scale: f64,
) -> Result<bool> {
    let mut have = false;
    let mut noise: Option<Noise> = None;
    for op in ops {
        match op {
            SdfOp::Add { shape } => {
                have = true;
                eval_shape(field, nx, ny, nz, vs, origin, scale, shape, &[]);
            }
            SdfOp::Union { shape, k } => {
                require(&have)?;
                eval_shape(field, nx, ny, nz, vs, origin, scale, shape, &[Combiner::Union(*k)]);
            }
            SdfOp::Subtract { shape, k } => {
                require(&have)?;
                eval_shape(field, nx, ny, nz, vs, origin, scale, shape, &[Combiner::Subtract(*k)]);
            }
            SdfOp::Intersect { shape, k } => {
                require(&have)?;
                eval_shape(field, nx, ny, nz, vs, origin, scale, shape, &[Combiner::Intersect(*k)]);
            }
            SdfOp::Noise { amplitude, frequency, seed, kind } => {
                require(&have)?;
                let f = if frequency.is_finite() && *frequency > 0.0 { *frequency } else { 1.0 };
                let a = if amplitude.is_finite() { *amplitude } else { 0.0 };
                let field_noise = noise.get_or_insert_with(|| Noise::new(*seed));
                for k in 0..nz {
                    for j in 0..ny {
                        for i in 0..nx {
                            let p = (origin
                                + DVec3::new(i as f64 * vs, j as f64 * vs, k as f64 * vs))
                                / scale;
                            let idx = point_index(nx, ny, nz, i, j, k);
                            let v = kind.sample(field_noise, p.x * f, p.y * f, p.z * f);
                            field[idx] -= (a * v) as f32;
                        }
                    }
                }
            }
            SdfOp::Field { field: slab, k } => {
                require(&have)?;
                if slab.width == 0 || slab.height == 0 || slab.samples.len() != slab.width as usize * slab.height as usize {
                    return Err(Error::SdfTooSmall(slab.width, slab.height, 0));
                }
                if !slab.cell.is_finite() || slab.cell <= 0.0 {
                    return Err(Error::InvalidVoxelSize(slab.cell));
                }
                if !slab.origin.is_finite() {
                    return Err(Error::BadParams("sdf origin must be finite"));
                }
                let comb = [Combiner::Subtract(*k)];
                for kk in 0..nz {
                    for j in 0..ny {
                        for i in 0..nx {
                            let p = (origin
                                + DVec3::new(i as f64 * vs, j as f64 * vs, kk as f64 * vs))
                                / scale;
                            let idx = point_index(nx, ny, nz, i, j, kk);
                            field[idx] = combine(field[idx], slab.distance(p) as f32, &comb);
                        }
                    }
                }
            }
            SdfOp::Shift { .. } | SdfOp::Scale { .. } => {}
        }
    }
    Ok(have)
}

fn require(have: &bool) -> Result<()> {
    if *have {
        Ok(())
    } else {
        Err(Error::BadParams("an sdf operation ran before any solid existed"))
    }
}

/// How a shape is combined with the field that is already there.
#[derive(Clone, Copy)]
enum Combiner {
    Union(f64),
    Subtract(f64),
    Intersect(f64),
}

fn point_index(nx: u32, ny: u32, _nz: u32, i: u32, j: u32, k: u32) -> usize {
    (k * ny + j) as usize * nx as usize + i as usize
}

/// Evaluate `shape` over the grid and combine it with `field` in place.
///
/// Each voxel is sampled at `origin + index * voxel_size` divided by `scale`,
/// which is what makes a scale operation dilate the solid.
#[allow(clippy::too_many_arguments)]
fn eval_shape(
    field: &mut [f32],
    nx: u32,
    ny: u32,
    nz: u32,
    vs: f64,
    origin: DVec3,
    scale: f64,
    shape: &SdfShape,
    comb: &[Combiner],
) {
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let p = (origin + DVec3::new(i as f64 * vs, j as f64 * vs, k as f64 * vs)) / scale;
                let d = shape.distance(p);
                let idx = point_index(nx, ny, nz, i, j, k);
                field[idx] = combine(field[idx], d as f32, comb);
            }
        }
    }
}

fn combine(current: f32, d: f32, comb: &[Combiner]) -> f32 {
    if comb.is_empty() {
        return d;
    }
    match comb[0] {
        Combiner::Union(k) => {
            if k > 0.0 {
                smooth_min(current, d, k as f32)
            } else {
                current.min(d)
            }
        }
        Combiner::Subtract(k) => {
            // Subtract is intersection with the complement, so the blend is
            // the same smooth maximum mirrored.
            let negated = -d;
            if k > 0.0 {
                -smooth_min(-current, negated, k as f32)
            } else {
                current.max(-d)
            }
        }
        Combiner::Intersect(k) => {
            if k > 0.0 {
                smooth_max(current, d, k as f32)
            } else {
                current.max(d)
            }
        }
    }
}

fn smooth_min(a: f32, b: f32, k: f32) -> f32 {
    let h = math::clamp(0.5 + 0.5 * (b - a) as f64 / k as f64, 0.0, 1.0) as f32;
    b * (1.0 - h) + a * h - k * h * (1.0 - h)
}

fn smooth_max(a: f32, b: f32, k: f32) -> f32 {
    -smooth_min(-a, -b, k)
}

/// The largest magnitude a field can hold, which the mesher uses as a sentinel.
pub const FAR: f32 = 1.0e20;

#[cfg(test)]
mod tests {
    #[test]
    fn resolution_covers_the_bounds() {
        let r = VolumeTerrain::resolution_for(DVec3::splat(-256.0), DVec3::new(256.0, 256.0, 128.0), 16.0);
        assert_eq!(r, [33, 33, 25]);
        // A degenerate box still gives a grid validate accepts.
        assert_eq!(VolumeTerrain::resolution_for(DVec3::ZERO, DVec3::ZERO, 16.0), [2, 2, 2]);
        // An enormous box is shrunk to fit the voxel budget.
        let big = VolumeTerrain::resolution_for(DVec3::ZERO, DVec3::splat(1.0e6), 1.0);
        assert!(big.iter().map(|n| *n as u64).product::<u64>() <= MAX_VOXELS);
    }

    use super::*;

    fn sphere_field(res: u32, ops: Vec<SdfOp>) -> Vec<f32> {
        let vt = VolumeTerrain {
            chunks: [res, res, res],
            voxel_size: 1.0,
            origin: DVec3::splat(-(res as f64) * 0.5),
            ops,
        };
        vt.evaluate().unwrap()
    }

    #[test]
    fn sphere_distance_is_exact_along_the_axis() {
        let s = SdfShape::Sphere { center: DVec3::ZERO, radius: 3.0 };
        assert!((s.distance(DVec3::new(3.0, 0.0, 0.0))).abs() < 1e-12);
        assert!((s.distance(DVec3::new(0.0, 0.0, 0.0)) + 3.0).abs() < 1e-12);
        assert!((s.distance(DVec3::new(5.0, 0.0, 0.0)) - 2.0).abs() < 1e-12);
        // A negative distance means inside.
        assert!(s.distance(DVec3::new(0.0, 0.0, 1.0)) < 0.0);
    }

    #[test]
    fn primitives_are_closed_and_bounded() {
        // Each shape with a point strictly inside it, because a solid standing
        // on the origin has its surface, not its interior, at the origin.
        let cases: Vec<(SdfShape, DVec3)> = vec![
            (
                SdfShape::Sphere { center: DVec3::ZERO, radius: 2.0 },
                DVec3::new(0.5, -0.25, 0.75),
            ),
            (
                SdfShape::Capsule { a: DVec3::new(-1.0, 0.0, 0.0), b: DVec3::new(1.0, 0.0, 0.0), radius: 0.5 },
                DVec3::ZERO,
            ),
            (
                SdfShape::Box { center: DVec3::ZERO, half: DVec3::splat(1.0), rotation: [0.0; 3] },
                DVec3::new(0.25, 0.5, -0.75),
            ),
            (
                SdfShape::Cylinder { base: DVec3::ZERO, height: 4.0, radius: 1.0 },
                DVec3::new(0.5, 0.5, 2.0),
            ),
            (
                SdfShape::Cone { base: DVec3::ZERO, height: 4.0, radius: 2.0 },
                DVec3::new(0.5, 0.0, 1.0),
            ),
            (SdfShape::Torus { center: DVec3::ZERO, major: 3.0, minor: 0.5 }, DVec3::new(3.0, 0.0, 0.0)),
            (
                SdfShape::HalfSpace { point: DVec3::ZERO, normal: DVec3::Z },
                DVec3::new(0.0, 0.0, -1.0),
            ),
        ];
        for (sh, inside) in cases {
            assert!(sh.reach().is_finite() && sh.reach() > 0.0, "reach {sh:?}");
            assert!(sh.distance(inside) < 0.0, "{sh:?} is solid at {inside:?}");
            // Far away the distance is positive and finite.
            let far = sh.distance(DVec3::new(1000.0, 1000.0, 1000.0));
            assert!(far.is_finite() && far > 0.0, "{sh:?} far is {far}");
        }
        // A shape that stands on a plane is solid just above it and empty just
        // below, which is what puts the base of a mound at the origin.
        for sh in [
            SdfShape::Cylinder { base: DVec3::ZERO, height: 4.0, radius: 1.0 },
            SdfShape::Cone { base: DVec3::ZERO, height: 4.0, radius: 2.0 },
        ] {
            assert!(sh.distance(DVec3::new(0.0, 0.0, 0.5)) < 0.0, "{sh:?} is hollow above its base");
            assert!(sh.distance(DVec3::new(0.0, 0.0, -0.5)) > 0.0, "{sh:?} is solid below its base");
        }
    }

    #[test]
    fn box_rotation_moves_the_corners() {
        let flat = SdfShape::Box { center: DVec3::ZERO, half: DVec3::new(2.0, 1.0, 0.5), rotation: [0.0; 3] };
        let spun = SdfShape::Box { center: DVec3::ZERO, half: DVec3::new(2.0, 1.0, 0.5), rotation: [0.0, 90.0, 0.0] };
        // A yaw carries the long axis from X to Y.
        assert!(flat.distance(DVec3::new(1.9, 0.0, 0.0)) < 0.0);
        assert!(spun.distance(DVec3::new(1.9, 0.0, 0.0)) > 0.0);
        assert!(spun.distance(DVec3::new(0.0, 1.9, 0.0)) < 0.0);
        // A roll keeps it on X but stands the flat face up.
        let rolled = SdfShape::Box { center: DVec3::ZERO, half: DVec3::new(2.0, 1.0, 0.5), rotation: [0.0, 0.0, 90.0] };
        assert!(rolled.distance(DVec3::new(1.9, 0.0, 0.0)) < 0.0);
        assert!(rolled.distance(DVec3::new(0.0, 0.0, 1.5)) > 0.0);
        // A box stays closed: the far corner lies on the surface, and a point one
        // inch beyond it is one inch out.
        assert!(flat.distance(DVec3::new(2.0, 1.0, 0.5)).abs() < 1e-9);
        assert!((flat.distance(DVec3::new(3.0, 1.0, 0.5)) - 1.0).abs() < 1e-9);
        assert!((flat.distance(DVec3::new(3.0, 2.0, 1.0)) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn evaluate_rejects_bad_fields() {
        let ok = VolumeTerrain {
            chunks: [4, 4, 4],
            voxel_size: 1.0,
            origin: DVec3::ZERO,
            ops: vec![SdfOp::Add {
                shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 1.0 },
            }],
        };
        assert!(ok.validate().is_ok());
        assert_eq!(ok.evaluate().unwrap().len(), 64);

        let tiny = VolumeTerrain { chunks: [1, 4, 4], ..ok.clone() };
        assert!(matches!(tiny.validate(), Err(Error::SdfTooSmall(..))));
        let bad_voxel = VolumeTerrain { voxel_size: 0.0, ..ok.clone() };
        assert!(matches!(bad_voxel.validate(), Err(Error::InvalidVoxelSize(_))));
        let empty = VolumeTerrain { ops: Vec::new(), ..ok.clone() };
        assert!(empty.validate().is_err());
        let no_add = VolumeTerrain {
            ops: vec![SdfOp::Scale { factor: 2.0 }],
            ..ok.clone()
        };
        assert!(no_add.validate().is_err());
        let huge = VolumeTerrain { chunks: [4096, 4096, 4096], ..ok.clone() };
        assert!(matches!(huge.validate(), Err(Error::SdfTooLarge(..))));
        let bad_origin = VolumeTerrain { origin: DVec3::splat(f64::NAN), ..ok.clone() };
        assert!(bad_origin.validate().is_err());
    }

    #[test]
    fn combinators_behave() {
        let big = SdfOp::Add {
            shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 8.0 },
        };
        let hole = SdfOp::Subtract {
            shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 2.0 },
            k: 0.0,
        };
        let plain = sphere_field(16, vec![big.clone()]);
        let drilled = sphere_field(16, vec![big.clone(), hole.clone()]);
        let (nx, ny, nz) = (16u32, 16u32, 16u32);
        let mid = point_index(nx, ny, nz, 8, 8, 8);
        assert!(plain[mid] < 0.0, "the big sphere should be solid at the centre");
        assert!(drilled[mid] > 0.0, "the hole should be empty at the centre");

        // A smoothed subtraction leaves a fillet rather than a flat lip.
        let soft = sphere_field(16, vec![
            big.clone(),
            SdfOp::Subtract { shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 2.0 }, k: 2.0 },
        ]);
        assert!(soft.iter().all(|v| v.is_finite()));
        assert_ne!(soft, drilled);

        // A union of two overlapping spheres is bigger than either.
        let one = sphere_field(16, vec![SdfOp::Add {
            shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 3.0 },
        }]);
        let two = sphere_field(16, vec![
            SdfOp::Add { shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 3.0 } },
            SdfOp::Union {
                shape: SdfShape::Sphere { center: DVec3::new(2.0, 0.0, 0.0), radius: 3.0 },
                k: 0.0,
            },
        ]);
        let solid_one = one.iter().filter(|v| **v < 0.0).count();
        let solid_two = two.iter().filter(|v| **v < 0.0).count();
        assert!(solid_two > solid_one, "{solid_two} vs {solid_one}");

        // An intersection keeps only the overlap.
        let clipped = sphere_field(16, vec![
            big.clone(),
            SdfOp::Intersect {
                shape: SdfShape::Box {
                    center: DVec3::ZERO,
                    half: DVec3::splat(3.0),
                    rotation: [0.0; 3],
                },
                k: 0.0,
            },
        ]);
        assert!(clipped[point_index(nx, ny, nz, 12, 8, 8)] > 0.0);
        assert!(clipped[mid] < 0.0);
    }

    #[test]
    fn noise_displacement_changes_the_surface_but_keeps_it_closed() {
        let plain = sphere_field(24, vec![SdfOp::Add {
            shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 6.0 },
        }]);
        // Gradient noise is zero on the integer lattice, and an unscaled field
        // samples nothing but lattice points, so the displacement needs a
        // frequency that moves the sample off it.
        let bumpy = sphere_field(24, vec![
                SdfOp::Add { shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 6.0 } },
                SdfOp::Noise { amplitude: 1.5, frequency: 0.37, seed: 5, kind: NoiseKind3::Perlin },
            ],
        );
        assert_ne!(plain, bumpy);
        assert!(bumpy.iter().all(|v| v.is_finite()));
        // Both spheres enclose the centre.
        let (nx, ny, nz) = (24u32, 24u32, 24u32);
        assert!(bumpy[point_index(nx, ny, nz, 12, 12, 12)] < 0.0);
    }

    #[test]
    fn scale_dilates_the_solid() {
        let a = sphere_field(16, vec![SdfOp::Add {
            shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 4.0 },
        }]);
        let b = sphere_field(16, vec![
            SdfOp::Add { shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 4.0 } },
            SdfOp::Scale { factor: 2.0 },
        ]);
        let solid_a = a.iter().filter(|v| **v < 0.0).count();
        let solid_b = b.iter().filter(|v| **v < 0.0).count();
        assert!(solid_b > solid_a * 2, "{solid_b} vs {solid_a}");
    }

    #[test]
    fn shift_moves_the_solid_by_the_offset() {
        let solid = SdfOp::Add { shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 3.0 } };
        let a = sphere_field(16, vec![solid.clone()]);
        let b = sphere_field(16, vec![solid.clone(), SdfOp::Shift { offset: DVec3::new(2.0, 0.0, 0.0) }],
        );
        // Sample 6 is two inches along X of the low corner, so it is inside the
        // sphere before the shift and two inches outside its far side after.
        assert!(a[point_index(16, 16, 16, 6, 8, 8)] < 0.0);
        assert!(b[point_index(16, 16, 16, 6, 8, 8)] > 0.0);
        assert!(b[point_index(16, 16, 16, 8, 8, 8)] < 0.0);
        // A non-finite offset is refused.
        let bad = VolumeTerrain {
            chunks: [8, 8, 8],
            voxel_size: 1.0,
            origin: DVec3::ZERO,
            ops: vec![solid.clone(), SdfOp::Shift { offset: DVec3::splat(f64::NAN) }],
        };
        assert!(matches!(bad.evaluate(), Err(Error::BadParams(_))));
    }

    #[test]
    fn scale_and_bad_scale() {
        let solid = SdfOp::Add { shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 3.0 } };
        let bad = VolumeTerrain {
            chunks: [8, 8, 8],
            voxel_size: 1.0,
            origin: DVec3::ZERO,
            ops: vec![solid.clone(), SdfOp::Scale { factor: 0.0 }],
        };
        assert!(matches!(bad.evaluate(), Err(Error::BadParams(_))));
        let nan = VolumeTerrain {
            chunks: [8, 8, 8],
            voxel_size: 1.0,
            origin: DVec3::ZERO,
            ops: vec![solid.clone(), SdfOp::Scale { factor: f64::NAN }],
        };
        assert!(nan.evaluate().is_err());
    }

    #[test]
    fn operations_before_a_solid_are_rejected() {
        let vt = VolumeTerrain {
            chunks: [8, 8, 8],
            voxel_size: 1.0,
            origin: DVec3::ZERO,
            ops: vec![SdfOp::Scale { factor: 1.0 }],
        };
        assert!(vt.evaluate().is_err());
    }

    #[test]
    fn evaluate_is_deterministic() {
        let vt = VolumeTerrain {
            chunks: [12, 10, 8],
            voxel_size: 2.0,
            origin: DVec3::new(-8.0, 1.0, 3.0),
            ops: vec![
                SdfOp::Add { shape: SdfShape::Box { center: DVec3::ZERO, half: DVec3::splat(9.0), rotation: [10.0, 20.0, 30.0] } },
                SdfOp::Subtract {
                    shape: SdfShape::Capsule {
                        a: DVec3::new(-9.0, 0.0, 0.0),
                        b: DVec3::new(9.0, 0.0, 0.0),
                        radius: 2.0,
                    },
                    k: 1.0,
                },
                SdfOp::Noise { amplitude: 0.8, frequency: 0.1, seed: 3, kind: NoiseKind3::Symmetric },
            ],
        };
        let a = vt.evaluate().unwrap();
        let b = vt.evaluate().unwrap();
        assert_eq!(a, b);
        assert_eq!(vt.chunk_grid(), [2, 2, 1]);
        assert_eq!(vt.voxel_origin(1, 2, 3), DVec3::new(-6.0, 5.0, 9.0));
        assert_eq!(vt.voxel_count(), 12 * 10 * 8);
    }

    #[test]
    fn noise_kinds_all_produce_finite_values() {
        let n = Noise::new(1);
        for k in [NoiseKind3::Perlin, NoiseKind3::Simplex, NoiseKind3::Value, NoiseKind3::Symmetric] {
            let mut peak = 0.0f64;
            for i in 0..200 {
                let v = k.sample(&n, i as f64 * 0.31, 0.5, -0.25);
                assert!(v.is_finite() && v.abs() <= 1.01, "{k:?} gave {v}");
                peak = peak.max(v.abs());
            }
            assert!(peak > 0.05, "{k:?} never varied");
        }
    }

    #[test]
    fn point_index_is_x_fastest() {
        assert_eq!(point_index(4, 4, 4, 0, 0, 0), 0);
        assert_eq!(point_index(4, 4, 4, 1, 0, 0), 1);
        assert_eq!(point_index(4, 4, 4, 0, 1, 0), 4);
        assert_eq!(point_index(4, 4, 4, 0, 0, 1), 16);
    }

    #[test]
    fn a_heightfield_slab_cuts_a_cave() {
        // A solid block with a heightfield slab carved out of it: the slab is
        // 4 units thick under its surface, so the middle of the block turns
        // hollow and the top and bottom stay solid.
        let res = 24u32;
        let hf = Heightfield::from_fn(9, 9, 4.0, |x, y| {
            4.0f32 + (x + y) as f32 * 0.25
        })
        .unwrap();
        let slab = VolumeField::from_heightfield(&hf, DVec3::new(-16.0, -16.0, 0.0), 4.0, 4.0).unwrap();
        assert_eq!((slab.width, slab.height), (9, 9));
        assert!((slab.max_height() - 8.0).abs() < 1e-6, "max height {}", slab.max_height());
        let (lo, hi) = slab.bounds(0.0);
        assert_eq!(lo, DVec3::new(-16.0, -16.0, 0.0));
        assert_eq!(hi, DVec3::new(16.0, 16.0, 8.0));

        let vt = VolumeTerrain {
            chunks: [res, res, res],
            voxel_size: 1.0,
            origin: DVec3::splat(-(res as f64) * 0.5),
            ops: vec![
                SdfOp::Add {
                    shape: SdfShape::Box {
                        center: DVec3::ZERO,
                        half: DVec3::splat(res as f64 * 0.4),
                        rotation: [0.0; 3],
                    },
                },
                SdfOp::Field { field: slab, k: 0.0 },
            ],
        };
        let field = vt.evaluate().unwrap();
        let origin = DVec3::splat(-(res as f64) * 0.5);
        // The field is sampled on the voxel lattice, so a world point reads the
        // voxel at or just below it.
        let at = |p: DVec3| {
            let i = ((p.x - origin.x).round() as u32).min(res - 1);
            let j = ((p.y - origin.y).round() as u32).min(res - 1);
            let k = ((p.z - origin.z).round() as u32).min(res - 1);
            field[point_index(res, res, res, i, j, k)]
        };
        // Inside the block and above the slab's surface, so the roof survives.
        assert!(at(DVec3::new(4.0, 4.0, 9.0)) < 0.0, "the roof was cut away");
        // Inside the block and inside the slab, which is now hollow.
        assert!(at(DVec3::new(4.0, 4.0, 4.0)) > 0.0, "the cave was not dug");
        // Below the slab, so the floor survives.
        assert!(at(DVec3::new(4.0, 4.0, 1.0)) < 0.0, "the floor was cut away");
        // Outside the block entirely.
        assert!(at(DVec3::new(11.0, 0.0, 0.0)) > 0.0);
        // And the mesh of it is still a closed surface.
        let m = crate::mesher::mesh(&vt).unwrap();
        let counts = m.edge_use_counts();
        assert!(!counts.is_empty());
        assert_eq!(counts.values().filter(|v| **v < 2).count(), 0, "the cave left an open edge");
    }

    #[test]
    fn a_heightfield_slab_validates_its_shape() {
        let hf = Heightfield::filled(4, 4, 8.0, 10.0).unwrap();
        assert!(matches!(
            VolumeField::from_heightfield(&hf, DVec3::ZERO, 0.0, 4.0),
            Err(Error::InvalidVoxelSize(_))
        ));
        assert!(matches!(
            VolumeField::from_heightfield(&hf, DVec3::ZERO, 8.0, 0.0),
            Err(Error::BadParams(_))
        ));
        assert!(matches!(
            VolumeField::from_heightfield(&hf, DVec3::splat(f64::NAN), 8.0, 4.0),
            Err(Error::BadParams(_))
        ));
        let empty = Heightfield { width: 2, height: 2, cell_size: 1.0, data: Vec::new() };
        assert!(matches!(VolumeField::from_heightfield(&empty, DVec3::ZERO, 1.0, 1.0), Err(Error::ZeroSize)));

        // A field whose sample count does not match its extent is refused.
        let mut slab = VolumeField::from_heightfield(&hf, DVec3::ZERO, 8.0, 4.0).unwrap();
        slab.samples.pop();
        let vt = VolumeTerrain {
            chunks: [8, 8, 8],
            voxel_size: 1.0,
            origin: DVec3::splat(-4.0),
            ops: vec![
                SdfOp::Add { shape: SdfShape::Sphere { center: DVec3::ZERO, radius: 2.0 } },
                SdfOp::Field { field: slab, k: 0.0 },
            ],
        };
        assert!(matches!(vt.evaluate(), Err(Error::SdfTooSmall(..))));
    }

    #[test]
    fn smooth_combinators_stay_within_their_inputs_bounds() {
        for k in [0.0f32, 0.5, 2.0] {
            for ai in -8..=8i32 {
                for bi in -8..=8i32 {
                    let (a, b) = (ai as f32 * 0.5, bi as f32 * 0.5);
                    let mn = smooth_min(a, b, k.max(1e-6));
                    let mx = smooth_max(a, b, k.max(1e-6));
                    assert!(mn.is_finite() && mx.is_finite());
                    assert!(mn <= a.min(b) + k.max(1e-6), "smooth_min {mn} above the inputs");
                    assert!(mx >= a.max(b) - k.max(1e-6), "smooth_max {mx} below the inputs");
                }
            }
        }
    }
}
