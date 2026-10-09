//! Editable polygon meshes (`.r5mesh`): an indexed n-gon mesh with a welded
//! vertex table, per-corner UVs and optional custom split normals.
//!
//! Faces are counter-clockwise seen from outside (glTF convention), so the one
//! CCW->CW flip in `rcomp::mesh_object` stays the only flip. Edges are derived
//! (sorted vertex pairs), never stored, which is what lets non-manifold input
//! through.

use glam::DVec3;
use serde::{Deserialize, Serialize};

pub mod adjacency;
pub mod boolean;
pub mod convert;
pub mod eval;
pub mod io;
pub mod modifiers;
pub mod ops;
pub mod request;
pub mod select;
pub mod session;
pub mod shading;
pub mod validate;

pub use adjacency::Adjacency;
pub use boolean::BoolOp;
pub use convert::{load_triangles, load_triangles_stacked, LoadedMesh};
pub use eval::{evaluate, EvaluatedMesh};
pub use modifiers::{evaluate_stack, EvalContext, Modifier, ModifierKind, StackCache};
pub use request::{plan, OpOutput, OpReport, OpRequest};
pub use session::{Buffers, PickQuery, PickReply, PreviewOutput, Session, SessionCounts};
pub use shading::{brush_outline, select_lines, soup_wire_edges, WireSoup, WIRE_LINE_BUDGET};

/// Refuse to grow an edit mesh past this many vertices.
pub const MAX_VERTICES: usize = 4_000_000;
/// Cap on faces, corners and edge flags each.
pub const MAX_FACES: usize = 4_000_000;
pub const MAX_CORNERS: usize = 16_000_000;
/// Cap on material slots and on one material name.
pub const MAX_MATERIALS: usize = 4096;
pub const MAX_MATERIAL_NAME: usize = 1024;
/// Coordinates must stay inside this magnitude, as for brushes.
pub const MAX_COORD: f64 = 1e6;
/// Positions closer than this weld to one vertex.
pub const WELD_EPS: f64 = 1e-4;
/// A face below this area is degenerate.
pub const DEGENERATE_AREA: f64 = 1e-9;

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("{0}")]
    Bad(String),
    #[error("unknown object {0}")]
    UnknownObject(crate::model::Id),
    #[error("object {0} is not a mesh")]
    NotAMesh(crate::model::Id),
    #[error("file {0} could not be read: {1}")]
    File(String, String),
}

fn bad(message: impl Into<String>) -> EditError {
    EditError::Bad(message.into())
}

/// Sparse per-edge flags, stored sorted by `(min, max)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EdgeFlags(pub u8);

impl EdgeFlags {
    pub const SHARP: u8 = 0x01;
    pub const SEAM: u8 = 0x02;

    pub fn empty() -> Self {
        Self(0)
    }

    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    pub fn set(&mut self, flag: u8, on: bool) {
        if on {
            self.0 |= flag;
        } else {
            self.0 &= !flag;
        }
    }
}

/// Per-face flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FaceFlags(pub u8);

impl FaceFlags {
    pub const SMOOTH: u8 = 0x01;

    pub fn empty() -> Self {
        Self(0)
    }

    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    pub fn set(&mut self, flag: u8, on: bool) {
        if on {
            self.0 |= flag;
        } else {
            self.0 &= !flag;
        }
    }
}

/// One n-gon: a run of `len` corners starting at `first`, CCW from outside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Face {
    pub first: u32,
    pub len: u32,
    pub material: u16,
    pub flags: FaceFlags,
}

/// One face corner: its vertex, its UV in texture tiles, and an optional
/// authored split normal that survives edits of other faces.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Corner {
    pub vertex: u32,
    pub uv: [f32; 2],
    pub normal: Option<[f32; 3]>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EditMesh {
    pub positions: Vec<DVec3>,
    pub faces: Vec<Face>,
    pub corners: Vec<Corner>,
    /// Sparse, sorted by `(min, max)`.
    pub edge_flags: Vec<(u32, u32, EdgeFlags)>,
    /// Slot names; `Mesh.material_map` maps them to game paths.
    pub materials: Vec<String>,
    /// Auto-smooth by angle in degrees; `None` means per-face flags only.
    pub smooth_angle: Option<f64>,
}

impl EditMesh {
    /// Corner indices of a face.
    pub fn face_corners(&self, face: usize) -> &[Corner] {
        let f = &self.faces[face];
        &self.corners[f.first as usize..f.first as usize + f.len as usize]
    }

    /// Vertex ids of a face, in winding order.
    pub fn face_verts(&self, face: usize) -> Vec<u32> {
        self.face_corners(face).iter().map(|c| c.vertex).collect()
    }

    /// Points of a face, in winding order.
    pub fn face_points(&self, face: usize) -> Vec<DVec3> {
        self.face_corners(face).iter().map(|c| self.positions[c.vertex as usize]).collect()
    }

    /// Newell normal of a face (unnormalized: length is twice the area).
    pub fn face_newell(&self, face: usize) -> DVec3 {
        newell(&self.face_points(face))
    }

    /// Unit normal of a face, or zero for a degenerate one.
    pub fn face_normal(&self, face: usize) -> DVec3 {
        self.face_newell(face).normalize_or_zero()
    }

    /// Area of a face.
    pub fn face_area(&self, face: usize) -> f64 {
        self.face_newell(face).length() * 0.5
    }

    /// Flags of an edge pair, or empty when unmarked.
    pub fn edge_flag(&self, a: u32, b: u32) -> EdgeFlags {
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        match self.edge_flags.binary_search_by(|e| (e.0, e.1).cmp(&(a, b))) {
            Ok(i) => self.edge_flags[i].2,
            Err(_) => EdgeFlags::empty(),
        }
    }

    /// Sets or clears one flag on an edge pair, keeping the table sorted.
    pub fn set_edge_flag(&mut self, a: u32, b: u32, flag: u8, on: bool) {
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        match self.edge_flags.binary_search_by(|e| (e.0, e.1).cmp(&(a, b))) {
            Ok(i) => {
                let mut f = self.edge_flags[i].2;
                f.set(flag, on);
                if f.0 == 0 {
                    self.edge_flags.remove(i);
                } else {
                    self.edge_flags[i].2 = f;
                }
            }
            Err(i) => {
                if on {
                    let mut f = EdgeFlags::empty();
                    f.set(flag, true);
                    self.edge_flags.insert(i, (a, b, f));
                }
            }
        }
    }

    /// Drops custom normals on the given faces; their corners recompute.
    pub fn clear_custom_normals(&mut self, faces: &[u32]) {
        for f in faces {
            let face = &self.faces[*f as usize];
            for k in face.first as usize..face.first as usize + face.len as usize {
                self.corners[k].normal = None;
            }
        }
    }

    /// Object-space bounds, or `None` for an empty mesh.
    pub fn bounds(&self) -> Option<(DVec3, DVec3)> {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for p in &self.positions {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        (lo.x <= hi.x).then_some((lo, hi))
    }
}

/// Newell's method: the normal whose length is twice the polygon area.
pub fn newell(pts: &[DVec3]) -> DVec3 {
    let mut n = DVec3::ZERO;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n * 0.5
}

/// Finite and inside the magnitude every other geometry in the project obeys.
pub fn finite_in_range(v: f64) -> bool {
    v.is_finite() && v.abs() <= MAX_COORD
}
