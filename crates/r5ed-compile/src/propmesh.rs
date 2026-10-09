//! Static prop meshes read from converted models, the triangles a prop blocks
//! baked light with.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use glam::Vec3;

/// The environment variable naming the model directories a build reads prop
/// meshes from, `;`-separated. A file found as `<dir>/<rel>.rmdl` is the
/// model `mdl/<rel>.rmdl`.
pub const PROP_MODELS_ENV: &str = "R5ED_PROP_MODELS";

/// Where a model named by a map is found under the given roots.
pub fn model_path(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    let rel = name.replace('\\', "/");
    let rel = rel.strip_prefix("mdl/").unwrap_or(&rel).to_string();
    let base = Path::new(&rel).file_stem()?.to_string_lossy().to_string();
    for r in roots {
        for p in [r.join(&rel), r.join(&base).join(format!("{base}.rmdl")), r.join(format!("{base}.rmdl"))] {
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// A model's hull and its render triangles in model space.
#[derive(Clone)]
pub struct ModelGeometry {
    pub min: Vec3,
    pub max: Vec3,
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
}

pub fn load_model(path: &Path) -> Result<ModelGeometry> {
    let rmdl = std::fs::read(path)?;
    let vg = ["vg", "vg_static"]
        .iter()
        .map(|e| path.with_extension(e))
        .find(|p| p.is_file())
        .with_context(|| format!("{} has no vertex file beside it", path.display()))?;
    let model = rpak::rmdl::parse_model(&rmdl, &std::fs::read(&vg)?)?;
    let to_model = |v: [f32; 3]| Vec3::from_array(rpak::rmdl::vertex_to_studiohdr_space(v));
    let mut out = ModelGeometry {
        min: to_model(model.bounds_min),
        max: to_model(model.bounds_max),
        positions: Vec::new(),
        indices: Vec::new(),
    };
    for m in &model.meshes {
        let base = out.positions.len() as u32;
        // The vertex blocks are already on the placement axes; only the
        // header's hull is stored rotated.
        out.positions.extend(m.positions.iter().map(|p| Vec3::from_array(*p)));
        out.indices.extend(m.indices.iter().map(|i| base + *i as u32));
    }
    Ok(out)
}

/// The directories [`PROP_MODELS_ENV`] names.
pub fn env_dirs() -> Vec<PathBuf> {
    std::env::var(PROP_MODELS_ENV)
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Every model under `dirs`, keyed by its lower-case `mdl/` path.
pub fn load_dirs(dirs: &[PathBuf]) -> Result<rcomp::PropMeshes> {
    let mut out = BTreeMap::new();
    let mut failed = Vec::new();
    for dir in dirs {
        let mut files = Vec::new();
        collect_models(dir, &mut files).with_context(|| format!("reading prop models under {}", dir.display()))?;
        for f in files {
            let Ok(rel) = f.strip_prefix(dir) else { continue };
            let key = format!("mdl/{}", rel.to_string_lossy().replace('\\', "/")).to_ascii_lowercase();
            match load_model(&f) {
                Ok(g) => {
                    out.insert(key, rcomp::PropMesh { positions: g.positions, indices: g.indices });
                }
                Err(_) => failed.push(key),
            }
        }
    }
    if !dirs.is_empty() {
        eprintln!("prop meshes: {} models, {} unreadable{}", out.len(), failed.len(),
            failed.first().map(|f| format!(" ({f} ...)")).unwrap_or_default());
    }
    Ok(rcomp::PropMeshes(Arc::new(out)))
}

fn collect_models(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_models(&path, out)?;
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rmdl")) {
            out.push(path);
        }
    }
    Ok(())
}
