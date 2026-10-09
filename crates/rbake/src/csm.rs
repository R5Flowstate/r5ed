//! Cascaded shadow map trees and the world shadow meshes they draw.
//!
//! The AABBs live in the environment's light frame, whose rows are
//! `csm_light_basis` of the shadow direction. Every world shadow mesh is
//! referenced from the deepest node whose light-space box contains it, growing
//! the environment's root when no node does, because a mesh no node references
//! is never drawn.

use glam::Vec3;
use rbsp::lumps::csm as rcsm;
use rbsp::lumps::lighting as lmp;

use crate::codec::{CsmNodeRecord, ShadowAlphaVertex, ShadowMeshRecord};
use crate::error::{Error, Result};

/// The direction a map with no sun uses for its cascade, chosen off vertical
/// so the light frame stays defined.
pub const FALLBACK_SHADOW_DIRECTION: [f32; 3] = [0.3, 0.2, -1.0];

/// Rows of the frame the caster AABBs are stored in, `z` being the negated
/// shadow direction.
pub fn csm_light_basis(shadow_dir: Vec3) -> Result<[[f32; 3]; 3]> {
    let z = -shadow_dir.normalize_or_zero();
    if z.length_squared() < 0.5 {
        return Err(Error::lighting("the shadow direction has no length"));
    }
    let x = Vec3::Z.cross(z);
    if x.length_squared() < 1.0e-12 {
        return Err(Error::lighting("a vertical shadow direction leaves the CSM light frame undefined"));
    }
    let x = x.normalize();
    let y = z.cross(x).normalize();
    Ok([[x.x, x.y, x.z], [y.x, y.y, y.z], [z.x, z.y, z.z]])
}

/// One cascade's slice of the tree and the mesh list.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CsmEnv {
    pub root: u32,
    pub refs_begin: u32,
    pub mesh_begin: u32,
    pub root_end: u32,
    pub refs_end: u32,
    pub mesh_end: u32,
    pub direction: [f32; 3],
}

impl CsmEnv {
    pub fn to_record(&self) -> lmp::ShadowEnvironment {
        lmp::ShadowEnvironment {
            aabb_root: self.root,
            aabb_refs_begin: self.refs_begin,
            shadow_mesh_begin: self.mesh_begin,
            aabb_end: self.root_end,
            aabb_refs_end: self.refs_end,
            shadow_mesh_end: self.mesh_end,
            shadow_direction: self.direction,
        }
    }
}

/// The cascade trees plus the geometry they draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Csm {
    pub envs: Vec<CsmEnv>,
    pub nodes: Vec<CsmNodeRecord>,
    pub refs: Vec<u32>,
    pub totals: Vec<u32>,
    pub meshes: Vec<ShadowMeshRecord>,
    pub opaque: Vec<[f32; 3]>,
    pub alpha: Vec<ShadowAlphaVertex>,
    pub indices: Vec<u16>,
}

impl Csm {
    /// Every world shadow mesh is named by at least one reference.
    pub fn every_mesh_referenced(&self) -> bool {
        self.meshes
            .iter()
            .enumerate()
            .all(|(i, _)| self.refs.iter().any(|r| *r as usize == i))
    }

    /// A node's own reference count plus its subtree's.
    pub fn subtree_refs(&self, node: usize) -> u32 {
        let Some(n) = self.nodes.get(node) else { return 0 };
        let mut total = n.ref_count;
        for c in n.first_child..n.first_child + n.child_count {
            total += self.subtree_refs(c as usize);
        }
        total
    }

    /// The 0x7F records, in the engine's own struct.
    pub fn mesh_records(&self) -> Vec<rcsm::ShadowMesh> {
        self.meshes
            .iter()
            .map(|m| rcsm::ShadowMesh {
                first_vertex: m.first_vertex,
                triangle_count: m.triangle_count,
                draw_alpha: u16::from(!m.draw_alpha),
                unknown_0: m.tail,
            })
            .collect()
    }
}

/// One caster mesh: its vertices, its indices and whether it is translucent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShadowCaster {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u16>,
    pub alpha: bool,
}

/// The most vertices one shadow mesh can name.
pub const SHADOW_MESH_VERTEX_LIMIT: usize = u16::MAX as usize;

/// A surface's triangles as one caster per 16-bit chunk. A merged world
/// surface outgrows one mesh's index range, so triangles are packed greedily
/// and a vertex shared across a split appears in both chunks; every triangle
/// survives intact. Triangles naming a vertex the surface does not have are
/// dropped whole.
pub fn split_caster(positions: Vec<[f32; 3]>, indices: &[u32], alpha: bool) -> Vec<ShadowCaster> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut idx: Vec<u16> = Vec::new();
    let mut local: HashMap<u32, u16> = HashMap::new();
    for tri in indices.chunks_exact(3) {
        if tri.iter().any(|v| (*v as usize) >= positions.len()) {
            continue;
        }
        let fresh = tri.iter().filter(|v| !local.contains_key(v)).count();
        if verts.len() + fresh > SHADOW_MESH_VERTEX_LIMIT && !idx.is_empty() {
            out.push(ShadowCaster { positions: std::mem::take(&mut verts), indices: std::mem::take(&mut idx), alpha });
            local.clear();
        }
        for v in tri {
            let slot = *local.entry(*v).or_insert_with(|| {
                verts.push(positions[*v as usize]);
                (verts.len() - 1) as u16
            });
            idx.push(slot);
        }
    }
    if !idx.is_empty() {
        out.push(ShadowCaster { positions: verts, indices: idx, alpha });
    }
    out
}

/// A cascade tree that references nothing and draws nothing: the shape a map
/// with no bake still needs to be well formed.
pub fn empty_csm(world_mins: Vec3, world_maxs: Vec3, direction: [f32; 3]) -> Result<Csm> {
    let d = normalise(Vec3::new(direction[0], direction[1], direction[2]));
    let basis = csm_light_basis(d)?;
    let corner = CsmNodeRecord {
        mins: world_mins.to_array(),
        maxs: world_maxs.to_array(),
        ..CsmNodeRecord::default()
    };
    let (lo, hi) = bounds_of(&[corner], &basis);
    Ok(Csm {
        envs: vec![CsmEnv {
            root: 0,
            refs_begin: 0,
            mesh_begin: 0,
            root_end: 1,
            refs_end: 0,
            mesh_end: 0,
            direction: d.to_array(),
        }],
        nodes: vec![CsmNodeRecord {
            mins: lo.to_array(),
            maxs: hi.to_array(),
            ..CsmNodeRecord::default()
        }],
        refs: Vec::new(),
        totals: vec![0],
        meshes: Vec::new(),
        opaque: Vec::new(),
        alpha: Vec::new(),
        indices: Vec::new(),
    })
}

/// A mesh list flattened for the cascade builder: the vertex runs, the shared
/// index run, and each mesh's slice of it.
#[derive(Default)]
struct MeshSet {
    meshes: Vec<ShadowMeshRecord>,
    opaque: Vec<[f32; 3]>,
    alpha: Vec<ShadowAlphaVertex>,
    indices: Vec<u16>,
}

impl MeshSet {
    fn position(&self, mesh: &ShadowMeshRecord, index: usize) -> [f32; 3] {
        if mesh.draw_alpha {
            self.alpha
                .get(index)
                .map(|v| v.position)
                .unwrap_or([0.0; 3])
        } else {
            self.opaque.get(index).copied().unwrap_or([0.0; 3])
        }
    }

    /// Where a mesh's index slice starts: the meshes' triangle counts in
    /// order, which is how the engine walks the shared index run.
    fn index_start(&self, mesh: u32) -> usize {
        let mut at = 0usize;
        for (i, m) in self.meshes.iter().enumerate() {
            if i as u32 == mesh {
                return at;
            }
            at += m.triangle_count as usize * 3;
        }
        at
    }
}

/// Marks a reference that names a static prop by its place in the prop lump;
/// [`resolve_prop_refs`] turns it into the engine's reference once the map
/// knows its first static prop index.
pub const PROP_REF_MARK: u32 = 0x4000_0000;

/// A static prop the cascade draws: its prop-lump index and world box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PropCaster {
    pub lump_index: u32,
    pub mins: Vec3,
    pub maxs: Vec3,
}

/// Builds the cascade trees and the world shadow meshes. A cascade reference
/// below the first static prop index names a shadow mesh, one above it a
/// static prop; without the props the engine draws no prop into the sun's
/// shadow map.
pub fn build_csm(
    casters: &[ShadowCaster],
    props: &[PropCaster],
    world_mins: Vec3,
    world_maxs: Vec3,
    directions: &[[f32; 3]],
) -> Result<Csm> {
    if casters.is_empty() && props.is_empty() {
        let dir = directions.first().copied().unwrap_or(FALLBACK_SHADOW_DIRECTION);
        return empty_csm(world_mins, world_maxs, dir);
    }
    let set = flatten_casters(casters)?;
    if set.meshes.is_empty() && props.is_empty() {
        let dir = directions.first().copied().unwrap_or(FALLBACK_SHADOW_DIRECTION);
        return empty_csm(world_mins, world_maxs, dir);
    }
    let dirs: Vec<[f32; 3]> = if directions.is_empty() {
        vec![FALLBACK_SHADOW_DIRECTION]
    } else {
        directions.to_vec()
    };
    let mut out = Csm {
        opaque: set.opaque.clone(),
        alpha: set.alpha.clone(),
        indices: set.indices.clone(),
        meshes: set.meshes.clone(),
        ..Csm::default()
    };
    for dir in &dirs {
        let d = normalise(Vec3::new(dir[0], dir[1], dir[2]));
        let basis = csm_light_basis(d)?;
        let node_begin = out.nodes.len() as u32;
        let mut items: Vec<(u32, Vec3, Vec3)> = (0..set.meshes.len() as u32)
            .map(|m| {
                let (lo, hi) = mesh_bounds(&set, m, &basis);
                (m, lo, hi)
            })
            .collect();
        for p in props {
            let (lo, hi) = box_bounds(p.mins, p.maxs, &basis);
            items.push((PROP_REF_MARK | p.lump_index, lo, hi));
        }
        let (nodes, own) = cascade_tree(&items)?;
        let root = node_begin;
        let root_end = out.nodes.len() as u32 + nodes.len() as u32;
        let refs_begin = out.refs.len() as u32;
        let laid = lay_out_refs(&nodes, &own);
        out.refs.extend_from_slice(&laid.refs);
        let refs_end = out.refs.len() as u32;
        for (i, mut n) in nodes.into_iter().enumerate() {
            n.first_ref = laid.first_ref[i];
            n.ref_count = laid.own_count[i];
            out.nodes.push(n);
        }
        out.totals = compute_totals(&out.nodes);
        out.envs.push(CsmEnv {
            root,
            refs_begin,
            mesh_begin: 0,
            root_end,
            refs_end,
            mesh_end: set.meshes.len() as u32,
            direction: d.to_array(),
        });
    }
    if out.envs.is_empty() {
        return empty_csm(world_mins, world_maxs, FALLBACK_SHADOW_DIRECTION);
    }
    Ok(out)
}

fn flatten_casters(casters: &[ShadowCaster]) -> Result<MeshSet> {
    let mut set = MeshSet::default();
    for c in casters {
        if c.indices.len() % 3 != 0 {
            return Err(Error::lighting("a shadow caster has an index count that is not a multiple of 3"));
        }
        if c.positions.len() > u16::MAX as usize {
            return Err(Error::lighting("a shadow caster runs past the 16-bit index range"));
        }
        if c.positions.is_empty() || c.indices.is_empty() {
            continue;
        }
        for i in c.indices.chunks_exact(3) {
            if i[0] as usize >= c.positions.len() || i[1] as usize >= c.positions.len() || i[2] as usize >= c.positions.len() {
                return Err(Error::lighting("a shadow caster names a vertex it does not have"));
            }
            set.indices.push(i[0]);
            set.indices.push(i[1]);
            set.indices.push(i[2]);
        }
        let tris = (c.indices.len() / 3) as u32;
        let base = if c.alpha {
            let b = set.alpha.len() as u32;
            for p in &c.positions {
                set.alpha.push(ShadowAlphaVertex { position: *p, tail: [0; 2] });
            }
            b
        } else {
            let b = set.opaque.len() as u32;
            for p in &c.positions {
                set.opaque.push(*p);
            }
            b
        };
        set.meshes.push(ShadowMeshRecord {
            first_vertex: base,
            triangle_count: tris,
            tail: if c.alpha { 0 } else { 0xFFFF },
            draw_alpha: c.alpha,
        });
    }
    Ok(set)
}

/// A node before the breadth-first renumber, when its children are two
/// explicit indices rather than a pair of adjacent slots.
struct RawCsmNode {
    mins: [f32; 3],
    maxs: [f32; 3],
    left: Option<usize>,
    right: Option<usize>,
    own: Vec<u32>,
}

/// A median-split tree over the cascade's meshes, in light space, with each
/// node's own mesh list. The nodes come out breadth first, so a node's two
/// children are adjacent and come after it.
fn cascade_tree(items: &[(u32, Vec3, Vec3)]) -> Result<(Vec<CsmNodeRecord>, Vec<Vec<u32>>)> {
    let boxes: Vec<(Vec3, Vec3)> = items.iter().map(|(_, lo, hi)| (*lo, *hi)).collect();
    let centres: Vec<Vec3> = boxes.iter().map(|(lo, _)| *lo).collect();
    let all: Vec<usize> = (0..items.len()).collect();
    let mut raw: Vec<RawCsmNode> = Vec::new();
    build_raw(&boxes, &centres, &all, &mut raw);
    let mut order: Vec<usize> = vec![0];
    let mut at = 0usize;
    while at < order.len() {
        if let (Some(l), Some(r)) = (raw[order[at]].left, raw[order[at]].right) {
            order.push(l);
            order.push(r);
        }
        at += 1;
    }
    let mut index_of = vec![0usize; raw.len()];
    for (new, old) in order.iter().enumerate() {
        index_of[*old] = new;
    }
    let mut nodes: Vec<CsmNodeRecord> = Vec::with_capacity(order.len());
    let mut own: Vec<Vec<u32>> = Vec::with_capacity(order.len());
    for old in &order {
        let n = &raw[*old];
        nodes.push(CsmNodeRecord {
            mins: n.mins,
            maxs: n.maxs,
            first_child: n.left.map(|l| index_of[l] as u32).unwrap_or(0),
            child_count: if n.left.is_some() { 2 } else { 0 },
            first_ref: 0,
            ref_count: 0,
        });
        own.push(n.own.iter().map(|i| items[*i as usize].0).collect());
    }
    Ok((nodes, own))
}

fn build_raw(boxes: &[(Vec3, Vec3)], centres: &[Vec3], list: &[usize], out: &mut Vec<RawCsmNode>) -> usize {
    let index = out.len();
    out.push(RawCsmNode { mins: [0.0; 3], maxs: [0.0; 3], left: None, right: None, own: Vec::new() });
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for i in list {
        lo = lo.min(boxes[*i].0);
        hi = hi.max(boxes[*i].1);
    }
    if !lo.is_finite() {
        lo = Vec3::ZERO;
        hi = Vec3::ZERO;
    }
    if list.len() <= 1 {
        out[index].own = list.iter().map(|i| *i as u32).collect();
        out[index].mins = lo.to_array();
        out[index].maxs = hi.to_array();
        return index;
    }
    let extent = hi - lo;
    let axis = if extent.y > extent.x {
        if extent.z > extent.y { 2 } else { 1 }
    } else if extent.z > extent.x {
        2
    } else {
        0
    };
    let mut keyed: Vec<(f32, usize)> = list.iter().map(|i| (centres[*i][axis], *i)).collect();
    keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let pivot = keyed[keyed.len() / 2].0;
    let left: Vec<usize> = keyed.iter().filter(|(c, _)| *c < pivot).map(|(_, i)| *i).collect();
    let right: Vec<usize> = keyed.iter().filter(|(c, _)| *c >= pivot).map(|(_, i)| *i).collect();
    if left.is_empty() || right.is_empty() {
        out[index].own = list.iter().map(|i| *i as u32).collect();
        out[index].mins = lo.to_array();
        out[index].maxs = hi.to_array();
        return index;
    }
    let l = build_raw(boxes, centres, &left, out);
    let r = build_raw(boxes, centres, &right, out);
    out[index] = RawCsmNode {
        mins: lo.to_array(),
        maxs: hi.to_array(),
        left: Some(l),
        right: Some(r),
        own: Vec::new(),
    };
    index
}

/// Lays a cascade's own references out in preorder and returns the offset each
/// node takes.
struct LaidRefs {
    refs: Vec<u32>,
    first_ref: Vec<u32>,
    own_count: Vec<u32>,
}

fn lay_out_refs(nodes: &[CsmNodeRecord], own: &[Vec<u32>]) -> LaidRefs {
    let mut out = LaidRefs { refs: Vec::new(), first_ref: vec![0; nodes.len()], own_count: vec![0; nodes.len()] };
    for (count, o) in out.own_count.iter_mut().zip(own) {
        *count = o.len().min(0xFF) as u32;
    }
    fn walk(i: usize, nodes: &[CsmNodeRecord], own: &[Vec<u32>], out: &mut LaidRefs) {
        out.first_ref[i] = out.refs.len() as u32;
        out.refs.extend_from_slice(&own[i]);
        let n = nodes[i];
        if n.child_count > 0 {
            // Children are laid out directly after their parent, the first at
            // `first_child`, so the walk descends in index order.
            for c in n.first_child as usize..(n.first_child + n.child_count) as usize {
                if c < nodes.len() {
                    walk(c, nodes, own, out);
                }
            }
        }
    }
    if !nodes.is_empty() {
        walk(0, nodes, own, &mut out);
    }
    out
}

fn compute_totals(nodes: &[CsmNodeRecord]) -> Vec<u32> {
    let mut totals = vec![0u32; nodes.len()];
    for i in (0..nodes.len()).rev() {
        let n = nodes[i];
        let mut s = n.ref_count;
        for c in n.first_child as usize..(n.first_child + n.child_count) as usize {
            s += totals.get(c).copied().unwrap_or(0);
        }
        totals[i] = s;
    }
    totals
}

fn project(p: [f32; 3], basis: &[[f32; 3]; 3]) -> Vec3 {
    let v = Vec3::new(p[0], p[1], p[2]);
    Vec3::new(
        basis[0][0] * v.x + basis[0][1] * v.y + basis[0][2] * v.z,
        basis[1][0] * v.x + basis[1][1] * v.y + basis[1][2] * v.z,
        basis[2][0] * v.x + basis[2][1] * v.y + basis[2][2] * v.z,
    )
}

fn bounds_of(nodes: &[CsmNodeRecord], basis: &[[f32; 3]; 3]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for n in nodes {
        for corner in [
            Vec3::new(n.mins[0], n.mins[1], n.mins[2]),
            Vec3::new(n.maxs[0], n.maxs[1], n.maxs[2]),
        ] {
            let p = project(corner.to_array(), basis);
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    if !lo.is_finite() {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    (lo, hi)
}

/// A world box's light-space bounds.
fn box_bounds(mins: Vec3, maxs: Vec3, basis: &[[f32; 3]; 3]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for k in 0..8 {
        let c = Vec3::new(
            if k & 1 == 0 { mins.x } else { maxs.x },
            if k & 2 == 0 { mins.y } else { maxs.y },
            if k & 4 == 0 { mins.z } else { maxs.z },
        );
        let q = project(c.to_array(), basis);
        lo = lo.min(q);
        hi = hi.max(q);
    }
    if !lo.is_finite() {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    (lo, hi)
}

/// Rewrites the marked prop references of a written map's cascade tree as
/// `first_prop + lump index`, the value the engine reads as a static prop.
pub fn resolve_prop_refs(bsp: &mut rbsp::Bsp, first_prop: u32) {
    let id = rbsp::ids::CSM_OBJ_REFERENCES;
    let mut bytes = bsp.lump(id).to_vec();
    let mut changed = false;
    for c in bytes.chunks_exact_mut(4) {
        let v = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        if v & PROP_REF_MARK != 0 {
            c.copy_from_slice(&(first_prop + (v & !PROP_REF_MARK)).to_le_bytes());
            changed = true;
        }
    }
    if changed {
        bsp.set_lump(id, bytes);
    }
}

fn mesh_bounds(set: &MeshSet, mesh: u32, basis: &[[f32; 3]; 3]) -> (Vec3, Vec3) {
    let Some(m) = set.meshes.get(mesh as usize) else {
        return (Vec3::ZERO, Vec3::ZERO);
    };
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    let start = set.index_start(mesh);
    for k in 0..(m.triangle_count as usize) * 3 {
        let Some(&i) = set.indices.get(start + k) else { continue };
        let q = project(set.position(m, m.first_vertex as usize + i as usize), basis);
        lo = lo.min(q);
        hi = hi.max(q);
    }
    if !lo.is_finite() {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    (lo, hi)
}

fn normalise(v: Vec3) -> Vec3 {
    let n = v.normalize_or_zero();
    if n.length_squared() < 0.5 {
        Vec3::new(FALLBACK_SHADOW_DIRECTION[0], FALLBACK_SHADOW_DIRECTION[1], FALLBACK_SHADOW_DIRECTION[2]).normalize()
    } else {
        n
    }
}

/// The declared triangle total of a mesh list.
pub fn declared_triangles(meshes: &[ShadowMeshRecord]) -> Result<u32> {
    let mut total: u64 = 0;
    for m in meshes {
        total += m.triangle_count as u64;
    }
    u32::try_from(total).map_err(|_| Error::lighting("the shadow mesh triangle count overflows 32 bits"))
}
