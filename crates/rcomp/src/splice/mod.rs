//! Splice: a document that references a shipped map compiles as that map's
//! own bytes with the document's edits applied to them.
//!
//! The reference is never decoded and re-encoded as a whole. Each target starts
//! from the matching half of the shipped pair, and every record the document
//! leaves alone keeps its bytes: an entity block its text, a static prop its
//! 64-byte record, the render and lighting lumps everything. What an edit
//! touches is patched where it lives, without moving any index another lump
//! holds:
//!
//! - an entity block is replaced in place, dropped, or appended;
//! - a static prop's record is rewritten in place; a dropped prop keeps its
//!   slot and loses its visibility references and its collision leaf;
//! - a dropped mesh group loses its visibility references and the collision
//!   leaves that carry its surface.
//!
//! - authored or promoted world geometry is appended to the world model: its
//!   meshes after the shipped ones, its collision as a second subtree under a
//!   new root at node 0, its meshes in the cell tree.
//!
//! A document that places a prefab on top of the reference is not handled
//! here; `compile` falls back to the decode-and-re-emit overlay for it.

mod ents;
mod props;
pub(crate) mod tables;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use rbsp::{Bsp, Version};
use rdoc::model::{Id, MapDoc, Object, ObjectKind, Partition, RefRecord, Reference, ReferenceKind};

use crate::overlay::OverlayError;
use crate::report::{Report, Stats};
use crate::{CompileOutput, Error, MapArtifacts};

pub use tables::{kill_collision_prop, kill_collision_surfaces, material_key, mesh_materials, remove_obj_refs, surfaces_named};

/// One half of a shipped map: the container and the `.ent` files beside it,
/// keyed by partition name.
#[derive(Clone, Debug)]
pub struct Half {
    pub path: PathBuf,
    pub bsp: Bsp,
    pub ents: BTreeMap<String, Vec<u8>>,
}

/// Both halves of a shipped map, as far as the reference root holds them.
#[derive(Clone, Debug)]
pub struct Pair {
    pub stem: String,
    pub client: Half,
    pub dedi: Option<Half>,
    /// Client lumps that were read as zeros and taken from the server half.
    pub repaired: Vec<u16>,
}

/// Where the client and the server half of `source` sit under `root`.
///
/// A project imported from a pair keeps it as `reference/<stem>/{client,dedi}`,
/// and the agent kit keeps it as `<dir>/{client,dedi}`; a map with no server
/// half beside it is found where the overlay looks for one.
pub fn locate_pair(root: &Path, source: &str) -> Option<(PathBuf, Option<PathBuf>)> {
    if source.is_empty() {
        return None;
    }
    let file = format!("{source}.bsp");
    for dir in [root.join(source), root.to_path_buf()] {
        let client = dir.join("client").join(&file);
        if client.is_file() {
            let dedi = dir.join("dedi").join(&file);
            return Some((client, dedi.is_file().then_some(dedi)));
        }
    }
    let client = crate::overlay::locate(root, source)?;
    let dedi = client
        .parent()
        .and_then(Path::parent)
        .map(|d| d.join("dedi").join(&file))
        .filter(|p| p.is_file());
    Some((client, dedi))
}

fn read_half(path: &Path) -> Result<Half, OverlayError> {
    let bsp = Bsp::read_path(path).map_err(|source| OverlayError::Map { path: path.to_path_buf(), source })?;
    let stem = path
        .file_name()
        .map(|n| n.to_string_lossy().trim_end_matches(".bsp").to_string())
        .unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut ents = BTreeMap::new();
    for p in Partition::FILES {
        let file = dir.join(format!("{stem}_{}.ent", p.name()));
        if let Ok(bytes) = std::fs::read(&file) {
            ents.insert(p.name().to_string(), bytes);
        }
    }
    Ok(Half { path: path.to_path_buf(), bsp, ents })
}

/// Reads both halves of a reference.
pub fn load(def: &Reference, root: &Path) -> Result<Pair, OverlayError> {
    let (client_path, dedi_path) =
        locate_pair(root, &def.source).ok_or_else(|| OverlayError::NotFound(def.source.clone()))?;
    let mut client = read_half(&client_path)?;
    let dedi = match dedi_path {
        Some(p) => Some(read_half(&p)?),
        None => None,
    };
    for half in std::iter::once(&client).chain(dedi.iter()) {
        if half.bsp.version_kind().is_none_or(|v| v == Version::Tf2V37) {
            return Err(OverlayError::WrongVersion(def.source.clone()));
        }
    }
    let repaired = match &dedi {
        Some(d) => repair_client(&mut client.bsp, &d.bsp),
        None => Vec::new(),
    };
    let stem = client_path
        .file_name()
        .map(|n| n.to_string_lossy().trim_end_matches(".bsp").to_string())
        .unwrap_or_else(|| def.source.clone());
    Ok(Pair { stem, client, dedi, repaired })
}

/// Fills the client lumps a pak export left as zeros from the server half.
///
/// The export of a client map writes some small lumps as zero bytes of the
/// right length: the level info, the lightmap headers, the entity partition
/// list, the cubemaps, the probe parent info and a few more. The server half of
/// the same map carries them, and every lump the two halves both carry with
/// data is byte-identical, so the server's copy is the client's. Returns the
/// lumps that were filled.
pub fn repair_client(client: &mut Bsp, dedi: &Bsp) -> Vec<u16> {
    let mut out = Vec::new();
    let ids: Vec<u16> = client.lumps.keys().copied().collect();
    for id in ids {
        let own = client.lump(id);
        let theirs = dedi.lump(id);
        if own.is_empty() || own.len() != theirs.len() {
            continue;
        }
        if own.iter().all(|b| *b == 0) && theirs.iter().any(|b| *b != 0) {
            let bytes = theirs.to_vec();
            client.set_lump(id, bytes);
            out.push(id);
        }
    }
    out
}

/// What becomes of one record of the reference.
#[derive(Clone, Copy, Debug)]
pub enum Fate<'a> {
    /// The document left it alone: the shipped bytes stay.
    Keep,
    /// The object is compiled in its place.
    Replace(&'a Object),
    /// The object was deleted, or promoted and hidden.
    Drop,
}

impl Fate<'_> {
    pub fn is_keep(&self) -> bool {
        matches!(self, Fate::Keep)
    }
}

/// Every record the document says something about, by kind.
#[derive(Debug, Default)]
pub struct Plan<'a> {
    pub entities: BTreeMap<u32, Fate<'a>>,
    pub props: BTreeMap<u32, Fate<'a>>,
    pub meshes: BTreeMap<(u32, String), Fate<'a>>,
    /// Collision groups no render mesh stands for: model, mask, surface.
    pub collision: BTreeMap<(u32, u32, String), Fate<'a>>,
    /// Compiled objects that stand for no record: the document's own content.
    pub authored: Vec<&'a Object>,
    /// Object ids that replace a record, so they are not compiled twice.
    pub replacing: BTreeSet<Id>,
}

impl Plan<'_> {
    /// True when nothing changes: every record kept and nothing authored.
    pub fn is_untouched(&self) -> bool {
        self.authored.is_empty()
            && self.entities.values().all(Fate::is_keep)
            && self.props.values().all(Fate::is_keep)
            && self.meshes.values().all(Fate::is_keep)
            && self.collision.values().all(Fate::is_keep)
    }

    /// Collision groups that no longer come from the reference.
    pub fn dropped_collision(&self) -> Vec<&(u32, u32, String)> {
        self.collision.iter().filter(|(_, f)| !f.is_keep()).map(|(k, _)| k).collect()
    }

    /// Mesh groups that no longer draw from the reference.
    pub fn dropped_meshes(&self) -> Vec<&(u32, String)> {
        self.meshes.iter().filter(|(_, f)| !f.is_keep()).map(|(k, _)| k).collect()
    }
}

/// The reference object of a document, when it is one this module splices.
pub fn reference_of(doc: &MapDoc) -> Option<(&Object, &Reference)> {
    let visible: Vec<Id> = doc.layers.iter().filter(|l| l.visible).map(|l| l.id).collect();
    doc.objects.iter().find_map(|o| match &o.kind {
        ObjectKind::Reference(r)
            if r.kind == ReferenceKind::ApexMap && !o.hidden && visible.contains(&o.layer) =>
        {
            Some((o, r))
        }
        _ => None,
    })
}

/// True for an object the splice compiles as authored world geometry, which
/// only the overlay can place today.
pub fn is_world_geometry(o: &Object, doc: &MapDoc) -> bool {
    match &o.kind {
        ObjectKind::Mesh(_) | ObjectKind::Terrain(_) | ObjectKind::VoxelTerrain(_) => true,
        // A brush under an entity is that entity's collision, compiled with it.
        ObjectKind::Brush(_) => !o.parent.is_some_and(|p| {
            doc.objects.iter().any(|q| q.id == p && matches!(q.kind, ObjectKind::Entity(_) | ObjectKind::Volume(_)))
        }),
        _ => false,
    }
}

/// Sorts every object of the document against the reference's records.
pub fn plan<'a>(doc: &'a MapDoc, reference: &'a Reference) -> Plan<'a> {
    let visible_layers: Vec<Id> = doc.layers.iter().filter(|l| l.visible).map(|l| l.id).collect();
    let by_id: HashMap<Id, &Object> = doc.objects.iter().map(|o| (o.id, o)).collect();
    let compiled = |o: &Object| !o.hidden && visible_layers.contains(&o.layer);
    let mut out = Plan::default();
    for (id, record) in &reference.sources {
        let fate = match by_id.get(id) {
            None => Fate::Drop,
            Some(o) if compiled(o) => {
                out.replacing.insert(*id);
                Fate::Replace(o)
            }
            Some(_) if reference.is_promoted(id) => Fate::Drop,
            Some(_) => Fate::Keep,
        };
        match record {
            RefRecord::Entity { index } => {
                out.entities.insert(*index, fate);
            }
            RefRecord::Prop { index } => {
                out.props.insert(*index, fate);
            }
            RefRecord::Mesh { model, material } => {
                out.meshes.insert((*model, material.clone()), fate);
            }
            RefRecord::Collision { model, contents, surface } => {
                out.collision.insert((*model, *contents, surface.clone()), fate);
            }
        }
    }
    for index in &reference.removed_entities {
        out.entities.insert(*index, Fate::Drop);
    }
    for o in &doc.objects {
        if matches!(o.kind, ObjectKind::Reference(_) | ObjectKind::Group) || !compiled(o) {
            continue;
        }
        if out.replacing.contains(&o.id) {
            continue;
        }
        out.authored.push(o);
    }
    out
}

/// True when the document can be compiled by splicing: it references a shipped
/// map and places no prefab on top of it.
pub fn applies(doc: &MapDoc) -> bool {
    let Some((_, reference)) = reference_of(doc) else { return false };
    let plan = plan(doc, reference);
    !plan.authored.iter().any(|o| matches!(o.kind, ObjectKind::PrefabInstance { .. }))
}

/// The world geometry the document authors or promotes, compiled the way a map
/// of its own would compile it: render surfaces and collision solids of model 0.
pub struct WorldGeometry {
    pub surfaces: Vec<rir::RenderSurface>,
    pub solids: Vec<rir::CollisionSolid>,
}

impl WorldGeometry {
    /// The box the added geometry spans, when there is any.
    pub fn bounds(&self) -> Option<(glam::Vec3, glam::Vec3)> {
        let points = self.surfaces.iter().flat_map(|s| s.positions.iter().copied());
        let (lo, hi) = points.fold((glam::Vec3::splat(f32::INFINITY), glam::Vec3::splat(f32::NEG_INFINITY)), |(a, b), p| (a.min(p), b.max(p)));
        (lo.x <= hi.x).then_some((lo, hi))
    }
}

fn world_geometry(doc: &MapDoc, plan: &Plan<'_>, fs: &dyn crate::ProjectFs, tables: &rir::AssetTables) -> Result<WorldGeometry, Error> {
    let promoted = plan.meshes.values().chain(plan.collision.values()).filter_map(|f| match f {
        Fate::Replace(o) => Some(*o),
        _ => None,
    });
    let objects: Vec<Object> =
        plan.authored.iter().copied().chain(promoted).filter(|o| is_world_geometry(o, doc)).cloned().collect();
    if objects.is_empty() {
        return Ok(WorldGeometry { surfaces: Vec::new(), solids: Vec::new() });
    }
    let sub = MapDoc {
        format: doc.format,
        meta: doc.meta.clone(),
        layers: doc.layers.clone(),
        objects,
        settings: doc.settings.clone(),
    };
    let worlds = crate::world_transforms(doc);
    let geo = crate::csg::build_geometry(&sub, &worlds)?;
    let mut stats = Stats::default();
    let extra = crate::build_extra_geometry(&sub, fs, &worlds, tables, &mut stats)?;
    let surfaces = geo.surfaces.into_iter().chain(extra.surfaces).filter(|s| s.model == 0).collect();
    let solids = geo.solids.into_iter().chain(extra.solids).filter(|s| s.model == 0).collect();
    Ok(WorldGeometry { surfaces, solids })
}

/// The document's own objects, the ones no reference record stands for: what
/// the entity schema checks. A promoted object is the shipped map's and is not
/// held to the schema.
pub fn authored_doc(doc: &MapDoc) -> MapDoc {
    let keep: BTreeSet<Id> = match reference_of(doc) {
        Some((_, r)) => plan(doc, r).authored.iter().map(|o| o.id).collect(),
        None => BTreeSet::new(),
    };
    MapDoc {
        format: doc.format,
        meta: doc.meta.clone(),
        layers: doc.layers.clone(),
        objects: doc.objects.iter().filter(|o| keep.contains(&o.id)).cloned().collect(),
        settings: doc.settings.clone(),
    }
}

/// Compiles a document by splicing its edits into the reference pair.
pub fn compile(
    doc: &MapDoc,
    fs: &dyn crate::ProjectFs,
    schema: &rdoc::schema::EntitySchema,
    assets: &dyn crate::AssetInfo,
    opts: &crate::CompileOptions,
) -> Result<CompileOutput, Error> {
    let mut report = Report::default();
    let started = crate::report::Clock::now();
    let (_, reference) = reference_of(doc).ok_or_else(|| Error::Project("the document references no map".into()))?;
    let root = crate::reference_root(opts);
    let pair = load(reference, &root)?;
    if !pair.repaired.is_empty() {
        report.warn(format!(
            "the client half of {} carries lumps {} as zeros; they were taken from the server half",
            pair.stem,
            pair.repaired.iter().map(|id| format!("0x{id:02x}")).collect::<Vec<_>>().join(", ")
        ));
    }
    report.record("reference", started);
    let plan = plan(doc, reference);
    let tables = crate::asset_tables(assets, doc);
    let stage = crate::report::Clock::now();
    let geometry = world_geometry(doc, &plan, fs, &tables)?;
    report.record("geometry", stage);

    let mut out = CompileOutput {
        client: None,
        dedi: None,
        nav_soup: crate::nav::NavSoup::default(),
        report: Report::default(),
    };
    let mut stats = Stats::default();
    crate::count_objects(doc, &mut stats);
    if opts.targets.client {
        let stage = crate::report::Clock::now();
        let a = target(&pair.client, Version::ClientV51, doc, schema, &tables, &plan, &geometry, &mut report)?;
        report.record("client", stage);
        out.client = Some(a);
    }
    if opts.targets.dedi {
        let stage = crate::report::Clock::now();
        let half = pair.dedi.as_ref().ok_or_else(|| {
            Error::Project(format!(
                "the reference {} has no dedicated-server half beside its client map; import the map with its server half to build the dedicated target",
                pair.stem
            ))
        })?;
        let a = target(half, Version::DediV47, doc, schema, &tables, &plan, &geometry, &mut report)?;
        report.record("dedi", stage);
        out.dedi = Some(a);
    }
    if let Some(a) = out.client.as_ref().or(out.dedi.as_ref()) {
        crate::emit::collect(&a.bsp, a.bsp.version_kind().unwrap_or(Version::ClientV51), &mut stats);
        stats.entities = a.entity_count();
        stats.props = props::prop_count(&a.bsp) as usize;
    }
    report.stats = stats;
    out.report = report;
    Ok(out)
}

/// One target's map: the shipped half with the plan applied.
#[allow(clippy::too_many_arguments)]
fn target(
    half: &Half,
    version: Version,
    doc: &MapDoc,
    schema: &rdoc::schema::EntitySchema,
    tables: &rir::AssetTables,
    plan: &Plan<'_>,
    geometry: &WorldGeometry,
    report: &mut Report,
) -> Result<MapArtifacts, Error> {
    let mut bsp = half.bsp.clone();
    let mut ent_bytes = half.ents.clone();
    let label = if version == Version::ClientV51 { "client" } else { "dedicated-server" };

    let dropped = plan.dropped_meshes();
    if !dropped.is_empty() {
        tables::drop_meshes(&mut bsp, &dropped, report).map_err(|message| Error::Invalid { target: label, message })?;
    }
    let dropped = plan.dropped_collision();
    if !dropped.is_empty() {
        tables::drop_collision(&mut bsp, &dropped, report).map_err(|message| Error::Invalid { target: label, message })?;
    }
    // Authored and promoted world geometry goes on the end of the world
    // model: render first, since both append to the shared position pool and
    // the collision's run has to start on its own 1024-vertex step.
    if !geometry.surfaces.is_empty() {
        let meshes = crate::meshes::append_render(&mut bsp, &geometry.surfaces, tables)?;
        tables::add_meshes_to_cells(&mut bsp, &meshes).map_err(|message| Error::Invalid { target: label, message })?;
        report.warn(format!(
            "{} mesh(es) appended to the world model; they draw without a lightmap and cast no cascaded shadow",
            meshes.len()
        ));
    }
    if !geometry.solids.is_empty() {
        let a = crate::collision::append_world(&mut bsp, &geometry.solids)?;
        report.warn(format!("{} collision node(s) and {} leaf dwords appended to the world model", a.nodes, a.leaf_words));
    }
    props::splice(&mut bsp, doc, tables, plan, report)?;
    // Entities last: a worldspawn block the compiler writes reads the world
    // box everything above may have grown.
    ents::splice(&mut bsp, &mut ent_bytes, doc, schema, plan, version, geometry.bounds(), report)?;

    let issues = rbsp::validate(&bsp, &ent_bytes);
    let mut validation = Vec::new();
    for i in &issues {
        validation.push(format!(
            "{:?} {}: {}",
            i.severity,
            i.lump.map(|l| format!("0x{l:02x}")).unwrap_or_default(),
            i.message
        ));
    }
    report.validation.extend(validation);
    if let Some(err) = issues.iter().find(|i| i.is_error()) {
        return Err(Error::Invalid { target: label, message: err.message.clone() });
    }
    let ent_files = Partition::FILES
        .into_iter()
        .filter(|p| ent_bytes.contains_key(p.name()))
        .map(|p| (p, p.name().to_string()))
        .collect();
    Ok(MapArtifacts { bsp, ent_files, ent_bytes, cubemap: None })
}

