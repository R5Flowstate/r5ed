import type { Editor } from "../state/editor";
import { select, setShading, setUi, state } from "../state/stores";
import { clampSingle, clampXray, isShadingMode, isSolidColor, setMode } from "../render/shading";
import { presentedFrames, targetView, type ViewController } from "../ui/viewRegistry";
import { setAxisView, setOrtho, toUserView, type CameraState, type ViewKind } from "../render/camera";
import { sceneBounds } from "../render/renderer";
import type { Aabb } from "../math/vec3";

/** What the dispatcher drives. Narrower than Editor so tests can use a stub. */
export interface ControlEnv {
  editor: Editor;
  takeErrors(): string[];
}

export type ControlReply = Record<string, unknown>;

const MAX_SCREENSHOT = 4096;
const MAX_IDS = 100000;
const WAIT_DEFAULT_MS = 10000;
const WAIT_MAX_MS = 120000;
const COORD_MAX = 1e6;

function err(message: string): ControlReply {
  return { error: message };
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function rejectExtra(params: Record<string, unknown>, allowed: readonly string[], verb: string): string | null {
  for (const key of Object.keys(params)) {
    if (!allowed.includes(key)) return `unknown key "${key}" for ${verb}`;
  }
  return null;
}

function num(v: unknown, key: string, verb: string): number | string {
  if (typeof v !== "number" || !Number.isFinite(v)) return `${verb}: "${key}" must be a finite number`;
  return v;
}

function int(v: unknown, key: string, verb: string, min: number, max: number): number | string {
  if (typeof v !== "number" || !Number.isInteger(v) || v < min || v > max) {
    return `${verb}: "${key}" must be an integer ${min}..${max}`;
  }
  return v;
}

function triple(v: unknown, key: string, verb: string): [number, number, number] | string {
  if (!Array.isArray(v) || v.length !== 3) return `${verb}: "${key}" must be three numbers`;
  const out: number[] = [];
  for (const n of v) {
    if (typeof n !== "number" || !Number.isFinite(n) || Math.abs(n) > COORD_MAX) {
      return `${verb}: "${key}" must be finite numbers within ${COORD_MAX}`;
    }
    out.push(n);
  }
  return [out[0], out[1], out[2]];
}

function bool(v: unknown, key: string, verb: string): boolean | string {
  if (typeof v !== "boolean") return `${verb}: "${key}" must be a boolean`;
  return v;
}

function str(v: unknown, key: string, verb: string): string | null {
  if (typeof v !== "string") return `${verb}: "${key}" must be a string`;
  return null;
}

function cameraCopy(c: CameraState): Record<string, unknown> {
  return JSON.parse(JSON.stringify(c)) as Record<string, unknown>;
}

function base64(bytes: Uint8Array): string {
  let out = "";
  const CHUNK = 0x8000;
  for (let at = 0; at < bytes.length; at += CHUNK) {
    out += String.fromCharCode(...bytes.subarray(at, at + CHUNK));
  }
  return btoa(out);
}

function unionBounds(list: { worldBounds: Aabb | null }[]): Aabb | null {
  let out: Aabb | null = null;
  for (const o of list) {
    const b = o.worldBounds;
    if (!b) continue;
    if (!out) {
      out = { min: [...b.min] as Aabb["min"], max: [...b.max] as Aabb["max"] };
      continue;
    }
    for (let i = 0; i < 3; i++) {
      out.min[i] = Math.min(out.min[i], b.min[i]);
      out.max[i] = Math.max(out.max[i], b.max[i]);
    }
  }
  return out;
}

function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

async function handleState(env: ControlEnv): Promise<ControlReply> {
  const store = env.editor.store;
  const snap = store?.snapshot();
  const depths = store?.depths() ?? { undo: 0, redo: 0 };
  const view = targetView();
  const meshEdit = env.editor.meshEdit;
  return {
    mode: meshEdit ? "edit" : "object",
    project: state.project,
    stem: state.stem,
    objects: store?.ids().length ?? 0,
    active: state.selection.primary,
    selection: { ids: [...state.selection.ids], count: state.selection.ids.length },
    edit: meshEdit ? { object: meshEdit.object, mode: meshEdit.mode, counts: { ...meshEdit.counts } } : null,
    undo: { depth: depths.undo, canUndo: snap?.canUndo ?? false, canRedo: snap?.canRedo ?? false },
    shading: {
      mode: state.ui.shading.mode,
      color: state.ui.shading.color,
      single: [...state.ui.shading.single],
      xray: state.ui.shading.xray,
      wire_overlay: state.ui.shading.wireOverlay,
      cull: state.ui.shading.cullBackfaces,
      lightmaps: state.ui.showLightmaps,
    },
    lastOp: env.editor.lastMeshOp ?? null,
    camera: view ? cameraCopy(view.camera()) : null,
    errors: env.takeErrors(),
  };
}

async function handleOpen(env: ControlEnv, params: Record<string, unknown>): Promise<ControlReply> {
  const bad = rejectExtra(params, ["project"], "open");
  if (bad) return err(bad);
  const m = str(params.project, "project", "open");
  if (m) return err(m);
  const name = params.project as string;
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(name)) return err('open: "project" must be 1-64 characters of A-Z a-z 0-9 _ -');
  try {
    await env.editor.openProject(name);
  } catch (e) {
    return err(messageOf(e));
  }
  return { project: name, objects: env.editor.store?.ids().length ?? 0 };
}

async function handleSelect(env: ControlEnv, params: Record<string, unknown>): Promise<ControlReply> {
  const bad = rejectExtra(params, ["ids", "query"], "select");
  if (bad) return err(bad);
  if (params.query !== undefined) return err("query select is not available yet");
  if (!Array.isArray(params.ids)) return err('select: "ids" must be an array of object ids');
  if (params.ids.length > MAX_IDS) return err(`select: at most ${MAX_IDS} ids`);
  const store = env.editor.store;
  if (!store) return err("select: no document is open");
  const ids: string[] = [];
  for (const id of params.ids) {
    if (typeof id !== "string" || id.length === 0) return err("select: every id must be a non-empty string");
    ids.push(id);
  }
  const missing = ids.filter((id) => !store.object(id));
  if (missing.length > 0) return err(`select: unknown object${missing.length > 1 ? "s" : ""}: ${missing.slice(0, 5).join(", ")}`);
  select(ids);
  return { ids };
}

const EDIT_MODES = ["vertex", "edge", "face"];

async function handleEdit(env: ControlEnv, params: Record<string, unknown>): Promise<ControlReply> {
  const bad = rejectExtra(params, ["object", "enter", "mode"], "edit");
  if (bad) return err(bad);
  let enter = true;
  if (params.enter !== undefined) {
    const e = bool(params.enter, "enter", "edit");
    if (typeof e === "string") return err(e);
    enter = e;
  }
  if (!enter) {
    try {
      env.editor.endMeshEdit();
    } catch (e) {
      return err(messageOf(e));
    }
    return { editing: false };
  }
  const m = str(params.object, "object", "edit");
  if (m) return err(m);
  const object = params.object as string;
  if (object.length === 0) return err('edit: "object" must be a non-empty id');
  let mode: string | null = null;
  if (params.mode !== undefined) {
    if (typeof params.mode !== "string" || !EDIT_MODES.includes(params.mode)) {
      return err('edit: "mode" must be one of vertex edge face');
    }
    mode = params.mode;
  }
  try {
    env.editor.beginMeshEdit(object);
    if (mode) env.editor.meshSetMode(mode as "vertex" | "edge" | "face");
  } catch (e) {
    return err(messageOf(e));
  }
  const session = env.editor.meshEdit;
  if (!session) return err("edit: the core refused the edit session");
  return { editing: true, object: session.object, mode: session.mode, counts: { ...session.counts } };
}

async function handleEditSelect(env: ControlEnv, params: Record<string, unknown>): Promise<ControlReply> {
  if (!env.editor.meshEdit) return err("edit.select: no mesh is being edited");
  const mode = params.mode;
  if (typeof mode !== "string" || !EDIT_MODES.includes(mode)) {
    return err('edit.select: "mode" must be one of vertex edge face');
  }
  if (params.ids !== undefined) {
    if (!Array.isArray(params.ids) || params.ids.length > MAX_IDS || !params.ids.every((i) => Number.isInteger(i) && (i as number) >= 0)) {
      return err('edit.select: "ids" must be an array of non-negative integer element ids');
    }
  }
  if (params.query !== undefined && !isRecord(params.query)) {
    return err('edit.select: "query" must be an object');
  }
  try {
    const out = env.editor.meshEditSelect(JSON.stringify(params));
    return { mode: out.mode, ids: out.ids };
  } catch (e) {
    return err(messageOf(e));
  }
}

async function handleOp(env: ControlEnv, params: Record<string, unknown>): Promise<ControlReply> {
  if (typeof params.op !== "string" || params.op.length === 0) {
    return err('op: "op" must be a non-empty operation name');
  }
  if (params.sel !== undefined && !isRecord(params.sel)) {
    return err('op: "sel" must be an object');
  }
  if (params.params !== undefined && !isRecord(params.params)) {
    return err('op: "params" must be an object');
  }
  try {
    const result = env.editor.runMeshOp(JSON.stringify({ op: params.op, object: params.object, sel: params.sel ?? {}, params: params.params ?? {} }));
    return { selection: result.selection, report: result.report, objects: result.objects ?? [] };
  } catch (e) {
    return err(messageOf(e));
  }
}

async function handleUndoRedo(env: ControlEnv, verb: "undo" | "redo"): Promise<ControlReply> {
  try {
    if (verb === "undo") env.editor.undo();
    else env.editor.redo();
  } catch (e) {
    return err(messageOf(e));
  }
  const snap = env.editor.store?.snapshot();
  const depths = env.editor.store?.depths() ?? { undo: 0, redo: 0 };
  return { depth: depths.undo, canUndo: snap?.canUndo ?? false, canRedo: snap?.canRedo ?? false };
}

const AXES: ViewKind[] = ["top", "front", "side", "right", "back", "bottom", "persp"];

async function handleView(env: ControlEnv, params: Record<string, unknown>): Promise<ControlReply> {
  const bad = rejectExtra(params, ["eye", "target", "fov", "frame", "axis", "ortho"], "view");
  if (bad) return err(bad);
  const hasEye = params.eye !== undefined;
  const hasTarget = params.target !== undefined;
  const hasFrame = params.frame !== undefined;
  const hasAxis = params.axis !== undefined;
  const forms = [hasEye || hasTarget, hasFrame, hasAxis].filter(Boolean).length;
  if (forms !== 1) return err('view: give exactly one of {eye+target}, {frame} or {axis}');
  if (hasEye || hasTarget) {
    if (!hasEye || !hasTarget) return err('view: "eye" and "target" go together');
    const eye = triple(params.eye, "eye", "view");
    if (typeof eye === "string") return err(eye);
    const target = triple(params.target, "target", "view");
    if (typeof target === "string") return err(target);
    let fov: number | null = null;
    if (params.fov !== undefined) {
      const f = num(params.fov, "fov", "view");
      if (typeof f === "string") return err(f);
      if (f < 1 || f > 179) return err('view: "fov" must be 1..179 degrees');
      fov = f;
    }
    let ortho: boolean | null = null;
    if (params.ortho !== undefined) {
      const o = bool(params.ortho, "ortho", "view");
      if (typeof o === "string") return err(o);
      ortho = o;
    }
    const view = targetView();
    if (!view) return err("view: no viewport is mounted");
    const cam = view.camera();
    toUserView(cam);
    cam.eye = eye;
    cam.target = target;
    cam.distance = Math.min(200000, Math.max(1, Math.hypot(eye[0] - target[0], eye[1] - target[1], eye[2] - target[2])));
    if (fov !== null) cam.fov = fov;
    if (ortho !== null) setOrtho(cam, ortho);
    view.cameraChanged();
    return { camera: cameraCopy(cam) };
  }
  if (hasFrame) {
    if (params.frame !== "selection" && params.frame !== "all") return err('view: "frame" must be "selection" or "all"');
    let bounds: Aabb | null;
    if (params.frame === "selection") {
      const wanted = new Set(state.selection.ids);
      bounds = unionBounds(env.editor.currentObjects().filter((o) => wanted.has(o.doc.id)));
      if (!bounds) return err("view: nothing to frame");
    } else {
      const scene = env.editor.scene;
      bounds = (scene ? sceneBounds(scene) : null) ?? unionBounds(env.editor.currentObjects());
      if (!bounds) return err("view: the scene is empty");
    }
    const view = targetView();
    if (!view) return err("view: no viewport is mounted");
    view.frame(bounds.min, bounds.max);
    return { camera: cameraCopy(view.camera()) };
  }
  if (typeof params.axis !== "string" || !AXES.includes(params.axis as ViewKind)) {
    return err('view: "axis" must be one of top front side right back bottom persp');
  }
  let ortho = true;
  if (params.ortho !== undefined) {
    const o = bool(params.ortho, "ortho", "view");
    if (typeof o === "string") return err(o);
    ortho = o;
  }
  const view = targetView();
  if (!view) return err("view: no viewport is mounted");
  const cam = view.camera();
  const kind = params.axis as ViewKind;
  if (kind === "persp") {
    toUserView(cam);
    setOrtho(cam, ortho);
  } else {
    setAxisView(cam, kind);
    if (!ortho) setOrtho(cam, false);
  }
  view.cameraChanged();
  return { camera: cameraCopy(cam) };
}

async function handleShading(params: Record<string, unknown>): Promise<ControlReply> {
  const bad = rejectExtra(params, ["mode", "lightmaps", "xray", "wire_overlay", "overlays", "color", "single", "cull"], "shading");
  if (bad) return err(bad);
  const m = str(params.mode, "mode", "shading");
  if (m) return err(m);
  const mode = params.mode as string;
  if (!isShadingMode(mode)) return err('shading: "mode" must be one of textured solid wireframe');
  let lightmaps = state.ui.showLightmaps;
  if (params.lightmaps !== undefined) {
    const l = bool(params.lightmaps, "lightmaps", "shading");
    if (typeof l === "string") return err(l);
    lightmaps = l;
    setUi({ showLightmaps: l });
  }
  const next = setMode(state.ui.shading, mode);
  if (params.xray !== undefined) {
    const x = clampXray(params.xray);
    if (x === null) return err('shading: "xray" must be a number 0..1');
    next.xray = x;
  }
  if (params.wire_overlay !== undefined) {
    const w = bool(params.wire_overlay, "wire_overlay", "shading");
    if (typeof w === "string") return err(w);
    next.wireOverlay = w;
  }
  if (params.color !== undefined) {
    if (!isSolidColor(params.color)) return err('shading: "color" must be one of material object single uv_checker');
    next.color = params.color;
  }
  if (params.single !== undefined) {
    const s = clampSingle(params.single);
    if (!s) return err('shading: "single" must be three numbers 0..1');
    next.single = s;
  }
  if (params.cull !== undefined) {
    const c = bool(params.cull, "cull", "shading");
    if (typeof c === "string") return err(c);
    next.cullBackfaces = c;
  }
  setShading(next);
  let overlays = false;
  if (params.overlays !== undefined) {
    const o = bool(params.overlays, "overlays", "shading");
    if (typeof o === "string") return err(o);
    overlays = o;
  }
  return {
    mode: next.mode,
    color: next.color,
    single: [...next.single],
    xray: next.xray,
    wire_overlay: next.wireOverlay,
    cull: next.cullBackfaces,
    lightmaps,
    overlays,
  };
}

async function handleWait(env: ControlEnv, params: Record<string, unknown>): Promise<ControlReply> {
  const bad = rejectExtra(params, ["idle", "timeout_ms"], "wait");
  if (bad) return err(bad);
  if (params.idle !== undefined && params.idle !== true) return err('wait: only {"idle": true} is supported');
  let timeout = WAIT_DEFAULT_MS;
  if (params.timeout_ms !== undefined) {
    const t = int(params.timeout_ms, "timeout_ms", "wait", 1, WAIT_MAX_MS);
    if (typeof t === "string") return err(t);
    timeout = t;
  }
  const started = Date.now();
  const firstFrame = presentedFrames;
  for (;;) {
    if (env.editor.pendingLoads() === 0 && presentedFrames > firstFrame) {
      return { idle: true, waited_ms: Date.now() - started };
    }
    if (Date.now() - started >= timeout) return err("wait: timed out waiting for idle");
    await new Promise((r) => setTimeout(r, 50));
  }
}

async function handleScreenshot(params: Record<string, unknown>): Promise<ControlReply> {
  const bad = rejectExtra(params, ["width", "height", "overlays"], "screenshot");
  if (bad) return err(bad);
  const w = int(params.width, "width", "screenshot", 1, MAX_SCREENSHOT);
  if (typeof w === "string") return err(w);
  const h = int(params.height, "height", "screenshot", 1, MAX_SCREENSHOT);
  if (typeof h === "string") return err(h);
  let overlays = false;
  if (params.overlays !== undefined) {
    const o = bool(params.overlays, "overlays", "screenshot");
    if (typeof o === "string") return err(o);
    overlays = o;
  }
  const view: ViewController | undefined = targetView();
  if (!view) return err("screenshot: no viewport is mounted");
  let png: Uint8Array;
  try {
    png = await view.capture(w, h, overlays);
  } catch (e) {
    return err(`screenshot: ${messageOf(e)}`);
  }
  return { width: w, height: h, overlays, png: base64(png) };
}

/** Runs one control message against the live editor. Every failure is a reply, never a throw. */
export async function handle(env: ControlEnv, msg: unknown): Promise<ControlReply> {
  if (!isRecord(msg)) return err("message must be an object");
  const top = rejectExtra(msg, ["verb", "params"], "message");
  if (top) return err(top);
  if (typeof msg.verb !== "string") return err('message: "verb" must be a string');
  const params = msg.params === undefined ? {} : msg.params;
  if (!isRecord(params)) return err('message: "params" must be an object');
  try {
    switch (msg.verb) {
      case "state":
        if (Object.keys(params).length > 0) return err('unknown key for state: only {} takes no params');
        return await handleState(env);
      case "open":
        return await handleOpen(env, params);
      case "select":
        return await handleSelect(env, params);
      case "edit":
        return await handleEdit(env, params);
      case "edit.select":
        return await handleEditSelect(env, params);
      case "op":
        return await handleOp(env, params);
      case "undo":
        if (Object.keys(params).length > 0) return err('unknown key for undo: it takes no params');
        return await handleUndoRedo(env, "undo");
      case "redo":
        if (Object.keys(params).length > 0) return err('unknown key for redo: it takes no params');
        return await handleUndoRedo(env, "redo");
      case "view":
        return await handleView(env, params);
      case "shading":
        return await handleShading(params);
      case "wait":
        return await handleWait(env, params);
      case "screenshot":
        return await handleScreenshot(params);
      default:
        return err(`unknown verb "${msg.verb}"`);
    }
  } catch (e) {
    return err(messageOf(e));
  }
}

/** Binds the dispatcher to one editor for the socket and headless paths. */
export function createHandler(env: ControlEnv): (msg: unknown) => Promise<ControlReply> {
  return (msg: unknown) => handle(env, msg);
}
