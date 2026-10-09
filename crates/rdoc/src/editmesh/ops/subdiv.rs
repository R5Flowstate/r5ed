//! subdivide, unsubdivide and dissolve.
//!
//! Subdivide splits selected faces (quads to a grid, triangles to a triangle
//! grid, n-gons to a centre fan) and fans the unselected neighbours of a cut
//! edge so no T-junction remains. Unsubdivide dissolves 2x2 grid blocks back
//! into n-gons; dissolve merges verts or edge pairs the same way.

use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use super::{clamp_finite, drop_faces, parse_params, selected_edges, selected_faces, selected_verts, OpResult};
use crate::editmesh::{bad, newell, Adjacency, Corner, EditMesh, Face, FaceFlags};
use crate::editmesh::select::{SelMode, Selection};

fn lerp(a: DVec3, b: DVec3, t: f64) -> DVec3 {
    a + (b - a) * t
}

fn lerp_uv(a: [f32; 2], b: [f32; 2], t: f64) -> [f32; 2] {
    [(a[0] as f64 + (b[0] as f64 - a[0] as f64) * t) as f32, (a[1] as f64 + (b[1] as f64 - a[1] as f64) * t) as f32]
}

fn avg_uv(uvs: &[[f32; 2]]) -> [f32; 2] {
    let (mut x, mut y) = (0.0f64, 0.0f64);
    for uv in uvs {
        x += uv[0] as f64;
        y += uv[1] as f64;
    }
    [(x / uvs.len() as f64) as f32, (y / uvs.len() as f64) as f32]
}

fn avg_normal(ns: &[Option<[f32; 3]>]) -> Option<[f32; 3]> {
    let mut acc = DVec3::ZERO;
    for n in ns {
        let n = (*n)?;
        acc += DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64);
    }
    let n = acc.normalize_or_zero();
    (n != DVec3::ZERO).then_some([n.x as f32, n.y as f32, n.z as f32])
}

/// Corner attributes of one face in winding order: vertices, UVs, normals.
type Attrs = (Vec<u32>, Vec<[f32; 2]>, Vec<Option<[f32; 3]>>);
/// One boundary point with its corner attributes.
type Pt = (u32, [f32; 2], Option<[f32; 3]>);

/// Corner attributes of a face in winding order.
fn face_attrs(mesh: &EditMesh, face: usize) -> Attrs {
    let f = &mesh.faces[face];
    let corners = &mesh.corners[f.first as usize..f.first as usize + f.len as usize];
    (corners.iter().map(|c| c.vertex).collect(), corners.iter().map(|c| c.uv).collect(), corners.iter().map(|c| c.normal).collect())
}

/// One subdivided polygon: parallel lists plus the inherited slot and flags.
struct Poly {
    verts: Vec<u32>,
    uvs: Vec<[f32; 2]>,
    normals: Vec<Option<[f32; 3]>>,
    material: u16,
    flags: FaceFlags,
}

/// Boundary loop with cut vertices inserted on cut edges: `(vertex, uv,
/// normal)` in winding order.
fn boundary_with_cuts(
    mesh: &EditMesh,
    face: usize,
    cut: &BTreeSet<(u32, u32)>,
    edge_verts: &BTreeMap<(u32, u32), Vec<u32>>,
    cuts: usize,
) -> Vec<Pt> {
    let (verts, uvs, normals) = face_attrs(mesh, face);
    let n = verts.len();
    let mut out = Vec::with_capacity(n * (cuts + 1));
    for k in 0..n {
        let (a, b) = (verts[k], verts[(k + 1) % n]);
        out.push((a, uvs[k], normals[k]));
        let key = (a.min(b), a.max(b));
        if cut.contains(&key) {
            let ids = &edge_verts[&key];
            let forward = a == key.0;
            for (j, id) in ids.iter().enumerate() {
                let t = (j + 1) as f64 / (cuts as f64 + 1.0);
                let tc = if forward { t } else { 1.0 - t };
                let at = if forward { *id } else { ids[cuts - 1 - j] };
                out.push((at, lerp_uv(uvs[k], uvs[(k + 1) % n], tc), super::loopcut::lerp_normal(normals[k], normals[(k + 1) % n], tc)));
            }
        }
    }
    out
}

/// Centre fan of a boundary loop with an even count: quads around the average
/// centre. Odd loops fan to triangles instead; the caller reports that.
fn fan(loop_pts: &[Pt], material: u16, flags: FaceFlags) -> Vec<Poly> {
    let centre_uv = avg_uv(&loop_pts.iter().map(|(_, uv, _)| *uv).collect::<Vec<_>>());
    let centre_n = avg_normal(&loop_pts.iter().map(|(_, _, n)| *n).collect::<Vec<_>>());
    let m = loop_pts.len();
    let mut polys = Vec::new();
    if m.is_multiple_of(2) {
        for i in 0..m / 2 {
            let (a, b, c) = (loop_pts[(2 * i) % m], loop_pts[(2 * i + 1) % m], loop_pts[(2 * i + 2) % m]);
            polys.push(Poly {
                verts: vec![u32::MAX, a.0, b.0, c.0],
                uvs: vec![centre_uv, a.1, b.1, c.1],
                normals: vec![centre_n, a.2, b.2, c.2],
                material,
                flags,
            });
        }
    } else {
        for i in 0..m {
            let (a, b) = (loop_pts[i], loop_pts[(i + 1) % m]);
            polys.push(Poly {
                verts: vec![u32::MAX, a.0, b.0],
                uvs: vec![centre_uv, a.1, b.1],
                normals: vec![centre_n, a.2, b.2],
                material,
                flags,
            });
        }
    }
    polys
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubdivideParams {
    #[serde(default = "default_cuts")]
    pub cuts: u32,
    #[serde(default)]
    pub smoothness: f64,
    #[serde(default)]
    pub fractal: f64,
    #[serde(default)]
    pub seed: i64,
}

fn default_cuts() -> u32 {
    1
}

pub fn subdivide(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: SubdivideParams = parse_params(params)?;
    let cuts = (p.cuts.clamp(1, 32)) as usize;
    let smoothness = clamp_finite(p.smoothness, 0.0, 1.0, "smoothness")?;
    let fractal = clamp_finite(p.fractal, 0.0, 1.0, "fractal")?;
    let adj = Adjacency::build(mesh);
    let mut group: BTreeSet<u32> = selected_faces(mesh, &adj, sel);
    if sel.mode == SelMode::Edge {
        for (a, b) in selected_edges(mesh, &adj, sel) {
            if let Some(e) = adj.edge(a, b) {
                group.extend(e.faces.iter().copied());
            }
        }
    }
    if group.is_empty() {
        return Err(bad("subdivide names no faces"));
    }
    let mut cut: BTreeSet<(u32, u32)> = BTreeSet::new();
    for f in &group {
        for e in super::face_edges(mesh, *f as usize) {
            cut.insert(e);
        }
    }
    let est_verts = mesh.positions.len() + cut.len() * cuts + mesh.faces.len() + group.len() * cuts * cuts;
    if est_verts > super::super::MAX_VERTICES {
        return Err(bad("the subdivision would pass the vertex budget"));
    }
    if mesh.faces.len() + group.len() * (cuts + 1) * (cuts + 1) > super::super::MAX_FACES {
        return Err(bad("the subdivision would pass the face budget"));
    }
    let mut edge_verts: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    for (a, b) in &cut {
        let (pa, pb) = (mesh.positions[*a as usize], mesh.positions[*b as usize]);
        let mut ids = Vec::with_capacity(cuts);
        for j in 0..cuts {
            let v = mesh.positions.len() as u32;
            mesh.positions.push(lerp(pa, pb, (j + 1) as f64 / (cuts as f64 + 1.0)));
            ids.push(v);
        }
        edge_verts.insert((*a, *b), ids);
    }
    let mut new_verts: BTreeSet<u32> = edge_verts.values().flatten().copied().collect();
    let mut polys: Vec<Poly> = Vec::new();
    let mut warnings = Vec::new();
    for fi in 0..mesh.faces.len() {
        let face = mesh.faces[fi];
        if !group.contains(&(fi as u32)) && !super::face_edges(mesh, fi).iter().any(|e| cut.contains(e)) {
            continue;
        }
        if group.contains(&(fi as u32)) && face.len == 4 {
            polys.extend(quad_grid(mesh, fi, &edge_verts, cuts, &mut new_verts));
        } else if group.contains(&(fi as u32)) && face.len == 3 {
            polys.extend(tri_grid(mesh, fi, &edge_verts, cuts, &mut new_verts));
        } else {
            let chained = boundary_with_cuts(mesh, fi, &cut, &edge_verts, cuts);
            if chained.len() % 2 == 1 {
                warnings.push(format!("a face with {} boundary vertices was fanned with triangles", chained.len()));
            }
            let centre_pos = chained.iter().map(|(v, _, _)| mesh.positions[*v as usize]).sum::<DVec3>() / chained.len() as f64;
            let mut fan_polys = fan(&chained, face.material, face.flags);
            let centre = mesh.positions.len() as u32;
            mesh.positions.push(centre_pos);
            new_verts.insert(centre);
            for poly in &mut fan_polys {
                poly.verts[0] = centre;
            }
            polys.extend(fan_polys);
        }
    }
    let dead: BTreeSet<u32> = (0..mesh.faces.len() as u32)
        .filter(|f| group.contains(f) || super::face_edges(mesh, *f as usize).iter().any(|e| cut.contains(e)))
        .collect();
    drop_faces(mesh, &dead);
    let before = mesh.faces.len();
    for poly in &polys {
        let first = mesh.corners.len() as u32;
        for (k, v) in poly.verts.iter().enumerate() {
            mesh.corners.push(Corner { vertex: *v, uv: poly.uvs[k], normal: poly.normals[k] });
        }
        mesh.faces.push(Face { first, len: poly.verts.len() as u32, material: poly.material, flags: poly.flags });
    }
    if smoothness > 0.0 {
        smooth_new(mesh, &new_verts, smoothness);
    }
    if fractal > 0.0 {
        fractal_new(mesh, &new_verts, fractal, p.seed);
    }
    let dropped = super::drop_degenerate(mesh);
    if dropped > 0 {
        warnings.push(format!("{dropped} degenerate faces were removed"));
    }
    let surviving = mesh.faces.len() - before;
    let made: Vec<u32> = ((mesh.faces.len() - surviving) as u32..mesh.faces.len() as u32).collect();
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}

/// Quad grid: edge rows come from the shared edge map so neighbours agree
/// bit for bit; interior points are face-local.
fn quad_grid(
    mesh: &mut EditMesh,
    fi: usize,
    edge_verts: &BTreeMap<(u32, u32), Vec<u32>>,
    cuts: usize,
    new_verts: &mut BTreeSet<u32>,
) -> Vec<Poly> {
    let n = cuts + 1;
    let (verts, uvs, normals) = face_attrs(mesh, fi);
    let face = mesh.faces[fi];
    let edge_row = |from: u32,
                    to: u32,
                    from_uv: [f32; 2],
                    to_uv: [f32; 2],
                    from_n: Option<[f32; 3]>,
                    to_n: Option<[f32; 3]>| {
        let key = (from.min(to), from.max(to));
        let ids = &edge_verts[&key];
        let forward = from == key.0;
        let mut vs = vec![from];
        let mut us = vec![from_uv];
        let mut ns = vec![from_n];
        for (j, id) in ids.iter().enumerate() {
            let t = (j + 1) as f64 / (cuts as f64 + 1.0);
            let (id, tc) = if forward { (*id, t) } else { (ids[cuts - 1 - j], 1.0 - t) };
            vs.push(id);
            us.push(lerp_uv(from_uv, to_uv, tc));
            ns.push(super::loopcut::lerp_normal(from_n, to_n, tc));
        }
        vs.push(to);
        us.push(to_uv);
        ns.push(to_n);
        (vs, us, ns)
    };
    let (row_a, uv_a, n_a) = edge_row(verts[0], verts[1], uvs[0], uvs[1], normals[0], normals[1]);
    let (row_b, uv_b, n_b) = edge_row(verts[3], verts[2], uvs[3], uvs[2], normals[3], normals[2]);
    let (col_l, uv_l, n_l) = edge_row(verts[0], verts[3], uvs[0], uvs[3], normals[0], normals[3]);
    let (col_r, uv_r, n_r) = edge_row(verts[1], verts[2], uvs[1], uvs[2], normals[1], normals[2]);
    let mut lattice_v: Vec<Vec<u32>> = vec![vec![0; n + 1]; n + 1];
    let mut lattice_uv: Vec<Vec<[f32; 2]>> = vec![vec![[0.0; 2]; n + 1]; n + 1];
    let mut lattice_n: Vec<Vec<Option<[f32; 3]>>> = vec![vec![None; n + 1]; n + 1];
    for i in 0..=n {
        lattice_v[i][0] = row_a[i];
        lattice_uv[i][0] = uv_a[i];
        lattice_n[i][0] = n_a[i];
        lattice_v[i][n] = row_b[i];
        lattice_uv[i][n] = uv_b[i];
        lattice_n[i][n] = n_b[i];
    }
    // Columns reuse the shared edge map too, so neighbours agree bit for bit
    // and no T-junction remains; only the strict interior is face-local.
    lattice_v[0].copy_from_slice(&col_l);
    lattice_uv[0].copy_from_slice(&uv_l);
    lattice_n[0].copy_from_slice(&n_l);
    lattice_v[n].copy_from_slice(&col_r);
    lattice_uv[n].copy_from_slice(&uv_r);
    lattice_n[n].copy_from_slice(&n_r);
    for i in 1..n {
        for j in 1..n {
            let t = j as f64 / n as f64;
            let v = mesh.positions.len() as u32;
            mesh.positions.push(lerp(mesh.positions[row_a[i] as usize], mesh.positions[row_b[i] as usize], t));
            lattice_v[i][j] = v;
            lattice_uv[i][j] = lerp_uv(uv_a[i], uv_b[i], t);
            lattice_n[i][j] = super::loopcut::lerp_normal(n_a[i], n_b[i], t);
            new_verts.insert(v);
        }
    }
    let mut polys = Vec::with_capacity(n * n);
    for i in 0..n {
        for j in 0..n {
            polys.push(Poly {
                verts: vec![lattice_v[i][j], lattice_v[i + 1][j], lattice_v[i + 1][j + 1], lattice_v[i][j + 1]],
                uvs: vec![lattice_uv[i][j], lattice_uv[i + 1][j], lattice_uv[i + 1][j + 1], lattice_uv[i][j + 1]],
                normals: vec![lattice_n[i][j], lattice_n[i + 1][j], lattice_n[i + 1][j + 1], lattice_n[i][j + 1]],
                material: face.material,
                flags: face.flags,
            });
        }
    }
    polys
}

/// Triangle grid with `n = cuts + 1` segments a side: `n * n` triangles.
/// Boundary lattice points reuse the shared edge map.
fn tri_grid(
    mesh: &mut EditMesh,
    fi: usize,
    edge_verts: &BTreeMap<(u32, u32), Vec<u32>>,
    cuts: usize,
    new_verts: &mut BTreeSet<u32>,
) -> Vec<Poly> {
    let n = cuts + 1;
    let (verts, uvs, normals) = face_attrs(mesh, fi);
    let face = mesh.faces[fi];
    let (pa, pb, pc) = (mesh.positions[verts[0] as usize], mesh.positions[verts[1] as usize], mesh.positions[verts[2] as usize]);
    let edge_id = |from: u32, to: u32, num: usize| -> u32 {
        if num == 0 {
            return from;
        }
        if num == n {
            return to;
        }
        let key = (from.min(to), from.max(to));
        let ids = &edge_verts[&key];
        if from == key.0 { ids[num - 1] } else { ids[n - 1 - num] }
    };
    let edge_tc = |from: u32, to: u32, num: usize| -> f64 {
        let t = num as f64 / n as f64;
        let key = (from.min(to), from.max(to));
        if from == key.0 { t } else { 1.0 - t }
    };
    let mut lat: BTreeMap<(usize, usize), Pt> = BTreeMap::new();
    for a in 0..=n {
        for b in 0..=n - a {
            let c = n - a - b;
            let pos = (pa * c as f64 + pb * a as f64 + pc * b as f64) / n as f64;
            let uv = [
                ((uvs[0][0] as f64 * c as f64 + uvs[1][0] as f64 * a as f64 + uvs[2][0] as f64 * b as f64) / n as f64) as f32,
                ((uvs[0][1] as f64 * c as f64 + uvs[1][1] as f64 * a as f64 + uvs[2][1] as f64 * b as f64) / n as f64) as f32,
            ];
            let mut acc = DVec3::ZERO;
            let mut all = true;
            for (k, w) in [c, a, b].iter().enumerate() {
                match normals[k] {
                    Some(nm) => acc += DVec3::new(nm[0] as f64, nm[1] as f64, nm[2] as f64) * *w as f64,
                    None => all = false,
                }
            }
            let normal = if all {
                let q = acc.normalize_or_zero();
                (q != DVec3::ZERO).then_some([q.x as f32, q.y as f32, q.z as f32])
            } else {
                None
            };
            if a == 0 && b == 0 {
                lat.insert((a, b), (verts[0], uvs[0], normals[0]));
            } else if b == 0 {
                let t = edge_tc(verts[0], verts[1], a);
                lat.insert(
                    (a, b),
                    (edge_id(verts[0], verts[1], a), lerp_uv(uvs[0], uvs[1], t), super::loopcut::lerp_normal(normals[0], normals[1], t)),
                );
            } else if a == 0 {
                let t = edge_tc(verts[0], verts[2], b);
                lat.insert(
                    (a, b),
                    (edge_id(verts[0], verts[2], b), lerp_uv(uvs[0], uvs[2], t), super::loopcut::lerp_normal(normals[0], normals[2], t)),
                );
            } else if a + b == n {
                let t = edge_tc(verts[1], verts[2], b);
                lat.insert(
                    (a, b),
                    (edge_id(verts[1], verts[2], b), lerp_uv(uvs[1], uvs[2], t), super::loopcut::lerp_normal(normals[1], normals[2], t)),
                );
            } else {
                let v = mesh.positions.len() as u32;
                mesh.positions.push(pos);
                new_verts.insert(v);
                lat.insert((a, b), (v, uv, normal));
            }
        }
    }
    let mut polys = Vec::with_capacity(n * n);
    for a in 0..n {
        for b in 0..n - a {
            let (p00, p10, p01) = (lat[&(a, b)], lat[&(a + 1, b)], lat[&(a, b + 1)]);
            polys.push(Poly {
                verts: vec![p00.0, p10.0, p01.0],
                uvs: vec![p00.1, p10.1, p01.1],
                normals: vec![p00.2, p10.2, p01.2],
                material: face.material,
                flags: face.flags,
            });
            if a + b + 1 < n {
                let p11 = lat[&(a + 1, b + 1)];
                polys.push(Poly {
                    verts: vec![p10.0, p11.0, p01.0],
                    uvs: vec![p10.1, p11.1, p01.1],
                    normals: vec![p10.2, p11.2, p01.2],
                    material: face.material,
                    flags: face.flags,
                });
            }
        }
    }
    polys
}

/// One Laplacian pass over new vertices toward their neighbour average.
fn smooth_new(mesh: &mut EditMesh, new_verts: &BTreeSet<u32>, s: f64) {
    let adj = Adjacency::build(mesh);
    let snapshot = mesh.positions.clone();
    for v in new_verts {
        let mut acc = DVec3::ZERO;
        let mut n = 0;
        for e in &adj.edges {
            if e.a == *v {
                acc += snapshot[e.b as usize];
                n += 1;
            } else if e.b == *v {
                acc += snapshot[e.a as usize];
                n += 1;
            }
        }
        if n > 0 {
            mesh.positions[*v as usize] = lerp(snapshot[*v as usize], acc / n as f64, s);
        }
    }
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// Displaces new vertices along their vertex normal by hashed noise in
/// `[-fractal, fractal]` of the local mean edge length.
fn fractal_new(mesh: &mut EditMesh, new_verts: &BTreeSet<u32>, fractal: f64, seed: i64) {
    let adj = Adjacency::build(mesh);
    let face_normals: Vec<DVec3> = (0..mesh.faces.len()).map(|f| mesh.face_normal(f)).collect();
    for v in new_verts {
        let mut n = DVec3::ZERO;
        let mut len = 0.0f64;
        let mut count = 0;
        for e in &adj.edges {
            let w = if e.a == *v { Some(e.b) } else if e.b == *v { Some(e.a) } else { None };
            if let Some(w) = w {
                len += (mesh.positions[w as usize] - mesh.positions[*v as usize]).length();
                count += 1;
                for f in &e.faces {
                    n += face_normals[*f as usize];
                }
            }
        }
        if count == 0 || n == DVec3::ZERO {
            continue;
        }
        let h = (splitmix64((seed as u64).wrapping_add((*v as u64).wrapping_mul(0x9E3779B97F4A7C15))) >> 11) as f64 / (1u64 << 53) as f64;
        mesh.positions[*v as usize] += n.normalize() * (h * 2.0 - 1.0) * fractal * (len / count as f64);
    }
}

/// Orients a fill loop like the dead faces it replaces: the loop follows the
/// dead traversal on each border edge, which keeps it opposite to whatever
/// replaces (or survives on) the other side. Runs before the drop, while the
/// dead faces are still in the mesh.
fn orient_like_dead(mesh: &EditMesh, incident: &[u32], chain: Vec<u32>) -> Vec<u32> {
    if chain.len() < 3 {
        return chain;
    }
    let (a, b) = (chain[0], chain[1]);
    for f in incident {
        let w = mesh.face_verts(*f as usize);
        for k in 0..w.len() {
            let (x, y) = (w[k], w[(k + 1) % w.len()]);
            if x == a && y == b {
                return chain;
            }
            if x == b && y == a {
                let mut rev = chain;
                rev.reverse();
                return rev;
            }
        }
    }
    chain
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsubdivideParams {
    #[serde(default = "default_iterations")]
    pub iterations: u32,
}

fn default_iterations() -> u32 {
    1
}

pub fn unsubdivide(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: UnsubdivideParams = parse_params(params)?;
    let iterations = p.iterations.clamp(1, 16);
    let adj = Adjacency::build(mesh);
    let mut scope: Option<BTreeSet<u32>> = None;
    if !sel.ids.is_empty() {
        scope = Some(selected_faces(mesh, &adj, sel));
    }
    let mut made: Vec<u32> = Vec::new();
    let mut warnings = Vec::new();
    for _ in 0..iterations {
        let adj = Adjacency::build(mesh);
        let boundary: BTreeSet<u32> = adj.edges.iter().filter(|e| e.faces.len() != 2).flat_map(|e| [e.a, e.b]).collect();
        let edge_count = |v: u32| adj.edges.iter().filter(|e| e.a == v || e.b == v).count();
        let mut candidates: Vec<u32> = (0..mesh.positions.len() as u32)
            .filter(|v| !boundary.contains(v) && edge_count(*v) == 4 && adj.faces_of(*v).len() == 4)
            .collect();
        candidates.sort_unstable();
        if let Some(scope) = &scope {
            candidates.retain(|v| adj.faces_of(*v).iter().any(|f| scope.contains(f)));
        }
        let mut dissolved: BTreeSet<u32> = BTreeSet::new();
        let mut dead: BTreeSet<u32> = BTreeSet::new();
        let mut fills: Vec<(Vec<u32>, u16, FaceFlags)> = Vec::new();
        for v in candidates {
            if dissolved.contains(&v) {
                continue;
            }
            let incident: Vec<u32> = adj.faces_of(v).to_vec();
            if incident.iter().any(|f| dead.contains(f)) {
                continue;
            }
            if incident.iter().flat_map(|f| mesh.face_verts(*f as usize)).any(|w| dissolved.contains(&w)) {
                continue;
            }
            if incident.iter().any(|f| mesh.faces[*f as usize].len != 4) {
                continue;
            }
            let in_set: BTreeSet<u32> = incident.iter().copied().collect();
            let mut border: BTreeSet<(u32, u32)> = BTreeSet::new();
            for f in &incident {
                let verts = mesh.face_verts(*f as usize);
                for k in 0..verts.len() {
                    let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
                    if a == b {
                        continue;
                    }
                    let inside = adj.edge(a, b).map(|e| e.faces.iter().filter(|nf| in_set.contains(nf)).count()).unwrap_or(0);
                    if inside < 2 {
                        border.insert((a.min(b), a.max(b)));
                    }
                }
            }
            let loops = super::order_edge_loops(&border);
            // Only 2x2 grid blocks dissolve: their border is eight points. An
            // edge midpoint between two original faces has six and would merge
            // across the edge into a bent hexagon instead of coarsening.
            if loops.len() != 1 || !super::loop_is_closed(&loops[0], &border) || loops[0].len() != 8 {
                continue;
            }
            dissolved.insert(v);
            dead.extend(incident.iter().copied());
            let first = &mesh.faces[incident[0] as usize];
            fills.push((orient_like_dead(mesh, &incident, loops[0].clone()), first.material, first.flags));
        }
        if dead.is_empty() {
            warnings.push("no grid vertices left to unsubdivide".to_string());
            break;
        }
        let mut uv_avg: BTreeMap<u32, [f32; 2]> = BTreeMap::new();
        let mut uv_count: BTreeMap<u32, usize> = BTreeMap::new();
        for f in &dead {
            let face = &mesh.faces[*f as usize];
            for c in &mesh.corners[face.first as usize..face.first as usize + face.len as usize] {
                let e = uv_avg.entry(c.vertex).or_insert([0.0, 0.0]);
                e[0] += c.uv[0];
                e[1] += c.uv[1];
                *uv_count.entry(c.vertex).or_insert(0) += 1;
            }
        }
        for (v, uv) in uv_avg.iter_mut() {
            let n = uv_count[v] as f32;
            uv[0] /= n;
            uv[1] /= n;
        }
        drop_faces(mesh, &dead);
        made.clear();
        for (chain, material, flags) in &fills {
            let first = mesh.corners.len() as u32;
            for v in chain.iter() {
                let uv = uv_avg.get(v).copied().unwrap_or([0.0, 0.0]);
                mesh.corners.push(Corner { vertex: *v, uv, normal: None });
            }
            mesh.faces.push(Face { first, len: chain.len() as u32, material: *material, flags: *flags });
            made.push(mesh.faces.len() as u32 - 1);
        }
        super::compact(mesh);
    }
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DissolveMode {
    #[default]
    Edges,
    Verts,
    Limited,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DissolveParams {
    #[serde(default)]
    pub mode: DissolveMode,
    #[serde(default = "default_angle")]
    pub angle: f64,
}

fn default_angle() -> f64 {
    5.0
}

pub fn dissolve(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: DissolveParams = parse_params(params)?;
    let angle = clamp_finite(p.angle, 0.0, 180.0, "angle")?;
    match p.mode {
        DissolveMode::Verts => dissolve_verts(mesh, sel),
        DissolveMode::Edges => dissolve_edges(mesh, sel, None),
        DissolveMode::Limited => dissolve_edges(mesh, sel, Some(angle)),
    }
}

fn dissolve_verts(mesh: &mut EditMesh, sel: &Selection) -> OpResult {
    let adj = Adjacency::build(mesh);
    let verts = selected_verts(mesh, &adj, sel);
    if verts.is_empty() {
        return Err(bad("dissolve verts names no vertices"));
    }
    if verts.len() * mesh.faces.len() > 64_000_000 {
        return Err(bad("the dissolve selection is too large"));
    }
    let mut made: Vec<u32> = Vec::new();
    let mut warnings = Vec::new();
    for v in verts {
        let adj = Adjacency::build(mesh);
        let incident: Vec<u32> = adj.faces_of(v).to_vec();
        if incident.is_empty() {
            continue;
        }
        let in_set: BTreeSet<u32> = incident.iter().copied().collect();
        let mut border: BTreeSet<(u32, u32)> = BTreeSet::new();
        for f in &incident {
            let w = mesh.face_verts(*f as usize);
            for k in 0..w.len() {
                let (a, b) = (w[k], w[(k + 1) % w.len()]);
                if a == b {
                    continue;
                }
                let inside = adj.edge(a, b).map(|e| e.faces.iter().filter(|nf| in_set.contains(nf)).count()).unwrap_or(0);
                if inside < 2 {
                    border.insert((a.min(b), a.max(b)));
                }
            }
        }
        let loops = super::order_edge_loops(&border);
        if loops.len() != 1 || !super::loop_is_closed(&loops[0], &border) {
            warnings.push(format!("vertex {v} has no single boundary loop and was left alone"));
            continue;
        }
        let pts: Vec<DVec3> = loops[0].iter().map(|w| mesh.positions[*w as usize]).collect();
        if newell(&pts).length() * 0.5 < super::super::DEGENERATE_AREA {
            warnings.push(format!("vertex {v} would leave a degenerate face and was left alone"));
            continue;
        }
        let first_face = &mesh.faces[incident[0] as usize];
        let (material, flags) = (first_face.material, first_face.flags);
        let mut uv_avg: BTreeMap<u32, [f32; 2]> = BTreeMap::new();
        let mut uv_count: BTreeMap<u32, usize> = BTreeMap::new();
        for f in &incident {
            let face = &mesh.faces[*f as usize];
            for c in &mesh.corners[face.first as usize..face.first as usize + face.len as usize] {
                let e = uv_avg.entry(c.vertex).or_insert([0.0, 0.0]);
                e[0] += c.uv[0];
                e[1] += c.uv[1];
                *uv_count.entry(c.vertex).or_insert(0) += 1;
            }
        }
        for (w, uv) in uv_avg.iter_mut() {
            let n = uv_count[w] as f32;
            uv[0] /= n;
            uv[1] /= n;
        }
        let chain = orient_like_dead(mesh, &incident, loops[0].clone());
        drop_faces(mesh, &in_set);
        let first = mesh.corners.len() as u32;
        for w in &chain {
            let uv = uv_avg.get(w).copied().unwrap_or([0.0, 0.0]);
            mesh.corners.push(Corner { vertex: *w, uv, normal: None });
        }
        mesh.faces.push(Face { first, len: chain.len() as u32, material, flags });
        made.push(mesh.faces.len() as u32 - 1);
    }
    super::compact(mesh);
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}

fn dihedral_deg(mesh: &EditMesh, a: u32, b: u32) -> Option<f64> {
    let adj = Adjacency::build(mesh);
    let e = adj.edge(a, b)?;
    if e.faces.len() != 2 {
        return None;
    }
    let (na, nb) = (mesh.face_normal(e.faces[0] as usize), mesh.face_normal(e.faces[1] as usize));
    if na == DVec3::ZERO || nb == DVec3::ZERO {
        return None;
    }
    Some(na.dot(nb).clamp(-1.0, 1.0).acos().to_degrees())
}

fn dissolve_edges(mesh: &mut EditMesh, sel: &Selection, limited: Option<f64>) -> OpResult {
    let adj = Adjacency::build(mesh);
    let mut pairs = selected_edges(mesh, &adj, sel);
    if pairs.is_empty() {
        return Err(bad("dissolve edges names no edges"));
    }
    if let Some(limit) = limited {
        pairs.retain(|(a, b)| dihedral_deg(mesh, *a, *b).is_some_and(|d| d < limit));
        if pairs.is_empty() {
            return Ok((Selection { mode: SelMode::Face, ids: vec![] }, vec!["no edge is flatter than the angle limit".to_string()]));
        }
    }
    if pairs.len() * mesh.faces.len() > 64_000_000 {
        return Err(bad("the dissolve selection is too large"));
    }
    let ordered: Vec<(u32, u32)> = pairs.into_iter().collect();
    let mut made: Vec<u32> = Vec::new();
    let mut warnings = Vec::new();
    for (a, b) in ordered {
        let adj = Adjacency::build(mesh);
        let Some(e) = adj.edge(a, b).cloned() else { continue };
        if e.faces.len() != 2 {
            warnings.push(format!("edge ({a}, {b}) bounds other than two faces and was left alone"));
            continue;
        }
        let (f1, f2) = (e.faces[0] as usize, e.faces[1] as usize);
        let (w1, w2) = (mesh.face_verts(f1), mesh.face_verts(f2));
        let i1 = (0..w1.len()).find(|k| w1[*k] == a && w1[(*k + 1) % w1.len()] == b);
        let j1 = (0..w1.len()).find(|k| w1[*k] == b && w1[(*k + 1) % w1.len()] == a);
        let i2 = (0..w2.len()).find(|k| w2[*k] == a && w2[(*k + 1) % w2.len()] == b);
        let j2 = (0..w2.len()).find(|k| w2[*k] == b && w2[(*k + 1) % w2.len()] == a);
        let ((seq_a, seq_b), mat, flags) = match ((i1, j1), (i2, j2)) {
            ((Some(i), None), (None, Some(j))) => {
                let mut s1 = Vec::with_capacity(w1.len() - 1);
                for k in 0..w1.len() - 1 {
                    s1.push(w1[(i + 2 + k) % w1.len()]);
                }
                let mut s2 = Vec::with_capacity(w2.len() - 1);
                for k in 0..w2.len() - 1 {
                    s2.push(w2[(j + 2 + k) % w2.len()]);
                }
                ((s1, s2), mesh.faces[f1].material, mesh.faces[f1].flags)
            }
            ((None, Some(j)), (Some(i), None)) => {
                let mut s1 = Vec::with_capacity(w1.len() - 1);
                for k in 0..w1.len() - 1 {
                    s1.push(w1[(j + 2 + k) % w1.len()]);
                }
                let mut s2 = Vec::with_capacity(w2.len() - 1);
                for k in 0..w2.len() - 1 {
                    s2.push(w2[(i + 2 + k) % w2.len()]);
                }
                ((s1, s2), mesh.faces[f1].material, mesh.faces[f1].flags)
            }
            _ => {
                warnings.push(format!("edge ({a}, {b}) is not wound against its neighbour and was left alone"));
                continue;
            }
        };
        let merged: Vec<u32> = seq_a.into_iter().chain(seq_b).collect();
        if merged.len() < 3 {
            warnings.push(format!("edge ({a}, {b}) would leave a degenerate face and was left alone"));
            continue;
        }
        let mut seen_check: BTreeSet<u32> = BTreeSet::new();
        let mut dup = false;
        for (k, v) in merged.iter().enumerate() {
            if merged[k] == merged[(k + 1) % merged.len()] || !seen_check.insert(*v) {
                dup = true;
            }
        }
        if dup {
            warnings.push(format!("edge ({a}, {b}) would fold the merged face and was left alone"));
            continue;
        }
        let pts: Vec<DVec3> = merged.iter().map(|v| mesh.positions[*v as usize]).collect();
        if newell(&pts).length() * 0.5 < super::super::DEGENERATE_AREA {
            warnings.push(format!("edge ({a}, {b}) would leave a degenerate face and was left alone"));
            continue;
        }
        let attr = |face: usize, v: u32| {
            let f = &mesh.faces[face];
            mesh.corners[f.first as usize..f.first as usize + f.len as usize]
                .iter()
                .find(|c| c.vertex == v)
                .map(|c| (c.uv, c.normal))
                .unwrap_or(([0.0, 0.0], None))
        };
        let in_first: BTreeSet<u32> = w1.into_iter().collect();
        let mut attrs = Vec::with_capacity(merged.len());
        for v in &merged {
            attrs.push(if in_first.contains(v) { attr(f1, *v) } else { attr(f2, *v) });
        }
        let first = mesh.corners.len() as u32;
        for (v, (uv, n)) in merged.iter().zip(attrs) {
            mesh.corners.push(Corner { vertex: *v, uv, normal: n });
        }
        mesh.faces.push(Face { first, len: merged.len() as u32, material: mat, flags });
        drop_faces(mesh, &BTreeSet::from([e.faces[0], e.faces[1]]));
        made.push(mesh.faces.len() as u32 - 1);
    }
    super::compact(mesh);
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}

#[cfg(test)]
mod tests {
    use super::super::test_meshes::*;
    use super::*;
    use crate::editmesh::convert::weld_primitives;
    use crate::editmesh::validate::invariants;
    use crate::import::glb::GlbMesh;
    use crate::meshgen::{build, MeshSpec};

    fn welded_box_tris() -> EditMesh {
        let g = build(&MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "t", "m", 1.0 / 512.0).unwrap();
        weld_primitives(&[GlbMesh { colors: Vec::new(),
            name: g.name,
            material: g.material,
            positions: g.positions,
            normals: g.normals,
            uvs: g.uvs,
            indices: g.indices,
        }])
        .unwrap()
    }

    #[test]
    fn subdivide_one_box_face_cuts_two_stays_closed_without_t_junctions() {
        let mut m = quad_box();
        let sel = Selection { mode: SelMode::Face, ids: vec![0] };
        let (out, warnings) = subdivide(&mut m, &sel, &serde_json::json!({"cuts": 2})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 9 + 4 * 3 + 1);
        assert_eq!(out.ids.len(), 9 + 4 * 3);
        let adj = Adjacency::build(&m);
        assert!(adj.edges.iter().all(|e| e.faces.len() == 2));
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn subdivide_all_box_faces_cuts_one() {
        let mut m = quad_box();
        let sel = Selection { mode: SelMode::Face, ids: vec![0, 1, 2, 3, 4, 5] };
        subdivide(&mut m, &sel, &serde_json::json!({"cuts": 1})).unwrap();
        assert_eq!(m.faces.len(), 24);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn subdivide_triangle_selection_uses_a_triangle_grid() {
        let mut m = strip_with_tri_cap();
        let sel = Selection { mode: SelMode::Face, ids: vec![2] };
        subdivide(&mut m, &sel, &serde_json::json!({"cuts": 2})).unwrap();
        assert_eq!(m.faces.len(), 1 + 9 + 3);
        let inv = invariants(&m);
        assert!(inv.wound && inv.degenerate.is_empty());
        let adj = Adjacency::build(&m);
        assert!(adj.edges.iter().all(|e| e.faces.len() <= 2), "no edge fans out after the straight cut");
        let loops = super::super::order_edge_loops(&inv.boundary.iter().copied().collect());
        assert_eq!(loops.len(), 1, "the outline stays one loop");
    }

    #[test]
    fn subdivide_ngon_cap_stays_closed() {
        let mut m = quad_cylinder(8);
        assert_eq!(m.faces.len(), 10);
        let sel = Selection { mode: SelMode::Face, ids: vec![8] };
        let (_, warnings) = subdivide(&mut m, &sel, &serde_json::json!({"cuts": 1})).unwrap();
        assert!(!warnings.is_empty(), "odd neighbour fans are reported");
        assert_eq!(m.faces.len(), 8 + 8 * 5 + 1);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn subdivide_is_deterministic_with_smooth_and_fractal() {
        let mut a = quad_box();
        let mut b = a.clone();
        let sel = Selection { mode: SelMode::Face, ids: vec![0, 1] };
        let params = serde_json::json!({"cuts": 2, "smoothness": 0.5, "fractal": 0.5, "seed": 7});
        subdivide(&mut a, &sel, &params).unwrap();
        subdivide(&mut b, &sel, &params).unwrap();
        assert_eq!(crate::editmesh::io::write(&a), crate::editmesh::io::write(&b));
    }

    #[test]
    fn fractal_differs_per_seed() {
        let sel = Selection { mode: SelMode::Face, ids: vec![0, 1, 2, 3, 4, 5] };
        let mut a = quad_box();
        let mut b = a.clone();
        subdivide(&mut a, &sel, &serde_json::json!({"cuts": 1, "fractal": 1.0, "seed": 1})).unwrap();
        subdivide(&mut b, &sel, &serde_json::json!({"cuts": 1, "fractal": 1.0, "seed": 2})).unwrap();
        assert_ne!(crate::editmesh::io::write(&a), crate::editmesh::io::write(&b));
    }

    #[test]
    fn unsubdivide_returns_a_subdivided_box_to_six_faces() {
        let mut m = quad_box();
        let all = Selection { mode: SelMode::Face, ids: vec![0, 1, 2, 3, 4, 5] };
        subdivide(&mut m, &all, &serde_json::json!({"cuts": 1})).unwrap();
        assert_eq!(m.faces.len(), 24);
        let relax = Selection { mode: SelMode::Face, ids: vec![] };
        let (_, warnings) = unsubdivide(&mut m, &relax, &serde_json::json!({"iterations": 4})).unwrap();
        assert_eq!(m.faces.len(), 6);
        assert!(m.faces.iter().all(|f| f.len == 8), "each 2x2 block coarsens to one n-gon");
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
        assert!(!warnings.is_empty(), "spent iterations are reported");
    }

    #[test]
    fn limited_dissolve_merges_the_welded_box_tris_into_quads() {
        let mut m = welded_box_tris();
        assert_eq!(m.faces.len(), 12);
        let adj = Adjacency::build(&m);
        let ids: Vec<u32> = (0..adj.edges.len() as u32).collect();
        let sel = Selection { mode: SelMode::Edge, ids };
        dissolve(&mut m, &sel, &serde_json::json!({"mode": "limited", "angle": 5.0})).unwrap();
        assert_eq!(m.faces.len(), 6);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
    }

    #[test]
    fn limited_dissolve_keeps_sharp_edges() {
        let mut m = quad_box();
        let adj = Adjacency::build(&m);
        let ids: Vec<u32> = (0..adj.edges.len() as u32).collect();
        let sel = Selection { mode: SelMode::Edge, ids };
        let (out, warnings) = dissolve(&mut m, &sel, &serde_json::json!({"mode": "limited", "angle": 5.0})).unwrap();
        assert_eq!(m.faces.len(), 6);
        assert!(out.ids.is_empty());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn edge_dissolve_merges_two_box_quads() {
        let mut m = quad_box();
        let adj = Adjacency::build(&m);
        let id = adj.edges.iter().position(|e| e.faces.len() == 2).unwrap() as u32;
        let (out, _) =
            dissolve(&mut m, &Selection { mode: SelMode::Edge, ids: vec![id] }, &serde_json::json!({"mode": "edges"})).unwrap();
        assert_eq!(m.faces.len(), 5);
        assert_eq!(out.ids.len(), 1);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound);
    }

    #[test]
    fn vert_dissolve_merges_a_box_corner_into_one_face() {
        let mut m = quad_box();
        dissolve(&mut m, &Selection { mode: SelMode::Vertex, ids: vec![0] }, &serde_json::json!({"mode": "verts"})).unwrap();
        assert_eq!(m.faces.len(), 4);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound);
    }

    #[test]
    fn subdivide_and_dissolve_reject_bad_params() {
        let mut m = quad_box();
        let face = Selection { mode: SelMode::Face, ids: vec![0] };
        assert!(subdivide(&mut m, &face, &serde_json::json!({"cuts": 1, "bogus": true})).is_err());
        assert!(subdivide(&mut m, &face, &serde_json::json!({"smoothness": f64::NAN})).is_err());
        let edge = Selection { mode: SelMode::Edge, ids: vec![0] };
        assert!(dissolve(&mut m, &edge, &serde_json::json!({"mode": "nope"})).is_err());
        assert!(dissolve(&mut m, &edge, &serde_json::json!({"angle": f64::NAN})).is_err());
        assert!(unsubdivide(&mut m, &face, &serde_json::json!({"iterations": 1, "bogus": true})).is_err());
    }

    #[test]
    fn m4_subdiv_ops_run_through_the_op_contract() {
        let mut m = quad_box();
        let sel = Selection { mode: SelMode::Face, ids: vec![0, 1, 2, 3, 4, 5] };
        let (out, _) = super::super::dispatch("subdivide", &mut m, &sel, &serde_json::json!({"cuts": 1})).unwrap();
        assert_eq!(out.mode, SelMode::Face);
        let all: Vec<u32> = (0..m.faces.len() as u32).collect();
        let (uns, _) = super::super::dispatch(
            "unsubdivide",
            &mut m,
            &Selection { mode: SelMode::Face, ids: all },
            &serde_json::json!({"iterations": 2}),
        )
        .unwrap();
        assert_eq!(uns.mode, SelMode::Face);
        assert_eq!(m.faces.len(), 6);
        let adj = Adjacency::build(&m);
        let ids: Vec<u32> = (0..adj.edges.len() as u32).collect();
        let (dis, _) =
            super::super::dispatch("dissolve", &mut m, &Selection { mode: SelMode::Edge, ids }, &serde_json::json!({"mode": "limited", "angle": 45.0}))
                .unwrap();
        assert_eq!(dis.mode, SelMode::Face);
        let _ = out;
    }
}
