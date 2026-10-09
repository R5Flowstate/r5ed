//! Conversions as Commands: Make Editable (glb -> `.r5mesh`), Brush -> Mesh,
//! and the editable `MeshSpec` output. Plus `load_triangles`, the one branch
//! point every `.r5mesh` reader routes through.

use std::collections::BTreeMap;

use glam::DVec3;

use super::{bad, Adjacency, EditError, EditMesh, Face, FaceFlags};
use crate::brush::{self, Solid};
use crate::command::Command;
use crate::doc::{new_object, DocError, Document};
use crate::import::glb::{self, GlbMesh};
use crate::model::{CollisionMode, Id, Mesh, ObjectKind};

/// One mesh source, loaded but unevaluated: the single format decision in
/// [`load_triangles`] reads the `.r5mesh` base here and raw glb prims there.
#[derive(Clone, Debug)]
pub enum LoadedMesh {
    Editable(EditMesh),
    Soup(Vec<GlbMesh>),
}

/// The single branch point for mesh sources: `.r5mesh` reads as the editable
/// base, anything else as raw glb prims with node transforms baked. Errors
/// carry no file name; the caller names the file once.
pub fn load_triangles(bytes: &[u8], path: &str) -> Result<LoadedMesh, EditError> {
    if path.ends_with(".r5mesh") {
        Ok(LoadedMesh::Editable(super::io::read(bytes)?))
    } else {
        Ok(LoadedMesh::Soup(glb::read_split(bytes).map_err(|e| bad(e.to_string()))?))
    }
}

/// As [`load_triangles`], but the base runs through its modifier stack first.
/// A glb-sourced mesh with modifiers is welded by value, then deformed.
pub fn load_triangles_stacked(
    bytes: &[u8],
    path: &str,
    mods: &[super::Modifier],
    ctx: &super::EvalContext,
) -> Result<Vec<GlbMesh>, EditError> {
    let staged = load_triangles(bytes, path).map_err(|e| bad(format!("{path}: {e}")))?;
    match staged {
        LoadedMesh::Editable(base) => stacked_primitives(&base, mods, ctx),
        LoadedMesh::Soup(prims) => {
            if mods.iter().all(|m| !m.enabled) {
                Ok(prims)
            } else {
                stacked_primitives(&weld_primitives(&prims)?, mods, ctx)
            }
        }
    }
}

/// The evaluated `.r5mesh` as one `GlbMesh` per material slot, in slot order.
pub fn stacked_primitives(
    base: &EditMesh,
    mods: &[super::Modifier],
    ctx: &super::EvalContext,
) -> Result<Vec<GlbMesh>, EditError> {
    Ok(evaluated_primitives(&super::evaluate_stack(base, mods, ctx, false)?))
}

pub fn evaluated_primitives(mesh: &EditMesh) -> Vec<GlbMesh> {
    let eval = super::eval::evaluate(mesh);
    let mut out = Vec::with_capacity(eval.ranges.len());
    for r in &eval.ranges {
        let idx = &eval.indices[r.start as usize..r.start as usize + r.count as usize];
        let mut used: BTreeMap<u32, u32> = BTreeMap::new();
        let mut prim = GlbMesh {
            name: mesh.materials.get(r.material).cloned().unwrap_or_default(),
            material: mesh.materials.get(r.material).cloned().unwrap_or_default(),
            ..GlbMesh::default()
        };
        for i in idx {
            let n = used.len() as u32;
            let v = *used.entry(*i).or_insert(n);
            if v == n {
                prim.positions.push(eval.positions[*i as usize]);
                prim.normals.push(eval.normals[*i as usize]);
                prim.uvs.push(eval.uvs[*i as usize]);
            }
            prim.indices.push(v);
        }
        out.push(prim);
    }
    out
}

/// Welds triangle-soup primitives by position within 1e-4, keeping corner UVs
/// and normals, with glTF material names as slots in first-seen order.
pub fn weld_primitives(prims: &[GlbMesh]) -> Result<EditMesh, EditError> {
    let mut total: usize = 0;
    for p in prims {
        total += p.indices.len() / 3;
    }
    if total == 0 {
        return Err(bad("the mesh has no triangles"));
    }
    if prims.iter().map(|p| p.positions.len()).sum::<usize>() > super::MAX_VERTICES {
        return Err(bad("the mesh is larger than the mesh budget"));
    }
    let mut slots: Vec<String> = Vec::new();
    let mut slot_of: BTreeMap<String, u16> = BTreeMap::new();
    for p in prims {
        if !slot_of.contains_key(&p.material) {
            if slots.len() >= super::MAX_MATERIALS {
                return Err(bad("the mesh names more material slots than the budget"));
            }
            slot_of.insert(p.material.clone(), slots.len() as u16);
            slots.push(p.material.clone());
        }
    }
    let cell = super::WELD_EPS;
    let mut grid: BTreeMap<[i64; 3], Vec<(u32, usize)>> = BTreeMap::new();
    let mut positions: Vec<DVec3> = Vec::new();
    let mut vert_of: Vec<Vec<u32>> = Vec::with_capacity(prims.len());
    for (pi, p) in prims.iter().enumerate() {
        let mut table = vec![u32::MAX; p.positions.len()];
        vert_of.push(Vec::new());
        for (vi, pos) in p.positions.iter().enumerate() {
            let d = DVec3::new(pos[0] as f64, pos[1] as f64, pos[2] as f64);
            if !(super::finite_in_range(d.x) && super::finite_in_range(d.y) && super::finite_in_range(d.z)) {
                return Err(bad("a glb position is not finite or is out of range"));
            }
            let key = [(d.x / cell).floor() as i64, (d.y / cell).floor() as i64, (d.z / cell).floor() as i64];
            let mut found = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(cell_vs) = grid.get(&[key[0] + dx, key[1] + dy, key[2] + dz]) {
                            for (v, _) in cell_vs {
                                if (positions[*v as usize] - d).length() <= super::WELD_EPS {
                                    found = Some(*v);
                                    break;
                                }
                            }
                        }
                        if found.is_some() {
                            break;
                        }
                    }
                    if found.is_some() {
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            let v = match found {
                Some(v) => v,
                None => {
                    let v = positions.len() as u32;
                    if positions.len() >= super::MAX_VERTICES {
                        return Err(bad("the welded mesh passes the vertex budget"));
                    }
                    positions.push(d);
                    grid.entry(key).or_default().push((v, pi));
                    v
                }
            };
            table[vi] = v;
        }
        vert_of[pi] = table;
    }
    let mut mesh = EditMesh {
        positions,
        faces: Vec::new(),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: slots,
        smooth_angle: None,
    };
    for (pi, p) in prims.iter().enumerate() {
        let slot = slot_of[&p.material];
        let has_n = p.normals.len() == p.positions.len();
        let has_uv = p.uvs.len() == p.positions.len();
        for t in p.indices.chunks_exact(3) {
            let vs = [t[0] as usize, t[1] as usize, t[2] as usize];
            if vs.iter().any(|v| *v >= p.positions.len()) {
                return Err(bad("a glb index addresses no vertex"));
            }
            // Corners that weld together (a sphere's pole quads) leave a
            // triangle with no area and a repeated vertex: not a face.
            let (a, b, c) = (vert_of[pi][vs[0]], vert_of[pi][vs[1]], vert_of[pi][vs[2]]);
            if a == b || b == c || c == a {
                continue;
            }
            let first = mesh.corners.len() as u32;
            for v in &vs {
                mesh.corners.push(super::Corner {
                    vertex: vert_of[pi][*v],
                    uv: if has_uv { p.uvs[*v] } else { [0.0, 0.0] },
                    normal: if has_n { Some(p.normals[*v]) } else { None },
                });
            }
            mesh.faces.push(Face { first, len: 3, material: slot, flags: FaceFlags::empty() });
        }
    }
    if mesh.faces.is_empty() {
        return Err(bad("every triangle of the mesh collapses to a point or a line"));
    }
    mark_smooth_groups(&mut mesh);
    Ok(mesh)
}

/// Faces whose authored normals deviate from the geometric normal are smooth;
/// edges across disagreeing faces are hard.
fn mark_smooth_groups(mesh: &mut EditMesh) {
    for fi in 0..mesh.faces.len() {
        let n = mesh.face_normal(fi);
        if n == DVec3::ZERO {
            continue;
        }
        let smooth = mesh.face_corners(fi).iter().any(|c| match c.normal {
            Some(nm) => {
                let cn = DVec3::new(nm[0] as f64, nm[1] as f64, nm[2] as f64).normalize_or_zero();
                cn != DVec3::ZERO && cn.dot(n) < 1.0 - 1e-3
            }
            None => false,
        });
        mesh.faces[fi].flags.set(FaceFlags::SMOOTH, smooth);
    }
    let adj = Adjacency::build(mesh);
    let normals: Vec<DVec3> = (0..mesh.faces.len()).map(|f| mesh.face_normal(f)).collect();
    for e in &adj.edges {
        if e.faces.len() != 2 {
            continue;
        }
        let (fa, fb) = (e.faces[0] as usize, e.faces[1] as usize);
        let a_smooth = mesh.faces[fa].flags.contains(FaceFlags::SMOOTH);
        let b_smooth = mesh.faces[fb].flags.contains(FaceFlags::SMOOTH);
        let crease = normals[fa].dot(normals[fb]) < 0.866;
        if a_smooth != b_smooth || crease {
            mesh.set_edge_flag(e.a, e.b, super::EdgeFlags::SHARP, true);
        }
    }
}

fn mesh_of(doc: &Document, id: Id) -> Result<(crate::model::Object, Mesh), EditError> {
    let object = doc.object(id).ok_or(EditError::UnknownObject(id))?.clone();
    match object.kind.clone() {
        ObjectKind::Mesh(m) => Ok((object, m)),
        _ => Err(EditError::NotAMesh(id)),
    }
}

fn read_file(doc: &Document, path: &str) -> Result<Vec<u8>, EditError> {
    doc.files.read(path).map_err(|e| EditError::File(path.to_string(), e.to_string()))
}

/// Make Editable: glb -> `.r5mesh`, repointing `Mesh.source`.
pub fn cmd_make_editable(doc: &Document, id: Id) -> Result<Command, EditError> {
    let (mut object, mut mesh) = mesh_of(doc, id)?;
    let bytes = read_file(doc, &mesh.source)?;
    let editable = match load_triangles(&bytes, &mesh.source)
        .map_err(|e| EditError::File(mesh.source.clone(), e.to_string()))?
    {
        LoadedMesh::Editable(_) => return Err(bad("the mesh is already editable")),
        LoadedMesh::Soup(prims) => weld_primitives(&prims)?,
    };
    let path = format!("meshes/{}.r5mesh", object.id);
    let file_bytes = super::io::write(&editable);
    mesh.source = path.clone();
    object.kind = ObjectKind::Mesh(mesh);
    Ok(Command::Batch {
        label: "make editable".into(),
        commands: vec![
            Command::SetFile { path, bytes: Some(file_bytes) },
            Command::ReplaceObjects { objects: vec![object] },
        ],
    })
}

/// Brush -> Mesh: each brush face becomes one n-gon, its `UvMapping` projected
/// into corner UVs, `lightmap_scale` carried to the mesh.
pub fn cmd_brush_to_mesh(doc: &Document, id: Id) -> Result<Command, EditError> {
    let object = doc.object(id).ok_or(EditError::UnknownObject(id))?.clone();
    let ObjectKind::Brush(brush) = object.kind.clone() else {
        return Err(bad(format!("object {id} is not a brush")));
    };
    let solid = Solid::from_faces(&brush.faces).map_err(|e| bad(e.to_string()))?;
    let mut slots: Vec<String> = Vec::new();
    let mut slot_of: BTreeMap<String, u16> = BTreeMap::new();
    let mut mesh = EditMesh {
        positions: solid.vertices.clone(),
        faces: Vec::with_capacity(solid.loops.len()),
        corners: Vec::new(),
        edge_flags: Vec::new(),
        materials: Vec::new(),
        smooth_angle: None,
    };
    for (fi, brush_face) in brush.faces.iter().enumerate() {
        let solid_face = solid.loops.get(fi).ok_or_else(|| bad("the brush has no geometry"))?;
        if !slot_of.contains_key(&brush_face.material) {
            slot_of.insert(brush_face.material.clone(), slots.len() as u16);
            slots.push(brush_face.material.clone());
        }
        let first = mesh.corners.len() as u32;
        for v in solid_face {
            let p = solid.vertices[*v as usize];
            let uv = brush::tile_uv(brush::project_uv(&brush_face.uv, brush_face.normal, p));
            mesh.corners.push(super::Corner { vertex: *v, uv: [uv[0] as f32, uv[1] as f32], normal: None });
        }
        mesh.faces.push(Face {
            first,
            len: solid_face.len() as u32,
            material: slot_of[&brush_face.material],
            flags: FaceFlags::empty(),
        });
    }
    mesh.materials = slots;
    let path = format!("meshes/{}.r5mesh", object.id);
    let file_bytes = super::io::write(&mesh);
    let mut material_map: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for name in &mesh.materials {
        material_map.insert(name.clone(), name.clone());
    }
    let mut replaced = object.clone();
    replaced.kind = ObjectKind::Mesh(Mesh {
        source: path.clone(),
        material_map,
        collision: CollisionMode::Triangles,
        contents: brush.contents,
        cast_shadows: true,
        lightmap_scale: brush.faces.first().map(|f| f.lightmap_scale).unwrap_or(0.0),
        render: true,
        role: crate::model::MeshRole::Normal,
        modifiers: Vec::new(),
    });
    Ok(Command::Batch {
        label: "brush to mesh".into(),
        commands: vec![
            Command::SetFile { path, bytes: Some(file_bytes) },
            Command::ReplaceObjects { objects: vec![replaced] },
        ],
    })
}

/// Builds a welded `.r5mesh` for a `MeshSpec`, with shared vertices and hard
/// edges marked SHARP, so a new primitive is editable from birth.
pub fn editable_from_spec(
    spec: &crate::meshgen::MeshSpec,
    name: &str,
    material: &str,
    uv_scale: f64,
) -> Result<EditMesh, EditError> {
    let glb = crate::meshgen::build(spec, name, material, uv_scale).map_err(|e| bad(e.to_string()))?;
    let mut mesh = weld_primitives(&[GlbMesh { colors: Vec::new(),
        name: glb.name,
        material: glb.material,
        positions: glb.positions,
        normals: glb.normals,
        uvs: glb.uvs,
        indices: glb.indices,
    }])?;
    // The generator emits triangles; coplanar ones merge back into the quads
    // and n-gon caps a modeller expects, so loop cut, rings, inset and bevel
    // have quad topology to walk. A sphere's pole fans stay triangles.
    let adj = Adjacency::build(&mesh);
    let all = super::select::Selection { mode: super::select::SelMode::Edge, ids: (0..adj.edges.len() as u32).collect() };
    super::ops::subdiv::dissolve(&mut mesh, &all, &serde_json::json!({"mode": "limited", "angle": PRIMITIVE_MERGE_DEG}))?;
    Ok(mesh)
}

/// Neighbouring primitive triangles within this angle share a face.
const PRIMITIVE_MERGE_DEG: f64 = 1.0;

impl From<DocError> for EditError {
    fn from(e: DocError) -> Self {
        bad(e.to_string())
    }
}

/// The command that writes `meshes/<id>.r5mesh` and adds a `Mesh` object for it.
pub fn cmd_add_editable_mesh(
    layer: Id,
    parent: Option<Id>,
    req: &crate::meshgen::MeshRequest,
) -> Result<Command, EditError> {
    let label = req.name.clone().unwrap_or_else(|| crate::meshgen::shape_name(&req.spec).to_string());
    let mesh = editable_from_spec(&req.spec, &label, &req.material, req.uv_scale)?;
    let bytes = super::io::write(&mesh);
    let mut material_map = indexmap::IndexMap::new();
    material_map.insert(req.material.clone(), req.material.clone());
    let kind = ObjectKind::Mesh(Mesh {
        source: String::new(),
        material_map,
        collision: req.collision,
        contents: 1,
        cast_shadows: true,
        lightmap_scale: 0.0,
        render: true,
        role: crate::model::MeshRole::Normal,
        modifiers: Vec::new(),
    });
    let mut object = new_object(label, layer, kind);
    let path = format!("meshes/{}.r5mesh", object.id);
    if let ObjectKind::Mesh(m) = &mut object.kind {
        m.source = path.clone();
    }
    object.parent = parent;
    if let Some(t) = req.transform {
        object.transform = t;
    }
    Ok(Command::Batch {
        label: format!("mesh {}", crate::meshgen::shape_name(&req.spec)),
        commands: vec![Command::SetFile { path, bytes: Some(bytes) }, Command::AddObjects { objects: vec![object] }],
    })
}
