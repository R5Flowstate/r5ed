import { describe, expect, it } from "vitest";
import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import type { Core, EditBuffers, MeshBuffers } from "../core/api";
import type { DocObject } from "../doc/model";
import { type EntitySchema, KEY_TYPES, parseSchema, observedFlagBits, orderedKeys, searchClasses, type KeyType } from "../doc/schema";
import { EntityForm, entityCommand, withKeyvalues } from "./EntityForm";

/**
 * A Core that answers the shape `EntityForm` depends on, with no behaviour
 * behind it: the form is driven entirely by the schema and the object it is
 * given, so the fake exists only to satisfy the interface.
 */
class FakeCore implements Core {
  applied: string[] = [];
  private nextId = 1;

  load(docJson: string): string {
    return docJson;
  }

  save(): string {
    return JSON.stringify({ format: 1, meta: {}, layers: [], objects: [], settings: {} });
  }

  apply(commandJson: string): string[] {
    this.applied.push(commandJson);
    return [`id-${this.nextId++}`];
  }

  undo(): string[] {
    return [];
  }

  redo(): string[] {
    return [];
  }

  canUndo(): boolean {
    return false;
  }

  canRedo(): boolean {
    return false;
  }

  object(): string | null {
    return null;
  }

  objectIds(): string[] {
    return [];
  }

  objectMesh(): MeshBuffers | null {
    return null;
  }

  objectEdges(): { positions: Float32Array; edges: Uint32Array } | null {
    return null;
  }

  soupEdges(): { positions: Float32Array; edges: Uint32Array } | null {
    return null;
  }

  pick(): null {
    return null;
  }

  brushBox(): string {
    return "{}";
  }

  brushClip(): string {
    return "{}";
  }

  brushHollow(): string {
    return "{}";
  }

  brushCarve(): string {
    return "{}";
  }

  brushMergeConvex(): string {
    return "{}";
  }

  brushExtrudeFace(): string {
    return "{}";
  }

  brushMoveVertex(): string {
    return "{}";
  }

  brushResize(): string {
    return JSON.stringify({ op: "batch", label: "resize", commands: [] });
  }
  brushBounds(): number[] {
    return [0, 0, 0, 1, 1, 1];
  }
  meshCreate(): string {
    return JSON.stringify({ op: "batch", label: "mesh", commands: [] });
  }
  meshOp(): string {
    return JSON.stringify({ command: { op: "batch", label: "mesh", commands: [] }, selection: { mode: "face", ids: [] }, report: { verts: 0, edges: 0, faces: 0, warnings: [] } });
  }
  meshMakeEditable(): string {
    return JSON.stringify({ op: "batch", label: "mesh", commands: [] });
  }
  brushToMesh(): string {
    return JSON.stringify({ op: "batch", label: "mesh", commands: [] });
  }
  editBegin(): string {
    return JSON.stringify({ verts: 0, edges: 0, faces: 0 });
  }
  editEnd(): void {
  }
  editSelect(): string {
    return JSON.stringify({ mode: "vertex", ids: [] });
  }
  editBuffers(): EditBuffers | null {
    return null;
  }
  editPick(): string {
    return JSON.stringify({ kind: "none" });
  }
  editPreview(): string {
    return JSON.stringify({ selection: { mode: "vertex", ids: [] }, report: { verts: 0, edges: 0, faces: 0, warnings: [] } });
  }
  editCommit(): string {
    return JSON.stringify({ op: "batch", label: "mesh", commands: [] });
  }
  editHide(): void {
  }
  editReveal(): void {
  }
  terrainGenerate(): Float32Array {
    return new Float32Array(0);
  }

  terrainSculpt(): string {
    return "{}";
  }

  terrainAutoPaint(): string {
    return "{}";
  }

  terrainScatter(): string {
    return "{}";
  }

  putFile(): void {
    /* the form never writes files */
  }

  getFile(): Uint8Array | null {
    return null;
  }

  fileNames(): string[] {
    return [];
  }

  reset(): string {
    return this.save();
  }

  schema(): string {
    return "{}";
  }

  validate(): string {
    return "[]";
  }

  compile(): never {
    throw new Error("the form never compiles");
  }

  bakePreview(): never {
    throw new Error("the form never bakes");
  }

  importObj(): string {
    return "";
  }

  importVmf(): string {
    return "{}";
  }

  importQuakeMap(): string {
    return "{}";
  }

  importReMap(): string {
    return "{}";
  }

  importGltf(): string {
    return "{}";
  }
}

/** One key per KeyType, plus the values a shipped map would show. */
function schemaWithEveryKeyType(): EntitySchema {
  const keys: Record<string, unknown> = {};
  const values: Record<KeyType, Record<string, unknown>> = {
    int: { "8": 0.5, "16": 0.3, "24": 0.2 },
    float: { "1.5": 0.6, "0.25": 0.4 },
    bool: { "0": 0.7, "1": 0.3 },
    string: { "hello": 0.8 },
    vec3: { "0 0 0": 0.5, "1 2 3": 0.5 },
    angles: { "0 90 0": 1 },
    color: { "255 0 0": 1 },
    model: { "mdl/props/crate.rmdl": 1 },
    material: { "props/concrete/rock": 1 },
    sound: { "sounds/footstep": 1 },
    target: { "door_main": 1 },
    flags: { "1": 0.5, "4": 0.5 },
    enum: { "small": 0.5, "large": 0.5 },
  };
  for (const ty of KEY_TYPES) {
    keys[`key_${ty}`] = {
      name: `key_${ty}`,
      ty,
      observed_values: Object.keys(values[ty]),
      default: Object.keys(values[ty])[0],
      required: ty === "string",
      sources: ["maps"],
    };
  }
  const raw = {
    classes: {
      info_target: {
        classname: "info_target",
        cpp_class: "CInfoTarget",
        base_chain: ["CBaseEntity"],
        kind: "point",
        partitions: { main: 12 },
        keys,
        inputs: [],
        outputs: [],
        sources: { engine_factory: true, engine_datamap: true, maps: 12, scripts: 0 },
        example_count: 12,
      },
      trigger_multiple: {
        classname: "trigger_multiple",
        cpp_class: "CTriggerMultiple",
        base_chain: ["CTriggerOnce", "CBaseTrigger"],
        kind: "brush",
        partitions: { main: 4 },
        keys: { spawnflags: { name: "spawnflags", ty: "flags", observed_values: ["1"], default: "0", required: false, sources: ["engine_datamap"] } },
        inputs: [],
        outputs: ["OnTrigger"],
        sources: { engine_factory: true, engine_datamap: true, maps: 4, scripts: 1 },
        example_count: 4,
      },
    },
  };
  return parseSchema(JSON.stringify(raw));
}

function entityObject(classname: string, keyvalues: Record<string, string> = {}): DocObject {
  return {
    id: "11111111-2222-4333-8444-555555555555",
    name: "target",
    layer: "l1",
    parent: null,
    transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    hidden: false,
    locked: false,
    kind: { type: "entity", entity: { classname, keyvalues, partition: null, links: {} } },
  };
}

interface Mounted {
  dispose: () => void;
  changes: DocObject[];
  root: HTMLDivElement;
}

/**
 * The form is controlled: the parent keeps the object it was handed and feeds
 * the edited one back, which is what the editor does after an apply.
 */
function mount(schema: EntitySchema, initial: DocObject | null, issues: { id: string | null; message: string }[] = []): Mounted {
  const root = document.createElement("div");
  document.body.appendChild(root);
  const changes: DocObject[] = [];
  const [object, setObject] = createSignal<DocObject | null>(initial);
  const dispose = render(
    () =>
      EntityForm({
        schema,
        get object() {
          return object();
        },
        issues,
        suggestions: { model: ["mdl/props/crate.rmdl"] },
        targets: [{ id: "target-1", name: "door" }],
        onCreate: null,
        onChange: (next) => {
          changes.push(next);
          setObject(() => next);
        },
      }),
    root,
  );
  return { dispose, changes, root };
}

function widget(root: HTMLElement, ty: KeyType): HTMLElement {
  const found = root.querySelector<HTMLElement>(`[data-keytype="${ty}"]`);
  if (!found) throw new Error(`no widget for ${ty}`);
  return found;
}

describe("entity form outputs", () => {
  it("lists every OnX output the schema declares and writes the chosen target", () => {
    const schema = schemaWithEveryKeyType();
    const { dispose, changes, root } = mount(schema, entityObject("trigger_multiple"));
    const output = root.querySelector<HTMLSelectElement>('#link-OnTrigger');
    expect(output).not.toBeNull();
    const options = [...(output as HTMLSelectElement).options].map((o) => o.value);
    expect(options).toEqual(["", "target-1"]);
    (output as HTMLSelectElement).value = "target-1";
    (output as HTMLSelectElement).dispatchEvent(new Event("change"));
    expect(changes).toHaveLength(1);
    const entity = changes[0].kind as { type: "entity"; entity: { links: Record<string, string> } };
    expect(entity.entity.links).toEqual({ OnTrigger: "target-1" });
    dispose();
  });

  it("drops a link the user cleared", () => {
    const schema = schemaWithEveryKeyType();
    const linked = entityObject("trigger_multiple");
    (linked.kind as { entity: { links: Record<string, string> } }).entity.links = { OnTrigger: "target-1" };
    const { dispose, changes, root } = mount(schema, linked);
    const output = root.querySelector<HTMLSelectElement>('#link-OnTrigger');
    expect(output?.value).toBe("target-1");
    (output as HTMLSelectElement).value = "";
    (output as HTMLSelectElement).dispatchEvent(new Event("change"));
    const entity = changes[0].kind as { type: "entity"; entity: { links: Record<string, string> } };
    expect(entity.entity.links).toEqual({});
    dispose();
  });

  it("keeps a stored link the schema does not list", () => {
    const schema = schemaWithEveryKeyType();
    const object = entityObject("info_target");
    (object.kind as { entity: { links: Record<string, string> } }).entity.links = { OnCustom: "target-1" };
    const { dispose, root } = mount(schema, object);
    expect(root.querySelector("#link-OnCustom")).not.toBeNull();
    expect(root.querySelector("#link-OnTrigger")).toBeNull();
    dispose();
  });

  it("shows no output editor for a class with no outputs", () => {
    const schema = schemaWithEveryKeyType();
    const { dispose, root } = mount(schema, entityObject("info_target"));
    expect(root.querySelector(".entity-links")).toBeNull();
    dispose();
  });

  it("keeps the links when a key is edited", () => {
    const schema = schemaWithEveryKeyType();
    const object = entityObject("trigger_multiple", { spawnflags: "1" });
    (object.kind as { entity: { links: Record<string, string> } }).entity.links = { OnTrigger: "target-1" };
    const { dispose, changes, root } = mount(schema, object);
    const field = root.querySelector<HTMLInputElement>('[data-keytype="flags"] input');
    (field as HTMLInputElement).checked = false;
    (field as HTMLInputElement).dispatchEvent(new Event("change"));
    const entity = changes[0].kind as { type: "entity"; entity: { links: Record<string, string>; keyvalues: Record<string, string> } };
    expect(entity.entity.links).toEqual({ OnTrigger: "target-1" });
    dispose();
  });
});

describe("entity form creation", () => {
  it("offers the class picker when there is no object and reports the pick", () => {
    const schema = schemaWithEveryKeyType();
    const root = document.createElement("div");
    document.body.appendChild(root);
    const created: string[] = [];
    const dispose = render(
      () =>
        EntityForm({
          schema,
          object: null,
          issues: [],
          suggestions: {},
          targets: [],
          onCreate: (classname) => created.push(classname),
          onChange: () => undefined,
        }),
      root,
    );
    const options = [...root.querySelectorAll(".class-list button")].map((b) => b.textContent ?? "");
    expect(options.some((o) => o.includes("info_target"))).toBe(true);
    expect(options.some((o) => o.includes("trigger_multiple"))).toBe(true);
    const target = [...root.querySelectorAll<HTMLButtonElement>(".class-list button")].find((b) =>
      (b.textContent ?? "").includes("trigger_multiple"),
    );
    (target as HTMLButtonElement).click();
    expect(created).toEqual(["trigger_multiple"]);
    dispose();
    root.remove();
  });
});


describe("entity form widgets", () => {
  it("renders one widget per KeyType from the schema", () => {
    const schema = schemaWithEveryKeyType();
    const mounted = mount(schema, entityObject("info_target"));
    for (const ty of KEY_TYPES) {
      const node = widget(mounted.root, ty);
      expect(node.getAttribute("data-key")).toBe(`key_${ty}`);
    }
    mounted.dispose();
  });

  it("orders required keys first and the rest alphabetically", () => {
    const schema = schemaWithEveryKeyType();
    const names = orderedKeys(schema.classes.info_target).map((k) => k.name);
    expect(names[0]).toBe("key_string");
    const rest = names.slice(1);
    expect(rest).toEqual([...rest].sort());
  });

  it("offers the observed values for an enum and the shipped models for a model key", () => {
    const schema = schemaWithEveryKeyType();
    const mounted = mount(schema, entityObject("info_target"));
    const options = [...widget(mounted.root, "enum").querySelectorAll("option")].map((o) => o.getAttribute("value"));
    expect(options).toEqual(["large", "small"]);
    const list = widget(mounted.root, "model").getAttribute("list");
    const datalist = mounted.root.querySelector(`datalist[id="${list}"]`);
    expect(datalist).not.toBeNull();
    expect([...(datalist as HTMLElement).querySelectorAll("option")].map((o) => o.getAttribute("value"))).toContain("mdl/props/crate.rmdl");
    mounted.dispose();
  });

  it("shows one checkbox per observed flag bit", () => {
    const schema = schemaWithEveryKeyType();
    const mounted = mount(schema, entityObject("info_target", { key_flags: "5" }));
    const bits = [...widget(mounted.root, "flags").querySelectorAll("input[type=checkbox]")];
    expect(bits).toHaveLength(2);
    expect(bits.map((b) => (b as HTMLInputElement).checked)).toEqual([true, true]);
    mounted.dispose();
  });

  it("reports validation messages against their key", () => {
    const schema = schemaWithEveryKeyType();
    const object = entityObject("info_target");
    const mounted = mount(schema, object, [
      { id: object.id, message: "key_string must not be empty" },
      { id: object.id, message: "key_int is out of range" },
    ]);
    const messages = [...mounted.root.querySelectorAll(".field-message")].map((m) => m.textContent?.trim());
    expect(messages).toContain("key_string must not be empty");
    expect(messages).toContain("key_int is out of range");
    mounted.dispose();
  });

  it("falls back to free-text keys for a class the schema lacks", () => {
    const schema = schemaWithEveryKeyType();
    const mounted = mount(schema, entityObject("not_in_schema", { adhoc: "7" }));
    expect(mounted.root.textContent).toContain("this class is not in the schema");
    expect(widget(mounted.root, "string").getAttribute("data-key")).toBe("adhoc");
    mounted.dispose();
  });

  it("shows the class picker when nothing is selected", () => {
    const schema = schemaWithEveryKeyType();
    const mounted = mount(schema, null);
    const list = mounted.root.querySelector(".class-list");
    expect(list).not.toBeNull();
    const names = [...(list as HTMLElement).querySelectorAll(".classname")].map((n) => n.textContent);
    // Most-used class first when the query is empty.
    expect(names).toEqual(["info_target", "trigger_multiple"]);
    mounted.dispose();
  });
});

describe("entity form editing", () => {
  it("writes a number, a triple, a colour and a flag back into the command", () => {
    const schema = schemaWithEveryKeyType();
    const mounted = mount(schema, entityObject("info_target", { key_int: "8", key_vec3: "0 0 0", key_flags: "0" }));
    const set = (node: HTMLInputElement, value: string): void => {
      node.value = value;
      node.dispatchEvent(new Event("change", { bubbles: true }));
    };
    set(widget(mounted.root, "int") as HTMLInputElement, "24");
    const triple = [...mounted.root.querySelectorAll<HTMLInputElement>('[data-keytype="vec3"]')];
    set(triple[1], "5");
    const flag = widget(mounted.root, "flags").querySelector<HTMLInputElement>('input[data-flag="4"]');
    if (!flag) throw new Error("no bit 4 checkbox");
    flag.checked = true;
    flag.dispatchEvent(new Event("change", { bubbles: true }));

    expect(mounted.changes).toHaveLength(3);
    const last = mounted.changes[2];
    const command = JSON.parse(entityCommand(last));
    expect(command.op).toBe("replace_objects");
    expect(command.objects).toHaveLength(1);
    expect(command.objects[0].kind.type).toBe("entity");
    expect(command.objects[0].kind.keyvalues).toEqual({
      key_int: "24",
      key_vec3: "0 5 0",
      key_flags: "4",
    });
    expect(command.objects[0].id).toBe("11111111-2222-4333-8444-555555555555");
    expect(command.objects[0].transform).toEqual({ position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] });
    mounted.dispose();
  });

  it("drops keys cleared back to empty", () => {
    const object = entityObject("info_target", { key_string: "hello", keep: "1" });
    const next = withKeyvalues(object, { key_string: "", keep: "1" });
    expect(next.kind.type === "entity" ? next.kind.entity.keyvalues : null).toEqual({ keep: "1" });
  });

  it("leaves a non-entity object untouched", () => {
    const object: DocObject = { ...entityObject("info_target"), kind: { type: "group" } };
    expect(withKeyvalues(object, { a: "1" })).toBe(object);
  });

  it("applies each edit through the core as a ReplaceObjects command", () => {
    const core = new FakeCore();
    expect(core.applied).toHaveLength(0);
    const command = entityCommand(entityObject("info_target", { a: "1" }));
    const ids = core.apply(command);
    expect(ids).toEqual(["id-1"]);
    expect(JSON.parse(command).op).toBe("replace_objects");
  });
});

describe("schema helpers", () => {
  it("searches every class and ranks exact then prefix then substring", () => {
    const schema = schemaWithEveryKeyType();
    expect(searchClasses(schema, "info_target").map((c) => c.classname)).toEqual(["info_target"]);
    expect(searchClasses(schema, "trigger").map((c) => c.classname)).toEqual(["trigger_multiple"]);
    expect(searchClasses(schema, "CTrigger").map((c) => c.classname)).toEqual(["trigger_multiple"]);
    expect(searchClasses(schema, "").map((c) => c.classname)).toEqual(["info_target", "trigger_multiple"]);
  });

  it("derives flag bits from the observed values", () => {
    const bits = observedFlagBits({
      name: "spawnflags",
      ty: "flags",
      observed_values: ["1", "6", "0"],
      default: "0",
      required: false,
      sources: [],
    });
    expect(bits.map((b) => b.bit)).toEqual([0, 1, 2]);
  });

  it("rejects a schema that is not an object with classes", () => {
    expect(() => parseSchema("[]")).toThrow(/no classes/);
    expect(() => parseSchema("{")).toThrow(/does not parse/);
  });

  it("falls back to a string key type for an unknown one", () => {
    const schema = parseSchema(JSON.stringify({ classes: { a: { keys: { k: { ty: "quaternion" } } } } }));
    expect(schema.classes.a.keys.k.ty).toBe("string");
  });
});
