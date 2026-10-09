//! M9 gates: the modifier stack (array, curve, displace, subdivision, mirror)
//! evaluated inside `evaluate`, one undo step per stack edit, and the compile
//! gates for array collision.

use glam::{DQuat, DVec3};
use rdoc::command::Command;
use rdoc::doc::{Document, KindFilter};
use rdoc::editmesh::{self, Adjacency, EditMesh, EvalContext, Modifier, ModifierKind};
use rdoc::meshgen::{cmd_add_mesh, MeshRequest};
use rdoc::model::{Id, ObjectKind, PathKind, PathObject};

fn doc_with_box() -> (Document, Id) {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let req: MeshRequest =
        serde_json::from_str(r#"{"spec":{"shape":"box","size":[64,64,64]},"editable":true}"#).unwrap();
    doc.apply(cmd_add_mesh(layer, None, &req).unwrap()).unwrap();
    let id = doc.kind_objects(KindFilter::Mesh)[0];
    (doc, id)
}

fn mesh_of(doc: &Document, id: Id) -> EditMesh {
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    editmesh::io::read(&doc.files.read(&m.source).unwrap()).unwrap()
}

fn mods_of(doc: &Document, id: Id) -> Vec<Modifier> {
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    m.modifiers.clone()
}

fn mod_op(doc: &mut Document, id: Id, op: &str, params: serde_json::Value) -> editmesh::OpReport {
    let req: editmesh::OpRequest =
        serde_json::from_value(serde_json::json!({"op": op, "object": id.to_string(), "sel": {"mode": "face", "ids": []}, "params": params}))
            .unwrap();
    let out = editmesh::plan(doc, &req).unwrap();
    doc.apply(out.command).unwrap();
    out.report
}

fn geo_op(doc: &mut Document, id: Id, op: &str, params: serde_json::Value) {
    let req: editmesh::OpRequest =
        serde_json::from_value(serde_json::json!({"op": op, "object": id.to_string(), "sel": {"mode": "face", "ids": []}, "params": params}))
            .unwrap();
    let out = editmesh::plan(doc, &req).unwrap();
    doc.apply(out.command).unwrap();
}

fn evaluated(doc: &Document, id: Id) -> EditMesh {
    let base = mesh_of(doc, id);
    let ctx = EvalContext::from_doc(doc, id);
    editmesh::evaluate_stack(&base, &mods_of(doc, id), &ctx, false).unwrap()
}

fn min_pairwise(m: &EditMesh) -> f64 {
    let mut lo = f64::INFINITY;
    for (i, a) in m.positions.iter().enumerate() {
        for b in &m.positions[i + 1..] {
            lo = lo.min((*a - *b).length());
        }
    }
    lo
}

/// A true six-quad cube: the welded soup boxes are twelve triangles, and the
/// subdivision gate counts quads.
fn quad_cube(half: f64) -> EditMesh {
    let s = half;
    let positions = vec![
        DVec3::new(-s, -s, -s),
        DVec3::new(s, -s, -s),
        DVec3::new(s, s, -s),
        DVec3::new(-s, s, -s),
        DVec3::new(-s, -s, s),
        DVec3::new(s, -s, s),
        DVec3::new(s, s, s),
        DVec3::new(-s, s, s),
    ];
    let loops: [[u32; 4]; 6] =
        [[0, 3, 2, 1], [4, 5, 6, 7], [2, 3, 7, 6], [0, 1, 5, 4], [1, 2, 6, 5], [0, 4, 7, 3]];
    let mut mesh = EditMesh {
        positions,
        faces: Vec::new(),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: vec!["m".into()],
        smooth_angle: None,
    };
    for verts in loops {
        let first = mesh.corners.len() as u32;
        for v in verts {
            mesh.corners.push(editmesh::Corner { vertex: v, uv: [0.0, 0.0], normal: None });
        }
        mesh.faces.push(editmesh::Face { first, len: 4, material: 0, flags: editmesh::FaceFlags::empty() });
    }
    mesh
}

fn array_kind(count: u32) -> Modifier {
    Modifier {
        enabled: true,
        show_in_viewport: true,
        kind: ModifierKind::Array(editmesh::modifiers::ArrayParams { count, ..Default::default() }),
    }
}

/// An annular sector (-18..18 deg): ten rotated copies tile a closed ring.
fn wedge() -> EditMesh {
    let (r0, r1, h) = (64.0, 128.0, 16.0);
    let (a0, a1) = ((-18.0f64).to_radians(), (18.0f64).to_radians());
    let positions = vec![
        DVec3::new(r0 * a0.cos(), r0 * a0.sin(), -h),
        DVec3::new(r1 * a0.cos(), r1 * a0.sin(), -h),
        DVec3::new(r1 * a1.cos(), r1 * a1.sin(), -h),
        DVec3::new(r0 * a1.cos(), r0 * a1.sin(), -h),
        DVec3::new(r0 * a0.cos(), r0 * a0.sin(), h),
        DVec3::new(r1 * a0.cos(), r1 * a0.sin(), h),
        DVec3::new(r1 * a1.cos(), r1 * a1.sin(), h),
        DVec3::new(r0 * a1.cos(), r0 * a1.sin(), h),
    ];
    let loops: [[u32; 4]; 6] =
        [[0, 3, 2, 1], [4, 5, 6, 7], [1, 2, 6, 5], [0, 4, 7, 3], [0, 1, 5, 4], [3, 7, 6, 2]];
    let mut mesh = EditMesh {
        positions,
        faces: Vec::new(),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: vec!["m".into()],
        smooth_angle: None,
    };
    for verts in loops {
        let first = mesh.corners.len() as u32;
        for v in verts {
            mesh.corners.push(editmesh::Corner { vertex: v, uv: [0.0, 0.0], normal: None });
        }
        mesh.faces.push(editmesh::Face { first, len: 4, material: 0, flags: editmesh::FaceFlags::empty() });
    }
    mesh
}

/// A document holding one `.r5mesh` object built by hand.
fn doc_with_mesh(name: &str, mesh: &EditMesh) -> (Document, Id) {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let path = format!("meshes/{name}.r5mesh");
    doc.apply(Command::SetFile { path: path.clone(), bytes: Some(editmesh::io::write(mesh)) }).unwrap();
    let mut material_map: rdoc::model::KeyValues = Default::default();
    material_map.insert("m".into(), "m".into());
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

#[test]
fn old_map_json_without_modifiers_loads_unchanged() {
    let text = r#"{"source":"m.glb","material_map":{},"collision":"triangles","contents":1,"cast_shadows":true,"lightmap_scale":0.0,"render":true}"#;
    let mesh: rdoc::model::Mesh = serde_json::from_str(text).unwrap();
    assert!(mesh.modifiers.is_empty());
    assert_eq!(serde_json::to_value(&mesh).unwrap().get("modifiers"), None);
}

#[test]
fn modifier_json_round_trips_with_defaults() {
    let m: Modifier = serde_json::from_value(serde_json::json!({"type": "array", "count": 3})).unwrap();
    assert!(m.enabled && m.show_in_viewport);
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v.get("type").unwrap(), "array");
    assert!(v.get("enabled").is_none(), "defaults are skipped");
    let c: Modifier =
        serde_json::from_value(serde_json::json!({"type": "curve", "path": "00000000-0000-0000-0000-000000000000", "axis": "-x"}))
            .unwrap();
    assert!(matches!(c.kind, ModifierKind::Curve(_)));
    let back: Modifier = serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
    assert_eq!(back, c);
    for kind in ["array", "curve", "displace", "subdivision", "mirror"] {
        assert!(serde_json::from_value::<Modifier>(serde_json::json!({"type": kind, "bogus": 1})).is_err(), "{kind} keeps unknown keys");
    }
}

#[test]
fn array_fence_fit_length_and_merge_welds_one_strip() {
    let (mut doc, id) = doc_with_box();
    geo_op(&mut doc, id, "translate", serde_json::json!({"translate": [0.0, 0.0, 0.0]}));
    let base = mesh_of(&doc, id);
    assert_eq!(base.positions.len(), 8);
    let report = mod_op(
        &mut doc,
        id,
        "modifier_add",
        serde_json::json!({"modifier": {"type": "array", "fit_length": 512.0, "merge": 0.01}}),
    );
    assert_eq!(report.faces, 6 * 8);
    let out = evaluated(&doc, id);
    assert_eq!(out.positions.len(), 8 + 7 * 4, "shared walls weld to one strip");
    assert!(min_pairwise(&out) > 5e-3, "no internal duplicate verts");
    let again = evaluated(&doc, id);
    assert_eq!(editmesh::io::write(&out), editmesh::io::write(&again), "evaluation is deterministic");
}

#[test]
fn array_rotating_via_object_offset_closes_a_ring() {
    let (mut doc, id) = doc_with_mesh("wedge", &wedge());
    let layer = doc.layer_ids()[0];
    let mut pivot = rdoc::doc::new_object("pivot", layer, ObjectKind::Group);
    pivot.transform.rotation = DQuat::from_rotation_z((36.0f64).to_radians());
    let pivot_id = pivot.id;
    doc.apply(Command::AddObjects { objects: vec![pivot] }).unwrap();
    mod_op(
        &mut doc,
        id,
        "modifier_add",
        serde_json::json!({"modifier": {"type": "array", "count": 10, "relative_offset": [0.0, 0.0, 0.0],
            "object_offset": pivot_id.to_string(), "merge": 0.01}}),
    );
    let out = evaluated(&doc, id);
    assert_eq!(out.positions.len(), 80 - 10 * 4, "every wall welds, seam included");
    assert!(min_pairwise(&out) > 5e-3, "no duplicate verts anywhere on the ring");
    let inv = editmesh::validate::invariants(&out);
    assert!(inv.boundary.is_empty(), "the ring has no boundary");
    assert!(inv.wound, "the ring stays wound");
    mod_op(&mut doc, id, "modifier_set", serde_json::json!({"index": 0,
        "modifier": {"type": "array", "count": 10, "relative_offset": [0.0, 0.0, 0.0],
            "object_offset": pivot_id.to_string(), "merge_first_last": true}}));
    let seam = evaluated(&doc, id);
    assert_eq!(seam.positions.len(), 80 - 4, "merge_first_last welds only the seam");
    assert!(editmesh::validate::invariants(&seam).boundary.is_empty());
}

#[test]
fn array_curve_quarter_circle_keeps_merlons_upright() {
    let mut merlon = quad_cube(8.0);
    for v in &mut merlon.positions {
        v.x += 8.0;
    }
    let (mut doc, id) = doc_with_mesh("merlon", &merlon);
    let layer = doc.layer_ids()[0];
    let mut pts = Vec::new();
    for k in 0..=6 {
        let a = (k as f64 * 15.0).to_radians();
        pts.push(DVec3::new(100.0 * a.cos(), 100.0 * a.sin(), 0.0));
    }
    let length: f64 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    let path = rdoc::doc::new_object(
        "arc",
        layer,
        ObjectKind::Path(PathObject { kind: PathKind::Spline, points: pts, keyvalues: Default::default() }),
    );
    let path_id = path.id;
    doc.apply(Command::AddObjects { objects: vec![path] }).unwrap();
    mod_op(
        &mut doc,
        id,
        "modifier_add",
        serde_json::json!({"modifier": {"type": "array", "fit_length": length, "merge": 0.01}}),
    );
    mod_op(
        &mut doc,
        id,
        "modifier_add",
        serde_json::json!({"modifier": {"type": "curve", "path": path_id.to_string(), "axis": "x"}}),
    );
    let out = evaluated(&doc, id);
    assert_eq!(out.positions.len(), 8 + 9 * 4, "ten merlons weld into one wall");
    for v in &out.positions {
        assert!((v.z.abs() - 8.0).abs() < 1e-6, "every merlon stays upright");
        let r = (v.x * v.x + v.y * v.y).sqrt();
        assert!(((r - 100.0).abs() - 8.0).abs() < 1.5, "the wall follows the arc");
    }
}

#[test]
fn subdivision_cube_level_two_counts() {
    let ctx = EvalContext::default();
    let cube = quad_cube(32.0);
    let one = editmesh::evaluate_stack(
        &cube,
        &[Modifier {
            enabled: true,
            show_in_viewport: true,
            kind: ModifierKind::Subdivision(editmesh::modifiers::SubdivisionParams {
                levels: 1,
                kind: editmesh::modifiers::SubdivKind::CatmullClark,
                keep_sharp_edges: true,
            }),
        }],
        &ctx,
        false,
    )
    .unwrap();
    assert_eq!(one.positions.len(), 26);
    assert_eq!(one.faces.len(), 24);
    let two = editmesh::evaluate_stack(
        &one,
        &[Modifier {
            enabled: true,
            show_in_viewport: true,
            kind: ModifierKind::Subdivision(editmesh::modifiers::SubdivisionParams {
                levels: 1,
                kind: editmesh::modifiers::SubdivKind::CatmullClark,
                keep_sharp_edges: true,
            }),
        }],
        &ctx,
        false,
    )
    .unwrap();
    assert_eq!(two.positions.len(), 98);
    assert_eq!(two.faces.len(), 96);
    let inv = editmesh::validate::invariants(&two);
    assert!(inv.closed && inv.wound && inv.euler == 2);
}

#[test]
fn subdivision_zero_levels_is_identity() {
    let cube = quad_cube(32.0);
    let out = editmesh::evaluate_stack(
        &cube,
        &[Modifier {
            enabled: true,
            show_in_viewport: true,
            kind: ModifierKind::Subdivision(editmesh::modifiers::SubdivisionParams {
                levels: 0,
                ..Default::default()
            }),
        }],
        &EvalContext::default(),
        false,
    )
    .unwrap();
    assert_eq!(out, cube);
}

#[test]
fn displace_is_deterministic_per_seed() {
    let cube = quad_cube(32.0);
    let with_seed = |seed: u64| {
        editmesh::evaluate_stack(
            &cube,
            &[Modifier {
                enabled: true,
                show_in_viewport: true,
                kind: ModifierKind::Displace(editmesh::modifiers::DisplaceParams {
                    strength: 8.0,
                    texture: editmesh::modifiers::NoiseTexture {
                        kind: rdoc::noise::NoiseKind::Value,
                        scale: 0.05,
                        octaves: 4,
                        seed,
                    },
                    ..Default::default()
                }),
            }],
            &EvalContext::default(),
            false,
        )
        .unwrap()
    };
    let a = with_seed(7);
    let b = with_seed(7);
    assert_eq!(a.positions, b.positions, "same seed, same rock");
    let c = with_seed(8);
    assert!(a.positions.iter().zip(&c.positions).any(|(p, q)| (*p - *q).length() > 1e-9), "a new seed moves verts");
    assert!(a.positions.iter().zip(&cube.positions).any(|(p, q)| (*p - *q).length() > 1e-9), "strength moves verts");
}

#[test]
fn mirror_merge_leaves_no_duplicates_on_the_plane() {
    let (mut doc, id) = doc_with_box();
    geo_op(&mut doc, id, "translate", serde_json::json!({"translate": [32.0, 0.0, 0.0]}));
    mod_op(
        &mut doc,
        id,
        "modifier_add",
        serde_json::json!({"modifier": {"type": "mirror", "axes": [true, false, false], "merge": 0.01}}),
    );
    let out = evaluated(&doc, id);
    assert_eq!(out.positions.len(), 12, "the four plane verts merge");
    assert!(min_pairwise(&out) > 5e-3);
    for v in &out.positions {
        let twin = out.positions.iter().any(|w| (*w - DVec3::new(-v.x, v.y, v.z)).length() < 1e-9);
        assert!(twin, "every vert has its mirror twin");
    }
}

#[test]
fn disabled_and_hidden_modifiers_are_skipped() {
    let (mut doc, id) = doc_with_box();
    mod_op(&mut doc, id, "modifier_add", serde_json::json!({"modifier": {"type": "array", "count": 4}}));
    let full = evaluated(&doc, id);
    assert_eq!(full.positions.len(), 32);
    mod_op(&mut doc, id, "modifier_toggle", serde_json::json!({"index": 0}));
    let off = evaluated(&doc, id);
    assert_eq!(off.positions.len(), 8, "a disabled modifier is skipped");
    mod_op(&mut doc, id, "modifier_toggle", serde_json::json!({"index": 0}));
    let base = mesh_of(&doc, id);
    let ctx = EvalContext::from_doc(&doc, id);
    let mut hidden = mods_of(&doc, id);
    hidden[0].show_in_viewport = false;
    let view = editmesh::evaluate_stack(&base, &hidden, &ctx, true).unwrap();
    assert_eq!(view.positions.len(), 8, "the viewport skips hidden modifiers");
    let render = editmesh::evaluate_stack(&base, &hidden, &ctx, false).unwrap();
    assert_eq!(render.positions.len(), 32, "the compiler still sees them");
}

#[test]
fn modifier_stack_edit_is_one_undo_step() {
    let (mut doc, id) = doc_with_box();
    let depth = doc.undo_depth();
    mod_op(&mut doc, id, "modifier_add", serde_json::json!({"modifier": {"type": "array", "count": 3}}));
    assert_eq!(doc.undo_depth(), depth + 1);
    assert_eq!(mods_of(&doc, id).len(), 1);
    doc.undo().unwrap();
    assert!(mods_of(&doc, id).is_empty());
    assert_eq!(doc.undo_depth(), depth);
}

#[test]
fn modifier_apply_bakes_the_prefix_into_the_file() {
    let (mut doc, id) = doc_with_box();
    mod_op(&mut doc, id, "modifier_add", serde_json::json!({"modifier": {"type": "array", "count": 2}}));
    mod_op(&mut doc, id, "modifier_add", serde_json::json!({"modifier": {"type": "mirror", "axes": [true, false, false]}}));
    let depth = doc.undo_depth();
    mod_op(&mut doc, id, "modifier_apply", serde_json::json!({"index": 0}));
    assert_eq!(doc.undo_depth(), depth + 1, "apply is one undo step");
    let baked = mesh_of(&doc, id);
    assert_eq!(baked.positions.len(), 16, "only the array prefix bakes");
    assert_eq!(mods_of(&doc, id).len(), 1, "the mirror stays on the stack");
    doc.undo().unwrap();
    assert_eq!(mods_of(&doc, id).len(), 2);
    assert_eq!(mesh_of(&doc, id).positions.len(), 8);
}

#[test]
fn apply_makes_a_glb_mesh_editable_first() {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let req: MeshRequest = serde_json::from_str(r#"{"spec":{"shape":"box","size":[32,32,32]}}"#).unwrap();
    doc.apply(cmd_add_mesh(layer, None, &req).unwrap()).unwrap();
    let id = doc.kind_objects(KindFilter::Mesh)[0];
    let before = doc.object(id).unwrap().clone();
    let ObjectKind::Mesh(m) = &before.kind else { panic!("not a mesh") };
    assert!(m.source.ends_with(".glb"));
    mod_op(&mut doc, id, "modifier_add", serde_json::json!({"modifier": {"type": "array", "count": 2}}));
    mod_op(&mut doc, id, "modifier_apply", serde_json::json!({"index": 0}));
    let after = doc.object(id).unwrap().clone();
    let ObjectKind::Mesh(m) = &after.kind else { panic!("not a mesh") };
    assert!(m.source.ends_with(".r5mesh"), "a glb source becomes editable on apply");
    assert!(mods_of(&doc, id).is_empty());
    assert_eq!(mesh_of(&doc, id).positions.len(), 16);
}

#[test]
fn modifier_moves_reorder_and_remove() {
    let (mut doc, id) = doc_with_box();
    mod_op(&mut doc, id, "modifier_add", serde_json::json!({"modifier": {"type": "array", "count": 2}}));
    mod_op(&mut doc, id, "modifier_add", serde_json::json!({"modifier": {"type": "mirror", "axes": [true, false, false]}}));
    mod_op(&mut doc, id, "modifier_move", serde_json::json!({"index": 1, "to": 0}));
    let mods = mods_of(&doc, id);
    assert!(matches!(mods[0].kind, ModifierKind::Mirror(_)));
    assert!(matches!(mods[1].kind, ModifierKind::Array(_)));
    mod_op(&mut doc, id, "modifier_set", serde_json::json!({"index": 1, "modifier": {"type": "array", "count": 5}}));
    assert!(matches!(mods_of(&doc, id)[1].kind, ModifierKind::Array(_)));
    assert_eq!(evaluated(&doc, id).positions.len(), 8 * 2 * 5);
    mod_op(&mut doc, id, "modifier_remove", serde_json::json!({"index": 0}));
    assert_eq!(mods_of(&doc, id).len(), 1);
}

#[test]
fn bad_modifier_requests_are_errors_not_panics() {
    let (mut doc, id) = doc_with_box();
    for (op, params) in [
        ("modifier_remove", serde_json::json!({"index": 0})),
        ("modifier_toggle", serde_json::json!({"index": 3})),
        ("modifier_move", serde_json::json!({"index": 0, "to": 1})),
        ("modifier_apply", serde_json::json!({"index": 0})),
        ("modifier_add", serde_json::json!({"modifier": {"type": "array", "count": 2, "bogus": 1}})),
        ("modifier_add", serde_json::json!({"modifier": {"type": "nope"}})),
        (
            "modifier_add",
            serde_json::json!({"modifier": {"type": "array", "count": 2, "object_offset": "00000000-0000-0000-0000-000000000000"}}),
        ),
    ] {
        let req: editmesh::OpRequest =
            serde_json::from_value(serde_json::json!({"op": op, "object": id.to_string(), "sel": {"mode": "face", "ids": []}, "params": params}))
                .unwrap();
        assert!(editmesh::plan(&doc, &req).is_err(), "{op} {params} should fail");
    }
    mod_op(&mut doc, id, "modifier_add", serde_json::json!({"modifier": {"type": "array", "count": 2}}));
    let req: editmesh::OpRequest = serde_json::from_value(
        serde_json::json!({"op": "modifier_set", "object": id.to_string(), "sel": {"mode": "face", "ids": []},
            "params": {"index": 5, "modifier": {"type": "array", "count": 2}}}),
    )
    .unwrap();
    assert!(editmesh::plan(&doc, &req).is_err());
}

#[test]
fn array_counts_are_clamped_and_checked_before_building() {
    let cube = quad_cube(32.0);
    let ctx = EvalContext::default();
    let run = |count: u32| {
        editmesh::evaluate_stack(&cube, &[array_kind(count)], &ctx, false).unwrap().positions.len()
    };
    assert_eq!(run(0), 8, "count clamps up to one");
    assert_eq!(run(5000), 8 * 1024, "count clamps to the 1024 budget");
    let mut strip = quad_cube(1.0);
    strip.positions.clear();
    strip.faces.clear();
    strip.corners.clear();
    for k in 0..2100 {
        let base = strip.positions.len() as u32;
        strip.positions.push(DVec3::new(k as f64 * 2.0, 0.0, 0.0));
        strip.positions.push(DVec3::new(k as f64 * 2.0, 1.0, 0.0));
        if k > 0 {
            let first = strip.corners.len() as u32;
            for v in [base - 2, base - 1, base, base + 1] {
                strip.corners.push(editmesh::Corner { vertex: v, uv: [0.0, 0.0], normal: None });
            }
            strip.faces.push(editmesh::Face { first, len: 4, material: 0, flags: editmesh::FaceFlags::empty() });
        }
    }
    let err = editmesh::evaluate_stack(&strip, &[array_kind(1024)], &ctx, false).unwrap_err();
    assert!(err.to_string().contains("budget"), "the vertex total is checked first: {err}");
}

#[test]
fn curve_without_a_path_and_array_without_an_object_are_errors() {
    let cube = quad_cube(32.0);
    let ctx = EvalContext::default();
    let missing = Id::nil();
    let curve = Modifier {
        enabled: true,
        show_in_viewport: true,
        kind: ModifierKind::Curve(editmesh::modifiers::CurveParams { path: missing, axis: editmesh::modifiers::CurveAxis::X }),
    };
    assert!(editmesh::evaluate_stack(&cube, &[curve], &ctx, false).is_err());
    let array = Modifier {
        enabled: true,
        show_in_viewport: true,
        kind: ModifierKind::Array(editmesh::modifiers::ArrayParams { object_offset: Some(missing), ..Default::default() }),
    };
    assert!(editmesh::evaluate_stack(&cube, &[array], &ctx, false).is_err());
}

#[test]
fn stack_cache_keys_on_file_stack_and_transforms() {
    let (mut doc, id) = doc_with_box();
    geo_op(&mut doc, id, "translate", serde_json::json!({"translate": [100.0, 0.0, 0.0]}));
    let layer = doc.layer_ids()[0];
    let mut pivot = rdoc::doc::new_object("pivot", layer, ObjectKind::Group);
    let pivot_id = pivot.id;
    doc.apply(Command::AddObjects { objects: vec![pivot] }).unwrap();
    mod_op(
        &mut doc,
        id,
        "modifier_add",
        serde_json::json!({"modifier": {"type": "array", "count": 3, "relative_offset": [0.0, 0.0, 0.0],
            "object_offset": pivot_id.to_string()}}),
    );
    let base = mesh_of(&doc, id);
    let mods = mods_of(&doc, id);
    let mut cache = editmesh::StackCache::new();
    let ctx = EvalContext::from_doc(&doc, id);
    let first = cache.evaluate(&base, &mods, &ctx, false).unwrap();
    let second = cache.evaluate(&base, &mods, &ctx, false).unwrap();
    assert_eq!(first.positions, second.positions);
    assert_eq!(cache.len(), 1, "the repeat hits");
    pivot = doc.object(pivot_id).unwrap().clone();
    pivot.transform.rotation = DQuat::from_rotation_z(0.5);
    doc.apply(Command::ReplaceObjects { objects: vec![pivot] }).unwrap();
    let moved = EvalContext::from_doc(&doc, id);
    let third = cache.evaluate(&base, &mods, &moved, false).unwrap();
    assert_eq!(cache.len(), 2, "a moved operand misses");
    assert!(third.positions.iter().zip(&first.positions).any(|(a, b)| (*a - *b).length() > 1e-9));
}

#[test]
fn soup_parts_split_disjoint_copies() {
    let (mut doc, id) = doc_with_box();
    mod_op(&mut doc, id, "modifier_add", serde_json::json!({"modifier": {"type": "array", "count": 2, "relative_offset": [2.0, 0.0, 0.0]}}));
    let base = mesh_of(&doc, id);
    let ctx = EvalContext::from_doc(&doc, id);
    let stacked = editmesh::evaluate_stack(&base, &mods_of(&doc, id), &ctx, false).unwrap();
    let prims = editmesh::convert::evaluated_primitives(&stacked);
    assert_eq!(prims.len(), 1, "one material slot");
    let parts = editmesh::modifiers::soup_parts_by_position(&prims[0].positions, &prims[0].indices);
    assert_eq!(parts.len(), 2, "two disjoint copies hull separately");
    assert_eq!(parts[0].len() + parts[1].len(), prims[0].indices.len() / 3);
}

#[test]
fn stack_evaluation_matches_across_repeated_runs() {
    let (mut doc, id) = doc_with_box();
    mod_op(
        &mut doc,
        id,
        "modifier_add",
        serde_json::json!({"modifier": {"type": "subdivision", "levels": 1}}),
    );
    mod_op(
        &mut doc,
        id,
        "modifier_add",
        serde_json::json!({"modifier": {"type": "displace", "strength": 4.0,
            "texture": {"kind": "perlin", "scale": 0.05, "octaves": 3, "seed": 42}}}),
    );
    let a = editmesh::io::write(&evaluated(&doc, id));
    let b = editmesh::io::write(&evaluated(&doc, id));
    assert_eq!(a, b);
    let adj = Adjacency::build(&evaluated(&doc, id));
    assert!(adj.is_consistently_wound());
}
