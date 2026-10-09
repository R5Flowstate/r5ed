import { describe, expect, it } from "vitest";
import type { Vec3 } from "../core/api";
import {
  QUAT_IDENTITY,
  mat3FromQuat,
  quatFromAxisAngle,
  quatFromSourceAngles,
  quatMul,
  quatRotate,
  sourceAnglesFromQuat,
} from "./quat";
import { fromRotationTranslationScale, mat4, transformDirection } from "./mat4";
import { cross, normalize, scale } from "./vec3";

/** Rz(yaw) * Ry(pitch) * Rx(roll) in row-major order, built numerically. */
function reference(yaw: number, pitch: number, roll: number): number[] {
  const rad = Math.PI / 180;
  const c = (t: number) => Math.cos(t * rad);
  const s = (t: number) => Math.sin(t * rad);
  const c1 = c(yaw), s1 = s(yaw), c2 = c(pitch), s2 = s(pitch), c3 = c(roll), s3 = s(roll);
  return [
    c1 * c2, s2 * c1 * s3 - s1 * c3, s2 * c1 * c3 + s1 * s3,
    s1 * c2, s2 * s1 * s3 + c1 * c3, s2 * s1 * c3 - c1 * s3,
    -s2, c2 * s3, c2 * c3,
  ];
}

const CASES: Vec3[] = [
  [0, 0, 0],
  [0, 90, 0],
  [90, 0, 0],
  [0, 0, 90],
  [30, 45, 20],
  [-12.5, 179, -88],
  [10, -70, 55],
];

/** Close enough to the pole that yaw and roll are no longer separable. */
const NEAR_LOCK: Vec3[] = [[89.99, 12, -30], [-89.99, -170, 20]];

describe("source angles", () => {
  it("builds Rz(yaw) * Ry(pitch) * Rx(roll)", () => {
    const scratch = new Float64Array(9);
    for (const [pitch, yaw, roll] of [...CASES, ...NEAR_LOCK]) {
      const got = Array.from(mat3FromQuat(quatFromSourceAngles([pitch, yaw, roll]), scratch));
      const want = reference(yaw, pitch, roll);
      got.forEach((n, i) => expect(n).toBeCloseTo(want[i], 5));
    }
  });

  it("round-trips angles through the quaternion away from the pole", () => {
    for (const angles of CASES) {
      const back = sourceAnglesFromQuat(quatFromSourceAngles(angles));
      back.forEach((n, i) => expect(n).toBeCloseTo(angles[i], 6));
    }
  });

  it("clamps to the pole with roll zero, where yaw and roll are not separable", () => {
    for (const angles of NEAR_LOCK) {
      const back = sourceAnglesFromQuat(quatFromSourceAngles(angles));
      expect(back[0]).toBeCloseTo(angles[0], 2);
      expect(back[2]).toBe(0);
      const surviving = angles[0] > 0 ? angles[1] - angles[2] : angles[1] + angles[2];
      expect(back[1]).toBeCloseTo(surviving, 2);
    }
  });

  it("round-trips a quaternion through angles and back", () => {
    for (const angles of CASES) {
      const q = quatFromSourceAngles(angles);
      const again = quatFromSourceAngles(sourceAnglesFromQuat(q));
      const rotated = quatRotate(again, [1, 0, 0]);
      const expected = quatRotate(q, [1, 0, 0]);
      rotated.forEach((n, i) => expect(n).toBeCloseTo(expected[i], 6));
    }
  });

  it("turns local +X into the Source forward vector", () => {
    // Source's AngleVectors: forward = (cos p cos y, cos p sin y, -sin p).
    for (const [pitch, yaw] of [[0, 0], [0, 90], [45, 0], [45, 30], [-30, 120]] as [number, number][]) {
      const f = quatRotate(quatFromSourceAngles([pitch, yaw, 0]), [1, 0, 0]);
      const rad = Math.PI / 180;
      expect(f[0]).toBeCloseTo(Math.cos(pitch * rad) * Math.cos(yaw * rad), 6);
      expect(f[1]).toBeCloseTo(Math.cos(pitch * rad) * Math.sin(yaw * rad), 6);
      expect(f[2]).toBeCloseTo(-Math.sin(pitch * rad), 6);
    }
  });
});

describe("quaternion algebra", () => {
  it("rotates local X onto world Y for a 90 degree yaw", () => {
    const y = quatRotate(quatFromAxisAngle([0, 0, 1], Math.PI / 2), [1, 0, 0]);
    expect(y[0]).toBeCloseTo(0, 6);
    expect(y[1]).toBeCloseTo(1, 6);
  });

  it("composes rotations in application order", () => {
    const a = quatFromAxisAngle([0, 0, 1], Math.PI / 2);
    const b = quatFromAxisAngle([0, 1, 0], Math.PI / 2);
    const viaQuat = quatRotate(quatMul(b, a), [1, 0, 0]);
    const viaSteps = quatRotate(b, quatRotate(a, [1, 0, 0]));
    viaQuat.forEach((n, i) => expect(n).toBeCloseTo(viaSteps[i], 6));
  });

  it("keeps the length of the rotated vector", () => {
    const q = quatFromSourceAngles([23, -47, 12]);
    for (const v of [[1, 0, 0], [0, 1, 0], [0, 0, 1], [1, 2, 3], [-4, 0.5, 2]] as Vec3[]) {
      const r = quatRotate(q, v);
      expect(Math.hypot(r[0], r[1], r[2])).toBeCloseTo(Math.hypot(v[0], v[1], v[2]), 6);
    }
  });

  it("agrees with the 4x4 transform", () => {
    const q = quatFromSourceAngles([15, 33, -8]);
    const v: Vec3 = [12, -7, 41];
    const m = fromRotationTranslationScale(mat4(), q, [0, 0, 0], [1, 1, 1]);
    const viaMat = transformDirection(m, v);
    const viaQuat = quatRotate(q, v);
    viaMat.forEach((n, i) => expect(n).toBeCloseTo(viaQuat[i], 5));
  });

  it("treats the identity quaternion as no rotation", () => {
    const v: Vec3 = [3, -4, 5];
    const r = quatRotate(QUAT_IDENTITY, v);
    r.forEach((n, i) => expect(n).toBe(v[i]));
  });
});

describe("axis angle basis", () => {
  it("is right-handed about every axis", () => {
    for (const [axis, base] of [
      [[1, 0, 0] as Vec3, [0, 1, 0] as Vec3],
      [[0, 1, 0] as Vec3, [0, 0, 1] as Vec3],
      [[0, 0, 1] as Vec3, [1, 0, 0] as Vec3],
    ] as [Vec3, Vec3][]) {
      // A positive 90 degree turn about the axis maps the next axis of the cycle.
      const r = quatRotate(quatFromAxisAngle(axis, Math.PI / 2), base);
      const want = cross(axis, base);
      r.forEach((n, i) => expect(n).toBeCloseTo(want[i], 6));
      // ... and leaves the axis itself alone.
      const along = quatRotate(quatFromAxisAngle(axis, Math.PI / 2), axis);
      along.forEach((n, i) => expect(n).toBeCloseTo(normalize(axis)[i], 6));
      void scale;
    }
  });
});
