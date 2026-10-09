//! r5ed terrain: heightfield and volumetric generation, erosion, sculpting,
//! splat painting, scatter and meshing. Units are Source inches, Z up.

pub mod erosion;
pub mod error;
pub mod filters;
pub mod generators;
pub mod heightfield;
pub mod heightmap;
pub mod mask;
pub mod math;
pub mod mesher;
pub mod noise;
pub mod parallel;
pub mod recipe;
pub mod rng;
pub mod sculpt;
pub mod sdf;
pub mod splat;
pub mod terrain_mesh;

pub use erosion::{hydraulic, pipe_erosion, thermal, HydraulicParams, PipeParams, ThermalParams};
pub use error::{Error, Result};
pub use filters::{BlendMode, ToneCurve};
pub use generators::{Generator, ShapeParams};
pub use heightfield::{Filter, Flow, Heightfield, Rect};
pub use heightmap::HeightmapSource;
pub use mask::Mask;
pub use mesher::{MeshVertex, VolumeMesh, VolumeMeshChunk};
pub use noise::{Fractal, FractalParams, Noise, NoiseKind, Worley2};
pub use recipe::{NodeKind, Recipe};
pub use sculpt::{apply_stroke, Stroke, StrokeResult, SculptTool};
pub use sdf::{SdfOp, SdfShape, VolumeField, VolumeTerrain};
pub use splat::{autopaint, scatter, LayerRule, Placement, SplatMaps};
pub use terrain_mesh::{MeshParams, TerrainMesh, TerrainMeshChunk};
