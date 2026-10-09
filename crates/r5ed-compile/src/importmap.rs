//! `import-map`: a map the install already carries becomes a project.
//!
//! A map inside a pak is a set of wrap assets: a header-only `.bsp`, one file
//! per lump beside it, and the entity partitions. The asset tool exports those
//! out of the pak, this reads them back into a container, and the compiler's
//! reverse pass turns that container into a document plus the project files its
//! meshes need.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rbsp::{Bsp, Header, Layout};
use rdoc::model::{MapDoc, ObjectKind};
use rdoc::project::{self, NativeProject};
use serde_json::{json, Value};

/// Everything `import-map` takes. The two positional arguments are the stem and
/// the project; everything else narrows where the map is read from.
#[derive(Clone, Debug)]
pub struct Args {
    pub stem: String,
    pub project: PathBuf,
    /// A map directory, a `.bsp`, or a pak to export the map out of.
    pub from: Option<PathBuf>,
    /// The dedicated-server half, when the caller has it beside the client one.
    pub dedi_from: Option<PathBuf>,
    /// Scratch directory for the export; defaults to the project directory.
    pub cache: Option<PathBuf>,
    /// Overwrite an existing document.
    pub force: bool,
}

/// The client map of a stem, in the install, and the half that carries the
/// entity partitions beside it.
pub fn install_paks(game_root: &Path, stem: &str) -> Vec<PathBuf> {
    let dir = game_root.join("paks").join("Win64");
    [format!("{stem}_client_perm.rpak"), format!("{stem}_client_temp.rpak")]
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.is_file())
        .collect()
}

/// Where a stem's `.bsp` sits under a map directory. RSX exports a wrap asset
/// under `wrap/`, keeping its `maps/` path.
pub fn map_path(dir: &Path, stem: &str) -> Option<PathBuf> {
    let name = format!("{stem}.bsp");
    [dir.join(&name), dir.join("maps").join(&name), dir.join("wrap").join("maps").join(&name), dir.join("wrap").join(&name)]
        .into_iter()
        .find(|p| p.is_file())
}

/// Reads a header-only map plus every lump file beside it. A pak stores a
/// client's lumps with a `.client` suffix and a dedicated map's with `.server`,
/// so both spellings are accepted.
pub fn read_map(dir: &Path, stem: &str) -> Result<(Bsp, Vec<PathBuf>)> {
    let Some(path) = map_path(dir, stem) else {
        return Err(anyhow!("{} has no {stem}.bsp", dir.display()));
    };
    let home = path.parent().unwrap_or(dir).to_path_buf();
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let header = Header::parse(&bytes).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let mut lumps: BTreeMap<u16, Vec<u8>> = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut files = vec![path.clone()];
    let entries = std::fs::read_dir(&home).with_context(|| format!("reading {}", home.display()))?;
    let mut found: Vec<(u16, PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(id) = lump_id(&name, &format!("{stem}.bsp.")) else {
            continue;
        };
        found.push((id, p));
    }
    for (id, p) in found {
        let data = std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
        names.insert(id, p.file_name().unwrap_or_default().to_string_lossy().into_owned());
        lumps.insert(id, data);
        files.push(p);
    }
    let bsp = Bsp {
        header,
        lumps,
        layout: Layout::Sidecar,
        sidecar_names: names,
        packed_size: 0,
    };
    files.sort();
    Ok((bsp, files))
}

/// The lump id a sidecar file name carries, or `None` when the name is not one.
fn lump_id(name: &str, prefix: &str) -> Option<u16> {
    let rest = name.strip_prefix(prefix)?;
    let (hex, tail) = rest.split_once('.')?;
    if !tail.starts_with("bsp_lump") {
        return None;
    }
    // Stock maps name sidecars in either hex case: staging_mu1's client ships
    // `.004F.bsp_lump` beside `.0003.bsp_lump`. A lowercase-only match skipped
    // 18 of its 60 sidecars, mesh indices among them.
    if hex.len() != 4 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(hex, 16).ok()
}

/// The entity partitions beside a map, keyed by partition name.
pub fn read_partitions(home: &Path, stem: &str) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for p in rbsp::container::PARTITION_NAMES {
        let path = home.join(format!("{stem}_{p}.ent"));
        if let Ok(bytes) = std::fs::read(&path) {
            out.insert((*p).to_string(), bytes);
        }
    }
    out
}

/// Exports a map's wrap assets out of the paks that carry it.
pub fn export(game_root: &Path, stem: &str, cache: &Path) -> Result<PathBuf> {
    let paks = install_paks(game_root, stem);
    if paks.is_empty() {
        bail!("the install carries no client map called {stem}");
    }
    export_paks(&paks, &cache.join(stem))
}

/// Exports the wrap assets of a map's paks into `out`: the first names the
/// BSP header, the rest carry the lumps and entity partitions beside it.
fn export_paks(paks: &[PathBuf], out: &Path) -> Result<PathBuf> {
    let rsx = crate::tools::tool(crate::tools::RSX)?;
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out)?;
    // One session over the pair, so a lump in one pak that the other names resolves.
    rpak::Rsx::new(&rsx).export(paks, &["wrap"], &[], out)?;
    Ok(out.to_path_buf())
}

/// A map's `_perm` pak and its `_temp` partner, perm first, whichever of the
/// two was named. A partner that is not on disk is left out.
pub fn pak_pair(path: &Path) -> Vec<PathBuf> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return vec![path.to_path_buf()] };
    let lower = name.to_ascii_lowercase();
    let swap = |from: &str, to: &str| -> Option<PathBuf> {
        let at = lower.rfind(from)?;
        Some(path.with_file_name(format!("{}{to}{}", &name[..at], &name[at + from.len()..])))
    };
    let (perm, temp) = if lower.contains("_temp.") {
        (swap("_temp.", "_perm."), Some(path.to_path_buf()))
    } else {
        (Some(path.to_path_buf()), swap("_perm.", "_temp."))
    };
    [perm, temp].into_iter().flatten().filter(|p| p.is_file()).collect()
}

/// The install a pak sits in: `<root>/paks/Win64/<pak>`.
fn install_of_pak(path: &Path) -> Option<PathBuf> {
    let win64 = path.parent()?;
    let paks = win64.parent()?;
    paks.file_name()?.to_str()?.eq_ignore_ascii_case("paks").then(|| paks.parent().map(Path::to_path_buf))?
}

/// Unpacks the install's dedicated-server half of a map, when it ships one.
fn export_dedi(game_root: &Path, stem: &str, cache: &Path) -> Result<Option<PathBuf>> {
    let dir_vpk = game_root.join("vpk").join(crate::vpk::dir_archive(stem));
    if !dir_vpk.is_file() {
        return Ok(None);
    }
    let revpk = crate::tools::tool(crate::tools::REVPK)?;
    let out = cache.join(format!("{stem}_dedi"));
    let _ = std::fs::remove_dir_all(&out);
    crate::vpk::unpack(&dir_vpk, &out, &revpk)?;
    Ok(map_path(&out, stem).is_some().then_some(out))
}

/// Reads a stem's map out of whatever the caller pointed at.
fn locate(args: &Args) -> Result<(PathBuf, Option<PathBuf>)> {
    if let Some(from) = &args.from {
        if from.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("rpak")) {
            let cache = args.cache.clone().unwrap_or_else(|| args.project.join(".import"));
            let paks = pak_pair(from);
            if paks.is_empty() {
                bail!("{} is not a pak on disk", from.display());
            }
            let out = export_paks(&paks, &cache.join(&args.stem))?;
            // The dedicated half ships in the same install's VPK, when it has one.
            let dedi = match args.dedi_from.clone() {
                Some(d) => Some(d),
                None => match install_of_pak(from).or_else(|| crate::tools::game_root().ok()) {
                    Some(root) => export_dedi(&root, &args.stem, &cache)?,
                    None => None,
                },
            };
            return Ok((out, dedi));
        }
        if from.extension().and_then(|e| e.to_str()) == Some("bsp") {
            let home = from.parent().unwrap_or(Path::new(".")).to_path_buf();
            return Ok((home, args.dedi_from.clone()));
        }
        return Ok((from.clone(), args.dedi_from.clone()));
    }
    let game_root = crate::tools::game_root()?;
    let cache = args.cache.clone().unwrap_or_else(|| args.project.join(".import"));
    let dir = export(&game_root, &args.stem, &cache)?;
    let dedi = match args.dedi_from.clone() {
        Some(d) => Some(d),
        None => export_dedi(&game_root, &args.stem, &cache)?,
    };
    Ok((dir, dedi))
}

/// Imports a map as a project.
pub fn run(args: &Args) -> Result<Value> {
    if !crate::register::launcher_stem_ok(&args.stem) {
        bail!("{:?} is not a stem a map can carry; expected mp_rr_<name> or mp_lobby", args.stem);
    }
    if args.project.join("map.json").exists() && !args.force {
        bail!("{} already holds a map; pass --force to import over it", args.project.display());
    }
    crate::build::progress(5, "locating the map");
    let (dir, dedi_dir) = locate(args)?;
    crate::build::progress(25, "reading the container");
    let (client, files) = read_map(&dir, &args.stem)?;
    let home = map_path(&dir, &args.stem).and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| dir.clone());
    let ents = read_partitions(&home, &args.stem);
    if ents.is_empty() {
        crate::build::progress(30, "no entity partitions beside the map");
    }
    let dedi = match &dedi_dir {
        Some(d) => read_map(d, &args.stem).ok().map(|(b, _)| b),
        None => None,
    };
    crate::build::progress(45, "rebuilding the document");
    let (doc, project_files) = rcomp::decompile::decompile(&client, dedi.as_ref(), &ents, &args.stem)
        .map_err(|e| anyhow!("{e}"))?;
    let mut written = write_project(args, &doc, &project_files)?;
    // The document compiles against the map it came from, so the project
    // carries that map, both halves, where a build looks for it.
    crate::build::progress(80, "keeping the reference map");
    let dedi_ents = match &dedi_dir {
        Some(d) => {
            let home = map_path(d, &args.stem).and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| d.clone());
            read_partitions(&home, &args.stem)
        }
        None => BTreeMap::new(),
    };
    written.extend(keep_reference(&args.project, &args.stem, "client", &client, &ents)?);
    if let Some(d) = &dedi {
        written.extend(keep_reference(&args.project, &args.stem, "dedi", d, &dedi_ents)?);
    }
    crate::build::progress(100, "done");
    let materials: std::collections::BTreeSet<String> = doc
        .objects
        .iter()
        .flat_map(object_materials)
        .collect();
    Ok(json!({
        "stem": args.stem,
        "project": args.project.display().to_string(),
        "source": dir.display().to_string(),
        "map_version": client.version(),
        "lumps": client.lumps.len(),
        "partitions": ents.keys().cloned().collect::<Vec<_>>(),
        "objects": doc.objects.len(),
        "layers": doc.layers.len(),
        "materials": materials.iter().cloned().collect::<Vec<_>>(),
        "files": written,
        "read_files": files.iter().map(|f| f.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect::<Vec<_>>(),
    }))
}

/// Where a project keeps the map it was imported from.
pub const REFERENCE_DIR: &str = "reference";

/// Writes one half of the source map under `reference/<stem>/<side>/`, the
/// sidecar layout with lower-case names, plus its partition files. Returns the
/// project-relative paths written.
/// The client and dedicated halves of the map the document references, as the
/// import kept them in the project, or None when it references none.
pub fn kept_reference(project: &Path, doc: &rdoc::model::MapDoc) -> Result<Option<(Bsp, Option<Bsp>)>> {
    let Some(stem) = doc.objects.iter().find_map(|o| match &o.kind {
        ObjectKind::Reference(r) => Some(r.source.clone()),
        _ => None,
    }) else {
        return Ok(None);
    };
    let read = |side: &str| -> Result<Option<Bsp>> {
        let path = project.join(REFERENCE_DIR).join(&stem).join(side).join(format!("{stem}.bsp"));
        if !path.is_file() {
            return Ok(None);
        }
        Bsp::read_path(&path).map(Some).map_err(|e| anyhow!("{}: {e}", path.display()))
    };
    let Some(client) = read("client")? else {
        return Err(anyhow!("{} does not keep the {stem} map it references; import it again", project.display()));
    };
    Ok(Some((client, read("dedi")?)))
}

pub fn keep_reference(
    project: &Path,
    stem: &str,
    side: &str,
    bsp: &Bsp,
    ents: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<String>> {
    let rel = format!("{REFERENCE_DIR}/{stem}/{side}");
    let dir = project.join(REFERENCE_DIR).join(stem).join(side);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut copy = bsp.clone();
    copy.layout = Layout::Sidecar;
    let path = dir.join(format!("{stem}.bsp"));
    copy.write_path(&path).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let mut out = vec![format!("{rel}/{stem}.bsp")];
    for id in copy.lumps.keys() {
        out.push(format!("{rel}/{}", Bsp::sidecar_name(&format!("{stem}.bsp"), *id)));
    }
    for (name, bytes) in ents {
        let file = format!("{stem}_{name}.ent");
        std::fs::write(dir.join(&file), bytes).with_context(|| format!("writing {file}"))?;
        out.push(format!("{rel}/{file}"));
    }
    Ok(out)
}

fn object_materials(o: &rdoc::model::Object) -> Vec<String> {
    let mut out = Vec::new();
    match &o.kind {
        ObjectKind::Brush(b) => out.extend(b.faces.iter().map(|f| f.material.clone())),
        ObjectKind::Volume(v) => out.extend(v.brushes.iter().flat_map(|b| b.faces.iter().map(|f| f.material.clone()))),
        ObjectKind::Mesh(m) => out.extend(m.material_map.values().cloned()),
        ObjectKind::Terrain(t) => out.extend(t.layers.iter().map(|l| l.material.clone())),
        ObjectKind::VoxelTerrain(v) => out.push(v.material.clone()),
        _ => {}
    }
    out
}

/// Writes the document and its files, returning every path written.
pub fn write_project(args: &Args, doc: &MapDoc, files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<String>> {
    let mut fs = NativeProject::new(&args.project).map_err(|e| anyhow!("{e}"))?;
    project::save(&mut fs, doc).map_err(|e| anyhow!("{e}"))?;
    let mut written = vec![project::MAP_FILE.to_string()];
    for (path, bytes) in files {
        rdoc::project::ProjectFs::write(&mut fs, path, bytes).map_err(|e| anyhow!("{path}: {e}"))?;
        written.push(path.clone());
    }
    written.sort();
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_pak_names_its_map_and_finds_its_partner() {
        let dir = std::env::temp_dir().join(format!("r5ed_pakpair_{}", std::process::id()));
        let win64 = dir.join("paks").join("Win64");
        std::fs::create_dir_all(&win64).unwrap();
        let perm = win64.join("mp_rr_x_client_perm.rpak");
        let temp = win64.join("mp_rr_x_client_temp.rpak");
        std::fs::write(&perm, b"p").unwrap();
        std::fs::write(&temp, b"t").unwrap();
        assert_eq!(pak_pair(&perm), vec![perm.clone(), temp.clone()], "perm first, then its partner");
        assert_eq!(pak_pair(&temp), vec![perm.clone(), temp.clone()], "either one finds the pair");
        assert_eq!(install_of_pak(&perm).as_deref(), Some(dir.as_path()));
        std::fs::remove_file(&temp).unwrap();
        assert_eq!(pak_pair(&perm), vec![perm.clone()], "a missing partner is left out");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sidecar_name_carries_its_lump_id() {
        assert_eq!(lump_id("mp_rr_x.bsp.0003.bsp_lump", "mp_rr_x.bsp."), Some(3));
        assert_eq!(lump_id("mp_rr_x.bsp.004f.bsp_lump", "mp_rr_x.bsp."), Some(0x4f));
        assert_eq!(lump_id("mp_rr_x.bsp.004F.bsp_lump", "mp_rr_x.bsp."), Some(0x4f), "stock uses uppercase hex too");
        assert_eq!(lump_id("mp_rr_x.bsp.0001.bsp_lump.client", "mp_rr_x.bsp."), Some(1));
        assert_eq!(lump_id("mp_lobby.bsp.0079.bsp_lump.client", "mp_lobby.bsp."), Some(0x79));
        // Upper case is how the engine and a pak export spell it.
        assert_eq!(lump_id("mp_rr_x.bsp.004F.bsp_lump", "mp_rr_x.bsp."), Some(0x4f));
        assert_eq!(lump_id("mp_rr_x.bsp.007E.bsp_lump.client", "mp_rr_x.bsp."), Some(0x7e));
        for bad in [
            "mp_rr_x.bsp.0003.txt",
            "mp_rr_x.bsp.003.bsp_lump",
            "mp_rr_x.bsp.0003g.bsp_lump",
            "other.bsp.0003.bsp_lump",
            "mp_rr_x.bsp.00003.bsp_lump",
        ] {
            assert_eq!(lump_id(bad, "mp_rr_x.bsp."), None, "{bad}");
        }
    }

    #[test]
    fn a_map_directory_is_read_back_as_one_container() {
        let dir = std::env::temp_dir().join(format!("r5ed-imp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut bsp = Bsp::new(51, 0, 0);
        bsp.set_lump(0x03, vec![1, 2, 3, 4]);
        bsp.set_lump(0x4f, vec![5, 6, 7, 8]);
        std::fs::write(dir.join("mp_rr_x.bsp"), bsp.header.to_bytes()).unwrap();
        std::fs::write(dir.join("mp_rr_x.bsp.0003.bsp_lump"), b"lump3").unwrap();
        std::fs::write(dir.join("mp_rr_x.bsp.004f.bsp_lump.client"), b"lump4f").unwrap();
        std::fs::write(dir.join("mp_rr_x_env.ent"), b"{}").unwrap();
        std::fs::write(dir.join("mp_rr_x.bsp.0001.bsp_lump.server"), b"ignored? no").unwrap();
        let (read, files) = read_map(&dir, "mp_rr_x").expect("reads");
        assert_eq!(read.lumps.len(), 3, "the .server sidecar is a lump too");
        assert_eq!(read.lump(0x03), b"lump3");
        assert_eq!(read.lump(0x4f), b"lump4f");
        assert_eq!(read.lump(0x01), b"ignored? no");
        assert_eq!(read.version(), 51);
        assert_eq!(files.len(), 4);
        let ents = read_partitions(&dir, "mp_rr_x");
        assert_eq!(ents.len(), 1);
        assert!(ents.contains_key("env"));
        assert!(read_partitions(&dir, "mp_rr_nothing").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_with_no_map_says_so() {
        let dir = std::env::temp_dir().join(format!("r5ed-imp-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_map(&dir, "mp_rr_x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_header_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("r5ed-imp-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mp_rr_x.bsp"), b"rBS").unwrap();
        assert!(read_map(&dir, "mp_rr_x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
