//! Render mesh emission: the vertex pools, the per-material mesh runs and the
//! model render ranges.
//!
//! Lumps written: 0x02 TEXTURE_DATA, 0x03 VERTICES, 0x0E MODELS (render
//! fields), 0x0F SURFACE_NAMES, 0x1E VERTEX_NORMALS, 0x47..0x4A the vertex
//! formats, 0x4F MESH_INDICES, 0x50 MESHES, 0x51 MESH_BOUNDS and 0x52
//! MATERIAL_SORT.

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use glam::{Vec2, Vec3, Vec4};
use rbsp::lumps::collision::Model;
use rbsp::lumps::prims::Vec3f;
use rbsp::lumps::render::{
    MaterialSort, Mesh, MeshBounds, MeshIndex, TexData, VertexLitBump, VertexLitFlat, VertexUnlit,
    VertexUnlitTs, VERTEX_TYPE_LIT_BUMP, VERTEX_TYPE_LIT_FLAT, VERTEX_TYPE_UNLIT, VERTEX_TYPE_UNLIT_TS,
};
use rbsp::lumps::strings::StringTable;
use rbsp::{ids, Bsp, Result as BspResult, Version};
use rir::{AssetTables, MaterialInfo, RenderSurface, VertexFormat};
use thiserror::Error;

/// Flag bits the shipped maps set on every mesh of each class;
/// `mesh_flags_match_fixtures` rebuilds this table by scanning them. Bit 9
/// separates a lit mesh from a tangent-space one, bit 10 names the tangent
/// format, and bit 16 marks a mesh the engine's own compiler wrote; a mesh with
/// no lightmap needs none of them.
pub const MESH_FLAG_LIT: u32 = 0x0000_0200;
pub const MESH_FLAG_UNLIT_TS: u32 = 0x0000_0600;
pub const MESH_FLAG_PLAIN: u32 = 0x0000_0400;
/// Set on every mesh of the shipped S21 client map in `agent-kit/` except its
/// skybox (4640 of 4645 meshes in staging_mu1), lit and unlit alike. Older
/// maps in the wider fixture set do not all carry it, which is why a table
/// built by intersecting every fixture lost it.
pub const MESH_FLAG_WORLD: u32 = 0x0010_0000;
/// Marks a mesh the skybox shows through. A sky mesh carries this with
/// [`MESH_FLAG_PLAIN`] and without [`MESH_FLAG_WORLD`], 0x404 on every one in
/// staging_mu1.
pub const MESH_FLAG_SKY: u32 = 0x0000_0004;
/// Set on every decal-range mesh of staging_mu1 (0x100210).
pub const MESH_FLAG_DECAL: u32 = 0x0000_0010;
/// The material a face uses to show the skybox through it.
pub const SKY_MATERIAL: &str = "tools/toolsskybox";
/// `TEXTURE_DATA` for the sky material, as shipped maps write it.
pub const SKY_TEXDATA_SIZE: i32 = 128;
pub const SKY_TEXDATA_FLAGS: u32 = 0x404;

/// Whether a material is the one the skybox shows through.
pub fn is_sky_material(material: &str) -> bool {
    material.replace('\\', "/").eq_ignore_ascii_case(SKY_MATERIAL)
}

/// `lightmap_scale` on a mesh no lightmap covers.
pub const LIGHTMAP_EXTENTS_NONE: u16 = 0xFFFF;
/// A mesh addresses its vertices through a u16 index, so this is the ceiling.
pub const MAX_MESH_VERTICES: usize = 0x1_0000;
/// `triangle_count` is a u16.
pub const MAX_MESH_TRIANGLES: usize = 0xFFFF;
/// The texture size a `TEXTURE_DATA` record declares: the tile a brush face's
/// texture coordinates are measured in, and what shipped maps write for the
/// dev materials.
pub const TEXDATA_SIZE: i32 = rdoc::model::UV_TEXELS_PER_TILE as i32;
/// `TEXTURE_DATA` flags: bit 9 marks a material a mesh draws with, bit 4 a
/// translucent one (0x210 on every decal of staging_mu1, 0x200 on its opaque
/// world materials).
pub const TEXDATA_DRAW: u32 = 0x200;
pub const TEXDATA_TRANSLUCENT: u32 = 0x10;
pub const TEXDATA_NODRAW: u32 = 0x80;

#[derive(Debug, Error)]
pub enum MeshError {
    #[error("surface {0} is not finite")]
    NotFinite(usize),
    #[error("surface {0} has an index that addresses no vertex")]
    SurfaceIndex(usize),
    #[error("surface {0} has {1} indices, not a multiple of three")]
    SurfaceTriangles(usize, usize),
    #[error("lump 0x{id:02x} of the map read is {reason}")]
    Lump { id: u16, reason: String },
    #[error("mesh {0} names vertex format {1}, which has no record layout")]
    VertexFormat(usize, u32),
    #[error("lump 0x50 declares {0} meshes but 0x51 declares {1} bounds")]
    BoundsCount(usize, usize),
    #[error("mesh {0} index run is not a multiple of three: {1}")]
    IndexRun(usize, usize),
    #[error("a vertex pool is full: {0} records is more than the {1} a mesh run can address")]
    PoolFull(usize, usize),
    #[error("surface {0} has a mesh run with no geometry")]
    EmptyRun(usize),
}

/// One emitted mesh, as the lumps record it.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshRef {
    pub surface: usize,
    pub first_index: u32,
    pub triangle_count: u16,
    pub first_vertex: u16,
    pub vertex_count: u16,
    pub vertex_type: u8,
    pub material_sort: u16,
    pub flags: u32,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
}

/// What the later stages need to find geometry again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderIndex {
    /// First mesh of each input surface, or `u32::MAX` when it emitted none.
    pub mesh_of_surface: Vec<u32>,
    pub meshes: Vec<MeshRef>,
    pub model_first_mesh: Vec<u32>,
    pub model_mesh_count: Vec<u32>,
    pub model_bounds: Vec<(Vec3, Vec3)>,
    /// Material path per sort, in sort order.
    pub sort_material: Vec<String>,
    /// The vertex window each sort's indices are relative to. The pool never
    /// grows past one window, so every sort shares window 0.
    pub sort_first_vertex: Vec<u32>,
    /// The highest vertex index each sort's meshes reach, which is what the
    /// sort's `lightmap_vertex_count` holds.
    pub sort_last_vertex: Vec<u32>,
    /// World meshes are opaque up to here; decals follow, drawn in mesh order.
    pub first_decal_mesh: u32,
    /// Translucent world meshes start here.
    pub first_transparent_mesh: u32,
    /// World meshes end here and the world's sky meshes follow.
    pub first_sky_mesh: u32,
}

impl RenderIndex {
    pub fn model_count(&self) -> usize {
        self.model_first_mesh.len()
    }

    /// Meshes of one model, in mesh order.
    pub fn model_meshes(&self, model: u32) -> &[MeshRef] {
        let a = self.model_first_mesh.get(model as usize).copied().unwrap_or(0) as usize;
        let n = self.model_mesh_count.get(model as usize).copied().unwrap_or(0) as usize;
        self.meshes.get(a..a + n).unwrap_or(&[])
    }
}

/// The vertex kind a mesh's material draws with.
pub fn vertex_kind(format: VertexFormat) -> u32 {
    match format {
        VertexFormat::Unlit => VERTEX_TYPE_UNLIT,
        VertexFormat::LitFlat => VERTEX_TYPE_LIT_FLAT,
        VertexFormat::LitBump => VERTEX_TYPE_LIT_BUMP,
        VertexFormat::UnlitTs => VERTEX_TYPE_UNLIT_TS,
    }
}

/// Flag bits a mesh of this class carries.
pub fn mesh_flags(format: VertexFormat, lightmapped: bool) -> u32 {
    MESH_FLAG_WORLD
        | match format {
            VertexFormat::LitFlat | VertexFormat::LitBump if lightmapped => MESH_FLAG_LIT,
            VertexFormat::UnlitTs => MESH_FLAG_UNLIT_TS,
            _ => MESH_FLAG_PLAIN,
        }
}

/// Whether a mesh of this class carries a lightmap.
pub fn is_lightmapped(format: VertexFormat) -> bool {
    matches!(format, VertexFormat::LitFlat | VertexFormat::LitBump)
}

/// `TEXTURE_DATA` flags for a material.
pub fn texdata_flags(info: Option<&MaterialInfo>) -> u32 {
    let mut f = TEXDATA_DRAW;
    if info.is_some_and(|m| m.translucent) {
        f |= TEXDATA_TRANSLUCENT;
    }
    if info.is_some_and(|m| m.nodraw) {
        f |= TEXDATA_NODRAW;
    }
    f
}

/// Records a finished sort's vertex window and moves the class boundaries
/// past it.
fn finish_sort(
    index: &mut RenderIndex,
    si: usize,
    sort_base: u32,
    sort_last: u32,
    sort_model: u32,
    sort_class: u8,
    mesh_count: u32,
) {
    index.sort_first_vertex[si] = sort_base;
    index.sort_last_vertex[si] = sort_last;
    if sort_model == 0 && sort_class == 0 {
        index.first_decal_mesh = mesh_count;
    }
    if sort_model == 0 && sort_class < 2 {
        index.first_transparent_mesh = mesh_count;
    }
    if sort_model == 0 && sort_class < 3 {
        index.first_sky_mesh = mesh_count;
    }
}

fn version_of(bsp: &Bsp) -> Version {
    bsp.version_kind().unwrap_or(Version::ClientV51)
}

fn empty_model() -> Model {
    Model {
        mins: [0.0; 3],
        maxs: [0.0; 3],
        first_mesh: 0,
        num_meshes: 0,
        bvh_node: -1,
        bvh_leaf: -1,
        vert_index: 0,
        flags: 0,
        origin: [0.0; 3],
        decode_scale: 0.0,
    }
}

/// Reads the models lump, so a stage that already wrote collision keeps it.
fn existing_models(bsp: &Bsp) -> BspResult<Vec<Model>> {
    if bsp.lump(ids::MODELS).is_empty() {
        return Ok(Vec::new());
    }
    rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), version_of(bsp))
}

fn lump_err(id: u16, e: impl std::fmt::Display) -> MeshError {
    MeshError::Lump { id, reason: e.to_string() }
}

/// A mesh run: the surface's triangles re-indexed into one vertex block.
struct Run {
    /// One local slot per triangle corner, three per triangle.
    locals: Vec<u32>,
    triangles: usize,
    /// Distinct surface vertices this run uses, in first-use order.
    used: Vec<u32>,
    /// Pool slot of each entry of `used`, filled by the emitter.
    local: Vec<u32>,
    lo: Vec3,
    hi: Vec3,
}

/// Splits a surface's triangle list into runs a single mesh can address.
fn split_indices(
    indices: &[u32],
    vertex_count: usize,
    surface: usize,
) -> Result<Vec<Run>, MeshError> {
    if !indices.len().is_multiple_of(3) {
        return Err(MeshError::SurfaceTriangles(surface, indices.len()));
    }
    for &i in indices {
        if i as usize >= vertex_count {
            return Err(MeshError::SurfaceIndex(surface));
        }
    }
    let mut out: Vec<Run> = Vec::new();
    let mut locals: Vec<u32> = Vec::new();
    let mut used: Vec<u32> = Vec::with_capacity(MAX_MESH_VERTICES.min(4096));
    let mut slot: HashMap<u32, u32> = HashMap::new();
    for t in indices.chunks_exact(3) {
        let full = used.len() + t.iter().filter(|i| !slot.contains_key(i)).count();
        // One below the cap leaves room for the pad triangle `finish_run` may add.
        if !used.is_empty() && (full > MAX_MESH_VERTICES || locals.len() / 3 >= MAX_MESH_TRIANGLES - 1) {
            out.push(finish_run(&mut locals, &mut used, &mut slot));
        }
        for &i in t {
            let next = used.len() as u32;
            let s = *slot.entry(i).or_insert_with(|| {
                used.push(i);
                next
            });
            locals.push(s);
        }
    }
    if !locals.is_empty() {
        out.push(finish_run(&mut locals, &mut used, &mut slot));
    }
    Ok(out)
}

/// The engine loads a mesh's triangle count rounded up to even and walks that
/// many, so an odd run gets a degenerate triangle; without it the decal clipper
/// reads the next mesh's first triangle against this mesh's vertex window.
fn finish_run(locals: &mut Vec<u32>, used: &mut Vec<u32>, slot: &mut HashMap<u32, u32>) -> Run {
    if !(locals.len() / 3).is_multiple_of(2) {
        let c = locals[locals.len() - 3];
        locals.extend_from_slice(&[c, c, c]);
    }
    let r = Run {
        triangles: locals.len() / 3,
        locals: std::mem::take(locals),
        used: std::mem::take(used),
        local: Vec::new(),
        lo: Vec3::ZERO,
        hi: Vec3::ZERO,
    };
    slot.clear();
    r
}

/// One surface vertex in a pool's own terms. Every format stores the same
/// fields, in this order, and drops the ones it has no use for: `pos`, `nrm`,
/// the uv pair as two f32, the colour as RGBA bytes, then the lightmap pair and
/// the tangent, which only the lit formats and the tangent-space one carry.
type VertexKey = [u32; 8];
const K_POSITION: usize = 0;
const K_NORMAL: usize = 1;
const K_UV_U: usize = 2;
const K_UV_V: usize = 3;
const K_COLOUR: usize = 4;
const K_LIGHTMAP_U: usize = 5;
const K_LIGHTMAP_V: usize = 6;
const K_TANGENT: usize = 7;

/// The four vertex pools, one per record layout. A mesh addresses its run
/// through a u16 `first_vertex`, so each mesh gets a contiguous block of one
/// pool; a block an earlier mesh already laid down is reused when it holds the
/// same record run, which is how a map keeps the pools inside that ceiling.
#[derive(Default)]
struct Pools {
    unlit: Vec<VertexUnlit>,
    lit_flat: Vec<VertexLitFlat>,
    lit_bump: Vec<VertexLitBump>,
    unlit_ts: Vec<VertexUnlitTs>,
    seen: HashMap<VertexFormat, HashMap<VertexKey, u32>>,
}

impl Pools {
    fn len(&self, format: VertexFormat) -> usize {
        match format {
            VertexFormat::Unlit => self.unlit.len(),
            VertexFormat::LitFlat => self.lit_flat.len(),
            VertexFormat::LitBump => self.lit_bump.len(),
            VertexFormat::UnlitTs => self.unlit_ts.len(),
        }
    }

    fn push(&mut self, format: VertexFormat, key: VertexKey) -> u32 {
        let at = self.len(format) as u32;
        match format {
            VertexFormat::Unlit => self.unlit.push(VertexUnlit {
                position_index: key[K_POSITION],
                normal_index: key[K_NORMAL],
                unknown_0: [key[K_UV_U], key[K_UV_V], key[K_COLOUR]],
            }),
            VertexFormat::LitFlat => self.lit_flat.push(VertexLitFlat {
                position_index: key[K_POSITION],
                normal_index: key[K_NORMAL],
                unknown_0: [
                    key[K_UV_U],
                    key[K_UV_V],
                    key[K_COLOUR],
                    key[K_LIGHTMAP_U],
                    key[K_LIGHTMAP_V],
                ],
            }),
            VertexFormat::LitBump => self.lit_bump.push(VertexLitBump {
                position_index: key[K_POSITION],
                normal_index: key[K_NORMAL],
                unknown_0: [
                    key[K_UV_U],
                    key[K_UV_V],
                    key[K_COLOUR],
                    key[K_LIGHTMAP_U],
                    key[K_LIGHTMAP_V],
                ],
                tangent_index: key[K_TANGENT],
            }),
            VertexFormat::UnlitTs => self.unlit_ts.push(VertexUnlitTs {
                position_index: key[K_POSITION],
                normal_index: key[K_NORMAL],
                unknown_0: [key[K_UV_U], key[K_UV_V], key[K_COLOUR]],
                tangent_index: key[K_TANGENT],
            }),
        }
        self.seen.entry(format).or_default().insert(key, at);
        at
    }

    /// One mesh's records as a contiguous block, and the pool slot of each.
    /// Lays a run's records down, or finds them already laid down, at or
    /// after `base`, the first slot of the run's material sort: a mesh counts
    /// its vertices from its sort's first vertex, so a sort's runs have to sit
    /// within 65536 records of it.
    fn intern_block(
        &mut self,
        format: VertexFormat,
        keys: &[VertexKey],
        base: u32,
    ) -> Result<(u32, Vec<u32>), MeshError> {
        if keys.is_empty() {
            return Err(MeshError::EmptyRun(0));
        }
        if let Some(seen) = self.seen.get(&format) {
            if let Some(&start) = seen.get(&keys[0]) {
                let reuse = start >= base
                    && keys
                        .iter()
                        .enumerate()
                        .all(|(j, k)| seen.get(k) == Some(&(start + j as u32)));
                if reuse {
                    return Ok((start, (0..keys.len() as u32).map(|j| start + j).collect()));
                }
            }
        }
        let first = self.len(format);
        if first + keys.len() - base as usize > MAX_MESH_VERTICES {
            return Err(MeshError::PoolFull(first + keys.len() - base as usize, MAX_MESH_VERTICES));
        }
        let mut slots = Vec::with_capacity(keys.len());
        for k in keys {
            let at = self.push(format, *k);
            slots.push(at);
        }
        Ok((first as u32, slots))
    }

    fn write(&self, bsp: &mut Bsp, v: Version) {
        use rbsp::lumps::render as w;
        bsp.set_lump(ids::VERTEX_UNLIT, w::write_vertex_unlit(&self.unlit, v));
        bsp.set_lump(ids::VERTEX_LIT_FLAT, w::write_vertex_lit_flat(&self.lit_flat, v));
        bsp.set_lump(ids::VERTEX_LIT_BUMP, w::write_vertex_lit_bump(&self.lit_bump, v));
        bsp.set_lump(ids::VERTEX_UNLIT_TS, w::write_vertex_unlit_ts(&self.unlit_ts, v));
    }
}

fn key3(v: Vec3) -> [u32; 3] {
    [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
}

/// One surface vertex, in every pool's own terms.
struct Vertex {
    position: Vec3,
    normal: Vec3,
    tangent: Option<Vec4>,
    uv0: Vec2,
    color: [u8; 4],
    uv1: Vec2,
}

fn surface_vertex(s: &RenderSurface, i: usize) -> Vertex {
    Vertex {
        position: s.positions.get(i).copied().unwrap_or(Vec3::ZERO),
        normal: s.normals.get(i).copied().unwrap_or(Vec3::Z),
        tangent: s.tangents.get(i).copied(),
        uv0: s.uv0.get(i).copied().unwrap_or(Vec2::ZERO),
        color: s.colors.get(i).copied().unwrap_or([255; 4]),
        uv1: s.uv1.get(i).copied().unwrap_or(Vec2::ZERO),
    }
}

/// Writes the render lumps for every surface and returns the index.
pub fn emit_render(
    surfaces: &[RenderSurface],
    assets: &AssetTables,
    bsp: &mut Bsp,
) -> Result<RenderIndex, MeshError> {
    emit_render_paged(surfaces, &vec![-1; surfaces.len()], assets, bsp)
}

/// [`emit_render`] with the lightmap page each surface's chart sits on, or -1
/// for none. Material sorts are keyed by material and page, the way the shipped
/// maps split them: staging_mu1 has 130 sorts over 129 materials, and every one
/// of its 4637 lit meshes names a page through its sort. A lit mesh whose sort
/// says -1 samples no lightmap at all, and no shipped map carries one.
pub fn emit_render_paged(
    surfaces: &[RenderSurface],
    pages: &[i16],
    assets: &AssetTables,
    bsp: &mut Bsp,
) -> Result<RenderIndex, MeshError> {
    let v = version_of(bsp);
    for (i, s) in surfaces.iter().enumerate() {
        if s.positions.iter().any(|p| !p.is_finite())
            || s.normals.iter().any(|n| !n.is_finite())
            || s.uv0.iter().any(|u| !u.is_finite())
            || s.uv1.iter().any(|u| !u.is_finite())
        {
            return Err(MeshError::NotFinite(i));
        }
    }

    // A sky face is drawn unlit whatever its material asset says.
    let format_of = |material: &str| -> VertexFormat {
        if is_sky_material(material) {
            return VertexFormat::Unlit;
        }
        assets.materials.get(material).map(|m| m.vertex_format).unwrap_or(VertexFormat::LitBump)
    };
    let page_of = |fi: usize, material: &str| -> i16 {
        let format = format_of(material);
        if is_lightmapped(format) {
            pages.get(fi).copied().unwrap_or(-1)
        } else {
            -1
        }
    };
    // Sorts run model by model, then opaque, decal, translucent and sky, so every
    // model's meshes are one contiguous run and the world's split the way the
    // level info's mesh ranges describe.
    let class = |material: &str| -> u8 {
        let info = assets.materials.get(material);
        if is_sky_material(material) {
            3
        } else if info.is_some_and(|m| m.decal) {
            1
        } else if info.is_some_and(|m| m.translucent) {
            2
        } else {
            0
        }
    };
    let mut sort_keys: Vec<(u32, u8, String, i16)> = Vec::new();
    for (fi, s) in surfaces.iter().enumerate() {
        let key = (s.model, class(&s.material), s.material.clone(), page_of(fi, &s.material));
        if !sort_keys.contains(&key) {
            sort_keys.push(key);
        }
    }
    sort_keys.sort_by_key(|k| (k.0, k.1));
    let mut sorts: Vec<String> = sort_keys.iter().map(|k| k.2.clone()).collect();
    let model_count = surfaces.iter().map(|s| s.model as usize + 1).max().unwrap_or(1);

    let mut index = RenderIndex {
        mesh_of_surface: vec![u32::MAX; surfaces.len()],
        meshes: Vec::new(),
        model_first_mesh: vec![0; model_count],
        model_mesh_count: vec![0; model_count],
        model_bounds: vec![(Vec3::ZERO, Vec3::ZERO); model_count],
        sort_material: sorts.clone(),
        sort_first_vertex: vec![0; sorts.len()],
        sort_last_vertex: vec![0; sorts.len()],
        first_decal_mesh: 0,
        first_transparent_mesh: 0,
        first_sky_mesh: 0,
    };

    let mut pools = Pools::default();
    let mut verts: Vec<Vec3f> = Vec::new();
    let mut vert_index: HashMap<[u32; 3], u32> = HashMap::new();
    let mut norms: Vec<Vec3f> = Vec::new();
    let mut norm_index: HashMap<[u32; 3], u32> = HashMap::new();
    let mut mesh_indices: Vec<MeshIndex> = Vec::new();
    let mut meshes: Vec<Mesh> = Vec::new();
    let mut mesh_bounds: Vec<MeshBounds> = Vec::new();
    let mut model_lo = vec![Vec3::splat(f32::INFINITY); model_count];
    let mut model_hi = vec![Vec3::splat(f32::NEG_INFINITY); model_count];

    let mut si = 0usize;
    while si < sorts.len() {
        let material = sorts[si].clone();
        let mut sort = si as u16;
        let format = format_of(&material);
        let kind = vertex_kind(format);
        let lightmapped = is_lightmapped(format);
        let mut flags = if is_sky_material(&material) { MESH_FLAG_PLAIN | MESH_FLAG_SKY } else { mesh_flags(format, lightmapped) };
        if sort_keys[si].1 == 1 {
            flags |= MESH_FLAG_DECAL;
        }
        let mut sort_last = 0u32;
        let (sort_model, sort_class, page) = (sort_keys[si].0, sort_keys[si].1, sort_keys[si].3);
        // Lit sorts get their own window, the shipped layout. Unlit sorts stay
        // at 0 like every shipped one, whether or not the engine offsets them.
        // A sort that would run past its 16-bit window opens a new sort with
        // the same key, the way shipped maps repeat a material across sorts.
        let mut sort_base = if page >= 0 { pools.len(format) as u32 } else { 0 };
        for (fi, s) in surfaces.iter().enumerate() {
            if s.model != sort_model || s.material != material || page_of(fi, &s.material) != page {
                continue;
            }
            if index.mesh_of_surface[fi] == u32::MAX {
                index.mesh_of_surface[fi] = meshes.len() as u32;
            }
            for mut run in split_indices(&s.indices, s.positions.len(), fi)? {
                let mut keys: Vec<VertexKey> = Vec::with_capacity(run.used.len());
                for &u in &run.used {
                    let vtx = surface_vertex(s, u as usize);
                    let pi = intern(&mut verts, &mut vert_index, vtx.position);
                    let ni = intern(&mut norms, &mut norm_index, vtx.normal);
                    let tangent = match (format, vtx.tangent) {
                        (VertexFormat::LitBump, Some(t)) | (VertexFormat::UnlitTs, Some(t)) => {
                            let w = if t.w < 0.0 { 0x8000_0000u32 } else { 0 };
                            Some(intern(&mut norms, &mut norm_index, t.truncate()) | w)
                        }
                        _ => None,
                    };
                    let mut key = [0u32; 8];
                    key[K_POSITION] = pi;
                    key[K_NORMAL] = ni;
                    key[K_UV_U] = vtx.uv0.x.to_bits();
                    key[K_UV_V] = vtx.uv0.y.to_bits();
                    key[K_COLOUR] = u32::from_le_bytes(vtx.color);
                    key[K_LIGHTMAP_U] = vtx.uv1.x.to_bits();
                    key[K_LIGHTMAP_V] = vtx.uv1.y.to_bits();
                    key[K_TANGENT] = tangent.unwrap_or(0);
                    keys.push(key);
                }
                if keys.is_empty() {
                    return Err(MeshError::EmptyRun(fi));
                }
                if page >= 0 && pools.len(format) + keys.len() > sort_base as usize + MAX_MESH_VERTICES {
                    finish_sort(&mut index, si, sort_base, sort_last, sort_model, sort_class, meshes.len() as u32);
                    sorts.insert(si + 1, material.clone());
                    sort_keys.insert(si + 1, sort_keys[si].clone());
                    index.sort_material.insert(si + 1, material.clone());
                    index.sort_first_vertex.insert(si + 1, 0);
                    index.sort_last_vertex.insert(si + 1, 0);
                    si += 1;
                    sort = si as u16;
                    sort_base = pools.len(format) as u32;
                    sort_last = 0;
                }
                let (first_vertex, slot_of) = pools.intern_block(format, &keys, sort_base)?;
                let first_vertex = first_vertex - sort_base;
                // The block is laid down in the run's own first-use order, so a
                // corner's local slot is its offset into the block; an index
                // counts from the sort's first vertex.
                run.local = run.locals.iter().map(|&l| slot_of[l as usize] - sort_base).collect();
                if run.local.iter().any(|&c| c > u16::MAX as u32) {
                    return Err(MeshError::PoolFull(pools.len(format) - sort_base as usize, MAX_MESH_VERTICES));
                }
                let first_index = mesh_indices.len() as u32;
                for t in run.local.chunks_exact(3) {
                    for &c in t {
                        mesh_indices.push(MeshIndex { value: c as u16 });
                    }
                }
                let mut lo = Vec3::splat(f32::INFINITY);
                let mut hi = Vec3::splat(f32::NEG_INFINITY);
                for &u in &run.used {
                    let p = s.positions[u as usize];
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
                if lo.x > hi.x {
                    return Err(MeshError::EmptyRun(fi));
                }
                run.lo = lo;
                run.hi = hi;
                let count = run.used.len() as u16;
                // A mesh names its lowest vertex and how far past it the
                // highest one lies, both relative to the sort's window.
                meshes.push(Mesh {
                    first_index,
                    triangle_count: run.triangles as u16,
                    first_vertex: first_vertex as u16,
                    lightmap_vertex_count: count - 1,
                    vertex_type: kind as u8,
                    // Shipped sky meshes sample no cubemap.
                    cube_index: if flags & MESH_FLAG_SKY != 0 { 0xFF } else { 0 },
                    unknown_0: [0; 4],
                    lightmap_scale: if lightmapped { [0, 0] } else { [LIGHTMAP_EXTENTS_NONE; 2] },
                    fade_distance: 0,
                    material_sort: sort,
                    flags,
                });
                let centre = (lo + hi) * 0.5;
                let radius = run.used.iter().map(|&u| s.positions[u as usize].distance(centre)).fold(0.0f32, f32::max);
                mesh_bounds.push(MeshBounds {
                    origin: centre.to_array(),
                    radius,
                    extents: ((hi - lo) * 0.5).to_array(),
                    tan_yaw: 0.0,
                });
                index.meshes.push(MeshRef {
                    surface: fi,
                    first_index,
                    triangle_count: run.triangles as u16,
                    first_vertex: first_vertex as u16,
                    vertex_count: count,
                    vertex_type: kind as u8,
                    material_sort: sort,
                    flags,
                    bounds_min: lo,
                    bounds_max: hi,
                });
                let model = s.model as usize;
                model_lo[model] = model_lo[model].min(lo);
                model_hi[model] = model_hi[model].max(hi);
                index.model_mesh_count[model] += 1;
                sort_last = sort_last.max(first_vertex + count as u32 - 1);
            }
        }
        finish_sort(&mut index, si, sort_base, sort_last, sort_model, sort_class, meshes.len() as u32);
        si += 1;
    }

    let mut next = 0u32;
    for model in 0..model_count {
        index.model_first_mesh[model] = next;
        if model_lo[model].x > model_hi[model].x {
            model_lo[model] = Vec3::ZERO;
            model_hi[model] = Vec3::ZERO;
        }
        index.model_bounds[model] = (model_lo[model], model_hi[model]);
        next += index.model_mesh_count[model];
    }

    let mut names = StringTable::default();
    let mut materials: Vec<String> = Vec::new();
    for m in &sorts {
        if !materials.contains(m) {
            materials.push(m.clone());
        }
    }
    let mut texdata: Vec<TexData> = Vec::with_capacity(materials.len());
    for material in &materials {
        let at = names.offset_of(names.strings.len());
        names.strings.push(material.clone());
        let sky = is_sky_material(material);
        texdata.push(TexData {
            name: at as u32,
            width: if sky { SKY_TEXDATA_SIZE } else { TEXDATA_SIZE },
            height: if sky { SKY_TEXDATA_SIZE } else { TEXDATA_SIZE },
            flags: if sky { SKY_TEXDATA_FLAGS } else { texdata_flags(assets.materials.get(material)) },
        });
    }
    let sort_lumps: Vec<MaterialSort> = (0..sorts.len())
        .map(|si| MaterialSort {
            tex_data: materials.iter().position(|m| *m == sorts[si]).unwrap_or(0) as u16,
            lightmap: sort_keys[si].3,
            unknown_0: 0,
            lightmap_vertex_count: index.sort_last_vertex[si].min(u16::MAX as u32) as u16,
            first_vertex: index.sort_first_vertex[si],
        })
        .collect();

    use rbsp::lumps::render as w;
    bsp.set_lump(ids::VERTICES, rbsp::lumps::prims::write(&verts, v));
    bsp.set_lump(ids::VERTEX_NORMALS, rbsp::lumps::prims::write(&norms, v));
    bsp.set_lump(ids::TEXTURE_DATA, w::write_texdata(&texdata, v));
    bsp.set_lump(ids::SURFACE_NAMES, w::write_surface_names(&names, v));
    bsp.set_lump(ids::MESH_INDICES, w::write_mesh_indices(&mesh_indices, v));
    bsp.set_lump(ids::MESHES, w::write_meshes(&meshes, v));
    bsp.set_lump(ids::MESH_BOUNDS, w::write_mesh_bounds(&mesh_bounds, v));
    bsp.set_lump(ids::MATERIAL_SORT, w::write_material_sorts(&sort_lumps, v));
    pools.write(bsp, v);

    let mut models = existing_models(bsp).map_err(|e| lump_err(ids::MODELS, e))?;
    while models.len() < model_count {
        models.push(empty_model());
    }
    for (model, e) in models.iter_mut().enumerate().take(model_count) {
        let (lo, hi) = index.model_bounds[model];
        e.mins = lo.to_array();
        e.maxs = hi.to_array();
        e.first_mesh = index.model_first_mesh[model];
        e.num_meshes = index.model_mesh_count[model];
        if e.bvh_node == 0 && e.bvh_leaf == 0 && e.decode_scale == 0.0 {
            e.bvh_node = -1;
            e.bvh_leaf = -1;
            e.vert_index = 0;
        }
    }
    bsp.set_lump(ids::MODELS, rbsp::lumps::collision::write_models(&models, v));
    Ok(index)
}

fn intern(pool: &mut Vec<Vec3f>, map: &mut HashMap<[u32; 3], u32>, v: Vec3) -> u32 {
    let k = key3(v);
    if let Some(i) = map.get(&k) {
        return *i;
    }
    let i = pool.len() as u32;
    pool.push(Vec3f::new(v.x, v.y, v.z));
    map.insert(k, i);
    i
}

/// A uv pair stored the way every vertex format holds it: two f32.
fn uv_from_bits(bits: [u32; 2]) -> Vec2 {
    Vec2::new(f32::from_bits(bits[0]), f32::from_bits(bits[1]))
}

// ---- decoding ---------------------------------------------------------------

/// A decoded mesh run, as the lumps hold it.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedRun {
    pub model: u32,
    pub material: String,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub tangents: Vec<Option<Vec4>>,
    pub uv0: Vec<Vec2>,
    pub uv1: Vec<Vec2>,
    pub colors: Vec<[u8; 4]>,
    pub indices: Vec<u32>,
    pub vertex_type: u8,
    pub flags: u32,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
}

fn f(v: &[[f32; 3]], i: u32) -> Option<Vec3> {
    let a = v.get(i as usize)?;
    Some(Vec3::new(a[0], a[1], a[2]))
}

/// The f32 pair a vertex record holds at `at`, or zeroes when the record is
/// short enough not to carry one.
fn uv_of(rec: &[u8], at: usize) -> (f32, f32) {
    if rec.len() < at + 8 {
        return (0.0, 0.0);
    }
    let pair = [
        u32::from_le_bytes([rec[at], rec[at + 1], rec[at + 2], rec[at + 3]]),
        u32::from_le_bytes([rec[at + 4], rec[at + 5], rec[at + 6], rec[at + 7]]),
    ];
    let uv = uv_from_bits(pair);
    (uv.x, uv.y)
}

fn parts(b: &[u8], size: usize) -> Vec<&[u8]> {
    b.chunks_exact(size).collect()
}

/// Decodes the render meshes of a map into per-mesh geometry.
pub fn decode_meshes(bsp: &Bsp) -> Result<Vec<DecodedRun>, MeshError> {
    use rbsp::lumps::render as r;
    let v = version_of(bsp);
    if bsp.lump(ids::MESHES).is_empty() {
        return Ok(Vec::new());
    }
    let verts: Vec<[f32; 3]> = rbsp::lumps::prims::parse(bsp.lump(ids::VERTICES), v)
        .map_err(|e| lump_err(ids::VERTICES, e))?
        .iter()
        .map(|p| [p.x, p.y, p.z])
        .collect();
    let norms: Vec<[f32; 3]> = rbsp::lumps::prims::parse(bsp.lump(ids::VERTEX_NORMALS), v)
        .map_err(|e| lump_err(ids::VERTEX_NORMALS, e))?
        .iter()
        .map(|p| [p.x, p.y, p.z])
        .collect();
    let meshes = r::parse_meshes(bsp.lump(ids::MESHES), v).map_err(|e| lump_err(ids::MESHES, e))?;
    let bounds = r::parse_mesh_bounds(bsp.lump(ids::MESH_BOUNDS), v).map_err(|e| lump_err(ids::MESH_BOUNDS, e))?;
    if meshes.len() != bounds.len() {
        return Err(MeshError::BoundsCount(meshes.len(), bounds.len()));
    }
    let indices = r::parse_mesh_indices(bsp.lump(ids::MESH_INDICES), v).map_err(|e| lump_err(ids::MESH_INDICES, e))?;
    let sorts = r::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), v).map_err(|e| lump_err(ids::MATERIAL_SORT, e))?;
    let texdata = r::parse_texdata(bsp.lump(ids::TEXTURE_DATA), v).map_err(|e| lump_err(ids::TEXTURE_DATA, e))?;
    let names = r::parse_surface_names(bsp.lump(ids::SURFACE_NAMES), v).map_err(|e| lump_err(ids::SURFACE_NAMES, e))?;
    let models = rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), v)
        .map_err(|e| lump_err(ids::MODELS, e))?;
    let pools: Vec<(u8, Vec<&[u8]>)> = vec![
        (VERTEX_TYPE_UNLIT as u8, parts(bsp.lump(ids::VERTEX_UNLIT), 20)),
        (VERTEX_TYPE_LIT_FLAT as u8, parts(bsp.lump(ids::VERTEX_LIT_FLAT), 28)),
        (VERTEX_TYPE_LIT_BUMP as u8, parts(bsp.lump(ids::VERTEX_LIT_BUMP), 32)),
        (VERTEX_TYPE_UNLIT_TS as u8, parts(bsp.lump(ids::VERTEX_UNLIT_TS), 24)),
    ];

    let mut out: Vec<DecodedRun> = Vec::with_capacity(meshes.len());
    for (mi, m) in meshes.iter().enumerate() {
        let pool = pools
            .iter()
            .find(|(k, _)| *k == (m.vertex_type & 3))
            .map(|(_, p)| p)
            .ok_or(MeshError::VertexFormat(mi, m.vertex_type as u32))?;
        let first = m.first_index as usize;
        let tri = m.triangle_count as usize;
        // Indices are relative to the sort's vertex window. Shipped unlit
        // sorts all sit at 0, so no map tells whether the engine offsets them.
        let base = sorts.get(m.material_sort as usize).map(|s| s.first_vertex).unwrap_or(0);
        // A mesh's vertex run is a contiguous block of the shared pool, and
        // other meshes may hold records inside that block, so the surface keeps
        // only the slots this mesh actually indexes.
        let mut used: Vec<u32> = Vec::new();
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for k in 0..tri * 3 {
            let Some(ix) = indices.get(first + k) else { break };
            let slot = base + ix.get() as u32;
            if seen.insert(slot) {
                used.push(slot);
            }
        }
        used.sort_unstable();
        let local_of: HashMap<u32, u32> = used.iter().enumerate().map(|(i, &s)| (s, i as u32)).collect();
        let count = used.len();
        let mut run = DecodedRun {
            model: 0,
            material: String::new(),
            positions: Vec::with_capacity(count),
            normals: Vec::with_capacity(count),
            tangents: Vec::with_capacity(count),
            uv0: Vec::with_capacity(count),
            uv1: Vec::with_capacity(count),
            colors: Vec::with_capacity(count),
            indices: Vec::new(),
            vertex_type: m.vertex_type & 3,
            flags: m.flags,
            bounds_min: Vec3::ZERO,
            bounds_max: Vec3::ZERO,
        };
        for &slot in &used {
            let Some(rec) = pool.get(slot as usize) else {
                return Err(MeshError::SurfaceIndex(mi));
            };
            let pi = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
            let ni = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]);
            run.positions.push(f(&verts, pi).unwrap_or(Vec3::ZERO));
            run.normals.push(f(&norms, ni).unwrap_or(Vec3::Z));
            let (u0, v0) = uv_of(rec, 8);
            run.uv0.push(Vec2::new(u0, v0));
            run.colors.push([rec[16], rec[17], rec[18], rec[19]]);
            let kind = m.vertex_type & 3;
            let lm_off: i64 = if kind as u32 == VERTEX_TYPE_LIT_FLAT || kind as u32 == VERTEX_TYPE_LIT_BUMP {
                20
            } else {
                -1
            };
            let (lu, lv) = if lm_off >= 0 { uv_of(rec, lm_off as usize) } else { (0.0, 0.0) };
            run.uv1.push(Vec2::new(lu, lv));
            let tang_off = if kind as u32 == VERTEX_TYPE_LIT_BUMP {
                Some(28)
            } else if kind as u32 == VERTEX_TYPE_UNLIT_TS {
                Some(20)
            } else {
                None
            };
            run.tangents.push(tang_off.and_then(|o| {
                if rec.len() < o + 4 {
                    return None;
                }
                let t = u32::from_le_bytes([rec[o], rec[o + 1], rec[o + 2], rec[o + 3]]);
                let w = if t & 0x8000_0000 != 0 { -1.0 } else { 1.0 };
                f(&norms, t & 0x7FFF_FFFF).map(|n| Vec4::new(n.x, n.y, n.z, w))
            }));
        }
        for t in 0..m.triangle_count as usize {
            for k in 0..3 {
                let at = first + t * 3 + k;
                let Some(ix) = indices.get(at) else {
                    return Err(MeshError::IndexRun(mi, at));
                };
                // The index addresses the shared pool; the surface sees it
                // through the compacted run.
                let Some(&local) = local_of.get(&(base + ix.get() as u32)) else {
                    return Err(MeshError::SurfaceIndex(mi));
                };
                run.indices.push(local);
            }
        }
        // The bounds record is a sphere and a box about the centre, not the
        // corners, so the run's box comes from its own vertices.
        if let Some(first) = run.positions.first() {
            let (lo, hi) = run.positions.iter().fold((*first, *first), |(a, b), p| (a.min(*p), b.max(*p)));
            run.bounds_min = lo;
            run.bounds_max = hi;
        }
        let _ = &bounds[mi];
        let sort = sorts.get(m.material_sort as usize);
        if let Some(s) = sort {
            if let Some(td) = texdata.get(s.tex_data as usize) {
                run.material = names.at_offset(td.name as usize).unwrap_or_default().to_string();
            }
        }
        for (i, model) in models.iter().enumerate() {
            let a = model.first_mesh as usize;
            if mi >= a && mi < a + model.num_meshes as usize {
                run.model = i as u32;
                break;
            }
        }
        out.push(run);
    }
    Ok(out)
}

/// The render meshes as render surfaces, one per mesh.
pub fn decode_surfaces(bsp: &Bsp) -> Result<Vec<RenderSurface>, MeshError> {
    Ok(decode_meshes(bsp)?
        .into_iter()
        .map(|m| {
            let mut indices = m.indices;
            indices.truncate(indices.len() - indices.len() % 3);
            RenderSurface {
                object: None,
                model: m.model,
                material: m.material,
                positions: m.positions,
                normals: m.normals,
                tangents: m.tangents.into_iter().map(|t| t.unwrap_or(Vec4::W)).collect(),
                uv0: m.uv0,
                uv1: m.uv1,
                colors: m.colors,
                indices,
                cast_shadows: true,
                lightmap_texel: 16.0,
                lightmap_size: [0, 0],
            }
        })
        .collect())
}

// ---- appending --------------------------------------------------------------

/// One mesh appended to a shipped map: its index in 0x50 and its box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppendedMesh {
    pub index: u32,
    pub mins: Vec3,
    pub maxs: Vec3,
}

/// The record size and the dword offset of the tangent index, per vertex lump.
const APPEND_POOLS: [(u16, usize, Option<usize>); 4] = [
    (ids::VERTEX_UNLIT, 20, None),
    (ids::VERTEX_LIT_FLAT, 28, None),
    (ids::VERTEX_LIT_BUMP, 32, Some(28)),
    (ids::VERTEX_UNLIT_TS, 24, Some(20)),
];

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Adds world-model render meshes to a map whose render lumps are already
/// built, without moving anything the map holds.
///
/// The surfaces are written the way `emit_render` writes a map of their own,
/// then every table is appended to the map's: positions and normals, each
/// vertex lump, the index stream, the material sorts, the texdata (a material
/// the map already names is reused, a new one is added to the name blob at its
/// end), the meshes and their bounds. A new sort starts at the first appended
/// vertex of its lump, so the indices need no rewriting. The meshes go at the
/// end of the table and model 0 grows over them, which only works while every
/// other model is mesh-less, as in a shipped map.
pub fn append_render(bsp: &mut Bsp, surfaces: &[RenderSurface], assets: &AssetTables) -> Result<Vec<AppendedMesh>, MeshError> {
    use rbsp::lumps::render as r;
    if surfaces.is_empty() {
        return Ok(Vec::new());
    }
    let v = version_of(bsp);
    let mut part = Bsp::new(bsp.header.version, bsp.header.flags, bsp.header.revision);
    let index = emit_render(surfaces, assets, &mut part)?;
    let _ = index;

    let mut models = existing_models(bsp).map_err(|e| lump_err(ids::MODELS, e))?;
    let base_meshes = r::parse_meshes(bsp.lump(ids::MESHES), v).map_err(|e| lump_err(ids::MESHES, e))?;
    let total = base_meshes.len() as u32;
    let Some(world) = models.first().copied() else {
        return Err(MeshError::Lump { id: ids::MODELS, reason: "the map has no world model".into() });
    };
    if world.first_mesh != 0 || world.first_mesh + world.num_meshes != total || models.iter().skip(1).any(|m| m.num_meshes != 0) {
        return Err(MeshError::Lump {
            id: ids::MODELS,
            reason: "meshes can only be appended while model 0 owns every mesh".into(),
        });
    }

    // Positions and normals.
    let pos_at = (bsp.lump(ids::VERTICES).len() / 12) as u32;
    let norm_at = (bsp.lump(ids::VERTEX_NORMALS).len() / 12) as u32;
    let mut positions = bsp.lump(ids::VERTICES).to_vec();
    positions.extend_from_slice(part.lump(ids::VERTICES));
    let mut normals = bsp.lump(ids::VERTEX_NORMALS).to_vec();
    normals.extend_from_slice(part.lump(ids::VERTEX_NORMALS));

    // Vertex lumps: the records move with the pools they index.
    let mut pool_at: HashMap<u16, u32> = HashMap::new();
    let mut pools: Vec<(u16, Vec<u8>)> = Vec::new();
    for (id, size, tangent) in APPEND_POOLS {
        let mut out = bsp.lump(id).to_vec();
        pool_at.insert(id, (out.len() / size) as u32);
        let added = part.lump(id);
        if added.is_empty() {
            continue;
        }
        for rec in added.chunks_exact(size) {
            let mut rec = rec.to_vec();
            let p = u32_at(&rec, 0) + pos_at;
            let n = u32_at(&rec, 4) + norm_at;
            rec[0..4].copy_from_slice(&p.to_le_bytes());
            rec[4..8].copy_from_slice(&n.to_le_bytes());
            if let Some(t) = tangent {
                let raw = u32_at(&rec, t);
                let moved = ((raw & 0x7FFF_FFFF) + norm_at) | (raw & 0x8000_0000);
                rec[t..t + 4].copy_from_slice(&moved.to_le_bytes());
            }
            out.extend_from_slice(&rec);
        }
        pools.push((id, out));
    }

    // Materials: reuse the map's texdata for a name it already has.
    let names = r::parse_surface_names(bsp.lump(ids::SURFACE_NAMES), v).map_err(|e| lump_err(ids::SURFACE_NAMES, e))?;
    let mut name_blob = bsp.lump(ids::SURFACE_NAMES).to_vec();
    let mut texdata = r::parse_texdata(bsp.lump(ids::TEXTURE_DATA), v).map_err(|e| lump_err(ids::TEXTURE_DATA, e))?;
    let part_names = r::parse_surface_names(part.lump(ids::SURFACE_NAMES), v).map_err(|e| lump_err(ids::SURFACE_NAMES, e))?;
    let part_texdata = r::parse_texdata(part.lump(ids::TEXTURE_DATA), v).map_err(|e| lump_err(ids::TEXTURE_DATA, e))?;
    let key = |s: &str| s.to_ascii_lowercase().replace('\\', "/");
    let mut tex_of: Vec<u16> = Vec::with_capacity(part_texdata.len());
    for td in &part_texdata {
        let name = part_names.at_offset(td.name as usize).unwrap_or_default().to_string();
        let found = texdata
            .iter()
            .position(|t| names.at_offset(t.name as usize).is_some_and(|n| key(n) == key(&name)));
        let at = match found {
            Some(i) => i,
            None => {
                let offset = name_blob.len() as u32;
                name_blob.extend(name.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }));
                name_blob.push(0);
                texdata.push(r::TexData { name: offset, ..*td });
                texdata.len() - 1
            }
        };
        tex_of.push(u16::try_from(at).map_err(|_| MeshError::Lump { id: ids::TEXTURE_DATA, reason: "more than 65535 texdata".into() })?);
    }

    // Sorts: each starts at the first appended record of its vertex lump.
    let part_meshes = r::parse_meshes(part.lump(ids::MESHES), v).map_err(|e| lump_err(ids::MESHES, e))?;
    let part_sorts = r::parse_material_sorts(part.lump(ids::MATERIAL_SORT), v).map_err(|e| lump_err(ids::MATERIAL_SORT, e))?;
    let mut sorts = r::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), v).map_err(|e| lump_err(ids::MATERIAL_SORT, e))?;
    let sort_at = sorts.len() as u32;
    for (si, s) in part_sorts.iter().enumerate() {
        let lump = part_meshes
            .iter()
            .find(|m| m.material_sort as usize == si)
            .map(|m| r::mesh_vertex_kind(m).1)
            .unwrap_or(ids::VERTEX_LIT_BUMP);
        if part_meshes.iter().any(|m| m.material_sort as usize == si && r::mesh_vertex_kind(m).1 != lump) {
            return Err(MeshError::Lump { id: ids::MATERIAL_SORT, reason: format!("sort {si} draws from two vertex lumps") });
        }
        let base = pool_at.get(&lump).copied().unwrap_or(0);
        sorts.push(r::MaterialSort {
            tex_data: tex_of.get(s.tex_data as usize).copied().unwrap_or(0),
            first_vertex: base + s.first_vertex,
            ..*s
        });
    }
    if sorts.len() > u16::MAX as usize {
        return Err(MeshError::Lump { id: ids::MATERIAL_SORT, reason: "more than 65535 material sorts".into() });
    }

    // Meshes, their bounds and their indices.
    let index_at = (bsp.lump(ids::MESH_INDICES).len() / 2) as u32;
    let mut meshes = base_meshes;
    let mut out = Vec::with_capacity(part_meshes.len());
    let part_bounds = r::parse_mesh_bounds(part.lump(ids::MESH_BOUNDS), v).map_err(|e| lump_err(ids::MESH_BOUNDS, e))?;
    for (i, m) in part_meshes.iter().enumerate() {
        meshes.push(r::Mesh {
            first_index: m.first_index + index_at,
            material_sort: (m.material_sort as u32 + sort_at) as u16,
            ..*m
        });
        let b = part_bounds[i];
        let o = Vec3::from_array(b.origin);
        let e = Vec3::from_array(b.extents);
        out.push(AppendedMesh { index: total + i as u32, mins: o - e, maxs: o + e });
    }
    let mut bounds = bsp.lump(ids::MESH_BOUNDS).to_vec();
    bounds.extend_from_slice(part.lump(ids::MESH_BOUNDS));
    let mut indices = bsp.lump(ids::MESH_INDICES).to_vec();
    indices.extend_from_slice(part.lump(ids::MESH_INDICES));

    // Model 0 grows over the new meshes; the mesh-less models follow them.
    let added = part_meshes.len() as u32;
    let (lo, hi) = out.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(a, b), m| (a.min(m.mins), b.max(m.maxs)));
    for (i, m) in models.iter_mut().enumerate() {
        if i == 0 {
            m.num_meshes += added;
            m.mins = Vec3::from_array(m.mins).min(lo).to_array();
            m.maxs = Vec3::from_array(m.maxs).max(hi).to_array();
        } else {
            m.first_mesh = total + added;
        }
    }

    bsp.set_lump(ids::VERTICES, positions);
    bsp.set_lump(ids::VERTEX_NORMALS, normals);
    for (id, bytes) in pools {
        bsp.set_lump(id, bytes);
    }
    bsp.set_lump(ids::SURFACE_NAMES, name_blob);
    bsp.set_lump(ids::TEXTURE_DATA, r::write_texdata(&texdata, v));
    bsp.set_lump(ids::MATERIAL_SORT, r::write_material_sorts(&sorts, v));
    bsp.set_lump(ids::MESHES, r::write_meshes(&meshes, v));
    bsp.set_lump(ids::MESH_BOUNDS, bounds);
    bsp.set_lump(ids::MESH_INDICES, indices);
    bsp.set_lump(ids::MODELS, rbsp::lumps::collision::write_models(&models, v));
    Ok(out)
}
