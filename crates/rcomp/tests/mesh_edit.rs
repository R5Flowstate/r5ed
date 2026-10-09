//! M1 compile gates: a project with an `.r5mesh` compiles, validates, and
//! carries the same lightmap pages as its glb-sourced twin; a stock
//! decompiled chunk round trips through `.r5mesh` unchanged.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::*;
use rbsp::Version;

fn fixtures() -> PathBuf {
    PathBuf::from(std::env::var("R5ED_FIXTURES").unwrap_or_else(|_| "fixtures-absent".into()))
}

/// One editable box mesh plus the file it reads, on top of the small map.
fn doc_with_mesh(editable: bool) -> (rdoc::model::MapDoc, BTreeMap<String, Vec<u8>>) {
    let mut doc = small_map();
    let layer = doc.layers[0].id;
    let req: rdoc::meshgen::MeshRequest =
        serde_json::from_str(&format!("{{\"spec\":{{\"shape\":\"box\",\"size\":[128,128,128]}},\"editable\":{editable}}}"))
            .unwrap();
    let cmd = rdoc::meshgen::cmd_add_mesh(layer, None, &req).unwrap();
    let rdoc::command::Command::Batch { commands, .. } = cmd else { panic!("not a batch") };
    let mut files = BTreeMap::new();
    let mut objects = Vec::new();
    for c in commands {
        match c {
            rdoc::command::Command::SetFile { path, bytes: Some(b) } => {
                files.insert(path, b);
            }
            rdoc::command::Command::AddObjects { objects: o } => objects.extend(o),
            _ => {}
        }
    }
    assert_eq!(objects.len(), 1);
    doc.objects.extend(objects);
    (doc, files)
}

fn lightmap_pages(map: &rcomp::MapArtifacts) -> usize {
    map.bsp.lump(rbsp::ids::LIGHTMAP_HEADERS).len() / 8
}

#[test]
fn an_r5mesh_project_compiles_and_matches_its_glb_twin() {
    let (doc, files) = doc_with_mesh(true);
    let (glb_doc, glb_files) = doc_with_mesh(false);
    let opts = CompileOptions { skip_rooms: true, ..options() };
    let fs_of = |files: BTreeMap<String, Vec<u8>>| {
        struct M(BTreeMap<String, Vec<u8>>);
        impl ProjectFs for M {
            fn read(&self, rel: &str) -> anyhow::Result<Vec<u8>> {
                self.0.get(rel).cloned().ok_or_else(|| anyhow::anyhow!("no project file {rel}"))
            }
        }
        M(files)
    };
    let out = compile(&doc, &fs_of(files), &Assets::new(), &opts).expect("the r5mesh project compiles");
    let twin = compile(&glb_doc, &fs_of(glb_files), &Assets::new(), &opts).expect("the glb twin compiles");
    for (label, map) in [("client", out.client.as_ref().expect("client")), ("dedi", out.dedi.as_ref().expect("dedi"))] {
        let issues = rbsp::validate(&map.bsp, &map.ent_bytes);
        let errors: Vec<String> = issues
            .iter()
            .filter(|i| i.is_error())
            .map(|i| format!("{:?} {:?}: {}", i.severity, i.lump, i.message))
            .collect();
        assert!(errors.is_empty(), "{label} has {} errors: {}", errors.len(), errors.join(" | "));
    }
    let client = out.client.as_ref().expect("client");
    let glb_client = twin.client.as_ref().expect("glb client");
    assert_eq!(
        lightmap_pages(client),
        lightmap_pages(glb_client),
        "the editable mesh charts exactly like its glb twin"
    );
    assert!(out.report.stats.triangles > 0);
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn a_stock_decompiled_chunk_round_trips_through_r5mesh() {
    let dir = fixtures().join("client/_test_s21_in");
    let stem = "mp_rr_canyonlands_staging_mu1";
    let bsp = rbsp::Bsp::read_path(&dir.join(format!("{stem}.bsp"))).expect("the fixture map reads");
    let v = bsp.version_kind().expect("a version");
    assert_eq!(v, Version::ClientV51);
    let (doc, files) = rcomp::decompile::decompile(&bsp, None, &BTreeMap::new(), stem).expect("the map decompiles");
    let glb_path = files.keys().find(|p| p.ends_with(".glb")).expect("the decompile wrote a mesh").clone();
    let glb_bytes = &files[&glb_path];
    let prims = rdoc::import::glb::read_split(glb_bytes).expect("the chunk reads");
    assert!(!prims.is_empty());
    let total_tris: usize = prims.iter().map(|p| p.indices.len() / 3).sum();
    assert!(total_tris > 0, "the chunk has triangles");
    let mesh = rdoc::editmesh::convert::weld_primitives(&prims).expect("the chunk welds");
    let bytes = rdoc::editmesh::io::write(&mesh);
    let back = rdoc::editmesh::io::read(&bytes).expect("the chunk reads back");
    assert_eq!(back, mesh, "the chunk round trips");
    // Weld keeps one corner per soup corner, in order, with its attributes.
    let total_corners: usize = prims.iter().map(|p| p.positions.len()).sum();
    assert_eq!(back.corners.len(), total_corners, "weld kept every soup corner");
    let mut corner = 0usize;
    for p in &prims {
        for (k, pos) in p.positions.iter().enumerate() {
            let c = &back.corners[corner];
            corner += 1;
            let v = back.positions[c.vertex as usize];
            assert!((v.x - pos[0] as f64).abs() < 1e-9 && (v.y - pos[1] as f64).abs() < 1e-9 && (v.z - pos[2] as f64).abs() < 1e-9);
            assert_eq!(c.uv, p.uvs[k], "a corner uv drifted");
            assert_eq!(c.normal, Some(p.normals[k]), "an authored normal was dropped");
        }
    }
    let eval = rdoc::editmesh::evaluate(&back);
    assert_eq!(eval.indices.len() / 3, total_tris, "the triangle count survives");
    let positions: Vec<[f32; 3]> = prims.iter().flat_map(|p| p.positions.clone()).collect();
    for p in &eval.positions {
        let near = positions.iter().any(|q| {
            let (dx, dy, dz) = (q[0] - p[0], q[1] - p[1], q[2] - p[2]);
            (dx * dx + dy * dy + dz * dz).sqrt() < 1e-5
        });
        assert!(near, "a position drifted past 1e-5");
    }
    // Shading: every evaluated normal matches an authored normal at its
    // position. A chunk with no authored normals shades flat, which the
    // fixture tests above already pin down.
    if prims.iter().any(|p| p.normals.iter().any(|n| *n != [0.0, 0.0, 1.0])) {
        let mut by_pos: std::collections::HashMap<[u32; 3], Vec<[f32; 3]>> = std::collections::HashMap::new();
        for p in &prims {
            for (pos, n) in p.positions.iter().zip(&p.normals) {
                by_pos.entry([pos[0].to_bits(), pos[1].to_bits(), pos[2].to_bits()]).or_default().push(*n);
            }
        }
        for (p, n) in eval.positions.iter().zip(&eval.normals) {
            let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
            let found = by_pos.get(&key).is_some_and(|ms| {
                ms.iter().any(|m| (m[0] * n[0] + m[1] * n[1] + m[2] * n[2]) as f64 > 0.999)
            });
            assert!(found, "a shading normal drifted: {n:?} at {p:?}");
        }
    }
    let _ = (doc, v);
}
