//! Edits to the shipped staging_mu1, compiled and read back.
//!
//! Each test decompiles the stock pair, makes one kind of edit, compiles both
//! targets and checks the result against the stock bytes: one copy of every
//! promoted record, nothing left of a deleted one, and every lump the edit
//! does not own still the shipped one.

mod common;
mod kit;

use std::collections::{BTreeMap, BTreeSet};

use common::{compile, Assets};
use glam::{DVec3, Vec3};
use kit::{decompiled, half, kit, options};
use rbsp::lumps::game_lump::sprp::StaticProp;
use rbsp::{ids, Bsp, Version};
use rcomp::collision::decode_leaves;
use rcomp::{CompileOutput, MapArtifacts};
use rdoc::model::{Entity, Id, MapDoc, Object, ObjectKind, Prop, RefRecord, Reference};
use uuid::Uuid;

fn reference(doc: &MapDoc) -> &Reference {
    doc.objects
        .iter()
        .find_map(|o| match &o.kind {
            ObjectKind::Reference(r) => Some(r),
            _ => None,
        })
        .expect("the document references the map")
}

/// The object that stands for a record.
fn object_for(doc: &MapDoc, record: &RefRecord) -> Id {
    *reference(doc).sources.iter().find(|(_, r)| *r == record).map(|(id, _)| id).expect("the record has an object")
}

fn object_mut<'a>(doc: &'a mut MapDoc, id: Id) -> &'a mut Object {
    doc.objects.iter_mut().find(|o| o.id == id).expect("the object is in the document")
}

fn props(bsp: &Bsp) -> Vec<StaticProp> {
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let base = bsp.header.lumps[ids::GAME_LUMP as usize].offset;
    match rbsp::lumps::parse_at(ids::GAME_LUMP, v, bsp.lump(ids::GAME_LUMP), base) {
        Ok(rbsp::lumps::Lump::GameLump(gl)) => gl.static_props().map(|b| b.props.clone()).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn first_prop_ref(bsp: &Bsp) -> u32 {
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    match rbsp::lumps::parse(ids::LEVEL_INFO, v, bsp.lump(ids::LEVEL_INFO)).expect("level info parses") {
        rbsp::lumps::Lump::LevelInfo(i) => i.first_static_prop_index,
        _ => panic!("level info decoded wrong"),
    }
}

fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

/// The cell references with their boxes, and the shadow references.
fn refs(bsp: &Bsp) -> (Vec<(u32, Vec3, Vec3)>, Vec<u32>) {
    let cells = u32s(bsp.lump(ids::OBJ_REFERENCES));
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let boxes = rbsp::lumps::vis::parse_obj_ref_bounds(bsp.lump(ids::OBJ_REFERENCE_BOUNDS), v).expect("ref bounds parse");
    assert_eq!(cells.len(), boxes.len(), "one box per cell reference");
    let cells = cells
        .into_iter()
        .zip(boxes)
        .map(|(r, b)| (r, Vec3::from_array(b.mins), Vec3::from_array(b.maxs)))
        .collect();
    (cells, u32s(bsp.lump(ids::CSM_OBJ_REFERENCES)))
}

/// Every lump the two maps disagree on.
fn changed(a: &Bsp, b: &Bsp) -> BTreeSet<u16> {
    let mut all: BTreeSet<u16> = a.lumps.keys().copied().collect();
    all.extend(b.lumps.keys().copied());
    all.into_iter().filter(|id| a.lump(*id) != b.lump(*id)).collect()
}

/// Both targets of a compile, each with the shipped half it was built from.
fn targets<'a>(out: &'a CompileOutput, client: &'a Bsp, dedi: &'a Bsp) -> [(&'static str, &'a MapArtifacts, &'a Bsp); 2] {
    [
        ("client", out.client.as_ref().expect("a client map"), client),
        ("dedi", out.dedi.as_ref().expect("a server map"), dedi),
    ]
}

fn valid(label: &str, a: &MapArtifacts) {
    let issues = rbsp::validate(&a.bsp, &a.ent_bytes);
    let errors: Vec<String> = issues.iter().filter(|i| i.is_error()).map(|i| i.message.clone()).collect();
    assert!(errors.is_empty(), "{label}: the edited map validates: {}", errors.join(" | "));
}

/// The shipped client with the zero-filled lumps taken from the server, which
/// is the base the compiler starts from.
fn shipped(kit: &std::path::Path) -> (Bsp, Bsp) {
    let (mut client, _) = half(kit, "client");
    let (dedi, _) = half(kit, "dedi");
    rcomp::splice::repair_client(&mut client, &dedi);
    (client, dedi)
}

/// The live collision leaves of a static prop: those a trace can still reach.
fn prop_leaves(bsp: &Bsp, prop: u32) -> Vec<rcomp::collision::DecodedLeaf> {
    decode_leaves(bsp, 0)
        .expect("the world collision decodes")
        .into_iter()
        .filter(|l| l.kind == rbsp::lumps::collision::CHILD_PROP && l.prop_index & 0xFFFF == prop && l.contents != 0)
        .collect()
}

/// A prop with a collision leaf and cell references, so every table a prop
/// edit touches is exercised.
fn busy_prop(bsp: &Bsp) -> u32 {
    let first = first_prop_ref(bsp);
    let (cells, _) = refs(bsp);
    let referenced: BTreeSet<u32> = cells.iter().map(|(r, _, _)| *r).collect();
    let mut with_leaf: Vec<u32> = decode_leaves(bsp, 0)
        .expect("the world collision decodes")
        .into_iter()
        .filter(|l| l.kind == rbsp::lumps::collision::CHILD_PROP && l.contents != 0)
        .map(|l| l.prop_index & 0xFFFF)
        .collect();
    with_leaf.sort_unstable();
    with_leaf.into_iter().find(|p| referenced.contains(&(first + p))).expect("some prop collides and is referenced")
}

#[test]
fn a_moved_prop_is_one_prop_at_its_new_place() {
    let Some(kit) = kit() else { return };
    let (client, dedi) = shipped(&kit);
    let (mut doc, fs) = decompiled(&kit);
    let k = busy_prop(&client);
    let id = rcomp::decompile::promote_prop(&mut doc, rcomp::decompile::prop_source(&client, Some(&dedi)), k).expect("the prop promotes");
    let shift = DVec3::new(256.0, -128.0, 32.0);
    {
        let o = object_mut(&mut doc, id);
        o.hidden = false;
        o.transform.position += shift;
    }
    let out = compile(&doc, &fs, &Assets::new(), &options(&kit)).expect("the edited document compiles");
    for (label, a, base) in targets(&out, &client, &dedi) {
        valid(label, a);
        let before = props(base);
        let after = props(&a.bsp);
        assert_eq!(after.len(), before.len(), "{label}: one copy, in the record's own slot");
        for (i, (x, y)) in before.iter().zip(&after).enumerate() {
            if i as u32 == k {
                let want = (Vec3::from_array(x.origin).as_dvec3() + shift).as_vec3();
                assert!(Vec3::from_array(y.origin).abs_diff_eq(want, 1e-2), "{label}: the prop is at its new place");
                assert_eq!(y.angles, x.angles, "{label}: an unturned prop keeps its angle bits");
                assert_eq!(
                    (y.model_index, y.unknown_0, y.unknown_1, y.unknown_2, y.unknown_3, y.wind_data, y.diffuse_modulation),
                    (x.model_index, x.unknown_0, x.unknown_1, x.unknown_2, x.unknown_3, x.wind_data, x.diffuse_modulation),
                    "{label}: the fields the document does not model keep their shipped values"
                );
            } else {
                assert_eq!(x, y, "{label}: prop {i} is untouched");
            }
        }
        let first = first_prop_ref(base);
        let new_origin = Vec3::from_array(after[k as usize].origin);
        let (cells, _) = refs(&a.bsp);
        let mine: Vec<&(u32, Vec3, Vec3)> = cells.iter().filter(|(r, _, _)| *r == first + k).collect();
        assert!(!mine.is_empty(), "{label}: the prop keeps its cell references");
        for (_, lo, hi) in &mine {
            assert!(new_origin.cmpge(*lo - 1.0).all() && new_origin.cmple(*hi + 1.0).all(), "{label}: its reference box moved with it");
        }
        let leaves = prop_leaves(&a.bsp, k);
        assert!(!leaves.is_empty(), "{label}: the prop still collides");
        for l in &leaves {
            let p = new_origin.as_dvec3();
            assert!(p.cmpge(l.bounds_min - 1.0).all() && p.cmple(l.bounds_max + 1.0).all(), "{label}: the collision tree reaches its new place");
        }
        let moved = changed(base, &a.bsp);
        let owned: BTreeSet<u16> = [
            ids::GAME_LUMP,
            ids::MODELS,
            ids::BVH_NODES,
            ids::CELL_AABB_NODES,
            ids::OBJ_REFERENCE_BOUNDS,
        ]
        .into();
        assert!(moved.is_subset(&owned), "{label}: only what a prop feeds changed: {moved:02x?}");
        assert!(moved.contains(&ids::GAME_LUMP));
    }
}

#[test]
fn a_deleted_prop_is_gone_and_no_index_moves() {
    let Some(kit) = kit() else { return };
    let (client, dedi) = shipped(&kit);
    let (mut doc, fs) = decompiled(&kit);
    let k = busy_prop(&client);
    let id = rcomp::decompile::promote_prop(&mut doc, rcomp::decompile::prop_source(&client, Some(&dedi)), k).expect("the prop promotes");
    doc.objects.retain(|o| o.id != id);
    let out = compile(&doc, &fs, &Assets::new(), &options(&kit)).expect("the edited document compiles");
    for (label, a, base) in targets(&out, &client, &dedi) {
        valid(label, a);
        let first = first_prop_ref(base);
        assert_eq!(props(&a.bsp), props(base), "{label}: every record stays in its slot");
        let (cells_before, shadows_before) = refs(base);
        let (cells, shadows) = refs(&a.bsp);
        let gone = |list: &[u32]| list.iter().filter(|r| **r == first + k).count();
        assert_eq!(gone(&cells.iter().map(|c| c.0).collect::<Vec<_>>()), 0, "{label}: nothing draws it");
        assert_eq!(gone(&shadows), 0, "{label}: nothing shadows with it");
        let was = gone(&cells_before.iter().map(|c| c.0).collect::<Vec<_>>());
        assert_eq!(cells.len(), cells_before.len() - was, "{label}: only its references left the cell tree");
        assert_eq!(shadows.len(), shadows_before.len() - gone(&shadows_before), "{label}: and the shadow tree");
        // Every other reference keeps its value and its box, in order.
        let others_before: Vec<&(u32, Vec3, Vec3)> = cells_before.iter().filter(|c| c.0 != first + k).collect();
        let others: Vec<&(u32, Vec3, Vec3)> = cells.iter().collect();
        assert_eq!(others, others_before, "{label}: the other references are untouched");
        assert!(prop_leaves(&a.bsp, k).is_empty(), "{label}: nothing collides with it");
        assert!(!prop_leaves(&a.bsp, k + 1).is_empty() || prop_leaves(base, k + 1).is_empty(), "{label}: its neighbour still collides");
    }
}

#[test]
fn a_dropped_mesh_group_no_longer_draws_or_collides() {
    let Some(kit) = kit() else { return };
    let (client, dedi) = shipped(&kit);
    let (mut doc, fs) = decompiled(&kit);
    // The group with the fewest meshes whose surface also carries collision.
    let materials = rcomp::splice::mesh_materials(&client).expect("the mesh table reads");
    let mut per: BTreeMap<(u32, String), Vec<u32>> = BTreeMap::new();
    for (i, (m, name)) in materials.iter().enumerate() {
        per.entry((*m, name.clone())).or_default().push(i as u32);
    }
    let ((model, material), meshes) = per
        .into_iter()
        .filter(|((m, name), _)| *m == 0 && !rcomp::splice::surfaces_named(&client, name).is_empty())
        .min_by_key(|(_, v)| v.len())
        .expect("some mesh group collides");
    let record = reference(&doc)
        .sources
        .values()
        .find(|r| matches!(r, RefRecord::Mesh { model: m, material: n } if *m == model && rcomp::splice::material_key(n) == rcomp::splice::material_key(&material)))
        .cloned()
        .expect("the group is a record");
    let id = object_for(&doc, &record);
    doc.objects.retain(|o| o.id != id);
    let out = compile(&doc, &fs, &Assets::new(), &options(&kit)).expect("the edited document compiles");
    let surfaces = rcomp::splice::surfaces_named(&client, &material);
    for (label, a, base) in targets(&out, &client, &dedi) {
        valid(label, a);
        let (cells, _) = refs(&a.bsp);
        assert!(cells.iter().all(|(r, _, _)| !meshes.contains(r)), "{label}: no cell draws the group");
        let live = |bsp: &Bsp| {
            decode_leaves(bsp, 0)
                .expect("the world collision decodes")
                .into_iter()
                .filter(|l| l.contents != 0 && surfaces.contains(&(l.surfprop as u32)))
                .count()
        };
        assert!(live(base) > 0, "{label}: the shipped group collides");
        assert_eq!(live(&a.bsp), 0, "{label}: the dropped group does not");
        let moved = changed(base, &a.bsp);
        let owned: BTreeSet<u16> = [
            ids::CELL_AABB_NODES,
            ids::OBJ_REFERENCES,
            ids::OBJ_REFERENCE_BOUNDS,
            ids::CELL_AABB_SUBTREE_REF_TOTALS,
            ids::BVH_NODES,
            ids::BVH_LEAF_DATA,
            ids::CONTENTS_MASKS,
        ]
        .into();
        assert!(moved.is_subset(&owned), "{label}: only the trees changed: {moved:02x?}");
        assert!(moved.contains(&ids::OBJ_REFERENCES));
        assert_eq!(a.bsp.lump(ids::MESHES), base.lump(ids::MESHES), "{label}: the mesh records stay where they are");
    }
}

fn entity_records(doc: &MapDoc) -> Vec<(Id, u32)> {
    reference(doc)
        .sources
        .iter()
        .filter_map(|(id, r)| match r {
            RefRecord::Entity { index } => Some((*id, *index)),
            _ => None,
        })
        .collect()
}

fn blocks(bytes: &[u8]) -> Vec<rbsp::lumps::entities::Entity> {
    rbsp::lumps::entities::parse(bytes, Version::ClientV51).expect("entity text parses").entities
}

#[test]
fn entities_are_promoted_in_place_deleted_and_added() {
    let Some(kit) = kit() else { return };
    let (client, dedi) = shipped(&kit);
    let (_, client_ents) = half(&kit, "client");
    let (mut doc, fs) = decompiled(&kit);
    // A point entity in the spawn partition: promote and move it. Another one:
    // delete it. Then add an entity of our own.
    let spawn_start = blocks(client.lump(ids::ENTITIES)).len()
        + ["env", "fx", "script", "snd"].iter().map(|p| blocks(&client_ents[*p]).len()).sum::<usize>();
    let spawn = blocks(&client_ents["spawn"]);
    let records = entity_records(&doc);
    let at = |i: usize| records.iter().find(|(_, x)| *x as usize == spawn_start + i).map(|(id, _)| *id).expect("a spawn record");
    let moved_id = at(3);
    let deleted_id = at(5);
    let moved_class = spawn[3].classname().unwrap_or_default().to_string();
    {
        let o = object_mut(&mut doc, moved_id);
        o.hidden = false;
        o.transform.position = DVec3::new(1234.0, -567.0, 89.0);
    }
    doc.objects.retain(|o| o.id != deleted_id);
    let layer = doc.layers[0].id;
    doc.objects.push(Object {
        id: Uuid::new_v4(),
        name: "added_target".into(),
        layer,
        parent: None,
        transform: rdoc::model::Transform { position: DVec3::new(10.0, 20.0, 30.0), ..Default::default() },
        hidden: false,
        locked: false,
        kind: ObjectKind::Entity(Entity {
            classname: "info_target".into(),
            keyvalues: [("targetname".to_string(), "r5ed_added_target".to_string())].into_iter().collect(),
            partition: None,
            links: Default::default(),
        }),
    });
    let out = compile(&doc, &fs, &Assets::new(), &options(&kit)).expect("the edited document compiles");
    let (_, dedi_ents) = half(&kit, "dedi");
    for (label, a, base, ents) in [
        ("client", out.client.as_ref().unwrap(), &client, &client_ents),
        ("dedi", out.dedi.as_ref().unwrap(), &dedi, &dedi_ents),
    ] {
        valid(label, a);
        let got = blocks(&a.ent_bytes["spawn"]);
        let want = blocks(&ents["spawn"]);
        assert_eq!(got.len(), want.len() - 1, "{label}: one spawn block deleted");
        assert_eq!(got[3].classname(), Some(moved_class.as_str()), "{label}: the promoted block kept its place");
        assert_eq!(got[3].get("origin"), Some("1234 -567 89"), "{label}: at its new origin");
        assert_eq!(&got[..3], &want[..3], "{label}: the blocks before it are the shipped text");
        assert_eq!(got[4], want[4], "{label}: the block between them is the shipped text");
        assert_eq!(&got[5..], &want[6..], "{label}: the blocks after the deleted one are the shipped text");
        let added: usize = a
            .ent_bytes
            .values()
            .map(|b| blocks(b).iter().filter(|e| e.get("targetname") == Some("r5ed_added_target")).count())
            .sum::<usize>()
            + blocks(a.bsp.lump(ids::ENTITIES)).iter().filter(|e| e.get("targetname") == Some("r5ed_added_target")).count();
        assert_eq!(added, 1, "{label}: the new entity is written once");
        for p in ["env", "fx", "snd"] {
            assert_eq!(a.ent_bytes[p], ents[p], "{label}: the {p} partition is untouched");
        }
        assert_eq!(a.bsp.lump(ids::GAME_LUMP), base.lump(ids::GAME_LUMP), "{label}: no prop moved");
    }
}

#[test]
fn a_promoted_brush_entity_keeps_its_model_and_collision() {
    let Some(kit) = kit() else { return };
    let (client, dedi) = shipped(&kit);
    let (_, client_ents) = half(&kit, "client");
    let (mut doc, fs) = decompiled(&kit);
    // staging_mu1's brush entities carry their collision as `*coll` blobs in
    // the entity text, over mesh-less model slots.
    let mut all: Vec<rbsp::lumps::entities::Entity> = blocks(client.lump(ids::ENTITIES));
    for p in ["env", "fx", "script", "snd", "spawn"] {
        all.extend(blocks(&client_ents[p]));
    }
    let (index, shipped_block) = all
        .iter()
        .enumerate()
        .find(|(_, b)| b.get("model").is_some_and(|m| m.starts_with('*')) && b.pairs.iter().any(|(k, _)| k.starts_with("*coll")))
        .expect("the map has a brush entity with a collision blob");
    let id = object_for(&doc, &RefRecord::Entity { index: index as u32 });
    object_mut(&mut doc, id).hidden = false;
    let out = compile(&doc, &fs, &Assets::new(), &options(&kit)).expect("the edited document compiles");
    let model = shipped_block.get("model").unwrap().to_string();
    let colls: Vec<&(String, String)> = shipped_block.pairs.iter().filter(|(k, _)| k.starts_with("*coll")).collect();
    for (label, a, base) in targets(&out, &client, &dedi) {
        valid(label, a);
        let mut found: Vec<rbsp::lumps::entities::Entity> =
            blocks(a.bsp.lump(ids::ENTITIES)).into_iter().filter(|b| b.get("model") == Some(model.as_str())).collect();
        for bytes in a.ent_bytes.values() {
            found.extend(blocks(bytes).into_iter().filter(|b| b.get("model") == Some(model.as_str())));
        }
        assert_eq!(found.len(), 1, "{label}: one block names {model}");
        let got: Vec<&(String, String)> = found[0].pairs.iter().filter(|(k, _)| k.starts_with("*coll")).collect();
        assert_eq!(got, colls, "{label}: the promoted entity carries its shipped collision blob");
        assert_eq!(found[0].classname(), shipped_block.classname());
        assert_eq!(a.bsp.lump(ids::MODELS), base.lump(ids::MODELS), "{label}: no model slot was added");
    }
}

#[test]
fn an_added_prop_is_drawn_counted_and_lit() {
    let Some(kit) = kit() else { return };
    let (client, dedi) = shipped(&kit);
    let (mut doc, fs) = decompiled(&kit);
    let model = props(&client)
        .first()
        .and_then(|p| {
            let v = Version::ClientV51;
            let base = client.header.lumps[ids::GAME_LUMP as usize].offset;
            match rbsp::lumps::parse_at(ids::GAME_LUMP, v, client.lump(ids::GAME_LUMP), base).ok()? {
                rbsp::lumps::Lump::GameLump(gl) => gl.static_props()?.models.get(p.model_index as usize).cloned(),
                _ => None,
            }
        })
        .expect("the first prop names a model");
    let at = Vec3::from_array(props(&client)[0].origin).as_dvec3() + DVec3::new(64.0, 0.0, 0.0);
    let layer = doc.layers[0].id;
    doc.objects.push(Object {
        id: Uuid::new_v4(),
        name: "added_prop".into(),
        layer,
        parent: None,
        transform: rdoc::model::Transform { position: at, ..Default::default() },
        hidden: false,
        locked: false,
        kind: ObjectKind::Prop(Prop {
            model: model.clone(),
            skin: 0,
            static_prop: true,
            solid: false,
            cast_shadows: true,
            fade_distance: 0.0,
            keyvalues: Default::default(),
        }),
    });
    let out = compile(&doc, &fs, &Assets::new(), &options(&kit)).expect("the edited document compiles");
    for (label, a, base) in targets(&out, &client, &dedi) {
        valid(label, a);
        let before = props(base);
        let after = props(&a.bsp);
        assert_eq!(after.len(), before.len() + 1, "{label}: the prop is appended");
        assert_eq!(&after[..before.len()], &before[..], "{label}: the shipped records are untouched");
        let new = before.len() as u32;
        let first = first_prop_ref(&a.bsp);
        let (cells, shadows) = refs(&a.bsp);
        assert_eq!(cells.iter().filter(|c| c.0 == first + new).count(), 1, "{label}: one cell draws it");
        assert_eq!(shadows.iter().filter(|r| **r == first + new).count(), 1, "{label}: it casts a shadow");
        let (cells_before, _) = refs(base);
        assert_eq!(cells.len(), cells_before.len() + 1);
        let probes = u32s(a.bsp.lump(ids::STATIC_PROP_LIGHTPROBE_INDICES));
        assert_eq!(probes.len(), after.len(), "{label}: every prop has a probe");
        let v = a.bsp.version_kind().unwrap();
        let rbsp::lumps::Lump::LevelInfo(info) = rbsp::lumps::parse(ids::LEVEL_INFO, v, a.bsp.lump(ids::LEVEL_INFO)).unwrap() else {
            panic!("level info");
        };
        assert_eq!(info.num_static_props as usize, after.len(), "{label}: the level info counts it");
    }
}

/// The live leaves of the world model, as (kind, contents, surface, rounded
/// triangle set), for comparing the shipped collision with the edited one.
type LeafKey = (u32, u32, u16, u32, Vec<[[i64; 3]; 3]>);

fn world_leaves(bsp: &Bsp) -> std::collections::HashSet<LeafKey> {
    decode_leaves(bsp, 0)
        .expect("the world collision decodes")
        .into_iter()
        .filter(|l| l.contents != 0)
        .map(|l| {
            let tris = l
                .triangles
                .iter()
                .map(|t| t.map(|p| [(p.x * 8.0).round() as i64, (p.y * 8.0).round() as i64, (p.z * 8.0).round() as i64]))
                .collect();
            (l.kind, l.contents, l.surfprop, l.prop_index, tris)
        })
        .collect()
}

#[test]
fn an_added_box_draws_and_collides_on_the_shipped_world() {
    let Some(kit) = kit() else { return };
    let (client, dedi) = shipped(&kit);
    let (mut doc, fs) = decompiled(&kit);
    // Next to the first prop, which is somewhere a player can stand.
    let at = Vec3::from_array(props(&client)[0].origin).as_dvec3() + DVec3::new(128.0, 0.0, 0.0);
    let (lo, hi) = (at - DVec3::new(32.0, 32.0, 0.0), at + DVec3::new(32.0, 32.0, 64.0));
    let layer = doc.layers[0].id;
    doc.objects.push(Object {
        id: Uuid::new_v4(),
        name: "added_box".into(),
        layer,
        parent: None,
        transform: Default::default(),
        hidden: false,
        locked: false,
        kind: ObjectKind::Brush(rdoc::brush::box_brush(lo, hi, rdoc::model::DEFAULT_MATERIAL).expect("a box")),
    });
    let out = compile(&doc, &fs, &Assets::new(), &options(&kit)).expect("the edited document compiles");
    for (label, a, base) in targets(&out, &client, &dedi) {
        valid(label, a);
        let v = a.bsp.version_kind().unwrap();
        let before = rbsp::lumps::render::parse_meshes(base.lump(ids::MESHES), v).unwrap().len();
        let runs = rcomp::meshes::decode_meshes(&a.bsp).expect("the edited map's meshes decode");
        assert!(runs.len() > before, "{label}: the box added meshes");
        let shipped_runs = rcomp::meshes::decode_meshes(base).expect("the shipped meshes decode");
        assert_eq!(&runs[..before], &shipped_runs[..], "{label}: every shipped mesh decodes as it did");
        for run in &runs[before..] {
            assert_eq!(run.model, 0, "{label}: the box is world geometry");
            // The map already names the dev material, in its own spelling,
            // and that texdata is the one reused.
            assert_eq!(rcomp::splice::material_key(&run.material), rdoc::model::DEFAULT_MATERIAL, "{label}: the box's material");
            for p in &run.positions {
                let p = p.as_dvec3();
                assert!(p.cmpge(lo - 0.01).all() && p.cmple(hi + 0.01).all(), "{label}: a box vertex at {p} is on the box");
            }
        }
        let new_meshes: BTreeSet<u32> = (before as u32..runs.len() as u32).collect();
        let (cells, _) = refs(&a.bsp);
        for m in &new_meshes {
            assert_eq!(cells.iter().filter(|c| c.0 == *m).count(), 1, "{label}: one cell draws mesh {m}");
        }
        // The shipped collision is all still there, and the box is added.
        let shipped_leaves = world_leaves(base);
        let leaves = world_leaves(&a.bsp);
        let added: Vec<_> = leaves.iter().filter(|l| !shipped_leaves.contains(l)).collect();
        assert!(shipped_leaves.iter().all(|l| leaves.contains(l)), "{label}: every shipped leaf still decodes");
        assert!(!added.is_empty(), "{label}: the box has collision");
        let corners: BTreeSet<[i64; 3]> = added.iter().flat_map(|l| l.4.iter().flatten().copied()).collect();
        for x in [lo.x, hi.x] {
            for y in [lo.y, hi.y] {
                for z in [lo.z, hi.z] {
                    let q = [(x * 8.0).round() as i64, (y * 8.0).round() as i64, (z * 8.0).round() as i64];
                    assert!(corners.iter().any(|c| (0..3).all(|k| (c[k] - q[k]).abs() <= 2)), "{label}: the box's collision reaches its corner {q:?}");
                }
            }
        }
    }
}

#[test]
fn a_moved_mesh_group_is_one_copy_at_its_new_place() {
    let Some(kit) = kit() else { return };
    let (client, dedi) = shipped(&kit);
    let (mut doc, fs) = decompiled(&kit);
    let materials = rcomp::splice::mesh_materials(&client).expect("the mesh table reads");
    let mut per: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (i, (_, name)) in materials.iter().enumerate() {
        per.entry(name.clone()).or_default().push(i as u32);
    }
    let (material, meshes) = per
        .into_iter()
        .filter(|(name, _)| !rcomp::splice::surfaces_named(&client, name).is_empty())
        .min_by_key(|(_, v)| v.len())
        .expect("some mesh group collides");
    let record = RefRecord::Mesh { model: 0, material: material.clone() };
    let id = object_for(&doc, &record);
    let shift = DVec3::new(0.0, 0.0, 512.0);
    {
        let o = object_mut(&mut doc, id);
        o.hidden = false;
        o.transform.position += shift;
    }
    let out = compile(&doc, &fs, &Assets::new(), &options(&kit)).expect("the edited document compiles");
    for (label, a, base) in targets(&out, &client, &dedi) {
        valid(label, a);
        let shipped_runs = rcomp::meshes::decode_meshes(base).unwrap();
        let runs = rcomp::meshes::decode_meshes(&a.bsp).unwrap();
        let before = shipped_runs.len();
        let (cells, _) = refs(&a.bsp);
        assert!(cells.iter().all(|c| !meshes.contains(&c.0)), "{label}: the shipped copy no longer draws");
        let moved: Vec<&rcomp::meshes::DecodedRun> = runs[before..].iter().collect();
        assert!(!moved.is_empty(), "{label}: the promoted group was appended");
        let want: usize = meshes.iter().map(|m| shipped_runs[*m as usize].indices.len() / 3).sum();
        let got: usize = moved.iter().map(|r| r.indices.len() / 3).sum();
        assert_eq!(got, want, "{label}: every triangle of the group came along");
        let lowest = |rs: &[&rcomp::meshes::DecodedRun]| rs.iter().flat_map(|r| r.positions.iter()).map(|p| p.z).fold(f32::INFINITY, f32::min);
        let shipped_group: Vec<&rcomp::meshes::DecodedRun> = meshes.iter().map(|m| &shipped_runs[*m as usize]).collect();
        assert!((lowest(&moved) - lowest(&shipped_group) - shift.z as f32).abs() < 0.5, "{label}: it stands {shift} from where it was");
        for m in before..runs.len() {
            assert_eq!(cells.iter().filter(|c| c.0 == m as u32).count(), 1, "{label}: one cell draws the moved mesh {m}");
        }
    }
}
