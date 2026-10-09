//! Per-target lump assembly: the client v51 map, the dedicated-server v47 map
//! and the pruning between them.

use rbsp::{ids, Bsp, Version};

use crate::csg::Face;
use crate::report::Stats;

/// The engine revision a fresh map declares.
pub const REVISION: u32 = 30;

/// A new map for one target, with the header the target expects.
pub fn new_map(target: Version) -> Bsp {
    Bsp::new(target.number(), target.flags(), REVISION)
}

/// Drops every lump the dedicated server does not read, and clears the header
/// entry of anything that goes.
pub fn prune_to_dedi(bsp: &mut Bsp) {
    let drop: Vec<u16> = bsp.lumps.keys().copied().filter(|id| !ids::is_dedi_lump(*id)).collect();
    for id in drop {
        bsp.remove_lump(id);
    }
}

/// The plane table the world brushes contribute, one record per face.
pub fn emit_planes(faces: &[Face], bsp: &mut Bsp, target: Version) {
    let planes: Vec<rbsp::lumps::render::Plane> = faces
        .iter()
        .map(|f| rbsp::lumps::render::Plane {
            normal: f.plane.normal.as_vec3().to_array(),
            dist: f.plane.dist as f32,
        })
        .collect();
    bsp.set_lump(ids::PLANES, rbsp::lumps::render::write_planes(&planes, target));
}

/// The partition file list, 0x18.
///
/// Every shipped map, on both targets, opens this lump with `01* ` before the
/// space-separated names, and the engine refuses anything else with "Unrecognized
/// header in Entity Partition lump in bsp".
pub fn emit_partitions(list: &[String], bsp: &mut Bsp, target: Version) {
    let blob = rbsp::lumps::level::EntityPartitions {
        ident: *b"01",
        entity_lump_flag: b'*',
        list: format!(" {}", list.join(" ")).into_bytes(),
        names: list.to_vec(),
        tail: Vec::new(),
    };
    bsp.set_lump(ids::ENTITY_PARTITIONS, rbsp::lumps::level::write_entity_partitions(&blob, target));
}

/// Fills the report counters that only the emitted map knows.
pub fn collect(bsp: &Bsp, target: Version, stats: &mut Stats) {
    stats.models = bsp.lump(ids::MODELS).len() / if target == Version::Tf2V37 { 32 } else { 64 };
    stats.render_meshes = bsp.lump(ids::MESHES).len() / 28;
    stats.bvh_nodes = bsp.lump(ids::BVH_NODES).len() / 48;
    stats.bvh_leaves = bsp.lump(ids::BVH_LEAF_DATA).len() / 4;
    stats.collision_solids = stats.bvh_leaves;
    stats.lightmap_pages = bsp.lump(ids::LIGHTMAP_HEADERS).len() / 8;
    stats.lightprobes = bsp.lump(ids::LIGHTPROBES).len() / if target == Version::ClientV51 { 44 } else { 48 };
    stats.cubemaps = bsp.lump(ids::CUBEMAPS).len() / 16;
    stats.csm_shadow_meshes = bsp.lump(ids::SHADOW_MESHES).len() / 12;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dedi_map_keeps_only_the_server_lump_set() {
        let mut bsp = new_map(Version::DediV47);
        assert_eq!(bsp.header.version, 47);
        assert_eq!(bsp.header.flags, 0);
        bsp.set_lump(ids::ENTITIES, b"{}".to_vec());
        bsp.set_lump(ids::LIGHTPROBES, vec![0; 48]);
        prune_to_dedi(&mut bsp);
        assert!(bsp.lumps.contains_key(&ids::ENTITIES));
        assert!(!bsp.lumps.contains_key(&ids::LIGHTPROBES));
        assert_eq!(bsp.header.lumps[ids::LIGHTPROBES as usize].length, 0);
    }

    #[test]
    fn the_client_map_writes_flags_one() {
        let bsp = new_map(Version::ClientV51);
        assert_eq!(bsp.header.version, 51);
        assert_eq!(bsp.header.flags, 1);
        assert_eq!(bsp.header.lump_count, 127);
    }

    #[test]
    fn the_partition_lump_opens_the_way_every_shipped_map_opens_it() {
        let mut bsp = new_map(Version::ClientV51);
        let names = vec!["env".to_string(), "script".to_string()];
        emit_partitions(&names, &mut bsp, Version::ClientV51);
        let bytes = bsp.lump(ids::ENTITY_PARTITIONS);
        assert_eq!(&bytes[..4], b"01* ", "the header the engine reads: {bytes:?}");
        assert_eq!(bytes.last(), Some(&0), "the name list is NUL terminated");
        assert_eq!(&bytes[3..], b" env script\0", "a space leads the list, as the shipped maps have it");

        // The same three bytes, read back out of the maps the engine ships.
        let Some(root) = std::env::var_os("R5ED_FIXTURES").map(std::path::PathBuf::from) else {
            return;
        };
        let mut checked = 0;
        for sub in ["dedi", "client/_test_s21_in"] {
            let Ok(rd) = std::fs::read_dir(root.join(sub)) else { continue };
            for dir in rd.flatten() {
                let maps = dir.path().join("maps");
                let Ok(files) = std::fs::read_dir(if maps.is_dir() { maps } else { dir.path() }) else {
                    continue;
                };
                for f in files.flatten() {
                    let p = f.path();
                    if p.extension().is_none_or(|e| e != "bsp") {
                        continue;
                    }
                    let Ok(shipped) = Bsp::read_path(&p) else { continue };
                    let head = shipped.lump(ids::ENTITY_PARTITIONS);
                    if head.len() < 4 {
                        continue;
                    }
                    assert_eq!(&head[..4], b"01* ", "{}: {head:?}", p.display());
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "a shipped map was read to compare against");
    }

    #[test]
    fn entity_text_ends_where_the_engine_stops_reading() {
        // What the engine does with a stored entity blob: it copies two bytes
        // per step and stops on the last byte when that byte is a NUL, so the
        // final `}` has to sit three bytes from the end with a NUL after it.
        let mut bsp = new_map(Version::ClientV51);
        bsp.set_lump(ids::ENTITIES, b"{\n\"classname\" \"worldspawn\"\n}\n\0".to_vec());
        assert!(rbsp::lumps::entities::terminator_is_intact(bsp.lump(ids::ENTITIES)));
        for short in [
            &b"{\n\"classname\" \"worldspawn\"\n}\0"[..],
            &b"{\n\"classname\" \"worldspawn\"\n}"[..],
            &b"{\n\"classname\" \"worldspawn\"\n}\n"[..],
        ] {
            assert!(
                !rbsp::lumps::entities::terminator_is_intact(short),
                "the engine runs a step past the buffer here: {short:?}"
            );
        }

        // The same predicate, read out of the maps the engine ships.
        let Some(root) = std::env::var_os("R5ED_FIXTURES").map(std::path::PathBuf::from) else {
            return;
        };
        let mut lumps = 0;
        let mut parts = 0;
        for sub in ["dedi", "client/_test_s21_in"] {
            let Ok(rd) = std::fs::read_dir(root.join(sub)) else { continue };
            for dir in rd.flatten() {
                let maps = dir.path().join("maps");
                let scan = if maps.is_dir() { maps } else { dir.path() };
                let Ok(files) = std::fs::read_dir(&scan) else { continue };
                for f in files.flatten() {
                    let p = f.path();
                    if p.extension().is_some_and(|e| e == "ent") {
                        let Ok(bytes) = std::fs::read(&p) else { continue };
                        assert!(
                            rbsp::lumps::entities::terminator_is_intact(&bytes),
                            "{}: {:?}",
                            p.display(),
                            &bytes[bytes.len().saturating_sub(4)..]
                        );
                        parts += 1;
                    } else if p.extension().is_none_or(|e| e == "bsp") {
                        let Ok(shipped) = Bsp::read_path(&p) else { continue };
                        let world = shipped.lump(ids::ENTITIES);
                        if world.is_empty() {
                            continue;
                        }
                        assert!(
                            rbsp::lumps::entities::terminator_is_intact(world),
                            "{}: {:?}",
                            p.display(),
                            &world[world.len().saturating_sub(4)..]
                        );
                        lumps += 1;
                    }
                }
            }
        }
        assert!(lumps > 0 && parts > 0, "shipped maps and partitions were read to compare against");
    }

    #[test]
    fn planes_round_trip() {
        let mut bsp = new_map(Version::ClientV51);
        let faces = vec![Face {
            object: None,
            model: 0,
            material: "world/dev/dev_gray_512".into(),
            plane: crate::csg::DPlane::new(glam::DVec3::Z, 64.0),
            points: Vec::new(),
            uv: rdoc::model::UvMapping::default(),
            lightmap_scale: 0.0,
            contents: 0,
            surface_prop: String::new(),
        }];
        emit_planes(&faces, &mut bsp, Version::ClientV51);
        let parsed = rbsp::lumps::parse(ids::PLANES, Version::ClientV51, bsp.lump(ids::PLANES)).expect("planes parse");
        match parsed {
            rbsp::lumps::Lump::Plane(p) => {
                assert_eq!(p.len(), 1);
                assert_eq!(p[0].dist, 64.0);
            }
            other => panic!("planes decoded as {}", other.kind()),
        }
    }
}
