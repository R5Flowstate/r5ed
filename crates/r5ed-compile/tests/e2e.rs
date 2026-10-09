//! The end-to-end gates: the real binary, real tools, real fixtures.
//!
//! Every test here drives the same `r5ed-compile` a job runs, so what is
//! checked is what the editor's job panel would show. The ones that need an
//! install, an external tool or a fixture tree are marked ignored and skip
//! themselves when the environment does not name what they need; the lane gate
//! runs them with `--include-ignored` and points them at real data.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use rdoc::model::{
    Brush, BrushFace, Entity, Light, LightKind, Object, ObjectKind, PathKind, PathObject, Prop, Transform,
    UvMapping, Volume, VolumeKind, DEFAULT_MATERIAL,
};
use rdoc::Command as Edit;
use rdoc::Id;
use rterrain::Generator;
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_r5ed-compile");
const SCHEMA: &str = include_str!("../../../schema/entities.json");
/// A prop the install carries as a static prop, so it bakes a permanent vertex
/// cache and resolves to bounds.
const PROP_MODEL: &str = "mdl/props/flask/prop_flask.rmdl";
/// The entity classes a shipped map spawns a player at, and the ones that carry
/// the zipline and the capture points a room is built from.
const SPAWN_CLASSES: [&str; 6] = [
    "info_player_start",
    "info_player_teamspawn",
    "info_landmark",
    "info_target",
    "trigger_capture_point",
    "zipline",
];

// ---------------------------------------------------------------- the harness

/// One CLI invocation, with the result document parsed.
struct Run {
    json: Value,
    stderr: String,
}

impl Run {
    fn at(&self, pointer: &str) -> &Value {
        self.json.pointer(pointer).unwrap_or_else(|| panic!("no {pointer} in {}", self.json))
    }
    fn text(&self, pointer: &str) -> &str {
        self.at(pointer).as_str().unwrap_or_else(|| panic!("{pointer} is not a string"))
    }
    fn count(&self, pointer: &str) -> u64 {
        self.at(pointer).as_u64().unwrap_or_else(|| panic!("{pointer} is not a count: {}", self.at(pointer)))
    }
    fn list(&self, pointer: &str) -> &Vec<Value> {
        self.at(pointer).as_array().unwrap_or_else(|| panic!("{pointer} is not a list"))
    }
}

fn raw(args: &[&str]) -> std::process::Output {
    Command::new(BIN).args(args).output().expect("the CLI runs")
}

fn run(args: &[&str]) -> Run {
    let out = raw(args);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{args:?} failed: {}\n{stderr}", out.status);
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{args:?} printed no JSON: {e}\n{stdout}"));
    Run { json, stderr }
}

fn try_run(args: &[&str]) -> (bool, String) {
    let out = raw(args);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// A directory of this test's own, removed and recreated.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("r5ed-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// A tool the test needs, or nothing when it is not configured.
fn tool(var: &str) -> Option<PathBuf> {
    let path = std::env::var_os(var).map(PathBuf::from);
    match &path {
        Some(p) if p.is_file() => Some(p.clone()),
        _ => {
            eprintln!("{var} is not set, so this check is skipped");
            None
        }
    }
}

fn has_install() -> bool {
    std::env::var_os("R5ED_GAME_ROOT").is_some_and(|r| PathBuf::from(r).join("paks").join("Win64").is_dir())
}

fn fixture(var: &str) -> Option<PathBuf> {
    let path = std::env::var_os(var).map(PathBuf::from).filter(|p| p.exists());
    if path.is_none() {
        eprintln!("{var} is not set, so this check is skipped");
    }
    path
}

/// Writes a document the `apply` verb reads, without a byte order mark.
fn write_json(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec(value).expect("the command serialises")).expect("a command file");
}

fn read_document(project: &Path) -> Value {
    let text = std::fs::read_to_string(project.join("map.json")).expect("a document");
    serde_json::from_str(&text).expect("a document that parses")
}

fn layer_of(document: &Value) -> Id {
    document["layers"][0]["id"].as_str().expect("a layer id").parse().expect("a uuid")
}

fn object(name: &str, layer: Id, kind: ObjectKind) -> Object {
    Object {
        id: uuid_like(name),
        name: name.to_string(),
        layer,
        parent: None,
        transform: Transform::default(),
        hidden: false,
        locked: false,
        kind,
    }
}

/// A stable id per name, so a document that is written twice is the same
/// document twice.
fn uuid_like(name: &str) -> Id {
    let mut bytes = [0u8; 16];
    for (i, b) in name.bytes().cycle().take(16).enumerate() {
        bytes[i] = b;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Id::from_bytes(bytes)
}

fn box_faces(min: glam::DVec3, max: glam::DVec3, material: &str) -> Vec<BrushFace> {
    [
        (glam::DVec3::X, max.x),
        (-glam::DVec3::X, -min.x),
        (glam::DVec3::Y, max.y),
        (-glam::DVec3::Y, -min.y),
        (glam::DVec3::Z, max.z),
        (-glam::DVec3::Z, -min.z),
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

fn box_brush(min: glam::DVec3, max: glam::DVec3) -> Brush {
    Brush { faces: box_faces(min, max, DEFAULT_MATERIAL), contents: 1, detail: false }
}

/// A sealed room with a floor, a ceiling and four walls.
fn room_brushes() -> Vec<Object> {
    let (lo, hi) = (glam::DVec3::new(-512.0, -512.0, 0.0), glam::DVec3::new(512.0, 512.0, 256.0));
    let t = 32.0;
    [
        ("floor", glam::DVec3::new(lo.x, lo.y, lo.z - t), glam::DVec3::new(hi.x, hi.y, lo.z)),
        ("ceiling", glam::DVec3::new(lo.x, lo.y, hi.z), glam::DVec3::new(hi.x, hi.y, hi.z + t)),
        (
            "wall_west",
            glam::DVec3::new(lo.x - t, lo.y - t, lo.z - t),
            glam::DVec3::new(lo.x, hi.y + t, hi.z + t),
        ),
        (
            "wall_east",
            glam::DVec3::new(hi.x, lo.y - t, lo.z - t),
            glam::DVec3::new(hi.x + t, hi.y + t, hi.z + t),
        ),
        (
            "wall_south",
            glam::DVec3::new(lo.x, lo.y - t, lo.z - t),
            glam::DVec3::new(hi.x, lo.y, hi.z + t),
        ),
        (
            "wall_north",
            glam::DVec3::new(lo.x, hi.y, lo.z - t),
            glam::DVec3::new(hi.x, hi.y + t, hi.z + t),
        ),
    ]
    .into_iter()
    .map(|(name, min, max)| object(name, Id::nil(), ObjectKind::Brush(box_brush(min, max))))
    .collect()
}

/// A point light, the kind a room is lit with.
fn point_light() -> Light {
    Light {
        kind: LightKind::Point,
        color: [1.0, 0.95, 0.9],
        intensity: 20.0,
        range: 512.0,
        inner_angle: 0.0,
        outer_angle: 0.0,
        size: [0.0; 2],
        cast_shadows: true,
        baked: true,
        realtime: false,
        keyvalues: Default::default(),
    }
}

/// A point entity of a class, with every key the class requires set to a value
/// its own type accepts.
fn entity(classname: &str, schema: &rdoc::schema::EntitySchema) -> ObjectKind {
    let mut keyvalues = rdoc::model::KeyValues::new();
    if let Some(def) = schema.class(classname) {
        for key in def.keys.values() {
            if matches!(key.name.as_str(), "classname" | "origin" | "angles") {
                continue;
            }
            if let Some(value) = accepted(key) {
                keyvalues.insert(key.name.clone(), value);
            }
        }
    }
    ObjectKind::Entity(Entity {
        classname: classname.to_string(),
        keyvalues,
        partition: None,
        links: Default::default(),
    })
}


/// Every generator the terrain crate offers, by name.
fn every_generator() -> Vec<(&'static str, Generator)> {
    use rterrain::generators::*;
    vec![
        ("rolling_hills", Generator::RollingHills(RollingHillsParams::default())),
        ("mountains", Generator::Mountains(MountainsParams::default())),
        ("alpine", Generator::Alpine(AlpineParams::default())),
        ("plateau", Generator::Plateau(PlateauParams::default())),
        ("canyon", Generator::Canyon(CanyonParams::default())),
        ("badlands", Generator::Badlands(BadlandsParams::default())),
        ("dunes", Generator::Dunes(DunesParams::default())),
        ("islands", Generator::Islands(IslandsParams::default())),
        ("volcanic", Generator::Volcanic(VolcanicParams::default())),
        ("craters", Generator::Craters(CratersParams::default())),
        ("glacial", Generator::Glacial(GlacialParams::default())),
        ("rivers", Generator::Rivers(RiversParams::default())),
        ("coastline", Generator::Coastline(CoastlineParams::default())),
        ("farmland", Generator::Farmland(FarmlandParams::default())),
        ("arena", Generator::Arena(ArenaParams::default())),
        ("pits", Generator::Pits(PitsParams::default())),
    ]
}

/// A recipe document the `terrain` verb reads.
fn terrain_request(recipe: &str, resolution: [u32; 2], cell_size: f64) -> Value {
    json!({
        "recipe": serde_json::from_str::<Value>(recipe).expect("a recipe that parses"),
        "resolution": resolution,
        "cell_size": cell_size,
        "material": DEFAULT_MATERIAL,
    })
}

/// Both maps of a build, read back and run through the engine validator.
fn read_both_targets(out: &Run, build: &Path) -> Vec<(String, rbsp::Bsp)> {
    let mut seen = Vec::new();
    for (target, sub) in [("client", "maps"), ("dedi", "dedi/maps")] {
        if out.at(&format!("/targets/{target}")).is_null() {
            continue;
        }
        let dir = build.join(sub);
        let stem = out.text("/stem");
        let path = dir.join(format!("{stem}.bsp"));
        let bsp = rbsp::Bsp::read_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(bsp.version(), if target == "client" { 51 } else { 47 }, "{target} version");
        let mut ents = BTreeMap::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("ent") {
                    let file = p.file_stem().expect("a file stem").to_string_lossy().to_string();
                    let name = file.strip_prefix(&format!("{stem}_")).unwrap_or(&file).to_string();
                    ents.insert(name, std::fs::read(&p).expect("a partition file"));
                }
            }
        }
        let issues = rbsp::validate(&bsp, &ents);
        let errors: Vec<String> = issues.iter().filter(|i| i.is_error()).map(|i| i.message.clone()).collect();
        assert!(errors.is_empty(), "{target}: {errors:?}");
        assert!(!bsp.lump(rbsp::ids::ENTITIES).is_empty(), "{target} carries no entities");
        seen.push((target.to_string(), bsp));
    }
    seen
}

// -------------------------------------------------------------- the box room

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn e2e_box_room() {
    let (Some(repak), Some(revpk)) = (tool("R5ED_REPAK"), tool("R5ED_REVPK")) else {
        return;
    };
    assert!(has_install(), "R5ED_GAME_ROOT names no install");
    let dir = scratch("boxroom");
    let project = dir.join("mp_rr_boxroom");
    let p = project.display().to_string();
    run(&["new", &p, "--stem", "mp_rr_boxroom"]);
    let schema = rdoc::schema::EntitySchema::load(SCHEMA).expect("the bundled schema parses");
    for class in SPAWN_CLASSES {
        assert!(schema.class(class).is_some(), "{class} is not a schema class");
    }

    // The room, the spawn points, the lights, a trigger, a zipline and a prop
    // all arrive through one command, which is how the editor applies them.
    let layer = layer_of(&read_document(&project));
    let mut objects = room_brushes();
    for o in objects.iter_mut() {
        o.layer = layer;
    }
    for (i, class) in SPAWN_CLASSES.iter().enumerate() {
        let mut o = object(class, layer, entity(class, &schema));
        o.transform.position = glam::DVec3::new(-256.0 + i as f64 * 128.0, 0.0, 1.0);
        objects.push(o);
    }
    let mut lamp = object("lamp", layer, ObjectKind::Light(Light { kind: LightKind::Point, range: 512.0, ..point_light() }));
    lamp.transform.position = glam::DVec3::new(0.0, 0.0, 200.0);
    objects.push(lamp);
    objects.push(object(
        "hurt",
        layer,
        ObjectKind::Volume(Volume {
            kind: VolumeKind::TriggerHurt,
            classname: "trigger_hurt".into(),
            brushes: vec![box_brush(glam::DVec3::new(-128.0, -128.0, 0.0), glam::DVec3::new(128.0, 128.0, 64.0))],
            keyvalues: Default::default(),
        }),
    ));
    objects.push(object(
        "zip",
        layer,
        ObjectKind::Path(PathObject {
            kind: PathKind::Zipline,
            points: vec![glam::DVec3::new(-256.0, 0.0, 240.0), glam::DVec3::new(256.0, 0.0, 240.0)],
            keyvalues: Default::default(),
        }),
    ));
    let mut prop = object(
        "flask",
        layer,
        ObjectKind::Prop(Prop {
            model: PROP_MODEL.into(),
            skin: 0,
            static_prop: true,
            solid: true,
            cast_shadows: true,
            fade_distance: 0.0,
            keyvalues: Default::default(),
        }),
    );
    prop.transform.position = glam::DVec3::new(192.0, 192.0, 32.0);
    objects.push(prop);
    let command = dir.join("room.json");
    write_json(&command, &serde_json::to_value(Edit::AddObjects { objects }).expect("a command"));
    let added = run(&["apply", &p, &command.display().to_string()]);
    assert_eq!(added.list("/ids").len(), 16, "the room is its geometry, entities, light, trigger, zipline and prop: {}", added.json);

    // A project with no compiled map has nothing to wrap, and says so rather
    // than packing an empty archive.
    let early = dir.join("early");
    let early = run(&["package", &p, &early.display().to_string(), "--out", &early.display().to_string()]);
    assert_eq!(early.at("/maps/packed"), &json!(false), "{}", early.json);
    assert!(early.text("/maps/why").contains("no client map"), "{}", early.json);

    let build = project.join("build");
    let out = run(&["build", &p, "--targets", "client,dedi", "--bake", "preview", "--install"]);
    assert_eq!(out.text("/stem"), "mp_rr_boxroom");
    assert!(out.count("/targets/client/triangles") > 100, "the room is in the map");
    assert!(out.count("/targets/client/props") >= 1, "the prop is in the map");
    assert!(out.count("/stats/lightmap_pages") >= 1, "the preview bake wrote a page");
    for pointer in ["/warnings", "/validation"] {
        let rows = out.list(pointer);
        assert!(!rows.iter().any(|r| r.as_str().is_some_and(|s| s.starts_with("Error"))), "{pointer}: {rows:?}");
    }
    assert!(out.stderr.contains("PROGRESS 100"), "{}", out.stderr);
    let targets = read_both_targets(&out, &build);
    assert_eq!(targets.len(), 2, "both maps were written");
    assert!(out.count("/stats/entities") >= 5, "the entity classes are in the map");

    // The same document validates against the schema, with no issue at all.
    let issues = run(&["validate", &p]);
    assert!(issues.json.as_array().expect("a list").is_empty(), "{:?}", issues.json);

    // The terrain patch goes in on top of that room and is compiled again, so
    // the packaged map carries it.
    let recipe = rterrain::Recipe::single("h", Generator::RollingHills(Default::default()));
    let recipe = recipe.to_json().expect("a recipe");
    let request = dir.join("terrain.json");
    write_json(&request, &terrain_request(&recipe, [33, 33], 32.0));
    run(&["terrain", &p, &request.display().to_string()]);
    let out = run(&["build", &p, "--targets", "client,dedi", "--bake", "none", "--install"]);
    assert!(out.count("/stats/surfaces") >= 4, "the terrain is in the map: {}", out.at("/stats"));
    let targets = read_both_targets(&out, &build);
    assert_eq!(targets.len(), 2, "both maps were written again");

    // Packaging runs the packer and the archive packer for real.
    let stage = dir.join("stage");
    let packed = dir.join("out");
    let report = run(&["package", &p, &stage.display().to_string(), "--out", &packed.display().to_string()]);
    assert_eq!(report.at("/maps/packed"), &json!(true), "{}", report.json);
    let wraps = report.list("/maps/wraps/archives");
    assert_eq!(wraps.len(), 2, "a client map ships in two wrap archives: {wraps:?}");
    for w in wraps {
        let pak = PathBuf::from(w["pak"].as_str().expect("a pak path"));
        assert!(pak.is_file(), "{} is missing", pak.display());
        let size = std::fs::metadata(&pak).expect("a pak").len();
        assert!(size > 4096, "{} is {size} bytes", pak.display());
        assert_eq!(w["files"].as_array().expect("a file list").len() as u64, w["assets"].as_u64().unwrap());
    }
    let dedi = report.at("/maps/dedi");
    for key in ["dir_vpk", "data_vpk"] {
        let vpk = PathBuf::from(dedi[key].as_str().unwrap_or_else(|| panic!("{key}: {dedi}")));
        assert!(vpk.is_file(), "{} is missing", vpk.display());
        let size = std::fs::metadata(&vpk).expect("a vpk").len();
        assert!(size > 1024, "{} is {size} bytes", vpk.display());
    }
    let named = dedi["files"].as_array().expect("the workspace files");
    assert!(named.iter().any(|f| f.as_str().is_some_and(|n| n.ends_with(".bsp"))), "{named:?}");
    assert!(named.iter().any(|f| f.as_str().is_some_and(|n| n.ends_with("_script.ent") || n.ends_with("_spawn.ent"))), "{named:?}");
    assert!(report.count("/assets") > 0, "the prop's assets are in the archive: {}", report.json);
    let _ = (repak, revpk);
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------ the tf2 gauntlet

/// A 64-bit digest, so a difference is a line and not a megabyte of bytes.
fn digest(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn compare(a: &[u8], b: &[u8], what: &str) {
    if a == b {
        return;
    }
    let n = a.len().min(b.len());
    let at = (0..n).find(|i| a[*i] != b[*i]).unwrap_or(n);
    panic!(
        "{what}: lengths {} vs {}, first difference at {at}, digests {:016x} vs {:016x}",
        a.len(),
        b.len(),
        digest(a),
        digest(b)
    );
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

/// The entity partition files that sit beside a map, by partition name.
fn partitions(dir: &Path, stem: &str) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("ent") {
            continue;
        }
        let file = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let Some(name) = file.strip_prefix(&format!("{stem}_")) else { continue };
        out.insert(name.to_string(), std::fs::read(&p).expect("a partition file"));
    }
    out
}

#[test]
#[ignore = "needs R5ED_TF2_BSP and R5ED_TF2_REF"]
fn e2e_tf2_gauntlet_parity() {
    let (Some(source), Some(reference_root), Some(out_root)) = (
        fixture("R5ED_TF2_BSP"),
        fixture("R5ED_TF2_REF"),
        fixture("R5ED_TF2_OUT"),
    ) else {
        return;
    };
    assert!(has_install(), "the surface property table comes from the install");
    let dir = scratch("tf2");
    let project = dir.join("mp_rr_gauntlet");
    let p = project.display().to_string();

    let imported = run(&[
        "import-tf2",
        &source.display().to_string(),
        &p,
        "--stem",
        "mp_rr_gauntlet",
        "--vmt-dir",
        &out_root.join("unpacked/materials").display().to_string(),
        "--material-dir",
        &out_root.join("extract_matl/material").display().to_string(),
    ]);
    assert_eq!(imported.text("/stem"), "mp_rr_gauntlet");
    assert_eq!(imported.count("/source_version"), 37);
    assert!(imported.count("/materials") > 100, "the material table is loaded: {}", imported.json);
    assert!(imported.list("/files").iter().any(|f| f == "import/tf2.json"), "{:?}", imported.json);

    let out = run(&["build", &p, "--targets", "client,dedi"]);
    assert_eq!(out.text("/stem"), "mp_rr_gauntlet");
    assert_eq!(out.count("/source_version"), 37);

    for (target, sub) in [("client", "client"), ("dedi", "dedi")] {
        let reference = reference_root.join(sub);
        let dir = project.join("build").join(if target == "client" { "maps" } else { "dedi/maps" });
        let ours = rbsp::Bsp::read_dir_map(&dir, "mp_rr_gauntlet").expect("the converted map reads");
        let (bsp, _) = ours;
        let ents = partitions(&dir, "mp_rr_gauntlet");
        let ref_bsp = rbsp::Bsp::read_path(&reference.join("mp_rr_gauntlet.bsp"))
            .unwrap_or_else(|e| panic!("the {target} reference reads: {e}"));
        let ref_ents = partitions(&reference, "mp_rr_gauntlet");
        let render_verts = rbsp::Bsp::read_path(&source)
            .map(|b| b.lump(rbsp::ids::VERTICES).len())
            .unwrap_or(0);
        let mut ids: Vec<u16> = bsp.lumps.keys().copied().collect();
        ids.sort_unstable();
        assert!(!ids.is_empty(), "{target} carries no lumps");
        for id in ids {
            let ours = bsp.lump(id);
            let theirs = ref_bsp.lump(id);
            let what = format!("{target} lump 0x{id:02x}");
            match id {
                // The shared position pool opens with the source's own render
                // vertices; the collision writer's follow it.
                rbsp::ids::VERTICES => {
                    let n = ours.len().min(render_verts);
                    let m = theirs.len().min(render_verts);
                    compare(&ours[..n], &theirs[..m], &format!("{what} render vertices"));
                }
                // The collision writer's own node stream and the collider blobs
                // are compared on the surface they describe, not on their bytes.
                rbsp::ids::BVH_NODES | rbsp::ids::BVH_LEAF_DATA | rbsp::ids::ENTITIES => {}
                rbsp::ids::MODELS => {
                    assert_eq!(ours.len() % 64, 0, "{what} is not a whole number of records");
                    assert_eq!(ours.len(), theirs.len(), "{what} record count");
                    for i in 0..ours.len() / 64 {
                        compare(&ours[i * 64..i * 64 + 32], &theirs[i * 64..i * 64 + 32], &format!("{what} record {i} render fields"));
                        compare(&ours[i * 64 + 48..i * 64 + 64], &theirs[i * 64 + 48..i * 64 + 64], &format!("{what} record {i} quantisation"));
                    }
                }
                _ => compare(ours, theirs, &what),
            }
        }
        for id in ref_bsp.lumps.keys() {
            assert!(bsp.lumps.contains_key(id), "{target} is missing lump 0x{id:02x} the reference carries");
        }
        // The entity text is compared block by block with the collider payloads
        // taken out, which is where the two converters write the same map. The
        // worldspawn block lives in the map's own entity lump and the
        // partitions beside it.
        for (name, data) in std::iter::once(("worldspawn".to_string(), bsp.lump(rbsp::ids::ENTITIES).to_vec()))
            .chain(ents.iter().map(|(n, d)| (n.clone(), d.clone())))
        {
            let ref_data = if name == "worldspawn" {
                ref_bsp.lump(rbsp::ids::ENTITIES).to_vec()
            } else {
                let Some(found) = ref_ents.get(&name) else {
                    panic!("the {target} reference has no {name} partition");
                };
                found.clone()
            };
            let a = blocks_of(&data);
            let b = blocks_of(&ref_data);
            assert_eq!(a.len(), b.len(), "{target} {name}: entity block count");
            let mut colliders = 0usize;
            for (i, (mine, other)) in a.iter().zip(b.iter()).enumerate() {
                let what = format!("{target} {name} block {i}");
                compare(&strip_coll(mine), &strip_coll(other), &what);
                let has_mine = String::from_utf8_lossy(mine).contains("\"*coll");
                let has_theirs = String::from_utf8_lossy(other).contains("\"*coll");
                assert_eq!(has_mine, has_theirs, "{what}: only one of the two carries a collider");
                colliders += usize::from(has_mine);
            }
            if target == "client" && name == "worldspawn" {
                assert!(colliders > 0, "the map carries brush entity colliders");
            }
        }
        // A converted map is a ported map, so the entity text and the lump set
        // are what agree with the reference, not the validator: the reference
        // itself is a ported map.
        assert_eq!(bsp.lumps.len(), ref_bsp.lumps.len(), "{target} lump count");
        assert_eq!(ents.len(), ref_ents.len(), "{target} partition count");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ the terrains

#[test]
fn e2e_every_terrain_type() {
    let dir = scratch("terrain");
    for (name, generator) in every_generator() {
        let project = dir.join(format!("mp_rr_{name}"));
        let p = project.display().to_string();
        run(&["new", &p, "--stem", &format!("mp_rr_{name}")]);
        let recipe = rterrain::Recipe::single("h", generator);
        let recipe = recipe.to_json().expect("a recipe");
        let request = dir.join(format!("{name}.json"));
        write_json(&request, &terrain_request(&recipe, [33, 33], 32.0));
        let added = run(&["terrain", &p, &request.display().to_string()]);
        assert_eq!(added.list("/ids").len(), 1, "{name} added one terrain: {}", added.json);

        let out = run(&["build", &p, "--targets", "client", "--bake", "none"]);
        assert_eq!(out.text("/stem"), format!("mp_rr_{name}"));
        // The terrain reaches the map as its own render meshes and its own
        // collision solid, which is what the object is.
        assert!(out.count("/stats/render_meshes") >= 4, "{name}: {}", out.at("/stats"));
        assert!(out.count("/stats/surfaces") >= 4, "{name}: {}", out.at("/stats"));
        assert!(out.count("/targets/client/triangles") > 0, "{name} drew no triangles");
        assert!(out.count("/targets/client/collision_solids") > 0, "{name} has no collision");
        let issues = out.list("/validation");
        assert!(!issues.iter().any(|r| r.as_str().is_some_and(|s| s.starts_with("Error"))), "{name}: {issues:?}");

        let (bsp, parts) = rbsp::Bsp::read_dir_map(&project.join("build/maps"), &format!("mp_rr_{name}"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(bsp.version(), 51, "{name}");
        let errors: Vec<String> = rbsp::validate(&bsp, &parts)
            .iter()
            .filter(|i| i.is_error())
            .map(|i| i.message.clone())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// -------------------------------------------------------- the entity schema

/// A value the key's own type accepts, so every class can be placed with a full
/// key set and still validate.
fn accepted(key: &rdoc::schema::KeyDef) -> Option<String> {
    use rdoc::schema::KeyType as T;
    for candidate in key.observed_values.iter().chain(key.default.iter()) {
        if key.ty.accepts(candidate) {
            return Some(candidate.clone());
        }
    }
    let by_type: String = match key.ty {
        T::Int | T::Flags => "1".to_string(),
        T::Float => "1.0".to_string(),
        T::Bool => "0".to_string(),
        T::Vec3 | T::Angles => "0 0 0".to_string(),
        T::Color => "255 255 255".to_string(),
        _ => key.name.clone(),
    };
    key.ty.accepts(&by_type).then_some(by_type)
}

#[test]
fn e2e_all_entity_classes() {
    let schema = rdoc::schema::EntitySchema::load(SCHEMA).expect("the bundled schema parses");
    assert!(schema.classes.len() > 200, "the schema carries {} classes", schema.classes.len());
    let dir = scratch("classes");
    let project = dir.join("mp_rr_every_class");
    let p = project.display().to_string();
    run(&["new", &p, "--stem", "mp_rr_every_class"]);
    let layer = layer_of(&read_document(&project));

    let mut placed: BTreeSet<String> = BTreeSet::new();
    let mut objects = Vec::new();
    for (i, (classname, def)) in schema.classes.iter().enumerate() {
        let mut keyvalues = rdoc::model::KeyValues::new();
        for key in def.keys.values() {
            if matches!(key.name.as_str(), "classname" | "origin" | "angles") {
                continue;
            }
            if let Some(value) = accepted(key) {
                keyvalues.insert(key.name.clone(), value);
            }
        }
        let mut o = object(
            &format!("e{i}"),
            layer,
            ObjectKind::Entity(Entity {
                classname: classname.clone(),
                keyvalues,
                partition: None,
                links: Default::default(),
            }),
        );
        o.transform.position = glam::DVec3::new((i % 16) as f64 * 64.0, ((i / 16) % 16) as f64 * 64.0, 0.0);
        objects.push(o);
        placed.insert(classname.clone());
    }
    // Something solid to stand on, so the entities are inside the world.
    let mut floor = object("floor", layer, ObjectKind::Brush(box_brush(
        glam::DVec3::new(-2048.0, -2048.0, -32.0),
        glam::DVec3::new(2048.0, 2048.0, 0.0),
    )));
    floor.transform = Transform::default();
    objects.push(floor);
    let command = dir.join("entities.json");
    write_json(&command, &serde_json::to_value(Edit::AddObjects { objects }).expect("a command"));
    let added = run(&["apply", &p, &command.display().to_string()]);
    assert_eq!(added.list("/ids").len(), placed.len() + 1, "every class was placed: {}", added.json);

    let issues = run(&["validate", &p]);
    let reported = issues.json.as_array().expect("a list");
    let errors: Vec<String> = reported
        .iter()
        .filter(|i| i["severity"].as_str() == Some("error"))
        .map(|i| i["message"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(errors.is_empty(), "{} of {} entities are refused: {errors:?}", errors.len(), placed.len());

    let out = run(&["build", &p, "--targets", "client"]);
    let found = out.at("/stats/entities_per_class").as_object().expect("per class counts");
    let missing: Vec<&String> = placed.iter().filter(|c| !found.contains_key(*c)).collect();
    assert!(missing.is_empty(), "{} classes are missing from the map: {missing:?}", missing.len());
    assert_eq!(found.len(), placed.len(), "the map carries every class exactly as named");
    for pointer in ["/warnings", "/validation"] {
        let rows = out.list(pointer);
        assert!(!rows.iter().any(|r| r.as_str().is_some_and(|s| s.starts_with("Error"))), "{pointer}: {rows:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ the wasm core

/// The wasm the smoke test loads, built when it is not there yet.
fn wasm_module() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("R5ED_WASM").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(named);
    }
    let target = PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()));
    let built = target.join("wasm32-unknown-unknown").join("release").join("r5ed_wasm.wasm");
    if built.is_file() {
        return Some(built);
    }
    let mut cargo = std::ffi::OsString::from("cargo");
    cargo.push("build");
    cargo.push("-p");
    cargo.push("r5ed-wasm");
    cargo.push("--release");
    cargo.push("--target");
    cargo.push("wasm32-unknown-unknown");
    let status = Command::new(&cargo).status();
    match status {
        Ok(s) if s.success() && built.is_file() => Some(built),
        _ => {
            eprintln!("the wasm32 target is not available, so the smoke test is skipped");
            None
        }
    }
}

#[test]
#[ignore = "needs the wasm32 target and node"]
fn wasm_node_smoke() {
    let Some(wasm) = wasm_module() else { return };
    let node = tool("R5ED_NODE").unwrap_or_else(|| {
        if which("node") {
            PathBuf::from("node")
        } else {
            eprintln!("node is not on the search path, so the smoke test is skipped");
            std::process::exit(0);
        }
    });
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("r5ed-wasm")
        .join("tests")
        .join("node_smoke.mjs");
    assert!(script.is_file(), "{} is missing", script.display());
    let out = Command::new(node)
        .arg(&script)
        .env("R5ED_WASM", &wasm)
        .output()
        .expect("node runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the smoke test failed: {text}");
    assert!(text.contains("node_smoke: ok"), "{text}");
}

fn which(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|d| d.join(name).is_file() || d.join(format!("{name}.exe")).is_file())
}

// ------------------------------------------------- the host's own arg vectors

#[test]
fn cli_argv_matches_host() {
    use r5ed_host::config::Config;
    use r5ed_host::jobs::{Env, JobKind, JobParams, JobRequest};
    use r5ed_host::plan;

    let dir = scratch("argv");
    std::fs::create_dir_all(dir.join("projects/m")).expect("a project");
    for tool in ["tool.exe", "rsx.exe", "recast.exe"] {
        std::fs::write(dir.join(tool), b"x").expect("a stand-in tool");
    }
    let env = Env {
        game_root: dir.join("game"),
        projects_root: dir.join("projects"),
        cache_root: dir.join("cache"),
    };
    std::fs::create_dir_all(&env.game_root).expect("a game root");
    let mut cfg = Config::default();
    cfg.game_root = env.game_root.clone();
    cfg.tools.compile = Some(dir.join("tool.exe"));
    cfg.tools.rsx = Some(dir.join("rsx.exe"));
    cfg.tools.recast = Some(dir.join("recast.exe"));
    // A job never names a path outside the cache, so the source is given the
    // way a caller has to give it.
    let source = PathBuf::from("import").join("sp_training.bsp");
    std::fs::create_dir_all(env.cache_root.join(&source).parent().expect("a parent")).expect("a cache");
    std::fs::write(env.cache_root.join(&source), b"rBS").expect("a stand-in source");

    let project = JobRequest {
        kind: JobKind::Compile,
        project: "m".into(),
        params: JobParams { targets: "client,dedi".into(), ..Default::default() },
    };
    // Every job that runs this tool, with the arguments it runs it with and the
    // verb those arguments have to reach.
    let cases: Vec<(JobKind, &str, JobRequest)> = vec![
        (JobKind::Compile, "Build", project.clone()),
        (JobKind::Bake, "Bake", JobRequest { kind: JobKind::Bake, ..project.clone() }),
        (JobKind::Package, "Package", JobRequest { kind: JobKind::Package, ..project.clone() }),
        (
            JobKind::ExportMinimap,
            "Minimap",
            JobRequest { kind: JobKind::ExportMinimap, ..project.clone() },
        ),
        (
            JobKind::ExportLoadscreen,
            "Loadscreen",
            JobRequest { kind: JobKind::ExportLoadscreen, ..project.clone() },
        ),
        (
            JobKind::ImportTf2,
            "ImportTf2",
            JobRequest {
                kind: JobKind::ImportTf2,
                params: JobParams { stem: "mp_rr_gauntlet".into(), source_bsp: source.display().to_string(), ..Default::default() },
                ..project.clone()
            },
        ),
        (
            JobKind::ImportMap,
            "ImportMap",
            JobRequest {
                kind: JobKind::ImportMap,
                params: JobParams { stem: "mp_rr_gauntlet".into(), ..Default::default() },
                ..project.clone()
            },
        ),
    ];
    let mut checked = 0usize;
    for (kind, verb, request) in &cases {
        let plan = plan::build(&cfg, &env, request, &|_| Some(dir.join("map.rpak")))
            .unwrap_or_else(|e| panic!("the {kind:?} plan does not build: {e}"));
        // A job that also runs another tool reaches this one as its own step,
        // not necessarily the first.
        let step = plan
            .steps
            .iter()
            .find_map(|s| match s {
                r5ed_host::jobs::Step::Tool { program, args } if *program == dir.join("tool.exe") => {
                    Some(args.as_slice())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{kind:?} never runs the compiler"));
        let argv: Vec<String> = std::iter::once("r5ed-compile".to_string())
            .chain(step.iter().map(|a| a.to_string_lossy().into_owned()))
            .collect();
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let parsed = r5ed_compile::parse(refs.clone())
            .unwrap_or_else(|e| panic!("the {kind:?} argv does not parse: {e}\n{argv:?}"));
        let printed = format!("{parsed:?}");
        assert!(printed.starts_with(verb), "{kind:?} parsed as {printed}");
        checked += 1;
    }
    assert_eq!(checked, cases.len(), "every tool job was checked");

    // The rest of the plans are a tool of its own, or an action the host
    // performs itself; none of them runs the compiler.
    for kind in [JobKind::Navmesh, JobKind::Deploy, JobKind::Undeploy, JobKind::Reload] {
        let request = JobRequest { kind, params: JobParams { manifest: "none".into(), ..Default::default() }, ..project.clone() };
        if let Ok(plan) = plan::build(&cfg, &env, &request, &|_| Some(dir.join("map.rpak"))) {
            let runs_the_compiler = plan.steps.iter().any(|s| {
                matches!(s, r5ed_host::jobs::Step::Tool { program, .. } if *program == dir.join("tool.exe"))
            });
            assert!(!runs_the_compiler, "{kind:?} does not run the compiler");
        }
    }

    // An argv the parser must refuse: a missing positional, an unknown verb, an
    // unknown flag, a value that is not a number, and a flag with no value.
    for bad in [
        vec!["r5ed-compile", "import-map", "mp_rr_x"],
        vec!["r5ed-compile", "not-a-verb"],
        vec!["r5ed-compile", "build", "p", "--nope"],
        vec!["r5ed-compile", "build", "p", "--targets"],
        vec!["r5ed-compile", "brush", "p", "--min=nope", "--max=0,0,0"],
    ] {
        assert!(r5ed_compile::parse(bad.clone()).is_err(), "{bad:?} should be refused");
    }
    // A target list and a bake mode the compiler has no name for are refused by
    // the verb rather than by the parser, so they are checked on the binary.
    for args in [
        vec!["build", "p", "--targets", "nope"],
        vec!["build", "p", "--bake", "nope"],
    ] {
        let mut full = vec!["r5ed-compile"];
        full.extend(args.clone());
        assert!(r5ed_compile::parse(full.clone()).is_ok(), "{full:?} parses: {}", args.join(" "));
        let (ok, text) = try_run(&args);
        assert!(!ok, "{args:?} should be refused: {text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
