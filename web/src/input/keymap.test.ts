// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { COMMANDS, type CommandId, type KeyContext, chordFor, chordOf, conflictFor, defaultKeymap, isTextTarget, keyChord, presetBindings, rebind, resolve } from "./keymap";

const press = (init: Partial<KeyboardEvent>): KeyboardEvent =>
  ({ key: "", code: "", ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, ...init }) as KeyboardEvent;

const VIEW: KeyContext[] = ["viewport", "window"];
const WINDOW: KeyContext[] = ["window"];

describe("keymap presets", () => {
  for (const preset of ["blender", "legacy"] as const) {
    it(`${preset}: no chord fires two commands in overlapping contexts`, () => {
      const seen = new Map<string, string>();
      for (const b of presetBindings(preset)) {
        const chord = chordOf(b.chord);
        expect(chord.length, `${b.command} has an empty chord`).toBeGreaterThan(0);
        // A window binding sits under every scope; an edit_mesh binding shadows
        // the viewport deterministically, so only same-scope pairs collide.
        for (const ctx of b.context === "window" ? ["window", "viewport"] : [b.context]) {
          const key = `${ctx}:${chord}`;
          const other = seen.get(key);
          expect(other === undefined || other === b.command, `${b.command} and ${other} both bind ${chord}`).toBe(true);
          seen.set(key, b.command);
        }
      }
    });

    it(`${preset}: binds only known commands`, () => {
      const ids = new Set(COMMANDS.map((c) => c.id));
      for (const b of presetBindings(preset)) expect(ids.has(b.command), b.command).toBe(true);
    });
  }

  it("gives every command a label", () => {
    for (const c of COMMANDS) expect(c.label.length).toBeGreaterThan(2);
  });
});

describe("Blender keymap", () => {
  const km = defaultKeymap("blender");

  it("maps the viewport keys only over a viewport", () => {
    expect(resolve(km, VIEW, press({ code: "KeyG" }))).toBe("grab");
    expect(resolve(km, WINDOW, press({ code: "KeyG" }))).toBeNull();
    expect(resolve(km, VIEW, press({ code: "KeyA", altKey: true }))).toBe("deselect");
    expect(resolve(km, VIEW, press({ code: "KeyD", shiftKey: true }))).toBe("duplicate");
    expect(resolve(km, VIEW, press({ code: "KeyZ", ctrlKey: true, shiftKey: true }))).toBe("redo");
  });

  it("keeps the numpad apart from the top row", () => {
    expect(resolve(km, VIEW, press({ code: "Numpad7" }))).toBe("view_top");
    expect(resolve(km, VIEW, press({ code: "Numpad7", ctrlKey: true }))).toBe("view_bottom");
    expect(resolve(km, VIEW, press({ code: "Digit7" }))).toBeNull();
    expect(resolve(km, VIEW, press({ code: "NumpadDecimal" }))).toBe("frame");
  });

  it("emulates the numpad on the top row when asked", () => {
    expect(resolve(km, VIEW, press({ code: "Digit1" }), { emulateNumpad: true })).toBe("view_front");
    expect(resolve(km, VIEW, press({ code: "Digit3", ctrlKey: true }), { emulateNumpad: true })).toBe("view_left");
  });

  it("reads the physical key, so a layout's shifted symbol still matches", () => {
    expect(resolve(km, VIEW, press({ code: "Backquote", key: "~", shiftKey: true }))).toBe("walk");
    expect(resolve(km, VIEW, press({ code: "Backquote", key: "`" }))).toBe("view_pie");
  });

  it("ignores a bare modifier", () => {
    expect(resolve(km, VIEW, press({ code: "ShiftLeft", key: "Shift", shiftKey: true }))).toBeNull();
  });
});

describe("edit_mesh keymap", () => {
  const EDIT: KeyContext[] = ["edit_mesh", "viewport", "window"];
  const OBJECT: KeyContext[] = ["viewport", "window"];
  const WINDOW: KeyContext[] = ["window"];

  for (const preset of ["blender", "legacy"] as const) {
    const km = defaultKeymap(preset);

    it(`${preset}: 1/2/3 switch element mode only while editing`, () => {
      expect(resolve(km, EDIT, press({ code: "Digit1" }))).toBe("mesh_mode_vertex");
      expect(resolve(km, EDIT, press({ code: "Digit2" }))).toBe("mesh_mode_edge");
      expect(resolve(km, EDIT, press({ code: "Digit3" }))).toBe("mesh_mode_face");
      if (preset === "legacy") {
        expect(resolve(km, OBJECT, press({ code: "Digit1" }))).toBe("tool_select");
        expect(resolve(km, OBJECT, press({ code: "Digit2" }))).toBe("tool_move");
        expect(resolve(km, OBJECT, press({ code: "Digit3" }))).toBe("tool_rotate");
      } else {
        expect(resolve(km, OBJECT, press({ code: "Digit1" }))).toBeNull();
      }
    });

    it(`${preset}: F9 adjusts the last operation and F1 helps`, () => {
      expect(resolve(km, WINDOW, press({ code: "F9" }))).toBe("adjust_last");
      expect(resolve(km, WINDOW, press({ code: "F1" }))).toBe("help");
    });

    it(`${preset}: Tab toggles edit mode in and out`, () => {
      expect(resolve(km, OBJECT, press({ code: "Tab" }))).toBe("mesh_toggle_edit");
      expect(resolve(km, EDIT, press({ code: "Tab" }))).toBe("mesh_toggle_edit");
    });

    it(`${preset}: the modelling chords resolve in edit mode`, () => {
      const chord = (init: Partial<KeyboardEvent>): CommandId | null => resolve(km, EDIT, press(init));
      expect(chord({ code: "KeyR", ctrlKey: true })).toBe("mesh_loop_cut");
      expect(chord({ code: "KeyI" })).toBe("mesh_inset");
      expect(chord({ code: "KeyB", ctrlKey: true })).toBe("mesh_bevel");
      expect(chord({ code: "KeyB", ctrlKey: true, shiftKey: true })).toBe("mesh_bevel_vertices");
      expect(chord({ code: "KeyA", shiftKey: true })).toBe("mesh_add_menu");
      expect(chord({ code: "KeyU" })).toBe("mesh_uv_menu");
      expect(chord({ code: "KeyE", ctrlKey: true })).toBe("mesh_edge_menu");
      expect(chord({ code: "KeyF", ctrlKey: true })).toBe("mesh_face_menu");
      expect(chord({ code: "KeyV", ctrlKey: true })).toBe("mesh_vertex_menu");
    });

    it(`${preset}: edit mode shadows the object-mode meaning of shared chords`, () => {
      if (preset === "blender") expect(resolve(km, OBJECT, press({ code: "KeyA", shiftKey: true }))).toBe("add_menu");
      expect(resolve(km, EDIT, press({ code: "KeyA", shiftKey: true }))).toBe("mesh_add_menu");
      expect(resolve(km, WINDOW, press({ code: "KeyB", ctrlKey: true, shiftKey: true }))).toBe("bake");
      expect(resolve(km, EDIT, press({ code: "KeyB", ctrlKey: true, shiftKey: true }))).toBe("mesh_bevel_vertices");
    });

    it(`${preset}: the op chords resolve in edit mode`, () => {
      const chord = (init: Partial<KeyboardEvent>): CommandId | null => resolve(km, EDIT, press(init));
      expect(chord({ code: "KeyE" })).toBe("mesh_extrude");
      expect(chord({ code: "KeyX" })).toBe("mesh_delete");
      expect(chord({ code: "KeyM" })).toBe("mesh_merge_menu");
      expect(chord({ code: "KeyF" })).toBe("mesh_fill");
      expect(chord({ code: "KeyP" })).toBe("mesh_separate");
      expect(chord({ code: "KeyH" })).toBe("mesh_hide");
      expect(chord({ code: "KeyH", altKey: true })).toBe("mesh_reveal");
      expect(chord({ code: "KeyL" })).toBe("mesh_select_linked");
      expect(chord({ code: "KeyL", ctrlKey: true })).toBe("mesh_linked_expand");
      expect(chord({ code: "NumpadAdd", ctrlKey: true })).toBe("mesh_grow");
      expect(chord({ code: "NumpadSubtract", ctrlKey: true })).toBe("mesh_shrink");
      expect(chord({ code: "KeyN", shiftKey: true })).toBe("mesh_recalc_outside");
      expect(chord({ code: "KeyN", altKey: true })).toBe("mesh_normals_menu");
      expect(chord({ code: "KeyA" })).toBe("select_all");
      expect(chord({ code: "KeyA", altKey: true })).toBe("deselect");
      expect(chord({ code: "KeyI", ctrlKey: true })).toBe("select_invert");
      expect(chord({ code: "KeyG" })).toBe("grab");
      expect(chord({ code: "KeyR" })).toBe("rotate");
      expect(chord({ code: "KeyS" })).toBe("resize");
    });

    it(`${preset}: X deletes objects outside edit mode`, () => {
      if (preset === "legacy") {
        expect(resolve(km, OBJECT, press({ code: "KeyX" }))).toBe("tool_voxel");
        expect(resolve(km, OBJECT, press({ code: "Delete" }))).toBe("delete");
      } else {
        expect(resolve(km, OBJECT, press({ code: "KeyX" }))).toBe("delete");
      }
    });
  }
});

describe("chords", () => {
  it("normalises a written chord", () => {
    expect(chordOf("shift + ctrl + s")).toBe("Ctrl+Shift+S");
    expect(chordOf("'")).toBe("Quote");
    expect(chordOf("return")).toBe("Enter");
    expect(chordOf("numpad7")).toBe("Numpad7");
    expect(chordOf("")).toBe("");
    expect(chordOf("Ctrl")).toBe("");
  });

  it("reads synthetic events that only carry a key", () => {
    expect(keyChord(press({ key: "s", ctrlKey: true, shiftKey: true }))).toBe("Ctrl+Shift+S");
    expect(keyChord(press({ key: " " }))).toBe("Space");
  });

  it("refuses to rebind onto a chord another command owns", () => {
    const km = defaultKeymap("blender");
    expect(conflictFor(km, "grid_toggle", "viewport", "Ctrl+S")).toBe("save");
    const refused = rebind(km, "grid_toggle", "Ctrl+S");
    expect(typeof refused).toBe("string");
    const allowed = rebind(km, "grid_toggle", "Ctrl+Alt+Y");
    expect(typeof allowed).not.toBe("string");
    if (typeof allowed !== "string") expect(chordFor(allowed, "grid_toggle")).toBe("Ctrl+Alt+Y");
    expect(rebind(km, "grid_toggle", "")).toBe("a binding needs at least one key");
  });

  it("lets a command keep its own chord", () => {
    const km = defaultKeymap("blender");
    const again = rebind(km, "undo", "Ctrl+Z");
    expect(typeof again).not.toBe("string");
  });

  it("knows when the keystroke belongs to a text field", () => {
    const input = document.createElement("input");
    const div = document.createElement("div");
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    document.body.append(input, div, editable);
    expect(isTextTarget(input)).toBe(true);
    expect(isTextTarget(div)).toBe(false);
    expect(isTextTarget(editable)).toBe(true);
    expect(isTextTarget(null)).toBe(false);
  });
});
