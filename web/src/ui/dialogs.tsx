import { type JSX, For, Show, createEffect, createMemo, createSignal } from "solid-js";
import type { Id } from "../core/api";
import type { MapMeta, MinimapSettings, Settings } from "../doc/model";
import { DEFAULT_MATERIAL } from "../doc/model";
import type { Editor } from "../state/editor";
import type { BrowseListing } from "../host/client";
import { setInputPrefs, state, select } from "../state/stores";
import { TextField } from "./fields";
import { MaterialField } from "./MaterialPicker";
import { MESH_SHAPES, defaultValues, parseSpec, specErrorOf, type MeshShapeId } from "../render/meshgen";
import { setFile } from "../doc/command";
import { COMMANDS, type Command, type CommandId, type InputPrefs, type Keymap, type PresetId, chordFor, defaultKeymap, keyChord, rebind } from "../input/keymap";
import { saveInputPrefs, saveKeymap } from "../input/prefs";
import { defaultRecipe, generateRequestJson, parseRecipe, recipeJson, storedRecipeJson, validateRecipe, type Recipe } from "../terrain/recipe";
import { RecipeForm } from "./Inspector";

const STEM_PATTERN = /^mp_rr_[a-z0-9_]+$/;

function num(v: string, fallback: number): number {
  const n = Number(v);
  return Number.isFinite(n) ? n : fallback;
}

function Modal(props: { title: string; children: JSX.Element; onClose: () => void; wide?: boolean }): JSX.Element {
  return (
    <div class="modal-backdrop" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="modal" classList={{ wide: !!props.wide }} role="dialog" aria-modal="true" aria-label={props.title}>
        <header>
          <h2>{props.title}</h2>
          <button type="button" onClick={props.onClose} aria-label="Close">
            x
          </button>
        </header>
        <div class="modal-body">{props.children}</div>
      </div>
    </div>
  );
}

/** New, open, save-as and the project list. */
export function ProjectDialog(props: { editor: Editor; mode: "new" | "open" | "save_as"; onClose: () => void }): JSX.Element {
  const [name, setName] = createSignal(state.project || "my_map");
  const [busy, setBusy] = createSignal(false);
  const run = (): void => {
    setBusy(true);
    const action = props.mode === "new" ? props.editor.newProject(name()) : props.mode === "open" ? props.editor.openProject(name()) : props.editor.saveAs(name());
    void action
      .then(() => props.onClose())
      .catch((e: unknown) => props.editor.note(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  };
  return (
    <Modal title={props.mode === "new" ? "New project" : props.mode === "open" ? "Open project" : "Save project as"} onClose={props.onClose}>
      <TextField label="Project name" value={name()} onChange={setName} />
      <Show when={props.mode === "open"}>
        <ul class="project-list" aria-label="Projects">
          <For each={props.editor.projects}>
            {(p) => (
              <li>
                <button type="button" onClick={() => setName(p.name)}>
                  {p.name}
                </button>
                <span class="modified">{new Date(p.modified * 1000).toISOString().slice(0, 16).replace("T", " ")}</span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <div class="row">
        <button type="button" disabled={busy() || !name().trim()} onClick={run}>
          {busy() ? "working" : "go"}
        </button>
      </div>
    </Modal>
  );
}

/** Meta, minimap, bake, navmesh and vis settings. */
export function MapSettingsDialog(props: { editor: Editor; onClose: () => void }): JSX.Element {
  const meta = createMemo<MapMeta>(() => ({ ...(props.editor.document()?.meta ?? emptyMeta()) }));
  const settings = createMemo<Settings>(() => props.editor.document()?.settings ?? emptySettings());
  const [stem, setStem] = createSignal(meta().stem);
  const [playlists, setPlaylists] = createSignal(meta().playlists.join(", "));
  const [loadscreen, setLoadscreen] = createSignal(meta().loadscreen ?? "");
  const [sky, setSky] = createSignal(meta().sky ?? "");
  const [minimap, setMinimap] = createSignal<MinimapSettings>({ ...meta().minimap });
  const [bake, setBake] = createSignal({ ...settings().bake });
  const [nav, setNav] = createSignal({ ...settings().navmesh });
  const [vis, setVis] = createSignal({ ...settings().vis });
  const stemValid = createMemo(() => STEM_PATTERN.test(stem()));
  const save = (): void => {
    props.editor.setMeta({
      ...meta(),
      stem: stem(),
      playlists: playlists().split(",").map((s) => s.trim()).filter((s) => s.length > 0),
      loadscreen: loadscreen() || null,
      sky: sky() || null,
      minimap: minimap(),
    });
    props.editor.setSettings({ bake: bake(), navmesh: nav(), vis: vis() });
    props.onClose();
  };
  return (
    <Modal title="Map settings" onClose={props.onClose} wide>
      <fieldset>
        <legend>Meta</legend>
        <TextField label="Stem (mp_rr_*)" live value={stem()} onChange={setStem} />
        <Show when={!stemValid()}>
          <p class="invalid-note" role="alert">
            the stem must match {STEM_PATTERN.source}
          </p>
        </Show>
        <TextField label="Playlists (comma separated)" value={playlists()} onChange={setPlaylists} />
        <TextField label="Loadscreen" value={loadscreen()} onChange={setLoadscreen} />
        <TextField label="Sky" value={sky()} onChange={setSky} />
      </fieldset>
      <fieldset>
        <legend>Minimap</legend>
        <TextField
          label="Enabled"
          type="checkbox"
          value={minimap().enabled ? 1 : 0}
          onChange={(v) => setMinimap({ ...minimap(), enabled: v === "1" })}
        />
        <TextField
          label="Origin x"
          type="number"
          step="1"
          value={minimap().origin[0]}
          onChange={(v) => setMinimap({ ...minimap(), origin: [num(v, -8192), minimap().origin[1]] })}
        />
        <TextField
          label="Origin y"
          type="number"
          step="1"
          value={minimap().origin[1]}
          onChange={(v) => setMinimap({ ...minimap(), origin: [minimap().origin[0], num(v, -8192)] })}
        />
        <TextField label="Size" type="number" step="256" value={minimap().size} onChange={(v) => setMinimap({ ...minimap(), size: num(v, 16384) })} />
        <TextField
          label="Resolution"
          type="number"
          step="64"
          value={minimap().resolution}
          onChange={(v) => setMinimap({ ...minimap(), resolution: num(v, 2048) })}
        />
      </fieldset>
      <fieldset>
        <legend>Bake</legend>
        <TextField label="Texel size" type="number" step="1" value={bake().texel_size} onChange={(v) => setBake({ ...bake(), texel_size: num(v, 16) })} />
        <TextField label="Bounces" type="number" step="1" value={bake().bounces} onChange={(v) => setBake({ ...bake(), bounces: num(v, 2) })} />
        <TextField label="Samples" type="number" step="8" value={bake().samples} onChange={(v) => setBake({ ...bake(), samples: num(v, 256) })} />
        <TextField
          label="Ambient occlusion"
          type="checkbox"
          value={bake().ambient_occlusion ? 1 : 0}
          onChange={(v) => setBake({ ...bake(), ambient_occlusion: v === "1" })}
        />
        <TextField label="Probe spacing" type="number" step="16" value={bake().probe_spacing} onChange={(v) => setBake({ ...bake(), probe_spacing: num(v, 256) })} />
        <TextField
          label="Cubemap spacing"
          type="number"
          step="256"
          value={bake().cubemap_spacing}
          onChange={(v) => setBake({ ...bake(), cubemap_spacing: num(v, 2048) })}
        />
      </fieldset>
      <fieldset>
        <legend>Navmesh and vis</legend>
        <TextField
          label="Navmesh enabled"
          type="checkbox"
          value={nav().enabled ? 1 : 0}
          onChange={(v) => setNav({ ...nav(), enabled: v === "1" })}
        />
        <TextField label="Hulls" value={nav().hulls.join(", ")} onChange={(v) => setNav({ ...nav(), hulls: v.split(",").map((s) => s.trim()).filter(Boolean) })} />
        <TextField label="Portals" type="checkbox" value={vis().portals ? 1 : 0} onChange={(v) => setVis({ portals: v === "1" })} />
      </fieldset>
      <div class="row">
        <button type="button" disabled={!stemValid()} onClick={save}>
          Apply
        </button>
      </div>
    </Modal>
  );
}

function emptyMeta(): MapMeta {
  return {
    stem: "mp_rr_untitled",
    display_name: "Untitled",
    description: "",
    playlists: [],
    loadscreen: null,
    minimap: { enabled: true, origin: [-8192, -8192], size: 16384, resolution: 2048 },
    sky: null,
    resident_paks: [],
  };
}

function emptySettings(): Settings {
  return {
    bake: { texel_size: 16, bounces: 2, samples: 256, sky_color: [0.5, 0.6, 0.8], sky_intensity: 1, ambient_occlusion: true, probe_spacing: 256, cubemap_spacing: 2048 },
    navmesh: { enabled: true, hulls: ["small", "med_short", "medium", "large", "extra_large"], bounds: null },
    vis: { portals: false },
  };
}

const IMPORTS = [
  { id: "rpak", label: "Apex map rpak (*_perm.rpak)", input: "rpak" },
  { id: "vmf", label: "Valve VMF", input: "text", accept: ".vmf" },
  { id: "quake", label: "Quake MAP", input: "text", accept: ".map" },
  { id: "remap", label: "ReMap JSON", input: "text", accept: ".json" },
  { id: "obj", label: "Wavefront OBJ", input: "text", accept: ".obj" },
  { id: "gltf", label: "glTF binary (.glb)", input: "file" },
  { id: "tf2", label: "TF2 BSP (host job)", input: "path" },
  { id: "mapref", label: "Apex map as reference (host job)", input: "map" },
] as const;

/** Text importers run in the core; file imports go through host jobs. */
export function ImportDialog(props: { editor: Editor; onClose: () => void }): JSX.Element {
  const [kind, setKind] = createSignal<string>(IMPORTS[0].id);
  const [text, setText] = createSignal("");
  const [path, setPath] = createSignal("");
  const [file, setFile] = createSignal<File | null>(null);
  const [stem, setStem] = createSignal("");
  const [maps, setMaps] = createSignal<string[]>([]);
  const [listing, setListing] = createSignal<BrowseListing | null>(null);
  const [pak, setPak] = createSignal<{ path: string; stem: string } | null>(null);
  // A picked or dropped text file; kept out of the textarea so a large VMF does not stall the page.
  const [loaded, setLoaded] = createSignal<{ name: string; text: string } | null>(null);
  const [objScale, setObjScale] = createSignal("1");
  const [objUp, setObjUp] = createSignal<"y" | "z">("y");
  const chosen = createMemo(() => IMPORTS.find((i) => i.id === kind()) ?? IMPORTS[0]);
  const source = (): string => loaded()?.text ?? text();
  const loadFile = async (f: File): Promise<void> => {
    const ext = f.name.toLowerCase().slice(f.name.lastIndexOf("."));
    if (ext === ".glb") {
      setKind("gltf");
      setFile(f);
      return;
    }
    const byExt = IMPORTS.find((i) => "accept" in i && i.accept === ext);
    if (byExt) setKind(byExt.id);
    else if (chosen().input !== "text") {
      props.editor.note(`${f.name}: not a .vmf, .map, .json or .glb file`);
      return;
    }
    setLoaded({ name: f.name, text: await f.text() });
  };
  const onDrop = (e: DragEvent): void => {
    e.preventDefault();
    const f = e.dataTransfer?.files?.[0];
    if (f) void loadFile(f);
  };
  const browse = (dir: string): void => {
    if (!props.editor.host) return;
    void props.editor.host
      .browse(dir)
      .then(setListing)
      .catch((e: unknown) => props.editor.note(e instanceof Error ? e.message : String(e)));
  };
  // Opens on the install's paks the first time the rpak import is picked.
  createEffect(() => {
    if (chosen().input === "rpak" && !listing()) browse("");
  });
  // The install's maps, fetched once the reference import is picked.
  createEffect(() => {
    if (chosen().input !== "map" || !props.editor.host) return;
    void props.editor.host
      .maps()
      .then((list) => {
        setMaps(list.map((m) => m.stem));
        if (!stem() && list.length > 0) setStem(list[0].stem);
      })
      .catch((e: unknown) => props.editor.note(e instanceof Error ? e.message : String(e)));
  });
  const ready = createMemo(() => {
    const c = chosen();
    if (c.input === "text") return source().trim().length > 0;
    if (c.input === "file") return file() !== null;
    if (c.input === "map") return stem().trim().length > 0;
    if (c.input === "rpak") return pak() !== null;
    return path().trim().length > 0;
  });
  const run = async (): Promise<void> => {
    const c = chosen();
    try {
      let added: Id[] = [];
      if (c.id === "vmf") added = props.editor.importVmf(source());
      else if (c.id === "quake") added = props.editor.importQuakeMap(source());
      else if (c.id === "remap") added = props.editor.importReMap(source());
      else if (c.id === "obj") {
        const scale = Number(objScale());
        if (!Number.isFinite(scale) || scale <= 0) throw new Error("scale must be a positive number");
        added = props.editor.importObj(source(), { scale, up: objUp(), material_prefix: `world/obj/${state.stem}` });
      }
      else if (c.id === "gltf") {
        const f = file();
        if (f) added = props.editor.importGltfFile(f.name, new Uint8Array(await f.arrayBuffer()));
      } else if (c.id === "tf2") {
        await props.editor.startJob("import_tf2", { source_bsp: path().trim() });
      } else if (c.id === "mapref") {
        props.onClose();
        await props.editor.importMap({ stem: stem().trim() });
        return;
      } else if (c.id === "rpak") {
        const p = pak();
        if (!p) return;
        props.onClose();
        await props.editor.importMap({ stem: p.stem, pak: p.path });
        return;
      }
      if (added.length > 0) {
        select(added);
        props.editor.note(`imported ${added.length} objects`);
      }
      props.onClose();
    } catch (e) {
      // The dialog stays open so the input can be fixed and tried again.
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <Modal title="Import" onClose={props.onClose}>
      <div onDragOver={(e) => e.preventDefault()} onDrop={onDrop}>
      <div class="field">
        <label class="field-name" for="import-kind">Format</label>
        <select id="import-kind" value={kind()} onChange={(e) => setKind(e.currentTarget.value)}>
          <For each={IMPORTS}>{(i) => <option value={i.id}>{i.label}</option>}</For>
        </select>
      </div>
      <Show when={chosen().input === "text"}>
        <div class="field">
          <label class="field-name" for="import-text-file">File</label>
          <input
            id="import-text-file"
            type="file"
            accept={"accept" in chosen() ? (chosen() as { accept: string }).accept : ""}
            onChange={(e) => {
              const f = e.currentTarget.files?.[0];
              if (f) void loadFile(f);
            }}
          />
        </div>
        <Show
          when={loaded()}
          fallback={
            <div class="field">
              <label class="field-name" for="import-text">Contents</label>
              <textarea id="import-text" rows={12} value={text()} onInput={(e) => setText(e.currentTarget.value)} />
            </div>
          }
        >
          <p class="hint">
            {loaded()!.name} ({Math.ceil(loaded()!.text.length / 1024)} KB){" "}
            <button type="button" onClick={() => setLoaded(null)}>
              Clear
            </button>
          </p>
        </Show>
        <p class="hint">pick a file, drop one on this dialog, or paste the contents</p>
      </Show>
      <Show when={chosen().id === "obj"}>
        <div class="row">
          <TextField label="Scale (world units per OBJ unit)" value={objScale()} onChange={setObjScale} />
          <div class="field">
            <label class="field-name" for="obj-up">Up axis</label>
            <select id="obj-up" value={objUp()} onChange={(e) => setObjUp(e.currentTarget.value === "z" ? "z" : "y")}>
              <option value="y">Y up (Blender, Maya export)</option>
              <option value="z">Z up</option>
            </select>
          </div>
        </div>
        <p class="hint">materials map to world/obj/{state.stem}/&lt;material&gt;; build them with scripts/source/objmat.py</p>
      </Show>
      <Show when={chosen().input === "file"}>
        <div class="field">
          <label class="field-name" for="import-file">File</label>
          <input id="import-file" type="file" accept=".glb,model/gltf-binary" onChange={(e) => setFile(e.currentTarget.files?.[0] ?? null)} />
        </div>
      </Show>
      <Show when={chosen().input === "path"}>
        <TextField label="Source bsp (relative to the host cache)" value={path()} onChange={setPath} />
      </Show>
      <Show when={chosen().input === "rpak"}>
        <div class="field">
          <label class="field-name" for="rpak-dir">Folder</label>
          <div class="row">
            <button type="button" disabled={!listing()?.parent} onClick={() => browse(listing()?.parent ?? "")}>
              Up
            </button>
            <input
              id="rpak-dir"
              value={listing()?.dir ?? ""}
              onChange={(e) => browse(e.currentTarget.value)}
            />
          </div>
        </div>
        <ul class="file-list">
          <For each={listing()?.entries ?? []}>
            {(e) => (
              <li>
                <button
                  type="button"
                  classList={{ selected: pak()?.path === e.path }}
                  onClick={() => (e.kind === "dir" ? browse(e.path) : setPak({ path: e.path, stem: e.stem }))}
                  onDblClick={() => {
                    if (e.kind === "pak") {
                      setPak({ path: e.path, stem: e.stem });
                      void run();
                    }
                  }}
                >
                  {e.kind === "dir" ? `[${e.name}]` : e.name}
                </button>
              </li>
            )}
          </For>
        </ul>
        <p class="hint">
          {pak() ? `imports ${pak()!.stem} (with its _temp pak) into project ${pak()!.stem}` : "pick a map's _perm.rpak; its _temp partner loads with it"}
        </p>
      </Show>
      <Show when={chosen().input === "map"}>
        <div class="field">
          <label class="field-name" for="import-map">Map</label>
          <input id="import-map" list="import-map-list" value={stem()} onInput={(e) => setStem(e.currentTarget.value)} />
          <datalist id="import-map-list">
            <For each={maps()}>{(m) => <option value={m} />}</For>
          </datalist>
        </div>
        <Show when={maps().length === 0}>
          <p class="hint">the install lists no maps</p>
        </Show>
      </Show>
      <div class="row">
        <button type="button" onClick={() => void run()} disabled={!ready()}>
          Import
        </button>
      </div>
      </div>
    </Modal>
  );
}

/** A terrain object built from a generator recipe. */
export function NewTerrainDialog(props: { editor: Editor; onClose: () => void }): JSX.Element {
  const [recipe, setRecipe] = createSignal<Recipe>(defaultRecipe());
  const [name, setName] = createSignal("terrain");
  // The world size the user asks for, converted to samples by the cell size.
  const [size, setSize] = createSignal(16384);
  const resolutionFor = (world: number, cell: number): [number, number] => {
    const n = Math.max(2, Math.min(4096, Math.round(world / Math.max(0.01, cell))));
    return [n, n];
  };
  const setWorldSize = (world: number): void => {
    setSize(world);
    setRecipe({ ...recipe(), resolution: resolutionFor(world, recipe().cell_size) });
  };
  const issues = createMemo(() => validateRecipe(recipe()));
  const create = (): void => {
    const r = recipe();
    let heights: Float32Array;
    try {
      heights = props.editor.terrainGenerate(generateRequestJson(r));
    } catch (e) {
      props.editor.note(`terrain: ${e instanceof Error ? e.message : String(e)}`);
      return;
    }
    const id = cryptoId();
    const path = `terrain/${id}.f32`;
    props.editor.apply(setFile(path, new Uint8Array(heights.buffer, heights.byteOffset, heights.byteLength)));
    props.editor.addObjects([
      {
        id,
        name: name(),
        layer: props.editor.store?.layers[0]?.id ?? "",
        parent: null,
        transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
        hidden: false,
        locked: false,
        kind: {
          type: "terrain",
          terrain: {
            heights: path,
            resolution: r.resolution,
            cell_size: r.cell_size,
            // No weights file yet: auto paint or a paint stroke writes one.
            layers: [{ name: "base", material: "terrain/default", weights: "", uv_scale: 0.02, scatter: [] }],
            chunk_size: 64,
            collision: true,
            contents: 1,
            lightmap_scale: 16,
            recipe: JSON.parse(storedRecipeJson(r)),
          },
        },
      },
    ]);
    props.onClose();
  };
  return (
    <Modal title="New terrain" onClose={props.onClose} wide>
      <TextField label="Name" value={name()} onChange={setName} />
      <TextField
        label="World size"
        type="number"
        step="1024"
        value={size()}
        onChange={(v) => setWorldSize(Math.max(64, num(v, 16384)))}
      />
      <RecipeForm recipe={recipe()} onChange={setRecipe} onGenerate={setRecipe} />
      <div class="row">
        <button type="button" onClick={create} disabled={issues().length > 0}>
          Create
        </button>
        <button type="button" onClick={() => setRecipe(parseRecipe(recipeJson(recipe())))}>
          Reset from JSON
        </button>
        <span class="hint">
          {recipe().resolution[0]} x {recipe().resolution[1]} samples at {recipe().cell_size} inches
        </span>
      </div>
    </Modal>
  );
}

function cryptoId(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (typeof g.crypto?.randomUUID === "function") return g.crypto.randomUUID();
  return `t${Math.floor(Math.random() * 1e9).toString(16)}`;
}

const COLLISION_MODES = ["triangles", "convex_hulls", "none"] as const;

/**
 * Picks a shape and its dimensions, then hands the spec to the core, which
 * writes the glb and the object that references it.
 */
export function NewMeshDialog(props: { editor: Editor; onClose: () => void }): JSX.Element {
  const [shape, setShape] = createSignal<MeshShapeId>("box");
  const [values, setValues] = createSignal<Record<string, string>>(defaultValues("box"));
  const [material, setMaterial] = createSignal(DEFAULT_MATERIAL);
  const [name, setName] = createSignal("mesh");
  const [collision, setCollision] = createSignal<(typeof COLLISION_MODES)[number]>("triangles");
  const entry = createMemo(() => MESH_SHAPES.find((s) => s.id === shape()) ?? MESH_SHAPES[0]);
  const parsed = createMemo(() => parseSpec(shape(), values()));
  const specError = createMemo(() => specErrorOf(parsed()));
  const pick = (id: MeshShapeId): void => {
    setShape(id);
    setValues(defaultValues(id));
  };
  const setValue = (key: string, text: string): void => {
    setValues({ ...values(), [key]: text });
  };
  const create = (): void => {
    const spec = parsed();
    if (!spec.ok) {
      props.editor.note(spec.error);
      return;
    }
    const at = state.cursorWorld ?? [0, 0, 0];
    let added: Id[] = [];
    try {
      added = props.editor.meshCreate(spec.spec, {
        position: at,
        material: material(),
        name: name(),
        collision: collision(),
      });
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
      return;
    }
    if (added.length === 0) {
      props.editor.note("the core added no mesh");
      return;
    }
    select(added);
    props.onClose();
  };
  return (
    <Modal title="New mesh" onClose={props.onClose} wide>
      <div class="field">
        <label class="field-name" for="mesh-shape">
          Shape
        </label>
        <select id="mesh-shape" value={shape()} onChange={(e) => pick(e.currentTarget.value as MeshShapeId)}>
          <For each={MESH_SHAPES}>{(s) => <option value={s.id}>{s.label}</option>}</For>
        </select>
      </div>
      <For each={entry().fields}>
        {(field) => (
          <Show
            when={field.kind === "boolean"}
            fallback={
              <TextField
                label={field.label}
                live
                type={field.kind === "integer" || field.kind === "number" ? "number" : "text"}
                step={String(field.step)}
                min={field.min}
                max={field.max}
                value={values()[field.name] ?? ""}
                onChange={(v) => setValue(field.name, v)}
              />
            }
          >
            <TextField
              label={field.label}
              type="checkbox"
              value={values()[field.name] === "1" ? 1 : 0}
              onChange={(v) => setValue(field.name, v === "1" ? "1" : "0")}
            />
          </Show>
        )}
      </For>
      <MaterialField
        label="Mesh material"
        value={material()}
        project={props.editor.projectMaterials()}
        onChange={setMaterial}
      />
      <div class="field">
        <label class="field-name" for="mesh-collision-mode">
          Collision
        </label>
        <select
          id="mesh-collision-mode"
          value={collision()}
          onChange={(e) => setCollision(e.currentTarget.value as (typeof COLLISION_MODES)[number])}
        >
          <For each={COLLISION_MODES}>{(m) => <option value={m}>{m}</option>}</For>
        </select>
      </div>
      <TextField label="Name" value={name()} onChange={setName} />
      <Show when={specError().length > 0}>
        <p class="invalid-note" role="alert">
          {specError()}
        </p>
      </Show>
      <div class="row">
        <button type="button" disabled={!parsed().ok} onClick={create}>
          Create
        </button>
        <span class="hint">
          {entry().label} at {state.cursorWorld ? state.cursorWorld.map((c) => Math.round(c)).join(", ") : "0, 0, 0"}
        </span>
      </div>
    </Modal>
  );
}

/** Keymap preset, input preferences and the rebindable command table. */
export function ShortcutsDialog(props: { editor: Editor; onClose: () => void }): JSX.Element {
  const [keymap, setKeymap] = createSignal<Keymap>(props.editor.keymap);
  const [capturing, setCapturing] = createSignal<CommandId | null>(null);
  const [message, setMessage] = createSignal("");
  const [filter, setFilter] = createSignal("");
  const commit = (next: Keymap): void => {
    setKeymap(next);
    props.editor.keymap = next;
    saveKeymap(next);
  };
  const groups = createMemo(() => {
    const q = filter().toLowerCase();
    const map = new Map<string, Command[]>();
    for (const c of COMMANDS) {
      const chord = chordFor(keymap(), c.id);
      if (q && !c.label.toLowerCase().includes(q) && !chord.toLowerCase().includes(q)) continue;
      const list = map.get(c.group);
      if (list) list.push(c);
      else map.set(c.group, [c]);
    }
    return [...map.entries()];
  });
  const capture = (id: CommandId, e: KeyboardEvent): void => {
    // Only a binding that was clicked listens; Tab and Enter on a focused one
    // keep their usual meaning.
    if (capturing() !== id) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") {
      setCapturing(null);
      setMessage("");
      return;
    }
    const chord = keyChord(e);
    if (!chord) return;
    const next = rebind(keymap(), id, chord);
    if (typeof next === "string") {
      setMessage(next);
      return;
    }
    commit(next);
    setMessage("");
    setCapturing(null);
  };
  const setPrefs = (patch: Partial<InputPrefs>): void => {
    setInputPrefs(patch);
    saveInputPrefs({ ...state.input, ...patch });
  };
  return (
    <Modal title="Keymap and input" onClose={props.onClose} wide>
      <fieldset>
        <legend>Preset</legend>
        <label class="inline">
          <span>Keymap</span>
          <select
            value={keymap().preset}
            aria-label="Keymap preset"
            onChange={(e) => {
              commit(defaultKeymap(e.currentTarget.value as PresetId));
              setMessage("");
            }}
          >
            <option value="blender">Blender</option>
            <option value="legacy">r5ed legacy</option>
          </select>
        </label>
        <button type="button" onClick={() => commit(defaultKeymap(keymap().preset))}>
          Restore defaults
        </button>
      </fieldset>
      <fieldset>
        <legend>Input</legend>
        <label class="inline">
          <input type="checkbox" checked={state.input.emulate3Button} onChange={(e) => setPrefs({ emulate3Button: e.currentTarget.checked })} />
          <span>Emulate 3 button mouse (Alt + left drag)</span>
        </label>
        <label class="inline">
          <input type="checkbox" checked={state.input.emulateNumpad} onChange={(e) => setPrefs({ emulateNumpad: e.currentTarget.checked })} />
          <span>Emulate numpad (top row digits)</span>
        </label>
        <label class="inline">
          <input type="checkbox" checked={state.input.zoomToMouse} onChange={(e) => setPrefs({ zoomToMouse: e.currentTarget.checked })} />
          <span>Zoom to mouse position</span>
        </label>
        <label class="inline">
          <input type="checkbox" checked={state.input.invertZoom} onChange={(e) => setPrefs({ invertZoom: e.currentTarget.checked })} />
          <span>Invert zoom direction</span>
        </label>
        <label class="inline">
          <span>Orbit sensitivity</span>
          <input
            type="range"
            min="0.002"
            max="0.02"
            step="0.001"
            value={state.input.orbitSpeed}
            onInput={(e) => setPrefs({ orbitSpeed: Number(e.currentTarget.value) })}
          />
        </label>
      </fieldset>
      <input type="search" placeholder="Filter commands or keys" value={filter()} onInput={(e) => setFilter(e.currentTarget.value)} />
      <p class="hint">click a binding, then press the new key combination</p>
      <p class="invalid-note" role="status">
        {message()}
      </p>
      <For each={groups()}>
        {([group, items]) => (
          <fieldset>
            <legend>{group}</legend>
            <table class="shortcut-table">
              <tbody>
                <For each={items}>
                  {(c) => (
                    <tr>
                      <td>{c.label}</td>
                      <td>
                        <button
                          type="button"
                          classList={{ capturing: capturing() === c.id }}
                          onClick={() => setCapturing(c.id)}
                          onKeyDown={(e) => capture(c.id, e)}
                        >
                          {capturing() === c.id ? "press a key" : chordFor(keymap(), c.id) || "-"}
                        </button>
                      </td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </fieldset>
        )}
      </For>
      <fieldset>
        <legend>Mouse</legend>
        <table class="shortcut-table">
          <tbody>
            <For each={MOUSE_CONTROLS}>
              {([what, how]) => (
                <tr>
                  <td>{what}</td>
                  <td>{how}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </fieldset>
    </Modal>
  );
}

/** The viewport's mouse controls, which follow Blender and are not rebindable. */
const MOUSE_CONTROLS: readonly [string, string][] = [
  ["Select (drag for box select)", "LMB"],
  ["Extend / toggle selection", "Shift+LMB"],
  ["Deselect (drag for box deselect)", "Ctrl+LMB"],
  ["Orbit", "MMB drag"],
  ["Pan", "Shift+MMB drag"],
  ["Zoom", "Wheel / Ctrl+MMB drag"],
  ["Center view on point", "Alt+MMB click"],
  ["Pan across / up and down", "Ctrl+Wheel / Shift+Wheel"],
  ["Step through walls along the cursor", "Alt+Wheel"],
  ["Place 3D cursor", "Shift+RMB"],
];

export function AboutDialog(props: { editor: Editor; onClose: () => void }): JSX.Element {
  return (
    <Modal title="About r5ed" onClose={props.onClose}>
      <p>
        host {props.editor.session?.version ?? "unavailable"} | game root {props.editor.session?.gameRoot ?? "unavailable"}
      </p>
      <p>{props.editor.schema.classes ? Object.keys(props.editor.schema.classes).length : 0} entity classes in the schema</p>
    </Modal>
  );
}
