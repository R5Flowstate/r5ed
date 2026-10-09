//! Regenerates `schema/entities.json` from the engine tables, the shipped map
//! entity partitions and the game scripts.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rdoc::model::Partition;
use rdoc::schema::{self, MapSource, SchemaInput, ScriptSource};

const FACTORIES: &str = "server_factories.json";
const DATAMAPS: &str = "server_datamaps.json";
/// No single partition or script is anywhere near this large.
const MAX_INPUT_BYTES: u64 = 256 << 20;

struct Args {
    check: bool,
    engine: PathBuf,
    maps: PathBuf,
    scripts: PathBuf,
    out: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut check = false;
    let mut engine = PathBuf::from("schema/engine");
    let mut maps = PathBuf::from(".");
    let mut scripts = PathBuf::from(".");
    let mut out = PathBuf::from("schema/entities.json");
    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        let mut value = || argv.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--check" => check = true,
            "--engine" => engine = PathBuf::from(value()?),
            "--maps" => maps = PathBuf::from(value()?),
            "--scripts" => scripts = PathBuf::from(value()?),
            "--out" => out = PathBuf::from(value()?),
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(Args { check, engine, maps, scripts, out })
}

const USAGE: &str = "r5ed-schema [--check] [--engine DIR] [--maps DIR] [--scripts DIR] [--out FILE]";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("r5ed-schema: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let factories = read(&args.engine.join(FACTORIES))?;
    let datamaps = read(&args.engine.join(DATAMAPS))?;

    let mut map_files = Vec::new();
    collect(&args.maps, &mut map_files, &[])?;
    map_files.sort();
    let mut maps: Vec<MapSource> = Vec::with_capacity(map_files.len());
    for path in &map_files {
        let Some((partition, text)) = read_entities(path) else { continue };
        maps.push(MapSource { path: display(path, &args.maps), partition, text });
    }
    if maps.is_empty() {
        return Err(format!("no .ent partitions or ENTITIES lumps under {}", args.maps.display()));
    }

    let mut script_files = Vec::new();
    collect(&args.scripts, &mut script_files, &["nut", "gnut"])?;
    script_files.sort();
    let mut scripts: Vec<ScriptSource> = Vec::with_capacity(script_files.len());
    for path in &script_files {
        if let Some(text) = read_optional(path)? {
            scripts.push(ScriptSource { path: display(path, &args.scripts), text });
        }
    }

    let input = SchemaInput {
        factories_json: &factories,
        datamaps_json: &datamaps,
        maps: &maps,
        scripts: &scripts,
    };
    let generated = schema::generate(&input)?;
    let text = generated.to_json();

    let wanted: Vec<String> = serde_json::from_str::<serde_json::Value>(&factories)
        .ok()
        .and_then(|v| v["factories"].as_array().cloned())
        .map(|list| {
            list.iter()
                .filter_map(|f| f["classname"].as_str().map(str::to_string))
                .filter(|c| generated.class(c).is_none())
                .collect()
        })
        .unwrap_or_default();
    if !wanted.is_empty() {
        return Err(format!("{} registered classnames are missing from the schema: {wanted:?}", wanted.len()));
    }

    if args.check {
        let on_disk = std::fs::read_to_string(&args.out)
            .map_err(|e| format!("{}: {e}", args.out.display()))?;
        if on_disk == text {
            println!("{} is up to date: {} classes", args.out.display(), generated.len());
            return Ok(());
        }
        return Err(format!(
            "{} is stale: regenerate it with r5ed-schema",
            args.out.display()
        ));
    }
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&args.out, text.as_bytes()).map_err(|e| format!("{}: {e}", args.out.display()))?;
    println!(
        "{}: {} classes, {} map partitions, {} scripts",
        args.out.display(),
        generated.len(),
        maps.len(),
        scripts.len()
    );
    Ok(())
}

fn read(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_INPUT_BYTES {
        return Err(format!("{} is larger than {MAX_INPUT_BYTES} bytes", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    String::from_utf8(bytes).map_err(|_| format!("{} is not valid UTF-8", path.display()))
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    let Ok(meta) = std::fs::metadata(path) else { return Ok(None) };
    if meta.len() > MAX_INPUT_BYTES {
        return Ok(None);
    }
    read(path).map(Some)
}

/// Every file under `dir`, recursively, with the given extensions when the
/// list is not empty. Entries are sorted so a run is reproducible.
fn collect(dir: &Path, out: &mut Vec<PathBuf>, extensions: &[&str]) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        paths.push(entry.path());
    }
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(&path, out, extensions)?;
            continue;
        }
        let keep = extensions.is_empty() || path_is_entity(&path) || extension_is(&path, extensions);
        if keep {
            out.push(path);
        }
    }
    Ok(())
}

fn extension_is(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| extensions.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// An `.ent` sidecar, or the ENTITIES lump sidecar a BSP splits its lumps into.
fn path_is_entity(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    if name.ends_with(".ent") {
        return true;
    }
    match name.split_once(".bsp.") {
        Some((stem, rest)) => stem.ends_with(".bsp") && rest.eq_ignore_ascii_case("0000.bsp_lump"),
        None => false,
    }
}

fn read_entities(path: &Path) -> Option<(Partition, String)> {
    let name = path.file_name().and_then(|n| n.to_str())?.to_ascii_lowercase();
    if name.ends_with(".ent") {
        let stem = name.strip_suffix(".ent")?;
        let mut partition = Partition::Main;
        for p in Partition::FILES {
            if let Some(suffix) = p.suffix() {
                if stem.ends_with(&format!("_{suffix}")) {
                    partition = p;
                }
            }
        }
        return Some((partition, read_optional(path).ok().flatten()?));
    }
    Some((Partition::Main, read_optional(path).ok().flatten()?))
}

fn display(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
