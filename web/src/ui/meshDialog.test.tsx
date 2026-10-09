import { describe, expect, it } from "vitest";
import { render } from "solid-js/web";
import type { Editor } from "../state/editor";
import { select, setCursorWorld, state } from "../state/stores";
import { NewMeshDialog } from "./dialogs";
import type { MeshShapeId } from "../render/meshgen";

interface Call {
  spec: unknown;
  options: Record<string, unknown>;
}

/** The dialog only drives `meshCreate`, `projectMaterials` and `note`. */
function stubEditor(): { editor: Editor; calls: Call[]; notes: string[] } {
  const calls: Call[] = [];
  const notes: string[] = [];
  const editor = {
    meshCreate: (spec: unknown, options: Record<string, unknown>) => {
      calls.push({ spec, options });
      return ["mesh-1"];
    },
    projectMaterials: () => ["world/props/rock"],
    note: (message: string) => notes.push(message),
  } as unknown as Editor;
  return { editor, calls, notes };
}

function mount(editor: Editor): { root: HTMLDivElement; dispose: () => void } {
  const root = document.createElement("div");
  document.body.appendChild(root);
  const dispose = render(() => NewMeshDialog({ editor, onClose: () => undefined }), root);
  return { root, dispose };
}

function setValue(root: HTMLElement, selector: string, value: string): void {
  const node = root.querySelector<HTMLInputElement>(selector);
  if (!node) throw new Error(`no field ${selector}`);
  node.value = value;
  node.dispatchEvent(new Event("input", { bubbles: true }));
}

describe("new mesh dialog", () => {
  it("offers every shape the core can build", () => {
    const { editor } = stubEditor();
    const { root, dispose } = mount(editor);
    const options = [...root.querySelectorAll<HTMLOptionElement>("#mesh-shape option")].map((o) => o.value);
    expect(options).toHaveLength(13);
    expect(options).toContain("lathe");
    expect(options).toContain("raw");
    dispose();
    root.remove();
  });

  it("hands the chosen shape's spec to the core and selects the result", () => {
    const { editor, calls } = stubEditor();
    const { root, dispose } = mount(editor);
    const shape = root.querySelector<HTMLSelectElement>("#mesh-shape");
    if (!shape) throw new Error("no shape picker");
    shape.value = "stairs";
    shape.dispatchEvent(new Event("change"));
    // The steps field is the first numeric one the Stairs shape offers.
    setValue(root, "#f-steps", "12");
    (root.querySelector<HTMLButtonElement>(".row button") as HTMLButtonElement).click();
    expect(calls).toHaveLength(1);
    expect(calls[0].spec).toEqual({ shape: "stairs", steps: 12, run: 128, rise: 64, width: 64 });
    expect(calls[0].options.material).toBe("world/dev/dev_gray_512");
    expect(calls[0].options.collision).toBe("triangles");
    expect(state.selection.ids).toEqual(["mesh-1"]);
    dispose();
    root.remove();
  });

  it("blocks the create button and explains a value the core would refuse", () => {
    const { editor, calls, notes } = stubEditor();
    const { root, dispose } = mount(editor);
    setValue(root, "#f-size-x-y-z", "0 64 64");
    const create = root.querySelector<HTMLButtonElement>(".row button") as HTMLButtonElement;
    expect(create.disabled).toBe(true);
    expect(root.querySelector(".invalid-note")?.textContent).toMatch(/out of range/);
    create.click();
    expect(calls).toHaveLength(0);
    expect(notes).toHaveLength(0);
    dispose();
    root.remove();
  });

  it("refills the fields when another shape is picked", () => {
    const { editor, calls } = stubEditor();
    const { root, dispose } = mount(editor);
    setValue(root, "#f-size-x-y-z", "8 8 8");
    const shape = root.querySelector<HTMLSelectElement>("#mesh-shape");
    if (!shape) throw new Error("no shape picker");
    shape.value = "sphere" satisfies MeshShapeId;
    shape.dispatchEvent(new Event("change"));
    expect(root.querySelector<HTMLInputElement>("#f-size-x-y-z")).toBeNull();
    expect(root.querySelector<HTMLInputElement>("#f-radius")?.value).toBe("32");
    (root.querySelector<HTMLButtonElement>(".row button") as HTMLButtonElement).click();
    expect(calls[0].spec).toEqual({ shape: "sphere", radius: 32, segments: 16, rings: 8 });
    dispose();
    root.remove();
  });

  it("places the mesh under the cursor and selects it", () => {
    const { editor, calls } = stubEditor();
    setCursorWorld([12, -4, 40]);
    const { root, dispose } = mount(editor);
    (root.querySelector<HTMLButtonElement>(".row button") as HTMLButtonElement).click();
    expect(calls[0].options.position).toEqual([12, -4, 40]);
    select([]);
    setCursorWorld(null);
    dispose();
    root.remove();
  });

  it("leads the material picker with the dev set", () => {
    const { editor } = stubEditor();
    const { root, dispose } = mount(editor);
    const groups = [...root.querySelectorAll("optgroup")].map((g) => g.getAttribute("label"));
    expect(groups[0]).toBe("Dev materials");
    expect(groups).toContain("Map materials");
    dispose();
    root.remove();
  });
});
