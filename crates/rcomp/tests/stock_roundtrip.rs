//! The shipped staging_mu1 pair, decompiled and compiled again.
//!
//! Every assertion here is against the stock bytes in
//! `agent-kit/bsp/staging_mu1_full`, never against a map this compiler wrote.
//! The tests skip, saying so, when the kit is not checked out.

mod common;
mod kit;

use std::collections::BTreeSet;

use common::compile;
use kit::{decompiled, half, kit, options};
use rbsp::{ids, Bsp, Version};
use rdoc::model::{ObjectKind, RefRecord};

/// The lumps the client export writes as zeros: the compiler takes the
/// server's copy, so these differ from the export on purpose.
const ZERO_FILLED: [u16; 9] = [
    ids::LIGHTPROBE_PARENT_INFOS,
    ids::SHADOW_ENVIRONMENTS,
    ids::ENTITY_PARTITIONS,
    ids::CUBEMAPS,
    ids::LIGHTMAP_HEADERS,
    ids::TWEAK_LIGHTS,
    ids::OCCLUSION_MESH_VERTICES,
    ids::OCCLUSION_MESH_INDICES,
    ids::LEVEL_INFO,
];

/// How a recompiled lump relates to the shipped one.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Class {
    Identical,
    /// Rebuilt rather than carried; the string is why.
    Regenerated(&'static str),
    Different,
}

fn classify(id: u16, got: &Bsp, want: &Bsp, dedi: &Bsp, is_client: bool) -> Class {
    let (a, b) = (got.lump(id), want.lump(id));
    if a == b {
        return Class::Identical;
    }
    if is_client && ZERO_FILLED.contains(&id) && b.iter().all(|x| *x == 0) && a == dedi.lump(id) {
        return Class::Regenerated("the client export zero-fills it; the server half's copy is the shipped one");
    }
    Class::Different
}

#[test]
fn stock_untouched_round_trip_is_the_shipped_pair() {
    let Some(kit) = kit() else { return };
    let (client, client_ents) = half(&kit, "client");
    let (dedi, dedi_ents) = half(&kit, "dedi");
    let (doc, fs) = decompiled(&kit);
    let reference = doc
        .objects
        .iter()
        .find_map(|o| match &o.kind {
            ObjectKind::Reference(r) => Some(r),
            _ => None,
        })
        .expect("the document references the map");
    assert!(reference.removed_entities.is_empty(), "no shipped block is dropped by default");
    let records: BTreeSet<&'static str> = reference
        .sources
        .values()
        .map(|r| match r {
            RefRecord::Entity { .. } => "entity",
            RefRecord::Prop { .. } => "prop",
            RefRecord::Mesh { .. } => "mesh",
            RefRecord::Collision { .. } => "collision",
        })
        .collect();
    assert_eq!(records, BTreeSet::from(["collision", "entity", "mesh"]), "every kind of record but the props is addressed");
    assert!(fs.0.contains_key(rcomp::decompile::props::PROP_TABLE_PATH), "the props ride in their table");

    let out = compile(&doc, &fs, &common::Assets::new(), &options(&kit)).expect("the untouched document compiles");
    let mut table: Vec<String> = Vec::new();
    for (label, got, want, want_ents, is_client) in [
        ("client", out.client.as_ref().expect("a client map"), &client, &client_ents, true),
        ("dedi", out.dedi.as_ref().expect("a server map"), &dedi, &dedi_ents, false),
    ] {
        assert_eq!(got.bsp.header.version, want.header.version, "{label} header version");
        assert_eq!(got.bsp.header.flags, want.header.flags, "{label} header flags");
        let mut every: BTreeSet<u16> = want.lumps.keys().copied().collect();
        every.extend(got.bsp.lumps.keys().copied());
        let mut different = Vec::new();
        for id in every {
            let class = classify(id, &got.bsp, want, &dedi, is_client);
            let name = rbsp::ids::name(id, want.version_kind().unwrap_or(Version::ClientV51)).unwrap_or("?");
            match &class {
                Class::Identical => {}
                Class::Regenerated(why) => table.push(format!("{label} 0x{id:02x} {name}: regenerated, {why}")),
                Class::Different => different.push(format!("0x{id:02x} {name}")),
            }
        }
        assert!(different.is_empty(), "{label}: lumps that differ from the shipped map: {}", different.join(", "));
        assert_eq!(&got.ent_bytes, want_ents, "{label}: the partition files are the shipped ones");
        let issues = rbsp::validate(&got.bsp, &got.ent_bytes);
        let errors: Vec<String> = issues.iter().filter(|i| i.is_error()).map(|i| i.message.clone()).collect();
        assert!(errors.is_empty(), "{label}: the validator refuses the shipped bytes: {}", errors.join(" | "));
    }
    let built = &out.client.as_ref().unwrap().bsp;
    let tree = rbsp::lumps::lighting::parse_lightprobe_tree(built.lump(ids::LIGHTPROBE_TREE), Version::ClientV51)
        .expect("the probe tree parses");
    let refs = rbsp::lumps::lighting::parse_lightprobe_refs(built.lump(ids::LIGHTPROBE_REFERENCES), Version::ClientV51)
        .expect("the probe references parse");
    let probe = rbsp::validate::probe_tree_issues(&tree, refs.len());
    assert!(probe.is_empty(), "the shipped probe tree passes the probe predicate: {probe:?}");
    // Nine lumps and nothing else come from the other half.
    assert_eq!(table.len(), ZERO_FILLED.len(), "{}", table.join("\n"));
    eprintln!("{}", table.join("\n"));
}

/// The probe parent info the compiler writes is the one the shipped server
/// half carries for the same references and tree.
#[test]
fn the_probe_parent_info_is_the_shipped_record() {
    let Some(kit) = kit() else { return };
    let (dedi, _) = half(&kit, "dedi");
    let v = Version::DediV47;
    let refs = rbsp::lumps::lighting::parse_lightprobe_refs(dedi.lump(ids::LIGHTPROBE_REFERENCES), v).expect("refs parse");
    let tree = rbsp::lumps::lighting::parse_lightprobe_tree(dedi.lump(ids::LIGHTPROBE_TREE), v).expect("tree parses");
    let shipped = rbsp::lumps::lighting::parse_parent_infos(dedi.lump(ids::LIGHTPROBE_PARENT_INFOS), v).expect("parent info parses");
    assert_eq!(shipped, vec![rbsp::lumps::lighting::LightProbeParentInfo::over(refs.len(), tree.len())]);
}

/// The render meshes decode to the geometry the map's own bounds records
/// describe: every vertex of a mesh sits inside its record's sphere. (The
/// sphere's centre is not always the centre of the vertex box.)
#[test]
fn stock_meshes_decode_inside_their_bounds_records() {
    let Some(kit) = kit() else { return };
    let (client, _) = half(&kit, "client");
    let runs = rcomp::meshes::decode_meshes(&client).expect("the render meshes decode");
    let records = rbsp::lumps::render::parse_mesh_bounds(client.lump(ids::MESH_BOUNDS), Version::ClientV51).expect("bounds parse");
    assert_eq!(runs.len(), records.len());
    let mut fit = 0;
    for (run, r) in runs.iter().zip(&records) {
        let origin = glam::Vec3::from_array(r.origin);
        let inside = run.positions.iter().all(|p| p.distance(origin) <= r.radius + 0.5);
        if inside && !run.positions.is_empty() {
            fit += 1;
        }
    }
    assert_eq!(fit, runs.len(), "every mesh decodes inside its own bounds record ({fit} of {})", runs.len());
}

/// Blocks as sets of keys and values, with the numeric triples normalised so
/// a transform that round-trips through the document compares equal.
fn block_keys(e: &rbsp::lumps::entities::Entity) -> BTreeMap<String, String> {
    e.pairs
        .iter()
        .map(|(k, v)| {
            let norm = match k.as_str() {
                "origin" | "angles" => {
                    let n: Vec<f64> = v.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                    if n.len() == 3 {
                        n.iter().map(|x| format!("{:.2}", if x.abs() < 0.005 { 0.0 } else { *x })).collect::<Vec<_>>().join(" ")
                    } else {
                        v.clone()
                    }
                }
                _ => v.clone(),
            };
            (k.clone(), norm)
        })
        .collect()
}

use std::collections::BTreeMap;

/// A detached staging_mu1 compiles from the document alone and keeps what the
/// document carries: every entity block of every file, every static prop
/// record field for field, every prop's collision leaf, the collision no render
/// mesh stands for triangle for triangle, and every render triangle per
/// material. Lighting, probes, cubemaps, visibility and the collision of the
/// render surfaces are rebuilt, not carried.
#[test]
fn a_detached_stock_map_keeps_what_it_carries() {
    let Some(kit) = kit() else { return };
    let (client, client_ents) = half(&kit, "client");
    let (doc, fs) = decompiled(&kit);
    let (dedi, _) = half(&kit, "dedi");
    let detached = rcomp::decompile::detach(&doc, Some(rcomp::decompile::prop_source(&client, Some(&dedi)))).expect("the document detaches");
    assert!(!detached.objects.iter().any(|o| matches!(o.kind, ObjectKind::Reference(_))), "no reference is left");
    assert!(detached.objects.iter().all(|o| !o.hidden), "every record is an object now");
    let opts =
        rcomp::CompileOptions { targets: common::Targets { client: true, dedi: false }, skip_rooms: true, ..Default::default() };
    let out = compile(&detached, &fs, &common::Assets::new(), &opts).expect("the detached map compiles from scratch");
    let got = out.client.as_ref().expect("a client map");
    let issues = rbsp::validate(&got.bsp, &got.ent_bytes);
    let errors: Vec<String> = issues.iter().filter(|i| i.is_error()).map(|i| i.message.clone()).collect();
    assert!(errors.is_empty(), "the detached map validates: {}", errors.join(" | "));

    // Models: the world box is the shipped one, and every other model is empty
    // the way the shipped ones are.
    let v = Version::ClientV51;
    let models = |bsp: &Bsp| rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), v).unwrap();
    let (mw, mh) = (models(&client), models(&got.bsp));
    assert_eq!(mh.len(), mw.len(), "every model comes back");
    for a in 0..3 {
        assert!((mh[0].mins[a] - mw[0].mins[a]).abs() <= 1.0 && (mh[0].maxs[a] - mw[0].maxs[a]).abs() <= 1.0, "the world box: {:?}..{:?} against {:?}..{:?}", mh[0].mins, mh[0].maxs, mw[0].mins, mw[0].maxs);
    }
    for i in 1..mw.len() {
        let mut want = mw[i];
        want.first_mesh = mh[0].num_meshes;
        assert_eq!(mh[i], want, "model {i} is empty as it shipped");
    }

    // The floor under the lobby spawn is there to stand on.
    let spawn = glam::DVec3::new(-11740.0, 512.0, 1584.0);
    let floor = rcomp::collision::decode_leaves(&got.bsp, 0)
        .unwrap()
        .iter()
        .filter(|l| l.contents != 0)
        .flat_map(|l| l.triangles.clone())
        .filter(|t| {
            let s = |a: glam::DVec3, b: glam::DVec3| (a.x - spawn.x) * (b.y - spawn.y) - (b.x - spawn.x) * (a.y - spawn.y);
            let d = [s(t[0], t[1]), s(t[1], t[2]), s(t[2], t[0])];
            let z = t.iter().map(|p| p.z).fold(f64::MIN, f64::max);
            !(d.iter().any(|x| *x < 0.0) && d.iter().any(|x| *x > 0.0)) && z <= spawn.z && z > spawn.z - 32.0
        })
        .count();
    assert!(floor > 0, "a collision triangle lies under the spawn");

    // Entities: every file holds the same blocks, key for key.
    let files = |main: &[u8], ents: &std::collections::BTreeMap<String, Vec<u8>>| -> Vec<(String, Vec<BTreeMap<String, String>>)> {
        let mut out = vec![(
            "main".to_string(),
            rbsp::lumps::entities::parse(main, v).unwrap().entities.iter().map(block_keys).collect(),
        )];
        for p in ["env", "fx", "script", "snd", "spawn"] {
            let blocks = ents.get(p).map(|b| rbsp::lumps::entities::parse(b, v).unwrap().entities).unwrap_or_default();
            out.push((p.to_string(), blocks.iter().map(block_keys).collect()));
        }
        out
    };
    let want = files(client.lump(ids::ENTITIES), &client_ents);
    let have = files(got.bsp.lump(ids::ENTITIES), &got.ent_bytes);
    for ((name, w), (_, h)) in want.iter().zip(&have) {
        assert_eq!(h.len(), w.len(), "the {name} file keeps its block count");
        let sort = |mut x: Vec<BTreeMap<String, String>>| {
            for b in &mut x {
                // The compiler writes the world box and its own worldspawn
                // defaults over the shipped ones only where the map had none.
                if b.get("classname").map(String::as_str) == Some("worldspawn") {
                    b.retain(|k, _| k != "world_mins" && k != "world_maxs");
                }
            }
            x.sort();
            x
        };
        let (w, h) = (sort(w.clone()), sort(h.clone()));
        let missing: Vec<&BTreeMap<String, String>> = w.iter().filter(|b| !h.contains(b)).collect();
        if !missing.is_empty() {
            // Name the keys that differ against the closest block of the class.
            let mut report = Vec::new();
            for m in missing.iter().take(4) {
                let near = h
                    .iter()
                    .filter(|b| b.get("classname") == m.get("classname"))
                    .min_by_key(|b| m.iter().filter(|(k, v)| b.get(*k) != Some(v)).count() + b.keys().filter(|k| !m.contains_key(*k)).count());
                let diff: Vec<String> = match near {
                    Some(b) => m
                        .iter()
                        .filter(|(k, v)| b.get(*k) != Some(v))
                        .map(|(k, v)| format!("{k}: {v:?} -> {:?}", b.get(k)))
                        .chain(b.keys().filter(|k| !m.contains_key(*k)).map(|k| format!("{k}: added {:?}", b[k])))
                        .collect(),
                    None => vec!["no block of the class".into()],
                };
                report.push(format!("{:?}: {}", m.get("classname"), diff.join(", ")));
            }
            panic!("{name}: {} shipped blocks the detached map lost; {}", missing.len(), report.join(" | "));
        }
    }

    // Static props: every record, field for field, model by name.
    let props_of = |bsp: &Bsp| {
        let base = bsp.header.lumps[ids::GAME_LUMP as usize].offset;
        match rbsp::lumps::parse_at(ids::GAME_LUMP, v, bsp.lump(ids::GAME_LUMP), base).unwrap() {
            rbsp::lumps::Lump::GameLump(gl) => gl.static_props().unwrap().clone(),
            _ => panic!("the game lump decoded wrong"),
        }
    };
    let (sw, sh) = (props_of(&client), props_of(&got.bsp));
    assert_eq!(sh.props.len(), sw.props.len(), "every prop comes back");
    for (i, (a, b)) in sw.props.iter().zip(&sh.props).enumerate() {
        let (mut a, mut b) = (*a, *b);
        assert_eq!(sw.models[a.model_index as usize], sh.models[b.model_index as usize], "prop {i}'s model");
        a.model_index = 0;
        b.model_index = 0;
        assert_eq!(a, b, "prop {i}'s record, field for field");
    }

    // Collision: the prop leaves, per prop, and the collision-only groups.
    let prop_leaves = |bsp: &Bsp| {
        let mut out: BTreeMap<u32, (u32, u32, [i64; 6])> = BTreeMap::new();
        for l in rcomp::collision::decode_leaves(bsp, 0).unwrap() {
            if l.kind == rbsp::lumps::collision::CHILD_PROP && l.contents != 0 {
                let q = |x: f64| x.round() as i64;
                out.insert(
                    l.prop_index & 0xFFFF,
                    (l.prop_index >> 16, l.contents, [q(l.bounds_min.x), q(l.bounds_min.y), q(l.bounds_min.z), q(l.bounds_max.x), q(l.bounds_max.y), q(l.bounds_max.z)]),
                );
            }
        }
        out
    };
    let (pw, ph) = (prop_leaves(&client), prop_leaves(&got.bsp));
    assert_eq!(ph.len(), pw.len(), "every colliding prop collides");
    for (k, (fw, cw, bw)) in &pw {
        let (fh, ch, bh) = ph.get(k).unwrap_or_else(|| panic!("prop {k} lost its leaf"));
        assert_eq!((fw, cw), (fh, ch), "prop {k}'s leaf flags and mask");
        for a in 0..6 {
            assert!((bw[a] - bh[a]).abs() <= 64, "prop {k}'s leaf box: {bw:?} against {bh:?}");
        }
    }
    let record_masks: Vec<(u32, String)> = doc
        .objects
        .iter()
        .filter_map(|o| match (&o.kind, reference_record(&doc, o.id)) {
            (ObjectKind::Mesh(m), Some(RefRecord::Collision { contents, surface, .. })) if !m.render => Some((contents, surface)),
            _ => None,
        })
        .collect();
    assert!(!record_masks.is_empty(), "staging_mu1 has collision no render mesh stands for");
    let masks_of = |bsp: &Bsp| -> std::collections::BTreeSet<u32> {
        rcomp::collision::decode_leaves(bsp, 0).unwrap().iter().filter(|l| l.contents != 0).map(|l| l.contents).collect()
    };
    let have_masks = masks_of(&got.bsp);
    for (mask, surface) in &record_masks {
        assert!(have_masks.contains(mask), "the {surface} collision keeps its mask 0x{mask:x}");
    }

    // Render: every triangle, per material.
    let per_material = |bsp: &Bsp| -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for r in rcomp::meshes::decode_meshes(bsp).unwrap() {
            let real = r.indices.chunks_exact(3).filter(|t| !(t[0] == t[1] && t[1] == t[2])).count();
            *out.entry(rcomp::splice::material_key(&r.material)).or_insert(0) += real;
        }
        out
    };
    assert_eq!(per_material(&got.bsp), per_material(&client), "every render triangle comes back under its material");
}

fn reference_record(doc: &rdoc::model::MapDoc, id: rdoc::model::Id) -> Option<RefRecord> {
    doc.objects.iter().find_map(|o| match &o.kind {
        ObjectKind::Reference(r) => r.sources.get(&id).cloned(),
        _ => None,
    })
}
