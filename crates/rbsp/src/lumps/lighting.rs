//! Lighting lumps: probes, lights, lightmap pages, cubemaps and the
//! real-time-light index table.

use crate::lumps::records::fixed_codec;
use crate::{Result, Version};

/// LUMP_LIGHTPROBE_PARENT_INFOS: one 28-byte record.
///
/// Not every field is established. The fifth word is the probe reference
/// count (0x68) and the seventh the probe tree's node count (0x67): the shipped
/// staging_mu1 server map reads `0 0 0 0 43031 0 11183` against 43031
/// references and 11183 nodes, and the Titanfall 2 record reads the same way.
/// The client half of that map, as a pak export writes it, carries the record
/// as 28 zero bytes, but so it does eight other small lumps the server half has
/// data for; the zeros are the export's, not the map's.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightProbeParentInfo {
    pub words: [u32; 7],
}

impl LightProbeParentInfo {
    /// The record over one reference list and one tree.
    pub fn over(refs: usize, tree_nodes: usize) -> Self {
        let mut words = [0u32; 7];
        words[4] = refs as u32;
        words[6] = tree_nodes as u32;
        Self { words }
    }
}

fixed_codec!(parse_parent_infos, write_parent_infos, LightProbeParentInfo, 0x04);

/// LUMP_SHADOW_ENVIRONMENTS: one cascade's slice of the CSM tree and of the
/// world shadow mesh list, plus the light direction its bounds live in.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShadowEnvironment {
    /// First CSM node of this cascade's tree.
    pub aabb_root: u32,
    /// First CSM reference of this cascade's tree.
    pub aabb_refs_begin: u32,
    /// First world shadow mesh this cascade draws.
    pub shadow_mesh_begin: u32,
    /// One past the last CSM node of this cascade's tree.
    pub aabb_end: u32,
    /// One past the last CSM reference of this cascade's tree.
    pub aabb_refs_end: u32,
    /// One past the last world shadow mesh this cascade draws.
    pub shadow_mesh_end: u32,
    pub shadow_direction: [f32; 3],
}

fixed_codec!(parse_shadow_envs, write_shadow_envs, ShadowEnvironment, 0x05);

/// LUMP_WORLD_LIGHTS.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WorldLight {
    pub origin: [f32; 3],
    pub direction: [f32; 3],
    pub unknown_0: [u32; 6],
    pub intensity: f32,
    /// 0 unknown, 1 directional, 2 point, 3 skylight, 4 spot.
    pub light_type: u32,
    pub unknown_1: u32,
    pub color: [f32; 3],
    pub unknown_2: [u32; 4],
    /// Bit 3 marks a real-time light; 0x20 marks a shadow caster.
    pub flags: u32,
    pub unknown_3: [u32; 5],
}

pub const LIGHT_TYPE_SKY: u32 = 3;
pub const LIGHT_FLAG_REALTIME: u32 = 0x8;

fixed_codec!(parse_world_lights, write_world_lights, WorldLight, 0x36);

/// LUMP_TWEAK_LIGHTS at v47/v51: a table of 32-bit ids the light setup reads.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TweakLight {
    pub value: u32,
}

fixed_codec!(parse_tweak_lights, write_tweak_lights, TweakLight, 0x55);

/// LUMP_LIGHTMAP_HEADERS.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightmapHeader {
    /// Selects the uncompressed sky format in 0x62.
    pub lightmap_type: u8,
    /// Selects the compressed sky format in 0x61; that lump has no type gate.
    pub compressed_type: u8,
    pub tag: u8,
    pub pad: u8,
    pub width: u16,
    pub height: u16,
}

impl LightmapHeader {
    /// The uncompressed sky path only runs for types 1, 9 and 10.
    pub fn has_sky_data(&self) -> bool {
        self.lightmap_type != 0 && self.lightmap_type != 2 && (self.lightmap_type.wrapping_sub(1)) & 0xF6 == 0
    }
}

fixed_codec!(parse_lightmap_headers, write_lightmap_headers, LightmapHeader, 0x53);

/// Bytes one page of a sky lightmap lump takes for a given format code.
pub fn lightmap_page_bytes(kind: u8, width: u16, height: u16) -> Option<usize> {
    let (w, h) = (width as usize, height as usize);
    let block = |n: usize| 32 * w.div_ceil(n) * h.div_ceil(n);
    Some(match kind {
        1 | 10 => 8 * w * h,
        4 | 8 => block(4),
        5 => block(5),
        6 => block(6),
        7 => block(8),
        9 => 12 * w * h,
        _ => return None,
    })
}

/// Bytes one page of the real-time-light lump takes once padded for the GPU.
pub fn rtl_page_bytes(width: u16, height: u16) -> usize {
    let (w, h) = (width as usize, height as usize);
    let padded = w.div_ceil(4) * 4 * h.div_ceil(4) * 4;
    2 * (padded + 2 * w * h)
}

/// One page of a bulk lightmap lump.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LightmapPage {
    pub index: u32,
    pub width: u16,
    pub height: u16,
    pub bytes: Vec<u8>,
}

/// A bulk lightmap lump (0x61, 0x62, 0x69). The page layout is driven by the
/// 0x53 headers, which live in another lump, so the codec keeps the bytes whole
/// and [`split_by_headers`] does the walk for callers that hold both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LightmapPages {
    pub pages: Vec<LightmapPage>,
    pub tail: Vec<u8>,
}

impl LightmapPages {
    pub fn len(&self) -> usize {
        self.pages.iter().map(|p| p.bytes.len()).sum::<usize>() + self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len());
        for p in &self.pages {
            out.extend_from_slice(&p.bytes);
        }
        out.extend_from_slice(&self.tail);
        out
    }
}

/// Which of the three bulk lightmap lumps a page walk is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageFormat {
    /// 0x62: driven by the uncompressed type byte, behind its gate.
    Sky,
    /// 0x61: driven by the compressed type byte, no gate.
    SkyCompressed,
    /// 0x69: behind the same gate as the sky lump.
    Rtl,
}

impl PageFormat {
    fn page_bytes(&self, h: &LightmapHeader) -> Option<usize> {
        match self {
            PageFormat::Sky => {
                if h.has_sky_data() {
                    lightmap_page_bytes(h.lightmap_type, h.width, h.height)
                } else {
                    None
                }
            }
            PageFormat::SkyCompressed => lightmap_page_bytes(h.compressed_type, h.width, h.height),
            PageFormat::Rtl => h.has_sky_data().then(|| rtl_page_bytes(h.width, h.height)),
        }
    }
}

/// Splits a bulk lightmap lump against its 0x53 headers. Whatever the headers
/// do not account for lands in `tail`, so a map whose headers were stripped
/// still round trips.
pub fn split_by_headers(data: &[u8], headers: &[LightmapHeader], format: PageFormat) -> LightmapPages {
    let mut out = LightmapPages::default();
    let mut at = 0usize;
    for (i, h) in headers.iter().enumerate() {
        let Some(n) = format.page_bytes(h) else { continue };
        let Some(stop) = at.checked_add(n).filter(|s| *s <= data.len()) else { break };
        out.pages.push(LightmapPage { index: i as u32, width: h.width, height: h.height, bytes: data[at..stop].to_vec() });
        at = stop;
    }
    out.tail = data[at..].to_vec();
    out
}

pub fn parse_lightmap_bulk(data: &[u8], _version: Version) -> Result<LightmapPages> {
    Ok(LightmapPages { pages: Vec::new(), tail: data.to_vec() })
}

pub fn write_lightmap_bulk(pages: &LightmapPages, _version: Version) -> Vec<u8> {
    pages.encode()
}

/// LUMP_LIGHTMAP_DATA_RTL_PAGE: world-light indices, 63 slots per lightmap
/// page. A texel's light group and slot index it as `group * 63 + slot`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RtlIndex {
    pub value: u16,
}

fixed_codec!(parse_rtl_index, write_rtl_index, RtlIndex, 0x7A);

/// Bytes one real-time-light page takes.
pub const RTL_PAGE_BYTES: usize = 126;
/// Slots one real-time-light page holds.
pub const RTL_PAGE_SLOTS: usize = RTL_PAGE_BYTES / 2;

/// LUMP_LIGHTPROBES at v51: the compact 44-byte probe.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightProbe {
    pub ambient_sh: [i16; 12],
    pub sky_dir_sun_vis: [i16; 4],
    pub static_light_weights: [u8; 4],
    pub static_light_indexes: [i16; 4],
}

fixed_codec!(parse_lightprobes_v51, write_lightprobes_v51, LightProbe, 0x65);

/// LUMP_LIGHTPROBES at v37 and v47: the same probe with a 4-byte SIMD pad.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightProbe48 {
    pub probe: LightProbe,
    pub pad: [u8; 4],
}

fixed_codec!(parse_lightprobes48, write_lightprobes48, LightProbe48, 0x65);

/// LUMP_STATIC_PROP_LIGHTPROBE_INDEX: one probe index per static prop.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PropLightprobeIndex {
    pub value: u32,
}

fixed_codec!(parse_prop_lightprobe_indices, write_prop_lightprobe_indices, PropLightprobeIndex, 0x66);

/// LUMP_LIGHTPROBETREE: a binary k-d tree over the probe references.
///
/// The low two bits of `flags` are the node kind. `3` is a leaf: `flags >> 2`
/// is its first reference and `data` is an integer reference count. `0..=2` is
/// an interior node splitting on that axis: `flags >> 2` is the first of its two
/// children, which sit side by side, and `data` holds the split coordinate as
/// `f32` bits. The low child keeps the references at or below the split.
///
/// `data` is raw because its type depends on the kind. A shipped leaf counts 1
/// to 8 references; a count written as a float reads back as a billion, and the
/// client scans that many 20-byte references off the end of the lump.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightProbeTreeNode {
    pub flags: u32,
    pub data: u32,
}

impl LightProbeTreeNode {
    /// The kind bits of a leaf, the only kind that ends the client's walk.
    pub const LEAF: u32 = 3;

    /// A leaf covering `count` references from `first`.
    pub fn leaf(first: u32, count: u32) -> Self {
        Self { flags: (first << 2) | Self::LEAF, data: count }
    }

    /// An interior node splitting on `axis` at `split`, whose children are
    /// `first_child` and the node after it.
    pub fn interior(axis: u32, first_child: u32, split: f32) -> Self {
        debug_assert!(axis < 3, "a split axis is 0, 1 or 2");
        Self { flags: (first_child << 2) | (axis & 3), data: split.to_bits() }
    }

    pub fn kind(&self) -> u32 {
        self.flags & 3
    }

    pub fn is_leaf(&self) -> bool {
        self.kind() == Self::LEAF
    }

    /// A leaf's first reference, or an interior node's first child.
    pub fn first(&self) -> u32 {
        self.flags >> 2
    }

    /// A leaf's reference count.
    pub fn count(&self) -> u32 {
        self.data
    }

    /// An interior node's split coordinate.
    pub fn split(&self) -> f32 {
        f32::from_bits(self.data)
    }
}

fixed_codec!(parse_lightprobe_tree, write_lightprobe_tree, LightProbeTreeNode, 0x67);

/// LUMP_LIGHTPROBEREFS: one probe's world position and its index.
///
/// The probe record carries no position, so this is where the client learns
/// where a probe is. A shipped map writes one reference per probe it places in
/// the tree. `cube_index` is the cubemap every probe-lit model near this
/// reference reflects: the client's nearest-reference search returns it with
/// the probe, so props, characters and weapons never read the meshes' own
/// cubemap indices.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightProbeRef {
    pub origin: [f32; 3],
    pub probe: u32,
    pub cube_index: u16,
    pub unknown: u16,
}

fixed_codec!(parse_lightprobe_refs, write_lightprobe_refs, LightProbeRef, 0x68);

/// LUMP_CUBEMAPS: one record per baked cubemap.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Cubemap {
    pub unknown_0: i32,
    pub unknown_1: i32,
    pub unknown_2: i32,
    /// The cubemap array texture the bake step fills in.
    pub texture_guid: u32,
}

fixed_codec!(parse_cubemaps, write_cubemaps, Cubemap, 0x2A);

/// LUMP_UNKNOWN_43 at v47/v51: one ambient scale per cubemap.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CubemapAmbient {
    pub scale: f32,
}

fixed_codec!(parse_cubemap_ambient, write_cubemap_ambient, CubemapAmbient, 0x2B);
