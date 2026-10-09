//! separate {selected | loose} (new Mesh objects plus files; the document
//! half lives in `request.rs`) and split (duplicate elements apart).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::{compact, parse_params, selected_faces, OpResult};
use crate::editmesh::{bad, Adjacency, EditMesh, Face};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeparateMode {
    #[default]
    Selected,
    Loose,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default)]
    pub mode: SeparateMode,
}

/// Face sets the separator hands back: the faces staying on the object, then
/// one set per new object.
pub fn plan_separate(mesh: &EditMesh, sel: &super::super::select::Sel, mode: SeparateMode) -> Result<(BTreeSet<u32>, Vec<BTreeSet<u32>>), crate::editmesh::EditError> {
    let adj = Adjacency::build(mesh);
    match mode {
        SeparateMode::Selected => {
            if sel.mode != super::super::select::SelMode::Face {
                return Err(bad("separate selected needs face ids"));
            }
            let part: BTreeSet<u32> = sel.ids.iter().copied().filter(|f| (*f as usize) < mesh.faces.len()).collect();
            if part.is_empty() {
                return Err(bad("separate needs selected faces"));
            }
            if part.len() == mesh.faces.len() {
                return Err(bad("separate needs at least one face left behind"));
            }
            let kept: BTreeSet<u32> = (0..mesh.faces.len() as u32).filter(|f| !part.contains(f)).collect();
            Ok((kept, vec![part]))
        }
        SeparateMode::Loose => {
            let mut todo: BTreeSet<u32> = (0..mesh.faces.len() as u32).collect();
            let mut parts = Vec::new();
            while let Some(seed) = todo.iter().next().copied() {
                let mut comp = BTreeSet::from([seed]);
                let mut queue: VecDeque<u32> = VecDeque::from([seed]);
                todo.remove(&seed);
                while let Some(f) = queue.pop_front() {
                    for e in super::face_edges(mesh, f as usize) {
                        if let Some(edge) = adj.edge(e.0, e.1) {
                            for nf in &edge.faces {
                                if todo.remove(nf) {
                                    queue.push_back(*nf);
                                    comp.insert(*nf);
                                }
                            }
                        }
                    }
                }
                parts.push(comp);
            }
            if parts.len() < 2 {
                return Err(bad("the mesh has no loose parts"));
            }
            let kept = parts.remove(0);
            Ok((kept, parts))
        }
    }
}

/// Builds a mesh holding only `faces`, with slots compacted to what it uses.
pub fn subset(mesh: &EditMesh, faces: &BTreeSet<u32>) -> EditMesh {
    let mut slot_map: BTreeMap<u16, u16> = BTreeMap::new();
    for f in faces {
        let s = mesh.faces[*f as usize].material;
        let n = slot_map.len() as u16;
        slot_map.entry(s).or_insert(n);
    }
    let materials: Vec<String> = {
        let mut by_new: Vec<(u16, u16)> = slot_map.iter().map(|(o, n)| (*n, *o)).collect();
        by_new.sort_unstable();
        by_new.into_iter().map(|(_, o)| mesh.materials[o as usize].clone()).collect()
    };
    let mut out = EditMesh {
        positions: mesh.positions.clone(),
        faces: Vec::with_capacity(faces.len()),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials,
        smooth_angle: mesh.smooth_angle,
    };
    for f in faces {
        let face = &mesh.faces[*f as usize];
        let first = out.corners.len() as u32;
        out.corners.extend_from_slice(&mesh.corners[face.first as usize..face.first as usize + face.len as usize]);
        out.faces.push(Face {
            first,
            len: face.len,
            material: slot_map[&face.material],
            flags: face.flags,
        });
    }
    let mut vert_set: BTreeSet<u32> = BTreeSet::new();
    for (a, b, f) in &mesh.edge_flags {
        if vert_set.is_empty() {
            for c in &out.corners {
                vert_set.insert(c.vertex);
            }
        }
        if vert_set.contains(a) && vert_set.contains(b) {
            out.edge_flags.push((*a, *b, *f));
        }
    }
    out.edge_flags.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    compact(&mut out);
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

pub fn split(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let _: NoParams = parse_params(params)?;
    if mesh.positions.len() + mesh.corners.len() > super::super::MAX_VERTICES {
        return Err(bad("the split would pass the vertex budget"));
    }
    let adj = Adjacency::build(mesh);
    let saved_flags = mesh.edge_flags.clone();
    match sel.mode {
        SelMode::Face => {
            let faces = selected_faces(mesh, &adj, sel);
            if faces.is_empty() {
                return Err(bad("split needs a selection"));
            }
            for f in &faces {
                let face = mesh.faces[*f as usize];
                let mut fresh: Vec<u32> = Vec::with_capacity(face.len as usize);
                for c in &mesh.corners[face.first as usize..face.first as usize + face.len as usize] {
                    let v = mesh.positions.len() as u32;
                    mesh.positions.push(mesh.positions[c.vertex as usize]);
                    fresh.push(v);
                }
                for (k, c) in mesh.corners[face.first as usize..face.first as usize + face.len as usize]
                    .iter_mut()
                    .enumerate()
                {
                    c.vertex = fresh[k];
                }
            }
            super::carry_flags(mesh, &saved_flags);
            let ids: Vec<u32> = faces.into_iter().collect();
            Ok((Selection { mode: SelMode::Face, ids }, Vec::new()))
        }
        SelMode::Vertex => {
            let mut verts: Vec<u32> = sel.ids.iter().copied().filter(|v| (*v as usize) < mesh.positions.len()).collect();
            if verts.is_empty() {
                return Err(bad("split needs a selection"));
            }
            verts.sort_unstable();
            for v in verts {
                let users: Vec<u32> = mesh
                    .corners
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.vertex == v)
                    .map(|(k, _)| k as u32)
                    .collect();
                for k in users.iter().skip(1) {
                    let nv = mesh.positions.len() as u32;
                    mesh.positions.push(mesh.positions[v as usize]);
                    mesh.corners[*k as usize].vertex = nv;
                }
            }
            super::carry_flags(mesh, &saved_flags);
            Ok((Selection { mode: SelMode::Vertex, ids: sel.ids.clone() }, Vec::new()))
        }
        SelMode::Edge => {
            let mut edges: Vec<(u32, u32)> =
                sel.ids.iter().filter_map(|id| adj.edges.get(*id as usize).map(|e| (e.a, e.b))).collect();
            if edges.is_empty() {
                return Err(bad("split needs a selection"));
            }
            edges.sort_unstable();
            for (a, b) in edges {
                let edge = adj.edge(a, b).cloned();
                let Some(edge) = edge else { continue };
                for f in edge.faces.iter().skip(1) {
                    let face = mesh.faces[*f as usize];
                    for c in &mut mesh.corners[face.first as usize..face.first as usize + face.len as usize] {
                        if c.vertex == a || c.vertex == b {
                            let nv = mesh.positions.len() as u32;
                            mesh.positions.push(mesh.positions[c.vertex as usize]);
                            c.vertex = nv;
                        }
                    }
                }
            }
            super::carry_flags(mesh, &saved_flags);
            Ok((Selection { mode: SelMode::Edge, ids: sel.ids.clone() }, Vec::new()))
        }
    }
}

