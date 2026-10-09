//! collision blobs and surface masks against the shipped maps.

mod common;

use common::*;

#[test]
fn volume_surface_masks_are_the_fixture_ones() {
    assert_eq!(entities::volume_surface("trigger_hurt"), (0x00EB_1280, 0x400, "TOOLS\\TOOLSTRIGGER".to_string()));
    assert_eq!(
        entities::volume_surface("envmap_volume"),
        (0x10E3_1240, 0x610, "TOOLS\\TOOLSENVMAPVOLUME".to_string())
    );
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn fixture_coll_blobs_carry_the_documented_masks() {
    // Every coll blob is paired with the classname written above it, so each
    // one is read by walking back from its first chunk rather than by cutting
    // the partition into blocks.
    let dir = fixtures().join("dedi");
    let mut seen_trigger = 0usize;
    let mut seen_envmap = 0usize;
    let mut seen_total = 0usize;
    for map in std::fs::read_dir(&dir).expect("the fixture root reads") {
        let map = map.expect("a fixture entry reads");
        let maps = map.path().join("maps");
        if !maps.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&maps).expect("a map directory reads") {
            let f = f.expect("a map file reads");
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("ent") {
                continue;
            }
            let text = std::fs::read(&p).expect("a partition file reads");
            let mut at = 0usize;
            while let Some(rel) = text[at..].windows(8).position(|w| w == b"\"*coll0\"") {
                let start = at + rel;
                let body = &text[..start];
                let cn = classname_before(body);
                let (blob, next) = coll_blob_at(&text, start);
                let model = rcomp::collision::parse_coll_blob(&blob, rcomp::collision::CollTarget::Dedi)
                    .expect("the blob parses");
                seen_total += 1;
                match cn.as_str() {
                    "envmap_volume" => {
                        assert_eq!(model.contents, vec![0x10E3_1240], "envmap volume contents");
                        seen_envmap += 1;
                    }
                    c if c.starts_with("trigger_") => {
                        assert_eq!(model.contents, vec![0x00EB_1280], "trigger contents");
                        seen_trigger += 1;
                    }
                    _ => {}
                }
                at = next;
            }
        }
    }
    assert!(seen_total > 0, "the fixtures hold no coll blobs at all");
    assert!(seen_trigger > 100, "the fixtures hold {} trigger blobs", seen_trigger);
    assert_eq!(seen_envmap, 0, "the dedicated fixtures hold no envmap volume");
}

