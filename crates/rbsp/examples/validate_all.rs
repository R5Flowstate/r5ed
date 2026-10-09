//! Validates every map under a directory tree and tallies issues by kind.
//!
//!     cargo run -p rbsp --release --example validate_all -- <dir>

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn maps(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            maps(&p, out);
        } else if p.extension().is_some_and(|x| x == "bsp") {
            out.push(p);
        }
    }
}

fn kind_of(message: &str) -> String {
    message
        .split(|c: char| c.is_ascii_digit())
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("#")
        .chars()
        .take(90)
        .collect()
}

fn main() {
    let root = std::env::args().nth(1).expect("usage: validate_all <dir>");
    let mut found = Vec::new();
    maps(Path::new(&root), &mut found);
    for path in found {
        let Ok(bsp) = rbsp::Bsp::read_path(&path) else {
            println!("{}: unreadable", path.display());
            continue;
        };
        let dir = path.parent().unwrap_or(Path::new("."));
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ents = rbsp::Bsp::read_dir_map(dir, &stem).map(|(_, e)| e).unwrap_or_default();
        let issues = rbsp::validate(&bsp, &ents);
        let mut tally: BTreeMap<(String, String), usize> = BTreeMap::new();
        for i in issues.iter().filter(|i| i.severity == rbsp::Severity::Error) {
            *tally.entry((i.lump.map(|l| format!("{l:#04x}")).unwrap_or_default(), kind_of(&i.message))).or_default() += 1;
        }
        println!("{} v{}: {} errors", stem, bsp.version(), tally.values().sum::<usize>());
        for ((lump, kind), n) in tally {
            println!("    {n:6}  {lump:6} {kind}");
        }
    }
}
