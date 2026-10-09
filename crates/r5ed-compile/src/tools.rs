//! Finding the external tools and the game install.
//!
//! A tool is named by an environment variable first, then looked for beside
//! this executable, then on the search path. Nothing here ever runs a tool: the
//! caller gets a path it can hand to a process with a fixed argument vector.

use std::path::{Path, PathBuf};

/// Environment variable and file name of each tool, in that order.
pub const RSX: (&str, &str) = ("R5ED_RSX", "rsx.exe");
/// The pak packer.
pub const REPAK: (&str, &str) = ("R5ED_REPAK", "repak.exe");
/// The dedicated-server archive packer.
pub const REVPK: (&str, &str) = ("R5ED_REVPK", "revpk.exe");
/// The game install root.
pub const GAME_ROOT: &str = "R5ED_GAME_ROOT";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{name} names {path:?}, which is not a file")]
    NotAFile { name: &'static str, path: PathBuf },
    #[error("{name} is not set and {file} is not beside this executable or on the search path")]
    NotFound { name: &'static str, file: &'static str },
    #[error("no game install: set {GAME_ROOT} to a directory that has paks\\Win64")]
    NoInstall,
}

/// The resolution order, exposed so it can be tested without touching the
/// process environment.
pub fn resolve(
    from_env: Option<PathBuf>,
    exe_dir: Option<&Path>,
    file: &str,
    on_path: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(p) = from_env {
        return p.is_file().then_some(p);
    }
    if let Some(d) = exe_dir {
        let beside = d.join(file);
        if beside.is_file() {
            return Some(beside);
        }
    }
    on_path.filter(|p| p.is_file())
}

/// The directory this executable runs from, when the platform reports one.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf))
}

/// A tool by name, resolved through the environment, this directory and the
/// search path.
pub fn tool(spec: (&'static str, &'static str)) -> Result<PathBuf, Error> {
    let (name, file) = spec;
    let from_env = std::env::var_os(name).map(PathBuf::from);
    if let Some(p) = &from_env {
        if !p.is_file() {
            return Err(Error::NotAFile { name, path: p.clone() });
        }
    }
    let on_path = which(file);
    resolve(from_env, exe_dir().as_deref(), file, on_path)
        .ok_or(Error::NotFound { name, file })
}

/// The first executable of that name on the search path.
pub fn which(file: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The game install root, when the environment names one that has client paks.
pub fn game_root() -> Result<PathBuf, Error> {
    let root = std::env::var_os(GAME_ROOT).map(PathBuf::from).ok_or(Error::NoInstall)?;
    if root.join("paks").join("Win64").is_dir() {
        Ok(root)
    } else {
        Err(Error::NoInstall)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("r5ed-tools-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_environment_comes_first_then_this_directory_then_the_path() {
        let dir = temp();
        let beside = dir.join("rsx.exe");
        std::fs::write(&beside, b"x").unwrap();
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let elsewhere = other.join("rsx.exe");
        std::fs::write(&elsewhere, b"x").unwrap();
        // The environment wins when it names a file.
        assert_eq!(resolve(Some(beside.clone()), Some(&other), "rsx.exe", Some(elsewhere.clone())), Some(beside.clone()));
        // Then this executable's own directory.
        assert_eq!(resolve(None, Some(&other), "rsx.exe", Some(elsewhere.clone())), Some(elsewhere.clone()));
        // Then the search path.
        assert_eq!(resolve(None, Some(Path::new("no-such-dir")), "rsx.exe", Some(elsewhere.clone())), Some(elsewhere.clone()));
        // An environment value that is not a file is not silently ignored.
        assert_eq!(resolve(Some(dir.join("gone.exe")), Some(&other), "rsx.exe", Some(elsewhere)), None);
        // Nothing anywhere is a miss, not a guess.
        assert_eq!(resolve(None, None, "rsx.exe", None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_named_tool_that_is_missing_says_which_one() {
        // Nothing in this environment names these, and the search path has no
        // such file under a name the test controls.
        match tool(("R5ED_CLI_MISSING_TOOL", "r5ed-cli-missing-tool.exe")) {
            Err(Error::NotFound { name, file }) => {
                assert_eq!(name, "R5ED_CLI_MISSING_TOOL");
                assert_eq!(file, "r5ed-cli-missing-tool.exe");
            }
            other => panic!("expected a miss, got {other:?}", other = other.is_ok()),
        }
    }
}
