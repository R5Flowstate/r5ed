//! Heightfield terrain to render surfaces, with the splat weights in the
//! vertex colours a blend material reads.

use glam::{DMat4, DVec3, Vec2, Vec3, Vec4};
use rdoc::model::Terrain;
use rir::RenderSurface;
use rterrain::splat::SplatMaps;
use rterrain::terrain_mesh::{self, MeshParams, TerrainMesh, TerrainVertex};

use super::{CsgError, TJUNCTION_EPS};
use uuid::Uuid;

/// Lightmap texel size a terrain chunk falls back to.
pub const TERRAIN_LIGHTMAP_SCALE: f32 = 32.0;

/// Turns one terrain object into one render surface per chunk. Chunk 0 of the
/// finest level carries the collision triangles.
pub fn terrain_surfaces(
    mesh: &TerrainMesh,
    material: &str,
) -> Result<Vec<RenderSurface>, CsgError> {
    if mesh.vertices.is_empty() {
        return Err(CsgError::Terrain { object: Uuid::nil(), reason: "the mesh has no vertices".into() });
    }
    let mut out: Vec<RenderSurface> = Vec::new();
    for chunk in &mesh.chunks {
        if chunk.indices.is_empty() {
            continue;
        }
        let base = chunk.vertex_offset as usize;
        let verts = &mesh.vertices[base..(base + chunk.vertex_count as usize).min(mesh.vertices.len())];
        if verts.len() != chunk.vertex_count as usize {
            return Err(CsgError::Terrain {
                object: Uuid::nil(),
                reason: "a chunk runs past the shared vertex buffer".into(),
            });
        }
        let mut s = RenderSurface {
            object: None,
            model: 0,
            material: material.to_string(),
            positions: Vec::with_capacity(verts.len()),
            normals: Vec::with_capacity(verts.len()),
            tangents: Vec::with_capacity(verts.len()),
            uv0: Vec::with_capacity(verts.len()),
            uv1: Vec::with_capacity(verts.len()),
            colors: Vec::with_capacity(verts.len()),
            indices: Vec::with_capacity(chunk.indices.len()),
            cast_shadows: true,
            lightmap_texel: mesh.lightmap_scale.max(TERRAIN_LIGHTMAP_SCALE),
            lightmap_size: [0, 0],
        };
        for v in verts {
            push_vertex(&mut s, v);
        }
        // The chunk's own indices are relative to its vertex offset.
        for &i in &chunk.indices {
            if i as usize >= verts.len() {
                return Err(CsgError::Terrain {
                    object: Uuid::nil(),
                    reason: format!("chunk index {i} is out of range"),
                });
            }
            s.indices.push(i);
        }
        out.push(s);
    }
    Ok(out)
}

/// Collision triangles of the finest level, in world space.
pub fn terrain_collision(
    mesh: &TerrainMesh,
    world: DMat4,
    contents: u32,
    surface_prop: u32,
) -> rir::CollisionSolid {
    let mut positions: Vec<Vec3> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for chunk in &mesh.chunks {
        if chunk.lod != 0 {
            continue;
        }
        let base = chunk.vertex_offset as usize;
        let verts = &mesh.vertices[base..(base + chunk.vertex_count as usize).min(mesh.vertices.len())];
        let start = positions.len() as u32;
        for v in verts {
            positions.push(world.transform_point3(Vec3::from_array(v.position).as_dvec3()).as_vec3());
        }
        for t in &chunk.collision {
            if t.iter().all(|&i| (i as usize) < verts.len()) {
                indices.extend_from_slice(&[start + t[0], start + t[1], start + t[2]]);
            }
        }
    }
    rir::CollisionSolid {
        object: None,
        model: 0,
        shape: rir::CollisionShape::Triangles { vertices: positions, indices },
        contents,
        surface_prop,
        surfaces: Vec::new(),
    }
}

fn push_vertex(s: &mut RenderSurface, v: &TerrainVertex) {
    s.positions.push(Vec3::from_array(v.position));
    s.normals.push(Vec3::from_array(v.normal));
    s.tangents.push(Vec4::new(v.tangent[0], v.tangent[1], v.tangent[2], v.tangent[3]));
    s.uv0.push(Vec2::from_array(v.uv0));
    s.uv1.push(Vec2::from_array(v.lightmap_uv));
    s.colors.push(splat_to_color(v.splat));
}

/// A blend material reads one weight per vertex-colour channel; the fourth
/// channel carries opacity, so a fourth layer folds into the third.
pub fn splat_to_color(splat: [u8; 4]) -> [u8; 4] {
    let third = splat[2].saturating_add(splat[3]);
    [splat[0], splat[1], third, 255]
}

/// Reads a weight map into the channel layout [`splat_to_color`] expects.
pub fn splat_channel(weights: &[u8], layer: usize) -> u8 {
    match layer {
        0..=2 => weights.get(layer).copied().unwrap_or(0),
        3 => 0,
        _ => 0,
    }
}

/// Splat maps of a terrain, in the order its layers are declared.
pub fn splat_maps(terrain: &Terrain, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<SplatMaps, CsgError> {
    let mut maps: Vec<Vec<u8>> = Vec::with_capacity(terrain.layers.len());
    for l in &terrain.layers {
        let Some(bytes) = read(&l.weights) else {
            return Err(CsgError::Terrain {
                object: Uuid::nil(),
                reason: format!("weight map {} is missing", l.weights),
            });
        };
        maps.push(bytes);
    }
    let want = (terrain.resolution[0] as usize) * (terrain.resolution[1] as usize);
    for (i, m) in maps.iter().enumerate() {
        if m.len() != want {
            return Err(CsgError::Terrain {
                object: Uuid::nil(),
                reason: format!("weight map {i} has {} bytes, expected {want}", m.len()),
            });
        }
    }
    Ok(SplatMaps {
        width: terrain.resolution[0],
        height: terrain.resolution[1],
        cell_size: terrain.cell_size as f32,
        layers: maps,
    })
}

/// Default meshing parameters for a terrain object.
pub fn mesh_params(terrain: &Terrain) -> MeshParams {
    MeshParams {
        chunk_size: terrain.chunk_size.max(2),
        lod_levels: 4,
        uv_scale: terrain.layers.first().map(|l| l.uv_scale).unwrap_or(1.0 / 128.0) as f32,
        lightmap_scale: if terrain.lightmap_scale > 0.0 {
            terrain.lightmap_scale as f32
        } else {
            TERRAIN_LIGHTMAP_SCALE
        },
        lightmap_padding: 0.02,
        skirt_depth: 64.0,
        collision_max_error: None,
    }
}

/// Meshes a heightfield the terrain object describes.
pub fn mesh(terrain: &Terrain, hf: &rterrain::heightfield::Heightfield, splat: Option<&SplatMaps>) -> Result<TerrainMesh, CsgError> {
    terrain_mesh::build(hf, splat, &mesh_params(terrain))
        .map_err(|e| CsgError::Terrain { object: Uuid::nil(), reason: e.to_string() })
}

/// Chunk world bounds, for the model slot and the vis stage.
pub fn chunk_bounds(mesh: &TerrainMesh, chunk: &[u32; 2], lod: u32) -> Option<(DVec3, DVec3)> {
    let c = mesh.chunk(chunk[0], chunk[1], lod)?;
    let verts = c.vertices(mesh);
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for v in verts {
        let p = DVec3::new(v.position[0] as f64, v.position[1] as f64, v.position[2] as f64);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    (lo.x <= hi.x).then_some((lo, hi))
}

/// The heightfield chunk edge a terrain object tessellates at.
pub fn chunk_edge(terrain: &Terrain) -> f64 {
    terrain.chunk_size.max(2) as f64 * terrain.cell_size.abs().max(TJUNCTION_EPS)
}
