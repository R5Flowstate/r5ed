//! Entity splicing: the reference's blocks keep their text, a promoted object's
//! block takes the place of the one it came from, and the document's own
//! entities are appended to the file their class lives in.

use std::collections::{BTreeMap, HashMap};

use rbsp::lumps::entities::{self as ent_text, Entity as Block, EntityText, TextHeader};
use rbsp::{ids, Bsp, Version};
use rdoc::model::{Id, MapDoc, Object, ObjectKind, Partition};
use rdoc::schema::EntitySchema;

use super::{Fate, Plan};
use crate::collision::CollTarget;
use crate::entities::{self, Built, Context};
use crate::report::Report;
use crate::Error;

/// One entity file of a target: the main lump or a partition.
struct File {
    part: Partition,
    text: EntityText,
    /// The blocks after the splice, or `None` while nothing touched the file.
    blocks: Option<Vec<Block>>,
}

impl File {
    fn edit(&mut self) -> &mut Vec<Block> {
        if self.blocks.is_none() {
            self.blocks = Some(self.text.entities.clone());
        }
        self.blocks.as_mut().expect("just filled")
    }
}

fn err(message: impl Into<String>) -> Error {
    Error::Project(message.into())
}

/// Applies the plan's entity fates and the document's own entities to one
/// target's entity lump and partition files.
#[allow(clippy::too_many_arguments)]
pub fn splice(
    bsp: &mut Bsp,
    ent_bytes: &mut BTreeMap<String, Vec<u8>>,
    doc: &MapDoc,
    schema: &EntitySchema,
    plan: &Plan<'_>,
    version: Version,
    added: Option<(glam::Vec3, glam::Vec3)>,
    report: &mut Report,
) -> Result<(), Error> {
    let world = world_box(bsp, version);
    let mut files: Vec<File> = Vec::new();
    let main = ent_text::parse(bsp.lump(ids::ENTITIES), version).map_err(|e| err(format!("entity lump: {e}")))?;
    files.push(File { part: Partition::Main, text: main, blocks: None });
    for p in Partition::FILES {
        let Some(bytes) = ent_bytes.get(p.name()) else { continue };
        let text = ent_text::parse(bytes, version).map_err(|e| err(format!("{} partition: {e}", p.name())))?;
        files.push(File { part: p, text, blocks: None });
    }
    // The record index runs over the files in this order, the order the
    // decompiler read them in.
    let mut starts: Vec<usize> = Vec::with_capacity(files.len());
    let mut total = 0usize;
    for f in &files {
        starts.push(total);
        total += f.text.entities.len();
    }
    let file_count = files.len();
    let locate = |index: u32| -> Option<(usize, usize)> {
        let i = index as usize;
        (0..file_count).rev().find(|k| starts[*k] <= i).filter(|_| i < total).map(|k| (k, i - starts[k]))
    };

    let worlds = crate::world_transforms(doc);
    let model_count = bsp.lump(ids::MODELS).len() / rbsp::lumps::collision::MODEL_SIZE;
    let mut next_model = model_count as u32;
    let targetnames = targetnames(doc);
    let target = if version == Version::ClientV51 { CollTarget::Client } else { CollTarget::Dedi };

    // Blocks are removed after every replacement has been placed, so an index
    // keeps naming the block it named in the shipped file.
    let mut removed: Vec<(usize, usize)> = Vec::new();
    let mut appended: Vec<(Partition, Block)> = Vec::new();
    for (index, fate) in &plan.entities {
        let Some((k, at)) = locate(*index) else {
            return Err(err(format!(
                "the document names entity record {index} but the reference carries {total} blocks"
            )));
        };
        match fate {
            Fate::Keep => {}
            Fate::Drop => removed.push((k, at)),
            Fate::Replace(o) => {
                // A promoted brush entity keeps the model slot it had.
                let own_model = model_key(&files[k].text.entities[at]);
                let built = object_blocks(o, doc, schema, &worlds, &targetnames, own_model, target)?;
                let mut blocks = built.into_iter();
                match (blocks.next(), blocks.len()) {
                    (Some((_, b)), 0) => files[k].edit()[at] = b,
                    (first, _) => {
                        removed.push((k, at));
                        appended.extend(first);
                        appended.extend(blocks);
                    }
                }
            }
        }
    }
    removed.sort_unstable();
    for (k, at) in removed.into_iter().rev() {
        files[k].edit().remove(at);
    }

    // Every map opens with a worldspawn block. When the shipped one is gone and
    // nothing took its place, the compiler writes one from the world model's
    // box. When it stays, its world box grows to hold the geometry the document
    // added outside it; the shipped box is not the world model's (staging_mu1's
    // is well inside it), so only what was added moves it.
    let has_world = |f: &File| f.blocks.as_ref().unwrap_or(&f.text.entities).iter().any(|b| b.classname() == Some("worldspawn"));
    if !has_world(&files[0]) {
        if let Some(bounds) = world {
            files[0].edit().insert(0, entities::worldspawn(bounds, &doc.settings));
            report.warn("the reference's worldspawn block was dropped; the compiler wrote one from the world's bounds");
        }
    } else if let Some((lo, hi)) = added {
        let at = files[0].blocks.as_ref().unwrap_or(&files[0].text.entities).iter().position(|b| b.classname() == Some("worldspawn"));
        if let Some(at) = at {
            let b = &files[0].blocks.as_ref().unwrap_or(&files[0].text.entities)[at];
            let own_lo = b.get("world_mins").and_then(crate::decompile::entity_vec3);
            let own_hi = b.get("world_maxs").and_then(crate::decompile::entity_vec3);
            if let (Some(a), Some(z)) = (own_lo, own_hi) {
                let (a, z) = (a.as_vec3(), z.as_vec3());
                if lo.cmplt(a).any() || hi.cmpgt(z).any() {
                    let (nlo, nhi) = (a.min(lo), z.max(hi));
                    let block = &mut files[0].edit()[at];
                    for (k, v) in block.pairs.iter_mut() {
                        match k.as_str() {
                            "world_mins" => *v = entities::vec3_text(nlo.as_dvec3()),
                            "world_maxs" => *v = entities::vec3_text(nhi.as_dvec3()),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    for o in &plan.authored {
        if !compiles_to_entities(o) {
            continue;
        }
        let model = match &o.kind {
            ObjectKind::Volume(_) => {
                next_model += 1;
                Some(next_model - 1)
            }
            ObjectKind::Entity(_) if has_brush_children(o, doc) => {
                next_model += 1;
                Some(next_model - 1)
            }
            _ => None,
        };
        appended.extend(object_blocks(o, doc, schema, &worlds, &targetnames, model, target)?);
    }
    if next_model as usize > model_count {
        crate::pad_models(bsp, version, next_model as usize);
    }

    for (part, b) in appended {
        let k = match files.iter().position(|f| f.part == part) {
            Some(k) => k,
            None => {
                // A file the shipped map does not have: take the header the
                // other partitions carry.
                let header = files.iter().find_map(|f| f.text.header.clone()).unwrap_or(TextHeader {
                    ident: "ENTITIES02".into(),
                    num_models: 1,
                });
                files.push(File {
                    part,
                    text: EntityText {
                        header: Some(header),
                        entities: Vec::new(),
                        tail: ent_text::AFTER_LAST_BLOCK.to_vec(),
                    },
                    blocks: None,
                });
                files.len() - 1
            }
        };
        files[k].edit().push(b);
    }

    let mut changed = 0usize;
    for f in files {
        let Some(blocks) = f.blocks else { continue };
        changed += 1;
        let text = EntityText { header: f.text.header.clone(), entities: blocks, tail: f.text.tail.clone() };
        let bytes = ent_text::write(&text, version);
        if f.part == Partition::Main {
            bsp.set_lump(ids::ENTITIES, bytes);
        } else {
            let new_file = !ent_bytes.contains_key(f.part.name());
            ent_bytes.insert(f.part.name().to_string(), bytes);
            if new_file {
                let names: Vec<String> =
                    Partition::FILES.iter().filter(|p| ent_bytes.contains_key(p.name())).map(|p| p.name().to_string()).collect();
                crate::emit::emit_partitions(&names, bsp, version);
            }
        }
    }
    if changed > 0 {
        report.warn(format!("{changed} entity file(s) of the reference were rewritten"));
    }
    Ok(())
}

/// The world model's box, which the worldspawn block's world keys describe.
fn world_box(bsp: &Bsp, version: Version) -> Option<(glam::Vec3, glam::Vec3)> {
    let models = rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), version).ok()?;
    let m = models.first()?;
    (m.mins[0] <= m.maxs[0]).then(|| (glam::Vec3::from_array(m.mins), glam::Vec3::from_array(m.maxs)))
}

/// Objects the entity writer places: everything but world geometry and static
/// props, which have lumps of their own.
fn compiles_to_entities(o: &Object) -> bool {
    match &o.kind {
        ObjectKind::Entity(_) | ObjectKind::Volume(_) | ObjectKind::Light(_) | ObjectKind::Path(_) => true,
        ObjectKind::Prop(p) => !p.static_prop,
        _ => false,
    }
}

fn has_brush_children(o: &Object, doc: &MapDoc) -> bool {
    doc.objects.iter().any(|c| c.parent == Some(o.id) && matches!(c.kind, ObjectKind::Brush(_)))
}

/// The model index a block's `model "*N"` key names.
fn model_key(b: &Block) -> Option<u32> {
    b.get("model")?.strip_prefix('*')?.parse().ok()
}

fn targetnames(doc: &MapDoc) -> HashMap<Id, String> {
    let mut out = HashMap::new();
    for o in &doc.objects {
        if let ObjectKind::Entity(e) = &o.kind {
            if let Some(n) = e.keyvalues.get("targetname").or_else(|| e.keyvalues.get("script_name")) {
                out.insert(o.id, n.clone());
            }
        }
    }
    out
}

/// The blocks one object compiles to, each with the file it goes in.
///
/// A plain entity keeps every key it carries, `model` and the `*coll` blob
/// included: the model slot it names is the reference's own and is not
/// rebuilt. Anything else goes through the entity writer on its own.
fn object_blocks(
    o: &Object,
    doc: &MapDoc,
    schema: &EntitySchema,
    worlds: &crate::csg::WorldMap,
    targetnames: &HashMap<Id, String>,
    model: Option<u32>,
    target: CollTarget,
) -> Result<Vec<(Partition, Block)>, Error> {
    if let ObjectKind::Entity(e) = &o.kind {
        if !has_brush_children(o, doc) {
            let mut pairs: Vec<(String, String)> = vec![("classname".into(), e.classname.clone())];
            pairs.push(("origin".into(), entities::vec3_text(o.transform.position)));
            let angles = o.transform.to_source_angles();
            if angles.length() > 0.0 {
                pairs.push(("angles".into(), entities::angles_text(angles)));
            }
            for (k, v) in &e.keyvalues {
                if k == "classname" || k == "origin" || k == "angles" {
                    continue;
                }
                pairs.push((k.clone(), v.clone()));
            }
            for (key, linked) in &e.links {
                if let Some(name) = targetnames.get(linked) {
                    pairs.retain(|(k, _)| k != key);
                    pairs.push((key.clone(), name.clone()));
                }
            }
            let part = e.partition.or_else(|| schema.home_partition(&e.classname)).unwrap_or(Partition::Main);
            return Ok(vec![(part, Block { pairs })]);
        }
    }
    // The writer reads a document; this one holds the object and whatever it
    // owns, so the blocks it returns are the object's alone.
    let mut objects: Vec<Object> = vec![o.clone()];
    objects.extend(doc.objects.iter().filter(|c| c.parent == Some(o.id)).cloned());
    let sub = MapDoc {
        format: doc.format,
        meta: doc.meta.clone(),
        layers: doc.layers.clone(),
        objects,
        settings: doc.settings.clone(),
    };
    let mut model_of: HashMap<Id, u32> = HashMap::new();
    if let Some(m) = model {
        model_of.insert(o.id, m);
    }
    let ctx = Context {
        doc: &sub,
        schema,
        model_of: &model_of,
        worlds,
        models: &[],
        surfaces: &[],
        target,
        world_bounds: (glam::Vec3::ZERO, glam::Vec3::ZERO),
    };
    let built: Built = entities::build(&ctx)?;
    let mut out: Vec<(Partition, Block)> = built.main.iter().skip(1).cloned().map(|b| (Partition::Main, b)).collect();
    for p in Partition::FILES {
        out.extend(built.partition(p).iter().cloned().map(|b| (p, b)));
    }
    Ok(out)
}
