//! Viewport wireframe helpers (phase M3): triangle-soup edges with coplanar
//! diagonals suppressed, brush polygon outlines, and the global line-budget
//! LOD selection. Pure Rust, no GPU, deterministic across builds.

use std::collections::BTreeMap;

use glam::DVec3;

use super::{bad, EditError, MAX_COORD, MAX_FACES, MAX_VERTICES, WELD_EPS};
use crate::model::Brush;

/// Global cap on wireframe segments drawn in one frame.
pub const WIRE_LINE_BUDGET: usize = 200_000;
/// Two face normals closer than this are one plane.
const COPLANAR_DOT: f64 = 0.9999;
/// Weld grid for soup positions, as in `WELD_EPS`.
const WELD_SCALE: f64 = 1.0 / WELD_EPS;

/// Welded positions plus the wire edges over them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WireSoup {
    pub positions: Vec<[f32; 3]>,
    pub edges: Vec<[u32; 2]>,
}

fn finite_in_range(v: f32) -> bool {
    v.is_finite() && (v as f64).abs() <= MAX_COORD
}

/// Canonical vertex table of a soup, welded within `WELD_EPS`. Ids run in
/// first-seen order, so the same input always maps to the same table.
fn weld(positions: &[[f32; 3]]) -> Result<(Vec<[f32; 3]>, Vec<u32>), EditError> {
    if positions.len() > MAX_VERTICES {
        return Err(bad("the mesh is larger than the mesh budget"));
    }
    let mut table: Vec<[f32; 3]> = Vec::new();
    let mut seen: BTreeMap<[i64; 3], u32> = BTreeMap::new();
    let mut remap = Vec::with_capacity(positions.len());
    for p in positions {
        if !finite_in_range(p[0]) || !finite_in_range(p[1]) || !finite_in_range(p[2]) {
            return Err(bad("the mesh carries a non-finite coordinate"));
        }
        let key = [
            ((p[0] as f64 * WELD_SCALE).round()) as i64,
            ((p[1] as f64 * WELD_SCALE).round()) as i64,
            ((p[2] as f64 * WELD_SCALE).round()) as i64,
        ];
        let id = match seen.get(&key) {
            Some(id) => *id,
            None => {
                let id = table.len() as u32;
                table.push(*p);
                seen.insert(key, id);
                id
            }
        };
        remap.push(id);
    }
    Ok((table, remap))
}

fn tri_normal(a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    (b - a).cross(c - a)
}

/// Triangle edges with coplanar diagonals removed: a box soup yields its 12
/// edges, a subdivided plane its grid lines. An interior edge shared by two
/// coplanar triangles is a triangulation diagonal exactly when it is the
/// longest edge of at least one of them (a quad side is always shorter than
/// the quad's diagonal through the same triangle, while a grid line between
/// two quads is a leg of both). Boundary and non-manifold edges are kept.
pub fn soup_wire_edges(positions: &[[f32; 3]], indices: &[u32]) -> Result<WireSoup, EditError> {
    if !indices.len().is_multiple_of(3) {
        return Err(bad("the triangle list is not a multiple of three"));
    }
    if indices.len() / 3 > MAX_FACES {
        return Err(bad("the mesh is larger than the mesh budget"));
    }
    let (table, remap) = weld(positions)?;
    let pts: Vec<DVec3> =
        table.iter().map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]);
        if a == b || b == c || c == a {
            continue;
        }
        tris.push([a, b, c]);
    }
    let mut normals: Vec<DVec3> = Vec::with_capacity(tris.len());
    let mut kept: Vec<[u32; 3]> = Vec::new();
    for t in &tris {
        let n = tri_normal(pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]);
        if n.length_squared() < super::DEGENERATE_AREA * super::DEGENERATE_AREA * 4.0 {
            continue;
        }
        normals.push(n.normalize());
        kept.push(*t);
    }
    let mut users: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
    for (ti, t) in kept.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let key = if a < b { (a, b) } else { (b, a) };
            users.entry(key).or_default().push(ti);
        }
    }
    let mut edges: Vec<[u32; 2]> = Vec::new();
    for ((a, b), list) in &users {
        if list.len() != 2 {
            edges.push([*a, *b]);
            continue;
        }
        let (t0, t1) = (kept[list[0]], kept[list[1]]);
        if normals[list[0]].dot(normals[list[1]]) < COPLANAR_DOT {
            edges.push([*a, *b]);
            continue;
        }
        if longest_of_either(&pts, t0, (*a, *b)) || longest_of_either(&pts, t1, (*a, *b)) {
            continue;
        }
        edges.push([*a, *b]);
    }
    Ok(WireSoup { positions: table, edges })
}

/// True when the shared edge is at least as long as the triangle's other two.
fn longest_of_either(pts: &[DVec3], t: [u32; 3], shared: (u32, u32)) -> bool {
    let len2 = |a: u32, b: u32| (pts[a as usize] - pts[b as usize]).length_squared();
    let shared2 = len2(shared.0, shared.1);
    let mut other = 1.0f64;
    let mut first = true;
    for k in 0..3 {
        let (a, b) = (t[k], t[(k + 1) % 3]);
        if (a == shared.0 && b == shared.1) || (a == shared.1 && b == shared.0) {
            continue;
        }
        let l = len2(a, b);
        if first || l > other {
            other = l;
            first = false;
        }
    }
    shared2 + 1e-12 >= other
}

/// Polygon outlines of one brush: every face loop as consecutive pairs over
/// the solid's own vertex table.
pub fn brush_outline(brush: &Brush) -> Result<WireSoup, EditError> {
    let solid = crate::brush::Solid::from_faces(&brush.faces)
        .map_err(|e| bad(format!("the brush has no usable solid: {e}")))?;
    if solid.vertices.len() > MAX_VERTICES {
        return Err(bad("the mesh is larger than the mesh budget"));
    }
    let positions: Vec<[f32; 3]> =
        solid.vertices.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect();
    let mut edges: Vec<[u32; 2]> = Vec::new();
    for ring in &solid.loops {
        for k in 0..ring.len() {
            let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
            if a == b {
                continue;
            }
            edges.push(if a < b { [a, b] } else { [b, a] });
        }
    }
    edges.sort_unstable();
    edges.dedup();
    Ok(WireSoup { positions, edges })
}

/// Nearest-first line selection under a global budget: segment indices sorted
/// by `(distance, index)`, truncated to `budget`. Non-finite distances sort
/// last, so bad input drops lines instead of panicking.
pub fn select_lines(distances: &[f64], budget: usize) -> Vec<u32> {
    let mut order: Vec<u32> = (0..distances.len() as u32).collect();
    order.sort_by(|a, b| {
        let (da, db) = (distances[*a as usize], distances[*b as usize]);
        match (da.is_finite(), db.is_finite()) {
            (true, true) => da.total_cmp(&db).then_with(|| a.cmp(b)),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => a.cmp(b),
        }
    });
    order.truncate(budget);
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editmesh::convert::weld_primitives;
    use crate::import::glb::GlbMesh;
    use crate::meshgen::{build, MeshSpec};

    fn soup_positions(spec: &MeshSpec) -> (Vec<[f32; 3]>, Vec<u32>) {
        let g = build(spec, "t", "m", 1.0 / 512.0).unwrap();
        (g.positions, g.indices)
    }

    #[test]
    fn box_soup_yields_twelve_edges() {
        let (positions, indices) = soup_positions(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
        let wire = soup_wire_edges(&positions, &indices).unwrap();
        assert_eq!(wire.edges.len(), 12);
        let mut sorted = wire.edges.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 12);
    }

    #[test]
    fn subdivided_plane_yields_grid_lines_only() {
        let n = 4u32;
        let mut positions: Vec<[f32; 3]> = Vec::new();
        for y in 0..=n {
            for x in 0..=n {
                positions.push([x as f32 * 16.0, y as f32 * 16.0, 0.0]);
            }
        }
        let at = |x: u32, y: u32| y * (n + 1) + x;
        let mut indices: Vec<u32> = Vec::new();
        for y in 0..n {
            for x in 0..n {
                let (a, b, c, d) = (at(x, y), at(x + 1, y), at(x + 1, y + 1), at(x, y + 1));
                indices.extend([a, b, c, a, c, d]);
            }
        }
        let wire = soup_wire_edges(&positions, &indices).unwrap();
        let grid = 2 * n * (n + 1);
        assert_eq!(wire.edges.len(), grid as usize);
    }

    #[test]
    fn folded_edge_is_kept() {
        let positions =
            vec![[0.0, 0.0, 0.0], [16.0, 0.0, 0.0], [16.0, 16.0, 0.0], [16.0, 0.0, 16.0]];
        let indices = vec![0, 1, 2, 1, 3, 2];
        let wire = soup_wire_edges(&positions, &indices).unwrap();
        assert_eq!(wire.edges.len(), 5);
    }

    #[test]
    fn soup_rejects_bad_input_without_panicking() {
        assert!(soup_wire_edges(&[[0.0, 0.0, 0.0]], &[0, 1]).is_err());
        assert!(soup_wire_edges(&[[f32::NAN, 0.0, 0.0]], &[0, 0, 0]).is_err());
        let big = vec![[0.0, 0.0, 0.0]; MAX_VERTICES + 1];
        assert!(soup_wire_edges(&big, &[]).is_err());
    }

    #[test]
    fn welded_box_matches_soup_box() {
        let g = build(&MeshSpec::Box { size: [64.0, 64.0, 64.0] }, "t", "m", 1.0 / 512.0).unwrap();
        let mesh = weld_primitives(&[GlbMesh { colors: Vec::new(),
            name: g.name,
            material: g.material,
            positions: g.positions,
            normals: g.normals,
            uvs: g.uvs,
            indices: g.indices,
        }])
        .unwrap();
        let eval = super::super::eval::evaluate(&mesh);
        assert_eq!(eval.poly_edges.len(), 18);
        let (positions, indices) = soup_positions(&MeshSpec::Box { size: [64.0, 64.0, 64.0] });
        assert_eq!(soup_wire_edges(&positions, &indices).unwrap().edges.len(), 12);
    }

    #[test]
    fn lod_keeps_nearest_under_budget() {
        assert_eq!(select_lines(&[9.0, 1.0, 4.0, 16.0], 2), vec![1, 2]);
        assert_eq!(select_lines(&[9.0, 1.0, 4.0, 16.0], 0), Vec::<u32>::new());
        assert_eq!(select_lines(&[3.0, 1.0, 2.0], 99), vec![1, 2, 0]);
    }

    #[test]
    fn lod_ties_break_by_index_and_defers_non_finite() {
        assert_eq!(select_lines(&[5.0, 5.0, 5.0], 2), vec![0, 1]);
        assert_eq!(select_lines(&[f64::NAN, 1.0, f64::INFINITY], 2), vec![1, 0]);
    }
}
