import { describe, expect, it } from "vitest";
import { fromRotationTranslationScale, identity, invert, lookAt, mat4, multiply, ortho, perspective, projectPoint, quatToAxisAngle, transformDirection, transformFromQuatPosition, transformPoint, transpose, unproject, writeInstance } from "./mat4";
import { QUAT_IDENTITY, quat, quatConjugate, quatDot, quatFromAxisAngle, quatMul, quatNormalize, quatRotate, quatSlerp, quatLength, sourceAnglesFromQuat, quatFromSourceAngles } from "./quat";
import { distanceToAxis, planeFromThree, ray, rayAabb, rayAt, rayPlane, rayTriangle, reflectDir } from "./ray";
import {
  EPS,
  aabbContains,
  aabbOverlaps,
  add,
  aabbCenter,
  aabbSize,
  clamp,
  cross,
  distance,
  dot,
  emptyAabb,
  expandAabb,
  finite3,
  growAabb,
  isEmptyAabb,
  length,
  lengthSq,
  lerp3,
  max3,
  min3,
  mul,
  neg,
  normalize,
  scale,
  sub,
  unionAabb,
  v3,
} from "./vec3";
import type { Vec3 } from "../core/api";
import type { Aabb } from "./vec3";

const near = (a: number, b: number, d = 1e-5): boolean => Math.abs(a - b) <= d;
const close = (a: Vec3, b: Vec3, d = 1e-5): boolean => near(a[0], b[0], d) && near(a[1], b[1], d) && near(a[2], b[2], d);

describe("vec3", () => {
  it("builds, negates and multiplies", () => {
    expect(v3(1, 2, 3)).toEqual([1, 2, 3]);
    expect(neg([1, -2, 3])).toEqual([-1, 2, -3]);
    expect(mul([1, 2, 3], [4, 5, 6])).toEqual([4, 10, 18]);
  });

  it("adds, subtracts, scales and dots", () => {
    expect(add([1, 2, 3], [4, 5, 6])).toEqual([5, 7, 9]);
    expect(sub([4, 5, 6], [1, 2, 3])).toEqual([3, 3, 3]);
    expect(scale([1, 2, 3], 2)).toEqual([2, 4, 6]);
    expect(dot([1, 0, 0], [0, 1, 0])).toBe(0);
    expect(dot([1, 2, 3], [4, 5, 6])).toBe(32);
  });

  it("crosses right-handed", () => {
    expect(cross([1, 0, 0], [0, 1, 0])).toEqual([0, 0, 1]);
    expect(cross([0, 1, 0], [1, 0, 0])).toEqual([0, 0, -1]);
  });

  it("measures length and distance", () => {
    expect(length([3, 4, 0])).toBe(5);
    expect(lengthSq([3, 4, 0])).toBe(25);
    expect(distance([0, 0, 0], [1, 2, 2])).toBe(3);
  });

  it("normalizes, falling back when the vector is degenerate", () => {
    expect(normalize([0, 5, 0])).toEqual([0, 1, 0]);
    expect(normalize([0, 0, 0], [0, 0, 1])).toEqual([0, 0, 1]);
    expect(length(normalize([1, 1, 1]))).toBeCloseTo(1, 6);
  });

  it("interpolates, clamps and takes component minima", () => {
    expect(lerp3([0, 0, 0], [2, 4, 6], 0.5)).toEqual([1, 2, 3]);
    expect(lerp3([0, 0, 0], [2, 4, 6], 0)).toEqual([0, 0, 0]);
    expect(lerp3([0, 0, 0], [2, 4, 6], 1)).toEqual([2, 4, 6]);
    expect(clamp(5, 0, 1)).toBe(1);
    expect(clamp(-5, 0, 1)).toBe(0);
    expect(clamp(0.5, 0, 1)).toBe(0.5);
    expect(min3([1, 5, 2], [3, 4, 6])).toEqual([1, 4, 2]);
    expect(max3([1, 5, 2], [3, 4, 6])).toEqual([3, 5, 6]);
  });

  it("reports whether every component is finite", () => {
    expect(finite3([1, 2, 3])).toBe(true);
    expect(finite3([1, Number.NaN, 3])).toBe(false);
    expect(finite3([Number.POSITIVE_INFINITY, 0, 0])).toBe(false);
    expect(EPS).toBeGreaterThan(0);
  });
});

describe("aabb", () => {
  const box: Aabb = { min: [-1, -1, -1], max: [1, 1, 1] };

  it("starts empty and unions into a real box", () => {
    expect(isEmptyAabb(emptyAabb())).toBe(true);
    const one = unionAabb(emptyAabb(), box);
    expect(one).toEqual(box);
    expect(isEmptyAabb(one)).toBe(false);
  });

  it("centres, sizes, grows and expands", () => {
    expect(aabbCenter(box)).toEqual([0, 0, 0]);
    expect(aabbSize(box)).toEqual([2, 2, 2]);
    expect(growAabb(box, [-4, 0, 3])).toEqual({ min: [-4, -1, -1], max: [1, 1, 3] });
    expect(expandAabb(box, 2)).toEqual({ min: [-3, -3, -3], max: [3, 3, 3] });
  });

  it("tests containment and overlap", () => {
    expect(aabbContains(box, [0, 0, 0])).toBe(true);
    expect(aabbContains(box, [1, 0, 0])).toBe(true);
    expect(aabbContains(box, [1.5, 0, 0])).toBe(false);
    expect(aabbOverlaps(box, { min: [0, 0, 0], max: [2, 2, 2] })).toBe(true);
    expect(aabbOverlaps(box, { min: [2, 2, 2], max: [3, 3, 3] })).toBe(false);
  });
});

describe("mat4", () => {
  it("multiplies by the identity and by a scale", () => {
    const a = mat4();
    const s = fromRotationTranslationScale(mat4(), QUAT_IDENTITY, [1, 2, 3], [2, 2, 2]);
    expect([...multiply(mat4(), a, a)]).toEqual([...a]);
    const p = transformPoint(multiply(mat4(), a, s), [1, 0, 0]);
    expect(close([p[0], p[1], p[2]] as Vec3, [3, 2, 3])).toBe(true);
  });

  it("builds a transform from a rotation, a position and a scale", () => {
    const spin = quatFromAxisAngle([0, 0, 1], Math.PI / 2);
    const m = fromRotationTranslationScale(mat4(), spin, [1, 0, 0], [1, 1, 1]);
    const p = transformPoint(m, [1, 0, 0]);
    expect(close([p[0], p[1], p[2]] as Vec3, [1, 1, 0], 1e-5)).toBe(true);
    const flat = transformFromQuatPosition(mat4(), spin, [1, 0, 0]);
    expect([...flat]).toEqual([...m]);
  });

  it("transforms a direction by dropping the perspective divide", () => {
    const m = fromRotationTranslationScale(mat4(), QUAT_IDENTITY, [10, 10, 10], [2, 2, 2]);
    expect(close(transformDirection(m, [1, 0, 0]), [12, 10, 10])).toBe(true);
    const proj = perspective(mat4(), Math.PI / 4, 1, 1, 100);
    const p = transformPoint(proj, [10, 0, -10]);
    const d = transformDirection(proj, [10, 0, -10]);
    expect(close(d, [p[0], p[1], p[2]], 1e-4)).toBe(true);
    expect(near(p[3], 10, 1e-4)).toBe(true);
  });

  it("inverts a transform and reports a singular one", () => {
    const m = transformFromQuatPosition(mat4(), QUAT_IDENTITY, [3, 4, 5]);
    const inv = invert(mat4(), m);
    expect(inv).not.toBeNull();
    if (inv) {
      const p = transformPoint(inv, [3, 4, 5]);
      expect(close([p[0], p[1], p[2]] as Vec3, [0, 0, 0], 1e-4)).toBe(true);
    }
    expect(invert(mat4(), identity(mat4()))).not.toBeNull();
    const flat = identity(mat4());
    flat[0] = 0;
    flat[5] = 0;
    flat[10] = 0;
    flat[15] = 0;
    expect(invert(mat4(), flat)).toBeNull();
  });

  it("transposes a matrix", () => {
    const m = identity(mat4());
    m[1] = 5;
    const t = transpose(mat4(), m);
    expect(t[1]).toBe(0);
    expect(t[4]).toBe(5);
  });

  it("projects a world point through a view-projection", () => {
    const proj = perspective(mat4(), Math.PI / 4, 1, 1, 100);
    const view = identity(mat4());
    view[14] = -10;
    const vp = multiply(mat4(), proj, view);
    const clip = projectPoint(vp, [0, 0, -10]);
    expect(clip).not.toBeNull();
    if (clip) {
      expect(near(clip[0], 0, 1e-3)).toBe(true);
      expect(near(clip[1], 0, 1e-3)).toBe(true);
    }
    // A point behind the eye is not in front of the camera.
    expect(projectPoint(vp, [0, 0, 10])).toBeNull();
  });

  it("builds an orthographic projection of the given half extents", () => {
    const m = ortho(mat4(), 100, 50, 1, 1000);
    const p = transformPoint(m, [100, 50, -1]);
    expect(near(p[0], 1, 1e-6)).toBe(true);
    expect(near(p[1], 1, 1e-6)).toBe(true);
    const q = transformPoint(m, [0, 0, -1000]);
    expect(near(q[2], 1, 1e-2)).toBe(true);
    const r = transformPoint(m, [0, 0, -1]);
    expect(near(r[2], 0, 1e-2)).toBe(true);
  });

  it("looks at a target from an eye", () => {
    const view = lookAt(mat4(), [0, -100, 0], [0, 0, 0], [0, 0, 1]);
    // The target lands on the view axis, so its x and y are zero.
    const p = transformPoint(view, [0, 0, 0]);
    expect(near(p[0], 0, 1e-4)).toBe(true);
    expect(near(p[1], 0, 1e-4)).toBe(true);
    expect(near(p[2], -100, 1e-3)).toBe(true);
  });

  it("unprojects the screen centre onto the view axis", () => {
    const proj = perspective(mat4(), Math.PI / 4, 1, 1, 100);
    const view = lookAt(mat4(), [0, -100, 0], [0, 0, 0], [0, 0, 1]);
    const inv = invert(mat4(), multiply(mat4(), proj, view));
    expect(inv).not.toBeNull();
    if (inv) {
      const { origin, dir } = unproject(inv, 0, 0);
      // The centre of the screen looks straight at the target, so the ray
      // starts on the eye's side of it and points along +Y.
      expect(near(origin[0], 0, 1e-3)).toBe(true);
      expect(near(origin[1], -99, 1e-1)).toBe(true);
      expect(close(dir, [0, 1, 0], 1e-3)).toBe(true);
    }
  });

  it("turns a quaternion back into an axis and an angle", () => {
    const q = quatFromAxisAngle([0, 0, 1], Math.PI / 2);
    const { axis, angle } = quatToAxisAngle(q);
    expect(close(axis, [0, 0, 1], 1e-4)).toBe(true);
    expect(near(angle, Math.PI / 2, 1e-4)).toBe(true);
    // The identity has no meaningful axis, so it reports a zero angle.
    expect(quatToAxisAngle(QUAT_IDENTITY).angle).toBe(0);
    const back = quatFromAxisAngle(quatToAxisAngle(q).axis, quatToAxisAngle(q).angle);
    expect(near(quatDot(back, q), 1, 1e-6)).toBe(true);
  });

  it("writes an instance transform at an offset", () => {
    const out = new Float32Array(32);
    writeInstance(out, 16, identity(mat4()));
    expect(out[16]).toBe(1);
    expect(out[0]).toBe(0);
  });
});

describe("quat", () => {
  it("multiplies and conjugates back to the identity", () => {
    const a = quatFromAxisAngle([0, 0, 1], 0.5);
    const b = quatFromAxisAngle([1, 0, 0], 0.25);
    const product = quatMul(a, b);
    expect(near(quatLength(product), 1, 1e-6)).toBe(true);
    expect(near(quatDot(quatMul(a, quatConjugate(a)), QUAT_IDENTITY), 1, 1e-6)).toBe(true);
    expect(near(quatLength(a), 1, 1e-6)).toBe(true);
  });

  it("rotates a vector about an axis", () => {
    const spin = quatFromAxisAngle([0, 0, 1], Math.PI / 2);
    expect(close(quatRotate(spin, [1, 0, 0]), [0, 1, 0], 1e-6)).toBe(true);
    expect(close(quatRotate(QUAT_IDENTITY, [1, 2, 3]), [1, 2, 3])).toBe(true);
  });

  it("slerps along the short arc and normalises", () => {
    const a = quatFromAxisAngle([0, 0, 1], 0);
    const b = quatFromAxisAngle([0, 0, 1], Math.PI / 2);
    const half = quatSlerp(a, b, 0.5);
    expect(near(quatLength(half), 1, 1e-6)).toBe(true);
    expect(close(quatRotate(half, [1, 0, 0]), [Math.SQRT1_2, Math.SQRT1_2, 0], 1e-5)).toBe(true);
    expect(near(quatDot(quatSlerp(a, b, 0), a), 1, 1e-6)).toBe(true);
    expect(quatNormalize([0, 0, 0, 0])).toEqual(quat());
  });

  it("round-trips the source angle convention", () => {
    for (const angles of [[0, 0, 0], [30, 45, 60], [-89, 180, 15], [12.5, -33, 7]] as [number, number, number][]) {
      const q = quatFromSourceAngles(angles);
      const back = sourceAnglesFromQuat(q);
      // Yaw wraps, so compare modulo a full turn.
      const same = angles.every((a, i) => {
        const diff = ((back[i] - a) % 360 + 540) % 360 - 180;
        return Math.abs(diff) < 1e-3;
      });
      expect(same, angles.join(",")).toBe(true);
    }
  });
});

describe("ray", () => {
  it("normalizes its direction and walks along it", () => {
    const r = ray([0, 0, 0], [0, 0, -5]);
    expect(close(r.dir, [0, 0, -1])).toBe(true);
    expect(close(rayAt(r, 3), [0, 0, -3])).toBe(true);
  });

  it("intersects a box and reports the miss", () => {
    const r = ray([0, 0, 5], [0, 0, -1]);
    const hit = rayAabb(r, [-1, -1, -1], [1, 1, 1]);
    expect(hit).not.toBeNull();
    if (hit) expect(near(hit.tmin, 4, 1e-5)).toBe(true);
    expect(rayAabb(ray([5, 5, 5], [1, 0, 0]), [-1, -1, -1], [1, 1, 1])).toBeNull();
  });

  it("intersects a plane and reports the parallel case", () => {
    const r = ray([0, 0, 5], [0, 0, -1]);
    expect(near(rayPlane(r, [0, 0, 0], [0, 0, 1]) ?? -1, 5, 1e-5)).toBe(true);
    expect(rayPlane(ray([0, 0, 5], [1, 0, 0]), [0, 0, 0], [0, 0, 1])).toBeNull();
  });

  it("finds the plane through three points", () => {
    const [normal, dist] = planeFromThree([0, 0, 0], [1, 0, 0], [0, 1, 0]);
    expect(close(normal, [0, 0, 1], 1e-6)).toBe(true);
    expect(near(dist, 0, 1e-6)).toBe(true);
    const shifted = planeFromThree([0, 0, 5], [1, 0, 5], [0, 1, 5]);
    expect(near(shifted[1], 5, 1e-6)).toBe(true);
    // Three collinear points span no plane, so the fallback is +Z.
    expect(close(planeFromThree([0, 0, 0], [1, 1, 1], [2, 2, 2])[0], [0, 0, 1])).toBe(true);
  });

  it("measures the distance from a point to an axis line", () => {
    expect(near(distanceToAxis([0, 5, 0], [0, 0, 0], [0, 0, 1]), 5, 1e-5)).toBe(true);
    // A point on the line has no distance to it at all.
    expect(near(distanceToAxis([0, 0, 7], [0, 0, 0], [0, 0, 1]), 0, 1e-5)).toBe(true);
    expect(near(distanceToAxis([3, 4, 0], [0, 0, 0], [1, 0, 0]), 4, 1e-5)).toBe(true);
  });

  it("intersects a triangle by the Moller-Trumbore test", () => {
    const r = ray([0.25, 0.25, 1], [0, 0, -1]);
    expect(near(rayTriangle(r, [0, 0, 0], [1, 0, 0], [0, 1, 0]) ?? -1, 1, 1e-5)).toBe(true);
    expect(rayTriangle(ray([5, 5, 1], [0, 0, -1]), [0, 0, 0], [1, 0, 0], [0, 1, 0])).toBeNull();
    // A ray parallel to the triangle's plane never hits it.
    expect(rayTriangle(ray([0.25, 0.25, 0], [1, 0, 0]), [0, 0, 0], [1, 0, 0], [0, 1, 0])).toBeNull();
  });

  it("reflects a direction about a normal", () => {
    const out = reflectDir([0, 0, -1], [0, 0, 1]);
    expect(close(out, [0, 0, 1], 1e-6)).toBe(true);
    // A direction along the normal is unchanged in magnitude and stays finite.
    const head = reflectDir([0, 0, 1], [0, 0, 1]);
    expect(Number.isFinite(head[0])).toBe(true);
  });
});
