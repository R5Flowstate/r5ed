//! Visibility lumps: the cell AABB tree, object references and bounds, and the
//! portal/cell lumps 0x6A..0x76.

use crate::lumps::records::{fixed_codec, parse_fixed, write_fixed};
use crate::{Error, Result, Version};

/// LUMP_UNKNOWN_37: per-cell-AABB-node subtree object-ref totals. The engine
/// only checks the stride, and shipped maps store a prefix of the nodes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CellAabbTotals {
    pub total: u32,
}

fixed_codec!(parse_cell_aabb_totals, write_cell_aabb_totals, CellAabbTotals, 0x25);

/// LUMP_UNKNOWN_39: one fade distance per cell AABB node, 0xFFFF for none.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FadeDist {
    pub value: u16,
}

fixed_codec!(parse_fade_dists, write_fade_dists, FadeDist, 0x27);

/// LUMP_CELL_AABB_NODES. `children` packs the count in its low byte and the
/// first child index in its high half; `obj_refs` does the same for the node's
/// own run into 0x78. Children always follow their parent.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct AabbNode {
    pub mins: [f32; 3],
    pub children: u32,
    pub maxs: [f32; 3],
    pub obj_refs: u32,
}

impl AabbNode {
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
        (self.obj_refs >> 8) & 0x7F_FFFF
    }
}

fixed_codec!(parse_aabb_nodes, write_aabb_nodes, AabbNode, 0x77);

/// LUMP_OBJ_REFS at v47/v51. Values below the level info's `first_sky_mesh`
/// are world mesh indices; from `first_static_prop_index` on they are static
/// props.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ObjRef {
    pub value: u32,
}

impl ObjRef {
    pub fn get(&self) -> u32 {
        self.value
    }
}

fixed_codec!(parse_obj_refs, write_obj_refs, ObjRef, 0x78);

/// LUMP_OBJ_REFS at v37, where the references are 16-bit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ObjRef16 {
    pub value: u16,
}

impl ObjRef16 {
    pub fn get(&self) -> u32 {
        self.value as u32
    }

    pub fn widen(&self) -> ObjRef {
        ObjRef { value: self.value as u32 }
    }
}

pub fn parse_obj_refs16(data: &[u8], _version: Version) -> Result<Vec<ObjRef16>> {
    parse_fixed(0x78, data)
}

pub fn write_obj_refs16(items: &[ObjRef16], _version: Version) -> Vec<u8> {
    write_fixed(items)
}

/// LUMP_OBJ_REF_BOUNDS: one record per object reference, in the same order.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ObjRefBounds {
    pub mins: [f32; 3],
    pub unknown_0: u32,
    pub maxs: [f32; 3],
    pub unknown_1: u32,
}

fixed_codec!(parse_obj_ref_bounds, write_obj_ref_bounds, ObjRefBounds, 0x79);

/// LUMP_CELL_BSP_NODES: a BSP over the cells. An interior node's `contents` is
/// an index into 0x01; a point on or in front of that plane goes to `child`,
/// one behind it to `child + 1`. A `contents` of -1 is a leaf whose `child` is
/// the cell index.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CellBspNode {
    pub contents: i32,
    pub child: u32,
}

impl CellBspNode {
    pub fn is_terminator(&self) -> bool {
        self.contents < 0
    }
}

fixed_codec!(parse_cell_bsp_nodes, write_cell_bsp_nodes, CellBspNode, 0x6A);

/// LUMP_CELLS: the run of 0x6C portals a cell owns. The engine flood-fills
/// through `portal_count` records from `first_portal`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Cell {
    pub portal_count: u16,
    pub first_portal: u16,
    /// The engine's `skyFlags`: bit 0 marks a cell the sky can be seen from,
    /// and the client draws the 3D skybox only from such a cell.
    pub sky_flags: u16,
    /// 0xFFFF on every shipped cell.
    pub unused: u16,
}

/// The `skyFlags` bit a cell sets when the sky is visible from it.
pub const CELL_SKY_VISIBLE: u16 = 1;
/// The `skyFlags` bit of the skybox cell: the last cell of a shipped map, with
/// no portals and an unbounded AABB root that holds only the vista. The sky
/// camera stands in it, and the 3D skybox is drawn from it.
pub const CELL_SKYBOX: u16 = 2;

impl Cell {
    pub fn portal_count(&self) -> u32 {
        self.portal_count as u32
    }

    pub fn portal_end(&self) -> usize {
        self.first_portal as usize + self.portal_count as usize
    }
}

fixed_codec!(parse_cells, write_cells, Cell, 0x6B);

/// LUMP_PORTALS: one side of a portal polygon. Its corners are the 0x70
/// vertex refs from `first_ref` and its sides the 0x71 edge refs alongside
/// them; ref `j` names the edge from corner `j - 1` to corner `j`. The corners
/// wind counter-clockwise around the plane normal, which faces into the cell
/// that owns the portal (`is_reversed` flips that for the twin in the
/// neighbouring cell).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Portal {
    pub is_reversed: u8,
    pub portal_type: u8,
    pub edge_count: u8,
    pub pad: u8,
    pub first_ref: u16,
    /// A cell index, or one past the last cell for a portal onto the sky.
    pub cell_to: u16,
    /// An index into 0x01.
    pub plane: u32,
}

/// `portal_type` of a portal between two cells.
pub const PORTAL_CELL: u8 = 0;
/// `portal_type` of a portal onto the sky. The client draws the 3D skybox only
/// when the view reaches the sky through one of these.
pub const PORTAL_SKY: u8 = 1;

impl Portal {
    pub fn ref_end(&self) -> usize {
        self.first_ref as usize + self.edge_count as usize
    }
}

fixed_codec!(parse_portals, write_portals, Portal, 0x6C);

/// LUMP_PORTAL_EDGES: the two vertices an edge joins.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PortalEdge {
    pub vert0: u16,
    pub vert1: u16,
}

fixed_codec!(parse_portal_edges, write_portal_edges, PortalEdge, 0x6E);

/// LUMP_PORTAL_VERT_EDGES: per portal vertex, the 0x6E edges that pass
/// through it; unused slots are 0xFFFF. Vertex 0 is a placeholder with none.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PortalVertEdges {
    pub edges: [u16; 8],
}

impl Default for PortalVertEdges {
    fn default() -> Self {
        Self { edges: [0xFFFF; 8] }
    }
}

fixed_codec!(parse_portal_vert_edges, write_portal_vert_edges, PortalVertEdges, 0x6F);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PortalRef {
    pub value: u16,
}

fixed_codec!(parse_portal_vert_refs, write_portal_vert_refs, PortalRef, 0x70);
fixed_codec!(parse_portal_edge_refs, write_portal_edge_refs, PortalRef, 0x71);

/// A slot in a portal edge's intersection list; 0xFFFF means unused.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct IsectSlot {
    pub value: u16,
}

/// LUMP_PORTAL_EDGE_ISECT_EDGE and LUMP_PORTAL_EDGE_ISECT_AT_VERT: eight
/// slots per record, sliced by the 0x74 header. Slot `k` of an edge's
/// ISECT_EDGE record names another edge and the same slot of its
/// ISECT_AT_VERT record the portal vertex where the two meet.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct IsectList {
    pub slots: [IsectSlot; 8],
}

impl Default for IsectList {
    fn default() -> Self {
        Self { slots: [IsectSlot { value: 0xFFFF }; 8] }
    }
}

fixed_codec!(parse_portal_isect_edges, write_portal_isect_edges, IsectList, 0x72);
fixed_codec!(parse_portal_isect_verts, write_portal_isect_verts, IsectList, 0x73);

/// LUMP_PORTAL_EDGE_ISECT_HEADER: one record per portal edge, giving the slice
/// of 0x72 and 0x73 that edge owns. The client scans at least one record
/// whatever `count` says, so every edge needs a count of one or more.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct IsectHeader {
    pub first: u32,
    pub count: u32,
}

fixed_codec!(parse_portal_isect_headers, write_portal_isect_headers, IsectHeader, 0x74);

/// LUMP_OCCLUSIONMESH_INDICES.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct OcclusionIndex {
    pub value: u16,
}

fixed_codec!(parse_occlusion_indices, write_occlusion_indices, OcclusionIndex, 0x76);

/// The byte length one portal-isect record takes.
pub const ISECT_LIST_SIZE: usize = 16;

/// Slices the 0x72/0x73 lists by the 0x74 headers, reporting a header that runs
/// past the end of its list.
pub fn isect_slices(headers: &[IsectHeader], list_len: usize) -> Result<Vec<(usize, usize)>> {
    let mut out = Vec::with_capacity(headers.len());
    for h in headers {
        let first = h.first as usize;
        let stop = first.checked_add(h.count as usize).ok_or(Error::Invalid {
            id: 0x74,
            msg: "intersection header overflows".to_string(),
        })?;
        if stop > list_len {
            return Err(Error::Invalid {
                id: 0x74,
                msg: format!("intersection header covers {first}..{stop} of {list_len} records"),
            });
        }
        out.push((first, h.count as usize));
    }
    Ok(out)
}
