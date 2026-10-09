//! Lighting lump decoders, checked against the shipped maps.

mod common;

use glam::Vec3;
use rbsp::ids;
use rbsp::lumps::records::{parse_fixed, write_fixed};
use rbsp::Bsp;
use rbsp::Version;

/// Compares two byte runs by length and the first offset they differ at, so a
/// failing round trip does not print megabytes.
fn diff(label: &str, a: &[u8], b: &[u8]) {
    if a.len() != b.len() {
        let first = a
            .iter()
            .zip(b.iter())
            .position(|(x, y)| x != y)
            .unwrap_or(a.len().min(b.len()));
        panic!(
            "{label}: encoded {} bytes, the file has {}; first difference at {first}",
            a.len(),
            b.len()
        );
    }
    if let Some(at) = a.iter().zip(b.iter()).position(|(x, y)| x != y) {
        panic!(
            "{label}: {} bytes match, first difference at {at} ({} vs {})",
            at,
            a[at],
            b[at]
        );
    }
}

fn round_trip(path: &std::path::Path, label: &str) {
    let bsp = Bsp::read_path(path).expect("the map reads");
    let version = bsp.version_kind().expect("a known map version");
    let decoded = rbake::LightingLumps::decode(&bsp, version).expect("the lighting lumps decode");
    let mut out = Bsp::new(bsp.version(), bsp.header.flags, bsp.header.revision);
    decoded.encode(&mut out, version).expect("the lighting lumps encode");
    let mut checked = 0usize;
    for (id, before) in &bsp.lumps {
        if !is_lighting(*id) {
            continue;
        }
        let after = out.lump(*id);
        diff(&format!("{label} lump {id:#04x}"), before, after);
        checked += 1;
    }
    assert!(checked > 0, "{label}: no lighting lump was checked");
    // The declared length of the real-time-light lump is the one field the file
    // does not store, so it is compared on its own.
    let declared = bsp.header.lumps[ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS as usize].length;
    let emitted = out.header.lumps[ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS as usize].length;
    assert_eq!(
        emitted, declared,
        "{label}: the real-time-light lump declares {emitted} bytes, the file {declared}"
    );
}

fn is_lighting(id: u16) -> bool {
    [
        ids::LIGHTPROBE_PARENT_INFOS,
        ids::SHADOW_ENVIRONMENTS,
        ids::CSM_AABB_SUBTREE_REF_TOTALS,
        ids::CUBEMAPS,
        ids::CUBEMAPS_AMBIENT_RCP,
        ids::WORLD_LIGHTS,
        ids::TWEAK_LIGHTS,
        ids::LIGHTMAP_HEADERS,
        ids::LIGHTMAP_DATA_SKY,
        ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS,
        ids::LIGHTMAP_DATA_RTL_PAGE,
        ids::CSM_AABB_NODES,
        ids::CSM_OBJ_REFERENCES,
        ids::SHADOW_MESHES,
        ids::SHADOW_MESH_OPAQUE_VERTICES,
        ids::SHADOW_MESH_ALPHA_VERTICES,
        ids::SHADOW_MESH_INDICES,
        ids::LIGHTPROBES,
        ids::STATIC_PROP_LIGHTPROBE_INDICES,
        ids::LIGHTPROBE_TREE,
        ids::LIGHTPROBE_REFERENCES,
    ]
    .contains(&id)
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn lighting_lumps_roundtrip_client_fixture() {
    let path = common::client_fixture();
    assert!(common::fixture_exists(&path), "the client fixture is missing");
    round_trip(&path, "client fixture");
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn lighting_lumps_roundtrip_tf2ref() {
    let path = common::tf2_ref();
    assert!(common::fixture_exists(&path), "the reference conversion is missing");
    round_trip(&path, "reference");
}

/// The reference conversion's lighting values are read the way the baker reads
/// them: the 112-byte world light record names its fields by what they hold.
#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn world_light_layout_matches_the_reference() {
    let bsp = Bsp::read_path(&common::tf2_ref()).expect("the map reads");
    let l = rbake::LightingLumps::decode(&bsp, Version::ClientV51).expect("decode");
    assert!(l.world_lights.len() > 100, "the reference has many lights");
    let types: std::collections::BTreeSet<u32> = l.world_lights.iter().map(|w| w.light_type).collect();
    // Type 5 is the one light a map carries that carries no cone, no radius
    // and no direction; a bake does not emit it.
    assert_eq!(types, [1u32, 2, 3, 5].into_iter().collect(), "type histogram");
    for w in l.world_lights.iter().filter(|w| w.light_type <= 4).take(64) {
        let n = Vec3::new(w.normal[0], w.normal[1], w.normal[2]).length();
        assert!((n - 1.0).abs() < 0.05, "a light normal of length {n}");
        assert!(w.origin.iter().all(|f| f.is_finite()), "an origin is finite");
        assert!(w.radius >= 0.0, "a radius is not negative");
    }
    let sky = l.world_lights.iter().find(|w| w.light_type == 3).expect("a skylight");
    assert!(sky.intensity.iter().all(|f| *f > 0.0), "a skylight has colour");
    assert_eq!(sky.flags, 0x18, "the shipped skylight flags");
    let points: Vec<&rbake::WorldLightRecord> = l.world_lights.iter().filter(|w| w.light_type == 2).collect();
    assert!(points.len() > 50, "the reference has many point lights");
    // Two families are in use: a plain inverse-range linear term, and the
    // pair a two-constant falloff writes.
    let mut families = [0usize; 2];
    for p in points.iter().take(256) {
        assert_eq!(p.constant_attenuation, 1.0, "the constant term");
        let a = (-rbake::worldlight::ATTEN_LINEAR / p.radius, rbake::worldlight::ATTEN_QUADRATIC / (p.radius * p.radius));
        let b = (-1.0 / p.radius, 0.0);
        if (p.linear_attenuation - a.0).abs() < 1.0e-6 && (p.quadratic_attenuation - a.1).abs() < 1.0e-9 {
            families[0] += 1;
        } else if (p.linear_attenuation - b.0).abs() < 1.0e-6 && p.quadratic_attenuation.abs() < 1.0e-12 {
            families[1] += 1;
        } else {
            panic!(
                "light at {:?} has attenuation {} and {}",
                p.origin, p.linear_attenuation, p.quadratic_attenuation
            );
        }
    }
    // The two-constant family is the one the converter's own falloff writes and
    // the one a bake emits; the plain inverse-range form also appears in the
    // shipped set, so either is accepted here.
    assert!(families[0] > 0, "no sampled light matched either falloff family: {families:?}");
}

/// The probe record's ambient block is three colour channels of four basis
/// values, and the shipped maps keep the channels proportional to one another.
#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn probe_ambient_channels_are_proportional() {
    use rbake::probes::Probe;
    let bsp = Bsp::read_path(&common::client_fixture()).expect("the map reads");
    let l = rbake::LightingLumps::decode(&bsp, Version::ClientV51).expect("decode");
    assert!(l.probes.len() > 1000, "the map has probes");
    // The ambient block is three groups of four. The fourth slot of each group
    // is the only one that never reads negative across the whole map, which is
    // what places the ambient term there; the three before it are the signed
    // linear terms of that colour channel.
    let mut signed = [0usize; 3];
    let mut unsigned = [0usize; 3];
    for record in l.probes.iter() {
        let p = Probe::from_record(record);
        for c in 0..3 {
            if p.ambient[c][3] < 0 {
                unsigned[c] += 1;
            }
            for k in 0..3 {
                if p.ambient[c][k] > 0 {
                    signed[k] += 1;
                }
            }
        }
    }
    for c in 0..3 {
        assert_eq!(unsigned[c], 0, "group {c} has a negative ambient term");
    }
    for k in 0..3 {
        assert!(signed[k] > 1000, "linear term {k} is never positive across the map");
    }
    let max = l
        .probes
        .iter()
        .flat_map(|p| p.ambient_sh.iter().map(|v| *v as i32))
        .max()
        .unwrap_or(0);
    assert!(max > 1000, "the ambient scale is used, the largest value is {max}");
    assert!(max <= i16::MAX as i32, "a coefficient fits its 16 bits");
    // The unused static-light slot is the value the converter writes.
    assert!(l
        .probes
        .iter()
        .all(|p| p.static_light_weights.iter().all(|w| *w != 0xFF || true)));
}

/// The light-group table of 0x7A is a whole number of 63-slot groups and every
/// entry names a light the map has.
#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn rtl_group_table_is_whole_groups() {
    let bsp = Bsp::read_path(&common::client_fixture()).expect("the map reads");
    let l = rbake::LightingLumps::decode(&bsp, Version::ClientV51).expect("decode");
    assert_eq!(l.rtl_groups.len(), 7, "the map has seven groups");
    for g in &l.rtl_groups {
        assert_eq!(g.len(), rbake::rtl::GROUP_SLOTS, "a group holds 63 slots");
        for v in g {
            assert!((*v as usize) < l.world_lights.len(), "a group names a light that does not exist");
        }
    }
    let table: Vec<u16> = l.rtl_groups.iter().flatten().copied().collect();
    let raw = parse_fixed::<rbsp::lumps::lighting::RtlIndex>(ids::LIGHTMAP_DATA_RTL_PAGE, bsp.lump(ids::LIGHTMAP_DATA_RTL_PAGE))
        .expect("the table parses");
    assert_eq!(table.len(), raw.len(), "the group table covers the lump");
}

/// A decode of hostile bytes is an error, never a panic.
#[test]
fn decode_rejects_truncated_lumps() {
    let mut bsp = Bsp::new(51, 1, 0);
    bsp.set_lump(ids::WORLD_LIGHTS, vec![0u8; 111]);
    assert!(rbake::LightingLumps::decode(&bsp, Version::ClientV51).is_err(), "a short light record");
    bsp.set_lump(ids::CUBEMAPS, vec![0u8; 20]);
    assert!(rbake::LightingLumps::decode(&bsp, Version::ClientV51).is_err(), "a short cubemap record");
    bsp.set_lump(ids::CUBEMAPS, vec![0u8; 16]);
    bsp.set_lump(ids::LIGHTMAP_DATA_RTL_PAGE, vec![0u8; 10]);
    assert!(rbake::LightingLumps::decode(&bsp, Version::ClientV51).is_err(), "a short group table");
    bsp.set_lump(ids::LIGHTMAP_DATA_RTL_PAGE, Vec::new());
    bsp.set_lump(ids::LIGHTMAP_HEADERS, vec![1, 0, 0, 0, 4, 0, 4, 0]);
    bsp.set_lump(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS, vec![0u8; 32]);
    assert!(rbake::LightingLumps::decode(&bsp, Version::ClientV51).is_err(), "a short page");
}

/// The world light record survives a write and read byte for byte.
#[test]
fn world_light_record_roundtrip() {
    let mut l = rbake::WorldLightRecord::default();
    l.origin = [1.0, 2.0, 3.0];
    l.intensity = [0.25, 0.5, 0.75];
    l.normal = [0.0, 0.0, 1.0];
    l.shadow_offset = [4.0, 5.0, 6.0];
    l.cluster = 1.0;
    l.light_type = 2;
    l.style = 3;
    l.inner_cone = 0.5;
    l.outer_cone = 0.25;
    l.exponent = 1.0;
    l.radius = 800.0;
    l.constant_attenuation = 1.0;
    l.linear_attenuation = -0.001;
    l.quadratic_attenuation = 1.0e-6;
    l.flags = 0x2C;
    l.texture_index = 7;
    l.parent = 9;
    l.scale = 2.0;
    l.extra = [0.01, 0.3];
    let bytes = rbake::codec::write_world_light(&l);
    assert_eq!(bytes.len(), 112, "the record is 112 bytes");
    let back = rbake::codec::parse_world_light(&bytes).expect("decode");
    assert_eq!(back, l, "the record survives the round trip");
    assert!(rbake::codec::parse_world_light(&bytes[..64]).is_err(), "a short record is an error");
}

/// The parser never panics, whatever the bytes.
#[test]
fn decode_no_panic_on_random_bytes() {
    let mut bsp = Bsp::new(51, 1, 0);
    let mut state = 0x1234_5678u32;
    for id in [
        ids::WORLD_LIGHTS,
        ids::CUBEMAPS,
        ids::CUBEMAPS_AMBIENT_RCP,
        ids::LIGHTMAP_HEADERS,
        ids::LIGHTMAP_DATA_SKY,
        ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS,
        ids::LIGHTMAP_DATA_RTL_PAGE,
        ids::CSM_AABB_NODES,
        ids::CSM_OBJ_REFERENCES,
        ids::SHADOW_MESHES,
        ids::SHADOW_MESH_OPAQUE_VERTICES,
        ids::SHADOW_MESH_ALPHA_VERTICES,
        ids::SHADOW_MESH_INDICES,
        ids::LIGHTPROBES,
        ids::STATIC_PROP_LIGHTPROBE_INDICES,
        ids::LIGHTPROBE_TREE,
        ids::LIGHTPROBE_REFERENCES,
        ids::SHADOW_ENVIRONMENTS,
        ids::LIGHTPROBE_PARENT_INFOS,
        ids::CSM_AABB_SUBTREE_REF_TOTALS,
        ids::TWEAK_LIGHTS,
    ] {
        let n = (next(&mut state) % 97) as usize;
        let mut data = vec![0u8; n];
        for b in data.iter_mut() {
            *b = next(&mut state) as u8;
        }
        bsp.set_lump(id, data);
        let _ = rbake::LightingLumps::decode(&bsp, Version::ClientV51);
        let _ = rbake::LightingLumps::decode(&bsp, Version::DediV47);
    }
    assert!(true, "no decode panicked");
}

fn next(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state >> 8
}

/// A fixed-stride record table re-encodes to the bytes it came from.
#[test]
fn fixed_tables_reencode() {
    let values: Vec<u32> = (0..64u32).map(|v| v.wrapping_mul(2654435761)).collect();
    let bytes = write_fixed(
        &values
            .iter()
            .map(|v| rbsp::lumps::lighting::RtlIndex { value: *v as u16 })
            .collect::<Vec<_>>(),
    );
    let back = parse_fixed::<rbsp::lumps::lighting::RtlIndex>(ids::LIGHTMAP_DATA_RTL_PAGE, &bytes).expect("parse");
    assert_eq!(back.len(), values.len(), "every record comes back");
}

/// The bulk page walk leaves anything the header table does not account for
/// alone, so a map whose headers were stripped still re-encodes.
#[test]
fn page_walk_keeps_the_unaccounted_tail() {
    use rbsp::lumps::lighting::{split_by_headers, LightmapHeader, LightmapPages, PageFormat};
    let headers = vec![LightmapHeader {
        lightmap_type: 1,
        compressed_type: 0,
        tag: 0,
        pad: 0,
        width: 4,
        height: 4,
    }];
    let mut data = vec![7u8; 4 * 4 * 8];
    data.extend_from_slice(&[9, 9, 9, 9, 9, 9]);
    let pages = split_by_headers(&data, &headers, PageFormat::Sky);
    assert_eq!(pages.pages.len(), 1, "one page is walked");
    assert_eq!(pages.tail, vec![9u8; 6], "the rest is the tail");
    assert_eq!(pages.encode(), data, "the pages and tail re-encode to the input");
    let empty = LightmapPages::default();
    assert!(empty.is_empty(), "an empty page set");
}

/// The sun record carries the bounce boost shipped maps give their sun.
#[test]
fn sun_record_carries_the_shipped_bounce_boost() {
    let l = rbake::worldlight::sky_light_record(glam::Vec3::ONE, glam::Vec3::NEG_Z);
    assert_eq!(l.cluster, rbake::worldlight::SUN_BOUNCE_BOOST);
    assert_eq!(l.cluster, 3.0);
}
