//! merge {center | cursor | collapse | by_distance}: selected vertices to one
//! point, each selected edge collapsed, or a distance weld.

use glam::DVec3;
use serde::{Deserialize, Serialize};

use super::{parse_params, selected_edges, selected_verts, weld, OpResult, clamp_finite};
use crate::editmesh::{bad, Adjacency, EditMesh, WELD_EPS};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMode {
    #[default]
    ByDistance,
    Center,
    Cursor,
    Collapse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default)]
    pub mode: MergeMode,
    #[serde(default = "default_vec")]
    pub cursor: [f64; 3],
    #[serde(default = "default_dist")]
    pub distance: f64,
}

fn default_vec() -> [f64; 3] {
    [0.0; 3]
}

fn default_dist() -> f64 {
    WELD_EPS
}

pub fn run(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    if sel.ids.is_empty() {
        return Err(bad("merge needs a selection"));
    }
    let adj = Adjacency::build(mesh);
    match p.mode {
        MergeMode::Center | MergeMode::Cursor => {
            let verts = selected_verts(mesh, &adj, sel);
            if verts.len() < 2 {
                return Err(bad("merge needs at least two vertices"));
            }
            let target = match p.mode {
                MergeMode::Cursor => {
                    let c = DVec3::new(p.cursor[0], p.cursor[1], p.cursor[2]);
                    if !c.is_finite() {
                        return Err(bad("param cursor is not finite"));
                    }
                    c
                }
                _ => verts.iter().map(|v| mesh.positions[*v as usize]).sum::<DVec3>() / verts.len() as f64,
            };
            for v in &verts {
                mesh.positions[*v as usize] = target;
            }
            weld(mesh, WELD_EPS);
            Ok((Selection { mode: SelMode::Vertex, ids: vec![] }, Vec::new()))
        }
        MergeMode::Collapse => {
            let edges = selected_edges(mesh, &adj, sel);
            if edges.is_empty() {
                return Err(bad("collapse names no edges"));
            }
            for (a, b) in &edges {
                let mid = (mesh.positions[*a as usize] + mesh.positions[*b as usize]) * 0.5;
                mesh.positions[*a as usize] = mid;
                mesh.positions[*b as usize] = mid;
            }
            weld(mesh, WELD_EPS);
            Ok((Selection { mode: SelMode::Edge, ids: vec![] }, Vec::new()))
        }
        MergeMode::ByDistance => {
            let distance = clamp_finite(p.distance, 0.0, 1e6, "distance")?;
            let before = mesh.positions.len();
            weld(mesh, distance.max(1e-12));
            let removed = before - mesh.positions.len();
            let mut warnings = Vec::new();
            if removed == 0 {
                warnings.push("no vertices were within the merge distance".to_string());
            }
            Ok((Selection { mode: sel.mode, ids: vec![] }, warnings))
        }
    }
}
