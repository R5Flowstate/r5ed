//! Decoders and encoders for every lighting lump.
//!
//! A decode is total: hostile bytes either produce a value or an error, and
//! every value re-encodes to exactly the bytes it came from, including a bulk
//! lightmap lump whose page table does not account for all of it.

use rbsp::ids;
use rbsp::lumps::lighting as lmp;
use rbsp::lumps::records::{parse_fixed, write_fixed};
use rbsp::Version;

use crate::error::{Error, Result};

/// A 0x2A record: a cubemap origin in integer inches and a texture id.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CubemapRecord {
    pub origin: [i32; 3],
    pub texture: u32,
}

/// A 0x63 caster node with the cell-tree bit packing split out.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CsmNodeRecord {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub first_child: u32,
    pub child_count: u32,
    pub first_ref: u32,
    pub ref_count: u32,
}

impl CsmNodeRecord {
    fn from_raw(n: rbsp::lumps::csm::CsmAabbNode) -> Self {
        Self {
            mins: n.mins,
            maxs: n.maxs,
            first_child: n.first_child(),
            child_count: n.child_count(),
            first_ref: n.first_obj_ref(),
            ref_count: n.obj_ref_count(),
        }
    }

    fn to_raw(self) -> rbsp::lumps::csm::CsmAabbNode {
        rbsp::lumps::csm::CsmAabbNode {
            mins: self.mins,
            children: (self.child_count & 0xFF) | (self.first_child << 8),
            maxs: self.maxs,
            obj_refs: (self.ref_count & 0xFF) | (self.first_ref << 8),
        }
    }
}

/// A 0x7F world shadow mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShadowMeshRecord {
    pub first_vertex: u32,
    pub triangle_count: u32,
    /// Drawn in the alpha pass. The record stores the opposite: its halfword
    /// is 1 for an opaque mesh and 0 for an alpha one, and the client draws a
    /// 0 from the alpha vertex buffer (0x7D).
    pub draw_alpha: bool,
    /// The alpha mesh's material; 0xFFFF on every shipped opaque mesh.
    pub tail: u16,
}

/// A 0x7D alpha shadow vertex: a position and the 8-byte stream the alpha pass
/// reads.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShadowAlphaVertex {
    pub position: [f32; 3],
    pub tail: [u32; 2],
}

/// A 0x36 world light with its fields named by what they hold.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldLightRecord {
    pub origin: [f32; 3],
    /// RGB intensity.
    pub intensity: [f32; 3],
    /// Unit vector the light points along.
    pub normal: [f32; 3],
    pub shadow_offset: [f32; 3],
    pub cluster: f32,
    /// 1 directional, 2 point or spot, 3 skylight, 4 spot.
    pub light_type: u32,
    pub style: u32,
    pub inner_cone: f32,
    pub outer_cone: f32,
    pub exponent: f32,
    pub radius: f32,
    pub constant_attenuation: f32,
    pub linear_attenuation: f32,
    pub quadratic_attenuation: f32,
    pub flags: u32,
    pub texture_index: u32,
    pub parent: u32,
    pub scale: f32,
    pub extra: [f32; 2],
}

fn f3(b: &[u8]) -> [f32; 3] {
    let mut o = [0.0f32; 3];
    for (i, slot) in o.iter_mut().enumerate() {
        let at = i * 4;
        if at + 4 <= b.len() {
            *slot = f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        }
    }
    o
}

/// Splits the 112-byte world light record into its named fields.
pub fn parse_world_light(b: &[u8]) -> Result<WorldLightRecord> {
    if b.len() < 112 {
        return Err(Error::lump(ids::WORLD_LIGHTS, format!("a world light is 112 bytes, got {}", b.len())));
    }
    let u32at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let fat = |o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    Ok(WorldLightRecord {
        origin: f3(&b[0..12]),
        intensity: f3(&b[12..24]),
        normal: f3(&b[24..36]),
        shadow_offset: f3(&b[36..48]),
        cluster: fat(48),
        light_type: u32at(52),
        style: u32at(56),
        inner_cone: fat(60),
        outer_cone: fat(64),
        exponent: fat(68),
        radius: fat(72),
        constant_attenuation: fat(76),
        linear_attenuation: fat(80),
        quadratic_attenuation: fat(84),
        flags: u32at(88),
        texture_index: u32at(92),
        parent: u32at(96),
        scale: fat(100),
        extra: [fat(104), fat(108)],
    })
}

/// The inverse of [`parse_world_light`].
pub fn write_world_light(l: &WorldLightRecord) -> [u8; 112] {
    let mut b = [0u8; 112];
    let put3 = |b: &mut [u8; 112], at: usize, v: &[f32; 3]| {
        for (i, f) in v.iter().enumerate() {
            b[at + i * 4..at + i * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
    };
    put3(&mut b, 0, &l.origin);
    put3(&mut b, 12, &l.intensity);
    put3(&mut b, 24, &l.normal);
    put3(&mut b, 36, &l.shadow_offset);
    b[48..52].copy_from_slice(&l.cluster.to_le_bytes());
    b[52..56].copy_from_slice(&l.light_type.to_le_bytes());
    b[56..60].copy_from_slice(&l.style.to_le_bytes());
    b[60..64].copy_from_slice(&l.inner_cone.to_le_bytes());
    b[64..68].copy_from_slice(&l.outer_cone.to_le_bytes());
    b[68..72].copy_from_slice(&l.exponent.to_le_bytes());
    b[72..76].copy_from_slice(&l.radius.to_le_bytes());
    b[76..80].copy_from_slice(&l.constant_attenuation.to_le_bytes());
    b[80..84].copy_from_slice(&l.linear_attenuation.to_le_bytes());
    b[84..88].copy_from_slice(&l.quadratic_attenuation.to_le_bytes());
    b[88..92].copy_from_slice(&l.flags.to_le_bytes());
    b[92..96].copy_from_slice(&l.texture_index.to_le_bytes());
    b[96..100].copy_from_slice(&l.parent.to_le_bytes());
    b[100..104].copy_from_slice(&l.scale.to_le_bytes());
    b[104..108].copy_from_slice(&l.extra[0].to_le_bytes());
    b[108..112].copy_from_slice(&l.extra[1].to_le_bytes());
    b
}

/// Real-time-light ids and visibilities for one lightmap page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RtlPage {
    pub width: u16,
    pub height: u16,
    /// `width * height` words: the light group in the top 8 bits, then four
    /// 6-bit slots. A slot of 63 is unused.
    pub ids: Vec<u32>,
    /// `4 * width * height` visibilities, slot 0 to slot 3 per texel.
    pub vis: Vec<u8>,
    /// The BC5 plane exactly as it was decoded, so an untouched page
    /// re-encodes to the bytes it came from. Empty on a page the baker built.
    pub raw_bc5: Vec<u8>,
}

impl RtlPage {
    pub fn new(width: u16, height: u16) -> Self {
        let n = width as usize * height as usize;
        Self {
            width,
            height,
            // Group zero with all four slots unused: the word a converter
            // leaves behind for a texel no light reaches.
            ids: vec![0x00FF_FFFF; n],
            vis: vec![0; n * 4],
            raw_bc5: Vec::new(),
        }
    }

    pub fn texels(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Puts `light`'s group and slot into a texel, keeping at most four
    /// slots. Returns the slot used, or `None` when the texel is full.
    pub fn set_slot(&mut self, texel: usize, group: u8, slot: u32, weight: u8) -> Option<u32> {
        if texel >= self.texels() || group > 63 || slot > 62 {
            return None;
        }
        let word = &mut self.ids[texel];
        for s in 0..4u32 {
            if ((*word >> (6 * s)) & 63) == 63 {
                *word = (*word & !(63u32 << (6 * s))) | ((slot & 63) << (6 * s));
                *word = (*word & 0x00FF_FFFF) | ((group as u32) << 24);
                self.vis[texel * 4 + s as usize] = weight;
                return Some(s);
            }
        }
        None
    }

    /// The slots a texel uses, in slot order.
    pub fn slots(&self, texel: usize) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        if texel >= self.texels() {
            return out;
        }
        let word = self.ids[texel];
        for slot in 0..4u32 {
            let v = (word >> (6 * slot)) & 63;
            if v != 63 {
                out.push((slot, v));
            }
        }
        out
    }

    /// The world light a texel's slot resolves to, or `None` for an unused
    /// slot or a group the table does not cover.
    pub fn slot_light(&self, groups: &[Vec<u16>], texel: usize, slot: u32) -> Option<u32> {
        if texel >= self.texels() || slot > 3 {
            return None;
        }
        let word = self.ids[texel];
        let group = (word >> 24) as usize;
        let value = (word >> (6 * slot)) & 63;
        if value == 63 {
            return None;
        }
        groups.get(group)?.get(value as usize).map(|v| *v as u32)
    }

    /// True when no texel names any light.
    pub fn is_unused(&self) -> bool {
        self.ids.iter().all(|w| *w == 0x00FF_FFFF) && self.vis.iter().all(|v| *v == 0)
    }
}

/// One page of the bulk sky lightmap: two RGBA8 images one after the other.
/// Image 0 is RGBM baked light; image 1 is the light direction with the sun
/// visibility in alpha. See `light::BakedPage` for the encoding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkyPage {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
}

impl SkyPage {
    pub fn filled(width: u16, height: u16, rgba: Vec<u8>) -> Result<Self> {
        let want = width as usize * height as usize * 8;
        if rgba.len() != want {
            return Err(Error::lighting(format!(
                "a {width}x{height} sky page holds {want} bytes, got {}",
                rgba.len()
            )));
        }
        Ok(Self { width, height, rgba })
    }

    pub fn texels(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Image 0's texel: RGBM colour and scale.
    pub fn texel(&self, x: usize, y: usize) -> [u8; 4] {
        let o = (y * self.width as usize + x) * 4;
        match self.rgba.get(o..o + 4) {
            Some(s) => [s[0], s[1], s[2], s[3]],
            None => [0; 4],
        }
    }

    /// Image 1's alpha: the sun visibility.
    pub fn sun(&self, x: usize, y: usize) -> u8 {
        let o = self.texels() * 4 + (y * self.width as usize + x) * 4 + 3;
        self.rgba.get(o).copied().unwrap_or(0)
    }
}

/// Every lighting lump a v51 client map carries, decoded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LightingLumps {
    pub parent_infos: Vec<lmp::LightProbeParentInfo>,
    pub shadow_envs: Vec<lmp::ShadowEnvironment>,
    pub csm_totals: Vec<u32>,
    pub cubemaps: Vec<CubemapRecord>,
    pub cubemap_ambient: Vec<f32>,
    pub world_lights: Vec<WorldLightRecord>,
    pub tweak_lights: Vec<u32>,
    pub lightmap_headers: Vec<lmp::LightmapHeader>,
    pub sky_pages: Vec<SkyPage>,
    /// Whatever the 0x53 table does not account for, kept so the lump
    /// re-encodes byte for byte.
    pub sky_tail: Vec<u8>,
    pub rtl_pages: Vec<RtlPage>,
    pub rtl_tail: Vec<u8>,
    pub csm_nodes: Vec<CsmNodeRecord>,
    pub csm_refs: Vec<u32>,
    pub shadow_meshes: Vec<ShadowMeshRecord>,
    pub shadow_opaque: Vec<[f32; 3]>,
    pub shadow_alpha: Vec<ShadowAlphaVertex>,
    pub shadow_indices: Vec<u16>,
    pub probes: Vec<lmp::LightProbe>,
    pub prop_probe_indices: Vec<u32>,
    pub probe_tree: Vec<lmp::LightProbeTreeNode>,
    pub probe_refs: Vec<lmp::LightProbeRef>,
    /// The group -> light index table of 0x7A, split into its 63-slot groups.
    pub rtl_groups: Vec<Vec<u16>>,
    /// Slots the map carried as records that all read zero, with the record
    /// count, so the encode puts the same number of zero records back.
    pub zero_tables: Vec<(u16, usize)>,
    /// The header's declared length for every lighting slot, which for the
    /// real-time-light lump is not the size it stores.
    pub declared: std::collections::BTreeMap<u16, u32>,
}

/// Bytes a real-time-light page takes once the ids plane is padded to whole
/// 4x4 blocks, plus the BC5 visibilities over `width` by `2 * height`.
pub fn rtl_page_stored(width: u16, height: u16) -> usize {
    let (w, h) = (width as usize, height as usize);
    let padded = w.div_ceil(4) * 4 * h.div_ceil(4) * 4;
    2 * (padded + 2 * w * h)
}

/// Bytes the BC5 visibility plane of a real-time-light page takes, over
/// `width` by `2 * height` padded to whole blocks.
pub fn rtl_page_bc5_bytes(width: u16, height: u16) -> usize {
    (width as usize).div_ceil(4) * ((height as usize) * 2).div_ceil(4) * 16
}

/// The unstored BC4 plane the header counts alongside each real-time-light
/// page but the file does not store.
pub fn rtl_page_unstored(width: u16, height: u16) -> usize {
    (width as usize * height as usize) / 2
}

/// The record stride of the tables a map may carry as all-zero records.
fn fixed_stride(id: u16) -> Option<usize> {
    Some(match id {
        ids::LIGHTPROBE_PARENT_INFOS => 28,
        ids::SHADOW_ENVIRONMENTS => 36,
        ids::CSM_AABB_SUBTREE_REF_TOTALS => 4,
        ids::LIGHTPROBE_TREE => 8,
        ids::LIGHTPROBE_REFERENCES => 20,
        ids::STATIC_PROP_LIGHTPROBE_INDICES => 4,
        _ => return None,
    })
}

fn u16s(data: &[u8]) -> Vec<u16> {
    data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

fn u32s(data: &[u8]) -> Vec<u32> {
    data.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn decode_rtl_pages(data: &[u8], headers: &[lmp::LightmapHeader]) -> Result<(Vec<RtlPage>, Vec<u8>)> {
    let mut pages = Vec::new();
    let mut at = 0usize;
    for h in headers {
        if !h.has_sky_data() {
            continue;
        }
        let (w, hgt) = (h.width as usize, h.height as usize);
        let n = w * hgt;
        let stored = rtl_page_stored(h.width, h.height);
        let end = at
            .checked_add(stored)
            .ok_or_else(|| Error::lump(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS, "page size overflows"))?;
        if end > data.len() {
            return Err(Error::lump(
                ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS,
                format!("a {w}x{hgt} page of {stored} bytes runs past the lump"),
            ));
        }
        let ids = u32s(&data[at..at + n * 4]);
        let bc5 = &data[at + n * 4..end];
        let bw = w.div_ceil(4);
        let bh = (hgt * 2).div_ceil(4);
        if bc5.len() < bw * bh * 16 {
            return Err(Error::lump(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS, "the BC5 plane is short"));
        }
        let mut page = RtlPage::new(h.width, h.height);
        page.ids = ids;
        // The red blocks hold slot 0 over the top half and slot 2 over the
        // bottom; the green blocks hold slots 1 and 3.
        for (half, rslot, gslot) in [(0usize, 0usize, 1usize), (1, 2, 3)] {
            for y in 0..hgt {
                for x in 0..w {
                    let t = y * w + x;
                    let yy = y + half * hgt;
                    page.vis[t * 4 + rslot] = bc4_sample(bc5, 0, bw, bh, x, yy);
                    page.vis[t * 4 + gslot] = bc4_sample(bc5, 1, bw, bh, x, yy);
                }
            }
        }
        pages.push(RtlPage {
            raw_bc5: bc5.to_vec(),
            ..page
        });
        at = end;
    }
    Ok((pages, data[at.min(data.len())..].to_vec()))
}

fn bc4_sample(plane: &[u8], channel: usize, bw: usize, bh: usize, x: usize, y: usize) -> u8 {
    let at = (channel * bw * bh + (y / 4) * bw + x / 4) * 8;
    if at + 8 > plane.len() {
        return 0;
    }
    let block: [u8; 8] = match plane[at..at + 8].try_into() {
        Ok(b) => b,
        Err(_) => return 0,
    };
    let v = crate::bc::decode_bc4_block(&block)[(y % 4) * 4 + (x % 4)];
    if v.is_finite() {
        v.clamp(0.0, 255.0).round() as u8
    } else {
        0
    }
}

fn decode_sky_pages(data: &[u8], headers: &[lmp::LightmapHeader]) -> Result<(Vec<SkyPage>, Vec<u8>)> {
    let mut pages = Vec::new();
    let mut at = 0usize;
    for h in headers {
        if !h.has_sky_data() {
            continue;
        }
        let Some(n) = lmp::lightmap_page_bytes(h.lightmap_type, h.width, h.height) else {
            continue;
        };
        let end = at
            .checked_add(n)
            .ok_or_else(|| Error::lump(ids::LIGHTMAP_DATA_SKY, "page size overflows"))?;
        if end > data.len() {
            return Err(Error::lump(
                ids::LIGHTMAP_DATA_SKY,
                format!("a {}x{} page of {} bytes runs past the lump", h.width, h.height, n),
            ));
        }
        pages.push(SkyPage { width: h.width, height: h.height, rgba: data[at..end].to_vec() });
        at = end;
    }
    Ok((pages, data[at.min(data.len())..].to_vec()))
}

fn split_rtl_groups(data: &[u8]) -> Result<Vec<Vec<u16>>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    if !data.len().is_multiple_of(126) {
        return Err(Error::lump(
            ids::LIGHTMAP_DATA_RTL_PAGE,
            format!("{} bytes is not a whole number of 63-slot groups", data.len()),
        ));
    }
    Ok(data.chunks_exact(126).map(u16s).collect())
}

/// The probe stride the map version uses: the compact 44-byte record from v51
/// on, the padded 48-byte one before that.
pub fn probe_stride(version: Version) -> usize {
    match version {
        Version::ClientV51 => 44,
        _ => 48,
    }
}

fn parse_lightprobes(data: &[u8], version: Version) -> Result<Vec<lmp::LightProbe>> {
    let stride = probe_stride(version);
    if !data.len().is_multiple_of(stride) {
        return Err(Error::lump(
            ids::LIGHTPROBES,
            format!("{} bytes is not a whole number of {stride}-byte probes", data.len()),
        ));
    }
    let mut out = Vec::with_capacity(data.len() / stride);
    for c in data.chunks_exact(stride) {
        let mut p = lmp::LightProbe::default();
        p.ambient_sh.copy_from_slice(bytemuck::cast_slice(&c[0..24]));
        p.sky_dir_sun_vis.copy_from_slice(bytemuck::cast_slice(&c[24..32]));
        p.static_light_weights.copy_from_slice(&c[32..36]);
        p.static_light_indexes.copy_from_slice(bytemuck::cast_slice(&c[36..44]));
        out.push(p);
    }
    Ok(out)
}

impl LightingLumps {
    /// Decodes every lighting lump of a map. Lumps the map does not carry
    /// decode to empty.
    pub fn decode(bsp: &rbsp::Bsp, version: Version) -> Result<Self> {
        let wl = bsp.lump(ids::WORLD_LIGHTS);
        if !wl.len().is_multiple_of(112) {
            return Err(Error::lump(
                ids::WORLD_LIGHTS,
                format!("{} bytes is not a whole number of 112-byte lights", wl.len()),
            ));
        }
        let mut out = LightingLumps {
            parent_infos: parse_fixed(ids::LIGHTPROBE_PARENT_INFOS, bsp.lump(ids::LIGHTPROBE_PARENT_INFOS))?,
            shadow_envs: parse_fixed(ids::SHADOW_ENVIRONMENTS, bsp.lump(ids::SHADOW_ENVIRONMENTS))?,
            csm_totals: parse_fixed(ids::CSM_AABB_SUBTREE_REF_TOTALS, bsp.lump(ids::CSM_AABB_SUBTREE_REF_TOTALS))?,
            cubemaps: Vec::new(),
            cubemap_ambient: bsp
                .lump(ids::CUBEMAPS_AMBIENT_RCP)
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
            world_lights: wl.chunks_exact(112).map(parse_world_light).collect::<Result<Vec<_>>>()?,
            tweak_lights: u32s(bsp.lump(ids::TWEAK_LIGHTS)),
            lightmap_headers: parse_fixed(ids::LIGHTMAP_HEADERS, bsp.lump(ids::LIGHTMAP_HEADERS))?,
            ..LightingLumps::default()
        };
        let cm = bsp.lump(ids::CUBEMAPS);
        if !cm.len().is_multiple_of(16) {
            return Err(Error::lump(
                ids::CUBEMAPS,
                format!("{} bytes is not a whole number of 16-byte cubemaps", cm.len()),
            ));
        }
        for c in cm.chunks_exact(16) {
            out.cubemaps.push(CubemapRecord {
                origin: [
                    i32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                    i32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                    i32::from_le_bytes([c[8], c[9], c[10], c[11]]),
                ],
                texture: u32::from_le_bytes([c[12], c[13], c[14], c[15]]),
            });
        }
        let (sky, sky_tail) = decode_sky_pages(bsp.lump(ids::LIGHTMAP_DATA_SKY), &out.lightmap_headers)?;
        out.sky_pages = sky;
        out.sky_tail = sky_tail;
        let (rtl, rtl_tail) =
            decode_rtl_pages(bsp.lump(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS), &out.lightmap_headers)?;
        out.rtl_pages = rtl;
        out.rtl_tail = rtl_tail;
        out.rtl_groups = split_rtl_groups(bsp.lump(ids::LIGHTMAP_DATA_RTL_PAGE))?;
        out.csm_nodes = parse_fixed(ids::CSM_AABB_NODES, bsp.lump(ids::CSM_AABB_NODES))?
            .into_iter()
            .map(CsmNodeRecord::from_raw)
            .collect();
        out.csm_refs = u32s(bsp.lump(ids::CSM_OBJ_REFERENCES));
        out.shadow_meshes = parse_fixed::<rbsp::lumps::csm::ShadowMesh>(ids::SHADOW_MESHES, bsp.lump(ids::SHADOW_MESHES))?
            .into_iter()
            .map(|m| ShadowMeshRecord {
                first_vertex: m.first_vertex,
                triangle_count: m.triangle_count,
                draw_alpha: m.draw_alpha == 0,
                tail: m.unknown_0,
            })
            .collect();
        out.shadow_opaque = bsp.lump(ids::SHADOW_MESH_OPAQUE_VERTICES).chunks_exact(12).map(f3).collect();
        out.shadow_alpha = bsp
            .lump(ids::SHADOW_MESH_ALPHA_VERTICES)
            .chunks_exact(20)
            .map(|c| ShadowAlphaVertex {
                position: f3(&c[..12]),
                tail: [
                    u32::from_le_bytes([c[12], c[13], c[14], c[15]]),
                    u32::from_le_bytes([c[16], c[17], c[18], c[19]]),
                ],
            })
            .collect();
        out.shadow_indices = u16s(bsp.lump(ids::SHADOW_MESH_INDICES));
        out.probes = parse_lightprobes(bsp.lump(ids::LIGHTPROBES), version)?;
        out.prop_probe_indices = u32s(bsp.lump(ids::STATIC_PROP_LIGHTPROBE_INDICES));
        out.probe_tree = parse_fixed(ids::LIGHTPROBE_TREE, bsp.lump(ids::LIGHTPROBE_TREE))?;
        out.probe_refs = parse_fixed(ids::LIGHTPROBE_REFERENCES, bsp.lump(ids::LIGHTPROBE_REFERENCES))?;
        for id in [
            ids::LIGHTPROBE_PARENT_INFOS,
            ids::SHADOW_ENVIRONMENTS,
            ids::CSM_AABB_SUBTREE_REF_TOTALS,
            ids::CUBEMAPS,
            ids::CUBEMAPS_AMBIENT_RCP,
            ids::WORLD_LIGHTS,
            ids::TWEAK_LIGHTS,
            ids::LIGHTMAP_HEADERS,
            ids::LIGHTMAP_DATA_SKY,
            ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS,
            ids::LIGHTMAP_DATA_RTL_PAGE,
            ids::CSM_AABB_NODES,
            ids::CSM_OBJ_REFERENCES,
            ids::SHADOW_MESHES,
            ids::SHADOW_MESH_OPAQUE_VERTICES,
            ids::SHADOW_MESH_ALPHA_VERTICES,
            ids::SHADOW_MESH_INDICES,
            ids::LIGHTPROBES,
            ids::STATIC_PROP_LIGHTPROBE_INDICES,
            ids::LIGHTPROBE_TREE,
            ids::LIGHTPROBE_REFERENCES,
        ] {
            out.declared.insert(id, bsp.header.lumps[id as usize].length);
        }
        for (id, stride) in [
            (ids::LIGHTPROBE_PARENT_INFOS, 28usize),
            (ids::SHADOW_ENVIRONMENTS, 36),
            (ids::CSM_AABB_SUBTREE_REF_TOTALS, 4),
            (ids::LIGHTPROBE_TREE, 8),
            (ids::LIGHTPROBE_REFERENCES, 20),
            (ids::STATIC_PROP_LIGHTPROBE_INDICES, 4),
        ] {
            let n = bsp.lump(id).len();
            if n > 0 && n.is_multiple_of(stride) && n / stride > 0 {
                let empty = match id {
                    ids::LIGHTPROBE_PARENT_INFOS => out.parent_infos.is_empty(),
                    ids::SHADOW_ENVIRONMENTS => out.shadow_envs.is_empty(),
                    ids::CSM_AABB_SUBTREE_REF_TOTALS => out.csm_totals.is_empty(),
                    ids::LIGHTPROBE_TREE => out.probe_tree.is_empty(),
                    ids::LIGHTPROBE_REFERENCES => out.probe_refs.is_empty(),
                    _ => out.prop_probe_indices.is_empty(),
                };
                if empty {
                    out.zero_tables.push((id, n / stride));
                }
            }
        }
        Ok(out)
    }

    /// The length the 0x69 header declares: the stored pages plus the planes
    /// the format counts but does not store.
    pub fn rtl_declared_len(&self) -> u32 {
        if let Some(v) = self.declared.get(&ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS) {
            return *v;
        }
        let mut total: u64 = 0;
        for p in &self.rtl_pages {
            total += (rtl_page_stored(p.width, p.height) + rtl_page_unstored(p.width, p.height)) as u64;
        }
        u32::try_from(total).unwrap_or(u32::MAX)
    }

    /// Encodes the 0x69 payload: the ids plane, then the BC5 visibilities. A
    /// page that still holds its decoded plane re-emits those exact bytes.
    pub fn encode_rtl(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for p in &self.rtl_pages {
            let (w, h) = (p.width as usize, p.height as usize);
            for word in &p.ids {
                out.extend_from_slice(&word.to_le_bytes());
            }
            if p.raw_bc5.len() == rtl_page_bc5_bytes(p.width, p.height) {
                out.extend_from_slice(&p.raw_bc5);
                continue;
            }
            let mut red = Vec::with_capacity(w * h * 2);
            let mut green = Vec::with_capacity(w * h * 2);
            for half in 0..2usize {
                for y in 0..h {
                    for x in 0..w {
                        let t = y * w + x;
                        red.push(p.vis.get(t * 4 + half * 2).copied().unwrap_or(0));
                        green.push(p.vis.get(t * 4 + half * 2 + 1).copied().unwrap_or(0));
                    }
                }
            }
            out.extend_from_slice(&crate::bc::encode_bc5_image(&red, &green, w as u32, (h * 2) as u32)?);
        }
        out.extend_from_slice(&self.rtl_tail);
        Ok(out)
    }

    /// Encodes the 0x62 payload: the sky pages back to back.
    pub fn encode_sky(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for p in &self.sky_pages {
            out.extend_from_slice(&p.rgba);
        }
        out.extend_from_slice(&self.sky_tail);
        out
    }

    fn encode_rtl_table(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.rtl_groups.len() * 126);
        for g in &self.rtl_groups {
            for v in g {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    /// Writes every lighting lump this struct holds into `bsp`. A v47 map takes
    /// only the two slots the dedicated server reads.
    pub fn encode(&self, bsp: &mut rbsp::Bsp, target: Version) -> Result<()> {
        let mut light_bytes: Vec<u8> = Vec::with_capacity(self.world_lights.len() * 112);
        for l in &self.world_lights {
            light_bytes.extend_from_slice(&write_world_light(l));
        }
        bsp.set_lump(ids::WORLD_LIGHTS, light_bytes);
        bsp.set_lump(
            ids::TWEAK_LIGHTS,
            write_fixed(&self.tweak_lights.iter().map(|v| lmp::TweakLight { value: *v }).collect::<Vec<_>>()),
        );
        if target != Version::ClientV51 {
            return Ok(());
        }
        bsp.set_lump(ids::LIGHTPROBE_PARENT_INFOS, write_fixed(&self.parent_infos));
        bsp.set_lump(ids::SHADOW_ENVIRONMENTS, write_fixed(&self.shadow_envs));
        bsp.set_lump(
            ids::CSM_AABB_SUBTREE_REF_TOTALS,
            write_fixed(&self.csm_totals.iter().map(|v| rbsp::lumps::csm::CsmAabbTotals { total: *v }).collect::<Vec<_>>()),
        );
        bsp.set_lump(
            ids::CUBEMAPS,
            write_fixed(
                &self
                    .cubemaps
                    .iter()
                    .map(|c| lmp::Cubemap {
                        unknown_0: c.origin[0],
                        unknown_1: c.origin[1],
                        unknown_2: c.origin[2],
                        texture_guid: c.texture,
                    })
                    .collect::<Vec<_>>(),
            ),
        );
        bsp.set_lump(
            ids::CUBEMAPS_AMBIENT_RCP,
            write_fixed(&self.cubemap_ambient.iter().map(|s| lmp::CubemapAmbient { scale: *s }).collect::<Vec<_>>()),
        );
        bsp.set_lump(ids::LIGHTMAP_HEADERS, write_fixed(&self.lightmap_headers));
        bsp.set_lump(ids::LIGHTMAP_DATA_SKY, self.encode_sky());
        bsp.set_lump_declared(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS, self.encode_rtl()?, Some(self.rtl_declared_len()));
        bsp.set_lump(ids::LIGHTMAP_DATA_RTL_PAGE, self.encode_rtl_table());
        bsp.set_lump(
            ids::CSM_AABB_NODES,
            write_fixed(&self.csm_nodes.iter().map(|n| n.to_raw()).collect::<Vec<_>>()),
        );
        bsp.set_lump(
            ids::CSM_OBJ_REFERENCES,
            write_fixed(&self.csm_refs.iter().map(|v| rbsp::lumps::csm::CsmObjRef { value: *v }).collect::<Vec<_>>()),
        );
        bsp.set_lump(
            ids::SHADOW_MESHES,
            write_fixed(
                &self
                    .shadow_meshes
                    .iter()
                    .map(|m| rbsp::lumps::csm::ShadowMesh {
                        first_vertex: m.first_vertex,
                        triangle_count: m.triangle_count,
                        draw_alpha: u16::from(!m.draw_alpha),
                        unknown_0: m.tail,
                    })
                    .collect::<Vec<_>>(),
            ),
        );
        bsp.set_lump(
            ids::SHADOW_MESH_OPAQUE_VERTICES,
            self.shadow_opaque
                .iter()
                .flat_map(|v| v.iter())
                .flat_map(|f| f.to_le_bytes())
                .collect::<Vec<u8>>(),
        );
        bsp.set_lump(
            ids::SHADOW_MESH_ALPHA_VERTICES,
            write_fixed(
                &self
                    .shadow_alpha
                    .iter()
                    .map(|v| rbsp::lumps::csm::ShadowMeshAlphaVert { position: v.position, unknown_0: v.tail })
                    .collect::<Vec<_>>(),
            ),
        );
        bsp.set_lump(
            ids::SHADOW_MESH_INDICES,
            write_fixed(&self.shadow_indices.iter().map(|v| rbsp::lumps::csm::ShadowMeshIndex { value: *v }).collect::<Vec<_>>()),
        );
        bsp.set_lump(ids::LIGHTPROBES, write_fixed(&self.probes));
        bsp.set_lump(
            ids::STATIC_PROP_LIGHTPROBE_INDICES,
            write_fixed(&self.prop_probe_indices.iter().map(|v| lmp::PropLightprobeIndex { value: *v }).collect::<Vec<_>>()),
        );
        bsp.set_lump(ids::LIGHTPROBE_TREE, write_fixed(&self.probe_tree));
        bsp.set_lump(ids::LIGHTPROBE_REFERENCES, write_fixed(&self.probe_refs));
        // A table the map carried as zero records goes back as zero records, so
        // a slot the engine loads is still there.
        for (id, count) in &self.zero_tables {
            if let Some(stride) = fixed_stride(*id) {
                bsp.set_lump(*id, vec![0u8; count * stride]);
            }
        }
        for (id, declared) in &self.declared {
            bsp.header.lumps[*id as usize].length = *declared;
        }
        Ok(())
    }
}
