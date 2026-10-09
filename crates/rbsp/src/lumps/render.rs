//! Render lumps: planes, texdata, the name blob, the vertex formats, meshes,
//! material sorts and mesh indices.

use crate::lumps::records::fixed_codec;
use crate::lumps::strings::StringTable;
use crate::{ids, Error, Result, Version};

/// LUMP_PLANES: an outward normal and the distance every point inside sits at.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Plane {
    pub normal: [f32; 3],
    pub dist: f32,
}

fixed_codec!(parse_planes, write_planes, Plane, 0x01);

/// LUMP_TEXDATA at v47/v51: a name-blob offset and the material's size and flags.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TexData {
    /// Byte offset of this material's path in lump 0x0F.
    pub name: u32,
    pub width: i32,
    pub height: i32,
    pub flags: u32,
}

fixed_codec!(parse_texdata, write_texdata, TexData, 0x02);

pub fn parse_surface_names(data: &[u8], _version: Version) -> Result<StringTable> {
    crate::lumps::strings::parse(data, Version::ClientV51)
}

pub fn write_surface_names(table: &StringTable, version: Version) -> Vec<u8> {
    crate::lumps::strings::write(table, version)
}

/// LUMP_VERTEX_UNLIT: position and normal indices, then the packed stream.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VertexUnlit {
    pub position_index: u32,
    pub normal_index: u32,
    pub unknown_0: [u32; 3],
}

fixed_codec!(parse_vertex_unlit, write_vertex_unlit, VertexUnlit, 0x47);

/// LUMP_VERTS_LIT_FLAT: the unlit pair plus one more packed dword.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VertexLitFlat {
    pub position_index: u32,
    pub normal_index: u32,
    pub unknown_0: [u32; 5],
}

fixed_codec!(parse_vertex_lit_flat, write_vertex_lit_flat, VertexLitFlat, 0x48);

/// LUMP_VERTS_LIT_BUMP at v47/v51: the flat pair plus a tangent index and pad.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VertexLitBump {
    pub position_index: u32,
    pub normal_index: u32,
    pub unknown_0: [u32; 5],
    pub tangent_index: u32,
}

fixed_codec!(parse_vertex_lit_bump, write_vertex_lit_bump, VertexLitBump, 0x49);

/// Which vertex lump a mesh's `flags` field selects: two bits at 9. The two
/// fields disagree on every map that carries vertex data, and the byte-10 field
/// is the one whose ranges fit, so this reader exists only for callers
/// inspecting the raw bits.
pub const VERTEX_TYPE_SHIFT: u32 = 9;
pub const VERTEX_TYPE_UNLIT: u32 = 0;
pub const VERTEX_TYPE_LIT_FLAT: u32 = 1;
pub const VERTEX_TYPE_LIT_BUMP: u32 = 2;
pub const VERTEX_TYPE_UNLIT_TS: u32 = 3;

pub fn mesh_vertex_type(flags: u32) -> u32 {
    (flags >> VERTEX_TYPE_SHIFT) & 3
}

/// LUMP_VERTS_UNLIT_TS at v47/v51.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VertexUnlitTs {
    pub position_index: u32,
    pub normal_index: u32,
    pub unknown_0: [u32; 3],
    pub tangent_index: u32,
}

fixed_codec!(parse_vertex_unlit_ts, write_vertex_unlit_ts, VertexUnlitTs, 0x4A);

/// LUMP_MESHES. Field-for-field identical at v37, where offsets 20 and 21 hold
/// lightmap extents instead of the fade distance.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Mesh {
    /// First index into 0x4F.
    pub first_index: u32,
    pub triangle_count: u16,
    pub first_vertex: u16,
    pub lightmap_vertex_count: u16,
    pub vertex_type: u8,
    pub cube_index: u8,
    pub unknown_0: [u8; 4],
    /// Lightmap UV scale; the luxel extents at v37.
    pub lightmap_scale: [u16; 2],
    /// A fade distance, 0 for none.
    pub fade_distance: u16,
    /// Index into 0x52.
    pub material_sort: u16,
    pub flags: u32,
}

fixed_codec!(parse_meshes, write_meshes, Mesh, 0x50);

/// LUMP_MESH_BOUNDS: one record per mesh, in the same order.
///
/// Read off staging_mu1 against each mesh's own vertices: `origin` is the
/// centre of the vertex box and `radius` the bounding sphere's radius about it
/// (exact on every mesh checked); `extents` is the box's half size on the 2602
/// meshes whose vertices are axis-aligned and a tighter, rotated extent on the
/// rest. `tan_yaw` turns that box about Z: the engine tests a point's offset as
/// `(dx - dy*t, dx*t + dy, dz)` against `extents` scaled by `sqrt(1 + t*t)`, so 0
/// is the axis-aligned box and -1 is the box turned 45 degrees.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshBounds {
    pub origin: [f32; 3],
    pub radius: f32,
    pub extents: [f32; 3],
    pub tan_yaw: f32,
}

fixed_codec!(parse_mesh_bounds, write_mesh_bounds, MeshBounds, 0x51);

/// LUMP_MATERIAL_SORT: which material and lightmap page a mesh draws with.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaterialSort {
    /// Index into 0x02.
    pub tex_data: u16,
    /// Index into 0x53, or -1 when the mesh is unlitmapped.
    pub lightmap: i16,
    pub unknown_0: u16,
    /// Vertices before this sort's own run starts.
    pub lightmap_vertex_count: u16,
    /// First index into the mesh's vertex lump.
    pub first_vertex: u32,
}

fixed_codec!(parse_material_sorts, write_material_sorts, MaterialSort, 0x52);

/// LUMP_MESH_INDICES: 16-bit indices, two triangles per quad.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshIndex {
    pub value: u16,
}

impl MeshIndex {
    pub fn get(&self) -> u16 {
        self.value
    }
}

fixed_codec!(parse_mesh_indices, write_mesh_indices, MeshIndex, 0x4F);

/// The vertex kind a mesh draws from, and the lump that holds it. Byte 10 of
/// the record is the selector: its ranges fit the stored vertex lumps on every
/// map that has them, at every version, while the two bits at 9 of `flags` name
/// lumps those maps do not carry.
pub fn mesh_vertex_kind(mesh: &Mesh) -> (u32, u16) {
    let kind = (mesh.vertex_type as u32) & 3;
    let lump = match kind {
        VERTEX_TYPE_UNLIT => ids::VERTEX_UNLIT,
        VERTEX_TYPE_LIT_FLAT => ids::VERTEX_LIT_FLAT,
        VERTEX_TYPE_LIT_BUMP => ids::VERTEX_LIT_BUMP,
        _ => ids::VERTEX_UNLIT_TS,
    };
    (kind, lump)
}

/// Vertex counts per vertex kind, for the mesh vertex-range check.
pub fn vertex_lump_size(kind: u32, version: Version) -> Result<usize> {
    Ok(match (kind, version) {
        (VERTEX_TYPE_UNLIT, _) => 20,
        (VERTEX_TYPE_LIT_FLAT, _) => 28,
        (VERTEX_TYPE_LIT_BUMP, Version::Tf2V37) => 44,
        (VERTEX_TYPE_LIT_BUMP, _) => 32,
        (VERTEX_TYPE_UNLIT_TS, Version::Tf2V37) => 28,
        (VERTEX_TYPE_UNLIT_TS, _) => 24,
        _ => return Err(Error::Invalid { id: 0x50, msg: format!("mesh vertex kind {kind} is not one of 0..3") }),
    })
}
