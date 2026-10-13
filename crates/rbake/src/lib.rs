//! Lighting for the rBSP compiler: world lights, lightmap pages and the
//! real-time-light index, light probes, cascaded shadow map trees with their
//! shadow meshes, and the map cubemaps.
//!
//! The scene arrives as [`rir::BakeScene`] with the surfaces already in world
//! space. [`bake`] produces a [`Lighting`] product, [`emit`] writes it into a
//! map through the typed codecs, and [`preview_rgba`] gives the editor a
//! per-object image of what was baked.

pub mod bc;
pub mod bc6;
pub mod bvh;
pub mod capture;
pub mod charts;
pub mod codec;
pub mod csm;
pub mod cubemap;
pub mod denoise;
pub mod error;
pub mod gpu;
pub mod light;
pub mod probes;
pub mod progress;
pub mod rtl;
pub mod worldlight;

use std::collections::BTreeMap;

use glam::Vec3;
use uuid::Uuid;

pub use bvh::{Bvh, Bounds, Ray, Tri};
pub use charts::{pack_charts, Chart, ChartLayout};
pub use codec::{LightingLumps, RtlPage, SkyPage, WorldLightRecord};
pub use csm::Csm;
pub use cubemap::{BakedCubemap, CubemapTexture, Cubemaps};
pub use denoise::Sample;
pub use error::{Error, Result};
pub use light::{AlbedoTable, BakedPage};
pub use probes::{Probe, Probes};
pub use rtl::{LightGroup, RtlIndex};
pub use worldlight::world_lights;

use rbsp::lumps::lighting as lmp;
use rbsp::Version;

/// How much of the bake to run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BakeMode {
    /// No lighting solved at all: the minimal valid lump set, so a map without
    /// a bake still loads.
    #[default]
    None,
    /// Fewer samples and no indirect bounces: the editor's live view.
    Preview,
    /// Full samples and the configured bounces.
    Final,
}

impl BakeMode {
    pub fn is_none(self) -> bool {
        self == BakeMode::None
    }
}

/// Which solver evaluates the lighting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    Cpu,
    Gpu,
}

/// What the bake produced, before it reaches a map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lighting {
    pub mode: BakeMode,
    pub layout: ChartLayout,
    pub pages: Vec<BakedPage>,
    pub world_lights: Vec<WorldLightRecord>,
    pub tweak_lights: Vec<u32>,
    /// The 0x7A group table the page slots index into.
    pub light_groups: Vec<rtl::LightGroup>,
    pub probes: Probes,
    pub csm: Csm,
    pub cubemaps: Cubemaps,
    /// The albedo the indirect pass used per surface index.
    pub albedo: Vec<[f32; 3]>,
    /// Counters for the compile report.
    pub report: Report,
}

/// Counters a compile report prints.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Cubemaps taken from an in-engine capture instead of the bake.
    pub cubemaps_captured: usize,
    pub surfaces: usize,
    pub charted_surfaces: usize,
    pub triangles: usize,
    pub shadow_casters: usize,
    pub shadow_triangles: u32,
    pub shadow_refs: usize,
    pub probes: usize,
    pub cubemaps: usize,
    pub lights: usize,
    pub denoised_texels: usize,
}

/// The page size a bake packs into, matching the size the reference map uses.
pub const PAGE_SIZE: u32 = 1024;
/// The page size the editor's preview view packs into, small enough to rebake
/// on a keystroke.
pub const PREVIEW_PAGE_SIZE: u32 = 256;
/// The cubemap spacing a scene with none configured gets.
pub const DEFAULT_CUBEMAP_SPACING: f32 = 1024.0;
/// The cubemap face edge the preview view renders at.
pub const PREVIEW_CUBEMAP_SIZE: u32 = 8;

/// Solves the lighting of a scene.
pub fn bake(scene: &rir::BakeScene, mode: BakeMode, backend: Backend) -> Result<Lighting> {
    bake_with_albedo(scene, mode, backend, &AlbedoTable::default())
}

/// The same solve with an explicit average-colour table.
pub fn bake_with_albedo(
    scene: &rir::BakeScene,
    mode: BakeMode,
    backend: Backend,
    albedo: &AlbedoTable,
) -> Result<Lighting> {
    light::check_scene(scene)?;
    let page_size = if mode == BakeMode::Preview { PREVIEW_PAGE_SIZE } else { PAGE_SIZE };
    let layout = charts::pack_charts_with(scene, page_size)?;
    bake_in_layout(scene, mode, backend, albedo, layout)
}

/// The same solve into a layout the caller already has, such as the atlas a
/// shipped map's vertices carry ([`charts::fixed_layout`]).
pub fn bake_in_layout(
    scene: &rir::BakeScene,
    mode: BakeMode,
    backend: Backend,
    albedo: &AlbedoTable,
    layout: ChartLayout,
) -> Result<Lighting> {
    light::check_scene(scene)?;
    let geometry = bvh::scene_geometry(scene)?;
    let hierarchy = Bvh::build_scene(&geometry)?;
    let tri_albedo = triangle_albedo(scene, &geometry, albedo);
    let light_index = light::LightIndex::build(&scene.lights);
    let ctx = light::DirectContext {
        bvh: &hierarchy,
        tris: &hierarchy.triangles,
        lights: &scene.lights,
        settings: &scene.settings,
        seed: probes::probe_seed(scene),
        index: Some(&light_index),
        albedo: &tri_albedo,
    };
    let world = worldlight::world_lights(scene);
    let tweak = worldlight::tweak_lights(scene, &world);
    let (groups, slots) = rtl::build_groups(&scene.lights, world.len())?;
    let rtl_slots = rtl::RtlSlots { slots };

    let mut report = Report {
        surfaces: scene.surfaces.len(),
        charted_surfaces: layout.charts.len(),
        triangles: geometry.tris.len(),
        lights: world.len(),
        ..Report::default()
    };
    let mut albedo_rgb: Vec<[f32; 3]> = Vec::with_capacity(scene.surfaces.len());
    for s in &scene.surfaces {
        let a = albedo.for_surface(s);
        albedo_rgb.push([a.x, a.y, a.z]);
    }

    if mode == BakeMode::None {
        let ambient = ambient_term(scene);
        let mut pages: Vec<BakedPage> = Vec::new();
        for p in &layout.pages {
            pages.push(BakedPage::ambient(
                p.index,
                p.width.min(u16::MAX as u32) as u16,
                p.height.min(u16::MAX as u32) as u16,
                ambient,
            )?);
        }
        let (probes, cascade, cubemaps) = minimal_products(scene, &hierarchy)?;
        report.probes = probes.probes.len();
        report.shadow_casters = cascade.meshes.len();
        report.shadow_refs = cascade.refs.len();
        report.cubemaps = cubemaps.maps.len();
        return Ok(Lighting {
            mode,
            layout,
            pages,
            world_lights: world,
            tweak_lights: tweak,
            light_groups: groups,
            probes,
            csm: cascade,
            cubemaps,
            albedo: albedo_rgb,
            report,
        });
    }

    // The GPU pass answers the same visibility question the CPU hierarchy does,
    // so the page raster only runs once either way.
    let gpu_vis = if backend == Backend::Gpu {
        gpu::sun_visibility_batch(&ctx, &layout, scene)?
    } else {
        None
    };

    let mut pages: Vec<BakedPage> = Vec::new();
    for p in &layout.pages {
        pages.push(BakedPage::new(
            p.index,
            p.width.min(u16::MAX as u32) as u16,
            p.height.min(u16::MAX as u32) as u16,
        )?);
    }
    let texels: u64 = layout.charts.iter().map(|c| c.inner[0] as u64 * c.inner[1] as u64).sum();
    let meter = progress::Meter::new("lightmaps", "texels", texels, 22, 78);
    shade_pages(scene, &ctx, &layout, &mut pages, albedo, mode, &rtl_slots, &meter)?;
    meter.finish();
    if let Some(v) = &gpu_vis {
        apply_gpu_visibility(&layout, &mut pages, v);
    }
    let strength = if mode == BakeMode::Preview { 0.5 } else { 0.85 };
    for p in &mut pages {
        denoise::denoise_page(scene, &layout, p, 2, strength);
    }
    // Every texel a bilinear tap at a chart's edge can reach takes the chart's
    // own light, sky direction and sun visibility; left at zero, the padding
    // darkened every chart edge and speckled charts only a few texels wide.
    for p in &mut pages {
        let covered = page_coverage(scene, &layout, p);
        denoise::fill_uncovered(p, &covered, FILL_RADIUS);
    }
    report.denoised_texels = layout.charts.iter().map(|c| (c.inner[0] * c.inner[1]) as usize).sum();

    let probe_set = if scene.probes_ignore_props && !scene.prop_occluders.is_empty() {
        // A shipped map puts a prop's probe at its lighting origin, inside the
        // prop, so the probes are traced without the props in the way.
        let bare = rir::BakeScene { prop_occluders: Vec::new(), ..scene.clone() };
        let bare_geometry = bvh::scene_geometry(&bare)?;
        let bare_bvh = Bvh::build_scene(&bare_geometry)?;
        let bare_albedo = triangle_albedo(&bare, &bare_geometry, albedo);
        let bare_ctx = light::DirectContext { bvh: &bare_bvh, tris: &bare_bvh.triangles, albedo: &bare_albedo, ..ctx };
        probes::bake_probes(scene, &bare_ctx, &bare_bvh)?
    } else {
        probes::bake_probes(scene, &ctx, &hierarchy)?
    };
    report.probes = probe_set.probes.len();
    let casters = shadow_casters(scene, &geometry);
    report.shadow_casters = casters.len();
    let directions = cascade_directions(scene);
    let prop_casters = prop_casters(scene);
    let cascade = csm::build_csm(&casters, &prop_casters, scene.world_mins, scene.world_maxs, &directions)?;
    report.shadow_refs = cascade.refs.len();
    report.shadow_triangles = csm::declared_triangles(&cascade.meshes)?;
    let open = |p: Vec3| !hierarchy.inside_solid(p);
    let spacing = if scene.settings.cubemap_spacing > 0.0 {
        scene.settings.cubemap_spacing
    } else {
        DEFAULT_CUBEMAP_SPACING
    };
    let floors = |x: f32, y: f32| cubemap_floors(&hierarchy, x, y, scene.world_mins.z, scene.world_maxs.z);
    let positions =
        cubemap::place_cubemaps(scene.world_mins, scene.world_maxs, spacing, &scene.cubemap_points, &open, &floors)?;
    let size = if mode == BakeMode::Preview { PREVIEW_CUBEMAP_SIZE } else { cubemap::DEFAULT_CUBEMAP_SIZE };
    let cube_meter = progress::Meter::new("cubemaps", "cubemaps", positions.len() as u64, 86, 89);
    let render = |p: &Vec3| {
        let m = cubemap::render_cubemap(*p, size, &ctx, &hierarchy);
        cube_meter.tick(1);
        m
    };
    #[cfg(feature = "parallel")]
    let mut maps: Vec<BakedCubemap> = {
        use rayon::prelude::*;
        positions.par_iter().map(render).collect::<Result<_>>()?
    };
    #[cfg(not(feature = "parallel"))]
    let mut maps: Vec<BakedCubemap> = positions.iter().map(render).collect::<Result<_>>()?;
    cube_meter.finish();
    if mode != BakeMode::Preview {
        if let Some(captured) = capture::from_env()? {
            report.cubemaps_captured = apply_capture(&mut maps, &captured, scene, &probe_set);
        }
    }
    report.cubemaps = maps.len();
    Ok(Lighting {
        mode,
        layout,
        pages,
        world_lights: world,
        tweak_lights: tweak,
        light_groups: groups,
        probes: probe_set,
        csm: cascade,
        cubemaps: Cubemaps { maps },
        albedo: albedo_rgb,
        report,
    })
}

/// Albedo per triangle the hierarchy keeps, in its order: a surface takes its
/// material's entry, a prop occluder its model's. Empty when the table is, so a
/// map without one bakes as before.
fn triangle_albedo(scene: &rir::BakeScene, geometry: &bvh::SceneGeometry, albedo: &AlbedoTable) -> Vec<Vec3> {
    if albedo.entries.is_empty() {
        return Vec::new();
    }
    geometry
        .tris
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_finite())
        .map(|(i, _)| {
            if let Some(s) = geometry.surface.get(i).and_then(|&si| scene.surfaces.get(si as usize)) {
                return albedo.for_surface(s);
            }
            geometry
                .prop
                .get(i)
                .and_then(|&pi| scene.prop_occluders.get(pi as usize))
                .and_then(|o| scene.props.get(o.0 as usize))
                .map_or(light::DEFAULT_ALBEDO, |p| albedo.for_model(&p.model))
        })
        .collect()
}

/// Texels past a chart's covered area that take its edge values: the padding
/// and any texel inside the chart no triangle reached.
const FILL_RADIUS: u32 = 4;

/// Which texels of a page some chart's triangles reach.
fn page_coverage(scene: &rir::BakeScene, layout: &ChartLayout, page: &BakedPage) -> Vec<bool> {
    let (w, h) = (page.width as usize, page.height as usize);
    let mut out = vec![false; w * h];
    for c in layout.charts.iter().filter(|c| c.page == page.index) {
        let Some(s) = scene.surfaces.get(c.surface) else { continue };
        let iw = c.inner[0] as usize;
        for (i, t) in light::rasterize(c, s).iter().enumerate() {
            if t.is_none() {
                continue;
            }
            let (x, y) = (c.origin[0] as usize + i % iw, c.origin[1] as usize + i / iw);
            if x < w && y < h {
                out[y * w + x] = true;
            }
        }
    }
    out
}

/// Shades every page, one page per thread when the `parallel` feature is on.
fn shade_pages(
    scene: &rir::BakeScene,
    ctx: &light::DirectContext,
    layout: &ChartLayout,
    pages: &mut [BakedPage],
    albedo: &AlbedoTable,
    mode: BakeMode,
    rtl: &rtl::RtlSlots,
    meter: &progress::Meter,
) -> Result<()> {
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        pages
            .par_iter_mut()
            .try_for_each(|p| light::shade_page(scene, ctx, layout, p, albedo, mode, rtl, Some(meter)))?;
    }
    #[cfg(not(feature = "parallel"))]
    for p in pages.iter_mut() {
        light::shade_page(scene, ctx, layout, p, albedo, mode, rtl, Some(meter))?;
    }
    Ok(())
}

/// The flat light a page with no bake carries.
fn ambient_term(scene: &rir::BakeScene) -> [f32; 3] {
    let v = scene.settings.sky_color * scene.settings.sky_intensity * 0.5;
    [v.x.max(0.0), v.y.max(0.0), v.z.max(0.0)]
}

/// The probe, cascade and cubemap set a map with no bake still needs.
fn minimal_products(scene: &rir::BakeScene, bvh: &Bvh) -> Result<(Probes, Csm, Cubemaps)> {
    let centre = (scene.world_mins + scene.world_maxs) * 0.5;
    let probes = Probes {
        probes: vec![Probe {
            position: centre,
            ambient: probes::default_ambient(&scene.settings),
            sky: probes::default_sky(probes::sun_direction(&scene.lights)),
            ..Probe::default()
        }],
        prop_indices: scene.props.iter().map(|_| 0u32).collect(),
        ..probes::single_probe_index(centre)
    };
    let direction = csm::FALLBACK_SHADOW_DIRECTION;
    let csm = csm::empty_csm(scene.world_mins, scene.world_maxs, direction)?;
    let open = |p: Vec3| !bvh.inside_solid(p);
    // The designer's own points replace the single centre cubemap, so a map
    // with no bake still reflects where its author placed them. Placed points
    // are kept as they are: culling one would renumber every prop's cubemap
    // slot behind it.
    let mut positions: Vec<Vec3> = scene.cubemap_points.iter().copied().filter(|p| p.is_finite()).collect();
    if positions.is_empty() {
        positions.push(if open(centre) { centre } else { scene.world_mins + Vec3::splat(1.0) });
    }
    let maps = positions
        .into_iter()
        .map(|position| BakedCubemap {
            position,
            ambient_scale: cubemap::DEFAULT_AMBIENT_SCALE,
            faces: minimal_faces(&scene.settings, cubemap::DEFAULT_CUBEMAP_SIZE),
            size: cubemap::DEFAULT_CUBEMAP_SIZE,
        })
        .collect();
    Ok((probes, csm, Cubemaps { maps }))
}

/// Six faces of flat sky, which is all a map with no bake can carry.
fn minimal_faces(settings: &rir::BakeSettings, size: u32) -> Vec<Vec<Vec3>> {
    let n = (size * size) as usize;
    let flat = light::sky_light(settings, Vec3::Z, 1.0).max(Vec3::ZERO);
    vec![vec![flat; n]; 6]
}

/// The caster meshes of a bake: every surface and prop that casts shadows,
/// split opaque and translucent by the material.
/// Heights of the upward-facing surfaces straight below (x, y, top), highest
/// first, each with room above it for a standing player.
fn cubemap_floors(bvh: &Bvh, x: f32, y: f32, bottom: f32, top: f32) -> Vec<f32> {
    const HEADROOM: f32 = 72.0;
    let mut out = Vec::new();
    let mut from = top + 1.0;
    let mut ceiling = f32::INFINITY;
    while from > bottom && out.len() < 16 {
        let ray = bvh::Ray::new(Vec3::new(x, y, from), Vec3::NEG_Z, 0.0, from - bottom + 1.0);
        let Some(hit) = bvh.closest_hit(&ray) else { break };
        let p = hit.point(&ray);
        // Seen from straight above, a flat face is the top of whatever it
        // bounds whichever way it is wound.
        let flat = bvh.triangles.get(hit.tri as usize).map(|t| t.normal().normalize_or_zero().z.abs()).unwrap_or(0.0);
        if flat > 0.7 && ceiling - p.z >= HEADROOM {
            out.push(p.z);
        }
        ceiling = p.z;
        from = p.z - 1.0;
    }
    out
}

fn shadow_casters(scene: &rir::BakeScene, geometry: &bvh::SceneGeometry) -> Vec<csm::ShadowCaster> {
    let mut out: Vec<csm::ShadowCaster> = Vec::new();
    for s in &scene.surfaces {
        if !s.cast_shadows || s.positions.is_empty() || s.indices.is_empty() {
            continue;
        }
        let mut positions: Vec<[f32; 3]> = Vec::with_capacity(s.positions.len());
        for p in &s.positions {
            if !p.is_finite() {
                return Vec::new();
            }
            positions.push([p.x, p.y, p.z]);
        }
        let alpha = s.material.to_ascii_lowercase().contains("glass")
            || s.material.to_ascii_lowercase().contains("transparent")
            || s.material.to_ascii_lowercase().contains("window");
        out.extend(csm::split_caster(positions, &s.indices, alpha));
    }
    for (positions, indices) in &scene.shadow_only {
        if positions.len() > u16::MAX as usize || positions.iter().any(|p| !p.is_finite()) {
            continue;
        }
        let verts: Vec<[f32; 3]> = positions.iter().map(|p| [p.x, p.y, p.z]).collect();
        let idx: Vec<u16> = indices.iter().filter(|&&i| (i as usize) < verts.len()).map(|&i| i as u16).collect();
        if !idx.is_empty() && idx.len().is_multiple_of(3) {
            out.push(csm::ShadowCaster { positions: verts, indices: idx, alpha: false });
        }
    }
    // Props cast through their own cascade refs (`prop_casters`). A prop mesh
    // here would also push the shadow mesh count past the first static prop
    // index, where the engine reads every prop ref as a shadow mesh.
    let _ = geometry;
    out
}

/// Every shadow-casting static prop with its world box, for the cascade tree.
fn prop_casters(scene: &rir::BakeScene) -> Vec<csm::PropCaster> {
    let mut out = Vec::new();
    for (i, p) in scene.props.iter().enumerate() {
        let Some(&lump_index) = scene.prop_lump_index.get(i) else { continue };
        if !p.cast_shadows || !p.bounds_min.cmple(p.bounds_max).all() {
            continue;
        }
        let (pitch, yaw, roll) = (p.angles.x.to_radians(), p.angles.y.to_radians(), p.angles.z.to_radians());
        let rot = glam::Quat::from_euler(glam::EulerRot::ZYX, yaw, pitch, roll);
        let scale = if p.scale > 0.0 { p.scale } else { 1.0 };
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for k in 0..8 {
            let c = Vec3::new(
                if k & 1 == 0 { p.bounds_min.x } else { p.bounds_max.x },
                if k & 2 == 0 { p.bounds_min.y } else { p.bounds_max.y },
                if k & 4 == 0 { p.bounds_min.z } else { p.bounds_max.z },
            );
            let w = p.origin + rot * (c * scale);
            lo = lo.min(w);
            hi = hi.max(w);
        }
        if lo.is_finite() && hi.is_finite() {
            out.push(csm::PropCaster { lump_index, mins: lo, maxs: hi });
        }
    }
    out
}

/// One cascade per shadow-casting directional light, in scene order.
fn cascade_directions(scene: &rir::BakeScene) -> Vec<[f32; 3]> {
    let mut out: Vec<[f32; 3]> = Vec::new();
    for l in &scene.lights {
        if l.kind != rir::LightType::Sun || !l.cast_shadows {
            continue;
        }
        let d = l.direction.normalize_or_zero();
        if d.length_squared() < 0.5 || d.z.abs() > 0.999 {
            continue;
        }
        out.push([d.x, d.y, d.z]);
    }
    out
}

/// Writes a bake into a map, returning the cubemap texture to package.
pub fn emit(l: &Lighting, bsp: &mut rbsp::Bsp, target: Version) -> Result<Vec<CubemapTexture>> {
    if target != Version::ClientV51 && target != Version::DediV47 {
        return Err(Error::Target(target));
    }
    let lumps = to_lumps(l, target)?;
    lumps.encode(bsp, target)?;
    if target != Version::ClientV51 {
        return Ok(Vec::new());
    }
    if l.cubemaps.maps.is_empty() {
        return Ok(Vec::new());
    }
    let positions: Vec<glam::Vec3> = l.cubemaps.maps.iter().map(|m| m.position).collect();
    cubemap::assign_indices(bsp, &positions)?;
    let name = "cubemaps_hdr".to_string();
    let tex = cubemap::build_texture(&l.cubemaps.maps, &name)?;
    Ok(vec![tex])
}

/// The lump set a bake emits.
pub fn to_lumps(l: &Lighting, target: Version) -> Result<LightingLumps> {
    let mut out = LightingLumps {
        world_lights: l.world_lights.clone(),
        tweak_lights: l.tweak_lights.clone(),
        ..LightingLumps::default()
    };
    if target != Version::ClientV51 {
        return Ok(out);
    }
    out.parent_infos = l.probes.parent_infos.clone();
    out.probes = l.probes.probes.iter().map(|p| p.to_record()).collect();
    out.prop_probe_indices = l.probes.prop_indices.clone();
    out.probe_tree = l.probes.tree.clone();
    out.probe_refs = l.probes.refs.clone();
    out.shadow_envs = l.csm.envs.iter().map(|e| e.to_record()).collect();
    out.csm_nodes = l.csm.nodes.clone();
    out.csm_refs = l.csm.refs.clone();
    out.csm_totals = l.csm.totals.clone();
    out.shadow_meshes = l.csm.meshes.clone();
    out.shadow_opaque = l.csm.opaque.clone();
    out.shadow_alpha = l.csm.alpha.clone();
    out.shadow_indices = l.csm.indices.clone();
    out.cubemaps = l.cubemaps.records();
    out.cubemap_ambient = l.cubemaps.ambient();
    for (i, p) in l.pages.iter().enumerate() {
        out.lightmap_headers.push(lmp::LightmapHeader {
            // Format 1 is eight bytes per texel: RGB then the sun visibility.
            lightmap_type: 1,
            // The compressed plane is not written, so its type stays zero.
            compressed_type: 0,
            tag: 0,
            pad: 0,
            width: p.width,
            height: p.height,
        });
        out.sky_pages.push(SkyPage {
            width: p.width,
            height: p.height,
            rgba: p.encode(),
        });
        out.rtl_pages.push(p.rtl.clone());
        let _ = i;
    }
    out.rtl_groups = rtl::group_table(&l.light_groups);
    Ok(out)
}

/// The editor's per-object view of the bake: every chart of an object
/// composited into one image in page texel space.
pub fn preview_rgba(l: &Lighting) -> BTreeMap<Uuid, (u32, u32, Vec<u8>)> {
    let mut lo: BTreeMap<Uuid, [u32; 2]> = BTreeMap::new();
    let mut hi: BTreeMap<Uuid, [u32; 2]> = BTreeMap::new();
    for c in &l.layout.charts {
        let key = chart_key(c);
        let e = lo.entry(key).or_insert([u32::MAX, u32::MAX]);
        e[0] = e[0].min(c.origin[0]);
        e[1] = e[1].min(c.origin[1]);
        let e = hi.entry(key).or_insert([0, 0]);
        e[0] = e[0].max(c.origin[0] + c.size[0]);
        e[1] = e[1].max(c.origin[1] + c.size[1]);
    }
    let mut out: BTreeMap<Uuid, (u32, u32, Vec<u8>)> = BTreeMap::new();
    for (key, a) in &lo {
        let b = hi.get(key).copied().unwrap_or([0, 0]);
        let w = b[0].saturating_sub(a[0]).max(1);
        let h = b[1].saturating_sub(a[1]).max(1);
        out.insert(*key, (w, h, vec![0u8; (w * h * 4) as usize]));
    }
    for c in &l.layout.charts {
        let key = chart_key(c);
        let Some(page) = l.pages.iter().find(|p| p.index == c.page) else { continue };
        let origin = lo.get(&key).copied().unwrap_or([0, 0]);
        let Some((w, h, buf)) = out.get_mut(&key) else { continue };
        for y in 0..c.size[1] {
            for x in 0..c.size[0] {
                let px = c.origin[0] + x;
                let py = c.origin[1] + y;
                if px >= page.width as u32 || py >= page.height as u32 {
                    continue;
                }
                let sx = px.saturating_sub(origin[0]);
                let sy = py.saturating_sub(origin[1]);
                if sx >= *w || sy >= *h {
                    continue;
                }
                let dst = ((sy as usize) * *w as usize + sx as usize) * 4;
                if dst + 4 <= buf.len() {
                    // The preview shows baked light plus the sun the page leaves out.
                    let c = page.colour(px as usize, py as usize) + Vec3::splat(page.sun(px as usize, py as usize) * 0.5);
                    buf[dst] = (c.x.clamp(0.0, 1.0) * 255.0) as u8;
                    buf[dst + 1] = (c.y.clamp(0.0, 1.0) * 255.0) as u8;
                    buf[dst + 2] = (c.z.clamp(0.0, 1.0) * 255.0) as u8;
                    buf[dst + 3] = 255;
                }
            }
        }
    }
    out
}

/// A chart's key: the object it came from, or a value derived from the surface
/// index when it has no object.
fn chart_key(c: &Chart) -> Uuid {
    c.object.unwrap_or_else(|| Uuid::from_u128(0x5242_414b_4500_0000_0000_0000_0000_0000u128 + c.surface as u128))
}

fn apply_gpu_visibility(layout: &ChartLayout, pages: &mut [BakedPage], vis: &gpu::Visibility) {
    for chart in &layout.charts {
        let Some(page) = pages.iter_mut().find(|p| p.index == chart.page) else {
            continue;
        };
        for y in 0..chart.size[1] {
            for x in 0..chart.size[0] {
                let px = chart.origin[0] + x;
                let py = chart.origin[1] + y;
                let Some(v) = vis.get(chart.page, px, py) else { continue };
                if v <= 0.0 {
                    continue;
                }
                let cur = page.sun(px as usize, py as usize);
                page.set(px as usize, py as usize, page.colour(px as usize, py as usize) * v, cur * v);
            }
        }
    }
}

/// Replaces every rendered cubemap the engine captured with its capture, and
/// brings the rest to the capture's face size so the array stays uniform.
fn apply_capture(maps: &mut [BakedCubemap], captured: &capture::CapturedSet, scene: &rir::BakeScene,
    probe_set: &probes::Probes) -> usize {
    let Some(size) = captured.cubemaps.first().map(|c| c.face_size()) else {
        return 0;
    };
    let sky = scene.settings.sky_color * scene.settings.sky_intensity;
    let mut used = 0;
    for m in maps.iter_mut() {
        match captured.at(m.position) {
            Some(c) => {
                let (open, ambient) = probe_set
                    .nearest(m.position)
                    .and_then(|i| probe_set.probes.get(i as usize))
                    .map(|p| {
                        // Openness: the length of the probe's sky vector (words 0-2).
                        let v = Vec3::new(p.sky[0] as f32, p.sky[1] as f32, p.sky[2] as f32) / probes::SKY_WORD_SCALE;
                        (v.length().min(1.0), probes::probe_ambient_rgb(p))
                    })
                    .unwrap_or((1.0, Vec3::ZERO));
                *m = c.to_baked(m.position, size, capture::ambient_scale(open, sky, ambient));
                used += 1;
            }
            None => *m = cubemap::resized(m, size),
        }
    }
    used
}
