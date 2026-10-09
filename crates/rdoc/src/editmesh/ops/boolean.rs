//! The destructive `boolean` op: the active mesh arranged against another
//! object (a Mesh or a Brush) or against its own unselected faces, through
//! the same solver the Boolean modifier evaluates.

use std::collections::BTreeSet;
use std::str::FromStr;

use serde::Deserialize;

use super::super::request::{OpOutput, OpReport, OpRequest};
use super::separate::subset;
use crate::command::Command;
use crate::doc::Document;
use crate::editmesh::select::{SelMode, Selection};
use crate::editmesh::{bad, Adjacency, BoolOp, EditError, EditMesh};
use crate::model::{Id, ObjectKind};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BooleanOpParams {
    #[serde(default)]
    operation: BoolOp,
    operand: String,
    #[serde(default)]
    keep_material: bool,
}

/// A brush operand as an editable mesh: one n-gon per brush face with its
/// `UvMapping` projected into corner UVs.
pub fn mesh_from_brush(brush: &crate::model::Brush) -> Result<EditMesh, EditError> {
    let solid = crate::brush::Solid::from_faces(&brush.faces).map_err(|e| bad(e.to_string()))?;
    let mut slots: Vec<String> = Vec::new();
    for face in &brush.faces {
        if !slots.contains(&face.material) {
            if slots.len() >= super::super::MAX_MATERIALS {
                return Err(bad("the brush names more material slots than the budget"));
            }
            slots.push(face.material.clone());
        }
    }
    let mut mesh = EditMesh {
        positions: solid.vertices.clone(),
        faces: Vec::with_capacity(solid.loops.len()),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: slots.clone(),
        smooth_angle: None,
    };
    for (fi, brush_face) in brush.faces.iter().enumerate() {
        let solid_face = solid.loops.get(fi).ok_or_else(|| bad("the brush has no geometry"))?;
        let first = mesh.corners.len() as u32;
        for v in solid_face {
            let p = solid.vertices[*v as usize];
            let uv = crate::brush::tile_uv(crate::brush::project_uv(&brush_face.uv, brush_face.normal, p));
            mesh.corners.push(crate::editmesh::Corner { vertex: *v, uv: [uv[0] as f32, uv[1] as f32], normal: None });
        }
        mesh.faces.push(crate::editmesh::Face {
            first,
            len: solid_face.len() as u32,
            material: slots.iter().position(|s| s == &brush_face.material).unwrap_or(0) as u16,
            flags: crate::editmesh::FaceFlags::empty(),
        });
    }
    Ok(mesh)
}

pub(crate) fn load_operand(doc: &Document, id: Id) -> Result<EditMesh, EditError> {
    let object = doc.object(id).ok_or(EditError::UnknownObject(id))?.clone();
    match &object.kind {
        ObjectKind::Mesh(mesh) => {
            let bytes = doc.files.read(&mesh.source).map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?;
            match super::super::load_triangles(&bytes, &mesh.source)
                .map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?
            {
                super::super::LoadedMesh::Editable(base) => Ok(base),
                super::super::LoadedMesh::Soup(prims) => super::super::convert::weld_primitives(&prims),
            }
        }
        ObjectKind::Brush(brush) => mesh_from_brush(brush),
        _ => Err(bad(format!("object {id} is neither a mesh nor a brush"))),
    }
}

pub fn plan_boolean(doc: &Document, req: &OpRequest) -> Result<OpOutput, EditError> {
    let p: BooleanOpParams =
        serde_json::from_value(req.params.clone()).map_err(|e| bad(format!("bad params: {e}")))?;
    let object = doc.object(req.object).ok_or(EditError::UnknownObject(req.object))?.clone();
    let ObjectKind::Mesh(mesh) = object.kind.clone() else {
        return Err(EditError::NotAMesh(req.object));
    };
    let bytes = doc.files.read(&mesh.source).map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?;
    let base = match super::super::load_triangles(&bytes, &mesh.source)
        .map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?
    {
        super::super::LoadedMesh::Editable(base) => base,
        super::super::LoadedMesh::Soup(prims) => super::super::convert::weld_primitives(&prims)?,
    };
    let active_world = doc.world_matrix(req.object).ok_or_else(|| bad("the mesh has no world transform"))?;
    let active_inv = active_world.inverse();
    if !active_inv.is_finite() {
        return Err(bad("the mesh transform does not invert"));
    }
    let (a, b, checked) = if p.operand == "unselected" {
        let adj = Adjacency::build(&base);
        let resolved = super::super::select::resolve(&base, &adj, &super::super::request::prepare_sel(doc, req.object, "boolean", &req.sel))?;
        let selected = super::selected_faces(&base, &adj, &resolved);
        if selected.is_empty() {
            return Err(bad("boolean with \"unselected\" needs a selected region"));
        }
        let rest: BTreeSet<u32> = (0..base.faces.len() as u32).filter(|f| !selected.contains(f)).collect();
        if rest.is_empty() {
            return Err(bad("boolean with \"unselected\" needs unselected faces left over"));
        }
        let region = subset(&base, &selected);
        let other = subset(&base, &rest);
        (region, other, false)
    } else {
        let id = Id::from_str(&p.operand).map_err(|_| bad(format!("operand {} is neither an object id nor \"unselected\"", p.operand)))?;
        if id == req.object {
            return Err(bad("the boolean operand is the mesh itself; use \"unselected\" for that"));
        }
        let operand_world = doc.world_matrix(id).ok_or(EditError::UnknownObject(id))?;
        let rel = active_inv * operand_world;
        if !rel.is_finite() {
            return Err(bad("the operand transform is not finite"));
        }
        let operand = load_operand(doc, id)?;
        let mut moved = EditMesh {
            positions: Vec::new(),
            faces: Vec::new(),
            corners: Vec::new(),
            edge_flags: Vec::new(),
            materials: base.materials.clone(),
            smooth_angle: base.smooth_angle,
        };
        super::append_transformed(&mut moved, &operand, rel)?;
        (base.clone(), moved, true)
    };
    let solved = if checked {
        super::super::boolean::solve(&a, &b, p.operation, p.keep_material)?
    } else {
        super::super::boolean::solve_open(&a, &b, p.operation, p.keep_material)?
    };
    let adj = Adjacency::build(&solved);
    let report = OpReport {
        verts: solved.positions.len() as u32,
        edges: adj.edges.len() as u32,
        faces: solved.faces.len() as u32,
        warnings: Vec::new(),
    };
    let path =
        if mesh.source.ends_with(".r5mesh") { mesh.source.clone() } else { format!("meshes/{}.r5mesh", object.id) };
    let mut replaced = object.clone();
    if let ObjectKind::Mesh(m) = &mut replaced.kind {
        m.source.clone_from(&path);
    }
    let selection =
        Selection { mode: SelMode::Face, ids: (0..solved.faces.len() as u32).collect() };
    Ok(OpOutput {
        command: Command::Batch {
            label: "mesh boolean".into(),
            commands: vec![
                Command::SetFile { path, bytes: Some(super::super::io::write(&solved)) },
                Command::ReplaceObjects { objects: vec![replaced] },
            ],
        },
        selection,
        report,
    })
}
