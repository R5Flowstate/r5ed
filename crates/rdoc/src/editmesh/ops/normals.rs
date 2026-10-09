//! recalc_normals {outside | inside}, flip, shade {smooth | flat}.

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::{parse_params, selected_faces, OpResult};
use crate::editmesh::{Adjacency, EditMesh, FaceFlags};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecalcMode {
    #[default]
    Outside,
    Inside,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecalcParams {
    #[serde(default)]
    pub mode: RecalcMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadeMode {
    Smooth,
    Flat,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadeParams {
    pub mode: ShadeMode,
}

fn reverse_face(mesh: &mut EditMesh, fi: usize) {
    let f = mesh.faces[fi];
    mesh.corners[f.first as usize..f.first as usize + f.len as usize].reverse();
}

fn signed_volume(mesh: &EditMesh, faces: &[u32]) -> f64 {
    let mut v = 0.0;
    for f in faces {
        let pts = mesh.face_points(*f as usize);
        if pts.len() < 3 {
            continue;
        }
        let normal = crate::editmesh::newell(&pts);
        let c = pts.iter().copied().sum::<glam::DVec3>() / pts.len() as f64;
        v += normal.dot(c) / 3.0;
    }
    v
}

fn linked_components(mesh: &EditMesh, adj: &Adjacency) -> Vec<Vec<u32>> {
    let mut todo: BTreeSet<u32> = (0..mesh.faces.len() as u32).collect();
    let mut out = Vec::new();
    while let Some(seed) = todo.iter().next().copied() {
        let mut comp = vec![seed];
        let mut queue: VecDeque<u32> = VecDeque::from([seed]);
        todo.remove(&seed);
        while let Some(f) = queue.pop_front() {
            for e in super::face_edges(mesh, f as usize) {
                if let Some(edge) = adj.edge(e.0, e.1) {
                    for nf in &edge.faces {
                        if todo.remove(nf) {
                            queue.push_back(*nf);
                            comp.push(*nf);
                        }
                    }
                }
            }
        }
        out.push(comp);
    }
    out
}

pub fn recalc(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: RecalcParams = parse_params(params)?;
    let adj = Adjacency::build(mesh);
    let scope: BTreeSet<u32> = {
        let s = selected_faces(mesh, &adj, sel);
        if s.is_empty() {
            (0..mesh.faces.len() as u32).collect()
        } else {
            s
        }
    };
    let mut flipped = 0usize;
    for comp in linked_components(mesh, &adj) {
        let comp: Vec<u32> = comp.into_iter().filter(|f| scope.contains(f)).collect();
        if comp.is_empty() {
            continue;
        }
        let mut oriented: BTreeSet<u32> = BTreeSet::new();
        let mut queue: VecDeque<u32> = VecDeque::new();
        oriented.insert(comp[0]);
        queue.push_back(comp[0]);
        while let Some(f) = queue.pop_front() {
            let order = mesh.face_verts(f as usize);
            for k in 0..order.len() {
                let (a, b) = (order[k], order[(k + 1) % order.len()]);
                if let Some(e) = adj.edge(a, b) {
                    for nf in &e.faces {
                        if !scope.contains(nf) || oriented.contains(nf) {
                            continue;
                        }
                        let norder = mesh.face_verts(*nf as usize);
                        let mut same = false;
                        for q in 0..norder.len() {
                            if norder[q] == a && norder[(q + 1) % norder.len()] == b {
                                same = true;
                            }
                        }
                        if same {
                            reverse_face(mesh, *nf as usize);
                            flipped += 1;
                        }
                        oriented.insert(*nf);
                        queue.push_back(*nf);
                    }
                }
            }
        }
        let vol = signed_volume(mesh, &comp);
        if (p.mode == RecalcMode::Outside && vol < 0.0) || (p.mode == RecalcMode::Inside && vol > 0.0) {
            for f in &comp {
                reverse_face(mesh, *f as usize);
                flipped += 1;
            }
        }
    }
    let mut warnings = Vec::new();
    if flipped == 0 {
        warnings.push("every normal already pointed the right way".to_string());
    }
    Ok((Selection { mode: SelMode::Face, ids: scope.into_iter().collect() }, warnings))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

pub fn flip(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let _: NoParams = parse_params(params)?;
    let adj = Adjacency::build(mesh);
    let mut faces = selected_faces(mesh, &adj, sel);
    if faces.is_empty() {
        faces = (0..mesh.faces.len() as u32).collect();
    }
    let ids: Vec<u32> = faces.iter().copied().collect();
    for f in &ids {
        reverse_face(mesh, *f as usize);
    }
    Ok((Selection { mode: SelMode::Face, ids }, Vec::new()))
}

pub fn shade(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: ShadeParams = parse_params(params)?;
    let adj = Adjacency::build(mesh);
    let mut faces = selected_faces(mesh, &adj, sel);
    if faces.is_empty() {
        faces = (0..mesh.faces.len() as u32).collect();
    }
    let ids: Vec<u32> = faces.iter().copied().collect();
    for f in &ids {
        let face = &mut mesh.faces[*f as usize];
        face.flags.set(FaceFlags::SMOOTH, p.mode == ShadeMode::Smooth);
    }
    mesh.clear_custom_normals(&ids);
    Ok((Selection { mode: SelMode::Face, ids }, Vec::new()))
}
