//! The command line and the verbs, so an argument vector built somewhere else
//! can be checked against the tool that runs it without starting a process.
//!
//! Every edit goes through the same `rdoc` commands the editor applies, and
//! every command prints JSON so scripts and agents can chain them.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use glam::DVec3;
use rdoc::meshgen::{cmd_add_mesh, MeshRequest};
use rdoc::model::{Entity, KeyValues, Light, LightKind, ObjectKind, Prop, Terrain, TerrainLayer, Transform, DEFAULT_MATERIAL, DEV_MATERIALS};
use rdoc::project::{self, NativeProject};
use rdoc::{Command, Document, Id, MapDoc};
use serde_json::json;

mod assets;
mod bake;
mod bc7;
mod build;
mod importglb;
mod ctl;
mod importmap;
mod importtf2;
mod kv;
mod loadscreen;
mod minimap;
mod package;
mod pak;
mod propmesh;
mod register;
mod relight;
mod schema;
mod tools;
mod uiia;
mod vpk;
mod wraps;


/// The whole command line.
#[derive(Parser)]
#[command(name = "r5ed-compile", about = "Headless r5ed project authoring")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// One verb.
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Creates an empty project directory.
    New {
        project: PathBuf,
        #[arg(long, default_value = "mp_rr_untitled")]
        stem: String,
    },
    /// Applies an rdoc Command (JSON file, or `-` for stdin).
    Apply { project: PathBuf, command: String },
    /// Adds a box brush.
    Brush {
        project: PathBuf,
        #[arg(long, value_parser = vec3, allow_hyphen_values = true)]
        min: DVec3,
        #[arg(long, value_parser = vec3, allow_hyphen_values = true)]
        max: DVec3,
        #[arg(long, default_value = DEFAULT_MATERIAL)]
        material: String,
    },
    /// Adds an authored mesh from a MeshRequest (JSON file, or `-`).
    Mesh { project: PathBuf, request: String },
    /// Runs one mesh op request (`{op, object, sel, params}`, file or `-`).
    MeshOp { project: PathBuf, request: String },
    /// Prints counts, bounds, boundary/non-manifold/degenerate lists and slots.
    MeshInfo { project: PathBuf, id: String },
    /// Prints the `.r5mesh` behind an object as JSON.
    MeshDump { project: PathBuf, id: String },
    /// Makes a glb-sourced mesh editable (glb -> `.r5mesh`).
    MakeEditable { project: PathBuf, id: String },
    /// Converts a brush into an editable mesh.
    BrushToMesh { project: PathBuf, id: String },
    /// Joins mesh and brush objects into the active one (the first id).
    Join { project: PathBuf, ids: Vec<String> },
    /// Imports a Wavefront `.obj` as mesh objects and prints the report.
    ImportObj {
        project: PathBuf,
        obj: PathBuf,
        /// JSON ObjOptions: `{"scale": 64, "up": "y", "material_prefix": "world/obj/<map>"}`.
        #[arg(long)]
        options: Option<PathBuf>,
    },
    /// Imports a glTF binary `.glb` as mesh objects and prints the report.
    ImportGlb {
        project: PathBuf,
        glb: PathBuf,
        /// The same JSON ObjOptions `import-obj` takes.
        #[arg(long)]
        options: Option<PathBuf>,
    },
    /// Imports a Hammer `.vmf` and prints what was kept, converted and dropped.
    ImportVmf {
        project: PathBuf,
        vmf: PathBuf,
        /// JSON `{"texture_sizes": {"<material path>": [w, h]}}` for exact UVs.
        #[arg(long)]
        options: Option<PathBuf>,
    },
    /// Adds any entity by classname.
    Entity {
        project: PathBuf,
        #[arg(long)]
        class: String,
        #[arg(long, value_parser = vec3, allow_hyphen_values = true, default_value = "0,0,0")]
        origin: DVec3,
        #[arg(long, value_parser = vec3, allow_hyphen_values = true, default_value = "0,0,0")]
        angles: DVec3,
        /// Keyvalue as key=value; repeatable.
        #[arg(long = "kv")]
        kv: Vec<String>,
    },
    /// Adds a prop.
    Prop {
        project: PathBuf,
        #[arg(long)]
        model: String,
        #[arg(long, value_parser = vec3, allow_hyphen_values = true, default_value = "0,0,0")]
        origin: DVec3,
        #[arg(long, value_parser = vec3, allow_hyphen_values = true, default_value = "0,0,0")]
        angles: DVec3,
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        #[arg(long)]
        dynamic: bool,
    },
    /// Adds a light.
    Light {
        project: PathBuf,
        #[arg(long, default_value = "point")]
        kind: String,
        #[arg(long, value_parser = vec3, allow_hyphen_values = true, default_value = "0,0,128")]
        origin: DVec3,
        #[arg(long, value_parser = vec3, allow_hyphen_values = true, default_value = "1,1,1")]
        color: DVec3,
        #[arg(long, default_value_t = 1.0)]
        intensity: f32,
        #[arg(long, default_value_t = 512.0)]
        range: f32,
    },
    /// Generates a terrain from an rterrain recipe and adds it.
    Terrain {
        project: PathBuf,
        /// `{recipe, resolution:[w,h], cell_size, material?, origin?:[x,y,z]}` (file or `-`).
        request: String,
    },
    /// Compiles the project into its maps.
    Build {
        project: PathBuf,
        /// Build tree the maps are written into (default `<project>/build`).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Which targets to compile.
        #[arg(long, default_value = "client,dedi")]
        targets: String,
        /// Explicit path for the client map.
        #[arg(long)]
        client: Option<PathBuf>,
        /// Explicit path for the dedicated-server map.
        #[arg(long)]
        dedi: Option<PathBuf>,
        /// Where the client map's `.ent` partitions are written.
        #[arg(long)]
        ent_dir: Option<PathBuf>,
        #[arg(long, default_value = "none")]
        bake: String,
        /// Asset table JSON to compile against; several (comma separated or
        /// repeated) merge in order, a later file's entry winning.
        #[arg(long, value_delimiter = ',')]
        assets: Vec<PathBuf>,
        /// Resolve the asset table from the game install instead.
        #[arg(long)]
        install: bool,
        /// Also write the asset table the build compiled against, for a later
        /// `--assets` build on a machine without the install.
        #[arg(long)]
        write_assets: Option<PathBuf>,
        /// With --write-assets and --install: write every asset of these kinds
        /// the install carries (materials, models), not just the map's.
        #[arg(long, value_delimiter = ',')]
        all_assets: Vec<String>,
        /// Let a final bake from --assets run on defaults for names the tables
        /// do not carry instead of failing.
        #[arg(long)]
        allow_unresolved: bool,
        /// A source pak's repak manifest (`<stem>_src.json`): its materials are
        /// read from the json beside it, without the asset tool. Repeatable.
        #[arg(long)]
        src_pak: Vec<PathBuf>,
        /// Compile and bake even when the output directory already holds a
        /// build of the same inputs (its `build.json` matches), which is
        /// otherwise reused as it stands.
        #[arg(long)]
        rebuild: bool,
    },
    /// Lists every object.
    List { project: PathBuf },
    /// Copies every material, texture and model the document names into the
    /// map's own archive.
    Package {
        project: PathBuf,
        /// Staging directory (default `<project>/build/package`).
        stage: Option<PathBuf>,
        /// Where the archive is written (default the staging directory).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Encodes an image as the map's loading screen.
    Loadscreen {
        project: PathBuf,
        /// Image to encode; defaults to the document's `meta.loadscreen`.
        image: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Writes the map overview: the image and the document that names it.
    Minimap {
        project: PathBuf,
        /// Image to publish; defaults to a render of the compiled map.
        image: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Writes the launcher and playlist registration for the map.
    Register {
        project: PathBuf,
        /// Where the registration files go (default `<project>/build/deploy`).
        #[arg(long)]
        out: Option<PathBuf>,
        /// A game install whose launcher catalog this map is merged into.
        #[arg(long)]
        install: Option<PathBuf>,
    },
    /// Imports a map the install carries as a project.
    ImportMap {
        stem: String,
        project: PathBuf,
        /// A map directory, a `.bsp`, or a pak to export the map out of.
        #[arg(long)]
        from: Option<PathBuf>,
        /// The dedicated-server half, when it is beside the client one.
        #[arg(long)]
        dedi_from: Option<PathBuf>,
        /// Scratch directory for the export.
        #[arg(long)]
        cache: Option<PathBuf>,
        /// Import over an existing document.
        #[arg(long)]
        force: bool,
    },
    /// Turns an imported map into a document of its own: every object that
    /// stood for a record of the reference compiles from the document, and the
    /// reference is removed. The build no longer reads the shipped map.
    Detach { project: PathBuf },
    /// Imports a Titanfall 2 v37 map as a project whose build converts it.
    ImportTf2 {
        /// The v37 source map, with its sidecars beside it.
        bsp: PathBuf,
        project: PathBuf,
        /// Stem the converted map is built under.
        #[arg(long)]
        stem: Option<String>,
        /// The install's surface property table.
        #[arg(long)]
        surface_properties: Option<PathBuf>,
        /// A `.vmt` tree; the first directory naming a material wins.
        #[arg(long = "vmt-dir")]
        vmt_dirs: Vec<PathBuf>,
        /// A directory of extracted material json, which wins over a `.vmt` tree.
        #[arg(long = "material-dir")]
        material_dirs: Vec<PathBuf>,
        /// Import over an existing project.
        #[arg(long)]
        force: bool,
    },
    /// Solves the lighting on its own and caches the product beside the project.
    Bake {
        project: PathBuf,
        #[arg(long, default_value = "final")]
        mode: String,
        /// Build tree the maps are written into (default `<project>/build`).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Which targets to write.
        #[arg(long, default_value = "client,dedi")]
        targets: String,
    },
    /// Lights a compiled client map again in place: only the lighting lumps
    /// change, baked into the lightmap atlas the map's vertices already use.
    Relight {
        /// The client `.bsp`.
        map: PathBuf,
        /// Scene JSON: settings, lights and the props that never occlude.
        scene: PathBuf,
        /// Where the relit `.bsp` is written, with its sidecars.
        #[arg(long)]
        out: Option<PathBuf>,
        /// A directory of converted models (`<path>.rmdl` + `.vg`); repeatable.
        #[arg(long)]
        models: Vec<PathBuf>,
        #[arg(long, default_value = "final")]
        mode: String,
        /// Where the cubemap array baked from the new lighting is written (.dds).
        #[arg(long)]
        cubemap: Option<PathBuf>,
        /// A directory each lightmap page is written to as a PNG.
        #[arg(long)]
        preview: Option<PathBuf>,
        /// Shade only `x0,y0,z0,x1,y1,z1`; writes the preview and nothing else.
        #[arg(long, value_delimiter = ',', allow_hyphen_values = true)]
        region: Option<Vec<f32>>,
        /// Explain texel `page,x,y` instead of baking; repeatable.
        #[arg(long)]
        probe: Vec<String>,
        /// Explain the texel nearest world point `x,y,z`; repeatable.
        #[arg(long, allow_hyphen_values = true)]
        probe_world: Vec<String>,
    },
    /// Prints a compiled map's world lights as the scene lights `relight` takes.
    RelightLights { map: PathBuf },
    /// Points every mesh and static prop of a map at its nearest cubemap, in place.
    RelightCubemaps { map: PathBuf },
    /// Prints the world meshes whose material names one of the given names.
    RelightWorld {
        map: PathBuf,
        #[arg(long, value_delimiter = ',')]
        materials: Vec<String>,
    },
    /// Prints models' hulls and per-mesh material, box and normal.
    RelightModels {
        #[arg(long)]
        models: Vec<PathBuf>,
        /// Also print every mesh's positions, normals, uv0 and indices.
        #[arg(long)]
        triangles: bool,
        names: Vec<String>,
    },
    /// Prints the entity schema this build carries.
    Schema,
    /// Prints one object.
    Show { project: PathBuf, id: String },
    /// Removes objects by id.
    Remove { project: PathBuf, ids: Vec<String> },
    /// Checks the document and every entity against the schema.
    Validate { project: PathBuf },
    /// Prints the dev material set.
    Materials,
    /// Prints every entity classname in the schema.
    Classes,
    /// Drives the user's open editor through the host relay.
    Ctl {
        /// The control verb, e.g. `state` or `screenshot`.
        verb: String,
        /// Params JSON: inline, a file path, or `-` for stdin. Empty means `{}`.
        json: Option<String>,
        /// Where a screenshot's PNG goes. Required for `screenshot`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn vec3(s: &str) -> std::result::Result<DVec3, String> {
    let v: Vec<f64> = s.split(',').map(|p| p.trim().parse::<f64>()).collect::<std::result::Result<_, _>>().map_err(|e| e.to_string())?;
    if v.len() != 3 || v.iter().any(|x| !x.is_finite()) {
        return Err("expected x,y,z".into());
    }
    Ok(DVec3::new(v[0], v[1], v[2]))
}

fn read_input(arg: &str) -> Result<String> {
    if arg == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s)
    } else {
        std::fs::read_to_string(arg).with_context(|| format!("reading {arg}"))
    }
}

fn open(project: &Path) -> Result<Document> {
    let fs = NativeProject::new(project).map_err(|e| anyhow::anyhow!("{e}"))?;
    project::load(Box::new(fs)).map_err(|e| anyhow::anyhow!("{}: {e}", project.display()))
}

fn save(project: &Path, doc: &Document) -> Result<()> {
    let mut fs = NativeProject::new(project).map_err(|e| anyhow::anyhow!("{e}"))?;
    project::save(&mut fs, &doc.doc).map_err(|e| anyhow::anyhow!("{e}"))
}

fn apply(project: &Path, cmd: Command) -> Result<Vec<Id>> {
    let mut doc = open(project)?;
    let (_, ids) = doc.apply(cmd).map_err(|e| anyhow::anyhow!("{e}"))?;
    save(project, &doc)?;
    Ok(ids)
}

fn layer(project: &Path) -> Result<Id> {
    open(project)?.layer_ids().first().copied().context("the document has no layer")
}

fn add(project: &Path, name: &str, kind: ObjectKind, transform: Transform) -> Result<Vec<Id>> {
    let mut o = rdoc::doc::new_object(name, layer(project)?, kind);
    o.transform = transform;
    apply(project, Command::AddObjects { objects: vec![o] })
}

fn at(origin: DVec3, angles: DVec3) -> Transform {
    Transform::from_source_angles(origin, angles)
}

fn print_ids(ids: &[Id]) {
    println!("{}", json!({ "ids": ids.iter().map(|i| i.to_string()).collect::<Vec<_>>() }));
}

/// Counts, bounds, boundary/non-manifold/degenerate lists and material slots.
fn mesh_info(doc: &Document, id: Id) -> Result<serde_json::Value> {
    let o = doc.object(id).context("no object with that id")?;
    let ObjectKind::Mesh(mesh) = &o.kind else { bail!("object {id} is not a mesh") };
    let bytes =
        doc.files.read(&mesh.source).map_err(|e| anyhow::anyhow!("{}: {e}", mesh.source))?;
    let editable = rdoc::editmesh::io::read(&bytes).map_err(|e| anyhow::anyhow!("{}: {e}", mesh.source))?;
    let adj = rdoc::editmesh::Adjacency::build(&editable);
    let inv = rdoc::editmesh::validate::invariants(&editable);
    let bounds = doc.object_bounds(id).map(|b| json!({ "min": b.min.to_array(), "max": b.max.to_array() }));
    let stretch = serde_json::to_value(rdoc::editmesh::ops::uv::stretch_report(&editable)).unwrap_or(json!({}));
    Ok(json!({
        "id": id.to_string(),
        "source": mesh.source,
        "verts": editable.positions.len(),
        "edges": adj.edges.len(),
        "faces": editable.faces.len(),
        "bounds": bounds,
        "closed": inv.closed,
        "wound": inv.wound,
        "euler": inv.euler,
        "boundary": inv.boundary.iter().map(|(a, b)| vec![a, b]).collect::<Vec<_>>(),
        "non_manifold": inv.non_manifold.iter().map(|(a, b)| vec![a, b]).collect::<Vec<_>>(),
        "degenerate": inv.degenerate,
        "materials": editable.materials,
        "stretch": stretch,
    }))
}

/// Parses an argument vector, the program name included at index zero. Nothing
/// is printed: a caller that wants the usage text asks clap for it.
pub fn parse<I, T>(args: I) -> Result<Cmd, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    Cli::try_parse_from(args).map(|c| c.cmd)
}

/// Runs one parsed command line, printing what the verb produced.
pub fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::New { project, stem } => {
            if project.join("map.json").exists() {
                bail!("{} already has a map.json", project.display());
            }
            std::fs::create_dir_all(&project)?;
            let mut doc = MapDoc::default();
            doc.meta.stem = stem;
            let mut fs = NativeProject::new(&project).map_err(|e| anyhow::anyhow!("{e}"))?;
            project::save(&mut fs, &doc).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", json!({ "project": project.display().to_string(), "stem": doc.meta.stem }));
        }
        Cmd::Apply { project, command } => {
            let cmd: Command = serde_json::from_str(&read_input(&command)?)?;
            print_ids(&apply(&project, cmd)?);
        }
        Cmd::ImportObj { project, obj, options } => {
            let text = std::fs::read_to_string(&obj).with_context(|| format!("{}", obj.display()))?;
            let opts: rdoc::import::obj::ObjOptions = match options {
                Some(p) => serde_json::from_str(&std::fs::read_to_string(&p).with_context(|| format!("{}", p.display()))?)?,
                None => Default::default(),
            };
            let doc = open(&project)?;
            let (cmd, report) = rdoc::import::obj::import(&doc, &text, &opts).map_err(|e| anyhow::anyhow!("{e}"))?;
            let ids = apply(&project, cmd)?;
            println!("{}", serde_json::json!({ "objects": ids.len(), "report": report }));
        }
        Cmd::ImportGlb { project, glb, options } => {
            let bytes = std::fs::read(&glb).map_err(|e| anyhow::anyhow!("{}: {e}", glb.display()))?;
            let opts: rdoc::import::obj::ObjOptions = match options {
                Some(p) => serde_json::from_str(&std::fs::read_to_string(&p).with_context(|| format!("{}", p.display()))?)?,
                None => Default::default(),
            };
            let doc = open(&project)?;
            let (cmd, report) = importglb::import(&doc, &bytes, &opts)?;
            let ids = apply(&project, cmd)?;
            println!("{}", serde_json::json!({ "objects": ids.len(), "report": report }));
        }
        Cmd::ImportVmf { project, vmf, options } => {
            let text = std::fs::read_to_string(&vmf).with_context(|| format!("{}", vmf.display()))?;
            let mut opts: rdoc::import::vmf::VmfOptions = match options {
                Some(p) => serde_json::from_str(&std::fs::read_to_string(&p).with_context(|| format!("{}", p.display()))?)?,
                None => Default::default(),
            };
            if opts.schema_classes.is_none() {
                let schema = rdoc::schema::EntitySchema::load(include_str!("../../../schema/entities.json"))
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                opts.schema_classes = Some(schema.classes.keys().cloned().collect());
            }
            let doc = open(&project)?;
            let (cmd, report) = rdoc::import::vmf::import_with(&doc, &text, &opts).map_err(|e| anyhow::anyhow!("{e}"))?;
            let ids = apply(&project, cmd)?;
            println!("{}", serde_json::json!({ "objects": ids.len(), "report": report }));
        }
        Cmd::Brush { project, min, max, material } => {
            let cmd = rdoc::brush::cmd_box(layer(&project)?, None, min, max, &material).map_err(|e| anyhow::anyhow!("{e}"))?;
            print_ids(&apply(&project, cmd)?);
        }
        Cmd::Mesh { project, request } => {
            let req: MeshRequest = serde_json::from_str(&read_input(&request)?)?;
            let cmd = cmd_add_mesh(layer(&project)?, None, &req).map_err(|e| anyhow::anyhow!("{e}"))?;
            print_ids(&apply(&project, cmd)?);
        }
        Cmd::MeshOp { project, request } => {
            let req: rdoc::editmesh::OpRequest = serde_json::from_str(&read_input(&request)?)?;
            let doc = open(&project)?;
            let out = rdoc::editmesh::plan(&doc, &req).map_err(|e| anyhow::anyhow!("{e}"))?;
            let ids = apply(&project, out.command.clone())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ids": ids.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                    "selection": out.selection,
                    "report": out.report,
                }))?
            );
        }
        Cmd::MeshInfo { project, id } => {
            let doc = open(&project)?;
            let id: Id = id.parse()?;
            println!("{}", serde_json::to_string_pretty(&mesh_info(&doc, id)?)?);
        }
        Cmd::MeshDump { project, id } => {
            let doc = open(&project)?;
            let id: Id = id.parse()?;
            let o = doc.object(id).context("no object with that id")?;
            let ObjectKind::Mesh(mesh) = &o.kind else { bail!("object {id} is not a mesh") };
            let bytes = doc.files.read(&mesh.source).map_err(|e| anyhow::anyhow!("{}: {e}", mesh.source))?;
            let editable = rdoc::editmesh::io::read(&bytes).map_err(|e| anyhow::anyhow!("{}: {e}", mesh.source))?;
            println!("{}", serde_json::to_string_pretty(&rdoc::editmesh::io::to_json(&editable))?);
        }
        Cmd::MakeEditable { project, id } => {
            let doc = open(&project)?;
            let id: Id = id.parse()?;
            let cmd = rdoc::editmesh::convert::cmd_make_editable(&doc, id).map_err(|e| anyhow::anyhow!("{e}"))?;
            print_ids(&apply(&project, cmd)?);
        }
        Cmd::BrushToMesh { project, id } => {
            let doc = open(&project)?;
            let id: Id = id.parse()?;
            let cmd = rdoc::editmesh::convert::cmd_brush_to_mesh(&doc, id).map_err(|e| anyhow::anyhow!("{e}"))?;
            print_ids(&apply(&project, cmd)?);
        }
        Cmd::Join { project, ids } => {
            let ids = ids.iter().map(|s| s.parse::<Id>()).collect::<std::result::Result<Vec<_>, _>>()?;
            let (active, sources) = ids.split_first().context("join needs an active id and at least one source id")?;
            let doc = open(&project)?;
            let cmd = rdoc::editmesh::ops::join::cmd_join(&doc, *active, sources).map_err(|e| anyhow::anyhow!("{e}"))?;
            print_ids(&apply(&project, cmd)?);
        }
        Cmd::Entity { project, class, origin, angles, kv } => {
            let mut keyvalues = KeyValues::new();
            for pair in kv {
                let (k, v) = pair.split_once('=').context("--kv expects key=value")?;
                keyvalues.insert(k.to_string(), v.to_string());
            }
            let schema = rdoc::schema::EntitySchema::load(include_str!("../../../schema/entities.json")).map_err(|e| anyhow::anyhow!("{e}"))?;
            let Some(def) = schema.class(&class) else { bail!("{class} is not a known entity class (see `classes`)") };
            for (name, key) in &def.keys {
                if key.required && !keyvalues.contains_key(name) && !matches!(name.as_str(), "classname" | "origin" | "angles") {
                    if let Some(v) = &key.default {
                        keyvalues.insert(name.clone(), v.clone());
                    }
                }
            }
            let kind = ObjectKind::Entity(Entity { classname: class.clone(), keyvalues, partition: None, links: Default::default() });
            print_ids(&add(&project, &class, kind, at(origin, angles))?);
        }
        Cmd::Prop { project, model, origin, angles, scale, dynamic } => {
            let prop = Prop { model: model.clone(), skin: 0, static_prop: !dynamic, solid: true, cast_shadows: true, fade_distance: 0.0, keyvalues: Default::default() };
            let mut t = at(origin, angles);
            t.scale = DVec3::splat(scale);
            let name = model.rsplit('/').next().unwrap_or("prop").to_string();
            print_ids(&add(&project, &name, ObjectKind::Prop(prop), t)?);
        }
        Cmd::Light { project, kind, origin, color, intensity, range } => {
            let kind = match kind.as_str() {
                "sun" => LightKind::Sun,
                "point" => LightKind::Point,
                "spot" => LightKind::Spot,
                "area" => LightKind::Area,
                "sky" => LightKind::Sky,
                other => bail!("unknown light kind {other}"),
            };
            let light = Light {
                kind,
                color: [color.x as f32, color.y as f32, color.z as f32],
                intensity,
                range,
                inner_angle: 30.0,
                outer_angle: 45.0,
                size: [64.0, 64.0],
                cast_shadows: true,
                baked: true,
                realtime: false,
                keyvalues: Default::default(),
            };
            print_ids(&add(&project, "light", ObjectKind::Light(light), at(origin, DVec3::ZERO))?);
        }
        Cmd::Terrain { project, request } => {
            #[derive(serde::Deserialize)]
            struct Req {
                recipe: serde_json::Value,
                resolution: [u32; 2],
                cell_size: f64,
                #[serde(default)]
                material: Option<String>,
                #[serde(default)]
                origin: Option<[f64; 3]>,
            }
            let req: Req = serde_json::from_str(&read_input(&request)?)?;
            let recipe = rterrain::Recipe::from_json(&req.recipe.to_string()).map_err(|e| anyhow::anyhow!("{e}"))?;
            let hf = recipe.evaluate(req.resolution, req.cell_size as f32).map_err(|e| anyhow::anyhow!("{e}"))?;
            let id = uuid::Uuid::new_v4();
            let heights = format!("terrain/{id}.f32");
            let weights = format!("terrain/{id}_0.u8");
            let n = (req.resolution[0] * req.resolution[1]) as usize;
            let terrain = Terrain {
                heights: heights.clone(),
                resolution: req.resolution,
                cell_size: req.cell_size,
                layers: vec![TerrainLayer {
                    name: "base".into(),
                    material: req.material.unwrap_or_else(|| DEFAULT_MATERIAL.into()),
                    weights: weights.clone(),
                    uv_scale: 1.0 / rdoc::model::UV_TEXELS_PER_TILE,
                    scatter: Vec::new(),
                }],
                chunk_size: 64,
                collision: true,
                contents: 1,
                lightmap_scale: 0.0,
                recipe: Some(req.recipe),
            };
            let mut o = rdoc::doc::new_object("terrain", layer(&project)?, ObjectKind::Terrain(terrain));
            o.id = id;
            if let Some(p) = req.origin {
                o.transform.position = DVec3::from_array(p);
            }
            let cmd = Command::Batch {
                label: "terrain".into(),
                commands: vec![
                    Command::SetFile { path: heights, bytes: Some(hf.to_le_bytes()) },
                    Command::SetFile { path: weights, bytes: Some(vec![255u8; n]) },
                    Command::AddObjects { objects: vec![o] },
                ],
            };
            print_ids(&apply(&project, cmd)?);
        }
        Cmd::Build { project, out, targets, client, dedi, ent_dir, bake, assets, install, write_assets, all_assets, allow_unresolved, src_pak, rebuild } => {
            let args = build::Args {
                project,
                out,
                targets,
                client,
                dedi,
                ent_dir,
                bake,
                assets,
                install,
                write_assets,
                all_assets,
                allow_unresolved,
                src_paks: src_pak,
                rebuild,
            };
            println!("{}", serde_json::to_string_pretty(&build::run(&args)?)?);
        }
        Cmd::Package { project, stage, out } => {
            let args = package::Args { project, stage, out };
            println!("{}", serde_json::to_string_pretty(&package::run(&args)?)?);
        }
        Cmd::Loadscreen { project, image, out } => {
            let args = loadscreen::Args { project, image, out };
            println!("{}", serde_json::to_string_pretty(&loadscreen::run(&args)?)?);
        }
        Cmd::Minimap { project, image, out } => {
            let args = minimap::Args { project, image, out };
            println!("{}", serde_json::to_string_pretty(&minimap::run(&args)?)?);
        }
        Cmd::Register { project, out, install } => {
            let args = register::Args { project, out, install };
            println!("{}", serde_json::to_string_pretty(&register::run(&args)?)?);
        }
        Cmd::ImportMap { stem, project, from, dedi_from, cache, force } => {
            let args = importmap::Args { stem, project, from, dedi_from, cache, force };
            println!("{}", serde_json::to_string_pretty(&importmap::run(&args)?)?);
        }
        Cmd::Detach { project } => {
            let doc = open(&project)?;
            let before = doc.doc.objects.len();
            let halves = importmap::kept_reference(&project, &doc.doc)?;
            let props_map = halves.as_ref().map(|(c, d)| rcomp::decompile::prop_source(c, d.as_ref()));
            let detached = rcomp::decompile::detach(&doc.doc, props_map).map_err(|e| anyhow::anyhow!("{e}"))?;
            if detached.objects.len() == before && !doc.doc.objects.iter().any(|o| matches!(o.kind, ObjectKind::Reference(_))) {
                anyhow::bail!("{} references no map; there is nothing to detach", project.display());
            }
            let mut fs = NativeProject::new(&project).map_err(|e| anyhow::anyhow!("{e}"))?;
            project::save(&mut fs, &detached).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "project": project.display().to_string(),
                    "objects": detached.objects.len(),
                    "removed": before - detached.objects.len(),
                }))?
            );
        }
        Cmd::ImportTf2 { bsp, project, stem, surface_properties, vmt_dirs, material_dirs, force } => {
            let stem = match stem {
                Some(s) => s,
                None => {
                    let source = bsp
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .ok_or_else(|| anyhow::anyhow!("{} has no file name to take a stem from", bsp.display()))?;
                    importtf2::stem_for(&source)
                }
            };
            let args =
                importtf2::Args { bsp, project, stem, surface_properties, vmt_dirs, material_dirs, force };
            println!("{}", serde_json::to_string_pretty(&importtf2::run(&args)?)?);
        }
        Cmd::Bake { project, mode, out, targets } => {
            let args = bake::Args { project, mode, out, targets };
            println!("{}", serde_json::to_string_pretty(&bake::run(&args)?)?);
        }
        Cmd::Relight { map, scene, out, models, mode, cubemap, preview, region, probe, probe_world } => {
            let probe = probe
                .iter()
                .map(|t| {
                    let v: Vec<u32> = t.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
                    match v.as_slice() {
                        [p, x, y] => Ok([*p, *x, *y]),
                        _ => bail!("--probe takes page,x,y, got {t}"),
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            let region = match region {
                Some(r) if r.len() == 6 => Some([r[0], r[1], r[2], r[3], r[4], r[5]]),
                Some(r) => bail!("--region takes six numbers, got {}", r.len()),
                None => None,
            };
            let probe_world = probe_world
                .iter()
                .map(|t| {
                    let v: Vec<f32> = t.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
                    match v.as_slice() {
                        [x, y, z] => Ok([*x, *y, *z]),
                        _ => bail!("--probe-world takes x,y,z, got {t}"),
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            let args = relight::Args { map, scene, out, models, mode, cubemap, preview, region, probe, probe_world };
            println!("{}", serde_json::to_string_pretty(&relight::run(&args)?)?);
        }
        Cmd::RelightModels { models, triangles, names } => {
            println!("{}", serde_json::to_string(&relight::model_report(&models, &names, triangles)?)?)
        }
        Cmd::RelightWorld { map, materials } => {
            println!("{}", serde_json::to_string(&relight::world_meshes(&map, &materials)?)?)
        }
        Cmd::RelightCubemaps { map } => println!("{}", serde_json::to_string_pretty(&relight::assign_cubemaps(&map)?)?),
        Cmd::RelightLights { map } => println!("{}", serde_json::to_string_pretty(&relight::shipped_lights(&map)?)?),
        Cmd::Schema => println!("{}", serde_json::to_string_pretty(&schema::run()?)?),
        Cmd::List { project } => {
            let doc = open(&project)?;
            let rows: Vec<_> = doc
                .doc
                .objects
                .iter()
                .map(|o| {
                    let kind = serde_json::to_value(&o.kind).ok().and_then(|v| v.get("type").cloned()).unwrap_or_default();
                    let class = match &o.kind {
                        ObjectKind::Entity(e) => Some(e.classname.clone()),
                        ObjectKind::Prop(p) => Some(p.model.clone()),
                        _ => None,
                    };
                    json!({ "id": o.id.to_string(), "name": o.name, "kind": kind, "detail": class, "position": o.transform.position.to_array() })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        Cmd::Show { project, id } => {
            let doc = open(&project)?;
            let id: Id = id.parse()?;
            let o = doc.object(id).context("no object with that id")?;
            println!("{}", serde_json::to_string_pretty(o)?);
        }
        Cmd::Remove { project, ids } => {
            let ids = ids.iter().map(|s| s.parse::<Id>()).collect::<std::result::Result<Vec<_>, _>>()?;
            print_ids(&apply(&project, Command::RemoveObjects { ids })?);
        }
        Cmd::Validate { project } => {
            let doc = open(&project)?;
            let schema = rdoc::schema::EntitySchema::load(include_str!("../../../schema/entities.json")).map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut issues = Vec::new();
            if let Err(e) = rdoc::doc::validate(&doc.doc) {
                issues.push(json!({ "severity": "error", "id": null, "message": e.to_string() }));
            }
            for o in &doc.doc.objects {
                for i in schema.validate_object(o) {
                    issues.push(json!({ "severity": format!("{:?}", i.severity).to_lowercase(), "id": i.object.map(|x| x.to_string()), "message": i.message }));
                }
            }
            println!("{}", serde_json::to_string_pretty(&issues)?);
        }
        Cmd::Ctl { verb, json, out } => {
            ctl::run_ctl(&verb, json.as_deref(), out.as_deref())?;
        }
        Cmd::Materials => println!("{}", serde_json::to_string_pretty(DEV_MATERIALS)?),
        Cmd::Classes => {
            let schema = rdoc::schema::EntitySchema::load(include_str!("../../../schema/entities.json")).map_err(|e| anyhow::anyhow!("{e}"))?;
            let v: serde_json::Value = serde_json::from_str(&schema.to_json())?;
            let names: Vec<String> = v.get("classes").and_then(|c| c.as_object()).map(|m| m.keys().cloned().collect()).unwrap_or_default();
            println!("{}", serde_json::to_string_pretty(&names)?);
        }
    }
    Ok(())
}
