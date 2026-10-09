//! The file browser behind "open a map's rpak": folders, and the client paks a
//! map is imported from. Only a map's `*_perm.rpak` is listed; importing it
//! brings its `_temp` partner along.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{ApiError, Result};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    /// `dir` or `pak`.
    pub kind: &'static str,
    /// The map a pak imports as; empty for a folder.
    pub stem: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Listing {
    pub dir: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

/// The map a client pak belongs to, from its file name:
/// `mp_rr_x_client_perm.rpak` and `mp_rr_x_client_temp.rpak` are both `mp_rr_x`.
pub fn stem_of_pak(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    ["_client_perm.rpak", "_client_temp.rpak", "_perm.rpak", "_temp.rpak"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A path as the page sees it: forward slashes, and without the `//?/` prefix
/// a canonical Windows path carries.
fn wire(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    s.strip_prefix("//?/").map(str::to_string).unwrap_or(s)
}

/// One folder's subfolders and `*_perm.rpak` files, folders first, each group
/// sorted by name. Hidden entries and anything unreadable are skipped.
pub fn list(dir: &Path) -> Result<Listing> {
    let dir: PathBuf = dir.canonicalize().map_err(|e| ApiError::not_found(format!("{}: {e}", dir.display())))?;
    if !dir.is_dir() {
        return Err(ApiError::bad(format!("{} is not a folder", dir.display())));
    }
    let mut dirs = Vec::new();
    let mut paks = Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|e| ApiError::Internal(format!("{}: {e}", dir.display())))?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.starts_with('$') {
            continue;
        }
        let path = e.path();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            dirs.push(Entry { name, path: wire(&path), kind: "dir", stem: String::new() });
        } else if name.to_ascii_lowercase().ends_with("_perm.rpak") {
            let stem = stem_of_pak(&path).unwrap_or_default();
            paks.push(Entry { name, path: wire(&path), kind: "pak", stem });
        }
    }
    dirs.sort_by_key(|e| e.name.to_ascii_lowercase());
    paks.sort_by_key(|e| e.name.to_ascii_lowercase());
    dirs.extend(paks);
    Ok(Listing { dir: wire(&dir), parent: dir.parent().map(wire), entries: dirs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_shows_folders_and_perm_paks_only() {
        let root = std::env::temp_dir().join(format!("r5ed_browse_{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for f in ["mp_rr_x_client_perm.rpak", "mp_rr_x_client_temp.rpak", "common.rpak", "notes.txt"] {
            std::fs::write(root.join(f), b"").unwrap();
        }
        let l = list(&root).unwrap();
        let names: Vec<(&str, &str)> = l.entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
        assert_eq!(names, vec![("sub", "dir"), ("mp_rr_x_client_perm.rpak", "pak")]);
        assert_eq!(l.entries[1].stem, "mp_rr_x");
        assert!(l.parent.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pak_name_gives_its_map() {
        assert_eq!(stem_of_pak(Path::new("mp_rr_x_client_perm.rpak")).as_deref(), Some("mp_rr_x"));
        assert_eq!(stem_of_pak(Path::new("MP_LOBBY_client_temp.rpak")).as_deref(), Some("mp_lobby"));
        assert_eq!(stem_of_pak(Path::new("common.rpak")), None);
    }
}
