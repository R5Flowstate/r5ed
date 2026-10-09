//! M10 compile gate: a doorway cut through a wall compiles and passes
//! `rbsp::validate`, and an object marked `role: Cutter` contributes no
//! geometry of its own.

mod common;

use std::collections::BTreeMap;

use common::*;

fn add_editable(doc: &mut MapDoc, files: &mut BTreeMap<String, Vec<u8>>, size: [f64; 3]) -> Id {
    let req: rdoc::meshgen::MeshRequest = serde_json::from_str(&format!(
        "{{\"spec\":{{\"shape\":\"box\",\"size\":[{},{},{}]}},\"editable\":true}}",
        size[0], size[1], size[2]
    ))
    .unwrap();
    let cmd = rdoc::meshgen::cmd_add_mesh(doc.layers[0].id, None, &req).unwrap();
    let rdoc::command::Command::Batch { commands, .. } = cmd else { panic!("not a batch") };
    let mut id = None;
    for c in commands {
        match c {
            rdoc::command::Command::SetFile { path, bytes: Some(b) } => {
                files.insert(path, b);
            }
            rdoc::command::Command::AddObjects { objects } => {
                for o in objects {
                    id = Some(o.id);
                    doc.objects.push(o);
                }
            }
            _ => {}
        }
    }
    id.expect("a mesh object")
}

fn source_of(doc: &MapDoc, id: Id) -> String {
    match &doc.objects.iter().find(|o| o.id == id).expect("the object").kind {
        ObjectKind::Mesh(m) => m.source.clone(),
        _ => panic!("not a mesh"),
    }
}

fn cutter_mesh() -> rdoc::editmesh::EditMesh {
    let g = rdoc::meshgen::build(
        &rdoc::meshgen::MeshSpec::Box { size: [48.0, 64.0, 96.0] },
        "cutter",
        rdoc::model::DEFAULT_MATERIAL,
        1.0 / 512.0,
    )
    .unwrap();
    let mut mesh = rdoc::editmesh::convert::weld_primitives(&[rdoc::import::glb::GlbMesh { colors: Vec::new(),
        name: g.name,
        material: g.material,
        positions: g.positions,
        normals: g.normals,
        uvs: g.uvs,
        indices: g.indices,
    }])
    .unwrap();
    for p in &mut mesh.positions {
        p.z -= 16.0;
    }
    mesh
}

fn validate_clean(map: &rcomp::MapArtifacts, label: &str) {
    let issues = rbsp::validate(&map.bsp, &map.ent_bytes);
    let errors: Vec<String> = issues
        .iter()
        .filter(|i| i.is_error())
        .map(|i| format!("{:?} {:?}: {}", i.severity, i.lump, i.message))
        .collect();
    assert!(errors.is_empty(), "{label} has {} errors: {}", errors.len(), errors.join(" | "));
}

#[test]
fn a_doorway_cut_through_a_wall_compiles_and_validates() {
    let mut doc = small_map();
    let mut files = BTreeMap::new();
    let wall_id = add_editable(&mut doc, &mut files, [256.0, 32.0, 128.0]);
    let wall_path = source_of(&doc, wall_id);
    let wall = rdoc::editmesh::io::read(&files[&wall_path]).unwrap();
    let solved =
        rdoc::editmesh::boolean::solve(&wall, &cutter_mesh(), rdoc::editmesh::BoolOp::Difference, false)
            .expect("the doorway cuts");
    let inv = rdoc::editmesh::validate::invariants(&solved);
    assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
    files.insert(wall_path, rdoc::editmesh::io::write(&solved));

    let cutter_id = add_editable(&mut doc, &mut files, [48.0, 64.0, 96.0]);
    for o in &mut doc.objects {
        if o.id == cutter_id {
            o.transform.position = DVec3::new(0.0, 0.0, -16.0);
            if let ObjectKind::Mesh(m) = &mut o.kind {
                m.role = rdoc::model::MeshRole::Cutter;
            }
        }
    }

    let opts = CompileOptions { skip_rooms: true, ..options() };
    let out = compile(&doc, &Fs(files.clone()), &Assets::new(), &opts).expect("the doorway compiles");
    validate_clean(out.client.as_ref().expect("client"), "client");
    validate_clean(out.dedi.as_ref().expect("dedi"), "dedi");
    let with_cutter = out.report.stats.triangles;
    assert!(with_cutter > 0, "the wall compiled to triangles");

    doc.objects.retain(|o| o.id != cutter_id);
    let plain = compile(&doc, &Fs(files), &Assets::new(), &opts).expect("the wall alone compiles");
    assert_eq!(
        with_cutter, plain.report.stats.triangles,
        "the cutter object contributes no geometry of its own"
    );
}

/// A wall with every edge chamfered, standing in for the M6 bevel op while
/// that phase lands elsewhere: the doorway must cut its angled strips and
/// still compile clean.
fn bevelled_wall() -> rdoc::editmesh::EditMesh {
    use rdoc::editmesh::{Corner, EditMesh, Face, FaceFlags};
    let (hx, hy, c) = (128.0, 16.0, 8.0);
    let (z0, z1) = (-64.0, 64.0);
    let ring = [
        (hx - c, -hy),
        (hx, -hy + c),
        (hx, hy - c),
        (hx - c, hy),
        (-hx + c, hy),
        (-hx, hy - c),
        (-hx, -hy + c),
        (-hx + c, -hy),
    ];
    let mut positions = Vec::with_capacity(16);
    for (_, (x, y)) in ring.iter().enumerate() {
        positions.push(DVec3::new(*x, *y, z0));
    }
    for (x, y) in &ring {
        positions.push(DVec3::new(*x, *y, z1));
    }
    let mut mesh = EditMesh {
        positions,
        faces: Vec::new(),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: vec![rdoc::model::DEFAULT_MATERIAL.to_string()],
        smooth_angle: None,
    };
    let mut push_face = |verts: &[u32], uv: &[[f32; 2]]| {
        let first = mesh.corners.len() as u32;
        for (v, u) in verts.iter().zip(uv.iter()) {
            mesh.corners.push(Corner { vertex: *v, uv: *u, normal: None });
        }
        mesh.faces.push(Face { first, len: verts.len() as u32, material: 0, flags: FaceFlags::empty() });
    };
    for k in 0..8 {
        let (a, b) = (k as u32, (k + 1) as u32 % 8);
        let (x0, _) = ring[k];
        let (x1, _) = ring[(k + 1) % 8];
        push_face(
            &[a, b, b + 8, a + 8],
            &[
                [(x0 / 512.0) as f32, (z0 / 512.0) as f32],
                [(x1 / 512.0) as f32, (z0 / 512.0) as f32],
                [(x1 / 512.0) as f32, (z1 / 512.0) as f32],
                [(x0 / 512.0) as f32, (z1 / 512.0) as f32],
            ],
        );
    }
    push_face(
        &[8, 9, 10, 11, 12, 13, 14, 15],
        &ring.map(|(x, y)| [(x / 512.0) as f32, (y / 512.0) as f32]),
    );
    push_face(
        &[7, 6, 5, 4, 3, 2, 1, 0],
        &ring.map(|(x, y)| [(x / 512.0) as f32, (y / 512.0) as f32]),
    );
    mesh
}

#[test]
fn a_doorway_cut_through_a_bevelled_wall_compiles_and_validates() {
    let wall = bevelled_wall();
    let inv = rdoc::editmesh::validate::invariants(&wall);
    assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
    let solved =
        rdoc::editmesh::boolean::solve(&wall, &cutter_mesh(), rdoc::editmesh::BoolOp::Difference, false)
            .expect("the doorway cuts the bevelled wall");
    let inv = rdoc::editmesh::validate::invariants(&solved);
    assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");

    let mut doc = small_map();
    let mut files = BTreeMap::new();
    let wall_id = add_editable(&mut doc, &mut files, [256.0, 32.0, 128.0]);
    let wall_path = source_of(&doc, wall_id);
    files.insert(wall_path, rdoc::editmesh::io::write(&solved));

    let opts = CompileOptions { skip_rooms: true, ..options() };
    let out = compile(&doc, &Fs(files), &Assets::new(), &opts).expect("the bevelled doorway compiles");
    validate_clean(out.client.as_ref().expect("client"), "client");
    validate_clean(out.dedi.as_ref().expect("dedi"), "dedi");
}
