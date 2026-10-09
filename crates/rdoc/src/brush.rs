//! Convex brush geometry, UV mapping and the brush editing operations.
//!
//! A `Solid` is a convex polyhedron stored as its face planes; face polygons,
//! volume and mesh buffers are derived from those planes in double precision.

use glam::{DMat3, DVec3};
use thiserror::Error;

pub use crate::model::{Brush, BrushFace, UvMapping};

/// A point is on a plane when `|n.p - d|` is below this.
pub const PLANE_EPS: f64 = 1e-6;
/// Two computed vertices closer than this are the same point.
pub const VERTEX_EPS: f64 = 1e-4;
/// Brushes thinner than this in any axis are degenerate.
pub const DEGENERATE_EPS: f64 = 1e-3;

#[derive(Debug, Error, PartialEq)]
pub enum BrushError {
    #[error("brush has no planes")]
    Empty,
    #[error("plane normal is not usable")]
    BadNormal,
    #[error("plane points are collinear")]
    CollinearPoints,
    #[error("brush is degenerate or not closed")]
    NotConvex,
    #[error("face index {0} is out of range")]
    FaceIndex(usize),
    #[error("vertex {0} is not on the brush")]
    UnknownVertex(usize),
    #[error("no geometry remains after the operation")]
    EmptyResult,
    #[error("merging these brushes does not produce a convex hull")]
    NotConvexHull,
}

/// `normal . p = dist`, normal pointing out of the solid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub normal: DVec3,
    pub dist: f64,
}

impl Plane {
    pub fn new(normal: DVec3, dist: f64) -> Result<Self, BrushError> {
        if !normal.is_finite() || !dist.is_finite() {
            return Err(BrushError::BadNormal);
        }
        let len = normal.length();
        if len <= PLANE_EPS {
            return Err(BrushError::BadNormal);
        }
        Ok(Self { normal: normal / len, dist })
    }

    /// Plane through three points, with the normal facing away from `interior`.
    pub fn from_points(a: DVec3, b: DVec3, c: DVec3, interior: DVec3) -> Result<Self, BrushError> {
        if !a.is_finite() || !b.is_finite() || !c.is_finite() || !interior.is_finite() {
            return Err(BrushError::CollinearPoints);
        }
        let mut n = (b - a).cross(c - a);
        if n.length_squared() <= PLANE_EPS * PLANE_EPS {
            return Err(BrushError::CollinearPoints);
        }
        n = n.normalize();
        if n.dot(interior) > n.dot(a) {
            n = -n;
        }
        Ok(Self { normal: n, dist: n.dot(a) })
    }

    pub fn distance(&self, p: DVec3) -> f64 {
        self.normal.dot(p) - self.dist
    }

    pub fn contains(&self, p: DVec3) -> bool {
        self.distance(p) <= PLANE_EPS
    }

    pub fn flipped(self) -> Self {
        Self { normal: -self.normal, dist: -self.dist }
    }

    /// Moves the plane along its own normal.
    pub fn offset(self, by: f64) -> Self {
        Self { normal: self.normal, dist: self.dist + by }
    }
}

impl From<&BrushFace> for Plane {
    fn from(f: &BrushFace) -> Self {
        Self { normal: f.normal, dist: f.dist }
    }
}


/// A convex polyhedron: face planes plus the exact polygon of each face.
#[derive(Clone, Debug, PartialEq)]
pub struct Solid {
    pub planes: Vec<Plane>,
    /// Indices into `vertices`, counter-clockwise seen from outside.
    pub loops: Vec<Vec<u32>>,
    pub vertices: Vec<DVec3>,
}

impl Solid {
    /// Builds the solid from half-spaces, dropping planes that bound nothing.
    pub fn from_planes(planes: Vec<Plane>) -> Result<Self, BrushError> {
        if planes.is_empty() {
            return Err(BrushError::Empty);
        }
        let mut unique: Vec<Plane> = Vec::with_capacity(planes.len());
        for p in planes {
            let p = Plane::new(p.normal, p.dist)?;
            let dup = unique.iter().any(|q| {
                (q.normal.dot(p.normal) > 1.0 - 1e-9) && (q.dist - p.dist).abs() <= PLANE_EPS
            });
            if !dup {
                unique.push(p);
            }
        }

        let inside: Vec<DVec3> = triple_intersections(&unique)
            .into_iter()
            .filter(|p| unique.iter().all(|q| q.distance(*p) <= PLANE_EPS))
            .collect();
        let mut vertices: Vec<DVec3> = Vec::with_capacity(inside.len());
        for p in inside {
            merge_vertex(&mut vertices, p);
        }
        if vertices.len() < 4 {
            return Err(BrushError::NotConvex);
        }

        let mut kept: Vec<(Plane, Vec<u32>)> = Vec::new();
        for p in &unique {
            let ring: Vec<u32> = vertices
                .iter()
                .enumerate()
                .filter(|(_, v)| p.distance(**v).abs() <= PLANE_EPS)
                .map(|(i, _)| i as u32)
                .collect();
            if ring.len() < 3 {
                continue;
            }
            kept.push((*p, order_loop(&vertices, &ring, p.normal)?));
        }
        if kept.len() < 4 {
            return Err(BrushError::NotConvex);
        }
        let (planes, loops): (Vec<Plane>, Vec<Vec<u32>>) = kept.into_iter().unzip();
        let solid = Self { planes, loops, vertices };
        if solid.volume() <= DEGENERATE_EPS {
            return Err(BrushError::NotConvex);
        }
        Ok(solid)
    }

    pub fn from_faces(faces: &[BrushFace]) -> Result<Self, BrushError> {
        let planes: Vec<Plane> =
            faces.iter().map(|f| Plane::new(f.normal, f.dist)).collect::<Result<_, _>>()?;
        Self::from_planes(planes)
    }

    /// Face list for this solid, keeping the material and UV of the template
    /// face that matches each plane. A template plane stored with the opposite
    /// normal still matches, so a brush that had to be flipped keeps its
    /// materials and UVs.
    pub fn to_faces(&self, template: &[BrushFace]) -> Vec<BrushFace> {
        self.planes
            .iter()
            .map(|p| {
                let same = template.iter().find(|f| {
                    f.normal.dot(p.normal) > 1.0 - 1e-9 && (f.dist - p.dist).abs() <= PLANE_EPS
                });
                let opposite = template.iter().find(|f| {
                    f.normal.dot(p.normal) < -1.0 + 1e-9 && (f.dist + p.dist).abs() <= PLANE_EPS
                });
                // A plane rebuilt from a thin sliver face drifts past the exact
                // match above; the template plane it came from is still the
                // nearest one.
                let nearest = || {
                    template
                        .iter()
                        .map(|f| (f, f.normal.dot(p.normal), (f.dist - p.dist).abs()))
                        .filter(|(_, d, gap)| *d > 0.999 && *gap <= 0.5)
                        .max_by(|a, b| (a.1 - a.2).total_cmp(&(b.1 - b.2)))
                        .map(|(f, _, _)| f)
                };
                let mut face = match same.or(opposite).or_else(nearest) {
                    Some(f) => f.clone(),
                    None => BrushFace {
                        material: String::new(),
                        uv: UvMapping::default(),
                        lightmap_scale: 0.0,
                        ..BrushFace::default()
                    },
                };
                face.normal = p.normal;
                face.dist = p.dist;
                face
            })
            .collect()
    }

    pub fn face_centroid(&self, face: usize) -> DVec3 {
        self.loop_points(face).iter().copied().sum::<DVec3>() / self.loops[face].len() as f64
    }

    pub fn loop_points(&self, face: usize) -> Vec<DVec3> {
        self.loops[face].iter().map(|i| self.vertices[*i as usize]).collect()
    }

    pub fn face_area(&self, face: usize) -> f64 {
        let pts = self.loop_points(face);
        // `polygon_normal` is already Newell's 2 * area * unit normal halved to
        // area * unit normal, so the dot product is the face area.
        polygon_normal(&pts).dot(self.planes[face].normal).abs()
    }

    /// Signed volume by the divergence theorem; positive for outward normals.
    pub fn volume(&self) -> f64 {
        let mut v = 0.0;
        for (i, p) in self.planes.iter().enumerate() {
            v += p.normal.dot(self.face_centroid(i)) * self.face_area(i);
        }
        v / 3.0
    }


    /// Every directed edge of the surface must have exactly one opposite twin.
    pub fn is_closed(&self) -> bool {
        use std::collections::HashMap;
        let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
        for loop_ in &self.loops {
            for i in 0..loop_.len() {
                let a = loop_[i];
                let b = loop_[(i + 1) % loop_.len()];
                *edges.entry((a, b)).or_insert(0) += 1;
            }
        }
        edges.iter().all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)).copied() == Some(1))
    }

    /// Closed, convex and thick enough to have a volume.
    pub fn is_valid(&self) -> bool {
        self.planes.len() >= 4
            && self.loops.len() == self.planes.len()
            && self.is_closed()
            && self.volume() > DEGENERATE_EPS
    }

    /// Applies a linear transform, re-deriving planes from the moved polygons.
    pub fn transformed(&self, m: DMat3) -> Result<Self, BrushError> {
        let pts: Vec<DVec3> = self.vertices.iter().map(|p| m * *p).collect();
        let mut planes = Vec::with_capacity(self.planes.len());
        for (i, p) in self.planes.iter().enumerate() {
            let moved: Vec<DVec3> = self.loops[i].iter().map(|v| pts[*v as usize]).collect();
            let n = polygon_normal(&moved);
            if n.length_squared() <= PLANE_EPS {
                return Err(BrushError::NotConvex);
            }
            let mut n = n.normalize();
            if n.dot(p.normal) < 0.0 {
                n = -n;
            }
            let c = moved.iter().copied().sum::<DVec3>() / moved.len() as f64;
            planes.push(Plane { normal: n, dist: n.dot(c) });
        }
        Self::from_planes(planes)
    }
}

fn polygon_normal(pts: &[DVec3]) -> DVec3 {
    let mut n = DVec3::ZERO;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n * 0.5
}

fn merge_vertex(vertices: &mut Vec<DVec3>, p: DVec3) {
    for v in vertices.iter() {
        if (*v - p).length() <= VERTEX_EPS {
            return;
        }
    }
    vertices.push(p);
}

fn triple_intersections(planes: &[Plane]) -> Vec<DVec3> {
    let mut out = Vec::new();
    for i in 0..planes.len() {
        for j in (i + 1)..planes.len() {
            let a = planes[i].normal;
            let b = planes[j].normal;
            let c = a.cross(b);
            if c.length_squared() <= PLANE_EPS * PLANE_EPS {
                continue;
            }
            for k in (j + 1)..planes.len() {
                let n3 = planes[k].normal;
                let denom = c.dot(n3);
                if denom.abs() <= 1e-9 {
                    continue;
                }
                // Cramer's rule for the three-plane system.
                let p = (b.cross(n3) * planes[i].dist
                    + n3.cross(a) * planes[j].dist
                    + c * planes[k].dist)
                    / denom;
                if p.is_finite() {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Sorts a coplanar vertex ring counter-clockwise around its centroid.
fn order_loop(vertices: &[DVec3], ring: &[u32], normal: DVec3) -> Result<Vec<u32>, BrushError> {
    let pts: Vec<DVec3> = ring.iter().map(|i| vertices[*i as usize]).collect();
    let c = pts.iter().copied().sum::<DVec3>() / pts.len() as f64;
    let u = (pts[0] - c).normalize();
    let v = normal.cross(u);
    let mut keyed: Vec<(f64, u32)> = ring
        .iter()
        .map(|i| {
            let d = vertices[*i as usize] - c;
            (d.dot(v).atan2(d.dot(u)), *i)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(keyed.into_iter().map(|(_, i)| i).collect())
}


/// Axis-aligned box brush.
pub fn box_brush(min: DVec3, max: DVec3, material: &str) -> Result<Brush, BrushError> {
    let (lo, hi) = if min.x <= max.x && min.y <= max.y && min.z <= max.z {
        (min, max)
    } else {
        (max, min)
    };
    if (hi - lo).min_element() <= DEGENERATE_EPS {
        return Err(BrushError::NotConvex);
    }
    let faces = vec![
        face_of(DVec3::X, hi.x, material),
        face_of(-DVec3::X, -lo.x, material),
        face_of(DVec3::Y, hi.y, material),
        face_of(-DVec3::Y, -lo.y, material),
        face_of(DVec3::Z, hi.z, material),
        face_of(-DVec3::Z, -lo.z, material),
    ];
    // Solid, the value the editor gives a new brush; 0 would be a brush
    // nothing collides with.
    Ok(Brush { faces, contents: 1, detail: false })
}

fn face_of(normal: DVec3, dist: f64, material: &str) -> BrushFace {
    let (u_axis, v_axis) = planar_axes(normal);
    BrushFace {
        normal,
        dist,
        material: material.to_string(),
        uv: UvMapping { u_axis, v_axis, offset: [0.0; 2], scale: [0.25; 2], rotation: 0.0 },
        lightmap_scale: 0.0,
    }
}

fn planar_axes(n: DVec3) -> (DVec3, DVec3) {
    let seed = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = n.cross(seed).normalize();
    let v = n.cross(u).normalize();
    (u, v)
}

/// Which side of the clip plane survives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// `n.p <= d`: the half the normal points away from.
    Front,
    /// `n.p >= d`.
    Back,
    Both,
}

/// Clips by a plane. `Both` returns the front and back pieces; either may be
/// missing when the plane misses the brush.
pub fn clip(brush: &Brush, plane: Plane, keep: Keep) -> Result<Vec<Brush>, BrushError> {
    let solid = Solid::from_faces(&brush.faces)?;
    let mut out = Vec::new();
    for p in match keep {
        Keep::Front => vec![plane],
        Keep::Back => vec![plane.flipped()],
        Keep::Both => vec![plane, plane.flipped()],
    } {
        if let Some(piece) = cut_once(&solid, p, &brush.faces)? {
            out.push(piece);
        }
    }
    if out.is_empty() {
        return Err(BrushError::EmptyResult);
    }
    Ok(out)
}

fn cut_once(solid: &Solid, plane: Plane, template: &[BrushFace]) -> Result<Option<Brush>, BrushError> {
    let mut planes: Vec<Plane> = solid.planes.clone();
    if !planes.iter().any(|p| {
        (p.normal.dot(plane.normal).abs() > 1.0 - 1e-9) && (p.dist - plane.dist).abs() <= PLANE_EPS
    }) {
        planes.push(plane);
    }
    match Solid::from_planes(planes) {
        Ok(s) => Ok(Some(Brush { faces: s.to_faces(template), contents: 0, detail: false })),
        Err(BrushError::NotConvex) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Turns a solid into a shell `thickness` thick, one convex piece per face.
pub fn hollow(brush: &Brush, thickness: f64) -> Result<Vec<Brush>, BrushError> {
    if thickness <= DEGENERATE_EPS {
        return Err(BrushError::NotConvex);
    }
    let solid = Solid::from_faces(&brush.faces)?;
    let mut out = Vec::new();
    for i in 0..solid.planes.len() {
        let planes: Vec<Plane> = solid
            .planes
            .iter()
            .enumerate()
            .map(|(j, q)| if i == j { q.offset(-thickness) } else { *q })
            .collect();
        if let Ok(piece) = Solid::from_planes(planes) {
            out.push(Brush { faces: piece.to_faces(&brush.faces), contents: 0, detail: false });
        }
    }
    if out.is_empty() {
        return Err(BrushError::EmptyResult);
    }
    Ok(out)
}


/// Subtracts `carver` from `target`, returning non-overlapping convex pieces.
pub fn carve(target: &Brush, carver: &Brush) -> Result<Vec<Brush>, BrushError> {
    let t = Solid::from_faces(&target.faces)?;
    let c = Solid::from_faces(&carver.faces)?;
    let mut out = Vec::new();
    // Sequential split: piece i lies inside carver faces 0..i and outside face
    // i, so the pieces are disjoint and their union is `target - carver`.
    let mut region: Vec<Plane> = t.planes.clone();
    for face in &c.planes {
        let mut planes = region.clone();
        planes.push(face.flipped());
        if let Ok(piece) = Solid::from_planes(planes) {
            out.push(Brush { faces: piece.to_faces(&target.faces), contents: 0, detail: false });
        }
        region.push(*face);
    }
    if out.is_empty() {
        return Err(BrushError::EmptyResult);
    }
    Ok(out)
}

/// Merges brushes whose union is already convex: the result is the
/// intersection of all input half-spaces, which must still contain every input
/// vertex. Disjoint or concave arrangements are rejected.
pub fn merge_convex(brushes: &[Brush]) -> Result<Brush, BrushError> {
    if brushes.is_empty() {
        return Err(BrushError::Empty);
    }
    let mut solids: Vec<Solid> = Vec::with_capacity(brushes.len());
    let mut vertices: Vec<DVec3> = Vec::new();
    for b in brushes {
        let s = Solid::from_faces(&b.faces)?;
        for v in &s.vertices {
            merge_vertex(&mut vertices, *v);
        }
        solids.push(s);
    }
    // When the union is convex, each of its faces lies on a face of one of the
    // pieces, and that face has every vertex of every piece behind it. A face
    // with a vertex in front of it (the shared wall of two boxes side by side)
    // is interior to the union and is dropped. Intersecting every face would
    // give the intersection of the pieces, not their union.
    let hull_planes: Vec<Plane> = solids
        .iter()
        .flat_map(|s| s.planes.iter().copied())
        .filter(|p| vertices.iter().all(|v| p.contains(*v)))
        .collect();
    if vertices.len() < 4 || hull_planes.len() < 4 {
        return Err(BrushError::NotConvexHull);
    }
    let merged = Solid::from_planes(hull_planes).map_err(|_| BrushError::NotConvexHull)?;
    // The result contains the union by construction. It equals it only when
    // none of its corners or face centres reach outside every piece; an L or a
    // plus shape leaves a corner of its bounding hull uncovered.
    let covered = |p: DVec3| solids.iter().any(|s| s.planes.iter().all(|q| q.contains(p)));
    let centres = (0..merged.planes.len()).map(|i| merged.face_centroid(i));
    if !merged.vertices.iter().copied().chain(centres).all(covered) {
        return Err(BrushError::NotConvexHull);
    }
    Ok(Brush { faces: merged.to_faces(&brushes[0].faces), contents: brushes[0].contents, detail: brushes[0].detail })
}

/// Extrudes one face outwards by `distance`, keeping the other faces fixed.
pub fn extrude_face(brush: &Brush, face: usize, distance: f64) -> Result<Brush, BrushError> {
    if face >= brush.faces.len() {
        return Err(BrushError::FaceIndex(face));
    }
    if distance.abs() <= DEGENERATE_EPS {
        return Err(BrushError::NotConvex);
    }
    let mut faces = brush.faces.clone();
    let p = Plane::new(faces[face].normal, faces[face].dist)?.offset(distance);
    faces[face].normal = p.normal;
    faces[face].dist = p.dist;
    Solid::from_faces(&faces)?;
    Ok(Brush { faces, contents: brush.contents, detail: brush.detail })
}


/// Moves vertex `vertex` to `to`, re-deriving every face plane that used it.
pub fn move_vertex(brush: &Brush, vertex: usize, to: DVec3) -> Result<Brush, BrushError> {
    let solid = Solid::from_faces(&brush.faces)?;
    if vertex >= solid.vertices.len() {
        return Err(BrushError::UnknownVertex(vertex));
    }
    if !to.is_finite() {
        return Err(BrushError::BadNormal);
    }
    let mut points = solid.vertices.clone();
    points[vertex] = to;

    let mut planes = Vec::with_capacity(solid.planes.len());
    for (i, loop_) in solid.loops.iter().enumerate() {
        if !loop_.contains(&(vertex as u32)) {
            planes.push(solid.planes[i]);
            continue;
        }
        // A dragged vertex pivots each incident face about the edge opposite
        // it: the new plane passes through the new position and the two loop
        // vertices furthest from it, which for a triangle are simply the
        // other two corners.
        let mut others: Vec<(f64, usize)> = loop_
            .iter()
            .map(|v| *v as usize)
            .filter(|v| *v != vertex)
            .map(|v| (points[v].distance(to), v))
            .collect();
        if others.len() < 2 {
            return Err(BrushError::NotConvex);
        }
        others.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let a = points[others[0].1];
        let b = points[others[1].1];
        let old = solid.planes[i];
        let mut n = (a - to).cross(b - to);
        if n.length_squared() <= PLANE_EPS * PLANE_EPS {
            return Err(BrushError::NotConvex);
        }
        n = n.normalize();
        if n.dot(old.normal) < 0.0 {
            n = -n;
        }
        planes.push(Plane { normal: n, dist: n.dot(to) });
    }
    let moved = Solid::from_planes(planes)?;
    if !moved.is_valid() {
        return Err(BrushError::NotConvex);
    }
    let mut out = brush.clone();
    out.faces = moved.to_faces(&brush.faces);
    Ok(out)
}

/// Source / Valve 220 texture projection for a world-space point.
///
/// `u = dot(p, u_axis) / scale_u + offset_u`; a non-zero `rotation` turns the
/// axes about the face normal by that many degrees first.
pub fn project_uv(uv: &UvMapping, normal: DVec3, p: DVec3) -> [f64; 2] {
    let mut du = uv.u_axis;
    let mut dv = uv.v_axis;
    if uv.rotation != 0.0 {
        let (s, c) = uv.rotation.to_radians().sin_cos();
        let n = normal.normalize();
        du = (du - n * du.dot(n)).normalize();
        dv = n.cross(du).normalize();
        let rotated_u = du * c + dv * s;
        let rotated_v = dv * c - du * s;
        du = rotated_u;
        dv = rotated_v;
    }
    let su = du / uv.scale[0];
    let sv = dv / uv.scale[1];
    [p.dot(su) + uv.offset[0], p.dot(sv) + uv.offset[1]]
}


/// Triangulated world-space render mesh of a brush.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl TriMesh {
    fn push(&mut self, p: DVec3, n: DVec3, uv: [f64; 2]) -> u32 {
        self.positions.push([p.x as f32, p.y as f32, p.z as f32]);
        self.normals.push([n.x as f32, n.y as f32, n.z as f32]);
        self.uvs.push([uv[0] as f32, uv[1] as f32]);
        (self.positions.len() - 1) as u32
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Texel coordinates as texture tiles, the unit a vertex's texture coordinate
/// is in.
pub fn tile_uv(uv: [f64; 2]) -> [f64; 2] {
    [uv[0] / crate::model::UV_TEXELS_PER_TILE, uv[1] / crate::model::UV_TEXELS_PER_TILE]
}

/// Triangulates a solid, fanning each face from its centroid so UVs do not
/// stretch across the face. UVs are in texture tiles.
pub fn triangulate(solid: &Solid, faces: &[BrushFace], m: DMat3) -> TriMesh {
    let mut mesh = TriMesh::default();
    for (i, loop_) in solid.loops.iter().enumerate() {
        let normal = (m * solid.planes[i].normal).normalize();
        let fallback = BrushFace {
            normal: solid.planes[i].normal,
            dist: solid.planes[i].dist,
            material: String::new(),
            uv: UvMapping::default(),
            lightmap_scale: 0.0,
        };
        let face = faces.get(i).unwrap_or(&fallback);
        let pts: Vec<DVec3> = loop_.iter().map(|v| m * solid.vertices[*v as usize]).collect();
        let c = pts.iter().copied().sum::<DVec3>() / pts.len() as f64;
        let center = mesh.push(c, normal, tile_uv(project_uv(&face.uv, face.normal, c)));
        let base = mesh.positions.len() as u32;
        for p in &pts {
            mesh.push(*p, normal, tile_uv(project_uv(&face.uv, face.normal, *p)));
        }
        for i in 0..pts.len() {
            let a = base + i as u32;
            let b = base + ((i + 1) % pts.len()) as u32;
            mesh.indices.extend_from_slice(&[center, a, b]);
        }
    }
    mesh
}


// ---- document-level brush operations ---------------------------------------
//
// Each of these reads a document, does the geometry work, and hands back the
// `Command` that performs the edit, so undo stays uniform.

use crate::command::Command;
use crate::doc::{new_object, DocError, Document};
use crate::model::{Id, Object, ObjectKind};

/// The brush an object holds, or a clear error when it holds something else.
fn brush_of(doc: &Document, id: Id) -> Result<Object, DocError> {
    let object = doc.object(id).ok_or(DocError::UnknownObject(id))?;
    if !matches!(object.kind, ObjectKind::Brush(_)) {
        return Err(DocError::NotABrush(id));
    }
    Ok(object.clone())
}

fn brush_kind(object: &Object) -> Brush {
    match &object.kind {
        ObjectKind::Brush(b) => b.clone(),
        _ => Brush::default(),
    }
}

/// A command that adds one brush to the document.
pub fn cmd_box(
    layer: Id,
    parent: Option<Id>,
    min: DVec3,
    max: DVec3,
    material: &str,
) -> Result<Command, DocError> {
    let brush = box_brush(min, max, material)?;
    let mut object = new_object("box", layer, ObjectKind::Brush(brush));
    object.parent = parent;
    Ok(Command::Batch { label: "brush box".into(), commands: vec![Command::AddObjects { objects: vec![object] }] })
}

/// Clips every selected brush. `Keep::Both` may leave each brush split in two.
pub fn cmd_clip(doc: &Document, ids: &[Id], plane: Plane, keep: Keep) -> Result<Command, DocError> {
    let mut commands: Vec<Command> = Vec::new();
    for id in ids {
        let object = brush_of(doc, *id)?;
        let brush = brush_kind(&object);
        let pieces = clip(&brush, plane, keep)?;
        commands.push(replace_with_solids(&object, pieces, brush.contents, brush.detail));
    }
    Ok(batch("brush clip", commands))
}

/// Turns each selected brush into a shell `thickness` thick.
pub fn cmd_hollow(doc: &Document, id: Id, thickness: f64) -> Result<Command, DocError> {
    let object = brush_of(doc, id)?;
    let brush = brush_kind(&object);
    let pieces = hollow(&brush, thickness)?;
    Ok(batch("brush hollow", vec![replace_with_solids(&object, pieces, brush.contents, brush.detail)]))
}

/// Subtracts `carver` from every target; the carver itself is consumed.
pub fn cmd_carve(doc: &Document, carver: Id, targets: &[Id]) -> Result<Command, DocError> {
    let carver_object = brush_of(doc, carver)?;
    let carver_brush = brush_kind(&carver_object);
    Solid::from_faces(&carver_brush.faces)?;
    let mut commands: Vec<Command> = Vec::new();
    for id in targets {
        let object = brush_of(doc, *id)?;
        let brush = brush_kind(&object);
        let pieces = carve(&brush, &carver_brush)?;
        commands.push(replace_with_solids(&object, pieces, brush.contents, brush.detail));
    }
    if !targets.contains(&carver) {
        commands.push(Command::RemoveObjects { ids: vec![carver] });
    }
    Ok(batch("brush carve", commands))
}

/// Replaces the selection with the hull of its brushes.
pub fn cmd_merge_convex(doc: &Document, ids: &[Id]) -> Result<Command, DocError> {
    if ids.len() < 2 {
        return Err(DocError::EmptySelection);
    }
    let mut parts: Vec<(Object, Brush)> = Vec::with_capacity(ids.len());
    for id in ids {
        let object = brush_of(doc, *id)?;
        let brush = brush_kind(&object);
        parts.push((object, brush));
    }
    let brushes: Vec<Brush> = parts.iter().map(|(_, b)| b.clone()).collect();
    let merged = merge_convex(&brushes)?;
    let (first, first_brush) = parts.first().cloned().ok_or(DocError::EmptySelection)?;
    let mut commands = vec![replace_with_solids(&first, vec![merged], first_brush.contents, first_brush.detail)];
    commands.push(Command::RemoveObjects { ids: ids[1..].to_vec() });
    Ok(batch("brush merge", commands))
}

/// Pushes one face of a brush outwards.
pub fn cmd_extrude_face(doc: &Document, id: Id, face: usize, distance: f64) -> Result<Command, DocError> {
    let object = brush_of(doc, id)?;
    let brush = brush_kind(&object);
    let moved = extrude_face(&brush, face, distance)?;
    let mut replaced = object.clone();
    replaced.kind = ObjectKind::Brush(moved);
    Ok(batch("brush extrude", vec![Command::ReplaceObjects { objects: vec![replaced] }]))
}

/// Drags the vertex nearest `from` to `to`.
pub fn cmd_move_vertex(doc: &Document, id: Id, from: DVec3, to: DVec3) -> Result<Command, DocError> {
    let object = brush_of(doc, id)?;
    let brush = brush_kind(&object);
    let solid = Solid::from_faces(&brush.faces)?;
    let index = solid
        .vertices
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = (**a - from).length_squared();
            let db = (**b - from).length_squared();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .ok_or(BrushError::Empty)?;
    let moved = move_vertex(&brush, index, to)?;
    let mut replaced = object.clone();
    replaced.kind = ObjectKind::Brush(moved);
    Ok(batch("brush move vertex", vec![Command::ReplaceObjects { objects: vec![replaced] }]))
}

fn batch(label: &str, commands: Vec<Command>) -> Command {
    Command::Batch { label: label.into(), commands }
}

/// Replaces one object with new solids that inherit its transform and parent.
fn replace_with_solids(object: &Object, pieces: Vec<Brush>, contents: u32, detail: bool) -> Command {
    let mut pieces = pieces.into_iter();
    let Some(first) = pieces.next() else {
        return Command::RemoveObjects { ids: vec![object.id] };
    };
    let rest: Vec<Brush> = pieces.collect();
    if rest.is_empty() {
        let mut only = object.clone();
        only.kind = ObjectKind::Brush(Brush { faces: first.faces, contents, detail });
        return Command::ReplaceObjects { objects: vec![only] };
    }
    let mut commands = vec![Command::RemoveObjects { ids: vec![object.id] }];
    let mut objects: Vec<Object> = Vec::with_capacity(rest.len() + 1);
    for (i, piece) in std::iter::once(first).chain(rest).enumerate() {
        let mut copy = object.clone();
        copy.id = uuid::Uuid::new_v4();
        copy.name = format!("{}_{i}", object.name);
        copy.kind = ObjectKind::Brush(Brush { faces: piece.faces, contents, detail });
        objects.push(copy);
    }
    commands.push(Command::AddObjects { objects });
    batch("replace brush", commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn unit_box() -> Solid {
        Solid::from_planes(vec![
            Plane::new(DVec3::X, 64.0).unwrap(),
            Plane::new(-DVec3::X, 0.0).unwrap(),
            Plane::new(DVec3::Y, 64.0).unwrap(),
            Plane::new(-DVec3::Y, 0.0).unwrap(),
            Plane::new(DVec3::Z, 64.0).unwrap(),
            Plane::new(-DVec3::Z, 0.0).unwrap(),
        ])
        .unwrap()
    }

    #[test]
    fn brush_box_topology() {
        let s = unit_box();
        assert_eq!(s.planes.len(), 6);
        assert_eq!(s.vertices.len(), 8);
        assert!(s.loops.iter().all(|l| l.len() == 4));
        assert!((s.volume() - 64.0 * 64.0 * 64.0).abs() < 1e-6);
        assert!(s.is_valid());
    }

    #[test]
    fn plane_from_points_orients_away_from_interior() {
        let p = Plane::from_points(
            DVec3::new(0.0, 0.0, 64.0),
            DVec3::new(0.0, 64.0, 64.0),
            DVec3::new(64.0, 0.0, 64.0),
            DVec3::ZERO,
        )
        .unwrap();
        assert!(p.normal.z > 0.9);
        assert!((p.dist - 64.0).abs() < 1e-9);
    }

    #[test]
    fn bad_planes_are_rejected() {
        assert!(matches!(Plane::new(DVec3::ZERO, 0.0), Err(BrushError::BadNormal)));
        assert!(matches!(Plane::new(DVec3::NAN, 0.0), Err(BrushError::BadNormal)));
        assert!(Solid::from_planes(vec![Plane::new(DVec3::X, 0.0).unwrap()]).is_err());
        assert!(Solid::from_planes(vec![]).is_err());
        assert!(Plane::from_points(DVec3::ZERO, DVec3::X, DVec3::X * 2.0, DVec3::ZERO).is_err());
    }

    #[test]
    fn brush_uv_known_point() {
        let uv = UvMapping {
            u_axis: DVec3::X,
            v_axis: -DVec3::Y,
            offset: [0.0, 0.0],
            scale: [0.25, 0.25],
            rotation: 0.0,
        };
        let got = project_uv(&uv, DVec3::Z, DVec3::new(32.0, -16.0, 64.0));
        assert!((got[0] - 128.0).abs() < 1e-9);
        assert!((got[1] - 64.0).abs() < 1e-9);
    }

    #[test]
    fn extrude_face_moves_one_plane() {
        let b = box_brush(DVec3::ZERO, DVec3::splat(64.0), "m").unwrap();
        let e = extrude_face(&b, 0, 16.0).unwrap();
        assert!((e.faces[0].dist - 80.0).abs() < 1e-9);
        let v = Solid::from_faces(&e.faces).unwrap().volume();
        assert!((v - 64.0 * 64.0 * 80.0).abs() < 1e-6);
        assert!(extrude_face(&b, 99, 1.0).is_err());
    }

    #[test]
    fn merge_convex_rejects_disjoint_pieces() {
        let a = box_brush(DVec3::ZERO, DVec3::splat(16.0), "m").unwrap();
        let far = box_brush(DVec3::splat(512.0), DVec3::splat(528.0), "m").unwrap();
        assert!(matches!(merge_convex(&[a.clone(), far]), Err(BrushError::NotConvexHull)));
        let hull = merge_convex(&[a.clone(), a.clone()]).unwrap();
        let v = Solid::from_faces(&hull.faces).unwrap().volume();
        assert!((v - 16.0 * 16.0 * 16.0).abs() < 1e-6);
        assert!(merge_convex(&[]).is_err());

        // Side by side along X: the shared wall is interior and the result is
        // the 32 x 16 x 16 box.
        let right = box_brush(DVec3::new(16.0, 0.0, 0.0), DVec3::new(32.0, 16.0, 16.0), "m").unwrap();
        let joined = merge_convex(&[a.clone(), right]).unwrap();
        let v = Solid::from_faces(&joined.faces).unwrap().volume();
        assert!((v - 32.0 * 16.0 * 16.0).abs() < 1e-6, "volume {v}");

        // Overlapping boxes whose union is a box merge to that box.
        let overlap = box_brush(DVec3::new(8.0, 0.0, 0.0), DVec3::new(24.0, 16.0, 16.0), "m").unwrap();
        let v = Solid::from_faces(&merge_convex(&[a.clone(), overlap]).unwrap().faces).unwrap().volume();
        assert!((v - 24.0 * 16.0 * 16.0).abs() < 1e-6, "volume {v}");

        // An L is not convex.
        let up = box_brush(DVec3::new(0.0, 16.0, 0.0), DVec3::new(16.0, 32.0, 16.0), "m").unwrap();
        let right = box_brush(DVec3::new(16.0, 0.0, 0.0), DVec3::new(32.0, 16.0, 16.0), "m").unwrap();
        assert!(matches!(merge_convex(&[a, up, right]), Err(BrushError::NotConvexHull)));
    }

    #[test]
    fn move_vertex_rebuilds_geometry() {
        let b = box_brush(DVec3::ZERO, DVec3::splat(64.0), "m").unwrap();
        let s = Solid::from_faces(&b.faces).unwrap();
        let v = s.vertices.iter().position(|p| (*p - DVec3::splat(64.0)).length() < 1e-6).unwrap();
        let moved = move_vertex(&b, v, DVec3::new(96.0, 64.0, 64.0)).unwrap();
        let ms = Solid::from_faces(&moved.faces).unwrap();
        assert!(ms.is_valid());
        assert!(ms.volume() > s.volume());
        assert!(move_vertex(&b, 999, DVec3::ZERO).is_err());
    }

    #[test]
    fn brush_clip_volume_conserved() {
        let b = box_brush(DVec3::ZERO, DVec3::splat(128.0), "m").unwrap();
        let whole = Solid::from_faces(&b.faces).unwrap().volume();
        let plane = Plane::new(DVec3::X, 32.0).unwrap();
        let pieces = clip(&b, plane, Keep::Both).unwrap();
        assert_eq!(pieces.len(), 2, "the plane crosses the box");
        let total: f64 = pieces
            .iter()
            .map(|p| {
                let s = Solid::from_faces(&p.faces).unwrap();
                assert!(s.is_valid());
                s.volume()
            })
            .sum();
        assert!((total - whole).abs() < 1e-6, "kept {total} of {whole}");
        assert!((whole - 128.0 * 128.0 * 128.0).abs() < 1e-6);

        // One side only keeps the volume under the plane.
        let front = clip(&b, plane, Keep::Front).unwrap();
        assert_eq!(front.len(), 1);
        let kept = Solid::from_faces(&front[0].faces).unwrap().volume();
        assert!((kept - 32.0 * 128.0 * 128.0).abs() < 1e-6, "kept {kept}");

        // A plane that misses the brush leaves it untouched.
        let miss = clip(&b, Plane::new(DVec3::X, 4096.0).unwrap(), Keep::Front).unwrap();
        assert_eq!(miss.len(), 1);
        assert!((Solid::from_faces(&miss[0].faces).unwrap().volume() - whole).abs() < 1e-6);
        assert!(clip(&b, Plane::new(DVec3::X, 4096.0).unwrap(), Keep::Back).is_err());
    }

    #[test]
    fn brush_carve_convex_disjoint() {
        let target = box_brush(DVec3::ZERO, DVec3::splat(128.0), "m").unwrap();
        let carver = box_brush(DVec3::splat(48.0), DVec3::splat(80.0), "m").unwrap();
        let pieces = carve(&target, &carver).unwrap();
        assert!(pieces.len() >= 4, "a corner cut leaves one piece per carver face");
        let removed = 32.0f64.powi(3);
        let carver_solid = Solid::from_faces(&carver.faces).unwrap();
        let total: f64 = pieces
            .iter()
            .map(|p| {
                let s = Solid::from_faces(&p.faces).unwrap();
                assert!(s.is_valid(), "every piece is a closed convex solid");
                // No vertex of a piece may sit strictly inside the carver, and
                // subtracting the carver again may not remove any volume.
                for v in &s.vertices {
                    assert!(
                        carver_solid.planes.iter().any(|q| q.distance(*v) >= -PLANE_EPS),
                        "a piece vertex {v:?} is inside the carver"
                    );
                }
                if let Ok(rest) = carve(p, &carver) {
                    let left: f64 =
                        rest.iter().map(|r| Solid::from_faces(&r.faces).unwrap().volume()).sum();
                    assert!((left - s.volume()).abs() < 1e-6, "a piece overlaps the carver");
                }
                s.volume()
            })
            .sum();
        let whole = 128.0f64.powi(3);
        assert!((total - (whole - removed)).abs() < 1e-6, "pieces total {total}");

        // A carver that misses the target leaves it whole.
        let far = box_brush(DVec3::splat(512.0), DVec3::splat(544.0), "m").unwrap();
        let untouched = carve(&target, &far).unwrap();
        assert_eq!(untouched.len(), 1);
        assert!((Solid::from_faces(&untouched[0].faces).unwrap().volume() - whole).abs() < 1e-6);
    }

    fn op_doc() -> crate::doc::Document {
        let mut d = crate::doc::Document::empty();
        let layer = d.layer_ids()[0];
        let mut b = new_object("wall", layer, ObjectKind::Brush(box_brush(DVec3::ZERO, DVec3::splat(64.0), "m").unwrap()));
        b.id = Uuid::new_v4();
        d.apply(Command::AddObjects { objects: vec![b] }).unwrap();
        d
    }

    /// Undoes back to the state `op_doc` left, which is the document the ops
    /// were applied to.
    fn undo_to_start(d: &mut crate::doc::Document) {
        while d.undo_depth() > 1 {
            d.undo().unwrap();
        }
    }

    #[test]
    fn brush_commands_reshape_the_document() {
        use crate::doc::KindFilter;

        let mut d = op_doc();
        let wall = d.kind_objects(KindFilter::Brush)[0];
        d.apply(cmd_extrude_face(&d, wall, 0, 32.0).unwrap()).unwrap();
        assert!((Solid::from_faces(&brush_of_obj(d.object(wall).unwrap())).unwrap().volume() - 96.0 * 64.0 * 64.0).abs() < 1e-6);

        let corners: Vec<DVec3> = Solid::from_faces(&brush_of_obj(d.object(wall).unwrap())).unwrap().vertices;
        let corner = *corners.iter().find(|v| (**v - DVec3::ZERO).length() < 1e-6).expect("a corner vertex");
        d.apply(cmd_move_vertex(&d, wall, corner, DVec3::new(0.0, 0.0, -32.0)).unwrap()).unwrap();
        let moved = Solid::from_faces(&brush_of_obj(d.object(wall).unwrap())).unwrap();
        assert!(moved.is_valid());
        assert!(moved.volume() > 96.0 * 64.0 * 64.0);
        d.undo().unwrap();
        assert!((Solid::from_faces(&brush_of_obj(d.object(wall).unwrap())).unwrap().volume() - 96.0 * 64.0 * 64.0).abs() < 1e-6);

        d.apply(cmd_box(d.layer_ids()[0], None, DVec3::ZERO, DVec3::splat(16.0), "m").unwrap()).unwrap();
        let extra = d.kind_objects(KindFilter::Brush)[1];
        d.apply(cmd_clip(&d, &[extra], Plane::new(DVec3::X, 8.0).unwrap(), Keep::Both).unwrap()).unwrap();
        assert_eq!(d.kind_objects(KindFilter::Brush).len(), 3, "the clip split the box in two");

        d.apply(cmd_hollow(&d, wall, 8.0).unwrap()).unwrap();
        assert!(d.kind_objects(KindFilter::Brush).len() > 3, "hollowing added shell pieces");

        undo_to_start(&mut d);
        assert_eq!(d.doc.objects.len(), 1, "every brush op undid cleanly");
    }

    #[test]
    fn carve_and_merge_commands_reshape_the_document() {
        use crate::doc::KindFilter;

        let mut d = op_doc();
        let wall = d.kind_objects(KindFilter::Brush)[0];
        let layer = d.layer_ids()[0];
        d.apply(cmd_box(layer, None, DVec3::splat(16.0), DVec3::splat(48.0), "m").unwrap()).unwrap();
        let tool = d.kind_objects(KindFilter::Brush)[1];
        d.apply(cmd_carve(&d, tool, &[wall]).unwrap()).unwrap();
        assert!(d.object(tool).is_none(), "the carver is consumed");
        let after_carve = d.kind_objects(KindFilter::Brush);
        assert!(after_carve.len() > 1);
        let total: f64 = after_carve
            .iter()
            .map(|id| Solid::from_faces(&brush_of_obj(d.object(*id).unwrap())).unwrap().volume())
            .sum();
        assert!((total - (64.0f64.powi(3) - 32.0f64.powi(3))).abs() < 1e-6, "total {total}");

        // Two boxes stacked face to face are one convex hull.
        let mut d = op_doc();
        let lower = d.kind_objects(KindFilter::Brush)[0];
        let layer = d.layer_ids()[0];
        d.apply(cmd_box(layer, None, DVec3::new(0.0, 0.0, 64.0), DVec3::new(64.0, 64.0, 128.0), "m").unwrap()).unwrap();
        let upper = d.kind_objects(KindFilter::Brush)[1];
        d.apply(cmd_merge_convex(&d, &[lower, upper]).unwrap()).unwrap();
        assert_eq!(d.kind_objects(KindFilter::Brush).len(), 1);
        let hull = Solid::from_faces(&brush_of_obj(d.object(lower).unwrap())).unwrap();
        assert!((hull.volume() - 2.0 * 64.0f64.powi(3)).abs() < 1e-6, "hull volume {}", hull.volume());
        assert!(cmd_merge_convex(&d, &[lower]).is_err(), "one brush is not a merge");

        // A pair whose union has a notch is not convex and is rejected.
        let mut d = op_doc();
        let a = d.kind_objects(KindFilter::Brush)[0];
        d.apply(cmd_box(d.layer_ids()[0], None, DVec3::splat(32.0), DVec3::splat(96.0), "m").unwrap()).unwrap();
        let b = d.kind_objects(KindFilter::Brush)[1];
        assert!(matches!(cmd_merge_convex(&d, &[a, b]), Err(crate::doc::DocError::Brush(BrushError::NotConvexHull))));
    }

    fn brush_of_obj(object: &Object) -> Vec<BrushFace> {
        match &object.kind {
            ObjectKind::Brush(b) => b.faces.clone(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn brush_ops_reject_objects_that_are_not_brushes() {
        let mut d = crate::doc::Document::empty();
        let layer = d.layer_ids()[0];
        let mut e = new_object("e", layer, ObjectKind::Group);
        e.id = Uuid::new_v4();
        d.apply(Command::AddObjects { objects: vec![e] }).unwrap();
        let id = d.object_ids()[0];
        assert!(matches!(cmd_extrude_face(&d, id, 0, 1.0), Err(crate::doc::DocError::NotABrush(_))));
        assert!(matches!(cmd_hollow(&d, id, 1.0), Err(crate::doc::DocError::NotABrush(_))));
        assert!(matches!(cmd_move_vertex(&d, id, DVec3::ZERO, DVec3::X), Err(crate::doc::DocError::NotABrush(_))));
        assert!(matches!(cmd_extrude_face(&d, Uuid::new_v4(), 0, 1.0), Err(crate::doc::DocError::UnknownObject(_))));
    }

    #[test]
    fn transformed_solid_scales_volume() {
        let s = unit_box();
        let m = DMat3::from_diagonal(DVec3::new(2.0, 2.0, 2.0));
        let t = s.transformed(m).unwrap();
        assert!((t.volume() - s.volume() * 8.0).abs() < 1e-6);
        let faces = box_brush(DVec3::ZERO, DVec3::splat(64.0), "m").unwrap().faces;
        let mesh = triangulate(&s, &faces, DMat3::IDENTITY);
        assert_eq!(mesh.triangle_count(), 24);
        assert_eq!(mesh.positions.len(), 6 * 5);
    }
}

/// Object-space bounds of a brush.
pub fn brush_bounds(brush: &Brush) -> Result<(DVec3, DVec3), BrushError> {
    let solid = Solid::from_faces(&brush.faces)?;
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for v in &solid.vertices {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    Ok((lo, hi))
}

/// Scales a brush so its bounds become `size`, keeping the min corner fixed.
/// Face planes are transformed exactly, so any convex brush keeps its shape.
pub fn resize(brush: &Brush, size: DVec3) -> Result<Brush, BrushError> {
    if !size.is_finite() || size.min_element() <= 0.0 {
        return Err(BrushError::NotConvex);
    }
    let (lo, hi) = brush_bounds(brush)?;
    let extent = hi - lo;
    if extent.min_element() <= 0.0 {
        return Err(BrushError::NotConvex);
    }
    let s = size / extent;
    let mut out = brush.clone();
    for f in &mut out.faces {
        // p' = lo + S (p - lo): the plane n.p = d maps to (n/S).p' = d - n.lo + (n/S).lo.
        let n = f.normal / s;
        let d = f.dist - f.normal.dot(lo) + n.dot(lo);
        let len = n.length();
        f.normal = n / len;
        f.dist = d / len;
    }
    Solid::from_faces(&out.faces)?;
    Ok(out)
}

/// The command that resizes a brush object to `size`.
pub fn cmd_resize(doc: &Document, id: Id, size: DVec3) -> Result<Command, DocError> {
    let object = brush_of(doc, id)?;
    let resized = resize(&brush_kind(&object), size)?;
    let mut replaced = object.clone();
    replaced.kind = ObjectKind::Brush(resized);
    Ok(batch("brush resize", vec![Command::ReplaceObjects { objects: vec![replaced] }]))
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    #[test]
    fn resize_box_keeps_min_corner() {
        let b = box_brush(DVec3::new(10.0, 20.0, 0.0), DVec3::new(74.0, 84.0, 128.0), "m").unwrap();
        let r = resize(&b, DVec3::new(256.0, 32.0, 512.0)).unwrap();
        let (lo, hi) = brush_bounds(&r).unwrap();
        assert!((lo - DVec3::new(10.0, 20.0, 0.0)).length() < 1e-6);
        assert!((hi - DVec3::new(266.0, 52.0, 512.0)).length() < 1e-6);
        assert!(resize(&b, DVec3::new(0.0, 1.0, 1.0)).is_err());
    }
}
