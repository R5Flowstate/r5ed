//! Lump indices. Slots whose meaning changes with the map version carry both
//! names.

pub const ENTITIES: u16 = 0x00;
pub const PLANES: u16 = 0x01;
pub const TEXTURE_DATA: u16 = 0x02;
pub const VERTICES: u16 = 0x03;
pub const LIGHTPROBE_PARENT_INFOS: u16 = 0x04;
pub const SHADOW_ENVIRONMENTS: u16 = 0x05;
pub const MODELS: u16 = 0x0E;
pub const SURFACE_NAMES: u16 = 0x0F;
pub const CONTENTS_MASKS: u16 = 0x10;
pub const SURFACE_PROPERTIES: u16 = 0x11;
pub const BVH_NODES: u16 = 0x12;
pub const BVH_LEAF_DATA: u16 = 0x13;
pub const PACKED_VERTICES: u16 = 0x14;
pub const ENTITY_PARTITIONS: u16 = 0x18;
pub const VERTEX_NORMALS: u16 = 0x1E;
pub const GAME_LUMP: u16 = 0x23;
pub const CELL_AABB_SUBTREE_REF_TOTALS: u16 = 0x25;
pub const CSM_AABB_SUBTREE_REF_TOTALS: u16 = 0x26;
pub const AABB_FADE_DISTS: u16 = 0x27;
pub const PAKFILE: u16 = 0x28;
pub const CUBEMAPS: u16 = 0x2A;
/// The cubemap ambient scales from v47 on; the texdata name blob at v37.
pub const CUBEMAPS_AMBIENT_RCP: u16 = 0x2B;
/// The texdata name blob at v37; unused from v47 on.
pub const TEXDATA_STRING_TABLE: u16 = 0x2C;
pub const WORLD_LIGHTS: u16 = 0x36;
pub const WORLD_LIGHT_PARENT_INFOS: u16 = 0x37;
/// The collision mesh vertex stream at v37.
pub const TRICOLL_TRIANGLES: u16 = 0x42;
/// The collision mesh BVH nodes at v37; not read by the converters.
pub const TRICOLL_NODES: u16 = 0x44;
pub const TRICOLL_HEADERS: u16 = 0x45;
pub const VERTEX_UNLIT: u16 = 0x47;
pub const VERTEX_LIT_FLAT: u16 = 0x48;
pub const VERTEX_LIT_BUMP: u16 = 0x49;
pub const VERTEX_UNLIT_TS: u16 = 0x4A;
pub const VERTEX_BLINN_PHONG: u16 = 0x4B;
pub const MESH_INDICES: u16 = 0x4F;
pub const MESHES: u16 = 0x50;
pub const MESH_BOUNDS: u16 = 0x51;
pub const MATERIAL_SORT: u16 = 0x52;
pub const LIGHTMAP_HEADERS: u16 = 0x53;
/// The collision grid at v37; tweak light ids from v47 on.
pub const TWEAK_LIGHTS: u16 = 0x55;
pub const CM_GRID: u16 = 0x55;
pub const CM_GRID_CELLS: u16 = 0x56;
pub const CM_GEO_SETS: u16 = 0x57;
pub const CM_GEO_SET_BOUNDS: u16 = 0x58;
pub const CM_PRIMITIVES: u16 = 0x59;
pub const CM_PRIMITIVE_BOUNDS: u16 = 0x5A;
pub const CM_UNIQUE_CONTENTS: u16 = 0x5B;
pub const CM_BRUSHES: u16 = 0x5C;
pub const CM_BRUSH_SIDE_PLANE_OFFSETS: u16 = 0x5D;
pub const CM_BRUSH_SIDE_PROPERTIES: u16 = 0x5E;
pub const CM_BRUSH_SIDE_TEXTURE_VECTORS: u16 = 0x5F;
/// The compressed sky lightmaps, the uncompressed ones sitting next to it.
pub const LIGHTMAP_DATA_SKY_COMPRESSED: u16 = 0x61;
pub const LIGHTMAP_DATA_SKY: u16 = 0x62;
pub const CSM_AABB_NODES: u16 = 0x63;
pub const CSM_OBJ_REFERENCES: u16 = 0x64;
pub const LIGHTPROBES: u16 = 0x65;
pub const STATIC_PROP_LIGHTPROBE_INDICES: u16 = 0x66;
pub const LIGHTPROBE_TREE: u16 = 0x67;
pub const LIGHTPROBE_REFERENCES: u16 = 0x68;
pub const LIGHTMAP_DATA_REAL_TIME_LIGHTS: u16 = 0x69;
pub const CELL_BSP_NODES: u16 = 0x6A;
pub const CELLS: u16 = 0x6B;
pub const PORTALS: u16 = 0x6C;
pub const PORTAL_VERTICES: u16 = 0x6D;
pub const PORTAL_EDGES: u16 = 0x6E;
pub const PORTAL_VERTEX_EDGES: u16 = 0x6F;
pub const PORTAL_VERTEX_REFERENCES: u16 = 0x70;
pub const PORTAL_EDGE_REFERENCES: u16 = 0x71;
pub const PORTAL_EDGE_INTERSECT_EDGE: u16 = 0x72;
pub const PORTAL_EDGE_INTERSECT_AT_VERTEX: u16 = 0x73;
pub const PORTAL_EDGE_INTERSECT_HEADER: u16 = 0x74;
pub const OCCLUSION_MESH_VERTICES: u16 = 0x75;
pub const OCCLUSION_MESH_INDICES: u16 = 0x76;
pub const CELL_AABB_NODES: u16 = 0x77;
pub const OBJ_REFERENCES: u16 = 0x78;
pub const OBJ_REFERENCE_BOUNDS: u16 = 0x79;
pub const LIGHTMAP_DATA_RTL_PAGE: u16 = 0x7A;
pub const LEVEL_INFO: u16 = 0x7B;
pub const SHADOW_MESH_OPAQUE_VERTICES: u16 = 0x7C;
pub const SHADOW_MESH_ALPHA_VERTICES: u16 = 0x7D;
pub const SHADOW_MESH_INDICES: u16 = 0x7E;
pub const SHADOW_MESHES: u16 = 0x7F;

/// Lumps the dedicated server (v47) loads; everything else is render data.
pub const DEDI_LUMPS: &[u16] = &[
    0x00, 0x01, 0x02, 0x03, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x18, 0x23, 0x25, 0x27, 0x36, 0x50, 0x52,
    0x55, 0x6A, 0x6B, 0x77, 0x78, 0x79, 0x7B,
];

/// True when the v47 dedicated server reads this slot.
pub fn is_dedi_lump(id: u16) -> bool {
    DEDI_LUMPS.contains(&id)
}

/// The name a slot has at the given map version, or `None` for a slot no
/// shipped map populates.
pub fn name(id: u16, version: crate::Version) -> Option<&'static str> {
    use crate::Version::*;
    Some(match (id, version) {
        (ENTITIES, _) => "ENTITIES",
        (PLANES, _) => "PLANES",
        (TEXTURE_DATA, _) => "TEXDATA",
        (VERTICES, _) => "VERTEXES",
        (LIGHTPROBE_PARENT_INFOS, _) => "LIGHTPROBE_PARENT_INFOS",
        (SHADOW_ENVIRONMENTS, _) => "SHADOW_ENVIRONMENTS",
        (MODELS, _) => "MODELS",
        (SURFACE_NAMES, DediV47 | ClientV51) => "TEXDATA_STRING_DATA",
        (SURFACE_NAMES, Tf2V37) => "UNUSED",
        (CONTENTS_MASKS, DediV47 | ClientV51) => "CONTENTS_MASKS",
        (SURFACE_PROPERTIES, DediV47 | ClientV51) => "SURFACE_PROPERTIES",
        (BVH_NODES, DediV47 | ClientV51) => "BVH_NODES",
        (BVH_LEAF_DATA, DediV47 | ClientV51) => "BVH_LEAF_DATA",
        (PACKED_VERTICES, DediV47 | ClientV51) => "PACKED_VERTICES",
        (ENTITY_PARTITIONS, _) => "ENTITY_PARTITIONS",
        (VERTEX_NORMALS, _) => "VERTNORMALS",
        (GAME_LUMP, _) => "GAME_LUMP",
        (CELL_AABB_SUBTREE_REF_TOTALS, DediV47 | ClientV51) => "UNKNOWN_37",
        (CSM_AABB_SUBTREE_REF_TOTALS, DediV47 | ClientV51) => "UNKNOWN_38",
        (AABB_FADE_DISTS, DediV47 | ClientV51) => "UNKNOWN_39",
        (PAKFILE, _) => "PAKFILE",
        (CUBEMAPS, _) => "CUBEMAPS",
        (CUBEMAPS_AMBIENT_RCP, DediV47 | ClientV51) => "UNKNOWN_43",
        (CUBEMAPS_AMBIENT_RCP, Tf2V37) => "TEXDATA_STRING_DATA",
        (TEXDATA_STRING_TABLE, Tf2V37) => "TEXDATA_STRING_TABLE",
        (WORLD_LIGHTS, _) => "WORLD_LIGHTS",
        (WORLD_LIGHT_PARENT_INFOS, DediV47 | ClientV51) => "WORLD_LIGHT_PARENT_INFOS",
        (TRICOLL_TRIANGLES, Tf2V37) => "TRICOLL_TRIANGLES",
        (TRICOLL_NODES, Tf2V37) => "TRICOLL_NODES",
        (TRICOLL_HEADERS, Tf2V37) => "TRICOLL_HEADERS",
        (VERTEX_UNLIT, _) => "VERTS_UNLIT",
        (VERTEX_LIT_FLAT, _) => "VERTS_LIT_FLAT",
        (VERTEX_LIT_BUMP, _) => "VERTS_LIT_BUMP",
        (VERTEX_UNLIT_TS, _) => "VERTS_UNLIT_TS",
        (VERTEX_BLINN_PHONG, DediV47 | ClientV51) => "VERTS_BLINN_PHONG",
        (MESH_INDICES, _) => "MESH_INDICES",
        (MESHES, _) => "MESHES",
        (MESH_BOUNDS, _) => "MESH_BOUNDS",
        (MATERIAL_SORT, _) => "MATERIAL_SORT",
        (LIGHTMAP_HEADERS, _) => "LIGHTMAP_HEADERS",
        (TWEAK_LIGHTS, DediV47 | ClientV51) => "TWEAK_LIGHTS",
        (CM_GRID, Tf2V37) => "CM_GRID",
        (CM_GRID_CELLS, Tf2V37) => "CM_GRID_CELLS",
        (CM_GEO_SETS, Tf2V37) => "CM_GEO_SETS",
        (CM_GEO_SET_BOUNDS, Tf2V37) => "CM_GEO_SET_BOUNDS",
        (CM_PRIMITIVES, Tf2V37) => "CM_PRIMITIVES",
        (CM_PRIMITIVE_BOUNDS, Tf2V37) => "CM_PRIMITIVE_BOUNDS",
        (CM_UNIQUE_CONTENTS, Tf2V37) => "CM_UNIQUE_CONTENTS",
        (CM_BRUSHES, Tf2V37) => "CM_BRUSHES",
        (CM_BRUSH_SIDE_PLANE_OFFSETS, Tf2V37) => "CM_BRUSH_SIDE_PLANE_OFFSETS",
        (CM_BRUSH_SIDE_PROPERTIES, Tf2V37) => "CM_BRUSH_SIDE_PROPERTIES",
        (CM_BRUSH_SIDE_TEXTURE_VECTORS, Tf2V37) => "CM_BRUSH_SIDE_TEXTURE_VECTORS",
        (LIGHTMAP_DATA_SKY_COMPRESSED, DediV47 | ClientV51) => "LIGHTMAP_DATA_SKY_COMPRESSED",
        (LIGHTMAP_DATA_SKY, _) => "LIGHTMAP_DATA_SKY",
        (CSM_AABB_NODES, _) => "CSM_AABB_NODES",
        (CSM_OBJ_REFERENCES, _) => "CSM_OBJ_REFS",
        (LIGHTPROBES, _) => "LIGHTPROBES",
        (STATIC_PROP_LIGHTPROBE_INDICES, _) => "STATIC_PROP_LIGHTPROBE_INDEX",
        (LIGHTPROBE_TREE, _) => "LIGHTPROBETREE",
        (LIGHTPROBE_REFERENCES, _) => "LIGHTPROBEREFS",
        (LIGHTMAP_DATA_REAL_TIME_LIGHTS, _) => "LIGHTMAP_DATA_REAL_TIME_LIGHTS",
        (CELL_BSP_NODES, _) => "CELL_BSP_NODES",
        (CELLS, _) => "CELLS",
        (PORTALS, _) => "PORTALS",
        (PORTAL_VERTICES, _) => "PORTAL_VERTS",
        (PORTAL_EDGES, _) => "PORTAL_EDGES",
        (PORTAL_VERTEX_EDGES, _) => "PORTAL_VERT_EDGES",
        (PORTAL_VERTEX_REFERENCES, _) => "PORTAL_VERT_REFS",
        (PORTAL_EDGE_REFERENCES, _) => "PORTAL_EDGE_REFS",
        (PORTAL_EDGE_INTERSECT_EDGE, _) => "PORTAL_EDGE_ISECT_EDGE",
        (PORTAL_EDGE_INTERSECT_AT_VERTEX, _) => "PORTAL_EDGE_ISECT_AT_VERT",
        (PORTAL_EDGE_INTERSECT_HEADER, _) => "PORTAL_EDGE_ISECT_HEADER",
        (OCCLUSION_MESH_VERTICES, _) => "OCCLUSIONMESH_VERTS",
        (OCCLUSION_MESH_INDICES, _) => "OCCLUSIONMESH_INDICES",
        (CELL_AABB_NODES, _) => "CELL_AABB_NODES",
        (OBJ_REFERENCES, _) => "OBJ_REFS",
        (OBJ_REFERENCE_BOUNDS, _) => "OBJ_REF_BOUNDS",
        (LIGHTMAP_DATA_RTL_PAGE, _) => "LIGHTMAP_DATA_RTL_PAGE",
        (LEVEL_INFO, _) => "LEVEL_INFO",
        (SHADOW_MESH_OPAQUE_VERTICES, _) => "SHADOW_MESH_OPAQUE_VERTS",
        (SHADOW_MESH_ALPHA_VERTICES, _) => "SHADOW_MESH_ALPHA_VERTS",
        (SHADOW_MESH_INDICES, _) => "SHADOW_MESH_INDICES",
        (SHADOW_MESHES, _) => "SHADOW_MESH_MESHES",
        _ => return None,
    })
}
