//! M10 gates: the arrangement solver, the destructive op, the Boolean
//! modifier, cutter handling, budgets, determinism and the transform fuzz.

use glam::{DMat4, DQuat, DVec3};
use rdoc::command::Command;
use rdoc::doc::Document;
use rdoc::editmesh::{self, BoolOp, EditMesh, EvalContext, Modifier, ModifierKind};
use rdoc::meshgen::{build, MeshSpec};
use rdoc::model::{Id, ObjectKind};

fn prim(spec: &MeshSpec) -> EditMesh {
    let g = build(spec, "t", "m", 1.0 / 512.0).unwrap();
    let mut mesh = editmesh::convert::weld_primitives(&[rdoc::import::glb::GlbMesh { colors: Vec::new(),
        name: g.name,
        material: g.material,
        positions: g.positions,
        normals: g.normals,
        uvs: g.uvs,
        indices: g.indices,
    }])
    .unwrap();
    for c in &mut mesh.corners {
        c.normal = None;
    }
    mesh
}

fn transformed(mesh: &EditMesh, m: DMat4) -> EditMesh {
    let mut out = mesh.clone();
    for p in &mut out.positions {
        *p = m.transform_point3(*p);
    }
    out
}

fn box_at(center: DVec3, size: f64) -> EditMesh {
    transformed(&prim(&MeshSpec::Box { size: [size, size, size] }), DMat4::from_translation(center))
}

fn invariants(mesh: &EditMesh) -> editmesh::validate::Invariants {
    editmesh::validate::invariants(mesh)
}

fn assert_solid(mesh: &EditMesh, euler: i64) {
    let inv = invariants(mesh);
    assert!(inv.closed, "open: {:?}", inv.boundary);
    assert!(inv.wound, "winding: {:?}", inv.winding_errors);
    assert!(inv.degenerate.is_empty(), "degenerate: {:?}", inv.degenerate);
    assert_eq!(inv.euler, euler, "euler");
}

fn volume(mesh: &EditMesh) -> f64 {
    let mut v = 0.0;
    for (fi, f) in mesh.faces.iter().enumerate() {
        let pts = mesh.face_points(fi);
        let _ = f;
        for k in 1..pts.len() - 1 {
            v += pts[0].dot(pts[k].cross(pts[k + 1])) / 6.0;
        }
    }
    v.abs()
}

fn doc_with_mesh(name: &str, mesh: &EditMesh) -> (Document, Id) {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let path = format!("meshes/{name}.r5mesh");
    doc.apply(Command::SetFile { path: path.clone(), bytes: Some(editmesh::io::write(mesh)) }).unwrap();
    let mut material_map: rdoc::model::KeyValues = Default::default();
    for m in &mesh.materials {
        material_map.insert(m.clone(), m.clone());
    }
    let obj = rdoc::doc::new_object(
        name,
        layer,
        ObjectKind::Mesh(rdoc::model::Mesh {
            source: path,
            material_map,
            collision: rdoc::model::CollisionMode::Triangles,
            contents: 1,
            cast_shadows: true,
            lightmap_scale: 0.0,
            render: true,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        }),
    );
    let id = obj.id;
    doc.apply(Command::AddObjects { objects: vec![obj] }).unwrap();
    (doc, id)
}

fn read_mesh(doc: &Document, id: Id) -> EditMesh {
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    editmesh::io::read(&doc.files.read(&m.source).unwrap()).unwrap()
}

fn bool_op(doc: &mut Document, object: Id, operand: &str, operation: &str) -> editmesh::OpReport {
    let req: editmesh::OpRequest = serde_json::from_value(serde_json::json!({
        "op": "boolean", "object": object.to_string(),
        "sel": {"mode": "face", "ids": []},
        "params": {"operation": operation, "operand": operand},
    }))
    .unwrap();
    let out = editmesh::plan(doc, &req).unwrap();
    doc.apply(out.command).unwrap();
    out.report
}

fn mod_op(doc: &mut Document, id: Id, op: &str, params: serde_json::Value) -> editmesh::OpReport {
    let req: editmesh::OpRequest =
        serde_json::from_value(serde_json::json!({"op": op, "object": id.to_string(), "sel": {"mode": "face", "ids": []}, "params": params}))
            .unwrap();
    let out = editmesh::plan(doc, &req).unwrap();
    doc.apply(out.command).unwrap();
    out.report
}

fn evaluated(doc: &Document, id: Id) -> EditMesh {
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    let bytes = doc.files.read(&m.source).unwrap();
    let base = match editmesh::load_triangles(&bytes, &m.source).unwrap() {
        editmesh::LoadedMesh::Editable(base) => base,
        editmesh::LoadedMesh::Soup(prims) => editmesh::convert::weld_primitives(&prims).unwrap(),
    };
    let ctx = EvalContext::from_doc(doc, id);
    editmesh::evaluate_stack(&base, &m.modifiers, &ctx, false).unwrap()
}

#[test]
fn boolean_modifier_json_round_trips() {
    let id = Id::nil();
    let m: Modifier = serde_json::from_value(serde_json::json!({"type": "boolean", "object": id.to_string()})).unwrap();
    assert!(matches!(m.kind, ModifierKind::Boolean(_)));
    assert_eq!(m.label(), "boolean");
    assert!(serde_json::from_value::<Modifier>(serde_json::json!({"type": "boolean", "object": id.to_string(), "bogus": 1})).is_err());
    let with_op: Modifier =
        serde_json::from_value(serde_json::json!({"type": "boolean", "object": id.to_string(), "operation": "union"})).unwrap();
    assert!(matches!(with_op.kind, ModifierKind::Boolean(ref p) if p.operation == BoolOp::Union));
    let back: Modifier = serde_json::from_value(serde_json::to_value(&with_op).unwrap()).unwrap();
    assert_eq!(back, with_op);
}

#[test]
fn box_minus_cylinder_is_genus_one() {
    let a = prim(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    let tube = prim(&MeshSpec::Cylinder { radius: 16.0, height: 128.0, segments: 24, caps: true });
    let b = transformed(&tube, DMat4::from_translation(DVec3::new(0.0, 0.0, -64.0)));
    let out = editmesh::boolean::solve(&a, &b, BoolOp::Difference, false).unwrap();
    assert_solid(&out, 0);
    let v = volume(&out);
    assert!(v < 64.0 * 64.0 * 64.0, "the hole removes volume: {v}");
    assert!(v > 100_000.0, "most of the box remains: {v}");
    assert!(out.faces.iter().any(|f| f.len > 3), "coplanar pieces re-weld into n-gons");
    let again = editmesh::boolean::solve(&a, &b, BoolOp::Difference, false).unwrap();
    assert_eq!(editmesh::io::write(&out), editmesh::io::write(&again), "native output is byte-identical");
}

#[test]
fn box_union_box_overlapping_is_a_ball() {
    let a = box_at(DVec3::ZERO, 64.0);
    let b = box_at(DVec3::new(32.0, 0.0, 0.0), 64.0);
    let out = editmesh::boolean::solve(&a, &b, BoolOp::Union, false).unwrap();
    assert_solid(&out, 2);
    let v = volume(&out);
    assert!((v - (64.0 * 64.0 * 96.0)).abs() < 64.0 * 64.0 * 96.0 * 0.02, "union volume: {v}");
}

#[test]
fn box_union_box_touching_stays_closed() {
    let a = box_at(DVec3::ZERO, 64.0);
    let b = box_at(DVec3::new(64.0, 0.0, 0.0), 64.0);
    let out = editmesh::boolean::solve(&a, &b, BoolOp::Union, false).unwrap();
    assert_solid(&out, 2);
    assert!((volume(&out) - 2.0 * 64.0f64.powi(3)).abs() < 1.0, "touching volume: {}", volume(&out));
}

#[test]
fn box_intersect_box_is_the_overlap() {
    let a = box_at(DVec3::ZERO, 64.0);
    let b = box_at(DVec3::new(32.0, 0.0, 0.0), 64.0);
    let out = editmesh::boolean::solve(&a, &b, BoolOp::Intersect, false).unwrap();
    assert_solid(&out, 2);
    assert!((volume(&out) - 32.0 * 64.0 * 64.0).abs() < 1.0, "overlap volume: {}", volume(&out));
}

#[test]
fn cutter_flush_on_a_face_stays_closed() {
    let wall = box_at(DVec3::ZERO, 64.0);
    let cutter = box_at(DVec3::new(0.0, 0.0, 32.0), 64.0);
    for op in [BoolOp::Difference, BoolOp::Union, BoolOp::Intersect] {
        let out = editmesh::boolean::solve(&wall, &cutter, op, false).unwrap();
        let inv = invariants(&out);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{op:?}: {inv:?}");
    }
}

#[test]
fn cutter_exactly_on_an_edge_stays_closed() {
    let wall = box_at(DVec3::ZERO, 64.0);
    let cutter = box_at(DVec3::new(64.0, 64.0, 0.0), 64.0);
    for op in [BoolOp::Difference, BoolOp::Intersect] {
        let out = editmesh::boolean::solve(&wall, &cutter, op, false).unwrap();
        let inv = invariants(&out);
        assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{op:?}: {inv:?}");
    }
    let out = editmesh::boolean::solve(&wall, &cutter, BoolOp::Union, false).unwrap();
    let inv = invariants(&out);
    assert!(inv.boundary.is_empty() && inv.wound && inv.degenerate.is_empty(), "union: {inv:?}");
    assert!(!inv.non_manifold.is_empty(), "the shared edge keeps four faces");
}

fn add_mesh(doc: &mut Document, name: &str, mesh: &EditMesh) -> Id {
    let layer = doc.layer_ids()[0];
    let path = format!("meshes/{name}.r5mesh");
    doc.apply(Command::SetFile { path: path.clone(), bytes: Some(editmesh::io::write(mesh)) }).unwrap();
    let mut material_map: rdoc::model::KeyValues = Default::default();
    for m in &mesh.materials {
        material_map.insert(m.clone(), m.clone());
    }
    let obj = rdoc::doc::new_object(
        name,
        layer,
        ObjectKind::Mesh(rdoc::model::Mesh {
            source: path,
            material_map,
            collision: rdoc::model::CollisionMode::Triangles,
            contents: 1,
            cast_shadows: true,
            lightmap_scale: 0.0,
            render: true,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        }),
    );
    let id = obj.id;
    doc.apply(Command::AddObjects { objects: vec![obj] }).unwrap();
    id
}

#[test]
fn cut_faces_take_the_operand_material_unless_kept() {
    let wall = prim(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    let mut cutter_mesh = transformed(&prim(&MeshSpec::Box { size: [32.0, 32.0, 128.0] }), DMat4::IDENTITY);
    cutter_mesh.materials = vec!["cutter_mat".to_string()];
    let (mut doc, wall_id) = doc_with_mesh("wall", &wall);
    let cutter_id = add_mesh(&mut doc, "cutter", &cutter_mesh);
    bool_op(&mut doc, wall_id, &cutter_id.to_string(), "difference");
    let out = read_mesh(&doc, wall_id);
    assert_solid(&out, 0);
    assert!(out.materials.contains(&"cutter_mat".to_string()), "operand slot merges: {:?}", out.materials);
    let cut_slot = out.materials.iter().position(|m| m == "cutter_mat").unwrap() as u16;
    assert!(out.faces.iter().any(|f| f.material == cut_slot), "cut walls wear the operand material");

    let (mut doc, wall_id) = doc_with_mesh("wall", &wall);
    let cutter_id = add_mesh(&mut doc, "cutter", &cutter_mesh);
    let req: editmesh::OpRequest = serde_json::from_value(serde_json::json!({
        "op": "boolean", "object": wall_id.to_string(),
        "sel": {"mode": "face", "ids": []},
        "params": {"operation": "difference", "operand": cutter_id.to_string(), "keep_material": true},
    }))
    .unwrap();
    let out = editmesh::plan(&doc, &req).unwrap();
    doc.apply(out.command).unwrap();
    let kept = read_mesh(&doc, wall_id);
    assert_solid(&kept, 0);
    let base_slot = kept.materials.iter().position(|m| m == "m").unwrap() as u16;
    assert!(kept.faces.iter().all(|f| f.material == base_slot), "keep_material dresses every cut in the base slot");
}

#[test]
fn destructive_op_plans_through_the_cli_path_and_undoes_in_one_step() {
    let (mut doc, wall_id) = doc_with_mesh("wall", &box_at(DVec3::ZERO, 64.0));
    let cutter_id = add_mesh(&mut doc, "cutter", &box_at(DVec3::new(32.0, 0.0, 0.0), 64.0));
    let depth = doc.undo_depth();
    let report = bool_op(&mut doc, wall_id, &cutter_id.to_string(), "union");
    assert_eq!(doc.undo_depth(), depth + 1, "one op is one undo step");
    assert_solid(&read_mesh(&doc, wall_id), 2);
    assert!(report.faces > 0);
    doc.undo().unwrap();
    assert_eq!(read_mesh(&doc, wall_id).faces.len(), 12);
    let _ = cutter_id;
}

#[test]
fn boolean_modifier_evaluates_and_caches_on_operand_moves() {
    let (mut doc, wall_id) = doc_with_mesh("wall", &box_at(DVec3::ZERO, 64.0));
    let cutter_id = add_mesh(&mut doc, "cutter", &prim(&MeshSpec::Box { size: [32.0, 32.0, 128.0] }));
    mod_op(
        &mut doc,
        wall_id,
        "modifier_add",
        serde_json::json!({"modifier": {"type": "boolean", "object": cutter_id.to_string(), "operation": "difference"}}),
    );
    let out = evaluated(&doc, wall_id);
    assert_solid(&out, 0);
    let base = read_mesh(&doc, wall_id);
    let mods = doc.object(wall_id).unwrap().clone();
    let ObjectKind::Mesh(m) = mods.kind else { panic!("not a mesh") };
    let mut cache = editmesh::StackCache::new();
    let ctx = EvalContext::from_doc(&doc, wall_id);
    let first = cache.evaluate(&base, &m.modifiers, &ctx, false).unwrap();
    let second = cache.evaluate(&base, &m.modifiers, &ctx, false).unwrap();
    assert_eq!(first.positions, second.positions);
    assert_eq!(cache.len(), 1, "the repeat hits");
    assert_eq!(editmesh::io::write(&first), editmesh::io::write(&out));
    let mut cutter = doc.object(cutter_id).unwrap().clone();
    cutter.transform.position = DVec3::new(100.0, 0.0, 0.0);
    doc.apply(Command::ReplaceObjects { objects: vec![cutter] }).unwrap();
    let moved = EvalContext::from_doc(&doc, wall_id);
    let third = cache.evaluate(&base, &m.modifiers, &moved, false).unwrap();
    assert_eq!(cache.len(), 2, "a moved operand misses");
    assert!(third.positions.iter().zip(&first.positions).any(|(a, b)| (*a - *b).length() > 1e-9));
}

#[test]
fn boolean_refuses_open_nonmanifold_and_bad_params() {
    let (mut doc, wall_id) = doc_with_mesh("wall", &box_at(DVec3::ZERO, 64.0));
    let cutter_id = add_mesh(&mut doc, "cutter", &box_at(DVec3::new(32.0, 0.0, 0.0), 64.0));
    let bad_op = |params: serde_json::Value| {
        let req: editmesh::OpRequest = serde_json::from_value(serde_json::json!({
            "op": "boolean", "object": wall_id.to_string(), "sel": {"mode": "face", "ids": []}, "params": params,
        }))
        .unwrap();
        editmesh::plan(&doc, &req).is_err()
    };
    assert!(bad_op(serde_json::json!({"operation": "difference", "operand": Id::nil().to_string()})), "unknown operand");
    assert!(bad_op(serde_json::json!({"operation": "difference", "operand": wall_id.to_string()})), "no self operand");
    assert!(bad_op(serde_json::json!({"operation": "difference", "operand": "nope"})), "operand parses");
    assert!(bad_op(serde_json::json!({"operation": "spin", "operand": cutter_id.to_string()})), "operation validates");
    assert!(bad_op(serde_json::json!({"operation": "difference", "operand": cutter_id.to_string(), "bogus": 1})), "unknown keys");
    assert!(bad_op(serde_json::json!({"operation": "difference", "operand": "unselected"})), "unselected needs a selection");

    let mut open = box_at(DVec3::ZERO, 64.0);
    open.faces.pop();
    let err = editmesh::boolean::solve(&open, &box_at(DVec3::ZERO, 64.0), BoolOp::Difference, false).unwrap_err();
    assert!(err.to_string().contains("open"), "open names itself: {err}");
    let err = editmesh::boolean::solve(&box_at(DVec3::ZERO, 64.0), &open, BoolOp::Difference, false).unwrap_err();
    assert!(err.to_string().contains("operand"), "the side is named: {err}");

    let mut nonmanifold = box_at(DVec3::ZERO, 64.0);
    let extra = nonmanifold.faces[0];
    let dup: Vec<rdoc::editmesh::Corner> =
        nonmanifold.corners[extra.first as usize..extra.first as usize + extra.len as usize].to_vec();
    let first = nonmanifold.corners.len() as u32;
    nonmanifold.corners.extend_from_slice(&dup);
    nonmanifold.faces.push(extra);
    nonmanifold.faces.last_mut().unwrap().first = first;
    let err = editmesh::boolean::solve(&nonmanifold, &box_at(DVec3::ZERO, 64.0), BoolOp::Difference, false).unwrap_err();
    assert!(err.to_string().contains("merge by distance"), "the fix is named: {err}");
}

#[test]
fn boolean_budget_aborts_before_pair_tests() {
    let dense = editmesh::evaluate_stack(
        &prim(&MeshSpec::Box { size: [64.0, 64.0, 64.0] }),
        &[Modifier {
            enabled: true,
            show_in_viewport: true,
            kind: ModifierKind::Subdivision(editmesh::modifiers::SubdivisionParams {
                levels: 4,
                kind: editmesh::modifiers::SubdivKind::CatmullClark,
                keep_sharp_edges: true,
            }),
        }],
        &EvalContext::default(),
        false,
    )
    .unwrap();
    assert!(dense.faces.len() > 2000, "the operand is dense: {}", dense.faces.len());
    let err = editmesh::boolean::solve(&dense, &dense, BoolOp::Difference, false).unwrap_err();
    assert!(err.to_string().contains("budget"), "the pair count aborts: {err}");
}

#[test]
fn boolean_unselected_mode_runs_deterministically() {
    let (mut doc, wall_id) = doc_with_mesh("wall", &box_at(DVec3::ZERO, 64.0));
    let req: editmesh::OpRequest = serde_json::from_value(serde_json::json!({
        "op": "boolean", "object": wall_id.to_string(),
        "sel": {"mode": "face", "ids": [0]},
        "params": {"operation": "difference", "operand": "unselected"},
    }))
    .unwrap();
    let first = editmesh::plan(&doc, &req).unwrap();
    let second = editmesh::plan(&doc, &req).unwrap();
    let bytes = |o: &editmesh::OpOutput| match &o.command {
        Command::Batch { commands, .. } => match &commands[0] {
            Command::SetFile { bytes: Some(b), .. } => b.clone(),
            _ => panic!("a file write"),
        },
        _ => panic!("a batch"),
    };
    assert_eq!(bytes(&first), bytes(&second), "planning twice plans the same bytes");
    doc.apply(first.command).unwrap();
    let out = read_mesh(&doc, wall_id);
    assert!(!out.faces.is_empty());
}

#[test]
fn coplanar_overlaps_resolve_by_normal_direction() {
    let a = box_at(DVec3::ZERO, 64.0);
    let vol = 64.0f64.powi(3);
    let united = editmesh::boolean::solve(&a, &a, BoolOp::Union, false).unwrap();
    assert_solid(&united, 2);
    assert!((volume(&united) - vol).abs() < 1.0, "union of twins is the box: {}", volume(&united));
    let met = editmesh::boolean::solve(&a, &a, BoolOp::Intersect, false).unwrap();
    assert_solid(&met, 2);
    assert!((volume(&met) - vol).abs() < 1.0, "intersect of twins is the box: {}", volume(&met));
    let gone = editmesh::boolean::solve(&a, &a, BoolOp::Difference, false).unwrap();
    assert!(gone.faces.is_empty(), "difference of twins is empty: {} faces", gone.faces.len());
    assert!(invariants(&gone).closed, "empty is boundary-free");

    let b = box_at(DVec3::new(64.0, 0.0, 0.0), 64.0);
    let kept = editmesh::boolean::solve(&a, &b, BoolOp::Difference, false).unwrap();
    assert_solid(&kept, 2);
    assert!((volume(&kept) - vol).abs() < 1.0, "a face-touching cutter removes nothing: {}", volume(&kept));
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (u64::MAX >> 11) as f64
    }
}

#[test]
fn boolean_fuzz_stays_closed_over_random_transforms() {
    let fixtures = [
        prim(&MeshSpec::Box { size: [64.0, 64.0, 64.0] }),
        prim(&MeshSpec::Box { size: [48.0, 32.0, 96.0] }),
        prim(&MeshSpec::Cylinder { radius: 16.0, height: 96.0, segments: 12, caps: true }),
        prim(&MeshSpec::Cylinder { radius: 24.0, height: 48.0, segments: 16, caps: true }),
    ];
    let ops = [BoolOp::Difference, BoolOp::Union, BoolOp::Intersect];
    let mut rng = XorShift(0xC10C_4E55);
    for case in 0..200 {
        let a = &fixtures[(rng.next() % fixtures.len() as u64) as usize];
        let b = &fixtures[(rng.next() % fixtures.len() as u64) as usize];
        let axis = DVec3::new(rng.unit() - 0.5, rng.unit() - 0.5, rng.unit() - 0.5);
        let axis = if axis.length_squared() < 1e-6 { DVec3::Z } else { axis.normalize() };
        let rot = DQuat::from_axis_angle(axis, (rng.unit() - 0.5) * std::f64::consts::TAU);
        let shift = DVec3::new(rng.unit() * 96.0 - 48.0, rng.unit() * 96.0 - 48.0, rng.unit() * 48.0 - 24.0);
        let moved = transformed(b, DMat4::from_rotation_translation(rot, shift));
        let op = ops[case % ops.len()];
        let out = editmesh::boolean::solve(a, &moved, op, false)
            .unwrap_or_else(|e| panic!("case {case} {op:?} failed: {e}"));
        let inv = invariants(&out);
        assert!(inv.closed, "case {case} {op:?} open: {:?}", inv.boundary);
        assert!(inv.wound, "case {case} {op:?} wound: {:?}", inv.winding_errors);
        assert!(inv.degenerate.is_empty(), "case {case} {op:?} degenerate: {:?}", inv.degenerate);
    }
}


#[test]
fn successive_cuts_stay_closed_across_collinear_ngon_corners() {
    // The first cut leaves n-gons with a corner on a straight run; cutting
    // again must not triangulate that corner into a zero-area hole.
    let mut tower = editmesh::convert::editable_from_spec(
        &MeshSpec::Cylinder { radius: 192.0, height: 768.0, segments: 16, caps: true },
        "t",
        "m",
        1.0 / 512.0,
    )
    .unwrap();
    let slit = prim(&MeshSpec::Box { size: [64.0, 64.0, 112.0] });
    for deg in [11.25f64, 101.25] {
        let a = deg.to_radians();
        let at = DMat4::from_translation(DVec3::new(a.cos() * 192.0, a.sin() * 192.0, 200.0)) * DMat4::from_scale(DVec3::new(0.2, 0.2, 1.0));
        tower = editmesh::boolean::solve(&tower, &transformed(&slit, at), BoolOp::Difference, false).unwrap();
        assert_solid(&tower, 2);
    }
}
