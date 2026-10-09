//! Procedural mesh authoring. A `MeshSpec` is plain JSON, so the editor, the
//! CLI and an automated author all build geometry the same way; the result is a
//! `.glb` in the project plus a `Mesh` object that references it.

use std::f64::consts::TAU;

use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::doc::new_object;
use crate::import::glb::{self, GlbMesh};
use crate::model::{CollisionMode, Id, Mesh, ObjectKind, Transform};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum MeshSpec {
    /// Axis-aligned box centred on the origin.
    Box { size: [f64; 3] },
    /// Z-up cylinder with its base on z = 0.
    Cylinder { radius: f64, height: f64, segments: u32, #[serde(default = "yes")] caps: bool },
    /// Z-up cone with its base on z = 0.
    Cone { radius: f64, height: f64, segments: u32 },
    Sphere { radius: f64, segments: u32, rings: u32 },
    /// Ring around the Z axis.
    Torus { major: f64, minor: f64, segments: u32, sides: u32 },
    /// Flat XY rectangle facing +Z, subdivided.
    Plane { size: [f64; 2], divisions: [u32; 2] },
    /// Straight stair climbing +X: `steps` treads over `run` x `rise`, `width` along Y.
    Stairs { steps: u32, run: f64, rise: f64, width: f64 },
    /// Wedge rising along +X.
    Ramp { length: f64, width: f64, height: f64 },
    /// Arch in the XZ plane, `thickness` along Y.
    Arch { width: f64, height: f64, thickness: f64, depth: f64, segments: u32 },
    /// Closed polygon in XY (counter-clockwise) extruded along +Z.
    Extrude { outline: Vec<[f64; 2]>, height: f64 },
    /// Profile in the XZ half-plane (x >= 0) revolved around Z.
    Lathe { profile: Vec<[f64; 2]>, segments: u32 },
    /// A cross-section in the local XY plane swept along a 3D path.
    Loft { section: Vec<[f64; 2]>, path: Vec<[f64; 3]>, #[serde(default)] closed_section: bool },
    /// Explicit triangles.
    Raw { positions: Vec<[f64; 3]>, indices: Vec<u32> },
}

fn yes() -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
pub enum MeshGenError {
    #[error("{0}")]
    Bad(&'static str),
    #[error("editable mesh: {0}")]
    Editable(String),
    #[error("glb: {0}")]
    Glb(#[from] glb::GlbError),
}

const MAX_SEGMENTS: u32 = 1024;
const MAX_VERTICES: usize = 4_000_000;

fn seg(n: u32, min: u32) -> Result<u32, MeshGenError> {
    if n < min || n > MAX_SEGMENTS {
        return Err(MeshGenError::Bad("segment count out of range"));
    }
    Ok(n)
}

fn pos(v: f64) -> Result<f64, MeshGenError> {
    if v.is_finite() && v > 0.0 && v < 1.0e6 {
        Ok(v)
    } else {
        Err(MeshGenError::Bad("dimensions must be positive and finite"))
    }
}

/// Flat-shaded triangle soup with planar UVs; faces are emitted with their own
/// vertices so hard edges stay hard.
#[derive(Default)]
struct Builder {
    p: Vec<[f32; 3]>,
    n: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    i: Vec<u32>,
    uv_scale: f64,
}

impl Builder {
    fn new(uv_scale: f64) -> Self {
        Self { uv_scale, ..Default::default() }
    }

    fn uv_for(&self, p: DVec3, n: DVec3) -> [f32; 2] {
        let a = n.abs();
        let (u, v) = if a.z >= a.x && a.z >= a.y {
            (p.x, p.y)
        } else if a.x >= a.y {
            (p.y, p.z)
        } else {
            (p.x, p.z)
        };
        [(u * self.uv_scale) as f32, (-v * self.uv_scale) as f32]
    }

    fn vertex(&mut self, p: DVec3, n: DVec3) -> u32 {
        let uv = self.uv_for(p, n);
        self.p.push([p.x as f32, p.y as f32, p.z as f32]);
        self.n.push([n.x as f32, n.y as f32, n.z as f32]);
        self.uv.push(uv);
        (self.p.len() - 1) as u32
    }

    /// A convex or star-shaped polygon, fanned; winding gives the normal.
    fn polygon(&mut self, pts: &[DVec3]) {
        if pts.len() < 3 {
            return;
        }
        let mut n = DVec3::ZERO;
        for k in 0..pts.len() {
            n += pts[k].cross(pts[(k + 1) % pts.len()]);
        }
        let n = n.normalize_or_zero();
        if n == DVec3::ZERO {
            return;
        }
        let base = self.p.len() as u32;
        for p in pts {
            self.vertex(*p, n);
        }
        for k in 1..pts.len() as u32 - 1 {
            self.i.extend([base, base + k, base + k + 1]);
        }
    }

    /// Quad with smooth per-vertex normals (for curved surfaces).
    fn smooth_quad(&mut self, p: [DVec3; 4], n: [DVec3; 4]) {
        let base = self.p.len() as u32;
        for k in 0..4 {
            self.vertex(p[k], n[k]);
        }
        self.i.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    fn triangle(&mut self, a: DVec3, b: DVec3, c: DVec3) {
        self.polygon(&[a, b, c]);
    }

    fn finish(self, name: &str, material: &str) -> Result<GlbMesh, MeshGenError> {
        if self.p.len() > MAX_VERTICES {
            return Err(MeshGenError::Bad("mesh is too large"));
        }
        if self.i.is_empty() {
            return Err(MeshGenError::Bad("the shape produced no triangles"));
        }
        Ok(GlbMesh { name: name.into(), material: material.into(), positions: self.p, normals: self.n, uvs: self.uv, colors: Vec::new(), indices: self.i })
    }
}

fn ring(radius: f64, segments: u32, k: u32) -> DVec3 {
    let a = TAU * k as f64 / segments as f64;
    DVec3::new(a.cos() * radius, a.sin() * radius, 0.0)
}

fn signed_area(pts: &[DVec2]) -> f64 {
    (0..pts.len()).map(|k| pts[k].perp_dot(pts[(k + 1) % pts.len()])).sum::<f64>() * 0.5
}

/// Ear-clipping triangulation of a simple polygon (counter-clockwise).
pub(crate) fn ear_clip(pts: &[DVec2]) -> Vec<[usize; 3]> {
    let mut idx: Vec<usize> = (0..pts.len()).collect();
    let mut out = Vec::new();
    let mut guard = 0;
    while idx.len() > 3 && guard < pts.len() * pts.len() {
        guard += 1;
        let n = idx.len();
        let mut clipped = false;
        // A collinear corner makes a zero-area ear; take one only when no
        // ear with real area is left.
        for (k, strict) in (0..n).map(|k| (k, true)).chain((0..n).map(|k| (k, false))) {
            let (a, b, c) = (idx[(k + n - 1) % n], idx[k], idx[(k + 1) % n]);
            let (pa, pb, pc) = (pts[a], pts[b], pts[c]);
            let cross = (pb - pa).perp_dot(pc - pb);
            let floor = if strict { 1e-9 * (pb - pa).length() * (pc - pb).length() } else { 0.0 };
            if cross <= floor {
                continue;
            }
            let inside = idx.iter().any(|&j| {
                if j == a || j == b || j == c {
                    return false;
                }
                let p = pts[j];
                let d1 = (pb - pa).perp_dot(p - pa);
                let d2 = (pc - pb).perp_dot(p - pb);
                let d3 = (pa - pc).perp_dot(p - pc);
                d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0
            });
            if inside {
                continue;
            }
            out.push([a, b, c]);
            idx.remove(k);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    if idx.len() == 3 {
        out.push([idx[0], idx[1], idx[2]]);
    }
    flip_zero_area(pts, &mut out);
    out
}

/// Greedy clipping can still leave a triangle whose corner rests on its
/// opposite edge. That edge is a diagonal, so flipping it with the triangle
/// across replaces the sliver with two triangles of real area.
fn flip_zero_area(pts: &[DVec2], tris: &mut [[usize; 3]]) {
    for _ in 0..tris.len() {
        let mut flipped = false;
        for t in 0..tris.len() {
            let tri = tris[t];
            let (k, len2) = (0..3)
                .map(|k| (k, (pts[tri[(k + 1) % 3]] - pts[tri[k]]).length_squared()))
                .fold((0, 0.0), |best, cur| if cur.1 > best.1 { cur } else { best });
            let (p, q, m) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
            let cross = (pts[q] - pts[p]).perp_dot(pts[m] - pts[p]);
            if cross.abs() > 1e-9 * len2 {
                continue;
            }
            let across = (0..tris.len()).find_map(|u| {
                let o = tris[u];
                (0..3).find(|j| o[*j] == q && o[(*j + 1) % 3] == p).map(|j| (u, o[(j + 2) % 3]))
            });
            let Some((u, x)) = across else { continue };
            tris[t] = [p, x, m];
            tris[u] = [x, q, m];
            flipped = true;
        }
        if !flipped {
            break;
        }
    }
}

/// Builds the mesh for a spec. `uv_scale` is texture repeats per world unit.
pub fn build(spec: &MeshSpec, name: &str, material: &str, uv_scale: f64) -> Result<GlbMesh, MeshGenError> {
    let mut b = Builder::new(if uv_scale.is_finite() && uv_scale > 0.0 { uv_scale } else { 1.0 / crate::model::UV_TEXELS_PER_TILE });
    match spec {
        MeshSpec::Box { size } => {
            let h = DVec3::new(pos(size[0])?, pos(size[1])?, pos(size[2])?) * 0.5;
            let c = |x: f64, y: f64, z: f64| DVec3::new(x * h.x, y * h.y, z * h.z);
            b.polygon(&[c(-1., -1., 1.), c(1., -1., 1.), c(1., 1., 1.), c(-1., 1., 1.)]);
            b.polygon(&[c(-1., 1., -1.), c(1., 1., -1.), c(1., -1., -1.), c(-1., -1., -1.)]);
            b.polygon(&[c(1., -1., -1.), c(1., 1., -1.), c(1., 1., 1.), c(1., -1., 1.)]);
            b.polygon(&[c(-1., 1., -1.), c(-1., -1., -1.), c(-1., -1., 1.), c(-1., 1., 1.)]);
            b.polygon(&[c(1., 1., -1.), c(-1., 1., -1.), c(-1., 1., 1.), c(1., 1., 1.)]);
            b.polygon(&[c(-1., -1., -1.), c(1., -1., -1.), c(1., -1., 1.), c(-1., -1., 1.)]);
        }
        MeshSpec::Cylinder { radius, height, segments, caps } => {
            let (r, h, s) = (pos(*radius)?, pos(*height)?, seg(*segments, 3)?);
            for k in 0..s {
                let (a, c) = (ring(r, s, k), ring(r, s, k + 1));
                let (na, nc) = (a.normalize(), c.normalize());
                b.smooth_quad([a, c, c + DVec3::Z * h, a + DVec3::Z * h], [na, nc, nc, na]);
            }
            if *caps {
                b.polygon(&(0..s).map(|k| ring(r, s, k) + DVec3::Z * h).collect::<Vec<_>>());
                b.polygon(&(0..s).rev().map(|k| ring(r, s, k)).collect::<Vec<_>>());
            }
        }
        MeshSpec::Cone { radius, height, segments } => {
            let (r, h, s) = (pos(*radius)?, pos(*height)?, seg(*segments, 3)?);
            let apex = DVec3::Z * h;
            for k in 0..s {
                b.triangle(ring(r, s, k), ring(r, s, k + 1), apex);
            }
            b.polygon(&(0..s).rev().map(|k| ring(r, s, k)).collect::<Vec<_>>());
        }
        MeshSpec::Sphere { radius, segments, rings } => {
            let (r, s, rn) = (pos(*radius)?, seg(*segments, 3)?, seg(*rings, 2)?);
            let at = |i: u32, j: u32| {
                let th = std::f64::consts::PI * j as f64 / rn as f64;
                let ph = TAU * i as f64 / s as f64;
                DVec3::new(th.sin() * ph.cos(), th.sin() * ph.sin(), th.cos())
            };
            for j in 0..rn {
                for i in 0..s {
                    let n = [at(i, j + 1), at(i + 1, j + 1), at(i + 1, j), at(i, j)];
                    b.smooth_quad(n.map(|v| v * r), n);
                }
            }
        }
        MeshSpec::Torus { major, minor, segments, sides } => {
            let (rm, rn, s, t) = (pos(*major)?, pos(*minor)?, seg(*segments, 3)?, seg(*sides, 3)?);
            let at = |i: u32, j: u32| {
                let u = TAU * i as f64 / s as f64;
                let v = TAU * j as f64 / t as f64;
                let centre = DVec3::new(u.cos(), u.sin(), 0.0) * rm;
                let n = DVec3::new(u.cos() * v.cos(), u.sin() * v.cos(), v.sin());
                (centre + n * rn, n)
            };
            for i in 0..s {
                for j in 0..t {
                    let q = [at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)];
                    b.smooth_quad(q.map(|x| x.0), q.map(|x| x.1));
                }
            }
        }
        MeshSpec::Plane { size, divisions } => {
            let (w, d) = (pos(size[0])?, pos(size[1])?);
            let (nx, ny) = (seg(divisions[0].max(1), 1)?, seg(divisions[1].max(1), 1)?);
            for y in 0..ny {
                for x in 0..nx {
                    let p = |i: u32, j: u32| DVec3::new(-w / 2. + w * i as f64 / nx as f64, -d / 2. + d * j as f64 / ny as f64, 0.0);
                    b.polygon(&[p(x, y), p(x + 1, y), p(x + 1, y + 1), p(x, y + 1)]);
                }
            }
        }
        MeshSpec::Stairs { steps, run, rise, width } => {
            let (n, run, rise, w) = (seg(*steps, 1)?, pos(*run)?, pos(*rise)?, pos(*width)?);
            let (dx, dz) = (run / n as f64, rise / n as f64);
            let mut outline = vec![DVec2::new(0.0, 0.0), DVec2::new(run, 0.0), DVec2::new(run, rise)];
            for k in (0..n).rev() {
                let x = dx * k as f64;
                let z = dz * (k + 1) as f64;
                outline.push(DVec2::new(x + dx, z));
                outline.push(DVec2::new(x, z));
                if k > 0 {
                    outline.push(DVec2::new(x, z - dz));
                }
            }
            outline.dedup();
            profile_prism(&mut b, &outline, w)?;
        }
        MeshSpec::Ramp { length, width, height } => {
            let (l, w, h) = (pos(*length)?, pos(*width)?, pos(*height)?);
            profile_prism(&mut b, &[DVec2::new(0.0, 0.0), DVec2::new(l, 0.0), DVec2::new(l, h)], w)?;
        }
        MeshSpec::Arch { width, height, thickness, depth, segments } => {
            let (w, h, t, d, s) = (pos(*width)?, pos(*height)?, pos(*thickness)?, pos(*depth)?, seg(*segments, 2)?);
            let r_in = w / 2.0;
            if h <= r_in {
                return Err(MeshGenError::Bad("an arch must be taller than half its width"));
            }
            let spring = h - r_in;
            let mut outline = vec![DVec2::new(-r_in - t, 0.0), DVec2::new(-r_in, 0.0), DVec2::new(-r_in, spring)];
            for k in 1..s {
                let a = std::f64::consts::PI * (1.0 - k as f64 / s as f64);
                outline.push(DVec2::new(a.cos() * r_in, spring + a.sin() * r_in));
            }
            outline.extend([DVec2::new(r_in, spring), DVec2::new(r_in, 0.0), DVec2::new(r_in + t, 0.0), DVec2::new(r_in + t, h + t), DVec2::new(-r_in - t, h + t)]);
            let outline: Vec<DVec2> = outline.into_iter().rev().collect();
            profile_prism(&mut b, &outline, d)?;
        }
        MeshSpec::Extrude { outline, height } => {
            let h = pos(*height)?;
            let mut pts: Vec<DVec2> = outline.iter().map(|p| DVec2::new(p[0], p[1])).collect();
            check_outline(&pts)?;
            if signed_area(&pts) < 0.0 {
                pts.reverse();
            }
            let tris = ear_clip(&pts);
            for [a, c, e] in &tris {
                b.triangle(pts[*a].extend(h), pts[*c].extend(h), pts[*e].extend(h));
                b.triangle(pts[*e].extend(0.0), pts[*c].extend(0.0), pts[*a].extend(0.0));
            }
            for k in 0..pts.len() {
                let (a, c) = (pts[k], pts[(k + 1) % pts.len()]);
                b.polygon(&[a.extend(0.0), c.extend(0.0), c.extend(h), a.extend(h)]);
            }
        }
        MeshSpec::Lathe { profile, segments } => {
            let s = seg(*segments, 3)?;
            if profile.len() < 2 || profile.iter().any(|p| !p[0].is_finite() || !p[1].is_finite() || p[0] < 0.0) {
                return Err(MeshGenError::Bad("a lathe profile needs two or more points with x >= 0"));
            }
            for w in profile.windows(2) {
                for k in 0..s {
                    let rot = |p: [f64; 2], k: u32| {
                        let a = TAU * k as f64 / s as f64;
                        DVec3::new(p[0] * a.cos(), p[0] * a.sin(), p[1])
                    };
                    b.polygon(&[rot(w[0], k), rot(w[0], k + 1), rot(w[1], k + 1), rot(w[1], k)]);
                }
            }
        }
        MeshSpec::Loft { section, path, closed_section } => {
            if section.len() < 2 || path.len() < 2 {
                return Err(MeshGenError::Bad("a loft needs a section of 2+ points and a path of 2+ points"));
            }
            let path: Vec<DVec3> = path.iter().map(|p| DVec3::from_array(*p)).collect();
            let sec: Vec<DVec2> = section.iter().map(|p| DVec2::new(p[0], p[1])).collect();
            if path.iter().any(|p| !p.is_finite()) || sec.iter().any(|p| !p.is_finite()) {
                return Err(MeshGenError::Bad("loft points must be finite"));
            }
            let frames: Vec<Vec<DVec3>> = (0..path.len())
                .map(|i| {
                    let t = if i + 1 < path.len() { path[i + 1] - path[i] } else { path[i] - path[i - 1] }.normalize_or(DVec3::X);
                    let side = t.cross(DVec3::Z).normalize_or(DVec3::Y);
                    let up = side.cross(t);
                    sec.iter().map(|q| path[i] + side * q.x + up * q.y).collect()
                })
                .collect();
            let m = sec.len();
            let edges = if *closed_section { m } else { m - 1 };
            for i in 0..path.len() - 1 {
                for k in 0..edges {
                    let (a, c) = (k, (k + 1) % m);
                    b.polygon(&[frames[i][a], frames[i + 1][a], frames[i + 1][c], frames[i][c]]);
                }
            }
        }
        MeshSpec::Raw { positions, indices } => {
            if indices.len() % 3 != 0 || positions.len() > MAX_VERTICES {
                return Err(MeshGenError::Bad("raw indices must be a triangle list"));
            }
            let pts: Vec<DVec3> = positions.iter().map(|p| DVec3::from_array(*p)).collect();
            if pts.iter().any(|p| !p.is_finite()) {
                return Err(MeshGenError::Bad("raw positions must be finite"));
            }
            for t in indices.chunks_exact(3) {
                let get = |i: u32| pts.get(i as usize).copied().ok_or(MeshGenError::Bad("raw index out of range"));
                b.triangle(get(t[0])?, get(t[1])?, get(t[2])?);
            }
        }
    }
    b.finish(name, material)
}

fn check_outline(pts: &[DVec2]) -> Result<(), MeshGenError> {
    if pts.len() < 3 || pts.iter().any(|p| !p.is_finite()) || signed_area(pts).abs() < 1e-6 {
        return Err(MeshGenError::Bad("an outline needs three or more non-collinear finite points"));
    }
    Ok(())
}

/// Extrudes an XZ profile (x along X, y along Z) across Y by `width`.
fn profile_prism(b: &mut Builder, profile: &[DVec2], width: f64) -> Result<(), MeshGenError> {
    check_outline(profile)?;
    let mut pts = profile.to_vec();
    if signed_area(&pts) < 0.0 {
        pts.reverse();
    }
    let to3 = |p: DVec2, y: f64| DVec3::new(p.x, y, p.y);
    let tris = ear_clip(&pts);
    let (y0, y1) = (-width / 2.0, width / 2.0);
    for [a, c, e] in &tris {
        b.triangle(to3(pts[*a], y0), to3(pts[*c], y0), to3(pts[*e], y0));
        b.triangle(to3(pts[*e], y1), to3(pts[*c], y1), to3(pts[*a], y1));
    }
    for k in 0..pts.len() {
        let (a, c) = (pts[k], pts[(k + 1) % pts.len()]);
        b.polygon(&[to3(a, y1), to3(c, y1), to3(c, y0), to3(a, y0)]);
    }
    Ok(())
}

/// Everything needed to add one authored mesh to a document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshRequest {
    pub spec: MeshSpec,
    #[serde(default = "default_material")]
    pub material: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub transform: Option<Transform>,
    #[serde(default = "default_uv")]
    pub uv_scale: f64,
    #[serde(default = "default_collision")]
    pub collision: CollisionMode,
    /// Write a welded `.r5mesh` instead of a triangle-soup `.glb`.
    #[serde(default)]
    pub editable: bool,
}

fn default_material() -> String {
    crate::model::DEFAULT_MATERIAL.into()
}

fn default_uv() -> f64 {
    1.0 / crate::model::UV_TEXELS_PER_TILE
}

fn default_collision() -> CollisionMode {
    CollisionMode::Triangles
}

/// The command that writes `meshes/<id>.glb` and adds a `Mesh` object for it.
/// With `editable` it writes a welded `meshes/<id>.r5mesh` instead.
pub fn cmd_add_mesh(layer: Id, parent: Option<Id>, req: &MeshRequest) -> Result<Command, MeshGenError> {
    if req.editable {
        return crate::editmesh::convert::cmd_add_editable_mesh(layer, parent, req)
            .map_err(|e| MeshGenError::Editable(e.to_string()));
    }
    let label = req.name.clone().unwrap_or_else(|| shape_name(&req.spec).to_string());
    let mesh = build(&req.spec, &label, &req.material, req.uv_scale)?;
    let bytes = glb::write(&mesh)?;
    let mut material_map = indexmap::IndexMap::new();
    material_map.insert(req.material.clone(), req.material.clone());
    let kind = ObjectKind::Mesh(Mesh {
        source: String::new(),
        material_map,
        collision: req.collision,
        contents: 1,
        cast_shadows: true,
        lightmap_scale: 0.0,
        render: true,
        role: crate::model::MeshRole::Normal,
        modifiers: Vec::new(),
    });
    let mut object = new_object(label, layer, kind);
    let path = format!("meshes/{}.glb", object.id);
    if let ObjectKind::Mesh(m) = &mut object.kind {
        m.source = path.clone();
    }
    object.parent = parent;
    if let Some(t) = req.transform {
        object.transform = t;
    }
    Ok(Command::Batch {
        label: format!("mesh {}", shape_name(&req.spec)),
        commands: vec![Command::SetFile { path, bytes: Some(bytes) }, Command::AddObjects { objects: vec![object] }],
    })
}

pub fn shape_name(spec: &MeshSpec) -> &'static str {
    match spec {
        MeshSpec::Box { .. } => "box",
        MeshSpec::Cylinder { .. } => "cylinder",
        MeshSpec::Cone { .. } => "cone",
        MeshSpec::Sphere { .. } => "sphere",
        MeshSpec::Torus { .. } => "torus",
        MeshSpec::Plane { .. } => "plane",
        MeshSpec::Stairs { .. } => "stairs",
        MeshSpec::Ramp { .. } => "ramp",
        MeshSpec::Arch { .. } => "arch",
        MeshSpec::Extrude { .. } => "extrusion",
        MeshSpec::Lathe { .. } => "lathe",
        MeshSpec::Loft { .. } => "loft",
        MeshSpec::Raw { .. } => "mesh",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ear_clip_skips_zero_area_ears_on_straight_runs() {
        // Corners resting on straight runs, rotated so they are collinear
        // only up to rounding; the first candidate ear is such a corner.
        let raw = [(6.4, 0.0), (12.8, 0.0), (12.8, 48.0), (12.8, 112.0), (0.0, 112.0), (0.0, 56.0), (0.0, 0.0)];
        for step in 0..256 {
            let (sn, cs) = (step as f64 * 0.0245).sin_cos();
            let pts: Vec<DVec2> = raw.iter().map(|&(x, y)| DVec2::new(183.3 + x * cs - y * sn, 31.1 + x * sn + y * cs)).collect();
            let tris = ear_clip(&pts);
            assert_eq!(tris.len(), pts.len() - 2);
            for [a, b, c] in tris {
                let area = (pts[b] - pts[a]).perp_dot(pts[c] - pts[a]) * 0.5;
                assert!(area > 1.0, "step {step}: zero-area ear {a} {b} {c}: {area}");
            }
        }
    }

    type Point = (i64, i64, i64);

    fn closed(m: &GlbMesh) -> bool {
        // Positions are welded by value so flat-shaded faces count as shared edges.
        use std::collections::HashMap;
        let key = |i: u32| {
            let p = m.positions[i as usize];
            ((p[0] * 64.0).round() as i64, (p[1] * 64.0).round() as i64, (p[2] * 64.0).round() as i64)
        };
        let mut edges: HashMap<(Point, Point), i32> = HashMap::new();
        for t in m.indices.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let (ka, kb) = (key(a), key(b));
                *edges.entry((ka.min(kb), ka.max(kb))).or_default() += if ka < kb { 1 } else { -1 };
            }
        }
        edges.values().all(|v| *v == 0)
    }

    #[test]
    fn solids_are_closed_and_consistently_wound() {
        let specs = vec![
            MeshSpec::Box { size: [64.0, 32.0, 16.0] },
            MeshSpec::Cylinder { radius: 32.0, height: 64.0, segments: 16, caps: true },
            MeshSpec::Cone { radius: 32.0, height: 64.0, segments: 12 },
            MeshSpec::Stairs { steps: 8, run: 256.0, rise: 128.0, width: 96.0 },
            MeshSpec::Ramp { length: 256.0, width: 128.0, height: 64.0 },
            MeshSpec::Arch { width: 128.0, height: 160.0, thickness: 16.0, depth: 32.0, segments: 12 },
            MeshSpec::Extrude { outline: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 40.0], [40.0, 40.0], [40.0, 100.0], [0.0, 100.0]], height: 30.0 },
        ];
        for s in specs {
            let m = build(&s, "t", "m", 1.0 / 128.0).unwrap();
            assert!(closed(&m), "{} is not closed", shape_name(&s));
        }
    }

    #[test]
    fn box_normals_point_outward() {
        let m = build(&MeshSpec::Box { size: [2.0, 2.0, 2.0] }, "b", "m", 1.0).unwrap();
        for t in m.indices.chunks_exact(3) {
            let p = |i: u32| DVec3::from_array(m.positions[i as usize].map(|v| v as f64));
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let n = (b - a).cross(c - a);
            assert!(n.dot((a + b + c) / 3.0) > 0.0);
        }
    }

    #[test]
    fn bad_specs_are_rejected() {
        assert!(build(&MeshSpec::Box { size: [0.0, 1.0, 1.0] }, "b", "m", 1.0).is_err());
        assert!(build(&MeshSpec::Cylinder { radius: 1.0, height: 1.0, segments: 2, caps: true }, "b", "m", 1.0).is_err());
        assert!(build(&MeshSpec::Raw { positions: vec![[0.0; 3]], indices: vec![0, 0, 5] }, "b", "m", 1.0).is_err());
        assert!(build(&MeshSpec::Extrude { outline: vec![[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]], height: 1.0 }, "b", "m", 1.0).is_err());
    }

    #[test]
    fn add_mesh_command_writes_glb_and_object() {
        let layer = uuid::Uuid::new_v4();
        let req: MeshRequest = serde_json::from_str(r#"{"spec":{"shape":"stairs","steps":4,"run":128,"rise":64,"width":64}}"#).unwrap();
        let cmd = cmd_add_mesh(layer, None, &req).unwrap();
        let Command::Batch { commands, .. } = cmd else { panic!("not a batch") };
        let Command::SetFile { path, bytes: Some(bytes) } = &commands[0] else { panic!("no file") };
        assert!(path.starts_with("meshes/") && bytes.starts_with(b"glTF"));
        let back = glb::read(bytes).unwrap();
        assert!(!back.indices.is_empty());
    }
}
