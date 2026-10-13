//! rBSP compiler: document -> client v51 + dedicated-server v47 maps.
//!
//! One core serves the browser editor, the host and the CLI, so everything a
//! compile needs arrives through [`ProjectFs`] and [`AssetInfo`] and the
//! output is a function of the document alone.

pub mod collision;
pub mod csg;
pub mod decompile;
pub mod emit;
pub mod entities;
pub mod meshes;
pub mod nav;
pub mod overlay;
pub mod props;
pub mod report;
pub mod sky;
pub mod environment;
pub mod splice;
pub mod tf2;
pub mod vis;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::OnceLock;

use glam::{DVec3, Vec2, Vec3, Vec4};
use rbake::{BakeMode, Backend};
use rbsp::lumps::collision::Model;
use rbsp::{Bsp, Version};
use rdoc::model::{
    CollisionMode, Id, LightKind, MapDoc, Object, ObjectKind, Partition, Transform,
};
use rdoc::schema::{EntitySchema, Severity};
use rir::{
    AssetTables, BakeLight, BakeScene, BakeSettings, CollisionShape, CollisionSolid, LightType, ModelSlot,
    RenderSurface,
};
use uuid::Uuid;

use crate::csg::WorldMap;
use crate::nav::NavSoup;
use crate::report::{Report, Stats};

/// The project's files, as the compiler reads them.
pub trait ProjectFs {
    fn read(&self, rel: &str) -> anyhow::Result<Vec<u8>>;
}

/// The asset metadata a compile needs.
pub trait AssetInfo {
    fn material(&self, path: &str) -> Option<rir::MaterialInfo>;
    fn model(&self, path: &str) -> Option<rir::ModelInfo>;
    fn surface_prop(&self, name: &str) -> Option<u32>;
}

impl AssetInfo for AssetTables {
    fn material(&self, path: &str) -> Option<rir::MaterialInfo> {
        self.materials.get(path).cloned()
    }

    fn model(&self, path: &str) -> Option<rir::ModelInfo> {
        self.models.get(path).cloned()
    }

    fn surface_prop(&self, name: &str) -> Option<u32> {
        self.surface_props.get(name).copied()
    }
}

/// Which engine builds the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Targets {
    pub client: bool,
    pub dedi: bool,
}

impl Default for Targets {
    fn default() -> Self {
        Self { client: true, dedi: true }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompileOptions {
    pub targets: Targets,
    pub bake: BakeMode,
    /// Directory a `Reference` object is resolved against.
    pub reference_root: Option<PathBuf>,
    /// Leaves out the menu and ceremony rooms every shipped map carries.
    pub skip_rooms: bool,
    /// Static prop meshes the bake traces against; without them props never
    /// shade the lightmaps under and around them.
    pub prop_meshes: PropMeshes,
}

/// One model's render triangles in model space, on the placement axes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PropMesh {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
}

/// Static prop meshes by lower-case model path (`mdl/.../name.rmdl`).
#[derive(Clone, Default, PartialEq)]
pub struct PropMeshes(pub std::sync::Arc<BTreeMap<String, PropMesh>>);

impl std::fmt::Debug for PropMeshes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PropMeshes({} models)", self.0.len())
    }
}

/// One target's map: the container plus the partition files beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct MapArtifacts {
    pub bsp: Bsp,
    /// Partition name per file, in `Partition::FILES` order.
    pub ent_files: Vec<(Partition, String)>,
    /// `partition name -> bytes`, the form the writer and the validator take.
    pub ent_bytes: BTreeMap<String, Vec<u8>>,
    /// The cubemap array the map's cubemaps sample, which ships in the map's
    /// own pak; the client map has one when the bake placed any cubemap.
    pub cubemap: Option<rbake::CubemapTexture>,
}

impl MapArtifacts {
    /// Every entity block the map carries, main lump first.
    pub fn entity_count(&self) -> usize {
        rbsp::lumps::entities::count_blocks(self.bsp.lump(rbsp::ids::ENTITIES))
            + self.ent_bytes.values().map(|v| rbsp::lumps::entities::count_blocks(v)).sum::<usize>()
    }
}

#[derive(Debug, PartialEq)]
pub struct CompileOutput {
    pub client: Option<MapArtifacts>,
    pub dedi: Option<MapArtifacts>,
    pub nav_soup: NavSoup,
    pub report: Report,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Project(String),
    #[error("{0}")]
    Geometry(#[from] csg::CsgError),
    #[error("{0}")]
    Meshes(#[from] meshes::MeshError),
    #[error("{0}")]
    Collision(#[from] collision::CollError),
    #[error("{0}")]
    Entities(#[from] entities::EntityError),
    #[error("{0}")]
    Props(#[from] props::PropError),
    #[error("{0}")]
    Vis(#[from] vis::VisError),
    #[error("{0}")]
    Overlay(#[from] overlay::OverlayError),
    #[error("{0}")]
    Nav(#[from] nav::NavError),
    #[error("lighting: {0}")]
    Bake(#[from] rbake::Error),
    #[error("{0}")]
    Sky(#[from] sky::SkyError),
    #[error("terrain: {0}")]
    Terrain(String),
    #[error("mesh of object {object}: {message}")]
    Mesh { object: Uuid, message: String },
    #[error("the entity schema reports {count} errors, first: {first}")]
    Schema { count: usize, first: String },
    #[error("the {target} map is invalid: {message}")]
    Invalid { target: &'static str, message: String },
}

/// The entity schema a project without one falls back to.
/// The vista a map without one of its own gets, so every map has a sky.
pub const DEFAULT_SKY: &str = "mdl/vistas/eden_se.rmdl";

/// The sky vista an install build gives a map: its own, the default when it
/// names none, or no sky at all when it names `none`. A Source skybox is the
/// sky on its own, so a map naming one gets no default vista to poke through
/// its cube. The compiler itself only builds the sky a document names, and
/// fails when that vista is not found.
pub fn sky_vista(doc: &MapDoc) -> Option<String> {
    match doc.meta.sky.as_deref().map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("none") => None,
        Some(v) if !v.is_empty() => Some(v.to_string()),
        _ if doc.meta.source_sky.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) => None,
        _ => Some(DEFAULT_SKY.to_string()),
    }
}

pub fn default_schema() -> &'static EntitySchema {
    static SCHEMA: OnceLock<EntitySchema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        EntitySchema::load(ENTITY_SCHEMA_JSON).expect("the bundled entity schema parses")
    })
}

const ENTITY_SCHEMA_JSON: &str = include_str!("../../../schema/entities.json");

/// How far the sky portals stand out from the world's bounds.
const SKY_PORTAL_PAD: f32 = 16384.0;
/// The coordinate range a shipped map stays inside.
const WORLD_EXTENT: f32 = 65535.0;

/// Reads the project's schema, falling back to the bundled one.
pub fn load_schema(fs: &dyn ProjectFs) -> Result<EntitySchema, Error> {
    match fs.read("schema/entities.json") {
        Ok(bytes) => EntitySchema::load(&String::from_utf8_lossy(&bytes))
            .map_err(|m| Error::Project(format!("schema/entities.json: {m}"))),
        Err(_) => Ok(default_schema().clone()),
    }
}

/// Compiles a document into every requested target.
pub fn compile(
    doc: &MapDoc,
    fs: &dyn ProjectFs,
    assets: &dyn AssetInfo,
    opts: &CompileOptions,
) -> Result<CompileOutput, Error> {
    let started = report::Clock::now();
    let mut report = Report::default();
    let schema = load_schema(fs)?;
    report.record("schema", started);

    // A document on top of a shipped map compiles as that map with the edits
    // spliced into it, when the edits are ones the splice can place.
    if splice::applies(doc) {
        let issues = validate(&splice::authored_doc(doc), &schema);
        if let Some(first) = issues.first() {
            return Err(Error::Schema { count: issues.len(), first: first.message.clone() });
        }
        return splice::compile(doc, fs, &schema, assets, opts);
    }

    let stage = report::Clock::now();
    let mut resolved = resolve_document(doc, fs, opts, &mut report)?;
    let doc = &resolved.doc;
    report.record("resolve", stage);

    let stage = report::Clock::now();
    let issues = validate(doc, &schema);
    if let Some(first) = issues.first() {
        return Err(Error::Schema { count: issues.len(), first: first.message.clone() });
    }
    report.record("validate", stage);

    let stage = report::Clock::now();
    let tables = asset_tables(assets, doc);
    let worlds = &resolved.worlds;
    let mut stats = Stats::default();
    count_objects(doc, &mut stats);
    report.record("measure", stage);

    let stage = report::Clock::now();
    let geo = csg::build_geometry(doc, worlds)?;
    let shadow_only = csg::shadow_only_faces(&geo.faces);
    let mut surfaces = geo.surfaces;
    let mut solids = geo.solids;
    let mut models = geo.models;
    let extra = build_extra_geometry(doc, fs, worlds, &tables, &mut stats)?;
    surfaces.extend(extra.surfaces);
    solids.extend(extra.solids);
    stats.faces = geo.faces.len();
    report.record("geometry", stage);

    let mut model_of = csg::object_model(doc);
    let mut reference_props: Vec<props::PropSource> = Vec::new();
    if let Some(reference) = &resolved.reference {
        // The world model is shared: the reference's model zero is the world it
        // declared, and it merges with the world the document authors, so only
        // the reference's own models move up behind the authored ones.
        let offset = reference.model_count().saturating_sub(1);
        let shift = |m: u32| if m == 0 { 0 } else { m + offset };
        for v in model_of.values_mut() {
            *v = shift(*v);
        }
        let mut r = reference.clone();
        for s in &mut r.surfaces {
            s.model = shift(s.model);
        }
        for s in &mut r.solids {
            s.model = shift(s.model);
        }
        let mut r_models: Vec<ModelSlot> = r.models.iter().filter(|m| m.index != 0).cloned().collect();
        for m in r_models.iter_mut() {
            m.index += offset;
            m.owner = None;
        }
        surfaces = r.surfaces.into_iter().chain(surfaces).collect();
        solids = r.solids.into_iter().chain(solids).collect();
        models.append(&mut r_models);
        reference_props = reference.props.clone();
    }

    let stage = report::Clock::now();
    let mut authored_props = collect_props(doc, worlds, fs, &tables, &mut stats)?;
    // A reference map's props, and a decompiled map's (every record carries
    // its original fields), keep the order their compiler gave them.
    if !authored_props.iter().all(|p| p.keys.contains_key("_sprp_unknown_1")) {
        props::sort_by_draw_class(&mut authored_props);
    }
    let mut all_props = reference_props;
    all_props.extend_from_slice(&authored_props);
    // A solid prop's collision is a leaf in the world tree that names it by
    // its place in the prop list.
    solids.extend(all_props.iter().enumerate().filter_map(|(i, p)| props::prop_solid(p, i as u32)));
    report.record("props", stage);

    let playable = resolved.world;
    let rooms = if opts.skip_rooms { Vec::new() } else { environment::plan_basement(doc, playable.0.as_dvec3(), playable.1.as_dvec3()) };
    let room_points: Vec<DVec3> = rooms.iter().flat_map(|r| r.points()).collect();
    for p in &room_points {
        resolved.world.0 = resolved.world.0.min(p.as_vec3());
        resolved.world.1 = resolved.world.1.max(p.as_vec3());
    }
    let sky = match doc.meta.sky.as_deref().map(str::trim) {
        Some(vista) if !vista.is_empty() && !vista.eq_ignore_ascii_case("none") => {
            Some(sky::build(vista, &surfaces, &all_props, &room_points, &tables)?)
        }
        _ => None,
    };
    // A Source skybox draws on a cube at the vista: around the vista when the
    // map names one, else around a vista position of its own.
    let source = source_sky(doc, &sky, &surfaces, &all_props, &tables, &mut report);
    let mut sky = sky;
    if let (Some(s), Some(src)) = (sky.as_mut(), source.as_ref()) {
        // The shell predates the cube, so it is rebuilt over both.
        let (lo, hi) = sky::world_bounds(&surfaces, &all_props);
        let (vlo, vhi) = s.vista.world_bounds();
        let mut extra = room_points.clone();
        extra.extend([src.bounds.0.as_dvec3(), src.bounds.1.as_dvec3()]);
        s.shell = sky::shell_for((lo, hi), &extra, (vlo.as_dvec3(), vhi.as_dvec3()), s.camera);
    }
    let camera = sky.as_ref().map(|s| s.camera).or(source.as_ref().map(|s| s.camera));
    let sky_doc = match (sky, source) {
        (None, None) => None,
        (sky, source) => {
            if let Some(s) = &sky {
                // The world box the entities declare has to hold the sky too: the
                // shell encloses both the playable space and the vista.
                for p in &s.shell.positions {
                    resolved.world.0 = resolved.world.0.min(*p);
                    resolved.world.1 = resolved.world.1.max(*p);
                }
            }
            if let Some(src) = &source {
                for surf in &src.surfaces {
                    for p in &surf.positions {
                        resolved.world.0 = resolved.world.0.min(*p);
                        resolved.world.1 = resolved.world.1.max(*p);
                    }
                }
            }
            if let Some(s) = sky {
                surfaces.push(s.shell.clone());
                solids.push(sky::shell_solid(&s.shell));
                all_props.push(s.vista.clone());
            } else if let Some(src) = &source {
                let (lo, hi) = sky::world_bounds(&surfaces, &all_props);
                let shell =
                    sky::shell_for((lo, hi), &room_points, (src.bounds.0.as_dvec3(), src.bounds.1.as_dvec3()), src.camera);
                surfaces.push(shell.clone());
                solids.push(sky::shell_solid(&shell));
            }
            if let Some(src) = source {
                surfaces.extend(src.surfaces);
            }
            Some(sky::with_camera(doc, camera.unwrap()))
        }
    };
    let env_doc = environment::with_basement(
        &environment::with_baseline(sky_doc.as_ref().unwrap_or(doc), playable.0.as_dvec3(), playable.1.as_dvec3()),
        &rooms,
    );
    let target_doc = &env_doc;
    let bake_doc = environment::with_basement(doc, &rooms);

    let stage = report::Clock::now();
    let mut scene = bake_scene(&bake_doc, &surfaces, &all_props);
    scene.shadow_only = shadow_only;
    add_prop_occluders(&mut scene, &opts.prop_meshes, &bake_doc.settings.bake.occluder_skip, &mut report);
    let lighting = rbake::bake_with_albedo(&scene, opts.bake, Backend::Cpu, &albedo_table(&bake_doc.settings.bake))?;
    report.record("bake", stage);

    let mut output = CompileOutput {
        client: None,
        dedi: None,
        nav_soup: NavSoup::default(),
        report: std::mem::take(&mut report),
    };
    if opts.targets.client {
        let (a, s) = build_target(
            Version::ClientV51,
            target_doc,
            &schema,
            &tables,
            &resolved,
            &surfaces,
            &solids,
            &models,
            &model_of,
            &all_props,
            &lighting,
            &mut output.report,
        )?;
        output.report.stats.merge(&s);
        output.client = Some(a);
    }
    if opts.targets.dedi {
        let (a, s) = build_target(
            Version::DediV47,
            target_doc,
            &schema,
            &tables,
            &resolved,
            &surfaces,
            &solids,
            &models,
            &model_of,
            &all_props,
            &lighting,
            &mut output.report,
        )?;
        output.report.stats.merge(&s);
        output.dedi = Some(a);
    }
    let stage = report::Clock::now();
    let empty = emit::new_map(Version::ClientV51);
    let soup_map = output.client.as_ref().map(|a| &a.bsp).unwrap_or(&empty);
    output.nav_soup = nav::build(
        soup_map,
        &all_props.iter().filter(|p| !p.sky).map(|p| p.instance()).collect::<Vec<_>>(),
        &tables,
        doc.settings.navmesh.bounds,
    )?;
    output.report.stats.nav_triangles = output.nav_soup.triangles();
    output.report.record("nav", stage);
    Ok(output)
}

/// Per-object lightmap images of a preview bake, keyed by document object id.
///
/// The bake always runs in preview mode and no map is emitted: the caller wants
/// the picture, not the container.
pub type BakedFiles = BTreeMap<Id, (u32, u32, Vec<u8>)>;

pub fn preview_lightmaps(
    doc: &MapDoc,
    fs: &dyn ProjectFs,
    assets: &dyn AssetInfo,
    opts: &CompileOptions,
) -> Result<BakedFiles, Error> {
    let mut report = Report::default();
    let schema = load_schema(fs)?;
    let resolved = resolve_document(doc, fs, opts, &mut report)?;
    let doc = &resolved.doc;
    if let Some(first) = validate(doc, &schema).first() {
        return Err(Error::Schema { count: 1, first: first.message.clone() });
    }
    let tables = asset_tables(assets, doc);
    let worlds = &resolved.worlds;
    let mut stats = Stats::default();
    let geo = csg::build_geometry(doc, worlds)?;
    let shadow_only = csg::shadow_only_faces(&geo.faces);
    let mut surfaces = geo.surfaces;
    let extra = build_extra_geometry(doc, fs, worlds, &tables, &mut stats)?;
    surfaces.extend(extra.surfaces);
    let prop_sources = collect_props(doc, worlds, fs, &tables, &mut stats)?;
    let mut scene = bake_scene(doc, &surfaces, &prop_sources);
    scene.shadow_only = shadow_only;
    add_prop_occluders(&mut scene, &opts.prop_meshes, &doc.settings.bake.occluder_skip, &mut report);
    let lighting = rbake::bake_with_albedo(&scene, BakeMode::Preview, Backend::Cpu, &albedo_table(&doc.settings.bake))?;
    Ok(rbake::preview_rgba(&lighting))
}

/// The bake's albedo table from the document's settings.
fn albedo_table(b: &rdoc::model::BakeSettings) -> rbake::AlbedoTable {
    rbake::AlbedoTable { entries: b.albedo.clone() }
}

/// Puts every shadow-casting static prop that has a mesh in the bake's way,
/// solid or not (a hedge with no collision still shades the ground). The
/// probes are then traced without them: each sits inside its prop.
fn add_prop_occluders(scene: &mut BakeScene, meshes: &PropMeshes, skip: &[String], report: &mut Report) {
    if meshes.0.is_empty() {
        return;
    }
    let skip: Vec<String> = skip.iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut missing = BTreeSet::new();
    for (i, p) in scene.props.iter().enumerate() {
        let key = p.model.replace('\\', "/").to_ascii_lowercase();
        if !p.cast_shadows || skip.iter().any(|s| key.contains(s.as_str())) {
            continue;
        }
        let Some(m) = meshes.0.get(&key) else {
            missing.insert(key);
            continue;
        };
        let rot = glam::Quat::from_euler(glam::EulerRot::ZYX, p.angles.y.to_radians(), p.angles.x.to_radians(), p.angles.z.to_radians());
        let scale = if p.scale > 0.0 { p.scale } else { 1.0 };
        let positions = m.positions.iter().map(|q| p.origin + rot * (*q * scale)).collect();
        scene.prop_occluders.push((i as u32, positions, m.indices.clone()));
    }
    scene.probes_ignore_props = !scene.prop_occluders.is_empty();
    if let Some(first) = missing.first() {
        report.warn(format!("{} prop models have no mesh to block baked light with ({first} ...)", missing.len()));
    }
}

/// One target's map, from the shared geometry.
#[allow(clippy::too_many_arguments)]
fn build_target(
    target: Version,
    doc: &MapDoc,
    schema: &EntitySchema,
    tables: &AssetTables,
    resolved: &Resolved,
    surfaces: &[RenderSurface],
    solids: &[CollisionSolid],
    models: &[ModelSlot],
    model_of: &HashMap<Id, u32>,
    prop_sources: &[props::PropSource],
    lighting: &rbake::Lighting,
    report: &mut Report,
) -> Result<(MapArtifacts, Stats), Error> {
    let mut bsp = emit::new_map(target);
    let mut stats = Stats::default();

    // Both halves list the same props, the vista included, as a shipped pair
    // does; the server's copy of a map's own models ships in its server pak.
    // The vista's box is part of the cell box on both halves: the sky camera
    // sits inside it, and a server whose cells stop short of the camera never
    // sends the camera to the client.
    let mut sky_bounds: Vec<(Vec3, Vec3)> = prop_sources
        .iter()
        .filter(|p| p.sky)
        .map(|p| {
            let (lo, hi) = p.world_bounds();
            let camera = p.origin + Vec3::Z * sky::CAMERA_ABOVE_VISTA as f32;
            (lo.min(camera), hi.max(camera))
        })
        .collect();

    let stage = report::Clock::now();
    // The baker shaded each chart at `ChartLayout::uv`, so that is the
    // coordinate a vertex has to sample, and the chart's page is the one its
    // material sort names.
    let (paged, pages) = lightmap_surfaces(surfaces, &lighting.layout);
    let index = meshes::emit_render_paged(&paged, &pages, tables, &mut bsp)?;
    // The cube's faces join the vista's bounds, so the split keeps the whole
    // cube on the skybox side.
    for m in index.model_meshes(0).iter().take(index.first_sky_mesh as usize) {
        if index.sort_material.get(m.material_sort as usize).is_some_and(|m| sky::is_source_sky_material(m)) {
            sky_bounds.push((m.bounds_min, m.bounds_max));
        }
    }
    report.record("render", stage);

    let stage = report::Clock::now();
    let solids = with_surface_types(solids, tables);
    let coll = collision::emit_collision(&solids, models, &mut bsp)?;
    if coll.dropped_hulls > 0 {
        report.warn(format!("{} convex hull(s) left out: no volume once rounded to the collision grid", coll.dropped_hulls));
    }
    restore_model_bounds(&mut bsp, models, target);
    emit::emit_planes(&resolved.faces, &mut bsp, target);
    report.record("collision", stage);

    let stage = report::Clock::now();
    let first_sky = index.first_sky_mesh;
    let first_prop = rbsp::lumps::level::first_static_prop_index(first_sky);
    let sun_normal = lighting
        .world_lights
        .iter()
        .find(|l| l.light_type == rbake::worldlight::TYPE_SKYLIGHT)
        .map(|l| l.normal)
        .unwrap_or_default();
    let mut prop_stats = Stats::default();
    props::emit(prop_sources, tables, &mut bsp, &mut prop_stats)?;
    let ranges = props::MeshRanges { first_decal: index.first_decal_mesh, first_transparent: index.first_transparent_mesh, first_sky, sun_normal };
    props::write_level_info(&mut bsp, ranges, &mut prop_stats);
    report.record("static_props", stage);

    let stage = report::Clock::now();
    let ctx = entities::Context {
        doc,
        schema,
        model_of,
        worlds: &resolved.worlds,
        models,
        surfaces: &[],
        target: if target == Version::ClientV51 { collision::CollTarget::Client } else { collision::CollTarget::Dedi },
        world_bounds: resolved.world,
    };
    let authored = entities::build(&ctx)?;
    let built = match &resolved.reference {
        None => authored,
        Some(reference) => merge_reference(reference, authored),
    };
    // A block that names a model slot needs the slot to exist, as a decompiled
    // brush entity does over the shipped map's mesh-less models.
    if let Some(n) = entities::max_model_key(&built) {
        let have = bsp.lump(rbsp::ids::MODELS).len() / rbsp::lumps::collision::MODEL_SIZE;
        if n as usize + 1 > have {
            pad_models(&mut bsp, target, n as usize + 1);
        }
    }
    let num_models = models.len() as u32;
    let coll_chunks = entities::max_coll_chunk(&built);
    if coll_chunks + 1 > num_models as usize {
        // A collider blob is split into numbered chunks, and the engine reads
        // every `*N` entity key as a model index, so the models table has to
        // reach the highest chunk number. Appending mesh-less records is what
        // the reference converter does for a plane-defined volume as well.
        pad_models(&mut bsp, target, coll_chunks + 1);
    }
    bsp.set_lump(rbsp::ids::ENTITIES, entities::main_lump_text(&built).into_bytes());
    emit::emit_partitions(&built.names, &mut bsp, target);
    let mut ent_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut ent_files: Vec<(Partition, String)> = Vec::new();
    for p in built.present() {
        ent_bytes.insert(p.name().to_string(), entities::partition_bytes(&built, p, num_models.max(1)));
        ent_files.push((p, p.name().to_string()));
    }
    report.record("entities", stage);

    let stage = report::Clock::now();
    // A cell references the world one mesh at a time; sky meshes and the
    // brush-entity meshes after them are not referenced. The skybox cube hangs
    // around the vista, so the skybox cell holds its meshes the way it holds
    // the vista.
    let mut objects: Vec<vis::VisObject> = Vec::new();
    let mut sky_objects: Vec<vis::VisObject> = Vec::new();
    for (i, m) in index.model_meshes(0).iter().take(first_sky as usize).enumerate() {
        if m.bounds_min.x > m.bounds_max.x {
            continue;
        }
        let o = vis::VisObject { reference: i as u32, mins: m.bounds_min, maxs: m.bounds_max };
        if index.sort_material.get(m.material_sort as usize).is_some_and(|m| sky::is_source_sky_material(m)) {
            sky_objects.push(o);
        } else {
            objects.push(o);
        }
    }
    for (i, p) in prop_sources.iter().enumerate() {
        let (lo, hi) = p.world_bounds();
        if lo.x > hi.x {
            continue;
        }
        let o = vis::VisObject { reference: first_prop + i as u32, mins: lo, maxs: hi };
        if p.sky {
            sky_objects.push(o);
        } else {
            objects.push(o);
        }
    }
    let split = sky_split(&objects, &sky_bounds);
    let (mut wlo, mut whi) = world_bounds_of(models, &objects);
    if split.is_none() {
        for (lo, hi) in &sky_bounds {
            if lo.x <= hi.x {
                wlo = wlo.min(*lo);
                whi = whi.max(*hi);
            }
        }
        objects.append(&mut sky_objects);
    }
    let cells_and_masks = cell_volumes(doc, models, &objects);
    let mut lumps = if doc.settings.vis.portals {
        vis::portal_cells(&cells_and_masks, &[], &[], &objects)?
    } else {
        vis::single_cell(&objects, wlo, whi)?
    };
    if let Some(x) = split {
        let plane = append_plane(&mut bsp, target, [1.0, 0.0, 0.0], x);
        vis::add_sky_cell(&mut lumps, &sky_objects, plane)?;
        // The sky portals box the world with room to spare above it: a camera
        // outside a face's plane drops that face, and the sky with it.
        let pad = Vec3::splat(SKY_PORTAL_PAD);
        let limit = Vec3::splat(WORLD_EXTENT);
        let faces: Vec<(Vec<Vec3>, u32)> = vis::box_faces((wlo - pad).max(-limit), (whi + pad).min(limit))
            .into_iter()
            .map(|f| {
                let plane = append_plane(&mut bsp, target, f.normal, f.dist);
                (f.points, plane)
            })
            .collect();
        vis::add_sky_portals(&mut lumps, 0, &faces)?;
    }
    let (cells, vis_objects) = vis::write(&lumps, &mut bsp, target);
    report.record("vis", stage);

    let stage = report::Clock::now();
    let cubemap = rbake::emit(lighting, &mut bsp, target)?.into_iter().next();
    rbake::csm::resolve_prop_refs(&mut bsp, first_prop);
    report.record("lighting", stage);

    if target == Version::DediV47 {
        emit::prune_to_dedi(&mut bsp);
    }
    let stage = report::Clock::now();
    let issues = rbsp::validate(&bsp, &ent_bytes);
    for i in &issues {
        report.validation.push(format!(
            "{:?} {}: {}",
            i.severity,
            i.lump.map(|l| format!("0x{l:02x}")).unwrap_or_default(),
            i.message
        ));
    }
    if let Some(err) = issues.iter().find(|i| i.is_error()) {
        return Err(Error::Invalid {
            target: if target == Version::ClientV51 { "client" } else { "dedicated-server" },
            message: err.message.clone(),
        });
    }
    report.record("validate", stage);

    emit::collect(&bsp, target, &mut stats);
    stats.vis_cells = cells;
    stats.vis_objects = vis_objects;
    // The entity set is the same for both targets, so only the client pass
    // reports it; the dedicated pass would double every class.
    if target == Version::ClientV51 {
        stats.entities_per_class = entities::class_counts(&built);
        stats.entities = built.main.len() + built.partitions.iter().map(|p| p.len()).sum::<usize>();
        stats.partition_files = built.names.len();
        stats.prop_models = prop_stats.prop_models;
        stats.props = prop_stats.props;
    }
    stats.surfaces = surfaces.len();
    stats.render_meshes = index.meshes.len();
    stats.triangles = surfaces.iter().map(|s| s.indices.len() / 3).sum();
    stats.collision_solids = solids.len();
    Ok((MapArtifacts { bsp, ent_files, ent_bytes, cubemap }, stats))
}

/// The reference's entities first, the authored ones appended, with the
/// dropped indices removed.
fn merge_reference(reference: &overlay::Reference, authored: entities::Built) -> entities::Built {
    let kept = reference.kept_entities();
    let mut out = entities::Built {
        main: kept
            .iter()
            .filter(|(p, _)| *p == Partition::Main)
            .map(|(_, b)| b.clone())
            .chain(authored.main.iter().cloned())
            .collect(),
        partitions: authored.partitions.clone(),
        names: Vec::new(),
    };
    for p in Partition::FILES {
        let extra = authored.partition(p);
        let base = kept.iter().filter(|(q, _)| *q == p).map(|(_, b)| b.clone()).collect::<Vec<_>>();
        if base.is_empty() && extra.is_empty() {
            continue;
        }
        let slot = match p {
            Partition::Env | Partition::Main => 0,
            Partition::Fx => 1,
            Partition::Script => 2,
            Partition::Snd => 3,
            Partition::Spawn => 4,
        };
        let mut all = base;
        all.extend(extra.iter().cloned());
        out.partitions[slot] = all;
    }
    out.names = out.present().iter().map(|p| p.name().to_string()).collect();
    out
}

/// One cell per authored volume, plus the world box, so a portal-mode map has
/// the cells its hint brushes describe.
fn cell_volumes(doc: &MapDoc, models: &[ModelSlot], objects: &[vis::VisObject]) -> Vec<vis::CellBox> {
    let (wlo, whi) = world_bounds_of(models, objects);
    let mut out = vec![vis::CellBox { mask: 0x48, mins: wlo, maxs: whi }];
    let slots: Vec<&ModelSlot> = models.iter().filter(|m| m.owner.is_some() && m.mins.x <= m.maxs.x).collect();
    for (i, m) in slots.iter().enumerate() {
        if i == 0 {
            continue;
        }
        out.push(vis::CellBox { mask: 0x48, mins: m.mins, maxs: m.maxs });
    }
    let _ = doc;
    out
}

fn world_bounds_of(models: &[ModelSlot], objects: &[vis::VisObject]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for m in models {
        if m.mins.x > m.maxs.x {
            continue;
        }
        lo = lo.min(m.mins);
        hi = hi.max(m.maxs);
    }
    for o in objects {
        lo = lo.min(o.mins);
        hi = hi.max(o.maxs);
    }
    if lo.x > hi.x {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    (lo, hi)
}

/// Writes the geometry-derived model boxes back over the placeholder the
/// render stage leaves on a model with no geometry of its own.
/// The directory a `Reference` object is resolved against.
pub(crate) fn reference_root(opts: &CompileOptions) -> PathBuf {
    opts.reference_root
        .clone()
        .or_else(|| std::env::var_os("R5ED_REFERENCE_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn restore_model_bounds(bsp: &mut Bsp, models: &[ModelSlot], target: Version) {
    let mut list = match rbsp::lumps::parse(rbsp::ids::MODELS, target, bsp.lump(rbsp::ids::MODELS)) {
        Ok(rbsp::lumps::Lump::Model(m)) => m,
        _ => return,
    };
    for (i, slot) in models.iter().enumerate() {
        // The brush stage leaves a zero box on a model it has no faces for; a
        // detached map has no brushes at all, and writing that box over the
        // world's mesh bounds left model 0 at the origin.
        if slot.mins.x > slot.maxs.x || slot.mins == slot.maxs {
            continue;
        }
        if let Some(m) = list.get_mut(i) {
            if m.mins[0] <= m.maxs[0] && (m.num_meshes > 0 || m.bvh_node >= 0) {
                m.mins = Vec3::from_array(m.mins).min(slot.mins).to_array();
                m.maxs = Vec3::from_array(m.maxs).max(slot.maxs).to_array();
            } else {
                m.mins = slot.mins.to_array();
                m.maxs = slot.maxs.to_array();
            }
        }
    }
    // A model left with nothing, not even a box, is written as the shipped
    // maps write one, its mesh range starting past the last mesh.
    let meshes: u32 = list.iter().map(|m| m.num_meshes).sum();
    for m in list.iter_mut() {
        if m.num_meshes == 0 && m.bvh_node < 0 && (m.mins[0] > m.maxs[0] || m.mins == m.maxs) {
            *m = rbsp::lumps::collision::Model { first_mesh: meshes, ..rbsp::lumps::collision::Model::empty() };
        }
    }
    bsp.set_lump(rbsp::ids::MODELS, rbsp::lumps::collision::write_models(&list, target));
}

/// Extends the models table with mesh-less records up to `count`. A record with
/// no geometry and no tree is what the engine walks past.
pub(crate) fn pad_models(bsp: &mut Bsp, target: Version, count: usize) {
    let mut list = match rbsp::lumps::parse(rbsp::ids::MODELS, target, bsp.lump(rbsp::ids::MODELS)) {
        Ok(rbsp::lumps::Lump::Model(m)) => m,
        _ => return,
    };
    let meshes: u32 = list.iter().map(|m| m.num_meshes).sum();
    while list.len() < count {
        list.push(rbsp::lumps::collision::Model { first_mesh: meshes, ..rbsp::lumps::collision::Model::empty() });
    }
    bsp.set_lump(rbsp::ids::MODELS, rbsp::lumps::collision::write_models(&list, target));
}

/// The world matrix of every object, parents first.
pub fn world_transforms(doc: &MapDoc) -> WorldMap {
    let by_id: HashMap<Id, &Object> = doc.objects.iter().map(|o| (o.id, o)).collect();
    let mut out: WorldMap = HashMap::with_capacity(doc.objects.len());
    for o in &doc.objects {
        let mut m = local_matrix(&o.transform);
        let mut parent = o.parent;
        let mut guard = 0;
        while let Some(p) = parent {
            guard += 1;
            if guard > 64 {
                break;
            }
            let Some(po) = by_id.get(&p) else { break };
            m = local_matrix(&po.transform) * m;
            parent = po.parent;
        }
        out.insert(o.id, m);
    }
    out
}

fn local_matrix(t: &Transform) -> glam::DMat4 {
    glam::DMat4::from_scale_rotation_translation(t.scale, t.rotation, t.position)
}

pub(crate) fn count_objects(doc: &MapDoc, stats: &mut Stats) {
    stats.objects = doc.objects.len();
    for o in &doc.objects {
        match &o.kind {
            ObjectKind::Brush(_) => stats.brushes += 1,
            ObjectKind::Volume(_) => stats.volumes += 1,
            ObjectKind::Light(_) => stats.lights += 1,
            ObjectKind::Path(_) => stats.paths += 1,
            ObjectKind::Terrain(_) | ObjectKind::VoxelTerrain(_) => stats.terrains += 1,
            ObjectKind::Prop(p) if p.static_prop => stats.props += 1,
            _ => {}
        }
    }
}

/// The document after prefab instantiation, with the reference map resolved.
struct Resolved {
    doc: MapDoc,
    faces: Vec<csg::Face>,
    worlds: WorldMap,
    world: (Vec3, Vec3),
    reference: Option<overlay::Reference>,
}

fn resolve_document(
    doc: &MapDoc,
    fs: &dyn ProjectFs,
    opts: &CompileOptions,
    report: &mut Report,
) -> Result<Resolved, Error> {
    let mut out = MapDoc {
        format: doc.format,
        meta: doc.meta.clone(),
        layers: doc.layers.clone(),
        objects: Vec::new(),
        settings: doc.settings.clone(),
    };
    let visible: Vec<Id> = doc.layers.iter().filter(|l| l.visible).map(|l| l.id).collect();
    for o in &doc.objects {
        if o.hidden || !visible.contains(&o.layer) {
            continue;
        }
        expand_object(o, fs, &mut out.objects, 0)?;
    }
    let mut reference = None;
    for o in &out.objects {
        let ObjectKind::Reference(r) = &o.kind else { continue };
        let loaded = overlay::load(r, &reference_root(opts))?;
        report.warn(format!(
            "reference map {} contributes {} surfaces, {} props and {} entities",
            loaded.stem,
            loaded.surfaces.len(),
            loaded.props.len(),
            loaded.entities.len()
        ));
        reference = Some(loaded);
    }
    out.objects.retain(|o| !matches!(o.kind, ObjectKind::Reference(_)));
    let worlds = world_transforms(&out);
    let geo_faces = csg::build_faces(&out, &worlds)?;
    let mut world = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for f in &geo_faces {
        for p in &f.points {
            world.0 = world.0.min(p.as_vec3());
            world.1 = world.1.max(p.as_vec3());
        }
    }
    if let Some(r) = &reference {
        world.0 = world.0.min(r.world.0);
        world.1 = world.1.max(r.world.1);
    }
    if world.0.x > world.1.x {
        world = (Vec3::ZERO, Vec3::ZERO);
    }
    Ok(Resolved { doc: out, faces: geo_faces, worlds, world, reference })
}

const PREFAB_DEPTH: usize = 8;

fn expand_object(o: &Object, fs: &dyn ProjectFs, out: &mut Vec<Object>, depth: usize) -> Result<(), Error> {
    let ObjectKind::PrefabInstance { prefab } = &o.kind else {
        out.push(o.clone());
        return Ok(());
    };
    if depth >= PREFAB_DEPTH {
        return Err(Error::Project(format!("prefab {prefab} nests too deeply")));
    }
    let path =
        rdoc::prefab::prefab_path(prefab).map_err(|e| Error::Project(format!("prefab {prefab}: {e}")))?;
    let bytes = fs.read(&path).map_err(|e| Error::Project(format!("{path}: {e}")))?;
    let pf = rdoc::prefab::Prefab::from_bytes(&bytes).map_err(|e| Error::Project(format!("{path}: {e}")))?;
    let remap: HashMap<Id, Id> = pf.objects.iter().map(|p| (p.id, Uuid::new_v4())).collect();
    let instance = local_matrix(&o.transform);
    for p in &pf.objects {
        let mut child = p.clone();
        child.id = remap[&p.id];
        child.layer = o.layer;
        if let Some(parent) = p.parent {
            if let Some(next) = remap.get(&parent) {
                child.parent = Some(*next);
            }
        }
        let world = instance * local_matrix(&p.transform);
        let (s, r, t) = world.to_scale_rotation_translation();
        child.transform = Transform { position: t, rotation: r, scale: s };
        expand_object(&child, fs, out, depth + 1)?;
    }
    Ok(())
}

/// Every authoring error the entity schema knows about: an unknown class, a
/// key the class does not have, or a value of the wrong type. A key the
/// compiler fills in itself is never a missing-key error, because the document
/// is not where it is written.
fn validate(doc: &MapDoc, schema: &EntitySchema) -> Vec<rdoc::schema::Issue> {
    let mut out = Vec::new();
    for o in &doc.objects {
        let Some((mut classname, keys)) = rdoc::schema::entity_of(o) else { continue };
        if let ObjectKind::Light(l) = &o.kind {
            classname = entities::light_class(l.kind);
        }
        let Some(def) = schema.class(classname) else {
            out.push(rdoc::schema::Issue {
                severity: Severity::Error,
                object: Some(o.id),
                message: format!("classname {classname} is not a known entity class"),
            });
            continue;
        };
        for (name, value) in keys {
            match def.keys.get(name) {
                Some(k) if !k.ty.accepts(value) => out.push(rdoc::schema::Issue {
                    severity: Severity::Error,
                    object: Some(o.id),
                    message: format!("{name} expects {:?} but got {value:?}", k.ty),
                }),
                Some(_) => {}
                None => out.push(rdoc::schema::Issue {
                    severity: Severity::Warning,
                    object: Some(o.id),
                    message: format!("{classname} has no key named {name}"),
                }),
            }
        }
        if let Some(k) = def.keys.get("origin") {
            let p = o.transform.position;
            let text = format!("{} {} {}", p.x, p.y, p.z);
            if !k.ty.accepts(&text) {
                out.push(rdoc::schema::Issue {
                    severity: Severity::Error,
                    object: Some(o.id),
                    message: format!("origin expects {:?} but the transform gives {text:?}", k.ty),
                });
            }
        }
        if let Some(k) = def.keys.get("angles") {
            let a = o.transform.to_source_angles();
            let text = format!("{} {} {}", a.x, a.y, a.z);
            if !k.ty.accepts(&text) {
                out.push(rdoc::schema::Issue {
                    severity: Severity::Error,
                    object: Some(o.id),
                    message: format!("angles expects {:?} but the transform gives {text:?}", k.ty),
                });
            }
        }
    }
    out.retain(|i| i.severity == Severity::Error);
    out
}

/// Impacts, footsteps and decals read the surfaceproperties id a collision
/// face carries, so each named material resolves to its own.
fn with_surface_types(solids: &[CollisionSolid], tables: &AssetTables) -> Vec<CollisionSolid> {
    let mut out = solids.to_vec();
    for surf in out.iter_mut().flat_map(|s| s.surfaces.iter_mut()) {
        surf.surface_type = tables
            .materials
            .get(&surf.material)
            .and_then(|m| tables.surface_props.get(&m.surface_prop))
            .map_or(0, |&id| id.min(u8::MAX as u32) as u8);
    }
    out
}

/// The asset rows the document actually references, resolved through the
/// host's tables.
pub(crate) fn asset_tables(assets: &dyn AssetInfo, doc: &MapDoc) -> AssetTables {
    let mut out = AssetTables::default();
    let mut materials: Vec<String> = Vec::new();
    let mut models: Vec<String> = Vec::new();
    for o in &doc.objects {
        match &o.kind {
            ObjectKind::Brush(b) => {
                for f in &b.faces {
                    if !materials.contains(&f.material) {
                        materials.push(f.material.clone());
                    }
                }
            }
            ObjectKind::Mesh(m) => {
                for (_, path) in &m.material_map {
                    if !materials.contains(path) {
                        materials.push(path.clone());
                    }
                }
            }
            ObjectKind::Terrain(t) => {
                for l in &t.layers {
                    if !materials.contains(&l.material) {
                        materials.push(l.material.clone());
                    }
                }
            }
            ObjectKind::VoxelTerrain(v) => {
                if !materials.contains(&v.material) {
                    materials.push(v.material.clone());
                }
            }
            ObjectKind::Prop(p) => {
                if !models.contains(&p.model) {
                    models.push(p.model.clone());
                }
            }
            _ => {}
        }
    }
    if materials.is_empty() {
        materials.push(rdoc::model::DEFAULT_MATERIAL.to_string());
    }
    if let Some(vista) = doc.meta.sky.as_ref().filter(|s| !s.trim().is_empty() && !s.trim().eq_ignore_ascii_case("none")) {
        if !models.contains(vista) {
            models.push(vista.clone());
        }
        materials.push(meshes::SKY_MATERIAL.to_string());
    }
    if let Some(sky) = doc.meta.source_sky.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        materials.push(meshes::SKY_MATERIAL.to_string());
        materials.extend(sky::face_materials(sky));
    }
    for path in materials {
        if let Some(info) = assets.material(&path) {
            if let Some(id) = assets.surface_prop(&info.surface_prop) {
                out.surface_props.insert(info.surface_prop.clone(), id);
            }
            out.materials.insert(path, info);
        }
    }
    for path in models {
        if let Some(info) = assets.model(&path) {
            out.models.insert(path, info);
        }
    }
    out
}

/// Meshes, terrain and voxel terrain: everything `csg` does not build itself.
pub(crate) struct Extra {
    pub(crate) surfaces: Vec<RenderSurface>,
    pub(crate) solids: Vec<CollisionSolid>,
}

pub(crate) fn build_extra_geometry(
    doc: &MapDoc,
    fs: &dyn ProjectFs,
    worlds: &WorldMap,
    _tables: &AssetTables,
    stats: &mut Stats,
) -> Result<Extra, Error> {
    let mut out = Extra { surfaces: Vec::new(), solids: Vec::new() };
    let chart = csg::chart_default(doc);
    let mut stack_cache = rdoc::editmesh::StackCache::new();
    for o in &doc.objects {
        let world = worlds.get(&o.id).copied().unwrap_or(glam::DMat4::IDENTITY);
        match &o.kind {
            ObjectKind::Mesh(mesh) => {
                if mesh.role == rdoc::model::MeshRole::Cutter {
                    continue;
                }
                let ctx = modifier_ctx(doc, worlds, o.id, fs)?;
                let (s, c) = mesh_object_stacked(o, mesh, world, fs, chart, &ctx, &mut stack_cache)?;
                out.surfaces.extend(s);
                out.solids.extend(c);
            }
            ObjectKind::Terrain(t) => {
                let (s, c) = terrain_object(o, t, world, fs, stats)?;
                out.surfaces.extend(s);
                out.solids.extend(c);
            }
            ObjectKind::VoxelTerrain(v) => {
                let (s, c) = voxel_object(o, v, world, chart)?;
                out.surfaces.extend(s);
                out.solids.extend(c);
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Object world matrices plus world-space path points, so the modifier stack
/// reads operands in the mesh's own object space.
fn modifier_ctx(
    doc: &MapDoc,
    worlds: &WorldMap,
    self_id: Id,
    fs: &dyn ProjectFs,
) -> Result<rdoc::editmesh::EvalContext, Error> {
    let mut ctx = rdoc::editmesh::EvalContext { self_id: Some(self_id), ..Default::default() };
    for (id, m) in worlds {
        ctx.worlds.insert(*id, *m);
    }
    for o in &doc.objects {
        if let ObjectKind::Path(p) = &o.kind {
            if let Some(w) = worlds.get(&o.id) {
                ctx.paths.insert(o.id, p.points.iter().map(|pt| w.transform_point3(*pt)).collect());
            }
        }
    }
    if let Some(o) = doc.objects.iter().find(|o| o.id == self_id) {
        if let ObjectKind::Mesh(m) = &o.kind {
            for operand in m.modifiers.iter().filter_map(|m| match &m.kind {
                rdoc::editmesh::ModifierKind::Boolean(p) => Some(p.object),
                _ => None,
            }) {
                let mesh = boolean_operand(doc, fs, operand)
                    .map_err(|message| Error::Mesh { object: self_id, message })?;
                ctx.booleans.insert(operand, mesh);
            }
        }
    }
    Ok(ctx)
}

/// A boolean operand mesh in its own object space: an editable mesh as-is, a
/// glb welded by value, a brush triangulated first.
/// A missing or unreadable operand is an error: skipping it would compile the
/// mesh uncut without a word.
fn boolean_operand(doc: &MapDoc, fs: &dyn ProjectFs, id: Id) -> Result<rdoc::editmesh::EditMesh, String> {
    let o = doc.objects.iter().find(|o| o.id == id).ok_or_else(|| format!("boolean operand {id} is not in the map"))?;
    let fail = |e: &dyn std::fmt::Display| format!("boolean operand {id}: {e}");
    match &o.kind {
        ObjectKind::Mesh(m) => {
            let bytes = fs.read(&m.source).map_err(|e| fail(&e))?;
            match rdoc::editmesh::load_triangles(&bytes, &m.source).map_err(|e| fail(&e))? {
                rdoc::editmesh::LoadedMesh::Editable(base) => Ok(base),
                rdoc::editmesh::LoadedMesh::Soup(prims) => {
                    rdoc::editmesh::convert::weld_primitives(&prims).map_err(|e| fail(&e))
                }
            }
        }
        ObjectKind::Brush(b) => rdoc::editmesh::ops::boolean::mesh_from_brush(b).map_err(|e| fail(&e)),
        _ => Err(format!("boolean operand {id} is neither a mesh nor a brush")),
    }
}

fn mesh_object_stacked(
    o: &Object,
    mesh: &rdoc::model::Mesh,
    world: glam::DMat4,
    fs: &dyn ProjectFs,
    chart_default: f64,
    ctx: &rdoc::editmesh::EvalContext,
    stack_cache: &mut rdoc::editmesh::StackCache,
) -> Result<(Vec<RenderSurface>, Vec<CollisionSolid>), Error> {
    let bytes = fs.read(&mesh.source).map_err(|e| Error::Project(format!("{}: {e}", mesh.source)))?;
    // Every mesh source loads through the one branch point: the `.r5mesh`
    // base evaluates here through its modifier stack, glb prims arrive with
    // node transforms baked, exactly as glTF would arrive.
    let staged = rdoc::editmesh::load_triangles(&bytes, &mesh.source)
        .map_err(|e| Error::Mesh { object: o.id, message: e.to_string() })?;
    let prims = match staged {
        rdoc::editmesh::LoadedMesh::Editable(base) => {
            let stacked = stack_cache
                .evaluate(&base, &mesh.modifiers, ctx, false)
                .map_err(|e| Error::Mesh { object: o.id, message: e.to_string() })?;
            rdoc::editmesh::convert::evaluated_primitives(&stacked)
        }
        rdoc::editmesh::LoadedMesh::Soup(prims) => {
            if mesh.modifiers.iter().all(|m| !m.enabled) {
                prims
            } else {
                let base = rdoc::editmesh::convert::weld_primitives(&prims)
                    .map_err(|e| Error::Mesh { object: o.id, message: e.to_string() })?;
                let stacked = stack_cache
                    .evaluate(&base, &mesh.modifiers, ctx, false)
                    .map_err(|e| Error::Mesh { object: o.id, message: e.to_string() })?;
                rdoc::editmesh::convert::evaluated_primitives(&stacked)
            }
        }
    };
    let nodes = prims
            .into_iter()
            .map(|p| csg::glb::GlbNode {
                name: p.name.clone(),
                transform: glam::DMat4::IDENTITY,
                primitives: vec![csg::glb::GlbPrimitive {
                    material: p.material.clone(),
                    positions: p.positions.clone(),
                    normals: if p.normals.len() == p.positions.len() {
                        p.normals.clone()
                    } else {
                        vec![[0.0, 0.0, 1.0]; p.positions.len()]
                    },
                    uv0: if p.uvs.len() == p.positions.len() { p.uvs.clone() } else { vec![[0.0, 0.0]; p.positions.len()] },
                    colors: if p.colors.len() == p.positions.len() {
                        p.colors.clone()
                    } else {
                        vec![[255, 255, 255, 255]; p.positions.len()]
                    },
                    indices: p.indices.clone(),
                }],
            })
            .collect::<Vec<_>>();
    let mut surfaces = Vec::new();
    let mut solids = Vec::new();
    let step = if mesh.lightmap_scale > 0.0 { mesh.lightmap_scale } else { chart_default };
    for node in &nodes {
        let node_world = world * node.transform;
        for prim in &node.primitives {
            if prim.indices.len() % 3 != 0 {
                return Err(Error::Mesh { object: o.id, message: "index run is not a multiple of three".into() });
            }
            let material = mesh
                .material_map
                .get(&prim.material)
                .cloned()
                .unwrap_or_else(|| rdoc::model::DEFAULT_MATERIAL.to_string());
            let collision_surface = vec![rir::CollisionSurface { material: material.clone(), surface_type: 0 }];
            let positions: Vec<Vec3> = prim
                .positions
                .iter()
                .map(|p| node_world.transform_point3(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).as_vec3())
                .collect();
            let mut normals: Vec<Vec3> = prim
                .normals
                .iter()
                .map(|n| {
                    let v = node_world.transform_vector3(DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64));
                    v.normalize_or(DVec3::Z).as_vec3()
                })
                .collect();
            if normals.len() != positions.len() {
                normals = vec![Vec3::Z; positions.len()];
            }
            let mut uv0: Vec<Vec2> = prim.uv0.iter().map(|u| Vec2::from_array(*u)).collect();
            if uv0.len() != positions.len() {
                uv0 = vec![Vec2::ZERO; positions.len()];
            }
            let tangents = csg::uv_tangents(&positions, &normals, &uv0, &prim.indices);
            let mut surface = RenderSurface {
                object: Some(o.id),
                model: 0,
                material,
                positions: positions.clone(),
                normals,
                tangents,
                uv0,
                uv1: vec![Vec2::ZERO; positions.len()],
                colors: prim.colors.clone(),
                // glTF fronts counter-clockwise triangles; the engine fronts
                // clockwise ones (see rbake::light::front_normal).
                indices: prim.indices.chunks_exact(3).flat_map(|t| [t[0], t[2], t[1]]).collect(),
                cast_shadows: mesh.cast_shadows,
                lightmap_texel: step as f32,
                lightmap_size: [0, 0],
            };
            for i in &surface.indices {
                if *i as usize >= surface.positions.len() {
                    return Err(Error::Mesh {
                        object: o.id,
                        message: format!("index {i} addresses no vertex"),
                    });
                }
            }
            // Collision that draws nothing (a clip, a skybox wall) has no
            // render surface.
            if mesh.render {
                csg::chart_mesh_surface(&mut surface, step)?;
                surfaces.push(surface);
            }
            match mesh.collision {
                CollisionMode::None => {}
                CollisionMode::Triangles => {
                    let mut indices = prim.indices.clone();
                    indices.truncate(indices.len() - indices.len() % 3);
                    solids.push(CollisionSolid {
                        object: Some(o.id),
                        model: 0,
                        shape: CollisionShape::Triangles { vertices: positions, indices },
                        contents: mesh.contents,
                        surface_prop: 0,
                        surfaces: collision_surface.clone(),
                    });
                }
                CollisionMode::ConvexHulls => {
                    // One hull per loose part, so an array of separate pieces
                    // collides as its pieces, not as one box around them all.
                    let world_pts: Vec<DVec3> = prim
                        .positions
                        .iter()
                        .map(|p| node_world.transform_point3(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)))
                        .collect();
                    let mut indices = prim.indices.clone();
                    indices.truncate(indices.len() - indices.len() % 3);
                    for part in rdoc::editmesh::modifiers::soup_parts_by_position(&prim.positions, &indices) {
                        let mut seen: BTreeSet<[u64; 3]> = BTreeSet::new();
                        let mut points: Vec<DVec3> = Vec::new();
                        for t in part {
                            for k in 0..3 {
                                let p = world_pts[indices[t as usize * 3 + k] as usize];
                                if seen.insert([p.x.to_bits(), p.y.to_bits(), p.z.to_bits()]) {
                                    points.push(p);
                                }
                            }
                        }
                        if points.len() < 4 {
                            continue;
                        }
                        if let Ok(hull) = collision::convex_hull(&points, 1e-4) {
                            let pts: Vec<Vec3> = hull.verts.iter().map(|i| points[*i].as_vec3()).collect();
                            if pts.len() >= 4 {
                                solids.push(CollisionSolid {
                                    object: Some(o.id),
                                    model: 0,
                                    shape: CollisionShape::ConvexHull { points: pts },
                                    contents: mesh.contents,
                                    surface_prop: 0,
                                    surfaces: collision_surface.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    Ok((surfaces, solids))
}

fn read_heightfield(t: &rdoc::model::Terrain, fs: &dyn ProjectFs) -> Result<rterrain::heightfield::Heightfield, Error> {
    let want = (t.resolution[0] as usize) * (t.resolution[1] as usize) * 4;
    let raw = fs.read(&t.heights).map_err(|e| Error::Project(format!("{}: {e}", t.heights)))?;
    if raw.len() != want {
        return Err(Error::Terrain(format!("{} holds {} bytes, expected {want}", t.heights, raw.len())));
    }
    rterrain::heightfield::Heightfield::from_le_bytes(&raw, t.resolution[0], t.resolution[1], t.cell_size as f32)
        .map_err(|e| Error::Terrain(e.to_string()))
}

fn terrain_object(
    o: &Object,
    t: &rdoc::model::Terrain,
    world: glam::DMat4,
    fs: &dyn ProjectFs,
    stats: &mut Stats,
) -> Result<(Vec<RenderSurface>, Vec<CollisionSolid>), Error> {
    let hf = read_heightfield(t, fs)?;
    let read = |p: &str| -> Option<Vec<u8>> { fs.read(p).ok() };
    let splat = csg::terrain::splat_maps(t, &read).ok();
    let mesh = csg::terrain::mesh(t, &hf, splat.as_ref()).map_err(Error::Geometry)?;
    let material = t
        .layers
        .first()
        .map(|l| l.material.clone())
        .unwrap_or_else(|| rdoc::model::DEFAULT_MATERIAL.to_string());
    let mut surfaces = csg::terrain::terrain_surfaces(&mesh, &material).map_err(Error::Geometry)?;
    for s in &mut surfaces {
        s.object = Some(o.id);
    }
    let mut solids = Vec::new();
    if t.collision {
        let mut solid = csg::terrain::terrain_collision(&mesh, world, t.contents, 0);
        solid.object = Some(o.id);
        solids.push(solid);
    }
    stats.terrains += 1;
    Ok((surfaces, solids))
}

fn voxel_object(
    o: &Object,
    v: &rdoc::model::VoxelTerrain,
    world: glam::DMat4,
    chart_default: f64,
) -> Result<(Vec<RenderSurface>, Vec<CollisionSolid>), Error> {
    let ops: Vec<rterrain::sdf::SdfOp> = serde_json::from_value(v.ops.clone())
        .map_err(|e| Error::Terrain(format!("voxel terrain {}: {e}", o.name)))?;
    // The field spans the object's bounds; `chunks` is its resolution in voxels.
    let voxel_size = v.voxel_size.abs().max(1.0);
    let terrain = rterrain::sdf::VolumeTerrain {
        chunks: rterrain::sdf::VolumeTerrain::resolution_for(v.bounds[0], v.bounds[1], voxel_size),
        voxel_size,
        origin: v.bounds[0],
        ops,
    };
    let mesh = rterrain::mesher::mesh(&terrain).map_err(|e| Error::Terrain(e.to_string()))?;
    let mut indices: Vec<u32> = Vec::new();
    for c in &mesh.chunks {
        indices.extend(c.indices.iter().copied());
    }
    indices.truncate(indices.len() - indices.len() % 3);
    let positions: Vec<Vec3> = mesh
        .vertices
        .iter()
        .map(|x| world.transform_point3(x.position).as_vec3())
        .collect();
    let normals: Vec<Vec3> = mesh
        .vertices
        .iter()
        .map(|x| {
            let n = world.transform_vector3(x.normal);
            n.normalize_or(DVec3::Z).as_vec3()
        })
        .collect();
    let scale = if v.uv_scale.abs() > 0.0 { v.uv_scale as f32 } else { 128.0 };
    let mut surface = RenderSurface {
        object: Some(o.id),
        model: 0,
        material: v.material.clone(),
        positions: positions.clone(),
        normals,
        tangents: vec![Vec4::W; positions.len()],
        uv0: positions.iter().map(|p| Vec2::new(p.x / scale, p.y / scale)).collect(),
        uv1: vec![Vec2::ZERO; positions.len()],
        colors: vec![[255, 255, 255, 255]; positions.len()],
        indices: indices.clone(),
        cast_shadows: true,
        lightmap_texel: if v.lightmap_scale > 0.0 { v.lightmap_scale as f32 } else { chart_default as f32 },
        lightmap_size: [0, 0],
    };
    surface.tangents = csg::uv_tangents(&surface.positions, &surface.normals, &surface.uv0, &surface.indices);
    let texel = surface.lightmap_texel as f64;
    csg::chart_mesh_surface(&mut surface, texel)?;
    let mut solids = Vec::new();
    if v.collision {
        solids.push(CollisionSolid {
            object: Some(o.id),
            model: 0,
            shape: CollisionShape::Triangles { vertices: positions, indices },
            contents: v.contents,
            surface_prop: 0,
            surfaces: vec![rir::CollisionSurface { material: v.material.clone(), surface_type: 0 }],
        });
    }
    Ok((vec![surface], solids))
}

impl props::PropSource {
    /// The baker's view of one prop.
    pub fn instance(&self) -> rir::StaticPropInstance {
        rir::StaticPropInstance {
            model: self.model.clone(),
            origin: self.origin,
            angles: self.angles,
            scale: if self.scale > 0.0 { self.scale } else { 1.0 },
            skin: self.skin,
            solid: self.solid,
            cast_shadows: self.cast_shadows,
            fade_distance: self.fade_distance,
            bounds_min: self.bounds_min,
            bounds_max: self.bounds_max,
        }
    }
}

/// Every static prop the document places, scatter included, in a stable order.
fn collect_props(
    doc: &MapDoc,
    worlds: &WorldMap,
    fs: &dyn ProjectFs,
    tables: &AssetTables,
    stats: &mut Stats,
) -> Result<Vec<props::PropSource>, Error> {
    let mut out: Vec<props::PropSource> = Vec::new();
    for o in &doc.objects {
        let ObjectKind::Prop(p) = &o.kind else { continue };
        if !p.static_prop {
            continue;
        }
        let world = worlds.get(&o.id).copied().unwrap_or(glam::DMat4::IDENTITY);
        let (s, _, _) = world.to_scale_rotation_translation();
        let model = tables.models.get(&p.model);
        out.push(props::PropSource {
            model: p.model.clone(),
            origin: world.w_axis.truncate().as_vec3(),
            angles: props::recorded_angles(&p.keyvalues, &o.transform)
                .unwrap_or_else(|| o.transform.to_source_angles().as_vec3()),
            scale: if s.x > 0.0 { s.x as f32 } else { 1.0 },
            skin: p.skin,
            solid: p.solid,
            cast_shadows: p.cast_shadows,
            fade_distance: p.fade_distance,
            bounds_min: model.map(|m| m.bounds_min).unwrap_or(Vec3::ZERO),
            bounds_max: model.map(|m| m.bounds_max).unwrap_or(Vec3::ZERO),
            sky: false,
            keys: p.keyvalues.clone(),
        });
    }
    for o in &doc.objects {
        let ObjectKind::Terrain(t) = &o.kind else { continue };
        if !t.layers.iter().any(|l| l.scatter.iter().any(|r| r.static_prop)) {
            continue;
        }
        let Ok(hf) = read_heightfield(t, fs) else { continue };
        let read = |p: &str| -> Option<Vec<u8>> { fs.read(p).ok() };
        let splat = csg::terrain::splat_maps(t, &read).ok();
        for (i, layer) in t.layers.iter().enumerate() {
            for rule in &layer.scatter {
                if !rule.static_prop {
                    continue;
                }
                let filters = rterrain::splat::ScatterFilters {
                    min_weight: rule.min_weight,
                    slope_deg: rule.slope_deg,
                    height: rule.height,
                    scale: rule.scale,
                    align_to_normal: rule.align_to_normal,
                    density: rule.density,
                    seed: rule.seed,
                    static_prop: rule.static_prop,
                };
                let transform =
                    rterrain::splat::ObjectTransform { origin: o.transform.position, yaw_deg: 0.0 };
                let Ok(placements) =
                    rterrain::splat::scatter(&hf, splat.as_ref(), i, &rule.models, &filters, &transform)
                else {
                    continue;
                };
                for p in placements {
                    let (yaw, _, _) = p.rotation.to_euler(glam::EulerRot::ZYX);
                    let model = tables.models.get(&p.model);
                    out.push(props::PropSource {
                        model: p.model,
                        origin: p.position.as_vec3(),
                        angles: Vec3::new(0.0, yaw.to_degrees() as f32, 0.0),
                        scale: p.scale,
                        skin: 0,
                        solid: true,
                        cast_shadows: true,
                        fade_distance: 0.0,
                        bounds_min: model.map(|m| m.bounds_min).unwrap_or(Vec3::ZERO),
                        bounds_max: model.map(|m| m.bounds_max).unwrap_or(Vec3::ZERO),
                        sky: false,
                        keys: Default::default(),
                    });
                }
            }
        }
    }
    stats.props = out.len();
    Ok(out)
}

/// The Source skybox cube at the vista, if the document names one and its six
/// face materials are in the asset table. A sky without materials is left out
/// with a warning, and the map compiles on.
fn source_sky(
    doc: &MapDoc,
    sky: &Option<sky::Sky>,
    surfaces: &[RenderSurface],
    props: &[props::PropSource],
    tables: &AssetTables,
    report: &mut Report,
) -> Option<sky::SourceSky> {
    let name = doc.meta.source_sky.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    let origin = match sky {
        Some(s) => s.vista.origin.as_dvec3(),
        None => {
            let (lo, hi) = sky::world_bounds(surfaces, props);
            sky::vista_origin((lo + hi) * 0.5, lo, 0.0)
        }
    };
    match sky::build_source(name, origin, tables) {
        Some(s) => Some(s),
        None => {
            report.warn(format!("the source sky {name:?} is missing its face materials, so it is left out"));
            None
        }
    }
}

/// Where the skybox cell's plane stands: an x between the world's objects and
/// the sky (the vista and its camera), which sky::build places apart on -x.
/// None when the two overlap, and the vista then stays in the world's cell.
fn sky_split(world: &[vis::VisObject], sky: &[(Vec3, Vec3)]) -> Option<f32> {
    let sky_max = sky.iter().filter(|(lo, hi)| lo.x <= hi.x).map(|(_, hi)| hi.x).reduce(f32::max)?;
    let world_min = world.iter().map(|o| o.mins.x).reduce(f32::min)?;
    (sky_max < world_min).then_some((sky_max + world_min) * 0.5)
}

/// Appends a plane to 0x01 and returns its index.
fn append_plane(bsp: &mut rbsp::Bsp, target: Version, normal: [f32; 3], dist: f32) -> u32 {
    let mut planes = match rbsp::lumps::parse(rbsp::ids::PLANES, target, bsp.lump(rbsp::ids::PLANES)) {
        Ok(rbsp::lumps::Lump::Plane(p)) => p,
        _ => Vec::new(),
    };
    planes.push(rbsp::lumps::render::Plane { normal, dist });
    bsp.set_lump(rbsp::ids::PLANES, rbsp::lumps::render::write_planes(&planes, target));
    (planes.len() - 1) as u32
}

/// The render surfaces with their lightmap coordinates moved onto the page the
/// baker placed their chart on, and each surface's page (-1 with no chart).
fn lightmap_surfaces(surfaces: &[RenderSurface], layout: &rbake::ChartLayout) -> (Vec<RenderSurface>, Vec<i16>) {
    let mut out = surfaces.to_vec();
    let mut pages = vec![-1i16; surfaces.len()];
    for c in &layout.charts {
        let Some(s) = out.get_mut(c.surface) else { continue };
        s.uv1 = s.positions.iter().zip(&s.uv1).map(|(p, u)| c.vertex_uv(*p, *u, layout.page_size)).collect();
        pages[c.surface] = c.page.min(i16::MAX as u32) as i16;
    }
    (out, pages)
}

fn bake_scene(doc: &MapDoc, surfaces: &[RenderSurface], props: &[props::PropSource]) -> BakeScene {
    let b = &doc.settings.bake;
    let mut scene = BakeScene {
        settings: BakeSettings {
            texel_size: b.texel_size as f32,
            bounces: b.bounces,
            samples: b.samples,
            sky_color: Vec3::from_array(b.sky_color),
            sky_intensity: b.sky_intensity,
            ambient_occlusion: b.ambient_occlusion,
            probe_spacing: b.probe_spacing as f32,
            cubemap_spacing: b.cubemap_spacing as f32,
        },
        cubemap_points: b.cubemap_points.iter().map(|p| DVec3::from_array(*p).as_vec3()).collect(),
        ..BakeScene::default()
    };
    // A sky face is neither lit nor in the way of the sky, so the bake sees
    // an empty surface in its place and the indices still line up. The skybox
    // cube hangs far off the world and draws unlit, so it gets the same.
    scene.surfaces = surfaces
        .iter()
        .map(|s| {
            if meshes::is_sky_material(&s.material) || sky::is_source_sky_material(&s.material) {
                RenderSurface { object: s.object, model: s.model, material: s.material.clone(), ..RenderSurface::default() }
            } else {
                s.clone()
            }
        })
        .collect();
    // A surface that casts no shadow keeps its lightmap but stays out of the
    // light's way and bounces nothing: decals, glass, light cards.
    scene.see_through =
        scene.surfaces.iter().enumerate().filter(|(_, s)| !s.cast_shadows).map(|(i, _)| i as u32).collect();
    scene.shadow_transmission = scene
        .surfaces
        .iter()
        .enumerate()
        .filter_map(|(i, s)| b.shadow_transmission.get(&s.material).map(|t| (i as u32, t.clamp(0.0, 1.0))))
        .filter(|(_, t)| *t > 0.0)
        .collect();
    for (i, p) in props.iter().enumerate().filter(|(_, p)| !p.sky) {
        scene.props.push(p.instance());
        scene.prop_lump_index.push(i as u32);
    }
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for s in &scene.surfaces {
        for p in &s.positions {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
    }
    if lo.x > hi.x {
        lo = Vec3::ZERO;
        hi = Vec3::ZERO;
    }
    scene.world_mins = lo;
    scene.world_maxs = hi;
    for o in &doc.objects {
        let ObjectKind::Light(l) = &o.kind else { continue };
        let world = local_matrix(&o.transform);
        let position = world.w_axis.truncate().as_vec3();
        // Lights shine along their local -Z, so an unrotated sun points down.
        let forward = world.transform_vector3(DVec3::NEG_Z).normalize_or(DVec3::NEG_Z).as_vec3();
        let kind = match l.kind {
            LightKind::Sun => LightType::Sun,
            LightKind::Point => LightType::Point,
            LightKind::Spot => LightType::Spot,
            LightKind::Area => LightType::Area,
            LightKind::Sky => LightType::Sky,
        };
        // A local light's colour and brightness are its `_light` key: gamma
        // colour and a brightness the engine reads as brightness / 255.
        let local = matches!(kind, LightType::Point | LightType::Spot);
        let (color, intensity) = if local {
            (Vec3::from_array(l.color).max(Vec3::ZERO).powf(2.2), l.intensity.max(0.0) / 255.0)
        } else {
            (Vec3::from_array(l.color), l.intensity)
        };
        let tweak_index = (l.keyvalues.get("script_tweakable").map(String::as_str) == Some("1"))
            .then(|| l.keyvalues.get("tweak_light_index").and_then(|v| v.trim().parse().ok()))
            .flatten();
        scene.lights.push(BakeLight {
            object: Some(o.id),
            kind,
            position,
            direction: forward,
            color,
            intensity,
            tweak_index,
            attenuation: None,
            flags: None,
            range: l.range,
            inner_angle: l.inner_angle,
            outer_angle: l.outer_angle,
            size: Vec2::from_array(l.size),
            cast_shadows: l.cast_shadows,
            baked: l.baked,
            realtime: l.realtime,
        });
    }
    scene
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gltf_front_face_stays_the_front_face() {
        struct One(Vec<u8>);
        impl ProjectFs for One {
            fn read(&self, _: &str) -> anyhow::Result<Vec<u8>> {
                Ok(self.0.clone())
            }
        }
        // Counter-clockwise seen from +Z: glTF's front face looks up.
        let glb = rdoc::import::glb::write(&rdoc::import::glb::GlbMesh {
            name: "tri".into(),
            material: "dev".into(),
            positions: vec![[0.0, 0.0, 0.0], [64.0, 0.0, 0.0], [0.0, 64.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: Vec::new(),
            colors: Vec::new(),
            indices: vec![0, 1, 2],
        })
        .unwrap();
        let mesh = rdoc::model::Mesh {
            source: "tri.glb".into(),
            material_map: Default::default(),
            collision: CollisionMode::None,
            contents: 0,
            cast_shadows: true,
            lightmap_scale: 16.0,
            render: true,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        };
        let o = rdoc::doc::new_object("tri", rdoc::model::Id::nil(), rdoc::model::ObjectKind::Mesh(mesh.clone()));
        let ctx = rdoc::editmesh::EvalContext::default();
        let mut cache = rdoc::editmesh::StackCache::new();
        let (surfaces, _) =
            mesh_object_stacked(&o, &mesh, glam::DMat4::IDENTITY, &One(glb), csg::DEFAULT_LIGHTMAP_SCALE, &ctx, &mut cache)
                .unwrap();
        let s = &surfaces[0];
        let p = |i: u32| s.positions[i as usize];
        let n = rbake::light::front_normal(p(s.indices[0]), p(s.indices[1]), p(s.indices[2]));
        assert!(n.z > 0.0, "the engine sees the triangle's back from above: {n}");
    }

    #[test]
    fn a_mesh_collides_with_its_materials_surface() {
        struct One(Vec<u8>);
        impl ProjectFs for One {
            fn read(&self, _: &str) -> anyhow::Result<Vec<u8>> {
                Ok(self.0.clone())
            }
        }
        let glb = rdoc::import::glb::write(&rdoc::import::glb::GlbMesh {
            name: "tri".into(),
            material: "dev".into(),
            positions: vec![[0.0, 0.0, 0.0], [64.0, 0.0, 0.0], [0.0, 64.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: Vec::new(),
            colors: Vec::new(),
            indices: vec![0, 1, 2],
        })
        .unwrap();
        let mut material_map = indexmap::IndexMap::new();
        material_map.insert("dev".to_string(), "world/dirt/path".to_string());
        let mesh = rdoc::model::Mesh {
            source: "tri.glb".into(),
            material_map,
            collision: CollisionMode::Triangles,
            contents: 0,
            cast_shadows: true,
            lightmap_scale: 16.0,
            render: false,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        };
        let mut tables = AssetTables::default();
        tables.materials.insert(
            "world/dirt/path".into(),
            rir::MaterialInfo {
                path: "world/dirt/path".into(),
                guid: 1,
                shader_set: String::new(),
                vertex_format: rir::VertexFormat::LitBump,
                surface_prop: "dirt".into(),
                translucent: false,
                decal: false,
                nodraw: false,
                textures: Default::default(),
            },
        );
        tables.surface_props.insert("dirt".into(), 7);
        let o = rdoc::doc::new_object("tri", rdoc::model::Id::nil(), rdoc::model::ObjectKind::Mesh(mesh.clone()));
        let ctx = rdoc::editmesh::EvalContext::default();
        let mut cache = rdoc::editmesh::StackCache::new();
        let (_, solids) =
            mesh_object_stacked(&o, &mesh, glam::DMat4::IDENTITY, &One(glb), csg::DEFAULT_LIGHTMAP_SCALE, &ctx, &mut cache)
                .unwrap();
        let solids = with_surface_types(&solids, &tables);
        assert_eq!(solids[0].surfaces, vec![rir::CollisionSurface { material: "world/dirt/path".into(), surface_type: 7 }]);
    }

    #[test]
    fn a_voxel_terrain_meshes_across_its_bounds() {
        let v = rdoc::model::VoxelTerrain {
            bounds: [DVec3::new(-256.0, -256.0, -128.0), DVec3::new(256.0, 256.0, 128.0)],
            voxel_size: 16.0,
            ops: serde_json::json!([
                { "op": "add", "shape": { "shape": "box", "center": [0.0, 0.0, 0.0], "half": [192.0, 192.0, 64.0], "rotation": [0.0, 0.0, 0.0] } }
            ]),
            material: "dev/dev_grey".into(),
            uv_scale: 0.05,
            collision: true,
            contents: 1,
            lightmap_scale: 0.0,
        };
        let o = rdoc::doc::new_object("voxel", rdoc::model::Id::nil(), rdoc::model::ObjectKind::VoxelTerrain(v.clone()));
        let (surfaces, _) = voxel_object(&o, &v, glam::DMat4::IDENTITY, csg::DEFAULT_LIGHTMAP_SCALE).expect("a voxel terrain compiles");
        let s = &surfaces[0];
        assert!(!s.indices.is_empty(), "the box meshed to nothing");
        // The surface reaches the box's sides, well past one 8 voxel chunk.
        let max_x = s.positions.iter().map(|p| p.x).fold(f32::MIN, f32::max);
        assert!(max_x > 180.0, "the mesh stops at x = {max_x}");
    }

    #[test]
    fn a_model_record_is_read_the_way_the_writer_writes_it() {
        let mut bsp = emit::new_map(Version::ClientV51);
        let models = vec![ModelSlot {
            index: 0,
            owner: None,
            mins: Vec3::splat(-16.0),
            maxs: Vec3::splat(16.0),
            has_render: false,
        }];
        collision::emit_collision(&[], &models, &mut bsp).expect("an empty collision writes");
        restore_model_bounds(&mut bsp, &models, Version::ClientV51);
        let parsed = rbsp::lumps::parse(rbsp::ids::MODELS, Version::ClientV51, bsp.lump(rbsp::ids::MODELS))
            .expect("models parse");
        let rbsp::lumps::Lump::Model(list) = parsed else { panic!("models decoded wrong") };
        assert_eq!(list[0].mins, [-16.0; 3]);
        let _ = unused_model_field(&list[0]);
    }
}

#[allow(dead_code)]
fn unused_model_field(m: &Model) -> bool {
    m.first_mesh == 0 && m.num_meshes == 0
}
