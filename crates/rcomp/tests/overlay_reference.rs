//! the reference-overlay path.

mod common;

use common::*;

#[test]
fn overlay_appends_models_valid() {
    // A reference map this compiler produced itself, written out as a map on
    // disk, so the overlay path is exercised without a shipped map's size.
    let base = compile(
        &small_map(),
        &Fs::new(),
        &Assets::new(),
        &CompileOptions { targets: Targets { client: true, dedi: false }, ..Default::default() },
    )
    .expect("the reference map compiles");
    let reference = base.client.expect("a client map");
    let dir = std::env::temp_dir().join(format!("r5ed-overlay-{}", Uuid::new_v4()));
    let maps = dir.join("maps");
    std::fs::create_dir_all(&maps).expect("the reference directory is created");
    let stem = "mp_rr_ref";
    reference
        .bsp
        .write_path(&maps.join(format!("{stem}.bsp")))
        .expect("the reference map is written");
    for (_, name) in &reference.ent_files {
        std::fs::write(maps.join(format!("{stem}_{name}.ent")), &reference.ent_bytes[name])
            .expect("a reference partition is written");
    }
    let reference_models = reference.bsp.lump(rbsp::ids::MODELS).len() / 64;
    let reference_meshes = reference.bsp.lump(rbsp::ids::MESHES).len() / 28;
    let reference_blocks = reference.entity_count();

    let layer = Layer { id: Uuid::new_v4(), name: "default".into(), visible: true, locked: false };
    let doc = MapDoc {
        format: rdoc::model::FORMAT_VERSION,
        meta: Default::default(),
        layers: vec![layer.clone()],
        objects: vec![
            new_object(
                "ref",
                layer.id,
                ObjectKind::Reference(Reference {
                    kind: ReferenceKind::ApexMap,
                    source: stem.into(),
                    promoted: Vec::new(),
                    // The reference's worldspawn block is entity zero.
                    removed_entities: vec![0],
                    sources: Default::default(),
                }),
            ),
            new_object(
                "added",
                layer.id,
                ObjectKind::Brush(box_brush(
                    DVec3::new(300.0, 300.0, 0.0),
                    DVec3::new(364.0, 364.0, 64.0),
                )),
            ),
            new_object(
                "added_light",
                layer.id,
                ObjectKind::Light(Light {
                    kind: LightKind::Point,
                    color: [1.0, 1.0, 1.0],
                    intensity: 1.0,
                    range: 128.0,
                    inner_angle: 0.0,
                    outer_angle: 0.0,
                    size: [0.0; 2],
                    cast_shadows: false,
                    baked: true,
                    realtime: false,
                    keyvalues: Default::default(),
                }),
            ),
        ],
        settings: Settings::default(),
    };
    let opts = CompileOptions {
        targets: Targets { client: true, dedi: false },
        reference_root: Some(dir.clone()),
        ..Default::default()
    };
    let out = compile(&doc, &Fs::new(), &Assets::new(), &opts).expect("the overlay compiles");
    let client = out.client.as_ref().expect("a client map");
    let issues = rbsp::validate(&client.bsp, &client.ent_bytes);
    let errors: Vec<String> = issues.iter().filter(|i| i.is_error()).map(|i| i.message.clone()).collect();
    assert!(errors.is_empty(), "the overlaid map has {} errors: {}", errors.len(), errors.join(" | "));
    let models = client.bsp.lump(rbsp::ids::MODELS).len() / 64;
    assert!(models >= reference_models, "{models} models against the reference's {reference_models}");
    // The authored brush lands in the world model, so the render surface count
    // stays put while the geometry behind it grows.
    let meshes = client.bsp.lump(rbsp::ids::MESHES).len() / 28;
    let reference_tris = reference.bsp.lump(rbsp::ids::MESH_INDICES).len() / 6;
    let tris = client.bsp.lump(rbsp::ids::MESH_INDICES).len() / 6;
    assert!(meshes >= 1, "the overlaid map has render meshes");
    // The world model has to reach the authored box, and the geometry behind it
    // has to grow.
    let world = model_record(&client.bsp, 0);
    assert!(world[3] >= 364.0, "the world model's maxs.x is {}", world[3]);
    assert!(tris > reference_tris, "{tris} triangles against the reference's {reference_tris}");
    assert!(client.bsp.lump(rbsp::ids::BVH_NODES).len() > 0, "the overlaid map has collision");
    // The reference carried its spawn point; the added brush and light join it,
    // and the worldspawn block the document dropped does not come back.
    let blocks = client.entity_count();
    assert!(
        blocks > reference_blocks - 1,
        "{blocks} entity blocks against the reference's {reference_blocks}"
    );
    let main = rbsp::lumps::entities::parse(client.bsp.lump(rbsp::ids::ENTITIES), rbsp::Version::ClientV51)
        .expect("the worldspawn lump parses");
    let worlds: Vec<_> = main.entities.iter().filter(|e| e.classname() == Some("worldspawn")).collect();
    assert_eq!(worlds.len(), 1, "the document authors exactly one worldspawn block");
    // The reference's own worldspawn was dropped; the one that survived is the
    // authored block, and its bounds span the reference world and the box on top.
    let maxs = worlds[0].get("world_maxs").expect("world_maxs").to_string();
    let bounds: Vec<f64> =
        maxs.split_whitespace().map(|v| v.parse::<f64>().unwrap_or(0.0)).collect();
    assert!(bounds[0] >= 364.0, "world_maxs is {maxs}, short of the authored box");
    let starts: usize = client
        .ent_bytes
        .values()
        .map(|data| {
            rbsp::lumps::entities::parse(data, rbsp::Version::ClientV51)
                .expect("a partition file parses")
                .entities
                .iter()
                .filter(|e| e.classname() == Some("info_player_start"))
                .count()
        })
        .sum();
    assert_eq!(starts, 1, "the reference's spawn point is carried over once");
    let _ = reference_meshes;
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(client.bsp.header.version, 51);
}

