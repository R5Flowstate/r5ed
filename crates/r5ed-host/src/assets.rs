//! The asset routes, over the shared library.
//!
//! The library is behind a mutex because building an index shells out to the
//! asset tool, and one rebuild at a time is the right amount of concurrency for
//! a localhost editor.

use std::sync::{Arc, RwLock};

use rpak::Library;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, Result};

#[derive(Clone)]
pub struct Assets {
    library: Arc<RwLock<Library>>,
    game_root: std::path::PathBuf,
    projects_root: std::path::PathBuf,
}

impl Assets {
    pub fn new(library: Library, game_root: std::path::PathBuf, projects_root: std::path::PathBuf) -> Self {
        Self { library: Arc::new(RwLock::new(library)), game_root, projects_root }
    }

    pub fn game_root(&self) -> &std::path::Path {
        &self.game_root
    }

    pub fn projects_root(&self) -> &std::path::Path {
        &self.projects_root
    }

    fn with<T>(&self, f: impl FnOnce(&Library) -> Result<T>) -> Result<T> {
        let g = self.library.read().map_err(|_| ApiError::Internal("the asset library is poisoned".into()))?;
        f(&g)
    }

    /// Substring search over the index, paged.
    pub fn search(&self, q: &str, kind: Option<&str>, offset: usize, limit: usize) -> Result<(usize, Vec<rpak::AssetHit>)> {
        if q.len() > 256 {
            return Err(ApiError::bad("the search text is too long"));
        }
        if let Some(k) = kind {
            if k.len() > 8 || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err(ApiError::bad("kind must be a short type code"));
            }
        }
        self.with(|l| Ok(l.search(q, kind, offset, limit)))
    }

    pub fn model_glb(&self, path: &str) -> Result<Vec<u8>> {
        self.with(|l| Ok(l.model_glb(path)?))
    }

    pub fn prefetch_materials(&self, names: &[String], suffixes: &[String]) -> Result<Vec<rpak::MaterialPrefetch>> {
        self.with(|l| Ok(l.prefetch_materials(names, suffixes)?))
    }

    pub fn prefetch_models(&self, paths: &[String]) -> Result<rpak::Prefetch> {
        self.with(|l| Ok(l.prefetch_models(paths)?))
    }

    pub fn model_thumbnail(&self, path: &str) -> Result<Vec<u8>> {
        self.with(|l| Ok(l.thumbnail_png(path)?))
    }

    pub fn texture_dds(&self, guid: &str) -> Result<Vec<u8>> {
        let g = parse_guid(guid)?;
        self.with(|l| Ok(l.texture_dds(g)?))
    }

    pub fn surface_props(&self) -> Result<rpak::SurfaceProps> {
        self.with(|l| Ok(l.surface_props().clone()))
    }

    pub fn maps(&self) -> Vec<rpak::MapEntry> {
        self.with(|l| Ok(l.maps())).unwrap_or_default()
    }

    /// The pak that carries a map's wrap assets, for the import job.
    pub fn map_pak(&self, stem: &str) -> Option<std::path::PathBuf> {
        self.with(|l| Ok(l.map_pak(stem))).unwrap_or_default()
    }

    pub fn rebuild(&self) -> Result<usize> {
        let mut g = self.library.write().map_err(|_| ApiError::Internal("the asset library is poisoned".into()))?;
        Ok(g.rebuild_index()?)
    }

    /// The model description the editor shows in its properties panel.
    pub fn model_info(&self, path: &str) -> Result<rpak::ModelInfo> {
        self.with(|l| Ok(l.parse_model_asset(path)?.0))
    }

    pub fn material_info(&self, path: &str) -> Result<rpak::MaterialInfo> {
        self.with(|l| Ok(l.material(path)?))
    }
}

fn parse_guid(s: &str) -> Result<u64> {
    let body = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    if body.is_empty() || body.len() > 16 || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad("guid must be up to 16 hex digits"));
    }
    u64::from_str_radix(body, 16).map_err(|_| ApiError::bad("guid must be hex"))
}

/// The session document, which is also how the page learns what it may do.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub version: String,
    pub game_root: String,
    pub projects_root: String,
    pub capabilities: Capabilities,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub assets: bool,
    pub compile: bool,
    pub bake: bool,
    pub navmesh: bool,
    pub package: bool,
    pub deploy: bool,
    pub live: bool,
    pub import_tf2: bool,
    pub import_map: bool,
    pub minimap: bool,
    pub loadscreen: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guid_must_be_hex_and_short() {
        assert_eq!(parse_guid("0").unwrap(), 0);
        assert_eq!(parse_guid("0x984DF66749169863").unwrap(), 0x984D_F667_4916_9863);
        assert_eq!(parse_guid("984df66749169863").unwrap(), 0x984D_F667_4916_9863);
        for bad in ["", "0x", "zz", "0x1234567890abcdef0", "1 2"] {
            assert!(parse_guid(bad).is_err(), "{bad:?}");
        }
    }
}
