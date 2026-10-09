//! Turning untrusted strings into filesystem paths.
//!
//! Every path the host accepts arrives from a browser on a loopback socket, so
//! each one is checked component by component and then canonicalised and
//! prefix-checked against the root it is supposed to stay under. A junction or
//! a symlink inside a project therefore cannot be used to read or write
//! outside it.

use std::path::{Component, Path, PathBuf};

use crate::error::{ApiError, Result};

/// Windows refuses these as file names whatever the extension, so a name that
/// normalises onto one of them is a probe, not a project.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
    "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The whole project name grammar, in one place.
fn is_project_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !RESERVED.contains(&name.to_ascii_lowercase().as_str())
}

/// Checks a project name and returns it unchanged. The name becomes one
/// directory component, so nothing else needs rejecting.
pub fn project_name(name: &str) -> Result<&str> {
    if !is_project_name(name) {
        return Err(ApiError::bad("project name must be 1-64 characters of A-Z a-z 0-9 _ -"));
    }
    Ok(name)
}

/// Rejects a relative path that is not a plain descent under its root.
///
/// Percent decoding has already happened by the time a handler sees a query
/// value, so `%2e%2e` arrives as `..` and is caught here; the raw form never
/// reaches this function.
pub fn relative_path(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        return Err(ApiError::bad("empty path"));
    }
    if rel.contains('\0') {
        return Err(ApiError::bad("path holds a NUL byte"));
    }
    // A leading separator is an absolute path or a UNC share, and a drive
    // letter is a whole different volume. Neither is a descent under a root.
    if rel.starts_with('/') || rel.starts_with('\\') {
        return Err(ApiError::bad("path is absolute"));
    }
    if rel.as_bytes().get(1) == Some(&b':') {
        return Err(ApiError::bad("path holds a drive letter"));
    }
    let mut out = PathBuf::new();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(ApiError::bad("path escapes its root"));
        }
        // A colon is a drive letter, an alternate data stream, or a UNC share.
        if part.contains(':') {
            return Err(ApiError::bad("path holds a drive letter or stream name"));
        }
        let stem = part.split('.').next().unwrap_or(part);
        if RESERVED.contains(&stem.to_ascii_lowercase().as_str()) {
            return Err(ApiError::bad("path names a reserved device"));
        }
        if part.ends_with(' ') || part.ends_with('.') {
            return Err(ApiError::bad("path component ends in a space or a dot"));
        }
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        return Err(ApiError::bad("path names no file"));
    }
    Ok(out)
}

/// The absolute path of `rel` under `root`, with the result canonicalised and
/// proven to still be under `root`.
///
/// The canonical form of the root is taken as the boundary, so a symlink that
/// points outside is caught after resolution rather than before.
pub fn contained(root: &Path, rel: &str) -> Result<PathBuf> {
    let base = root
        .canonicalize()
        .map_err(|e| ApiError::NotFound(format!("{}: {e}", root.display())))?;
    let rel = relative_path(rel)?;
    let joined = base.join(&rel);
    // The deepest existing ancestor decides whether the leaf is reachable: a
    // path that does not exist yet is fine, a path that leaves the root is not.
    let mut probe = joined.as_path();
    let resolved = loop {
        match probe.canonicalize() {
            Ok(p) => break p,
            Err(_) => match probe.parent() {
                Some(p) => probe = p,
                None => return Err(ApiError::bad("path cannot be resolved")),
            },
        }
    };
    if !resolved.starts_with(&base) {
        return Err(ApiError::bad("path escapes its root"));
    }
    Ok(plain(&joined))
}

/// Joins `rel` under `root` and resolves symlinks, for a directory the host
/// is about to create rather than read.
pub fn contained_dir(root: &Path, name: &str) -> Result<PathBuf> {
    let name = project_name(name)?;
    let base = root
        .canonicalize()
        .map_err(|e| ApiError::NotFound(format!("{}: {e}", root.display())))?;
    let joined = base.join(name);
    if let Ok(resolved) = joined.canonicalize() {
        if !resolved.starts_with(&base) {
            return Err(ApiError::bad("path escapes its root"));
        }
    }
    Ok(plain(&joined))
}

/// Canonicalising on Windows hands back the verbatim `\\?\` form, which every
/// other tool on the machine was not written to expect. This turns one back into
/// the ordinary spelling: a drive path loses the prefix, and a share path goes
/// back to its `\\server\share` form.
pub fn plain(p: &Path) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    let Some(rest) = s.strip_prefix(r"\\?\") else { return p.to_path_buf() };
    if let Some(unc) = rest.strip_prefix(r"UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    if rest.starts_with('.') {
        // A device namespace path, which only the kernel understands.
        return p.to_path_buf();
    }
    PathBuf::from(rest)
}

/// True when `child` is `parent` or lives under it, both canonicalised.
pub fn is_under(parent: &Path, child: &Path) -> bool {
    match (parent.canonicalize(), child.canonicalize()) {
        (Ok(p), Ok(c)) => c.starts_with(p),
        _ => false,
    }
}

/// Normalises a path for the wire: forward slashes, no leading `./`.
pub fn wire_path(p: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_are_a_closed_grammar() {
        for ok in ["a", "map_1", "A-b-9", &"x".repeat(64)] {
            assert!(project_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "..", "a/b", "a\\b", "a.b", "a b", "CON", "nul.txt", "a:b", &"x".repeat(65)] {
            assert!(project_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn relative_paths_reject_every_escape_shape() {
        for bad in [
            "", ".", "..", "../x", "a/../../b", "a\\..\\..\\b", "C:\\Windows", "c:x", "a:b",
            "\\\\server\\share", "//server/share", "a/./../b", "\0", "a/CON", "con.txt", "a.",
            "a ", "a/b\\..\\..\\c",
        ] {
            assert!(relative_path(bad).is_err(), "{bad:?} should be rejected");
        }
        assert_eq!(relative_path("a/b/c.txt").unwrap(), PathBuf::from("a").join("b").join("c.txt"));
        assert_eq!(relative_path("a\\b").unwrap(), PathBuf::from("a").join("b"));
        assert_eq!(relative_path("./a/./b").unwrap(), PathBuf::from("a").join("b"));
    }

    #[test]
    fn the_verbatim_prefix_never_reaches_a_command_line() {
        assert_eq!(plain(Path::new(r"\\?\C:\games\apex")), PathBuf::from(r"C:\games\apex"));
        assert_eq!(plain(Path::new(r"\\?\UNC\srv\share\map.bsp")), PathBuf::from(r"\\srv\share\map.bsp"));
        assert_eq!(plain(Path::new(r"\\.\PIPE\x")), PathBuf::from(r"\\.\PIPE\x"));
        assert_eq!(plain(Path::new(r"C:\games\apex")), PathBuf::from(r"C:\games\apex"));
        // A canonicalised directory comes back in the spelling a tool expects.
        let dir = std::env::temp_dir().join("r5ed-plain");
        std::fs::create_dir_all(&dir).unwrap();
        let got = contained_dir(&dir, "m").unwrap();
        assert!(!got.to_string_lossy().starts_with(r"\\?\"), "{}", got.display());
        assert!(got.starts_with(plain(&dir.canonicalize().unwrap())), "{}", got.display());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn containment_holds_for_a_real_tree() {
        let dir = std::env::temp_dir().join("r5ed-paths");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("p").join("meshes")).unwrap();
        // The result is under the canonical root, whatever shape the root was
        // handed in.
        let root = plain(&dir.canonicalize().unwrap().join("p"));
        let p = contained(&dir.join("p"), "meshes/a.glb").unwrap();
        assert_eq!(p.file_name().unwrap(), "a.glb");
        assert!(p.starts_with(&root), "{} not under {}", p.display(), root.display());
        assert!(contained(&dir.join("p"), "../p2/x").is_err());
        assert!(contained(&dir.join("p"), "meshes/../../p2/x").is_err());
        assert!(contained(&dir.join("p"), "/meshes/a.glb").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
