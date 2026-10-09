//! `package`: a project becomes the archives a deploy places.
//!
//! Four things come out of a package. The archive the document's own assets go
//! in carries every material, texture and model the document names, so a
//! packaged map no longer depends on a shipped map carrying those assets. The
//! two wrap archives carry the client map, split the way every shipped map
//! splits it. The dedicated-server VPK carries the dedicated map and its entity
//! partitions. And when the server map names a model no pak the server always
//! loads carries, the install's server copy of it goes into the map's server pak.
//!
//! The asset tool writes each export under its own file name, so the staged
//! tree is rebuilt here at the path each asset is stored under, which is what a
//! manifest entry names and what the packer opens. Every asset is pinned to the
//! guid the install gave it, so a copy is findable by the same identity.
//!
//! A model is staged with the model itself, its physics, the permanent vertex
//! cache a static prop reads its geometry from, and its streamed vertex groups.
//! The client streams every model's vertex groups at some point, static props
//! included, so those go in the map's own starpak beside its archive. A model
//! exported without them is named in the report rather than packed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rbsp::Bsp;
use rdoc::project::{self, NativeProject};
use serde_json::{json, Value};

use crate::assets::{self, Install};
use rpak::AssetRow;
use crate::pak::{self, Entry, Manifest};
use crate::vpk::Built;
use crate::{vpk, wraps};

/// Everything `package` takes.
#[derive(Clone, Debug)]
pub struct Args {
    pub project: PathBuf,
    /// Staging directory; defaults to `<project>/build/package`.
    pub stage: Option<PathBuf>,
    /// Where the archives are written; defaults to the staging directory.
    pub out: Option<PathBuf>,
}

/// One asset the document needs and the archive could not take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub name: String,
    pub kind: &'static str,
    /// Why it could not go in, in words the caller can act on.
    pub why: String,
}

/// The default staging directory of a project.
pub fn default_stage(project: &Path) -> PathBuf {
    project.join("build").join("package")
}

/// Copies the assets a document names into a map archive and reports what went in.
pub fn run(args: &Args) -> Result<Value> {
    let document = project::load(Box::new(NativeProject::new(&args.project).map_err(|e| anyhow!("{e}"))?))
        .map_err(|e| anyhow!("{}: {e}", args.project.display()))?;
    let stem = document.doc.meta.stem.clone();
    // The sky the install build gave the map is packed with it.
    let mut document = document;
    document.doc.meta.sky = rcomp::sky_vista(&document.doc);
    let wanted = assets::referenced(&document.doc);
    let stage = args.stage.clone().unwrap_or_else(|| default_stage(&args.project));
    let out = args.out.clone().unwrap_or_else(|| stage.clone());
    let build = crate::build::default_out(&args.project);
    let repak = crate::tools::tool(crate::tools::REPAK)?;

    let mut assets_out = match if wanted.is_empty() {
        json!({ "packed": false, "why": "the document names no material or model" })
    } else {
        pack_assets(&document, &wanted, &stage, &out, &repak, &build)?
    } {
        Value::Object(m) => m,
        other => bail!("the asset step answered with {other}"),
    };
    let maps = match (compiled(&build, &stem, "maps"), compiled(&build, &stem, "dedi/maps")) {
        (Some((client, ents)), Some((dedi, dedi_ents))) => {
            crate::build::progress(85, "packing the map");
            json!({
                "packed": true,
                "wraps": pack_wraps(&client, &ents, &stem, &stage, &out, &repak)?,
                "dedi": pack_dedi(&dedi, &dedi_ents, &stem, &out)?,
                "server_pak": pack_server_models(&dedi, &dedi_ents, &stem, &stage, &out, &repak)?,
            })
        }
        (Some((client, ents)), None) => {
            crate::build::progress(85, "packing the map");
            json!({
                "packed": true,
                "wraps": pack_wraps(&client, &ents, &stem, &stage, &out, &repak)?,
                "dedi": null,
            })
        }
        (None, _) => json!({ "packed": false, "why": "the project has no client map to wrap; run build first" }),
    };
    crate::build::progress(100, "done");
    assets_out.insert("maps".into(), maps);
    assets_out.insert("stem".into(), json!(stem));
    Ok(Value::Object(assets_out))
}

/// The compiled map of a project, with its entity partitions, when it has one.
fn compiled(build: &Path, stem: &str, sub: &str) -> Option<(Bsp, BTreeMap<String, Vec<u8>>)> {
    let dir = build.join(sub);
    let path = dir.join(format!("{stem}.bsp"));
    if !path.is_file() {
        return None;
    }
    let bsp = Bsp::read_path(&path).ok()?;
    let ents = Bsp::read_dir_map(&dir, stem).ok().map(|(_, e)| e).unwrap_or_default();
    Some((bsp, ents))
}

/// The document's own assets, packed into one archive.
fn pack_assets(
    document: &rdoc::Document,
    wanted: &assets::Referenced,
    stage: &Path,
    out: &Path,
    repak: &Path,
    build: &Path,
) -> Result<Value> {
    // The staging tree is not wiped: the asset tool writes an export into it and
    // an export that is already there is not fetched again, so a second run
    // over the same project does not re-export every material. The packer reads
    // only what the manifest names, so a file left over changes nothing.
    std::fs::create_dir_all(stage).with_context(|| format!("creating {}", stage.display()))?;
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let game_root = crate::tools::game_root()?;
    let rsx = crate::tools::tool(crate::tools::RSX)?;
    let cache = stage.join("cache");
    crate::build::progress(5, "reading the install");
    let stem = &document.doc.meta.stem;
    let install = Install::open(&game_root, &cache, &rsx, stem).map_err(|e| anyhow!("{e}"))?;
    crate::build::progress(6, "exporting from the install");
    prefetch(&install, wanted, &stage.join("export").join("batch"));

    let mut manifest = Manifest::new(stem, stage, out);
    let mut packed: BTreeMap<String, u64> = BTreeMap::new();
    let mut missing: Vec<Missing> = Vec::new();
    let mut textures: Vec<u64> = Vec::new();
    let mut models_packed: Vec<String> = Vec::new();
    let mut models_left: Vec<String> = Vec::new();
    let total = wanted.materials.len() + wanted.models.len();
    let mut done = 0usize;

    for name in &wanted.materials {
        crate::build::progress(percent(done, total), &format!("material {name}"));
        done += 1;
        match stage_material(&install, name, stage, &mut manifest, &mut textures, &mut missing) {
            Ok(n) => {
                packed.insert(format!("material {name}"), n);
            }
            Err(m) => missing.push(m),
        }
    }
    for name in &wanted.models {
        crate::build::progress(percent(done, total), &format!("model {name}"));
        done += 1;
        match stage_model(&install, name, stage, &mut manifest) {
            Ok((n, materials)) => {
                packed.insert(format!("model {name}"), n);
                models_packed.push(name.clone());
                // A model draws with materials of its own, which may live only
                // in the pak it came from.
                for m in materials {
                    let key = format!("material {m}");
                    if packed.contains_key(&key) || wanted.materials.contains(&m) {
                        continue;
                    }
                    match stage_material(&install, &m, stage, &mut manifest, &mut textures, &mut missing) {
                        Ok(n) => {
                            packed.insert(key, n);
                        }
                        Err(miss) => missing.push(miss),
                    }
                }
            }
            Err(m) => {
                models_left.push(name.clone());
                missing.push(m);
            }
        }
    }
    if let Some(path) = stage_cubemap(build, stem, stage, &mut manifest)? {
        packed.insert(format!("cubemap {path}"), 1);
    }
    if manifest.files.is_empty() {
        return Ok(json!({
            "packed": false,
            "why": format!(
                "{} of the {} assets the document names are not in the install",
                missing.len(),
                total
            ),
            "missing": missing.iter().map(|m| json!({"name": m.name, "kind": m.kind, "why": m.why})).collect::<Vec<_>>(),
        }));
    }
    let manifest_path = stage.join(format!("{stem}.json"));
    manifest.save(&manifest_path)?;
    crate::build::progress(80, "packing");
    let header = pak::pack(repak, &manifest, &manifest_path)?;
    let built = manifest.output();
    Ok(json!({
        "packed": true,
        "pak": built.display().to_string(),
        "bytes": std::fs::metadata(&built).map(|m| m.len()).unwrap_or(0),
        "flags": format!("0x{:04X}", header.flags),
        "assets": header.assets,
        "packaged": packed,
        "missing": missing.iter().map(|m| json!({"name": m.name, "kind": m.kind, "why": m.why})).collect::<Vec<_>>(),
        "models_packed": models_packed,
        "models_not_packed": models_left,
        "textures_not_packed": textures.iter().map(|g| format!("0x{g:016X}")).collect::<Vec<_>>(),
        "manifest": manifest_path.display().to_string(),
    }))
}

/// The two wrap archives a client map ships in.
fn pack_wraps(
    bsp: &Bsp,
    ents: &BTreeMap<String, Vec<u8>>,
    stem: &str,
    stage: &Path,
    out: &Path,
    repak: &Path,
) -> Result<Value> {
    let packed = wraps::build(bsp, ents, stem, &stage.join("wraps"), &out.join("paks").join("Win64"), repak)?;
    Ok(wraps::report(&packed))
}
/// The dedicated-server VPK a map ships in.
///
/// The workspace and the tree the packer writes into are both under the archive
/// directory, so the packer is handed two relative paths it cannot climb out of,
/// and the archives are moved up beside it once they are written.
fn pack_dedi(
    bsp: &Bsp,
    ents: &BTreeMap<String, Vec<u8>>,
    stem: &str,
    out: &Path,
) -> Result<Value> {
    let revpk = crate::tools::tool(crate::tools::REVPK)?;
    let root = out.join("vpk");
    let workspace = root.join("_work").join(stem);
    let packed = root.join("_work").join("packed");
    vpk::stage(bsp, ents, stem, &workspace)?;
    let built = vpk::pack(&workspace, &packed, stem, &revpk)?;
    for archive in [&built.dir, &built.data] {
        let target = root.join(archive.file_name().unwrap_or_default());
        std::fs::rename(archive, &target)
            .with_context(|| format!("moving {} to {}", archive.display(), target.display()))?;
    }
    let mut report = vpk::report(&Built {
        dir: root.join(vpk::dir_archive(stem)),
        data: root.join(vpk::data_archive(stem, 0)),
        ..built
    });
    report["packed_in"] = json!(vpk::data_archive(stem, 0));
    Ok(report)
}

/// The dedicated server's pak of the map, `paks/Win64_server/<map>.rpak`.
///
/// The server loads the model of every static prop and entity its map names
/// and stops the map on one it cannot find. A model a pak the server always
/// loads carries is left to that pak; any other is taken, as the server's own
/// copy, from the map pak of the install that ships it.
fn pack_server_models(
    bsp: &Bsp,
    ents: &BTreeMap<String, Vec<u8>>,
    stem: &str,
    stage: &Path,
    out: &Path,
    repak: &Path,
) -> Result<Value> {
    let models = server_models(bsp, ents);
    let pak_dir = out.join("paks").join("Win64_server");
    let built = pak_dir.join(format!("{stem}.rpak"));
    if models.is_empty() {
        let _ = std::fs::remove_file(&built);
        return Ok(json!({ "packed": false, "why": "the server map names no model" }));
    }
    let game_root = crate::tools::game_root()?;
    let rsx = crate::tools::tool(crate::tools::RSX)?;
    let install = Install::open(&game_root, &stage.join("cache"), &rsx, stem).map_err(|e| anyhow!("{e}"))?;
    let own = format!("{stem}.rpak");
    let assets_dir = stage.join("server");
    let mut files: Vec<Entry> = Vec::new();
    let mut resident: Vec<String> = Vec::new();
    let mut missing: Vec<Missing> = Vec::new();
    for name in &models {
        let miss = |why: &str| Missing { name: name.clone(), kind: "server model", why: why.to_string() };
        // A map's own earlier output is never a source.
        let rows: Vec<_> = install.server_model_rows(name).into_iter().filter(|(r, _)| r.pak != own).collect();
        if rows.iter().any(|(r, _)| !r.pak.starts_with("mp_rr_")) {
            resident.push(name.clone());
            continue;
        }
        let Some((row, pak)) = rows.first() else {
            missing.push(miss("no server pak in the install carries it"));
            continue;
        };
        let scratch = stage.join("export").join(format!("srv-{:016x}", row.guid));
        let home = match install.export_model_from(pak, row, &scratch) {
            Ok(h) => h,
            Err(e) => {
                missing.push(miss(&e.to_string()));
                continue;
            }
        };
        let base = assets::base_name(row.stem());
        let mut staged = false;
        for ext in SERVER_MODEL_FILES {
            let Ok(bytes) = std::fs::read(home.join(format!("{base}.{ext}"))) else { continue };
            let to = assets_dir.join(format!("{}.{ext}", without_extension(&row.path, "")));
            if let Some(d) = to.parent() {
                std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
            }
            std::fs::write(&to, &bytes).with_context(|| format!("writing {}", to.display()))?;
            staged |= ext == "rmdl";
        }
        if !staged {
            missing.push(miss("the export wrote no model"));
            continue;
        }
        files.push(Entry::pinned("mdl_", &pak::clean_path(&row.path).map_err(|e| anyhow!("{e}"))?, row.guid));
    }
    let report_missing = missing.iter().map(|m| json!({"name": m.name, "kind": m.kind, "why": m.why})).collect::<Vec<_>>();
    if files.is_empty() {
        let _ = std::fs::remove_file(&built);
        return Ok(json!({ "packed": false, "why": "every model is in a pak the server always loads", "resident": resident, "missing": report_missing }));
    }
    std::fs::create_dir_all(&pak_dir).with_context(|| format!("creating {}", pak_dir.display()))?;
    let manifest_path = stage.join(format!("{stem}_server.json"));
    let body = serde_json::to_string_pretty(&pak::server_manifest(stem, &assets_dir, &pak_dir, &files))?;
    std::fs::write(&manifest_path, body).with_context(|| format!("writing {}", manifest_path.display()))?;
    let header = pak::pack_server(repak, &manifest_path, &built)?;
    Ok(json!({
        "packed": true,
        "pak": built.display().to_string(),
        "assets": header.assets,
        "models": files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        "resident": resident,
        "missing": report_missing,
    }))
}

/// The files a server model is made of: the model and its physics.
const SERVER_MODEL_FILES: [&str; 2] = ["rmdl", "phy"];

/// Every model the server map names: its static props, and the `model` key of
/// its entities.
fn server_models(bsp: &Bsp, ents: &BTreeMap<String, Vec<u8>>) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let version = bsp.version_kind().unwrap_or(rbsp::Version::DediV47);
    if let Ok(lump) = rbsp::lumps::parse(rbsp::ids::GAME_LUMP, version, bsp.lump(rbsp::ids::GAME_LUMP)) {
        if let Some(block) = lump.as_static_props() {
            out.extend(block.models.iter().map(|m| m.to_ascii_lowercase()));
        }
    }
    let lump_ents = String::from_utf8_lossy(bsp.lump(rbsp::ids::ENTITIES)).into_owned();
    for text in ents.values().map(|b| String::from_utf8_lossy(b).into_owned()).chain(std::iter::once(lump_ents)) {
        for rest in text.split("\"model\"").skip(1) {
            let value = rest.trim_start().strip_prefix('"').and_then(|v| v.split('"').next()).unwrap_or("");
            if value.starts_with("mdl/") && value.ends_with(".rmdl") {
                out.insert(value.to_ascii_lowercase());
            }
        }
    }
    out
}

fn percent(done: usize, total: usize) -> u8 {
    (10 + (done * 70 / total.max(1)) as u8).min(80)
}

/// The material fields that name another material: its depth and collision
/// passes.
const PASS_MATERIALS: [&str; 5] =
    ["$depthShadowMaterial", "$depthPrepassMaterial", "$depthVSMMaterial", "$depthShadowTightMaterial", "$colpassMaterial"];
/// Fields that name an asset of a kind the packer cannot carry.
const UNPACKABLE_REFS: [(&str, &str); 1] = [("$textureAnimation", "texture animation")];

/// Exports a material and the textures it names, staging each at the path the
/// manifest entry opens. A material it names that only a map's own archive
/// carries is staged with it, since the client resolves every reference when
/// the archive loads.
fn stage_material(
    install: &Install,
    name: &str,
    stage: &Path,
    manifest: &mut Manifest,
    left: &mut Vec<u64>,
    missing: &mut Vec<Missing>,
) -> std::result::Result<u64, Missing> {
    let miss = |why: &str| Missing { name: name.to_string(), kind: "material", why: why.to_string() };
    let Some(row) = install.material_row(name) else {
        return Err(miss("the install carries no material under that name"));
    };
    if let Some(info) = install.material(name).map_err(|e| miss(&e.to_string()))? {
        let scratch = stage.join("export").join(format!("mat-{:016x}", row.guid));
        install.export_material(row, &scratch).map_err(|e| miss(&e.to_string()))?;
        let stem = without_extension(&row.path, "");
        let n = move_asset(&scratch, &stem, stage).map_err(|e| miss(&e.to_string()))? as u64;
        manifest
            .push(Entry::pinned("matl", &row.path, row.guid))
            .map_err(|e| miss(&e.to_string()))?;
        let mut n = n;
        let staged = |m: &Manifest, g: u64| {
            let tag = format!("0x{g:016X}");
            m.files.iter().any(|e| e.guid.as_deref() == Some(tag.as_str()))
        };
        for guid in info.textures.values() {
            if staged(manifest, *guid) {
                continue;
            }
            match stage_texture(install, *guid, stage, manifest) {
                Ok(()) => n += 1,
                Err(()) => left.push(*guid),
            }
        }
        let doc: serde_json::Value = std::fs::read_to_string(stage.join(format!("{stem}.json")))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let guid_of = |key: &str| {
            doc.get(key)
                .and_then(|v| v.as_str())
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok())
                .filter(|g| *g != 0)
        };
        for key in PASS_MATERIALS {
            let Some(g) = guid_of(key) else { continue };
            if staged(manifest, g) || install.is_core(g) {
                continue;
            }
            let Some(pass) = install.material_row_by_guid(g) else { continue };
            let pass_name = assets::normalize_material(&pass.path);
            match stage_material(install, &pass_name, stage, manifest, left, missing) {
                Ok(k) => n += k,
                Err(m) => missing.push(m),
            }
        }
        if let Some(g) = guid_of("shaderSet") {
            match stage_shader_set(install, g, stage, manifest) {
                Ok(k) => n += k,
                Err(why) => missing.push(Missing { name: format!("{name} shaderSet 0x{g:016X}"), kind: "shader set", why }),
            }
        }
        for (key, kind) in UNPACKABLE_REFS {
            let Some(g) = guid_of(key) else { continue };
            if !install.is_resident(g) {
                missing.push(Missing {
                    name: format!("{name} {key} 0x{g:016X}"),
                    kind,
                    why: format!("only {} carry it, and the packer cannot carry a {kind}", install.paks_of(g).join(", ")),
                });
            }
        }
        return Ok(n);
    }
    Err(miss("the install carries no material under that name"))
}

/// Exports everything the map will stage with one run of the asset tool per
/// pak and kind: the materials the document names, the models and the
/// materials they draw with, the pass materials those name, and then every
/// texture, shader set and shader of the lot. Staging copies from the batch;
/// an asset the batch missed is still exported on its own.
fn prefetch(install: &Install, wanted: &assets::Referenced, root: &Path) {
    let hex = |v: &serde_json::Value| {
        v.as_str()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok())
            .filter(|g| *g != 0)
    };
    let models: Vec<&AssetRow> = wanted.models.iter().filter_map(|m| install.model_row(m)).collect();
    install.prefetch("mdl_", &models, root);
    let mut materials: Vec<&AssetRow> = wanted.materials.iter().filter_map(|m| install.material_row(m)).collect();
    for row in &models {
        let scratch = root.join("probe").join(format!("{:016x}", row.guid));
        if let Ok(dir) = install.export_model(row, &scratch) {
            if let Ok(bytes) = std::fs::read(dir.join(format!("{}.rmdl", assets::base_name(row.stem())))) {
                materials.extend(install.materials_named_by(&bytes));
            }
        }
    }
    let mut seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut textures: Vec<u64> = Vec::new();
    let mut sets: Vec<u64> = Vec::new();
    while !materials.is_empty() {
        install.prefetch("matl", &materials, root);
        let mut next: Vec<&AssetRow> = Vec::new();
        for row in materials.drain(..) {
            if !seen.insert(row.guid) {
                continue;
            }
            let scratch = root.join("probe").join(format!("{:016x}", row.guid));
            let Ok(json) = install.export_material(row, &scratch) else { continue };
            let Ok(text) = std::fs::read_to_string(&json) else { continue };
            let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
            if let Some(map) = doc.get("$textures").and_then(|t| t.as_object()) {
                for v in map.values() {
                    if let Some(g) = v.as_str().and_then(|s| install.texture_guid(s)) {
                        textures.push(g);
                    }
                }
            }
            for key in PASS_MATERIALS {
                if let Some(g) = doc.get(key).and_then(hex) {
                    if !install.is_core(g) && !seen.contains(&g) {
                        if let Some(r) = install.material_row_by_guid(g) {
                            next.push(r);
                        }
                    }
                }
            }
            if let Some(g) = doc.get("shaderSet").and_then(hex) {
                if !install.is_core(g) {
                    sets.push(g);
                }
            }
        }
        materials = next;
    }
    let texture_rows: Vec<&AssetRow> = textures.iter().filter_map(|g| install.texture_row(*g)).collect();
    install.prefetch("txtr", &texture_rows, root);
    sets.sort_unstable();
    sets.dedup();
    let set_rows: Vec<&AssetRow> = sets.iter().filter_map(|g| install.row_of("shds", *g)).collect();
    install.prefetch("shds", &set_rows, root);
    let mut shaders: Vec<u64> = Vec::new();
    for row in &set_rows {
        let scratch = root.join("probe").join(format!("{:016x}", row.guid));
        if install.export_asset("shds", row.guid, &scratch).is_err() {
            continue;
        }
        let Ok(bytes) = std::fs::read(scratch.join(format!("shaderset/0x{:X}.msw", row.guid))) else { continue };
        for at in [5usize, 13] {
            if let Some(b) = bytes.get(at..at + 8) {
                let g = u64::from_le_bytes(b.try_into().unwrap());
                if g != 0 && !install.is_core(g) {
                    shaders.push(g);
                }
            }
        }
    }
    shaders.sort_unstable();
    shaders.dedup();
    let shader_rows: Vec<&AssetRow> = shaders.iter().filter_map(|g| install.row_of("shdr", *g)).collect();
    install.prefetch("shdr", &shader_rows, root);
}

/// Stages a shader set and the two shaders it names, unless a pak the client
/// always loads carries them. Map materials draw with sets that only other
/// maps' archives hold, so the map's own archive has to carry them.
fn stage_shader_set(install: &Install, guid: u64, stage: &Path, manifest: &mut Manifest) -> std::result::Result<u64, String> {
    let tag = |g: u64| format!("0x{g:016X}");
    let staged = |m: &Manifest, g: u64| m.files.iter().any(|e| e.guid.as_deref() == Some(tag(g).as_str()));
    if install.is_core(guid) || staged(manifest, guid) {
        return Ok(0);
    }
    let scratch = stage.join("export").join(format!("shds-{guid:016x}"));
    install.export_asset("shds", guid, &scratch).map_err(|e| e.to_string())?;
    let set_rel = format!("shaderset/{}", tag(guid));
    // The tool names exported files by the unpadded guid.
    let set_file = scratch.join(format!("shaderset/0x{guid:X}.msw"));
    let bytes = std::fs::read(&set_file).map_err(|e| format!("{}: {e}", set_file.display()))?;
    if bytes.len() < 21 {
        return Err(format!("{} is {} bytes, too short for its shader references", set_file.display(), bytes.len()));
    }
    let mut n = 0;
    for at in [5usize, 13] {
        let shader = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        if shader == 0 || install.is_core(shader) || staged(manifest, shader) {
            continue;
        }
        n += stage_shader(install, shader, stage, manifest)?;
    }
    let to = stage.join(format!("{set_rel}.msw"));
    std::fs::create_dir_all(to.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&to, &bytes).map_err(|e| e.to_string())?;
    manifest.push(Entry::pinned("shds", &set_rel, guid)).map_err(|e| e.to_string())?;
    Ok(n + 1)
}

/// Stages one shader. A child shader has no bytecode to export; it is written
/// as an entry naming its parent, which is staged beside it.
fn stage_shader(install: &Install, shader: u64, stage: &Path, manifest: &mut Manifest) -> std::result::Result<u64, String> {
    let tag = |g: u64| format!("0x{g:016X}");
    let staged = |m: &Manifest, g: u64| m.files.iter().any(|e| e.guid.as_deref() == Some(tag(g).as_str()));
    if install.is_core(shader) || staged(manifest, shader) {
        return Ok(0);
    }
    let dir = stage.join("export").join(format!("shdr-{shader:016x}"));
    let rel = format!("shader/{}", tag(shader));
    let from = dir.join(format!("shader/0x{shader:X}.msw"));
    let _ = install.export_asset("shdr", shader, &dir);
    // A child shader exports nothing from any pak, so its parent is looked up
    // before trying the other paks one by one.
    let parent = if from.is_file() { None } else { install.shader_parent(shader) };
    if parent.is_none() {
        // A shader some paks only reference exports empty from them; the one
        // that holds its bytecode writes the file.
        for pak in install.paks_carrying("shdr", shader) {
            if from.is_file() {
                break;
            }
            let _ = install.export_asset_from(&pak, "shdr", shader, &dir);
        }
    }
    if from.is_file() {
        let to = stage.join(format!("{rel}.msw"));
        std::fs::create_dir_all(to.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        manifest.push(Entry::pinned("shdr", &rel, shader)).map_err(|e| e.to_string())?;
        return Ok(1);
    }
    let parent = parent
        .ok_or_else(|| format!("shader {} exported nothing and its pak names no parent for it", tag(shader)))?;
    let n = stage_shader(install, parent, stage, manifest)?;
    let mut entry = Entry::pinned("shdr", &rel, shader);
    entry.extra.insert("$parentShader".into(), serde_json::json!(parent));
    manifest.push(entry).map_err(|e| e.to_string())?;
    Ok(n + 1)
}

/// The files a model is staged with, beside the model itself. A `.vg` makes the
/// packer write that model's streamed vertex groups into the map's starpak.
const MODEL_FILES: [&str; 4] = ["rmdl", "phy", "vg_static", "vg"];

/// Whether a model's export carries what the client reads its geometry from.
/// The client issues a streamed vertex read for every model, static props with a
/// permanent cache included, and a model without streamed vertex groups fails
/// that read and takes the client down, so the `.vg` is required.
pub fn model_vertex_data_ships(home: &Path, base: &str) -> bool {
    home.join(format!("{base}.rmdl")).is_file() && home.join(format!("{base}.vg")).is_file()
}

/// Stages a model at the path the install stores it under, with the files the
/// packer reads beside it.
fn stage_model(
    install: &Install,
    name: &str,
    stage: &Path,
    manifest: &mut Manifest,
) -> std::result::Result<(u64, Vec<String>), Missing> {
    let miss = |why: &str| Missing { name: name.to_string(), kind: "model", why: why.to_string() };
    let Some(row) = install.model_row(name) else {
        return Err(miss("the install carries no model under that path"));
    };
    let scratch = stage.join("export").join(format!("mdl-{:016x}", row.guid));
    let mut home = install.export_model(row, &scratch).map_err(|e| miss(&e.to_string()))?;
    let base = assets::base_name(row.stem());
    if !model_vertex_data_ships(&home, base) {
        // The streamed groups come from the pak's starpaks, and an install can lack
        // some (the optional ones); another pak carrying the model may have them.
        let first = install.pak_of(row);
        for (i, pak) in install.paks_carrying("mdl_", row.guid).into_iter().enumerate() {
            if Some(&pak) == first.as_ref() {
                continue;
            }
            let alt = stage.join("export").join(format!("mdl-{:016x}-{i}", row.guid));
            if let Ok(h) = install.export_model_from(&pak, row, &alt) {
                if model_vertex_data_ships(&h, base) {
                    home = h;
                    break;
                }
            }
        }
    }
    if !model_vertex_data_ships(&home, base) {
        return Err(miss("its streamed vertex groups were not exported, and the client streams every model's"));
    }
    let mut n = 0u64;
    let mut materials: Vec<String> = Vec::new();
    for ext in MODEL_FILES {
        let Ok(bytes) = std::fs::read(home.join(format!("{base}.{ext}"))) else {
            continue;
        };
        if ext == "rmdl" {
            materials = install.materials_named_by(&bytes).iter().map(|r| assets::normalize_material(&r.path)).collect();
        }
        let to = stage.join(format!("{}.{ext}", without_extension(&row.path, "")));
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d).map_err(|e| miss(&e.to_string()))?;
        }
        std::fs::write(&to, &bytes).map_err(|e| miss(&e.to_string()))?;
        if ext == "vg" {
            manifest.stream_from_own_starpak();
        }
        n += 1;
    }
    manifest
        .push(Entry::pinned("mdl_", &row.path, row.guid))
        .map_err(|e| miss(&e.to_string()))?;
    Ok((n, materials))
}

/// The map's cubemap array, which the engine looks up in the map's pak under
/// `texture/maps/<map>/cubemaps_hdr`. The metadata is the shipped maps': type
/// 18, every mip permanent, and the cube bit in the resource flags.
fn stage_cubemap(build: &Path, stem: &str, stage: &Path, manifest: &mut Manifest) -> Result<Option<String>> {
    let dds_path = crate::build::cubemap_path(&build.join("maps").join(format!("{stem}.bsp")), stem);
    let Ok(dds) = std::fs::read(&dds_path) else { return Ok(None) };
    if dds.len() < 148 || &dds[0..4] != b"DDS " {
        bail!("{} is not a DDS", dds_path.display());
    }
    let mips = u32::from_le_bytes([dds[28], dds[29], dds[30], dds[31]]).clamp(1, 255) as u8;
    let asset = format!("texture/maps/{stem}/cubemaps_hdr.rpak");
    let stem_path = without_extension(&asset, "");
    let target = stage.join(format!("{stem_path}.dds"));
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&target, &dds).with_context(|| format!("writing {}", target.display()))?;
    let mut tail = [0u8; 36];
    tail[4] = mips;
    tail[9] = CUBEMAP_TEXTURE_TYPE;
    let meta = json!({
        "streamLayout": vec!["permanent"; (mips as usize).saturating_sub(1).max(1)],
        "hdrTail": tail.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        "resourceFlags": "0x2",
        "usageFlags": "0x0",
    });
    std::fs::write(stage.join(format!("{stem_path}.json")), serde_json::to_vec_pretty(&meta)?)
        .with_context(|| format!("writing the metadata of {asset}"))?;
    let guid = pak::string_to_guid(&asset).map_err(|e| anyhow!("{e}"))?;
    // The v10 texture writer reads the cube bit and the type from the manifest
    // entry, not the metadata document; without them the engine binds the default cubemap.
    let mut entry = Entry::pinned("txtr", &asset, guid);
    entry.extra.insert("$layerCount".into(), json!(CUBEMAP_LAYER_FLAGS));
    entry.extra.insert("$hdrTail".into(), json!(tail.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    manifest.push(entry).map_err(|e| anyhow!("{e}"))?;
    Ok(Some(asset))
}

/// The texture type the shipped maps' cubemap arrays carry.
const CUBEMAP_TEXTURE_TYPE: u8 = 0x12;
/// Header byte 0x11 of the shipped maps' cubemap arrays (0x2 = cube).
const CUBEMAP_LAYER_FLAGS: u8 = 0x6;

/// Exports a texture as the metadata document and its whole mip chain, which is
/// what the packer reads.
fn stage_texture(install: &Install, guid: u64, stage: &Path, manifest: &mut Manifest) -> std::result::Result<(), ()> {
    let Some(row) = install.texture_row(guid) else {
        return Err(());
    };
    let scratch = stage.join("export").join(format!("txr-{guid:016x}"));
    install.export_texture(row, &scratch).map_err(|_| ())?;
    let leaf = row.file_name();
    let base = leaf.rsplit_once('.').map(|(a, _)| a).unwrap_or(leaf);
    let from = assets::export_dir(&scratch, row.stem());
    let stem_path = without_extension(&row.path, "");
    let mip = mip_chain(&from, base, ".dds").or_else(|| highest_mip(&from, base, ".dds")).ok_or(())?;
    std::fs::create_dir_all(stage.join(without_extension(&row.path, "")).parent().unwrap_or(stage)).map_err(|_| ())?;
    std::fs::write(stage.join(format!("{stem_path}.dds")), mip).map_err(|_| ())?;
    if let Ok(json) = std::fs::read(from.join(format!("{base}.json"))) {
        std::fs::write(stage.join(format!("{stem_path}.json")), json).map_err(|_| ())?;
    }
    manifest.push(Entry::pinned("txtr", &row.path, row.guid)).map_err(|e| anyhow!("{e}")).map_err(|_| ())?;
    Ok(())
}

/// Replaces the extension of an asset path, keeping the directory.
fn without_extension(path: &str, ext: &str) -> String {
    match path.rsplit_once('.') {
        Some((head, _)) if !head.is_empty() => format!("{head}{ext}"),
        _ => format!("{path}{ext}"),
    }
}

/// Copies every file an export wrote for one asset, which are the ones that
/// share the asset's own name, from the export tree into the staging tree the
/// manifest names. Returns how many files moved.
fn move_asset(from: &Path, stem: &str, stage: &Path) -> Result<usize> {
    let dir = from.join(stem).parent().map(Path::to_path_buf).unwrap_or_else(|| from.to_path_buf());
    let base = stem.rsplit('/').next().unwrap_or(stem);
    let mut n = 0usize;
    let entries = std::fs::read_dir(&dir).map_err(|e| anyhow!("{}: {e}", dir.display()))?;
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(tail) = name.strip_prefix(&format!("{base}.")) else {
            continue;
        };
        let target = stage.join(format!("{stem}.{tail}"));
        if let Some(d) = target.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::copy(&p, &target).with_context(|| format!("staging {}", target.display()))?;
        n += 1;
    }
    if n == 0 {
        bail!("{} holds no file of {stem}", dir.display());
    }
    Ok(n)
}

/// Every mip the export wrote, one file per level numbered from the smallest,
/// joined into one DDS largest first. A texture shipped with only its top level
/// has no mips at all in game and aliases into noise at range. None when a level
/// is missing or does not halve the one above it.
fn mip_chain(dir: &Path, base: &str, ext: &str) -> Option<Vec<u8>> {
    let mut levels: BTreeMap<u32, PathBuf> = BTreeMap::new();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(n) = name
            .strip_prefix(&format!("{base}_level"))
            .and_then(|r| r.strip_suffix(ext))
            .and_then(|d| d.parse::<u32>().ok())
        else {
            continue;
        };
        levels.insert(n, p);
    }
    let top = *levels.keys().next_back()?;
    if levels.len() as u32 != top + 1 {
        return None;
    }
    let head_bytes = std::fs::read(&levels[&top]).ok()?;
    let head = rpak::dds::parse_dds_header(&head_bytes).ok()?;
    if head.mip_count != 1 || head.array_size != 1 || head.depth != 1 {
        return None;
    }
    let mut out = head_bytes[..head.data_offset as usize].to_vec();
    for n in (0..=top).rev() {
        let b = if n == top { head_bytes.clone() } else { std::fs::read(&levels[&n]).ok()? };
        let info = rpak::dds::parse_dds_header(&b).ok()?;
        let shift = top - n;
        if info.width != (head.width >> shift).max(1) || info.height != (head.height >> shift).max(1) || info.format != head.format {
            return None;
        }
        out.extend_from_slice(b.get(info.data_offset as usize..)?);
    }
    const DDSD_MIPMAPCOUNT: u32 = 0x2_0000;
    const DDSCAPS_COMPLEX_MIPMAP: u32 = 0x40_0008;
    let or_at = |out: &mut Vec<u8>, at: usize, v: u32| {
        let cur = u32::from_le_bytes(out[at..at + 4].try_into().unwrap());
        out[at..at + 4].copy_from_slice(&(cur | v).to_le_bytes());
    };
    out[28..32].copy_from_slice(&(top + 1).to_le_bytes());
    or_at(&mut out, 8, DDSD_MIPMAPCOUNT);
    or_at(&mut out, 108, DDSCAPS_COMPLEX_MIPMAP);
    Some(out)
}

/// The highest-numbered mip the export wrote, which is the largest image. A
/// file that is not a mip is skipped rather than ending the scan, because the
/// metadata document sits in the same directory and the order is not fixed.
fn highest_mip(dir: &Path, base: &str, ext: &str) -> Option<Vec<u8>> {
    let mut best: Option<(u32, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(rest) = name.strip_prefix(&format!("{base}_level")) else {
            continue;
        };
        let Some(digits) = rest.strip_suffix(ext) else {
            continue;
        };
        let Ok(n) = digits.parse::<u32>() else {
            continue;
        };
        if best.as_ref().is_none_or(|(b, _)| n > *b) {
            best = Some((n, p));
        }
    }
    std::fs::read(best?.1).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_asset_path_keeps_its_directory_when_the_extension_changes() {
        assert_eq!(without_extension("material/world/dev/x.rpak", ".json"), "material/world/dev/x.json");
        assert_eq!(without_extension("mdl/props/crate.rmdl", ""), "mdl/props/crate");
        assert_eq!(without_extension("texture/0xABC", ""), "texture/0xABC");
        assert_eq!(without_extension("nodots", ".json"), "nodots.json");
        assert_eq!(without_extension(".hidden", ".json"), ".hidden.json");
    }

    #[test]
    fn a_moved_export_lands_at_the_manifest_path() {
        let dir = std::env::temp_dir().join(format!("r5ed-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let from = dir.join("export").join("material").join("world").join("dev");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("x.json"), b"{}").unwrap();
        std::fs::write(from.join("x.uber"), b"uber").unwrap();
        std::fs::write(from.join("other.json"), b"no").unwrap();
        let stage = dir.join("stage");
        let n = move_asset(&dir.join("export"), "material/world/dev/x", &stage).unwrap();
        assert_eq!(n, 2, "only the files of this asset move");
        assert!(stage.join("material/world/dev/x.json").is_file());
        assert!(stage.join("material/world/dev/x.uber").is_file());
        assert!(!stage.join("material/world/dev/other.json").exists());
        assert!(move_asset(&dir.join("elsewhere"), "material/world/dev/x", &stage).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn one_level_dds(w: u32, h: u32, fill: u8) -> Vec<u8> {
        let mut b = vec![0u8; 148];
        b[0..4].copy_from_slice(b"DDS ");
        b[4..8].copy_from_slice(&124u32.to_le_bytes());
        b[12..16].copy_from_slice(&h.to_le_bytes());
        b[16..20].copy_from_slice(&w.to_le_bytes());
        b[28..32].copy_from_slice(&1u32.to_le_bytes());
        b[80..84].copy_from_slice(&4u32.to_le_bytes());
        b[84..88].copy_from_slice(b"DX10");
        b[128..132].copy_from_slice(&98u32.to_le_bytes());
        b[132..136].copy_from_slice(&3u32.to_le_bytes());
        b[140..144].copy_from_slice(&1u32.to_le_bytes());
        let blocks = (w.div_ceil(4) * h.div_ceil(4)) as usize;
        b.extend(std::iter::repeat_n(fill, blocks * 16));
        b
    }

    #[test]
    fn the_mip_chain_is_joined_largest_first() {
        let dir = std::env::temp_dir().join(format!("r5ed-chain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (n, w) in [(0u32, 1u32), (1, 2), (2, 4), (3, 8)] {
            std::fs::write(dir.join(format!("0xAB_level{n}.dds")), one_level_dds(w, w, n as u8 + 1)).unwrap();
        }
        let out = mip_chain(&dir, "0xAB", ".dds").unwrap();
        let info = rpak::dds::parse_dds_header(&out).unwrap();
        assert_eq!((info.width, info.mip_count), (8, 4));
        let body = &out[148..];
        assert_eq!(body.len(), (4 + 1 + 1 + 1) * 16);
        assert!(body[..64].iter().all(|b| *b == 4), "the top level comes first");
        assert!(body[64..].chunks(16).map(|c| c[0]).eq([3u8, 2, 1]));
        std::fs::remove_file(dir.join("0xAB_level1.dds")).unwrap();
        assert!(mip_chain(&dir, "0xAB", ".dds").is_none(), "a gap is not a chain");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_highest_mip_is_the_one_that_is_kept() {
        let dir = std::env::temp_dir().join(format!("r5ed-mip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x_level0.dds"), b"small").unwrap();
        std::fs::write(dir.join("x_level3.dds"), b"big").unwrap();
        std::fs::write(dir.join("x_level1.dds"), b"mid").unwrap();
        assert_eq!(highest_mip(&dir, "x", ".dds").unwrap(), b"big");
        assert!(highest_mip(&dir, "nope", ".dds").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The metadata document a real texture export writes sits in the same
    /// directory as the mips, so the scan has to step over it rather than let it
    /// end the search, whatever order the directory hands the names back in.
    #[test]
    fn a_mip_scan_steps_over_the_files_that_are_not_mips() {
        let dir = std::env::temp_dir().join(format!("r5ed-mip2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("0xABC.json"), b"{}").unwrap();
        std::fs::write(dir.join("0xABC_level0.dds"), b"small").unwrap();
        std::fs::write(dir.join("0xABC_level7.dds"), b"big").unwrap();
        std::fs::write(dir.join("0xABC_levelX.dds"), b"not a number").unwrap();
        std::fs::write(dir.join("0xABC_extra.dds"), b"no level").unwrap();
        assert_eq!(highest_mip(&dir, "0xABC", ".dds").unwrap(), b"big");
        // A name that is a prefix of another is not read as one of its mips.
        assert!(highest_mip(&dir, "0xAB", ".dds").is_none());
        assert!(highest_mip(&dir.join("nowhere"), "0xABC", ".dds").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The client streams every model's vertex groups, so a model packs only with
    /// its `.vg`; the shape that crashed a client (one LOD group, permanent cache
    /// only) is refused.
    #[test]
    fn only_a_model_with_streamed_vertex_groups_packs() {
        let dir = std::env::temp_dir().join(format!("r5ed-mdl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!model_vertex_data_ships(&dir, "prop_flask"), "no export at all");
        std::fs::write(dir.join("prop_flask.rmdl"), b"rmdl").unwrap();
        assert!(!model_vertex_data_ships(&dir, "prop_flask"), "no vertex data");
        std::fs::write(dir.join("prop_flask.vg_static"), b"cache").unwrap();
        assert!(!model_vertex_data_ships(&dir, "prop_flask"), "permanent cache only");
        std::fs::write(dir.join("prop_flask.vg"), b"stream").unwrap();
        assert!(model_vertex_data_ships(&dir, "prop_flask"));
        assert!(!model_vertex_data_ships(&dir, "other"), "a different model");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_staging_directory_follows_the_project() {
        assert_eq!(default_stage(Path::new("p")), PathBuf::from("p/build/package"));
    }
}
