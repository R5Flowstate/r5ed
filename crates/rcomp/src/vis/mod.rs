//! Visibility: the cell tree, the portals between cells, the occlusion meshes
//! and the per-cell AABB tree over the drawable objects.

use glam::Vec3;
use rbsp::lumps::vis as vl;
use rbsp::{ids, Bsp, Version};

/// One drawable object a cell has to know about.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisObject {
    /// Render mesh index, or the level info's first static prop index plus the
    /// prop's own.
    pub reference: u32,
    pub mins: Vec3,
    pub maxs: Vec3,
}

/// A portal polygon in world space. Its points wind counter-clockwise around
/// the normal of `plane` (an index into 0x01), which faces into the cell the
/// portal sits in; `cell_to` is the cell it opens onto.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Portal {
    pub points: Vec<Vec3>,
    pub plane: u32,
    pub cell_to: u16,
}

/// One face of an axis-aligned box, as a portal wants it: the plane facing
/// into the box and the corners wound counter-clockwise around that normal.
#[derive(Clone, Debug, PartialEq)]
pub struct BoxFace {
    pub normal: [f32; 3],
    pub dist: f32,
    pub points: Vec<Vec3>,
}

/// A convex mesh the vis pass traces against.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OcclusionMesh {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u16>,
}

/// A cell's contents mask and its world box.
#[derive(Clone, Debug, PartialEq)]
pub struct CellBox {
    pub mask: u32,
    pub mins: Vec3,
    pub maxs: Vec3,
}

#[derive(Debug, thiserror::Error)]
pub enum VisError {
    #[error("portal {0} has fewer than three vertices")]
    PortalDegenerate(usize),
    #[error("cell {0} is empty")]
    CellEmpty(usize),
    #[error("object {0} has a non-finite bound")]
    ObjectNotFinite(usize),
    #[error("a cell AABB tree would need {0} nodes, more than the 65535 the child field addresses")]
    TreeTooLarge(usize),
    #[error("lump 0x{id:02x}: {message}")]
    Lump { id: u16, message: String },
}

/// Every vis lump, decoded. Both the single-cell writer and the reader produce
/// this, so a decode followed by an encode reproduces the input bytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VisLumps {
    pub cell_bsp_nodes: Vec<vl::CellBspNode>,
    pub cells: Vec<vl::Cell>,
    pub portals: Vec<vl::Portal>,
    pub portal_vertices: Vec<[f32; 3]>,
    pub portal_edges: Vec<vl::PortalEdge>,
    pub portal_vert_edges: Vec<vl::PortalVertEdges>,
    pub portal_vert_refs: Vec<u16>,
    pub portal_edge_refs: Vec<u16>,
    pub isect_edges: Vec<vl::IsectList>,
    pub isect_verts: Vec<vl::IsectList>,
    pub isect_headers: Vec<vl::IsectHeader>,
    pub occlusion_vertices: Vec<[f32; 3]>,
    pub occlusion_indices: Vec<u16>,
    pub aabb_nodes: Vec<vl::AabbNode>,
    pub obj_refs: Vec<u32>,
    /// The v37 form stores the references 16 bits wide.
    pub narrow_obj_refs: bool,
    pub obj_ref_bounds: Vec<vl::ObjRefBounds>,
    pub totals: Vec<u32>,
    pub fade: Vec<u16>,
}

fn narrow(list: &[vl::ObjRef16]) -> Vec<u32> {
    list.iter().map(|o| u32::from(o.value)).collect()
}

impl VisLumps {
    /// Decodes every vis lump a map carries. Lumps the map does not hold
    /// decode to empty.
    pub fn decode(bsp: &Bsp, version: Version) -> Result<Self, VisError> {
        let lump_err = |id: u16, e: rbsp::Error| VisError::Lump { id, message: e.to_string() };
        let get = |id: u16| -> Result<rbsp::lumps::Lump, VisError> {
            rbsp::lumps::parse_at(id, version, bsp.lump(id), 0).map_err(|e| lump_err(id, e))
        };
        let vec3f = |id: u16| -> Result<Vec<[f32; 3]>, VisError> {
            match get(id)? {
                rbsp::lumps::Lump::Vec3f(v) => Ok(v.into_iter().map(|p| [p.x, p.y, p.z]).collect()),
                other => Err(VisError::Lump { id, message: format!("lump decoded as {}", other.kind()) }),
            }
        };
        let typed = |id: u16, want: &str| -> Result<rbsp::lumps::Lump, VisError> {
            let l = get(id)?;
            if l.kind() != want {
                return Err(VisError::Lump { id, message: format!("lump decoded as {}", l.kind()) });
            }
            Ok(l)
        };
        let mut out = VisLumps {
            portal_vertices: vec3f(ids::PORTAL_VERTICES)?,
            occlusion_vertices: vec3f(ids::OCCLUSION_MESH_VERTICES)?,
            ..VisLumps::default()
        };
        if let rbsp::lumps::Lump::CellBspNode(v) = typed(ids::CELL_BSP_NODES, "cell_bsp_node")? {
            out.cell_bsp_nodes = v;
        }
        if let rbsp::lumps::Lump::Cell(v) = typed(ids::CELLS, "cell")? {
            out.cells = v;
        }
        if let rbsp::lumps::Lump::Portal(v) = typed(ids::PORTALS, "portal")? {
            out.portals = v;
        }
        if let rbsp::lumps::Lump::PortalEdge(v) = typed(ids::PORTAL_EDGES, "portal_edge")? {
            out.portal_edges = v;
        }
        if let rbsp::lumps::Lump::PortalVertEdges(v) = typed(ids::PORTAL_VERTEX_EDGES, "portal_vert_edges")? {
            out.portal_vert_edges = v;
        }
        if let rbsp::lumps::Lump::PortalRef(v) = typed(ids::PORTAL_VERTEX_REFERENCES, "portal_ref")? {
            out.portal_vert_refs = v.into_iter().map(|x| x.value).collect();
        }
        if let rbsp::lumps::Lump::PortalRef(v) = typed(ids::PORTAL_EDGE_REFERENCES, "portal_ref")? {
            out.portal_edge_refs = v.into_iter().map(|x| x.value).collect();
        }
        if let rbsp::lumps::Lump::PortalIsectList(v) = typed(ids::PORTAL_EDGE_INTERSECT_EDGE, "portal_isect_list")? {
            out.isect_edges = v;
        }
        if let rbsp::lumps::Lump::PortalIsectList(v) =
            typed(ids::PORTAL_EDGE_INTERSECT_AT_VERTEX, "portal_isect_list")?
        {
            out.isect_verts = v;
        }
        if let rbsp::lumps::Lump::PortalIsectHeader(v) =
            typed(ids::PORTAL_EDGE_INTERSECT_HEADER, "portal_isect_header")?
        {
            out.isect_headers = v;
        }
        if let rbsp::lumps::Lump::OcclusionIndex(v) = typed(ids::OCCLUSION_MESH_INDICES, "occlusion_index")? {
            out.occlusion_indices = v.into_iter().map(|x| x.value).collect();
        }
        if let rbsp::lumps::Lump::AabbNode(v) = typed(ids::CELL_AABB_NODES, "cell_aabb_node")? {
            out.aabb_nodes = v;
        }
        match typed(ids::OBJ_REFERENCES, "obj_ref")? {
            rbsp::lumps::Lump::ObjRef(v) => out.obj_refs = v.into_iter().map(|x| x.get()).collect(),
            rbsp::lumps::Lump::ObjRef16(v) => {
                out.obj_refs = narrow(&v);
                out.narrow_obj_refs = true;
            }
            _ => {}
        }
        if let rbsp::lumps::Lump::ObjRefBounds(v) = typed(ids::OBJ_REFERENCE_BOUNDS, "obj_ref_bounds")? {
            out.obj_ref_bounds = v;
        }
        if let rbsp::lumps::Lump::CellAabbTotals(v) = typed(ids::CELL_AABB_SUBTREE_REF_TOTALS, "cell_aabb_totals")? {
            out.totals = v.into_iter().map(|x| x.total).collect();
        }
        if let rbsp::lumps::Lump::FadeDist(v) = typed(ids::AABB_FADE_DISTS, "fade_dist")? {
            out.fade = v.into_iter().map(|x| x.value).collect();
        }
        Ok(out)
    }

    /// The vis lumps as bytes, ready to be set on a map.
    pub fn encode(&self, version: Version) -> Vec<(u16, Vec<u8>)> {
        let vecs = |v: &[[f32; 3]]| -> Vec<u8> { v.iter().flat_map(|p| p.iter()).flat_map(|f| f.to_le_bytes()).collect() };
        let u16s = |v: &[u16]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
        let u32s = |v: &[u32]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
        vec![
            (ids::CELL_BSP_NODES, vl::write_cell_bsp_nodes(&self.cell_bsp_nodes, version)),
            (ids::CELLS, vl::write_cells(&self.cells, version)),
            (ids::PORTALS, vl::write_portals(&self.portals, version)),
            (ids::PORTAL_VERTICES, vecs(&self.portal_vertices)),
            (ids::PORTAL_EDGES, vl::write_portal_edges(&self.portal_edges, version)),
            (ids::PORTAL_VERTEX_EDGES, vl::write_portal_vert_edges(&self.portal_vert_edges, version)),
            (ids::PORTAL_VERTEX_REFERENCES, u16s(&self.portal_vert_refs)),
            (ids::PORTAL_EDGE_REFERENCES, u16s(&self.portal_edge_refs)),
            (ids::PORTAL_EDGE_INTERSECT_EDGE, vl::write_portal_isect_edges(&self.isect_edges, version)),
            (ids::PORTAL_EDGE_INTERSECT_AT_VERTEX, vl::write_portal_isect_verts(&self.isect_verts, version)),
            (ids::PORTAL_EDGE_INTERSECT_HEADER, vl::write_portal_isect_headers(&self.isect_headers, version)),
            (ids::OCCLUSION_MESH_VERTICES, vecs(&self.occlusion_vertices)),
            (ids::OCCLUSION_MESH_INDICES, u16s(&self.occlusion_indices)),
            (ids::CELL_AABB_NODES, vl::write_aabb_nodes(&self.aabb_nodes, version)),
            (
                ids::OBJ_REFERENCES,
                if self.narrow_obj_refs {
                    vl::write_obj_refs16(
                        &self.obj_refs.iter().map(|v| vl::ObjRef16 { value: *v as u16 }).collect::<Vec<_>>(),
                        version,
                    )
                } else {
                    vl::write_obj_refs(
                        &self.obj_refs.iter().map(|v| vl::ObjRef { value: *v }).collect::<Vec<_>>(),
                        version,
                    )
                },
            ),
            (ids::OBJ_REFERENCE_BOUNDS, vl::write_obj_ref_bounds(&self.obj_ref_bounds, version)),
            (
                ids::CELL_AABB_SUBTREE_REF_TOTALS,
                u32s(&self.totals),
            ),
            (ids::AABB_FADE_DISTS, u16s(&self.fade)),
        ]
    }
}

/// Leaf objects one AABB node owns before the tree splits it.
const LEAF_OBJECTS: usize = 8;
/// Children one AABB node may address.
const MAX_CHILDREN: usize = 4;

fn centroid(o: &VisObject, axis: usize) -> f32 {
    (o.mins[axis] + o.maxs[axis]) * 0.5
}

struct TreeBuilder {
    nodes: Vec<vl::AabbNode>,
    refs: Vec<u32>,
    bounds: Vec<vl::ObjRefBounds>,
}

/// Splits a list into at most four runs along its longest axis, at even
/// counts. The order is a function of the input order alone, so the same
/// objects always produce the same tree.
fn split(objects: &mut [VisObject]) -> Vec<Vec<VisObject>> {
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for o in objects.iter() {
        lo = lo.min(o.mins);
        hi = hi.max(o.maxs);
    }
    let extent = hi - lo;
    let axis = (0usize..3).max_by(|&a, &b| extent[a].total_cmp(&extent[b]).then(b.cmp(&a))).unwrap_or(0);
    let groups = (objects.len() / LEAF_OBJECTS).clamp(1, MAX_CHILDREN);
    objects.sort_by(|a, b| {
        centroid(a, axis).total_cmp(&centroid(b, axis)).then(a.reference.cmp(&b.reference))
    });
    let mut out = Vec::with_capacity(groups);
    let chunk = objects.len().div_ceil(groups);
    for g in objects.chunks(chunk) {
        if !g.is_empty() {
            out.push(g.to_vec());
        }
    }
    out
}

impl TreeBuilder {
    fn build(objects: Vec<VisObject>) -> Result<Self, VisError> {
        let mut b = TreeBuilder { nodes: Vec::new(), refs: Vec::new(), bounds: Vec::new() };
        b.nodes.push(vl::AabbNode::default());
        b.fill(0, objects)?;
        Ok(b)
    }

    /// The node at `at` takes ownership of `objects`, either as a leaf or as
    /// the root of a split whose children are laid out right after the whole
    /// tree, so a child index is always past its parent.
    fn fill(&mut self, at: usize, objects: Vec<VisObject>) -> Result<(), VisError> {
        let (lo, hi) = axis_bounds_all(&objects)?;
        let mut objects = objects;
        if objects.is_empty() {
            self.nodes[at] = vl::AabbNode {
                mins: lo.to_array(),
                children: 0,
                maxs: hi.to_array(),
                obj_refs: 0,
            };
            return Ok(());
        }
        let groups = if objects.len() > LEAF_OBJECTS { split(&mut objects) } else { Vec::new() };
        if groups.len() < 2 {
            let first = self.refs.len() as u32;
            for o in &objects {
                if !o.mins.is_finite() || !o.maxs.is_finite() {
                    return Err(VisError::ObjectNotFinite(o.reference as usize));
                }
                self.refs.push(o.reference);
                self.bounds.push(vl::ObjRefBounds {
                    mins: o.mins.to_array(),
                    unknown_0: 0,
                    maxs: o.maxs.to_array(),
                    unknown_1: 0,
                });
            }
            let count = objects.len().min(0xFF) as u32;
            // A leaf names no children, so the child field stays zero and the
            // object run rides entirely in the reference field.
            self.nodes[at] = vl::AabbNode {
                mins: lo.to_array(),
                children: 0,
                maxs: hi.to_array(),
                obj_refs: count | (first << 8),
            };
            return Ok(());
        }
        let count = groups.len() as u32;
        let first_child = self.nodes.len();
        for _ in 0..groups.len() {
            self.nodes.push(vl::AabbNode::default());
        }
        if first_child > 0xFFFF {
            return Err(VisError::TreeTooLarge(self.nodes.len()));
        }
        let subtree_first = self.refs.len() as u32;
        for (i, g) in groups.into_iter().enumerate() {
            self.fill(first_child + i, g)?;
        }
        // A node whose whole box is in view marks its subtree total (0x25) of
        // references from here on without visiting its children, so an
        // interior node names where its subtree's run starts.
        self.nodes[at] = vl::AabbNode {
            mins: lo.to_array(),
            children: count | ((first_child as u32) << 8),
            maxs: hi.to_array(),
            obj_refs: subtree_first << 8,
        };
        Ok(())
    }
}

fn axis_bounds_all(objects: &[VisObject]) -> Result<(Vec3, Vec3), VisError> {
    if objects.is_empty() {
        return Ok((Vec3::ZERO, Vec3::ZERO));
    }
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for o in objects {
        lo = lo.min(o.mins);
        hi = hi.max(o.maxs);
    }
    if lo.x > hi.x {
        return Err(VisError::CellEmpty(0));
    }
    Ok((lo, hi))
}

/// Per-node subtree object-reference totals, in node order.
fn subtree_totals(nodes: &[vl::AabbNode], refs: usize) -> Vec<u32> {
    let mut out = vec![0u32; nodes.len()];
    for i in (0..nodes.len()).rev() {
        let n = &nodes[i];
        let mut total = n.obj_ref_count();
        for c in n.first_child() as usize..n.first_child() as usize + n.child_count() as usize {
            if let Some(v) = out.get(c) {
                total += v;
            }
        }
        out[i] = total;
    }
    let _ = refs;
    out
}

/// The single-cell visibility a map without portal brushes gets: one cell over
/// the world box that references every object from one AABB tree.
pub fn single_cell(
    objects: &[VisObject],
    world_mins: Vec3,
    world_maxs: Vec3,
) -> Result<VisLumps, VisError> {
    let mut lumps = VisLumps {
        cell_bsp_nodes: vec![vl::CellBspNode { contents: -1, child: 0 }],
        cells: vec![vl::Cell { portal_count: 0, first_portal: 0, sky_flags: vl::CELL_SKY_VISIBLE, unused: 0xFFFF }],
        ..VisLumps::default()
    };
    attach_objects(&mut lumps, objects)?;
    if !world_mins.is_finite() || !world_maxs.is_finite() {
        return Err(VisError::CellEmpty(0));
    }
    if let Some(root) = lumps.aabb_nodes.first_mut() {
        root.mins = world_mins.to_array();
        root.maxs = world_maxs.to_array();
    }
    Ok(lumps)
}

/// Adds the skybox cell a shipped map ends its cell list with: no portals,
/// `skyFlags` 3, and an AABB root with unbounded extents, no children, and the
/// sky objects' references, which go last. Cell `i`'s root is node `i`, so the
/// root is inserted after the other cells' roots and every later node moves up
/// one. The cell BSP gains a root split on `plane` (an index into 0x01): the
/// front half keeps the old tree and the back half is the skybox cell, which
/// is where the sky camera's position then resolves.
pub fn add_sky_cell(lumps: &mut VisLumps, sky: &[VisObject], plane: u32) -> Result<(), VisError> {
    for (i, o) in sky.iter().enumerate() {
        if !o.mins.is_finite() || !o.maxs.is_finite() {
            return Err(VisError::ObjectNotFinite(i));
        }
    }
    let cell = lumps.cells.len() as u32;
    let root = cell as usize;
    if lumps.aabb_nodes.len() + 1 > u16::MAX as usize {
        return Err(VisError::TreeTooLarge(lumps.aabb_nodes.len() + 1));
    }
    let first_ref = lumps.obj_refs.len() as u32;
    if first_ref + sky.len() as u32 > 0x3F_FFFF || sky.len() > 0xFF {
        return Err(VisError::TreeTooLarge(lumps.obj_refs.len() + sky.len()));
    }
    for n in &mut lumps.aabb_nodes {
        if n.child_count() > 0 && n.first_child() as usize >= root {
            n.children = (n.children & !0x00FF_FF00) | ((n.first_child() + 1) << 8);
        }
    }
    lumps.aabb_nodes.insert(
        root.min(lumps.aabb_nodes.len()),
        vl::AabbNode {
            mins: [f32::NEG_INFINITY; 3],
            children: 0,
            maxs: [f32::INFINITY; 3],
            obj_refs: (first_ref << 8) | sky.len() as u32,
        },
    );
    for o in sky {
        lumps.obj_refs.push(o.reference);
        lumps.obj_ref_bounds.push(vl::ObjRefBounds { mins: o.mins.to_array(), maxs: o.maxs.to_array(), ..Default::default() });
    }
    lumps.totals = subtree_totals(&lumps.aabb_nodes, lumps.obj_refs.len());
    lumps.fade = vec![0xFFFF; lumps.aabb_nodes.len()];
    lumps.cells.push(vl::Cell {
        portal_count: 0,
        first_portal: lumps.portals.len().min(u16::MAX as usize) as u16,
        sky_flags: vl::CELL_SKY_VISIBLE | vl::CELL_SKYBOX,
        unused: 0xFFFF,
    });
    // [split -> (old tree, skybox leaf), old nodes after them]: node 1 is the
    // old root and node 2 the leaf, so the old tree's own child indices move
    // up by two.
    let old = std::mem::take(&mut lumps.cell_bsp_nodes);
    let shift = |n: vl::CellBspNode| {
        if n.is_terminator() {
            n
        } else {
            vl::CellBspNode { contents: n.contents, child: if n.child >= 1 { n.child + 2 } else { 1 } }
        }
    };
    lumps.cell_bsp_nodes.push(vl::CellBspNode { contents: plane as i32, child: 1 });
    lumps.cell_bsp_nodes.push(old.first().copied().map(shift).unwrap_or(vl::CellBspNode { contents: -1, child: 0 }));
    lumps.cell_bsp_nodes.push(vl::CellBspNode { contents: -1, child: cell });
    lumps.cell_bsp_nodes.extend(old.iter().skip(1).copied().map(shift));
    Ok(())
}

/// Appends one portal with vertices, edges and intersections of its own. Ref
/// `j` names corner `j` and the edge from corner `j - 1` to it, so every edge
/// ref runs forward (flip bit clear). Each corner's fan holds the two edges
/// meeting there, and each edge's one intersection record names those same
/// neighbours at its two ends. Vertex 0 stays a placeholder: the client reads
/// an intersection vertex of 0 as "none recorded".
fn push_portal(lumps: &mut VisLumps, points: &[Vec3], plane: u32, cell_to: u16, portal_type: u8) -> Result<(), VisError> {
    let n = points.len();
    if n < 3 || n > u8::MAX as usize {
        return Err(VisError::PortalDegenerate(lumps.portals.len()));
    }
    if lumps.portal_vertices.is_empty() {
        lumps.portal_vertices.push([0.0; 3]);
        lumps.portal_vert_edges.push(vl::PortalVertEdges::default());
    }
    let v0 = lumps.portal_vertices.len();
    let e0 = lumps.portal_edges.len();
    let first_ref = lumps.portal_vert_refs.len();
    if v0 + n > u16::MAX as usize || e0 + n > (u16::MAX >> 1) as usize || first_ref + n > u16::MAX as usize {
        return Err(VisError::Lump { id: ids::PORTALS, message: "portal tables outgrow their 16-bit indices".to_string() });
    }
    let vert = |j: usize| (v0 + j % n) as u16;
    let edge = |j: usize| (e0 + j % n) as u16;
    for (j, p) in points.iter().enumerate() {
        lumps.portal_vertices.push(p.to_array());
        lumps.portal_edges.push(vl::PortalEdge { vert0: vert(j + n - 1), vert1: vert(j) });
        let mut fan = vl::PortalVertEdges::default();
        fan.edges[0] = edge(j);
        fan.edges[1] = edge(j + 1);
        lumps.portal_vert_edges.push(fan);
        lumps.portal_vert_refs.push(vert(j));
        lumps.portal_edge_refs.push(edge(j) << 1);
        let mut with = vl::IsectList::default();
        let mut at = vl::IsectList::default();
        with.slots[0].value = edge(j + n - 1);
        at.slots[0].value = vert(j + n - 1);
        with.slots[1].value = edge(j + 1);
        at.slots[1].value = vert(j);
        lumps.isect_headers.push(vl::IsectHeader { first: lumps.isect_edges.len() as u32, count: 1 });
        lumps.isect_edges.push(with);
        lumps.isect_verts.push(at);
    }
    lumps.portals.push(vl::Portal {
        is_reversed: 0,
        portal_type,
        edge_count: n as u8,
        pad: 0,
        first_ref: first_ref as u16,
        cell_to,
        plane,
    });
    Ok(())
}

/// The six faces of a box, each facing into it.
pub fn box_faces(mins: Vec3, maxs: Vec3) -> Vec<BoxFace> {
    let mut out = Vec::with_capacity(6);
    for axis in 0..3 {
        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
        for inward in [1.0f32, -1.0] {
            let at = if inward > 0.0 { mins[axis] } else { maxs[axis] };
            let corner = |u: f32, v: f32| {
                let mut p = Vec3::ZERO;
                p[axis] = at;
                p[b] = u;
                p[c] = v;
                p
            };
            let mut points = vec![corner(mins[b], mins[c]), corner(maxs[b], mins[c]), corner(maxs[b], maxs[c]), corner(mins[b], maxs[c])];
            let mut normal = Vec3::ZERO;
            normal[axis] = inward;
            let wound = (points[1] - points[0]).cross(points[2] - points[1]);
            if wound.dot(normal) < 0.0 {
                points.reverse();
            }
            out.push(BoxFace { normal: normal.to_array(), dist: inward * at, points });
        }
    }
    out
}

/// Gives `cell` portals onto the sky. The client draws the 3D skybox only
/// when the view walks out of a cell through one of these, which open onto
/// the cell one past the last. Each face pairs corners with a plane index.
/// Cells after `cell` must own no portals yet.
pub fn add_sky_portals(lumps: &mut VisLumps, cell: usize, faces: &[(Vec<Vec3>, u32)]) -> Result<(), VisError> {
    let sky = u16::try_from(lumps.cells.len() + 1).map_err(|_| VisError::CellEmpty(cell))?;
    let run = lumps.cells.get(cell).ok_or(VisError::CellEmpty(cell))?;
    if run.portal_count > 0 && run.portal_end() != lumps.portals.len() {
        return Err(VisError::Lump { id: ids::CELLS, message: format!("cell {cell}'s portals are not the last run") });
    }
    if lumps.cells[cell + 1..].iter().any(|c| c.portal_count > 0) {
        return Err(VisError::Lump { id: ids::CELLS, message: format!("a cell after {cell} already owns portals") });
    }
    let first = if run.portal_count > 0 { run.first_portal as usize } else { lumps.portals.len() };
    for (points, plane) in faces {
        push_portal(lumps, points, *plane, sky, vl::PORTAL_SKY)?;
    }
    let end = lumps.portals.len();
    lumps.cells[cell].first_portal = first as u16;
    lumps.cells[cell].portal_count = (end - first) as u16;
    lumps.cells[cell].sky_flags |= vl::CELL_SKY_VISIBLE;
    for c in &mut lumps.cells[cell + 1..] {
        c.first_portal = end as u16;
    }
    Ok(())
}

/// Cell-based visibility: one cell per `CellBox`, one portal polygon per entry,
/// and each object referenced from every cell its box touches.
pub fn portal_cells(
    cells: &[CellBox],
    portals: &[Portal],
    occlusion: &[OcclusionMesh],
    objects: &[VisObject],
) -> Result<VisLumps, VisError> {
    if cells.is_empty() {
        return Err(VisError::CellEmpty(0));
    }
    let mut lumps = VisLumps::default();
    // 0x6A is one BSP run per cell, each closed by a terminator carrying the
    // cell index. A run with no interior node is the minimum the loader accepts.
    for (i, c) in cells.iter().enumerate() {
        lumps.cell_bsp_nodes.push(vl::CellBspNode { contents: c.mask as i32, child: i as u32 });
        lumps.cell_bsp_nodes.push(vl::CellBspNode { contents: -1, child: i as u32 });
        lumps.cells.push(vl::Cell { portal_count: 0, first_portal: 0, sky_flags: vl::CELL_SKY_VISIBLE, unused: 0xFFFF });
    }
    // A cell's portals are one contiguous run, so they go out grouped by the
    // cell whose box holds their centre.
    let mut owned: Vec<(usize, usize)> = Vec::with_capacity(portals.len());
    for (pi, p) in portals.iter().enumerate() {
        if p.points.len() < 3 {
            return Err(VisError::PortalDegenerate(pi));
        }
        let centre = p.points.iter().fold(Vec3::ZERO, |a, b| a + *b) / p.points.len() as f32;
        let owner = cells.iter().position(|c| centre.cmpge(c.mins).all() && centre.cmple(c.maxs).all());
        owned.push((owner.unwrap_or(0), pi));
    }
    owned.sort();
    for &(cell, pi) in &owned {
        let p = &portals[pi];
        let first = lumps.portals.len();
        push_portal(&mut lumps, &p.points, p.plane, p.cell_to, vl::PORTAL_CELL).map_err(|_| VisError::PortalDegenerate(pi))?;
        let c = &mut lumps.cells[cell];
        if c.portal_count == 0 {
            c.first_portal = first as u16;
        }
        c.portal_count += 1;
    }
    for m in occlusion {
        let base = lumps.occlusion_vertices.len() as u16;
        for p in &m.positions {
            if lumps.occlusion_vertices.len() >= u16::MAX as usize {
                break;
            }
            lumps.occlusion_vertices.push(p.to_array());
        }
        for i in &m.indices {
            lumps.occlusion_indices.push(base + *i);
        }
    }
    attach_objects(&mut lumps, objects)?;
    Ok(lumps)
}

/// Builds the object AABB tree and its totals, over every cell at once. With
/// one cell the tree covers the world; with several, each object is referenced
/// once and the tree is the union of the per-cell boxes.
fn attach_objects(lumps: &mut VisLumps, objects: &[VisObject]) -> Result<(), VisError> {
    if objects.is_empty() {
        lumps.aabb_nodes.push(vl::AabbNode::default());
        return Ok(());
    }
    let tree = TreeBuilder::build(objects.to_vec())?;
    lumps.totals = subtree_totals(&tree.nodes, tree.refs.len());
    lumps.fade = vec![0xFFFF; tree.nodes.len()];
    lumps.aabb_nodes = tree.nodes;
    lumps.obj_refs = tree.refs;
    lumps.obj_ref_bounds = tree.bounds;
    Ok(())
}

/// Writes a decoded vis set into a map and returns the counts it reported.
pub fn write(lumps: &VisLumps, bsp: &mut Bsp, target: Version) -> (usize, usize) {
    for (id, data) in lumps.encode(target) {
        bsp.set_lump(id, data);
    }
    (lumps.cells.len(), lumps.obj_refs.len())
}

/// Reads the vis lumps back out of a map.
pub fn read(bsp: &Bsp, target: Version) -> Result<VisLumps, VisError> {
    VisLumps::decode(bsp, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn objects(n: usize) -> Vec<VisObject> {
        (0..n)
            .map(|i| VisObject {
                reference: i as u32,
                mins: Vec3::new(i as f32 * 16.0, 0.0, 0.0),
                maxs: Vec3::new(i as f32 * 16.0 + 8.0, 8.0, 8.0),
            })
            .collect()
    }

    #[test]
    fn single_cell_tree_is_consistent() {
        let lumps = single_cell(&objects(40), Vec3::splat(-1024.0), Vec3::splat(1024.0))
            .expect("one cell over forty objects");
        assert_eq!(lumps.cells.len(), 1);
        assert_eq!(lumps.cell_bsp_nodes.len(), 1);
        assert!(lumps.cell_bsp_nodes[0].is_terminator());
        assert_eq!(lumps.obj_refs.len(), 40);
        assert_eq!(lumps.obj_ref_bounds.len(), 40);
        assert_eq!(lumps.totals.len(), lumps.aabb_nodes.len());
        assert_eq!(lumps.fade.len(), lumps.aabb_nodes.len());
        assert_eq!(lumps.totals[0], 40);
        for (i, n) in lumps.aabb_nodes.iter().enumerate() {
            assert!(n.first_child() as usize > i || n.child_count() == 0, "children follow the parent");
            assert!(n.first_child() as usize + n.child_count() as usize <= lumps.aabb_nodes.len());
            assert!(n.first_obj_ref() as usize + n.obj_ref_count() as usize <= lumps.obj_refs.len());
        }
        assert_eq!(lumps.obj_refs, (0..40u32).collect::<Vec<_>>());
    }

    #[test]
    fn an_interior_node_names_its_subtree_run() {
        let lumps = single_cell(&objects(4000), Vec3::splat(-1e5), Vec3::splat(1e5)).expect("a deep tree");
        let nodes = &lumps.aabb_nodes;
        fn span(nodes: &[vl::AabbNode], i: usize, out: &mut Vec<(u32, u32)>) {
            let n = &nodes[i];
            if n.obj_ref_count() > 0 {
                out.push((n.first_obj_ref(), n.obj_ref_count()));
            }
            for c in n.first_child() as usize..n.first_child() as usize + n.child_count() as usize {
                span(nodes, c, out);
            }
        }
        let mut interior = 0;
        for (i, n) in nodes.iter().enumerate() {
            if n.child_count() == 0 {
                continue;
            }
            interior += 1;
            let mut runs = Vec::new();
            span(nodes, i, &mut runs);
            runs.sort();
            for w in runs.windows(2) {
                assert_eq!(w[0].0 + w[0].1, w[1].0, "node {i}: its subtree's references are one run");
            }
            assert_eq!(n.first_obj_ref(), runs[0].0, "node {i} names where its subtree starts");
            assert_eq!(lumps.totals[i], runs.iter().map(|r| r.1).sum::<u32>(), "node {i} total");
        }
        assert!(interior > 1, "the test needs nested interior nodes");
    }

    #[test]
    fn a_single_object_needs_no_split() {
        let lumps = single_cell(&objects(1), Vec3::splat(-8.0), Vec3::splat(8.0)).expect("one object");
        assert_eq!(lumps.aabb_nodes.len(), 1);
        assert_eq!(lumps.aabb_nodes[0].child_count(), 0, "a leaf has no children");
        assert_eq!(lumps.aabb_nodes[0].obj_ref_count(), 1);
    }

    #[test]
    fn empty_geometry_still_yields_a_node() {
        let lumps = single_cell(&[], Vec3::ZERO, Vec3::ZERO).expect("an empty map still has one node");
        assert_eq!(lumps.aabb_nodes.len(), 1);
        assert!(lumps.obj_refs.is_empty());
    }

    #[test]
    fn portal_cells_link_portals_to_the_cell_they_sit_in() {
        let cells = vec![
            CellBox { mask: 0x48, mins: Vec3::splat(-64.0), maxs: Vec3::ZERO },
            CellBox { mask: 0x48, mins: Vec3::ZERO, maxs: Vec3::splat(64.0) },
        ];
        let portals = vec![Portal {
            points: vec![Vec3::new(0.0, -32.0, -32.0), Vec3::new(0.0, 32.0, -32.0), Vec3::new(0.0, 0.0, 32.0)],
            plane: 0,
            cell_to: 0,
        }];
        let objects = vec![VisObject { reference: 0, mins: Vec3::splat(-8.0), maxs: Vec3::splat(8.0) }];
        let lumps = portal_cells(&cells, &portals, &[], &objects).expect("two cells one portal");
        assert_eq!(lumps.cells.len(), 2);
        assert_eq!(lumps.portals.len(), 1);
        // Three corners plus the placeholder vertex 0.
        assert_eq!(lumps.portal_vertices.len(), 4);
        assert_eq!(lumps.portal_edges.len(), 3);
        assert_eq!(lumps.portal_vert_edges.len(), 4);
        assert_eq!(lumps.isect_headers.len(), 3);
        assert_eq!(lumps.isect_edges.len(), lumps.isect_verts.len());
        for c in &lumps.cells {
            assert!(c.portal_end() <= lumps.portals.len());
        }
    }

    #[test]
    fn sky_portals_follow_the_shipped_layout() {
        let mut lumps = single_cell(&objects(4), Vec3::splat(-1024.0), Vec3::splat(1024.0)).expect("one cell");
        add_sky_cell(&mut lumps, &[VisObject { reference: 9, mins: Vec3::splat(-8.0), maxs: Vec3::splat(8.0) }], 0)
            .expect("skybox cell");
        let faces = box_faces(Vec3::splat(-100.0), Vec3::splat(100.0));
        let with_planes: Vec<(Vec<Vec3>, u32)> = faces.iter().enumerate().map(|(i, f)| (f.points.clone(), i as u32)).collect();
        add_sky_portals(&mut lumps, 0, &with_planes).expect("sky portals");
        assert_eq!(lumps.cells[0].portal_count, 6);
        assert_eq!(lumps.cells[1].first_portal, 6);
        assert_eq!(lumps.cells[1].portal_count, 0);
        let inside = Vec3::new(10.0, -20.0, 30.0);
        for (f, p) in faces.iter().zip(&lumps.portals) {
            assert_eq!(p.cell_to, 3);
            assert_eq!(p.portal_type, vl::PORTAL_SKY);
            let n = Vec3::from_array(f.normal);
            // The client keeps a portal whose plane faces the camera.
            assert!(n.dot(inside) - f.dist > 0.0);
            let c: Vec<Vec3> = (0..4).map(|j| Vec3::from_array(lumps.portal_vertices[lumps.portal_vert_refs[p.first_ref as usize + j] as usize])).collect();
            for q in &c {
                assert!((n.dot(*q) - f.dist).abs() < 1e-3);
            }
            assert!((c[1] - c[0]).cross(c[2] - c[1]).dot(n) > 0.0);
            for j in 0..4 {
                let e = lumps.portal_edges[(lumps.portal_edge_refs[p.first_ref as usize + j] >> 1) as usize];
                let dir = Vec3::from_array(lumps.portal_vertices[e.vert1 as usize]) - Vec3::from_array(lumps.portal_vertices[e.vert0 as usize]);
                assert!(dir.dot(c[j] - c[(j + 3) % 4]) > 0.0);
            }
        }
        assert!(lumps.isect_headers.iter().all(|h| h.count == 1));
        assert_eq!(lumps.portal_vert_edges[0], vl::PortalVertEdges::default());
    }

    #[test]
    fn a_degenerate_portal_is_rejected() {
        let cells = vec![CellBox { mask: 1, mins: Vec3::ZERO, maxs: Vec3::ONE }];
        let err = portal_cells(&cells, &[Portal { points: vec![Vec3::ZERO, Vec3::ONE], plane: 0, cell_to: 0 }], &[], &[]);
        assert!(matches!(err, Err(VisError::PortalDegenerate(0))));
    }
}
