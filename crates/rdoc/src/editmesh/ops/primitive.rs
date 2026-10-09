//! add_primitive: build every `MeshSpec` shape into the edited mesh at a
//! given transform, selected, so a tower gets a buttress without a new object.
//! The UI passes the 3D cursor as the transform position.

use glam::DMat4;
use serde::{Deserialize, Serialize};

use super::{clamp_finite, parse_params, OpResult};
use crate::editmesh::{bad, EditError, EditMesh};
use crate::editmesh::select::{SelMode, Selection};
use crate::model::{Transform, UV_TEXELS_PER_TILE};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    pub spec: crate::meshgen::MeshSpec,
    #[serde(default)]
    pub transform: Option<Transform>,
    #[serde(default)]
    pub material: Option<String>,
}

fn check_transform(t: &Transform) -> Result<(), EditError> {
    for v in [t.position.x, t.position.y, t.position.z, t.scale.x, t.scale.y, t.scale.z] {
        if !v.is_finite() {
            return Err(bad("add_primitive transform is not finite"));
        }
    }
    if !t.rotation.is_finite() {
        return Err(bad("add_primitive transform is not finite"));
    }
    for v in [t.position.x, t.position.y, t.position.z] {
        clamp_finite(v, -1e6, 1e6, "transform.position")?;
    }
    for v in [t.scale.x, t.scale.y, t.scale.z] {
        clamp_finite(v, -1e4, 1e4, "transform.scale")?;
    }
    Ok(())
}

pub fn run(mesh: &mut EditMesh, _sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    let t = p.transform.unwrap_or_default();
    check_transform(&t)?;
    let slot = match p.material {
        Some(name) => {
            if name.is_empty() || name.len() > super::super::MAX_MATERIAL_NAME {
                return Err(bad("add_primitive material name is not usable"));
            }
            name
        }
        None => mesh.materials.first().cloned().unwrap_or_else(|| crate::model::DEFAULT_MATERIAL.into()),
    };
    let built = super::super::convert::editable_from_spec(&p.spec, "primitive", &slot, 1.0 / UV_TEXELS_PER_TILE)?;
    let matrix = DMat4::from_scale_rotation_translation(t.scale, t.rotation, t.position);
    if !matrix.is_finite() {
        return Err(bad("add_primitive transform is not finite"));
    }
    let first_face = mesh.faces.len() as u32;
    super::append_transformed(mesh, &built, matrix)?;
    super::compact(mesh);
    let dropped = super::drop_degenerate(mesh);
    let mut warnings = Vec::new();
    if dropped > 0 {
        warnings.push(format!("{dropped} primitive faces collapsed and were removed"));
    }
    if mesh.faces.len() as u32 <= first_face {
        return Err(bad("the primitive collapsed to nothing"));
    }
    let made: Vec<u32> = (first_face..mesh.faces.len() as u32).collect();
    Ok((Selection { mode: SelMode::Face, ids: made }, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editmesh::validate::invariants;

    fn empty_mesh() -> EditMesh {
        EditMesh {
            positions: Vec::new(),
            faces: Vec::new(),
            corners: Vec::new(),
            edge_flags: Vec::new(),
            materials: vec!["m".to_string()],
            smooth_angle: None,
        }
    }

    fn no_sel() -> Selection {
        Selection { mode: SelMode::Face, ids: Vec::new() }
    }

    fn add(mesh: &mut EditMesh, params: serde_json::Value) -> (Selection, Vec<String>) {
        run(mesh, &no_sel(), &params).expect("the primitive builds")
    }

    #[test]
    fn every_shape_builds_into_the_mesh_selected() {
        let shapes: Vec<(&str, serde_json::Value)> = vec![
            ("box", serde_json::json!({"shape": "box", "size": [64.0, 64.0, 64.0]})),
            ("cylinder", serde_json::json!({"shape": "cylinder", "radius": 32.0, "height": 64.0, "segments": 12})),
            ("cone", serde_json::json!({"shape": "cone", "radius": 32.0, "height": 64.0, "segments": 12})),
            ("sphere", serde_json::json!({"shape": "sphere", "radius": 32.0, "segments": 12, "rings": 8})),
            ("torus", serde_json::json!({"shape": "torus", "major": 64.0, "minor": 16.0, "segments": 16, "sides": 8})),
            ("plane", serde_json::json!({"shape": "plane", "size": [64.0, 64.0], "divisions": [2, 2]})),
            ("stairs", serde_json::json!({"shape": "stairs", "steps": 4, "run": 128.0, "rise": 64.0, "width": 64.0})),
            ("ramp", serde_json::json!({"shape": "ramp", "length": 128.0, "width": 64.0, "height": 32.0})),
            (
                "arch",
                serde_json::json!({"shape": "arch", "width": 128.0, "height": 160.0, "thickness": 16.0, "depth": 32.0, "segments": 8}),
            ),
            (
                "extrude",
                serde_json::json!({"shape": "extrude", "outline": [[0.0, 0.0], [64.0, 0.0], [64.0, 64.0], [0.0, 64.0]], "height": 32.0}),
            ),
            ("lathe", serde_json::json!({"shape": "lathe", "profile": [[32.0, 0.0], [32.0, 64.0]], "segments": 12})),
            (
                "loft",
                serde_json::json!({"shape": "loft", "section": [[-16.0, 0.0], [16.0, 0.0]], "path": [[0.0, 0.0, 0.0], [0.0, 0.0, 64.0]]}),
            ),
        ];
        for (name, spec) in &shapes {
            let mut m = empty_mesh();
            let (out, _) = add(&mut m, serde_json::json!({"spec": spec}));
            assert!(!m.faces.is_empty(), "{name} built no faces");
            assert_eq!(out.ids.len(), m.faces.len(), "{name} selects what it built");
            assert_eq!(out.mode, SelMode::Face);
            let inv = invariants(&m);
            assert!(inv.degenerate.is_empty(), "{name} left degenerate faces: {inv:?}");
        }
    }

    #[test]
    fn transform_moves_the_primitive_to_the_cursor() {
        let mut m = empty_mesh();
        let (out, _) = add(
            &mut m,
            serde_json::json!({
                "spec": {"shape": "box", "size": [64.0, 64.0, 64.0]},
                "transform": {"position": [100.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0]},
            }),
        );
        assert!(!out.ids.is_empty());
        let (lo, hi) = m.bounds().expect("bounds");
        assert!((lo.x - 68.0).abs() < 1e-6 && (hi.x - 132.0).abs() < 1e-6, "{lo:?} {hi:?}");
        assert_eq!(m.materials, vec!["m".to_string()], "reuses the mesh slot");
    }

    #[test]
    fn second_material_slot_is_added_by_name() {
        let mut m = empty_mesh();
        let (_, _) = add(
            &mut m,
            serde_json::json!({"spec": {"shape": "box", "size": [16.0, 16.0, 16.0]}, "material": "stone"}),
        );
        assert_eq!(m.materials, vec!["m".to_string(), "stone".to_string()]);
        assert!(m.faces.iter().all(|f| f.material == 1));
    }

    #[test]
    fn add_primitive_through_plan_reaches_the_cli_and_wasm_path() {
        let mut doc = crate::doc::Document::empty();
        let layer = doc.layer_ids()[0];
        let req: crate::meshgen::MeshRequest =
            serde_json::from_str(r#"{"spec":{"shape":"box","size":[64,64,64]},"editable":true}"#).unwrap();
        let cmd = crate::editmesh::convert::cmd_add_editable_mesh(layer, None, &req).unwrap();
        let (_, ids) = doc.apply(cmd).unwrap();
        let op = crate::editmesh::OpRequest {
            op: "add_primitive".into(),
            object: ids[0],
            sel: crate::editmesh::select::Sel::default(),
            params: serde_json::json!({"spec": {"shape": "cylinder", "radius": 16.0, "height": 32.0, "segments": 8}}),
        };
        let before = {
            let o = doc.object(ids[0]).unwrap().clone();
            let crate::model::ObjectKind::Mesh(mesh) = o.kind else { panic!("a mesh") };
            let bytes = doc.files.read(&mesh.source).unwrap();
            crate::editmesh::io::read(&bytes).unwrap().faces.len()
        };
        let out = crate::editmesh::plan(&doc, &op).unwrap();
        assert!(out.report.faces > before as u32, "the cylinder landed in the mesh: {:?}", out.report);
        assert_eq!(out.selection.ids.len(), (out.report.faces - before as u32) as usize);
    }

    #[test]
    fn add_primitive_rejects_bad_input() {
        let mut m = empty_mesh();
        assert!(run(&mut m, &no_sel(), &serde_json::json!({"nope": 1})).is_err());
        assert!(
            run(
                &mut m,
                &no_sel(),
                &serde_json::json!({"spec": {"shape": "box", "size": [0.0, 1.0, 1.0]}}),
            )
            .is_err()
        );
        assert!(
            run(
                &mut m,
                &no_sel(),
                &serde_json::json!({"spec": {"shape": "raw", "positions": [], "indices": []}}),
            )
            .is_err()
        );
    }

    #[test]
    fn add_primitive_is_deterministic_and_dispatched() {
        let params = serde_json::json!({"spec": {"shape": "box", "size": [16.0, 16.0, 16.0]}});
        let mut a = empty_mesh();
        let mut b = empty_mesh();
        let _ = run(&mut a, &no_sel(), &params).unwrap();
        let _ = run(&mut b, &no_sel(), &params).unwrap();
        assert_eq!(super::super::super::io::write(&a), super::super::super::io::write(&b));
        let mut m = empty_mesh();
        let (out, _) = super::super::dispatch("add_primitive", &mut m, &no_sel(), &params).unwrap();
        assert_eq!(out.mode, SelMode::Face);
    }
}
