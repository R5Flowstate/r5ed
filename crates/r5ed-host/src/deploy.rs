//! Deploying compiled output into the game install, and taking it back out.
//!
//! Three rules make this safe to point at a real install: only the six
//! directories a map's output legitimately lives in are writable, every file
//! about to be replaced is copied into the cache first, and a file the host did
//! not write is never replaced unless the job says so explicitly. The manifest
//! records every file a deploy touched, so an undeploy restores exactly those.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ApiError, Result};
use crate::jobs::LogBuffer;
use crate::paths;

/// The only directories under the game root a deploy may write to.
pub const ALLOWED: [&str; 6] = [
    "maps",
    "vpk",
    "paks/Win64",
    "paks/Win64_server",
    "platform/scripts",
    "platform/cfg",
];

/// Where the build output of a project lives, relative to the project.
const BUILD_DIR: &str = "build";

/// One file a deploy wrote, and what was there before it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrittenFile {
    /// Path under the game root, forward slashed.
    pub target: String,
    /// The backup, relative to the backup root, when a file was replaced.
    pub backup: Option<String>,
    /// True when the target did not exist before, so undeploy deletes it.
    pub created: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Milliseconds since the epoch; also the manifest's file name.
    pub stamp: u64,
    pub project: String,
    pub files: Vec<WrittenFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeployReport {
    pub manifest: String,
    pub written: Vec<WrittenFile>,
}

/// True when `rel` is a deploy target: a forward-slashed path under one of the
/// six allowed directories, with nothing above it.
pub fn target_allowed(rel: &str) -> bool {
    let rel = rel.replace('\\', "/");
    if rel.is_empty() || rel.starts_with('/') {
        return false;
    }
    if rel.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        return false;
    }
    if paths::relative_path(&rel).is_err() {
        return false;
    }
    ALLOWED.iter().any(|a| rel == *a || rel.starts_with(&format!("{a}/")))
}

fn target_path(game_root: &Path, rel: &str) -> Result<PathBuf> {
    if !target_allowed(rel) {
        return Err(ApiError::bad(format!(
            "{rel:?} is not under a deployable directory; allowed: {}",
            ALLOWED.join(", ")
        )));
    }
    paths::contained(game_root, &rel.replace('\\', "/"))
}

fn backup_root(cache_root: &Path, stamp: u64) -> PathBuf {
    cache_root.join("backups").join(stamp.to_string())
}

fn manifest_path(cache_root: &Path, stamp: u64) -> PathBuf {
    cache_root.join("deploy").join(format!("{stamp}.json"))
}

/// A short, filesystem-safe id for a manifest so it can be named in a URL.
/// It is the deploy's own stamp, so the id and the file name always agree.
pub fn manifest_id(stamp: u64) -> String {
    stamp.to_string()
}

fn write_manifest(cache_root: &Path, m: &Manifest) -> Result<()> {
    let p = manifest_path(cache_root, m.stamp);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| ApiError::Internal(format!("{}: {e}", d.display())))?;
    }
    let body = serde_json::to_string_pretty(m).map_err(|e| ApiError::Internal(e.to_string()))?;
    std::fs::write(&p, body).map_err(|e| ApiError::Internal(format!("{}: {e}", p.display())))
}

pub fn read_manifest(cache_root: &Path, id: &str) -> Result<Manifest> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || id.len() > 20 {
        return Err(ApiError::bad("a manifest id is a timestamp"));
    }
    let p = manifest_path(cache_root, id.parse::<u64>().map_err(|_| ApiError::bad("bad manifest id"))?);
    let text = std::fs::read_to_string(&p)
        .map_err(|_| ApiError::not_found(format!("no deploy manifest {id}")))?;
    serde_json::from_str(&text).map_err(|e| ApiError::Internal(e.to_string()))
}

/// Copies `files` from the project's build directory into the install.
///
/// Every compiled `.bsp` is read back through the container reader first, so a
/// malformed map can never reach the install even if the compiler produced it.
pub fn deploy(
    env: &crate::jobs::Env,
    project: &str,
    files: &[String],
    replace_shipped: bool,
    log: &LogBuffer,
) -> Result<DeployReport> {
    if files.is_empty() {
        return Err(ApiError::bad("a deploy job must name at least one file"));
    }
    if files.len() > 4096 {
        return Err(ApiError::bad("a deploy job may name at most 4096 files"));
    }
    let build = env.project_dir(project)?.join(BUILD_DIR);
    if !build.is_dir() {
        return Err(ApiError::not_found(format!("{} has no build output", project)));
    }
    // Validate the whole set before touching anything, so a bad name cannot
    // leave a half-finished deploy behind.
    let mut sources = Vec::with_capacity(files.len());
    for f in files {
        let src = paths::contained(&build, f)?;
        if !src.is_file() {
            return Err(ApiError::not_found(format!("{f} is not in the build output")));
        }
        target_path(&env.game_root, f)?;
        if f.ends_with(".bsp") {
            validate_bsp(&src)?;
        }
        sources.push((src, f.clone()));
    }

    // The stamp names the manifest and the backup directory, so two deploys in
    // the same millisecond must not share one: the second would overwrite the
    // first's manifest and lose the backups an undeploy restores.
    let mut stamp = crate::jobs::now_ms();
    while manifest_path(&env.cache_root, stamp).exists() || backup_root(&env.cache_root, stamp).exists() {
        stamp += 1;
    }
    let backups = backup_root(&env.cache_root, stamp);
    let mut written = Vec::with_capacity(sources.len());
    for (src, rel) in sources {
        let dst = target_path(&env.game_root, &rel)?;
        let existing = dst.is_file();
        let backup = if existing {
            if !replace_shipped && !wrote_before(&env.cache_root, &rel) {
                return Err(ApiError::Conflict(format!(
                    "{rel} was not written by this host; pass replace_shipped to overwrite it"
                )));
            }
            let b = backups.join(&rel);
            if let Some(d) = b.parent() {
                std::fs::create_dir_all(d).map_err(|e| ApiError::Internal(format!("{}: {e}", d.display())))?;
            }
            std::fs::copy(&dst, &b)
                .map_err(|e| ApiError::Internal(format!("{}: {e}", dst.display())))?;
            Some(paths::wire_path(b.strip_prefix(&env.cache_root).unwrap_or(&b)))
        } else {
            None
        };
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d).map_err(|e| ApiError::Internal(format!("{}: {e}", d.display())))?;
        }
        std::fs::copy(&src, &dst)
            .map_err(|e| ApiError::Internal(format!("{}: {e}", dst.display())))?;
        log.push(format!("wrote {rel}"));
        written.push(WrittenFile { target: rel, backup, created: !existing });
    }
    let manifest = Manifest { stamp, project: project.to_string(), files: written.clone() };
    write_manifest(&env.cache_root, &manifest)?;
    // The id is the stamp, so it is what an undeploy job names.
    Ok(DeployReport { manifest: manifest_id(stamp), written })
}

/// True when some earlier manifest already lists this target, which is what
/// makes replacing a previous deploy of the same project allowed by default.
fn wrote_before(cache_root: &Path, rel: &str) -> bool {
    let dir = cache_root.join("deploy");
    let Ok(rd) = std::fs::read_dir(&dir) else { return false };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(m) = serde_json::from_str::<Manifest>(&text) else { continue };
        if m.files.iter().any(|f| f.target == rel) {
            return true;
        }
    }
    false
}

/// Restores one deploy: puts every replaced file back and deletes every file it
/// created. Returns how many entries it touched.
pub fn undeploy(env: &crate::jobs::Env, id: &str, log: &LogBuffer) -> Result<usize> {
    let m = read_manifest(&env.cache_root, id)?;
    let backups = backup_root(&env.cache_root, m.stamp);
    let mut n = 0;
    for f in &m.files {
        let dst = target_path(&env.game_root, &f.target)?;
        match (&f.backup, f.created) {
            (Some(b), _) => {
                let src = env.cache_root.join(b);
                if !src.is_file() {
                    return Err(ApiError::not_found(format!("backup for {} is gone", f.target)));
                }
                if let Some(d) = dst.parent() {
                    std::fs::create_dir_all(d).map_err(|e| ApiError::Internal(e.to_string()))?;
                }
                std::fs::copy(&src, &dst)
                    .map_err(|e| ApiError::Internal(format!("{}: {e}", dst.display())))?;
                log.push(format!("restored {}", f.target));
            }
            (None, true) => {
                if dst.is_file() {
                    std::fs::remove_file(&dst)
                        .map_err(|e| ApiError::Internal(format!("{}: {e}", dst.display())))?;
                    log.push(format!("removed {}", f.target));
                }
            }
            (None, false) => {}
        }
        n += 1;
    }
    let _ = std::fs::remove_dir_all(&backups);
    let _ = std::fs::remove_file(manifest_path(&env.cache_root, m.stamp));
    Ok(n)
}

/// Reads a compiled map through the container reader. Anything the reader
/// rejects, the deploy refuses.
pub fn validate_bsp(path: &Path) -> Result<()> {
    let bsp = rbsp::Bsp::read_path(path).map_err(|e| {
        ApiError::Internal(format!("{} did not read back as a map: {e}", paths::wire_path(path)))
    })?;
    let version = rbsp::Version::from_header(bsp.version()).ok_or_else(|| {
        ApiError::Internal(format!("{} has unknown map version {}", paths::wire_path(path), bsp.version()))
    })?;
    if !matches!(version, rbsp::Version::DediV47 | rbsp::Version::ClientV51) {
        return Err(ApiError::Internal(format!(
            "{} is map version {}, which this build does not emit",
            paths::wire_path(path),
            version.number()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::Env;

    fn env(tag: &str) -> Env {
        let base = std::env::temp_dir().join(format!("r5ed-deploy-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let e = Env {
            game_root: base.join("game"),
            projects_root: base.join("projects"),
            cache_root: base.join("cache"),
        };
        std::fs::create_dir_all(&e.game_root).unwrap();
        std::fs::create_dir_all(&e.projects_root).unwrap();
        std::fs::create_dir_all(&e.cache_root).unwrap();
        e
    }

    #[test]
    fn only_the_six_documented_directories_are_targets() {
        for ok in [
            "maps/mp_test.bsp",
            "vpk/mp_test_dir.vpk",
            "paks/Win64/mp_test.bsp",
            "paks/Win64_server/mp_test.bsp",
            "platform/scripts/foo.nut",
            "platform/cfg/foo.cfg",
        ] {
            assert!(target_allowed(ok), "{ok}");
        }
        for bad in [
            "",
            "/maps/x",
            "../maps/x",
            "maps/../../etc/passwd",
            "paks/x.rpak",
            "bin/r5apex.exe",
            "maps/./x",
            "maps//x",
            "C:/maps/x",
        ] {
            assert!(!target_allowed(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_manifest_id_is_a_timestamp() {
        let e = env("id");
        assert!(read_manifest(&e.cache_root, "abc").is_err());
        assert!(read_manifest(&e.cache_root, "../1").is_err());
        assert!(read_manifest(&e.cache_root, "99999999999999999999").is_err());
        assert!(read_manifest(&e.cache_root, "1").is_err());
    }

    #[test]
    fn a_malformed_bsp_is_refused_before_anything_is_written() {
        let e = env("bsp");
        let proj = e.project_dir("m").unwrap();
        let build = proj.join("build");
        std::fs::create_dir_all(build.join("maps")).unwrap();
        std::fs::write(build.join("maps/mp_test.bsp"), b"not a map at all").unwrap();
        std::fs::create_dir_all(e.game_root.join("maps")).unwrap();
        std::fs::write(e.game_root.join("maps/mp_test.bsp"), b"shipped").unwrap();
        let log = LogBuffer::default();
        let r = deploy(&e, "m", &["maps/mp_test.bsp".into()], true, &log);
        assert!(r.is_err(), "{r:?}");
        assert_eq!(std::fs::read(e.game_root.join("maps/mp_test.bsp")).unwrap(), b"shipped");
        let _ = std::fs::remove_dir_all(e.game_root.parent().unwrap());
    }
}
