//! join: merge mesh and brush objects into the active mesh, the inverse of
//! separate. Brushes convert through Brush -> Mesh first; materials merge by
//! name; every source bakes its world transform into the active's space.
//! The first id of the CLI verb is the active object.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::super::request::{OpOutput, OpReport, OpRequest};
use crate::brush::Solid;
use crate::command::Command;
use crate::doc::Document;
use crate::editmesh::select::{SelMode, Selection};
    use crate::editmesh::{bad, Adjacency, EditError, EditMesh, Face, FaceFlags};
    use crate::model::{Id, ObjectKind};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinParams {
    with: Vec<Id>,
}

struct Joined {
    command: Command,
    added: Vec<u32>,
    report: OpReport,
}

/// Merge `sources` into `active`: meshes load as-is (glb sources weld on the
/// fly), brushes convert, everything lands in the active's object space.
pub fn cmd_join(doc: &Document, active: Id, sources: &[Id]) -> Result<Command, EditError> {
    Ok(join(doc, active, sources)?.command)
}

pub fn plan_join(doc: &Document, req: &OpRequest) -> Result<OpOutput, EditError> {
    let p: JoinParams =
        serde_json::from_value(req.params.clone()).map_err(|e| bad(format!("bad params: {e}")))?;
    let j = join(doc, req.object, &p.with)?;
    Ok(OpOutput {
        command: j.command,
        selection: Selection { mode: SelMode::Face, ids: j.added },
        report: j.report,
    })
}

fn mesh_source_of(object: &crate::model::Object) -> Result<&crate::model::Mesh, EditError> {
    match &object.kind {
        ObjectKind::Mesh(m) => Ok(m),
        _ => Err(EditError::NotAMesh(object.id)),
    }
}

fn load_object_space(doc: &Document, id: Id) -> Result<EditMesh, EditError> {
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
        ObjectKind::Brush(brush) => brush_to_editmesh(brush),
        _ => Err(bad(format!("object {id} is neither a mesh nor a brush"))),
    }
}

fn brush_to_editmesh(brush: &crate::model::Brush) -> Result<EditMesh, EditError> {
    let solid = Solid::from_faces(&brush.faces).map_err(|e| bad(e.to_string()))?;
    let mut slots: Vec<String> = Vec::new();
    let mut slot_of: BTreeMap<String, u16> = BTreeMap::new();
    for face in &brush.faces {
        if !slot_of.contains_key(&face.material) {
            if slots.len() >= super::super::MAX_MATERIALS {
                return Err(bad("the brush names more material slots than the budget"));
            }
            slot_of.insert(face.material.clone(), slots.len() as u16);
            slots.push(face.material.clone());
        }
    }
    let mut mesh = EditMesh {
        positions: solid.vertices.clone(),
        faces: Vec::with_capacity(solid.loops.len()),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: slots,
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
        mesh.faces.push(Face {
            first,
            len: solid_face.len() as u32,
            material: slot_of[&brush_face.material],
            flags: FaceFlags::empty(),
        });
    }
    Ok(mesh)
}

fn join(doc: &Document, active: Id, sources: &[Id]) -> Result<Joined, EditError> {
    if sources.is_empty() {
        return Err(bad("join needs at least one other object"));
    }
    let mut seen = BTreeSet::new();
    for id in sources {
        if *id == active {
            return Err(bad("join cannot merge an object into itself"));
        }
        if !seen.insert(*id) {
            return Err(bad(format!("join names object {id} twice")));
        }
        let object = doc.object(*id).ok_or(EditError::UnknownObject(*id))?;
        if doc.doc.objects.iter().any(|o| o.parent == Some(*id)) {
            return Err(bad(format!("object {id} has children; unparent them before joining")));
        }
        match &object.kind {
            ObjectKind::Mesh(_) | ObjectKind::Brush(_) => {}
            _ => return Err(bad(format!("object {id} is neither a mesh nor a brush"))),
        }
    }
    let active_object = doc.object(active).ok_or(EditError::UnknownObject(active))?.clone();
    let active_mesh = mesh_source_of(&active_object)?.clone();
    let active_world = doc.world_matrix(active).ok_or_else(|| bad("the active object has no world transform"))?;
    let active_inv = active_world.inverse();
    if !active_inv.is_finite() {
        return Err(bad("the active object transform is not invertible"));
    }
    let mut combined = load_object_space(doc, active)?;
    let first_kept = combined.faces.len() as u32;
    let mut material_map = active_mesh.material_map.clone();
    for id in sources {
        let source_world = doc.world_matrix(*id).ok_or_else(|| bad(format!("object {id} has no world transform")))?;
        let rel = active_inv * source_world;
        if !rel.is_finite() {
            return Err(bad(format!("object {id} transform is not finite")));
        }
        let source_mesh = load_object_space(doc, *id)?;
        let object = doc.object(*id).ok_or(EditError::UnknownObject(*id))?.clone();
        if let ObjectKind::Mesh(mesh) = &object.kind {
            for name in &source_mesh.materials {
                if !material_map.contains_key(name) {
                    material_map.insert(name.clone(), mesh.material_map.get(name).cloned().unwrap_or_else(|| name.clone()));
                }
            }
        } else {
            for name in &source_mesh.materials {
                if !material_map.contains_key(name) {
                    material_map.insert(name.clone(), name.clone());
                }
            }
        }
        super::append_transformed(&mut combined, &source_mesh, rel)?;
    }
    let added: Vec<u32> = (first_kept..combined.faces.len() as u32).collect();
    let adj = Adjacency::build(&combined);
    let report = OpReport {
        verts: combined.positions.len() as u32,
        edges: adj.edges.len() as u32,
        faces: combined.faces.len() as u32,
        warnings: Vec::new(),
    };
    let bytes = super::super::io::write(&combined);
    let active_path =
        if active_mesh.source.ends_with(".r5mesh") { active_mesh.source.clone() } else { format!("meshes/{active}.r5mesh") };
    let mut updated = active_object.clone();
    if let ObjectKind::Mesh(m) = &mut updated.kind {
        m.source = active_path.clone();
        m.material_map = material_map;
    }
    let live: BTreeSet<&str> = doc
        .doc
        .objects
        .iter()
        .filter(|o| o.id == active || !sources.contains(&o.id))
        .filter_map(|o| match &o.kind {
            ObjectKind::Mesh(m) => Some(m.source.as_str()),
            _ => None,
        })
        .collect();
    let mut dead_files: BTreeSet<String> = BTreeSet::new();
    for id in sources {
        if let ObjectKind::Mesh(mesh) = &doc.object(*id).ok_or(EditError::UnknownObject(*id))?.kind {
            if mesh.source != active_path && !live.contains(mesh.source.as_str()) {
                dead_files.insert(mesh.source.clone());
            }
        }
    }
    let mut commands = vec![
        Command::SetFile { path: active_path, bytes: Some(bytes) },
        Command::ReplaceObjects { objects: vec![updated] },
        Command::RemoveObjects { ids: sources.to_vec() },
    ];
    for path in dead_files {
        commands.push(Command::SetFile { path, bytes: None });
    }
    Ok(Joined {
        command: Command::Batch { label: "mesh join".into(), commands },
        added,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Document;
    use crate::editmesh::validate::invariants;
    use crate::model::CollisionMode;

    fn doc() -> (Document, Id) {
        let d = Document::empty();
        let layer = d.layer_ids()[0];
        (d, layer)
    }

    fn mesh_req(spec: crate::meshgen::MeshSpec, material: &str) -> crate::meshgen::MeshRequest {
        crate::meshgen::MeshRequest {
            spec,
            material: material.into(),
            name: None,
            transform: None,
            uv_scale: 1.0 / crate::model::UV_TEXELS_PER_TILE,
            collision: CollisionMode::Triangles,
            editable: false,
        }
    }

    fn add_editable(doc: &mut Document, layer: Id, spec: crate::meshgen::MeshSpec, material: &str) -> Id {
        let cmd = crate::editmesh::convert::cmd_add_editable_mesh(layer, None, &mesh_req(spec, material)).unwrap();
        let (_, ids) = doc.apply(cmd).unwrap();
        ids[0]
    }

    fn add_brush(doc: &mut Document, layer: Id, min: [f64; 3], max: [f64; 3], material: &str) -> Id {
        let cmd = crate::brush::cmd_box(
            layer,
            None,
            glam::DVec3::from_array(min),
            glam::DVec3::from_array(max),
            material,
        )
        .unwrap();
        let (_, ids) = doc.apply(cmd).unwrap();
        ids[0]
    }

    fn set_position(doc: &mut Document, id: Id, p: [f64; 3]) {
        let mut o = doc.object(id).unwrap().clone();
        o.transform.position = glam::DVec3::from_array(p);
        doc.apply(Command::ReplaceObjects { objects: vec![o] }).unwrap();
    }

    fn read_mesh(doc: &Document, id: Id) -> EditMesh {
        let o = doc.object(id).unwrap().clone();
        let ObjectKind::Mesh(m) = o.kind else { panic!("object is not a mesh") };
        let bytes = doc.files.read(&m.source).unwrap();
        crate::editmesh::io::read(&bytes).unwrap()
    }

    fn mesh_objects(doc: &Document) -> Vec<Id> {
        doc.doc.objects.iter().filter(|o| matches!(o.kind, ObjectKind::Mesh(_))).map(|o| o.id).collect()
    }

    #[test]
    fn join_two_meshes_merges_by_name_and_bakes_transforms() {
        let (mut doc, layer) = doc();
        let a = add_editable(&mut doc, layer, crate::meshgen::MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "a");
        let b = add_editable(&mut doc, layer, crate::meshgen::MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "b");
        set_position(&mut doc, b, [256.0, 0.0, 0.0]);
        let (na, nb) = (read_mesh(&doc, a), read_mesh(&doc, b));
        let cmd = cmd_join(&doc, a, &[b]).unwrap();
        doc.apply(cmd).unwrap();
        assert_eq!(mesh_objects(&doc).len(), 1, "one object remains");
        assert!(doc.object(b).is_none(), "the source is gone");
        let merged = read_mesh(&doc, a);
        assert_eq!(merged.positions.len(), na.positions.len() + nb.positions.len());
        assert_eq!(merged.faces.len(), na.faces.len() + nb.faces.len());
        assert_eq!(merged.materials, vec!["a".to_string(), "b".to_string()]);
        assert!(merged.positions.iter().any(|p| p.x > 200.0), "the source baked at x = 256");
        assert!(merged.positions.iter().any(|p| p.x < -30.0), "the active stayed at the origin");
        let inv = invariants(&merged);
        assert!(inv.degenerate.is_empty(), "{inv:?}");
        let o = doc.object(a).unwrap().clone();
        let ObjectKind::Mesh(m) = o.kind else { panic!("a mesh") };
        assert!(m.material_map.contains_key("a") && m.material_map.contains_key("b"));
    }

    #[test]
    fn join_shares_one_slot_for_one_name() {
        let (mut doc, layer) = doc();
        let a = add_editable(&mut doc, layer, crate::meshgen::MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "same");
        let b = add_editable(&mut doc, layer, crate::meshgen::MeshSpec::Box { size: [32.0, 32.0, 32.0] }, "same");
        doc.apply(cmd_join(&doc, a, &[b]).unwrap()).unwrap();
        let merged = read_mesh(&doc, a);
        assert_eq!(merged.materials, vec!["same".to_string()]);
        assert!(merged.faces.iter().all(|f| f.material == 0));
    }

    #[test]
    fn join_brush_converts_first() {
        let (mut doc, layer) = doc();
        let a = add_editable(&mut doc, layer, crate::meshgen::MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "m");
        let before = read_mesh(&doc, a);
        let b = add_brush(&mut doc, layer, [-32.0, -32.0, 0.0], [32.0, 32.0, 64.0], "world/dev/dev_gray_512");
        doc.apply(cmd_join(&doc, a, &[b]).unwrap()).unwrap();
        assert_eq!(mesh_objects(&doc).len(), 1);
        assert!(doc.object(b).is_none(), "the brush is gone");
        let merged = read_mesh(&doc, a);
        assert_eq!(merged.faces.len(), before.faces.len() + 6, "one n-gon per brush face");
        assert!(merged.materials.contains(&"world/dev/dev_gray_512".to_string()));
        let inv = invariants(&merged);
        assert!(inv.wound && inv.degenerate.is_empty(), "{inv:?}");
    }

    #[test]
    fn join_welds_glb_sources_and_repoints_a_glb_active() {
        let (mut doc, layer) = doc();
        let req = mesh_req(crate::meshgen::MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "m");
        let (_, ids) = doc.apply(crate::meshgen::cmd_add_mesh(layer, None, &req).unwrap()).unwrap();
        let active = ids[0];
        let b = add_brush(&mut doc, layer, [128.0, -32.0, 0.0], [192.0, 32.0, 64.0], "m");
        doc.apply(cmd_join(&doc, active, &[b]).unwrap()).unwrap();
        let o = doc.object(active).unwrap().clone();
        let ObjectKind::Mesh(m) = o.kind else { panic!("a mesh") };
        assert!(m.source.ends_with(".r5mesh"), "the active is editable now: {}", m.source);
        let merged = read_mesh(&doc, active);
        assert_eq!(merged.faces.len(), 12 + 6, "welded glb box plus brush");
        assert!(doc.object(b).is_none());
    }

    #[test]
    fn join_reports_through_plan() {
        let (mut doc, layer) = doc();
        let a = add_editable(&mut doc, layer, crate::meshgen::MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "m");
        let b = add_editable(&mut doc, layer, crate::meshgen::MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "m");
        let (na, nb) = (read_mesh(&doc, a).faces.len() as u32, read_mesh(&doc, b).faces.len() as u32);
        let req = OpRequest {
            op: "join".into(),
            object: a,
            sel: crate::editmesh::select::Sel::default(),
            params: serde_json::json!({"with": [b]}),
        };
        let first = crate::editmesh::plan(&doc, &req).unwrap();
        let second = crate::editmesh::plan(&doc, &req).unwrap();
        assert_eq!(first.report.faces, na + nb);
        assert_eq!(first.selection.ids.len(), nb as usize, "the joined faces come selected");
        assert_eq!(first.selection.mode, SelMode::Face);
        let Command::Batch { commands: c1, .. } = &first.command else { panic!("a batch") };
        let Command::Batch { commands: c2, .. } = &second.command else { panic!("a batch") };
        assert_eq!(c1, c2, "planning twice plans the same bytes");
        doc.apply(first.command).unwrap();
        assert_eq!(mesh_objects(&doc).len(), 1);
    }

    #[test]
    fn join_refuses_bad_input() {
        let (mut doc, layer) = doc();
        let a = add_editable(&mut doc, layer, crate::meshgen::MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "m");
        let b = add_editable(&mut doc, layer, crate::meshgen::MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "m");
        assert!(cmd_join(&doc, a, &[]).is_err(), "needs a source");
        assert!(cmd_join(&doc, a, &[a]).is_err(), "no self join");
        assert!(cmd_join(&doc, a, &[b, b]).is_err(), "no double join");
        assert!(cmd_join(&doc, a, &[Id::nil()]).is_err(), "unknown source");
        assert!(cmd_join(&doc, Id::nil(), &[b]).is_err(), "unknown active");
        let brush = add_brush(&mut doc, layer, [0.0, 0.0, 0.0], [16.0, 16.0, 16.0], "m");
        assert!(cmd_join(&doc, brush, &[a]).is_err(), "a brush is not a join target");
        let req = OpRequest {
            op: "join".into(),
            object: a,
            sel: crate::editmesh::select::Sel::default(),
            params: serde_json::json!({"with": "nope"}),
        };
        assert!(crate::editmesh::plan(&doc, &req).is_err(), "params are validated");
    }
}
