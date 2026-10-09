use rdoc::schema::{self, ClassKind, KeyType, MapSource, SchemaInput};

const FACTORIES: &str = include_str!("../../../schema/engine/server_factories.json");
const DATAMAPS: &str = include_str!("../../../schema/engine/server_datamaps.json");

#[test]
fn every_light_kind_compiles_as_a_class_the_schema_knows() {
    let schema = schema::EntitySchema::load(include_str!("../../../schema/entities.json"))
        .expect("the shipped schema loads");
    for kind in [rdoc::model::LightKind::Sun, rdoc::model::LightKind::Point, rdoc::model::LightKind::Spot,
                 rdoc::model::LightKind::Area, rdoc::model::LightKind::Sky] {
        let object = rdoc::model::Object {
            id: uuid::Uuid::new_v4(),
            name: "l".into(),
            layer: uuid::Uuid::new_v4(),
            parent: None,
            transform: rdoc::model::Transform::default(),
            hidden: false,
            locked: false,
            kind: rdoc::model::ObjectKind::Light(rdoc::model::Light {
                kind,
                color: [1.0; 3],
                intensity: 1.0,
                range: 0.0,
                inner_angle: 0.0,
                outer_angle: 0.0,
                size: [0.0; 2],
                cast_shadows: true,
                baked: true,
                realtime: false,
                keyvalues: Default::default(),
            }),
        };
        let (classname, _) = schema::entity_of(&object).expect("a light compiles as an entity");
        assert!(schema.class(classname).is_some(), "{kind:?} compiles as {classname}, which the schema lacks");
    }
}

/// The engine tables on their own: every factory classname has to be there.
fn engine_schema() -> schema::EntitySchema {
    schema::generate(&SchemaInput {
        factories_json: FACTORIES,
        datamaps_json: DATAMAPS,
        maps: &[],
        scripts: &[],
    })
    .expect("the engine tables parse")
}

fn factory_classnames() -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(FACTORIES).expect("factories json");
    value["factories"]
        .as_array()
        .expect("factories is an array")
        .iter()
        .map(|f| f["classname"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn schema_has_all_factories() {
    let schema = engine_schema();
    let wanted = factory_classnames();
    assert_eq!(wanted.len(), 268, "the server factory table has 268 classnames");
    let missing: Vec<&String> = wanted.iter().filter(|c| schema.class(c).is_none()).collect();
    assert!(missing.is_empty(), "{missing:?} are registered but missing from the schema");
    for classname in &wanted {
        let def = schema.class(classname).expect("class is present");
        assert_eq!(&def.classname, classname);
        assert!(def.sources.engine_factory, "{classname} must record its factory");
        assert!(!def.cpp_class.is_empty(), "{classname} must record its C++ class");
    }
    // Every class is usable as a form: it has a kind and the schema loads back.
    let text = schema.to_json();
    assert_eq!(schema::EntitySchema::load(&text).unwrap(), schema);
    assert!(schema.len() >= wanted.len());
}

#[test]
fn schema_trigger_multiple_keys() {
    let schema = engine_schema();
    let def = schema.class("trigger_multiple").expect("trigger_multiple is a registered class");
    assert_eq!(def.cpp_class, "CTriggerMultiple");
    assert_eq!(def.base_chain[0], "CTriggerMultiple", "the chain starts at the class itself");
    assert!(def.base_chain.contains(&"CBaseEntity".to_string()), "keys inherit down to the root");
    assert!(def.keys.contains_key("spawnflags"), "spawnflags comes from CBaseEntity");
    assert_eq!(def.keys["spawnflags"].ty, KeyType::Flags);
    assert!(def.outputs.contains(&"OnTrigger".to_string()));
    assert!(def.inputs.contains(&"Trigger".to_string()));
    assert!(def.sources.engine_datamap);
    assert_eq!(def.sources.maps, 0, "no map evidence in this schema");

    // The datamap field type drives the key type until a map shows a value.
    let info = schema.class("info_target").expect("info_target is a registered class");
    assert_eq!(info.kind, ClassKind::Point);
    assert_eq!(info.base_chain, vec!["CBaseEntity".to_string()], "CPointEntity ships no datamap");
    assert_eq!(info.keys["rendercolor"].ty, KeyType::Color);
    assert_eq!(info.keys["health"].ty, KeyType::Int);
    assert_eq!(info.keys["friction"].ty, KeyType::Float);
    assert_eq!(info.keys["velocity"].ty, KeyType::Vec3);
    assert_eq!(info.keys["CollideWithOwner"].ty, KeyType::Bool);
    assert_eq!(info.keys["model"].ty, KeyType::String, "the datamap calls the model a string");
    assert_eq!(info.keys["modelindex"].ty, KeyType::Int);
    assert_eq!(info.keys["target"].ty, KeyType::String);
    assert!(info.keys.contains_key("parentname"));
    // A field the engine marks as both key and input is a key, not a bare
    // input handler.
    assert_eq!(info.keys["damageNotifications"].ty, KeyType::Bool);
    assert!(!info.inputs.contains(&"damageNotifications".to_string()));
    assert!(info.outputs.contains(&"OnDeath".to_string()));
    assert!(info.outputs.contains(&"OnUser1".to_string()));

    let prop = schema.class("prop_dynamic").expect("prop_dynamic is a registered class");
    assert!(prop.keys.contains_key("model"), "prop_dynamic has a model key");
    assert_eq!(prop.base_chain[0], "CDynamicProp");
    assert!(prop.base_chain.contains(&"CBreakableProp".to_string()));
}

#[test]
fn schema_falls_back_to_the_root_for_classes_with_no_datamap() {
    let schema = engine_schema();
    let def = schema.class("trigger_once").expect("trigger_once is a registered class");
    assert_eq!(def.base_chain[0], "CTriggerMultiple", "the trigger family shares one datamap");
    assert!(def.keys.contains_key("spawnflags"));
    let point = schema.class("info_target").expect("info_target is a registered class");
    assert_eq!(point.base_chain, vec!["CBaseEntity".to_string()], "CPointEntity ships no datamap");
    assert!(!point.sources.engine_datamap, "the class itself has no datamap");
}

/// A shipped partition feeds the observation half of the generator.
#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn schema_from_shipped_maps() {
    let root = std::env::var("R5ED_FIXTURES").expect("R5ED_FIXTURES must point at the fixture tree");
    let dir = std::path::Path::new(&root).join("dedi").join("mp_rr_angel_city").join("maps");
    let mut maps: Vec<MapSource> = Vec::new();
    for partition in rdoc::model::Partition::FILES {
        let name = partition.sidecar("mp_rr_angel_city").expect("sidecar name");
        let path = dir.join(&name);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        maps.push(MapSource {
            path: name,
            partition,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    let lump = dir.join("mp_rr_angel_city.bsp.0000.bsp_lump");
    if let Ok(bytes) = std::fs::read(&lump) {
        maps.push(MapSource {
            path: "mp_rr_angel_city.bsp.0000.bsp_lump".into(),
            partition: rdoc::model::Partition::Main,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    assert!(!maps.is_empty(), "the fixture map has entity partitions");
    let schema = schema::generate(&SchemaInput {
        factories_json: FACTORIES,
        datamaps_json: DATAMAPS,
        maps: &maps,
        scripts: &[],
    })
    .expect("generation succeeds");

    // Light spots dominate the environment partition of this map.
    let spot = schema.class("light_spot").expect("light_spot is registered");
    assert!(spot.sources.maps > 500, "light_spot is one of the most common classes in the map");
    assert_eq!(schema.home_partition("light_spot"), Some(rdoc::model::Partition::Env));
    assert!(spot.keys.contains_key("_light"));
    assert!(spot.keys.contains_key("targetname"));

    // A brush entity in the map set is a brush kind, a light is a point kind.
    let soundscape = schema.class("trigger_soundscape").expect("registered");
    assert_eq!(soundscape.kind, ClassKind::Brush);
    assert!(soundscape.keys.contains_key("spawnflags"));
    assert!(soundscape.keys.contains_key("targetname"), "maps carry the keys the datamap lacks");
    // Every value of targetname names another entity, which the type records.
    assert_eq!(spot.keys["targetname"].ty, KeyType::Target);
    assert_eq!(spot.keys["origin"].ty, KeyType::Vec3);
    assert_eq!(spot.keys["angles"].ty, KeyType::Angles);
    assert!(spot.keys["_light"].required, "every light in the map has a colour");
    assert!(
        spot.keys["_light"].observed_values.iter().all(|v| v.split_whitespace().count() == 4),
        "a light colour is four components"
    );
    assert!(spot.keys["_light"].observed_values.len() <= 16, "observed values are capped");

    // Every key type the UI offers round trips through the serialiser.
    for ty in KeyType::ALL {
        assert!(ty.accepts("0") || ty == KeyType::Vec3 || ty == KeyType::Angles || ty == KeyType::Color);
    }
    assert_eq!(schema::EntitySchema::load(&schema.to_json()).unwrap(), schema);
}
