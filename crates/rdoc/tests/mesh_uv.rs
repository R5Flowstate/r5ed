//! M8 gates: box/cylinder/planar projection, seams, LSCM unwrap, packing,
//! reset, determinism, world space, the stretch report, and bad input.

use glam::DVec3;
use rdoc::brush::{box_brush, project_uv, tile_uv};
use rdoc::command::Command;
use rdoc::doc::{Document, KindFilter};
use rdoc::editmesh::{self, Adjacency, EditMesh};
use rdoc::meshgen::{cmd_add_mesh, MeshRequest, MeshSpec};
use rdoc::model::ObjectKind;

fn doc_with(spec: &MeshSpec) -> (Document, rdoc::model::Id) {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let req: MeshRequest =
        serde_json::from_str(&format!("{{\"spec\":{}}}", serde_json::to_string(spec).unwrap())).unwrap();
    let cmd = cmd_add_mesh(layer, None, &req).unwrap();
    doc.apply(cmd).unwrap();
    let id = doc.kind_objects(KindFilter::Mesh)[0];
    let editable = editmesh::convert::cmd_make_editable(&doc, id).unwrap();
    doc.apply(editable).unwrap();
    (doc, id)
}

fn mesh_of(doc: &Document, id: rdoc::model::Id) -> EditMesh {
    let o = doc.object(id).unwrap();
    let ObjectKind::Mesh(m) = &o.kind else { panic!("not a mesh") };
    let bytes = doc.files.read(&m.source).unwrap();
    editmesh::io::read(&bytes).unwrap()
}

fn apply_op(
    doc: &mut Document,
    id: rdoc::model::Id,
    op: &str,
    sel: serde_json::Value,
    params: serde_json::Value,
) -> editmesh::OpReport {
    let req: editmesh::OpRequest =
        serde_json::from_value(serde_json::json!({"op": op, "object": id.to_string(), "sel": sel, "params": params}))
            .unwrap();
    let out = editmesh::plan(doc, &req).unwrap();
    doc.apply(out.command).unwrap();
    out.report
}

fn all_faces() -> serde_json::Value {
    serde_json::json!({"mode": "face", "ids": []})
}

fn tri_area(p: &[DVec3; 3]) -> f64 {
    ((p[1] - p[0]).cross(p[2] - p[0])).length() * 0.5
}

fn uv_area(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs() * 0.5
}

#[test]
fn box_projection_matches_brush_corner_for_corner() {
    let (mut doc, id) = doc_with(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    apply_op(&mut doc, id, "uv_box", all_faces(), serde_json::json!({}));
    let m = mesh_of(&doc, id);
    let brush = box_brush(DVec3::splat(-32.0), DVec3::splat(32.0), "m").unwrap();
    for (fi, _) in m.faces.iter().enumerate() {
        let n = m.face_normal(fi);
        let bf = brush.faces.iter().find(|f| f.normal == n).expect("the brush has the face");
        for c in m.face_corners(fi) {
            let p = m.positions[c.vertex as usize];
            let want = tile_uv(project_uv(&bf.uv, n, p));
            assert_eq!(c.uv, [want[0] as f32, want[1] as f32], "corner uv drifted on face {fi}");
        }
    }
    let seams = m.edge_flags.iter().filter(|(_, _, f)| f.contains(rdoc::editmesh::EdgeFlags::SEAM)).count();
    assert_eq!(seams, 12, "the box marks its twelve edges, not the six diagonals");
    let v = editmesh::validate::invariants(&m);
    assert!(v.closed && v.wound && v.euler == 2 && v.degenerate.is_empty());
}

#[test]
fn cylinder_projection_has_one_side_seam_and_uniform_density() {
    let spec = MeshSpec::Cylinder { radius: 32.0, height: 64.0, segments: 32, caps: false };
    let (mut doc, id) = doc_with(&spec);
    let before = mesh_of(&doc, id);
    apply_op(&mut doc, id, "uv_cylinder", all_faces(), serde_json::json!({"seam_angle_deg": 180.0}));
    let m = mesh_of(&doc, id);
    assert_eq!(m.positions, before.positions, "a UV op moved vertices");
    let adj = Adjacency::build(&m);
    let mut side_seams = 0;
    for (a, b, f) in &m.edge_flags {
        if !f.contains(rdoc::editmesh::EdgeFlags::SEAM) {
            continue;
        }
        let e = adj.edge(*a, *b).unwrap();
        assert_eq!(e.faces.len(), 2, "a seam marks a boundary edge");
        side_seams += 1;
    }
    assert_eq!(side_seams, 1, "the tube carries exactly one seam edge");
    let t = 128.0;
    let ratios: Vec<f64> = {
        let e = editmesh::evaluate(&m);
        e.indices
            .chunks_exact(3)
            .map(|ix| {
                let p = |i: u32| {
                    let q = e.positions[i as usize];
                    DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64)
                };
                let u = |i: u32| {
                    let q = e.uvs[i as usize];
                    [q[0] as f64, q[1] as f64]
                };
                let (a, b, c) = (p(ix[0]), p(ix[1]), p(ix[2]));
                uv_area(u(ix[0]), u(ix[1]), u(ix[2])) * t * t / tri_area(&[a, b, c])
            })
            .collect()
    };
    assert!(!ratios.is_empty());
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for r in &ratios {
        lo = lo.min(*r);
        hi = hi.max(*r);
    }
    assert!((hi - lo) / hi < 0.01, "texel density varies: {lo}..{hi}");
    let mean: f64 = ratios.iter().sum::<f64>() / ratios.len() as f64;
    assert!((mean - 1.0).abs() < 0.01, "density is not world scale: {mean}");
}

fn vertical_edge_at(m: &EditMesh, x: f64, y: f64) -> u32 {
    let adj = Adjacency::build(m);
    let at = |x: f64, y: f64, z: f64| {
        m.positions.iter().position(|p| (p.x - x).abs() < 1e-9 && (p.y - y).abs() < 1e-9 && (p.z - z).abs() < 1e-9).unwrap()
            as u32
    };
    adj.index_of(at(x, y, 0.0), at(x, y, 64.0))
}

#[test]
fn unwrap_of_a_slit_cylinder_is_angle_preserving() {
    let spec = MeshSpec::Cylinder { radius: 32.0, height: 64.0, segments: 16, caps: false };
    let (mut doc, id) = doc_with(&spec);
    let edge = vertical_edge_at(&mesh_of(&doc, id), 32.0, 0.0);
    apply_op(&mut doc, id, "mark", serde_json::json!({"mode": "edge", "ids": [edge]}), serde_json::json!({"kind": "seam"}));
    apply_op(&mut doc, id, "uv_unwrap", all_faces(), serde_json::json!({}));
    let m = mesh_of(&doc, id);
    let mut worst: f64 = 0.0;
    let e = editmesh::evaluate(&m);
    for t in e.indices.chunks_exact(3) {
        let p = |i: u32| {
            let q = e.positions[i as usize];
            DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64)
        };
        let u = |i: u32| {
            let q = e.uvs[i as usize];
            glam::DVec2::new(q[0] as f64, q[1] as f64)
        };
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        let (ua, ub, uc) = (u(t[0]), u(t[1]), u(t[2]));
        for (p_, q_, r, u_, v_, w_) in [(a, b, c, ua, ub, uc), (b, c, a, ub, uc, ua), (c, a, b, uc, ua, ub)] {
            let d3 = corner_angle(p_, q_, r);
            let (du, dv) = (u_ - v_, w_ - v_);
            assert!(du.length_squared() > 1e-30 && dv.length_squared() > 1e-30, "unwrap collapsed a triangle");
            let d2 = (du.normalize().dot(dv.normalize())).clamp(-1.0, 1.0).acos().to_degrees();
            worst = worst.max((d3 - d2).abs());
        }
    }
    assert!(worst < 1.0, "max angle distortion is {worst} deg");
}

fn corner_angle(a: DVec3, b: DVec3, c: DVec3) -> f64 {
    let (u, v) = ((a - b).normalize_or_zero(), (c - b).normalize_or_zero());
    u.dot(v).clamp(-1.0, 1.0).acos().to_degrees()
}

#[test]
fn uv_ops_are_deterministic_down_to_the_byte() {
    let spec = MeshSpec::Cylinder { radius: 32.0, height: 64.0, segments: 12, caps: true };
    let run = |op: &str, params: serde_json::Value| {
        let (mut doc, id) = doc_with(&spec);
        if op == "uv_unwrap" {
            let edge = vertical_edge_at(&mesh_of(&doc, id), 32.0, 0.0);
            apply_op(&mut doc, id, "mark", serde_json::json!({"mode": "edge", "ids": [edge]}), serde_json::json!({"kind": "seam"}));
        }
        apply_op(&mut doc, id, op, all_faces(), params);
        let m = mesh_of(&doc, id);
        editmesh::io::write(&m)
    };
    for (op, params) in [
        ("uv_box", serde_json::json!({})),
        ("uv_cylinder", serde_json::json!({"axis": "fit"})),
        ("uv_planar", serde_json::json!({"axis": "x"})),
        ("uv_unwrap", serde_json::json!({})),
        ("uv_pack", serde_json::json!({})),
        ("uv_reset", serde_json::json!({})),
    ] {
        assert_eq!(run(op, params.clone()), run(op, params), "{op} is not deterministic");
    }
}

#[test]
fn uv_ops_keep_closed_wound_euler_positions() {
    let specs = [
        MeshSpec::Box { size: [64.0, 64.0, 64.0] },
        MeshSpec::Cylinder { radius: 32.0, height: 64.0, segments: 16, caps: true },
        MeshSpec::Torus { major: 64.0, minor: 16.0, segments: 24, sides: 12 },
    ];
    let eulers = [2, 2, 0];
    let ops: Vec<(&str, serde_json::Value)> = vec![
        ("uv_box", serde_json::json!({})),
        ("uv_box", serde_json::json!({"scale": "fit"})),
        ("uv_cylinder", serde_json::json!({})),
        ("uv_cylinder", serde_json::json!({"axis": "fit", "cap": "none"})),
        ("uv_planar", serde_json::json!({})),
        ("uv_planar", serde_json::json!({"axis": "view", "view_dir": [1.0, 2.0, 3.0]})),
        ("uv_unwrap", serde_json::json!({})),
        ("uv_unwrap", serde_json::json!({"scale": "fit", "fill_holes": false})),
        ("uv_pack", serde_json::json!({})),
        ("uv_reset", serde_json::json!({})),
    ];
    for (spec, euler) in specs.iter().zip(eulers) {
        for (op, params) in &ops {
            let (mut doc, id) = doc_with(spec);
            let before = mesh_of(&doc, id);
            apply_op(&mut doc, id, op, all_faces(), params.clone());
            let after = mesh_of(&doc, id);
            assert_eq!(after.positions, before.positions, "{op} moved vertices");
            let v = editmesh::validate::invariants(&after);
            assert!(v.closed, "{op} opened the mesh");
            assert!(v.wound, "{op} broke winding");
            assert_eq!(v.euler, euler, "{op} changed euler");
            assert!(v.degenerate.is_empty(), "{op} left degenerate faces");
            assert!(after.corners.iter().all(|c| c.uv.iter().all(|u| u.is_finite())), "{op} wrote a bad uv");
        }
    }
}

#[test]
fn pack_fits_the_unit_square_without_overlap() {
    let (mut doc, id) = doc_with(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    apply_op(&mut doc, id, "uv_box", all_faces(), serde_json::json!({}));
    apply_op(&mut doc, id, "uv_pack", all_faces(), serde_json::json!({"margin": 0.02}));
    let m = mesh_of(&doc, id);
    for c in &m.corners {
        assert!(c.uv[0] >= -1e-6 && c.uv[0] <= 1.0 + 1e-6 && c.uv[1] >= -1e-6 && c.uv[1] <= 1.0 + 1e-6, "pack left the unit square");
    }
    let mut boxes: Vec<[f64; 4]> = Vec::new();
    let groups = seam_islands(&m);
    assert_eq!(groups.len(), 6, "the seamed box packs six islands");
    for g in &groups {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for f in g {
            let face = &m.faces[*f as usize];
            for k in face.first as usize..face.first as usize + face.len as usize {
                lo[0] = lo[0].min(m.corners[k].uv[0] as f64);
                lo[1] = lo[1].min(m.corners[k].uv[1] as f64);
                hi[0] = hi[0].max(m.corners[k].uv[0] as f64);
                hi[1] = hi[1].max(m.corners[k].uv[1] as f64);
            }
        }
        boxes.push([lo[0], lo[1], hi[0], hi[1]]);
    }
    for i in 0..boxes.len() {
        for j in (i + 1)..boxes.len() {
            let (a, b) = (boxes[i], boxes[j]);
            let overlap = a[0] < b[2] - 1e-9 && b[0] < a[2] - 1e-9 && a[1] < b[3] - 1e-9 && b[1] < a[3] - 1e-9;
            assert!(!overlap, "packed islands {i} and {j} overlap");
        }
    }
}

fn seam_islands(m: &EditMesh) -> Vec<Vec<u32>> {
    let adj = Adjacency::build(m);
    let mut parent: std::collections::BTreeMap<u32, u32> =
        (0..m.faces.len() as u32).map(|f| (f, f)).collect();
    fn find(parent: &mut std::collections::BTreeMap<u32, u32>, mut x: u32) -> u32 {
        while parent[&x] != x {
            x = parent[&x];
        }
        x
    }
    for e in &adj.edges {
        if m.edge_flag(e.a, e.b).contains(rdoc::editmesh::EdgeFlags::SEAM) {
            continue;
        }
        for w in e.faces.windows(2) {
            let (a, b) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
            if a != b {
                parent.insert(a.max(b), a.min(b));
            }
        }
    }
    let mut groups: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
    for f in 0..m.faces.len() as u32 {
        groups.entry(find(&mut parent, f)).or_default().push(f);
    }
    groups.into_values().collect()
}

#[test]
fn planar_projects_along_the_axis_and_leaves_the_rest() {
    let (mut doc, id) = doc_with(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    let before = mesh_of(&doc, id);
    let sel = serde_json::json!({"mode": "face", "query": {"kind": "normal", "dir": [0.0, 0.0, 1.0], "max_angle_deg": 5.0}});
    apply_op(&mut doc, id, "uv_planar", sel, serde_json::json!({"axis": "z"}));
    let m = mesh_of(&doc, id);
    for (fi, _) in m.faces.iter().enumerate() {
        for (k, c) in m.face_corners(fi).iter().enumerate() {
            let p = m.positions[c.vertex as usize];
            let b = before.face_corners(fi)[k];
            if m.face_normal(fi).z > 0.99 {
                assert_eq!(c.uv, [(p.y / 128.0) as f32, (-p.x / 128.0) as f32], "planar uv is off");
            } else {
                assert_eq!(c.uv, b.uv, "an unselected face moved");
            }
        }
    }
}

#[test]
fn reset_zeroes_uvs_and_keeps_seams() {
    let (mut doc, id) = doc_with(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    apply_op(&mut doc, id, "uv_box", all_faces(), serde_json::json!({}));
    let flags = mesh_of(&doc, id).edge_flags.clone();
    assert!(!flags.is_empty());
    apply_op(&mut doc, id, "uv_reset", all_faces(), serde_json::json!({}));
    let m = mesh_of(&doc, id);
    assert!(m.corners.iter().all(|c| c.uv == [0.0, 0.0]));
    assert_eq!(m.edge_flags, flags, "reset touched edge flags");
}

#[test]
fn world_space_follows_the_object_transform() {
    let spec = MeshSpec::Box { size: [64.0, 64.0, 64.0] };
    let shift = DVec3::new(64.0, 0.0, 0.0);
    let run = |space: &str, at: DVec3| {
        let (mut doc, id) = doc_with(&spec);
        let mut o = doc.object(id).unwrap().clone();
        o.transform.position = at;
        doc.apply(Command::ReplaceObjects { objects: vec![o] }).unwrap();
        apply_op(&mut doc, id, "uv_box", all_faces(), serde_json::json!({"space": space}));
        mesh_of(&doc, id)
    };
    let (a, b) = (run("object", DVec3::ZERO), run("world", DVec3::ZERO));
    assert_eq!(editmesh::io::write(&a), editmesh::io::write(&b), "identity world space differs");
    let moved = run("world", shift);
    let plain = run("object", shift);
    assert_eq!(moved.positions, plain.positions, "world space moved vertices");
    for (f, _) in moved.faces.iter().enumerate() {
        for (cm, co) in moved.face_corners(f).iter().zip(plain.face_corners(f)) {
            let p = moved.positions[cm.vertex as usize];
            let n = moved.face_normal(f);
            let bf = box_brush(DVec3::splat(-32.0), DVec3::splat(32.0), "m").unwrap();
            let face = bf.faces.iter().find(|x| x.normal == n).unwrap();
            let want = tile_uv(project_uv(&face.uv, n, p + shift));
            assert_eq!(cm.uv, [want[0] as f32, want[1] as f32], "world uv is off");
            let _ = co;
        }
    }
    let (mut doc, id) = doc_with(&spec);
    let req: editmesh::OpRequest = serde_json::from_value(
        serde_json::json!({"op": "uv_cylinder", "object": id.to_string(), "sel": all_faces(), "params": {"axis": "world_z"}}),
    )
    .unwrap();
    let out = editmesh::plan(&doc, &req).unwrap();
    doc.apply(out.command).unwrap();
    let via_world = mesh_of(&doc, id);
    let (mut doc, id) = doc_with(&spec);
    apply_op(&mut doc, id, "uv_cylinder", all_faces(), serde_json::json!({"axis": "object_z"}));
    assert_eq!(editmesh::io::write(&via_world), editmesh::io::write(&mesh_of(&doc, id)));
}

#[test]
fn stretch_report_flags_stretched_islands() {
    let (mut doc, id) = doc_with(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    apply_op(&mut doc, id, "uv_box", all_faces(), serde_json::json!({}));
    let boxed = mesh_of(&doc, id);
    let report = rdoc::editmesh::ops::uv::stretch_report(&boxed);
    assert_eq!(report.islands, 6, "the seamed box has six islands");
    assert_eq!(report.worst.len(), 6);
    assert!(report.worst[0].angle_deg < 1e-3, "a planar projection reports angle error");
    let want = 1.0 / (128.0 * 128.0);
    for w in &report.worst {
        assert!((w.area_ratio - want).abs() / want < 0.01, "area ratio is not world scale");
    }
    apply_op(&mut doc, id, "uv_reset", all_faces(), serde_json::json!({}));
    let reset = mesh_of(&doc, id);
    let flat = rdoc::editmesh::ops::uv::stretch_report(&reset);
    assert_eq!(flat.islands, 6);
    assert_eq!(flat.worst[0].angle_deg, 180.0, "collapsed uvs report no distortion");
    let json = serde_json::to_value(&flat).unwrap();
    assert_eq!(json["islands"], 6);
}

#[test]
fn bad_uv_requests_are_errors_not_panics() {
    let (doc, id) = doc_with(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    let req = |op: &str, params: serde_json::Value| -> editmesh::OpRequest {
        serde_json::from_value(serde_json::json!({"op": op, "object": id.to_string(), "sel": all_faces(), "params": params})).unwrap()
    };
    for (op, params) in [
        ("uv_box", serde_json::json!({"texels_per_tile": "wide"})),
        ("uv_box", serde_json::json!({"bogus": 1})),
        ("uv_cylinder", serde_json::json!({"axis": "diagonal"})),
        ("uv_cylinder", serde_json::json!({"cap": "dome"})),
        ("uv_planar", serde_json::json!({"axis": "view", "view_dir": [0.0, 0.0, 0.0]})),
        ("uv_planar", serde_json::json!({"axis": "w"})),
        ("uv_unwrap", serde_json::json!({"method": "angle_based"})),
        ("uv_reset", serde_json::json!({"all": true})),
        ("uv_pack", serde_json::json!({"margin": "wide"})),
    ] {
        assert!(editmesh::plan(&doc, &req(op, params)).is_err(), "{op} accepted a bad request");
    }
    let (mut doc, id) = doc_with(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
    apply_op(&mut doc, id, "uv_box", all_faces(), serde_json::json!({"texels_per_tile": 1e9}));
    let _ = mesh_of(&doc, id);
}
