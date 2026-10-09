//! Reference overlay: a document with a `Reference` object compiles as the
//! referenced map plus the authored content on top of it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use glam::Vec3;
use rbsp::lumps::entities;
use rbsp::Bsp;
use rdoc::model::{Partition, Reference as ReferenceDef};
use rir::{CollisionShape, CollisionSolid, ModelSlot, RenderSurface};

use crate::entities::Block;
use crate::props::PropSource;

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("the reference map {0} is not under the reference root")]
    NotFound(String),
    #[error("reference map {path}: {source}")]
    Map { path: PathBuf, source: rbsp::Error },
    #[error("reference map {0} decodes as neither a v47 nor a v51 map")]
    WrongVersion(String),
    #[error("reference map {map}: {message}")]
    Decode { map: String, message: String },
    #[error("path {0} leaves the reference root")]
    Escapes(String),
}

/// The referenced map, decoded into the shapes the writers take.
#[derive(Clone, Debug)]
pub struct Reference {
    pub stem: String,
    /// Render geometry the reference carries, with its own model indices.
    pub surfaces: Vec<RenderSurface>,
    pub solids: Vec<CollisionSolid>,
    pub models: Vec<ModelSlot>,
    pub props: Vec<PropSource>,
    /// The reference's entities, in enumeration order, tagged with the file
    /// they came from.
    pub entities: Vec<(Partition, Block)>,
    pub world: (Vec3, Vec3),
    /// Entity indices the document drops.
    pub removed: BTreeSet<u32>,
    /// The materials the reference's meshes name, so the asset table can be
    /// consulted for them.
    pub version: rbsp::Version,
}

impl Reference {
    /// The reference's model count, which is where an authored model index
    /// starts.
    pub fn model_count(&self) -> u32 {
        self.models.len() as u32
    }

    /// The reference's entity list with the dropped indices removed.
    pub fn kept_entities(&self) -> Vec<(Partition, Block)> {
        self.entities
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.removed.contains(&(*i as u32)))
            .map(|(_, e)| e.clone())
            .collect()
    }
}

/// The `.bsp` of a reference under `root`. A `source` that already ends in
/// `.bsp` is a path relative to the root; otherwise the map is looked for in
/// `<root>/<source>/maps/<source>.bsp` and then `<root>/<source>.bsp`.
pub fn locate(root: &Path, source: &str) -> Option<PathBuf> {
    if source.is_empty() {
        return None;
    }
    let mut candidates = Vec::new();
    if source.ends_with(".bsp") {
        candidates.push(root.join(source));
    } else {
        candidates.push(root.join(source).join("maps").join(format!("{source}.bsp")));
        candidates.push(root.join(source).join(format!("{source}.bsp")));
        candidates.push(root.join(format!("{source}.bsp")));
        candidates.push(root.join("maps").join(format!("{source}.bsp")));
        candidates.push(root.join(source));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Reads a reference map and decodes it into the compiler's shapes.
pub fn load(def: &ReferenceDef, root: &Path) -> Result<Reference, OverlayError> {
    let path = locate(root, &def.source).ok_or_else(|| OverlayError::NotFound(def.source.clone()))?;
    let map = def.source.clone();
    let bsp = Bsp::read_path(&path).map_err(|source| OverlayError::Map { path: path.clone(), source })?;
    let version = bsp
        .version_kind()
        .filter(|v| *v != rbsp::Version::Tf2V37)
        .ok_or_else(|| OverlayError::WrongVersion(map.clone()))?;
    let mut out = Reference {
        stem: stem_of(&path),
        version,
        surfaces: Vec::new(),
        solids: Vec::new(),
        models: Vec::new(),
        props: Vec::new(),
        entities: Vec::new(),
        world: (Vec3::ZERO, Vec3::ZERO),
        removed: BTreeSet::new(),
    };
    out.removed = def.removed_entities.iter().copied().collect();

    // A dedicated-server map carries the mesh table for its own bookkeeping and
    // none of the vertex lumps, so there is nothing to decode back into
    // surfaces.
    let renderless = bsp.lump(rbsp::ids::MESHES).is_empty() || bsp.lump(rbsp::ids::MESH_BOUNDS).is_empty();
    if !renderless {
        let decoded = crate::meshes::decode_surfaces(&bsp)
            .map_err(|e| OverlayError::Decode { map: map.clone(), message: e.to_string() })?;
        // A reference map can carry a mesh whose vertex pool has a slot no
        // corner ever uses, and those records are not always filled in. A
        // surface with a non-finite vertex cannot be re-emitted, so it is
        // dropped rather than failing the whole compile.
        out.surfaces = decoded
            .into_iter()
            .filter(|s| {
                !s.positions.iter().any(|v| !v.is_finite())
                    && !s.normals.iter().any(|v| !v.is_finite())
                    && !s.uv0.iter().any(|v| !v.is_finite())
                    && s.indices.iter().all(|i| (*i as usize) < s.positions.len())
            })
            .map(|mut s| {
                // The lightmap coordinates of a reference are page-relative and
                // are repacked by the baker, so a value that did not survive
                // the read is replaced rather than dropping the surface.
                for uv in &mut s.uv1 {
                    if !uv.is_finite() {
                        *uv = glam::Vec2::ZERO;
                    }
                }
                // Charted face by face the way an authored surface is, so an
                // untouched reference lays out its lightmap as its source did.
                // One that cannot be packed keeps the baker's own projection.
                let texel = s.lightmap_texel as f64;
                if crate::csg::chart_surface(&mut s, &[texel]).is_err() {
                    s.lightmap_size = [0, 0];
                }
                s
            })
            .collect();
    }
    let model_count = bsp.lump(rbsp::ids::MODELS).len() / 64;
    out.models = (0..model_count)
        .map(|i| ModelSlot {
            index: i as u32,
            owner: None,
            mins: Vec3::ZERO,
            maxs: Vec3::ZERO,
            has_render: false,
        })
        .collect();
    for m in &mut out.models {
        m.has_render = false;
    }
    // Model boxes come from the models lump the render stage reads back.
    if let Ok(rbsp::lumps::Lump::Model(list)) =
        rbsp::lumps::parse(rbsp::ids::MODELS, version, bsp.lump(rbsp::ids::MODELS))
    {
        for (i, m) in list.iter().enumerate() {
            if let Some(slot) = out.models.get_mut(i) {
                slot.mins = Vec3::from_array(m.mins);
                slot.maxs = Vec3::from_array(m.maxs);
            }
        }
    }
    for s in &out.surfaces {
        if let Some(slot) = out.models.get_mut(s.model as usize) {
            slot.has_render = true;
            for p in &s.positions {
                slot.mins = slot.mins.min(*p);
                slot.maxs = slot.maxs.max(*p);
            }
        }
    }
    for (model, tris) in crate::collision::decode_collision(&bsp)
        .map_err(|e| OverlayError::Decode { map: map.clone(), message: e.to_string() })?
    {
        if tris.is_empty() {
            continue;
        }
        let mut vertices = Vec::with_capacity(tris.len() * 3);
        let mut indices = Vec::with_capacity(tris.len() * 3);
        for t in tris {
            let base = vertices.len() as u32;
            vertices.extend(t.iter().map(|p| p.as_vec3()));
            indices.extend_from_slice(&[base, base + 1, base + 2]);
        }
        let (mins, maxs) = bounds_of(&vertices);
        if let Some(slot) = out.models.get_mut(model as usize) {
            slot.mins = slot.mins.min(mins);
            slot.maxs = slot.maxs.max(maxs);
        }
        out.solids.push(CollisionSolid {
            object: None,
            model,
            shape: CollisionShape::Triangles { vertices, indices },
            contents: crate::collision::CONTENTS_EMPTY,
            surface_prop: 0,
            surfaces: Vec::new(),
        });
    }
    out.props = props_of(&bsp, version);
    out.world = world_of(&out.models);
    let dir = path.parent().unwrap_or(root).to_path_buf();
    out.entities = entities_of(&bsp, version, &dir, &out.stem)?;
    Ok(out)
}

fn stem_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().trim_end_matches(".bsp").to_string()).unwrap_or_default()
}

fn bounds_of(vertices: &[Vec3]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for v in vertices {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    (lo, hi)
}

fn world_of(models: &[ModelSlot]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for m in models {
        if m.mins.x > m.maxs.x {
            continue;
        }
        lo = lo.min(m.mins);
        hi = hi.max(m.maxs);
    }
    if lo.x > hi.x {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    (lo, hi)
}

fn props_of(bsp: &Bsp, version: rbsp::Version) -> Vec<PropSource> {
    let Ok(rbsp::lumps::Lump::GameLump(gl)) =
        rbsp::lumps::parse_at(rbsp::ids::GAME_LUMP, version, bsp.lump(rbsp::ids::GAME_LUMP), 0)
    else {
        return Vec::new();
    };
    let Some(block) = gl.static_props() else { return Vec::new() };
    (0..block.prop_count())
        .filter_map(|i| {
            let p = block.props.get(i)?;
            let model = block.model_of(i)?;
            Some(PropSource {
                model: model.to_string(),
                origin: Vec3::from_array(p.origin),
                angles: Vec3::from_array(p.angles),
                scale: p.scale,
                skin: p.unknown_2 as i32,
                solid: false,
                cast_shadows: true,
                fade_distance: p.unknown_4,
                bounds_min: Vec3::ZERO,
                bounds_max: Vec3::ZERO,
                sky: false,
                keys: Default::default(),
            })
        })
        .collect()
}

fn entities_of(
    bsp: &Bsp,
    version: rbsp::Version,
    dir: &Path,
    stem: &str,
) -> Result<Vec<(Partition, Block)>, OverlayError> {
    let mut out: Vec<(Partition, Block)> = Vec::new();
    let world = bsp.lump(rbsp::ids::ENTITIES);
    if !world.is_empty() {
        let text = entities::parse(world, version)
            .map_err(|e| OverlayError::Decode { map: stem.to_string(), message: e.to_string() })?;
        for b in text.entities {
            out.push((Partition::Main, b));
        }
    }
    for p in Partition::FILES {
        let Some(path) = reference_sidecar(dir, stem, p) else { continue };
        let data = std::fs::read(&path).map_err(|e| OverlayError::Decode { map: stem.to_string(), message: e.to_string() })?;
        let text = entities::parse(&data, version)
            .map_err(|e| OverlayError::Decode { map: stem.to_string(), message: e.to_string() })?;
        for b in text.entities {
            out.push((p, b));
        }
    }
    Ok(out)
}

fn reference_sidecar(root: &Path, stem: &str, p: Partition) -> Option<PathBuf> {
    let suffix = p.suffix()?;
    let path = root.join(format!("{stem}_{suffix}.ent"));
    path.is_file().then_some(path)
}

/// The `.ent` file a partition of a reference map lives in.
pub fn sidecar_name(stem: &str, p: Partition) -> String {
    p.sidecar(stem).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_prefers_the_maps_subdirectory() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("mp_test").join("maps")).unwrap();
        std::fs::write(dir.join("mp_test").join("maps").join("mp_test.bsp"), b"rBSP").unwrap();
        let p = locate(&dir, "mp_test").expect("the map is found");
        assert!(p.ends_with("maps/mp_test.bsp") || p.ends_with("maps\\mp_test.bsp"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_reference_is_an_error() {
        let dir = tempdir();
        let def = ReferenceDef {
            kind: rdoc::model::ReferenceKind::ApexMap,
            source: "mp_absent".into(),
            promoted: Vec::new(),
            removed_entities: Vec::new(),
            sources: Default::default(),
        };
        assert!(matches!(load(&def, &dir), Err(OverlayError::NotFound(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn empty_reference() -> Reference {
        Reference {
            stem: String::new(),
            version: rbsp::Version::ClientV51,
            surfaces: Vec::new(),
            solids: Vec::new(),
            models: Vec::new(),
            props: Vec::new(),
            entities: Vec::new(),
            world: (Vec3::ZERO, Vec3::ZERO),
            removed: BTreeSet::new(),
        }
    }

    #[test]
    fn kept_entities_drop_the_removed_indices() {
        let mut r = empty_reference();
        for i in 0..4u32 {
            r.entities.push((Partition::Main, entities::Entity { pairs: vec![("classname".into(), format!("c{i}"))] }));
        }
        r.removed.insert(1);
        r.removed.insert(3);
        let kept = r.kept_entities();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].1.get("classname"), Some("c0"));
        assert_eq!(kept[1].1.get("classname"), Some("c2"));
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("r5ed-overlay-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        p
    }
}
