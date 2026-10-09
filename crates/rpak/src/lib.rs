//! Read-only access to the game's RPak assets.
//!
//! Everything here is either a parser for a file the RSX CLI writes or a
//! typed wrapper around invoking that CLI. The paks themselves stay sealed:
//! only RSX links the decompressor they need.

pub mod asset_list;
pub mod dds;
pub mod error;
pub mod glb;
pub mod index;
pub mod library;
pub mod material;
pub mod model_info;
pub mod png;
pub mod rmdl;
pub mod rson;
pub mod rsx;
pub mod thumbnail;

pub use error::{Error, Result};

pub use asset_list::{parse_asset_list, AssetRow};
pub use dds::{parse_dds_header, DdsFormat, DdsInfo};
pub use glb::{chunk_bounds, chunk_to_glb, ChunkPrimitive, MeshChunk};
pub use index::{AssetIndex, PakStamp};
pub use library::{AssetHit, Library, MapEntry, MaterialPrefetch, Prefetch};
pub use material::{parse_material_json, MaterialInfo, RawMaterial, VertexFormat};
pub use model_info::ModelInfo;
pub use png::encode_rgba8;
pub use rson::{parse_surface_props, read_surface_props_file, SurfaceProps};
pub use rsx::Rsx;
