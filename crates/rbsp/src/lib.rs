//! rBSP container and lump codecs for Respawn map versions 37 (Titanfall 2),
//! 47 (Apex dedicated server) and 51 (Apex client).

pub mod container;
pub mod ids;
pub mod lumps;
pub mod validate;

pub use container::{Bsp, Header, Layout, LumpEntry, HEADER_SIZE, LUMP_COUNT};
pub use lumps::Lump;
pub use validate::{validate, Issue, Severity};

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error on {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("bad ident {0:?}")]
    BadIdent([u8; 4]),
    #[error("{what} truncated: need {need} bytes, have {have}")]
    Truncated {
        what: &'static str,
        need: usize,
        have: usize,
    },
    #[error("lump {id:#04x} lies outside the file")]
    LumpRange { id: u16 },
    #[error("lump {id:#04x}: size {size} is not a multiple of the {stride}-byte record")]
    Stride { id: u16, size: usize, stride: usize },
    #[error("lump {id:#04x}: {msg}")]
    Invalid { id: u16, msg: String },
}

/// Map version families the codecs understand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Version {
    Tf2V37,
    DediV47,
    ClientV51,
}

impl Version {
    pub fn from_header(v: u16) -> Option<Self> {
        match v {
            37 => Some(Self::Tf2V37),
            47 => Some(Self::DediV47),
            // v52 keeps every v51 record; it only adds lumps 0x39..0x3B.
            51 | 52 => Some(Self::ClientV51),
            _ => None,
        }
    }

    pub fn number(self) -> u16 {
        match self {
            Self::Tf2V37 => 37,
            Self::DediV47 => 47,
            Self::ClientV51 => 51,
        }
    }

    /// Header flags the engine expects for a freshly written map.
    pub fn flags(self) -> u16 {
        match self {
            Self::ClientV51 => 1,
            _ => 0,
        }
    }
}
