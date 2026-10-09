//! M8 D7 gate: compiling before and after each UV op gives the same
//! lightmap page count, since charts derive from geometry, never from uv0.

mod common;

use std::collections::BTreeMap;

use common::*;
use rdoc::editmesh::select::{Sel, SelQuery};

fn doc_with_mesh() -> (rdoc::model::MapDoc, BTreeMap<String, Vec<u8>>) {
    let mut doc = small_map();
    let layer = doc.layers[0].id;
    let req: rdoc::meshgen::MeshRequest =
        serde_json::from_str("{\"spec\":{\"shape\":\"box\",\"size\":[128,128,128]},\"editable\":true}").unwrap();
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

fn mesh_path(files: &BTreeMap<String, Vec<u8>>) -> String {
    files.keys().find(|p| p.ends_with(".r5mesh")).expect("an editable mesh file").clone()
}

fn apply_uv(files: &mut BTreeMap<String, Vec<u8>>, op: &str, params: serde_json::Value) {
    let path = mesh_path(files);
    let bytes = files[&path].clone();
    let mut mesh = rdoc::editmesh::io::read(&bytes).expect("the mesh reads");
    let adj = rdoc::editmesh::Adjacency::build(&mesh);
    let sel = rdoc::editmesh::select::resolve(
        &mesh,
        &adj,
        &Sel { mode: rdoc::editmesh::select::SelMode::Face, ids: Vec::new(), query: Some(SelQuery::All) },
    )
    .expect("all faces resolve");
    rdoc::editmesh::ops::dispatch(op, &mut mesh, &sel, &params).expect("the uv op applies");
    files.insert(path, rdoc::editmesh::io::write(&mesh));
}

fn lightmap_pages(map: &rcomp::MapArtifacts) -> usize {
    map.bsp.lump(rbsp::ids::LIGHTMAP_HEADERS).len() / 8
}

fn compile_pages(doc: &rdoc::model::MapDoc, files: &BTreeMap<String, Vec<u8>>) -> (usize, usize) {
    struct M(BTreeMap<String, Vec<u8>>);
    impl ProjectFs for M {
        fn read(&self, rel: &str) -> anyhow::Result<Vec<u8>> {
            self.0.get(rel).cloned().ok_or_else(|| anyhow::anyhow!("no project file {rel}"))
        }
    }
    let opts = CompileOptions { skip_rooms: true, ..options() };
    let out = compile(doc, &M(files.clone()), &Assets::new(), &opts).expect("the project compiles");
    let client = out.client.as_ref().expect("client");
    let dedi = out.dedi.as_ref().expect("dedi");
    for (label, map) in [("client", client), ("dedi", dedi)] {
        let errors: Vec<String> = rbsp::validate(&map.bsp, &map.ent_bytes)
            .iter()
            .filter(|i| i.is_error())
            .map(|i| format!("{:?} {:?}: {}", i.severity, i.lump, i.message))
            .collect();
        assert!(errors.is_empty(), "{label} has {} errors: {}", errors.len(), errors.join(" | "));
    }
    (lightmap_pages(client), lightmap_pages(dedi))
}

#[test]
fn every_uv_op_keeps_the_lightmap_page_count() {
    let (doc, files) = doc_with_mesh();
    let baseline = compile_pages(&doc, &files);
    let ops: Vec<(&str, serde_json::Value)> = vec![
        ("uv_box", serde_json::json!({})),
        ("uv_cylinder", serde_json::json!({})),
        ("uv_planar", serde_json::json!({"axis": "x"})),
        ("uv_unwrap", serde_json::json!({})),
        ("uv_pack", serde_json::json!({})),
        ("uv_reset", serde_json::json!({})),
    ];
    for (op, params) in ops {
        let mut after = files.clone();
        apply_uv(&mut after, op, params);
        assert_eq!(compile_pages(&doc, &after), baseline, "{op} changed the lightmap page count");
    }
}
