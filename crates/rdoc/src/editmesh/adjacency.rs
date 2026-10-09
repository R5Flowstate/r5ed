//! Derived topology: the edge table (sorted `(min, max)` pairs with their
//! faces), vertex->faces, and the boundary / non-manifold / winding queries
//! the validators and the ops build on.

use std::collections::BTreeMap;

use super::EditMesh;

/// One undirected edge and the faces around it, in face order.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub a: u32,
    pub b: u32,
    pub faces: Vec<u32>,
    /// Per face: true when the face traverses a->b.
    pub dirs: Vec<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct Adjacency {
    pub edges: Vec<Edge>,
    index: BTreeMap<(u32, u32), usize>,
    pub vertex_faces: Vec<Vec<u32>>,
}

impl Adjacency {
    pub fn build(mesh: &EditMesh) -> Self {
        let mut order: BTreeMap<(u32, u32), Vec<(u32, bool)>> = BTreeMap::new();
        for (fi, f) in mesh.faces.iter().enumerate() {
            let verts: Vec<u32> = mesh.corners[f.first as usize..f.first as usize + f.len as usize]
                .iter()
                .map(|c| c.vertex)
                .collect();
            for k in 0..verts.len() {
                let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
                if a == b {
                    continue;
                }
                let (key, dir) = if a < b { ((a, b), true) } else { ((b, a), false) };
                order.entry(key).or_default().push((fi as u32, dir));
            }
        }
        let mut edges = Vec::with_capacity(order.len());
        let mut index = BTreeMap::new();
        for ((a, b), list) in order {
            index.insert((a, b), edges.len());
            let (faces, dirs) = list.into_iter().unzip();
            edges.push(Edge { a, b, faces, dirs });
        }
        let mut vertex_faces: Vec<Vec<u32>> = vec![Vec::new(); mesh.positions.len()];
        for (fi, f) in mesh.faces.iter().enumerate() {
            for c in &mesh.corners[f.first as usize..f.first as usize + f.len as usize] {
                let list = &mut vertex_faces[c.vertex as usize];
                if list.last() != Some(&(fi as u32)) {
                    list.push(fi as u32);
                }
            }
        }
        Self { edges, index, vertex_faces }
    }

    pub fn edge(&self, a: u32, b: u32) -> Option<&Edge> {
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        self.index.get(&(a, b)).map(|i| &self.edges[*i])
    }

    pub fn index_of(&self, a: u32, b: u32) -> u32 {
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        self.index.get(&(a, b)).copied().unwrap_or(usize::MAX) as u32
    }

    /// Edges with exactly one face.
    pub fn boundary(&self) -> Vec<(u32, u32)> {
        self.edges.iter().filter(|e| e.faces.len() == 1).map(|e| (e.a, e.b)).collect()
    }

    /// Edges with more than two faces.
    pub fn non_manifold(&self) -> Vec<(u32, u32)> {
        self.edges.iter().filter(|e| e.faces.len() > 2).map(|e| (e.a, e.b)).collect()
    }

    /// Edges with exactly two faces that traverse them the same way: one of
    /// the two faces is wound backwards.
    pub fn winding_errors(&self) -> Vec<(u32, u32)> {
        self.edges.iter().filter(|e| e.faces.len() == 2 && e.dirs[0] == e.dirs[1]).map(|e| (e.a, e.b)).collect()
    }

    /// No boundary and no non-manifold edges.
    pub fn is_closed(&self) -> bool {
        self.edges.iter().all(|e| e.faces.len() == 2)
    }

    /// Every shared edge is traversed once each way.
    pub fn is_consistently_wound(&self) -> bool {
        self.edges.iter().all(|e| e.faces.len() != 2 || e.dirs[0] != e.dirs[1])
    }

    /// V - E + F over referenced vertices, so orphan vertices left behind
    /// by an edit do not move the characteristic.
    pub fn euler(&self, mesh: &EditMesh) -> i64 {
        let mut used = vec![false; mesh.positions.len()];
        for c in &mesh.corners {
            used[c.vertex as usize] = true;
        }
        used.iter().filter(|u| **u).count() as i64 - self.edges.len() as i64 + mesh.faces.len() as i64
    }

    /// Faces around a vertex, in face order.
    pub fn faces_of(&self, v: u32) -> &[u32] {
        self.vertex_faces.get(v as usize).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editmesh::convert::weld_primitives;
    use crate::meshgen::{build, MeshSpec};

    fn box_mesh() -> EditMesh {
        let g = build(&MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "b", "m", 1.0 / 512.0).unwrap();
        weld_primitives(&[crate::import::glb::GlbMesh { colors: Vec::new(),
            name: g.name.clone(),
            material: g.material.clone(),
            positions: g.positions.clone(),
            normals: g.normals.clone(),
            uvs: g.uvs.clone(),
            indices: g.indices.clone(),
        }])
        .unwrap()
    }

    #[test]
    fn box_edges_are_all_manifold_pairs() {
        let m = box_mesh();
        let a = Adjacency::build(&m);
        assert_eq!(a.edges.len(), 18);
        assert!(a.is_closed());
        assert!(a.is_consistently_wound());
        assert_eq!(a.euler(&m), 2);
    }
}
