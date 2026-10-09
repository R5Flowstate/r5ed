//! M1 gates: round trip, op invariants on closed fixtures, determinism, fuzz.

use glam::DVec3;
use rdoc::brush::box_brush;
use rdoc::command::Command;
use rdoc::doc::Document;
use rdoc::editmesh::{self, Adjacency, EditMesh};
use rdoc::import::glb::{self, GlbMesh};
use rdoc::meshgen::{build, cmd_add_mesh, MeshRequest, MeshSpec};
use rdoc::model::{Mesh, ObjectKind};

fn specs() -> Vec<(&'static str, MeshSpec)> {
    vec![
        ("box", MeshSpec::Box { size: [64.0, 64.0, 64.0] }),
        ("cylinder", MeshSpec::Cylinder { radius: 32.0, height: 64.0, segments: 16, caps: true }),
        ("stairs", MeshSpec::Stairs { steps: 8, run: 256.0, rise: 128.0, width: 96.0 }),
        (
            "arch",
            MeshSpec::Arch { width: 128.0, height: 160.0, thickness: 16.0, depth: 32.0, segments: 12 },
        ),
        ("torus", MeshSpec::Torus { major: 64.0, minor: 16.0, segments: 24, sides: 12 }),
    ]
}

fn glb_of(spec: &MeshSpec) -> GlbMesh {
    let g = build(spec, "t", "m", 1.0 / 512.0).unwrap();
    GlbMesh { colors: Vec::new(),
        name: g.name,
        material: g.material,
        positions: g.positions,
        normals: g.normals,
        uvs: g.uvs,
        indices: g.indices,
    }
}

fn welded(spec: &MeshSpec) -> (GlbMesh, EditMesh) {
    let g = glb_of(spec);
    let m = editmesh::convert::weld_primitives(std::slice::from_ref(&g)).unwrap();
    (g, m)
}

fn all_names() -> [&'static str; 6] {
    ["box", "cylinder", "stairs", "arch", "torus", "room"]
}

fn spec_of(name: &str) -> MeshSpec {
    specs().into_iter().find(|(n, _)| *n == name).map(|(_, s)| s).expect("a known fixture")
}

fn fixture_doc(name: &str) -> (Document, rdoc::model::Id) {
    if name == "room" {
        brush_room_doc()
    } else {
        doc_with(&spec_of(name))
    }
}

fn fixture_euler(name: &str) -> i64 {
    if name == "torus" {
        0
    } else {
        2
    }
}

/// A document holding one editable mesh plus its file.
fn doc_with(spec: &MeshSpec) -> (Document, rdoc::model::Id) {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let req: MeshRequest = serde_json::from_str(&format!("{{\"spec\":{}}}", serde_json::to_string(spec).unwrap())).unwrap();
    let cmd = cmd_add_mesh(layer, None, &req).unwrap();
    doc.apply(cmd).unwrap();
    let id = doc.kind_objects(rdoc::doc::KindFilter::Mesh)[0];
    let editable = editmesh::convert::cmd_make_editable(&doc, id).unwrap();
    doc.apply(editable).unwrap();
    (doc, id)
}

fn brush_room_doc() -> (Document, rdoc::model::Id) {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let brush = box_brush(DVec3::ZERO, DVec3::new(256.0, 256.0, 128.0), "m").unwrap();
    let mut o = rdoc::doc::new_object("room", layer, ObjectKind::Brush(brush));
    o.transform.position = DVec3::ZERO;
    doc.apply(Command::AddObjects { objects: vec![o] }).unwrap();
    let id = doc.kind_objects(rdoc::doc::KindFilter::Brush)[0];
    let cmd = editmesh::convert::cmd_brush_to_mesh(&doc, id).unwrap();
    doc.apply(cmd).unwrap();
    (doc, id)
}

fn mesh_of(doc: &Document, id: rdoc::model::Id) -> EditMesh {
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    let bytes = doc.files.read(&m.source).unwrap();
    editmesh::io::read(&bytes).unwrap()
}

fn apply_op(doc: &mut Document, id: rdoc::model::Id, op: &str, sel: serde_json::Value, params: serde_json::Value) -> editmesh::OpReport {
    let req: editmesh::OpRequest =
        serde_json::from_value(serde_json::json!({"op": op, "object": id.to_string(), "sel": sel, "params": params})).unwrap();
    let out = editmesh::plan(doc, &req).unwrap();
    doc.apply(out.command).unwrap();
    out.report
}

fn face_sel(ids: &[u32]) -> serde_json::Value {
    serde_json::json!({"mode": "face", "ids": ids})
}

fn inv(mesh: &EditMesh) -> editmesh::validate::Invariants {
    editmesh::validate::invariants(mesh)
}

#[test]
fn glb_round_trip_preserves_triangles_positions_and_shading() {
    for (name, spec) in specs() {
        let (g, m) = welded(&spec);
        let bytes = editmesh::io::write(&m);
        let back = editmesh::io::read(&bytes).unwrap();
        assert_eq!(back, m, "{name} does not round trip");
        let eval = editmesh::evaluate(&back);
        assert_eq!(eval.indices.len() / 3, g.indices.len() / 3, "{name} triangle count");
        for p in &eval.positions {
            let d = DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
            let near = g.positions.iter().any(|q| {
                (DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64) - d).length() < 1e-5
            });
            assert!(near, "{name} has a position the glb does not: {d:?}");
        }
        for n in &eval.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "{name} has a non-unit normal");
        }
    }
    let (_, m) = welded(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    let eval = editmesh::evaluate(&m);
    for n in &eval.normals {
        let dominant = n[0].abs().max(n[1].abs()).max(n[2].abs());
        assert!((dominant - 1.0).abs() < 1e-3, "a flat box face changed shading: {n:?}");
    }
}

#[test]
fn closed_fixtures_have_consistent_invariants() {
    let expected_euler = [("box", 2), ("cylinder", 2), ("stairs", 2), ("arch", 2), ("torus", 0)];
    for ((name, spec), (_, euler)) in specs().into_iter().zip(expected_euler) {
        let (_, m) = welded(&spec);
        let v = inv(&m);
        assert!(v.closed, "{name} is not closed");
        assert!(v.wound, "{name} is not consistently wound");
        assert_eq!(v.euler, euler, "{name} euler");
        assert!(v.degenerate.is_empty(), "{name} has degenerate faces");
        assert!(editmesh::validate::closed_edge_sum(&m), "{name} fails the edge-sum test");
    }
}

fn closed_ops() -> Vec<(&'static str, serde_json::Value, serde_json::Value)> {
    vec![
        ("translate", serde_json::json!({"mode": "face", "ids": []}), serde_json::json!({"translate": [16.0, 0.0, 8.0]})),
        ("rotate", serde_json::json!({"mode": "face", "ids": []}), serde_json::json!({"axis": [0.0, 0.0, 1.0], "angle_deg": 15.0})),
        ("scale", serde_json::json!({"mode": "face", "ids": []}), serde_json::json!({"scale": [1.5, 1.5, 1.5]})),
        (
            "translate",
            serde_json::json!({"mode": "vertex", "ids": []}),
            serde_json::json!({"translate": [4.0, 0.0, 0.0], "proportional": {"radius": 512.0, "falloff": "smooth"}}),
        ),
        ("merge", serde_json::json!({"mode": "vertex", "ids": [0]}), serde_json::json!({"mode": "by_distance", "distance": 0.0001})),
        ("merge", serde_json::json!({"mode": "edge", "ids": [0]}), serde_json::json!({"mode": "collapse"})),
        (
            "extrude_region",
            serde_json::json!({"mode": "face", "query": {"kind": "normal", "dir": [0.0, 0.0, 1.0], "max_angle_deg": 20.0}}),
            serde_json::json!({"distance": 16.0}),
        ),
        (
            "extrude_region",
            serde_json::json!({"mode": "face", "query": {"kind": "normal", "dir": [0.0, 0.0, 1.0], "max_angle_deg": 20.0}}),
            serde_json::json!({"distance": 16.0, "individual": true}),
        ),
        ("recalc_normals", serde_json::json!({"mode": "face", "ids": []}), serde_json::json!({"mode": "outside"})),
        ("recalc_normals", serde_json::json!({"mode": "face", "ids": []}), serde_json::json!({"mode": "inside"})),
        ("flip", serde_json::json!({"mode": "face", "ids": []}), serde_json::json!({})),
        ("shade", serde_json::json!({"mode": "face", "ids": []}), serde_json::json!({"mode": "smooth"})),
        ("shade", serde_json::json!({"mode": "face", "ids": []}), serde_json::json!({"mode": "flat"})),
        ("mark", serde_json::json!({"mode": "edge", "ids": []}), serde_json::json!({"kind": "sharp"})),
        ("mark", serde_json::json!({"mode": "edge", "ids": []}), serde_json::json!({"kind": "sharp", "clear": true})),
        ("mark", serde_json::json!({"mode": "edge", "ids": []}), serde_json::json!({"kind": "seam"})),
        ("mark", serde_json::json!({"mode": "edge", "ids": [0]}), serde_json::json!({"kind": "seam", "clear": true})),
        ("set_material", serde_json::json!({"mode": "face", "ids": [0]}), serde_json::json!({"slot": 0})),
        ("delete", serde_json::json!({"mode": "face", "ids": [0]}), serde_json::json!({"mode": "dissolve"})),
    ]
}

/// Closed-mesh requests that must fail with a specific error, never a skip:
/// a closed mesh has no boundary to fill, one vertex is nothing to merge, and
/// every fixture carries fewer than 99 material slots.
fn closed_op_errors() -> Vec<(&'static str, serde_json::Value, serde_json::Value, &'static str)> {
    vec![
        (
            "fill",
            serde_json::json!({"mode": "edge", "ids": []}),
            serde_json::json!({}),
            "fill needs boundary edges",
        ),
        (
            "merge",
            serde_json::json!({"mode": "vertex", "ids": [0]}),
            serde_json::json!({"mode": "center"}),
            "merge needs at least two vertices",
        ),
        (
            "set_material",
            serde_json::json!({"mode": "face", "ids": [0]}),
            serde_json::json!({"slot": 99}),
            "material slot 99 does not exist",
        ),
    ]
}

#[test]
fn closed_preserving_ops_keep_closed_wound_and_euler() {
    for name in all_names() {
        let base_euler = fixture_euler(name);
        for (op, sel, params) in closed_ops() {
            let (mut doc, id) = fixture_doc(name);
            let _ = apply_op(&mut doc, id, op, sel.clone(), params.clone());
            let after = mesh_of(&doc, id);
            let v = inv(&after);
            assert!(v.wound, "{name} {op} broke winding");
            assert!(v.degenerate.is_empty(), "{name} {op} left degenerate faces");
            assert!(v.closed, "{name} {op} opened the mesh");
            assert_eq!(v.euler, base_euler, "{name} {op} changed euler");
            assert!(editmesh::validate::closed_edge_sum(&after), "{name} {op} fails the edge-sum test");
        }
        for (op, sel, params, want) in closed_op_errors() {
            let (doc, id) = fixture_doc(name);
            let req: editmesh::OpRequest =
                serde_json::from_value(serde_json::json!({"op": op, "object": id.to_string(), "sel": sel, "params": params}))
                    .unwrap();
            let err = editmesh::plan(&doc, &req).expect_err(&format!("{name} {op} should fail"));
            assert!(err.to_string().contains(want), "{name} {op}: {err} names no {want}");
        }
    }
}

fn top_faces(doc: &Document, id: rdoc::model::Id) -> Vec<u32> {
    let m = mesh_of(doc, id);
    let adj = Adjacency::build(&m);
    rdoc::editmesh::select::resolve(
        &m,
        &adj,
        &serde_json::from_value(serde_json::json!({"mode": "face", "query": {"kind": "normal", "dir": [0.0, 0.0, 1.0], "max_angle_deg": 20.0}})).unwrap(),
    )
    .unwrap()
    .ids
}

#[test]
fn opening_ops_stay_wound_and_degenerate_free() {
    for name in all_names() {
        let (mut doc, id) = fixture_doc(name);
        apply_op(&mut doc, id, "delete", face_sel(&[0]), serde_json::json!({"mode": "faces"}));
        let open = mesh_of(&doc, id);
        let v = inv(&open);
        assert!(!v.closed, "{name}: deleting one face left it closed");
        assert!(v.wound, "{name}: delete broke winding");
        assert!(v.degenerate.is_empty(), "{name}: delete left degenerate faces");
        assert!(!v.boundary.is_empty(), "{name}: delete left no boundary");
        apply_op(&mut doc, id, "fill", serde_json::json!({"mode": "edge", "ids": []}), serde_json::json!({}));
        let refilled = mesh_of(&doc, id);
        let v = inv(&refilled);
        assert!(v.closed, "{name}: fill did not close the hole");
        assert!(v.wound, "{name}: fill broke winding");
        assert_eq!(v.euler, fixture_euler(name), "{name}: fill changed euler");
        assert!(v.degenerate.is_empty(), "{name}: fill left degenerate faces");

        let (mut doc, id) = fixture_doc(name);
        let top = top_faces(&doc, id);
        assert!(!top.is_empty(), "{name} has no top faces");
        apply_op(&mut doc, id, "delete", face_sel(&top), serde_json::json!({"mode": "faces"}));
        apply_op(&mut doc, id, "fill", serde_json::json!({"mode": "edge", "ids": []}), serde_json::json!({}));
        let v = inv(&mesh_of(&doc, id));
        assert!(v.closed, "{name}: fill did not close the top hole");
        assert!(v.wound, "{name}: top fill broke winding");
        assert!(v.degenerate.is_empty(), "{name}: top fill left degenerate faces");
    }
}

#[test]
fn dissolve_collapse_split_and_separate_behave() {
    for name in all_names() {
        let (mut doc, id) = fixture_doc(name);
        let all: Vec<u32> = {
            let m = mesh_of(&doc, id);
            (0..m.faces.len() as u32).collect()
        };
        apply_op(&mut doc, id, "split", face_sel(&all[..2.min(all.len())]), serde_json::json!({}));
        let v = inv(&mesh_of(&doc, id));
        assert!(!v.closed, "{name}: splitting faces left it closed");
        assert!(v.wound, "{name}: split broke winding");
        assert!(v.degenerate.is_empty(), "{name}: split left degenerate faces");

        let (mut doc, id) = fixture_doc(name);
        let m = mesh_of(&doc, id);
        let adj = Adjacency::build(&m);
        let edge = adj.edges.iter().position(|e| e.faces.len() == 2).unwrap() as u32;
        apply_op(&mut doc, id, "merge", serde_json::json!({"mode": "edge", "ids": [edge]}), serde_json::json!({"mode": "collapse"}));
        let v = inv(&mesh_of(&doc, id));
        assert!(v.closed, "{name}: collapsing one edge opened it");
        assert!(v.wound, "{name}: collapse broke winding");
        assert_eq!(v.euler, fixture_euler(name), "{name}: collapse changed euler");

        let (mut doc, id) = fixture_doc(name);
        let kept = mesh_of(&doc, id).faces.len() - 2;
        let req: editmesh::OpRequest = serde_json::from_value(
            serde_json::json!({"op": "separate", "object": id.to_string(), "sel": {"mode": "face", "ids": [0, 1]}, "params": {"mode": "selected"}}),
        )
        .unwrap();
        let out = editmesh::plan(&doc, &req).unwrap();
        doc.apply(out.command).unwrap();
        assert_eq!(out.report.faces, kept as u32, "{name}: separate kept the wrong face count");
        assert_eq!(doc.kind_objects(rdoc::doc::KindFilter::Mesh).len(), 2, "{name}: separate made no new object");

        let (mut doc, id) = fixture_doc(name);
        apply_op(&mut doc, id, "delete", face_sel(&[0]), serde_json::json!({"mode": "dissolve"}));
        let v = inv(&mesh_of(&doc, id));
        assert!(v.wound, "{name}: dissolve broke winding");
        assert!(v.degenerate.is_empty(), "{name}: dissolve left degenerate faces");
    }
}

#[test]
fn box_query_uses_the_contract_name() {
    let (doc, id) = fixture_doc("box");
    let m = mesh_of(&doc, id);
    let adj = Adjacency::build(&m);
    let sel: rdoc::editmesh::select::Sel = serde_json::from_value(
        serde_json::json!({"mode": "vertex", "query": {"kind": "box", "min": [-1000.0, -1000.0, -1000.0], "max": [1000.0, 1000.0, 1000.0]}}),
    )
    .unwrap();
    let resolved = rdoc::editmesh::select::resolve(&m, &adj, &sel).unwrap();
    assert_eq!(resolved.ids.len(), m.positions.len() as usize);
    let top: rdoc::editmesh::select::Sel = serde_json::from_value(
        serde_json::json!({"mode": "face", "query": {"kind": "box", "min": [-100.0, -100.0, 0.1], "max": [100.0, 100.0, 100.0]}}),
    )
    .unwrap();
    let resolved_top = rdoc::editmesh::select::resolve(&m, &adj, &top).unwrap();
    assert_eq!(resolved_top.ids.len(), 2, "the top box holds the two top triangles");
    for id in &resolved_top.ids {
        assert!(m.face_normal(*id as usize).z > 0.99);
    }
    let legacy: rdoc::editmesh::select::Sel = serde_json::from_value(
        serde_json::json!({"mode": "vertex", "query": {"kind": "bounds", "min": [-1000.0, -1000.0, -1000.0], "max": [1000.0, 1000.0, 1000.0]}}),
    )
    .unwrap();
    let resolved_legacy = rdoc::editmesh::select::resolve(&m, &adj, &legacy).unwrap();
    assert_eq!(resolved_legacy.ids, resolved.ids);
}

#[test]
fn ops_are_deterministic_down_to_the_byte() {
    let (_, spec) = &specs()[1];
    let run = || {
        let (mut doc, id) = doc_with(spec);
        apply_op(
            &mut doc,
            id,
            "extrude_region",
            serde_json::json!({"mode": "face", "query": {"kind": "normal", "dir": [0.0, 0.0, 1.0], "max_angle_deg": 5.0}}),
            serde_json::json!({"distance": 16.0}),
        );
        apply_op(&mut doc, id, "translate", serde_json::json!({"mode": "vertex", "ids": []}), serde_json::json!({"translate": [1.0, 2.0, 3.0]}));
        let m = mesh_of(&doc, id);
        editmesh::io::write(&m)
    };
    assert_eq!(run(), run());
}

#[test]
fn brush_to_mesh_keeps_one_ngon_per_face() {
    let (doc, id) = brush_room_doc();
    let m = mesh_of(&doc, id);
    assert_eq!(m.faces.len(), 6);
    assert!(m.faces.iter().all(|f| f.len == 4));
    let v = inv(&m);
    assert!(v.closed && v.wound && v.euler == 2);
    assert!(m.corners.iter().all(|c| c.uv.iter().all(|u| u.is_finite())));
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(mesh) = &o.kind else { panic!("not a mesh") };
    assert_eq!(mesh.lightmap_scale, 0.0);
    assert!(doc.object_bounds(id).is_some());
}

#[test]
fn editable_spec_output_shares_vertices_and_marks_sharp() {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let req: MeshRequest = serde_json::from_str(r#"{"spec":{"shape":"box","size":[64,64,64]},"editable":true}"#).unwrap();
    doc.apply(cmd_add_mesh(layer, None, &req).unwrap()).unwrap();
    let id = doc.kind_objects(rdoc::doc::KindFilter::Mesh)[0];
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    assert!(m.source.ends_with(".r5mesh"));
    let mesh = mesh_of(&doc, id);
    assert_eq!(mesh.positions.len(), 8, "the box shares its eight corners");
    assert!(!mesh.edge_flags.is_empty(), "flat-soup edges are marked");
    assert!(mesh.edge_flags.iter().all(|(_, _, f)| f.contains(rdoc::editmesh::EdgeFlags::SHARP)));
    let eval = editmesh::evaluate(&mesh);
    assert_eq!(eval.indices.len() / 3, 12);
}

#[test]
fn old_map_json_without_role_loads_unchanged() {
    let text = r#"{"source":"m.glb","material_map":{},"collision":"triangles","contents":1,"cast_shadows":true,"lightmap_scale":0.0,"render":true}"#;
    let mesh: Mesh = serde_json::from_str(text).unwrap();
    assert_eq!(mesh.role, rdoc::model::MeshRole::Normal);
    assert_eq!(serde_json::to_value(&mesh).unwrap().get("role"), None);
    let cutter: Mesh = serde_json::from_str(&text.replace("\"render\":true", "\"render\":true,\"role\":\"cutter\"")).unwrap();
    assert_eq!(cutter.role, rdoc::model::MeshRole::Cutter);
}

#[test]
fn bad_requests_are_errors_not_panics() {
    let (doc, id) = doc_with(&specs()[0].1);
    let bad_op: editmesh::OpRequest = serde_json::from_value(
        serde_json::json!({"op": "nope", "object": id.to_string(), "sel": {"mode": "face", "ids": []}, "params": {}}),
    )
    .unwrap();
    assert!(editmesh::plan(&doc, &bad_op).is_err());
    let bad_sel: editmesh::OpRequest = serde_json::from_value(
        serde_json::json!({"op": "flip", "object": id.to_string(), "sel": {"mode": "face", "ids": [9999]}, "params": {}}),
    )
    .unwrap();
    assert!(editmesh::plan(&doc, &bad_sel).is_err());
    let unknown_param: editmesh::OpRequest = serde_json::from_value(
        serde_json::json!({"op": "flip", "object": id.to_string(), "sel": {"mode": "face", "ids": []}, "params": {"bogus": 1}}),
    )
    .unwrap();
    assert!(editmesh::plan(&doc, &unknown_param).is_err());
    assert!(editmesh::io::read(&[]).is_err());
    assert!(editmesh::io::read(b"R5MS\x01\x00\x00\x00").is_err());
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[test]
fn fuzzed_r5mesh_bytes_never_panic_the_loader() {
    let (_, m) = welded(&MeshSpec::Torus { major: 64.0, minor: 16.0, segments: 16, sides: 8 });
    let good = editmesh::io::write(&m);
    let mut rng = Rng(0x1234_5678);
    for _ in 0..500 {
        let mut bytes = good.clone();
        let cut = (rng.next() as usize) % (bytes.len() + 1);
        bytes.truncate(cut);
        let mutations = 1 + (rng.next() % 4) as usize;
        for _ in 0..mutations {
            if bytes.is_empty() {
                break;
            }
            let at = (rng.next() as usize) % bytes.len();
            bytes[at] = rng.next() as u8;
        }
        let _ = editmesh::io::read(&bytes);
    }
    let mut rng = Rng(0xABCD_EF01);
    for _ in 0..200 {
        let len = (rng.next() % 256) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = editmesh::io::read(&bytes);
    }
    let back = editmesh::io::read(&good).unwrap();
    assert_eq!(back, m, "the unmutated file still reads");
}

#[test]
fn glb_split_reader_keeps_materials() {
    let g = glb_of(&MeshSpec::Box { size: [8.0, 8.0, 8.0] });
    let bytes = glb::write(&g).unwrap();
    let prims = glb::read_split(&bytes).unwrap();
    assert_eq!(prims.len(), 1);
    assert_eq!(prims[0].material, "m");
    assert_eq!(prims[0].indices.len(), 36);
}

/// A document holding one glb-sourced mesh plus its file.
fn glb_doc(spec: &MeshSpec) -> (Document, rdoc::model::Id) {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let req: MeshRequest = serde_json::from_str(&format!("{{\"spec\":{}}}", serde_json::to_string(spec).unwrap())).unwrap();
    let cmd = cmd_add_mesh(layer, None, &req).unwrap();
    doc.apply(cmd).unwrap();
    let id = doc.kind_objects(rdoc::doc::KindFilter::Mesh)[0];
    (doc, id)
}

fn source_of(doc: &Document, id: rdoc::model::Id) -> (String, Vec<u8>) {
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    (m.source.clone(), doc.files.read(&m.source).unwrap())
}

fn tris_of(prims: &[GlbMesh]) -> usize {
    prims.iter().map(|p| p.indices.len() / 3).sum()
}

fn bounds_of(prims: &[GlbMesh]) -> (DVec3, DVec3) {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for p in prims {
        for q in &p.positions {
            let v = DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    (lo, hi)
}

#[test]
fn triangle_readers_share_the_one_branch_point() {
    let spec = MeshSpec::Box { size: [64.0, 64.0, 64.0] };
    let mirror: editmesh::Modifier =
        serde_json::from_value(serde_json::json!({"type": "mirror", "axes": [true, false, false]})).unwrap();
    let disabled: editmesh::Modifier =
        serde_json::from_value(serde_json::json!({"enabled": false, "type": "mirror", "axes": [true, false, false]})).unwrap();
    for (doc, id) in [doc_with(&spec), glb_doc(&spec)] {
        let (source, bytes) = source_of(&doc, id);
        let ctx = editmesh::EvalContext::from_doc(&doc, id);
        let plain = match editmesh::load_triangles(&bytes, &source).unwrap() {
            editmesh::LoadedMesh::Editable(base) => editmesh::convert::evaluated_primitives(&base),
            editmesh::LoadedMesh::Soup(prims) => prims,
        };
        assert_eq!(tris_of(&plain), 12);
        assert_eq!(plain, editmesh::load_triangles_stacked(&bytes, &source, &[], &ctx).unwrap());
        assert_eq!(plain, editmesh::load_triangles_stacked(&bytes, &source, &[disabled.clone()], &ctx).unwrap());
        let stacked = editmesh::load_triangles_stacked(&bytes, &source, &[mirror.clone()], &ctx).unwrap();
        assert_eq!(tris_of(&stacked), 24, "{source} runs its stack through the branch point");
        let base = match editmesh::load_triangles(&bytes, &source).unwrap() {
            editmesh::LoadedMesh::Editable(base) => base,
            editmesh::LoadedMesh::Soup(prims) => editmesh::convert::weld_primitives(&prims).unwrap(),
        };
        let mut cache = editmesh::StackCache::new();
        let cached_stacked =
            editmesh::convert::evaluated_primitives(&cache.evaluate(&base, &[mirror.clone()], &ctx, false).unwrap());
        assert_eq!(tris_of(&cached_stacked), 24, "{source} runs its cached stack through the branch point");
        assert_eq!(cached_stacked, stacked, "{source} caches the same stack");
        let bounds = doc.object_bounds(id).expect("{source} has bounds");
        let (lo, hi) = bounds_of(&plain);
        for a in 0..3 {
            assert!((bounds.min.to_array()[a] - lo.to_array()[a]).abs() < 1e-9, "{source} bounds agree");
            assert!((bounds.max.to_array()[a] - hi.to_array()[a]).abs() < 1e-9, "{source} bounds agree");
        }
    }
}

/// The format decision lives in exactly one place: every triangle reader
/// routes through `editmesh::load_triangles`, so no other production file
/// may dispatch on the extension or call a glb reader.
#[test]
fn triangle_loading_has_one_branch_point() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let files = [
        "src/editmesh/convert.rs",
        "src/editmesh/modifiers.rs",
        "src/editmesh/session.rs",
        "src/editmesh/request.rs",
        "src/doc.rs",
        "../rcomp/src/lib.rs",
        "../r5ed-wasm/src/lib.rs",
    ];
    let mut branch_hits = 0;
    for f in files {
        let src = std::fs::read_to_string(root.join(f)).unwrap();
        branch_hits += src.matches("ends_with(\".r5mesh\")").count();
        assert_eq!(src.matches("read_glb").count(), 0, "{f} calls a glb reader past the branch point");
        if f != "src/editmesh/convert.rs" {
            assert_eq!(src.matches("read_split").count(), 0, "{f} calls a glb reader past the branch point");
        }
    }
    assert_eq!(branch_hits, 1, "the format decision is not singular");
    let convert = std::fs::read_to_string(root.join("src/editmesh/convert.rs")).unwrap();
    assert_eq!(convert.matches("read_split").count(), 1, "convert reads glb in one place");
    let branch_at = convert.find("ends_with(\".r5mesh\")").unwrap();
    let loader_at = convert.find("pub fn load_triangles(").unwrap();
    let stacked_at = convert.find("pub fn load_triangles_stacked(").unwrap();
    assert!(loader_at < branch_at && branch_at < stacked_at, "the branch sits inside load_triangles");
}

fn fnv1a(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The parity script (`web/src/core/wasmParity.test.mjs` replays it through
/// the wasm core against `testdata/mesh_parity.json`): an editable box, a
/// translate, an extrude of the top. The hashes pin the `.r5mesh` bytes every
/// build must produce; anything order- or platform-dependent fails here.
#[test]
fn op_bytes_match_committed_goldens() {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let req: MeshRequest =
        serde_json::from_str(r#"{"spec":{"shape":"box","size":[64,64,64]},"editable":true}"#).unwrap();
    doc.apply(cmd_add_mesh(layer, None, &req).unwrap()).unwrap();
    let id = doc.kind_objects(rdoc::doc::KindFilter::Mesh)[0];
    let file_bytes = |doc: &Document| {
        let o = doc.object(id).unwrap();
        let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
        doc.files.read(&m.source).unwrap()
    };
    assert_eq!(fnv1a(&file_bytes(&doc)), "cb0f74027b56e841");
    apply_op(
        &mut doc,
        id,
        "translate",
        serde_json::json!({"mode": "face", "ids": []}),
        serde_json::json!({"translate": [16.0, 0.0, 8.0]}),
    );
    assert_eq!(fnv1a(&file_bytes(&doc)), "c4951ff4567ef941");
    apply_op(
        &mut doc,
        id,
        "extrude_region",
        serde_json::json!({"mode": "face", "query": {"kind": "normal", "dir": [0.0, 0.0, 1.0], "max_angle_deg": 20.0}}),
        serde_json::json!({"distance": 16.0}),
    );
    assert_eq!(fnv1a(&file_bytes(&doc)), "5538da89c6229bfd");
}

fn corrupt_file(doc: &mut Document, id: rdoc::model::Id) -> String {
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    let path = m.source.clone();
    doc.apply(Command::SetFile { path: path.clone(), bytes: Some(b"not a mesh file".to_vec()) }).unwrap();
    path
}

#[test]
fn corrupt_glb_names_its_file_once_on_both_modifier_paths() {
    let mirror = serde_json::json!({"type": "mirror"});
    let add = |id: rdoc::model::Id| -> editmesh::OpRequest {
        serde_json::from_value(
            serde_json::json!({"op": "modifier_add", "object": id.to_string(), "sel": {"mode": "face", "ids": []}, "params": {"modifier": mirror}}),
        )
        .unwrap()
    };
    let (mut doc, id) = glb_doc(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    let source = corrupt_file(&mut doc, id);
    let err = editmesh::plan(&doc, &add(id)).expect_err("a corrupt glb fails the modifier report");
    assert_eq!(err.to_string().matches(source.as_str()).count(), 1, "plan_modifier names {source} once: {err}");

    let (mut doc, id) = glb_doc(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    let out = editmesh::plan(&doc, &add(id)).unwrap();
    doc.apply(out.command).unwrap();
    let source = corrupt_file(&mut doc, id);
    let apply: editmesh::OpRequest = serde_json::from_value(
        serde_json::json!({"op": "modifier_apply", "object": id.to_string(), "sel": {"mode": "face", "ids": []}, "params": {"index": 0}}),
    )
    .unwrap();
    let err = editmesh::plan(&doc, &apply).expect_err("a corrupt glb fails modifier apply");
    assert_eq!(err.to_string().matches(source.as_str()).count(), 1, "plan_apply names {source} once: {err}");
}
#[test]
fn every_editable_primitive_is_a_clean_solid() {
    // Pole and apex corners weld together; the triangles they collapse are
    // not faces and must not survive into the editable mesh.
    let shapes = [
        MeshSpec::Box { size: [64.0, 64.0, 64.0] },
        MeshSpec::Cylinder { radius: 32.0, height: 96.0, segments: 16, caps: true },
        MeshSpec::Sphere { radius: 160.0, segments: 12, rings: 8 },
    ];
    for spec in &shapes {
        let mesh = editmesh::convert::editable_from_spec(spec, "t", "m", 1.0 / 512.0).unwrap();
        let inv = editmesh::validate::invariants(&mesh);
        assert!(inv.closed && inv.degenerate.is_empty() && inv.non_manifold.is_empty(), "{spec:?}: {inv:?}");
        assert_eq!(inv.euler, 2, "{spec:?}");
    }
}
