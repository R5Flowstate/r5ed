//! Project files: the document, the authored meshes and the build output.
//!
//! Reads and writes go through one containment check, and a listing never
//! follows a link out of the project.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ApiError, Result};
use crate::paths;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub name: String,
    pub modified: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRow {
    pub path: String,
    pub size: u64,
    pub modified: u64,
}

/// Milliseconds since the epoch, or zero when the platform cannot say.
pub fn modified_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

pub fn root_dir(projects_root: &Path, name: &str) -> Result<PathBuf> {
    paths::contained_dir(projects_root, name)
}

/// Every project directory under `projects_root`, newest first.
pub fn list(root: &Path) -> Result<Vec<ProjectSummary>> {
    let base = root
        .canonicalize()
        .map_err(|e| ApiError::NotFound(format!("{}: {e}", root.display())))?;
    let mut out = Vec::new();
    for e in std::fs::read_dir(&base).map_err(|e| ApiError::Internal(e.to_string()))?.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
        if paths::project_name(&name).is_err() {
            continue;
        }
        let dir = e.path();
        if !paths::is_under(&base, &dir) {
            continue;
        }
        out.push(ProjectSummary { name, modified: newest_mtime(&dir) });
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

fn newest_mtime(dir: &Path) -> u64 {
    let mut best = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            // A directory's own mtime moves whenever an entry is added, so it
            // is the file times that say when the project was last worked on.
            if md.is_dir() {
                best = best.max(newest_mtime(&e.path()));
            } else {
                best = best.max(modified_ms(&md));
            }
        }
    }
    best
}

/// Creates an empty project with the directories the format expects.
pub fn create(projects_root: &Path, name: &str) -> Result<ProjectSummary> {
    let dir = root_dir(projects_root, name)?;
    if dir.exists() {
        return Err(ApiError::conflict(format!("project {name} already exists")));
    }
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::Internal(format!("{}: {e}", dir.display())))?;
    for sub in ["meshes", "terrain", "prefabs", "bake", "build"] {
        std::fs::create_dir_all(dir.join(sub))
            .map_err(|e| ApiError::Internal(format!("{}: {e}", dir.display())))?;
    }
    let doc = dir.join("map.json");
    if !doc.exists() {
        std::fs::write(&doc, format!("{{\"version\":1,\"name\":\"{name}\",\"entities\":[]}}\n"))
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    Ok(ProjectSummary { name: name.to_string(), modified: newest_mtime(&dir) })
}

/// Recursively lists the project's own files, relative and forward-slashed.
pub fn files(dir: &Path) -> Result<Vec<FileRow>> {
    let base = dir
        .canonicalize()
        .map_err(|e| ApiError::NotFound(format!("{}: {e}", dir.display())))?;
    let mut out = Vec::new();
    walk(&base, &base, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<FileRow>) -> Result<()> {
    for e in std::fs::read_dir(dir).map_err(|e| ApiError::Internal(e.to_string()))?.flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if md.is_symlink() {
            // A link is not part of the project; listing it would leak whether
            // the target exists and let a reader follow it out of the tree.
            continue;
        }
        if md.is_dir() {
            walk(base, &p, out)?;
            continue;
        }
        let Ok(rel) = p.strip_prefix(base) else { continue };
        out.push(FileRow {
            path: paths::wire_path(rel),
            size: md.len(),
            modified: modified_ms(&md),
        });
    }
    Ok(())
}

pub fn read_file(dir: &Path, rel: &str) -> Result<Vec<u8>> {
    let p = paths::contained(dir, rel)?;
    if !p.is_file() {
        return Err(ApiError::not_found(format!("no file at {rel}")));
    }
    let md = std::fs::metadata(&p).map_err(|e| ApiError::Internal(e.to_string()))?;
    if md.len() as usize > crate::secure::MAX_FILE_BYTES {
        return Err(ApiError::BadRequest(format!("{rel} is larger than the file cap")));
    }
    std::fs::read(&p).map_err(|e| ApiError::Internal(format!("{}: {e}", p.display())))
}

/// Writes a file, creating the directories its path needs. An existing file is
/// replaced atomically so a crash cannot leave a half-written document.
pub fn write_file(dir: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() > crate::secure::MAX_FILE_BYTES {
        return Err(ApiError::BadRequest("file is larger than the file cap".into()));
    }
    let p = paths::contained(dir, rel)?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::Internal(format!("{}: {e}", parent.display())))?;
    }
    let tmp = p.with_extension("r5edtmp");
    std::fs::write(&tmp, bytes).map_err(|e| ApiError::Internal(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &p)
        .map_err(|e| ApiError::Internal(format!("{}: {e}", tmp.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("r5ed-host-projects-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_created_project_has_the_layout_the_format_documents() {
        let root = scratch("create");
        let p = create(&root, "map_one").unwrap();
        assert_eq!(p.name, "map_one");
        let dir = root_dir(&root, "map_one").unwrap();
        for f in ["map.json", "meshes", "terrain", "prefabs", "bake", "build"] {
            assert!(dir.join(f).exists(), "{f}");
        }
        assert!(create(&root, "map_one").is_err(), "a second create conflicts");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn listing_is_newest_first_and_skips_odd_names() {
        use std::time::{Duration, SystemTime};
        let root = scratch("list");
        create(&root, "alpha").unwrap();
        create(&root, "beta").unwrap();
        std::fs::create_dir_all(root.join("not a project")).unwrap();
        // The order is by the newest file anywhere in the tree, so the two
        // trees need mtimes that cannot land in the same millisecond.
        let now = SystemTime::now();
        let stamp = |name: &str, age: u64| {
            let f = std::fs::File::options()
                .write(true)
                .open(root.join(name).join("map.json"))
                .expect("open");
            f.set_modified(now - Duration::from_secs(age)).expect("set mtime");
        };
        stamp("alpha", 3600);
        stamp("beta", 60);
        let all = list(&root).unwrap();
        assert_eq!(all.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["beta", "alpha"]);
        assert!(all[0].modified > all[1].modified);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_round_trip_and_are_relative() {
        let root = scratch("files");
        create(&root, "m").unwrap();
        let dir = root_dir(&root, "m").unwrap();
        write_file(&dir, "meshes/a.glb", b"glb").unwrap();
        write_file(&dir, "map.json", b"{}").unwrap();
        assert_eq!(read_file(&dir, "meshes/a.glb").unwrap(), b"glb");
        let rows = files(&dir).unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"meshes/a.glb"), "{paths:?}");
        assert!(paths.contains(&"map.json"), "{paths:?}");
        assert!(rows.iter().all(|r| !r.path.contains('\\')));
        assert!(read_file(&dir, "../escape").is_err());
        assert!(write_file(&dir, "a/../../escape", b"x").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_write_larger_than_the_cap_is_refused() {
        let root = scratch("cap");
        create(&root, "m").unwrap();
        let dir = root_dir(&root, "m").unwrap();
        let big = vec![0u8; crate::secure::MAX_FILE_BYTES + 1];
        assert!(write_file(&dir, "big.bin", &big).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
