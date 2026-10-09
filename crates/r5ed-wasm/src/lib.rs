//! Browser core: implements `web/src/core/api.ts` over rdoc and rterrain.

use std::collections::BTreeMap;

use glam::{DMat3, DMat4, DVec3};
use js_sys::{Array, Float32Array, Object as JsObject, Reflect, Uint32Array, Uint8Array};
use rcomp::{CompileOptions, MapArtifacts, ProjectFs, Targets};
use rdoc::brush::{self, Keep, Plane, Solid};
use rdoc::schema::EntitySchema;
use rdoc::{Command, Document, Id, MapDoc, MemoryFileStore, Object, ObjectKind};
use rir::AssetTables;
use rterrain::{Heightfield, Recipe, SplatMaps, Stroke};
use wasm_bindgen::prelude::*;

const SCHEMA_JSON: &str = include_str!("../../../schema/entities.json");

fn err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

fn parse_id(s: &str) -> Result<Id, JsValue> {
    uuid::Uuid::parse_str(s).map_err(err)
}

fn v3(a: &[f64]) -> Result<DVec3, JsValue> {
    if a.len() != 3 || a.iter().any(|x| !x.is_finite()) {
        return Err(err("expected three finite numbers"));
    }
    Ok(DVec3::new(a[0], a[1], a[2]))
}

fn ids_to_js(ids: &[Id]) -> Array {
    ids.iter().map(|i| JsValue::from_str(&i.to_string())).collect()
}

fn set(o: &JsObject, k: &str, v: &JsValue) {
    let _ = Reflect::set(o, &JsValue::from_str(k), v);
}

/// World-space triangles for one draw.
#[derive(Default)]
struct Mesh {
    positions: Vec<f32>,
    normals: Vec<f32>,
    uvs: Vec<f32>,
    indices: Vec<u32>,
    ranges: Vec<(String, u32, u32)>,
}

impl Mesh {
    fn begin(&mut self, material: &str) {
        let start = self.indices.len() as u32;
        match self.ranges.last_mut() {
            Some(r) if r.0 == material => {}
            _ => self.ranges.push((material.to_string(), start, 0)),
        }
    }

    fn close(&mut self) {
        let end = self.indices.len() as u32;
        if let Some(r) = self.ranges.last_mut() {
            r.2 = end - r.1;
        }
    }

    fn to_js(&self) -> JsValue {
        let o = JsObject::new();
        set(&o, "positions", &Float32Array::from(self.positions.as_slice()).into());
        set(&o, "normals", &Float32Array::from(self.normals.as_slice()).into());
        set(&o, "uvs", &Float32Array::from(self.uvs.as_slice()).into());
        set(&o, "indices", &Uint32Array::from(self.indices.as_slice()).into());
        let ranges = Array::new();
        for (m, s, c) in &self.ranges {
            let r = JsObject::new();
            set(&r, "material", &JsValue::from_str(m));
            set(&r, "start", &JsValue::from_f64(*s as f64));
            set(&r, "count", &JsValue::from_f64(*c as f64));
            ranges.push(&r);
        }
        set(&o, "ranges", &ranges);
        o.into()
    }

    fn triangles(&self) -> impl Iterator<Item = [DVec3; 3]> + '_ {
        let p = |i: u32| {
            let i = i as usize * 3;
            DVec3::new(self.positions[i] as f64, self.positions[i + 1] as f64, self.positions[i + 2] as f64)
        };
        self.indices.chunks_exact(3).map(move |t| [p(t[0]), p(t[1]), p(t[2])])
    }
}

fn add_brush(mesh: &mut Mesh, b: &rdoc::Brush, world: DMat4) {
    let Ok(solid) = Solid::from_faces(&b.faces) else { return };
    let m3 = DMat3::from_mat4(world);
    let t = brush::triangulate(&solid, &b.faces, m3);
    let offset = world.w_axis.truncate();
    let material = b.faces.first().map(|f| f.material.as_str()).unwrap_or("");
    mesh.begin(material);
    let base = (mesh.positions.len() / 3) as u32;
    for p in &t.positions {
        mesh.positions.extend([p[0] + offset.x as f32, p[1] + offset.y as f32, p[2] + offset.z as f32]);
    }
    for n in &t.normals {
        mesh.normals.extend(n);
    }
    for uv in &t.uvs {
        mesh.uvs.extend(uv);
    }
    mesh.indices.extend(t.indices.iter().map(|i| i + base));
    mesh.close();
}

fn ray_tri(o: DVec3, d: DVec3, t: &[DVec3; 3]) -> Option<(f64, DVec3)> {
    let e1 = t[1] - t[0];
    let e2 = t[2] - t[0];
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let dist = e2.dot(q) * inv;
    (dist > 0.0).then(|| (dist, e1.cross(e2).normalize_or_zero()))
}

fn ray_box(o: DVec3, d: DVec3, lo: DVec3, hi: DVec3) -> Option<f64> {
    let inv = DVec3::ONE / d;
    let t0 = (lo - o) * inv;
    let t1 = (hi - o) * inv;
    let tmin = t0.min(t1).max_element();
    let tmax = t0.max(t1).min_element();
    (tmax >= tmin.max(0.0)).then_some(tmin.max(0.0))
}

#[wasm_bindgen]
pub struct Core {
    doc: Document,
    files: BTreeMap<String, Vec<u8>>,
    schema: EntitySchema,
    session: Option<rdoc::editmesh::Session>,
    stack_cache: rdoc::editmesh::StackCache,
}

#[wasm_bindgen]
impl Core {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<Core, JsValue> {
        let schema = EntitySchema::load(SCHEMA_JSON).map_err(err)?;
        Ok(Core { doc: Document::new(MapDoc::default(), Box::new(MemoryFileStore::new())), files: BTreeMap::new(), schema, session: None, stack_cache: rdoc::editmesh::StackCache::new() })
    }

    pub fn load(&mut self, doc_json: &str) -> Result<String, JsValue> {
        let doc: MapDoc = serde_json::from_str(doc_json).map_err(err)?;
        rdoc::doc::validate(&doc).map_err(err)?;
        let mut store = MemoryFileStore::new();
        for (p, b) in &self.files {
            rdoc::FileStore::write(&mut store, p, b).map_err(err)?;
        }
        self.doc = Document::new(doc, Box::new(store));
        self.session = None;
        self.save()
    }

    pub fn save(&self) -> Result<String, JsValue> {
        serde_json::to_string_pretty(&self.doc.doc).map_err(err)
    }

    pub fn apply(&mut self, command_json: &str) -> Result<Array, JsValue> {
        let cmd: Command = serde_json::from_str(command_json).map_err(err)?;
        self.mirror_files(&cmd);
        let (_, ids) = self.doc.apply(cmd).map_err(err)?;
        self.rebase_session();
        Ok(ids_to_js(&ids))
    }

    pub fn undo(&mut self) -> Result<Array, JsValue> {
        let cmd = self.doc.undo().map_err(err)?;
        self.mirror_files(&cmd);
        self.rebase_session();
        Ok(ids_to_js(&touched(&cmd)))
    }

    pub fn redo(&mut self) -> Result<Array, JsValue> {
        let cmd = self.doc.redo().map_err(err)?;
        self.mirror_files(&cmd);
        self.rebase_session();
        Ok(ids_to_js(&touched(&cmd)))
    }

    #[wasm_bindgen(js_name = canUndo)]
    pub fn can_undo(&self) -> bool {
        self.doc.can_undo()
    }

    #[wasm_bindgen(js_name = canRedo)]
    pub fn can_redo(&self) -> bool {
        self.doc.can_redo()
    }

    pub fn object(&self, id: &str) -> Result<Option<String>, JsValue> {
        let id = parse_id(id)?;
        self.doc.object(id).map(|o| serde_json::to_string(o).map_err(err)).transpose()
    }

    #[wasm_bindgen(js_name = objectIds)]
    pub fn object_ids(&self) -> Array {
        ids_to_js(&self.doc.object_ids())
    }

    #[wasm_bindgen(js_name = objectMesh)]
    pub fn object_mesh(&mut self, id: &str) -> Result<JsValue, JsValue> {
        let id = parse_id(id)?;
        Ok(self.mesh_of(id).map(|m| m.to_js()).unwrap_or(JsValue::NULL))
    }

    /// Wireframe edges of one object in object space: brush face outlines,
    /// polygon edges of an editable mesh (through its modifier stack), and
    /// coplanar-suppressed triangle edges for anything else. Null when the
    /// object has no line drawing.
    #[wasm_bindgen(js_name = objectEdges)]
    pub fn object_edges(&mut self, id: &str) -> Result<JsValue, JsValue> {
        let id = parse_id(id)?;
        Ok(self.edges_of(id).map(|w| wire_to_js(&w)).unwrap_or(JsValue::NULL))
    }

    /// Triangle soup to wireframe edges: welded positions plus edge pairs
    /// with coplanar diagonals removed. Null for an empty soup.
    #[wasm_bindgen(js_name = soupEdges)]
    pub fn soup_edges(&self, positions: Vec<f32>, indices: Vec<u32>) -> Result<JsValue, JsValue> {
        if !positions.len().is_multiple_of(3) {
            return Err(err("positions are not xyz triples"));
        }
        if !indices.len().is_multiple_of(3) {
            return Err(err("indices are not triangles"));
        }
        if positions.len() / 3 > rdoc::editmesh::MAX_VERTICES || indices.len() / 3 > rdoc::editmesh::MAX_FACES {
            return Err(err("the mesh is larger than the mesh budget"));
        }
        let packed: Vec<[f32; 3]> =
            positions.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        let wire = rdoc::editmesh::soup_wire_edges(&packed, &indices).map_err(err)?;
        if wire.edges.is_empty() {
            return Ok(JsValue::NULL);
        }
        Ok(wire_to_js(&wire))
    }

    pub fn pick(&mut self, origin: Vec<f64>, dir: Vec<f64>, max_dist: f64) -> Result<JsValue, JsValue> {
        let o = v3(&origin)?;
        let d = v3(&dir)?.normalize_or_zero();
        if d == DVec3::ZERO {
            return Err(err("zero pick direction"));
        }
        let mut best: Option<(f64, Id, DVec3)> = None;
        for id in self.doc.object_ids() {
            let hit = if let Some(mesh) = self.mesh_of(id) {
                mesh.triangles().filter_map(|t| ray_tri(o, d, &t)).min_by(|a, b| a.0.total_cmp(&b.0))
            } else if let Some(b) = self.doc.object_bounds(id) {
                ray_box(o, d, b.min, b.max).map(|t| (t, -d))
            } else {
                None
            };
            if let Some((t, n)) = hit {
                if t <= max_dist && best.as_ref().is_none_or(|b| t < b.0) {
                    best = Some((t, id, n));
                }
            }
        }
        let Some((t, id, n)) = best else { return Ok(JsValue::NULL) };
        let p = o + d * t;
        let r = JsObject::new();
        set(&r, "id", &JsValue::from_str(&id.to_string()));
        set(&r, "position", &Array::of3(&p.x.into(), &p.y.into(), &p.z.into()));
        set(&r, "normal", &Array::of3(&n.x.into(), &n.y.into(), &n.z.into()));
        set(&r, "distance", &JsValue::from_f64(t));
        Ok(r.into())
    }

    #[wasm_bindgen(js_name = brushBox)]
    pub fn brush_box(&self, min: Vec<f64>, max: Vec<f64>, material: &str) -> Result<String, JsValue> {
        let layer = self.doc.layer_ids().first().copied().ok_or_else(|| err("document has no layer"))?;
        to_json(brush::cmd_box(layer, None, v3(&min)?, v3(&max)?, material).map_err(err)?)
    }

    #[wasm_bindgen(js_name = brushClip)]
    pub fn brush_clip(&self, ids: Vec<String>, normal: Vec<f64>, dist: f64, keep: &str) -> Result<String, JsValue> {
        let ids = ids.iter().map(|s| parse_id(s)).collect::<Result<Vec<_>, _>>()?;
        let keep = match keep {
            "front" => Keep::Front,
            "back" => Keep::Back,
            "both" => Keep::Both,
            other => return Err(err(format!("unknown clip side {other}"))),
        };
        let plane = Plane::new(v3(&normal)?, dist).map_err(err)?;
        to_json(brush::cmd_clip(&self.doc, &ids, plane, keep).map_err(err)?)
    }

    #[wasm_bindgen(js_name = brushHollow)]
    pub fn brush_hollow(&self, id: &str, thickness: f64) -> Result<String, JsValue> {
        to_json(brush::cmd_hollow(&self.doc, parse_id(id)?, thickness).map_err(err)?)
    }

    #[wasm_bindgen(js_name = brushCarve)]
    pub fn brush_carve(&self, carver: &str, targets: Vec<String>) -> Result<String, JsValue> {
        let t = targets.iter().map(|s| parse_id(s)).collect::<Result<Vec<_>, _>>()?;
        to_json(brush::cmd_carve(&self.doc, parse_id(carver)?, &t).map_err(err)?)
    }

    #[wasm_bindgen(js_name = brushMergeConvex)]
    pub fn brush_merge_convex(&self, ids: Vec<String>) -> Result<String, JsValue> {
        let ids = ids.iter().map(|s| parse_id(s)).collect::<Result<Vec<_>, _>>()?;
        to_json(brush::cmd_merge_convex(&self.doc, &ids).map_err(err)?)
    }

    #[wasm_bindgen(js_name = brushExtrudeFace)]
    pub fn brush_extrude_face(&self, id: &str, face: u32, distance: f64) -> Result<String, JsValue> {
        to_json(brush::cmd_extrude_face(&self.doc, parse_id(id)?, face as usize, distance).map_err(err)?)
    }

    /// Resizes a brush so its bounds become `size`, keeping the min corner.
    #[wasm_bindgen(js_name = brushResize)]
    pub fn brush_resize(&self, id: &str, size: Vec<f64>) -> Result<String, JsValue> {
        to_json(brush::cmd_resize(&self.doc, parse_id(id)?, v3(&size)?).map_err(err)?)
    }

    /// Object-space `[minx, miny, minz, maxx, maxy, maxz]` of a brush.
    #[wasm_bindgen(js_name = brushBounds)]
    pub fn brush_bounds(&self, id: &str) -> Result<Vec<f64>, JsValue> {
        let o = self.doc.object(parse_id(id)?).ok_or_else(|| err("unknown object"))?;
        let ObjectKind::Brush(b) = &o.kind else { return Err(err("object is not a brush")) };
        let (lo, hi) = brush::brush_bounds(b).map_err(err)?;
        Ok(vec![lo.x, lo.y, lo.z, hi.x, hi.y, hi.z])
    }

    #[wasm_bindgen(js_name = brushMoveVertex)]
    pub fn brush_move_vertex(&self, id: &str, vertex: Vec<f64>, to: Vec<f64>) -> Result<String, JsValue> {
        to_json(brush::cmd_move_vertex(&self.doc, parse_id(id)?, v3(&vertex)?, v3(&to)?).map_err(err)?)
    }

    /// Adds an authored mesh; `request_json` is an `rdoc::meshgen::MeshRequest`.
    #[wasm_bindgen(js_name = meshCreate)]
    pub fn mesh_create(&self, request_json: &str) -> Result<String, JsValue> {
        let req: rdoc::meshgen::MeshRequest = serde_json::from_str(request_json).map_err(err)?;
        let layer = self.doc.layer_ids().first().copied().ok_or_else(|| err("document has no layer"))?;
        to_json(rdoc::meshgen::cmd_add_mesh(layer, None, &req).map_err(err)?)
    }

    /// Runs one mesh op request (`{op, object, sel, params}`); returns the op
    /// result JSON (`{command, selection, report}`) to apply.
    #[wasm_bindgen(js_name = meshOp)]
    pub fn mesh_op(&mut self, request_json: &str) -> Result<String, JsValue> {
        self.sync_files();
        let req: rdoc::editmesh::OpRequest = serde_json::from_str(request_json).map_err(err)?;
        let out = rdoc::editmesh::plan(&self.doc, &req).map_err(err)?;
        serde_json::to_string(&serde_json::json!({
            "command": out.command,
            "selection": out.selection,
            "report": out.report,
        }))
        .map_err(err)
    }

    /// Makes a glb-sourced mesh editable; returns the Command JSON to apply.
    #[wasm_bindgen(js_name = meshMakeEditable)]
    pub fn mesh_make_editable(&mut self, id: &str) -> Result<String, JsValue> {
        self.sync_files();
        to_json(rdoc::editmesh::convert::cmd_make_editable(&self.doc, parse_id(id)?).map_err(err)?)
    }

    /// Converts a brush to an editable mesh; returns the Command JSON to apply.
    #[wasm_bindgen(js_name = brushToMesh)]
    pub fn brush_to_mesh(&mut self, id: &str) -> Result<String, JsValue> {
        self.sync_files();
        to_json(rdoc::editmesh::convert::cmd_brush_to_mesh(&self.doc, parse_id(id)?).map_err(err)?)
    }

    /// Loads the mesh into an edit session; returns its element counts.
    #[wasm_bindgen(js_name = editBegin)]
    pub fn edit_begin(&mut self, id: &str) -> Result<String, JsValue> {
        self.sync_files();
        let object = parse_id(id)?;
        let session = rdoc::editmesh::Session::begin(&self.doc, object).map_err(err)?;
        let counts = session.counts();
        self.session = Some(session);
        serde_json::to_string(&counts).map_err(err)
    }

    /// Leaves edit mode, committing nothing new.
    #[wasm_bindgen(js_name = editEnd)]
    pub fn edit_end(&mut self) {
        self.session = None;
    }

    /// Replaces the session selection (`{mode, ids?, query?}`); returns it.
    #[wasm_bindgen(js_name = editSelect)]
    pub fn edit_select(&mut self, sel_json: &str) -> Result<String, JsValue> {
        let sel: rdoc::editmesh::select::Sel = serde_json::from_str(sel_json).map_err(err)?;
        let session = self.session.as_mut().ok_or_else(|| err("no edit session"))?;
        let out = session.select(&sel).map_err(err)?;
        serde_json::to_string(&out).map_err(err)
    }

    /// Draw buffers over the session mesh (positions, triangles, polygon
    /// edges, selection masks), in world space.
    #[wasm_bindgen(js_name = editBuffers)]
    pub fn edit_buffers(&self) -> Result<JsValue, JsValue> {
        let session = self.session.as_ref().ok_or_else(|| err("no edit session"))?;
        let b = session.buffers();
        let o = JsObject::new();
        let flat3 = |v: &[[f32; 3]]| v.iter().flat_map(|p| *p).collect::<Vec<f32>>();
        let flat2 = |v: &[[f32; 2]]| v.iter().flat_map(|p| *p).collect::<Vec<f32>>();
        let flat_pairs = |v: &[[u32; 2]]| v.iter().flat_map(|p| *p).collect::<Vec<u32>>();
        set(&o, "verts", &Float32Array::from(flat3(&b.verts).as_slice()).into());
        set(&o, "positions", &Float32Array::from(flat3(&b.positions).as_slice()).into());
        set(&o, "normals", &Float32Array::from(flat3(&b.normals).as_slice()).into());
        set(&o, "uvs", &Float32Array::from(flat2(&b.uvs).as_slice()).into());
        set(&o, "tris", &Uint32Array::from(b.tris.as_slice()).into());
        set(&o, "polyEdges", &Uint32Array::from(flat_pairs(&b.poly_edges).as_slice()).into());
        set(&o, "faceCentres", &Float32Array::from(flat3(&b.face_centres).as_slice()).into());
        set(&o, "vertSel", &Uint8Array::from(b.vert_sel.as_slice()).into());
        set(&o, "edgeSel", &Uint8Array::from(b.edge_sel.as_slice()).into());
        set(&o, "faceSel", &Uint8Array::from(b.face_sel.as_slice()).into());
        set(&o, "edgeFlags", &Uint8Array::from(b.edge_flags.as_slice()).into());
        let counts = Array::of3(&(b.counts[0] as f64).into(), &(b.counts[1] as f64).into(), &(b.counts[2] as f64).into());
        set(&o, "counts", &counts.into());
        set(&o, "topoHash", &JsValue::from_str(&b.topo_hash.to_string()));
        Ok(o.into())
    }

    /// CPU pick against the session mesh; `query_json` is a
    /// `rdoc::editmesh::PickQuery`. Returns the `PickReply` JSON.
    #[wasm_bindgen(js_name = editPick)]
    pub fn edit_pick(&self, query_json: &str) -> Result<String, JsValue> {
        let query: rdoc::editmesh::PickQuery = serde_json::from_str(query_json).map_err(err)?;
        let session = self.session.as_ref().ok_or_else(|| err("no edit session"))?;
        let reply = session.pick(&query).map_err(err)?;
        serde_json::to_string(&reply).map_err(err)
    }

    /// Mutates the session for a drag without producing a command; returns
    /// `{selection, report}`. The release calls `editCommit`.
    #[wasm_bindgen(js_name = editPreview)]
    pub fn edit_preview(&mut self, request_json: &str) -> Result<String, JsValue> {
        let req: rdoc::editmesh::OpRequest = serde_json::from_str(request_json).map_err(err)?;
        let session = self.session.as_mut().ok_or_else(|| err("no edit session"))?;
        let out = session.preview(&self.doc, &req).map_err(err)?;
        serde_json::to_string(&serde_json::json!({ "selection": out.selection, "report": out.report })).map_err(err)
    }

    /// The one `SetFile` for the `.r5mesh` a drag release applies.
    #[wasm_bindgen(js_name = editCommit)]
    pub fn edit_commit(&self) -> Result<String, JsValue> {
        let session = self.session.as_ref().ok_or_else(|| err("no edit session"))?;
        to_json(session.commit())
    }

    /// Hides the selected elements until revealed; session view state only.
    #[wasm_bindgen(js_name = editHide)]
    pub fn edit_hide(&mut self) -> Result<(), JsValue> {
        let session = self.session.as_mut().ok_or_else(|| err("no edit session"))?;
        session.hide();
        Ok(())
    }

    /// Reveals every hidden element.
    #[wasm_bindgen(js_name = editReveal)]
    pub fn edit_reveal(&mut self) -> Result<(), JsValue> {
        let session = self.session.as_mut().ok_or_else(|| err("no edit session"))?;
        session.reveal();
        Ok(())
    }

    /// Ring cut polylines for the loop-cut hover preview: one polyline per cut
    /// across the ring through `edge`, in walk order (closed for closed rings).
    #[wasm_bindgen(js_name = editRingPreview)]
    pub fn edit_ring_preview(&mut self, id: &str, edge: u32, cuts: u32) -> Result<String, JsValue> {
        self.sync_files();
        let o = self.doc.object(parse_id(id)?).ok_or_else(|| err("unknown object"))?;
        let ObjectKind::Mesh(m) = &o.kind else { return Err(err("object is not a mesh")) };
        let bytes = self.mesh_bytes(&m.source).ok_or_else(|| err("mesh file is missing"))?;
        let editable = rdoc::editmesh::io::read(&bytes).map_err(err)?;
        let rows = rdoc::editmesh::ops::loopcut::ring_preview(&editable, edge, cuts).map_err(err)?;
        serde_json::to_string(&rows).map_err(err)
    }

    #[wasm_bindgen(js_name = terrainGenerate)]
    pub fn terrain_generate(&self, recipe_json: &str) -> Result<Float32Array, JsValue> {
        #[derive(serde::Deserialize)]
        struct Req {
            recipe: serde_json::Value,
            resolution: [u32; 2],
            cell_size: f32,
        }
        let req: Req = serde_json::from_str(recipe_json).map_err(err)?;
        let recipe = Recipe::from_json(&req.recipe.to_string()).map_err(err)?;
        let hf = recipe.evaluate(req.resolution, req.cell_size).map_err(err)?;
        Ok(Float32Array::from(hf.data.as_slice()))
    }

    #[wasm_bindgen(js_name = terrainSculpt)]
    pub fn terrain_sculpt(&self, id: &str, stroke_json: &str) -> Result<String, JsValue> {
        let (_, t) = self.terrain(parse_id(id)?)?;
        let stroke: Stroke = serde_json::from_str(stroke_json).map_err(err)?;
        let mut hf = self.heights(&t)?;
        rterrain::apply_stroke(&mut hf, &stroke).map_err(err)?;
        to_json(Command::Batch {
            label: "sculpt".into(),
            commands: vec![Command::SetFile { path: t.heights.clone(), bytes: Some(hf.to_le_bytes()) }],
        })
    }

    #[wasm_bindgen(js_name = terrainAutoPaint)]
    pub fn terrain_auto_paint(&self, id: &str, rules_json: &str) -> Result<String, JsValue> {
        let (_, t) = self.terrain(parse_id(id)?)?;
        let rules: Vec<rterrain::LayerRule> = serde_json::from_str(rules_json).map_err(err)?;
        let hf = self.heights(&t)?;
        let maps = rterrain::autopaint(&hf, &rules).map_err(err)?;
        let commands = t
            .layers
            .iter()
            .zip(maps.layers)
            .map(|(layer, bytes)| Command::SetFile { path: layer.weights.clone(), bytes: Some(bytes) })
            .collect();
        to_json(Command::Batch { label: "auto paint".into(), commands })
    }

    #[wasm_bindgen(js_name = terrainScatter)]
    pub fn terrain_scatter(&self, id: &str) -> Result<String, JsValue> {
        let id = parse_id(id)?;
        let (object, t) = self.terrain(id)?;
        let hf = self.heights(&t)?;
        let splat = self.splat(&t)?;
        let yaw = object.transform.rotation.to_euler(glam::EulerRot::ZYX).0.to_degrees();
        let origin = self.doc.world_position(id).unwrap_or(object.transform.position);
        let xf = rterrain::splat::ObjectTransform { origin, yaw_deg: yaw };
        let mut objects = Vec::new();
        for (li, layer) in t.layers.iter().enumerate() {
            for rule in &layer.scatter {
                let f = rterrain::splat::ScatterFilters {
                    min_weight: rule.min_weight,
                    slope_deg: rule.slope_deg,
                    height: rule.height,
                    scale: rule.scale,
                    align_to_normal: rule.align_to_normal,
                    density: rule.density,
                    seed: rule.seed,
                    static_prop: rule.static_prop,
                };
                let placed = rterrain::scatter(&hf, splat.as_ref(), li, &rule.models, &f, &xf).map_err(err)?;
                for p in placed {
                    let prop = rdoc::Prop {
                        model: p.model.clone(),
                        skin: 0,
                        static_prop: p.static_prop,
                        solid: true,
                        cast_shadows: true,
                        fade_distance: 0.0,
                        keyvalues: Default::default(),
                    };
                    let mut o = rdoc::doc::new_object(p.model.rsplit('/').next().unwrap_or("prop"), object.layer, ObjectKind::Prop(prop));
                    o.transform.position = p.position;
                    o.transform.rotation = p.rotation;
                    o.transform.scale = DVec3::splat(p.scale as f64);
                    objects.push(o);
                }
            }
        }
        to_json(Command::Batch { label: "scatter".into(), commands: vec![Command::AddObjects { objects }] })
    }

    #[wasm_bindgen(js_name = putFile)]
    pub fn put_file(&mut self, path: &str, bytes: Vec<u8>) -> Result<(), JsValue> {
        rdoc::project::check_path(path).map_err(err)?;
        self.files.insert(path.to_string(), bytes);
        Ok(())
    }

    #[wasm_bindgen(js_name = getFile)]
    pub fn get_file(&self, path: &str) -> Option<Uint8Array> {
        self.files.get(path).map(|b| Uint8Array::from(b.as_slice()))
    }

    /// Every project file the core holds, so a save can write them all.
    #[wasm_bindgen(js_name = fileNames)]
    pub fn file_names(&self) -> Array {
        self.files.keys().map(|k| JsValue::from_str(k)).collect()
    }

    /// Drops the document, its history and every file, as a new project does.
    pub fn reset(&mut self) -> Result<String, JsValue> {
        self.files.clear();
        self.session = None;
        self.stack_cache = rdoc::editmesh::StackCache::new();
        self.doc = Document::new(MapDoc::default(), Box::new(MemoryFileStore::new()));
        self.save()
    }

    pub fn schema(&self) -> String {
        SCHEMA_JSON.to_string()
    }

    pub fn validate(&self) -> Result<String, JsValue> {
        let mut out = Vec::new();
        if let Err(e) = rdoc::doc::validate(&self.doc.doc) {
            out.push(serde_json::json!({"severity": "error", "id": null, "message": e.to_string()}));
        }
        for o in &self.doc.doc.objects {
            for i in self.schema.validate_object(o) {
                out.push(serde_json::json!({
                    "severity": format!("{:?}", i.severity).to_lowercase(),
                    "id": i.object.map(|x| x.to_string()),
                    "message": i.message,
                }));
            }
        }
        serde_json::to_string(&out).map_err(err)
    }

    pub fn compile(&self, targets_json: &str, asset_info_json: &str) -> JsValue {
        let o = JsObject::new();
        let mut errors: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let stats = JsObject::new();
        let timings = JsObject::new();

        let wanted = match serde_json::from_str::<Vec<String>>(targets_json) {
            Ok(t) => targets_of(&t),
            Err(e) => {
                errors.push(format!("targets: {e}"));
                Targets::default()
            }
        };
        let tables: AssetTables = if asset_info_json.trim().is_empty() {
            AssetTables::default()
        } else {
            match serde_json::from_str(asset_info_json) {
                Ok(t) => t,
                Err(e) => {
                    errors.push(format!("asset tables: {e}"));
                    AssetTables::default()
                }
            }
        };
        let options = CompileOptions { targets: wanted, ..Default::default() };
        let started = js_sys::Date::now();
        match rcomp::compile(&self.doc.doc, &DocFs(self.doc.files.as_ref()), &tables, &options) {
            Ok(mut out) => {
                set(&o, "ok", &JsValue::TRUE);
                for stage in &out.report.stages {
                    set(&timings, &stage.name, &JsValue::from_f64(stage.micros as f64 / 1000.0));
                }
                for (name, value) in stats_of(&out.report.stats) {
                    set(&stats, &name, &JsValue::from_f64(value));
                }
                warnings.append(&mut out.report.warnings);
                warnings.extend(out.report.validation.iter().filter(|v| v.starts_with("Error")).cloned());
                let stem = self.doc.doc.meta.stem.as_str();
                if let Some(a) = &out.client {
                    set(&o, "client", &file_map(target_files(a, stem, ".client")));
                }
                if let Some(a) = &out.dedi {
                    set(&o, "dedi", &file_map(target_files(a, stem, ".server")));
                }
            }
            Err(e) => {
                set(&o, "ok", &JsValue::FALSE);
                errors.push(e.to_string());
            }
        }
        let report = JsObject::new();
        set(&report, "warnings", &string_array(&warnings));
        set(&report, "errors", &string_array(&errors));
        set(&report, "stats", &stats);
        // The compiler's own stage clock has no source on this target, so the
        // wall clock the page does have is what the total is measured with.
        set(&timings, "total", &JsValue::from_f64(js_sys::Date::now() - started));
        set(&report, "timings_ms", &timings);
        set(&o, "report", &report);
        o.into()
    }

    #[wasm_bindgen(js_name = bakePreview)]
    pub fn bake_preview(&self, settings_json: &str) -> Result<JsValue, JsValue> {
        let out = JsObject::new();
        let overrides: serde_json::Value = serde_json::from_str(settings_json).unwrap_or(serde_json::Value::Null);
        let mut doc = self.doc.doc.clone();
        if overrides.is_object() {
            if let Some(b) = overrides.get("bake") {
                if let Ok(b) = serde_json::from_value::<rdoc::model::BakeSettings>(b.clone()) {
                    doc.settings.bake = b;
                }
            }
        }
        // A viewport rebakes on every keystroke, so the preview runs at a texel
        // size a box room stays interactive at whatever the document asked for.
        if doc.settings.bake.texel_size > 32.0 {
            doc.settings.bake.texel_size = 32.0;
        }
        let options = CompileOptions { targets: Targets { client: true, dedi: false }, ..Default::default() };
        let tables = AssetTables::default();
        match rcomp::preview_lightmaps(&doc, &DocFs(self.doc.files.as_ref()), &tables, &options) {
            Ok(previews) => {
                for (id, (w, h, rgba)) in previews {
                    let e = JsObject::new();
                    set(&e, "width", &JsValue::from_f64(f64::from(w)));
                    set(&e, "height", &JsValue::from_f64(f64::from(h)));
                    set(&e, "rgba", &Uint8Array::from(rgba.as_slice()).into());
                    let _ = Reflect::set(&out, &JsValue::from_str(&id.to_string()), &e);
                }
            }
            Err(e) => return Err(err(e)),
        }
        Ok(out.into())
    }

    /// `options_json` is an ObjOptions object (`{"scale": 64, "up": "y", ...}`); returns the Command JSON.
    #[wasm_bindgen(js_name = importObj)]
    pub fn import_obj(&self, text: &str, options_json: &str) -> Result<String, JsValue> {
        let opts: rdoc::import::obj::ObjOptions =
            if options_json.trim().is_empty() { Default::default() } else { serde_json::from_str(options_json).map_err(err)? };
        to_json(rdoc::import::obj::import(&self.doc, text, &opts).map_err(err)?.0)
    }

    #[wasm_bindgen(js_name = importVmf)]
    pub fn import_vmf(&self, text: &str) -> Result<String, JsValue> {
        to_json(rdoc::import::vmf::import(&self.doc, text).map_err(err)?)
    }

    #[wasm_bindgen(js_name = importQuakeMap)]
    pub fn import_quake_map(&self, text: &str) -> Result<String, JsValue> {
        to_json(rdoc::import::quake::import(&self.doc, text).map_err(err)?)
    }

    #[wasm_bindgen(js_name = importReMap)]
    pub fn import_remap(&self, json: &str) -> Result<String, JsValue> {
        to_json(rdoc::import::remap::import(&self.doc, json).map_err(err)?)
    }

    #[wasm_bindgen(js_name = importGltf)]
    pub fn import_gltf(&self, path: &str) -> Result<String, JsValue> {
        if !self.files.contains_key(path) {
            return Err(err(format!("{path} was not stored with putFile")));
        }
        let layer = self.doc.layer_ids().first().copied().ok_or_else(|| err("document has no layer"))?;
        let mesh = rdoc::Mesh {
            source: path.to_string(),
            material_map: Default::default(),
            collision: rdoc::CollisionMode::Triangles,
            contents: 1,
            cast_shadows: true,
            lightmap_scale: 0.0,
            render: true,
            role: rdoc::model::MeshRole::Normal,
            modifiers: Vec::new(),
        };
        let o = rdoc::doc::new_object(path.rsplit('/').next().unwrap_or("mesh"), layer, ObjectKind::Mesh(mesh));
        to_json(Command::Batch {
            label: "import glTF".into(),
            commands: vec![Command::AddObjects { objects: vec![o] }],
        })
    }
}

fn to_json(c: Command) -> Result<String, JsValue> {
    serde_json::to_string(&c).map_err(err)
}

/// `{positions, edges}` wire soup as the page's `objectEdges` reads it.
fn wire_to_js(w: &rdoc::editmesh::WireSoup) -> JsValue {
    let o = JsObject::new();
    let flat: Vec<f32> = w.positions.iter().flat_map(|p| *p).collect();
    let pairs: Vec<u32> = w.edges.iter().flat_map(|e| *e).collect();
    set(&o, "positions", &Float32Array::from(flat.as_slice()).into());
    set(&o, "edges", &Uint32Array::from(pairs.as_slice()).into());
    o.into()
}

/// The project's files, as the compiler asks for them.
struct DocFs<'a>(&'a dyn rdoc::doc::FileStore);

impl ProjectFs for DocFs<'_> {
    fn read(&self, rel: &str) -> anyhow::Result<Vec<u8>> {
        self.0.read(rel).map_err(|e| anyhow::anyhow!("{rel}: {e}"))
    }
}

/// The targets a compile asks for; a name the compiler does not know is not a
/// target and is dropped rather than guessed at.
fn targets_of(names: &[String]) -> Targets {
    Targets {
        client: names.iter().any(|n| n == "client"),
        dedi: names.iter().any(|n| n == "dedi"),
    }
}

/// One target's files, named the way a pak stores them: the header beside its
/// lumps, the lumps tagged for the engine that reads them, and one entity
/// partition file per partition.
fn target_files(artifacts: &MapArtifacts, stem: &str, suffix: &str) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let header = format!("maps/{stem}.bsp");
    match artifacts.bsp.layout {
        rbsp::Layout::Monolithic => {
            out.insert(header, artifacts.bsp.to_monolithic());
        }
        rbsp::Layout::Sidecar => {
            out.insert(header, artifacts.bsp.header.to_bytes());
            for (id, bytes) in &artifacts.bsp.lumps {
                out.insert(format!("maps/{stem}.bsp.{id:04X}.bsp_lump{suffix}"), bytes.clone());
            }
        }
    }
    for (name, bytes) in &artifacts.ent_bytes {
        out.insert(format!("maps/{stem}_{name}.ent"), bytes.clone());
    }
    out
}

fn file_map(files: BTreeMap<String, Vec<u8>>) -> JsValue {
    let o = JsObject::new();
    for (name, bytes) in files {
        set(&o, &name, &Uint8Array::from(bytes.as_slice()).into());
    }
    o.into()
}

fn string_array(values: &[String]) -> Array {
    values.iter().map(|v| JsValue::from_str(v)).collect()
}

/// The compile counters, in the shape the report field declares.
fn stats_of(stats: &rcomp::report::Stats) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = Vec::new();
    let mut push = |name: &str, value: usize| out.push((name.to_string(), value as f64));
    push("objects", stats.objects);
    push("brushes", stats.brushes);
    push("faces", stats.faces);
    push("render_meshes", stats.render_meshes);
    push("triangles", stats.triangles);
    push("surfaces", stats.surfaces);
    push("collision_solids", stats.collision_solids);
    push("bvh_nodes", stats.bvh_nodes);
    push("bvh_leaves", stats.bvh_leaves);
    push("models", stats.models);
    push("props", stats.props);
    push("volumes", stats.volumes);
    push("lights", stats.lights);
    push("terrains", stats.terrains);
    push("nav_triangles", stats.nav_triangles);
    push("entities", stats.entities);
    push("partition_files", stats.partition_files);
    push("vis_cells", stats.vis_cells);
    push("vis_objects", stats.vis_objects);
    push("lightmap_pages", stats.lightmap_pages);
    push("lightprobes", stats.lightprobes);
    push("cubemaps", stats.cubemaps);
    push("csm_shadow_meshes", stats.csm_shadow_meshes);
    out
}

fn touched(c: &Command) -> Vec<Id> {
    match c {
        Command::AddObjects { objects } | Command::ReplaceObjects { objects } => objects.iter().map(|o| o.id).collect(),
        Command::RemoveObjects { ids } => ids.clone(),
        Command::Batch { commands, .. } => commands.iter().flat_map(touched).collect(),
        _ => Vec::new(),
    }
}

impl Core {
    fn mirror_files(&mut self, c: &Command) {        match c {
            Command::SetFile { path, bytes: Some(b) } => {
                self.files.insert(path.clone(), b.clone());
            }
            Command::SetFile { path, bytes: None } => {
                self.files.remove(path);
            }
            Command::Batch { commands, .. } => commands.iter().for_each(|c| self.mirror_files(c)),
            _ => {}
        }
    }

    /// Reloads the session mesh after the document changed under it (a commit,
    /// an undo), carrying the selection across. A session whose object is gone
    /// simply ends.
    fn rebase_session(&mut self) {
        let Some(current) = self.session.take() else { return };
        let object = current.object();
        let selection = current.selection().clone();
        if let Ok(mut next) = rdoc::editmesh::Session::begin(&self.doc, object) {
            next.set_selection(selection);
            self.session = Some(next);
        }
    }

    fn terrain(&self, id: Id) -> Result<(Object, rdoc::Terrain), JsValue> {
        let o = self.doc.object(id).ok_or_else(|| err("unknown object"))?;
        match &o.kind {
            ObjectKind::Terrain(t) => Ok((o.clone(), t.clone())),
            _ => Err(err("object is not a terrain")),
        }
    }

    fn heights(&self, t: &rdoc::Terrain) -> Result<Heightfield, JsValue> {
        let b = self.files.get(&t.heights).ok_or_else(|| err(format!("{} not loaded", t.heights)))?;
        Heightfield::from_le_bytes(b, t.resolution[0], t.resolution[1], t.cell_size as f32).map_err(err)
    }

    fn splat(&self, t: &rdoc::Terrain) -> Result<Option<SplatMaps>, JsValue> {
        let mut bytes = Vec::new();
        for l in &t.layers {
            match self.files.get(&l.weights) {
                Some(b) => bytes.extend_from_slice(b),
                None => return Ok(None),
            }
        }
        if t.layers.is_empty() {
            return Ok(None);
        }
        SplatMaps::from_le_bytes(&bytes, t.layers.len(), t.resolution[0], t.resolution[1], t.cell_size as f32)
            .map(Some)
            .map_err(err)
    }

    fn mesh_bytes(&self, path: &str) -> Option<Vec<u8>> {
        if let Some(b) = self.files.get(path) {
            return Some(b.clone());
        }
        self.doc.files.read(path).ok()
    }

    /// Object-space wireframe edges of one object, or `None` when it has no
    /// line drawing. Meshes run through their modifier stack, as the draw does.
    fn edges_of(&mut self, id: Id) -> Option<rdoc::editmesh::WireSoup> {
        let o = self.doc.object(id)?;
        match &o.kind {
            ObjectKind::Brush(b) => rdoc::editmesh::brush_outline(b).ok(),
            ObjectKind::Volume(v) => {
                let mut positions = Vec::new();
                let mut edges = Vec::new();
                for b in &v.brushes {
                    let w = rdoc::editmesh::brush_outline(b).ok()?;
                    let base = positions.len() as u32;
                    positions.extend_from_slice(&w.positions);
                    edges.extend(w.edges.iter().map(|e| [e[0] + base, e[1] + base]));
                }
                (!edges.is_empty()).then_some(rdoc::editmesh::WireSoup { positions, edges })
            }
            ObjectKind::Mesh(m) => {
                let bytes = self.mesh_bytes(&m.source)?;
                match rdoc::editmesh::load_triangles(&bytes, &m.source).ok()? {
                    rdoc::editmesh::LoadedMesh::Editable(base) => {
                        let ctx = rdoc::editmesh::EvalContext::from_doc(&self.doc, id);
                        let stacked = self.stack_cache.evaluate(&base, &m.modifiers, &ctx, true).ok()?;
                        let eval = rdoc::editmesh::evaluate(&stacked);
                        let positions =
                            stacked.positions.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect();
                        (!eval.poly_edges.is_empty())
                            .then_some(rdoc::editmesh::WireSoup { positions, edges: eval.poly_edges })
                    }
                    rdoc::editmesh::LoadedMesh::Soup(prims) => {
                        let mut positions = Vec::new();
                        let mut indices = Vec::new();
                        for prim in &prims {
                            let base = positions.len() as u32;
                            positions.extend_from_slice(&prim.positions);
                            indices.extend(prim.indices.iter().map(|i| i + base));
                        }
                        rdoc::editmesh::soup_wire_edges(&positions, &indices).ok()
                    }
                }
            }
            _ => None,
        }
    }

    /// Copies staged files into the document store, so mesh ops see glbs that
    /// arrived after the last load.
    fn sync_files(&mut self) {
        for (p, b) in self.files.clone() {
            if !self.doc.files.exists(&p) {
                let _ = self.doc.files.write(&p, &b);
            }
        }
    }

    fn mesh_of(&mut self, id: Id) -> Option<Mesh> {
        let o = self.doc.object(id)?;
        let world = self.doc.world_matrix(id)?;
        let mut mesh = Mesh::default();
        match &o.kind {
            ObjectKind::Brush(b) => add_brush(&mut mesh, b, world),
            ObjectKind::Volume(v) => v.brushes.iter().for_each(|b| add_brush(&mut mesh, b, world)),
            ObjectKind::Mesh(m) => {
                // An open session owns the mesh: the viewport draws the drag,
                // not the last commit. Either way the base runs through its
                // modifier stack before drawing.
                let ctx = rdoc::editmesh::EvalContext::from_doc(&self.doc, id);
                let prims = if let Some(base) =
                    self.session.as_ref().filter(|s| s.object() == id).map(|s| s.mesh().clone())
                {
                    let stacked = self.stack_cache.evaluate(&base, &m.modifiers, &ctx, true).ok()?;
                    rdoc::editmesh::convert::evaluated_primitives(&stacked)
                } else {
                    let bytes = self.mesh_bytes(&m.source)?;
                    let staged = rdoc::editmesh::load_triangles(&bytes, &m.source).ok()?;
                    match staged {
                        rdoc::editmesh::LoadedMesh::Editable(base) => {
                            let stacked = self.stack_cache.evaluate(&base, &m.modifiers, &ctx, true).ok()?;
                            rdoc::editmesh::convert::evaluated_primitives(&stacked)
                        }
                        rdoc::editmesh::LoadedMesh::Soup(_) => {
                            rdoc::editmesh::load_triangles_stacked(&bytes, &m.source, &m.modifiers, &ctx).ok()?
                        }
                    }
                };
                for glb in &prims {
                    let material = m
                        .material_map
                        .get(&glb.material)
                        .map(|s| s.as_str())
                        .unwrap_or(glb.material.as_str())
                        .to_string();
                    mesh.begin(&material);
                    let base = (mesh.positions.len() / 3) as u32;
                    for (k, p) in glb.positions.iter().enumerate() {
                        let wp = world.transform_point3(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64));
                        let n = glb.normals.get(k).copied().unwrap_or([0.0, 0.0, 1.0]);
                        let wn =
                            world.transform_vector3(DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64)).normalize_or_zero();
                        mesh.positions.extend([wp.x as f32, wp.y as f32, wp.z as f32]);
                        mesh.normals.extend([wn.x as f32, wn.y as f32, wn.z as f32]);
                        mesh.uvs.extend(glb.uvs.get(k).copied().unwrap_or([0.0, 0.0]));
                    }
                    mesh.indices.extend(glb.indices.iter().map(|i| i + base));
                    mesh.close();
                }
            }
            ObjectKind::Terrain(t) => {
                let hf = self.heights(t).ok()?;
                let tm = rterrain::terrain_mesh::build(&hf, None, &Default::default()).ok()?;
                let material = t.layers.first().map(|l| l.material.as_str()).unwrap_or("");
                mesh.begin(material);
                for v in &tm.vertices {
                    let p = world.transform_point3(DVec3::new(v.position[0] as f64, v.position[1] as f64, v.position[2] as f64));
                    let n = world.transform_vector3(DVec3::new(v.normal[0] as f64, v.normal[1] as f64, v.normal[2] as f64)).normalize_or_zero();
                    mesh.positions.extend([p.x as f32, p.y as f32, p.z as f32]);
                    mesh.normals.extend([n.x as f32, n.y as f32, n.z as f32]);
                    mesh.uvs.extend(v.uv0);
                }
                for c in tm.chunks.iter().filter(|c| c.lod == 0) {
                    mesh.indices.extend(c.indices.iter().map(|i| i + c.vertex_offset));
                }
                mesh.close();
            }
            // Meshed the way the compiler meshes it (`rcomp` `voxel_object`), so
            // the viewport draws, frames and picks what the map will carry.
            ObjectKind::VoxelTerrain(v) => {
                let ops: Vec<rterrain::sdf::SdfOp> = serde_json::from_value(v.ops.clone()).ok()?;
                if ops.is_empty() {
                    return None;
                }
                let voxel_size = v.voxel_size.abs().max(1.0);
                let terrain = rterrain::sdf::VolumeTerrain {
                    chunks: rterrain::sdf::VolumeTerrain::resolution_for(v.bounds[0], v.bounds[1], voxel_size),
                    voxel_size,
                    origin: v.bounds[0],
                    ops,
                };
                let vm = rterrain::mesher::mesh(&terrain).ok()?;
                let scale = if v.uv_scale.abs() > 0.0 { v.uv_scale } else { 128.0 };
                mesh.begin(&v.material);
                for x in &vm.vertices {
                    let p = world.transform_point3(x.position);
                    let n = world.transform_vector3(x.normal).normalize_or_zero();
                    mesh.positions.extend([p.x as f32, p.y as f32, p.z as f32]);
                    mesh.normals.extend([n.x as f32, n.y as f32, n.z as f32]);
                    mesh.uvs.extend([(p.x / scale) as f32, (p.y / scale) as f32]);
                }
                for c in &vm.chunks {
                    mesh.indices.extend(c.indices.iter().copied());
                }
                let whole = mesh.indices.len() - mesh.indices.len() % 3;
                mesh.indices.truncate(whole);
                mesh.close();
            }
            _ => return None,
        }
        (!mesh.indices.is_empty()).then_some(mesh)
    }
}
