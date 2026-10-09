//! A minimal glTF-binary writer: one buffer, one mesh, indexed triangles,
//! no extensions, no compression.
//!
//! Every attribute is interleaved into a single buffer view set so the whole
//! document is one `JSON` chunk and one `BIN` chunk.

use crate::error::{Error, Result};
use crate::rmdl::RmdlModel;

const GLB_MAGIC: u32 = 0x4654_6C67; // "glTF"
const CHUNK_JSON: u32 = 0x4E4F_534A; // "JSON"
const CHUNK_BIN: u32 = 0x004E_4942; // "BIN\0"

const COMPONENT_UNSIGNED_SHORT: u32 = 5123;
const COMPONENT_UNSIGNED_BYTE: u32 = 5121;
const TARGET_ARRAY_BUFFER: u32 = 34962;
const TARGET_ELEMENT_ARRAY_BUFFER: u32 = 34963;
const MODE_TRIANGLES: u32 = 4;
const COMPONENT_FLOAT: u32 = 5126;

fn align4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

struct Buffer {
    bytes: Vec<u8>,
}

impl Buffer {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Appends `data` 4-byte aligned and returns (offset, length).
    fn push(&mut self, data: &[u8]) -> (usize, usize) {
        while self.bytes.len() % 4 != 0 {
            self.bytes.push(0);
        }
        let off = self.bytes.len();
        self.bytes.extend_from_slice(data);
        (off, data.len())
    }
}

fn min_max(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for p in positions {
        for k in 0..3 {
            if p[k].is_finite() {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    if !lo[0].is_finite() {
        return ([0.0; 3], [0.0; 3]);
    }
    (lo, hi)
}

fn json_f(v: &[f32]) -> String {
    let body: Vec<String> = v.iter().map(|x| format!("{x:?}")).collect();
    format!("[{}]", body.join(","))
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Appends one 4-byte aligned buffer view and returns its index.
fn push_view(buf: &mut Buffer, views: &mut Vec<String>, data: &[u8], target: u32) -> usize {
    let (off, len) = buf.push(data);
    views.push(format!(r#"{{"buffer":0,"byteOffset":{off},"byteLength":{len},"target":{target}}}"#));
    views.len() - 1
}

/// Converts a parsed model to a single-mesh glTF-binary, LOD0 only.
pub fn model_to_glb(model: &RmdlModel) -> Result<Vec<u8>> {
    model_to_glb_named(model, &|_| None)
}

/// As [`model_to_glb`], naming each glTF material by what `name_of` returns
/// for its slot's guid, and `slot_<n>` when it returns nothing.
pub fn model_to_glb_named(model: &RmdlModel, name_of: &dyn Fn(u64) -> Option<String>) -> Result<Vec<u8>> {
    let prims: Vec<&crate::rmdl::Mesh> =
        model.meshes.iter().filter(|m| !m.indices.is_empty() && !m.positions.is_empty()).collect();
    if prims.is_empty() {
        return Err(Error::Malformed { what: "model", msg: "no LOD0 geometry to convert".into() });
    }
    let mut buf = Buffer::new();
    let mut accessors: Vec<String> = Vec::new();
    let mut views: Vec<String> = Vec::new();
    let mut gltf_primitives: Vec<String> = Vec::new();
    let mut materials: Vec<String> = Vec::new();

    for (pi, m) in prims.iter().enumerate() {
        let vcount = m.positions.len();
        let flat_pos: Vec<u8> =
            m.positions.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect();
        let flat_nrm: Vec<u8> =
            m.normals.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect();
        let flat_uv: Vec<u8> = m.uv0.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect();
        let flat_col: Vec<u8> = m.colors.iter().flat_map(|v| v.iter().copied()).collect();
        let flat_idx: Vec<u8> = m.indices.iter().flat_map(|i| i.to_le_bytes()).collect();

        let pv = push_view(&mut buf, &mut views, &flat_pos, TARGET_ARRAY_BUFFER);
        let (lo, hi) = min_max(&m.positions);
        let pos_acc = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{pv},"componentType":{COMPONENT_FLOAT},"count":{vcount},"type":"VEC3","min":{},"max":{}}}"#,
            json_f(&lo),
            json_f(&hi)
        ));
        let nv = push_view(&mut buf, &mut views, &flat_nrm, TARGET_ARRAY_BUFFER);
        let nrm_acc = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{nv},"componentType":{COMPONENT_FLOAT},"count":{vcount},"type":"VEC3"}}"#
        ));
        let tv = push_view(&mut buf, &mut views, &flat_uv, TARGET_ARRAY_BUFFER);
        let uv_acc = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{tv},"componentType":{COMPONENT_FLOAT},"count":{vcount},"type":"VEC2"}}"#
        ));
        let cv = push_view(&mut buf, &mut views, &flat_col, TARGET_ARRAY_BUFFER);
        let col_acc = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{cv},"componentType":{COMPONENT_UNSIGNED_BYTE},"count":{vcount},"type":"VEC4","normalized":true}}"#
        ));
        let iv = push_view(&mut buf, &mut views, &flat_idx, TARGET_ELEMENT_ARRAY_BUFFER);
        let idx_acc = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{iv},"componentType":{COMPONENT_UNSIGNED_SHORT},"count":{},"type":"SCALAR"}}"#,
            m.indices.len()
        ));
        gltf_primitives.push(format!(
            r#"{{"attributes":{{"POSITION":{pos_acc},"NORMAL":{nrm_acc},"TEXCOORD_0":{uv_acc},"COLOR_0":{col_acc}}},"indices":{idx_acc},"material":{pi},"mode":{MODE_TRIANGLES}}}"#
        ));
        let tex = model.textures.get(m.material as usize).copied().unwrap_or(0);
        materials.push(format!(
            r#"{{"name":{name},"pbrMetallicRoughness":{{"baseColorFactor":[1.0,1.0,1.0,1.0]}},"doubleSided":true,"extras":{{"textureGuid":"0x{tex:016X}","materialSlot":{slot}}}}}"#,
            name = json_str(&name_of(tex).unwrap_or_else(|| format!("slot_{}", m.material))),
            slot = m.material
        ));
    }

    let json = format!(
        r#"{{"asset":{{"version":"2.0","generator":"rpak"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"name":{name}}}],"meshes":[{{"name":{name},"primitives":[{prims}]}}],"materials":[{materials}],"accessors":[{accessors}],"bufferViews":[{views}],"buffers":[{{"byteLength":{binlen}}}]}}"#,
        name = json_str(&model.name),
        prims = gltf_primitives.join(","),
        materials = materials.join(","),
        accessors = accessors.join(","),
        views = views.join(","),
        binlen = buf.bytes.len(),
    );
    assemble(json.into_bytes(), buf.bytes)
}

/// One primitive of a mesh chunk: an indexed triangle list with a name.
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkPrimitive {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl ChunkPrimitive {
    /// An empty primitive, for a mesh that decoded to nothing.
    pub fn empty(name: impl Into<String>) -> Self {
        Self { name: name.into(), positions: Vec::new(), normals: Vec::new(), uvs: Vec::new(), indices: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty() || self.indices.len() < 3
    }
}

/// A mesh chunk: a set of primitives under one node, in the order the map
/// stored them. One chunk is one glb file, which is what the reference layer
/// streams to the page.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshChunk {
    pub name: String,
    pub primitives: Vec<ChunkPrimitive>,
}

impl MeshChunk {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), primitives: Vec::new() }
    }

    pub fn push(&mut self, p: ChunkPrimitive) {
        if !p.is_empty() {
            self.primitives.push(p);
        }
    }

    pub fn triangles(&self) -> usize {
        self.primitives.iter().map(|p| p.indices.len() / 3).sum()
    }

    pub fn vertices(&self) -> usize {
        self.primitives.iter().map(|p| p.positions.len()).sum()
    }
}

/// The axis-aligned extent of a chunk, or the origin for an empty one.
pub fn chunk_bounds(c: &MeshChunk) -> ([f32; 3], [f32; 3]) {
    let mut v = Vec::new();
    for p in &c.primitives {
        v.extend_from_slice(&p.positions);
    }
    min_max(&v)
}

/// Writes a mesh chunk as a glTF-binary: one node, one mesh, one primitive per
/// entry, positions and normals always and UVs when they are there.
pub fn chunk_to_glb(c: &MeshChunk) -> Result<Vec<u8>> {
    if c.primitives.is_empty() {
        return Err(Error::Malformed { what: "glb", msg: "a chunk with no geometry".into() });
    }
    let mut buf = Buffer::new();
    let mut accessors: Vec<String> = Vec::new();
    let mut views: Vec<String> = Vec::new();
    let mut gltf_primitives: Vec<String> = Vec::new();
    let mut materials: Vec<String> = Vec::new();

    for p in &c.primitives {
        let n = p.positions.len();
        let flat_pos: Vec<u8> = p.positions.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect();
        let pv = push_view(&mut buf, &mut views, &flat_pos, TARGET_ARRAY_BUFFER);
        let (lo, hi) = min_max(&p.positions);
        let pos_acc = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{pv},"componentType":{COMPONENT_FLOAT},"count":{n},"type":"VEC3","min":{},"max":{}}}"#,
            json_f(&lo),
            json_f(&hi)
        ));
        // A primitive whose normals are missing still has to carry something, so
        // the straight-up vector is written once per position.
        let nrm: Vec<[f32; 3]> =
            if p.normals.len() == n { p.normals.clone() } else { vec![[0.0, 0.0, 1.0]; n] };
        let flat_nrm: Vec<u8> = nrm.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect();
        let nv = push_view(&mut buf, &mut views, &flat_nrm, TARGET_ARRAY_BUFFER);
        let nrm_acc = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{nv},"componentType":{COMPONENT_FLOAT},"count":{n},"type":"VEC3"}}"#
        ));
        let mut attrs = format!(r#""POSITION":{pos_acc},"NORMAL":{nrm_acc}"#);
        if p.uvs.len() == n {
            let flat_uv: Vec<u8> = p.uvs.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect();
            let tv = push_view(&mut buf, &mut views, &flat_uv, TARGET_ARRAY_BUFFER);
            let uv_acc = accessors.len();
            accessors.push(format!(
                r#"{{"bufferView":{tv},"componentType":{COMPONENT_FLOAT},"count":{n},"type":"VEC2"}}"#
            ));
            attrs.push_str(&format!(r#","TEXCOORD_0":{uv_acc}"#));
        }
        // Index components widen at 65536 vertices, which one mesh crosses.
        let (ctype, flat_idx): (u32, Vec<u8>) = if n <= u16::MAX as usize {
            (COMPONENT_UNSIGNED_SHORT, p.indices.iter().flat_map(|i| (*i as u16).to_le_bytes()).collect())
        } else {
            (COMPONENT_UNSIGNED_INT, p.indices.iter().flat_map(|i| i.to_le_bytes()).collect())
        };
        let iv = push_view(&mut buf, &mut views, &flat_idx, TARGET_ELEMENT_ARRAY_BUFFER);
        let idx_acc = accessors.len();
        accessors.push(format!(
            r#"{{"bufferView":{iv},"componentType":{ctype},"count":{},"type":"SCALAR"}}"#,
            p.indices.len()
        ));
        gltf_primitives.push(format!(
            r#"{{"attributes":{{{attrs}}},"indices":{idx_acc},"material":{},"mode":{MODE_TRIANGLES}}}"#,
            materials.len()
        ));
        materials.push(format!(
            r#"{{"name":{name},"pbrMetallicRoughness":{{"baseColorFactor":[1.0,1.0,1.0,1.0]}},"doubleSided":true}}"#,
            name = json_str(&p.name)
        ));
    }

    let json = format!(
        r#"{{"asset":{{"version":"2.0","generator":"r5ed"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"name":{name}}}],"meshes":[{{"name":{name},"primitives":[{prims}]}}],"materials":[{materials}],"accessors":[{accessors}],"bufferViews":[{views}],"buffers":[{{"byteLength":{binlen}}}]}}"#,
        name = json_str(&c.name),
        prims = gltf_primitives.join(","),
        materials = materials.join(","),
        accessors = accessors.join(","),
        views = views.join(","),
        binlen = buf.bytes.len(),
    );
    assemble(json.into_bytes(), buf.bytes)
}

const COMPONENT_UNSIGNED_INT: u32 = 5125;

fn assemble(mut json_bytes: Vec<u8>, mut bin_bytes: Vec<u8>) -> Result<Vec<u8>> {
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' ');
    }
    while bin_bytes.len() % 4 != 0 {
        bin_bytes.push(0);
    }
    let total = 12 + 8 + json_bytes.len() + 8 + bin_bytes.len();
    if total > u32::MAX as usize {
        return Err(Error::Malformed { what: "glb", msg: "document exceeds 4 GiB".into() });
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&GLB_MAGIC.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&CHUNK_JSON.to_le_bytes());
    out.extend_from_slice(&json_bytes);
    out.extend_from_slice(&(bin_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&CHUNK_BIN.to_le_bytes());
    out.extend_from_slice(&bin_bytes);
    Ok(out)
}

/// Minimal reader for a glb, used to prove the layout this module writes.
#[derive(Debug, PartialEq)]
pub struct GlbSummary {
    pub json: serde_json::Value,
    pub bin_len: usize,
}

pub fn read_glb(b: &[u8]) -> Result<GlbSummary> {
    if b.len() < 12 {
        return Err(Error::Truncated { what: "glb header", need: 12, have: b.len() });
    }
    let rd = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    if rd(0) != GLB_MAGIC {
        return Err(Error::Malformed { what: "glb", msg: "bad magic".into() });
    }
    if rd(4) != 2 {
        return Err(Error::Malformed { what: "glb", msg: format!("version {} is not 2", rd(4)) });
    }
    let total = rd(8) as usize;
    if total > b.len() {
        return Err(Error::Truncated { what: "glb", need: total, have: b.len() });
    }
    let jlen = rd(12) as usize;
    if rd(16) != CHUNK_JSON || 20usize.checked_add(jlen).is_none_or(|e| e > b.len()) {
        return Err(Error::Malformed { what: "glb", msg: "first chunk is not JSON".into() });
    }
    let json: serde_json::Value =
        serde_json::from_slice(&b[20..20 + jlen]).map_err(|e| Error::Json(e.to_string()))?;
    let bo = align4(20 + jlen);
    if bo + 8 > b.len() {
        return Err(Error::Truncated { what: "glb bin header", need: bo + 8, have: b.len() });
    }
    let blen = u32::from_le_bytes([b[bo], b[bo + 1], b[bo + 2], b[bo + 3]]) as usize;
    if bo + 8 + blen > b.len() {
        return Err(Error::Truncated { what: "glb bin", need: bo + 8 + blen, have: b.len() });
    }
    Ok(GlbSummary { json, bin_len: blen })
}
