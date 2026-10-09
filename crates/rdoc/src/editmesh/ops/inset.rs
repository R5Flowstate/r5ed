//! inset: shrink a face region (or each face on its own) inside its planes,
//! leaving a quad rim strip around the boundary. Single faces offset through
//! the shared polygon helper; region boundaries use the same bisector math in
//! 3D so non-planar regions work too.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

use super::offset::offset_polygon;
use super::{clamp_finite, face_edges, loop_is_closed, order_edge_loops, parse_params, selected_faces, OpResult};
use crate::editmesh::{
    bad, finite_in_range, newell, Adjacency, Corner, EditMesh, Face, FaceFlags, DEGENERATE_AREA,
};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default = "default_thickness")]
    pub thickness: f64,
    #[serde(default)]
    pub depth: f64,
    #[serde(default = "default_true")]
    pub even_offset: bool,
    #[serde(default)]
    pub relative_offset: bool,
    #[serde(default = "default_true")]
    pub boundary: bool,
    #[serde(default)]
    pub outset: bool,
    #[serde(default)]
    pub individual: bool,
    #[serde(default)]
    pub edge_rail: bool,
}

fn default_thickness() -> f64 {
    1.0
}

fn default_true() -> bool {
    true
}

pub fn run(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    let thickness = clamp_finite(p.thickness, 0.0, 1e6, "thickness")?;
    let depth = clamp_finite(p.depth, -1e6, 1e6, "depth")?;
    if thickness < 1e-12 && depth.abs() < 1e-12 {
        return Err(bad("inset thickness and depth are both zero"));
    }
    let adj = Adjacency::build(mesh);
    let region = selected_faces(mesh, &adj, sel);
    if region.is_empty() {
        return Err(bad("inset names no faces"));
    }
    if p.individual {
        run_individual(mesh, &adj, &region, &p, thickness, depth)
    } else {
        run_region(mesh, &adj, &region, &p, thickness, depth)
    }
}

/// One region boundary edge: its single region face and that face's traversal.
struct Border {
    face: u32,
    from: u32,
    to: u32,
}

fn borders(mesh: &EditMesh, adj: &Adjacency, region: &BTreeSet<u32>) -> Result<BTreeMap<(u32, u32), Border>, crate::editmesh::EditError> {
    let mut counts: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    for f in region {
        for e in face_edges(mesh, *f as usize) {
            counts.entry(e).or_default().push(*f);
        }
    }
    let mut out = BTreeMap::new();
    for (e, faces) in &counts {
        if faces.len() != 1 {
            continue;
        }
        let total = adj.edge(e.0, e.1).map(|edge| edge.faces.len()).unwrap_or(0);
        if total > 2 {
            return Err(bad(format!("edge {} is non-manifold; merge by distance or fill it first", adj.index_of(e.0, e.1))));
        }
        let f = faces[0];
        let verts = mesh.face_verts(f as usize);
        let k = verts.iter().position(|v| *v == e.0).unwrap_or(0);
        let (from, to) = if verts[(k + 1) % verts.len()] == e.1 { (e.0, e.1) } else { (e.1, e.0) };
        out.insert(*e, Border { face: f, from, to });
    }
    Ok(out)
}

fn run_region(
    mesh: &mut EditMesh,
    adj: &Adjacency,
    region: &BTreeSet<u32>,
    p: &Params,
    thickness: f64,
    depth: f64,
) -> OpResult {
    let border = borders(mesh, adj, region)?;
    if border.is_empty() {
        let (mut sel, mut warnings) = run_individual(mesh, adj, region, p, thickness, depth)?;
        warnings.push("the whole mesh is selected; each face was inset on its own".to_string());
        sel.ids.sort_unstable();
        return Ok((sel, warnings));
    }
    let edge_set: BTreeSet<(u32, u32)> = border.keys().copied().collect();
    for (e, b) in &border {
        if adj.edge(e.0, e.1).is_some_and(|edge| edge.faces.len() == 1) && !p.boundary {
            return Err(bad(format!("edge {} is on the mesh boundary", adj.index_of(b.from, b.to))));
        }
    }
    let face_normal: BTreeMap<u32, DVec3> = region.iter().map(|f| (*f, mesh.face_normal(*f as usize))).collect();
    let loops = order_edge_loops(&edge_set);
    let mut nbr: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for chain in &loops {
        let closed = loop_is_closed(chain, &edge_set);
        for (k, v) in chain.iter().enumerate() {
            let (a, b) = match (k == 0, k + 1 == chain.len(), closed) {
                (true, true, _) => continue,
                (true, _, _) => (chain[1], if closed { chain[chain.len() - 1] } else { u32::MAX }),
                (_, true, _) => (chain[chain.len() - 2], if closed { chain[0] } else { u32::MAX }),
                _ => (chain[k - 1], chain[k + 1]),
            };
            let list = nbr.entry(*v).or_default();
            for w in [a, b] {
                if w != u32::MAX && !list.contains(&w) {
                    list.push(w);
                }
            }
            list.sort_unstable();
            if list.len() > 2 {
                return Err(bad(format!("vertex {v} sits on more than two region boundary edges")));
            }
        }
    }
    let mut scale = 1.0;
    if p.relative_offset {
        let mut sum = 0.0;
        let mut n = 0usize;
        for (a, b) in edge_set.iter() {
            sum += (mesh.positions[*a as usize] - mesh.positions[*b as usize]).length();
            n += 1;
        }
        if n > 0 {
            scale = sum / n as f64;
        }
    }
    let t = thickness * scale * if p.outset { -1.0 } else { 1.0 };
    let mut ctx: Vec<VertCtx> = Vec::new();
    for (v, ns) in &nbr {
        let ends_open = ns.len() == 1;
        let (prev, next) = (ns[0], if ns.len() > 1 { ns[1] } else { ns[0] });
        let e_prev = (*v.min(&prev), *v.max(&prev));
        let e_next = (*v.min(&next), *v.max(&next));
        for e in [e_prev, e_next] {
            let (a, b) = (mesh.positions[e.0 as usize], mesh.positions[e.1 as usize]);
            if (a - b).length() < 1e-12 {
                return Err(bad(format!("boundary edge {} is degenerate", adj.index_of(e.0, e.1))));
            }
        }
        let b_prev = &border[&e_prev];
        let b_next = &border[&e_next];
        let n_prev = face_normal[&b_prev.face];
        let n_next = face_normal[&b_next.face];
        let n_avg = (n_prev + n_next).normalize_or(n_next);
        let cap = if ends_open {
            (mesh.positions[*v as usize] - mesh.positions[ns[0] as usize]).length() * 0.5
        } else {
            ((mesh.positions[*v as usize] - mesh.positions[prev as usize]).length()
                .min((mesh.positions[*v as usize] - mesh.positions[next as usize]).length()))
                * 0.5
        };
        ctx.push(VertCtx {
            v: *v,
            prev: if ends_open { None } else { Some(prev) },
            next,
            n_avg,
            n_prev,
            n_next,
            e_prev_dir: travel_dir(mesh, b_prev.face, e_prev),
            e_next_dir: travel_dir(mesh, b_next.face, e_next),
            cap: cap.max(0.0),
        });
    }
    ctx.sort_by_key(|c| c.v);
    let want: Vec<DVec3> = ctx.iter().map(|c| shift(mesh, c, t, p.edge_rail, p.even_offset)).collect();
    let depths: Vec<DVec3> = ctx.iter().map(|c| c.n_avg * depth).collect();
    let valid = |s: f64| {
        for (k, c) in ctx.iter().enumerate() {
            let q = mesh.positions[c.v as usize] + want[k] * s + depths[k];
            if !(q.is_finite() && finite_in_range(q.x) && finite_in_range(q.y) && finite_in_range(q.z)) {
                return false;
            }
        }
        let placed: BTreeMap<u32, DVec3> =
            ctx.iter().enumerate().map(|(k, c)| (c.v, mesh.positions[c.v as usize] + want[k] * s + depths[k])).collect();
        if !rims_valid(mesh, &border, &placed, &face_normal, t >= 0.0) {
            return false;
        }
        faces_valid(mesh, region, &placed, &face_normal)
    };
    let mut s = 1.0;
    let mut clamped = false;
    if t.abs() < 1e-12 {
        s = 0.0;
    } else {
        while !valid(s) {
            clamped = true;
            s *= 0.5;
            if s * t.abs() < 1e-9 {
                return Err(bad("the inset collapses; the region is too tight to inset"));
            }
        }
    }
    let applied = t * s;
    if mesh.positions.len() + ctx.len() > super::super::MAX_VERTICES {
        return Err(bad("the inset would pass the vertex budget"));
    }
    if mesh.faces.len() + border.len() > super::super::MAX_FACES
        || mesh.corners.len() + border.len() * 4 > super::super::MAX_CORNERS
    {
        return Err(bad("the inset would pass the face budget"));
    }
    let saved_flags = mesh.edge_flags.clone();
    let mut fresh: BTreeMap<u32, u32> = BTreeMap::new();
    for (k, c) in ctx.iter().enumerate() {
        let q = mesh.positions[c.v as usize] + want[k] * s + depths[k];
        let id = mesh.positions.len() as u32;
        mesh.positions.push(q);
        fresh.insert(c.v, id);
    }
    let mut rewritten: BTreeSet<u32> = BTreeSet::new();
    for f in region {
        rewritten.insert(*f);
    }
    mesh.clear_custom_normals(&rewritten.iter().copied().collect::<Vec<_>>());
    for f in region {
        let face = mesh.faces[*f as usize];
        for k in face.first as usize..face.first as usize + face.len as usize {
            if let Some(nv) = fresh.get(&mesh.corners[k].vertex) {
                mesh.corners[k].vertex = *nv;
            }
        }
    }
    let mut ordered: Vec<((u32, u32), Border)> = border.into_iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, b) in &ordered {
        let na = fresh[&b.from];
        let nb = fresh[&b.to];
        let face = mesh.faces[b.face as usize];
        let (uva, uvb) = (corner_uv(mesh, b.face, b.from), corner_uv(mesh, b.face, b.to));
        let first = mesh.corners.len() as u32;
        mesh.corners.push(Corner { vertex: b.from, uv: uva, normal: None });
        mesh.corners.push(Corner { vertex: b.to, uv: uvb, normal: None });
        mesh.corners.push(Corner { vertex: nb, uv: uvb, normal: None });
        mesh.corners.push(Corner { vertex: na, uv: uva, normal: None });
        mesh.faces.push(Face { first, len: 4, material: face.material, flags: face.flags });
    }
    super::carry_flags(mesh, &saved_flags);
    super::compact(mesh);
    let mut warnings = Vec::new();
    if clamped {
        warnings.push(format!("thickness clamped to {applied:.6} to avoid overlap"));
    }
    let ids: Vec<u32> = rewritten.into_iter().collect();
    Ok((Selection { mode: SelMode::Face, ids }, warnings))
}

struct VertCtx {
    v: u32,
    prev: Option<u32>,
    next: u32,
    n_avg: DVec3,
    n_prev: DVec3,
    n_next: DVec3,
    e_prev_dir: DVec3,
    e_next_dir: DVec3,
    cap: f64,
}

fn travel_dir(mesh: &EditMesh, face: u32, edge: (u32, u32)) -> DVec3 {
    let verts = mesh.face_verts(face as usize);
    let k = verts.iter().position(|v| *v == edge.0).unwrap_or(0);
    let nxt = verts[(k + 1) % verts.len()];
    let (a, b) = if nxt == edge.1 { (edge.0, edge.1) } else { (edge.1, edge.0) };
    (mesh.positions[b as usize] - mesh.positions[a as usize]).normalize_or_zero()
}

fn corner_uv(mesh: &EditMesh, face: u32, vert: u32) -> [f32; 2] {
    let f = mesh.faces[face as usize];
    for c in &mesh.corners[f.first as usize..f.first as usize + f.len as usize] {
        if c.vertex == vert {
            return c.uv;
        }
    }
    [0.0, 0.0]
}

fn shift(mesh: &EditMesh, c: &VertCtx, t: f64, rail: bool, even: bool) -> DVec3 {
    let p = mesh.positions[c.v as usize];
    let inward_next = c.n_next.cross(c.e_next_dir).normalize_or_zero();
    let capped = t.clamp(-c.cap, c.cap);
    match c.prev {
        None => {
            let along = (mesh.positions[c.next as usize] - p).normalize_or_zero();
            (along + inward_next).normalize_or(inward_next) * capped
        }
        Some(_) => {
            if rail {
                rail_shift(mesh, c, capped).unwrap_or_else(|| bisector_shift(c, capped, even))
            } else {
                bisector_shift(c, capped, even)
            }
        }
    }
}

fn bisector_shift(c: &VertCtx, capped: f64, even: bool) -> DVec3 {
    let p1 = c.n_prev.cross(c.e_prev_dir).normalize_or_zero();
    let p2 = c.n_next.cross(c.e_next_dir).normalize_or_zero();
    let sum = p1 + p2;
    if sum.length() < 1e-9 {
        return p1 * capped;
    }
    let along = sum.normalize();
    if even {
        along * (2.0 * capped / sum.length())
    } else {
        along * capped
    }
}

fn rail_shift(mesh: &EditMesh, c: &VertCtx, capped: f64) -> Option<DVec3> {
    let p = mesh.positions[c.v as usize];
    let a_prev = mesh.positions[c.prev? as usize];
    let a_next = mesh.positions[c.next as usize];
    let q1 = c.n_prev.cross(c.e_prev_dir).normalize_or_zero();
    let q2 = c.n_next.cross(c.e_next_dir).normalize_or_zero();
    let l1 = a_prev + q1 * capped;
    let d1 = (p - a_prev).normalize_or_zero();
    let l2 = a_next + q2 * capped;
    let d2 = (p - a_next).normalize_or_zero();
    closest_mid(l1, d1, l2, d2).map(|q| q - p)
}

fn closest_mid(p1: DVec3, d1: DVec3, p2: DVec3, d2: DVec3) -> Option<DVec3> {
    let r = p1 - p2;
    let a = d1.dot(d1);
    let e = d2.dot(d2);
    let b = d1.dot(d2);
    let denom = a * e - b * b;
    if denom.abs() < 1e-12 {
        return None;
    }
    let c = d1.dot(r);
    let f = d2.dot(r);
    let s = (b * f - c * e) / denom;
    let t = (a * f - b * c) / denom;
    Some((p1 + d1 * s + p2 + d2 * t) * 0.5)
}

fn rims_valid(
    mesh: &EditMesh,
    border: &BTreeMap<(u32, u32), Border>,
    placed: &BTreeMap<u32, DVec3>,
    normals: &BTreeMap<u32, DVec3>,
    inward: bool,
) -> bool {
    for b in border.values() {
        let moved_quad = [mesh.positions[b.from as usize], mesh.positions[b.to as usize], placed[&b.to], placed[&b.from]];
        let n = newell(&moved_quad);
        if n.length() * 0.5 < DEGENERATE_AREA {
            return false;
        }
        if inward && n.normalize().dot(normals[&b.face]) <= 0.0 {
            return false;
        }
    }
    true
}

fn faces_valid(
    mesh: &EditMesh,
    region: &BTreeSet<u32>,
    placed: &BTreeMap<u32, DVec3>,
    normals: &BTreeMap<u32, DVec3>,
) -> bool {
    for f in region {
        let pts: Vec<DVec3> = mesh.face_corners(*f as usize)
            .iter()
            .map(|c| placed.get(&c.vertex).copied().unwrap_or(mesh.positions[c.vertex as usize]))
            .collect();
        let n = newell(&pts);
        if n.length() * 0.5 < DEGENERATE_AREA {
            return false;
        }
        if n.normalize().dot(normals[f]) <= 0.0 {
            return false;
        }
    }
    true
}

fn run_individual(
    mesh: &mut EditMesh,
    adj: &Adjacency,
    region: &BTreeSet<u32>,
    p: &Params,
    thickness: f64,
    depth: f64,
) -> OpResult {
    for f in region {
        for e in face_edges(mesh, *f as usize) {
            if adj.edge(e.0, e.1).is_some_and(|edge| edge.faces.len() > 2) {
                return Err(bad(format!("edge {} is non-manifold; merge by distance or fill it first", adj.index_of(e.0, e.1))));
            }
        }
    }
    struct Plan {
        face: u32,
        loop3: Vec<DVec3>,
        inset3: Vec<DVec3>,
        material: u16,
        flags: FaceFlags,
        uvs: Vec<[f32; 2]>,
    }
    let mut plans: Vec<Plan> = Vec::new();
    let mut clamped_faces = 0usize;
    for f in region {
        let n = mesh.face_normal(*f as usize);
        if n == DVec3::ZERO {
            return Err(bad(format!("face {f} is degenerate")));
        }
        let corners = mesh.face_corners(*f as usize).to_vec();
        let pts: Vec<DVec3> = corners.iter().map(|c| mesh.positions[c.vertex as usize]).collect();
        let seed = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
        let u = n.cross(seed).normalize_or(DVec3::X);
        let v = n.cross(u).normalize_or(DVec3::Y);
        let c0 = pts.iter().copied().sum::<DVec3>() / pts.len() as f64;
        let flat: Vec<DVec2> = pts.iter().map(|q| DVec2::new((*q - c0).dot(u), (*q - c0).dot(v))).collect();
        let mut scale = 1.0;
        if p.relative_offset {
            let mut sum = 0.0;
            for k in 0..pts.len() {
                sum += (pts[k] - pts[(k + 1) % pts.len()]).length();
            }
            scale = sum / pts.len() as f64;
        }
        let ask = thickness * scale * if p.outset { -1.0 } else { 1.0 };
        let Some(out) = offset_polygon(&flat, ask, p.even_offset) else {
            return Err(bad(format!("face {f} collapses; the region is too tight to inset")));
        };
        if out.clamped {
            clamped_faces += 1;
        }
        let inset3: Vec<DVec3> = out.points.iter().map(|q| c0 + u * q.x + v * q.y + n * depth).collect();
        for q in &inset3 {
            if !(q.is_finite() && finite_in_range(q.x) && finite_in_range(q.y) && finite_in_range(q.z)) {
                return Err(bad("the inset leaves the 1e6 build range"));
            }
        }
        plans.push(Plan {
            face: *f,
            loop3: pts,
            inset3,
            material: mesh.faces[*f as usize].material,
            flags: mesh.faces[*f as usize].flags,
            uvs: corners.iter().map(|c| c.uv).collect(),
        });
    }
    plans.sort_by_key(|q| q.face);
    let new_verts: usize = plans.iter().map(|q| q.inset3.len()).sum();
    let new_faces: usize = plans.iter().map(|q| q.loop3.len() + 1).sum();
    let new_corners: usize = plans.iter().map(|q| q.loop3.len() * 5).sum();
    if mesh.positions.len() + new_verts > super::super::MAX_VERTICES
        || mesh.faces.len() + new_faces > super::super::MAX_FACES
        || mesh.corners.len() + new_corners > super::super::MAX_CORNERS
    {
        return Err(bad("the inset would pass the mesh budget"));
    }
    let saved_flags = mesh.edge_flags.clone();
    let dead: BTreeSet<u32> = region.clone();
    let mut made: Vec<u32> = Vec::new();
    for plan in &plans {
        let mut ring: Vec<u32> = Vec::with_capacity(plan.inset3.len());
        for q in &plan.inset3 {
            let id = mesh.positions.len() as u32;
            mesh.positions.push(*q);
            ring.push(id);
        }
        let first = mesh.corners.len() as u32;
        for (k, v) in ring.iter().enumerate() {
            mesh.corners.push(Corner { vertex: *v, uv: plan.uvs[k], normal: None });
        }
        mesh.faces.push(Face { first, len: ring.len() as u32, material: plan.material, flags: plan.flags });
        made.push(mesh.faces.len() as u32 - 1);
        let verts = mesh.face_verts(plan.face as usize);
        for k in 0..verts.len() {
            let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
            let wall = mesh.corners.len() as u32;
            mesh.corners.push(Corner { vertex: a, uv: plan.uvs[k], normal: None });
            mesh.corners.push(Corner { vertex: b, uv: plan.uvs[(k + 1) % verts.len()], normal: None });
            mesh.corners.push(Corner { vertex: ring[(k + 1) % ring.len()], uv: plan.uvs[(k + 1) % verts.len()], normal: None });
            mesh.corners.push(Corner { vertex: ring[k], uv: plan.uvs[k], normal: None });
            mesh.faces.push(Face { first: wall, len: 4, material: plan.material, flags: plan.flags });
        }
    }
    super::drop_faces(mesh, &dead);
    super::carry_flags(mesh, &saved_flags);
    super::compact(mesh);
    made.sort_unstable();
    let mut warnings = Vec::new();
    if clamped_faces > 0 {
        warnings.push(format!("thickness clamped on {clamped_faces} faces to avoid overlap"));
    }
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editmesh::validate::invariants;

    fn quad_box_at(c: [f64; 3]) -> EditMesh {
        let (cx, cy, cz) = (c[0], c[1], c[2]);
        let s = 32.0;
        let positions = vec![
            DVec3::new(cx - s, cy - s, cz - s),
            DVec3::new(cx + s, cy - s, cz - s),
            DVec3::new(cx + s, cy + s, cz - s),
            DVec3::new(cx - s, cy + s, cz - s),
            DVec3::new(cx - s, cy - s, cz + s),
            DVec3::new(cx + s, cy - s, cz + s),
            DVec3::new(cx + s, cy + s, cz + s),
            DVec3::new(cx - s, cy + s, cz + s),
        ];
        let loops: Vec<Vec<u32>> =
            vec![vec![0, 3, 2, 1], vec![4, 5, 6, 7], vec![0, 1, 5, 4], vec![2, 3, 7, 6], vec![3, 0, 4, 7], vec![1, 2, 6, 5]];
        let mut faces = Vec::new();
        let mut corners = Vec::new();
        for lp in &loops {
            let first = corners.len() as u32;
            for v in lp {
                corners.push(Corner { vertex: *v, uv: [0.0, 0.0], normal: None });
            }
            faces.push(Face { first, len: lp.len() as u32, material: 0, flags: FaceFlags::empty() });
        }
        EditMesh { positions, faces, corners, edge_flags: Vec::new(), materials: vec!["m".to_string()], smooth_angle: None }
    }

    fn grid4() -> EditMesh {
        let step = 16.0;
        let mut positions = Vec::new();
        for j in 0..5 {
            for i in 0..5 {
                positions.push(DVec3::new(i as f64 * step, j as f64 * step, 0.0));
            }
        }
        let mut faces = Vec::new();
        let mut corners = Vec::new();
        for j in 0..4 {
            for i in 0..4 {
                let lp = [j * 5 + i, j * 5 + i + 1, (j + 1) * 5 + i + 1, (j + 1) * 5 + i];
                let first = corners.len() as u32;
                for v in lp {
                    corners.push(Corner { vertex: v, uv: [0.0, 0.0], normal: None });
                }
                faces.push(Face { first, len: 4, material: 0, flags: FaceFlags::empty() });
            }
        }
        EditMesh { positions, faces, corners, edge_flags: Vec::new(), materials: vec!["m".to_string()], smooth_angle: None }
    }

    fn face_sel(ids: Vec<u32>) -> Selection {
        Selection { mode: SelMode::Face, ids }
    }

    fn point_line(a: DVec3, b: DVec3, q: DVec3) -> f64 {
        ((q - a).cross(b - a)).length() / (b - a).length()
    }

    fn welded_box() -> EditMesh {
        use crate::editmesh::convert::weld_primitives;
        use crate::import::glb::GlbMesh;
        use crate::meshgen::{build, MeshSpec};
        let g = build(&MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "b", "m", 1.0 / 512.0).unwrap();
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
    fn box_top_inset_has_five_faces_and_even_rim() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let (out, warnings) = run(&mut m, &face_sel(vec![1]), &serde_json::json!({"thickness": 8.0})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 10);
        assert_eq!(out.ids, vec![1]);
        for c in m.face_corners(1) {
            let q = m.positions[c.vertex as usize];
            assert!((q.x.abs() - 24.0).abs() < 1e-6 && (q.y.abs() - 24.0).abs() < 1e-6 && (q.z - 32.0).abs() < 1e-9);
        }
        for fi in 6..10 {
            let vs = m.face_verts(fi);
            assert_eq!(vs.len(), 4);
            let (a, b, na) = (m.positions[vs[0] as usize], m.positions[vs[1] as usize], m.positions[vs[3] as usize]);
            let w = point_line(a, b, na);
            assert!((w - 8.0).abs() < 1e-6, "rim width {w}");
        }
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn individual_grid_insets_every_face() {
        let mut m = grid4();
        let ids: Vec<u32> = (0..16).collect();
        let (out, warnings) = run(&mut m, &face_sel(ids), &serde_json::json!({"thickness": 2.0, "individual": true})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 80);
        assert_eq!(out.ids.len(), 16);
        let inv = invariants(&m);
        assert!(!inv.closed && inv.wound && inv.degenerate.is_empty());
        let mut again = grid4();
        let _ = run(&mut again, &face_sel((0..16).collect()), &serde_json::json!({"thickness": 2.0, "individual": true})).unwrap();
        assert_eq!(super::super::super::io::write(&m), super::super::super::io::write(&again));
    }

    #[test]
    fn over_thick_inset_clamps_and_stays_valid() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let (_, warnings) = run(&mut m, &face_sel(vec![1]), &serde_json::json!({"thickness": 1000.0})).unwrap();
        assert!(warnings.iter().any(|w| w.contains("clamp")));
        assert_eq!(m.faces.len(), 10);
        assert!(m.face_normal(1).z > 0.0);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
    }

    #[test]
    fn triangulated_top_stays_closed() {
        use crate::editmesh::select::{resolve, Sel, SelMode as M, SelQuery};
        let mut m = welded_box();
        let adj = Adjacency::build(&m);
        let s = resolve(
            &m,
            &adj,
            &Sel { mode: M::Face, ids: vec![], query: Some(SelQuery::Normal { dir: [0.0, 0.0, 1.0], max_angle_deg: 5.0 }) },
        )
        .unwrap();
        let (out, _) = run(&mut m, &s, &serde_json::json!({"thickness": 4.0})).unwrap();
        assert_eq!(out.mode, SelMode::Face);
        assert_eq!(m.faces.len(), 16);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn depth_raises_the_inset() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let _ = run(&mut m, &face_sel(vec![1]), &serde_json::json!({"thickness": 8.0, "depth": 4.0})).unwrap();
        for c in m.face_corners(1) {
            assert!((m.positions[c.vertex as usize].z - 36.0).abs() < 1e-9);
        }
        assert!(invariants(&m).closed);
    }

    #[test]
    fn outset_grows_the_region() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let before = m.face_area(1);
        let _ = run(&mut m, &face_sel(vec![1]), &serde_json::json!({"thickness": 8.0, "outset": true})).unwrap();
        assert!(m.face_area(1) > before);
        let inv = invariants(&m);
        assert!(inv.wound && inv.degenerate.is_empty());
    }

    #[test]
    fn rail_matches_bisector_on_planar_regions() {
        let mut a = quad_box_at([0.0, 0.0, 0.0]);
        let mut b = quad_box_at([0.0, 0.0, 0.0]);
        let _ = run(&mut a, &face_sel(vec![1]), &serde_json::json!({"thickness": 8.0})).unwrap();
        let _ =
            run(&mut b, &face_sel(vec![1]), &serde_json::json!({"thickness": 8.0, "edge_rail": true})).unwrap();
        let mut pa: Vec<[f64; 3]> = a.positions.iter().map(|p| p.to_array()).collect();
        let mut pb: Vec<[f64; 3]> = b.positions.iter().map(|p| p.to_array()).collect();
        pa.sort_by(|x, y| x.partial_cmp(y).unwrap());
        pb.sort_by(|x, y| x.partial_cmp(y).unwrap());
        assert_eq!(pa.len(), pb.len());
        for (x, y) in pa.iter().zip(pb.iter()) {
            assert!((x[0] - y[0]).abs() < 1e-6 && (x[1] - y[1]).abs() < 1e-6 && (x[2] - y[2]).abs() < 1e-6);
        }
    }

    #[test]
    fn uneven_region_inset_travels_the_bisector() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let _ = run(&mut m, &face_sel(vec![1]), &serde_json::json!({"thickness": 8.0, "even_offset": false})).unwrap();
        assert_eq!(m.faces.len(), 10);
        for c in m.face_corners(1) {
            let q = m.positions[c.vertex as usize];
            let d = 8.0 / 2.0f64.sqrt();
            assert!((q.x.abs() - (32.0 - d)).abs() < 1e-6 && (q.y.abs() - (32.0 - d)).abs() < 1e-6);
        }
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
    }

    #[test]
    fn whole_mesh_falls_back_to_individual() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let (_, warnings) =
            run(&mut m, &face_sel((0..6).collect()), &serde_json::json!({"thickness": 2.0})).unwrap();
        assert!(warnings.iter().any(|w| w.contains("on its own")));
        assert_eq!(m.faces.len(), 30);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
    }

    #[test]
    fn relative_offset_scales_by_edge_length() {
        let mut m = grid4();
        let (_, warnings) =
            run(&mut m, &face_sel(vec![5]), &serde_json::json!({"thickness": 0.25, "relative_offset": true})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 20);
        let vs = m.face_verts(17);
        let (a, b, na) = (m.positions[vs[0] as usize], m.positions[vs[1] as usize], m.positions[vs[3] as usize]);
        assert!((point_line(a, b, na) - 4.0).abs() < 1e-6);
    }

    #[test]
    fn boundary_off_refuses_open_meshes() {
        let mut m = grid4();
        let err = run(&mut m, &face_sel(vec![0]), &serde_json::json!({"thickness": 2.0, "boundary": false})).unwrap_err();
        assert!(err.to_string().contains("boundary"));
    }

    #[test]
    fn non_manifold_boundary_is_refused_untouched() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let f = m.faces[1];
        let first = m.corners.len() as u32;
        m.corners.extend_from_slice(&m.corners[f.first as usize..f.first as usize + f.len as usize].to_vec());
        m.faces.push(Face { first, len: f.len, material: f.material, flags: f.flags });
        let before = m.faces.len();
        let err = run(&mut m, &face_sel(vec![1]), &serde_json::json!({"thickness": 8.0})).unwrap_err();
        assert!(err.to_string().contains("non-manifold"));
        assert_eq!(m.faces.len(), before);
    }

    #[test]
    fn dispatch_reaches_inset_and_rejects_unknown_params() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let (out, _) =
            super::super::dispatch("inset", &mut m, &face_sel(vec![1]), &serde_json::json!({"thickness": 8.0})).unwrap();
        assert_eq!(out.mode, SelMode::Face);
        let err =
            super::super::dispatch("inset", &mut m, &face_sel(vec![1]), &serde_json::json!({"nope": 1})).unwrap_err();
        assert!(err.to_string().contains("bad params"));
    }
}
