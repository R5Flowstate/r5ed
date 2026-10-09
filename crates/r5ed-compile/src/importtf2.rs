//! `import-tf2`: a Titanfall 2 v37 map becomes a project whose build reproduces
//! the Apex map the converter produces.
//!
//! A v37 map has no brush tree to edit, so the project holds the document, the
//! entity partitions and a descriptor for the source. `build` reads the
//! descriptor and runs the same per-lump conversion the reference converter
//! runs, which is why the two agree byte for byte.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rdoc::model::{MapDoc, MapMeta, Partition};
use rdoc::project::{self, NativeProject, ProjectFs};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The project file the source descriptor lives in.
pub const DESCRIPTOR: &str = "import/tf2.json";

/// The descriptor format version this build writes and reads.
pub const DESCRIPTOR_VERSION: u32 = 1;

/// Everything `import-tf2` takes.
#[derive(Clone, Debug)]
pub struct Args {
    /// The v37 source map.
    pub bsp: PathBuf,
    pub project: PathBuf,
    /// Stem the map is built under; derived from the source name when absent.
    pub stem: String,
    /// The install's surface property table.
    pub surface_properties: Option<PathBuf>,
    /// `.vmt` trees, first directory wins where two name a material.
    pub vmt_dirs: Vec<PathBuf>,
    /// Extracted material json, which wins over a `.vmt` tree.
    pub material_dirs: Vec<PathBuf>,
    /// Import over an existing project.
    pub force: bool,
}

/// What a project needs to rebuild its map from a v37 source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub version: u32,
    /// Stem the converted map is named.
    pub stem: String,
    /// The v37 source, with its sidecars beside it.
    pub bsp: PathBuf,
    /// The surface property table the material names resolve against.
    #[serde(default)]
    pub surface_properties: Option<PathBuf>,
    /// `.vmt` trees, first directory wins.
    #[serde(default)]
    pub vmt_dirs: Vec<PathBuf>,
    /// Extracted material json, which wins over a `.vmt` tree.
    #[serde(default)]
    pub material_dirs: Vec<PathBuf>,
}

/// The stem a source name becomes: an `mp_rr_*` name is kept, anything else is
/// put under that prefix so the launcher will offer it.
pub fn stem_for(source_stem: &str) -> String {
    let clean: String = source_stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    let clean = clean.trim_matches('_').to_string();
    if clean.starts_with("mp_rr_") && clean.len() > 6 {
        clean
    } else if clean.is_empty() {
        "mp_rr_untitled".to_string()
    } else {
        format!("mp_rr_{clean}")
    }
}

/// The install's surface property table, when the environment names an install
/// that carries one.
pub fn default_surface_properties() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os(crate::tools::GAME_ROOT)?);
    let path = root.join("platform/scripts/surfaceproperties.rson");
    path.is_file().then_some(path)
}

/// Reads the descriptor a project carries, or `None` when it has none.
pub fn descriptor(project: &Path) -> Result<Option<Source>> {
    let path = project.join(DESCRIPTOR);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let source: Source = serde_json::from_str(&text).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    if source.version != DESCRIPTOR_VERSION {
        bail!("{} is format {} and this build reads {}", path.display(), source.version, DESCRIPTOR_VERSION);
    }
    Ok(Some(source))
}

/// The surface tables a conversion resolves material names against.
pub fn surface_tables(source: &Source) -> Result<rcomp::collision::tf2::SurfaceTables> {
    use rcomp::collision::tf2;
    let Some(path) = &source.surface_properties else {
        bail!("the tf2 source names no surface property table, so no material resolves");
    };
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let surf_ids = tf2::load_surfprop_ids(&text);
    if surf_ids.is_empty() {
        bail!("{} names no surface properties", path.display());
    }
    // The `.vmt` trees are the fallback the extracted material json does not
    // cover, and the json wins where both name a material.
    let mut material_surfprop = std::collections::HashMap::new();
    for dir in &source.vmt_dirs {
        for (name, prop) in tf2::read_vmt_dir(dir) {
            material_surfprop.entry(name).or_insert(prop);
        }
    }
    for dir in &source.material_dirs {
        material_surfprop.extend(tf2::read_material_dir(dir));
    }
    if material_surfprop.is_empty() {
        bail!("no vmt or material directory names a surface property");
    }
    Ok(tf2::SurfaceTables { surf_ids, material_surfprop })
}

/// The v37 source and its entity partitions.
pub fn read_source(bsp: &Path, stem: &str) -> Result<(rbsp::Bsp, BTreeMap<String, Vec<u8>>)> {
    let map = rcomp::tf2::read_source(bsp).map_err(|e| anyhow!("{e}"))?;
    if map.header.version != 37 {
        bail!("{} is a v{} map and this import reads a v37 source", bsp.display(), map.header.version);
    }
    let dir = bsp.parent().unwrap_or(Path::new(".")).to_path_buf();
    let source_stem = bsp.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut ents = BTreeMap::new();
    for p in Partition::FILES {
        for name in [source_stem.as_str(), stem] {
            let path = dir.join(format!("{name}_{}.ent", p.name()));
            if path.is_file() {
                ents.insert(p.name().to_string(), std::fs::read(&path).unwrap_or_default());
                break;
            }
        }
    }
    Ok((map, ents))
}

/// Imports the source as a project.
pub fn run(args: &Args) -> Result<Value> {
    crate::build::progress(5, "reading the source");
    if !args.bsp.is_file() {
        bail!("{} is not a file", args.bsp.display());
    }
    if args.project.join(project::MAP_FILE).exists() && !args.force {
        bail!("{} already holds a map; pass --force to import over it", args.project.display());
    }
    let (_, ents) = read_source(&args.bsp, &args.stem)?;
    let surface_properties = args
        .surface_properties
        .clone()
        .or_else(default_surface_properties)
        .ok_or_else(|| anyhow!("no surface property table: pass --surface-properties or set {}", crate::tools::GAME_ROOT))?;
    if !surface_properties.is_file() {
        bail!("{} is not a file", surface_properties.display());
    }
    for dir in args.vmt_dirs.iter().chain(&args.material_dirs) {
        if !dir.is_dir() {
            bail!("{} is not a directory", dir.display());
        }
    }
    let source = Source {
        version: DESCRIPTOR_VERSION,
        stem: args.stem.clone(),
        bsp: args.bsp.clone(),
        surface_properties: Some(surface_properties.clone()),
        vmt_dirs: args.vmt_dirs.clone(),
        material_dirs: args.material_dirs.clone(),
    };
    // The tables are resolved now, so a source that cannot convert is refused
    // at import rather than at the first build.
    let tables = surface_tables(&source)?;

    crate::build::progress(45, "writing the project");
    std::fs::create_dir_all(&args.project).with_context(|| format!("creating {}", args.project.display()))?;
    let mut fs = NativeProject::new(&args.project).map_err(|e| anyhow!("{e}"))?;
    let doc = MapDoc {
        meta: MapMeta { stem: args.stem.clone(), display_name: display_name(&args.stem), ..MapMeta::default() },
        ..MapDoc::default()
    };
    project::save(&mut fs, &doc).map_err(|e| anyhow!("{e}"))?;
    let mut body = serde_json::to_string_pretty(&source)?;
    body.push('\n');
    ProjectFs::write(&mut fs, DESCRIPTOR, body.as_bytes()).map_err(|e| anyhow!("{DESCRIPTOR}: {e}"))?;
    let mut written = vec![project::MAP_FILE.to_string(), DESCRIPTOR.to_string()];
    for (name, bytes) in &ents {
        let path = format!("import/{name}.ent");
        ProjectFs::write(&mut fs, &path, bytes).map_err(|e| anyhow!("{path}: {e}"))?;
        written.push(path);
    }
    written.sort();
    crate::build::progress(100, "done");
    Ok(json!({
        "stem": args.stem,
        "project": args.project.display().to_string(),
        "source": args.bsp.display().to_string(),
        "source_version": 37,
        "surface_properties": surface_properties.display().to_string(),
        "vmt_dirs": args.vmt_dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>(),
        "material_dirs": args.material_dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>(),
        "surf_props": tables.surf_ids.len(),
        "materials": tables.material_surfprop.len(),
        "partitions": ents.keys().cloned().collect::<Vec<_>>(),
        "descriptor": args.project.join(DESCRIPTOR).display().to_string(),
        "files": written,
    }))
}

/// The name a picker shows, from the stem's last component.
pub fn display_name(stem: &str) -> String {
    stem.rsplit('_')
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut out = String::new();
            for (i, c) in s.chars().enumerate() {
                if i == 0 {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
            }
            out
        })
        .unwrap_or_else(|| stem.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_name_becomes_a_stem_the_launcher_offers() {
        assert_eq!(stem_for("sp_training"), "mp_rr_sp_training");
        assert_eq!(stem_for("mp_rr_gauntlet"), "mp_rr_gauntlet");
        assert_eq!(stem_for("The Gauntlet"), "mp_rr_the_gauntlet");
        assert_eq!(stem_for("--"), "mp_rr_untitled");
        assert_eq!(stem_for(""), "mp_rr_untitled");
        for stem in [stem_for("sp_training"), stem_for("mp_rr_x"), stem_for("A B")] {
            assert!(crate::register::launcher_stem_ok(&stem), "{stem}");
        }
    }

    #[test]
    fn a_stem_becomes_a_name_a_picker_shows() {
        assert_eq!(display_name("mp_rr_freedm_skulltown"), "Skulltown");
        assert_eq!(display_name("mp_lobby"), "Lobby");
        assert_eq!(display_name("mp_rr_a"), "A");
    }

    #[test]
    fn a_project_with_no_descriptor_is_not_a_tf2_project() {
        let dir = std::env::temp_dir().join(format!("r5ed-tf2desc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(descriptor(&dir).unwrap().is_none());
        std::fs::write(dir.join("import").join("x"), b"x").ok();
        assert!(descriptor(&dir).unwrap().is_none(), "the directory alone is not a descriptor");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_descriptor_in_another_format_is_refused() {
        let dir = std::env::temp_dir().join(format!("r5ed-tf2desc2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("import")).unwrap();
        std::fs::write(
            dir.join(DESCRIPTOR),
            br#"{"version":99,"stem":"mp_rr_x","bsp":"a.bsp"}"#,
        )
        .unwrap();
        let err = descriptor(&dir).unwrap_err().to_string();
        assert!(err.contains("format 99"), "{err}");
        std::fs::write(
            dir.join(DESCRIPTOR),
            br#"{"version":1,"stem":"mp_rr_x","bsp":"a.bsp","material_dirs":["m"]}"#,
        )
        .unwrap();
        let back = descriptor(&dir).unwrap().expect("reads");
        assert_eq!(back.stem, "mp_rr_x");
        assert_eq!(back.material_dirs, vec![PathBuf::from("m")]);
        assert!(back.vmt_dirs.is_empty());
        let err = surface_tables(&back).unwrap_err().to_string();
        assert!(err.contains("surface property table"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_source_with_no_surface_table_or_no_materials_is_refused_by_name() {
        let source = Source {
            version: DESCRIPTOR_VERSION,
            stem: "mp_rr_x".into(),
            bsp: PathBuf::from("a.bsp"),
            surface_properties: None,
            vmt_dirs: Vec::new(),
            material_dirs: Vec::new(),
        };
        let err = surface_tables(&source).unwrap_err().to_string();
        assert!(err.contains("no surface property table"), "{err}");

        let dir = std::env::temp_dir().join(format!("r5ed-tf2surf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("surfaceproperties.rson"), "\"default\"\n{\n}\n").unwrap();
        let source = Source {
            version: DESCRIPTOR_VERSION,
            stem: "mp_rr_x".into(),
            bsp: PathBuf::from("a.bsp"),
            surface_properties: Some(dir.join("surfaceproperties.rson")),
            vmt_dirs: Vec::new(),
            material_dirs: Vec::new(),
        };
        let err = surface_tables(&source).unwrap_err().to_string();
        assert!(err.contains("no surface properties"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
