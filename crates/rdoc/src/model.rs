//! The `.r5map` document. Units are Source inches, Z up, right-handed.

use glam::{DQuat, DVec3};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FORMAT_VERSION: u32 = 1;

/// Material new brushes and meshes get: the 512x512 grey dev texture.
pub const DEFAULT_MATERIAL: &str = "world/dev/dev_gray_512";

/// Texels in one texture tile. With a face scale of 1 a texel is one world
/// unit, so a 512x512 dev texture repeats every 512 units.
pub const UV_TEXELS_PER_TILE: f64 = 512.0;

/// The dev material set, as shipped maps name them.
pub const DEV_MATERIALS: &[&str] = &[
    "world/dev/dev_gray_512",
    "world/dev/dev_white_512",
    "world/dev/dev_white_512_v_color",
    "world/dev/dev_blue_512",
    "world/dev/dev_brown_512",
    "world/dev/dev_green_512",
    "world/dev/dev_ground_512",
    "world/dev/dev_orange_512",
    "world/dev/dev_red_512",
    "world/dev/dev_yellow_512",
    "world/dev/dev_blue_light_01",
    "world/dev/dev_blue_orange_01",
    "world/dev/dev_green_light",
    "world/dev/dev_red_light_01",
    "world/dev/dev_white_light",
    "world/dev/dev_yellow_light_01",
    "world/dev/dev_light_vertex_color",
    "models/world/dev/dev_gray_512",
    "models/world/dev/test_pattern_2x2",
];

pub type Id = Uuid;
pub type KeyValues = IndexMap<String, String>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MapDoc {
    pub format: u32,
    pub meta: MapMeta,
    pub layers: Vec<Layer>,
    pub objects: Vec<Object>,
    pub settings: Settings,
}

impl Default for MapDoc {
    fn default() -> Self {
        let layer = Layer { id: Uuid::new_v4(), name: "default".into(), visible: true, locked: false };
        Self { format: FORMAT_VERSION, meta: MapMeta::default(), layers: vec![layer], objects: Vec::new(), settings: Settings::default() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MapMeta {
    /// Level stem. The launcher only lists `mp_rr_*` and `mp_lobby`.
    pub stem: String,
    pub display_name: String,
    pub description: String,
    /// Playlists the map is registered in.
    pub playlists: Vec<String>,
    /// Loading-screen image (project-relative path), encoded at package time.
    pub loadscreen: Option<String>,
    pub minimap: MinimapSettings,
    /// Skybox model or sky material the map uses.
    pub sky: Option<String>,
    /// Source 2D skybox (`worldspawn skyname`): the compiler draws its six
    /// `world/source/skybox/<name><face>` faces on a cube at the vista instead
    /// of a vista model.
    #[serde(default)]
    pub source_sky: Option<String>,
    /// Extra client/dedi rpaks whose assets the map may reference without packing them.
    pub resident_paks: Vec<String>,
}

impl Default for MapMeta {
    fn default() -> Self {
        Self {
            stem: "mp_rr_untitled".into(),
            display_name: "Untitled".into(),
            description: String::new(),
            playlists: Vec::new(),
            loadscreen: None,
            minimap: MinimapSettings::default(),
            sky: None,
            source_sky: None,
            resident_paks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MinimapSettings {
    pub enabled: bool,
    /// World-space square the minimap covers: min corner XY and edge length.
    pub origin: [f64; 2],
    pub size: f64,
    pub resolution: u32,
}

impl Default for MinimapSettings {
    fn default() -> Self {
        Self { enabled: true, origin: [-8192.0, -8192.0], size: 16384.0, resolution: 2048 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: Id,
    pub name: String,
    pub visible: bool,
    pub locked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub position: DVec3,
    pub rotation: DQuat,
    pub scale: DVec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self { position: DVec3::ZERO, rotation: DQuat::IDENTITY, scale: DVec3::ONE }
    }
}

impl Transform {
    /// Source-style angles (pitch, yaw, roll) in degrees, as written to `angles` keys.
    pub fn to_source_angles(&self) -> DVec3 {
        let (yaw, pitch, roll) = self.rotation.to_euler(glam::EulerRot::ZYX);
        DVec3::new(pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees())
    }

    pub fn from_source_angles(position: DVec3, angles: DVec3) -> Self {
        let r = DQuat::from_euler(glam::EulerRot::ZYX, angles.y.to_radians(), angles.x.to_radians(), angles.z.to_radians());
        Self { position, rotation: r, scale: DVec3::ONE }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Object {
    pub id: Id,
    pub name: String,
    pub layer: Id,
    pub parent: Option<Id>,
    pub transform: Transform,
    pub hidden: bool,
    pub locked: bool,
    pub kind: ObjectKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObjectKind {
    Brush(Brush),
    Mesh(Mesh),
    Prop(Prop),
    Entity(Entity),
    Volume(Volume),
    Light(Light),
    Path(PathObject),
    Terrain(Terrain),
    /// Signed-distance terrain for overhangs, caves and arches.
    VoxelTerrain(VoxelTerrain),
    Reference(Reference),
    /// Pure grouping node; children carry the content.
    Group,
    /// An instance of a prefab stored in `prefabs/<name>.r5prefab`.
    PrefabInstance { prefab: String },
}

/// Convex solid given by bounding planes in object space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Brush {
    pub faces: Vec<BrushFace>,
    /// Collision contents mask written to the BVH (player clip, ...). 0 is
    /// ordinary solid world geometry, which the compiler writes as the mask
    /// shipped maps use for it.
    pub contents: u32,
    /// Material that renders nothing (tool textures) never emits render faces.
    pub detail: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BrushFace {
    /// Plane `normal . p = dist`, normal pointing out of the solid.
    pub normal: DVec3,
    pub dist: f64,
    pub material: String,
    pub uv: UvMapping,
    /// Lightmap texel size in world units; 0 uses the bake default.
    pub lightmap_scale: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct UvMapping {
    pub u_axis: DVec3,
    pub v_axis: DVec3,
    pub offset: [f64; 2],
    pub scale: [f64; 2],
    pub rotation: f64,
}

impl Default for UvMapping {
    fn default() -> Self {
        Self { u_axis: DVec3::X, v_axis: DVec3::NEG_Y, offset: [0.0; 2], scale: [1.0; 2], rotation: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionMode {
    /// Triangle collision from the render mesh.
    Triangles,
    /// Convex hulls of each mesh part.
    ConvexHulls,
    /// Render only.
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mesh {
    /// Project-relative `.glb`, or `.r5mesh` for an editable mesh.
    pub source: String,
    /// glTF material name -> game material path.
    pub material_map: IndexMap<String, String>,
    pub collision: CollisionMode,
    pub contents: u32,
    pub cast_shadows: bool,
    pub lightmap_scale: f64,
    /// False for collision that draws nothing: a clip, a skybox wall.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub render: bool,
    #[serde(default, skip_serializing_if = "MeshRole::is_default")]
    pub role: MeshRole,
    /// Non-destructive modifiers evaluated top to bottom; empty means the base.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<super::editmesh::Modifier>,
}

/// A boolean operand: the compiler skips it, so it never renders, collides
/// or bakes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshRole {
    #[default]
    Normal,
    Cutter,
}

impl MeshRole {
    fn is_default(v: &MeshRole) -> bool {
        *v == MeshRole::Normal
    }
}

fn yes() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prop {
    /// `mdl/....rmdl`.
    pub model: String,
    pub skin: i32,
    pub static_prop: bool,
    pub solid: bool,
    pub cast_shadows: bool,
    pub fade_distance: f32,
    /// Extra entity keys for dynamic props (ignored for static props).
    pub keyvalues: KeyValues,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Partition {
    /// Main ENTITIES lump.
    Main,
    Env,
    Fx,
    Script,
    Snd,
    Spawn,
}

impl Partition {
    pub const FILES: [Partition; 5] = [Partition::Env, Partition::Fx, Partition::Script, Partition::Snd, Partition::Spawn];

    pub fn suffix(self) -> Option<&'static str> {
        match self {
            Partition::Main => None,
            Partition::Env => Some("env"),
            Partition::Fx => Some("fx"),
            Partition::Script => Some("script"),
            Partition::Snd => Some("snd"),
            Partition::Spawn => Some("spawn"),
        }
    }

    pub fn name(self) -> &'static str {
        self.suffix().unwrap_or("main")
    }

    pub const ALL: [Partition; 6] = [
        Partition::Main,
        Partition::Env,
        Partition::Fx,
        Partition::Script,
        Partition::Snd,
        Partition::Spawn,
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// `.ent` sidecar file name for a map stem, or `None` for the main lump.
    pub fn sidecar(self, stem: &str) -> Option<String> {
        self.suffix().map(|s| format!("{stem}_{s}.ent"))
    }
}

/// Any engine entity. `origin`/`angles` come from the object transform and are
/// not stored in `keyvalues`. Brush entities own their child `Brush` objects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub classname: String,
    pub keyvalues: KeyValues,
    /// `None` = the schema's home partition for the class.
    pub partition: Option<Partition>,
    /// Targets and parent links by object id, resolved to targetnames at compile time.
    pub links: IndexMap<String, Id>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    Trigger,
    TriggerHurt,
    TriggerSoundscape,
    EnvmapVolume,
    LightEnvironmentVolume,
    FogVolume,
    PlayerClip,
    OutOfBounds,
    Custom,
}

/// A brush-bounded entity with no render geometry. Compiles to a mesh-less
/// model plus `model "*N"` and a `*coll` key on the entity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Volume {
    pub kind: VolumeKind,
    pub classname: String,
    pub brushes: Vec<Brush>,
    pub keyvalues: KeyValues,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightKind {
    Sun,
    Point,
    Spot,
    Area,
    Sky,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Light {
    pub kind: LightKind,
    /// Linear RGB.
    pub color: [f32; 3],
    pub intensity: f32,
    pub range: f32,
    pub inner_angle: f32,
    pub outer_angle: f32,
    /// Area light extent (width, height) in world units.
    pub size: [f32; 2],
    pub cast_shadows: bool,
    /// Baked into lightmaps/probes; realtime-only lights are skipped by the baker.
    pub baked: bool,
    pub realtime: bool,
    pub keyvalues: KeyValues,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Zipline,
    Ziprail,
    Spline,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathObject {
    pub kind: PathKind,
    /// Object-space control points.
    pub points: Vec<DVec3>,
    pub keyvalues: KeyValues,
}

/// Heightfield terrain. Heights live in a sidecar file so the JSON stays small.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Terrain {
    /// Project-relative little-endian f32 heights, row-major, `resolution[0] * resolution[1]`.
    pub heights: String,
    pub resolution: [u32; 2],
    /// World units between samples.
    pub cell_size: f64,
    /// Splat layers, bottom first; each has a project-relative u8 weight map at `resolution`.
    pub layers: Vec<TerrainLayer>,
    /// Samples per render/collision chunk edge.
    pub chunk_size: u32,
    pub collision: bool,
    pub contents: u32,
    pub lightmap_scale: f64,
    /// Recipe that produced the heights; re-running it regenerates `heights`.
    pub recipe: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VoxelTerrain {
    /// Object-space bounds of the field.
    pub bounds: [DVec3; 2],
    pub voxel_size: f64,
    /// `rterrain` SDF operation list, evaluated in order.
    pub ops: serde_json::Value,
    pub material: String,
    pub uv_scale: f64,
    pub collision: bool,
    pub contents: u32,
    pub lightmap_scale: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainLayer {
    pub name: String,
    pub material: String,
    pub weights: String,
    pub uv_scale: f64,
    /// Scatter rules evaluated against this layer's weights.
    pub scatter: Vec<ScatterRule>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScatterRule {
    pub models: Vec<String>,
    /// Instances per 1000x1000 units at full weight.
    pub density: f64,
    pub min_weight: f32,
    pub slope_deg: [f32; 2],
    pub height: [f64; 2],
    pub scale: [f32; 2],
    pub align_to_normal: f32,
    pub seed: u64,
    pub static_prop: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    /// A shipped Apex map (client v51 + dedi v47 pair).
    ApexMap,
    /// A Titanfall 2 v37 map.
    Tf2Map,
}

/// A read-only map layer. Its lumps pass through the compiler unless promoted.
///
/// Every object decompiled out of the map is listed in `sources` with the
/// record it came from. The compiler reads that list to decide, per record:
///
/// - the object is still in the document, hidden and not in `promoted`: the
///   reference's own record is kept, byte for byte;
/// - the object is visible, or its id is in `promoted`: the record is replaced
///   by what the object compiles to, so there is one copy, not two;
/// - the object is gone from the document: the record is dropped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub kind: ReferenceKind,
    /// Map stem or source path, resolved by the host.
    pub source: String,
    /// Object ids promoted out of the reference; their original entries are dropped.
    pub promoted: Vec<String>,
    /// Remove these entity indices of the reference from the output.
    pub removed_entities: Vec<u32>,
    /// Decompiled object id -> the reference record it stands for.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub sources: IndexMap<Id, RefRecord>,
}

impl Reference {
    /// True when `id` has been promoted out of the reference by name.
    pub fn is_promoted(&self, id: &Id) -> bool {
        let text = id.to_string();
        self.promoted.contains(&text)
    }
}

/// One record of a reference map, addressed so it survives a recompile.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RefRecord {
    /// An entity block: its index in the map's enumeration, the main lump
    /// first and then the `.ent` partitions in `Partition::FILES` order.
    Entity { index: u32 },
    /// A static prop: its index in the `sprp` block.
    Prop { index: u32 },
    /// Every render mesh of one model that draws with one material.
    Mesh { model: u32, material: String },
    /// The collision leaves of one model with one contents mask and one
    /// surface that no render mesh of the model draws: clips, skybox walls.
    Collision { model: u32, contents: u32, surface: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub bake: BakeSettings,
    pub navmesh: NavmeshSettings,
    pub vis: VisSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BakeSettings {
    pub texel_size: f64,
    pub bounces: u32,
    pub samples: u32,
    pub sky_color: [f32; 3],
    pub sky_intensity: f32,
    pub ambient_occlusion: bool,
    pub probe_spacing: f64,
    pub cubemap_spacing: f64,
    /// Designer cubemap positions. When any are set they replace the grid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cubemap_points: Vec<[f64; 3]>,
    /// Mean linear albedo by material path or static prop model path: what a
    /// bounce off that surface keeps of the light. Anything unlisted is 0.5 grey.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub albedo: std::collections::BTreeMap<String, [f32; 3]>,
    /// Static props whose model path holds one of these never block baked
    /// light: light cards, foliage sheets, decals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub occluder_skip: Vec<String>,
    /// Fraction of baked light that passes through a surface of the material,
    /// by material path: the alpha-tested coverage of leaves, grates and fences.
    /// Unlisted surfaces that cast shadows block all of it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub shadow_transmission: std::collections::BTreeMap<String, f32>,
}

impl Default for BakeSettings {
    fn default() -> Self {
        Self {
            texel_size: 16.0,
            bounces: 2,
            samples: 256,
            sky_color: [0.5, 0.6, 0.8],
            sky_intensity: 1.0,
            ambient_occlusion: true,
            probe_spacing: 256.0,
            cubemap_spacing: 2048.0,
            cubemap_points: Vec::new(),
            albedo: Default::default(),
            occluder_skip: Vec::new(),
            shadow_transmission: Default::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavmeshSettings {
    pub enabled: bool,
    /// Hull names to bake (`small`, `med_short`, `medium`, `large`, `extra_large`).
    pub hulls: Vec<String>,
    pub bounds: Option<[DVec3; 2]>,
}

impl Default for NavmeshSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hulls: ["small", "med_short", "medium", "large", "extra_large"].iter().map(|s| s.to_string()).collect(),
            bounds: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VisSettings {
    /// Generate cells and portals from `hint`/`areaportal` brushes; otherwise one cell.
    pub portals: bool,
}
