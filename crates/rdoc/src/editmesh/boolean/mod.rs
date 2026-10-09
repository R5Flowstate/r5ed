//! Mesh boolean: arrangement of two closed triangle soups.
//!
//! Both operands triangulate, every triangle pair that overlaps splits both
//! triangles along the intersection (exact orientation tests from `robust`,
//! re-triangulation per face through a `spade` constrained triangulation),
//! each piece classifies inside/outside the other operand by ray-cast majority
//! vote over flood-filled regions, and the kept pieces weld back into n-gons
//! where they share a plane and a material. Coplanar overlaps keep the
//! first operand's copy and drop the second's, so no wall doubles.

pub mod classify;
pub mod intersect;

use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use super::{bad, Adjacency, EditError, EditMesh, DEGENERATE_AREA, MAX_CORNERS, MAX_FACES, MAX_VERTICES};
use crate::editmesh::eval::triangulate_face;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoolOp {
    #[default]
    Difference,
    Union,
    Intersect,
}

/// Exact triangle-triangle tests above this count abort instead of hanging.
pub const MAX_BOOL_PAIRS: usize = 4_000_000;

/// One input triangle with the attributes its fragments inherit.
#[derive(Clone, Debug)]
pub struct SoupTri {
    pub p: [DVec3; 3],
    pub uv: [[f32; 2]; 3],
    pub normal: [Option<[f32; 3]>; 3],
    pub mat: usize,
    pub vid: [u32; 3],
    pub from_b: bool,
}

/// One split fragment awaiting classification.
#[derive(Clone, Debug)]
pub struct Frag {
    pub p: [DVec3; 3],
    pub uv: [[f32; 2]; 3],
    pub normal: [Option<[f32; 3]>; 3],
    pub mat: usize,
    pub from_b: bool,
    /// Per edge (corner k to k+1): true when the edge runs along a cut.
    pub cut: [bool; 3],
    /// Zero-width intent: a vertex resting on its opposite edge. Slivers
    /// never join regions; safe ones are dropped before classification.
    pub sliver: bool,
}

/// Both operands closed, wound and free of degenerate faces, or the boolean
/// has no well-defined inside to classify against.
pub fn check_closed(mesh: &EditMesh, label: &str) -> Result<(), EditError> {
    if mesh.faces.is_empty() {
        return Err(bad(format!("{label} has no faces")));
    }
    let adj = Adjacency::build(mesh);
    let boundary = adj.boundary();
    if !boundary.is_empty() {
        return Err(bad(format!(
            "{label} is open along {} edges; close it with fill before the boolean",
            boundary.len()
        )));
    }
    let non_manifold = adj.non_manifold();
    if !non_manifold.is_empty() {
        let (a, b) = non_manifold[0];
        return Err(bad(format!(
            "{label} is non-manifold at edge {a} {b} ({} more); run merge by distance, then fill",
            non_manifold.len() - 1
        )));
    }
    if !adj.winding_errors().is_empty() {
        return Err(bad(format!("{label} has backwards faces; run recalc normals outside first")));
    }
    let mut degenerate = 0usize;
    for fi in 0..mesh.faces.len() {
        if mesh.face_area(fi) < DEGENERATE_AREA {
            degenerate += 1;
        }
    }
    if degenerate > 0 {
        return Err(bad(format!("{label} has {degenerate} degenerate faces; dissolve them first")));
    }
    Ok(())
}

/// Every face as triangles carrying corner UVs, normals, material and vertex
/// ids. `mat_base` remaps operand-local slots into the merged table.
pub fn soup_of(mesh: &EditMesh, mat_base: usize, from_b: bool) -> Vec<SoupTri> {
    let mut out = Vec::new();
    for f in &mesh.faces {
        let corners = &mesh.corners[f.first as usize..f.first as usize + f.len as usize];
        let pts: Vec<DVec3> = corners.iter().map(|c| mesh.positions[c.vertex as usize]).collect();
        for [a, b, c] in triangulate_face(&pts) {
            out.push(SoupTri {
                p: [pts[a], pts[b], pts[c]],
                uv: [corners[a].uv, corners[b].uv, corners[c].uv],
                normal: [corners[a].normal, corners[b].normal, corners[c].normal],
                mat: mat_base + f.material as usize,
                vid: [corners[a].vertex, corners[b].vertex, corners[c].vertex],
                from_b,
            });
        }
    }
    out
}

/// Material slots merged by name, operand slots appended in first-seen order.
pub fn merge_materials(a: &EditMesh, b: &EditMesh) -> Vec<String> {
    let mut out = a.materials.clone();
    for name in &b.materials {
        if !out.contains(name) {
            out.push(name.clone());
        }
    }
    out
}

fn slot_base(merged: &[String], mesh: &EditMesh) -> usize {
    mesh.materials.first().and_then(|n| merged.iter().position(|m| m == n)).unwrap_or(0)
}

/// Arranges `a` against `b` and keeps the pieces `op` names. Both meshes are
/// in the same space; `keep_material` gives the cut walls the base material
/// instead of the operand's, as the destructive op documents.
pub fn solve(a: &EditMesh, b: &EditMesh, op: BoolOp, keep_material: bool) -> Result<EditMesh, EditError> {
    check_closed(a, "the mesh")?;
    check_closed(b, "the operand")?;
    solve_open(a, b, op, keep_material)
}

/// As `solve` without the closed preconditions: the selected region of a
/// mesh against the rest is two open patches sharing a border.
pub fn solve_open(a: &EditMesh, b: &EditMesh, op: BoolOp, keep_material: bool) -> Result<EditMesh, EditError> {
    check_usable(a, "the mesh")?;
    check_usable(b, "the operand")?;
    solve_unchecked(a, b, op, keep_material)
}

fn check_usable(mesh: &EditMesh, label: &str) -> Result<(), EditError> {
    if mesh.faces.is_empty() {
        return Err(bad(format!("{label} has no faces")));
    }
    let mut degenerate = 0usize;
    for fi in 0..mesh.faces.len() {
        if mesh.face_area(fi) < DEGENERATE_AREA {
            degenerate += 1;
        }
    }
    if degenerate > 0 {
        return Err(bad(format!("{label} has {degenerate} degenerate faces; dissolve them first")));
    }
    Ok(())
}

fn solve_unchecked(
    a: &EditMesh,
    b: &EditMesh,
    op: BoolOp,
    keep_material: bool,
) -> Result<EditMesh, EditError> {
    let materials = merge_materials(a, b);
    let base_b = slot_base(&materials, b);
    let soup_a = soup_of(a, 0, false);
    let soup_b = soup_of(b, base_b, true);
    if soup_a.is_empty() || soup_b.is_empty() {
        return Err(bad("the boolean needs triangles on both sides"));
    }
    let pairs = soup_a.len().saturating_mul(soup_b.len());
    if pairs > MAX_BOOL_PAIRS {
        return Err(bad(format!(
            "the boolean of {} and {} triangles passes the work budget ({pairs} pairs); simplify the operands",
            soup_a.len(),
            soup_b.len()
        )));
    }
    let mut soup = soup_a;
    soup.extend(soup_b);
    let cuts = intersect::arrange(&soup)?;
    let mut frags = intersect::split_all(&soup, &cuts)?;
    let scale = soup_scale(&soup);
    for f in &mut frags {
        f.sliver = is_sliver(&f.p, scale);
    }
    let frags = drop_slivers(frags, scale);
    let kept = classify::label(&soup, &cuts.patches, &frags, op)?;
    assemble(&materials, &kept, op, keep_material, a.smooth_angle, scale)
}

fn soup_scale(soup: &[SoupTri]) -> f64 {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for t in soup {
        for p in &t.p {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
    }
    1.0 + (hi - lo).length()
}

/// Drops zero-width slivers whose every edge keeps zero or at least two
/// faces: the triangulation's rounding debris, never load-bearing surface.
/// Passes repeat to fixpoint so coincident pairs leave together.
pub(crate) fn drop_slivers(frags: Vec<Frag>, scale: f64) -> Vec<Frag> {
    let tris: Vec<[DVec3; 3]> = frags.iter().map(|f| f.p).collect();
    let (_, wid) = weld_positions(&tris);
    let mut live = vec![true; frags.len()];
    loop {
        let mut counts: BTreeMap<[u32; 2], Vec<usize>> = BTreeMap::new();
        for (fi, w) in wid.iter().enumerate() {
            if !live[fi] {
                continue;
            }
            for k in 0..3 {
                if w[k] == w[(k + 1) % 3] {
                    continue;
                }
                counts.entry([w[k].min(w[(k + 1) % 3]), w[k].max(w[(k + 1) % 3])]).or_default().push(fi);
            }
        }
        let mut drop: BTreeSet<usize> = BTreeSet::new();
        let mut grew = true;
        while grew {
            grew = false;
            for (fi, f) in frags.iter().enumerate() {
                if !live[fi] || !f.sliver || drop.contains(&fi) {
                    continue;
                }
                let mut ok = true;
                for k in 0..3 {
                    let (a, b) = (wid[fi][k], wid[fi][(k + 1) % 3]);
                    if a == b {
                        continue;
                    }
                    let users = &counts[&[a.min(b), a.max(b)]];
                    let left = users.iter().filter(|u| live[**u] && !drop.contains(*u)).count();
                    if left == 1 {
                        ok = false;
                        break;
                    }
                    if left == 2 && users.iter().any(|u| *u != fi && frags[*u].sliver && live[*u] && !drop.contains(u)) {
                        continue;
                    }
                    if left == 2 {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    drop.insert(fi);
                    grew = true;
                }
            }
        }
        if drop.is_empty() {
            break;
        }
        for fi in drop {
            live[fi] = false;
        }
    }
    let _ = scale;
    frags.into_iter().enumerate().filter(|(i, _)| live[*i]).map(|(_, f)| f).collect()
}

/// A zero-width triangle: one vertex resting on its opposite edge. Only the
/// triangulation's rounding slivers qualify; genuine features dwarf the
/// tolerance.
pub(crate) fn is_sliver(p: &[DVec3; 3], scale: f64) -> bool {
    for k in 0..3 {
        let (a, b) = (p[(k + 1) % 3], p[(k + 2) % 3]);
        let ab = b - a;
        let len2 = ab.length_squared();
        if len2 == 0.0 {
            return true;
        }
        let t = ((p[k] - a).dot(ab) / len2).clamp(0.0, 1.0);
        if (a + ab * t - p[k]).length() <= 1e-9 * scale {
            return true;
        }
    }
    false
}

/// Welds kept fragments, drops collapsed ones, and greedily merges coplanar
/// same-material neighbours back into n-gons so faces stay editable.
fn assemble(
    materials: &[String],
    frags: &[classify::Kept],
    op: BoolOp,
    keep_material: bool,
    smooth_angle: Option<f64>,
    scale: f64,
) -> Result<EditMesh, EditError> {
    let _ = op;
    let base_mat = frags.iter().filter(|k| !k.from_b).map(|k| k.mat).min().unwrap_or(0);
    let tris: Vec<[DVec3; 3]> = frags.iter().map(|k| k.p).collect();
    let (positions, wid) = weld_positions(&tris);
    let mut mesh = EditMesh {
        positions,
        faces: Vec::new(),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: materials.to_vec(),
        smooth_angle,
    };
    for (k, w) in frags.iter().zip(wid.iter()) {
        let (vs, uvs, normals) = if k.reversed {
            ([w[0], w[2], w[1]], [k.uv[0], k.uv[2], k.uv[1]], [k.normal[0], k.normal[2], k.normal[1]])
        } else {
            ([w[0], w[1], w[2]], [k.uv[0], k.uv[1], k.uv[2]], [k.normal[0], k.normal[1], k.normal[2]])
        };
        if vs[0] == vs[1] || vs[1] == vs[2] || vs[2] == vs[0] {
            continue;
        }
        let mat = if keep_material && k.from_b { base_mat } else { k.mat };
        let first = mesh.corners.len() as u32;
        for i in 0..3 {
            mesh.corners.push(super::Corner { vertex: vs[i], uv: uvs[i], normal: normals[i] });
        }
        mesh.faces.push(super::Face { first, len: 3, material: mat as u16, flags: super::FaceFlags::empty() });
    }
    if mesh.faces.len() * 4 > MAX_CORNERS || mesh.faces.len() > MAX_FACES {
        return Err(bad("the boolean output passes the mesh budget"));
    }
    stitch_tjunctions(&mut mesh, scale);
    merge_ngons(&mut mesh);
    super::ops::compact(&mut mesh);
    super::ops::drop_degenerate(&mut mesh);
    if mesh.positions.len() > MAX_VERTICES {
        return Err(bad("the boolean output passes the vertex budget"));
    }
    Ok(mesh)
}

/// Welds fragment corners to shared vertices: exact bit matching first,
/// then a 1e-7 grid merge for the same geometric point computed through two
/// triangle pairs. Both classification and assembly share these ids, so the
/// regions vote on the topology the output is built from.
pub(crate) fn weld_positions(tris: &[[DVec3; 3]]) -> (Vec<DVec3>, Vec<[u32; 3]>) {
    let mut remap: BTreeMap<[u64; 3], u32> = BTreeMap::new();
    let mut positions: Vec<DVec3> = Vec::new();
    let mut wid: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
    for t in tris {
        let mut w = [0u32; 3];
        for (k, p) in t.iter().enumerate() {
            let key = [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
            let next = positions.len() as u32;
            w[k] = *remap.entry(key).or_insert_with(|| {
                positions.push(*p);
                next
            });
        }
        wid.push(w);
    }
    merge_near(&mut positions, &mut wid);
    (positions, wid)
}

fn merge_near(positions: &mut Vec<DVec3>, wid: &mut [[u32; 3]]) {
    let cell = 1e-7f64;
    let mut grid: BTreeMap<[i64; 3], Vec<u32>> = BTreeMap::new();
    for (i, p) in positions.iter().enumerate() {
        let key = [(p.x / cell).floor() as i64, (p.y / cell).floor() as i64, (p.z / cell).floor() as i64];
        grid.entry(key).or_default().push(i as u32);
    }
    let mut parent: Vec<u32> = (0..positions.len() as u32).collect();
    let find = |mut x: u32, parent: &mut [u32]| -> u32 {
        while parent[x as usize] != x {
            let n = parent[x as usize];
            parent[x as usize] = parent[n as usize];
            x = parent[n as usize];
        }
        x
    };
    let keys: Vec<[i64; 3]> = grid.keys().copied().collect();
    for key in &keys {
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let nk = [key[0] + dx, key[1] + dy, key[2] + dz];
                    if nk < *key {
                        continue;
                    }
                    let (a, b) = match (grid.get(key), grid.get(&nk)) {
                        (Some(a), Some(b)) => (a.clone(), b.clone()),
                        _ => continue,
                    };
                    for i in &a {
                        for j in &b {
                            if i >= j && nk == *key {
                                continue;
                            }
                            if (positions[*i as usize] - positions[*j as usize]).length() <= 1e-7 {
                                let (ri, rj) = (find(*i, &mut parent), find(*j, &mut parent));
                                if ri != rj {
                                    parent[ri.max(rj) as usize] = ri.min(rj);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut rep = vec![0u32; positions.len()];
    for (i, r) in rep.iter_mut().enumerate() {
        *r = find(i as u32, &mut parent);
    }
    if rep.iter().enumerate().any(|(i, r)| *r as usize != i) {
        let mut kept: BTreeMap<u32, u32> = BTreeMap::new();
        let mut next = Vec::new();
        for r in &rep {
            if let std::collections::btree_map::Entry::Vacant(e) = kept.entry(*r) {
                e.insert(next.len() as u32);
                next.push(positions[*r as usize]);
            }
        }
        for w in wid.iter_mut() {
            for v in w.iter_mut() {
                *v = kept[&rep[*v as usize]];
            }
        }
        *positions = next;
    }
}

/// Heals T-junctions the weld cannot see: a kept vertex resting strictly
/// inside a boundary edge splits the face there, so no boundary survives a
/// vertex that sits on it. Runs to fixpoint, bounded by the corner budget.
fn stitch_tjunctions(mesh: &mut EditMesh, scale: f64) {
    let tol = 1e-9 * scale;
    loop {
        if mesh.corners.len() + 1 > MAX_CORNERS {
            break;
        }
        let adj = Adjacency::build(mesh);
        let mut hit: Option<(u32, u32, u32, f64)> = None;
        let mut boundary: Vec<(u32, u32)> = adj.boundary();
        boundary.sort_unstable();
        for (a, b) in boundary {
            let (pa, pb) = (mesh.positions[a as usize], mesh.positions[b as usize]);
            let ab = pb - pa;
            let len2 = ab.length_squared();
            if len2 == 0.0 {
                continue;
            }
            let mut best: Option<(u32, f64)> = None;
            for (vi, p) in mesh.positions.iter().enumerate() {
                let v = vi as u32;
                if v == a || v == b {
                    continue;
                }
                let t = ((*p - pa).dot(ab) / len2).clamp(0.0, 1.0);
                if t <= 1e-9 || t >= 1.0 - 1e-9 {
                    continue;
                }
                if (pa + ab * t - *p).length() <= tol {
                    match best {
                        Some((_, bt)) if (bt - 0.5).abs() <= (t - 0.5).abs() => {}
                        _ => best = Some((v, t)),
                    }
                }
            }
            if let Some((v, t)) = best {
                hit = Some((a, b, v, t));
                break;
            }
        }
        let Some((a, b, v, t)) = hit else { break };
        let edge = adj.edge(a, b).cloned();
        let Some(fi) = edge.as_ref().and_then(|e| e.faces.first().copied()) else { break };
        let loop_vs = mesh.face_verts(fi as usize);
        let n = loop_vs.len();
        let forward = (0..n).any(|k| loop_vs[k] == a && loop_vs[(k + 1) % n] == b);
        let at = (0..n).find(|k| {
            (forward && loop_vs[*k] == a && loop_vs[(*k + 1) % n] == b)
                || (!forward && loop_vs[*k] == b && loop_vs[(*k + 1) % n] == a)
        });
        let Some(k) = at else { break };
        let corners = mesh.face_corners(fi as usize).to_vec();
        let (ca, cb) = (corners[k], corners[(k + 1) % n]);
        let tt = if forward { t } else { 1.0 - t };
        let uv = [
            (ca.uv[0] as f64 + (cb.uv[0] as f64 - ca.uv[0] as f64) * tt) as f32,
            (ca.uv[1] as f64 + (cb.uv[1] as f64 - ca.uv[1] as f64) * tt) as f32,
        ];
        let mut runs: Vec<Vec<super::Corner>> = Vec::with_capacity(mesh.faces.len());
        for (gi, _) in mesh.faces.iter().enumerate() {
            let mut run = mesh.face_corners(gi).to_vec();
            if gi as u32 == fi {
                run.insert(k + 1, super::Corner { vertex: v, uv, normal: None });
            }
            runs.push(run);
        }
        let saved: Vec<(u16, super::FaceFlags)> = mesh.faces.iter().map(|f| (f.material, f.flags)).collect();
        mesh.corners.clear();
        mesh.faces.clear();
        for (run, (mat, flags)) in runs.into_iter().zip(saved) {
            let first = mesh.corners.len() as u32;
            mesh.corners.extend_from_slice(&run);
            mesh.faces.push(super::Face { first, len: run.len() as u32, material: mat, flags });
        }
    }
}
fn merge_ngons(mesh: &mut EditMesh) {
    loop {
        let adj = Adjacency::build(mesh);
        let mut order: Vec<(u32, u32, usize)> = Vec::new();
        for (i, e) in adj.edges.iter().enumerate() {
            if e.faces.len() == 2 {
                order.push((e.a.min(e.b), e.a.max(e.b), i));
            }
        }
        order.sort();
        let mut merged: Option<(Vec<super::Corner>, u16, super::FaceFlags, BTreeSet<u32>)> = None;
        for (_, _, i) in order {
            let e = &adj.edges[i];
            let (fa, fb) = (e.faces[0] as usize, e.faces[1] as usize);
            let (a, b) = (&mesh.faces[fa], &mesh.faces[fb]);
            if a.material != b.material || a.flags != b.flags {
                continue;
            }
            if mesh.edge_flag(e.a, e.b) != super::EdgeFlags::empty() {
                continue;
            }
            if let Some(corners) = merge_pair(mesh, fa, fb, e.a, e.b) {
                let mut dead = BTreeSet::new();
                dead.insert(e.faces[0]);
                dead.insert(e.faces[1]);
                merged = Some((corners, a.material, a.flags, dead));
                break;
            }
        }
        let Some((corners, mat, flags, dead)) = merged else { break };
        super::ops::drop_faces(mesh, &dead);
        let first = mesh.corners.len() as u32;
        for c in &corners {
            mesh.corners.push(*c);
        }
        mesh.faces.push(super::Face { first, len: corners.len() as u32, material: mat, flags });
    }
}

/// The merged corner loop of two faces sharing edge (a, b), or `None` when
/// the union is not a flat simple polygon.
fn merge_pair(mesh: &EditMesh, fa: usize, fb: usize, a: u32, b: u32) -> Option<Vec<super::Corner>> {
    let la = mesh.face_verts(fa);
    let lb = mesh.face_verts(fb);
    let mut looped = Vec::with_capacity(la.len() + lb.len());
    let start = la.iter().position(|v| *v == a)?;
    let mut k = start;
    loop {
        looped.push(la[k]);
        k = (k + 1) % la.len();
        if la[k] == b {
            break;
        }
        if looped.len() > la.len() {
            return None;
        }
    }
    let start = lb.iter().position(|v| *v == b)?;
    let mut k = start;
    loop {
        looped.push(lb[k]);
        k = (k + 1) % lb.len();
        if lb[k] == a {
            break;
        }
        if looped.len() > la.len() + lb.len() {
            return None;
        }
    }
    let mut seen = BTreeSet::new();
    for v in &looped {
        if !seen.insert(*v) {
            return None;
        }
    }
    if looped.len() < 3 {
        return None;
    }
    let pts: Vec<DVec3> = looped.iter().map(|v| mesh.positions[*v as usize]).collect();
    let n = super::newell(&pts);
    let area = n.length() * 0.5;
    if area < DEGENERATE_AREA {
        return None;
    }
    let unit = n / (area * 2.0);
    for p in &pts {
        if ((p - pts[0]).dot(unit)).abs() > 1e-6 * (1.0 + pts[0].length()) {
            return None;
        }
    }
    let mut sum = 0.0;
    sum += mesh.face_area(fa) + mesh.face_area(fb);
    if (area - sum).abs() > 1e-6 * (1.0 + sum) {
        return None;
    }
    let na = mesh.face_normal(fa);
    let nb = mesh.face_normal(fb);
    if na.dot(nb) < 1.0 - 1e-9 {
        return None;
    }
    let ca = mesh.face_corners(fa);
    let cb = mesh.face_corners(fb);
    let at = |v: u32| -> super::Corner {
        if let Some(k) = la.iter().position(|w| *w == v) {
            ca[k]
        } else {
            cb[lb.iter().position(|w| *w == v).unwrap_or(0)]
        }
    };
    Some(looped.iter().map(|v| at(*v)).collect())
}
