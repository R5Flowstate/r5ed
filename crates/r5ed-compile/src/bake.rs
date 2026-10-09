//! `bake`: the lighting solve, run on its own and cached beside the project.
//!
//! A bake is only valid for the document and the mode it was solved for, so the
//! product record carries a digest of the document it came from and a digest of
//! the lighting lumps it wrote. A later run over an unchanged document says the
//! cache is still current instead of solving it again.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rbake::BakeMode;
use rbsp::{Bsp, Version};
use rdoc::project::{self, NativeProject};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::build;

/// The product record format this build writes.
pub const RECORD_VERSION: u32 = 2;

/// The lumps a bake writes, in the order they are hashed.
pub const LIGHTING_LUMPS: [u16; 18] = [
    rbsp::ids::CUBEMAPS,
    rbsp::ids::CUBEMAPS_AMBIENT_RCP,
    rbsp::ids::WORLD_LIGHTS,
    rbsp::ids::WORLD_LIGHT_PARENT_INFOS,
    rbsp::ids::LIGHTPROBES,
    rbsp::ids::LIGHTPROBE_PARENT_INFOS,
    rbsp::ids::LIGHTPROBE_TREE,
    rbsp::ids::LIGHTPROBE_REFERENCES,
    rbsp::ids::LIGHTMAP_HEADERS,
    rbsp::ids::LIGHTMAP_DATA_SKY,
    rbsp::ids::LIGHTMAP_DATA_SKY_COMPRESSED,
    rbsp::ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS,
    rbsp::ids::LIGHTMAP_DATA_RTL_PAGE,
    rbsp::ids::SHADOW_ENVIRONMENTS,
    rbsp::ids::CSM_AABB_NODES,
    rbsp::ids::CSM_AABB_SUBTREE_REF_TOTALS,
    rbsp::ids::SHADOW_MESHES,
    rbsp::ids::SHADOW_MESH_INDICES,
];

/// Everything `bake` takes.
#[derive(Clone, Debug)]
pub struct Args {
    pub project: PathBuf,
    /// Bake mode, the same three names `build --bake` takes.
    pub mode: String,
    /// Where the maps are written; defaults to the project's build tree.
    pub out: Option<PathBuf>,
    /// Which targets to write.
    pub targets: String,
}

/// The record of one bake, written beside the project's build tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub stem: String,
    pub mode: String,
    /// Digest of the document the solve ran on.
    pub document: String,
    /// Digest of the lighting lumps the solve wrote.
    pub lighting: String,
    pub lightmap_pages: usize,
    pub lightprobes: usize,
    pub cubemaps: usize,
    pub csm_shadow_meshes: usize,
    /// Bytes of lighting lump per name, so a changed page is visible.
    pub lumps: Vec<(String, u64)>,
}

/// The directory a project caches its bakes in.
pub fn cache_dir(project: &Path) -> PathBuf {
    project.join("bake")
}

/// The record file for one stem and mode.
pub fn record_path(project: &Path, stem: &str, mode: &str) -> PathBuf {
    cache_dir(project).join(format!("{stem}-{mode}.json"))
}

/// A 64-bit FNV-1a digest, printed as sixteen hex digits.
pub fn digest(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The digest of the lighting lumps of a map, by name.
pub fn lighting_digest(bsp: &Bsp) -> (String, Vec<(String, u64)>) {
    let mut sizes = Vec::new();
    let mut running = 0xcbf2_9ce4_8422_2325u64;
    for id in LIGHTING_LUMPS.iter() {
        let bytes = bsp.lump(*id);
        if bytes.is_empty() {
            continue;
        }
        let version = bsp.version_kind().unwrap_or(Version::ClientV51);
        let name = rbsp::ids::name(*id, version).unwrap_or("OPAQUE");
        sizes.push((format!("{name}_{id:02x}"), bytes.len() as u64));
        for b in bytes {
            running ^= u64::from(*b);
            running = running.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    (format!("{running:016x}"), sizes)
}

/// The name a bake mode is written under, the spelling the record carries.
pub fn mode_name(mode: BakeMode) -> &'static str {
    match mode {
        BakeMode::None => "none",
        BakeMode::Preview => "preview",
        BakeMode::Final => "final",
    }
}

/// Solves the lighting and records what it produced.
pub fn run(args: &Args) -> Result<Value> {
    let mode = build::bake_mode(&args.mode)?;
    let document = project::load(Box::new(NativeProject::new(&args.project).map_err(|e| anyhow::anyhow!("{e}"))?))
        .map_err(|e| anyhow::anyhow!("{}: {e}", args.project.display()))?;
    let stem = document.doc.meta.stem.clone();
    if stem.is_empty() {
        bail!("the document has no map stem");
    }
    let path = record_path(&args.project, &stem, &args.mode);

    let build_args = build::Args {
        project: args.project.clone(),
        out: args.out.clone(),
        targets: args.targets.clone(),
        client: None,
        dedi: None,
        ent_dir: None,
        bake: args.mode.clone(),
        assets: Vec::new(),
        install: false,
        write_assets: None,
        all_assets: Vec::new(),
        allow_unresolved: false,
        src_paks: Vec::new(),
        rebuild: false,
    };
    let built = build::run(&build_args)?;
    // The build reuses its outputs when the inputs are unchanged; the record
    // here then describes the same lighting again.
    let was_current = built["reused"].as_bool().unwrap_or(false);
    let document_digest = built["inputs"].as_str().unwrap_or_default().to_string();
    let client = PathBuf::from(built["targets"]["client"]["bsp"].as_str().unwrap_or_default());
    let out = args.out.clone().unwrap_or_else(|| build::default_out(&args.project));
    let full = if client.is_absolute() { client } else { out.join(client) };
    let bsp = Bsp::read_path(&full).map_err(|e| anyhow::anyhow!("{}: {e}", full.display()))?;
    let (lighting, lumps) = lighting_digest(&bsp);
    let stats = &built["stats"];
    let count = |k: &str| stats.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let record = Record {
        version: RECORD_VERSION,
        stem: stem.clone(),
        mode: args.mode.clone(),
        document: document_digest,
        lighting,
        lightmap_pages: count("lightmap_pages"),
        lightprobes: count("lightprobes"),
        cubemaps: count("cubemaps"),
        csm_shadow_meshes: count("csm_shadow_meshes"),
        lumps,
    };
    std::fs::create_dir_all(cache_dir(&args.project)).with_context(|| format!("creating {}", cache_dir(&args.project).display()))?;
    let mut body = serde_json::to_string_pretty(&record)?;
    body.push('\n');
    std::fs::write(&path, &body).with_context(|| format!("writing {}", path.display()))?;
    build::progress(100, "done");
    Ok(json!({
        "stem": stem,
        "mode": mode_name(mode),
        "record": path.display().to_string(),
        "document": record.document,
        "lighting": record.lighting,
        "lightmap_pages": record.lightmap_pages,
        "lightprobes": record.lightprobes,
        "cubemaps": record.cubemaps,
        "csm_shadow_meshes": record.csm_shadow_meshes,
        "lumps": record.lumps.iter().map(|(n, b)| json!({"lump": n, "bytes": b})).collect::<Vec<_>>(),
        "was_current": was_current,
        "targets": built["targets"].clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_is_stable_and_separates_different_bytes() {
        assert_eq!(digest(b""), "cbf29ce484222325");
        assert_eq!(digest(b"abc"), "e71fa2190541574b");
        assert_ne!(digest(b"abc"), digest(b"abd"));
        assert_eq!(digest(b"abc").len(), 16);
    }

    #[test]
    fn the_record_path_follows_the_project() {
        assert_eq!(record_path(Path::new("p"), "mp_rr_x", "final"), PathBuf::from("p/bake/mp_rr_x-final.json"));
        assert_eq!(cache_dir(Path::new("p")), PathBuf::from("p/bake"));
    }

    /// A map with two lighting lumps digests to something that is not the digest
    /// of either alone, and an empty map still digests.
    #[test]
    fn a_lighting_digest_covers_every_lump_that_was_written() {
        let mut bsp = Bsp::new(51, 0, 0);
        assert_eq!(lighting_digest(&bsp).1.len(), 0, "an unbaked map carries no lighting");
        bsp.set_lump(rbsp::ids::WORLD_LIGHTS, vec![1, 2, 3]);
        bsp.set_lump(rbsp::ids::LIGHTPROBES, vec![4, 5, 6]);
        let (first, sizes) = lighting_digest(&bsp);
        assert_eq!(sizes.len(), 2, "{sizes:?}");
        assert!(sizes.iter().any(|(n, b)| n.starts_with("WORLD_LIGHTS") && *b == 3), "{sizes:?}");
        bsp.set_lump(rbsp::ids::WORLD_LIGHTS, vec![1, 2, 4]);
        let (second, _) = lighting_digest(&bsp);
        assert_ne!(first, second, "one changed byte changes the digest");
    }
}
