import type { Vec3 } from "../core/api";

export type TransformMode = "grab" | "rotate" | "resize";
export type Orientation = "global" | "local" | "view";

export interface Constraint {
  /** 0, 1, 2 for X, Y, Z. */
  axis: number;
  /** Excludes the axis: the move stays in the plane the other two span. */
  plane: boolean;
  orientation: Orientation;
}

export interface TransformOp {
  mode: TransformMode;
  constraint: Constraint | null;
  /** Typed values per axis; a non-empty one overrides the mouse. */
  typed: [string, string, string];
  /** The axis Tab moves typing to. */
  typingAxis: number;
  /** Trackball rotation, entered with R R. */
  trackball: boolean;
  /** Expression entry, switched on with = or numpad *: operators type literally. */
  advanced: boolean;
}

export function beginOp(mode: TransformMode): TransformOp {
  return { mode, constraint: null, typed: ["", "", ""], typingAxis: 0, trackball: false, advanced: false };
}

const AXIS_NAMES = ["X", "Y", "Z"];

/**
 * An axis key, as Blender's axis locking: the first press locks to the
 * header orientation, the second to global (local when the header is
 * already global), the third frees it. Shift locks the plane instead.
 */
export function pressAxis(op: TransformOp, axis: number, plane: boolean, header: Orientation = "global"): TransformOp {
  const c = op.constraint;
  const second: Orientation = header === "global" ? "local" : "global";
  let next: Constraint | null;
  if (!c || c.axis !== axis || c.plane !== plane) next = { axis, plane, orientation: header };
  else if (c.orientation === header && header !== second) next = { axis, plane, orientation: second };
  else next = null;
  return { ...op, constraint: next, typingAxis: next && !next.plane ? axis : op.typingAxis, trackball: false };
}

/** True while any value has been typed, so the mouse no longer drives the op. */
export function isTyping(op: TransformOp): boolean {
  return op.typed.some((t) => t.length > 0);
}

/**
 * Feeds one key to the number entry, following Blender's simple mode (minus
 * negates, slash takes the reciprocal) and advanced mode (expressions).
 * `ctrl` reaches the simple-mode meaning of minus and slash in advanced mode.
 * Returns null when the key is not numeric input.
 */
export function typeKey(op: TransformOp, key: string, ctrl = false): TransformOp | null {
  const typed = [...op.typed] as [string, string, string];
  const i = op.typingAxis;
  const t = typed[i];
  const literalOps = op.advanced && !ctrl;
  if (/^[0-9.]$/.test(key)) {
    typed[i] = t + key;
    return { ...op, typed };
  }
  if (key === "=" || key === "*") {
    if (!op.advanced) return { ...op, advanced: true };
    if (key === "*") {
      typed[i] = t + "*";
      return { ...op, typed };
    }
    return op;
  }
  if (literalOps && (key === "+" || key === "-" || key === "/" || key === "(" || key === ")")) {
    typed[i] = t + key;
    return { ...op, typed };
  }
  if (key === "-") {
    if (t.length === 0) typed[i] = "-";
    else if (t === "-") typed[i] = "";
    else typed[i] = t.startsWith("-(") && t.endsWith(")") ? t.slice(2, -1) : `-(${t})`;
    return { ...op, typed };
  }
  if (key === "/") {
    if (t.length === 0) return op;
    typed[i] = t.startsWith("1/(") && t.endsWith(")") ? t.slice(3, -1) : `1/(${t})`;
    return { ...op, typed };
  }
  if (key === "Backspace") {
    typed[i] = t.slice(0, -1);
    return { ...op, typed };
  }
  if (key === "Tab" || key === "ShiftTab") {
    if (op.mode === "rotate" || (op.constraint && !op.constraint.plane)) return op;
    return { ...op, typingAxis: (i + (key === "Tab" ? 1 : 2)) % 3 };
  }
  return null;
}

/**
 * Evaluates a typed number: + - * / and parentheses over decimals. Anything
 * else, or a malformed expression, is null.
 */
export function evaluate(text: string): number | null {
  const src = text.replace(/\s+/g, "");
  if (src.length === 0) return null;
  let pos = 0;
  const peek = (): string => src[pos] ?? "";
  const expr = (): number | null => {
    let v = term();
    while (v !== null && (peek() === "+" || peek() === "-")) {
      const op = src[pos++];
      const r = term();
      if (r === null) return null;
      v = op === "+" ? v + r : v - r;
    }
    return v;
  };
  const term = (): number | null => {
    let v = unary();
    while (v !== null && (peek() === "*" || peek() === "/")) {
      const op = src[pos++];
      const r = unary();
      if (r === null) return null;
      v = op === "*" ? v * r : v / r;
    }
    return v;
  };
  const unary = (): number | null => {
    if (peek() === "-") {
      pos++;
      const v = unary();
      return v === null ? null : -v;
    }
    if (peek() === "(") {
      pos++;
      const v = expr();
      if (peek() !== ")") return null;
      pos++;
      return v;
    }
    const m = /^(\d+\.?\d*|\.\d+)/.exec(src.slice(pos));
    if (!m) return null;
    pos += m[0].length;
    return Number(m[0]);
  };
  const v = expr();
  return v !== null && pos === src.length && Number.isFinite(v) ? v : null;
}

/** The typed values, each axis null when nothing valid is typed there. */
export function typedValues(op: TransformOp): [number | null, number | null, number | null] {
  return [evaluate(op.typed[0]), evaluate(op.typed[1]), evaluate(op.typed[2])];
}

/** Axis mask of what an op may change: one axis, a plane, or all three. */
export function axisMask(op: TransformOp): Vec3 {
  const c = op.constraint;
  if (!c) return [1, 1, 1];
  const m: Vec3 = c.plane ? [1, 1, 1] : [0, 0, 0];
  m[c.axis] = c.plane ? 0 : 1;
  return m;
}

/** Status line for the op, e.g. "Dx 64  Dy 0  Dz 0 along global X". */
export function describeOp(op: TransformOp, value: Vec3 | number): string {
  const c = op.constraint;
  const along = c ? ` ${c.plane ? "locking" : "along"} ${c.orientation} ${AXIS_NAMES[c.axis]}` : "";
  const adv = op.advanced ? " (expression)" : "";
  const typedMark = (i: number): string => (op.typed[i] ? `[${op.typed[i]}${op.typingAxis === i ? "|" : ""}]` : "");
  if (op.mode === "rotate") {
    const deg = typeof value === "number" ? value : 0;
    return `Rotation ${op.typed[0] ? typedMark(0) : fmt(deg)} deg${along}${op.trackball ? " trackball" : ""}`;
  }
  const v = typeof value === "number" ? [value, value, value] : value;
  const names = op.mode === "grab" ? ["Dx", "Dy", "Dz"] : ["Sx", "Sy", "Sz"];
  const mask = axisMask(op);
  const parts = [0, 1, 2].filter((i) => mask[i] !== 0).map((i) => `${names[i]} ${typedMark(i) || fmt(v[i])}`);
  return `${parts.join("  ")}${along}${adv}`;
}

function fmt(n: number): string {
  return Math.abs(n) >= 100 ? n.toFixed(1) : n.toFixed(3).replace(/0+$/, "").replace(/\.$/, "");
}

export const TRANSFORM_HINTS: [string, string][] = [
  ["LMB / Enter", "Confirm"],
  ["RMB / Esc", "Cancel"],
  ["X Y Z", "Axis"],
  ["Shift X Y Z", "Plane"],
  ["Ctrl", "Invert snap"],
  ["Shift", "Precision"],
  ["0-9", "Type value"],
  ["MMB", "Pick axis"],
  ["Wheel", "Proportional size"],
];
