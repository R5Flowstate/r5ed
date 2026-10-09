//! Surface nets meshing of a signed distance field.
//!
//! Naive surface nets places one vertex per cell that straddles the surface, at
//! the average of that cell's zero crossings, and emits one quad per crossing
//! grid edge. Because the vertex table is global and each crossing edge is
//! emitted by exactly one cell, two chunks that share a cell boundary index the
//! same vertex, so the surface is watertight across chunk borders by
//! construction rather than by stitching.

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::math;
use crate::sdf::VolumeTerrain;

/// A meshed vertex with a normal and a signed distance.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshVertex {
    /// Object-space position, in inches.
    pub position: DVec3,
    /// Outward unit normal.
    pub normal: DVec3,
    /// Field value at the vertex, negative inside the solid.
    pub value: f32,
}

/// One chunk's slice of the shared index buffer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolumeMeshChunk {
    /// Chunk coordinate on each axis.
    pub chunk: [u32; 3],
    /// Indices into the mesh's shared vertex buffer.
    pub indices: Vec<u32>,
}

/// A chunked surface-nets mesh. Vertices are shared between chunks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolumeMesh {
    pub vertices: Vec<MeshVertex>,
    pub chunks: Vec<VolumeMeshChunk>,
    /// Voxel size the mesh was built at.
    pub voxel_size: f64,
    /// Object-space low corner of the field.
    pub origin: DVec3,
    /// Field resolution the mesh was built at.
    pub resolution: [u32; 3],
}

impl VolumeMesh {
    /// Triangle count across every chunk.
    pub fn triangle_count(&self) -> usize {
        self.chunks.iter().map(|c| c.indices.len() / 3).sum()
    }

    /// Every triangle of the mesh, chunk by chunk, as vertex indices.
    pub fn triangles(&self) -> Vec<[u32; 3]> {
        let mut out = Vec::with_capacity(self.triangle_count());
        for c in &self.chunks {
            for t in c.indices.chunks_exact(3) {
                out.push([t[0], t[1], t[2]]);
            }
        }
        out
    }

    /// How many triangles use each undirected edge. A closed surface uses every
    /// edge exactly twice.
    pub fn edge_use_counts(&self) -> std::collections::HashMap<(u32, u32), u32> {
        let mut counts = std::collections::HashMap::new();
        for t in self.triangles() {
            for e in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let key = if e.0 <= e.1 { (e.0, e.1) } else { (e.1, e.0) };
                *counts.entry(key).or_insert(0u32) += 1;
            }
        }
        counts
    }

    /// Axis-aligned bounds of the vertices, or zero bounds when empty.
    pub fn bounds(&self) -> (DVec3, DVec3) {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for v in &self.vertices {
            lo = lo.min(v.position);
            hi = hi.max(v.position);
        }
        if !lo.is_finite() {
            return (DVec3::ZERO, DVec3::ZERO);
        }
        (lo, hi)
    }
}

/// Corner index to voxel offset, in X-fastest order: bit 0 is X, 1 is Y, 2 is Z.
const CORNER_OFFSET: [(u32, u32, u32); 8] = [
    (0, 0, 0),
    (1, 0, 0),
    (0, 1, 0),
    (1, 1, 0),
    (0, 0, 1),
    (1, 0, 1),
    (0, 1, 1),
    (1, 1, 1),
];

/// The twelve cell edges as corner index pairs, every one running in a
/// positive direction along exactly one axis.
const ALL_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 3),
    (2, 3),
    (0, 2),
    (4, 5),
    (5, 7),
    (6, 7),
    (4, 6),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

/// The two axes an edge does not run along, in index order.
fn lateral_axes(axis: usize) -> [usize; 2] {
    match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    }
}

/// The four cells that share the lattice edge leaving corner `a` of a cell in
/// the positive `axis` direction, in cycle order around that edge.
fn sharing_cells(a: (u32, u32, u32), axis: usize) -> [(i32, i32, i32); 4] {
    let base = [a.0 as i32, a.1 as i32, a.2 as i32];
    let [e0, e1] = lateral_axes(axis);
    let mut c = base;
    c[e0] -= 1;
    let d = c;
    let mut e = base;
    e[e1] -= 1;
    let f = e;
    let mut g = base;
    g[e0] -= 1;
    g[e1] -= 1;
    [(base[0], base[1], base[2]), (d[0], d[1], d[2]), (g[0], g[1], g[2]), (f[0], f[1], f[2])]
}

#[inline]
fn idx(nx: u32, ny: u32, i: u32, j: u32, k: u32) -> usize {
    (k * ny + j) as usize * nx as usize + i as usize
}

/// Evaluate a volume terrain and mesh it with surface nets.
pub fn mesh(terrain: &VolumeTerrain) -> Result<VolumeMesh> {
    mesh_field(&terrain.evaluate()?, terrain.chunks, terrain.voxel_size, terrain.origin)
}

/// Surface nets over an existing distance field.
pub fn mesh_field(
    field: &[f32],
    resolution: [u32; 3],
    voxel_size: f64,
    origin: DVec3,
) -> Result<VolumeMesh> {
    let (nx, ny, nz) = (resolution[0], resolution[1], resolution[2]);
    if nx < 2 || ny < 2 || nz < 2 {
        return Err(Error::SdfTooSmall(nx, ny, nz));
    }
    let want = nx as usize * ny as usize * nz as usize;
    if field.len() != want {
        return Err(Error::SizeMismatch(nx, ny, field.len()));
    }
    if !voxel_size.is_finite() || voxel_size <= 0.0 {
        return Err(Error::InvalidVoxelSize(voxel_size));
    }
    if !origin.is_finite() {
        return Err(Error::BadParams("mesh origin must be finite"));
    }
    let (cx, cy, cz) = (nx - 1, ny - 1, nz - 1);

    // One vertex per straddling cell, addressed by cell so a cell that two
    // chunks both touch can only ever produce one vertex.
    let mut vertex_of_cell: Vec<u32> = vec![u32::MAX; cx as usize * cy as usize * cz as usize];
    let mut vertices: Vec<MeshVertex> = Vec::new();
    for k in 0..cz {
        for j in 0..cy {
            for i in 0..cx {
                let mask = corner_mask(field, nx, ny, i, j, k);
                if mask == 0 || mask == 255 {
                    continue;
                }
                let cell = cell_index(cx, cy, i, j, k);
                vertex_of_cell[cell] = vertices.len() as u32;
                vertices.push(cell_vertex(field, nx, ny, i, j, k, mask, voxel_size, origin));
            }
        }
    }

    let cv = VolumeTerrain::CHUNK_VOXELS;
    let grid = [cx.div_ceil(cv).max(1), cy.div_ceil(cv).max(1), cz.div_ceil(cv).max(1)];
    let mut per_chunk: Vec<Vec<u32>> = vec![Vec::new(); (grid[0] * grid[1] * grid[2]) as usize];
    let at = |i: i32, j: i32, k: i32| -> Option<u32> {
        if i < 0 || j < 0 || k < 0 || i >= cx as i32 || j >= cy as i32 || k >= cz as i32 {
            return None;
        }
        let v = vertex_of_cell[cell_index(cx, cy, i as u32, j as u32, k as u32)];
        if v == u32::MAX {
            None
        } else {
            Some(v)
        }
    };

    for k in 0..cz {
        for j in 0..cy {
            for i in 0..cx {
                let mask = corner_mask(field, nx, ny, i, j, k);
                if mask == 0 || mask == 255 {
                    continue;
                }
                for (ca, cb) in ALL_EDGES.iter() {
                    let a_in = (mask >> ca) & 1 == 1;
                    let b_in = (mask >> cb) & 1 == 1;
                    if a_in == b_in {
                        continue;
                    }
                    let a = CORNER_OFFSET[*ca];
                    let b = CORNER_OFFSET[*cb];
                    // The edge runs along whichever axis its two corners differ
                    // on, and the four cells that share it all sit around the
                    // cell's own corner `a` in absolute lattice coordinates.
                    let axis = if a.0 != b.0 {
                        0
                    } else if a.1 != b.1 {
                        1
                    } else {
                        2
                    };
                    let cells = sharing_cells((i + a.0, j + a.1, k + a.2), axis);
                    // Only the lexicographically first cell emits the quad, so
                    // the four cells that share the edge never double it up.
                    let owner = cells
                        .iter()
                        .filter(|(x, y, z)| {
                            *x >= 0 && *y >= 0 && *z >= 0
                                && *x < cx as i32 && *y < cy as i32 && *z < cz as i32
                        })
                        .min_by_key(|(x, y, z)| (*x, *y, *z));
                    if owner != Some(&(i as i32, j as i32, k as i32)) {
                        continue;
                    }

                    let mut ring = [u32::MAX; 4];
                    let mut ok = true;
                    for (slot, cell) in cells.iter().enumerate() {
                        match at(cell.0, cell.1, cell.2) {
                            Some(v) => ring[slot] = v,
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        continue;
                    }
                    // Two triangles per quad, wound so the face normal agrees
                    // with the outward field gradient.
                    let (q0, q1, q2, q3) = (ring[0], ring[1], ring[2], ring[3]);
                    if q0 == q1 || q1 == q2 || q2 == q3 || q3 == q0 || q0 == q2 || q1 == q3 {
                        continue;
                    }
                    let outward = outward_of(&vertices, q0, q1, q2);
                    let (t0, t1) = if quad_faces_out(&vertices, [q0, q1, q2, q3], outward) {
                        ([q0, q1, q2], [q0, q2, q3])
                    } else {
                        ([q0, q2, q1], [q0, q3, q2])
                    };
                    let ci = (i / cv).min(grid[0] - 1);
                    let cj = (j / cv).min(grid[1] - 1);
                    let ck = (k / cv).min(grid[2] - 1);
                    let slot = ((ck * grid[1] + cj) * grid[0] + ci) as usize;
                    per_chunk[slot].extend_from_slice(&t0);
                    per_chunk[slot].extend_from_slice(&t1);
                }
            }
        }
    }

    let mut chunks = Vec::with_capacity(per_chunk.len());
    for ck in 0..grid[2] {
        for cj in 0..grid[1] {
            for ci in 0..grid[0] {
                let slot = ((ck * grid[1] + cj) * grid[0] + ci) as usize;
                if per_chunk[slot].is_empty() {
                    continue;
                }
                chunks.push(VolumeMeshChunk {
                    chunk: [ci, cj, ck],
                    indices: std::mem::take(&mut per_chunk[slot]),
                });
            }
        }
    }
    Ok(VolumeMesh { vertices, chunks, voxel_size, origin, resolution })
}

#[inline]
fn cell_index(cx: u32, cy: u32, i: u32, j: u32, k: u32) -> usize {
    (k * cy + j) as usize * cx as usize + i as usize
}

/// Average of the quad's vertex normals, which points away from the solid.
fn outward_of(vertices: &[MeshVertex], a: u32, b: u32, c: u32) -> DVec3 {
    let mut acc = DVec3::ZERO;
    for v in [a, b, c] {
        acc += vertices[v as usize].normal;
    }
    acc.normalize_or(DVec3::Z)
}

/// True when the quad's own normal agrees with the outward direction.
fn quad_faces_out(vertices: &[MeshVertex], quad: [u32; 4], outward: DVec3) -> bool {
    let p = |i: u32| vertices[i as usize].position;
    let n = (p(quad[1]) - p(quad[0])).cross(p(quad[2]) - p(quad[0]));
    if n.length_squared() < 1e-18 {
        return true;
    }
    n.normalize_or(DVec3::Z).dot(outward) > 0.0
}

/// Which of a cell's eight corners are inside the solid, as an 8-bit mask.
fn corner_mask(field: &[f32], nx: u32, ny: u32, i: u32, j: u32, k: u32) -> u8 {
    let mut mask = 0u8;
    for (c, (di, dj, dk)) in CORNER_OFFSET.iter().enumerate() {
        if field[idx(nx, ny, i + di, j + dj, k + dk)] < 0.0 {
            mask |= 1 << c;
        }
    }
    mask
}

/// The surface nets vertex of one cell: the average of the cell's edge crossings.
#[allow(clippy::too_many_arguments)]
fn cell_vertex(
    field: &[f32],
    nx: u32,
    ny: u32,
    i: u32,
    j: u32,
    k: u32,
    mask: u8,
    voxel_size: f64,
    origin: DVec3,
) -> MeshVertex {
    let mut acc = DVec3::ZERO;
    let mut count = 0.0f64;
    for (ca, cb) in ALL_EDGES.iter() {
        let a_in = (mask >> ca) & 1 == 1;
        let b_in = (mask >> cb) & 1 == 1;
        if a_in == b_in {
            continue;
        }
        let pa = CORNER_OFFSET[*ca];
        let pb = CORNER_OFFSET[*cb];
        let va = field[idx(nx, ny, i + pa.0, j + pa.1, k + pa.2)];
        let vb = field[idx(nx, ny, i + pb.0, j + pb.1, k + pb.2)];
        let denom = va - vb;
        let t = if denom.abs() > 1e-20 { (va as f64 / denom as f64).clamp(0.0, 1.0) } else { 0.5 };
        let a = DVec3::new(pa.0 as f64, pa.1 as f64, pa.2 as f64);
        let b = DVec3::new(pb.0 as f64, pb.1 as f64, pb.2 as f64);
        acc += a.lerp(b, t);
        count += 1.0;
    }
    let local = if count > 0.0 { acc / count } else { DVec3::splat(0.5) };
    let position = origin + (DVec3::new(i as f64, j as f64, k as f64) + local) * voxel_size;
    let value = sample_field(field, nx, ny, position, origin, voxel_size);
    // The field grows as it leaves the solid, so its gradient is already the
    // outward surface normal.
    let h = voxel_size * 0.5;
    let diff = |axis: DVec3| {
        f64::from(
            sample_field(field, nx, ny, position + axis * h, origin, voxel_size)
                - sample_field(field, nx, ny, position - axis * h, origin, voxel_size),
        )
    };
    let g = DVec3::new(diff(DVec3::X), diff(DVec3::Y), diff(DVec3::Z));
    let normal = g.normalize_or(DVec3::Z);
    MeshVertex { position, normal, value }
}

/// Trilinear field value at an object-space position.
fn sample_field(
    field: &[f32],
    nx: u32,
    ny: u32,
    p: DVec3,
    origin: DVec3,
    voxel_size: f64,
) -> f32 {
    let nz = (field.len() / (nx as usize * ny as usize).max(1)) as u32;
    let g = (p - origin) / voxel_size;
    // Clamp the last sample to the final lattice point, then clamp the upper
    // tap, so a position on the far face reads the last column exactly.
    let x = math::clamp(g.x, 0.0, (nx - 1) as f64);
    let y = math::clamp(g.y, 0.0, (ny - 1) as f64);
    let z = math::clamp(g.z, 0.0, (nz - 1) as f64);
    let (i, j, k) = (x.floor() as u32, y.floor() as u32, z.floor() as u32);
    let (fx, fy, fz) = (x - i as f64, y - j as f64, z - k as f64);
    let (i1, j1, k1) = ((i + 1).min(nx - 1), (j + 1).min(ny - 1), (k + 1).min(nz - 1));
    let (w000, w100) = (1.0 - fx, fx);
    let (w010, w110) = (1.0 - fy, fy);
    let (w001, w101) = (1.0 - fz, fz);
    let tap = |ii: u32, jj: u32, kk: u32| f64::from(field[idx(nx, ny, ii, jj, kk)]);
    let c00 = tap(i, j, k) * w000 + tap(i1, j, k) * w100;
    let c10 = tap(i, j1, k) * w000 + tap(i1, j1, k) * w100;
    let c01 = tap(i, j, k1) * w000 + tap(i1, j, k1) * w100;
    let c11 = tap(i, j1, k1) * w000 + tap(i1, j1, k1) * w100;
    let c0 = c00 * w010 + c10 * w110;
    let c1 = c01 * w010 + c11 * w110;
    (c0 * w001 + c1 * w101) as f32
}

/// One chunk's triangles, as vertex indices.
pub fn chunk_triangles(chunk: &VolumeMeshChunk) -> Vec<[u32; 3]> {
    chunk.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect()
}

/// Weld vertices whose positions lie within `tolerance` of each other and drop
/// degenerate triangles. A seam check needs this before it can trust the vertex
/// count it sees across chunk borders.
pub fn weld(mesh: &VolumeMesh, tolerance: f32) -> (Vec<MeshVertex>, Vec<u32>) {
    let tol = if tolerance > 0.0 { tolerance } else { 1e-4 };
    let cell = f64::from(tol.max(1e-6));
    let mut remap = vec![u32::MAX; mesh.vertices.len()];
    let mut out: Vec<MeshVertex> = Vec::with_capacity(mesh.vertices.len());
    // A hash grid on the quantised position keeps the neighbour search local.
    let mut buckets: std::collections::HashMap<(i64, i64, i64), Vec<u32>> = std::collections::HashMap::new();
    for (i, v) in mesh.vertices.iter().enumerate() {
        let q = (
            (v.position.x / cell).round() as i64,
            (v.position.y / cell).round() as i64,
            (v.position.z / cell).round() as i64,
        );
        let mut found: Option<u32> = None;
        'search: for dz in -1..=1i64 {
            for dy in -1..=1i64 {
                for dx in -1..=1i64 {
                    let Some(list) = buckets.get(&(q.0 + dx, q.1 + dy, q.2 + dz)) else { continue };
                    for cand in list {
                        let c = out[*cand as usize].position;
                        if (c - v.position).length() <= f64::from(tol) {
                            found = Some(*cand);
                            break 'search;
                        }
                    }
                }
            }
        }
        match found {
            Some(target) => remap[i] = target,
            None => {
                let id = out.len() as u32;
                out.push(*v);
                buckets.entry(q).or_default().push(id);
                remap[i] = id;
            }
        }
    }
    let mut indices = Vec::new();
    for t in mesh.triangles() {
        let a = remap[t[0] as usize];
        let b = remap[t[1] as usize];
        let c = remap[t[2] as usize];
        if a == b || b == c || a == c {
            continue;
        }
        indices.extend_from_slice(&[a, b, c]);
    }
    (out, indices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdf::{SdfOp, SdfShape};

    /// A sphere cut by the plane through its centre, so the solid is a closed
    /// hemisphere with a flat disc across the middle.
    fn hemisphere(res: u32) -> VolumeMesh {
        let vt = VolumeTerrain {
            chunks: [res, res, res],
            voxel_size: 1.0,
            origin: DVec3::splat(-(res as f64) * 0.5),
            ops: vec![
                SdfOp::Add {
                    shape: SdfShape::Sphere { center: DVec3::ZERO, radius: res as f64 * 0.3 },
                },
                SdfOp::Intersect {
                    shape: SdfShape::HalfSpace { point: DVec3::ZERO, normal: DVec3::Z },
                    k: 0.0,
                },
            ],
        };
        mesh(&vt).unwrap()
    }

    #[test]
    fn sdf_mesh_watertight() {
        let m = hemisphere(24);
        assert!(!m.vertices.is_empty());
        assert!(m.triangle_count() > 100);
        let counts = m.edge_use_counts();
        let bad: Vec<_> = counts.iter().filter(|(_, v)| **v != 2).collect();
        assert!(bad.is_empty(), "{} edges are not shared by exactly two triangles: {bad:?}", bad.len());
    }

    #[test]
    fn a_cube_carved_with_a_torus_hole_is_watertight() {
        let res = 28u32;
        // A rotated box reaches `half * sqrt(3)` from its centre, so the half
        // extent has to stay well inside the field or the surface is clipped by
        // the grid and cannot close.
        let half = res as f64 * 0.25;
        let vt = VolumeTerrain {
            chunks: [res, res, res],
            voxel_size: 1.0,
            origin: DVec3::splat(-(res as f64) * 0.5),
            ops: vec![
                SdfOp::Add {
                    shape: SdfShape::Box {
                        center: DVec3::ZERO,
                        half: DVec3::splat(half),
                        rotation: [12.0, 34.0, 5.0],
                    },
                },
                SdfOp::Subtract {
                    shape: SdfShape::Cylinder {
                        base: DVec3::new(0.0, 0.0, -(res as f64) * 0.5),
                        height: res as f64,
                        radius: res as f64 * 0.15,
                    },
                    k: 0.0,
                },
            ],
        };
        let m = mesh(&vt).unwrap();
        let counts = m.edge_use_counts();
        // No edge may be left open: a crack would show as a hole in the shell.
        let open = counts.values().filter(|v| **v < 2).count();
        assert_eq!(open, 0, "{open} edges are not shared by two triangles");
        // A bilinear isosurface through a saddle can leave a pinch where two
        // quads meet on the same vertex pair, so a handful of over-shared edges
        // are a property of the surface rather than a crack in the mesh.
        let pinched = counts.values().filter(|v| **v > 2).count();
        assert!(
            pinched * 200 <= counts.len(),
            "{pinched} of {} edges are over-shared", counts.len()
        );
    }

    #[test]
    fn a_torus_is_closed() {
        let res = 24u32;
        let vt = VolumeTerrain {
            chunks: [res, res, res],
            voxel_size: 1.0,
            origin: DVec3::splat(-(res as f64) * 0.5),
            ops: vec![SdfOp::Add {
                shape: SdfShape::Torus { center: DVec3::ZERO, major: 5.0, minor: 2.0 },
            }],
        };
        let m = mesh(&vt).unwrap();
        let counts = m.edge_use_counts();
        let bad = counts.iter().filter(|(_, v)| **v != 2).count();
        assert_eq!(bad, 0, "{bad} edges are not shared by exactly two triangles");
    }


    #[test]
    fn every_triangle_has_three_distinct_vertices() {
        let m = hemisphere(16);
        for t in m.triangles() {
            assert!(t[0] < m.vertices.len() as u32);
            assert!(t[0] != t[1] && t[1] != t[2] && t[0] != t[2], "degenerate triangle {t:?}");
        }
    }

    #[test]
    fn chunk_indices_are_in_range_and_every_vertex_is_used() {
        let m = hemisphere(24);
        assert!(!m.chunks.is_empty());
        for c in &m.chunks {
            assert_eq!(c.indices.len() % 3, 0);
            for i in &c.indices {
                assert!(*i < m.vertices.len() as u32, "chunk references vertex {i}");
            }
        }
        let tris = m.triangles();
        assert_eq!(tris.len(), m.triangle_count());
        for (i, _) in m.vertices.iter().enumerate() {
            assert!(tris.iter().any(|t| t.contains(&(i as u32))), "vertex {i} is unreferenced");
        }
        // Chunk coordinates stay inside the chunk grid.
        let cv = VolumeTerrain::CHUNK_VOXELS;
        for c in &m.chunks {
            assert!(c.chunk[0] * cv < m.resolution[0]);
            assert!(c.chunk[1] * cv < m.resolution[1]);
            assert!(c.chunk[2] * cv < m.resolution[2]);
        }
    }

    #[test]
    fn shared_chunk_borders_produce_a_watertight_surface() {
        // A fine field split into many chunks: the whole surface must still be
        // closed, which is only true if the vertex table is global.
        let res = 32u32;
        let m = hemisphere(res);
        assert!(m.chunks.len() > 1, "the test needs more than one chunk");
        let counts = m.edge_use_counts();
        assert!(counts.values().all(|v| *v == 2), "chunk seams left an open edge");
    }

    #[test]
    fn normals_point_outwards_and_are_unit_length() {
        let res = 20u32;
        let vt = VolumeTerrain {
            chunks: [res, res, res],
            voxel_size: 1.0,
            origin: DVec3::splat(-(res as f64) * 0.5),
            ops: vec![SdfOp::Add {
                shape: SdfShape::Sphere { center: DVec3::ZERO, radius: res as f64 * 0.3 },
            }],
        };
        let m = mesh(&vt).unwrap();
        assert!(!m.vertices.is_empty());
        for v in &m.vertices {
            assert!(v.normal.is_finite());
            assert!((v.normal.length() - 1.0).abs() < 1e-6, "normal {} is not unit", v.normal.length());
            let outward = v.position.normalize_or(DVec3::Z);
            assert!(v.normal.dot(outward) > 0.0, "normal points inwards at {:?}", v.position);
            assert!(v.value.is_finite());
        }
    }

    #[test]
    fn triangle_winding_agrees_with_the_vertex_normals() {
        let m = hemisphere(20);
        for t in m.triangles() {
            let (a, b, c) = (
                m.vertices[t[0] as usize],
                m.vertices[t[1] as usize],
                m.vertices[t[2] as usize],
            );
            let n = (b.position - a.position).cross(c.position - a.position);
            let outward = (a.normal + b.normal + c.normal).normalize_or(DVec3::Z);
            assert!(n.length() > 1e-9, "degenerate triangle {t:?}");
            assert!(n.normalize_or(DVec3::Z).dot(outward) > 0.0, "triangle {t:?} is wound inwards");
        }
    }

    #[test]
    fn mesh_field_rejects_bad_input() {
        let field = vec![1.0f32; 8];
        assert!(mesh_field(&field, [2, 2, 2], 1.0, DVec3::ZERO).is_ok());
        assert!(matches!(
            mesh_field(&field[..3], [2, 2, 2], 1.0, DVec3::ZERO),
            Err(Error::SizeMismatch(..))
        ));
        assert!(matches!(
            mesh_field(&field, [1, 2, 2], 1.0, DVec3::ZERO),
            Err(Error::SdfTooSmall(..))
        ));
        assert!(matches!(mesh_field(&field, [2, 2, 2], 0.0, DVec3::ZERO), Err(Error::InvalidVoxelSize(_))));
        assert!(matches!(mesh_field(&field, [2, 2, 2], f64::NAN, DVec3::ZERO), Err(Error::InvalidVoxelSize(_))));
        assert!(mesh_field(&field, [2, 2, 2], 1.0, DVec3::splat(f64::NAN)).is_err());
    }

    #[test]
    fn an_empty_field_meshes_to_nothing() {
        let field = vec![1.0f32; 4 * 4 * 4];
        let m = mesh_field(&field, [4, 4, 4], 1.0, DVec3::ZERO).unwrap();
        assert!(m.vertices.is_empty());
        assert!(m.chunks.is_empty());
        assert_eq!(m.triangle_count(), 0);
        assert_eq!(m.bounds(), (DVec3::ZERO, DVec3::ZERO));
    }

    #[test]
    fn a_fully_solid_field_has_no_surface() {
        let field = vec![-1.0f32; 4 * 4 * 4];
        let m = mesh_field(&field, [4, 4, 4], 1.0, DVec3::ZERO).unwrap();
        assert!(m.vertices.is_empty());
    }

    #[test]
    fn welding_merges_coincident_vertices() {
        let m = hemisphere(20);
        let (verts, indices) = weld(&m, 1e-3);
        assert!(!verts.is_empty());
        assert_eq!(indices.len() % 3, 0);
        for t in indices.chunks_exact(3) {
            assert!(t[0] != t[1] && t[1] != t[2] && t[0] != t[2]);
            assert!((t[2] as usize) < verts.len());
        }
        // Welding a mesh that is already welded changes nothing.
        let already = VolumeMesh {
            vertices: verts.clone(),
            chunks: vec![VolumeMeshChunk {
                chunk: [0, 0, 0],
                indices: indices.clone(),
            }],
            voxel_size: 1.0,
            origin: DVec3::ZERO,
            resolution: [2, 2, 2],
        };
        let (v2, i2) = weld(&already, 1e-3);
        assert_eq!(v2.len(), verts.len());
        assert_eq!(i2, indices);
        // A zero tolerance still produces a valid index list.
        let (v0, i0) = weld(&m, 0.0);
        assert_eq!(v0.len(), verts.len());
        assert_eq!(i0, indices);
    }

    #[test]
    fn corner_masks_classify_a_known_cell() {
        let mut field = vec![1.0f32; 8];
        field[0] = -1.0;
        assert_eq!(corner_mask(&field, 2, 2, 0, 0, 0), 0b0000_0001);
        assert_eq!(corner_mask(&vec![-1.0f32; 8], 2, 2, 0, 0, 0), 255);
        assert_eq!(corner_mask(&vec![1.0f32; 8], 2, 2, 0, 0, 0), 0);
    }

    #[test]
    fn chunk_triangles_helper_matches_the_flat_list() {
        let m = hemisphere(16);
        let total: usize = m.chunks.iter().map(chunk_triangles).map(|v| v.len()).sum();
        assert_eq!(total, m.triangle_count());
    }

    #[test]
    fn sampling_is_clamped_and_interpolates() {
        // A field that ramps from 0 to 1 along X samples linearly.
        let (nx, ny, nz) = (3u32, 3u32, 3u32);
        let mut field = vec![0.0f32; (nx * ny * nz) as usize];
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    field[idx(nx, ny, i, j, k)] = i as f32 / (nx - 1) as f32;
                }
            }
        }
        assert_eq!(sample_field(&field, nx, ny, DVec3::new(-5.0, 1.0, 1.0), DVec3::ZERO, 1.0), 0.0);
        assert_eq!(sample_field(&field, nx, ny, DVec3::new(99.0, 1.0, 1.0), DVec3::ZERO, 1.0), 1.0);
        let mid = sample_field(&field, nx, ny, DVec3::new(1.0, 1.0, 1.0), DVec3::ZERO, 1.0);
        assert!((mid - 0.5).abs() < 1e-5, "midpoint sample {mid}");
        // Off the ramp axis every sample is the same, so Y and Z are flat.
        let y = sample_field(&field, nx, ny, DVec3::new(2.0, 0.5, 1.0), DVec3::ZERO, 1.0);
        assert!((y - 1.0).abs() < 1e-5, "y sample {y}");
    }
}
