//! bridge_edge_loops: connect two or more edge loops with a quad strip
//! (triangles where loop lengths differ), deleting faces the loops bound when
//! the loops were selected via faces, so two facing faces become a tunnel.

use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use super::{face_edges, loop_is_closed, order_edge_loops, orient_loop, parse_params, selected_edges, selected_faces, OpResult};
use crate::editmesh::{bad, finite_in_range, newell, Adjacency, Corner, EditMesh, Face, FaceFlags, DEGENERATE_AREA};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    #[default]
    Linear,
    Path,
    Surface,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default)]
    pub cuts: u32,
    #[serde(default)]
    pub twist: i32,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub interpolation: Interp,
    #[serde(default)]
    pub smoothness: f64,
    #[serde(default)]
    pub merge: bool,
}

struct Loop {
    verts: Vec<u32>,
    closed: bool,
}

struct Span {
    ai: usize,
    aj: usize,
    bi: usize,
    bj: usize,
}

pub fn run(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    let cuts = p.cuts.min(64);
    let twist = p.twist.clamp(-4096, 4096);
    if !p.smoothness.is_finite() {
        return Err(bad("param smoothness is not finite"));
    }
    let smoothness = p.smoothness.clamp(0.0, 1.0);
    let adj = Adjacency::build(mesh);
    let (loops, dead) = resolve_loops(mesh, &adj, sel)?;
    if loops.len() < 2 {
        return Err(bad("bridge needs at least two edge loops"));
    }
    for w in loops.windows(2) {
        if w[0].closed != w[1].closed {
            return Err(bad("cannot bridge a closed loop to an open chain"));
        }
    }
    if !dead.is_empty() {
        super::drop_faces(mesh, &dead);
    }
    let adj = Adjacency::build(mesh);
    let loops: Vec<Loop> = loops.into_iter().map(|l| Loop { verts: orient_loop(mesh, &adj, l.verts), closed: l.closed }).collect();
    let material = loop_material(mesh, &adj, &loops[0]);
    let mut fresh_pos: Vec<DVec3> = Vec::new();
    let mut fresh_faces: Vec<Vec<(u32, [f32; 2])>> = Vec::new();
    let mut tri_spans = 0usize;
    for pair in 0..loops.len() - 1 {
        let qa = loops[pair].verts.clone();
        let mut qb = loops[pair + 1].verts.clone();
        qb.reverse();
        if p.reverse && pair % 2 == 0 {
            qb.reverse();
        }
        bridge_pair(
            mesh,
            &Loop { verts: qa, closed: loops[pair].closed },
            &Loop { verts: qb, closed: loops[pair + 1].closed },
            cuts,
            twist,
            p.interpolation,
            smoothness,
            &mut fresh_pos,
            &mut fresh_faces,
            &mut tri_spans,
        )?;
    }
    let base = mesh.positions.len();
    if base + fresh_pos.len() > super::super::MAX_VERTICES
        || mesh.faces.len() + fresh_faces.len() > super::super::MAX_FACES
        || mesh.corners.len() + fresh_faces.iter().map(Vec::len).sum::<usize>() > super::super::MAX_CORNERS
    {
        return Err(bad("the bridge would pass the mesh budget"));
    }
    let mut near: BTreeSet<(u32, Vec<u32>)> = BTreeSet::new();
    {
        let mut loop_verts: BTreeSet<u32> = BTreeSet::new();
        for l in &loops {
            loop_verts.extend(l.verts.iter().copied());
        }
        let mut adj_faces: BTreeSet<u32> = BTreeSet::new();
        for v in &loop_verts {
            adj_faces.extend(adj.faces_of(*v).iter().copied());
        }
        for f in adj_faces {
            let mut vs = mesh.face_verts(f as usize);
            vs.sort_unstable();
            near.insert((vs.len() as u32, vs));
        }
    }
    for face in &fresh_faces {
        if face.iter().any(|(v, _)| (*v as usize) >= base) {
            continue;
        }
        let mut vs: Vec<u32> = face.iter().map(|(v, _)| *v).collect();
        vs.sort_unstable();
        if near.contains(&(vs.len() as u32, vs)) {
            return Err(bad("the bridge duplicates existing faces; add twist or move a loop"));
        }
    }
    let mut full: BTreeMap<u32, DVec3> = BTreeMap::new();
    for (k, q) in fresh_pos.iter().enumerate() {
        full.insert((base + k) as u32, *q);
    }
    for face in &fresh_faces {
        let pts: Vec<DVec3> = face
            .iter()
            .map(|(v, _)| {
                if let Some(q) = full.get(v) {
                    *q
                } else {
                    mesh.positions[*v as usize]
                }
            })
            .collect();
        if pts.iter().any(|q| !(q.is_finite() && finite_in_range(q.x) && finite_in_range(q.y) && finite_in_range(q.z))) {
            return Err(bad("the bridge leaves the 1e6 build range"));
        }
        if newell(&pts).length() * 0.5 < DEGENERATE_AREA {
            return Err(bad("the bridge collapses; the loops touch or overlap"));
        }
    }
    let saved_flags = mesh.edge_flags.clone();
    for q in &fresh_pos {
        mesh.positions.push(*q);
    }
    let first_face = mesh.faces.len() as u32;
    for face in &fresh_faces {
        let first = mesh.corners.len() as u32;
        for (v, uv) in face {
            mesh.corners.push(Corner { vertex: *v, uv: *uv, normal: None });
        }
        mesh.faces.push(Face { first, len: face.len() as u32, material, flags: FaceFlags::empty() });
    }
    if p.merge {
        super::weld(mesh, super::super::WELD_EPS);
    }
    super::carry_flags(mesh, &saved_flags);
    super::compact(mesh);
    let made: Vec<u32> = (first_face..mesh.faces.len() as u32).collect();
    let mut warnings = Vec::new();
    if tri_spans > 0 {
        warnings.push(format!("{tri_spans} spans are triangles where the loop lengths differ"));
    }
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}

fn fractions(mesh: &EditMesh, lp: &Loop) -> Vec<f64> {
    let n = lp.verts.len();
    let edges = if lp.closed { n } else { n - 1 };
    let mut lens = Vec::with_capacity(edges);
    let mut total = 0.0;
    for k in 0..edges {
        let len = (mesh.positions[lp.verts[k] as usize] - mesh.positions[lp.verts[(k + 1) % n] as usize]).length();
        lens.push(len);
        total += len;
    }
    let mut out = vec![0.0; n];
    let mut acc = 0.0;
    for k in 1..n {
        acc += lens[k - 1];
        out[k] = if total > 1e-12 { acc / total } else { k as f64 / n as f64 };
    }
    out
}

fn rotation_dists(a: &[DVec3], b: &[DVec3]) -> Vec<(f64, usize)> {
    let (na, nb) = (a.len(), b.len());
    let n = na.min(nb).max(1);
    let stride = (n / 256).max(1);
    let mut out = Vec::with_capacity(nb);
    for r in 0..nb {
        let mut d = 0.0;
        let mut k = 0;
        while k * stride < n {
            let i = k * stride;
            d += (a[i * na / n] - b[(i * nb / n + r) % nb]).length_squared();
            k += 1;
        }
        out.push((d, r));
    }
    out.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal).then(x.1.cmp(&y.1)));
    out
}

fn span_loop(na: usize, nb: usize, closed: bool) -> Vec<Span> {
    let (ea, eb) = if closed { (na, nb) } else { (na - 1, nb - 1) };
    let mut spans = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    loop {
        if i == ea && j == eb {
            break;
        }
        if i > ea || j > eb || spans.len() > ea + eb + 2 {
            return Vec::new();
        }
        let a_only = j == eb || (i < ea && (i + 1) * eb < (j + 1) * ea);
        let b_only = i == ea || (j < eb && (j + 1) * ea < (i + 1) * eb);
        let (di, dj) = match (a_only, b_only) {
            (true, _) => (1, 0),
            (_, true) => (0, 1),
            _ => (1, 1),
        };
        let idx = |x: usize, n: usize| if closed { x % n } else { x };
        spans.push(Span { ai: idx(i, na), aj: idx(i + di, na), bi: idx(j, nb), bj: idx(j + dj, nb) });
        i += di;
        j += dj;
    }
    spans
}

fn spans_hold(pa: &[DVec3], pb: &[DVec3], spans: &[Span], r: usize) -> bool {
    let nb = pb.len();
    for s in spans {
        let corners = [pa[s.ai], pa[s.aj], pb[(s.bj + r) % nb], pb[(s.bi + r) % nb]];
        let mut deduped: Vec<DVec3> = Vec::with_capacity(4);
        for q in corners {
            if deduped.last() != Some(&q) {
                deduped.push(q);
            }
        }
        if deduped.len() == 4 && deduped[0] == deduped[3] {
            deduped.pop();
        }
        if deduped.len() < 3 || newell(&deduped).length() * 0.5 < DEGENERATE_AREA {
            return false;
        }
    }
    true
}

struct PairCtx {
    na: DVec3,
    nb: DVec3,
    mode: Interp,
    smooth: f64,
}

fn line_point(a: DVec3, b: DVec3, t: f64, ctx: &PairCtx) -> DVec3 {
    match ctx.mode {
        Interp::Linear | Interp::Surface => a + (b - a) * t,
        Interp::Path => {
            let dist = (b - a).length();
            let (ma, mb) = (ctx.na * dist * ctx.smooth, ctx.nb * dist * ctx.smooth);
            let (t2, t3) = (t * t, t * t * t);
            a * (2.0 * t3 - 3.0 * t2 + 1.0) + ma * (t3 - 2.0 * t2 + t) + b * (-2.0 * t3 + 3.0 * t2) + mb * (t3 - t2)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn bridge_pair(
    mesh: &EditMesh,
    la: &Loop,
    lb: &Loop,
    cuts: u32,
    twist: i32,
    interp: Interp,
    smooth: f64,
    out_pos: &mut Vec<DVec3>,
    out_faces: &mut Vec<Vec<(u32, [f32; 2])>>,
    tri_spans: &mut usize,
) -> Result<(), crate::editmesh::EditError> {
    let pa: Vec<DVec3> = la.verts.iter().map(|v| mesh.positions[*v as usize]).collect();
    let pb_raw: Vec<DVec3> = lb.verts.iter().map(|v| mesh.positions[*v as usize]).collect();
    let (na, nb) = (la.verts.len(), lb.verts.len());
    let spans = span_loop(na, nb, la.closed);
    if spans.is_empty() {
        return Err(bad("the loop walk ran past its bounds"));
    }
    let mut start = rotation_dists(&pa, &pb_raw).first().map(|(_, r)| *r).unwrap_or(0);
    for (_, r) in rotation_dists(&pa, &pb_raw) {
        let rt = (r as i64 + twist as i64).rem_euclid(nb as i64) as usize;
        if spans_hold(&pa, &pb_raw, &spans, rt) {
            start = r;
            break;
        }
    }
    let start = (start as i64 + twist as i64).rem_euclid(nb as i64) as usize;
    let mut order: Vec<usize> = (0..lb.verts.len()).collect();
    order.rotate_left(start);
    let pb: Vec<DVec3> = order.iter().map(|k| pb_raw[*k]).collect();
    let bverts: Vec<u32> = order.iter().map(|k| lb.verts[*k]).collect();
    let (ea, eb) = if la.closed { (na, nb) } else { (na - 1, nb - 1) };
    if 2 * (ea + eb) * (cuts as usize + 1) > super::super::MAX_FACES
        || 2 * (ea + eb) * cuts as usize > super::super::MAX_VERTICES
    {
        return Err(bad("the bridge would pass the mesh budget"));
    }
    for s in &spans {
        if s.ai == s.aj && s.bi == s.bj {
            continue;
        }
        if (s.ai != s.aj) != (s.bi != s.bj) {
            *tri_spans += 1;
        }
    }
    let ctx = PairCtx { na: loop_normal(mesh, &la.verts), nb: loop_normal(mesh, &bverts), mode: interp, smooth };
    let fa = fractions(mesh, la);
    let fb_full = fractions(mesh, lb);
    let fb: Vec<f64> = order.iter().map(|k| fb_full[*k]).collect();
    let mut used: BTreeSet<(usize, usize)> = BTreeSet::new();
    for s in &spans {
        used.insert((s.ai, s.bi));
        used.insert((s.aj, s.bj));
    }
    let mut ring_id: BTreeMap<(usize, usize), Vec<u32>> = BTreeMap::new();
    let mut ring_uv: BTreeMap<u32, [f32; 2]> = BTreeMap::new();
    let mut next = (mesh.positions.len() + out_pos.len()) as u32;
    for key in used.iter() {
        let s = spans.iter().find(|s| (s.ai, s.bi) == *key || (s.aj, s.bj) == *key).unwrap();
        let end_a = (s.ai, s.bi) == *key;
        let (a_pos, b_pos) = if end_a { (pa[s.ai], pb[s.bi]) } else { (pa[s.aj], pb[s.bj]) };
        let (ua, ub) = if end_a { (fa[s.ai], fb[s.bi]) } else { (fa[s.aj], fb[s.bj]) };
        let mut ids = Vec::with_capacity(cuts as usize);
        for k in 1..=cuts {
            let t = k as f64 / (cuts + 1) as f64;
            let q = line_point(a_pos, b_pos, t, &ctx);
            if !q.is_finite() {
                return Err(bad("the bridge interpolation is not finite"));
            }
            ring_uv.insert(next, [(ua + (ub - ua) * t) as f32, t as f32]);
            out_pos.push(q);
            ids.push(next);
            next += 1;
        }
        ring_id.insert(*key, ids);
    }
    if interp == Interp::Surface {
        relax(&pa, &pb, mesh.positions.len() as u32, out_pos, &ring_id, smooth);
    }
    for s in &spans {
        let (a1, a2, b1, b2) = (la.verts[s.ai], la.verts[s.aj], bverts[s.bi], bverts[s.bj]);
        let seq1: Vec<u32> =
            std::iter::once(a1).chain(ring_id[&(s.ai, s.bi)].iter().copied()).chain(std::iter::once(b1)).collect();
        let seq2: Vec<u32> =
            std::iter::once(a2).chain(ring_id[&(s.aj, s.bj)].iter().copied()).chain(std::iter::once(b2)).collect();
        let uv_of = |v: u32| -> [f32; 2] {
            if let Some(uv) = ring_uv.get(&v) {
                return *uv;
            }
            if v == a1 {
                return [fa[s.ai] as f32, 0.0];
            }
            if v == a2 {
                return [fa[s.aj] as f32, 0.0];
            }
            if v == b1 {
                return [fb[s.bi] as f32, 1.0];
            }
            [fb[s.bj] as f32, 1.0]
        };
        for k in 0..seq1.len() - 1 {
            let corners = [seq1[k], seq2[k], seq2[k + 1], seq1[k + 1]];
            let mut deduped: Vec<u32> = Vec::with_capacity(4);
            for v in corners {
                if deduped.last() != Some(&v) {
                    deduped.push(v);
                }
            }
            if deduped.len() == 4 && deduped[0] == deduped[3] {
                deduped.pop();
            }
            if deduped.len() < 3 {
                continue;
            }
            out_faces.push(deduped.into_iter().map(|v| (v, uv_of(v))).collect());
        }
    }
    Ok(())
}

fn relax(
    pa: &[DVec3],
    pb: &[DVec3],
    base: u32,
    out_pos: &mut [DVec3],
    rails: &BTreeMap<(usize, usize), Vec<u32>>,
    smooth: f64,
) {
    let iters = (smooth * 16.0).round() as usize;
    for _ in 0..iters {
        let mut next_vals: BTreeMap<u32, DVec3> = BTreeMap::new();
        for ((i, j), seq) in rails.iter() {
            let (a, b) = (pa[*i], pb[*j]);
            for (k, v) in seq.iter().enumerate() {
                let prev = if k == 0 { a } else { out_pos[(seq[k - 1] - base) as usize] };
                let next = if k + 1 == seq.len() { b } else { out_pos[(seq[k + 1] - base) as usize] };
                next_vals.insert(*v, (prev + next) * 0.5);
            }
        }
        let mut moved = false;
        for (v, q) in next_vals {
            let slot = &mut out_pos[(v - base) as usize];
            if *slot != q {
                *slot = q;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
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

    fn join(a: &EditMesh, b: &EditMesh) -> EditMesh {
        let off = a.positions.len() as u32;
        let mut positions = a.positions.clone();
        positions.extend_from_slice(&b.positions);
        let mut corners: Vec<Corner> = a.corners.clone();
        for c in &b.corners {
            corners.push(Corner { vertex: c.vertex + off, uv: c.uv, normal: c.normal });
        }
        let mut faces = a.faces.clone();
        let coff = a.corners.len() as u32;
        for f in &b.faces {
            faces.push(Face { first: f.first + coff, len: f.len, material: f.material, flags: f.flags });
        }
        EditMesh { positions, faces, corners, edge_flags: Vec::new(), materials: a.materials.clone(), smooth_angle: None }
    }

    fn prism_at(n: usize, r: f64, h: f64, c: [f64; 3]) -> EditMesh {
        let ring: Vec<[f64; 2]> = if n == 8 {
            let s = 0.7071067811865476;
            vec![[1.0, 0.0], [s, s], [0.0, 1.0], [-s, s], [-1.0, 0.0], [-s, -s], [0.0, -1.0], [s, -s]]
        } else {
            let (cc, ss) = (0.8660254037844386, 0.5);
            vec![
                [1.0, 0.0],
                [cc, ss],
                [ss, cc],
                [0.0, 1.0],
                [-ss, cc],
                [-cc, ss],
                [-1.0, 0.0],
                [-cc, -ss],
                [-ss, -cc],
                [0.0, -1.0],
                [ss, -cc],
                [cc, -ss],
            ]
        };
        assert_eq!(ring.len(), n);
        let mut positions = Vec::new();
        for k in 0..n {
            positions.push(DVec3::new(c[0] + r * ring[k][0], c[1] + r * ring[k][1], c[2] - h / 2.0));
        }
        for k in 0..n {
            positions.push(DVec3::new(c[0] + r * ring[k][0], c[1] + r * ring[k][1], c[2] + h / 2.0));
        }
        let mut loops: Vec<Vec<u32>> = Vec::new();
        for k in 0..n {
            let (b0, b1, t0, t1) = (k as u32, ((k + 1) % n) as u32, (n + k) as u32, (n + (k + 1) % n) as u32);
            loops.push(vec![b0, b1, t1, t0]);
        }
        loops.push((0..n as u32).map(|k| n as u32 + k).collect());
        loops.push(std::iter::once(0).chain((1..n as u32).rev()).collect());
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

    fn face_sel(ids: Vec<u32>) -> Selection {
        Selection { mode: SelMode::Face, ids }
    }

    fn tunnel() -> EditMesh {
        join(&quad_box_at([-33.0, 0.0, 0.0]), &quad_box_at([33.0, 0.0, 0.0]))
    }

    fn tunnel_sel(m: &EditMesh) -> Selection {
        let a = (0..6).find(|f| (m.face_normal(*f) - DVec3::X).length() < 1e-9).unwrap() as u32;
        let b = (6..12).find(|f| (m.face_normal(*f) - DVec3::NEG_X).length() < 1e-9).unwrap() as u32;
        face_sel(vec![a, b])
    }

    #[test]
    fn two_boxes_bridge_into_a_closed_sphere() {
        let mut m = tunnel();
        let sel = tunnel_sel(&m);
        let (out, warnings) = run(&mut m, &sel, &serde_json::json!({})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 14);
        assert_eq!(out.ids.len(), 4);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn one_box_two_faces_bridge_into_a_torus() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let (out, _) = run(&mut m, &face_sel(vec![0, 1]), &serde_json::json!({"twist": 1})).unwrap();
        assert_eq!(m.faces.len(), 8);
        assert_eq!(out.ids.len(), 4);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 0);
    }

    #[test]
    fn straight_tube_through_a_box_is_refused() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let err = run(&mut m, &face_sel(vec![0, 1]), &serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("duplicates"));
    }

    #[test]
    fn unequal_8_and_12_loops_stay_closed() {
        let mut m = join(&prism_at(8, 32.0, 64.0, [0.0, 0.0, 0.0]), &prism_at(12, 32.0, 64.0, [100.0, 0.0, 0.0]));
        let caps: Vec<u32> =
            (0..m.faces.len()).filter(|f| (m.face_normal(*f) - DVec3::Z).length() < 1e-9).map(|f| f as u32).collect();
        assert_eq!(caps.len(), 2);
        let (out, warnings) = run(&mut m, &face_sel(caps), &serde_json::json!({})).unwrap();
        assert!(warnings.iter().any(|w| w.contains("triangles")));
        assert_eq!(m.faces.len(), 38);
        assert_eq!(out.ids.len(), 16);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn cuts_subdivide_every_span() {
        let mut m = tunnel();
        let sel = tunnel_sel(&m);
        let (out, _) = run(&mut m, &sel, &serde_json::json!({"cuts": 2})).unwrap();
        assert_eq!(m.faces.len(), 22);
        assert_eq!(out.ids.len(), 12);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn twist_reverse_and_interpolations_stay_closed() {
        for params in [
            serde_json::json!({"twist": 1}),
            serde_json::json!({"cuts": 2, "interpolation": "path", "smoothness": 0.5}),
            serde_json::json!({"cuts": 2, "interpolation": "surface", "smoothness": 0.5}),
            serde_json::json!({"merge": true}),
        ] {
            let mut m = tunnel();
            let sel = tunnel_sel(&m);
            if let Err(e) = run(&mut m, &sel, &params) {
                panic!("params {params}: {e}");
            }
            let inv = invariants(&m);
            assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "params {params}");
            assert_eq!(inv.euler, 2);
        }
        let mut m = tunnel();
        let sel = tunnel_sel(&m);
        let (_, _) = run(&mut m, &sel, &serde_json::json!({"reverse": true})).unwrap();
        assert!(invariants(&m).closed);
    }

    #[test]
    fn open_chains_bridge_into_a_strip() {
        let m = tunnel();
        let adj = Adjacency::build(&m);
        let sel = Selection {
            mode: SelMode::Edge,
            ids: vec![
                adj.index_of(4, 5),
                adj.index_of(5, 6),
                adj.index_of(12, 13),
                adj.index_of(13, 14),
            ],
        };
        let mut m = m;
        let (out, _) = run(&mut m, &sel, &serde_json::json!({})).unwrap();
        assert_eq!(m.faces.len(), 14);
        assert_eq!(out.ids.len(), 2);
        let inv = invariants(&m);
        assert!(!inv.closed && inv.wound && inv.degenerate.is_empty());
    }

    #[test]
    fn non_manifold_edges_are_refused_untouched() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let f = m.faces[1];
        let first = m.corners.len() as u32;
        m.corners.extend_from_slice(&m.corners[f.first as usize..f.first as usize + f.len as usize].to_vec());
        m.faces.push(Face { first, len: f.len, material: f.material, flags: f.flags });
        let adj = Adjacency::build(&m);
        let eid = adj.index_of(4, 5);
        let before = (m.faces.len(), m.positions.len());
        let sel = Selection { mode: SelMode::Edge, ids: vec![eid, adj.index_of(5, 6)] };
        let err = run(&mut m, &sel, &serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("non-manifold") && err.to_string().contains(&eid.to_string()));
        assert_eq!((m.faces.len(), m.positions.len()), before);
    }

    #[test]
    fn one_loop_and_mixed_loops_are_refused() {
        let mut m = quad_box_at([0.0, 0.0, 0.0]);
        let err = run(&mut m, &face_sel(vec![1]), &serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("at least two"));
        let adj = Adjacency::build(&m);
        let sel = Selection {
            mode: SelMode::Edge,
            ids: vec![adj.index_of(4, 5), adj.index_of(5, 6), adj.index_of(6, 7), adj.index_of(7, 4), adj.index_of(0, 1)],
        };
        let err = run(&mut m, &sel, &serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("closed loop to an open chain"));
    }

    #[test]
    fn bridge_is_deterministic_and_dispatched() {
        let mut a = tunnel();
        let mut b = tunnel();
        let sa = tunnel_sel(&a);
        let sb = tunnel_sel(&b);
        let _ = run(&mut a, &sa, &serde_json::json!({"cuts": 1})).unwrap();
        let _ = run(&mut b, &sb, &serde_json::json!({"cuts": 1})).unwrap();
        assert_eq!(super::super::super::io::write(&a), super::super::super::io::write(&b));
        let mut m = tunnel();
        let sel = tunnel_sel(&m);
        let (out, _) = super::super::dispatch("bridge_edge_loops", &mut m, &sel, &serde_json::json!({})).unwrap();
        assert_eq!(out.mode, SelMode::Face);
        let err = super::super::dispatch("bridge_edge_loops", &mut m, &sel, &serde_json::json!({"nope": 1})).unwrap_err();
        assert!(err.to_string().contains("bad params"));
    }
}

fn resolve_loops(
    mesh: &EditMesh,
    adj: &Adjacency,
    sel: &Selection,
) -> Result<(Vec<Loop>, BTreeSet<u32>), crate::editmesh::EditError> {
    match sel.mode {
        SelMode::Face => {
            let faces = selected_faces(mesh, adj, sel);
            if faces.is_empty() {
                return Err(bad("bridge names no faces"));
            }
            let mut todo = faces.clone();
            let mut loops = Vec::new();
            while let Some(seed) = todo.iter().next().copied() {
                let mut comp = BTreeSet::from([seed]);
                let mut queue = vec![seed];
                todo.remove(&seed);
                while let Some(f) = queue.pop() {
                    for e in face_edges(mesh, f as usize) {
                        if let Some(edge) = adj.edge(e.0, e.1) {
                            for nf in &edge.faces {
                                if faces.contains(nf) && comp.insert(*nf) {
                                    queue.push(*nf);
                                    todo.remove(nf);
                                }
                            }
                        }
                    }
                }
                let mut counts: BTreeMap<(u32, u32), usize> = BTreeMap::new();
                for f in &comp {
                    for e in face_edges(mesh, *f as usize) {
                        *counts.entry(e).or_insert(0) += 1;
                    }
                }
                let border: BTreeSet<(u32, u32)> = counts.into_iter().filter(|(_, n)| *n == 1).map(|(e, _)| e).collect();
                for (a, b) in &border {
                    if adj.edge(*a, *b).is_some_and(|e| e.faces.len() > 2) {
                        return Err(bad(format!(
                            "edge {} is non-manifold; merge by distance or fill it first",
                            adj.index_of(*a, *b)
                        )));
                    }
                }
                let chains = order_edge_loops(&border);
                if chains.len() != 1 {
                    return Err(bad(format!(
                        "a patch of {} faces has no single boundary loop and cannot be bridged",
                        comp.len()
                    )));
                }
                let closed = loop_is_closed(&chains[0], &border);
                if closed && chains[0].len() < 3 {
                    return Err(bad("a boundary loop has fewer than three vertices"));
                }
                loops.push(Loop { verts: chains.into_iter().next().unwrap_or_default(), closed });
            }
            loops.sort_by(|a, b| a.verts.cmp(&b.verts));
            Ok((loops, faces))
        }
        SelMode::Edge | SelMode::Vertex => {
            let edges = selected_edges(mesh, adj, sel);
            if edges.is_empty() {
                return Err(bad("bridge names no edges"));
            }
            for (a, b) in &edges {
                if adj.edge(*a, *b).is_some_and(|e| e.faces.len() > 2) {
                    return Err(bad(format!(
                        "edge {} is non-manifold; merge by distance or fill it first",
                        adj.index_of(*a, *b)
                    )));
                }
            }
            let mut loops = Vec::new();
            for chain in order_edge_loops(&edges) {
                let closed = loop_is_closed(&chain, &edges);
                if (closed && chain.len() >= 3) || (!closed && chain.len() >= 2) {
                    loops.push(Loop { verts: chain, closed });
                }
            }
            if loops.is_empty() {
                return Err(bad("the selected edges form no usable loop"));
            }
            Ok((loops, BTreeSet::new()))
        }
    }
}

fn loop_normal(mesh: &EditMesh, verts: &[u32]) -> DVec3 {
    newell(&verts.iter().map(|v| mesh.positions[*v as usize]).collect::<Vec<_>>()).normalize_or_zero()
}

fn loop_material(mesh: &EditMesh, adj: &Adjacency, first: &Loop) -> u16 {
    for k in 0..first.verts.len() {
        let (a, b) = (first.verts[k], first.verts[(k + 1) % first.verts.len()]);
        if let Some(e) = adj.edge(a.min(b), a.max(b)) {
            if let Some(f) = e.faces.first() {
                return mesh.faces[*f as usize].material;
            }
        }
    }
    0
}
