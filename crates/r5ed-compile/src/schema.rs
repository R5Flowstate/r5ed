//! `schema`: which entity schema this build carries.
//!
//! The schema is a generated file, so what identifies it is the digest of its
//! bytes; a build whose digest differs is a build with different entity forms.

use anyhow::Result;
use serde_json::json;

use crate::bake::digest;

/// The schema this build was compiled against.
const SCHEMA: &str = include_str!("../../../schema/entities.json");

/// Prints the schema digest and what it holds. The schema is embedded, so the
/// path names the file inside the repo; the build machine's copy is meaningless
/// anywhere else.
pub fn run() -> Result<serde_json::Value> {
    let schema = rdoc::schema::EntitySchema::load(SCHEMA).map_err(|e| anyhow::anyhow!("{e}"))?;
    let keys: usize = schema.classes.values().map(|c| c.keys.len()).sum();
    let kind = |want: rdoc::schema::ClassKind| {
        schema.classes.values().filter(|c| c.kind == want).count()
    };
    Ok(json!({
        "path": "schema/entities.json",
        "digest": digest(SCHEMA.as_bytes()),
        "bytes": SCHEMA.len(),
        "classes": schema.classes.len(),
        "keys": keys,
        "point_classes": kind(rdoc::schema::ClassKind::Point),
        "brush_classes": kind(rdoc::schema::ClassKind::Brush),
        "both_classes": kind(rdoc::schema::ClassKind::Both),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_the_build_carries_names_every_factory() {
        let value = run().expect("the bundled schema parses");
        assert!(value["classes"].as_u64().expect("a class count") > 200, "{value}");
        assert!(value["keys"].as_u64().expect("a key count") > value["classes"].as_u64().unwrap());
        assert_eq!(value["digest"].as_str().expect("a digest").len(), 16);
        assert_eq!(value["bytes"].as_u64().expect("a size"), SCHEMA.len() as u64);
        assert!(value["point_classes"].as_u64().unwrap() > 0);
        assert!(value["brush_classes"].as_u64().unwrap() > 0);
        assert!(value["path"].as_str().expect("a path").ends_with("schema/entities.json"), "{value}");
    }
}
