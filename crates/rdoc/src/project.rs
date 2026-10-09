//! Project IO: the on-disk layout, format migration, backups and path safety.

use std::collections::BTreeMap;
#[cfg(feature = "native")]
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::doc::{DocError, Document, MemoryFileStore};
use crate::model::{FORMAT_VERSION, MapDoc};

/// The document itself; every other project file is a blob in the file store.
pub const MAP_FILE: &str = "map.json";
/// Backups kept beside the document, oldest first.
pub const BACKUP_COUNT: usize = 3;
/// Refuse to grow a project file past this.
pub const MAX_FILE_BYTES: usize = 512 << 20;

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("file path is not allowed: {0}")]
    BadPath(String),
    #[error("map.json does not parse")]
    Malformed,
    #[error("format {0} is newer than this build, which reads up to {1}")]
    FutureFormat(u32, u32),
    #[error("no migration from format {0} to {1}")]
    NoMigration(u32, u32),
    #[error("io: {0}")]
    Io(String),
}

/// Names Windows refuses in a path component, with or without an extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validates a project-relative path and returns it in `a/b` form.
///
/// Rejects absolute paths, drive letters, UNC prefixes, `..` traversal, NUL,
/// control characters, empty components and Windows device names.
pub fn check_path(path: &str) -> Result<String, ProjectError> {
    let bad = |why: &str| ProjectError::BadPath(format!("{path} ({why})"));
    if path.is_empty() {
        return Err(bad("empty"));
    }
    if path.contains('\0') {
        return Err(bad("NUL byte"));
    }
    if path.chars().any(|c| (c as u32) < 0x20) {
        return Err(bad("control character"));
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(bad("absolute"));
    }
    if path.starts_with("//") || path.starts_with("\\\\") {
        return Err(bad("unc"));
    }
    let unified = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        if part.is_empty() || part == "." {
            return Err(bad("empty component"));
        }
        if part == ".." {
            return Err(bad("parent traversal"));
        }
        if part.contains(':') {
            return Err(bad("drive letter or stream"));
        }
        let stem = part.split('.').next().unwrap_or(part);
        if stem.is_empty() || part.ends_with('.') || part.ends_with(' ') {
            return Err(bad("untrimmed component"));
        }
        if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
            return Err(bad("reserved device name"));
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(bad("empty"));
    }
    Ok(parts.join("/"))
}

/// Same rules as `check_path`, for paths already resolved by a `Path`.
#[cfg(feature = "native")]
fn check_native(rel: &Path) -> Result<PathBuf, ProjectError> {
    let text = rel.to_string_lossy().replace('\\', "/");
    check_path(&text)?;
    if rel.is_absolute() {
        return Err(ProjectError::BadPath(format!("{} (absolute)", rel.display())));
    }
    for c in rel.components() {
        match c {
            Component::Normal(_) => {}
            _ => return Err(ProjectError::BadPath(format!("{} (not a plain relative path)", rel.display()))),
        }
    }
    Ok(rel.to_path_buf())
}

/// Filesystem side of a project directory.
pub trait ProjectFs {
    fn read(&self, rel: &str) -> Result<Vec<u8>, ProjectError>;
    fn write(&mut self, rel: &str, bytes: &[u8]) -> Result<(), ProjectError>;
    fn remove(&mut self, rel: &str) -> Result<(), ProjectError>;
    fn exists(&self, rel: &str) -> bool;
    fn list(&self) -> Result<Vec<String>, ProjectError>;
}

/// An in-memory project, used by tests and by the wasm editor's scratch space.
#[derive(Clone, Debug, Default)]
pub struct MemoryProject {
    files: BTreeMap<String, Vec<u8>>,
}

impl MemoryProject {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProjectFs for MemoryProject {
    fn read(&self, rel: &str) -> Result<Vec<u8>, ProjectError> {
        let key = check_path(rel)?;
        self.files.get(&key).cloned().ok_or_else(|| ProjectError::Io(format!("{key} does not exist")))
    }

    fn write(&mut self, rel: &str, bytes: &[u8]) -> Result<(), ProjectError> {
        let key = check_path(rel)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(ProjectError::Io(format!("{key} is larger than {MAX_FILE_BYTES} bytes")));
        }
        self.files.insert(key, bytes.to_vec());
        Ok(())
    }

    fn remove(&mut self, rel: &str) -> Result<(), ProjectError> {
        let key = check_path(rel)?;
        self.files.remove(&key);
        Ok(())
    }

    fn exists(&self, rel: &str) -> bool {
        check_path(rel).map(|k| self.files.contains_key(&k)).unwrap_or(false)
    }

    fn list(&self) -> Result<Vec<String>, ProjectError> {
        Ok(self.files.keys().cloned().collect())
    }
}

/// A real project directory. Only this file touches the filesystem.
#[cfg(feature = "native")]
pub struct NativeProject {
    root: PathBuf,
}

#[cfg(feature = "native")]
impl NativeProject {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ProjectError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| ProjectError::Io(format!("{}: {e}", root.display())))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn full(&self, rel: &str) -> Result<PathBuf, ProjectError> {
        let rel = check_native(Path::new(rel))?;
        Ok(self.root.join(rel))
    }
}

#[cfg(feature = "native")]
impl ProjectFs for NativeProject {
    fn read(&self, rel: &str) -> Result<Vec<u8>, ProjectError> {
        let full = self.full(rel)?;
        std::fs::read(&full).map_err(|e| ProjectError::Io(format!("{}: {e}", full.display())))
    }

    fn write(&mut self, rel: &str, bytes: &[u8]) -> Result<(), ProjectError> {
        let full = self.full(rel)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ProjectError::Io(format!("{}: {e}", parent.display())))?;
        }
        std::fs::write(&full, bytes).map_err(|e| ProjectError::Io(format!("{}: {e}", full.display())))
    }

    fn remove(&mut self, rel: &str) -> Result<(), ProjectError> {
        let full = self.full(rel)?;
        match std::fs::remove_file(&full) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ProjectError::Io(format!("{}: {e}", full.display()))),
        }
    }

    fn exists(&self, rel: &str) -> bool {
        self.full(rel).map(|p| p.is_file()).unwrap_or(false)
    }

    fn list(&self) -> Result<Vec<String>, ProjectError> {
        let mut out = Vec::new();
        walk(&self.root, &self.root, &mut out)?;
        out.sort();
        Ok(out)
    }
}

#[cfg(feature = "native")]
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), ProjectError> {
    let entries = std::fs::read_dir(dir).map_err(|e| ProjectError::Io(format!("{}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| ProjectError::Io(format!("{}: {e}", dir.display())))?;
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// A `FileStore` view of a `ProjectFs`, so the document engine can use either.
pub struct ProjectFiles {
    pub fs: Box<dyn ProjectFs>,
}

impl ProjectFiles {
    pub fn new(fs: Box<dyn ProjectFs>) -> Self {
        Self { fs }
    }
}

impl crate::doc::FileStore for ProjectFiles {
    fn read(&self, path: &str) -> Result<Vec<u8>, DocError> {
        let key = check_path(path)?;
        self.fs.read(&key).map_err(DocError::from)
    }

    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), DocError> {
        let key = check_path(path)?;
        self.fs.write(&key, bytes).map_err(DocError::from)
    }

    fn remove(&mut self, path: &str) -> Result<(), DocError> {
        let key = check_path(path)?;
        self.fs.remove(&key).map_err(DocError::from)
    }

    fn exists(&self, path: &str) -> bool {
        self.fs.exists(path)
    }

    fn list(&self) -> Result<Vec<crate::doc::FileEntry>, DocError> {
        let mut out = Vec::new();
        for path in self.fs.list().map_err(DocError::from)? {
            let size = self.fs.read(&path).map(|b| b.len()).unwrap_or(0);
            out.push(crate::doc::FileEntry { path, size });
        }
        Ok(out)
    }
}

/// `map.json` text: pretty printed, two-space indent, LF, trailing newline.
pub fn serialize(doc: &MapDoc) -> String {
    let mut text = serde_json::to_string_pretty(doc).expect("MapDoc always serializes");
    text.push('\n');
    text
}

pub fn deserialize(text: &str) -> Result<MapDoc, ProjectError> {
    let mut doc: MapDoc = serde_json::from_str(text).map_err(|_| ProjectError::Malformed)?;
    migrate(&mut doc)?;
    Ok(doc)
}

/// Brings an older document up to `FORMAT_VERSION`.
pub fn migrate(doc: &mut MapDoc) -> Result<(), ProjectError> {
    if doc.format > FORMAT_VERSION {
        return Err(ProjectError::FutureFormat(doc.format, FORMAT_VERSION));
    }
    while doc.format < FORMAT_VERSION {
        let from = doc.format;
        let step = MIGRATIONS.iter().find(|(f, _)| *f == from).ok_or(ProjectError::NoMigration(from, FORMAT_VERSION))?;
        (step.1)(doc);
        doc.format = from + 1;
    }
    Ok(())
}

type Migration = fn(&mut MapDoc);

/// Steps applied in order when a document predates the current format. Version 0
/// is the pre-versioning layout: the field did not exist, and its absence meant
/// the current one.
const MIGRATIONS: &[(u32, Migration)] = &[(0, |doc: &mut MapDoc| {
    doc.meta.stem = if doc.meta.stem.is_empty() { "mp_rr_untitled".into() } else { doc.meta.stem.clone() };
})];

pub fn backup_name(index: usize) -> String {
    format!("{MAP_FILE}.bak{index}")
}

/// Copies `map.json` over the oldest backup slot before overwriting it.
pub fn autosave(fs: &mut dyn ProjectFs, doc: &MapDoc) -> Result<(), ProjectError> {
    let text = serialize(doc);
    if fs.exists(MAP_FILE) {
        // Oldest slot first, so each slot keeps its own generation.
        for index in (2..=BACKUP_COUNT).rev() {
            let from = backup_name(index - 1);
            if fs.exists(&from) {
                let bytes = fs.read(&from)?;
                fs.write(&backup_name(index), &bytes)?;
            } else {
                let _ = fs.remove(&backup_name(index));
            }
        }
        let previous = fs.read(MAP_FILE)?;
        fs.write(&backup_name(1), &previous)?;
    }
    fs.write(MAP_FILE, text.as_bytes())
}

/// Writes `map.json` without touching the backups.
pub fn save(fs: &mut dyn ProjectFs, doc: &MapDoc) -> Result<(), ProjectError> {
    fs.write(MAP_FILE, serialize(doc).as_bytes())
}

/// Newest existing backup, used to recover a document after a bad edit.
pub fn newest_backup(fs: &dyn ProjectFs) -> Option<MapDoc> {
    for index in 1..=BACKUP_COUNT {
        let name = backup_name(index);
        if fs.exists(&name) {
            if let Ok(bytes) = fs.read(&name) {
                if let Ok(text) = String::from_utf8(bytes) {
                    if let Ok(doc) = deserialize(&text) {
                        return Some(doc);
                    }
                }
            }
        }
    }
    None
}

/// Loads a project directory into a document backed by the same filesystem.
pub fn load(fs: Box<dyn ProjectFs>) -> Result<Document, ProjectError> {
    let text = fs.read(MAP_FILE)?;
    let text = String::from_utf8(text).map_err(|_| ProjectError::Malformed)?;
    let doc = deserialize(&text)?;
    Ok(Document::new(doc, Box::new(ProjectFiles::new(fs))))
}

/// Loads a project into an in-memory store, which is what the browser does.
pub fn load_to_memory(fs: &dyn ProjectFs) -> Result<Document, ProjectError> {
    let text = fs.read(MAP_FILE)?;
    let text = String::from_utf8(text).map_err(|_| ProjectError::Malformed)?;
    let doc = deserialize(&text)?;
    let mut store = MemoryFileStore::new();
    for path in fs.list()? {
        if path == MAP_FILE {
            continue;
        }
        if let Ok(bytes) = fs.read(&path) {
            let _ = crate::doc::FileStore::write(&mut store, &path, &bytes);
        }
    }
    Ok(Document::new(doc, Box::new(store)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;

    #[test]
    fn project_path_safety() {
        for good in ["map.json", "terrain/heights.f32", "meshes/a/b/c.glb", "prefabs/x.r5prefab", "a.b.c/d"] {
            assert_eq!(check_path(good).unwrap(), good, "{good} should be allowed");
        }
        let bad = [
            "",
            "/etc/passwd",
            "\\windows\\system32",
            "C:/windows",
            "c:map.json",
            "\\\\server\\share\\x",
            "//server/share/x",
            "a/../../b",
            "..",
            "a//b",
            "a/./b",
            "map.json/",
            "a/b/../c",
            "aux",
            "NUL.json",
            "com3",
            "trailing.",
            "trailing ",
            "nul\0byte",
            "tab\there",
            "stream:file",
        ];
        for path in bad {
            assert!(check_path(path).is_err(), "{path:?} should be rejected");
        }
        assert_eq!(check_path("terrain\\heights.f32").unwrap(), "terrain/heights.f32");
    }

    #[test]
    fn serialization_is_stable_and_newline_terminated() {
        let doc = MapDoc::default();
        let a = serialize(&doc);
        assert!(a.ends_with("}\n"));
        assert!(a.contains("\n  \"format\": 1"));
        let b = serialize(&deserialize(&a).unwrap());
        assert_eq!(a, b);
    }

    #[test]
    fn migration_runs_from_version_zero_and_rejects_the_future() {
        let mut doc = MapDoc { format: 0, ..MapDoc::default() };
        migrate(&mut doc).unwrap();
        assert_eq!(doc.format, FORMAT_VERSION);
        doc.format = FORMAT_VERSION + 1;
        assert!(matches!(migrate(&mut doc), Err(ProjectError::FutureFormat(..))));
    }

    #[test]
    fn backups_rotate_on_autosave() {
        let mut p = MemoryProject::new();
        let mut doc = MapDoc::default();
        for stem in ["mp_rr_one", "mp_rr_two", "mp_rr_three", "mp_rr_four"] {
            doc.meta.stem = stem.into();
            autosave(&mut p, &doc).unwrap();
        }
        assert!(!p.exists(&backup_name(4)));
        let stem_at = |index: usize| {
            deserialize(&String::from_utf8(p.read(&backup_name(index)).unwrap()).unwrap()).unwrap().meta.stem
        };
        assert_eq!(stem_at(3), "mp_rr_one");
        assert_eq!(stem_at(2), "mp_rr_two");
        assert_eq!(newest_backup(&p).unwrap().meta.stem, "mp_rr_three");
        let live = deserialize(&String::from_utf8(p.read(MAP_FILE).unwrap()).unwrap()).unwrap();
        assert_eq!(live.meta.stem, "mp_rr_four");
    }

    #[test]
    fn plain_save_leaves_backups_alone() {
        let mut p = MemoryProject::new();
        let mut doc = MapDoc::default();
        doc.meta.stem = "mp_rr_a".into();
        autosave(&mut p, &doc).unwrap();
        doc.meta.stem = "mp_rr_b".into();
        save(&mut p, &doc).unwrap();
        assert!(!p.exists(&backup_name(1)));
    }

    #[test]
    fn load_reads_a_saved_document_and_its_files() {
        let mut p = MemoryProject::new();
        let mut doc = MapDoc::default();
        doc.meta.stem = "mp_rr_x".into();
        save(&mut p, &doc).unwrap();
        p.write("terrain/h.f32", b"abc").unwrap();
        let mut d = load_to_memory(&p).unwrap();
        assert_eq!(d.doc.meta.stem, "mp_rr_x");
        assert_eq!(d.files.read("terrain/h.f32").unwrap(), b"abc");
        d.apply(Command::SetFile { path: "terrain/h.f32".into(), bytes: Some(b"zz".to_vec()) }).unwrap();
        assert_eq!(d.files.read("terrain/h.f32").unwrap(), b"zz");
        d.undo().unwrap();
        assert_eq!(d.files.read("terrain/h.f32").unwrap(), b"abc");
    }

    #[test]
    fn a_file_command_rejects_an_unsafe_path() {
        let mut p = MemoryProject::new();
        assert!(save(&mut p, &MapDoc::default()).is_ok());
        let mut d = load_to_memory(&p).unwrap();
        assert!(matches!(
            d.apply(Command::SetFile { path: "../escape".into(), bytes: None }),
            Err(DocError::BadPath(_))
        ));
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_project_round_trips() {
        let dir = std::env::temp_dir().join(format!("r5ed-proj-{}", std::process::id()));
        let mut p = NativeProject::new(&dir).unwrap();
        let doc = MapDoc::default();
        save(&mut p, &doc).unwrap();
        p.write("meshes/a.glb", b"glb").unwrap();
        assert_eq!(p.list().unwrap(), vec!["map.json".to_string(), "meshes/a.glb".to_string()]);
        let mut d = load(Box::new(NativeProject::new(&dir).unwrap())).unwrap();
        assert_eq!(d.doc.format, FORMAT_VERSION);
        assert_eq!(d.files.read("meshes/a.glb").unwrap(), b"glb");
        assert!(d.apply(Command::SetFile { path: "../x".into(), bytes: None }).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
