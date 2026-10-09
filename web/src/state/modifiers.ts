import type { Id } from "../core/api";
import type { Modifier } from "../doc/model";
import { opRequest } from "./meshEdit";

/** One editable param of a modifier kind, as the Inspector draws it. */
export interface ModField {
  key: string;
  kind: "number" | "int" | "bool" | "enum" | "vec3" | "vec2" | "axes" | "object" | "optional_number" | "optional_vec3";
  options?: readonly string[];
  min?: number;
  max?: number;
  /** For object refs: the document kinds the field may name. */
  objects?: readonly ("mesh" | "brush" | "path")[];
}

export interface ModKind {
  type: Modifier["type"];
  label: string;
  fields: ModField[];
  /** A new entry of this kind; `pick` returns an object of the given kinds for required refs, or null. */
  make(pick: (kinds: readonly ("mesh" | "brush" | "path")[]) => Id | null): Modifier | null;
}

export const MODIFIER_KINDS: ModKind[] = [
  {
    type: "array",
    label: "Array",
    fields: [
      { key: "count", kind: "int", min: 1, max: 1024 },
      { key: "fit_length", kind: "optional_number", min: 0 },
      { key: "relative_offset", kind: "optional_vec3" },
      { key: "constant_offset", kind: "optional_vec3" },
      { key: "object_offset", kind: "object", objects: ["mesh", "brush", "path"] },
      { key: "merge", kind: "optional_number", min: 0 },
      { key: "merge_first_last", kind: "bool" },
      { key: "uv_offset", kind: "vec2" },
    ],
    make: () => ({ type: "array", count: 2, relative_offset: [1, 0, 0] }),
  },
  {
    type: "curve",
    label: "Curve",
    fields: [
      { key: "path", kind: "object", objects: ["path"] },
      { key: "axis", kind: "enum", options: ["x", "y", "z", "-x", "-y", "-z"] },
    ],
    make: (pick) => {
      const path = pick(["path"]);
      return path ? { type: "curve", path, axis: "x" } : null;
    },
  },
  {
    type: "displace",
    label: "Displace",
    fields: [
      { key: "strength", kind: "number" },
      { key: "mid_level", kind: "number", min: 0, max: 1 },
      { key: "direction", kind: "enum", options: ["normal", "x", "y", "z"] },
      { key: "space", kind: "enum", options: ["local", "world"] },
    ],
    make: () => ({ type: "displace", strength: 8 }),
  },
  {
    type: "subdivision",
    label: "Subdivision",
    fields: [
      { key: "levels", kind: "int", min: 0, max: 6 },
      { key: "kind", kind: "enum", options: ["catmull_clark", "simple"] },
      { key: "keep_sharp_edges", kind: "bool" },
    ],
    make: () => ({ type: "subdivision", levels: 1 }),
  },
  {
    type: "mirror",
    label: "Mirror",
    fields: [
      { key: "axes", kind: "axes" },
      { key: "merge", kind: "optional_number", min: 0 },
      { key: "clip", kind: "bool" },
      { key: "mirror_object", kind: "object", objects: ["mesh", "brush", "path"] },
    ],
    make: () => ({ type: "mirror", axes: [true, false, false] }),
  },
  {
    type: "boolean",
    label: "Boolean",
    fields: [
      { key: "object", kind: "object", objects: ["mesh", "brush"] },
      { key: "operation", kind: "enum", options: ["difference", "union", "intersect"] },
      { key: "keep_material", kind: "bool" },
    ],
    make: (pick) => {
      const object = pick(["mesh", "brush"]);
      return object ? { type: "boolean", object, operation: "difference" } : null;
    },
  },
];

export function modifierKind(type: string): ModKind | undefined {
  return MODIFIER_KINDS.find((k) => k.type === type);
}

const NO_SEL = { mode: "face", ids: [] as number[] };

/** The op request JSON for one stack edit on `object`; every edit is one undo step. */
export const stackRequest = {
  add: (object: Id, modifier: Modifier): string => opRequest(object, "modifier_add", NO_SEL, { modifier }),
  remove: (object: Id, index: number): string => opRequest(object, "modifier_remove", NO_SEL, { index }),
  move: (object: Id, index: number, to: number): string => opRequest(object, "modifier_move", NO_SEL, { index, to }),
  toggle: (object: Id, index: number): string => opRequest(object, "modifier_toggle", NO_SEL, { index }),
  set: (object: Id, index: number, modifier: Modifier): string => opRequest(object, "modifier_set", NO_SEL, { index, modifier }),
  apply: (object: Id, index: number): string => opRequest(object, "modifier_apply", NO_SEL, { index }),
};

/** `modifier` with one field changed; `null` drops an optional field so the core default applies. */
export function withField(modifier: Modifier, key: string, value: unknown): Modifier {
  const next: Modifier = { ...modifier };
  if (value === null || value === undefined) delete next[key];
  else next[key] = value;
  return next;
}

/** Parses "x y z" (spaces or commas) into a vec of `n` finite numbers, or null. */
export function parseVec(text: string, n: number): number[] | null {
  const parts = text.trim().split(/[\s,]+/).filter((p) => p.length > 0).map(Number);
  return parts.length === n && parts.every((v) => Number.isFinite(v)) ? parts : null;
}
