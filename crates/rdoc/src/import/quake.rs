//! Quake / Valve 220 `.map` import: entity blocks of textured brush faces.
//!
//! A face is `( x y z ) ( x y z ) ( x y z ) TEXTURE offX offY rot scaleX scaleY`.
//! The three points wound counter-clockwise seen from outside give the outward
//! normal, and the texture axes follow the Valve 220 convention:
//! `uAxis = normalize(p0 - p1)`, `vAxis = normalize(cross(normal, uAxis))`.

use glam::DVec3;
use indexmap::IndexMap;

use crate::brush::{Brush, BrushFace, BrushError, Plane, Solid, UvMapping};
use crate::command::Command;
use crate::doc::{new_object, Document};
use crate::model::{Entity, KeyValues, Object, ObjectKind, Transform};

use super::tokens::{tokenize, Cursor, Tok};
use super::{entity_name, ImportError};

/// One parsed face of a brush.
#[derive(Clone, Debug, PartialEq)]
pub struct QFace {
    pub points: [DVec3; 3],
    pub texture: String,
    pub offset: [f64; 2],
    pub rotation: f64,
    pub scale: [f64; 2],
}

impl QFace {
    pub fn plane(&self) -> Result<Plane, ImportError> {
        let normal = (self.points[1] - self.points[0]).cross(self.points[2] - self.points[0]);
        if normal.length_squared() <= 1e-12 {
            return Err(ImportError::NotASolid("a face's three plane points are collinear".into()));
        }
        let normal = normal.normalize();
        Ok(Plane { normal, dist: normal.dot(self.points[0]) })
    }

    /// Valve 220 texture projection.
    pub fn uv(&self, normal: DVec3) -> UvMapping {
        let u = self.points[0] - self.points[1];
        let u_axis = if u.length_squared() > 1e-12 { u.normalize() } else { DVec3::X };
        let v = normal.cross(u_axis);
        let v_axis = if v.length_squared() > 1e-12 { v.normalize() } else { DVec3::NEG_Y };
        UvMapping {
            u_axis,
            v_axis,
            offset: self.offset,
            scale: [if self.scale[0].abs() > 1e-9 { self.scale[0] } else { 1.0 }, if self.scale[1].abs() > 1e-9 { self.scale[1] } else { 1.0 }],
            rotation: self.rotation,
        }
    }
}

/// A top-level block inside a `.map`: keyvalues plus the faces of any brushes
/// it contains.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QBlock {
    pub props: IndexMap<String, String>,
    pub faces: Vec<QFace>,
}

fn parse_face(cursor: &mut Cursor) -> Result<QFace, ImportError> {
    let mut points = [DVec3::ZERO; 3];
    for slot in points.iter_mut() {
        let v = cursor.vector(&Tok::LParen, &Tok::RParen)?;
        if v.len() != 3 {
            return Err(ImportError::Unsupported(format!("plane point has {} components", v.len())));
        }
        *slot = DVec3::new(v[0], v[1], v[2]);
    }
    let texture = match cursor.take()? {
        Tok::Ident(name) => name,
        Tok::Str(name) => name,
        other => return Err(ImportError::Unexpected(other.describe())),
    };
    let mut numbers: Vec<f64> = Vec::with_capacity(5);
    while numbers.len() < 5 {
        match cursor.peek() {
            Some(Tok::Num(n)) => {
                numbers.push(*n);
                cursor.pos += 1;
            }
            _ => break,
        }
    }
    if numbers.len() < 2 {
        return Err(ImportError::Unsupported("a face needs at least an offset pair".into()));
    }
    Ok(QFace {
        points,
        texture,
        offset: [numbers[0], numbers[1]],
        rotation: numbers.get(2).copied().unwrap_or(0.0),
        scale: [numbers.get(3).copied().unwrap_or(1.0), numbers.get(4).copied().unwrap_or(1.0)],
    })
}

fn parse_brush(cursor: &mut Cursor) -> Result<QBlock, ImportError> {
    let mut block = QBlock::default();
    loop {
        match cursor.take()? {
            Tok::RBrace => return Ok(block),
            Tok::LParen => {
                cursor.pos -= 1;
                block.faces.push(parse_face(cursor)?);
            }
            other => return Err(ImportError::Unexpected(other.describe())),
        }
    }
}

/// Parses the top-level entity blocks of a `.map` file.
pub fn parse(text: &str) -> Result<Vec<QBlock>, ImportError> {
    let mut cursor = Cursor::new(tokenize(text)?);
    let mut out = Vec::new();
    while cursor.peek().is_some() {
        cursor.expect(&Tok::LBrace)?;
        let mut block = QBlock::default();
        loop {
            match cursor.take()? {
                Tok::RBrace => break,
                Tok::Str(key) => match cursor.take()? {
                    Tok::Str(value) => {
                        block.props.insert(key, value);
                    }
                    other => return Err(ImportError::Unexpected(other.describe())),
                },
                Tok::LBrace => {
                    // A nested block holds faces; anything the format carries in
                    // that is not a face list simply has none.
                    let mut inner = parse_brush(&mut cursor)?;
                    block.faces.append(&mut inner.faces);
                }
                other => return Err(ImportError::Unexpected(other.describe())),
            }
        }
        out.push(block);
    }
    Ok(out)
}

fn faces_of(block: &QBlock) -> Result<Vec<BrushFace>, ImportError> {
    let mut out = Vec::with_capacity(block.faces.len());
    for face in &block.faces {
        let plane = face.plane()?;
        out.push(BrushFace {
            normal: plane.normal,
            dist: plane.dist,
            material: face.texture.clone(),
            uv: face.uv(plane.normal),
            lightmap_scale: 0.0,
        });
    }
    Ok(out)
}

/// Builds the solid and returns the faces with outward-facing normals, retrying
/// with every plane flipped when the brush is written the other way round.
pub fn orient_faces(faces: &[BrushFace]) -> Result<Vec<BrushFace>, ImportError> {
    if faces.len() < 4 {
        return Err(ImportError::NotASolid(format!("{} faces", faces.len())));
    }
    let planes: Vec<Plane> = faces.iter().map(Plane::from).collect();
    let solid = match Solid::from_planes(planes.clone()) {
        Ok(s) => s,
        Err(BrushError::NotConvex) => Solid::from_planes(planes.into_iter().map(Plane::flipped).collect())
            .map_err(|e| ImportError::NotASolid(e.to_string()))?,
        Err(e) => return Err(e.into()),
    };
    Ok(solid.to_faces(faces))
}

/// Turns a `.map` document into one batch of edits.
///
/// A block with a `classname` and faces is a brush entity, and its solids
/// become child `Brush` objects. The `worldspawn` block is the map's shell
/// rather than placed geometry, so it produces nothing.
pub fn import(doc: &Document, text: &str) -> Result<Command, ImportError> {
    let blocks = parse(text)?;
    let layer = doc
        .layer_ids()
        .first()
        .copied()
        .ok_or_else(|| ImportError::Unsupported("document has no layer".into()))?;
    let mut objects: Vec<Object> = Vec::new();
    let mut index = 0usize;
    for block in &blocks {
        let classname = block.props.get("classname").cloned();
        if classname.as_deref() == Some("worldspawn") {
            continue;
        }
        let faces = faces_of(block)?;
        let solids = match orient_faces(&faces) {
            Ok(faces) if !faces.is_empty() => {
                index += 1;
                vec![Brush { faces, contents: 0, detail: false }]
            }
            _ => Vec::new(),
        };
        let Some(classname) = classname else {
            // A block of faces with no classname is a loose brush.
            for solid in solids {
                objects.push(new_object(format!("brush_{index}"), layer, ObjectKind::Brush(solid)));
            }
            continue;
        };
        let keyvalues: KeyValues = block
            .props
            .iter()
            .filter(|(k, _)| k.as_str() != "classname")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let name = entity_name(&classname, &keyvalues, "entity");
        let mut object = new_object(
            name.clone(),
            layer,
            ObjectKind::Entity(Entity { classname, keyvalues, partition: None, links: Default::default() }),
        );
        if let Some(origin) = block.props.get("origin").map(String::as_str).map(parse_vec3).transpose()? {
            object.transform.position = origin;
        }
        if let Some(angles) = block.props.get("angles").map(String::as_str).map(parse_vec3).transpose()? {
            object.transform = Transform::from_source_angles(object.transform.position, angles);
        }
        let entity_id = object.id;
        objects.push(object);
        for (i, solid) in solids.into_iter().enumerate() {
            let mut child = new_object(format!("{name}_{i}"), layer, ObjectKind::Brush(solid));
            child.parent = Some(entity_id);
            objects.push(child);
        }
    }
    if objects.is_empty() {
        return Err(ImportError::Unsupported("the file contains no brushes or entities".into()));
    }
    Ok(Command::Batch { label: "import quake map".into(), commands: vec![Command::AddObjects { objects }] })
}

/// A `.map` keyvalue vector: three bare numbers, e.g. `"origin" "0 0 256"`.
fn parse_vec3(text: &str) -> Result<DVec3, ImportError> {
    let mut cursor = Cursor::new(tokenize(text)?);
    let mut v = Vec::with_capacity(3);
    while v.len() < 3 {
        match cursor.take()? {
            Tok::Num(n) => v.push(n),
            other => return Err(ImportError::Unexpected(other.describe())),
        }
    }
    Ok(DVec3::new(v[0], v[1], v[2]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, MemoryFileStore};
    use crate::model::MapDoc;

    const SMALL_MAP: &str = r#"
// a comment
{
    "classname" "worldspawn"
    "wad" "test.wad"
    {
        ( -64 -64 -64 ) ( -64 -63 -64 ) ( -63 -64 -64 ) SKY 0 0 0 1 1
        ( -64 -64 -64 ) ( -63 -64 -64 ) ( -63 -63 -64 ) SKY 0 0 0 1 1
        ( -64 -64 -64 ) ( -64 -63 -64 ) ( -64 -63 -63 ) SKY 0 0 0 1 1
        ( 64 64 64 ) ( 64 63 64 ) ( 63 64 64 ) SKY 0 0 0 1 1
        ( 64 64 64 ) ( 63 64 64 ) ( 63 63 64 ) SKY 0 0 0 1 1
        ( 64 64 64 ) ( 64 63 64 ) ( 64 63 63 ) SKY 0 0 0 1 1
    }
}
{
    "classname" "func_door"
    "origin" "0 0 0"
    "targetname" "door"
    {
        ( 0 0 0 ) ( 0 128 0 ) ( 128 128 0 ) METAL 16 32 0 0.5 0.25
        ( 0 0 64 ) ( 128 0 64 ) ( 128 128 64 ) METAL 16 32 0 0.5 0.25
        ( 0 0 0 ) ( 0 0 64 ) ( 0 128 64 ) METAL 16 32 0 0.5 0.25
        ( 128 0 0 ) ( 128 128 0 ) ( 128 128 64 ) METAL 16 32 0 0.5 0.25
        ( 0 0 0 ) ( 128 0 0 ) ( 128 0 64 ) METAL 16 32 0 0.5 0.25
        ( 0 128 0 ) ( 0 128 64 ) ( 128 128 64 ) METAL 16 32 0 0.5 0.25
    }
}
{
    "classname" "trigger_multiple"
    "origin" "256 0 0"
    "spawnflags" "1"
    {
        ( 0 0 0 ) ( 0 64 0 ) ( 64 64 0 ) TRIGGER 0 0 0 1 1
        ( 0 0 32 ) ( 64 0 32 ) ( 64 64 32 ) TRIGGER 0 0 0 1 1
        ( 0 0 0 ) ( 0 0 32 ) ( 0 64 32 ) TRIGGER 0 0 0 1 1
        ( 64 0 0 ) ( 64 64 0 ) ( 64 64 32 ) TRIGGER 0 0 0 1 1
        ( 0 0 0 ) ( 64 0 0 ) ( 64 0 32 ) TRIGGER 0 0 0 1 1
        ( 0 64 0 ) ( 0 64 32 ) ( 64 64 32 ) TRIGGER 0 0 0 1 1
    }
}
{
    "classname" "info_target"
    "targetname" "marker"
    "origin" "0 0 512"
}
"#;

    fn doc() -> Document {
        Document::new(MapDoc::default(), Box::new(MemoryFileStore::new()))
    }

    #[test]
    fn quake_map_import_small() {
        let mut d = doc();
        let (inverse, ids) = d.apply(import(&d, SMALL_MAP).unwrap()).unwrap();
        assert_eq!(ids.len(), 5, "three entities plus the two brush solids they own");
        let names: Vec<&str> = d.doc.objects.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["door", "door_0", "trigger_multiple", "trigger_multiple_0", "marker"]);

        let door = d.doc.objects.iter().find(|o| o.name == "door").unwrap();
        let ObjectKind::Entity(e) = &door.kind else { panic!("expected an entity") };
        assert_eq!(e.classname, "func_door");
        assert_eq!(e.keyvalues["targetname"], "door");
        assert_eq!(e.keyvalues.get("classname"), None);
        let children = d.children(door.id);
        assert_eq!(children.len(), 1);
        let ObjectKind::Brush(b) = &d.object(children[0]).unwrap().kind else { panic!("expected a brush") };
        assert_eq!(b.faces.len(), 6);
        assert!((Solid::from_faces(&b.faces).unwrap().volume() - 128.0 * 128.0 * 64.0).abs() < 1e-6);
        assert!(b.faces.iter().all(|f| f.material == "METAL"));

        let marker = d.doc.objects.iter().find(|o| o.name == "marker").unwrap();
        assert_eq!(marker.transform.position, DVec3::new(0.0, 0.0, 512.0));

        d.apply(inverse).unwrap();
        assert!(d.doc.objects.is_empty());
    }

    #[test]
    fn quake_uv_matches_a_hand_computed_value() {
        let face = QFace {
            points: [DVec3::new(0.0, 0.0, 0.0), DVec3::new(0.0, 128.0, 0.0), DVec3::new(128.0, 128.0, 0.0)],
            texture: "METAL".into(),
            offset: [16.0, 32.0],
            rotation: 0.0,
            scale: [0.5, 0.25],
        };
        let plane = face.plane().unwrap();
        assert_eq!(plane.normal, DVec3::NEG_Z);
        assert_eq!(plane.dist, 0.0);
        let uv = face.uv(plane.normal);
        // p0 - p1 is (0,-128,0), so uAxis is -Y and vAxis is -Z x -Y = -X.
        assert_eq!(uv.u_axis, DVec3::NEG_Y);
        assert_eq!(uv.v_axis, DVec3::NEG_X);
        let p = DVec3::new(64.0, 32.0, 8.0);
        let u = p.dot(uv.u_axis) / uv.scale[0] + uv.offset[0];
        let v = p.dot(uv.v_axis) / uv.scale[1] + uv.offset[1];
        assert!((u - (-48.0)).abs() < 1e-9, "u was {u}");
        assert!((v - (-224.0)).abs() < 1e-9, "v was {v}");
    }

    #[test]
    fn quake_brushes_written_inward_still_import() {
        let map = "{\n\"classname\" \"info_target\"\n\"targetname\" \"t\"\n{\n\
            ( 0 0 0 ) ( 64 0 0 ) ( 64 64 0 ) TEX 0 0 0 1 1\n\
            ( 0 0 64 ) ( 64 64 64 ) ( 64 0 64 ) TEX 0 0 0 1 1\n\
            ( 0 0 0 ) ( 0 64 64 ) ( 0 0 64 ) TEX 0 0 0 1 1\n\
            ( 64 0 0 ) ( 64 64 64 ) ( 64 64 0 ) TEX 0 0 0 1 1\n\
            ( 0 0 0 ) ( 64 0 64 ) ( 64 0 0 ) TEX 0 0 0 1 1\n\
            ( 0 64 0 ) ( 64 64 64 ) ( 0 64 64 ) TEX 0 0 0 1 1\n\
        }\n}\n";
        let mut d = doc();
        d.apply(import(&d, map).unwrap()).unwrap();
        let door = &d.doc.objects[0];
        let children = d.children(door.id);
        let ObjectKind::Brush(b) = &d.object(children[0]).unwrap().kind else { panic!("expected a brush") };
        let volume = Solid::from_faces(&b.faces).unwrap().volume();
        assert!((volume - 64.0 * 64.0 * 64.0).abs() < 1e-6, "volume was {volume}");
    }

    #[test]
    fn quake_geometry_blocks_that_carry_no_faces_are_skipped() {
        let map = "{\n\"classname\" \"info_target\"\n\"targetname\" \"t\"\n}\n";
        let mut d = doc();
        d.apply(import(&d, map).unwrap()).unwrap();
        assert_eq!(d.doc.objects.len(), 1);
    }

    #[test]
    fn malformed_map_text_is_rejected_without_a_panic() {
        let d = doc();
        for bad in ["", "{", "{ \"classname\" }", "{ ( 1 2 3 ) }", "{ \"a\" \"b\" ", "{ x }"] {
            assert!(import(&d, bad).is_err(), "{bad:?} should not import");
        }
    }
}

