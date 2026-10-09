//! The shipped staging_mu1 pair in the agent kit, for the tests that check
//! against stock bytes. Every helper returns `None` when the kit is not
//! checked out, and the tests skip.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rbsp::Bsp;
use rcomp::decompile;
use rcomp::{CompileOptions, ProjectFs, Targets};
use rdoc::model::{MapDoc, Partition};

pub struct Files(pub BTreeMap<String, Vec<u8>>);

impl ProjectFs for Files {
    fn read(&self, rel: &str) -> anyhow::Result<Vec<u8>> {
        self.0.get(rel).cloned().ok_or_else(|| anyhow::anyhow!("no project file {rel}"))
    }
}

pub const STEM: &str = "mp_rr_canyonlands_staging_mu1";

/// The unreduced pair, or `None` when this checkout does not carry it.
pub fn kit() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agent-kit/bsp/staging_mu1_full");
    if dir.join("client").join(format!("{STEM}.bsp")).is_file() && dir.join("dedi").join(format!("{STEM}.bsp")).is_file() {
        Some(dir)
    } else {
        eprintln!("skipped: {} is not checked out", dir.display());
        None
    }
}

/// One half: the container and its partition files.
pub fn half(kit: &Path, side: &str) -> (Bsp, BTreeMap<String, Vec<u8>>) {
    let dir = kit.join(side);
    let bsp = Bsp::read_path(&dir.join(format!("{STEM}.bsp"))).expect("the stock map reads");
    let mut ents = BTreeMap::new();
    for p in Partition::FILES {
        let f = dir.join(format!("{STEM}_{}.ent", p.name()));
        ents.insert(p.name().to_string(), std::fs::read(&f).expect("a stock partition reads"));
    }
    (bsp, ents)
}

/// The decompiled document and its project files.
pub fn decompiled(kit: &Path) -> (MapDoc, Files) {
    let (client, ents) = half(kit, "client");
    let (dedi, _) = half(kit, "dedi");
    let (doc, files) = decompile::decompile(&client, Some(&dedi), &ents, STEM).expect("the stock map decompiles");
    (doc, Files(files))
}

pub fn options(kit: &Path) -> CompileOptions {
    CompileOptions {
        targets: Targets { client: true, dedi: true },
        reference_root: Some(kit.to_path_buf()),
        ..Default::default()
    }
}

