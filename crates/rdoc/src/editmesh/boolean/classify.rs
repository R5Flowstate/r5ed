//! Inside/outside classification: fragments flood-fill into regions across
//! uncut edges, each region votes with ray casts against the other operand,
//! and coplanar overlaps keep the first operand's copy. Regions vote as one
//! so a single grazing ray cannot punch a hole.

use std::collections::BTreeMap;

use glam::DVec3;

use super::{bad, BoolOp, EditError, Frag, SoupTri};
use crate::editmesh::boolean::intersect::CoplanarPatch;

/// Fragment users of one welded edge, per operand side.
type EdgeUsers = BTreeMap<(bool, [u32; 2]), Vec<(usize, usize)>>;

pub struct Kept {
    pub p: [DVec3; 3],
    pub uv: [[f32; 2]; 3],
    pub normal: [Option<[f32; 3]>; 3],
    pub mat: usize,
    pub from_b: bool,
    pub reversed: bool,
}

const RAYS: [[f64; 3]; 5] = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.2672612419124244, 0.5345224838248488, 0.8017837257372732],
    [-0.5345224838248488, 0.2672612419124244, 0.8017837257372732],
];

fn ray_hit(o: DVec3, d: DVec3, t: &[DVec3; 3]) -> bool {
    let e1 = t[1] - t[0];
    let e2 = t[2] - t[0];
    let p = d.cross(e2);
    let det = e1.dot(p);
    let scale = e1.length() * e2.length();
    if det.abs() <= 1e-15 * scale {
        return false;
    }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return false;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return false;
    }
    e2.dot(q) * inv > 1e-12
}

fn ray_hits(o: DVec3, d: DVec3, other: &[SoupTri]) -> u32 {
    let mut hits = 0u32;
    for t in other {
        if ray_hit(o, d, &t.p) {
            hits += 1;
        }
    }
    hits
}

fn strictly_inside(c: DVec3, other: &[SoupTri]) -> bool {
    let mut inside = 0;
    for r in RAYS {
        if ray_hits(c, DVec3::from_array(r), other) % 2 == 1 {
            inside += 1;
        }
    }
    inside >= 3
}

fn inside_other(c: DVec3, n: DVec3, other: &[SoupTri], eps: f64) -> bool {
    strictly_inside(c + n * eps, other)
}

/// The coplanar direction of the overlap a centroid sits in, if any: true
/// when every covering patch pairs opposite normals (the solids on opposite
/// sides of the plane, a shared interface), false when some patch pairs
/// equal normals (both solids on the inner side, outer surface for both).
fn coplanar_dir(patches: &[CoplanarPatch], c: DVec3, scale: f64) -> Option<bool> {
    let mut found = false;
    let mut opposite = true;
    for patch in patches {
        let dist = (c - patch.origin).dot(patch.normal);
        if dist.abs() > 1e-9 * scale {
            continue;
        }
        let a = c.to_array();
        let r = [a[patch.ax.0], a[patch.ax.1]];
        if point_in_poly(&patch.poly, r) {
            found = true;
            opposite &= patch.opposite;
        }
    }
    found.then_some(opposite)
}

fn point_in_poly(poly: &[[f64; 2]], r: [f64; 2]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a[1] > r[1]) != (b[1] > r[1]) {
            let x = a[0] + (r[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]);
            if r[0] < x || (r[0] - x).abs() <= 1e-12 * (1.0 + x.abs()) {
                inside = !inside;
            }
        }
    }
    inside
}

pub fn label(
    soup: &[SoupTri],
    patches: &[CoplanarPatch],
    frags: &[Frag],
    op: BoolOp,
) -> Result<Vec<Kept>, EditError> {
    let tris: Vec<[DVec3; 3]> = frags.iter().map(|f| f.p).collect();
    let (_, wid) = super::weld_positions(&tris);
    let mut parent: Vec<u32> = (0..frags.len() as u32).collect();
    let find = |mut x: u32, parent: &mut [u32]| -> u32 {
        while parent[x as usize] != x {
            let n = parent[x as usize];
            parent[x as usize] = parent[n as usize];
            x = parent[n as usize];
        }
        x
    };
    let mut edge_map: EdgeUsers = BTreeMap::new();
    for (fi, f) in frags.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (wid[fi][k], wid[fi][(k + 1) % 3]);
            if a == b {
                continue;
            }
            edge_map.entry((f.from_b, [a.min(b), a.max(b)])).or_default().push((fi, k));
        }
    }
    for list in edge_map.values() {
        for i in 0..list.len() {
            for j in i + 1..list.len() {
                let (fi, ei) = list[i];
                let (fj, ej) = list[j];
                if frags[fi].sliver || frags[fj].sliver {
                    continue;
                }
                if frags[fi].cut[ei] || frags[fj].cut[ej] {
                    continue;
                }
                let (ri, rj) = (find(fi as u32, &mut parent), find(fj as u32, &mut parent));
                if ri != rj {
                    parent[ri.max(rj) as usize] = ri.min(rj);
                }
            }
        }
    }
    let mut regions: BTreeMap<(bool, u32), Vec<usize>> = BTreeMap::new();
    for (fi, f) in frags.iter().enumerate() {
        regions.entry((f.from_b, find(fi as u32, &mut parent))).or_default().push(fi);
    }
    let (mut soup_a, mut soup_b) = (Vec::new(), Vec::new());
    for t in soup {
        if t.from_b {
            soup_b.push(t.clone());
        } else {
            soup_a.push(t.clone());
        }
    }
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for t in soup {
        for p in &t.p {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
    }
    let scale = 1.0 + (hi - lo).length();
    let eps = 1e-7 * scale;
    let estimate: u64 =
        regions.values().map(|m| (m.len() as u64) * 5 * soup.len().max(1) as u64).sum();
    if estimate > 64_000_000 {
        return Err(bad("the boolean classification passes the work budget; simplify the operands"));
    }
    let mut verdict: BTreeMap<(bool, u32), bool> = BTreeMap::new();
    for ((from_b, root), members) in &regions {
        if members.len() == 1 && frags[members[0]].sliver {
            continue;
        }
        let other = if *from_b { &soup_a } else { &soup_b };
        let (mut inside, mut outside) = (0u32, 0u32);
        for fi in members {
            let f = &frags[*fi];
            let c = (f.p[0] + f.p[1] + f.p[2]) / 3.0;
            if coplanar_dir(patches, c, scale).is_some() {
                continue;
            }
            let n = (f.p[1] - f.p[0]).cross(f.p[2] - f.p[0]).normalize_or_zero();
            if inside_other(c, n, other, eps) {
                inside += 1;
            } else {
                outside += 1;
            }
        }
        verdict.insert((*from_b, *root), inside > outside);
    }
    let keep_of = |inside: bool, from_b: bool| -> (bool, bool) {
        if from_b {
            let keep = (inside && op != BoolOp::Union) || (!inside && op == BoolOp::Union);
            (keep, keep && op == BoolOp::Difference)
        } else {
            let keep = (!inside && op != BoolOp::Intersect) || (inside && op == BoolOp::Intersect);
            (keep, false)
        }
    };
    let mut out = Vec::new();
    for (fi, f) in frags.iter().enumerate() {
        let c = (f.p[0] + f.p[1] + f.p[2]) / 3.0;
        if let Some(opposite) = coplanar_dir(patches, c, scale) {
            if f.from_b {
                continue;
            }
            let n = (f.p[1] - f.p[0]).cross(f.p[2] - f.p[0]).normalize_or_zero();
            let inner = strictly_inside(c - n * eps, &soup_b);
            let keep = match op {
                BoolOp::Difference => !inner,
                BoolOp::Union => inner && !strictly_inside(c + n * eps, &soup_b),
                BoolOp::Intersect => !opposite || inner,
            };
            if keep {
                out.push(Kept {
                    p: f.p,
                    uv: f.uv,
                    normal: f.normal,
                    mat: f.mat,
                    from_b: f.from_b,
                    reversed: false,
                });
            }
            continue;
        }
        if f.sliver {
            let mut votes = [0u32, 0u32];
            for k in 0..3 {
                let (a, b) = (wid[fi][k], wid[fi][(k + 1) % 3]);
                if a == b {
                    continue;
                }
                if let Some(list) = edge_map.get(&(f.from_b, [a.min(b), a.max(b)])) {
                    for (other, _) in list {
                        if *other == fi || frags[*other].sliver {
                            continue;
                        }
                        let root = find(*other as u32, &mut parent);
                        match verdict.get(&(f.from_b, root)) {
                            Some(true) => votes[1] += 1,
                            _ => votes[0] += 1,
                        }
                    }
                }
            }
            let inside = if votes[0] + votes[1] > 0 {
                votes[1] > votes[0]
            } else {
                let n = (f.p[1] - f.p[0]).cross(f.p[2] - f.p[0]).normalize_or_zero();
                let other = if f.from_b { &soup_a } else { &soup_b };
                inside_other(c, n, other, eps)
            };
            let (keep, reversed) = keep_of(inside, f.from_b);
            if keep {
                out.push(Kept { p: f.p, uv: f.uv, normal: f.normal, mat: f.mat, from_b: f.from_b, reversed });
            }
            continue;
        }
        let inside = verdict[&(f.from_b, find(fi as u32, &mut parent))];
        let (keep, reversed) = keep_of(inside, f.from_b);
        if keep {
            out.push(Kept { p: f.p, uv: f.uv, normal: f.normal, mat: f.mat, from_b: f.from_b, reversed });
        }
    }
    Ok(out)
}
