//! The per-install asset library: the pak index, the preview caches and the
//! typed lookups the host serves.
//!
//! Only the RSX CLI can read these paks, so every byte that comes out of one
//! lands in a cache directory keyed by a hash of the asset's identity. Nothing
//! here ever writes into the game install.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::asset_list::AssetRow;
use crate::error::{Error, Result};
use crate::glb::model_to_glb_named;
use crate::index::AssetIndex;
use crate::material::{parse_material_json, MaterialInfo};
use crate::model_info::ModelInfo;
use crate::png::encode_rgba8;
use crate::rmdl::parse_model;
use crate::rson::{read_surface_props_file, SurfaceProps};
use crate::thumbnail;
use crate::Rsx;

/// The pak directories a client install keeps its assets in.
pub const CLIENT_PAK_DIRS: [&str; 2] = ["Win64", "opt"];
pub const DEDI_PAK_DIRS: [&str; 1] = ["Win64_server"];

/// The shared paks every map pak is loaded with in game.
const CLOSURE_PAKS: [&str; 4] = ["common.rpak", "common_early.rpak", "common_mp.rpak", "mp_lobby.rpak"];

/// Subdirectory of the cache the RSX CLI exports into.
const EXPORT_SCRATCH: &str = "rsx";

/// One row of a search result, flattened for the wire.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AssetHit {
    pub kind: String,
    pub path: String,
    pub guid: u64,
}

/// One map the install carries, and which halves of it are present.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MapEntry {
    pub stem: String,
    pub has_client: bool,
    pub has_dedi: bool,
}

/// FNV-1a, run twice with different offset bases so the cache key is 128 bits.
fn cache_hash(identity: &str) -> String {
    fn once(seed: u64, s: &str) -> u64 {
        let mut h = seed;
        for &b in s.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    }
    format!("{:016x}{:016x}", once(0xcbf2_9ce4_8422_2325, identity), once(0x9E37_79B9_7F4A_7C15, identity))
}

/// One document material as a prefetch resolved it: the material asset that
/// answered and its albedo texture guid, now cached, or why there is none.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct MaterialPrefetch {
    pub name: String,
    pub path: Option<String>,
    pub albedo: Option<String>,
    pub translucent: bool,
    pub error: Option<String>,
}

/// What a prefetch did: models already exported or converted, models it
/// exported now, and paths the index does not know.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Prefetch {
    pub ready: usize,
    pub exported: usize,
    pub missing: Vec<String>,
}

pub struct Library {
    game_root: PathBuf,
    cache_root: PathBuf,
    rsx: Rsx,
    paks: Vec<PathBuf>,
    /// Pak file name -> the directory under `paks` it lives in.
    pak_dirs: BTreeMap<String, String>,
    index: AssetIndex,
    surface_props: SurfaceProps,
}

impl Library {
    /// Opens the library, building the index if the cached one is stale.
    pub fn open(game_root: &Path, cache_root: &Path, rsx: Rsx) -> Result<Self> {
        let mut paks = AssetIndex::scan_paks(game_root, &CLIENT_PAK_DIRS)?;
        paks.extend(AssetIndex::scan_paks(game_root, &DEDI_PAK_DIRS)?);
        paks.sort();
        let mut pak_dirs = BTreeMap::new();
        for p in &paks {
            let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let dir = p
                .parent()
                .and_then(|d| d.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            pak_dirs.entry(name).or_insert(dir);
        }
        let surface_props = load_surface_props(game_root);
        let index_path = cache_root.join("index").join("assets.json");
        let scratch = cache_root.join(EXPORT_SCRATCH);
        let index = AssetIndex::refresh(&index_path, game_root, &paks, |stale| rsx.list(stale, &scratch))?;
        Ok(Self { game_root: game_root.to_path_buf(), cache_root: cache_root.to_path_buf(), rsx, paks, pak_dirs, index, surface_props })
    }

    pub fn game_root(&self) -> &Path {
        &self.game_root
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub fn index(&self) -> &AssetIndex {
        &self.index
    }

    /// The surface prop table, empty when the install has no script tree.
    pub fn surface_props(&self) -> &SurfaceProps {
        &self.surface_props
    }

    /// Re-reads every pak and rewrites the cached index.
    pub fn rebuild_index(&mut self) -> Result<usize> {
        let rows = self.rsx.list(&self.paks, &self.cache_root.join(EXPORT_SCRATCH))?;
        let idx = AssetIndex::build(&self.game_root, &self.paks, rows)?;
        idx.save(&self.cache_root.join("index").join("assets.json"))?;
        self.index = idx;
        Ok(self.index.total())
    }

    /// Substring search over asset paths, optionally narrowed to one kind.
    pub fn search(&self, query: &str, kind: Option<&str>, offset: usize, limit: usize) -> (usize, Vec<AssetHit>) {
        let (total, rows) = self.index.search(query, kind, offset, limit.clamp(1, 200));
        (total, rows.into_iter().map(hit).collect())
    }

    /// The model export of one asset, parsed. `path` may carry or omit `.rmdl`.
    pub fn parse_model_asset(&self, path: &str) -> Result<(ModelInfo, crate::rmdl::RmdlModel)> {
        let row = self
            .index
            .find_model(path)
            .ok_or_else(|| Error::NotIndexed { path: path.to_string() })?
            .clone();
        let (rmdl, vg) = self.model_files(&row).ok_or_else(|| Error::Malformed {
            what: "model export",
            msg: format!("{} has no exported .rmdl and .vg", row.path),
        })?;
        let rmdl = read_or_err(&rmdl)?;
        let vg = read_or_err(&vg)?;
        let model = parse_model(&rmdl, &vg)?;
        let paks = self.paks_of(&row);
        Ok((ModelInfo::from_rmdl(&model, &row.path, row.guid, paks), model))
    }

    /// A glb of the model's LOD0, cached under `models/`.
    pub fn model_glb(&self, path: &str) -> Result<Vec<u8>> {
        let row = self
            .index
            .find_model(path)
            .ok_or_else(|| Error::NotIndexed { path: path.to_string() })?
            .clone();
        let cached = self.glb_cache_path(&row);
        if let Ok(bytes) = std::fs::read(&cached) {
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
        self.export_model(&row)?;
        let (_, model) = self.parse_model_asset(&row.path)?;
        // A slot's guid names a material asset; the page looks materials up by
        // that path, without the `material/` prefix and `.rpak`.
        let glb = model_to_glb_named(&model, &|guid| {
            self.index
                .material_by_guid(guid)
                .map(|r| r.stem().strip_prefix("material/").unwrap_or(r.stem()).to_string())
        })?;
        write_cache(&cached, &glb)?;
        Ok(glb)
    }

    /// A 128x128 shaded render of the model as a PNG, cached under `thumbs/`.
    pub fn thumbnail_png(&self, path: &str) -> Result<Vec<u8>> {
        let row = self
            .index
            .find_model(path)
            .ok_or_else(|| Error::NotIndexed { path: path.to_string() })?
            .clone();
        let key = cache_hash(&format!("thumb\x1f{}\x1f{:016x}", row.path, row.guid));
        let cached = self.cache_root.join("thumbs").join(format!("{key}.png"));
        if let Ok(bytes) = std::fs::read(&cached) {
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
        self.export_model(&row)?;
        let (_, model) = self.parse_model_asset(&row.path)?;
        let img = thumbnail::render_thumbnail(&model)?;
        let png = encode_rgba8(&img.pixels, img.width as u32, img.height as u32)?;
        write_cache(&cached, &png)?;
        Ok(png)
    }

    /// One material's compiler-facing description.
    pub fn material(&self, path: &str) -> Result<MaterialInfo> {
        let row = self
            .index
            .find(path)
            .filter(|r| r.kind == "matl")
            .ok_or_else(|| Error::NotIndexed { path: path.to_string() })?
            .clone();
        self.export_materials(std::slice::from_ref(&row))?;
        self.read_material(&row)
    }

    /// Where a pak's materials are exported, by full asset path.
    fn material_dir(&self, row: &AssetRow) -> PathBuf {
        self.pak_scratch("materials-src", row)
    }

    fn pak_scratch(&self, kind: &str, row: &AssetRow) -> PathBuf {
        let pak = row.pak.strip_suffix(".rpak").unwrap_or(&row.pak);
        let dir = self.pak_dirs.get(&row.pak).map_or("Win64", String::as_str);
        self.cache_root.join(kind).join(dir).join(pak)
    }

    fn material_json(&self, row: &AssetRow) -> PathBuf {
        self.material_dir(row).join(format!("{}.json", row.stem()))
    }

    /// The first file `path_of` names for the asset or any other pak's copy of
    /// it that is on disk: a batch reads each asset from whichever pak covers
    /// it best, which need not be the pak a lookup answered with.
    fn exported_copy(&self, row: &AssetRow, path_of: impl Fn(&AssetRow) -> PathBuf) -> Option<(AssetRow, PathBuf)> {
        std::iter::once(row)
            .chain(self.index.copies(row))
            .map(|r| (r.clone(), path_of(r)))
            .find(|(_, p)| p.is_file())
    }

    /// Exports every material not exported yet, one run per pak.
    fn export_materials(&self, rows: &[AssetRow]) -> Result<()> {
        let wanted: Vec<&AssetRow> = rows.iter().filter(|r| self.exported_copy(r, |c| self.material_json(c)).is_none()).collect();
        let by_pak = self.plan_paks(&wanted);
        for rows in by_pak.values() {
            let rows: Vec<&AssetRow> = rows.iter().collect();
            let guids: Vec<u64> = rows.iter().map(|r| r.guid).collect();
            let (pak, dir) = (self.pak_path(rows[0])?, self.material_dir(rows[0]));
            isolate_failures(&guids, &mut |g| self.rsx.export_materials(std::slice::from_ref(&pak), g, &dir))?;
        }
        Ok(())
    }

    fn read_material(&self, row: &AssetRow) -> Result<MaterialInfo> {
        let path = self.exported_copy(row, |c| self.material_json(c)).map_or_else(|| self.material_json(row), |(_, p)| p);
        let json = read_or_err(&path)?;
        let raw = parse_material_json(&json_str(&json)?)?;
        let mut info = MaterialInfo::resolve(raw, |p| self.texture_guid_for(p));
        info.guid = row.guid;
        Ok(info)
    }

    fn texture_cache_path(&self, guid: u64) -> PathBuf {
        let key = cache_hash(&format!("dds\x1f{guid:016x}"));
        self.cache_root.join("textures").join(format!("{key}.dds"))
    }

    /// The highest mip of a texture as raw DDS bytes, cached under `textures/`.
    pub fn texture_dds(&self, guid: u64) -> Result<Vec<u8>> {
        let cached = self.texture_cache_path(guid);
        if let Ok(bytes) = std::fs::read(&cached) {
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
        if self.index.texture_by_guid(guid).is_none() {
            return Err(Error::NotIndexed { path: format!("0x{guid:016x}") });
        }
        let failed = self.cache_textures(&[guid])?;
        if let Some((_, why)) = failed.into_iter().next() {
            return Err(Error::Malformed { what: "texture export", msg: why });
        }
        read_or_err(&cached)
    }

    /// Exports and caches the highest mip of every texture not cached yet: one
    /// run per owning pak, then one more over the shared paks for whatever the
    /// owning pak alone could not supply. Returns the guids that still failed.
    fn cache_textures(&self, guids: &[u64]) -> Result<Vec<(u64, String)>> {
        let mut wanted: Vec<&AssetRow> = Vec::new();
        let mut failed: Vec<(u64, String)> = Vec::new();
        let mut known = self.load_unexportable();
        for &g in guids {
            if self.texture_cache_path(g).is_file() {
                continue;
            }
            match self.index.texture_by_guid(g) {
                Some(r) if known.get(&g) == Some(&self.pak_stamp_key(r)) => {
                    failed.push((g, format!("{} exported no DDS", r.path)));
                }
                Some(r) => wanted.push(r),
                None => failed.push((g, format!("0x{g:016x} is not in the index"))),
            }
        }
        let by_pak = self.plan_paks(&wanted);
        let mut left: Vec<AssetRow> = Vec::new();
        for rows in by_pak.values() {
            let dir = self.pak_scratch("textures-src", &rows[0]);
            let refs: Vec<&AssetRow> = rows.iter().collect();
            left.extend(self.export_texture_batch(&[self.pak_path(&rows[0])?], &refs, &dir)?.into_iter().cloned());
        }
        // What its own pak could not supply gets one more run with the shared
        // paks loaded beside every pak involved; one run, since each shared
        // pak takes seconds to load.
        if !left.is_empty() {
            let mut paks = self.closure_paks(&left[0])?;
            for r in &left {
                let own = self.pak_path(r)?;
                if !paks.contains(&own) {
                    paks.push(own);
                }
            }
            let dir = self.cache_root.join("textures-src").join("closure");
            let refs: Vec<&AssetRow> = left.iter().collect();
            let still = self.export_texture_batch(&paks, &refs, &dir)?;
            for r in &still {
                known.insert(r.guid, self.pak_stamp_key(r));
            }
            failed.extend(still.iter().map(|r| (r.guid, format!("{} exported no DDS", r.path))));
            self.save_unexportable(&known);
        }
        Ok(failed)
    }

    /// Textures the tool exported nothing for, by guid, each with the stamp of
    /// the pak it was read from: retrying one costs a shared pak load every
    /// time, so it is skipped until its pak changes.
    fn unexportable_path(&self) -> PathBuf {
        self.cache_root.join("index").join("unexportable_textures.json")
    }

    fn load_unexportable(&self) -> BTreeMap<u64, String> {
        std::fs::read(self.unexportable_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_unexportable(&self, known: &BTreeMap<u64, String>) {
        if let Ok(body) = serde_json::to_vec(known) {
            let _ = write_cache(&self.unexportable_path(), &body);
        }
    }

    /// The stamps of every pak that carries the asset, so the key is the same
    /// whichever copy a batch happened to read.
    fn pak_stamp_key(&self, row: &AssetRow) -> String {
        let mut parts: Vec<String> = std::iter::once(row)
            .chain(self.index.copies(row))
            .map(|r| match self.index.paks.get(&r.pak) {
                Some(s) => format!("{}:{}:{}", r.pak, s.size, s.mtime_ms),
                None => r.pak.clone(),
            })
            .collect();
        parts.sort();
        parts.dedup();
        parts.join(",")
    }

    /// Exports a batch of textures from `paks` and caches each one's largest
    /// mip. Returns the rows that produced no usable DDS.
    fn export_texture_batch<'a>(&self, paks: &[PathBuf], rows: &[&'a AssetRow], dir: &Path) -> Result<Vec<&'a AssetRow>> {
        let guids: Vec<u64> = rows.iter().map(|r| r.guid).collect();
        isolate_failures(&guids, &mut |g| self.rsx.export_textures(paks, g, dir))?;
        let mips = highest_mips(&dir.join("texture"), ".dds");
        let left: Vec<&AssetRow> = rows
            .iter()
            .copied()
            .filter(|r| {
                let Some(p) = mips.get(&format!("0x{:X}", r.guid)) else { return true };
                // Serving BC bytes verbatim is the whole point, so the header
                // is only read to prove the payload is a real DDS.
                std::fs::read(p)
                    .map_err(|e| e.to_string())
                    .and_then(|b| {
                        crate::dds::parse_dds_header(&b).map_err(|e| e.to_string())?;
                        write_cache(&self.texture_cache_path(r.guid), &b).map_err(|e| e.to_string())
                    })
                    .is_err()
            })
            .collect();
        let _ = std::fs::remove_dir_all(dir.join("texture"));
        Ok(left)
    }

    /// Resolves document material names to their albedo textures in one pass:
    /// each name is tried with each shader suffix in turn, every candidate
    /// material is exported one run per pak, and every albedo one run per pak,
    /// so the page's texture requests that follow are cache reads.
    pub fn prefetch_materials(&self, names: &[String], suffixes: &[String]) -> Result<Vec<MaterialPrefetch>> {
        let mut candidates: Vec<Vec<AssetRow>> = Vec::with_capacity(names.len());
        for name in names {
            let key = name.replace('\\', "/").to_ascii_lowercase();
            let rows: Vec<AssetRow> = suffixes
                .iter()
                .filter_map(|sfx| self.index.find(&format!("material/{key}{sfx}.rpak")))
                .filter(|r| r.kind == "matl")
                .cloned()
                .collect();
            candidates.push(rows);
        }
        let all: Vec<AssetRow> = candidates.iter().flatten().cloned().collect();
        self.export_materials(&all)?;
        let mut out: Vec<MaterialPrefetch> = Vec::with_capacity(names.len());
        for (name, rows) in names.iter().zip(&candidates) {
            let mut item = MaterialPrefetch { name: name.clone(), ..Default::default() };
            if rows.is_empty() {
                item.error = Some("not in the indexed paks".into());
            }
            for row in rows {
                match self.read_material(row) {
                    Ok(info) => match info.textures.get(&0) {
                        Some(g) => {
                            item.path = Some(row.path.clone());
                            item.albedo = Some(format!("0x{g:016x}"));
                            item.translucent = info.translucent;
                            item.error = None;
                            break;
                        }
                        None => item.error = Some(format!("{} has no albedo the host can resolve", row.path)),
                    },
                    Err(e) => item.error = Some(format!("{}: {e}", row.path)),
                }
            }
            out.push(item);
        }
        let albedos: Vec<u64> = out
            .iter()
            .filter_map(|i| i.albedo.as_deref())
            .filter_map(|g| u64::from_str_radix(g.trim_start_matches("0x"), 16).ok())
            .collect();
        let failed: BTreeMap<u64, String> = self.cache_textures(&albedos)?.into_iter().collect();
        for item in &mut out {
            let Some(g) = item.albedo.as_deref().and_then(|g| u64::from_str_radix(g.trim_start_matches("0x"), 16).ok()) else { continue };
            if let Some(why) = failed.get(&g) {
                item.error = Some(format!("albedo 0x{g:016x}: {why}"));
                item.albedo = None;
            }
        }
        Ok(out)
    }

    /// Every map stem the install carries, with the halves it has.
    pub fn maps(&self) -> Vec<MapEntry> {
        let mut out: BTreeMap<String, MapEntry> = BTreeMap::new();
        for row in &self.index.assets {
            if row.kind != "wrap" {
                continue;
            }
            let Some(rest) = row.path.strip_prefix("maps/") else { continue };
            let stem = rest.strip_suffix(".bsp").unwrap_or(rest);
            if stem.is_empty() || stem.contains('/') {
                continue;
            }
            let dir = self.pak_dirs.get(&row.pak).map(String::as_str).unwrap_or("");
            let e = out
                .entry(stem.to_string())
                .or_insert_with(|| MapEntry { stem: stem.to_string(), has_client: false, has_dedi: false });
            match dir {
                "Win64_server" => e.has_dedi = true,
                "Win64" | "opt" => e.has_client = true,
                _ => {}
            }
        }
        out.into_values().collect()
    }

    /// The pak a row was read out of.
    fn pak_path(&self, row: &AssetRow) -> Result<PathBuf> {
        self.paks
            .iter()
            .find(|p| p.file_name().map(|s| s.to_string_lossy() == row.pak).unwrap_or(false))
            .cloned()
            .ok_or_else(|| Error::NotIndexed { path: row.pak.clone() })
    }

    /// The pak that carries a map's wrap assets, preferring the client half
    /// because a reference layer is drawn from the client map. The asset is
    /// listed under its `.bsp` name and lives in the `<stem>.rpak` beside it.
    pub fn map_pak(&self, stem: &str) -> Option<PathBuf> {
        if stem.is_empty() || stem.contains('/') || stem.contains('\\') {
            return None;
        }
        let want = format!("maps/{stem}.bsp");
        let row = self
            .index
            .assets
            .iter()
            .filter(|r| r.kind == "wrap" && r.path.eq_ignore_ascii_case(&want))
            .min_by_key(|r| match self.pak_dirs.get(&r.pak).map(String::as_str) {
                Some("Win64") | Some("opt") => 0,
                Some("Win64_server") => 1,
                _ => 2,
            })?;
        self.pak_path(row).ok()
    }

    fn paks_of(&self, row: &AssetRow) -> Vec<String> {
        let dir = self.pak_dirs.get(&row.pak).cloned().unwrap_or_default();
        vec![format!("{dir}/{}", row.pak)]
    }

    /// Which pak each asset of a batch is read from. An export loads its whole
    /// pak, so the batch should touch as few paks as it can: the pak carrying
    /// the most of what is still unassigned goes first, a tie to the smaller
    /// pak, until every asset has one. A map's own pak wins its map's batch.
    fn plan_paks(&self, wanted: &[&AssetRow]) -> BTreeMap<String, Vec<AssetRow>> {
        let copies: Vec<Vec<&AssetRow>> = wanted
            .iter()
            .map(|r| {
                let c = self.index.copies(r);
                if c.is_empty() { vec![*r] } else { c }
            })
            .collect();
        let mut out: BTreeMap<String, Vec<AssetRow>> = BTreeMap::new();
        let mut left: Vec<usize> = (0..wanted.len()).collect();
        while !left.is_empty() {
            let mut count: BTreeMap<&str, usize> = BTreeMap::new();
            for &i in &left {
                for c in &copies[i] {
                    *count.entry(c.pak.as_str()).or_default() += 1;
                }
            }
            let size = |p: &str| self.index.pak_size(p).unwrap_or(u64::MAX);
            let Some(best) = count.iter().max_by(|a, b| a.1.cmp(b.1).then(size(b.0).cmp(&size(a.0)))).map(|(p, _)| p.to_string()) else { break };
            let mut rest = Vec::new();
            for i in left {
                match copies[i].iter().find(|c| c.pak == best) {
                    Some(c) => out.entry(best.clone()).or_default().push((*c).clone()),
                    None => rest.push(i),
                }
            }
            left = rest;
        }
        out
    }

    /// Where a pak's models are exported, by full asset path: one directory per
    /// pak, so one export run fills it for any number of that pak's models.
    fn model_dir(&self, row: &AssetRow) -> PathBuf {
        let pak = row.pak.strip_suffix(".rpak").unwrap_or(&row.pak);
        let dir = self.pak_dirs.get(&row.pak).map_or("Win64", String::as_str);
        self.cache_root.join("models-src").join(dir).join(pak)
    }

    /// The model files of the asset or of another pak's copy of it, when one
    /// was exported: the `.rmdl` and the `.vg` beside it.
    fn model_files(&self, row: &AssetRow) -> Option<(PathBuf, PathBuf)> {
        let (_, rmdl) = self.exported_copy(row, |c| self.model_dir(c).join(format!("{}.rmdl", c.stem())))?;
        let vg = rmdl.with_extension("vg");
        vg.is_file().then_some((rmdl, vg))
    }

    fn model_exported(&self, row: &AssetRow) -> bool {
        self.model_files(row).is_some()
    }

    fn export_model(&self, row: &AssetRow) -> Result<()> {
        if self.model_exported(row) {
            return Ok(());
        }
        let dir = self.model_dir(row);
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io(dir.clone(), e))?;
        self.rsx.export_models(&self.pak_path(row)?, &[row.guid], &dir)
    }

    fn glb_cache_path(&self, row: &AssetRow) -> PathBuf {
        let key = cache_hash(&format!("glb-named\x1f{}\x1f{:016x}", row.path, row.guid));
        self.cache_root.join("models").join(format!("{key}.glb"))
    }

    /// Exports every named model not yet exported, one RSX run per pak rather
    /// than one per model: each run decompresses its pak once, which is most of
    /// what a model export costs.
    pub fn prefetch_models(&self, paths: &[String]) -> Result<Prefetch> {
        let mut out = Prefetch::default();
        let mut wanted: Vec<&AssetRow> = Vec::new();
        for path in paths {
            let Some(row) = self.index.find_model(path) else {
                out.missing.push(path.clone());
                continue;
            };
            if self.glb_cache_path(row).is_file() || self.model_exported(row) {
                out.ready += 1;
                continue;
            }
            wanted.push(row);
        }
        let by_pak = self.plan_paks(&wanted);
        for rows in by_pak.values() {
            let dir = self.model_dir(&rows[0]);
            std::fs::create_dir_all(&dir).map_err(|e| Error::Io(dir.clone(), e))?;
            let guids: Vec<u64> = rows.iter().map(|r| r.guid).collect();
            let pak = self.pak_path(&rows[0])?;
            isolate_failures(&guids, &mut |g| self.rsx.export_models(&pak, g, &dir))?;
            out.exported += rows.iter().filter(|r| self.model_exported(r)).count();
        }
        Ok(out)
    }

    /// The paks an export loads: the shared ones every map pak depends on, then
    /// the asset's own pak. An asset read alone can name data (streamed mips,
    /// shared textures) that only these paks together supply.
    fn closure_paks(&self, row: &AssetRow) -> Result<Vec<PathBuf>> {
        let own = self.pak_path(row)?;
        let mut out: Vec<PathBuf> = CLOSURE_PAKS
            .iter()
            .filter_map(|name| self.paks.iter().find(|p| p.file_name().is_some_and(|f| f.eq_ignore_ascii_case(name))).cloned())
            .filter(|p| *p != own)
            .collect();
        out.push(own);
        Ok(out)
    }

    /// A material's `$textures` entry can be an exported path rather than a
    /// guid; resolve it back through the index.
    fn texture_guid_for(&self, exported: &str) -> Option<u64> {
        if let Some(hex) = exported.strip_prefix("0x").or_else(|| exported.strip_prefix("0X")) {
            return u64::from_str_radix(hex, 16).ok();
        }
        let stem = exported.strip_suffix(".rpak").unwrap_or(exported).replace('\\', "/");
        let full = if stem.starts_with("texture/") { stem.clone() } else { format!("texture/{stem}") };
        if let Some(r) = self.index.assets.iter().find(|r| r.kind == "txtr" && r.stem() == full) {
            return Some(r.guid);
        }
        // Named textures come back as an export-relative path whose directory
        // part is not an asset path, so fall back to the texture's own name.
        let base = stem.rsplit('/').next().unwrap_or(&stem).to_ascii_lowercase();
        self.index
            .assets
            .iter()
            .find(|r| r.kind == "txtr" && r.file_name().rsplit_once('.').map(|(a, _)| a).unwrap_or(r.file_name()).eq_ignore_ascii_case(&base))
            .map(|r| r.guid)
    }
}

/// Runs a batch export, and when the tool dies on the batch, runs each half
/// again until the assets it cannot export stand alone. One asset that crashes
/// the tool then costs itself, not the whole batch; the caller sees it as not
/// exported. Returns the guids that failed on their own.
fn isolate_failures(guids: &[u64], export: &mut dyn FnMut(&[u64]) -> Result<()>) -> Result<Vec<u64>> {
    if guids.is_empty() {
        return Ok(Vec::new());
    }
    match export(guids) {
        Ok(()) => Ok(Vec::new()),
        Err(Error::RsxExit { .. }) if guids.len() == 1 => Ok(guids.to_vec()),
        Err(Error::RsxExit { .. }) => {
            let (a, b) = guids.split_at(guids.len() / 2);
            let mut failed = isolate_failures(a, export)?;
            failed.extend(isolate_failures(b, export)?);
            Ok(failed)
        }
        Err(e) => Err(e),
    }
}

fn hit(r: &AssetRow) -> AssetHit {
    AssetHit { kind: r.kind.clone(), path: r.path.clone(), guid: r.guid }
}

fn read_or_err(p: &Path) -> Result<Vec<u8>> {
    std::fs::read(p).map_err(|e| Error::Io(p.to_path_buf(), e))
}

fn write_cache(p: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::Io(d.to_path_buf(), e))?;
    }
    std::fs::write(p, bytes).map_err(|e| Error::Io(p.to_path_buf(), e))
}

fn json_str(b: &[u8]) -> Result<String> {
    String::from_utf8(b.to_vec()).map_err(|e| Error::Malformed { what: "material json", msg: e.to_string() })
}

/// The largest mip of every texture exported into `dir`, by base name
/// (`0x<GUID>`), from one directory scan. RSX numbers the mip files from the
/// smallest up, so the largest is the highest-numbered one that exists.
fn highest_mips(dir: &Path, ext: &str) -> BTreeMap<String, PathBuf> {
    let mut best: BTreeMap<String, (u32, PathBuf)> = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return BTreeMap::new() };
    for e in rd.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().map(|s| s.to_string_lossy().into_owned()) else { continue };
        let Some(stem) = name.strip_suffix(ext) else { continue };
        let Some((base, level)) = stem.rsplit_once("_level") else { continue };
        let Ok(n) = level.parse::<u32>() else { continue };
        if best.get(base).is_none_or(|(b, _)| n > *b) {
            best.insert(base.to_string(), (n, p));
        }
    }
    best.into_iter().map(|(k, (_, p))| (k, p)).collect()
}

fn load_surface_props(game_root: &Path) -> SurfaceProps {
    read_surface_props_file(&game_root.join("platform").join("scripts").join("surfaceproperties.rson")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::PakStamp;

    #[test]
    fn the_cache_key_is_stable_and_wide() {
        let a = cache_hash("mdl/props/x.rmdl");
        assert_eq!(a, cache_hash("mdl/props/x.rmdl"));
        assert_eq!(a.len(), 32);
        assert_ne!(a, cache_hash("mdl/props/y.rmdl"));
    }

    #[test]
    fn a_batch_the_tool_dies_on_is_split_until_the_bad_asset_stands_alone() {
        let mut runs = 0;
        let failed = isolate_failures(&[1, 2, 3, 4, 5, 6, 7], &mut |g| {
            runs += 1;
            if g.contains(&5) {
                Err(Error::RsxExit { status: -1073740791, stderr: String::new() })
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(failed, vec![5]);
        assert!(runs <= 7, "{runs} runs for one bad asset in seven");
        let other = isolate_failures(&[1, 2], &mut |_| Err(Error::Json("no".into())));
        assert!(other.is_err(), "a failure that is not the tool dying is not retried");
    }

    #[test]
    fn a_batch_reads_from_the_pak_that_covers_most_of_it() {
        let mut l = two_pak_library("cover");
        let rows = vec![
            row("texture/0x1.rpak", "txtr", 1, "client.rpak"),
            row("texture/0x1.rpak", "txtr", 1, "server.rpak"),
            row("texture/0x2.rpak", "txtr", 2, "server.rpak"),
            row("texture/0x3.rpak", "txtr", 3, "client.rpak"),
        ];
        l.index = AssetIndex::from_parts(l.index.game_root.clone(), l.index.paks.clone(), rows.clone());
        let plan = l.plan_paks(&[&rows[0], &rows[2]]);
        assert_eq!(plan.keys().collect::<Vec<_>>(), ["server.rpak"], "one pak carries both, so one pak loads");
        assert_eq!(plan["server.rpak"].len(), 2);
        let plan = l.plan_paks(&[&rows[0], &rows[2], &rows[3]]);
        assert_eq!(plan.values().map(Vec::len).sum::<usize>(), 3, "every asset is assigned once");
    }

    #[test]
    fn a_material_exported_from_another_paks_copy_is_found() {
        let mut l = two_pak_library("copies");
        let rows = vec![
            row("material/world/b_rgdp.rpak", "matl", 20, "client.rpak"),
            row("material/world/b_rgdp.rpak", "matl", 20, "server.rpak"),
            row("texture/0x77.rpak", "txtr", 0x77, "server.rpak"),
        ];
        l.index = AssetIndex::from_parts(l.index.game_root.clone(), l.index.paks.clone(), rows.clone());
        let json = r#"{"name": "world/b", "shaderSet": "0x1", "shaderType": "rgdp", "uberBufferFlags": "0x0", "$textures": {"0": "0x77"}}"#;
        write_cache(&l.material_json(&rows[1]), json.as_bytes()).unwrap();
        write_cache(&l.texture_cache_path(0x77), b"DDS ").unwrap();
        let info = l.read_material(&rows[0]).expect("the server copy stands in");
        assert_eq!(info.textures.get(&0), Some(&0x77));
        let got = l.prefetch_materials(&["world/b".to_string()], &["_rgdp".to_string()]).unwrap();
        assert_eq!(got[0].albedo.as_deref(), Some("0x0000000000000077"), "no export runs for a copy already on disk");
    }

    #[test]
    fn a_texture_that_exports_nothing_is_not_retried_until_its_pak_changes() {
        let mut l = two_pak_library("unexportable");
        let rows = vec![row("texture/0x42.rpak", "txtr", 0x42, "client.rpak")];
        l.index = AssetIndex::from_parts(l.index.game_root.clone(), l.index.paks.clone(), rows.clone());
        let mut known = BTreeMap::new();
        known.insert(0x42u64, l.pak_stamp_key(&rows[0]));
        l.save_unexportable(&known);
        // The tool path is bogus, so any export attempt would be an error.
        let failed = l.cache_textures(&[0x42]).expect("a known failure runs no export");
        assert_eq!(failed.len(), 1);
        known.insert(0x42, "client.rpak:0:0".into());
        l.save_unexportable(&known);
        assert!(l.cache_textures(&[0x42]).is_err(), "a changed pak is tried again");
    }

    #[test]
    fn one_scan_picks_each_textures_largest_mip() {
        let dir = std::env::temp_dir().join(format!("r5ed_mips_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["0xAB_level0.dds", "0xAB_level7.dds", "0xAB_level12.dds", "0xCD_level3.dds", "0xAB.json", "0xEF_levelx.dds"] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        let mips = highest_mips(&dir, ".dds");
        assert_eq!(mips.len(), 2);
        assert!(mips["0xAB"].ends_with("0xAB_level12.dds"), "12 beats 7 as a number, not as text");
        assert!(mips["0xCD"].ends_with("0xCD_level3.dds"));
        assert!(highest_mips(&dir.join("nowhere"), ".dds").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_install_yields_empty_tables_rather_than_an_error() {
        let props = load_surface_props(Path::new("Z:/nowhere/at/all"));
        assert!(props.is_empty());
    }

    fn row(path: &str, kind: &str, guid: u64, pak: &str) -> AssetRow {
        AssetRow {
            path: path.to_string(),
            kind: kind.to_string(),
            guid,
            pak: pak.to_string(),
            pak_version: 1,
            rsx_version: 1,
        }
    }

    /// A library over one client pak and one server pak, with two map wraps in
    /// them, which is the shape the import job resolves against.
    fn two_pak_library(tag: &str) -> Library {
        let dir = std::env::temp_dir().join(format!("r5ed-rpak-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let mut paks = Vec::new();
        for (sub, name) in [("Win64", "client.rpak"), ("Win64_server", "server.rpak")] {
            let p = dir.join("game").join("paks").join(sub).join(name);
            std::fs::create_dir_all(p.parent().unwrap()).expect("pak dir");
            std::fs::write(&p, b"pak").expect("pak");
            paks.push(p);
        }
        let cache = dir.join("cache");
        std::fs::create_dir_all(cache.join("index")).expect("cache");
        let index = AssetIndex::from_parts(
            dir.join("game").to_string_lossy().into_owned(),
            BTreeMap::from([
                (
                    "client.rpak".to_string(),
                    PakStamp { size: 3, mtime_ms: 0 },
                ),
                (
                    "server.rpak".to_string(),
                    PakStamp { size: 3, mtime_ms: 0 },
                ),
            ]),
            vec![
                row("maps/mp_x.bsp", "wrap", 1, "client.rpak"),
                row("maps/mp_x.bsp", "wrap", 2, "server.rpak"),
                row("maps/mp_y.bsp", "wrap", 3, "server.rpak"),
                row("mdl/props/a.rmdl", "mdl_", 4, "client.rpak"),
            ],
        );
        index.save(&cache.join("index").join("assets.json")).expect("save index");
        Library {
            game_root: dir.join("game"),
            cache_root: cache,
            rsx: Rsx::new("rsx"),
            paks,
            pak_dirs: BTreeMap::from([
                ("client.rpak".to_string(), "Win64".to_string()),
                ("server.rpak".to_string(), "Win64_server".to_string()),
            ]),
            index,
            surface_props: SurfaceProps::default(),
        }
    }

    #[test]
    fn a_material_name_takes_the_first_suffix_with_an_albedo() {
        let mut l = two_pak_library("materials");
        let rows = vec![
            row("material/world/a_wldc.rpak", "matl", 10, "client.rpak"),
            row("material/world/a_rgdp.rpak", "matl", 11, "client.rpak"),
            row("texture/0x99.rpak", "txtr", 0x99, "client.rpak"),
        ];
        l.index = AssetIndex::from_parts(l.index.game_root.clone(), l.index.paks.clone(), rows.clone());
        let json = |albedo: Option<&str>| {
            let textures = albedo.map(|g| format!(r#"{{"0": "{g}"}}"#)).unwrap_or_else(|| "{}".into());
            format!(r#"{{"name": "world/a", "shaderSet": "0x1", "shaderType": "rgdp", "uberBufferFlags": "0x0", "$textures": {textures}}}"#)
        };
        for (r, albedo) in [(&rows[0], None), (&rows[1], Some("0x99"))] {
            write_cache(&l.material_json(r), json(albedo).as_bytes()).unwrap();
        }
        // Both are exported and the albedo is cached already, so no tool runs.
        write_cache(&l.texture_cache_path(0x99), b"DDS ").unwrap();
        let suffixes: Vec<String> = ["_wldc", "_rgdp", ""].iter().map(|s| s.to_string()).collect();
        let got = l.prefetch_materials(&[r"World\A".to_string(), "world/none".to_string()], &suffixes).unwrap();
        assert_eq!(got[0].path.as_deref(), Some("material/world/a_rgdp.rpak"), "the first suffix has no albedo");
        assert_eq!(got[0].albedo.as_deref(), Some("0x0000000000000099"));
        assert!(got[0].error.is_none());
        assert_eq!(got[1].error.as_deref(), Some("not in the indexed paks"));
        assert!(got[1].albedo.is_none());
    }

    #[test]
    fn a_map_import_resolves_to_the_pak_that_carries_it() {
        let l = two_pak_library("map-pak");
        let client = l.map_pak("mp_x").expect("mp_x");
        assert!(client.to_string_lossy().contains("Win64"), "{}", client.display());
        assert!(client.to_string_lossy().ends_with("client.rpak"));
        // A map that only the server pak carries resolves there.
        let server = l.map_pak("mp_y").expect("mp_y");
        assert!(server.to_string_lossy().contains("Win64_server"), "{}", server.display());
        assert_eq!(l.map_pak("mp_nope"), None);
        assert_eq!(l.map_pak(""), None);
        let _ = std::fs::remove_dir_all(l.cache_root.parent().unwrap());
    }

    #[test]
    fn the_map_listing_says_which_halves_are_present() {
        let l = two_pak_library("map-list");
        let maps = l.maps();
        assert_eq!(maps.len(), 2);
        let x = maps.iter().find(|m| m.stem == "mp_x").expect("mp_x");
        assert!(x.has_client && x.has_dedi, "{x:?}");
        let y = maps.iter().find(|m| m.stem == "mp_y").expect("mp_y");
        assert!(!y.has_client && y.has_dedi, "{y:?}");
        let _ = std::fs::remove_dir_all(l.cache_root.parent().unwrap());
    }
}
