//! The container round trip: header plus every sidecar, and header plus body
//! for a packed file, reproduced byte for byte.

mod common;

use std::collections::BTreeMap;
use std::fs;

use common::{all_cases, assert_lump_survived, client_case, dedi_maps, scratch, tf2_case};
use rbsp::{Bsp, Layout, HEADER_SIZE};

fn dir_files(dir: &std::path::Path, stem: &str) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let prefix = format!("{stem}.");
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) {
                if let Ok(data) = fs::read(e.path()) {
                    out.insert(name, data);
                }
            }
        }
    }
    out
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn roundtrip_container_all_maps() {
    let cases = all_cases();
    assert!(cases.len() >= 25, "expected the dedicated set, a client map and a v37 source; found {}", cases.len());
    let mut sidecar_maps = 0usize;
    let mut packed_maps = 0usize;
    for case in cases.iter() {
        if case.bsp.layout == Layout::Monolithic {
            // A packed map has to come back byte for byte, offsets and padding
            // included, and the packing has to be idempotent.
            let original = fs::read(&case.path).expect("source bytes");
            let packed = case.bsp.to_monolithic();
            assert_eq!(packed, original, "{}: packing did not reproduce the file", case.name);
            let back = Bsp::from_monolithic(&packed).expect("parse the packed bytes");
            assert_eq!(back.to_monolithic(), packed, "{}: packing is not idempotent", case.name);
            assert_eq!(back.lumps, case.bsp.lumps, "{}: a lump changed", case.name);
            packed_maps += 1;
            continue;
        }
        let name = case.path.file_stem().expect("stem").to_string_lossy().to_string();
        let dir = scratch(&format!("ct-{}", name));
        let path = dir.join(format!("{name}.bsp"));
        case.bsp.write_path(&path).expect("write");
        let before = dir_files(&dir, &name);
        assert_eq!(
            before.get(&format!("{name}.bsp")).map(|v| v.len()),
            Some(HEADER_SIZE),
            "{}: a sidecar map wrote a body into the header file",
            case.name
        );

        let reread = Bsp::read_path(&path).expect("reread");
        reread.write_path(&path).expect("rewrite");
        assert_eq!(dir_files(&dir, &name), before, "{}: the second write changed the files", case.name);
        assert_eq!(case.bsp.header, reread.header, "{}: the header table changed", case.name);
        assert_eq!(case.bsp.lumps, reread.lumps, "{}: a lump changed", case.name);
        assert_eq!(case.bsp.layout, reread.layout, "{}: the layout changed", case.name);
        for (id, name) in &reread.sidecar_names {
            assert!(Bsp::sidecar_name_is_lowercase(name), "{}: lump {id:#04x} wrote {name}", case.name);
        }
        // Packing what a sidecar map holds has to produce a readable file.
        let packed = reread.to_monolithic();
        let from_packed = Bsp::from_monolithic(&packed)
            .unwrap_or_else(|e| panic!("{}: the packed form does not parse: {e}", case.name));
        for (id, data) in &reread.lumps {
            assert_lump_survived(
                &case.name,
                *id,
                data,
                from_packed.lump(*id),
                reread.header.lumps[*id as usize].offset,
                from_packed.header.lumps[*id as usize].offset,
            );
        }
        sidecar_maps += 1;
        fs::remove_dir_all(&dir).ok();
    }
    assert_eq!(sidecar_maps + packed_maps, cases.len());
    assert!(packed_maps >= 1, "no fixture exercised the packed layout");
    assert!(dedi_maps().len() >= 23, "the dedicated fixture set shrank to {}", dedi_maps().len());
    assert!(client_case().is_some(), "the client fixture is missing");
    assert!(tf2_case().is_some(), "the v37 fixture is missing (set R5ED_TF2_BSP)");
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn packed_map_preserves_offsets_and_padding() {
    let Some(case) = client_case() else { return };
    // Packing a sidecar map lays the lumps out afresh; packing that result again
    // has to be byte-identical, which is what makes the layout stable.
    let first = case.bsp.to_monolithic();
    let back = Bsp::from_monolithic(&first).expect("parse the packed bytes");
    assert_eq!(back.to_monolithic(), first, "packing a packed map is not byte-identical");
    for (id, data) in &case.bsp.lumps {
        assert_lump_survived(
            &case.name,
            *id,
            data,
            back.lump(*id),
            case.bsp.header.lumps[*id as usize].offset,
            back.header.lumps[*id as usize].offset,
        );
    }
    let header = rbsp::Header::parse(&first).expect("header");
    for (id, data) in &back.lumps {
        let e = header.lumps[*id as usize];
        assert!(e.offset as usize >= HEADER_SIZE, "lump {id:#04x} sits inside the header");
        assert!(e.offset as usize + data.len() <= first.len(), "lump {id:#04x} runs past the file");
        // Packing writes the stored size, so a lump that declared a padded
        // length on disk comes back declaring what it holds.
        assert_eq!(e.length as usize, data.len(), "lump {id:#04x} declares a length it does not have");
    }

    // Replacing a lump must leave every other offset alone.
    let mut edited = back.clone();
    edited.set_lump(0x7B, vec![7u8; 36]);
    let after = edited.to_monolithic();
    let re = Bsp::from_monolithic(&after).expect("parse the edited bytes");
    assert_eq!(re.lump(0x7B), &[7u8; 36]);
    for (id, data) in &back.lumps {
        if *id == 0x7B {
            continue;
        }
        assert_lump_survived(
            &case.name,
            *id,
            data,
            re.lump(*id),
            back.header.lumps[*id as usize].offset,
            re.header.lumps[*id as usize].offset,
        );
        assert_eq!(
            re.header.lumps[*id as usize].offset,
            header.lumps[*id as usize].offset,
            "lump {id:#04x} offset moved"
        );
    }
    assert!(after.len() >= first.len(), "the edited file shrank");
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn read_dir_map_returns_every_partition() {
    for (name, dir) in dedi_maps() {
        let (bsp, ents) = Bsp::read_dir_map(&dir, &name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!bsp.lumps.is_empty(), "{name}: read no lumps");
        let version = bsp.version_kind().expect("version");
        let list = rbsp::lumps::level::parse_entity_partitions(bsp.lump(0x18), version).expect("0x18");
        for part in &list.names {
            assert!(ents.contains_key(part), "{name}: partition {part} was not read");
        }
        assert_eq!(ents.len(), list.names.len(), "{name}: read {} of {} partitions", ents.len(), list.names.len());
        for (part, data) in &ents {
            let parsed = rbsp::lumps::entities::parse_id(0x00, data, version)
                .unwrap_or_else(|e| panic!("{name}: partition {part} does not parse: {e}"));
            assert_eq!(parsed.entities.len(), rbsp::lumps::entities::count_blocks(data));
        }
    }
}
