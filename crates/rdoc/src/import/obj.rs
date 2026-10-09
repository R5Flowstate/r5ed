//! Wavefront `.obj` (+ `.mtl`) import: one mesh object per (object, material)
//! pair, each a `.glb` in the project, material names mapped to the Apex
//! material the image-material builder makes for them.

use glam::DVec3;
use indexmap::IndexMap;

use crate::command::Command;
use crate::doc::{new_object, Document};
use crate::model::{CollisionMode, Mesh, Object, ObjectKind};

use super::glb::{self, GlbMesh};
use super::ImportError;

/// A file referring to more vertices than this is refused before allocation.
const MAX_VERTICES: usize = 16_000_000;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ObjOptions {
    /// World units per OBJ unit.
    pub scale: f64,
    /// `y` for the Y-up files most DCCs write; `z` keeps coordinates as they are.
    pub up: String,
    /// Prefix of the Apex material each MTL material maps to:
    /// `<prefix>/<material name, lower case>`.
    pub material_prefix: String,
    /// One mesh per material across the whole file instead of per object.
    pub merge_objects: bool,
    pub collision: CollisionMode,
}

impl Default for ObjOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            up: "y".into(),
            material_prefix: "world/obj".into(),
            merge_objects: false,
            collision: CollisionMode::Triangles,
        }
    }
}

/// One MTL material's maps, as written in the file.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct MtlMaterial {
    pub diffuse: Option<String>,
    pub alpha: Option<String>,
    pub normal: Option<String>,
    pub specular: Option<String>,
    /// `d` below 1 (and above 0) or a `map_d`: the surface is see-through.
    pub dissolve: f64,
    pub color: [f64; 3],
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ObjReport {
    pub vertices: usize,
    pub triangles: usize,
    pub meshes: usize,
    /// Material name -> the Apex material it was mapped to.
    pub materials: IndexMap<String, String>,
    pub bounds: Option<[[f64; 3]; 2]>,
    pub skipped_faces: usize,
}

/// `newmtl` blocks of an `.mtl` file. Map options (`-bm 1`, `-s 1 1 1`) are
/// skipped; the last token of a map line is the image.
pub fn parse_mtl(text: &str) -> IndexMap<String, MtlMaterial> {
    let mut out: IndexMap<String, MtlMaterial> = IndexMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        if key == "newmtl" {
            current = Some(rest.to_string());
            out.insert(rest.to_string(), MtlMaterial { dissolve: 1.0, color: [0.8; 3], ..Default::default() });
            continue;
        }
        let Some(m) = current.as_ref().and_then(|c| out.get_mut(c)) else { continue };
        let image = || rest.rsplit(char::is_whitespace).next().filter(|s| !s.is_empty()).map(str::to_string);
        match key.to_ascii_lowercase().as_str() {
            "map_kd" => m.diffuse = image(),
            "map_d" => m.alpha = image(),
            "map_bump" | "bump" | "norm" | "map_kn" => m.normal = image(),
            "map_ks" => m.specular = image(),
            "d" => m.dissolve = rest.parse().unwrap_or(1.0),
            "tr" => m.dissolve = 1.0 - rest.parse::<f64>().unwrap_or(0.0),
            "kd" => {
                let v: Vec<f64> = rest.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                if v.len() >= 3 {
                    m.color = [v[0], v[1], v[2]];
                }
            }
            _ => {}
        }
    }
    out
}

pub fn material_path(prefix: &str, name: &str) -> String {
    let clean: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    format!("{}/{}", prefix.trim_end_matches('/'), clean)
}

struct Chunk {
    object: String,
    material: String,
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    remap: std::collections::HashMap<(usize, Option<usize>, Option<usize>), u32>,
}

fn index(tok: &str, count: usize) -> Option<usize> {
    let i: i64 = tok.parse().ok()?;
    let r = if i < 0 { count as i64 + i } else { i - 1 };
    (r >= 0 && (r as usize) < count).then_some(r as usize)
}

/// Turns an `.obj` document into one batch of edits and a report.
pub fn import(doc: &Document, text: &str, opts: &ObjOptions) -> Result<(Command, ObjReport), ImportError> {
    if !(opts.scale.is_finite() && opts.scale > 0.0) {
        return Err(ImportError::Unsupported("scale must be positive".into()));
    }
    let layer = doc
        .layer_ids()
        .first()
        .copied()
        .ok_or_else(|| ImportError::Unsupported("document has no layer".into()))?;
    let y_up = opts.up.eq_ignore_ascii_case("y");
    let to_apex = |x: f64, y: f64, z: f64| -> DVec3 {
        let v = if y_up { DVec3::new(x, -z, y) } else { DVec3::new(x, y, z) };
        v * opts.scale
    };

    let (mut v, mut vt, mut vn) = (Vec::<DVec3>::new(), Vec::<[f32; 2]>::new(), Vec::<DVec3>::new());
    let mut chunks: IndexMap<(String, String), Chunk> = IndexMap::new();
    let (mut object, mut material) = (String::from("obj"), String::from("default"));
    let mut report = ObjReport::default();
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));

    for line in text.lines() {
        let line = line.trim();
        let mut it = line.split_whitespace();
        let Some(key) = it.next() else { continue };
        match key {
            "v" => {
                let c: Vec<f64> = it.take(3).filter_map(|t| t.parse().ok()).collect();
                if c.len() == 3 && c.iter().all(|x| x.is_finite()) {
                    let p = to_apex(c[0], c[1], c[2]);
                    lo = lo.min(p);
                    hi = hi.max(p);
                    v.push(p);
                } else {
                    return Err(ImportError::Unsupported(format!("bad vertex line: {line}")));
                }
                if v.len() > MAX_VERTICES {
                    return Err(ImportError::Unsupported("the file has too many vertices".into()));
                }
            }
            "vt" => {
                let c: Vec<f32> = it.take(2).filter_map(|t| t.parse().ok()).collect();
                let (u, w) = (c.first().copied().unwrap_or(0.0), c.get(1).copied().unwrap_or(0.0));
                vt.push([u, 1.0 - w]);
            }
            "vn" => {
                let c: Vec<f64> = it.take(3).filter_map(|t| t.parse().ok()).collect();
                if c.len() == 3 {
                    let n = to_apex(c[0], c[1], c[2]).normalize_or(DVec3::Z);
                    vn.push(n);
                }
            }
            "o" | "g" => object = it.collect::<Vec<_>>().join(" "),
            "usemtl" => material = it.collect::<Vec<_>>().join(" "),
            "f" => {
                let corners: Vec<(usize, Option<usize>, Option<usize>)> = it
                    .filter_map(|c| {
                        let mut p = c.split('/');
                        let vi = index(p.next()?, v.len())?;
                        let ti = p.next().filter(|s| !s.is_empty()).and_then(|s| index(s, vt.len()));
                        let ni = p.next().filter(|s| !s.is_empty()).and_then(|s| index(s, vn.len()));
                        Some((vi, ti, ni))
                    })
                    .collect();
                if corners.len() < 3 {
                    report.skipped_faces += 1;
                    continue;
                }
                let owner = if opts.merge_objects { String::new() } else { object.clone() };
                let chunk = chunks.entry((owner.clone(), material.clone())).or_insert_with(|| Chunk {
                    object: owner,
                    material: material.clone(),
                    positions: Vec::new(),
                    normals: Vec::new(),
                    uvs: Vec::new(),
                    indices: Vec::new(),
                    remap: Default::default(),
                });
                // A face with no normals gets its own flat normal.
                let face_n = (v[corners[1].0] - v[corners[0].0]).cross(v[corners[2].0] - v[corners[0].0]).normalize_or(DVec3::Z);
                let mut ids = Vec::with_capacity(corners.len());
                for &(vi, ti, ni) in &corners {
                    let key = (vi, ti, ni);
                    let id = if let (Some(_), Some(&id)) = (ni, chunk.remap.get(&key)) {
                        id
                    } else {
                        let id = chunk.positions.len() as u32;
                        let p = v[vi];
                        let n = ni.map(|i| vn[i]).unwrap_or(face_n);
                        chunk.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                        chunk.normals.push([n.x as f32, n.y as f32, n.z as f32]);
                        chunk.uvs.push(ti.map(|i| vt[i]).unwrap_or([0.0, 0.0]));
                        if ni.is_some() {
                            chunk.remap.insert(key, id);
                        }
                        id
                    };
                    ids.push(id);
                }
                for k in 1..ids.len() - 1 {
                    chunk.indices.extend_from_slice(&[ids[0], ids[k], ids[k + 1]]);
                    report.triangles += 1;
                }
            }
            _ => {}
        }
    }
    report.vertices = v.len();
    if chunks.is_empty() {
        return Err(ImportError::Unsupported("the file contains no faces".into()));
    }
    if lo.is_finite() {
        report.bounds = Some([lo.to_array(), hi.to_array()]);
    }

    let mut commands = Vec::new();
    let mut objects: Vec<Object> = Vec::new();
    for chunk in chunks.into_values() {
        if chunk.indices.is_empty() {
            continue;
        }
        let apex = material_path(&opts.material_prefix, &chunk.material);
        report.materials.insert(chunk.material.clone(), apex.clone());
        let label = if chunk.object.is_empty() { chunk.material.clone() } else { format!("{}/{}", chunk.object, chunk.material) };
        let mesh = GlbMesh {
            name: label.clone(),
            material: chunk.material.clone(),
            positions: chunk.positions,
            normals: chunk.normals,
            uvs: chunk.uvs,
            colors: Vec::new(),
            indices: chunk.indices,
        };
        let bytes = glb::write(&mesh).map_err(|e| ImportError::Unsupported(format!("{label}: {e}")))?;
        let mut material_map = IndexMap::new();
        material_map.insert(chunk.material, apex);
        let mut object = new_object(
            label,
            layer,
            ObjectKind::Mesh(Mesh {
                source: String::new(),
                material_map,
                collision: opts.collision,
                contents: 1,
                cast_shadows: true,
                lightmap_scale: 0.0,
                render: true,
                role: crate::model::MeshRole::Normal,
                modifiers: Vec::new(),
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
    Ok((Command::Batch { label: "import obj".into(), commands }, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::MemoryFileStore;
    use crate::model::MapDoc;

    const CUBE: &str = "mtllib c.mtl\no Cube\nv -1 -1 -1\nv 1 -1 -1\nv 1 1 -1\nv -1 1 -1\nv -1 -1 1\nv 1 -1 1\nv 1 1 1\nv -1 1 1\n\
vt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nusemtl Stone\nf 1/1 4/4 3/3 2/2\nf 5/1 6/2 7/3 8/4\nf 1/1 2/2 6/3 5/4\nusemtl Wood Plank\n\
f 2/1 3/2 7/3 6/4\nf 3/1 4/2 8/3 7/4\nf 4/1 1/2 5/3 8/4\n";

    fn doc() -> Document {
        Document::new(MapDoc::default(), Box::new(MemoryFileStore::new()))
    }

    #[test]
    fn quads_split_by_material_and_y_up_becomes_z_up() {
        let mut d = doc();
        let opts = ObjOptions { scale: 10.0, ..Default::default() };
        let (cmd, report) = import(&d, CUBE, &opts).unwrap();
        assert_eq!(report.meshes, 2);
        assert_eq!(report.triangles, 12);
        assert_eq!(report.materials["Wood Plank"], "world/obj/wood_plank");
        let [lo, hi] = report.bounds.unwrap();
        assert_eq!((lo, hi), ([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0]));
        d.apply(cmd).unwrap();
        assert_eq!(d.doc.objects.len(), 2);
    }

    #[test]
    fn a_y_up_point_lands_on_apex_z() {
        let (_, report) = import(&doc(), "v 0 5 0\nv 1 5 0\nv 0 5 1\nf 1 2 3\n", &ObjOptions::default()).unwrap();
        let [lo, hi] = report.bounds.unwrap();
        assert_eq!(lo[2], 5.0);
        assert_eq!(hi[2], 5.0);
    }

    #[test]
    fn negative_indices_and_bad_faces() {
        let text = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\nf 1 2\nf 1 2 99\n";
        let (_, report) = import(&doc(), text, &ObjOptions::default()).unwrap();
        assert_eq!(report.triangles, 1);
        assert_eq!(report.skipped_faces, 2);
    }

    #[test]
    fn mtl_maps_take_the_last_token() {
        let m = parse_mtl("newmtl A\nmap_Kd -s 1 1 1 tex/a.png\nmap_d a_alpha.png\nd 0.5\nnewmtl B\nKd 1 0 0\n");
        assert_eq!(m["A"].diffuse.as_deref(), Some("tex/a.png"));
        assert_eq!(m["A"].alpha.as_deref(), Some("a_alpha.png"));
        assert_eq!(m["A"].dissolve, 0.5);
        assert_eq!(m["B"].color, [1.0, 0.0, 0.0]);
    }
}
