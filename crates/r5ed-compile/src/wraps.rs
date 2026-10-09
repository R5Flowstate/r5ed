//! The wrap archives a client map ships in.
//!
//! A map is not one archive: the render and collision geometry go in one and the
//! entities, the visibility, the cubemaps and the game lump in the other, which
//! is the split every shipped map uses. A few lumps the engine reads early
//! travel in both. The assignment is therefore a table, and a lump in neither is
//! a build that would silently lose geometry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rbsp::{Bsp, Layout};
use serde_json::{json, Value};

use crate::pak::{self, Entry, Manifest};

/// Lumps the perm archive carries: the render and collision geometry. The two
/// object-reference lumps are here because that is the split a converted map
/// ships with.
pub const PERM: [u16; 47] = [
    0x01, 0x03, 0x05, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x1E, 0x25, 0x26, 0x27, 0x47, 0x49, 0x4A, 0x4F, 0x50,
    0x51, 0x52, 0x61, 0x62, 0x63, 0x64, 0x65, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x71,
    0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7C, 0x7E, 0x7F,
];

/// Lumps the temp archive carries: entities, texture data, visibility, cubemaps
/// and the game lump.
pub const TEMP: [u16; 18] = [
    0x00, 0x02, 0x04, 0x0E, 0x18, 0x23, 0x2A, 0x2B, 0x36, 0x50, 0x51, 0x52, 0x53, 0x55, 0x66, 0x7A, 0x7B, 0x7D,
];

/// The entity partitions a client map carries, in the engine's order.
pub const PARTITIONS: [&str; 5] = ["env", "fx", "script", "snd", "spawn"];

/// The archive name one half of a map's wraps goes in.
pub fn archive_name(stem: &str, label: &str) -> String {
    format!("{stem}_client_{label}")
}

/// Whether the tables name a lump at all. A lump they do not is packed perm and
/// reported, never dropped.
pub fn is_assigned(id: u16) -> bool {
    PERM.contains(&id) || TEMP.contains(&id)
}

/// Which archives a lump id travels in, perm first. An id neither table names
/// goes to perm, which is the archive the engine mounts first.
pub fn sides_of(id: u16) -> Vec<&'static str> {
    let mut out = Vec::new();
    if PERM.contains(&id) || !is_assigned(id) {
        out.push("perm");
    }
    if TEMP.contains(&id) {
        out.push("temp");
    }
    out
}

/// The file name a lump takes inside an archive: the engine reads a client's
/// lumps with the `.client` suffix.
pub fn lump_file(stem: &str, id: u16, suffix: &str) -> String {
    format!("maps/{stem}.bsp.{id:04X}.bsp_lump{suffix}")
}

/// What one archive ended up carrying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Built {
    pub name: String,
    pub path: PathBuf,
    pub files: Vec<String>,
    pub bytes: u64,
    pub assets: u32,
}
/// Copies a map's lumps and partitions into a staging tree, one directory per
/// archive, and returns the file names each manifest will name.
pub fn stage(
    bsp: &Bsp,
    ent_bytes: &BTreeMap<String, Vec<u8>>,
    stem: &str,
    stage: &Path,
) -> Result<Staged> {
    let mut halves: Vec<(&str, Vec<String>)> = vec![("perm", Vec::new()), ("temp", Vec::new())];
    let mut unassigned: Vec<String> = Vec::new();
    if bsp.layout == Layout::Sidecar {
        write_file(&stage.join("perm").join("maps").join(format!("{stem}.bsp")), &bsp.header.to_bytes())?;
        halves[0].1.push(format!("maps/{stem}.bsp"));
    }
    for (id, bytes) in &bsp.lumps {
        if !is_assigned(*id) {
            unassigned.push(format!("{id:04X}"));
        }
        for side in sides_of(*id) {
            let leaf = format!("{stem}.bsp.{id:04X}.bsp_lump.client");
            write_file(&stage.join(side).join("maps").join(&leaf), bytes)?;
            halves[if side == "perm" { 0 } else { 1 }].1.push(lump_file(stem, *id, ".client"));
        }
    }
    for name in PARTITIONS {
        let Some(bytes) = ent_bytes.get(name) else { continue };
        let leaf = format!("{stem}_{name}.ent");
        write_file(&stage.join("temp").join("maps").join(&leaf), bytes)?;
        halves[1].1.push(format!("maps/{leaf}"));
    }
    for (side, files) in &halves {
        if files.is_empty() {
            bail!("the {side} archive would hold no files");
        }
    }
    Ok(Staged { halves, unassigned })
}

/// A staged map: the two halves and the lumps neither table named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub halves: Vec<(&'static str, Vec<String>)>,
    pub unassigned: Vec<String>,
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// Packs both halves of a map's wraps.
pub fn build(
    bsp: &Bsp,
    ent_bytes: &BTreeMap<String, Vec<u8>>,
    stem: &str,
    work: &Path,
    out: &Path,
    repak: &Path,
) -> Result<Packed> {
    let _ = std::fs::remove_dir_all(work);
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let staged = stage(bsp, ent_bytes, stem, work)?;
    let mut built = Vec::new();
    for (side, files) in staged.halves.iter() {
        let name = archive_name(stem, side);
        let mut manifest = Manifest::new(&name, &work.join(side), out);
        for f in files.iter() {
            manifest.push(Entry::new("wrap", f)).map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        let manifest_path = work.join(format!("{name}.json"));
        manifest.save(&manifest_path).map_err(|e| anyhow::anyhow!("{e}"))?;
        let header = pak::pack(repak, &manifest, &manifest_path).map_err(|e| anyhow::anyhow!("{e}"))?;
        let bytes = std::fs::metadata(manifest.output()).map(|m| m.len()).unwrap_or(0);
        built.push(Built { name, path: manifest.output(), files: files.clone(), bytes, assets: header.assets });
    }
    Ok(Packed { archives: built, unassigned: staged.unassigned })
}

/// Both archives, and the lumps neither table named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packed {
    pub archives: Vec<Built>,
    pub unassigned: Vec<String>,
}

/// The report the caller prints.
pub fn report(packed: &Packed) -> Value {
    json!({
        "archives": packed.archives.iter().map(|b| json!({
            "name": b.name,
            "pak": b.path.display().to_string(),
            "bytes": b.bytes,
            "assets": b.assets,
            "files": b.files,
        })).collect::<Vec<_>>(),
        "unassigned": packed.unassigned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lump_tables_are_the_split_every_shipped_map_uses() {
        assert_eq!(PERM.len(), 47);
        assert_eq!(TEMP.len(), 18);
        // The mesh table and its bounds are read before the archives are
        // mounted, so they travel in both halves.
        let both: Vec<u16> = PERM.iter().copied().filter(|i| TEMP.contains(i)).collect();
        assert_eq!(both, vec![0x50, 0x51, 0x52], "the shared lumps are the mesh table and its bounds");
        // The renderer output every map carries is geometry, so it is perm.
        for id in [0x01, 0x03, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x47, 0x49, 0x4A, 0x4F, 0x61, 0x62, 0x63, 0x78, 0x79, 0x7C, 0x7E, 0x7F] {
            assert_eq!(sides_of(id), vec!["perm"], "lump 0x{id:02x}");
        }
        // And the entity, texture, cubemap and game lumps are temp.
        for id in [0x00, 0x02, 0x04, 0x0E, 0x18, 0x23, 0x2A, 0x66, 0x7A, 0x7B, 0x7D] {
            assert_eq!(sides_of(id), vec!["temp"], "lump 0x{id:02x}");
        }
        assert_eq!(sides_of(0x50), vec!["perm", "temp"]);
        // A lump neither table names is packed perm and reported, never dropped.
        assert!(!is_assigned(0x5E));
        assert_eq!(sides_of(0x5E), vec!["perm"]);
    }

    #[test]
    fn a_lump_is_named_the_way_the_engine_looks_it_up() {
        assert_eq!(lump_file("mp_rr_x", 0x4f, ".client"), "maps/mp_rr_x.bsp.004F.bsp_lump.client");
        assert_eq!(lump_file("mp_rr_x", 1, ".server"), "maps/mp_rr_x.bsp.0001.bsp_lump.server");
        assert_eq!(archive_name("mp_rr_x", "perm"), "mp_rr_x_client_perm");
        assert_eq!(archive_name("mp_rr_x", "temp"), "mp_rr_x_client_temp");
    }

    #[test]
    fn a_map_is_staged_into_the_two_archives_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("r5ed-wraps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut bsp = Bsp::new(51, 0, 0);
        bsp.layout = Layout::Sidecar;
        bsp.set_lump(0x03, vec![1, 2, 3, 4]);
        bsp.set_lump(0x50, vec![5, 6, 7, 8]);
        bsp.set_lump(0x00, vec![9]);
        let mut ents = BTreeMap::new();
        ents.insert("script".to_string(), b"{}".to_vec());
        let staged = stage(&bsp, &ents, "mp_rr_x", &dir).expect("stages");
        assert!(staged.unassigned.is_empty(), "every lump of this map is named: {staged:?}");
        let halves = &staged.halves;
        assert_eq!(halves[0].0, "perm");
        assert_eq!(halves[1].0, "temp");
        assert_eq!(
            halves[0].1,
            vec!["maps/mp_rr_x.bsp", "maps/mp_rr_x.bsp.0003.bsp_lump.client", "maps/mp_rr_x.bsp.0050.bsp_lump.client"]
        );
        assert_eq!(
            halves[1].1,
            vec!["maps/mp_rr_x.bsp.0000.bsp_lump.client", "maps/mp_rr_x.bsp.0050.bsp_lump.client", "maps/mp_rr_x_script.ent"]
        );
        for (_, files) in halves.iter() {
            for f in files {
                // Joined per component, so the check holds on any platform:
                // replacing '/' with '\\' only named a subdirectory on Windows.
                let local: PathBuf = f.split('/').collect();
                let staged = [dir.join("perm").join(&local), dir.join("temp").join(&local)]
                    .into_iter()
                    .any(|p| p.is_file());
                assert!(staged, "{f} was named but not staged");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lump_neither_table_names_is_packed_perm_and_reported() {
        let dir = std::env::temp_dir().join(format!("r5ed-wraps2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut bsp = Bsp::new(51, 0, 0);
        bsp.layout = Layout::Sidecar;
        bsp.set_lump(0x03, vec![1]);
        bsp.set_lump(0x00, vec![2]);
        bsp.set_lump(0x5E, vec![3]);
        let staged = stage(&bsp, &BTreeMap::new(), "mp_rr_x", &dir).expect("stages");
        assert_eq!(staged.unassigned, vec!["005E".to_string()]);
        assert!(staged.halves[0].1.iter().any(|f| f.ends_with("005E.bsp_lump.client")), "it went to perm: {staged:?}");
        assert!(dir.join("perm/maps/mp_rr_x.bsp.005E.bsp_lump.client").is_file(), "and it was staged");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
