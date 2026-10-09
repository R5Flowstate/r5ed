//! Evaluation: an `EditMesh` triangulated per material slot, with normals
//! (flat / smooth / auto-smooth / sharp edges / custom), uv0, and the polygon
//! edge list the wireframe pass draws.

use glam::{DVec2, DVec3};

use super::{Adjacency, DEGENERATE_AREA, EditMesh, EdgeFlags, FaceFlags};

/// One material run inside `indices`.
#[derive(Clone, Debug, PartialEq)]
pub struct MatRange {
    pub material: usize,
    pub start: u32,
    pub count: u32,
}

/// Triangles plus the polygon edges, ready to feed a glb-shaped consumer.
#[derive(Clone, Debug, Default)]
pub struct EvaluatedMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub ranges: Vec<MatRange>,
    /// Polygon edges (not triangulation diagonals) in vertex-table ids.
    pub poly_edges: Vec<[u32; 2]>,
}

/// Orthonormal face-plane basis with `u x v = n`.
fn plane_basis(n: DVec3) -> (DVec3, DVec3) {
    let seed = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = n.cross(seed).normalize_or(DVec3::X);
    let v = n.cross(u).normalize_or(DVec3::Y);
    (u, v)
}

fn signed_area_2d(pts: &[DVec2]) -> f64 {
    (0..pts.len()).map(|k| pts[k].perp_dot(pts[(k + 1) % pts.len()])).sum::<f64>() * 0.5
}

/// Triangulates one face in its own plane. Returns corner offsets.
pub(crate) fn triangulate_face(pts: &[DVec3]) -> Vec<[usize; 3]> {
    let n = super::newell(pts).normalize_or_zero();
    if n == DVec3::ZERO {
        return Vec::new();
    }
    let (u, v) = plane_basis(n);
    let c = pts.iter().copied().sum::<DVec3>() / pts.len() as f64;
    let mut flat: Vec<DVec2> = pts.iter().map(|p| DVec2::new((*p - c).dot(u), (*p - c).dot(v))).collect();
    let flip = signed_area_2d(&flat) < 0.0;
    if flip {
        flat.reverse();
    }
    let tris = crate::meshgen::ear_clip(&flat);
    if flip {
        tris.into_iter().map(|[a, b, c]| [pts.len() - 1 - a, pts.len() - 1 - b, pts.len() - 1 - c]).collect()
    } else {
        tris
    }
}

/// Every triangle of the mesh with its face id, in object space, through
/// the same clipper `evaluate` uses. The pick and occlusion tests build on
/// this so they agree with what the viewport draws.
pub fn face_triangles(mesh: &EditMesh) -> Vec<(u32, [DVec3; 3])> {
    let mut out = Vec::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let pts: Vec<DVec3> =
            mesh.corners[f.first as usize..f.first as usize + f.len as usize].iter().map(|c| mesh.positions[c.vertex as usize]).collect();
        if super::newell(&pts).length() * 0.5 < DEGENERATE_AREA {
            continue;
        }
        for [a, b, c] in triangulate_face(&pts) {
            out.push((fi as u32, [pts[a], pts[b], pts[c]]));
        }
    }
    out
}
/// True when the edge between two faces is a hard shading break.
fn edge_is_hard(mesh: &EditMesh, a: u32, b: u32, na: DVec3, nb: DVec3) -> bool {
    if mesh.edge_flag(a, b).contains(EdgeFlags::SHARP) {
        return true;
    }
    match mesh.smooth_angle {
        Some(limit) => {
            let cos = na.dot(nb).clamp(-1.0, 1.0);
            cos.acos().to_degrees() > limit + 1e-9
        }
        None => false,
    }
}

pub fn evaluate(mesh: &EditMesh) -> EvaluatedMesh {
    let adj = Adjacency::build(mesh);
    let face_normals: Vec<DVec3> = (0..mesh.faces.len()).map(|f| mesh.face_normal(f)).collect();
    let mut out = EvaluatedMesh::default();
    let mut edge_set: Vec<[u32; 2]> = Vec::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let corners = &mesh.corners[f.first as usize..f.first as usize + f.len as usize];
        let pts: Vec<DVec3> = corners.iter().map(|c| mesh.positions[c.vertex as usize]).collect();
        for k in 0..corners.len() {
            let (a, b) = (corners[k].vertex, corners[(k + 1) % corners.len()].vertex);
            if a != b {
                edge_set.push(if a < b { [a, b] } else { [b, a] });
            }
        }
        if super::newell(&pts).length() * 0.5 < DEGENERATE_AREA {
            continue;
        }
        let tris = triangulate_face(&pts);
        if tris.is_empty() {
            continue;
        }
        let smooth_face = f.flags.contains(FaceFlags::SMOOTH) || mesh.smooth_angle.is_some();
        let own_verts = mesh.face_verts(fi);
        let mut own_edges: Vec<(u32, u32)> = Vec::with_capacity(own_verts.len());
        for k in 0..own_verts.len() {
            let (a, b) = (own_verts[k], own_verts[(k + 1) % own_verts.len()]);
            own_edges.push((a.min(b), a.max(b)));
        }
        let mut vert_of: Vec<u32> = Vec::with_capacity(corners.len());
        for c in corners.iter() {
            let n = match c.normal {
                Some(n) => DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64).normalize_or(face_normals[fi]),
                _ if !smooth_face => face_normals[fi],
                _ => {
                    let mut sum = face_normals[fi];
                    for nf in adj.faces_of(c.vertex) {
                        let nf = *nf as usize;
                        if nf == fi {
                            continue;
                        }
                        if mesh.smooth_angle.is_none() && !mesh.faces[nf].flags.contains(FaceFlags::SMOOTH) {
                            continue;
                        }
                        let wn = mesh.face_verts(nf);
                        let mut keep = true;
                        for q in 0..wn.len() {
                            let (x, y) = (wn[q], wn[(q + 1) % wn.len()]);
                            if x != c.vertex && y != c.vertex {
                                continue;
                            }
                            let e = (x.min(y), x.max(y));
                            if own_edges.contains(&e)
                                && edge_is_hard(mesh, e.0, e.1, face_normals[fi], face_normals[nf])
                            {
                                keep = false;
                                break;
                            }
                        }
                        if keep {
                            sum += face_normals[nf];
                        }
                    }
                    sum.normalize_or(face_normals[fi])
                }
            };
            let p = mesh.positions[c.vertex as usize];
            vert_of.push(out.positions.len() as u32);
            out.positions.push([p.x as f32, p.y as f32, p.z as f32]);
            out.normals.push([n.x as f32, n.y as f32, n.z as f32]);
            out.uvs.push(c.uv);
        }
        let slot = match out.ranges.last() {
            Some(r) if r.material == f.material as usize => out.ranges.len() - 1,
            _ => {
                out.ranges.push(MatRange {
                    material: f.material as usize,
                    start: out.indices.len() as u32,
                    count: 0,
                });
                out.ranges.len() - 1
            }
        };
        for [a, b, c] in tris {
            out.indices.extend([vert_of[a], vert_of[b], vert_of[c]]);
        }
        out.ranges[slot].count = out.indices.len() as u32 - out.ranges[slot].start;
    }
    edge_set.sort_unstable();
    edge_set.dedup();
    out.poly_edges = edge_set;
    out
}

/// Evaluates the base mesh through its modifier stack, then triangulates.
pub fn evaluate_with(
    base: &EditMesh,
    mods: &[super::Modifier],
    ctx: &super::EvalContext,
    viewport_only: bool,
) -> Result<EvaluatedMesh, super::EditError> {
    Ok(evaluate(&super::evaluate_stack(base, mods, ctx, viewport_only)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editmesh::convert::weld_primitives;
    use crate::import::glb::GlbMesh;
    use crate::meshgen::{build, MeshSpec};

    fn welded(spec: &MeshSpec) -> EditMesh {
        let g = build(spec, "t", "m", 1.0 / 512.0).unwrap();
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
    fn box_evaluates_to_twelve_triangles() {
        let m = welded(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
        let e = evaluate(&m);
        assert_eq!(e.indices.len() / 3, 12);
        assert_eq!(e.ranges.len(), 1);
        assert_eq!(e.poly_edges.len(), 18);
    }

    #[test]
    fn cylinder_caps_triangulate() {
        let m = welded(&MeshSpec::Cylinder { radius: 32.0, height: 64.0, segments: 16, caps: true });
        let e = evaluate(&m);
        assert_eq!(e.indices.len() / 3, 16 * 2 + (16 - 2) * 2);
    }
}
