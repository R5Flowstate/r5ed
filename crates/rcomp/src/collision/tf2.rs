//! Titanfall 2 (rBSP v37) collision, turned into the Apex BVH4 lumps.
//!
//! Primitive mapping: a CM brush becomes a convex hull leaf with faces from the
//! brush planes, a tricoll becomes float-vertex poly leaves, and a prop becomes
//! a static-prop reference. Contents come straight from `CM_UNIQUE_CONTENTS`.

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use glam::DVec3;
use rbsp::lumps::tf2::{
    CmBounds, CmBrush, CmGeoSet, CmGrid, CmGridCell, CmPrimitive, CmSidePlaneOffset, CmSideProp,
    TricollHeader, Tf2Model, Tf2TexData, PRIM_BRUSH, PRIM_PROP, PRIM_TRICOLL,
};
use rbsp::{ids, Bsp, Version};
use rir::ModelSlot;
use thiserror::Error;

use super::{BvhWriter, CollError, ModelFields, COLL_CHUNK};

/// Surface types the S21 table does not define, mapped to the closest one.
pub const SURFPROP_ALIASES: [(&str, &str); 1] = [("cloth", "carpet")];

#[derive(Debug, Error)]
pub enum Tf2Error {
    #[error("the map is rBSP v{0}, which is not a v37 read")]
    Version(u16),
    #[error("lump 0x{id:02x} of the v37 map is {reason}")]
    Lump { id: u16, reason: String },
    #[error("brush {0} is degenerate")]
    Brush(usize),
    #[error("brush {0} is primitive type {1}, which has no Apex form")]
    PrimitiveKind(usize, u8),
    #[error("collision: {0}")]
    Coll(#[from] CollError),
    #[error("{0}")]
    Io(String),
}

/// One primitive, tagged with the model it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct Prim {
    pub kind: u8,
    pub index: u32,
    pub contents_idx: u8,
    pub bounds: (DVec3, DVec3),
    pub model: u32,
}

/// A brush: a convex volume with per-side surfaces.
#[derive(Clone, Debug, PartialEq)]
pub struct Brush {
    /// Outward planes, `n.x <= d` inside.
    pub planes: Vec<(DVec3, f64)>,
    pub side_texdata: Vec<u32>,
    pub side_flags: Vec<u32>,
}

/// The v37 map's collision.
pub struct Tf2Collision {
    bsp: Bsp,
    pub planes: Vec<(DVec3, f64)>,
    pub verts: Vec<DVec3>,
    pub models: Vec<(DVec3, DVec3)>,
    pub texdata: Vec<Tf2TexData>,
    pub texnames: Vec<String>,
    /// 0x2C: one byte offset into the name blob per texdata record.
    pub string_table: Vec<u32>,
    pub grid: CmGrid,
    pub cells: Vec<CmGridCell>,
    pub geosets: Vec<CmGeoSet>,
    pub geoset_bounds: Vec<CmBounds>,
    pub prims: Vec<CmPrimitive>,
    pub prim_bounds: Vec<CmBounds>,
    pub unique_contents: Vec<u32>,
    pub brushes: Vec<CmBrush>,
    pub side_plane_ofs: Vec<CmSidePlaneOffset>,
    pub side_props: Vec<CmSideProp>,
    pub tri_hdrs: Vec<TricollHeader>,
    tri_tris: Vec<u32>,
}

impl Tf2Collision {
    /// Reads the collision side of a v37 map.
    pub fn open(bsp: Bsp) -> Result<Self, Tf2Error> {
        if bsp.header.version != 37 {
            return Err(Tf2Error::Version(bsp.header.version));
        }
        let v = Version::Tf2V37;
        let get = |id: u16| bsp.lump(id).to_vec();
        let planes_raw = get(ids::PLANES);
        if planes_raw.len() % 16 != 0 {
            return Err(Tf2Error::Lump { id: ids::PLANES, reason: "size is not a multiple of 16".into() });
        }
        let planes: Vec<(DVec3, f64)> = planes_raw
            .chunks_exact(16)
            .map(|c| {
                (
                    DVec3::new(f32::from_le_bytes(c[0..4].try_into().unwrap()) as f64,
                               f32::from_le_bytes(c[4..8].try_into().unwrap()) as f64,
                               f32::from_le_bytes(c[8..12].try_into().unwrap()) as f64),
                    f32::from_le_bytes(c[12..16].try_into().unwrap()) as f64,
                )
            })
            .collect();
        let verts: Vec<DVec3> = get(ids::VERTICES)
            .chunks_exact(12)
            .map(|c| {
                DVec3::new(
                    f32::from_le_bytes(c[0..4].try_into().unwrap()) as f64,
                    f32::from_le_bytes(c[4..8].try_into().unwrap()) as f64,
                    f32::from_le_bytes(c[8..12].try_into().unwrap()) as f64,
                )
            })
            .collect();
        let models_raw = get(ids::MODELS);
        let models: Vec<(DVec3, DVec3)> = models_raw
            .chunks_exact(32)
            .map(|c| {
                let rd = |o: usize| f32::from_le_bytes(c[o..o + 4].try_into().unwrap()) as f64;
                (DVec3::new(rd(0), rd(4), rd(8)), DVec3::new(rd(12), rd(16), rd(20)))
            })
            .collect();
        let texdata: Vec<Tf2TexData> =
            rbsp::lumps::tf2::parse_texdata(&get(ids::TEXTURE_DATA), v).map_err(|e| Tf2Error::Lump {
                id: ids::TEXTURE_DATA,
                reason: e.to_string(),
            })?;
        let string_table: Vec<u32> = rbsp::lumps::tf2::parse_string_table(&get(ids::TEXDATA_STRING_TABLE), v)
            .map_err(|e| Tf2Error::Lump { id: ids::TEXDATA_STRING_TABLE, reason: e.to_string() })?;
        let table: Vec<u32> = string_table.clone();
        let names_raw = get(ids::CUBEMAPS_AMBIENT_RCP);
        let texnames: Vec<String> = table
            .iter()
            .map(|&o| {
                let start = o as usize;
                if start >= names_raw.len() {
                    return String::new();
                }
                let end = names_raw[start..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|i| start + i)
                    .unwrap_or(names_raw.len());
                String::from_utf8_lossy(&names_raw[start..end]).into_owned()
            })
            .collect();
        let one = |id: u16| -> Result<CmGrid, Tf2Error> {
            let raw = bsp.lump(id);
            rbsp::lumps::tf2::parse_cm_grid(raw, v)
                .map_err(|e| Tf2Error::Lump { id, reason: e.to_string() })?
                .into_iter()
                .next()
                .ok_or(Tf2Error::Lump { id, reason: "lump is empty".into() })
        };
        let grid = one(ids::CM_GRID)?;
        let cells = rbsp::lumps::tf2::parse_cm_grid_cells(&get(ids::CM_GRID_CELLS), v).map_err(|e| Tf2Error::Lump {
            id: ids::CM_GRID_CELLS,
            reason: e.to_string(),
        })?;
        let geosets = rbsp::lumps::tf2::parse_cm_geo_sets(&get(ids::CM_GEO_SETS), v).map_err(|e| Tf2Error::Lump {
            id: ids::CM_GEO_SETS,
            reason: e.to_string(),
        })?;
        let geoset_bounds =
            rbsp::lumps::tf2::parse_cm_geo_set_bounds(&get(ids::CM_GEO_SET_BOUNDS), v).map_err(|e| Tf2Error::Lump {
                id: ids::CM_GEO_SET_BOUNDS,
                reason: e.to_string(),
            })?;
        let prims = rbsp::lumps::tf2::parse_cm_primitives(&get(ids::CM_PRIMITIVES), v).map_err(|e| Tf2Error::Lump {
            id: ids::CM_PRIMITIVES,
            reason: e.to_string(),
        })?;
        let prim_bounds =
            rbsp::lumps::tf2::parse_cm_primitive_bounds(&get(ids::CM_PRIMITIVE_BOUNDS), v).map_err(|e| Tf2Error::Lump {
                id: ids::CM_PRIMITIVE_BOUNDS,
                reason: e.to_string(),
            })?;
        let unique_contents = rbsp::lumps::tf2::parse_cm_unique_contents(&get(ids::CM_UNIQUE_CONTENTS), v)
            .map_err(|e| Tf2Error::Lump { id: ids::CM_UNIQUE_CONTENTS, reason: e.to_string() })?;
        let brushes = rbsp::lumps::tf2::parse_cm_brushes(&get(ids::CM_BRUSHES), v).map_err(|e| Tf2Error::Lump {
            id: ids::CM_BRUSHES,
            reason: e.to_string(),
        })?;
        let side_plane_ofs = rbsp::lumps::tf2::parse_cm_side_plane_offsets(&get(ids::CM_BRUSH_SIDE_PLANE_OFFSETS), v)
            .map_err(|e| Tf2Error::Lump { id: ids::CM_BRUSH_SIDE_PLANE_OFFSETS, reason: e.to_string() })?;
        let side_props = rbsp::lumps::tf2::parse_cm_side_props(&get(ids::CM_BRUSH_SIDE_PROPERTIES), v)
            .map_err(|e| Tf2Error::Lump { id: ids::CM_BRUSH_SIDE_PROPERTIES, reason: e.to_string() })?;
        let tri_hdrs = rbsp::lumps::tf2::parse_tricoll_headers(&get(ids::TRICOLL_HEADERS), v).map_err(|e| Tf2Error::Lump {
            id: ids::TRICOLL_HEADERS,
            reason: e.to_string(),
        })?;
        let tri_tris: Vec<u32> = get(ids::TRICOLL_TRIANGLES)
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        Ok(Self {
            bsp,
            planes,
            verts,
            models,
            texdata,
            texnames,
            string_table,
            grid,
            cells,
            geosets,
            geoset_bounds,
            prims,
            prim_bounds,
            unique_contents: unique_contents.into_iter().map(|c| c.mask).collect(),
            brushes,
            side_plane_ofs,
            side_props,
            tri_hdrs,
            tri_tris,
        })
    }

    pub fn bsp(&self) -> &Bsp {
        &self.bsp
    }

    pub fn texdata_name(&self, i: u32) -> &str {
        match self.texdata.get(i as usize) {
            Some(t) => self.texnames.get(t.name as usize).map(|s| s.as_str()).unwrap_or(""),
            None => "",
        }
    }

    fn bounds_of(&self, rec: &CmBounds) -> (DVec3, DVec3) {
        let (a, b) = rec.aabb();
        (
            DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64),
            DVec3::new(b[0] as f64, b[1] as f64, b[2] as f64),
        )
    }

    /// Worldspawn owns the first `cx*cy` cells, then one extra empty cell, then
    /// exactly one cell per brush model.
    fn model_cells(&self) -> Vec<Vec<usize>> {
        let n = (self.grid.cells_x as i64).max(0) * (self.grid.cells_y as i64).max(0);
        let n = n.max(0) as usize;
        let mut out: Vec<Vec<usize>> = vec![(0..=n).collect()];
        for m in 1..self.models.len() {
            out.push(vec![n + m]);
        }
        out
    }

    /// Every primitive once, tagged with its model.
    pub fn primitives(&self) -> Vec<Prim> {
        let mut seen: Vec<(u32, u8, u32, u8)> = Vec::new();
        let mut out: Vec<Prim> = Vec::new();
        for (model, cells) in self.model_cells().into_iter().enumerate() {
            for ci in cells {
                let Some(c) = self.cells.get(ci) else { continue };
                for gi in c.first_geoset as usize..(c.first_geoset as usize + c.geoset_count as usize) {
                    let Some(gs) = self.geosets.get(gi) else { continue };
                    if gs.count == 1 {
                        let key = (model as u32, gs.primitive_kind(), gs.primitive_index(), gs.contents_index());
                        if seen.contains(&key) {
                            continue;
                        }
                        seen.push(key);
                        let rec = self.geoset_bounds.get(gi);
                        out.push(Prim {
                            kind: gs.primitive_kind(),
                            index: gs.primitive_index(),
                            contents_idx: gs.contents_index(),
                            bounds: rec.map(|r| self.bounds_of(r)).unwrap_or((DVec3::ZERO, DVec3::ZERO)),
                            model: model as u32,
                        });
                    } else {
                        let first = gs.primitive_index() as usize;
                        for pi in first..first + gs.count as usize {
                            let Some(p) = self.prims.get(pi) else { continue };
                            let key = (model as u32, p.kind(), p.index(), p.contents_index());
                            if seen.contains(&key) {
                                continue;
                            }
                            seen.push(key);
                            let rec = self.prim_bounds.get(pi);
                            out.push(Prim {
                                kind: p.kind(),
                                index: p.index(),
                                contents_idx: p.contents_index(),
                                bounds: rec.map(|r| self.bounds_of(r)).unwrap_or((DVec3::ZERO, DVec3::ZERO)),
                                model: model as u32,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// The planes and per-side surfaces of one CM brush.
    pub fn brush_hull(&self, bi: u32) -> Result<Brush, Tf2Error> {
        let br = self.brushes.get(bi as usize).ok_or(Tf2Error::Brush(bi as usize))?;
        let o = DVec3::new(br.origin[0] as f64, br.origin[1] as f64, br.origin[2] as f64);
        let e = DVec3::new(br.extents[0] as f64, br.extents[1] as f64, br.extents[2] as f64);
        let mut planes: Vec<(DVec3, f64)> = vec![
            (DVec3::X, o.x + e.x),
            (-DVec3::X, -(o.x - e.x)),
            (DVec3::Y, o.y + e.y),
            (-DVec3::Y, -(o.y - e.y)),
            (DVec3::Z, o.z + e.z),
            (-DVec3::Z, -(o.z - e.z)),
        ];
        for i in 0..br.num_planes as usize {
            let off = br.side_offset as usize + i;
            let rel = self.side_plane_ofs.get(off).map(|s| s.value as usize).unwrap_or(0);
            let pidx = (self.grid.first_brush_plane as usize).wrapping_add(off).wrapping_sub(rel);
            let Some(p) = self.planes.get(pidx) else { continue };
            planes.push(*p);
        }
        let start = 6 * br.index as usize + br.side_offset as usize;
        let mut tex = Vec::with_capacity(planes.len());
        let mut flags = Vec::with_capacity(planes.len());
        for (i, _) in planes.iter().enumerate() {
            let p = self.side_props.get(start + i);
            tex.push(p.map(|s| s.tex_data()).unwrap_or(0));
            flags.push(p.map(|s| s.flags()).unwrap_or(0));
        }
        Ok(Brush { planes, side_texdata: tex, side_flags: flags })
    }

    /// The triangle indices of one tricoll mesh.
    pub fn tricoll_triangles(&self, hi: u32) -> (Vec<[u32; 3]>, Vec<u8>) {
        let Some(h) = self.tri_hdrs.get(hi as usize) else { return (Vec::new(), Vec::new()) };
        let mut out = Vec::with_capacity(h.triangle_count.max(0) as usize);
        let mut fl = Vec::with_capacity(h.triangle_count.max(0) as usize);
        for t in 0..h.triangle_count.max(0) as usize {
            let at = h.first_triangle as usize + t;
            let Some(&v) = self.tri_tris.get(at) else { break };
            let a = (v & 0x3FF) as i32;
            let b = ((v >> 10) & 0x7F) as i32;
            let c = ((v >> 17) & 0x7F) as i32;
            let fv = h.first_vertex;
            out.push([(fv + a) as u32, (fv + a + b) as u32, (fv + a + c) as u32]);
            fl.push(((v >> 24) & 0xFF) as u8);
        }
        (out, fl)
    }
}

/// Corner points and out-of-plane loops of the volume `{x : n.x <= d}`.
pub fn hull_from_planes(planes: &[(DVec3, f64)], eps: f64) -> (Vec<DVec3>, Vec<(Vec<usize>, usize)>) {
    let n = planes.len();
    let mut pts: Vec<DVec3> = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            for k in (j + 1)..n {
                let m = [
                    planes[i].0,
                    planes[j].0,
                    planes[k].0,
                ];
                let det = m[0].dot(m[1].cross(m[2]));
                if det.abs() < 1e-9 {
                    continue;
                }
                let d = DVec3::new(planes[i].1, planes[j].1, planes[k].1);
                let x = (m[1].cross(m[2]) * d.x + m[2].cross(m[0]) * d.y + m[0].cross(m[1]) * d.z) / det;
                if !x.is_finite() {
                    continue;
                }
                if planes.iter().all(|p| p.0.dot(x) - p.1 <= eps) {
                    pts.push(x);
                }
            }
        }
    }
    if pts.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut verts: Vec<DVec3> = Vec::with_capacity(pts.len());
    for p in pts {
        if verts.iter().any(|q| (*q - p).abs().max_element() < eps * 10.0) {
            continue;
        }
        verts.push(p);
    }
    let mut faces = Vec::new();
    for (pi, p) in planes.iter().enumerate() {
        let ring: Vec<usize> = (0..verts.len()).filter(|&i| (p.0.dot(verts[i]) - p.1).abs() < eps * 10.0).collect();
        if ring.len() < 3 {
            continue;
        }
        let c = ring.iter().map(|&i| verts[i]).sum::<DVec3>() / ring.len() as f64;
        let u = (verts[ring[0]] - c).normalize();
        let w = p.0.cross(u);
        let mut keyed: Vec<(f64, usize)> = ring
            .iter()
            .map(|&i| {
                let d = verts[i] - c;
                (d.dot(w).atan2(d.dot(u)), i)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        faces.push((keyed.into_iter().map(|(_, i)| i).collect(), pi));
    }
    (verts, faces)
}

/// The contents mask, surface type and name a class collides with, taken from
/// the surface properties table.
#[derive(Clone, Debug, Default)]
pub struct SurfaceTables {
    /// Name to surface property id.
    pub surf_ids: HashMap<String, u32>,
    /// Material name to surface property name.
    pub material_surfprop: HashMap<String, String>,
}

/// Reads a surface property table: a block name, a brace, then the keys, one
/// of which is `id`.
pub fn load_surfprop_ids(text: &str) -> HashMap<String, u32> {
    let mut out = HashMap::new();
    let mut pending: Option<String> = None;
    let mut name: Option<String> = None;
    let mut id: Option<u32> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(head) = t.strip_suffix('{') {
            let head = head.trim().trim_end_matches(':').trim();
            name = if is_identifier(head) { Some(head.to_ascii_lowercase()) } else { pending.take() };
            id = None;
            continue;
        }
        if t.starts_with('}') {
            if let (Some(n), Some(v)) = (name.take(), id.take()) {
                out.entry(n).or_insert(v);
            }
            pending = None;
            continue;
        }
        if let Some(v) = t.strip_suffix(':') {
            let head = v.trim();
            if is_identifier(head) {
                pending = Some(head.to_ascii_lowercase());
            }
            continue;
        }
        if let Some(v) = t.strip_prefix("id") {
            if let Some(v) = v.trim_start().strip_prefix(':') {
                if let Ok(n) = v.trim().parse::<u32>() {
                    id = Some(n);
                }
            }
        }
    }
    out
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Reads a material table of `{"name": ..., "surfaceProp": ...}` records.
pub fn load_material_surfaceprops(text: &str) -> HashMap<String, String> {    let mut out = HashMap::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else { return out };
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        match v {
            serde_json::Value::Object(map) => {
                let name = map.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let sp = map.get("surfaceProp").and_then(|n| n.as_str()).unwrap_or("");
                if !name.is_empty() && !sp.is_empty() {
                    out.entry(name.to_ascii_lowercase().replace('\\', "/")).or_insert_with(|| sp.to_string());
                }
                for c in map.into_values() {
                    stack.push(c);
                }
            }
            serde_json::Value::Array(a) => stack.extend(a),
            _ => {}
        }
    }
    out
}

/// Reads `$surfaceprop` out of a VMT, keyed by the material's path relative to
/// the materials root with forward slashes. The world brushes and tools that no
/// exported material json carries are only reachable this way.
pub fn load_vmt_surfaceprops(text: &str, name: &str, out: &mut HashMap<String, String>) {
    let lower = text.to_ascii_lowercase();
    let Some(at) = lower.find("\"$surfaceprop\"") else { return };
    let rest = text[at + "\"$surfaceprop\"".len()..].trim_start();
    let Some(value) = rest.strip_prefix('"') else { return };
    let Some(end) = value.find('"') else { return };
    let value = &value[..end];
    if value.is_empty() {
        return;
    }
    out.insert(name.to_string(), value.to_string());
}

/// Reads every material json under a directory, in sorted path order. The
/// browser build has no filesystem, so the walk is native-only.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_material_dir(dir: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        collect_json(&e.path(), &mut files);
    }
    files.sort();
    for f in files {
        if let Ok(text) = std::fs::read_to_string(&f) {
            for (k, v) in load_material_surfaceprops(&text) {
                out.entry(k).or_insert(v);
            }
        }
    }
    out
}

/// Every `.vmt` under a materials root, keyed as the material path. The value
/// is the surface property as the vmt spells it.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_vmt_dir(dir: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        collect_vmt(&e.path(), &mut files);
    }
    files.sort();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let Ok(rel) = f.strip_prefix(dir) else { continue };
        let name = rel.to_string_lossy().replace('\\', "/");
        let Some(stem) = name.strip_suffix(".vmt") else { continue };
        load_vmt_surfaceprops(&text, &stem.to_ascii_lowercase(), &mut out);
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn collect_vmt(path: &Path, out: &mut Vec<std::path::PathBuf>) {
    if path.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            let mut kids: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).collect();
            kids.sort();
            for k in kids {
                collect_vmt(&k, out);
            }
        }
    } else if path.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("vmt")) {
        out.push(path.to_path_buf());
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn collect_json(path: &Path, out: &mut Vec<std::path::PathBuf>) {
    if path.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            let mut kids: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).collect();
            kids.sort();
            for k in kids {
                collect_json(&k, out);
            }
        }
    } else if path.extension().is_some_and(|e| e == "json") {
        out.push(path.to_path_buf());
    }
}

/// Statistics the conversion reports.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ConvStats {
    pub hulls: u32,
    pub hull_faces: u32,
    pub tris: u32,
    pub tris_degenerate: u32,
    pub props: u32,
}

/// The v37 collision converted into the Apex BVH4 lumps.
pub struct Converter {
    tf2: Tf2Collision,
    writer: BvhWriter,
    surf_ids: HashMap<String, u32>,
    material_surfprop: HashMap<String, String>,
    name_ofs: Vec<u32>,
    pub unknown_materials: Vec<String>,
    pub unknown_surfprops: Vec<String>,
    pub aliased_surfprops: Vec<String>,
    pub stats: ConvStats,
}

impl Converter {
    pub fn new(tf2: Tf2Collision, tables: &SurfaceTables) -> Self {
        let mut seeds: Vec<u32> = tf2.unique_contents.clone();
        if seeds.is_empty() {
            seeds.push(0);
        }
        // A surface property names its material by a byte offset into the 0x2B
        // name blob, not by the texdata record's own name index.
        let name_ofs: Vec<u32> = tf2
            .texdata
            .iter()
            .map(|t| {
                let i = t.name as usize;
                tf2.string_table.get(i).copied().unwrap_or(0)
            })
            .collect();
        Self {
            writer: BvhWriter::new(seeds),
            tf2,
            surf_ids: tables.surf_ids.clone(),
            material_surfprop: tables.material_surfprop.clone(),
            name_ofs,
            unknown_materials: Vec::new(),
            unknown_surfprops: Vec::new(),
            aliased_surfprops: Vec::new(),
            stats: ConvStats::default(),
        }
    }

    pub fn writer(&self) -> &BvhWriter {
        &self.writer
    }

    pub fn into_writer(self) -> BvhWriter {
        self.writer
    }

    /// The surface type of a texdata record, 0 when it is unknown.
    pub fn surf_type(&mut self, texdata: u32) -> u32 {
        let name = self.tf2.texdata_name(texdata).to_ascii_lowercase().replace('\\', "/");
        let Some(sp) = self.material_surfprop.get(&name).cloned() else {
            if !self.unknown_materials.contains(&name) {
                self.unknown_materials.push(name);
            }
            return 0;
        };
        let lower = sp.to_ascii_lowercase();
        if let Some(id) = self.surf_ids.get(&lower) {
            return *id;
        }
        if let Some((_, to)) = SURFPROP_ALIASES.iter().find(|(from, _)| *from == lower) {
            if let Some(id) = self.surf_ids.get(*to) {
                if !self.aliased_surfprops.contains(&sp) {
                    self.aliased_surfprops.push(sp);
                }
                return *id;
            }
        }
        if !self.unknown_surfprops.contains(&sp) {
            self.unknown_surfprops.push(sp);
        }
        0
    }

    pub fn surfprop(&mut self, texdata: u32, contents_idx: u8) -> Result<u16, Tf2Error> {
        let flags = self.tf2.texdata.get(texdata as usize).map(|t| t.flags as u16).unwrap_or(0);
        let st = self.surf_type(texdata);
        let name = self.name_ofs.get(texdata as usize).copied().unwrap_or(0);
        Ok(self.writer.surfprop_index(flags, st as u8, contents_idx, name)?)
    }

    /// One hull item from a CM brush.
    pub fn brush_item(&mut self, prim: &Prim) -> Result<super::Item, Tf2Error> {
        let hull = self.tf2.brush_hull(prim.index)?;
        let (verts, _faces) = hull_from_planes(&hull.planes, 1e-3);
        if verts.len() < 4 {
            return Err(Tf2Error::Brush(prim.index as usize));
        }
        let normals: Vec<DVec3> = hull.planes.iter().map(|p| p.0).collect();
        let surfprops: Vec<u16> = hull
            .side_texdata
            .iter()
            .map(|&t| self.surfprop(t, prim.contents_idx))
            .collect::<Result<Vec<_>, _>>()?;
        self.stats.hulls += 1;
        self.stats.hull_faces += surfprops.len() as u32;
        Ok(self.writer.hull_item(prim.contents_idx, &verts, &normals, &surfprops)?)
    }

    /// One poly item from a tricoll mesh. TF2 winds clockwise and the engine
    /// wants counter-clockwise, so the last two corners trade places; the three
    /// components of each corner are already in engine order.
    pub fn tricoll_item(&mut self, prim: &Prim) -> Result<Option<super::Item>, Tf2Error> {
        let (idx, _fl) = self.tf2.tricoll_triangles(prim.index);
        let mut tris: Vec<[DVec3; 3]> = Vec::with_capacity(idx.len());
        let mut degenerate = 0u32;
        for t in idx {
            let (Some(a), Some(b), Some(c)) =
                (self.tf2.verts.get(t[0] as usize), self.tf2.verts.get(t[1] as usize), self.tf2.verts.get(t[2] as usize))
            else {
                continue;
            };
            let p = [*a, *c, *b];
            let area = (p[1] - p[0]).cross(p[2] - p[0]).length();
            if area <= 1e-6 {
                degenerate += 1;
                continue;
            }
            tris.push(p);
        }
        self.stats.tris_degenerate += degenerate;
        self.stats.tris += tris.len() as u32;
        if tris.is_empty() {
            return Ok(None);
        }
        let texdata = self.tf2.tri_hdrs.get(prim.index as usize).map(|h| h.tex_data.max(0) as u32).unwrap_or(0);
        let sp = self.surfprop(texdata, prim.contents_idx)?;
        Ok(Some(self.writer.tri_item(prim.contents_idx, sp, tris)))
    }

    /// One static-prop reference item.
    pub fn prop_item(&mut self, prim: &Prim) -> Result<super::Item, Tf2Error> {
        self.stats.props += 1;
        Ok(self.writer.prop_item(prim.contents_idx, prim.index, prim.bounds.0, prim.bounds.1))
    }

    /// Every model's `MODELS` collision fields, in model order.
    pub fn run(&mut self) -> Result<Vec<ModelFields>, Tf2Error> {
        let mut per_model: Vec<Vec<super::Item>> = vec![Vec::new(); self.tf2.models.len()];
        for prim in self.tf2.primitives() {
            let item = match prim.kind {
                PRIM_BRUSH => Some(self.brush_item(&prim)?),
                PRIM_TRICOLL => self.tricoll_item(&prim)?,
                PRIM_PROP => Some(self.prop_item(&prim)?),
                k => return Err(Tf2Error::PrimitiveKind(prim.index as usize, k)),
            };
            if let Some(it) = item {
                if let Some(slot) = per_model.get_mut(prim.model as usize) {
                    slot.push(it);
                }
            }
        }
        let mut fields = Vec::with_capacity(per_model.len());
        for (m, mut items) in per_model.into_iter().enumerate() {
            let (base_mins, base_maxs) = self.tf2.models[m];
            let mut lo = base_mins;
            let mut hi = base_maxs;
            for it in &items {
                lo = lo.min(it.mins);
                hi = hi.max(it.maxs);
            }
            let origin = ((lo + hi) * 0.5).round();
            let step = super::decode_step_for(lo, hi)?;
            fields.push(self.writer.build_model(std::mem::take(&mut items), origin, step)?);
        }
        Ok(fields)
    }
}

/// The v37 map's own models as collision slots, for the caller that keeps the
/// render models and adds collision to them.
pub fn tf2_model_slots(tf2: &Tf2Collision) -> Vec<ModelSlot> {
    tf2.models
        .iter()
        .enumerate()
        .map(|(i, (lo, hi))| ModelSlot {
            index: i as u32,
            owner: None,
            mins: lo.as_vec3(),
            maxs: hi.as_vec3(),
            has_render: true,
        })
        .collect()
}

/// Patches a v37 `MODELS` lump with the converted collision fields, keeping the
/// render range the source map declared.
pub fn patch_models(raw: &[u8], fields: &[ModelFields], vert_offset: i32) -> Result<Vec<u8>, Tf2Error> {
    let n = raw.len() / 64;
    if n != fields.len() {
        return Err(Tf2Error::Io(format!("model count mismatch: lump {n}, collision {}", fields.len())));
    }
    let mut out = raw.to_vec();
    for (i, f) in fields.iter().enumerate() {
        let base = i * 64 + 32;
        let words: [u32; 4] = [
            f.bvh_node as u32,
            f.bvh_leaf as u32,
            (f.vert_index + vert_offset) as u32,
            f.flags,
        ];
        out[base..base + 16].copy_from_slice(bytemuck::cast_slice(&words));
        let tail: [f32; 4] = [f.origin[0], f.origin[1], f.origin[2], f.decode_scale];
        out[base + 16..base + 32].copy_from_slice(bytemuck::cast_slice(&tail));
    }
    Ok(out)
}

/// A v37 map's `MODELS` lump read as the 64-byte records the v47 layout needs.
pub fn widen_models(raw: &[u8]) -> Vec<Tf2Model> {
    rbsp::lumps::tf2::parse_models(raw, Version::Tf2V37).unwrap_or_default()
}

/// The base64 chunk size a `*coll` key carries, re-exported for the caller.
pub const TF2_COLL_CHUNK: usize = COLL_CHUNK;
