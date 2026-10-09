//! The edit session (decision D5): entering edit mode loads the `EditMesh`
//! into a session; interactive drags mutate it and redraw; release commits
//! one `SetFile` for the `.r5mesh`. Selection and element hiding live in the
//! session, not the document.

use std::collections::BTreeSet;

use glam::{DMat4, DVec3};
use serde::{Deserialize, Serialize};

use super::select::{resolve, Sel, SelMode, Selection};
use super::{bad, eval, io, Adjacency, EditError, EditMesh, OpReport};
use crate::command::Command;
use crate::doc::Document;
use crate::model::{Id, ObjectKind};

/// An open edit session over one editable mesh.
#[derive(Clone, Debug)]
pub struct Session {
    object: Id,
    path: String,
    world: DMat4,
    mesh: EditMesh,
    selection: Selection,
    hidden_verts: BTreeSet<u32>,
    hidden_edges: BTreeSet<u32>,
    hidden_faces: BTreeSet<u32>,
}

impl Session {
    /// Loads the object's `.r5mesh` into a session. A glb-sourced mesh is
    /// refused; the caller offers Make Editable for those.
    pub fn begin(doc: &Document, object: Id) -> Result<Session, EditError> {
        let o = doc.object(object).ok_or(EditError::UnknownObject(object))?.clone();
        let ObjectKind::Mesh(m) = &o.kind else {
            return Err(EditError::NotAMesh(object));
        };
        let bytes = doc.files.read(&m.source).map_err(|e| EditError::File(m.source.clone(), e.to_string()))?;
        let mesh = match super::load_triangles(&bytes, &m.source)
            .map_err(|e| EditError::File(m.source.clone(), e.to_string()))?
        {
            super::LoadedMesh::Editable(mesh) => mesh,
            super::LoadedMesh::Soup(_) => return Err(bad("the mesh is not editable; make it editable first")),
        };
        let world = doc.world_matrix(object).unwrap_or(DMat4::IDENTITY);
        Ok(Session {
            object,
            path: m.source.clone(),
            world,
            mesh,
            selection: Selection::default(),
            hidden_verts: BTreeSet::new(),
            hidden_edges: BTreeSet::new(),
            hidden_faces: BTreeSet::new(),
        })
    }

    pub fn object(&self) -> Id {
        self.object
    }

    pub fn mesh(&self) -> &EditMesh {
        &self.mesh
    }

    pub fn world(&self) -> DMat4 {
        self.world
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Element counts, the shape the control socket reports.
    pub fn counts(&self) -> SessionCounts {
        let adj = Adjacency::build(&self.mesh);
        SessionCounts {
            verts: self.mesh.positions.len() as u32,
            edges: adj.edges.len() as u32,
            faces: self.mesh.faces.len() as u32,
        }
    }

    /// Replaces the session selection. An empty `Sel` clears it.
    pub fn select(&mut self, sel: &Sel) -> Result<Selection, EditError> {
        let adj = Adjacency::build(&self.mesh);
        let resolved = resolve(&self.mesh, &adj, sel)?;
        self.selection = resolved.clone();
        self.prune_selection();
        Ok(self.selection.clone())
    }

    /// Restores an earlier selection, dropping ids the mesh no longer has.
    /// The wasm core uses this to carry the selection across an undo.
    pub fn set_selection(&mut self, sel: Selection) {
        self.selection = sel;
        self.prune_selection();
    }

    /// Drops ids that no longer exist and clears hidden elements from it.
    fn prune_selection(&mut self) {
        let adj = Adjacency::build(&self.mesh);
        let (nv, ne, nf) = (self.mesh.positions.len() as u32, adj.edges.len() as u32, self.mesh.faces.len() as u32);
        let hidden = match self.selection.mode {
            SelMode::Vertex => &self.hidden_verts,
            SelMode::Edge => &self.hidden_edges,
            SelMode::Face => &self.hidden_faces,
        };
        let n = match self.selection.mode {
            SelMode::Vertex => nv,
            SelMode::Edge => ne,
            SelMode::Face => nf,
        };
        self.selection.ids.retain(|id| *id < n && !hidden.contains(id));
    }

    /// Hides the selected elements (Blender `H`); they leave the buffers and
    /// the pick results until revealed. Hiding is session view state: a commit
    /// still writes the whole mesh.
    pub fn hide(&mut self) {
        match self.selection.mode {
            SelMode::Vertex => self.hidden_verts.extend(self.selection.ids.iter().copied()),
            SelMode::Edge => self.hidden_edges.extend(self.selection.ids.iter().copied()),
            SelMode::Face => self.hidden_faces.extend(self.selection.ids.iter().copied()),
        }
        self.selection.ids.clear();
    }

    /// Reveals every hidden element (Blender `Alt+H`).
    pub fn reveal(&mut self) {
        self.hidden_verts.clear();
        self.hidden_edges.clear();
        self.hidden_faces.clear();
    }

    /// Runs one op request against the session mesh without producing a
    /// `Command`: the shape a drag previews with. Selection and id defaults
    /// match `plan`, so a preview then a commit writes the same bytes as the
    /// direct `meshOp` path.
    pub fn preview(&mut self, doc: &Document, req: &super::OpRequest) -> Result<PreviewOutput, EditError> {
        if req.object != self.object {
            return Err(bad("the op names another object than the session"));
        }
        if req.op == "separate" || req.op == "join" {
            return Err(bad("separate and join change objects; they commit directly, never preview"));
        }
        if self.mesh.faces.is_empty() {
            return Err(bad("the mesh has no faces"));
        }
        let sel = super::request::prepare_sel(doc, self.object, &req.op, &req.sel);
        let adj = Adjacency::build(&self.mesh);
        let resolved = resolve(&self.mesh, &adj, &sel)?;
        let (selection, warnings) = super::ops::dispatch(&req.op, &mut self.mesh, &resolved, &req.params)?;
        self.selection = selection.clone();
        self.prune_selection();
        self.world = doc.world_matrix(self.object).unwrap_or(DMat4::IDENTITY);
        let report = OpReport {
            verts: self.mesh.positions.len() as u32,
            edges: Adjacency::build(&self.mesh).edges.len() as u32,
            faces: self.mesh.faces.len() as u32,
            warnings,
        };
        Ok(PreviewOutput { selection: self.selection.clone(), report })
    }

    /// The one `SetFile` of the `.r5mesh` a drag release applies: a single
    /// undo step whatever the drag previewed.
    pub fn commit(&self) -> Command {
        Command::SetFile { path: self.path.clone(), bytes: Some(io::write(&self.mesh)) }
    }

    /// Draw buffers over the session mesh: render positions, triangle indices,
    /// the polygon edge list, the vertex table, face centres and per-element
    /// selection masks. Hidden elements are left out of the drawn lists.
    pub fn buffers(&self) -> Buffers {
        let mut shown = self.mesh.clone();
        let dead: BTreeSet<u32> = self.hidden_faces.iter().copied().collect();
        super::ops::drop_faces(&mut shown, &dead);
        let eval = eval::evaluate(&shown);
        let adj = Adjacency::build(&self.mesh);
        let to_world = |p: DVec3| self.world.transform_point3(p);
        let verts: Vec<[f32; 3]> =
            self.mesh.positions.iter().map(|p| { let w = to_world(*p); [w.x as f32, w.y as f32, w.z as f32] }).collect();
        let positions: Vec<[f32; 3]> = eval
            .positions
            .iter()
            .map(|p| {
                let w = self.world.transform_point3(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64));
                [w.x as f32, w.y as f32, w.z as f32]
            })
            .collect();
        let mut poly_edges = Vec::with_capacity(adj.edges.len());
        let mut edge_flags = Vec::with_capacity(adj.edges.len());
        for (i, e) in adj.edges.iter().enumerate() {
            if !self.hidden_edges.contains(&(i as u32)) {
                poly_edges.push([e.a, e.b]);
                edge_flags.push(self.mesh.edge_flag(e.a, e.b).0);
            }
        }
        let mut face_centres = Vec::with_capacity(self.mesh.faces.len());
        for fi in 0..self.mesh.faces.len() {
            let pts = self.mesh.face_points(fi);
            let c = pts.iter().copied().sum::<DVec3>() / pts.len().max(1) as f64;
            let w = to_world(c);
            face_centres.push([w.x as f32, w.y as f32, w.z as f32]);
        }
        let (nv, ne, nf) = (self.mesh.positions.len(), adj.edges.len(), self.mesh.faces.len());
        let mut vert_sel = vec![0u8; nv];
        let mut edge_sel = vec![0u8; ne];
        let mut face_sel = vec![0u8; nf];
        match self.selection.mode {
            SelMode::Vertex => {
                for id in &self.selection.ids {
                    if (*id as usize) < nv {
                        vert_sel[*id as usize] = 1;
                    }
                }
            }
            SelMode::Edge => {
                for id in &self.selection.ids {
                    if (*id as usize) < ne {
                        edge_sel[*id as usize] = 1;
                    }
                }
            }
            SelMode::Face => {
                for id in &self.selection.ids {
                    if (*id as usize) < nf {
                        face_sel[*id as usize] = 1;
                    }
                }
            }
        }
        let topo_hash = topo_hash(&self.mesh, &self.hidden_verts, &self.hidden_edges, &self.hidden_faces);
        Buffers {
            verts,
            positions,
            normals: eval.normals,
            uvs: eval.uvs,
            tris: eval.indices,
            poly_edges,
            face_centres,
            vert_sel,
            edge_sel,
            face_sel,
            edge_flags,
            counts: [nv as u32, ne as u32, nf as u32],
            topo_hash,
        }
    }

    /// CPU pick against the session mesh. Vertices and edges match by
    /// projected pixel distance, faces by ray; box select uses projected
    /// positions. Unless `xray` is set, elements occluded by the mesh itself
    /// are excluded by a ray test against the evaluated triangles.
    pub fn pick(&self, query: &PickQuery) -> Result<PickReply, EditError> {
        query.validate()?;
        let world_of = |p: DVec3| self.world.transform_point3(p);
        let tris: Vec<(u32, [DVec3; 3])> =
            eval::face_triangles(&self.mesh).into_iter().map(|(f, t)| (f, [world_of(t[0]), world_of(t[1]), world_of(t[2])])).collect();
        let eye = DVec3::from_array(query.eye);
        if let Some(rect) = query.rect {
            return self.pick_box(query, &tris, eye, rect);
        }
        match query.mode {
            SelMode::Vertex => {
                let mut best: Option<(u32, f64)> = None;
                for (i, p) in self.mesh.positions.iter().enumerate() {
                    if self.hidden_verts.contains(&(i as u32)) {
                        continue;
                    }
                    let w = world_of(*p);
                    let Some(s) = project(query, w) else { continue };
                    let d = ((s[0] - query.x).powi(2) + (s[1] - query.y).powi(2)).sqrt();
                    if d <= query.threshold_px && best.is_none_or(|b| d < b.1) {
                        if !query.xray && occluded(&tris, eye, w) {
                            continue;
                        }
                        best = Some((i as u32, d));
                    }
                }
                Ok(match best {
                    Some((id, _)) => PickReply::Vertex { id, distance: (world_of(self.mesh.positions[id as usize]) - eye).length() },
                    None => PickReply::None,
                })
            }
            SelMode::Edge => {
                let adj = Adjacency::build(&self.mesh);
                let mut best: Option<(u32, f64)> = None;
                for (i, e) in adj.edges.iter().enumerate() {
                    if self.hidden_edges.contains(&(i as u32)) {
                        continue;
                    }
                    let (a, b) = (world_of(self.mesh.positions[e.a as usize]), world_of(self.mesh.positions[e.b as usize]));
                    let (Some(sa), Some(sb)) = (project(query, a), project(query, b)) else { continue };
                    let d = seg_dist(sa, sb, [query.x, query.y]);
                    if d <= query.threshold_px && best.is_none_or(|b| d < b.1) {
                        if !query.xray && occluded(&tris, eye, (a + b) * 0.5) {
                            continue;
                        }
                        best = Some((i as u32, d));
                    }
                }
                Ok(match best {
                    Some((id, _)) => {
                        let e = &adj.edges[id as usize];
                        let mid =
                            (world_of(self.mesh.positions[e.a as usize]) + world_of(self.mesh.positions[e.b as usize])) * 0.5;
                        PickReply::Edge { id, distance: (mid - eye).length() }
                    }
                    None => PickReply::None,
                })
            }
            SelMode::Face => {
                let origin = DVec3::from_array(query.ray_origin);
                let dir = DVec3::from_array(query.ray_dir).normalize();
                let mut best: Option<(u32, f64)> = None;
                for (f, t) in &tris {
                    if self.hidden_faces.contains(f) {
                        continue;
                    }
                    if let Some(d) = ray_tri(origin, dir, t) {
                        if best.is_none_or(|b| d < b.1) {
                            best = Some((*f, d));
                        }
                    }
                }
                Ok(match best {
                    Some((id, distance)) => PickReply::Face { id, distance },
                    None => PickReply::None,
                })
            }
        }
    }

    fn pick_box(&self, query: &PickQuery, tris: &[(u32, [DVec3; 3])], eye: DVec3, rect: [f64; 4]) -> Result<PickReply, EditError> {
        let (x0, y0, x1, y1) = (rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3]));
        let inside = |s: [f64; 2]| s[0] >= x0 && s[0] <= x1 && s[1] >= y0 && s[1] <= y1;
        let mut verts: BTreeSet<u32> = BTreeSet::new();
        for (i, p) in self.mesh.positions.iter().enumerate() {
            if self.hidden_verts.contains(&(i as u32)) {
                continue;
            }
            let w = self.world.transform_point3(*p);
            let Some(s) = project(query, w) else { continue };
            if inside(s) && (query.xray || !occluded(tris, eye, w)) {
                verts.insert(i as u32);
            }
        }
        let adj = Adjacency::build(&self.mesh);
        let mut ids: Vec<u32> = match query.mode {
            SelMode::Vertex => verts.into_iter().collect(),
            SelMode::Edge => adj
                .edges
                .iter()
                .enumerate()
                .filter(|(i, e)| {
                    !self.hidden_edges.contains(&(*i as u32)) && verts.contains(&e.a) && verts.contains(&e.b)
                })
                .map(|(i, _)| i as u32)
                .collect(),
            SelMode::Face => (0..self.mesh.faces.len())
                .filter(|fi| {
                    !self.hidden_faces.contains(&(*fi as u32))
                        && self.mesh.face_verts(*fi).iter().all(|v| verts.contains(v))
                })
                .map(|fi| fi as u32)
                .collect(),
        };
        ids.sort_unstable();
        Ok(PickReply::Elements { mode: query.mode, ids })
    }
}

/// Element counts of the session mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCounts {
    pub verts: u32,
    pub edges: u32,
    pub faces: u32,
}

/// A previewed op: the resulting selection and report, without a command.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviewOutput {
    pub selection: Selection,
    pub report: OpReport,
}

/// Draw buffers over the session mesh, in world space.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Buffers {
    /// Welded vertex table.
    pub verts: Vec<[f32; 3]>,
    /// Per-corner render positions of the evaluated triangles.
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// Indices into `positions`.
    pub tris: Vec<u32>,
    /// Polygon edges (never triangulation diagonals) as vertex-table pairs.
    pub poly_edges: Vec<[u32; 2]>,
    pub face_centres: Vec<[f32; 3]>,
    pub vert_sel: Vec<u8>,
    pub edge_sel: Vec<u8>,
    pub face_sel: Vec<u8>,
    /// One byte per adjacency edge: bit 0 SHARP, bit 1 SEAM.
    pub edge_flags: Vec<u8>,
    /// Vertex, edge and face counts.
    pub counts: [u32; 3],
    /// Changes exactly when the drawn topology does; drags that only move
    /// positions keep it, so the overlay reuses its line buffers.
    pub topo_hash: u64,
}

/// One pick query: a point or a screen rect, with the camera to project
/// through and the ray for faces. `xray` includes occluded elements (the M3
/// toggle feeds this; it defaults to off).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PickQuery {
    pub mode: SelMode,
    pub x: f64,
    pub y: f64,
    pub eye: [f64; 3],
    /// Column-major view-projection matrix.
    pub view_proj: [f64; 16],
    pub width: f64,
    pub height: f64,
    pub ray_origin: [f64; 3],
    pub ray_dir: [f64; 3],
    #[serde(default)]
    pub xray: bool,
    #[serde(default = "default_threshold")]
    pub threshold_px: f64,
    /// Screen-rect select `[x0, y0, x1, y1]`; a point pick when absent.
    #[serde(default)]
    pub rect: Option<[f64; 4]>,
}

fn default_threshold() -> f64 {
    12.0
}

impl PickQuery {
    fn validate(&self) -> Result<(), EditError> {
        for v in [self.x, self.y, self.width, self.height, self.threshold_px] {
            if !v.is_finite() {
                return Err(bad("the pick query carries a non-finite number"));
            }
        }
        if !(1.0..=4096.0).contains(&self.width) || !(1.0..=4096.0).contains(&self.height) {
            return Err(bad("the pick viewport is outside 1..4096"));
        }
        if !(0.0..=512.0).contains(&self.threshold_px) {
            return Err(bad("the pick threshold is outside 0..512"));
        }
        for v in self.eye.iter().chain(self.ray_origin.iter()).chain(self.ray_dir.iter()).chain(self.view_proj.iter()) {
            if !v.is_finite() {
                return Err(bad("the pick query carries a non-finite number"));
            }
        }
        if DVec3::from_array(self.ray_dir) == DVec3::ZERO {
            return Err(bad("the pick ray has no direction"));
        }
        if let Some(r) = self.rect {
            if r.iter().any(|v| !v.is_finite()) {
                return Err(bad("the pick rect is not finite"));
            }
        }
        Ok(())
    }
}

/// One pick answer: a single element, a rect's element list, or nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PickReply {
    Vertex { id: u32, distance: f64 },
    Edge { id: u32, distance: f64 },
    Face { id: u32, distance: f64 },
    Elements { mode: SelMode, ids: Vec<u32> },
    None,
}

/// Projects a world point to CSS pixels. `None` when behind the camera.
fn project(q: &PickQuery, p: DVec3) -> Option<[f64; 2]> {
    let m = &q.view_proj;
    let cx = m[0] * p.x + m[4] * p.y + m[8] * p.z + m[12];
    let cy = m[1] * p.x + m[5] * p.y + m[9] * p.z + m[13];
    let cw = m[3] * p.x + m[7] * p.y + m[11] * p.z + m[15];
    if cw <= 1e-9 {
        return None;
    }
    Some([(cx / cw * 0.5 + 0.5) * q.width, (0.5 - cy / cw * 0.5) * q.height])
}

fn seg_dist(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len2 <= 0.0 { 0.0 } else { (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0) };
    let dx = p[0] - (a[0] + ab[0] * t);
    let dy = p[1] - (a[1] + ab[1] * t);
    (dx * dx + dy * dy).sqrt()
}

/// Moller-Trumbore, double sided, in f64 plain arithmetic.
fn ray_tri(origin: DVec3, dir: DVec3, t: &[DVec3; 3]) -> Option<f64> {
    let e1 = t[1] - t[0];
    let e2 = t[2] - t[0];
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-18 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - t[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let dist = e2.dot(q) * inv;
    (dist > 0.0).then_some(dist)
}

/// True when the mesh covers `p` seen from `eye`.
fn occluded(tris: &[(u32, [DVec3; 3])], eye: DVec3, p: DVec3) -> bool {
    let to = p - eye;
    let dist = to.length();
    if dist <= 1e-9 {
        return false;
    }
    let dir = to / dist;
    let eps = (dist * 1e-4).max(1e-6);
    tris.iter().any(|(_, t)| ray_tri(eye, dir, t).is_some_and(|d| d < dist - eps))
}

fn fnv(out: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *out ^= *b as u64;
        *out = out.wrapping_mul(0x0100_0000_01B3);
    }
}

/// Content hash of everything the overlay draws from, so a positions-only
/// drag keeps it: counts, face runs, corner vertices, edge flags and the
/// hidden sets, all in stored order.
fn topo_hash(mesh: &EditMesh, hv: &BTreeSet<u32>, he: &BTreeSet<u32>, hf: &BTreeSet<u32>) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    fnv(&mut h, &(mesh.positions.len() as u64).to_le_bytes());
    fnv(&mut h, &(mesh.faces.len() as u64).to_le_bytes());
    for f in &mesh.faces {
        fnv(&mut h, &f.first.to_le_bytes());
        fnv(&mut h, &f.len.to_le_bytes());
        fnv(&mut h, &f.material.to_le_bytes());
        fnv(&mut h, &[f.flags.0]);
    }
    for c in &mesh.corners {
        fnv(&mut h, &c.vertex.to_le_bytes());
    }
    for (a, b, f) in &mesh.edge_flags {
        fnv(&mut h, &a.to_le_bytes());
        fnv(&mut h, &b.to_le_bytes());
        fnv(&mut h, &[f.0]);
    }
    for id in hv.iter().chain(he.iter()).chain(hf.iter()) {
        fnv(&mut h, &id.to_le_bytes());
    }
    h
}
