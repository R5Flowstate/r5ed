//! The game install as the compiler and the packer see it: which pak carries an
//! asset, what a material resolves to, and which assets a document needs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rdoc::model::{MapDoc, ObjectKind};
use rir::{AssetTables, MaterialInfo, ModelInfo, VertexFormat};
use rpak::{parse_material_json, AssetIndex, AssetRow, RawMaterial, Rsx};

/// The client pak directories an install keeps its assets in.
const PAK_DIRS: [&str; 3] = ["Win64", "opt", "Win64_server"];
/// The newest model version the dedicated server's paks carry; client paks
/// carry later ones.
const SERVER_MODEL_VERSION: u32 = 10;
/// The file the surface prop table lives in, relative to the install root.
const SURFACE_PROPS: &str = "platform/scripts/surfaceproperties.rson";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no pak under {0} carries a map's assets")]
    NoPaks(PathBuf),
    #[error("the asset tool failed: {0}")]
    Rsx(#[from] rpak::Error),
    #[error("the asset tool could not export {guid:016X} from {pak}")]
    Export { pak: String, guid: u64 },
    #[error("the export of {name} holds no {file}")]
    NoExportFile { name: String, file: &'static str },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// The assets a document names, in the order they are packed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Referenced {
    pub materials: BTreeSet<String>,
    pub models: BTreeSet<String>,
}

impl Referenced {
    pub fn is_empty(&self) -> bool {
        self.materials.is_empty() && self.models.is_empty()
    }
}

fn brushes<'a>(kind: &'a ObjectKind, mut out: impl FnMut(&'a rdoc::model::BrushFace)) {
    match kind {
        ObjectKind::Brush(b) => b.faces.iter().for_each(&mut out),
        ObjectKind::Volume(v) => v.brushes.iter().flat_map(|b| b.faces.iter()).for_each(out),
        _ => {}
    }
}

/// Every material and model a document references, from its brushes, meshes,
/// terrain and props.
pub fn referenced(doc: &MapDoc) -> Referenced {
    let mut out = drawn(doc);
    for o in &doc.objects {
        if let ObjectKind::Entity(e) = &o.kind {
            // Editor markers (mdl/dev) never draw in a shipped map.
            if let Some(m) = e.keyvalues.get("model").filter(|m| m.ends_with(".rmdl") && !m.starts_with("mdl/dev/")) {
                out.models.insert(m.clone());
            }
        }
    }
    // The compiler adds the menu and ceremony rooms a map lacks.
    out.models.extend(rcomp::environment::basement_models(doc));
    out.retain_valid();
    out
}

/// What the compiler itself reads: the materials and models it builds
/// geometry, props and the sky from. Models an entity only names are packed
/// but never read at compile time.
fn drawn(doc: &MapDoc) -> Referenced {
    let mut out = Referenced::default();
    for o in &doc.objects {
        match &o.kind {
            ObjectKind::Mesh(m) => {
                out.materials.extend(m.material_map.values().cloned());
            }
            ObjectKind::Terrain(t) => {
                for l in &t.layers {
                    out.materials.insert(l.material.clone());
                    for s in &l.scatter {
                        out.models.extend(s.models.iter().cloned());
                    }
                }
            }
            ObjectKind::VoxelTerrain(v) => {
                out.materials.insert(v.material.clone());
            }
            ObjectKind::Prop(p) => {
                out.models.insert(p.model.clone());
            }
            _ => {}
        }
        brushes(&o.kind, |f| {
            out.materials.insert(f.material.clone());
        });
    }
    // The compiler builds the skybox from the vista, and its shell draws with
    // the sky material.
    if let Some(vista) = doc.meta.sky.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none")) {
        out.models.insert(vista.to_string());
        out.materials.insert(rcomp::meshes::SKY_MATERIAL.to_string());
    }
    // A Source skybox draws its six faces on a cube at the vista, under the
    // same shell.
    if let Some(sky) = doc.meta.source_sky.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        out.materials.insert(rcomp::meshes::SKY_MATERIAL.to_string());
        out.materials.extend(rcomp::sky::face_materials(sky));
    }
    out.retain_valid();
    out
}

impl Referenced {
    fn retain_valid(&mut self) {
        self.materials.retain(|m| !m.is_empty());
        self.models.retain(|m| !m.is_empty());
    }
}

/// An install's asset index, plus the tool that reads the paks.
pub struct Install {
    game_root: PathBuf,
    cache: PathBuf,
    rsx: Rsx,
    paks: Vec<PathBuf>,
    index: AssetIndex,
    surface_props: BTreeMap<String, u32>,
    /// Where a batched export already wrote an asset, by kind and guid.
    batch: std::sync::Mutex<std::collections::HashMap<(String, u64), PathBuf>>,
}

impl Install {
    /// Opens an install, listing the paks that carry a map's assets. The index
    /// is cached and reused until one of those paks changes. The paks the map
    /// `own` builds are left out: a map sourcing an asset from its own last
    /// build pairs that build's rewritten model with another pak's vertex data.
    pub fn open(game_root: &Path, cache: &Path, rsx_exe: &Path, own: &str) -> Result<Self, Error> {
        let mut paks = AssetIndex::scan_paks(game_root, &PAK_DIRS)?;
        paks.retain(|p| !is_own_pak(p, own));
        paks.sort();
        if paks.is_empty() {
            return Err(Error::NoPaks(game_root.to_path_buf()));
        }
        let index_path = cache.join("index").join("assets.json");
        let scratch = cache.join("rsx");
        let index = AssetIndex::refresh(&index_path, game_root, &paks, |stale| Rsx::new(rsx_exe).list(stale, &scratch))?;
        let surface_props = rpak::read_surface_props_file(&game_root.join(SURFACE_PROPS)).unwrap_or_default();
        Ok(Self {
            game_root: game_root.to_path_buf(),
            cache: cache.to_path_buf(),
            rsx: Rsx::new(rsx_exe),
            paks,
            index,
            surface_props,
            batch: Default::default(),
        })
    }

    /// The pak a row was read out of.
    pub fn pak_of(&self, row: &AssetRow) -> Option<PathBuf> {
        self.paks
            .iter()
            .find(|p| p.file_name().map(|s| s.to_string_lossy() == row.pak).unwrap_or(false))
            .cloned()
    }

    /// A material row for a name a document uses. The install stores materials
    /// with a variant suffix, so an exact name is preferred and a variant of it
    /// is the fallback. Among variants the world surface itself (`_wldc`) wins,
    /// then the other world and model variants, and the `_colpass` materials,
    /// which are a secondary pass the world material names, come last; a tie
    /// goes to the path outside `models/`, then the lexicographically first,
    /// so the choice never depends on row order.
    pub fn material_row(&self, name: &str) -> Option<&AssetRow> {
        let want = normalize_material(name);
        best_material(&want, self.index.assets.iter().filter(|r| r.kind == "matl").map(|r| (r.path.as_str(), r)))
    }

    /// The materials a model asset names, in the order it names them. A model
    /// holds each material as its 64-bit guid on a 4-byte boundary (the skin
    /// table packs them after 32-bit fields), so every such value that is the
    /// guid of a material the install carries is one.
    pub fn materials_named_by(&self, model: &[u8]) -> Vec<&AssetRow> {
        let by_guid: BTreeMap<u64, &AssetRow> =
            self.index.assets.iter().filter(|r| r.kind == "matl").map(|r| (r.guid, r)).collect();
        let mut out: Vec<&AssetRow> = Vec::new();
        for chunk in model.windows(8).step_by(4) {
            let guid = u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8]));
            if let Some(row) = by_guid.get(&guid) {
                if !out.iter().any(|r| r.guid == guid) {
                    out.push(row);
                }
            }
        }
        out
    }

    /// A model row for a `mdl/....rmdl` path.
    pub fn model_row(&self, path: &str) -> Option<&AssetRow> {
        self.index.find_model(path)
    }

    /// A material's compiler-facing description, read by exporting it once.
    pub fn material(&self, name: &str) -> Result<Option<MaterialInfo>, Error> {
        let Some(row) = self.material_row(name) else {
            return Ok(None);
        };
        self.material_of(row, name).map(Some)
    }

    /// The compiler-facing description of one material row, under `name`.
    fn material_of(&self, row: &AssetRow, name: &str) -> Result<MaterialInfo, Error> {
        let dir = self.scratch("material", row);
        let exported = dir.join(without_extension(&row.path, ".json"));
        // An interrupted or failed export can leave an empty file behind.
        if std::fs::metadata(&exported).map(|m| m.len()).unwrap_or(0) == 0 {
            self.export_material(row, &dir)?;
        }
        let text = std::fs::read_to_string(&exported)?;
        let raw: RawMaterial = parse_material_json(&text)?;
        let mut info = rpak::MaterialInfo::resolve(raw, |slot| self.texture_guid(slot));
        info.guid = row.guid;
        info.path = name.to_string();
        Ok(to_rir_material(info))
    }

    /// A model's compiler-facing description.
    pub fn model(&self, path: &str) -> Result<Option<ModelInfo>, Error> {
        let Some(row) = self.model_row(path) else {
            return Ok(None);
        };
        if self.pak_of(row).is_none() {
            return Ok(None);
        }
        self.model_of(row, path).map(Some)
    }

    /// The compiler-facing description of one model row, under `path`.
    fn model_of(&self, row: &AssetRow, path: &str) -> Result<ModelInfo, Error> {
        let out = self.scratch("model", row);
        let home = export_dir(&out, row.stem());
        let base = base_name(row.stem());
        let rmdl = home.join(format!("{base}.rmdl"));
        // The tool names a model's vertex data .vg_static; .vg is the older name.
        let vg_of = || ["vg_static", "vg"].map(|e| home.join(format!("{base}.{e}"))).into_iter().find(|p| p.is_file());
        if !rmdl.is_file() || vg_of().is_none() {
            self.export_model(row, &out)?;
        }
        let vg = vg_of().unwrap_or_else(|| home.join(format!("{base}.vg_static")));
        let model = rpak::rmdl::parse_model(
            &read_asset(&rmdl, row, "rmdl")?,
            &read_asset(&vg, row, "vg")?,
        )?;
        let info = rpak::ModelInfo::from_rmdl(&model, path, row.guid, vec![row.pak.clone()]);
        Ok(ModelInfo {
            path: info.path,
            guid: info.guid,
            bounds_min: glam::Vec3::from_array(rpak::rmdl::vertex_to_studiohdr_space(info.bounds_min)),
            bounds_max: glam::Vec3::from_array(rpak::rmdl::vertex_to_studiohdr_space(info.bounds_max)),
            has_collision: info.has_collision,
            resident_paks: info.resident_paks,
        })
    }

    /// The compiler's asset table for the assets a document names. A name the
    /// install does not carry is left out, and the compiler falls back to its
    /// documented defaults for it.
    pub fn asset_tables(&self, doc: &MapDoc) -> Result<AssetTables, Error> {
        let want = drawn(doc);
        let mut tables = AssetTables { surface_props: self.surface_props.clone(), ..Default::default() };
        for name in &want.materials {
            if let Some(m) = self.material(name)? {
                tables.materials.insert(name.clone(), m);
            }
        }
        for path in &want.models {
            if let Some(m) = self.model(path)? {
                tables.models.insert(path.clone(), m);
            }
        }
        Ok(tables)
    }

    /// `asset_tables` plus every model the map packs but the compiler never
    /// reads (entity models, the rooms' props), so a written table describes
    /// all of the map. A model the install cannot export is left out.
    pub fn written_tables(&self, doc: &MapDoc) -> Result<AssetTables, Error> {
        let mut tables = self.asset_tables(doc)?;
        for path in referenced(doc).models {
            if !tables.models.contains_key(&path) {
                if let Ok(Some(m)) = self.model(&path) {
                    tables.models.insert(path, m);
                }
            }
        }
        Ok(tables)
    }

    /// Every asset of `kinds` (`matl`, `mdl_`) the index carries on a pak on disk, keyed by
    /// the asset's own path, so a machine without the install can build any map
    /// from it (`resolve_tables` finds a document's names in it). Exports run
    /// one pak at a time into `root`, reporting each pak through `progress`
    /// (done, total, what); an asset the tool cannot export is counted, not fatal.
    pub fn all_tables(&self, kinds: &[&str], root: &Path, mut progress: impl FnMut(usize, usize, &str)) -> (AssetTables, usize) {
        let mut tables = AssetTables { surface_props: self.surface_props.clone(), ..Default::default() };
        let mut failed = 0usize;
        for kind in kinds.iter().copied().filter(|k| matches!(*k, "matl" | "mdl_")) {
            let export = kind;
            let mut seen = BTreeSet::new();
            let mut by_pak: BTreeMap<PathBuf, Vec<&AssetRow>> = BTreeMap::new();
            for row in self.index.assets.iter().filter(|r| r.kind == kind) {
                if let Some(pak) = self.pak_of(row) {
                    if seen.insert(row.path.to_ascii_lowercase()) {
                        by_pak.entry(pak).or_default().push(row);
                    }
                }
            }
            let total: usize = by_pak.values().map(Vec::len).sum();
            let mut done = 0usize;
            for (pak, rows) in &by_pak {
                let name = pak.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                progress(done, total, &format!("{kind} {name} ({} assets)", rows.len()));
                self.prefetch(export, rows, root);
                for row in rows {
                    let key = row.path.replace('\\', "/").to_ascii_lowercase();
                    let ok = if kind == "matl" {
                        self.material_of(row, &key).map(|m| tables.materials.insert(normalize_material(&key), m)).is_ok()
                    } else {
                        self.model_of(row, &key).map(|m| tables.models.insert(key.clone(), m)).is_ok()
                    };
                    failed += usize::from(!ok);
                }
                done += rows.len();
            }
            progress(total, total, &format!("{kind} done"));
        }
        (tables, failed)
    }

    /// Exports one material with its texture slots written as guids, which is
    /// the form that lets a packer resolve them from the archive it is building
    /// instead of from a path inside one export.
    pub fn export_material(&self, row: &AssetRow, out: &Path) -> Result<PathBuf, Error> {
        use std::process::Command;
        if self.copy_batched("matl", row, out)
            && std::fs::metadata(out.join(without_extension(&row.path, ".json"))).map(|m| m.len()).unwrap_or(0) > 0
        {
            return Ok(out.join(without_extension(&row.path, ".json")));
        }
        let pak = self.pak_of(row).ok_or_else(|| Error::NoPaks(self.game_root.clone()))?;
        std::fs::create_dir_all(out)?;
        let guids = out.join("export_guids.txt");
        std::fs::write(&guids, format!("{:016x}\n", row.guid))?;
        let status = Command::new(self.rsx.exe())
            .args(["-nogui", "-nocachedb", "-export", "--exporttypes", "matl", "--texturenames", "guid"])
            .arg("--exportguids")
            .arg(&guids)
            .arg("--exportdir")
            .arg(out)
            .arg(&pak)
            // The tool writes a multi-megabyte cache database into whatever
            // directory it runs in, so it runs in the export tree rather than
            // wherever the caller happened to launch from.
            .current_dir(out)
            .output()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let json = out.join(without_extension(&row.path, ".json"));
        if !status.status.success() || std::fs::metadata(&json).map(|m| m.len()).unwrap_or(0) == 0 {
            // An archive can carry a material it cannot export whole (or at
            // all, like a pak r5ed built); another archive with it usually can.
            for other in self.paks_carrying("matl", row.guid) {
                if other == pak {
                    continue;
                }
                let _ = self.rsx.export_material(&[other], row.guid, out);
                if std::fs::metadata(&json).map(|m| m.len()).unwrap_or(0) > 0 {
                    break;
                }
            }
            if std::fs::metadata(&json).map(|m| m.len()).unwrap_or(0) == 0 {
                return Err(Error::Export { pak: pak.display().to_string(), guid: row.guid });
            }
        }
        Ok(json)
    }

    /// Exports a model, returning the directory its files landed in. The vertex
    /// groups travel beside the model, so they come out of the same export.
    pub fn export_model(&self, row: &AssetRow, out: &Path) -> Result<PathBuf, Error> {
        if self.copy_batched("mdl_", row, out) {
            return Ok(export_dir(out, row.stem()));
        }
        let pak = self.pak_of(row).ok_or_else(|| Error::NoPaks(self.game_root.clone()))?;
        std::fs::create_dir_all(out)?;
        self.rsx.export(&[pak], &["mdl_", "mdl"], &[row.guid], out)?;
        Ok(export_dir(out, row.stem()))
    }

    /// The dedicated server's copies of a model: the rows its own paks carry,
    /// each with the server pak it is read from. Client and server paks share
    /// file names, so the row's pak is looked up in the server directory.
    pub fn server_model_rows(&self, path: &str) -> Vec<(&AssetRow, PathBuf)> {
        let want = path.replace('\\', "/").to_ascii_lowercase();
        let dir = self.game_root.join("paks").join("Win64_server");
        self.index
            .assets
            .iter()
            .filter(|r| r.kind == "mdl_" && r.rsx_version <= SERVER_MODEL_VERSION && r.path.to_ascii_lowercase() == want)
            .filter_map(|r| {
                let pak = dir.join(&r.pak);
                pak.is_file().then_some((r, pak))
            })
            .collect()
    }

    /// Exports a model out of one given pak, returning the directory its files
    /// landed in.
    pub fn export_model_from(&self, pak: &Path, row: &AssetRow, out: &Path) -> Result<PathBuf, Error> {
        std::fs::create_dir_all(out)?;
        self.rsx.export(&[pak.to_path_buf()], &["mdl_", "mdl"], &[row.guid], out)?;
        Ok(export_dir(out, row.stem()))
    }

    /// Exports a texture as its metadata document and one DDS per mip, which is
    /// what the packer reads back.
    pub fn export_texture(&self, row: &AssetRow, out: &Path) -> Result<(), Error> {
        if self.copy_batched("txtr", row, out) {
            return Ok(());
        }
        let pak = self.pak_of(row).ok_or_else(|| Error::NoPaks(self.game_root.clone()))?;
        std::fs::create_dir_all(out)?;
        self.rsx.export_texture(&[pak], row.guid, out)?;
        Ok(())
    }

    /// The guid a material's texture slot names, which the export writes either
    /// as a guid or as the path the texture was exported under.
    pub fn texture_guid(&self, slot: &str) -> Option<u64> {
        if let Some(hex) = slot.strip_prefix("0x").or_else(|| slot.strip_prefix("0X")) {
            return u64::from_str_radix(hex, 16).ok();
        }
        let stem = slot.strip_suffix(".rpak").unwrap_or(slot).replace('\\', "/");
        let full = if stem.starts_with("texture/") { stem.clone() } else { format!("texture/{stem}") };
        if let Some(r) = self.index.assets.iter().find(|r| r.kind == "txtr" && r.stem() == full) {
            return Some(r.guid);
        }
        let base = stem.rsplit('/').next().unwrap_or(&stem).to_ascii_lowercase();
        self.index
            .assets
            .iter()
            .find(|r| r.kind == "txtr" && r.file_name().split('.').next().unwrap_or_else(|| r.file_name()).eq_ignore_ascii_case(&base))
            .map(|r| r.guid)
    }

    /// The row a texture guid belongs to.
    pub fn texture_row(&self, guid: u64) -> Option<&AssetRow> {
        self.index.assets.iter().find(|r| r.kind == "txtr" && r.guid == guid)
    }

    /// A material row by guid.
    pub fn material_row_by_guid(&self, guid: u64) -> Option<&AssetRow> {
        self.index.assets.iter().find(|r| r.kind == "matl" && r.guid == guid)
    }

    /// Whether the client can resolve an asset without the map's own archive:
    /// true when a pak other than a map's own (`mp_rr_*`) carries it. The index
    /// covers the common and lobby paks and the maps; an asset it has never
    /// seen lives in the core paks it does not list.
    pub fn is_resident(&self, guid: u64) -> bool {
        let mut rows = self.index.assets.iter().filter(|r| r.guid == guid).peekable();
        if rows.peek().is_none() {
            return true;
        }
        rows.any(|r| !r.pak.starts_with("mp_rr_"))
    }

    /// Whether a pak the client always has loaded carries an asset. The lobby
    /// pak is not one of them: a match unloads it.
    pub fn is_core(&self, guid: u64) -> bool {
        self.index.assets.iter().any(|r| r.guid == guid && r.pak.starts_with("common"))
    }

    /// Exports one asset of any kind out of the first pak that carries it,
    /// into `out` at the path the tool gives that kind.
    pub fn export_asset(&self, kind: &str, guid: u64, out: &Path) -> Result<(), Error> {
        if let Some(row) = self.index.assets.iter().find(|r| r.kind == kind && r.guid == guid) {
            if self.copy_batched(kind, row, out) {
                return Ok(());
            }
        }
        // A pak r5ed built itself can carry an asset the tool cannot export
        // back out; any other pak holding the same asset serves instead.
        let mut last = Error::NoPaks(self.game_root.clone());
        std::fs::create_dir_all(out)?;
        for pak in self.index.assets.iter().filter(|r| r.kind == kind && r.guid == guid).filter_map(|r| self.pak_of(r)) {
            match self.rsx.export(&[pak], &[kind], &[guid], out) {
                Ok(()) => return Ok(()),
                Err(e) => last = e.into(),
            }
        }
        Err(last)
    }

    /// Exports `rows` of one kind with one run of the tool per pak, into
    /// `root/<kind>/<pak>`, and remembers where each landed; the single-asset
    /// exports then copy from there instead of parsing the pak again.
    pub fn prefetch(&self, kind: &str, rows: &[&AssetRow], root: &Path) {
        let (kinds, extra): (Vec<&str>, Vec<&str>) = match kind {
            "matl" => (vec!["matl"], vec!["--texturenames", "guid"]),
            "txtr" => (vec!["txtr"], vec!["--exportsetting", "txtr=3"]),
            "mdl_" => (vec!["mdl_", "mdl"], vec![]),
            other => (vec![other], vec![]),
        };
        let mut by_pak: BTreeMap<PathBuf, Vec<u64>> = BTreeMap::new();
        {
            let mut done = self.batch.lock().unwrap_or_else(|e| e.into_inner());
            for row in rows {
                if done.contains_key(&(kind.to_string(), row.guid)) {
                    continue;
                }
                if let Some(pak) = self.pak_of(row) {
                    // An earlier run over the same build of the pak already wrote it.
                    let dir = self.batch_dir(root, kind, &pak);
                    if batched_files(kind, row, &dir).is_some() {
                        done.insert((kind.to_string(), row.guid), dir);
                        continue;
                    }
                    let list = by_pak.entry(pak).or_default();
                    if !list.contains(&row.guid) {
                        list.push(row.guid);
                    }
                }
            }
        }
        for (pak, guids) in by_pak {
            let dir = self.batch_dir(root, kind, &pak);
            if self.rsx.export_batch(&[pak], &kinds, &extra, &guids, &dir).is_ok() {
                let mut done = self.batch.lock().unwrap_or_else(|e| e.into_inner());
                for g in guids {
                    done.insert((kind.to_string(), g), dir.clone());
                }
            }
        }
    }

    /// Where a batched export out of `pak` lands, tagged with the pak's build.
    fn batch_dir(&self, root: &Path, kind: &str, pak: &Path) -> PathBuf {
        let name = pak.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let tag = self.pak_tag(&name);
        root.join(kind.trim_end_matches('_')).join(format!("{name}-{tag}"))
    }

    /// Copies a batched export of `row` into `out` where a single export would
    /// have written it; false when the batch holds no file of it.
    fn copy_batched(&self, kind: &str, row: &AssetRow, out: &Path) -> bool {
        let found = self.batch.lock().unwrap_or_else(|e| e.into_inner()).get(&(kind.to_string(), row.guid)).cloned();
        let Some(from_root) = found else { return false };
        let Some((from, files)) = batched_files(kind, row, &from_root) else { return false };
        let to = match kind {
            "shds" => out.join("shaderset"),
            "shdr" => out.join("shader"),
            _ => export_dir(out, row.stem()),
        };
        for name in files {
            if std::fs::create_dir_all(&to).is_err() || std::fs::copy(from.join(&name), to.join(&name)).is_err() {
                return false;
            }
        }
        true
    }

    /// The first row of `kind` with this guid that a pak on disk carries.
    pub fn row_of(&self, kind: &str, guid: u64) -> Option<&AssetRow> {
        self.index.assets.iter().find(|r| r.kind == kind && r.guid == guid && self.pak_of(r).is_some())
    }

    /// Every pak on disk that carries an asset of `kind` with this guid.
    pub fn paks_carrying(&self, kind: &str, guid: u64) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for r in self.index.assets.iter().filter(|r| r.kind == kind && r.guid == guid) {
            if let Some(p) = self.pak_of(r) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Exports one asset out of one given pak.
    pub fn export_asset_from(&self, pak: &Path, kind: &str, guid: u64, out: &Path) -> Result<(), Error> {
        if let Some(row) = self.index.assets.iter().find(|r| r.kind == kind && r.guid == guid) {
            if self.copy_batched(kind, row, out) {
                return Ok(());
            }
        }
        std::fs::create_dir_all(out)?;
        self.rsx.export(&[pak.to_path_buf()], &[kind], &[guid], out)?;
        Ok(())
    }

    /// The parent of a child shader: a shader with no bytecode of its own,
    /// which the packer rebuilds from its parent. Read from the dependency
    /// lists of the paks that carry it, where a child's line names two shaders.
    pub fn shader_parent(&self, guid: u64) -> Option<u64> {
        let shader = |s: &str| {
            let s = s.trim();
            let hex = s.strip_prefix(r"shader\0x").or_else(|| s.strip_prefix("shader/0x"))?;
            u64::from_str_radix(hex.split('.').next()?, 16).ok()
        };
        for pak in self.paks_carrying("shdr", guid) {
            let name = pak.file_name()?.to_string_lossy().into_owned();
            let adj = self.cache.join("dep").join(format!("{name}-{}.adj", self.pak_tag(&name)));
            if !adj.is_file() && self.rsx.depfile(&[pak.clone()], &adj).is_err() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&adj) else { continue };
            let parent = text.lines().find_map(|line| {
                let (a, b) = line.split_once(',')?;
                (shader(a)? == guid).then(|| shader(b)).flatten()
            });
            if parent.is_some() {
                return parent;
            }
        }
        None
    }

    /// The paks that carry an asset, for a report.
    pub fn paks_of(&self, guid: u64) -> Vec<String> {
        let mut out: Vec<String> = self.index.assets.iter().filter(|r| r.guid == guid).map(|r| r.pak.clone()).collect();
        out.sort();
        out.dedup();
        out
    }

    /// A scratch directory that belongs to one asset.
    fn scratch(&self, kind: &str, row: &AssetRow) -> PathBuf {
        self.cache.join("rsx").join(format!("{kind}-{:016x}-{}", row.guid, self.pak_tag(&row.pak)))
    }

    /// A short tag of a pak's size and time, so an export cached from one build
    /// of a pak is never read back after the pak is rebuilt.
    fn pak_tag(&self, pak: &str) -> String {
        let s = self.index.paks.get(pak).map_or((0, 0), |s| (s.size, s.mtime_ms));
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in s.0.to_le_bytes().into_iter().chain(s.1.to_le_bytes()) {
            h = (h ^ b as u64).wrapping_mul(0x1000_0000_01b3);
        }
        format!("{:08x}", h as u32)
    }
}

/// The best match for a normalized material name among `(asset path, item)`
/// candidates. The install stores materials with a variant suffix, so an exact
/// name is preferred and a variant of it is the fallback. Among variants the
/// world surface itself (`_wldc`) wins, then the other world and model
/// variants, and the `_colpass` materials, which are a secondary pass the
/// world material names, come last; a tie goes to the path outside `models/`,
/// then the lexicographically first, so the choice never depends on order.
fn best_material<'a, T>(want: &str, candidates: impl Iterator<Item = (&'a str, T)>) -> Option<T> {
    let mut best: Option<(&'a str, T, (u8, bool))> = None;
    for (raw, item) in candidates {
        let path = normalize_material(raw);
        let class = if path == want {
            0
        } else if path.starts_with(&format!("{want}_")) {
            let tail = &path[want.len()..];
            if tail.contains("colpass") {
                5
            } else if tail == "_wldc" {
                1
            } else if tail.contains("wldc") {
                2
            } else if tail.contains("rgd") {
                3
            } else {
                4
            }
        } else {
            continue;
        };
        let rank = (class, raw.to_ascii_lowercase().contains("/models/"));
        let better = best.as_ref().is_none_or(|(b, _, br)| rank < *br || (rank == *br && raw < *b));
        if better {
            best = Some((raw, item, rank));
        }
    }
    best.map(|(_, item, _)| item)
}

/// A document's table out of loaded ones: every material and model the
/// compiler reads, found under the document's own name first and otherwise
/// the way the install finds it, so a full table written by `all_tables`
/// serves any map. Surface props carry over whole.
pub fn resolve_tables(loaded: &AssetTables, doc: &MapDoc) -> AssetTables {
    let want = drawn(doc);
    let mut out = AssetTables { surface_props: loaded.surface_props.clone(), ..Default::default() };
    for name in &want.materials {
        let found = loaded.materials.get(name).or_else(|| {
            let norm = normalize_material(name);
            loaded.materials.get(&norm).or_else(|| {
                // A full table keys by normalized path and keeps the asset's own
                // path in the entry, which is what the variant ranking reads.
                best_material(&norm, loaded.materials.values().map(|m| (m.path.as_str(), m)))
            })
        });
        if let Some(m) = found {
            out.materials.insert(name.clone(), MaterialInfo { path: name.clone(), ..m.clone() });
        }
    }
    for path in referenced(doc).models {
        let lower = path.replace('\\', "/").to_ascii_lowercase();
        let found = loaded.models.get(&path).or_else(|| loaded.models.get(&lower)).or_else(|| loaded.models.get(&format!("{lower}.rmdl")));
        if let Some(m) = found {
            out.models.insert(path.clone(), ModelInfo { path: path.clone(), ..m.clone() });
        }
    }
    out
}

/// The materials a source pak builds, read from the repak manifest objmat,
/// srcmat and the other material builders write (`<stem>_src.json`) and the
/// material json beside each entry, the same document a material export holds.
/// The asset tool cannot read a pak repak built, so this is the only way to
/// a map's authored materials, and it needs no tool at all. Entries key by
/// normalized path and keep the asset path, as a full table does, so
/// `resolve_tables` finds a document's names in them. A material the
/// manifest names but whose json is missing or unreadable is an error.
pub fn src_pak_tables(manifest: &Path) -> Result<AssetTables, Error> {
    let text = std::fs::read_to_string(manifest)?;
    let doc: serde_json::Value = serde_json::from_str(&text)?;
    let named = doc.get("assetsDir").and_then(|v| v.as_str()).map(PathBuf::from);
    // The manifest names its assets by the absolute path it was written with;
    // a copy moved elsewhere finds them beside itself.
    let assets = named
        .filter(|d| d.is_dir())
        .or_else(|| manifest.parent().map(|p| p.join("assets")))
        .unwrap_or_else(|| PathBuf::from("assets"));
    let mut tables = AssetTables::default();
    for entry in doc.get("files").and_then(|v| v.as_array()).into_iter().flatten() {
        if entry.get("_type").and_then(|v| v.as_str()) != Some("matl") {
            continue;
        }
        let Some(rel) = entry.get("_path").and_then(|v| v.as_str()) else { continue };
        let json = assets.join(without_extension(rel, ".json"));
        let body = std::fs::read_to_string(&json).map_err(|_| Error::NoExportFile { name: rel.to_string(), file: "json" })?;
        let raw = parse_material_json(&body)?;
        let guid = match entry.get("$guid") {
            Some(serde_json::Value::Number(n)) => n.as_u64(),
            Some(serde_json::Value::String(h)) => u64::from_str_radix(h.trim_start_matches("0x").trim_start_matches("0X"), 16).ok(),
            _ => None,
        };
        let guid = match guid {
            Some(g) => g,
            None => crate::pak::string_to_guid(rel).map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?,
        };
        let mut info = rpak::MaterialInfo::resolve(raw, |slot| {
            u64::from_str_radix(slot.strip_prefix("0x").or_else(|| slot.strip_prefix("0X"))?, 16).ok()
        });
        info.guid = guid;
        info.path = rel.replace('\\', "/");
        tables.materials.insert(normalize_material(rel), to_rir_material(info));
    }
    Ok(tables)
}

/// Everything the compiler reads that `tables` does not carry, which it would
/// otherwise compile on its documented defaults without a word. A tool
/// material is left out: without a table entry the compiler draws nothing for
/// it (`csg::nodraw` takes every `tools/` name as nodraw) and collision reads
/// its contents by name, so its default is its meaning, not a fallback.
pub fn unresolved(tables: &AssetTables, doc: &MapDoc) -> Referenced {
    let want = drawn(doc);
    let tool = |m: &str| {
        let m = m.to_ascii_lowercase();
        m.starts_with("tools/") || m.starts_with("tools\\")
    };
    Referenced {
        materials: want.materials.into_iter().filter(|m| !tables.materials.contains_key(m) && !tool(m)).collect(),
        models: want.models.into_iter().filter(|m| !tables.models.contains_key(m)).collect(),
    }
}

/// Whether `pak` is one the map `own` builds: its map pak, the client
/// perm/temp paks and the loading screen.
fn is_own_pak(pak: &Path, own: &str) -> bool {
    let own = own.to_ascii_lowercase();
    if own.is_empty() {
        return false;
    }
    let stem = pak.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    stem == own || stem == format!("{own}_client_perm") || stem == format!("{own}_client_temp") || stem == format!("{own}_loadscreen")
}

/// The directory and the files a batched export of `row` holds under `root`,
/// or None when it holds none. An interrupted export can leave an empty file
/// behind, and that is not an export.
fn batched_files(kind: &str, row: &AssetRow, root: &Path) -> Option<(PathBuf, Vec<String>)> {
    let (from, base) = match kind {
        "mdl_" => (export_dir(root, row.stem()), base_name(row.stem()).to_string()),
        "shds" => (root.join("shaderset"), format!("0x{:X}", row.guid)),
        "shdr" => (root.join("shader"), format!("0x{:X}", row.guid)),
        _ => {
            let leaf = row.file_name();
            (export_dir(root, row.stem()), leaf.split('.').next().unwrap_or(leaf).to_string())
        }
    };
    let mut files = Vec::new();
    for e in std::fs::read_dir(&from).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let next = name.as_bytes().get(base.len()).copied();
        if !name.starts_with(&base) || !matches!(next, Some(b'.') | Some(b'_')) {
            continue;
        }
        match e.metadata() {
            Ok(m) if m.is_file() && m.len() > 0 => files.push(name),
            Ok(m) if m.is_file() => return None,
            _ => {}
        }
    }
    (!files.is_empty()).then_some((from, files))
}

/// An asset path with its extension replaced, which is where the tool writes the
/// asset and what a manifest entry opens.
pub fn without_extension(path: &str, ext: &str) -> String {
    match path.rsplit_once('.') {
        Some((head, _)) if !head.is_empty() => format!("{head}{ext}"),
        _ => format!("{path}{ext}"),
    }
}

/// The file name of an asset, without its directory.
pub fn base_name(stem: &str) -> &str {
    stem.rsplit('/').next().unwrap_or(stem)
}

/// The directory the asset tool writes one asset's own files into. A model gets
/// a folder named after itself, because the vertex groups it loads from travel
/// beside it; every other kind lands in the mirror of the asset's own path.
pub fn export_dir(scratch: &Path, stem: &str) -> PathBuf {
    if let Some((kind, rest)) = stem.split_once('/') {
        if kind == "mdl" {
            return scratch.join(kind).join(base_name(rest));
        }
    }
    scratch.join(stem).parent().map(Path::to_path_buf).unwrap_or_else(|| scratch.to_path_buf())
}

/// One file an export was supposed to write and did not.
fn read_asset(path: &Path, row: &AssetRow, file: &'static str) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|_| Error::NoExportFile { name: row.path.clone(), file })
}

/// A material path as both sides name it: no `material/` prefix, no extension,
/// lower case, forward slashed.
pub fn normalize_material(name: &str) -> String {
    let mut s = name.replace('\\', "/").to_ascii_lowercase();
    for suffix in [".rpak", ".json"] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.to_string();
        }
    }
    for prefix in ["material/", "models/", "model/"] {
        if let Some(stripped) = s.strip_prefix(prefix) {
            s = stripped.to_string();
        }
    }
    s
}

fn to_rir_material(info: rpak::MaterialInfo) -> MaterialInfo {
    let textures = info.textures.iter().map(|(slot, guid)| (format!("{slot}"), *guid)).collect();
    MaterialInfo {
        path: info.path,
        guid: info.guid,
        shader_set: format!("{:#x}", info.shader_set),
        vertex_format: match info.vertex_format {
            rpak::VertexFormat::Unlit => VertexFormat::Unlit,
            rpak::VertexFormat::LitFlat => VertexFormat::LitFlat,
            rpak::VertexFormat::LitBump => VertexFormat::LitBump,
            rpak::VertexFormat::UnlitTs => VertexFormat::UnlitTs,
        },
        surface_prop: info.surface_prop,
        translucent: info.translucent,
        decal: info.decal,
        nodraw: info.nodraw,
        textures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdoc::model::{Brush, BrushFace, Mesh, Object, Prop, ScatterRule, Terrain, TerrainLayer};

    fn obj(kind: ObjectKind) -> Object {
        Object { id: uuid::Uuid::new_v4(), name: "o".into(), layer: uuid::Uuid::new_v4(), parent: None, transform: Default::default(), hidden: false, locked: false, kind }
    }

    fn face(material: &str) -> BrushFace {
        BrushFace { material: material.into(), ..Default::default() }
    }

    fn prop(model: &str) -> Prop {
        Prop { model: model.into(), skin: 0, static_prop: true, solid: true, cast_shadows: true, fade_distance: 0.0, keyvalues: Default::default() }
    }

    fn layer(material: &str) -> TerrainLayer {
        TerrainLayer {
            name: "base".into(),
            material: material.into(),
            weights: "terrain/w.u8".into(),
            uv_scale: 1.0,
            scatter: vec![ScatterRule {
                models: vec!["mdl/props/crate.rmdl".into()],
                density: 1.0,
                min_weight: 0.5,
                slope_deg: [0.0, 90.0],
                height: [0.0, 1000.0],
                scale: [1.0, 1.0],
                align_to_normal: 1.0,
                seed: 1,
                static_prop: true,
            }],
        }
    }

    fn mesh(map: Vec<(String, String)>) -> Mesh {
        Mesh {
            source: "meshes/a.glb".into(),
            material_map: map.into_iter().collect(),
            collision: rdoc::model::CollisionMode::Triangles,
            contents: 0,
            cast_shadows: true,
            lightmap_scale: 0.0,
            render: true,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        }
    }

    fn terrain(layers: Vec<TerrainLayer>) -> Terrain {
        Terrain {
            heights: "terrain/h.f32".into(),
            resolution: [2, 2],
            cell_size: 32.0,
            layers,
            chunk_size: 64,
            collision: true,
            contents: 1,
            lightmap_scale: 0.0,
            recipe: None,
        }
    }

    fn row(kind: &str, path: &str, guid: u64) -> AssetRow {
        AssetRow { kind: kind.into(), pak_version: 1, rsx_version: 1, guid, pak: "common.rpak".into(), path: path.into() }
    }

    #[test]
    fn a_document_names_every_material_and_model_it_uses() {
        let mut doc = MapDoc::default();
        doc.objects.push(obj(ObjectKind::Brush(Brush { faces: vec![face("world/dev/dev_gray_512"), face("world/dev/dev_gray_512"), face("")], ..Default::default() })));
        doc.objects.push(obj(ObjectKind::Mesh(mesh(vec![("a".into(), "models/x/y".into())]))));
        doc.objects.push(obj(ObjectKind::Terrain(terrain(vec![layer("world/dev/dev_ground_512")]))));
        doc.objects.push(obj(ObjectKind::Prop(prop("mdl/props/crate.rmdl"))));
        let r = referenced(&doc);
        assert_eq!(
            r.materials.iter().cloned().collect::<Vec<_>>(),
            vec!["models/x/y", "world/dev/dev_gray_512", "world/dev/dev_ground_512"]
        );
        let rooms = rcomp::environment::basement_models(&doc);
        // Without R5ED_BASEMENT a map gets no rooms, so there is nothing more to check.
        if std::env::var_os("R5ED_BASEMENT").is_none() {
            return;
        }
        assert!(!rooms.is_empty(), "the rooms every map gets draw models of their own");
        let mut want: Vec<String> = rooms.clone();
        want.push("mdl/props/crate.rmdl".into());
        want.sort();
        want.dedup();
        assert_eq!(r.models.iter().cloned().collect::<Vec<_>>(), want);
        assert!(!r.is_empty());
        let empty = referenced(&MapDoc::default());
        assert!(empty.materials.is_empty());
        assert_eq!(empty.models.len(), rooms.len());
    }

    #[test]
    fn material_names_normalize_to_the_installs_own_form() {
        assert_eq!(normalize_material("world/dev/dev_gray_512"), "world/dev/dev_gray_512");
        assert_eq!(normalize_material("material\\world\\dev\\dev_gray_512_colpass_wldc.rpak"), "world/dev/dev_gray_512_colpass_wldc");
        assert_eq!(normalize_material("MATERIAL/world/dev/x.json"), "world/dev/x");
        assert_eq!(normalize_material("models/world/dev/y"), "world/dev/y");
    }

    /// The two shapes the asset tool writes, checked against the paths real
    /// exports land at: a model in a folder of its own, everything else in the
    /// mirror of the asset's path.
    #[test]
    fn an_export_lands_where_the_asset_tool_writes_it() {
        let scratch = Path::new("s");
        assert_eq!(base_name("mdl/props/flask/prop_flask"), "prop_flask");
        assert_eq!(base_name("prop_flask"), "prop_flask");
        assert_eq!(
            export_dir(scratch, "mdl/props/flask/prop_flask"),
            Path::new("s/mdl/prop_flask"),
            "a model keeps a folder of its own so the vertex groups sit beside it"
        );
        assert_eq!(
            export_dir(scratch, "material/models/world/dev/dev_gray_512_colpass_wldc"),
            Path::new("s/material/models/world/dev")
        );
        assert_eq!(export_dir(scratch, "texture/0x556001E2EBAFD73E"), Path::new("s/texture"));
        assert_eq!(export_dir(scratch, "lonely"), Path::new("s"));
        // A directory that merely starts with `mdl` is a mirror like any other.
        assert_eq!(export_dir(scratch, "mdl_extra/x"), Path::new("s/mdl_extra"));
    }

    /// A model whose export the tool did not write says which file was missing,
    /// rather than surfacing a bare io error the caller cannot act on.
    #[test]
    fn a_missing_export_file_names_the_asset_and_the_file() {
        let row = row("mdl_", "mdl/props/flask/prop_flask.rmdl", 0x1234);
        let err = read_asset(Path::new("no/such/prop_flask.rmdl"), &row, "rmdl").unwrap_err();
        assert!(matches!(err, Error::NoExportFile { ref name, file: "rmdl" } if name == "mdl/props/flask/prop_flask.rmdl"), "{err}");
    }

    /// A Source skybox draws its six faces under the same shell: the packager
    /// carries the face materials and the sky material, and no vista model.
    #[test]
    fn a_source_sky_names_its_faces_and_no_vista() {
        let mut doc = MapDoc::default();
        doc.meta.source_sky = Some("tides".into());
        let r = referenced(&doc);
        for m in rcomp::sky::face_materials("tides") {
            assert!(r.materials.contains(&m), "the packager carries {m}");
        }
        assert!(r.materials.contains(rcomp::meshes::SKY_MATERIAL));
        assert!(r.models.iter().all(|m| !m.contains("vistas")), "no vista model is packed");
        let sky = rcomp::sky_vista(&doc);
        assert_eq!(sky, None, "a Source sky takes the default vista's place");
    }

    /// The variant rule, against the rows a shipped pak actually carries.
    #[test]
    fn a_material_resolves_to_its_world_variant() {
        let mut index = AssetIndex::from_parts("g".into(), BTreeMap::new(), Vec::new());
        index.assets.push(row("matl", "material/models/world/dev/dev_gray_512_colpass_wldc.rpak", 0x6964));
        index.assets.push(row("matl", "material/models/world/dev/dev_gray_512_wldc.rpak", 0x6963));
        index.assets.push(row("matl", "material/world/dev/dev_gray_512_colpass_wldc.rpak", 0x6965));
        index.assets.push(row("matl", "material/world/dev/dev_gray_512_wldc.rpak", 0x6966));
        index.assets.push(row("matl", "material/world/dev/dev_gray_512_rgdc.rpak", 0x6967));
        index.assets.push(row("matl", "material/world/dev/dev_gray_512_colpass_rgdp.rpak", 0x85d0));
        index.assets.push(row("matl", "material/world/dev/dev_blue_512_colpass_wldc.rpak", 0x85d0_0001));
        index.assets.push(row("txtr", "texture/world/dev/dev_gray.dds.rpak", 7));
        let install = Install {
            game_root: PathBuf::from("g"),
            cache: PathBuf::from("c"),
            rsx: Rsx::new("rsx.exe"),
            paks: Vec::new(),
            index,
            surface_props: BTreeMap::new(),
            batch: Default::default(),
        };
        let hit = install.material_row("world/dev/dev_gray_512").expect("resolved");
        assert_eq!(hit.guid, 0x6966, "the world surface itself wins over its model twin and its colpass");
        assert_eq!(install.material_row("world/dev/dev_blue_512").unwrap().guid, 0x85d0_0001);
        assert_eq!(install.material_row("world/dev/dev_gray_512_colpass_rgdp").unwrap().guid, 0x85d0, "an exact name wins");
        assert!(install.material_row("world/dev/not_there").is_none());
        assert!(install.material_row("").is_none());
        assert_eq!(install.texture_row(7).unwrap().path, "texture/world/dev/dev_gray.dds.rpak");
        assert!(install.texture_row(8).is_none());
        assert_eq!(install.texture_guid("0x00000000000000FF"), Some(255));
        assert_eq!(install.texture_guid("world/dev/dev_gray"), Some(7), "resolved by file name");
        assert!(install.texture_guid("nope/none").is_none());
        assert!(install.surface_props.is_empty());
        assert!(install.pak_of(&row("matl", "x", 1)).is_none());
    }

    fn info(path: &str, guid: u64) -> MaterialInfo {
        MaterialInfo {
            path: path.into(),
            guid,
            shader_set: "0x1".into(),
            vertex_format: VertexFormat::LitBump,
            surface_prop: "concrete".into(),
            translucent: false,
            decal: false,
            nodraw: false,
            textures: Default::default(),
        }
    }

    fn model_info(path: &str, guid: u64) -> ModelInfo {
        ModelInfo {
            path: path.into(),
            guid,
            bounds_min: glam::Vec3::splat(-1.0),
            bounds_max: glam::Vec3::splat(1.0),
            has_collision: false,
            resident_paks: Vec::new(),
        }
    }

    /// A full table keys materials by their normalized asset path; a document's
    /// names resolve through it the way the install resolves them.
    #[test]
    fn a_full_table_serves_a_documents_names_like_the_install() {
        let mut full = AssetTables::default();
        for (raw, guid) in [
            ("material/models/world/dev/dev_gray_512_wldc.rpak", 1u64),
            ("material/world/dev/dev_gray_512_colpass_wldc.rpak", 2),
            ("material/world/dev/dev_gray_512_wldc.rpak", 3),
            ("material/world/obj/x/concrete.rpak", 4),
        ] {
            full.materials.insert(normalize_material(raw), info(raw, guid));
        }
        full.models.insert("mdl/vistas/eden_se.rmdl".into(), model_info("mdl/vistas/eden_se.rmdl", 9));
        full.surface_props.insert("concrete".into(), 3);
        let mut doc = MapDoc::default();
        doc.objects.push(obj(ObjectKind::Mesh(mesh(vec![
            ("a".into(), "world/dev/dev_gray_512".into()),
            ("b".into(), "world/obj/x/concrete".into()),
            ("c".into(), "world/obj/x/missing".into()),
        ]))));
        doc.objects.push(obj(ObjectKind::Prop(prop("mdl/vistas/eden_se"))));
        let t = resolve_tables(&full, &doc);
        assert_eq!(t.materials["world/dev/dev_gray_512"].guid, 3, "the world surface wins, as in the install");
        assert_eq!(t.materials["world/dev/dev_gray_512"].path, "world/dev/dev_gray_512", "entries carry the document's name");
        assert_eq!(t.materials["world/obj/x/concrete"].guid, 4);
        assert!(!t.materials.contains_key("world/obj/x/missing"));
        assert_eq!(t.models["mdl/vistas/eden_se"].guid, 9, "a model path without its extension resolves");
        assert_eq!(t.surface_props.get("concrete"), Some(&3));
        let left = unresolved(&t, &doc);
        assert_eq!(left.materials.into_iter().collect::<Vec<_>>(), vec!["world/obj/x/missing".to_string()]);
        assert!(left.models.is_empty());
    }

    /// A per-map table (keyed by the document's own names) resolves to itself,
    /// so a table written for one map and read back changes nothing.
    #[test]
    fn a_map_table_round_trips() {
        let mut doc = MapDoc::default();
        doc.objects.push(obj(ObjectKind::Brush(Brush { faces: vec![face("tools/toolstrigger")], ..Default::default() })));
        doc.meta.sky = Some("mdl/vistas/eden_se.rmdl".into());
        let mut map = AssetTables::default();
        map.materials.insert("tools/toolstrigger".into(), info("tools/toolstrigger", 5));
        map.materials.insert(rcomp::meshes::SKY_MATERIAL.into(), info(rcomp::meshes::SKY_MATERIAL, 6));
        map.models.insert("mdl/vistas/eden_se.rmdl".into(), model_info("mdl/vistas/eden_se.rmdl", 7));
        let t = resolve_tables(&map, &doc);
        assert_eq!(t, map);
        assert!(unresolved(&t, &doc).is_empty());
        let none = resolve_tables(&AssetTables::default(), &doc);
        let left = unresolved(&none, &doc);
        assert!(left.materials.is_empty(), "tool materials (the trigger, the sky shell) are drawn by name: {left:?}");
        assert_eq!(left.models.len(), 1, "the vista is read too");
    }

    /// A source pak's materials come off its manifest and the json beside each
    /// entry, the way objmat writes them, with no asset tool; a moved copy
    /// finds its assets beside itself, and a document's name resolves to the
    /// world variant with the guid the manifest pinned.
    #[test]
    fn a_source_paks_materials_are_read_without_the_asset_tool() {
        let dir = std::env::temp_dir().join(format!("r5ed-srcpak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mat = dir.join("assets/material/world/obj/x");
        std::fs::create_dir_all(&mat).unwrap();
        let body = r#"{ "name": "world/obj/x/blue", "depthStencilFlags": "0x17", "uberBufferFlags": "0x0",
            "surfaceProp": "concrete", "shaderType": "wldc", "shaderSet": "0x1D2E3F",
            "$textures": { "0": "0x00000000000000AA", "1": "0x00000000000000BB" } }"#;
        std::fs::write(mat.join("blue_wldc.json"), body).unwrap();
        std::fs::write(mat.join("red_wldc.json"), body.replace("blue", "red")).unwrap();
        let manifest = serde_json::json!({
            "version": 8, "name": "x_src", "assetsDir": "Z:/nowhere/assets/",
            "files": [
                { "_type": "txtr", "_path": "texture/a.rpak" },
                { "_type": "matl", "_path": "material/world/obj/x/blue_wldc.rpak", "$guid": 4242u64 },
                { "_type": "matl", "_path": "material/world/obj/x/red_wldc.rpak" }
            ]
        });
        std::fs::write(dir.join("x_src.json"), manifest.to_string()).unwrap();
        let src = src_pak_tables(&dir.join("x_src.json")).unwrap();
        assert_eq!(src.materials.len(), 2, "{:?}", src.materials.keys());
        let mut doc = MapDoc::default();
        doc.objects.push(obj(ObjectKind::Mesh(mesh(vec![("a".into(), "world/obj/x/blue".into()), ("b".into(), "world/obj/x/red".into())]))));
        let t = resolve_tables(&src, &doc);
        let blue = &t.materials["world/obj/x/blue"];
        assert_eq!(blue.guid, 4242, "the manifest's pinned guid wins");
        assert_eq!(blue.vertex_format, VertexFormat::LitBump);
        assert_eq!(blue.surface_prop, "concrete");
        assert!(!blue.translucent && !blue.nodraw);
        assert_eq!(blue.textures.len(), 2);
        assert_eq!(
            t.materials["world/obj/x/red"].guid,
            crate::pak::string_to_guid("material/world/obj/x/red_wldc.rpak").unwrap(),
            "an unpinned entry takes the path's hash, as the packer does"
        );
        assert!(unresolved(&t, &doc).is_empty());
        std::fs::remove_file(mat.join("red_wldc.json")).unwrap();
        assert!(src_pak_tables(&dir.join("x_src.json")).is_err(), "a named material without its json is an error");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
