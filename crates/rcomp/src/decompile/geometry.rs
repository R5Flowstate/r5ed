//! World render geometry read back as `Mesh` objects, one per model and
//! material, with the geometry itself written as a glb beside the document.

use std::collections::{BTreeMap, HashMap, HashSet};

use glam::DVec3;
use indexmap::IndexMap;
use rbsp::lumps::collision::CHILD_HULL;
use rbsp::{ids, Bsp};
use rdoc::import::glb;
use rdoc::model::{CollisionMode, Id, Mesh, Object, ObjectKind, RefRecord};

use super::{decode, hidden_object, DecompileError};
use crate::collision;
use crate::meshes;
use crate::splice::tables;

/// Longest material slug a mesh file name carries; the ordinal keeps it unique.
const SLUG_LEN: usize = 48;

/// The meshes of lump 0x50, grouped by the model and material they draw with,
/// and the collision no render mesh stands for.
///
/// Every render group becomes one `Mesh` object and one glb under `meshes/`.
/// Its collision is the leaves of its model whose surface is its material,
/// with the mask most of them carry; a material with no such leaves draws and
/// does not collide. Every other live polygon and hull leaf of a model, by
/// mask and surface (a clip, a skybox wall, a surface's leaves with a second
/// mask), becomes a collision-only `Mesh` under `collision/`.
pub fn world_meshes(
    bsp: &Bsp,
    layer: Id,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<Vec<(Object, RefRecord)>, DecompileError> {
    let mut out: Vec<(Object, RefRecord)> = Vec::new();
    let mut covered: HashSet<(u32, String, u32)> = HashSet::new();
    let mut masks: BTreeMap<u32, HashMap<(String, u32), usize>> = BTreeMap::new();
    // A dedicated-server map may keep a mesh table of its own and none of the
    // vertex lumps, so there is no render geometry to write out.
    let render = !(bsp.lump(ids::MESH_BOUNDS).is_empty() || bsp.lump(ids::VERTICES).is_empty());
    let runs = if render { meshes::decode_meshes(bsp).map_err(|e| decode("world", e))? } else { Vec::new() };
    let groups = group_runs(&runs);
    for (ordinal, group) in groups.iter().enumerate() {
        let (model, material, parts) = group;
        let (positions, normals, uvs, indices) = join(parts, &runs);
        let material = material.to_string();
        let slug = slug(&material);
        let name = format!("mesh{ordinal:04}_{slug}");
        let mesh = glb::GlbMesh {
            name: name.clone(),
            material: material.clone(),
            positions,
            normals,
            uvs,
            colors: Vec::new(),
            indices,
        };
        let bytes = glb::write(&mesh).map_err(|e| DecompileError::Glb { name: name.clone(), message: e.to_string() })?;
        let path = format!("meshes/{ordinal:04}_{slug}.glb");
        files.insert(path.clone(), bytes);
        let mut map = IndexMap::new();
        map.insert(material.clone(), material.clone());
        let model_masks = masks.entry(*model).or_insert_with(|| tables::surface_masks(bsp, *model as usize));
        let contents = tables::dominant_mask(model_masks, &material);
        if let Some(c) = contents {
            covered.insert((*model, tables::material_key(&material), c));
        }
        let kind = ObjectKind::Mesh(Mesh {
            source: path,
            material_map: map,
            collision: if contents.is_some() { CollisionMode::Triangles } else { CollisionMode::None },
            contents: contents.unwrap_or(0),
            cast_shadows: true,
            lightmap_scale: 0.0,
            render: true,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        });
        out.push((
            hidden_object(name, layer, Default::default(), kind),
            RefRecord::Mesh { model: *model, material },
        ));
    }
    out.extend(collision_only(bsp, layer, files, &covered)?);
    Ok(out)
}

/// The collision groups no render mesh stands for, one object per model,
/// mask and surface.
fn collision_only(
    bsp: &Bsp,
    layer: Id,
    files: &mut BTreeMap<String, Vec<u8>>,
    covered: &HashSet<(u32, String, u32)>,
) -> Result<Vec<(Object, RefRecord)>, DecompileError> {
    let v = bsp.version_kind().unwrap_or(rbsp::Version::ClientV51);
    let models = rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), v).unwrap_or_default();
    let props = rbsp::lumps::collision::parse_surface_props(bsp.lump(ids::SURFACE_PROPERTIES), v).unwrap_or_default();
    let names = rbsp::lumps::render::parse_surface_names(bsp.lump(ids::SURFACE_NAMES), v).unwrap_or_default();
    let mut groups: BTreeMap<(u32, u32, String), Vec<[DVec3; 3]>> = BTreeMap::new();
    for (mi, m) in models.iter().enumerate() {
        if !collision::model_has_collision(m) {
            continue;
        }
        let Ok(leaves) = collision::decode_leaves(bsp, mi) else { continue };
        for l in leaves {
            let poly = (rbsp::lumps::collision::CHILD_POLY..=collision::CHILD_POLY_MAX).contains(&l.kind);
            if l.contents == 0 || l.triangles.is_empty() || !(poly || l.kind == CHILD_HULL) {
                continue;
            }
            let surface = props
                .get(l.surfprop as usize)
                .and_then(|p| names.at_offset(p.name as usize))
                .unwrap_or_default()
                .to_string();
            if covered.contains(&(mi as u32, tables::material_key(&surface), l.contents)) {
                continue;
            }
            groups.entry((mi as u32, l.contents, surface)).or_default().extend(l.triangles);
        }
    }
    let mut out = Vec::with_capacity(groups.len());
    for (ordinal, ((model, contents, surface), tris)) in groups.into_iter().enumerate() {
        let name = format!("collision{ordinal:04}_{}", slug(&surface));
        let mut positions: Vec<[f32; 3]> = Vec::with_capacity(tris.len() * 3);
        for t in &tris {
            positions.extend(t.iter().map(|p| p.as_vec3().to_array()));
        }
        let indices: Vec<u32> = (0..positions.len() as u32).collect();
        let mesh = glb::GlbMesh {
            name: name.clone(),
            material: surface.clone(),
            positions,
            normals: Vec::new(),
            uvs: Vec::new(),
            colors: Vec::new(),
            indices,
        };
        let bytes = glb::write(&mesh).map_err(|e| DecompileError::Glb { name: name.clone(), message: e.to_string() })?;
        let path = format!("collision/{ordinal:04}_{}.glb", slug(&surface));
        files.insert(path.clone(), bytes);
        let mut map = IndexMap::new();
        map.insert(surface.clone(), surface.clone());
        let kind = ObjectKind::Mesh(Mesh {
            source: path,
            material_map: map,
            collision: CollisionMode::Triangles,
            contents,
            cast_shadows: false,
            lightmap_scale: 0.0,
            render: false,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        });
        let mut o = hidden_object(name, layer, Default::default(), kind);
        o.transform = Default::default();
        out.push((o, RefRecord::Collision { model, contents, surface }));
    }
    Ok(out)
}

/// One group of mesh runs: the model, the material, and the runs that draw it,
/// in the order the mesh table lists them.
type Group<'a> = (u32, &'a str, Vec<usize>);

/// Groups decoded mesh runs by the model and material they draw with, first
/// appearance first.
fn group_runs(runs: &[meshes::DecodedRun]) -> Vec<Group<'_>> {
    let mut order: Vec<(u32, &str)> = Vec::new();
    let mut groups: BTreeMap<(u32, &str), Vec<usize>> = BTreeMap::new();
    for (i, run) in runs.iter().enumerate() {
        let key = (run.model, run.material.as_str());
        if !groups.contains_key(&key) {
            order.push(key);
        }
        groups.entry(key).or_default().push(i);
    }
    order
        .into_iter()
        .map(|(model, material)| {
            let parts = groups[&(model, material)].clone();
            (model, material, parts)
        })
        .collect()
}

/// The concatenated geometry of a group, with every run's indices shifted onto
/// the vertices in front of it.
type JoinedGeometry = (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<u32>);

fn join(parts: &[usize], runs: &[meshes::DecodedRun]) -> JoinedGeometry {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for &i in parts {
        let run = &runs[i];
        let base = positions.len() as u32;
        positions.extend(run.positions.iter().map(|p| p.to_array()));
        normals.extend(run.normals.iter().map(|n| n.to_array()));
        uvs.extend(run.uv0.iter().map(|uv| uv.to_array()));
        // Skips the degenerate triangle that pads an odd mesh to an even count.
        for t in run.indices.chunks_exact(3) {
            if t[0] == t[1] && t[1] == t[2] {
                continue;
            }
            indices.extend(t.iter().map(|ix| base.saturating_add(*ix)));
        }
    }
    (positions, normals, uvs, indices)
}

/// A file-name-safe fragment of a material path.
pub fn slug(material: &str) -> String {
    let mut out = String::with_capacity(material.len().min(SLUG_LEN));
    for c in material.chars() {
        if out.len() >= SLUG_LEN {
            break;
        }
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "mesh".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit;
    use glam::{Vec2, Vec3, Vec4};
    use rir::{AssetTables, RenderSurface};
    use uuid::Uuid;

    fn surface(model: u32, material: &str, offset: f32) -> RenderSurface {
        let p = |x: f32, y: f32| Vec3::new(x + offset, y, 0.0);
        RenderSurface {
            object: None,
            model,
            material: material.to_string(),
            positions: vec![p(0.0, 0.0), p(64.0, 0.0), p(0.0, 64.0)],
            normals: vec![Vec3::Z; 3],
            tangents: vec![Vec4::W; 3],
            uv0: vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
            uv1: vec![Vec2::ZERO; 3],
            colors: vec![[255; 4]; 3],
            indices: vec![0, 1, 2],
            cast_shadows: true,
            lightmap_texel: 16.0,
            lightmap_size: [0, 0],
        }
    }

    fn map_of(surfaces: &[RenderSurface]) -> Bsp {
        let mut bsp = emit::new_map(rbsp::Version::ClientV51);
        meshes::emit_render(surfaces, &AssetTables::default(), &mut bsp).expect("the surfaces write");
        bsp
    }

    #[test]
    fn a_file_name_fragment_keeps_only_what_a_file_name_holds() {
        assert_eq!(slug("world/dev/dev_gray_512"), "world_dev_dev_gray_512");
        assert_eq!(slug("a/b:c*d"), "a_b_c_d", "a path separator and a stream are not file-name characters");
        assert_eq!(slug("///"), "mesh", "nothing usable is still a name");
        let long = slug(&"x".repeat(200));
        assert_eq!(long.len(), SLUG_LEN, "a long path is cut to a file-name length");
    }

    /// The material name the glb's json chunk declares.
    fn glb_material(bytes: &[u8]) -> String {
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let doc: serde_json::Value = serde_json::from_slice(&bytes[20..20 + json_len]).expect("the json chunk parses");
        doc["materials"][0]["name"].as_str().unwrap_or_default().to_string()
    }

    fn run(model: u32, material: &str, triangles: u32) -> meshes::DecodedRun {
        let count = (triangles * 3) as usize;
        meshes::DecodedRun {
            model,
            material: material.to_string(),
            positions: (0..count).map(|i| Vec3::new(i as f32, 0.0, 0.0)).collect(),
            normals: vec![Vec3::Z; count],
            tangents: vec![None; count],
            uv0: vec![Vec2::ZERO; count],
            uv1: vec![Vec2::ZERO; count],
            colors: vec![[255; 4]; count],
            indices: (0..count as u32).collect(),
            vertex_type: 2,
            flags: 0,
            bounds_min: Vec3::ZERO,
            bounds_max: Vec3::ZERO,
        }
    }

    #[test]
    fn the_groups_are_the_model_and_material_pairs_in_mesh_order() {
        let runs = vec![
            run(0, "m/one", 1),
            run(1, "m/one", 1),
            run(0, "m/two", 1),
            run(1, "m/one", 1),
            run(0, "m/one", 1),
        ];
        let groups = group_runs(&runs);
        let keys: Vec<(u32, &str)> = groups.iter().map(|(model, material, _)| (*model, *material)).collect();
        assert_eq!(keys, vec![(0, "m/one"), (1, "m/one"), (0, "m/two")], "first appearance, mesh order kept");
        assert_eq!(groups[0].2, vec![0, 4], "the world model's two meshes join");
        assert_eq!(groups[1].2, vec![1, 3], "so do the second model's");
    }

    #[test]
    fn a_join_shifts_every_run_onto_the_vertices_in_front_of_it() {
        let runs = vec![run(0, "m/one", 1), run(0, "m/one", 1), run(0, "m/two", 1)];
        let (positions, normals, uvs, indices) = join(&[0, 1, 2], &runs);
        assert_eq!(positions.len(), 9);
        assert_eq!(normals.len(), 9);
        assert_eq!(uvs.len(), 9);
        assert_eq!(indices, vec![0, 1, 2, 3, 4, 5, 6, 7, 8], "no index addresses a vertex of another run");
        let (positions, _, _, indices) = join(&[0], &runs);
        assert_eq!(indices, vec![0, 1, 2]);
        assert_eq!(positions.len(), 3);
    }

    #[test]
    fn meshes_are_grouped_by_model_and_material() {
        let bsp = map_of(&[
            surface(0, "world/dev/dev_gray_512", 0.0),
            surface(0, "world/dev/dev_red_512", 128.0),
            surface(0, "world/dev/dev_gray_512", 256.0),
        ]);
        let mut files = BTreeMap::new();
        let objects: Vec<Object> =
            world_meshes(&bsp, Uuid::new_v4(), &mut files).expect("the meshes decompile").into_iter().map(|(o, _)| o).collect();
        assert_eq!(objects.len(), 2, "one group per material in the world model");
        assert_eq!(files.len(), 2, "one glb per group");
        let mut paths: Vec<&String> = files.keys().collect();
        paths.sort();
        for p in &paths {
            assert!(p.starts_with("meshes/") && p.ends_with(".glb"), "{p} is a project file");
        }
        let mut names: Vec<&str> = objects.iter().map(|o| o.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["mesh0000_world_dev_dev_gray_512", "mesh0001_world_dev_dev_red_512"]);
        for o in &objects {
            assert!(o.hidden, "a decompiled mesh does not compile until it is unhidden");
            let ObjectKind::Mesh(m) = &o.kind else { panic!("{} is a mesh", o.name) };
            assert_eq!(m.collision, CollisionMode::None, "a map with no collision leaves of a surface gives its mesh none");
            let path = &m.source;
            let mesh = glb::read(&files[path]).expect("the glb reads back");
            assert!(mesh.indices.len() % 3 == 0 && !mesh.indices.is_empty(), "{path} carries triangles");
            let named = m.material_map.keys().next().expect("a material").as_str();
            assert_eq!(glb_material(&files[path]), named, "the glb names its material");
            assert_eq!(m.material_map.get(named).map(String::as_str), Some(named), "and maps it to itself");
        }
    }

    #[test]
    fn every_triangle_of_a_group_reaches_its_glb() {
        let bsp = map_of(&[
            surface(0, "m/one", 0.0),
            surface(0, "m/one", 64.0),
            surface(0, "m/two", 128.0),
        ]);
        let mut files = BTreeMap::new();
        let objects: Vec<Object> =
            world_meshes(&bsp, Uuid::new_v4(), &mut files).expect("the meshes decompile").into_iter().map(|(o, _)| o).collect();
        let triangles = |o: &Object| -> usize {
            let ObjectKind::Mesh(m) = &o.kind else { panic!("a mesh") };
            glb::read(&files[&m.source]).expect("the glb reads").indices.len() / 3
        };
        let mut counts: Vec<usize> = objects.iter().map(triangles).collect();
        counts.sort_unstable();
        assert_eq!(counts, vec![1, 2], "one group holds two meshes, the other one");
    }

    #[test]
    fn a_map_with_no_render_geometry_has_no_meshes() {
        let mut files = BTreeMap::new();
        let objects = world_meshes(&emit::new_map(rbsp::Version::DediV47), Uuid::new_v4(), &mut files)
            .expect("a renderless map decompiles");
        assert!(objects.is_empty() && files.is_empty(), "a dedicated-server map carries no render lumps");
    }

    #[test]
    fn a_mesh_takes_the_contents_mask_its_collision_carries() {
        let mut bsp = map_of(&[surface(0, "m/one", 0.0)]);
        let slots = vec![rir::ModelSlot {
            index: 0,
            owner: None,
            mins: Vec3::ZERO,
            maxs: Vec3::splat(64.0),
            has_render: true,
        }];
        let solids = vec![rir::CollisionSolid {
            object: None,
            model: 0,
            shape: rir::CollisionShape::Triangles {
                vertices: vec![Vec3::ZERO, Vec3::X * 64.0, Vec3::Y * 64.0],
                indices: vec![0, 1, 2],
            },
            contents: 0x4000_0048,
            surface_prop: 0,
            surfaces: Vec::new(),
        }];
        collision::emit_collision(&solids, &slots, &mut bsp).expect("the collision writes");
        let mut files = BTreeMap::new();
        let objects: Vec<Object> =
            world_meshes(&bsp, Uuid::new_v4(), &mut files).expect("the meshes decompile").into_iter().map(|(o, _)| o).collect();
        let ObjectKind::Mesh(m) = &objects[0].kind else { panic!("a mesh") };
        assert_eq!(m.contents, 0x4000_0048, "a promoted mesh rebuilds what it collides with");
    }
}
