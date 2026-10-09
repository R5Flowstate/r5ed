//! A minimal glTF 2.0 binary writer, enough to ship an imported mesh as `.glb`.

use serde_json::{json, Value};

/// One indexed triangle mesh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlbMesh {
    pub name: String,
    pub material: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// RGBA per vertex; empty means no `COLOR_0` attribute.
    pub colors: Vec<[u8; 4]>,
    /// Triangle list.
    pub indices: Vec<u32>,
}

const GLB_MAGIC: u32 = 0x4654_6C67;
const CHUNK_JSON: u32 = 0x4E4F_534A;
const CHUNK_BIN: u32 = 0x004E_4942;
const FLOAT: u32 = 5126;
const UNSIGNED_INT: u32 = 5125;

#[derive(Debug, PartialEq, Eq)]
pub enum GlbError {
    NoPositions,
    /// Normals or UVs were supplied but their count does not match positions.
    MismatchedAttributes,
    /// An index does not address a vertex.
    IndexOutOfRange(u32),
    /// A primitive is not triangles.
    PrimitiveMode(usize, u64),
    /// A position is not finite.
    NotFinite,
    /// More vertices than a u32 index can address.
    TooManyVertices,
}

impl std::fmt::Display for GlbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "glb: ")?;
        match self {
            GlbError::NoPositions => write!(f, "the mesh has no positions"),
            GlbError::MismatchedAttributes => write!(f, "normal or uv count does not match the positions"),
            GlbError::IndexOutOfRange(i) => write!(f, "index {i} has no vertex"),
            GlbError::PrimitiveMode(pi, mode) => write!(f, "primitive {pi} uses mode {mode}; only triangles load"),
            GlbError::NotFinite => write!(f, "a position is not finite"),
            GlbError::TooManyVertices => write!(f, "the mesh has more vertices than an index can address"),
        }
    }
}

impl std::error::Error for GlbError {}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn pad4(out: &mut Vec<u8>, pad: u8) {
    while !out.len().is_multiple_of(4) {
        out.push(pad);
    }
}

fn vec3_json(values: &[[f32; 3]]) -> Value {
    json!(values.iter().map(|v| json!([v[0], v[1], v[2]])).collect::<Vec<_>>())
}

fn accessor(buffer_view: u32, component: u32, count: usize, kind: &str) -> Value {
    json!({ "bufferView": buffer_view, "componentType": component, "count": count, "type": kind })
}

/// Serialises one primitive mesh as a `.glb` container.
pub fn write(mesh: &GlbMesh) -> Result<Vec<u8>, GlbError> {
    if mesh.positions.is_empty() {
        return Err(GlbError::NoPositions);
    }
    if !mesh.normals.is_empty() && mesh.normals.len() != mesh.positions.len() {
        return Err(GlbError::MismatchedAttributes);
    }
    if !mesh.uvs.is_empty() && mesh.uvs.len() != mesh.positions.len() {
        return Err(GlbError::MismatchedAttributes);
    }
    if !mesh.colors.is_empty() && mesh.colors.len() != mesh.positions.len() {
        return Err(GlbError::MismatchedAttributes);
    }
    if mesh.positions.len() > u32::MAX as usize {
        return Err(GlbError::TooManyVertices);
    }
    if mesh.positions.iter().flatten().any(|v| !v.is_finite()) {
        return Err(GlbError::NotFinite);
    }
    for &i in &mesh.indices {
        if i as usize >= mesh.positions.len() {
            return Err(GlbError::IndexOutOfRange(i));
        }
    }

    let count = mesh.positions.len();
    let mut bin: Vec<u8> = Vec::with_capacity(count * 24 + mesh.indices.len() * 4);
    for p in &mesh.positions {
        for c in p {
            push_u32(&mut bin, c.to_bits());
        }
    }
    let position_bytes = bin.len() as u32;
    let normal_view = if mesh.normals.is_empty() { None } else {
        for n in &mesh.normals {
            for c in n {
                push_u32(&mut bin, c.to_bits());
            }
        }
        Some((bin.len() as u32 - position_bytes, bin.len() as u32))
    };
    let uv_view = if mesh.uvs.is_empty() {
        None
    } else {
        let start = bin.len() as u32;
        for uv in &mesh.uvs {
            push_u32(&mut bin, uv[0].to_bits());
            push_u32(&mut bin, uv[1].to_bits());
        }
        Some((start, bin.len() as u32))
    };
    let color_view = if mesh.colors.is_empty() {
        None
    } else {
        let start = bin.len() as u32;
        for c in &mesh.colors {
            bin.extend_from_slice(c);
        }
        Some((start, bin.len() as u32))
    };
    let index_start = bin.len() as u32;
    for i in &mesh.indices {
        push_u32(&mut bin, *i);
    }
    let index_bytes = bin.len() as u32 - index_start;

    let mut views = vec![
        json!({ "buffer": 0, "byteOffset": 0, "byteLength": position_bytes }),
    ];
    let mut attributes = json!({ "POSITION": 0 });
    if let Some((start, end)) = normal_view {
        views.push(json!({ "buffer": 0, "byteOffset": start, "byteLength": end - start }));
        attributes["NORMAL"] = json!(views.len() - 1);
    }
    if let Some((start, end)) = uv_view {
        views.push(json!({ "buffer": 0, "byteOffset": start, "byteLength": end - start }));
        attributes["TEXCOORD_0"] = json!(views.len() - 1);
    }
    let uv_view_idx = uv_view.map(|_| views.len() as u32 - 1);
    let color_view_idx = if let Some((start, end)) = color_view {
        views.push(json!({ "buffer": 0, "byteOffset": start, "byteLength": end - start }));
        attributes["COLOR_0"] = json!(views.len() - 1);
        Some(views.len() as u32 - 1)
    } else {
        None
    };
    if !mesh.indices.is_empty() {
        views.push(json!({ "buffer": 0, "byteOffset": index_start, "byteLength": index_bytes }));
    }

    let mut accessors = vec![accessor(0, FLOAT, count, "VEC3")];
    if normal_view.is_some() {
        accessors.push(accessor(1, FLOAT, count, "VEC3"));
    }
    if let Some(vi) = uv_view_idx {
        accessors.push(accessor(vi, FLOAT, count, "VEC2"));
    }
    if let Some(vi) = color_view_idx {
        accessors.push(json!({ "bufferView": vi, "componentType": 5121u32, "count": count, "type": "VEC4", "normalized": true }));
    }
    if !mesh.indices.is_empty() {
        accessors.push(accessor(views.len() as u32 - 1, UNSIGNED_INT, mesh.indices.len(), "SCALAR"));
    }

    let min = vec3_json(&[fold3(&mesh.positions, true)]);
    let max = vec3_json(&[fold3(&mesh.positions, false)]);
    accessors[0]["min"] = min;
    accessors[0]["max"] = max;

    let document = json!({
        "asset": { "version": "2.0", "generator": "r5ed" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0, "name": mesh.name }],
        "meshes": [{
            "name": mesh.name,
            "primitives": [{
                "attributes": attributes,
                "indices": if mesh.indices.is_empty() { Value::Null } else { json!(accessors.len() - 1) },
                "material": 0
            }]
        }],
        "materials": [{
            "name": mesh.material,
            "pbrMetallicRoughness": { "baseColorFactor": [1.0, 1.0, 1.0, 1.0] }
        }],
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{ "byteLength": bin.len() }]
    });

    let mut json_bytes = serde_json::to_vec(&document).map_err(|_| GlbError::MismatchedAttributes)?;
    pad4(&mut json_bytes, b' ');
    pad4(&mut bin, 0);

    let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    push_u32(&mut out, GLB_MAGIC);
    push_u32(&mut out, 2);
    push_u32(&mut out, total as u32);
    push_u32(&mut out, json_bytes.len() as u32);
    push_u32(&mut out, CHUNK_JSON);
    out.extend_from_slice(&json_bytes);
    push_u32(&mut out, bin.len() as u32);
    push_u32(&mut out, CHUNK_BIN);
    out.extend_from_slice(&bin);
    Ok(out)
}

fn fold3(points: &[[f32; 3]], min: bool) -> [f32; 3] {
    let mut acc = if min { [f32::INFINITY; 3] } else { [f32::NEG_INFINITY; 3] };
    for p in points {
        for i in 0..3 {
            acc[i] = if min { acc[i].min(p[i]) } else { acc[i].max(p[i]) };
        }
    }
    acc
}

/// A node's local matrix: the explicit column-major `matrix`, else TRS.
fn node_local(node: &Value) -> glam::DMat4 {
    if let Some(m) = node.get("matrix").and_then(|v| v.as_array()) {
        if m.len() == 16 {
            let f: Vec<f64> = m.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect();
            return glam::DMat4::from_cols_array(&[
                f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7], f[8], f[9], f[10], f[11], f[12], f[13],
                f[14], f[15],
            ]);
        }
    }
    let num = |key: &str, i: usize, or: f64| {
        node.get(key).and_then(|v| v.as_array()).and_then(|a| a.get(i)).and_then(|x| x.as_f64()).unwrap_or(or)
    };
    glam::DMat4::from_scale_rotation_translation(
        glam::DVec3::new(num("scale", 0, 1.0), num("scale", 1, 1.0), num("scale", 2, 1.0)),
        glam::DQuat::from_xyzw(num("rotation", 0, 0.0), num("rotation", 1, 0.0), num("rotation", 2, 0.0), num("rotation", 3, 1.0)),
        glam::DVec3::new(num("translation", 0, 0.0), num("translation", 1, 0.0), num("translation", 2, 0.0)),
    )
}

/// One `GlbMesh` per triangle primitive, keeping each primitive's material
/// name, so an import can turn material names into slots. Primitives arrive
/// in node-visit order with node transforms baked, the way the compiler
/// reads them; a mesh no node reaches is not rendered and is skipped.
pub fn read_split(bytes: &[u8]) -> Result<Vec<GlbMesh>, GlbError> {
    let u32_at = |o: usize| -> Option<u32> { bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) };
    if u32_at(0) != Some(GLB_MAGIC) {
        return Err(GlbError::NoPositions);
    }
    let mut json: Option<Value> = None;
    let mut bin: &[u8] = &[];
    let mut cursor = 12usize;
    while cursor + 8 <= bytes.len() {
        let len = u32_at(cursor).ok_or(GlbError::NoPositions)? as usize;
        let kind = u32_at(cursor + 4).ok_or(GlbError::NoPositions)?;
        let body = bytes.get(cursor + 8..cursor + 8 + len).ok_or(GlbError::NoPositions)?;
        match kind {
            CHUNK_JSON => json = serde_json::from_slice(body).ok(),
            CHUNK_BIN => bin = body,
            _ => {}
        }
        cursor += 8 + len;
    }
    let json = json.ok_or(GlbError::NoPositions)?;
    let material_name = |i: Option<u64>| -> String {
        i.and_then(|k| json["materials"].get(k as usize))
            .and_then(|m| m["name"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let floats = |b: &[u8]| b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect::<Vec<f32>>();
    let fetch = |idx: &Value| -> Option<(Vec<u8>, usize, u64, usize)> {
        let a = json["accessors"].get(idx.as_u64()? as usize)?;
        let v = json["bufferViews"].get(a["bufferView"].as_u64()? as usize)?;
        let count = a["count"].as_u64()? as usize;
        let comps = match a["type"].as_str()? { "SCALAR" => 1, "VEC2" => 2, "VEC3" => 3, "VEC4" => 4, _ => return None };
        let ct = a["componentType"].as_u64()?;
        let size = match ct { 5121 => 1, 5123 => 2, 5125 | 5126 => 4, _ => return None };
        let stride = v["byteStride"].as_u64().map(|s| s as usize).unwrap_or(size * comps);
        let start = v["byteOffset"].as_u64().unwrap_or(0) as usize + a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let mut out = Vec::with_capacity(count.min(1 << 24) * size * comps);
        for k in 0..count {
            let o = start.checked_add(k.checked_mul(stride)?)?;
            out.extend_from_slice(bin.get(o..o + size * comps)?);
        }
        Some((out, comps, ct, count))
    };
    let mut prims: Vec<GlbMesh> = Vec::new();
    let mut emit = |mesh_index: usize, world: glam::DMat4| -> Result<(), GlbError> {
        let m = json["meshes"].get(mesh_index).ok_or(GlbError::NoPositions)?;
        let normal_world = glam::DMat3::from_mat4(world.inverse()).transpose();
        let bake_p = |p: [f32; 3]| -> [f32; 3] {
            let v = world.transform_point3(glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64));
            [v.x as f32, v.y as f32, v.z as f32]
        };
        let bake_n = |n: [f32; 3]| -> [f32; 3] {
            let v = normal_world * glam::DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64);
            let v = v.normalize_or_zero();
            [v.x as f32, v.y as f32, v.z as f32]
        };
        let mesh_name = m["name"].as_str().unwrap_or("").to_string();
        for (pi, prim) in m["primitives"].as_array().into_iter().flatten().enumerate() {
            if prim["mode"].as_u64().unwrap_or(4) != 4 {
                return Err(GlbError::PrimitiveMode(pi, prim["mode"].as_u64().unwrap_or(4)));
            }
            let material = material_name(prim["material"].as_u64());
            let (p, pc, _, count) = fetch(&prim["attributes"]["POSITION"]).ok_or(GlbError::NoPositions)?;
            let mut mesh = GlbMesh { name: mesh_name.clone(), material, ..GlbMesh::default() };
            if pc != 3 {
                return Err(GlbError::NoPositions);
            }
            mesh.positions.extend(floats(&p).chunks_exact(3).map(|c| bake_p([c[0], c[1], c[2]])));
            match fetch(&prim["attributes"]["NORMAL"]) {
                Some((n, 3, 5126, _)) => {
                    mesh.normals.extend(floats(&n).chunks_exact(3).map(|c| bake_n([c[0], c[1], c[2]])))
                }
                _ => mesh.normals.extend(std::iter::repeat_n([0.0, 0.0, 1.0], count)),
            }
            match fetch(&prim["attributes"]["TEXCOORD_0"]) {
                Some((t, 2, 5126, _)) => mesh.uvs.extend(floats(&t).chunks_exact(2).map(|c| [c[0], c[1]])),
                _ => mesh.uvs.extend(std::iter::repeat_n([0.0, 0.0], count)),
            }
            match fetch(&prim["attributes"]["COLOR_0"]) {
                Some((c, 4, 5121, n)) if c.len() == n * 4 => {
                    mesh.colors.extend(c.chunks_exact(4).map(|b| [b[0], b[1], b[2], b[3]]))
                }
                Some((c, 4, 5126, n)) if c.len() == n * 16 => mesh.colors.extend(floats(&c).chunks_exact(4).map(|f| {
                    [f[0], f[1], f[2], f[3]].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                })),
                _ => {}
            }
            match fetch(&prim["indices"]) {
                Some((ix, 1, ct, _)) => {
                    let idx: Vec<u32> = match ct {
                        5121 => ix.iter().map(|b| *b as u32).collect(),
                        5123 => ix.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as u32).collect(),
                        _ => ix.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
                    };
                    for i in idx {
                        if i as usize >= count {
                            return Err(GlbError::IndexOutOfRange(i));
                        }
                        mesh.indices.push(i);
                    }
                }
                _ => mesh.indices.extend(0..count as u32),
            }
            if !mesh.positions.is_empty() {
                prims.push(mesh);
            }
        }
        Ok(())
    };
    let node_count = json["nodes"].as_array().map(|n| n.len()).unwrap_or(0);
    if node_count == 0 {
        for mi in 0..json["meshes"].as_array().map(|m| m.len()).unwrap_or(0) {
            emit(mi, glam::DMat4::IDENTITY)?;
        }
    } else {
        let roots: Vec<usize> = match json
            .get("scene")
            .and_then(|s| s.as_u64())
            .and_then(|s| json["scenes"].get(s as usize))
            .and_then(|s| s.get("nodes"))
            .and_then(|n| n.as_array())
        {
            Some(ns) => ns.iter().filter_map(|n| n.as_u64()).map(|n| n as usize).collect(),
            None => (0..node_count).collect(),
        };
        let mut visited = vec![false; node_count];
        let mut stack: Vec<(usize, glam::DMat4, usize)> =
            roots.into_iter().filter(|r| *r < node_count).map(|r| (r, glam::DMat4::IDENTITY, 0)).collect();
        while let Some((ni, world, depth)) = stack.pop() {
            if visited[ni] || depth > 64 {
                continue;
            }
            visited[ni] = true;
            let node = &json["nodes"][ni];
            let world = world * node_local(node);
            if let Some(mi) = node.get("mesh").and_then(|v| v.as_u64()).map(|v| v as usize) {
                emit(mi, world)?;
            }
            if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
                for c in children.iter().filter_map(|v| v.as_u64()).map(|v| v as usize).filter(|c| *c < node_count) {
                    stack.push((c, world, depth + 1));
                }
            }
        }
    }
    if prims.is_empty() {
        return Err(GlbError::NoPositions);
    }
    Ok(prims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> GlbMesh {
        GlbMesh {
            name: "disp".into(),
            material: "nodraw".into(),
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [2.0, 0.0, 0.0],
            ],
            normals: vec![[0.0; 3]; 5],
            uvs: vec![[0.0, 0.0]; 5],
            colors: vec![],
            indices: vec![0, 1, 2, 0, 2, 3],
        }
    }

    #[test]
    fn container_header_is_well_formed() {
        let bytes = write(&quad()).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), GLB_MAGIC);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize, bytes.len());
        assert_eq!(bytes.len() % 4, 0);
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), CHUNK_JSON);
        let doc: Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
        assert_eq!(doc["asset"]["version"], "2.0");
        assert_eq!(doc["meshes"][0]["primitives"][0]["indices"], json!(3));
        assert_eq!(doc["accessors"][0]["min"], json!([[0.0, 0.0, 0.0]]));
        assert_eq!(doc["accessors"][0]["max"], json!([[2.0, 1.0, 0.0]]));
        assert_eq!(doc["buffers"][0]["byteLength"].as_u64().unwrap() as usize, bytes.len() - 20 - json_len - 8);
    }

    #[test]
    fn positions_round_trip_through_the_binary_chunk() {
        let mesh = quad();
        let bytes = write(&mesh).unwrap();
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let bin_start = 20 + json_len + 8;
        for (i, p) in mesh.positions.iter().enumerate() {
            let off = bin_start + i * 12;
            let v: Vec<f32> = (0..3)
                .map(|c| f32::from_le_bytes(bytes[off + c * 4..off + c * 4 + 4].try_into().unwrap()))
                .collect();
            assert_eq!(v, p.to_vec());
        }
    }

    #[test]
    fn bad_meshes_are_rejected() {
        let mut m = quad();
        m.positions.clear();
        assert_eq!(write(&m), Err(GlbError::NoPositions));
        m = quad();
        m.normals.pop();
        assert_eq!(write(&m), Err(GlbError::MismatchedAttributes));
        m = quad();
        m.indices = vec![0, 1, 99];
        assert_eq!(write(&m), Err(GlbError::IndexOutOfRange(99)));
        m = quad();
        m.positions[0][0] = f32::NAN;
        assert_eq!(write(&m), Err(GlbError::NotFinite));
    }

    #[test]
    fn a_mesh_without_normals_or_uvs_still_writes() {
        let mesh = GlbMesh {
            name: "bare".into(),
            material: String::new(),
            positions: vec![[0.0; 3]; 3],
            normals: vec![],
            uvs: vec![],
            colors: vec![],
            indices: vec![0, 1, 2],
        };
        let bytes = write(&mesh).unwrap();
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let doc: Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
        let attributes = &doc["meshes"][0]["primitives"][0]["attributes"];
        assert_eq!(attributes.as_object().unwrap().len(), 1);
    }

    fn with_patch(bytes: &[u8], patch: impl FnOnce(&mut Value)) -> Vec<u8> {
        let u32at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) as usize;
        let json_len = u32at(12);
        let mut doc: Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
        patch(&mut doc);
        let mut body = serde_json::to_vec(&doc).unwrap();
        while body.len() % 4 != 0 {
            body.push(b' ');
        }
        let bin_len = u32at(20 + json_len);
        let bin = &bytes[20 + json_len + 8..20 + json_len + 8 + bin_len];
        let mut out = Vec::new();
        out.extend_from_slice(&bytes[0..8]);
        out.extend_from_slice(&((12 + 8 + body.len() + 8 + bin.len()) as u32).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&CHUNK_JSON.to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&CHUNK_BIN.to_le_bytes());
        out.extend_from_slice(bin);
        out
    }

    fn with_node(bytes: &[u8], key: &str, value: Value) -> Vec<u8> {
        with_patch(bytes, |doc| {
            doc["nodes"][0][key] = value;
        })
    }

    #[test]
    fn split_reader_bakes_node_transforms() {
        let plain = read_split(&write(&quad()).unwrap()).unwrap();
        assert_eq!(plain[0].positions, quad().positions, "identity nodes read back exactly");
        for (key, value) in [
            ("translation", json!([8.0, -4.0, 2.0])),
            ("matrix", json!([1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 8.0, -4.0, 2.0, 1.0])),
        ] {
            let moved = with_node(&write(&quad()).unwrap(), key, value);
            let prims = read_split(&moved).unwrap();
            assert_eq!(prims.len(), 1, "{key} keeps the primitive");
            assert_eq!(prims[0].positions[0], [8.0, -4.0, 2.0], "{key} moves the origin");
            assert_eq!(prims[0].positions[4], [10.0, -4.0, 2.0], "{key} moves every vertex");
            assert_eq!(prims[0].indices, quad().indices, "{key} keeps the index run");
        }
    }

    #[test]
    fn split_reader_refuses_non_triangle_modes() {
        let lines = with_patch(&write(&quad()).unwrap(), |doc| {
            doc["meshes"][0]["primitives"][0]["mode"] = json!(1);
        });
        assert_eq!(read_split(&lines), Err(GlbError::PrimitiveMode(0, 1)));
    }
}

/// Reads every triangle primitive of a `.glb` into one mesh (node transforms
/// are not applied). Handles float attributes and u8/u16/u32 indices.
pub fn read(bytes: &[u8]) -> Result<GlbMesh, GlbError> {
    let u32_at = |o: usize| -> Option<u32> { bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) };
    if u32_at(0) != Some(GLB_MAGIC) {
        return Err(GlbError::NoPositions);
    }
    let mut json: Option<Value> = None;
    let mut bin: &[u8] = &[];
    let mut at = 12usize;
    while at + 8 <= bytes.len() {
        let len = u32_at(at).ok_or(GlbError::NoPositions)? as usize;
        let kind = u32_at(at + 4).ok_or(GlbError::NoPositions)?;
        let body = bytes.get(at + 8..at + 8 + len).ok_or(GlbError::NoPositions)?;
        match kind {
            CHUNK_JSON => json = serde_json::from_slice(body).ok(),
            CHUNK_BIN => bin = body,
            _ => {}
        }
        at += 8 + len;
    }
    let json = json.ok_or(GlbError::NoPositions)?;
    let accessor = |i: &Value| -> Option<(Vec<u8>, usize, u64, usize)> {
        let a = json["accessors"].get(i.as_u64()? as usize)?;
        let v = json["bufferViews"].get(a["bufferView"].as_u64()? as usize)?;
        let count = a["count"].as_u64()? as usize;
        let comps = match a["type"].as_str()? { "SCALAR" => 1, "VEC2" => 2, "VEC3" => 3, "VEC4" => 4, _ => return None };
        let ct = a["componentType"].as_u64()?;
        let size = match ct { 5121 => 1, 5123 => 2, 5125 | 5126 => 4, _ => return None };
        let stride = v["byteStride"].as_u64().map(|s| s as usize).unwrap_or(size * comps);
        let start = v["byteOffset"].as_u64().unwrap_or(0) as usize + a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let mut out = Vec::with_capacity(count.min(1 << 24) * size * comps);
        for k in 0..count {
            let o = start.checked_add(k.checked_mul(stride)?)?;
            out.extend_from_slice(bin.get(o..o + size * comps)?);
        }
        Some((out, comps, ct, count))
    };
    let floats = |b: &[u8]| b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect::<Vec<f32>>();
    let mut mesh = GlbMesh::default();
    for m in json["meshes"].as_array().into_iter().flatten() {
        mesh.name = m["name"].as_str().unwrap_or("").to_string();
        for prim in m["primitives"].as_array().into_iter().flatten() {
            if prim["mode"].as_u64().unwrap_or(4) != 4 {
                continue;
            }
            let base = mesh.positions.len() as u32;
            let (p, pc, _, count) = accessor(&prim["attributes"]["POSITION"]).ok_or(GlbError::NoPositions)?;
            if pc != 3 {
                return Err(GlbError::NoPositions);
            }
            let p = floats(&p);
            mesh.positions.extend(p.chunks_exact(3).map(|c| [c[0], c[1], c[2]]));
            match accessor(&prim["attributes"]["NORMAL"]) {
                Some((n, 3, 5126, _)) => mesh.normals.extend(floats(&n).chunks_exact(3).map(|c| [c[0], c[1], c[2]])),
                _ => mesh.normals.extend(std::iter::repeat_n([0.0, 0.0, 1.0], count)),
            }
            match accessor(&prim["attributes"]["TEXCOORD_0"]) {
                Some((t, 2, 5126, _)) => mesh.uvs.extend(floats(&t).chunks_exact(2).map(|c| [c[0], c[1]])),
                _ => mesh.uvs.extend(std::iter::repeat_n([0.0, 0.0], count)),
            }
            let had_color = mesh.colors.len();
            match accessor(&prim["attributes"]["COLOR_0"]) {
                Some((c, 4, 5121, n)) if c.len() == n * 4 => {
                    mesh.colors.extend(c.chunks_exact(4).map(|b| [b[0], b[1], b[2], b[3]]))
                }
                Some((c, 4, 5126, n)) if c.len() == n * 16 => {
                    mesh.colors.extend(floats(&c).chunks_exact(4).map(|f| {
                        [f[0].clamp(0.0, 1.0) * 255.0, f[1].clamp(0.0, 1.0) * 255.0,
                         f[2].clamp(0.0, 1.0) * 255.0, f[3].clamp(0.0, 1.0) * 255.0]
                            .map(|v| v.round() as u8)
                    }))
                }
                _ => {}
            }
            if mesh.colors.len() == had_color {
                mesh.colors.extend(std::iter::repeat_n([255u8; 4], count));
            }
            match accessor(&prim["indices"]) {
                Some((ix, 1, ct, _)) => {
                    let idx: Vec<u32> = match ct {
                        5121 => ix.iter().map(|b| *b as u32).collect(),
                        5123 => ix.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as u32).collect(),
                        _ => ix.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
                    };
                    for i in idx {
                        if i as usize >= count {
                            return Err(GlbError::IndexOutOfRange(i));
                        }
                        mesh.indices.push(base + i);
                    }
                }
                _ => mesh.indices.extend(base..base + count as u32),
            }
        }
    }
    if mesh.positions.is_empty() {
        return Err(GlbError::NoPositions);
    }
    Ok(mesh)
}
