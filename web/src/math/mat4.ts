import type { Quat, Vec3 } from "../core/api";

/** Column-major 4x4 matrix, the layout WGSL expects. */
export type Mat4 = Float32Array;

export function mat4(): Mat4 {
  const m = new Float32Array(16);
  m[0] = m[5] = m[10] = m[15] = 1;
  return m;
}

export function identity(out: Mat4): Mat4 {
  out.fill(0);
  out[0] = out[5] = out[10] = out[15] = 1;
  return out;
}

const mulScratch = new Float32Array(16);

export function multiply(out: Mat4, a: Mat4, b: Mat4): Mat4 {
  for (let c = 0; c < 4; c++) {
    const b0 = b[c * 4], b1 = b[c * 4 + 1], b2 = b[c * 4 + 2], b3 = b[c * 4 + 3];
    mulScratch[c * 4 + 0] = a[0] * b0 + a[4] * b1 + a[8] * b2 + a[12] * b3;
    mulScratch[c * 4 + 1] = a[1] * b0 + a[5] * b1 + a[9] * b2 + a[13] * b3;
    mulScratch[c * 4 + 2] = a[2] * b0 + a[6] * b1 + a[10] * b2 + a[14] * b3;
    mulScratch[c * 4 + 3] = a[3] * b0 + a[7] * b1 + a[11] * b2 + a[15] * b3;
  }
  out.set(mulScratch);
  return out;
}

/** Right-handed look-at producing a view matrix (world -> eye). */
export function lookAt(out: Mat4, eye: Vec3, center: Vec3, up: Vec3): Mat4 {
  const zx = eye[0] - center[0], zy = eye[1] - center[1], zz = eye[2] - center[2];
  const zl = Math.hypot(zx, zy, zz);
  if (zl < 1e-9) {
    identity(out);
    return out;
  }
  const z0 = zx / zl, z1 = zy / zl, z2 = zz / zl;
  // A view straight along `up` has no screen right, so pick another reference.
  let ux = up[0], uy = up[1], uz = up[2];
  if (Math.abs(ux * z0 + uy * z1 + uz * z2) > 0.9999) {
    ux = 0; uy = 1; uz = 0;
    if (Math.abs(uy * z0 + uy * z1 + uz * z2) > 0.9999) {
      ux = 1; uy = 0; uz = 0;
    }
  }
  let x0 = uy * z2 - uz * z1;
  let x1 = uz * z0 - ux * z2;
  let x2 = ux * z1 - uy * z0;
  const xl = Math.hypot(x0, x1, x2) || 1;
  x0 /= xl; x1 /= xl; x2 /= xl;
  const y0 = z1 * x2 - z2 * x1;
  const y1 = z2 * x0 - z0 * x2;
  const y2 = z0 * x1 - z1 * x0;
  out[0] = x0; out[1] = y0; out[2] = z0; out[3] = 0;
  out[4] = x1; out[5] = y1; out[6] = z1; out[7] = 0;
  out[8] = x2; out[9] = y2; out[10] = z2; out[11] = 0;
  out[12] = -(x0 * eye[0] + x1 * eye[1] + x2 * eye[2]);
  out[13] = -(y0 * eye[0] + y1 * eye[1] + y2 * eye[2]);
  out[14] = -(z0 * eye[0] + z1 * eye[1] + z2 * eye[2]);
  out[15] = 1;
  return out;
}

/** Right-handed perspective with WebGPU's 0..1 depth range. */
export function perspective(out: Mat4, fovY: number, aspect: number, near: number, far: number): Mat4 {
  const f = 1 / Math.tan(fovY * 0.5);
  out.fill(0);
  out[0] = f / (aspect || 1);
  out[5] = f;
  out[10] = far / (near - far);
  out[11] = -1;
  out[14] = (far * near) / (near - far);
  return out;
}

/** Right-handed orthographic with WebGPU's 0..1 depth range; args are half-extents. */
export function ortho(out: Mat4, halfW: number, halfH: number, near: number, far: number): Mat4 {
  const dx = 1 / (halfW || 1);
  const dy = 1 / (halfH || 1);
  const dz = 1 / (near - far);
  out.fill(0);
  out[0] = dx;
  out[5] = dy;
  out[10] = dz;
  out[14] = near * dz;
  out[15] = 1;
  return out;
}

export function invert(out: Mat4, a: Mat4): Mat4 | null {
  const a00 = a[0], a01 = a[1], a02 = a[2], a03 = a[3];
  const a10 = a[4], a11 = a[5], a12 = a[6], a13 = a[7];
  const a20 = a[8], a21 = a[9], a22 = a[10], a23 = a[11];
  const a30 = a[12], a31 = a[13], a32 = a[14], a33 = a[15];
  const b00 = a00 * a11 - a01 * a10, b01 = a00 * a12 - a02 * a10;
  const b02 = a00 * a13 - a03 * a10, b03 = a01 * a12 - a02 * a11;
  const b04 = a01 * a13 - a03 * a11, b05 = a02 * a13 - a03 * a12;
  const b06 = a20 * a31 - a21 * a30, b07 = a20 * a32 - a22 * a30;
  const b08 = a20 * a33 - a23 * a30, b09 = a21 * a32 - a22 * a31;
  const b10 = a21 * a33 - a23 * a31, b11 = a22 * a33 - a23 * a32;
  let det = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
  if (Math.abs(det) < 1e-20) return null;
  det = 1 / det;
  out[0] = (a11 * b11 - a12 * b10 + a13 * b09) * det;
  out[1] = (a02 * b10 - a01 * b11 - a03 * b09) * det;
  out[2] = (a31 * b05 - a32 * b04 + a33 * b03) * det;
  out[3] = (a22 * b04 - a21 * b05 - a23 * b03) * det;
  out[4] = (a12 * b08 - a10 * b11 - a13 * b07) * det;
  out[5] = (a00 * b11 - a02 * b08 + a03 * b07) * det;
  out[6] = (a32 * b02 - a30 * b05 - a33 * b01) * det;
  out[7] = (a20 * b05 - a22 * b02 + a23 * b01) * det;
  out[8] = (a10 * b10 - a11 * b08 + a13 * b06) * det;
  out[9] = (a01 * b08 - a00 * b10 - a03 * b06) * det;
  out[10] = (a30 * b04 - a31 * b02 + a33 * b00) * det;
  out[11] = (a21 * b02 - a20 * b04 - a23 * b00) * det;
  out[12] = (a11 * b07 - a10 * b09 - a12 * b06) * det;
  out[13] = (a00 * b09 - a01 * b07 + a02 * b06) * det;
  out[14] = (a31 * b01 - a30 * b03 - a32 * b00) * det;
  out[15] = (a20 * b03 - a21 * b01 + a22 * b00) * det;
  return out;
}

export function transpose(out: Mat4, a: Mat4): Mat4 {
  const t = [a[0], a[4], a[8], a[12], a[1], a[5], a[9], a[13], a[2], a[6], a[10], a[14], a[3], a[7], a[11], a[15]];
  out.set(t);
  return out;
}

export function transformPoint(m: Mat4, p: Vec3): [number, number, number, number] {
  return [
    m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
    m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
    m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15],
  ];
}

export function transformDirection(m: Mat4, v: Vec3): Vec3 {
  const r = transformPoint(m, v);
  return [r[0], r[1], r[2]];
}


export function fromRotationTranslationScale(out: Mat4, q: Quat, t: Vec3, s: Vec3): Mat4 {
  const [x, y, z, w] = q;
  const x2 = x + x, y2 = y + y, z2 = z + z;
  const xx = x * x2, xy = x * y2, xz = x * z2;
  const yy = y * y2, yz = y * z2, zz = z * z2;
  const wx = w * x2, wy = w * y2, wz = w * z2;
  out[0] = (1 - (yy + zz)) * s[0];
  out[1] = (xy + wz) * s[0];
  out[2] = (xz - wy) * s[0];
  out[3] = 0;
  out[4] = (xy - wz) * s[1];
  out[5] = (1 - (xx + zz)) * s[1];
  out[6] = (yz + wx) * s[1];
  out[7] = 0;
  out[8] = (xz + wy) * s[2];
  out[9] = (yz - wx) * s[2];
  out[10] = (1 - (xx + yy)) * s[2];
  out[11] = 0;
  out[12] = t[0];
  out[13] = t[1];
  out[14] = t[2];
  out[15] = 1;
  return out;
}

export function transformFromQuatPosition(m: Mat4, q: Quat, t: Vec3): Mat4 {
  return fromRotationTranslationScale(m, q, t, [1, 1, 1]);
}

/** Writes one transform into `out` at `offset` floats, 16 per instance. */
export function writeInstance(out: Float32Array, offset: number, m: Mat4): void {
  for (let i = 0; i < 16; i++) out[offset + i] = m[i];
}

/** Projects a world point to NDC; null when the point is behind the eye plane. */
export function projectPoint(vp: Mat4, p: Vec3): [number, number, number] | null {
  const c = transformPoint(vp, p);
  if (c[3] <= 1e-6) return null;
  return [c[0] / c[3], c[1] / c[3], c[2] / c[3]];
}

/** The world-space ray through a point given in NDC x/y. */
export function unproject(invVp: Mat4, ndcX: number, ndcY: number): { origin: Vec3; dir: Vec3 } {
  const n = transformPoint(invVp, [ndcX, ndcY, 0]);
  const f = transformPoint(invVp, [ndcX, ndcY, 1]);
  const o: Vec3 = [n[0] / n[3], n[1] / n[3], n[2] / n[3]];
  const p: Vec3 = [f[0] / f[3], f[1] / f[3], f[2] / f[3]];
  const d: Vec3 = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
  const len = Math.hypot(d[0], d[1], d[2]);
  return { origin: o, dir: len > 1e-9 ? [d[0] / len, d[1] / len, d[2] / len] : [0, 0, -1] };
}

export function quatToAxisAngle(q: Quat): { axis: Vec3; angle: number } {
  const n = Math.hypot(q[0], q[1], q[2], q[3]) || 1;
  const w = Math.min(1, Math.max(-1, q[3] / n));
  const angle = 2 * Math.acos(w);
  const s = Math.sqrt(Math.max(0, 1 - w * w));
  if (s < 1e-9) return { axis: [0, 0, 1], angle: 0 };
  return { axis: [q[0] / n / s, q[1] / n / s, q[2] / n / s], angle };
}

export function quatFromAxisAngle(axis: Vec3, angle: number): Quat {
  const l = Math.hypot(axis[0], axis[1], axis[2]);
  if (l < 1e-9) return [0, 0, 0, 1];
  const h = angle * 0.5;
  const s = Math.sin(h) / l;
  return [axis[0] * s, axis[1] * s, axis[2] * s, Math.cos(h)];
}
