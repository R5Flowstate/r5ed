//! M9 compile gates: a 200-count array compiles and validates, and convex
//! hulls follow loose parts instead of boxing the whole array.

mod common;

use std::collections::BTreeMap;

use common::*;
use rdoc::editmesh::Modifier;

fn doc_with_array_box(count: u32, rel: f64, merge: Option<f64>, collision: CollisionMode) -> (MapDoc, Fs) {
    let mut doc = small_map();
    let layer = doc.layers[0].id;
    let req: rdoc::meshgen::MeshRequest =
        serde_json::from_str(r#"{"spec":{"shape":"box","size":[32,32,32]},"editable":true}"#).unwrap();
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
    if let rdoc::model::ObjectKind::Mesh(m) = &mut objects[0].kind {
        let mut params = serde_json::json!({"type": "array", "count": count, "relative_offset": [rel, 0.0, 0.0]});
        if let Some(d) = merge {
            params["merge"] = serde_json::json!(d);
        }
        m.modifiers.push(serde_json::from_value::<Modifier>(params).unwrap());
        m.collision = collision;
    } else {
        panic!("not a mesh");
    }
    doc.objects.extend(objects);
    let mut fs = Fs::new();
    fs.0.extend(files);
    (doc, fs)
}

fn solids_of(doc: &MapDoc, fs: &Fs) -> usize {
    let opts = CompileOptions { skip_rooms: true, ..options() };
    compile(doc, fs, &Assets::new(), &opts).expect("the array compiles").report.stats.collision_solids
}

#[test]
fn a_map_with_a_200_count_array_compiles_and_validates() {
    let (doc, fs) = doc_with_array_box(200, 1.0, Some(0.01), CollisionMode::Triangles);
    let opts = CompileOptions { skip_rooms: true, ..options() };
    let out = compile(&doc, &fs, &Assets::new(), &opts).expect("the 200-count array compiles");
    for (label, map) in [("client", out.client.as_ref().expect("client")), ("dedi", out.dedi.as_ref().expect("dedi"))] {
        let issues = rbsp::validate(&map.bsp, &map.ent_bytes);
        let errors: Vec<String> = issues
            .iter()
            .filter(|i| i.is_error())
            .map(|i| format!("{:?} {:?}: {}", i.severity, i.lump, i.message))
            .collect();
        assert!(errors.is_empty(), "{label} has {} errors: {}", errors.len(), errors.join(" | "));
    }
    assert!(out.report.stats.triangles >= 200 * 12, "every copy renders");
}

#[test]
fn convex_hulls_follow_loose_parts() {
    let opts = CompileOptions { skip_rooms: true, ..options() };
    let bare = compile(&small_map(), &Fs::new(), &Assets::new(), &opts).expect("bare").report.stats.collision_solids;
    let (one_doc, one_fs) = doc_with_array_box(1, 1.0, None, CollisionMode::ConvexHulls);
    let (two_doc, two_fs) = doc_with_array_box(2, 2.0, None, CollisionMode::ConvexHulls);
    let one = solids_of(&one_doc, &one_fs);
    let two = solids_of(&two_doc, &two_fs);
    // The report merges both targets, so one hull per target reads as two.
    assert_eq!(one, bare + 2, "one box hulls once per target");
    assert_eq!(two, one + 2, "two disjoint copies hull as two hulls per target");
}

fn doc_with_glb(modifier: Option<serde_json::Value>) -> (MapDoc, Fs) {
    let mut doc = small_map();
    let layer = doc.layers[0].id;
    let req: rdoc::meshgen::MeshRequest =
        serde_json::from_str(r#"{"spec":{"shape":"box","size":[32,32,32]}}"#).unwrap();
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
    if let rdoc::model::ObjectKind::Mesh(m) = &mut objects[0].kind {
        if let Some(p) = modifier {
            m.modifiers.push(serde_json::from_value::<Modifier>(p).unwrap());
        }
    } else {
        panic!("not a mesh");
    }
    doc.objects.extend(objects);
    let mut fs = Fs::new();
    fs.0.extend(files);
    (doc, fs)
}

#[test]
fn a_glb_source_runs_its_modifier_stack_in_the_compiler() {
    let array = Some(serde_json::json!({"type": "array", "count": 2, "relative_offset": [2.0, 0.0, 0.0]}));
    let (doc, fs) = doc_with_glb(array);
    let opts = CompileOptions { skip_rooms: true, ..options() };
    let out = compile(&doc, &fs, &Assets::new(), &opts).expect("the stacked glb compiles");
    for (label, map) in [("client", out.client.as_ref().expect("client")), ("dedi", out.dedi.as_ref().expect("dedi"))] {
        let issues = rbsp::validate(&map.bsp, &map.ent_bytes);
        let errors: Vec<String> = issues
            .iter()
            .filter(|i| i.is_error())
            .map(|i| format!("{:?} {:?}: {}", i.severity, i.lump, i.message))
            .collect();
        assert!(errors.is_empty(), "{label} has {} errors: {}", errors.len(), errors.join(" | "));
    }
    let (plain_doc, plain_fs) = doc_with_glb(None);
    let plain = compile(&plain_doc, &plain_fs, &Assets::new(), &opts).expect("the plain glb compiles");
    assert_eq!(
        out.report.stats.triangles,
        plain.report.stats.triangles + 24,
        "the array copy renders through the compiler stack on both targets"
    );
}
