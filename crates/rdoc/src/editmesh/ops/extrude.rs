//! extrude_region: selected faces along their average normal (or each face
//! along its own with `individual`), with side walls around the boundary.

use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use super::{clamp_finite, parse_params, selected_faces, OpResult};
use crate::editmesh::{bad, Adjacency, Corner, EditMesh, Face, FaceFlags};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default = "default_distance")]
    pub distance: f64,
    #[serde(default)]
    pub individual: bool,
}

fn default_distance() -> f64 {
    16.0
}

pub fn run(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    let distance = clamp_finite(p.distance, -1e6, 1e6, "distance")?;
    if distance.abs() < 1e-9 {
        return Err(bad("extrude distance is zero"));
    }
    let adj = Adjacency::build(mesh);
    let mut region = selected_faces(mesh, &adj, sel);
    if region.is_empty() {
        region = (0..mesh.faces.len() as u32).collect();
    }
    if region.is_empty() {
        return Err(bad("the mesh has no faces to extrude"));
    }
    if mesh.positions.len() + region.len() * 8 > super::super::MAX_VERTICES {
        return Err(bad("the extrusion would pass the vertex budget"));
    }
    let saved_flags = mesh.edge_flags.clone();
    let avg = {
        let mut n = DVec3::ZERO;
        for f in &region {
            n += mesh.face_normal(*f as usize);
        }
        n.normalize_or(DVec3::Z)
    };
    let offset = avg * distance;
    let mut new_selection = Vec::new();
    let faces_before = mesh.faces.len();
    let mut shared: BTreeMap<u32, u32> = BTreeMap::new();
    if p.individual {
        let faces: Vec<u32> = region.iter().copied().collect();
        for f in faces {
            let face = mesh.faces[f as usize];
            let n = mesh.face_normal(f as usize);
            let off = n * distance;
            let mut owned: BTreeMap<u32, u32> = BTreeMap::new();
            extrude_one(mesh, f, &all_edges(mesh, f), off, &mut owned, &mut new_selection, face.material);
        }
    } else {
        let mut boundary: BTreeSet<(u32, u32)> = BTreeSet::new();
        let mut counts: BTreeMap<(u32, u32), usize> = BTreeMap::new();
        for f in &region {
            for e in super::face_edges(mesh, *f as usize) {
                *counts.entry(e).or_insert(0) += 1;
            }
        }
        for (e, n) in counts {
            if n == 1 {
                boundary.insert(e);
            }
        }
        let faces: Vec<u32> = region.iter().copied().collect();
        for f in faces {
            extrude_one(mesh, f, &boundary, offset, &mut shared, &mut new_selection, mesh.faces[f as usize].material);
        }
    }
    let added = mesh.faces.len() - faces_before;
    super::drop_faces(mesh, &region);
    super::carry_flags(mesh, &saved_flags);
    super::compact(mesh);
    let new_selection: Vec<u32> = ((mesh.faces.len() - added) as u32..mesh.faces.len() as u32).collect();
    Ok((Selection { mode: SelMode::Face, ids: new_selection }, Vec::new()))
}

fn all_edges(mesh: &EditMesh, f: u32) -> BTreeSet<(u32, u32)> {
    super::face_edges(mesh, f as usize).into_iter().collect()
}

/// Copies face `f` along `off` and walls the given boundary edges. The caller
/// drops the original region faces, so the caps replace them.
fn extrude_one(
    mesh: &mut EditMesh,
    f: u32,
    boundary: &BTreeSet<(u32, u32)>,
    off: DVec3,
    moved_map: &mut BTreeMap<u32, u32>,
    caps: &mut Vec<u32>,
    material: u16,
) {
    let face = mesh.faces[f as usize];
    let src: Vec<Corner> = mesh.corners[face.first as usize..face.first as usize + face.len as usize].to_vec();
    let ordered: Vec<u32> = src.iter().map(|c| c.vertex).collect();
    let mut moved: Vec<u32> = Vec::with_capacity(src.len());
    for c in &src {
        let v = match moved_map.get(&c.vertex) {
            Some(v) => *v,
            None => {
                let v = mesh.positions.len() as u32;
                mesh.positions.push(mesh.positions[c.vertex as usize] + off);
                moved_map.insert(c.vertex, v);
                v
            }
        };
        moved.push(v);
    }
    let cap_first = mesh.corners.len() as u32;
    for (k, c) in src.iter().enumerate() {
        mesh.corners.push(Corner { vertex: moved[k], uv: c.uv, normal: None });
    }
    mesh.faces.push(Face { first: cap_first, len: src.len() as u32, material, flags: face.flags });
    caps.push(mesh.faces.len() as u32 - 1);
    for k in 0..ordered.len() {
        let (a, b) = (ordered[k], ordered[(k + 1) % ordered.len()]);
        if !boundary.contains(&(a.min(b), a.max(b))) {
            continue;
        }
        let (ma, mb) = (moved[k], moved[(k + 1) % moved.len()]);
        let wall_first = mesh.corners.len() as u32;
        let auv = src[k].uv;
        let buv = src[(k + 1) % src.len()].uv;
        mesh.corners.push(Corner { vertex: a, uv: auv, normal: None });
        mesh.corners.push(Corner { vertex: b, uv: buv, normal: None });
        mesh.corners.push(Corner { vertex: mb, uv: buv, normal: None });
        mesh.corners.push(Corner { vertex: ma, uv: auv, normal: None });
        let flags = FaceFlags::empty();
        mesh.faces.push(Face { first: wall_first, len: 4, material, flags });
    }
}
