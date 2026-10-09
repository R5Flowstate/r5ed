//! mark {sharp | seam, clear}: edge flag edits over an edge, face or vertex
//! selection.

use serde::{Deserialize, Serialize};

use super::{parse_params, selected_edges, OpResult};
use crate::editmesh::{Adjacency, EdgeFlags, EditMesh};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkKind {
    Sharp,
    Seam,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    pub kind: MarkKind,
    #[serde(default)]
    pub clear: bool,
}

pub fn run(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    let adj = Adjacency::build(mesh);
    let mut edges = selected_edges(mesh, &adj, sel);
    if edges.is_empty() {
        edges = adj.edges.iter().map(|e| (e.a, e.b)).collect();
    }
    let flag = match p.kind {
        MarkKind::Sharp => EdgeFlags::SHARP,
        MarkKind::Seam => EdgeFlags::SEAM,
    };
    let ids: Vec<u32> = edges.iter().map(|(a, b)| adj.index_of(*a, *b)).filter(|i| (*i as usize) < adj.edges.len()).collect();
    for (a, b) in &edges {
        mesh.set_edge_flag(*a, *b, flag, !p.clear);
    }
    Ok((Selection { mode: SelMode::Edge, ids }, Vec::new()))
}
