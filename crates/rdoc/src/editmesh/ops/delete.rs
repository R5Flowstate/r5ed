//! delete {verts | edges | faces | only_faces | dissolve}: removal plus the
//! face-region dissolve that merges a selected patch back into n-gons.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{compact, drop_faces, loop_is_closed, order_edge_loops, parse_params, selected_edges, selected_faces, selected_verts, OpResult};
use crate::editmesh::{bad, newell, Adjacency, Corner, EditMesh, DEGENERATE_AREA};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    #[default]
    Faces,
    Verts,
    Edges,
    OnlyFaces,
    Dissolve,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default)]
    pub mode: DeleteMode,
}

pub fn run(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    if sel.ids.is_empty() {
        return Err(bad("delete needs a selection"));
    }
    match p.mode {
        DeleteMode::Faces | DeleteMode::OnlyFaces => {
            let adj = Adjacency::build(mesh);
            let dead = selected_faces(mesh, &adj, sel);
            if dead.is_empty() {
                return Err(bad("delete names no faces"));
            }
            drop_faces(mesh, &dead);
            if p.mode == DeleteMode::Faces {
                compact(mesh);
            }
            Ok((Selection { mode: SelMode::Face, ids: vec![] }, Vec::new()))
        }
        DeleteMode::Edges => {
            let adj = Adjacency::build(mesh);
            let edges = selected_edges(mesh, &adj, sel);
            if edges.is_empty() {
                return Err(bad("delete names no edges"));
            }
            let mut dead = BTreeSet::new();
            for e in &adj.edges {
                if edges.contains(&(e.a, e.b)) {
                    dead.extend(e.faces.iter().copied());
                }
            }
            drop_faces(mesh, &dead);
            compact(mesh);
            Ok((Selection { mode: SelMode::Edge, ids: vec![] }, Vec::new()))
        }
        DeleteMode::Verts => {
            let adj = Adjacency::build(mesh);
            let verts = selected_verts(mesh, &adj, sel);
            if verts.is_empty() {
                return Err(bad("delete names no vertices"));
            }
            let mut dead = BTreeSet::new();
            for (fi, _) in mesh.faces.iter().enumerate() {
                if mesh.face_verts(fi).iter().any(|v| verts.contains(v)) {
                    dead.insert(fi as u32);
                }
            }
            drop_faces(mesh, &dead);
            compact(mesh);
            Ok((Selection { mode: SelMode::Vertex, ids: vec![] }, Vec::new()))
        }
        DeleteMode::Dissolve => dissolve(mesh, sel),
    }
}

/// Merges each edge-connected component of the selected face region into its
/// boundary n-gons. Components whose boundary is not one closed loop stay
/// untouched and are reported.
fn dissolve(mesh: &mut EditMesh, sel: &Selection) -> OpResult {
    let adj = Adjacency::build(mesh);
    let region = selected_faces(mesh, &adj, sel);
    if region.is_empty() {
        return Err(bad("dissolve names no faces"));
    }
    let in_region: BTreeSet<u32> = region.clone();
    let mut components: Vec<BTreeSet<u32>> = Vec::new();
    let mut todo = in_region.clone();
    while let Some(seed) = todo.iter().next().copied() {
        let mut comp = BTreeSet::from([seed]);
        let mut queue = vec![seed];
        todo.remove(&seed);
        while let Some(f) = queue.pop() {
            let verts = mesh.face_verts(f as usize);
            for k in 0..verts.len() {
                let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
                if let Some(e) = adj.edge(a, b) {
                    for nf in &e.faces {
                        if in_region.contains(nf) && comp.insert(*nf) {
                            queue.push(*nf);
                            todo.remove(nf);
                        }
                    }
                }
            }
        }
        components.push(comp);
    }
    let mut warnings = Vec::new();
    let mut replacement: Vec<(Vec<u32>, u16)> = Vec::new();
    let mut dead: BTreeSet<u32> = BTreeSet::new();
    for comp in &components {
        let mut border: BTreeSet<(u32, u32)> = BTreeSet::new();
        for f in comp {
            let verts = mesh.face_verts(*f as usize);
            for k in 0..verts.len() {
                let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
                if a == b {
                    continue;
                }
                let e = adj.edge(a, b);
                let inside = e.map(|e| e.faces.iter().filter(|nf| comp.contains(nf)).count()).unwrap_or(0);
                if inside < 2 {
                    border.insert((a.min(b), a.max(b)));
                }
            }
        }
        let loops = order_edge_loops(&border);
        let closed: Vec<&Vec<u32>> = loops.iter().filter(|l| loop_is_closed(l, &border)).collect();
        let border_edges: usize = border.len();
        let loop_edges: usize = closed.iter().map(|l| l.len()).collect::<Vec<_>>().into_iter().sum();
        if closed.len() != 1 || loop_edges != border_edges {
            warnings.push(format!("a patch of {} faces has no single boundary loop and was left alone", comp.len()));
            continue;
        }
        let chain = closed[0];
        let pts: Vec<glam::DVec3> = chain.iter().map(|v| mesh.positions[*v as usize]).collect();
        let n = newell(&pts);
        if n.length() * 0.5 < DEGENERATE_AREA {
            warnings.push(format!("a patch of {} faces is degenerate and was left alone", comp.len()));
            continue;
        }
        let material = mesh.faces[*comp.iter().next().unwrap() as usize].material;
        dead.extend(comp.iter().copied());
        replacement.push((chain.clone(), material));
    }
    let mut uv_avg: BTreeMap<u32, [f32; 2]> = BTreeMap::new();
    let mut uv_count: BTreeMap<u32, usize> = BTreeMap::new();
    for f in &dead {
        let face = &mesh.faces[*f as usize];
        for c in &mesh.corners[face.first as usize..face.first as usize + face.len as usize] {
            let e = uv_avg.entry(c.vertex).or_insert([0.0, 0.0]);
            e[0] += c.uv[0];
            e[1] += c.uv[1];
            *uv_count.entry(c.vertex).or_insert(0) += 1;
        }
    }
    for (v, uv) in uv_avg.iter_mut() {
        let n = uv_count[v] as f32;
        uv[0] /= n;
        uv[1] /= n;
    }
    drop_faces(mesh, &dead);
    let adj = Adjacency::build(mesh);
    for (chain, material) in &replacement {
        let chain = super::orient_loop(mesh, &adj, chain.clone());
        let first = mesh.corners.len() as u32;
        for v in &chain {
            let uv = uv_avg.get(v).copied().unwrap_or([0.0, 0.0]);
            mesh.corners.push(Corner { vertex: *v, uv, normal: None });
        }
        mesh.faces.push(crate::editmesh::Face {
            first,
            len: chain.len() as u32,
            material: *material,
            flags: crate::editmesh::FaceFlags::empty(),
        });
    }
    compact(mesh);
    Ok((Selection { mode: SelMode::Face, ids: vec![] }, warnings))
}
