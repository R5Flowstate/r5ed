//! `build`: a project directory becomes the client and dedicated-server maps.
//!
//! The compiler is asked for both targets in one call and hands back a container
//! plus the partition files beside it, so the only decisions here are where the
//! bytes land and what the caller is told about them.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rbake::BakeMode;
use rbsp::{Bsp, Layout};
use rcomp::{CompileOptions, MapArtifacts, Targets};
use rdoc::project::{self, NativeProject};
use rdoc::Document;
use serde_json::{json, Map, Value};

/// Everything `build` takes, already parsed from the command line.
#[derive(Clone, Debug)]
pub struct Args {
    pub project: PathBuf,
    pub out: Option<PathBuf>,
    pub targets: String,
    pub client: Option<PathBuf>,
    pub dedi: Option<PathBuf>,
    pub ent_dir: Option<PathBuf>,
    pub bake: String,
    /// Asset tables to compile against, merged in order (a later file's entry
    /// wins), and the document's names found in them.
    pub assets: Vec<PathBuf>,
    pub install: bool,
    /// Where to write the asset table the compile ran against, so a machine
    /// without the install can repeat the build with `--assets` and `--install`.
    pub write_assets: Option<PathBuf>,
    /// With `--write-assets` and `--install`: write every asset of these kinds
    /// (`materials`, `models`) the install carries instead of the map's own.
    pub all_assets: Vec<String>,
    /// Lets a final bake from `--assets` run on defaults for names the tables
    /// do not carry instead of failing.
    pub allow_unresolved: bool,
    /// Source pak manifests (`<stem>_src.json`) whose materials the document's
    /// names also resolve against, read from disk without the asset tool.
    pub src_paks: Vec<PathBuf>,
    /// Compile and bake even when the output directory holds a build of the
    /// same inputs.
    pub rebuild: bool,
}

/// The record a build leaves beside its outputs: what went in and what it
/// reported. A later build of the same inputs into the same directory reuses the
/// outputs instead of compiling and baking again, so a map baked on one machine
/// ships from another with its `build/` copied over.
pub const BUILD_RECORD: &str = "build.json";
/// Bumped when the record or the input digest changes meaning.
const BUILD_RECORD_VERSION: u32 = 1;

/// A digest of everything a compile reads: the compiler's version, the bake
/// mode and targets, the document (re-serialized, so formatting and key order
/// do not count), every file it names (mesh sources, terrain heights, the
/// project's entity schema), and the asset table entries it resolves. Built from
/// `--install` on one machine and from that install's `--write-assets` table on
/// another, the same project digests the same.
pub fn inputs_digest(args: &Args, document: &Document, tables: &rir::AssetTables) -> Result<String> {
    let mut h = Fnv::new();
    h.part(b"r5ed-build");
    h.part(env!("CARGO_PKG_VERSION").as_bytes());
    h.part(args.bake.trim().to_ascii_lowercase().as_bytes());
    let wanted = targets(&args.targets)?;
    h.part(&[u8::from(wanted.client), u8::from(wanted.dedi)]);
    h.part(serde_json::to_vec(&serde_json::to_value(&document.doc)?)?.as_slice());
    h.part(std::env::var(crate::propmesh::PROP_MODELS_ENV).unwrap_or_default().as_bytes());
    let mut files = std::collections::BTreeSet::new();
    for o in &document.doc.objects {
        match &o.kind {
            rdoc::model::ObjectKind::Mesh(m) => {
                files.insert(m.source.clone());
            }
            rdoc::model::ObjectKind::Terrain(t) => {
                files.insert(t.heights.clone());
            }
            _ => {}
        }
    }
    files.insert("schema/entities.json".to_string());
    for f in &files {
        h.part(f.as_bytes());
        match document.files.read(f) {
            Ok(bytes) => h.part(&bytes),
            Err(_) => h.part(b"<absent>"),
        }
    }
    let resolved = crate::assets::resolve_tables(tables, &document.doc);
    let used: std::collections::BTreeSet<&str> = resolved.materials.values().map(|m| m.surface_prop.as_str()).collect();
    let props: std::collections::BTreeMap<&String, &u32> =
        tables.surface_props.iter().filter(|(k, _)| used.contains(k.as_str())).collect();
    h.part(serde_json::to_vec(&serde_json::to_value(&resolved.materials)?)?.as_slice());
    h.part(serde_json::to_vec(&serde_json::to_value(&resolved.models)?)?.as_slice());
    h.part(serde_json::to_vec(&serde_json::to_value(&props)?)?.as_slice());
    Ok(h.hex())
}

/// 64-bit FNV-1a over length-prefixed parts, so no two part lists collide by
/// concatenation.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for x in b {
            self.0 ^= u64::from(*x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn part(&mut self, b: &[u8]) {
        self.bytes(&(b.len() as u64).to_le_bytes());
        self.bytes(b);
    }
    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// The report of an earlier build of the same inputs into `base`, if its record
/// matches and every file it wrote is still there.
fn reusable(base: &Path, inputs: &str) -> Option<Value> {
    let text = std::fs::read_to_string(base.join(BUILD_RECORD)).ok()?;
    let record: Value = serde_json::from_str(&text).ok()?;
    if record["version"].as_u64() != Some(u64::from(BUILD_RECORD_VERSION)) || record["inputs"].as_str() != Some(inputs) {
        return None;
    }
    let report = record.get("report")?.clone();
    let targets = report["targets"].as_object()?;
    if targets.is_empty() {
        return None;
    }
    for t in targets.values() {
        for f in t["files"].as_array()? {
            if !base.join(f.as_str()?).is_file() {
                return None;
            }
        }
    }
    Some(report)
}

/// Reads the project's files for the compiler, which asks through a trait so the
/// same call works from the editor and from here.
struct DocFs<'a>(&'a Document);

impl rcomp::ProjectFs for DocFs<'_> {
    fn read(&self, rel: &str) -> Result<Vec<u8>> {
        self.0.files.read(rel).map_err(|e| anyhow!("{rel}: {e}"))
    }
}

/// Writes one line of progress. The result document goes to stdout, so progress
/// belongs on stderr where a log reader sees it without parsing the answer.
pub fn progress(percent: u8, message: &str) {
    eprintln!("PROGRESS {percent} {message}");
}

/// Parses a target list, the spelling `client,dedi` in any order.
pub fn targets(list: &str) -> Result<Targets> {
    let mut out = Targets { client: false, dedi: false };
    for part in list.split(',').map(str::trim) {
        match part {
            "client" => out.client = true,
            "dedi" => out.dedi = true,
            other => bail!("--targets takes client and dedi, not {other:?}"),
        }
    }
    if !out.client && !out.dedi {
        bail!("--targets needs at least one of client, dedi");
    }
    Ok(out)
}

/// Parses a bake mode by name.
pub fn bake_mode(name: &str) -> Result<BakeMode> {
    match name {
        "none" => Ok(BakeMode::None),
        "preview" => Ok(BakeMode::Preview),
        "final" => Ok(BakeMode::Final),
        other => bail!("--bake takes none, preview or final, not {other:?}"),
    }
}

/// The build tree a project writes into by default.
pub fn default_out(project: &Path) -> PathBuf {
    project.join("build")
}

/// The client's map and partition paths, and the dedicated-server pair.
pub struct Outputs {
    pub client: PathBuf,
    pub client_ent_dir: PathBuf,
    pub dedi: PathBuf,
}

impl Outputs {
    pub fn resolve(args: &Args, stem: &str) -> Result<Self> {
        let out = args.out.clone().unwrap_or_else(|| default_out(&args.project));
        let client = match &args.client {
            Some(p) => p.clone(),
            None => out.join("maps").join(format!("{stem}.bsp")),
        };
        let dedi = match &args.dedi {
            Some(p) => p.clone(),
            None => out.join("dedi").join("maps").join(format!("{stem}.bsp")),
        };
        for (label, path) in [("client", &client), ("dedi", &dedi)] {
            if path.extension().and_then(|e| e.to_str()) != Some("bsp") {
                bail!("the {label} map path has to end in .bsp, not {}", path.display());
            }
        }
        let client_ent_dir = args.ent_dir.clone().unwrap_or_else(|| {
            client.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
        });
        Ok(Self { client, client_ent_dir, dedi })
    }
}

/// Writes a map and its partition files, returning every path written.
/// Where a build keeps the client map's cubemap texture, beside the map.
pub fn cubemap_path(map: &Path, stem: &str) -> PathBuf {
    map.with_file_name(format!("{stem}.cubemaps_hdr.dds"))
}

/// Writes the cubemap texture beside the map, or removes one a previous build
/// left when this build has none.
fn write_cubemap(tex: Option<&rbake::CubemapTexture>, map: &Path, stem: &str) -> Result<Option<PathBuf>> {
    let path = cubemap_path(map, stem);
    match tex {
        Some(t) => {
            std::fs::write(&path, &t.dds).with_context(|| format!("writing {}", path.display()))?;
            Ok(Some(path))
        }
        None => {
            let _ = std::fs::remove_file(&path);
            Ok(None)
        }
    }
}

pub fn write_map(bsp: &Bsp, ent_bytes: &std::collections::BTreeMap<String, Vec<u8>>, map: &Path, ent_dir: &Path, stem: &str) -> Result<Vec<PathBuf>> {
    let dir = map.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::create_dir_all(ent_dir).with_context(|| format!("creating {}", ent_dir.display()))?;
    bsp.write_path(map).map_err(|e| anyhow!("{}: {e}", map.display()))?;
    let mut out = vec![map.to_path_buf()];
    if bsp.layout == Layout::Sidecar {
        let name = map.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        for id in bsp.lumps.keys() {
            out.push(dir.join(Bsp::sidecar_name(&name, *id)));
        }
    }
    for (name, bytes) in ent_bytes {
        // The engine looks a partition up by the map stem and the partition
        // name, so that is the file name it is written under.
        let file = format!("{stem}_{name}.ent");
        let path = ent_dir.join(&file);
        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
        out.push(path);
    }
    Ok(out)
}

/// The counts a written map itself reports: triangles from its mesh table,
/// collision models from its model table, props from its game lump.
fn counts(bsp: &Bsp) -> (usize, usize, usize) {
    use rbsp::lumps::{parse, Lump};
    let version = bsp.version_kind().unwrap_or(rbsp::Version::ClientV51);
    let triangles = match parse(rbsp::ids::MESHES, version, bsp.lump(rbsp::ids::MESHES)) {
        Ok(Lump::Mesh(m)) => m.iter().map(|x| x.triangle_count as usize).sum(),
        _ => 0,
    };
    let solids = match parse(rbsp::ids::MODELS, version, bsp.lump(rbsp::ids::MODELS)) {
        Ok(Lump::Model(m)) => m.iter().filter(|x| x.bvh_node >= 0 || x.bvh_leaf >= 0).count(),
        _ => 0,
    };
    let props = match parse(rbsp::ids::GAME_LUMP, version, bsp.lump(rbsp::ids::GAME_LUMP)) {
        Ok(Lump::GameLump(g)) => g.static_props().map_or(0, |p| p.props.len()),
        _ => 0,
    };
    (triangles, solids, props)
}

/// One target's lump table, in id order, with the engine's name for each.
fn lump_sizes(bsp: &Bsp) -> Map<String, Value> {
    let version = bsp.version_kind();
    let mut out = Map::new();
    for (id, bytes) in &bsp.lumps {
        let name = version.and_then(|v| rbsp::ids::name(*id, v)).unwrap_or("OPAQUE");
        out.insert(format!("{name}_{id:02x}"), json!(bytes.len()));
    }
    out
}

fn target_report(artifacts: &MapArtifacts, files: &[PathBuf], base: &Path) -> Value {
    let (triangles, solids, props) = counts(&artifacts.bsp);
    let wire = |p: &Path| {
        p.strip_prefix(base).unwrap_or(p).to_string_lossy().replace('\\', "/")
    };
    json!({
        "bsp": wire(&files[0]),
        "bytes": files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum::<u64>(),
        "files": files.iter().map(|f| wire(f)).collect::<Vec<_>>(),
        "lumps": lump_sizes(&artifacts.bsp),
        "triangles": triangles,
        "collision_solids": solids,
        "props": props,
        "entities": artifacts.entity_count(),
    })
}

/// Compiles a project and writes the maps it asked for.
pub fn run(args: &Args) -> Result<Value> {
    progress(0, "opening the project");
    let document = project::load(Box::new(NativeProject::new(&args.project).map_err(|e| anyhow!("{e}"))?))
        .map_err(|e| anyhow!("{}: {e}", args.project.display()))?;
    let stem = document.doc.meta.stem.clone();
    if stem.is_empty() {
        bail!("the document has no map stem");
    }
    let wanted = targets(&args.targets)?;
    if (args.client.is_some() || args.dedi.is_some()) && !(wanted.client && wanted.dedi) {
        bail!("--client and --dedi name both targets, so --targets has to ask for both");
    }
    let paths = Outputs::resolve(args, &stem)?;
    let base = args.out.clone().unwrap_or_else(|| default_out(&args.project));
    // A project imported from a v37 source has no brush tree of its own: its
    // map is the source converted, so the conversion is what runs.
    if let Some(source) = crate::importtf2::descriptor(&args.project)? {
        let mode = bake_mode(&args.bake)?;
        return build_tf2(args, &source, &wanted, &paths, &base, mode);
    }

    progress(10, "resolving assets");
    // Every map built against an install gets a sky; the compiler then fails
    // loudly if the install cannot supply that vista.
    let mut document = document;
    if args.install {
        document.doc.meta.sky = rcomp::sky_vista(&document.doc);
    }
    let (tables, install) = load_tables(args, &document)?;
    let missing = crate::assets::unresolved(&tables, &document.doc);
    if !missing.is_empty() {
        let names: Vec<&String> = missing.materials.iter().chain(missing.models.iter()).collect();
        eprintln!("WARNING {} asset(s) the compiler reads are not in the asset table and compile on defaults: {names:?}", names.len());
        if !args.assets.is_empty() && bake_mode(&args.bake)? == BakeMode::Final && !args.allow_unresolved {
            bail!(
                "a final bake from --assets would light {} unresolved asset(s) on defaults: {names:?}; write the table again on the install \
                 with --src-pak <the map's _src.json> or pass --allow-unresolved",
                names.len()
            );
        }
    }
    let written = match &args.write_assets {
        Some(path) => Some(write_tables(args, &document, &tables, install.as_ref(), path)?),
        None => None,
    };
    // An imported project carries the map it references; anything else falls
    // back to R5ED_REFERENCE_ROOT.
    let inputs = inputs_digest(args, &document, &tables)?;
    if !args.rebuild {
        if let Some(mut report) = reusable(&base, &inputs) {
            eprintln!("REUSED {} holds a build of these inputs ({inputs}); pass --rebuild to compile and bake again", base.join(BUILD_RECORD).display());
            report["reused"] = json!(true);
            report["assets_written"] = json!(written);
            progress(100, "done (reused)");
            return Ok(report);
        }
    }
    // A build that stops half way must not leave an old record vouching for
    // the outputs it was overwriting.
    let _ = std::fs::remove_file(base.join(BUILD_RECORD));
    let kept = args.project.join(crate::importmap::REFERENCE_DIR);
    let reference_root = kept.is_dir().then_some(kept);
    let prop_meshes = crate::propmesh::load_dirs(&crate::propmesh::env_dirs())?;
    let options = CompileOptions { targets: wanted, bake: bake_mode(&args.bake)?, reference_root, skip_rooms: false, prop_meshes };
    progress(20, "compiling");
    let output = rcomp::compile(&document.doc, &DocFs(&document), &tables, &options)
        .map_err(|e| anyhow!("{e}"))?;

    let mut targets_json = Map::new();
    if wanted.client {
        let artifacts = output.client.as_ref().ok_or_else(|| anyhow!("the compiler produced no client map"))?;
        let mut files = write_map(&artifacts.bsp, &artifacts.ent_bytes, &paths.client, &paths.client_ent_dir, &stem)?;
        files.extend(write_cubemap(artifacts.cubemap.as_ref(), &paths.client, &stem)?);
        targets_json.insert("client".into(), target_report(artifacts, &files, &base));
    }
    if wanted.dedi {
        let artifacts = output.dedi.as_ref().ok_or_else(|| anyhow!("the compiler produced no dedicated map"))?;
        let ent_dir = paths.dedi.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let files = write_map(&artifacts.bsp, &artifacts.ent_bytes, &paths.dedi, &ent_dir, &stem)?;
        targets_json.insert("dedi".into(), target_report(artifacts, &files, &base));
    }
    progress(90, "reporting");
    let stats = &output.report.stats;
    let value = json!({
        "project": args.project.display().to_string(),
        "stem": stem,
        "bake": args.bake,
        "unresolved": { "materials": missing.materials, "models": missing.models },
        "assets_written": written,
        "targets": Value::Object(targets_json),
        "stats": {
            "objects": stats.objects,
            "brushes": stats.brushes,
            "faces": stats.faces,
            "render_meshes": stats.render_meshes,
            "triangles": stats.triangles,
            "surfaces": stats.surfaces,
            "collision_solids": stats.collision_solids,
            "bvh_nodes": stats.bvh_nodes,
            "bvh_leaves": stats.bvh_leaves,
            "models": stats.models,
            "props": stats.props,
            "volumes": stats.volumes,
            "lights": stats.lights,
            "terrains": stats.terrains,
            "nav_triangles": stats.nav_triangles,
            "entities": stats.entities,
            "partition_files": stats.partition_files,
            "vis_cells": stats.vis_cells,
            "vis_objects": stats.vis_objects,
            "lightmap_pages": stats.lightmap_pages,
            "lightprobes": stats.lightprobes,
            "cubemaps": stats.cubemaps,
            "csm_shadow_meshes": stats.csm_shadow_meshes,
            "entities_per_class": stats.entities_per_class,
        },
        "warnings": output.report.warnings,
        "validation": output.report.validation,
        "stages": output.report.stages.iter().map(|s| json!({"name": s.name, "micros": s.micros})).collect::<Vec<_>>(),
        "inputs": inputs,
        "reused": false,
    });
    let record = json!({ "version": BUILD_RECORD_VERSION, "inputs": inputs, "report": value });
    let mut body = serde_json::to_string_pretty(&record)?;
    body.push('\n');
    std::fs::write(base.join(BUILD_RECORD), body).with_context(|| format!("writing {}", base.join(BUILD_RECORD).display()))?;
    progress(100, "done");
    Ok(value)
}

/// A project imported from a Titanfall 2 source: the map is the source
/// converted, once per target, and the entity partitions come out of the same
/// conversion.
fn build_tf2(
    args: &Args,
    source: &crate::importtf2::Source,
    wanted: &Targets,
    paths: &Outputs,
    base: &Path,
    mode: BakeMode,
) -> Result<Value> {
    progress(10, "reading the v37 source");
    let tables = crate::importtf2::surface_tables(source)?;
    let (map, ents) = crate::importtf2::read_source(&source.bsp, &source.stem)?;
    let mut targets_json = Map::new();
    let mut stats = rcomp::report::Stats::default();
    let mut warnings: Vec<String> = Vec::new();
    for (label, version, out) in [
        ("client", rbsp::Version::ClientV51, paths.client.clone()),
        ("dedi", rbsp::Version::DediV47, paths.dedi.clone()),
    ] {
        if (label == "client" && !wanted.client) || (label == "dedi" && !wanted.dedi) {
            continue;
        }
        progress(40, &format!("converting for the {label} engine"));
        let converted = rcomp::tf2::convert_tf2(&map, &ents, version, &tables, &source.stem)
            .map_err(|e| anyhow!("{e}"))?;
        let ent_dir = out.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let files = write_map(&converted.artifacts.bsp, &converted.artifacts.ent_bytes, &out, &ent_dir, &source.stem)?;
        stats.merge(&converted.stats);
        warnings.extend(converted.report.warnings.clone());
        let (triangles, solids, props) = counts(&converted.artifacts.bsp);
        let wire = |p: &Path| p.strip_prefix(base).unwrap_or(p).to_string_lossy().replace('\\', "/");
        targets_json.insert(
            label.into(),
            json!({
                "bsp": wire(&files[0]),
                "bytes": files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum::<u64>(),
                "files": files.iter().map(|f| wire(f)).collect::<Vec<_>>(),
                "lumps": lump_sizes(&converted.artifacts.bsp),
                "triangles": triangles,
                "collision_solids": solids,
                "props": props,
                "entities": converted.artifacts.entity_count(),
                "collider_blobs": converted.blob_models,
            }),
        );
    }
    if targets_json.is_empty() {
        bail!("--targets did not ask for the client or the dedicated engine, so there was nothing to convert");
    }
    progress(100, "done");
    Ok(json!({
        "project": args.project.display().to_string(),
        "stem": source.stem.clone(),
        "source": source.bsp.display().to_string(),
        "source_version": 37,
        "bake": args.bake,
        "targets": Value::Object(targets_json),
        "stats": {
            "objects": stats.objects,
            "brushes": stats.brushes,
            "faces": stats.faces,
            "render_meshes": stats.render_meshes,
            "triangles": stats.triangles,
            "surfaces": stats.surfaces,
            "collision_solids": stats.collision_solids,
            "bvh_nodes": stats.bvh_nodes,
            "bvh_leaves": stats.bvh_leaves,
            "models": stats.models,
            "props": stats.props,
            "nav_triangles": 0,
            "entities": stats.entities,
            "partition_files": stats.partition_files,
            "lightmap_pages": stats.lightmap_pages,
            "lightprobes": stats.lightprobes,
            "cubemaps": stats.cubemaps,
            "csm_shadow_meshes": stats.csm_shadow_meshes,
            "entities_per_class": stats.entities_per_class,
        },
        "warnings": warnings,
        "validation": Vec::<String>::new(),
        "stages": Vec::<Value>::new(),
        "bake_mode": match mode {
            BakeMode::None => "none",
            BakeMode::Preview => "preview",
            BakeMode::Final => "final",
        },
    }))
}

/// The asset table a compile runs against: a file the caller generated, the
/// install's own index, or nothing at all, which leaves every name to the
/// compiler's documented defaults.
fn load_tables(args: &Args, document: &Document) -> Result<(rir::AssetTables, Option<crate::assets::Install>)> {
    let (mut tables, install) = load_base_tables(args, document)?;
    // A source pak's materials fill the names the install or the files left
    // open: the asset tool cannot read a pak repak built.
    let mut src = rir::AssetTables::default();
    for path in &args.src_paks {
        let one = crate::assets::src_pak_tables(path).map_err(|e| anyhow!("{}: {e}", path.display()))?;
        src.materials.extend(one.materials);
    }
    if !src.materials.is_empty() {
        let found = crate::assets::resolve_tables(&src, &document.doc);
        for (name, info) in found.materials {
            tables.materials.entry(name).or_insert(info);
        }
    }
    Ok((tables, install))
}

fn load_base_tables(args: &Args, document: &Document) -> Result<(rir::AssetTables, Option<crate::assets::Install>)> {
    if !args.assets.is_empty() {
        let mut merged = rir::AssetTables::default();
        for path in &args.assets {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            let one: rir::AssetTables = serde_json::from_str(&text).map_err(|e| anyhow!("{}: {e}", path.display()))?;
            merged.materials.extend(one.materials);
            merged.models.extend(one.models);
            merged.surface_props.extend(one.surface_props);
        }
        return Ok((crate::assets::resolve_tables(&merged, &document.doc), None));
    }
    if args.install {
        let game_root = crate::tools::game_root()?;
        let rsx = crate::tools::tool(crate::tools::RSX)?;
        let cache = crate::tools::exe_dir()
            .map(|d| d.join("r5ed-cache"))
            .unwrap_or_else(std::env::temp_dir);
        let install = crate::assets::Install::open(&game_root, &cache, &rsx, &document.doc.meta.stem).map_err(|e| anyhow!("{e}"))?;
        let tables = install.asset_tables(&document.doc).map_err(|e| anyhow!("{e}"))?;
        return Ok((tables, Some(install)));
    }
    Ok((rir::AssetTables::default(), None))
}

/// Writes `--write-assets`: from an install, the map's whole table (the models
/// it only packs included) or, with `--all-assets`, every asset of those kinds;
/// otherwise the table the compile ran against. Returns what was written.
fn write_tables(
    args: &Args,
    document: &Document,
    tables: &rir::AssetTables,
    install: Option<&crate::assets::Install>,
    path: &Path,
) -> Result<Value> {
    let mut failed = 0usize;
    let out = match install {
        Some(install) if !args.all_assets.is_empty() => {
            let mut kinds = Vec::new();
            for k in &args.all_assets {
                match k.trim() {
                    "materials" => kinds.push("matl"),
                    "models" => kinds.push("mdl_"),
                    other => bail!("--all-assets takes materials and models, not {other:?}"),
                }
            }
            let root = crate::tools::exe_dir().map(|d| d.join("r5ed-cache")).unwrap_or_else(std::env::temp_dir).join("batch");
            let (all, n) = install.all_tables(&kinds, &root, |done, total, what| {
                eprintln!("ASSETS {done}/{total} {what}");
            });
            failed = n;
            all
        }
        Some(install) => {
            // What the compile resolved past the install (a source pak's
            // materials) goes in too.
            let mut out = install.written_tables(&document.doc).map_err(|e| anyhow!("{e}"))?;
            for (name, info) in &tables.materials {
                out.materials.entry(name.clone()).or_insert_with(|| info.clone());
            }
            out
        }
        None if !args.all_assets.is_empty() => bail!("--all-assets reads the install, so it needs --install and no --assets"),
        None => tables.clone(),
    };
    let mut body = serde_json::to_string_pretty(&out)?;
    body.push('\n');
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(json!({
        "path": path.display().to_string(),
        "materials": out.materials.len(),
        "models": out.models.len(),
        "surface_props": out.surface_props.len(),
        "failed": failed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_record_is_reused_only_for_the_same_inputs_and_files() {
        let dir = std::env::temp_dir().join(format!("r5ed-buildrec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("maps")).unwrap();
        assert!(reusable(&dir, "aa").is_none(), "nothing built yet");
        let report = json!({ "targets": { "client": { "files": ["maps/mp_rr_x.bsp"] } }, "stats": {} });
        let record = json!({ "version": BUILD_RECORD_VERSION, "inputs": "aa", "report": report });
        std::fs::write(dir.join(BUILD_RECORD), record.to_string()).unwrap();
        assert!(reusable(&dir, "aa").is_none(), "a file the record names is missing");
        std::fs::write(dir.join("maps/mp_rr_x.bsp"), b"x").unwrap();
        assert_eq!(reusable(&dir, "aa").unwrap()["targets"]["client"]["files"][0], "maps/mp_rr_x.bsp");
        assert!(reusable(&dir, "bb").is_none(), "changed inputs build again");
        std::fs::write(dir.join(BUILD_RECORD), "{ not json").unwrap();
        assert!(reusable(&dir, "aa").is_none(), "an unreadable record is no build");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_target_list_is_parsed_in_either_order() {
        assert_eq!(targets("client,dedi").unwrap(), Targets { client: true, dedi: true });
        assert_eq!(targets("dedi").unwrap(), Targets { client: false, dedi: true });
        assert_eq!(targets(" client , dedi ").unwrap(), Targets { client: true, dedi: true });
        for bad in ["", "nope", "client,rm", ",", "client,,dedi"] {
            assert!(targets(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_bake_mode_is_parsed_by_name() {
        assert_eq!(bake_mode("none").unwrap(), BakeMode::None);
        assert_eq!(bake_mode("preview").unwrap(), BakeMode::Preview);
        assert_eq!(bake_mode("final").unwrap(), BakeMode::Final);
        assert!(bake_mode("").is_err());
        assert!(bake_mode("fast").is_err());
    }

    #[test]
    fn output_paths_follow_the_build_tree_unless_named() {
        let project = PathBuf::from("p");
        let args = Args {
            project: project.clone(),
            out: None,
            targets: "client,dedi".into(),
            client: None,
            dedi: None,
            ent_dir: None,
            bake: "none".into(),
            assets: Vec::new(),
            install: false,
            write_assets: None,
            all_assets: Vec::new(),
            allow_unresolved: false,
            src_paks: Vec::new(),
            rebuild: false,
        };
        let l = Outputs::resolve(&args, "mp_rr_x").unwrap();
        assert_eq!(l.client, PathBuf::from("p/build/maps/mp_rr_x.bsp"));
        assert_eq!(l.dedi, PathBuf::from("p/build/dedi/maps/mp_rr_x.bsp"));
        assert_eq!(l.client_ent_dir, PathBuf::from("p/build/maps"));
        let named = Args { client: Some("c/x.bsp".into()), dedi: Some("d/y.bsp".into()), ent_dir: Some("e".into()), ..args.clone() };
        let l = Outputs::resolve(&named, "mp_rr_x").unwrap();
        assert_eq!(l.client, PathBuf::from("c/x.bsp"));
        assert_eq!(l.dedi, PathBuf::from("d/y.bsp"));
        assert_eq!(l.client_ent_dir, PathBuf::from("e"));
        let bad = Args { client: Some("c/x.map".into()), ..args };
        assert!(Outputs::resolve(&bad, "mp_rr_x").is_err(), "a map path has to end in .bsp");
    }

    #[test]
    fn a_written_map_reports_its_own_counts() {
        let mut bsp = Bsp::new(51, 0, 0);
        bsp.set_lump(rbsp::ids::MODELS, rbsp::lumps::collision::write_models(
            &[rbsp::lumps::collision::Model { bvh_node: 0, bvh_leaf: 1, ..Default::default() },
              rbsp::lumps::collision::Model { bvh_node: -1, bvh_leaf: -1, ..Default::default() }],
            rbsp::Version::ClientV51,
        ));
        let (triangles, solids, props) = counts(&bsp);
        assert_eq!((triangles, solids, props), (0, 1, 0), "only the model with a tree is a solid");
        let sizes = lump_sizes(&bsp);
        assert!(sizes.keys().any(|k| k.starts_with("MODELS_0e")), "{sizes:?}");
        assert_eq!(sizes.values().next(), Some(&json!(128)));
    }
}
