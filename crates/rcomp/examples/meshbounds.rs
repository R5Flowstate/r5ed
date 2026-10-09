use std::path::Path;
use rbsp::{ids, Version};
fn main() {
    for p in std::env::args().skip(1) {
        let bsp = rbsp::Bsp::read_path(Path::new(&p)).expect("bsp");
        let runs = rcomp::meshes::decode_meshes(&bsp).expect("meshes");
        let recs = rbsp::lumps::render::parse_mesh_bounds(bsp.lump(ids::MESH_BOUNDS), Version::ClientV51).expect("bounds");
        println!("{p}: {} meshes, {} bounds", runs.len(), recs.len());
        for (i, (run, r)) in runs.iter().zip(&recs).enumerate() {
            if run.positions.is_empty() { continue; }
            let o = glam::Vec3::from_array(r.origin);
            let e = glam::Vec3::from_array(r.extents);
            let (mut lo, mut hi) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
            let mut far = 0f32;
            for q in &run.positions { lo = lo.min(*q); hi = hi.max(*q); far = far.max(q.distance(o)); }
            let out_box = (lo.cmplt(o - e - 1.0).any()) || (hi.cmpgt(o + e + 1.0).any());
            let flag = if far > r.radius + 0.5 || out_box { "BAD" } else { "ok" };
            if flag == "BAD" || i < 3 {
                println!("  mesh {i} {flag} verts {} box {:?}..{:?} rec o {:?} e {:?} r {} tan_yaw {} far {}", run.positions.len(), lo, hi, r.origin, r.extents, r.radius, r.tan_yaw, far);
            }
        }
    }
}
