//! Temporary measurement harness for validator work.


use std::path::{Path, PathBuf};
use std::time::Instant;

use rbsp::lumps::collision::{self, BvhLeafData, BvhNode, Model};
use rbsp::lumps::Lump;

fn fixtures() -> PathBuf {
    PathBuf::from(std::env::var("R5ED_FIXTURES").unwrap_or_else(|_| "r5ed-fixtures".into()))
}

fn main() {
    let root = fixtures().join("dedi");
    let mut cases: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            let maps = e.path().join("maps");
            if !maps.is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(&maps) {
                for f in files.flatten() {
                    let n = f.file_name().to_string_lossy().to_string();
                    if n.ends_with(".bsp") {
                        cases.push((n.trim_end_matches(".bsp").to_string(), e.path()));
                    }
                }
            }
        }
    }
    cases.sort();

    let cdir = fixtures().join("client").join("_test_s21_in");
    if let Ok(rd) = std::fs::read_dir(&cdir) {
        for f in rd.flatten() {
            let n = f.file_name().to_string_lossy().to_string();
            if n.ends_with(".bsp") {
                cases.push((n.trim_end_matches(".bsp").to_string(), cdir.clone()));
            }
        }
    }
    if let Ok(p) = std::env::var("R5ED_TF2_BSP") {
        let p = PathBuf::from(p);
        if p.is_file() {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            let bsp = rbsp::Bsp::read_path(&p).expect("tf2");
            let v = bsp.version_kind().expect("tf2 version");
            probe_leaf(&format!("{stem} v{}", v.number()), &bsp);
        }
    }

    for (name, dir) in cases {
        let Ok((bsp, _)) = rbsp::Bsp::read_dir_map(dir.as_path(), &name) else { continue };
        if bsp.version_kind().is_none() {
            continue;
        }
        probe_leaf(&format!("{name} v{}", bsp.header.version), &bsp);
    }
    let _ = Path::new("");
}

/// Decides whether a node's leaf reference is absolute or relative to the
/// model's own first leaf, by counting how many leaves land on a plausible
/// poly header under each reading.
fn probe_leaf(name: &str, bsp: &rbsp::Bsp) {
    let v = match bsp.version_kind() {
        Some(v) => v,
        None => return,
    };
    let (Some(models), Some(nodes), Some(leaves), Some(props)) = (
        match rbsp::lumps::parse_at(0x0E, v, bsp.lump(0x0E), 0) {
            Ok(Lump::Model(m)) => Some(m),
            _ => None,
        },
        match rbsp::lumps::parse_at(0x12, v, bsp.lump(0x12), 0) {
            Ok(Lump::BvhNode(n)) => Some(n),
            _ => None,
        },
        match rbsp::lumps::parse_at(0x13, v, bsp.lump(0x13), 0) {
            Ok(Lump::BvhLeaf(l)) => Some(l),
            _ => None,
        },
        match rbsp::lumps::parse_at(0x11, v, bsp.lump(0x11), 0) {
            Ok(Lump::SurfaceProp(s)) => Some(s),
            _ => None,
        },
    ) else {
        return;
    };
    let props = props.len();
    for relative in [false, true] {
        let t = Instant::now();
        let mut good = 0usize;
        let mut bad = 0usize;
        let mut visited: Vec<bool> = Vec::new();
        let mut allowance = 20_000_000usize;
        let mut per_model: Vec<(usize, usize)> = Vec::new();
        let mut detail: Vec<String> = Vec::new();
        for (mi, m) in models.iter().filter(|m| m.has_collision()).enumerate() {
            let base = if relative { m.bvh_leaf.max(0) as usize } else { 0 };
            let (mut g, mut b) = (0usize, 0usize);
            let r = count(
                &leaves, &nodes, m, base, props, &mut visited, &mut allowance, &mut g, &mut b, &mut detail, mi,
            );
            good += g;
            bad += b;
            per_model.push((r, mi));
        }
        per_model.sort_by(|a, b| b.0.cmp(&a.0));
        println!(
            "== {name} {}: good={good} bad={bad} in {:?}; collidable models={} biggest={:?}",
            if relative { "relative" } else { "absolute" },
            t.elapsed(),
            per_model.len(),
            per_model.iter().take(3).collect::<Vec<_>>()
        );
        for d in detail.iter().take(12) {
            println!("    {d}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn count(
    leaves: &BvhLeafData,
    nodes: &[BvhNode],
    m: &Model,
    base: usize,
    props: usize,
    visited: &mut Vec<bool>,
    allowance: &mut usize,
    good: &mut usize,
    bad: &mut usize,
    detail: &mut Vec<String>,
    model: usize,
) -> usize {
    if *allowance == 0 {
        return 0;
    }
    *allowance -= 1;
    visited.clear();
    visited.resize(nodes.len(), false);
    let mut stack = vec![m.bvh_node as usize];
    let mut reached = 0usize;
    while let Some(n) = stack.pop() {
        if n >= nodes.len() {
            return reached;
        }
        if visited[n] {
            continue;
        }
        visited[n] = true;
        reached += 1;
        let node = &nodes[n];
        for slot in 0..4 {
            let kind = node.child_kind(slot);
            let at = node.child_index(slot) as usize;
            match kind {
                collision::CHILD_EMPTY => {}
                collision::CHILD_NODE => stack.push(at),
                collision::CHILD_BUNDLE => {
                    let start = base + at;
                    let c = leaves.word(start).unwrap_or(0xDEAD_BEEF);
                    if start >= leaves.len() || start + 1 + c as usize > leaves.len() {
                        *bad += 1;
                        detail.push(format!("model {model} node {n} slot {slot} bundle at {at} count {c}"));
                    } else {
                        *good += 1;
                    }
                }
                collision::CHILD_POLY => {
                    let start = base + at;
                    let h = leaves.word(start).unwrap_or(0xDEAD_BEEF);
                    if start >= leaves.len() || (h & 0xFFF) as usize >= props {
                        *bad += 1;
                        detail.push(format!("model {model} node {n} slot {slot} poly at {at} header {h:#010x}"));
                    } else {
                        *good += 1;
                    }
                }
                _ => {
                    if base + at < leaves.len() {
                        *good += 1;
                    } else {
                        *bad += 1;
                        detail.push(format!("model {model} node {n} slot {slot} kind {kind} at {at}"));
                    }
                }
            }
        }
    }
    reached
}
