//! The base op families, one file each, all through the op contract: the
//! request names the op, the selection and the params; the op returns the new
//! mesh in place plus the resulting selection.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DMat3, DMat4, DVec3};
use serde::de::DeserializeOwned;

use super::{bad, select::*, Adjacency, DEGENERATE_AREA, EditError, EditMesh, WELD_EPS};

pub mod bridge;
pub mod bevel;
pub mod boolean;
pub mod delete;
pub mod extrude;
pub mod fill;
pub mod inset;
pub mod join;
pub mod loopcut;
pub mod mark;
pub mod material;
pub mod merge;
pub mod normals;
pub mod offset;
pub mod primitive;
pub mod separate;
pub mod spin;
pub mod subdiv;
pub mod transform;
pub mod uv;

pub type OpResult = Result<(Selection, Vec<String>), EditError>;

pub fn parse_params<T: DeserializeOwned>(params: &serde_json::Value) -> Result<T, EditError> {
    serde_json::from_value(params.clone()).map_err(|e| bad(format!("bad params: {e}")))
}

pub fn clamp_finite(v: f64, lo: f64, hi: f64, name: &str) -> Result<f64, EditError> {
    if !v.is_finite() {
        return Err(bad(format!("param {name} is not finite")));
    }
    Ok(v.clamp(lo, hi))
}

pub fn clamp_vec3(v: [f64; 3], lo: f64, hi: f64, name: &str) -> Result<[f64; 3], EditError> {
    Ok([clamp_finite(v[0], lo, hi, name)?, clamp_finite(v[1], lo, hi, name)?, clamp_finite(v[2], lo, hi, name)?])
}

/// Faces a selection touches: the faces themselves, or every face incident to
/// a selected edge or vertex.
pub fn selected_faces(mesh: &EditMesh, adj: &Adjacency, sel: &Selection) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    match sel.mode {
        SelMode::Face => out.extend(sel.ids.iter().copied().filter(|f| (*f as usize) < mesh.faces.len())),
        SelMode::Edge => {
            for id in &sel.ids {
                if let Some(e) = adj.edges.get(*id as usize) {
                    out.extend(e.faces.iter().copied());
                }
            }
        }
        SelMode::Vertex => {
            for id in &sel.ids {
                out.extend(adj.faces_of(*id).iter().copied());
            }
        }
    }
    out
}

/// Edges a selection touches.
pub fn selected_edges(mesh: &EditMesh, adj: &Adjacency, sel: &Selection) -> BTreeSet<(u32, u32)> {
    let mut out = BTreeSet::new();
    match sel.mode {
        SelMode::Edge => {
            for id in &sel.ids {
                if let Some(e) = adj.edges.get(*id as usize) {
                    out.insert((e.a, e.b));
                }
            }
        }
        SelMode::Face => {
            for f in selected_faces(mesh, adj, sel) {
                for e in face_edges(mesh, f as usize) {
                    out.insert(e);
                }
            }
        }
        SelMode::Vertex => {
            let verts: BTreeSet<u32> = sel.ids.iter().copied().collect();
            for e in &adj.edges {
                if verts.contains(&e.a) && verts.contains(&e.b) {
                    out.insert((e.a, e.b));
                }
            }
        }
    }
    out
}

/// Vertices a selection touches.
pub fn selected_verts(mesh: &EditMesh, adj: &Adjacency, sel: &Selection) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    match sel.mode {
        SelMode::Vertex => out.extend(sel.ids.iter().copied().filter(|v| (*v as usize) < mesh.positions.len())),
        SelMode::Edge => {
            for id in &sel.ids {
                if let Some(e) = adj.edges.get(*id as usize) {
                    out.insert(e.a);
                    out.insert(e.b);
                }
            }
        }
        SelMode::Face => {
            for f in selected_faces(mesh, adj, sel) {
                out.extend(mesh.face_verts(f as usize));
            }
        }
    }
    out
}

/// Sorted edge pairs of a face.
pub fn face_edges(mesh: &EditMesh, fi: usize) -> Vec<(u32, u32)> {
    let verts = mesh.face_verts(fi);
    let mut out = Vec::with_capacity(verts.len());
    for k in 0..verts.len() {
        let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
        if a != b {
            out.push((a.min(b), a.max(b)));
        }
    }
    out
}

/// Removes faces, rebuilds the corner table, and restores `first` offsets.
pub fn drop_faces(mesh: &mut EditMesh, dead: &BTreeSet<u32>) {
    if dead.is_empty() {
        return;
    }
    let mut faces = Vec::with_capacity(mesh.faces.len() - dead.len());
    let mut corners = Vec::with_capacity(mesh.corners.len());
    for (fi, f) in mesh.faces.iter().enumerate() {
        if dead.contains(&(fi as u32)) {
            continue;
        }
        let first = corners.len() as u32;
        let mut f = *f;
        corners.extend_from_slice(&mesh.corners[f.first as usize..f.first as usize + f.len as usize]);
        f.first = first;
        faces.push(f);
    }
    mesh.faces = faces;
    mesh.corners = corners;
    mesh.edge_flags.retain(|(a, b, _)| {
        let used: BTreeSet<u32> = mesh.corners.iter().map(|c| c.vertex).collect();
        used.contains(a) && used.contains(b)
    });
}

/// Removes unreferenced vertices and remaps corners and edge flags.
pub fn compact(mesh: &mut EditMesh) -> Vec<Option<u32>> {
    let mut used = vec![false; mesh.positions.len()];
    for c in &mesh.corners {
        used[c.vertex as usize] = true;
    }
    let mut remap: Vec<Option<u32>> = vec![None; mesh.positions.len()];
    let mut positions = Vec::new();
    for (i, p) in mesh.positions.iter().enumerate() {
        if used[i] {
            remap[i] = Some(positions.len() as u32);
            positions.push(*p);
        }
    }
    mesh.positions = positions;
    for c in &mut mesh.corners {
        c.vertex = remap[c.vertex as usize].unwrap_or(0);
    }
    let mut flags = Vec::new();
    for (a, b, f) in &mesh.edge_flags {
        if let (Some(na), Some(nb)) = (remap[*a as usize], remap[*b as usize]) {
            if na != nb {
                flags.push((na.min(nb), na.max(nb), *f));
            }
        }
    }
    flags.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    flags.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    mesh.edge_flags = flags;
    remap
}

/// Merges vertices closer than `eps`, drops faces left with fewer than three
/// distinct vertices, and removes exactly duplicated faces.
pub fn weld(mesh: &mut EditMesh, eps: f64) {
    let n = mesh.positions.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let cell = eps.max(1e-12);
    let mut grid: BTreeMap<[i64; 3], Vec<usize>> = BTreeMap::new();
    for (i, p) in mesh.positions.iter().enumerate() {
        let key = [(p.x / cell).floor() as i64, (p.y / cell).floor() as i64, (p.z / cell).floor() as i64];
        grid.entry(key).or_default().push(i);
    }
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
                            if (mesh.positions[*i] - mesh.positions[*j]).length() <= eps {
                                let (ri, rj) = (find(&mut parent, *i), find(&mut parent, *j));
                                if ri != rj {
                                    parent[ri.max(rj)] = ri.min(rj);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut rep: Vec<u32> = vec![0; n];
    for (i, r) in rep.iter_mut().enumerate() {
        *r = find(&mut parent, i) as u32;
    }
    for c in &mut mesh.corners {
        c.vertex = rep[c.vertex as usize];
    }
    let mut dead = BTreeSet::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let verts: Vec<u32> =
            mesh.corners[f.first as usize..f.first as usize + f.len as usize].iter().map(|c| c.vertex).collect();
        let mut distinct = verts.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() < 3 {
            dead.insert(fi as u32);
        }
    }
    drop_faces(mesh, &dead);
    dedupe_faces(mesh);
    compact(mesh);
    let _ = WELD_EPS;
}

/// Removes faces whose vertex loop repeats another face exactly.
pub fn dedupe_faces(mesh: &mut EditMesh) {
    let mut seen: BTreeSet<Vec<u32>> = BTreeSet::new();
    let mut dead = BTreeSet::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let verts: Vec<u32> =
            mesh.corners[f.first as usize..f.first as usize + f.len as usize].iter().map(|c| c.vertex).collect();
        let mut canon = verts.clone();
        if let Some(p) = canon.iter().position(|v| *v == *canon.iter().min().unwrap_or(&0)) {
            canon.rotate_left(p);
        }
        if !seen.insert(canon) {
            dead.insert(fi as u32);
        }
    }
    drop_faces(mesh, &dead);
}

/// Orders boundary-style edges into closed or open loops. Each edge is used
/// once; open chains come back as-is.
pub fn order_edge_loops(edges: &BTreeSet<(u32, u32)>) -> Vec<Vec<u32>> {
    let mut next: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (a, b) in edges {
        next.entry(*a).or_default().push(*b);
        next.entry(*b).or_default().push(*a);
    }
    for v in next.values_mut() {
        v.sort_unstable();
    }
    let mut used: BTreeSet<(u32, u32)> = BTreeSet::new();
    let mut loops = Vec::new();
    for (a, b) in edges.iter() {
        let key = (*a, *b);
        if used.contains(&key) {
            continue;
        }
        let mut chain = vec![*a, *b];
        used.insert(key);
        loop {
            let tip = *chain.last().unwrap();
            let next_v = next.get(&tip).and_then(|v| v.iter().copied().find(|w| !used.contains(&(tip.min(*w), tip.max(*w)))));
            match next_v {
                Some(w) => {
                    used.insert((tip.min(w), tip.max(w)));
                    if w == chain[0] {
                        break;
                    }
                    chain.push(w);
                }
                None => break,
            }
        }
        loops.push(chain);
    }
    loops
}

/// True when a vertex loop closes on itself.
pub fn loop_is_closed(chain: &[u32], edges: &BTreeSet<(u32, u32)>) -> bool {
    chain.len() >= 3 && {
        let (a, b) = (chain[chain.len() - 1], chain[0]);
        edges.contains(&(a.min(b), a.max(b)))
    }
}

/// Orients a boundary loop against its surviving neighbours: every boundary
/// edge must run opposite to the neighbour's traversal, or the new face's
/// shared edges wind the same way as theirs. Edges with no surviving
/// neighbour keep the loop as ordered.
pub fn orient_loop(mesh: &EditMesh, adj: &Adjacency, ordered: Vec<u32>) -> Vec<u32> {
    if ordered.len() < 3 {
        return ordered;
    }
    let (a, b) = (ordered[0], ordered[1]);
    let same_as_neighbour = adj
        .edge(a, b)
        .map(|e| {
            e.faces.iter().any(|nf| {
                let w = mesh.face_verts(*nf as usize);
                (0..w.len()).any(|k| w[k] == a && w[(k + 1) % w.len()] == b)
            })
        })
        .unwrap_or(false);
    if same_as_neighbour {
        let mut rev = ordered;
        rev.reverse();
        rev
    } else {
        ordered
    }
}

/// Carries edge flags across a rebuild that kept vertex identities.
pub fn carry_flags(mesh: &mut EditMesh, saved: &[(u32, u32, super::EdgeFlags)]) {
    for (a, b, f) in saved {
        if (*a as usize) < mesh.positions.len() && (*b as usize) < mesh.positions.len() && a != b {
            let cur = mesh.edge_flag(*a, *b);
            let mut next = cur;
            if f.contains(super::EdgeFlags::SHARP) {
                next.set(super::EdgeFlags::SHARP, true);
            }
            if f.contains(super::EdgeFlags::SEAM) {
                next.set(super::EdgeFlags::SEAM, true);
            }
            if next != cur {
                mesh.set_edge_flag(*a, *b, super::EdgeFlags::SHARP, next.contains(super::EdgeFlags::SHARP));
                mesh.set_edge_flag(*a, *b, super::EdgeFlags::SEAM, next.contains(super::EdgeFlags::SEAM));
            }
        }
    }
}

/// Removes faces below the degenerate area; returns how many went.
pub fn drop_degenerate(mesh: &mut EditMesh) -> usize {
    let dead: BTreeSet<u32> = mesh
        .faces
        .iter()
        .enumerate()
        .filter(|(fi, _)| mesh.face_area(*fi) < DEGENERATE_AREA)
        .map(|(fi, _)| fi as u32)
        .collect();
    let n = dead.len();
    drop_faces(mesh, &dead);
    n
}

pub fn dispatch(
    op: &str,
    mesh: &mut EditMesh,
    sel: &Selection,
    params: &serde_json::Value,
) -> Result<(Selection, Vec<String>), EditError> {
    if op == "add_primitive" {
        return primitive::run(mesh, sel, params);
    }
    if sel.ids.is_empty() {
        return Err(bad("the selection matches nothing"));
    }
    match op {
        "translate" | "rotate" | "scale" => transform::run(op, mesh, sel, params),
        "delete" => delete::run(mesh, sel, params),
        "merge" => merge::run(mesh, sel, params),
        "extrude_region" => extrude::run(mesh, sel, params),
        "fill" => fill::run(mesh, sel, params),
        "recalc_normals" => normals::recalc(mesh, sel, params),
        "flip" => normals::flip(mesh, sel, params),
        "shade" => normals::shade(mesh, sel, params),
        "mark" => mark::run(mesh, sel, params),
        "set_material" => material::run(mesh, sel, params),
        "separate" | "join" | "boolean" => Err(bad(format!("{op} needs the document; it runs through the op request, not bare dispatch"))),
        "split" => separate::split(mesh, sel, params),
        "inset" => inset::run(mesh, sel, params),
        "bevel" => bevel::run(mesh, sel, params),
        "bridge_edge_loops" => bridge::run(mesh, sel, params),
        "uv_box" | "uv_cylinder" | "uv_planar" | "uv_unwrap" | "uv_pack" | "uv_reset" => uv::run(op, mesh, sel, params),
        "loop_cut" => loopcut::loop_cut(mesh, sel, params),
        "edge_slide" => loopcut::edge_slide(mesh, sel, params),
        "subdivide" => subdiv::subdivide(mesh, sel, params),
        "unsubdivide" => subdiv::unsubdivide(mesh, sel, params),
        "dissolve" => subdiv::dissolve(mesh, sel, params),
        "spin" => spin::spin(mesh, sel, params),
        "screw" => spin::screw(mesh, sel, params),
        _ => Err(bad(format!("unknown mesh op {op}"))),
    }
}

/// Appends `src` into `dst` with `matrix` baked into positions (normals
/// rotated), merging material slots by name. Returns the new face ids.
pub fn append_transformed(dst: &mut EditMesh, src: &EditMesh, matrix: DMat4) -> Result<Vec<u32>, EditError> {
    if !matrix.is_finite() {
        return Err(bad("the transform is not finite"));
    }
    if dst.positions.len() + src.positions.len() > super::MAX_VERTICES
        || dst.faces.len() + src.faces.len() > super::MAX_FACES
        || dst.corners.len() + src.corners.len() > super::MAX_CORNERS
        || dst.materials.len() + src.materials.len() > super::MAX_MATERIALS
    {
        return Err(bad("the append would pass the mesh budget"));
    }
    let mut slot: Vec<u16> = Vec::with_capacity(src.materials.len());
    for name in &src.materials {
        match dst.materials.iter().position(|m| m == name) {
            Some(i) => slot.push(i as u16),
            None => {
                slot.push(dst.materials.len() as u16);
                dst.materials.push(name.clone());
            }
        }
    }
    let linear = DMat3::from_mat4(matrix);
    let base = dst.positions.len() as u32;
    for p in &src.positions {
        let q = matrix.transform_point3(*p);
        if !(q.is_finite() && super::finite_in_range(q.x) && super::finite_in_range(q.y) && super::finite_in_range(q.z)) {
            return Err(bad("the append leaves the 1e6 build range"));
        }
        dst.positions.push(q);
    }
    let first_face = dst.faces.len() as u32;
    for f in &src.faces {
        let first = dst.corners.len() as u32;
        for c in &src.corners[f.first as usize..f.first as usize + f.len as usize] {
            let normal = c.normal.and_then(|n| {
                let r = linear * DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64);
                (r != DVec3::ZERO).then(|| {
                    let r = r.normalize();
                    [r.x as f32, r.y as f32, r.z as f32]
                })
            });
            dst.corners.push(super::Corner { vertex: c.vertex + base, uv: c.uv, normal });
        }
        dst.faces.push(super::Face { first, len: f.len, material: slot[f.material as usize], flags: f.flags });
    }
    let mut flags = dst.edge_flags.clone();
    for (a, b, f) in &src.edge_flags {
        let key = ((a + base).min(b + base), (a + base).max(b + base));
        match flags.binary_search_by(|e| (e.0, e.1).cmp(&key)) {
            Ok(i) => {
                let mut cur = flags[i].2;
                if f.contains(super::EdgeFlags::SHARP) {
                    cur.set(super::EdgeFlags::SHARP, true);
                }
                if f.contains(super::EdgeFlags::SEAM) {
                    cur.set(super::EdgeFlags::SEAM, true);
                }
                flags[i].2 = cur;
            }
            Err(i) => flags.insert(i, (key.0, key.1, *f)),
        }
    }
    dst.edge_flags = flags;
    Ok((first_face..dst.faces.len() as u32).collect())
}

/// Quad fixtures the M4 tests share: welded triangle soup has no quads for a
/// ring to cross, so these build quad topology directly, CCW from outside.
#[cfg(test)]
pub(crate) mod test_meshes {
    use glam::DVec3;

    use crate::editmesh::{Corner, EditMesh, Face, FaceFlags};

    fn assemble(positions: Vec<DVec3>, quads: Vec<Vec<u32>>, uvs: Vec<[f32; 2]>) -> EditMesh {
        let mut mesh = EditMesh {
            positions,
            faces: Vec::with_capacity(quads.len()),
            corners: Vec::new(),
            edge_flags: Vec::new(),
            materials: vec!["m".to_string()],
            smooth_angle: None,
        };
        for (fi, q) in quads.iter().enumerate() {
            let first = mesh.corners.len() as u32;
            for (k, v) in q.iter().enumerate() {
                mesh.corners.push(Corner { vertex: *v, uv: uvs[fi * q.len() + k], normal: None });
            }
            mesh.faces.push(Face { first, len: q.len() as u32, material: 0, flags: FaceFlags::empty() });
        }
        mesh
    }

    fn planar_uv(p: DVec3, n: DVec3) -> [f32; 2] {
        let a = n.abs();
        let (u, v) = if a.z >= a.x && a.z >= a.y { (p.x, p.y) } else if a.x >= a.y { (p.y, p.z) } else { (p.x, p.z) };
        [(u / 512.0) as f32, (-v / 512.0) as f32]
    }

    /// Six quads, 64 units a side.
    pub(crate) fn quad_box() -> EditMesh {
        let h = 32.0;
        let positions = vec![
            DVec3::new(-h, -h, -h),
            DVec3::new(h, -h, -h),
            DVec3::new(h, h, -h),
            DVec3::new(-h, h, -h),
            DVec3::new(-h, -h, h),
            DVec3::new(h, -h, h),
            DVec3::new(h, h, h),
            DVec3::new(-h, h, h),
        ];
        let quads = vec![vec![4, 5, 6, 7], vec![3, 2, 1, 0], vec![1, 2, 6, 5], vec![3, 0, 4, 7], vec![2, 3, 7, 6], vec![0, 1, 5, 4]];
        let uvs: Vec<[f32; 2]> = quads
            .iter()
            .map(|q| {
                let pts: Vec<DVec3> = q.iter().map(|v| positions[*v as usize]).collect();
                let n = crate::editmesh::newell(&pts).normalize_or(DVec3::Z);
                q.iter().map(|v| planar_uv(positions[*v as usize], n)).collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .concat();
        assemble(positions, quads, uvs)
    }

    /// Quad side wall plus cap n-gons, `segs` around, radius 32, height 64.
    /// Corner UVs are cylindrical on the wall so the wall has no UV seam.
    pub(crate) fn quad_cylinder(segs: u32) -> EditMesh {
        let (r, h) = (32.0, 64.0);
        let mut positions = Vec::new();
        for k in 0..segs {
            let a = std::f64::consts::TAU * k as f64 / segs as f64;
            positions.push(DVec3::new(a.cos() * r, a.sin() * r, 0.0));
        }
        for k in 0..segs {
            let a = std::f64::consts::TAU * k as f64 / segs as f64;
            positions.push(DVec3::new(a.cos() * r, a.sin() * r, h));
        }
        let mut mesh = EditMesh {
            positions,
            faces: Vec::new(),
            corners: Vec::new(),
            edge_flags: Vec::new(),
            materials: vec!["m".to_string()],
            smooth_angle: None,
        };
        let wall_uv = |k: u32, z: f64| {
            let a = std::f64::consts::TAU * k as f64 / segs as f64;
            [(a * r / 512.0) as f32, (z / 512.0) as f32]
        };
        for k in 0..segs {
            let (b0, b1, t0, t1) = (k, (k + 1) % segs, segs + k, segs + (k + 1) % segs);
            let first = mesh.corners.len() as u32;
            for (v, uv) in [(b0, wall_uv(k, 0.0)), (b1, wall_uv(k + 1, 0.0)), (t1, wall_uv(k + 1, h)), (t0, wall_uv(k, h))] {
                mesh.corners.push(Corner { vertex: v, uv, normal: None });
            }
            mesh.faces.push(Face { first, len: 4, material: 0, flags: FaceFlags::empty() });
        }
        let first = mesh.corners.len() as u32;
        for k in 0..segs {
            let p = mesh.positions[(segs + k) as usize];
            mesh.corners.push(Corner { vertex: segs + k, uv: planar_uv(p, DVec3::Z), normal: None });
        }
        mesh.faces.push(Face { first, len: segs, material: 0, flags: FaceFlags::empty() });
        let first = mesh.corners.len() as u32;
        for k in (0..segs).rev() {
            let p = mesh.positions[k as usize];
            mesh.corners.push(Corner { vertex: k, uv: planar_uv(p, -DVec3::Z), normal: None });
        }
        mesh.faces.push(Face { first, len: segs, material: 0, flags: FaceFlags::empty() });
        mesh
    }

    /// Closed quad torus: `segs` around the major ring, `sides` minor.
    pub(crate) fn quad_torus(segs: u32, sides: u32) -> EditMesh {
        let (major, minor) = (64.0, 16.0);
        let mut positions = Vec::new();
        for i in 0..segs {
            for j in 0..sides {
                let u = std::f64::consts::TAU * i as f64 / segs as f64;
                let v = std::f64::consts::TAU * j as f64 / sides as f64;
                let centre = DVec3::new(u.cos() * major, u.sin() * major, 0.0);
                let n = DVec3::new(u.cos() * v.cos(), u.sin() * v.cos(), v.sin());
                positions.push(centre + n * minor);
            }
        }
        let at = |i: u32, j: u32| (i % segs) * sides + (j % sides);
        let quads: Vec<Vec<u32>> =
            (0..segs).flat_map(|i| (0..sides).map(|j| vec![at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]).collect::<Vec<_>>()).collect();
        let uvs: Vec<[f32; 2]> = quads
            .iter()
            .map(|q| {
                let pts: Vec<DVec3> = q.iter().map(|v| positions[*v as usize]).collect();
                let n = crate::editmesh::newell(&pts).normalize_or(DVec3::Z);
                q.iter().map(|v| planar_uv(positions[*v as usize], n)).collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .concat();
        let mut mesh = assemble(positions, quads, uvs);
        if !crate::editmesh::Adjacency::build(&mesh).is_consistently_wound() {
            for f in &mut mesh.faces {
                let k = f.first as usize..f.first as usize + f.len as usize;
                mesh.corners[k].reverse();
            }
        }
        mesh
    }

    /// Two quads in a strip ending in a triangle cap, all in z = 0. The cap
    /// traverses its shared edge opposite to its neighbour, as winding needs.
    pub(crate) fn strip_with_tri_cap() -> EditMesh {
        let positions = vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
            DVec3::new(2.0, 0.0, 0.0),
            DVec3::new(0.0, 1.0, 0.0),
            DVec3::new(1.0, 1.0, 0.0),
            DVec3::new(2.0, 1.0, 0.0),
            DVec3::new(1.5, 2.0, 0.0),
        ];
        assemble(
            positions,
            vec![vec![0, 1, 4, 3], vec![1, 2, 5, 4], vec![2, 6, 5]],
            vec![[0.0; 2]; 11],
        )
    }

    /// True for the wall-vertical edges of `quad_cylinder`.
    pub(crate) fn is_vertical(mesh: &EditMesh, a: u32, b: u32) -> bool {
        let (pa, pb) = (mesh.positions[a as usize], mesh.positions[b as usize]);
        (pa.x - pb.x).abs() < 1e-9 && (pa.y - pb.y).abs() < 1e-9 && (pa.z - pb.z).abs() > 1.0
    }

    /// True for the top ring edges of `quad_cylinder`.
    pub(crate) fn is_top_ring(mesh: &EditMesh, a: u32, b: u32) -> bool {
        let (pa, pb) = (mesh.positions[a as usize], mesh.positions[b as usize]);
        (pa.z - 64.0).abs() < 1e-9 && (pb.z - 64.0).abs() < 1e-9
    }
}
