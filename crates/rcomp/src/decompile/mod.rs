//! Decompile: a compiled map read back as an editable document.
//!
//! The document keeps a `Reference` to the map it came from, and every object
//! this module produces is hidden: the reference already supplies that content,
//! so a compile with no edits hands the map straight back. A decompiled object
//! joins the output once it is unhidden, which is how content is promoted out of
//! the reference.

mod entities;
mod geometry;
pub mod props;

use std::collections::BTreeMap;

use indexmap::IndexMap;
use rbsp::Bsp;
use rdoc::model::{
    Id, Layer, MapDoc, MapMeta, Object, ObjectKind, Partition, RefRecord, Reference, ReferenceKind, Transform,
};
use thiserror::Error;
use uuid::Uuid;

pub use entities::{vec3 as entity_vec3, Block};

#[derive(Debug, Error)]
pub enum DecompileError {
    #[error("map {stem} declares lump version {version}, which this build does not read")]
    UnknownVersion { stem: String, version: i32 },
    #[error("map {stem}: {message}")]
    Decode { stem: String, message: String },
    #[error("the {name} partition of {stem} does not parse: {message}")]
    Partition { stem: String, name: String, message: String },
    #[error("mesh {name} does not write as a glb: {message}")]
    Glb { name: String, message: String },
    #[error("{0}")]
    Invalid(String),
}

fn decode(stem: &str, e: impl std::fmt::Display) -> DecompileError {
    DecompileError::Decode { stem: stem.to_string(), message: e.to_string() }
}

/// Reads a compiled map into a document and the project files its meshes need.
///
/// `client` is the map the document is built from; it may be the client map or
/// the dedicated-server map of a pair, and a lump it does not carry is taken
/// from `dedi`. `ents` holds the `.ent` partition files, keyed by partition name
/// or by file name.
pub fn decompile(
    client: &Bsp,
    dedi: Option<&Bsp>,
    ents: &BTreeMap<String, Vec<u8>>,
    stem: &str,
) -> Result<(MapDoc, BTreeMap<String, Vec<u8>>), DecompileError> {
    let Some(version) = client.version_kind() else {
        return Err(DecompileError::UnknownVersion { stem: stem.to_string(), version: client.header.version as i32 });
    };
    let layer = Layer { id: Uuid::new_v4(), name: "map".into(), visible: true, locked: false };
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    let blocks = entities::read(client, dedi, ents, version, stem)?;
    // Every object below stands for one record of the map, and the reference
    // keeps the address of that record, so the compiler can tell a record that
    // is untouched from one that was promoted or deleted.
    let mut sources: IndexMap<Id, RefRecord> = IndexMap::new();
    let mut objects: Vec<Object> = Vec::new();
    for (i, o) in entities::objects(&blocks, layer.id, client)?.into_iter().enumerate() {
        sources.insert(o.id, RefRecord::Entity { index: i as u32 });
        objects.push(o);
    }
    // Static props stay in the reference: the page draws them from a table and
    // an edit promotes one into an object.
    let prop_map = prop_source(client, dedi);
    if let Some(table) = props::prop_table(prop_map, prop_map.version_kind().unwrap_or(version))? {
        files.insert(props::PROP_TABLE_PATH.to_string(), table);
    }
    for (o, record) in geometry::world_meshes(client, layer.id, &mut files)? {
        sources.insert(o.id, record);
        objects.push(o);
    }
    objects.insert(0, reference_object(stem, layer.id, sources));

    let mut doc = MapDoc {
        format: rdoc::model::FORMAT_VERSION,
        meta: MapMeta { stem: stem.to_string(), display_name: stem.to_string(), ..Default::default() },
        layers: vec![layer],
        objects,
        settings: Default::default(),
    };
    doc.settings.bake.cubemap_points = shipped_cubemap_points(client, dedi);
    Ok((doc, files))
}

/// The shipped cubemap positions, so a rebake samples where the shipped map
/// did and props keep their cubemap slots. The client export zero-fills the
/// lump, so a zeroed half carries nothing and the other half is read.
fn shipped_cubemap_points(client: &Bsp, dedi: Option<&Bsp>) -> Vec<[f64; 3]> {
    let mut halves = vec![client];
    halves.extend(dedi);
    for bsp in halves {
        let v = bsp.version_kind().unwrap_or(rbsp::Version::ClientV51);
        let Ok(cubes) = rbsp::lumps::lighting::parse_cubemaps(bsp.lump(rbsp::ids::CUBEMAPS), v) else { continue };
        let points: Vec<[f64; 3]> =
            cubes.iter().map(|c| [c.unknown_0 as f64, c.unknown_1 as f64, c.unknown_2 as f64]).collect();
        if points.iter().any(|p| *p != [0.0, 0.0, 0.0]) {
            return points;
        }
    }
    Vec::new()
}

/// The half that carries the prop list: the server half when the client
/// half has none.
pub fn prop_source<'a>(client: &'a Bsp, dedi: Option<&'a Bsp>) -> &'a Bsp {
    if client.lump(rbsp::ids::GAME_LUMP).is_empty() { dedi.unwrap_or(client) } else { client }
}

fn reference_mut(doc: &mut MapDoc) -> Option<&mut Reference> {
    doc.objects.iter_mut().find_map(|o| match &mut o.kind {
        ObjectKind::Reference(r) => Some(r),
        _ => None,
    })
}

/// Makes static prop record `index` of the reference an object of the
/// document, hidden, so the record still compiles until the object is edited.
/// Returns the object that already stands for it when there is one.
pub fn promote_prop(doc: &mut MapDoc, props_map: &Bsp, index: u32) -> Result<Id, DecompileError> {
    let reference = reference_mut(doc).ok_or_else(|| DecompileError::Invalid("the document references no map".into()))?;
    if let Some((id, _)) = reference.sources.iter().find(|(_, r)| **r == RefRecord::Prop { index }) {
        return Ok(*id);
    }
    let layer = doc.layers.first().map(|l| l.id).unwrap_or_else(Uuid::new_v4);
    let version = props_map.version_kind().unwrap_or(rbsp::Version::ClientV51);
    let object = props::static_prop(props_map, version, layer, index)?
        .ok_or_else(|| DecompileError::Invalid(format!("the reference has no prop record {index}")))?;
    let id = object.id;
    if let Some(r) = reference_mut(doc) {
        r.sources.insert(id, RefRecord::Prop { index });
    }
    doc.objects.push(object);
    Ok(id)
}

fn reference_object(stem: &str, layer: Id, sources: IndexMap<Id, RefRecord>) -> Object {
    Object {
        id: Uuid::new_v4(),
        name: format!("{stem} reference"),
        layer,
        parent: None,
        transform: Transform::default(),
        hidden: false,
        locked: true,
        kind: ObjectKind::Reference(Reference {
            kind: ReferenceKind::ApexMap,
            source: stem.to_string(),
            promoted: Vec::new(),
            removed_entities: Vec::new(),
            sources,
        }),
    }
}

/// A decompiled document with its reference taken away: every record the
/// reference still supplied becomes the object that stands for it, and the
/// map compiles from the document alone.
///
/// A record the document dropped stays dropped: its object, if it still has
/// one, is removed. A promoted object stays as it is. Objects on a hidden layer
/// are left alone, and so are objects no reference names.
///
/// `props_map` is the reference half that carries the static props: a prop the
/// document never promoted lives only there and becomes an object here.
pub fn detach(doc: &MapDoc, props_map: Option<&Bsp>) -> Result<MapDoc, DecompileError> {
    let mut out = doc.clone();
    let references: Vec<(Id, Reference)> = doc
        .objects
        .iter()
        .filter_map(|o| match &o.kind {
            ObjectKind::Reference(r) => Some((o.id, r.clone())),
            _ => None,
        })
        .collect();
    let mut drop: std::collections::HashSet<Id> = references.iter().map(|(id, _)| *id).collect();
    for (_, r) in &references {
        let removed: std::collections::HashSet<u32> = r.removed_entities.iter().copied().collect();
        for (id, record) in &r.sources {
            let Some(o) = out.objects.iter_mut().find(|o| o.id == *id) else { continue };
            let gone = r.is_promoted(id) && o.hidden
                || matches!(record, RefRecord::Entity { index } if removed.contains(index));
            if gone {
                drop.insert(*id);
            } else {
                o.hidden = false;
            }
        }
    }
    out.objects.retain(|o| !drop.contains(&o.id));
    if let (Some(map), Some((_, r))) = (props_map, references.first()) {
        let taken: std::collections::HashSet<u32> = r
            .sources
            .values()
            .filter_map(|x| match x {
                RefRecord::Prop { index } => Some(*index),
                _ => None,
            })
            .collect();
        let layer = doc.layers.first().map(|l| l.id).unwrap_or_else(Uuid::new_v4);
        let version = map.version_kind().unwrap_or(rbsp::Version::ClientV51);
        for (i, mut o) in props::static_props(map, version, layer)?.into_iter().enumerate() {
            if !taken.contains(&(i as u32)) {
                o.hidden = false;
                out.objects.push(o);
            }
        }
    }
    // A shipped map lights tens of thousands of units with two 1024 pages; at
    // the editor's default of 16 units a texel staging_mu1 bakes to 31 pages
    // and a 480 MB client. 64 units lands near the shipped texel count.
    if !references.is_empty() && out.settings.bake.texel_size < DETACHED_TEXEL {
        out.settings.bake.texel_size = DETACHED_TEXEL;
    }
    Ok(out)
}

/// The lightmap texel size a detached shipped map bakes at, in units.
pub const DETACHED_TEXEL: f64 = 64.0;

/// The partition file bytes for `p`, by partition name or by file name.
pub(crate) fn partition_bytes<'a>(
    ents: &'a BTreeMap<String, Vec<u8>>,
    stem: &str,
    p: Partition,
) -> Option<&'a [u8]> {
    let by_name = ents.get(p.name()).map(Vec::as_slice);
    let by_file = p.sidecar(stem).and_then(|f| ents.get(&f)).map(Vec::as_slice);
    by_name.or(by_file)
}

/// An object this module produced: hidden, because the reference map already
/// carries it.
pub(crate) fn hidden_object(name: String, layer: Id, transform: Transform, kind: ObjectKind) -> Object {
    Object {
        id: Uuid::new_v4(),
        name,
        layer,
        parent: None,
        transform,
        hidden: true,
        locked: false,
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdoc::model::{ObjectKind, Partition};

    fn world_text(blocks: &[(&str, &str)]) -> Vec<u8> {
        let text = rbsp::lumps::entities::EntityText {
            header: None,
            entities: blocks
                .iter()
                .map(|(c, n)| rbsp::lumps::entities::Entity {
                    pairs: vec![("classname".into(), (*c).into()), ("name".into(), (*n).into())],
                })
                .collect(),
            tail: vec![0],
        };
        text.to_text().into_bytes()
    }

    fn partition_text(classname: &str) -> Vec<u8> {
        let text = rbsp::lumps::entities::EntityText {
            header: Some(rbsp::lumps::entities::TextHeader {
                ident: "ENTITIES02".into(),
                num_models: 1,
            }),
            entities: vec![rbsp::lumps::entities::Entity {
                pairs: vec![("classname".into(), classname.into())],
            }],
            tail: vec![0],
        };
        text.to_text().into_bytes()
    }

    fn map_with(blocks: &[(&str, &str)]) -> Bsp {
        let mut bsp = Bsp::new(51, 1, 30);
        bsp.set_lump(rbsp::ids::ENTITIES, world_text(blocks));
        bsp
    }

    #[test]
    fn every_decompiled_object_knows_its_record() {
        let map = map_with(&[("worldspawn", "world"), ("info_player_start", "s")]);
        let (doc, files) = decompile(&map, None, &BTreeMap::new(), "mp_rr_x").expect("the map decompiles");
        assert!(files.is_empty(), "a map with no render geometry writes no project file");
        let ObjectKind::Reference(r) = &doc.objects[0].kind else { panic!("the first object is the reference") };
        assert_eq!(r.source, "mp_rr_x");
        assert!(r.removed_entities.is_empty(), "the map's own worldspawn block passes through");
        assert_eq!(r.sources.len(), 2, "one source record per decompiled object");
        assert_eq!(r.sources.get(&doc.objects[1].id), Some(&RefRecord::Entity { index: 0 }));
        assert_eq!(r.sources.get(&doc.objects[2].id), Some(&RefRecord::Entity { index: 1 }));
        assert_eq!(doc.meta.stem, "mp_rr_x");
        assert_eq!(doc.objects.len(), 3, "the reference, the worldspawn block and the entity");
        assert!(!doc.objects[0].hidden, "the reference is what the compile reads");
        assert!(doc.objects[2].hidden, "decompiled content does not compile until it is unhidden");
        let ObjectKind::Entity(e) = &doc.objects[2].kind else { panic!("a point entity stays an entity") };
        assert_eq!(e.classname, "info_player_start");
        assert_eq!(e.partition, Some(Partition::Main));
        assert_eq!(e.keyvalues.get("name").map(String::as_str), Some("s"));
    }

    #[test]
    fn a_partition_file_is_read_by_name_and_by_file_name() {
        let map = map_with(&[("worldspawn", "world")]);
        let by_name = BTreeMap::from([("script".to_string(), partition_text("trigger_hurt"))]);
        let by_file = BTreeMap::from([("mp_rr_x_script.ent".to_string(), partition_text("trigger_hurt"))]);
        for ents in [by_name, by_file] {
            let (doc, _) = decompile(&map, None, &ents, "mp_rr_x").expect("the map decompiles");
            let partition = doc
                .objects
                .iter()
                .filter_map(|o| match &o.kind {
                    ObjectKind::Entity(e) if e.classname == "trigger_hurt" => e.partition,
                    _ => None,
                })
                .next();
            assert_eq!(partition, Some(Partition::Script), "the partition the file carried is recorded");
        }
    }

    #[test]
    fn the_entity_lump_falls_back_to_the_dedicated_map() {
        let mut client = Bsp::new(51, 1, 30);
        client.set_lump(rbsp::ids::GAME_LUMP, Vec::new());
        let mut dedi = Bsp::new(47, 0, 30);
        dedi.set_lump(rbsp::ids::ENTITIES, world_text(&[("worldspawn", "world"), ("info_target", "t")]));
        let (doc, _) = decompile(&client, Some(&dedi), &BTreeMap::new(), "mp_rr_x").expect("the pair decompiles");
        let classes: Vec<&str> = doc
            .objects
            .iter()
            .filter_map(|o| match &o.kind {
                ObjectKind::Entity(e) => Some(e.classname.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(classes, vec!["worldspawn", "info_target"], "a client map with no entity lump takes the server's");
    }

    #[test]
    fn a_map_with_no_lumps_decompiles_to_the_reference_alone() {
        let map = Bsp::new(47, 0, 30);
        let (doc, files) = decompile(&map, None, &BTreeMap::new(), "mp_rr_x").expect("an empty map decompiles");
        assert_eq!(doc.objects.len(), 1);
        assert!(files.is_empty());
        assert!(rdoc::doc::validate(&doc).is_ok(), "the document is well formed");
    }

    #[test]
    fn a_version_this_build_does_not_read_is_an_error() {
        let mut map = Bsp::new(37, 1, 30);
        map.set_lump(rbsp::ids::ENTITIES, world_text(&[("worldspawn", "world")]));
        map.header.version = 61;
        let err = decompile(&map, None, &BTreeMap::new(), "mp_rr_x").expect_err("an unknown version is refused");
        assert!(matches!(err, DecompileError::UnknownVersion { .. }), "{err}");
    }

    #[test]
    fn entity_blocks_keep_their_pairs_in_order() {
        let text = rbsp::lumps::entities::EntityText {
            header: None,
            entities: vec![rbsp::lumps::entities::Entity {
                pairs: vec![
                    ("classname".into(), "info_target".into()),
                    ("targetname".into(), "first".into()),
                    ("origin".into(), "1 2 3".into()),
                    ("angles".into(), "0 90 0".into()),
                ],
            }],
            tail: vec![0],
        };
        let mut map = Bsp::new(51, 1, 30);
        map.set_lump(rbsp::ids::ENTITIES, text.to_text().into_bytes());
        let (doc, _) = decompile(&map, None, &BTreeMap::new(), "mp_rr_x").expect("the map decompiles");
        let entity = doc.objects.iter().find_map(|o| match &o.kind {
            ObjectKind::Entity(e) => Some(e),
            _ => None,
        });
        let e = entity.expect("the block became an entity");
        let keys: Vec<&str> = e.keyvalues.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["targetname"], "origin and angles move to the transform");
        assert_eq!(doc.objects[1].transform.position, glam::DVec3::new(1.0, 2.0, 3.0));
        assert!(doc.objects[1].transform.to_source_angles().y > 89.0, "the yaw round trips");
    }

    #[test]
    fn an_entity_without_a_classname_keeps_its_keys() {
        let text = rbsp::lumps::entities::EntityText {
            header: None,
            entities: vec![rbsp::lumps::entities::Entity { pairs: vec![("name".into(), "odd".into())] }],
            tail: vec![0],
        };
        let mut map = Bsp::new(51, 1, 30);
        map.set_lump(rbsp::ids::ENTITIES, text.to_text().into_bytes());
        let (doc, _) = decompile(&map, None, &BTreeMap::new(), "mp_rr_x").expect("the map decompiles");
        let ObjectKind::Entity(e) = &doc.objects[1].kind else { panic!("the block stayed an entity") };
        assert_eq!(e.classname, "");
        assert_eq!(e.keyvalues.get("name").map(String::as_str), Some("odd"));
    }

    #[test]
    fn the_entity_schema_accepts_a_decompiled_map() {
        let map = map_with(&[("worldspawn", "world"), ("info_player_start", "s")]);
        let (doc, _) = decompile(&map, None, &BTreeMap::new(), "mp_rr_x").expect("the map decompiles");
        let issues: Vec<String> = doc
            .objects
            .iter()
            .filter_map(|o| match &o.kind {
                ObjectKind::Entity(e) => Some(format!("{}: {:?}", e.classname, e.keyvalues)),
                _ => None,
            })
            .collect();
        assert_eq!(issues.len(), 2, "the worldspawn block and the spawn point");
        for i in &issues {
            let class = i.split(':').next().unwrap_or_default();
            assert!(crate::default_schema().class(class).is_some(), "{i} has no schema class");
        }
    }

    #[test]
    fn a_duplicate_key_collapses_to_the_last_value() {
        let text = rbsp::lumps::entities::EntityText {
            header: None,
            entities: vec![rbsp::lumps::entities::Entity {
                pairs: vec![
                    ("classname".into(), "info_target".into()),
                    ("k".into(), "first".into()),
                    ("k".into(), "second".into()),
                ],
            }],
            tail: vec![0],
        };
        let mut map = Bsp::new(51, 1, 30);
        map.set_lump(rbsp::ids::ENTITIES, text.to_text().into_bytes());
        let (doc, _) = decompile(&map, None, &BTreeMap::new(), "mp_rr_x").expect("the map decompiles");
        let ObjectKind::Entity(e) = &doc.objects[1].kind else { panic!("the block stayed an entity") };
        assert_eq!(e.keyvalues.get("k").map(String::as_str), Some("second"));
    }

    #[test]
    fn a_partition_that_does_not_parse_is_reported_with_its_name() {
        let map = map_with(&[("worldspawn", "world")]);
        let ents = BTreeMap::from([("spawn".to_string(), b"{\"classname\"".to_vec())]);
        let err = decompile(&map, None, &ents, "mp_rr_x").expect_err("a truncated partition is refused");
        match err {
            DecompileError::Partition { name, .. } => assert_eq!(name, "spawn"),
            other => panic!("{other}"),
        }
    }

    #[test]
    fn a_doc_from_a_map_is_valid() {
        let map = map_with(&[("worldspawn", "world"), ("light", "l")]);
        let (doc, _) = decompile(&map, None, &BTreeMap::new(), "mp_rr_x").expect("the map decompiles");
        rdoc::doc::validate(&doc).expect("the document is well formed");
        assert_eq!(doc.layers.len(), 1);
        for o in &doc.objects {
            assert_eq!(o.layer, doc.layers[0].id, "{} sits on the map layer", o.name);
        }
        let light = doc.objects.iter().find_map(|o| match &o.kind {
            ObjectKind::Light(l) => Some(l),
            _ => None,
        });
        let l = light.expect("a light block became a light");
        assert_eq!(l.kind, rdoc::model::LightKind::Point);
    }
}
