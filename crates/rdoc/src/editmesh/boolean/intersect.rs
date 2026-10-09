//! Triangle-triangle arrangement: exact intersection segments through
//! `robust` orientation predicates, one global Steiner table shared by both
//! sides so the weld closes, and per-face constrained re-triangulation
//! through `spade`. Constraint endpoints on shared edges propagate to the
//! edge neighbours, so no T-junction survives the split.

use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;
use spade::{handles::FixedVertexHandle, ConstrainedDelaunayTriangulation, Point2, Triangulation};

use super::{bad, EditError, Frag, SoupTri};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pt {
    Corner(u8),
    Steiner(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct Seg(pub Pt, pub Pt);

pub struct SteinerRec {
    pub p: DVec3,
    pub edges: Vec<(usize, [u32; 2])>,
}

pub struct CoplanarPatch {
    pub origin: DVec3,
    pub normal: DVec3,
    pub ax: (usize, usize),
    pub poly: Vec<[f64; 2]>,
    /// True when the pair's normals oppose: the solids on opposite sides of
    /// the plane, so the overlap is a shared interface rather than surface.
    pub opposite: bool,
}

pub struct Cuts {
    pub points: Vec<SteinerRec>,
    pub segs: Vec<Vec<Seg>>,
    pub extra: Vec<Vec<u32>>,
    pub patches: Vec<CoplanarPatch>,
}

fn o3d(a: DVec3, b: DVec3, c: DVec3, d: DVec3) -> f64 {
    robust::orient3d(
        robust::Coord3D { x: a.x, y: a.y, z: a.z },
        robust::Coord3D { x: b.x, y: b.y, z: b.z },
        robust::Coord3D { x: c.x, y: c.y, z: c.z },
        robust::Coord3D { x: d.x, y: d.y, z: d.z },
    )
}

fn o2d(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    robust::orient2d(
        robust::Coord { x: a[0], y: a[1] },
        robust::Coord { x: b[0], y: b[1] },
        robust::Coord { x: c[0], y: c[1] },
    )
}

fn frame_of(n: DVec3) -> (usize, usize) {
    let a = n.abs();
    if a.x >= a.y && a.x >= a.z {
        (1, 2)
    } else if a.y >= a.z {
        (0, 2)
    } else {
        (0, 1)
    }
}

fn proj(p: DVec3, ax: (usize, usize)) -> [f64; 2] {
    let a = p.to_array();
    [a[ax.0], a[ax.1]]
}

fn tri_normal(t: &SoupTri) -> DVec3 {
    (t.p[1] - t.p[0]).cross(t.p[2] - t.p[0]).normalize_or_zero()
}

fn in_tri(poly: &[[f64; 2]; 3], p: [f64; 2]) -> bool {
    let s0 = o2d(poly[0], poly[1], p);
    let s1 = o2d(poly[1], poly[2], p);
    let s2 = o2d(poly[2], poly[0], p);
    (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0) || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0)
}

fn on_edge_exact(poly: &[[f64; 2]; 3], p: [f64; 2]) -> Option<(usize, usize)> {
    for k in 0..3 {
        if o2d(poly[k], poly[(k + 1) % 3], p) == 0.0 {
            let a = poly[k];
            let b = poly[(k + 1) % 3];
            let dot = (p[0] - a[0]) * (p[0] - b[0]) + (p[1] - a[1]) * (p[1] - b[1]);
            if dot <= 0.0 {
                return Some((k, (k + 1) % 3));
            }
        }
    }
    None
}

struct Builder {
    cuts: Cuts,
    by_bits: BTreeMap<[u64; 3], u32>,
    soup_b: Vec<bool>,
    edge_map: BTreeMap<(bool, [u32; 2]), Vec<usize>>,
}

impl Builder {
    fn new(soup: &[SoupTri]) -> Self {
        let mut edge_map: BTreeMap<(bool, [u32; 2]), Vec<usize>> = BTreeMap::new();
        for (i, t) in soup.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (t.vid[k], t.vid[(k + 1) % 3]);
                if a == b {
                    continue;
                }
                edge_map.entry((t.from_b, [a.min(b), a.max(b)])).or_default().push(i);
            }
        }
        let n = soup.len();
        Self {
            cuts: Cuts { points: Vec::new(), segs: vec![Vec::new(); n], extra: vec![Vec::new(); n], patches: Vec::new() },
            by_bits: BTreeMap::new(),
            soup_b: soup.iter().map(|t| t.from_b).collect(),
            edge_map,
        }
    }

    fn steiner(&mut self, p: DVec3) -> u32 {
        let key = [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        if let Some(i) = self.by_bits.get(&key) {
            return *i;
        }
        let i = self.cuts.points.len() as u32;
        self.cuts.points.push(SteinerRec { p, edges: Vec::new() });
        self.by_bits.insert(key, i);
        i
    }

    fn record(&mut self, s: u32, tri: usize, edge: [u32; 2]) {
        let rec = &mut self.cuts.points[s as usize];
        if !rec.edges.contains(&(tri, edge)) {
            rec.edges.push((tri, edge));
        }
    }

    fn pt_for(&mut self, _tri: usize, t: &SoupTri, p: DVec3) -> Pt {
        for k in 0..3 {
            if t.p[k] == p {
                return Pt::Corner(k as u8);
            }
        }
        Pt::Steiner(self.steiner(p))
    }

    fn seg(&mut self, ai: usize, bi: usize, soup: &[SoupTri], p: DVec3, q: DVec3) {
        let a = soup[ai].clone();
        let b = soup[bi].clone();
        let pa = self.pt_for(ai, &a, p);
        let qa = self.pt_for(ai, &a, q);
        let pb = self.pt_for(bi, &b, p);
        let qb = self.pt_for(bi, &b, q);
        if pa != qa {
            self.cuts.segs[ai].push(Seg(pa, qa));
        }
        if pb != qb {
            self.cuts.segs[bi].push(Seg(pb, qb));
        }
    }

    fn finish(mut self) -> Cuts {
        for s in 0..self.cuts.points.len() {
            let edges = self.cuts.points[s].edges.clone();
            for (tri, edge) in edges {
                let key = (self.soup_b[tri], edge);
                for sib in self.edge_map.get(&key).cloned().unwrap_or_default() {
                    if sib == tri || self.cuts.extra[sib].contains(&(s as u32)) {
                        continue;
                    }
                    self.cuts.extra[sib].push(s as u32);
                }
            }
        }
        self.cuts
    }
}

/// One candidate intersection point with the exact edge records that carry
/// it to the neighbouring triangles.
struct Cand {
    p: DVec3,
    a_on: Vec<[u32; 2]>,
    b_on: Vec<[u32; 2]>,
}

fn edge_key(t: &SoupTri, i: usize, j: usize) -> [u32; 2] {
    [t.vid[i].min(t.vid[j]), t.vid[i].max(t.vid[j])]
}

fn pair_candidates(a: &SoupTri, b: &SoupTri, sa: &[f64; 3], sb: &[f64; 3]) -> Vec<Cand> {
    let mut out: Vec<Cand> = Vec::new();
    let mut push = |p: DVec3, a_on: Vec<[u32; 2]>, b_on: Vec<[u32; 2]>| {
        for c in &mut out {
            if c.p == p {
                for e in a_on {
                    if !c.a_on.contains(&e) {
                        c.a_on.push(e);
                    }
                }
                for e in b_on {
                    if !c.b_on.contains(&e) {
                        c.b_on.push(e);
                    }
                }
                return;
            }
        }
        out.push(Cand { p, a_on, b_on });
    };
    let ax = frame_of(tri_normal(a));
    let bx = frame_of(tri_normal(b));
    let pa = [proj(a.p[0], ax), proj(a.p[1], ax), proj(a.p[2], ax)];
    let pb = [proj(b.p[0], bx), proj(b.p[1], bx), proj(b.p[2], bx)];
    for i in 0..3 {
        if sa[i] == 0.0 && in_tri(&pb, proj(a.p[i], bx)) {
            let mut b_on = Vec::new();
            if let Some((x, y)) = on_edge_exact(&pb, proj(a.p[i], bx)) {
                b_on.push(edge_key(b, x, y));
            }
            push(a.p[i], Vec::new(), b_on);
        }
        if sb[i] == 0.0 && in_tri(&pa, proj(b.p[i], ax)) {
            let mut a_on = Vec::new();
            if let Some((x, y)) = on_edge_exact(&pa, proj(b.p[i], ax)) {
                a_on.push(edge_key(a, x, y));
            }
            push(b.p[i], a_on, Vec::new());
        }
    }
    for k in 0..3 {
        let (i, j) = (k, (k + 1) % 3);
        if sa[i] * sa[j] < 0.0 {
            let t = sa[i] / (sa[i] - sa[j]);
            let x = a.p[i].lerp(a.p[j], t);
            if in_tri(&pb, proj(x, bx)) {
                let mut b_on = Vec::new();
                if let Some((x, y)) = on_edge_exact(&pb, proj(x, bx)) {
                    b_on.push(edge_key(b, x, y));
                }
                push(x, vec![edge_key(a, i, j)], b_on);
            }
        } else if sa[i] == 0.0 && sa[j] == 0.0 {
            for (p, q, e) in clip_edge_to_tri(a.p[i], a.p[j], b, bx) {
                push(p, vec![edge_key(a, i, j)], e.0);
                push(q, vec![edge_key(a, i, j)], e.1);
            }
        }
        if sb[i] * sb[j] < 0.0 {
            let t = sb[i] / (sb[i] - sb[j]);
            let x = b.p[i].lerp(b.p[j], t);
            if in_tri(&pa, proj(x, ax)) {
                let mut a_on = Vec::new();
                if let Some((x, y)) = on_edge_exact(&pa, proj(x, ax)) {
                    a_on.push(edge_key(a, x, y));
                }
                push(x, a_on, vec![edge_key(b, i, j)]);
            }
        } else if sb[i] == 0.0 && sb[j] == 0.0 {
            for (p, q, e) in clip_edge_to_tri(b.p[i], b.p[j], a, ax) {
                push(p, e.0, vec![edge_key(b, i, j)]);
                push(q, e.1, vec![edge_key(b, i, j)]);
            }
        }
    }
    out
}

/// One clipped piece of a coplanar edge: its endpoints plus the exact `t`
/// edges each endpoint rests on.
type ClippedPiece = (DVec3, DVec3, (Vec<[u32; 2]>, Vec<[u32; 2]>));

/// Clips segment (p, q) to triangle `t` in `t`'s frame. Each result carries
/// the exact `t` edges its endpoints rest on.
fn clip_edge_to_tri(p: DVec3, q: DVec3, t: &SoupTri, tx: (usize, usize)) -> Vec<ClippedPiece> {
    let poly = [proj(t.p[0], tx), proj(t.p[1], tx), proj(t.p[2], tx)];
    let mut pp = proj(p, tx);
    let mut qq = proj(q, tx);
    let mut ccw = [poly[0], poly[1], poly[2]];
    if o2d(ccw[0], ccw[1], ccw[2]) < 0.0 {
        ccw.swap(1, 2);
    }
    for k in 0..3 {
        let (e0, e1) = (ccw[k], ccw[(k + 1) % 3]);
        let dp = o2d(e0, e1, pp);
        let dq = o2d(e0, e1, qq);
        if dp < 0.0 && dq < 0.0 {
            return Vec::new();
        }
        if dp < 0.0 || dq < 0.0 {
            let t = dp / (dp - dq);
            let hit = [pp[0] + (qq[0] - pp[0]) * t, pp[1] + (qq[1] - pp[1]) * t];
            if dp < 0.0 {
                pp = hit;
            } else {
                qq = hit;
            }
        }
    }
    let p0 = proj(p, tx);
    let q0 = proj(q, tx);
    let span = [q0[0] - p0[0], q0[1] - p0[1]];
    let len2 = span[0] * span[0] + span[1] * span[1];
    if len2 == 0.0 {
        return Vec::new();
    }
    let param = |r: [f64; 2]| (((r[0] - p0[0]) * span[0] + (r[1] - p0[1]) * span[1]) / len2).clamp(0.0, 1.0);
    let tp = param(pp);
    let tq = param(qq);
    if (tp - tq).abs() < 1e-12 {
        return Vec::new();
    }
    let lift = |t: f64| p.lerp(q, t);
    let edge_of = |r: [f64; 2]| {
        let mut out = Vec::new();
        if let Some((x, y)) = on_edge_exact(&poly, r) {
            out.push(edge_key(t, x, y));
        }
        out
    };
    vec![(lift(tp), lift(tq), (edge_of(pp), edge_of(qq)))]
}

#[derive(Clone)]
struct SplitPt {
    pos2: [f64; 2],
    pos3: DVec3,
    uv: [f32; 2],
    corner: Option<u8>,
}

fn dist_to_seg(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    if len2 == 0.0 {
        return dist2(p, a).sqrt();
    }
    let t = (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0);
    dist2(p, [a[0] + ab[0] * t, a[1] + ab[1] * t]).sqrt()
}

fn param_along(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    if len2 == 0.0 {
        return 0.0;
    }
    ((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2
}

/// Merges Steiner points closer than rounding noise so collinear evidence
/// from different triangle pairs shares vertices exactly. Corners never
/// move; only Steiner points snap, to the first in sorted order.
fn snap_near(
    pts: &mut Vec<SplitPt>,
    seen: &mut BTreeMap<[u64; 2], usize>,
    mut segs: Vec<(usize, usize)>,
    tol: f64,
) -> Vec<(usize, usize)> {
    let snap_tol = tol * 1e-3;
    let mut order: Vec<usize> = (3..pts.len()).collect();
    order.sort_by_key(|i| [pts[*i].pos2[0].to_bits(), pts[*i].pos2[1].to_bits()]);
    let mut target_of: BTreeMap<usize, usize> = BTreeMap::new();
    let mut kept: Vec<usize> = Vec::new();
    for i in order {
        let mut found = None;
        for k in &kept {
            if dist2(pts[i].pos2, pts[*k].pos2) <= snap_tol * snap_tol {
                found = Some(*k);
                break;
            }
        }
        match found {
            Some(k) => {
                target_of.insert(i, k);
            }
            None => kept.push(i),
        }
    }
    if target_of.is_empty() {
        return segs;
    }
    let mut new_of = vec![0usize; pts.len()];
    let mut fresh: Vec<SplitPt> = Vec::with_capacity(3 + kept.len());
    for (i, p) in pts.iter().enumerate().take(3) {
        new_of[i] = fresh.len();
        fresh.push(p.clone());
    }
    let mut kept_asc = kept.clone();
    kept_asc.sort_unstable();
    for old in kept_asc {
        new_of[old] = fresh.len();
        fresh.push(pts[old].clone());
    }
    for (old, keep) in target_of.iter() {
        new_of[*old] = new_of[*keep];
    }
    *pts = fresh;
    seen.clear();
    for (i, p) in pts.iter().enumerate() {
        seen.insert([p.pos2[0].to_bits(), p.pos2[1].to_bits()], i);
    }
    for (a, b) in segs.iter_mut() {
        *a = new_of[*a];
        *b = new_of[*b];
    }
    segs.retain(|(a, b)| *a != *b);
    segs.sort_unstable();
    segs.dedup();
    segs
}

/// Splits every cut segment at each interior crossing so the constraints
/// meet only at shared endpoints, which is what the triangulation demands.
fn complete_arrangement(
    t: &SoupTri,
    corners: &[[f64; 2]; 3],
    pts: &mut Vec<SplitPt>,
    seen: &mut BTreeMap<[u64; 2], usize>,
    segs: Vec<(usize, usize)>,
) -> Vec<(usize, usize)> {
    if segs.len() < 2 {
        return segs;
    }
    let pos = |pts: &[SplitPt], i: usize| pts[i].pos2;
    let mut params: Vec<Vec<f64>> = segs.iter().map(|_| vec![0.0, 1.0]).collect();
    for i in 0..segs.len() {
        for j in i + 1..segs.len() {
            let (a, b) = (pos(pts, segs[i].0), pos(pts, segs[i].1));
            let (c, d) = (pos(pts, segs[j].0), pos(pts, segs[j].1));
            let (d1, d2) = (o2d(a, b, c), o2d(a, b, d));
            let (d3, d4) = (o2d(c, d, a), o2d(c, d, b));
            if d1 == 0.0 && d2 == 0.0 && d3 == 0.0 && d4 == 0.0 {
                for r in [c, d] {
                    let t = param_along(a, b, r);
                    if t > 1e-12 && t < 1.0 - 1e-12 {
                        params[i].push(t);
                    }
                }
                for r in [a, b] {
                    let t = param_along(c, d, r);
                    if t > 1e-12 && t < 1.0 - 1e-12 {
                        params[j].push(t);
                    }
                }
            } else if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
                params[i].push(d1 / (d1 - d2));
                params[j].push(d3 / (d3 - d4));
            } else {
                for (r, host) in [(c, j), (d, j), (a, i), (b, i)] {
                    let line = if host == i { (a, b) } else { (c, d) };
                    if o2d(line.0, line.1, r) == 0.0 {
                        let t = param_along(line.0, line.1, r);
                        if t > 1e-9 && t < 1.0 - 1e-9 {
                            params[host].push(t);
                        }
                    }
                }
            }
        }
    }
    let mut sub: Vec<(usize, usize)> = Vec::new();
    let mut used: BTreeSet<([u64; 2], [u64; 2])> = BTreeSet::new();
    for (s, (a, b)) in segs.iter().enumerate() {
        let mut ts = params[s].clone();
        ts.sort_by(|x, y| x.total_cmp(y));
        let mut chain = vec![*a];
        for w in ts.windows(2) {
            if w[1] - w[0] < 1e-12 {
                continue;
            }
            let mid = (w[0] + w[1]) * 0.5;
            let endpoint = if w[0] <= 1e-12 {
                *a
            } else if w[1] >= 1.0 - 1e-12 {
                *b
            } else {
                let r = lerp2(pos(pts, *a), pos(pts, *b), mid);
                let key = [r[0].to_bits(), r[1].to_bits()];
                match seen.get(&key) {
                    Some(i) => *i,
                    None => {
                        let bary = barycentric(corners, r).unwrap_or([1.0 / 3.0; 3]);
                        let uv = [
                            (t.uv[0][0] as f64 * bary[0] + t.uv[1][0] as f64 * bary[1] + t.uv[2][0] as f64 * bary[2])
                                as f32,
                            (t.uv[0][1] as f64 * bary[0] + t.uv[1][1] as f64 * bary[1] + t.uv[2][1] as f64 * bary[2])
                                as f32,
                        ];
                        let i = pts.len();
                        pts.push(SplitPt { pos2: r, pos3: bary3(t, bary), uv, corner: None });
                        seen.insert(key, i);
                        i
                    }
                }
            };
            chain.push(endpoint);
        }
        chain.push(*b);
        for w in chain.windows(2) {
            if w[0] == w[1] {
                continue;
            }
            let (p, q) = (pts[w[0]].pos2, pts[w[1]].pos2);
            let key = if p < q {
                ([p[0].to_bits(), p[1].to_bits()], [q[0].to_bits(), q[1].to_bits()])
            } else {
                ([q[0].to_bits(), q[1].to_bits()], [p[0].to_bits(), p[1].to_bits()])
            };
            if used.insert(key) {
                sub.push((w[0], w[1]));
            }
        }
    }
    sub.retain(|(a, b)| !on_same_boundary_edge(pts, corners, *a, *b));
    sub
}

fn lerp2(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn bary3(t: &SoupTri, bary: [f64; 3]) -> DVec3 {
    t.p[0] * bary[0] + t.p[1] * bary[1] + t.p[2] * bary[2]
}

fn on_same_boundary_edge(pts: &[SplitPt], corners: &[[f64; 2]; 3], a: usize, b: usize) -> bool {
    let tol = 1e-9;
    for k in 0..3 {
        if dist_to_seg(pts[a].pos2, corners[k], corners[(k + 1) % 3]) <= tol
            && dist_to_seg(pts[b].pos2, corners[k], corners[(k + 1) % 3]) <= tol
        {
            return true;
        }
    }
    false
}

/// Re-triangulates every cut triangle through a constrained Delaunay
/// triangulation of its corners plus the cut points, in its own plane.
/// Uncut triangles pass through untouched with their attributes intact.
pub fn split_all(soup: &[SoupTri], cuts: &Cuts) -> Result<Vec<Frag>, EditError> {
    let mut frags = Vec::new();
    for (ti, t) in soup.iter().enumerate() {
        if cuts.segs[ti].is_empty() && cuts.extra[ti].is_empty() {
            frags.push(Frag {
                p: t.p,
                uv: t.uv,
                normal: t.normal,
                mat: t.mat,
                from_b: t.from_b,
                cut: [false; 3],
                sliver: false,
            });
            continue;
        }
        split_one(t, &cuts.segs[ti], &cuts.extra[ti], &cuts.points, &mut frags)?;
    }
    frags.sort_by(|a, b| {
        (a.from_b, centroid_key(a)).cmp(&(b.from_b, centroid_key(b)))
    });
    Ok(frags)
}

fn centroid_key(f: &Frag) -> [u64; 3] {
    let c = (f.p[0] + f.p[1] + f.p[2]) / 3.0;
    [c.x.to_bits(), c.y.to_bits(), c.z.to_bits()]
}

fn barycentric(corners: &[[f64; 2]; 3], p: [f64; 2]) -> Option<[f64; 3]> {
    let d = (corners[1][1] - corners[2][1]) * (corners[0][0] - corners[2][0])
        + (corners[2][0] - corners[1][0]) * (corners[0][1] - corners[2][1]);
    if d == 0.0 {
        return None;
    }
    let l0 = ((corners[1][1] - corners[2][1]) * (p[0] - corners[2][0])
        + (corners[2][0] - corners[1][0]) * (p[1] - corners[2][1]))
        / d;
    let l1 = ((corners[2][1] - corners[0][1]) * (p[0] - corners[2][0])
        + (corners[0][0] - corners[2][0]) * (p[1] - corners[2][1]))
        / d;
    Some([l0, l1, 1.0 - l0 - l1])
}

fn split_one(
    t: &SoupTri,
    segs: &[Seg],
    extra: &[u32],
    points: &[SteinerRec],
    frags: &mut Vec<Frag>,
) -> Result<(), EditError> {
    let n = tri_normal(t);
    if n == DVec3::ZERO {
        return Err(bad("a boolean triangle collapsed"));
    }
    let ax = frame_of(n);
    let corners = [proj(t.p[0], ax), proj(t.p[1], ax), proj(t.p[2], ax)];
    let scale = 1.0 + (t.p[0] - t.p[1]).length().max((t.p[1] - t.p[2]).length());
    let tol = 1e-9 * scale;
    let mut pts: Vec<SplitPt> = (0..3)
        .map(|k| SplitPt { pos2: corners[k], pos3: t.p[k], uv: t.uv[k], corner: Some(k as u8) })
        .collect();
    let mut seen: BTreeMap<[u64; 2], usize> = BTreeMap::new();
    for (k, c) in corners.iter().enumerate() {
        seen.insert([c[0].to_bits(), c[1].to_bits()], k);
    }
    let resolve = |p3: DVec3, pts: &mut Vec<SplitPt>, seen: &mut BTreeMap<[u64; 2], usize>| -> usize {
        if let Some(k) = t.p.iter().position(|p| *p == p3) {
            return k;
        }
        let r = proj(p3, ax);
        if let Some(k) = corners.iter().position(|c| dist2(r, *c) <= tol * tol) {
            return k;
        }
        let key = [r[0].to_bits(), r[1].to_bits()];
        if let Some(i) = seen.get(&key) {
            return *i;
        }
        let bary = barycentric(&corners, r).unwrap_or([1.0 / 3.0; 3]);
        let clamped: [f64; 3] = {
            let mut c = bary.map(|v| if v < 0.0 && v > -1e-9 { 0.0 } else { v });
            let sum = c[0] + c[1] + c[2];
            if sum != 0.0 {
                c = [c[0] / sum, c[1] / sum, c[2] / sum];
            }
            c
        };
        let uv = [
            (t.uv[0][0] as f64 * clamped[0] + t.uv[1][0] as f64 * clamped[1] + t.uv[2][0] as f64 * clamped[2]) as f32,
            (t.uv[0][1] as f64 * clamped[0] + t.uv[1][1] as f64 * clamped[1] + t.uv[2][1] as f64 * clamped[2]) as f32,
        ];
        let i = pts.len();
        pts.push(SplitPt { pos2: r, pos3: p3, uv, corner: None });
        seen.insert(key, i);
        i
    };
    let mut seg_idx: Vec<(usize, usize)> = Vec::with_capacity(segs.len());
    for Seg(a, b) in segs {
        let ia = match a {
            Pt::Corner(k) => *k as usize,
            Pt::Steiner(s) => resolve(points[*s as usize].p, &mut pts, &mut seen),
        };
        let ib = match b {
            Pt::Corner(k) => *k as usize,
            Pt::Steiner(s) => resolve(points[*s as usize].p, &mut pts, &mut seen),
        };
        if ia != ib {
            seg_idx.push((ia, ib));
        }
    }
    for s in extra {
        resolve(points[*s as usize].p, &mut pts, &mut seen);
    }
    let seg_idx = snap_near(&mut pts, &mut seen, seg_idx, tol);
    let seg_idx = complete_arrangement(t, &corners, &mut pts, &mut seen, seg_idx);
    let mut order: Vec<usize> = (3..pts.len()).collect();
    order.sort_by_key(|i| [pts[*i].pos2[0].to_bits(), pts[*i].pos2[1].to_bits()]);
    let mut cdt = ConstrainedDelaunayTriangulation::<Point2<f64>>::new();
    let mut handles = vec![None; pts.len()];
    for (k, p) in pts.iter().enumerate().take(3) {
        handles[k] = cdt.insert(Point2::new(p.pos2[0], p.pos2[1])).ok();
    }
    for i in order {
        handles[i] = cdt.insert(Point2::new(pts[i].pos2[0], pts[i].pos2[1])).ok();
    }
    if handles.iter().any(|h| h.is_none()) {
        return Err(bad("the boolean triangulation refused a cut point"));
    }
    let edge_of_pt = |i: usize| -> Option<usize> {
        if pts[i].corner.is_some() {
            return None;
        }
        (0..3).find(|k| dist_to_seg(pts[i].pos2, corners[*k], corners[(*k + 1) % 3]) <= tol)
    };
    let emit = |cdt: &mut ConstrainedDelaunayTriangulation<Point2<f64>>,
                    handles: &[Option<FixedVertexHandle>],
                    pts: &[SplitPt],
                    cut_set: &mut BTreeSet<(usize, usize)>,
                    is_cut: bool,
                    a: usize,
                    b: usize| {
        if a == b {
            return;
        }
        let mut chain = vec![a];
        let mut mids: Vec<(f64, usize)> = Vec::new();
        for (i, p) in pts.iter().enumerate() {
            if i == a || i == b {
                continue;
            }
            if dist_to_seg(p.pos2, pts[a].pos2, pts[b].pos2) <= tol {
                let t = param_along(pts[a].pos2, pts[b].pos2, p.pos2);
                if t > 1e-9 && t < 1.0 - 1e-9 {
                    mids.push((t, i));
                }
            }
        }
        mids.sort_by(|x, y| x.0.total_cmp(&y.0));
        for (_, i) in mids {
            if chain[chain.len() - 1] != i {
                chain.push(i);
            }
        }
        if chain[chain.len() - 1] != b {
            chain.push(b);
        }
        for w in chain.windows(2) {
            if w[0] == w[1] {
                continue;
            }
            cdt.add_constraint(handles[w[0]].unwrap(), handles[w[1]].unwrap());
            if is_cut {
                cut_set.insert((w[0].min(w[1]), w[0].max(w[1])));
            }
        }
    };
    let mut cut_set: BTreeSet<(usize, usize)> = BTreeSet::new();
    for k in 0..3 {
        let mut chain = vec![k];
        for (i, _) in pts.iter().enumerate().skip(3) {
            if edge_of_pt(i) == Some(k) {
                chain.push(i);
            }
        }
        chain.sort_by(|a, b| {
            param_along(corners[k], corners[(k + 1) % 3], pts[*a].pos2)
                .total_cmp(&param_along(corners[k], corners[(k + 1) % 3], pts[*b].pos2))
        });
        chain.push((k + 1) % 3);
        for w in chain.windows(2) {
            emit(&mut cdt, &handles, &pts, &mut cut_set, false, w[0], w[1]);
        }
    }
    for (a, b) in seg_idx.clone() {
        emit(&mut cdt, &handles, &pts, &mut cut_set, true, a, b);
    }
    let mut by_pos: BTreeMap<[u64; 2], usize> = BTreeMap::new();
    for (i, p) in pts.iter().enumerate() {
        by_pos.insert([p.pos2[0].to_bits(), p.pos2[1].to_bits()], i);
    }
    let sign = o2d(corners[0], corners[1], corners[2]);
    for face in cdt.inner_faces() {
        let vs = face.vertices();
        let mut idx = [0usize; 3];
        let mut ok = true;
        for (k, v) in vs.iter().enumerate() {
            let pos = v.position();
            match by_pos.get(&[pos.x.to_bits(), pos.y.to_bits()]) {
                Some(i) => idx[k] = *i,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        if idx[0] == idx[1] || idx[1] == idx[2] || idx[2] == idx[0] {
            continue;
        }
        let tri2 = [pts[idx[0]].pos2, pts[idx[1]].pos2, pts[idx[2]].pos2];
        if o2d(tri2[0], tri2[1], tri2[2]) == 0.0 {
            continue;
        }
        let centre = [(tri2[0][0] + tri2[1][0] + tri2[2][0]) / 3.0, (tri2[0][1] + tri2[1][1] + tri2[2][1]) / 3.0];
        if !in_tri(&corners, centre) {
            continue;
        }
        let mut cut = [false; 3];
        for k in 0..3 {
            let (a, b) = (idx[k], idx[(k + 1) % 3]);
            cut[k] = cut_set.contains(&(a.min(b), a.max(b)));
        }
        let mut order = [0usize, 1, 2];
        let flipped = sign * o2d(tri2[0], tri2[1], tri2[2]) < 0.0;
        if flipped {
            order = [0, 2, 1];
        }
        let pick = |k: usize| pts[idx[order[k]]].clone();
        let (v0, v1, v2) = (pick(0), pick(1), pick(2));
        let has_steiner = [v0.corner, v1.corner, v2.corner].iter().any(|c| c.is_none());
        frags.push(Frag {
            p: [v0.pos3, v1.pos3, v2.pos3],
            uv: [v0.uv, v1.uv, v2.uv],
            normal: if has_steiner { [None; 3] } else { t.normal },
            mat: t.mat,
            from_b: t.from_b,
            cut: if flipped { [cut[2], cut[1], cut[0]] } else { cut },
            sliver: false,
        });
    }
    Ok(())
}

/// Axis-aligned boxes of every soup triangle, for the broadphase.
fn aabbs(soup: &[SoupTri]) -> Vec<([f64; 3], [f64; 3])> {
    soup.iter()
        .map(|t| {
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for p in &t.p {
                for k in 0..3 {
                    lo[k] = lo[k].min(p.to_array()[k]);
                    hi[k] = hi[k].max(p.to_array()[k]);
                }
            }
            (lo, hi)
        })
        .collect()
}

fn boxes_hit(a: &([f64; 3], [f64; 3]), b: &([f64; 3], [f64; 3])) -> bool {
    (0..3).all(|k| a.0[k] <= b.1[k] && b.0[k] <= a.1[k])
}

/// Every overlapping A/B triangle pair splits both triangles along the
/// intersection, so the two tessellations share every cut vertex.
pub fn arrange(soup: &[SoupTri]) -> Result<Cuts, EditError> {
    let mut order_a = Vec::new();
    let mut order_b = Vec::new();
    for (i, t) in soup.iter().enumerate() {
        if t.from_b {
            order_b.push(i);
        } else {
            order_a.push(i);
        }
    }
    let boxes = aabbs(soup);
    let mut builder = Builder::new(soup);
    for ai in &order_a {
        for bi in &order_b {
            if !boxes_hit(&boxes[*ai], &boxes[*bi]) {
                continue;
            }
            pair(&mut builder, soup, *ai, *bi)?;
        }
    }
    Ok(builder.finish())
}

fn pair(builder: &mut Builder, soup: &[SoupTri], ai: usize, bi: usize) -> Result<(), EditError> {
    let (a, b) = (soup[ai].clone(), soup[bi].clone());
    let mut sa = [0.0; 3];
    let mut sb = [0.0; 3];
    for i in 0..3 {
        sa[i] = o3d(b.p[0], b.p[1], b.p[2], a.p[i]);
        sb[i] = o3d(a.p[0], a.p[1], a.p[2], b.p[i]);
    }
    if sa == [0.0; 3] && sb == [0.0; 3] {
        return coplanar(builder, soup, ai, bi);
    }
    if sa.iter().all(|s| *s > 0.0) || sa.iter().all(|s| *s < 0.0) {
        return Ok(());
    }
    if sb.iter().all(|s| *s > 0.0) || sb.iter().all(|s| *s < 0.0) {
        return Ok(());
    }
    let cands = pair_candidates(&a, &b, &sa, &sb);
    if cands.len() < 2 {
        return Ok(());
    }
    let mut far = (0usize, 1usize, 0.0f64);
    for (i, c) in cands.iter().enumerate() {
        for (j, d) in cands.iter().enumerate().skip(i + 1) {
            let dist = (c.p - d.p).length_squared();
            if dist > far.2 {
                far = (i, j, dist);
            }
        }
    }
    if far.2 <= 1e-24 {
        return Ok(());
    }
    let (c0, c1) = (&cands[far.0], &cands[far.1]);
    let (s0, s1) = (builder.steiner(c0.p), builder.steiner(c1.p));
    for (tri, key) in c0.a_on.iter().map(|e| (ai, *e)).chain(c0.b_on.iter().map(|e| (bi, *e))) {
        builder.record(s0, tri, key);
    }
    for (tri, key) in c1.a_on.iter().map(|e| (ai, *e)).chain(c1.b_on.iter().map(|e| (bi, *e))) {
        builder.record(s1, tri, key);
    }
    builder.seg(ai, bi, soup, c0.p, c1.p);
    Ok(())
}

/// Two triangles on one plane: the overlap polygon cuts both, and the patch
/// is recorded so classification resolves the area by side, not by ray.
fn coplanar(builder: &mut Builder, soup: &[SoupTri], ai: usize, bi: usize) -> Result<(), EditError> {
    let (a, b) = (soup[ai].clone(), soup[bi].clone());
    let n = tri_normal(&a);
    if n == DVec3::ZERO {
        return Ok(());
    }
    let ax = frame_of(n);
    let mut pa = [proj(a.p[0], ax), proj(a.p[1], ax), proj(a.p[2], ax)];
    let mut pb = [proj(b.p[0], ax), proj(b.p[1], ax), proj(b.p[2], ax)];
    if o2d(pa[0], pa[1], pa[2]) < 0.0 {
        pa.swap(1, 2);
    }
    if o2d(pb[0], pb[1], pb[2]) < 0.0 {
        pb.swap(1, 2);
    }
    let mut poly = vec![pb[0], pb[1], pb[2]];
    for k in 0..3 {
        let (e0, e1) = (pa[k], pa[(k + 1) % 3]);
        let mut next = Vec::new();
        for i in 0..poly.len() {
            let (c, n) = (poly[i], poly[(i + 1) % poly.len()]);
            let (dc, dn) = (o2d(e0, e1, c), o2d(e0, e1, n));
            if dc >= 0.0 {
                next.push(c);
            }
            if (dc < 0.0 && dn > 0.0) || (dc > 0.0 && dn < 0.0) {
                let t = dc / (dc - dn);
                next.push([c[0] + (n[0] - c[0]) * t, c[1] + (n[1] - c[1]) * t]);
            }
        }
        poly = next;
        if poly.is_empty() {
            return Ok(());
        }
    }
    if poly.len() < 2 {
        return Ok(());
    }
    if poly.len() == 2 {
        if dist2(poly[0], poly[1]) <= 1e-24 {
            return Ok(());
        }
        let (p, q) = (lift_plane(poly[0], ax, &a), lift_plane(poly[1], ax, &a));
        edges_for(builder, &a, &b, ai, bi, p);
        edges_for(builder, &a, &b, ai, bi, q);
        builder.seg(ai, bi, soup, p, q);
        return Ok(());
    }
    let mut area = 0.0;
    for i in 0..poly.len() {
        let (c, n) = (poly[i], poly[(i + 1) % poly.len()]);
        area += c[0] * n[1] - n[0] * c[1];
    }
    if area.abs() <= 1e-18 {
        return Ok(());
    }
    builder.cuts.patches.push(CoplanarPatch { origin: a.p[0], normal: n, ax, poly: poly.clone(), opposite: n.dot(tri_normal(&b)) < 0.0 });
    for i in 0..poly.len() {
        let (c, n) = (poly[i], poly[(i + 1) % poly.len()]);
        if dist2(c, n) <= 1e-24 {
            continue;
        }
        let (p, q) = (lift_plane(c, ax, &a), lift_plane(n, ax, &a));
        edges_for(builder, &a, &b, ai, bi, p);
        edges_for(builder, &a, &b, ai, bi, q);
        builder.seg(ai, bi, soup, p, q);
    }
    Ok(())
}

fn dist2(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]) * (a[0] - b[0]) + (a[1] - b[1]) * (a[1] - b[1])
}

fn lift_plane(r: [f64; 2], ax: (usize, usize), t: &SoupTri) -> DVec3 {
    let n = tri_normal(t);
    let mut c = t.p[0].to_array();
    c[ax.0] = r[0];
    c[ax.1] = r[1];
    let drop = 3 - ax.0 - ax.1;
    let nd = n.to_array()[drop];
    if nd.abs() > 1e-300 {
        c[drop] -= (DVec3::from_array(c) - t.p[0]).dot(n) / nd;
    }
    DVec3::from_array(c)
}

/// Records the exact triangle edges a coplanar overlap vertex rests on, so
/// the neighbour across each edge splits its boundary at the same point.
fn edges_for(builder: &mut Builder, a: &SoupTri, b: &SoupTri, ai: usize, bi: usize, p: DVec3) {
    let s = builder.steiner(p);
    let n = tri_normal(a);
    if n == DVec3::ZERO {
        return;
    }
    let ax = frame_of(n);
    let r = proj(p, ax);
    let pa = [proj(a.p[0], ax), proj(a.p[1], ax), proj(a.p[2], ax)];
    let pb = [proj(b.p[0], ax), proj(b.p[1], ax), proj(b.p[2], ax)];
    for k in 0..3 {
        if o2d(pa[k], pa[(k + 1) % 3], r) == 0.0 {
            let e = edge_key(a, k, (k + 1) % 3);
            if edge_within(a, &pa, k, r) {
                builder.record(s, ai, e);
            }
        }
        if o2d(pb[k], pb[(k + 1) % 3], r) == 0.0 {
            let e = edge_key(b, k, (k + 1) % 3);
            if edge_within(b, &pb, k, r) {
                builder.record(s, bi, e);
            }
        }
    }
}

fn edge_within(_t: &SoupTri, poly: &[[f64; 2]; 3], k: usize, r: [f64; 2]) -> bool {
    let (a, b) = (poly[k], poly[(k + 1) % 3]);
    (r[0] - a[0]) * (r[0] - b[0]) + (r[1] - a[1]) * (r[1] - b[1]) <= 0.0
}
