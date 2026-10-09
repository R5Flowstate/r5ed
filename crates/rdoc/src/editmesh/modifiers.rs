//! The modifier stack: non-destructive Array, Curve, Displace, Subdivision and
//! Mirror evaluated top to bottom inside `evaluate`. (Boolean joins the same
//! stack in M10.) Every modifier checks its output size against the mesh
//! budget before allocating and never panics on bad input.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use glam::{DMat4, DVec3};
use serde::{Deserialize, Serialize};

use super::ops::{compact, dedupe_faces, drop_faces, weld};
use super::{bad, Adjacency, EditError, EditMesh, Face};
use crate::command::Command;
use crate::doc::Document;
use crate::model::{Id, ObjectKind};
use crate::noise::{self, Noise};

fn yes() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

fn default_count() -> u32 {
    2
}

fn default_strength() -> f64 {
    1.0
}

fn default_mid() -> f64 {
    0.5
}

fn default_scale() -> f64 {
    1.0
}

fn default_octaves() -> u32 {
    4
}

fn default_levels() -> u32 {
    1
}

fn default_axes() -> [bool; 3] {
    [true, false, false]
}

/// One entry of `Mesh.modifiers`: its switches plus its per-kind params.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Modifier {
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub show_in_viewport: bool,
    #[serde(flatten)]
    pub kind: ModifierKind,
}

impl Modifier {
    pub fn label(&self) -> &'static str {
        match &self.kind {
            ModifierKind::Array(_) => "array",
            ModifierKind::Curve(_) => "curve",
            ModifierKind::Displace(_) => "displace",
            ModifierKind::Subdivision(_) => "subdivision",
            ModifierKind::Mirror(_) => "mirror",
            ModifierKind::Boolean(_) => "boolean",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModifierKind {
    Array(ArrayParams),
    Curve(CurveParams),
    Displace(DisplaceParams),
    Subdivision(SubdivisionParams),
    Mirror(MirrorParams),
    Boolean(BooleanParams),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrayParams {
    #[serde(default = "default_count")]
    pub count: u32,
    #[serde(default)]
    pub fit_length: Option<f64>,
    #[serde(default)]
    pub relative_offset: Option<[f64; 3]>,
    #[serde(default)]
    pub constant_offset: Option<[f64; 3]>,
    #[serde(default)]
    pub object_offset: Option<Id>,
    #[serde(default)]
    pub merge: Option<f64>,
    #[serde(default)]
    pub merge_first_last: bool,
    #[serde(default)]
    pub uv_offset: [f64; 2],
}

impl Default for ArrayParams {
    fn default() -> Self {
        Self {
            count: default_count(),
            fit_length: None,
            relative_offset: None,
            constant_offset: None,
            object_offset: None,
            merge: None,
            merge_first_last: false,
            uv_offset: [0.0, 0.0],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CurveAxis {
    #[serde(rename = "x")]
    #[default]
    X,
    #[serde(rename = "y")]
    Y,
    #[serde(rename = "z")]
    Z,
    #[serde(rename = "-x")]
    NegX,
    #[serde(rename = "-y")]
    NegY,
    #[serde(rename = "-z")]
    NegZ,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurveParams {
    pub path: Id,
    #[serde(default)]
    pub axis: CurveAxis,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplaceDirection {
    #[default]
    Normal,
    X,
    Y,
    Z,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplaceSpace {
    #[default]
    Local,
    World,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoiseTexture {
    #[serde(default)]
    pub kind: noise::NoiseKind,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default = "default_octaves")]
    pub octaves: u32,
    #[serde(default)]
    pub seed: u64,
}

impl Default for NoiseTexture {
    fn default() -> Self {
        Self { kind: noise::NoiseKind::default(), scale: default_scale(), octaves: default_octaves(), seed: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplaceParams {
    #[serde(default = "default_strength")]
    pub strength: f64,
    #[serde(default = "default_mid")]
    pub mid_level: f64,
    #[serde(default)]
    pub direction: DisplaceDirection,
    #[serde(default)]
    pub space: DisplaceSpace,
    #[serde(default)]
    pub texture: NoiseTexture,
}

impl Default for DisplaceParams {
    fn default() -> Self {
        Self {
            strength: default_strength(),
            mid_level: default_mid(),
            direction: DisplaceDirection::default(),
            space: DisplaceSpace::default(),
            texture: NoiseTexture::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubdivKind {
    #[default]
    CatmullClark,
    Simple,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubdivisionParams {
    #[serde(default = "default_levels")]
    pub levels: u32,
    #[serde(default)]
    pub kind: SubdivKind,
    #[serde(default = "yes")]
    pub keep_sharp_edges: bool,
}

impl Default for SubdivisionParams {
    fn default() -> Self {
        Self { levels: default_levels(), kind: SubdivKind::default(), keep_sharp_edges: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorParams {
    #[serde(default = "default_axes")]
    pub axes: [bool; 3],
    #[serde(default)]
    pub merge: Option<f64>,
    #[serde(default = "yes")]
    pub clip: bool,
    #[serde(default)]
    pub mirror_object: Option<Id>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BooleanParams {
    pub object: Id,
    #[serde(default)]
    pub operation: super::BoolOp,
    #[serde(default)]
    pub keep_material: bool,
}

impl Default for MirrorParams {
    fn default() -> Self {
        Self { axes: default_axes(), merge: None, clip: true, mirror_object: None }
    }
}

/// What a modifier reads beyond the base mesh: world matrices, world-space
/// path points, and boolean operand meshes in their own object space. The
/// mesh itself is evaluated in its own object space; callers convert with
/// the mesh world matrix.
#[derive(Clone, Debug, Default)]
pub struct EvalContext {
    pub self_id: Option<Id>,
    pub worlds: BTreeMap<Id, DMat4>,
    pub paths: BTreeMap<Id, Vec<DVec3>>,
    pub booleans: BTreeMap<Id, EditMesh>,
}

impl EvalContext {
    pub fn from_doc(doc: &Document, self_id: Id) -> Self {
        let mut worlds = BTreeMap::new();
        for id in doc.object_ids() {
            if let Some(m) = doc.world_matrix(id) {
                worlds.insert(id, m);
            }
        }
        let mut paths = BTreeMap::new();
        for o in &doc.doc.objects {
            if let ObjectKind::Path(p) = &o.kind {
                if let Some(w) = worlds.get(&o.id) {
                    paths.insert(o.id, p.points.iter().map(|pt| w.transform_point3(*pt)).collect());
                }
            }
        }
        let mut booleans = BTreeMap::new();
        if let Some(o) = doc.object(self_id) {
            if let ObjectKind::Mesh(m) = &o.kind {
                for operand in m.modifiers.iter().filter_map(|m| match &m.kind {
                    ModifierKind::Boolean(p) => Some(p.object),
                    _ => None,
                }) {
                    if let Ok(mesh) = super::ops::boolean::load_operand(doc, operand) {
                        booleans.insert(operand, mesh);
                    }
                }
            }
        }
        Self { self_id: Some(self_id), worlds, paths, booleans }
    }

    fn mesh_world(&self) -> DMat4 {
        self.self_id.and_then(|id| self.worlds.get(&id)).copied().unwrap_or(DMat4::IDENTITY)
    }
}

fn finite3(v: [f64; 3], name: &str) -> Result<DVec3, EditError> {
    if !v.iter().all(|x| x.is_finite()) {
        return Err(bad(format!("param {name} is not finite")));
    }
    Ok(DVec3::from_array(v))
}

fn clamp_coord(v: f64) -> f64 {
    v.clamp(-super::MAX_COORD, super::MAX_COORD)
}

fn clamp_point(p: DVec3) -> DVec3 {
    DVec3::new(clamp_coord(p.x), clamp_coord(p.y), clamp_coord(p.z))
}

/// Applies the enabled modifiers top to bottom. With `viewport_only`, ones
/// hidden in the viewport are skipped, as the viewport draws.
pub fn evaluate_stack(
    base: &EditMesh,
    mods: &[Modifier],
    ctx: &EvalContext,
    viewport_only: bool,
) -> Result<EditMesh, EditError> {
    let mut mesh = base.clone();
    for m in mods {
        if !m.enabled || (viewport_only && !m.show_in_viewport) {
            continue;
        }
        mesh = apply_modifier(&mesh, &m.kind, ctx)?;
    }
    Ok(mesh)
}

fn apply_modifier(mesh: &EditMesh, kind: &ModifierKind, ctx: &EvalContext) -> Result<EditMesh, EditError> {
    match kind {
        ModifierKind::Array(p) => apply_array(mesh, p, ctx),
        ModifierKind::Curve(p) => apply_curve(mesh, p, ctx),
        ModifierKind::Displace(p) => apply_displace(mesh, p, ctx),
        ModifierKind::Subdivision(p) => apply_subdivision(mesh, p),
        ModifierKind::Mirror(p) => apply_mirror(mesh, p, ctx),
        ModifierKind::Boolean(p) => apply_boolean(mesh, p, ctx),
    }
}

fn check_output(verts: usize, faces: usize, corners: usize) -> Result<(), EditError> {
    if verts > super::MAX_VERTICES {
        return Err(bad("the modifier output passes the vertex budget"));
    }
    if faces > super::MAX_FACES || corners > super::MAX_CORNERS {
        return Err(bad("the modifier output passes the mesh budget"));
    }
    Ok(())
}

fn apply_array(mesh: &EditMesh, p: &ArrayParams, ctx: &EvalContext) -> Result<EditMesh, EditError> {
    let rel = match p.relative_offset {
        Some(v) => finite3(v, "relative_offset")?,
        None => DVec3::new(1.0, 0.0, 0.0),
    };
    let con = match p.constant_offset {
        Some(v) => {
            let c = finite3(v, "constant_offset")?;
            DVec3::new(clamp_coord(c.x), clamp_coord(c.y), clamp_coord(c.z))
        }
        None => DVec3::ZERO,
    };
    let size = match mesh.bounds() {
        Some((lo, hi)) => hi - lo,
        None => DVec3::ZERO,
    };
    let lin = DVec3::new(rel.x * size.x + con.x, rel.y * size.y + con.y, rel.z * size.z + con.z);
    let delta = match p.object_offset {
        Some(id) => {
            let other = ctx.worlds.get(&id).ok_or_else(|| bad(format!("array object {id} is not in the project")))?;
            let inv = ctx.mesh_world().inverse();
            if !inv.is_finite() {
                return Err(bad("the mesh transform does not invert"));
            }
            *other * inv
        }
        None => DMat4::IDENTITY,
    };
    let mut count = p.count.clamp(1, 1024);
    if let Some(l) = p.fit_length {
        if !l.is_finite() {
            return Err(bad("param fit_length is not finite"));
        }
        let adv = lin.length();
        if adv > 1e-9 {
            count = (l.max(0.0) / adv).round().clamp(1.0, 1024.0) as u32;
        }
    }
    if mesh.faces.is_empty() {
        return Ok(mesh.clone());
    }
    let n = mesh.positions.len();
    check_output(n.checked_mul(count as usize).ok_or_else(|| bad("the array passes the vertex budget"))?, mesh.faces.len() * count as usize, mesh.corners.len() * count as usize)?;
    let uv_off = if p.uv_offset.iter().all(|x| x.is_finite()) {
        p.uv_offset
    } else {
        return Err(bad("param uv_offset is not finite"));
    };
    let step = DMat4::from_translation(lin) * delta;
    let mut out = EditMesh {
        positions: Vec::with_capacity(n * count as usize),
        faces: Vec::with_capacity(mesh.faces.len() * count as usize),
        corners: Vec::with_capacity(mesh.corners.len() * count as usize),
        edge_flags: Vec::new(),
        materials: mesh.materials.clone(),
        smooth_angle: mesh.smooth_angle,
    };
    let mut xf = DMat4::IDENTITY;
    for copy in 0..count {
        let base_vert = out.positions.len() as u32;
        for v in &mesh.positions {
            out.positions.push(clamp_point(xf.transform_point3(*v)));
        }
        let base_corner = out.corners.len() as u32;
        for c in &mesh.corners {
            let mut corner = *c;
            corner.vertex += base_vert;
            corner.uv[0] += uv_off[0] as f32 * copy as f32;
            corner.uv[1] += uv_off[1] as f32 * copy as f32;
            if !corner.uv.iter().all(|x| x.is_finite()) {
                return Err(bad("the array uv offset overflows"));
            }
            out.corners.push(corner);
        }
        for f in &mesh.faces {
            out.faces.push(Face { first: base_corner + f.first, len: f.len, material: f.material, flags: f.flags });
        }
        for (a, b, f) in &mesh.edge_flags {
            out.edge_flags.push((a + base_vert, b + base_vert, *f));
        }
        xf = step * xf;
    }
    match p.merge {
        Some(d) => {
            if !d.is_finite() {
                return Err(bad("param merge is not finite"));
            }
            weld(&mut out, d.clamp(0.0, super::MAX_COORD).max(1e-12));
        }
        None if p.merge_first_last && count > 1 => weld_seam(&mut out, n, super::WELD_EPS),
        _ => {}
    }
    Ok(out)
}

/// Welds only the seam between the first and the last array copy, so a
/// rotating array closes its ring without merging its middle.
fn weld_seam(mesh: &mut EditMesh, first_count: usize, eps: f64) {
    let total = mesh.positions.len();
    if first_count == 0 || total <= first_count {
        return;
    }
    let last_start = total - first_count.min(total - total % first_count.max(1));
    let first_end = first_count.min(total);
    let mut rep: Vec<u32> = (0..total as u32).collect();
    for (v, slot) in rep.iter_mut().enumerate().take(total).skip(last_start) {
        let p = mesh.positions[v];
        for u in 0..first_end {
            if (mesh.positions[u] - p).length() <= eps {
                *slot = u as u32;
                break;
            }
        }
    }
    if rep.iter().enumerate().all(|(i, r)| *r as usize == i) {
        return;
    }
    for c in &mut mesh.corners {
        c.vertex = rep[c.vertex as usize];
    }
    let mut dead = BTreeSet::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let mut distinct: Vec<u32> =
            mesh.corners[f.first as usize..f.first as usize + f.len as usize].iter().map(|c| c.vertex).collect();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() < 3 {
            dead.insert(fi as u32);
        }
    }
    drop_faces(mesh, &dead);
    dedupe_faces(mesh);
    compact(mesh);
}

struct Frame {
    point: DVec3,
    normal: DVec3,
    binormal: DVec3,
    length: f64,
}

fn apply_curve(mesh: &EditMesh, p: &CurveParams, ctx: &EvalContext) -> Result<EditMesh, EditError> {
    let world_pts = ctx.paths.get(&p.path).ok_or_else(|| bad(format!("curve path {} is not in the project", p.path)))?;
    let inv = ctx.mesh_world().inverse();
    if !inv.is_finite() {
        return Err(bad("the mesh transform does not invert"));
    }
    let pts: Vec<DVec3> = world_pts.iter().map(|q| inv.transform_point3(*q)).collect();
    let mut chain: Vec<DVec3> = Vec::with_capacity(pts.len());
    for q in pts {
        if chain.last().is_none_or(|l| (*l - q).length() > 1e-9) {
            chain.push(q);
        }
    }
    if chain.len() < 2 {
        return Err(bad("the curve path has fewer than two points"));
    }
    let mut frames: Vec<Frame> = Vec::with_capacity(chain.len());
    let mut seed = (chain[1] - chain[0]).normalize_or_zero();
    if seed == DVec3::ZERO {
        return Err(bad("the curve path has no length"));
    }
    let mut n = if seed.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    n = (n - seed * n.dot(seed)).normalize_or(DVec3::X);
    let mut length = 0.0;
    for k in 0..chain.len() {
        let t = if k + 1 < chain.len() {
            (chain[k + 1] - chain[k]).normalize_or(seed)
        } else {
            (chain[k] - chain[k - 1]).normalize_or(seed)
        };
        n = (n - t * n.dot(t)).normalize_or(n);
        let b = t.cross(n).normalize_or(DVec3::Y);
        if k > 0 {
            length += (chain[k] - chain[k - 1]).length();
        }
        frames.push(Frame { point: chain[k], normal: n, binormal: b, length });
        seed = t;
    }
    let total = length.max(1e-9);
    let (axis, sign) = match p.axis {
        CurveAxis::X => (0, 1.0),
        CurveAxis::Y => (1, 1.0),
        CurveAxis::Z => (2, 1.0),
        CurveAxis::NegX => (0, -1.0),
        CurveAxis::NegY => (1, -1.0),
        CurveAxis::NegZ => (2, -1.0),
    };
    let (l1, l2) = match axis {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    };
    let mut out = mesh.clone();
    for v in &mut out.positions {
        let arr = v.to_array();
        let s = (arr[axis] * sign).clamp(0.0, total);
        let mut k = 0;
        while k + 1 < frames.len() && frames[k + 1].length < s {
            k += 1;
        }
        let (a, b) = (&frames[k], &frames[k + 1.min(frames.len() - 1)]);
        let span = (b.length - a.length).max(1e-12);
        let t = ((s - a.length) / span).clamp(0.0, 1.0);
        let center = a.point.lerp(b.point, t);
        let normal = (a.normal.lerp(b.normal, t)).normalize_or(a.normal);
        let binormal = (a.binormal.lerp(b.binormal, t)).normalize_or(a.binormal);
        *v = clamp_point(center + normal * arr[l1] + binormal * arr[l2]);
    }
    out.clear_custom_normals(&(0..out.faces.len() as u32).collect::<Vec<_>>());
    Ok(out)
}

fn fbm(kind: noise::NoiseKind, n: &Noise, octaves: u32, scale: f64, x: f64, y: f64, z: f64) -> f64 {
    let octaves = octaves.clamp(1, 24);
    let mut freq = scale;
    let mut amp = 1.0;
    let mut sum = 0.0;
    let mut norm = 0.0;
    for _ in 0..octaves {
        sum += kind.sample3(n, x * freq, y * freq, z * freq) * amp;
        norm += amp;
        freq *= 2.0;
        amp *= 0.5;
    }
    sum / norm
}

fn apply_displace(mesh: &EditMesh, p: &DisplaceParams, ctx: &EvalContext) -> Result<EditMesh, EditError> {
    if !p.strength.is_finite() {
        return Err(bad("param strength is not finite"));
    }
    if !p.mid_level.is_finite() {
        return Err(bad("param mid_level is not finite"));
    }
    if !p.texture.scale.is_finite() {
        return Err(bad("param scale is not finite"));
    }
    let strength = p.strength.clamp(-super::MAX_COORD, super::MAX_COORD);
    let mid = p.mid_level.clamp(0.0, 1.0);
    let scale = p.texture.scale.abs().clamp(1e-6, 1e4);
    let octaves = p.texture.octaves.clamp(1, 24);
    let noise = Noise::new(p.texture.seed);
    let world = ctx.mesh_world();
    let to_world = matches!(p.space, DisplaceSpace::World);
    let axis_of = |d: DisplaceDirection| match d {
        DisplaceDirection::X => DVec3::X,
        DisplaceDirection::Y => DVec3::Y,
        DisplaceDirection::Z => DVec3::Z,
        DisplaceDirection::Normal => DVec3::ZERO,
    };
    let world_axis = match p.direction {
        DisplaceDirection::Normal => DVec3::ZERO,
        d => world.transform_vector3(axis_of(d)).normalize_or(axis_of(d)),
    };
    let normals = vertex_normals(mesh);
    let mut out = mesh.clone();
    for (vi, v) in mesh.positions.iter().enumerate() {
        let sample_at = if to_world { world.transform_point3(*v) } else { *v };
        if !sample_at.is_finite() {
            return Err(bad("the mesh transform overflows"));
        }
        let raw = fbm(p.texture.kind, &noise, octaves, scale, sample_at.x, sample_at.y, sample_at.z);
        let t = match p.texture.kind {
            noise::NoiseKind::Perlin | noise::NoiseKind::Simplex | noise::NoiseKind::Value => (raw * 0.5 + 0.5).clamp(0.0, 1.0),
            _ => raw.clamp(0.0, 1.0),
        };
        let offset = strength * (t - mid);
        let next = match p.direction {
            DisplaceDirection::Normal => *v + normals[vi] * offset,
            _ if to_world => {
                let inv = world.inverse();
                if !inv.is_finite() {
                    return Err(bad("the mesh transform does not invert"));
                }
                inv.transform_point3(world.transform_point3(*v) + world_axis * offset)
            }
            d => *v + axis_of(d) * offset,
        };
        if !next.is_finite() {
            return Err(bad("the displacement overflows"));
        }
        out.positions[vi] = clamp_point(next);
    }
    out.clear_custom_normals(&(0..out.faces.len() as u32).collect::<Vec<_>>());
    Ok(out)
}

fn vertex_normals(mesh: &EditMesh) -> Vec<DVec3> {
    let mut acc = vec![DVec3::ZERO; mesh.positions.len()];
    for fi in 0..mesh.faces.len() {
        let n = mesh.face_newell(fi);
        for c in mesh.face_corners(fi) {
            acc[c.vertex as usize] += n;
        }
    }
    acc.into_iter().map(|n| n.normalize_or_zero()).collect()
}

fn apply_subdivision(mesh: &EditMesh, p: &SubdivisionParams) -> Result<EditMesh, EditError> {
    let mut levels = p.levels.clamp(0, 4);
    let mut out = mesh.clone();
    while levels > 0 {
        levels -= 1;
        out = subdivide_once(&out, p.kind, p.keep_sharp_edges)?;
    }
    Ok(out)
}

fn subdivide_once(mesh: &EditMesh, kind: SubdivKind, keep_sharp: bool) -> Result<EditMesh, EditError> {
    if mesh.faces.is_empty() {
        return Ok(mesh.clone());
    }
    let adj = Adjacency::build(mesh);
    let sharp_of = |a: u32, b: u32| keep_sharp && mesh.edge_flag(a, b).contains(super::EdgeFlags::SHARP);
    let mut face_point: Vec<DVec3> = Vec::with_capacity(mesh.faces.len());
    for fi in 0..mesh.faces.len() {
        let pts = mesh.face_points(fi);
        face_point.push(pts.iter().sum::<DVec3>() / pts.len().max(1) as f64);
    }
    let mut edge_id: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    let mut edge_point: Vec<DVec3> = Vec::new();
    let mut edge_sharp: Vec<bool> = Vec::new();
    for e in &adj.edges {
        let (a, b) = (e.a, e.b);
        let pa = mesh.positions[a as usize];
        let pb = mesh.positions[b as usize];
        let smooth = e.faces.len() == 2 && !sharp_of(a, b);
        let point = if smooth {
            let f1 = face_point[e.faces[0] as usize];
            let f2 = face_point[e.faces[1] as usize];
            (pa + pb + f1 + f2) * 0.25
        } else {
            (pa + pb) * 0.5
        };
        edge_id.insert((a, b), edge_point.len() as u32);
        edge_point.push(point);
        edge_sharp.push(e.faces.len() != 2 || sharp_of(a, b));
    }
    let mut vert_point: Vec<DVec3> = Vec::with_capacity(mesh.positions.len());
    for vi in 0..mesh.positions.len() {
        let p = mesh.positions[vi];
        let faces = adj.faces_of(vi as u32);
        let mut crease: Vec<DVec3> = Vec::new();
        let mut bounded = false;
        for e in &adj.edges {
            if e.a != vi as u32 && e.b != vi as u32 {
                continue;
            }
            let other = if e.a == vi as u32 { e.b } else { e.a };
            if e.faces.len() != 2 || sharp_of(e.a, e.b) {
                bounded = true;
                if e.faces.len() <= 2 {
                    crease.push(mesh.positions[other as usize]);
                }
            }
        }
        let next = if kind == SubdivKind::Simple || faces.is_empty() {
            p
        } else if !bounded {
            let n = faces.len() as f64;
            let f = faces.iter().map(|f| face_point[*f as usize]).sum::<DVec3>() / n;
            let mut r = DVec3::ZERO;
            let mut count = 0u32;
            for e in &adj.edges {
                if e.a == vi as u32 || e.b == vi as u32 {
                    r += (mesh.positions[e.a as usize] + mesh.positions[e.b as usize]) * 0.5;
                    count += 1;
                }
            }
            if count == 0 {
                p
            } else {
                r /= count as f64;
                (f + r * 2.0 + p * (n - 3.0)) / n
            }
        } else if crease.len() == 2 {
            p * 0.75 + (crease[0] + crease[1]) * 0.125
        } else {
            p
        };
        vert_point.push(next);
    }
    let v_count = mesh.positions.len();
    let new_face_total: usize = mesh.faces.iter().map(|f| f.len as usize).sum();
    let new_corner_total = new_face_total * 4;
    check_output(v_count + edge_point.len() + face_point.len(), new_face_total, new_corner_total)?;
    let mut out = EditMesh {
        positions: Vec::with_capacity(v_count + edge_point.len() + face_point.len()),
        faces: Vec::with_capacity(new_face_total),
        corners: Vec::with_capacity(new_corner_total),
        edge_flags: Vec::new(),
        materials: mesh.materials.clone(),
        smooth_angle: mesh.smooth_angle,
    };
    out.positions.extend(vert_point);
    let edge_base = out.positions.len() as u32;
    out.positions.extend(edge_point);
    let face_base = out.positions.len() as u32;
    out.positions.extend(face_point);
    for (fi, f) in mesh.faces.iter().enumerate() {
        let corners = &mesh.corners[f.first as usize..f.first as usize + f.len as usize];
        let n = corners.len();
        let uvs: Vec<[f32; 2]> = corners.iter().map(|c| c.uv).collect();
        let avg_uv = {
            let (mut a, mut b) = (0.0f64, 0.0f64);
            for u in &uvs {
                a += u[0] as f64;
                b += u[1] as f64;
            }
            [(a / n as f64) as f32, (b / n as f64) as f32]
        };
        for k in 0..n {
            let vk = corners[k].vertex;
            let vnext = corners[(k + 1) % n].vertex;
            let key = (vk.min(vnext), vk.max(vnext));
            let ek = edge_base + edge_id.get(&key).copied().ok_or_else(|| bad("the mesh has an edge the table missed"))?;
            let sharp_k = edge_sharp[adj.index_of(key.0, key.1) as usize];
            let vprev = corners[(k + n - 1) % n].vertex;
            let eprev = {
                let key = (vprev.min(vk), vprev.max(vk));
                edge_base + edge_id[&key]
            };
            let fp = face_base + fi as u32;
            let mid_k = [(uvs[k][0] + uvs[(k + 1) % n][0]) * 0.5, (uvs[k][1] + uvs[(k + 1) % n][1]) * 0.5];
            let mid_prev = [(uvs[(k + n - 1) % n][0] + uvs[k][0]) * 0.5, (uvs[(k + n - 1) % n][1]) * 0.5];
            let quad = [(vk, uvs[k]), (ek, mid_k), (fp, avg_uv), (eprev, mid_prev)];
            let first = out.corners.len() as u32;
            for (v, uv) in quad {
                out.corners.push(super::Corner { vertex: v, uv, normal: None });
            }
            out.faces.push(Face { first, len: 4, material: f.material, flags: f.flags });
            if sharp_k {
                out.set_edge_flag(vk, ek, super::EdgeFlags::SHARP, true);
                out.set_edge_flag(eprev, vk, super::EdgeFlags::SHARP, true);
            }
        }
    }
    super::ops::drop_degenerate(&mut out);
    Ok(out)
}

/// The Boolean modifier arranges the mesh against another Mesh or Brush
/// object, read in the mesh's own object space like every other modifier.
fn apply_boolean(mesh: &EditMesh, p: &BooleanParams, ctx: &EvalContext) -> Result<EditMesh, EditError> {
    let operand = ctx
        .booleans
        .get(&p.object)
        .ok_or_else(|| bad(format!("boolean operand {} is not a usable mesh or brush", p.object)))?;
    let inv = ctx.mesh_world().inverse();
    if !inv.is_finite() {
        return Err(bad("the mesh transform does not invert"));
    }
    let rel = inv * ctx.worlds.get(&p.object).copied().unwrap_or(DMat4::IDENTITY);
    if !rel.is_finite() {
        return Err(bad("the boolean operand transform is not finite"));
    }
    let mut moved = EditMesh {
        positions: Vec::new(),
        faces: Vec::new(),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: mesh.materials.clone(),
        smooth_angle: mesh.smooth_angle,
    };
    super::ops::append_transformed(&mut moved, operand, rel)?;
    super::boolean::solve(mesh, &moved, p.operation, p.keep_material)
}

fn apply_mirror(mesh: &EditMesh, p: &MirrorParams, ctx: &EvalContext) -> Result<EditMesh, EditError> {
    if !p.axes.iter().any(|a| *a) {
        return Ok(mesh.clone());
    }
    if let Some(d) = p.merge {
        if !d.is_finite() {
            return Err(bad("param merge is not finite"));
        }
    }
    let mut out = mesh.clone();
    for axis in 0..3 {
        if !p.axes[axis] {
            continue;
        }
        out = mirror_once(&out, axis, p, ctx)?;
    }
    Ok(out)
}

fn mirror_once(mesh: &EditMesh, axis: usize, p: &MirrorParams, ctx: &EvalContext) -> Result<EditMesh, EditError> {
    let mut unit = [0.0; 3];
    unit[axis] = 1.0;
    let plane_n = DVec3::from_array(unit);
    let (n, through) = match p.mirror_object {
        Some(id) => {
            let mw = ctx.worlds.get(&id).ok_or_else(|| bad(format!("mirror object {id} is not in the project")))?;
            let self_inv = ctx.mesh_world().inverse();
            if !self_inv.is_finite() {
                return Err(bad("the mesh transform does not invert"));
            }
            let n = (self_inv * *mw).transform_vector3(plane_n).normalize_or(plane_n);
            let q0 = self_inv.transform_point3(mw.w_axis.truncate());
            (n, q0)
        }
        None => (plane_n, DVec3::ZERO),
    };
    if !n.is_finite() || !through.is_finite() {
        return Err(bad("the mirror plane does not invert"));
    }
    check_output(mesh.positions.len() * 2, mesh.faces.len() * 2, mesh.corners.len() * 2)?;
    let eps = p.merge.unwrap_or(super::WELD_EPS).clamp(0.0, super::MAX_COORD).max(1e-12);
    let mut out = mesh.clone();
    if p.clip {
        for v in &mut out.positions {
            let d = (*v - through).dot(n);
            if d.abs() <= eps {
                *v -= n * d;
            }
        }
    }
    let base = out.positions.len() as u32;
    for v in mesh.positions.iter() {
        let mut v = *v;
        if p.clip {
            let d = (v - through).dot(n);
            if d.abs() <= eps {
                v -= n * d;
            }
        }
        out.positions.push(clamp_point(v - n * (2.0 * (v - through).dot(n))));
    }
    let base_corner = out.corners.len() as u32;
    for c in mesh.corners.iter() {
        let mut corner = *c;
        corner.vertex += base;
        if let Some(nm) = corner.normal {
            let m = DVec3::new(nm[0] as f64, nm[1] as f64, nm[2] as f64);
            let mirrored = m - n * (2.0 * m.dot(n));
            corner.normal = Some([mirrored.x as f32, mirrored.y as f32, mirrored.z as f32]);
        }
        out.corners.push(corner);
    }
    let mut faces: Vec<Face> = Vec::with_capacity(mesh.faces.len());
    let mut k = base_corner as usize;
    for f in mesh.faces.iter() {
        let first = k as u32;
        let mut mirrored: Vec<super::Corner> = out.corners[k..k + f.len as usize].to_vec();
        mirrored.reverse();
        for (i, c) in mirrored.iter().enumerate() {
            out.corners[k + i] = *c;
        }
        faces.push(Face { first, len: f.len, material: f.material, flags: f.flags });
        k += f.len as usize;
    }
    out.faces.extend(faces);
    let mut flags = mesh.edge_flags.clone();
    for (a, b, f) in &mesh.edge_flags {
        flags.push((a + base, b + base, *f));
    }
    flags.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    out.edge_flags = flags;
    if p.merge.is_some() {
        weld(&mut out, eps);
    }
    Ok(out)
}

/// Triangle-soup loose parts joined by bitwise-equal positions, ordered by
/// their smallest triangle. The evaluated soup carries one vertex per corner,
/// so index sharing alone never joins anything; coincident positions do.
pub fn soup_parts_by_position(positions: &[[f32; 3]], indices: &[u32]) -> Vec<Vec<u32>> {
    let mut canon: BTreeMap<[u32; 3], u32> = BTreeMap::new();
    let mut remap: Vec<u32> = Vec::with_capacity(positions.len());
    for p in positions {
        let n = canon.len() as u32;
        remap.push(*canon.entry([p[0].to_bits(), p[1].to_bits(), p[2].to_bits()]).or_insert(n));
    }
    let mapped: Vec<u32> = indices.iter().map(|i| remap[*i as usize]).collect();
    soup_parts(canon.len(), &mapped)
}

/// Triangle-soup loose parts: groups of triangle indices joined by shared
/// vertices, ordered by their smallest triangle. The compiler hulls each part.
pub fn soup_parts(vert_count: usize, indices: &[u32]) -> Vec<Vec<u32>> {
    let tris = indices.len() / 3;
    let mut by_vert: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for t in 0..tris {
        for k in 0..3 {
            let v = indices[t * 3 + k];
            if (v as usize) < vert_count {
                by_vert.entry(v).or_default().push(t as u32);
            }
        }
    }
    let mut todo: BTreeSet<u32> = (0..tris as u32).collect();
    let mut parts = Vec::new();
    while let Some(seed) = todo.iter().next().copied() {
        let mut comp = vec![seed];
        let mut queue: VecDeque<u32> = VecDeque::from([seed]);
        todo.remove(&seed);
        while let Some(t) = queue.pop_front() {
            for k in 0..3 {
                let v = indices[t as usize * 3 + k];
                if let Some(users) = by_vert.get(&v) {
                    for u in users {
                        if todo.remove(u) {
                            queue.push_back(*u);
                            comp.push(*u);
                        }
                    }
                }
            }
        }
        comp.sort_unstable();
        parts.push(comp);
    }
    parts.sort_by_key(|p| p[0]);
    parts
}

fn mesh_hash(mesh: &EditMesh) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: &[u8]| {
        for x in b {
            h ^= *x as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for p in &mesh.positions {
        mix(&p.x.to_bits().to_le_bytes());
        mix(&p.y.to_bits().to_le_bytes());
        mix(&p.z.to_bits().to_le_bytes());
    }
    for f in &mesh.faces {
        mix(&f.first.to_le_bytes());
        mix(&f.len.to_le_bytes());
        mix(&f.material.to_le_bytes());
        mix(&[f.flags.0]);
    }
    for c in &mesh.corners {
        mix(&c.vertex.to_le_bytes());
        mix(&c.uv[0].to_bits().to_le_bytes());
        mix(&c.uv[1].to_bits().to_le_bytes());
        match c.normal {
            Some(n) => {
                mix(&[1]);
                for v in n {
                    mix(&v.to_bits().to_le_bytes());
                }
            }
            None => mix(&[0]),
        }
    }
    for (a, b, f) in &mesh.edge_flags {
        mix(&a.to_le_bytes());
        mix(&b.to_le_bytes());
        mix(&[f.0]);
    }
    for m in &mesh.materials {
        mix(m.as_bytes());
        mix(&[0xff]);
    }
    if let Some(a) = mesh.smooth_angle {
        mix(&a.to_bits().to_le_bytes());
    }
    h
}

fn stack_hash(mods: &[Modifier]) -> u64 {
    let bytes = serde_json::to_vec(mods).unwrap_or_default();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn referenced_ids(mods: &[Modifier]) -> BTreeSet<Id> {
    let mut out = BTreeSet::new();
    for m in mods {
        match &m.kind {
            ModifierKind::Array(p) => {
                if let Some(id) = p.object_offset {
                    out.insert(id);
                }
            }
            ModifierKind::Curve(p) => {
                out.insert(p.path);
            }
            ModifierKind::Mirror(p) => {
                if let Some(id) = p.mirror_object {
                    out.insert(id);
                }
            }
            ModifierKind::Boolean(p) => {
                out.insert(p.object);
            }
            _ => {}
        }
    }
    out
}

/// The evaluated stack, cached by file hash, stack hash and the referenced
/// object transforms. Used by the wasm core, which evaluates every frame.
#[derive(Clone, Debug, Default)]
pub struct StackCache {
    entries: Vec<([u64; 3], EditMesh)>,
}

impl StackCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn evaluate(
        &mut self,
        base: &EditMesh,
        mods: &[Modifier],
        ctx: &EvalContext,
        viewport_only: bool,
    ) -> Result<EditMesh, EditError> {
        let effective: Vec<Modifier> =
            mods.iter().filter(|m| m.enabled && (!viewport_only || m.show_in_viewport)).cloned().collect();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for id in referenced_ids(mods) {
            for b in id.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            if let Some(m) = ctx.worlds.get(&id) {
                for c in m.to_cols_array() {
                    h ^= c.to_bits();
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
            if let Some(pts) = ctx.paths.get(&id) {
                for p in pts {
                    for c in [p.x, p.y, p.z] {
                        h ^= c.to_bits();
                        h = h.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                }
            }
            if let Some(operand) = ctx.booleans.get(&id) {
                h ^= mesh_hash(operand);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        if let Some(self_id) = ctx.self_id {
            if let Some(m) = ctx.worlds.get(&self_id) {
                for c in m.to_cols_array() {
                    h ^= c.to_bits();
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
        }
        let key = [mesh_hash(base), stack_hash(&effective), h];
        if let Some(hit) = self.entries.iter().find(|(k, _)| *k == key) {
            return Ok(hit.1.clone());
        }
        let mesh = evaluate_stack(base, &effective, ctx, false)?;
        if self.entries.len() >= 32 {
            self.entries.remove(0);
        }
        self.entries.push((key, mesh.clone()));
        Ok(mesh)
    }
}

pub fn is_modifier_op(op: &str) -> bool {
    matches!(
        op,
        "modifier_add" | "modifier_remove" | "modifier_move" | "modifier_set" | "modifier_toggle" | "modifier_apply"
    )
}

fn mesh_of(doc: &Document, req: &super::request::OpRequest) -> Result<(crate::model::Object, crate::model::Mesh), EditError> {
    let object = doc.object(req.object).ok_or(EditError::UnknownObject(req.object))?.clone();
    match object.kind.clone() {
        ObjectKind::Mesh(m) => Ok((object, m)),
        _ => Err(EditError::NotAMesh(req.object)),
    }
}

fn check_refs(doc: &Document, kind: &ModifierKind) -> Result<(), EditError> {
    let mut need_path: Vec<Id> = Vec::new();
    let mut need_any: Vec<Id> = Vec::new();
    match kind {
        ModifierKind::Array(p) => {
            if let Some(id) = p.object_offset {
                need_any.push(id);
            }
        }
        ModifierKind::Curve(p) => need_path.push(p.path),
        ModifierKind::Mirror(p) => {
            if let Some(id) = p.mirror_object {
                need_any.push(id);
            }
        }
        ModifierKind::Boolean(p) => {
            match doc.object(p.object).map(|o| &o.kind) {
                Some(ObjectKind::Mesh(_)) | Some(ObjectKind::Brush(_)) => {}
                Some(_) => return Err(bad(format!("boolean operand {} is neither a mesh nor a brush", p.object))),
                None => return Err(bad(format!("modifier names unknown object {}", p.object))),
            }
        }
        _ => {}
    }
    for id in need_any {
        if doc.object(id).is_none() {
            return Err(bad(format!("modifier names unknown object {id}")));
        }
    }
    for id in need_path {
        match doc.object(id).map(|o| &o.kind) {
            Some(ObjectKind::Path(_)) => {}
            Some(_) => return Err(bad(format!("modifier path {id} is not a path object"))),
            None => return Err(bad(format!("modifier names unknown object {id}"))),
        }
    }
    Ok(())
}

fn report_of(doc: &Document, id: Id, mesh: &crate::model::Mesh, base: &EditMesh) -> super::request::OpReport {
    let ctx = EvalContext::from_doc(doc, id);
    match evaluate_stack(base, &mesh.modifiers, &ctx, false) {
        Ok(eval) => {
            let adj = Adjacency::build(&eval);
            super::request::OpReport {
                verts: eval.positions.len() as u32,
                edges: adj.edges.len() as u32,
                faces: eval.faces.len() as u32,
                warnings: Vec::new(),
            }
        }
        Err(e) => super::request::OpReport {
            verts: base.positions.len() as u32,
            edges: Adjacency::build(base).edges.len() as u32,
            faces: base.faces.len() as u32,
            warnings: vec![e.to_string()],
        },
    }
}

/// Plans a modifier-stack op: mutates `Mesh.modifiers` through
/// `ReplaceObjects`, so every stack edit is one undo step.
pub fn plan_modifier(doc: &Document, req: &super::request::OpRequest) -> Result<super::request::OpOutput, EditError> {
    use super::request::OpOutput;
    use crate::editmesh::select::{SelMode, Selection};
    let (object, mut mesh) = mesh_of(doc, req)?;
    let base = match doc.files.read(&mesh.source) {
        Ok(bytes) => match super::load_triangles(&bytes, &mesh.source)
            .map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?
        {
            super::convert::LoadedMesh::Editable(base) => base,
            super::convert::LoadedMesh::Soup(_) => EditMesh::default(),
        },
        _ => EditMesh::default(),
    };
    let empty_sel = Selection { mode: SelMode::Face, ids: Vec::new() };
    let replace = |mesh: crate::model::Mesh, object: crate::model::Object| {
        let report = report_of(doc, object.id, &mesh, &base);
        let mut next = object.clone();
        next.kind = ObjectKind::Mesh(mesh);
        OpOutput {
            command: Command::Batch {
                label: format!("mesh {}", req.op),
                commands: vec![Command::ReplaceObjects { objects: vec![next] }],
            },
            selection: empty_sel.clone(),
            report,
        }
    };
    match req.op.as_str() {
        "modifier_add" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {
                modifier: Modifier,
            }
            let p: P = serde_json::from_value(req.params.clone()).map_err(|e| bad(format!("bad params: {e}")))?;
            check_refs(doc, &p.modifier.kind)?;
            mesh.modifiers.push(p.modifier);
            Ok(replace(mesh, object))
        }
        "modifier_remove" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {
                index: usize,
            }
            let p: P = serde_json::from_value(req.params.clone()).map_err(|e| bad(format!("bad params: {e}")))?;
            if p.index >= mesh.modifiers.len() {
                return Err(bad(format!("modifier {} does not exist", p.index)));
            }
            mesh.modifiers.remove(p.index);
            Ok(replace(mesh, object))
        }
        "modifier_move" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {
                index: usize,
                to: usize,
            }
            let p: P = serde_json::from_value(req.params.clone()).map_err(|e| bad(format!("bad params: {e}")))?;
            if p.index >= mesh.modifiers.len() || p.to >= mesh.modifiers.len() {
                return Err(bad(format!("modifier {} does not exist", p.index.max(p.to))));
            }
            let m = mesh.modifiers.remove(p.index);
            mesh.modifiers.insert(p.to, m);
            Ok(replace(mesh, object))
        }
        "modifier_set" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {
                index: usize,
                modifier: Modifier,
            }
            let p: P = serde_json::from_value(req.params.clone()).map_err(|e| bad(format!("bad params: {e}")))?;
            if p.index >= mesh.modifiers.len() {
                return Err(bad(format!("modifier {} does not exist", p.index)));
            }
            check_refs(doc, &p.modifier.kind)?;
            mesh.modifiers[p.index] = p.modifier;
            Ok(replace(mesh, object))
        }
        "modifier_toggle" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct P {
                index: usize,
            }
            let p: P = serde_json::from_value(req.params.clone()).map_err(|e| bad(format!("bad params: {e}")))?;
            if p.index >= mesh.modifiers.len() {
                return Err(bad(format!("modifier {} does not exist", p.index)));
            }
            mesh.modifiers[p.index].enabled = !mesh.modifiers[p.index].enabled;
            Ok(replace(mesh, object))
        }
        "modifier_apply" => plan_apply(doc, &object, &mesh, &req.params),
        _ => Err(bad(format!("unknown mesh op {}", req.op))),
    }
}

fn plan_apply(
    doc: &Document,
    object: &crate::model::Object,
    mesh: &crate::model::Mesh,
    params: &serde_json::Value,
) -> Result<super::request::OpOutput, EditError> {
    use super::request::OpOutput;
    use crate::editmesh::select::{SelMode, Selection};
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        index: usize,
    }
    let p: P = serde_json::from_value(params.clone()).map_err(|e| bad(format!("bad params: {e}")))?;
    if p.index >= mesh.modifiers.len() {
        return Err(bad(format!("modifier {} does not exist", p.index)));
    }
    let bytes = doc.files.read(&mesh.source).map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?;
    let (editable, path) = match super::load_triangles(&bytes, &mesh.source)
        .map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?
    {
        super::convert::LoadedMesh::Editable(base) => (base, mesh.source.clone()),
        super::convert::LoadedMesh::Soup(prims) => {
            (super::convert::weld_primitives(&prims)?, format!("meshes/{}.r5mesh", object.id))
        }
    };
    let ctx = EvalContext::from_doc(doc, object.id);
    let prefix: Vec<Modifier> = mesh.modifiers[..=p.index].to_vec();
    let baked = evaluate_stack(&editable, &prefix, &ctx, false)?;
    check_output(baked.positions.len(), baked.faces.len(), baked.corners.len())?;
    let bytes = super::io::write(&baked);
    let mut next_mesh = mesh.clone();
    next_mesh.source = path.clone();
    next_mesh.modifiers.drain(..=p.index);
    let mut next_object = object.clone();
    next_object.kind = ObjectKind::Mesh(next_mesh);
    let adj = Adjacency::build(&baked);
    Ok(OpOutput {
        command: Command::Batch {
            label: "mesh modifier apply".into(),
            commands: vec![
                Command::SetFile { path, bytes: Some(bytes) },
                Command::ReplaceObjects { objects: vec![next_object] },
            ],
        },
        selection: Selection { mode: SelMode::Face, ids: Vec::new() },
        report: super::request::OpReport {
            verts: baked.positions.len() as u32,
            edges: adj.edges.len() as u32,
            faces: baked.faces.len() as u32,
            warnings: Vec::new(),
        },
    })
}
