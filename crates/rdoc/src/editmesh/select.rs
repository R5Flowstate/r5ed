//! Selection sets plus the `sel.query` kinds from the Operator contract.
//! Queries resolve to ids in the core, so the UI and the CLI select identically.

use std::collections::{BTreeSet, VecDeque};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use super::{bad, Adjacency, EditError, EditMesh};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelMode {
    #[default]
    Vertex,
    Edge,
    Face,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub mode: SelMode,
    #[serde(default)]
    pub ids: Vec<u32>,
}

/// A selection as an op request carries it: explicit ids, a query, or both
/// (the union, in the given mode).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sel {
    #[serde(default)]
    pub mode: SelMode,
    #[serde(default)]
    pub ids: Vec<u32>,
    #[serde(default)]
    pub query: Option<SelQuery>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelQuery {
    All,
    None,
    Normal { dir: [f64; 3], #[serde(default = "default_angle")] max_angle_deg: f64 },
    Material { name: String },
    #[serde(alias = "bounds")]
    Box { min: [f64; 3], max: [f64; 3] },
    Boundary,
    NonManifold,
    Loop { edge: u32 },
    Ring { edge: u32 },
    Linked { from: u32 },
    Sharp { #[serde(default = "default_sharp")] min_angle_deg: f64 },
    /// Neighbours of `ids`, in the current mode: what Ctrl+NumpadAdd grows to.
    Grow,
    /// `ids` minus the elements touching something unselected: Ctrl+NumpadSubtract.
    Shrink,
    /// Everything linked to `ids`: what Ctrl+L selects.
    LinkedSel,
}

fn default_angle() -> f64 {
    5.0
}

fn default_sharp() -> f64 {
    30.0
}

fn clamp_angle(v: f64) -> f64 {
    if !v.is_finite() {
        0.0
    } else {
        v.clamp(0.0, 180.0)
    }
}

fn face_cyclic(mesh: &EditMesh, fi: usize) -> Vec<u32> {
    mesh.face_verts(fi)
}

/// The edge opposite `edge` in a quad face, in (min,max) form.
fn opposite_in_quad(cyclic: &[u32], a: u32, b: u32) -> Option<(u32, u32)> {
    if cyclic.len() != 4 {
        return None;
    }
    let pos = |v: u32| cyclic.iter().position(|w| *w == v);
    let (pa, pb) = (pos(a)?, pos(b)?);
    if (pa + 1) % 4 != pb && (pb + 1) % 4 != pa {
        return None;
    }
    let rest: Vec<u32> = cyclic.iter().copied().filter(|w| *w != a && *w != b).collect();
    if rest.len() != 2 {
        return None;
    }
    Some((rest[0].min(rest[1]), rest[0].max(rest[1])))
}

/// Walks an edge loop across quads only, stopping at triangles, n-gons,
/// boundary and non-manifold edges. A closed ring wraps.
pub fn edge_loop(mesh: &EditMesh, adj: &Adjacency, start: (u32, u32)) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = vec![start];
    let mut seen: BTreeSet<(u32, u32)> = BTreeSet::from([start]);
    for dir in [0, 1] {
        let mut current = start;
        loop {
            let e = adj.edge(current.0, current.1);
            let face = match e {
                Some(e) if e.faces.len() == 2 => e.faces[dir as usize % e.faces.len()],
                _ => break,
            };
            let cyclic = face_cyclic(mesh, face as usize);
            let Some(next) = opposite_in_quad(&cyclic, current.0, current.1) else { break };
            if next == start || seen.contains(&next) {
                break;
            }
            let ne = adj.edge(next.0, next.1);
            if ne.is_none_or(|e| e.faces.len() != 2) {
                break;
            }
            seen.insert(next);
            if dir == 0 {
                out.push(next);
            } else {
                out.insert(0, next);
            }
            current = next;
        }
    }
    out
}

/// Walks an edge ring: from each edge, the next edge around the shared face,
/// then across into the neighbour, keeping the same turn.
pub fn edge_ring(mesh: &EditMesh, adj: &Adjacency, start: (u32, u32)) -> Vec<(u32, u32)> {
    let step = |face: u32, a: u32, b: u32| -> Option<((u32, u32), u32)> {
        let cyclic = face_cyclic(mesh, face as usize);
        if cyclic.len() != 4 {
            return None;
        }
        let pb = cyclic.iter().position(|w| *w == b)?;
        let next = cyclic[(pb + 1) % cyclic.len()];
        let e = (b.min(next), b.max(next));
        if e == (a.min(b), a.max(b)) {
            return None;
        }
        Some((e, next))
    };
    let mut out: Vec<(u32, u32)> = vec![start];
    let mut seen: BTreeSet<(u32, u32)> = BTreeSet::from([start]);
    for dir in [0, 1] {
        let (mut a, mut b) = if dir == 0 { start } else { (start.1, start.0) };
        let mut face = match adj.edge(a, b) {
            Some(e) if e.faces.len() == 2 => e.faces[0],
            _ => continue,
        };
        if dir == 1 {
            face = match adj.edge(a, b) {
                Some(e) if e.faces.len() == 2 => e.faces[1],
                _ => continue,
            };
        }
        while let Some((e, head)) = step(face, a, b) {
            if seen.contains(&e) {
                break;
            }
            let neighbour = match adj.edge(e.0, e.1) {
                Some(ne) if ne.faces.len() == 2 => ne.faces.iter().copied().find(|f| *f != face),
                _ => None,
            };
            let Some(nf) = neighbour else { break };
            let cyclic = face_cyclic(mesh, nf as usize);
            if cyclic.len() != 4 {
                break;
            }
            seen.insert(e);
            if dir == 0 {
                out.push(e);
            } else {
                out.insert(0, e);
            }
            let ph = cyclic.iter().position(|w| *w == head).unwrap_or(0);
            let ahead = cyclic[(ph + 1) % cyclic.len()];
            (a, b) = (head, ahead);
            face = nf;
        }
    }
    out
}

fn linked(mesh: &EditMesh, adj: &Adjacency, mode: SelMode, from: u32) -> Result<Vec<u32>, EditError> {
    match mode {
        SelMode::Vertex => {
            if from as usize >= mesh.positions.len() {
                return Err(bad("linked.from names no vertex"));
            }
            let mut seen: BTreeSet<u32> = BTreeSet::from([from]);
            let mut queue: VecDeque<u32> = VecDeque::from([from]);
            while let Some(v) = queue.pop_front() {
                for e in adj.edges.iter().filter(|e| e.a == v || e.b == v) {
                    let w = if e.a == v { e.b } else { e.a };
                    if seen.insert(w) {
                        queue.push_back(w);
                    }
                }
            }
            Ok(seen.into_iter().collect())
        }
        SelMode::Edge => {
            if adj.edges.get(from as usize).is_none() {
                return Err(bad("linked.from names no edge"));
            }
            let at = |v: u32| -> Vec<usize> {
                adj.edges.iter().enumerate().filter(|(_, e)| e.a == v || e.b == v).map(|(i, _)| i).collect()
            };
            let mut seen: BTreeSet<u32> = BTreeSet::from([from]);
            let mut queue: VecDeque<u32> = VecDeque::from([from]);
            while let Some(i) = queue.pop_front() {
                let e = &adj.edges[i as usize];
                for v in [e.a, e.b] {
                    for j in at(v) {
                        if seen.insert(j as u32) {
                            queue.push_back(j as u32);
                        }
                    }
                }
            }
            Ok(seen.into_iter().collect())
        }
        SelMode::Face => {
            if from as usize >= mesh.faces.len() {
                return Err(bad("linked.from names no face"));
            }
            let mut seen: BTreeSet<u32> = BTreeSet::from([from]);
            let mut queue: VecDeque<u32> = VecDeque::from([from]);
            while let Some(f) = queue.pop_front() {
                let verts = mesh.face_verts(f as usize);
                for k in 0..verts.len() {
                    let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
                    if let Some(e) = adj.edge(a, b) {
                        for nf in &e.faces {
                            if seen.insert(*nf) {
                                queue.push_back(*nf);
                            }
                        }
                    }
                }
            }
            Ok(seen.into_iter().collect())
        }
    }
}

/// Neighbours of a set, in the selection's mode.
fn grow(_mesh: &EditMesh, adj: &Adjacency, mode: SelMode, ids: &BTreeSet<u32>) -> Vec<u32> {
    let mut out: BTreeSet<u32> = ids.clone();
    match mode {
        SelMode::Vertex => {
            for e in &adj.edges {
                if ids.contains(&e.a) {
                    out.insert(e.b);
                }
                if ids.contains(&e.b) {
                    out.insert(e.a);
                }
            }
        }
        SelMode::Edge => {
            let mut at: BTreeSet<u32> = BTreeSet::new();
            for id in ids {
                if let Some(e) = adj.edges.get(*id as usize) {
                    at.insert(e.a);
                    at.insert(e.b);
                }
            }
            for (i, e) in adj.edges.iter().enumerate() {
                if at.contains(&e.a) || at.contains(&e.b) {
                    out.insert(i as u32);
                }
            }
        }
        SelMode::Face => {
            for e in &adj.edges {
                if e.faces.iter().any(|f| ids.contains(f)) {
                    out.extend(e.faces.iter().copied());
                }
            }
        }
    }
    out.into_iter().collect()
}

/// The set minus the elements touching something outside it.
fn shrink(_mesh: &EditMesh, adj: &Adjacency, mode: SelMode, ids: &BTreeSet<u32>) -> Vec<u32> {
    let mut boundary: BTreeSet<u32> = BTreeSet::new();
    match mode {
        SelMode::Vertex => {
            for e in &adj.edges {
                if ids.contains(&e.a) != ids.contains(&e.b) {
                    boundary.insert(e.a);
                    boundary.insert(e.b);
                }
            }
        }
        SelMode::Edge => {
            let selected_verts: BTreeSet<u32> = ids
                .iter()
                .filter_map(|id| adj.edges.get(*id as usize))
                .flat_map(|e| [e.a, e.b])
                .collect();
            for (i, e) in adj.edges.iter().enumerate() {
                if !ids.contains(&(i as u32)) && (selected_verts.contains(&e.a) || selected_verts.contains(&e.b)) {
                    for id in ids {
                        if let Some(se) = adj.edges.get(*id as usize) {
                            if se.a == e.a || se.b == e.a || se.a == e.b || se.b == e.b {
                                boundary.insert(*id);
                            }
                        }
                    }
                }
            }
        }
        SelMode::Face => {
            for e in &adj.edges {
                if e.faces.iter().any(|f| ids.contains(f)) && e.faces.iter().any(|f| !ids.contains(f)) {
                    boundary.extend(e.faces.iter().filter(|f| ids.contains(f)).copied());
                }
            }
        }
    }
    boundary.into_iter().collect()
}

fn check_id(n: usize, id: u32, what: &str) -> Result<(), EditError> {
    if (id as usize) < n {
        Ok(())
    } else {
        Err(bad(format!("selection names no {what} {id}")))
    }
}

/// Resolves a `Sel` to sorted, deduplicated element ids.
pub fn resolve(mesh: &EditMesh, adj: &Adjacency, sel: &Sel) -> Result<Selection, EditError> {
    let (nv, ne, nf) = (mesh.positions.len(), adj.edges.len(), mesh.faces.len());
    let mut ids: BTreeSet<u32> = BTreeSet::new();
    for id in &sel.ids {
        match sel.mode {
            SelMode::Vertex => check_id(nv, *id, "vertex")?,
            SelMode::Edge => check_id(ne, *id, "edge")?,
            SelMode::Face => check_id(nf, *id, "face")?,
        }
        ids.insert(*id);
    }
    if let Some(q) = &sel.query {
        match q {
            SelQuery::All => match sel.mode {
                SelMode::Vertex => ids.extend(0..nv as u32),
                SelMode::Edge => ids.extend(0..ne as u32),
                SelMode::Face => ids.extend(0..nf as u32),
            },
            SelQuery::None => {}
            SelQuery::Normal { dir, max_angle_deg } => {
                if sel.mode != SelMode::Face {
                    return Err(bad("a normal query selects faces"));
                }
                let d = DVec3::new(dir[0], dir[1], dir[2]);
                if !d.is_finite() || d == DVec3::ZERO {
                    return Err(bad("a normal query needs a finite direction"));
                }
                let d = d.normalize();
                let limit = clamp_angle(*max_angle_deg).to_radians();
                for fi in 0..nf {
                    let n = mesh.face_normal(fi);
                    if n != DVec3::ZERO && n.dot(d).clamp(-1.0, 1.0).acos() <= limit + 1e-9 {
                        ids.insert(fi as u32);
                    }
                }
            }
            SelQuery::Material { name } => {
                if sel.mode != SelMode::Face {
                    return Err(bad("a material query selects faces"));
                }
                let slot = mesh.materials.iter().position(|m| m == name)
                    .ok_or_else(|| bad(format!("no material slot named {name}")))?;
                for (fi, f) in mesh.faces.iter().enumerate() {
                    if f.material as usize == slot {
                        ids.insert(fi as u32);
                    }
                }
            }
            SelQuery::Box { min, max } => {
                let (lo, hi) = (DVec3::new(min[0], min[1], min[2]), DVec3::new(max[0], max[1], max[2]));
                if !(lo.is_finite() && hi.is_finite()) {
                    return Err(bad("a bounds query needs finite corners"));
                }
                let inside = |p: DVec3| p.cmpge(lo).all() && p.cmple(hi).all();
                match sel.mode {
                    SelMode::Vertex => {
                        for (i, p) in mesh.positions.iter().enumerate() {
                            if inside(*p) {
                                ids.insert(i as u32);
                            }
                        }
                    }
                    SelMode::Edge => {
                        for (i, e) in adj.edges.iter().enumerate() {
                            if inside(mesh.positions[e.a as usize]) && inside(mesh.positions[e.b as usize]) {
                                ids.insert(i as u32);
                            }
                        }
                    }
                    SelMode::Face => {
                        for (fi, _) in mesh.faces.iter().enumerate() {
                            if mesh.face_points(fi).iter().all(|p| inside(*p)) {
                                ids.insert(fi as u32);
                            }
                        }
                    }
                }
            }
            SelQuery::Boundary => {
                if sel.mode != SelMode::Edge {
                    return Err(bad("a boundary query selects edges"));
                }
                for (i, e) in adj.edges.iter().enumerate() {
                    if e.faces.len() == 1 {
                        ids.insert(i as u32);
                    }
                }
            }
            SelQuery::NonManifold => {
                if sel.mode != SelMode::Edge {
                    return Err(bad("a non_manifold query selects edges"));
                }
                for (i, e) in adj.edges.iter().enumerate() {
                    if e.faces.len() > 2 {
                        ids.insert(i as u32);
                    }
                }
            }
            SelQuery::Loop { edge } => {
                if sel.mode != SelMode::Edge {
                    return Err(bad("a loop query selects edges"));
                }
                check_id(ne, *edge, "edge")?;
                let e = &adj.edges[*edge as usize];
                for (a, b) in edge_loop(mesh, adj, (e.a, e.b)) {
                    ids.insert(adj.index_of(a, b));
                }
            }
            SelQuery::Ring { edge } => {
                if sel.mode != SelMode::Edge {
                    return Err(bad("a ring query selects edges"));
                }
                check_id(ne, *edge, "edge")?;
                let e = &adj.edges[*edge as usize];
                for (a, b) in edge_ring(mesh, adj, (e.a, e.b)) {
                    let i = adj.index_of(a, b);
                    ids.insert(i);
                }
            }
            SelQuery::Linked { from } => {
                for id in linked(mesh, adj, sel.mode, *from)? {
                    ids.insert(id);
                }
            }
            SelQuery::LinkedSel => {
                for from in sel.ids.clone() {
                    for id in linked(mesh, adj, sel.mode, from)? {
                        ids.insert(id);
                    }
                }
            }
            SelQuery::Grow => {
                for id in grow(mesh, adj, sel.mode, &ids.clone()) {
                    ids.insert(id);
                }
            }
            SelQuery::Shrink => {
                for id in shrink(mesh, adj, sel.mode, &ids.clone()) {
                    ids.remove(&id);
                }
            }
            SelQuery::Sharp { min_angle_deg } => {
                if sel.mode != SelMode::Edge {
                    return Err(bad("a sharp query selects edges"));
                }
                let limit = clamp_angle(*min_angle_deg).to_radians();
                let normals: Vec<DVec3> = (0..nf).map(|f| mesh.face_normal(f)).collect();
                for (i, e) in adj.edges.iter().enumerate() {
                    if e.faces.len() == 2 {
                        let (a, b) = (normals[e.faces[0] as usize], normals[e.faces[1] as usize]);
                        if a != DVec3::ZERO && b != DVec3::ZERO && a.dot(b).clamp(-1.0, 1.0).acos() > limit {
                            ids.insert(i as u32);
                        }
                    } else if e.faces.len() == 1 {
                        ids.insert(i as u32);
                    }
                }
            }
        }
    }
    Ok(Selection { mode: sel.mode, ids: ids.into_iter().collect() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editmesh::convert::weld_primitives;
    use crate::import::glb::GlbMesh;
    use crate::meshgen::{build, MeshSpec};

    fn welded(spec: &MeshSpec) -> EditMesh {
        let g = build(spec, "t", "m", 1.0 / 512.0).unwrap();
        weld_primitives(&[GlbMesh { colors: Vec::new(),
            name: g.name,
            material: g.material,
            positions: g.positions,
            normals: g.normals,
            uvs: g.uvs,
            indices: g.indices,
        }])
        .unwrap()
    }

    #[test]
    fn normal_query_finds_the_box_top() {
        let m = welded(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
        let adj = Adjacency::build(&m);
        let s = resolve(
            &m,
            &adj,
            &Sel {
                mode: SelMode::Face,
                ids: vec![],
                query: Some(SelQuery::Normal { dir: [0.0, 0.0, 1.0], max_angle_deg: 5.0 }),
            },
        )
        .unwrap();
        assert_eq!(s.ids.len(), 2);
        for id in &s.ids {
            assert!(m.face_normal(*id as usize).z > 0.99);
        }
    }

    #[test]
    fn loop_walks_a_cylinder_ring() {
        let m = welded(&MeshSpec::Cylinder { radius: 32.0, height: 64.0, segments: 12, caps: true });
        let adj = Adjacency::build(&m);
        let vertical = adj.edges.iter().position(|e| e.faces.len() == 2).unwrap() as u32;
        let s = resolve(
            &m,
            &adj,
            &Sel { mode: SelMode::Edge, ids: vec![], query: Some(SelQuery::Loop { edge: vertical }) },
        )
        .unwrap();
        assert!(!s.ids.is_empty());
    }
}
