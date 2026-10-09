//! Error type for the baker.

use std::path::PathBuf;

/// Everything the baker can fail with. Every variant carries enough context to
/// locate the input that produced it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("bsp lump {id:#04x}: {msg}")]
    Lump { id: u16, msg: String },
    #[error("lightmap chart {chart} of surface {surface}: {msg}")]
    Chart { surface: usize, chart: usize, msg: String },
    #[error("scene: {0}")]
    Scene(String),
    #[error("lighting: {0}")]
    Lighting(String),
    #[error("map version {0:?} is not a target the baker emits for")]
    Target(rbsp::Version),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error(transparent)]
    Bsp(#[from] rbsp::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn lump(id: u16, msg: impl Into<String>) -> Self {
        Self::Lump { id, msg: msg.into() }
    }

    pub fn scene(msg: impl Into<String>) -> Self {
        Self::Scene(msg.into())
    }

    pub fn lighting(msg: impl Into<String>) -> Self {
        Self::Lighting(msg.into())
    }
}
