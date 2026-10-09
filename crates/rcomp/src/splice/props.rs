//! Static prop splicing: the `sprp` block keeps every record it has, in place.
//!
//! - A promoted prop rewrites its own record. Fields the document does not
//!   model keep the shipped values, and a placement the document did not
//!   change keeps its shipped bits.
//! - A dropped prop keeps its slot, so no index anywhere moves, and leaves the
//!   two reference trees and the collision tree: nothing draws it, nothing
//!   shadows with it, nothing collides with it.
//! - A document prop is appended and put into the cell tree and the shadow
//!   tree.

use std::collections::BTreeSet;

use glam::{DVec3, Vec3};
use rbsp::lumps::game_lump::sprp::{self, StaticProp, StaticProps};
use rbsp::lumps::game_lump::{self, Block, GameLump};
use rbsp::lumps::lighting::PropLightprobeIndex;
use rbsp::{ids, Bsp, Version};
use rdoc::model::{MapDoc, Object, ObjectKind, Prop, Transform};
use rir::AssetTables;

use super::tables::{self, RefTree, Tree};
use super::{Fate, Plan};
use crate::report::Report;
use crate::Error;

fn err(message: impl Into<String>) -> Error {
    Error::Project(message.into())
}

fn version_of(bsp: &Bsp) -> Version {
    bsp.version_kind().unwrap_or(Version::ClientV51)
}

/// The game lump and the index of its prop block.
fn read(bsp: &Bsp) -> Option<(GameLump, usize)> {
    let v = version_of(bsp);
    let base = bsp.header.lumps[ids::GAME_LUMP as usize].offset;
    let gl = game_lump::parse_at(bsp.lump(ids::GAME_LUMP), v, base).ok()?;
    let at = gl.entries.iter().position(|e| e.is_sprp())?;
    Some((gl, at))
}

/// How many static props a map declares.
pub fn prop_count(bsp: &Bsp) -> u32 {
    read(bsp)
        .and_then(|(gl, at)| gl.blocks.get(at).and_then(Block::as_static_props).map(|b| b.prop_count() as u32))
        .unwrap_or(0)
}

/// What the decompiler made of a record, so a promoted prop whose placement
/// nobody changed can be told apart from one that moved.
fn decompiled_transform(p: &StaticProp) -> Transform {
    let mut t = Transform::from_source_angles(p.origin.map(f64::from).into(), p.angles.map(f64::from).into());
    t.scale = DVec3::splat(if p.scale > 0.0 { p.scale as f64 } else { 1.0 });
    t
}

fn same_transform(a: &Transform, b: &Transform) -> bool {
    a.position.abs_diff_eq(b.position, 1e-4)
        && (a.rotation.dot(b.rotation).abs() > 1.0 - 1e-9)
        && a.scale.abs_diff_eq(b.scale, 1e-6)
}

/// Writes a document prop over a record. Returns true when the prop moved.
fn patch(record: &mut StaticProp, block: &mut StaticProps, o: &Object, p: &Prop) -> Result<bool, Error> {
    let was = decompiled_transform(record);
    let moved = !same_transform(&was, &o.transform);
    if moved {
        let angles = o.transform.to_source_angles();
        record.origin = o.transform.position.as_vec3().to_array();
        record.angles = angles.as_vec3().to_array();
        let s = o.transform.scale.x;
        record.scale = if s > 0.0 { s as f32 } else { 1.0 };
    }
    let current = block.models.get(record.model_index as usize).map(String::as_str);
    if current != Some(p.model.as_str()) {
        if p.model.len() >= sprp::MODEL_NAME_SIZE {
            return Err(err(format!("prop {} names a model path longer than the record holds", o.name)));
        }
        let index = match block.models.iter().position(|m| *m == p.model) {
            Some(i) => i,
            None => {
                block.models.push(p.model.clone());
                block.models.len() - 1
            }
        };
        record.model_index = u16::try_from(index).map_err(|_| err("the prop block names more than 65535 models"))?;
    }
    record.unknown_2 = p.skin.clamp(0, u16::MAX as i32) as u16;
    // The record stores "no fade" as a negative distance, and the decompiler
    // reads anything not positive as zero; leave a record that says the same
    // thing as the prop alone.
    if p.fade_distance > 0.0 {
        record.unknown_4 = p.fade_distance;
    } else if record.unknown_4 > 0.0 {
        record.unknown_4 = -1.0;
    }
    crate::props::apply_record_keys(record, &p.keyvalues);
    Ok(moved)
}

/// The world box of a prop moved from `was` to `now`, from the box it had.
///
/// The box is taken back into the prop's own frame through the old placement
/// and out again through the new one, so a turn or a scale is covered as well
/// as a move.
fn moved_box(was: &Transform, now: &Transform, lo: Vec3, hi: Vec3) -> (Vec3, Vec3) {
    let from = glam::DMat4::from_scale_rotation_translation(was.scale, was.rotation, was.position);
    let to = glam::DMat4::from_scale_rotation_translation(now.scale, now.rotation, now.position);
    let m = to * from.inverse();
    let mut a = Vec3::splat(f32::INFINITY);
    let mut b = Vec3::splat(f32::NEG_INFINITY);
    for x in [lo.x, hi.x] {
        for y in [lo.y, hi.y] {
            for z in [lo.z, hi.z] {
                let p = m.transform_point3(DVec3::new(x as f64, y as f64, z as f64)).as_vec3();
                a = a.min(p);
                b = b.max(p);
            }
        }
    }
    (a, b)
}

/// Applies the plan's prop fates and the document's own static props.
pub fn splice(bsp: &mut Bsp, doc: &MapDoc, tables: &AssetTables, plan: &Plan<'_>, report: &mut Report) -> Result<(), Error> {
    let authored: Vec<(&Object, &Prop)> = plan
        .authored
        .iter()
        .filter_map(|o| match &o.kind {
            ObjectKind::Prop(p) if p.static_prop => Some((*o, p)),
            _ => None,
        })
        .collect();
    if plan.props.values().all(Fate::is_keep) && authored.is_empty() {
        return Ok(());
    }
    let _ = doc;
    let v = version_of(bsp);
    let Some((mut gl, at)) = read(bsp) else {
        return Err(err("the reference map has no static prop block to edit"));
    };
    let Some(Block::StaticProps(block)) = gl.blocks.get_mut(at) else {
        return Err(err("the reference map's prop block does not parse"));
    };
    let first_ref = tables::first_prop_ref(bsp)
        .ok_or_else(|| err("the reference map's level info names no first static prop index"))?;
    let count = block.prop_count().min(block.props.len());

    let mut dropped: BTreeSet<u32> = BTreeSet::new();
    let mut moves: Vec<(u32, Transform, Transform)> = Vec::new();
    for (index, fate) in &plan.props {
        let i = *index as usize;
        if i >= count {
            return Err(err(format!("the document names prop record {i} but the reference has {count}")));
        }
        match fate {
            Fate::Keep => {}
            Fate::Drop => {
                dropped.insert(*index);
            }
            Fate::Replace(o) => {
                let ObjectKind::Prop(p) = &o.kind else {
                    return Err(err(format!("object {} stands for prop record {i} but is not a prop", o.name)));
                };
                if !p.static_prop {
                    // A prop made dynamic leaves the record for an entity block.
                    dropped.insert(*index);
                    continue;
                }
                let mut record = block.props[i];
                let was = decompiled_transform(&record);
                if patch(&mut record, block, o, p)? {
                    moves.push((*index, was, o.transform));
                }
                block.props[i] = record;
            }
        }
    }

    // Props the document adds go after the shipped ones.
    let mut added: Vec<(u32, Vec3, Vec3)> = Vec::new();
    let mut leaves: Vec<rir::CollisionSolid> = Vec::new();
    for (o, p) in &authored {
        if p.model.len() >= sprp::MODEL_NAME_SIZE {
            return Err(err(format!("prop {} names a model path longer than the record holds", o.name)));
        }
        let mut record = sprp::default_prop();
        record.unknown_4 = -1.0;
        let index = match block.models.iter().position(|m| *m == p.model) {
            Some(i) => i,
            None => {
                block.models.push(p.model.clone());
                block.models.len() - 1
            }
        };
        record.model_index = u16::try_from(index).map_err(|_| err("the prop block names more than 65535 models"))?;
        // Start from the record the decompiler would read back as this
        // placement, then let the prop's own fields and keys override it.
        let t = o.transform;
        record.origin = t.position.as_vec3().to_array();
        record.angles = t.to_source_angles().as_vec3().to_array();
        record.scale = if t.scale.x > 0.0 { t.scale.x as f32 } else { 1.0 };
        let placed = decompiled_transform(&record);
        let mut probe = record;
        patch(&mut probe, block, &Object { transform: placed, ..(*o).clone() }, p)?;
        record = probe;
        block.props.push(record);
        let model = tables.models.get(&p.model);
        let (lo, hi) = match model {
            Some(m) if m.bounds_min.x <= m.bounds_max.x => (m.bounds_min, m.bounds_max),
            _ => (Vec3::splat(-32.0), Vec3::splat(32.0)),
        };
        let (lo, hi) = moved_box(&Transform::default(), &t, lo, hi);
        let index = block.props.len() as u32 - 1;
        added.push((index, lo, hi));
        if p.solid {
            let source = crate::props::PropSource {
                model: p.model.clone(),
                origin: Vec3::from_array(record.origin),
                angles: Vec3::from_array(record.angles),
                scale: record.scale,
                solid: true,
                bounds_min: model.map(|m| m.bounds_min).unwrap_or(Vec3::ZERO),
                bounds_max: model.map(|m| m.bounds_max).unwrap_or(Vec3::ZERO),
                keys: p.keyvalues.clone(),
                ..Default::default()
            };
            match crate::props::prop_solid(&source, index) {
                Some(s) => leaves.push(s),
                None => report.warn(format!(
                    "prop {} is placed without collision: the asset table has no box for {}",
                    o.name, p.model
                )),
            }
        }
    }
    let total = block.props.len();
    block.counts[0] = total as u32;
    let encoded_len = block.encode().len();
    if let Some(e) = gl.entries.get_mut(at) {
        e.length = encoded_len as i32;
    }
    bsp.set_lump(ids::GAME_LUMP, game_lump::write(&gl, v));
    if !leaves.is_empty() {
        crate::collision::append_world(bsp, &leaves)?;
    }

    // A dropped prop leaves every tree.
    if !dropped.is_empty() {
        let values: BTreeSet<u32> = dropped.iter().map(|i| first_ref + i).collect();
        let (cells, shadows) = tables::remove_obj_refs(bsp, &values);
        let mut leaves = 0;
        for i in &dropped {
            leaves += tables::kill_collision_prop(bsp, *i).map_err(err)?;
        }
        report.warn(format!(
            "{} static prop(s) dropped: {cells} cell references, {shadows} shadow references and {leaves} collision leaves removed; their records stay so no index moves",
            dropped.len()
        ));
    }

    // A moved prop's references and collision follow it.
    if !moves.is_empty() {
        if let Some(mut cells) = RefTree::load(bsp, Tree::Cells) {
            for (i, was, now) in &moves {
                cells.rebox(first_ref + i, |lo, hi| moved_box(was, now, lo, hi));
            }
            cells.store(bsp);
        }
        let mut leaves = 0;
        for (i, was, now) in &moves {
            leaves += tables::grow_collision_prop(bsp, *i, |lo, hi| moved_box(was, now, lo, hi)).map_err(err)?;
        }
        report.warn(format!("{} static prop(s) moved; {leaves} collision leaves follow them", moves.len()));
    }

    // An added prop goes into the cell tree of the cell whose root box holds
    // it, and into the shadow tree's root.
    if !added.is_empty() {
        if first_ref as usize + total > u16::MAX as usize {
            return Err(err("the prop list would reach past what a 16-bit node run can address"));
        }
        if let Some(mut cells) = RefTree::load(bsp, Tree::Cells) {
            for (i, lo, hi) in &added {
                let node = tables::cell_for(&cells, (*lo + *hi) * 0.5);
                cells.add_to(node, &[(first_ref + i, Some((*lo, *hi)))]).map_err(err)?;
            }
            cells.store(bsp);
        }
        if let Some(mut shadows) = RefTree::load(bsp, Tree::Shadows) {
            let root = shadows.roots().first().copied().unwrap_or(0);
            let refs: Vec<(u32, Option<(Vec3, Vec3)>)> = added.iter().map(|(i, _, _)| (first_ref + i, None)).collect();
            shadows.add_to(root, &refs).map_err(err)?;
            shadows.store(bsp);
        }
        // Each prop reads its lighting from one probe: take the probe the
        // nearest shipped prop uses.
        let probes = bsp.lump(ids::STATIC_PROP_LIGHTPROBE_INDICES);
        if !probes.is_empty() {
            let mut list: Vec<PropLightprobeIndex> =
                rbsp::lumps::lighting::parse_prop_lightprobe_indices(probes, v).map_err(|e| err(e.to_string()))?;
            let Some((gl, at)) = read(bsp) else { return Err(err("the prop block went away")) };
            let block = gl.blocks[at].as_static_props().cloned().unwrap_or_default();
            for (i, lo, hi) in &added {
                let centre = (*lo + *hi) * 0.5;
                let nearest = block.props[..count]
                    .iter()
                    .enumerate()
                    .min_by(|a, b| {
                        let da = Vec3::from_array(a.1.origin).distance_squared(centre);
                        let db = Vec3::from_array(b.1.origin).distance_squared(centre);
                        da.total_cmp(&db)
                    })
                    .map(|(k, _)| k);
                let value = nearest.and_then(|k| list.get(k)).map(|p| p.value).unwrap_or(0);
                while list.len() < *i as usize {
                    list.push(PropLightprobeIndex { value: 0 });
                }
                list.push(PropLightprobeIndex { value });
            }
            bsp.set_lump(
                ids::STATIC_PROP_LIGHTPROBE_INDICES,
                rbsp::lumps::lighting::write_prop_lightprobe_indices(&list, v),
            );
        }
        report.warn(format!("{} static prop(s) added after the shipped ones", added.len()));
    }

    // The level info counts the props.
    if let Ok(rbsp::lumps::Lump::LevelInfo(mut info)) = rbsp::lumps::parse(ids::LEVEL_INFO, v, bsp.lump(ids::LEVEL_INFO)) {
        if info.num_static_props as usize != total {
            info.num_static_props = total as u32;
            bsp.set_lump(ids::LEVEL_INFO, rbsp::lumps::level::write_level_info(&info, v));
        }
    }
    Ok(())
}
