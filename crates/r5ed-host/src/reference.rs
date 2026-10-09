//! The reference layer: a shipped map's render geometry, chunked for the page.
//!
//! A map in an install lives inside its pak, so the first thing the editor needs
//! is a readable BSP. The `import_map` job exports the map's wrap assets into
//! the cache, and this module turns the render lumps of the client map that
//! lands there into glb chunks the page can stream.
//!
//! The decode is defensive by construction: every index is checked against the
//! pool it indexes, a mesh that does not decode is counted and left out rather
//! than aborting the map, and the counts are reported in the reference document
//! so nothing is dropped silently.

use std::path::{Path, PathBuf};

use rbsp::{Bsp, Version};
use rpak::{chunk_bounds, chunk_to_glb, ChunkPrimitive, MeshChunk};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, Result};

/// One entity as the reference layer reports it: the class and where it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefEntity {
    pub classname: String,
    pub origin: [f32; 3],
    pub model: Option<String>,
}

/// The extent of a map's geometry, as the page wants it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

/// The document the page loads a map's reference layer from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub stem: String,
    /// One URL per chunk, in load order.
    pub chunks: Vec<String>,
    pub entities: Vec<RefEntity>,
    pub bounds: Bounds,
    /// Meshes the map declares, how many of them decoded, and how many triangles
    /// that is. The editor shows the difference rather than hiding it.
    pub meshes: usize,
    pub decoded: usize,
    pub triangles: usize,
    pub skipped: Vec<String>,
}

/// Triangles per chunk. Small enough that a page can hold several at once, big
/// enough that a shipped map is a few dozen requests rather than thousands.
pub const CHUNK_TRIANGLES: usize = 24_000;
/// A map cannot produce more chunks than this, whatever the lumps claim.
pub const MAX_CHUNKS: usize = 4096;
/// The most entities a reference document carries.
const MAX_ENTITIES: usize = 20_000;
/// The record size of each render vertex lump, and the two leading indices.
mod vert {
    /// `LUMP_VERTS_UNLIT`: position, normal, then the packed stream.
    pub const UNLIT: usize = 20;
    /// `LUMP_VERTS_LIT_FLAT`: the unlit pair plus one more packed dword.
    pub const LIT_FLAT: usize = 24;
    /// `LUMP_VERTS_LIT_BUMP`: the flat pair plus a tangent and the stream.
    pub const LIT_BUMP: usize = 32;
    /// `LUMP_VERTS_UNLIT_TS`: the unlit pair plus a tangent and pad.
    pub const UNLIT_TS: usize = 24;
}

/// A float vector as the vertex and position lumps store it.
fn vec3_at(b: &[u8], i: usize) -> Option<[f32; 3]> {
    let o = i.checked_mul(12)?;
    let s = b.get(o..o.checked_add(12)?)?;
    let mut v = [0.0f32; 3];
    for (k, out) in v.iter_mut().enumerate() {
        *out = f32::from_le_bytes([s[k * 4], s[k * 4 + 1], s[k * 4 + 2], s[k * 4 + 3]]);
    }
    v.iter().all(|x| x.is_finite()).then_some(v)
}

/// The two leading `u32` of a vertex record: the position and normal it uses.
fn vert_pair(b: &[u8], i: usize, stride: usize) -> Option<(u32, u32)> {
    let o = i.checked_mul(stride)?;
    let s = b.get(o..o + 8)?;
    let p = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
    let n = u32::from_le_bytes([s[4], s[5], s[6], s[7]]);
    Some((p, n))
}

/// The record size and the lump a mesh's vertex type selects.
fn vertex_lump(t: u8) -> Option<(usize, u16)> {
    match t {
        0 => Some((vert::UNLIT, 0x47)),
        1 => Some((vert::LIT_FLAT, 0x48)),
        2 => Some((vert::LIT_BUMP, 0x49)),
        3 => Some((vert::UNLIT_TS, 0x4A)),
        _ => None,
    }
}

/// The render lumps of one map, decoded into chunks.
pub struct Decoded {
    pub chunks: Vec<MeshChunk>,
    pub entities: Vec<RefEntity>,
    pub bounds: ([f32; 3], [f32; 3]),
    pub meshes: usize,
    pub decoded: usize,
    pub triangles: usize,
    pub skipped: Vec<String>,
}

/// The `LUMP_MATERIAL_SORT` fields the reference layer reads, at their byte
/// offsets inside the 12-byte record.
mod sort_field {
    pub const LIGHTMAP: usize = 2;
    pub const FIRST_VERTEX: usize = 8;
    pub const STRIDE: usize = 12;
}

/// The `LUMP_TEXDATA` name offset, at its byte offset inside the 16-byte record.
const TEXDATA_NAME: usize = 0;
const TEXDATA_STRIDE: usize = 16;

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    let s = b.get(o..o.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    let s = b.get(o..o.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// One `LUMP_MESHES` record, 28 bytes at v47 and v51 alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MeshRec {
    first_index: u32,
    triangle_count: u16,
    first_vertex: u16,
    vertex_count: u16,
    vertex_type: u8,
    material_sort: u16,
}

const MESH_STRIDE: usize = 28;
const SORT_STRIDE: usize = sort_field::STRIDE;

fn mesh_at(b: &[u8], i: usize) -> Option<MeshRec> {
    let o = i.checked_mul(MESH_STRIDE)?;
    let s = b.get(o..o + MESH_STRIDE)?;
    Some(MeshRec {
        first_index: u32::from_le_bytes(s[0..4].try_into().ok()?),
        triangle_count: u16::from_le_bytes(s[4..6].try_into().ok()?),
        first_vertex: u16::from_le_bytes(s[6..8].try_into().ok()?),
        vertex_count: u16::from_le_bytes(s[8..10].try_into().ok()?),
        vertex_type: s[10],
        material_sort: u16::from_le_bytes(s[22..24].try_into().ok()?),
    })
}

/// The material path a material sort points at, through the texdata table and
/// the name blob.
fn sort_material(sort: u16, texdata: &[u8], names: &[u8]) -> String {
    let at = (sort as usize).checked_mul(TEXDATA_STRIDE).and_then(|o| o.checked_add(TEXDATA_NAME));
    let Some(off) = at.and_then(|o| u32_at(texdata, o)) else { return String::new() };
    let start = off as usize;
    if start >= names.len() {
        return String::new();
    }
    let tail = &names[start..];
    let end = tail.iter().position(|c| *c == 0).unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[..end]).into_owned()
}

/// Decodes the render lumps of a client or dedicated map.
///
/// A map with no render vertex lumps decodes to nothing rather than failing: a
/// dedicated map genuinely does not carry them.
pub fn decode(bsp: &Bsp) -> Result<Decoded> {
    let version = bsp
        .version_kind()
        .ok_or_else(|| ApiError::bad(format!("map version {} is not one this build reads", bsp.version())))?;
    if version == Version::Tf2V37 {
        return Err(ApiError::bad("a v37 map has no render lumps to show"));
    }
    let verts = bsp.lump(0x03);
    let normals = bsp.lump(0x1E);
    let indices = bsp.lump(0x4F);
    let meshes = bsp.lump(0x50);
    let sorts = bsp.lump(0x52);
    let texdata = bsp.lump(0x02);
    let names = bsp.lump(0x0F);
    let mesh_count = meshes.len() / MESH_STRIDE;
    let sort_count = sorts.len() / SORT_STRIDE;

    let mut chunks: Vec<MeshChunk> = Vec::new();
    let mut current = MeshChunk::new("chunk_0");
    let mut skipped: Vec<String> = Vec::new();
    let mut decoded = 0usize;
    let mut triangles = 0usize;
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];

    for mi in 0..mesh_count {
        let Some(m) = mesh_at(meshes, mi) else {
            skipped.push(format!("mesh {mi}: truncated record"));
            continue;
        };
        if m.triangle_count == 0 {
            skipped.push(format!("mesh {mi}: no triangles"));
            continue;
        }
        let Some((stride, vump)) = vertex_lump(m.vertex_type) else {
            skipped.push(format!("mesh {mi}: unknown vertex type {}", m.vertex_type));
            continue;
        };
        if m.material_sort as usize >= sort_count {
            skipped.push(format!("mesh {mi}: material sort {} is past the table", m.material_sort));
            continue;
        }
        // A litmapped run starts at the sort's own base; an unlitmapped one has
        // a run of its own that starts at zero.
        let sort_at = (m.material_sort as usize).checked_mul(SORT_STRIDE);
        let Some(sort_at) = sort_at else {
            skipped.push(format!("mesh {mi}: material sort overflows"));
            continue;
        };
        let unlitmapped = u16_at(sorts, sort_at + sort_field::LIGHTMAP) == Some(u16::MAX);
        let base = if unlitmapped {
            0
        } else {
            u32_at(sorts, sort_at + sort_field::FIRST_VERTEX).unwrap_or(0) as usize
        };
        let vdata = bsp.lump(vump);
        let vcount = vdata.len() / stride;
        let first = base.checked_add(m.first_vertex as usize);
        let count = m.vertex_count as usize;
        // A mesh's run is its first vertex through its count, both ends
        // included, so a run of `n` spans `n + 1` records and the next mesh in
        // the same sort starts on the one after it.
        let Some(first) = first else {
            skipped.push(format!("mesh {mi}: vertex range overflows"));
            continue;
        };
        let Some(vlast) = first.checked_add(count).filter(|e| *e < vcount) else {
            skipped.push(format!("mesh {mi}: vertex range past the lump"));
            continue;
        };
        let run = vlast - first + 1;
        let Some(ibeg) = (m.first_index as usize).checked_add(0) else { continue };
        let Some(iend) = ibeg.checked_add(m.triangle_count as usize * 3) else {
            skipped.push(format!("mesh {mi}: index range overflows"));
            continue;
        };
        if iend > indices.len() / 2 {
            skipped.push(format!("mesh {mi}: index range past the lump"));
            continue;
        }

        let mut prim = ChunkPrimitive::empty(sort_material(m.material_sort, texdata, names));
        if prim.name.is_empty() {
            prim.name = format!("mesh_{mi}");
        }
        let mut bad = false;
        for vi in 0..run {
            let Some((pi, ni)) = vert_pair(vdata, first + vi, stride) else {
                bad = true;
                break;
            };
            let Some(p) = vec3_at(verts, pi as usize) else {
                bad = true;
                break;
            };
            let n = vec3_at(normals, ni as usize).unwrap_or([0.0, 0.0, 1.0]);
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
            prim.positions.push(p);
            prim.normals.push(n);
        }
        if bad {
            skipped.push(format!("mesh {mi}: a vertex index is past its pool"));
            continue;
        }
        for ii in ibeg..iend {
            let Some(at) = ii.checked_mul(2) else {
                bad = true;
                break;
            };
            let Some(raw) = u16_at(indices, at) else {
                bad = true;
                break;
            };
            // The index stream is offset from the sort's base, so it is rebased
            // onto the mesh's own first vertex before it reaches the page.
            let rel = raw.checked_sub(m.first_vertex);
            let Some(rel) = rel.filter(|r| (*r as usize) <= count) else {
                bad = true;
                break;
            };
            prim.indices.push(rel as u32);
        }
        if bad {
            skipped.push(format!("mesh {mi}: an index is outside the mesh's vertices"));
            continue;
        }
        let tris = prim.indices.len() / 3;
        triangles += tris;
        decoded += 1;
        if current.triangles() + tris > CHUNK_TRIANGLES && !current.primitives.is_empty() {
            chunks.push(std::mem::replace(&mut current, MeshChunk::new(format!("chunk_{}", chunks.len()))));
        }
        current.push(prim);
        if chunks.len() >= MAX_CHUNKS {
            break;
        }
    }
    if !current.primitives.is_empty() {
        chunks.push(current);
    }
    if !lo.iter().all(|x| x.is_finite()) {
        lo = [0.0; 3];
        hi = [0.0; 3];
    }
    let entities = read_entities(bsp);
    let _ = version;
    Ok(Decoded { chunks, entities, bounds: (lo, hi), meshes: mesh_count, decoded, triangles, skipped })
}

/// The entity list, taken from the map's own worldspawn lump. The partition
/// files beside a map carry the brush entities, which the reference layer does
/// not draw.
pub fn read_entities(bsp: &Bsp) -> Vec<RefEntity> {
    let mut out = Vec::new();
    let Some(version) = bsp.version_kind() else { return out };
    let Ok(rbsp::Lump::EntityText(items)) = rbsp::lumps::parse_at(0x00, version, bsp.lump(0x00), 0) else {
        return out;
    };
    for e in items.entities() {
        if out.len() >= MAX_ENTITIES {
            break;
        }
        let Some(classname) = e.classname() else { continue };
        if classname.is_empty() {
            continue;
        }
        out.push(RefEntity {
            classname: classname.to_string(),
            origin: e.get("origin").and_then(parse_origin).unwrap_or([0.0; 3]),
            model: e.get("modelpath").or_else(|| e.get("model")).map(str::to_string),
        });
    }
    out
}

/// An entity origin as three floats, in any of the spellings the maps use.
pub fn parse_origin(v: &str) -> Option<[f32; 3]> {
    let mut out = [0.0f32; 3];
    let mut n = 0;
    for part in v.split([' ', '\t', ',']).filter(|s| !s.is_empty()) {
        if n == 3 {
            return None;
        }
        out[n] = part.parse().ok()?;
        if !out[n].is_finite() {
            return None;
        }
        n += 1;
    }
    (n == 3).then_some(out)
}

/// Where a map's exported wrap assets and its decoded chunks live.
pub fn cache_dir(cache_root: &Path, stem: &str) -> PathBuf {
    cache_root.join("reference").join(stem)
}

/// The BSP an imported map was exported to. The asset tool writes the map's
/// lumps into `<export dir>/<stem>.bsp` beside the other wrap assets.
pub fn imported_bsp(cache_root: &Path, stem: &str) -> PathBuf {
    cache_root.join("import").join(stem).join(format!("{stem}.bsp"))
}

/// Decodes a map's reference layer, writing the chunks next to the document so
/// a second request for one of them is a file read.
pub fn build(cache_root: &Path, stem: &str) -> Result<(Reference, Vec<PathBuf>)> {
    let bsp_path = imported_bsp(cache_root, stem);
    if !bsp_path.is_file() {
        return Err(ApiError::not_found(format!(
            "{stem} has not been imported; run the import_map job for it first"
        )));
    }
    let dir = cache_dir(cache_root, stem);
    let d = decode(&Bsp::read_path(&bsp_path).map_err(|e| {
        ApiError::Internal(format!("{}: {e}", crate::paths::wire_path(&bsp_path)))
    })?)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| ApiError::Internal(format!("{}: {e}", dir.display())))?;
    let mut written = Vec::with_capacity(d.chunks.len());
    for (i, c) in d.chunks.iter().enumerate() {
        let glb = chunk_to_glb(c).map_err(|e| ApiError::Internal(e.to_string()))?;
        let p = dir.join(format!("{i}.glb"));
        std::fs::write(&p, glb).map_err(|e| ApiError::Internal(format!("{}: {e}", p.display())))?;
        written.push(p);
    }
    let bounds = if d.chunks.is_empty() { d.bounds } else { fold_bounds(&d.chunks) };
    Ok((
        Reference {
            stem: stem.to_string(),
            chunks: (0..d.chunks.len()).map(|i| format!("/api/maps/{stem}/chunk/{i}")).collect(),
            entities: d.entities,
            bounds: Bounds { min: bounds.0, max: bounds.1 },
            meshes: d.meshes,
            decoded: d.decoded,
            triangles: d.triangles,
            skipped: d.skipped,
        },
        written,
    ))
}

fn fold_bounds(chunks: &[MeshChunk]) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for c in chunks {
        let (a, b) = chunk_bounds(c);
        for k in 0..3 {
            lo[k] = lo[k].min(a[k]);
            hi[k] = hi[k].max(b[k]);
        }
    }
    (lo, hi)
}

/// One chunk's glb, decoded and written by `build` and read back here.
pub fn chunk(cache_root: &Path, stem: &str, n: usize) -> Result<Vec<u8>> {
    let p = cache_dir(cache_root, stem).join(format!("{n}.glb"));
    let bytes = std::fs::read(&p)
        .map_err(|_| ApiError::not_found(format!("{stem} has no chunk {n}; build the reference layer first")))?;
    if bytes.len() < 12 {
        return Err(ApiError::Internal("a chunk on disk is not a glb".into()));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vector_needs_three_finite_numbers() {
        assert_eq!(parse_origin("1 2 3"), Some([1.0, 2.0, 3.0]));
        assert_eq!(parse_origin("  -4.5\t0 , 12 "), Some([-4.5, 0.0, 12.0]));
        assert_eq!(parse_origin("1 2"), None);
        assert_eq!(parse_origin("1 2 3 4"), None);
        assert_eq!(parse_origin("1 2 x"), None);
        assert_eq!(parse_origin(""), None);
        assert_eq!(parse_origin("1 2 nan"), None);
        assert_eq!(parse_origin("1 2 inf"), None);
    }

    #[test]
    fn a_truncated_lump_reads_as_nothing_rather_than_panicking() {
        assert_eq!(vec3_at(&[0u8; 4], 0), None);
        assert_eq!(vec3_at(&[], 0), None);
        assert_eq!(u16_at(&[1, 2], 0), Some(0x0201));
        assert_eq!(u16_at(&[1, 2], 1), None);
        assert_eq!(u16_at(&[1, 2], usize::MAX), None);
        assert_eq!(u32_at(&[1, 2, 3, 4], 0), Some(0x0403_0201));
        assert_eq!(u32_at(&[0u8; 8], 1), Some(0), "the offsets are byte offsets");
        assert_eq!(u32_at(&[0u8; 8], 5), None);
        assert_eq!(u32_at(&[0u8; 8], usize::MAX), None);
        assert_eq!(u16_at(&[0u8; 4], 1), Some(0));
        assert_eq!(u16_at(&[0u8; 4], 3), None);
        assert_eq!(mesh_at(&[0u8; 3], 0), None);
        assert_eq!(mesh_at(&[0u8; 28], 0).map(|m| m.triangle_count), Some(0));
        assert_eq!(mesh_at(&[0u8; 28], 1), None);
    }

    #[test]
    fn every_vertex_type_maps_to_a_lump_and_a_stride() {
        assert_eq!(vertex_lump(0), Some((vert::UNLIT, 0x47)));
        assert_eq!(vertex_lump(1), Some((vert::LIT_FLAT, 0x48)));
        assert_eq!(vertex_lump(2), Some((vert::LIT_BUMP, 0x49)));
        assert_eq!(vertex_lump(3), Some((vert::UNLIT_TS, 0x4A)));
        assert_eq!(vertex_lump(4), None);
        assert_eq!(vertex_lump(255), None);
    }

    #[test]
    fn a_material_name_stops_at_its_terminator() {
        // The first texdata record points past the blob, the second one names it.
        let mut tex = vec![0u8; TEXDATA_STRIDE * 2];
        tex[0..4].copy_from_slice(&9999u32.to_le_bytes());
        tex[TEXDATA_STRIDE..TEXDATA_STRIDE + 4].copy_from_slice(&12u32.to_le_bytes());
        let mut names = b"aaaaaaaaaaaaaaaaaaaaaa".to_vec();
        names.truncate(12);
        names.extend_from_slice(b"world/wall_concrete\0rest\0");
        assert_eq!(sort_material(1, &tex, &names), "world/wall_concrete");
        assert_eq!(sort_material(0, &tex, &names), "");
        assert_eq!(sort_material(7, &tex, &names), "");
        assert_eq!(sort_material(1, &tex, b"short"), "");
    }
}
