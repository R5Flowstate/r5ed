//! The per-install asset index: the parsed asset list, keyed by the sizes and
//! mtimes of the paks it was built from, stored as JSON under `index/`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::asset_list::AssetRow;
use crate::error::{Error, Result};

/// What makes one index entry: the pak's size and last-write time, in
/// milliseconds since the epoch. A change in either invalidates the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PakStamp {
    pub size: u64,
    pub mtime_ms: u128,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetIndex {
    pub game_root: String,
    pub paks: BTreeMap<String, PakStamp>,
    pub assets: Vec<AssetRow>,
    /// Lookups by path and guid, built on first use: a map asks for thousands
    /// of assets and a scan per request is what made model loading crawl.
    #[serde(skip)]
    pub lookup: OnceLock<Lookup>,
}

#[derive(Clone, Debug, Default)]
pub struct Lookup {
    /// Lowercase `/`-separated path to the first row with it.
    by_path: HashMap<String, usize>,
    /// Lowercase path to the first model row with it.
    model_by_path: HashMap<String, usize>,
    /// Guid to the first material row with it.
    material_by_guid: HashMap<u64, usize>,
    /// Guid to the first texture row with it.
    texture_by_guid: HashMap<u64, usize>,
    /// Guid to every row with it, one per pak that carries the asset.
    all_by_guid: HashMap<u64, Vec<usize>>,
}

fn is_model(kind: &str) -> bool {
    kind == "mdl_" || kind == "mdl"
}

fn stamp_of(p: &Path) -> Result<PakStamp> {
    let md = std::fs::metadata(p).map_err(|e| Error::Io(p.to_path_buf(), e))?;
    let mtime_ms = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis());
    Ok(PakStamp { size: md.len(), mtime_ms })
}

fn pak_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

impl AssetIndex {
    /// Builds an index from an asset list, stamping each named pak.
    pub fn build(game_root: &Path, paks: &[PathBuf], rows: Vec<AssetRow>) -> Result<Self> {
        let mut stamps = BTreeMap::new();
        for p in paks {
            stamps.insert(pak_name(p), stamp_of(p)?);
        }
        Ok(Self::from_parts(game_root.to_string_lossy().into_owned(), stamps, rows))
    }

    pub fn from_parts(game_root: String, paks: BTreeMap<String, PakStamp>, assets: Vec<AssetRow>) -> Self {
        Self { game_root, paks, assets, lookup: OnceLock::new() }
    }

    /// The index at `index_path` brought up to date with `paks`: a pak whose
    /// stamp is unchanged keeps its rows, and only new or changed paks are
    /// listed again. Saves when anything changed.
    pub fn refresh(
        index_path: &Path,
        game_root: &Path,
        paks: &[PathBuf],
        list: impl FnOnce(&[PathBuf]) -> Result<Vec<AssetRow>>,
    ) -> Result<Self> {
        let root = game_root.to_string_lossy().into_owned();
        let old = Self::load(index_path).ok().filter(|i| i.game_root == root);
        let mut stamps = BTreeMap::new();
        for p in paks {
            stamps.insert(pak_name(p), stamp_of(p)?);
        }
        if let Some(old) = &old {
            if old.matches(&stamps) {
                return Ok(Self::from_parts(root, stamps, old.assets.clone()));
            }
        }
        let kept: Vec<&PathBuf> = paks
            .iter()
            .filter(|p| old.as_ref().is_some_and(|o| o.paks.get(&pak_name(p)) == stamps.get(&pak_name(p))))
            .collect();
        let stale: Vec<PathBuf> = paks.iter().filter(|p| !kept.contains(p)).cloned().collect();
        let keep_names: std::collections::HashSet<String> = kept.iter().map(|p| pak_name(p)).collect();
        let mut rows: Vec<AssetRow> = match &old {
            Some(o) => o.assets.iter().filter(|r| keep_names.contains(&r.pak)).cloned().collect(),
            None => Vec::new(),
        };
        if !stale.is_empty() {
            let listed = list(&stale)?;
            // A pak the lister could not read is left unstamped, so the next
            // refresh tries it again instead of trusting an empty listing.
            for p in &stale {
                let name = pak_name(p);
                if !listed.iter().any(|r| r.pak == name) {
                    stamps.remove(&name);
                }
            }
            rows.extend(listed);
        }
        let idx = Self::from_parts(root, stamps, rows);
        idx.save(index_path)?;
        Ok(idx)
    }

    fn lookup(&self) -> &Lookup {
        self.lookup.get_or_init(|| {
            // An asset several paks carry is read from the smallest of them:
            // an export loads its whole pak, and the shared common.rpak alone
            // takes seconds to load where a map pak takes one.
            let size = |i: usize| self.paks.get(&self.assets[i].pak).map_or(u64::MAX, |s| s.size);
            fn keep<K: std::hash::Hash + Eq>(map: &mut HashMap<K, usize>, key: K, i: usize, size: &dyn Fn(usize) -> u64) {
                match map.get_mut(&key) {
                    Some(at) if size(i) < size(*at) => *at = i,
                    Some(_) => {}
                    None => {
                        map.insert(key, i);
                    }
                }
            }
            let mut l = Lookup::default();
            for (i, r) in self.assets.iter().enumerate() {
                let p = r.path.replace('\\', "/").to_ascii_lowercase();
                if is_model(&r.kind) {
                    keep(&mut l.model_by_path, p.clone(), i, &size);
                }
                if r.kind == "matl" {
                    keep(&mut l.material_by_guid, r.guid, i, &size);
                }
                if r.kind == "txtr" {
                    keep(&mut l.texture_by_guid, r.guid, i, &size);
                }
                keep(&mut l.by_path, p, i, &size);
                l.all_by_guid.entry(r.guid).or_default().push(i);
            }
            l
        })
    }

    /// Every pak's copy of an asset, by its guid and kind.
    pub fn copies(&self, row: &AssetRow) -> Vec<&AssetRow> {
        self.lookup()
            .all_by_guid
            .get(&row.guid)
            .map(|v| v.iter().map(|i| &self.assets[*i]).filter(|r| r.kind == row.kind).collect())
            .unwrap_or_default()
    }

    /// The size of a pak the index read, when it read it.
    pub fn pak_size(&self, pak: &str) -> Option<u64> {
        self.paks.get(pak).map(|s| s.size)
    }

    /// The texture asset with this guid.
    pub fn texture_by_guid(&self, guid: u64) -> Option<&AssetRow> {
        self.lookup().texture_by_guid.get(&guid).map(|i| &self.assets[*i])
    }

    /// The material asset with this guid.
    pub fn material_by_guid(&self, guid: u64) -> Option<&AssetRow> {
        self.lookup().material_by_guid.get(&guid).map(|i| &self.assets[*i])
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
        serde_json::from_str(&text).map_err(|e| Error::Json(e.to_string()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| Error::Io(p.to_path_buf(), e))?;
        }
        let body = serde_json::to_string(self).map_err(|e| Error::Json(e.to_string()))?;
        std::fs::write(path, body).map_err(|e| Error::Io(path.to_path_buf(), e))
    }

    /// True when `stamps` matches what this index was built from.
    pub fn matches(&self, stamps: &BTreeMap<String, PakStamp>) -> bool {
        self.paks.len() == stamps.len() && self.paks.iter().all(|(k, v)| stamps.get(k) == Some(v))
    }

    /// The current on-disk stamp of every pak under `<game_root>/paks/<pakdir>`.
    pub fn scan_paks(game_root: &Path, pak_dirs: &[&str]) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for d in pak_dirs {
            let dir = game_root.join("paks").join(d);
            if !dir.is_dir() {
                continue;
            }
            let rd = std::fs::read_dir(&dir).map_err(|e| Error::Io(dir.clone(), e))?;
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|s| s.eq_ignore_ascii_case("rpak")).unwrap_or(false) && indexed_pak(game_root, &p) {
                    out.push(p);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn total(&self) -> usize {
        self.assets.len()
    }

    /// Substring search over asset paths, optionally restricted to one kind.
    /// The result is ordered by path so paging is stable.
    pub fn search(&self, query: &str, kind: Option<&str>, offset: usize, limit: usize) -> (usize, Vec<&AssetRow>) {
        self.search_in(query, kind, "", offset, limit)
    }

    /// `search` narrowed to paths under `prefix`, a folder such as `mdl/props/`.
    pub fn search_in(&self, query: &str, kind: Option<&str>, prefix: &str, offset: usize, limit: usize) -> (usize, Vec<&AssetRow>) {
        let q = query.to_ascii_lowercase();
        let pre = prefix.to_ascii_lowercase();
        let mut hits: Vec<&AssetRow> = self
            .assets
            .iter()
            .filter(|r| kind.is_none_or(|k| r.kind == k))
            .filter(|r| {
                let path = r.path.to_ascii_lowercase();
                path.starts_with(&pre) && (q.is_empty() || path.contains(&q))
            })
            .collect();
        hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.guid.cmp(&b.guid)));
        let total = hits.len();
        let page: Vec<&AssetRow> = hits.into_iter().skip(offset).take(limit).collect();
        (total, page)
    }

    /// Finds an asset by exact path, case-insensitively.
    pub fn find(&self, path: &str) -> Option<&AssetRow> {
        let p = path.replace('\\', "/").to_ascii_lowercase();
        self.lookup().by_path.get(&p).map(|i| &self.assets[*i])
    }

    /// Finds a model by its stem with either extension the engine writes.
    pub fn find_model(&self, path: &str) -> Option<&AssetRow> {
        let want = path.replace('\\', "/").to_ascii_lowercase();
        let l = self.lookup();
        l.model_by_path
            .get(&want)
            .or_else(|| l.model_by_path.get(&format!("{want}.rmdl")))
            .or_else(|| l.by_path.get(&want))
            .map(|i| &self.assets[*i])
    }

    /// The folders directly under `prefix`, each with how many assets of
    /// `kind` it holds at any depth, sorted by name. Paths compare lower case.
    pub fn folders(&self, kind: Option<&str>, prefix: &str) -> Vec<(String, usize)> {
        let pre = prefix.to_ascii_lowercase();
        let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for r in self.assets.iter().filter(|r| kind.is_none_or(|k| r.kind == k)) {
            let path = r.path.to_ascii_lowercase();
            let Some(rest) = path.strip_prefix(&pre) else { continue };
            if let Some(slash) = rest.find('/') {
                *counts.entry(rest[..slash].to_string()).or_default() += 1;
            }
        }
        counts.into_iter().collect()
    }

    pub fn kinds(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.assets.iter().map(|r| r.kind.as_str()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// Whether the index reads a pak: the shared `common*` paks, and every map's
/// own pak, which is the one the install keeps a `<stem>_client_perm.rpak`
/// beside in `paks/Win64`. A `name(01).rpak` is a backup copy the game never
/// loads. `R5ED_INDEX_PAKS` (comma-separated pak stems) adds more.
pub fn indexed_pak(game_root: &Path, path: &Path) -> bool {
    let name = path.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if name.is_empty() || name.contains('(') {
        return false;
    }
    if name.starts_with("common") && !name.contains("_client_") {
        return true;
    }
    let win64 = game_root.join("paks").join("Win64");
    if !name.contains("_client_") && !name.ends_with("_loadscreen") && win64.join(format!("{name}_client_perm.rpak")).is_file() {
        return true;
    }
    let extra = std::env::var("R5ED_INDEX_PAKS").unwrap_or_default();
    extra.split(',').map(|w| w.trim().to_ascii_lowercase()).any(|w| !w.is_empty() && name == w)
}

#[cfg(test)]
mod pak_filter_tests {
    use super::indexed_pak;

    #[test]
    fn indexes_the_shared_paks_and_each_maps_own_pak() {
        let root = std::env::temp_dir().join(format!("r5ed_pakfilter_{}", std::process::id()));
        let win64 = root.join("paks").join("Win64");
        std::fs::create_dir_all(&win64).unwrap();
        for f in ["mp_rr_x.rpak", "mp_rr_x_client_perm.rpak", "mp_rr_x_client_temp.rpak", "mp_rr_x_loadscreen.rpak", "mp_rr_y.rpak"] {
            std::fs::write(win64.join(f), b"").unwrap();
        }
        for keep in ["common.rpak", "common_flowstate.rpak", "mp_rr_x.rpak"] {
            assert!(indexed_pak(&root, &win64.join(keep)), "{keep}");
        }
        for skip in ["common(01).rpak", "mp_rr_x_client_perm.rpak", "mp_rr_x_loadscreen.rpak", "mp_rr_y.rpak", "charm_x.rpak", "subtitles_english.rpak"] {
            assert!(!indexed_pak(&root, &win64.join(skip)), "{skip}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    fn row(pak: &str, path: &str, kind: &str, guid: u64) -> AssetRow {
        AssetRow { kind: kind.into(), pak_version: 8, rsx_version: 8, guid, pak: pak.into(), path: path.into() }
    }

    #[test]
    fn folders_count_assets_below_a_prefix() {
        let index = AssetIndex {
            assets: vec![
                row("a", "mdl/props/crate/crate_a.rmdl", "mdl_", 1),
                row("a", "mdl/props/crate/crate_b.rmdl", "mdl_", 2),
                row("a", "mdl/props/barrel.rmdl", "mdl_", 3),
                row("a", "mdl/Vehicle/car.rmdl", "mdl_", 4),
                row("a", "material/world/wall.rpak", "matl", 5),
            ],
            game_root: String::new(),
            paks: BTreeMap::new(),
            lookup: OnceLock::new(),
        };
        assert_eq!(index.folders(Some("mdl_"), ""), vec![("mdl".to_string(), 4)]);
        assert_eq!(index.folders(Some("mdl_"), "mdl/"), vec![("props".to_string(), 3), ("vehicle".to_string(), 1)]);
        assert_eq!(index.folders(Some("mdl_"), "MDL/Props/"), vec![("crate".to_string(), 2)]);
        let (total, rows) = index.search_in("", Some("mdl_"), "mdl/props/", 0, 10);
        assert_eq!(total, 3);
        assert_eq!(rows[0].path, "mdl/props/barrel.rmdl");
        let (total, _) = index.search_in("crate_b", Some("mdl_"), "mdl/props/", 0, 10);
        assert_eq!(total, 1);
    }

    #[test]
    fn only_changed_paks_are_listed_again_and_lookups_find_them() {
        let dir = std::env::temp_dir().join(format!("r5ed_refresh_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.rpak"), dir.join("b.rpak"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let path = dir.join("assets.json");
        let paks = vec![a.clone(), b.clone()];
        let first = AssetIndex::refresh(&path, &dir, &paks, |stale| {
            assert_eq!(stale.len(), 2, "a fresh index lists every pak");
            Ok(vec![row("a.rpak", "mdl/x/one.rmdl", "mdl_", 1), row("b.rpak", "material/m", "matl", 7)])
        })
        .unwrap();
        assert_eq!(first.total(), 2);
        std::fs::write(&b, b"bigger").unwrap();
        let second = AssetIndex::refresh(&path, &dir, &paks, |stale| {
            assert_eq!(stale, &[b.clone()], "only the changed pak is listed");
            Ok(vec![row("b.rpak", "material/n", "matl", 8)])
        })
        .unwrap();
        assert_eq!(second.find_model("mdl/x/one").map(|r| r.guid), Some(1), "found without the extension");
        assert_eq!(second.find_model(r"MDL\X\ONE.rmdl").map(|r| r.guid), Some(1));
        assert_eq!(second.material_by_guid(8).map(|r| r.path.as_str()), Some("material/n"));
        assert!(second.material_by_guid(7).is_none(), "the changed pak's old rows are gone");
        let third = AssetIndex::refresh(&path, &dir, &paks, |_| panic!("nothing changed")).unwrap();
        let mut both = AssetIndex::from_parts(
            third.game_root.clone(),
            BTreeMap::from([("big.rpak".to_string(), PakStamp { size: 900, mtime_ms: 0 }), ("small.rpak".to_string(), PakStamp { size: 9, mtime_ms: 0 })]),
            vec![row("big.rpak", "texture/0x5", "txtr", 5), row("small.rpak", "texture/0x5", "txtr", 5)],
        );
        assert_eq!(both.texture_by_guid(5).map(|r| r.pak.as_str()), Some("small.rpak"), "the smaller pak loads faster");
        both.assets.reverse();
        both.lookup = OnceLock::new();
        assert_eq!(both.find("texture/0x5").map(|r| r.pak.as_str()), Some("small.rpak"), "whatever order the rows came in");
        assert_eq!(third.total(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
