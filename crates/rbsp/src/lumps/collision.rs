//! Collision lumps: MODELS, CONTENTS_MASKS, SURFACE_PROPERTIES, BVH nodes,
//! the BVH leaf stream and the legacy packed-vertex list.

use crate::lumps::records::{fixed_codec, parse_fixed, write_fixed};
use crate::{Error, Result, Version};

pub const MODEL_SIZE: usize = 64;
pub const NODE_SIZE: usize = 64;
pub const MAX_POLYS_PER_LEAF: usize = 16;
pub const EDGE_FLAGS_ALL: u32 = 7;

pub const CHILD_NODE: u32 = 0;
pub const CHILD_EMPTY: u32 = 1;
pub const CHILD_BUNDLE: u32 = 3;
pub const CHILD_POLY: u32 = 4;
pub const CHILD_HULL: u32 = 8;
pub const CHILD_PROP: u32 = 9;
/// Highest kind the engine's leaf dispatch knows.
pub const CHILD_KIND_MAX: u32 = 9;

/// What a leaf visitor asks the walk to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Walk {
    Continue,
    Stop,
}

/// Per-node marks for a leaf walk, stamped with a generation so one buffer
/// serves every model of a map without being cleared between them.
#[derive(Clone, Debug, Default)]
pub struct LeafVisited {
    stamp: Vec<u32>,
    generation: u32,
}

impl LeafVisited {
    pub fn new() -> Self {
        Self::default()
    }

    fn begin(&mut self, nodes: usize) {
        if self.stamp.len() != nodes {
            self.stamp.clear();
            self.stamp.resize(nodes, 0);
            self.generation = 0;
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.stamp.iter_mut().for_each(|s| *s = 0);
            self.generation = 1;
        }
    }

    /// True the first time a node is seen in the current walk.
    fn mark(&mut self, n: usize) -> bool {
        if self.stamp[n] == self.generation {
            return false;
        }
        self.stamp[n] = self.generation;
        true
    }
}

/// LUMP_MODELS at v47/v51. The first eight fields are the render mesh range;
/// the next four address this model's slice of the BVH trees and the vertex
/// lump, and the last four are the quantisation its node bounds are read with.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Model {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub first_mesh: u32,
    pub num_meshes: u32,
    /// Index of this model's first BVH node, or -1 for a model with no collision.
    pub bvh_node: i32,
    /// First dword of this model's leaf run.
    pub bvh_leaf: i32,
    /// First vertex of this model's collision vertices.
    pub vert_index: i32,
    pub flags: u32,
    pub origin: [f32; 3],
    pub decode_scale: f32,
}

impl Model {
    /// A model with no meshes and no tree, stored the way shipped maps store
    /// one: staging_mu1's 75 empty models all carry an inverted `FLT_MAX` box,
    /// node 0, leaf 0 and a zero decode scale.
    pub fn empty() -> Self {
        Self { mins: [f32::MAX; 3], maxs: [-f32::MAX; 3], ..Default::default() }
    }

    /// A tree the model owns. An empty model names node 0 like the world does,
    /// so the inverted box is what says it has none.
    pub fn has_collision(&self) -> bool {
        self.bvh_node >= 0 && self.mins[0] <= self.maxs[0]
    }

    /// A node slot's minimum corner, dequantised the way the engine does it.
    pub fn slot_mins(&self, q: [i16; 3]) -> [f32; 3] {
        [
            self.origin[0] + q[0] as f32 * self.decode_scale,
            self.origin[1] + q[1] as f32 * self.decode_scale,
            self.origin[2] + q[2] as f32 * self.decode_scale,
        ]
    }

    /// A node slot's maximum corner, dequantised.
    pub fn slot_maxs(&self, q: [i16; 3]) -> [f32; 3] {
        [
            self.origin[0] + q[0] as f32 * self.decode_scale,
            self.origin[1] + q[1] as f32 * self.decode_scale,
            self.origin[2] + q[2] as f32 * self.decode_scale,
        ]
    }
}

fixed_codec!(parse_models, write_models, Model, 0x0E);

/// LUMP_CONTENTS_MASKS: one bitmask per contents index, gating every BVH node
/// and leaf that names it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ContentMask {
    pub mask: u32,
}

fixed_codec!(parse_content_masks, write_content_masks, ContentMask, 0x10);

/// LUMP_SURFACE_PROPERTIES: what a collision face is made of. `name` is a byte
/// offset into the 0x0F name blob, `contents` an index into 0x10.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SurfaceProp {
    pub flags: u16,
    pub surface_type: u8,
    pub contents: u8,
    pub name: u32,
}

fixed_codec!(parse_surface_props, write_surface_props, SurfaceProp, 0x11);

/// LUMP_BVH_NODES. `min_max[axis][0|1][slot]` are quantised against the owning
/// model's `origin`/`decode_scale`; `meta[i]` holds a child index in bits 8 and
/// up, and the child's kind in the low nibbles of `meta[2]`/`meta[3]`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BvhNode {
    pub min_max: [i16; 24],
    pub meta: [u32; 4],
}

impl BvhNode {
    pub fn contents_index(&self) -> u8 {
        (self.meta[0] & 0xFF) as u8
    }

    pub fn child_index(&self, slot: usize) -> u32 {
        self.meta[slot] >> 8
    }

    pub fn child_kind(&self, slot: usize) -> u32 {
        let packed = if slot < 2 { self.meta[2] } else { self.meta[3] };
        (packed >> (4 * (slot % 2))) & 0xF
    }

    pub fn slot_mins(&self, slot: usize) -> [i16; 3] {
        [self.min_max[slot], self.min_max[4 + slot], self.min_max[8 + slot]]
    }

    pub fn slot_maxs(&self, slot: usize) -> [i16; 3] {
        [self.min_max[12 + slot], self.min_max[16 + slot], self.min_max[20 + slot]]
    }
}

fixed_codec!(parse_bvh_nodes, write_bvh_nodes, BvhNode, 0x12);

/// LUMP_PACKED_VERTICES, the 3x int8 position plus pad the pre-BVH4 collision
/// used. It is dead weight at v47 and later, but the dedicated server still
/// reads the slot.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PackVert {
    pub x: i8,
    pub y: i8,
    pub z: i8,
    pub pad: u8,
}

fixed_codec!(parse_pack_verts, write_pack_verts, PackVert, 0x14);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PolyTriangle {
    pub packed: u32,
}

impl PolyTriangle {
    /// The three deltas from the running vertex cursor.
    pub fn deltas(&self) -> (u32, u32, u32) {
        (self.packed & 0x7FF, (self.packed >> 11) & 0x1FF, (self.packed >> 20) & 0x1FF)
    }

    pub fn edge_flags(&self) -> u32 {
        self.packed >> 29
    }
}

/// One decoded poly leaf: a run of triangles whose vertex indices are stored as
/// deltas from a running cursor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PolyLeaf {
    pub surface_prop: u16,
    /// Absolute index of the first triangle's first vertex.
    pub first_vertex: u32,
    pub triangles: [PolyTriangle; MAX_POLYS_PER_LEAF],
    pub triangle_count: u32,
    /// Dwords this leaf occupies, header included.
    pub dwords: u32,
}

impl PolyLeaf {
    /// Runs the leaf's vertex indices through `f`, three per triangle, without
    /// materialising them.
    pub fn for_each_vertex(&self, mut f: impl FnMut(u32)) {
        let mut running = self.first_vertex;
        for t in self.triangles.iter().take(self.triangle_count as usize) {
            let (d0, d1, d2) = t.deltas();
            running += d0;
            f(running);
            f(running + d1 + 1);
            f(running + d2 + 1);
        }
    }

    /// The leaf's three vertex indices, one triangle at a time.
    pub fn vertices(&self) -> Vec<[u32; 3]> {
        let mut out = Vec::with_capacity(self.triangle_count as usize * 3);
        let mut tri = Vec::with_capacity(3);
        self.for_each_vertex(|idx| {
            tri.push(idx);
            if tri.len() == 3 {
                out.push([tri[0], tri[1], tri[2]]);
                tri.clear();
            }
        });
        out
    }
}

/// A convex volume leaf: quantised vertices plus a plane triple per face, and
/// the tri sets those faces are drawn with.
#[derive(Clone, Debug, PartialEq)]
pub struct HullLeaf {
    pub origin: [f32; 3],
    pub scale: f32,
    pub verts: Vec<[i16; 3]>,
    pub planes: Vec<[u8; 3]>,
    pub tri_set_count: u8,
    pub quad_set_count: u8,
    /// Dwords the header and the two tables take, padding included.
    pub header_dwords: u32,
}

/// LUMP_BVH_LEAF_DATA is a dword stream only the node tree can navigate, so it
/// is carried verbatim and decoded on demand.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BvhLeafData {
    pub words: Vec<u32>,
}

impl BvhLeafData {
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn word(&self, at: usize) -> Option<u32> {
        self.words.get(at).copied()
    }

    fn byte(&self, at: usize) -> Option<u8> {
        self.words.get(at / 4).map(|w| (w >> (8 * (at % 4))) as u8)
    }

    fn f32_at(&self, dword: usize) -> Result<f32> {
        self.words
            .get(dword)
            .map(|w| f32::from_bits(*w))
            .ok_or(Error::Invalid { id: 0x13, msg: "leaf runs past the end of the stream".to_string() })
    }

    /// Decodes the poly leaf at `at`, or reports why it does not fit.
    pub fn poly_leaf(&self, at: usize) -> Result<PolyLeaf> {
        let h = *self
            .words
            .get(at)
            .ok_or(Error::Invalid { id: 0x13, msg: "poly leaf past the end of the stream".to_string() })?;
        let count = ((h >> 12) & 0xF) as usize + 1;
        if count > MAX_POLYS_PER_LEAF {
            return Err(Error::Invalid { id: 0x13, msg: format!("poly leaf claims {count} triangles") });
        }
        if at + 1 + count > self.words.len() {
            return Err(Error::Truncated { what: "poly leaf triangles", need: at + 1 + count, have: self.words.len() });
        }
        let mut triangles = [PolyTriangle::default(); MAX_POLYS_PER_LEAF];
        for (i, slot) in triangles.iter_mut().enumerate().take(count) {
            *slot = PolyTriangle { packed: self.words[at + 1 + i] };
        }
        Ok(PolyLeaf {
            surface_prop: (h & 0xFFF) as u16,
            first_vertex: (h >> 16) << 10,
            triangles,
            triangle_count: count as u32,
            dwords: (1 + count) as u32,
        })
    }

    /// Decodes the convex hull leaf at `at`.
    pub fn hull_leaf(&self, at: usize) -> Result<HullLeaf> {
        let head = self.f32_at(at)? as u32;
        let nv = (head & 0xFF) as usize;
        let npl = ((head >> 8) & 0xFF) as usize;
        let nts = ((head >> 16) & 0xFF) as usize;
        let nqs = ((head >> 24) & 0xFF) as usize;
        let header_bytes = 20 + nv * 6 + npl * 3;
        let header_dwords = header_bytes.div_ceil(4);
        let end_byte = (at + header_dwords) * 4;
        if end_byte > self.words.len() * 4 {
            return Err(Error::Truncated { what: "hull leaf tables", need: end_byte, have: self.words.len() * 4 });
        }
        let mut verts = Vec::with_capacity(nv);
        let vert_base = at * 4 + 20;
        for i in 0..nv {
            let b = vert_base + i * 6;
            verts.push([
                i16::from_le_bytes([self.byte(b).unwrap_or(0), self.byte(b + 1).unwrap_or(0)]),
                i16::from_le_bytes([self.byte(b + 2).unwrap_or(0), self.byte(b + 3).unwrap_or(0)]),
                i16::from_le_bytes([self.byte(b + 4).unwrap_or(0), self.byte(b + 5).unwrap_or(0)]),
            ]);
        }
        let plane_base = vert_base + nv * 6;
        let mut planes = Vec::with_capacity(npl);
        for i in 0..npl {
            let b = plane_base + i * 3;
            planes.push([
                self.byte(b).unwrap_or(0),
                self.byte(b + 1).unwrap_or(0),
                self.byte(b + 2).unwrap_or(0),
            ]);
        }
        Ok(HullLeaf {
            origin: [self.f32_at(at + 1)?, self.f32_at(at + 2)?, self.f32_at(at + 3)?],
            scale: self.f32_at(at + 4)?,
            verts,
            planes,
            tri_set_count: nts as u8,
            quad_set_count: nqs as u8,
            header_dwords: header_dwords as u32,
        })
    }

    /// The static prop index a `CHILD_PROP` leaf carries. Shipped maps put
    /// something else in the byte above it, so only the low 16 bits are the
    /// index.
    pub fn prop_index(&self, at: usize) -> Result<u32> {
        Ok(self.word(at).ok_or(Error::Invalid {
            id: 0x13,
            msg: "prop leaf past the end of the stream".to_string(),
        })? & 0xFFFF)
    }
}

/// Whether a bounded walk reached the end of the tree or ran out of allowance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkLimit {
    Complete,
    /// The tree was left partly unexplored; the allowance was the smaller of
    /// the two, not the tree.
    Exhausted,
}

/// One unit of walking allowance per node and one per leaf visited. A corrupt
/// tree can make every model reach the whole node table, so a caller that has
/// to survive hostile input caps the total rather than the depth.
pub fn walk_allowance(nodes: usize, models: usize) -> usize {
    (16 * nodes + 4096).saturating_mul(models.clamp(1, 64))
}

impl BvhLeafData {
    /// Every leaf the model's node tree reaches, as `(kind, dword offset)`.
pub fn leaves_of(&self, nodes: &[BvhNode], model: &Model) -> Result<Vec<(u32, usize)>> {
    let mut out = Vec::new();
    let mut visited = LeafVisited::new();
    self.walk_leaves(nodes, model, &mut visited, |kind, at| {
        out.push((kind, at));
        Ok(Walk::Continue)
    })?;
    Ok(out)
}

/// Streams every leaf the model's node tree reaches. A caller that checks
/// one model after another reuses `visited` so the mark array is built once
/// per map rather than once per model.
pub fn walk_leaves(
    &self,
    nodes: &[BvhNode],
    model: &Model,
    visited: &mut LeafVisited,
    f: impl FnMut(u32, usize) -> Result<Walk>,
) -> Result<()> {
    let mut unlimited = usize::MAX;
    self.walk_leaves_bounded(nodes, model, visited, &mut unlimited, f).map(|_| ())
}

/// [`walk_leaves`] with a shared allowance, so a corrupt map cannot turn the
/// per-model walk into work quadratic in the node count.
pub fn walk_leaves_bounded(
    &self,
    nodes: &[BvhNode],
    model: &Model,
    visited: &mut LeafVisited,
    allowance: &mut usize,
    mut f: impl FnMut(u32, usize) -> Result<Walk>,
) -> Result<WalkLimit> {
    if !model.has_collision() {
        return Ok(WalkLimit::Complete);
    }
    visited.begin(nodes.len());
    let base = model.bvh_leaf.max(0) as usize;
    let mut stack = vec![model.bvh_node as usize];
    // A node several slots point at is shared, which shipped maps do; the
    // bounds of every reference are still checked.
    while let Some(n) = stack.pop() {
        if *allowance == 0 {
            return Ok(WalkLimit::Exhausted);
        }
        *allowance -= 1;
        if n >= nodes.len() {
            return Err(Error::Invalid { id: 0x12, msg: format!("bvh node {n} is out of range") });
        }
        if !visited.mark(n) {
            continue;
        }
        let node = &nodes[n];
        for slot in 0..4 {
            let kind = node.child_kind(slot);
            let at = node.child_index(slot) as usize;
            match kind {
                CHILD_EMPTY => {}
                CHILD_NODE => stack.push(at),
                CHILD_BUNDLE => {
                        let start = leaf_at(&self.words, at, base, 1)?;
                    let count = self.words[start] as usize;
                    if start + 1 + count > self.words.len() {
                        return Err(Error::Truncated {
                            what: "bundle descriptors",
                            need: start + 1 + count,
                            have: self.words.len(),
                        });
                    }
                    let mut pay = start + 1 + count;
                    for k in 0..count {
                        *allowance = allowance.saturating_sub(1);
                        if *allowance == 0 {
                            return Ok(WalkLimit::Exhausted);
                        }
                        let desc = self.words[start + 1 + k];
                        if f((desc >> 8) & 0xFF, pay)? == Walk::Stop {
                            return Ok(WalkLimit::Complete);
                        }
                        pay += (desc >> 16) as usize;
                    }
                }
                _ => {
                    let start = leaf_at(&self.words, at, base, 0)?;
                    *allowance = allowance.saturating_sub(1);
                    if *allowance == 0 {
                        return Ok(WalkLimit::Exhausted);
                    }
                    if f(kind, start)? == Walk::Stop {
                        return Ok(WalkLimit::Complete);
                    }
                }
            }
        }
    }
    Ok(WalkLimit::Complete)
}
}

/// A leaf reference is an absolute dword index into the stream. A map built
/// from scratch may count from its own model's first leaf instead, so both
/// readings are tried, absolute first.
fn leaf_at(words: &[u32], at: usize, base: usize, need: usize) -> Result<usize> {
    for start in [at, base + at] {
        if start.saturating_add(need) <= words.len() {
            return Ok(start);
        }
    }
    Err(Error::Invalid {
        id: 0x13,
        msg: format!("leaf at dword {at} needs {need} words, past a {}-word stream", words.len()),
    })
}

pub fn parse_bvh_leaf(data: &[u8], _version: Version) -> Result<BvhLeafData> {
    Ok(BvhLeafData { words: parse_fixed(0x13, data)? })
}

pub fn write_bvh_leaf(data: &BvhLeafData, _version: Version) -> Vec<u8> {
    write_fixed(&data.words)
}
