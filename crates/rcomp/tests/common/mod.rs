#![allow(dead_code, unused_imports)]
//! Shared fixtures and helpers for the compiler integration tests.

//! Compiler-level tests: the document contract, the entity schema, the vis
//! codec against a shipped map and the Titanfall 2 conversion against the
//! reference the Python converter produced.

pub use std::collections::BTreeMap;
pub use std::path::PathBuf;

pub use glam::{DVec3, Vec3};
pub use rcomp::collision::tf2::{self, SurfaceTables};
pub use rcomp::entities;
pub use rcomp::vis;
pub use rcomp::{compile, AssetInfo, CompileOptions, ProjectFs, Targets};
pub use rdoc::model::{
    Brush, BrushFace, CollisionMode, Entity, Id, Layer, Light, LightKind, MapDoc, Object, ObjectKind,
    Partition, PathKind, PathObject, Prop, Reference, ReferenceKind, Settings, Transform, UvMapping,
    Volume, VolumeKind,
};
pub use rir::{AssetTables, ModelInfo};
pub use uuid::Uuid;

pub fn fixtures() -> PathBuf {
    PathBuf::from(std::env::var("R5ED_FIXTURES").unwrap_or_else(|_| "fixtures-absent".into()))
}

/// A project file store that only knows what the test put in it.
pub struct Fs(pub BTreeMap<String, Vec<u8>>);

impl Fs {
    pub fn new() -> Self {
        Fs(BTreeMap::new())
    }
}

impl ProjectFs for Fs {
    fn read(&self, rel: &str) -> anyhow::Result<Vec<u8>> {
        self.0
            .get(rel)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no project file {rel}"))
    }
}

/// An asset table that knows one dev material and one prop model.
pub struct Assets {
    tables: AssetTables,
}

impl Assets {
    pub fn new() -> Self {
        let mut tables = AssetTables::default();
        tables.materials.insert(
            rdoc::model::DEFAULT_MATERIAL.to_string(),
            rir::MaterialInfo {
                path: rdoc::model::DEFAULT_MATERIAL.to_string(),
                guid: 1,
                shader_set: "unlit".into(),
                vertex_format: rir::VertexFormat::LitBump,
                surface_prop: "world_solid".into(),
                translucent: false,
                nodraw: false,
                textures: BTreeMap::new(),
            },
        );
        for name in ["mdl/props/one.rmdl", "mdl/props/two.rmdl", "mdl/props/three.rmdl"] {
            tables.models.insert(
                name.to_string(),
                ModelInfo {
                    path: name.to_string(),
                    guid: 7,
                    bounds_min: Vec3::splat(-24.0),
                    bounds_max: Vec3::splat(24.0),
                    has_collision: false,
                    resident_paks: Vec::new(),
                },
            );
        }
        tables.surface_props.insert("world_solid".into(), 1);
        Assets { tables }
    }

    /// Six unlit face materials of a Source skybox, what the material
    /// converter builds for it.
    pub fn add_source_sky(&mut self, sky: &str) {
        for m in rcomp::sky::face_materials(sky) {
            self.tables.materials.insert(
                m.clone(),
                rir::MaterialInfo {
                    path: m,
                    guid: 11,
                    shader_set: "unlit".into(),
                    vertex_format: rir::VertexFormat::Unlit,
                    surface_prop: "concrete".into(),
                    translucent: false,
                    nodraw: false,
                    textures: BTreeMap::new(),
                },
            );
        }
    }
}

impl AssetInfo for Assets {
    fn material(&self, path: &str) -> Option<rir::MaterialInfo> {
        self.tables.materials.get(path).cloned()
    }
    fn model(&self, path: &str) -> Option<ModelInfo> {
        self.tables.models.get(path).cloned()
    }
    fn surface_prop(&self, name: &str) -> Option<u32> {
        self.tables.surface_props.get(name).copied()
    }
}

pub const MAT: &str = rdoc::model::DEFAULT_MATERIAL;

pub fn box_faces(mins: DVec3, maxs: DVec3, material: &str) -> Vec<BrushFace> {
    [
        (DVec3::X, maxs.x),
        (-DVec3::X, -mins.x),
        (DVec3::Y, maxs.y),
        (-DVec3::Y, -mins.y),
        (DVec3::Z, maxs.z),
        (-DVec3::Z, -mins.z),
    ]
    .into_iter()
    .map(|(normal, dist)| BrushFace {
        normal,
        dist,
        material: material.to_string(),
        uv: UvMapping::default(),
        lightmap_scale: 0.0,
    })
    .collect()
}

pub fn box_brush(mins: DVec3, maxs: DVec3) -> Brush {
    Brush { faces: box_faces(mins, maxs, MAT), contents: 1, detail: false }
}

pub fn new_object(name: &str, layer: Id, kind: ObjectKind) -> Object {
    Object {
        id: Uuid::new_v4(),
        name: name.to_string(),
        layer,
        parent: None,
        transform: Transform::default(),
        hidden: false,
        locked: false,
        kind,
    }
}

/// Six brushes forming a sealed room, a spawn point, a light, a trigger volume
/// and three static props.
pub fn box_room() -> MapDoc {
    let layer = Layer { id: Uuid::new_v4(), name: "default".into(), visible: true, locked: false };
    let id = layer.id;
    let mut objects = Vec::new();
    let (lo, hi) = (DVec3::new(-256.0, -256.0, 0.0), DVec3::new(256.0, 256.0, 128.0));
    let t = 32.0;
    objects.push(new_object("floor", id, ObjectKind::Brush(box_brush(
        DVec3::new(lo.x, lo.y, lo.z - t),
        DVec3::new(hi.x, hi.y, lo.z),
    ))));
    objects.push(new_object("ceiling", id, ObjectKind::Brush(box_brush(
        DVec3::new(lo.x, lo.y, hi.z),
        DVec3::new(hi.x, hi.y, hi.z + t),
    ))));
    objects.push(new_object(
        "wall_west",
        id,
        ObjectKind::Brush(box_brush(DVec3::new(lo.x - t, lo.y - t, lo.z - t), DVec3::new(lo.x, hi.y + t, hi.z + t))),
    ));
    objects.push(new_object(
        "wall_east",
        id,
        ObjectKind::Brush(box_brush(DVec3::new(hi.x, lo.y - t, lo.z - t), DVec3::new(hi.x + t, hi.y + t, hi.z + t))),
    ));
    objects.push(new_object(
        "wall_south",
        id,
        ObjectKind::Brush(box_brush(DVec3::new(lo.x, lo.y - t, lo.z - t), DVec3::new(hi.x, lo.y, hi.z + t))),
    ));
    objects.push(new_object(
        "wall_north",
        id,
        ObjectKind::Brush(box_brush(DVec3::new(lo.x, hi.y, lo.z - t), DVec3::new(hi.x, hi.y + t, hi.z + t))),
    ));
    objects.push(new_object(
        "spawn",
        id,
        ObjectKind::Entity(Entity {
            classname: "info_player_start".into(),
            keyvalues: Default::default(),
            partition: None,
            links: Default::default(),
        }),
    ));
    objects.push(new_object(
        "sun",
        id,
        ObjectKind::Light(Light {
            kind: LightKind::Sun,
            color: [1.0, 0.95, 0.9],
            intensity: 1.0,
            range: 0.0,
            inner_angle: 0.0,
            outer_angle: 0.0,
            size: [0.0; 2],
            cast_shadows: true,
            baked: true,
            realtime: false,
            keyvalues: Default::default(),
        }),
    ));
    objects.push(new_object(
        "trigger",
        id,
        ObjectKind::Volume(Volume {
            kind: VolumeKind::Trigger,
            classname: "trigger_hurt".into(),
            brushes: vec![box_brush(DVec3::new(-64.0, -64.0, 0.0), DVec3::new(64.0, 64.0, 64.0))],
            keyvalues: Default::default(),
        }),
    ));
    for i in 0..3 {
        objects.push(new_object(
            &format!("prop{i}"),
            id,
            ObjectKind::Prop(Prop {
                model: format!("mdl/props/{}.rmdl", ["one", "two", "three"][i]),
                skin: 0,
                static_prop: true,
                solid: true,
                cast_shadows: true,
                fade_distance: 0.0,
                keyvalues: Default::default(),
            }),
        ));
    }
    MapDoc {
        format: rdoc::model::FORMAT_VERSION,
        meta: rdoc::model::MapMeta { stem: "mp_rr_box_room".into(), ..Default::default() },
        layers: vec![layer],
        objects,
        settings: Settings::default(),
    }
}

/// One brush and one point entity: the smallest map that still has geometry,
/// an entity and a model table.
pub fn small_map() -> MapDoc {
    let layer = Layer { id: Uuid::new_v4(), name: "default".into(), visible: true, locked: false };
    let id = layer.id;
    MapDoc {
        format: rdoc::model::FORMAT_VERSION,
        meta: rdoc::model::MapMeta { stem: "mp_rr_ref".into(), ..Default::default() },
        layers: vec![layer.clone()],
        objects: vec![
            new_object(
                "floor",
                id,
                ObjectKind::Brush(box_brush(DVec3::new(-128.0, -128.0, -32.0), DVec3::new(128.0, 128.0, 0.0))),
            ),
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
        ],
        settings: Settings::default(),
    }
}

pub fn options() -> CompileOptions {
    CompileOptions { targets: Targets { client: true, dedi: true }, ..Default::default() }
}

pub fn hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

pub fn compare(a: &[u8], b: &[u8], what: &str) {
    if a == b {
        return;
    }
    let n = a.len().min(b.len());
    let at = (0..n).find(|i| a[*i] != b[*i]).unwrap_or(n);
    let mut msg = format!(
        "{what}: lengths {} vs {}, first difference at {at}, hashes {:016x} vs {:016x}",
        a.len(),
        b.len(),
        hash(a),
        hash(b)
    );
    if let Some(ctx) = text_window(a, b, at) {
        msg.push_str(&format!("\n  ours: {ctx}"));
    }
    panic!("{msg}");
}

/// A bounded window of readable text around a difference, for the lumps that are
/// entity text. Anything else, or a window that is not mostly text, gives
/// nothing.
pub fn text_window(a: &[u8], b: &[u8], at: usize) -> Option<String> {
    let start = at.saturating_sub(48);
    let end = at + 48;
    let one = |d: &[u8]| -> String {
        let Some(s) = d.get(start..end.min(d.len())) else { return String::new() };
        s.iter()
            .map(|&c| match c {
                b'\n' => 'n',
                b'\r' => 'r',
                b'\t' => 't',
                0x20..=0x7e => c as char,
                _ => '.',
            })
            .collect()
    };
    let (left, right) = (one(a), one(b));
    if left.is_empty() || right.is_empty() {
        return None;
    }
    Some(format!("{left}\n  ref:  {right}"))
}

/// A value the key's own type accepts, for a class whose keys the fixtures do
/// not show a value for.
pub fn default_for(key: &str, ty: rdoc::schema::KeyType) -> String {
    use rdoc::schema::KeyType as T;
    match ty {
        T::Angles => "0 0 0".to_string(),
        T::Vec3 => "0 0 0".to_string(),
        T::Color => "255 255 255".to_string(),
        T::String | T::Model | T::Material | T::Sound | T::Target => key.to_string(),
        T::Bool | T::Flags | T::Enum => "0".to_string(),
        _ => {
            if key.starts_with("light") || key.starts_with("_light") {
                "1 1 1 0".to_string()
            } else {
                "0".to_string()
            }
        }
    }
}

pub fn tf2_tables() -> (SurfaceTables, PathBuf) {
    let out_root = PathBuf::from(std::env::var("R5ED_TF2_OUT").expect("R5ED_TF2_OUT is set"));
    let game_root = PathBuf::from(std::env::var("R5ED_GAME_ROOT").expect("R5ED_GAME_ROOT is set"));
    let surf_text = std::fs::read_to_string(game_root.join("platform/scripts/surfaceproperties.rson"))
        .expect("the surface property table is available");
    // The `.vmt` tree is the fallback the extracted material json does not
    // cover, and the json wins where both name a material.
    let mut material_surfprop = tf2::read_vmt_dir(&out_root.join("unpacked/materials"));
    let extracted = tf2::read_material_dir(&out_root.join("extract_matl/material"));
    if !extracted.is_empty() {
        material_surfprop.extend(extracted);
    } else {
        material_surfprop.extend(tf2::read_material_dir(&out_root.join("vmt_matl/material")));
    }
    assert!(!material_surfprop.is_empty(), "the material surface property table is available");
    (
        SurfaceTables { surf_ids: tf2::load_surfprop_ids(&surf_text), material_surfprop },
        out_root,
    )
}

pub fn tf2_source() -> (rbsp::Bsp, BTreeMap<String, Vec<u8>>) {
    let bsp_path = PathBuf::from(std::env::var("R5ED_TF2_BSP").expect("R5ED_TF2_BSP is set"));
    let bsp = rcomp::tf2::read_source(&bsp_path).expect("the v37 source map reads");
    assert_eq!(bsp.header.version, 37);
    let dir = bsp_path.parent().expect("the map has a directory").to_path_buf();
    let stem = bsp_path.file_stem().expect("the map has a stem").to_string_lossy().to_string();
    let mut ents = BTreeMap::new();
    for p in Partition::FILES {
        let path = dir.join(format!("{stem}_{}.ent", p.name()));
        if path.is_file() {
            ents.insert(p.name().to_string(), std::fs::read(&path).expect("a partition file reads"));
        }
    }
    (bsp, ents)
}

pub fn check_tf2_parity(target: &str, version: rbsp::Version) {
    let ref_root =
        PathBuf::from(std::env::var("R5ED_TF2_REF").expect("R5ED_TF2_REF is set")).join(target);
    if !ref_root.join("mp_rr_gauntlet.bsp").is_file() {
        panic!("the {target} reference is missing");
    }
    let (tables, _) = tf2_tables();
    let (src, ents) = tf2_source();
    let out = rcomp::tf2::convert_tf2(&src, &ents, version, &tables, "mp_rr_gauntlet")
        .unwrap_or_else(|e| panic!("the {target} conversion failed: {e}"));
    let reference = rbsp::Bsp::read_path(&ref_root.join("mp_rr_gauntlet.bsp"))
        .expect("the reference map reads");
    let render_verts = src.lump(rbsp::ids::VERTICES).len();
    let mut ids: Vec<u16> = out.artifacts.bsp.lumps.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let ours = out.artifacts.bsp.lump(id);
        let theirs = reference.lump(id);
        let what = format!("{target} lump 0x{id:02x}");
        match id {
            // The shared position pool opens with the source's own render
            // vertices; the collision writer's follow and are compared through
            // the geometry they decode to.
            rbsp::ids::VERTICES => {
                let (a, b) = (ours.len().min(render_verts), theirs.len().min(render_verts));
                compare(&ours[..a], &theirs[..b], &format!("{what} render vertices"));
            }
            rbsp::ids::MODELS => compare_models(ours, theirs, target),
            rbsp::ids::BVH_NODES | rbsp::ids::BVH_LEAF_DATA | rbsp::ids::ENTITIES => {}
            _ => compare(ours, theirs, &what),
        }
    }
    let ref_ids: Vec<u16> = reference.lumps.keys().copied().collect();
    for id in ref_ids {
        assert!(
            out.artifacts.bsp.lumps.contains_key(&id),
            "{target} is missing lump 0x{id:02x} the reference carries"
        );
    }
    check_entity_text(
        out.artifacts.bsp.lump(rbsp::ids::ENTITIES),
        reference.lump(rbsp::ids::ENTITIES),
        target,
        "worldspawn",
        version,
    );
    let dir = ref_root.clone();
    for (partition, data) in &out.artifacts.ent_bytes {
        let path = dir.join(format!("mp_rr_gauntlet_{partition}.ent"));
        assert!(path.is_file(), "the reference has no {partition} partition file");
        let theirs = std::fs::read(&path).expect("a reference partition reads");
        check_entity_text(data, &theirs, target, partition, version);
    }
}

/// The parts of a MODELS record that are not the collision writer's: the render
/// box and range, the quantisation origin and step. `bvh_leaf` is the one
/// collision field left out, because it is an offset into a leaf stream whose
/// order this crate's hull writer chooses.
fn compare_models(ours: &[u8], theirs: &[u8], target: &str) {
    if ours.len() % 64 != 0 || ours.len() != theirs.len() {
        compare(ours, theirs, &format!("{target} models lump size"));
        return;
    }
    for i in 0..ours.len() / 64 {
        let (a, b) = (&ours[i * 64..i * 64 + 64], &theirs[i * 64..i * 64 + 64]);
        let what = format!("{target} model {i}");
        compare(&a[..32], &b[..32], &format!("{what} render fields"));
        compare(&a[48..], &b[48..], &format!("{what} quantisation"));
        let at = |r: &[u8], o: usize| i32::from_le_bytes(r[o..o + 4].try_into().unwrap());
        assert_eq!(at(a, 32), at(b, 32), "{what} first bvh node");
        assert_eq!(at(a, 40), at(b, 40), "{what} first collision vertex");
        assert_eq!(at(a, 44), at(b, 44), "{what} flags");
    }
}

/// A hull's corners as one sorted list, so the order the two hull writers give
/// them does not matter.
fn corner_list(verts: &[[f32; 3]]) -> Vec<[i64; 3]> {
    let mut out: Vec<[i64; 3]> = verts
        .iter()
        .map(|v| [(v[0] * 64.0).round() as i64, (v[1] * 64.0).round() as i64, (v[2] * 64.0).round() as i64])
        .collect();
    out.sort_unstable();
    out
}

/// One entity file: every block's text compared byte for byte, and every
/// `*coll` payload compared on the model it describes rather than on its bytes.
///
/// A collider blob is the collision writer's. Its tables -- the contents mask,
/// the surface property rows, the material name -- and its corner list are fixed
/// by the brush the entity carries, and those are compared exactly, as is the
/// size of its node lump. Inside a hull leaf the face order and the diagonal
/// each face is cut along follow the convex hull this crate triangulates, which
/// is not the hull library the reference converter triangulates with, so the
/// leaf words cannot be byte-identical: two hulls of one box cut a quad face
/// across different diagonals and describe the same surface.
fn check_entity_text(
    ours: &[u8],
    theirs: &[u8],
    target: &str,
    partition: &str,
    version: rbsp::Version,
) {
    let coll_target = if version == rbsp::Version::ClientV51 {
        rcomp::collision::CollTarget::Client
    } else {
        rcomp::collision::CollTarget::Dedi
    };
    let (a, b) = (blocks_of(ours), blocks_of(theirs));
    assert_eq!(a.len(), b.len(), "{target} {partition}: entity block count");
    for (i, (mine, other)) in a.iter().zip(b.iter()).enumerate() {
        let what = format!("{target} {partition} block {i}");
        compare(&strip_coll(mine), &strip_coll(other), &what);
        match (coll_model(mine, coll_target), coll_model(other, coll_target)) {
            (None, None) => {}
            (Some(x), Some(y)) => {
                assert_eq!(x.contents, y.contents, "{what}: contents mask");
                assert_eq!(x.surfprops, y.surfprops, "{what}: surface property rows");
                assert_eq!(x.names, y.names, "{what}: material name");
                assert_eq!(corner_list(&x.verts), corner_list(&y.verts), "{what}: corners");
                assert_eq!(x.nodes.len(), y.nodes.len(), "{what}: bvh node lump size");
                assert!(!x.triangles().is_empty(), "{what}: its collider decodes to no triangles");
            }
            _ => panic!("{what}: only one of the two carries a collider blob"),
        }    }
}

fn blocks_of(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(open) = data[i..].iter().position(|&b| b == b'{') {
        let open = i + open;
        let Some(close) = data[open..].iter().position(|&b| b == b'}') else { break };
        out.push(data[open..open + close + 1].to_vec());
        i = open + close + 1;
    }
    out
}

fn strip_coll(block: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(block.len());
    for line in block.split_inclusive(|&b| b == b'\n') {
        if String::from_utf8_lossy(line).trim_start().starts_with("\"*coll") {
            continue;
        }
        out.extend_from_slice(line);
    }
    out
}

fn coll_model(
    block: &[u8],
    target: rcomp::collision::CollTarget,
) -> Option<rcomp::collision::CollModel> {
    let mut blob = Vec::new();
    for i in 0.. {
        let needle = format!("\"*coll{i}\" \"");
        let Some(p) = block.windows(needle.len()).position(|w| w == needle.as_bytes()) else { break };
        let tail = &block[p + needle.len()..];
        let Some(e) = tail.iter().position(|&b| b == b'"') else { break };
        let Some(chunk) = rcomp::collision::base64_decode(&String::from_utf8_lossy(&tail[..e])).ok()
        else {
            break;
        };
        blob.extend_from_slice(&chunk);
    }
    if blob.is_empty() {
        return None;
    }
    rcomp::collision::parse_coll_blob(&blob, target).ok()
}


/// The six floats of a MODELS record.
pub fn model_record(bsp: &rbsp::Bsp, index: usize) -> [f64; 6] {
    let lump = bsp.lump(rbsp::ids::MODELS);
    let at = index * 64;
    let mut out = [0.0f64; 6];
    for (i, v) in out.iter_mut().enumerate() {
        *v = f32::from_le_bytes(lump[at + i * 4..at + i * 4 + 4].try_into().unwrap()) as f64;
    }
    out
}

/// The classname of the block the bytes before end belong to.
pub fn classname_before(body: &[u8]) -> String {
    let needle = b"\"classname\"";
    let p = body.windows(needle.len()).rposition(|w| w == needle).unwrap_or(0);
    classname(&body[p..])
}

/// The blob whose first chunk starts at start, and where the next blob
/// begins.
pub fn coll_blob_at(text: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut at = start;
    let mut i = 0usize;
    loop {
        let needle = format!("\"*coll{i}\"");
        let Some(rel) = text[at..].windows(needle.len()).position(|w| w == needle.as_bytes()) else { break };
        let key = at + rel;
        let tail = &text[key + needle.len()..];
        let q = tail.iter().position(|&b| b == b'\"').expect("a quoted value");
        let rest = &tail[q + 1..];
        let e = rest.iter().position(|&b| b == b'\"').expect("a closed value");
        out.extend_from_slice(
            &rcomp::collision::base64_decode(&String::from_utf8_lossy(&rest[..e])).expect("base64"),
        );
        at = key + needle.len() + q + 1 + e + 1;
        i += 1;
    }
    (out, at)
}

pub fn classname(body: &[u8]) -> String {
    let needle = b"\"classname\"";
    let Some(p) = body.windows(needle.len()).position(|w| w == needle) else { return String::new() };
    let tail = &body[p + needle.len()..];
    let Some(q) = tail.iter().position(|&b| b == b'"') else { return String::new() };
    let rest = &tail[q + 1..];
    let Some(e) = rest.iter().position(|&b| b == b'"') else { return String::new() };
    String::from_utf8_lossy(&rest[..e]).to_string()
}

/// The smallest mesh a document can reference, written as a glb by hand so the
/// test does not depend on any one glb writer.
pub fn cube_glb() -> Vec<u8> {
    let positions: [f32; 9] = [0.0, 0.0, 0.0, 64.0, 0.0, 0.0, 0.0, 64.0, 0.0];
    let normals: [f32; 9] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
    let uvs: [f32; 6] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
    let indices: [u16; 3] = [0, 1, 2];
    let _ = (normals, uvs);
    let mut bin: Vec<u8> = Vec::new();
    bin.extend_from_slice(bytemuck::cast_slice(&positions));
    bin.extend_from_slice(bytemuck::cast_slice(&indices));
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let text = concat!(
        r#"{"asset":{"version":"2.0"},"scene":0,"#,
        r#""scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],"#,
        r#""meshes":[{"primitives":[{"attributes":{"POSITION":0},"#,
        r#""indices":1,"material":0}]}],"#,
        r#""materials":[{"name":"dev"}],"#,
        r#""buffers":[{"byteLength":42}],"#,
        r#""bufferViews":[{"#,
        r#""buffer":0,"byteOffset":0,"byteLength":36},"#,
        r#"{"buffer":0,"byteOffset":36,"byteLength":6}],"#,
        r#""accessors":[{"#,
        r#""bufferView":0,"componentType":5126,"count":3,"type":"VEC3","#,
        r#""min":[0,0,0],"max":[64,64,0]},"#,
        r#"{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}]}"#
    );
    let mut json = text.as_bytes().to_vec();
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    let json_chunk = 8 + json.len();
    let bin_chunk = 8 + bin.len();
    let mut out = Vec::with_capacity(12 + json_chunk + bin_chunk);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&((12 + json_chunk + bin_chunk) as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

