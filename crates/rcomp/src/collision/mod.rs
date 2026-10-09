//! Collision: the BVH4 writer, the decoder and the standalone `*coll` model
//! a brush entity carries.

pub mod tf2;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use glam::{DVec3, Vec3};
use rbsp::lumps::collision::{
    BvhLeafData, BvhNode, ContentMask, Model, PackVert, PolyTriangle, SurfaceProp, CHILD_BUNDLE,
    CHILD_EMPTY, CHILD_HULL, CHILD_NODE, CHILD_POLY, CHILD_PROP, EDGE_FLAGS_ALL, MAX_POLYS_PER_LEAF, NODE_SIZE,
};
use rbsp::{ids, Bsp, Result as BspResult, Version};
use rir::{CollisionShape, CollisionSolid, ModelSlot};
use thiserror::Error;

/// Bytes one `*collN` key holds, base64 encoded.
pub const COLL_CHUNK: usize = 0x78;
/// Contents index ceiling.
pub const MAX_CONTENTS: usize = 0xFF;
/// Surface property index ceiling.
pub const MAX_SURFPROPS: usize = 0xFFF;
/// Decoded node quantisation, i16 in units of 1/65536 of a model unit.
pub const DECODE_SCALE_UNIT: f64 = 65536.0;
/// Contents index 0 is the empty mask.
pub const CONTENTS_EMPTY: u32 = 0;
/// The mask shipped maps put on ordinary world geometry (96724 of
/// staging_mu1's 108924 world leaves): opaque, physics clip, sound occlusion,
/// player, monster, titan and operator clip, and line of sight. World
/// collision never carries the bare solid bit.
pub const CONTENTS_WORLD: u32 = 0x00EB_1280;

/// The mask a document's contents value writes. 0 and the bare solid bit both
/// mean ordinary world geometry; anything else is an explicit engine mask.
pub fn world_contents(contents: u32) -> u32 {
    match contents {
        0 | 1 => CONTENTS_WORLD,
        other => other,
    }
}
/// Two triangle corners closer than this are the same point.
pub const CORNER_EPS: f64 = 1.0e-6;
/// The parallelogram polygon leaf: same entry layout as [`CHILD_POLY`] with
/// bit 1 set, so one entry yields two triangles.
pub const CHILD_POLY_QUAD: u32 = 6;
/// Highest child type the leaf dispatch treats as a polygon leaf.
pub const CHILD_POLY_MAX: u32 = 7;

#[derive(Debug, Error)]
pub enum CollError {
    #[error("solid {0} is not a closed convex volume")]
    NotASolid(usize),
    #[error("solid {0} has no vertices")]
    Empty(usize),
    #[error("a hull of {0} vertices needs {1} planes or triangle sets, more than a hull leaf can hold")]
    HullTooLarge(usize, usize),
    #[error("a hull quantised to {0}, past the i16 grid")]
    HullQuant(usize),
    #[error("a poly leaf needs a vertex delta of ({0},{1},{2})")]
    Delta(i64, i64, i64),
    #[error("more than {0} distinct contents masks")]
    TooManyContents(usize),
    #[error("more than {0} surface properties")]
    TooManySurfProps(usize),
    #[error("a bundle entry is {0} dwords, more than the field holds")]
    BundleTooBig(usize),
    #[error("a leaf offset is {0}, past the 24-bit field")]
    LeafOffset(usize),
    #[error("lump 0x{id:02x} of the map read is {reason}")]
    Lump { id: u16, reason: String },
    #[error("a model {0} needs a decode step of {1}, past the i16 grid")]
    DecodeStep(usize, f64),
    #[error("a hull of {0} points is not a volume")]
    HullEmpty(usize),
    #[error("collision of {what}: {source}")]
    Solid { what: String, source: Box<CollError> },
    #[error("a `*coll` blob is {0} bytes, too short for a header")]
    CollShort(usize),
    #[error("a `*coll` blob section offset {0} is past its {1} bytes")]
    CollRange(u32, usize),
    #[error("a `*coll` blob is truncated at {0}")]
    CollTruncated(usize),
    #[error("a `*coll` key is not base64")]
    CollBase64,
    #[error("a poly leaf has no triangle with three distinct corners")]
    NoTriangles,
}

fn version_of(bsp: &Bsp) -> Version {
    bsp.version_kind().unwrap_or(Version::ClientV51)
}

/// Whether a model carries a collision tree. The node field alone is not
/// enough: a shipped map marks a model with no geometry by an inverted bounds
/// box and leaves its node field at zero, so without the bounds test every
/// such model would re-walk model zero's tree.
pub fn model_has_collision(m: &Model) -> bool {
    m.bvh_node >= 0 && m.mins[0] <= m.maxs[0]
}

fn lump_err(id: u16, e: impl std::fmt::Display) -> CollError {
    CollError::Lump { id, reason: e.to_string() }
}

// ---- convex hull ------------------------------------------------------------

/// A triangular face of a hull, as indices into the point array.
pub type Face3 = [usize; 3];

/// Triangular hull of a point cloud, with every face wound outward.
#[derive(Clone, Debug, PartialEq)]
pub struct Hull {
    /// Indices of the points the hull uses, ascending.
    pub verts: Vec<usize>,
    pub faces: Vec<Face3>,
}

/// A failing solid named the way a map author finds it: its object and shape.
fn describe_solid(index: usize, s: &CollisionSolid) -> String {
    let shape = match &s.shape {
        CollisionShape::Brush { planes } => format!("brush of {} planes", planes.len()),
        CollisionShape::ConvexHull { points } => format!("hull of {} points", points.len()),
        CollisionShape::Triangles { indices, .. } => format!("{} triangles", indices.len() / 3),
        _ => "shape".to_string(),
    };
    match s.object {
        Some(id) => format!("solid {index} ({shape}) of object {id}"),
        None => format!("solid {index} ({shape}), generated"),
    }
}

/// True when `faces` close one convex surface over `verts` points: at most
/// 2V-4 triangles and every directed edge matched by its reverse exactly once.
fn hull_is_closed(verts: usize, faces: &[Face3]) -> bool {
    if verts < 4 || faces.len() > 2 * verts - 4 {
        return false;
    }
    let mut edges: HashMap<(usize, usize), i32> = HashMap::new();
    for f in faces {
        for (a, b) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
            *edges.entry((a, b)).or_default() += 1;
        }
    }
    edges.iter().all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1))
}

/// The hull of points already snapped to a grid of `step`. Snapping leaves
/// faces that were planar a fraction of a step out of plane; an absolute
/// epsilon then flips visibility tests and the horizon splits, so the
/// tolerance follows the grid and a hull that does not close is retried looser.
fn grid_hull(points: &[DVec3], step: f64) -> Result<Hull, CollError> {
    let mut last = None;
    for eps in [1e-9, step * 0.01, step * 0.25, step] {
        match convex_hull(points, eps) {
            Ok(h) if hull_is_closed(h.verts.len(), &h.faces) => return Ok(h),
            Ok(_) => last = None,
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or(CollError::HullEmpty(points.len())))
}

/// Convex hull of `points`, computed incrementally. `eps` is how far outside a
/// face a point must be to be outside the hull.
pub fn convex_hull(points: &[DVec3], eps: f64) -> Result<Hull, CollError> {
    let n = points.len();
    if n < 4 {
        return Err(CollError::HullEmpty(n));
    }
    let mut base: Option<[usize; 4]> = None;
    'outer: for a in 0..n {
        for b in a + 1..n {
            for c in b + 1..n {
                let nrm = (points[b] - points[a]).cross(points[c] - points[a]);
                let l = nrm.length();
                if l <= eps * eps.max(1.0) {
                    continue;
                }
                let nrm = nrm / l;
                // A triple whose plane leaves a point strictly off it is
                // non-degenerate, and that point completes a tetrahedron.
                for (i, p) in points.iter().enumerate() {
                    if i == a || i == b || i == c {
                        continue;
                    }
                    if nrm.dot(*p - points[a]).abs() > eps {
                        base = Some([a, b, c, i]);
                        break 'outer;
                    }
                }
            }
        }
    }
    let Some([a, b, c, d]) = base else {
        // Flat point set: no volume, so no hull.
        return Err(CollError::HullEmpty(n));
    };
    let normal = |i: usize, j: usize, k: usize| {
        let n = (points[j] - points[i]).cross(points[k] - points[i]);
        let l = n.length();
        if l <= 0.0 {
            None
        } else {
            Some(n / l)
        }
    };
    let outward = |i: usize, j: usize, k: usize, inside: usize| {
        normal(i, j, k).is_some_and(|n| n.dot(points[inside] - points[i]) <= 0.0)
    };
    let mut faces: Vec<Face3> = Vec::new();
    for (i, j, k, in4) in [(a, b, c, d), (a, b, d, c), (a, c, d, b), (b, c, d, a)] {
        if outward(i, j, k, in4) {
            faces.push([i, j, k]);
        } else {
            faces.push([i, k, j]);
        }
    }
    let rest: Vec<usize> = (0..n).filter(|&i| i != a && i != b && i != c && i != d).collect();
    for p in rest {
        let visible: Vec<Face3> = faces
            .iter()
            .copied()
            .filter(|f| normal(f[0], f[1], f[2]).is_some_and(|n| n.dot(points[p] - points[f[0]]) > eps))
            .collect();
        if visible.is_empty() {
            continue;
        }
        // Every face in view means the start tetrahedron was too thin to
        // orient against; the set has no usable volume.
        if visible.len() == faces.len() {
            return Err(CollError::HullEmpty(n));
        }
        // The counts are keyed by edge, but the horizon that comes out of them
        // is walked in first-insertion order: a hashed map would make the hull's
        // face order, and with it the whole leaf stream, differ run to run.
        let mut local: HashMap<(usize, usize), i32> = HashMap::new();
        let mut order: Vec<(usize, usize)> = Vec::new();
        for f in &visible {
            for e in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                for key in [e, (e.1, e.0)] {
                    match local.get_mut(&key) {
                        Some(c) => *c += if key == e { 1 } else { -1 },
                        None => {
                            local.insert(key, if key == e { 1 } else { -1 });
                            order.push(key);
                        }
                    }
                }
            }
        }
        let horizon: Vec<(usize, usize)> =
            order.into_iter().filter(|e| local.get(e).copied().unwrap_or(0) > 0).collect();
        faces.retain(|f| !visible.contains(f));
        // A new face keeps its horizon edge in the direction the removed face
        // had it, which is outward by construction; testing against a hull
        // vertex instead flips faces whose plane that vertex lies on.
        for (i, j) in horizon {
            faces.push([i, j, p]);
        }
    }
    let mut used: Vec<usize> = faces.iter().flat_map(|f| f.iter().copied()).collect();
    used.sort_unstable();
    used.dedup();
    let remap: HashMap<usize, usize> = used.iter().enumerate().map(|(i, &o)| (o, i)).collect();
    let faces = faces
        .into_iter()
        .map(|f| [remap[&f[0]], remap[&f[1]], remap[&f[2]]])
        .collect();
    Ok(Hull { verts: used, faces })
}

// ---- the writer -------------------------------------------------------------

/// What a tree item carries.
#[derive(Clone, Debug)]
enum ItemData {
    /// Float-vertex triangles, counter-clockwise seen from outside.
    Poly { surfprop: u16, tris: Vec<[DVec3; 3]> },
    Hull(Box<HullItem>),
    Prop(u32),
}

#[derive(Clone, Debug)]
struct HullItem {
    origin: [f32; 3],
    step: f64,
    q: Vec<[i64; 3]>,
    /// One corner triple per hull face, wound outward.
    planes: Vec<[u8; 3]>,
    /// Surface property per face, and the face corners that draw with it.
    sets: Vec<(u16, Vec<[u8; 3]>)>,
}

/// One entry of a model's tree: a poly run, a convex hull or a prop reference,
/// with the contents index that gates it.
#[derive(Clone, Debug)]
pub struct Item {
    kind: u32,
    contents: u8,
    mins: DVec3,
    maxs: DVec3,
    data: ItemData,
}

impl Item {
    pub fn kind(&self) -> u32 {
        self.kind
    }

    pub fn contents(&self) -> u8 {
        self.contents
    }

    pub fn bounds(&self) -> (DVec3, DVec3) {
        (self.mins, self.maxs)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModelFields {
    pub bvh_node: i32,
    pub bvh_leaf: i32,
    pub vert_index: i32,
    pub flags: u32,
    pub origin: [f32; 3],
    pub decode_scale: f32,
}

/// Accumulates every model's tree into shared lump arrays.
#[derive(Default)]
pub struct BvhWriter {
    pub nodes: Vec<u8>,
    pub leaf: Vec<u32>,
    pub verts: Vec<[f32; 3]>,
    pub contents_masks: Vec<u32>,
    pub surfprops: Vec<(u16, u8, u8, u32)>,
    surfprop_index: HashMap<(u16, u8, u8, u32), u16>,
    contents_index: HashMap<u32, u8>,
    model_vert_base: usize,
    origin: DVec3,
    step: f64,
    arena: Vec<Node>,
}

impl BvhWriter {
    /// `contents_masks` seeds the contents table, so a converter can keep the
    /// source map's ordering.
    pub fn new(contents_masks: Vec<u32>) -> Self {
        let mut index = HashMap::new();
        for (i, m) in contents_masks.iter().enumerate() {
            index.insert(*m, i as u8);
        }
        Self { contents_masks, contents_index: index, ..BvhWriter::default() }
    }

    pub fn contents_index(&mut self, mask: u32) -> Result<u8, CollError> {
        if let Some(i) = self.contents_index.get(&mask) {
            return Ok(*i);
        }
        if self.contents_masks.len() > MAX_CONTENTS {
            return Err(CollError::TooManyContents(MAX_CONTENTS));
        }
        let i = self.contents_masks.len() as u8;
        self.contents_masks.push(mask);
        self.contents_index.insert(mask, i);
        Ok(i)
    }

    pub fn surfprop_index(&mut self, flags: u16, surf_type: u8, contents: u8, name_ofs: u32) -> Result<u16, CollError> {
        let key = (flags, surf_type, contents, name_ofs);
        if let Some(i) = self.surfprop_index.get(&key) {
            return Ok(*i);
        }
        if self.surfprops.len() > MAX_SURFPROPS {
            return Err(CollError::TooManySurfProps(MAX_SURFPROPS));
        }
        let i = self.surfprops.len() as u16;
        self.surfprops.push(key);
        self.surfprop_index.insert(key, i);
        Ok(i)
    }

    /// Float-vertex triangle item, counter-clockwise seen from outside.
    pub fn tri_item(&mut self, contents: u8, surfprop: u16, tris: Vec<[DVec3; 3]>) -> Item {
        let mut mins = DVec3::splat(f64::INFINITY);
        let mut maxs = DVec3::splat(f64::NEG_INFINITY);
        for t in &tris {
            for p in t {
                mins = mins.min(*p);
                maxs = maxs.max(*p);
            }
        }
        Item { kind: CHILD_POLY, contents, mins, maxs, data: ItemData::Poly { surfprop, tris } }
    }

    /// Static prop reference.
    pub fn prop_item(&mut self, contents: u8, prop_index: u32, mins: DVec3, maxs: DVec3) -> Item {
        Item { kind: CHILD_PROP, contents, mins, maxs, data: ItemData::Prop(prop_index) }
    }

    /// Convex volume from its exact corner points, quantised to its own grid
    /// and rebuilt as the hull of the quantised points, so the faces, plane
    /// triples and winding are all consistent with what the engine decodes.
    /// Each face takes the surface of the source side whose normal it matches
    /// best.
    pub fn hull_item(
        &mut self,
        contents: u8,
        points: &[DVec3],
        side_normals: &[DVec3],
        side_surfprops: &[u16],
    ) -> Result<Item, CollError> {
        if points.len() < 4 || side_normals.is_empty() || side_normals.len() != side_surfprops.len() {
            return Err(CollError::HullEmpty(points.len()));
        }
        let mut mn = DVec3::splat(f64::INFINITY);
        let mut mx = DVec3::splat(f64::NEG_INFINITY);
        for p in points {
            mn = mn.min(*p);
            mx = mx.max(*p);
        }
        let origin = DVec3::new(
            (((mn.x + mx.x) * 0.5) as f32) as f64,
            (((mn.y + mx.y) * 0.5) as f32) as f64,
            (((mn.z + mx.z) * 0.5) as f32) as f64,
        );
        let span = points.iter().fold(0.0f64, |a, p| a.max((*p - origin).abs().max_element()));
        let step = 2.0f64.powf((span.max(1e-6) / 32767.0).log2().ceil());
        let mut q: Vec<[i64; 3]> = Vec::with_capacity(points.len());
        for p in points {
            let c = [
                ((p.x - origin.x) / step).round() as i64,
                ((p.y - origin.y) / step).round() as i64,
                ((p.z - origin.z) / step).round() as i64,
            ];
            if !q.contains(&c) {
                q.push(c);
            }
        }
        q.sort_unstable();
        let world: Vec<DVec3> =
            q.iter().map(|c| origin + DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64) * step).collect();
        let hull = grid_hull(&world, step)?;
        let keep = hull.verts.clone();
        let world: Vec<DVec3> = keep.iter().map(|&i| world[i]).collect();
        let q: Vec<[i64; 3]> = keep.iter().map(|&i| q[i]).collect();
        // Group the hull's triangles by their plane, the way the reference
        // writer does, so one face gets one corner triple. A face's normal is
        // the sum of its triangles', which is the best fit to the plane the
        // face lies in and what decides which source side it takes its surface
        // from.
        let mut groups: Vec<(DVec3, f64, Vec<[usize; 3]>, DVec3)> = Vec::new();
        for f in &hull.faces {
            let (a, b, c) = (f[0], f[1], f[2]);
            if a == b || b == c || a == c {
                continue;
            }
            let mut n = (world[b] - world[a]).cross(world[c] - world[a]);
            if n.length_squared() <= 0.0 {
                continue;
            }
            n = n.normalize();
            let key = (n, n.dot(world[a]));
            match groups.iter_mut().find(|(gn, gd, _, _)| {
                (*gn - key.0).abs().max_element() <= 1e-5 && (*gd - key.1).abs() <= 1e-3
            }) {
                Some(g) => {
                    g.2.push([a, b, c]);
                    g.3 += n;
                }
                None => groups.push((key.0, key.1, vec![[a, b, c]], n)),
            }
        }
        let mut planes: Vec<[u8; 3]> = Vec::with_capacity(groups.len());
        let mut sets: Vec<(u16, Vec<[u8; 3]>)> = Vec::new();
        for (_, _, tris, sum) in groups {
            // The widest triangle of a face is the one the engine needs.
            let best =
                first_max_by(tris.iter().copied(), |&t| tri_area(&world, t)).unwrap_or([0, 0, 0]);
            let n = if sum.length_squared() > 0.0 { sum.normalize() } else { DVec3::Z };
            let dots: Vec<f64> = side_normals.iter().map(|s| s.dot(n)).collect();
            let side = first_max(&dots).unwrap_or(0);
            let sp = side_surfprops[side];
            planes.push([best[0] as u8, best[1] as u8, best[2] as u8]);
            // The plane triples are one byte per corner, so a hull beyond 255
            // corners cannot be written and its faces are dropped.
            let usable: Vec<[u8; 3]> =
                tris.iter().filter(|t| t[0] < 0xFF && t[1] < 0xFF && t[2] < 0xFF)
                    .map(|t| [t[0] as u8, t[1] as u8, t[2] as u8])
                    .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
                    .collect();
            if usable.is_empty() {
                continue;
            }
            match sets.iter_mut().find(|(s, _)| *s == sp) {
                Some(e) => e.1.extend(usable),
                None => sets.push((sp, usable)),
            }
        }
        if planes.len() > 0xFF {
            return Err(CollError::HullTooLarge(q.len(), planes.len()));
        }
        if q.iter().flat_map(|c| c.iter()).any(|v| v.abs() > 32767) {
            return Err(CollError::HullQuant(q.len()));
        }
        let mut mins = DVec3::splat(f64::INFINITY);
        let mut maxs = DVec3::splat(f64::NEG_INFINITY);
        for p in &world {
            mins = mins.min(*p);
            maxs = maxs.max(*p);
        }
        let item = HullItem { origin: [origin.x as f32, origin.y as f32, origin.z as f32], step, q, planes, sets };
        Ok(Item { kind: CHILD_HULL, contents, mins, maxs, data: ItemData::Hull(Box::new(item)) })
    }

    fn alloc_verts(&mut self, pts: &[DVec3]) -> Vec<u32> {
        let mut local: HashMap<[u32; 3], u32> = HashMap::new();
        let mut out = Vec::with_capacity(pts.len());
        for p in pts {
            let key = [pool_key(p.x), pool_key(p.y), pool_key(p.z)];
            let i = match local.get(&key) {
                Some(i) => *i,
                None => {
                    let i = self.verts.len() as u32;
                    self.verts.push([p.x as f32, p.y as f32, p.z as f32]);
                    local.insert(key, i);
                    i
                }
            };
            out.push(i);
        }
        out
    }

    /// Packs one poly leaf; each triangle is rotated so its smallest index
    /// leads, which is what keeps the deltas inside their bit fields.
    fn encode_polys(&self, surfprop: u16, tris: &[[u32; 3]], vert_base: u32) -> Result<Vec<u32>, CollError> {
        let mut rot: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
        for t in tris {
            // A triangle that repeats a vertex bounds no area and cannot be
            // packed as a pair of deltas.
            if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                continue;
            }
            let [mut a, mut b, mut c] = *t;
            let m = a.min(b).min(c);
            if m == b {
                let t2 = (a, b, c);
                a = t2.1;
                b = t2.2;
                c = t2.0;
            } else if m == c {
                let t2 = (a, b, c);
                a = t2.2;
                b = t2.0;
                c = t2.1;
            }
            rot.push([a, b, c]);
        }
        rot.sort_by_key(|t| t[0]);
        if rot.is_empty() {
            return Err(CollError::NoTriangles);
        }
        let base = (rot[0][0] as i64 - vert_base as i64) >> 10;
        if !(0..=0xFFFF).contains(&base) {
            return Err(CollError::Delta(base, 0, 0));
        }
        let mut out = vec![(((rot.len() - 1) as u32) << 12) | ((surfprop as u32) & 0xFFF) | ((base as u32) << 16)];
        let mut running = vert_base as i64 + (base << 10);
        for t in rot {
            let d0 = t[0] as i64 - running;
            let d1 = t[1] as i64 - t[0] as i64 - 1;
            let d2 = t[2] as i64 - t[0] as i64 - 1;
            if !(0..=0x7FF).contains(&d0) || !(0..=0x1FF).contains(&d1) || !(0..=0x1FF).contains(&d2) {
                return Err(CollError::Delta(d0, d1, d2));
            }
            out.push(
                (d0 as u32) | ((d1 as u32) << 11) | ((d2 as u32) << 20) | (EDGE_FLAGS_ALL << 29),
            );
            running = t[0] as i64;
        }
        Ok(out)
    }

    fn encode_tri_leaves(&mut self, surfprop: u16, tris: &[[DVec3; 3]]) -> Result<Vec<Vec<u32>>, CollError> {
        let mut out = Vec::new();
        for chunk in tris.chunks(MAX_POLYS_PER_LEAF) {
            let mut pts: Vec<DVec3> = Vec::with_capacity(chunk.len() * 3);
            for t in chunk {
                pts.extend_from_slice(t);
            }
            let idx = self.alloc_verts(&pts);
            // The pool dedups by f32 bits, so two corners a quantum apart end up
            // on one index; such a triangle bounds no area and is dropped.
            let tri_idx: Vec<[u32; 3]> = (0..chunk.len())
                .map(|k| [idx[3 * k], idx[3 * k + 1], idx[3 * k + 2]])
                .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
                .collect();
            if tri_idx.is_empty() {
                continue;
            }
            let base = self.model_vert_base as u32;
            out.push(self.encode_polys(surfprop, &tri_idx, base)?);
        }
        Ok(out)
    }

    fn encode_hull(&mut self, h: &HullItem) -> Result<Vec<u32>, CollError> {
        let mut blob: Vec<u8> = Vec::with_capacity(20 + h.q.len() * 6 + h.planes.len() * 3);
        blob.push(h.q.len() as u8);
        blob.push(h.planes.len() as u8);
        blob.push(0);
        blob.push(0);
        for v in h.origin {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        let scale = (h.step / DECODE_SCALE_UNIT) as f32;
        blob.extend_from_slice(&scale.to_le_bytes());
        for v in &h.q {
            for c in v {
                blob.extend_from_slice(&(*c as i16).to_le_bytes());
            }
        }
        for p in &h.planes {
            for c in p {
                blob.push(*c);
            }
        }
        while !blob.len().is_multiple_of(4) {
            blob.push(0);
        }
        let mut dwords: Vec<u32> = blob
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut tri_sets = 0u32;
        for (surfprop, tris) in &h.sets {
            for chunk in tris.chunks(MAX_POLYS_PER_LEAF) {
                let idx: Vec<[u32; 3]> =
                    chunk.iter().map(|t| [t[0] as u32, t[1] as u32, t[2] as u32]).collect();
                let words = self.encode_polys(*surfprop, &idx, 0)?;
                dwords.extend(words);
                tri_sets += 1;
            }
        }
        if tri_sets > 0xFF {
            return Err(CollError::HullTooLarge(h.q.len(), tri_sets as usize));
        }
        dwords[0] = (dwords[0] & !0x00FF_0000) | (tri_sets << 16);
        Ok(dwords)
    }

    fn encode_item(&mut self, it: &Item) -> Result<Vec<(u32, Vec<u32>)>, CollError> {
        match &it.data {
            ItemData::Prop(p) => Ok(vec![(CHILD_PROP, vec![*p])]),
            ItemData::Hull(h) => Ok(vec![(CHILD_HULL, self.encode_hull(h)?)]),
            ItemData::Poly { surfprop, tris } => {
                Ok(self.encode_tri_leaves(*surfprop, tris)?.into_iter().map(|d| (CHILD_POLY, d)).collect())
            }
        }
    }

    /// Encodes one model and returns its `MODELS` collision fields.
    pub fn build_model(&mut self, items: Vec<Item>, origin: DVec3, decode_step: f64) -> Result<ModelFields, CollError> {
        let node_base = self.nodes.len() / NODE_SIZE;
        if items.is_empty() {
            let cidx = self.contents_index(CONTENTS_EMPTY)?;
            let meta = [cidx as u32, 0, CHILD_EMPTY | (CHILD_EMPTY << 4), CHILD_EMPTY | (CHILD_EMPTY << 4)];
            self.nodes.extend_from_slice(&[0u8; 48]);
            for m in meta {
                self.nodes.extend_from_slice(&m.to_le_bytes());
            }
            return Ok(ModelFields {
                bvh_node: node_base as i32,
                bvh_leaf: self.leaf.len() as i32,
                vert_index: self.verts.len() as i32,
                flags: 0,
                origin: [origin.x as f32, origin.y as f32, origin.z as f32],
                decode_scale: (decode_step / DECODE_SCALE_UNIT) as f32,
            });
        }
        self.model_vert_base = self.verts.len();
        let leaf_base = self.leaf.len();
        self.origin = origin;
        self.step = decode_step;

        let mut flat: Vec<Item> = Vec::new();
        for it in items {
            match &it.data {
                ItemData::Poly { surfprop, tris } if tris.len() > MAX_POLYS_PER_LEAF => {
                    flat.extend(self.split_tris(it.contents, *surfprop, tris)?);
                }
                _ => flat.push(it),
            }
        }
        self.arena.clear();
        let root = self.partition(flat);
        // Breadth-first numbering, the order the engine walks the tree in.
        let mut order: Vec<usize> = vec![root];
        let mut at = 0;
        while at < order.len() {
            let n = order[at];
            for slot in 0..4 {
                if let Some(Slot::Node(inner)) = self.arena[n].children.get(slot) {
                    order.push(*inner);
                }
            }
            at += 1;
        }
        // A child slot stores a node index relative to the model's first node,
        // which is what `bvh_node` addresses.
        for (i, &n) in order.iter().enumerate() {
            self.arena[n].index = i;
        }
        self.nodes.resize(self.nodes.len() + NODE_SIZE * order.len(), 0);
        let order_nodes: Vec<Node> = order.iter().map(|&n| self.arena[n].clone()).collect();
        for (i, n) in order_nodes.iter().enumerate() {
            self.emit_node(n, node_base, leaf_base, i)?;
        }
        Ok(ModelFields {
            bvh_node: node_base as i32,
            bvh_leaf: leaf_base as i32,
            vert_index: self.model_vert_base as i32,
            flags: 0,
            origin: [origin.x as f32, origin.y as f32, origin.z as f32],
            decode_scale: (decode_step / DECODE_SCALE_UNIT) as f32,
        })
    }

    fn split_tris(&self, contents: u8, surfprop: u16, tris: &[[DVec3; 3]]) -> Result<Vec<Item>, CollError> {
        let n = tris.len();
        let mut centroid = vec![[0.0; 3]; n];
        for (i, t) in tris.iter().enumerate() {
            for a in 0..3 {
                centroid[i][a] = (t[0][a] + t[1][a] + t[2][a]) / 3.0;
            }
        }
        let mut out: Vec<Item> = Vec::new();
        let mut sel: Vec<usize> = Vec::new();
        split_rec(tris, &centroid, &mut sel, (0..n).collect(), surfprop, contents, &mut out)?;
        Ok(out)
    }

    /// Top-down build: four groups at a time, splitting on the widest axis of
    /// the child centroids.
    fn partition(&mut self, items: Vec<Item>) -> usize {
        let mut mins = DVec3::splat(f64::INFINITY);
        let mut maxs = DVec3::splat(f64::NEG_INFINITY);
        let mut mask = 0u32;
        for it in &items {
            mins = mins.min(it.mins);
            maxs = maxs.max(it.maxs);
            mask |= self.contents_masks.get(it.contents as usize).copied().unwrap_or(0);
        }
        let at = self.arena.len();
        self.arena.push(Node { mins, maxs, mask, children: Vec::new(), index: 0 });
        let groups = if items.len() <= 4 {
            items.into_iter().map(|it| vec![it]).collect::<Vec<_>>()
        } else {
            split4(items)
        };
        for g in groups {
            if g.len() == 1 {
                let it = &g[0];
                let (mins, maxs) = (it.mins, it.maxs);
                self.arena[at].children.push(Slot::Item { item: it.clone(), mins, maxs });
            } else {
                let inner = self.partition(g);
                self.arena[at].children.push(Slot::Node(inner));
            }
        }
        at
    }

    fn quant(&self, v: DVec3, up: bool) -> [i64; 3] {
        let pad = if up { 1e-3 } else { -1e-3 };
        let a = [
            ((v.x + pad - self.origin.x) / self.step).ceil() as i64,
            ((v.y + pad - self.origin.y) / self.step).ceil() as i64,
            ((v.z + pad - self.origin.z) / self.step).ceil() as i64,
        ];
        let b = [
            ((v.x + pad - self.origin.x) / self.step).floor() as i64,
            ((v.y + pad - self.origin.y) / self.step).floor() as i64,
            ((v.z + pad - self.origin.z) / self.step).floor() as i64,
        ];
        let s = if up { a } else { b };
        [
            s[0].clamp(-32768, 32767),
            s[1].clamp(-32768, 32767),
            s[2].clamp(-32768, 32767),
        ]
    }

    fn emit_node(
        &mut self,
        n: &Node,
        node_base: usize,
        leaf_base: usize,
        order: usize,
    ) -> Result<(), CollError> {
        // The record is `minMax[3][2][4]`: each axis contributes its four child
        // minima and then its four child maxima.
        let mut mm = [0i16; 24];
        let mut meta = [0u32; 4];
        let mut types = [CHILD_EMPTY; 4];
        let cidx = self.contents_index(n.mask)?;
        for slot in 0..4 {
            let Some(ch) = n.children.get(slot) else { continue };
            let (mins, maxs) = match ch {
                Slot::Item { mins, maxs, .. } => (*mins, *maxs),
                Slot::Node(i) => (self.arena[*i].mins, self.arena[*i].maxs),
            };
            let q = self.quant(mins, false);
            let p = self.quant(maxs, true);
            for a in 0..3 {
                mm[a * 8 + slot] = q[a] as i16;
                mm[a * 8 + 4 + slot] = p[a] as i16;
            }
            match ch {
                Slot::Node(i) => {
                    types[slot] = CHILD_NODE;
                    meta[slot] = (self.arena[*i].index as u32) << 8;
                }
                Slot::Item { item, .. } => {
                    let subs = self.encode_item(item)?;
                    if subs.len() == 1 && item.contents == cidx {
                        let (t, dw) = subs.into_iter().next().unwrap();
                        let off = self.leaf.len() - leaf_base;
                        self.leaf.extend(dw);
                        types[slot] = t;
                        if off > 0xFF_FFFF {
                            return Err(CollError::LeafOffset(off));
                        }
                        meta[slot] = (off as u32) << 8;
                    } else {
                        let item_contents = item.contents;
                        types[slot] = CHILD_BUNDLE;
                        let off = self.leaf.len() - leaf_base;
                        self.leaf.push(subs.len() as u32);
                        for (st, dw) in &subs {
                            if dw.len() > 0xFFFF {
                                return Err(CollError::BundleTooBig(dw.len()));
                            }
                            self.leaf.push((item_contents as u32 & 0xFF) | (st << 8) | ((dw.len() as u32) << 16));
                        }
                        for (_, dw) in subs {
                            self.leaf.extend(dw);
                        }
                        if off > 0xFF_FFFF {
                            return Err(CollError::LeafOffset(off));
                        }
                        meta[slot] = (off as u32) << 8;
                    }
                }
            }
        }
        meta[0] |= cidx as u32;
        meta[2] |= types[0] | (types[1] << 4);
        meta[3] |= types[2] | (types[3] << 4);
        let mut rec: Vec<u8> = Vec::with_capacity(NODE_SIZE);
        for c in &mm {
            rec.extend_from_slice(&c.to_le_bytes());
        }
        for m in meta {
            rec.extend_from_slice(&m.to_le_bytes());
        }
        let at = (node_base + order) * NODE_SIZE;
        self.nodes[at..at + NODE_SIZE].copy_from_slice(&rec);
        Ok(())
    }

    // ---- lump bytes ----

    pub fn lump_nodes(&self) -> &[u8] {
        &self.nodes
    }

    pub fn lump_leaf(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.leaf.len() * 4);
        for w in &self.leaf {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }

    pub fn lump_contents(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.contents_masks.len() * 4);
        for m in &self.contents_masks {
            out.extend_from_slice(&m.to_le_bytes());
        }
        out
    }

    pub fn lump_surfprops(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.surfprops.len() * 8);
        for (f, t, c, n) in &self.surfprops {
            out.extend_from_slice(&f.to_le_bytes());
            out.push(*t);
            out.push(*c);
            out.extend_from_slice(&n.to_le_bytes());
        }
        out
    }

    /// The collision vertices, as the shared position pool appends them.
    pub fn lump_verts(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.verts.len() * 12);
        for v in &self.verts {
            for c in v {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        out
    }
}

fn tri_area(p: &[DVec3], t: [usize; 3]) -> f64 {
    (p[t[1]] - p[t[0]]).cross(p[t[2]] - p[t[0]]).length()
}

/// The lowest index of the largest value, so a tie resolves the way the
/// reference writer's `argmax` and `max` do. Iterating with `max_by` would take
/// the last of a tie and pick a different axis, and a different axis changes the
/// tree and the leaf offsets.
fn first_max(v: &[f64]) -> Option<usize> {
    let mut best = None;
    for (i, &x) in v.iter().enumerate() {
        if best.is_none_or(|b| x > v[b]) {
            best = Some(i);
        }
    }
    best
}

fn first_max_by<T: Copy, F: Fn(&T) -> f64>(it: impl Iterator<Item = T>, f: F) -> Option<T> {
    let mut best: Option<(T, f64)> = None;
    for x in it {
        let k = f(&x);
        if best.as_ref().is_none_or(|(_, b)| k > *b) {
            best = Some((x, k));
        }
    }
    best.map(|(x, _)| x)
}

/// The identity a collision vertex is pooled by. A float-to-integer cast
/// saturates, so the bits are the only lossless key, and the two zeroes
/// compare equal the way a float tuple would.
fn pool_key(v: f64) -> u32 {
    let f = v as f32;
    if f == 0.0 {
        0
    } else {
        f.to_bits()
    }
}

fn split_rec(
    tris: &[[DVec3; 3]],
    centroid: &[[f64; 3]],
    _sel: &mut Vec<usize>,
    sel: Vec<usize>,
    surfprop: u16,
    contents: u8,
    out: &mut Vec<Item>,
) -> Result<(), CollError> {
    if sel.len() <= MAX_POLYS_PER_LEAF {
        let list: Vec<[DVec3; 3]> = sel.iter().map(|&i| tris[i]).collect();
        let mut mins = DVec3::splat(f64::INFINITY);
        let mut maxs = DVec3::splat(f64::NEG_INFINITY);
        for t in &list {
            for p in t {
                mins = mins.min(*p);
                maxs = maxs.max(*p);
            }
        }
        out.push(Item { kind: CHILD_POLY, contents, mins, maxs, data: ItemData::Poly { surfprop, tris: list } });
        return Ok(());
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for &i in &sel {
        for a in 0..3 {
            lo[a] = lo[a].min(centroid[i][a]);
            hi[a] = hi[a].max(centroid[i][a]);
        }
    }
    let spread = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let axis = first_max(&spread).unwrap_or(0);
    let mut order = sel.clone();
    order.sort_by(|&a, &b| {
        centroid[a][axis].partial_cmp(&centroid[b][axis]).unwrap_or(std::cmp::Ordering::Equal)
    });
    let h = order.len() / 2;
    let (a, b) = order.split_at(h);
    split_rec(tris, centroid, _sel, a.to_vec(), surfprop, contents, out)?;
    split_rec(tris, centroid, _sel, b.to_vec(), surfprop, contents, out)
}

#[derive(Clone)]
enum Slot {
    Item { item: Item, mins: DVec3, maxs: DVec3 },
    /// Index into the writer's node arena.
    Node(usize),
}

#[derive(Clone)]
struct Node {
    mins: DVec3,
    maxs: DVec3,
    mask: u32,
    children: Vec<Slot>,
    index: usize,
}

fn split2(items: Vec<Item>) -> (Vec<Item>, Vec<Item>) {
    let axis = widest_axis(&items);
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        let ca = (items[a].mins + items[a].maxs) * 0.5;
        let cb = (items[b].mins + items[b].maxs) * 0.5;
        ca[axis].partial_cmp(&cb[axis]).unwrap_or(std::cmp::Ordering::Equal)
    });
    let h = items.len() / 2;
    let (a, b) = order.split_at(h);
    (
        a.iter().map(|&i| items[i].clone()).collect(),
        b.iter().map(|&i| items[i].clone()).collect(),
    )
}

fn widest_axis(items: &[Item]) -> usize {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for it in items {
        let c = (it.mins + it.maxs) * 0.5;
        lo = lo.min(c);
        hi = hi.max(c);
    }
    let d = hi - lo;
    first_max(&[d.x, d.y, d.z]).unwrap_or(0)
}

fn split4(items: Vec<Item>) -> Vec<Vec<Item>> {
    let (a, b) = split2(items);
    let mut out = Vec::new();
    for g in [a, b] {
        if g.len() >= 2 {
            let (x, y) = split2(g);
            out.push(x);
            out.push(y);
        } else if !g.is_empty() {
            out.push(g);
        }
    }
    out
}

/// The node quantisation a model needs: a power-of-two step that keeps the
/// whole box inside the i16 grid. The span is measured from the same rounded
/// origin the model record stores, so a box whose centre lands on a half unit
/// gives the step the writer did.
pub fn decode_step_for(mins: DVec3, maxs: DVec3) -> Result<f64, CollError> {
    let origin = ((mins + maxs) * 0.5).round();
    let span = (maxs - origin).max(origin - mins).max_element();
    if !span.is_finite() {
        return Err(CollError::DecodeStep(0, f64::NAN));
    }
    let mut step = 1.0f64;
    while span / step > 32000.0 {
        step *= 2.0;
    }
    while step > 1.0 / 64.0 && span / (step * 0.5) <= 32000.0 {
        step *= 0.5;
    }
    Ok(step)
}

// ---- emission ---------------------------------------------------------------

/// What the entity stage and the tests need from an emission.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CollisionIndex {
    /// Leaf run and vertex offset per model, plus the quantisation to read
    /// its node bounds with.
    pub models: Vec<Model>,
    /// Contents mask table.
    pub contents: Vec<u32>,
    /// Surface property table, as the lumps hold it.
    pub surfprops: Vec<SurfaceProp>,
    /// Node count and leaf dword count.
    pub node_count: usize,
    pub leaf_words: usize,
    /// Collision vertices appended to the shared position pool.
    pub collision_vertices: usize,
    /// Point-cloud hulls with no surface left to collide with, once rounded
    /// to the grid the engine decodes; hulls that only lost their volume are
    /// written as triangles instead.
    pub dropped_hulls: usize,
}

/// Writes the BVH4 lumps for every solid and returns the index.
pub fn emit_collision(
    solids: &[CollisionSolid],
    models: &[ModelSlot],
    bsp: &mut Bsp,
) -> Result<CollisionIndex, CollError> {
    let v = version_of(bsp);
    let mut writer = BvhWriter::new(vec![CONTENTS_EMPTY]);
    // A solid's `surface_prop` is an index into the 0x11 table the caller
    // seeded; with no table every solid shares one entry.
    let existing = existing_surfprops(bsp);
    let mut names = SurfaceNames::of(bsp);
    let model_count = models.len().max(solids.iter().map(|s| s.model as usize + 1).max().unwrap_or(0));
    let mut per_model: Vec<Vec<Item>> = vec![Vec::new(); model_count];

    let mut dropped_hulls = 0;
    for (i, s) in solids.iter().enumerate() {
        let contents = writer.contents_index(world_contents(s.contents))?;
        let slot = s.model as usize;
        if slot >= per_model.len() {
            return Err(CollError::NotASolid(i));
        }
        let item = match solid_item(&mut writer, i, s, contents, &existing, 0, &mut names) {
            Err(CollError::NotASolid(_) | CollError::HullEmpty(_))
                if matches!(s.shape, CollisionShape::ConvexHull { .. }) =>
            {
                // The hull's surface still collides as triangles.
                match hull_surface(s) {
                    Some(tris) => solid_item(&mut writer, i, &tris, contents, &existing, 0, &mut names)?,
                    None => {
                        dropped_hulls += 1;
                        None
                    }
                }
            }
            other => other.map_err(|e| CollError::Solid { what: describe_solid(i, s), source: Box::new(e) })?,
        };
        if let Some(item) = item {
            per_model[slot].push(item);
        }
    }

    let mut out_models =
        existing_models(bsp).map_err(|e| lump_err(ids::MODELS, e))?;
    while out_models.len() < model_count {
        out_models.push(empty_model());
    }
    // The shared position pool already holds the render vertices; the collision
    // ones are appended, so a model's vertex index is offset by that count.
    let vert_offset = (bsp.lump(ids::VERTICES).len() / 12) as i32;
    for (m, slot) in models.iter().enumerate() {
        let items = std::mem::take(&mut per_model[m]);
        if items.is_empty() {
            let e = &mut out_models[m];
            e.bvh_node = -1;
            e.bvh_leaf = -1;
            e.vert_index = 0;
            e.origin = [0.0; 3];
            e.decode_scale = 0.0;
            e.mins = slot.mins.to_array();
            e.maxs = slot.maxs.to_array();
            continue;
        }
        let mut lo = slot.mins.as_dvec3();
        let mut hi = slot.maxs.as_dvec3();
        // The model box leaves prop leaves out: staging_mu1's world box is the
        // union of its render and collision triangles, and its prop leaves
        // reach past it on five sides.
        let (mut blo, mut bhi) = (lo, hi);
        for it in &items {
            lo = lo.min(it.mins);
            hi = hi.max(it.maxs);
            if !matches!(it.data, ItemData::Prop(_)) {
                blo = blo.min(it.mins);
                bhi = bhi.max(it.maxs);
            }
        }
        let step = decode_step_for(lo, hi)?;
        let f = writer.build_model(items, ((lo + hi) * 0.5).round(), step)?;
        let e = &mut out_models[m];
        e.bvh_node = f.bvh_node;
        e.bvh_leaf = f.bvh_leaf;
        e.vert_index = f.vert_index + vert_offset;
        e.flags = f.flags;
        e.origin = f.origin;
        e.decode_scale = f.decode_scale;
        // The model box covers the tree as well as the meshes: staging_mu1's
        // world box reaches the collision-only walls at its edges, and a
        // decoder bounds a leaf's geometry by it.
        if blo.x > bhi.x {
            // Props alone: the box is theirs.
            (blo, bhi) = (lo, hi);
        }
        if e.mins[0] >= e.maxs[0] {
            e.mins = [blo.x as f32, blo.y as f32, blo.z as f32];
            e.maxs = [bhi.x as f32, bhi.y as f32, bhi.z as f32];
        } else {
            for a in 0..3 {
                e.mins[a] = e.mins[a].min(blo[a] as f32);
                e.maxs[a] = e.maxs[a].max(bhi[a] as f32);
            }
        }
    }

    let content_lump: Vec<ContentMask> = writer.contents_masks.iter().map(|m| ContentMask { mask: *m }).collect();
    let surf_lump: Vec<SurfaceProp> = writer
        .surfprops
        .iter()
        .map(|(f, t, c, n)| SurfaceProp { flags: *f, surface_type: *t, contents: *c, name: *n })
        .collect();
    let node_lump: Vec<BvhNode> = parse_nodes(&writer.nodes).map_err(|e| lump_err(ids::BVH_NODES, e))?;
    let leaf_lump = BvhLeafData { words: writer.leaf.clone() };
    let pack_verts: Vec<PackVert> = Vec::new();

    let mut vert_data = bsp.lump(ids::VERTICES).to_vec();
    vert_data.extend_from_slice(&writer.lump_verts());

    bsp.set_lump(ids::VERTICES, vert_data);
    bsp.set_lump(ids::MODELS, rbsp::lumps::collision::write_models(&out_models, v));
    bsp.set_lump(ids::CONTENTS_MASKS, rbsp::lumps::collision::write_content_masks(&content_lump, v));
    bsp.set_lump(ids::SURFACE_PROPERTIES, rbsp::lumps::collision::write_surface_props(&surf_lump, v));
    names.write(bsp, v);
    bsp.set_lump(ids::BVH_NODES, rbsp::lumps::collision::write_bvh_nodes(&node_lump, v));
    bsp.set_lump(ids::BVH_LEAF_DATA, rbsp::lumps::collision::write_bvh_leaf(&leaf_lump, v));
    if pack_verts.is_empty() {
        bsp.remove_lump(ids::PACKED_VERTICES);
    } else {
        bsp.set_lump(ids::PACKED_VERTICES, rbsp::lumps::collision::write_pack_verts(&pack_verts, v));
    }
    Ok(CollisionIndex {
        models: out_models,
        contents: writer.contents_masks,
        surfprops: surf_lump,
        node_count: writer.nodes.len() / NODE_SIZE,
        leaf_words: writer.leaf.len(),
        collision_vertices: writer.verts.len(),
        dropped_hulls,
    })
}

/// The surface flags a shipped map files a material's faces under.
fn surface_flags(material: &str) -> u16 {
    let m = material.to_ascii_lowercase().replace('\\', "/");
    match m.as_str() {
        "tools/toolsskybox" => 0x404,
        "tools/toolsnodraw" => 0x680,
        "tools/toolsblack" => 0x400,
        _ if m.starts_with("tools/") => 0x480,
        _ => 0x200,
    }
}

/// The 0x0F name blob a surface property names its material in; a material
/// the render pass did not already file is appended.
struct SurfaceNames {
    table: Option<rbsp::lumps::strings::StringTable>,
    added: bool,
}

impl SurfaceNames {
    fn of(bsp: &Bsp) -> Self {
        let table = rbsp::lumps::render::parse_surface_names(bsp.lump(ids::SURFACE_NAMES), version_of(bsp)).ok();
        Self { table, added: false }
    }

    fn offset(&mut self, material: &str) -> u32 {
        let Some(t) = self.table.as_mut() else { return 0 };
        if let Some(k) = t.strings.iter().position(|n| n.eq_ignore_ascii_case(material)) {
            return t.offset_of(k) as u32;
        }
        if !t.tail.is_empty() {
            return 0;
        }
        t.strings.push(material.to_string());
        self.added = true;
        t.offset_of(t.strings.len() - 1) as u32
    }

    fn write(&self, bsp: &mut Bsp, v: rbsp::Version) {
        if let (true, Some(t)) = (self.added, self.table.as_ref()) {
            bsp.set_lump(ids::SURFACE_NAMES, rbsp::lumps::render::write_surface_names(t, v));
        }
    }
}

/// A point-cloud hull as the triangle soup of its surface, for a hull that
/// does not survive being rebuilt from its planes.
fn hull_surface(s: &CollisionSolid) -> Option<CollisionSolid> {
    let CollisionShape::ConvexHull { points } = &s.shape else { return None };
    let pts: Vec<DVec3> = points.iter().map(|p| p.as_dvec3()).collect();
    let hull = convex_hull(&pts, 1e-4).ok()?;
    let vertices: Vec<Vec3> = hull.verts.iter().map(|&i| points[i]).collect();
    let indices: Vec<u32> = hull.faces.iter().flat_map(|f| f.iter().map(|&v| v as u32)).collect();
    Some(CollisionSolid { shape: CollisionShape::Triangles { vertices, indices }, ..s.clone() })
}

/// The item one solid becomes, or `None` for a triangle soup with nothing left
/// once its degenerate triangles are gone. `name` is the 0x0F offset a solid
/// with no surface of its own is filed under.
fn solid_item(
    writer: &mut BvhWriter,
    i: usize,
    s: &CollisionSolid,
    contents: u8,
    existing: &[(u16, u8, u8, u32)],
    name: u32,
    names: &mut SurfaceNames,
) -> Result<Option<Item>, CollError> {
    let mut own = Vec::with_capacity(s.surfaces.len());
    for surf in &s.surfaces {
        let name = names.offset(&surf.material);
        own.push(writer.surfprop_index(surface_flags(&surf.material), surf.surface_type, contents, name)?);
    }
    let sp = match (own.first(), existing.get(s.surface_prop as usize)) {
        (Some(sp), _) => *sp,
        (None, Some((flags, stype, cidx, name))) => writer.surfprop_index(*flags, *stype, *cidx, *name)?,
        (None, None) => writer.surfprop_index(0, 0, contents, name)?,
    };
    let sides = |n: usize| if own.len() == n { own.clone() } else { vec![sp; n] };
    Ok(Some(match &s.shape {
        CollisionShape::Brush { planes } => {
            let hull: Vec<crate::csg::DPlane> =
                planes.iter().map(|p| crate::csg::DPlane::new(p.0.as_dvec3(), p.1 as f64)).collect();
            let verts = crate::csg::hull_vertices_planes(&hull).ok_or(CollError::NotASolid(i))?;
            let normals: Vec<DVec3> = hull.iter().map(|p| p.normal).collect();
            writer.hull_item(contents, &verts, &normals, &sides(normals.len()))?
        }
        CollisionShape::ConvexHull { points } => {
            let pts: Vec<DVec3> = points.iter().map(|p| p.as_dvec3()).collect();
            let hull = crate::csg::hull_vertices_planes(&hull_planes_of(&pts)).ok_or(CollError::NotASolid(i))?;
            // Two nearly coplanar facets meet far outside the points; a hull
            // rebuilt that way is not the solid it came from.
            let (lo, hi) = pts.iter().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
            if hull.iter().any(|v| v.cmplt(lo - DVec3::ONE).any() || v.cmpgt(hi + DVec3::ONE).any()) {
                return Err(CollError::NotASolid(i));
            }
            let normals: Vec<DVec3> = hull_planes_of(&pts).iter().map(|p| p.normal).collect();
            writer.hull_item(contents, &hull, &normals, &vec![sp; normals.len()])?
        }
        CollisionShape::Prop { word, mins, maxs } => {
            if mins.x > maxs.x {
                return Ok(None);
            }
            writer.prop_item(contents, *word, mins.as_dvec3(), maxs.as_dvec3())
        }
        CollisionShape::Triangles { vertices, indices } => {
            if indices.len() % 3 != 0 {
                return Err(CollError::NotASolid(i));
            }
            let mut tris: Vec<[DVec3; 3]> = Vec::with_capacity(indices.len() / 3);
            for t in indices.chunks_exact(3) {
                let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
                if a >= vertices.len() || b >= vertices.len() || c >= vertices.len() {
                    return Err(CollError::NotASolid(i));
                }
                let tri = [vertices[a].as_dvec3(), vertices[b].as_dvec3(), vertices[c].as_dvec3()];
                let e1 = tri[1] - tri[0];
                let e2 = tri[2] - tri[0];
                let e3 = tri[2] - tri[1];
                if e1.length() <= CORNER_EPS || e2.length() <= CORNER_EPS || e3.length() <= CORNER_EPS {
                    continue;
                }
                let area = e1.cross(e2).length();
                if area <= 1e-6 * (1.0 + e1.length() + e2.length()) {
                    continue;
                }
                tris.push(tri);
            }
            if tris.is_empty() {
                return Ok(None);
            }
            writer.tri_item(contents, sp, tris)
        }
    }))
}

/// What appending collision to a shipped world took.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Appended {
    pub nodes: usize,
    pub leaf_words: usize,
    pub vertices: usize,
}

/// Adds solids to model 0 of a map whose collision is already built, without
/// moving anything the map holds.
///
/// The new solids get a tree of their own, quantised in model 0's frame and
/// appended after every node, leaf dword and vertex the map has. Node 0 must
/// stay model 0's root, since every child index in the model counts from it,
/// so the old root moves to the end and node 0 becomes a root with two
/// children: the old tree and the new one. A polygon leaf addresses the vertex
/// pool from the model's `vert_index` in steps of 1024, so the pool is padded
/// to the next such step before the new vertices go in.
pub fn append_world(bsp: &mut Bsp, solids: &[CollisionSolid]) -> Result<Appended, CollError> {
    use rbsp::lumps::collision as c;
    let v = version_of(bsp);
    let mut models = c::parse_models(bsp.lump(ids::MODELS), v).map_err(|e| lump_err(ids::MODELS, e))?;
    let Some(m0) = models.first().copied() else { return Err(CollError::NotASolid(0)) };
    if !model_has_collision(&m0) || m0.bvh_node != 0 || m0.bvh_leaf != 0 {
        return Err(CollError::Lump {
            id: ids::MODELS,
            reason: "model 0 has to own the tree at node 0 and leaf 0 for collision to be appended".into(),
        });
    }
    let masks: Vec<u32> = c::parse_content_masks(bsp.lump(ids::CONTENTS_MASKS), v)
        .map_err(|e| lump_err(ids::CONTENTS_MASKS, e))?
        .iter()
        .map(|m| m.mask)
        .collect();
    let mut writer = BvhWriter::new(masks);
    let existing = existing_surfprops(bsp);
    for sp in &existing {
        writer.surfprop_index(sp.0, sp.1, sp.2, sp.3)?;
    }
    // A solid with no surface of its own is filed under `_DEFAULT` when the map
    // has that name, as the shipped surfaces with no material of their own are.
    let names = rbsp::lumps::render::parse_surface_names(bsp.lump(ids::SURFACE_NAMES), v).ok();
    let default_name = names
        .as_ref()
        .and_then(|t| (0..t.strings.len()).find(|k| t.strings[*k] == "_DEFAULT").map(|k| t.offset_of(k) as u32))
        .unwrap_or(0);
    let mut surface_names = SurfaceNames::of(bsp);
    let mut items = Vec::new();
    // "Solid" is whatever the map's own world is made of: the shipped mask
    // when the map uses it, the document's value otherwise.
    let shipped_solid = writer.contents_masks.contains(&CONTENTS_WORLD);
    for (i, s) in solids.iter().enumerate() {
        let mask = if shipped_solid { world_contents(s.contents) } else { s.contents };
        let contents = writer.contents_index(mask)?;
        // A solid's surface index names a shipped surface only when the caller
        // says so; an authored solid always gets one of its own.
        let own = CollisionSolid { surface_prop: u32::MAX, ..s.clone() };
        if let Some(item) = solid_item(&mut writer, i, &own, contents, &[], default_name, &mut surface_names)? {
            items.push(item);
        }
    }
    if items.is_empty() {
        return Ok(Appended::default());
    }
    let origin = DVec3::new(m0.origin[0] as f64, m0.origin[1] as f64, m0.origin[2] as f64);
    let step = m0.decode_scale as f64 * DECODE_SCALE_UNIT;
    writer.build_model(items, origin, step)?;

    let mut nodes = c::parse_bvh_nodes(bsp.lump(ids::BVH_NODES), v).map_err(|e| lump_err(ids::BVH_NODES, e))?;
    let mut words = c::parse_bvh_leaf(bsp.lump(ids::BVH_LEAF_DATA), v).map_err(|e| lump_err(ids::BVH_LEAF_DATA, e))?.words;
    let added = parse_nodes(&writer.nodes).map_err(|e| lump_err(ids::BVH_NODES, e))?;
    let node_at = nodes.len();
    let word_at = words.len();

    // The position pool: pad so the new vertices start a whole number of
    // 1024-vertex steps after model 0's first collision vertex.
    let mut verts = bsp.lump(ids::VERTICES).to_vec();
    let base = m0.vert_index.max(0) as usize;
    let mut start = verts.len() / 12;
    if start < base {
        start = base;
    }
    let steps = (start - base).div_ceil(1024);
    let start = base + steps * 1024;
    verts.resize(start * 12, 0);
    verts.extend_from_slice(&writer.lump_verts());

    // Rebase the new tree: node children onto the appended block, leaves onto
    // the appended words, polygon headers onto the appended vertices.
    let mut leaf = writer.leaf.clone();
    let mut polys: Vec<usize> = Vec::new();
    let mut seen = vec![false; added.len()];
    let mut stack = vec![0usize];
    while let Some(n) = stack.pop() {
        if n >= added.len() || std::mem::replace(&mut seen[n], true) {
            continue;
        }
        for slot in 0..4 {
            let kind = added[n].child_kind(slot);
            let at = added[n].child_index(slot) as usize;
            match kind {
                CHILD_EMPTY => {}
                CHILD_NODE => stack.push(at),
                CHILD_BUNDLE => {
                    let count = leaf.get(at).copied().unwrap_or(0) as usize;
                    let mut pay = at + 1 + count;
                    for k in 0..count {
                        let desc = leaf[at + 1 + k];
                        if (CHILD_POLY..=CHILD_POLY_MAX).contains(&((desc >> 8) & 0xFF)) {
                            polys.push(pay);
                        }
                        pay += (desc >> 16) as usize;
                    }
                }
                k if (CHILD_POLY..=CHILD_POLY_MAX).contains(&k) => polys.push(at),
                _ => {}
            }
        }
    }
    let lift = steps as u32;
    for at in polys {
        let h = leaf[at];
        let first = (h >> 16) + lift;
        if first > 0xFFFF {
            return Err(CollError::LeafOffset(first as usize));
        }
        leaf[at] = (h & 0xFFFF) | (first << 16);
    }
    let mut rebased = added.clone();
    for node in &mut rebased {
        for slot in 0..4 {
            let kind = node.child_kind(slot);
            let idx = node.child_index(slot) as usize;
            let moved = match kind {
                CHILD_EMPTY => continue,
                CHILD_NODE => idx + node_at,
                _ => idx + word_at,
            };
            if moved > 0xFF_FFFF {
                return Err(CollError::LeafOffset(moved));
            }
            node.meta[slot] = (node.meta[slot] & 0xFF) | ((moved as u32) << 8);
        }
    }
    words.extend_from_slice(&leaf);

    // Node 0 becomes a root over the old tree, moved to the end, and the new.
    let old_root = nodes[0];
    let old_root_at = node_at + rebased.len();
    let union = |n: &BvhNode| -> ([i16; 3], [i16; 3]) {
        let mut lo = [i16::MAX; 3];
        let mut hi = [i16::MIN; 3];
        for slot in 0..4 {
            if n.child_kind(slot) == CHILD_EMPTY {
                continue;
            }
            for a in 0..3 {
                lo[a] = lo[a].min(n.min_max[a * 8 + slot]);
                hi[a] = hi[a].max(n.min_max[a * 8 + 4 + slot]);
            }
        }
        (lo, hi)
    };
    let (old_lo, old_hi) = union(&old_root);
    let (new_lo, new_hi) = union(&rebased[0]);
    let old_mask = writer.contents_masks.get(old_root.contents_index() as usize).copied().unwrap_or(0);
    let new_mask = writer.contents_masks.get(rebased[0].contents_index() as usize).copied().unwrap_or(0);
    let root_contents = writer.contents_index(old_mask | new_mask)?;
    let mut root = BvhNode::default();
    for a in 0..3 {
        root.min_max[a * 8] = old_lo[a];
        root.min_max[a * 8 + 4] = old_hi[a];
        root.min_max[a * 8 + 1] = new_lo[a];
        root.min_max[a * 8 + 4 + 1] = new_hi[a];
        for slot in 2..4 {
            root.min_max[a * 8 + slot] = 0;
            root.min_max[a * 8 + 4 + slot] = 0;
        }
    }
    root.meta[0] = root_contents as u32 | ((old_root_at as u32) << 8);
    root.meta[1] = (node_at as u32) << 8;
    root.meta[2] = CHILD_NODE | (CHILD_NODE << 4);
    root.meta[3] = CHILD_EMPTY | (CHILD_EMPTY << 4);
    nodes.extend(rebased.iter().copied());
    nodes.push(old_root);
    nodes[0] = root;

    // Model 0's box grows to hold the new solids.
    let step32 = step as f32;
    let m = &mut models[0];
    for a in 0..3 {
        m.mins[a] = m.mins[a].min(m0.origin[a] + new_lo[a] as f32 * step32);
        m.maxs[a] = m.maxs[a].max(m0.origin[a] + new_hi[a] as f32 * step32);
    }

    let content_lump: Vec<ContentMask> = writer.contents_masks.iter().map(|m| ContentMask { mask: *m }).collect();
    let surf_lump: Vec<SurfaceProp> = writer
        .surfprops
        .iter()
        .map(|(f, t, c, n)| SurfaceProp { flags: *f, surface_type: *t, contents: *c, name: *n })
        .collect();
    let out = Appended { nodes: rebased.len() + 1, leaf_words: leaf.len(), vertices: writer.verts.len() };
    bsp.set_lump(ids::VERTICES, verts);
    bsp.set_lump(ids::MODELS, c::write_models(&models, v));
    bsp.set_lump(ids::CONTENTS_MASKS, c::write_content_masks(&content_lump, v));
    bsp.set_lump(ids::SURFACE_PROPERTIES, c::write_surface_props(&surf_lump, v));
    surface_names.write(bsp, v);
    bsp.set_lump(ids::BVH_NODES, c::write_bvh_nodes(&nodes, v));
    bsp.set_lump(ids::BVH_LEAF_DATA, c::write_bvh_leaf(&BvhLeafData { words }, v));
    Ok(out)
}

fn hull_planes_of(pts: &[DVec3]) -> Vec<crate::csg::DPlane> {
    planes_from_points(pts)
}

/// The planes of the convex volume a point cloud spans, taken from every triple
/// of hull facets.
fn planes_from_points(pts: &[DVec3]) -> Vec<crate::csg::DPlane> {
    match convex_hull(pts, 1e-6) {
        Ok(h) => {
            let used: Vec<DVec3> = h.verts.iter().map(|&i| pts[i]).collect();
            let mut out: Vec<crate::csg::DPlane> = Vec::new();
            for f in &h.faces {
                let n = (used[f[1]] - used[f[0]]).cross(used[f[2]] - used[f[0]]);
                if n.length_squared() <= 0.0 {
                    continue;
                }
                let n = n.normalize();
                let p = crate::csg::DPlane { normal: n, dist: n.dot(used[f[0]]) };
                if !out.iter().any(|q| q.same(&p) && q.normal.dot(p.normal) > 0.0) {
                    out.push(p);
                }
            }
            out
        }
        Err(_) => Vec::new(),
    }
}

fn parse_nodes(raw: &[u8]) -> BspResult<Vec<BvhNode>> {
    rbsp::lumps::collision::parse_bvh_nodes(raw, Version::ClientV51)
}

fn existing_models(bsp: &Bsp) -> BspResult<Vec<Model>> {
    if bsp.lump(ids::MODELS).is_empty() {
        return Ok(Vec::new());
    }
    rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), version_of(bsp))
}

fn existing_surfprops(bsp: &Bsp) -> Vec<(u16, u8, u8, u32)> {
    if bsp.lump(ids::SURFACE_PROPERTIES).is_empty() {
        return Vec::new();
    }
    rbsp::lumps::collision::parse_surface_props(bsp.lump(ids::SURFACE_PROPERTIES), version_of(bsp))
        .map(|p| p.iter().map(|s| (s.flags, s.surface_type, s.contents, s.name)).collect())
        .unwrap_or_default()
}

fn empty_model() -> Model {
    Model {
        mins: [0.0; 3],
        maxs: [0.0; 3],
        first_mesh: 0,
        num_meshes: 0,
        bvh_node: -1,
        bvh_leaf: -1,
        vert_index: 0,
        flags: 0,
        origin: [0.0; 3],
        decode_scale: 0.0,
    }
}

// ---- decoding ---------------------------------------------------------------

/// One leaf of a decoded map: its kind, contents mask and slot bounds.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedLeaf {
    pub kind: u32,
    pub contents: u32,
    pub surfprop: u16,
    pub bounds_min: DVec3,
    pub bounds_max: DVec3,
    pub prop_index: u32,
    /// The triangles the leaf draws, world space.
    pub triangles: Vec<[DVec3; 3]>,
}

/// Every collision lump of a map, parsed once so a whole-map walk does not
/// re-read the node table per model.
struct Collider {
    models: Vec<Model>,
    nodes: Vec<BvhNode>,
    leaf: BvhLeafData,
    cmasks: Vec<ContentMask>,
    verts: Vec<[f32; 3]>,
    /// The same vertex bytes as int16 triples, for the leaf types that read
    /// them at that width.
    verts_i16: Vec<i16>,
}

impl Collider {
    fn load(bsp: &Bsp) -> Result<Self, CollError> {
        let v = version_of(bsp);
        let verts: Vec<[f32; 3]> = rbsp::lumps::prims::parse(bsp.lump(ids::VERTICES), v)
            .map_err(|e| lump_err(ids::VERTICES, e))?
            .iter()
            .map(|p| [p.x, p.y, p.z])
            .collect();
        let verts_i16 = f32_pool_as_i16(&verts);
        Ok(Self {
            models: rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), v)
                .map_err(|e| lump_err(ids::MODELS, e))?,
            nodes: rbsp::lumps::collision::parse_bvh_nodes(bsp.lump(ids::BVH_NODES), v)
                .map_err(|e| lump_err(ids::BVH_NODES, e))?,
            leaf: rbsp::lumps::collision::parse_bvh_leaf(bsp.lump(ids::BVH_LEAF_DATA), v)
                .map_err(|e| lump_err(ids::BVH_LEAF_DATA, e))?,
            cmasks: rbsp::lumps::collision::parse_content_masks(bsp.lump(ids::CONTENTS_MASKS), v)
                .map_err(|e| lump_err(ids::CONTENTS_MASKS, e))?,
            verts,
            verts_i16,
        })
    }

    /// Every leaf one model's tree reaches. A child node slot is an index
    /// relative to the model's own first node, so a tree never leaves the
    /// block of nodes `bvh_node` opens.
    fn leaves(&self, model: usize) -> Result<Vec<DecodedLeaf>, CollError> {
        let Some(m) = self.models.get(model) else { return Ok(Vec::new()) };
        if !model_has_collision(m) {
            return Ok(Vec::new());
        }
        let base = m.bvh_leaf.max(0) as usize;
        let node0 = m.bvh_node.max(0) as usize;
        // A node stores each child bound as an int16 the engine widens by 16
        // bits before scaling, so the stored step is decodeScale * 65536.
        let scale = m.decode_scale as f64 * DECODE_SCALE_UNIT;
        let mut out: Vec<DecodedLeaf> = Vec::new();
        let mut visited: Vec<bool> = vec![false; self.nodes.len()];
        let mut stack: Vec<usize> = vec![node0];
        while let Some(ni) = stack.pop() {
            if ni >= self.nodes.len() {
                continue;
            }
            let node = &self.nodes[ni];
            if std::mem::replace(&mut visited[ni], true) {
                continue;
            }
            let cidx = node.contents_index() as usize;
            let node_contents = self.cmasks.get(cidx).map(|c| c.mask).unwrap_or(0);
            for slot in 0..4 {
                let kind = node.child_kind(slot);
                let at = node.child_index(slot) as usize;
                let smin = slot_min(node, slot, m, scale);
                let smax = slot_max(node, slot, m, scale);
                match kind {
                    CHILD_EMPTY => {}
                    CHILD_NODE => stack.push(node0 + at),
                    CHILD_BUNDLE => {
                        // A bundle is its count, then every descriptor, then the
                        // payloads in descriptor order: the layout the writer
                        // emits and the one shipped maps use.
                        let Some(&count) = self.leaf.words.get(base + at) else { continue };
                        let mut pay = base + at + 1 + count as usize;
                        for k in 0..count as usize {
                            let Some(&desc) = self.leaf.words.get(base + at + 1 + k) else { break };
                            let sub = (desc >> 8) & 0xFF;
                            let size = (desc >> 16) as usize;
                            let contents =
                                self.cmasks.get((desc & 0xFF) as usize).map(|c| c.mask).unwrap_or(0);
                            read_leaf(
                                &self.leaf,
                                &self.verts,
                                &self.verts_i16,
                                m,
                                sub,
                                pay,
                                contents,
                                smin,
                                smax,
                                &mut out,
                            )?;
                            pay += size;
                        }
                    }
                    _ => read_leaf(
                        &self.leaf,
                        &self.verts,
                        &self.verts_i16,
                        m,
                        kind,
                        base + at,
                        node_contents,
                        smin,
                        smax,
                        &mut out,
                    )?,
                }
            }
        }
        Ok(out.into_iter()
            .map(|mut l| {
                l.triangles.retain(|t| inside_model(t, m));
                l
            })
            .collect())
    }
}

/// Every leaf a model's tree reaches.
pub fn decode_leaves(bsp: &Bsp, model: usize) -> Result<Vec<DecodedLeaf>, CollError> {
    Collider::load(bsp)?.leaves(model)
}

/// The float position pool's own bytes read as int16 triples. A leaf type with
/// the int16 bit set indexes the same region at that width, and the engine
/// points its `verts` and `packedVerts` at one address and lets the leaf type
/// choose, so the byte pattern is what has to be shared.
pub fn f32_pool_as_i16(verts: &[[f32; 3]]) -> Vec<i16> {
    let mut out = Vec::with_capacity(verts.len() * 6);
    for v in verts {
        for c in v {
            let bits = c.to_bits();
            out.push(bits as u16 as i16);
            out.push((bits >> 16) as u16 as i16);
        }
    }
    out
}

/// A hull leaf, decoded from the raw byte view of the dword stream. The header
/// packs four byte counts ahead of the origin floats, so it cannot be read as
/// a float dword.
#[derive(Clone, Debug, PartialEq)]
pub struct HullView {
    pub origin: [f32; 3],
    pub scale: f32,
    pub verts: Vec<[i16; 3]>,
    pub planes: Vec<[u8; 3]>,
    pub tri_set_count: u8,
    pub quad_set_count: u8,
    pub header_dwords: u32,
}

fn word_byte(leaf: &BvhLeafData, at: usize) -> Option<u8> {
    leaf.words.get(at / 4).map(|w| (w >> (8 * (at % 4))) as u8)
}

fn word_i16(leaf: &BvhLeafData, at: usize) -> Option<i16> {
    let lo = word_byte(leaf, at)? as u16;
    let hi = word_byte(leaf, at + 1)? as u16;
    Some(i16::from_le_bytes([lo as u8, hi as u8]))
}

fn word_f32(leaf: &BvhLeafData, at: usize) -> Option<f32> {
    let b: [u8; 4] = [
        word_byte(leaf, at)?,
        word_byte(leaf, at + 1)?,
        word_byte(leaf, at + 2)?,
        word_byte(leaf, at + 3)?,
    ];
    Some(f32::from_le_bytes(b))
}

/// Decodes the hull leaf header, its quantised corners and its plane triples.
pub fn decode_hull(leaf: &BvhLeafData, at: usize) -> Result<HullView, CollError> {
    let base = at * 4;
    let need = |b: usize| -> Result<(), CollError> {
        if base + b >= leaf.words.len() * 4 {
            Err(CollError::Lump { id: 0x13, reason: "hull leaf runs past the stream".into() })
        } else {
            Ok(())
        }
    };
    need(0)?;
    let nv = word_byte(leaf, base).unwrap_or(0) as usize;
    let npl = word_byte(leaf, base + 1).unwrap_or(0) as usize;
    let nts = word_byte(leaf, base + 2).unwrap_or(0);
    let nqs = word_byte(leaf, base + 3).unwrap_or(0);
    need(19)?;
    let origin = [
        word_f32(leaf, base + 4).unwrap_or(0.0),
        word_f32(leaf, base + 8).unwrap_or(0.0),
        word_f32(leaf, base + 12).unwrap_or(0.0),
    ];
    let scale = word_f32(leaf, base + 16).unwrap_or(0.0);
    let header_dwords = (20 + nv * 6 + npl * 3).div_ceil(4) as u32;
    need(20 + nv * 6 + npl * 3)?;
    let mut verts = Vec::with_capacity(nv);
    for i in 0..nv {
        let b = 20 + i * 6;
        verts.push([
            word_i16(leaf, base + b).unwrap_or(0),
            word_i16(leaf, base + b + 2).unwrap_or(0),
            word_i16(leaf, base + b + 4).unwrap_or(0),
        ]);
    }
    let mut planes = Vec::with_capacity(npl);
    for i in 0..npl {
        let b = 20 + nv * 6 + i * 3;
        planes.push([
            word_byte(leaf, base + b).unwrap_or(0),
            word_byte(leaf, base + b + 1).unwrap_or(0),
            word_byte(leaf, base + b + 2).unwrap_or(0),
        ]);
    }
    Ok(HullView { origin, scale, verts, planes, tri_set_count: nts, quad_set_count: nqs, header_dwords })
}

/// The record is `minMax[3][2][4]`: each axis contributes its four child minima
/// and then its four child maxima, so axis `a` reads at `a * 8` and
/// `a * 8 + 4`.
fn slot_min(node: &BvhNode, slot: usize, m: &Model, scale: f64) -> DVec3 {
    let q = &node.min_max;
    DVec3::new(
        m.origin[0] as f64 + q[slot] as f64 * scale,
        m.origin[1] as f64 + q[8 + slot] as f64 * scale,
        m.origin[2] as f64 + q[16 + slot] as f64 * scale,
    )
}

fn slot_max(node: &BvhNode, slot: usize, m: &Model, scale: f64) -> DVec3 {
    let q = &node.min_max;
    DVec3::new(
        m.origin[0] as f64 + q[4 + slot] as f64 * scale,
        m.origin[1] as f64 + q[12 + slot] as f64 * scale,
        m.origin[2] as f64 + q[20 + slot] as f64 * scale,
    )
}

/// A leaf's geometry lies inside the AABB its own model declares. The shared
/// position pool opens with a padding record of garbage that a handful of leaf
/// entries name, and a corner that far out is an entry no geometry backs: it is
/// not a triangle, and letting it through inflates a model's quantisation step
/// until nothing else in the model addresses anything.
const MODEL_SLOP: f64 = 1.0;

fn inside_model(t: &[DVec3; 3], m: &Model) -> bool {
    let lo = DVec3::new(m.mins[0] as f64, m.mins[1] as f64, m.mins[2] as f64);
    let hi = DVec3::new(m.maxs[0] as f64, m.maxs[1] as f64, m.maxs[2] as f64);
    t.iter().all(|p| {
        p.is_finite()
            && (0..3).all(|a| p[a] >= lo[a] - MODEL_SLOP && p[a] <= hi[a] + MODEL_SLOP)
    })
}

/// Where a polygon leaf's vertex indices resolve.
enum VertexSource<'a> {
    /// The shared float position pool, indexed from a model's vertex block.
    Pool { verts: &'a [[f32; 3]], base: usize },
    /// A convex hull leaf's own quantised corners.
    Local { points: &'a [DVec3] },
    /// The same bytes read as int16 triples, dequantised against the owner.
    /// A leaf type with bit 0 set addresses the vertex region this way.
    Quantised { origin: DVec3, scale: f64, raw: &'a [i16] },
}

impl VertexSource<'_> {
    fn at(&self, i: u32) -> Option<DVec3> {
        match self {
            VertexSource::Pool { verts, base } => {
                let p = verts.get(base.checked_add(i as usize)?)?;
                Some(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))
            }
            VertexSource::Local { points } => points.get(i as usize).copied(),
            VertexSource::Quantised { origin, scale, raw } => {
                let o = (i as usize).checked_mul(3)?;
                let q = raw.get(o..o + 3)?;
                Some(*origin + DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64) * *scale)
            }
        }
    }
}

/// A polygon leaf's entries, before the corner indices are resolved. The child
/// type nibble picks the layout: bit 0 selects the int16 vertex reading and bit 1
/// makes every entry a parallelogram, which yields two triangles and packs its
/// vertex deltas one bit narrower.
struct PolyEntries {
    surfprop: u16,
    dwords: usize,
    /// Three corner indices per entry. A parallelogram entry stores three of its
    /// four corners; the fourth is a position the engine forms from the other
    /// three, so it is never an index.
    corners: Vec<[u32; 3]>,
    quad: bool,
}

fn poly_entries(leaf: &BvhLeafData, kind: u32, at: usize) -> Option<PolyEntries> {
    let h = *leaf.words.get(at)?;
    let count = ((h >> 12) & 0xF) as usize + 1;
    if leaf.words.len() < at + 1 + count {
        return None;
    }
    let quad = kind & 2 != 0;
    let (mask, s1, s2) = if quad { (0x3FFu32, 10, 19) } else { (0x7FFu32, 11, 20) };
    let mut run = (h >> 16) << 10;
    let mut corners = Vec::with_capacity(count);
    for p in 0..count {
        let pk = leaf.words[at + 1 + p];
        run += pk & mask;
        corners.push([run, run + ((pk >> s1) & 0x1FF) + 1, run + ((pk >> s2) & 0x1FF) + 1]);
    }
    Some(PolyEntries { surfprop: (h & 0xFFF) as u16, dwords: 1 + count, corners, quad })
}

/// The corner a parallelogram's three stored corners imply. The engine forms it
/// in single precision, so a double-precision reference diverges from it.
fn implied_corner(a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    let f = |v: DVec3| Vec3::new(v.x as f32, v.y as f32, v.z as f32);
    DVec3::from(f(b) + f(c) - f(a))
}

/// The corner triples a polygon leaf names, resolved through `src`. An entry
/// whose corner does not address the pool is dropped whole; skipping one corner
/// would shift every triangle after it.
fn poly_triangles(src: &VertexSource<'_>, e: &PolyEntries) -> Vec<[DVec3; 3]> {
    let mut out = Vec::with_capacity(e.corners.len() * if e.quad { 2 } else { 1 });
    for c in &e.corners {
        let (Some(a), Some(b), Some(c0)) = (src.at(c[0]), src.at(c[1]), src.at(c[2])) else { continue };
        out.push([a, b, c0]);
        if e.quad {
            out.push([c0, b, implied_corner(a, b, c0)]);
        }
    }
    out
}

/// The triangles a polygon leaf at `at` holds.
fn poly_run(leaf: &BvhLeafData, kind: u32, at: usize, src: &VertexSource<'_>) -> Option<(u16, usize, Vec<[DVec3; 3]>)> {
    let e = poly_entries(leaf, kind, at)?;
    let tris = poly_triangles(src, &e);
    Some((e.surfprop, e.dwords, tris))
}

/// The corner triples a polygon leaf stores, exposed for the parity test. A
/// parallelogram entry stores three of its four corners.
pub fn poly_leaf_corners(leaf: &BvhLeafData, kind: u32, at: usize) -> Option<(u16, Vec<[u32; 3]>)> {
    let e = poly_entries(leaf, kind, at)?;
    Some((e.surfprop, e.corners))
}

#[allow(clippy::too_many_arguments)]
fn read_leaf(
    leaf: &BvhLeafData,
    verts: &[[f32; 3]],
    raw: &[i16],
    m: &Model,
    kind: u32,
    at: usize,
    contents: u32,
    smin: DVec3,
    smax: DVec3,
    out: &mut Vec<DecodedLeaf>,
) -> Result<(), CollError> {
    if (CHILD_POLY..=CHILD_POLY_MAX).contains(&kind) {
        // Bit 0 of the child type picks the width the vertex region is read
        // at; the same bytes are float32 for the even types and int16 for the
        // odd ones, both quantised against the owning model.
        let src = if kind & 1 != 0 {
            VertexSource::Quantised {
                origin: DVec3::new(m.origin[0] as f64, m.origin[1] as f64, m.origin[2] as f64),
                scale: m.decode_scale as f64 * DECODE_SCALE_UNIT,
                raw,
            }
        } else {
            VertexSource::Pool { verts, base: m.vert_index.max(0) as usize }
        };
        let Some((surfprop, _, triangles)) = poly_run(leaf, kind, at, &src) else { return Ok(()) };
        out.push(DecodedLeaf {
            kind,
            contents,
            surfprop,
            bounds_min: smin,
            bounds_max: smax,
            prop_index: 0,
            triangles,
        });
        return Ok(());
    }
    match kind {
        CHILD_HULL => {
            let Ok(h) = decode_hull(leaf, at) else { return Ok(()) };
            let scale = h.scale as f64 * DECODE_SCALE_UNIT;
            let origin = DVec3::new(h.origin[0] as f64, h.origin[1] as f64, h.origin[2] as f64);
            let points: Vec<DVec3> = h
                .verts
                .iter()
                .map(|q| origin + DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64) * scale)
                .collect();
            // A hull's first table is triangle leaves and its second is
            // parallelogram leaves; both index the hull's own corners.
            let mut tris: Vec<[DVec3; 3]> = Vec::new();
            let mut off = at + h.header_dwords as usize;
            let mut surface: Option<u16> = None;
            let sets = (0..h.tri_set_count).map(|_| CHILD_POLY).chain((0..h.quad_set_count).map(|_| CHILD_POLY_QUAD));
            for kind in sets {
                let Some((sp, dwords, t)) = poly_run(leaf, kind, off, &VertexSource::Local { points: &points })
                else {
                    break;
                };
                surface.get_or_insert(sp);
                tris.extend(t);
                off += dwords;
            }
            out.push(DecodedLeaf {
                kind,
                contents,
                // A hull's faces each name a surface; the first one stands for
                // the hull.
                surfprop: surface.unwrap_or(0),
                bounds_min: smin,
                bounds_max: smax,
                prop_index: 0,
                triangles: tris,
            });
        }
        CHILD_PROP => {
            // The prop index is the low 16 bits; shipped maps keep a flag
            // above it, so the whole word is carried.
            let p = leaf.word(at).unwrap_or(0);
            out.push(DecodedLeaf {
                kind,
                contents,
                surfprop: 0,
                bounds_min: smin,
                bounds_max: smax,
                prop_index: p,
                triangles: Vec::new(),
            });
        }
        _ => {}
    }
    Ok(())
}

/// Every model's collision triangles, world space.
pub type ModelTriangles = Vec<(u32, Vec<[DVec3; 3]>)>;

pub fn decode_collision(bsp: &Bsp) -> Result<ModelTriangles, CollError> {
    let c = Collider::load(bsp)?;
    let mut out = Vec::with_capacity(c.models.len());
    for (i, m) in c.models.iter().enumerate() {
        if !model_has_collision(m) {
            out.push((i as u32, Vec::new()));
            continue;
        }
        let leaves = c.leaves(i)?;
        let tris: Vec<[DVec3; 3]> = leaves.iter().flat_map(|l| l.triangles.iter().copied()).collect();
        out.push((i as u32, tris));
    }
    Ok(out)
}

// ---- the `*coll` standalone model -------------------------------------------

/// Which engine reads a `*coll` blob; the two have different headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollTarget {
    /// S21 client, header layout v121.
    Client,
    /// S3 dedicated server, header layout v8.
    Dedi,
}

impl CollTarget {
    pub fn header_size(self) -> usize {
        match self {
            CollTarget::Client => 16 + 40,
            CollTarget::Dedi => 16 + 32,
        }
    }
}

/// A standalone collision model, as a brush entity's `*coll` keys carry it.
#[derive(Clone, Debug, PartialEq)]
pub struct CollModel {
    pub target: CollTarget,
    pub fields: ModelFields,
    pub contents: Vec<u32>,
    pub surfprops: Vec<(u16, u8, u8, u32)>,
    pub names: Vec<u8>,
    pub verts: Vec<[f32; 3]>,
    pub leaf: Vec<u32>,
    pub nodes: Vec<u8>,
}

impl CollModel {
    /// The blob bytes, laid out the way the target's loader reads them.
    pub fn to_blob(&self) -> Vec<u8> {
        let mut surfprops = Vec::with_capacity(self.surfprops.len() * 8);
        for (f, t, c, n) in &self.surfprops {
            surfprops.extend_from_slice(&f.to_le_bytes());
            surfprops.push(*t);
            surfprops.push(*c);
            surfprops.extend_from_slice(&n.to_le_bytes());
        }
        let mut contents = Vec::with_capacity(self.contents.len() * 4);
        for m in &self.contents {
            contents.extend_from_slice(&m.to_le_bytes());
        }
        let sp = self.target.header_size();
        let cm = sp + surfprops.len();
        let sn = cm + contents.len();
        let mut vt = sn + self.names.len();
        vt += (4 - vt % 4) % 4;
        let lf = vt + self.verts.len() * 12;
        let mut nd = lf + self.leaf.len() * 4;
        nd += (16 - nd % 16) % 16;
        let mut blob = vec![0u8; nd + self.nodes.len()];
        let head: [i32; 4] = [cm as i32, sp as i32, sn as i32, 1];
        for (i, v) in head.iter().enumerate() {
            blob[i * 4..(i + 1) * 4].copy_from_slice(&v.to_le_bytes());
        }
        let tail: [i32; 6] = [
            self.fields.flags as i32,
            nd as i32,
            vt as i32,
            lf as i32,
            lf as i32,
            0,
        ];
        match self.target {
            CollTarget::Client => {
                for (i, v) in tail.iter().enumerate() {
                    blob[16 + i * 4..20 + i * 4].copy_from_slice(&v.to_le_bytes());
                }
                for (i, v) in self.fields.origin.iter().chain([&self.fields.decode_scale]).enumerate() {
                    blob[40 + i * 4..44 + i * 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            CollTarget::Dedi => {
                for (i, v) in tail[..4].iter().enumerate() {
                    blob[16 + i * 4..20 + i * 4].copy_from_slice(&v.to_le_bytes());
                }
                for (i, v) in self.fields.origin.iter().chain([&self.fields.decode_scale]).enumerate() {
                    blob[32 + i * 4..36 + i * 4].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        blob[sp..cm].copy_from_slice(&surfprops);
        blob[cm..sn].copy_from_slice(&contents);
        blob[sn..sn + self.names.len()].copy_from_slice(&self.names);
        let mut vw = Vec::with_capacity(self.verts.len() * 12);
        for v in &self.verts {
            for c in v {
                vw.extend_from_slice(&c.to_le_bytes());
            }
        }
        blob[vt..vt + vw.len()].copy_from_slice(&vw);
        let mut at = lf;
        for w in &self.leaf {
            blob[at..at + 4].copy_from_slice(&w.to_le_bytes());
            at += 4;
        }
        blob[nd..nd + self.nodes.len()].copy_from_slice(&self.nodes);
        blob
    }

    /// The `*collN` key values the blob is written as.
    pub fn to_keys(&self) -> Vec<String> {
        let blob = self.to_blob();
        blob_chunks(&blob)
    }

    /// The triangles the blob's tree decodes to.
    pub fn triangles(&self) -> Vec<[DVec3; 3]> {
        let m = Model {
            mins: [0.0; 3],
            maxs: [0.0; 3],
            first_mesh: 0,
            num_meshes: 0,
            bvh_node: 0,
            bvh_leaf: 0,
            vert_index: 0,
            flags: self.fields.flags,
            origin: self.fields.origin,
            decode_scale: self.fields.decode_scale,
        };
        let nodes: Vec<BvhNode> = parse_nodes(&self.nodes).unwrap_or_default();
        let leaf = BvhLeafData { words: self.leaf.clone() };
        let raw = f32_pool_as_i16(&self.verts);
        let mut out: Vec<DecodedLeaf> = Vec::new();
        let mut stack: Vec<usize> = vec![0];
        let mut seen: Vec<bool> = vec![false; nodes.len()];
        while let Some(ni) = stack.pop() {
            let Some(node) = nodes.get(ni) else { continue };
            if std::mem::replace(&mut seen[ni], true) {
                continue;
            }
            for slot in 0..4 {
                let kind = node.child_kind(slot);
                let at = node.child_index(slot) as usize;
                match kind {
                    CHILD_EMPTY => {}
                    CHILD_NODE => stack.push(at),
                    CHILD_BUNDLE => {
                        let Some(&count) = leaf.words.get(at) else { continue };
                        let mut pay = at + 1 + count as usize;
                        for k in 0..count as usize {
                            let Some(&desc) = leaf.words.get(at + 1 + k) else { break };
                            let sub = (desc >> 8) & 0xFF;
                            let size = (desc >> 16) as usize;
                            let _ = read_leaf(
                                &leaf, &self.verts, &raw, &m, sub, pay, 0, DVec3::ZERO, DVec3::ZERO, &mut out,
                            );
                            pay += size;
                        }
                    }
                    _ => {
                        let _ = read_leaf(
                            &leaf, &self.verts, &raw, &m, kind, at, 0, DVec3::ZERO, DVec3::ZERO, &mut out,
                        );
                    }
                }
            }
        }
        out.into_iter().flat_map(|l| l.triangles).collect()
    }
}

/// The `*collN` key values a blob is written as.
pub fn blob_chunks(blob: &[u8]) -> Vec<String> {
    blob.chunks(COLL_CHUNK)
        .enumerate()
        .map(|(i, c)| format!("*coll{} \"{}\"", i, base64(c)))
        .collect()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, which is what the keyvalues carry.
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Inverse of [`base64`], with the padding the encoder wrote.
pub fn base64_decode(text: &str) -> Result<Vec<u8>, CollError> {
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for ch in text.bytes() {
        if ch == b'=' || ch.is_ascii_whitespace() {
            continue;
        }
        let v = B64.iter().position(|&c| c == ch).ok_or(CollError::CollBase64)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// Reads a blob back into its tables and tree.
pub fn parse_coll_blob(blob: &[u8], target: CollTarget) -> Result<CollModel, CollError> {
    if blob.len() < target.header_size() {
        return Err(CollError::CollShort(blob.len()));
    }
    let i32_at = |o: usize| -> Result<i32, CollError> {
        let b: [u8; 4] = blob.get(o..o + 4).and_then(|s| s.try_into().ok()).ok_or(CollError::CollTruncated(o))?;
        Ok(i32::from_le_bytes(b))
    };
    let f32_at = |o: usize| -> Result<f32, CollError> {
        let b: [u8; 4] = blob.get(o..o + 4).and_then(|s| s.try_into().ok()).ok_or(CollError::CollTruncated(o))?;
        Ok(f32::from_le_bytes(b))
    };
    let cm = i32_at(0)? as usize;
    let sp = i32_at(4)? as usize;
    let sn = i32_at(8)? as usize;
    if i32_at(12)? != 1 {
        return Err(CollError::CollRange(1, blob.len()));
    }
    if sp < target.header_size() || cm < sp || sn < cm {
        return Err(CollError::CollTruncated(cm));
    }
    let flags = i32_at(16)? as u32;
    let nd = i32_at(20)? as usize;
    let vt = i32_at(24)? as usize;
    let lf = i32_at(28)? as usize;
    // The quantisation floats follow the fixed header fields, whose width is
    // what tells the two targets apart.
    let quant = if target == CollTarget::Client { 40 } else { 32 };
    for (o, n) in [(sp, cm - sp), (cm, sn - cm), (sn, vt - sn), (vt, lf - vt), (lf, nd - lf), (nd, blob.len() - nd)] {
        if n > blob.len() {
            return Err(CollError::CollRange(o as u32, n));
        }
        if o > blob.len() {
            return Err(CollError::CollTruncated(o));
        }
    }
    if vt < sn || lf < vt || nd < lf || nd > blob.len() {
        return Err(CollError::CollTruncated(nd));
    }
    let surfprops: Vec<(u16, u8, u8, u32)> = (0..(cm - sp) / 8)
        .map(|i| {
            let o = sp + i * 8;
            let flags = u16::from_le_bytes([blob[o], blob[o + 1]]);
            (flags, blob[o + 2], blob[o + 3], u32::from_le_bytes([blob[o + 4], blob[o + 5], blob[o + 6], blob[o + 7]]))
        })
        .collect();
    let contents: Vec<u32> = (0..(sn - cm) / 4)
        .map(|i| u32::from_le_bytes(blob[cm + i * 4..cm + i * 4 + 4].try_into().unwrap_or([0; 4])))
        .collect();
    // The name section is zero padded to the vertex table's alignment, and a
    // blob carries one NUL-terminated name, so the padding is not part of it.
    let mut names = blob[sn..vt].to_vec();
    if let Some(end) = names.iter().position(|&b| b == 0) {
        names.truncate(end + 1);
    }
    let verts: Vec<[f32; 3]> = (0..(lf - vt) / 12)
        .map(|i| {
            let o = vt + i * 12;
            [
                f32::from_le_bytes(blob[o..o + 4].try_into().unwrap_or([0; 4])),
                f32::from_le_bytes(blob[o + 4..o + 8].try_into().unwrap_or([0; 4])),
                f32::from_le_bytes(blob[o + 8..o + 12].try_into().unwrap_or([0; 4])),
            ]
        })
        .collect();
    let leaf: Vec<u32> = (0..(nd - lf) / 4)
        .map(|i| u32::from_le_bytes(blob[lf + i * 4..lf + i * 4 + 4].try_into().unwrap_or([0; 4])))
        .collect();
    Ok(CollModel {
        target,
        fields: ModelFields {
            bvh_node: 0,
            bvh_leaf: 0,
            vert_index: 0,
            flags,
            origin: [f32_at(quant)?, f32_at(quant + 4)?, f32_at(quant + 8)?],
            decode_scale: f32_at(quant + 12)?,
        },
        contents,
        surfprops,
        names,
        verts,
        leaf,
        nodes: blob[nd..].to_vec(),
    })
}

/// Builds a `*coll` model from plane-defined brushes, one hull leaf each. The
/// surface triple is the contents mask, the surface flags and the name blob
/// entry the engine reads a trigger or a volume with.
pub fn volume_blob(
    brushes: &[Vec<crate::csg::DPlane>],
    surface: (u32, u16, &[u8]),
    target: CollTarget,
) -> Result<CollModel, CollError> {
    let mut writer = BvhWriter::new(vec![surface.0]);
    let sp = writer.surfprop_index(surface.1, 0, 0, 0)?;
    let mut items: Vec<Item> = Vec::new();
    for planes in brushes {
        let verts = crate::csg::hull_vertices_planes(planes).ok_or(CollError::NotASolid(0))?;
        let normals: Vec<DVec3> = planes.iter().map(|p| p.normal).collect();
        items.push(writer.hull_item(0, &verts, &normals, &vec![sp; normals.len()])?);
    }
    if items.is_empty() {
        return Err(CollError::Empty(0));
    }
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for it in &items {
        lo = lo.min(it.mins);
        hi = hi.max(it.maxs);
    }
    let step = decode_step_for(lo, hi)?;
    let fields = writer.build_model(items, (lo + hi) * 0.5, step)?;
    let mut name = surface.2.to_vec();
    name.push(0);
    Ok(CollModel {
        target,
        fields,
        contents: writer.contents_masks,
        surfprops: writer.surfprops,
        names: name,
        verts: writer.verts,
        leaf: writer.leaf,
        nodes: writer.nodes,
    })
}

/// The contents mask, surface flags and name a volume class collides with.
pub fn volume_surface(classname: &str) -> Option<(u32, u16, &'static [u8])> {
    match classname {
        "envmap_volume" => Some((0x10E3_1240, 0x610, b"TOOLS\\TOOLSENVMAPVOLUME")),
        "light_environment_volume" | "light_probe_volume" | "trigger_soundscape" => {
            Some((0x00EB_1280, 0x400, b"TOOLS\\TOOLSTRIGGER"))
        }
        c if c.starts_with("trigger_") => Some((0x00EB_1280, 0x400, b"TOOLS\\TOOLSTRIGGER")),
        _ => None,
    }
}

/// A poly leaf header, exposed for the parity test.
pub fn poly_leaf_header(surfprop: u16, count: usize, base: u32) -> u32 {
    (((count - 1) as u32) << 12) | (surfprop as u32 & 0xFFF) | (base << 16)
}

/// A poly leaf triangle word, exposed for the parity test.
pub fn poly_tri_word(d0: u32, d1: u32, d2: u32) -> u32 {
    (d0 & 0x7FF) | ((d1 & 0x1FF) << 11) | ((d2 & 0x1FF) << 20) | (EDGE_FLAGS_ALL << 29)
}

/// Reads a poly triangle word, exposed for the parity test.
pub fn poly_tri_read(w: u32) -> (u32, u32, u32, u32) {
    let t = PolyTriangle { packed: w };
    let (a, b, c) = t.deltas();
    (a, b, c, t.edge_flags())
}
