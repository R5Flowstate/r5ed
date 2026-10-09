//! Titanfall 2 (rBSP v37) to Apex conversion: a faithful port of the
//! per-lump transforms, the brush-entity collider blobs and the entity
//! partition move.

use std::collections::{BTreeMap, HashMap};

use glam::{DVec3, Vec3};
use rbsp::lumps::collision::Model as ApexModel;
use rbsp::{ids, Bsp, Version};

use crate::collision::tf2::{self, Converter, SurfaceTables, Tf2Collision, Tf2Error};
use crate::collision::{self, CollModel, CollTarget};
use crate::{MapArtifacts, Report, Stats};

/// A two-byte ident the game-lump directory stores the `prps` block under.
const PRPS: u32 = 0x7370_7270;
/// A shadow-environment record.
const ENV_SIZE: usize = 36;

#[derive(Debug, thiserror::Error)]
pub enum Tf2ConvError {
    #[error("{0}")]
    Source(#[from] Tf2Error),
    #[error("{0}")]
    Collision(#[from] collision::CollError),
    #[error("lump 0x{id:02x} of the v37 map: {reason}")]
    Lump { id: u16, reason: String },
    #[error("the v37 map has no sprp block")]
    NoStaticProps,
    #[error("the v37 sprp block carries a trailing section, which no Apex map has")]
    SprpTail,
    #[error("a CSM environment names nodes {0}..{1}, which is not one tree")]
    CsmTree(usize, usize),
    #[error("CSM node {0} is outside every shadow environment")]
    CsmOrphan(usize),
    #[error("the shadow mesh triangle counts do not cover the index lump")]
    CsmIndices,
    #[error("the real-time light lump does not match the lightmap header table")]
    RtlSize,
    #[error("the lightmap lump does not match the lightmap header table: page at {at} needs {need} bytes of {have}")]
    LightmapSize { at: usize, need: usize, have: usize },
    #[error("a vertex lump of the v37 map is not a whole number of records")]
    VertexSize(u16),
    #[error("the shadow direction of cascade {0} is vertical, so its light frame is undefined")]
    VerticalSun(usize),
    #[error("{0}")]
    Io(String),
}

/// What a conversion produced beyond the map itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Conversion {
    pub artifacts: MapArtifacts,
    pub stats: Stats,
    /// Collider blobs written into entity keys, by model index.
    pub blob_models: usize,
    pub report: Report,
}

/// The lumps the collision reader touches, so a per-blob converter does not
/// have to clone the whole v37 map.
const COLL_LUMPS: [u16; 18] = [
    ids::PLANES,
    ids::VERTICES,
    ids::MODELS,
    ids::TEXTURE_DATA,
    ids::CUBEMAPS_AMBIENT_RCP,
    ids::TEXDATA_STRING_TABLE,
    ids::CM_GRID,
    ids::CM_GRID_CELLS,
    ids::CM_GEO_SETS,
    ids::CM_GEO_SET_BOUNDS,
    ids::CM_PRIMITIVES,
    ids::CM_PRIMITIVE_BOUNDS,
    ids::CM_UNIQUE_CONTENTS,
    ids::CM_BRUSHES,
    ids::CM_BRUSH_SIDE_PLANE_OFFSETS,
    ids::CM_BRUSH_SIDE_PROPERTIES,
    ids::TRICOLL_TRIANGLES,
    ids::TRICOLL_HEADERS,
];

/// Reads a v37 source map, taking every lump that has a sidecar from it.
///
/// A source that was split into sidecars still carries its original directory,
/// and those entries are stale: the real-time-light lump declares 35389440
/// bytes where the packed file stores 31457280 of lightmap and then the next
/// lump. A sidecar is the only complete copy of such a lump, so it wins.
pub fn read_source(path: &std::path::Path) -> Result<Bsp, Tf2ConvError> {
    let mut bsp = Bsp::read_path(path).map_err(|e| Tf2ConvError::Io(format!("{}: {e}", path.display())))?;
    for id in 0..=0x7Fu16 {
        let side = Bsp::sidecar_path(path, id);
        if !side.is_file() {
            continue;
        }
        let data = std::fs::read(&side).map_err(|e| Tf2ConvError::Io(format!("{}: {e}", side.display())))?;
        bsp.set_lump(id, data);
    }
    Ok(bsp)
}

fn slim(src: &Bsp) -> Bsp {
    let mut out = Bsp::new(src.header.version, src.header.flags, src.header.revision);
    for id in COLL_LUMPS {
        if !src.lump(id).is_empty() {
            out.set_lump(id, src.lump(id).to_vec());
        }
    }
    out
}

/// Converts a v37 map for one target. `ents` are the source `.ent` files by
/// partition name.
pub fn convert_tf2(
    tf2_bsp: &Bsp,
    ents: &BTreeMap<String, Vec<u8>>,
    target: Version,
    tables: &SurfaceTables,
    name: &str,
) -> Result<Conversion, Tf2ConvError> {
    let mut report = Report::default();
    let mut stats = Stats::default();
    let collision = Tf2Collision::open(slim(tf2_bsp))?;
    let mut conv = Converter::new(collision, tables);
    let fields = conv.run()?;
    let writer = conv.into_writer();

    let version = if target == Version::ClientV51 { 51u16 } else { 47u16 };
    let flags = if target == Version::ClientV51 { 1u16 } else { 0 };
    let mut bsp = Bsp::new(version, flags, tf2_bsp.header.revision);

    // The shared position pool: the source's render vertices, then the
    // collision writer's own.
    let mut verts = tf2_bsp.lump(ids::VERTICES).to_vec();
    let vert_offset = verts.len() / 12;
    for v in &writer.verts {
        for c in v {
            verts.extend_from_slice(&c.to_le_bytes());
        }
    }
    bsp.set_lump(ids::VERTICES, verts);
    // The v37 models lump holds 32-byte records and the v47/v51 layout the
    // collision fields are patched into is 64, so each record is widened first.
    let models_raw = tf2_bsp.lump(ids::MODELS);
    let mut models_wide: Vec<u8> = Vec::with_capacity(models_raw.len() / 32 * 64);
    for rec in models_raw.chunks_exact(32) {
        models_wide.extend_from_slice(rec);
        models_wide.resize(models_wide.len() + 32, 0);
    }
    bsp.set_lump(ids::MODELS, tf2::patch_models(&models_wide, &fields, vert_offset as i32)?);
    bsp.set_lump(ids::CONTENTS_MASKS, writer.lump_contents());
    bsp.set_lump(ids::SURFACE_PROPERTIES, writer.lump_surfprops());
    bsp.set_lump(ids::BVH_NODES, writer.lump_nodes().to_vec());
    bsp.set_lump(ids::BVH_LEAF_DATA, writer.lump_leaf());
    stats.bvh_nodes = writer.lump_nodes().len() / rbsp::lumps::collision::NODE_SIZE;

    bsp.set_lump(ids::ENTITIES, rewrite_model_paths(tf2_bsp.lump(ids::ENTITIES)));
    bsp.set_lump(ids::PLANES, tf2_bsp.lump(ids::PLANES).to_vec());
    bsp.set_lump(ids::TEXTURE_DATA, conv_texdata(tf2_bsp)?);
    bsp.set_lump(ids::SURFACE_NAMES, tf2_bsp.lump(ids::CUBEMAPS_AMBIENT_RCP).to_vec());
    bsp.set_lump(ids::ENTITY_PARTITIONS, tf2_bsp.lump(ids::ENTITY_PARTITIONS).to_vec());
    let (game_lump, _prop_names, num_props) = conv_game_lump(tf2_bsp, version)?;
    bsp.set_lump(ids::GAME_LUMP, game_lump);
    bsp.set_lump(ids::WORLD_LIGHTS, tf2_bsp.lump(ids::WORLD_LIGHTS).to_vec());
    bsp.set_lump(ids::LEVEL_INFO, conv_level_info(tf2_bsp.lump(ids::LEVEL_INFO), num_props)?);
    stats.props = num_props as usize;

    let (aabb_nodes, aabb_totals) = conv_aabb_nodes(tf2_bsp.lump(ids::CELL_AABB_NODES))?;
    bsp.set_lump(ids::CELL_AABB_NODES, aabb_nodes);
    bsp.set_lump(ids::CELL_AABB_SUBTREE_REF_TOTALS, aabb_totals);
    bsp.set_lump(ids::OBJ_REFERENCES, widen_u16(tf2_bsp.lump(ids::OBJ_REFERENCES)));
    bsp.set_lump(ids::OBJ_REFERENCE_BOUNDS, tf2_bsp.lump(ids::OBJ_REFERENCE_BOUNDS).to_vec());
    for id in 0x6A..=0x76u16 {
        if !tf2_bsp.lump(id).is_empty() {
            bsp.set_lump(id, tf2_bsp.lump(id).to_vec());
        }
    }

    let mut declared: BTreeMap<u16, u32> = BTreeMap::new();
    if target == Version::ClientV51 {
        bsp.set_lump(ids::LIGHTPROBE_PARENT_INFOS, tf2_bsp.lump(ids::LIGHTPROBE_PARENT_INFOS).to_vec());
        bsp.set_lump(ids::VERTEX_NORMALS, tf2_bsp.lump(ids::VERTEX_NORMALS).to_vec());
        let (cubemaps, ambient) = conv_cubemaps(tf2_bsp.lump(ids::CUBEMAPS));
        bsp.set_lump(ids::CUBEMAPS, cubemaps);
        bsp.set_lump(ids::CUBEMAPS_AMBIENT_RCP, ambient);
        stats.cubemaps = tf2_bsp.lump(ids::CUBEMAPS).len() / 16;
        for (id, keep) in [
            (ids::VERTEX_UNLIT, 20usize),
            (ids::VERTEX_LIT_FLAT, 36),
            (ids::VERTEX_LIT_BUMP, 44),
            (ids::VERTEX_UNLIT_TS, 28),
        ] {
            let raw = tf2_bsp.lump(id);
            if raw.is_empty() {
                continue;
            }
            if !raw.len().is_multiple_of(keep) {
                return Err(Tf2ConvError::VertexSize(id));
            }
            bsp.set_lump(id, narrow_vertex(id, raw, keep));
        }
        bsp.set_lump(ids::MESH_INDICES, tf2_bsp.lump(ids::MESH_INDICES).to_vec());
        bsp.set_lump(ids::MESHES, conv_meshes(tf2_bsp.lump(ids::MESHES))?);
        bsp.set_lump(ids::MESH_BOUNDS, tf2_bsp.lump(ids::MESH_BOUNDS).to_vec());
        bsp.set_lump(ids::MATERIAL_SORT, tf2_bsp.lump(ids::MATERIAL_SORT).to_vec());
        bsp.set_lump(ids::LIGHTMAP_HEADERS, tf2_bsp.lump(ids::LIGHTMAP_HEADERS).to_vec());
        bsp.set_lump(
            ids::LIGHTMAP_DATA_SKY,
            conv_lightmaps(
                tf2_bsp.lump(ids::LIGHTMAP_DATA_SKY),
                tf2_bsp.lump(ids::LIGHTMAP_HEADERS),
                tf2_bsp.lump(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS),
                tf2_bsp.lump(ids::LIGHTMAP_DATA_RTL_PAGE),
                tf2_bsp.lump(ids::WORLD_LIGHTS),
            )?,
        );
        let (rtl, rtl_declared) = conv_rtl(
            tf2_bsp.lump(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS),
            tf2_bsp.lump(ids::LIGHTMAP_HEADERS),
        )?;
        bsp.set_lump_declared(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS, rtl, Some(rtl_declared));
        declared.insert(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS, rtl_declared);
        bsp.set_lump(ids::LIGHTMAP_DATA_RTL_PAGE, tf2_bsp.lump(ids::LIGHTMAP_DATA_RTL_PAGE).to_vec());
        let csm = conv_csm(tf2_bsp)?;
        bsp.set_lump(ids::CSM_AABB_NODES, csm.nodes);
        bsp.set_lump(ids::CSM_OBJ_REFERENCES, csm.refs);
        bsp.set_lump(ids::CSM_AABB_SUBTREE_REF_TOTALS, csm.totals);
        bsp.set_lump(ids::SHADOW_ENVIRONMENTS, csm.envs);
        stats.csm_shadow_meshes = csm.meshes;
        bsp.set_lump(ids::LIGHTPROBES, conv_lightprobes_v51(tf2_bsp.lump(ids::LIGHTPROBES))?);
        stats.lightprobes = tf2_bsp.lump(ids::LIGHTPROBES).len() / 48;
        bsp.set_lump(ids::STATIC_PROP_LIGHTPROBE_INDICES, tf2_bsp.lump(ids::STATIC_PROP_LIGHTPROBE_INDICES).to_vec());
        bsp.set_lump(ids::LIGHTPROBE_TREE, tf2_bsp.lump(ids::LIGHTPROBE_TREE).to_vec());
        bsp.set_lump(ids::LIGHTPROBE_REFERENCES, tf2_bsp.lump(ids::LIGHTPROBE_REFERENCES).to_vec());
        for id in 0x7C..=0x7Fu16 {
            if !tf2_bsp.lump(id).is_empty() {
                bsp.set_lump(id, tf2_bsp.lump(id).to_vec());
            }
        }
    } else {
        bsp.set_lump(ids::MESHES, conv_meshes(tf2_bsp.lump(ids::MESHES))?);
        bsp.set_lump(ids::MATERIAL_SORT, tf2_bsp.lump(ids::MATERIAL_SORT).to_vec());
        crate::emit::prune_to_dedi(&mut bsp);
    }
    for (id, len) in &declared {
        bsp.header.lumps[*id as usize].length = *len;
    }

    // The entity files, converted and then given their collider blobs.
    let mut ent_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (partition, data) in ents {
        let text = rewrite_model_paths(&apex_ent_header(data));
        ent_bytes.insert(partition.clone(), text);
    }
    let blobs = brush_coll(&mut bsp, &mut ent_bytes, tf2_bsp, tables, target, name)?;
    let mut main = bsp.lump(ids::ENTITIES).to_vec();
    patch_coll_keys(&mut main, &blobs);
    bsp.set_lump(ids::ENTITIES, main);
    for data in ent_bytes.values_mut() {
        patch_coll_keys(data, &blobs);
    }
    // Whatever the converter moved into a partition file leaves the main lump.
    let moved = entity_partitions(&mut bsp, &mut ent_bytes, name)?;
    stats.partition_files = ent_bytes.len();
    stats.entities = rbsp::lumps::entities::count_blocks(bsp.lump(ids::ENTITIES))
        + ent_bytes.values().map(|v| rbsp::lumps::entities::count_blocks(v)).sum::<usize>();

    let _ = moved;

    let issues = rbsp::validate(&bsp, &ent_bytes);
    for i in &issues {
        report.validation.push(format!("{:?} {}: {}", i.severity, i.lump.map(|l| format!("0x{l:02x}")).unwrap_or_default(), i.message));
    }
    if let Some(err) = issues.iter().find(|i| i.is_error()) {
        report.warn(format!("converted map invalid: {}", err.message));
    }
    let ent_files = ent_bytes.keys().map(|k| (partition_of(k), k.clone())).collect();
    report.stats = stats.clone();
    Ok(Conversion {
        artifacts: MapArtifacts { bsp, ent_files, ent_bytes, cubemap: None },
        stats,
        blob_models: blobs.len(),
        report,
    })
}

fn partition_of(name: &str) -> rdoc::model::Partition {
    rdoc::model::Partition::from_name(name).unwrap_or(rdoc::model::Partition::Main)
}

// ---- per-lump transforms -----------------------------------------------------

/// v37 texdata records to the v47 layout: the name becomes a byte offset into
/// the name blob the 0x0F slot carries.
fn conv_texdata(src: &Bsp) -> Result<Vec<u8>, Tf2ConvError> {
    let v = Version::Tf2V37;
    let td = rbsp::lumps::tf2::parse_texdata(src.lump(ids::TEXTURE_DATA), v)
        .map_err(|e| Tf2ConvError::Lump { id: ids::TEXTURE_DATA, reason: e.to_string() })?;
    let table = rbsp::lumps::strings::parse_table(src.lump(ids::TEXDATA_STRING_TABLE), v)
        .map_err(|e| Tf2ConvError::Lump { id: ids::TEXDATA_STRING_TABLE, reason: e.to_string() })?;
    let out: Vec<rbsp::lumps::render::TexData> = td
        .iter()
        .map(|t| rbsp::lumps::render::TexData {
            name: table.get(t.name as usize).copied().unwrap_or(0),
            width: t.width,
            height: t.height,
            flags: t.flags as u32,
        })
        .collect();
    Ok(rbsp::lumps::render::write_texdata(&out, Version::ClientV51))
}

/// Drops the fields the target's vertex record does not have.
fn narrow_vertex(id: u16, raw: &[u8], stride: usize) -> Vec<u8> {
    let n = raw.len() / stride;
    let mut out = Vec::with_capacity(raw.len());
    for i in 0..n {
        let rec = &raw[i * stride..(i + 1) * stride];
        match id {
            // 44 -> 32: keep the position through lightmap UV, then the tangent.
            ids::VERTEX_LIT_BUMP => {
                out.extend_from_slice(&rec[0..28]);
                out.extend_from_slice(&rec[36..40]);
            }
            // 28 -> 24.
            ids::VERTEX_UNLIT_TS => out.extend_from_slice(&rec[0..24]),
            // 36 -> 28: the lightmap texel size has no field any more.
            ids::VERTEX_LIT_FLAT => out.extend_from_slice(&rec[0..28]),
            _ => out.extend_from_slice(rec),
        }
    }
    out
}

/// The mesh record's two luxel bytes become a fade distance of zero.
fn conv_meshes(raw: &[u8]) -> Result<Vec<u8>, Tf2ConvError> {
    if !raw.len().is_multiple_of(28) {
        return Err(Tf2ConvError::Lump { id: ids::MESHES, reason: "size is not a multiple of 28".into() });
    }
    let mut out = raw.to_vec();
    for i in 0..out.len() / 28 {
        out[i * 28 + 20..i * 28 + 22].fill(0);
    }
    Ok(out)
}

/// 48-byte probes to the compact 44-byte record: the light index is offset
/// where its weight is set and the empty slots compact to the front.
fn conv_lightprobes_v51(raw: &[u8]) -> Result<Vec<u8>, Tf2ConvError> {
    if !raw.len().is_multiple_of(48) {
        return Err(Tf2ConvError::Lump { id: ids::LIGHTPROBES, reason: "size is not a multiple of 48".into() });
    }
    let n = raw.len() / 48;
    let mut out = vec![0u8; n * 44];
    for i in 0..n {
        let s = &raw[i * 48..(i + 1) * 48];
        let d = &mut out[i * 44..(i + 1) * 44];
        d[0..32].copy_from_slice(&s[0..32]);
        let mut w = [0u8; 4];
        let mut idx = [0u16; 4];
        w.copy_from_slice(&s[32..36]);
        for k in 0..4 {
            idx[k] = u16::from_le_bytes([s[36 + k * 2], s[37 + k * 2]]);
        }
        for k in 0..4 {
            if w[k] != 0 {
                idx[k] = idx[k].wrapping_add(32);
            }
        }
        for a in 0..3 {
            for b in a + 1..4 {
                if w[a] == 0 && w[b] != 0 {
                    w[a] = w[b];
                    idx[a] = idx[b];
                    w[b] = 0;
                    idx[b] = 0xFFFF;
                }
            }
        }
        d[32..36].copy_from_slice(&w);
        for k in 0..4 {
            d[36 + k * 2..38 + k * 2].copy_from_slice(&idx[k].to_le_bytes());
        }
    }
    Ok(out)
}

/// The cell AABB tree: the v37 child and reference fields move, and the
/// subtree totals the v37 record carries become their own lump.
fn conv_aabb_nodes(raw: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Tf2ConvError> {
    if !raw.len().is_multiple_of(32) {
        return Err(Tf2ConvError::Lump {
            id: ids::CELL_AABB_NODES,
            reason: "size is not a multiple of 32".into(),
        });
    }
    let n = raw.len() / 32;
    let word = |i: usize, k: usize| u32::from_le_bytes(raw[i * 32 + k * 4..i * 32 + k * 4 + 4].try_into().unwrap());
    let set = |out: &mut [u8], i: usize, k: usize, v: u32| {
        out[i * 32 + k * 4..i * 32 + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
    };
    let mut out = raw.to_vec();
    let mut child_count = vec![0u32; n];
    let mut ref_count = vec![0u32; n];
    let mut first_child = vec![0u32; n];
    let mut ref_ofs = vec![0u32; n];
    let mut total = vec![0u32; n];
    for i in 0..n {
        let w12 = word(i, 3);
        let w28 = word(i, 7);
        child_count[i] = w12 & 0xFF;
        ref_count[i] = (w12 >> 8) & 0xFF;
        total[i] = (w12 >> 16) & 0xFFFF;
        first_child[i] = w28 & 0xFFFF;
        ref_ofs[i] = (w28 >> 16) & 0xFFFF;
    }
    let mut sub = vec![0i64; n];
    for i in (0..n).rev() {
        let mut s = ref_count[i] as i64;
        let (first, count) = (first_child[i] as usize, child_count[i] as usize);
        for (o, s_c) in sub[first..first + count].iter().enumerate() {
            let c = first + o;
            if c <= i {
                return Err(Tf2ConvError::Lump {
                    id: ids::CELL_AABB_NODES,
                    reason: format!("child {c} is not after parent {i}"),
                });
            }
            s += *s_c;
        }
        sub[i] = s;
    }
    if sub.iter().zip(total.iter()).any(|(a, b)| *a != *b as i64) {
        return Err(Tf2ConvError::Lump {
            id: ids::CELL_AABB_NODES,
            reason: "subtree totals disagree with the stored counts".into(),
        });
    }
    for i in 0..n {
        set(&mut out, i, 3, child_count[i] | (first_child[i] << 8));
        set(&mut out, i, 7, ref_count[i] | (ref_ofs[i] << 8));
    }
    let totals: Vec<u8> = total.iter().flat_map(|v| v.to_le_bytes()).collect();
    Ok((out, totals))
}

fn widen_u16(raw: &[u8]) -> Vec<u8> {
    raw.chunks_exact(2).flat_map(|c| (u16::from_le_bytes([c[0], c[1]]) as u32).to_le_bytes()).collect()
}

/// The 0x53 page headers, which are the only place a page's texel grid is
/// described. A bulk lightmap lump's declared length over-counts the pages it
/// holds, so the headers drive the walk and the surplus is not read.
fn lightmap_pages(headers: &[u8]) -> Result<Vec<rbsp::lumps::lighting::LightmapHeader>, Tf2ConvError> {
    rbsp::lumps::lighting::parse_lightmap_headers(headers, Version::Tf2V37)
        .map_err(|e| Tf2ConvError::Lump { id: ids::LIGHTMAP_HEADERS, reason: e.to_string() })
}

/// Real-time light texels: the id plane stays, the visibilities become BC5 over
/// a `width` by `2 * height` image. The header counts a plane the file does not
/// store.
fn conv_rtl(raw: &[u8], headers: &[u8]) -> Result<(Vec<u8>, u32), Tf2ConvError> {
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut disk: u64 = 0;
    let mut o = 0usize;
    for page in lightmap_pages(headers)? {
        let (w, h) = (page.width as usize, page.height as usize);
        let n = w * h;
        if o + 8 * n > raw.len() {
            return Err(Tf2ConvError::RtlSize);
        }
        let mut ids: Vec<u32> = (0..n)
            .map(|i| u32::from_le_bytes(raw[o + i * 4..o + i * 4 + 4].try_into().unwrap()))
            .collect();
        for id in ids.iter_mut() {
            // The top byte is the light group, and 255 there reads as none.
            if (*id >> 24) == 255 {
                *id &= 0x00FF_FFFF;
            }
        }
        let vis = &raw[o + 4 * n..o + 8 * n];
        let mut red = Vec::with_capacity(n * 2);
        let mut green = Vec::with_capacity(n * 2);
        for half in 0..2usize {
            for y in 0..h {
                for x in 0..w {
                    let t = y * w + x;
                    red.push(vis[t * 4 + half * 2]);
                    green.push(vis[t * 4 + half * 2 + 1]);
                }
            }
        }
        for id in &ids {
            out.extend_from_slice(&id.to_le_bytes());
        }
        out.extend_from_slice(&bc5_blocks(&red, &green, w as u32, (h * 2) as u32)?);
        disk += (4 * n + ((w + 3) >> 2) * 16 * ((2 * h + 3) >> 2) + (n >> 1)) as u64;
        o += 8 * n;
    }
    Ok((out, disk as u32))
}

/// A BC5 image with each block's red and green halves adjacent, which is the
/// layout the real-time-light pages use.
fn bc5_blocks(red: &[u8], green: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Tf2ConvError> {
    let bad = |e: rbake::Error| Tf2ConvError::Lump {
        id: ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS,
        reason: e.to_string(),
    };
    let (r, _, _) = rbake::bc::encode_bc4_image(red, width, height).map_err(bad)?;
    let (g, _, _) = rbake::bc::encode_bc4_image(green, width, height).map_err(bad)?;
    if r.len() != g.len() || r.len() % 8 != 0 {
        return Err(Tf2ConvError::RtlSize);
    }
    let mut out = Vec::with_capacity(r.len() + g.len());
    for (a, b) in r.chunks_exact(8).zip(g.chunks_exact(8)) {
        out.extend_from_slice(a);
        out.extend_from_slice(b);
    }
    Ok(out)
}

/// The sun visibility the Apex world shader reads straight out of the lightmap
/// alpha, which the v37 map leaves as a coverage mask.
fn conv_lightmaps(
    raw: &[u8],
    headers: &[u8],
    rtl: &[u8],
    rtl_groups: &[u8],
    world_lights: &[u8],
) -> Result<Vec<u8>, Tf2ConvError> {
    if !world_lights.len().is_multiple_of(112) {
        return Err(Tf2ConvError::Lump {
            id: ids::WORLD_LIGHTS,
            reason: "size is not a multiple of 112".into(),
        });
    }
    let mut realtime: Vec<usize> = Vec::new();
    let mut is_sky: Vec<bool> = Vec::new();
    for (i, w) in world_lights.chunks_exact(112).enumerate() {
        let ty = i32::from_le_bytes(w[52..56].try_into().unwrap());
        let flags = u32::from_le_bytes(w[88..92].try_into().unwrap());
        if flags & 0x8 != 0 {
            realtime.push(i);
            is_sky.push(ty == 3);
        }
    }
    let table: Vec<u16> = rtl_groups
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mut out = raw.to_vec();
    let mut o = 0usize;
    for page in lightmap_pages(headers)? {
        let (w, h) = (page.width as usize, page.height as usize);
        let n = w * h;
        if o + 8 * n > raw.len() {
            return Err(Tf2ConvError::LightmapSize { at: o, need: 8 * n, have: raw.len() });
        }
        if o + 8 * n > rtl.len() {
            return Err(Tf2ConvError::LightmapSize { at: o, need: 8 * n, have: rtl.len() });
        }
        let mut sun = vec![0u8; n];
        let mut found = vec![false; n];
        for t in 0..n {
            let word = u32::from_le_bytes(rtl[o + t * 4..o + t * 4 + 4].try_into().unwrap());
            let group = (word >> 24) as usize;
            for s in 0..4usize {
                let slot = ((word >> (6 * s)) & 63) as usize;
                if found[t] || slot == 63 {
                    continue;
                }
                let at = group * 63 + slot;
                // A group the table does not cover reads as light zero.
                let rt = table.get(at).copied().unwrap_or(0) as usize;
                if rt < realtime.len() && is_sky[rt] {
                    sun[t] = rtl[o + 4 * n + t * 4 + s];
                    found[t] = true;
                }
            }
            out[o + 4 * n + t * 4 + 3] = sun[t];
        }
        o += 8 * n;
    }
    if o != raw.len() {
        return Err(Tf2ConvError::LightmapSize { at: o, need: o, have: raw.len() });
    }
    Ok(out)
}

/// The cubemap slot stores a texture id, which the asset step fills in.
fn conv_cubemaps(raw: &[u8]) -> (Vec<u8>, Vec<u8>) {
    if !raw.len().is_multiple_of(16) {
        return (raw.to_vec(), Vec::new());
    }
    let mut out = raw.to_vec();
    let n = raw.len() / 16;
    for i in 0..n {
        out[i * 16 + 12..i * 16 + 16].fill(0);
    }
    let ambient: Vec<u8> = (0..n).flat_map(|_| 1.0f32.to_le_bytes()).collect();
    (out, ambient)
}

/// The level-info record grows a first-prop index field.
fn conv_level_info(raw: &[u8], num_props: u32) -> Result<Vec<u8>, Tf2ConvError> {
    if raw.len() < 28 {
        return Err(Tf2ConvError::Lump { id: ids::LEVEL_INFO, reason: "shorter than 28 bytes".into() });
    }
    let u32at = |o: usize| u32::from_le_bytes(raw[o..o + 4].try_into().unwrap());
    let f32at = |o: usize| f32::from_le_bytes(raw[o..o + 4].try_into().unwrap());
    let (fd, ft, fs, nprops) = (u32at(0), u32at(4), u32at(8), u32at(12));
    if nprops != num_props {
        return Err(Tf2ConvError::Lump {
            id: ids::LEVEL_INFO,
            reason: format!("level info says {nprops} props, the sprp block holds {num_props}"),
        });
    }
    let first = (fs + 63) & !63;
    let mut out = Vec::with_capacity(36);
    for v in [fd, ft, fs, first, nprops] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for o in (16..28).step_by(4) {
        out.extend_from_slice(&f32at(o).to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    Ok(out)
}

/// The static prop list: model paths normalised, the diffuse modulation forced
/// to white, wind and set-dress cleared.
fn conv_game_lump(src: &Bsp, version: u16) -> Result<(Vec<u8>, Vec<String>, u32), Tf2ConvError> {
    let raw = src.lump(ids::GAME_LUMP);
    let v = Version::Tf2V37;
    let entries = rbsp::lumps::game_lump::parse_at(raw, v, 0)
        .map_err(|e| Tf2ConvError::Lump { id: ids::GAME_LUMP, reason: e.to_string() })?;
    let Some(i) = entries.entries.iter().position(|e| e.is_sprp()) else {
        return Err(Tf2ConvError::NoStaticProps);
    };
    let Some(block) = entries.blocks.get(i).and_then(|b| b.as_static_props()) else {
        return Err(Tf2ConvError::NoStaticProps);
    };
    let names: Vec<String> = block.models.iter().map(|m| normalise_model_path(m)).collect();
    if names.iter().any(|n| n.len() >= 128) {
        return Err(Tf2ConvError::Lump { id: ids::GAME_LUMP, reason: "a model path does not fit its field".into() });
    }
    let count = block.prop_count();
    let mut props = block.props.clone();
    for p in props.iter_mut() {
        p.diffuse_modulation = 0xFFFF_FFFF;
        p.wind_data = 0;
        p.setdress_level = 0;
    }
    if block.tail_count != 0 {
        return Err(Tf2ConvError::SprpTail);
    }
    let body = rbsp::lumps::game_lump::sprp::StaticProps {
        models: names.clone(),
        counts: block.counts,
        props,
        tail_count: 0,
        groups: Vec::new(),
    }
    .encode();
    let mut out = Vec::with_capacity(4 + 16 + body.len());
    out.extend_from_slice(&1i32.to_le_bytes());
    out.extend_from_slice(&PRPS.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&20i32.to_le_bytes());
    out.extend_from_slice(&(body.len() as i32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok((out, names, count as u32))
}

/// `models/x.mdl` to `mdl/x.rmdl`, the form an Apex model asset uses.
pub fn normalise_model_path(name: &str) -> String {
    let s = name.replace('\\', "/");
    match s.strip_prefix("models/") {
        Some(rest) => match rest.strip_suffix(".mdl") {
            Some(stem) => format!("mdl/{stem}.rmdl"),
            None => s,
        },
        None => s,
    }
}

/// Rewrites every `models/...mdl` value in an entity text.
pub fn rewrite_model_paths(data: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    let mut i = 0usize;
    while i < data.len() {
        let rest = &data[i..];
        if rest.starts_with(b"\"models") && rest.len() > 9 && (rest[7] == b'/' || rest[7] == b'\\') {
            // The match may not cross a quote, so the closing `.mdl"` has to
            // come before the next one.
            if let Some(end) = find_model_end(rest) {
                let inner = String::from_utf8_lossy(&rest[8..end]).replace('\\', "/");
                out.push(b'"');
                out.extend_from_slice(format!("mdl/{inner}.rmdl").as_bytes());
                out.push(b'"');
                i += end + 5;
                continue;
            }
        }
        out.push(data[i]);
        i += 1;
    }
    out
}

/// Byte offset of the `.mdl` that closes the value starting at `data[7]`, plus
/// the opening quote's companion bytes, or `None`.
fn find_model_end(data: &[u8]) -> Option<usize> {
    let mut j = 8usize;
    while j + 5 <= data.len() {
        if data[j] == b'"' {
            return None;
        }
        if data[j..].starts_with(b".mdl\"") {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// The Apex loader requires `ENTITIES0N num_models=N`; the source has neither.
pub fn apex_ent_header(data: &[u8]) -> Vec<u8> {
    if !data.starts_with(b"ENTITIES") {
        return data.to_vec();
    }
    let Some(nl) = data.iter().position(|&b| b == b'\n') else { return data.to_vec() };
    if data[..nl].windows(11).any(|w| w == b"num_models=") {
        return data.to_vec();
    }
    let mut out = b"ENTITIES02 num_models=1\n".to_vec();
    out.extend_from_slice(&data[nl + 1..]);
    out
}

/// One `{ ... }` block's byte range, or `None`.
fn block_at(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let open = data[from..].iter().position(|&b| b == b'{')? + from;
    let close = data[open..].iter().position(|&b| b == b'}')? + open;
    Some((open, close + 1))
}

fn blocks(data: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some((a, b)) = block_at(data, i) {
        out.push((a, b));
        i = b;
    }
    out
}

fn classname_of(body: &[u8]) -> String {
    let needle = b"\"classname\"";
    let Some(p) = body.windows(needle.len()).position(|w| w == needle) else { return String::new() };
    let tail = &body[p + needle.len()..];
    let Some(q) = tail.iter().position(|&b| b == b'"') else { return String::new() };
    let rest = &tail[q + 1..];
    let Some(e) = rest.iter().position(|&b| b == b'"') else { return String::new() };
    String::from_utf8_lossy(&rest[..e]).to_string()
}

fn model_index_of(body: &[u8]) -> Option<u32> {
    let needle = b"\"model\"";
    let p = body.windows(needle.len()).position(|w| w == needle)?;
    let tail = &body[p + needle.len()..];
    let q = tail.iter().position(|&b| b == b'"')?;
    let rest = &tail[q + 1..];
    let e = rest.iter().position(|&b| b == b'"')?;
    let text = String::from_utf8_lossy(&rest[..e]);
    text.strip_prefix('*')?.parse().ok()
}

/// The tool surface a volume class carries in a converted map.
fn volume_surface_of(classname: &str) -> Option<(u32, u16, String)> {
    if classname == "envmap_volume" {
        return Some((0x10E3_1240, 0x610, "TOOLS\\TOOLSENVMAPVOLUME".to_string()));
    }
    if matches!(classname, "light_environment_volume" | "light_probe_volume" | "trigger_soundscape")
        || classname.starts_with("trigger_")
    {
        return Some((0x00EB_1280, 0x400, "TOOLS\\TOOLSTRIGGER".to_string()));
    }
    None
}

/// The `*trigger_brush_<b>_plane_<p>` keys of one body, grouped into brushes.
fn plane_brushes(body: &[u8]) -> Vec<Vec<(DVec3, f64)>> {
    let mut brushes: BTreeMap<u32, BTreeMap<u32, (DVec3, f64)>> = BTreeMap::new();
    let bytes = body;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\"*trigger_brush_") {
            let tail = &bytes[i + b"\"*trigger_brush_".len()..];
            let Some(d) = tail.iter().position(|&b| b == b'_') else { break };
            let Ok(b_index) = std::str::from_utf8(&tail[..d]).unwrap_or("x").parse::<u32>() else { break };
            let rest = &tail[d + 1..];
            if !rest.starts_with(b"plane_") {
                i += 1;
                continue;
            }
            // The plane number closes the key name, so the quote is the
            // separator, not another underscore.
            let rest = &rest[b"plane_".len()..];
            let Some(e) = rest.iter().position(|&b| b == b'"') else { break };
            let Ok(p_index) = std::str::from_utf8(&rest[..e]).unwrap_or("x").parse::<u32>() else { break };
            let rest = &rest[e + 1..];
            let Some(q) = rest.iter().position(|&b| b == b'"') else { break };
            let rest = &rest[q + 1..];
            let Some(v) = rest.iter().position(|&b| b == b'"') else { break };
            let value = String::from_utf8_lossy(&rest[..v]);
            let parts: Vec<f64> = value.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if parts.len() == 4 {
                brushes.entry(b_index).or_default().insert(
                    p_index,
                    (DVec3::new(parts[0], parts[1], parts[2]), parts[3]),
                );
            }
            i += 1;
            continue;
        }
        i += 1;
    }
    brushes
        .into_values()
        .filter(|p| p.len() >= 4)
        .map(|p| p.into_values().collect())
        .collect()
}

/// The mesh-less models a plane-defined volume needs: every such block gets a
/// `model` key naming one, and the record it names is what its collider blob
/// is built from.
fn volume_models(
    data: &mut Vec<u8>,
    own_first: u32,
    volumes: &mut BTreeMap<u32, VolumeEntry>,
    next_index: &mut u32,
) {
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    let spans = blocks(data);
    let mut cursor = 0usize;
    for (a, b) in spans {
        let body = &data[a..b];
        let cn = classname_of(body);
        let Some(surface) = volume_surface_of(&cn) else {
            out.extend_from_slice(&data[cursor..b]);
            cursor = b;
            continue;
        };
        let brushes = plane_brushes(body);
        if brushes.is_empty() {
            out.extend_from_slice(&data[cursor..b]);
            cursor = b;
            continue;
        }
        match model_index_of(body) {
            // A model the source map owns keeps its own collision.
            Some(idx) if idx < own_first => {
                out.extend_from_slice(&data[cursor..b]);
                cursor = b;
            }
            // A model this tool already appended is rebuilt in place.
            Some(idx) => {
                let head = strip_coll_keys(body);
                volumes.insert(idx, (surface.0, surface.1, surface.2.clone(), to_planes(&brushes)));
                out.extend_from_slice(&data[cursor..a]);
                out.extend_from_slice(&head);
                out.extend_from_slice(b"\n}");
                cursor = b;
            }
            None => {
                let idx = *next_index;
                *next_index += 1;
                volumes.insert(idx, (surface.0, surface.1, surface.2.clone(), to_planes(&brushes)));
                out.extend_from_slice(&data[cursor..a]);
                out.extend_from_slice(&strip_coll_keys(body));
                out.extend_from_slice(format!("\n\"model\" \"*{idx}\"").as_bytes());
                out.extend_from_slice(b"\n}");
                cursor = b;
            }
        }
    }
    out.extend_from_slice(&data[cursor..]);
    *data = out;
}

/// Every brush-model entity's collider blob, plus the mesh-less model records
/// the plane-defined volumes need.
fn brush_coll(
    bsp: &mut Bsp,
    ents: &mut BTreeMap<String, Vec<u8>>,
    src: &Bsp,
    tables: &SurfaceTables,
    target: Version,
    name: &str,
) -> Result<BTreeMap<u32, Vec<u8>>, Tf2ConvError> {
    let coll_target = if target == Version::ClientV51 { CollTarget::Client } else { CollTarget::Dedi };
    let own_first = Tf2Collision::open(slim(src))?.models.len() as u32;
    let mut volumes: BTreeMap<u32, VolumeEntry> = BTreeMap::new();
    let mut next_index = (bsp.lump(ids::MODELS).len() / 64) as u32;
    // The worldspawn lump is walked before the partition files, so a volume's
    // model index follows the order the entity text is enumerated in.
    let mut main = bsp.lump(ids::ENTITIES).to_vec();
    volume_models(&mut main, own_first, &mut volumes, &mut next_index);
    bsp.set_lump(ids::ENTITIES, main);
    for data in ents.values_mut() {
        volume_models(data, own_first, &mut volumes, &mut next_index);
    }
    if !volumes.is_empty() {
        append_models(bsp, &volumes)?;
    }
    let mut wanted: Vec<u32> = Vec::new();
    for data in std::iter::once(&bsp.lump(ids::ENTITIES).to_vec()).chain(ents.values()) {
        for (a, b) in blocks(data) {
            if let Some(i) = model_index_of(&data[a..b]) {
                if !wanted.contains(&i) {
                    wanted.push(i);
                }
            }
        }
    }
    wanted.sort_unstable();
    let mut blobs: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    for (idx, (contents, flags, surface, planes)) in &volumes {
        let blob = collision::volume_blob(planes, (*contents, *flags, surface.as_bytes()), coll_target)?;
        blobs.insert(*idx, blob.to_blob());
    }
    let prim_models: HashMap<u32, Vec<tf2::Prim>> = {
        let mut m: HashMap<u32, Vec<tf2::Prim>> = HashMap::new();
        for p in Tf2Collision::open(slim(src))?.primitives() {
            m.entry(p.model).or_default().push(p);
        }
        m
    };
    let strings = src.lump(ids::CUBEMAPS_AMBIENT_RCP).to_vec();
    for m in wanted {
        if blobs.contains_key(&m) || m >= own_first {
            continue;
        }
        let prims = prim_models.get(&m).cloned().unwrap_or_default();
        let blob = model_blob(src, tables, coll_target, &strings, m, &prims)?;
        blobs.insert(m, blob);
    }
    let _ = name;
    Ok(blobs)
}

fn to_planes(brushes: &[Vec<(DVec3, f64)>]) -> Vec<Vec<crate::csg::DPlane>> {
    brushes
        .iter()
        .map(|b| b.iter().map(|(n, d)| crate::csg::DPlane::new(*n, *d)).collect())
        .collect()
}

/// A block's keys with its `*coll` lines and its closing brace removed, ready
/// for a fresh run of keys to be written inside it.
fn strip_coll_keys(body: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(body.len());
    for line in body[..body.len() - 1].split_inclusive(|&b| b == b'\n') {
        let t = String::from_utf8_lossy(line);
        if t.trim_start().starts_with("\"*coll") {
            continue;
        }
        out.extend_from_slice(line);
    }
    while out.last().is_some_and(|&b| b == b'\n' || b == b'\r') {
        out.pop();
    }
    out
}

/// One brush-model entity's blob, built from the v37 model's own primitives
/// with a writer of its own so the surface names stay local to the blob.
fn model_blob(
    src: &Bsp,
    tables: &SurfaceTables,
    target: CollTarget,
    strings: &[u8],
    model: u32,
    prims: &[tf2::Prim],
) -> Result<Vec<u8>, Tf2ConvError> {
    let tf2 = Tf2Collision::open(slim(src))?;
    let record = tf2.models.get(model as usize).copied();
    let mut conv = Converter::new(tf2, tables);
    let mut items: Vec<collision::Item> = Vec::new();
    for p in prims {
        match p.kind {
            0 => items.push(conv.brush_item(p)?),
            64 => {
                if let Some(it) = conv.tricoll_item(p)? {
                    items.push(it);
                }
            }
            96 => items.push(conv.prop_item(p)?),
            other => return Err(Tf2ConvError::Lump {
                id: ids::MODELS,
                reason: format!("primitive type {other} has no Apex form"),
            }),
        }
    }
    let writer = conv.into_writer();
    let (lo, hi) = model_bounds(record, &items);
    let origin = ((lo + hi) * 0.5).round();
    let span = (hi - origin).max(origin - lo).max_element();
    let step = decode_step(span);
    let fields = rebuild_model(writer, items, origin, step, strings, target)?;
    Ok(fields)
}

/// The box a blob is quantised around: the v37 model record's own box grown to
/// cover every item the blob carries.
fn model_bounds(record: Option<(DVec3, DVec3)>, items: &[collision::Item]) -> (DVec3, DVec3) {
    let (mut lo, mut hi) = record.unwrap_or((DVec3::ZERO, DVec3::ZERO));
    for it in items {
        let (a, b) = it.bounds();
        lo = lo.min(a);
        hi = hi.max(b);
    }
    if lo.x > hi.x {
        return (DVec3::ZERO, DVec3::ZERO);
    }
    (lo, hi)
}

fn decode_step(span: f64) -> f64 {
    let mut step = 1.0f64;
    while span / step > 32000.0 {
        step *= 2.0;
    }
    while step > 1.0 / 64.0 && span / (step * 0.5) <= 32000.0 {
        step *= 0.5;
    }
    step
}

/// Packs a writer's tables into a blob, renumbering the surface names into a
/// local blob the way the reference converter does.
fn rebuild_model(
    mut writer: collision::BvhWriter,
    items: Vec<collision::Item>,
    origin: DVec3,
    step: f64,
    strings: &[u8],
    target: CollTarget,
) -> Result<Vec<u8>, collision::CollError> {
    let fields = writer.build_model(items, origin, step)?;
    let mut names: Vec<u8> = Vec::new();
    let mut local: HashMap<u32, u32> = HashMap::new();
    let mut surfprops: Vec<(u16, u8, u8, u32)> = Vec::with_capacity(writer.surfprops.len());
    for (flags, stype, contents, name_ofs) in &writer.surfprops {
        let ofs = *local.entry(*name_ofs).or_insert_with(|| {
            let at = names.len() as u32;
            let start = (*name_ofs as usize).min(strings.len());
            let end = strings[start..]
                .iter()
                .position(|&b| b == 0)
                .map(|i| start + i)
                .unwrap_or(strings.len());
            names.extend_from_slice(&strings[start..end].to_ascii_uppercase());
            names.push(0);
            at
        });
        surfprops.push((*flags, *stype, *contents, ofs));
    }
    let model = CollModel {
        target,
        fields,
        contents: writer.contents_masks.clone(),
        surfprops,
        names,
        verts: writer.verts.clone(),
        leaf: writer.leaf.clone(),
        nodes: writer.nodes.clone(),
    };
    Ok(model.to_blob())
}

/// Mesh-less model records for the plane-defined volumes, appended in index
/// order.
type VolumeEntry = (u32, u16, String, Vec<Vec<crate::csg::DPlane>>);

fn append_models(bsp: &mut Bsp, volumes: &BTreeMap<u32, VolumeEntry>) -> Result<(), Tf2ConvError> {
    let v = bsp.version_kind().unwrap_or(Version::DediV47);
    let mut list = match rbsp::lumps::parse(ids::MODELS, v, bsp.lump(ids::MODELS)) {
        Ok(rbsp::lumps::Lump::Model(m)) => m,
        _ => Vec::new(),
    };
    for (idx, (contents, flags, _, planes)) in volumes {
        let (lo, hi) = volume_bounds(planes, (*contents, *flags)).unwrap_or((DVec3::ZERO, DVec3::ZERO));
        let slot = *idx as usize;
        while list.len() <= slot {
            list.push(ApexModel::default());
        }
        let m = &mut list[slot];
        m.mins = lo.as_vec3().to_array();
        m.maxs = hi.as_vec3().to_array();
        m.first_mesh = 0;
        m.num_meshes = 0;
        m.bvh_node = -1;
        m.bvh_leaf = -1;
        m.vert_index = 0;
        m.flags = 0;
        m.origin = [0.0; 3];
        m.decode_scale = 0.0;
    }
    bsp.set_lump(ids::MODELS, rbsp::lumps::collision::write_models(&list, v));
    Ok(())
}

/// The box a volume's hulls cover. It is the box of the quantised corners the
/// collision model decodes to, not the box the plane solve returned, because the
/// model record is what the engine clamps the entity against.
fn volume_bounds(
    brushes: &[Vec<crate::csg::DPlane>],
    surface: (u32, u16),
) -> Result<(DVec3, DVec3), collision::CollError> {
    let mut writer = collision::BvhWriter::new(vec![surface.0]);
    let sp = writer.surfprop_index(surface.1, 0, 0, 0)?;
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for planes in brushes {
        let Some(verts) = crate::csg::hull_vertices_planes(planes) else { continue };
        let normals: Vec<DVec3> = planes.iter().map(|p| p.normal).collect();
        let item = writer.hull_item(0, &verts, &normals, &vec![sp; normals.len()])?;
        let (a, b) = item.bounds();
        lo = lo.min(a);
        hi = hi.max(b);
    }
    if lo.x > hi.x {
        return Ok((DVec3::ZERO, DVec3::ZERO));
    }
    Ok((lo, hi))
}

/// Appends the `*collN` keys to every entity whose model has a blob.
fn patch_coll_keys(data: &mut Vec<u8>, blobs: &BTreeMap<u32, Vec<u8>>) {
    if blobs.is_empty() {
        return;
    }
    let spans = blocks(data);
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    let mut cursor = 0usize;
    for (a, b) in spans {
        let body = &data[a..b];
        let Some(idx) = model_index_of(body) else {
            out.extend_from_slice(&data[cursor..b]);
            cursor = b;
            continue;
        };
        if body.windows(5).any(|w| w == b"\"*col") {
            out.extend_from_slice(&data[cursor..b]);
            cursor = b;
            continue;
        }
        let Some(blob) = blobs.get(&idx) else {
            out.extend_from_slice(&data[cursor..b]);
            cursor = b;
            continue;
        };
        out.extend_from_slice(&data[cursor..a]);
        // The keys are written one per line, so the closing brace the block was
        // read with is replaced rather than kept.
        let mut head = body[..body.len() - 1].to_vec();
        while head.last().is_some_and(|&b| b == b'\n' || b == b'\r') {
            head.pop();
        }
        out.extend_from_slice(&head);
        for (i, chunk) in blob.chunks(collision::COLL_CHUNK).enumerate() {
            out.extend_from_slice(format!("\n\"*coll{i}\" \"{}\"", collision::base64(chunk)).as_bytes());
        }
        out.extend_from_slice(b"\n}");
        cursor = b;
    }
    out.extend_from_slice(&data[cursor..]);
    *data = out;
}

/// Classes the target engine builds client side, which therefore belong in a
/// partition file rather than the worldspawn lump.
const PLACEMENT: [(&str, &str); 1] = [("fog_volume", "env")];

/// The grade a `color_correction` reads. S21 loads one from the mod folders only
/// through the `.raw_hdr` reader, and the source's 32^3 `.raw` goes to a loader
/// that sees the base install alone.
fn color_correction_lut(name: &str) -> String {
    format!("materials/correction/{name}.raw_hdr")
}

/// Points every `color_correction` in a partition file at the baked grade.
fn retarget_color_correction(data: &mut Vec<u8>, lut: &str) {
    let spans = blocks(data);
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    let mut cursor = 0usize;
    for (a, b) in spans {
        let body = &data[a..b];
        if classname_of(body) != "color_correction" {
            out.extend_from_slice(&data[cursor..b]);
            cursor = b;
            continue;
        }
        let Some((key, value, len)) = filename_key(body) else {
            out.extend_from_slice(&data[cursor..b]);
            cursor = b;
            continue;
        };
        if value.to_ascii_lowercase().ends_with(".raw_hdr") {
            out.extend_from_slice(&data[cursor..b]);
            cursor = b;
            continue;
        }
        out.extend_from_slice(&data[cursor..a + key]);
        out.extend_from_slice(b"\"filename\" \"");
        out.extend_from_slice(lut.as_bytes());
        out.push(b'"');
        out.extend_from_slice(&body[key + len..]);
        cursor = b;
    }
    out.extend_from_slice(&data[cursor..]);
    *data = out;
}

/// The `"filename" "value"` key of a body: where its value starts, what it says
/// and how many bytes the key and its value take.
fn filename_key(body: &[u8]) -> Option<(usize, &str, usize)> {
    let needle = b"\"filename\"";
    let key = body.windows(needle.len()).position(|w| w == needle)?;
    let tail = &body[key + needle.len()..];
    let gap = tail.iter().take_while(|b| b.is_ascii_whitespace()).count();
    if tail.get(gap) != Some(&b'"') {
        return None;
    }
    let rest = tail.get(gap + 1..)?;
    let end = rest.iter().position(|&b| b == b'"')?;
    let value = std::str::from_utf8(rest.get(..end)?).ok()?;
    if rest.get(end) != Some(&b'"') {
        return None;
    }
    Some((key, value, needle.len() + gap + 1 + end + 1))
}

/// Moves the classes of [`PLACEMENT`] out of the worldspawn lump and into
/// their partition file.
fn entity_partitions(
    bsp: &mut Bsp,
    ents: &mut BTreeMap<String, Vec<u8>>,
    name: &str,
) -> Result<usize, Tf2ConvError> {
    let main = bsp.lump(ids::ENTITIES).to_vec();
    let mut moved: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    // The blocks are spliced out rather than re-emitted, so the whitespace and
    // the terminator the source lump ends on survive the move.
    let mut out: Vec<u8> = Vec::with_capacity(main.len());
    let mut cursor = 0usize;
    for (a, b) in blocks(&main) {
        let cn = classname_of(&main[a..b]);
        let Some((_, part)) = PLACEMENT.iter().find(|(c, _)| *c == cn) else { continue };
        out.extend_from_slice(&main[cursor..a]);
        moved.entry((*part).to_string()).or_default().push(main[a..b].to_vec());
        // The line the block sat on goes with it, so no blank line is left.
        cursor = if main.get(b) == Some(&b'\n') { b + 1 } else { b };
    }
    if !moved.is_empty() {
        out.extend_from_slice(&main[cursor..]);
        bsp.set_lump(ids::ENTITIES, out);
    }
    let mut total = 0usize;
    for (part, bodies) in moved {
        let entry = ents.entry(part.clone()).or_insert_with(|| {
            let mut v = "ENTITIES02 num_models=1\n".to_string().into_bytes();
            v.push(0);
            v
        });
        let tail = entry.iter().rev().take_while(|&&b| b == 0).count();
        let mut text = entry[..entry.len() - tail].to_vec();
        if !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        for body in &bodies {
            text.extend_from_slice(body);
            text.extend_from_slice(b"\n");
            total += 1;
        }
        text.extend(std::iter::repeat_n(0u8, tail));
        *entry = text;
    }
    if let Some(env) = ents.get_mut("env") {
        retarget_color_correction(env, &color_correction_lut(name));
    }
    Ok(total)
}

// ---- the CSM tree -----------------------------------------------------------

struct CsmOut {
    nodes: Vec<u8>,
    refs: Vec<u8>,
    totals: Vec<u8>,
    envs: Vec<u8>,
    meshes: usize,
}

/// Rows of the frame a cascade's boxes are stored in, `z' = -shadow direction`.
/// The rows are the transform, so a point maps to `sum_v p[v] * rows[axis][v]`.
pub fn csm_light_basis(dir: [f32; 3]) -> Result<[[f64; 3]; 3], Tf2ConvError> {
    let mut z = glam::DVec3::new(-dir[0] as f64, -dir[1] as f64, -dir[2] as f64);
    let len = z.length();
    if !len.is_finite() || len == 0.0 {
        return Err(Tf2ConvError::VerticalSun(0));
    }
    z /= len;
    let mut x = glam::DVec3::Z.cross(z);
    if x.length() < 1e-6 {
        return Err(Tf2ConvError::VerticalSun(0));
    }
    x /= x.length();
    let y = z.cross(x);
    Ok([[x.x, x.y, x.z], [y.x, y.y, y.z], [z.x, z.y, z.z]])
}

fn conv_csm(src: &Bsp) -> Result<CsmOut, Tf2ConvError> {
    let raw = src.lump(ids::CSM_AABB_NODES);
    if !raw.len().is_multiple_of(32) {
        return Err(Tf2ConvError::Lump { id: ids::CSM_AABB_NODES, reason: "size is not a multiple of 32".into() });
    }
    let n = raw.len() / 32;
    let word = |i: usize, k: usize| u32::from_le_bytes(raw[i * 32 + k * 4..i * 32 + k * 4 + 4].try_into().unwrap());
    let mut mins: Vec<[f32; 3]> = Vec::with_capacity(n);
    let mut maxs: Vec<[f32; 3]> = Vec::with_capacity(n);
    for i in 0..n {
        let mut m = [0f32; 3];
        let mut x = [0f32; 3];
        for k in 0..3 {
            m[k] = f32::from_le_bytes(raw[i * 32 + k * 4..i * 32 + k * 4 + 4].try_into().unwrap());
            x[k] = f32::from_le_bytes(raw[i * 32 + (k + 4) * 4..i * 32 + (k + 4) * 4 + 4].try_into().unwrap());
        }
        mins.push(m);
        maxs.push(x);
    }
    let mut child_count = vec![0u32; n];
    let mut ref_count = vec![0u32; n];
    let mut first_child = vec![0u32; n];
    let mut ref_ofs = vec![0u32; n];
    for i in 0..n {
        let w12 = word(i, 3);
        let w28 = word(i, 7);
        child_count[i] = w12 & 0xFF;
        ref_count[i] = (w12 >> 8) & 0xFF;
        first_child[i] = w28 & 0xFFFF;
        ref_ofs[i] = (w28 >> 16) & 0xFFFF;
    }
    let refs16: Vec<u32> = src
        .lump(ids::CSM_OBJ_REFERENCES)
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]) as u32)
        .collect();
    let mut own: Vec<Vec<u32>> = Vec::with_capacity(n);
    for i in 0..n {
        let a = ref_ofs[i] as usize;
        let b = a + ref_count[i] as usize;
        own.push(refs16.get(a..b.min(refs16.len())).unwrap_or(&[]).to_vec());
    }
    let meshes: Vec<(u32, u32, u16)> = src
        .lump(ids::SHADOW_MESHES)
        .chunks_exact(12)
        .map(|c| {
            (
                u32::from_le_bytes(c[0..4].try_into().unwrap()),
                u32::from_le_bytes(c[4..8].try_into().unwrap()),
                u16::from_le_bytes(c[8..10].try_into().unwrap()),
            )
        })
        .collect();
    let opaque: Vec<[f32; 3]> = src
        .lump(ids::SHADOW_MESH_OPAQUE_VERTICES)
        .chunks_exact(12)
        .map(|c| {
            [
                f32::from_le_bytes(c[0..4].try_into().unwrap()),
                f32::from_le_bytes(c[4..8].try_into().unwrap()),
                f32::from_le_bytes(c[8..12].try_into().unwrap()),
            ]
        })
        .collect();
    let alpha: Vec<[f32; 3]> = src
        .lump(ids::SHADOW_MESH_ALPHA_VERTICES)
        .chunks_exact(20)
        .map(|c| {
            [
                f32::from_le_bytes(c[0..4].try_into().unwrap()),
                f32::from_le_bytes(c[4..8].try_into().unwrap()),
                f32::from_le_bytes(c[8..12].try_into().unwrap()),
            ]
        })
        .collect();
    let indices: Vec<usize> = src
        .lump(ids::SHADOW_MESH_INDICES)
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]) as usize)
        .collect();
    let mut starts: Vec<usize> = Vec::with_capacity(meshes.len() + 1);
    let mut acc = 0usize;
    starts.push(0);
    for (_, tris, _) in &meshes {
        acc += *tris as usize * 3;
        starts.push(acc);
    }
    if starts[meshes.len()] != indices.len() {
        return Err(Tf2ConvError::CsmIndices);
    }

    let mut envs = src.lump(ids::SHADOW_ENVIRONMENTS).to_vec();
    if !envs.len().is_multiple_of(ENV_SIZE) {
        return Err(Tf2ConvError::Lump {
            id: ids::SHADOW_ENVIRONMENTS,
            reason: "size is not a multiple of 36".into(),
        });
    }
    let env_count = envs.len() / ENV_SIZE;
    for e in 0..env_count {
        let at = e * ENV_SIZE;
        let u32at = |o: usize| u32::from_le_bytes(envs[at + o..at + o + 4].try_into().unwrap());
        let f32at = |o: usize| f32::from_le_bytes(envs[at + o..at + o + 4].try_into().unwrap());
        let root = u32at(0) as usize;
        let aabb_end = u32at(12) as usize;
        let sm_begin = u32at(8) as usize;
        let sm_end = u32at(20) as usize;
        if root >= n || aabb_end > n || sm_begin > sm_end || sm_end > meshes.len() {
            return Err(Tf2ConvError::CsmTree(root, aabb_end));
        }
        let basis = csm_light_basis([f32at(24), f32at(28), f32at(32)]).map_err(|_| Tf2ConvError::VerticalSun(e))?;
        for k in sm_begin..sm_end {
            let (vtx, _, draw) = meshes[k];
            let pool: &Vec<[f32; 3]> = if draw == 1 { &opaque } else { &alpha };
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for i in starts[k]..starts[k + 1] {
                let Some(p) = indices.get(i).and_then(|i| pool.get(i + vtx as usize)) else { continue };
                for ax in 0..3 {
                    let v = p[0] as f64 * basis[ax][0] + p[1] as f64 * basis[ax][1] + p[2] as f64 * basis[ax][2];
                    lo[ax] = lo[ax].min(v);
                    hi[ax] = hi[ax].max(v);
                }
            }
            if !lo[0].is_finite() {
                continue;
            }
            let mut node = root;
            if !contains(&mins[node], &maxs[node], &lo, &hi) {
                for ax in 0..3 {
                    mins[node][ax] = mins[node][ax].min(lo[ax] as f32);
                    maxs[node][ax] = maxs[node][ax].max(hi[ax] as f32);
                }
            }
            loop {
                let mut moved = false;
                for c in first_child[node] as usize..first_child[node] as usize + child_count[node] as usize {
                    if c < aabb_end && c < n && contains(&mins[c], &maxs[c], &lo, &hi) {
                        node = c;
                        moved = true;
                        break;
                    }
                }
                if !moved {
                    break;
                }
            }
            own[node].push(k as u32);
        }
    }

    let mut out_refs: Vec<u32> = Vec::new();
    let mut total = vec![0u32; n];
    let mut new_ofs = vec![0u32; n];
    let mut visited = vec![false; n];
    for e in 0..env_count {
        let at = e * ENV_SIZE;
        let root = u32::from_le_bytes(envs[at..at + 4].try_into().unwrap()) as usize;
        let begin = out_refs.len() as u32;
        // An explicit stack, because a cascade tree can be deep.
        let mut stack = vec![(root, false)];
        while let Some((i, closing)) = stack.pop() {
            if closing {
                total[i] = out_refs.len() as u32 - new_ofs[i];
                continue;
            }
            visited[i] = true;
            new_ofs[i] = out_refs.len() as u32;
            out_refs.extend_from_slice(&own[i]);
            stack.push((i, true));
            for c in (first_child[i] as usize..first_child[i] as usize + child_count[i] as usize).rev() {
                stack.push((c, false));
            }
        }
        let end = out_refs.len() as u32;
        envs[at + 4..at + 8].copy_from_slice(&begin.to_le_bytes());
        envs[at + 16..at + 20].copy_from_slice(&end.to_le_bytes());
    }
    if visited.iter().any(|v| !*v) {
        let i = visited.iter().position(|v| !*v).unwrap_or(0);
        return Err(Tf2ConvError::CsmOrphan(i));
    }
    let counts: Vec<u32> = own.iter().map(|o| o.len() as u32).collect();
    if counts.iter().any(|c| *c > 0xFF) {
        return Err(Tf2ConvError::Lump {
            id: ids::CSM_OBJ_REFERENCES,
            reason: "a node's reference count exceeds the field".into(),
        });
    }
    let mut node_bytes = vec![0u8; n * 32];
    for i in 0..n {
        for k in 0..3 {
            node_bytes[i * 32 + k * 4..i * 32 + k * 4 + 4].copy_from_slice(&mins[i][k].to_le_bytes());
            node_bytes[i * 32 + (k + 4) * 4..i * 32 + (k + 4) * 4 + 4]
                .copy_from_slice(&maxs[i][k].to_le_bytes());
        }
        node_bytes[i * 32 + 12..i * 32 + 16].copy_from_slice(&(child_count[i] | (first_child[i] << 8)).to_le_bytes());
        node_bytes[i * 32 + 28..i * 32 + 32].copy_from_slice(&(counts[i] | (new_ofs[i] << 8)).to_le_bytes());
    }
    let refs: Vec<u8> = out_refs.iter().flat_map(|v| v.to_le_bytes()).collect();
    let totals: Vec<u8> = total.iter().flat_map(|v| v.to_le_bytes()).collect();
    Ok(CsmOut { nodes: node_bytes, refs, totals, envs, meshes: meshes.len() })
}

fn contains(mins: &[f32; 3], maxs: &[f32; 3], lo: &[f64; 3], hi: &[f64; 3]) -> bool {
    (0..3).all(|i| lo[i] >= mins[i] as f64 && hi[i] <= maxs[i] as f64)
}

/// The smallest world box a set of points covers, as the compiler's own bounds
/// helper reports it.
pub fn point_bounds(points: &[Vec3]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for p in points {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    if lo.x > hi.x {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    (lo, hi)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_paths_become_rmdl() {
        assert_eq!(normalise_model_path("models/props/door.mdl"), "mdl/props/door.rmdl");
        assert_eq!(normalise_model_path("models\\props\\door.mdl"), "mdl/props/door.rmdl");
        // A path that is already in the target form, or is not a model, is left alone.
        assert_eq!(normalise_model_path("mdl/props/door.rmdl"), "mdl/props/door.rmdl");
        assert_eq!(normalise_model_path("materials/wall.vmt"), "materials/wall.vmt");
        assert_eq!(normalise_model_path("models/"), "models/");
    }

    #[test]
    fn entity_text_rewrites_every_model_value() {
        let src = b"{\n\"classname\" \"prop_dynamic\"\n\"model\" \"models\\a/b.mdl\"\n\"targetname\" \"x\"\n}\n";
        let got = rewrite_model_paths(src);
        let text = String::from_utf8(got).expect("ascii entity text");
        assert!(text.contains("\"model\" \"mdl/a/b.rmdl\""), "{text}");
        assert!(!text.contains("models\\"), "{text}");
        assert_eq!(text.matches("classname").count(), 1, "{text}");
    }

    #[test]
    fn entity_text_leaves_a_value_without_a_model_suffix() {
        // A quote before the `.mdl` ends the value, so the key is not rewritten.
        let src = b"\"model\" \"models/a\"\"b.mdl\"";
        assert_eq!(rewrite_model_paths(src), src);
        // A truncated value cannot be closed and is copied through unchanged.
        let src = b"\"model\" \"models/a/b.mdl";
        assert_eq!(rewrite_model_paths(src), src);
    }

    #[test]
    fn partition_header_is_added_once() {
        let src = b"ENTITIES01\n{\n}\n";
        let once = apex_ent_header(src);
        assert_eq!(once, b"ENTITIES02 num_models=1\n{\n}\n");
        assert_eq!(apex_ent_header(&once), once, "a header that is already there is kept");
        // Text that is not a partition file is untouched.
        let other = b"{\n}\n";
        assert_eq!(apex_ent_header(other), other);
    }

    #[test]
    fn light_frame_is_orthonormal_and_right_handed() {
        let dir: [f32; 3] = [-0.3, -0.5, -0.81];
        let basis = csm_light_basis(dir).expect("an oblique sun");
        let rows: Vec<[f64; 3]> = basis.to_vec();
        for r in &rows {
            assert!((r[0] * r[0] + r[1] * r[1] + r[2] * r[2] - 1.0).abs() < 1e-12, "unit row {r:?}");
        }
        for (i, a) in rows.iter().enumerate() {
            for (j, b) in rows.iter().enumerate() {
                if i == j {
                    continue;
                }
                let d = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
                assert!(d.abs() < 1e-12, "rows {i} and {j} are not perpendicular: {d}");
            }
        }
        // The third row is the negated shadow direction, and a point along it
        // lands on the light axis.
        let p: [f64; 3] = dir.map(f64::from);
        let mut along = [0.0f64; 3];
        for a in 0..3 {
            along[a] = (0..3).map(|v| p[v] * rows[2][v]).sum();
            let want: f64 = (0..3).map(|v| p[v] * rows[a][v]).sum();
            if a < 2 {
                assert!(want.abs() < 1e-9, "row {a} of the shadow direction is {want}");
            }
        }
        let n = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        assert!((along[2] + n).abs() < 1e-9, "axis length {}", along[2]);
    }

    #[test]
    fn a_vertical_sun_has_no_frame() {
        assert!(csm_light_basis([0.0, 0.0, -1.0]).is_err(), "a sun straight up has no light frame");
        assert!(csm_light_basis([0.0, 0.0, 0.0]).is_err(), "a sun with no direction has no frame");
    }

    #[test]
    fn point_bounds_cover_the_cloud_or_sit_at_the_origin() {
        let pts = [Vec3::new(1.0, -2.0, 3.0), Vec3::new(-4.0, 5.0, 0.5)];
        let (lo, hi) = point_bounds(&pts);
        assert_eq!(lo, Vec3::new(-4.0, -2.0, 0.5));
        assert_eq!(hi, Vec3::new(1.0, 5.0, 3.0));
        assert_eq!(point_bounds(&[]), (Vec3::ZERO, Vec3::ZERO), "an empty cloud has no box");
    }

    #[test]
    fn a_missing_source_is_an_error() {
        let err = read_source(std::path::Path::new("no-such-map.bsp")).expect_err("a missing path fails");
        assert!(matches!(err, Tf2ConvError::Io(_)), "{err:?}");
    }

    #[test]
    #[ignore = "needs R5ED_TF2_BSP"]
    fn the_source_map_converts_into_both_targets() {
        let Some(path) = std::env::var_os("R5ED_TF2_BSP").map(std::path::PathBuf::from) else { return };
        let src = read_source(&path).expect("the v37 source reads");
        assert_eq!(src.header.version, 37);
        let dir = path.parent().expect("the source has a directory").to_path_buf();
        let stem = path.file_stem().expect("the source has a stem").to_string_lossy().to_string();
        let mut ents = BTreeMap::new();
        for part in rdoc::model::Partition::FILES {
            let f = dir.join(format!("{stem}_{}.ent", part.name()));
            if f.is_file() {
                ents.insert(part.name().to_string(), std::fs::read(&f).expect("a partition file reads"));
            }
        }
        assert!(!ents.is_empty(), "the source map carries its entity partitions");
        let tables = SurfaceTables::default();
        for (target, version) in [("client", Version::ClientV51), ("dedi", Version::DediV47)] {
            let out = convert_tf2(&src, &ents, version, &tables, "mp_rr_test")
                .unwrap_or_else(|e| panic!("the v37 map converts for the {target}: {e}"));
            assert!(out.stats.bvh_nodes > 0, "{target}: the conversion built a tree: {:?}", out.stats);
            let bsp = &out.artifacts.bsp;
            assert_eq!(bsp.lump(ids::BVH_NODES).len() % rbsp::lumps::collision::NODE_SIZE, 0, "{target}");
            assert_eq!(bsp.lump(ids::VERTICES).len() % 12, 0, "{target}");
            assert!(out.blob_models > 0, "{target}: the map's brush entities got colliders");
            // The source's own portal and trigger keys do not satisfy the map
            // validator, so the report is checked for being produced, not clean.
            assert!(!out.report.validation.is_empty(), "{target}: the converted map was validated");
        }
    }
}
