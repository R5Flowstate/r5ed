//! Intermediate representation shared by the compiler stages and the baker.
//! World space, Source inches, Z up.

use glam::{Vec2, Vec3, Vec4};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Vertex layout a material's shader expects; selects the vertex lump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VertexFormat {
    Unlit,
    LitFlat,
    LitBump,
    UnlitTs,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaterialInfo {
    pub path: String,
    pub guid: u64,
    pub shader_set: String,
    pub vertex_format: VertexFormat,
    pub surface_prop: String,
    pub translucent: bool,
    #[serde(default)]
    pub decal: bool,
    pub nodraw: bool,
    /// Texture slot name -> texture guid.
    pub textures: std::collections::BTreeMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub path: String,
    pub guid: u64,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub has_collision: bool,
    /// Paks that already carry this model on each target.
    pub resident_paks: Vec<String>,
}

/// Asset metadata the compiler needs; the host fills it from the game paks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AssetTables {
    pub materials: std::collections::BTreeMap<String, MaterialInfo>,
    pub models: std::collections::BTreeMap<String, ModelInfo>,
    pub surface_props: std::collections::BTreeMap<String, u32>,
}

/// One drawable batch: a single material on a single BSP model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderSurface {
    /// Source object in the document, for reports and lightmap reuse.
    pub object: Option<Uuid>,
    /// 0 = worldspawn, N = brush-entity model `*N`.
    pub model: u32,
    pub material: String,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    /// xyz tangent, w handedness.
    pub tangents: Vec<Vec4>,
    pub uv0: Vec<Vec2>,
    /// Lightmap UV in 0..1 of the surface's own chart; the baker packs charts into pages.
    pub uv1: Vec<Vec2>,
    pub colors: Vec<[u8; 4]>,
    pub indices: Vec<u32>,
    pub cast_shadows: bool,
    /// World units per lightmap texel.
    pub lightmap_texel: f32,
    /// Texel size of the `uv1` atlas, or `[0, 0]` when `uv1` is not one and the
    /// baker projects the surface onto a plane of its own.
    #[serde(default)]
    pub lightmap_size: [u32; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CollisionShape {
    /// Convex solid by outward planes (normal, dist).
    Brush { planes: Vec<(Vec3, f32)> },
    Triangles { vertices: Vec<Vec3>, indices: Vec<u32> },
    ConvexHull { points: Vec<Vec3> },
    /// A static prop's own collision, placed by its record: the leaf names the
    /// prop and the slot box bounds where a trace can reach it. `word` is the
    /// whole leaf dword, the prop index in the low 16 bits and the flags the
    /// shipped maps keep above it.
    Prop { word: u32, mins: Vec3, maxs: Vec3 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollisionSolid {
    pub object: Option<Uuid>,
    pub model: u32,
    pub shape: CollisionShape,
    pub contents: u32,
    /// Index into the SURFACE_PROPERTIES lump table.
    pub surface_prop: u32,
    /// What the solid is made of, when it has a material of its own; this wins
    /// over `surface_prop`. One entry covers every side, otherwise one per plane.
    #[serde(default)]
    pub surfaces: Vec<CollisionSurface>,
}

/// A collision face's material and its surfaceproperties id, the pair a trace
/// hands to impacts, footsteps and decals.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CollisionSurface {
    pub material: String,
    pub surface_type: u8,
}

/// A BSP model slot (0 = world). Brush entities and volumes get one each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelSlot {
    pub index: u32,
    pub owner: Option<Uuid>,
    pub mins: Vec3,
    pub maxs: Vec3,
    /// Mesh-less models (trigger volumes) still carry collision.
    pub has_render: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightType {
    Sun,
    Point,
    Spot,
    Area,
    Sky,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BakeLight {
    pub object: Option<Uuid>,
    pub kind: LightType,
    pub position: Vec3,
    /// Unit vector the light points along (sun: direction light travels).
    pub direction: Vec3,
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
    pub inner_angle: f32,
    pub outer_angle: f32,
    pub size: Vec2,
    pub cast_shadows: bool,
    pub baked: bool,
    pub realtime: bool,
    /// The slot a script-tweakable light takes in the tweak-light table.
    #[serde(default)]
    pub tweak_index: Option<u32>,
    /// The record's own falloff, `c + l*d + q*d^2` out to `range`, in place of
    /// the baker's default curve.
    #[serde(default)]
    pub attenuation: Option<[f32; 3]>,
    /// The world light record's flags, in place of the ones its kind gets.
    #[serde(default)]
    pub flags: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaticPropInstance {
    pub model: String,
    pub origin: Vec3,
    /// Pitch, yaw, roll degrees.
    pub angles: Vec3,
    pub scale: f32,
    pub skin: i32,
    pub solid: bool,
    pub cast_shadows: bool,
    pub fade_distance: f32,
    /// Model space: rotation, scale and origin are not applied.
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
}

impl StaticPropInstance {
    /// The centre of the model bounds, placed in the world.
    pub fn world_centre(&self) -> Vec3 {
        let local = if self.bounds_min.cmple(self.bounds_max).all() {
            (self.bounds_min + self.bounds_max) * 0.5
        } else {
            Vec3::ZERO
        };
        let (pitch, yaw, roll) =
            (self.angles.x.to_radians(), self.angles.y.to_radians(), self.angles.z.to_radians());
        let rot = glam::Quat::from_euler(glam::EulerRot::ZYX, yaw, pitch, roll);
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        self.origin + rot * (local * scale)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BakeSettings {
    pub texel_size: f32,
    pub bounces: u32,
    pub samples: u32,
    pub sky_color: Vec3,
    pub sky_intensity: f32,
    pub ambient_occlusion: bool,
    pub probe_spacing: f32,
    pub cubemap_spacing: f32,
}

/// Everything the baker reads. Surfaces are already in world space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BakeScene {
    pub surfaces: Vec<RenderSurface>,
    pub props: Vec<StaticPropInstance>,
    /// Triangles of static props for occlusion (prop index, positions, indices).
    pub prop_occluders: Vec<(u32, Vec<Vec3>, Vec<u32>)>,
    /// Each entry of `props`' position in the map's static prop lump, which
    /// also holds the skybox vista the bake leaves out.
    #[serde(default)]
    pub prop_lump_index: Vec<u32>,
    /// Faces that are never drawn but block light (Source `nodraw` sides of
    /// solid brushes), as (positions, indices) batches of at most 65535 vertices.
    #[serde(default)]
    pub shadow_only: Vec<(Vec<Vec3>, Vec<u32>)>,
    /// Surfaces that take a lightmap but never block light: decals that float
    /// over the face they mark, water, glass and light cards.
    #[serde(default)]
    pub see_through: Vec<u32>,
    /// Surfaces that let part of the light through, as (surface, fraction):
    /// alpha-tested leaves and grates, whose holes the tracer cannot see.
    #[serde(default)]
    pub shadow_transmission: Vec<(u32, f32)>,
    /// Light probes are traced without the static props: each sits at its
    /// prop's lighting origin, inside the prop.
    #[serde(default)]
    pub probes_ignore_props: bool,
    pub lights: Vec<BakeLight>,
    pub settings: BakeSettings,
    pub world_mins: Vec3,
    pub world_maxs: Vec3,
    /// Designer-placed cubemap/probe positions (in addition to the automatic grid).
    pub cubemap_points: Vec<Vec3>,
    pub probe_points: Vec<Vec3>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prop_centre_is_placed_in_the_world() {
        let p = StaticPropInstance {
            model: String::new(),
            origin: Vec3::new(1000.0, 2000.0, 300.0),
            angles: Vec3::new(0.0, 90.0, 0.0),
            scale: 2.0,
            skin: 0,
            solid: true,
            cast_shadows: true,
            fade_distance: -1.0,
            bounds_min: Vec3::new(0.0, -4.0, 0.0),
            bounds_max: Vec3::new(20.0, 4.0, 10.0),
        };
        let c = p.world_centre();
        assert!((c - Vec3::new(1000.0, 2020.0, 310.0)).length() < 1e-3, "{c}");
    }
}
