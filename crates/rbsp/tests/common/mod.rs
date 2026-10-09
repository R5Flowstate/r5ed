//! Fixture discovery shared by the integration tests. Paths come from the
//! environment so no machine-local path is baked into the source.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rbsp::Bsp;

pub fn fixtures_root() -> PathBuf {
    PathBuf::from(std::env::var("R5ED_FIXTURES").unwrap_or_else(|_| "r5ed-fixtures".to_string()))
}

/// Every shipped dedicated-server map, as `(map stem, directory holding it)`.
/// A few directories ship a map under a different stem than the directory.
pub fn dedi_maps() -> &'static Vec<(String, PathBuf)> {
    static CACHE: OnceLock<Vec<(String, PathBuf)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let root = fixtures_root().join("dedi");
        let mut out: Vec<(String, PathBuf)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&root) {
            for e in rd.flatten() {
                let maps = e.path().join("maps");
                if !maps.is_dir() {
                    continue;
                }
                if let Ok(files) = std::fs::read_dir(&maps) {
                    for f in files.flatten() {
                        let n = f.file_name().to_string_lossy().to_string();
                        if n.ends_with(".bsp") {
                            out.push((n.trim_end_matches(".bsp").to_string(), e.path()));
                        }
                    }
                }
            }
        }
        out.sort();
        out
    })
}

pub fn client_dir() -> PathBuf {
    fixtures_root().join("client").join("_test_s21_in")
}

pub fn client_map() -> Option<(String, PathBuf)> {
    let dir = client_dir();
    if !dir.is_dir() {
        return None;
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".bsp"))
        .collect();
    names.sort();
    let name = names.first()?.trim_end_matches(".bsp").to_string();
    Some((name, dir))
}

/// The Titanfall 2 source map, whose path only ever comes from the environment.
pub fn tf2_map() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("R5ED_TF2_BSP").ok()?);
    p.is_file().then_some(p)
}

/// One loaded map: the container, its entity partition files and its path.
#[derive(Clone)]
pub struct MapCase {
    pub name: String,
    pub version: rbsp::Version,
    pub bsp: Bsp,
    pub ents: BTreeMap<String, Vec<u8>>,
    pub path: PathBuf,
}

impl MapCase {
    pub fn load(name: &str, dir: &Path) -> Option<MapCase> {
        let (bsp, ents) = Bsp::read_dir_map(dir, name).ok()?;
        let version = bsp.version_kind()?;
        let path = bsp_path(dir, name);
        Some(MapCase { name: format!("{name} v{}", version.number()), version, bsp, ents, path })
    }
}

fn bsp_path(dir: &Path, name: &str) -> PathBuf {
    let direct = dir.join(format!("{name}.bsp"));
    if direct.is_file() {
        return direct;
    }
    dir.join("maps").join(format!("{name}.bsp"))
}

pub fn client_case() -> Option<&'static MapCase> {
    static CACHE: OnceLock<Option<MapCase>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let (name, dir) = client_map()?;
            MapCase::load(&name, &dir)
        })
        .as_ref()
}

pub fn tf2_case() -> Option<&'static MapCase> {
    static CACHE: OnceLock<Option<MapCase>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let p = tf2_map()?;
            let name = p.file_stem()?.to_string_lossy().to_string();
            let bsp = Bsp::read_path(&p).ok()?;
            let version = bsp.version_kind()?;
            Some(MapCase {
                name: format!("{name} v{}", version.number()),
                version,
                bsp,
                ents: BTreeMap::new(),
                path: p,
            })
        })
        .as_ref()
}

/// Every fixture map, sidecar and v37 source included. Loading the source map
/// costs a few hundred megabytes, so the list is built once.
pub fn all_cases() -> &'static Vec<MapCase> {
    static CACHE: OnceLock<Vec<MapCase>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut out: Vec<MapCase> = Vec::new();
        for (name, dir) in dedi_maps() {
            if let Some(c) = MapCase::load(&name, &dir) {
                out.push(c);
            }
        }
        if let Some(c) = client_case() {
            out.push(c.clone());
        }
        if let Some(c) = tf2_case() {
            out.push(c.clone());
        }
        out
    })
}

/// A scratch directory unique to the calling test, emptied on the way in.
pub fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("r5ed-rbsp-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// A small deterministic generator, so a failing fuzz case is reproducible.
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 32) as u32
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            self.next_u32() as usize % n
        }
    }
}

/// A lump has to survive a pack byte for byte, with one exception: the game
/// lump's block pointers are absolute against the packed file, so packing
/// shifts each of them by the distance the lump itself moved and leaves every
/// other byte alone.
pub fn assert_lump_survived(name: &str, id: u16, before: &[u8], after: &[u8], was: u32, now: u32) {
    if id != rbsp::ids::GAME_LUMP {
        assert_eq!(after, before, "{name}: lump {id:#04x} changed across a pack");
        return;
    }
    let dir = rbsp::lumps::game_lump::directory(before).unwrap_or_else(|e| panic!("{name}: game lump: {e}"));
    let mut want = before.to_vec();
    let delta = now as i64 - was as i64;
    for i in 0..dir.len() {
        let at = 4 + i * rbsp::lumps::game_lump::DIRECTORY_ENTRY_SIZE + 8;
        let v = i32::from_le_bytes(before[at..at + 4].try_into().unwrap()) as i64 + delta;
        want[at..at + 4].copy_from_slice(&(v as i32).to_le_bytes());
    }
    assert_eq!(after, &want[..], "{name}: lump {id:#04x} is not a pure pointer shift");
}
