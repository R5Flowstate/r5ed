//! Level-level lumps: 0x18 entity partitions and 0x7B level info.

use crate::lumps::records::{parse_fixed, write_fixed};
use crate::{Error, Result, Version};

/// LUMP_ENTITY_PARTITIONS: a two-character ident, a flag byte the loader skips,
/// then the partition names as one space-separated NUL-terminated string.
/// The trailing bytes after that NUL are kept verbatim.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityPartitions {
    pub ident: [u8; 2],
    pub entity_lump_flag: u8,
    /// The name list exactly as stored, separators and all.
    pub list: Vec<u8>,
    pub names: Vec<String>,
    pub tail: Vec<u8>,
}

impl EntityPartitions {
    pub fn has(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }
}

const PARTITION_HEAD: usize = 3;

pub fn parse_entity_partitions(data: &[u8], _version: Version) -> Result<EntityPartitions> {
    if data.len() < PARTITION_HEAD {
        return Err(Error::Truncated { what: "entity partition header", need: PARTITION_HEAD, have: data.len() });
    }
    let mut out = EntityPartitions {
        ident: [data[0], data[1]],
        entity_lump_flag: data[2],
        ..Default::default()
    };
    let rest = &data[PARTITION_HEAD..];
    let end = rest.iter().position(|&b| b == 0).ok_or_else(|| Error::Invalid {
        id: 0x18,
        msg: "partition name list is not NUL terminated".to_string(),
    })?;
    out.list = rest[..end].to_vec();
    for name in rest[..end].split(|&b| b == b' ') {
        if name.is_empty() {
            continue;
        }
        out.names.push(name.iter().map(|&b| b as char).collect());
    }
    out.tail = rest[end + 1..].to_vec();
    Ok(out)
}

pub fn write_entity_partitions(p: &EntityPartitions, _version: Version) -> Vec<u8> {
    let mut out = Vec::with_capacity(PARTITION_HEAD + p.list.len() + 1 + p.tail.len());
    out.extend_from_slice(&p.ident);
    out.push(p.entity_lump_flag);
    out.extend_from_slice(&p.list);
    out.push(0);
    out.extend_from_slice(&p.tail);
    out
}

/// LUMP_LEVEL_INFO at v47/v51. The world's meshes are ordered opaque, decal,
/// transparent, then sky and brush-entity meshes, and the three `first_*_mesh`
/// fields are the boundaries: the opaque pass draws `[0, first_decal_mesh)`.
/// Object references below `first_sky_mesh` are mesh indices, and static prop
/// `i` is referenced as `first_static_prop_index + i`, which is
/// `first_sky_mesh` rounded up to a multiple of 64.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LevelInfo {
    pub first_decal_mesh: u32,
    pub first_transparent_mesh: u32,
    pub first_sky_mesh: u32,
    pub first_static_prop_index: u32,
    pub num_static_props: u32,
    /// The direction the sun's light travels.
    pub sun_normal: [f32; 3],
    pub unknown_0: u32,
}

/// Where static prop references start for a given first sky mesh.
pub fn first_static_prop_index(first_sky_mesh: u32) -> u32 {
    first_sky_mesh.div_ceil(64) * 64
}

pub fn parse_level_info(data: &[u8], _version: Version) -> Result<LevelInfo> {
    let all = parse_fixed::<LevelInfo>(0x7B, data)?;
    all.into_iter().next().ok_or(Error::Truncated { what: "level info", need: 36, have: data.len() })
}

pub fn write_level_info(info: &LevelInfo, _version: Version) -> Vec<u8> {
    write_fixed(&[*info])
}
