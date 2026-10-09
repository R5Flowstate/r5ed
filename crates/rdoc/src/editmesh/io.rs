//! The `.r5mesh` container: little-endian `"R5MS"`, version 1, counts, then
//! positions (f64), faces, corners (vertex, uv, per-corner normal presence),
//! sparse edge flags, the optional smooth angle, and material names.

use glam::DVec3;

use super::{bad, finite_in_range, Corner, EditError, EditMesh, EdgeFlags, Face, FaceFlags};
use super::{MAX_CORNERS, MAX_FACES, MAX_MATERIALS, MAX_MATERIAL_NAME, MAX_VERTICES};

const MAGIC: &[u8; 4] = b"R5MS";
const VERSION: u32 = 1;

fn u32_at(bytes: &[u8], at: &mut usize) -> Result<u32, EditError> {
    let end = at.checked_add(4).ok_or_else(|| bad("r5mesh is truncated"))?;
    let v = bytes.get(*at..end).ok_or_else(|| bad("r5mesh is truncated"))?;
    *at = end;
    Ok(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
}

fn u16_at(bytes: &[u8], at: &mut usize) -> Result<u16, EditError> {
    let end = at.checked_add(2).ok_or_else(|| bad("r5mesh is truncated"))?;
    let v = bytes.get(*at..end).ok_or_else(|| bad("r5mesh is truncated"))?;
    *at = end;
    Ok(u16::from_le_bytes([v[0], v[1]]))
}

fn byte_at(bytes: &[u8], at: &mut usize) -> Result<u8, EditError> {
    let b = *bytes.get(*at).ok_or_else(|| bad("r5mesh is truncated"))?;
    *at += 1;
    Ok(b)
}

fn f32_at(bytes: &[u8], at: &mut usize) -> Result<f32, EditError> {
    let raw = bytes.get(*at..*at + 4).ok_or_else(|| bad("r5mesh is truncated"))?;
    *at += 4;
    Ok(f32::from_le_bytes(raw.try_into().unwrap()))
}

fn f64_at(bytes: &[u8], at: &mut usize) -> Result<f64, EditError> {
    let v = f64::from_le_bytes(bytes.get(*at..*at + 8).ok_or_else(|| bad("r5mesh is truncated"))?.try_into().unwrap());
    *at += 8;
    Ok(v)
}

/// Reads and validates an `.r5mesh` file. Counts, indices, finiteness and
/// magnitudes are checked; nothing panics on bad input.
pub fn read(bytes: &[u8]) -> Result<EditMesh, EditError> {
    if bytes.len() < 28 || &bytes[0..4] != MAGIC {
        return Err(bad("not an r5mesh file"));
    }
    let mut at = 4usize;
    if u32_at(bytes, &mut at)? != VERSION {
        return Err(bad("unsupported r5mesh version"));
    }
    let (nv, nf, nc, ne, nm) =
        (u32_at(bytes, &mut at)? as usize, u32_at(bytes, &mut at)? as usize, u32_at(bytes, &mut at)? as usize, u32_at(bytes, &mut at)? as usize, u32_at(bytes, &mut at)? as usize);
    if nv > MAX_VERTICES || nf > MAX_FACES || nc > MAX_CORNERS || ne > MAX_CORNERS * 2 || nm > MAX_MATERIALS {
        return Err(bad("r5mesh counts are larger than the mesh budget"));
    }
    let need = 28 + (nv as u64) * 24 + (nf as u64) * 11 + (nc as u64) * 12 + (ne as u64) * 9 + 1;
    if (bytes.len() as u64) < need {
        return Err(bad("r5mesh is truncated"));
    }
    let mut positions = Vec::with_capacity(nv.min(1 << 20));
    for _ in 0..nv {
        let p = DVec3::new(f64_at(bytes, &mut at)?, f64_at(bytes, &mut at)?, f64_at(bytes, &mut at)?);
        if !(finite_in_range(p.x) && finite_in_range(p.y) && finite_in_range(p.z)) {
            return Err(bad("r5mesh position is not finite or is out of range"));
        }
        positions.push(p);
    }
    let mut faces = Vec::with_capacity(nf.min(1 << 20));
    for _ in 0..nf {
        let (first, len) = (u32_at(bytes, &mut at)?, u32_at(bytes, &mut at)?);
        let (material, flags) = (u16_at(bytes, &mut at)?, byte_at(bytes, &mut at)?);
        if flags & !0x01 != 0 {
            return Err(bad("r5mesh face carries unknown flags"));
        }
        if len < 3 {
            return Err(bad("r5mesh face has fewer than three corners"));
        }
        if first as usize > nc || len as usize > nc || first as usize + len as usize > nc {
            return Err(bad("r5mesh face runs past the corner table"));
        }
        if material as usize >= nm {
            return Err(bad("r5mesh face names a material slot that does not exist"));
        }
        faces.push(Face { first, len, material, flags: FaceFlags(flags) });
    }
    let mut corners = Vec::with_capacity(nc.min(1 << 20));
    for _ in 0..nc {
        let vertex = u32_at(bytes, &mut at)?;
        let uv = [f32_at(bytes, &mut at)?, f32_at(bytes, &mut at)?];
        let present = byte_at(bytes, &mut at)?;
        if present & !0x01 != 0 {
            return Err(bad("r5mesh corner carries unknown flags"));
        }
        let normal = if present & 0x01 != 0 {
            let n = [f32_at(bytes, &mut at)?, f32_at(bytes, &mut at)?, f32_at(bytes, &mut at)?];
            if n.iter().any(|v| !v.is_finite()) {
                return Err(bad("r5mesh normal is not finite"));
            }
            Some(n)
        } else {
            None
        };
        if vertex as usize >= nv {
            return Err(bad("r5mesh corner names a vertex that does not exist"));
        }
        if uv.iter().any(|v| !v.is_finite()) {
            return Err(bad("r5mesh uv is not finite"));
        }
        corners.push(Corner { vertex, uv, normal });
    }
    let mut edge_flags = Vec::with_capacity(ne.min(1 << 20));
    for _ in 0..ne {
        let (a, b) = (u32_at(bytes, &mut at)?, u32_at(bytes, &mut at)?);
        let bits = byte_at(bytes, &mut at)?;
        if bits & !(EdgeFlags::SHARP | EdgeFlags::SEAM) != 0 || bits == 0 {
            return Err(bad("r5mesh edge carries unknown or empty flags"));
        }
        if a as usize >= nv || b as usize >= nv || a == b {
            return Err(bad("r5mesh edge names a vertex that does not exist"));
        }
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        if edge_flags.last().is_some_and(|e: &(u32, u32, EdgeFlags)| (e.0, e.1) >= (a, b)) {
            return Err(bad("r5mesh edge flags are not sorted"));
        }
        edge_flags.push((a, b, EdgeFlags(bits)));
    }
    let smooth_angle = match byte_at(bytes, &mut at)? {
        0 => None,
        1 => {
            let a = f64_at(bytes, &mut at)?;
            if !a.is_finite() || !(0.0..=180.0).contains(&a) {
                return Err(bad("r5mesh smooth angle is out of range"));
            }
            Some(a)
        }
        _ => return Err(bad("r5mesh smooth angle flag is not valid")),
    };
    let mut materials = Vec::with_capacity(nm.min(1024));
    for _ in 0..nm {
        let len = u16_at(bytes, &mut at)? as usize;
        if len > MAX_MATERIAL_NAME {
            return Err(bad("r5mesh material name is too long"));
        }
        let end = at.checked_add(len).ok_or_else(|| bad("r5mesh is truncated"))?;
        let name = std::str::from_utf8(bytes.get(at..end).ok_or_else(|| bad("r5mesh is truncated"))?)
            .map_err(|_| bad("r5mesh material name is not UTF-8"))?;
        at = end;
        materials.push(name.to_string());
    }
    Ok(EditMesh { positions, faces, corners, edge_flags, materials, smooth_angle })
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Serializes a mesh. Edge flags are emitted sorted, so the bytes are a
/// function of the content alone.
pub fn write(mesh: &EditMesh) -> Vec<u8> {
    let mut flags = mesh.edge_flags.clone();
    flags.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let mut out = Vec::with_capacity(32 + mesh.positions.len() * 24);
    out.extend_from_slice(MAGIC);
    push_u32(&mut out, VERSION);
    for n in [mesh.positions.len(), mesh.faces.len(), mesh.corners.len(), flags.len(), mesh.materials.len()] {
        push_u32(&mut out, n as u32);
    }
    for p in &mesh.positions {
        out.extend_from_slice(&p.x.to_le_bytes());
        out.extend_from_slice(&p.y.to_le_bytes());
        out.extend_from_slice(&p.z.to_le_bytes());
    }
    for f in &mesh.faces {
        push_u32(&mut out, f.first);
        push_u32(&mut out, f.len);
        out.extend_from_slice(&f.material.to_le_bytes());
        out.push(f.flags.0);
    }
    for c in &mesh.corners {
        push_u32(&mut out, c.vertex);
        out.extend_from_slice(&c.uv[0].to_le_bytes());
        out.extend_from_slice(&c.uv[1].to_le_bytes());
        match c.normal {
            Some(n) => {
                out.push(1);
                for v in n {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
            None => out.push(0),
        }
    }
    for (a, b, f) in &flags {
        push_u32(&mut out, *a);
        push_u32(&mut out, *b);
        out.push(f.0);
    }
    match mesh.smooth_angle {
        Some(a) => {
            out.push(1);
            out.extend_from_slice(&a.to_le_bytes());
        }
        None => out.push(0),
    }
    for m in &mesh.materials {
        let bytes = m.as_bytes();
        out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

/// The `mesh-dump` JSON: the whole mesh in reviewable form.
pub fn to_json(mesh: &EditMesh) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "positions": mesh.positions.iter().map(|p| [p.x, p.y, p.z]).collect::<Vec<_>>(),
        "faces": mesh.faces.iter().map(|f| serde_json::json!({
            "first": f.first, "len": f.len, "material": f.material, "smooth": f.flags.contains(FaceFlags::SMOOTH)
        })).collect::<Vec<_>>(),
        "corners": mesh.corners.iter().map(|c| serde_json::json!({
            "vertex": c.vertex, "uv": c.uv, "normal": c.normal
        })).collect::<Vec<_>>(),
        "edge_flags": mesh.edge_flags.iter().map(|(a, b, f)| serde_json::json!({
            "verts": [a, b], "sharp": f.contains(EdgeFlags::SHARP), "seam": f.contains(EdgeFlags::SEAM)
        })).collect::<Vec<_>>(),
        "materials": mesh.materials,
        "smooth_angle": mesh.smooth_angle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_mesh_round_trips() {
        let m = EditMesh::default();
        let back = read(&write(&m)).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn bad_magic_and_truncation_are_errors() {
        assert!(read(b"nope").is_err());
        let mut bytes = write(&EditMesh::default());
        bytes.truncate(bytes.len() - 1);
        assert!(read(&bytes).is_err());
    }
}
