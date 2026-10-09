//! Importers that turn foreign map data into `Command::Batch` edits.

pub mod glb;
pub mod obj;
pub mod quake;
pub mod remap;
pub mod source;
pub mod tokens;
pub mod vmf;

use thiserror::Error;

use crate::brush::BrushError;
use glb::GlbError;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("input ended early")]
    Eof,
    #[error("unexpected {0}")]
    Unexpected(String),
    #[error("{0:?} is not a number")]
    BadNumber(String),
    #[error("the brush does not form a solid: {0}")]
    NotASolid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("the project file path is not allowed: {0}")]
    BadPath(String),
    #[error(transparent)]
    Brush(#[from] BrushError),
    #[error(transparent)]
    Mesh(#[from] GlbError),
}

/// `name` for an imported object: the engine keys that name it, in priority order.
pub fn entity_name(classname: &str, keys: &crate::model::KeyValues, fallback: &str) -> String {
    for key in ["targetname", "script_name", "name", "hammerid"] {
        if let Some(v) = keys.get(key) {
            if !v.is_empty() {
                return v.clone();
            }
        }
    }
    if classname.is_empty() {
        fallback.to_string()
    } else {
        classname.to_string()
    }
}

/// A mesh file path for an imported primitive, numbered by `index`.
pub fn mesh_path(index: usize) -> String {
    format!("meshes/import_{index:04}.glb")
}
