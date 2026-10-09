//! Brush CSG, face extraction, lightmap charts and the render surface list.
//!
//! World space throughout, Source inches, Z up. Every geometric decision here
//! is taken in `f64`; the `f32` pools are produced by the render emitter.

pub mod glb;
pub mod terrain;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use glam::{DMat3, DMat4, DVec2, DVec3, DVec4, Vec2, Vec3, Vec4};
use rdoc::brush::{self, Solid};
use rdoc::model::{Brush, BrushFace, Entity, Id, MapDoc, Object, ObjectKind, UvMapping, Volume};
use rir::{
    AssetTables, CollisionShape, CollisionSolid, MaterialInfo, ModelSlot, RenderSurface, VertexFormat,
};
use thiserror::Error;
use uuid::Uuid;

/// A vertex this far inside an edge splits it.
pub const TJUNCTION_EPS: f64 = 0.01;
/// Two planes closer than this in distance are the same plane.
pub const PLANE_EPS: f64 = 1e-4;
/// A face smaller than this is not drawn.
pub const AREA_EPS: f64 = 1e-7;
/// Lightmap rects are padded by this many texels on every side.
pub const LIGHTMAP_PADDING: i32 = 2;
/// Lightmap texel size used when a face does not name one.
pub const DEFAULT_LIGHTMAP_SCALE: f64 = 16.0;

/// The chart step for faces that name no lightmap scale: the map's bake texel
/// size, so raising it coarsens every chart without an explicit scale.
pub fn chart_default(doc: &MapDoc) -> f64 {
    let t = doc.settings.bake.texel_size;
    if t > 0.0 && t.is_finite() { t } else { DEFAULT_LIGHTMAP_SCALE }
}
/// The vertex-colour channels a blend material can read splat weights from.
pub const MAX_BLEND_CHANNELS: usize = 3;

#[derive(Debug, Error, PartialEq)]
pub enum CsgError {
    #[error("brush of object {object} is not a closed convex solid: {source}")]
    Brush { object: Uuid, source: brush::BrushError },
    #[error("transform of object {0} is not finite")]
    Transform(Uuid),
    #[error("face of object {object} has no material")]
    NoMaterial { object: Uuid },
    #[error("mesh of object {object} has no geometry: {reason}")]
    Mesh { object: Uuid, reason: String },
    #[error("terrain of object {object} could not be meshed: {reason}")]
    Terrain { object: Uuid, reason: String },
    #[error("lightmap chart for {surface} needs a {needed} texel chart, more than the {limit} limit")]
    ChartTooBig { surface: String, needed: u32, limit: u32 },
    #[error("surface {0} has more than {1} vertices")]
    TooManyVertices(String, u32),
}

/// World transform per object id, as the parent chain resolved it. An id that
/// is absent is the identity.
pub type WorldMap = HashMap<Id, DMat4>;

/// A plane in world space, `normal . p = dist`, normal pointing out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DPlane {
    pub normal: DVec3,
    pub dist: f64,
}

impl DPlane {
    pub fn new(normal: DVec3, dist: f64) -> Self {
        let len = normal.length();
        if len <= PLANE_EPS || !normal.is_finite() || !dist.is_finite() {
            return Self { normal: DVec3::Z, dist };
        }
        Self { normal: normal / len, dist: dist / len }
    }

    pub fn distance(&self, p: DVec3) -> f64 {
        self.normal.dot(p) - self.dist
    }

    /// The same plane with the normal reversed.
    pub fn flipped(self) -> Self {
        Self { normal: -self.normal, dist: -self.dist }
    }

    /// True when the two planes hold the same set of points.
    pub fn same(&self, other: &DPlane) -> bool {
        self.normal.dot(other.normal).abs() > 1.0 - 1e-9 && (self.dist - other.dist).abs() <= PLANE_EPS
    }

    /// True when the two planes are the same plane facing opposite ways.
    pub fn opposing(&self, other: &DPlane) -> bool {
        self.normal.dot(other.normal) < -1.0 + 1e-9 && (self.dist + other.dist).abs() <= PLANE_EPS
    }
}

/// One drawable polygon of a brush, in world space.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub object: Option<Uuid>,
    pub model: u32,
    pub material: String,
    pub plane: DPlane,
    /// World-space loop, counter-clockwise seen from outside.
    pub points: Vec<DVec3>,
    pub uv: UvMapping,
    pub lightmap_scale: f64,
    pub contents: u32,
    /// Empty when the material names none; the collision stage resolves it.
    pub surface_prop: String,
}

/// Everything the geometry stage produces, in the order the later stages want.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    pub faces: Vec<Face>,
    pub solids: Vec<CollisionSolid>,
    pub models: Vec<ModelSlot>,
    pub surfaces: Vec<RenderSurface>,
}

impl Geometry {
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for s in &self.surfaces {
            for p in &s.positions {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        for s in &self.solids {
            if let Some((a, b)) = solid_bounds(&s.shape) {
                lo = lo.min(a.as_vec3());
                hi = hi.max(b.as_vec3());
            }
        }
        if lo.x > hi.x {
            return (Vec3::ZERO, Vec3::ZERO);
        }
        (lo, hi)
    }
}

/// A brush in world space, before the face rules run.
#[derive(Clone, Debug)]
pub struct WBrush {
    object: Uuid,
    /// Position in document order, which breaks coplanar-face ties.
    order: usize,
    /// 0 for a detail brush, 1 for ordinary world geometry.
    solidity: u32,
    contents: u32,
    faces: Vec<WFace>,
    /// Every bounding plane with its template face, as the brush was before the
    /// face rules removed or split any face: collision is the whole solid.
    solid: Vec<(DPlane, BrushFace)>,
}

#[derive(Clone, Debug)]
struct WFace {
    plane: DPlane,
    poly: Vec<DVec3>,
    /// Material and UV of the template face this plane came from.
    face: BrushFace,
}

fn linear_of(m: &DMat4, object: Uuid) -> Result<(DMat3, DVec3), CsgError> {
    if !m.is_finite() {
        return Err(CsgError::Transform(object));
    }
    let (s, r, t) = m.to_scale_rotation_translation();
    if !s.is_finite() || !t.is_finite() || s.abs().min_element() <= 0.0 {
        return Err(CsgError::Transform(object));
    }
    Ok((DMat3::from_diagonal(s) * DMat3::from_quat(r), t))
}

/// Splits the document into models. Model 0 is the world; every brush entity
/// and every volume takes the next index, in document order.
pub fn model_assignment(doc: &MapDoc) -> (HashMap<Id, u32>, Vec<ModelSlot>) {
    let by_id: HashMap<Id, &Object> = doc.objects.iter().map(|o| (o.id, o)).collect();
    let mut out: HashMap<Id, u32> = HashMap::new();
    let mut models = vec![empty_slot(None)];
    for object in &doc.objects {
        let takes_model = match &object.kind {
            ObjectKind::Entity(e) => e.classname != "worldspawn" && has_brushes(&by_id, object.id),
            ObjectKind::Volume(_) => true,
            _ => false,
        };
        if takes_model {
            out.insert(object.id, models.len() as u32);
            models.push(empty_slot(Some(object.id)));
        }
    }
    for (i, m) in models.iter_mut().enumerate() {
        m.index = i as u32;
    }
    (out, models)
}

fn empty_slot(owner: Option<Uuid>) -> ModelSlot {
    ModelSlot {
        index: 0,
        owner,
        mins: Vec3::splat(f32::INFINITY),
        maxs: Vec3::splat(f32::NEG_INFINITY),
        has_render: false,
    }
}

fn has_brushes(by_id: &HashMap<Id, &Object>, parent: Id) -> bool {
    by_id.values().any(|o| o.parent == Some(parent) && matches!(o.kind, ObjectKind::Brush(_)))
}

fn to_world_brush(
    object: Uuid,
    order: usize,
    brush: &Brush,
    world: &DMat4,
) -> Result<WBrush, CsgError> {
    let solid = Solid::from_faces(&brush.faces).map_err(|source| CsgError::Brush { object, source })?;
    let (lin, offset) = linear_of(world, object)?;
    let moved = solid.transformed(lin).map_err(|source| CsgError::Brush { object, source })?;
    let template = moved.to_faces(&brush.faces);
    let flip = lin.determinant() < 0.0;
    let mut faces = Vec::with_capacity(moved.loops.len());
    for (i, loop_) in moved.loops.iter().enumerate() {
        let mut poly: Vec<DVec3> = loop_.iter().map(|v| moved.vertices[*v as usize] + offset).collect();
        let mut plane = DPlane::new(moved.planes[i].normal, moved.planes[i].dist + moved.planes[i].normal.dot(offset));
        if flip {
            poly.reverse();
            plane = plane.flipped();
        }
        let face = template.get(i).cloned().unwrap_or_default();
        if face.material.is_empty() {
            return Err(CsgError::NoMaterial { object });
        }
        faces.push(WFace { plane, poly, face });
    }
    let solid = faces.iter().map(|f| (f.plane, f.face.clone())).collect();
    Ok(WBrush { object, order, solidity: u32::from(!brush.detail), contents: brush.contents, faces, solid })
}

/// Sutherland-Hodgman split of a convex loop by a plane, returning the part
/// outside (`n.p >= -PLANE_EPS`) and the part inside.
fn split_by_plane(poly: &[DVec3], plane: &DPlane) -> (Vec<DVec3>, Vec<DVec3>) {
    if poly.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut out: Vec<DVec3> = Vec::with_capacity(poly.len() + 1);
    let mut inn: Vec<DVec3> = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let da = plane.distance(a);
        let db = plane.distance(b);
        let oa = da >= -PLANE_EPS;
        let ob = db >= -PLANE_EPS;
        if oa {
            out.push(a);
        }
        if da < PLANE_EPS {
            inn.push(a);
        }
        if oa != ob {
            let x = a + (b - a) * (da / (da - db));
            out.push(x);
            inn.push(x);
        }
    }
    (dedup_poly(out), dedup_poly(inn))
}

/// The convex polygon minus a convex volume, as the convex pieces it is cut
/// into. Both inputs are convex, so the pieces tile the difference exactly.
fn poly_minus_convex(poly: &[DVec3], volume: &[DPlane]) -> Vec<Vec<DVec3>> {
    if poly.len() < 3 {
        return Vec::new();
    }
    // A polygon that misses the volume on one side, or lies wholly inside it,
    // needs no cut at all; taking these first keeps the face count down.
    if volume.iter().any(|q| poly.iter().all(|p| q.distance(*p) >= -PLANE_EPS)) {
        return vec![poly.to_vec()];
    }
    if volume.iter().all(|q| poly.iter().all(|p| q.distance(*p) <= PLANE_EPS)) {
        return Vec::new();
    }
    let mut pieces: Vec<Vec<DVec3>> = Vec::new();
    let mut rest: Vec<Vec<DVec3>> = vec![poly.to_vec()];
    for q in volume {
        let mut next: Vec<Vec<DVec3>> = Vec::new();
        for r in rest.drain(..) {
            let (outside, inside) = split_by_plane(&r, q);
            if outside.len() >= 3 && poly_area(&outside) > AREA_EPS {
                pieces.push(outside);
            }
            if inside.len() >= 3 && poly_area(&inside) > AREA_EPS {
                next.push(inside);
            }
        }
        rest = next;
    }
    pieces
}

fn dedup_poly(mut p: Vec<DVec3>) -> Vec<DVec3> {
    let mut out: Vec<DVec3> = Vec::with_capacity(p.len());
    for v in p.drain(..) {
        if out.last().is_some_and(|last| (*last - v).length() <= PLANE_EPS) {
            continue;
        }
        out.push(v);
    }
    while out.len() > 1 && (out[0] - out[out.len() - 1]).length() <= PLANE_EPS {
        out.pop();
    }
    out
}

fn poly_area(poly: &[DVec3]) -> f64 {
    if poly.len() < 3 {
        return 0.0;
    }
    let mut n = DVec3::ZERO;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n.length() * 0.5
}

/// True when every point of `inner` lies inside the convex polygon `outer`.
/// Both are coplanar, so only the outer loop's edges decide.
fn poly_contains(outer: &[DVec3], inner: &[DVec3]) -> bool {
    if outer.len() < 3 || inner.is_empty() {
        return false;
    }
    for i in 0..outer.len() {
        let a = outer[i];
        let b = outer[(i + 1) % outer.len()];
        let e = b - a;
        let n = e.cross(DVec3::Z);
        if n.length_squared() <= PLANE_EPS * PLANE_EPS {
            return false;
        }
        let n = n.normalize();
        if inner.iter().any(|p| n.dot(*p - a) < -TJUNCTION_EPS) {
            return false;
        }
    }
    true
}

/// True when the two coplanar loops hold the same corner set.
fn poly_same(a: &[DVec3], b: &[DVec3]) -> bool {
    if a.len() != b.len() || a.len() < 3 {
        return false;
    }
    a.iter().all(|p| b.iter().any(|q| (*p - *q).length() <= PLANE_EPS))
        && b.iter().all(|q| a.iter().any(|p| (*p - *q).length() <= PLANE_EPS))
}

/// Applies one occluder's rules to one brush: the coplanar removals first, then
/// every surviving loop is cut back to the outside of the occluder. A brush
/// face that an occluder cuts in two keeps both pieces.
fn occlude(brush: &mut WBrush, occ: &WBrush) {
    let volume: Vec<DPlane> = occ.faces.iter().map(|q| q.plane).collect();
    let mut out: Vec<WFace> = Vec::with_capacity(brush.faces.len());
    for f in brush.faces.drain(..) {
        let mut drop = false;
        for q in &occ.faces {
            // Two solids that meet on one whole face have no visible surface
            // there, so both faces go. A partial overlap is left to the volume
            // clip, which cuts the exposed part back exactly.
            if f.plane.opposing(&q.plane) && poly_same(&f.poly, &q.poly) {
                drop = true;
                break;
            }
            // A same-facing duplicate the earlier brush already covers goes too.
            if f.plane.same(&q.plane) && occ.order < brush.order && poly_contains(&q.poly, &f.poly) {
                drop = true;
                break;
            }
        }
        if drop {
            continue;
        }
        for poly in poly_minus_convex(&f.poly, &volume) {
            out.push(WFace { plane: f.plane, poly, face: f.face.clone() });
        }
    }
    brush.faces = out;
}

/// Every model's brushes in world space, after the face rules.
type ModelBrushes = (Vec<(u32, Vec<WBrush>)>, Vec<ModelSlot>);

fn world_brushes(doc: &MapDoc, worlds: &WorldMap) -> Result<ModelBrushes, CsgError> {
    let (model_of_obj, mut models) = model_assignment(doc);
    let mut by_model: HashMap<u32, Vec<WBrush>> = HashMap::new();
    let mut order = 0usize;
    for o in &doc.objects {
        let ObjectKind::Brush(b) = &o.kind else { continue };
        if o.parent.is_some_and(|p| model_of_obj.contains_key(&p)) {
            continue;
        }
        let world = worlds.get(&o.id).copied().unwrap_or(DMat4::IDENTITY);
        by_model.entry(0).or_default().push(to_world_brush(o.id, order, b, &world)?);
        order += 1;
    }
    for o in &doc.objects {
        let Some(&model) = model_of_obj.get(&o.id) else { continue };
        let mut list: Vec<WBrush> = Vec::new();
        match &o.kind {
            ObjectKind::Volume(v) => {
                let world = worlds.get(&o.id).copied().unwrap_or(DMat4::IDENTITY);
                for b in &v.brushes {
                    list.push(to_world_brush(o.id, order, b, &world)?);
                    order += 1;
                }
            }
            ObjectKind::Entity(_) => {
                for child in doc.objects.iter().filter(|c| c.parent == Some(o.id)) {
                    let ObjectKind::Brush(b) = &child.kind else { continue };
                    let world = worlds.get(&child.id).copied().unwrap_or(DMat4::IDENTITY);
                    list.push(to_world_brush(child.id, order, b, &world)?);
                    order += 1;
                }
            }
            _ => {}
        }
        if !list.is_empty() {
            by_model.insert(model, list);
        }
    }
    for list in by_model.values_mut() {
        let snapshot = list.clone();
        for (i, slot) in list.iter_mut().enumerate() {
            for (j, occ) in snapshot.iter().enumerate() {
                if i == j || occ.solidity < slot.solidity {
                    continue;
                }
                occlude(slot, occ);
            }
        }
    }
    let mut ordered: Vec<(u32, Vec<WBrush>)> = by_model.into_iter().collect();
    ordered.sort_by_key(|(m, _)| *m);
    for (m, list) in &ordered {
        for b in list {
            let slot = &mut models[*m as usize];
            slot.has_render |= !b.faces.is_empty();
            for f in &b.faces {
                for p in &f.poly {
                    slot.mins = slot.mins.min(p.as_vec3());
                    slot.maxs = slot.maxs.max(p.as_vec3());
                }
            }
        }
    }
    for m in models.iter_mut() {
        if m.mins.x > m.maxs.x {
            m.mins = Vec3::ZERO;
            m.maxs = Vec3::ZERO;
            m.has_render = false;
        }
    }
    Ok((ordered, models))
}

/// Every render face of the document, in world space, after the coplanar and
/// overlap rules and with the T-junction pass applied.
pub fn build_faces(doc: &MapDoc, worlds: &WorldMap) -> Result<Vec<Face>, CsgError> {
    Ok(build_geometry(doc, worlds)?.faces)
}

/// The same face list before the T-junction pass, so a caller can see the
/// split it does.
pub fn build_faces_raw(doc: &MapDoc, worlds: &WorldMap) -> Result<Vec<Face>, CsgError> {
    let (by_model, models) = world_brushes(doc, worlds)?;
    Ok(extract_faces(&by_model, models))
}

/// Face list, collision solids, model slots and render surfaces.
pub fn build_geometry(doc: &MapDoc, worlds: &WorldMap) -> Result<Geometry, CsgError> {
    let (by_model, models) = world_brushes(doc, worlds)?;
    let mut geo = collect_geometry(&by_model, models);
    fix_tjunctions(&mut geo.faces);
    geo.surfaces = build_surfaces(&geo.faces, &AssetTables::default(), chart_default(doc))?;
    Ok(geo)
}

/// Face list, collision solids, model slots and render surfaces for a model
/// table the CSG pass already produced.
pub fn collect_geometry(by_model: &[(u32, Vec<WBrush>)], models: Vec<ModelSlot>) -> Geometry {
    let mut faces: Vec<Face> = Vec::new();
    let mut solids: Vec<CollisionSolid> = Vec::new();
    for (model, list) in by_model.iter() {
        for b in list {
            for f in &b.faces {
                faces.push(Face {
                    object: Some(b.object),
                    model: *model,
                    material: f.face.material.clone(),
                    plane: f.plane,
                    points: f.poly.clone(),
                    uv: f.face.uv,
                    lightmap_scale: f.face.lightmap_scale,
                    contents: b.contents,
                    surface_prop: String::new(),
                });
            }
            // A face the rules removed still bounds the solid; its plane stays.
            if b.solid.is_empty() {
                continue;
            }
            solids.push(CollisionSolid {
                object: Some(b.object),
                model: *model,
                shape: CollisionShape::Brush {
                    planes: b.solid.iter().map(|(p, _)| (p.normal.as_vec3(), p.dist as f32)).collect(),
                },
                contents: b.contents,
                surface_prop: 0,
                surfaces: b
                    .solid
                    .iter()
                    .map(|(_, f)| rir::CollisionSurface { material: f.material.clone(), surface_type: 0 })
                    .collect(),
            });
        }
    }
    Geometry { faces, solids, models, surfaces: Vec::new() }
}

fn extract_faces(by_model: &[(u32, Vec<WBrush>)], models: Vec<ModelSlot>) -> Vec<Face> {
    collect_geometry(by_model, models).faces
}

// ---- T-junctions ------------------------------------------------------------

/// A vertex pool keyed on a TJUNCTION_EPS grid, so a lookup touches 27 cells.
/// Cell size of the grid segment queries walk: the fine grid dedupes, but a
/// diagonal edge's bounding box spans ~1e10 fine cells, so edges search this one.
const SEGMENT_CELL: f64 = 64.0;

struct VertexGrid {
    cell: f64,
    map: HashMap<(i64, i64, i64), Vec<usize>>,
    coarse: HashMap<(i64, i64, i64), Vec<usize>>,
    points: Vec<DVec3>,
}

fn coarse_of(p: DVec3) -> (i64, i64, i64) {
    ((p.x / SEGMENT_CELL).floor() as i64, (p.y / SEGMENT_CELL).floor() as i64, (p.z / SEGMENT_CELL).floor() as i64)
}

impl VertexGrid {
    fn new(points: &[DVec3]) -> Self {
        let mut g = Self { cell: TJUNCTION_EPS, map: HashMap::new(), coarse: HashMap::new(), points: Vec::new() };
        for p in points {
            if p.is_finite() {
                g.insert(*p);
            }
        }
        g
    }

    fn cell_of(&self, p: DVec3) -> (i64, i64, i64) {
        ((p.x / self.cell).floor() as i64, (p.y / self.cell).floor() as i64, (p.z / self.cell).floor() as i64)
    }

    fn insert(&mut self, p: DVec3) {
        let key = self.cell_of(p);
        let bucket = self.map.entry(key).or_default();
        if bucket.iter().any(|&i| (self.points[i] - p).length() <= PLANE_EPS) {
            return;
        }
        bucket.push(self.points.len());
        self.coarse.entry(coarse_of(p)).or_default().push(self.points.len());
        self.points.push(p);
    }

    /// Points within TJUNCTION_EPS of the open segment `a..b`.
    fn on_segment(&self, a: DVec3, b: DVec3) -> Vec<DVec3> {
        let d = b - a;
        let len2 = d.length_squared();
        if len2 <= PLANE_EPS * PLANE_EPS {
            return Vec::new();
        }
        // Sampling at half a cell and taking each sample's 3x3x3 neighbourhood
        // covers every cell the segment (plus TJUNCTION_EPS) passes through.
        let steps = (len2.sqrt() / (SEGMENT_CELL * 0.5)).ceil().max(1.0) as usize;
        let mut cells: Vec<(i64, i64, i64)> = Vec::new();
        for k in 0..=steps {
            let (x, y, z) = coarse_of(a + d * (k as f64 / steps as f64));
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        cells.push((x + dx, y + dy, z + dz));
                    }
                }
            }
        }
        cells.sort_unstable();
        cells.dedup();
        let mut out: Vec<DVec3> = Vec::new();
        for cell in cells {
            let Some(list) = self.coarse.get(&cell) else { continue };
            for &i in list {
                let p = self.points[i];
                let t = (p - a).dot(d) / len2;
                if t <= PLANE_EPS || t >= 1.0 - PLANE_EPS {
                    continue;
                }
                if (p - (a + d * t)).length() > TJUNCTION_EPS {
                    continue;
                }
                out.push(p);
            }
        }
        out
    }
}

/// Splits every edge at the vertices lying on it, so no face has a vertex
/// strictly inside another face's edge.
pub fn fix_tjunctions(faces: &mut [Face]) {
    let all: Vec<DVec3> = faces.iter().flat_map(|f| f.points.iter().copied()).collect();
    if all.is_empty() {
        return;
    }
    let grid = VertexGrid::new(&all);
    for f in faces.iter_mut() {
        let mut out: Vec<DVec3> = Vec::with_capacity(f.points.len() * 2);
        for i in 0..f.points.len() {
            let a = f.points[i];
            let b = f.points[(i + 1) % f.points.len()];
            out.push(a);
            let mut cuts = grid.on_segment(a, b);
            cuts.sort_by(|p, q| {
                let d = b - a;
                let tp = (p - a).dot(d);
                let tq = (q - a).dot(d);
                tp.partial_cmp(&tq).unwrap_or(std::cmp::Ordering::Equal)
            });
            for c in cuts {
                if out.iter().any(|p| (*p - c).length() <= PLANE_EPS) {
                    continue;
                }
                out.push(c);
            }
        }
        f.points = out;
    }
}

// ---- render surfaces --------------------------------------------------------

/// Triangulates, UV-maps and charts every face, grouped into one surface per
/// model and material. A `nodraw` material emits no render face; its collision
/// survives in `solids`, which the caller already holds.
pub fn build_surfaces(
    faces: &[Face],
    assets: &AssetTables,
    chart_default: f64,
) -> Result<Vec<RenderSurface>, CsgError> {
    let mut order: Vec<(u32, String)> = Vec::new();
    for f in faces {
        if nodraw(assets, &f.material) {
            continue;
        }
        let key = (f.model, f.material.clone());
        if !order.contains(&key) {
            order.push(key);
        }
    }
    let mut out: Vec<RenderSurface> = Vec::with_capacity(order.len());
    for (model, material) in order {
        let group: Vec<&Face> = faces
            .iter()
            .filter(|f| f.model == model && f.material == material && !nodraw(assets, &f.material))
            .collect();
        let info = assets.materials.get(&material);
        let format = info.map(|m| m.vertex_format).unwrap_or(VertexFormat::LitBump);
        let mut s = triangulate_group(&group, model, &material, format, info);
        let scales: Vec<f64> = group.iter().map(|f| f.lightmap_scale).collect();
        if scales.iter().any(|v| *v > 0.0) {
            chart_surface(&mut s, &scales)?;
        } else {
            chart_surface(&mut s, &[chart_default])?;
        }
        out.push(s);
    }
    Ok(out)
}

/// Solid faces nobody sees still block light: a Source mapper textures every
/// side the player cannot reach `nodraw`, so without them the sun shines
/// through the back of every structure. Batched under the u16 index limit of
/// a shadow mesh. A sky brush is the exception: light that reaches its sky
/// face has reached the sky, so its nodraw backs must not shade the world.
pub fn shadow_only_faces(faces: &[Face]) -> Vec<(Vec<Vec3>, Vec<u32>)> {
    const MAX_VERTS: usize = 60_000;
    let sky_brushes: std::collections::HashSet<Uuid> =
        faces.iter().filter(|f| crate::meshes::is_sky_material(&f.material)).filter_map(|f| f.object).collect();
    let mut out: Vec<(Vec<Vec3>, Vec<u32>)> = Vec::new();
    let mut positions: Vec<Vec3> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for f in faces {
        let m = f.material.to_ascii_lowercase();
        if !matches!(m.as_str(), "tools/toolsnodraw" | "tools/toolsblocklight") || f.points.len() < 3 {
            continue;
        }
        if f.object.is_some_and(|o| sky_brushes.contains(&o)) {
            continue;
        }
        if positions.len() + f.points.len() > MAX_VERTS {
            out.push((std::mem::take(&mut positions), std::mem::take(&mut indices)));
        }
        let base = positions.len() as u32;
        positions.extend(f.points.iter().map(|p| p.as_vec3()));
        for k in 1..f.points.len() as u32 - 1 {
            indices.extend([base, base + k, base + k + 1]);
        }
    }
    if !indices.is_empty() {
        out.push((positions, indices));
    }
    out
}

fn nodraw(assets: &AssetTables, material: &str) -> bool {
    match assets.materials.get(material) {
        Some(m) => m.nodraw,
        None => {
            let m = material.to_ascii_lowercase();
            m.is_empty() || m.starts_with("tools/") || m.starts_with("tools\\") || m.contains("nodraw")
        }
    }
}

fn triangulate_group(
    group: &[&Face],
    model: u32,
    material: &str,
    format: VertexFormat,
    info: Option<&MaterialInfo>,
) -> RenderSurface {
    let mut s = RenderSurface {
        object: group.first().and_then(|f| f.object),
        model,
        material: material.to_string(),
        positions: Vec::new(),
        normals: Vec::new(),
        tangents: Vec::new(),
        uv0: Vec::new(),
        uv1: Vec::new(),
        colors: Vec::new(),
        indices: Vec::new(),
        cast_shadows: !info.is_some_and(|m| m.translucent),
        lightmap_texel: DEFAULT_LIGHTMAP_SCALE as f32,
        lightmap_size: [0, 0],
    };
    let wants_tangent = matches!(format, VertexFormat::LitBump | VertexFormat::UnlitTs);
    for f in group {
        let n = f.plane.normal;
        let (tu, _) = tangent_basis(n);
        let base = s.positions.len() as u32;
        for p in &f.points {
            let uv = brush::tile_uv(brush::project_uv(&f.uv, n, *p));
            s.positions.push(p.as_vec3());
            s.normals.push(n.as_vec3());
            if wants_tangent {
                s.tangents.push(DVec4::new(tu.x, tu.y, tu.z, 1.0).as_vec4());
            }
            s.uv0.push(Vec2::new(uv[0] as f32, uv[1] as f32));
            s.uv1.push(Vec2::ZERO);
            s.colors.push([255, 255, 255, 255]);
        }
        // The engine reads a front face whose cross product opposes the vertex
        // normal, so a loop wound counter-clockwise from outside is emitted
        // with the opposite order.
        let count = f.points.len() as u32;
        for i in 1..count.saturating_sub(1) {
            s.indices.extend_from_slice(&[base, base + i + 1, base + i]);
        }
    }
    if wants_tangent {
        s.tangents = uv_tangents(&s.positions, &s.normals, &s.uv0, &s.indices);
    }
    s
}

/// Per-vertex tangents along +U of `uv`, the frame the lit shaders read a
/// normal map in. `w` is negative where the bitangent `cross(n, t)` runs
/// against +V, the unmirrored case on every shipped map.
pub fn uv_tangents(positions: &[Vec3], normals: &[Vec3], uv: &[Vec2], indices: &[u32]) -> Vec<Vec4> {
    let n = positions.len();
    let mut tan = vec![DVec3::ZERO; n];
    let mut bit = vec![DVec3::ZERO; n];
    if normals.len() == n && uv.len() == n {
        for t in indices.chunks_exact(3) {
            let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
            if a >= n || b >= n || c >= n {
                continue;
            }
            let e1 = (positions[b] - positions[a]).as_dvec3();
            let e2 = (positions[c] - positions[a]).as_dvec3();
            let d1 = (uv[b] - uv[a]).as_dvec2();
            let d2 = (uv[c] - uv[a]).as_dvec2();
            let det = d1.x * d2.y - d1.y * d2.x;
            if det.abs() < 1e-12 {
                continue;
            }
            // Area-weighted: the raw cross products carry each triangle's size.
            let area = e1.cross(e2).length();
            let du = (e1 * d2.y - e2 * d1.y) / det;
            let dv = (e2 * d1.x - e1 * d2.x) / det;
            for v in [a, b, c] {
                tan[v] += du.normalize_or_zero() * area;
                bit[v] += dv.normalize_or_zero() * area;
            }
        }
    }
    (0..n)
        .map(|i| {
            let nn = normals.get(i).map_or(DVec3::Z, |v| v.as_dvec3().normalize_or(DVec3::Z));
            let mut t = tan[i] - nn * nn.dot(tan[i]);
            if t.length_squared() < 1e-12 {
                t = tangent_basis(nn).0;
            }
            let t = t.normalize();
            let w = if nn.cross(t).dot(bit[i]) < 0.0 { -1.0 } else { 1.0 };
            DVec4::new(t.x, t.y, t.z, w).as_vec4()
        })
        .collect()
}

/// A unit tangent frame for a face normal, matching the box brush convention.
pub fn tangent_basis(n: DVec3) -> (DVec3, DVec3) {
    let seed = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = n.cross(seed).normalize();
    let v = n.cross(u).normalize();
    (u, v)
}

/// Packs every face of a surface into one lightmap chart: a planar projection
/// per face, rects padded by [`LIGHTMAP_PADDING`] texels and shelf packed.
///
/// A surface too large for one chart at its texel size (a shipped map's
/// skybox shell, its terrain) is charted at twice the size, and again, until
/// it fits: the bake lights it more coarsely rather than not at all.
pub fn chart_surface(surface: &mut RenderSurface, face_scales: &[f64]) -> Result<(), CsgError> {
    let named: Vec<f64> = face_scales.iter().copied().filter(|v| *v > 0.0).collect();
    let mut step = if named.is_empty() {
        DEFAULT_LIGHTMAP_SCALE
    } else {
        named.iter().sum::<f64>() / named.len() as f64
    };
    if step <= 0.0 || !step.is_finite() {
        return Err(CsgError::ChartTooBig {
            surface: surface.material.clone(),
            needed: 0,
            limit: LIGHTMAP_CHART_LIMIT,
        });
    }
    let mut tries = 0;
    loop {
        match chart_surface_at(surface, step) {
            Err(CsgError::ChartTooBig { .. }) if tries < 16 => {
                step *= 2.0;
                tries += 1;
            }
            other => return other,
        }
    }
}

fn chart_surface_at(surface: &mut RenderSurface, step: f64) -> Result<(), CsgError> {
    // `triangulate_group` emits one run of triangles per face, each run
    // starting at its own first vertex.
    let mut groups: Vec<(DVec3, Vec<usize>)> = Vec::new();
    let mut last = None;
    for (t, tri) in surface.indices.chunks_exact(3).enumerate() {
        let key = tri[0];
        match groups.last_mut() {
            Some(g) if last == Some(key) => g.1.push(t),
            _ => groups.push((surface.normals[key as usize].as_dvec3(), vec![t])),
        }
        last = Some(key);
    }
    layout_charts(surface, step, &groups)
}

/// Charts a free-form mesh surface. Triangles are grouped by the axis their
/// face leans toward and then by connectivity, so a terrain or a cliff charts
/// as a few sheets rather than one padded rect per triangle. Each sheet is
/// projected along its axis, which no triangle in it faces away from, and a
/// vertex on a sheet border is split so every sheet owns its lightmap
/// coordinates.
pub fn chart_mesh_surface(surface: &mut RenderSurface, step: f64) -> Result<(), CsgError> {
    let groups = split_mesh_charts(surface);
    let mut step = if step > 0.0 && step.is_finite() { step } else { DEFAULT_LIGHTMAP_SCALE };
    let mut tries = 0;
    loop {
        match layout_charts(surface, step, &groups) {
            Err(CsgError::ChartTooBig { .. }) if tries < 16 => {
                step *= 2.0;
                tries += 1;
            }
            other => return other,
        }
    }
}

const AXES: [DVec3; 6] = [DVec3::X, DVec3::NEG_X, DVec3::Y, DVec3::NEG_Y, DVec3::Z, DVec3::NEG_Z];

/// The axis a triangle's face leans toward, on the side its vertex normal is.
fn face_axis(surface: &RenderSurface, tri: &[u32; 3]) -> usize {
    let p = |i: u32| surface.positions[i as usize].as_dvec3();
    let vn = surface.normals.get(tri[0] as usize).map_or(DVec3::Z, |n| n.as_dvec3());
    let mut n = (p(tri[1]) - p(tri[0])).cross(p(tri[2]) - p(tri[0]));
    if n.length_squared() < 1.0e-12 {
        n = vn;
    } else if n.dot(vn) < 0.0 {
        n = -n;
    }
    let a = n.abs();
    if a.x >= a.y && a.x >= a.z {
        usize::from(n.x < 0.0)
    } else if a.y >= a.z {
        2 + usize::from(n.y < 0.0)
    } else {
        4 + usize::from(n.z < 0.0)
    }
}

/// Reorders a mesh surface so each chart's triangles are contiguous and own
/// their vertices, duplicating a vertex wherever two charts meet, and returns
/// every chart's projection axis and triangles.
fn split_mesh_charts(surface: &mut RenderSurface) -> Vec<(DVec3, Vec<usize>)> {
    use std::collections::HashMap;
    let tris: Vec<[u32; 3]> = surface.indices.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    let axis: Vec<usize> = tris.iter().map(|t| face_axis(surface, t)).collect();
    // Welding by position lets a mesh exported with split normals or UV seams
    // still chart as one sheet.
    let mut weld: HashMap<[i64; 3], usize> = HashMap::new();
    let welded: Vec<usize> = surface
        .positions
        .iter()
        .map(|p| {
            let k = [(p.x * 64.0).round() as i64, (p.y * 64.0).round() as i64, (p.z * 64.0).round() as i64];
            let next = weld.len();
            *weld.entry(k).or_insert(next)
        })
        .collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut parent: Vec<usize> = (0..tris.len()).collect();
    let mut owner: HashMap<(usize, usize), usize> = HashMap::new();
    for (t, tri) in tris.iter().enumerate() {
        for &v in tri {
            let key = (welded[v as usize], axis[t]);
            match owner.get(&key) {
                Some(&o) => {
                    let (a, b) = (find(&mut parent, o), find(&mut parent, t));
                    parent[a] = b;
                }
                None => {
                    owner.insert(key, t);
                }
            }
        }
    }
    let mut chart_of_root: HashMap<usize, usize> = HashMap::new();
    let mut members: Vec<Vec<usize>> = Vec::new();
    for t in 0..tris.len() {
        let r = find(&mut parent, t);
        let c = *chart_of_root.entry(r).or_insert_with(|| {
            members.push(Vec::new());
            members.len() - 1
        });
        members[c].push(t);
    }

    let old = std::mem::take(surface);
    let n = old.positions.len();
    let mut out = RenderSurface {
        object: old.object,
        model: old.model,
        material: old.material.clone(),
        cast_shadows: old.cast_shadows,
        lightmap_texel: old.lightmap_texel,
        ..RenderSurface::default()
    };
    let mut groups = Vec::with_capacity(members.len());
    for list in &members {
        let mut local: HashMap<u32, u32> = HashMap::new();
        let first_tri = out.indices.len() / 3;
        for &t in list {
            for &v in &tris[t] {
                let i = *local.entry(v).or_insert_with(|| {
                    let vi = v as usize;
                    out.positions.push(old.positions[vi]);
                    out.normals.push(old.normals.get(vi).copied().unwrap_or(glam::Vec3::Z));
                    if old.tangents.len() == n {
                        out.tangents.push(old.tangents[vi]);
                    }
                    if old.uv0.len() == n {
                        out.uv0.push(old.uv0[vi]);
                    }
                    if old.colors.len() == n {
                        out.colors.push(old.colors[vi]);
                    }
                    out.uv1.push(Vec2::ZERO);
                    (out.positions.len() - 1) as u32
                });
                out.indices.push(i);
            }
        }
        groups.push((AXES[axis[list[0]]], (first_tri..first_tri + list.len()).collect()));
    }
    *surface = out;
    groups
}

fn layout_charts(surface: &mut RenderSurface, step: f64, groups: &[(DVec3, Vec<usize>)]) -> Result<(), CsgError> {
    surface.lightmap_texel = step as f32;
    let mut rects: Vec<(i32, i32, i32, i32)> = Vec::with_capacity(groups.len());
    let mut basis: Vec<(DVec3, DVec3, DVec3)> = Vec::with_capacity(groups.len());
    for (n, tris) in groups {
        let n = *n;
        let (u, v) = tangent_basis(n);
        let mut lo = DVec2::splat(f64::INFINITY);
        let mut hi = DVec2::splat(f64::NEG_INFINITY);
        for &t in tris {
            for &vi in &surface.indices[t * 3..t * 3 + 3] {
                let p = surface.positions[vi as usize].as_dvec3();
                let c = p - n * n.dot(p);
                let a = DVec2::new(c.dot(u), c.dot(v)) / step;
                lo = lo.min(a);
                hi = hi.max(a);
            }
        }
        if !lo.is_finite() {
            lo = DVec2::ZERO;
            hi = DVec2::ZERO;
        }
        let (Some(x0), Some(y0), Some(x1), Some(y1)) =
            (texel_i(lo.x.floor()), texel_i(lo.y.floor()), texel_i(hi.x.ceil()), texel_i(hi.y.ceil()))
        else {
            return Err(CsgError::ChartTooBig {
                surface: surface.material.clone(),
                needed: LIGHTMAP_CHART_LIMIT + 1,
                limit: LIGHTMAP_CHART_LIMIT,
            });
        };
        rects.push((x0 - LIGHTMAP_PADDING, y0 - LIGHTMAP_PADDING, x1 + LIGHTMAP_PADDING, y1 + LIGHTMAP_PADDING));
        basis.push((u, v, n));
    }
    let (w, h, origins) = pack_rects(&rects).ok_or_else(|| CsgError::ChartTooBig {
        surface: surface.material.clone(),
        needed: rects.iter().map(|r| (r.2 - r.0) * (r.3 - r.1)).max().unwrap_or(0) as u32,
        limit: LIGHTMAP_CHART_LIMIT,
    })?;
    surface.lightmap_size = [w.max(1), h.max(1)];
    let w = (w as f64).max(1.0);
    let h = (h as f64).max(1.0);
    for (gi, (_, tris)) in groups.iter().enumerate() {
        let (u, v, n) = basis[gi];
        let (ox, oy) = origins[gi];
        let rect = rects[gi];
        for &t in tris {
            for &vi in &surface.indices[t * 3..t * 3 + 3] {
                let p = surface.positions[vi as usize].as_dvec3();
                let c = p - n * n.dot(p);
                let a = DVec2::new(c.dot(u), c.dot(v)) / step;
                surface.uv1[vi as usize] = Vec2::new(
                    ((ox as f64 + (a.x - rect.0 as f64)) / w) as f32,
                    ((oy as f64 + (a.y - rect.1 as f64)) / h) as f32,
                );
            }
        }
    }
    Ok(())
}

/// Largest lightmap chart edge a single surface may ask for.
pub const LIGHTMAP_CHART_LIMIT: u32 = 8192;

/// A texel coordinate the chart can hold, or `None` when it is out of range.
fn texel_i(v: f64) -> Option<i32> {
    if v.is_finite() && v.abs() <= LIGHTMAP_CHART_LIMIT as f64 {
        Some(v as i32)
    } else {
        None
    }
}

/// Shelf packs integer rects into a square chart, doubling until they fit.
type PackedCharts = (u32, u32, Vec<(i32, i32)>);

fn pack_rects(rects: &[(i32, i32, i32, i32)]) -> Option<PackedCharts> {
    if rects.is_empty() {
        return Some((1, 1, Vec::new()));
    }
    let mut width = 16i32;
    loop {
        if let Some(at) = try_pack(rects, width) {
            return Some((width as u32, width as u32, at));
        }
        width *= 2;
        if width > LIGHTMAP_CHART_LIMIT as i32 {
            return None;
        }
    }
}

fn try_pack(rects: &[(i32, i32, i32, i32)], width: i32) -> Option<Vec<(i32, i32)>> {
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by_key(|&i| (rects[i].3 - rects[i].1, rects[i].2 - rects[i].0));
    let mut out = vec![(0i32, 0i32); rects.len()];
    let (mut x, mut y, mut shelf) = (0i32, 0i32, 0i32);
    for &i in &order {
        let w = (rects[i].2 - rects[i].0).max(1);
        let h = (rects[i].3 - rects[i].1).max(1);
        if w > width {
            return None;
        }
        if x + w > width {
            x = 0;
            y += shelf;
            shelf = 0;
        }
        if y + h > width {
            return None;
        }
        out[i] = (x, y);
        x += w;
        shelf = shelf.max(h);
    }
    Some(out)
}

// ---- collision helpers ------------------------------------------------------

/// Every point a collision shape contributes, for bounds and model slots.
pub fn solid_bounds(shape: &CollisionShape) -> Option<(DVec3, DVec3)> {
    match shape {
        CollisionShape::Brush { planes } => {
            let verts = hull_vertices(planes)?;
            Some((
                verts.iter().copied().fold(DVec3::splat(f64::INFINITY), DVec3::min),
                verts.iter().copied().fold(DVec3::splat(f64::NEG_INFINITY), DVec3::max),
            ))
        }
        CollisionShape::Triangles { vertices, .. } | CollisionShape::ConvexHull { points: vertices } => {
            if vertices.is_empty() {
                return None;
            }
            Some((
                vertices.iter().fold(Vec3::splat(f32::INFINITY), |a, b| a.min(*b)).as_dvec3(),
                vertices.iter().fold(Vec3::splat(f32::NEG_INFINITY), |a, b| a.max(*b)).as_dvec3(),
            ))
        }
        CollisionShape::Prop { mins, maxs, .. } => (mins.x <= maxs.x).then(|| (mins.as_dvec3(), maxs.as_dvec3())),
    }
}

/// Corner points of the convex volume `{x : n.x <= d}`, or `None` when the half
/// spaces do not bound a solid.
pub fn hull_vertices(planes: &[(Vec3, f32)]) -> Option<Vec<DVec3>> {
    let pl: Vec<DPlane> = planes.iter().map(|(n, d)| DPlane::new(n.as_dvec3(), *d as f64)).collect();
    hull_vertices_planes(&pl)
}

pub fn hull_vertices_planes(planes: &[DPlane]) -> Option<Vec<DVec3>> {
    let eps = 1.0e-3;
    let mut pts: Vec<DVec3> = Vec::new();
    for i in 0..planes.len() {
        for j in (i + 1)..planes.len() {
            for k in (j + 1)..planes.len() {
                let (a, b, c) = (planes[i].normal, planes[j].normal, planes[k].normal);
                let det = a.dot(b.cross(c));
                if det.abs() < 1e-9 {
                    continue;
                }
                let d = DVec3::new(planes[i].dist, planes[j].dist, planes[k].dist);
                let x = (b.cross(c) * d.x + c.cross(a) * d.y + a.cross(b) * d.z) / det;
                if x.is_finite() && planes.iter().all(|p| p.distance(x) <= eps) {
                    pts.push(x);
                }
            }
        }
    }
    let mut verts: Vec<DVec3> = Vec::with_capacity(pts.len());
    for p in pts {
        if verts.iter().any(|q| (*q - p).length() < eps * 10.0) {
            continue;
        }
        verts.push(p);
    }
    (verts.len() >= 4).then_some(verts)
}

/// The out-of-plane loop of every plane that bounds the volume, in plane order.
pub fn plane_loops(planes: &[DPlane]) -> Vec<(usize, Vec<u32>)> {
    let Some(verts) = hull_vertices_planes(planes) else { return Vec::new() };
    let eps = 1.0e-2;
    let mut out = Vec::new();
    for (pi, p) in planes.iter().enumerate() {
        let ring: Vec<u32> = verts
            .iter()
            .enumerate()
            .filter(|(_, v)| p.distance(**v).abs() < eps)
            .map(|(i, _)| i as u32)
            .collect();
        if ring.len() < 3 {
            continue;
        }
        let c = ring.iter().map(|&i| verts[i as usize]).sum::<DVec3>() / ring.len() as f64;
        let u = (verts[ring[0] as usize] - c).normalize();
        let w = p.normal.cross(u);
        let mut keyed: Vec<(f64, u32)> = ring
            .iter()
            .map(|&i| {
                let d = verts[i as usize] - c;
                (d.dot(w).atan2(d.dot(u)), i)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        out.push((pi, keyed.into_iter().map(|(_, i)| i).collect()));
    }
    out
}

// ---- document queries the entity stage needs --------------------------------

/// The model index every object contributes geometry to, matching
/// [`model_assignment`].
pub fn object_model(doc: &MapDoc) -> HashMap<Id, u32> {
    let (mut map, _) = model_assignment(doc);
    for o in &doc.objects {
        if let ObjectKind::Brush(_) = o.kind {
            let m = o.parent.and_then(|p| map.get(&p).copied()).unwrap_or(0);
            map.insert(o.id, m);
        }
    }
    map
}

/// Volumes take one model each, in document order, matching `model_of`.
pub fn volume_models(doc: &MapDoc) -> Vec<(&Object, &Volume)> {
    doc.objects
        .iter()
        .filter_map(|o| match &o.kind {
            ObjectKind::Volume(v) => Some((o, v)),
            _ => None,
        })
        .collect()
}

/// Brush entities take one model each, in document order, matching `model_of`.
pub fn brush_entities(doc: &MapDoc) -> Vec<(&Object, &Entity)> {
    let by_id: HashMap<Id, &Object> = doc.objects.iter().map(|o| (o.id, o)).collect();
    doc.objects
        .iter()
        .filter(|o| {
            matches!(&o.kind, ObjectKind::Entity(e) if e.classname != "worldspawn") && has_brushes(&by_id, o.id)
        })
        .map(|o| match &o.kind {
            ObjectKind::Entity(e) => (o, e),
            _ => unreachable!("filtered to entities"),
        })
        .collect()
}
