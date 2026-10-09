//! Valve `.vmf` import: brushes, point entities, brush entities and displacements.

use glam::{DQuat, DVec3};
use indexmap::IndexMap;

use crate::brush::{project_uv, tile_uv, Brush, BrushFace, BrushError, Plane, Solid, UvMapping, PLANE_EPS};
use crate::command::Command;
use crate::doc::{new_object, Document};
use crate::model::{
    CollisionMode, Entity, Id, KeyValues, Light, LightKind, Mesh, Object, ObjectKind, Prop, Transform, Volume,
    VolumeKind,
};

use super::glb::{self, GlbMesh};
use super::source::{self, Role};
use super::tokens::{tokenize, Cursor, Tok};
use super::{entity_name, mesh_path, ImportError};

/// A `.vmf` block: an optional name, `"key" "value"` pairs and nested blocks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VBlock {
    pub name: Option<String>,
    pub props: IndexMap<String, String>,
    /// Repeated keys in file order; `props` keeps the first. Connections
    /// blocks repeat the output name once per connection.
    pub dupes: Vec<(String, String)>,
    pub children: Vec<VBlock>,
}

impl VBlock {
    pub fn prop(&self, key: &str) -> Option<&str> {
        self.props.get(key).map(String::as_str)
    }

    pub fn named(&self, name: &str) -> impl Iterator<Item = &VBlock> {
        let want = name.to_string();
        self.children.iter().filter(move |c| c.name.as_deref() == Some(want.as_str()))
    }
}

/// Parses a `.vmf` document into its top-level blocks.
pub fn parse(text: &str) -> Result<Vec<VBlock>, ImportError> {
    let mut cursor = Cursor::new(tokenize(text)?);
    let mut out = Vec::new();
    while cursor.peek().is_some() {
        let name = match cursor.peek() {
            Some(Tok::Ident(_)) => match cursor.take()? {
                Tok::Ident(s) => Some(s),
                other => return Err(ImportError::Unexpected(other.describe())),
            },
            _ => None,
        };
        out.push(parse_block(&mut cursor, name)?);
    }
    Ok(out)
}

fn parse_block(cursor: &mut Cursor, name: Option<String>) -> Result<VBlock, ImportError> {
    cursor.expect(&Tok::LBrace)?;
    let mut block = VBlock { name, ..Default::default() };
    loop {
        match cursor.take()? {
            Tok::RBrace => return Ok(block),
            Tok::Str(key) => match cursor.take()? {
                Tok::Str(value) => {
                    if block.props.contains_key(&key) {
                        block.dupes.push((key, value));
                    } else {
                        block.props.insert(key, value);
                    }
                }
                other => return Err(ImportError::Unexpected(other.describe())),
            },
            Tok::Ident(child) => block.children.push(parse_block(cursor, Some(child))?),
            other => return Err(ImportError::Unexpected(other.describe())),
        }
    }
}

/// Plane from the three points a side stores, oriented outwards.
///
/// `cross(p1 - p0, p2 - p0)` is the winding the sibling `.map` format uses, and
/// `solid_of` retries with every plane flipped so a brush written the other way
/// round still imports.
pub fn plane_from_points(points: [DVec3; 3]) -> Result<Plane, ImportError> {
    let normal = (points[1] - points[0]).cross(points[2] - points[0]);
    if normal.length_squared() <= PLANE_EPS * PLANE_EPS {
        return Err(ImportError::NotASolid("a side's three plane points are collinear".into()));
    }
    let normal = normal.normalize();
    Ok(Plane { normal, dist: normal.dot(points[0]) })
}

fn parse_points(text: &str) -> Result<[DVec3; 3], ImportError> {
    let mut cursor = Cursor::new(tokenize(text)?);
    let mut out = [DVec3::ZERO; 3];
    for slot in out.iter_mut() {
        let v = cursor.vector(&Tok::LParen, &Tok::RParen)?;
        if v.len() != 3 {
            return Err(ImportError::Unsupported(format!("plane point has {} components", v.len())));
        }
        *slot = DVec3::new(v[0], v[1], v[2]);
    }
    Ok(out)
}

/// A vector keyvalue, written bare (`"origin" "0 0 256"`), in parentheses (a
/// displacement's `startPos` / `DIP_` keys) or in brackets (its
/// `startposition`). A trailing alpha component is ignored.
fn parse_vec3(text: &str) -> Result<DVec3, ImportError> {
    let mut cursor = Cursor::new(tokenize(text)?);
    let mut v = Vec::with_capacity(4);
    while v.len() < 4 {
        match cursor.peek() {
            Some(Tok::LParen) => {
                v.append(&mut cursor.vector(&Tok::LParen, &Tok::RParen)?);
            }
            Some(Tok::LBracket) => {
                v.append(&mut cursor.vector(&Tok::LBracket, &Tok::RBracket)?);
            }
            Some(Tok::Num(n)) => {
                v.push(*n);
                cursor.pos += 1;
            }
            _ => break,
        }
    }
    if v.len() < 3 {
        return Err(ImportError::Unsupported(format!("vector has {} components", v.len())));
    }
    Ok(DVec3::new(v[0], v[1], v[2]))
}

/// `uaxis "[1 0 0 0] 0.25"` -> unit axis, offset and scale.
fn parse_axis(text: &str) -> Result<(DVec3, f64, f64), ImportError> {
    let mut cursor = Cursor::new(tokenize(text)?);
    let v = cursor.vector(&Tok::LBracket, &Tok::RBracket)?;
    if v.len() != 4 {
        return Err(ImportError::Unsupported(format!("texture axis has {} components", v.len())));
    }
    let axis = DVec3::new(v[0], v[1], v[2]);
    if axis.length_squared() <= PLANE_EPS * PLANE_EPS {
        return Err(ImportError::Unsupported("texture axis has no length".into()));
    }
    let scale = match cursor.take()? {
        Tok::Num(n) => n,
        other => return Err(ImportError::Unexpected(other.describe())),
    };
    if scale.abs() <= PLANE_EPS {
        return Err(ImportError::Unsupported("texture axis scale is zero".into()));
    }
    Ok((axis.normalize(), v[3], scale))
}

fn plane_basis(plane: &Plane) -> (DVec3, DVec3) {
    let seed = if plane.normal.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = plane.normal.cross(seed).normalize();
    (u, plane.normal.cross(u).normalize())
}

fn is_solid(block: &VBlock) -> bool {
    matches!(block.name.as_deref().map(str::to_ascii_lowercase).as_deref(), Some("solid" | "brush"))
}

/// Source UVs are texels of the face's own texture; r5ed's are texels of a
/// fixed tile, so the scale and offset are rescaled by the texture's size.
fn side_face(side: &VBlock, sizes: &IndexMap<String, [u32; 2]>) -> Result<BrushFace, ImportError> {
    let mut face = side_face_raw(side)?;
    let source_name = source::clean_name(side.prop("material").unwrap_or(""));
    let [w, h] = sizes.get(&source_name).copied().unwrap_or([512, 512]);
    let tile = crate::model::UV_TEXELS_PER_TILE;
    let (fw, fh) = (w.max(1) as f64 / tile, h.max(1) as f64 / tile);
    face.uv.scale = [face.uv.scale[0] * fw, face.uv.scale[1] * fh];
    face.uv.offset = [face.uv.offset[0] / fw, face.uv.offset[1] / fh];
    face.material = source::material(&source_name);
    Ok(face)
}

fn side_face_raw(side: &VBlock) -> Result<BrushFace, ImportError> {
    let plane_text = side.prop("plane").ok_or_else(|| ImportError::Unsupported("side has no plane".into()))?;
    let plane = plane_from_points(parse_points(plane_text)?)?;
    let (u_axis, u_offset, u_scale) = match side.prop("uaxis") {
        Some(t) => parse_axis(t)?,
        None => (plane_basis(&plane).0, 0.0, 0.25),
    };
    let (v_axis, v_offset, v_scale) = match side.prop("vaxis") {
        Some(t) => parse_axis(t)?,
        None => (plane_basis(&plane).1, 0.0, 0.25),
    };
    let rotation = match side.prop("rotation") {
        Some(t) => t.parse::<f64>().map_err(|_| ImportError::BadNumber(t.to_string()))?,
        None => 0.0,
    };
    let lightmap_scale = side.prop("lightmapscale").and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0);
    Ok(BrushFace {
        normal: plane.normal,
        dist: plane.dist,
        material: side.prop("material").unwrap_or("").to_string(),
        uv: UvMapping { u_axis, v_axis, offset: [u_offset, v_offset], scale: [u_scale, v_scale], rotation },
        lightmap_scale,
    })
}

/// Builds the solid and returns the faces with outward-facing normals, retrying
/// with every plane flipped when the brush is written the other way round.
pub fn orient_faces(faces: &[BrushFace]) -> Result<Vec<BrushFace>, ImportError> {
    if faces.len() < 4 {
        return Err(ImportError::NotASolid(format!("{} faces", faces.len())));
    }
    let planes: Vec<Plane> = faces.iter().map(Plane::from).collect();
    let solid = match Solid::from_planes(planes.clone()) {
        Ok(s) => s,
        Err(BrushError::NotConvex) => Solid::from_planes(planes.into_iter().map(Plane::flipped).collect())
            .map_err(|e| ImportError::NotASolid(e.to_string()))?,
        Err(e) => return Err(e.into()),
    };
    Ok(solid.to_faces(faces))
}

struct Displacement {
    name: String,
    start: DVec3,
    end: DVec3,
    grid: usize,
    points: Vec<DVec3>,
    u_axis: DVec3,
    v_axis: DVec3,
    u_scale: f64,
    v_scale: f64,
}

fn parse_displacement(block: &VBlock) -> Result<Displacement, ImportError> {
    let grid = block
        .prop("gridSize")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|g| (1..=256).contains(g))
        .ok_or_else(|| ImportError::Unsupported("displacement gridSize is missing or out of range".into()))?;
    let start = parse_vec3(block.prop("startPos").ok_or_else(|| ImportError::Unsupported("displacement has no startPos".into()))?)?;
    let end = parse_vec3(block.prop("endPos").ok_or_else(|| ImportError::Unsupported("displacement has no endPos".into()))?)?;
    let mut points: Vec<DVec3> = vec![DVec3::ZERO; (grid + 1) * (grid + 1)];
    let mut seen = 0usize;
    for (index, slot) in points.iter_mut().enumerate() {
        if let Some(text) = block.prop(&format!("DIP_{index:02}")) {
            *slot = parse_vec3(text)?;
            seen += 1;
        }
    }
    if seen == 0 {
        return Err(ImportError::Unsupported("displacement has no DIP_ points".into()));
    }
    let (u_axis, _, u_scale) = match block.prop("uaxis") {
        Some(t) => parse_axis(t)?,
        None => (DVec3::X, 0.0, 1.0),
    };
    let (v_axis, _, v_scale) = match block.prop("vaxis") {
        Some(t) => parse_axis(t)?,
        None => (DVec3::NEG_Y, 0.0, 1.0),
    };
    Ok(Displacement {
        name: block.prop("name").unwrap_or("displacement").to_string(),
        start,
        end,
        grid,
        points,
        u_axis,
        v_axis,
        u_scale,
        v_scale,
    })
}

fn displacement_mesh(d: &Displacement) -> GlbMesh {
    let n = d.grid;
    let span = d.end - d.start;
    let side = n + 1;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(side * side);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(side * side);
    for row in 0..=n {
        for col in 0..=n {
            let t = DVec3::new(col as f64 / n as f64, row as f64 / n as f64, 0.0);
            let p = d.start + span * t + d.points[row * side + col];
            positions.push([p.x as f32, p.y as f32, p.z as f32]);
            uvs.push([(p.dot(d.u_axis) / d.u_scale) as f32, (p.dot(d.v_axis) / d.v_scale) as f32]);
        }
    }
    let at = |row: usize, col: usize| (row * side + col) as u32;
    let mut indices = Vec::with_capacity(n * n * 6);
    for row in 0..n {
        for col in 0..n {
            indices.extend_from_slice(&[at(row, col), at(row, col + 1), at(row + 1, col + 1)]);
            indices.extend_from_slice(&[at(row, col), at(row + 1, col + 1), at(row + 1, col)]);
        }
    }
    let normals = grid_normals(&positions, n);
    GlbMesh { name: d.name.clone(), material: String::new(), positions, normals, uvs, colors: Vec::new(), indices }
}

fn grid_normals(positions: &[[f32; 3]], n: usize) -> Vec<[f32; 3]> {
    let side = n + 1;
    let at = |row: i64, col: i64| -> DVec3 {
        let r = row.clamp(0, n as i64) as usize;
        let c = col.clamp(0, n as i64) as usize;
        let p = positions[r * side + c];
        DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
    };
    let mut out = Vec::with_capacity(side * side);
    for row in 0..side as i64 {
        for col in 0..side as i64 {
            let du = at(row, col + 1) - at(row, col - 1);
            let dv = at(row + 1, col) - at(row - 1, col);
            let n = du.cross(dv);
            let n = if n.length_squared() > 0.0 { n.normalize() } else { DVec3::Z };
            out.push([n.x as f32, n.y as f32, n.z as f32]);
        }
    }
    out
}

/// A Hammer `dispinfo` block: the grid power, the corner it starts at, the
/// elevation along the face normal and one row block per vertex attribute.
struct DispInfo {
    power: u32,
    start: DVec3,
    elevation: f64,
    normals: Vec<DVec3>,
    distances: Vec<f64>,
    offsets: Vec<DVec3>,
    alphas: Vec<f32>,
}

fn disp_block<'b>(block: &'b VBlock, name: &str) -> Option<&'b VBlock> {
    block.children.iter().find(|c| c.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(name)))
}

/// One `row0..row{side-1}` per grid row, each exactly `width` finite numbers.
/// A row longer than 32 characters per number is not a row.
fn disp_rows(block: &VBlock, name: &str, side: usize, width: usize) -> Option<Vec<f64>> {
    let rows = disp_block(block, name)?;
    if rows.props.len() > 64 {
        return None;
    }
    let mut out = Vec::with_capacity(side * side * width);
    for i in 0..side {
        let key = format!("row{i}");
        let text = rows.props.iter().find(|(k, _)| k.eq_ignore_ascii_case(&key)).map(|(_, v)| v.as_str())?;
        if text.len() > side * width * 32 {
            return None;
        }
        let mut vals = Vec::with_capacity(width);
        for t in text.split_whitespace() {
            vals.push(t.parse::<f64>().ok().filter(|v| v.is_finite())?);
        }
        if vals.len() != width {
            return None;
        }
        out.extend(vals);
    }
    Some(out)
}

fn parse_dispinfo(block: &VBlock, face_normal: DVec3) -> Option<DispInfo> {
    let power: u32 = block.prop("power")?.trim().parse().ok()?;
    if !(1..=4).contains(&power) {
        return None;
    }
    let side = 2usize.pow(power) + 1;
    let start = parse_vec3(block.prop("startposition")?).ok()?;
    if !start.is_finite() {
        return None;
    }
    let elevation: f64 = block.prop("elevation").and_then(|t| t.trim().parse().ok()).unwrap_or(0.0);
    if !elevation.is_finite() {
        return None;
    }
    let normals = disp_rows(block, "normals", side, side * 3)
        .map(|v| v.chunks_exact(3).map(|c| DVec3::new(c[0], c[1], c[2])).collect())
        .unwrap_or_else(|| vec![face_normal; side * side]);
    let distances = disp_rows(block, "distances", side, side).unwrap_or_else(|| vec![0.0; side * side]);
    let offsets = disp_rows(block, "offsets", side, side * 3)
        .map(|v| v.chunks_exact(3).map(|c| DVec3::new(c[0], c[1], c[2])).collect())
        .unwrap_or_else(|| vec![DVec3::ZERO; side * side]);
    let alphas = disp_rows(block, "alphas", side, side)
        .map(|v| v.into_iter().map(|a| a as f32).collect())
        .unwrap_or_else(|| vec![0.0; side * side]);
    Some(DispInfo { power, start, elevation, normals, distances, offsets, alphas })
}

/// The displacement grid over its base quad. The quad corners start at the one
/// nearest `startposition`, U running to the next loop corner and V to the
/// previous one, so U x V is the outward normal and the triangle winding that
/// faces it stays the front face.
fn dispinfo_mesh(face: &BrushFace, source_material: &str, info: &DispInfo, corners: [DVec3; 4]) -> GlbMesh {
    let n = 2usize.pow(info.power);
    let side = n + 1;
    let mut first = 0;
    for (i, c) in corners.iter().enumerate().skip(1) {
        if (*c - info.start).length_squared() < (corners[first] - info.start).length_squared() {
            first = i;
        }
    }
    let (c0, cu, cv, cuv) = (corners[first], corners[(first + 1) % 4], corners[(first + 3) % 4], corners[(first + 2) % 4]);
    let mut positions = Vec::with_capacity(side * side);
    let mut normals = Vec::with_capacity(side * side);
    let mut uvs = Vec::with_capacity(side * side);
    let mut colors = Vec::with_capacity(side * side);
    for row in 0..=n {
        for col in 0..=n {
            let (s, t) = (col as f64 / n as f64, row as f64 / n as f64);
            let base = c0 * (1.0 - s) * (1.0 - t) + cu * s * (1.0 - t) + cv * (1.0 - s) * t + cuv * s * t;
            let k = row * side + col;
            let p = base + info.normals[k] * info.distances[k] + info.offsets[k] + face.normal * info.elevation;
            positions.push([p.x as f32, p.y as f32, p.z as f32]);
            let nv = info.normals[k];
            let nn = if nv.length_squared() > 0.0 { nv.normalize() } else { face.normal };
            normals.push([nn.x as f32, nn.y as f32, nn.z as f32]);
            let uv = tile_uv(project_uv(&face.uv, face.normal, p));
            uvs.push([uv[0] as f32, uv[1] as f32]);
            colors.push([255, 255, 255, (info.alphas[k].clamp(0.0, 1.0) * 255.0).round() as u8]);
        }
    }
    let at = |row: usize, col: usize| (row * side + col) as u32;
    let mut indices = Vec::with_capacity(n * n * 6);
    for row in 0..n {
        for col in 0..n {
            indices.extend_from_slice(&[at(row, col), at(row, col + 1), at(row + 1, col + 1)]);
            indices.extend_from_slice(&[at(row, col), at(row + 1, col + 1), at(row + 1, col)]);
        }
    }
    GlbMesh { name: String::new(), material: source_material.to_string(), positions, normals, uvs, colors, indices }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct VmfOptions {
    /// Source material path (lower case, no extension) -> texture size, for
    /// exact UVs; a material missing here is taken as 512x512.
    #[serde(default)]
    pub texture_sizes: IndexMap<String, [u32; 2]>,
    /// Source `.mdl` paths (lower case) that have an Apex model. When given, a
    /// prop naming any other model is left out and reported: the engine
    /// refuses a map that references a model it cannot load.
    #[serde(default)]
    pub models: Option<Vec<String>>,
    /// Classnames of `schema/entities.json`. When given, the Source I/O
    /// classes (`source::IO_ENTITIES`) import as passthrough only when named
    /// here and are dropped otherwise; when absent the historical roles apply.
    #[serde(default)]
    pub schema_classes: Option<Vec<String>>,
}

/// What the import kept, converted and left out.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct VmfReport {
    pub brushes: usize,
    pub volumes: usize,
    pub spawns: usize,
    pub props: usize,
    pub lights: usize,
    pub cubemaps: usize,
    pub entities: usize,
    /// Displaced sides imported as meshes.
    pub meshes: usize,
    /// Distinct `.mdl` paths the props reference, as Source paths.
    pub models: Vec<String>,
    /// Distinct Source material paths the brushes use.
    pub materials: Vec<String>,
    pub skyname: Option<String>,
    pub dropped_classes: IndexMap<String, usize>,
    pub dropped_keys: IndexMap<String, usize>,
    /// Repeated connections past the first per output name, as
    /// `{classname}.{output}`: the document holds one value per key.
    pub dropped_outputs: IndexMap<String, usize>,
    /// Brushes whose planes do not close a solid.
    pub bad_brushes: usize,
    /// Props left out because their model was not converted.
    pub missing_models: Vec<String>,
    /// `trigger_teleport` targets no entity in the file names.
    pub unresolved_teleports: Vec<String>,
    /// Doors, buttons and other movers kept as static brushes, by class.
    pub frozen_movers: IndexMap<String, usize>,
    /// Solids made only of Source's bullet-blocker tool, left out.
    pub bullet_blockers: usize,
}

/// Turns a `.vmf` document into one batch of edits.
pub fn import(doc: &Document, text: &str) -> Result<Command, ImportError> {
    import_with(doc, text, &VmfOptions::default()).map(|(c, _)| c)
}

pub fn import_with(doc: &Document, text: &str, opts: &VmfOptions) -> Result<(Command, VmfReport), ImportError> {
    let blocks = parse(text)?;
    let layer = doc
        .layer_ids()
        .first()
        .copied()
        .ok_or_else(|| ImportError::Unsupported("document has no layer".into()))?;
    let mut builder = Builder {
        layer,
        objects: Vec::new(),
        files: Vec::new(),
        meshes: 0,
        index: 0,
        sizes: &opts.texture_sizes,
        models: opts.models.as_ref(),
        schema_classes: opts.schema_classes.as_ref(),
        report: VmfReport::default(),
        cubemaps: Vec::new(),
        named: IndexMap::new(),
        teleports: Vec::new(),
        sky_ambient: None,
    };
    for block in &blocks {
        builder.top_level(block);
    }
    builder.resolve_teleports();
    if builder.objects.is_empty() {
        return Err(ImportError::Unsupported("the file contains no brushes or entities".into()));
    }
    let mut commands = builder.files;
    if let Some(sky) = builder.report.skyname.clone() {
        let mut meta = doc.doc.meta.clone();
        meta.source_sky = Some(sky);
        commands.push(Command::SetMeta { meta });
    }
    commands.push(Command::AddObjects { objects: builder.objects });
    if !builder.cubemaps.is_empty() || builder.sky_ambient.is_some() {
        let mut settings = doc.doc.settings.clone();
        settings.bake.cubemap_points.extend(builder.cubemaps.iter().copied());
        if let Some((color, intensity)) = builder.sky_ambient {
            settings.bake.sky_color = color;
            settings.bake.sky_intensity = intensity;
        }
        commands.push(Command::SetSettings { settings });
    }
    let mut report = builder.report;
    report.models.sort();
    report.models.dedup();
    report.materials.sort();
    report.materials.dedup();
    report.missing_models.sort();
    Ok((Command::Batch { label: "import vmf".into(), commands }, report))
}

/// A block's pairs with its repeated keys appended in file order.
fn all_pairs(block: &VBlock) -> impl Iterator<Item = (&str, &str)> {
    block
        .props
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .chain(block.dupes.iter().map(|(k, v)| (k.as_str(), v.as_str())))
}

/// Every output of an entity block: its `connections` child first, then plain
/// `OnXxx` keys (the shape a BSP lump stores its outputs in), each normalized
/// to the comma form the partitions carry. The second vector names the
/// repeated connections past the first per output name.
fn entity_outputs(block: &VBlock) -> (Vec<(String, String)>, Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut dropped = Vec::new();
    let mut take = |key: &str, value: &str| {
        if seen.insert(key.to_string()) {
            out.push((key.to_string(), source::connection_value(value)));
        } else {
            dropped.push(key.to_string());
        }
    };
    for conn in block.children.iter().filter(|c| c.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case("connections"))) {
        for (k, v) in all_pairs(conn) {
            take(k, v);
        }
    }
    for (k, v) in all_pairs(block) {
        if source::is_output_key(k, v) {
            take(k, v);
        }
    }
    (out, dropped)
}

/// Entity props without the plain output keys, which `carry_outputs` handles so
/// they never count as dropped keys.
fn plain_props(block: &VBlock) -> IndexMap<String, String> {
    block
        .props
        .iter()
        .filter(|(k, v)| !source::is_output_key(k, v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

struct Builder<'a> {
    layer: Id,
    objects: Vec<Object>,
    files: Vec<Command>,
    meshes: usize,
    index: usize,
    sizes: &'a IndexMap<String, [u32; 2]>,
    models: Option<&'a Vec<String>>,
    schema_classes: Option<&'a Vec<String>>,
    report: VmfReport,
    cubemaps: Vec<[f64; 3]>,
    /// targetname -> (origin, angles) of every point entity that has one.
    named: IndexMap<String, (String, String)>,
    /// Object index of each teleport volume and the target it names.
    teleports: Vec<(usize, String)>,
    /// The sun's `_ambient`: the sky light every surface the sun misses gets.
    sky_ambient: Option<([f32; 3], f32)>,
}

impl Builder<'_> {
    fn next_name(&mut self, prefix: &str) -> String {
        self.index += 1;
        format!("{prefix}_{}", self.index)
    }

    /// Hammer's editor state (`visgroups`, `viewsettings`, `cameras`, `cordons`,
    /// Hammer++ palettes, ...) carries no geometry and is skipped.
    fn top_level(&mut self, block: &VBlock) {
        match block.name.as_deref().unwrap_or("").to_ascii_lowercase().as_str() {
            "world" => self.world(block),
            "brush" | "solid" => self.brush(block, None),
            "entity" => self.entity(block, None),
            "group" => self.group(block),
            "hidden" => {
                for child in &block.children {
                    self.top_level(child);
                }
            }
            _ => {}
        }
    }

    /// The world's own sides are the map's clipping hull, not geometry; only the
    /// brushes nested inside it are. Hammer's `group` blocks inside the world are
    /// editor bookkeeping (membership lives in each solid's `editor` block).
    fn world(&mut self, block: &VBlock) {
        if let Some(sky) = block.prop("skyname").filter(|s| !s.is_empty()) {
            self.report.skyname = Some(sky.to_string());
        }
        for child in &block.children {
            match child.name.as_deref().unwrap_or("").to_ascii_lowercase().as_str() {
                "brush" | "solid" => self.brush(child, None),
                "hidden" => {
                    for solid in child.children.iter().filter(|c| is_solid(c)) {
                        self.brush(solid, None);
                    }
                }
                _ => {}
            }
        }
    }

    fn group(&mut self, block: &VBlock) {
        let mut object = new_object("group", self.layer, ObjectKind::Group);
        object.name = block.prop("id").map(|s| s.to_string()).unwrap_or_else(|| self.next_name("group"));
        let group_id = object.id;
        self.objects.push(object);
        for child in &block.children {
            match child.name.as_deref().unwrap_or("").to_ascii_lowercase().as_str() {
                "brush" => self.brush(child, Some(group_id)),
                "entity" => self.entity(child, Some(group_id)),
                "group" => self.group(child),
                _ => {}
            }
        }
    }

    fn solids(block: &VBlock) -> impl Iterator<Item = &VBlock> {
        block.children.iter().flat_map(|c| {
            let hidden = c.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case("hidden"));
            let own = is_solid(c).then_some(c);
            own.into_iter().chain(c.children.iter().filter(move |g| hidden && is_solid(g)))
        })
    }

    fn placed(block: &VBlock, angles_override_pitch: Option<f64>) -> Transform {
        let origin = block.prop("origin").and_then(|t| parse_vec3(t).ok()).unwrap_or(DVec3::ZERO);
        let mut angles = block.prop("angles").and_then(|t| parse_vec3(t).ok()).unwrap_or(DVec3::ZERO);
        if let Some(p) = angles_override_pitch {
            angles.x = p;
        }
        Transform::from_source_angles(origin, angles)
    }

    /// Outputs onto kept keyvalues: the first connection per name, the rest
    /// counted as `{classname}.{output}` since the document holds one value.
    fn carry_outputs(&mut self, classname: &str, block: &VBlock, keyvalues: &mut KeyValues) {
        let (outputs, dropped) = entity_outputs(block);
        for name in dropped {
            *self.report.dropped_outputs.entry(format!("{classname}.{name}")).or_default() += 1;
        }
        for (k, v) in outputs {
            keyvalues.insert(k, v);
        }
    }

    fn entity(&mut self, block: &VBlock, parent: Option<Id>) {
        let classname = block.prop("classname").unwrap_or("info_target").to_ascii_lowercase();
        if let (Some(name), Some(origin)) = (block.prop("targetname"), block.prop("origin")) {
            let angles = block.prop("angles").unwrap_or("0 0 0").to_string();
            self.named.entry(name.to_ascii_lowercase()).or_insert((origin.to_string(), angles));
        }
        let mut role = source::role(&classname);
        if source::IO_ENTITIES.contains(&classname.as_str()) {
            if let Some(known) = self.schema_classes {
                role = source::role_in_schema(&classname, known.iter().any(|k| k == &classname));
                // A mover the schema cannot drive still stands as geometry.
                if role == Role::Drop && source::is_frozen_mover(&classname) && Self::solids(block).next().is_some() {
                    role = source::role(&classname);
                }
            }
        }
        match role {
            Role::Passthrough => self.entity_raw(block, parent),
            Role::Drop => {
                *self.report.dropped_classes.entry(classname).or_default() += 1;
            }
            Role::WorldBrushes | Role::RenderBrushes => {
                if source::is_frozen_mover(&classname) {
                    *self.report.frozen_movers.entry(classname.clone()).or_default() += 1;
                }
                for solid in Self::solids(block) {
                    self.brush(solid, parent);
                }
            }
            Role::Volume(kind) => self.volume(block, &classname, kind),
            Role::Spawn => {
                let mut keyvalues = source::spawn_keys();
                if let Some(guid) = block.prop("id") {
                    keyvalues.insert("link_guid".into(), format!("source_spawn_{guid}"));
                }
                let transform = Self::placed(block, None);
                self.push_entity("info_spawnpoint_human", keyvalues, transform, parent);
                self.report.spawns += 1;
            }
            Role::Prop { dynamic } => self.prop(block, &classname, dynamic, parent),
            Role::Light(kind) => self.light(block, &classname, kind, parent),
            Role::Cubemap => {
                if let Some(o) = block.prop("origin").and_then(|t| parse_vec3(t).ok()) {
                    self.cubemaps.push([o.x, o.y, o.z]);
                    self.report.cubemaps += 1;
                }
            }
            Role::Keep => {
                let mut keyvalues = source::keep_keys(&classname, &plain_props(block), &mut self.report.dropped_keys);
                self.carry_outputs(&classname, block, &mut keyvalues);
                let transform = Self::placed(block, None);
                self.push_entity(&classname, keyvalues, transform, parent);
            }
        }
    }

    fn push_entity(&mut self, classname: &str, keyvalues: KeyValues, transform: Transform, parent: Option<Id>) {
        let name = entity_name(classname, &keyvalues, "entity");
        let mut object = new_object(
            name,
            self.layer,
            ObjectKind::Entity(Entity {
                classname: classname.to_string(),
                keyvalues,
                partition: None,
                links: Default::default(),
            }),
        );
        object.transform = transform;
        object.parent = parent;
        self.objects.push(object);
        self.report.entities += 1;
    }

    fn volume(&mut self, block: &VBlock, classname: &str, kind: VolumeKind) {
        let brushes: Vec<Brush> = Self::solids(block).filter_map(|s| self.solid_brush(s)).collect();
        if brushes.is_empty() {
            self.report.bad_brushes += 1;
            return;
        }
        let mut keyvalues = source::keep_keys(classname, &plain_props(block), &mut self.report.dropped_keys);
        self.carry_outputs(classname, block, &mut keyvalues);
        let name = entity_name(classname, &keyvalues, classname);
        // Apex has no teleport or push trigger class; the map script drives
        // these through the editor class of a plain trigger_multiple.
        let apex_class = match classname {
            _ if kind == VolumeKind::PlayerClip => "func_brush",
            "trigger_teleport" => {
                keyvalues.insert("editorclass".into(), "trigger_source_teleport".into());
                if let Some(target) = keyvalues.get("target") {
                    self.teleports.push((self.objects.len(), target.to_ascii_lowercase()));
                }
                "trigger_multiple"
            }
            "trigger_push" => {
                keyvalues.insert("editorclass".into(), "trigger_source_push".into());
                "trigger_multiple"
            }
            _ => classname,
        };
        self.objects.push(new_object(
            name,
            self.layer,
            ObjectKind::Volume(Volume { kind, classname: apex_class.to_string(), brushes, keyvalues }),
        ));
        self.report.volumes += 1;
    }

    /// Writes each teleport's destination onto it, since the script cannot
    /// look an arbitrary Source targetname up at runtime.
    fn resolve_teleports(&mut self) {
        for (index, target) in std::mem::take(&mut self.teleports) {
            let Some((origin, angles)) = self.named.get(&target).cloned() else {
                if !self.report.unresolved_teleports.contains(&target) {
                    self.report.unresolved_teleports.push(target);
                }
                continue;
            };
            if let Some(ObjectKind::Volume(v)) = self.objects.get_mut(index).map(|o| &mut o.kind) {
                v.keyvalues.insert("teleport_origin".into(), origin);
                v.keyvalues.insert("teleport_angles".into(), angles);
            }
        }
    }

    fn prop(&mut self, block: &VBlock, classname: &str, dynamic: bool, parent: Option<Id>) {
        let Some(mdl) = block.prop("model").filter(|m| !m.is_empty()) else {
            *self.report.dropped_classes.entry(format!("{classname} (no model)")).or_default() += 1;
            return;
        };
        let source_path = mdl.replace('\\', "/").to_ascii_lowercase();
        self.report.models.push(source_path.clone());
        if self.models.is_some_and(|m| !m.contains(&source_path)) {
            if !self.report.missing_models.contains(&source_path) {
                self.report.missing_models.push(source_path);
            }
            return;
        }
        let mut transform = Self::placed(block, None);
        let scale = ["uniformscale", "modelscale"]
            .iter()
            .find_map(|k| block.prop(k)?.trim().parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(1.0);
        transform.scale = DVec3::splat(scale);
        let fade = block.prop("fademaxdist").and_then(|t| t.trim().parse::<f32>().ok()).unwrap_or(0.0);
        let solid = block.prop("solid").map(|s| s.trim() != "0").unwrap_or(true);
        let shadows = block.prop("disableshadows").map(|s| s.trim() == "0").unwrap_or(true);
        let mut keyvalues = if dynamic {
            source::keep_keys(classname, &plain_props(block), &mut self.report.dropped_keys)
        } else {
            KeyValues::new()
        };
        if dynamic {
            self.carry_outputs(classname, block, &mut keyvalues);
        }
        let prop = Prop {
            model: source::model_path(mdl),
            skin: block.prop("skin").and_then(|t| t.trim().parse().ok()).unwrap_or(0),
            static_prop: !dynamic,
            solid,
            cast_shadows: shadows,
            fade_distance: fade.max(0.0),
            keyvalues,
        };
        let name = mdl.rsplit(['/', '\\']).next().unwrap_or(mdl).trim_end_matches(".mdl").to_string();
        let mut object = new_object(name, self.layer, ObjectKind::Prop(prop));
        object.transform = transform;
        object.parent = parent;
        self.objects.push(object);
        self.report.props += 1;
    }

    fn light(&mut self, block: &VBlock, classname: &str, kind: LightKind, parent: Option<Id>) {
        let key = |k: &str| block.props.iter().find(|(p, _)| p.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str());
        let (color, intensity) = if kind == LightKind::Sun {
            source::sun_value(key("_light"), key("_lightHDR"), key("_lightscaleHDR"))
        } else {
            source::hdr_light_value(key("_light"), key("_lightHDR"), key("_lightscaleHDR"))
        };
        if kind == LightKind::Sun && key("_ambient").is_some() {
            self.sky_ambient = Some(source::hdr_light_value(key("_ambient"), key("_ambientHDR"), key("_AmbientScaleHDR")));
        }
        let number = |k: &str, d: f32| block.prop(k).and_then(|t| t.trim().parse::<f32>().ok()).unwrap_or(d);
        let light = Light {
            kind,
            color,
            intensity,
            range: number("_distance", 0.0),
            inner_angle: number("_inner_cone", 30.0),
            outer_angle: number("_cone", 45.0),
            size: [0.0, 0.0],
            cast_shadows: true,
            baked: true,
            realtime: false,
            keyvalues: KeyValues::new(),
        };
        let mut object = new_object(classname, self.layer, ObjectKind::Light(light));
        object.transform = Self::placed(block, None);
        if matches!(kind, LightKind::Sun | LightKind::Spot) {
            // An r5ed light shines along its local -Z.
            object.transform.rotation = DQuat::from_rotation_arc(DVec3::NEG_Z, source::light_direction(block.prop("angles"), block.prop("pitch"), block.prop("angle")));
        }
        object.parent = parent;
        self.objects.push(object);
        self.report.lights += 1;
    }

    /// A class the Source table does not know, kept exactly as written.
    fn entity_raw(&mut self, block: &VBlock, parent: Option<Id>) {
        let classname = block.prop("classname").unwrap_or("info_target").to_string();
        let mut keyvalues: KeyValues = block
            .props
            .iter()
            .filter(|(k, _)| k.as_str() != "id" && k.as_str() != "_tb_text")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.carry_outputs(&classname.to_ascii_lowercase(), block, &mut keyvalues);
        let name = entity_name(&classname, &keyvalues, "entity");
        let mut object = new_object(
            name,
            self.layer,
            ObjectKind::Entity(Entity { classname, keyvalues, partition: None, links: Default::default() }),
        );
        if let Some(origin) = block.prop("origin").and_then(|t| parse_vec3(t).ok()) {
            object.transform.position = origin;
        }
        if let Some(angles) = block.prop("angles").and_then(|t| parse_vec3(t).ok()) {
            object.transform = Transform::from_source_angles(object.transform.position, angles);
        }
        object.parent = parent;
        let entity_id = object.id;
        self.objects.push(object);
        for child in &block.children {
            match child.name.as_deref().unwrap_or("").to_ascii_lowercase().as_str() {
                "brush" | "solid" => self.brush(child, Some(entity_id)),
                "hidden" => {
                    for solid in child.children.iter().filter(|c| is_solid(c)) {
                        self.brush(solid, Some(entity_id));
                    }
                }
                "displacement" => self.displacement(child, Some(entity_id)),
                _ => {}
            }
        }
    }

    fn solid_parts(&mut self, block: &VBlock) -> Option<(Brush, Solid)> {
        let sides: Vec<BrushFace> =
            block.named("side").map(|s| side_face(s, self.sizes)).collect::<Result<_, _>>().unwrap_or_default();
        for side in block.named("side") {
            if let Some(m) = side.prop("material") {
                self.report.materials.push(source::clean_name(m));
            }
        }
        match orient_faces(&sides) {
            Ok(faces) => match Solid::from_faces(&faces) {
                Ok(solid) => Some((Brush { faces, contents: 0, detail: false }, solid)),
                Err(_) => {
                    self.report.bad_brushes += 1;
                    None
                }
            },
            Err(_) => {
                self.report.bad_brushes += 1;
                None
            }
        }
    }

    fn solid_brush(&mut self, block: &VBlock) -> Option<Brush> {
        self.solid_parts(block).map(|(brush, _)| brush)
    }

    fn brush(&mut self, block: &VBlock, parent: Option<Id>) {
        let Some((brush, solid)) = self.solid_parts(block) else { return };
        // Source bullet blockers stop bullets and let players through; Apex
        // has no bullets-only collision, so a solid of nothing else is left out.
        if !brush.faces.is_empty() && brush.faces.iter().all(|f| f.material == source::BLOCK_BULLETS) {
            self.report.bullet_blockers += 1;
            return;
        }
        // A solid made only of clip faces is collision the player cannot see.
        if !brush.faces.is_empty() && brush.faces.iter().all(|f| source::is_clip(&f.material)) {
            let name = self.next_name("playerclip");
            self.objects.push(new_object(
                name,
                self.layer,
                ObjectKind::Volume(Volume {
                    kind: VolumeKind::PlayerClip,
                    classname: "func_brush".into(),
                    brushes: vec![brush],
                    keyvalues: KeyValues::new(),
                }),
            ));
            self.report.volumes += 1;
            return;
        }
        let mut object = new_object(self.next_name("brush"), self.layer, ObjectKind::Brush(brush));
        object.parent = parent;
        self.objects.push(object);
        self.report.brushes += 1;
        for side in block.named("side") {
            let Some(disp) = disp_block(side, "dispinfo") else { continue };
            self.side_dispinfo(side, disp, &solid, parent);
        }
        for child in block.named("displacement") {
            self.displacement(child, parent);
        }
    }

    /// One displaced side as a mesh; the brush above keeps all its faces.
    fn side_dispinfo(&mut self, side: &VBlock, disp: &VBlock, solid: &Solid, parent: Option<Id>) {
        let Ok(face) = side_face(side, self.sizes) else { return };
        let Some(info) = parse_dispinfo(disp, face.normal) else { return };
        let mut corners = None;
        for (i, p) in solid.planes.iter().enumerate() {
            let aligned = p.normal.dot(face.normal);
            let gap = (p.dist - face.dist).abs();
            let opposed = (p.dist + face.dist).abs();
            if (aligned > 1.0 - 1e-9 && gap <= 1e-3) || (aligned < -1.0 + 1e-9 && opposed <= 1e-3) {
                let pts = solid.loop_points(i);
                if pts.len() == 4 {
                    corners = Some([pts[0], pts[1], pts[2], pts[3]]);
                }
                break;
            }
        }
        let Some(corners) = corners else { return };
        let source = side.prop("material").unwrap_or("").replace('\\', "/").to_ascii_lowercase();
        let apex = source::material(&source);
        let name = self.next_name("displacement");
        let mut mesh = dispinfo_mesh(&face, &source, &info, corners);
        mesh.name = name.clone();
        let Ok(bytes) = glb::write(&mesh) else { return };
        self.meshes += 1;
        self.report.meshes += 1;
        let path = mesh_path(self.meshes);
        self.files.push(Command::SetFile { path: path.clone(), bytes: Some(bytes) });
        let mut material_map = IndexMap::new();
        material_map.insert(source, apex);
        let mut object = new_object(
            name,
            self.layer,
            ObjectKind::Mesh(Mesh { modifiers: Vec::new(), role: Default::default(),
                source: path,
                material_map,
                collision: CollisionMode::Triangles,
                contents: 0,
                cast_shadows: true,
                lightmap_scale: 0.0,
                render: true,
            }),
        );
        object.parent = parent;
        self.objects.push(object);
    }

    fn displacement(&mut self, block: &VBlock, parent: Option<Id>) {
        let Ok(d) = parse_displacement(block) else { return };
        let Ok(bytes) = glb::write(&displacement_mesh(&d)) else { return };
        self.meshes += 1;
        let path = mesh_path(self.meshes);
        self.files.push(Command::SetFile { path: path.clone(), bytes: Some(bytes) });
        let mut object = new_object(
            d.name.clone(),
            self.layer,
            ObjectKind::Mesh(Mesh {
                source: path,
                material_map: Default::default(),
                collision: CollisionMode::Triangles,
                contents: 0,
                cast_shadows: true,
                lightmap_scale: 0.0,
                render: true,
                role: crate::model::MeshRole::Normal,
                modifiers: Vec::new(),
            }),
        );
        object.parent = parent;
        self.objects.push(object);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, MemoryFileStore};
    use crate::model::MapDoc;
    use crate::schema::{EntitySchema, Severity};

    /// A 128x128x128 box at the origin and a 128x128x64 box, both written with
    /// outward-facing planes, then a brush entity and a point entity.
    const SMALL_VMF: &str = r#"
VersionInfo
{
    "editorversion" "400"
}
world
{
    "id" "1"
    "classname" "worldspawn"
    "solidcolor" "255 0 0"
    side
    {
        "id" "2"
        "plane" "(0 0 1) (0 1 0) (0 0 0)"
        "material" "BRICK/BRICKWALL"
    }
}
brush
{
    "id" "10"
    side
    {
        "id" "11"
        "plane" "(-64 -64 0) (-64 64 0) (64 64 0)"
        "material" "CONCRETE/BRICK"
    }
    side
    {
        "id" "12"
        "plane" "(-64 -64 128) (64 -64 128) (64 64 128)"
        "material" "CONCRETE/BRICK"
        "uaxis" "[1 0 0 0] 0.25"
        "vaxis" "[0 -1 0 0] 0.25"
        "rotation" "0"
        "lightmapscale" "16"
    }
    side
    {
        "id" "13"
        "plane" "(-64 -64 0) (-64 -64 128) (-64 64 128)"
    }
    side
    {
        "id" "14"
        "plane" "(64 -64 0) (64 64 0) (64 64 128)"
    }
    side
    {
        "id" "15"
        "plane" "(-64 -64 0) (64 -64 0) (64 -64 128)"
    }
    side
    {
        "id" "16"
        "plane" "(-64 64 0) (-64 64 128) (64 64 128)"
    }
}
brush
{
    "id" "20"
    side { "plane" "(0 0 0) (0 128 0) (128 128 0)" }
    side { "plane" "(0 0 64) (128 0 64) (128 128 64)" }
    side { "plane" "(0 0 0) (0 0 64) (0 128 64)" }
    side { "plane" "(128 0 0) (128 128 0) (128 128 64)" }
    side { "plane" "(0 0 0) (128 0 0) (128 0 64)" }
    side { "plane" "(0 128 0) (0 128 64) (128 128 64)" }
}
entity
{
    "id" "30"
    "classname" "func_monitor"
    "origin" "256 256 0"
    "targetname" "door_main"
    brush
    {
        "id" "31"
        side { "plane" "(0 0 0) (0 64 0) (64 64 0)" }
        side { "plane" "(0 0 32) (64 0 32) (64 64 32)" }
        side { "plane" "(0 0 0) (0 0 32) (0 64 32)" }
        side { "plane" "(64 0 0) (64 64 0) (64 64 32)" }
        side { "plane" "(0 0 0) (64 0 0) (64 0 32)" }
        side { "plane" "(0 64 0) (0 64 32) (64 64 32)" }
    }
}
entity
{
    "id" "40"
    "classname" "info_target"
    "origin" "0 0 200"
    "targetname" "marker"
}
"#;

    fn doc() -> Document {
        Document::new(MapDoc::default(), Box::new(MemoryFileStore::new()))
    }

    #[test]
    fn vmf_worldspawn_skyname_becomes_the_documents_source_sky() {
        let text = format!("world\n{{\n\"classname\" \"worldspawn\"\n\"skyname\" \"tides\"\n{}}}\n", cube_solid(2, 0.0, 64.0));
        let d = doc();
        let (cmd, report) = import_with(&d, &text, &VmfOptions::default()).expect("imports");
        assert_eq!(report.skyname.as_deref(), Some("tides"));
        let mut d = d;
        d.apply(cmd).unwrap();
        assert_eq!(d.doc.meta.source_sky.as_deref(), Some("tides"));
    }

    fn kind_of(d: &Document) -> Vec<&'static str> {
        d.doc
            .objects
            .iter()
            .map(|o| match o.kind {
                ObjectKind::Brush(_) => "brush",
                ObjectKind::Entity(_) => "entity",
                ObjectKind::Group => "group",
                ObjectKind::Mesh(_) => "mesh",
                _ => "other",
            })
            .collect()
    }

    fn cube_solid(id: u32, min: f64, max: f64) -> String {
        let (a, b) = (min, max);
        let sides = [
            format!("({a} {b} {b}) ({b} {b} {b}) ({b} {a} {b})"),
            format!("({a} {a} {a}) ({b} {a} {a}) ({b} {b} {a})"),
            format!("({a} {b} {b}) ({a} {a} {b}) ({a} {a} {a})"),
            format!("({b} {b} {a}) ({b} {a} {a}) ({b} {a} {b})"),
            format!("({b} {b} {b}) ({a} {b} {b}) ({a} {b} {a})"),
            format!("({b} {a} {a}) ({a} {a} {a}) ({a} {a} {b})"),
        ];
        let sides: String = sides
            .iter()
            .map(|p| format!("side\n{{\n\"plane\" \"{p}\"\n\"material\" \"DEV/DEV_MEASUREGENERIC01\"\n\"uaxis\" \"[1 0 0 0] 0.25\"\n\"vaxis\" \"[0 -1 0 0] 0.25\"\n}}\n"))
            .collect();
        format!("solid\n{{\n\"id\" \"{id}\"\n{sides}editor\n{{\n\"color\" \"0 255 0\"\n}}\n}}\n")
    }

    /// Hammer's own layout: solids nested in `world` and `entity`, `hidden`
    /// wrappers, world `group` bookkeeping and editor-state blocks around them.
    #[test]
    fn vmf_import_hammer_layout() {
        let text = format!(
            "versioninfo\n{{\n\"editorversion\" \"400\"\n}}\nvisgroups\n{{\n}}\nviewsettings\n{{\n\"bSnapToGrid\" \"1\"\n}}\n\
             world\n{{\n\"id\" \"1\"\n\"classname\" \"worldspawn\"\n{}hidden\n{{\n{}}}\ngroup\n{{\n\"id\" \"9\"\n}}\n}}\n\
             entity\n{{\n\"id\" \"20\"\n\"classname\" \"trigger_teleport\"\n{}}}\n\
             hidden\n{{\nentity\n{{\n\"id\" \"30\"\n\"classname\" \"info_player_start\"\n\"origin\" \"0 0 64\"\n}}\n}}\n\
             cameras\n{{\n\"activecamera\" \"-1\"\n}}\ncordons\n{{\n\"active\" \"0\"\n}}\n",
            cube_solid(2, 0.0, 64.0),
            cube_solid(3, 128.0, 192.0),
            cube_solid(21, 256.0, 320.0),
        );
        let d = doc();
        let cmd = import(&d, &text).expect("a Hammer vmf imports");
        let mut d = d;
        d.apply(cmd).unwrap();
        assert_eq!(kind_of(&d), vec!["brush", "brush", "other", "entity"]);
        let ObjectKind::Volume(teleport) = &d.doc.objects[2].kind else { panic!("expected a volume") };
        assert_eq!(teleport.classname, "trigger_multiple");
        assert_eq!(teleport.keyvalues.get("editorclass").map(String::as_str), Some("trigger_source_teleport"));
        assert_eq!(teleport.brushes.len(), 1, "the trigger owns its solid");
        let ObjectKind::Entity(spawn) = &d.doc.objects[3].kind else { panic!("expected a spawn") };
        assert_eq!(spawn.classname, "info_spawnpoint_human");
    }

    #[test]
    fn vmf_bullet_blocker_solids_are_left_out() {
        let blocker = cube_solid(2, 128.0, 192.0).replace(
            &cube_solid(2, 128.0, 192.0).split("\"material\" \"").nth(1).unwrap().split('"').next().unwrap().to_string(),
            "TOOLS/TOOLSBLOCKBULLETS",
        );
        let text = format!("world
{{
\"classname\" \"worldspawn\"
{}{}}}
", cube_solid(1, 0.0, 64.0), blocker);
        let d = doc();
        let (_, report) = import_with(&d, &text, &VmfOptions::default()).expect("imports");
        assert_eq!(report.bullet_blockers, 1);
        assert_eq!(report.brushes, 1);
    }

    #[test]
    fn vmf_teleports_carry_their_destination_and_pushes_their_speed() {
        let text = format!(
            "world
{{
\"classname\" \"worldspawn\"
{}}}
             entity
{{
\"classname\" \"trigger_teleport\"
\"target\" \"Dest_A\"
{}}}
             entity
{{
\"classname\" \"trigger_teleport\"
\"target\" \"nowhere\"
{}}}
             entity
{{
\"classname\" \"trigger_push\"
\"pushdir\" \"-90 0 0\"
\"speed\" \"800\"
{}}}
             entity
{{
\"classname\" \"info_teleport_destination\"
\"targetname\" \"dest_a\"
             \"origin\" \"10 20 30\"
\"angles\" \"0 90 0\"
}}
",
            cube_solid(1, 0.0, 64.0),
            cube_solid(2, 128.0, 192.0),
            cube_solid(3, 256.0, 320.0),
            cube_solid(4, 384.0, 448.0),
        );
        let d = doc();
        let (_, report) = import_with(&d, &text, &VmfOptions::default()).expect("imports");
        assert_eq!(report.unresolved_teleports, vec!["nowhere".to_string()]);
        let (cmd, _) = import_with(&d, &text, &VmfOptions::default()).unwrap();
        let mut d = d;
        d.apply(cmd).unwrap();
        let volumes: Vec<&Volume> = d.doc.objects.iter()
            .filter_map(|o| if let ObjectKind::Volume(v) = &o.kind { Some(v) } else { None })
            .collect();
        assert_eq!(volumes.len(), 3);
        let kv = |v: &Volume, k: &str| v.keyvalues.get(k).cloned();
        assert_eq!(kv(volumes[0], "teleport_origin").as_deref(), Some("10 20 30"));
        assert_eq!(kv(volumes[0], "teleport_angles").as_deref(), Some("0 90 0"));
        assert_eq!(kv(volumes[1], "teleport_origin"), None);
        assert_eq!(volumes[2].classname, "trigger_multiple");
        assert_eq!(kv(volumes[2], "editorclass").as_deref(), Some("trigger_source_push"));
        assert_eq!(kv(volumes[2], "pushspeed").as_deref(), Some("800"));
        assert_eq!(kv(volumes[2], "pushdir").as_deref(), Some("-90 0 0"));
    }

    #[test]
    fn vmf_import_small() {
        let mut d = doc();
        let (inverse, ids) = d.apply(import(&d, SMALL_VMF).unwrap()).unwrap();
        assert_eq!(kind_of(&d), vec!["brush", "brush", "entity", "brush", "entity"]);
        assert_eq!(ids.len(), 5);
        assert_eq!(d.doc.objects[0].name, "brush_1");
        let ObjectKind::Brush(first) = &d.doc.objects[0].kind else { panic!("expected a brush") };
        assert_eq!(first.faces.len(), 6);
        assert!((Solid::from_faces(&first.faces).unwrap().volume() - 128.0 * 128.0 * 128.0).abs() < 1e-6);

        let entity = d.doc.objects.iter().find(|o| o.name == "door_main").expect("brush entity");
        let ObjectKind::Entity(e) = &entity.kind else { panic!("expected an entity") };
        assert_eq!(e.classname, "func_monitor");
        assert_eq!(e.keyvalues["classname"], "func_monitor");
        assert_eq!(e.keyvalues.get("id"), None, "the VMF id is not an entity key");
        assert_eq!(entity.transform.position, DVec3::new(256.0, 256.0, 0.0));
        let children = d.children(entity.id);
        assert_eq!(children.len(), 1, "the entity owns the brush solid");
        let ObjectKind::Brush(solid) = &d.object(children[0]).unwrap().kind else { panic!("expected a brush") };
        assert_eq!(solid.faces.len(), 6);
        assert!(!solid.detail);
        assert!((Solid::from_faces(&solid.faces).unwrap().volume() - 64.0 * 64.0 * 32.0).abs() < 1e-6);

        let point = d.doc.objects.iter().find(|o| o.name == "marker").expect("point entity");
        assert!(d.children(point.id).is_empty());
        assert_eq!(point.transform.position.z, 200.0);

        d.apply(inverse).unwrap();
        assert!(d.doc.objects.is_empty());
    }

    #[test]
    fn vmf_uv_axes_and_lightmap_scale_survive() {
        let mut d = doc();
        d.apply(import(&d, SMALL_VMF).unwrap()).unwrap();
        let ObjectKind::Brush(b) = &d.doc.objects[0].kind else { panic!("expected a brush") };
        let top = b.faces.iter().find(|f| f.normal.z > 0.9).expect("a +Z face");
        assert_eq!(top.material, "world/source/concrete/brick");
        assert_eq!(top.uv.u_axis, DVec3::X);
        assert_eq!(top.uv.v_axis, DVec3::NEG_Y);
        assert_eq!(top.uv.scale, [0.25, 0.25]);
        assert_eq!(top.uv.offset, [0.0, 0.0]);
        assert_eq!(top.uv.rotation, 0.0);
        assert_eq!(top.lightmap_scale, 16.0);
        let bottom = b.faces.iter().find(|f| f.normal.z < -0.9).expect("a -Z face");
        assert_eq!(bottom.uv.u_axis, DVec3::NEG_Y, "an unstated axis is derived from the plane");
        assert_eq!(bottom.uv.v_axis, DVec3::NEG_X);
    }

    #[test]
    fn vmf_with_inward_planes_still_builds_a_solid() {
        let mut inward = String::from("brush\n{\n");
        for (i, p) in [
            "(0 0 64) (64 64 64) (64 0 64)",
            "(0 0 0) (64 64 0) (0 64 0)",
            "(0 0 0) (0 64 64) (0 0 64)",
            "(64 0 0) (64 64 64) (64 64 0)",
            "(0 0 0) (64 0 64) (64 0 0)",
            "(0 64 0) (64 64 64) (0 64 64)",
        ]
        .iter()
        .enumerate()
        {
            inward.push_str(&format!("side {{ \"id\" \"{}\" \"plane\" \"{p}\" }}\n", i + 1));
        }
        inward.push_str("}\n");
        let mut d = doc();
        d.apply(import(&d, &inward).unwrap()).unwrap();
        assert_eq!(d.doc.objects.len(), 1);
        let ObjectKind::Brush(b) = &d.doc.objects[0].kind else { panic!("expected a brush") };
        let volume = Solid::from_faces(&b.faces).unwrap().volume();
        assert!((volume - 64.0 * 64.0 * 64.0).abs() < 1e-6, "volume was {volume}");
    }

    #[test]
    fn vmf_displacement_becomes_a_glb_mesh() {
        let vmf = r#"
brush
{
    side { "plane" "(0 0 0) (0 64 0) (64 64 0)" }
    side { "plane" "(0 0 32) (64 0 32) (64 64 32)" }
    side { "plane" "(0 0 0) (0 0 32) (0 64 32)" }
    side { "plane" "(64 0 0) (64 64 0) (64 64 32)" }
    side { "plane" "(0 0 0) (64 0 0) (64 0 32)" }
    side { "plane" "(0 64 0) (0 64 32) (64 64 32)" }
    displacement
    {
        "name" "d1"
        "startPos" "(0 0 32)"
        "endPos" "(128 128 32)"
        "gridSize" "2"
        "uaxis" "[1 0 0 0] 1"
        "vaxis" "[0 -1 0 0] 1"
        "DIP_00" "0 0 0 0"
        "DIP_01" "0 0 16 0"
        "DIP_02" "0 0 0 0"
        "DIP_08" "0 0 32 0"
    }
}
"#;
        let mut d = doc();
        d.apply(import(&d, vmf).unwrap()).unwrap();
        let Some(m) = d.doc.objects.iter().find_map(|o| match &o.kind {
            ObjectKind::Mesh(m) => Some(m),
            _ => None,
        }) else {
            panic!("expected a mesh object");
        };
        assert!(d.files.exists(&m.source), "the glb was written to the file store");
        assert_eq!(&d.files.read(&m.source).unwrap()[0..4], b"glTF");
        let ObjectKind::Brush(b) = &d.doc.objects[0].kind else { panic!("expected a brush") };
        assert_eq!(b.faces.len(), 6);
    }

    #[test]
    fn malformed_vmf_is_rejected_without_a_panic() {
        let d = doc();
        for bad in ["", "{", "brush {", "brush { side { \"plane\" \"(0 0 0)\" } }", "\"only\"", "nonsense { }"] {
            assert!(import(&d, bad).is_err(), "{bad:?} should not import");
        }
    }

    fn disp_side(id: u32, plane: &str) -> String {
        let flat5: Vec<String> = (0..5).map(|_| "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1".to_string()).collect();
        let zeros5: Vec<String> = (0..5).map(|_| "0 0 0 0 0".to_string()).collect();
        let zeros15: Vec<String> = (0..5).map(|_| "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0".to_string()).collect();
        let normals = flat5.iter().enumerate().map(|(i, v)| format!("\"row{i}\" \"{v}\"")).collect::<Vec<_>>().join("\n");
        let mut distances = zeros5.clone();
        distances[2] = "0 0 16 0 0".to_string();
        let distances = distances.iter().enumerate().map(|(i, v)| format!("\"row{i}\" \"{v}\"")).collect::<Vec<_>>().join("\n");
        let offsets = zeros15.iter().enumerate().map(|(i, v)| format!("\"row{i}\" \"{v}\"")).collect::<Vec<_>>().join("\n");
        let mut alphas = zeros5;
        alphas[2] = "0 0 1 0 0".to_string();
        let alphas = alphas.iter().enumerate().map(|(i, v)| format!("\"row{i}\" \"{v}\"")).collect::<Vec<_>>().join("\n");
        format!(
            "side\n{{\n\"id\" \"{id}\"\n\"plane\" \"{plane}\"\n\"material\" \"TEST/WALL\"\n\
             \"uaxis\" \"[1 0 0 0] 0.25\"\n\"vaxis\" \"[0 -1 0 0] 0.25\"\n\"rotation\" \"0\"\n\"lightmapscale\" \"16\"\n\
             dispinfo\n{{\n\"power\" \"2\"\n\"startposition\" \"[0 0 32]\"\n\"elevation\" \"4\"\n\
             \"subdiv\" \"0\"\nnormals\n{{\n{normals}\n}}\ndistances\n{{\n{distances}\n}}\n\
             offsets\n{{\n{offsets}\n}}\nalphas\n{{\n{alphas}\n}}\n}}\n}}\n"
        )
    }

    #[test]
    fn vmf_dispinfo_power2_grid_positions_and_alpha() {
        let vmf = format!(
            "solid\n{{\n\"id\" \"1\"\n{}\n{}\n{}\n{}\n{}\n{}\n}}\n",
            disp_side(2, "(0 0 32) (64 0 32) (64 64 32)"),
            "side\n{\n\"id\" \"3\"\n\"plane\" \"(0 0 0) (64 64 0) (64 0 0)\"\n\"material\" \"TEST/WALL\"\n}",
            "side\n{\n\"id\" \"4\"\n\"plane\" \"(64 0 0) (64 64 0) (64 64 32)\"\n\"material\" \"TEST/WALL\"\n}",
            "side\n{\n\"id\" \"5\"\n\"plane\" \"(0 0 0) (0 64 32) (0 64 0)\"\n\"material\" \"TEST/WALL\"\n}",
            "side\n{\n\"id\" \"6\"\n\"plane\" \"(0 0 0) (64 0 0) (64 0 32)\"\n\"material\" \"TEST/WALL\"\n}",
            "side\n{\n\"id\" \"7\"\n\"plane\" \"(0 64 0) (0 64 32) (64 64 32)\"\n\"material\" \"TEST/WALL\"\n}",
        );
        let d = doc();
        let (cmd, report) = import_with(&d, &vmf, &VmfOptions::default()).expect("imports");
        assert_eq!((report.meshes, report.brushes), (1, 1), "the displaced side is a mesh and the brush stays");
        let mut d = d;
        d.apply(cmd).unwrap();
        let mesh_object = d.doc.objects.iter().find(|o| matches!(o.kind, ObjectKind::Mesh(_))).expect("a mesh");
        let ObjectKind::Mesh(m) = &mesh_object.kind else { panic!("expected a mesh") };
        assert_eq!(m.collision, CollisionMode::Triangles);
        assert_eq!(m.material_map.get("test/wall").map(String::as_str), Some("world/source/test/wall"));
        let bytes = d.files.read(&m.source).expect("the glb was written");
        let mesh = glb::read(&bytes).expect("the glb reads back");
        assert_eq!((mesh.positions.len(), mesh.colors.len(), mesh.indices.len()), (25, 25, 96));
        assert_eq!(mesh.positions[0], [0.0, 0.0, 36.0], "vertex zero is the corner nearest startposition, lifted by elevation");
        assert_eq!(mesh.positions[12], [32.0, 32.0, 52.0], "the middle vertex rides its distance along the normal");
        let corners: Vec<[u32; 3]> = mesh.indices.chunks_exact(3).take(1).map(|t| [t[0], t[1], t[2]]).collect();
        assert_eq!(corners.len(), 1);
        let (a, b, c) = (mesh.positions[corners[0][0] as usize], mesh.positions[corners[0][1] as usize], mesh.positions[corners[0][2] as usize]);
        let edge1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let edge2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let wound = [edge1[1] * edge2[2] - edge1[2] * edge2[1], edge1[2] * edge2[0] - edge1[0] * edge2[2], edge1[0] * edge2[1] - edge1[1] * edge2[0]];
        assert!(wound[2] > 0.0, "the first triangle faces the displacement normal");
        let mut grid_corners: Vec<[f32; 3]> = [0, 4, 20, 24].iter().map(|&i| mesh.positions[i]).collect();
        grid_corners.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(grid_corners, [[0.0, 0.0, 36.0], [0.0, 64.0, 36.0], [64.0, 0.0, 36.0], [64.0, 64.0, 36.0]]);
        assert_eq!(mesh.colors[0], [255, 255, 255, 0]);
        assert_eq!(mesh.colors[12], [255, 255, 255, 255], "alphas land in COLOR_0.a");
        assert!((mesh.uvs[12][0] - 0.25).abs() < 1e-6 && (mesh.uvs[12][1] + 0.25).abs() < 1e-6);
    }

    const SCHEMA_JSON: &str = include_str!("../../../../schema/entities.json");

    fn schema_classes() -> Vec<String> {
        let schema = EntitySchema::load(SCHEMA_JSON).expect("the shipped schema loads");
        schema.classes.keys().cloned().collect()
    }

    fn opts_with_schema() -> VmfOptions {
        VmfOptions { schema_classes: Some(schema_classes()), ..Default::default() }
    }

    fn brush_entity(classname: &str, extra: &str, id: u32) -> String {
        format!("entity\n{{\n\"id\" \"{id}\"\n\"classname\" \"{classname}\"\n{extra}{}}}\n", cube_solid(id + 100, 0.0, 64.0))
    }

    /// A button (touch trigger) firing a sliding door, with the relay classes
    /// the schema does not know. Outputs round-trip as comma keyvalues the
    /// schema accepts; the unknown classes stay dropped and listed.
    #[test]
    fn vmf_io_button_door_relay_round_trips() {
        let trigger = brush_entity(
            "trigger_multiple",
            "\"targetname\" \"button\"\nconnections\n{\n\"OnTrigger\" \"door_main,Open,,0,-1\"\n}\n",
            20,
        );
        let door = brush_entity(
            "func_movelinear",
            "\"targetname\" \"door_main\"\nconnections\n{\n\"OnFullyOpen\" \"button\x1bTrigger\x1b\x1b0.5\x1b-1\"\n}\n",
            30,
        );
        let text = format!(
            "world\n{{\n\"classname\" \"worldspawn\"\n{}\n}}\n{trigger}{door}\
             entity\n{{\n\"classname\" \"logic_relay\"\n\"targetname\" \"relay1\"\n}}\n\
             entity\n{{\n\"classname\" \"func_button\"\n\"targetname\" \"btn\"\n}}\n\
             entity\n{{\n\"classname\" \"func_door\"\n\"targetname\" \"door2\"\n}}\n",
            cube_solid(2, 256.0, 320.0),
        );
        let d = doc();
        let (cmd, report) = import_with(&d, &text, &opts_with_schema()).expect("imports");
        for class in ["logic_relay", "func_button", "func_door"] {
            assert_eq!(report.dropped_classes.get(class), Some(&1), "{class} stays dropped");
        }
        let mut d = d;
        d.apply(cmd).unwrap();
        let button = d.doc.objects.iter().find(|o| o.name == "button").expect("the button");
        let ObjectKind::Volume(v) = &button.kind else { panic!("the button is a volume") };
        assert_eq!(v.classname, "trigger_multiple");
        assert_eq!(v.keyvalues.get("OnTrigger").map(String::as_str), Some("door_main,Open,,0,-1"));
        assert_eq!(v.brushes.len(), 1, "the trigger owns its solid");
        let door = d.doc.objects.iter().find(|o| o.name == "door_main").expect("the door");
        let ObjectKind::Entity(e) = &door.kind else { panic!("the door is a passthrough entity") };
        assert_eq!(e.classname, "func_movelinear");
        assert_eq!(e.keyvalues.get("OnFullyOpen").map(String::as_str), Some("button,Trigger,,0.5,-1"));
        assert_eq!(d.children(door.id).len(), 1, "the door keeps its brushes");
        assert!(d.doc.objects.iter().all(|o| o.name != "relay1" && o.name != "btn" && o.name != "door2"));
        let schema = EntitySchema::load(SCHEMA_JSON).expect("the shipped schema loads");
        for o in [&button, &door] {
            let issues = schema.validate_object(o);
            assert!(
                issues.iter().all(|i| i.severity != Severity::Error),
                "the schema accepts the carried outputs: {issues:?}"
            );
            assert!(
                issues.iter().all(|i| !i.message.contains("OnTrigger") && !i.message.contains("OnFullyOpen")),
                "no issue names the outputs: {issues:?}"
            );
        }
    }

    #[test]
    fn vmf_io_unknown_classes_stay_dropped_and_listed() {
        let mut text = format!("world\n{{\n\"classname\" \"worldspawn\"\n{}\n}}\n", cube_solid(2, 0.0, 64.0));
        for (i, class) in ["func_button", "func_door_rotating", "func_rotating", "func_tracktrain", "logic_timer", "math_counter"]
            .iter()
            .enumerate()
        {
            text.push_str(&format!("entity\n{{\n\"classname\" \"{class}\"\n\"targetname\" \"e{i}\"\n}}\n"));
        }
        let d = doc();
        let (_, report) = import_with(&d, &text, &VmfOptions { schema_classes: Some(Vec::new()), ..Default::default() })
            .expect("imports");
        for class in ["func_button", "func_door_rotating", "func_rotating", "func_tracktrain", "logic_timer", "math_counter"] {
            assert_eq!(report.dropped_classes.get(class), Some(&1), "{class} is listed");
        }
        let d = doc();
        let (_, report) = import_with(&d, &text, &VmfOptions::default()).expect("imports");
        assert_eq!(report.frozen_movers.get("func_button"), Some(&1), "without a schema a mover freezes as geometry");
    }

    #[test]
    fn vmf_io_schema_known_brush_classes_stay_passthrough() {
        let text = format!(
            "world\n{{\n\"classname\" \"worldspawn\"\n{}\n}}\n{}{}entity\n{{\n\"classname\" \"path_track\"\n\"targetname\" \"p0\"\n}}\n",
            cube_solid(2, 256.0, 320.0),
            brush_entity("func_breakable", "\"targetname\" \"crate\"\n", 20),
            brush_entity("func_movelinear", "\"targetname\" \"slide\"\n", 30),
        );
        let d = doc();
        let (cmd, report) = import_with(&d, &text, &opts_with_schema()).expect("imports");
        assert!(report.dropped_classes.get("func_breakable").is_none());
        assert!(report.dropped_classes.get("func_movelinear").is_none());
        assert!(report.dropped_classes.get("path_track").is_none());
        let mut d = d;
        d.apply(cmd).unwrap();
        for name in ["crate", "slide"] {
            let e = d.doc.objects.iter().find(|o| o.name == name).expect(name);
            assert!(matches!(e.kind, ObjectKind::Entity(_)), "{name} is passthrough");
            assert_eq!(d.children(e.id).len(), 1, "{name} keeps its brushes");
        }
        let track = d.doc.objects.iter().find(|o| o.name == "p0").expect("the track");
        assert!(matches!(&track.kind, ObjectKind::Entity(e) if e.classname == "path_track"));
    }

    #[test]
    fn vmf_io_repeated_connections_keep_the_first_and_count_the_rest() {
        let trigger = brush_entity(
            "trigger_multiple",
            "\"targetname\" \"button\"\nconnections\n{\n\"OnTrigger\" \"a,Trigger,,0,-1\"\n\"OnTrigger\" \"b,Trigger,,0.5,-1\"\n}\n",
            20,
        );
        let text = format!("world\n{{\n\"classname\" \"worldspawn\"\n{}\n}}\n{trigger}", cube_solid(2, 256.0, 320.0));
        let d = doc();
        let (cmd, report) = import_with(&d, &text, &opts_with_schema()).expect("imports");
        assert_eq!(report.dropped_outputs.get("trigger_multiple.OnTrigger"), Some(&1));
        let mut d = d;
        d.apply(cmd).unwrap();
        let button = d.doc.objects.iter().find(|o| o.name == "button").expect("the button");
        let ObjectKind::Volume(v) = &button.kind else { panic!("the button is a volume") };
        assert_eq!(v.keyvalues.get("OnTrigger").map(String::as_str), Some("a,Trigger,,0,-1"));
    }

    #[test]
    fn vmf_io_bsp_style_plain_output_keys_carry_over() {
        let trigger = brush_entity(
            "trigger_multiple",
            "\"targetname\" \"button\"\n\"OnStartTouch\" \"relay1\x1bTrigger\x1b\x1b0\x1b-1\"\n",
            20,
        );
        let text = format!("world\n{{\n\"classname\" \"worldspawn\"\n{}\n}}\n{trigger}", cube_solid(2, 256.0, 320.0));
        let d = doc();
        let (cmd, report) = import_with(&d, &text, &opts_with_schema()).expect("imports");
        assert!(report.dropped_keys.keys().all(|k| !k.contains("OnStartTouch")), "outputs are not dropped keys");
        let mut d = d;
        d.apply(cmd).unwrap();
        let button = d.doc.objects.iter().find(|o| o.name == "button").expect("the button");
        let ObjectKind::Volume(v) = &button.kind else { panic!("the button is a volume") };
        assert_eq!(v.keyvalues.get("OnStartTouch").map(String::as_str), Some("relay1,Trigger,,0,-1"));
    }
}


