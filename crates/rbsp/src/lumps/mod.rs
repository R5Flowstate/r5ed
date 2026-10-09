//! Typed codecs for every lump of v37, v47 and v51, and the dispatch that
//! picks one from a lump index and version.

pub mod collision;
pub mod csm;
pub mod entities;
pub mod game_lump;
pub mod level;
pub mod lighting;
pub mod prims;
pub mod records;
pub mod render;
pub mod strings;
pub mod tf2;
pub mod vis;

use crate::{ids, Result, Version};

/// One decoded lump. `Opaque` carries a lump this crate gives no structure to;
/// the bytes are kept verbatim, so it still round trips.
#[derive(Clone, Debug, PartialEq)]
pub enum Lump {
    Opaque(Vec<u8>),
    EntityText(entities::EntityText),
    EntityPartitions(level::EntityPartitions),
    LevelInfo(level::LevelInfo),
    Tf2LevelInfo(tf2::Tf2LevelInfo),
    GameLump(game_lump::GameLump),
    SurfaceNames(strings::StringTable),
    StringTable(strings::StringTable),
    U32Table(Vec<u32>),
    BvhLeaf(collision::BvhLeafData),
    LightmapPages(lighting::LightmapPages),
    Vec3f(Vec<prims::Vec3f>),
    Plane(Vec<render::Plane>),
    TexData(Vec<render::TexData>),
    Tf2TexData(Vec<tf2::Tf2TexData>),
    Model(Vec<collision::Model>),
    Tf2Model(Vec<tf2::Tf2Model>),
    ContentMask(Vec<collision::ContentMask>),
    SurfaceProp(Vec<collision::SurfaceProp>),
    BvhNode(Vec<collision::BvhNode>),
    PackVert(Vec<collision::PackVert>),
    CellAabbTotals(Vec<vis::CellAabbTotals>),
    CsmAabbTotals(Vec<csm::CsmAabbTotals>),
    FadeDist(Vec<vis::FadeDist>),
    Cubemap(Vec<lighting::Cubemap>),
    CubemapAmbient(Vec<lighting::CubemapAmbient>),
    WorldLight(Vec<lighting::WorldLight>),
    TweakLight(Vec<lighting::TweakLight>),
    LightmapHeader(Vec<lighting::LightmapHeader>),
    LightProbe(Vec<lighting::LightProbe>),
    LightProbe48(Vec<lighting::LightProbe48>),
    PropLightprobeIndex(Vec<lighting::PropLightprobeIndex>),
    LightProbeTreeNode(Vec<lighting::LightProbeTreeNode>),
    LightProbeRef(Vec<lighting::LightProbeRef>),
    ShadowEnvironment(Vec<lighting::ShadowEnvironment>),
    LightProbeParentInfo(Vec<lighting::LightProbeParentInfo>),
    RtlIndex(Vec<lighting::RtlIndex>),
    CellBspNode(Vec<vis::CellBspNode>),
    Cell(Vec<vis::Cell>),
    Portal(Vec<vis::Portal>),
    PortalEdge(Vec<vis::PortalEdge>),
    PortalVertEdges(Vec<vis::PortalVertEdges>),
    PortalRef(Vec<vis::PortalRef>),
    PortalIsectList(Vec<vis::IsectList>),
    PortalIsectHeader(Vec<vis::IsectHeader>),
    OcclusionIndex(Vec<vis::OcclusionIndex>),
    AabbNode(Vec<vis::AabbNode>),
    ObjRef(Vec<vis::ObjRef>),
    ObjRef16(Vec<vis::ObjRef16>),
    ObjRefBounds(Vec<vis::ObjRefBounds>),
    ShadowMeshAlphaVert(Vec<csm::ShadowMeshAlphaVert>),
    ShadowMeshIndex(Vec<csm::ShadowMeshIndex>),
    ShadowMesh(Vec<csm::ShadowMesh>),
    CsmAabbNode(Vec<csm::CsmAabbNode>),
    CsmObjRef(Vec<csm::CsmObjRef>),
    CsmObjRef16(Vec<csm::CsmObjRef16>),
    Mesh(Vec<render::Mesh>),
    MeshBounds(Vec<render::MeshBounds>),
    MaterialSort(Vec<render::MaterialSort>),
    MeshIndex(Vec<render::MeshIndex>),
    VertexUnlit(Vec<render::VertexUnlit>),
    VertexLitFlat(Vec<render::VertexLitFlat>),
    VertexLitBump(Vec<render::VertexLitBump>),
    VertexUnlitTs(Vec<render::VertexUnlitTs>),
    Tf2VertexLitBump(Vec<tf2::Tf2VertexLitBump>),
    Tf2VertexUnlitTs(Vec<tf2::Tf2VertexUnlitTs>),
    CmGrid(Vec<tf2::CmGrid>),
    CmGridCell(Vec<tf2::CmGridCell>),
    CmGeoSet(Vec<tf2::CmGeoSet>),
    CmBounds(Vec<tf2::CmBounds>),
    CmPrimitive(Vec<tf2::CmPrimitive>),
    CmUniqueContents(Vec<tf2::CmUniqueContents>),
    CmBrush(Vec<tf2::CmBrush>),
    CmSidePlaneOffset(Vec<tf2::CmSidePlaneOffset>),
    CmSideProp(Vec<tf2::CmSideProp>),
    CmSideTexVec(Vec<tf2::CmSideTexVec>),
    TricollTriangle(Vec<tf2::TricollTriangle>),
    TricollHeader(Vec<tf2::TricollHeader>),
}


impl Lump {
    /// The name of the codec this lump was parsed with.
    pub fn kind(&self) -> &'static str {
        use Lump::*;
        match self {
            Opaque(_) => "opaque",
            EntityText(_) => "entities",
            EntityPartitions(_) => "entity_partitions",
            LevelInfo(_) => "level_info",
            Tf2LevelInfo(_) => "level_info_v37",
            GameLump(_) => "game_lump",
            Plane(_) => "plane",
            TexData(_) => "texdata",
            Tf2TexData(_) => "texdata_v37",
            SurfaceNames(_) => "surface_names",
            StringTable(_) => "string_table",
            U32Table(_) => "u32_table",
            Vec3f(_) => "vec3",
            Model(_) => "model",
            Tf2Model(_) => "model_v37",
            ContentMask(_) => "content_mask",
            SurfaceProp(_) => "surface_prop",
            BvhNode(_) => "bvh_node",
            BvhLeaf(_) => "bvh_leaf",
            PackVert(_) => "pack_vert",
            CellAabbTotals(_) => "cell_aabb_totals",
            CsmAabbTotals(_) => "csm_aabb_totals",
            FadeDist(_) => "fade_dist",
            Cubemap(_) => "cubemap",
            CubemapAmbient(_) => "cubemap_ambient",
            LightmapPages(_) => "lightmap_pages",
            WorldLight(_) => "world_light",
            TweakLight(_) => "tweak_light",
            LightmapHeader(_) => "lightmap_header",
            LightProbe(_) => "lightprobe",
            LightProbe48(_) => "lightprobe_padded",
            PropLightprobeIndex(_) => "prop_lightprobe_index",
            LightProbeTreeNode(_) => "lightprobe_tree",
            LightProbeRef(_) => "lightprobe_ref",
            ShadowEnvironment(_) => "shadow_environment",
            LightProbeParentInfo(_) => "lightprobe_parent_info",
            RtlIndex(_) => "rtl_index",
            CellBspNode(_) => "cell_bsp_node",
            Cell(_) => "cell",
            Portal(_) => "portal",
            PortalEdge(_) => "portal_edge",
            PortalVertEdges(_) => "portal_vert_edges",
            PortalRef(_) => "portal_ref",
            PortalIsectList(_) => "portal_isect_list",
            PortalIsectHeader(_) => "portal_isect_header",
            OcclusionIndex(_) => "occlusion_index",
            AabbNode(_) => "cell_aabb_node",
            ObjRef(_) => "obj_ref",
            ObjRef16(_) => "obj_ref_narrow",
            ObjRefBounds(_) => "obj_ref_bounds",
            ShadowMeshAlphaVert(_) => "shadow_mesh_alpha_vert",
            ShadowMeshIndex(_) => "shadow_mesh_index",
            ShadowMesh(_) => "shadow_mesh",
            CsmAabbNode(_) => "csm_aabb_node",
            CsmObjRef(_) => "csm_obj_ref",
            CsmObjRef16(_) => "csm_obj_ref_narrow",
            Mesh(_) => "mesh",
            MeshBounds(_) => "mesh_bounds",
            MaterialSort(_) => "material_sort",
            MeshIndex(_) => "mesh_index",
            VertexUnlit(_) => "vertex_unlit",
            VertexLitFlat(_) => "vertex_lit_flat",
            VertexLitBump(_) => "vertex_lit_bump",
            VertexUnlitTs(_) => "vertex_unlit_ts",
            Tf2VertexLitBump(_) => "vertex_lit_bump_v37",
            Tf2VertexUnlitTs(_) => "vertex_unlit_ts_v37",
            CmGrid(_) => "cm_grid",
            CmGridCell(_) => "cm_grid_cell",
            CmGeoSet(_) => "cm_geo_set",
            CmBounds(_) => "cm_bounds",
            CmPrimitive(_) => "cm_primitive",
            CmUniqueContents(_) => "cm_unique_contents",
            CmBrush(_) => "cm_brush",
            CmSidePlaneOffset(_) => "cm_side_plane_offset",
            CmSideProp(_) => "cm_side_prop",
            CmSideTexVec(_) => "cm_side_tex_vec",
            TricollTriangle(_) => "tricoll_triangle",
            TricollHeader(_) => "tricoll_header",
        }
    }

    /// The 0x18 partition list, when this is one.
    pub fn as_entity_partitions(&self) -> Option<&level::EntityPartitions> {
        match self {
            Lump::EntityPartitions(p) => Some(p),
            _ => None,
        }
    }

    /// The static prop list, whether it came from the game lump or on its own.
    pub fn as_static_props(&self) -> Option<&game_lump::sprp::StaticProps> {
        match self {
            Lump::GameLump(g) => g.static_props(),
            _ => None,
        }
    }

    /// Object references, widened when the map stores the narrow v37 form.
    pub fn obj_refs(&self) -> Option<Vec<u32>> {
        match self {
            Lump::ObjRef(r) => Some(r.iter().map(|o| o.get()).collect()),
            Lump::ObjRef16(r) => Some(r.iter().map(|o| o.get()).collect()),
            _ => None,
        }
    }
}

/// `base` is the lump's own file offset. Only the game lump needs it: its
/// block pointers are absolute against the packed file, so a caller holding
/// the header entry must pass `header.lumps[id].offset`.
pub fn parse_at(id: u16, version: Version, data: &[u8], base: u32) -> Result<Lump> {
    use Version::*;
    let v = version;
    match (id, v) {
        (ids::ENTITIES, _) => Ok(Lump::EntityText(entities::parse(data, v)?)),
        (ids::ENTITY_PARTITIONS, _) => Ok(Lump::EntityPartitions(level::parse_entity_partitions(data, v)?)),
        (ids::PLANES, _) => Ok(Lump::Plane(render::parse_planes(data, v)?)),
        (ids::TEXTURE_DATA, DediV47 | ClientV51) => Ok(Lump::TexData(render::parse_texdata(data, v)?)),
        (ids::TEXTURE_DATA, Tf2V37) => Ok(Lump::Tf2TexData(tf2::parse_texdata(data, v)?)),
        (ids::VERTICES, _) => Ok(Lump::Vec3f(prims::parse(data, v)?)),
        (ids::LIGHTPROBE_PARENT_INFOS, _) => Ok(Lump::LightProbeParentInfo(lighting::parse_parent_infos(data, v)?)),
        (ids::SHADOW_ENVIRONMENTS, _) => Ok(Lump::ShadowEnvironment(lighting::parse_shadow_envs(data, v)?)),
        (ids::MODELS, DediV47 | ClientV51) => Ok(Lump::Model(collision::parse_models(data, v)?)),
        (ids::MODELS, Tf2V37) => Ok(Lump::Tf2Model(tf2::parse_models(data, v)?)),
        (ids::SURFACE_NAMES, DediV47 | ClientV51) => Ok(Lump::SurfaceNames(render::parse_surface_names(data, v)?)),
        (ids::SURFACE_NAMES, Tf2V37) => Ok(Lump::StringTable(strings::parse(data, v)?)),
        (ids::CONTENTS_MASKS, DediV47 | ClientV51) => Ok(Lump::ContentMask(collision::parse_content_masks(data, v)?)),
        (ids::SURFACE_PROPERTIES, DediV47 | ClientV51) => Ok(Lump::SurfaceProp(collision::parse_surface_props(data, v)?)),
        (ids::BVH_NODES, DediV47 | ClientV51) => Ok(Lump::BvhNode(collision::parse_bvh_nodes(data, v)?)),
        (ids::BVH_LEAF_DATA, DediV47 | ClientV51) => Ok(Lump::BvhLeaf(collision::parse_bvh_leaf(data, v)?)),
        (ids::PACKED_VERTICES, DediV47 | ClientV51) => Ok(Lump::PackVert(collision::parse_pack_verts(data, v)?)),
        (ids::VERTEX_NORMALS, _) => Ok(Lump::Vec3f(prims::parse(data, v)?)),
        (ids::GAME_LUMP, _) => Ok(Lump::GameLump(game_lump::parse_at(data, v, base)?)),
        (ids::CELL_AABB_SUBTREE_REF_TOTALS, DediV47 | ClientV51) => Ok(Lump::CellAabbTotals(vis::parse_cell_aabb_totals(data, v)?)),
        (ids::CSM_AABB_SUBTREE_REF_TOTALS, DediV47 | ClientV51) => Ok(Lump::CsmAabbTotals(csm::parse_totals(data, v)?)),
        (ids::AABB_FADE_DISTS, DediV47 | ClientV51) => Ok(Lump::FadeDist(vis::parse_fade_dists(data, v)?)),
        (ids::CUBEMAPS, _) => Ok(Lump::Cubemap(lighting::parse_cubemaps(data, v)?)),
        (ids::CUBEMAPS_AMBIENT_RCP, DediV47 | ClientV51) => Ok(Lump::CubemapAmbient(lighting::parse_cubemap_ambient(data, v)?)),
        (ids::CUBEMAPS_AMBIENT_RCP, Tf2V37) => Ok(Lump::StringTable(strings::parse(data, v)?)),
        (ids::TEXDATA_STRING_TABLE, Tf2V37) => Ok(Lump::U32Table(strings::parse_table(data, v)?)),
        (ids::WORLD_LIGHTS, _) => Ok(Lump::WorldLight(lighting::parse_world_lights(data, v)?)),
        (ids::TRICOLL_TRIANGLES, Tf2V37) => Ok(Lump::TricollTriangle(tf2::parse_tricoll_triangles(data, v)?)),
        (ids::TRICOLL_HEADERS, Tf2V37) => Ok(Lump::TricollHeader(tf2::parse_tricoll_headers(data, v)?)),
        (ids::VERTEX_UNLIT, _) => Ok(Lump::VertexUnlit(render::parse_vertex_unlit(data, v)?)),
        (ids::VERTEX_LIT_FLAT, _) => Ok(Lump::VertexLitFlat(render::parse_vertex_lit_flat(data, v)?)),
        (ids::VERTEX_LIT_BUMP, DediV47 | ClientV51) => Ok(Lump::VertexLitBump(render::parse_vertex_lit_bump(data, v)?)),
        (ids::VERTEX_LIT_BUMP, Tf2V37) => Ok(Lump::Tf2VertexLitBump(tf2::parse_vertex_lit_bump(data, v)?)),
        (ids::VERTEX_UNLIT_TS, DediV47 | ClientV51) => Ok(Lump::VertexUnlitTs(render::parse_vertex_unlit_ts(data, v)?)),
        (ids::VERTEX_UNLIT_TS, Tf2V37) => Ok(Lump::Tf2VertexUnlitTs(tf2::parse_vertex_unlit_ts(data, v)?)),
        (ids::MESH_INDICES, _) => Ok(Lump::MeshIndex(render::parse_mesh_indices(data, v)?)),
        (ids::MESHES, _) => Ok(Lump::Mesh(render::parse_meshes(data, v)?)),
        (ids::MESH_BOUNDS, _) => Ok(Lump::MeshBounds(render::parse_mesh_bounds(data, v)?)),
        (ids::MATERIAL_SORT, _) => Ok(Lump::MaterialSort(render::parse_material_sorts(data, v)?)),
        (ids::LIGHTMAP_HEADERS, _) => Ok(Lump::LightmapHeader(lighting::parse_lightmap_headers(data, v)?)),
        (ids::TWEAK_LIGHTS, DediV47 | ClientV51) => Ok(Lump::TweakLight(lighting::parse_tweak_lights(data, v)?)),
        (ids::CM_GRID, Tf2V37) => Ok(Lump::CmGrid(tf2::parse_cm_grid(data, v)?)),
        (ids::CM_GRID_CELLS, Tf2V37) => Ok(Lump::CmGridCell(tf2::parse_cm_grid_cells(data, v)?)),
        (ids::CM_GEO_SETS, Tf2V37) => Ok(Lump::CmGeoSet(tf2::parse_cm_geo_sets(data, v)?)),
        (ids::CM_GEO_SET_BOUNDS, Tf2V37) => Ok(Lump::CmBounds(tf2::parse_cm_geo_set_bounds(data, v)?)),
        (ids::CM_PRIMITIVES, Tf2V37) => Ok(Lump::CmPrimitive(tf2::parse_cm_primitives(data, v)?)),
        (ids::CM_PRIMITIVE_BOUNDS, Tf2V37) => Ok(Lump::CmBounds(tf2::parse_cm_primitive_bounds(data, v)?)),
        (ids::CM_UNIQUE_CONTENTS, Tf2V37) => Ok(Lump::CmUniqueContents(tf2::parse_cm_unique_contents(data, v)?)),
        (ids::CM_BRUSHES, Tf2V37) => Ok(Lump::CmBrush(tf2::parse_cm_brushes(data, v)?)),
        (ids::CM_BRUSH_SIDE_PLANE_OFFSETS, Tf2V37) => Ok(Lump::CmSidePlaneOffset(tf2::parse_cm_side_plane_offsets(data, v)?)),
        (ids::CM_BRUSH_SIDE_PROPERTIES, Tf2V37) => Ok(Lump::CmSideProp(tf2::parse_cm_side_props(data, v)?)),
        (ids::CM_BRUSH_SIDE_TEXTURE_VECTORS, Tf2V37) => Ok(Lump::CmSideTexVec(tf2::parse_cm_side_tex_vecs(data, v)?)),
        (ids::LIGHTMAP_DATA_SKY_COMPRESSED, DediV47 | ClientV51) => Ok(Lump::LightmapPages(lighting::parse_lightmap_bulk(data, v)?)),
        (ids::LIGHTMAP_DATA_SKY, _) => Ok(Lump::LightmapPages(lighting::parse_lightmap_bulk(data, v)?)),
        (ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS, _) => Ok(Lump::LightmapPages(lighting::parse_lightmap_bulk(data, v)?)),
        (ids::CSM_AABB_NODES, _) => Ok(Lump::CsmAabbNode(csm::parse_nodes(data, v)?)),
        (ids::CSM_OBJ_REFERENCES, DediV47 | ClientV51) => Ok(Lump::CsmObjRef(csm::parse_obj_refs(data, v)?)),
        (ids::CSM_OBJ_REFERENCES, Tf2V37) => Ok(Lump::CsmObjRef16(csm::parse_obj_refs16(data, v)?)),
        // The compact 44-byte probe and the 48-byte padded one are told apart
        // by the lump size, with the target version breaking a tie.
        (ids::LIGHTPROBES, ClientV51) => match lighting::parse_lightprobes_v51(data, v) {
            Ok(p) => Ok(Lump::LightProbe(p)),
            Err(_) => Ok(Lump::LightProbe48(lighting::parse_lightprobes48(data, v)?)),
        },
        (ids::LIGHTPROBES, DediV47 | Tf2V37) => match lighting::parse_lightprobes48(data, v) {
            Ok(p) => Ok(Lump::LightProbe48(p)),
            Err(_) => Ok(Lump::LightProbe(lighting::parse_lightprobes_v51(data, v)?)),
        },
        (ids::STATIC_PROP_LIGHTPROBE_INDICES, _) => Ok(Lump::PropLightprobeIndex(lighting::parse_prop_lightprobe_indices(data, v)?)),
        (ids::LIGHTPROBE_TREE, _) => Ok(Lump::LightProbeTreeNode(lighting::parse_lightprobe_tree(data, v)?)),
        (ids::LIGHTPROBE_REFERENCES, _) => Ok(Lump::LightProbeRef(lighting::parse_lightprobe_refs(data, v)?)),
        (ids::CELL_BSP_NODES, _) => Ok(Lump::CellBspNode(vis::parse_cell_bsp_nodes(data, v)?)),
        (ids::CELLS, _) => Ok(Lump::Cell(vis::parse_cells(data, v)?)),
        (ids::PORTALS, _) => Ok(Lump::Portal(vis::parse_portals(data, v)?)),
        (ids::PORTAL_VERTICES, _) => Ok(Lump::Vec3f(prims::parse(data, v)?)),
        (ids::PORTAL_EDGES, _) => Ok(Lump::PortalEdge(vis::parse_portal_edges(data, v)?)),
        (ids::PORTAL_VERTEX_EDGES, _) => Ok(Lump::PortalVertEdges(vis::parse_portal_vert_edges(data, v)?)),
        (ids::PORTAL_VERTEX_REFERENCES, _) => Ok(Lump::PortalRef(vis::parse_portal_vert_refs(data, v)?)),
        (ids::PORTAL_EDGE_REFERENCES, _) => Ok(Lump::PortalRef(vis::parse_portal_edge_refs(data, v)?)),
        (ids::PORTAL_EDGE_INTERSECT_EDGE, _) => Ok(Lump::PortalIsectList(vis::parse_portal_isect_edges(data, v)?)),
        (ids::PORTAL_EDGE_INTERSECT_AT_VERTEX, _) => Ok(Lump::PortalIsectList(vis::parse_portal_isect_verts(data, v)?)),
        (ids::PORTAL_EDGE_INTERSECT_HEADER, _) => Ok(Lump::PortalIsectHeader(vis::parse_portal_isect_headers(data, v)?)),
        (ids::OCCLUSION_MESH_VERTICES, _) => Ok(Lump::Vec3f(prims::parse(data, v)?)),
        (ids::OCCLUSION_MESH_INDICES, _) => Ok(Lump::OcclusionIndex(vis::parse_occlusion_indices(data, v)?)),
        (ids::CELL_AABB_NODES, _) => Ok(Lump::AabbNode(vis::parse_aabb_nodes(data, v)?)),
        (ids::OBJ_REFERENCES, DediV47 | ClientV51) => Ok(Lump::ObjRef(vis::parse_obj_refs(data, v)?)),
        (ids::OBJ_REFERENCES, Tf2V37) => Ok(Lump::ObjRef16(vis::parse_obj_refs16(data, v)?)),
        (ids::OBJ_REFERENCE_BOUNDS, _) => Ok(Lump::ObjRefBounds(vis::parse_obj_ref_bounds(data, v)?)),
        (ids::LIGHTMAP_DATA_RTL_PAGE, _) => Ok(Lump::RtlIndex(lighting::parse_rtl_index(data, v)?)),
        (ids::LEVEL_INFO, DediV47 | ClientV51) => Ok(Lump::LevelInfo(level::parse_level_info(data, v)?)),
        (ids::LEVEL_INFO, Tf2V37) => Ok(Lump::Tf2LevelInfo(tf2::parse_level_info(data, v)?)),
        (ids::SHADOW_MESH_OPAQUE_VERTICES, _) => Ok(Lump::Vec3f(prims::parse(data, v)?)),
        (ids::SHADOW_MESH_ALPHA_VERTICES, _) => Ok(Lump::ShadowMeshAlphaVert(csm::parse_alpha_verts(data, v)?)),
        (ids::SHADOW_MESH_INDICES, _) => Ok(Lump::ShadowMeshIndex(csm::parse_indices(data, v)?)),
        (ids::SHADOW_MESHES, _) => Ok(Lump::ShadowMesh(csm::parse_meshes(data, v)?)),
        _ => Ok(Lump::Opaque(data.to_vec())),
    }
}
/// Re-encodes a decoded lump. The bytes come back exactly as they were parsed
/// for every shipped map.
pub fn write(lump: &Lump, version: Version) -> Vec<u8> {
    match lump {
        Lump::EntityText(items) => entities::write(items, version),
        Lump::EntityPartitions(items) => level::write_entity_partitions(items, version),
        Lump::LevelInfo(items) => level::write_level_info(items, version),
        Lump::Tf2LevelInfo(items) => tf2::write_level_info(items, version),
        Lump::GameLump(items) => game_lump::write(items, version),
        Lump::Plane(items) => render::write_planes(items, version),
        Lump::TexData(items) => render::write_texdata(items, version),
        Lump::Tf2TexData(items) => tf2::write_texdata(items, version),
        Lump::SurfaceNames(items) => render::write_surface_names(items, version),
        Lump::StringTable(items) => strings::write(items, version),
        Lump::U32Table(items) => strings::write_offsets(items, version),
        Lump::Vec3f(items) => prims::write(items, version),
        Lump::Model(items) => collision::write_models(items, version),
        Lump::Tf2Model(items) => tf2::write_models(items, version),
        Lump::ContentMask(items) => collision::write_content_masks(items, version),
        Lump::SurfaceProp(items) => collision::write_surface_props(items, version),
        Lump::BvhNode(items) => collision::write_bvh_nodes(items, version),
        Lump::BvhLeaf(items) => collision::write_bvh_leaf(items, version),
        Lump::PackVert(items) => collision::write_pack_verts(items, version),
        Lump::CellAabbTotals(items) => vis::write_cell_aabb_totals(items, version),
        Lump::CsmAabbTotals(items) => csm::write_totals(items, version),
        Lump::FadeDist(items) => vis::write_fade_dists(items, version),
        Lump::Cubemap(items) => lighting::write_cubemaps(items, version),
        Lump::CubemapAmbient(items) => lighting::write_cubemap_ambient(items, version),
        Lump::LightmapPages(items) => lighting::write_lightmap_bulk(items, version),
        Lump::WorldLight(items) => lighting::write_world_lights(items, version),
        Lump::TweakLight(items) => lighting::write_tweak_lights(items, version),
        Lump::LightmapHeader(items) => lighting::write_lightmap_headers(items, version),
        Lump::LightProbe(items) => lighting::write_lightprobes_v51(items, version),
        Lump::LightProbe48(items) => lighting::write_lightprobes48(items, version),
        Lump::PropLightprobeIndex(items) => lighting::write_prop_lightprobe_indices(items, version),
        Lump::LightProbeTreeNode(items) => lighting::write_lightprobe_tree(items, version),
        Lump::LightProbeRef(items) => lighting::write_lightprobe_refs(items, version),
        Lump::ShadowEnvironment(items) => lighting::write_shadow_envs(items, version),
        Lump::LightProbeParentInfo(items) => lighting::write_parent_infos(items, version),
        Lump::RtlIndex(items) => lighting::write_rtl_index(items, version),
        Lump::CellBspNode(items) => vis::write_cell_bsp_nodes(items, version),
        Lump::Cell(items) => vis::write_cells(items, version),
        Lump::Portal(items) => vis::write_portals(items, version),
        Lump::PortalEdge(items) => vis::write_portal_edges(items, version),
        Lump::PortalVertEdges(items) => vis::write_portal_vert_edges(items, version),
        Lump::PortalRef(items) => vis::write_portal_edge_refs(items, version),
        Lump::PortalIsectList(items) => vis::write_portal_isect_verts(items, version),
        Lump::PortalIsectHeader(items) => vis::write_portal_isect_headers(items, version),
        Lump::OcclusionIndex(items) => vis::write_occlusion_indices(items, version),
        Lump::AabbNode(items) => vis::write_aabb_nodes(items, version),
        Lump::ObjRef(items) => vis::write_obj_refs(items, version),
        Lump::ObjRef16(items) => vis::write_obj_refs16(items, version),
        Lump::ObjRefBounds(items) => vis::write_obj_ref_bounds(items, version),
        Lump::ShadowMeshAlphaVert(items) => csm::write_alpha_verts(items, version),
        Lump::ShadowMeshIndex(items) => csm::write_indices(items, version),
        Lump::ShadowMesh(items) => csm::write_meshes(items, version),
        Lump::CsmAabbNode(items) => csm::write_nodes(items, version),
        Lump::CsmObjRef(items) => csm::write_obj_refs(items, version),
        Lump::CsmObjRef16(items) => csm::write_obj_refs16(items, version),
        Lump::Mesh(items) => render::write_meshes(items, version),
        Lump::MeshBounds(items) => render::write_mesh_bounds(items, version),
        Lump::MaterialSort(items) => render::write_material_sorts(items, version),
        Lump::MeshIndex(items) => render::write_mesh_indices(items, version),
        Lump::VertexUnlit(items) => render::write_vertex_unlit(items, version),
        Lump::VertexLitFlat(items) => render::write_vertex_lit_flat(items, version),
        Lump::VertexLitBump(items) => render::write_vertex_lit_bump(items, version),
        Lump::VertexUnlitTs(items) => render::write_vertex_unlit_ts(items, version),
        Lump::Tf2VertexLitBump(items) => tf2::write_vertex_lit_bump(items, version),
        Lump::Tf2VertexUnlitTs(items) => tf2::write_vertex_unlit_ts(items, version),
        Lump::CmGrid(items) => tf2::write_cm_grid(items, version),
        Lump::CmGridCell(items) => tf2::write_cm_grid_cells(items, version),
        Lump::CmGeoSet(items) => tf2::write_cm_geo_sets(items, version),
        Lump::CmBounds(items) => tf2::write_cm_geo_set_bounds(items, version),
        Lump::CmPrimitive(items) => tf2::write_cm_primitives(items, version),
        Lump::CmUniqueContents(items) => tf2::write_cm_unique_contents(items, version),
        Lump::CmBrush(items) => tf2::write_cm_brushes(items, version),
        Lump::CmSidePlaneOffset(items) => tf2::write_cm_side_plane_offsets(items, version),
        Lump::CmSideProp(items) => tf2::write_cm_side_props(items, version),
        Lump::CmSideTexVec(items) => tf2::write_cm_side_tex_vecs(items, version),
        Lump::TricollTriangle(items) => tf2::write_tricoll_triangles(items, version),
        Lump::TricollHeader(items) => tf2::write_tricoll_headers(items, version),
        Lump::Opaque(raw) => raw.clone(),
    }
}

/// Parses a lump whose directory pointers are lump-relative, which is what a
/// map this crate wrote holds. A map read from disk needs [`parse_at`] with the
/// header's offset for the game lump.
pub fn parse(id: u16, version: Version, data: &[u8]) -> Result<Lump> {
    parse_at(id, version, data, 0)
}

/// Parses and re-encodes a lump, given the offset the header records for it.
pub fn roundtrip(id: u16, version: Version, data: &[u8], base: u32) -> Result<Vec<u8>> {
    let lump = parse_at(id, version, data, base)?;
    Ok(write(&lump, version))
}
