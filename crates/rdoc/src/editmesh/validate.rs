//! Load validation plus the structural invariants the tests assert after
//! every op: closed, consistently wound, Euler characteristic, degenerate faces.

use super::{Adjacency, EditMesh, DEGENERATE_AREA};

/// The structural report one mesh carries: what the gates assert on.
#[derive(Clone, Debug, PartialEq)]
pub struct Invariants {
    pub closed: bool,
    pub wound: bool,
    pub euler: i64,
    pub degenerate: Vec<u32>,
    pub boundary: Vec<(u32, u32)>,
    pub non_manifold: Vec<(u32, u32)>,
    pub winding_errors: Vec<(u32, u32)>,
}

pub fn invariants(mesh: &EditMesh) -> Invariants {
    let adj = Adjacency::build(mesh);
    let mut degenerate = Vec::new();
    for (fi, _) in mesh.faces.iter().enumerate() {
        if mesh.face_area(fi) < DEGENERATE_AREA {
            degenerate.push(fi as u32);
        }
    }
    Invariants {
        closed: adj.is_closed(),
        wound: adj.is_consistently_wound(),
        euler: adj.euler(mesh),
        degenerate,
        boundary: adj.boundary(),
        non_manifold: adj.non_manifold(),
        winding_errors: adj.winding_errors(),
    }
}

/// True when every directed edge has its twin: the `closed()` edge-sum test
/// the `meshgen` tests use, generalized to shared topology.
pub fn closed_edge_sum(mesh: &EditMesh) -> bool {
    use std::collections::HashMap;
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    for f in &mesh.faces {
        let verts: Vec<u32> =
            mesh.corners[f.first as usize..f.first as usize + f.len as usize].iter().map(|c| c.vertex).collect();
        for k in 0..verts.len() {
            let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
            if a == b {
                continue;
            }
            let key = if a < b { (a, b) } else { (b, a) };
            *edges.entry(key).or_default() += if a < b { 1 } else { -1 };
        }
    }
    edges.values().all(|v| *v == 0)
}
