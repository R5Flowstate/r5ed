//! set_material {slot}: repoints faces at another material slot.

use serde::{Deserialize, Serialize};

use super::{parse_params, selected_faces, OpResult};
use crate::editmesh::{bad, Adjacency, EditMesh};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    pub slot: u16,
}

pub fn run(mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    if (p.slot as usize) >= mesh.materials.len() {
        return Err(bad(format!("material slot {} does not exist", p.slot)));
    }
    let adj = Adjacency::build(mesh);
    let mut faces = selected_faces(mesh, &adj, sel);
    if faces.is_empty() {
        faces = (0..mesh.faces.len() as u32).collect();
    }
    let ids: Vec<u32> = faces.iter().copied().collect();
    for f in &ids {
        mesh.faces[*f as usize].material = p.slot;
    }
    Ok((Selection { mode: SelMode::Face, ids }, Vec::new()))
}
