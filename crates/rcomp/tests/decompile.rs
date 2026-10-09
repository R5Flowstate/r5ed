//! The decompiler: a compiled map read back as a document, and that document
//! compiled again.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::*;
use rcomp::decompile::{self, DecompileError};
use rbsp::lumps::render as render_lump;
use rbsp::{ids, Bsp, Version};
use rdoc::model::{CollisionMode, MapDoc, ObjectKind, Partition, Settings};

/// The dedicated-server maps of the fixture set this lane reads.
const DEDI_MAPS: [&str; 5] = [
    "mp_rr_canyonlands_staging",
    "mp_rr_aqueduct",
    "mp_rr_party_crasher",
    "mp_rr_salham",
    "mp_rr_boneyard",
];
const CLIENT_DIR: &str = "client/_test_s21_in";
const CLIENT_MAP: &str = "mp_rr_canyonlands_staging_mu1";
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("r5ed-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("maps")).expect("a scratch directory");
    dir
}

/// A map of the fixture set: the `.bsp` plus the `.ent` partitions beside it.
/// `dir` is the directory the map's files sit in.
fn read_map(dir: &str, stem: &str) -> (Bsp, BTreeMap<String, Vec<u8>>) {
    let maps = fixtures().join(dir);
    let bsp = Bsp::read_path(&maps.join(format!("{stem}.bsp"))).expect("the map reads");
    let mut ents = BTreeMap::new();
    for p in Partition::FILES {
        let path = maps.join(format!("{stem}_{}.ent", p.name()));
        if path.is_file() {
            ents.insert(p.name().to_string(), std::fs::read(&path).expect("a partition file reads"));
        }
    }
    (bsp, ents)
}

/// Every entity block the map carries, the worldspawn lump and the partitions.
fn source_entities(bsp: &Bsp, ents: &BTreeMap<String, Vec<u8>>) -> usize {
    let v = bsp.version_kind().expect("the map declares a version");
    let mut total = rbsp::lumps::entities::parse(bsp.lump(ids::ENTITIES), v)
        .map(|t| t.entities.len())
        .unwrap_or(0);
    for data in ents.values() {
        total += rbsp::lumps::entities::parse(data, v).map(|t| t.entities.len()).unwrap_or(0);
    }
    total
}

/// The static prop count the `sprp` block declares.
fn source_props(bsp: &Bsp) -> usize {
    let v = bsp.version_kind().expect("the map declares a version");
    match rbsp::lumps::parse_at(ids::GAME_LUMP, v, bsp.lump(ids::GAME_LUMP), 0) {
        Ok(rbsp::lumps::Lump::GameLump(gl)) => gl.static_props().map(|b| b.prop_count()).unwrap_or(0),
        _ => 0,
    }
}

/// Triangles per material name, read out of the raw mesh lumps rather than
/// through the decoder the decompiler uses.
///
/// A dedicated-server map keeps a mesh table of its own for bookkeeping and
/// none of the vertex data, so its table names triangles the map does not
/// carry: without the bounds, the vertex pool and the index stream there is
/// nothing to count.
fn source_triangles(bsp: &Bsp) -> BTreeMap<String, usize> {
    let v = bsp.version_kind().expect("the map declares a version");
    for id in [ids::MESHES, ids::MESH_BOUNDS, ids::MESH_INDICES, ids::VERTICES] {
        if bsp.lump(id).is_empty() {
            return BTreeMap::new();
        }
    }
    let meshes = render_lump::parse_meshes(bsp.lump(ids::MESHES), v).expect("the mesh table parses");
    let sorts = render_lump::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), v).expect("the sorts parse");
    let texdata = render_lump::parse_texdata(bsp.lump(ids::TEXTURE_DATA), v).expect("the tex data parses");
    let names = render_lump::parse_surface_names(bsp.lump(ids::SURFACE_NAMES), v).expect("the names parse");
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for m in &meshes {
        let Some(sort) = sorts.get(m.material_sort as usize) else { continue };
        let Some(td) = texdata.get(sort.tex_data as usize) else { continue };
        let name = names.at_offset(td.name as usize).unwrap_or_default().to_string();
        *out.entry(name).or_insert(0) += m.triangle_count as usize;
    }
    out
}

/// Triangles per material in the document, taken from the glb files it wrote.
fn doc_triangles(doc: &MapDoc, files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for o in &doc.objects {
        let ObjectKind::Mesh(m) = &o.kind else { continue };
        let mesh = rdoc::import::glb::read(&files[&m.source])
            .unwrap_or_else(|e| panic!("{}: {e}", m.source));
        let material = m.material_map.values().next().cloned().unwrap_or_default();
        assert_eq!(mesh.indices.len() % 3, 0, "{} has whole triangles", m.source);
        *out.entry(material).or_insert(0) += mesh.indices.len() / 3;
    }
    out
}

fn doc_entities(doc: &MapDoc) -> usize {
    doc.objects
        .iter()
        .filter(|o| {
            matches!(
                o.kind,
                ObjectKind::Entity(_) | ObjectKind::Volume(_) | ObjectKind::Light(_) | ObjectKind::Path(_)
            )
        })
        .count()
}

fn doc_props(doc: &MapDoc) -> usize {
    doc.objects
        .iter()
        .filter(|o| matches!(&o.kind, ObjectKind::Prop(p) if p.static_prop))
        .count()
}

fn assert_counts_match(label: &str, bsp: &Bsp, ents: &BTreeMap<String, Vec<u8>>, stem: &str) {
    eprintln!("decompiling {label}");
    let (doc, files) = decompile::decompile(bsp, None, ents, stem).unwrap_or_else(|e| panic!("{label}: {e}"));
    assert_eq!(
        doc_entities(&doc),
        source_entities(bsp, ents),
        "{label}: every entity block became an object"
    );
    assert_eq!(doc_props(&doc), source_props(bsp), "{label}: every static prop became an object");
    assert_eq!(doc_triangles(&doc, &files), source_triangles(bsp), "{label}: per-material triangles");
    rdoc::doc::validate(&doc).unwrap_or_else(|e| panic!("{label}: the document is malformed: {e}"));
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn decompile_counts_match_fixture() {
    for stem in DEDI_MAPS {
        let (bsp, ents) = read_map(&format!("dedi/{stem}/maps"), stem);
        assert_eq!(bsp.version_kind(), Some(Version::DediV47), "{stem} is a dedicated-server map");
        assert_counts_match(stem, &bsp, &ents, stem);
        assert!(source_triangles(&bsp).is_empty(), "{stem} is a dedicated-server map and has no render lumps");
    }
    let (client, client_ents) = read_map(CLIENT_DIR, CLIENT_MAP);
    assert_eq!(client.version_kind(), Some(Version::ClientV51), "{CLIENT_MAP} is a client map");
    assert_counts_match(CLIENT_MAP, &client, &client_ents, CLIENT_MAP);
    assert!(!source_triangles(&client).is_empty(), "the client map carries render geometry");
}

// ---- the round trip ---------------------------------------------------------

/// One triangle of render geometry with matching triangle collision, a spawn
/// point, a target and a static prop: enough of a map that a compile exercises
/// every lump the decompiler reads.
fn source_doc(with_prop: bool) -> MapDoc {
    let layer = Layer { id: Uuid::new_v4(), name: "default".into(), visible: true, locked: false };
    let id = layer.id;
    let mut material_map = indexmap::IndexMap::new();
    material_map.insert("dev".to_string(), MAT.to_string());
    let mut mesh = new_object(
        "tri",
        id,
        ObjectKind::Mesh(rdoc::model::Mesh {
            source: "meshes/tri.glb".into(),
            material_map,
            collision: CollisionMode::Triangles,
            contents: 0,
            cast_shadows: true,
            lightmap_scale: 16.0,
            render: true,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        }),
    );
    mesh.transform.position = DVec3::new(128.0, 128.0, 0.0);
    let mut objects = vec![
        mesh,
        new_object(
            "spawn",
            id,
            ObjectKind::Entity(Entity {
                classname: "info_player_start".into(),
                keyvalues: Default::default(),
                partition: None,
                links: Default::default(),
            }),
        ),
        new_object(
            "target",
            id,
            ObjectKind::Entity(Entity {
                classname: "info_target".into(),
                keyvalues: Default::default(),
                partition: None,
                links: Default::default(),
            }),
        ),
    ];
    if with_prop {
        // A model path no asset table knows, so its prop bounds stay the zero
        // box the reference reader also reports.
        let mut prop = new_object(
            "crate",
            id,
            ObjectKind::Prop(Prop {
                model: "mdl/levels/test/crate.rmdl".into(),
                skin: 0,
                static_prop: true,
                solid: true,
                cast_shadows: true,
                fade_distance: 0.0,
                keyvalues: Default::default(),
            }),
        );
        prop.transform.position = DVec3::new(64.0, 64.0, 0.0);
        objects.push(prop);
    }
    MapDoc {
        format: rdoc::model::FORMAT_VERSION,
        meta: rdoc::model::MapMeta { stem: "mp_rr_source".into(), ..Default::default() },
        layers: vec![layer],
        objects,
        settings: Settings::default(),
    }
}

/// The smallest glb the reader takes: three positions and no other attribute.
fn triangle_glb() -> Vec<u8> {
    rdoc::import::glb::write(&rdoc::import::glb::GlbMesh {
        name: "tri".into(),
        material: "dev".into(),
        positions: vec![[0.0, 0.0, 0.0], [64.0, 0.0, 0.0], [0.0, 64.0, 0.0]],
        normals: Vec::new(),
        uvs: Vec::new(),
        colors: Vec::new(),
        indices: Vec::new(),
    })
    .expect("the triangle glb writes")
}

/// Compiles the source document and writes it out as a reference map.
///
/// A document with no brush geometry leaves the world model's box at the zero
/// box, which is not what a map carries: the box is put back to the geometry's
/// own bounds and the worldspawn block with it, so the map reads the way a
/// shipped one does.
fn build_source(with_prop: bool) -> (PathBuf, BTreeMap<String, Vec<u8>>) {
    let mut files = Fs::new();
    files.0.insert("meshes/tri.glb".to_string(), triangle_glb());
    let out = compile(
        &source_doc(with_prop),
        &files,
        &Assets::new(),
        &CompileOptions { targets: Targets { client: true, dedi: false }, ..Default::default() },
    )
    .expect("the source map compiles");
    let mut artifacts = out.client.expect("a client map");
    let mut models = rbsp::lumps::collision::parse_models(artifacts.bsp.lump(ids::MODELS), Version::ClientV51)
        .expect("the model table parses");
    assert_eq!(models.len(), 1, "the source map has one model");
    let (lo, hi) = geometry_bounds(&artifacts.bsp);
    assert!(
        (models[0].mins[0] - lo.x as f32).abs() > 1.0,
        "the CSG pass left the world model's box at {:?} where the geometry is {lo}..{hi}",
        models[0].mins
    );
    models[0].mins = lo.to_array();
    models[0].maxs = hi.to_array();
    artifacts.bsp.set_lump(ids::MODELS, rbsp::lumps::collision::write_models(&models, Version::ClientV51));
    with_world_bounds(&mut artifacts.bsp);

    let dir = scratch("source");
    let stem = "mp_rr_source";
    artifacts
        .bsp
        .write_path(&dir.join("maps").join(format!("{stem}.bsp")))
        .expect("the source map is written");
    for (name, data) in &artifacts.ent_bytes {
        std::fs::write(dir.join("maps").join(format!("{stem}_{name}.ent")), data).expect("a partition writes");
    }
    (dir, artifacts.ent_bytes)
}

/// The box the map's own render vertices span.
fn geometry_bounds(bsp: &Bsp) -> (Vec3, Vec3) {
    let v = bsp.version_kind().expect("the map declares a version");
    let verts = rbsp::lumps::prims::parse(bsp.lump(ids::VERTICES), v).expect("the vertex pool parses");
    assert!(!verts.is_empty(), "the map has render geometry");
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for p in verts {
        lo = lo.min(Vec3::new(p.x, p.y, p.z));
        hi = hi.max(Vec3::new(p.x, p.y, p.z));
    }
    (lo, hi)
}

/// The world model box a map's models table declares, which is the bounds the
/// compiler writes the worldspawn block from.
fn model_world(bsp: &Bsp) -> (Vec3, Vec3) {
    let models = rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), Version::ClientV51)
        .expect("the model table parses");
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for m in &models {
        if m.mins[0] > m.maxs[0] {
            continue;
        }
        lo = lo.min(Vec3::from_array(m.mins));
        hi = hi.max(Vec3::from_array(m.maxs));
    }
    assert!(lo.x <= hi.x, "the world model box is a real box");
    (lo, hi)
}

/// Rewrites the worldspawn block's bounds to the world model box, so the block
/// a recompile writes from the map's own bounds is the block the map carries.
fn with_world_bounds(bsp: &mut Bsp) {
    let (lo, hi) = model_world(bsp);
    let v = bsp.version_kind().expect("the map declares a version");
    let mut text = rbsp::lumps::entities::parse(bsp.lump(ids::ENTITIES), v).expect("the entity lump parses");
    let mut worldspawns = 0;
    for e in &mut text.entities {
        if e.classname() != Some("worldspawn") {
            continue;
        }
        worldspawns += 1;
        for (k, value) in e.pairs.iter_mut() {
            match k.as_str() {
                "world_mins" => *value = rcomp::entities::vec3_text(lo.as_dvec3()),
                "world_maxs" => *value = rcomp::entities::vec3_text(hi.as_dvec3()),
                _ => {}
            }
        }
    }
    assert_eq!(worldspawns, 1, "the source map opens with one worldspawn block");
    bsp.set_lump(ids::ENTITIES, rbsp::lumps::entities::write(&text, v));
}

#[test]
fn decompile_recompile_untouched_is_identical() {
    let (dir, ents) = build_source(false);
    let bsp = Bsp::read_path(&dir.join("maps").join("mp_rr_source.bsp")).expect("the reference map reads");
    let (doc, files) = decompile::decompile(&bsp, None, &ents, "mp_rr_source").expect("the map decompiles");
    assert!(!files.is_empty(), "the world geometry became glb files");
    assert!(doc.objects.iter().all(|o| o.hidden || matches!(o.kind, ObjectKind::Reference(_))));

    let mut project = Fs::new();
    for (path, bytes) in &files {
        project.0.insert(path.clone(), bytes.clone());
    }
    let opts = CompileOptions {
        targets: Targets { client: true, dedi: false },
        reference_root: Some(dir.clone()),
        ..Default::default()
    };
    let out = compile(&doc, &project, &Assets::new(), &opts).expect("the untouched document compiles");
    let again = out.client.as_ref().expect("a client map");
    let issues = rbsp::validate(&again.bsp, &again.ent_bytes);
    let errors: Vec<String> = issues.iter().filter(|i| i.is_error()).map(|i| i.message.clone()).collect();
    assert!(errors.is_empty(), "the recompiled map validates: {}", errors.join(" | "));

    let mut lump_ids: Vec<u16> = bsp.lumps.keys().copied().collect();
    for id in again.bsp.lumps.keys() {
        if !lump_ids.contains(id) {
            lump_ids.push(*id);
        }
    }
    lump_ids.sort_unstable();
    assert!(lump_ids.len() > 20, "the map has {} lumps to compare", lump_ids.len());
    // The reference passes through: every lump, the world model box
    // included, is the map's own bytes.
    for id in lump_ids {
        compare(again.bsp.lump(id), bsp.lump(id), &format!("lump 0x{id:02x}"));
    }
    assert!(!ents.is_empty(), "the map carries partition files");
    for (name, data) in &ents {
        let Some(back) = again.ent_bytes.get(name) else {
            panic!("the recompiled map dropped its {name} partition");
        };
        compare(back, data, name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn decompile_edit_one_prop_recompiles_valid() {
    let (dir, ents) = build_source(true);
    let bsp = Bsp::read_path(&dir.join("maps").join("mp_rr_source.bsp")).expect("the reference map reads");
    let (mut doc, files) = decompile::decompile(&bsp, None, &ents, "mp_rr_source").expect("the map decompiles");
    let mut project = Fs::new();
    for (path, bytes) in &files {
        project.0.insert(path.clone(), bytes.clone());
    }
    let opts = CompileOptions {
        targets: Targets { client: true, dedi: false },
        reference_root: Some(dir.clone()),
        ..Default::default()
    };
    let untouched = compile(&doc, &project, &Assets::new(), &opts).expect("the untouched document compiles");
    let baseline = untouched.client.as_ref().expect("a client map");

    // Promote one prop: it becomes part of the document, so it compiles.
    assert!(files.contains_key(decompile::props::PROP_TABLE_PATH), "the props ride in their table");
    let id = decompile::promote_prop(&mut doc, &bsp, 0).expect("the map has a static prop");
    let prop = doc.objects.iter_mut().find(|o| o.id == id).expect("the promoted prop is in the document");
    prop.hidden = false;
    prop.transform.position = DVec3::new(256.0, 0.0, 16.0);

    let out = compile(&doc, &project, &Assets::new(), &opts).expect("the edited document compiles");
    let after = out.client.as_ref().expect("a client map");
    let issues = rbsp::validate(&after.bsp, &after.ent_bytes);
    let errors: Vec<String> = issues.iter().filter(|i| i.is_error()).map(|i| i.message.clone()).collect();
    assert!(errors.is_empty(), "the edited map validates: {}", errors.join(" | "));

    // The promoted prop takes its own record's place: the prop block is the
    // lump that changes, and the object-reference and collision lumps may
    // follow it to its new place. Nothing else moves.
    const PROP_OWNED: [u16; 8] = [
        ids::MODELS,
        ids::BVH_NODES,
        ids::GAME_LUMP,
        ids::CELL_AABB_SUBTREE_REF_TOTALS,
        ids::CELL_AABB_NODES,
        ids::OBJ_REFERENCES,
        ids::OBJ_REFERENCE_BOUNDS,
        ids::LEVEL_INFO,
    ];
    let changed = changed_lumps(&baseline.bsp, &after.bsp);
    assert!(changed.contains(&ids::GAME_LUMP), "the prop block changed");
    assert!(changed.iter().all(|id| PROP_OWNED.contains(id)), "the prop edit moved only what a prop feeds: {changed:x?}");
    let before = prop_origins(&baseline.bsp);
    let origins = prop_origins(&after.bsp);
    assert_eq!(origins.len(), before.len(), "one copy: the promoted prop replaces its record");
    assert!(origins.contains(&[256.0, 0.0, 16.0]), "the promoted prop is at its new place: {origins:?}");
    for (name, data) in &baseline.ent_bytes {
        let Some(back) = after.ent_bytes.get(name) else { panic!("the {name} partition went away") };
        compare(back, data, name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every lump the two maps disagree on, ascending, including a lump only one of
/// them carries.
fn changed_lumps(a: &Bsp, b: &Bsp) -> Vec<u16> {
    let mut ids: Vec<u16> = a.lumps.keys().copied().collect();
    for id in b.lumps.keys() {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    ids.sort_unstable();
    ids.into_iter().filter(|id| a.lump(*id) != b.lump(*id)).collect()
}

/// Where the map's static props stand.
fn prop_origins(bsp: &Bsp) -> Vec<[f32; 3]> {
    let v = Version::ClientV51;
    let Ok(rbsp::lumps::Lump::GameLump(gl)) = rbsp::lumps::parse_at(ids::GAME_LUMP, v, bsp.lump(ids::GAME_LUMP), 0)
    else {
        return Vec::new();
    };
    let Some(block) = gl.static_props() else { return Vec::new() };
    (0..block.prop_count()).map(|i| block.props[i].origin).collect()
}

#[test]
fn a_map_that_is_not_a_map_is_refused() {
    let mut bsp = Bsp::new(51, 1, 30);
    bsp.set_lump(ids::ENTITIES, b"{\n\"classname\" \"worldspawn\"\n".to_vec());
    let err = decompile::decompile(&bsp, None, &BTreeMap::new(), "mp_rr_x");
    assert!(matches!(err, Err(DecompileError::Decode { .. })), "an unterminated block is an error");
}
