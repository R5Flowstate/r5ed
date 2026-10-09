//! M6 compile gate: a bevelled, harden-normals box compiles on both
//! targets, passes `rbsp::validate`, and carries lightmap charts.
//!
//! The box is built as six quads: bevel's face counts (26 for segments 1)
//! assume quad topology, and flat triangulation diagonals are dissolved
//! with the M4 dissolve op before bevelling soup.

mod common;

use common::*;

/// Eight corners, six quads wound CCW from outside, like the unit fixtures.
fn quad_box_editmesh(h: f64) -> rdoc::editmesh::EditMesh {
    use rdoc::editmesh::{Corner, EditMesh, Face, FaceFlags};
    let positions = vec![
        DVec3::new(-h, -h, -h),
        DVec3::new(h, -h, -h),
        DVec3::new(h, h, -h),
        DVec3::new(-h, h, -h),
        DVec3::new(-h, -h, h),
        DVec3::new(h, -h, h),
        DVec3::new(h, h, h),
        DVec3::new(-h, h, h),
    ];
    let quads: Vec<Vec<u32>> =
        vec![vec![4, 5, 6, 7], vec![3, 2, 1, 0], vec![1, 2, 6, 5], vec![3, 0, 4, 7], vec![2, 3, 7, 6], vec![0, 1, 5, 4]];
    let mut mesh = EditMesh {
        positions,
        faces: Vec::new(),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: vec!["m".to_string()],
        smooth_angle: None,
    };
    for lp in &quads {
        let first = mesh.corners.len() as u32;
        for v in lp {
            mesh.corners.push(Corner { vertex: *v, uv: [0.0, 0.0], normal: None });
        }
        mesh.faces.push(Face { first, len: lp.len() as u32, material: 0, flags: FaceFlags::empty() });
    }
    mesh
}

fn lightmap_pages(map: &rcomp::MapArtifacts) -> usize {
    map.bsp.lump(rbsp::ids::LIGHTMAP_HEADERS).len() / 8
}

#[test]
fn a_bevelled_hardened_box_compiles_and_validates() {
    let mut doc = small_map();
    let layer = doc.layers[0].id;
    let path = "meshes/bevelbox.r5mesh".to_string();
    let boxed = quad_box_editmesh(64.0);
    let adj = rdoc::editmesh::Adjacency::build(&boxed);
    assert_eq!(adj.edges.len(), 12, "a quad box has twelve edges");
    let mut files = BTreeMap::new();
    files.insert(path.clone(), rdoc::editmesh::io::write(&boxed));
    let mut material_map = indexmap::IndexMap::new();
    material_map.insert("m".to_string(), MAT.to_string());
    doc.objects.push(new_object(
        "bevelbox",
        layer,
        ObjectKind::Mesh(rdoc::model::Mesh {
            source: path.clone(),
            material_map,
            collision: CollisionMode::Triangles,
            contents: 1,
            cast_shadows: true,
            lightmap_scale: 0.0,
            render: true,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        }),
    ));
    let mut editable = rdoc::editmesh::io::read(&files[&path]).expect("the mesh reads");
    let adj = rdoc::editmesh::Adjacency::build(&editable);
    let sel = rdoc::editmesh::select::Selection {
        mode: rdoc::editmesh::select::SelMode::Edge,
        ids: (0..adj.edges.len() as u32).collect(),
    };
    let (_, warnings) = rdoc::editmesh::ops::dispatch(
        "bevel",
        &mut editable,
        &sel,
        &serde_json::json!({"amount": 8.0, "segments": 2, "profile": 0.5, "harden_normals": true}),
    )
    .expect("the bevel runs");
    assert!(warnings.is_empty());
    assert_eq!(editable.faces.len(), 54, "6 shrunk + 24 strip + 8*3 corner quads");
    let inv = rdoc::editmesh::validate::invariants(&editable);
    assert!(inv.closed && inv.wound && inv.degenerate.is_empty(), "{inv:?}");
    files.insert(path, rdoc::editmesh::io::write(&editable));
    let opts = CompileOptions { skip_rooms: true, ..options() };
    let out = compile(&doc, &Fs(files), &Assets::new(), &opts).expect("the bevelled project compiles");
    for (label, map) in [("client", out.client.as_ref().expect("client")), ("dedi", out.dedi.as_ref().expect("dedi"))] {
        let issues = rbsp::validate(&map.bsp, &map.ent_bytes);
        let errors: Vec<String> = issues
            .iter()
            .filter(|i| i.is_error())
            .map(|i| format!("{:?} {:?}: {}", i.severity, i.lump, i.message))
            .collect();
        assert!(errors.is_empty(), "{label} has {} errors: {}", errors.len(), errors.join(" | "));
    }
    assert!(lightmap_pages(out.client.as_ref().expect("client")) > 0, "the client charts the bevelled mesh");
    assert!(out.report.stats.triangles > 0);
}
