//! Host configuration, discovered rather than compiled in.
//!
//! The file lives in the user's application data directory and is written on
//! first run. Detection walks the environment, then `PATH`, then the directory
//! the host executable is in, so a build that is moved between machines still
//! finds the tools without carrying one machine's layout in the source.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ApiError, Result};

pub const DEFAULT_PORT: u16 = 7797;

/// The directory the config and its caches live in.
pub fn app_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        return PathBuf::from(d).join("r5ed");
    }
    if let Some(h) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h).join(".config").join("r5ed");
    }
    std::env::temp_dir().join("r5ed")
}

pub fn config_path() -> PathBuf {
    app_dir().join("host.json")
}

/// One external tool. `None` means detection did not find it, which is not a
/// startup failure: only the jobs that need it become unavailable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    pub rsx: Option<PathBuf>,
    pub repak: Option<PathBuf>,
    pub revpk: Option<PathBuf>,
    pub recast: Option<PathBuf>,
    pub compile: Option<PathBuf>,
}

impl Tools {
    /// A tool is usable when it is configured and the file is there.
    pub fn usable(p: &Option<PathBuf>) -> bool {
        p.as_ref().is_some_and(|p| p.is_file())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub game_root: PathBuf,
    pub projects_root: PathBuf,
    pub cache_root: PathBuf,
    pub port: u16,
    pub tools: Tools,
}

impl Default for Config {
    fn default() -> Self {
        let home = app_dir();
        Self {
            game_root: PathBuf::new(),
            projects_root: home.join("projects"),
            cache_root: home.join("cache"),
            port: DEFAULT_PORT,
            tools: Tools::default(),
        }
    }
}

/// The file name each tool is known by.
const TOOL_NAMES: [(&str, &str, &str); 5] = [
    ("rsx", "R5ED_RSX", "rsx.exe"),
    ("repak", "R5ED_REPAK", "repak.exe"),
    ("revpk", "R5ED_REVPK", "revpk.exe"),
    ("recast", "R5ED_RECAST", "recast.exe"),
    ("compile", "R5ED_COMPILE", "r5ed-compile.exe"),
];

/// Every directory a bare tool name is looked up in, in order.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.to_path_buf());
            // The compiler CLI is built next to the host, and the test helper
            // tools live one directory further out.
            if let Some(up) = d.parent() {
                dirs.push(up.to_path_buf());
            }
        }
    }
    dirs
}

fn which(name: &str) -> Option<PathBuf> {
    search_dirs().into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Finds a tool: the environment variable first, then the search path, then a
/// sibling of the host executable.
pub fn detect(env_var: &str, file_name: &str) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(env_var).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
    }
    which(file_name)
}

/// A game root is a directory that has an executable and a pak tree.
pub fn looks_like_game_root(p: &Path) -> bool {
    p.join("paks").is_dir() && (p.join("r5apex.exe").is_file() || p.join("r5apex_ds.exe").is_file() || p.join("platform").is_dir())
}

/// Searches the usual install roots and the environment for a game install.
pub fn detect_game_root() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("R5ED_GAME_ROOT").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        if looks_like_game_root(&p) {
            return Some(p);
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            let p = PathBuf::from(v);
            roots.push(p.join("Steam").join("steamapps").join("common").join("Apex"));
            roots.push(p.join("EA Games").join("Apex"));
        }
    }
    if let Some(d) = search_dirs().into_iter().find(|d| d.join("paks").is_dir()) {
        roots.push(d);
    }
    roots.into_iter().find(|p| looks_like_game_root(p))
}

impl Config {
    /// Fills in every field detection can supply, leaving the rest as they are.
    pub fn detect(&mut self) {
        if self.game_root.as_os_str().is_empty() {
            self.game_root = detect_game_root().unwrap_or_default();
        }
        for (slot, env, name) in TOOL_NAMES {
            let target = match slot {
                "rsx" => &mut self.tools.rsx,
                "repak" => &mut self.tools.repak,
                "revpk" => &mut self.tools.revpk,
                "recast" => &mut self.tools.recast,
                _ => &mut self.tools.compile,
            };
            if target.is_none() {
                *target = detect(env, name);
            }
        }
    }

    /// Reads the config, creating it with detected defaults when it is absent.
    /// An existing file is never rewritten: the user owns it.
    pub fn load_or_create() -> Result<Self> {
        let path = config_path();
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| ApiError::Internal(format!("{}: {e}", path.display())))?;
            let mut cfg: Config = serde_json::from_str(&text)
                .map_err(|e| ApiError::Internal(format!("{}: {e}", path.display())))?;
            cfg.detect();
            cfg.validate()?;
            return Ok(cfg);
        }
        let mut cfg = Config::default();
        cfg.detect();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| ApiError::Internal(format!("{}: {e}", d.display())))?;
        }
        let body = serde_json::to_string_pretty(&cfg)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        std::fs::write(&path, body).map_err(|e| ApiError::Internal(format!("{}: {e}", path.display())))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Fails loudly on a config that could not possibly work.
    pub fn validate(&self) -> Result<()> {
        if self.port == 0 {
            return Err(ApiError::Internal("port 0 is not bindable".into()));
        }
        if self.projects_root.as_os_str().is_empty() || self.cache_root.as_os_str().is_empty() {
            return Err(ApiError::Internal("projects_root and cache_root are required".into()));
        }
        if self.game_root.as_os_str().is_empty() {
            return Err(ApiError::Internal(
                "game_root is not set; edit the config to point at the install".into(),
            ));
        }
        if !self.game_root.is_dir() {
            return Err(ApiError::Internal(format!(
                "game_root {} is not a directory",
                self.game_root.display()
            )));
        }
        for d in [&self.projects_root, &self.cache_root] {
            std::fs::create_dir_all(d).map_err(|e| ApiError::Internal(format!("{}: {e}", d.display())))?;
        }
        Ok(())
    }

    /// The compiler CLI, which every compile-shaped job needs.
    pub fn compile_tool(&self) -> Result<PathBuf> {
        if Tools::usable(&self.tools.compile) {
            return Ok(self.tools.compile.clone().expect("checked above"));
        }
        Err(ApiError::Unavailable("the compiler CLI is not configured".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_config_serialises_and_reloads_with_the_same_shape() {
        let mut c = Config::default();
        c.game_root = PathBuf::from("/games/apex");
        let body = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&body).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.port, DEFAULT_PORT);
    }

    #[test]
    fn a_config_missing_fields_keeps_the_defaults() {
        let c: Config = serde_json::from_str(r#"{"port": 9000}"#).unwrap();
        assert_eq!(c.port, 9000);
        assert_eq!(c.projects_root, Config::default().projects_root);
        assert!(c.tools.rsx.is_none());
    }

    #[test]
    fn validation_rejects_an_unusable_config() {
        let mut c = Config::default();
        assert!(c.validate().is_err(), "no game root");
        c.game_root = PathBuf::from("/definitely/not/here");
        assert!(c.validate().is_err(), "game root is not a directory");
        c.port = 0;
        assert!(c.validate().is_err(), "port 0");
    }

    #[test]
    fn a_tool_is_only_usable_when_the_file_is_there() {
        assert!(!Tools::usable(&None));
        assert!(!Tools::usable(&Some(PathBuf::from("no/such/tool.exe"))));
        let here = std::env::current_exe().unwrap();
        assert!(Tools::usable(&Some(here)));
    }

    #[test]
    fn detection_prefers_the_environment_and_ignores_a_stale_value() {
        let here = std::env::current_exe().unwrap();
        let name = here.file_name().unwrap().to_string_lossy().into_owned();
        let key = TOOL_NAMES.iter().find(|(_, e, _)| name.contains(e)).map(|(_, e, _)| *e);
        if let Some(k) = key {
            assert_eq!(detect(k, &name), Some(here));
            std::env::set_var(k, "no/such/tool.exe");
            assert_ne!(detect(k, &name), Some(PathBuf::from("no/such/tool.exe")));
            std::env::remove_var(k);
        }
    }
}
