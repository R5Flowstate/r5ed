//! loop_cut, edge_slide and the ring preview polylines the hover shows.
//!
//! A ring walks quad faces only: from an edge it steps to the opposite edge
//! in the same quad, then across to the neighbour face. Triangles, n-gons,
//! boundary and non-manifold edges stop the walk; a walk that returns to its
//! start edge is a closed ring.

use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use super::{clamp_finite, drop_faces, parse_params, selected_edges, OpResult};
use crate::editmesh::{bad, Adjacency, Corner, EditError, EditMesh, Face, FaceFlags};
use crate::editmesh::select::{SelMode, Selection};

/// One strip side while a crossed quad is split: the vertex chain from one
/// rail to the other with its corner attributes.
type Strip = (Vec<u32>, Vec<[f32; 2]>, Vec<Option<[f32; 3]>>);
/// One fresh quad from a split: its vertices with corner attributes plus the
/// inherited slot and flags.
type FreshFace = (Vec<u32>, Vec<[f32; 2]>, Vec<Option<[f32; 3]>>, u16, FaceFlags);

/// One crossed ring: the cut rails in walk order and the quad face between
/// each consecutive pair (the last face spans back to the first rail when
/// `closed`).
#[derive(Clone, Debug)]
pub struct Ring {
    pub rails: Vec<(u32, u32)>,
    pub faces: Vec<u32>,
    pub closed: bool,
}

/// The edge opposite `(a, b)` in a quad corner loop, or `None` when the pair
/// is not an edge of the loop.
fn opposite_in_quad(cyclic: &[u32], a: u32, b: u32) -> Option<(u32, u32)> {
    if cyclic.len() != 4 {
        return None;
    }
    let pa = cyclic.iter().position(|w| *w == a)?;
    let pb = cyclic.iter().position(|w| *w == b)?;
    let adjacent = (pa + 1) % 4 == pb || (pb + 1) % 4 == pa;
    if !adjacent {
        return None;
    }
    let rest: Vec<u32> = cyclic.iter().copied().filter(|w| *w != a && *w != b).collect();
    if rest.len() != 2 {
        return None;
    }
    Some((rest[0].min(rest[1]), rest[0].max(rest[1])))
}

/// One ring side in progress: the crossed rails and faces so far, the rails
/// already visited, and whether the walk returned to its start edge.
struct Walk {
    rails: Vec<(u32, u32)>,
    faces: Vec<u32>,
    seen: BTreeSet<(u32, u32)>,
    closed: bool,
}

/// Walks one side of the ring from `(rail, face)`, appending rails and crossed
/// faces. Stops at non-quad faces and at rails shared by other than two
/// faces; sets `closed` when the walk returns to `start`.
fn walk_side(mesh: &EditMesh, adj: &Adjacency, start: (u32, u32), face: u32, walk: &mut Walk) {
    let (mut rail, mut face) = (start, face);
    loop {
        if mesh.faces[face as usize].len != 4 {
            break;
        }
        let cyclic = mesh.face_verts(face as usize);
        let Some(next) = opposite_in_quad(&cyclic, rail.0, rail.1) else { break };
        walk.faces.push(face);
        if next == start {
            walk.closed = true;
            break;
        }
        if !walk.seen.insert(next) {
            break;
        }
        walk.rails.push(next);
        let two = adj.edge(next.0, next.1).is_some_and(|e| e.faces.len() == 2);
        if !two {
            break;
        }
        let nf = adj.edge(next.0, next.1).and_then(|e| e.faces.iter().copied().find(|f| *f != face));
        let Some(nf) = nf else { break };
        (rail, face) = (next, nf);
    }
}

/// The full ring through an edge index: both faces walked, joined at the
/// start edge. Errors when the edge is boundary or non-manifold and when both
/// sides stop at once.
pub fn ring_through(mesh: &EditMesh, adj: &Adjacency, edge: u32) -> Result<Ring, EditError> {
    let e = adj.edges.get(edge as usize).ok_or_else(|| bad(format!("loop cut names no edge {edge}")))?;
    if e.faces.len() != 2 {
        return Err(bad("loop cut needs an edge shared by two quads; this one is on a boundary or a non-manifold edge"));
    }
    let start = (e.a, e.b);
    let mut walk = Walk { rails: vec![start], faces: Vec::new(), seen: BTreeSet::from([start]), closed: false };
    walk_side(mesh, adj, start, e.faces[0], &mut walk);
    if walk.closed {
        let ring = Ring { rails: walk.rails, faces: walk.faces, closed: true };
        check_ring(&ring)?;
        return Ok(ring);
    }
    let mut other = Walk { rails: vec![start], faces: Vec::new(), seen: walk.seen, closed: false };
    walk_side(mesh, adj, start, e.faces[1], &mut other);
    if other.closed {
        other.rails.remove(0);
        other.rails.reverse();
        other.rails.extend(walk.rails.into_iter().skip(1));
        other.faces.reverse();
        other.faces.extend(walk.faces);
        let ring = Ring { rails: other.rails, faces: other.faces, closed: true };
        check_ring(&ring)?;
        return Ok(ring);
    }
    let mut rails = other.rails;
    rails.remove(0);
    rails.reverse();
    rails.extend(walk.rails);
    other.faces.reverse();
    other.faces.extend(walk.faces);
    if other.faces.is_empty() {
        return Err(bad("the ring stops at once: the neighbouring faces are not quads"));
    }
    let ring = Ring { rails, faces: other.faces, closed: false };
    check_ring(&ring)?;
    Ok(ring)
}

/// A ring that crosses one face twice cannot split it; refuse before changing
/// anything.
fn check_ring(ring: &Ring) -> Result<(), EditError> {
    let uniq: BTreeSet<u32> = ring.faces.iter().copied().collect();
    if uniq.len() != ring.faces.len() {
        return Err(bad("the ring crosses a face twice; pick another edge"));
    }
    Ok(())
}

fn lerp(a: DVec3, b: DVec3, t: f64) -> DVec3 {
    a + (b - a) * t
}

fn lerp_uv(a: [f32; 2], b: [f32; 2], t: f64) -> [f32; 2] {
    [(a[0] as f64 + (b[0] as f64 - a[0] as f64) * t) as f32, (a[1] as f64 + (b[1] as f64 - a[1] as f64) * t) as f32]
}

pub(crate) fn lerp_normal(a: Option<[f32; 3]>, b: Option<[f32; 3]>, t: f64) -> Option<[f32; 3]> {
    let (a, b) = (a?, b?);
    let n = DVec3::new(
        a[0] as f64 + (b[0] as f64 - a[0] as f64) * t,
        a[1] as f64 + (b[1] as f64 - a[1] as f64) * t,
        a[2] as f64 + (b[2] as f64 - a[2] as f64) * t,
    )
    .normalize_or_zero();
    (n != DVec3::ZERO).then_some([n.x as f32, n.y as f32, n.z as f32])
}

fn corner_uv(mesh: &EditMesh, face: usize, vertex: u32) -> [f32; 2] {
    let f = &mesh.faces[face];
    mesh.corners[f.first as usize..f.first as usize + f.len as usize]
        .iter()
        .find(|c| c.vertex == vertex)
        .map(|c| c.uv)
        .unwrap_or([0.0, 0.0])
}

fn corner_normal(mesh: &EditMesh, face: usize, vertex: u32) -> Option<[f32; 3]> {
    let f = &mesh.faces[face];
    mesh.corners[f.first as usize..f.first as usize + f.len as usize]
        .iter()
        .find(|c| c.vertex == vertex)
        .and_then(|c| c.normal)
}

/// Cut parameters along each rail at even spacing, shifted by `factor` whole
/// intervals and clamped to the rail. Rails are straight, so even parameter
/// spacing is even arc spacing.
fn cut_offsets(cuts: usize, mut factor: f64, flipped: bool) -> Vec<f64> {
    if flipped {
        factor = -factor;
    }
    let step = 1.0 / (cuts as f64 + 1.0);
    let mut deduped: Vec<f64> = Vec::with_capacity(cuts);
    for j in 0..cuts {
        let t = ((j + 1) as f64 * step + factor * step).clamp(0.0, 1.0);
        if deduped.last().is_none_or(|p| (t - *p).abs() > 1e-9) {
            deduped.push(t);
        }
    }
    deduped
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopCutParams {
    #[serde(default)]
    pub edge: Option<u32>,
    #[serde(default = "default_cuts")]
    pub cuts: u32,
    #[serde(default)]
    pub smoothness: f64,
    #[serde(default)]
    pub factor: f64,
    #[serde(default)]
    pub even: bool,
    #[serde(default)]
    pub flipped: bool,
}

fn default_cuts() -> u32 {
    1
}

pub fn loop_cut(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: LoopCutParams = parse_params(params)?;
    let cuts = (p.cuts.clamp(1, 64)) as usize;
    let smoothness = clamp_finite(p.smoothness, 0.0, 1.0, "smoothness")?;
    let factor = clamp_finite(p.factor, -1.0, 1.0, "factor")?;
    let _ = p.even;
    let edge = match p.edge {
        Some(e) => e,
        None => {
            if sel.mode != SelMode::Edge || sel.ids.len() != 1 {
                return Err(bad("loop_cut needs one edge: pass params.edge or select a single edge"));
            }
            sel.ids[0]
        }
    };
    let adj = Adjacency::build(mesh);
    let ring = ring_through(mesh, &adj, edge)?;
    let rails: BTreeSet<(u32, u32)> = ring.rails.iter().copied().collect();
    if mesh.positions.len() + rails.len() * cuts > super::super::MAX_VERTICES {
        return Err(bad("the cut would pass the vertex budget"));
    }
    if mesh.faces.len() + ring.faces.len() * cuts > super::super::MAX_FACES {
        return Err(bad("the cut would pass the face budget"));
    }
    if mesh.corners.len() + ring.faces.len() * cuts * 4 > super::super::MAX_CORNERS {
        return Err(bad("the cut would pass the corner budget"));
    }
    let offsets = cut_offsets(cuts, factor, p.flipped);
    let mut rail_verts: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    for (a, b) in &rails {
        let (pa, pb) = (mesh.positions[*a as usize], mesh.positions[*b as usize]);
        let mut ids = Vec::with_capacity(offsets.len());
        for t in &offsets {
            let v = mesh.positions.len() as u32;
            mesh.positions.push(lerp(pa, pb, *t));
            ids.push(v);
        }
        rail_verts.insert((*a, *b), ids);
    }
    let rows = offsets.len();
    if smoothness > 0.0 {
        relax_rows(mesh, &ring, &rail_verts, rows, smoothness);
    }
    let mut dead: BTreeSet<u32> = BTreeSet::new();
    let mut fresh: Vec<FreshFace> = Vec::new();
    for f in &ring.faces {
        dead.insert(*f);
        let face = mesh.faces[*f as usize];
        let cyclic = mesh.face_verts(*f as usize);
        let edges: Vec<(u32, u32)> =
            (0..4).map(|k| (cyclic[k].min(cyclic[(k + 1) % 4]), cyclic[k].max(cyclic[(k + 1) % 4]))).collect();
        let rail_pos: Vec<usize> = (0..4).filter(|k| rails.contains(&edges[*k])).collect();
        if rail_pos.len() != 2 || (rail_pos[0] + 2) % 4 != rail_pos[1] {
            return Err(bad("the ring crosses a face it cannot split"));
        }
        let r0 = rail_pos[0];
        let rot: Vec<u32> = (0..4).map(|k| cyclic[(r0 + k) % 4]).collect();
        // Both chains run in rot order: strip_a from rot[0] to rot[1] along
        // the first rail, strip_b from rot[3] to rot[2] along the opposite
        // rail, so quad j pairs segment j on both sides.
        let mut strips: Vec<Strip> = Vec::with_capacity(2);
        for (v_from, v_to) in [(rot[0], rot[1]), (rot[3], rot[2])] {
            let key = (v_from.min(v_to), v_from.max(v_to));
            let ids = &rail_verts[&key];
            let forward = v_from == key.0;
            let (uva, uvb) = (corner_uv(mesh, *f as usize, v_from), corner_uv(mesh, *f as usize, v_to));
            let (na, nb) = (corner_normal(mesh, *f as usize, v_from), corner_normal(mesh, *f as usize, v_to));
            let mut chain = vec![v_from];
            let mut uv_chain = vec![uva];
            let mut n_chain = vec![na];
            for (j, t) in offsets.iter().enumerate() {
                let id = if forward { ids[j] } else { ids[rows - 1 - j] };
                let tc = if forward { *t } else { 1.0 - *t };
                chain.push(id);
                uv_chain.push(lerp_uv(uva, uvb, tc));
                n_chain.push(lerp_normal(na, nb, tc));
            }
            chain.push(v_to);
            uv_chain.push(uvb);
            n_chain.push(nb);
            strips.push((chain, uv_chain, n_chain));
        }
        let (strip_a, uv_a, n_a) = &strips[0];
        let (strip_b, uv_b, n_b) = &strips[1];
        for j in 0..=rows {
            fresh.push((
                vec![strip_a[j], strip_a[j + 1], strip_b[j + 1], strip_b[j]],
                vec![uv_a[j], uv_a[j + 1], uv_b[j + 1], uv_b[j]],
                vec![n_a[j], n_a[j + 1], n_b[j + 1], n_b[j]],
                face.material,
                face.flags,
            ));
        }
    }
    drop_faces(mesh, &dead);
    let pushed = fresh.len();
    for (verts, uvs, normals, material, flags) in &fresh {
        let first = mesh.corners.len() as u32;
        for (k, v) in verts.iter().enumerate() {
            mesh.corners.push(Corner { vertex: *v, uv: uvs[k], normal: normals[k] });
        }
        mesh.faces.push(Face { first, len: verts.len() as u32, material: *material, flags: *flags });
    }
    let before = mesh.faces.len() - pushed;
    let mut warnings = Vec::new();
    let dropped = super::drop_degenerate(mesh);
    if dropped > 0 {
        warnings.push(format!("{dropped} degenerate faces were removed"));
    }
    let surviving = mesh.faces.len() - before;
    let made: Vec<u32> = ((mesh.faces.len() - surviving) as u32..mesh.faces.len() as u32).collect();
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}

/// One Laplacian pass over the new cut rows: each cut vertex drifts toward
/// the average of its row neighbours.
fn relax_rows(mesh: &mut EditMesh, ring: &Ring, rail_verts: &BTreeMap<(u32, u32), Vec<u32>>, rows: usize, s: f64) {
    let snapshot = mesh.positions.clone();
    let at = |rail: &(u32, u32), j: usize| rail_verts[rail][j];
    for j in 0..rows {
        for (i, rail) in ring.rails.iter().enumerate() {
            let v = at(rail, j) as usize;
            let mut acc = DVec3::ZERO;
            let mut n = 0;
            if i > 0 {
                acc += snapshot[at(&ring.rails[i - 1], j) as usize];
                n += 1;
            }
            if i + 1 < ring.rails.len() {
                acc += snapshot[at(&ring.rails[i + 1], j) as usize];
                n += 1;
            } else if ring.closed {
                acc += snapshot[at(&ring.rails[0], j) as usize];
                n += 1;
            }
            if n > 0 {
                mesh.positions[v] = lerp(snapshot[v], acc / n as f64, s);
            }
        }
    }
}

/// Cut rows as object-space polylines for the hover preview: one polyline per
/// cut across the rails in walk order (closed for closed rings).
pub fn ring_preview(mesh: &EditMesh, edge: u32, cuts: u32) -> Result<Vec<Vec<[f64; 3]>>, EditError> {
    let cuts = (cuts.clamp(1, 64)) as usize;
    let adj = Adjacency::build(mesh);
    let ring = ring_through(mesh, &adj, edge)?;
    let offsets = cut_offsets(cuts, 0.0, false);
    let mut rows: Vec<Vec<[f64; 3]>> = vec![Vec::new(); offsets.len()];
    for rail in &ring.rails {
        let (pa, pb) = (mesh.positions[rail.0 as usize], mesh.positions[rail.1 as usize]);
        for (j, t) in offsets.iter().enumerate() {
            let p = lerp(pa, pb, *t);
            rows[j].push([p.x, p.y, p.z]);
        }
    }
    if ring.closed {
        for row in &mut rows {
            if let Some(first) = row.first().copied() {
                row.push(first);
            }
        }
    }
    Ok(rows)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeSlideParams {
    #[serde(default)]
    pub factor: f64,
    #[serde(default)]
    pub even: bool,
    #[serde(default)]
    pub flipped: bool,
    #[serde(default = "default_clamp")]
    pub clamp: bool,
}

fn default_clamp() -> bool {
    true
}

pub fn edge_slide(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: EdgeSlideParams = parse_params(params)?;
    let mut factor = clamp_finite(p.factor, -2.0, 2.0, "factor")?;
    if p.clamp {
        factor = factor.clamp(-1.0, 1.0);
    }
    if p.flipped {
        factor = -factor;
    }
    if factor.abs() < 1e-12 {
        return Err(bad("edge_slide factor is zero"));
    }
    let adj = Adjacency::build(mesh);
    let edges = selected_edges(mesh, &adj, sel);
    if edges.is_empty() {
        return Err(bad("edge_slide names no edges"));
    }
    // Each loop vertex needs its side rails: two on an interior loop, one on
    // a boundary loop (the missing side clamps in place).
    let mut moves: BTreeMap<u32, (Option<u32>, Option<u32>)> = BTreeMap::new();
    let mut loop_edges: BTreeSet<(u32, u32)> = BTreeSet::new();
    for chain in &super::order_edge_loops(&edges) {
        let n = chain.len();
        let closed_chain = super::loop_is_closed(chain, &edges);
        for k in 0..n {
            if k + 1 < n {
                let (a, b) = (chain[k], chain[k + 1]);
                loop_edges.insert((a.min(b), a.max(b)));
            }
        }
        if closed_chain {
            let (a, b) = (chain[n - 1], chain[0]);
            loop_edges.insert((a.min(b), a.max(b)));
        }
        // The two sides of one vertex are the two rail directions; consecutive
        // loop vertices pick the side adjacent to the previous pick, so the
        // whole loop slides one way instead of shearing.
        let mut prev_pos: Option<u32> = None;
        for (i, v) in chain.iter().enumerate() {
            let mut loop_nbrs: BTreeSet<u32> = BTreeSet::new();
            if i > 0 {
                loop_nbrs.insert(chain[i - 1]);
            }
            if i + 1 < n {
                loop_nbrs.insert(chain[i + 1]);
            }
            if closed_chain {
                loop_nbrs.insert(chain[(i + 1) % n]);
                loop_nbrs.insert(chain[(i + n - 1) % n]);
            }
            let mut sides: Vec<u32> = Vec::new();
            for e in &adj.edges {
                if e.a == *v && !loop_nbrs.contains(&e.b) {
                    sides.push(e.b);
                } else if e.b == *v && !loop_nbrs.contains(&e.a) {
                    sides.push(e.a);
                }
            }
            sides.sort_unstable();
            sides.dedup();
            if sides.is_empty() || sides.len() > 2 {
                return Err(bad(format!("edge_slide needs a clean quad loop: vertex {v} has {} side edges", sides.len())));
            }
            let (neg, pos) = if sides.len() == 2 {
                let (mut a, mut b) = (sides[0], sides[1]);
                if let Some(p) = prev_pos {
                    let a_adj = adj.edge(p, a).is_some();
                    let b_adj = adj.edge(p, b).is_some();
                    if a_adj && !b_adj {
                        std::mem::swap(&mut a, &mut b);
                    }
                }
                prev_pos = Some(b);
                (Some(a), Some(b))
            } else {
                prev_pos = None;
                (None, Some(sides[0]))
            };
            if moves.insert(*v, (neg, pos)).is_some() {
                return Err(bad(format!("vertex {v} is in two loops; slide one loop at a time")));
            }
        }
    }
    if moves.is_empty() {
        return Err(bad("edge_slide names no loop vertices"));
    }
    let target_of = |sides: &(Option<u32>, Option<u32>)| if factor >= 0.0 { sides.1 } else { sides.0 };
    let mean_len = if p.even {
        let mut total = 0.0;
        let mut count = 0;
        for (v, sides) in &moves {
            if let Some(t) = target_of(sides) {
                total += (mesh.positions[t as usize] - mesh.positions[*v as usize]).length();
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    } else {
        0.0
    };
    let mut warnings = Vec::new();
    let mut moved: BTreeSet<u32> = BTreeSet::new();
    for (v, sides) in &moves {
        let cur = mesh.positions[*v as usize];
        let next = match target_of(sides) {
            None => {
                warnings.push(format!("vertex {v} is already on the boundary and stays"));
                cur
            }
            Some(t) => {
                let end = mesh.positions[t as usize];
                if p.even {
                    let dir = (end - cur).normalize_or_zero();
                    if dir == DVec3::ZERO {
                        cur
                    } else {
                        cur + dir * (factor.abs() * mean_len)
                    }
                } else if factor >= 1.0 || factor <= -1.0 {
                    end
                } else {
                    lerp(cur, end, factor.abs())
                }
            }
        };
        if next != cur {
            if !(super::super::finite_in_range(next.x) && super::super::finite_in_range(next.y) && super::super::finite_in_range(next.z)) {
                return Err(bad("the slide would move a vertex out of range"));
            }
            mesh.positions[*v as usize] = next;
            moved.insert(*v);
        }
    }
    if moved.is_empty() {
        warnings.push("the slide moved nothing".to_string());
    }
    let adj = Adjacency::build(mesh);
    let mut dirty: Vec<u32> = Vec::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let uses = mesh.corners[f.first as usize..f.first as usize + f.len as usize].iter().any(|c| moved.contains(&c.vertex));
        if uses {
            dirty.push(fi as u32);
        }
    }
    mesh.clear_custom_normals(&dirty);
    let ids: Vec<u32> = loop_edges.iter().map(|(a, b)| adj.index_of(*a, *b)).collect();
    Ok((Selection { mode: SelMode::Edge, ids }, warnings))
}

#[cfg(test)]
mod tests {
    use super::super::test_meshes::*;
    use super::*;
    use crate::editmesh::validate::invariants;

    fn edge_of(mesh: &EditMesh, want: impl Fn(&crate::editmesh::adjacency::Edge) -> bool) -> u32 {
        let adj = Adjacency::build(mesh);
        adj.edges.iter().position(|e| want(e)).unwrap() as u32
    }

    fn vertical_edge(mesh: &EditMesh) -> u32 {
        edge_of(mesh, |e| e.faces.len() == 2 && is_vertical(mesh, e.a, e.b))
    }

    fn top_ring_edges(mesh: &EditMesh) -> Vec<u32> {
        let adj = Adjacency::build(mesh);
        adj.edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.faces.len() == 2 && is_top_ring(mesh, e.a, e.b))
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// Edges whose both ends were created after `verts_before`: the new loops
    /// a loop cut leaves, without the split rail fragments.
    fn fresh_loop_edges(mesh: &EditMesh, verts_before: u32) -> Vec<u32> {
        let adj = Adjacency::build(mesh);
        adj.edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.a >= verts_before && e.b >= verts_before)
            .map(|(i, _)| i as u32)
            .collect()
    }

    #[test]
    fn cylinder_closed_ring_counts_are_exact() {
        let mut m = quad_cylinder(12);
        assert_eq!(m.faces.len(), 14);
        let v = vertical_edge(&m);
        let (out, warnings) =
            loop_cut(&mut m, &Selection { mode: SelMode::Edge, ids: vec![v] }, &serde_json::json!({"cuts": 1})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 26);
        assert_eq!(out.ids.len(), 24);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn cylinder_three_cuts_gives_fifty_faces() {
        let mut m = quad_cylinder(12);
        let v = vertical_edge(&m);
        loop_cut(&mut m, &Selection { mode: SelMode::Edge, ids: vec![v] }, &serde_json::json!({"cuts": 3})).unwrap();
        assert_eq!(m.faces.len(), 12 * 4 + 2);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
    }

    #[test]
    fn torus_closed_ring_counts_are_exact() {
        let mut m = quad_torus(8, 6);
        assert_eq!(m.faces.len(), 48);
        let adj = Adjacency::build(&m);
        let start = adj.edges.iter().position(|e| e.faces.len() == 2).unwrap() as u32;
        let ring = ring_through(&m, &adj, start).unwrap();
        assert!(ring.closed);
        let crossed = ring.faces.len();
        assert!(crossed > 0);
        loop_cut(&mut m, &Selection { mode: SelMode::Edge, ids: vec![start] }, &serde_json::json!({"cuts": 2})).unwrap();
        assert_eq!(m.faces.len(), 48 + crossed * 2);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 0);
    }

    #[test]
    fn stair_ring_stops_at_the_triangle_cap() {
        let mut m = strip_with_tri_cap();
        assert_eq!(m.faces.len(), 3);
        let adj = Adjacency::build(&m);
        let rail = adj
            .edges
            .iter()
            .position(|e| {
                let (pa, pb) = (m.positions[e.a as usize], m.positions[e.b as usize]);
                e.faces.len() == 2 && (pa.x - pb.x).abs() < 1e-9 && (pa.y - pb.y).abs() > 0.5
            })
            .unwrap() as u32;
        let ring = ring_through(&m, &Adjacency::build(&m), rail).unwrap();
        assert!(!ring.closed);
        assert_eq!(ring.faces.len(), 2);
        loop_cut(&mut m, &Selection { mode: SelMode::Edge, ids: vec![rail] }, &serde_json::json!({"cuts": 1})).unwrap();
        assert_eq!(m.faces.len(), 5);
        assert_eq!(m.faces.iter().filter(|f| f.len == 3).count(), 1);
        let _ = adj;
    }

    fn edge_uv_mismatch(mesh: &EditMesh) -> f64 {
        let adj = Adjacency::build(mesh);
        let mut worst: f32 = 0.0;
        for e in &adj.edges {
            if e.faces.len() != 2 {
                continue;
            }
            let uvs: Vec<[f32; 2]> = e
                .faces
                .iter()
                .flat_map(|f| {
                    let face = &mesh.faces[*f as usize];
                    mesh.corners[face.first as usize..face.first as usize + face.len as usize]
                        .iter()
                        .filter(|c| c.vertex == e.a || c.vertex == e.b)
                        .map(|c| c.uv)
                })
                .collect();
            for w in uvs.windows(2) {
                worst = worst.max((w[0][0] - w[1][0]).abs()).max((w[0][1] - w[1][1]).abs());
            }
        }
        worst as f64
    }

    #[test]
    fn cut_keeps_uvs_continuous_and_adds_no_seam() {
        let mut m = quad_cylinder(12);
        let before = edge_uv_mismatch(&m);
        let v = vertical_edge(&m);
        let seams_before = m.edge_flags.iter().filter(|(_, _, f)| f.contains(crate::editmesh::EdgeFlags::SEAM)).count();
        loop_cut(&mut m, &Selection { mode: SelMode::Edge, ids: vec![v] }, &serde_json::json!({"cuts": 1})).unwrap();
        let seams_after = m.edge_flags.iter().filter(|(_, _, f)| f.contains(crate::editmesh::EdgeFlags::SEAM)).count();
        assert_eq!(seams_before, seams_after);
        assert!(edge_uv_mismatch(&m) <= before + 1e-5, "the cut introduced a UV discontinuity");
    }

    #[test]
    fn cut_interpolates_custom_normals_along_the_rail() {
        let mut m = quad_cylinder(8);
        for (fi, _) in m.faces.iter().enumerate() {
            let n = m.face_normal(fi);
            let corners = m.faces[fi].first as usize..m.faces[fi].first as usize + m.faces[fi].len as usize;
            for c in &mut m.corners[corners] {
                c.normal = Some([n.x as f32, n.y as f32, n.z as f32]);
            }
        }
        let v = vertical_edge(&m);
        loop_cut(&mut m, &Selection { mode: SelMode::Edge, ids: vec![v] }, &serde_json::json!({"cuts": 1})).unwrap();
        for c in &m.corners {
            let n = c.normal.expect("a cut corner keeps an interpolated normal");
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn boundary_and_non_quad_rings_refuse() {
        let mut open = strip_with_tri_cap();
        let adj = Adjacency::build(&open);
        let boundary = adj.edges.iter().position(|e| e.faces.len() == 1).unwrap() as u32;
        let sel = Selection { mode: SelMode::Edge, ids: vec![boundary] };
        assert!(loop_cut(&mut open, &sel, &serde_json::json!({})).is_err());
        let mut m = quad_cylinder(8);
        let sel = Selection { mode: SelMode::Edge, ids: vec![0, 1] };
        assert!(loop_cut(&mut m, &sel, &serde_json::json!({"cuts": 1})).is_err());
    }

    #[test]
    fn slide_plus_minus_one_lands_exactly_on_the_neighbour_loop() {
        for factor in [1.0, -1.0] {
            let mut m = quad_cylinder(8);
            let before = m.positions.len() as u32;
            let v = vertical_edge(&m);
            loop_cut(&mut m, &Selection { mode: SelMode::Edge, ids: vec![v] }, &serde_json::json!({"cuts": 1})).unwrap();
            let fresh = fresh_loop_edges(&m, before);
            assert_eq!(fresh.len(), 8);
            let (slid, _) = super::super::dispatch(
                "edge_slide",
                &mut m,
                &Selection { mode: SelMode::Edge, ids: fresh },
                &serde_json::json!({"factor": factor}),
            )
            .unwrap();
            assert_eq!(slid.mode, SelMode::Edge);
            assert!(!slid.ids.is_empty());
            let adj = Adjacency::build(&m);
            let mut zs: Vec<f64> = slid
                .ids
                .iter()
                .flat_map(|id| {
                    let e = &adj.edges[*id as usize];
                    [m.positions[e.a as usize].z, m.positions[e.b as usize].z]
                })
                .collect();
            zs.sort_by(|a, b| a.total_cmp(b));
            zs.dedup();
            assert_eq!(zs.len(), 1, "the slid loop is flat after factor {factor}: {zs:?}");
            assert!(zs[0] == 64.0 || zs[0] == 0.0, "factor {factor} left the loop at {}", zs[0]);
        }
    }

    #[test]
    fn slide_boundary_loop_clamps_at_the_mesh_end() {
        let mut m = quad_cylinder(8);
        let top = top_ring_edges(&m);
        assert_eq!(top.len(), 8);
        let sel = Selection { mode: SelMode::Edge, ids: top };
        let (_, warnings) = edge_slide(&mut m, &sel, &serde_json::json!({"factor": 1.0})).unwrap();
        assert!(warnings.is_empty());
        let top_gone = m.positions.iter().filter(|p| (p.z - 64.0).abs() < 1e-9).count();
        assert_eq!(top_gone, 0, "sliding the top ring down lands every vert on z = 0");
        let mut m = quad_cylinder(8);
        let top = top_ring_edges(&m);
        let sel = Selection { mode: SelMode::Edge, ids: top };
        let (_, warnings) = edge_slide(&mut m, &sel, &serde_json::json!({"factor": -1.0})).unwrap();
        assert!(!warnings.is_empty(), "sliding past the boundary reports the clamp");
        assert!(m.positions.iter().filter(|p| (p.z - 64.0).abs() < 1e-9).count() > 0);
    }

    #[test]
    fn slide_clears_normals_only_on_faces_it_moves() {
        let mut m = quad_cylinder(8);
        for (fi, _) in m.faces.iter().enumerate() {
            let n = m.face_normal(fi);
            let corners = m.faces[fi].first as usize..m.faces[fi].first as usize + m.faces[fi].len as usize;
            for c in &mut m.corners[corners] {
                c.normal = Some([n.x as f32, n.y as f32, n.z as f32]);
            }
        }
        let v = vertical_edge(&m);
        let before = m.positions.len() as u32;
        loop_cut(&mut m, &Selection { mode: SelMode::Edge, ids: vec![v] }, &serde_json::json!({"cuts": 1})).unwrap();
        let fresh = fresh_loop_edges(&m, before);
        edge_slide(&mut m, &Selection { mode: SelMode::Edge, ids: fresh }, &serde_json::json!({"factor": 0.5})).unwrap();
        assert!(m.corners.iter().any(|c| c.normal.is_none()));
        let cap = m.faces.iter().position(|f| f.len != 4).expect("the caps stay n-gons");
        let corners = m.faces[cap].first as usize..m.faces[cap].first as usize + m.faces[cap].len as usize;
        assert!(m.corners[corners].iter().all(|c| c.normal.is_some()), "the untouched cap keeps its normals");
    }

    #[test]
    fn ring_preview_returns_one_row_per_cut() {
        let m = quad_cylinder(12);
        let v = vertical_edge(&m);
        let rows = ring_preview(&m, v, 2).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 13);
        assert_eq!(rows[0].first(), rows[0].last());
        let open = strip_with_tri_cap();
        let adj = Adjacency::build(&open);
        let rail = adj.edges.iter().position(|e| e.faces.len() == 2).unwrap() as u32;
        let rows = ring_preview(&open, rail, 1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0].first(), rows[0].last());
    }

    #[test]
    fn loop_cut_is_deterministic() {
        let mut a = quad_cylinder(12);
        let mut b = a.clone();
        let v = vertical_edge(&a);
        let params = serde_json::json!({"cuts": 2, "smoothness": 0.5, "factor": 0.25});
        let sel = Selection { mode: SelMode::Edge, ids: vec![v] };
        loop_cut(&mut a, &sel, &params).unwrap();
        loop_cut(&mut b, &sel, &params).unwrap();
        assert_eq!(crate::editmesh::io::write(&a), crate::editmesh::io::write(&b));
    }

    #[test]
    fn loop_cut_rejects_bad_params() {
        let mut m = quad_cylinder(8);
        let v = vertical_edge(&m);
        let sel = Selection { mode: SelMode::Edge, ids: vec![v] };
        assert!(loop_cut(&mut m, &sel, &serde_json::json!({"cuts": 1, "bogus": true})).is_err());
        assert!(loop_cut(&mut m, &sel, &serde_json::json!({"smoothness": f64::NAN})).is_err());
        assert!(loop_cut(&mut m, &sel, &serde_json::json!({"factor": f64::INFINITY})).is_err());
    }

    #[test]
    fn m4_ops_run_through_the_op_contract() {
        let mut m = quad_cylinder(8);
        let before = m.positions.len() as u32;
        let v = vertical_edge(&m);
        let sel = Selection { mode: SelMode::Edge, ids: vec![v] };
        let (out, _) = super::super::dispatch("loop_cut", &mut m, &sel, &serde_json::json!({"cuts": 1})).unwrap();
        assert_eq!(out.mode, SelMode::Face);
        let fresh = fresh_loop_edges(&m, before);
        let (back, _) =
            super::super::dispatch("edge_slide", &mut m, &Selection { mode: SelMode::Edge, ids: fresh }, &serde_json::json!({"factor": 0.5}))
                .unwrap();
        assert_eq!(back.mode, SelMode::Edge);
    }
}
