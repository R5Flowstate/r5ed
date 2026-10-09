//! A real RSX run against the local install: list a pak, export one model out
//! of it, read the result back and convert it. Skips cleanly when no game
//! install is configured.

use std::path::PathBuf;

use rpak::glb::model_to_glb;
use rpak::index::{AssetIndex, PakStamp};
use rpak::rmdl::parse_model;
use rpak::Rsx;

/// The env var the gate sets to the real install.
const GAME_ROOT: &str = "R5ED_GAME_ROOT";

fn env_path(name: &str) -> Option<PathBuf> {
    let v = std::env::var(name).ok()?;
    if v.is_empty() {
        return None;
    }
    let p = PathBuf::from(v);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("r5ed-rpak-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
#[ignore = "needs game"]
fn real_rsx_list_and_model_export() {
    let Some(game_root) = env_path(GAME_ROOT) else {
        eprintln!("{GAME_ROOT} is not set or does not exist; skipping");
        return;
    };
    let Some(rsx_exe) = env_path("R5ED_RSX") else {
        eprintln!("R5ED_RSX is not set or does not exist; skipping");
        return;
    };
    let pak_dir = game_root.join("paks").join("Win64");
    assert!(pak_dir.is_dir(), "{} must hold the paks", pak_dir.display());
    let pak = pak_dir.join("common.rpak");
    assert!(pak.is_file(), "{} must exist", pak.display());

    let rsx = Rsx::new(rsx_exe);
    let work = scratch("list");
    let rows = rsx.list(&[pak.clone()], &work).expect("rsx list");
    assert!(rows.len() > 1000, "a real pak list has thousands of rows, got {}", rows.len());
    assert!(rows.iter().all(|r| !r.path.is_empty() && !r.pak.is_empty()));
    // The list is the index, so it must round-trip through the cache format.
    let idx = AssetIndex::build(&game_root, &[pak.clone()], rows).expect("index build");
    let idx_path = work.join("index").join("common.json");
    idx.save(&idx_path).expect("index save");
    let back = AssetIndex::load(&idx_path).expect("index load");
    assert_eq!(back.assets.len(), idx.assets.len());
    assert!(back.matches(&idx.paks));
    assert!(!back.matches(&one_other_pak()));

    // Find a model that is a prop-sized, LOD0-bearing asset.
    let prop = back
        .find_model("mdl/props/caustic_mug/caustic_mug_v")
        .cloned()
        .or_else(|| {
            back.assets
                .iter()
                .find(|r| r.path.starts_with("mdl/props/") && r.path.ends_with(".rmdl"))
                .cloned()
        })
        .expect("at least one prop model in common.rpak");
    assert!(prop.path.starts_with("mdl/"), "{}", prop.path);

    let out = scratch("model");
    rsx.export(&[pak], &["mdl_", "mdl"], &[prop.guid], &out).expect("rsx export");

    let rmdl_path = exported(&out, &prop, "rmdl");
    let vg_path = exported(&out, &prop, "vg");
    let rmdl_bytes = std::fs::read(&rmdl_path).unwrap_or_else(|e| panic!("{}: {e}", rmdl_path.display()));
    let vg_bytes = std::fs::read(&vg_path).unwrap_or_else(|e| panic!("{}: {e}", vg_path.display()));

    let model = parse_model(&rmdl_bytes, &vg_bytes)
        .unwrap_or_else(|e| panic!("parse {}: {e}", prop.path));
    let _ = model;
    let meshes = model.meshes.iter().filter(|m| !m.indices.is_empty()).count();
    assert!(meshes > 0, "{} has no LOD0 geometry", prop.path);
    for m in &model.meshes {
        assert_eq!(m.positions.len(), m.normals.len());
        assert_eq!(m.positions.len(), m.uv0.len());
        assert_eq!(m.positions.len(), m.colors.len());
        assert!(m.indices.len() % 3 == 0);
        assert!(m.indices.iter().all(|i| (*i as usize) < m.positions.len()));
        for p in &m.positions {
            assert!(p.iter().all(|x| x.is_finite()), "non-finite position in {}", prop.path);
        }
    }
    // The studiohdr bounds must bound the geometry.
    for m in &model.meshes {
        for p in &m.positions {
            for k in 0..3 {
                assert!(p[k] >= model.bounds_min[k] - 0.01, "below hull_min on axis {k}");
                assert!(p[k] <= model.bounds_max[k] + 0.01, "above hull_max on axis {k}");
            }
        }
    }
    let glb = model_to_glb(&model).expect("model to glb");
    assert_eq!(&glb[0..4], b"glTF");
    assert!(glb.len() > 128);

    // A mesh chunk written from the same geometry is a glb too, with one
    // accessor per attribute and every index inside its own primitive.
    let mut chunk = rpak::MeshChunk::new("real");
    for m in model.meshes.iter().filter(|m| !m.indices.is_empty()).take(4) {
        chunk.push(rpak::ChunkPrimitive {
            name: format!("slot_{}", m.material),
            positions: m.positions.clone(),
            normals: m.normals.clone(),
            uvs: m.uv0.clone(),
            indices: m.indices.iter().map(|i| *i as u32).collect(),
        });
    }
    assert!(chunk.triangles() > 0);
    let (lo, hi) = rpak::chunk_bounds(&chunk);
    assert!(lo.iter().all(|x| x.is_finite()) && hi.iter().all(|x| x.is_finite()));
    let chunk_glb = rpak::chunk_to_glb(&chunk).expect("chunk to glb");
    let read = rpak::glb::read_glb(&chunk_glb).expect("read chunk");
    assert_eq!(read.json["meshes"][0]["primitives"].as_array().expect("prims").len(), chunk.primitives.len());
    for p in &chunk.primitives {
        assert!(p.indices.iter().all(|i| (*i as usize) < p.positions.len()));
    }
    assert!(rpak::chunk_to_glb(&rpak::MeshChunk::new("empty")).is_err(), "a chunk with no geometry");

    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_dir_all(&out);
}

/// The CLI lays an export out as `<out>/<type>/<file name without ext>/`, with
/// the type's trailing underscore dropped.
fn exported(out: &std::path::Path, row: &rpak::AssetRow, ext: &str) -> PathBuf {
    let stem = row.file_name().strip_suffix(".rmdl").unwrap_or_else(|| row.file_name());
    let dir = out.join(row.kind.trim_end_matches('_')).join(stem);
    let path = dir.join(format!("{stem}.{ext}"));
    assert!(path.is_file(), "{} was not written", path.display());
    path
}

fn one_other_pak() -> std::collections::BTreeMap<String, PakStamp> {
    let mut m = std::collections::BTreeMap::new();
    m.insert("other.rpak".to_string(), PakStamp { size: 1, mtime_ms: 1 });
    m
}
