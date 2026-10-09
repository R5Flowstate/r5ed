//! M2 gates: the edit session API. Begin/preview/commit walks a drag without a
//! document change until release; a commit after a preview writes the same
//! bytes as the direct `meshOp` path; the CPU pick hits the expected
//! vertex, edge and face on a box from a fixed camera.

use glam::{DMat4, DVec3};
use rdoc::command::Command;
use rdoc::doc::{Document, KindFilter};
use rdoc::editmesh::{self, Adjacency, PickQuery, PickReply, Session};
use rdoc::meshgen::{cmd_add_mesh, MeshRequest, MeshSpec};

fn doc_with_box() -> (Document, rdoc::model::Id) {
    let spec = MeshSpec::Box { size: [64.0, 64.0, 64.0] };
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let req: MeshRequest =
        serde_json::from_str(&format!("{{\"spec\":{}}}", serde_json::to_string(&spec).unwrap())).unwrap();
    doc.apply(cmd_add_mesh(layer, None, &req).unwrap()).unwrap();
    let id = doc.kind_objects(KindFilter::Mesh)[0];
    doc.apply(editmesh::convert::cmd_make_editable(&doc, id).unwrap()).unwrap();
    (doc, id)
}

fn op_req(doc_id: rdoc::model::Id, op: &str, sel: serde_json::Value, params: serde_json::Value) -> editmesh::OpRequest {
    serde_json::from_value(serde_json::json!({"op": op, "object": doc_id.to_string(), "sel": sel, "params": params})).unwrap()
}

fn mesh_of(doc: &Document, id: rdoc::model::Id) -> editmesh::EditMesh {
    let o = doc.object(id).unwrap();
    let rdoc::model::ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    let bytes = doc.files.read(&m.source).unwrap();
    editmesh::io::read(&bytes).unwrap()
}

#[test]
fn begin_preview_commit_is_one_set_file() {
    let (doc, id) = doc_with_box();
    let mut session = Session::begin(&doc, id).unwrap();
    let counts = session.counts();
    assert_eq!((counts.verts, counts.faces), (8, 12));
    assert_eq!(counts.edges, 18);
    let sel: editmesh::select::Sel = serde_json::from_value(
        serde_json::json!({"mode": "face", "query": {"kind": "normal", "dir": [0.0, 0.0, 1.0], "max_angle_deg": 5.0}}),
    )
    .unwrap();
    let selected = session.select(&sel).unwrap();
    assert_eq!(selected.ids.len(), 2);
    let req = op_req(id, "translate", serde_json::json!({"mode": "face", "ids": selected.ids}), serde_json::json!({"translate": [0.0, 0.0, 16.0]}));
    let preview = session.preview(&doc, &req).unwrap();
    assert_eq!(preview.selection.ids, selected.ids);
    let Command::SetFile { path, bytes: Some(bytes) } = session.commit() else {
        panic!("a commit is one SetFile, not a batch");
    };
    assert!(path.ends_with(".r5mesh"));
    let mut doc = doc;
    doc.apply(Command::SetFile { path, bytes: Some(bytes) }).unwrap();
    let after = mesh_of(&doc, id);
    assert_eq!(after.positions.len(), 8);
    let top_z = after.positions.iter().map(|p| p.z).fold(f64::NEG_INFINITY, f64::max);
    assert!((top_z - 48.0).abs() < 1e-9, "the top moved up by 16: {top_z}");
}

#[test]
fn commit_after_preview_matches_direct_mesh_op_bytes() {
    let sel = serde_json::json!({"mode": "vertex", "ids": []});
    let params = serde_json::json!({"translate": [4.0, -8.0, 12.0]});
    let (doc, id) = doc_with_box();
    let mut session = Session::begin(&doc, id).unwrap();
    session.preview(&doc, &op_req(id, "translate", sel.clone(), params.clone())).unwrap();
    let Command::SetFile { bytes: Some(session_bytes), .. } = session.commit() else {
        panic!("a commit is one SetFile");
    };
    let (doc, id) = doc_with_box();
    let out = editmesh::plan(&doc, &op_req(id, "translate", sel, params)).unwrap();
    let Command::Batch { commands, .. } = out.command else { panic!("plan wraps in a batch") };
    let Command::SetFile { bytes: Some(plan_bytes), .. } = &commands[0] else { panic!("plan starts with a SetFile") };
    assert_eq!(session_bytes, *plan_bytes);
}

#[test]
fn preview_rejects_a_foreign_object_and_separate() {
    let (doc, id) = doc_with_box();
    let mut session = Session::begin(&doc, id).unwrap();
    let foreign = op_req(rdoc::model::Id::new_v4(), "translate", serde_json::json!({"mode": "face", "ids": [0]}), serde_json::json!({}));
    assert!(session.preview(&doc, &foreign).is_err());
    let separate = op_req(id, "separate", serde_json::json!({"mode": "face", "ids": [0]}), serde_json::json!({}));
    assert!(session.preview(&doc, &separate).is_err());
}

#[test]
fn buffers_keep_topology_hash_across_a_drag() {
    let (doc, id) = doc_with_box();
    let mut session = Session::begin(&doc, id).unwrap();
    let before = session.buffers();
    assert_eq!(before.tris.len(), 12 * 3);
    assert_eq!(before.poly_edges.len(), 18);
    assert_eq!(before.face_centres.len(), 12);
    assert_eq!(before.vert_sel.len(), 8);
    session
        .preview(&doc, &op_req(id, "translate", serde_json::json!({"mode": "vertex", "ids": []}), serde_json::json!({"translate": [1.0, 0.0, 0.0]})))
        .unwrap();
    let after = session.buffers();
    assert_eq!(before.topo_hash, after.topo_hash, "moving positions is not a topology change");
    assert_ne!(before.verts, after.verts);
}

#[test]
fn hide_and_reveal_move_the_drawn_lists() {
    let (doc, id) = doc_with_box();
    let mut session = Session::begin(&doc, id).unwrap();
    let full = session.buffers();
    let sel: editmesh::select::Sel =
        serde_json::from_value(serde_json::json!({"mode": "face", "ids": [0]})).unwrap();
    session.select(&sel).unwrap();
    session.hide();
    let hidden = session.buffers();
    assert!(hidden.tris.len() < full.tris.len());
    assert_ne!(hidden.topo_hash, full.topo_hash);
    session.reveal();
    let back = session.buffers();
    assert_eq!(back.tris.len(), full.tris.len());
    assert_eq!(back.topo_hash, full.topo_hash);
}

#[test]
fn grow_shrink_and_linked_queries_resolve() {
    let (doc, id) = doc_with_box();
    let mesh = mesh_of(&doc, id);
    let adj = Adjacency::build(&mesh);
    let grown: editmesh::select::Selection = editmesh::select::resolve(
        &mesh,
        &adj,
        &serde_json::from_value(serde_json::json!({"mode": "vertex", "ids": [0], "query": {"kind": "grow"}})).unwrap(),
    )
    .unwrap();
    assert!(grown.ids.len() > 1);
    let shrunk = editmesh::select::resolve(
        &mesh,
        &adj,
        &editmesh::select::Sel {
            mode: editmesh::select::SelMode::Vertex,
            ids: grown.ids.clone(),
            query: Some(editmesh::select::SelQuery::Shrink),
        },
    )
    .unwrap();
    assert!(shrunk.ids.len() < grown.ids.len());
    let linked = editmesh::select::resolve(
        &mesh,
        &adj,
        &editmesh::select::Sel {
            mode: editmesh::select::SelMode::Vertex,
            ids: vec![0],
            query: Some(editmesh::select::SelQuery::LinkedSel),
        },
    )
    .unwrap();
    assert_eq!(linked.ids.len(), mesh.positions.len() as usize, "the box is one linked island");
}

/// A fixed camera over the box: the eye off the -Y corner, looking at the
/// origin, +Z up.
fn camera() -> (DVec3, DMat4) {
    let eye = DVec3::new(0.0, -160.0, 80.0);
    let target = DVec3::ZERO;
    let view = DMat4::look_at_rh(eye, target, DVec3::Z);
    let proj = DMat4::perspective_rh(60.0_f64.to_radians(), 1.0, 1.0, 10000.0);
    (eye, proj * view)
}

fn pick_query(
    session_mode: editmesh::select::SelMode,
    eye: DVec3,
    view_proj: DMat4,
    x: f64,
    y: f64,
    xray: bool,
) -> PickQuery {
    let dir = (DVec3::ZERO - eye).normalize();
    serde_json::from_value(serde_json::json!({
        "mode": session_mode,
        "x": x, "y": y,
        "eye": [eye.x, eye.y, eye.z],
        "view_proj": view_proj.to_cols_array(),
        "width": 800.0, "height": 800.0,
        "ray_origin": [eye.x, eye.y, eye.z],
        "ray_dir": [dir.x, dir.y, dir.z],
        "xray": xray,
        "threshold_px": 4.0,
    }))
    .unwrap()
}

fn project_all(mesh: &editmesh::EditMesh, view_proj: DMat4) -> Vec<[f64; 2]> {
    mesh.positions
        .iter()
        .map(|p| {
            let c = view_proj * glam::DVec4::new(p.x, p.y, p.z, 1.0);
            [(c.x / c.w * 0.5 + 0.5) * 800.0, (0.5 - c.y / c.w * 0.5) * 800.0]
        })
        .collect()
}

#[test]
fn pick_hits_the_expected_vertex_edge_and_face() {
    let (doc, id) = doc_with_box();
    let session = Session::begin(&doc, id).unwrap();
    let mesh = mesh_of(&doc, id);
    let (eye, view_proj) = camera();
    let screen = project_all(&mesh, view_proj);

    let front: Vec<usize> = mesh.positions.iter().enumerate().filter(|(_, p)| p.y < -31.0).map(|(i, _)| i).collect();
    assert_eq!(front.len(), 4);
    let back: Vec<usize> = mesh
        .positions
        .iter()
        .enumerate()
        .filter(|(_, p)| p.y > 31.0 && p.z < -31.0)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(back.len(), 2);

    let v = front[0];
    let PickReply::Vertex { id: hit, .. } =
        session.pick(&pick_query(editmesh::select::SelMode::Vertex, eye, view_proj, screen[v][0], screen[v][1], false)).unwrap()
    else {
        panic!("a front vertex picks");
    };
    assert_eq!(hit as usize, v);

    let b = back[0];
    let reply = session.pick(&pick_query(editmesh::select::SelMode::Vertex, eye, view_proj, screen[b][0], screen[b][1], false)).unwrap();
    assert!(
        !matches!(reply, PickReply::Vertex { id, .. } if id == b as u32),
        "an occluded vertex is excluded without x-ray: {reply:?}"
    );
    let PickReply::Vertex { id: seen, .. } =
        session.pick(&pick_query(editmesh::select::SelMode::Vertex, eye, view_proj, screen[b][0], screen[b][1], true)).unwrap()
    else {
        panic!("x-ray sees the back vertex");
    };
    assert_eq!(seen as usize, b);

    let adj = Adjacency::build(&mesh);
    let (edge_id, (ea, eb)) = adj
        .edges
        .iter()
        .enumerate()
        .find(|(_, e)| mesh.positions[e.a as usize].y < -31.0 && mesh.positions[e.b as usize].y < -31.0)
        .map(|(i, e)| (i as u32, (e.a as usize, e.b as usize)))
        .expect("a front edge");
    let mx = (screen[ea][0] + screen[eb][0]) * 0.5;
    let my = (screen[ea][1] + screen[eb][1]) * 0.5;
    let PickReply::Edge { id: edge_hit, .. } =
        session.pick(&pick_query(editmesh::select::SelMode::Edge, eye, view_proj, mx, my, false)).unwrap()
    else {
        panic!("a front edge picks");
    };
    assert_eq!(edge_hit, edge_id);

    let dir = (DVec3::ZERO - eye).normalize();
    let face_query: PickQuery = serde_json::from_value(serde_json::json!({
        "mode": "face", "x": 400.0, "y": 400.0,
        "eye": [eye.x, eye.y, eye.z],
        "view_proj": view_proj.to_cols_array(),
        "width": 800.0, "height": 800.0,
        "ray_origin": [eye.x, eye.y, eye.z],
        "ray_dir": [dir.x, dir.y, dir.z],
    }))
    .unwrap();
    let PickReply::Face { id: face, .. } = session.pick(&face_query).unwrap() else {
        panic!("the centre ray hits the box");
    };
    let n = mesh.face_normal(face as usize);
    assert!(n.dot(eye - DVec3::ZERO) > 0.0, "the hit faces the camera: {n:?}");
}
