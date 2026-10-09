//! Heightfield meshing: chunked render meshes with level of detail, a skirt to
//! hide the LOD seams, per-chunk lightmap UVs, per-vertex splat weights and
//! full-resolution collision triangles.

use serde::{Deserialize, Serialize};

use glam::DVec3;

use crate::error::{Error, Result};
use crate::heightfield::{Heightfield, Rect, normalize3};
use crate::math;
use crate::splat::SplatMaps;

/// Vertex slots a chunk can carry, which is the usual hardware limit.
pub const MAX_SPLAT_LAYERS: usize = 4;

/// One render vertex.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainVertex {
    /// Object-space position, in inches.
    pub position: [f32; 3],
    /// Unit surface normal.
    pub normal: [f32; 3],
    /// Tangent with the bitangent sign in `w`.
    pub tangent: [f32; 4],
    /// World-planar UV scaled by the mesh's `uv_scale`.
    pub uv0: [f32; 2],
    /// Per-chunk lightmap UV in 0..1, padded away from the edge.
    pub lightmap_uv: [f32; 2],
    /// Splat weights, in the owning chunk's `layers` order.
    pub splat: [u8; 4],
}

/// One chunk at one level of detail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainMeshChunk {
    /// Chunk coordinate on each axis.
    pub chunk: [u32; 2],
    /// Level of detail, zero being the finest.
    pub lod: u32,
    /// First vertex of this chunk in the mesh's shared vertex buffer.
    pub vertex_offset: u32,
    /// Vertex count of this chunk.
    pub vertex_count: u32,
    /// Triangles as indices relative to `vertex_offset`.
    pub indices: Vec<u32>,
    /// Global layer indices matching the `splat` slots, heaviest first.
    pub layers: Vec<u8>,
    /// Collision triangles at full resolution, relative to `vertex_offset`.
    pub collision: Vec<[u32; 3]>,
}

impl TerrainMeshChunk {
    /// This chunk's vertices out of the shared buffer.
    pub fn vertices<'a>(&self, mesh: &'a TerrainMesh) -> &'a [TerrainVertex] {
        let a = self.vertex_offset as usize;
        &mesh.vertices[a..a + self.vertex_count as usize]
    }

    /// This chunk's world-space sample rectangle.
    pub fn rect(&self, mesh: &TerrainMesh) -> Rect {
        chunk_rect(self.chunk, mesh.chunk_size, mesh.resolution)
    }
}

/// How a heightfield is turned into chunks.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshParams {
    /// Samples per chunk edge.
    pub chunk_size: u32,
    /// Levels of detail, at least three.
    pub lod_levels: u32,
    /// World-planar UV repeats per inch.
    pub uv_scale: f32,
    /// World inches covered by one lightmap texel.
    pub lightmap_scale: f32,
    /// Lightmap border padding, as a fraction of the chunk's 0..1 range.
    pub lightmap_padding: f32,
    /// How far the skirt hangs below the chunk edge, in inches.
    pub skirt_depth: f32,
    /// Largest error a collision merge may introduce, in inches. `None` keeps
    /// every triangle.
    pub collision_max_error: Option<f32>,
}

impl Default for MeshParams {
    fn default() -> Self {
        Self {
            chunk_size: 64,
            lod_levels: 3,
            uv_scale: 1.0 / 128.0,
            lightmap_scale: 32.0,
            lightmap_padding: 0.02,
            skirt_depth: 64.0,
            collision_max_error: None,
        }
    }
}

impl MeshParams {
    fn sanitized(&self) -> MeshParams {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        MeshParams {
            chunk_size: self.chunk_size.clamp(2, 4096),
            lod_levels: self.lod_levels.clamp(3, 8),
            uv_scale: f(self.uv_scale, 0.0, 1.0, 1.0 / 128.0),
            lightmap_scale: f(self.lightmap_scale, 0.25, 65536.0, 32.0),
            lightmap_padding: f(self.lightmap_padding, 0.0, 0.45, 0.02),
            skirt_depth: f(self.skirt_depth, 0.0, 1.0e6, 64.0),
            collision_max_error: match self.collision_max_error {
                Some(v) if v.is_finite() && v > 0.0 => Some(v),
                _ => None,
            },
        }
    }
}

/// A whole heightfield meshed into chunks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainMesh {
    pub chunk_size: u32,
    pub lod_levels: u32,
    pub uv_scale: f32,
    pub lightmap_scale: f32,
    /// Heightfield sample resolution the mesh was built from.
    pub resolution: [u32; 2],
    pub cell_size: f32,
    pub vertices: Vec<TerrainVertex>,
    pub chunks: Vec<TerrainMeshChunk>,
}

impl TerrainMesh {
    /// Chunks on each axis.
    pub fn chunk_grid(&self) -> [u32; 2] {
        [
            self.resolution[0].div_ceil(self.chunk_size).max(1),
            self.resolution[1].div_ceil(self.chunk_size).max(1),
        ]
    }

    /// Look a chunk up by coordinate and level of detail.
    pub fn chunk(&self, cx: u32, cy: u32, lod: u32) -> Option<&TerrainMeshChunk> {
        self.chunks.iter().find(|c| c.chunk == [cx, cy] && c.lod == lod)
    }

    /// Total render triangles across every chunk and level.
    pub fn triangle_count(&self) -> usize {
        self.chunks.iter().map(|c| c.indices.len() / 3).sum()
    }

    /// Total collision triangles across the finest level.
    pub fn collision_triangle_count(&self) -> usize {
        self.chunks.iter().filter(|c| c.lod == 0).map(|c| c.collision.len()).sum()
    }
}

/// The sample rectangle a chunk covers, half-open on the high edge.
pub fn chunk_rect(chunk: [u32; 2], chunk_size: u32, resolution: [u32; 2]) -> Rect {
    let last_x = resolution[0].saturating_sub(1);
    let last_y = resolution[1].saturating_sub(1);
    let x0 = (chunk[0] * chunk_size).min(last_x);
    let y0 = (chunk[1] * chunk_size).min(last_y);
    let x1 = (x0 + chunk_size).min(last_x);
    let y1 = (y0 + chunk_size).min(last_y);
    Rect::new(x0, y0, x1 + 1, y1 + 1)
}

/// The sample indices a chunk samples at a level of detail.
///
/// The step is `1 << lod`, and both rectangle ends are always present, so two
/// neighbouring chunks at the same level sample exactly the same world columns
/// on the seam and cannot crack.
pub fn sample_indices(start: u32, end_exclusive: u32, lod: u32) -> Vec<u32> {
    if end_exclusive <= start {
        return vec![start];
    }
    let last = end_exclusive - 1;
    let step = 1u32 << lod.min(16);
    let mut out = Vec::new();
    let mut x = start;
    while x < last {
        out.push(x);
        match x.checked_add(step) {
            Some(n) if n > x => x = n,
            _ => break,
        }
    }
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

/// Build the chunked mesh.
pub fn build(hf: &Heightfield, splat: Option<&SplatMaps>, params: &MeshParams) -> Result<TerrainMesh> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    let p = params.sanitized();
    if let Some(s) = splat {
        if s.width != hf.width || s.height != hf.height {
            return Err(Error::WeightMapSize(0, s.width, s.height, hf.width, hf.height));
        }
    }
    let grid = [hf.width.div_ceil(p.chunk_size).max(1), hf.height.div_ceil(p.chunk_size).max(1)];
    let mut mesh = TerrainMesh {
        chunk_size: p.chunk_size,
        lod_levels: p.lod_levels,
        uv_scale: p.uv_scale,
        lightmap_scale: p.lightmap_scale,
        resolution: [hf.width, hf.height],
        cell_size: hf.cell_size,
        vertices: Vec::new(),
        chunks: Vec::new(),
    };

    for lod in 0..p.lod_levels {
        for cy in 0..grid[1] {
            for cx in 0..grid[0] {
                let (verts, indices, layers, collision) = build_chunk(hf, splat, &p, [cx, cy], lod);
                let offset = mesh.vertices.len() as u32;
                let count = verts.len() as u32;
                mesh.vertices.extend_from_slice(&verts);
                mesh.chunks.push(TerrainMeshChunk {
                    chunk: [cx, cy],
                    lod,
                    vertex_offset: offset,
                    vertex_count: count,
                    indices,
                    layers,
                    collision,
                });
            }
        }
    }
    Ok(mesh)
}

type ChunkBuild = (Vec<TerrainVertex>, Vec<u32>, Vec<u8>, Vec<[u32; 3]>);

fn build_chunk(
    hf: &Heightfield,
    splat: Option<&SplatMaps>,
    p: &MeshParams,
    chunk: [u32; 2],
    lod: u32,
) -> ChunkBuild {
    let r = chunk_rect(chunk, p.chunk_size, [hf.width, hf.height]);
    let xs = sample_indices(r.x0, r.x1, lod);
    let ys = sample_indices(r.y0, r.y1, lod);
    let nx = xs.len();
    let ny = ys.len();
    let layers = splat.map(|s| s.layers_in(r)).unwrap_or_default();
    let top: Vec<u8> = layers.iter().take(MAX_SPLAT_LAYERS).map(|l| *l as u8).collect();

    // The chunk's world footprint, used to normalise the lightmap UV.
    let (wx0, wy0) = hf.sample_to_world(xs[0] as f32, ys[0] as f32);
    let (wx1, wy1) = hf.sample_to_world(xs[nx - 1] as f32, ys[ny - 1] as f32);
    let span_x = (wx1 - wx0).abs().max(f32::MIN_POSITIVE);
    let span_y = (wy1 - wy0).abs().max(f32::MIN_POSITIVE);

    let mut verts: Vec<TerrainVertex> = Vec::with_capacity(nx * ny);
    for y in ys.iter() {
        for x in xs.iter() {
            verts.push(vertex_at(hf, splat, p, &top, *x, *y, wx0, wy0, span_x, span_y));
        }
    }

    let mut indices: Vec<u32> = Vec::new();
    for iy in 0..ny.saturating_sub(1) {
        for ix in 0..nx.saturating_sub(1) {
            let a = (iy * nx + ix) as u32;
            let b = a + 1;
            let c = a + nx as u32;
            let d = c + 1;
            emit_wound_quad(&mut indices, [a, b, d, c], &verts);
        }
    }

    // The skirt hangs below the border so a coarser neighbour cannot show a
    // gap along the seam, whatever the level mismatch.
    let ring = skirt_ring(nx, ny);
    let grid_count = (nx * ny) as u32;
    for i in 0..ring {
        let src = border_vertex(i, nx, ny) as u32;
        let mut low = verts[src as usize];
        low.position[2] -= p.skirt_depth;
        verts.push(low);
        // The last slot joins back to the first, so the skirt is a closed band.
        let next = border_vertex((i + 1) % ring, nx, ny) as u32;
        let a = grid_count + i as u32;
        let b = grid_count + ((i + 1) % ring) as u32;
        push_quad(&mut indices, [a, b, next, src]);
    }

    let collision = if lod == 0 { collision_triangles(hf, &xs, &ys, p.collision_max_error) } else { Vec::new() };
    (verts, indices, top, collision)
}

#[allow(clippy::too_many_arguments)]
fn vertex_at(
    hf: &Heightfield,
    splat: Option<&SplatMaps>,
    p: &MeshParams,
    top: &[u8],
    x: u32,
    y: u32,
    wx0: f32,
    wy0: f32,
    span_x: f32,
    span_y: f32,
) -> TerrainVertex {
    let (wx, wy) = hf.sample_to_world(x as f32, y as f32);
    let h = hf.get(x, y).unwrap_or(0.0);
    let n = hf.normal(x, y);
    // The tangent follows +X projected onto the surface.
    let (gx, _) = hf.gradient(x, y);
    let tangent = normalize3([1.0, -gx, 0.0]);
    let pad = p.lightmap_padding;
    let mut splat_w = [0u8; 4];
    if let Some(s) = splat {
        for (slot, layer) in top.iter().enumerate().take(MAX_SPLAT_LAYERS) {
            splat_w[slot] = s.weight(*layer as usize, x, y);
        }
    }
    TerrainVertex {
        position: [wx, wy, h],
        normal: n,
        tangent: [tangent[0], tangent[1], tangent[2], 1.0],
        uv0: [wx * p.uv_scale, wy * p.uv_scale],
        lightmap_uv: [
            pad + (1.0 - 2.0 * pad) * ((wx - wx0) / span_x),
            pad + (1.0 - 2.0 * pad) * ((wy - wy0) / span_y),
        ],
        splat: splat_w,
    }
}

/// Split a quad into two triangles, winding them against the vertex normals so
/// a steep cell cannot flip the face.
fn emit_wound_quad(out: &mut Vec<u32>, q: [u32; 4], verts: &[TerrainVertex]) {
    let n = geom_normal(verts, q[0], q[1], q[2]);
    let mut up = DVec3::ZERO;
    for v in q {
        up += to_d3(verts[v as usize].normal);
    }
    if n.dot(up) >= 0.0 {
        push_quad(out, q);
    } else {
        push_quad(out, [q[0], q[3], q[2], q[1]]);
    }
}

fn push_quad(out: &mut Vec<u32>, q: [u32; 4]) {
    let [a, b, c, d] = q;
    if a == b || b == c || c == d || d == a || a == c || b == d {
        return;
    }
    out.extend_from_slice(&[a, b, c]);
    out.extend_from_slice(&[a, c, d]);
}

fn geom_normal(verts: &[TerrainVertex], a: u32, b: u32, c: u32) -> DVec3 {
    let p = |i: u32| to_d3(verts[i as usize].position);
    (p(b) - p(a)).cross(p(c) - p(a))
}

fn to_d3(v: [f32; 3]) -> DVec3 {
    DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
}

/// Number of vertices in the skirt ring, which walks the grid border once.
fn skirt_ring(nx: usize, ny: usize) -> usize {
    if nx < 2 || ny < 2 {
        return 0;
    }
    2 * (nx + ny) - 4
}

/// The grid vertex that skirt slot `i` hangs from, walking the border
/// anticlockwise from the low X corner and visiting every border vertex once.
fn border_vertex(i: usize, nx: usize, ny: usize) -> usize {
    let last_x = nx - 1;
    let last_y = ny - 1;
    // The four runs are the low Y row, the high X column, the high Y row walked
    // back, and the low X column, each starting one step past the corner the
    // previous run ended on.
    let (cx, cy) = if i < nx {
        (i, 0)
    } else if i < nx + last_y {
        (last_x, i - nx + 1)
    } else if i < nx + last_y + last_x {
        (last_x - 1 - (i - nx - last_y), last_y)
    } else {
        (0, last_y - 1 - (i - nx - last_y - last_x))
    };
    cy * nx + cx
}

/// Full-resolution collision triangles for a chunk, optionally merged.
///
/// Two cells side by side collapse into one quad through their four outer
/// corners when those corners sit within `max_error` of their mean, which is
/// the tightest bound a heightfield allows without a quadric. A heightfield is
/// exact in x and y, so dropping the shared edge leaves the surface watertight:
/// the block's outline is exactly that quad, and every row still tiles the whole
/// footprint.
fn collision_triangles(
    hf: &Heightfield,
    xs: &[u32],
    ys: &[u32],
    max_error: Option<f32>,
) -> Vec<[u32; 3]> {
    let nx = xs.len();
    let ny = ys.len();
    let at = |ix: usize, iy: usize| hf.get(xs[ix], ys[iy]).unwrap_or(0.0);
    let mut out: Vec<[u32; 3]> = Vec::new();
    for iy in 0..ny.saturating_sub(1) {
        let mut ix = 0usize;
        while ix + 1 < nx {
            let wide = max_error.is_some() && ix + 2 < nx;
            let corners =
                [at(ix, iy), at(ix + 2.min(nx - 1 - ix), iy), at(ix, iy + 1), at(ix + 2.min(nx - 1 - ix), iy + 1)];
            let step = if wide && can_merge(&corners, max_error) { 2 } else { 1 };
            let c00 = (iy * nx + ix) as u32;
            let c10 = (iy * nx + ix + step) as u32;
            let c01 = ((iy + 1) * nx + ix) as u32;
            let c11 = ((iy + 1) * nx + ix + step) as u32;
            out.push([c00, c10, c11]);
            out.push([c00, c11, c01]);
            ix += step;
        }
    }
    out
}


/// True when four corners can be replaced by one planar quad.
fn can_merge(corners: &[f32; 4], max_error: Option<f32>) -> bool {
    let Some(tol) = max_error else { return false };
    if tol <= 0.0 || !tol.is_finite() {
        return false;
    }
    // A heightfield is exact in x and y, so the least-squares plane through the
    // four points is the constant at their mean, and the error is the largest
    // deviation from it.
    let mean = (corners[0] + corners[1] + corners[2] + corners[3]) * 0.25;
    let mut worst = 0.0f32;
    for c in corners {
        worst = worst.max((*c - mean).abs());
    }
    worst <= tol
}

/// Total surface area of a chunk's collision mesh, in square inches.
pub fn collision_area(chunk: &TerrainMeshChunk, mesh: &TerrainMesh) -> f64 {
    let verts = chunk.vertices(mesh);
    let mut area = 0.0f64;
    for t in &chunk.collision {
        let p = |i: u32| to_d3(verts[i as usize].position);
        area += 0.5 * (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])).length();
    }
    area
}

/// Lightmap texel count a chunk needs, given the mesh's scale.
pub fn lightmap_size(mesh: &TerrainMesh, chunk: &TerrainMeshChunk) -> (u32, u32) {
    let r = chunk.rect(mesh);
    let w = (r.width() as f32 * mesh.cell_size / mesh.lightmap_scale).ceil().max(1.0);
    let h = (r.height() as f32 * mesh.cell_size / mesh.lightmap_scale).ceil().max(1.0);
    ((w as u32).max(1), (h as u32).max(1))
}

/// Fraction of a chunk's samples that fall below `height`, used by gameplay
/// queries that need a cheap ground test.
pub fn fraction_below(hf: &Heightfield, rect: Rect, height: f32) -> f64 {
    let r = rect.clamp_to(hf.width, hf.height);
    if r.is_empty() {
        return 0.0;
    }
    let mut under = 0u32;
    let mut total = 0u32;
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            if hf.get_clamped(x as i64, y as i64) < height {
                under += 1;
            }
            total += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        math::saturate(under as f64 / total as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generators::{Generator, MountainsParams};
    use crate::splat::{LayerRule, autopaint};

    fn terrain(w: u32, h: u32) -> Heightfield {
        Generator::Mountains(MountainsParams::default()).generate(w, h, 16.0).unwrap()
    }

    #[test]
    fn build_produces_chunks_at_every_level() {
        let hf = terrain(128, 96);
        let p = MeshParams { chunk_size: 32, lod_levels: 3, ..MeshParams::default() };
        let mesh = build(&hf, None, &p).unwrap();
        assert_eq!(mesh.chunk_grid(), [4, 3]);
        assert_eq!(mesh.chunks.len(), 4 * 3 * 3);
        for c in &mesh.chunks {
            assert!(c.vertex_count > 0);
            assert_eq!(c.indices.len() % 3, 0);
            assert_eq!(c.vertices(&mesh).len(), c.vertex_count as usize);
        }
        assert!(mesh.triangle_count() > 0);
        // Only the finest level carries collision.
        assert!(mesh.collision_triangle_count() > 0);
        assert!(mesh.chunks.iter().filter(|c| c.lod > 0).all(|c| c.collision.is_empty()));
    }

    #[test]
    fn vertex_offsets_index_the_shared_buffer() {
        let hf = terrain(96, 96);
        let p = MeshParams { chunk_size: 32, ..MeshParams::default() };
        let mesh = build(&hf, None, &p).unwrap();
        let mut end = 0u32;
        for c in &mesh.chunks {
            assert_eq!(c.vertex_offset, end, "chunk {:?} has a gap in the vertex buffer", c.chunk);
            end += c.vertex_count;
            assert!(end as usize <= mesh.vertices.len());
        }
        assert_eq!(end as usize, mesh.vertices.len());
    }

    #[test]
    fn vertex_attributes_are_finite_and_ordered() {
        let hf = terrain(64, 64);
        let p = MeshParams { chunk_size: 32, uv_scale: 0.01, ..MeshParams::default() };
        let mesh = build(&hf, None, &p).unwrap();
        for v in &mesh.vertices {
            assert!(v.position.iter().all(|c| c.is_finite()));
            assert!(v.normal.iter().all(|c| c.is_finite()));
            assert!(v.normal[2].abs() <= 1.0 + 1e-6, "normal is not unit: {v:?}");
            assert!(v.tangent.iter().all(|c| c.is_finite()));
            assert_eq!(v.tangent[3], 1.0);
            assert!((v.uv0[0] - v.position[0] * 0.01).abs() < 1e-6);
            assert!(v.lightmap_uv[0] > 0.0 && v.lightmap_uv[0] < 1.0, "lightmap u {}", v.lightmap_uv[0]);
            assert!(v.lightmap_uv[1] > 0.0 && v.lightmap_uv[1] < 1.0, "lightmap v {}", v.lightmap_uv[1]);
        }
    }

    #[test]
    fn splat_weights_name_the_layers_the_chunk_uses() {
        let hf = terrain(96, 96);
        let rules = vec![
            LayerRule { name: "low".into(), range_lo: 0.0, range_hi: 300.0, ..LayerRule::default() },
            LayerRule { name: "mid".into(), range_lo: 300.0, range_hi: 900.0, ..LayerRule::default() },
            LayerRule { name: "high".into(), range_lo: 900.0, range_hi: 3000.0, ..LayerRule::default() },
            LayerRule { name: "extra".into(), range_lo: 0.0, range_hi: 3000.0, ..LayerRule::default() },
            LayerRule { name: "fifth".into(), range_lo: 0.0, range_hi: 100.0, ..LayerRule::default() },
        ];
        let splat = autopaint(&hf, &rules).unwrap();
        let p = MeshParams { chunk_size: 32, ..MeshParams::default() };
        let mesh = build(&hf, Some(&splat), &p).unwrap();
        for c in &mesh.chunks {
            assert!(c.layers.len() <= MAX_SPLAT_LAYERS, "chunk carries {} layers", c.layers.len());
            for layer in &c.layers {
                assert!(*layer < 5);
            }
            // Every slot the chunk claims is actually used by a vertex.
            for (slot, _) in c.layers.iter().enumerate() {
                assert!(
                    c.vertices(&mesh).iter().any(|v| v.splat[slot] > 0),
                    "chunk {:?} claims layer {slot} but no vertex uses it",
                    c.chunk
                );
            }
        }
        // A weight map that does not match the heightfield is refused.
        let other = autopaint(&terrain(32, 32), &rules).unwrap();
        assert!(build(&hf, Some(&other), &p).is_err());
    }

    #[test]
    fn collision_decimation_reduces_triangles_within_the_bound() {
        let hf = Heightfield::filled(64, 64, 16.0, 500.0).unwrap();
        let p = MeshParams { chunk_size: 32, ..MeshParams::default() };
        let full = build(&hf, None, &p).unwrap();
        let loose = MeshParams { collision_max_error: Some(1000.0), ..p };
        let merged = build(&hf, None, &loose).unwrap();
        assert!(
            merged.collision_triangle_count() < full.collision_triangle_count(),
            "{} vs {}",
            merged.collision_triangle_count(),
            full.collision_triangle_count()
        );
        // Every collision index stays inside the chunk's vertex range.
        for c in &merged.chunks {
            for t in &c.collision {
                for i in t {
                    assert!(*i < c.vertex_count, "collision index {i} outside the chunk");
                }
            }
        }
        // A perfectly flat field's merged collision covers the same area.
        let area_full = area_of(&full);
        let area_merged = area_of(&merged);
        assert!((area_full - area_merged).abs() < area_full * 0.01);
    }

    fn area_of(mesh: &TerrainMesh) -> f64 {
        mesh.chunks.iter().filter(|c| c.lod == 0).map(|c| collision_area(c, mesh)).sum()
    }

    #[test]
    fn merge_bound_is_respected() {
        let flat = [10.0f32; 4];
        assert!(can_merge(&flat, Some(1.0)));
        assert!(!can_merge(&flat, None));
        let bumpy = [0.0, 5.0, 0.0, 0.0];
        assert!(!can_merge(&bumpy, Some(1.0)));
        assert!(can_merge(&bumpy, Some(10.0)));
        assert!(!can_merge(&bumpy, Some(f32::NAN)));
        assert!(!can_merge(&bumpy, Some(0.0)));
    }

    #[test]
    fn build_rejects_bad_input_and_clamps_extremes() {
        let empty = Heightfield { width: 1, height: 1, cell_size: 1.0, data: Vec::new() };
        assert!(matches!(build(&empty, None, &MeshParams::default()), Err(Error::ZeroSize)));
        let hf = terrain(32, 32);
        let tiny = MeshParams { chunk_size: 0, ..MeshParams::default() };
        assert!(build(&hf, None, &tiny).is_ok());
        let wild = MeshParams {
            chunk_size: u32::MAX,
            lod_levels: 0,
            uv_scale: f32::NAN,
            lightmap_scale: -1.0,
            lightmap_padding: 9.0,
            skirt_depth: f32::NEG_INFINITY,
            collision_max_error: Some(-4.0),
        };
        let m = build(&hf, None, &wild).unwrap();
        assert_eq!(m.lod_levels, 3);
        assert!(m.uv_scale.is_finite());
        assert_eq!(m.chunk_size, 4096);
    }

    #[test]
    fn sample_indices_always_include_both_ends() {
        for lod in 0..6u32 {
            let idx = sample_indices(0, 65, lod);
            assert_eq!(idx[0], 0);
            assert_eq!(*idx.last().unwrap(), 64);
            assert!(idx.windows(2).all(|w| w[0] < w[1]), "indices are not ascending");
        }
        assert_eq!(sample_indices(5, 5, 2), vec![5]);
        assert_eq!(sample_indices(0, 5, 0).len(), 5);
        // A step of two over four gaps is three samples, endpoints included.
        assert_eq!(sample_indices(0, 5, 1), vec![0, 2, 4]);
        assert_eq!(sample_indices(0, 5, 2), vec![0, 4]);
    }

    #[test]
    fn chunk_rect_stays_inside_the_grid() {
        assert_eq!(chunk_rect([0, 0], 32, [40, 40]), Rect::new(0, 0, 33, 33));
        assert_eq!(chunk_rect([1, 1], 32, [40, 40]), Rect::new(32, 32, 40, 40));
        assert_eq!(chunk_rect([9, 9], 32, [40, 40]), Rect::new(39, 39, 40, 40));
    }

    #[test]
    fn skirt_ring_walks_the_border_once() {
        for (nx, ny) in [(4usize, 4usize), (33, 5), (2, 9)] {
            let ring = skirt_ring(nx, ny);
            assert_eq!(ring, 2 * (nx + ny) - 4);
            let mut seen = std::collections::HashSet::new();
            for i in 0..ring {
                let b = border_vertex(i, nx, ny);
                assert!(b < nx * ny, "slot {i} went out of range");
                assert!(seen.insert(b), "slot {i} repeated border vertex {b}");
            }
            assert_eq!(seen.len(), ring);
        }
        assert_eq!(skirt_ring(1, 4), 0);
    }

    #[test]
    fn mesh_lookup_counts_and_helpers() {
        let hf = terrain(64, 64);
        let mesh = build(&hf, None, &MeshParams { chunk_size: 32, ..MeshParams::default() }).unwrap();
        assert!(mesh.chunk(0, 0, 0).is_some());
        assert!(mesh.chunk(0, 0, 5).is_none());
        assert!(mesh.chunk(9, 9, 0).is_none());
        let c = mesh.chunk(0, 0, 0).unwrap();
        assert_eq!(c.rect(&mesh), Rect::new(0, 0, 33, 33));
        let (tw, th) = lightmap_size(&mesh, c);
        assert!(tw >= 1 && th >= 1);
        let frac = fraction_below(&hf, Rect::all(64, 64), 1.0e9);
        assert!((frac - 1.0).abs() < 1e-9);
        assert_eq!(fraction_below(&hf, Rect::new(100, 100, 110, 110), 0.0), 0.0);
    }

    #[test]
    fn lod_borders_crack_free() {
        let hf = terrain(96, 80);
        let p = MeshParams { chunk_size: 32, lod_levels: 3, skirt_depth: 48.0, ..MeshParams::default() };
        let mesh = build(&hf, None, &p).unwrap();
        let [gx, gy] = mesh.chunk_grid();
        // Two chunks either side of a seam overlap on one sample column or row,
        // and both read that column out of the same heightfield, so their seam
        // vertices must land on the same positions. A crack would show up here
        // as a seam sample one chunk placed and the other did not.
        for lod in 0..mesh.lod_levels {
            for cy in 0..gy {
                for cx in 0..gx {
                    let here = mesh.chunk(cx, cy, lod).expect("every chunk exists at every level");
                    let seam = |c: &TerrainMeshChunk, sample: u32, axis: usize| -> Vec<(u32, u32)> {
                        let world = if axis == 0 {
                            hf.sample_to_world(sample as f32, 0.0).0
                        } else {
                            hf.sample_to_world(0.0, sample as f32).1
                        };
                        c.vertices(&mesh)
                            .iter()
                            .filter_map(|v| {
                                let along = if axis == 0 { v.position[1] } else { v.position[0] };
                                let across = if axis == 0 { v.position[0] } else { v.position[1] };
                                if (across - world).abs() > 1e-3 {
                                    return None;
                                }
                                Some((along.to_bits() as u32, v.position[2].to_bits() as u32))
                            })
                            .collect()
                    };
                    if cx + 1 < gx {
                        let right = mesh.chunk(cx + 1, cy, lod).expect("neighbour exists");
                        // The two rectangles share their last and first sample.
                        let s = here.rect(&mesh).x1 - 1;
                        let mut a = seam(here, s, 0);
                        let mut b = seam(right, s, 0);
                        a.sort_unstable();
                        b.sort_unstable();
                        assert!(!a.is_empty(), "chunk {cx},{cy} lod {lod} has no seam vertex");
                        assert_eq!(a, b, "the seam between {cx} and {} at lod {lod} is open", cx + 1);
                    }
                    if cy + 1 < gy {
                        let above = mesh.chunk(cx, cy + 1, lod).expect("neighbour exists");
                        let s = here.rect(&mesh).y1 - 1;
                        let mut a = seam(here, s, 1);
                        let mut b = seam(above, s, 1);
                        a.sort_unstable();
                        b.sort_unstable();
                        assert!(!a.is_empty(), "chunk {cx},{cy} lod {lod} has no seam vertex");
                        assert_eq!(a, b, "the seam between {cy} and {} at lod {lod} is open", cy + 1);
                    }
                }
            }
        }
        // Every chunk hangs a skirt from its whole border, which is what covers
        // a level mismatch: one extra vertex per border sample, exactly
        // `skirt_depth` below the surface vertex it hangs from.
        for c in &mesh.chunks {
            let rect = c.rect(&mesh);
            let nx = sample_indices(rect.x0, rect.x1, c.lod).len();
            let ny = sample_indices(rect.y0, rect.y1, c.lod).len();
            let ring = skirt_ring(nx, ny);
            let verts = c.vertices(&mesh);
            assert_eq!(verts.len(), nx * ny + ring, "chunk {:?} lod {} vertex count", c.chunk, c.lod);
            for i in 0..ring {
                let src = border_vertex(i, nx, ny);
                let low = &verts[nx * ny + i];
                let top = &verts[src];
                assert_eq!(
                    (low.position[0], low.position[1]),
                    (top.position[0], top.position[1]),
                    "the skirt at slot {i} does not hang from its border vertex"
                );
                assert!(
                    (low.position[2] - (top.position[2] - p.skirt_depth)).abs() < 1e-3,
                    "the skirt at slot {i} hangs {} deep", top.position[2] - low.position[2]
                );
            }
        }
    }
}
