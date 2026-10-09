use std::path::Path;
fn main() {
    for p in std::env::args().skip(1) {
        let bsp = rbsp::Bsp::read_path(Path::new(&p)).expect("bsp");
        let models = rcomp::collision::decode_collision(&bsp).expect("decode");
        let (mut up, mut down, mut n) = (0u64, 0u64, 0u64);
        for (m, tris) in &models {
            if *m != 0 { continue; }
            for t in tris {
                n += 1;
                let e1 = t[1] - t[0];
                let e2 = t[2] - t[0];
                let c = e1.cross(e2); // CCW normal
                let l = c.length();
                if l < 1e-9 { continue; }
                let z = c.z / l;
                if z > 0.7 { up += 1 } else if z < -0.7 { down += 1 }
            }
        }
        println!("{p}: model0 tris {n}, flat CCW-normal up {up}, down {down}");
    }
}
