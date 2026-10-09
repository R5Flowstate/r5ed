//! glTF 2.0 binary reader: positions, normals, uv0, indices, material names
//! and the node transforms a `.glb` object is placed with.

use glam::{DMat4, DQuat, DVec3};
use serde::Deserialize;
use thiserror::Error;

const MAGIC: u32 = 0x4654_6C67;
const CHUNK_JSON: u32 = 0x4E4F_534A;
const CHUNK_BIN: u32 = 0x004E_4942;

const F32: u32 = 5126;
const U8: u32 = 5121;
const U16: u32 = 5123;
const U32: u32 = 5125;

const MODE_TRIANGLES: i64 = 4;

#[derive(Debug, Error, PartialEq)]
pub enum GlbError {
    #[error("glb is too short to hold a header")]
    Short,
    #[error("glb magic is 0x{0:08x}, not a glTF binary container")]
    BadMagic(u32),
    #[error("glb version {0} is not 2")]
    BadVersion(u32),
    #[error("glb chunk {index} runs past the end of the file")]
    ChunkPast { index: u32 },
    #[error("glb json chunk is missing or not first")]
    NoJson,
    #[error("glb json is not valid: {0}")]
    Json(String),
    #[error("glb accessor {0} has no buffer view")]
    AccessorWithoutView(u32),
    #[error("glb accessor {0} names component type {1}, which a vertex stream cannot use")]
    AccessorComponent(u32, u32),
    #[error("glb accessor {0} is a {1}, which a vertex stream cannot use")]
    AccessorType(u32, String),
    #[error("glb accessor {0} reads past its buffer view")]
    AccessorRange(u32),
    #[error("glb buffer view {0} names buffer {1}, which does not exist")]
    ViewBuffer(u32, u32),
    #[error("glb buffer {0} has no data of the declared length")]
    BufferRange(u32),
    #[error("glb node {0} names a mesh that does not exist")]
    NodeMesh(u32),
    #[error("glb mesh {0} has no primitive with positions")]
    EmptyMesh(u32),
    #[error("glb primitive {0} is mode {1}; only triangles are read")]
    PrimitiveMode(usize, i64),
    #[error("glb mesh has more than {0} triangles")]
    TooManyTriangles(u32),
    #[error("glb mesh is not finite")]
    NotFinite,
}

/// One primitive of a glb, already in the mesh's own space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlbPrimitive {
    pub material: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uv0: Vec<[f32; 2]>,
    /// RGBA per vertex; white when the primitive has no `COLOR_0`.
    pub colors: Vec<[u8; 4]>,
    /// Triangle list; every index addresses this primitive's own arrays.
    pub indices: Vec<u32>,
}

/// One mesh plus the local transform its node carries.
#[derive(Clone, Debug, PartialEq)]
pub struct GlbNode {
    pub name: String,
    pub transform: DMat4,
    pub primitives: Vec<GlbPrimitive>,
}

#[derive(Deserialize)]
struct Gltf {
    #[serde(default)]
    buffers: Vec<Buffer>,
    #[serde(default)]
    #[serde(rename = "bufferViews")]
    buffer_views: Vec<BufferView>,
    #[serde(default)]
    accessors: Vec<Accessor>,
    #[serde(default)]
    meshes: Vec<Mesh>,
    #[serde(default)]
    materials: Vec<Material>,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    scenes: Vec<Scene>,
    #[serde(default)]
    scene: Option<usize>,
}

#[derive(Deserialize)]
struct Buffer {
    #[serde(default)]
    #[serde(rename = "byteLength")]
    byte_length: u32,
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Deserialize)]
struct BufferView {
    #[serde(default)]
    buffer: u32,
    #[serde(default)]
    #[serde(rename = "byteOffset")]
    byte_offset: u32,
    #[serde(default)]
    #[serde(rename = "byteLength")]
    byte_length: u32,
}

#[derive(Deserialize)]
struct Accessor {
    #[serde(default)]
    #[serde(rename = "bufferView")]
    buffer_view: Option<u32>,
    #[serde(default)]
    #[serde(rename = "byteOffset")]
    byte_offset: u32,
    #[serde(default)]
    #[serde(rename = "componentType")]
    component_type: u32,
    #[serde(default)]
    count: u32,
    #[serde(rename = "type")]
    type_name: String,
}

#[derive(Deserialize)]
struct Mesh {
    #[serde(default)]
    name: String,
    #[serde(default)]
    primitives: Vec<Primitive>,
}

#[derive(Deserialize)]
struct Primitive {
    #[serde(default)]
    attributes: Attributes,
    #[serde(default)]
    indices: Option<u32>,
    #[serde(default)]
    material: Option<u32>,
    #[serde(default)]
    mode: Option<i64>,
}

#[derive(Deserialize, Default)]
struct Attributes {
    #[serde(default, rename = "POSITION")]
    position: Option<u32>,
    #[serde(default, rename = "NORMAL")]
    normal: Option<u32>,
    #[serde(default, rename = "TEXCOORD_0")]
    texcoord0: Option<u32>,
    #[serde(default, rename = "COLOR_0")]
    color0: Option<u32>,
}

#[derive(Deserialize, Default)]
struct Material {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Default)]
struct Node {
    #[serde(default)]
    name: String,
    #[serde(default)]
    matrix: Option<[f32; 16]>,
    #[serde(default)]
    translation: Option<[f32; 3]>,
    #[serde(default)]
    rotation: Option<[f32; 4]>,
    #[serde(default)]
    scale: Option<[f32; 3]>,
    #[serde(default)]
    mesh: Option<u32>,
    #[serde(default)]
    children: Vec<u32>,
}

#[derive(Deserialize, Default)]
struct Scene {
    #[serde(default)]
    nodes: Vec<u32>,
}

/// Largest triangle count one glb may contribute to a surface.
pub const MAX_TRIANGLES: u32 = 1 << 22;

/// Reads every mesh of a glb, each with the world transform its node chain
/// gives it. Nodes without a mesh contribute only their transform.
pub fn read_glb(bytes: &[u8]) -> Result<Vec<GlbNode>, GlbError> {
    let (gltf, bin) = split(bytes)?;
    let json: Gltf = serde_json::from_slice(gltf).map_err(|e| GlbError::Json(e.to_string()))?;
    // Only the buffer that rides in the binary chunk carries vertex data; a
    // declared external buffer is not read, so its view bounds are rejected.
    let mut buffers: Vec<&[u8]> = Vec::with_capacity(json.buffers.len());
    for b in &json.buffers {
        buffers.push(if b.uri.is_none() && b.byte_length as usize <= bin.len() { bin } else { &[] });
    }
    let roots: Vec<u32> = match json.scene.and_then(|s| json.scenes.get(s)) {
        Some(s) => s.nodes.clone(),
        None => (0..json.nodes.len() as u32).collect(),
    };
    let mut out: Vec<GlbNode> = Vec::new();
    let mut seen: Vec<u32> = Vec::new();
    for r in roots {
        walk(&json, &buffers, r, DMat4::IDENTITY, &mut out, &mut seen)?;
    }
    Ok(out)
}

fn walk(
    gltf: &Gltf,
    buffers: &[&[u8]],
    node: u32,
    parent: DMat4,
    out: &mut Vec<GlbNode>,
    seen: &mut Vec<u32>,
) -> Result<(), GlbError> {
    if seen.contains(&node) {
        return Err(GlbError::NodeMesh(node));
    }
    seen.push(node);
    let n = gltf.nodes.get(node as usize).ok_or(GlbError::NodeMesh(node))?;
    let local = node_matrix(n);
    let world = parent * local;
    if let Some(mi) = n.mesh {
        let mesh = gltf.meshes.get(mi as usize).ok_or(GlbError::NodeMesh(mi))?;
        if mesh.primitives.is_empty() {
            return Err(GlbError::EmptyMesh(mi));
        }
        let mut prims = Vec::with_capacity(mesh.primitives.len());
        for (pi, p) in mesh.primitives.iter().enumerate() {
            if let Some(mode) = p.mode {
                if mode != MODE_TRIANGLES {
                    return Err(GlbError::PrimitiveMode(pi, mode));
                }
            }
            prims.push(read_primitive(gltf, buffers, p, material_name(gltf, p))?);
        }
        let name = if mesh.name.is_empty() { n.name.clone() } else { mesh.name.clone() };
        out.push(GlbNode { name, transform: world, primitives: prims });
    }
    for &c in &n.children {
        walk(gltf, buffers, c, world, out, seen)?;
    }
    Ok(())
}

fn node_matrix(n: &Node) -> DMat4 {
    if let Some(m) = n.matrix {
        return DMat4::from_cols_array(&[
            m[0] as f64, m[1] as f64, m[2] as f64, m[3] as f64, m[4] as f64, m[5] as f64, m[6] as f64,
            m[7] as f64, m[8] as f64, m[9] as f64, m[10] as f64, m[11] as f64, m[12] as f64, m[13] as f64,
            m[14] as f64, m[15] as f64,
        ]);
    }
    let t = n.translation.unwrap_or([0.0; 3]);
    let r = n.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let s = n.scale.unwrap_or([1.0; 3]);
    DMat4::from_scale_rotation_translation(
        DVec3::new(s[0] as f64, s[1] as f64, s[2] as f64),
        DQuat::from_xyzw(r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64).normalize(),
        DVec3::new(t[0] as f64, t[1] as f64, t[2] as f64),
    )
}

fn material_name(gltf: &Gltf, p: &Primitive) -> String {
    p.material
        .and_then(|m| gltf.materials.get(m as usize))
        .map(|m| m.name.clone())
        .unwrap_or_default()
}

fn read_primitive(
    gltf: &Gltf,
    buffers: &[&[u8]],
    p: &Primitive,
    material: String,
) -> Result<GlbPrimitive, GlbError> {
    let pos_idx = p.attributes.position.ok_or(GlbError::EmptyMesh(0))?;
    let positions = read_vec3(gltf, buffers, pos_idx)?;
    if positions.len() > MAX_TRIANGLES as usize * 3 {
        return Err(GlbError::TooManyTriangles(MAX_TRIANGLES));
    }
    if positions.iter().any(|v| !v.iter().all(|c| c.is_finite())) {
        return Err(GlbError::NotFinite);
    }
    let normals = match p.attributes.normal {
        Some(a) => read_vec3(gltf, buffers, a)?,
        None => vec![[0.0; 3]; positions.len()],
    };
    let uv0 = match p.attributes.texcoord0 {
        Some(a) => read_vec2(gltf, buffers, a)?,
        None => vec![[0.0; 2]; positions.len()],
    };
    let colors = match p.attributes.color0 {
        Some(a) => read_colors(gltf, buffers, a)?,
        None => vec![[255; 4]; positions.len()],
    };
    let mut indices = match p.indices {
        Some(a) => read_indices(gltf, buffers, a)?,
        None => (0..positions.len() as u32).collect(),
    };
    if indices.len() % 3 != 0 {
        indices.truncate(indices.len() - indices.len() % 3);
    }
    for &i in &indices {
        if i as usize >= positions.len() {
            return Err(GlbError::AccessorRange(pos_idx));
        }
    }
    let colors = if colors.len() == positions.len() { colors } else { vec![[255; 4]; positions.len()] };
    Ok(GlbPrimitive { material, positions, normals, uv0, colors, indices })
}

fn view_bytes<'a>(
    gltf: &Gltf,
    buffers: &[&'a [u8]],
    view: u32,
) -> Result<&'a [u8], GlbError> {
    let v = gltf.buffer_views.get(view as usize).ok_or(GlbError::ViewBuffer(view, 0))?;
    let data = buffers.get(v.buffer as usize).copied().ok_or(GlbError::ViewBuffer(view, v.buffer))?;
    let start = v.byte_offset as usize;
    let end = start.checked_add(v.byte_length as usize).ok_or(GlbError::BufferRange(v.buffer))?;
    data.get(start..end).ok_or(GlbError::BufferRange(v.buffer))
}

fn accessor_slice<'a>(
    gltf: &Gltf,
    buffers: &[&'a [u8]],
    acc: u32,
    want_type: &str,
    want_components: usize,
) -> Result<(&'a [u8], usize, u32), GlbError> {
    let a = gltf.accessors.get(acc as usize).ok_or(GlbError::AccessorRange(acc))?;
    if a.type_name != want_type {
        return Err(GlbError::AccessorType(acc, a.type_name.clone()));
    }
    let view = a.buffer_view.ok_or(GlbError::AccessorWithoutView(acc))?;
    let bytes = view_bytes(gltf, buffers, view)?;
    let comp = match a.component_type {
        F32 => 4,
        U8 => 1,
        U16 => 2,
        U32 => 4,
        other => return Err(GlbError::AccessorComponent(acc, other)),
    };
    let stride = comp * want_components;
    let need = a.byte_offset as usize
        + stride.checked_mul(a.count as usize).ok_or(GlbError::AccessorRange(acc))?;
    if need > bytes.len() {
        return Err(GlbError::AccessorRange(acc));
    }
    Ok((&bytes[a.byte_offset as usize..], stride, a.component_type))
}

fn read_vec3(
    gltf: &Gltf,
    buffers: &[&[u8]],
    acc: u32,
) -> Result<Vec<[f32; 3]>, GlbError> {
    let (bytes, stride, comp) = accessor_slice(gltf, buffers, acc, "VEC3", 3)?;
    let count = gltf.accessors[acc as usize].count as usize;
    if comp != F32 {
        return Err(GlbError::AccessorComponent(acc, comp));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = i * stride;
        let v = bytes.get(at..at + 12).ok_or(GlbError::AccessorRange(acc))?;
        out.push([
            f32::from_le_bytes([v[0], v[1], v[2], v[3]]),
            f32::from_le_bytes([v[4], v[5], v[6], v[7]]),
            f32::from_le_bytes([v[8], v[9], v[10], v[11]]),
        ]);
    }
    Ok(out)
}

fn read_vec2(
    gltf: &Gltf,
    buffers: &[&[u8]],
    acc: u32,
) -> Result<Vec<[f32; 2]>, GlbError> {
    let (bytes, stride, comp) = accessor_slice(gltf, buffers, acc, "VEC2", 2)?;
    let count = gltf.accessors[acc as usize].count as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = i * stride;
        // `stride` is the pair's size in bytes; `comp` is the component type
        // code (5126 for a float), not a size.
        let raw = bytes.get(at..at + stride).ok_or(GlbError::AccessorRange(acc))?;
        out.push([component(raw, 0, comp)?, component(raw, 1, comp)?]);
    }
    Ok(out)
}

/// `COLOR_0` as RGBA8: normalized u8/u16 or float, VEC4 or VEC3 (opaque).
fn read_colors(gltf: &Gltf, buffers: &[&[u8]], acc: u32) -> Result<Vec<[u8; 4]>, GlbError> {
    let a = gltf.accessors.get(acc as usize).ok_or(GlbError::AccessorRange(acc))?;
    let n = if a.type_name == "VEC3" { 3 } else { 4 };
    let (bytes, stride, comp) = accessor_slice(gltf, buffers, acc, if n == 3 { "VEC3" } else { "VEC4" }, n)?;
    let scale = match comp {
        U8 => 1.0,
        U16 => 255.0 / 65535.0,
        _ => 255.0,
    };
    let count = a.count as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let raw = bytes.get(i * stride..i * stride + stride).ok_or(GlbError::AccessorRange(acc))?;
        let mut c = [255u8; 4];
        for (k, slot) in c.iter_mut().enumerate().take(n) {
            *slot = (component(raw, k, comp)? * scale).round().clamp(0.0, 255.0) as u8;
        }
        out.push(c);
    }
    Ok(out)
}

fn read_indices(
    gltf: &Gltf,
    buffers: &[&[u8]],
    acc: u32,
) -> Result<Vec<u32>, GlbError> {
    let (bytes, stride, comp) = accessor_slice(gltf, buffers, acc, "SCALAR", 1)?;
    let count = gltf.accessors[acc as usize].count as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = i * stride;
        let raw = bytes.get(at..at + component_size(comp)).ok_or(GlbError::AccessorRange(acc))?;
        out.push(component(raw, 0, comp)? as u32);
    }
    Ok(out)
}

/// Bytes one component of a type code takes.
fn component_size(comp: u32) -> usize {
    match comp {
        U8 => 1,
        U16 => 2,
        _ => 4,
    }
}

fn component(raw: &[u8], i: usize, comp: u32) -> Result<f32, GlbError> {
    let at = i * component_size(comp);
    let raw = raw.get(at..).ok_or(GlbError::AccessorRange(0))?;
    Ok(match comp {
        F32 => {
            let b: [u8; 4] = raw.get(..4).and_then(|s| s.try_into().ok()).ok_or(GlbError::AccessorRange(0))?;
            f32::from_le_bytes(b)
        }
        U8 => *raw.first().ok_or(GlbError::AccessorRange(0))? as f32,
        U16 => {
            let b: [u8; 2] = raw.get(..2).and_then(|s| s.try_into().ok()).ok_or(GlbError::AccessorRange(0))?;
            u16::from_le_bytes(b) as f32
        }
        U32 => {
            let b: [u8; 4] = raw.get(..4).and_then(|s| s.try_into().ok()).ok_or(GlbError::AccessorRange(0))?;
            u32::from_le_bytes(b) as f32
        }
        other => return Err(GlbError::AccessorComponent(0, other)),
    })
}

fn split(bytes: &[u8]) -> Result<(&[u8], &[u8]), GlbError> {
    if bytes.len() < 12 {
        return Err(GlbError::Short);
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if magic != MAGIC {
        return Err(GlbError::BadMagic(magic));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != 2 {
        return Err(GlbError::BadVersion(version));
    }
    let mut at = 12usize;
    let mut json: Option<&[u8]> = None;
    let mut bin: &[u8] = &[];
    let mut index = 0u32;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap());
        let start = at + 8;
        let end = start.checked_add(len).ok_or(GlbError::ChunkPast { index })?;
        let data = bytes.get(start..end).ok_or(GlbError::ChunkPast { index })?;
        match kind {
            CHUNK_JSON if json.is_none() => json = Some(data),
            CHUNK_BIN => bin = data,
            _ => {}
        }
        at = end;
        index += 1;
    }
    let json = json.ok_or(GlbError::NoJson)?;
    Ok((json, bin))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A glb the decompiler writes reads back whole: uv0 included, which the
    /// component type code used to size.
    #[test]
    fn a_decompiled_mesh_reads_back_with_its_uvs() {
        let mesh = rdoc::import::glb::GlbMesh {
            name: "m".into(),
            material: "world/dev/dev_gray_512".into(),
            positions: vec![[0.0, 0.0, 0.0], [64.0, 0.0, 0.0], [0.0, 64.0, 0.0], [64.0, 64.0, 8.0]],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            colors: Vec::new(),
            indices: vec![0, 1, 2, 2, 1, 3],
        };
        let bytes = rdoc::import::glb::write(&mesh).expect("the mesh writes");
        let nodes = read_glb(&bytes).expect("the mesh reads back");
        let prim = &nodes[0].primitives[0];
        assert_eq!(prim.positions, mesh.positions);
        assert_eq!(prim.uv0, mesh.uvs);
        assert_eq!(prim.indices, mesh.indices);
    }
}
