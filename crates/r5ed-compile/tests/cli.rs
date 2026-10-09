//! End-to-end tests of the headless CLI. Every one of them runs the real
//! binary, so what is checked is what a caller gets.
//!
//! The tests that need an external tool or a game install are marked ignored and
//! skip themselves when the tool is not configured; the gate runs them with
//! `--include-ignored` and points them at a real install.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_r5ed-compile");

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
    fn count(&self, pointer: &str) -> usize {
        self.at(pointer).as_u64().unwrap_or_else(|| panic!("{pointer} is not a count")) as usize
    }
}

fn raw(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("the CLI runs")
}

fn run(args: &[&str]) -> Run {
    let out = raw(args);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{args:?} failed: {}\n{stderr}", out.status);
    let json: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{args:?} printed no JSON: {e}\n{stdout}"));
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
    let dir = std::env::temp_dir().join(format!("r5ed-cli-{}-{name}", std::process::id()));
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

/// A room to compile: a floor, four walls and a light.
fn room(project: &Path, stem: &str) {
    let p = project.display().to_string();
    run(&["new", &p, "--stem", stem]);
    for (min, max) in [
        ("-512,-512,0", "512,512,16"),
        ("-512,-512,16", "512,-496,272"),
        ("-512,496,16", "512,512,272"),
        ("-512,-496,16", "-496,496,272"),
        ("496,-496,16", "512,496,272"),
    ] {
        run(&["brush", &p, &format!("--min={min}"), &format!("--max={max}")]);
    }
    run(&["light", &p, "--kind", "point", "--origin=0,0,200"]);
}

/// A solid test image, encoded the way a caller would hand one over.
fn image_png(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let o = ((y * width + x) * 4) as usize;
            pixels[o] = (x * 7) as u8;
            pixels[o + 1] = (y * 5) as u8;
            pixels[o + 2] = 90;
            pixels[o + 3] = 255;
        }
    }
    rpak::encode_rgba8(&pixels, width, height).expect("a png")
}

/// The container fields of a `uiia`, read without the encoder that wrote it.
fn parse_uiia(bytes: &[u8]) -> (u16, u16, u16, usize) {
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    assert_eq!(u32_at(0), 0x4149_4955, "the container magic");
    let n = u32_at(4) as usize;
    let sub = 8 + n * 4;
    let raw = sub + 64;
    (
        u16_at(sub + 0x18),
        u16_at(sub + 0x1A),
        u16_at(sub + 0x12),
        bytes.len() - raw - 64,
    )
}

/// One asset a pak carries.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Asset {
    kind: String,
    path: String,
    guid: u64,
}

/// Every asset a pak carries, read by the asset tool.
fn pak_assets(rsx: &Path, pak: &Path, dir: &Path) -> Vec<Asset> {
    std::fs::create_dir_all(dir).expect("a list directory");
    let list = dir.join("assets.csv");
    let _ = std::fs::remove_file(&list);
    let out = Command::new(rsx)
        .args(["-nogui", "-nocachedb", "--list"])
        .arg(&list)
        .args(["--listformat", "csv"])
        .arg(pak)
        // The tool writes a multi-megabyte cache database into whatever directory
        // it runs in, which under a test run is the crate itself.
        .current_dir(dir)
        .output()
        .expect("the asset tool runs");
    assert!(out.status.success(), "listing {} failed", pak.display());
    let text = std::fs::read_to_string(&list).expect("a list");
    let mut rows = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 6 {
            continue;
        }
        if let Ok(guid) = u64::from_str_radix(f[3], 16) {
            rows.push(Asset { kind: f[0].to_string(), path: f[5..].join(",").replace('\\', "/"), guid });
        }
    }
    rows
}

/// Exports one asset out of a pak and returns the file it wrote.
fn export_asset(rsx: &Path, pak: &Path, kind: &str, dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("an export directory");
    let out = Command::new(rsx)
        .args(["-nogui", "-nocachedb", "-export", "--exporttypes", kind, "--exportdir"])
        .arg(dir)
        .arg(pak)
        .current_dir(dir)
        .output()
        .expect("the asset tool runs");
    assert!(out.status.success(), "exporting {kind} from {} failed", pak.display());
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("a directory").flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some(kind) {
                found.push(p);
            }
        }
    }
    assert_eq!(found.len(), 1, "expected one {kind} in the export, got {}", found.len());
    found.remove(0)
}

#[test]
fn cli_new_then_build_writes_both_targets() {
    let dir = scratch("build");
    let project = dir.join("mp_rr_cli");
    room(&project, "mp_rr_cli");

    let out = run(&["build", &project.display().to_string(), "--out", &dir.join("build").display().to_string(), "--targets", "client,dedi"]);
    assert_eq!(out.text("/stem"), "mp_rr_cli");
    assert_eq!(out.text("/targets/client/bsp"), "maps/mp_rr_cli.bsp");
    assert_eq!(out.text("/targets/dedi/bsp"), "dedi/maps/mp_rr_cli.bsp");
    assert!(out.count("/targets/client/triangles") >= 12, "the room has faces: {}", out.at("/targets/client"));
    assert!(out.count("/targets/client/collision_solids") >= 1);
    assert_eq!(out.count("/stats/entities") > 0, true, "the map carries entities");
    assert!(out.stderr.contains("PROGRESS 100"), "the progress lines are on stderr: {}", out.stderr);
    for pointer in ["/warnings", "/validation"] {
        let rows = out.at(pointer).as_array().expect("a list");
        assert!(!rows.iter().any(|r| r.as_str().is_some_and(|s| s.starts_with("Error"))), "{pointer} has an error: {rows:?}");
    }

    // Both maps are read back through the container reader and the engine
    // validator, which is the same path a deploy takes.
    for (target, sub) in [("client", "maps"), ("dedi", "dedi/maps")] {
        let bsp_path = dir.join("build").join(sub).join("mp_rr_cli.bsp");
        assert!(bsp_path.is_file(), "{} is missing", bsp_path.display());
        let bsp = rbsp::Bsp::read_path(&bsp_path).unwrap_or_else(|e| panic!("{}: {e}", bsp_path.display()));
        assert_eq!(bsp.version(), if target == "client" { 51 } else { 47 });
        let mut ents = std::collections::BTreeMap::new();
        if let Ok(rd) = std::fs::read_dir(bsp_path.parent().unwrap()) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("ent") {
                    // The validator keys partitions by name, not by file name.
                    let name = p.file_stem().unwrap().to_string_lossy().into_owned();
                    let name = name.strip_prefix("mp_rr_cli_").unwrap_or(&name).to_string();
                    ents.insert(name, std::fs::read(&p).unwrap());
                }
            }
        }
        let issues = rbsp::validate(&bsp, &ents);
        let errors: Vec<String> = issues.iter().filter(|i| i.is_error()).map(|i| i.message.clone()).collect();
        assert!(errors.is_empty(), "{target} map: {errors:?}");
        assert!(bsp.lump(rbsp::ids::ENTITIES).len() > 0, "{target} map carries no entities");
    }
    // The client map keeps its lumps beside it, which is the layout a pak holds.
    let lumps = std::fs::read_dir(dir.join("build/maps")).unwrap().flatten().count();
    assert!(lumps > 2, "the client map is {lumps} files");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn cli_package_copies_used_materials() {
    let (Some(rsx), Some(_)) = (tool("R5ED_RSX"), tool("R5ED_REPAK")) else {
        return;
    };
    assert!(has_install(), "R5ED_GAME_ROOT names no install");
    let dir = scratch("package");
    let project = dir.join("mp_rr_pkg");
    let p = project.display().to_string();
    run(&["new", &p, "--stem", "mp_rr_pkg"]);
    // A material the install carries, a static prop that bakes a permanent
    // vertex cache, and a model that only streams its vertex groups.
    run(&["brush", &p, "--min=-64,-64,0", "--max=64,64,32", "--material", "world/dev/dev_gray_512"]);
    run(&["prop", &p, "--model", PACKABLE_MODEL, "--origin=0,0,32"]);
    run(&["prop", &p, "--model", STREAMING_MODEL, "--origin=128,0,32"]);

    let out = run(&["package", &p, &dir.join("stage").display().to_string()]);
    assert_eq!(out.text("/stem"), "mp_rr_pkg");
    assert!(out.count("/assets") > 0, "the archive carries assets: {}", out.json);
    assert_eq!(out.text("/flags"), "0x0020", "the loader flag is set and nothing is encoded");

    // Every texture a material names is staged, which is the whole point of a
    // packaged map not leaning on a shipped pak for its own surfaces.
    assert_eq!(
        out.at("/textures_not_packed").as_array().expect("a list"),
        &Vec::<Value>::new(),
        "every texture the document reaches is in the archive"
    );

    // Both models go in with their streamed vertex groups, which the packer
    // writes into the map's own starpak beside the archive.
    let mut packed_models: Vec<&str> =
        out.at("/models_packed").as_array().expect("a list").iter().filter_map(Value::as_str).collect();
    packed_models.sort_unstable();
    let mut expected = vec![PACKABLE_MODEL, STREAMING_MODEL];
    expected.sort_unstable();
    assert_eq!(packed_models, expected);
    assert_eq!(out.at("/models_not_packed").as_array().expect("a list"), &Vec::<Value>::new());
    assert_eq!(out.at("/missing").as_array().expect("a list"), &Vec::<Value>::new());

    let pak = PathBuf::from(out.text("/pak"));
    assert!(pak.is_file(), "{} is missing", pak.display());
    let starpak = pak.with_extension("starpak");
    assert!(starpak.is_file(), "{} is missing", starpak.display());
    let rows = pak_assets(&rsx, &pak, &dir.join("list"));
    let named: Vec<&str> = rows.iter().map(|a| a.kind.as_str()).collect();
    assert!(named.contains(&"matl"), "the material is in the archive: {named:?}");
    assert!(named.contains(&"mdl_"), "the model is in the archive: {named:?}");
    assert!(named.contains(&"txtr"), "the textures are in the archive: {named:?}");

    // Every material the document names is in the archive, checked by identity.
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(project.join("map.json")).unwrap()).unwrap();
    let mut wanted: Vec<String> = Vec::new();
    for o in doc["objects"].as_array().expect("objects") {
        if o["kind"]["type"] == "brush" {
            for f in o["kind"]["faces"].as_array().expect("faces") {
                wanted.push(f["material"].as_str().unwrap_or_default().to_string());
            }
        }
    }
    wanted.dedup();
    assert_eq!(wanted, vec!["world/dev/dev_gray_512".to_string()], "the document names its materials");
    let packed = pak_assets(&rsx, &pak, &dir.join("list2"));
    assert!(!packed.is_empty(), "the archive lists its assets: {packed:?}");
    let materials: Vec<&Asset> = packed.iter().filter(|a| a.kind == "matl").collect();
    assert_eq!(materials.len(), 1, "one material is packed: {materials:?}");
    let copy = materials[0];
    assert!(copy.path.contains("dev_gray_512"), "{}", copy.path);
    let same = install_asset_by_guid(&rsx, "matl", copy.guid)
        .unwrap_or_else(|| panic!("the install carries no material {copy:?}"));
    assert_eq!(same.path, copy.path, "the copy kept the install's name for it");
    // The model kept its identity too, so a prop still resolves to the same
    // asset it was authored against.
    let models: Vec<&Asset> = packed.iter().filter(|a| a.kind == "mdl_").collect();
    assert_eq!(models.len(), 1, "one model is packed: {models:?}");
    assert!(models[0].path.ends_with(".rmdl"), "{}", models[0].path);
    let same = install_asset_by_guid(&rsx, "mdl_", models[0].guid)
        .unwrap_or_else(|| panic!("the install carries no model {models:?}"));
    assert_eq!(same.path, models[0].path, "the copy kept the install's name for it");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A model the install carries as a static prop, with a permanent vertex cache
/// as well as its streamed vertex groups.
const PACKABLE_MODEL: &str = "mdl/props/flask/prop_flask.rmdl";
/// A model whose vertex groups only stream.
const STREAMING_MODEL: &str = "mdl/props/small_flag/small_flag_w.rmdl";

/// A document that names a model is compiled against the install's own copy of
/// it, so the model has to be read out of the export at the path the asset tool
/// wrote it, not the path its name suggests.
#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn cli_build_against_the_install_resolves_models() {
    let (Some(_), Some(_)) = (tool("R5ED_RSX"), tool("R5ED_REPAK")) else {
        return;
    };
    assert!(has_install(), "R5ED_GAME_ROOT names no install");
    let dir = scratch("install");
    let project = dir.join("mp_rr_inst");
    let p = project.display().to_string();
    run(&["new", &p, "--stem", "mp_rr_inst"]);
    run(&["brush", &p, "--min=-64,-64,0", "--max=64,64,32", "--material", "world/dev/dev_gray_512"]);
    run(&["prop", &p, "--model", PACKABLE_MODEL, "--origin=0,0,32"]);

    // The asset table the install produces has to resolve both the material and
    // the model, or the run fails before a byte of the map is written.
    let out = run(&["build", &p, "--install", "--targets", "client"]);
    assert_eq!(out.text("/stem"), "mp_rr_inst");
    assert_eq!(out.count("/stats/props"), 1, "the prop is in the map: {}", out.at("/stats"));
    assert!(out.count("/targets/client/triangles") > 0, "the room is in the map: {}", out.at("/targets/client"));
    let issues = out.at("/validation").as_array().expect("a list");
    assert!(!issues.iter().any(|i| i.as_str().is_some_and(|s| s.starts_with("Error"))), "{issues:?}");
    let bsp_path = project.join("build/maps/mp_rr_inst.bsp");
    let bsp = rbsp::Bsp::read_path(&bsp_path).unwrap_or_else(|e| panic!("{}: {e}", bsp_path.display()));
    assert_eq!(bsp.version(), 51);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The guid an install carries one asset under, found by listing its pak.
/// The paks the asset index covers, which is where a map's own assets live.
fn indexed_paks() -> Vec<String> {
    let Some(root) = std::env::var_os("R5ED_GAME_ROOT") else { return Vec::new() };
    rpak::AssetIndex::scan_paks(Path::new(&root), &["Win64"])
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// The asset an install carries under one guid.
fn install_asset_by_guid(rsx: &Path, kind: &str, guid: u64) -> Option<Asset> {
    let root = PathBuf::from(std::env::var_os("R5ED_GAME_ROOT")?);
    let dir = std::env::temp_dir().join(format!("r5ed-cli-{}-{kind}", std::process::id()));
    for pak in indexed_paks() {
        let p = root.join("paks").join("Win64").join(pak);
        if !p.is_file() {
            continue;
        }
        let rows = pak_assets(rsx, &p, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(hit) = rows.into_iter().find(|a| a.kind == kind && a.guid == guid) {
            return Some(hit);
        }
    }
    None
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn cli_loadscreen_writes_a_uiia() {
    let (Some(rsx), Some(_)) = (tool("R5ED_RSX"), tool("R5ED_REPAK")) else {
        return;
    };
    let dir = scratch("loadscreen");
    let project = dir.join("mp_rr_ls");
    let p = project.display().to_string();
    run(&["new", &p, "--stem", "mp_rr_ls"]);
    let image = dir.join("art.png");
    std::fs::write(&image, image_png(1920, 64)).expect("an image");
    let out = run(&["loadscreen", &p, &image.display().to_string()]);
    assert_eq!(out.text("/stem"), "mp_rr_ls");
    assert_eq!(out.text("/flags"), "0x0020");
    assert_eq!(out.count("/assets"), 1, "a loadscreen archive holds one asset");
    assert_eq!(out.text("/uiia/asset"), "ui_image/loadscreens/mp_rr_ls_widescreen.rpak");
    let guid = out.text("/uiia/guid").trim_start_matches("0x").to_string();
    assert_eq!(guid.len(), 16, "the guid is a full one: {guid}");
    assert_eq!(out.count("/uiia/width"), 1920);
    assert_eq!(out.count("/uiia/height"), 64);

    // The archive the packer wrote is read back by the asset tool, and the
    // asset comes out of it as a container this test parses on its own.
    let pak = PathBuf::from(out.text("/pak"));
    assert!(pak.is_file(), "{} is missing", pak.display());
    let rows = pak_assets(&rsx, &pak, &dir.join("list"));
    assert_eq!(rows.len(), 1, "a loadscreen archive holds one asset: {rows:?}");
    let want = u64::from_str_radix(&guid, 16).unwrap();
    assert!(rows.iter().any(|a| a.kind == "uiia" && a.guid == want), "the asset is in the archive: {rows:?}");
    let raw = export_asset(&rsx, &pak, "uiia", &dir.join("export"));
    let bytes = std::fs::read(&raw).expect("the exported asset");
    let (w, h, flags, tiles) = parse_uiia(&bytes);
    assert_eq!((w, h), (1920, 64), "the extents survive the archive");
    assert_eq!(flags, 0x00C2, "a BC7 tile grid the engine builds itself");
    // 62 x 3 tiles of 1024 bytes for a 1920 x 64 image.
    assert_eq!(tiles, 62 * 3 * 1024, "the tile blob is whole tiles");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn cli_minimap_writes_an_overview() {
    let (Some(rsx), Some(_)) = (tool("R5ED_RSX"), tool("R5ED_REPAK")) else {
        return;
    };
    let dir = scratch("minimap");
    let project = dir.join("mp_rr_mm");
    let p = project.display().to_string();
    room(&project, "mp_rr_mm");
    // The map goes where the host puts it, so the minimap finds it without being told.
    run(&["build", &p, "--targets", "client"]);

    let out = run(&["minimap", &p]);
    assert_eq!(out.text("/stem"), "mp_rr_mm");
    assert_eq!(out.text("/overview/path"), "resource/overviews/mp_rr_mm.txt");
    assert_eq!(out.text("/overview/minimap"), "overviews\\mp_rr_mm");
    assert_eq!(out.count("/assets"), 2, "the image and the document");
    assert_eq!(out.text("/uiia/asset"), "ui_image/overviews/mp_rr_mm.rpak");

    // The document is where the engine looks for it, and it names the image.
    let overview = project.join("build/overview/resource/overviews/mp_rr_mm.txt");
    assert!(overview.is_file(), "{} is missing", overview.display());
    let text = std::fs::read_to_string(&overview).expect("the overview document");
    assert!(text.starts_with("mp_rr_mm\n{\n"), "{text}");
    assert!(text.contains("minimap overviews\\mp_rr_mm\n"), "{text}");
    assert!(text.contains("\tpos_x -8192\n") && text.contains("\tpos_y -8192\n"), "{text}");
    assert!(text.contains("\tscale 8\n"), "{text}");

    // The archive carries both, and the image is the render of the map.
    let pak = PathBuf::from(out.text("/pak"));
    let rows = pak_assets(&rsx, &pak, &dir.join("list"));
    let names: Vec<&str> = rows.iter().map(|a| a.kind.as_str()).collect();
    assert!(names.contains(&"uiia"), "the image is in the archive: {names:?}");
    assert!(names.contains(&"wrap"), "the document is in the archive: {names:?}");
    assert!(out.count("/uiia/width") > 0 && out.count("/uiia/height") > 0, "the image has an extent");
    let raw = export_asset(&rsx, &pak, "uiia", &dir.join("export"));
    let (w, h, flags, tiles) = parse_uiia(&std::fs::read(&raw).expect("the exported image"));
    assert_eq!((w, h), (out.count("/uiia/width") as u16, out.count("/uiia/height") as u16));
    assert_eq!(flags, 0x00C2);
    assert!(tiles > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_register_writes_the_manifest() {
    let dir = scratch("register");
    let project = dir.join("mp_rr_reg");
    let p = project.display().to_string();
    room(&project, "mp_rr_reg");
    // The map goes where the host puts it, which is where a register job looks.
    run(&["build", &p, "--targets", "client,dedi"]);
    let out = run(&["register", &p]);
    assert_eq!(out.text("/stem"), "mp_rr_reg");
    assert_eq!(out.text("/display_name"), "Untitled");
    assert_eq!(out.text("/wip_maps"), format!("{:<34}= Untitled", "mp_rr_reg"));

    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(project.join("build/deploy/register.json")).unwrap()).unwrap();
    assert_eq!(manifest["stem"], "mp_rr_reg");
    let files = manifest["files"].as_array().expect("the file list");
    assert!(!files.is_empty(), "the manifest names the map's files");
    let bsp = files.iter().find(|f| f["path"] == "maps/mp_rr_reg.bsp").expect("the client map is named");
    let named: Vec<String> = files.iter().filter_map(|f| f["path"].as_str().map(str::to_string)).collect();
    assert!(named.iter().all(|p| p.starts_with("maps/") || p.starts_with("paks/")), "only deployable directories are named: {named:?}");
    assert!(bsp["bytes"].as_u64().expect("a size") > 0, "the client map has bytes: {bsp}");
    for f in files {
        let rel = f["path"].as_str().expect("a path");
        let full = project.join("build").join(rel);
        assert!(full.is_file(), "{rel} is named but missing");
        assert_eq!(std::fs::metadata(&full).unwrap().len(), f["bytes"].as_u64().unwrap(), "{rel}");
    }
    // The two files an operator merges by hand are written too.
    // The build-local files are named so they cannot be mistaken for the
    // install's own: a one-line file called r5f_map_names.txt would empty the
    // launcher's allowlist the moment anyone copied it over.
    let names = std::fs::read_to_string(project.join("build/deploy/r5f_wip_maps.txt")).unwrap();
    assert!(names.contains("mp_rr_reg"), "{names}");
    assert!(!project.join("build/deploy/r5f_map_names.txt").exists(), "the ship allowlist is never written");
    let patch = std::fs::read_to_string(project.join("build/deploy/playlists_r5_patch.fragment.txt")).unwrap();
    assert!(patch.contains("mp_rr_reg 1"), "{patch}");
    assert!(patch.contains("inherit survival_dev"), "{patch}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_register_merges_into_an_install_without_disturbing_it() {
    let dir = scratch("register-install");
    let project = dir.join("mp_rr_merge");
    let install = dir.join("install");
    std::fs::create_dir_all(install.join("platform")).unwrap();
    let patch_path = install.join("platform/playlists_r5_patch.txt");
    let wip_path = install.join("platform/r5f_wip_maps.txt");
    let patch = "playlists\n{\n\tGamemodes\n\t{\n\t\tdefaults\n\t\t{\n\t\t\tmaps\n\t\t\t{\n\t\t\t}\n\t\t}\n\t}\n}\n";
    std::fs::write(&patch_path, patch).unwrap();
    std::fs::write(&wip_path, "# extra maps\nmp_rr_salham = Loba Chronicle\n").unwrap();
    let p = project.display().to_string();
    room(&project, "mp_rr_merge");
    run(&["build", &p, "--targets", "client"]);
    let out = run(&["register", &p, "--install", &install.display().to_string()]);
    assert_eq!(out.at("/installed/declared").as_bool(), Some(true));
    assert_eq!(out.at("/installed/named").as_bool(), Some(true));

    let merged = std::fs::read_to_string(&patch_path).unwrap();
    assert!(merged.contains("mp_rr_merge 1"), "{merged}");
    assert!(merged.contains("maps"), "the block it went into is still there");
    let wip = std::fs::read_to_string(&wip_path).unwrap();
    assert!(wip.contains("mp_rr_salham = Loba Chronicle"), "the other map survives: {wip}");
    assert!(wip.contains("mp_rr_merge"), "{wip}");

    // A second run is a no-op, which is what makes this safe on every build.
    let again = run(&["register", &p, "--install", &install.display().to_string()]);
    assert_eq!(again.at("/installed/declared").as_bool(), Some(false));
    assert_eq!(again.at("/installed/named").as_bool(), Some(false));
    assert_eq!(std::fs::read_to_string(&patch_path).unwrap(), merged, "the patch is byte-identical");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_import_map_takes_stem_and_project() {
    let dir = scratch("import");
    let project = dir.join("mp_rr_imp");
    let p = project.display().to_string();
    room(&project, "mp_rr_imp");
    run(&["build", &p, "--out", &dir.join("build").display().to_string(), "--targets", "client"]);

    // Exactly two positional arguments, and a stem the launcher offers.
    let (ok, text) = try_run(&["import-map", "mp_rr_imp"]);
    assert!(!ok, "one argument is not a map import: {text}");
    assert!(text.contains("required"), "{text}");
    let (ok, text) = try_run(&["import-map", "mp_rr_imp", &p, "extra"]);
    assert!(!ok, "three arguments are not a map import: {text}");
    let maps = dir.join("build/maps").display().to_string();
    let (ok, text) = try_run(&["import-map", "not_a_map_stem", &p, "--from", &maps]);
    assert!(!ok, "a stem the launcher will not offer is refused: {text}");
    assert!(text.contains("mp_rr_"), "{text}");

    // With both, the map that was just compiled comes back as a project.
    let back = dir.join("reimported");
    let back_arg = back.display().to_string();
    let out = run(&["import-map", "mp_rr_imp", &back_arg, "--from", &maps]);
    assert_eq!(out.text("/stem"), "mp_rr_imp");
    assert_eq!(out.count("/map_version"), 51);
    assert!(out.count("/lumps") > 0, "the container was read: {}", out.json);
    assert!(out.count("/objects") > 0, "the document has objects: {}", out.json);
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(back.join("map.json")).unwrap()).expect("a document");
    assert_eq!(doc["meta"]["stem"], "mp_rr_imp");
    let files = out.at("/files").as_array().expect("the written files");
    assert!(files.iter().any(|f| f == "map.json"), "the document is named: {files:?}");
    for f in files {
        let rel = f.as_str().expect("a path");
        assert!(back.join(rel).is_file(), "{rel} was named but not written");
    }
    // An import over an existing document is refused unless it is asked for.
    let (ok, text) = try_run(&["import-map", "mp_rr_imp", &back_arg, "--from", &maps]);
    assert!(!ok, "a second import over the same project is refused: {text}");
    assert!(text.contains("--force"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn cli_mesh_verbs_edit_an_editable_mesh() {
    let dir = scratch("mesh");
    let project = dir.join("mp_rr_mesh");
    let p = project.display().to_string();
    run(&["new", &p, "--stem", "mp_rr_mesh"]);

    let write_req = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("a request file");
        path.display().to_string()
    };

    // An editable box straight from the generator.
    let req = write_req("box.json", r#"{"spec":{"shape":"box","size":[64,64,64]},"editable":true}"#);
    let added = run(&["mesh", &p, &req]);
    let id = added.text("/ids/0").to_string();

    // Info names the counts, the slots and a clean bill of structure.
    let info = run(&["mesh-info", &p, id.as_str()]);
    assert_eq!(info.count("/verts"), 8, "the box shares its corners: {}", info.json);
    assert_eq!(info.count("/faces"), 6, "the generator's triangles merge back into quads");
    assert_eq!(info.at("/closed"), &Value::Bool(true));
    assert_eq!(info.at("/wound"), &Value::Bool(true));
    assert_eq!(info.count("/euler"), 2);
    assert!(info.at("/materials").as_array().is_some_and(|m| !m.is_empty()));

    // Dump renders the file as JSON.
    let dump = run(&["mesh-dump", &p, id.as_str()]);
    assert_eq!(dump.at("/positions").as_array().expect("positions").len(), 8);
    assert_eq!(dump.at("/faces").as_array().expect("faces").len(), 6);

    // One op through the contract: lift the top along its normal.
    let op = write_req(
        "op.json",
        &format!(
            r#"{{"op":"extrude_region","object":"{id}","sel":{{"mode":"face","query":{{"kind":"normal","dir":[0,0,1]}}}},"params":{{"distance":16.0}}}}"#
        ),
    );
    let done = run(&["mesh-op", &p, &op]);
    assert_eq!(done.count("/report/faces"), 10, "six minus the top, plus its cap and four walls: {}", done.json);
    let info = run(&["mesh-info", &p, id.as_str()]);
    assert_eq!(info.at("/closed"), &Value::Bool(true), "the extrusion stayed closed: {}", info.json);

    // A glb mesh becomes editable, and a brush becomes a mesh.
    let greq = write_req("gbox.json", r#"{"spec":{"shape":"box","size":[32,32,32]}}"#);
    let gadded = run(&["mesh", &p, &greq]);
    let gid = gadded.text("/ids/0").to_string();
    run(&["make-editable", &p, gid.as_str()]);
    let ginfo = run(&["mesh-info", &p, gid.as_str()]);
    assert_eq!(ginfo.count("/verts"), 8);

    run(&["brush", &p, "--min=0,0,0", "--max=64,64,64"]);
    let list = run(&["list", &p]);
    let bid = list
        .json
        .as_array()
        .expect("a list")
        .iter()
        .find(|o| o["kind"] == "brush")
        .expect("a brush")
        .get("id")
        .expect("an id")
        .as_str()
        .expect("a string")
        .to_string();
    run(&["brush-to-mesh", &p, bid.as_str()]);
    let binfo = run(&["mesh-info", &p, bid.as_str()]);
    assert_eq!(binfo.count("/faces"), 6, "one n-gon per brush face: {}", binfo.json);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_join_merges_mesh_and_brush_then_builds() {
    let dir = scratch("join");
    let project = dir.join("mp_rr_join");
    let p = project.display().to_string();
    run(&["new", &p, "--stem", "mp_rr_join"]);

    let req = dir.join("box.json");
    std::fs::write(&req, r#"{"spec":{"shape":"box","size":[64,64,64]},"editable":true}"#).expect("a request file");
    let added = run(&["mesh", &p, &req.display().to_string()]);
    let mid = added.text("/ids/0").to_string();

    run(&["brush", &p, "--min=128,-32,0", "--max=192,32,64"]);
    let list = run(&["list", &p]);
    let bid = list
        .json
        .as_array()
        .expect("a list")
        .iter()
        .find(|o| o["kind"] == "brush")
        .expect("a brush")
        .get("id")
        .expect("an id")
        .as_str()
        .expect("a string")
        .to_string();

    // One id is not a join.
    let (ok, _) = try_run(&["join", &p, mid.as_str()]);
    assert!(!ok, "join needs an active id and at least one source");

    run(&["join", &p, mid.as_str(), bid.as_str()]);
    let info = run(&["mesh-info", &p, mid.as_str()]);
    assert_eq!(info.count("/faces"), 12, "six box quads plus six brush n-gons: {}", info.json);
    assert_eq!(info.count("/verts"), 16, "both corners tables survive: {}", info.json);

    let list = run(&["list", &p]);
    assert_eq!(
        list.json.as_array().expect("a list").iter().filter(|o| o["kind"] == "mesh").count(),
        1,
        "one object remains: {}",
        list.json
    );

    // The joined object compiles: twelve triangles plus six quads as two each.
    run(&["light", &p, "--kind", "point", "--origin=0,0,200"]);
    let out =
        run(&["build", &p, "--out", &dir.join("build").display().to_string(), "--targets", "client"]);
    assert_eq!(out.text("/stem"), "mp_rr_join");
    assert!(out.count("/targets/client/triangles") >= 24, "the joined mesh compiled: {}", out.at("/targets/client"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_mesh_boolean_cuts_a_cutter() {
    let dir = scratch("bool");
    let project = dir.join("mp_rr_bool");
    let p = project.display().to_string();
    run(&["new", &p, "--stem", "mp_rr_bool"]);

    let wall = dir.join("wall.json");
    std::fs::write(&wall, r#"{"spec":{"shape":"box","size":[128,32,64]},"editable":true}"#).expect("a request file");
    let wall_id = run(&["mesh", &p, &wall.display().to_string()]).text("/ids/0").to_string();

    let cutter = dir.join("cutter.json");
    std::fs::write(
        &cutter,
        r#"{"spec":{"shape":"box","size":[32,64,96]},"editable":true,"transform":{"position":[32,0,-8],"rotation":[0,0,0,1],"scale":[1,1,1]}}"#,
    )
    .expect("a request file");
    let cutter_id = run(&["mesh", &p, &cutter.display().to_string()]).text("/ids/0").to_string();

    let op = dir.join("op.json");
    std::fs::write(
        &op,
        format!(
            r#"{{"op":"boolean","object":"{wall_id}","sel":{{"mode":"face","ids":[]}},"params":{{"operation":"difference","operand":"{cutter_id}"}}}}"#
        ),
    )
    .expect("a request file");
    let done = run(&["mesh-op", &p, &op.display().to_string()]);
    assert!(done.count("/report/faces") > 0, "the cut keeps faces: {}", done.json);
    let info = run(&["mesh-info", &p, wall_id.as_str()]);
    assert_eq!(info.at("/closed"), &Value::Bool(true), "the doorway stayed closed: {}", info.json);
    let _ = std::fs::remove_dir_all(&dir);
}
