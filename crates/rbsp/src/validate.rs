//! Structural checks over a decoded map: strides, cross-lump indices, the
//! header, the sidecar names and the dedicated-server lump set.

use std::collections::{BTreeMap, BTreeSet};

use crate::container::Bsp;
use crate::ids;
use crate::lumps::{csm, collision, entities, game_lump, level, lighting, render, strings, vis, Lump};
use crate::{Error, Version};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    /// The lump the issue is about, or `None` for a whole-map one.
    pub lump: Option<u16>,
    pub message: String,
}

impl Issue {
    pub fn error(lump: Option<u16>, message: impl Into<String>) -> Self {
        Self { severity: Severity::Error, lump, message: message.into() }
    }

    pub fn warning(lump: Option<u16>, message: impl Into<String>) -> Self {
        Self { severity: Severity::Warning, lump, message: message.into() }
    }

    pub fn info(lump: Option<u16>, message: impl Into<String>) -> Self {
        Self { severity: Severity::Info, lump, message: message.into() }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Everything a single lump decoded to, kept so the checks can read fields
/// without parsing twice.
struct Decoded {
    lump: u16,
    value: Lump,
}

/// Most issues one map is allowed to report. A map whose lumps are internally
/// inconsistent can trip a cross-lump rule once per record, and there are maps
/// with millions of records, so the rest are counted rather than written.
const ISSUE_CAP: usize = 400;

/// The issue list, bounded so a corrupt map cannot make the caller allocate
/// without limit.
#[derive(Default)]
struct Issues {
    list: Vec<Issue>,
    errors: usize,
    warnings: usize,
    dropped: usize,
}

impl Issues {
    /// Errors and warnings are capped separately, so a flood of warnings can
    /// never stop a later check from reporting an error.
    fn push(&mut self, issue: Issue) {
        let count = if issue.severity == Severity::Error { &mut self.errors } else { &mut self.warnings };
        if *count < ISSUE_CAP {
            *count += 1;
            self.list.push(issue);
        } else {
            self.dropped += 1;
        }
    }

    /// True once the error cap is reached, so a caller can stop the walk that
    /// is filling it.
    fn full(&self) -> bool {
        self.errors >= ISSUE_CAP
    }

    fn into_vec(mut self) -> Vec<Issue> {
        if self.dropped > 0 {
            self.list.push(Issue::warning(
                None,
                format!("{} further issues were not listed, the map is inconsistent throughout", self.dropped),
            ));
        }
        self.list
    }
}

pub fn validate(bsp: &Bsp, ents: &BTreeMap<String, Vec<u8>>) -> Vec<Issue> {
    let mut out = Issues::default();
    let Some(version) = bsp.version_kind() else {
        out.push(Issue::error(None, format!("header version {} is not one of 37, 47, 51 or 52", bsp.header.version)));
        return out.into_vec();
    };
    check_header(bsp, version, &mut out);

    let mut decoded: Vec<Decoded> = Vec::new();
    for (id, data) in &bsp.lumps {
        if *id as usize >= bsp.header.lumps.len() {
            out.push(Issue::error(Some(*id), "lump index is outside the 128-slot table"));
            continue;
        }
        let e = bsp.header.lumps[*id as usize];
        if e.length as usize != data.len() {
            out.push(Issue::warning(
                Some(*id),
                format!("header declares {} bytes, {} are stored", e.length, data.len()),
            ));
        }
        if let Some(name) = bsp.sidecar_names.get(id) {
            if !Bsp::sidecar_name_is_lowercase(name) {
                out.push(Issue::warning(
                    Some(*id),
                    format!("sidecar {name} is not lowercase hex; the engine lookup lowercases and would miss it"),
                ));
            }
        }
        match crate::lumps::parse_at(*id, version, data, e.offset) {
            Ok(l) => decoded.push(Decoded { lump: *id, value: l }),
            Err(err) => out.push(Issue::error(Some(*id), err.to_string())),
        }
    }

    let get = |id: u16| -> Option<&Lump> { decoded.iter().find(|d| d.lump == id).map(|d| &d.value) };

    // A Titanfall 2 map is a conversion source, not something the engine
    // loads; its cross-lump layout is not the Apex one the checks below read.
    if version == Version::Tf2V37 {
        return out.into_vec();
    }

    check_collision(bsp, &get, &mut out);
    check_materials(&get, &mut out);
    check_render(bsp, version, &get, &mut out);
    check_vis(bsp, &get, &mut out);
    check_csm(&get, &mut out);
    check_lighting(&get, &mut out);
    check_props(&get, &mut out);
    check_lightmap_pages(bsp, &get, &mut out);
    check_entities(bsp, ents, &get, &mut out);
    out.into_vec()
}

fn check_header(bsp: &Bsp, version: Version, out: &mut Issues) {
    if bsp.header.ident != crate::container::IDENT {
        out.push(Issue::error(None, format!("header ident is {:?}", bsp.header.ident)));
    }
    if bsp.header.lump_count != crate::container::DECLARED_LUMP_COUNT {
        out.push(Issue::error(
            None,
            format!("header declares {} lumps, every shipped map says {}", bsp.header.lump_count, crate::container::DECLARED_LUMP_COUNT),
        ));
    }
    if version == Version::ClientV51 && bsp.header.flags != 1 {
        out.push(Issue::warning(None, format!("a v51 map writes flags 1, this one has {:#06x}", bsp.header.flags)));
    }
    if version == Version::DediV47 && bsp.header.flags != 0 {
        out.push(Issue::warning(None, format!("a v47 map writes flags 0, this one has {:#06x}", bsp.header.flags)));
    }
    for (id, data) in &bsp.lumps {
        let e = bsp.header.lumps[*id as usize];
        if bsp.layout == crate::container::Layout::Monolithic && e.length > 0 {
            let end = e.offset as usize + e.length as usize;
            if (e.offset as usize) < crate::container::HEADER_SIZE || end > bsp.packed_size {
                out.push(Issue::error(Some(*id), "declared range lies outside the packed file"));
            }
        }
        if data.is_empty() {
            out.push(Issue::warning(Some(*id), "lump is present but empty"));
        }
    }
    for id in 0..crate::container::LUMP_COUNT as u16 {
        if bsp.lumps.contains_key(&id) {
            continue;
        }
        let e = bsp.header.lumps[id as usize];
        if e.length > 0 {
            let name = ids::name(id, version).unwrap_or("unused");
            out.push(Issue::warning(
                Some(id),
                format!("header declares {name} ({} bytes) but no lump file is stored", e.length),
            ));
        }
    }
}

/// Most issues one model's leaf walk may report before the walk gives up: a
/// model whose tree is wrong tends to be wrong in every leaf, and the first few
/// already name the fault.
const MODEL_ISSUE_CAP: usize = 8;

fn check_collision<'a>(bsp: &Bsp, get: &dyn Fn(u16) -> Option<&'a Lump>, out: &mut Issues) {
    let models: &[collision::Model] = match get(ids::MODELS) {
        Some(Lump::Model(m)) => m,
        _ => {
            if bsp.lump(ids::MODELS).is_empty() {
                out.push(Issue::error(Some(ids::MODELS), "the map has no models"));
            }
            return;
        }
    };
    let nodes: &[collision::BvhNode] = match get(ids::BVH_NODES) {
        Some(Lump::BvhNode(n)) => n,
        _ => return,
    };
    let leaves: &collision::BvhLeafData = match get(ids::BVH_LEAF_DATA) {
        Some(Lump::BvhLeaf(l)) => l,
        _ => {
            out.push(Issue::error(Some(ids::BVH_LEAF_DATA), "the map has no BVH leaf stream"));
            return;
        }
    };
    let masks: &[collision::ContentMask] = match get(ids::CONTENTS_MASKS) {
        Some(Lump::ContentMask(m)) => m,
        _ => return,
    };
    let surf_props: &[collision::SurfaceProp] = match get(ids::SURFACE_PROPERTIES) {
        Some(Lump::SurfaceProp(s)) => s,
        _ => return,
    };
    let vert_count = bsp.lump(ids::VERTICES).len() / 12;
    let leaf_dwords = leaves.len();
    let prop_count = get(ids::GAME_LUMP).and_then(|l| l.as_static_props().map(|s| s.prop_count()));

    let mut visited = collision::LeafVisited::new();
    // Every model gets its own walk, so a node table that every model can reach
    // would make the whole pass quadratic. The allowance is a multiple of what
    // a sound map needs; a map that hits it is reported rather than explored.
    let mut allowance = collision::walk_allowance(nodes.len(), models.len());
    let mut truncated = false;
    for (i, m) in models.iter().enumerate() {
        if out.full() || truncated || !m.has_collision() {
            continue;
        }
        if m.bvh_node as usize >= nodes.len() {
            out.push(Issue::error(Some(ids::MODELS), format!("model {i} starts at bvh node {} of {}", m.bvh_node, nodes.len())));
            continue;
        }
        if m.bvh_leaf < 0 || m.bvh_leaf as usize > leaf_dwords {
            out.push(Issue::error(Some(ids::MODELS), format!("model {i} starts at bvh leaf {} of {leaf_dwords} dwords", m.bvh_leaf)));
            continue;
        }
        if m.vert_index < 0 || m.vert_index as usize > vert_count {
            out.push(Issue::error(Some(ids::MODELS), format!("model {i} starts at vertex {} of {vert_count}", m.vert_index)));
        }
        if !m.decode_scale.is_finite() {
            out.push(Issue::error(Some(ids::MODELS), format!("model {i} has a decode scale of {}", m.decode_scale)));
        }
        for (a, what) in m.origin.iter().enumerate() {
            if !what.is_finite() {
                out.push(Issue::error(Some(ids::MODELS), format!("model {i} origin axis {a} is {what}")));
            }
        }
        let mut found = 0usize;
        let walk = leaves.walk_leaves_bounded(nodes, m, &mut visited, &mut allowance, |kind, at| {
            found += 1;
            let mut note = |issue: Issue| {
                out.push(issue);
                if found >= MODEL_ISSUE_CAP {
                    Ok(collision::Walk::Stop)
                } else {
                    Ok(collision::Walk::Continue)
                }
            };
            if kind > collision::CHILD_KIND_MAX {
                return note(Issue::warning(Some(ids::BVH_NODES), format!("model {i} reaches leaf kind {kind}, which is not one the engine dispatches")));
            }
            if at >= leaf_dwords {
                return note(Issue::error(Some(ids::BVH_LEAF_DATA), format!("model {i} leaf at {at} of {leaf_dwords} dwords")));
            }
            match kind {
                collision::CHILD_POLY => {
                    let poly = match leaves.poly_leaf(at) {
                        Ok(p) => p,
                        Err(e) => return note(Issue::warning(Some(ids::BVH_LEAF_DATA), format!("model {i}: {e}"))),
                    };
                    if poly.surface_prop as usize >= surf_props.len() {
                        return note(Issue::warning(
                            Some(ids::SURFACE_PROPERTIES),
                            format!("model {i} poly leaf uses surface prop {} of {}", poly.surface_prop, surf_props.len()),
                        ));
                    }
                    let mut over = 0usize;
                    // A poly leaf names positions in the whole VERTEXES lump,
                    // not in its own model.
                    poly.for_each_vertex(|world| {
                        if over == 0 && world as usize >= vert_count {
                            over = 1;
                        }
                    });
                    if over == 1 {
                        return note(Issue::warning(
                            Some(ids::VERTICES),
                            format!("model {i} poly leaf at {at} names a vertex past the {vert_count} stored"),
                        ));
                    }
                }
                collision::CHILD_HULL => {
                    if let Err(e) = leaves.hull_leaf(at) {
                        return note(Issue::warning(Some(ids::BVH_LEAF_DATA), format!("model {i}: {e}")));
                    }
                }
                collision::CHILD_PROP => {
                    if let Some(props) = prop_count {
                        match leaves.prop_index(at) {
                            Ok(idx) if idx as usize >= props => {
                                return note(Issue::warning(
                                    Some(ids::GAME_LUMP),
                                    format!("model {i} prop leaf names static prop {idx} of {props}"),
                                ))
                            }
                            Err(e) => return note(Issue::error(Some(ids::BVH_LEAF_DATA), format!("model {i}: {e}"))),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            Ok(collision::Walk::Continue)
        });
        match walk {
            Ok(collision::WalkLimit::Complete) => {}
            Ok(collision::WalkLimit::Exhausted) => truncated = true,
            Err(e) => out.push(Issue::warning(Some(ids::BVH_NODES), format!("model {i}: {e}"))),
        }
    }
    if truncated {
        out.push(Issue::warning(
            Some(ids::BVH_NODES),
            "the node tables overlap far enough that the leaf walk was cut short; the tree is not a forest of per-model trees",
        ));
    }
    for (i, n) in nodes.iter().enumerate() {
        if out.full() {
            break;
        }
        if n.contents_index() as usize >= masks.len() {
            out.push(Issue::error(Some(ids::CONTENTS_MASKS), format!("node {i} gates on contents {} of {}", n.contents_index(), masks.len())));
        }
        for slot in 0..4 {
            let kind = n.child_kind(slot);
            if kind > collision::CHILD_KIND_MAX {
                out.push(Issue::warning(Some(ids::BVH_NODES), format!("node {i} slot {slot} has child kind {kind}")));
            }
            if kind == collision::CHILD_NODE {
                // A node may point forwards or backwards, so only existence
                // is a defect.
                let child = n.child_index(slot) as usize;
                if child == i {
                    out.push(Issue::error(Some(ids::BVH_NODES), format!("node {i} slot {slot} points at itself")));
                }
                if child >= nodes.len() {
                    out.push(Issue::error(Some(ids::BVH_NODES), format!("node {i} slot {slot} names node {child} of {}", nodes.len())));
                }
            }
        }
    }
    for (i, s) in surf_props.iter().enumerate() {
        if out.full() {
            break;
        }
        if s.contents as usize >= masks.len() {
            out.push(Issue::error(Some(ids::SURFACE_PROPERTIES), format!("surface prop {i} names contents {} of {}", s.contents, masks.len())));
        }
    }
    if !bsp.lump(ids::PACKED_VERTICES).is_empty() && !bsp.lump(ids::PACKED_VERTICES).len().is_multiple_of(4) {
        out.push(Issue::error(Some(ids::PACKED_VERTICES), "size is not a multiple of the 4-byte record"));
    }
    if !bsp.lump(ids::VERTICES).len().is_multiple_of(12) {
        out.push(Issue::error(Some(ids::VERTICES), "size is not a multiple of the 12-byte record"));
    }
}

fn check_materials<'a>(get: &dyn Fn(u16) -> Option<&'a Lump>, out: &mut Issues) {
    let names: Option<&strings::StringTable> = match get(ids::SURFACE_NAMES) {
        Some(Lump::SurfaceNames(s)) => Some(s),
        _ => None,
    };
    let Some(names) = names else { return };
    let tex_count = match get(ids::TEXTURE_DATA) {
        Some(Lump::TexData(t)) => t.len(),
        _ => 0,
    };
    for (i, p) in match get(ids::SURFACE_PROPERTIES) {
        Some(Lump::SurfaceProp(s)) => s.iter().enumerate(),
        _ => return,
    } {
        if names.at_offset(p.name as usize).is_none() {
            out.push(Issue::error(
                Some(ids::SURFACE_PROPERTIES),
                format!("surface prop {i} names byte {} of a {}-byte name blob that is not a string start", p.name, names.len()),
            ));
        }
    }
    for (i, t) in match get(ids::TEXTURE_DATA) {
        Some(Lump::TexData(t)) => t.iter().enumerate(),
        _ => return,
    } {
        if names.at_offset(t.name as usize).is_none() {
            out.push(Issue::error(
                Some(ids::TEXTURE_DATA),
                format!("texdata {i} of {tex_count} names byte {} that is not a string start", t.name),
            ));
        }
    }
}

fn check_render<'a>(bsp: &Bsp, version: Version, get: &dyn Fn(u16) -> Option<&'a Lump>, out: &mut Issues) {
    let meshes: &[render::Mesh] = match get(ids::MESHES) {
        Some(Lump::Mesh(m)) => m,
        _ => return,
    };
    let sorts: &[render::MaterialSort] = match get(ids::MATERIAL_SORT) {
        Some(Lump::MaterialSort(s)) => s,
        _ => return,
    };
    let bounds: &[render::MeshBounds] = match get(ids::MESH_BOUNDS) {
        Some(Lump::MeshBounds(b)) => b,
        _ => return,
    };
    let tex_count = match get(ids::TEXTURE_DATA) {
        Some(Lump::TexData(t)) => t.len(),
        Some(Lump::Tf2TexData(t)) => t.len(),
        _ => 0,
    };
    let index_count = bsp.lump(ids::MESH_INDICES).len() / 2;
    let lightmap_pages = bsp.lump(ids::LIGHTMAP_HEADERS).len() / 8;
    let vert_count = bsp.lump(ids::VERTICES).len() / 12;
    let normal_count = bsp.lump(ids::VERTEX_NORMALS).len() / 12;

    if bounds.len() != meshes.len() {
        out.push(Issue::error(Some(ids::MESH_BOUNDS), format!("{} bounds for {} meshes", bounds.len(), meshes.len())));
    }
    for (i, m) in meshes.iter().enumerate() {
        if m.material_sort as usize >= sorts.len() {
            out.push(Issue::error(Some(ids::MESHES), format!("mesh {i} uses material sort {} of {}", m.material_sort, sorts.len())));
        }
        if m.first_index as usize + 3 * m.triangle_count as usize > index_count {
            out.push(Issue::error(
                Some(ids::MESH_INDICES),
                format!("mesh {i} covers indices {}..{} of {index_count}", m.first_index, m.first_index as usize + 3 * m.triangle_count as usize),
            ));
        }
        // The engine walks an odd mesh's triangle count rounded up to even;
        // the extra triangle must stay inside this mesh's vertex window.
        if version != Version::Tf2V37 && !m.triangle_count.is_multiple_of(2) {
            let at = m.first_index as usize + 3 * m.triangle_count as usize;
            let window = m.first_vertex..=m.first_vertex.saturating_add(m.lightmap_vertex_count);
            let raw = bsp.lump(ids::MESH_INDICES);
            let pad = raw.get(2 * at..2 * at + 6);
            let inside = pad.is_some_and(|p| p.chunks_exact(2).all(|c| window.contains(&u16::from_le_bytes([c[0], c[1]]))));
            if !raw.is_empty() && !inside {
                out.push(Issue::error(
                    Some(ids::MESHES),
                    format!("mesh {i} has an odd triangle count {} and the engine's padding triangle at index {at} leaves its vertex window", m.triangle_count),
                ));
            }
        }
        if version != Version::Tf2V37 && m.fade_distance != 0 {
            out.push(Issue::warning(
                Some(ids::MESHES),
                format!("mesh {i} carries a fade distance of {}; every shipped map writes 0", m.fade_distance),
            ));
        }
        if let Ok(stride) = render::vertex_lump_size(render::mesh_vertex_kind(m).0, version) {
            let (_, lump) = render::mesh_vertex_kind(m);
            // A dedicated-server map carries the mesh table for its
            // first_mesh bookkeeping and none of the vertex lumps it names.
            if bsp.lump(lump).is_empty() {
                continue;
            }
            // Both ends are inclusive and relative to the sort's window; a
            // sort with no lightmap addresses the pool from its start.
            let have = bsp.lump(lump).len() / stride;
            let base = sorts.get(m.material_sort as usize).map(|s| s.first_vertex as usize).unwrap_or(0);
            let first = base + m.first_vertex as usize;
            let last = first + m.lightmap_vertex_count as usize;
            if last >= have {
                out.push(Issue::error(
                    Some(lump),
                    format!("mesh {i} uses vertices {first}..={last} of {have}"),
                ));
            }
        } else {
            out.push(Issue::error(Some(ids::MESHES), format!("mesh {i} names vertex kind {}", render::mesh_vertex_kind(m).0)));
        }
    }
    for (i, s) in sorts.iter().enumerate() {
        if s.tex_data as usize >= tex_count {
            out.push(Issue::error(Some(ids::MATERIAL_SORT), format!("material sort {i} uses texdata {} of {tex_count}", s.tex_data)));
        }
        if s.lightmap >= 0 && s.lightmap as usize >= lightmap_pages {
            out.push(Issue::error(
                Some(ids::MATERIAL_SORT),
                format!("material sort {i} uses lightmap {} of {lightmap_pages} pages", s.lightmap),
            ));
        }
    }
    check_vertex_lump(bsp, version, ids::VERTEX_UNLIT, 20, None, vert_count, normal_count, out);
    check_vertex_lump(bsp, version, ids::VERTEX_LIT_FLAT, 28, None, vert_count, normal_count, out);
    check_vertex_lump(bsp, version, ids::VERTEX_LIT_BUMP, 32, Some(28), vert_count, normal_count, out);
    check_vertex_lump(bsp, version, ids::VERTEX_UNLIT_TS, 24, Some(20), vert_count, normal_count, out);
}

#[allow(clippy::too_many_arguments)]
fn check_vertex_lump(
    bsp: &Bsp,
    version: Version,
    id: u16,
    stride: usize,
    tangent_dword: Option<usize>,
    vert_count: usize,
    normal_count: usize,
    out: &mut Issues,
) {
    let data = bsp.lump(id);
    if data.is_empty() {
        return;
    }
    if version == Version::Tf2V37 {
        return;
    }
    if stride < 8 || !stride.is_multiple_of(4) {
        return;
    }
    if !data.len().is_multiple_of(stride) {
        out.push(Issue::error(Some(id), format!("size is not a multiple of the {stride}-byte record")));
        return;
    }
    let per_record = stride / 4;
    // The tangent is named by its byte offset inside the record.
    let tangent = tangent_dword.map(|byte| byte / 4).filter(|w| *w + 1 < per_record);
    let word = |record: usize, field: usize| -> u32 {
        let at = (record * per_record + field) * 4;
        u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
    };
    for i in 0..data.len() / stride {
        if out.full() {
            break;
        }
        if word(i, 0) as usize >= vert_count {
            out.push(Issue::error(Some(id), format!("record {i} names position {} of {vert_count}", word(i, 0))));
        }
        if word(i, 1) as usize >= normal_count {
            out.push(Issue::error(Some(id), format!("record {i} names normal {} of {normal_count}", word(i, 1))));
        }
        if let Some(td) = tangent {
            let t = word(i, td) & 0x7FFF_FFFF;
            if t as usize >= normal_count {
                out.push(Issue::error(Some(id), format!("record {i} names tangent {t} of {normal_count}")));
            }
        }
    }
}

fn check_vis<'a>(bsp: &Bsp, get: &dyn Fn(u16) -> Option<&'a Lump>, out: &mut Issues) {
    let nodes: &[vis::AabbNode] = match get(ids::CELL_AABB_NODES) {
        Some(Lump::AabbNode(n)) => n,
        _ => return,
    };
    let refs: Vec<u32> = match get(ids::OBJ_REFERENCES) {
        Some(l) => l.obj_refs().unwrap_or_default(),
        _ => Vec::new(),
    };
    let cells: &[vis::Cell] = match get(ids::CELLS) {
        Some(Lump::Cell(c)) => c,
        _ => &[],
    };
    let portals: &[vis::Portal] = match get(ids::PORTALS) {
        Some(Lump::Portal(p)) => p,
        _ => &[],
    };
    let portal_verts = bsp.lump(ids::PORTAL_VERTICES).len() / 12;
    let bounds_count = bsp.lump(ids::OBJ_REFERENCE_BOUNDS).len() / 32;
    let masks = bsp.lump(ids::CONTENTS_MASKS).len() / 4;

    for (i, n) in nodes.iter().enumerate() {
        let stop = n.first_child() as usize + n.child_count() as usize;
        if n.child_count() > 0 && stop > nodes.len() {
            out.push(Issue::error(Some(ids::CELL_AABB_NODES), format!("node {i} names children {}..{stop} of {}", n.first_child(), nodes.len())));
        }
        if n.child_count() > 0 && n.first_child() <= i as u32 {
            out.push(Issue::warning(Some(ids::CELL_AABB_NODES), format!("node {i} names a child at or before itself")));
        }
        let ref_stop = n.first_obj_ref() as usize + n.obj_ref_count() as usize;
        if ref_stop > refs.len() {
            out.push(Issue::error(
                Some(ids::OBJ_REFERENCES),
                format!("node {i} names object refs {}..{ref_stop} of {}", n.first_obj_ref(), refs.len()),
            ));
        }
    }
    // A cell ref names a render mesh, or a static prop counted from the level
    // info's first static prop index. In staging_mu1 every ref below 4672 has
    // a box inside that mesh's bounds record, and the rest run from 4672 to
    // 4672 + 11197, one past each of its props.
    let meshes = bsp.lump(ids::MESHES).len() / 28;
    let props = get(ids::GAME_LUMP).and_then(|l| l.as_static_props().map(|s| s.prop_count())).unwrap_or(0);
    if let Some(Lump::LevelInfo(li)) = get(ids::LEVEL_INFO) {
        let base = li.first_static_prop_index as usize;
        let stray: Vec<u32> = refs
            .iter()
            .copied()
            .filter(|r| {
                let r = *r as usize;
                r >= meshes && !(r >= base && r < base + props)
            })
            .collect();
        if let Some(max) = stray.iter().max() {
            out.push(Issue::warning(
                Some(ids::OBJ_REFERENCES),
                format!(
                    "{} cell refs (up to {max}) name neither one of the {meshes} meshes nor one of the {props} static props from {base}",
                    stray.len()
                ),
            ));
        }
        if props > 0 && base < meshes {
            out.push(Issue::warning(
                Some(ids::LEVEL_INFO),
                format!("static prop refs start at {base}, inside the {meshes} mesh refs"),
            ));
        }
    }
    if bounds_count != refs.len() {
        out.push(Issue::error(
            Some(ids::OBJ_REFERENCE_BOUNDS),
            format!("{bounds_count} bounds for {} object refs", refs.len()),
        ));
    }
    if let Some(Lump::CellAabbTotals(t)) = get(ids::CELL_AABB_SUBTREE_REF_TOTALS) {
        if t.len() > nodes.len() {
            out.push(Issue::error(
                Some(ids::CELL_AABB_SUBTREE_REF_TOTALS),
                format!("{} totals for {} nodes", t.len(), nodes.len()),
            ));
        }
    }
    if let Some(Lump::FadeDist(f)) = get(ids::AABB_FADE_DISTS) {
        if f.len() != nodes.len() {
            out.push(Issue::error(
                Some(ids::AABB_FADE_DISTS),
                format!("{} fade distances for {} nodes", f.len(), nodes.len()),
            ));
        }
    }
    let _ = masks;
    // The v47 server keeps the client's cells but not the portals they name;
    // it never walks them.
    let portals_shipped = bsp.version_kind() != Some(Version::DediV47);
    for (i, c) in cells.iter().enumerate() {
        if portals_shipped && c.portal_end() > portals.len() {
            out.push(Issue::error(
                Some(ids::CELLS),
                format!("cell {i} spans portals {}..{} of {}", c.first_portal, c.portal_end(), portals.len()),
            ));
        }
    }
    let ref_count = bsp.lump(ids::PORTAL_VERTEX_REFERENCES).len() / 2;
    let plane_count = bsp.lump(ids::PLANES).len() / 16;
    for (i, p) in portals.iter().enumerate() {
        if p.ref_end() > ref_count {
            out.push(Issue::error(
                Some(ids::PORTALS),
                format!("portal {i} covers refs {}..{} of {ref_count}", p.first_ref, p.ref_end()),
            ));
        }
        if p.plane as usize >= plane_count {
            out.push(Issue::error(Some(ids::PORTALS), format!("portal {i} names plane {} of {plane_count}", p.plane)));
        }
        if p.cell_to as usize > cells.len() + 1 {
            out.push(Issue::error(Some(ids::PORTALS), format!("portal {i} leads to cell {} of {}", p.cell_to, cells.len())));
        }
    }
    let edge_bytes = bsp.lump(ids::PORTAL_EDGES).len();
    if !portals.is_empty() {
        let refs = bsp.lump(ids::PORTAL_VERTEX_REFERENCES);
        let bad = refs.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).filter(|v| *v as usize >= portal_verts).count();
        if bad > 0 {
            out.push(Issue::error(Some(ids::PORTAL_VERTEX_REFERENCES), format!("{bad} refs past the {portal_verts} portal vertices")));
        }
        let edges = edge_bytes / 4;
        let bad = bsp
            .lump(ids::PORTAL_EDGE_REFERENCES)
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]) >> 1)
            .filter(|e| *e as usize >= edges)
            .count();
        if bad > 0 {
            out.push(Issue::error(Some(ids::PORTAL_EDGE_REFERENCES), format!("{bad} refs past the {edges} portal edges")));
        }
    }
    let vert_edge_records = bsp.lump(ids::PORTAL_VERTEX_EDGES).len() / 16;
    if vert_edge_records != portal_verts {
        out.push(Issue::error(
            Some(ids::PORTAL_VERTEX_EDGES),
            format!("{vert_edge_records} records for {portal_verts} portal vertices"),
        ));
    }
    let vert_refs = bsp.lump(ids::PORTAL_VERTEX_REFERENCES).len();
    if vert_refs != bsp.lump(ids::PORTAL_EDGE_REFERENCES).len() {
        out.push(Issue::error(
            Some(ids::PORTAL_EDGE_REFERENCES),
            format!("{} edge refs for {vert_refs} vertex refs", bsp.lump(ids::PORTAL_EDGE_REFERENCES).len()),
        ));
    }
    let isect = bsp.lump(ids::PORTAL_EDGE_INTERSECT_EDGE).len() / vis::ISECT_LIST_SIZE;
    if bsp.lump(ids::PORTAL_EDGE_INTERSECT_AT_VERTEX).len() / vis::ISECT_LIST_SIZE != isect {
        out.push(Issue::error(
            Some(ids::PORTAL_EDGE_INTERSECT_AT_VERTEX),
            format!("intersect-at-vertex list does not match the {isect}-record intersect-edge list"),
        ));
    }
    if let Some(Lump::PortalIsectHeader(h)) = get(ids::PORTAL_EDGE_INTERSECT_HEADER) {
        if h.len() != edge_bytes / 4 {
            out.push(Issue::error(
                Some(ids::PORTAL_EDGE_INTERSECT_HEADER),
                format!("{} headers for {} portal edges", h.len(), edge_bytes / 4),
            ));
        }
        if let Err(e) = vis::isect_slices(h, isect) {
            out.push(Issue::error(Some(ids::PORTAL_EDGE_INTERSECT_HEADER), e.to_string()));
        }
        if let Some(i) = h.iter().position(|x| x.count == 0) {
            out.push(Issue::error(
                Some(ids::PORTAL_EDGE_INTERSECT_HEADER),
                format!("edge {i} has no intersection record; the client reads one regardless"),
            ));
        }
    }
    for (id, stride) in [
        (ids::CELL_BSP_NODES, 8usize),
        (ids::CELLS, 8),
        (ids::PORTALS, 12),
        (ids::PORTAL_VERTICES, 12),
        (ids::PORTAL_EDGES, 4),
        (ids::PORTAL_VERTEX_EDGES, 16),
        (ids::PORTAL_VERTEX_REFERENCES, 2),
        (ids::PORTAL_EDGE_REFERENCES, 2),
        (ids::PORTAL_EDGE_INTERSECT_EDGE, 16),
        (ids::PORTAL_EDGE_INTERSECT_AT_VERTEX, 16),
        (ids::PORTAL_EDGE_INTERSECT_HEADER, 8),
        (ids::OCCLUSION_MESH_VERTICES, 12),
        (ids::OCCLUSION_MESH_INDICES, 2),
    ] {
        if !bsp.lump(id).is_empty() && !bsp.lump(id).len().is_multiple_of(stride) {
            out.push(Issue::error(Some(id), format!("size is not a multiple of the {stride}-byte record")));
        }
    }
}

fn check_csm<'a>(get: &dyn Fn(u16) -> Option<&'a Lump>, out: &mut Issues) {
    let nodes: Vec<csm::CsmAabbNode> = match get(ids::CSM_AABB_NODES) {
        Some(Lump::CsmAabbNode(n)) => n.clone(),
        _ => return,
    };
    let refs: Vec<u32> = match get(ids::CSM_OBJ_REFERENCES) {
        Some(Lump::CsmObjRef(r)) => r.iter().map(|x| x.get()).collect(),
        Some(Lump::CsmObjRef16(r)) => r.iter().map(|x| x.value as u32).collect(),
        _ => Vec::new(),
    };
    let meshes: Vec<csm::ShadowMesh> = match get(ids::SHADOW_MESHES) {
        Some(Lump::ShadowMesh(m)) => m.clone(),
        _ => return,
    };
    let envs: Vec<lighting::ShadowEnvironment> = match get(ids::SHADOW_ENVIRONMENTS) {
        Some(Lump::ShadowEnvironment(e)) => e.clone(),
        _ => return,
    };
    for (i, n) in nodes.iter().enumerate() {
        let stop = n.first_child() as usize + n.child_count() as usize;
        if n.child_count() > 0 && (stop > nodes.len() || n.first_child() <= i as u32) {
            out.push(Issue::error(
                Some(ids::CSM_AABB_NODES),
                format!("node {i} names children {}..{stop} of {}", n.first_child(), nodes.len()),
            ));
        }
        let ref_stop = n.first_obj_ref() as usize + n.obj_ref_count() as usize;
        if ref_stop > refs.len() {
            out.push(Issue::error(
                Some(ids::CSM_OBJ_REFERENCES),
                format!("csm node {i} names refs {}..{ref_stop} of {}", n.first_obj_ref(), refs.len()),
            ));
        }
    }
    for (i, e) in envs.iter().enumerate() {
        if e.aabb_root as usize >= nodes.len() || e.aabb_end > nodes.len() as u32 {
            out.push(Issue::error(Some(ids::SHADOW_ENVIRONMENTS), format!("cascade {i} names csm nodes {}..{} of {}", e.aabb_root, e.aabb_end, nodes.len())));
        }
        if e.aabb_refs_begin > e.aabb_refs_end || e.aabb_refs_end as usize > refs.len() {
            out.push(Issue::error(
                Some(ids::SHADOW_ENVIRONMENTS),
                format!("cascade {i} names csm refs {}..{} of {}", e.aabb_refs_begin, e.aabb_refs_end, refs.len()),
            ));
        }
        if e.shadow_mesh_begin > e.shadow_mesh_end || e.shadow_mesh_end as usize > meshes.len() {
            out.push(Issue::error(
                Some(ids::SHADOW_ENVIRONMENTS),
                format!("cascade {i} names shadow meshes {}..{} of {}", e.shadow_mesh_begin, e.shadow_mesh_end, meshes.len()),
            ));
        }
        if e.shadow_direction.iter().any(|f| !f.is_finite()) {
            out.push(Issue::error(Some(ids::SHADOW_ENVIRONMENTS), format!("cascade {i} has a non-finite shadow direction")));
        }
    }
    // A shadow ref names a world shadow mesh, or a static prop counted from the
    // level info's first static prop index: staging_mu1's 6146 refs are its 49
    // shadow meshes once each and 6097 props from 4672 up, nothing between.
    // Anything else is summarised once rather than per ref.
    let props = get(ids::GAME_LUMP).and_then(|l| l.as_static_props().map(|s| s.prop_count())).unwrap_or(0);
    let prop_base = match get(ids::LEVEL_INFO) {
        Some(Lump::LevelInfo(li)) => li.first_static_prop_index as usize,
        _ => meshes.len(),
    };
    if props > 0 && meshes.len() > prop_base {
        out.push(Issue::error(
            Some(ids::SHADOW_MESHES),
            format!(
                "{} shadow meshes run past the first static prop index {prop_base}: the engine reads prop refs as shadow meshes and crashes",
                meshes.len()
            ),
        ));
    }
    let beyond: Vec<u32> = refs
        .iter()
        .copied()
        .filter(|r| {
            let r = *r as usize;
            r >= meshes.len() && !(r >= prop_base && r < prop_base + props)
        })
        .collect();
    if let Some(max) = beyond.iter().max() {
        out.push(Issue::warning(
            Some(ids::CSM_OBJ_REFERENCES),
            format!(
                "{} csm refs (up to {max}) name neither one of the {} shadow meshes nor one of the {props} static props from {prop_base}",
                beyond.len(),
                meshes.len()
            ),
        ));
    }
    if let Ok(total) = csm::declared_triangles(&meshes) {
        let indices = match get(ids::SHADOW_MESH_INDICES) {
            Some(Lump::ShadowMeshIndex(i)) => i.len(),
            _ => 0,
        };
        if total as usize * 3 != indices {
            out.push(Issue::error(
                Some(ids::SHADOW_MESH_INDICES),
                format!("{} indices for {total} declared shadow mesh triangles", indices),
            ));
        }
    }
    if let Some(Lump::CsmAabbTotals(t)) = get(ids::CSM_AABB_SUBTREE_REF_TOTALS) {
        if t.len() > nodes.len() {
            out.push(Issue::error(
                Some(ids::CSM_AABB_SUBTREE_REF_TOTALS),
                format!("{} totals for {} csm nodes", t.len(), nodes.len()),
            ));
        }
    }
}

/// The client's nearest-probe walk over LUMP_LIGHTPROBETREE.
///
/// It descends from node 0 into both children of an interior node until it
/// reaches leaves, then scans each leaf's references. So every node reachable
/// from the root has to be in range, the descent has to end, and a leaf's count
/// is an integer that stays inside the reference lump. A leaf count written as
/// a float passes a float-reading check and crashes the client.
pub fn probe_tree_issues(nodes: &[lighting::LightProbeTreeNode], refs: usize) -> Vec<Issue> {
    let mut out = Issues::default();
    check_probe_tree(nodes, refs, &mut out);
    out.into_vec()
}

fn check_probe_tree(nodes: &[lighting::LightProbeTreeNode], refs: usize, out: &mut Issues) {
    let id = Some(ids::LIGHTPROBE_TREE);
    if nodes.is_empty() {
        return;
    }
    let mut seen = vec![false; nodes.len()];
    let mut stack = vec![0usize];
    while let Some(i) = stack.pop() {
        if seen[i] {
            out.push(Issue::error(
                id,
                format!("node {i} is reached twice from the root; the client descends it forever"),
            ));
            return;
        }
        seen[i] = true;
        let n = &nodes[i];
        let first = n.first() as usize;
        if n.is_leaf() {
            let count = n.count() as usize;
            if first + count > refs {
                let hint = if n.count() >= 0x3000_0000 {
                    format!(" (the count reads as the float {}, not an integer)", f32::from_bits(n.count()))
                } else {
                    String::new()
                };
                out.push(Issue::error(
                    id,
                    format!("leaf {i} covers refs {first}..{} of {refs}{hint}", first + count),
                ));
            }
            continue;
        }
        if first <= i || first + 1 >= nodes.len() {
            out.push(Issue::error(
                id,
                format!(
                    "node {i} is interior and names children {first} and {} of {}; they must follow it",
                    first + 1,
                    nodes.len()
                ),
            ));
            continue;
        }
        if !n.split().is_finite() {
            out.push(Issue::error(id, format!("node {i} splits at a non-finite coordinate")));
        }
        stack.push(first + 1);
        stack.push(first);
    }
}

fn check_lighting<'a>(get: &dyn Fn(u16) -> Option<&'a Lump>, out: &mut Issues) {
    let probes: usize = match get(ids::LIGHTPROBES) {
        Some(Lump::LightProbe(p)) => p.len(),
        Some(Lump::LightProbe48(p)) => p.len(),
        _ => 0,
    };
    let refs: usize = match get(ids::LIGHTPROBE_REFERENCES) {
        Some(Lump::LightProbeRef(r)) => {
            // The client reads the probe record at `44 * probe` for the nearest
            // reference it finds, so an index past the probes reads off the lump.
            for (i, p) in r.iter().enumerate() {
                if p.probe as usize >= probes {
                    out.push(Issue::error(
                        Some(ids::LIGHTPROBE_REFERENCES),
                        format!("probe ref {i} names probe {} of {probes}", p.probe),
                    ));
                }
                if !p.origin.iter().all(|c| c.is_finite()) {
                    out.push(Issue::error(
                        Some(ids::LIGHTPROBE_REFERENCES),
                        format!("probe ref {i} has a non-finite origin"),
                    ));
                }
            }
            r.len()
        }
        _ => 0,
    };
    if let Some(Lump::LightProbeTreeNode(nodes)) = get(ids::LIGHTPROBE_TREE) {
        check_probe_tree(nodes, refs, out);
    }
    if let Some(Lump::PropLightprobeIndex(p)) = get(ids::STATIC_PROP_LIGHTPROBE_INDICES) {
        for (i, x) in p.iter().enumerate() {
            if probes > 0 && x.value as usize >= probes {
                out.push(Issue::error(
                    Some(ids::STATIC_PROP_LIGHTPROBE_INDICES),
                    format!("static prop {i} names lightprobe {} of {probes}", x.value),
                ));
            }
        }
    }
    let lights = match get(ids::WORLD_LIGHTS) {
        Some(Lump::WorldLight(l)) => l.len(),
        _ => 0,
    };
    if let Some(Lump::RtlIndex(r)) = get(ids::LIGHTMAP_DATA_RTL_PAGE) {
        if r.len() % lighting::RTL_PAGE_SLOTS != 0 {
            out.push(Issue::error(
                Some(ids::LIGHTMAP_DATA_RTL_PAGE),
                format!("{} bytes is not a whole number of {}-byte pages", r.len() * 2, lighting::RTL_PAGE_BYTES),
            ));
        }
        for (i, x) in r.iter().enumerate() {
            if lights > 0 && x.value as usize >= lights {
                out.push(Issue::error(
                    Some(ids::LIGHTMAP_DATA_RTL_PAGE),
                    format!("real-time light slot {i} names light {} of {lights}", x.value),
                ));
            }
        }
    }
    let cubemaps = match get(ids::CUBEMAPS) {
        Some(Lump::Cubemap(c)) => c.len(),
        _ => 0,
    };
    if cubemaps > 0 {
        match get(ids::CUBEMAPS_AMBIENT_RCP) {
            Some(Lump::CubemapAmbient(a)) if a.len() != cubemaps => out.push(Issue::error(
                Some(ids::CUBEMAPS_AMBIENT_RCP),
                format!("{} ambient scales for {cubemaps} cubemaps", a.len()),
            )),
            Some(Lump::StringTable(_)) => out.push(Issue::warning(
                Some(ids::CUBEMAPS_AMBIENT_RCP),
                "the slot holds a name blob, not cubemap ambient scales",
            )),
            _ => {}
        }
    }
    if let Some(Lump::TweakLight(_)) = get(ids::TWEAK_LIGHTS) {
        if lights == 0 {
            out.push(Issue::info(Some(ids::TWEAK_LIGHTS), "tweak light ids are present with no world lights"));
        }
    }
    for (i, w) in match get(ids::WORLD_LIGHTS) {
        Some(Lump::WorldLight(l)) => l.iter().enumerate(),
        _ => return,
    } {
        if !w.origin.iter().chain(w.direction.iter()).chain(w.color.iter()).all(|f| f.is_finite()) {
            out.push(Issue::error(Some(ids::WORLD_LIGHTS), format!("light {i} has a non-finite field")));
        }
        if w.light_type > 4 {
            out.push(Issue::warning(Some(ids::WORLD_LIGHTS), format!("light {i} has type {}", w.light_type)));
        }
    }
}

fn check_props<'a>(get: &dyn Fn(u16) -> Option<&'a Lump>, out: &mut Issues) {
    let Some(gl) = get(ids::GAME_LUMP) else { return };
    let Lump::GameLump(g) = gl else { return };
    if g.entries.is_empty() {
        out.push(Issue::warning(Some(ids::GAME_LUMP), "the game lump has no entries"));
        return;
    }
    let Some(sprp) = g.static_props() else {
        out.push(Issue::error(Some(ids::GAME_LUMP), "no sprp block, so the map has no static props"));
        return;
    };
    for e in &g.entries {
        if e.is_sprp() {
            if ![47, 51, 52].contains(&e.version) {
                out.push(Issue::warning(
                    Some(ids::GAME_LUMP),
                    format!("the sprp version is {:#06x}; shipped maps use the map version there", e.version),
                ));
            }
            continue;
        }
        out.push(Issue::info(Some(ids::GAME_LUMP), format!("game lump {:?} is carried verbatim", String::from_utf8_lossy(&e.id))));
    }
    if sprp.counts[0] as usize != sprp.props.len() {
        out.push(Issue::error(
            Some(ids::GAME_LUMP),
            format!("the sprp prop count is {} but {} records follow", sprp.counts[0], sprp.props.len()),
        ));
    }
    if sprp.tail_count != 0 {
        out.push(Issue::warning(Some(ids::GAME_LUMP), format!("the sprp trailing section count is {}", sprp.tail_count)));
    }
    let empty = sprp.models.iter().position(|m| m.is_empty());
    if let Some(i) = empty {
        out.push(Issue::error(Some(ids::GAME_LUMP), format!("static prop model {i} has an empty path")));
    }
    for (i, p) in sprp.props.iter().enumerate() {
        if p.model_index as usize >= sprp.models.len() {
            out.push(Issue::error(
                Some(ids::GAME_LUMP),
                format!("static prop {i} uses model {} of {}", p.model_index, sprp.models.len()),
            ));
        }
        if p.diffuse_modulation == 0 {
            out.push(Issue::warning(Some(ids::GAME_LUMP), format!("static prop {i} modulates to black")));
        }
    }
    for (i, m) in sprp.models.iter().enumerate() {
        if m.starts_with("models/") || m.ends_with(".mdl") {
            out.push(Issue::error(
                Some(ids::GAME_LUMP),
                format!("static prop model {i} is {m:?}, an asset path rather than a map brush model reference"),
            ));
        }
    }
    if let Some(Lump::LevelInfo(li)) = get(ids::LEVEL_INFO) {
        let meshes = match get(ids::MESHES) {
            Some(Lump::Mesh(m)) => m.len(),
            _ => 0,
        };
        let (decal, transparent, sky) = (li.first_decal_mesh, li.first_transparent_mesh, li.first_sky_mesh);
        if decal > transparent || transparent > sky || sky as usize > meshes {
            out.push(Issue::error(
                Some(ids::LEVEL_INFO),
                format!("the mesh ranges decal {decal}, transparent {transparent}, sky {sky} are not ordered within {meshes} meshes"),
            ));
        } else if meshes > 0 && decal == 0 {
            out.push(Issue::warning(Some(ids::LEVEL_INFO), "the first decal mesh is 0, so the opaque pass draws no world mesh"));
        }
        if li.first_static_prop_index != level::first_static_prop_index(sky) {
            out.push(Issue::warning(
                Some(ids::LEVEL_INFO),
                format!("the first static prop index is {}, not the sky mesh {sky} rounded up to 64", li.first_static_prop_index),
            ));
        }
        if li.num_static_props as usize != sprp.props.len() {
            out.push(Issue::warning(
                Some(ids::LEVEL_INFO),
                format!("level info says {} static props, the sprp block holds {}", li.num_static_props, sprp.props.len()),
            ));
        }
    }
    let _ = game_lump::DIRECTORY_ENTRY_SIZE;
}

fn check_lightmap_pages<'a>(bsp: &Bsp, get: &dyn Fn(u16) -> Option<&'a Lump>, out: &mut Issues) {
    let headers: Vec<lighting::LightmapHeader> = match get(ids::LIGHTMAP_HEADERS) {
        Some(Lump::LightmapHeader(h)) => h.clone(),
        _ => return,
    };
    if headers.is_empty() {
        return;
    }
    let sky = bsp.lump(ids::LIGHTMAP_DATA_SKY).len();
    let want: usize = headers
        .iter()
        .filter(|h| h.has_sky_data())
        .filter_map(|h| lighting::lightmap_page_bytes(h.lightmap_type, h.width, h.height))
        .sum();
    if sky > 0 && sky != want {
        out.push(Issue::warning(
            Some(ids::LIGHTMAP_DATA_SKY),
            format!("{sky} bytes stored, the {}-entry header table accounts for {want}", headers.len()),
        ));
    }
    let compressed = bsp.lump(ids::LIGHTMAP_DATA_SKY_COMPRESSED).len();
    let want_c: usize = headers.iter().filter_map(|h| lighting::lightmap_page_bytes(h.compressed_type, h.width, h.height)).sum();
    if compressed > 0 && compressed != want_c {
        out.push(Issue::warning(
            Some(ids::LIGHTMAP_DATA_SKY_COMPRESSED),
            format!("{compressed} bytes stored, the header table accounts for {want_c}"),
        ));
    }
    let rtl = bsp.lump(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS).len();
    let want_r: usize = headers.iter().filter(|h| h.has_sky_data()).map(|h| lighting::rtl_page_bytes(h.width, h.height)).sum();
    if rtl > 0 && rtl != want_r {
        out.push(Issue::warning(
            Some(ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS),
            format!("{rtl} bytes stored, the header table accounts for {want_r}"),
        ));
    }
    for h in &headers {
        for (which, t) in [("lightmap", h.lightmap_type), ("compressed", h.compressed_type)] {
            if t != 0 && lighting::lightmap_page_bytes(t, h.width, h.height).is_none() {
                out.push(Issue::warning(
                    Some(ids::LIGHTMAP_HEADERS),
                    format!("{which} format {t} is not one the engine's switch handles"),
                ));
            }
        }
    }
}

fn check_entities<'a>(bsp: &Bsp, ents: &BTreeMap<String, Vec<u8>>, get: &dyn Fn(u16) -> Option<&'a Lump>, out: &mut Issues) {
    let model_count = bsp.lump(ids::MODELS).len() / if bsp.version_kind() == Some(Version::Tf2V37) { 32 } else { 64 };
    let world = bsp.lump(ids::ENTITIES);
    let mut all: Vec<(&str, &[u8])> = vec![("worldspawn", world)];
    for (name, data) in ents {
        all.push((name.as_str(), data.as_slice()));
    }
    for (label, data) in all {
        if data.is_empty() {
            continue;
        }
        let parsed = match entities::parse_id(0x00, data, Version::ClientV51) {
            Ok(p) => p,
            Err(e) => {
                out.push(Issue::error(Some(ids::ENTITIES), format!("{label}: {e}")));
                continue;
            }
        };
        if parsed.entities.len() != entities::count_blocks(data) {
            out.push(Issue::error(
                Some(ids::ENTITIES),
                format!("{label}: parsed {} entities from {} blocks", parsed.entities.len(), entities::count_blocks(data)),
            ));
        }
        if !entities::terminator_is_intact(data) {
            let tail = &data[data.len().saturating_sub(4)..];
            out.push(Issue::error(
                Some(ids::ENTITIES),
                format!(
                    "{label}: the last block closes at byte {} of {} and the blob ends {tail:?}, not `}}`, newline, NUL; \
                     the engine reads one step past the buffer otherwise",
                    data.iter().rposition(|&b| b == b'}').unwrap_or(0),
                    data.len(),
                ),
            ));
        }
        for key in parsed.model_keys() {
            if !entities::is_model_key(key) {
                out.push(Issue::error(Some(ids::ENTITIES), format!("{label}: {key:?} is not a well-formed model key")));
                continue;
            }
            match entities::model_index(key) {
                Some(i) if i as usize >= model_count => out.push(Issue::error(
                    Some(ids::ENTITIES),
                    format!("{label}: {key:?} names model {i} of {model_count}"),
                )),
                None => out.push(Issue::error(Some(ids::ENTITIES), format!("{label}: {key:?} has no index"))),
                _ => {}
            }
        }
    }
    if let Some(Lump::EntityPartitions(p)) = get(ids::ENTITY_PARTITIONS) {
        let declared: BTreeSet<&str> = p.names.iter().map(|s| s.as_str()).collect();
        let present: BTreeSet<&str> = ents.keys().map(|s| s.as_str()).collect();
        for n in declared.difference(&present) {
            out.push(Issue::error(
                Some(ids::ENTITY_PARTITIONS),
                format!("the partition list names {n:?} but no {n}.ent file was read"),
            ));
        }
        for n in present.difference(&declared) {
            out.push(Issue::warning(
                Some(ids::ENTITY_PARTITIONS),
                format!("a {n}.ent file is present but the partition list does not name it"),
            ));
        }
    } else if !ents.is_empty() {
        out.push(Issue::warning(Some(ids::ENTITY_PARTITIONS), "partition files were read but the map has no partition list"));
    }
}


/// True when nothing came back at Error severity.
pub fn is_clean(issues: &[Issue]) -> bool {
    !issues.iter().any(|i| i.is_error())
}

/// The Error-severity issues only, for a caller that wants to fail on them.
pub fn errors(issues: &[Issue]) -> Vec<&Issue> {
    issues.iter().filter(|i| i.is_error()).collect()
}

impl From<Error> for Issue {
    fn from(e: Error) -> Self {
        let lump = match &e {
            Error::LumpRange { id } | Error::Stride { id, .. } | Error::Invalid { id, .. } => Some(*id),
            _ => None,
        };
        Issue::error(lump, e.to_string())
    }
}
