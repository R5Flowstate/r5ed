//! Reader for the model export the RSX CLI writes in headless mode.
//!
//! `-nogui -export` always produces the raw form: a `<stem>.rmdl` holding the
//! `studiohdr` and its data, plus a `<stem>.vg` holding the vertex groups
//! already decompressed and concatenated. This module reads LOD0 out of that
//! pair; the SMD/QC viewer formats are GUI-only and never produced by the CLI.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The studiohdr version every export this build produces is written at. v16
/// and v17 share every field this reader touches, including the 2-byte packing
/// and the "offset counted from the field's own address" convention.
const HDR_VERSION: u16 = 17;
const HDR_MIN_SIZE: usize = 192;

/// Field offsets within `studiohdr_v16_t` / `studiohdr_v17_t` (pack(2)).
mod f {
    pub const SZNAMEINDEX: usize = 8;
    pub const CONTENTS: usize = 48;
    pub const ILLUMPOSITION: usize = 56;
    pub const HULL_MIN: usize = 68;
    pub const HULL_MAX: usize = 80;
    pub const BONE_COUNT: usize = 116;
    pub const NUM_TEXTURES: usize = 144;
    pub const TEXTURE_INDEX: usize = 146;
    pub const NUM_BODYPARTS: usize = 154;
    pub const BODYPART_INDEX: usize = 156;
    pub const SURFACEPROP_INDEX: usize = 166;
    pub const MESH_COUNT: usize = 172;
    pub const GROUP_HEADER_OFFSET: usize = 180;
    pub const GROUP_HEADER_COUNT: usize = 182;
    pub const LOD_COUNT: usize = 186;
}

/// `studio_hw_groupdata_v16_t` is 16 bytes; the group table is addressed from
/// the offset of the field that carries it, as the engine's inline helpers do.
const GROUP_TABLE_AT: usize = f::GROUP_HEADER_OFFSET;
const GROUP_SIZE: usize = 16;

/// `mstudiobodyparts_t`: two u16 indices and a u32 model count. The mesh table
/// hangs off the model index, not off the end of this record.
const BODYPART_SIZE: usize = 16;
const BODYPART_MODEL_INDEX: usize = 2;
const BODYPART_NUM_MODELS: usize = 8;
/// `mstudiomodel_t`: five u16 fields, so the record is 10 bytes.
const MODEL_SIZE: usize = 10;
const MODEL_MESH_COUNT_TOTAL: usize = 2;
const MODEL_MESH_COUNT_BASE: usize = 4;
const MODEL_MESH_OFFSET: usize = 8;
/// `mstudiomesh_t`: material index, mesh id, 4 unknown bytes, centre.
const MESH_SIZE: usize = 20;

/// rev4 `VertexGroupHeader_t`.
const VGH_LOD_COUNT: usize = 1;
const VGH_LOD_OFFSET: usize = 4;
/// rev4 `ModelLODHeader_t`. The mesh table hangs off the `meshOffset` field
/// itself, not off the end of the record, so its base is `+4`.
const MLH_MESH_COUNT: usize = 0;
const MLH_LOD_LEVEL: usize = 2;
const MLH_MESH_OFFSET: usize = 4;
const MLH_SIZE: usize = 8;
/// rev4 `MeshHeader_t`.
const MH_SIZE: usize = 48;
const MH_FLAGS: usize = 0;
const MH_VERT_COUNT: usize = 8;
const MH_VERT_CACHE_SIZE: usize = 12;
const MH_INDEX_OFFSET: usize = 16;
const MH_INDEX_COUNT: usize = 20;
const MH_VERT_OFFSET: usize = 24;

/// Mesh vertex layout flags (`VERT_*` in the engine's studio headers).
const VERT_POSITION_MASK: u64 = 0x3;
const VERT_POSITION_UNPACKED: u64 = 0x1;
const VERT_POSITION_PACKED64: u64 = 0x2;
const VERT_POSITION_PACKED48: u64 = 0x3;
const VERT_COLOR: u64 = 0x10;
const VERT_BLENDINDICES: u64 = 0x1000;
const VERT_BLENDWEIGHTS_UNPACKED: u64 = 0x2000;
const VERT_BLENDWEIGHTS_PACKED: u64 = 0x4000;
const VERT_NORMAL_UNPACKED: u64 = 0x100;
/// The four texcoord slots, slot 0 at bit 24.
const VERT_TEXCOORD_SLOTS: u32 = 4;
/// Texcoord slot counts live in a nibble per slot, slot 0 at bit 24.
const VERT_TEXCOORD_SHIFT: u32 = 24;
const VERT_TEXCOORD_NIBBLE: u64 = 0xF;


fn u16_at(b: &[u8], o: usize, what: &'static str) -> Result<u16> {
    if b.len() < o + 2 {
        return Err(Error::Truncated { what, need: o + 2, have: b.len() });
    }
    Ok(u16::from_le_bytes([b[o], b[o + 1]]))
}

fn u32_at(b: &[u8], o: usize, what: &'static str) -> Result<u32> {
    if b.len() < o + 4 {
        return Err(Error::Truncated { what, need: o + 4, have: b.len() });
    }
    Ok(u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]))
}

fn u64_at(b: &[u8], o: usize, what: &'static str) -> Result<u64> {
    if b.len() < o + 8 {
        return Err(Error::Truncated { what, need: o + 8, have: b.len() });
    }
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    Ok(u64::from_le_bytes(a))
}

fn f32x3(b: &[u8], o: usize) -> [f32; 3] {
    let g = |i: usize| {
        let v = u32::from_le_bytes([b[o + i * 4], b[o + i * 4 + 1], b[o + i * 4 + 2], b[o + i * 4 + 3]]);
        f32::from_bits(v)
    };
    [g(0), g(1), g(2)]
}

fn cstr(b: &[u8], off: usize) -> String {
    if off >= b.len() {
        return String::new();
    }
    let end = b[off..].iter().position(|&c| c == 0).unwrap_or(b.len() - off);
    String::from_utf8_lossy(&b[off..off + end]).into_owned()
}

/// Unpacks the 64-bit position: 21/21/22 unsigned bits around a -1024/-2048 bias.
pub fn unpack_position64(bits: u64) -> [f32; 3] {
    let x = (bits & 0x1F_FFFF) as f32;
    let y = ((bits >> 21) & 0x1F_FFFF) as f32;
    let z = ((bits >> 42) & 0x3F_FFFF) as f32;
    [x * 0.0009765625 - 1024.0, y * 0.0009765625 - 1024.0, z * 0.0009765625 - 2048.0]
}

/// Unpacks a `Normal32`: the dominant axis is stored as +/-255 and the other
/// two as 9-bit signed values biased by 256, then the vector is normalised.
/// `None` for axis 3, the one encoding the writer never produces.
pub fn unpack_normal32(bits: u32) -> Option<[f32; 3]> {
    let i1 = ((bits >> 29) & 3) as usize;
    if i1 > 2 {
        return None;
    }
    let sign = (bits >> 28) & 1 != 0;
    let val1 = if sign { -255.0 } else { 255.0 };
    let val2 = ((bits >> 19) & 0x1FF) as f32 - 256.0;
    let val3 = ((bits >> 10) & 0x1FF) as f32 - 256.0;
    let i2 = ((0x124u32 >> (2 * i1 + 2)) & 3) as usize;
    let i3 = ((0x124u32 >> (2 * i1 + 4)) & 3) as usize;
    let len = (255.0f32 * 255.0 + val2 * val2 + val3 * val3).sqrt();
    let mut out = [0.0f32; 3];
    if len > 0.0 {
        out[i1] = val1 / len;
        out[i2] = val2 / len;
        out[i3] = val3 / len;
    }
    Some(out)
}

/// The studiohdr stores its hull on rotated axes relative to the vertex blocks:
/// hdr `(x, y, z)` is vertex space `(y, z, x)`. Vertex positions are already in
/// render space and must not be touched, so this maps a header vector into it.
pub fn studiohdr_to_vertex_space(v: [f32; 3]) -> [f32; 3] {
    [v[1], v[2], v[0]]
}

/// The inverse: a vertex-space vector back on the header's axes, which are the
/// axes a map places the model on. A static prop's world box is its header
/// hull turned by the prop's angles.
pub fn vertex_to_studiohdr_space(v: [f32; 3]) -> [f32; 3] {
    [v[2], v[0], v[1]]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mesh {
    pub material: u16,
    pub center: [f32; 3],
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uv0: Vec<[f32; 2]>,
    pub colors: Vec<[u8; 4]>,
    pub indices: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RmdlModel {
    pub name: String,
    pub version: u16,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub illum_position: [f32; 3],
    pub bone_count: u16,
    /// The studiohdr `contents` mask; non-zero means the model has collision.
    pub contents: i32,
    /// One texture guid per material slot, in slot order.
    pub textures: Vec<u64>,
    pub surface_prop: String,
    pub meshes: Vec<Mesh>,
}

/// Parses one `vertCacheSize` record out of a mesh's vertex block.
fn parse_vertex(vg: &[u8], at: usize, stride: usize, flags: u64) -> Result<MeshVertex> {
    let end = at.checked_add(stride).ok_or(Error::Truncated { what: "vertex", need: usize::MAX, have: vg.len() })?;
    if end > vg.len() {
        return Err(Error::Truncated { what: "vertex", need: end, have: vg.len() });
    }
    if flags & VERT_BLENDWEIGHTS_UNPACKED != 0 {
        return Err(Error::Malformed {
            what: "mesh",
            msg: "unpacked blend weights are never written by the engine".into(),
        });
    }
    let mut p = at;
    let position = match flags & VERT_POSITION_MASK {
        VERT_POSITION_UNPACKED => {
            if p + 12 > end {
                return Err(Error::Truncated { what: "vertex position", need: p + 12, have: vg.len() });
            }
            let v = f32x3(vg, p);
            p += 12;
            v
        }
        VERT_POSITION_PACKED64 => {
            if p + 8 > end {
                return Err(Error::Truncated { what: "vertex position", need: p + 8, have: vg.len() });
            }
            let v = unpack_position64(u64_at(vg, p, "vertex position")?);
            p += 8;
            v
        }
        VERT_POSITION_PACKED48 => {
            // The engine reads six bytes and discards them, so no position
            // survives this encoding and there is nothing to skip past.
            return Err(Error::VertexPosition(VERT_POSITION_PACKED48));
        }
        _ => return Err(Error::VertexPosition(0)),
    };
    if flags & (VERT_BLENDINDICES | VERT_BLENDWEIGHTS_PACKED) != 0 {
        // Two packed weights then three bone indices and a count. A preview
        // draws every mesh with the bind pose, so the values are skipped.
        if p + 8 > end {
            return Err(Error::Truncated { what: "vertex weights", need: p + 8, have: vg.len() });
        }
        p += 8;
    }
    let normal = if flags & VERT_NORMAL_UNPACKED != 0 {
        if p + 12 > end {
            return Err(Error::Truncated { what: "vertex normal", need: p + 12, have: vg.len() });
        }
        let v = f32x3(vg, p);
        p += 12;
        v
    } else {
        if p + 4 > end {
            return Err(Error::Truncated { what: "vertex normal", need: p + 4, have: vg.len() });
        }
        let v = unpack_normal32(u32_at(vg, p, "vertex normal")?).ok_or(Error::NormalAxis)?;
        p += 4;
        v
    };
    let mut color = [255u8, 255, 255, 255];
    if flags & VERT_COLOR != 0 {
        if p + 4 > end {
            return Err(Error::Truncated { what: "vertex colour", need: p + 4, have: vg.len() });
        }
        color.copy_from_slice(&vg[p..p + 4]);
        p += 4;
    }
    let mut uv0 = None;
    for slot in 0..VERT_TEXCOORD_SLOTS {
        let nibble = (flags >> (VERT_TEXCOORD_SHIFT + slot * 4)) & VERT_TEXCOORD_NIBBLE;
        if nibble == 0 {
            continue;
        }
        if p + 8 > end {
            return Err(Error::Truncated { what: "vertex texcoord", need: p + 8, have: vg.len() });
        }
        let t = [
            f32::from_bits(u32_at(vg, p, "vertex texcoord")?),
            f32::from_bits(u32_at(vg, p + 4, "vertex texcoord")?),
        ];
        p += 8;
        // A mesh can leave slot 0 empty and only carry a later slot, so the
        // first slot that is actually present becomes uv0.
        if uv0.is_none() {
            uv0 = Some(t);
        }
    }
    Ok(MeshVertex { position, normal, uv0, color })
}

/// One decoded vertex. `uv0` is `None` for a mesh whose layout has no texcoords.
struct MeshVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv0: Option<[f32; 2]>,
    color: [u8; 4],
}

////// The export is the studiohdr verbatim, so it carries no version field: the
////// asset's own version lives in the packed header the pak holds, not here.
////// v16 and v17 share every field this reader touches, so one constant covers
////// both and the file length is only used to reject a runt.
fn check_header(rmdl: &[u8]) -> Result<()> {
    if rmdl.len() < HDR_MIN_SIZE {
        return Err(Error::Truncated { what: "studiohdr", need: HDR_MIN_SIZE, have: rmdl.len() });
    }
    Ok(())
}

/// Reads a model from the two files the RSX CLI writes for one asset.
pub fn parse_model(rmdl: &[u8], vg: &[u8]) -> Result<RmdlModel> {
    check_header(rmdl)?;
    let version = HDR_VERSION;
    let bounds_min = studiohdr_to_vertex_space(f32x3(rmdl, f::HULL_MIN));
    let bounds_max = studiohdr_to_vertex_space(f32x3(rmdl, f::HULL_MAX));
    let illum_position = studiohdr_to_vertex_space(f32x3(rmdl, f::ILLUMPOSITION));
    let bone_count = u16_at(rmdl, f::BONE_COUNT, "bone count")?;
    let contents = u32_at(rmdl, f::CONTENTS, "contents")? as i32;
    let num_textures = u16_at(rmdl, f::NUM_TEXTURES, "texture count")? as usize;
    let texture_index = u16_at(rmdl, f::TEXTURE_INDEX, "texture index")? as usize;
    let num_bodyparts = u16_at(rmdl, f::NUM_BODYPARTS, "bodypart count")? as usize;
    let bodypart_index = u16_at(rmdl, f::BODYPART_INDEX, "bodypart index")? as usize;
    let surface_prop_index = u16_at(rmdl, f::SURFACEPROP_INDEX, "surface prop index")? as usize;
    let group_offset = u16_at(rmdl, f::GROUP_HEADER_OFFSET, "group offset")? as usize;
    let group_count = u16_at(rmdl, f::GROUP_HEADER_COUNT, "group count")? as usize;
    let lod_count = u16_at(rmdl, f::LOD_COUNT, "lod count")? as usize;
    let hdr_mesh_count = u16_at(rmdl, f::MESH_COUNT, "mesh count")? as usize;
    if group_count > 64 || lod_count > 16 || num_bodyparts > 256 || num_textures > 1024 {
        return Err(Error::Malformed { what: "studiohdr", msg: "out-of-range table counts".into() });
    }
    if hdr_mesh_count > 65535 {
        return Err(Error::Malformed { what: "studiohdr", msg: "mesh count out of range".into() });
    }
    let mut textures = Vec::with_capacity(num_textures);
    for i in 0..num_textures {
        textures.push(u64_at(rmdl, texture_index + i * 8, "texture guid")?);
    }
    let surface_prop = if surface_prop_index > 0 { cstr(rmdl, surface_prop_index) } else { String::new() };

    // Each group's decompressed bytes sit back to back in the .vg in table
    // order, so a group's base is the running sum of the declared sizes.
    let group_table = GROUP_TABLE_AT.checked_add(group_offset).ok_or(Error::Truncated {
        what: "vertex group table",
        need: usize::MAX,
        have: rmdl.len(),
    })?;
    let mut group_base = 0usize;
    let mut lod0_group: Option<usize> = None;
    for g in 0..group_count {
        let at = group_table + g * GROUP_SIZE;
        let size = u32_at(rmdl, at + 8, "group decompressed size")? as usize;
        if g == 0 && group_base + VGH_LOD_OFFSET + 4 <= vg.len() && vg[group_base + 3] & 1 != 0 {
            lod0_group = Some(group_base);
        }
        group_base = group_base.saturating_add(size);
    }
    let gbase = lod0_group
        .ok_or(Error::Malformed { what: "model", msg: "no vertex group carries LOD0".into() })?;

    // Walk the bodypart/model/mesh table to learn each mesh's material slot.
    let mut slots: Vec<(u16, u16, [f32; 3])> = Vec::new();
    for b in 0..num_bodyparts {
        let bp = bodypart_index + b * BODYPART_SIZE;
        let model_index = u16_at(rmdl, bp + BODYPART_MODEL_INDEX, "bodypart model index")? as usize;
        let num_models = u32_at(rmdl, bp + BODYPART_NUM_MODELS, "bodypart model count")? as usize;
        if num_models > 4096 {
            return Err(Error::Malformed { what: "model", msg: "bodypart model count out of range".into() });
        }
            for m in 0..num_models {
                let mo = bodypart_index + model_index + m * MODEL_SIZE;
            let mesh_count_total = u16_at(rmdl, mo + MODEL_MESH_COUNT_TOTAL, "model mesh count")? as usize;
            let mesh_count_base = u16_at(rmdl, mo + MODEL_MESH_COUNT_BASE, "model base mesh count")? as usize;
            let mesh_offset = u16_at(rmdl, mo + MODEL_MESH_OFFSET, "model mesh offset")? as usize;
            for q in 0..mesh_count_total.min(mesh_count_base) {
                let qo = bodypart_index + model_index + mesh_offset + q * MESH_SIZE;
                let material = u16_at(rmdl, qo, "mesh material")?;
                let mesh_id = u16_at(rmdl, qo + 2, "mesh id")?;
                let center = f32x3(rmdl, qo + 8);
                slots.push((mesh_id, material, studiohdr_to_vertex_space(center)));
            }
        }
    }
    let meshes = read_lod_meshes(vg, gbase, &slots)?;
    let name = cstr(rmdl, u16_at(rmdl, f::SZNAMEINDEX, "name index")? as usize);
    Ok(RmdlModel {
        name,
        version,
        bounds_min,
        bounds_max,
        illum_position,
        bone_count,
        contents,
        textures,
        surface_prop,
        meshes,
    })
}

/// Decodes every mesh of the LOD whose level is 0 out of one vertex group.
fn read_lod_meshes(vg: &[u8], gbase: usize, slots: &[(u16, u16, [f32; 3])]) -> Result<Vec<Mesh>> {
    if gbase + VGH_LOD_OFFSET + 4 > vg.len() {
        return Err(Error::Truncated { what: "vertex group header", need: gbase + 8, have: vg.len() });
    }
    let lod_count = vg[gbase + VGH_LOD_COUNT] as usize;
    let lod_offset = u32_at(vg, gbase + VGH_LOD_OFFSET, "lod table offset")? as usize;
    let mut out = Vec::new();
    if lod_count == 0 || lod_count > 8 {
        return Ok(out);
    }
    for j in 0..lod_count {
        let lo = gbase + VGH_LOD_OFFSET + lod_offset + j * MLH_SIZE;
        if lo + MLH_SIZE > vg.len() {
            return Err(Error::Truncated { what: "lod header", need: lo + MLH_SIZE, have: vg.len() });
        }
        if vg[lo + MLH_LOD_LEVEL] != 0 {
            continue;
        }
        let mesh_count = vg[lo + MLH_MESH_COUNT] as usize;
        let mesh_offset = u32_at(vg, lo + MLH_MESH_OFFSET, "mesh table offset")? as usize;
        let table = lo + MLH_MESH_OFFSET + mesh_offset;
        for k in 0..mesh_count {
            let mb = table + k * MH_SIZE;
            if mb + MH_SIZE > vg.len() {
                return Err(Error::Truncated { what: "mesh header", need: mb + MH_SIZE, have: vg.len() });
            }
            let flags = u64_at(vg, mb + MH_FLAGS, "mesh flags")?;
            if flags == 0 {
                continue;
            }
            let vert_count = u32_at(vg, mb + MH_VERT_COUNT, "mesh vertex count")? as usize;
            let stride = u16_at(vg, mb + MH_VERT_CACHE_SIZE, "mesh vertex stride")? as usize;
            if vert_count > 8_000_000 || stride == 0 || stride > 4096 {
                return Err(Error::Malformed { what: "mesh", msg: "vertex count or stride out of range".into() });
            }
            let index_offset = u32_at(vg, mb + MH_INDEX_OFFSET, "mesh index offset")? as usize;
            // indexCount:28 and indexType:4 share one word. indexType is a
            // strip-group flag rather than a width; the engine reads u16.
            let packed = u32_at(vg, mb + MH_INDEX_COUNT, "mesh index count")?;
            let index_count = (packed & 0x0FFF_FFFF) as usize;
            if index_count > 24_000_000 || index_count % 3 != 0 {
                return Err(Error::Malformed { what: "mesh", msg: "index count out of range".into() });
            }
            let vert_offset = u32_at(vg, mb + MH_VERT_OFFSET, "mesh vertex offset")? as usize;
            let vb = mb + MH_VERT_OFFSET + vert_offset;
            let ib = mb + MH_INDEX_OFFSET + index_offset;
            if vb + stride * vert_count > vg.len() {
                return Err(Error::Truncated { what: "mesh vertices", need: vb + stride * vert_count, have: vg.len() });
            }
            if ib + index_count * 2 > vg.len() {
                return Err(Error::Truncated { what: "mesh indices", need: ib + index_count * 2, have: vg.len() });
            }
            let mut positions = Vec::with_capacity(vert_count);
            let mut normals = Vec::with_capacity(vert_count);
            let mut uv0 = Vec::with_capacity(vert_count);
            let mut colors = Vec::with_capacity(vert_count);
            for vi in 0..vert_count {
                let v = parse_vertex(vg, vb + vi * stride, stride, flags)?;
                positions.push(v.position);
                normals.push(v.normal);
                uv0.push(v.uv0.unwrap_or([0.0, 0.0]));
                colors.push(v.color);
            }
            let mut indices = Vec::with_capacity(index_count);
            for i in 0..index_count {
                let idx = u16_at(vg, ib + i * 2, "mesh index")?;
                if idx as usize >= vert_count {
                    return Err(Error::Malformed {
                        what: "mesh",
                        msg: format!("index {idx} is outside {vert_count} vertices"),
                    });
                }
                indices.push(idx);
            }
            let (material, center) = slots
                .iter()
                .find(|(id, _, _)| *id as usize == k)
                .map_or((0, [0.0; 3]), |&(_, m, c)| (m, c));
            out.push(Mesh { material, center, positions, normals, uv0, colors, indices });
        }
        break;
    }
    Ok(out)
}
