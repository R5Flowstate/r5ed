//! Prefabs: a selection saved as one project file and re-instantiated on demand.

use std::collections::HashMap;

use glam::DVec3;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::command::Command;
use crate::doc::{Document, DocError};
use crate::model::{FORMAT_VERSION, Id, Object, ObjectKind, Transform};

pub const PREFAB_DIR: &str = "prefabs";
pub const PREFAB_EXT: &str = "r5prefab";

/// Stand-in parent meaning "attach where the exploded root used to be".
const DROP: Id = Uuid::nil();

/// A saved selection. Object transforms are stored in the prefab's own frame
/// (roots relative to `pivot`, children relative to their parent), so a prefab
/// drops anywhere without carrying the map offset with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prefab {
    pub format: u32,
    pub name: String,
    /// World-space point the stored root transforms are relative to.
    pub pivot: DVec3,
    pub objects: Vec<Object>,
}

impl Prefab {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("Prefab always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DocError> {
        let prefab: Prefab =
            serde_json::from_slice(bytes).map_err(|_| DocError::FileStore("prefab is malformed".into()))?;
        if prefab.format > FORMAT_VERSION {
            return Err(DocError::FileStore(format!("prefab format {} is too new", prefab.format)));
        }
        Ok(prefab)
    }
}

/// Project-relative path of a named prefab.
pub fn prefab_path(name: &str) -> Result<String, DocError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(DocError::BadPath("prefab name is empty".into()));
    }
    if !trimmed.ends_with(PREFAB_EXT) {
        return Err(DocError::BadPath(format!("prefab name must end in {PREFAB_EXT}")));
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(DocError::BadPath("a prefab name has no directory part".into()));
    }
    let path = format!("{PREFAB_DIR}/{trimmed}");
    crate::project::check_path(&path)?;
    Ok(path)
}

fn display_name(name: &str) -> String {
    name.strip_suffix(&format!(".{PREFAB_EXT}")).unwrap_or(name).to_string()
}

/// `child` expressed in `parent`'s frame.
fn relative_to(child: Transform, parent: Transform) -> Transform {
    let inv = parent.rotation.inverse();
    let scale = child.scale / parent.scale;
    Transform {
        position: inv * (child.position - parent.position) / parent.scale,
        rotation: inv * child.rotation,
        scale,
    }
}

/// `child` pushed through one more level of `outer` on the way to world space.
fn compose(outer: Transform, child: Transform) -> Transform {
    Transform {
        position: outer.position + outer.rotation * (outer.scale * child.position),
        rotation: outer.rotation * child.rotation,
        scale: outer.scale * child.scale,
    }
}

fn world_transform(doc: &Document, id: Id) -> Option<Transform> {
    let m = doc.world_matrix(id)?;
    let (_, q, _) = m.to_scale_rotation_translation();
    Some(Transform { position: m.w_axis.truncate(), rotation: q, scale: DVec3::ONE })
}

/// World-space pivot of a selection: the centre of its bounds, or the mean of
/// its object positions when the selection has no geometry.
pub fn selection_pivot(doc: &Document, ids: &[Id]) -> Result<DVec3, DocError> {
    if ids.is_empty() {
        return Err(DocError::EmptySelection);
    }
    for id in ids {
        if doc.object(*id).is_none() {
            return Err(DocError::UnknownObject(*id));
        }
    }
    if let Some(b) = doc.bounds(ids) {
        if !b.is_empty() {
            return Ok(b.center());
        }
    }
    let mut sum = DVec3::ZERO;
    for id in ids {
        sum += doc.world_position(*id).unwrap_or(DVec3::ZERO);
    }
    Ok(sum / ids.len() as f64)
}

/// Command that writes the selection to `prefabs/<name>.r5prefab`.
///
/// The whole subtree of every selected object is stored, so a prefab of a group
/// keeps its children.
pub fn save_selection(doc: &Document, ids: &[Id], name: &str) -> Result<Command, DocError> {
    let path = prefab_path(name)?;
    let pivot = selection_pivot(doc, ids)?;
    let mut keep: Vec<Id> = Vec::new();
    for id in ids {
        for node in doc.subtree(*id) {
            if !keep.contains(&node) {
                keep.push(node);
            }
        }
    }
    let mut worlds: HashMap<Id, Transform> = HashMap::with_capacity(keep.len());
    for id in &keep {
        worlds.insert(*id, world_transform(doc, *id).ok_or(DocError::UnknownObject(*id))?);
    }
    let mut objects: Vec<Object> = Vec::with_capacity(keep.len());
    for id in &keep {
        let src = doc.object(*id).ok_or(DocError::UnknownObject(*id))?;
        let mut out = src.clone();
        let inside = src.parent.map(|p| keep.contains(&p)).unwrap_or(false);
        if inside {
            let parent = match src.parent {
                Some(p) => p,
                None => return Err(DocError::UnknownObject(*id)),
            };
            out.transform = relative_to(worlds[id], worlds[&parent]);
            out.parent = Some(parent);
        } else {
            let mut t = worlds[id];
            t.position -= pivot;
            out.transform = t;
            out.parent = None;
        }
        objects.push(out);
    }
    let prefab = Prefab { format: FORMAT_VERSION, name: name.to_string(), pivot, objects };
    Ok(Command::Batch {
        label: format!("save prefab {name}"),
        commands: vec![Command::SetFile { path, bytes: Some(prefab.to_bytes()) }],
    })
}

/// Fresh copy of a prefab: a `Group` root plus its contents, at local origin.
fn spawn_prefab(doc: &Document, name: &str, layer: Id) -> Result<(Object, Vec<Object>), DocError> {
    let bytes = doc.files.read(&prefab_path(name)?)?;
    let prefab = Prefab::from_bytes(&bytes)?;
    let mut remap: HashMap<Id, Id> = HashMap::with_capacity(prefab.objects.len());
    for o in &prefab.objects {
        remap.insert(o.id, Uuid::new_v4());
    }
    let group = crate::doc::new_object(display_name(name), layer, ObjectKind::Group);
    let group_id = group.id;
    let mut children: Vec<Object> = Vec::with_capacity(prefab.objects.len());
    for src in &prefab.objects {
        let mut copy = src.clone();
        copy.id = remap[&src.id];
        copy.layer = layer;
        copy.parent = Some(match src.parent {
            Some(p) => remap.get(&p).copied().unwrap_or(group_id),
            None => group_id,
        });
        children.push(copy);
    }
    Ok((group, children))
}

/// Command that adds a copy of `prefab` to the document with fresh ids.
///
/// The copy lands under one `Group` at `at`; a nested `PrefabInstance` stays an
/// instance, so a prefab of prefabs does not explode on placement.
pub fn instantiate(doc: &Document, name: &str, at: DVec3, layer: Id) -> Result<Command, DocError> {
    let (group, mut children) = spawn_prefab(doc, name, layer)?;
    let mut root = group;
    root.parent = None;
    // The prefab's contents are local to its own root, so moving the root moves
    // them all.
    root.transform.position = at;
    let mut objects = Vec::with_capacity(children.len() + 1);
    objects.push(root);
    objects.append(&mut children);
    Ok(Command::Batch {
        label: format!("instantiate {name}"),
        commands: vec![Command::AddObjects { objects }],
    })
}

/// Command that replaces an instance (or a group) with editable objects,
/// recursively expanding any nested prefabs.
pub fn explode(doc: &Document, id: Id) -> Result<Command, DocError> {
    let root = doc.object(id).ok_or(DocError::UnknownObject(id))?.clone();
    let outer_parent = root.parent;
    let layer = root.layer;
    let is_instance = matches!(root.kind, ObjectKind::PrefabInstance { .. });

    let mut objects: Vec<Object> = Vec::new();
    let mut remap: HashMap<Id, Id> = HashMap::new();
    for old in doc.subtree(id) {
        let o = doc.object(old).ok_or(DocError::UnknownObject(old))?;
        match &o.kind {
            ObjectKind::PrefabInstance { prefab } => {
                let (group, mut children) = spawn_prefab(doc, prefab, layer)?;
                remap.insert(old, group.id);
                if old == id {
                    let mut head = group;
                    head.name = root.name.clone();
                    objects.push(head);
                } else {
                    objects.push(group);
                }
                objects.append(&mut children);
            }
            _ => {
                let new_id = Uuid::new_v4();
                if old == id {
                    remap.insert(old, DROP);
                    continue;
                }
                remap.insert(old, new_id);
                let mut copy = o.clone();
                copy.id = new_id;
                copy.layer = layer;
                copy.parent = o.parent.and_then(|p| remap.get(&p).copied());
                objects.push(copy);
            }
        }
    }
    // The removed container's own transform is folded into the children that
    // now attach to its parent, so world positions do not move.
    let head_transform = if is_instance { Transform::default() } else { root.transform };
    for o in objects.iter_mut() {
        match o.parent {
            Some(DROP) => {
                o.parent = outer_parent;
                o.transform = compose(head_transform, o.transform);
            }
            None => o.parent = outer_parent,
            _ => {}
        }
    }
    if let Some(first) = objects.first_mut() {
        first.name = root.name.clone();
    }
    Ok(Command::Batch {
        label: format!("explode {}", root.name),
        commands: vec![Command::RemoveObjects { ids: vec![id] }, Command::AddObjects { objects }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::KindFilter;
    use crate::brush::box_brush;
    use crate::model::Entity;

    fn prefab_doc() -> Document {
        let mut d = Document::empty();
        let layer = d.layer_ids()[0];
        let mut g = crate::doc::new_object("room", layer, ObjectKind::Group);
        g.transform.position = DVec3::new(100.0, 0.0, 0.0);
        let g_id = g.id;
        d.apply(Command::AddObjects { objects: vec![g] }).unwrap();
        let mut wall = crate::doc::new_object(
            "wall",
            layer,
            ObjectKind::Brush(box_brush(DVec3::ZERO, DVec3::splat(32.0), "m").unwrap()),
        );
        wall.parent = Some(g_id);
        wall.transform.position = DVec3::new(0.0, 64.0, 0.0);
        d.apply(Command::AddObjects { objects: vec![wall] }).unwrap();
        let mut target = crate::doc::new_object(
            "spawn",
            layer,
            ObjectKind::Entity(Entity { classname: "info_target".into(), keyvalues: Default::default(), partition: None, links: Default::default() }),
        );
        target.transform.position = DVec3::new(100.0, 0.0, 0.0);
        d.apply(Command::AddObjects { objects: vec![target] }).unwrap();
        d
    }

    fn roots(d: &Document) -> Vec<Id> {
        d.doc.objects.iter().filter(|o| o.parent.is_none()).map(|o| o.id).collect()
    }

    #[test]
    fn prefab_roundtrip() {
        let mut d = prefab_doc();
        let selection = roots(&d);
        let before = crate::project::serialize(&d.doc);
        d.apply(save_selection(&d, &selection, "room.r5prefab").unwrap()).unwrap();
        assert!(d.files.exists("prefabs/room.r5prefab"));

        let layer = d.layer_ids()[0];
        let pivot = selection_pivot(&d, &selection).unwrap();
        assert_eq!(pivot, DVec3::new(100.0, 0.0, 0.0));
        let cmd = instantiate(&d, "room.r5prefab", pivot, layer).unwrap();
        let (inverse, ids) = d.apply(cmd).unwrap();
        assert_eq!(ids.len(), 4, "instance root, wall, target and the original room");
        let instance = d.object(ids[0]).expect("instance root");
        assert_eq!(instance.transform.position, pivot);
        let placed = d.descendants(instance.id);
        assert_eq!(placed.len(), 3, "the prefab's own group plus the wall and the target");
        let copied = |name: &str| {
            placed
                .iter()
                .copied()
                .find(|id| d.object(*id).map(|o| o.name.as_str()) == Some(name))
                .unwrap_or_else(|| panic!("the copied {name}"))
        };
        assert_eq!(d.world_position(copied("wall")), Some(DVec3::new(100.0, 64.0, 0.0)));
        assert_eq!(d.world_position(copied("spawn")), Some(DVec3::new(100.0, 0.0, 0.0)));

        d.apply(inverse).unwrap();
        assert_eq!(before, crate::project::serialize(&d.doc));
    }

    #[test]
    fn explode_turns_an_instance_into_editable_objects() {
        let mut d = prefab_doc();
        let layer = d.layer_ids()[0];
        d.apply(save_selection(&d, &[d.object_ids()[0]], "one.r5prefab").unwrap()).unwrap();
        d.apply(instantiate(&d, "one.r5prefab", DVec3::new(200.0, 0.0, 0.0), layer).unwrap()).unwrap();
        let instance = roots(&d)
            .into_iter()
            .find(|id| d.object(*id).unwrap().transform.position == DVec3::new(200.0, 0.0, 0.0))
            .expect("placed instance");
        let before = d.doc.objects.len();
        d.apply(explode(&d, instance).unwrap()).unwrap();
        assert!(d.object(instance).is_none());
        assert_eq!(d.doc.objects.len(), before - 1, "the instance group is replaced by its contents");
        assert!(d.doc.objects.iter().any(|o| matches!(o.kind, ObjectKind::Brush(_))));
        let mut walls: Vec<DVec3> = d
            .kind_objects(KindFilter::Brush)
            .into_iter()
            .filter_map(|id| d.world_position(id))
            .collect();
        walls.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(walls, vec![DVec3::new(100.0, 64.0, 0.0), DVec3::new(200.0, 64.0, 0.0)], "explode keeps world positions");
        d.undo().unwrap();
        assert!(d.object(instance).is_some());
    }

    #[test]
    fn nested_prefabs_stay_instances_until_exploded() {
        let mut d = prefab_doc();
        let layer = d.layer_ids()[0];
        d.apply(save_selection(&d, &[d.object_ids()[0]], "inner.r5prefab").unwrap()).unwrap();
        let mut nested = crate::doc::new_object("nested", layer, ObjectKind::PrefabInstance {
            prefab: "inner.r5prefab".into(),
        });
        nested.transform.position = DVec3::new(10.0, 0.0, 0.0);
        d.apply(Command::AddObjects { objects: vec![nested] }).unwrap();
        let nested_id = roots(&d)
            .into_iter()
            .find(|id| matches!(d.object(*id).unwrap().kind, ObjectKind::PrefabInstance { .. }))
            .unwrap();
        d.apply(save_selection(&d, &[nested_id], "outer.r5prefab").unwrap()).unwrap();
        d.apply(instantiate(&d, "outer.r5prefab", DVec3::new(900.0, 0.0, 0.0), layer).unwrap()).unwrap();
        let instances = |d: &Document| {
            d.doc.objects.iter().filter(|o| matches!(o.kind, ObjectKind::PrefabInstance { .. })).count()
        };
        assert_eq!(instances(&d), 2, "the outer copy keeps its nested instance");
        let placed = roots(&d)
            .into_iter()
            .find(|id| d.object(*id).unwrap().transform.position == DVec3::new(900.0, 0.0, 0.0))
            .expect("placed outer instance");
        d.apply(explode(&d, placed).unwrap()).unwrap();
        assert_eq!(instances(&d), 1);
        assert!(d.doc.objects.iter().filter(|o| matches!(o.kind, ObjectKind::Brush(_))).count() >= 2);
    }

    #[test]
    fn prefab_names_are_validated() {
        let d = prefab_doc();
        assert!(matches!(prefab_path(""), Err(DocError::BadPath(_))));
        assert!(matches!(prefab_path("room.txt"), Err(DocError::BadPath(_))));
        assert!(matches!(prefab_path("../room.r5prefab"), Err(DocError::BadPath(_))));
        assert!(matches!(prefab_path("a/b.r5prefab"), Err(DocError::BadPath(_))));
        assert_eq!(prefab_path("room.r5prefab").unwrap(), "prefabs/room.r5prefab");
        let missing = instantiate(&d, "nope.r5prefab", DVec3::ZERO, d.layer_ids()[0]);
        assert!(matches!(missing, Err(DocError::FileMissing(_))));
    }

    #[test]
    fn pivot_uses_selection_bounds() {
        let d = prefab_doc();
        assert_eq!(selection_pivot(&d, &roots(&d)).unwrap(), DVec3::new(100.0, 0.0, 0.0));
        let brush = d.doc.objects.iter().find(|o| matches!(o.kind, ObjectKind::Brush(_))).map(|o| o.id).unwrap();
        assert_eq!(selection_pivot(&d, &[brush]).unwrap(), DVec3::new(116.0, 80.0, 16.0));
        assert!(matches!(selection_pivot(&d, &[]), Err(DocError::EmptySelection)));
        assert!(matches!(selection_pivot(&d, &[Uuid::new_v4()]), Err(DocError::UnknownObject(_))));
    }

    #[test]
    fn entity_objects_survive_the_prefab_round_trip() {
        let mut d = prefab_doc();
        let target = d
            .doc
            .objects
            .iter()
            .find(|o| matches!(o.kind, ObjectKind::Entity(_)))
            .map(|o| o.id)
            .unwrap();
        d.apply(save_selection(&d, &[target], "spot.r5prefab").unwrap()).unwrap();
        let raw = d.files.read("prefabs/spot.r5prefab").unwrap();
        let prefab = Prefab::from_bytes(&raw).unwrap();
        assert_eq!(prefab.objects.len(), 1);
        assert_eq!(prefab.objects[0].id, target);
        assert!(matches!(prefab.objects[0].kind, ObjectKind::Entity(_)));
    }
}

