//! `relight`: a compiled client map lit again in place.
//!
//! Everything but the lighting keeps its bytes: geometry, collision, props,
//! entities and the vertices' lightmap coordinates. The bake shades the
//! lightmap atlas the vertices already address, one chart per connected island
//! of each mesh, and writes the lighting lumps over the old ones. Static props
//! occlude with their own render meshes, read from the converted models.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use glam::{Quat, Vec2, Vec3};
use rbake::{AlbedoTable, Backend};
use rbsp::lumps::{game_lump, level, lighting as lmp, render};
use rbsp::{ids, Bsp, Version};
use rir::{BakeLight, BakeScene, BakeSettings, LightType, RenderSurface, StaticPropInstance};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::propmesh::{load_model, model_path, ModelGeometry};

/// The cascaded shadow map lumps a relight leaves as they are.
const CSM_LUMPS: [u16; 9] = [
    // The shipped cubemap normalisers: a capture taken at night has so little
    // light at its origin that a fresh one scales reflections up several times.
    ids::CUBEMAPS_AMBIENT_RCP,
    ids::SHADOW_ENVIRONMENTS,
    ids::CSM_AABB_NODES,
    ids::CSM_OBJ_REFERENCES,
    ids::CSM_AABB_SUBTREE_REF_TOTALS,
    ids::SHADOW_MESHES,
    ids::SHADOW_MESH_INDICES,
    ids::SHADOW_MESH_OPAQUE_VERTICES,
    ids::SHADOW_MESH_ALPHA_VERTICES,
];

/// The static prop flag that keeps a prop out of baked shadows.
const SPRP_NO_SHADOW: u8 = 0x10;

/// How closely the sun has to follow the cascade tree's direction (cos 1 deg).
const SUN_MATCH: f32 = 0.99985;

/// What a relight is given: the sky, the bake quality and every light.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scene {
    pub settings: Settings,
    pub lights: Vec<BakeLight>,
    /// Props whose model name holds one of these never block light: light
    /// cards, fog sheets and other drawn-but-not-there geometry.
    #[serde(default)]
    pub occluder_skip: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub sky_color: [f32; 3],
    pub sky_intensity: f32,
    pub bounces: u32,
    pub samples: u32,
}

/// Everything `relight` takes.
#[derive(Clone, Debug)]
pub struct Args {
    pub map: PathBuf,
    pub scene: PathBuf,
    pub out: Option<PathBuf>,
    pub models: Vec<PathBuf>,
    pub mode: String,
    pub cubemap: Option<PathBuf>,
    pub preview: Option<PathBuf>,
    /// Only the charts inside this box are shaded; nothing is written but the
    /// preview.
    pub region: Option<[f32; 6]>,
    /// Texels (`page`, `x`, `y`) to explain instead of baking: what covers
    /// each, and what its ray toward the sun hits first.
    pub probe: Vec<[u32; 3]>,
    /// World points to explain: the texel nearest each, and what blocks the
    /// rays from it to the closest lights.
    pub probe_world: Vec<[f32; 3]>,
}

fn probe_world_points(scene: &BakeScene, layout: &rbake::ChartLayout, points: &[[f32; 3]]) -> Result<Value> {
    let geometry = rbake::bvh::scene_geometry(scene)?;
    let bvh = rbake::Bvh::build(&geometry.tris)?;
    let mut out = Vec::new();
    for p in points {
        let p = Vec3::from_array(*p);
        let mut best: Option<(f32, u32, u32, u32, rbake::light::Texel, usize)> = None;
        for c in &layout.charts {
            let Some(s) = scene.surfaces.get(c.surface) else { continue };
            let (lo, hi) = bounds_of(std::slice::from_ref(s));
            if p.x < lo.x - 32.0 || p.y < lo.y - 32.0 || p.z < lo.z - 32.0 || p.x > hi.x + 32.0 || p.y > hi.y + 32.0 || p.z > hi.z + 32.0 {
                continue;
            }
            let iw = c.inner[0] as usize;
            for (i, t) in rbake::light::rasterize(c, s).iter().enumerate() {
                let Some(t) = t else { continue };
                let d = t.position.distance(p);
                if best.as_ref().is_none_or(|b| d < b.0) {
                    best = Some((d, c.page, c.origin[0] + (i % iw) as u32, c.origin[1] + (i / iw) as u32, *t, c.surface));
                }
            }
        }
        let Some((d, page, x, y, t, si)) = best else {
            out.push(json!({ "point": p.to_array(), "texel": Value::Null }));
            continue;
        };
        let mut near: Vec<&BakeLight> = scene.lights.iter().filter(|l| l.kind != LightType::Sun).collect();
        near.sort_by(|a, b| a.position.distance(t.position).total_cmp(&b.position.distance(t.position)));
        let rays: Vec<Value> = near
            .iter()
            .take(4)
            .map(|l| {
                let to = l.position - t.position;
                let len = to.length();
                let ray = rbake::Ray::new(t.position, to / len.max(1e-6), 0.01, len);
                let hit = bvh.closest_hit(&ray).map(|h| {
                    let i = h.tri as usize;
                    match geometry.kinds.get(i) {
                        Some(2) => {
                            let k = geometry.prop.get(i).copied().unwrap_or(u32::MAX) as usize;
                            json!({ "t": h.t, "prop": scene.prop_occluders.get(k).and_then(|o| scene.props.get(o.0 as usize)).map(|m| m.model.clone()) })
                        }
                        _ => {
                            let s = geometry.surface.get(i).copied().unwrap_or(u32::MAX) as usize;
                            json!({ "t": h.t, "material": scene.surfaces.get(s).map(|s| s.material.clone()) })
                        }
                    }
                });
                json!({ "light": l.position.to_array(), "kind": format!("{:?}", l.kind), "intensity": l.intensity, "distance": len,
                        "n_dot_l": t.normal.normalize_or_zero().dot(to / len.max(1e-6)), "blocked_by": hit })
            })
            .collect();
        out.push(json!({ "point": p.to_array(), "texel": [page, x, y], "texel_distance": d, "texel_position": t.position.to_array(),
                         "normal": t.normal.to_array(), "material": scene.surfaces.get(si).map(|s| s.material.clone()), "lights": rays }));
    }
    Ok(Value::Array(out))
}

fn probe_texels(scene: &BakeScene, layout: &rbake::ChartLayout, texels: &[[u32; 3]], sun: Vec3) -> Result<Value> {
    let geometry = rbake::bvh::scene_geometry(scene)?;
    let bvh = rbake::Bvh::build(&geometry.tris)?;
    let mut out = Vec::new();
    for &[page, x, y] in texels {
        let mut found = Vec::new();
        for c in layout.charts.iter().filter(|c| c.page == page) {
            if x < c.origin[0] || y < c.origin[1] || x >= c.origin[0] + c.inner[0] || y >= c.origin[1] + c.inner[1] {
                continue;
            }
            let Some(s) = scene.surfaces.get(c.surface) else { continue };
            let at = ((y - c.origin[1]) * c.inner[0] + (x - c.origin[0])) as usize;
            let Some(Some(t)) = rbake::light::rasterize(c, s).get(at).copied() else { continue };
            let dir = -sun.normalize_or_zero();
            let ray = rbake::Ray::new(t.position, dir, 0.01, f32::MAX);
            let hit = bvh.closest_hit(&ray).map(|h| {
                let i = h.tri as usize;
                let what = match geometry.kinds.get(i) {
                    Some(2) => {
                        let p = geometry.prop.get(i).copied().unwrap_or(u32::MAX) as usize;
                        let model = scene.prop_occluders.get(p).and_then(|o| scene.props.get(o.0 as usize)).map(|m| m.model.clone());
                        json!({ "prop": model })
                    }
                    _ => {
                        let si = geometry.surface.get(i).copied().unwrap_or(u32::MAX) as usize;
                        json!({ "surface": si, "material": scene.surfaces.get(si).map(|s| s.material.clone()) })
                    }
                };
                json!({ "t": h.t, "point": h.point(&ray).to_array(), "front": h.front, "hit": what })
            });
            found.push(json!({
                "surface": c.surface,
                "material": s.material,
                "position": t.position.to_array(),
                "normal": t.normal.to_array(),
                "n_dot_l": t.normal.normalize_or_zero().dot(dir),
                "sun_ray": hit,
            }));
        }
        out.push(json!({ "page": page, "x": x, "y": y, "covered_by": found }));
    }
    Ok(Value::Array(out))
}

/// The world lights a map carries, as scene lights a relight takes back.
pub fn shipped_lights(map: &Path) -> Result<Value> {
    let bsp = Bsp::read_path(map).with_context(|| format!("reading {}", map.display()))?;
    let bytes = bsp.lump(ids::WORLD_LIGHTS);
    let mut lights = Vec::new();
    let mut skipped = Vec::new();
    for (i, rec) in bytes.chunks_exact(112).enumerate() {
        let r = rbake::codec::parse_world_light(rec)?;
        let colour = Vec3::from_array(r.intensity);
        let base = BakeLight {
            object: None,
            kind: LightType::Point,
            position: Vec3::from_array(r.origin),
            direction: Vec3::from_array(r.normal),
            color: colour,
            intensity: 1.0,
            range: r.radius,
            inner_angle: 0.0,
            outer_angle: 0.0,
            size: Vec2::ZERO,
            cast_shadows: true,
            baked: true,
            realtime: false,
            tweak_index: None,
            attenuation: Some([r.constant_attenuation, r.linear_attenuation, r.quadratic_attenuation]),
            flags: Some(r.flags),
        };
        let light = match r.light_type {
            rbake::worldlight::TYPE_POINT => base,
            rbake::worldlight::TYPE_SPOT => BakeLight {
                kind: LightType::Spot,
                inner_angle: r.inner_cone.clamp(-1.0, 1.0).acos().to_degrees(),
                outer_angle: r.outer_cone.clamp(-1.0, 1.0).acos().to_degrees(),
                ..base
            },
            rbake::worldlight::TYPE_SKYLIGHT => {
                BakeLight { kind: LightType::Sun, range: 0.0, attenuation: None, flags: None, realtime: true, ..base }
            }
            _ => {
                skipped.push(json!({ "index": i, "type": r.light_type, "intensity": r.intensity, "origin": r.origin }));
                continue;
            }
        };
        lights.push(json!({ "index": i, "light": light }));
    }
    Ok(json!({ "map": map.display().to_string(), "records": bytes.len() / 112, "lights": lights, "skipped": skipped }))
}

/// One mesh of the map as the bake sees it, and the lightmap page it reads.
struct MeshSurface {
    surface: RenderSurface,
    page: Option<u32>,
    /// Lit, but never in the way of light.
    clear: bool,
}

fn decode_surfaces(bsp: &Bsp, v: Version) -> Result<Vec<MeshSurface>> {
    let meshes = render::parse_meshes(bsp.lump(ids::MESHES), v)?;
    let sorts = render::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), v)?;
    let info = level::parse_level_info(bsp.lump(ids::LEVEL_INFO), v)?;
    let texdata = render::parse_texdata(bsp.lump(ids::TEXTURE_DATA), v)?;
    let surfaces = rcomp::meshes::decode_surfaces(bsp).map_err(|e| anyhow::anyhow!("{e}"))?;
    if surfaces.len() != meshes.len() {
        bail!("{} meshes decoded to {} surfaces", meshes.len(), surfaces.len());
    }
    let mut out = Vec::with_capacity(surfaces.len());
    for (mi, (s, m)) in surfaces.into_iter().zip(&meshes).enumerate() {
        let sky = mi as u32 >= info.first_sky_mesh || rcomp::meshes::is_sky_material(&s.material);
        if sky {
            // Neither lit nor in the way: the sky draws unlit behind everything.
            out.push(MeshSurface {
                surface: RenderSurface { material: s.material, ..RenderSurface::default() },
                page: None,
                clear: true,
            });
            continue;
        }
        let sort = sorts.get(m.material_sort as usize);
        let page = sort.and_then(|s| u32::try_from(s.lightmap).ok());
        let translucent = sort
            .and_then(|s| texdata.get(s.tex_data as usize))
            .is_some_and(|t| t.flags & rcomp::meshes::TEXDATA_TRANSLUCENT != 0);
        // Meshes are sorted opaque, decal, translucent, sky: a decal floats
        // over the face it marks and would shadow it.
        let material = s.material.replace('\\', "/").to_ascii_lowercase();
        let clear = mi as u32 >= info.first_decal_mesh
            || translucent
            || ["/atmosphere/", "/decal/", "water"].iter().any(|w| material.contains(w));
        out.push(MeshSurface { surface: s, page, clear });
    }
    Ok(out)
}

/// A lightmapped mesh split into the islands its triangles form, so each
/// island is shaded inside its own rectangle of the atlas.
fn islands(s: &RenderSurface) -> Vec<RenderSurface> {
    let n = s.positions.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            p[x as usize] = p[p[x as usize] as usize];
            x = p[x as usize];
        }
        x
    }
    for t in s.indices.chunks_exact(3) {
        let a = find(&mut parent, t[0]);
        for &k in &t[1..] {
            let b = find(&mut parent, k);
            if a != b {
                parent[b as usize] = a;
            }
        }
    }
    let mut groups: HashMap<u32, Vec<[u32; 3]>> = HashMap::new();
    let mut order: Vec<u32> = Vec::new();
    for t in s.indices.chunks_exact(3) {
        let root = find(&mut parent, t[0]);
        groups
            .entry(root)
            .or_insert_with(|| {
                order.push(root);
                Vec::new()
            })
            .push([t[0], t[1], t[2]]);
    }
    if order.len() <= 1 {
        return vec![s.clone()];
    }
    order
        .into_iter()
        .map(|root| {
            let mut remap: HashMap<u32, u32> = HashMap::new();
            let mut out = RenderSurface {
                object: s.object,
                model: s.model,
                material: s.material.clone(),
                cast_shadows: s.cast_shadows,
                lightmap_texel: s.lightmap_texel,
                ..RenderSurface::default()
            };
            for tri in &groups[&root] {
                for &i in tri {
                    let local = *remap.entry(i).or_insert_with(|| {
                        let k = out.positions.len() as u32;
                        out.positions.push(s.positions[i as usize]);
                        out.normals.push(s.normals[i as usize]);
                        out.tangents.push(s.tangents.get(i as usize).copied().unwrap_or(glam::Vec4::W));
                        out.uv0.push(s.uv0[i as usize]);
                        out.uv1.push(s.uv1[i as usize]);
                        out.colors.push(s.colors.get(i as usize).copied().unwrap_or([255; 4]));
                        k
                    });
                    out.indices.push(local);
                }
            }
            out
        })
        .collect()
}

struct Props {
    instances: Vec<StaticPropInstance>,
    occluders: Vec<(u32, Vec<Vec3>, Vec<u32>)>,
    missing: Vec<String>,
    skipped: usize,
    triangles: usize,
}

fn props(bsp: &Bsp, v: Version, roots: &[PathBuf], skip: &[String]) -> Result<Props> {
    let base = bsp.header.lumps[ids::GAME_LUMP as usize].offset;
    let gl = game_lump::parse_at(bsp.lump(ids::GAME_LUMP), v, base)?;
    let mut out = Props { instances: Vec::new(), occluders: Vec::new(), missing: Vec::new(), skipped: 0, triangles: 0 };
    let Some(block) = gl.static_props() else { return Ok(out) };
    let mut cache: HashMap<String, Option<ModelGeometry>> = HashMap::new();
    for i in 0..block.prop_count() {
        let (Some(p), Some(name)) = (block.props.get(i), block.model_of(i)) else { continue };
        let geo = cache
            .entry(name.to_string())
            .or_insert_with(|| model_path(roots, name).and_then(|path| load_model(&path).ok()))
            .clone();
        let scale = if p.scale > 0.0 { p.scale } else { 1.0 };
        let (min, max) = geo.as_ref().map(|g| (g.min, g.max)).unwrap_or((Vec3::ZERO, Vec3::ZERO));
        out.instances.push(StaticPropInstance {
            model: name.to_string(),
            origin: Vec3::from_array(p.origin),
            angles: Vec3::from_array(p.angles),
            scale,
            skin: 0,
            solid: true,
            cast_shadows: true,
            fade_distance: p.unknown_4,
            bounds_min: min,
            bounds_max: max,
        });
        let Some(g) = geo else {
            if !out.missing.iter().any(|m| m == name) {
                out.missing.push(name.to_string());
            }
            continue;
        };
        let lower = name.to_ascii_lowercase();
        // A prop the map compiler kept out of its shadows (not solid, or
        // flagged no-shadow) stays out of them here.
        let shadowless = p.unknown_0 == 0 || p.unknown_1 & SPRP_NO_SHADOW != 0;
        if shadowless || skip.iter().any(|s| lower.contains(&s.to_ascii_lowercase())) {
            out.skipped += 1;
            continue;
        }
        let rot = Quat::from_euler(glam::EulerRot::ZYX, p.angles[1].to_radians(), p.angles[0].to_radians(), p.angles[2].to_radians());
        let origin = Vec3::from_array(p.origin);
        let positions: Vec<Vec3> = g.positions.iter().map(|q| origin + rot * (*q * scale)).collect();
        out.triangles += g.indices.len() / 3;
        out.occluders.push((i as u32, positions, g.indices));
    }
    Ok(out)
}

fn bounds_of(surfaces: &[RenderSurface]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for p in surfaces.iter().flat_map(|s| s.positions.iter()) {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    if lo.x > hi.x { (Vec3::ZERO, Vec3::ZERO) } else { (lo, hi) }
}

/// The scene lights in the order the world light records take them: the sun,
/// then spots, then points. A texel's real-time slot counts the flagged
/// records in that order, so the scene has to match it.
fn ordered(lights: &[BakeLight]) -> Result<Vec<BakeLight>> {
    let suns = lights.iter().filter(|l| l.kind == LightType::Sun).count();
    if suns != 1 {
        bail!("the scene has {suns} suns; a map takes exactly one");
    }
    let rank = |l: &BakeLight| match l.kind {
        LightType::Sun => 0,
        LightType::Spot => 1,
        LightType::Point | LightType::Area => 2,
        LightType::Sky => 3,
    };
    let mut out: Vec<BakeLight> = lights.iter().filter(|l| l.kind != LightType::Sky).cloned().collect();
    out.sort_by_key(rank);
    Ok(out)
}

fn write_preview(dir: &Path, l: &rbake::Lighting) -> Result<Vec<String>> {
    std::fs::create_dir_all(dir)?;
    let mut files = Vec::new();
    for p in &l.pages {
        let (w, h) = (p.width as u32, p.height as u32);
        let mut img = image::RgbImage::new(w * 2, h);
        for y in 0..h {
            for x in 0..w {
                let at = (y * w + x) as usize;
                let tone = |c: f32| ((c / (1.0 + c)).powf(1.0 / 2.2) * 255.0).round().clamp(0.0, 255.0) as u8;
                let c = p.light[at];
                img.put_pixel(x, y, image::Rgb([tone(c.x), tone(c.y), tone(c.z)]));
                let s = (p.sun_vis[at] * 255.0).round() as u8;
                img.put_pixel(w + x, y, image::Rgb([s, s, s]));
            }
        }
        let path = dir.join(format!("page_{}.png", p.index));
        img.save(&path)?;
        files.push(path.display().to_string());
    }
    Ok(files)
}

pub fn run(args: &Args) -> Result<Value> {
    let started = std::time::Instant::now();
    let mut bsp = Bsp::read_path(&args.map).with_context(|| format!("reading {}", args.map.display()))?;
    let input = bsp.clone();
    let v = bsp.version_kind().filter(|v| *v == Version::ClientV51).context("relight takes a v51 client map")?;
    let scene_in: Scene = serde_json::from_slice(&std::fs::read(&args.scene)?)
        .with_context(|| format!("reading {}", args.scene.display()))?;
    let mode = crate::build::bake_mode(&args.mode)?;

    let headers = lmp::parse_lightmap_headers(bsp.lump(ids::LIGHTMAP_HEADERS), v)?;
    let pages: Vec<[u32; 2]> = headers.iter().map(|h| [h.width as u32, h.height as u32]).collect();
    if pages.is_empty() {
        bail!("{} has no lightmap pages to relight", args.map.display());
    }
    let meshes = decode_surfaces(&bsp, v)?;
    let mut surfaces: Vec<RenderSurface> = Vec::new();
    let mut page_of: Vec<Option<u32>> = Vec::new();
    let mut see_through: Vec<u32> = Vec::new();
    for m in meshes {
        let first = surfaces.len();
        match m.page {
            Some(p) => {
                for s in islands(&m.surface) {
                    let inside = args.region.is_none_or(|r| {
                        let (lo, hi) = bounds_of(std::slice::from_ref(&s));
                        lo.x <= r[3] && hi.x >= r[0] && lo.y <= r[4] && hi.y >= r[1] && lo.z <= r[5] && hi.z >= r[2]
                    });
                    surfaces.push(s);
                    page_of.push(inside.then_some(p));
                }
            }
            None => {
                surfaces.push(m.surface);
                page_of.push(None);
            }
        }
        if m.clear {
            see_through.extend(first as u32..surfaces.len() as u32);
        }
    }
    let props = props(&bsp, v, &args.models, &scene_in.occluder_skip)?;
    let (world_mins, world_maxs) = bounds_of(&surfaces);
    let cubemap_points: Vec<Vec3> = lmp::parse_cubemaps(bsp.lump(ids::CUBEMAPS), v)?
        .iter()
        .map(|c| Vec3::new(c.unknown_0 as f32, c.unknown_1 as f32, c.unknown_2 as f32))
        .collect();
    let probe_refs = lmp::parse_lightprobe_refs(bsp.lump(ids::LIGHTPROBE_REFERENCES), v)?;
    let probe_count = probe_refs.len();
    let probe_points: Vec<Vec3> =
        lmp::parse_lightprobe_refs(bsp.lump(ids::LIGHTPROBE_REFERENCES), v)?.iter().map(|r| Vec3::from_array(r.origin)).collect();
    let lights = ordered(&scene_in.lights)?;
    let sun = lights.iter().find(|l| l.kind == LightType::Sun).map(|l| l.direction).unwrap_or(Vec3::NEG_Z);
    if let Some(env) = lmp::parse_shadow_envs(bsp.lump(ids::SHADOW_ENVIRONMENTS), v)?.first() {
        let shipped = Vec3::from_array(env.shadow_direction).normalize_or_zero();
        if sun.normalize_or_zero().dot(shipped) < SUN_MATCH {
            bail!(
                "the sun travels along {sun}, the map's cascade tree was built for {shipped}; a relight keeps that tree, so the sun has to keep its direction"
            );
        }
    }
    let s = &scene_in.settings;
    let mut scene = BakeScene {
        surfaces,
        props: props.instances.clone(),
        prop_occluders: props.occluders,
        prop_lump_index: (0..props.instances.len() as u32).collect(),
        shadow_only: Vec::new(),
        see_through,
        shadow_transmission: Vec::new(),
        probes_ignore_props: true,
        lights,
        settings: BakeSettings {
            texel_size: 16.0,
            bounces: s.bounces,
            samples: s.samples,
            sky_color: Vec3::from_array(s.sky_color),
            sky_intensity: s.sky_intensity,
            ambient_occlusion: false,
            // The map's own probe positions are the probe set; the grid is
            // one cell over the whole world.
            probe_spacing: 1.0e7,
            cubemap_spacing: 0.0,
        },
        world_mins,
        world_maxs,
        cubemap_points,
        probe_points: if args.region.is_some() { Vec::new() } else { probe_points },
    };
    let layout = rbake::charts::fixed_layout(&mut scene, &pages, &page_of)?;
    let charts = layout.charts.len();
    if !args.probe.is_empty() {
        return probe_texels(&scene, &layout, &args.probe, sun);
    }
    if !args.probe_world.is_empty() {
        return probe_world_points(&scene, &layout, &args.probe_world);
    }
    let mut lighting = rbake::bake_in_layout(&scene, mode, Backend::Cpu, &AlbedoTable::default(), layout)?;
    let reassigned = if args.region.is_none() { keep_prop_probes(&input, v, &mut lighting, probe_count)? } else { 0 };

    let preview = match &args.preview {
        Some(dir) => write_preview(dir, &lighting)?,
        None => Vec::new(),
    };
    let mut written = Value::Null;
    if args.region.is_none() {
        if let Some(out) = &args.out {
            let lumps = rbake::to_lumps(&lighting, Version::ClientV51)?;
            if lighting.probes.prop_indices.len() != props.instances.len() {
                bail!("{} props took {} probe indices", props.instances.len(), lighting.probes.prop_indices.len());
            }
            // The shipped cascade tree is kept: its boxes live in the light
            // space of the direction it was built for, which the sun keeps.
            let kept: Vec<(u16, Vec<u8>)> = CSM_LUMPS.iter().map(|id| (*id, bsp.lump(*id).to_vec())).collect();
            lumps.encode(&mut bsp, Version::ClientV51)?;
            for (id, bytes) in kept {
                bsp.set_lump(id, bytes);
            }
            let stem = out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let ents = out
                .parent()
                .map(|d| Bsp::read_dir_map(d, &stem).map(|(_, e)| e).unwrap_or_default())
                .unwrap_or_default();
            // The relight answers only for what it wrote: an issue the input
            // already carried is reported, not held against it.
            let before: std::collections::HashSet<String> =
                rbsp::validate(&input, &ents).iter().map(|i| i.message.clone()).collect();
            let issues = rbsp::validate(&bsp, &ents);
            let lines: Vec<String> = issues
                .iter()
                .map(|i| format!("{:?} {}: {}", i.severity, i.lump.map(|l| format!("0x{l:02x}")).unwrap_or_default(), i.message))
                .collect();
            if let Some(err) = issues.iter().find(|i| i.is_error() && !before.contains(&i.message)) {
                bail!("the relit map does not validate: {}", err.message);
            }
            bsp.write_path(out).with_context(|| format!("writing {}", out.display()))?;
            let mut cube = Value::Null;
            if let Some(path) = &args.cubemap {
                if !lighting.cubemaps.maps.is_empty() {
                    let tex = rbake::cubemap::build_texture(&lighting.cubemaps.maps, "cubemaps_hdr")?;
                    std::fs::write(path, &tex.dds)?;
                    cube = json!({ "path": path.display().to_string(), "count": tex.layers / 6, "size": tex.width, "mips": tex.mips });
                }
            }
            written = json!({ "map": out.display().to_string(), "validation": lines, "cubemap": cube });
        }
    }
    let r = &lighting.report;
    Ok(json!({
        "mode": args.mode,
        "seconds": started.elapsed().as_secs_f64(),
        "pages": pages,
        "charts": charts,
        "surfaces": r.surfaces,
        "triangles": r.triangles,
        "props": props.instances.len(),
        "prop_occluder_triangles": props.triangles,
        "props_not_occluding": props.skipped,
        "models_missing": props.missing,
        "lights": r.lights,
        "props_on_shipped_probes": reassigned,
        "probes": r.probes,
        "cubemaps": r.cubemaps,
        "shadow_casters": r.shadow_casters,
        "shadow_refs": r.shadow_refs,
        "preview": preview,
        "written": written,
    }))
}

/// Each named model's hull and its meshes: material guid, box and mean normal
/// in model space, so a script can find a lamp's emitting surface.
pub fn model_report(roots: &[PathBuf], names: &[String], triangles: bool) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for name in names {
        let Some(path) = model_path(roots, name) else {
            out.insert(name.clone(), Value::Null);
            continue;
        };
        let rmdl = std::fs::read(&path)?;
        let vg = ["vg", "vg_static"].iter().map(|e| path.with_extension(e)).find(|p| p.is_file());
        let Some(vg) = vg else { continue };
        let model = rpak::rmdl::parse_model(&rmdl, &std::fs::read(&vg)?)?;
        let to_model = |v: [f32; 3]| Vec3::from_array(rpak::rmdl::vertex_to_studiohdr_space(v));
        let meshes: Vec<Value> = model
            .meshes
            .iter()
            .map(|m| {
                let (mut lo, mut hi, mut n) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY), Vec3::ZERO);
                for p in &m.positions {
                    let q = Vec3::from_array(*p);
                    lo = lo.min(q);
                    hi = hi.max(q);
                }
                for v in &m.normals {
                    n += Vec3::from_array(*v);
                }
                let guid = model.textures.get(m.material as usize).copied().unwrap_or(0);
                json!({
                    "material": m.material,
                    "guid": format!("0x{guid:016X}"),
                    "min": lo.to_array(),
                    "max": hi.to_array(),
                    "normal": n.normalize_or_zero().to_array(),
                    "triangles": m.indices.len() / 3,
                    "geometry": triangles.then(|| json!({
                        "positions": m.positions.iter().flatten().collect::<Vec<_>>(),
                        "normals": m.normals.iter().flatten().collect::<Vec<_>>(),
                        "uv0": m.uv0.iter().flatten().collect::<Vec<_>>(),
                        "indices": m.indices,
                    })),
                })
            })
            .collect();
        out.insert(
            name.clone(),
            json!({
                "path": path.display().to_string(),
                "min": to_model(model.bounds_min).to_array(),
                "max": to_model(model.bounds_max).to_array(),
                "meshes": meshes,
            }),
        );
    }
    Ok(Value::Object(out))
}

/// The map's render meshes whose material names one of `materials` (a case
/// insensitive substring), with their world-space geometry, so a script can
/// find emitting world faces the way it finds them on models.
pub fn world_meshes(map: &Path, materials: &[String]) -> Result<Value> {
    let bsp = Bsp::read_path(map).with_context(|| format!("reading {}", map.display()))?;
    let wanted: Vec<String> = materials.iter().map(|m| m.replace('\\', "/").to_ascii_lowercase()).collect();
    let surfaces = rcomp::meshes::decode_surfaces(&bsp).map_err(|e| anyhow::anyhow!("{e}"))?;
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let meshes = render::parse_meshes(bsp.lump(ids::MESHES), v)?;
    let sorts = render::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), v)?;
    let mut out = Vec::new();
    for (i, s) in surfaces.iter().enumerate() {
        let page = meshes.get(i).and_then(|m| sorts.get(m.material_sort as usize)).map(|s| s.lightmap);
        let name = s.material.replace('\\', "/").to_ascii_lowercase();
        if !wanted.iter().any(|w| name.contains(w.as_str())) {
            continue;
        }
        out.push(json!({
            "mesh": i,
            "material": s.material,
            "positions": s.positions.iter().flat_map(|p| p.to_array()).collect::<Vec<_>>(),
            "normals": s.normals.iter().flat_map(|p| p.to_array()).collect::<Vec<_>>(),
            "uv0": s.uv0.iter().flat_map(|p| p.to_array()).collect::<Vec<_>>(),
            "uv1": s.uv1.iter().flat_map(|p| p.to_array()).collect::<Vec<_>>(),
            "lightmap_page": page,
            "vertex_type": meshes.get(i).map(|m| m.vertex_type),
            "indices": s.indices,
        }));
    }
    Ok(Value::Array(out))
}

/// Props keep the probe the shipped map gave them. The bake's probes stand at the
/// shipped reference origins, in reference order after the grid, so a shipped
/// probe's first reference names the baked probe at its place. The shipped choice
/// was made against the map's own visibility; a nearest-visible pick through a
/// tree's own canopy lands on an unlit probe and draws the tree black.
fn keep_prop_probes(map: &Bsp, v: Version, lighting: &mut rbake::Lighting, refs_len: usize) -> Result<usize> {
    let shipped = lmp::parse_prop_lightprobe_indices(map.lump(ids::STATIC_PROP_LIGHTPROBE_INDICES), v)?;
    let refs = lmp::parse_lightprobe_refs(map.lump(ids::LIGHTPROBE_REFERENCES), v)?;
    let grid = lighting.probes.probes.len().saturating_sub(refs_len);
    let mut first: HashMap<u32, usize> = HashMap::new();
    for (j, r) in refs.iter().enumerate() {
        first.entry(r.probe).or_insert(j);
    }
    let mut n = 0;
    for (i, slot) in lighting.probes.prop_indices.iter_mut().enumerate() {
        let Some(old) = shipped.get(i) else { continue };
        if let Some(j) = first.get(&old.value) {
            *slot = (grid + j) as u32;
            n += 1;
        }
    }
    Ok(n)
}

/// Points every mesh and static prop at its nearest cubemap, in place. A map
/// whose converter tagged most of them with cubemap 0 reflects one capture
/// everywhere otherwise.
pub fn assign_cubemaps(map: &Path) -> Result<Value> {
    let mut bsp = Bsp::read_path(map).with_context(|| format!("reading {}", map.display()))?;
    let v = bsp.version_kind().context("not a known map version")?;
    let cubes: Vec<Vec3> = lmp::parse_cubemaps(bsp.lump(ids::CUBEMAPS), v)?
        .iter()
        .map(|c| Vec3::new(c.unknown_0 as f32, c.unknown_1 as f32, c.unknown_2 as f32))
        .collect();
    if cubes.is_empty() || cubes.len() > 0xFF {
        bail!("{} cubemaps cannot be indexed by a byte", cubes.len());
    }
    let nearest = |p: Vec3| -> usize {
        cubes.iter().enumerate().min_by(|a, b| a.1.distance(p).total_cmp(&b.1.distance(p))).map(|(i, _)| i).unwrap_or(0)
    };
    let bounds = render::parse_mesh_bounds(bsp.lump(ids::MESH_BOUNDS), v)?;
    let mut meshes = render::parse_meshes(bsp.lump(ids::MESHES), v)?;
    let mut mesh_hist = vec![0usize; cubes.len()];
    for (m, b) in meshes.iter_mut().zip(&bounds) {
        if m.cube_index == 0xFF {
            continue;
        }
        let i = nearest(Vec3::from_array(b.origin));
        m.cube_index = i as u8;
        mesh_hist[i] += 1;
    }
    bsp.set_lump(ids::MESHES, render::write_meshes(&meshes, v));
    let base = bsp.header.lumps[ids::GAME_LUMP as usize].offset;
    let mut gl = game_lump::parse_at(bsp.lump(ids::GAME_LUMP), v, base)?;
    let mut prop_hist = vec![0usize; cubes.len()];
    if let Some(at) = gl.entries.iter().position(|e| e.is_sprp()) {
        if let Some(game_lump::Block::StaticProps(block)) = gl.blocks.get_mut(at) {
            for p in block.props.iter_mut() {
                let i = nearest(Vec3::from_array(p.origin));
                p.unknown_3 = i as u16;
                prop_hist[i] += 1;
            }
        }
    }
    bsp.set_lump(ids::GAME_LUMP, game_lump::write(&gl, v));
    // Probe-lit models (props, characters, weapons) reflect the cubemap of the
    // probe reference nearest them, not the mesh or prop index above.
    let mut refs = lmp::parse_lightprobe_refs(bsp.lump(ids::LIGHTPROBE_REFERENCES), v)?;
    let mut probe_hist = vec![0usize; cubes.len()];
    for r in refs.iter_mut() {
        let i = nearest(Vec3::from_array(r.origin));
        r.cube_index = i as u16;
        probe_hist[i] += 1;
    }
    bsp.set_lump(ids::LIGHTPROBE_REFERENCES, lmp::write_lightprobe_refs(&refs, v));
    bsp.write_path(map)?;
    Ok(json!({ "cubemaps": cubes.len(), "meshes_per_cubemap": mesh_hist, "props_per_cubemap": prop_hist,
               "probe_refs_per_cubemap": probe_hist }))
}
