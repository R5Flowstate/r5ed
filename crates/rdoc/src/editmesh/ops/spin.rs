//! spin and screw: sweep a profile around an axis, the lathe a tower wall
//! and the helix a spiral stair are built from.
//!
//! The profile is the selected edges, or the boundary of the selected face
//! region. Faces the sweep consumes are deleted: their boundary loop is what
//! the walls continue, so a closed profile spun full circle closes into a tube
//! instead of going non-manifold along the profile. Neighbouring unselected
//! faces stay stitched through the shared ring-0 vertices. New walls wind
//! outward: each wall is checked against its profile edge's in-face
//! perpendicular swept to the wall's angle and flipped when it disagrees.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};

use super::{clamp_finite, parse_params, selected_edges, selected_faces, OpResult};
use crate::editmesh::{
    bad, finite_in_range, newell, Adjacency, Corner, EditMesh, Face, FaceFlags,
};
use crate::editmesh::select::{SelMode, Selection};

fn default_axis() -> [f64; 3] {
    [0.0, 0.0, 1.0]
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpinParams {
    #[serde(default = "default_axis")]
    pub axis: [f64; 3],
    #[serde(default)]
    pub center: [f64; 3],
    #[serde(default = "default_spin_angle")]
    pub angle_deg: f64,
    #[serde(default = "default_spin_steps")]
    pub steps: u32,
    #[serde(default)]
    pub dupli: bool,
    #[serde(default = "default_true")]
    pub merge_ends: bool,
}

fn default_spin_angle() -> f64 {
    90.0
}

fn default_spin_steps() -> u32 {
    9
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScrewParams {
    #[serde(default = "default_axis")]
    pub axis: [f64; 3],
    #[serde(default)]
    pub center: [f64; 3],
    #[serde(default = "default_turn_angle")]
    pub angle_deg: f64,
    #[serde(default = "default_turns")]
    pub turns: u32,
    #[serde(default = "default_height")]
    pub height: f64,
    #[serde(default = "default_screw_steps")]
    pub steps: u32,
}

fn default_turn_angle() -> f64 {
    360.0
}

fn default_turns() -> u32 {
    1
}

fn default_height() -> f64 {
    64.0
}

fn default_screw_steps() -> u32 {
    16
}

pub fn spin(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: SpinParams = parse_params(params)?;
    let (axis, center) = axis_center(&p.axis, &p.center)?;
    let angle = clamp_finite(p.angle_deg, -360.0, 360.0, "angle_deg")?;
    if angle.abs() < 1e-9 {
        return Err(bad("spin angle is zero"));
    }
    let steps = p.steps.clamp(1, 512) as usize;
    let merged = p.merge_ends && (angle.abs() - 360.0).abs() < 1e-6;
    if merged && steps < 2 {
        return Err(bad("a closed spin needs at least 2 steps"));
    }
    let sweep = Sweep {
        axis,
        center,
        step_angle: (angle / steps as f64).to_radians(),
        steps,
        axial: 0.0,
        merge_last: merged,
    };
    run_sweep(mesh, sel, &sweep, p.dupli)
}

pub fn screw(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: ScrewParams = parse_params(params)?;
    let (axis, center) = axis_center(&p.axis, &p.center)?;
    let per_turn = clamp_finite(p.angle_deg, -360.0, 360.0, "angle_deg")?;
    let turns = p.turns.clamp(1, 64) as usize;
    let height = clamp_finite(p.height, -1e6, 1e6, "height")?;
    let per_step = p.steps.clamp(1, 512) as usize;
    if per_turn.abs() < 1e-9 && height.abs() < 1e-9 {
        return Err(bad("a screw with no turn and no rise copies the profile onto itself"));
    }
    let sweep = Sweep {
        axis,
        center,
        step_angle: (per_turn / per_step as f64).to_radians(),
        steps: turns * per_step,
        axial: height / per_step as f64,
        merge_last: false,
    };
    run_sweep(mesh, sel, &sweep, false)
}

fn axis_center(axis: &[f64; 3], center: &[f64; 3]) -> Result<(DVec3, DVec3), crate::editmesh::EditError> {
    let axis = DVec3::new(
        clamp_finite(axis[0], -1e6, 1e6, "axis")?,
        clamp_finite(axis[1], -1e6, 1e6, "axis")?,
        clamp_finite(axis[2], -1e6, 1e6, "axis")?,
    );
    if !axis.is_finite() || axis == DVec3::ZERO {
        return Err(bad("param axis has no direction"));
    }
    let center = DVec3::new(
        clamp_finite(center[0], -1e6, 1e6, "center")?,
        clamp_finite(center[1], -1e6, 1e6, "center")?,
        clamp_finite(center[2], -1e6, 1e6, "center")?,
    );
    Ok((axis.normalize(), center))
}

struct Sweep {
    axis: DVec3,
    center: DVec3,
    step_angle: f64,
    steps: usize,
    axial: f64,
    merge_last: bool,
}

struct Profile {
    verts: Vec<u32>,
    /// Profile edges directed along their reference face's traversal, so the
    /// wall quads inherit the loop direction instead of a sorted pair order.
    edges: Vec<(u32, u32)>,
    dead: BTreeSet<u32>,
    reference: BTreeMap<(u32, u32), u32>,
}

type WallFace = (Vec<(u32, [f32; 2])>, u16);
type DupliFace = (Vec<(u32, [f32; 2])>, Face);

/// The directed traversal of an edge inside a face loop.
fn directed_in_face(mesh: &EditMesh, face: u32, a: u32, b: u32) -> (u32, u32) {
    let w = mesh.face_verts(face as usize);
    for k in 0..w.len() {
        let (x, y) = (w[k], w[(k + 1) % w.len()]);
        if (x == a && y == b) || (x == b && y == a) {
            if x == a {
                return (a, b);
            }
            return (b, a);
        }
    }
    (a, b)
}

fn resolve_profile(mesh: &EditMesh, adj: &Adjacency, sel: &Selection) -> Result<Profile, crate::editmesh::EditError> {
    match sel.mode {
        SelMode::Face => {
            let region = selected_faces(mesh, adj, sel);
            if region.is_empty() {
                return Err(bad("spin names no faces"));
            }
            let mut counts: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
            for f in &region {
                for e in super::face_edges(mesh, *f as usize) {
                    counts.entry(e).or_default().push(*f);
                }
            }
            let mut edges = Vec::new();
            let mut reference = BTreeMap::new();
            for (e, faces) in &counts {
                if faces.len() == 1 {
                    edges.push(directed_in_face(mesh, faces[0], e.0, e.1));
                    reference.insert(*e, faces[0]);
                }
            }
            if edges.is_empty() {
                return Err(bad("the spin profile has no boundary; select a profile with an open edge loop"));
            }
            Ok(Profile { verts: Vec::new(), edges, dead: region, reference })
        }
        _ => {
            let set = selected_edges(mesh, adj, sel);
            if set.is_empty() {
                return Err(bad("spin names no edges"));
            }
            let mut dead = BTreeSet::new();
            for (fi, _) in mesh.faces.iter().enumerate() {
                if super::face_edges(mesh, fi).iter().all(|e| set.contains(e)) {
                    dead.insert(fi as u32);
                }
            }
            let mut reference = BTreeMap::new();
            let mut edges = Vec::new();
            for e in &set {
                let edge = adj.edge(e.0, e.1);
                let pick = edge
                    .iter()
                    .flat_map(|v| v.faces.iter().copied())
                    .filter(|f| dead.contains(f))
                    .min()
                    .or_else(|| edge.and_then(|v| v.faces.iter().copied().min()));
                if let Some(f) = pick {
                    reference.insert(*e, f);
                    edges.push(directed_in_face(mesh, f, e.0, e.1));
                }
            }
            Ok(Profile { verts: Vec::new(), edges, dead, reference })
        }
    }
}

fn corner_uv(mesh: &EditMesh, face: u32, vert: u32) -> [f32; 2] {
    let f = &mesh.faces[face as usize];
    for c in &mesh.corners[f.first as usize..f.first as usize + f.len as usize] {
        if c.vertex == vert {
            return c.uv;
        }
    }
    [0.0, 0.0]
}

fn run_sweep(mesh: &mut EditMesh, sel: &Selection, sweep: &Sweep, dupli: bool) -> OpResult {
    let adj = Adjacency::build(mesh);
    let mut profile = resolve_profile(mesh, &adj, sel)?;
    let mut sweep_verts: BTreeSet<u32> = BTreeSet::new();
    for (a, b) in &profile.edges {
        sweep_verts.insert(*a);
        sweep_verts.insert(*b);
    }
    if dupli {
        for f in &profile.dead {
            sweep_verts.extend(mesh.face_verts(*f as usize));
        }
    }
    if sweep_verts.is_empty() {
        return Err(bad("the spin profile touches no vertices"));
    }
    profile.verts = sweep_verts.iter().copied().collect();
    let at: BTreeMap<u32, usize> = profile.verts.iter().enumerate().map(|(k, v)| (*v, k)).collect();
    let last = if sweep.merge_last { sweep.steps - 1 } else { sweep.steps };
    let on_axis: Vec<bool> = profile
        .verts
        .iter()
        .map(|v| (mesh.positions[*v as usize] - sweep.center).cross(sweep.axis).length() <= 1e-9)
        .collect();
    let mut extra: Vec<u32> = Vec::new();
    for (k, _) in profile.verts.iter().enumerate() {
        if on_axis[k] {
            extra.push(1);
        } else {
            extra.push(last as u32);
        }
    }
    let new_verts: usize = extra.iter().map(|n| *n as usize).sum();
    let wall_count = profile.edges.len() * sweep.steps;
    let dupli_count: usize = if dupli { profile.dead.len() * last } else { 0 };
    let dupli_corners: usize = if dupli {
        profile.dead.iter().map(|f| mesh.faces[*f as usize].len as usize * last).sum()
    } else {
        0
    };
    if mesh.positions.len() + new_verts > super::super::MAX_VERTICES
        || mesh.faces.len() + wall_count + dupli_count > super::super::MAX_FACES
        || mesh.corners.len() + wall_count * 4 + dupli_corners > super::super::MAX_CORNERS
    {
        return Err(bad("the sweep would pass the mesh budget"));
    }
    let mut ring: Vec<Vec<u32>> = vec![Vec::new(); profile.verts.len()];
    for (k, v) in profile.verts.iter().enumerate() {
        ring[k].push(*v);
        if on_axis[k] {
            let p = mesh.positions[*v as usize];
            let along = (p - sweep.center).dot(sweep.axis);
            let q = sweep.center + sweep.axis * along;
            if !(q.is_finite() && finite_in_range(q.x) && finite_in_range(q.y) && finite_in_range(q.z)) {
                return Err(bad("the sweep leaves the 1e6 build range"));
            }
            let id = mesh.positions.len() as u32;
            mesh.positions.push(q);
            for _ in 1..=last {
                ring[k].push(id);
            }
            continue;
        }
        for i in 1..=last {
            let r = DQuat::from_axis_angle(sweep.axis, sweep.step_angle * i as f64);
            let q = sweep.center + r * (mesh.positions[*v as usize] - sweep.center) + sweep.axis * (sweep.axial * i as f64);
            if !(q.is_finite() && finite_in_range(q.x) && finite_in_range(q.y) && finite_in_range(q.z)) {
                return Err(bad("the sweep leaves the 1e6 build range"));
            }
            ring[k].push(mesh.positions.len() as u32);
            mesh.positions.push(q);
        }
    }
    let ring_id = |v: u32, i: usize| -> u32 {
        if i == sweep.steps && sweep.merge_last {
            v
        } else {
            ring[at[&v]][i]
        }
    };
    let mut walls: Vec<WallFace> = Vec::with_capacity(wall_count);
    for (a, b) in &profile.edges {
        let key = ((*a).min(*b), (*a).max(*b));
        let f = *profile.reference.get(&key).unwrap_or(&0);
        let material = mesh.faces.get(f as usize).map(|x| x.material).unwrap_or(0);
        let (uv_a, uv_b) = (corner_uv(mesh, f, *a), corner_uv(mesh, f, *b));
        let ref_n = mesh.face_normal(f as usize);
        for i in 0..sweep.steps {
            let quad = [(ring_id(*a, i), uv_a), (ring_id(*b, i), uv_b), (ring_id(*b, i + 1), uv_b), (ring_id(*a, i + 1), uv_a)];
            let mut deduped: Vec<(u32, [f32; 2])> = Vec::with_capacity(4);
            for q in quad {
                if deduped.last().map(|d| d.0) != Some(q.0) {
                    deduped.push(q);
                }
            }
            if deduped.len() == 4 && deduped[0].0 == deduped[3].0 {
                deduped.pop();
            }
            if deduped.len() < 3 {
                continue;
            }
            let pts: Vec<DVec3> = deduped.iter().map(|(v, _)| mesh.positions[*v as usize]).collect();
            let n = newell(&pts);
            if n != DVec3::ZERO && ref_n != DVec3::ZERO {
                let e0 = mesh.positions[*b as usize] - mesh.positions[*a as usize];
                let side = e0.cross(ref_n);
                if side != DVec3::ZERO {
                    let rm = DQuat::from_axis_angle(sweep.axis, sweep.step_angle * (i as f64 + 0.5));
                    if n.dot(rm * side) < 0.0 {
                        deduped.reverse();
                    }
                }
            }
            walls.push((deduped, material));
        }
    }
    let mut copies: Vec<DupliFace> = Vec::new();
    if dupli {
        let mut dead: Vec<u32> = profile.dead.iter().copied().collect();
        dead.sort_unstable();
        for i in 1..=last {
            for f in &dead {
                let face = mesh.faces[*f as usize];
                let mut corners = Vec::with_capacity(face.len as usize);
                for c in &mesh.corners[face.first as usize..face.first as usize + face.len as usize] {
                    corners.push((ring_id(c.vertex, i), c.uv));
                }
                copies.push((corners, face));
            }
        }
    }
    let saved_flags = mesh.edge_flags.clone();
    super::drop_faces(mesh, &profile.dead);
    let first_face = mesh.faces.len() as u32;
    for (corners, material) in &walls {
        let first = mesh.corners.len() as u32;
        for (v, uv) in corners {
            mesh.corners.push(Corner { vertex: *v, uv: *uv, normal: None });
        }
        mesh.faces.push(Face { first, len: corners.len() as u32, material: *material, flags: FaceFlags::empty() });
    }
    for (corners, face) in &copies {
        let first = mesh.corners.len() as u32;
        for (v, uv) in corners {
            mesh.corners.push(Corner { vertex: *v, uv: *uv, normal: None });
        }
        mesh.faces.push(Face { first, len: corners.len() as u32, material: face.material, flags: face.flags });
    }
    super::carry_flags(mesh, &saved_flags);
    super::compact(mesh);
    let dropped = super::drop_degenerate(mesh);
    let mut warnings = Vec::new();
    if dropped > 0 {
        warnings.push(format!("{dropped} swept faces collapsed and were removed"));
    }
    if !(finite_mesh(mesh)) {
        return Err(bad("the sweep left non-finite positions"));
    }
    let made: Vec<u32> = (first_face..mesh.faces.len() as u32).collect();
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}

fn finite_mesh(mesh: &EditMesh) -> bool {
    mesh.positions.iter().all(|p| p.is_finite()) && mesh.corners.iter().all(|c| c.uv.iter().all(|v| v.is_finite()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editmesh::validate::invariants;
    use crate::editmesh::{Corner, FaceFlags};

    fn plate(loop_pts: Vec<DVec3>) -> EditMesh {
        let n = loop_pts.len();
        let mut mesh = EditMesh {
            positions: loop_pts,
            faces: Vec::new(),
            corners: Vec::new(),
            edge_flags: Vec::new(),
            materials: vec!["m".to_string()],
            smooth_angle: None,
        };
        for v in 0..n as u32 {
            mesh.corners.push(Corner { vertex: v, uv: [v as f32, 0.0], normal: None });
        }
        mesh.faces.push(Face { first: 0, len: n as u32, material: 0, flags: FaceFlags::empty() });
        mesh
    }

    fn arch_loop() -> Vec<DVec3> {
        vec![
            DVec3::new(48.0, 0.0, 0.0),
            DVec3::new(80.0, 0.0, 0.0),
            DVec3::new(80.0, 0.0, 24.0),
            DVec3::new(75.31, 0.0, 35.31),
            DVec3::new(64.0, 0.0, 40.0),
            DVec3::new(52.69, 0.0, 35.31),
            DVec3::new(48.0, 0.0, 24.0),
        ]
    }

    fn face_sel(ids: Vec<u32>) -> Selection {
        Selection { mode: SelMode::Face, ids }
    }

    fn all_faces(mesh: &EditMesh) -> Selection {
        face_sel((0..mesh.faces.len() as u32).collect())
    }

    fn min_separation(mesh: &EditMesh) -> f64 {
        let mut d = f64::INFINITY;
        for i in 0..mesh.positions.len() {
            for j in i + 1..mesh.positions.len() {
                d = d.min((mesh.positions[i] - mesh.positions[j]).length());
            }
        }
        d
    }

    #[test]
    fn spin_arch_360_32_steps_closes_and_welds_seam() {
        let mut m = plate(arch_loop());
        let n = m.positions.len();
        let sel = all_faces(&m);
        let (out, warnings) = spin(&mut m, &sel, &serde_json::json!({"angle_deg": 360.0, "steps": 32})).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(m.positions.len(), 32 * n, "seam merged: no extra final ring");
        assert_eq!(m.faces.len(), 32 * n, "one swept quad per profile edge per step");
        assert_eq!(out.ids.len(), 32 * n);
        assert_eq!(out.mode, SelMode::Face);
        let inv = invariants(&m);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        assert_eq!(inv.euler, 0, "a torus has characteristic 0");
        assert!(min_separation(&m) > 1e-6, "no duplicate verts on the seam");
    }

    #[test]
    fn screw_three_turns_has_exact_vertex_count() {
        let mut m = plate(vec![
            DVec3::new(64.0, 0.0, 0.0),
            DVec3::new(96.0, 0.0, 0.0),
            DVec3::new(96.0, 0.0, 32.0),
            DVec3::new(64.0, 0.0, 32.0),
        ]);
        let sel = all_faces(&m);
        let params = serde_json::json!({"angle_deg": 360.0, "turns": 3, "height": 32.0, "steps": 8});
        let (out, _) = screw(&mut m, &sel, &params).unwrap();
        assert_eq!(m.positions.len(), 4 + 4 * 24, "profile verts times total steps");
        assert_eq!(m.faces.len(), 4 * 24);
        assert_eq!(out.ids.len(), 4 * 24);
        let inv = invariants(&m);
        assert!(inv.wound && inv.degenerate.is_empty(), "{inv:?}");
        let top = m.positions.iter().map(|p| p.z).fold(f64::NEG_INFINITY, f64::max);
        assert!((top - 128.0).abs() < 1e-6, "three rises of 32 over a 32 base: {top}");
    }

    #[test]
    fn spin_partial_fan_counts_and_stays_open() {
        let mut m = plate(vec![
            DVec3::new(64.0, 0.0, 0.0),
            DVec3::new(96.0, 0.0, 0.0),
            DVec3::new(96.0, 0.0, 32.0),
            DVec3::new(64.0, 0.0, 32.0),
        ]);
        let sel = all_faces(&m);
        let (out, _) = spin(&mut m, &sel, &serde_json::json!({"angle_deg": 90.0, "steps": 4})).unwrap();
        assert_eq!(m.positions.len(), 4 + 4 * 4);
        assert_eq!(m.faces.len(), 4 * 4);
        assert_eq!(out.ids.len(), 4 * 4);
        let inv = invariants(&m);
        assert!(!inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
    }

    #[test]
    fn spin_half_turn_does_not_merge() {
        let mut m = plate(vec![
            DVec3::new(64.0, 0.0, 0.0),
            DVec3::new(96.0, 0.0, 0.0),
            DVec3::new(96.0, 0.0, 32.0),
            DVec3::new(64.0, 0.0, 32.0),
        ]);
        let sel = all_faces(&m);
        let (_, _) = spin(&mut m, &sel, &serde_json::json!({"angle_deg": 180.0, "steps": 8})).unwrap();
        assert_eq!(m.positions.len(), 4 + 4 * 8);
        assert_eq!(m.faces.len(), 4 * 8);
    }

    #[test]
    fn spin_edge_mode_consumes_fully_selected_faces() {
        let mut m = plate(vec![
            DVec3::new(64.0, 0.0, 0.0),
            DVec3::new(96.0, 0.0, 0.0),
            DVec3::new(96.0, 0.0, 32.0),
            DVec3::new(64.0, 0.0, 32.0),
        ]);
        let adj = Adjacency::build(&m);
        let ids: Vec<u32> = (0..adj.edges.len() as u32).collect();
        let sel = Selection { mode: SelMode::Edge, ids };
        let (_, _) = spin(&mut m, &sel, &serde_json::json!({"angle_deg": 90.0, "steps": 4})).unwrap();
        assert_eq!(m.positions.len(), 4 + 4 * 4);
        assert_eq!(m.faces.len(), 4 * 4);
        let inv = invariants(&m);
        assert!(inv.wound && inv.degenerate.is_empty(), "{inv:?}");
    }

    #[test]
    fn spin_rejects_bad_input() {
        let mut m = plate(arch_loop());
        let sel = all_faces(&m);
        assert!(spin(&mut m, &sel, &serde_json::json!({"angle_deg": 0.0})).is_err());
        assert!(spin(&mut m, &sel, &serde_json::json!({"angle_deg": 90.0, "nope": 1})).is_err());
        assert!(spin(&mut m, &sel, &serde_json::json!({"axis": [0.0, 0.0, 0.0], "angle_deg": 90.0})).is_err());
        assert!(
            screw(&mut m, &sel, &serde_json::json!({"angle_deg": 0.0, "height": 0.0, "turns": 1, "steps": 4}))
                .is_err()
        );
        let empty = Selection { mode: SelMode::Face, ids: vec![99] };
        assert!(spin(&mut m, &empty, &serde_json::json!({"angle_deg": 90.0})).is_err());
    }

    #[test]
    fn sweeps_are_deterministic_and_dispatched() {
        let mut a = plate(arch_loop());
        let mut b = plate(arch_loop());
        let sa = all_faces(&a);
        let sb = all_faces(&b);
        let params = serde_json::json!({"angle_deg": 90.0, "steps": 4});
        let _ = spin(&mut a, &sa, &params).unwrap();
        let _ = spin(&mut b, &sb, &params).unwrap();
        assert_eq!(super::super::super::io::write(&a), super::super::super::io::write(&b));
        let mut m = plate(arch_loop());
        let sel = all_faces(&m);
        let (out, _) = super::super::dispatch("spin", &mut m, &sel, &params).unwrap();
        assert_eq!(out.mode, SelMode::Face);
        let mut m = plate(arch_loop());
        let sel = all_faces(&m);
        let (out, _) =
            super::super::dispatch("screw", &mut m, &sel, &serde_json::json!({"turns": 1, "steps": 4})).unwrap();
        assert_eq!(out.mode, SelMode::Face);
    }

    #[test]
    fn spin_through_plan_reaches_the_cli_and_wasm_path() {
        let profile = plate(arch_loop());
        let n = profile.positions.len() as u32;
        let mut doc = crate::doc::Document::empty();
        let layer = doc.layer_ids()[0];
        let mut object = crate::doc::new_object("arch", layer, crate::model::ObjectKind::Mesh(crate::model::Mesh {
            source: String::new(),
            material_map: [("m".to_string(), "m".to_string())].into_iter().collect(),
            collision: crate::model::CollisionMode::Triangles,
            contents: 1,
            cast_shadows: true,
            lightmap_scale: 0.0,
            render: true,
            role: crate::model::MeshRole::Normal,
            modifiers: Vec::new(),
        }));
        let path = format!("meshes/{}.r5mesh", object.id);
        if let crate::model::ObjectKind::Mesh(m) = &mut object.kind {
            m.source = path.clone();
        }
        let object_id = object.id;
        doc.apply(crate::command::Command::Batch {
            label: "add".into(),
            commands: vec![
                crate::command::Command::SetFile { path, bytes: Some(crate::editmesh::io::write(&profile)) },
                crate::command::Command::AddObjects { objects: vec![object] },
            ],
        })
        .unwrap();
        let op = crate::editmesh::OpRequest {
            op: "spin".into(),
            object: object_id,
            sel: crate::editmesh::select::Sel {
                mode: SelMode::Face,
                ids: vec![0],
                query: None,
            },
            params: serde_json::json!({"angle_deg": 360.0, "steps": 32}),
        };
        let out = crate::editmesh::plan(&doc, &op).unwrap();
        assert_eq!(out.report.faces, 32 * n, "the plan path spins like the direct one: {:?}", out.report);
        assert_eq!(out.selection.ids.len(), (32 * n) as usize);
    }
}
