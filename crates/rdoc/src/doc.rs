//! The editing document: command application, undo history and scene queries.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use glam::{DMat3, DMat4, DVec3};
use thiserror::Error;
use uuid::Uuid;

use crate::brush::BrushError;
use crate::command::Command;
use crate::model::{Brush, Id, KeyValues, MapDoc, Object, ObjectKind, Partition};

/// Deepest nesting accepted inside a `Command::Batch`.
const MAX_BATCH_DEPTH: u32 = 64;

#[derive(Debug, Error)]
pub enum DocError {
    #[error("object {0} does not exist")]
    UnknownObject(Id),
    #[error("object id {0} is used more than once")]
    DuplicateId(Id),
    #[error("object {0} is parented to itself")]
    SelfParent(Id),
    #[error("object {child} is an ancestor of its own parent {parent}")]
    ParentCycle { child: Id, parent: Id },
    #[error("layer {0} does not exist")]
    UnknownLayer(Id),
    #[error("layer id {0} is used more than once")]
    DuplicateLayer(Id),
    #[error("a document needs at least one layer")]
    NoLayers,
    #[error("layer {0} still holds objects")]
    LayerNotEmpty(Id),
    #[error("there is nothing to undo")]
    NothingToUndo,
    #[error("there is nothing to redo")]
    NothingToRedo,
    #[error("no objects were selected")]
    EmptySelection,
    #[error("object {0} is not a brush")]
    NotABrush(Id),
    #[error("prefab {0} is not in the project")]
    MissingPrefab(String),
    #[error("file path is not allowed: {0}")]
    BadPath(String),
    #[error("project file {0} could not be read")]
    FileMissing(String),
    #[error("file store: {0}")]
    FileStore(String),
    #[error("command nesting is too deep")]
    TooDeep,
    #[error(transparent)]
    Brush(#[from] BrushError),
}

impl From<crate::project::ProjectError> for DocError {
    fn from(e: crate::project::ProjectError) -> Self {
        match e {
            crate::project::ProjectError::BadPath(p) => DocError::BadPath(p),
            crate::project::ProjectError::Io(m) => DocError::FileStore(m),
            other => DocError::FileStore(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub size: usize,
}

/// Project-relative binary blobs: terrain heights, splat weights, meshes, prefabs.
pub trait FileStore {
    fn read(&self, path: &str) -> Result<Vec<u8>, DocError>;
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), DocError>;
    fn remove(&mut self, path: &str) -> Result<(), DocError>;
    fn exists(&self, path: &str) -> bool;
    fn list(&self) -> Result<Vec<FileEntry>, DocError>;
}

/// In-memory store, used by the browser editor and by every test.
#[derive(Clone, Debug, Default)]
pub struct MemoryFileStore {
    files: BTreeMap<String, Vec<u8>>,
}

impl MemoryFileStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn paths(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }
}

impl FileStore for MemoryFileStore {
    fn read(&self, path: &str) -> Result<Vec<u8>, DocError> {
        crate::project::check_path(path)?;
        self.files.get(path).cloned().ok_or_else(|| DocError::FileMissing(path.to_string()))
    }

    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), DocError> {
        crate::project::check_path(path)?;
        self.files.insert(path.to_string(), bytes.to_vec());
        Ok(())
    }

    fn remove(&mut self, path: &str) -> Result<(), DocError> {
        crate::project::check_path(path)?;
        self.files.remove(path);
        Ok(())
    }

    fn exists(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    fn list(&self) -> Result<Vec<FileEntry>, DocError> {
        Ok(self.files.iter().map(|(p, b)| FileEntry { path: p.clone(), size: b.len() }).collect())
    }
}

/// Axis-aligned box in world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: DVec3,
    pub max: DVec3,
}

impl Bounds {
    pub fn empty() -> Self {
        Self { min: DVec3::splat(f64::INFINITY), max: DVec3::splat(f64::NEG_INFINITY) }
    }

    pub fn from_points(points: impl IntoIterator<Item = DVec3>) -> Option<Self> {
        let mut b = Self::empty();
        let mut any = false;
        for p in points {
            b.add_point(p);
            any = true;
        }
        if any {
            Some(b)
        } else {
            None
        }
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x
    }

    pub fn add_point(&mut self, p: DVec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, other: &Bounds) -> Bounds {
        Bounds { min: self.min.min(other.min), max: self.max.max(other.max) }
    }

    pub fn size(&self) -> DVec3 {
        if self.is_empty() {
            DVec3::ZERO
        } else {
            self.max - self.min
        }
    }

    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    pub fn grow(&self, by: f64) -> Bounds {
        if self.is_empty() {
            return *self;
        }
        Bounds { min: self.min - DVec3::splat(by), max: self.max + DVec3::splat(by) }
    }
}

/// Which `ObjectKind` variants a query returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KindFilter {
    All,
    Brush,
    /// Entity objects that own at least one child brush.
    BrushEntity,
    /// Entity objects with no child brush.
    PointEntity,
    Volume,
    Path,
    Light,
    Prop,
    Mesh,
    Terrain,
    Group,
    PrefabInstance,
    Reference,
}

impl KindFilter {
    fn matches(self, k: &ObjectKind) -> Option<bool> {
        Some(match self {
            KindFilter::All => true,
            KindFilter::Brush => matches!(k, ObjectKind::Brush(_)),
            KindFilter::BrushEntity | KindFilter::PointEntity => return None,
            KindFilter::Volume => matches!(k, ObjectKind::Volume(_)),
            KindFilter::Path => matches!(k, ObjectKind::Path(_)),
            KindFilter::Light => matches!(k, ObjectKind::Light(_)),
            KindFilter::Prop => matches!(k, ObjectKind::Prop(_)),
            KindFilter::Mesh => matches!(k, ObjectKind::Mesh(_)),
            KindFilter::Terrain => matches!(k, ObjectKind::Terrain(_) | ObjectKind::VoxelTerrain(_)),
            KindFilter::Group => matches!(k, ObjectKind::Group),
            KindFilter::PrefabInstance => matches!(k, ObjectKind::PrefabInstance { .. }),
            KindFilter::Reference => matches!(k, ObjectKind::Reference(_)),
        })
    }
}

struct UndoEntry {
    command: Command,
    inverse: Command,
    /// Object ids in the order the document held them before the command ran, so
    /// undo and redo restore the draw order as well as the values.
    order: Vec<Id>,
}

/// Consecutive `ReplaceObjects` on the same objects collapse into one undo step.
#[derive(Default)]
struct Gesture {
    start: usize,
    /// Object state before the gesture first replaced each of these ids.
    pre: HashMap<Id, Object>,
    /// Ids already replaced by a `ReplaceObjects` inside this gesture.
    replaced: BTreeSet<Id>,
}

/// The document plus its file blobs, with a linear undo history.
pub struct Document {
    pub doc: MapDoc,
    pub files: Box<dyn FileStore>,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    gesture: Option<Gesture>,
}

impl Document {
    pub fn new(doc: MapDoc, files: Box<dyn FileStore>) -> Self {
        Self { doc, files, undo: Vec::new(), redo: Vec::new(), gesture: None }
    }

    pub fn empty() -> Self {
        Self::new(MapDoc::default(), Box::new(MemoryFileStore::new()))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    pub fn layer_ids(&self) -> Vec<Id> {
        self.doc.layers.iter().map(|l| l.id).collect()
    }

    /// Opens a coalescing gesture. Nested calls are ignored.
    pub fn begin_gesture(&mut self) {
        if self.gesture.is_some() {
            return;
        }
        self.gesture = Some(Gesture { start: self.undo.len(), ..Default::default() });
    }

    /// Closes the gesture, folding every `ReplaceObjects` it recorded into one
    /// entry, so a multi-object drag is a single undo step.
    pub fn end_gesture(&mut self) {
        let Some(g) = self.gesture.take() else { return };
        if g.replaced.is_empty() {
            return;
        }
        let run: Vec<usize> = (g.start..self.undo.len())
            .filter(|i| matches!(&self.undo[*i].command, Command::ReplaceObjects { .. }))
            .collect();
        if run.len() < 2 {
            return;
        }
        let (Some(&first), Some(&last)) = (run.first(), run.last()) else {
            return;
        };
        let command = self.undo[last].command.clone();
        let pre: Vec<Object> = g.replaced.iter().filter_map(|id| g.pre.get(id).cloned()).collect();
        if pre.is_empty() {
            return;
        }
        self.undo[first] = UndoEntry {
            command,
            inverse: Command::ReplaceObjects { objects: pre },
            order: self.undo[last].order.clone(),
        };
        for i in run.iter().skip(1).rev() {
            self.undo.remove(*i);
        }
    }

    /// Applies a command atomically and returns its inverse plus the ids it touched.
    pub fn apply(&mut self, cmd: Command) -> Result<(Command, Vec<Id>), DocError> {
        let order = self.object_ids();
        let (inverse, ids) = self.run(cmd.clone())?;
        if self.coalesce(&cmd, &inverse) {
            return Ok((inverse, ids));
        }
        self.undo.push(UndoEntry { command: cmd, inverse: inverse.clone(), order });
        self.redo.clear();
        Ok((inverse, ids))
    }

    /// True when the command was folded into the gesture's pending `ReplaceObjects` run.
    fn coalesce(&mut self, cmd: &Command, inverse: &Command) -> bool {
        let (Some(g), Command::ReplaceObjects { objects }, Command::ReplaceObjects { objects: previous }) =
            (self.gesture.as_mut(), cmd, inverse)
        else {
            return false;
        };
        let ids: Vec<Id> = objects.iter().map(|o| o.id).collect();
        if ids.is_empty() {
            return false;
        }
        let first_time = !ids.iter().all(|id| g.replaced.contains(id));
        for id in &ids {
            g.replaced.insert(*id);
        }
        if first_time {
            // The pre-state is captured once per object, at the first replace,
            // so undo returns the document to where the gesture started.
            for o in previous {
                g.pre.insert(o.id, o.clone());
            }
        }
        !first_time
    }

    pub fn undo(&mut self) -> Result<Command, DocError> {
        let Some(entry) = self.undo.pop() else {
            return Err(DocError::NothingToUndo);
        };
        let before = self.object_ids();
        let (redo_inverse, _) = self.run(entry.inverse)?;
        self.restore_order(&entry.order);
        self.redo.push(UndoEntry { command: entry.command.clone(), inverse: redo_inverse, order: before });
        Ok(entry.command)
    }

    pub fn redo(&mut self) -> Result<Command, DocError> {
        let Some(entry) = self.redo.pop() else {
            return Err(DocError::NothingToRedo);
        };
        let before = self.object_ids();
        let (undo_inverse, _) = self.run(entry.command.clone())?;
        self.restore_order(&entry.order);
        self.undo.push(UndoEntry { command: entry.command.clone(), inverse: undo_inverse, order: before });
        Ok(entry.command)
    }

    /// Sorts the object list back into a recorded order. Ids the order does not
    /// mention keep their relative position at the end, which is where an
    /// `AddObjects` puts them.
    fn restore_order(&mut self, order: &[Id]) {
        let rank: HashMap<Id, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let missing = order.len();
        self.doc.objects.sort_by_key(|o| rank.get(&o.id).copied().unwrap_or(missing));
    }

    /// Applies a command to a copy of the document and to the file store, and only
    /// commits once both have succeeded.
    fn run(&mut self, cmd: Command) -> Result<(Command, Vec<Id>), DocError> {
        let mut next = self.doc.clone();
        let (inverse, ids) = execute(&mut next, &cmd, self.files.as_ref(), 0)?;
        validate(&next)?;

        let ops = collect_files(&cmd);
        let mut saved: Vec<(String, Option<Vec<u8>>)> = Vec::with_capacity(ops.len());
        for op in &ops {
            let before = if self.files.exists(&op.path) { Some(self.files.read(&op.path)?) } else { None };
            saved.push((op.path.clone(), before));
        }
        for (index, op) in ops.iter().enumerate() {
            let outcome = match &op.bytes {
                Some(bytes) => self.files.write(&op.path, bytes),
                None => self.files.remove(&op.path),
            };
            if let Err(e) = outcome {
                for (path, before) in saved.iter().take(index).rev() {
                    let _ = match before {
                        Some(bytes) => self.files.write(path, bytes),
                        None => self.files.remove(path),
                    };
                }
                return Err(e);
            }
        }
        self.doc = next;
        Ok((inverse, ids))
    }

    // ---- queries -----------------------------------------------------------

    pub fn object(&self, id: Id) -> Option<&Object> {
        self.doc.objects.iter().find(|o| o.id == id)
    }

    pub fn object_mut(&mut self, id: Id) -> Option<&mut Object> {
        self.doc.objects.iter_mut().find(|o| o.id == id)
    }

    pub fn object_ids(&self) -> Vec<Id> {
        self.doc.objects.iter().map(|o| o.id).collect()
    }

    /// Direct children, in document order.
    pub fn children(&self, id: Id) -> Vec<Id> {
        self.doc.objects.iter().filter(|o| o.parent == Some(id)).map(|o| o.id).collect()
    }

    /// Every descendant, breadth first.
    pub fn descendants(&self, id: Id) -> Vec<Id> {
        let mut out = Vec::new();
        let mut queue: Vec<Id> = self.children(id);
        while !queue.is_empty() {
            let next = queue.remove(0);
            out.push(next);
            queue.extend(self.children(next));
        }
        out
    }

    /// `id` and every descendant, parents before children.
    pub fn subtree(&self, id: Id) -> Vec<Id> {
        let mut out = vec![id];
        out.extend(self.descendants(id));
        out
    }

    pub fn root_objects(&self) -> Vec<Id> {
        self.doc.objects.iter().filter(|o| o.parent.is_none()).map(|o| o.id).collect()
    }

    /// Composed transform of the whole parent chain.
    pub fn world_matrix(&self, id: Id) -> Option<DMat4> {
        let mut chain: Vec<Id> = Vec::new();
        let mut cursor = Some(id);
        while let Some(current) = cursor {
            if self.object(current).is_none() || chain.len() > self.doc.objects.len() {
                return None;
            }
            chain.push(current);
            cursor = self.object(current).and_then(|o| o.parent);
        }
        let mut m = DMat4::IDENTITY;
        for node in chain.iter().rev() {
            m *= local_matrix(self.object(*node)?);
        }
        Some(m)
    }

    pub fn world_position(&self, id: Id) -> Option<DVec3> {
        self.world_matrix(id).map(|m| m.w_axis.truncate())
    }

    /// Linear part of the world transform, without translation.
    pub fn world_basis(&self, id: Id) -> Option<DMat3> {
        self.world_matrix(id).map(DMat3::from_mat4)
    }

    pub fn layer_objects(&self, layer: Id) -> Vec<Id> {
        self.doc.objects.iter().filter(|o| o.layer == layer).map(|o| o.id).collect()
    }

    pub fn kind_objects(&self, filter: KindFilter) -> Vec<Id> {
        let brush_owners: BTreeSet<Id> = self
            .doc
            .objects
            .iter()
            .filter(|o| matches!(o.kind, ObjectKind::Brush(_)))
            .filter_map(|o| o.parent)
            .collect();
        self.doc
            .objects
            .iter()
            .filter(|o| match filter.matches(&o.kind) {
                Some(v) => v,
                None => {
                    let is_entity = matches!(o.kind, ObjectKind::Entity(_));
                    is_entity && brush_owners.contains(&o.id) == (filter == KindFilter::BrushEntity)
                }
            })
            .map(|o| o.id)
            .collect()
    }

    /// World-space bounds of one object, or `None` for kinds with no geometry.
    pub fn object_bounds(&self, id: Id) -> Option<Bounds> {
        let o = self.object(id)?;
        let m = self.world_matrix(id)?;
        let linear = DMat3::from_mat4(m);
        let world = m.w_axis.truncate();
        match &o.kind {
            ObjectKind::Brush(b) => solid_bounds(b, linear, world),
            ObjectKind::Mesh(m) => {
                let bytes = self.files.read(&m.source).ok()?;
                let ctx = crate::editmesh::EvalContext::from_doc(self, id);
                let prims = crate::editmesh::load_triangles_stacked(&bytes, &m.source, &m.modifiers, &ctx).ok()?;
                Bounds::from_points(prims.iter().flat_map(|p| p.positions.iter()).map(|q| {
                    linear * DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64) + world
                }))
            }
            ObjectKind::Volume(v) => v
                .brushes
                .iter()
                .filter_map(|b| solid_bounds(b, linear, world))
                .reduce(|a, b| a.union(&b)),
            ObjectKind::Path(p) => Bounds::from_points(p.points.iter().map(|pt| linear * *pt + world)),
            ObjectKind::Terrain(t) => {
                let n = DVec3::new(
                    t.resolution[0].saturating_sub(1) as f64 * t.cell_size,
                    t.resolution[1].saturating_sub(1) as f64 * t.cell_size,
                    0.0,
                );
                let b = Bounds::from_points([DVec3::ZERO, n])?;
                Some(Bounds { min: linear * b.min + world, max: linear * b.max + world })
            }
            ObjectKind::VoxelTerrain(v) => {
                Bounds::from_points(v.bounds.iter().map(|c| linear * *c + world))
            }
            _ => None,
        }
    }

    /// World-space bounds of a selection, or of the whole document when empty.
    pub fn bounds(&self, ids: &[Id]) -> Option<Bounds> {
        let ids: Vec<Id> = if ids.is_empty() { self.object_ids() } else { ids.to_vec() };
        let mut out: Option<Bounds> = None;
        for id in ids {
            let Some(b) = self.object_bounds(id) else { continue };
            out = Some(match out {
                Some(acc) => acc.union(&b),
                None => b,
            });
        }
        out
    }
}

fn solid_bounds(b: &Brush, linear: DMat3, world: DVec3) -> Option<Bounds> {
    let solid = crate::brush::Solid::from_faces(&b.faces).ok()?;
    Bounds::from_points(solid.vertices.iter().map(|v| linear * *v + world))
}

pub fn local_matrix(o: &Object) -> DMat4 {
    DMat4::from_scale_rotation_translation(o.transform.scale, o.transform.rotation, o.transform.position)
}

struct FileOp {
    path: String,
    bytes: Option<Vec<u8>>,
}

fn collect_files(cmd: &Command) -> Vec<FileOp> {
    let mut out = Vec::new();
    collect_files_into(cmd, &mut out);
    out
}

fn collect_files_into(cmd: &Command, out: &mut Vec<FileOp>) {
    match cmd {
        Command::SetFile { path, bytes } => out.push(FileOp { path: path.clone(), bytes: bytes.clone() }),
        Command::Batch { commands, .. } => {
            for c in commands {
                collect_files_into(c, out);
            }
        }
        _ => {}
    }
}

/// Structural checks every document must satisfy.
pub fn validate(doc: &MapDoc) -> Result<(), DocError> {
    if doc.layers.is_empty() {
        return Err(DocError::NoLayers);
    }
    let mut layers: BTreeSet<Id> = BTreeSet::new();
    for l in &doc.layers {
        if !layers.insert(l.id) {
            return Err(DocError::DuplicateLayer(l.id));
        }
    }
    let mut objects: HashMap<Id, &Object> = HashMap::with_capacity(doc.objects.len());
    for o in &doc.objects {
        if objects.contains_key(&o.id) {
            return Err(DocError::DuplicateId(o.id));
        }
        if !layers.contains(&o.layer) {
            return Err(DocError::UnknownLayer(o.layer));
        }
        objects.insert(o.id, o);
    }
    for o in &doc.objects {
        let Some(parent) = o.parent else { continue };
        if parent == o.id {
            return Err(DocError::SelfParent(o.id));
        }
        if !objects.contains_key(&parent) {
            return Err(DocError::UnknownObject(parent));
        }
        let mut seen: BTreeSet<Id> = BTreeSet::new();
        let mut cursor = Some(parent);
        while let Some(p) = cursor {
            if p == o.id || !seen.insert(p) {
                return Err(DocError::ParentCycle { child: o.id, parent });
            }
            cursor = objects.get(&p).and_then(|x| x.parent);
        }
    }
    Ok(())
}

fn execute(
    doc: &mut MapDoc,
    cmd: &Command,
    files: &dyn FileStore,
    depth: u32,
) -> Result<(Command, Vec<Id>), DocError> {
    if depth > MAX_BATCH_DEPTH {
        return Err(DocError::TooDeep);
    }
    match cmd {
        Command::AddObjects { objects } => {
            let mut ids = Vec::with_capacity(objects.len());
            for o in objects {
                if doc.objects.iter().any(|e| e.id == o.id) {
                    return Err(DocError::DuplicateId(o.id));
                }
                doc.objects.push(o.clone());
                ids.push(o.id);
            }
            Ok((Command::RemoveObjects { ids: ids.clone() }, ids))
        }
        Command::RemoveObjects { ids } => {
            for id in ids {
                if !doc.objects.iter().any(|o| o.id == *id) {
                    return Err(DocError::UnknownObject(*id));
                }
            }
            let mut gone: BTreeSet<Id> = ids.iter().copied().collect();
            let mut pending: Vec<Id> = gone.iter().copied().collect();
            while let Some(id) = pending.pop() {
                for child in doc.objects.iter().filter(|o| o.parent == Some(id)).map(|o| o.id).collect::<Vec<_>>() {
                    if gone.insert(child) {
                        pending.push(child);
                    }
                }
            }
            let removed: Vec<Object> = doc.objects.iter().filter(|o| gone.contains(&o.id)).cloned().collect();
            doc.objects.retain(|o| !gone.contains(&o.id));
            let ids = removed.iter().map(|o| o.id).collect();
            Ok((Command::AddObjects { objects: removed }, ids))
        }
        Command::ReplaceObjects { objects } => {
            // An object decompiled out of a reference map is hidden while the
            // map's own record stands for it. Editing what it is or where it
            // stands promotes it: it becomes visible, so the compiler drops
            // the record and compiles the object in its place. Hiding,
            // showing, renaming or reparenting it does not.
            let decompiled: std::collections::HashSet<Id> = doc
                .objects
                .iter()
                .filter_map(|o| match &o.kind {
                    ObjectKind::Reference(r) => Some(r.sources.keys().copied()),
                    _ => None,
                })
                .flatten()
                .collect();
            let mut old = Vec::with_capacity(objects.len());
            let mut ids = Vec::with_capacity(objects.len());
            for o in objects {
                let slot =
                    doc.objects.iter_mut().find(|e| e.id == o.id).ok_or(DocError::UnknownObject(o.id))?;
                let mut next = o.clone();
                if decompiled.contains(&o.id)
                    && slot.hidden
                    && next.hidden
                    && (slot.transform != next.transform || slot.kind != next.kind)
                {
                    next.hidden = false;
                }
                stale_light_keys(slot, &mut next);
                old.push(std::mem::replace(slot, next));
                ids.push(o.id);
            }
            Ok((Command::ReplaceObjects { objects: old }, ids))
        }
        Command::AddLayer { layer } => {
            if doc.layers.iter().any(|l| l.id == layer.id) {
                return Err(DocError::DuplicateLayer(layer.id));
            }
            doc.layers.push(layer.clone());
            Ok((Command::RemoveLayer { id: layer.id }, Vec::new()))
        }
        Command::RemoveLayer { id } => {
            if doc.layers.len() <= 1 {
                return Err(DocError::NoLayers);
            }
            let Some(pos) = doc.layers.iter().position(|l| l.id == *id) else {
                return Err(DocError::UnknownLayer(*id));
            };
            if doc.objects.iter().any(|o| o.layer == *id) {
                return Err(DocError::LayerNotEmpty(*id));
            }
            let layer = doc.layers.remove(pos);
            Ok((Command::AddLayer { layer }, Vec::new()))
        }
        Command::ReplaceLayer { layer } => {
            let slot =
                doc.layers.iter_mut().find(|l| l.id == layer.id).ok_or(DocError::UnknownLayer(layer.id))?;
            let old = std::mem::replace(slot, layer.clone());
            Ok((Command::ReplaceLayer { layer: old }, Vec::new()))
        }
        Command::SetMeta { meta } => {
            let old = std::mem::replace(&mut doc.meta, meta.clone());
            Ok((Command::SetMeta { meta: old }, Vec::new()))
        }
        Command::SetSettings { settings } => {
            let old = std::mem::replace(&mut doc.settings, settings.clone());
            Ok((Command::SetSettings { settings: old }, Vec::new()))
        }
        Command::SetFile { path, .. } => {
            crate::project::check_path(path)?;
            let previous = if files.exists(path) { Some(files.read(path)?) } else { None };
            // The meshes that read this file change shape with it.
            let readers = doc
                .objects
                .iter()
                .filter(|o| matches!(&o.kind, ObjectKind::Mesh(m) if m.source == *path))
                .map(|o| o.id)
                .collect();
            Ok((Command::SetFile { path: path.clone(), bytes: previous }, readers))
        }
        Command::Batch { label, commands } => {
            let mut inverses = Vec::with_capacity(commands.len());
            let mut ids: Vec<Id> = Vec::new();
            for c in commands {
                let (inv, touched) = execute(doc, c, files, depth + 1)?;
                inverses.push(inv);
                for id in touched {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
            inverses.reverse();
            Ok((Command::Batch { label: format!("undo {label}"), commands: inverses }, ids))
        }
    }
}

/// Object keyvalues of an entity, the shape `EntitySchema::validate_entity` reads.
pub fn entity_of(o: &Object) -> Option<(&str, &KeyValues)> {
    match &o.kind {
        ObjectKind::Entity(e) => Some((e.classname.as_str(), &e.keyvalues)),
        ObjectKind::Volume(v) => Some((v.classname.as_str(), &v.keyvalues)),
        ObjectKind::Light(l) => Some(("light", &l.keyvalues)),
        ObjectKind::Path(p) => Some(("path", &p.keyvalues)),
        _ => None,
    }
}

/// The partition a document object compiles into when it does not name one.
pub fn partition_of(o: &Object) -> Option<Partition> {
    match &o.kind {
        ObjectKind::Entity(e) => e.partition,
        ObjectKind::Volume(v) => v.keyvalues.get("_partition").and_then(|s| Partition::from_name(s)),
        _ => None,
    }
}

pub fn new_id() -> Id {
    Uuid::new_v4()
}

pub fn new_object(name: impl Into<String>, layer: Id, kind: ObjectKind) -> Object {
    Object {
        id: new_id(),
        name: name.into(),
        layer,
        parent: None,
        transform: crate::model::Transform::default(),
        hidden: false,
        locked: false,
        kind,
    }
}

/// The keys a light's writer derives from its fields and its transform. A
/// decompiled light carries the shipped text of these, which it compiles back
/// to until the field behind one is edited.
const LIGHT_FIELD_KEYS: &[&str] = &[
    "_light",
    "_distance",
    "_inner_cone",
    "_cone",
    "_fifty_percent_distance",
    "_zero_percent_distance",
    "realtime",
    "realtime_shadows",
    "shadow",
    "shadows",
    "fogdensity",
];

/// Drops a light's shipped derived keys once what they were derived from has
/// changed, so the edit is what compiles.
fn stale_light_keys(old: &Object, next: &mut Object) {
    let (ObjectKind::Light(a), ObjectKind::Light(b)) = (&old.kind, &mut next.kind) else { return };
    let fields_changed = a.kind != b.kind
        || a.color != b.color
        || a.intensity != b.intensity
        || a.range != b.range
        || a.inner_angle != b.inner_angle
        || a.outer_angle != b.outer_angle
        || a.size != b.size
        || a.cast_shadows != b.cast_shadows
        || a.baked != b.baked
        || a.realtime != b.realtime;
    if fields_changed {
        b.keyvalues.retain(|k, _| !LIGHT_FIELD_KEYS.contains(&k.as_str()));
        if a.kind != b.kind {
            b.keyvalues.shift_remove("classname");
        }
    }
    if old.transform.rotation != next.transform.rotation {
        if let ObjectKind::Light(b) = &mut next.kind {
            b.keyvalues.shift_remove("pitch");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::box_brush;
    use crate::model::{Entity, Layer, ObjectKind};

    fn blank_entity() -> Entity {
        Entity { classname: "info_target".into(), keyvalues: Default::default(), partition: None, links: Default::default() }
    }

    fn brush_doc() -> Document {
        let mut d = Document::empty();
        let layer = d.layer_ids()[0];
        let o = new_object("box", layer, ObjectKind::Brush(box_brush(DVec3::ZERO, DVec3::splat(64.0), "m").unwrap()));
        d.apply(Command::AddObjects { objects: vec![o] }).unwrap();
        d
    }

    #[test]
    fn add_and_remove_round_trip() {
        let mut d = brush_doc();
        let id = d.object_ids()[0];
        assert!(d.can_undo());
        d.undo().unwrap();
        assert!(!d.can_undo());
        assert!(d.object(id).is_none());
        d.redo().unwrap();
        assert!(d.object(id).is_some());
    }

    /// A document with a reference and one decompiled, hidden entity.
    fn decompiled_doc() -> (Document, Id) {
        let mut d = Document::empty();
        let layer = d.layer_ids()[0];
        let mut target = new_object("target", layer, ObjectKind::Entity(blank_entity()));
        target.hidden = true;
        let mut sources = indexmap::IndexMap::new();
        sources.insert(target.id, crate::model::RefRecord::Entity { index: 3 });
        let reference = new_object(
            "reference",
            layer,
            ObjectKind::Reference(crate::model::Reference {
                kind: crate::model::ReferenceKind::ApexMap,
                source: "mp_rr_x".into(),
                promoted: Vec::new(),
                removed_entities: Vec::new(),
                sources,
            }),
        );
        let id = target.id;
        d.apply(Command::AddObjects { objects: vec![reference, target] }).unwrap();
        (d, id)
    }

    #[test]
    fn an_edit_to_a_decompiled_object_promotes_it() {
        let (mut d, id) = decompiled_doc();
        let mut moved = d.object(id).unwrap().clone();
        moved.transform.position = DVec3::new(64.0, 0.0, 0.0);
        d.apply(Command::ReplaceObjects { objects: vec![moved] }).unwrap();
        assert!(!d.object(id).unwrap().hidden, "a moved decompiled object is compiled in its record's place");
        d.undo().unwrap();
        assert!(d.object(id).unwrap().hidden, "undo puts the record back");

        let mut renamed = d.object(id).unwrap().clone();
        renamed.name = "renamed".into();
        d.apply(Command::ReplaceObjects { objects: vec![renamed] }).unwrap();
        assert!(d.object(id).unwrap().hidden, "a rename is not an edit of the record");

        let mut keyed = d.object(id).unwrap().clone();
        if let ObjectKind::Entity(e) = &mut keyed.kind {
            e.keyvalues.insert("targetname".into(), "t".into());
        }
        d.apply(Command::ReplaceObjects { objects: vec![keyed] }).unwrap();
        assert!(!d.object(id).unwrap().hidden, "a key edit promotes it");
    }

    #[test]
    fn an_edit_to_an_ordinary_hidden_object_leaves_it_hidden() {
        let mut d = brush_doc();
        let id = d.object_ids()[0];
        let mut o = d.object(id).unwrap().clone();
        o.hidden = true;
        d.apply(Command::ReplaceObjects { objects: vec![o.clone()] }).unwrap();
        o.transform.position = DVec3::splat(8.0);
        d.apply(Command::ReplaceObjects { objects: vec![o] }).unwrap();
        assert!(d.object(id).unwrap().hidden);
    }

    #[test]
    fn undo_on_empty_history_is_an_error() {
        let mut d = Document::empty();
        assert!(matches!(d.undo(), Err(DocError::NothingToUndo)));
        assert!(matches!(d.redo(), Err(DocError::NothingToRedo)));
    }

    #[test]
    fn remove_cascades_to_children() {
        let mut d = brush_doc();
        let root = d.object_ids()[0];
        let layer = d.layer_ids()[0];
        let child = new_object("child", layer, ObjectKind::Group);
        let mut child = child;
        child.parent = Some(root);
        d.apply(Command::AddObjects { objects: vec![child] }).unwrap();
        assert_eq!(d.children(root).len(), 1);
        let (_, ids) = d.apply(Command::RemoveObjects { ids: vec![root] }).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(d.doc.objects.is_empty());
    }

    #[test]
    fn world_matrix_composes_the_parent_chain() {
        let mut d = brush_doc();
        let root = d.object_ids()[0];
        let layer = d.layer_ids()[0];
        let mut parent = new_object("p", layer, ObjectKind::Group);
        parent.transform.position = DVec3::new(10.0, 0.0, 0.0);
        let parent_id = parent.id;
        d.apply(Command::AddObjects { objects: vec![parent] }).unwrap();
        let mut child = new_object("c", layer, ObjectKind::Group);
        child.parent = Some(parent_id);
        child.transform.position = DVec3::new(0.0, 5.0, 0.0);
        let child_id = child.id;
        d.apply(Command::AddObjects { objects: vec![child] }).unwrap();
        assert_eq!(d.world_position(child_id), Some(DVec3::new(10.0, 5.0, 0.0)));
        assert_eq!(d.world_position(root), Some(DVec3::ZERO));
    }

    #[test]
    fn parent_cycles_are_rejected() {
        let mut d = brush_doc();
        let a = d.object_ids()[0];
        let layer = d.layer_ids()[0];
        let mut b = new_object("b", layer, ObjectKind::Group);
        b.parent = Some(a);
        let b_id = b.id;
        d.apply(Command::AddObjects { objects: vec![b] }).unwrap();
        let mut a_obj = d.object(a).unwrap().clone();
        a_obj.parent = Some(b_id);
        assert!(matches!(
            d.apply(Command::ReplaceObjects { objects: vec![a_obj] }),
            Err(DocError::ParentCycle { .. })
        ));
    }

    #[test]
    fn bounds_of_a_box_object() {
        let d = brush_doc();
        let b = d.bounds(&[]).unwrap();
        assert_eq!(b.min, DVec3::ZERO);
        assert_eq!(b.max, DVec3::splat(64.0));
        assert_eq!(b.size(), DVec3::splat(64.0));
    }

    #[test]
    fn kind_filter_separates_brush_entities() {
        let mut d = brush_doc();
        let layer = d.layer_ids()[0];
        let owner = new_object("door", layer, ObjectKind::Entity(Entity {
            classname: "func_door".into(),
            ..blank_entity()
        }));
        let owner_id = owner.id;
        d.apply(Command::AddObjects { objects: vec![owner] }).unwrap();
        let mut solid = new_object(
            "slab",
            layer,
            ObjectKind::Brush(crate::brush::box_brush(DVec3::ZERO, DVec3::splat(8.0), "m").unwrap()),
        );
        solid.parent = Some(owner_id);
        d.apply(Command::AddObjects { objects: vec![solid] }).unwrap();
        assert_eq!(d.kind_objects(KindFilter::BrushEntity), vec![owner_id]);
        assert!(d.kind_objects(KindFilter::PointEntity).is_empty());
        let mut point = new_object("marker", layer, ObjectKind::Entity(Entity {
            classname: "info_target".into(),
            ..blank_entity()
        }));
        point.parent = Some(owner_id);
        let point_id = point.id;
        d.apply(Command::AddObjects { objects: vec![point] }).unwrap();
        assert_eq!(d.kind_objects(KindFilter::PointEntity), vec![point_id]);
        assert_eq!(d.kind_objects(KindFilter::BrushEntity), vec![owner_id]);
    }

    /// SplitMix64, so a failing sequence replays from its seed.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }

        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    fn seeded_doc() -> Document {
        let mut d = Document::empty();
        let layer = d.layer_ids()[0];
        for i in 0..6u32 {
            let min = DVec3::splat(i as f64 * 64.0);
            let mut o = new_object(
                format!("box{i}"),
                layer,
                ObjectKind::Brush(box_brush(min, min + DVec3::splat(32.0), "m").unwrap()),
            );
            o.transform.position = DVec3::new(i as f64 * 16.0, 0.0, 0.0);
            d.apply(Command::AddObjects { objects: vec![o] }).unwrap();
        }
        d.apply(Command::SetFile { path: "terrain/h.f32".into(), bytes: Some(vec![1, 2, 3]) }).unwrap();
        d
    }

    /// A command drawn from the shapes the editor actually issues. Some of them
    /// name ids that are not there, so a run covers rejected edits too.
    fn random_command(rng: &mut Rng, d: &Document) -> Command {
        let layer = d.layer_ids()[0];
        let ids = d.object_ids();
        let pick = |rng: &mut Rng| ids[rng.below(ids.len() as u64) as usize];
        match rng.below(8) {
            0 | 1 => {
                let at = DVec3::splat(rng.unit() * 512.0);
                let mut o = new_object(
                    "new",
                    layer,
                    ObjectKind::Brush(box_brush(at, at + DVec3::splat(16.0 + rng.unit() * 32.0), "m").unwrap()),
                );
                o.transform.position = at;
                Command::AddObjects { objects: vec![o] }
            }
            2 => Command::RemoveObjects { ids: vec![pick(rng)] },
            3 => {
                let id = if rng.below(2) == 0 { uuid::Uuid::new_v4() } else { pick(rng) };
                let mut o = new_object("ghost", layer, ObjectKind::Group);
                o.id = id;
                Command::ReplaceObjects { objects: vec![o] }
            }
            4 => {
                let mut meta = d.doc.meta.clone();
                meta.stem = format!("mp_rr_{}", rng.below(64));
                meta.playlists = vec![format!("pl{}", rng.below(4))];
                Command::SetMeta { meta }
            }
            5 => {
                let mut settings = d.doc.settings.clone();
                settings.bake.bounces = rng.below(5) as u32;
                settings.vis.portals = rng.below(2) == 1;
                Command::SetSettings { settings }
            }
            6 => Command::SetFile {
                path: format!("terrain/h{}.f32", rng.below(4)),
                bytes: if rng.below(4) == 0 { None } else { Some(vec![rng.next() as u8; 8]) },
            },
            _ => {
                let extra = Layer { id: uuid::Uuid::new_v4(), name: format!("l{}", rng.below(8)), visible: true, locked: false };
                let id = extra.id;
                Command::Batch {
                    label: "pair".into(),
                    commands: vec![Command::AddLayer { layer: extra }, Command::RemoveLayer { id }],
                }
            }
        }
    }

    fn state(d: &Document) -> String {
        let files = d
            .files
            .list()
            .map(|e| e.into_iter().map(|f| format!("{}={}", f.path, f.size)).collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        format!("{}\n{files}", crate::project::serialize(&d.doc))
    }

    #[test]
    fn random_commands_undo_roundtrip() {
        let mut rng = Rng(0x5EED_1234);
        let mut d = seeded_doc();
        let baseline = state(&d);
        let seed_depth = d.undo_depth();
        let mut applied = 0usize;
        for _ in 0..1000 {
            if d.apply(random_command(&mut rng, &d)).is_ok() {
                applied += 1;
            }
        }
        assert!(applied > 400, "the run must really change the document, applied {applied}");
        let after = state(&d);
        assert_ne!(baseline, after);

        let mut undone = 0usize;
        while d.undo_depth() > seed_depth {
            d.undo().unwrap();
            undone += 1;
        }
        assert_eq!(undone, applied, "one undo entry per applied command");
        if baseline != state(&d) {
            let now = state(&d);
            let first = baseline.chars().zip(now.chars()).position(|(a, b)| a != b);
            let names: Vec<&str> = d.doc.objects.iter().map(|o| o.name.as_str()).collect();
            panic!("undo did not restore the document: {} objects {names:?}, first difference at char {first:?}", names.len());
        }

        while d.can_redo() {
            d.redo().unwrap();
        }
        if after != state(&d) {
            panic!("redo did not replay the whole sequence");
        }
    }

    #[test]
    fn failing_batch_is_atomic() {
        let mut d = seeded_doc();
        let before = state(&d);
        let depth = d.undo_depth();
        let layer = d.layer_ids()[0];
        let good = new_object("ok", layer, ObjectKind::Group);
        let bad = Command::Batch {
            label: "half".into(),
            commands: vec![
                Command::AddObjects { objects: vec![good.clone()] },
                Command::RemoveObjects { ids: vec![uuid::Uuid::new_v4()] },
            ],
        };
        assert!(matches!(d.apply(bad), Err(DocError::UnknownObject(_))));
        assert_eq!(before, state(&d), "the first command of a failing batch left no trace");
        assert_eq!(d.undo_depth(), depth, "a rejected command records no history");
        assert!(d.object(good.id).is_none());
    }

    #[test]
    fn a_failing_file_write_restores_the_files_it_touched() {
        let mut d = seeded_doc();
        let before = state(&d);
        let depth = d.undo_depth();
        let bad = Command::Batch {
            label: "files".into(),
            commands: vec![
                Command::SetFile { path: "terrain/other.f32".into(), bytes: Some(vec![9, 9]) },
                Command::SetFile { path: "../escape".into(), bytes: Some(vec![1]) },
            ],
        };
        assert!(matches!(d.apply(bad), Err(DocError::BadPath(_))));
        assert_eq!(before, state(&d));
        assert_eq!(d.undo_depth(), depth);
    }

    #[test]
    fn a_gesture_coalesces_consecutive_replaces() {
        let mut d = seeded_doc();
        let id = d.object_ids()[0];
        let depth_before = d.undo_depth();
        d.begin_gesture();
        for step in 1..=5 {
            let mut o = d.object(id).unwrap().clone();
            o.transform.position.x += step as f64 * 16.0;
            d.apply(Command::ReplaceObjects { objects: vec![o] }).unwrap();
        }
        d.end_gesture();
        assert_eq!(d.undo_depth(), depth_before + 1, "the drag is one undo step");
        let moved = d.object(id).unwrap().transform.position.x;
        d.undo().unwrap();
        assert_ne!(moved, d.object(id).unwrap().transform.position.x);
        assert_eq!(d.object(id).unwrap().transform.position.x, 0.0);
    }

    #[test]
    fn a_gesture_folds_every_replaced_object_into_one_step() {
        let mut d = seeded_doc();
        let a = d.object_ids()[0];
        let b = d.object_ids()[1];
        let depth_before = d.undo_depth();
        d.begin_gesture();
        for step in 1..=3 {
            for id in [a, b] {
                let mut o = d.object(id).unwrap().clone();
                o.transform.position.z += step as f64;
                d.apply(Command::ReplaceObjects { objects: vec![o] }).unwrap();
            }
        }
        d.end_gesture();
        assert_eq!(d.undo_depth(), depth_before + 1, "the whole drag is one undo step");
        assert_eq!(d.object(a).unwrap().transform.position.z, 6.0, "1 + 2 + 3");
        assert_eq!(d.object(b).unwrap().transform.position.z, 6.0);
        d.undo().unwrap();
        assert_eq!(d.object(a).unwrap().transform.position.z, 0.0);
        assert_eq!(d.object(b).unwrap().transform.position.z, 0.0);
    }

    #[test]
    fn nested_batches_beyond_the_depth_limit_are_rejected() {
        let mut d = seeded_doc();
        let before = state(&d);
        let mut cmd = Command::SetMeta { meta: d.doc.meta.clone() };
        for _ in 0..80 {
            cmd = Command::Batch { label: "nest".into(), commands: vec![cmd] };
        }
        assert!(matches!(d.apply(cmd), Err(DocError::TooDeep)));
        assert_eq!(before, state(&d));
    }

    #[test]
    fn layers_must_exist_and_must_not_hold_objects() {
        let mut d = brush_doc();
        let layer = d.layer_ids()[0];
        let mut ghost = new_object("g", uuid::Uuid::new_v4(), ObjectKind::Group);
        ghost.layer = uuid::Uuid::new_v4();
        assert!(matches!(d.apply(Command::AddObjects { objects: vec![ghost] }), Err(DocError::UnknownLayer(_))));
        let spare = Layer { id: uuid::Uuid::new_v4(), name: "spare".into(), visible: true, locked: false };
        let spare_id = spare.id;
        d.apply(Command::AddLayer { layer: spare }).unwrap();
        assert!(matches!(d.apply(Command::RemoveLayer { id: layer }), Err(DocError::LayerNotEmpty(_))));
        assert!(matches!(d.apply(Command::RemoveLayer { id: spare_id }), Ok((_, _))));
        let mut solo = Document::empty();
        let only = solo.layer_ids()[0];
        assert!(matches!(solo.apply(Command::RemoveLayer { id: only }), Err(DocError::NoLayers)));

        let empty = Layer { id: uuid::Uuid::new_v4(), name: "empty".into(), visible: true, locked: false };
        let empty_id = empty.id;
        d.apply(Command::AddLayer { layer: empty }).unwrap();
        d.apply(Command::RemoveLayer { id: empty_id }).unwrap();
        assert!(!d.layer_ids().contains(&empty_id));
        d.undo().unwrap();
        assert!(d.layer_ids().contains(&empty_id));
        d.redo().unwrap();
        assert!(!d.layer_ids().contains(&empty_id));
    }
}
