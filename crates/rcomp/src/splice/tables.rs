//! Surgery on the reference's index tables: the two object-reference trees
//! (cells and cascaded shadows) and the collision BVH.
//!
//! Every operation here keeps the index every other lump holds. A reference is
//! taken out of a tree, or put into one, by moving the runs of the nodes after
//! it; a collision leaf is switched off where it sits.
//!
//! What the trees look like in a shipped map, read off staging_mu1: every
//! subtree's references are one contiguous run, the node's own references
//! first and its children's after them in child order, and the subtree totals
//! lump (0x25 for the cells, 0x26 for the shadows) holds each node's subtree
//! reference count for a prefix of the nodes. The cell tree has one root per
//! cell; the shadow tree has one root.

use std::collections::BTreeSet;

use glam::Vec3;
use rbsp::lumps::collision::{
    self as coll, BvhNode, ContentMask, CHILD_BUNDLE, CHILD_EMPTY, CHILD_NODE, CHILD_POLY, CHILD_PROP,
};
use rbsp::lumps::vis::ObjRefBounds;
use rbsp::{ids, Bsp, Version};

use crate::collision::CHILD_POLY_MAX;
use crate::report::Report;

/// One node of either reference tree; both lumps share the 32-byte layout.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TreeNode {
    pub mins: [f32; 3],
    pub children: u32,
    pub maxs: [f32; 3],
    pub obj_refs: u32,
}

impl TreeNode {
    pub fn child_count(&self) -> usize {
        (self.children & 0xFF) as usize
    }
    pub fn first_child(&self) -> usize {
        ((self.children >> 8) & 0xFFFF) as usize
    }
    pub fn ref_count(&self) -> usize {
        (self.obj_refs & 0xFF) as usize
    }
    pub fn first_ref(&self) -> usize {
        ((self.obj_refs >> 8) & 0xFFFF) as usize
    }
    fn set_refs(&mut self, first: usize, count: usize) {
        self.obj_refs = (self.obj_refs & 0xFF00_0000) | ((first as u32 & 0xFFFF) << 8) | (count as u32 & 0xFF);
    }
    fn children(&self) -> std::ops::Range<usize> {
        self.first_child()..self.first_child() + self.child_count()
    }
}

fn read_nodes(bytes: &[u8]) -> Vec<TreeNode> {
    bytes
        .chunks_exact(32)
        .map(|c| {
            let f = |o: usize| f32::from_le_bytes(c[o..o + 4].try_into().unwrap());
            let u = |o: usize| u32::from_le_bytes(c[o..o + 4].try_into().unwrap());
            TreeNode { mins: [f(0), f(4), f(8)], children: u(12), maxs: [f(16), f(20), f(24)], obj_refs: u(28) }
        })
        .collect()
}

fn write_nodes(nodes: &[TreeNode]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nodes.len() * 32);
    for n in nodes {
        for v in n.mins {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&n.children.to_le_bytes());
        for v in n.maxs {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&n.obj_refs.to_le_bytes());
    }
    out
}

fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn from_u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Which of the two trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tree {
    Cells,
    Shadows,
}

impl Tree {
    fn lumps(self) -> (u16, u16, u16) {
        match self {
            Tree::Cells => (ids::CELL_AABB_NODES, ids::OBJ_REFERENCES, ids::CELL_AABB_SUBTREE_REF_TOTALS),
            Tree::Shadows => (ids::CSM_AABB_NODES, ids::CSM_OBJ_REFERENCES, ids::CSM_AABB_SUBTREE_REF_TOTALS),
        }
    }
}

/// A reference tree read out of a map, edited, and written back.
#[derive(Clone, Debug)]
pub struct RefTree {
    pub tree: Tree,
    pub nodes: Vec<TreeNode>,
    pub refs: Vec<u32>,
    /// One box per reference, for the cell tree only.
    pub bounds: Option<Vec<ObjRefBounds>>,
    /// How many nodes the totals lump covers.
    totals_len: usize,
    parent: Vec<Option<usize>>,
}

impl RefTree {
    pub fn load(bsp: &Bsp, tree: Tree) -> Option<Self> {
        let (n, r, t) = tree.lumps();
        let nodes = read_nodes(bsp.lump(n));
        if nodes.is_empty() {
            return None;
        }
        let refs = u32s(bsp.lump(r));
        let bounds = match tree {
            Tree::Cells => {
                let v = bsp.version_kind().unwrap_or(Version::ClientV51);
                Some(rbsp::lumps::vis::parse_obj_ref_bounds(bsp.lump(ids::OBJ_REFERENCE_BOUNDS), v).ok()?)
            }
            Tree::Shadows => None,
        };
        let totals_len = bsp.lump(t).len() / 4;
        let mut parent = vec![None; nodes.len()];
        for (i, node) in nodes.iter().enumerate() {
            for c in node.children() {
                if let Some(p) = parent.get_mut(c) {
                    *p = Some(i);
                }
            }
        }
        Some(Self { tree, nodes, refs, bounds, totals_len, parent })
    }

    pub fn store(&self, bsp: &mut Bsp) {
        let (n, r, t) = self.tree.lumps();
        bsp.set_lump(n, write_nodes(&self.nodes));
        bsp.set_lump(r, from_u32s(&self.refs));
        if let Some(b) = &self.bounds {
            let v = bsp.version_kind().unwrap_or(Version::ClientV51);
            bsp.set_lump(ids::OBJ_REFERENCE_BOUNDS, rbsp::lumps::vis::write_obj_ref_bounds(b, v));
        }
        let totals: Vec<u32> = (0..self.totals_len.min(self.nodes.len())).map(|i| self.subtree_refs(i) as u32).collect();
        bsp.set_lump(t, from_u32s(&totals));
    }

    /// The references a node's subtree holds.
    pub fn subtree_refs(&self, node: usize) -> usize {
        let mut total = 0;
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            let Some(x) = self.nodes.get(n) else { continue };
            total += x.ref_count();
            stack.extend(x.children());
        }
        total
    }

    /// Takes every reference `drop` picks out of the tree, keeping the order of
    /// the rest. Returns how many went.
    pub fn remove_where(&mut self, drop: impl Fn(u32) -> bool) -> usize {
        let gone: Vec<bool> = self.refs.iter().map(|r| drop(*r)).collect();
        let removed = gone.iter().filter(|g| **g).count();
        if removed == 0 {
            return 0;
        }
        // before[i]: references removed ahead of position i.
        let mut before = Vec::with_capacity(gone.len() + 1);
        let mut acc = 0usize;
        for g in &gone {
            before.push(acc);
            acc += usize::from(*g);
        }
        before.push(acc);
        for node in &mut self.nodes {
            let (first, count) = (node.first_ref(), node.ref_count());
            let a = first.min(gone.len());
            let b = (first + count).min(gone.len());
            let inside = before[b] - before[a];
            node.set_refs(first - before[a], count - inside);
        }
        let mut k = 0;
        self.refs.retain(|_| {
            k += 1;
            !gone[k - 1]
        });
        if let Some(bounds) = &mut self.bounds {
            let mut k = 0;
            bounds.retain(|_| {
                k += 1;
                !gone.get(k - 1).copied().unwrap_or(false)
            });
        }
        removed
    }

    /// Adds references to a node's own run, after the ones it has. The node's
    /// box and every ancestor's grow to cover `bounds` when one is given.
    pub fn add_to(&mut self, node: usize, added: &[(u32, Option<(Vec3, Vec3)>)]) -> Result<(), String> {
        let Some(n) = self.nodes.get(node).copied() else {
            return Err(format!("{:?} tree has no node {node}", self.tree));
        };
        if n.ref_count() + added.len() > 0xFF {
            return Err(format!(
                "{:?} tree node {node} holds {} references and cannot take {} more",
                self.tree,
                n.ref_count(),
                added.len()
            ));
        }
        if self.refs.len() + added.len() > 0xFFFF {
            return Err(format!("{:?} tree would hold more than 65535 references", self.tree));
        }
        let at = n.first_ref() + n.ref_count();
        let k = added.len();
        // Every run after the insertion point moves up. A run that starts right
        // at it moves too, unless it is an ancestor's: an ancestor's subtree
        // run has to keep starting where it did, since it now holds the new
        // references.
        let shift: Vec<bool> = (0..self.nodes.len())
            .map(|i| {
                let first = self.nodes[i].first_ref();
                i != node && (first > at || (first == at && !self.is_ancestor(i, node)))
            })
            .collect();
        for (i, other) in self.nodes.iter_mut().enumerate() {
            if shift[i] {
                other.set_refs(other.first_ref() + k, other.ref_count());
            }
        }
        let own = &mut self.nodes[node];
        own.set_refs(n.first_ref(), n.ref_count() + k);
        for (j, (value, _)) in added.iter().enumerate() {
            self.refs.insert(at + j, *value);
        }
        if let Some(bounds) = &mut self.bounds {
            for (j, (_, b)) in added.iter().enumerate() {
                let (lo, hi) = b.unwrap_or((Vec3::ZERO, Vec3::ZERO));
                bounds.insert(at + j, ObjRefBounds { mins: lo.to_array(), unknown_0: 0, maxs: hi.to_array(), unknown_1: 0 });
            }
        }
        for (_, b) in added {
            if let Some((lo, hi)) = b {
                self.grow(node, *lo, *hi);
            }
        }
        Ok(())
    }

    /// True when `ancestor` is on the path from `node` up to its root.
    fn is_ancestor(&self, ancestor: usize, mut node: usize) -> bool {
        let mut guard = 0;
        while let Some(p) = self.parent.get(node).copied().flatten() {
            if p == ancestor {
                return true;
            }
            node = p;
            guard += 1;
            if guard > 4096 {
                break;
            }
        }
        false
    }

    /// Grows a node's box and every ancestor's to cover a box.
    pub fn grow(&mut self, node: usize, lo: Vec3, hi: Vec3) {
        let mut at = Some(node);
        let mut guard = 0;
        while let Some(n) = at {
            let x = &mut self.nodes[n];
            for a in 0..3 {
                x.mins[a] = x.mins[a].min(lo[a]);
                x.maxs[a] = x.maxs[a].max(hi[a]);
            }
            at = self.parent[n];
            guard += 1;
            if guard > 4096 {
                break;
            }
        }
    }

    /// Replaces the box of every reference to `value` and grows the nodes that
    /// hold it. Returns how many references were moved.
    pub fn rebox(&mut self, value: u32, map: impl Fn(Vec3, Vec3) -> (Vec3, Vec3)) -> usize {
        let Some(bounds) = &mut self.bounds else { return 0 };
        let mut owners: Vec<(usize, Vec3, Vec3)> = Vec::new();
        for (node_index, node) in self.nodes.iter().enumerate() {
            for i in node.first_ref()..node.first_ref() + node.ref_count() {
                if self.refs.get(i) != Some(&value) {
                    continue;
                }
                let Some(b) = bounds.get_mut(i) else { continue };
                let (lo, hi) = map(Vec3::from_array(b.mins), Vec3::from_array(b.maxs));
                b.mins = lo.to_array();
                b.maxs = hi.to_array();
                owners.push((node_index, lo, hi));
            }
        }
        let moved = owners.len();
        for (n, lo, hi) in owners {
            self.grow(n, lo, hi);
        }
        moved
    }

    /// The roots: every node no other node names as a child.
    pub fn roots(&self) -> Vec<usize> {
        (0..self.nodes.len()).filter(|i| self.parent[*i].is_none()).collect()
    }
}

/// The first reference value that names a static prop rather than a mesh.
pub fn first_prop_ref(bsp: &Bsp) -> Option<u32> {
    let v = bsp.version_kind()?;
    match rbsp::lumps::parse(ids::LEVEL_INFO, v, bsp.lump(ids::LEVEL_INFO)).ok()? {
        rbsp::lumps::Lump::LevelInfo(info) if info.first_static_prop_index > 0 => Some(info.first_static_prop_index),
        _ => None,
    }
}

/// Takes every reference to a value out of both trees.
pub fn remove_obj_refs(bsp: &mut Bsp, values: &BTreeSet<u32>) -> (usize, usize) {
    let mut out = (0, 0);
    for (tree, slot) in [(Tree::Cells, 0), (Tree::Shadows, 1)] {
        let Some(mut t) = RefTree::load(bsp, tree) else { continue };
        let n = t.remove_where(|r| values.contains(&r));
        if n > 0 {
            t.store(bsp);
        }
        if slot == 0 {
            out.0 = n;
        } else {
            out.1 = n;
        }
    }
    out
}

// ---- collision --------------------------------------------------------------

/// Where a leaf sits: a direct slot of a node, or an entry of a bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeafSite {
    Slot { node: usize, slot: usize },
    Bundle { descriptor: usize },
}

#[derive(Clone, Copy, Debug)]
struct LeafAt {
    site: LeafSite,
    kind: u32,
    /// The leaf's first dword.
    at: usize,
    /// The node slot the leaf, or the bundle holding it, hangs from.
    hang: (usize, usize),
    /// The contents index the leaf is tested with.
    contents: u8,
}

/// What a leaf is made of: a polygon leaf's surface, or the surface of a
/// hull's first face; `None` for any other kind.
fn leaf_surface(words: &[u32], kind: u32, at: usize) -> Option<u32> {
    if (CHILD_POLY..=CHILD_POLY_MAX).contains(&kind) {
        return words.get(at).map(|h| h & 0xFFF);
    }
    if kind == coll::CHILD_HULL {
        // The header's first dword is four byte counts: vertices, planes,
        // triangle sets, quad sets.
        let head = *words.get(at)?;
        let (nv, npl) = ((head & 0xFF) as usize, ((head >> 8) & 0xFF) as usize);
        let header = (20 + nv * 6 + npl * 3).div_ceil(4);
        return words.get(at + header).map(|h| h & 0xFFF);
    }
    None
}

/// Every leaf one model's tree reaches, with where it hangs.
fn leaf_sites(nodes: &[BvhNode], words: &[u32], model: &coll::Model) -> Vec<LeafAt> {
    let mut out = Vec::new();
    if !crate::collision::model_has_collision(model) {
        return out;
    }
    let base = model.bvh_leaf.max(0) as usize;
    let node0 = model.bvh_node.max(0) as usize;
    let mut seen = vec![false; nodes.len()];
    let mut stack = vec![node0];
    while let Some(ni) = stack.pop() {
        if ni >= nodes.len() || std::mem::replace(&mut seen[ni], true) {
            continue;
        }
        let node = &nodes[ni];
        for slot in 0..4 {
            let kind = node.child_kind(slot);
            let at = node.child_index(slot) as usize;
            match kind {
                CHILD_EMPTY => {}
                CHILD_NODE => stack.push(node0 + at),
                CHILD_BUNDLE => {
                    let start = base + at;
                    let Some(&count) = words.get(start) else { continue };
                    let mut pay = start + 1 + count as usize;
                    for k in 0..count as usize {
                        let Some(&desc) = words.get(start + 1 + k) else { break };
                        out.push(LeafAt {
                            site: LeafSite::Bundle { descriptor: start + 1 + k },
                            kind: (desc >> 8) & 0xFF,
                            at: pay,
                            hang: (ni, slot),
                            contents: (desc & 0xFF) as u8,
                        });
                        pay += (desc >> 16) as usize;
                    }
                }
                _ => out.push(LeafAt {
                    site: LeafSite::Slot { node: ni, slot },
                    kind,
                    at: base + at,
                    hang: (ni, slot),
                    contents: node.contents_index(),
                }),
            }
        }
    }
    out
}

/// The contents index whose mask is zero, adding one when the table has none.
fn empty_contents(masks: &mut Vec<ContentMask>) -> Result<u32, String> {
    if let Some(i) = masks.iter().position(|m| m.mask == 0) {
        return Ok(i as u32);
    }
    if masks.len() >= 0xFF {
        return Err("the contents table is full, so no leaf can be switched off".into());
    }
    masks.push(ContentMask { mask: 0 });
    Ok(masks.len() as u32 - 1)
}

/// Switches off every leaf of `model` that `kill` picks: a leaf in a node slot
/// becomes an empty slot, and a bundle entry takes a contents mask of zero,
/// which no trace tests against. Nothing moves. Returns the leaves switched off.
fn kill_leaves(bsp: &mut Bsp, model: usize, kill: impl Fn(u32, usize, &[u32], u32) -> bool) -> Result<usize, String> {
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let models = coll::parse_models(bsp.lump(ids::MODELS), v).map_err(|e| e.to_string())?;
    let Some(m) = models.get(model).copied() else { return Ok(0) };
    let mut nodes = coll::parse_bvh_nodes(bsp.lump(ids::BVH_NODES), v).map_err(|e| e.to_string())?;
    let mut words = coll::parse_bvh_leaf(bsp.lump(ids::BVH_LEAF_DATA), v).map_err(|e| e.to_string())?.words;
    let mut masks = coll::parse_content_masks(bsp.lump(ids::CONTENTS_MASKS), v).map_err(|e| e.to_string())?;
    let sites = leaf_sites(&nodes, &words, &m);
    let mut killed = 0usize;
    let mut zero: Option<u32> = None;
    let mut touched_masks = false;
    for s in sites {
        let mask = masks.get(s.contents as usize).map(|m| m.mask).unwrap_or(0);
        if mask == 0 || !kill(s.kind, s.at, &words, mask) {
            continue;
        }
        match s.site {
            LeafSite::Slot { node, slot } => {
                let w = if slot < 2 { 2 } else { 3 };
                let shift = 4 * (slot % 2);
                let meta = &mut nodes[node].meta[w];
                *meta = (*meta & !(0xF << shift)) | (CHILD_EMPTY << shift);
            }
            LeafSite::Bundle { descriptor } => {
                let z = match zero {
                    Some(z) => z,
                    None => {
                        let before = masks.len();
                        let z = empty_contents(&mut masks)?;
                        touched_masks |= masks.len() != before;
                        zero = Some(z);
                        z
                    }
                };
                words[descriptor] = (words[descriptor] & !0xFF) | z;
            }
        }
        killed += 1;
    }
    if killed > 0 {
        bsp.set_lump(ids::BVH_NODES, coll::write_bvh_nodes(&nodes, v));
        bsp.set_lump(ids::BVH_LEAF_DATA, coll::write_bvh_leaf(&coll::BvhLeafData { words }, v));
        if touched_masks {
            bsp.set_lump(ids::CONTENTS_MASKS, coll::write_content_masks(&masks, v));
        }
    }
    Ok(killed)
}

/// Switches off the collision leaf of one static prop.
pub fn kill_collision_prop(bsp: &mut Bsp, prop: u32) -> Result<usize, String> {
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let count = coll::parse_models(bsp.lump(ids::MODELS), v).map(|m| m.len()).unwrap_or(0);
    let mut total = 0;
    for model in 0..count {
        total += kill_leaves(bsp, model, |kind, at, words, _| {
            kind == CHILD_PROP && words.get(at).is_some_and(|w| w & 0xFFFF == prop)
        })?;
    }
    Ok(total)
}

/// Switches off every polygon and hull leaf of `model` whose surface is one of
/// `surfaces` (indices into 0x11) and, when `contents` names one, whose mask is
/// that one.
pub fn kill_collision_surfaces(
    bsp: &mut Bsp,
    model: usize,
    surfaces: &BTreeSet<u32>,
    contents: Option<u32>,
) -> Result<usize, String> {
    kill_leaves(bsp, model, |kind, at, words, mask| {
        contents.is_none_or(|c| c == mask) && leaf_surface(words, kind, at).is_some_and(|s| surfaces.contains(&s))
    })
}

/// How many polygon and hull leaves of `model` each (surface name, mask) pair
/// has, the surface name in `material_key` form.
pub fn surface_masks(bsp: &Bsp, model: usize) -> std::collections::HashMap<(String, u32), usize> {
    let mut out = std::collections::HashMap::new();
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let (Ok(models), Ok(nodes), Ok(leaf), Ok(masks), Ok(props), Ok(names)) = (
        coll::parse_models(bsp.lump(ids::MODELS), v),
        coll::parse_bvh_nodes(bsp.lump(ids::BVH_NODES), v),
        coll::parse_bvh_leaf(bsp.lump(ids::BVH_LEAF_DATA), v),
        coll::parse_content_masks(bsp.lump(ids::CONTENTS_MASKS), v),
        coll::parse_surface_props(bsp.lump(ids::SURFACE_PROPERTIES), v),
        rbsp::lumps::render::parse_surface_names(bsp.lump(ids::SURFACE_NAMES), v),
    ) else {
        return out;
    };
    let Some(m) = models.get(model) else { return out };
    for s in leaf_sites(&nodes, &leaf.words, m) {
        let Some(surface) = leaf_surface(&leaf.words, s.kind, s.at) else { continue };
        let mask = masks.get(s.contents as usize).map(|x| x.mask).unwrap_or(0);
        if mask == 0 {
            continue;
        }
        let name = props.get(surface as usize).and_then(|p| names.at_offset(p.name as usize)).unwrap_or_default();
        *out.entry((material_key(name), mask)).or_insert(0) += 1;
    }
    out
}

/// The mask most of a surface's leaves in `model` carry, which is the one its
/// render mesh stands for.
pub fn dominant_mask(masks: &std::collections::HashMap<(String, u32), usize>, material: &str) -> Option<u32> {
    let key = material_key(material);
    masks.iter().filter(|((n, _), _)| *n == key).max_by_key(|(k, c)| (**c, k.1)).map(|((_, m), _)| *m)
}

/// Slot bounds are int16 steps of `decode_scale * 65536` from the model's
/// origin, laid out `[min x; 4] [max x; 4] [min y; 4] [max y; 4] [min z; 4]
/// [max z; 4]`, one column per slot.
fn slot_box(node: &BvhNode, slot: usize, m: &coll::Model) -> (Vec3, Vec3) {
    let step = m.decode_scale * crate::collision::DECODE_SCALE_UNIT as f32;
    let q = &node.min_max;
    let o = Vec3::from_array(m.origin);
    let lo = o + Vec3::new(q[slot] as f32, q[8 + slot] as f32, q[16 + slot] as f32) * step;
    let hi = o + Vec3::new(q[4 + slot] as f32, q[12 + slot] as f32, q[20 + slot] as f32) * step;
    (lo, hi)
}

fn set_slot_box(node: &mut BvhNode, slot: usize, m: &coll::Model, lo: Vec3, hi: Vec3) -> Result<(), String> {
    let step = (m.decode_scale as f64 * crate::collision::DECODE_SCALE_UNIT).max(f64::MIN_POSITIVE);
    let q = |v: f32, o: f32, up: bool| -> Result<i16, String> {
        let t = (v as f64 - o as f64) / step;
        let t = if up { t.ceil() } else { t.floor() };
        if t < i16::MIN as f64 || t > i16::MAX as f64 {
            return Err(format!("a box edge at {v} is outside the model's collision grid"));
        }
        Ok(t as i16)
    };
    for a in 0..3 {
        node.min_max[8 * a + slot] = q(lo[a], m.origin[a], false)?;
        node.min_max[8 * a + 4 + slot] = q(hi[a], m.origin[a], true)?;
    }
    Ok(())
}

/// Moves a static prop's collision to where its record now stands.
///
/// A prop leaf carries only the prop's index; the engine places the prop's own
/// collision with the record. What keeps a trace from reaching it at the new
/// place is the tree: the slot the leaf hangs from, and every slot above it,
/// only covers the old place. `map` takes the old slot box to the new one, and
/// each of those slots grows to cover it. Returns the leaves moved.
pub fn grow_collision_prop(bsp: &mut Bsp, prop: u32, map: impl Fn(Vec3, Vec3) -> (Vec3, Vec3)) -> Result<usize, String> {
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let mut models = coll::parse_models(bsp.lump(ids::MODELS), v).map_err(|e| e.to_string())?;
    let mut nodes = coll::parse_bvh_nodes(bsp.lump(ids::BVH_NODES), v).map_err(|e| e.to_string())?;
    let words = coll::parse_bvh_leaf(bsp.lump(ids::BVH_LEAF_DATA), v).map_err(|e| e.to_string())?.words;
    let mut moved = 0;
    #[allow(clippy::needless_range_loop)]
    for mi in 0..models.len() {
        let m = models[mi];
        let sites: Vec<LeafAt> = leaf_sites(&nodes, &words, &m)
            .into_iter()
            .filter(|s| s.kind == CHILD_PROP && words.get(s.at).is_some_and(|w| w & 0xFFFF == prop))
            .collect();
        if sites.is_empty() {
            continue;
        }
        // Which slot of which node names each node of this model's tree.
        let node0 = m.bvh_node.max(0) as usize;
        let mut up: std::collections::HashMap<usize, (usize, usize)> = std::collections::HashMap::new();
        let mut stack = vec![node0];
        let mut seen = BTreeSet::new();
        while let Some(n) = stack.pop() {
            if n >= nodes.len() || !seen.insert(n) {
                continue;
            }
            for slot in 0..4 {
                if nodes[n].child_kind(slot) == CHILD_NODE {
                    let c = node0 + nodes[n].child_index(slot) as usize;
                    up.entry(c).or_insert((n, slot));
                    stack.push(c);
                }
            }
        }
        for s in sites {
            let (node, slot) = s.hang;
            let (lo, hi) = map(slot_box(&nodes[node], slot, &m).0, slot_box(&nodes[node], slot, &m).1);
            let mut at = Some((node, slot));
            let mut guard = 0;
            while let Some((n, sl)) = at {
                let (a, b) = slot_box(&nodes[n], sl, &m);
                set_slot_box(&mut nodes[n], sl, &m, a.min(lo), b.max(hi))?;
                at = up.get(&n).copied();
                guard += 1;
                if guard > 4096 {
                    break;
                }
            }
            let md = &mut models[mi];
            for a in 0..3 {
                md.mins[a] = md.mins[a].min(lo[a]);
                md.maxs[a] = md.maxs[a].max(hi[a]);
            }
            moved += 1;
        }
    }
    if moved > 0 {
        bsp.set_lump(ids::BVH_NODES, coll::write_bvh_nodes(&nodes, v));
        bsp.set_lump(ids::MODELS, coll::write_models(&models, v));
    }
    Ok(moved)
}

/// A material path the way two lumps can be compared: lower case, forward
/// slashes.
pub fn material_key(name: &str) -> String {
    name.to_ascii_lowercase().replace('\\', "/")
}

/// The model and material every render mesh draws with, by mesh index.
pub fn mesh_materials(bsp: &Bsp) -> Result<Vec<(u32, String)>, String> {
    use rbsp::lumps::render as r;
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let meshes = r::parse_meshes(bsp.lump(ids::MESHES), v).map_err(|e| e.to_string())?;
    let sorts = r::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), v).map_err(|e| e.to_string())?;
    let texdata = r::parse_texdata(bsp.lump(ids::TEXTURE_DATA), v).map_err(|e| e.to_string())?;
    let names = r::parse_surface_names(bsp.lump(ids::SURFACE_NAMES), v).map_err(|e| e.to_string())?;
    let models = coll::parse_models(bsp.lump(ids::MODELS), v).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(meshes.len());
    for (mi, m) in meshes.iter().enumerate() {
        let material = sorts
            .get(m.material_sort as usize)
            .and_then(|s| texdata.get(s.tex_data as usize))
            .and_then(|td| names.at_offset(td.name as usize))
            .unwrap_or_default()
            .to_string();
        let model = models
            .iter()
            .position(|md| mi >= md.first_mesh as usize && mi < (md.first_mesh + md.num_meshes) as usize)
            .unwrap_or(0) as u32;
        out.push((model, material));
    }
    Ok(out)
}

/// The surface properties (indices into 0x11) whose name is `material`.
pub fn surfaces_named(bsp: &Bsp, material: &str) -> BTreeSet<u32> {
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let want = material_key(material);
    let Ok(props) = coll::parse_surface_props(bsp.lump(ids::SURFACE_PROPERTIES), v) else { return BTreeSet::new() };
    let Ok(names) = rbsp::lumps::render::parse_surface_names(bsp.lump(ids::SURFACE_NAMES), v) else {
        return BTreeSet::new();
    };
    props
        .iter()
        .enumerate()
        .filter(|(_, p)| names.at_offset(p.name as usize).is_some_and(|n| material_key(n) == want))
        .map(|(i, _)| i as u32)
        .collect()
}

/// Takes mesh groups out of a map: their meshes leave both reference trees, so
/// nothing draws them, and the collision polygons carrying their surface are
/// switched off. The mesh records themselves stay where they are.
pub fn drop_meshes(bsp: &mut Bsp, groups: &[&(u32, String)], report: &mut Report) -> Result<(), String> {
    if bsp.lump(ids::MESHES).is_empty() {
        return Ok(());
    }
    let materials = mesh_materials(bsp)?;
    let first_prop = first_prop_ref(bsp).unwrap_or(u32::MAX);
    let want: BTreeSet<(u32, String)> = groups.iter().map(|(m, name)| (*m, material_key(name))).collect();
    let meshes: BTreeSet<u32> = materials
        .iter()
        .enumerate()
        .filter(|(_, (model, name))| want.contains(&(*model, material_key(name))))
        .map(|(i, _)| i as u32)
        .filter(|i| *i < first_prop)
        .collect();
    let (cells, shadows) = remove_obj_refs(bsp, &meshes);
    let mut polys = 0;
    for (model, material) in groups {
        // A mesh group stands for the leaves of its surface with the mask most
        // of them carry; the rest are collision records of their own.
        let masks = surface_masks(bsp, *model as usize);
        let surfaces = surfaces_named(bsp, material);
        if let (false, Some(mask)) = (surfaces.is_empty(), dominant_mask(&masks, material)) {
            polys += kill_collision_surfaces(bsp, *model as usize, &surfaces, Some(mask))?;
        }
    }
    report.warn(format!(
        "{} mesh group(s) dropped: {} meshes, {cells} cell references, {shadows} shadow references, {polys} collision leaves; the merged shadow meshes still carry their shape",
        groups.len(),
        meshes.len()
    ));
    Ok(())
}

/// The cell root whose box holds a point, the fullest root when none does.
pub fn cell_for(cells: &RefTree, p: Vec3) -> usize {
    let roots = cells.roots();
    let holds = |n: usize| {
        let x = &cells.nodes[n];
        (0..3).all(|a| p[a] >= x.mins[a] && p[a] <= x.maxs[a])
    };
    roots
        .iter()
        .copied()
        .filter(|r| holds(*r) && cells.subtree_refs(*r) > 0)
        .max_by_key(|r| cells.subtree_refs(*r))
        .or_else(|| roots.iter().copied().max_by_key(|r| cells.subtree_refs(*r)))
        .unwrap_or(0)
}

/// Moves every static prop reference in both trees so props start at
/// `new_base`, and says so in the level info. Mesh references stay.
pub fn shift_prop_refs(bsp: &mut Bsp, new_base: u32) -> Result<(), String> {
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let rbsp::lumps::Lump::LevelInfo(mut info) =
        rbsp::lumps::parse(ids::LEVEL_INFO, v, bsp.lump(ids::LEVEL_INFO)).map_err(|e| e.to_string())?
    else {
        return Err("the level info does not parse".into());
    };
    let old = info.first_static_prop_index;
    if new_base <= old {
        return Ok(());
    }
    let delta = new_base - old;
    for tree in [Tree::Cells, Tree::Shadows] {
        let Some(mut t) = RefTree::load(bsp, tree) else { continue };
        for r in &mut t.refs {
            if *r >= old {
                *r += delta;
            }
        }
        t.store(bsp);
    }
    info.first_static_prop_index = new_base;
    info.first_sky_mesh += delta;
    bsp.set_lump(ids::LEVEL_INFO, rbsp::lumps::level::write_level_info(&info, v));
    Ok(())
}

/// Puts appended meshes into the cell tree, each in the cell whose root box
/// holds it. Mesh references sit below the first static prop index, so the
/// props move up a step of 64 when the meshes reach it.
pub fn add_meshes_to_cells(bsp: &mut Bsp, meshes: &[crate::meshes::AppendedMesh]) -> Result<(), String> {
    if meshes.is_empty() {
        return Ok(());
    }
    let reach = meshes.iter().map(|m| m.index + 1).max().unwrap_or(0);
    if let Some(base) = first_prop_ref(bsp) {
        if reach > base {
            shift_prop_refs(bsp, reach.div_ceil(64) * 64)?;
        }
    }
    let Some(mut cells) = RefTree::load(bsp, Tree::Cells) else { return Ok(()) };
    for m in meshes {
        let node = cell_for(&cells, (m.mins + m.maxs) * 0.5);
        cells.add_to(node, &[(m.index, Some((m.mins, m.maxs)))])?;
    }
    cells.store(bsp);
    Ok(())
}

/// Takes collision groups out of a map: every leaf of each group's model with
/// the group's mask and surface is switched off.
pub fn drop_collision(bsp: &mut Bsp, groups: &[&(u32, u32, String)], report: &mut Report) -> Result<(), String> {
    let mut leaves = 0;
    for (model, contents, surface) in groups {
        let surfaces = surfaces_named(bsp, surface);
        if !surfaces.is_empty() {
            leaves += kill_collision_surfaces(bsp, *model as usize, &surfaces, Some(*contents))?;
        }
    }
    report.warn(format!("{} collision group(s) dropped: {leaves} leaves switched off", groups.len()));
    Ok(())
}
