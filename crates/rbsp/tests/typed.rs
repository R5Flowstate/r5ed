//! Every lump of every fixture map parses and re-encodes to the same bytes.

mod common;

use rbsp::lumps::{self, Lump};
use rbsp::Bsp;
use common::{all_cases, assert_lump_survived, client_case, tf2_case};

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn roundtrip_typed_all_lumps() {
    let cases = all_cases();
    assert!(cases.len() >= 25, "expected 25 or more fixture maps, found {}", cases.len());
    let mut lump_count = 0usize;
    let mut opaque: Vec<(String, u16)> = Vec::new();
    for case in cases.iter() {
        for (id, data) in &case.bsp.lumps {
            let base = case.bsp.header.lumps[*id as usize].offset;
            let parsed = lumps::parse_at(*id, case.version, data, base)
                .unwrap_or_else(|e| panic!("{}: lump {id:#04x} does not parse: {e}", case.name));
            let written = lumps::write(&parsed, case.version);
            assert_eq!(
                written.len(),
                data.len(),
                "{}: lump {id:#04x} re-encoded to {} bytes from {}",
                case.name,
                written.len(),
                data.len()
            );
            assert_eq!(
                written.as_slice(),
                &data[..],
                "{}: lump {id:#04x} ({} records) does not round trip",
                case.name,
                parsed.kind()
            );
            if let Lump::Opaque(_) = parsed {
                opaque.push((case.name.clone(), *id));
            }
            lump_count += 1;
        }
    }
    assert!(lump_count > 700, "only {lump_count} lumps were checked");
    // Only the slots the converters never read may stay opaque.
    for (name, id) in &opaque {
        let version = if name.contains("v37") {
            rbsp::Version::Tf2V37
        } else if name.contains("v51") {
            rbsp::Version::ClientV51
        } else {
            rbsp::Version::DediV47
        };
        assert!(
            // 0x61 and 0x62 are only read from an Apex map: the v37 converter
            // copies them across without decoding them.
            matches!(id, 0x06 | 0x1D | 0x28 | 0x60 | 0x37 | 0x44 | 0x61 | 0x62),
            "{name}: lump {id:#04x} has no codec"
        );
        let _ = version;
    }
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn entity_partitions_roundtrip() {
    for case in all_cases().iter() {
        let data = case.bsp.lump(0x18);
        if data.is_empty() {
            continue;
        }
        let p = lumps::level::parse_entity_partitions(data, case.version).unwrap();
        let back = lumps::level::write_entity_partitions(&p, case.version);
        assert_eq!(back, data, "{}: the partition list does not round trip", case.name);
    }
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn record_strides_match_the_engines_loaders() {
    // Every stride the offline load gate pins down, checked against the layout
    // a real map of that version carries.
    let Some(client) = client_case() else { return };
    let v = client.version;
    let pairs: &[(u16, usize)] = &[
        (0x01, 16),
        (0x02, 16),
        (0x03, 12),
        (0x0E, 64),
        (0x10, 4),
        (0x11, 8),
        (0x12, 64),
        (0x13, 4),
        (0x1E, 12),
        (0x36, 112),
        (0x47, 20),
        (0x49, 32),
        (0x4F, 2),
        (0x50, 28),
        (0x51, 32),
        (0x52, 12),
        (0x53, 8),
        (0x63, 32),
        (0x64, 4),
        (0x65, 44),
        (0x66, 4),
        (0x67, 8),
        (0x68, 20),
        (0x6A, 8),
        (0x6B, 8),
        (0x6C, 12),
        (0x6D, 12),
        (0x6E, 4),
        (0x6F, 16),
        (0x70, 2),
        (0x71, 2),
        (0x72, 16),
        (0x73, 16),
        (0x74, 8),
        (0x75, 12),
        (0x76, 2),
        (0x77, 32),
        (0x78, 4),
        (0x79, 32),
        (0x7B, 36),
        (0x7C, 12),
        (0x7D, 20),
        (0x7E, 2),
        (0x7F, 12),
    ];
    for (id, stride) in pairs {
        let data = client.bsp.lump(*id);
        if data.is_empty() {
            continue;
        }
        assert_eq!(data.len() % stride, 0, "client lump {id:#04x} is not a whole number of {stride}-byte records");
        assert!(lumps::parse_at(*id, v, data, 0).is_ok(), "client lump {id:#04x} does not parse");
    }
    assert_eq!(client.bsp.lump(0x7A).len() % 126, 0, "the real-time light page table is not a whole number of 126-byte pages");

    if let Some(tf2) = tf2_case() {
        let v = tf2.version;
        let pairs: &[(u16, usize)] = &[
            (0x01, 16),
            (0x02, 36),
            (0x03, 12),
            (0x0E, 32),
            (0x42, 4),
            (0x45, 44),
            (0x49, 44),
            (0x4A, 28),
            (0x55, 28),
            (0x56, 4),
            (0x57, 8),
            (0x58, 16),
            (0x59, 4),
            (0x5A, 16),
            (0x5B, 4),
            (0x5C, 32),
            (0x5D, 2),
            (0x5E, 2),
            (0x5F, 16),
            (0x64, 2),
            (0x65, 48),
            (0x78, 2),
            (0x7B, 28),
        ];
        for (id, stride) in pairs {
            let data = tf2.bsp.lump(*id);
            if data.is_empty() {
                continue;
            }
            assert_eq!(data.len() % stride, 0, "v37 lump {id:#04x} is not a whole number of {stride}-byte records");
            assert!(lumps::parse_at(*id, v, data, 0).is_ok(), "v37 lump {id:#04x} does not parse");
        }
        // The two v37 texdata/name pairs have to agree with each other.
        let table = match lumps::parse_at(0x2C, v, tf2.bsp.lump(0x2C), 0).expect("0x2c") {
            Lump::U32Table(t) => t,
            other => panic!("0x2c parsed as {}", other.kind()),
        };
        let names = match lumps::parse_at(0x2B, v, tf2.bsp.lump(0x2B), 0).expect("0x2b") {
            Lump::StringTable(s) => s,
            other => panic!("0x2b parsed as {}", other.kind()),
        };
        let tex = match lumps::parse_at(0x02, v, tf2.bsp.lump(0x02), 0).expect("0x02") {
            Lump::Tf2TexData(t) => t,
            other => panic!("0x02 parsed as {}", other.kind()),
        };
        assert_eq!(table.len(), tex.len(), "one name offset per texdata record");
        for i in 0..tex.len() {
            let off = table[i] as usize;
            let name = names.at_offset(off).unwrap_or_else(|| panic!("v37 texdata {i} names byte {off}"));
            assert!(name.contains('/') || name.contains('\\'), "v37 texdata {i} resolved to {name:?}");
        }
    }
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn bvh_leaf_stream_decodes_against_the_models() {
    use rbsp::lumps::{collision, Lump};
    let Some(case) = client_case() else { return };
    let Some(Lump::Model(models)) = lumps::parse_at(0x0E, case.version, case.bsp.lump(0x0E), 0).ok() else { return };
    let Some(Lump::BvhNode(nodes)) = lumps::parse_at(0x12, case.version, case.bsp.lump(0x12), 0).ok() else { return };
    let Some(Lump::BvhLeaf(leaves)) = lumps::parse_at(0x13, case.version, case.bsp.lump(0x13), 0).ok() else { return };
    let mut polys = 0usize;
    let mut hulls = 0usize;
    let mut props = 0usize;
    let mut other = 0usize;
    for m in models.iter().filter(|m| m.has_collision()) {
        let reached = leaves.leaves_of(&nodes, m).expect("walk the tree");
        for (kind, at) in reached {
            match kind {
                collision::CHILD_POLY => {
                    let p = leaves.poly_leaf(at).expect("poly leaf");
                    assert!((p.triangle_count as usize) <= collision::MAX_POLYS_PER_LEAF);
                    polys += 1;
                }
                collision::CHILD_HULL => {
                    let h = leaves.hull_leaf(at).expect("hull leaf");
                    assert!(h.verts.len() <= 255 && h.planes.len() <= 255);
                    hulls += 1;
                }
                collision::CHILD_PROP => {
                    leaves.prop_index(at).expect("prop leaf");
                    props += 1;
                }
                k => {
                    // 2, 5, 6 and 7 are direct leaves whose payload only the
                    // engine's own handlers read; being one is not an error.
                    assert!(k <= collision::CHILD_KIND_MAX, "leaf kind {k} is not one the engine dispatches");
                    other += 1;
                }
            }
        }
    }
    assert!(polys + hulls + props + other > 0, "the collision tree decoded no leaves at all");
    assert!(polys > 0, "no poly leaf decoded from {polys} hulls, {props} props and {other} other");
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn packed_and_split_reads_agree() {
    let Some(case) = client_case() else { return };
    let packed = case.bsp.to_monolithic();
    let from_packed = Bsp::from_monolithic(&packed).expect("parse the packed bytes");
    assert_eq!(from_packed.lumps.keys().collect::<Vec<_>>(), case.bsp.lumps.keys().collect::<Vec<_>>());
    for (id, data) in &case.bsp.lumps {
        assert_lump_survived(
            &case.name,
            *id,
            data,
            from_packed.lump(*id),
            case.bsp.header.lumps[*id as usize].offset,
            from_packed.header.lumps[*id as usize].offset,
        );
    }
    for (id, data) in &case.bsp.lumps {
        // A read needs the offset the header records for the lump, because the
        // game lump's block pointers are absolute against it.
        let a = lumps::parse_at(*id, case.version, data, case.bsp.header.lumps[*id as usize].offset).expect("split");
        let b = lumps::parse_at(*id, case.version, from_packed.lump(*id), packed_header_offset(&packed, *id))
            .expect("packed");
        if *id == rbsp::ids::GAME_LUMP {
            // Its directory records where the blocks sit, so only the blocks
            // themselves have to be identical.
            assert_eq!(game_lump_shape(&a), game_lump_shape(&b), "lump {id:#04x} blocks changed across a pack");
            continue;
        }
        assert_eq!(lumps::write(&a, case.version), lumps::write(&b, case.version), "lump {id:#04x} re-encoded differently");
    }
}

/// Block count and total payload of a decoded game lump.
fn game_lump_shape(lump: &Lump) -> (usize, usize) {
    match lump {
        Lump::GameLump(g) => (
            g.blocks.len(),
            g.blocks.iter().map(rbsp::lumps::game_lump::block_len).sum(),
        ),
        other => panic!("lump 0x23 decoded as {}?", other.kind()),
    }
}

fn packed_header_offset(packed: &[u8], id: u16) -> u32 {
    rbsp::Header::parse(packed).expect("header").lumps[id as usize].offset
}
