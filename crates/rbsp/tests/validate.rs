//! The validator: clean on every shipped map, and loud on a deliberately
//! broken copy of one.

mod common;

use std::collections::BTreeMap;

use rbsp::lumps::{self, entities, Lump};
use rbsp::{Bsp, Issue, Severity};
use common::{all_cases, client_case};

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn validate_all_maps_clean() {
    let cases = all_cases();
    assert!(cases.len() >= 25, "expected 25 or more fixture maps, found {}", cases.len());
    for case in cases.iter() {
        let issues = rbsp::validate(&case.bsp, &case.ents);
        let errors: Vec<&Issue> = issues.iter().filter(|i| i.severity == Severity::Error).collect();
        assert!(
            errors.is_empty(),
            "{}: {} error-severity issues: {:?}",
            case.name,
            errors.len(),
            errors.iter().map(|e| (e.lump.map(|l| format!("{l:#04x}")), e.message.clone())).collect::<Vec<_>>()
        );
        for i in &issues {
            assert!(!i.message.contains("panicked"), "{}: {i:?}", case.name);
        }
    }
}

/// Corrupts a copy of a map in one specific way and asserts the validator
/// notices.
struct Corrupt {
    name: &'static str,
    apply: fn(&mut Bsp),
    expect_lump: Option<u16>,
    expect: &'static str,
}

const CORRUPTIONS: &[Corrupt] = &[
    Corrupt {
        name: "bvh node points at itself",
        apply: |b| {
            let nodes = b.lump(0x12).to_vec();
            let mut n = lumps::collision::parse_bvh_nodes(&nodes, rbsp::Version::ClientV51).expect("nodes");
            if n.is_empty() {
                return;
            }
            n[0].meta[0] = (0u32) | (0 << 8);
            n[0].meta[2] = collision::CHILD_NODE | (collision::CHILD_EMPTY << 4);
            b.set_lump(0x12, lumps::collision::write_bvh_nodes(&n, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x12),
        expect: "points at itself",
    },
    Corrupt {
        name: "a model starts past the node lump",
        apply: |b| {
            let models = b.lump(0x0E).to_vec();
            let mut m = lumps::collision::parse_models(&models, rbsp::Version::ClientV51).expect("models");
            if m.is_empty() {
                return;
            }
            m[0].bvh_node = 0x7FFF_FFFF;
            b.set_lump(0x0E, lumps::collision::write_models(&m, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x0E),
        expect: "bvh node",
    },
    Corrupt {
        name: "a texdata name offset is off a string start",
        apply: |b| {
            let tex = b.lump(0x02).to_vec();
            let mut t = lumps::render::parse_texdata(&tex, rbsp::Version::ClientV51).expect("texdata");
            if t.is_empty() {
                return;
            }
            t[0].name = 1;
            b.set_lump(0x02, lumps::render::write_texdata(&t, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x02),
        expect: "not a string start",
    },
    Corrupt {
        name: "a surface prop names a contents index that does not exist",
        apply: |b| {
            let sp = b.lump(0x11).to_vec();
            let mut s = lumps::collision::parse_surface_props(&sp, rbsp::Version::ClientV51).expect("surfprops");
            if s.is_empty() {
                return;
            }
            s[0].contents = 0xFE;
            b.set_lump(0x11, lumps::collision::write_surface_props(&s, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x11),
        expect: "names contents",
    },
    Corrupt {
        name: "a cell aabb node names children out of range",
        apply: |b| {
            let n = b.lump(0x77).to_vec();
            let mut a = lumps::vis::parse_aabb_nodes(&n, rbsp::Version::ClientV51).expect("aabb");
            if a.is_empty() {
                return;
            }
            a[0].children = 0xFF | (0xFFF0 << 8);
            b.set_lump(0x77, lumps::vis::write_aabb_nodes(&a, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x77),
        expect: "names children",
    },
    Corrupt {
        name: "an object reference bound runs past the reference list",
        apply: |b| {
            let n = b.lump(0x77).to_vec();
            let mut a = lumps::vis::parse_aabb_nodes(&n, rbsp::Version::ClientV51).expect("aabb");
            if a.is_empty() {
                return;
            }
            a[0].obj_refs = 0xFF | (0xFFF0 << 8);
            b.set_lump(0x77, lumps::vis::write_aabb_nodes(&a, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x78),
        expect: "object refs",
    },
    Corrupt {
        name: "a mesh points at a material sort that does not exist",
        apply: |b| {
            let m = b.lump(0x50).to_vec();
            let mut meshes = lumps::render::parse_meshes(&m, rbsp::Version::ClientV51).expect("meshes");
            if meshes.is_empty() {
                return;
            }
            meshes[0].material_sort = 0xFFFE;
            b.set_lump(0x50, lumps::render::write_meshes(&meshes, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x50),
        expect: "material sort",
    },
    Corrupt {
        name: "a material sort names texdata that does not exist",
        apply: |b| {
            let s = b.lump(0x52).to_vec();
            let mut sorts = lumps::render::parse_material_sorts(&s, rbsp::Version::ClientV51).expect("sorts");
            if sorts.is_empty() {
                return;
            }
            sorts[0].tex_data = 0xFFF0;
            b.set_lump(0x52, lumps::render::write_material_sorts(&sorts, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x52),
        expect: "texdata",
    },
    Corrupt {
        name: "a static prop names a model outside the dictionary",
        apply: |b| {
            let g = b.lump(0x23).to_vec();
            let Some(Lump::GameLump(mut gl)) = lumps::parse_at(0x23, rbsp::Version::ClientV51, &g, 0).ok() else { return };
            let Some(i) = gl.entries.iter().position(|e| e.is_sprp()) else { return };
            let Some(sprp) = gl.blocks[i].as_static_props() else { return };
            let mut props = sprp.props.clone();
            if props.is_empty() {
                return;
            }
            props[0].model_index = 0xFFF0;
            gl.blocks[i] = rbsp::lumps::game_lump::Block::StaticProps(rbsp::lumps::game_lump::sprp::StaticProps {
                models: sprp.models.clone(),
                counts: sprp.counts,
                props,
                tail_count: sprp.tail_count,
                groups: sprp.groups.clone(),
            });
            b.set_lump(0x23, lumps::write(&Lump::GameLump(gl), rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x23),
        expect: "uses model",
    },
    Corrupt {
        name: "a static prop lightprobe index is past the probe list",
        apply: |b| {
            let d = b.lump(0x66).to_vec();
            let mut p = lumps::lighting::parse_prop_lightprobe_indices(&d, rbsp::Version::ClientV51).expect("indices");
            if p.is_empty() {
                return;
            }
            p[0].value = 0x7FFF_FFF0;
            b.set_lump(0x66, lumps::lighting::write_prop_lightprobe_indices(&p, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x66),
        expect: "lightprobe",
    },
    Corrupt {
        name: "a real-time light slot names a light that does not exist",
        apply: |b| {
            let d = b.lump(0x7A).to_vec();
            let mut r = lumps::lighting::parse_rtl_index(&d, rbsp::Version::ClientV51).expect("rtl");
            if r.is_empty() {
                return;
            }
            r[0].value = 0xFFF0;
            b.set_lump(0x7A, lumps::lighting::write_rtl_index(&r, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x7A),
        expect: "names light",
    },
    Corrupt {
        name: "a csm node names refs past the end",
        apply: |b| {
            let d = b.lump(0x63).to_vec();
            let mut n = lumps::csm::parse_nodes(&d, rbsp::Version::ClientV51).expect("csm");
            if n.is_empty() {
                return;
            }
            n[0].obj_refs = 0xFF | (0xFFF0 << 8);
            b.set_lump(0x63, lumps::csm::write_nodes(&n, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x64),
        expect: "csm",
    },
    Corrupt {
        name: "the shadow mesh index lump does not cover the declared triangles",
        apply: |b| {
            let d = b.lump(0x7E).to_vec();
            if d.len() < 6 {
                return;
            }
            let mut v = d.clone();
            v.truncate(v.len() - 2);
            b.set_lump(0x7E, v);
        },
        expect_lump: Some(0x7E),
        expect: "triangle",
    },
    Corrupt {
        name: "a vertex position index is past the vertex lump",
        apply: |b| {
            let d = b.lump(0x47).to_vec();
            let mut v = lumps::render::parse_vertex_unlit(&d, rbsp::Version::ClientV51).expect("verts");
            if v.is_empty() {
                return;
            }
            v[0].position_index = 0x7FFF_FFF0;
            b.set_lump(0x47, lumps::render::write_vertex_unlit(&v, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x47),
        expect: "position",
    },
    Corrupt {
        name: "an entity model key names a model past the end",
        apply: |b| {
            let data = b.lump(0x00).to_vec();
            let Ok(mut t) = entities::parse_id(0x00, &data, rbsp::Version::ClientV51) else { return };
            if t.entities.is_empty() {
                return;
            }
            t.entities[0].pairs.push(("*999999".to_string(), String::new()));
            b.set_lump(0x00, entities::write_id(0x00, &t, rbsp::Version::ClientV51));
        },
        expect_lump: Some(0x00),
        expect: "names model",
    },
    Corrupt {
        name: "a bad stride on the planes lump",
        apply: |b| {
            let d = b.lump(0x01).to_vec();
            let mut v = d.clone();
            v.push(0);
            b.set_lump(0x01, v);
        },
        expect_lump: Some(0x01),
        expect: "not a multiple",
    },
    Corrupt {
        name: "a truncated game lump header",
        apply: |b| {
            let d = b.lump(0x23).to_vec();
            b.set_lump(0x23, d[..2].to_vec());
        },
        expect_lump: Some(0x23),
        expect: "truncated",
    },
    Corrupt {
        name: "a game lump count that does not fit the lump",
        apply: |b| {
            let mut d = b.lump(0x23).to_vec();
            if d.len() < 4 {
                return;
            }
            d[0..4].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
            b.set_lump(0x23, d);
        },
        expect_lump: Some(0x23),
        expect: "truncated",
    },
];

use rbsp::lumps::collision;

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn validate_catches_corruption() {
    let Some(case) = client_case() else {
        panic!("the client fixture is missing");
    };
    let baseline = rbsp::validate(&case.bsp, &case.ents);
    assert!(baseline.iter().all(|i| i.severity != Severity::Error), "the baseline is not clean");

    let mut caught = 0usize;
    for c in CORRUPTIONS {
        let mut broken = case.bsp.clone();
        (c.apply)(&mut broken);
        let issues = rbsp::validate(&broken, &case.ents);
        let errors: Vec<&Issue> = issues.iter().filter(|i| i.severity == Severity::Error).collect();
        if errors.is_empty() {
            let warnings: Vec<&str> = issues.iter().filter(|i| i.severity == Severity::Warning).map(|i| i.message.as_str()).collect();
            panic!("{}: not caught; warnings: {warnings:?}", c.name);
        }
        let hit = errors
            .iter()
            .any(|e| e.message.contains(c.expect) && c.expect_lump.map(|l| e.lump == Some(l)).unwrap_or(true));
        assert!(hit, "{}: caught, but not with the expected text {:?}; got {:?}", c.name, c.expect, errors.iter().map(|e| (&e.lump, &e.message)).collect::<Vec<_>>());
        caught += 1;
    }
    assert_eq!(caught, CORRUPTIONS.len());
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn validate_catches_a_missing_partition_file() {
    let case = all_cases().into_iter().find(|c| !c.ents.is_empty()).expect("a map with partitions");
    let stripped: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let issues = rbsp::validate(&case.bsp, &stripped);
    let errors: Vec<&Issue> = issues.iter().filter(|i| i.severity == Severity::Error).collect();
    assert!(!errors.is_empty(), "{}: dropping every partition file was not caught", case.name);
    assert!(errors.iter().any(|e| e.message.contains("no ") && e.message.contains(".ent")), "{:?}", errors);
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn validate_catches_an_uppercase_sidecar_name() {
    let Some(case) = client_case() else { return };
    let mut broken = case.bsp.clone();
    broken.sidecar_names.insert(0x00, "mp_rr_test.bsp.0000.BSP_LUMP".to_string());
    let issues = rbsp::validate(&broken, &case.ents);
    assert!(issues.iter().any(|i| i.message.contains("lowercase")), "{:?}", issues);
    assert!(Bsp::sidecar_name_is_lowercase("mp_rr_test.bsp.006a.bsp_lump"));
    assert!(!Bsp::sidecar_name_is_lowercase("mp_rr_test.bsp.006A.bsp_lump"));
    assert!(!Bsp::sidecar_name_is_lowercase("mp_rr_test.bsp.bsp_lump"));
}
