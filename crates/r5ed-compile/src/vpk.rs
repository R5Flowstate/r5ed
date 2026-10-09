//! The dedicated-server VPK a map ships in.
//!
//! A dedicated map is a directory archive that names its contents and a data
//! archive that holds them, and it is packed from a workspace whose manifest
//! lists every file. The packer re-splits its own command line on spaces, so it
//! is run from the workspace with relative arguments, which is the only way it
//! reads them correctly.
//!
//! The manifest is written in the shape a shipped map's carries, one entry per
//! file with the same five fields, and it is read back with the engine's key
//! value grammar before anything is packed from it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rbsp::{Bsp, Layout};
use serde_json::{json, Value};


/// Locale a server map is filed under.
pub const LOCALE: &str = "english";
/// The context every dedicated map is packed in.
pub const CONTEXT: &str = "server";

/// The manifest entry fields a shipped map's directory archive carries, in the
/// order the packer reads them.
pub const ENTRY_FIELDS: [(&str, &str); 5] = [
    ("preloadSize", "0"),
    ("loadFlags", "257"),
    ("textureFlags", "0"),
    ("useCompression", "1"),
    ("deDuplicate", "1"),
];

/// The directory archive of a map.
pub fn dir_archive(stem: &str) -> String {
    format!("{LOCALE}{CONTEXT}_{stem}.bsp.pak000_dir.vpk")
}

/// The data archive of a map. A map big enough for more than one gets a second
/// with the same stem and a higher index.
pub fn data_archive(stem: &str, index: u32) -> String {
    format!("{CONTEXT}_{stem}.bsp.pak000_{index:03}.vpk")
}

/// The manifest file inside a workspace. It is the directory archive's name with
/// a text extension, which is where the packer looks for it.
pub fn manifest_name(stem: &str) -> String {
    format!("{LOCALE}{CONTEXT}_{stem}.txt")
}

/// The manifest a workspace carries, as the key values the packer reads.
///
/// A path in the manifest is relative with a single backslash: the packer looks
/// the file up under the workspace and the engine lowercases the lookup, so a
/// path that does not match the file on disk is a map that loads nothing.
pub fn manifest_text(files: &[String]) -> String {
    let mut lines = vec!["\"BuildManifest\"".to_string(), "{".to_string()];
    for file in files {
        lines.push(format!("\"maps\\{file}\""));
        lines.push("{".to_string());
        for (key, value) in ENTRY_FIELDS {
            lines.push(format!("\"{key}\"\"{value}\""));
        }
        lines.push("}".to_string());
    }
    lines.push("}".to_string());
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Every file a dedicated map ships: the container, its lumps and its entity
/// partitions, in the order the manifest names them.
pub fn map_files(bsp: &Bsp, ent_bytes: &std::collections::BTreeMap<String, Vec<u8>>, stem: &str) -> Result<Vec<String>> {
    if bsp.layout != Layout::Sidecar {
        bail!("a dedicated map has to be written as a header beside its lumps, not as one file");
    }
    let mut out = vec![format!("{stem}.bsp")];
    for id in bsp.lumps.keys() {
        // The engine lowercases the path it looks the lump up by, so a manifest
        // that names an uppercase sidecar never finds it.
        out.push(format!("{stem}.bsp.{id:04x}.bsp_lump"));
    }
    for name in crate::wraps::PARTITIONS {
        if ent_bytes.contains_key(name) {
            out.push(format!("{stem}_{name}.ent"));
        }
    }
    Ok(out)
}

/// A workspace, a manifest and the archives built out of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Built {
    pub dir: PathBuf,
    pub data: PathBuf,
    pub manifest: PathBuf,
    pub files: Vec<String>,
    pub bytes: u64,
}

/// Reads the manifest back with the packer's own shape: a quoted path line, then
/// the field block that describes it. The shipped manifests carry a single
/// backslash in a path, which the engine's key value grammar reads as an escape,
/// so this is deliberately not that grammar.
pub fn manifest_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with("\"maps\\") {
            continue;
        }
        let Some(rest) = t.strip_prefix('"') else { continue };
        let Some(end) = rest.rfind('"') else { continue };
        out.push(rest[..end].to_string());
    }
    out
}

/// Copies a dedicated map into a workspace and writes the manifest that names
/// it. The manifest is read back first, so a workspace that would pack nothing
/// never reaches the packer.
pub fn stage(
    bsp: &Bsp,
    ent_bytes: &std::collections::BTreeMap<String, Vec<u8>>,
    stem: &str,
    workspace: &Path,
) -> Result<Vec<String>> {
    let files = map_files(bsp, ent_bytes, stem)?;
    let maps = workspace.join("maps");
    let _ = std::fs::remove_dir_all(workspace);
    std::fs::create_dir_all(&maps).with_context(|| format!("creating {}", maps.display()))?;
    for file in &files {
        let (bytes, name) = if let Some(id) = lump_id(file) {
            (bsp.lump(id).to_vec(), file.clone())
        } else if file == &format!("{stem}.bsp") {
            (bsp.header.to_bytes(), file.clone())
        } else {
            let partition = file
                .rsplit_once('_')
                .map(|(_, tail)| tail.trim_end_matches(".ent").to_string())
                .filter(|p| ent_bytes.contains_key(p))
                .ok_or_else(|| anyhow::anyhow!("{file} names no partition of the map"))?;
            (ent_bytes.get(&partition).cloned().unwrap_or_default(), file.clone())
        };
        let path = maps.join(&name);
        std::fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    let text = manifest_text(&files);
    let named = manifest_paths(&text);
    if named != files.iter().map(|f| format!("maps\\{f}")).collect::<Vec<_>>() {
        bail!("the build manifest does not read back: {} of {} files named", named.len(), files.len());
    }
    for file in &files {
        if !maps.join(file).is_file() {
            bail!("the manifest names {file} and the workspace does not hold it");
        }
    }
    let manifest = workspace.join("manifest").join(manifest_name(stem));
    std::fs::create_dir_all(manifest.parent().unwrap_or(workspace))?;
    std::fs::write(&manifest, &text).with_context(|| format!("writing {}", manifest.display()))?;
    Ok(files)
}

/// The lump id a sidecar file name carries.
fn lump_id(name: &str) -> Option<u16> {
    let rest = name.split_once(".bsp.").map(|(_, tail)| tail)?;
    let hex = rest.split_once(".bsp_lump").map(|(h, _)| h)?;
    if hex.len() != 4 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    u16::from_str_radix(hex, 16).ok()
}

/// Unpacks a dedicated map's archives into `out`, which then holds `maps/`.
///
/// The unpacker writes into existing directories only and reads its output
/// path as a prefix, so `maps/` is made first and the path ends in a slash.
pub fn unpack(dir_vpk: &Path, out: &Path, revpk: &Path) -> Result<()> {
    std::fs::create_dir_all(out.join("maps")).with_context(|| format!("creating {}", out.display()))?;
    let target = format!("{}/", out.display()).replace('\\', "/");
    if target.contains(' ') || dir_vpk.display().to_string().contains(' ') {
        bail!("the unpacker splits its arguments on spaces: {} or {}", dir_vpk.display(), target);
    }
    let run = std::process::Command::new(revpk)
        .arg("-unpack")
        .arg(dir_vpk)
        .arg(&target)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| anyhow::anyhow!("could not start {}: {e}", revpk.display()))?;
    if !run.status.success() {
        bail!("{} exited with {}", revpk.display(), run.status);
    }
    Ok(())
}

/// Packs a workspace into the two archives a dedicated map ships in.
///
/// The packer re-splits its command line on spaces and takes a relative path for
/// the workspace and one for the output, so both are handed relative to a
/// directory that holds the workspace and the build tree side by side; nothing
/// it is given can therefore reach outside them.
pub fn pack(workspace: &Path, build: &Path, stem: &str, revpk: &Path) -> Result<Built> {
    std::fs::create_dir_all(build).with_context(|| format!("creating {}", build.display()))?;
    let parent = workspace
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent to run the packer from", workspace.display()))?;
    let workspace_rel = relative(parent, workspace)?;
    let build_rel = relative(parent, build)?;
    let out = std::process::Command::new(revpk)
        .args(["-pack", LOCALE, CONTEXT, stem])
        .arg(format!("{workspace_rel}/"))
        .arg(format!("{build_rel}/"))
        .args(["-1", "default"])
        .current_dir(parent)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| anyhow::anyhow!("could not start {}: {e}", revpk.display()))?;
    if !out.status.success() {
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        let tail: String = text.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("{} exited with {}: {}", revpk.display(), out.status, tail.trim());
    }
    let dir = build.join(dir_archive(stem));
    let data = build.join(data_archive(stem, 0));
    for archive in [&dir, &data] {
        if !archive.is_file() {
            bail!("the packer wrote no {}", archive.display());
        }
    }
    let bytes = std::fs::metadata(&dir).map(|m| m.len()).unwrap_or(0)
        + std::fs::metadata(&data).map(|m| m.len()).unwrap_or(0);
    Ok(Built {
        dir,
        data,
        manifest: workspace.join("manifest").join(manifest_name(stem)),
        files: std::fs::read_dir(workspace.join("maps"))
            .map(|rd| {
                rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        bytes,
    })
}

/// A path of `to` relative to `from`. A parent step is never returned, so
/// nothing the packer is handed can climb out of the directory it runs in.
fn relative(from: &Path, to: &Path) -> Result<String> {
    let to = to.canonicalize().with_context(|| format!("resolving {}", to.display()))?;
    let base = from.canonicalize().unwrap_or_else(|_| from.to_path_buf());
    let r = to
        .strip_prefix(&base)
        .map_err(|_| anyhow::anyhow!("{} is not under {}", to.display(), from.display()))?;
    Ok(r.to_string_lossy().replace('\\', "/"))
}

/// The report the caller prints.
pub fn report(built: &Built) -> Value {
    json!({
        "dir_vpk": built.dir.display().to_string(),
        "data_vpk": built.data.display().to_string(),
        "manifest": built.manifest.display().to_string(),
        "bytes": built.bytes,
        "files": built.files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn the_archive_names_are_the_ones_a_server_map_ships_under() {
        assert_eq!(dir_archive("mp_rr_x"), "englishserver_mp_rr_x.bsp.pak000_dir.vpk");
        assert_eq!(data_archive("mp_rr_x", 0), "server_mp_rr_x.bsp.pak000_000.vpk");
        assert_eq!(data_archive("mp_rr_x", 1), "server_mp_rr_x.bsp.pak000_001.vpk");
        assert_eq!(manifest_name("mp_rr_x"), "englishserver_mp_rr_x.txt");
    }

    #[test]
    fn a_manifest_names_every_file_with_the_five_fields_a_shipped_map_carries() {
        let text = manifest_text(&["mp_rr_x.bsp".into(), "mp_rr_x_script.ent".into()]);
        assert_eq!(
            text,
            "\"BuildManifest\"\n{\n\"maps\\mp_rr_x.bsp\"\n{\n\"preloadSize\"\"0\"\n\"loadFlags\"\"257\"\n\
             \"textureFlags\"\"0\"\n\"useCompression\"\"1\"\n\"deDuplicate\"\"1\"\n}\n\
             \"maps\\mp_rr_x_script.ent\"\n{\n\"preloadSize\"\"0\"\n\"loadFlags\"\"257\"\n\
             \"textureFlags\"\"0\"\n\"useCompression\"\"1\"\n\"deDuplicate\"\"1\"\n}\n}\n"
        );
        // The shipped manifests carry one backslash, which the engine's own key
        // value grammar reads as an escape, so the packer's shape is parsed here
        // rather than with that grammar.
        assert_eq!(
            manifest_paths(&text),
            vec!["maps\\mp_rr_x.bsp".to_string(), "maps\\mp_rr_x_script.ent".to_string()]
        );
        for (key, want) in ENTRY_FIELDS {
            assert_eq!(text.matches(&format!("\"{key}\"\"{want}\"")).count(), 2, "{key}");
        }
    }

    /// The engine lowercases the path it looks a lump up by, so an uppercase
    /// sidecar in the manifest is a lump the map never finds.
    #[test]
    fn the_sidecar_names_a_manifest_carries_are_lowercase() {
        assert_eq!(lump_id("mp_rr_x.bsp.004a.bsp_lump"), Some(0x4a));
        assert_eq!(lump_id("mp_rr_x.bsp.0000.bsp_lump"), Some(0));
        for bad in ["mp_rr_x.bsp.004A.bsp_lump", "mp_rr_x.bsp.4a.bsp_lump", "mp_rr_x.bsp.004a.txt", "mp_rr_x.bsp"] {
            assert_eq!(lump_id(bad), None, "{bad}");
        }
        let mut bsp = Bsp::new(47, 0, 0);
        bsp.layout = Layout::Sidecar;
        bsp.set_lump(0x0E, vec![1, 2]);
        bsp.set_lump(0x4A, vec![3]);
        let mut ents = BTreeMap::new();
        ents.insert("script".to_string(), b"{}".to_vec());
        let files = map_files(&bsp, &ents, "mp_rr_x").expect("lists");
        assert_eq!(
            files,
            vec!["mp_rr_x.bsp", "mp_rr_x.bsp.000e.bsp_lump", "mp_rr_x.bsp.004a.bsp_lump", "mp_rr_x_script.ent"]
        );
        for f in &files {
            if let Some(id) = lump_id(f) {
                assert!(rbsp::Bsp::sidecar_name_is_lowercase(f), "{f} would be missed by the engine");
                let _ = id;
            }
        }
    }

    #[test]
    fn a_map_written_as_one_file_is_refused_rather_than_packed_half() {
        let mut bsp = Bsp::new(47, 0, 0);
        bsp.layout = Layout::Monolithic;
        let err = map_files(&bsp, &BTreeMap::new(), "mp_rr_x").unwrap_err().to_string();
        assert!(err.contains("header beside its lumps"), "{err}");
    }

    #[test]
    fn a_workspace_holds_every_file_the_manifest_names() {
        let dir = std::env::temp_dir().join(format!("r5ed-vpk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut bsp = Bsp::new(47, 0, 0);
        bsp.layout = Layout::Sidecar;
        bsp.set_lump(0x0E, vec![1, 2, 3]);
        bsp.set_lump(0x00, vec![b'{', b'}']);
        let mut ents = BTreeMap::new();
        ents.insert("script".to_string(), b"{}".to_vec());
        ents.insert("nope".to_string(), b"x".to_vec());
        let files = stage(&bsp, &ents, "mp_rr_x", &dir.join("workspace")).expect("stages");
        assert_eq!(files, vec!["mp_rr_x.bsp", "mp_rr_x.bsp.0000.bsp_lump", "mp_rr_x.bsp.000e.bsp_lump", "mp_rr_x_script.ent"]);
        for f in &files {
            assert!(dir.join("workspace").join("maps").join(f).is_file(), "{f} was named but not written");
        }
        let manifest = dir.join("workspace/manifest/englishserver_mp_rr_x.txt");
        assert!(manifest.is_file(), "{}", manifest.display());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
