//! entity emission against the schema and the shipped maps.

mod common;

use common::*;

#[test]
fn every_schema_class_emits() {
    let schema = rcomp::default_schema();
    assert!(schema.len() > 100, "the schema carries the factory classes");
    let layer = Layer { id: Uuid::new_v4(), name: "default".into(), visible: true, locked: false };
    let mut objects = Vec::new();
    let mut expected: Vec<(String, String)> = Vec::new();
    for (i, classname) in schema.classes.keys().enumerate() {
        let mut keyvalues: rdoc::model::KeyValues = Default::default();
        for (name, def) in &schema.classes[classname].keys {
            if !def.required {
                continue;
            }
            if matches!(name.as_str(), "origin" | "angles" | "classname" | "model" | "targetname") {
                continue;
            }
            let observed = def.default.clone().or_else(|| def.observed_values.first().cloned());
            let value = match observed {
                Some(v) if def.ty.accepts(&v) => v,
                _ => default_for(name, def.ty),
            };
            keyvalues.insert(name.clone(), value);
        }
        if classname == "worldspawn" {
            continue;
        }
        objects.push(new_object(
            &format!("e{i}"),
            layer.id,
            ObjectKind::Entity(Entity {
                classname: classname.clone(),
                keyvalues,
                partition: None,
                links: Default::default(),
            }),
        ));
        expected.push((classname.clone(), classname.clone()));
    }
    let doc = MapDoc {
        format: rdoc::model::FORMAT_VERSION,
        meta: Default::default(),
        layers: vec![layer],
        objects,
        settings: Settings::default(),
    };
    let opts = rcomp::CompileOptions { skip_rooms: true, ..options() };
    let out = compile(&doc, &Fs::new(), &Assets::new(), &opts).expect("every class emits");
    let client = out.client.as_ref().expect("a client map");
    let mut all: Vec<String> = Vec::new();
    let main = rbsp::lumps::entities::parse(
        client.bsp.lump(rbsp::ids::ENTITIES),
        rbsp::Version::ClientV51,
    )
    .expect("the worldspawn lump parses");
    all.extend(main.entities.iter().filter_map(|e| e.classname().map(str::to_string)));
    for data in client.ent_bytes.values() {
        let text = rbsp::lumps::entities::parse(data, rbsp::Version::ClientV51)
            .expect("a partition file parses");
        all.extend(text.entities.iter().filter_map(|e| e.classname().map(str::to_string)));
    }
    // The document's own worldspawn block is skipped, so the schema class of
    // the same name has no authored entity of its own.
    let authored: Vec<&String> = expected.iter().map(|(c, _)| c).filter(|c| *c != "worldspawn").collect();
    for class in &authored {
        assert!(
            all.iter().any(|c| c == *class),
            "class {class} does not appear in the emitted map"
        );
    }
    assert_eq!(all.len(), authored.len() + 1, "one worldspawn block plus one block per class");
}

#[test]
fn zipline_matches_fixture_structure() {
    let layer = Layer { id: Uuid::new_v4(), name: "default".into(), visible: true, locked: false };
    let points = vec![
        DVec3::new(0.0, 0.0, 128.0),
        DVec3::new(0.0, 256.0, 96.0),
        DVec3::new(0.0, 512.0, 64.0),
    ];
    let doc = MapDoc {
        format: rdoc::model::FORMAT_VERSION,
        meta: Default::default(),
        layers: vec![layer.clone()],
        objects: vec![new_object(
            "line",
            layer.id,
            ObjectKind::Path(PathObject { kind: PathKind::Zipline, points, keyvalues: Default::default() }),
        )],
        settings: Settings::default(),
    };
    let out = compile(&doc, &Fs::new(), &Assets::new(), &options()).expect("the path compiles");
    let client = out.client.as_ref().expect("a client map");
    let script = client.ent_bytes.get("script").expect("ziplines live in the script partition");
    let text = String::from_utf8_lossy(script);
    let blocks = rdoc::enttext::parse(&text).expect("the partition parses").1;
    let ziplines: Vec<_> = blocks.iter().filter(|b| b.classname() == Some("zipline")).collect();
    let ends: Vec<_> = blocks.iter().filter(|b| b.classname() == Some("zipline_end")).collect();
    assert_eq!(ziplines.len(), 1, "one head zipline");
    assert_eq!(ends.len(), 1, "one end entity");
    let head = &ziplines[0].keyvalues;
    let end = &ends[0].keyvalues;
    for i in 0..3 {
        let key = format!("_zipline_rest_point_{i}");
        assert!(head.contains_key(&key), "the head carries {key}");
    }
    assert_eq!(
        head.get("link_to_guid_0"),
        end.get("link_guid"),
        "the head links to the end"
    );
    assert!(head.contains_key("link_guid"), "the head has its own guid");
    assert!(end.contains_key("ZiplineAutoDetachDistance"), "the end carries the auto-detach key");
    for key in ["Material", "Width", "ZiplineLengthScale", "ZiplineSpeedScale", "ZiplineVertical"] {
        assert!(head.contains_key(key), "the fixture key {key} is emitted");
    }
}

#[test]
fn light_entities_match_fixture_format() {
    let layer = Layer { id: Uuid::new_v4(), name: "default".into(), visible: true, locked: false };
    let mut objects = Vec::new();
    for (name, kind) in [
        ("point", LightKind::Point),
        ("spot", LightKind::Spot),
        ("sun", LightKind::Sun),
        ("sky", LightKind::Sky),
    ] {
        objects.push(new_object(
            name,
            layer.id,
            ObjectKind::Light(Light {
                kind,
                color: [1.0, 0.5, 0.25],
                intensity: 2.0,
                range: 300.0,
                inner_angle: 30.0,
                outer_angle: 60.0,
                size: [0.0; 2],
                cast_shadows: true,
                baked: true,
                realtime: false,
                keyvalues: Default::default(),
            }),
        ));
    }
    let doc = MapDoc {
        format: rdoc::model::FORMAT_VERSION,
        meta: Default::default(),
        layers: vec![layer],
        objects,
        settings: Settings::default(),
    };
    let out = compile(&doc, &Fs::new(), &Assets::new(), &options()).expect("the lights compile");
    let client = out.client.as_ref().expect("a client map");
    let env = client.ent_bytes.get("env").expect("lights live in the env partition");
    let blocks = rdoc::enttext::parse(&String::from_utf8_lossy(env)).expect("the partition parses").1;
    let of = |cn: &str| {
        blocks
            .iter()
            .find(|b| b.classname() == Some(cn))
            .unwrap_or_else(|| panic!("no {cn} in the env partition"))
            .keyvalues
            .clone()
    };
    let point = of("light");
    assert_eq!(point["_light"], "255 128 64 2", "_light is r g b brightness");
    assert_eq!(point["_distance"], "300");
    assert_eq!(point["_fifty_percent_distance"], "90");
    assert_eq!(point["_zero_percent_distance"], "300");
    assert_eq!(point["_emitter_radius"], "0");
    assert_eq!(point["_specular_intensity"], "1");
    assert_eq!(point["style"], "0");
    assert!(point.contains_key("origin"));
    let spot = of("light_spot");
    assert_eq!(spot["_inner_cone"], "30");
    assert_eq!(spot["_cone"], "60");
    assert_eq!(spot["shadow"], "1");
    assert!(spot.contains_key("pitch"));
    assert!(spot.contains_key("angles"));
    let sun = of("light_environment");
    assert_eq!(sun["lightEnvironmentIndex"], "*0");
    assert_eq!(sun["cloudoffset"], "0 0 0 0");
    assert!(sun.contains_key("angles"));
    let sky = of("sky_camera");
    assert_eq!(sky["targetname"], "skybox_cam_level");
    assert_eq!(sky["skyscale"], "1000");
    assert!(sky.contains_key("fogcolor"));
}

