//! Cascaded shadow map lumps: the caster AABB tree, its references, and the
//! world shadow meshes those references index.

use crate::lumps::records::{fixed_codec, parse_fixed, write_fixed};
use crate::{Error, Result, Version};

/// LUMP_UNKNOWN_38: per-CSM-node subtree reference totals, a prefix of the nodes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CsmAabbTotals {
    pub total: u32,
}

fixed_codec!(parse_totals, write_totals, CsmAabbTotals, 0x26);

/// LUMP_CSM_AABB_NODES. Bounds are in the environment's light frame; the
/// packing of `children` and `obj_refs` matches the cell AABB tree.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CsmAabbNode {
    pub mins: [f32; 3],
    pub children: u32,
    pub maxs: [f32; 3],
    pub obj_refs: u32,
}

impl CsmAabbNode {
    pub fn child_count(&self) -> u32 {
        self.children & 0xFF
    }

    pub fn first_child(&self) -> u32 {
        (self.children >> 8) & 0xFFFF
    }

    pub fn obj_ref_count(&self) -> u32 {
        self.obj_refs & 0xFF
    }

    pub fn first_obj_ref(&self) -> u32 {
        (self.obj_refs >> 8) & 0xFFFF
    }
}

fixed_codec!(parse_nodes, write_nodes, CsmAabbNode, 0x63);

/// LUMP_CSM_OBJ_REFS at v47/v51: a shadow mesh index.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CsmObjRef {
    pub value: u32,
}

impl CsmObjRef {
    pub fn get(&self) -> u32 {
        self.value
    }
}

fixed_codec!(parse_obj_refs, write_obj_refs, CsmObjRef, 0x64);

/// LUMP_CSM_OBJ_REFS at v37, where the references are 16-bit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CsmObjRef16 {
    pub value: u16,
}

pub fn parse_obj_refs16(data: &[u8], _version: Version) -> Result<Vec<CsmObjRef16>> {
    parse_fixed(0x64, data)
}

pub fn write_obj_refs16(items: &[CsmObjRef16], _version: Version) -> Vec<u8> {
    write_fixed(items)
}

/// LUMP_SHADOW_MESH_ALPHA_VERTS: a position plus an 8-byte stream the alpha
/// shadow pass reads.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShadowMeshAlphaVert {
    pub position: [f32; 3],
    pub unknown_0: [u32; 2],
}

fixed_codec!(parse_alpha_verts, write_alpha_verts, ShadowMeshAlphaVert, 0x7D);

/// LUMP_SHADOW_MESH_INDICES.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShadowMeshIndex {
    pub value: u16,
}

fixed_codec!(parse_indices, write_indices, ShadowMeshIndex, 0x7E);

/// LUMP_SHADOW_MESH_MESHES: which vertex run and how many triangles a world
/// shadow mesh covers. `draw` picks the opaque or alpha vertex list.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShadowMesh {
    pub first_vertex: u32,
    pub triangle_count: u32,
    /// 1 draws the alpha vertex list, anything else the opaque one.
    pub draw_alpha: u16,
    pub unknown_0: u16,
}

fixed_codec!(parse_meshes, write_meshes, ShadowMesh, 0x7F);

/// Total triangles the shadow mesh list declares; the index lump must match.
pub fn declared_triangles(meshes: &[ShadowMesh]) -> Result<u32> {
    let mut total: u64 = 0;
    for m in meshes {
        total += m.triangle_count as u64;
    }
    u32::try_from(total).map_err(|_| Error::Invalid {
        id: 0x7F,
        msg: format!("{total} shadow mesh triangles exceed the 32-bit index count"),
    })
}
