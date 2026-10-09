//! fill: boundary edge loops become n-gons.

use serde::{Deserialize, Serialize};

use super::{loop_is_closed, order_edge_loops, parse_params, selected_edges, selected_faces, selected_verts, OpResult};
use crate::editmesh::{bad, newell, Adjacency, Corner, EditMesh, Face, FaceFlags};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default)]
    pub slot: Option<u16>,
}

pub fn run(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    let adj = Adjacency::build(mesh);
    let mut edges = selected_edges(mesh, &adj, sel);
    if sel.mode != SelMode::Edge {
        let faces = selected_faces(mesh, &adj, sel);
        let verts = selected_verts(mesh, &adj, sel);
        for e in &adj.edges {
            if e.faces.len() != 1 {
                continue;
            }
            let touches_face = e.faces.iter().any(|f| faces.contains(f));
            let touches_vert = verts.contains(&e.a) || verts.contains(&e.b);
            if touches_face || touches_vert || sel.ids.is_empty() {
                edges.insert((e.a, e.b));
            }
        }
    } else {
        edges.retain(|(a, b)| adj.edge(*a, *b).is_some_and(|e| e.faces.len() == 1));
    }
    if edges.is_empty() {
        return Err(bad("fill needs boundary edges"));
    }
    let slot = match p.slot {
        Some(s) if (s as usize) < mesh.materials.len() => s,
        Some(s) => return Err(bad(format!("material slot {s} does not exist"))),
        None => 0,
    };
    if mesh.materials.is_empty() {
        return Err(bad("the mesh has no material slot to fill with"));
    }
    let loops = order_edge_loops(&edges);
    let mut made = Vec::new();
    let mut warnings = Vec::new();
    for chain in &loops {
        if !loop_is_closed(chain, &edges) {
            warnings.push(format!("an edge chain of {} edges is not closed and was skipped", chain.len()));
            continue;
        }
        let pts: Vec<glam::DVec3> = chain.iter().map(|v| mesh.positions[*v as usize]).collect();
        let n = newell(&pts);
        if n.length() * 0.5 < super::super::DEGENERATE_AREA {
            warnings.push("a degenerate loop was skipped".to_string());
            continue;
        }
        let n = n.normalize();
        let seed = if n.x.abs() < 0.9 { glam::DVec3::X } else { glam::DVec3::Y };
        let u = n.cross(seed).normalize_or(glam::DVec3::X);
        let v = n.cross(u).normalize_or(glam::DVec3::Y);
        let c = pts.iter().copied().sum::<glam::DVec3>() / pts.len() as f64;
        let mut keyed: Vec<(f64, u32)> = chain
            .iter()
            .map(|vert| {
                let d = mesh.positions[*vert as usize] - c;
                (d.dot(v).atan2(d.dot(u)), *vert)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let ordered: Vec<u32> = keyed.into_iter().map(|(_, vert)| vert).collect();
        let ordered = super::orient_loop(mesh, &adj, ordered);
        let first = mesh.corners.len() as u32;
        for vert in &ordered {
            mesh.corners.push(Corner { vertex: *vert, uv: [0.0, 0.0], normal: None });
        }
        mesh.faces.push(Face { first, len: ordered.len() as u32, material: slot, flags: FaceFlags::empty() });
        made.push(mesh.faces.len() as u32 - 1);
    }
    if made.is_empty() && warnings.is_empty() {
        warnings.push("no loop was filled".to_string());
    }
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}
