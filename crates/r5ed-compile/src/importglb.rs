//! `import-glb`: a glTF 2.0 `.glb` becomes mesh objects, one per (node,
//! primitive), each a `.glb` in the project with the Apex material the
//! image-material builder makes for that glTF material.
//!
//! Reading is the compiler's own glb reader (`rcomp::csg::glb`), which walks
//! the node transforms a DCC export and bounds every accessor against its
//! buffer view, so nothing here re-checks what that already refuses.

use anyhow::{bail, Result};
use glam::{DMat3, DMat4, DVec3};
use indexmap::IndexMap;
use rdoc::command::Command;
use rdoc::doc::{new_object, Document};
use rdoc::import::glb::GlbMesh;
use rdoc::import::obj::{material_path, ObjOptions};
use rdoc::model::{Mesh, Object, ObjectKind};

/// The material name a glb primitive carries none of.
pub const DEFAULT_MATERIAL: &str = "default";

/// What one import produced.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Report {
    pub nodes: usize,
    pub meshes: usize,
    pub triangles: usize,
    pub vertices: usize,
    /// glTF material name -> the Apex material it maps to.
    pub materials: IndexMap<String, String>,
    pub bounds: Option<[[f64; 3]; 2]>,
    /// Primitives with nothing in them: no positions, or no indices.
    pub skipped_primitives: usize,
}

struct Chunk {
    label: String,
    material: String,
    apex: String,
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[u8; 4]>,
    indices: Vec<u32>,
}

/// The Apex Z-up a Y-up glb is read as, then the unit scale every point takes.
fn to_apex(opts: &ObjOptions) -> DMat4 {
    let up = if opts.up.eq_ignore_ascii_case("y") {
        DMat4::from_cols_array(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0])
    } else {
        DMat4::IDENTITY
    };
    up * DMat4::from_scale(DVec3::splat(opts.scale))
}

/// Turns a glb into one batch of edits and a report.
pub fn import(doc: &Document, bytes: &[u8], opts: &ObjOptions) -> Result<(Command, Report)> {
    if !(opts.scale.is_finite() && opts.scale > 0.0) {
        bail!("scale must be positive");
    }
    let layer = doc.layer_ids().first().copied().ok_or_else(|| anyhow::anyhow!("the document has no layer"))?;
    let nodes = rcomp::csg::glb::read_glb(bytes).map_err(|e| anyhow::anyhow!("{e}"))?;
    let world_of = |transform: DMat4| to_apex(opts) * transform;

    let mut report = Report::default();
    let mut chunks: IndexMap<(String, String), Chunk> = IndexMap::new();
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for node in nodes {
        report.nodes += 1;
        let world = world_of(node.transform);
        let to_normal = DMat3::from_mat4(world).inverse().transpose();
        for prim in node.primitives {
            if prim.positions.is_empty() || prim.indices.is_empty() {
                report.skipped_primitives += 1;
                continue;
            }
            let material = if prim.material.is_empty() { DEFAULT_MATERIAL.to_string() } else { prim.material.clone() };
            let apex = material_path(&opts.material_prefix, &material);
            report.materials.entry(material.clone()).or_insert_with(|| apex.clone());
            report.triangles += prim.indices.len() / 3;
            let owner = if opts.merge_objects { String::new() } else { node.name.clone() };
            let chunk = chunks.entry((owner, material.clone())).or_insert_with(|| Chunk {
                label: if node.name.is_empty() { material.clone() } else { format!("{}/{}", node.name, material) },
                material,
                apex,
                positions: Vec::new(),
                normals: Vec::new(),
                uvs: Vec::new(),
                colors: Vec::new(),
                indices: Vec::new(),
            });
            let base = chunk.positions.len() as u32;
            for p in &prim.positions {
                let v = world.transform_point3(p.map(|c| c as f64).into());
                lo = lo.min(v);
                hi = hi.max(v);
                chunk.positions.push([v.x as f32, v.y as f32, v.z as f32]);
            }
            for n in &prim.normals {
                let v = to_normal * DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64);
                chunk.normals.push(v.normalize_or(DVec3::Z).to_array().map(|c| c as f32));
            }
            // glTF's UV origin is the image's top left, the engine's too (the
            // decompiler and the rpak exporter write r5 UVs into .glb as they
            // are), so they pass through. Flipping V here turned every imported
            // texture upside down: text read inverted, and a mesh mapping half
            // of its texture showed the other half.
            chunk.uvs.extend(prim.uv0.iter().copied());
            chunk.colors.extend(prim.colors.iter().copied());
            for i in &prim.indices {
                chunk.indices.push(base + i);
            }
        }
    }
    report.vertices = chunks.values().map(|c| c.positions.len()).sum();
    if lo.is_finite() {
        report.bounds = Some([lo.to_array(), hi.to_array()]);
    }
    if chunks.is_empty() {
        bail!("the file holds no triangle mesh");
    }

    let mut commands = Vec::new();
    let mut objects: Vec<Object> = Vec::new();
    for chunk in chunks.into_values() {
        let mesh = GlbMesh {
            name: chunk.label.clone(),
            material: chunk.material.clone(),
            positions: chunk.positions,
            normals: chunk.normals,
            uvs: chunk.uvs,
            colors: chunk.colors,
            indices: chunk.indices,
        };
        let bytes = rdoc::import::glb::write(&mesh).map_err(|e| anyhow::anyhow!("{}: {e}", chunk.label))?;
        let mut material_map = IndexMap::new();
        material_map.insert(chunk.material, chunk.apex);
        let mut object = new_object(
            chunk.label,
            layer,
            ObjectKind::Mesh(Mesh { modifiers: Vec::new(), role: Default::default(),
                source: String::new(),
                material_map,
                collision: opts.collision,
                contents: 1,
                cast_shadows: true,
                lightmap_scale: 0.0,
                render: true,
            }),
        );
        let path = format!("meshes/{}.glb", object.id);
        if let ObjectKind::Mesh(m) = &mut object.kind {
            m.source = path.clone();
        }
        commands.push(Command::SetFile { path, bytes: Some(bytes) });
        objects.push(object);
    }
    report.meshes = objects.len();
    commands.push(Command::AddObjects { objects });
    Ok((Command::Batch { label: "import glb".into(), commands }, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    use rdoc::doc::MemoryFileStore;
    use rdoc::model::MapDoc;

    /// One node of the test file: its name, the materials it carries and where
    /// it stands.
    type Node<'a> = (&'a str, &'a [&'a str], [f32; 3]);

    fn doc() -> Document {
        Document::new(MapDoc::default(), Box::new(MemoryFileStore::new()))
    }

    /// A glb the test writes itself: one mesh per node holding a triangle for
    /// each of that node's materials, every node sharing one index buffer.
    fn glb(nodes: &[Node<'_>]) -> Vec<u8> {
        let mut names: Vec<&str> = Vec::new();
        for (_, mats, _) in nodes {
            for m in *mats {
                if !names.contains(m) {
                    names.push(m);
                }
            }
        }
        let mut views = Vec::new();
        let mut accessors = Vec::new();
        let mut meshes = Vec::new();
        let mut bin: Vec<u8> = Vec::new();
        for i in 0..3u32 {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        views.push(json!({ "buffer": 0, "byteOffset": 0, "byteLength": 12 }));
        accessors.push(json!({ "bufferView": 0, "componentType": 5125, "count": 3, "type": "SCALAR" }));
        for (name, mats, _) in nodes {
            let mut prims = Vec::new();
            for m in *mats {
                let at = bin.len() as u32;
                for c in [0.0f32, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0] {
                    bin.extend_from_slice(&c.to_le_bytes());
                }
                views.push(json!({ "buffer": 0, "byteOffset": at, "byteLength": 36 }));
                accessors.push(json!({ "bufferView": views.len() - 1, "componentType": 5126, "count": 3, "type": "VEC3" }));
                prims.push(json!({
                    "attributes": { "POSITION": accessors.len() - 1 },
                    "indices": 0,
                    "material": names.iter().position(|n| n == m).unwrap()
                }));
            }
            meshes.push(json!({ "name": name, "primitives": prims }));
        }
        pack(
            json!({
                "asset": { "version": "2.0" },
                "scene": 0,
                "scenes": [{ "nodes": (0..nodes.len()).collect::<Vec<_>>() }],
                "nodes": nodes
                    .iter()
                    .enumerate()
                    .map(|(i, (name, _, t))| json!({ "name": name, "mesh": i, "translation": t }))
                    .collect::<Vec<_>>(),
                "meshes": meshes,
                "materials": names.iter().map(|n| json!({ "name": n })).collect::<Vec<_>>(),
                "accessors": accessors,
                "bufferViews": views,
                "buffers": [{ "byteLength": bin.len() }]
            }),
            bin,
        )
    }

    /// Wraps a gltf json and a binary chunk in the container.
    fn pack(doc: Value, mut bin: Vec<u8>) -> Vec<u8> {
        let mut json_bytes = serde_json::to_vec(&doc).unwrap();
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut out = Vec::new();
        for v in [0x4654_6C67u32, 2, total as u32, json_bytes.len() as u32, 0x4E4F_534Au32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&json_bytes);
        for v in [bin.len() as u32, 0x004E_4942u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&bin);
        out
    }

    #[test]
    fn a_y_up_triangle_lands_on_apex_z_and_takes_the_scale() {
        let opts = ObjOptions { scale: 10.0, ..Default::default() };
        let (_, report) = import(&doc(), &glb(&[("cube", &["Stone"], [0.0; 3])]), &opts).unwrap();
        assert_eq!(report.materials["Stone"], "world/obj/stone");
        assert_eq!((report.nodes, report.meshes, report.triangles, report.vertices), (1, 1, 1, 3));
        let [lo, hi] = report.bounds.unwrap();
        // (0,1,0) is up in a y-up file, so it lands on Apex z.
        assert_eq!((lo, hi), ([0.0, -10.0, 0.0], [0.0, 0.0, 10.0]));
    }

    #[test]
    fn z_up_leaves_the_coordinates_alone() {
        let opts = ObjOptions { scale: 1.0, up: "z".into(), ..Default::default() };
        let (_, report) = import(&doc(), &glb(&[("cube", &["Stone"], [0.0; 3])]), &opts).unwrap();
        let [lo, hi] = report.bounds.unwrap();
        assert_eq!((lo, hi), ([0.0, 0.0, 0.0], [0.0, 1.0, 1.0]));
    }

    #[test]
    fn a_node_transform_reaches_the_vertices_and_the_label() {
        let bytes = glb(&[("Rail", &["Wood Plank"], [0.0, 5.0, 0.0])]);
        let mut d = doc();
        let (cmd, report) = import(&d, &bytes, &ObjOptions::default()).unwrap();
        assert_eq!(report.nodes, 1);
        let [lo, hi] = report.bounds.unwrap();
        assert_eq!((lo, hi), ([0.0, -1.0, 5.0], [0.0, 0.0, 6.0]));
        d.apply(cmd).unwrap();
        assert_eq!(d.doc.objects[0].name, "Rail/Wood Plank");
    }

    #[test]
    fn each_node_keeps_its_own_object_unless_objects_are_merged() {
        let bytes = glb(&[("a", &["Stone"], [0.0; 3]), ("b", &["Stone", "Wood Plank"], [2.0, 0.0, 0.0])]);
        let (_, split) = import(&doc(), &bytes, &ObjOptions::default()).unwrap();
        assert_eq!((split.nodes, split.meshes, split.triangles, split.materials.len()), (2, 3, 3, 2));
        let opts = ObjOptions { merge_objects: true, ..Default::default() };
        let (_, merged) = import(&doc(), &bytes, &opts).unwrap();
        assert_eq!((merged.nodes, merged.meshes, merged.vertices, merged.triangles), (2, 2, 9, 3));
    }

    #[test]
    fn a_material_no_primitive_names_still_maps_somewhere() {
        let mut d = doc();
        let bytes = glb(&[("cube", &["Stone"], [0.0; 3])]);
        let (cmd, _) = import(&d, &bytes, &ObjOptions::default()).unwrap();
        d.apply(cmd).unwrap();
        let source = match &d.doc.objects[0].kind {
            ObjectKind::Mesh(m) => m.source.clone(),
            other => panic!("{other:?} is not a mesh"),
        };
        assert!(source.starts_with("meshes/"));
        assert_eq!(rdoc::import::glb::read(&d.files.read(&source).unwrap()).unwrap().indices.len(), 3);
    }

    #[test]
    fn a_file_that_is_not_a_glb_is_named_and_refused() {
        let err = import(&doc(), b"not a glb at all", &ObjOptions::default()).unwrap_err();
        assert!(err.to_string().contains("glTF binary container"), "{err}");
        let bad = ObjOptions { scale: 0.0, ..Default::default() };
        assert!(import(&doc(), &glb(&[("a", &["Stone"], [0.0; 3])]), &bad).is_err());
    }

    #[test]
    fn uvs_pass_through_as_gltf_writes_them() {
        let mut bin = Vec::new();
        for i in 0..3u32 {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        for c in [0.0f32, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0] {
            bin.extend_from_slice(&c.to_le_bytes());
        }
        for c in [0.25f32, 0.75, 0.5, 0.125, 1.0, 0.875] {
            bin.extend_from_slice(&c.to_le_bytes());
        }
        let bytes = pack(
            json!({
                "asset": { "version": "2.0" },
                "scene": 0,
                "scenes": [{ "nodes": [0] }],
                "nodes": [{ "name": "quad", "mesh": 0 }],
                "materials": [{ "name": "Stone" }],
                "meshes": [{ "name": "quad", "primitives": [
                    { "attributes": { "POSITION": 1, "TEXCOORD_0": 2 }, "indices": 0, "material": 0 }
                ] }],
                "accessors": [
                    { "bufferView": 0, "componentType": 5125, "count": 3, "type": "SCALAR" },
                    { "bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3" },
                    { "bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2" }
                ],
                "bufferViews": [
                    { "buffer": 0, "byteOffset": 0, "byteLength": 12 },
                    { "buffer": 0, "byteOffset": 12, "byteLength": 36 },
                    { "buffer": 0, "byteOffset": 48, "byteLength": 24 }
                ],
                "buffers": [{ "byteLength": bin.len() }]
            }),
            bin,
        );
        let mut d = doc();
        let (cmd, _) = import(&d, &bytes, &ObjOptions::default()).unwrap();
        d.apply(cmd).unwrap();
        let source = match &d.doc.objects[0].kind {
            ObjectKind::Mesh(m) => m.source.clone(),
            other => panic!("{other:?} is not a mesh"),
        };
        let mesh = rdoc::import::glb::read(&d.files.read(&source).unwrap()).unwrap();
        assert_eq!(mesh.uvs, vec![[0.25, 0.75], [0.5, 0.125], [1.0, 0.875]], "no V flip");
    }

    #[test]
    fn a_primitive_without_a_material_maps_to_default() {
        let mut bin = Vec::new();
        for i in 0..3u32 {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        for c in [0.0f32, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0] {
            bin.extend_from_slice(&c.to_le_bytes());
        }
        let bytes = pack(
            json!({
                "asset": { "version": "2.0" },
                "scene": 0,
                "scenes": [{ "nodes": [0] }],
                "nodes": [{ "name": "cube", "mesh": 0 }],
                "meshes": [{ "name": "cube", "primitives": [
                    { "attributes": { "POSITION": 1 }, "indices": 0 }
                ] }],
                "accessors": [
                    { "bufferView": 0, "componentType": 5125, "count": 3, "type": "SCALAR" },
                    { "bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3" }
                ],
                "bufferViews": [
                    { "buffer": 0, "byteOffset": 0, "byteLength": 12 },
                    { "buffer": 0, "byteOffset": 12, "byteLength": 36 }
                ],
                "buffers": [{ "byteLength": bin.len() }]
            }),
            bin,
        );
        let (_, report) = import(&doc(), &bytes, &ObjOptions::default()).unwrap();
        assert_eq!(report.materials[DEFAULT_MATERIAL], "world/obj/default");
        assert_eq!(report.triangles, 1);
    }

    #[test]
    fn a_container_with_no_triangles_is_refused() {
        let bytes = pack(json!({ "asset": { "version": "2.0" }, "scenes": [], "nodes": [] }), Vec::new());
        let err = import(&doc(), &bytes, &ObjOptions::default()).unwrap_err();
        assert!(err.to_string().contains("no triangle mesh"), "{err}");
    }
}