//! M8 UV ops: box / cylinder / planar projection, conformal unwrap (LSCM),
//! island packing, reset, and the stretch report behind `mesh-info`.
//!
//! Every op rewrites corner UVs only; positions and topology never move, so
//! the geometry-derived lightmap charts (D7) are untouched. Seam marking
//! reuses the M1 `mark` op; these ops only add SEAM where a projection is
//! discontinuous.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

use super::{clamp_finite, parse_params, selected_faces, OpResult};
use crate::editmesh::select::{SelMode, Selection};
use crate::editmesh::{bad, Adjacency, EditError, EditMesh, EdgeFlags, MAX_CORNERS};

const COS_30: f64 = 0.8660254037844386;
const TAU: f64 = std::f64::consts::TAU;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UvScale {
    #[default]
    World,
    Fit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UvSpace {
    #[default]
    Object,
    World,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CylAxis {
    #[default]
    ObjectZ,
    WorldZ,
    Fit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CapMode {
    #[default]
    Planar,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanarAxis {
    X,
    Y,
    #[default]
    Z,
    NegX,
    NegY,
    NegZ,
    View,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UnwrapMethod {
    Conformal,
}

fn default_texels() -> f64 {
    128.0
}

fn default_margin() -> f64 {
    0.001
}

fn default_seam() -> f64 {
    180.0
}

fn clamp_texels(v: f64) -> Result<f64, EditError> {
    clamp_finite(v, 1.0, 65536.0, "texels_per_tile")
}

fn clamp_offset(v: [f64; 2]) -> Result<[f64; 2], EditError> {
    Ok([clamp_finite(v[0], -4096.0, 4096.0, "offset")?, clamp_finite(v[1], -4096.0, 4096.0, "offset")?])
}

fn clamp_margin(v: f64) -> Result<f64, EditError> {
    clamp_finite(v, 0.0, 0.45, "margin")
}

fn clamp_point(v: [f64; 3], name: &str) -> Result<DVec3, EditError> {
    let p = DVec3::new(
        clamp_finite(v[0], -1e6, 1e6, name)?,
        clamp_finite(v[1], -1e6, 1e6, name)?,
        clamp_finite(v[2], -1e6, 1e6, name)?,
    );
    Ok(p)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoxParams {
    #[serde(default)]
    scale: UvScale,
    #[serde(default = "default_texels")]
    texels_per_tile: f64,
    #[serde(default)]
    offset: [f64; 2],
    #[serde(default)]
    rotation: f64,
    #[serde(default)]
    space: UvSpace,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CylinderParams {
    #[serde(default)]
    axis: CylAxis,
    #[serde(default)]
    center: Option<[f64; 3]>,
    #[serde(default = "default_seam")]
    seam_angle_deg: f64,
    #[serde(default)]
    scale: UvScale,
    #[serde(default = "default_texels")]
    texels_per_tile: f64,
    #[serde(default)]
    cap: CapMode,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanarParams {
    #[serde(default)]
    axis: PlanarAxis,
    #[serde(default)]
    view_dir: Option<[f64; 3]>,
    #[serde(default)]
    scale: UvScale,
    #[serde(default = "default_texels")]
    texels_per_tile: f64,
    #[serde(default)]
    offset: [f64; 2],
    #[serde(default)]
    rotation: f64,
    #[serde(default)]
    space: UvSpace,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnwrapParams {
    #[serde(default = "conformal")]
    method: UnwrapMethod,
    #[serde(default = "yes")]
    fill_holes: bool,
    #[serde(default = "default_margin")]
    margin: f64,
    #[serde(default)]
    scale: UvScale,
    #[serde(default = "default_texels")]
    texels_per_tile: f64,
}

fn conformal() -> UnwrapMethod {
    UnwrapMethod::Conformal
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackParams {
    #[serde(default = "default_margin")]
    margin: f64,
    #[serde(default = "yes")]
    rotate: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetParams {}

/// The brush projection basis for a face normal, the same function
/// `brush.rs` uses, so a box-unwrapped mesh aligns with a brush beside it.
fn planar_axes(n: DVec3) -> (DVec3, DVec3) {
    let seed = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = n.cross(seed).normalize_or(DVec3::X);
    let v = n.cross(u).normalize_or(DVec3::Y);
    (u, v)
}

/// Index of the dominant axis, with the same priority as the mesh generator.
fn dominant_axis(n: DVec3) -> usize {
    let a = n.abs();
    if a.z >= a.x && a.z >= a.y {
        2
    } else if a.x >= a.y {
        0
    } else {
        1
    }
}

fn sanitize(v: f64) -> f32 {
    let f = v as f32;
    if f.is_finite() { f } else { 0.0 }
}

fn write_corner(mesh: &mut EditMesh, corner: usize, uv: DVec2) {
    mesh.corners[corner].uv = [sanitize(uv.x), sanitize(uv.y)];
}

fn corner_at(mesh: &EditMesh, face: u32, vertex: u32) -> Option<usize> {
    let f = &mesh.faces[face as usize];
    (f.first as usize..f.first as usize + f.len as usize)
        .find(|k| mesh.corners[*k].vertex == vertex)
}

/// Faces to work on: the selection, or everything when it names no faces.
fn target_faces(mesh: &EditMesh, adj: &Adjacency, sel: &Selection) -> BTreeSet<u32> {
    let mut faces = selected_faces(mesh, adj, sel);
    if faces.is_empty() {
        faces = (0..mesh.faces.len() as u32).collect();
    }
    faces
}

/// Face groups connected across non-seam edges, each sorted, islands sorted
/// by their lowest face, so every consumer below is deterministic.
fn islands(mesh: &EditMesh, adj: &Adjacency, faces: &BTreeSet<u32>) -> Vec<Vec<u32>> {
    let mut parent: BTreeMap<u32, u32> = faces.iter().map(|f| (*f, *f)).collect();
    fn find(parent: &mut BTreeMap<u32, u32>, mut x: u32) -> u32 {
        while parent[&x] != x {
            let p = parent[&x];
            let g = parent[&p];
            parent.insert(x, g);
            x = g;
        }
        x
    }
    for e in &adj.edges {
        if mesh.edge_flag(e.a, e.b).contains(EdgeFlags::SEAM) {
            continue;
        }
        let in_set: Vec<u32> = e.faces.iter().copied().filter(|f| faces.contains(f)).collect();
        for w in in_set.windows(2) {
            let (a, b) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
            if a != b {
                parent.insert(a.max(b), a.min(b));
            }
        }
    }
    let mut groups: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for f in faces {
        let r = find(&mut parent, *f);
        groups.entry(r).or_default().push(*f);
    }
    let mut out: Vec<Vec<u32>> = groups.into_values().collect();
    for g in &mut out {
        g.sort_unstable();
    }
    out.sort_by_key(|g| g[0]);
    out
}

/// Orthonormal face-plane basis with `u x v = n`.
fn face_basis(n: DVec3) -> (DVec3, DVec3) {
    let seed = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = n.cross(seed).normalize_or(DVec3::X);
    let v = n.cross(u).normalize_or(DVec3::Y);
    (u, v)
}

/// Corner-offset triangles of one face via the shared ear clipper.
fn face_tris(mesh: &EditMesh, fi: usize) -> Vec<[usize; 3]> {
    let f = &mesh.faces[fi];
    let n = f.len as usize;
    if n == 3 {
        return vec![[0, 1, 2]];
    }
    let normal = mesh.face_normal(fi);
    if normal == DVec3::ZERO {
        return Vec::new();
    }
    let (u, v) = face_basis(normal);
    let pts3: Vec<DVec3> = (0..n).map(|k| mesh.positions[mesh.corners[f.first as usize + k].vertex as usize]).collect();
    let c = pts3.iter().copied().sum::<DVec3>() / n as f64;
    let mut pts: Vec<DVec2> =
        pts3.iter().map(|p| DVec2::new((*p - c).dot(u), (*p - c).dot(v))).collect();
    let area: f64 = (0..n).map(|k| pts[k].perp_dot(pts[(k + 1) % n])).sum::<f64>() * 0.5;
    let flip = area < 0.0;
    if flip {
        pts.reverse();
    }
    crate::meshgen::ear_clip(&pts)
        .into_iter()
        .map(|[a, b, c]| if flip { [n - 1 - a, n - 1 - b, n - 1 - c] } else { [a, b, c] })
        .collect()
}

/// Largest-eigenvalue axis of the point cloud, by fixed-count power
/// iteration from a fixed start, so the result is deterministic.
fn principal_axis(pts: &[DVec3]) -> DVec3 {
    if pts.is_empty() {
        return DVec3::Z;
    }
    let c = pts.iter().copied().sum::<DVec3>() / pts.len() as f64;
    let mut cov = [[0.0f64; 3]; 3];
    for p in pts {
        let d = (*p - c).to_array();
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    let mut v = DVec3::new(1.0, 1.0, 1.0).normalize();
    for _ in 0..200 {
        let w = DVec3::new(
            cov[0][0] * v.x + cov[0][1] * v.y + cov[0][2] * v.z,
            cov[1][0] * v.x + cov[1][1] * v.y + cov[1][2] * v.z,
            cov[2][0] * v.x + cov[2][1] * v.y + cov[2][2] * v.z,
        );
        if w.length_squared() < 1e-24 {
            break;
        }
        v = w.normalize();
    }
    if !v.is_finite() {
        v = DVec3::Z;
    }
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for p in pts {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    let e = hi - lo;
    if e.length_squared() < 1e-18 {
        return DVec3::Z;
    }
    if v.length_squared() < 0.5 || !v.is_finite() {
        v = if e.x >= e.y && e.x >= e.z {
            DVec3::X
        } else if e.y >= e.z {
            DVec3::Y
        } else {
            DVec3::Z
        };
    }
    v.normalize_or(DVec3::Z)
}

fn selection_of(faces: &BTreeSet<u32>) -> Selection {
    Selection { mode: SelMode::Face, ids: faces.iter().copied().collect() }
}

fn face_normals(mesh: &EditMesh, faces: &BTreeSet<u32>) -> BTreeMap<u32, DVec3> {
    faces.iter().map(|f| (*f, mesh.face_normal(*f as usize))).collect()
}

fn run_box(mesh: &mut EditMesh, adj: &Adjacency, faces: &BTreeSet<u32>, params: &serde_json::Value) -> OpResult {
    let p: BoxParams = parse_params(params)?;
    let t = clamp_texels(p.texels_per_tile)?;
    let offset = clamp_offset(p.offset)?;
    let rotation = clamp_finite(p.rotation, -360.0, 360.0, "rotation")?;
    if p.space == UvSpace::World {
        return Err(bad("space world needs the document; it runs through the op request, not bare dispatch"));
    }
    let s = t / crate::model::UV_TEXELS_PER_TILE;
    let normals = face_normals(mesh, faces);
    let project = |n: DVec3, pnt: DVec3| -> DVec2 {
        let (du, dv) = planar_axes(n);
        let uv = crate::brush::project_uv(
            &crate::model::UvMapping { u_axis: du, v_axis: dv, offset: [0.0, 0.0], scale: [s, s], rotation },
            n,
            pnt,
        );
        let tiled = crate::brush::tile_uv(uv);
        DVec2::new(tiled[0] + offset[0], tiled[1] + offset[1])
    };
    if p.scale == UvScale::Fit {
        let mut lo = DVec2::splat(f64::INFINITY);
        let mut hi = DVec2::splat(f64::NEG_INFINITY);
        let mut per_corner: BTreeMap<usize, DVec2> = BTreeMap::new();
        for f in faces {
            let face = &mesh.faces[*f as usize];
            for k in face.first as usize..face.first as usize + face.len as usize {
                let uv = project(normals[f], mesh.positions[mesh.corners[k].vertex as usize]);
                per_corner.insert(k, uv);
                lo = lo.min(uv);
                hi = hi.max(uv);
            }
        }
        let e = hi - lo;
        let m = e.x.max(e.y);
        if !m.is_finite() || m <= 1e-18 {
            return Err(bad("the selection has no extent to fit"));
        }
        for (k, uv) in per_corner {
            let fit = DVec2::new((uv.x - lo.x) / m, (uv.y - lo.y) / m);
            write_corner(mesh, k, fit);
        }
    } else {
        for f in faces {
            let face = mesh.faces[*f as usize];
            for k in face.first as usize..face.first as usize + face.len as usize {
                let uv = project(normals[f], mesh.positions[mesh.corners[k].vertex as usize]);
                write_corner(mesh, k, uv);
            }
        }
    }
    let axes: BTreeMap<u32, usize> = normals.iter().map(|(f, n)| (*f, dominant_axis(*n))).collect();
    for e in &adj.edges {
        if e.faces.len() != 2 || !e.faces.iter().any(|f| faces.contains(f)) {
            continue;
        }
        let (a, b) = (axes.get(&e.faces[0]), axes.get(&e.faces[1]));
        if a != b {
            mesh.set_edge_flag(e.a, e.b, EdgeFlags::SEAM, true);
        }
    }
    Ok((selection_of(faces), Vec::new()))
}

struct CylFrame {
    axis: DVec3,
    b1: DVec3,
    b2: DVec3,
    center: DVec3,
}

fn cyl_radial(frame: &CylFrame, p: DVec3) -> (f64, f64, f64) {
    let rel = p - frame.center;
    let h = rel.dot(frame.axis);
    let radial = rel - frame.axis * h;
    let theta = radial.dot(frame.b2).atan2(radial.dot(frame.b1));
    (theta, h, radial.length())
}

fn run_cylinder(mesh: &mut EditMesh, adj: &Adjacency, faces: &BTreeSet<u32>, params: &serde_json::Value) -> OpResult {
    let p: CylinderParams = parse_params(params)?;
    if p.axis == CylAxis::WorldZ {
        return Err(bad("axis world_z needs the document; it runs through the op request, not bare dispatch"));
    }
    let t = clamp_texels(p.texels_per_tile)?;
    let seam = clamp_finite(p.seam_angle_deg, -720.0, 720.0, "seam_angle_deg")?.to_radians();
    let axis = match p.axis {
        CylAxis::ObjectZ | CylAxis::WorldZ => DVec3::Z,
        CylAxis::Fit => {
            let pts: Vec<DVec3> = faces
                .iter()
                .flat_map(|f| mesh.face_points(*f as usize))
                .collect();
            principal_axis(&pts)
        }
    };
    let seed = if axis.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let b1 = axis.cross(seed).normalize_or(DVec3::X);
    let b2 = axis.cross(b1).normalize_or(DVec3::Y);
    let center = match p.center {
        Some(c) => clamp_point(c, "center")?,
        None => {
            let pts: Vec<DVec3> =
                faces.iter().flat_map(|f| mesh.face_points(*f as usize)).collect();
            if pts.is_empty() {
                return Err(bad("the selection is empty"));
            }
            pts.iter().copied().sum::<DVec3>() / pts.len() as f64
        }
    };
    if !center.is_finite() {
        return Err(bad("the selection center is not finite"));
    }
    let frame = CylFrame { axis, b1, b2, center };
    let normals = face_normals(mesh, faces);
    let is_cap = |f: u32| normals.get(&f).is_some_and(|n| n.dot(axis).abs() > COS_30) && p.cap == CapMode::Planar;
    let mut per_corner: BTreeMap<usize, DVec2> = BTreeMap::new();
    for f in faces {
        let face = mesh.faces[*f as usize];
        if is_cap(*f) {
            for k in face.first as usize..face.first as usize + face.len as usize {
                let rel = mesh.positions[mesh.corners[k].vertex as usize] - center;
                per_corner.insert(k, DVec2::new(rel.dot(b1), rel.dot(b2)));
            }
            continue;
        }
        let keys: Vec<usize> = (face.first as usize..face.first as usize + face.len as usize).collect();
        let mut thetas: Vec<f64> = keys
            .iter()
            .map(|k| cyl_radial(&frame, mesh.positions[mesh.corners[*k].vertex as usize]).0)
            .collect();
        for i in 1..thetas.len() {
            while thetas[i] - thetas[i - 1] > std::f64::consts::PI {
                thetas[i] -= TAU;
            }
            while thetas[i] - thetas[i - 1] < -std::f64::consts::PI {
                thetas[i] += TAU;
            }
        }
        let avg = thetas.iter().sum::<f64>() / thetas.len() as f64;
        let turns = -((avg - seam) / TAU).floor();
        for (k, theta) in keys.iter().zip(thetas) {
            let (_, h, r) = cyl_radial(&frame, mesh.positions[mesh.corners[*k].vertex as usize]);
            per_corner.insert(*k, DVec2::new((theta + turns * TAU - seam) * r, h));
        }
    }
    if p.scale == UvScale::Fit {
        let mut lo = DVec2::splat(f64::INFINITY);
        let mut hi = DVec2::splat(f64::NEG_INFINITY);
        for uv in per_corner.values() {
            lo = lo.min(*uv);
            hi = hi.max(*uv);
        }
        let e = hi - lo;
        let m = e.x.max(e.y);
        if !m.is_finite() || m <= 1e-18 {
            return Err(bad("the selection has no extent to fit"));
        }
        for uv in per_corner.values_mut() {
            *uv = DVec2::new((uv.x - lo.x) / m, (uv.y - lo.y) / m);
        }
    } else {
        for uv in per_corner.values_mut() {
            *uv /= t;
        }
    }
    for (k, uv) in &per_corner {
        write_corner(mesh, *k, *uv);
    }
    let mut caps: BTreeMap<u32, bool> = BTreeMap::new();
    for f in 0..mesh.faces.len() as u32 {
        caps.insert(f, mesh.face_normal(f as usize).dot(axis).abs() > COS_30 && p.cap == CapMode::Planar);
    }
    for e in &adj.edges {
        if !e.faces.iter().any(|f| faces.contains(f)) {
            continue;
        }
        let kinds: Vec<bool> = e.faces.iter().map(|f| caps[f]).collect();
        if kinds.iter().any(|k| *k) && kinds.iter().any(|k| !k) {
            mesh.set_edge_flag(e.a, e.b, EdgeFlags::SEAM, true);
            continue;
        }
        if kinds.iter().all(|k| !k) && e.faces.len() == 2 {
            let mid = |f: u32| -> Option<(f64, f64)> {
                let ka = corner_at(mesh, f, e.a)?;
                let kb = corner_at(mesh, f, e.b)?;
                let ua = mesh.corners[ka].uv[0] as f64;
                let ub = mesh.corners[kb].uv[0] as f64;
                let ra = cyl_radial(&frame, mesh.positions[e.a as usize]).2;
                let rb = cyl_radial(&frame, mesh.positions[e.b as usize]).2;
                Some(((ua + ub) * 0.5, (ra + rb) * 0.5))
            };
            if let (Some((u0, r0)), Some((u1, r1))) = (mid(e.faces[0]), mid(e.faces[1])) {
                let r = (r0 + r1) * 0.5;
                if r > 1e-12 && (u0 - u1).abs() * t > std::f64::consts::PI * r {
                    mesh.set_edge_flag(e.a, e.b, EdgeFlags::SEAM, true);
                }
            }
        }
    }
    Ok((selection_of(faces), Vec::new()))
}

fn run_planar(mesh: &mut EditMesh, faces: &BTreeSet<u32>, params: &serde_json::Value) -> OpResult {
    let p: PlanarParams = parse_params(params)?;
    if p.space == UvSpace::World {
        return Err(bad("space world needs the document; it runs through the op request, not bare dispatch"));
    }
    let t = clamp_texels(p.texels_per_tile)?;
    let offset = clamp_offset(p.offset)?;
    let rotation = clamp_finite(p.rotation, -360.0, 360.0, "rotation")?;
    let dir = match p.axis {
        PlanarAxis::X => DVec3::X,
        PlanarAxis::Y => DVec3::Y,
        PlanarAxis::Z => DVec3::Z,
        PlanarAxis::NegX => DVec3::NEG_X,
        PlanarAxis::NegY => DVec3::NEG_Y,
        PlanarAxis::NegZ => DVec3::NEG_Z,
        PlanarAxis::View => match p.view_dir {
            Some(d) => {
                let v = clamp_point(d, "view_dir")?;
                if v.length_squared() < 1e-24 {
                    return Err(bad("view_dir is zero"));
                }
                v.normalize()
            }
            None => DVec3::Z,
        },
    };
    let s = t / crate::model::UV_TEXELS_PER_TILE;
    let (du, dv) = planar_axes(dir);
    let mapping =
        crate::model::UvMapping { u_axis: du, v_axis: dv, offset: [0.0, 0.0], scale: [s, s], rotation };
    let mut per_corner: BTreeMap<usize, DVec2> = BTreeMap::new();
    let mut lo = DVec2::splat(f64::INFINITY);
    let mut hi = DVec2::splat(f64::NEG_INFINITY);
    for f in faces {
        let face = mesh.faces[*f as usize];
        for k in face.first as usize..face.first as usize + face.len as usize {
            let projected = crate::brush::project_uv(&mapping, dir, mesh.positions[mesh.corners[k].vertex as usize]);
            let tiled = crate::brush::tile_uv(projected);
            let uv = DVec2::new(tiled[0] + offset[0], tiled[1] + offset[1]);
            per_corner.insert(k, uv);
            lo = lo.min(uv);
            hi = hi.max(uv);
        }
    }
    if p.scale == UvScale::Fit {
        let e = hi - lo;
        let m = e.x.max(e.y);
        if !m.is_finite() || m <= 1e-18 {
            return Err(bad("the selection has no extent to fit"));
        }
        for uv in per_corner.values_mut() {
            *uv = DVec2::new((uv.x - lo.x) / m, (uv.y - lo.y) / m);
        }
    }
    for (k, uv) in &per_corner {
        write_corner(mesh, *k, *uv);
    }
    Ok((selection_of(faces), Vec::new()))
}

struct CgRow {
    cols: Vec<(usize, f64)>,
    rhs: f64,
}

fn cg_solve(rows: &[CgRow], n: usize) -> Vec<f64> {
    let apply = |x: &[f64]| -> Vec<f64> {
        let mut ax = vec![0.0; rows.len()];
        for (i, r) in rows.iter().enumerate() {
            ax[i] = r.cols.iter().map(|(c, v)| x[*c] * v).sum();
        }
        ax
    };
    let transpose = |y: &[f64]| -> Vec<f64> {
        let mut out = vec![0.0; n];
        for (r, w) in rows.iter().zip(y) {
            for (c, v) in &r.cols {
                out[*c] += v * w;
            }
        }
        out
    };
    let rhs: Vec<f64> = rows.iter().map(|r| r.rhs).collect();
    let mut x = vec![0.0; n];
    let mut r = transpose(&rhs);
    let mut p = r.clone();
    let mut rs: f64 = r.iter().map(|v| v * v).sum();
    if !rs.is_finite() || rs == 0.0 {
        return x;
    }
    let target = rs * 1e-20;
    let iters = n.min(5000);
    for _ in 0..iters {
        let ap = transpose(&apply(&p));
        let pap: f64 = p.iter().zip(&ap).map(|(a, b)| a * b).sum();
        if !pap.is_finite() || pap <= 1e-30 {
            break;
        }
        let alpha = rs / pap;
        for (xi, pi) in x.iter_mut().zip(&p) {
            *xi += alpha * pi;
        }
        for (ri, api) in r.iter_mut().zip(&ap) {
            *ri -= alpha * api;
        }
        let rs_new: f64 = r.iter().map(|v| v * v).sum();
        if !rs_new.is_finite() || rs_new <= target {
            break;
        }
        let beta = rs_new / rs;
        for (pi, ri) in p.iter_mut().zip(&r) {
            *pi = *ri + beta * *pi;
        }
        rs = rs_new;
    }
    x
}

const MAX_UNWRAP_TRIS: usize = 262144;
const MAX_UNWRAP_VERTS: usize = 65536;

fn farthest_from(pts: &[DVec3], from: DVec3, skip: Option<usize>) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, p) in pts.iter().enumerate() {
        if Some(i) == skip {
            continue;
        }
        let d = (*p - from).length_squared();
        if best.is_none_or(|(_, bd)| d > bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

fn unwrap_island(
    mesh: &EditMesh,
    adj: &Adjacency,
    island: &[u32],
    fill_holes: bool,
) -> Result<(BTreeMap<usize, DVec2>, f64), EditError> {
    let in_island: BTreeSet<u32> = island.iter().copied().collect();
    let mut corner_of: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for f in island {
        let face = &mesh.faces[*f as usize];
        for k in face.first as usize..face.first as usize + face.len as usize {
            corner_of.insert((*f, mesh.corners[k].vertex), k);
        }
    }
    // One unknown per corner sector: corners at one vertex join across shared
    // non-seam edges, so a slit side unwraps apart from its welded twin.
    let mut parent: BTreeMap<usize, usize> = corner_of.values().map(|k| (*k, *k)).collect();
    fn find(parent: &mut BTreeMap<usize, usize>, mut x: usize) -> usize {
        while parent[&x] != x {
            let p = parent[&x];
            let g = parent[&p];
            parent.insert(x, g);
            x = g;
        }
        x
    }
    let unite = |a: usize, b: usize, parent: &mut BTreeMap<usize, usize>| {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent.insert(ra.max(rb), ra.min(rb));
        }
    };
    for f in island {
        let face = &mesh.faces[*f as usize];
        let mut by_vert: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for k in face.first as usize..face.first as usize + face.len as usize {
            by_vert.entry(mesh.corners[k].vertex).or_default().push(k);
        }
        for ks in by_vert.values() {
            for w in ks.windows(2) {
                unite(w[0], w[1], &mut parent);
            }
        }
    }
    for e in &adj.edges {
        if mesh.edge_flag(e.a, e.b).contains(EdgeFlags::SEAM) {
            continue;
        }
        let here: Vec<u32> = e.faces.iter().copied().filter(|f| in_island.contains(f)).collect();
        for w in here.windows(2) {
            for v in [e.a, e.b] {
                if let (Some(ka), Some(kb)) = (corner_of.get(&(w[0], v)), corner_of.get(&(w[1], v))) {
                    unite(*ka, *kb, &mut parent);
                }
            }
        }
    }
    let mut root_of: BTreeMap<usize, usize> = BTreeMap::new();
    for k in parent.keys().copied().collect::<Vec<_>>() {
        root_of.insert(k, find(&mut parent, k));
    }
    let mut roots: Vec<usize> = root_of.values().copied().collect();
    roots.sort_unstable();
    roots.dedup();
    if roots.len() > MAX_UNWRAP_VERTS {
        return Err(bad("the island is larger than the unwrap budget"));
    }
    let index_of = |r: usize| -> usize { roots.binary_search(&r).unwrap_or(usize::MAX) };
    let mut first: BTreeMap<usize, usize> = BTreeMap::new();
    for (k, r) in &root_of {
        first.entry(*r).or_insert(*k);
    }
    let mut solve_pts: Vec<DVec3> =
        roots.iter().map(|r| mesh.positions[mesh.corners[first[r]].vertex as usize]).collect();
    let mut tris: Vec<[usize; 3]> = Vec::new();
    for f in island {
        for [x, y, z] in face_tris(mesh, *f as usize) {
            let face = &mesh.faces[*f as usize];
            let vs = [
                index_of(root_of[&(face.first as usize + x)]),
                index_of(root_of[&(face.first as usize + y)]),
                index_of(root_of[&(face.first as usize + z)]),
            ];
            if vs.contains(&usize::MAX) {
                continue;
            }
            tris.push(vs);
        }
    }
    if fill_holes {
        let mut counts: BTreeMap<(u32, u32), usize> = BTreeMap::new();
        let mut edge_face: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        for f in island {
            for [x, y, z] in face_tris(mesh, *f as usize) {
                let face = &mesh.faces[*f as usize];
                let v = |o: usize| mesh.corners[face.first as usize + o].vertex;
                for (p, q) in [(v(x), v(y)), (v(y), v(z)), (v(z), v(x))] {
                    let key = (p.min(q), p.max(q));
                    *counts.entry(key).or_insert(0) += 1;
                    edge_face.insert(key, *f);
                }
            }
        }
        let boundary: BTreeSet<(u32, u32)> =
            counts.into_iter().filter(|(_, n)| *n == 1).map(|(e, _)| e).collect();
        // Hole fill scaffolds interior loops only: fanning the outer boundary
        // would close the island into a surface that cannot flatten.
        let loops: Vec<Vec<u32>> = super::order_edge_loops(&boundary)
            .into_iter()
            .filter(|chain| super::loop_is_closed(chain, &boundary) && chain.len() >= 3)
            .collect();
        let longest = loops.iter().map(|c| c.len()).max().unwrap_or(0);
        for chain in loops.iter().filter(|c| c.len() < longest) {
            let centroid =
                chain.iter().map(|v| mesh.positions[*v as usize]).sum::<DVec3>() / chain.len() as f64;
            if !centroid.is_finite() {
                continue;
            }
            let ci = solve_pts.len();
            solve_pts.push(centroid);
            for k in 0..chain.len() {
                let (p, q) = (chain[k], chain[(k + 1) % chain.len()]);
                let key = (p.min(q), p.max(q));
                let Some(f) = edge_face.get(&key) else { continue };
                let (Some(ka), Some(kb)) = (corner_of.get(&(*f, p)), corner_of.get(&(*f, q))) else { continue };
                let (a, b) = (index_of(root_of[ka]), index_of(root_of[kb]));
                if a != usize::MAX && b != usize::MAX {
                    tris.push([ci, a, b]);
                }
            }
        }
    }
    if tris.len() > MAX_UNWRAP_TRIS {
        return Err(bad("the island is larger than the unwrap budget"));
    }
    let real = roots.len();
    let mesh_verts: Vec<u32> = {
        let mut vs: BTreeSet<u32> = BTreeSet::new();
        for f in island {
            vs.extend(mesh.face_verts(*f as usize));
        }
        vs.into_iter().collect()
    };
    let pts: Vec<DVec3> = mesh_verts.iter().map(|v| mesh.positions[*v as usize]).collect();
    let ai = farthest_from(&pts, pts[0], None).unwrap_or(0);
    let bi = farthest_from(&pts, pts[ai], Some(ai)).unwrap_or(ai);
    let root_of_vert = |v: u32| -> Option<usize> {
        corner_of.iter().find(|((_, w), _)| *w == v).map(|(_, k)| root_of[k])
    };
    let (Some(ra), Some(rb)) = (root_of_vert(mesh_verts[ai]), root_of_vert(mesh_verts[bi])) else {
        return Err(bad("the island is degenerate"));
    };
    if ra == rb {
        return Err(bad("the island is degenerate"));
    }
    let pin_uv = [(index_of(ra), DVec2::ZERO), (index_of(rb), DVec2::X)];
    if pin_uv.iter().any(|(s, _)| *s == usize::MAX) {
        return Err(bad("the island is degenerate"));
    }
    let is_pinned = |s: usize| -> Option<DVec2> {
        pin_uv.iter().find(|(w, _)| *w == s).map(|(_, uv)| *uv)
    };
    let mut slot: BTreeMap<usize, usize> = BTreeMap::new();
    for s in 0..solve_pts.len() {
        if is_pinned(s).is_none() {
            slot.insert(s, slot.len() * 2);
        }
    }
    let n = slot.len() * 2;
    let mut rows: Vec<CgRow> = Vec::with_capacity(tris.len() * 2);
    let mut world_area = 0.0;
    for [i, j, k] in &tris {
        let (p0, p1, p2) = (solve_pts[*i], solve_pts[*j], solve_pts[*k]);
        let e1 = p1 - p0;
        let d = e1.length();
        if d < 1e-18 {
            continue;
        }
        let h1 = e1 / d;
        let e2 = p2 - p0;
        let e = e2.dot(h1);
        let perp = e2 - h1 * e;
        let f = perp.length();
        if f < 1e-18 {
            continue;
        }
        let temporary = *i >= real || *j >= real || *k >= real;
        if !temporary {
            world_area += d * f * 0.5;
        }
        let w = 1.0 / (2.0 * d * f).sqrt();
        let vs = [*i, *j, *k];
        let cu = [-f * w, f * w, 0.0];
        let cv = [-(d - e) * w, -e * w, d * w];
        let du = [(e - d) * w, -e * w, d * w];
        let dv = [f * w, -f * w, 0.0];
        for (cu_row, cv_row) in [(cu, cv), (du, dv)] {
            let mut row = CgRow { cols: Vec::with_capacity(6), rhs: 0.0 };
            for (m, c) in cu_row.iter().zip(&vs) {
                match is_pinned(*c) {
                    Some(pin) => row.rhs -= m * pin.x,
                    None => row.cols.push((slot[c], *m)),
                }
            }
            for (m, c) in cv_row.iter().zip(&vs) {
                match is_pinned(*c) {
                    Some(pin) => row.rhs -= m * pin.y,
                    None => row.cols.push((slot[c] + 1, *m)),
                }
            }
            rows.push(row);
        }
    }
    if rows.is_empty() {
        return Err(bad("the island has no area to unwrap"));
    }
    let x = cg_solve(&rows, n);
    let mut uv: BTreeMap<usize, DVec2> = BTreeMap::new();
    for (k, r) in &root_of {
        let s = index_of(*r);
        if s == usize::MAX {
            continue;
        }
        match is_pinned(s) {
            Some(pin) => {
                uv.insert(*k, pin);
            }
            None => {
                if let Some(col) = slot.get(&s) {
                    uv.insert(*k, DVec2::new(x[*col], x[*col + 1]));
                }
            }
        }
    }
    Ok((uv, world_area))
}

fn run_unwrap(mesh: &mut EditMesh, adj: &Adjacency, faces: &BTreeSet<u32>, params: &serde_json::Value) -> OpResult {
    let p: UnwrapParams = parse_params(params)?;
    let margin = clamp_margin(p.margin)?;
    let t = clamp_texels(p.texels_per_tile)?;
    let mut warnings = Vec::new();
    for island in islands(mesh, adj, faces) {
        match unwrap_island(mesh, adj, &island, p.fill_holes) {
            Err(e) => warnings.push(format!("island starting at face {} skipped: {e}", island[0])),
            Ok((mut uv, world_area)) => {
                let mut signed = 0.0;
                for f in &island {
                    for [x, y, z] in face_tris(mesh, *f as usize) {
                        let face = &mesh.faces[*f as usize];
                        let at = |o: usize| uv[&(face.first as usize + o)];
                        let (a, b, c) = (at(x), at(y), at(z));
                        signed += (b - a).perp_dot(c - a) * 0.5;
                    }
                }
                let centroid = uv.values().copied().sum::<DVec2>() / uv.len() as f64;
                if signed < 0.0 {
                    for v in uv.values_mut() {
                        v.y = 2.0 * centroid.y - v.y;
                    }
                }
                match p.scale {
                    UvScale::World => {
                        let have: f64 = uv_area_of(mesh, &island, &uv);
                        let want = world_area / (t * t);
                        let s = if have > 1e-24 && want.is_finite() { (want / have).sqrt() } else { 1.0 };
                        let s = if s.is_finite() { s * (1.0 - margin) } else { 1.0 };
                        for v in uv.values_mut() {
                            *v = centroid + (*v - centroid) * s;
                        }
                    }
                    UvScale::Fit => {
                        let mut lo = DVec2::splat(f64::INFINITY);
                        let mut hi = DVec2::splat(f64::NEG_INFINITY);
                        for v in uv.values() {
                            lo = lo.min(*v);
                            hi = hi.max(*v);
                        }
                        let e = hi - lo;
                        let m = e.x.max(e.y);
                        if !m.is_finite() || m <= 1e-18 {
                            warnings.push(format!("island starting at face {} has no extent", island[0]));
                            continue;
                        }
                        for v in uv.values_mut() {
                            let q = DVec2::new((v.x - lo.x) / m, (v.y - lo.y) / m);
                            let q = DVec2::new(q.x + (1.0 - e.x / m) * 0.5, q.y + (1.0 - e.y / m) * 0.5);
                            *v = DVec2::new(0.5, 0.5) + (q - DVec2::new(0.5, 0.5)) * (1.0 - margin);
                        }
                    }
                }
                for f in &island {
                    let face = mesh.faces[*f as usize];
                    for k in face.first as usize..face.first as usize + face.len as usize {
                        if let Some(v) = uv.get(&k) {
                            write_corner(mesh, k, *v);
                        }
                    }
                }
            }
        }
    }
    Ok((selection_of(faces), warnings))
}

fn uv_area_of(mesh: &EditMesh, island: &[u32], uv: &BTreeMap<usize, DVec2>) -> f64 {
    let mut area = 0.0;
    for f in island {
        for [x, y, z] in face_tris(mesh, *f as usize) {
            let face = &mesh.faces[*f as usize];
            let at = |o: usize| uv.get(&(face.first as usize + o)).copied().unwrap_or(DVec2::ZERO);
            let (a, b, c) = (at(x), at(y), at(z));
            area += ((b - a).perp_dot(c - a) * 0.5).abs();
        }
    }
    area
}

fn run_pack(mesh: &mut EditMesh, adj: &Adjacency, faces: &BTreeSet<u32>, params: &serde_json::Value) -> OpResult {
    let p: PackParams = parse_params(params)?;
    let margin = clamp_margin(p.margin)?;
    let g = margin.max(1e-6);
    struct Item {
        faces: Vec<u32>,
        min: DVec2,
        w: f64,
        h: f64,
    }
    let mut items: Vec<Item> = Vec::new();
    for island in islands(mesh, adj, faces) {
        let mut lo = DVec2::splat(f64::INFINITY);
        let mut hi = DVec2::splat(f64::NEG_INFINITY);
        for f in &island {
            let face = &mesh.faces[*f as usize];
            for k in face.first as usize..face.first as usize + face.len as usize {
                let v = DVec2::new(mesh.corners[k].uv[0] as f64, mesh.corners[k].uv[1] as f64);
                if v.is_finite() {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
        }
        if !lo.is_finite() {
            lo = DVec2::ZERO;
            hi = DVec2::ZERO;
        }
        let e = hi - lo;
        items.push(Item { faces: island, min: lo, w: e.x.max(0.0), h: e.y.max(0.0) });
    }
    if items.is_empty() {
        return Err(bad("the selection has no faces to pack"));
    }
    let biggest = items.iter().map(|it| it.w.max(it.h)).fold(0.0f64, f64::max);
    let pre = if biggest.is_finite() && biggest > 1e-18 { 1.0 / biggest } else { 1.0 };
    items.sort_by(|a, b| {
        (b.w * b.h)
            .partial_cmp(&(a.w * a.h))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.faces[0].cmp(&b.faces[0]))
    });
    struct Placed {
        x: f64,
        y: f64,
        w: f64,
        rotated: bool,
    }
    let mut placed: Vec<Placed> = Vec::with_capacity(items.len());
    let mut row_y = 0.0;
    let mut row_h = 0.0;
    let mut row_x = 0.0;
    for it in &items {
        let (w0, h0) = (it.w * pre, it.h * pre);
        let fits = |w: f64, _h: f64| row_x + w <= 1.0 + 1e-9;
        let mut rotated = false;
        if p.rotate && !fits(w0, h0) && fits(h0, w0) {
            rotated = true;
        }
        let (mut w, mut h) = if rotated { (h0, w0) } else { (w0, h0) };
        if row_h > 0.0 && !fits(w, h) {
            row_y += row_h + g;
            row_x = 0.0;
            row_h = 0.0;
            if p.rotate {
                if h0 < w0 {
                    rotated = true;
                    w = h0;
                    h = w0;
                } else {
                    rotated = false;
                    w = w0;
                    h = h0;
                }
            }
        }
        if row_h == 0.0 {
            row_h = h;
        } else {
            row_h = row_h.max(h);
        }
        placed.push(Placed { x: row_x, y: row_y, w, rotated });
        row_x += w + g;
    }
    let used_w = placed.iter().map(|q| q.x + q.w).fold(0.0f64, f64::max);
    let used_h = row_y + row_h;
    let most = used_w.max(used_h);
    let k = if most.is_finite() && most > 1.0 { 1.0 / most } else { 1.0 };
    for (it, q) in items.iter().zip(&placed) {
        for f in &it.faces {
            let face = mesh.faces[*f as usize];
            for ci in face.first as usize..face.first as usize + face.len as usize {
                let raw = DVec2::new(mesh.corners[ci].uv[0] as f64, mesh.corners[ci].uv[1] as f64);
                let mut local = (raw - it.min) * pre;
                if !local.is_finite() {
                    local = DVec2::ZERO;
                }
                if q.rotated {
                    local = DVec2::new(local.y, it.w * pre - local.x);
                }
                write_corner(mesh, ci, DVec2::new((local.x + q.x) * k, (local.y + q.y) * k));
            }
        }
    }
    Ok((selection_of(faces), Vec::new()))
}

fn run_reset(mesh: &mut EditMesh, faces: &BTreeSet<u32>, params: &serde_json::Value) -> OpResult {
    let _: ResetParams = parse_params(params)?;
    for f in faces {
        let face = mesh.faces[*f as usize];
        for k in face.first as usize..face.first as usize + face.len as usize {
            mesh.corners[k].uv = [0.0, 0.0];
        }
    }
    Ok((selection_of(faces), Vec::new()))
}

/// One island's stretch: UV area over 3D area, and the worst angle error.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IslandStretch {
    pub faces: u32,
    pub area_ratio: f64,
    pub angle_deg: f64,
}

/// Worst islands first, for the `mesh-info` report.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StretchReport {
    pub islands: u32,
    pub worst: Vec<IslandStretch>,
}

fn sanitize_report(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

fn corner_angle(a: DVec3, b: DVec3, c: DVec3) -> f64 {
    let (u, v) = ((a - b).normalize_or_zero(), (c - b).normalize_or_zero());
    if u == DVec3::ZERO || v == DVec3::ZERO {
        return 0.0;
    }
    u.dot(v).clamp(-1.0, 1.0).acos().to_degrees()
}

fn uv_angle(a: DVec2, b: DVec2, c: DVec2) -> Option<f64> {
    let (u, v) = (a - b, c - b);
    if u.length_squared() < 1e-30 || v.length_squared() < 1e-30 {
        return None;
    }
    Some((u.normalize().dot(v.normalize())).clamp(-1.0, 1.0).acos().to_degrees())
}

/// Area and angle distortion per seam-delimited island over all faces.
pub fn stretch_report(mesh: &EditMesh) -> StretchReport {
    let adj = Adjacency::build(mesh);
    let all: BTreeSet<u32> = (0..mesh.faces.len() as u32).collect();
    let mut rows: Vec<(f64, IslandStretch)> = Vec::new();
    for island in islands(mesh, &adj, &all) {
        let mut world = 0.0;
        let mut uv_area = 0.0;
        let mut worst_angle: f64 = 0.0;
        for f in &island {
            for [x, y, z] in face_tris(mesh, *f as usize) {
                let face = &mesh.faces[*f as usize];
                let corner = |o: usize| &mesh.corners[face.first as usize + o];
                let (ca, cb, cc) = (corner(x), corner(y), corner(z));
                let (pa, pb, pc) = (
                    mesh.positions[ca.vertex as usize],
                    mesh.positions[cb.vertex as usize],
                    mesh.positions[cc.vertex as usize],
                );
                let (ua, ub, uc) = (
                    DVec2::new(ca.uv[0] as f64, ca.uv[1] as f64),
                    DVec2::new(cb.uv[0] as f64, cb.uv[1] as f64),
                    DVec2::new(cc.uv[0] as f64, cc.uv[1] as f64),
                );
                world += ((pb - pa).cross(pc - pa)).length() * 0.5;
                uv_area += ((ub - ua).perp_dot(uc - ua) * 0.5).abs();
                for (p, q, r, u, v, w) in [(pa, pb, pc, ua, ub, uc), (pb, pc, pa, ub, uc, ua), (pc, pa, pb, uc, ua, ub)] {
                    let a3 = corner_angle(p, q, r);
                    match uv_angle(u, v, w) {
                        Some(a2) => worst_angle = worst_angle.max((a3 - a2).abs()),
                        None => worst_angle = worst_angle.max(180.0),
                    }
                }
            }
        }
        let ratio = if world > 1e-24 { uv_area / world } else { 0.0 };
        let score = worst_angle + 20.0 * sanitize_report(ratio).max(1e-300).ln().abs();
        rows.push((
            if score.is_finite() { score } else { 0.0 },
            IslandStretch {
                faces: island.len() as u32,
                area_ratio: sanitize_report(ratio),
                angle_deg: sanitize_report(worst_angle),
            },
        ));
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    StretchReport { islands: rows.len() as u32, worst: rows.into_iter().take(8).map(|(_, r)| r).collect() }
}

pub fn run(op: &str, mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    if mesh.corners.len() > MAX_CORNERS {
        return Err(bad("the mesh is larger than the mesh budget"));
    }
    let adj = Adjacency::build(mesh);
    let faces = target_faces(mesh, &adj, sel);
    if faces.is_empty() {
        return Err(bad("the mesh has no faces"));
    }
    match op {
        "uv_box" => run_box(mesh, &adj, &faces, params),
        "uv_cylinder" => run_cylinder(mesh, &adj, &faces, params),
        "uv_planar" => run_planar(mesh, &faces, params),
        "uv_unwrap" => run_unwrap(mesh, &adj, &faces, params),
        "uv_pack" => run_pack(mesh, &adj, &faces, params),
        "uv_reset" => run_reset(mesh, &faces, params),
        _ => Err(bad(format!("unknown mesh op {op}"))),
    }
}
