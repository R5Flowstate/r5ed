import type { Vec3 } from "../core/api";

export type { Vec3 };

export const EPS = 1e-9;

export function v3(x = 0, y = 0, z = 0): Vec3 {
  return [x, y, z];
}

export function add(a: Vec3, b: Vec3): Vec3 {
  return [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
}

export function sub(a: Vec3, b: Vec3): Vec3 {
  return [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
}

export function mul(a: Vec3, b: Vec3): Vec3 {
  return [a[0] * b[0], a[1] * b[1], a[2] * b[2]];
}

export function scale(a: Vec3, s: number): Vec3 {
  return [a[0] * s, a[1] * s, a[2] * s];
}

export function neg(a: Vec3): Vec3 {
  return [-a[0], -a[1], -a[2]];
}

export function dot(a: Vec3, b: Vec3): number {
  return a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
}

export function cross(a: Vec3, b: Vec3): Vec3 {
  return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
}

export function lengthSq(a: Vec3): number {
  return dot(a, a);
}

export function length(a: Vec3): number {
  return Math.sqrt(dot(a, a));
}

export function distance(a: Vec3, b: Vec3): number {
  return length(sub(a, b));
}

export function normalize(a: Vec3, fallback: Vec3 = [0, 0, 0]): Vec3 {
  const l = length(a);
  return l > EPS ? scale(a, 1 / l) : fallback.slice() as Vec3;
}

export function lerp3(a: Vec3, b: Vec3, t: number): Vec3 {
  return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
}

export function min3(a: Vec3, b: Vec3): Vec3 {
  return [Math.min(a[0], b[0]), Math.min(a[1], b[1]), Math.min(a[2], b[2])];
}

export function max3(a: Vec3, b: Vec3): Vec3 {
  return [Math.max(a[0], b[0]), Math.max(a[1], b[1]), Math.max(a[2], b[2])];
}

export function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

export function finite3(a: Vec3): boolean {
  return Number.isFinite(a[0]) && Number.isFinite(a[1]) && Number.isFinite(a[2]);
}

export interface Aabb {
  min: Vec3;
  max: Vec3;
}

export function emptyAabb(): Aabb {
  return { min: [Infinity, Infinity, Infinity], max: [-Infinity, -Infinity, -Infinity] };
}

export function isEmptyAabb(b: Aabb): boolean {
  return b.min[0] > b.max[0] || b.min[1] > b.max[1] || b.min[2] > b.max[2];
}

export function growAabb(b: Aabb, p: Vec3): Aabb {
  return { min: min3(b.min, p), max: max3(b.max, p) };
}

export function unionAabb(a: Aabb, b: Aabb): Aabb {
  if (isEmptyAabb(a)) return { min: b.min.slice() as Vec3, max: b.max.slice() as Vec3 };
  if (isEmptyAabb(b)) return { min: a.min.slice() as Vec3, max: a.max.slice() as Vec3 };
  return { min: min3(a.min, b.min), max: max3(a.max, b.max) };
}

export function expandAabb(b: Aabb, amount: number): Aabb {
  if (isEmptyAabb(b)) return b;
  return { min: sub(b.min, [amount, amount, amount]), max: add(b.max, [amount, amount, amount]) };
}

export function aabbCenter(b: Aabb): Vec3 {
  return [(b.min[0] + b.max[0]) * 0.5, (b.min[1] + b.max[1]) * 0.5, (b.min[2] + b.max[2]) * 0.5];
}

export function aabbSize(b: Aabb): Vec3 {
  return [b.max[0] - b.min[0], b.max[1] - b.min[1], b.max[2] - b.min[2]];
}

export function aabbContains(b: Aabb, p: Vec3, eps = 0): boolean {
  return (
    p[0] >= b.min[0] - eps && p[0] <= b.max[0] + eps &&
    p[1] >= b.min[1] - eps && p[1] <= b.max[1] + eps &&
    p[2] >= b.min[2] - eps && p[2] <= b.max[2] + eps
  );
}

export function aabbOverlaps(a: Aabb, b: Aabb, eps = 0): boolean {
  return (
    a.min[0] <= b.max[0] + eps && a.max[0] + eps >= b.min[0] &&
    a.min[1] <= b.max[1] + eps && a.max[1] + eps >= b.min[1] &&
    a.min[2] <= b.max[2] + eps && a.max[2] + eps >= b.min[2]
  );
}
