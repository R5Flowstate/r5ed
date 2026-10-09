import type { Quat, Vec3 } from "../core/api";
import { EPS, add, cross, normalize, scale } from "./vec3";

export type { Quat };

export const QUAT_IDENTITY: Quat = [0, 0, 0, 1];
const DEG = Math.PI / 180;

export function quat(x = 0, y = 0, z = 0, w = 1): Quat {
  return [x, y, z, w];
}

export function quatLength(q: Quat): number {
  return Math.hypot(q[0], q[1], q[2], q[3]);
}

export function quatNormalize(q: Quat): Quat {
  const l = quatLength(q);
  return l > EPS ? [q[0] / l, q[1] / l, q[2] / l, q[3] / l] : QUAT_IDENTITY.slice() as Quat;
}

export function quatMul(a: Quat, b: Quat): Quat {
  return [
    a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
    a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
    a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
    a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
  ];
}

export function quatConjugate(q: Quat): Quat {
  return [-q[0], -q[1], -q[2], q[3]];
}

export function quatFromAxisAngle(axis: Vec3, radians: number): Quat {
  const a = normalize(axis, [0, 0, 1]);
  const h = radians * 0.5;
  const s = Math.sin(h);
  return quatNormalize([a[0] * s, a[1] * s, a[2] * s, Math.cos(h)]);
}

export function quatRotate(q: Quat, v: Vec3): Vec3 {
  const qv: Vec3 = [q[0], q[1], q[2]];
  const t = scale(cross(qv, v), 2);
  return add(v, add(scale(t, q[3]), cross(qv, t)));
}

export function quatSlerp(a: Quat, b: Quat, t: number): Quat {
  const d = quatDot(a, b);
  const end: Quat = d < 0 ? [-b[0], -b[1], -b[2], -b[3]] : b;
  const cos = Math.abs(d);
  if (cos > 0.9995) {
    return quatNormalize([
      a[0] + (end[0] - a[0]) * t,
      a[1] + (end[1] - a[1]) * t,
      a[2] + (end[2] - a[2]) * t,
      a[3] + (end[3] - a[3]) * t,
    ]);
  }
  const theta = Math.acos(Math.min(1, Math.max(-1, cos)));
  const sin = Math.sin(theta);
  const wa = Math.sin((1 - t) * theta) / sin;
  const wb = Math.sin(t * theta) / sin;
  return [a[0] * wa + end[0] * wb, a[1] * wa + end[1] * wb, a[2] * wa + end[2] * wb, a[3] * wa + end[3] * wb];
}

export function quatDot(a: Quat, b: Quat): number {
  return a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
}

/**
 * Row-major 3x3 for `EulerRot::ZYX` with angles in the order (yaw, pitch, roll):
 * index 3*r + c, equal to `Rz(yaw) * Ry(pitch) * Rx(roll)`.
 */
function eulerZyxMatrix(yaw: number, pitch: number, roll: number, out: Float64Array): Float64Array {
  const c1 = Math.cos(yaw), s1 = Math.sin(yaw);
  const c2 = Math.cos(pitch), s2 = Math.sin(pitch);
  const c3 = Math.cos(roll), s3 = Math.sin(roll);
  out[0] = c1 * c2;
  out[1] = s2 * c1 * s3 - s1 * c3;
  out[2] = s2 * c1 * c3 + s1 * s3;
  out[3] = s1 * c2;
  out[4] = s2 * s1 * s3 + c1 * c3;
  out[5] = s2 * s1 * c3 - c1 * s3;
  out[6] = -s2;
  out[7] = c2 * s3;
  out[8] = c2 * c3;
  return out;
}

const eulerScratch = new Float64Array(9);

/**
 * `angles` are (pitch, yaw, roll) degrees, the order written to `.ent` keys.
 * The rotation is the one `rdoc::model::Transform` builds for those angles.
 */
export function quatFromSourceAngles(angles: Vec3): Quat {
  const m = eulerZyxMatrix(angles[1] * DEG, angles[0] * DEG, angles[2] * DEG, eulerScratch);
  return quatFromMat3(m);
}

/** Inverse of `quatFromSourceAngles`, in degrees. */
export function sourceAnglesFromQuat(q: Quat): Vec3 {
  const m = mat3FromQuat(q, eulerScratch);
  const pitch = Math.asin(Math.min(1, Math.max(-1, -m[6])));
  // At the poles only yaw-roll (+) or yaw+roll (-) survives; the other is set to 0.
  if (m[6] < -0.99999) return [pitch / DEG, Math.atan2(m[5], m[4]) / DEG, 0];
  if (m[6] > 0.99999) return [pitch / DEG, Math.atan2(-m[5], m[4]) / DEG, 0];
  return [pitch / DEG, Math.atan2(m[3], m[0]) / DEG, Math.atan2(m[7], m[8]) / DEG];
}

/** Row-major 3x3 rotation matrix of `q`. */
export function mat3FromQuat(q: Quat, out: Float64Array): Float64Array {
  const [x, y, z, w] = q;
  const xx = x * x, yy = y * y, zz = z * z;
  const xy = x * y, xz = x * z, yz = y * z;
  const wx = w * x, wy = w * y, wz = w * z;
  out[0] = 1 - 2 * (yy + zz);
  out[1] = 2 * (xy - wz);
  out[2] = 2 * (xz + wy);
  out[3] = 2 * (xy + wz);
  out[4] = 1 - 2 * (xx + zz);
  out[5] = 2 * (yz - wx);
  out[6] = 2 * (xz - wy);
  out[7] = 2 * (yz + wx);
  out[8] = 1 - 2 * (xx + yy);
  return out;
}

export function quatFromMat3(m: Float64Array): Quat {
  const trace = m[0] + m[4] + m[8];
  if (trace > 0) {
    const s = Math.sqrt(trace + 1) * 2;
    return [(m[7] - m[5]) / s, (m[2] - m[6]) / s, (m[3] - m[1]) / s, 0.25 * s];
  }
  if (m[0] > m[4] && m[0] > m[8]) {
    const s = Math.sqrt(1 + m[0] - m[4] - m[8]) * 2;
    return [0.25 * s, (m[1] + m[3]) / s, (m[2] + m[6]) / s, (m[7] - m[5]) / s];
  }
  if (m[4] > m[8]) {
    const s = Math.sqrt(1 + m[4] - m[0] - m[8]) * 2;
    return [(m[1] + m[3]) / s, 0.25 * s, (m[5] + m[7]) / s, (m[2] - m[6]) / s];
  }
  const s = Math.sqrt(1 + m[8] - m[0] - m[4]) * 2;
  return [(m[2] + m[6]) / s, (m[5] + m[7]) / s, 0.25 * s, (m[3] - m[1]) / s];
}
