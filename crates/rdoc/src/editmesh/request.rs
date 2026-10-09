//! The op contract: an `OpRequest` JSON (`{op, object, sel, params}`) goes in,
//! a `Command` plus the resulting selection comes out. The UI modal, the redo
//! panel, `r5ed-compile mesh-op` and the control socket all build the same
//! request.

use glam::DMat4;
use serde::{Deserialize, Serialize};

use super::ops::separate::{plan_separate, subset, SeparateMode};
use super::select::{resolve, Sel, SelMode, Selection};
use super::{bad, Adjacency, EditError, EditMesh};
use crate::command::Command;
use crate::doc::Document;
use crate::model::{Id, ObjectKind};

/// One mesh op, as every caller phrases it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpRequest {
    pub op: String,
    pub object: Id,
    #[serde(default)]
    pub sel: Sel,
    #[serde(default = "empty_params")]
    pub params: serde_json::Value,
}

fn empty_params() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

/// The op result: the document change, the selection chains use next, and the
/// counts plus warnings the UI and the CLI report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpReport {
    pub verts: u32,
    pub edges: u32,
    pub faces: u32,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct OpOutput {
    pub command: Command,
    pub selection: Selection,
    pub report: OpReport,
}

fn load_mesh(doc: &Document, id: Id) -> Result<(crate::model::Object, crate::model::Mesh, EditMesh), EditError> {
    let object = doc.object(id).ok_or(EditError::UnknownObject(id))?.clone();
    let ObjectKind::Mesh(mesh) = object.kind.clone() else {
        return Err(EditError::NotAMesh(id));
    };
    let bytes = doc.files.read(&mesh.source).map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?;
    let editable = super::io::read(&bytes).map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?;
    Ok((object, mesh, editable))
}

fn report_of(mesh: &EditMesh, warnings: Vec<String>) -> OpReport {
    let adj = Adjacency::build(mesh);
    OpReport { verts: mesh.positions.len() as u32, edges: adj.edges.len() as u32, faces: mesh.faces.len() as u32, warnings }
}

/// The selection defaults every op path shares: an empty `Sel` means the
/// whole mesh, except for the ops that need an explicit selection.
pub fn prepare_sel(doc: &Document, object: Id, op: &str, sel: &Sel) -> Sel {
    let mut out = sel.clone();
    if out.query.is_none() && out.ids.is_empty() && !matches!(op, "delete" | "merge" | "split" | "separate" | "spin" | "screw" | "add_primitive") {
        out.query = Some(super::select::SelQuery::All);
    }
    box_to_object(doc, object, &mut out);
    out
}

/// A `box` query arrives in world units; the mesh resolves it in its own.
fn box_to_object(doc: &Document, object: Id, sel: &mut Sel) {
    let Some(super::select::SelQuery::Box { min, max }) = &sel.query else { return };
    let world = doc.world_matrix(object).unwrap_or(DMat4::IDENTITY);
    let inv = world.inverse();
    if !inv.is_finite() {
        return;
    }
    let corners = [
        [min[0], min[1], min[2]],
        [min[0], min[1], max[2]],
        [min[0], max[1], min[2]],
        [min[0], max[1], max[2]],
        [max[0], min[1], min[2]],
        [max[0], min[1], max[2]],
        [max[0], max[1], min[2]],
        [max[0], max[1], max[2]],
    ];
    let mut lo = glam::DVec3::splat(f64::INFINITY);
    let mut hi = glam::DVec3::splat(f64::NEG_INFINITY);
    for c in corners {
        let p = inv.transform_point3(glam::DVec3::from_array(c));
        lo = lo.min(p);
        hi = hi.max(p);
    }
    sel.query = Some(super::select::SelQuery::Box { min: lo.to_array(), max: hi.to_array() });
}

/// Plans an op against the document: reads the `.r5mesh`, runs the op, and
/// returns the `Command` (usually a `SetFile` of the `.r5mesh` in a `Batch`)
/// that performs it. Planning mutates nothing.
pub fn plan(doc: &Document, req: &OpRequest) -> Result<OpOutput, EditError> {
    if super::modifiers::is_modifier_op(&req.op) {
        return super::modifiers::plan_modifier(doc, req);
    }
    if req.op == "join" {
        return super::ops::join::plan_join(doc, req);
    }
    if req.op == "boolean" {
        return super::ops::boolean::plan_boolean(doc, req);
    }
    let (object, mesh, mut editable) = load_mesh(doc, req.object)?;
    if req.op != "separate" && editable.faces.is_empty() {
        return Err(bad("the mesh has no faces"));
    }
    let sel = prepare_sel(doc, req.object, &req.op, &req.sel);
    let sel_empty = req.sel.query.is_none() && req.sel.ids.is_empty();
    if sel_empty {
        match req.op.as_str() {
            "delete" | "merge" | "split" => return Err(bad(format!("{} needs a selection", req.op))),
            "separate" => {}
            _ => {}
        }
    }
    if req.op == "separate" {
        return plan_separate_cmd(doc, &object, &mesh, &editable, &sel, &req.params);
    }
    let adj = Adjacency::build(&editable);
    let resolved = resolve(&editable, &adj, &sel)?;
    // World-space UV projections evaluate in world coordinates and restore
    // the object-space positions after, so only the corner UVs change.
    let saved = if wants_world(&req.op, &req.params) {
        let world = doc.world_matrix(req.object).unwrap_or(DMat4::IDENTITY);
        let original = editable.positions.clone();
        for p in &mut editable.positions {
            *p = world.transform_point3(*p);
        }
        Some(original)
    } else {
        None
    };
    let (selection, warnings) = if saved.is_some() {
        let mut params = req.params.clone();
        if let Some(obj) = params.as_object_mut() {
            if req.op == "uv_cylinder" {
                obj.insert("axis".into(), serde_json::Value::String("object_z".into()));
            } else {
                obj.insert("space".into(), serde_json::Value::String("object".into()));
            }
        }
        super::ops::dispatch(&req.op, &mut editable, &resolved, &params)?
    } else {
        super::ops::dispatch(&req.op, &mut editable, &resolved, &req.params)?
    };
    if let Some(original) = saved {
        editable.positions = original;
    }
    let bytes = super::io::write(&editable);
    let report = report_of(&editable, warnings);
    let mut replaced = object.clone();
    let _ = &mesh;
    replaced.kind = ObjectKind::Mesh(mesh);
    let command = Command::Batch {
        label: format!("mesh {}", req.op),
        commands: vec![
            Command::SetFile { path: replaced_mesh_path(&replaced), bytes: Some(bytes) },
            Command::ReplaceObjects { objects: vec![replaced] },
        ],
    };
    Ok(OpOutput { command, selection, report })
}

fn wants_world(op: &str, params: &serde_json::Value) -> bool {
    match op {
        "uv_box" | "uv_planar" => params.get("space").and_then(|v| v.as_str()) == Some("world"),
        "uv_cylinder" => params.get("axis").and_then(|v| v.as_str()) == Some("world_z"),
        _ => false,
    }
}

fn replaced_mesh_path(object: &crate::model::Object) -> String {
    match &object.kind {
        ObjectKind::Mesh(m) => m.source.clone(),
        _ => format!("meshes/{}.r5mesh", object.id),
    }
}

fn plan_separate_cmd(
    _doc: &Document,
    object: &crate::model::Object,
    mesh: &crate::model::Mesh,
    editable: &EditMesh,
    sel: &Sel,
    params: &serde_json::Value,
) -> Result<OpOutput, EditError> {
    #[derive(Deserialize)]
    struct P {
        #[serde(default)]
        mode: SeparateMode,
    }
    let mode = serde_json::from_value::<P>(params.clone()).map_err(|e| bad(format!("bad params: {e}")))?.mode;
    let adj = Adjacency::build(editable);
    let resolved = resolve(editable, &adj, sel)?;
    let face_sel = Sel {
        mode: SelMode::Face,
        ids: super::ops::selected_faces(editable, &adj, &resolved).into_iter().collect(),
        query: None,
    };
    let (kept, parts) = plan_separate(editable, &face_sel, mode)?;
    let mut commands = Vec::new();
    let kept_mesh = subset(editable, &kept);
    commands.push(Command::SetFile {
        path: mesh.source.clone(),
        bytes: Some(super::io::write(&kept_mesh)),
    });
    let mut replaced = object.clone();
    replaced.kind = ObjectKind::Mesh(mesh.clone());
    commands.push(Command::ReplaceObjects { objects: vec![replaced] });
    let mut new_ids = Vec::new();
    let mut new_objects = Vec::new();
    for part in &parts {
        let part_mesh = subset(editable, part);
        let mut material_map: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
        for name in &part_mesh.materials {
            let mapped = mesh.material_map.get(name).cloned().unwrap_or_else(|| name.clone());
            material_map.insert(name.clone(), mapped);
        }
        let mut obj = crate::doc::new_object(format!("{}.part", object.name), object.layer, ObjectKind::Mesh(
            crate::model::Mesh {
                source: String::new(),
                material_map,
                collision: mesh.collision,
                contents: mesh.contents,
                cast_shadows: mesh.cast_shadows,
                lightmap_scale: mesh.lightmap_scale,
                render: mesh.render,
                role: mesh.role,
                modifiers: Vec::new(),
            },
        ));
        obj.parent = object.parent;
        obj.transform = object.transform;
        let path = format!("meshes/{}.r5mesh", obj.id);
        if let ObjectKind::Mesh(m) = &mut obj.kind {
            m.source = path.clone();
        }
        new_ids.push(obj.id);
        commands.push(Command::SetFile { path, bytes: Some(super::io::write(&part_mesh)) });
        new_objects.push(obj);
    }
    commands.push(Command::AddObjects { objects: new_objects });
    let report = report_of(&kept_mesh, vec![]);
    Ok(OpOutput {
        command: Command::Batch { label: "mesh separate".into(), commands },
        selection: Selection { mode: SelMode::Face, ids: new_ids.iter().map(|_| 0).collect() },
        report,
    })
}
