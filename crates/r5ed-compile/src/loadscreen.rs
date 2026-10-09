//! `loadscreen`: a PNG becomes the map's loading screen, packed as the one
//! `uiia` a loadscreen archive holds.
//!
//! The image is looked up by the guid its own name hashes to, because that is
//! how the loading screen is found, and the archive carries the header flag the
//! native pak loader requires and no compression, which is the only combination
//! this engine reads back.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use rdoc::project::{self, NativeProject};
use serde_json::{json, Value};

use crate::pak::{self, Entry, Manifest};
use crate::uiia::{self, Rgba};

/// Everything `loadscreen` takes.
#[derive(Clone, Debug)]
pub struct Args {
    pub project: PathBuf,
    /// Image to encode; defaults to the document's `meta.loadscreen`.
    pub image: Option<PathBuf>,
    /// Where the archive is written; defaults to `<project>/build`.
    pub out: Option<PathBuf>,
}

/// The asset name a map's loading screen is stored under.
pub fn asset_path(stem: &str) -> String {
    format!("ui_image/loadscreens/{stem}_widescreen.rpak")
}

/// The name the encoded image is written under inside the archive.
pub fn image_file(stem: &str) -> String {
    format!("{stem}_widescreen.uiia")
}

/// Encodes the image and packs it.
pub fn run(args: &Args) -> Result<Value> {
    let document = project::load(Box::new(NativeProject::new(&args.project).map_err(|e| anyhow::anyhow!("{e}"))?))
        .map_err(|e| anyhow::anyhow!("{}: {e}", args.project.display()))?;
    let stem = document.doc.meta.stem.clone();
    let source = match &args.image {
        Some(p) => p.clone(),
        None => match &document.doc.meta.loadscreen {
            Some(rel) => args.project.join(rel),
            None => bail!("no image given and the document names no loadscreen"),
        },
    };
    let bytes = std::fs::read(&source).with_context(|| format!("reading {}", source.display()))?;
    let image = Rgba::from_png(&bytes).map_err(|e| anyhow::anyhow!("{}: {e}", source.display()))?;
    let encoded = uiia::encode(&image).map_err(|e| anyhow::anyhow!("{}: {e}", source.display()))?;
    let parsed = uiia::parse(&encoded).map_err(|e| anyhow::anyhow!("the encoded image does not read back: {e}"))?;

    let out = args.out.clone().unwrap_or_else(|| crate::build::default_out(&args.project));
    let stage = out.join("loadscreen");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).with_context(|| format!("creating {}", stage.display()))?;
    std::fs::create_dir_all(&out)?;
    let file = image_file(&stem);
    std::fs::write(stage.join(&file), &encoded).with_context(|| format!("writing {}", file))?;

    let mut manifest = Manifest::new(&format!("{stem}_loadscreen"), &stage, &out);
    let guid = pak::string_to_guid(&asset_path(&stem))?;
    manifest
        .push(Entry::pinned("uiia", &file, guid))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let manifest_path = stage.join(format!("{stem}_loadscreen.json"));
    manifest.save(&manifest_path)?;
    crate::build::progress(60, "packing");
    let repak = crate::tools::tool(crate::tools::REPAK)?;
    let header = pak::pack(&repak, &manifest, &manifest_path)?;
    crate::build::progress(100, "done");
    Ok(json!({
        "stem": stem,
        "pak": manifest.output().display().to_string(),
        "bytes": std::fs::metadata(manifest.output()).map(|m| m.len()).unwrap_or(0),
        "flags": format!("0x{:04X}", header.flags),
        "assets": header.assets,
        "source": source.display().to_string(),
        "uiia": {
            "asset": asset_path(&stem),
            "guid": format!("0x{guid:016X}"),
            "file": file,
            "bytes": encoded.len(),
            "width": parsed.width,
            "height": parsed.height,
            "flags": format!("0x{:04X}", parsed.flags),
            "tiles": parsed.tile_bytes,
        },
        "manifest": manifest_path.display().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_asset_name_is_the_one_the_loading_screen_is_found_by() {
        assert_eq!(asset_path("mp_rr_x"), "ui_image/loadscreens/mp_rr_x_widescreen.rpak");
        assert_eq!(image_file("mp_rr_x"), "mp_rr_x_widescreen.uiia");
        // The guid of a known map's loading screen, as its pak carries it.
        assert_eq!(
            pak::string_to_guid(&asset_path("mp_rr_district")).unwrap(),
            0xF826_8129_E803_1940
        );
    }
}
