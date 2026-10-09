//! `register`: the files that make a launcher and a playlist offer the map.
//!
//! Two things have to name a map before it appears in a picker: the line the
//! launcher's allowlist file carries, and a playlist entry that maps the stem
//! onto a mode. Both are written here next to the build output, together with a
//! manifest that names the stem and every file the map ships as, which is what a
//! deploy places.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use rdoc::project::{self, NativeProject};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::kv::{self, Node};

/// Everything `register` takes.
#[derive(Clone, Debug)]
pub struct Args {
    pub project: PathBuf,
    /// Where the registration files are written; defaults to
    /// `<project>/build/deploy`.
    pub out: Option<PathBuf>,
    /// A game install whose launcher catalog is merged with this map.
    pub install: Option<PathBuf>,
}

/// The playlist a new map inherits from when the document names none.
pub const DEFAULT_BASE: &str = "survival_dev";
/// Column the stem is padded to in the launcher allowlist.
const NAME_COLUMN: usize = 33;
/// Only directories a deploy may write to are named by the manifest.
const DEPLOYABLE: [&str; 6] = ["maps", "vpk", "paks/Win64", "paks/Win64_server", "platform/scripts", "platform/cfg"];

/// The registration manifest, as written to disk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub stem: String,
    pub display_name: String,
    /// Playlists the document puts the map in.
    pub playlists: Vec<String>,
    /// The line the launcher allowlist needs.
    pub map_names: String,
    /// The playlist patch, by file name.
    pub playlist_patch: String,
    /// Every file the map ships, relative to the build tree.
    pub files: Vec<File>,
}

/// One file the map ships.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct File {
    pub path: String,
    pub bytes: u64,
}

/// A stem the launcher will offer: `mp_rr_*` or the lobby.
pub fn launcher_stem_ok(stem: &str) -> bool {
    let well_formed = |s: &str| {
        s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    (stem.starts_with("mp_rr_") && stem.len() > 6 && well_formed(stem)) || stem == "mp_lobby"
}

/// The allowlist line for a stem and name.
pub fn map_names_line(stem: &str, display: &str) -> String {
    format!("{stem:<NAME_COLUMN$} = {display}")
}

/// The line endings a file mostly uses, so a merge does not mix them.
fn dominant_eol(text: &str) -> &'static str {
    match (text.matches("\r\n").count(), text.matches('\n').count()) {
        (crlf, lf) if crlf > lf - crlf => "\r\n",
        _ => "\n",
    }
}

/// The whitespace a line starts with.
fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// One level of indent, taken from the file so a merge does not mix tabs and
/// spaces in a document a human also edits.
fn indent_unit(text: &str) -> &'static str {
    if text.lines().any(|l| l.starts_with('\t')) {
        "\t"
    } else {
        "    "
    }
}

/// True when some line already names the stem as a map.
fn declared(text: &str, stem: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim();
        t == stem || t.starts_with(&format!("{stem} ")) || t.starts_with(&format!("{stem}\t"))
    })
}

/// The stem a launcher catalog already declares, as a map of a gamemode.
///
/// The launcher's picker offers a stem only when a playlist declares it, its
/// files are on disk, and it is named, so a map that is not in a `maps{}` block
/// never appears however complete its paks are. An existing declaration is left
/// alone, which makes this safe to run on every build.
pub fn declare_in_patch(text: &str, stem: &str) -> Result<String> {
    if !launcher_stem_ok(stem) {
        bail!("{stem:?} is not a stem the launcher offers");
    }
    if declared(text, stem) {
        return Ok(text.to_string());
    }
    let eol = dominant_eol(text);
    let unit = indent_unit(text);
    let lines: Vec<&str> = text.lines().collect();
    let next_content = |from: usize| (from..lines.len()).find(|i| !lines[*i].trim().is_empty());

    // An empty `maps` block takes the stem without inventing structure.
    for i in 0..lines.len() {
        if lines[i].trim() != "maps" {
            continue;
        }
        let Some(open) = next_content(i + 1) else { continue };
        if lines[open].trim() != "{" {
            continue;
        }
        let Some(close) = next_content(open + 1) else { continue };
        if lines[close].trim() != "}" {
            continue;
        }
        let pad = format!("{}{unit}", leading_ws(lines[i]));
        let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        out.insert(open + 1, format!("{pad}{stem} 1"));
        return Ok(out.join(eol) + eol);
    }

    // Otherwise the first gamemode grows one, since a `maps` block only means
    // anything under a gamemode.
    for i in 0..lines.len() {
        if lines[i].trim() != "Gamemodes" {
            continue;
        }
        let Some(open) = next_content(i + 1) else { continue };
        if lines[open].trim() != "{" {
            continue;
        }
        let depth = leading_ws(lines[i]).chars().count();
        let Some(mode) = next_content(open + 1) else { continue };
        if lines[mode].trim() == "}" || leading_ws(lines[mode]).chars().count() <= depth {
            continue;
        }
        let Some(mode_open) = next_content(mode + 1) else { continue };
        if lines[mode_open].trim() != "{" {
            continue;
        }
        let base = format!("{}{unit}", leading_ws(lines[mode]));
        let inner = format!("{base}{unit}");
        let block = [
            format!("{base}maps"),
            format!("{base}{{"),
            format!("{inner}{stem} 1"),
            format!("{base}}}"),
        ];
        let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        for (n, l) in block.iter().enumerate() {
            out.insert(mode_open + 1 + n, l.clone());
        }
        return Ok(out.join(eol) + eol);
    }

    bail!("the playlist patch has neither a maps block nor a Gamemodes block, so {stem} cannot be declared in it")
}

/// A stem's line in the WIP list, replacing its own line when it has one.
pub fn upsert_wip_line(text: &str, stem: &str, display: &str) -> String {
    let eol = dominant_eol(text);
    let line = map_names_line(stem, display);
    let mut hit = false;
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        let key = t.split('=').next().unwrap_or("").trim();
        if key == stem {
            if !hit {
                out.push(line.clone());
                hit = true;
            }
            continue;
        }
        out.push(l.to_string());
    }
    if !hit {
        if !out.is_empty() && !out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
            out.push(String::new());
        }
        out.push(line);
    }
    out.join(eol) + eol
}

/// The playlist patch that maps a stem onto a mode.
pub fn playlist_patch(entries: &[(String, String)]) -> Vec<(String, Node)> {
    let mut playlists = Vec::new();
    for (id, stem) in entries {
        playlists.push((
            id.clone(),
            Node::block(vec![
                ("inherit".into(), Node::leaf(DEFAULT_BASE)),
                (
                    "vars".into(),
                    Node::block(vec![
                        ("name".into(), Node::leaf(id.clone())),
                        ("lobbytitle".into(), Node::leaf(id.clone())),
                    ]),
                ),
                (
                    "gamemodes".into(),
                    Node::block(vec![(
                        "survival".into(),
                        Node::block(vec![("maps".into(), Node::block(vec![(stem.clone(), Node::leaf("1"))]))]),
                    )]),
                ),
            ]),
        ));
    }
    vec![("playlists".into(), Node::block(vec![("Playlists".into(), Node::block(playlists))]))]
}

/// Every file under `dir` a deploy is allowed to place, relative to `base`.
pub fn deployable_files(base: &Path) -> Result<Vec<File>> {
    let mut out = Vec::new();
    walk(base, base, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<File>) -> Result<()> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(base, &p, out)?;
            continue;
        }
        let rel = match p.strip_prefix(base) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let head = rel.split('/').next().unwrap_or_default();
        let allowed = DEPLOYABLE
            .iter()
            .any(|d| *d == head || rel.starts_with(&format!("{d}/")));
        if !allowed {
            continue;
        }
        let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        out.push(File { path: rel, bytes });
    }
    Ok(())
}

/// Writes the registration files and the manifest.
pub fn run(args: &Args) -> Result<serde_json::Value> {
    let document = project::load(Box::new(NativeProject::new(&args.project).map_err(|e| anyhow!("{e}"))?))
        .map_err(|e| anyhow!("{}: {e}", args.project.display()))?;
    let meta = document.doc.meta.clone();
    if !launcher_stem_ok(&meta.stem) {
        bail!(
            "{:?} is not a stem the launcher offers; it has to be mp_rr_<name> or mp_lobby",
            meta.stem
        );
    }
    let build = crate::build::default_out(&args.project);
    let out = args.out.clone().unwrap_or_else(|| build.join("deploy"));
    std::fs::create_dir_all(&out).map_err(|e| anyhow!("{}: {e}", out.display()))?;
    let files = deployable_files(&build)?;
    if files.is_empty() {
        bail!("{} has no deployable output; build the map first", build.display());
    }
    let base = meta.playlists.first().map(String::as_str).unwrap_or(DEFAULT_BASE);
    let entries = if meta.playlists.is_empty() {
        vec![(meta.stem.clone(), meta.stem.clone())]
    } else {
        meta.playlists.iter().map(|p| (format!("{}_{}", meta.stem, p), meta.stem.clone())).collect()
    };
    let manifest = Manifest {
        stem: meta.stem.clone(),
        display_name: meta.display_name.clone(),
        playlists: if meta.playlists.is_empty() { vec![DEFAULT_BASE.to_string()] } else { meta.playlists.clone() },
        map_names: map_names_line(&meta.stem, &meta.display_name),
        playlist_patch: "playlists_r5_patch.txt".to_string(),
        files,
    };
    let manifest_path = out.join("register.json");
    let mut body = serde_json::to_string_pretty(&manifest)?;
    body.push('\n');
    std::fs::write(&manifest_path, &body).map_err(|e| anyhow!("{}: {e}", manifest_path.display()))?;
    // The build-local copies are fragments of an install's files, so they are
    // named so they cannot be mistaken for one: copying a one-line file over
    // the launcher's allowlist would empty the picker.
    let names_path = out.join("r5f_wip_maps.txt");
    std::fs::write(&names_path, upsert_wip_line("", &meta.stem, &meta.display_name))
        .map_err(|e| anyhow!("{}: {e}", names_path.display()))?;
    let patch = playlist_patch(&entries);
    let patch_text = kv::write(&patch);
    let patch_path = out.join("playlists_r5_patch.fragment.txt");
    std::fs::write(&patch_path, &patch_text).map_err(|e| anyhow!("{}: {e}", patch_path.display()))?;

    let mut installed = serde_json::Value::Null;
    if let Some(root) = args.install.as_ref() {
        let platform = root.join("platform");
        if !platform.is_dir() {
            bail!("{} has no platform directory, so it is not a game install", root.display());
        }
        let patch_install = platform.join("playlists_r5_patch.txt");
        let wip_install = platform.join("r5f_wip_maps.txt");
        let before_patch = read_optional(&patch_install)?;
        let merged_patch = declare_in_patch(&before_patch, &meta.stem)?;
        let before_wip = read_optional(&wip_install)?;
        let merged_wip = upsert_wip_line(&before_wip, &meta.stem, &meta.display_name);
        if merged_patch != before_patch {
            std::fs::write(&patch_install, &merged_patch)
                .map_err(|e| anyhow!("{}: {e}", patch_install.display()))?;
        }
        if merged_wip != before_wip {
            std::fs::write(&wip_install, &merged_wip)
                .map_err(|e| anyhow!("{}: {e}", wip_install.display()))?;
        }
        crate::build::progress(90, "registered with the launcher");
        installed = json!({
            "platform": patch_install.display().to_string(),
            "declared": merged_patch != before_patch,
            "named": merged_wip != before_wip,
        });
    }
    crate::build::progress(100, "done");
    Ok(json!({
        "stem": manifest.stem,
        "display_name": manifest.display_name,
        "playlists": manifest.playlists,
        "inherit": base,
        "wip_maps": map_names_line(&meta.stem, &meta.display_name),
        "manifest": manifest_path.display().to_string(),
        "wip_maps_file": names_path.display().to_string(),
        "playlist_fragment": patch_path.display().to_string(),
        "installed": installed,
        "files": manifest.files.iter().map(|f| json!({"path": f.path, "bytes": f.bytes})).collect::<Vec<_>>(),
    }))
}

fn read_optional(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(anyhow!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_stem_the_launcher_offers_is_accepted() {
        assert!(launcher_stem_ok("mp_rr_x"));
        assert!(launcher_stem_ok("mp_rr_gauntlet"));
        assert!(launcher_stem_ok("mp_lobby"));
        for bad in ["", "mp_rr_", "mp_rr_a b", "mp_rr_a.b", "mp_lobby_x", "mp_rr_x/y", "MP_RR_X", &"a".repeat(65)] {
            assert!(!launcher_stem_ok(bad), "{bad:?} should be refused");
        }
        assert!(launcher_stem_ok("mp_rr_x"), "the shortest real stem");
    }

    /// The allowlist puts every `=` in the same column, at 34.
    #[test]
    fn the_allowlist_line_pads_the_stem_the_way_the_shipped_file_does() {
        for (stem, name) in [
            ("mp_rr_canyonlands_staging_mu1", "Firing Range"),
            ("mp_rr_desertlands_hu", "World's Edge"),
            ("mp_rr_aqueduct", "Overflow"),
            ("mp_lobby", "Lobby"),
        ] {
            let line = map_names_line(stem, name);
            assert_eq!(line, format!("{stem:<34}= {name}"), "{line:?}");
            assert_eq!(line.find('='), Some(34), "{line:?}");
        }
    }

    #[test]
    fn the_playlist_patch_inherits_a_base_and_maps_the_stem() {
        let patch = playlist_patch(&[("mp_rr_x".into(), "mp_rr_x".into())]);
        let text = kv::write(&patch);
        assert!(text.starts_with("playlists\n{\n\tPlaylists\n"), "{text}");
        assert!(text.contains("\t\tmp_rr_x\n\t\t{\n\t\t\tinherit survival_dev\n"), "{text}");
        assert!(text.contains("gamemodes\n\t\t\t{\n\t\t\t\tsurvival\n\t\t\t\t{\n\t\t\t\t\tmaps\n\t\t\t\t\t{\n\t\t\t\t\t\tmp_rr_x 1\n"), "{text}");
        let back = kv::parse(&text).expect("reads back");
        let entry = &back[0].1.path("Playlists.mp_rr_x").expect("the entry is there");
        assert_eq!(entry.path("inherit").unwrap().token(), Some("survival_dev"));
        assert_eq!(entry.path("gamemodes.survival.maps.mp_rr_x").unwrap().token(), Some("1"));
    }

    /// A patch as an install ships it: tabs, comments, a gamemode with an empty
    /// map choice, and thousands of lines this must not disturb.
    const REAL_PATCH: &str = "playlists\n{\n\tversion stable\n\n\tGamemodes\n\t{\n\t\tdefaults\n\t\t{\n\t\t\t// the map choice a mode offers\n\t\t\tmaps\n\t\t\t{\n\t\t\t}\n\t\t}\n\n\t\tsurvival\n\t\t{\n\t\t\tinherit defaults\n\t\t}\n\t}\n}\n";

    #[test]
    fn the_stem_lands_in_the_existing_map_block_and_nothing_else_moves() {
        let out = declare_in_patch(REAL_PATCH, "mp_rr_smoke").expect("declared");
        assert_eq!(out.matches("mp_rr_smoke").count(), 1, "{out}");
        let added: Vec<&str> = out.lines().filter(|l| !REAL_PATCH.lines().any(|o| o == *l)).collect();
        assert_eq!(added, vec!["\t\t\t\tmp_rr_smoke 1"], "one line added: {added:?}");
        // Take the added line back out and the document is the one we started
        // with, line for line and in order.
        let back: Vec<&str> = out.lines().filter(|l| !l.contains("mp_rr_smoke")).collect();
        assert_eq!(back, REAL_PATCH.lines().collect::<Vec<_>>(), "nothing else moved");
        assert!(out.contains("// the map choice a mode offers"), "comments survive");
    }

    #[test]
    fn declaring_twice_changes_nothing() {
        let once = declare_in_patch(REAL_PATCH, "mp_rr_smoke").expect("declared");
        let twice = declare_in_patch(&once, "mp_rr_smoke").expect("declared again");
        assert_eq!(once, twice, "a rebuild must not append the stem twice");
        assert_eq!(twice.matches("mp_rr_smoke").count(), 1);
    }

    #[test]
    fn a_gamemode_with_no_map_block_grows_one() {
        let patch = "playlists\n{\n\tGamemodes\n\t{\n\t\tdefaults\n\t\t{\n\t\t\tmax_players 16\n\t\t}\n\t}\n}\n";
        let out = declare_in_patch(patch, "mp_rr_smoke").expect("declared");
        assert!(out.contains("\t\t\tmaps\n\t\t\t{\n\t\t\t\tmp_rr_smoke 1\n\t\t\t}\n"), "{out}");
        assert!(out.contains("max_players 16"), "the gamemode keeps its own keys");
        // The block is nested under the gamemode, not under Gamemodes itself.
        let lines: Vec<&str> = out.lines().collect();
        let gamemode = lines.iter().position(|l| l.trim() == "defaults").expect("the gamemode");
        let maps = lines.iter().position(|l| l.trim() == "maps").expect("the block");
        assert!(maps > gamemode, "it goes inside the gamemode: {out}");
        assert!(
            leading_ws(lines[maps]).chars().count() > leading_ws(lines[gamemode]).chars().count(),
            "and one level deeper: {out}"
        );
    }

    #[test]
    fn a_patch_with_nowhere_to_declare_is_refused_rather_than_guessed() {
        let err = declare_in_patch("playlists\n{\n\tversion stable\n}\n", "mp_rr_smoke")
            .expect_err("refused");
        assert!(err.to_string().contains("mp_rr_smoke"), "{err}");
        assert!(declare_in_patch(REAL_PATCH, "not_a_map").is_err(), "a stem the launcher refuses");
    }

    #[test]
    fn the_wip_line_is_added_once_and_then_replaced_in_place() {
        let wip = "# extra maps\n#\n# mp_rr_my_wip = Mine\nmp_rr_salham = Loba Chronicle\n";
        let once = upsert_wip_line(wip, "mp_rr_smoke", "Smoke Test");
        assert!(once.contains("mp_rr_smoke                       = Smoke Test"), "{once}");
        assert!(once.contains("mp_rr_salham = Loba Chronicle"), "another stem is untouched");
        assert!(once.contains("# mp_rr_my_wip = Mine"), "comments survive");
        let renamed = upsert_wip_line(&once, "mp_rr_smoke", "Renamed");
        assert!(renamed.contains("mp_rr_smoke                       = Renamed"), "{renamed}");
        assert_eq!(renamed.matches("mp_rr_smoke").count(), 1, "the old line is replaced, not appended to");
    }

    #[test]
    fn a_merge_keeps_the_line_endings_the_file_already_has() {
        let crlf = REAL_PATCH.replace('\n', "\r\n");
        let out = declare_in_patch(&crlf, "mp_rr_smoke").expect("declared");
        assert!(!out.contains("\n\n"), "no bare newline was introduced");
        assert_eq!(out.matches("\r\n").count(), out.matches('\n').count());
    }

    #[test]
    fn only_deployable_directories_are_named() {        let dir = std::env::temp_dir().join(format!("r5ed-reg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("maps")).unwrap();
        std::fs::create_dir_all(dir.join("dedi/maps")).unwrap();
        std::fs::create_dir_all(dir.join("paks/Win64")).unwrap();
        std::fs::create_dir_all(dir.join("overview")).unwrap();
        std::fs::create_dir_all(dir.join("deploy")).unwrap();
        std::fs::write(dir.join("maps/x.bsp"), b"12345").unwrap();
        std::fs::write(dir.join("dedi/maps/x.bsp"), b"1").unwrap();
        std::fs::write(dir.join("paks/Win64/x.rpak"), b"12").unwrap();
        std::fs::write(dir.join("overview/x.uiia"), b"1").unwrap();
        std::fs::write(dir.join("deploy/register.json"), b"{}").unwrap();
        let files = deployable_files(&dir).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        // The dedicated-server half is not under a deployable directory: it
        // ships as a VPK, so only the client map and the paks are named.
        assert_eq!(paths, vec!["maps/x.bsp", "paks/Win64/x.rpak"]);
        assert_eq!(files[0].bytes, 5);
        assert!(deployable_files(&dir.join("nowhere")).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
