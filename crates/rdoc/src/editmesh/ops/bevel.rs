//! bevel: chamfer beveled edges (or cut off vertices) with a multi-segment
//! superellipse profile, the 2-texel lit edge gap analysis 5.2 asks for.
//!
//! Each beveled edge grows two rails offset along its adjacent faces (the M5
//! selective-offset math), joined by a `segments` strip following a
//! superellipse whose exponent comes from `profile` (`n = 1/(1-p)`, so 0 is a
//! straight chamfer and 0.5 is circular). Vertices where beveled edges meet
//! are patched by valence: 1 terminates on the unbevelled neighbour (sliding
//! along it with `loop_slide`), 2 continues straight, 3 at a convex corner
//! gets Blender's three-sector quad grid fill, anything else a single cutoff
//! polygon. `affect: vertices` cuts selected corners off directly with
//! profiled rings. Strip and patch faces are smooth, originals keep their
//! flags, the strip boundary is marked sharp, and `harden_normals` freezes
//! the originals flat with custom split normals.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

use super::{clamp_finite, parse_params, selected_edges, selected_verts, OpResult};
use crate::editmesh::{
    bad, finite_in_range, newell, Adjacency, Corner, EditMesh, EdgeFlags, Face, FaceFlags, DEGENERATE_AREA,
    MAX_CORNERS, MAX_FACES, MAX_VERTICES,
};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Affect {
    #[default]
    Edges,
    Vertices,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffsetType {
    #[default]
    Offset,
    Width,
    Depth,
    Percent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default)]
    pub affect: Affect,
    #[serde(default)]
    pub offset_type: OffsetType,
    #[serde(default = "default_amount")]
    pub amount: f64,
    #[serde(default = "default_segments")]
    pub segments: u32,
    #[serde(default = "default_profile")]
    pub profile: f64,
    #[serde(default = "default_true")]
    pub clamp_overlap: bool,
    #[serde(default = "default_true")]
    pub loop_slide: bool,
    #[serde(default = "default_material")]
    pub material: i32,
    #[serde(default)]
    pub harden_normals: bool,
    #[serde(default)]
    pub mark_seam: bool,
    #[serde(default)]
    pub mark_sharp: bool,
}

fn default_amount() -> f64 {
    1.0
}

fn default_segments() -> u32 {
    1
}

fn default_profile() -> f64 {
    0.5
}

fn default_true() -> bool {
    true
}

fn default_material() -> i32 {
    -1
}

pub fn run(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    let amount = match p.offset_type {
        OffsetType::Percent => clamp_finite(p.amount, 0.0, 100.0, "amount")?,
        _ => clamp_finite(p.amount, 0.0, 1e6, "amount")?,
    };
    if amount <= 0.0 {
        return Err(bad("bevel amount is zero"));
    }
    let segments = p.segments.clamp(1, 64) as usize;
    let profile = clamp_finite(p.profile, 0.0, 1.0, "profile")?;
    if p.material < -1 || (p.material >= 0 && (p.material as usize) >= mesh.materials.len()) {
        return Err(bad(format!("material slot {} does not exist", p.material)));
    }
    match p.affect {
        Affect::Edges => run_edges(mesh, sel, &p, amount, segments, profile),
        Affect::Vertices => run_vertices(mesh, sel, &p, amount, segments, profile),
    }
}

/// Superellipse exponent from the profile slider: 0 cuts straight, 0.5 runs
/// circular, 1 would hold the square (capped so the powers stay finite).
fn exponent(profile: f64) -> f64 {
    (1.0 / (1.0 - profile)).min(64.0)
}

/// Profile easing for vertex rings: straight at 0, quarter-circular at 0.5.
fn ease(profile: f64, t: f64) -> f64 {
    let n = exponent(profile);
    if n <= 1.0 {
        return t;
    }
    if n == 2.0 {
        return 1.0 - (1.0 - t * t).max(0.0).sqrt();
    }
    if n >= 8.0 {
        let c = (1.0 - t).clamp(0.0, 1.0);
        if c <= 0.0 {
            return 1.0;
        }
        return 1.0 - (1.0 - c.powf(8.0)).max(0.0).powf(1.0 / 8.0);
    }
    1.0 - (1.0 - t.powf(n)).max(0.0).powf(1.0 / n)
}

fn corner_uv(mesh: &EditMesh, face: u32, vert: u32) -> [f32; 2] {
    let f = mesh.faces[face as usize];
    mesh.corners[f.first as usize..f.first as usize + f.len as usize]
        .iter()
        .find(|c| c.vertex == vert)
        .map(|c| c.uv)
        .unwrap_or([0.0, 0.0])
}

fn outward_ref(mesh: &EditMesh, faces: &[u32]) -> DVec3 {
    let mut n = DVec3::ZERO;
    for f in faces {
        n += mesh.face_normal(*f as usize);
    }
    n.normalize_or_zero()
}

/// One beveled edge with its two faces (smaller id first), fold angle and
/// per-edge offset state in along-face units.
struct BevEdge {
    a: u32,
    b: u32,
    f1: u32,
    f2: u32,
    len: f64,
    theta: f64,
    off: f64,
    cap: f64,
}

fn collect_edges(mesh: &EditMesh, adj: &Adjacency, sel: &Selection) -> Result<Vec<BevEdge>, crate::editmesh::EditError> {
    let pairs = selected_edges(mesh, adj, sel);
    if pairs.is_empty() {
        return Err(bad("bevel names no edges"));
    }
    let mut out = Vec::with_capacity(pairs.len());
    for (a, b) in pairs {
        let e = adj.edge(a, b).ok_or_else(|| bad("bevel names no edge"))?;
        if e.faces.len() == 1 {
            return Err(bad(format!("edge {} is on the mesh boundary", adj.index_of(a, b))));
        }
        if e.faces.len() > 2 {
            return Err(bad(format!(
                "edge {} is non-manifold; merge by distance or fill it first",
                adj.index_of(a, b)
            )));
        }
        let (f1, f2) = (e.faces[0].min(e.faces[1]), e.faces[0].max(e.faces[1]));
        let len = (mesh.positions[a as usize] - mesh.positions[b as usize]).length();
        if len < 1e-12 {
            return Err(bad(format!("edge {} is degenerate", adj.index_of(a, b))));
        }
        let (n1, n2) = (mesh.face_normal(f1 as usize), mesh.face_normal(f2 as usize));
        if n1 == DVec3::ZERO || n2 == DVec3::ZERO {
            return Err(bad(format!("face {f1} is degenerate")));
        }
        let cos_n = (-n1.dot(n2)).clamp(-1.0, 1.0);
        let theta = cos_n.acos();
        if (theta * 0.5).sin() < 1e-6 {
            return Err(bad(format!("edge {} is flat; bevel needs a fold", adj.index_of(a, b))));
        }
        out.push(BevEdge { a, b, f1, f2, len, theta, off: 0.0, cap: f64::INFINITY });
    }
    out.sort_by(|x, y| (x.a, x.b).cmp(&(y.a, y.b)));
    Ok(out)
}

/// Along-face offset distance for one edge at the global amount.
fn convert(edge: &BevEdge, ty: OffsetType, amount: f64) -> f64 {
    match ty {
        OffsetType::Offset => amount,
        OffsetType::Width => amount / (2.0 * (edge.theta * 0.5).sin()),
        OffsetType::Depth => {
            let c = (edge.theta * 0.5).cos().max(1e-6);
            amount / c
        }
        OffsetType::Percent => amount / 100.0 * edge.len,
    }
}

/// Largest along-face offset before a rail runs past a neighbour midpoint.
fn edge_cap(mesh: &EditMesh, edge: &BevEdge) -> f64 {
    let mut cap = f64::INFINITY;
    for v in [edge.a, edge.b] {
        for f in [edge.f1, edge.f2] {
            let verts = mesh.face_verts(f as usize);
            if let Some(k) = verts.iter().position(|w| *w == v) {
                let n = verts.len();
                for w in [verts[(k + 1) % n], verts[(k + n - 1) % n]] {
                    if w == edge.a || w == edge.b {
                        continue;
                    }
                    let l = (mesh.positions[v as usize] - mesh.positions[w as usize]).length();
                    cap = cap.min(l * 0.5);
                }
            }
        }
    }
    cap
}

fn cap_to_amount(edge: &BevEdge, ty: OffsetType, cap_len: f64) -> f64 {
    match ty {
        OffsetType::Offset => cap_len,
        OffsetType::Width => cap_len * 2.0 * (edge.theta * 0.5).sin(),
        OffsetType::Depth => cap_len * (edge.theta * 0.5).cos().max(1e-6),
        OffsetType::Percent => {
            if edge.len > 1e-12 {
                cap_len / edge.len * 100.0
            } else {
                0.0
            }
        }
    }
}

/// A touched face with its new loop: kept vertices stay, moved ones carry
/// their shrunken position, snapped ones reuse an existing vertex, and
/// single-sided slides record their split onto the neighbour edge.
struct Shrink {
    fresh: BTreeMap<u32, DVec3>,
    alias: BTreeMap<u32, u32>,
    splits: Vec<Split>,
    /// A slide ran past its neighbour vertex: the scale overlaps.
    oob: bool,
}

struct Split {
    edge: (u32, u32),
    t: f64,
    pos: DVec3,
    /// The corner vertex whose slide this is; a face flanked by two splits
    /// from the same corner drops that corner to the patch.
    from: u32,
}

fn face_frame(n: DVec3) -> (DVec3, DVec3) {
    let seed = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = n.cross(seed).normalize_or(DVec3::X);
    let v = n.cross(u).normalize_or(DVec3::Y);
    (u, v)
}

fn line_hit(p1: DVec2, d1: DVec2, p2: DVec2, d2: DVec2) -> Option<DVec2> {
    let denom = d1.perp_dot(d2);
    let scale = d1.length() * d2.length();
    if denom.abs() < 1e-12 * scale.max(1e-12) {
        return None;
    }
    Some(p1 + d1 * ((p2 - p1).perp_dot(d2) / denom))
}

/// Shrinks one face: beveled edges move inward by their offset, the rest
/// hold. Returns the moved corners and any slide splits onto unbevelled
/// neighbour edges.
fn shrink_face(
    mesh: &EditMesh,
    fi: u32,
    set: &BTreeSet<(u32, u32)>,
    off_of: &BTreeMap<(u32, u32), f64>,
) -> Result<Shrink, crate::editmesh::EditError> {
    let n = mesh.face_normal(fi as usize);
    if n == DVec3::ZERO {
        return Err(bad(format!("face {fi} is degenerate")));
    }
    let loop_v = mesh.face_verts(fi as usize);
    let pts: Vec<DVec3> = loop_v.iter().map(|v| mesh.positions[*v as usize]).collect();
    let (u, v) = face_frame(n);
    let c0 = pts.iter().copied().sum::<DVec3>() / pts.len() as f64;
    let flat: Vec<DVec2> = pts.iter().map(|q| DVec2::new((*q - c0).dot(u), (*q - c0).dot(v))).collect();
    let area: f64 = (0..flat.len()).map(|k| flat[k].perp_dot(flat[(k + 1) % flat.len()])).sum::<f64>() * 0.5;
    if !area.is_finite() || area.abs() < 1e-12 {
        return Err(bad(format!("face {fi} is degenerate")));
    }
    let sign = area.signum();
    let m = loop_v.len();
    let mut moved: Vec<bool> = Vec::with_capacity(m);
    let mut dist: Vec<f64> = Vec::with_capacity(m);
    for k in 0..m {
        let e = (loop_v[k].min(loop_v[(k + 1) % m]), loop_v[k].max(loop_v[(k + 1) % m]));
        moved.push(set.contains(&e));
        dist.push(if set.contains(&e) { off_of[&e] } else { 0.0 });
    }
    if !dist.is_empty() && moved.iter().all(|b| *b) && dist.windows(2).all(|w| w[0] == w[1]) {
        if let Some(outcome) = super::offset::offset_polygon(&flat, dist[0], true) {
            if !outcome.clamped {
                let mut out = Shrink { fresh: BTreeMap::new(), alias: BTreeMap::new(), splits: Vec::new(), oob: false };
                for (i, vert) in loop_v.iter().enumerate() {
                    let q = outcome.points[i];
                    let q3 = c0 + u * q.x + v * q.y;
                    if !(q3.is_finite() && finite_in_range(q3.x) && finite_in_range(q3.y) && finite_in_range(q3.z)) {
                        return Err(bad("the bevel leaves the 1e6 build range"));
                    }
                    out.fresh.insert(*vert, q3);
                }
                return Ok(out);
            }
        }
    }
    let mut base: Vec<DVec2> = Vec::with_capacity(m);
    let mut dir: Vec<DVec2> = Vec::with_capacity(m);
    for k in 0..m {
        let d = flat[(k + 1) % m] - flat[k];
        if d.length() < 1e-12 {
            return Err(bad(format!("face {fi} is degenerate")));
        }
        let inward = DVec2::new(-d.y, d.x).normalize() * sign;
        base.push(flat[k] + inward * dist[k]);
        dir.push(d);
    }
    let mut out = Shrink { fresh: BTreeMap::new(), alias: BTreeMap::new(), splits: Vec::new(), oob: false };
    for i in 0..m {
        if !moved[(i + m - 1) % m] && !moved[i] {
            continue;
        }
        let (a, da) = (base[(i + m - 1) % m], dir[(i + m - 1) % m]);
        let (b, db) = (base[i], dir[i]);
        let q = line_hit(a, da, b, db).ok_or_else(|| bad(format!("face {fi} has collinear edges the bevel cannot offset")))?;
        if !q.is_finite() {
            return Err(bad(format!("face {fi} has collinear edges the bevel cannot offset")));
        }
        let q3 = c0 + u * q.x + v * q.y;
        if !(q3.is_finite() && finite_in_range(q3.x) && finite_in_range(q3.y) && finite_in_range(q3.z)) {
            return Err(bad("the bevel leaves the 1e6 build range"));
        }
        let vert = loop_v[i];
        if moved[(i + m - 1) % m] != moved[i] {
            let other = if moved[i] { loop_v[(i + m - 1) % m] } else { loop_v[(i + 1) % m] };
            let (p, x) = if moved[i] {
                (mesh.positions[other as usize], mesh.positions[vert as usize])
            } else {
                (mesh.positions[vert as usize], mesh.positions[other as usize])
            };
            let seg = x - p;
            let along = (q3 - p).dot(seg) / seg.length_squared().max(1e-24);
            let tv = if moved[i] { 1.0 - along } else { along };
            if !(0.0..=1.0).contains(&tv) {
                out.oob = true;
                return Ok(out);
            }
            if tv <= 1e-9 {
                out.alias.insert(vert, vert);
                continue;
            }
            if tv >= 1.0 - 1e-9 {
                out.alias.insert(vert, other);
                continue;
            }
            let (lo, hi) = (vert.min(other), vert.max(other));
            let tmin = if vert == lo { tv } else { 1.0 - tv };
            let proj = mesh.positions[lo as usize] + (mesh.positions[hi as usize] - mesh.positions[lo as usize]) * tmin;
            out.splits.push(Split { edge: (lo, hi), t: tmin, pos: proj, from: vert });
            out.fresh.insert(vert, proj);
        } else {
            out.fresh.insert(vert, q3);
        }
    }
    Ok(out)
}

/// One slide split on an unbevelled neighbour edge, with its parameter
/// from the edge minimum and the corner vertex it slid from.
#[derive(Clone, Copy)]
struct EdgeSplit {
    t: f64,
    pos: DVec3,
    from: u32,
}

/// One profile polyline from the f1 side to the f2 side at an edge end.
struct Profile {
    pos: Vec<DVec3>,
    uv: Vec<[f32; 2]>,
}

/// One strip cross-section row: rail end, profile points, rail end with
/// their vertex ids and UVs. A sliding end terminates through its corner,
/// so its row is rail, corner, rail.
#[derive(Clone)]
struct Row {
    ids: Vec<u32>,
    pos: Vec<DVec3>,
    uv: Vec<[f32; 2]>,
}
/// Quad columns joining two equal rows, as index ranges into each row.
fn strip_quads(la: usize, lb: usize) -> Vec<([usize; 2], [usize; 2])> {
    if la != lb || la < 2 {
        return Vec::new();
    }
    (0..la - 1).map(|k| ([k, k + 1], [k, k + 1])).collect()
}

/// Cells joining unequal rows: the short row stretches by repeating corners
/// in place, so every cell winds like a quad; emission drops the repeated
/// corner. Corners are row ids with UVs, in rail-opposition order, mirrored
/// when the first face traverses the edge backwards.
fn strip_cells(ra: &Row, rb: &Row, a_first: bool) -> Vec<(Vec<u32>, Vec<[f32; 2]>)> {
    let (la, lb) = (ra.ids.len(), rb.ids.len());
    if la == lb {
        return Vec::new();
    }
    let m = la.max(lb);
    let exp = |len: usize| -> Vec<usize> {
        if len == m {
            (0..m).collect()
        } else {
            (0..m).map(|j| (j * (len - 1) * 2 + (m - 1)) / (2 * (m - 1))).collect()
        }
    };
    let (ea, eb) = (exp(la), exp(lb));
    let at = |a: bool, i: usize| -> (u32, [f32; 2]) {
        if a {
            (ra.ids[i], ra.uv[i])
        } else {
            (rb.ids[i], rb.uv[i])
        }
    };
    let mut cells = Vec::new();
    for j in 0..m - 1 {
        let quad = [(true, ea[j]), (true, ea[j + 1]), (false, eb[j + 1]), (false, eb[j])];
        let mut verts = Vec::with_capacity(4);
        let mut uvs = Vec::with_capacity(4);
        for (a, i) in quad {
            let (id, uv) = at(a, i);
            if verts.last() != Some(&id) {
                verts.push(id);
                uvs.push(uv);
            }
        }
        if verts.len() >= 3 && verts[0] != verts[verts.len() - 1] {
            if !a_first {
                verts.reverse();
                uvs.reverse();
            }
            cells.push((verts, uvs));
        }
    }
    cells
}

/// Superellipse cross-section from `p1` to `p2` around `apex`: the exponent
/// comes from `profile`, the endpoints land exactly so strips meet faces.
fn profile_points(
    p1: DVec3,
    p2: DVec3,
    apex: DVec3,
    uv1: [f32; 2],
    uv2: [f32; 2],
    s: usize,
    n_exp: f64,
) -> Profile {
    let mut pos = Vec::with_capacity(s + 1);
    let mut uv = Vec::with_capacity(s + 1);
    let (r1, r2) = ((p1 - apex).length(), (p2 - apex).length());
    let straight = || {
        let mut pos = Vec::with_capacity(s + 1);
        let mut uv = Vec::with_capacity(s + 1);
        for k in 0..=s {
            let t = k as f64 / s as f64;
            pos.push(p1 + (p2 - p1) * t);
            uv.push([uv1[0] + (uv2[0] - uv1[0]) * t as f32, uv1[1] + (uv2[1] - uv1[1]) * t as f32]);
        }
        Profile { pos, uv }
    };
    if r1 < 1e-12 || r2 < 1e-12 {
        return straight();
    }
    let u1 = (p1 - apex) / r1;
    let w = p2 - apex;
    let cos_o = ((w.dot(u1)) / r2).clamp(-1.0, 1.0);
    let perp = w - u1 * w.dot(u1);
    let sin_o = (perp.length() / r2).clamp(0.0, 1.0);
    if sin_o < 1e-9 {
        return straight();
    }
    let u2 = perp / perp.length();
    let b_denom = if n_exp <= 1.0 {
        1.0 / r2 - cos_o / r1
    } else if n_exp == 2.0 {
        (1.0 / (r2 * r2) - (cos_o / r1) * (cos_o / r1)).max(0.0).sqrt()
    } else {
        (1.0 / r2.powf(n_exp) - (cos_o / r1).powf(n_exp)).max(0.0).powf(1.0 / n_exp)
    };
    if b_denom <= 1e-18 {
        return straight();
    }
    let (a, b) = (r1, sin_o / b_denom);
    if !(a.is_finite() && b.is_finite() && b > 1e-12) {
        return straight();
    }
    let omega = sin_o.atan2(cos_o);
    for k in 0..=s {
        if k == 0 {
            pos.push(p1);
            uv.push(uv1);
            continue;
        }
        if k == s {
            pos.push(p2);
            uv.push(uv2);
            continue;
        }
        let t = k as f64 / s as f64;
        let (cp, sp) = ((omega * t).cos(), (omega * t).sin());
        let rho = if n_exp <= 1.0 {
            1.0 / (cp.abs() / a + sp.abs() / b)
        } else if n_exp == 2.0 {
            1.0 / ((cp / a) * (cp / a) + (sp / b) * (sp / b)).max(1e-24).sqrt()
        } else {
            1.0 / ((cp.abs() / a).powf(n_exp) + (sp.abs() / b).powf(n_exp)).max(1e-24).powf(1.0 / n_exp)
        };
        if !rho.is_finite() {
            return straight();
        }
        pos.push(apex + (u1 * cp + u2 * sp) * rho);
        uv.push([uv1[0] + (uv2[0] - uv1[0]) * t as f32, uv1[1] + (uv2[1] - uv1[1]) * t as f32]);
    }
    Profile { pos, uv }
}

fn lerp_uv(a: [f32; 2], b: [f32; 2], t: f64) -> [f32; 2] {
    [(a[0] + (b[0] - a[0]) * t as f32), (a[1] + (b[1] - a[1]) * t as f32)]
}


/// One vertex component: incident beveled edges in chain order with the
/// shared face between consecutive pair, cyclic when `closed`.
struct VComp {
    chain: Vec<u32>,
    closed: bool,
}

/// Orders incident edges (as other-ends) into face-linked chains over the
/// shared edge-loop walk: the link graph has max degree two (one pair per
/// incident face), so each component comes back as a path or a closed
/// cycle, plus singles for edges no face joins.
fn order_components(others: &BTreeSet<u32>, links: &BTreeMap<(u32, u32), Vec<u32>>) -> Vec<VComp> {
    let link_edges: BTreeSet<(u32, u32)> = links.keys().copied().collect();
    let mut chains = super::order_edge_loops(&link_edges);
    let mut covered: BTreeSet<u32> = chains.iter().flat_map(|c| c.iter().copied()).collect();
    for w in others {
        if covered.insert(*w) {
            chains.push(vec![*w]);
        }
    }
    chains
        .into_iter()
        .map(|chain| {
            let closed = super::loop_is_closed(&chain, &link_edges);
            VComp { chain, closed }
        })
        .collect()
}

/// Faces joining each other-end pair at `v`, over the given edge set.
fn links_at(mesh: &EditMesh, adj: &Adjacency, v: u32, set: &BTreeSet<(u32, u32)>) -> BTreeMap<(u32, u32), Vec<u32>> {
    let mut links: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    for f in adj.faces_of(v) {
        let verts = mesh.face_verts(*f as usize);
        if let Some(k) = verts.iter().position(|w| *w == v) {
            let n = verts.len();
            let (a, b) = (verts[(k + 1) % n], verts[(k + n - 1) % n]);
            let (ea, eb) = ((v.min(a), v.max(a)), (v.min(b), v.max(b)));
            if set.contains(&ea) && set.contains(&eb) {
                links.entry((a.min(b), a.max(b))).or_default().push(*f);
            }
        }
    }
    for v in links.values_mut() {
        v.sort_unstable();
    }
    links
}

/// One loop-corner insert: edge index, vertex id, UV, normal.
type LoopIns = (usize, u32, [f32; 2], Option<[f32; 3]>);

/// One lattice corner: vertex id, position, UV.
type LatCorner = (u32, DVec3, [f32; 2]);

/// The patch where three bevelled edges meet at a convex corner, laid out as
/// Blender lays it: three quad sectors around the corner's centre, plus a
/// centre triangle joined to each side by a band when `segments` is odd, so a
/// corner has 3h^2 faces (even, h = segments / 2) or 3h(h + 1) + 1 (odd).
/// `sides` run joint to joint around the patch; interior points are blended
/// from the boundary and, for a circular profile, put on the sphere the three
/// profiles lie on. `fresh` turns a new position into a vertex id.
fn corner_grid(
    sides: [&Vec<LatCorner>; 3],
    segments: usize,
    profile: f64,
    fresh: &mut dyn FnMut(DVec3) -> u32,
) -> Vec<Vec<LatCorner>> {
    let n = segments;
    let h = n / 2;
    let ring: Vec<DVec3> = sides.iter().flat_map(|s| s.iter().take(n).map(|c| c.1)).collect();
    let sphere = if (profile - 0.5).abs() < 1e-6 { fit_sphere(&ring) } else { None };
    let place = |q: DVec3| match sphere {
        Some((c, r)) => c + (q - c).normalize_or_zero() * r,
        None => q,
    };
    let joint = |i: usize| sides[i][n];
    let centre_pos = place((joint(0).1 + joint(1).1 + joint(2).1) / 3.0);
    let centre_uv: [f32; 2] = [0, 1].map(|k| (joint(0).2[k] + joint(1).2[k] + joint(2).2[k]) / 3.0);
    if h == 0 {
        return vec![(0..3).map(joint).collect()];
    }
    // One triangle vertex per joint; all three are the centre when even.
    let tri: Vec<LatCorner> = if n.is_multiple_of(2) {
        let c = (fresh(centre_pos), centre_pos, centre_uv);
        vec![c, c, c]
    } else {
        (0..3)
            .map(|i| {
                let j = joint(i);
                let t = 1.0 / n as f64;
                let q = place(centre_pos.lerp(j.1, t));
                let uv = [0, 1].map(|k| centre_uv[k] + (j.2[k] - centre_uv[k]) * t as f32);
                (fresh(q), q, uv)
            })
            .collect()
    };
    let mut spoke = |from: LatCorner, to: LatCorner| -> Vec<LatCorner> {
        (0..=h)
            .map(|v| {
                if v == 0 {
                    return from;
                }
                if v == h {
                    return to;
                }
                let t = v as f64 / h as f64;
                let q = place(from.1.lerp(to.1, t));
                let uv = [0, 1].map(|k| from.2[k] + (to.2[k] - from.2[k]) * t as f32);
                (fresh(q), q, uv)
            })
            .collect()
    };
    // Side j runs from joint j-1 to joint j. Its left spoke leaves its point h
    // towards the triangle vertex of joint j-1, its right spoke leaves point
    // n-h towards that of joint j; with an even count they are one line.
    let mut left: Vec<Vec<LatCorner>> = Vec::with_capacity(3);
    let mut right: Vec<Vec<LatCorner>> = Vec::with_capacity(3);
    for j in 0..3 {
        let l = spoke(sides[j][h], tri[(j + 2) % 3]);
        let r = if n.is_multiple_of(2) { l.clone() } else { spoke(sides[j][n - h], tri[j]) };
        left.push(l);
        right.push(r);
    }
    let mut faces: Vec<Vec<LatCorner>> = Vec::new();
    for i in 0..3 {
        // The sector at joint i, between the end of side i and the start of side a.
        let a = (i + 1) % 3;
        let mut g: BTreeMap<(usize, usize), LatCorner> = BTreeMap::new();
        for u in 0..=h {
            g.insert((u, 0), sides[a][u]);
            g.insert((u, h), right[i][u]);
        }
        for v in 0..=h {
            g.insert((0, v), sides[i][n - v]);
            g.insert((h, v), left[a][v]);
        }
        for u in 1..h {
            for v in 1..h {
                let (fu, fv) = (u as f64 / h as f64, v as f64 / h as f64);
                let coons = |k: usize| -> f64 {
                    let at = |uu: usize, vv: usize| -> f64 {
                        let c = g[&(uu, vv)];
                        if k < 3 { c.1[k] } else { c.2[k - 3] as f64 }
                    };
                    (1.0 - fu) * at(0, v) + fu * at(h, v) + (1.0 - fv) * at(u, 0) + fv * at(u, h)
                        - (1.0 - fu) * (1.0 - fv) * at(0, 0)
                        - fu * (1.0 - fv) * at(h, 0)
                        - (1.0 - fu) * fv * at(0, h)
                        - fu * fv * at(h, h)
                };
                let q = place(DVec3::new(coons(0), coons(1), coons(2)));
                let uv = [coons(3) as f32, coons(4) as f32];
                g.insert((u, v), (fresh(q), q, uv));
            }
        }
        for u in 0..h {
            for v in 0..h {
                faces.push(vec![g[&(u, v)], g[&(u + 1, v)], g[&(u + 1, v + 1)], g[&(u, v + 1)]]);
            }
        }
    }
    if !n.is_multiple_of(2) {
        for j in 0..3 {
            for v in 0..h {
                faces.push(vec![left[j][v], right[j][v], right[j][v + 1], left[j][v + 1]]);
            }
        }
        faces.push(tri);
    }
    faces
}

/// Centre and radius of the sphere through `pts` (least squares), or `None`
/// when they do not pin one down.
fn fit_sphere(pts: &[DVec3]) -> Option<(DVec3, f64)> {
    if pts.len() < 4 {
        return None;
    }
    // |p|^2 = 2 p.c + k with k = r^2 - |c|^2 is linear in (c, k).
    let mut ata = [[0.0f64; 4]; 4];
    let mut atb = [0.0f64; 4];
    for q in pts {
        let row = [2.0 * q.x, 2.0 * q.y, 2.0 * q.z, 1.0];
        let rhs = q.length_squared();
        for i in 0..4 {
            atb[i] += row[i] * rhs;
            for j in 0..4 {
                ata[i][j] += row[i] * row[j];
            }
        }
    }
    let x = solve4(ata, atb)?;
    let c = DVec3::new(x[0], x[1], x[2]);
    let r2 = x[3] + c.length_squared();
    if !(r2.is_finite() && r2 > 0.0) {
        return None;
    }
    let r = r2.sqrt();
    let worst = pts.iter().map(|q| ((*q - c).length() - r).abs()).fold(0.0, f64::max);
    (worst <= r * 1e-3).then_some((c, r))
}

fn solve4(mut a: [[f64; 4]; 4], mut b: [f64; 4]) -> Option<[f64; 4]> {
    for col in 0..4 {
        let piv = (col..4).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        let pivot_row = a[col];
        let pivot_b = b[col];
        for (row, (ar, br)) in a.iter_mut().zip(b.iter_mut()).enumerate() {
            if row != col {
                let f = ar[col] / pivot_row[col];
                for (x, px) in ar.iter_mut().zip(pivot_row.iter()).skip(col) {
                    *x -= f * px;
                }
                *br -= f * pivot_b;
            }
        }
    }
    Some([b[0] / a[0][0], b[1] / a[1][1], b[2] / a[2][2], b[3] / a[3][3]])
}

struct NewFace {
    verts: Vec<u32>,
    uvs: Vec<[f32; 2]>,
    material: u16,
}

/// Everything `build_edges` needs beyond the mesh itself.
struct EdgeBuild<'a> {
    adj: &'a Adjacency,
    edges: &'a [BevEdge],
    set: &'a BTreeSet<(u32, u32)>,
    valence: &'a BTreeMap<u32, usize>,
    p: &'a Params,
    amount: f64,
    scale: f64,
    segments: usize,
    n_exp: f64,
}

/// Builds the bevel at one scale. `None` means the rails cross at this
/// scale and the caller halves; `Err` is a hard refusal.
fn build_edges(mesh: &EditMesh, b: &EdgeBuild) -> Result<Option<Commit>, crate::editmesh::EditError> {
    let EdgeBuild { adj, edges, set, valence, p, amount, scale, segments, n_exp } = *b;
    let mut offs = BTreeMap::new();
    for e in edges {
        offs.insert((e.a, e.b), convert(e, p.offset_type, amount) * scale);
    }
    let mut touched: BTreeSet<u32> = BTreeSet::new();
    for e in edges {
        touched.insert(e.f1);
        touched.insert(e.f2);
    }
    let mut shrinks: BTreeMap<u32, Shrink> = BTreeMap::new();
    for f in &touched {
        let s = shrink_face(mesh, *f, set, &offs)?;
        if s.oob {
            return Ok(None);
        }
        shrinks.insert(*f, s);
    }
    let mut pinched: BTreeSet<(u32, u32, u32)> = BTreeSet::new();
    if !p.loop_slide {
        for e in edges {
            for end in [e.a, e.b] {
                if valence.get(&end) != Some(&1usize) {
                    continue;
                }
                if shrinks.get(&e.f1).is_some_and(|s| s.fresh.contains_key(&end))
                    || shrinks.get(&e.f2).is_some_and(|s| s.fresh.contains_key(&end))
                {
                    pinched.insert((e.a, e.b, end));
                }
            }
        }
        for (v, n) in valence {
            if *n == 1 {
                for f in adj.faces_of(*v) {
                    if let Some(s) = shrinks.get_mut(f) {
                        s.fresh.remove(v);
                        s.alias.remove(v);
                        s.splits.retain(|sp| sp.from != *v);
                    }
                }
            }
        }
    }
    // A terminating rail that must not slide pinches to its corner: the
    // strip row is that corner alone and the fan covers the taper.
    let is_pinch_end = |e: &BevEdge, end: u32| -> bool { pinched.contains(&(e.a, e.b, end)) };
    for (f, s) in &shrinks {
        let loop_v = mesh.face_verts(*f as usize);
        let mut pts = Vec::with_capacity(loop_v.len());
        for w in &loop_v {
            if let Some(q) = s.fresh.get(w) {
                pts.push(*q);
            } else if let Some(r) = s.alias.get(w) {
                pts.push(mesh.positions[*r as usize]);
            } else {
                pts.push(mesh.positions[*w as usize]);
            }
        }
        let orig = mesh.face_normal(*f as usize);
        let nn = newell(&pts);
        if nn.length() * 0.5 < DEGENERATE_AREA || nn.normalize().dot(orig) <= 0.0 {
            return Ok(None);
        }
    }
    let mut splits: BTreeMap<(u32, u32), Vec<EdgeSplit>> = BTreeMap::new();
    for s in shrinks.values() {
        for sp in &s.splits {
            splits.entry(sp.edge).or_default().push(EdgeSplit { t: sp.t, pos: sp.pos, from: sp.from });
        }
    }
    for v in splits.values_mut() {
        v.sort_by(|a, b| a.t.total_cmp(&b.t));
        let mut deduped: Vec<EdgeSplit> = Vec::with_capacity(v.len());
        for sp in v.drain(..) {
            if deduped.last().is_none_or(|p| (sp.t - p.t).abs() > 1e-9 || sp.from != p.from) {
                deduped.push(sp);
            }
        }
        *v = deduped;
    }
    let end_pos = |shrinks: &BTreeMap<u32, Shrink>, f: u32, w: u32| -> DVec3 {
        if let Some(s) = shrinks.get(&f) {
            if let Some(q) = s.fresh.get(&w) {
                return *q;
            }
            if let Some(r) = s.alias.get(&w) {
                return mesh.positions[*r as usize];
            }
        }
        mesh.positions[w as usize]
    };
    let mut profiles: BTreeMap<(u32, u32, u32), Profile> = BTreeMap::new();
    for e in edges {
        for end in [e.a, e.b] {
            let (p1, p2) = (end_pos(&shrinks, e.f1, end), end_pos(&shrinks, e.f2, end));
            let (uv1, uv2) = (corner_uv(mesh, e.f1, end), corner_uv(mesh, e.f2, end));
            let vpos = mesh.positions[end as usize];
            profiles.insert((e.a, e.b, end), profile_points(p1, p2, vpos, uv1, uv2, segments, n_exp));
        }
    }
    // A rail end that slides onto unbevelled neighbours terminates through
    // its corner: the strip row runs rail, corner, rail and no patch fills
    // the end.
    let is_slide_end = |e: &BevEdge, end: u32| -> bool {
        p.loop_slide
            && valence.get(&end) == Some(&1usize)
            && shrinks.get(&e.f1).is_some_and(|s| s.fresh.contains_key(&end))
            && shrinks.get(&e.f2).is_some_and(|s| s.fresh.contains_key(&end))
    };
    let slot_of = |faces: &[u32]| -> u16 {
        if p.material >= 0 {
            return p.material as u16;
        }
        faces.iter().min().map(|f| mesh.faces[*f as usize].material).unwrap_or(0)
    };
    let mut fresh_pos: Vec<DVec3> = Vec::new();
    let mut shr_id: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    let base = mesh.positions.len() as u32;
    for f in &touched {
        let s = &shrinks[f];
        for w in mesh.face_verts(*f as usize) {
            if s.fresh.contains_key(&w) {
                shr_id.insert((*f, w), base + fresh_pos.len() as u32);
                fresh_pos.push(s.fresh[&w]);
            }
        }
    }
    let resolve = |shrinks: &BTreeMap<u32, Shrink>, shr_id: &BTreeMap<(u32, u32), u32>, f: u32, w: u32| -> u32 {
        if let Some(id) = shr_id.get(&(f, w)) {
            return *id;
        }
        if let Some(s) = shrinks.get(&f) {
            if let Some(r) = s.alias.get(&w) {
                return *r;
            }
        }
        w
    };
    let mut vinc: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for e in edges {
        vinc.entry(e.a).or_default().push(e.b);
        vinc.entry(e.b).or_default().push(e.a);
    }
    for w in vinc.values_mut() {
        w.sort_unstable();
    }
    let edge_of = |a: u32, b: u32| -> &BevEdge {
        edges.iter().find(|e| (e.a, e.b) == (a.min(b), a.max(b))).expect("a linked edge is beveled")
    };
    let mut through_v: BTreeMap<(u32, u32, u32), Row> = BTreeMap::new();
    for (v, nbrs) in &vinc {
        let v = *v;
        let vset: BTreeSet<(u32, u32)> = nbrs.iter().map(|w| (v.min(*w), v.max(*w))).collect();
        let others: BTreeSet<u32> = nbrs.iter().copied().collect();
        let links = links_at(mesh, adj, v, &vset);
        for comp in order_components(&others, &links) {
            if comp.closed || comp.chain.len() != 2 {
                continue;
            }
            for w in comp.chain.iter().copied() {
                let e = edge_of(v, w);
                let (u1, u2) = (corner_uv(mesh, e.f1, v), corner_uv(mesh, e.f2, v));
                let (c1, c2) = (resolve(&shrinks, &shr_id, e.f1, v), resolve(&shrinks, &shr_id, e.f2, v));
                let (p1, p2) = (end_pos(&shrinks, e.f1, v), end_pos(&shrinks, e.f2, v));
                through_v.insert(
                    (e.a, e.b, v),
                    Row {
                        ids: vec![c1, v, c2],
                        pos: vec![p1, mesh.positions[v as usize], p2],
                        uv: vec![u1, lerp_uv(u1, u2, 0.5), u2],
                    },
                );
            }
        }
    }
    let mut mid_id: BTreeMap<(u32, u32, u32, u32), u32> = BTreeMap::new();
    for e in edges {
        for end in [e.a, e.b] {
            if is_slide_end(e, end) || is_pinch_end(e, end) || through_v.contains_key(&(e.a, e.b, end)) {
                continue;
            }
            let prof = &profiles[&(e.a, e.b, end)];
            for k in 1..segments {
                mid_id.insert((e.a, e.b, end, k as u32), base + fresh_pos.len() as u32);
                fresh_pos.push(prof.pos[k]);
            }
        }
    }
    let mut split_id: BTreeMap<(u32, u32, usize), u32> = BTreeMap::new();
    for ((a, b), list) in &splits {
        for (i, sp) in list.iter().enumerate() {
            split_id.insert((*a, *b, i), base + fresh_pos.len() as u32);
            fresh_pos.push(sp.pos);
        }
    }
    let split_uv = |loop_uv: &[[f32; 2]], at: usize, t: f64| -> [f32; 2] {
        lerp_uv(loop_uv[at], loop_uv[(at + 1) % loop_uv.len()], t)
    };
    let mut rows: BTreeMap<(u32, u32, u32), Row> = BTreeMap::new();
    for e in edges {
        for end in [e.a, e.b] {
            if let Some(trow) = through_v.get(&(e.a, e.b, end)) {
                rows.insert((e.a, e.b, end), trow.clone());
                continue;
            }
            if is_pinch_end(e, end) {
                let (u1, u2) = (corner_uv(mesh, e.f1, end), corner_uv(mesh, e.f2, end));
                rows.insert(
                    (e.a, e.b, end),
                    Row {
                        ids: vec![end],
                        pos: vec![mesh.positions[end as usize]],
                        uv: vec![lerp_uv(u1, u2, 0.5)],
                    },
                );
                continue;
            }
            if is_slide_end(e, end) {
                let (c1, c2) = (resolve(&shrinks, &shr_id, e.f1, end), resolve(&shrinks, &shr_id, e.f2, end));
                let (u1, u2) = (corner_uv(mesh, e.f1, end), corner_uv(mesh, e.f2, end));
                rows.insert(
                    (e.a, e.b, end),
                    Row {
                        ids: vec![c1, end, c2],
                        pos: vec![end_pos(&shrinks, e.f1, end), mesh.positions[end as usize], end_pos(&shrinks, e.f2, end)],
                        uv: vec![u1, lerp_uv(u1, u2, 0.5), u2],
                    },
                );
                continue;
            }
            let prof = &profiles[&(e.a, e.b, end)];
            let mut ids = Vec::with_capacity(segments + 1);
            for k in 0..=segments {
                if k == 0 {
                    ids.push(resolve(&shrinks, &shr_id, e.f1, end));
                } else if k == segments {
                    ids.push(resolve(&shrinks, &shr_id, e.f2, end));
                } else {
                    ids.push(mid_id[&(e.a, e.b, end, k as u32)]);
                }
            }
            rows.insert((e.a, e.b, end), Row { ids, pos: prof.pos.clone(), uv: prof.uv.clone() });
        }
    }
    for e in edges {
        let (ra, rb) = (&rows[&(e.a, e.b, e.a)], &rows[&(e.a, e.b, e.b)]);
        let at = |a: bool, i: usize| -> DVec3 {
            if a {
                ra.pos[i]
            } else {
                rb.pos[i]
            }
        };
        for (qa, qb) in strip_quads(ra.ids.len(), rb.ids.len()) {
            if newell(&[at(true, qa[0]), at(true, qa[1]), at(false, qb[1]), at(false, qb[0])]).length() * 0.5
                < DEGENERATE_AREA
            {
                return Ok(None);
            }
        }
        let f1loop = mesh.face_verts(e.f1 as usize);
        let ka = f1loop.iter().position(|w| *w == e.a).unwrap_or(0);
        let a_first = f1loop[(ka + 1) % f1loop.len()] == e.b;
        for (verts, _) in strip_cells(ra, rb, a_first) {
            let pts: Vec<DVec3> = verts
                .iter()
                .map(|v| if (*v as usize) < mesh.positions.len() { mesh.positions[*v as usize] } else { fresh_pos[(*v - base) as usize] })
                .collect();
            if newell(&pts).length() * 0.5 < DEGENERATE_AREA {
                return Ok(None);
            }
        }
    }
    let mut split_faces: BTreeSet<u32> = touched.clone();
    for (fi, _) in mesh.faces.iter().enumerate() {
        let loop_v = mesh.face_verts(fi);
        for k in 0..loop_v.len() {
            let (x, y) = (loop_v[k], loop_v[(k + 1) % loop_v.len()]);
            if x != y && splits.contains_key(&(x.min(y), x.max(y))) {
                split_faces.insert(fi as u32);
                break;
            }
        }
    }
    let mut rewritten: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut rewritten_uv: BTreeMap<u32, Vec<[f32; 2]>> = BTreeMap::new();
    let mut rewritten_normal: BTreeMap<u32, Vec<Option<[f32; 3]>>> = BTreeMap::new();
    for f in &split_faces {
        let loop_v = mesh.face_verts(*f as usize);
        let face = mesh.faces[*f as usize];
        let orig: Vec<Corner> = mesh.corners[face.first as usize..face.first as usize + face.len as usize].to_vec();
        let orig_uv: Vec<[f32; 2]> = orig.iter().map(|c| c.uv).collect();
        let mut verts: Vec<u32> = Vec::with_capacity(loop_v.len() + 2);
        let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(loop_v.len() + 2);
        let mut norms: Vec<Option<[f32; 3]>> = Vec::with_capacity(loop_v.len() + 2);
        if touched.contains(f) {
            for (k, w) in loop_v.iter().enumerate() {
                verts.push(resolve(&shrinks, &shr_id, *f, *w));
                uvs.push(orig_uv[k]);
                norms.push(orig[k].normal);
            }
            let m = verts.len();
            let mut ins: Vec<LoopIns> = Vec::new();
            for k in 0..m {
                let (x, y) = (verts[k], verts[(k + 1) % m]);
                if x == y {
                    continue;
                }
                if let Some(list) = splits.get(&(x.min(y), x.max(y))) {
                    let mut ordered: Vec<(f64, u32)> = list
                        .iter()
                        .enumerate()
                        .map(|(i, sp)| {
                            let txy = if x == x.min(y) { sp.t } else { 1.0 - sp.t };
                            (txy, split_id[&(x.min(y), x.max(y), i)])
                        })
                        .collect();
                    ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
                    for (txy, id) in ordered {
                        ins.push((k, id, split_uv(&uvs, k, txy), super::loopcut::lerp_normal(norms[k], norms[(k + 1) % m], txy)));
                    }
                }
            }
            ins.sort_by(|a, b| a.0.cmp(&b.0));
            let mut grown_v = Vec::with_capacity(verts.len() + ins.len());
            let mut grown_u = Vec::with_capacity(uvs.len() + ins.len());
            let mut grown_n = Vec::with_capacity(norms.len() + ins.len());
            let mut ii = 0usize;
            for k in 0..m {
                grown_v.push(verts[k]);
                grown_u.push(uvs[k]);
                grown_n.push(norms[k]);
                while ii < ins.len() && ins[ii].0 == k {
                    let (_, id, uv, normal) = ins[ii];
                    grown_v.push(id);
                    grown_u.push(uv);
                    grown_n.push(normal);
                    ii += 1;
                }
            }
            verts = grown_v;
            uvs = grown_u;
            norms = grown_n;
        } else {
            let m = loop_v.len();
            for k in 0..m {
                let (v, n) = (loop_v[k], loop_v[(k + 1) % m]);
                verts.push(v);
                uvs.push(orig_uv[k]);
                norms.push(orig[k].normal);
                let ek = (v.min(n), v.max(n));
                if let Some(list) = splits.get(&ek) {
                    let mut ordered: Vec<(f64, usize)> = list
                        .iter()
                        .enumerate()
                        .map(|(i, sp)| (if v == ek.0 { sp.t } else { 1.0 - sp.t }, i))
                        .collect();
                    ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
                    for (txy, i) in ordered {
                        verts.push(split_id[&(ek.0, ek.1, i)]);
                        uvs.push(lerp_uv(orig_uv[k], orig_uv[(k + 1) % m], txy));
                        norms.push(super::loopcut::lerp_normal(orig[k].normal, orig[(k + 1) % m].normal, txy));
                    }
                }
            }
        }
        if touched.contains(f) && !p.harden_normals {
            for n in norms.iter_mut() {
                *n = None;
            }
        }
        rewritten.insert(*f, verts);
        rewritten_uv.insert(*f, uvs);
        rewritten_normal.insert(*f, norms);
    }
    let mut new_faces: Vec<NewFace> = Vec::new();
    for e in edges {
        let (ra, rb) = (&rows[&(e.a, e.b, e.a)], &rows[&(e.a, e.b, e.b)]);
        let f1loop = mesh.face_verts(e.f1 as usize);
        let ka = f1loop.iter().position(|w| *w == e.a).unwrap_or(0);
        let a_first = f1loop[(ka + 1) % f1loop.len()] == e.b;
        let mat = slot_of(&[e.f1, e.f2]);
        let outward = mesh.face_normal(e.f1 as usize) + mesh.face_normal(e.f2 as usize);
        let mut cells: Vec<(Vec<u32>, Vec<[f32; 2]>)> = Vec::new();
        for (qa, qb) in strip_quads(ra.ids.len(), rb.ids.len()) {
            let (verts, uvs) = if a_first {
                (vec![ra.ids[qa[0]], ra.ids[qa[1]], rb.ids[qb[1]], rb.ids[qb[0]]],
                 vec![ra.uv[qa[0]], ra.uv[qa[1]], rb.uv[qb[1]], rb.uv[qb[0]]])
            } else {
                (vec![ra.ids[qa[0]], rb.ids[qb[0]], rb.ids[qb[1]], ra.ids[qa[1]]],
                 vec![ra.uv[qa[0]], rb.uv[qb[0]], rb.uv[qb[1]], ra.uv[qa[1]]])
            };
            cells.push((verts, uvs));
        }
        cells.extend(strip_cells(ra, rb, a_first));
        let pos_of = |v: u32| -> DVec3 {
            if (v as usize) < mesh.positions.len() {
                mesh.positions[v as usize]
            } else {
                fresh_pos[(v - base) as usize]
            }
        };
        for (verts, _) in &cells {
            let pts: Vec<DVec3> = verts.iter().map(|v| pos_of(*v)).collect();
            if newell(&pts).length() * 0.5 < DEGENERATE_AREA {
                continue;
            }
        }
        let flip = cells.first().is_some_and(|(verts, _)| {
            let pts: Vec<DVec3> = verts.iter().map(|v| pos_of(*v)).collect();
            newell(&pts).normalize_or_zero().dot(outward) < 0.0
        });
        for (verts, uvs) in cells {
            let (verts, uvs) = if !flip {
                (verts, uvs)
            } else if verts.len() == 4 {
                (vec![verts[0], verts[3], verts[2], verts[1]], vec![uvs[0], uvs[3], uvs[2], uvs[1]])
            } else {
                (verts.iter().rev().copied().collect(), uvs.iter().rev().copied().collect())
            };
            new_faces.push(NewFace { verts, uvs, material: mat });
        }
    }
    let surrounded = |v: u32| -> bool {
        adj.faces_of(v).iter().all(|f| {
            let verts = mesh.face_verts(*f as usize);
            verts.contains(&v)
                && {
                    let k = verts.iter().position(|w| *w == v).unwrap_or(0);
                    let n = verts.len();
                    let (a, b) = (verts[(k + 1) % n], verts[(k + n - 1) % n]);
                    set.contains(&(v.min(a), v.max(a))) || set.contains(&(v.min(b), v.max(b)))
                }
        })
    };
    for (v, nbrs) in &vinc {
        let v = *v;
        let vset: BTreeSet<(u32, u32)> = nbrs.iter().map(|w| (v.min(*w), v.max(*w))).collect();
        let others: BTreeSet<u32> = nbrs.iter().copied().collect();
        let links = links_at(mesh, adj, v, &vset);
        for comp in order_components(&others, &links) {
            let k = comp.chain.len();
            if k == 1 {
                let e = edge_of(v, comp.chain[0]);
                if is_slide_end(e, v) || is_pinch_end(e, v) {
                    continue;
                }
            }
            if !comp.closed && k == 2 {
                let (e0, e1) = (edge_of(v, comp.chain[0]), edge_of(v, comp.chain[1]));
                if through_v.contains_key(&(e0.a, e0.b, v)) && through_v.contains_key(&(e1.a, e1.b, v)) {
                    continue;
                }
            }
            let shared_face = |i: usize, j: usize| -> u32 {
                let (a, b) = (comp.chain[i], comp.chain[j]);
                links[&(a.min(b), a.max(b))][0]
            };
            let mut comp_faces: Vec<u32> = Vec::new();
            let mut bounds: Vec<Vec<(u32, DVec3, [f32; 2])>> = Vec::with_capacity(k);
            for i in 0..k {
                let w = comp.chain[i];
                let e = edge_of(v, w);
                let (fa, fb) = if k == 1 {
                    let faces = adj.edge(v, w).map(|ed| ed.faces.clone()).unwrap_or_default();
                    (faces.first().copied().unwrap_or(e.f1), faces.get(1).copied().unwrap_or(e.f2))
                } else if i == 0 && !comp.closed {
                    let outer = adj
                        .edge(v, w)
                        .map(|ed| ed.faces.iter().copied().find(|f| *f != shared_face(0, 1)).unwrap_or(e.f1))
                        .unwrap_or(e.f1);
                    (outer, shared_face(0, 1))
                } else if i + 1 == k && !comp.closed {
                    (shared_face(i - 1, i), adj.edge(v, w).map(|ed| ed.faces.iter().copied().find(|f| *f != shared_face(i - 1, i)).unwrap_or(e.f2)).unwrap_or(e.f2))
                } else {
                    let prev = shared_face((i + k - 1) % k, i);
                    let next = shared_face(i, (i + 1) % k);
                    (prev, next)
                };
                comp_faces.push(fa);
                comp_faces.push(fb);
                let prof = &profiles[&(e.a, e.b, v)];
                let fwd = (fa, fb) == (e.f1, e.f2);
                let mut line = Vec::with_capacity(segments + 1);
                for m in 0..=segments {
                    let id = if m == 0 {
                        resolve(&shrinks, &shr_id, fa, v)
                    } else if m == segments {
                        resolve(&shrinks, &shr_id, fb, v)
                    } else if fwd {
                        mid_id[&(e.a, e.b, v, m as u32)]
                    } else {
                        mid_id[&(e.a, e.b, v, (segments - m) as u32)]
                    };
                    let pi = if fwd { m } else { segments - m };
                    line.push((id, prof.pos[pi], prof.uv[pi]));
                }
                bounds.push(line);
            }
            comp_faces.sort_unstable();
            comp_faces.dedup();
            let outward = outward_ref(mesh, &comp_faces);
            let mat = slot_of(&comp_faces);
            let vpos = mesh.positions[v as usize];
            let closed_loop = comp.closed && surrounded(v);
            if closed_loop && k == 3 {
                let joints = [bounds[2][segments], bounds[0][segments], bounds[1][segments]];
                let npts: usize = bounds.iter().map(|l| l.len()).sum();
                let centroid =
                    bounds.iter().flat_map(|l| l.iter().map(|(_, q, _)| *q)).sum::<DVec3>() / npts as f64;
                let convex = outward != DVec3::ZERO && outward.normalize().dot(vpos - centroid) > 1e-9;
                let snapped = joints.iter().any(|(id, _, _)| *id == v);
                if convex && !snapped {
                    let sides = [&bounds[0], &bounds[1], &bounds[2]];
                    let faces = corner_grid(sides, segments, p.profile, &mut |q| {
                        let id = base + fresh_pos.len() as u32;
                        fresh_pos.push(q);
                        id
                    });
                    for f in &faces {
                        let pts: Vec<DVec3> = f.iter().map(|c| c.1).collect();
                        let n = newell(&pts);
                        if n.length() * 0.5 < DEGENERATE_AREA {
                            return Ok(None);
                        }
                        let mut f = f.clone();
                        if outward != DVec3::ZERO && n.dot(outward) < 0.0 {
                            f.reverse();
                        }
                        new_faces.push(NewFace {
                            verts: f.iter().map(|c| c.0).collect(),
                            uvs: f.iter().map(|c| c.2).collect(),
                            material: mat,
                        });
                    }
                    continue;
                }
            }
            let mut poly: Vec<(u32, DVec3, [f32; 2])> = Vec::new();
            for (li, line) in bounds.iter().enumerate() {
                if closed_loop {
                    for pt in line.iter().take(segments) {
                        poly.push(*pt);
                    }
                } else if li == 0 {
                    for pt in line {
                        poly.push(*pt);
                    }
                } else {
                    for pt in line.iter().skip(1) {
                        poly.push(*pt);
                    }
                }
            }
            if !closed_loop {
                let vpt = (v, vpos, corner_uv(mesh, comp_faces[0], v));
                if k == 1 && !poly.is_empty() {
                    poly.insert(1, vpt);
                } else {
                    poly.push(vpt);
                }
            }
            let mut clean: Vec<(u32, DVec3, [f32; 2])> = Vec::with_capacity(poly.len());
            for pt in poly {
                if clean.iter().all(|last: &(u32, DVec3, [f32; 2])| (last.1 - pt.1).length() > 1e-9) {
                    clean.push(pt);
                }
            }
            if clean.len() < 3 {
                continue;
            }
            let pts: Vec<DVec3> = clean.iter().map(|(_, q, _)| *q).collect();
            if newell(&pts).length() * 0.5 < DEGENERATE_AREA {
                return Ok(None);
            }
            let verts0: Vec<u32> = clean.iter().map(|(id, _, _)| *id).collect();
            let uvs0: Vec<[f32; 2]> = clean.iter().map(|(_, _, uv)| *uv).collect();
            let nn = newell(&pts).normalize_or_zero();
            let d = if outward == DVec3::ZERO { 0.0 } else { nn.dot(outward) };
            let reversed = d < -1e-6;
            let (verts, uvs) = if reversed {
                (verts0.iter().rev().copied().collect(), uvs0.iter().rev().copied().collect())
            } else {
                (verts0, uvs0)
            };
            new_faces.push(NewFace { verts, uvs, material: mat });
        }
    }
    let harden = if p.harden_normals { touched.clone() } else { BTreeSet::new() };
    Ok(Some(Commit {
        fresh_pos,
        rewritten,
        rewritten_uv,
        rewritten_normal,
        new_faces,
        harden,
    }))
}

/// Everything the commit writes: fresh positions in allocation order, the
/// rewritten loops with their UVs and normals, and the new faces.
struct Commit {
    fresh_pos: Vec<DVec3>,
    rewritten: BTreeMap<u32, Vec<u32>>,
    rewritten_uv: BTreeMap<u32, Vec<[f32; 2]>>,
    rewritten_normal: BTreeMap<u32, Vec<Option<[f32; 3]>>>,
    new_faces: Vec<NewFace>,
    harden: BTreeSet<u32>,
}

fn commit(mesh: &mut EditMesh, d: Commit, p: &Params) -> Selection {
    let base = mesh.positions.len() as u32;
    let mut order: Vec<usize> = (0..d.fresh_pos.len()).collect();
    order.sort_by(|a, b| {
        d.fresh_pos[*a]
            .x
            .total_cmp(&d.fresh_pos[*b].x)
            .then(d.fresh_pos[*a].y.total_cmp(&d.fresh_pos[*b].y))
            .then(d.fresh_pos[*a].z.total_cmp(&d.fresh_pos[*b].z))
    });
    let mut rep: Vec<u32> = (0..d.fresh_pos.len()).map(|i| base + i as u32).collect();
    let mut start: Option<usize> = None;
    for i in order {
        match start {
            None => start = Some(i),
            Some(r) => {
                if (d.fresh_pos[i] - d.fresh_pos[r]).length() < 1e-9 {
                    rep[i] = base + r as u32;
                } else {
                    start = Some(i);
                }
            }
        }
    }
    for q in &d.fresh_pos {
        mesh.positions.push(*q);
    }
    let map_id = |v: u32| -> u32 {
        if v >= base {
            rep[(v - base) as usize]
        } else {
            v
        }
    };
    let mut faces: Vec<Face> = Vec::with_capacity(mesh.faces.len() + d.new_faces.len());
    let mut corners: Vec<Corner> =
        Vec::with_capacity(mesh.corners.len() + d.new_faces.iter().map(|f| f.verts.len()).sum::<usize>());
    for (fi, f) in mesh.faces.iter().enumerate() {
        let first = corners.len() as u32;
        if let Some(loop_v) = d.rewritten.get(&(fi as u32)) {
            let uvs = &d.rewritten_uv[&(fi as u32)];
            let kept = &d.rewritten_normal[&(fi as u32)];
            let harden_normal = if d.harden.contains(&(fi as u32)) {
                let pts: Vec<DVec3> = loop_v.iter().map(|v| mesh.positions[map_id(*v) as usize]).collect();
                let n = newell(&pts).normalize_or_zero();
                (n != DVec3::ZERO).then_some([n.x as f32, n.y as f32, n.z as f32])
            } else {
                None
            };
            for ((v, uv), keep) in loop_v.iter().zip(uvs.iter()).zip(kept.iter()) {
                corners.push(Corner { vertex: map_id(*v), uv: *uv, normal: harden_normal.or(*keep) });
            }
            faces.push(Face { first, len: loop_v.len() as u32, material: f.material, flags: f.flags });
        } else {
            corners.extend_from_slice(&mesh.corners[f.first as usize..f.first as usize + f.len as usize]);
            let mut f = *f;
            f.first = first;
            faces.push(f);
        }
    }
    let made_start = faces.len() as u32;
    for nf in &d.new_faces {
        let first = corners.len() as u32;
        for (v, uv) in nf.verts.iter().zip(nf.uvs.iter()) {
            corners.push(Corner { vertex: map_id(*v), uv: *uv, normal: None });
        }
        let mut flags = FaceFlags::empty();
        flags.set(FaceFlags::SMOOTH, true);
        faces.push(Face { first, len: nf.verts.len() as u32, material: nf.material, flags });
    }
    mesh.faces = faces;
    mesh.corners = corners;
    super::compact(mesh);
    let adj = Adjacency::build(mesh);
    let is_new = |f: u32| f >= made_start;
    let mut edge_of_new: BTreeSet<(u32, u32)> = BTreeSet::new();
    for fi in made_start..mesh.faces.len() as u32 {
        let verts = mesh.face_verts(fi as usize);
        for k in 0..verts.len() {
            let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
            if a != b {
                edge_of_new.insert((a.min(b), a.max(b)));
            }
        }
    }
    for (a, b) in &edge_of_new {
        let (mut has_old, mut has_new) = (false, false);
        if let Some(e) = adj.edge(*a, *b) {
            for f in &e.faces {
                if is_new(*f) {
                    has_new = true;
                } else {
                    has_old = true;
                }
            }
        }
        if has_new && (has_old || p.mark_sharp) {
            mesh.set_edge_flag(*a, *b, EdgeFlags::SHARP, true);
        }
        if has_new && p.mark_seam {
            mesh.set_edge_flag(*a, *b, EdgeFlags::SEAM, true);
        }
    }
    Selection { mode: SelMode::Face, ids: (made_start..mesh.faces.len() as u32).collect() }
}

struct VC {
    v: u32,
    nbrs: Vec<u32>,
    ask: f64,
    cap: f64,
}

/// Cuts every selected corner off at its cut distance with profiled rings:
///
/// the cut polygon plus a quad strip per incident face. One ring per
/// segment; the profile eases ring spacing from the corner to the cut.
fn build_verts(
    mesh: &EditMesh,
    adj: &Adjacency,
    vs: &[VC],
    p: &Params,
    scale: f64,
    segments: usize,
    profile: f64,
) -> Result<Option<Commit>, crate::editmesh::EditError> {
    let s = segments;
    let selset: BTreeSet<u32> = vs.iter().map(|c| c.v).collect();
    let mut frac: BTreeMap<(u32, u32, u32), f64> = BTreeMap::new();
    let mut rpos: BTreeMap<(u32, u32, u32), DVec3> = BTreeMap::new();
    for c in vs {
        let d = c.ask * scale;
        for w in &c.nbrs {
            let len = (mesh.positions[c.v as usize] - mesh.positions[*w as usize]).length();
            if d.partial_cmp(&len) != Some(std::cmp::Ordering::Less) {
                return Ok(None);
            }
            let dir = (mesh.positions[*w as usize] - mesh.positions[c.v as usize]) / len;
            for k in 1..=s as u32 {
                let g = ease(profile, k as f64 / s as f64) * d / len;
                frac.insert((c.v, *w, k), g);
                rpos.insert((c.v, *w, k), mesh.positions[c.v as usize] + dir * (g * len));
            }
        }
    }
    let mut fresh_pos: Vec<DVec3> = Vec::new();
    let mut rid: BTreeMap<(u32, u32, u32), u32> = BTreeMap::new();
    let base = mesh.positions.len() as u32;
    for c in vs {
        for w in &c.nbrs {
            for k in 1..=s as u32 {
                rid.insert((c.v, *w, k), base + fresh_pos.len() as u32);
                fresh_pos.push(rpos[&(c.v, *w, k)]);
            }
        }
    }
    let face_uv = |f: u32, w: u32| -> [f32; 2] { corner_uv(mesh, f, w) };
    let mut touched: BTreeSet<u32> = BTreeSet::new();
    for c in vs {
        touched.extend(adj.faces_of(c.v).iter().copied());
    }
    let mut rewritten: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut rewritten_uv: BTreeMap<u32, Vec<[f32; 2]>> = BTreeMap::new();
    let mut rewritten_normal: BTreeMap<u32, Vec<Option<[f32; 3]>>> = BTreeMap::new();
    for f in &touched {
        let loop_v = mesh.face_verts(*f as usize);
        let face = mesh.faces[*f as usize];
        let orig: Vec<Corner> = mesh.corners[face.first as usize..face.first as usize + face.len as usize].to_vec();
        let orig_uv: Vec<[f32; 2]> = orig.iter().map(|c| c.uv).collect();
        let n = loop_v.len();
        let mut verts: Vec<u32> = Vec::with_capacity(n + 2);
        let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(n + 2);
        let mut norms: Vec<Option<[f32; 3]>> = Vec::with_capacity(n + 2);
        for (k, w) in loop_v.iter().enumerate() {
            if !selset.contains(w) {
                verts.push(*w);
                uvs.push(orig_uv[k]);
                norms.push(None);
                continue;
            }
            let (pv, nx) = (loop_v[(k + n - 1) % n], loop_v[(k + 1) % n]);
            let (ga, gb) = (frac[&(*w, pv, 1)], frac[&(*w, nx, 1)]);
            verts.push(rid[&(*w, pv, 1)]);
            verts.push(rid[&(*w, nx, 1)]);
            uvs.push(lerp_uv(orig_uv[k], face_uv(*f, pv), ga));
            uvs.push(lerp_uv(orig_uv[k], face_uv(*f, nx), gb));
            norms.push(None);
            norms.push(None);
        }
        let pts: Vec<DVec3> = verts
            .iter()
            .map(|v| if (*v as usize) < mesh.positions.len() { mesh.positions[*v as usize] } else { fresh_pos[(*v - base) as usize] })
            .collect();
        let orig = mesh.face_normal(*f as usize);
        if newell(&pts).length() * 0.5 < DEGENERATE_AREA || newell(&pts).normalize().dot(orig) <= 0.0 {
            return Ok(None);
        }
        rewritten.insert(*f, verts);
        rewritten_uv.insert(*f, uvs);
        rewritten_normal.insert(*f, norms);
    }
    let mut new_faces: Vec<NewFace> = Vec::new();
    let vmat = |faces: &[u32]| -> u16 {
        if p.material >= 0 {
            return p.material as u16;
        }
        faces.iter().min().map(|f| mesh.faces[*f as usize].material).unwrap_or(0)
    };
    for c in vs {
        for f in adj.faces_of(c.v) {
            let loop_v = mesh.face_verts(*f as usize);
            let k = loop_v.iter().position(|w| *w == c.v).unwrap_or(0);
            let n = loop_v.len();
            let (pv, nx) = (loop_v[(k + n - 1) % n], loop_v[(k + 1) % n]);
            let mat = if p.material >= 0 { p.material as u16 } else { mesh.faces[*f as usize].material };
            for kk in 1..s as u32 {
                let (a0, b0) = (rid[&(c.v, pv, kk)], rid[&(c.v, nx, kk)]);
                let (a1, b1) = (rid[&(c.v, pv, kk + 1)], rid[&(c.v, nx, kk + 1)]);
                let (ua0, ub0) = (frac[&(c.v, pv, kk)], frac[&(c.v, nx, kk)]);
                let (ua1, ub1) = (frac[&(c.v, pv, kk + 1)], frac[&(c.v, nx, kk + 1)]);
                let uvs = [
                    lerp_uv(face_uv(*f, c.v), face_uv(*f, pv), ua0),
                    lerp_uv(face_uv(*f, c.v), face_uv(*f, pv), ua1),
                    lerp_uv(face_uv(*f, c.v), face_uv(*f, nx), ub1),
                    lerp_uv(face_uv(*f, c.v), face_uv(*f, nx), ub0),
                ];
                let pos = |id: u32| fresh_pos[(id - base) as usize];
                let quad = [pos(a0), pos(a1), pos(b1), pos(b0)];
                if newell(&quad).length() * 0.5 < DEGENERATE_AREA {
                    return Ok(None);
                }
                new_faces.push(NewFace { verts: vec![a0, a1, b1, b0], uvs: uvs.to_vec(), material: mat });
            }
        }
    }
    for c in vs {
        let all: BTreeSet<(u32, u32)> = c.nbrs.iter().map(|w| (c.v.min(*w), c.v.max(*w))).collect();
        let others: BTreeSet<u32> = c.nbrs.iter().copied().collect();
        let links = links_at(mesh, adj, c.v, &all);
        for comp in order_components(&others, &links) {
            let mut poly: Vec<u32> = Vec::new();
            let mut puv: Vec<[f32; 2]> = Vec::new();
            let mut comp_faces: Vec<u32> = Vec::new();
            for w in &comp.chain {
                if let Some(e) = adj.edge(c.v, *w) {
                    comp_faces.extend(e.faces.iter().copied());
                    let g = frac[&(c.v, *w, s as u32)];
                    let mut acc = [0.0f32, 0.0f32];
                    for f in &e.faces {
                        let u = lerp_uv(face_uv(*f, c.v), face_uv(*f, *w), g);
                        acc[0] += u[0];
                        acc[1] += u[1];
                    }
                    let n = e.faces.len().max(1) as f32;
                    poly.push(rid[&(c.v, *w, s as u32)]);
                    puv.push([acc[0] / n, acc[1] / n]);
                }
            }
            if poly.len() < 3 {
                continue;
            }
            comp_faces.sort_unstable();
            comp_faces.dedup();
            let outward = outward_ref(mesh, &comp_faces);
            let pts: Vec<DVec3> = poly.iter().map(|v| fresh_pos[(*v - base) as usize]).collect();
            if newell(&pts).length() * 0.5 < DEGENERATE_AREA {
                return Ok(None);
            }
            let reversed = outward != DVec3::ZERO && newell(&pts).normalize_or_zero().dot(outward) < 0.0;
            let (poly, puv) = if reversed {
                (poly.iter().rev().copied().collect(), puv.iter().rev().copied().collect())
            } else {
                (poly, puv)
            };
            new_faces.push(NewFace { verts: poly, uvs: puv, material: vmat(&comp_faces) });
        }
    }
    let harden = if p.harden_normals { touched } else { BTreeSet::new() };
    Ok(Some(Commit { fresh_pos, rewritten, rewritten_uv, rewritten_normal, new_faces, harden }))
}

fn run_edges(mesh: &mut EditMesh, sel: &Selection, p: &Params, amount: f64, segments: usize, profile: f64) -> OpResult {
    let adj = Adjacency::build(mesh);
    let mut edges = collect_edges(mesh, &adj, sel)?;
    let mut valence: BTreeMap<u32, usize> = BTreeMap::new();
    let mut set: BTreeSet<(u32, u32)> = BTreeSet::new();
    for e in &edges {
        set.insert((e.a, e.b));
        *valence.entry(e.a).or_default() += 1;
        *valence.entry(e.b).or_default() += 1;
    }
    for e in &mut edges {
        e.off = convert(e, p.offset_type, amount);
        e.cap = edge_cap(mesh, e);
    }
    let ask_max = edges.iter().map(|e| e.off).fold(0.0f64, f64::max);
    let mut scale = 1.0;
    if p.clamp_overlap {
        let mut cap_min = f64::INFINITY;
        for e in &edges {
            cap_min = cap_min.min(cap_to_amount(e, p.offset_type, e.cap) / amount);
        }
        scale = cap_min.min(1.0);
        if !scale.is_finite() {
            scale = 1.0;
        }
    }
    let (ne, ns, nv) = (edges.len() as u64, segments as u64, valence.len() as u64);
    if mesh.positions.len() as u64 + mesh.corners.len() as u64 + 2 * ne * ns + nv * ns * ns + 2 * ne
        > MAX_VERTICES as u64
        || mesh.faces.len() as u64 + ne * ns + nv * ns * ns > MAX_FACES as u64
        || mesh.corners.len() as u64 + 4 * ne * ns + 3 * nv * ns * ns + 2 * ne > MAX_CORNERS as u64
    {
        return Err(bad("the bevel would pass the mesh budget"));
    }
    let n_exp = exponent(profile);
    let built = loop {
        match build_edges(mesh, &EdgeBuild { adj: &adj, edges: &edges, set: &set, valence: &valence, p, amount, scale, segments, n_exp })? {
            Some(c) => break c,
            None => {
                if !p.clamp_overlap {
                    return Err(bad("the bevel overlaps itself; lower the amount or enable clamp_overlap"));
                }
                scale *= 0.5;
                if scale * ask_max < 1e-9 {
                    return Err(bad("the bevel collapses; the selection is too tight to bevel"));
                }
            }
        }
    };
    let mut warnings = Vec::new();
    if scale < 1.0 {
        warnings.push(format!("amount clamped to {applied:.6} to avoid overlap", applied = amount * scale));
    }
    Ok((commit(mesh, built, p), warnings))
}

fn run_vertices(mesh: &mut EditMesh, sel: &Selection, p: &Params, amount: f64, segments: usize, profile: f64) -> OpResult {
    let adj = Adjacency::build(mesh);
    let verts = selected_verts(mesh, &adj, sel);
    if verts.is_empty() {
        return Err(bad("bevel names no vertices"));
    }
    let mut vs: Vec<VC> = Vec::new();
    for v in verts {
        let mut nbrs = Vec::new();
        let mut min_len = f64::INFINITY;
        for e in &adj.edges {
            if e.a == v || e.b == v {
                if e.faces.len() > 2 {
                    return Err(bad(format!(
                        "edge {} is non-manifold; merge by distance or fill it first",
                        adj.index_of(e.a, e.b)
                    )));
                }
                let w = if e.a == v { e.b } else { e.a };
                min_len = min_len.min((mesh.positions[v as usize] - mesh.positions[w as usize]).length());
                nbrs.push(w);
            }
        }
        if nbrs.is_empty() {
            return Err(bad(format!("vertex {v} has no edges")));
        }
        if min_len < 1e-12 {
            return Err(bad(format!("vertex {v} is degenerate")));
        }
        nbrs.sort_unstable();
        let ask = match p.offset_type {
            OffsetType::Offset => amount,
            OffsetType::Width => amount * 0.5,
            OffsetType::Depth => amount,
            OffsetType::Percent => amount / 100.0 * min_len,
        };
        vs.push(VC { v, nbrs, ask, cap: min_len * 0.5 });
    }
    vs.sort_by_key(|c| c.v);
    let mut scale = 1.0;
    if p.clamp_overlap {
        let mut m = f64::INFINITY;
        for c in &vs {
            m = m.min(c.cap / c.ask);
        }
        scale = m.min(1.0);
        if !scale.is_finite() {
            scale = 1.0;
        }
    }
    let ask_max = vs.iter().map(|c| c.ask).fold(0.0f64, f64::max);
    let total_nbrs: u64 = vs.iter().map(|c| c.nbrs.len() as u64).sum();
    let (nv, s) = (vs.len() as u64, segments as u64);
    if mesh.positions.len() as u64 + total_nbrs * s + nv > MAX_VERTICES as u64
        || mesh.faces.len() as u64 + total_nbrs * s + nv > MAX_FACES as u64
        || mesh.corners.len() as u64 + 4 * total_nbrs * s + total_nbrs > MAX_CORNERS as u64
    {
        return Err(bad("the bevel would pass the mesh budget"));
    }
    let built = loop {
        match build_verts(mesh, &adj, &vs, p, scale, segments, profile)? {
            Some(c) => break c,
            None => {
                if !p.clamp_overlap {
                    return Err(bad("the bevel overlaps itself; lower the amount or enable clamp_overlap"));
                }
                scale *= 0.5;
                if scale * ask_max < 1e-9 {
                    return Err(bad("the bevel collapses; the selection is too tight to bevel"));
                }
            }
        }
    };
    let mut warnings = Vec::new();
    if scale < 1.0 {
        warnings.push(format!("amount clamped to {applied:.6} to avoid overlap", applied = amount * scale));
    }
    Ok((commit(mesh, built, p), warnings))
}

#[cfg(test)]
mod tests {
    use super::super::test_meshes::*;
    use super::*;
    use crate::editmesh::validate::invariants;

    fn all_edges(mesh: &EditMesh) -> Selection {
        let adj = Adjacency::build(mesh);
        Selection { mode: SelMode::Edge, ids: (0..adj.edges.len() as u32).collect() }
    }

    fn one_edge(mesh: &EditMesh, a: u32, b: u32) -> Selection {
        let adj = Adjacency::build(mesh);
        Selection { mode: SelMode::Edge, ids: vec![adj.index_of(a, b)] }
    }

    fn counts(mesh: &EditMesh) -> (usize, usize, usize) {
        let adj = Adjacency::build(mesh);
        (mesh.positions.len(), adj.edges.len(), mesh.faces.len())
    }

    #[test]
    fn full_box_seg1_has_26_faces() {
        let mut m = quad_box();
        let sel = all_edges(&m);
        let (out, warnings) =
            run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 1, "profile": 0.5})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 26, "6 shrunk + 12 strips + 8 corner tris, got {:?}", counts(&m));
        assert_eq!(out.ids.len(), 20);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn full_box_seg3_has_98_faces() {
        let mut m = quad_box();
        let sel = all_edges(&m);
        let (_, warnings) =
            run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 3, "profile": 0.5})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 98, "6 + 12*3 strips + 8*7 corner faces, as Blender, got {:?}", counts(&m));
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn full_box_seg8_has_486_faces() {
        let mut m = quad_box();
        let sel = all_edges(&m);
        let (_, warnings) =
            run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 8, "profile": 0.5})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 486, "6 + 12*8 strips + 8*48 corner quads, as Blender, got {:?}", counts(&m));
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn rails_sit_at_amount_along_each_face() {
        let mut m = quad_box();
        let sel = all_edges(&m);
        run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 1})).unwrap();
        for (axis, sign) in [(0usize, 1.0), (0, -1.0), (1, 1.0), (1, -1.0), (2, 1.0), (2, -1.0)] {
            let flat: Vec<usize> = (0..m.faces.len())
                .filter(|fi| {
                    let n = m.face_normal(*fi);
                    let mut e = [0.0; 3];
                    e[axis] = sign;
                    (n - DVec3::from_array(e)).length() < 1e-9
                })
                .collect();
            assert_eq!(flat.len(), 1, "one shrunk face per side, axis {axis} sign {sign}");
            let fi = flat[0];
            assert_eq!(m.faces[fi].len, 4);
            for c in m.face_corners(fi) {
                let q = m.positions[c.vertex as usize];
                let a = if axis == 0 { [q.y, q.z] } else if axis == 1 { [q.x, q.z] } else { [q.x, q.y] };
                assert!((a[0].abs() - 28.0).abs() < 1e-6 && (a[1].abs() - 28.0).abs() < 1e-6, "rail at 32-4: {q:?}");
                assert!(((q.to_array()[axis] - sign * 32.0)).abs() < 1e-9);
            }
        }
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
    }

    #[test]
    fn shrink_matches_inset_on_uniform_faces() {
        let top_id = |m: &EditMesh| {
            (0..m.faces.len())
                .find(|fi| (m.face_normal(*fi) - DVec3::Z).length() < 1e-9)
                .expect("a top face") as u32
        };
        let mut b = quad_box();
        let bs = all_edges(&b);
        run(&mut b, &bs, &serde_json::json!({"amount": 4.0, "segments": 1})).unwrap();
        let t = top_id(&b);
        let mut bp: Vec<[f64; 3]> = b.face_corners(t as usize).iter().map(|c| b.positions[c.vertex as usize].to_array()).collect();
        bp.sort_by(|a, c| a.partial_cmp(c).unwrap());
        let mut m = quad_box();
        let t = top_id(&m);
        let sel = Selection { mode: SelMode::Face, ids: vec![t] };
        super::super::inset::run(&mut m, &sel, &serde_json::json!({"thickness": 4.0, "individual": true})).unwrap();
        let t = top_id(&m);
        let mut ip: Vec<[f64; 3]> = m.face_corners(t as usize).iter().map(|c| m.positions[c.vertex as usize].to_array()).collect();
        ip.sort_by(|a, c| a.partial_cmp(c).unwrap());
        assert_eq!(bp, ip, "a uniform bevel shrink runs the M5 offset helper");
    }

    #[test]
    fn two_edges_continue_straight_through_their_vertex() {
        let mut m = quad_box();
        let adj = Adjacency::build(&m);
        let sel = Selection { mode: SelMode::Edge, ids: vec![adj.index_of(5, 6), adj.index_of(6, 7)] };
        let (out, warnings) = run(&mut m, &sel, &serde_json::json!({"amount": 2.0, "segments": 1})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 10, "6 shrunk + 2 joined strips of 2, got {:?}", counts(&m));
        assert_eq!(out.ids.len(), 4);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn clamp_saves_a_thin_slab() {
        let mut m = quad_box();
        for p in &mut m.positions {
            p.z *= 4.0 / 64.0;
        }
        let sel = all_edges(&m);
        let (_, warnings) = run(&mut m, &sel, &serde_json::json!({"amount": 8.0, "segments": 1})).unwrap();
        assert!(warnings.iter().any(|w| w.contains("clamp")), "{warnings:?}");
        assert_eq!(m.faces.len(), 26);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn unclamped_overlap_is_refused() {
        let mut m = quad_box();
        for p in &mut m.positions {
            p.z *= 4.0 / 64.0;
        }
        let sel = all_edges(&m);
        let before = counts(&m);
        let err = run(&mut m, &sel, &serde_json::json!({"amount": 8.0, "clamp_overlap": false})).unwrap_err();
        assert!(err.to_string().contains("overlaps"), "{err}");
        assert_eq!(counts(&m), before, "a refused bevel touches nothing");
    }

    #[test]
    fn vertex_bevel_cuts_the_corner() {
        let mut m = quad_box();
        let sel = Selection { mode: SelMode::Vertex, ids: vec![6] };
        let (out, warnings) =
            run(&mut m, &sel, &serde_json::json!({"affect": "vertices", "amount": 8.0, "segments": 1})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 7, "6 reworked + 1 cutoff, got {:?}", counts(&m));
        assert_eq!(out.ids.len(), 1);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        assert_eq!(inv.euler, 2);
        let cap = out.ids[0] as usize;
        let n = m.face_normal(cap);
        let want = DVec3::new(1.0, 1.0, 1.0).normalize();
        assert!((n - want).length() < 1e-9, "the cutoff faces the corner: {n:?}");
        let mut pts: Vec<DVec3> = m.face_points(cap);
        pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)).then(a.z.total_cmp(&b.z)));
        let want_pts = [DVec3::new(24.0, 32.0, 32.0), DVec3::new(32.0, 24.0, 32.0), DVec3::new(32.0, 32.0, 24.0)];
        for (q, w) in pts.iter().zip(want_pts.iter()) {
            assert!((*q - *w).length() < 1e-6, "cut 8 along each edge: {q:?}");
        }
    }

    #[test]
    fn vertex_bevel_segments_add_rings() {
        let mut m = quad_box();
        let sel = Selection { mode: SelMode::Vertex, ids: vec![6] };
        run(&mut m, &sel, &serde_json::json!({"affect": "vertices", "amount": 8.0, "segments": 2})).unwrap();
        assert_eq!(m.faces.len(), 10, "6 + 3 quads + 1 cutoff, got {:?}", counts(&m));
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
    }

    #[test]
    fn single_edge_terminates_with_and_without_slide() {
        let mut slid = quad_box();
        let sel = one_edge(&slid, 5, 6);
        let (_, warnings) = run(&mut slid, &sel, &serde_json::json!({"amount": 4.0, "segments": 1})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(slid.faces.len(), 8, "6 + 2 end-join quads, got {:?}", counts(&slid));
        assert!(invariants(&slid).closed && invariants(&slid).wound);
        let uses_v5 = slid.corners.iter().any(|c| c.vertex == 5 && slid.faces.iter().any(|_| true));
        let _ = uses_v5;
        let mut pinned = quad_box();
        let sel = one_edge(&pinned, 5, 6);
        let _ = run(&mut pinned, &sel, &serde_json::json!({"amount": 4.0, "segments": 1, "loop_slide": false})).unwrap();
        assert_eq!(pinned.faces.len(), 6, "a fully pinched one-segment strip is a no-op, got {:?}", counts(&pinned));
        let inv = invariants(&pinned);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        assert_ne!(
            super::super::super::io::write(&slid),
            super::super::super::io::write(&pinned),
            "slide changes the termination"
        );
    }

    fn pyramid() -> EditMesh {
        use crate::editmesh::{Corner, Face, FaceFlags};
        let h = 32.0;
        let positions = vec![
            DVec3::new(-h, -h, -h),
            DVec3::new(h, -h, -h),
            DVec3::new(h, h, -h),
            DVec3::new(-h, h, -h),
            DVec3::new(0.0, 0.0, h),
        ];
        let loops: Vec<Vec<u32>> = vec![vec![3, 2, 1, 0], vec![0, 1, 4], vec![1, 2, 4], vec![2, 3, 4], vec![3, 0, 4]];
        let mut mesh = EditMesh {
            positions,
            faces: Vec::new(),
            corners: Vec::new(),
            edge_flags: Vec::new(),
            materials: vec!["m".to_string()],
            smooth_angle: None,
        };
        for lp in &loops {
            let first = mesh.corners.len() as u32;
            for v in lp {
                mesh.corners.push(Corner { vertex: *v, uv: [0.0, 0.0], normal: None });
            }
            mesh.faces.push(Face { first, len: lp.len() as u32, material: 0, flags: FaceFlags::empty() });
        }
        let adj = Adjacency::build(&mesh);
        let all = Selection { mode: SelMode::Face, ids: (0..5).collect() };
        super::super::normals::recalc(&mut mesh, &all, &serde_json::json!({})).unwrap();
        let _ = adj;
        assert!(invariants(&mesh).closed && invariants(&mesh).wound);
        mesh
    }

    #[test]
    fn valence_four_apex_gets_one_cutoff() {
        let mut m = pyramid();
        let adj = Adjacency::build(&m);
        let ids: Vec<u32> = [0u32, 1, 2, 3].iter().map(|w| adj.index_of(4, *w)).collect();
        let sel = Selection { mode: SelMode::Edge, ids };
        let (_, warnings) = run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 1})).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(m.faces.len(), 14, "5 + 4 joined strips of 2 + 1 cutoff, got {:?}", counts(&m));
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        assert_eq!(inv.euler, 2);
    }

    #[test]
    fn material_minus_one_inherits_the_adjacent_face() {
        let mut m = quad_box();
        m.materials.push("n".to_string());
        m.faces[0].material = 1;
        let sel = one_edge(&m, 5, 6);
        let (out, _) = run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 1})).unwrap();
        assert_eq!(out.ids.len(), 2);
        assert_eq!(m.faces[out.ids[0] as usize].material, 1, "the strip takes the smaller face id");
        let mut m = quad_box();
        let sel = one_edge(&m, 5, 6);
        let (out, _) = run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 1, "material": 0})).unwrap();
        assert_eq!(m.faces[out.ids[0] as usize].material, 0);
        let mut m = quad_box();
        let sel = one_edge(&m, 5, 6);
        let err = run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "material": 7})).unwrap_err();
        assert!(err.to_string().contains("material"), "{err}");
    }

    #[test]
    fn strip_boundary_is_sharp_and_flags_extend_it() {
        let mut m = quad_box();
        let sel = all_edges(&m);
        run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 3})).unwrap();
        let adj = Adjacency::build(&m);
        let mut boundary = 0usize;
        let mut inner = 0usize;
        for e in &adj.edges {
            if e.faces.len() != 2 {
                continue;
            }
            let (old, new) = (e.faces[0] < 6, e.faces[1] < 6);
            if old != new {
                boundary += 1;
                assert!(m.edge_flag(e.a, e.b).contains(EdgeFlags::SHARP), "strip boundary is sharp");
                assert!(!m.edge_flag(e.a, e.b).contains(EdgeFlags::SEAM));
            } else if e.faces[0] >= 6 {
                inner += 1;
                assert!(!m.edge_flag(e.a, e.b).contains(EdgeFlags::SHARP), "strip interior stays smooth");
            }
        }
        assert!(boundary > 0 && inner > 0, "both kinds exist: {boundary} boundary, {inner} inner");
        let mut m = quad_box();
        let sel = all_edges(&m);
        run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 1, "mark_sharp": true, "mark_seam": true})).unwrap();
        let adj = Adjacency::build(&m);
        for e in &adj.edges {
            if e.faces.len() == 2 && (e.faces[0] >= 6 || e.faces[1] >= 6) {
                assert!(m.edge_flag(e.a, e.b).contains(EdgeFlags::SHARP), "mark_sharp takes every new edge");
                assert!(m.edge_flag(e.a, e.b).contains(EdgeFlags::SEAM), "mark_seam takes every new edge");
            }
        }
    }

    #[test]
    fn harden_normals_freezes_flats_and_spares_new_faces() {
        let mut m = quad_box();
        let sel = all_edges(&m);
        run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 1, "harden_normals": true})).unwrap();
        for fi in 0..6 {
            let n = m.face_normal(fi);
            for c in m.face_corners(fi) {
                let got = c.normal.expect("a hardened corner carries its face normal");
                assert!(
                    (DVec3::new(got[0] as f64, got[1] as f64, got[2] as f64) - n).length() < 1e-6,
                    "hardened equals the face normal"
                );
            }
        }
        for fi in 6..m.faces.len() {
            assert!(m.face_corners(fi).iter().all(|c| c.normal.is_none()), "new faces shade smooth");
        }
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty());
    }

    #[test]
    fn untouched_faces_keep_their_normals() {
        let mut m = quad_box();
        for c in &mut m.corners {
            c.normal = Some([0.0, 0.0, 1.0]);
        }
        let sel = one_edge(&m, 5, 6);
        run(&mut m, &sel, &serde_json::json!({"amount": 4.0, "segments": 1})).unwrap();
        for fi in 0..m.faces.len() {
            if (fi as u32) < 6 && fi != 0 && fi != 2 {
                assert!(m.face_corners(fi).iter().all(|c| c.normal.is_some()), "face {fi} keeps its normals");
            }
        }
        for fi in [0usize, 2] {
            assert!(m.face_corners(fi).iter().all(|c| c.normal.is_none()), "rewritten face {fi} is cleared");
        }
    }

    #[test]
    fn offset_types_and_profiles_stay_closed() {
        for params in [
            serde_json::json!({"amount": 4.0, "offset_type": "width"}),
            serde_json::json!({"amount": 4.0, "offset_type": "depth"}),
            serde_json::json!({"amount": 5.0, "offset_type": "percent"}),
            serde_json::json!({"amount": 4.0, "segments": 2, "profile": 0.0}),
            serde_json::json!({"amount": 4.0, "segments": 2, "profile": 1.0}),
            serde_json::json!({"amount": 4.0, "segments": 2, "profile": 0.25}),
        ] {
            let mut m = quad_box();
            let sel = all_edges(&m);
            let (out, _) = run(&mut m, &sel, &params).unwrap_or_else(|e| panic!("{params}: {e}"));
            assert_eq!(out.mode, SelMode::Face);
            let inv = invariants(&m);
            assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{params}: {inv:?}");
            assert_eq!(inv.euler, 2);
        }
    }

    #[test]
    fn bevel_is_deterministic_and_dispatched() {
        let mut a = quad_box();
        let mut b = quad_box();
        let params = serde_json::json!({"amount": 4.0, "segments": 2, "profile": 0.5});
        let sa = all_edges(&a);
        run(&mut a, &sa, &params).unwrap();
        let sb = all_edges(&b);
        run(&mut b, &sb, &params).unwrap();
        assert_eq!(super::super::super::io::write(&a), super::super::super::io::write(&b));
        let mut m = quad_box();
        let sel = all_edges(&m);
        let (out, _) = super::super::dispatch("bevel", &mut m, &sel, &params).unwrap();
        assert_eq!(out.mode, SelMode::Face);
        assert_eq!(m.faces.len(), 54, "segments 2: 6 + 12*2 strips + 8*3 corner quads");
        let err = super::super::dispatch("bevel", &mut m, &sel, &serde_json::json!({"nope": 1})).unwrap_err();
        assert!(err.to_string().contains("bad params"));
        let err = run(&mut m, &sel, &serde_json::json!({"amount": 0.0})).unwrap_err();
        assert!(err.to_string().contains("zero"));
    }

    #[test]
    fn non_manifold_and_boundary_edges_are_refused_untouched() {
        let mut m = quad_box();
        let f = m.faces[0];
        let first = m.corners.len() as u32;
        m.corners.extend_from_slice(&m.corners[f.first as usize..f.first as usize + f.len as usize].to_vec());
        m.faces.push(crate::editmesh::Face { first, len: f.len, material: f.material, flags: f.flags });
        let before = counts(&m);
        let adj = Adjacency::build(&m);
        let sel = Selection { mode: SelMode::Edge, ids: vec![adj.index_of(4, 5), adj.index_of(5, 6)] };
        let err = run(&mut m, &sel, &serde_json::json!({"amount": 4.0})).unwrap_err();
        assert!(err.to_string().contains("non-manifold"), "{err}");
        assert_eq!(counts(&m), before);
        let mut open = strip_with_tri_cap();
        let adj = Adjacency::build(&open);
        let boundary = adj.edges.iter().position(|e| e.faces.len() == 1).unwrap() as u32;
        let sel = Selection { mode: SelMode::Edge, ids: vec![boundary] };
        let before = counts(&open);
        let err = run(&mut open, &sel, &serde_json::json!({"amount": 0.1})).unwrap_err();
        assert!(err.to_string().contains("boundary"), "{err}");
        assert_eq!(counts(&open), before);
    }
}





