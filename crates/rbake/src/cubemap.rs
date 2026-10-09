//! Cubemap placement, the six rendered faces, and the array texture the pak
//! carries.
//!
//! The client loads one texture whose array size is six per cubemap and whose
//! blocks are BC6H, in slice order, largest mip first. A map with no cubemaps
//! would leave the array empty and every sample index past zero reading outside
//! it, so a bake always emits at least one.

use glam::Vec3;

use crate::bc6::{bc6h_level_bytes, encode_bc6h_face};
use crate::bvh::{Bvh, Ray};
use crate::codec::CubemapRecord;
use crate::error::{Error, Result};
use crate::light::{sky_light, DirectContext, Rng};

/// The face edge a cubemap is rendered at.
pub const DEFAULT_CUBEMAP_SIZE: u32 = 32;
/// The ambient scale 0x2B carries for a baked cubemap.
pub const DEFAULT_AMBIENT_SCALE: f32 = 1.0;

/// The six face directions, in the order the texture's slices are stored.
pub fn face_directions() -> [Vec3; 6] {
    [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ]
}

/// The world direction a face texel looks along, with `s` running right and
/// `t` down the stored image, both in -1..1. The client samples the array with
/// the world direction itself, so the faces follow the D3D cube layout.
pub fn face_direction(face: usize, s: f32, t: f32) -> Vec3 {
    let d = match face.min(5) {
        0 => Vec3::new(1.0, -t, -s),
        1 => Vec3::new(-1.0, -t, s),
        2 => Vec3::new(s, 1.0, t),
        3 => Vec3::new(s, -1.0, -t),
        4 => Vec3::new(s, -t, 1.0),
        _ => Vec3::new(-s, -t, -1.0),
    };
    d.normalize_or_zero()
}

/// One baked cubemap: where it sits, its six faces and its ambient scale.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BakedCubemap {
    pub position: Vec3,
    pub ambient_scale: f32,
    /// Six faces of `size * size` linear colours, rows top first.
    pub faces: Vec<Vec<Vec3>>,
    pub size: u32,
}

/// The cubemap set of a map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cubemaps {
    pub maps: Vec<BakedCubemap>,
}

impl Cubemaps {
    /// The 0x2A records: the origin in integer inches and a texture id the
    /// asset step fills in.
    pub fn records(&self) -> Vec<CubemapRecord> {
        self.maps
            .iter()
            .map(|m| CubemapRecord {
                origin: [m.position.x.round() as i32, m.position.y.round() as i32, m.position.z.round() as i32],
                texture: 0,
            })
            .collect()
    }

    /// The 0x2B ambient scales.
    pub fn ambient(&self) -> Vec<f32> {
        self.maps.iter().map(|m| m.ambient_scale).collect()
    }

    /// True when every map has six faces of the declared size and each face's
    /// block count matches the face's pixel count.
    pub fn faces_valid(&self) -> bool {
        self.maps.iter().all(|m| {
            m.size > 0 && m.size % 4 == 0 && m.faces.len() == 6 && m.faces.iter().all(|f| f.len() == (m.size * m.size) as usize)
        })
    }
}

/// Height above a floor a placed cubemap sits at: a standing player's eyes.
pub const CUBEMAP_EYE_HEIGHT: f32 = 64.0;
/// Floors per grid column a cubemap is placed on, top down.
const FLOORS_PER_COLUMN: usize = 4;

/// Places cubemaps on the floors the player stands on plus the designer's own
/// points, culled to open space. A grid of columns `spacing` apart covers the
/// world box; `floors` answers, for a column's (x, y), the heights of the
/// upward-facing surfaces under the top of the box, highest first.
pub fn place_cubemaps(
    world_mins: Vec3,
    world_maxs: Vec3,
    spacing: f32,
    extra: &[Vec3],
    open: &dyn Fn(Vec3) -> bool,
    floors: &dyn Fn(f32, f32) -> Vec<f32>,
) -> Result<Vec<Vec3>> {
    let step = if spacing.is_finite() && spacing > 0.0 { spacing } else { 1024.0 };
    let size = (world_maxs - world_mins).max(Vec3::ZERO);
    let cells = |len: f32| ((len / step).ceil().clamp(1.0, 1024.0)) as usize;
    let (nx, ny) = (cells(size.x), cells(size.y));
    if nx * ny * FLOORS_PER_COLUMN > 1_000_000 {
        return Err(Error::lighting(format!(
            "a cubemap every {step} units over the world bounds needs {} columns; raise cubemap_spacing",
            nx * ny
        )));
    }
    // Placed points are kept as they are: they are the ones a surface should
    // reflect, and culling one would renumber every prop's cubemap slot. They
    // are not tested against `open`: its ray-parity test assumes closed
    // geometry, and on a world of open shells (an imported mesh map) it called
    // 3 of CyberPark's 7 original reflection captures solid and dropped them.
    let mut out: Vec<Vec3> = extra.iter().copied().filter(|p| p.is_finite()).collect();
    // Designer points win; otherwise one cubemap stands above each floor of
    // each column, so a surface reflects the space a player sees it from.
    if out.is_empty() {
        for iy in 0..ny {
            for ix in 0..nx {
                let x = world_mins.x + (ix as f32 + 0.5) * size.x / nx as f32;
                let y = world_mins.y + (iy as f32 + 0.5) * size.y / ny as f32;
                for z in floors(x, y).into_iter().take(FLOORS_PER_COLUMN) {
                    let p = Vec3::new(x, y, (z + CUBEMAP_EYE_HEIGHT).min(world_maxs.z));
                    if open(p) {
                        out.push(p);
                    }
                }
            }
        }
    }
    if out.is_empty() {
        let centre = (world_mins + world_maxs) * 0.5;
        if open(centre) {
            out.push(centre);
        } else {
            out.push(world_mins + Vec3::splat(1.0));
        }
    }
    Ok(out)
}

/// Renders one cubemap's six faces with the CPU tracer.
pub fn render_cubemap(
    position: Vec3,
    size: u32,
    ctx: &DirectContext,
    bvh: &Bvh,
) -> Result<BakedCubemap> {
    if size == 0 || !size.is_multiple_of(4) {
        return Err(Error::lighting(format!("a cubemap face must be a non-zero multiple of 4, got {size}")));
    }
    let n = size as usize;
    let rays = (ctx.settings.samples.max(1) as usize / 2).clamp(1, 16);
    let mut rng = Rng::new(0x0C0B_E123);
    let mut faces: Vec<Vec<Vec3>> = Vec::with_capacity(6);
    for face in 0..6 {
        let mut buf = vec![Vec3::ZERO; n * n];
        for y in 0..n {
            for x in 0..n {
                let s = 2.0 * (x as f32 + 0.5) / n as f32 - 1.0;
                let t = 2.0 * (y as f32 + 0.5) / n as f32 - 1.0;
                let dir = face_direction(face, s, t);
                let mut acc = Vec3::ZERO;
                for _ in 0..rays {
                    acc += sample_cubemap_ray(ctx, bvh, position, dir, &mut rng, 1);
                }
                buf[y * n + x] = to_sample(acc / rays as f32);
            }
        }
        faces.push(buf);
    }
    Ok(BakedCubemap {
        position,
        ambient_scale: DEFAULT_AMBIENT_SCALE,
        faces,
        size,
    })
}

fn to_sample(v: Vec3) -> Vec3 {
    if !v.is_finite() {
        return Vec3::ZERO;
    }
    v.clamp(Vec3::ZERO, Vec3::splat(crate::bc6::BC6H_MAX))
}

/// One gathered direction of a cubemap face. `depth` counts the bounces still
/// to take, so the gather always terminates.
fn sample_cubemap_ray(
    ctx: &DirectContext,
    bvh: &Bvh,
    origin: Vec3,
    dir: Vec3,
    rng: &mut Rng,
    depth: u32,
) -> Vec3 {
    let d = dir.normalize_or_zero();
    let o = origin + d * 0.5;
    let Some(hit) = bvh.closest_hit(&Ray::new(o, d, 0.01, f32::MAX)) else {
        return sky_light(ctx.settings, d, 1.0);
    };
    let ray = Ray::new(o, d, 0.01, f32::MAX);
    let p = hit.point(&ray);
    let n = hit.normal(&ray, ctx.tris);
    let mut acc = Vec3::ZERO;
    for l in ctx.lights_at(p) {
        acc += ctx.direct(l, p, n, hit.t).colour;
    }
    let open = crate::light::sky_visibility(bvh, p, n, 4);
    acc += sky_light(ctx.settings, n, open);
    if depth > 0 {
        // One jittered bounce keeps the faces from reading as flat.
        let nd = rng.cosine_dir(n);
        if nd.length_squared() > 0.5 {
            acc += 0.5 * sample_cubemap_ray(ctx, bvh, p + nd * 0.05, nd, rng, depth - 1);
        }
    }
    // A reflection sees the light a surface sends back, not what reaches it.
    ctx.albedo_of(hit.tri) * acc
}

/// The array texture the pak carries: a DDS file with a DX10 header, BC6H
/// blocks, six slices per cubemap and the mip chain largest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CubemapTexture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub mips: u32,
    pub layers: u32,
    pub dds: Vec<u8>,
}

const DXGI_FORMAT_BC6H_UF16: u32 = 95;
/// The 4-byte magic, the 124-byte DDS header and the 20-byte DX10 extension.
const DDS_HEADER_SIZE: usize = 148;

impl CubemapTexture {
    /// The number of mip levels a cubemap array carries: the full chain down
    /// to 1x1, as the shipped maps' cubemap textures do (9 for 256).
    pub fn mip_count(width: u32, height: u32) -> u32 {
        32 - width.max(height).max(1).leading_zeros()
    }
}

/// Builds the cubemap array texture from a baked set.
pub fn build_texture(maps: &[BakedCubemap], name: &str) -> Result<CubemapTexture> {
    let first = maps
        .first()
        .ok_or_else(|| Error::lighting("a cubemap texture needs at least one cubemap"))?;
    let size = first.size;
    if size == 0 || size % 4 != 0 {
        return Err(Error::lighting(format!("a cubemap face must be a non-zero multiple of 4, got {size}")));
    }
    let mips = CubemapTexture::mip_count(size, size);
    // A DDS array is stored slice by slice, each with its whole mip chain; a
    // cubemap is six consecutive slices.
    let mut data: Vec<u8> = Vec::new();
    for m in maps {
        if m.size != size || m.faces.len() != 6 {
            return Err(Error::lighting("every cubemap in a texture needs six faces of the same size"));
        }
        let chain = prefilter_chain(m, mips)?;
        for face in 0..6 {
            for (level, faces) in chain.iter().enumerate() {
                let (w, h) = level_size(size, mips, level as u32);
                data.extend_from_slice(&encode_bc6h_face(&faces[face], w, h)?);
            }
        }
    }
    let layers = (maps.len() * 6) as u32;
    let mut dds = dds_header(size, size, mips, layers);
    dds.extend_from_slice(&data);
    Ok(CubemapTexture {
        name: name.to_string(),
        width: size,
        height: size,
        mips,
        layers,
        dds,
    })
}

fn level_size(size: u32, _mips: u32, level: u32) -> (u32, u32) {
    let mut w = size;
    let mut h = size;
    for _ in 0..level {
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    (w, h)
}

/// The per-level face sets, largest first, boxed to whole blocks.
fn downsample_chain(m: &BakedCubemap, mips: u32) -> Result<Vec<Vec<Vec<Vec3>>>> {
    let mut chain: Vec<Vec<Vec<Vec3>>> = vec![m.faces.clone()];
    for level in 1..mips {
        let (w, h) = level_size(m.size, mips, level);
        let (pw, ph) = level_size(m.size, mips, level - 1);
        let mut next: Vec<Vec<Vec3>> = Vec::with_capacity(6);
        for src in &chain[(level - 1) as usize] {
            let mut buf = vec![Vec3::ZERO; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    let (x0, y0) = (x * pw / w, y * ph / h);
                    let mut acc = Vec3::ZERO;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let sx = (x0 + dx).min(pw - 1);
                            let sy = (y0 + dy).min(ph - 1);
                            acc += src[(sy * pw + sx) as usize];
                        }
                    }
                    buf[(y * w + x) as usize] = acc / 4.0;
                }
            }
            next.push(buf);
        }
        chain.push(next);
    }
    Ok(chain)
}

/// A cubemap resampled to `size` per face.
pub fn resized(m: &BakedCubemap, size: u32) -> BakedCubemap {
    let n = size as usize;
    let mut faces = Vec::with_capacity(6);
    for face in 0..6 {
        let mut buf = vec![Vec3::ZERO; n * n];
        for y in 0..n {
            for x in 0..n {
                let s = 2.0 * (x as f32 + 0.5) / n as f32 - 1.0;
                let t = 2.0 * (y as f32 + 0.5) / n as f32 - 1.0;
                buf[y * n + x] = sample_faces(&m.faces, m.size, face_direction(face, s, t));
            }
        }
        faces.push(buf);
    }
    BakedCubemap { position: m.position, ambient_scale: m.ambient_scale, faces, size }
}

/// The Phong exponent of the last mip; each finer level multiplies it by four,
/// so a 256 texel cubemap runs from about 1638 at the top to 0.025 at 1x1.
const PHONG_LAST_LEVEL: f32 = 0.025;

/// The face and position a world direction lands on: the inverse of
/// [`face_direction`], with `s` and `t` in -1..1.
pub fn direction_to_face(d: Vec3) -> (usize, f32, f32) {
    let a = d.abs();
    if a.x >= a.y && a.x >= a.z {
        let m = a.x.max(1e-9);
        if d.x > 0.0 { (0, -d.z / m, -d.y / m) } else { (1, d.z / m, -d.y / m) }
    } else if a.y >= a.z {
        let m = a.y.max(1e-9);
        if d.y > 0.0 { (2, d.x / m, d.z / m) } else { (3, d.x / m, -d.z / m) }
    } else {
        let m = a.z.max(1e-9);
        if d.z > 0.0 { (4, d.x / m, -d.y / m) } else { (5, -d.x / m, -d.y / m) }
    }
}

fn sample_faces(faces: &[Vec<Vec3>], n: u32, d: Vec3) -> Vec3 {
    let (face, s, t) = direction_to_face(d);
    let n = n.max(1) as usize;
    let fx = ((s + 1.0) * 0.5 * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
    let fy = ((t + 1.0) * 0.5 * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(n - 1), (y0 + 1).min(n - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let p = |x: usize, y: usize| faces[face][y * n + x];
    p(x0, y0).lerp(p(x1, y0), tx).lerp(p(x0, y1).lerp(p(x1, y1), tx), ty)
}

fn radical_inverse(mut i: u32) -> f32 {
    i = i.reverse_bits();
    i as f32 * (1.0 / 4_294_967_296.0)
}

/// The per-level face sets, largest first, each level the cubemap convolved
/// with a Phong lobe that widens by a factor of four in exponent per level.
/// The lobe is importance-sampled from the box level whose texels match the
/// sample footprint, so no level aliases and none is sharper than its lobe.
fn prefilter_chain(m: &BakedCubemap, mips: u32) -> Result<Vec<Vec<Vec<Vec3>>>> {
    let boxed = downsample_chain(m, mips)?;
    let last = mips.saturating_sub(1);
    let mut chain: Vec<Vec<Vec<Vec3>>> = Vec::with_capacity(mips as usize);
    for level in 0..mips {
        let (w, _) = level_size(m.size, mips, level);
        let exponent = PHONG_LAST_LEVEL * 4f32.powi((last - level) as i32);
        let samples: u32 = if level == 0 { 16 } else { 64 };
        let half_power = 0.5f32.powf(1.0 / (exponent + 1.0)).clamp(-1.0, 1.0).acos();
        let footprint = (2.0 * half_power / (samples as f32).sqrt()).max(1e-4);
        let want = std::f32::consts::FRAC_PI_2 / footprint;
        let src_level = (0..mips).rev().find(|&l| level_size(m.size, mips, l).0 as f32 >= want).unwrap_or(0);
        let (sw, _) = level_size(m.size, mips, src_level);
        let src = &boxed[src_level as usize];
        let mut faces = Vec::with_capacity(6);
        for face in 0..6 {
            let mut buf = vec![Vec3::ZERO; (w * w) as usize];
            for y in 0..w {
                for x in 0..w {
                    let s = 2.0 * (x as f32 + 0.5) / w as f32 - 1.0;
                    let t = 2.0 * (y as f32 + 0.5) / w as f32 - 1.0;
                    let n = face_direction(face, s, t);
                    let a = if n.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
                    let tx = a.cross(n).normalize_or_zero();
                    let ty = n.cross(tx);
                    let mut acc = Vec3::ZERO;
                    for i in 0..samples {
                        let u = (i as f32 + 0.5) / samples as f32;
                        let phi = std::f32::consts::TAU * radical_inverse(i);
                        let cos_t = u.powf(1.0 / (exponent + 1.0));
                        let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
                        let d = tx * (sin_t * phi.cos()) + ty * (sin_t * phi.sin()) + n * cos_t;
                        acc += sample_faces(src, sw, d);
                    }
                    buf[(y * w + x) as usize] = acc / samples as f32;
                }
            }
            faces.push(buf);
        }
        chain.push(faces);
    }
    Ok(chain)
}

/// A DDS header with the DX10 extension, array size and all.
pub fn dds_header(width: u32, height: u32, mips: u32, layers: u32) -> Vec<u8> {
    let mut out = vec![0u8; DDS_HEADER_SIZE];
    out[0..4].copy_from_slice(b"DDS ");
    let mut put32 = |o: usize, v: u32| out[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put32(4, 124);
    // caps: height, width, pixelformat, mipmapcount, linearsize, caps
    put32(8, 0x000A_1007);
    put32(12, height);
    put32(16, width);
    put32(20, bc6h_level_bytes(width, height) as u32);
    put32(24, 1);
    put32(28, mips);
    // pixel format: its size, the fourcc flag and "DX10"
    put32(76, 32);
    put32(80, 0x4);
    put32(84, 0x3031_5844);
    // caps: complex, texture, mipmap
    put32(108, 0x0040_1008);
    put32(128, DXGI_FORMAT_BC6H_UF16);
    put32(132, 3);
    put32(136, 0);
    put32(140, layers);
    put32(144, 0);
    out
}

/// The cubemap nearest a point, by index.
pub fn nearest_cubemap(positions: &[Vec3], p: Vec3) -> Option<usize> {
    positions
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.distance_squared(p).total_cmp(&b.1.distance_squared(p)))
        .map(|(i, _)| i)
}

/// Points every world mesh and static prop at its nearest cubemap. Sky meshes
/// (0xFF) and skybox vistas (prop flag 0x40) sample none and are left alone.
pub fn assign_indices(bsp: &mut rbsp::Bsp, positions: &[Vec3]) -> Result<()> {
    use rbsp::lumps::{game_lump, render};
    use rbsp::ids;
    if positions.is_empty() {
        return Ok(());
    }
    let v = bsp.version_kind().unwrap_or(rbsp::Version::ClientV51);
    let bounds = render::parse_mesh_bounds(bsp.lump(ids::MESH_BOUNDS), v).map_err(|e| Error::lighting(e.to_string()))?;
    let mut meshes = render::parse_meshes(bsp.lump(ids::MESHES), v).map_err(|e| Error::lighting(e.to_string()))?;
    if bounds.len() == meshes.len() && positions.len() <= 0xFF {
        for (m, b) in meshes.iter_mut().zip(&bounds) {
            if m.cube_index == 0xFF {
                continue;
            }
            if let Some(i) = nearest_cubemap(positions, Vec3::from_array(b.origin)) {
                m.cube_index = i as u8;
            }
        }
        bsp.set_lump(ids::MESHES, render::write_meshes(&meshes, v));
    }
    // Probe-lit models reflect the cubemap their nearest probe reference names.
    // A set that already names cubemaps (a decompiled map) keeps them.
    let mut refs = rbsp::lumps::lighting::parse_lightprobe_refs(bsp.lump(ids::LIGHTPROBE_REFERENCES), v)
        .map_err(|e| Error::lighting(e.to_string()))?;
    if !refs.is_empty() && refs.iter().all(|r| r.cube_index == 0) {
        for r in refs.iter_mut() {
            if let Some(i) = nearest_cubemap(positions, Vec3::from_array(r.origin)) {
                r.cube_index = i as u16;
            }
        }
        bsp.set_lump(ids::LIGHTPROBE_REFERENCES, rbsp::lumps::lighting::write_lightprobe_refs(&refs, v));
    }
    let base = bsp.header.lumps[ids::GAME_LUMP as usize].offset;
    let Ok(mut gl) = game_lump::parse_at(bsp.lump(ids::GAME_LUMP), v, base) else {
        return Ok(());
    };
    let Some(at) = gl.entries.iter().position(|e| e.is_sprp()) else {
        return Ok(());
    };
    if let Some(game_lump::Block::StaticProps(block)) = gl.blocks.get_mut(at) {
        for p in block.props.iter_mut() {
            if p.unknown_1 & 0x40 != 0 {
                continue;
            }
            // A prop that names its cubemap keeps it, so a decompiled map
            // keeps its shipped slots; one that names none takes its nearest.
            if p.unknown_3 != 0 {
                continue;
            }
            if let Some(i) = nearest_cubemap(positions, Vec3::from_array(p.origin)) {
                p.unknown_3 = i as u16;
            }
        }
        bsp.set_lump(ids::GAME_LUMP, game_lump::write(&gl, v));
    }
    Ok(())
}

#[cfg(test)]
mod downsample_tests {
    use super::*;

    #[test]
    fn grid_cubemaps_stand_above_the_floors_not_at_the_world_mins() {
        let (lo, hi) = (Vec3::new(-1024.0, -1024.0, -64.0), Vec3::new(1024.0, 1024.0, 512.0));
        let floors = |_x: f32, _y: f32| vec![0.0f32];
        let got = place_cubemaps(lo, hi, 2048.0, &[], &|_| true, &floors).unwrap();
        assert_eq!(got, vec![Vec3::new(0.0, 0.0, CUBEMAP_EYE_HEIGHT)]);
        let placed = [Vec3::new(10.0, 20.0, 30.0)];
        assert_eq!(place_cubemaps(lo, hi, 2048.0, &placed, &|_| true, &floors).unwrap(), placed.to_vec());
    }

    #[test]
    fn placed_points_survive_an_open_test_that_calls_them_solid() {
        let (lo, hi) = (Vec3::splat(-1000.0), Vec3::splat(1000.0));
        let placed = [Vec3::new(10.0, 0.0, 50.0), Vec3::new(-300.0, 20.0, 80.0)];
        let floors = |_: f32, _: f32| vec![0.0];
        assert_eq!(place_cubemaps(lo, hi, 2048.0, &placed, &|_| false, &floors).unwrap(), placed.to_vec());
    }

    #[test]
    fn a_box_level_averages_its_own_quad() {
        let n = 8u32;
        let face: Vec<Vec3> = (0..n * n).map(|i| Vec3::splat((i % n) as f32)).collect();
        let m = BakedCubemap { position: Vec3::ZERO, ambient_scale: 1.0, faces: vec![face; 6], size: n };
        let chain = downsample_chain(&m, 2).unwrap();
        for x in 0..4 {
            assert_eq!(chain[1][0][x].x, 2.0 * x as f32 + 0.5, "column {x}");
        }
    }
}
