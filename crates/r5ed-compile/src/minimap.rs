//! `minimap`: the map overview, as the pair the engine reads at level load.
//!
//! Two files make a minimap. The image is a `uiia` looked up by the guid its
//! name hashes to, and the overview document is a key value file naming that
//! image together with the world square it covers, which is what turns a texture
//! into something drawn on the map screen.
//!
//! With no image given the picture is rendered from the compiled map: every
//! render triangle is projected onto the overview square and filled with a flat
//! shade of its own facing, near to far, so the result is the map's own outline
//! rather than a picture of something else.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rbsp::{Bsp, Version};
use rdoc::project::{self, NativeProject};

use serde_json::{json, Value};

use crate::kv::{self, Node};
use crate::pak::{self, Entry, Manifest};
use crate::uiia::{self, Rgba};

/// Everything `minimap` takes.
#[derive(Clone, Debug)]
pub struct Args {
    pub project: PathBuf,
    /// Image to publish; defaults to a render of the compiled map.
    pub image: Option<PathBuf>,
    /// Where the archives are written; defaults to `<project>/build`.
    pub out: Option<PathBuf>,
}

/// The asset name a map's overview image is stored under.
pub fn asset_path(stem: &str) -> String {
    format!("ui_image/overviews/{stem}.rpak")
}

/// The overview document path, which is also how the engine finds it.
pub fn overview_path(stem: &str) -> String {
    format!("resource/overviews/{stem}.txt")
}

/// The span the client scripts take every overview image to have, whatever its
/// pixel size (`OVERVIEW_MAP_SIZE` in cl_fullmap): the map covers `scale * 4096`
/// world units.
pub const OVERVIEW_SPAN: f64 = 4096.0;

/// The key values an overview document holds, for the world square the document
/// names: the image's top-left corner (least X, greatest Y, as the render draws
/// north up) and the world units per overview unit.
pub fn overview_doc(stem: &str, origin: [f64; 2], size: f64, resolution: u32) -> Vec<(String, Node)> {
    let _ = resolution;
    let scale = size / OVERVIEW_SPAN;
    vec![
        (stem.to_string(), Node::block(vec![
            ("assets".into(), Node::block(vec![("minimap".into(), Node::leaf(format!("overviews\\{stem}")))])),
            ("pos_x".into(), Node::leaf(trunc(origin[0]))),
            ("pos_y".into(), Node::leaf(trunc(origin[1] + size))),
            ("scale".into(), Node::leaf(trunc(scale))),
            ("displayDist".into(), Node::leaf("2048")),
            ("threatMaxDist".into(), Node::leaf("2500")),
            ("threatNearDist".into(), Node::leaf("500")),
            ("threatFarDist".into(), Node::leaf("2000")),
        ])),
    ]
}

/// A whole number prints without a fraction, which is the spelling the shipped
/// documents use.
fn trunc(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1.0e9 {
        format!("{}", v as i64)
    } else {
        format!("{v:.4}")
    }
}

/// Renders a top-down view of a map's render geometry.
pub fn render(bsp: &Bsp, origin: [f64; 2], size: f64, resolution: u32) -> Result<Rgba> {
    let n = resolution.max(2) as usize;
    let mut rgba = vec![0u8; n * n * 4];
    for px in rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&[16, 18, 20, 255]);
    }
    let mut depth = vec![f32::NEG_INFINITY; n * n];
    let scale = (size / n as f64) as f32;
    let triangles = triangles(bsp)?;
    if triangles.is_empty() {
        bail!("the map carries no render triangles, so there is nothing to draw");
    }
    // A fixed light so the same map always draws the same picture.
    let light = glam::Vec3::new(0.4, 0.3, 0.86).normalize();
    let mut stats = 0usize;
    for tri in triangles {
        let face = tri_normal(&tri);
        let base = tri_shade(&tri, light);
        let mut area = 0usize;
        for (x, y) in triangle_pixels(&tri, origin, size, scale, n as f32) {
            let Some((u, v, w)) = barycentric(&tri, x, y, origin, size, scale) else {
                continue;
            };
            let z = tri.0.z * u + tri.1.z * v + tri.2.z * w;
            let i = y as usize * n + x as usize;
            if z <= depth[i] {
                continue;
            }
            depth[i] = z;
            let lit = (face.dot(light) * 0.5 + 0.5).clamp(0.15, 1.0) * base;
            let o = i * 4;
            rgba[o] = (lit * 150.0).min(255.0) as u8;
            rgba[o + 1] = (lit * 165.0).min(255.0) as u8;
            rgba[o + 2] = (lit * 190.0).min(255.0) as u8;
            rgba[o + 3] = 255;
            area += 1;
        }
        if area > 0 {
            stats += 1;
        }
    }
    if stats == 0 {
        bail!("no triangle of the map lands inside the overview square");
    }
    Ok(Rgba { width: resolution, height: resolution, pixels: rgba })
}

/// A per-triangle base shade, so models and the world read differently.
fn tri_shade(tri: &Triangle, light: glam::Vec3) -> f32 {
    let face = tri_normal(tri);
    let facing = (face.dot(light) * 0.5 + 0.5).clamp(0.15, 1.0);
    let tint = ((tri.0.x * 0.013 + tri.0.y * 0.021).rem_euclid(1.0) as f32) * 0.35 + 0.65;
    (facing * tint).clamp(0.1, 1.0)
}

fn tri_normal(tri: &Triangle) -> glam::Vec3 {
    let e1 = tri.1 - tri.0;
    let e2 = tri.2 - tri.0;
    let n = e1.cross(e2);
    if n.length_squared() > 0.0 {
        n.normalize()
    } else {
        glam::Vec3::Z
    }
}

fn triangle_pixels(tri: &Triangle, origin: [f64; 2], size: f64, scale: f32, n: f32) -> Vec<(i64, i64)> {
    let mut lo = [f32::MAX; 2];
    let mut hi = [f32::MIN; 2];
    for v in [tri.0, tri.1, tri.2] {
        let (sx, sy) = to_screen(v, origin, size, scale);
        lo[0] = lo[0].min(sx);
        lo[1] = lo[1].min(sy);
        hi[0] = hi[0].max(sx);
        hi[1] = hi[1].max(sy);
    }
    let (x0, x1) = (lo[0].floor() as i64, hi[0].ceil() as i64);
    let (y0, y1) = (lo[1].floor() as i64, hi[1].ceil() as i64);
    let (w, h) = (n as i64, n as i64);
    let mut out = Vec::new();
    for y in y0.max(0)..y1.min(h) {
        for x in x0.max(0)..x1.min(w) {
            out.push((x, y));
        }
    }
    out
}

fn to_screen(v: glam::Vec3, origin: [f64; 2], size: f64, scale: f32) -> (f32, f32) {
    let x = (v.x as f64 - origin[0]) / scale as f64;
    let y = (origin[1] + size - v.y as f64) / scale as f64;
    (x as f32, y as f32)
}

fn barycentric(tri: &Triangle, x: i64, y: i64, origin: [f64; 2], size: f64, scale: f32) -> Option<(f32, f32, f32)> {
    let a = to_screen(tri.0, origin, size, scale);
    let b = to_screen(tri.1, origin, size, scale);
    let c = to_screen(tri.2, origin, size, scale);
    let px = x as f32 + 0.5;
    let py = y as f32 + 0.5;
    let d = (b.1 - c.1) * (a.0 - c.0) + (c.0 - b.0) * (a.1 - c.1);
    if d.abs() < 1.0e-9 {
        return None;
    }
    let u = ((b.1 - c.1) * (px - c.0) + (c.0 - b.0) * (py - c.1)) / d;
    let v = ((c.1 - a.1) * (px - c.0) + (a.0 - c.0) * (py - c.1)) / d;
    let w = 1.0 - u - v;
    (u >= -0.002 && v >= -0.002 && w >= -0.002).then_some((u, v, w))
}

/// Three positions, in map order.
type Triangle = (glam::Vec3, glam::Vec3, glam::Vec3);

/// Every render triangle of a map, read from its position pool, index lump and
/// mesh table. An index outside the pool ends the mesh it belongs to rather than
/// the whole map.
fn triangles(bsp: &Bsp) -> Result<Vec<Triangle>> {
    use rbsp::lumps::{parse, Lump};
    let version = bsp.version_kind().unwrap_or(Version::ClientV51);
    let positions: Vec<glam::Vec3> = rbsp::lumps::prims::parse(bsp.lump(rbsp::ids::VERTICES), version)
        .map_err(|e| anyhow!("{e}"))?
        .into_iter()
        .map(|v| glam::Vec3::new(v.x, v.y, v.z))
        .collect();
    let indices: Vec<u16> = match parse(rbsp::ids::MESH_INDICES, version, bsp.lump(rbsp::ids::MESH_INDICES)) {
        Ok(Lump::MeshIndex(i)) => i.iter().map(|m| m.get()).collect(),
        _ => Vec::new(),
    };
    let meshes: Vec<rbsp::lumps::render::Mesh> = match parse(rbsp::ids::MESHES, version, bsp.lump(rbsp::ids::MESHES)) {
        Ok(Lump::Mesh(m)) => m,
        _ => Vec::new(),
    };
    if positions.is_empty() || indices.is_empty() || meshes.is_empty() {
        bail!("the map has no render geometry to draw");
    }
    let mut out = Vec::new();
    for mesh in &meshes {
        let start = mesh.first_index as usize;
        for t in 0..mesh.triangle_count as usize {
            let base = start + t * 3;
            let Some(tri) = [base, base + 1, base + 2]
                .map(|i| indices.get(i).map(|v| *v as usize))
                .into_iter()
                .collect::<Option<Vec<usize>>>()
                .and_then(|v| v.into_iter().map(|i| positions.get(i).copied()).collect::<Option<Vec<_>>>())
            else {
                break;
            };
            out.push((tri[0], tri[1], tri[2]));
        }
    }
    Ok(out)
}

/// The compiled client map of a project, when it has one.
pub fn compiled_map(project: &Path) -> Option<PathBuf> {
    let out = crate::build::default_out(project);
    for candidate in [out.join("maps"), out.join("dedi").join("maps")] {
        if let Ok(rd) = std::fs::read_dir(candidate) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("bsp") {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Writes the overview pair and reports both files.
pub fn run(args: &Args) -> Result<Value> {
    let document = project::load(Box::new(NativeProject::new(&args.project).map_err(|e| anyhow!("{e}"))?))
        .map_err(|e| anyhow!("{}: {e}", args.project.display()))?;
    let stem = document.doc.meta.stem.clone();
    let settings = &document.doc.meta.minimap;
    let image = match &args.image {
        Some(p) => {
            let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            Rgba::from_png(&bytes).map_err(|e| anyhow!("{}: {e}", p.display()))?
        }
        None => {
            let map = compiled_map(&args.project)
                .ok_or_else(|| anyhow!("no image given and the project has no compiled map to render"))?;
            let bsp = Bsp::read_path(&map).map_err(|e| anyhow!("{}: {e}", map.display()))?;
            render(&bsp, settings.origin, settings.size, settings.resolution)?
        }
    };
    let resolution = image.width.max(image.height);
    let encoded = uiia::encode(&image).map_err(|e| anyhow!("{e}"))?;
    let parsed = uiia::parse(&encoded).map_err(|e| anyhow!("the encoded image does not read back: {e}"))?;

    let out = args.out.clone().unwrap_or_else(|| crate::build::default_out(&args.project));
    let stage = out.join("overview");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;
    std::fs::create_dir_all(&out)?;
    let image_rel = format!("{stem}.uiia");
    std::fs::write(stage.join(&image_rel), &encoded)?;
    let overview = overview_doc(&stem, settings.origin, settings.size, resolution);
    let overview_text = kv::write(&overview);
    // The document is read back with the same grammar the engine uses, so a
    // minimap that would come up blank never leaves here.
    let parsed_overview = kv::parse(&overview_text)
        .map_err(|e| anyhow!("the overview document does not read back: {e}"))?;
    let entry = parsed_overview
        .first()
        .and_then(|(name, node)| if *name == stem { Some(node) } else { None })
        .ok_or_else(|| anyhow!("the overview document does not name {stem}"))?;
    let named = entry
        .path("assets.minimap")
        .and_then(Node::token)
        .ok_or_else(|| anyhow!("the overview document names no image"))?;
    if named != format!("overviews\\{stem}") {
        bail!("the overview names {named:?} instead of its own image");
    }
    let overview_rel = overview_path(&stem);
    let overview_file = stage.join(&overview_rel);
    std::fs::create_dir_all(overview_file.parent().unwrap_or(&stage))?;
    std::fs::write(&overview_file, &overview_text)?;

    let mut manifest = Manifest::new(&stem, &stage, &out);
    let guid = pak::string_to_guid(&asset_path(&stem))?;
    manifest
        .push(Entry::pinned("uiia", &image_rel, guid))
        .map_err(|e| anyhow!("{e}"))?;
    manifest
        .push(Entry::new("wrap", &overview_rel))
        .map_err(|e| anyhow!("{e}"))?;
    let manifest_path = stage.join(format!("{stem}_overview.json"));
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
        "overview": {
            "path": overview_rel,
            "bytes": overview_text.len(),
            "minimap": format!("overviews\\{stem}"),
            "pos_x": settings.origin[0],
            "pos_y": settings.origin[1] + settings.size,
            "scale": settings.size / resolution as f64,
        },
        "uiia": {
            "asset": asset_path(&stem),
            "guid": format!("0x{guid:016X}"),
            "bytes": encoded.len(),
            "width": parsed.width,
            "height": parsed.height,
            "flags": format!("0x{:04X}", parsed.flags),
        },
        "manifest": manifest_path.display().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_overview_document_names_the_image_and_the_square_it_covers() {
        let doc = overview_doc("mp_rr_x", [-4788.0, 689.0], 16384.0, 2048);
        let text = kv::write(&doc);
        assert_eq!(
            text,
            "mp_rr_x\n{\n\tassets\n\t{\n\t\tminimap overviews\\mp_rr_x\n\t}\n\tpos_x -4788\n\tpos_y 17073\n\tscale 4\n\tdisplayDist 2048\n\tthreatMaxDist 2500\n\tthreatNearDist 500\n\tthreatFarDist 2000\n}\n"
        );
        let back = kv::parse(&text).expect("reads back");
        assert_eq!(back[0].1.path("assets.minimap").unwrap().token(), Some("overviews\\mp_rr_x"));
        assert_eq!(back[0].1.path("scale").unwrap().token(), Some("4"), "16384 units over the 4096 overview span");
        assert_eq!(back[0].1.path("displayDist").unwrap().token(), Some("2048"));
        assert_eq!(overview_path("mp_rr_x"), "resource/overviews/mp_rr_x.txt");
    }
    #[test]
    fn a_fractional_scale_keeps_its_fraction() {
        let doc = overview_doc("mp_rr_y", [-4788.0, 689.0], 16384.0, 2048);
        let text = kv::write(&doc);
        assert!(text.contains("scale 4\n"), "{text}");
        let doc = overview_doc("mp_rr_y", [0.0, 0.0], 5248.0, 3);
        assert!(kv::write(&doc).contains("scale 1.2812\n"), "{}", kv::write(&doc));
        assert!(kv::write(&overview_doc("x", [0.0, 0.0], 0.0, 0)).contains("scale 0\n"));
    }

    #[test]
    fn the_overview_image_is_found_by_the_guid_its_name_hashes_to() {
        assert_eq!(asset_path("mp_rr_district"), "ui_image/overviews/mp_rr_district.rpak");
        assert_eq!(pak::string_to_guid(&asset_path("mp_rr_district")).unwrap(), 0xE647_4E48_BBD4_991D);
    }

    /// A box the size of a room, drawn from above, covers the middle of the
    /// picture and leaves the corners alone.
    #[test]
    fn a_render_puts_the_geometry_inside_the_overview_square() {
        let mut bsp = Bsp::new(51, 0, 0);
        let verts: Vec<[f32; 3]> = vec![
            [-512.0, -512.0, 0.0], [512.0, -512.0, 0.0], [512.0, 512.0, 0.0], [-512.0, 512.0, 0.0],
            [-512.0, -512.0, 128.0], [512.0, -512.0, 128.0], [512.0, 512.0, 128.0], [-512.0, 512.0, 128.0],
        ];
        let mut pool = Vec::new();
        for v in &verts {
            pool.extend_from_slice(&v[0].to_le_bytes());
            pool.extend_from_slice(&v[1].to_le_bytes());
            pool.extend_from_slice(&v[2].to_le_bytes());
        }
        bsp.set_lump(rbsp::ids::VERTICES, pool);
        let indices: Vec<u16> = vec![0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7];
        bsp.set_lump(rbsp::ids::MESH_INDICES, indices.iter().flat_map(|i| i.to_le_bytes()).collect());
        let mesh = rbsp::lumps::render::Mesh { first_index: 0, triangle_count: 4, ..Default::default() };
        bsp.set_lump(rbsp::ids::MESHES, rbsp::lumps::render::write_meshes(&[mesh], Version::ClientV51));
        let image = render(&bsp, [-1024.0, -1024.0], 2048.0, 64).expect("draws");
        assert_eq!((image.width, image.height), (64, 64));
        let at = |x: usize, y: usize| {
            let o = (y * 64 + x) * 4;
            (image.pixels[o], image.pixels[o + 1], image.pixels[o + 2])
        };
        assert_ne!(at(32, 32), at(1, 1), "the middle is drawn and the corner is not");
        let drawn = (0..64 * 64).filter(|i| image.pixels[i * 4 + 2] > 60).count();
        assert!(drawn > 1000, "the floor covers a quarter of the picture, drew {drawn}");
    }

    #[test]
    fn a_map_with_no_geometry_says_so_instead_of_drawing_nothing() {
        let bsp = Bsp::new(51, 0, 0);
        assert!(render(&bsp, [0.0, 0.0], 1024.0, 32).is_err());
    }
}
