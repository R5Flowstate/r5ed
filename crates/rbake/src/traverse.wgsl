// Lightmap texel visibility, one ray per texel, against the same binned-SAH
// hierarchy the CPU walks. A short stack of 64 nodes is enough for a tree built
// with four triangles a leaf.

const STACK: u32 = 64u;
const EPSILON: f32 = 0.01;

struct Node {
    lo: vec3<f32>,
    hi: vec3<f32>,
    left_or_first: u32,
    count: u32,
};

struct Tri {
    a: vec3<f32>,
    b: vec3<f32>,
    c: vec3<f32>,
};

@group(0) @binding(0) var<storage, read> nodes: array<Node>;
@group(0) @binding(1) var<storage, read> tris: array<Tri>;
@group(0) @binding(2) var<storage, read> origins: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> visibility: array<f32>;
@group(0) @binding(4) var<uniform> dir: vec4<f32>;

fn box_hit(origin: vec3<f32>, d: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>) -> bool {
    let inv = 1.0 / d;
    let t0 = (lo - origin) * inv;
    let t1 = (hi - origin) * inv;
    let a = min(t0, t1);
    let b = max(t0, t1);
    let lo_hit = max(a.x, max(a.y, a.z));
    let hi_hit = min(b.x, min(b.y, b.z));
    return hi_hit >= max(lo_hit, EPSILON);
}

fn tri_hit(origin: vec3<f32>, d: vec3<f32>, t: Tri) -> bool {
    let e1 = t.b - t.a;
    let e2 = t.c - t.a;
    let p = cross(d, e2);
    let det = dot(e1, p);
    if abs(det) < 1e-12 {
        return false;
    }
    let inv = 1.0 / det;
    let s = origin - t.a;
    let u = dot(s, p) * inv;
    if u < 0.0 || u > 1.0 {
        return false;
    }
    let q = cross(s, e1);
    let v = dot(d, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return false;
    }
    return dot(e2, q) * inv > EPSILON;
}

fn occluded(origin: vec3<f32>, d: vec3<f32>) -> bool {
    var stack: array<u32, 64>;
    var sp: u32 = 0u;
    stack[sp] = 0u;
    sp = sp + 1u;
    loop {
        if sp == 0u {
            return false;
        }
        sp = sp - 1u;
        let n = nodes[stack[sp]];
        if (!box_hit(origin, d, n.lo, n.hi)) {
            continue;
        }
        if (n.count != 0u) {
            for (var i: u32 = 0u; i < n.count; i = i + 1u) {
                if (tri_hit(origin, d, tris[n.left_or_first + i])) {
                    return true;
                }
            }
            continue;
        }
        if (sp + 2u > STACK) {
            return false;
        }
        stack[sp] = n.left_or_first;
        stack[sp + 1u] = n.left_or_first + 1u;
        sp = sp + 2u;
    }
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= arrayLength(&origins)) {
        return;
    }
    let o = origins[i].xyz;
    let blocked = occluded(o, dir.xyz);
    if (i < arrayLength(&visibility)) {
        visibility[i] = select(1.0, 0.0, blocked);
    }
}
