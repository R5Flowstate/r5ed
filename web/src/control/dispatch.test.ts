import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { handle, type ControlEnv } from "./dispatch";
import { markPresented, views, type ViewController } from "../ui/viewRegistry";
import { select, setShading, setUi } from "../state/stores";
import { defaultShading } from "../render/shading";
import type { Editor } from "../state/editor";
import type { CameraState } from "../render/camera";

function stubEditor(over: Record<string, unknown> = {}): Editor {
  const store = {
    ids: () => ["a1", "b2"],
    object: (id: string) => (id === "a1" || id === "b2" ? { id } : null),
    snapshot: () => ({ canUndo: true, canRedo: false }),
    depths: () => ({ undo: 3, redo: 0 }),
  };
  return {
    store,
    openProject: async () => {},
    undo: () => ["a1"],
    redo: () => ["a1"],
    pendingLoads: () => 0,
    currentObjects: () => [],
    scene: null,
    ...over,
  } as unknown as Editor;
}

const env = (editor: Editor): ControlEnv => ({ editor, takeErrors: () => [] });

function fakeCamera(): CameraState {
  return {
    kind: "persp",
    target: [0, 0, 0],
    eye: [512, -768, 384],
    fov: 60,
    near: 4,
    far: 1 << 22,
    orthoHeight: 512,
    distance: 1024,
  };
}

function fakeView(capture?: ViewController["capture"]): { view: ViewController; cam: CameraState } {
  const cam = fakeCamera();
  const view: ViewController = {
    run: () => false,
    frame: () => {},
    busy: () => false,
    camera: () => cam,
    cameraChanged: () => {},
    frameSeq: () => 0,
    capture: capture ?? (async () => {
      throw new Error("no gpu in tests");
    }),
    ray: () => ({ origin: [0, 0, 0], dir: [0, 0, 1] }),
  };
  return { view, cam };
}

afterEach(() => {
  views.clear();
  select([]);
  vi.restoreAllMocks();
});

describe("dispatch message shape", () => {
  it("rejects a message that is not an object", async () => {
    expect((await handle(env(stubEditor()), null)).error).toBeTruthy();
    expect((await handle(env(stubEditor()), "state")).error).toBeTruthy();
  });

  it("rejects unknown verbs", async () => {
    const r = await handle(env(stubEditor()), { verb: "launch", params: {} });
    expect(String(r.error)).toContain("unknown verb");
  });

  it("rejects unknown top-level keys", async () => {
    const r = await handle(env(stubEditor()), { verb: "state", params: {}, extra: 1 });
    expect(String(r.error)).toContain("unknown key");
  });

  it("rejects non-object params", async () => {
    const r = await handle(env(stubEditor()), { verb: "state", params: [1] });
    expect(String(r.error)).toContain("params");
  });

  it("rejects unknown param keys", async () => {
    const r = await handle(env(stubEditor()), { verb: "state", params: { nope: 1 } });
    expect(String(r.error)).toContain("unknown key");
  });
});

describe("dispatch state", () => {
  it("reports mode, objects, selection and undo depth", async () => {
    const r = await handle(env(stubEditor()), { verb: "state" });
    expect(r.mode).toBe("object");
    expect(r.objects).toBe(2);
    expect(r.active).toBeNull();
    expect(r.lastOp).toBeNull();
    expect(r.undo).toEqual({ depth: 3, canUndo: true, canRedo: false });
    expect(r.errors).toEqual([]);
  });
});

describe("dispatch open", () => {
  it("rejects a bad project name without touching the editor", async () => {
    const editor = stubEditor({ openProject: vi.fn() });
    const r = await handle(env(editor), { verb: "open", params: { project: "../evil" } });
    expect(String(r.error)).toContain("project");
    expect(editor.openProject).not.toHaveBeenCalled();
  });

  it("opens a good name and reports its objects", async () => {
    const opened: string[] = [];
    const editor = stubEditor({ openProject: async (n: string) => void opened.push(n) });
    const r = await handle(env(editor), { verb: "open", params: { project: "mp_rr_test" } });
    expect(r).toEqual({ project: "mp_rr_test", objects: 2 });
    expect(opened).toEqual(["mp_rr_test"]);
  });

  it("turns an editor failure into a reply", async () => {
    const editor = stubEditor({
      openProject: async () => {
        throw new Error("no such project");
      },
    });
    const r = await handle(env(editor), { verb: "open", params: { project: "mp_rr_nope" } });
    expect(String(r.error)).toContain("no such project");
  });
});

describe("dispatch select", () => {
  it("selects known ids", async () => {
    const r = await handle(env(stubEditor()), { verb: "select", params: { ids: ["a1"] } });
    expect(r).toEqual({ ids: ["a1"] });
  });

  it("rejects unknown ids", async () => {
    const r = await handle(env(stubEditor()), { verb: "select", params: { ids: ["a1", "zzz"] } });
    expect(String(r.error)).toContain("zzz");
  });

  it("rejects a non-array and a query", async () => {
    expect(String((await handle(env(stubEditor()), { verb: "select", params: { ids: "a1" } })).error)).toContain("array");
    expect(String((await handle(env(stubEditor()), { verb: "select", params: { query: "all" } })).error)).toContain("not available yet");
  });
});

describe("dispatch edit", () => {
  function meshEditor() {
    const st: { meshEdit: { object: string; mode: string; counts: { verts: number; edges: number; faces: number } } | null } = {
      meshEdit: null,
    };
    const editor = stubEditor({
      beginMeshEdit: (id: string) => {
        st.meshEdit = { object: id, mode: "vertex", counts: { verts: 8, edges: 18, faces: 12 } };
      },
      endMeshEdit: () => {
        st.meshEdit = null;
      },
      meshSetMode: (mode: string) => {
        if (st.meshEdit) st.meshEdit.mode = mode;
      },
      meshEditSelect: (sel: string) => ({ mode: (JSON.parse(sel) as { mode: string }).mode, ids: [0, 1] }),
      runMeshOp: (req: string) => {
        const v = JSON.parse(req) as { op: string };
        if (v.op === "nope") throw new Error("unknown op nope");
        return { selection: { mode: "face", ids: [0] }, report: { verts: 8, edges: 18, faces: 12, warnings: [] } };
      },
    });
    Object.defineProperty(editor, "meshEdit", { get: () => st.meshEdit, configurable: true });
    return editor;
  }

  it("enters edit mode on an object and reports its counts", async () => {
    const r = await handle(env(meshEditor()), { verb: "edit", params: { object: "a1", enter: true, mode: "face" } });
    expect(r).toEqual({ editing: true, object: "a1", mode: "face", counts: { verts: 8, edges: 18, faces: 12 } });
  });

  it("leaves edit mode without committing anything new", async () => {
    const editor = meshEditor();
    await handle(env(editor), { verb: "edit", params: { object: "a1", enter: true } });
    expect(await handle(env(editor), { verb: "edit", params: { enter: false } })).toEqual({ editing: false });
  });

  it("rejects a missing object, a bad mode and extra keys", async () => {
    const editor = meshEditor();
    expect(String((await handle(env(editor), { verb: "edit", params: { enter: true } })).error)).toContain("object");
    expect(String((await handle(env(editor), { verb: "edit", params: { object: "a1", mode: "corner" } })).error)).toContain("mode");
    expect(String((await handle(env(editor), { verb: "edit", params: { object: "a1", shade: 1 } })).error)).toContain("unknown key");
  });

  it("selects session elements and refuses without a session", async () => {
    expect(String((await handle(env(stubEditor()), { verb: "edit.select", params: { mode: "face", ids: [0] } })).error)).toContain(
      "no mesh is being edited",
    );
    const editor = meshEditor();
    await handle(env(editor), { verb: "edit", params: { object: "a1", enter: true } });
    expect(await handle(env(editor), { verb: "edit.select", params: { mode: "face", ids: [0, 1] } })).toEqual({
      mode: "face",
      ids: [0, 1],
    });
    expect(String((await handle(env(editor), { verb: "edit.select", params: { mode: "corner" } })).error)).toContain("mode");
  });

  it("runs an op and reports the new selection", async () => {
    const editor = meshEditor();
    await handle(env(editor), { verb: "edit", params: { object: "a1", enter: true } });
    const r = await handle(env(editor), {
      verb: "op",
      params: { op: "extrude_region", object: "a1", sel: { mode: "face", ids: [0] }, params: {} },
    });
    expect(r).toMatchObject({
      selection: { mode: "face", ids: [0] },
      report: { verts: 8, edges: 18, faces: 12, warnings: [] },
    });
    // The touched objects come back so a caller can find what an op created.
    expect(Array.isArray(r.objects)).toBe(true);
    expect(String((await handle(env(editor), { verb: "op", params: { op: "nope", sel: {}, params: {} } })).error)).toContain(
      "unknown op",
    );
    expect(String((await handle(env(editor), { verb: "op", params: { sel: {}, params: {} } })).error)).toContain('"op"');
  });

  it("reports edit mode from state while a session is open", async () => {
    const editor = meshEditor();
    expect((await handle(env(editor), { verb: "state" })).mode).toBe("object");
    await handle(env(editor), { verb: "edit", params: { object: "a1", enter: true } });
    const r = await handle(env(editor), { verb: "state" });
    expect(r.mode).toBe("edit");
    expect(r.edit).toEqual({ object: "a1", mode: "vertex", counts: { verts: 8, edges: 18, faces: 12 } });
  });
});

describe("dispatch undo and redo", () => {
  it("reports the depth after each", async () => {
    expect(await handle(env(stubEditor()), { verb: "undo" })).toEqual({ depth: 3, canUndo: true, canRedo: false });
    expect(await handle(env(stubEditor()), { verb: "redo" })).toEqual({ depth: 3, canUndo: true, canRedo: false });
  });

  it("rejects params", async () => {
    expect(String((await handle(env(stubEditor()), { verb: "undo", params: { x: 1 } })).error)).toContain("unknown key");
  });
});

describe("dispatch view", () => {
  it("rejects non-finite numbers", async () => {
    const editor = stubEditor();
    for (const eye of [[NaN, 0, 0], [Infinity, 0, 0], [0, 0, 1e7]]) {
      const r = await handle(env(editor), { verb: "view", params: { eye, target: [0, 0, 0] } });
      expect(String(r.error)).toContain("eye");
    }
    const r = await handle(env(editor), { verb: "view", params: { eye: [1, 2, 3], target: [0, 0, "x"] } });
    expect(String(r.error)).toContain("target");
  });

  it("rejects half an eye/target pair and mixed forms", async () => {
    const editor = stubEditor();
    expect(String((await handle(env(editor), { verb: "view", params: { eye: [1, 2, 3] } })).error)).toContain("together");
    expect(
      String((await handle(env(editor), { verb: "view", params: { eye: [1, 2, 3], target: [0, 0, 0], frame: "all" } })).error),
    ).toContain("exactly one");
  });

  it("rejects a bad axis and a bad fov", async () => {
    const editor = stubEditor();
    expect(String((await handle(env(editor), { verb: "view", params: { axis: "sideways" } })).error)).toContain("axis");
    expect(
      String((await handle(env(editor), { verb: "view", params: { eye: [1, 2, 3], target: [0, 0, 0], fov: 400 } })).error),
    ).toContain("fov");
  });

  it("moves the camera to an eye and target", async () => {
    const { view } = fakeView();
    views.set(0, view);
    const r = await handle(env(stubEditor()), { verb: "view", params: { eye: [100, 0, 50], target: [0, 0, 0], fov: 70 } });
    const camera = r.camera as CameraState;
    expect(camera.eye).toEqual([100, 0, 50]);
    expect(camera.target).toEqual([0, 0, 0]);
    expect(camera.fov).toBe(70);
    expect(camera.distance).toBeCloseTo(Math.hypot(100, 50), 9);
  });

  it("snaps to an axis view", async () => {
    const { view } = fakeView();
    views.set(0, view);
    const r = await handle(env(stubEditor()), { verb: "view", params: { axis: "top" } });
    expect((r.camera as CameraState).kind).toBe("top");
  });

  it("frames nothing on an empty scene", async () => {
    const { view } = fakeView();
    views.set(0, view);
    const r = await handle(env(stubEditor()), { verb: "view", params: { frame: "all" } });
    expect(String(r.error)).toContain("empty");
  });
});

describe("dispatch shading", () => {
  beforeEach(() => {
    setShading(defaultShading());
    setUi({ showLightmaps: false });
  });

  it("accepts textured and the lightmap toggle", async () => {
    const r = await handle(env(stubEditor()), { verb: "shading", params: { mode: "textured", lightmaps: true } });
    expect(r).toMatchObject({ mode: "textured", lightmaps: true, overlays: false });
  });

  it("accepts every mode with x-ray, overlay, colour and culling", async () => {
    for (const mode of ["textured", "solid", "wireframe"]) {
      const r = await handle(
        env(stubEditor()),
        { verb: "shading", params: { mode, xray: 0.5, wire_overlay: true, color: "uv_checker", single: [1, 0.5, 0], cull: true } },
      );
      expect(r).toMatchObject({ mode, xray: 0.5, wire_overlay: true, color: "uv_checker", single: [1, 0.5, 0], cull: true });
    }
    const solid = await handle(env(stubEditor()), { verb: "shading", params: { mode: "solid", color: "object" } });
    expect(solid).toMatchObject({ mode: "solid", color: "object" });
  });

  it("reports the shading in state", async () => {
    await handle(env(stubEditor()), { verb: "shading", params: { mode: "solid", xray: 0.25 } });
    const r = await handle(env(stubEditor()), { verb: "state" });
    expect(r.shading).toMatchObject({ mode: "solid", xray: 0.25 });
  });

  it("rejects a bad mode, x-ray, colour and single", async () => {
    expect(String((await handle(env(stubEditor()), { verb: "shading", params: { mode: "flat" } })).error)).toContain("mode");
    expect(String((await handle(env(stubEditor()), { verb: "shading", params: { mode: "solid", xray: 2 } })).error)).toContain("xray");
    expect(String((await handle(env(stubEditor()), { verb: "shading", params: { mode: "solid", xray: true } })).error)).toContain("xray");
    expect(String((await handle(env(stubEditor()), { verb: "shading", params: { mode: "solid", color: "vertex" } })).error)).toContain("color");
    expect(String((await handle(env(stubEditor()), { verb: "shading", params: { mode: "solid", single: [2, 0, 0] } })).error)).toContain("single");
    expect(String((await handle(env(stubEditor()), { verb: "shading", params: { mode: "solid", wire_overlay: 1 } })).error)).toContain("wire_overlay");
  });

  it("rejects unknown keys", async () => {
    const r = await handle(env(stubEditor()), { verb: "shading", params: { mode: "textured", flat: true } });
    expect(String(r.error)).toContain("unknown key");
  });
});

describe("dispatch wait", () => {
  it("resolves once a frame presents with nothing pending", async () => {
    const editor = stubEditor();
    const pending = handle(env(editor), { verb: "wait", params: { timeout_ms: 1000 } });
    markPresented();
    const r = await pending;
    expect(r.idle).toBe(true);
  });

  it("times out instead of hanging", async () => {
    const editor = stubEditor({ pendingLoads: () => 1 });
    const r = await handle(env(editor), { verb: "wait", params: { timeout_ms: 60 } });
    expect(String(r.error)).toContain("timed out");
  });

  it("rejects anything but an idle wait and a bad timeout", async () => {
    const editor = stubEditor();
    expect(String((await handle(env(editor), { verb: "wait", params: { idle: false } })).error)).toContain("idle");
    expect(String((await handle(env(editor), { verb: "wait", params: { timeout_ms: 0 } })).error)).toContain(
      "timeout_ms",
    );
  });
});

describe("dispatch screenshot", () => {
  it("rejects oversize shots without touching the GPU", async () => {
    const capture = vi.fn(async () => new Uint8Array(0));
    const { view } = fakeView(capture);
    views.set(0, view);
    for (const params of [{ width: 0, height: 64 }, { width: 5000, height: 64 }, { width: 64.5, height: 64 }]) {
      const r = await handle(env(stubEditor()), { verb: "screenshot", params });
      expect(String(r.error)).toMatch(/width|height/);
    }
    expect(capture).not.toHaveBeenCalled();
  });

  it("needs a mounted viewport", async () => {
    const r = await handle(env(stubEditor()), { verb: "screenshot", params: { width: 64, height: 64 } });
    expect(String(r.error)).toContain("no viewport");
  });

  it("returns base64 PNG bytes from the view", async () => {
    const { view } = fakeView(async () => new Uint8Array([1, 2, 3, 4]));
    views.set(0, view);
    const r = await handle(env(stubEditor()), { verb: "screenshot", params: { width: 64, height: 64 } });
    expect(r.width).toBe(64);
    expect(typeof r.png).toBe("string");
    expect(atob(r.png as string).length).toBe(4);
  });

  it("turns a GPU failure into a reply", async () => {
    const { view } = fakeView();
    views.set(0, view);
    const r = await handle(env(stubEditor()), { verb: "screenshot", params: { width: 64, height: 64 } });
    expect(String(r.error)).toContain("no gpu");
  });
});
