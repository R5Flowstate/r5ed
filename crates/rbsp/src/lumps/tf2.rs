//! Lumps that only exist at v37 (Titanfall 2): the collision-model table, the
//! texdata record, the CM_* brush/grid lumps and the tricoll meshes.

use crate::lumps::records::{fixed_codec, parse_fixed, write_fixed};
use crate::{Result, Version};

/// LUMP_MODELS at v37: bounds and a mesh range, 32 bytes with no BVH fields.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Tf2Model {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub first_mesh: u32,
    pub num_meshes: u32,
}

fixed_codec!(parse_models, write_models, Tf2Model, 0x0E);

/// LUMP_TEXTURE_DATA at v37: a reflection vector and six ints, 36 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Tf2TexData {
    pub reflection: [f32; 3],
    /// Index into the 0x2C offset table.
    pub name: u32,
    pub width: i32,
    pub height: i32,
    pub view_width: i32,
    pub view_height: i32,
    pub flags: i32,
}

fixed_codec!(parse_texdata, write_texdata, Tf2TexData, 0x02);

/// LUMP_LEVEL_INFO at v37: no aligned static prop index, 28 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Tf2LevelInfo {
    pub first_decal_mesh: u32,
    pub first_transparent_mesh: u32,
    pub first_sky_mesh: u32,
    pub num_static_props: u32,
    pub sun_normal: [f32; 3],
}

pub fn parse_level_info(data: &[u8], _version: Version) -> Result<Tf2LevelInfo> {
    let all = parse_fixed::<Tf2LevelInfo>(0x7B, data)?;
    all.into_iter().next().ok_or(crate::Error::Truncated { what: "level info", need: 28, have: data.len() })
}

pub fn write_level_info(info: &Tf2LevelInfo, _version: Version) -> Vec<u8> {
    write_fixed(&[*info])
}

/// LUMP_VERTS_LIT_BUMP at v37: two more dwords than the v47 record.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Tf2VertexLitBump {
    pub position_index: u32,
    pub normal_index: u32,
    pub unknown_0: [u32; 7],
    pub tangent_index: u32,
    pub unknown_1: u32,
}

fixed_codec!(parse_vertex_lit_bump, write_vertex_lit_bump, Tf2VertexLitBump, 0x49);

/// LUMP_VERTS_UNLIT_TS at v37: one dword more than the v47 record.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Tf2VertexUnlitTs {
    pub position_index: u32,
    pub normal_index: u32,
    pub unknown_0: [u32; 4],
    pub tangent_index: u32,
}

fixed_codec!(parse_vertex_unlit_ts, write_vertex_unlit_ts, Tf2VertexUnlitTs, 0x4A);

/// LUMP_CM_GRID: the single world grid record.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmGrid {
    pub scale: f32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub cells_x: i32,
    pub cells_y: i32,
    pub straddle_cell: i32,
    /// Plane index the first brush side plane offset is relative to.
    pub first_brush_plane: i32,
}

fixed_codec!(parse_cm_grid, write_cm_grid, CmGrid, 0x55);

/// LUMP_CM_GRID_CELLS: the geoset run each grid cell holds.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmGridCell {
    pub first_geoset: u16,
    pub geoset_count: u16,
}

fixed_codec!(parse_cm_grid_cells, write_cm_grid_cells, CmGridCell, 0x56);

/// LUMP_CM_GEO_SETS: either one inline primitive or a run into 0x59.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmGeoSet {
    pub straddle_cell: u16,
    pub count: u16,
    pub prim: u32,
}

impl CmGeoSet {
    pub fn primitive_kind(&self) -> u8 {
        (self.prim >> 24) as u8
    }

    pub fn primitive_index(&self) -> u32 {
        (self.prim >> 8) & 0xFFFF
    }

    pub fn contents_index(&self) -> u8 {
        self.prim as u8
    }
}

fixed_codec!(parse_cm_geo_sets, write_cm_geo_sets, CmGeoSet, 0x57);

/// A yaw-box: an int16 origin, two half-extents packed as 16.16, an int16
/// half-extent triple and the same extents turned by the box's yaw.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmBounds {
    pub origin: [i16; 3],
    /// Yaw as a 16.16 fixed point fraction of a turn.
    pub yaw: [u8; 2],
    pub extents: [i16; 3],
    /// The other half of the yaw, as 16.16.
    pub yaw_sin: [u8; 2],
}

impl CmBounds {
    pub fn yaw_cos(&self) -> f32 {
        i16::from_le_bytes(self.yaw) as f32 / 32768.0
    }

    pub fn yaw_sin(&self) -> f32 {
        i16::from_le_bytes(self.yaw_sin) as f32 / 32768.0
    }

    /// The axis-aligned box this yaw box covers.
    pub fn aabb(&self) -> ([f32; 3], [f32; 3]) {
        let o = [self.origin[0] as f32, self.origin[1] as f32, self.origin[2] as f32];
        let e = [self.extents[0] as f32, self.extents[1] as f32, self.extents[2] as f32];
        let (c, s) = (self.yaw_cos().abs(), self.yaw_sin().abs());
        let h = [c * e[0] + s * e[1], s * e[0] + c * e[1], e[2]];
        ([o[0] - h[0], o[1] - h[1], o[2] - h[2]], [o[0] + h[0], o[1] + h[1], o[2] + h[2]])
    }
}

fixed_codec!(parse_cm_geo_set_bounds, write_cm_geo_set_bounds, CmBounds, 0x58);
fixed_codec!(parse_cm_primitive_bounds, write_cm_primitive_bounds, CmBounds, 0x5A);

pub const PRIM_BRUSH: u8 = 0;
pub const PRIM_TRICOLL: u8 = 64;
pub const PRIM_PROP: u8 = 96;

/// LUMP_CM_PRIMITIVES: kind in bits 24 and up, index in bits 8..24, contents
/// index in the low byte.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmPrimitive {
    pub value: u32,
}

impl CmPrimitive {
    pub fn kind(&self) -> u8 {
        (self.value >> 24) as u8
    }

    pub fn index(&self) -> u32 {
        (self.value >> 8) & 0xFFFF
    }

    pub fn contents_index(&self) -> u8 {
        self.value as u8
    }
}

fixed_codec!(parse_cm_primitives, write_cm_primitives, CmPrimitive, 0x59);

/// LUMP_CM_UNIQUE_CONTENTS: the mask table the contents indexes name.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmUniqueContents {
    pub mask: u32,
}

fixed_codec!(parse_cm_unique_contents, write_cm_unique_contents, CmUniqueContents, 0x5B);

/// LUMP_CM_BRUSHES: an oriented box plus the run of its extra side planes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmBrush {
    pub origin: [f32; 3],
    pub non_axial_no_discard: u8,
    pub num_planes: u8,
    /// Index of this brush's first side in the 0x5E table.
    pub index: u16,
    pub extents: [f32; 3],
    pub side_offset: i32,
}

fixed_codec!(parse_cm_brushes, write_cm_brushes, CmBrush, 0x5C);

/// LUMP_CM_BRUSH_SIDE_PLANE_OFFSETS: a plane index relative to the grid's base.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmSidePlaneOffset {
    pub value: u16,
}

fixed_codec!(parse_cm_side_plane_offsets, write_cm_side_plane_offsets, CmSidePlaneOffset, 0x5D);

/// LUMP_CM_BRUSH_SIDE_PROPERTIES: a texdata index in bits 0..9 and side flags
/// above them.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmSideProp {
    pub value: u16,
}

impl CmSideProp {
    pub fn tex_data(&self) -> u32 {
        (self.value & 0x1FF) as u32
    }

    pub fn flags(&self) -> u32 {
        (self.value >> 9) as u32
    }
}

fixed_codec!(parse_cm_side_props, write_cm_side_props, CmSideProp, 0x5E);

/// LUMP_CM_BRUSH_SIDE_TEXTURE_VECTORS: two 2D texture axes per side.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CmSideTexVec {
    pub u: [f32; 2],
    pub v: [f32; 2],
}

fixed_codec!(parse_cm_side_tex_vecs, write_cm_side_tex_vecs, CmSideTexVec, 0x5F);

/// LUMP_TRICOLL_TRIANGLES: three 7-to-10 bit deltas and an edge-flags byte.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TricollTriangle {
    pub value: u32,
}

impl TricollTriangle {
    /// The three vertex deltas inside the mesh.
    pub fn deltas(&self) -> (u32, u32, u32) {
        (self.value & 0x3FF, (self.value >> 10) & 0x7F, (self.value >> 17) & 0x7F)
    }

    pub fn edge_flags(&self) -> u32 {
        (self.value >> 24) & 0xFF
    }
}

fixed_codec!(parse_tricoll_triangles, write_tricoll_triangles, TricollTriangle, 0x42);

/// LUMP_TRICOLL_HEADERS.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TricollHeader {
    pub flags: i16,
    pub texture_flags: i16,
    pub tex_data: i16,
    pub vertex_count: i16,
    pub triangle_count: i16,
    pub bevel_count: i16,
    pub first_vertex: i32,
    pub first_triangle: i32,
    pub first_node: i32,
    pub first_bevel: i32,
    pub origin: [f32; 3],
    pub scale: f32,
}

fixed_codec!(parse_tricoll_headers, write_tricoll_headers, TricollHeader, 0x45);

/// LUMP_TEXTURE_DATA_STRING_TABLE at v37: one `u32` byte offset per texdata
/// record, pointing into the 0x2B name blob.
pub fn parse_string_table(data: &[u8], _version: Version) -> Result<Vec<u32>> {
    parse_fixed(0x2C, data)
}

pub fn write_string_table(items: &[u32], _version: Version) -> Vec<u8> {
    write_fixed(items)
}
