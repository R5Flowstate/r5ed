import { type JSX, For, Show, createMemo, createSignal } from "solid-js";
import type { DocObject, Light, Modifier, PathObject, RefRecord, Terrain, VoxelTerrain, Volume } from "../doc/model";
import { MODIFIER_KINDS, type ModField, modifierKind, parseVec, stackRequest, withField } from "../state/modifiers";
import type { Id } from "../core/api";
import { sourceAnglesFromQuat, quatFromSourceAngles } from "../math/quat";
import { planeFromThree } from "../math/ray";
import { select, setSculpt, setState, state } from "../state/stores";
import type { Editor } from "../state/editor";
import { setFile } from "../doc/command";
import { EntityForm } from "./EntityForm";
import { decodePng } from "../render/png";
import { TextField } from "./fields";
import { RedoPanel } from "./RedoPanel";
import { MaterialField } from "./MaterialPicker";
import {
  BLEND_MODES,
  FILTER_OP_KINDS,
  GENERATOR_KINDS,
  MASK_FIELDS,
  MASK_KINDS,
  NODE_KINDS,
  SDF_OP_FIELDS,
  SDF_OP_KINDS,
  SDF_SHAPE_FIELDS,
  SDF_SHAPE_KINDS,
  type BlendMode,
  type BlendTerm,
  type FieldSpec,
  type FieldValue,
  type MaskKind,
  type MaskNode,
  type NodeKind,
  type Recipe,
  type RecipeNode,
  type SdfOpKind,
  type SdfOpNode,
  type SdfShapeKind,
  type SdfShapeNode,
  defaultMask,
  defaultNode,
  defaultRecipe,
  defaultSdfOp,
  defaultSdfShape,
  getField,
  heightmapFromImage,
  getMaskField,
  getSdfOpField,
  getSdfShapeField,
  sdfOpsFromWire,
  sdfOpsToWire,
  nodeFields,
  parseRecipe,
  generateRequestJson,
  setField,
  setMaskField,
  setSdfOpField,
  setSdfShapeField,
  storedRecipeJson,
  validateRecipe,
} from "../terrain/recipe";
import {
  FALLOFFS,
  SCULPT_TOOLS,
  STAMP_BLEND_MODES,
  type Falloff,
  type SculptTool,
  type Stroke,
  defaultStroke,
  falloffName,
  falloffValue,
  strokeJson,
} from "../terrain/sculpt";
import {
  LAYER_DRIVERS,
  RULE_SPECS,
  type LayerRule,
  type RuleSpec,
  defaultLayerRule,
  getRuleField,
  rulesJson,
  setRuleField,
} from "../terrain/splat";

export interface InspectorProps {
  editor: Editor;
  object: DocObject | null;
  issues: { id: string | null; message: string }[];
  suggestions: Record<string, string[]>;
  /** Every object an OnX output can point at. */
  targets: { id: string; name: string }[];
}

function num(v: string, fallback = 0): number {
  const n = Number(v);
  return Number.isFinite(n) ? n : fallback;
}

/** Reads three numbers out of a text field, so a plane can be typed in. */
function parseTriple(text: string): [number, number, number] {
  const parts = text.trim().split(/[\s,]+/).map(Number);
  return [num(String(parts[0] ?? 0)), num(String(parts[1] ?? 0)), num(String(parts[2] ?? 0))];
}

function TripleEditor(props: {
  label: string;
  value: [number, number, number];
  step: string;
  onChange: (value: [number, number, number]) => void;
}): JSX.Element {
  const labels = ["x", "y", "z"];
  return (
    <fieldset class="triple-editor">
      <legend>{props.label}</legend>
      <For each={[0, 1, 2] as const}>
        {(i) => (
          <TextField
            label={labels[i]}
            type="number"
            step={props.step}
            value={props.value[i]}
            onChange={(v) => {
              const next: [number, number, number] = [...props.value] as [number, number, number];
              next[i] = num(v, props.value[i]);
              props.onChange(next);
            }}
          />
        )}
      </For>
    </fieldset>
  );
}

function PairEditor(props: {
  label: string;
  value: [number, number];
  step: string;
  onChange: (value: [number, number]) => void;
}): JSX.Element {
  return (
    <fieldset class="triple-editor">
      <legend>{props.label}</legend>
      <For each={[0, 1] as const}>
        {(i) => (
          <TextField
            label={i === 0 ? "u" : "v"}
            type="number"
            step={props.step}
            value={props.value[i]}
            onChange={(v) => {
              const next: [number, number] = [...props.value] as [number, number];
              next[i] = num(v, props.value[i]);
              props.onChange(next);
            }}
          />
        )}
      </For>
    </fieldset>
  );
}

/** Position, Source-style angles and scale. */
function TransformEditor(props: { object: DocObject; onChange: (patch: Partial<DocObject["transform"]>) => void }): JSX.Element {
  const angles = createMemo(() => sourceAnglesFromQuat(props.object.transform.rotation));
  return (
    <div class="transform-editor">
      <TripleEditor
        label="Position"
        value={props.object.transform.position}
        step="1"
        onChange={(v) => props.onChange({ position: v })}
      />
      <TripleEditor
        label="Angles (pitch yaw roll)"
        value={angles()}
        step="1"
        onChange={(v) => props.onChange({ rotation: quatFromSourceAngles(v) })}
      />
      <TripleEditor
        label="Scale"
        value={props.object.transform.scale}
        step="0.05"
        onChange={(v) => props.onChange({ scale: v })}
      />
    </div>
  );
}

/** Free key/value rows for the object kinds that carry them. */
export function KeyValuesEditor(props: {
  keyvalues: Record<string, string>;
  onChange: (next: Record<string, string>) => void;
}): JSX.Element {
  const [name, setName] = createSignal("");
  const names = createMemo(() => Object.keys(props.keyvalues).sort());
  return (
    <div class="keyvalues">
      <h4>Keys</h4>
      <For each={names()}>
        {(key) => (
          <TextField
            label={key}
            value={props.keyvalues[key]}
            onChange={(v) => props.onChange({ ...props.keyvalues, [key]: v })}
          />
        )}
      </For>
      <div class="add-key">
        <TextField label="New key" value={name()} onChange={setName} />
        <button
          type="button"
          disabled={!name().trim()}
          onClick={() => {
            if (!name().trim()) return;
            props.onChange({ ...props.keyvalues, [name().trim()]: "" });
            setName("");
          }}
        >
          Add
        </button>
      </div>
    </div>
  );
}

/** The brush operations the core exposes, reachable from the selection. */
function BrushOps(props: { object: DocObject; editor: Editor }): JSX.Element {
  const [faceIndex, setFaceIndex] = createSignal(0);
  const [distance, setDistance] = createSignal(32);
  const [thickness, setThickness] = createSignal(8);
  const [vertex, setVertex] = createSignal("0 0 0");
  const [to, setTo] = createSignal("0 0 0");
  const [plane, setPlane] = createSignal("0 0 0");
  const [plane2, setPlane2] = createSignal("1 0 0");
  const [plane3, setPlane3] = createSignal("0 1 0");
  const [keep, setKeep] = createSignal<"front" | "back" | "both">("front");
  const [carver, setCarver] = createSignal("");
  const run = (fn: () => Id[]): void => {
    try {
      fn();
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };
  const selection = (): Id[] => state.selection.ids.length > 0 ? [...state.selection.ids] : [props.object.id];
  const faceCount = (): number => (props.object.kind.type === "brush" ? props.object.kind.brush.faces.length : 0);
  const size = (): [number, number, number] => {
    void state.docRevision;
    const b = props.editor.brushBounds(props.object.id);
    return b ? [b[3] - b[0], b[4] - b[1], b[5] - b[2]] : [0, 0, 0];
  };
  const setAxis = (axis: 0 | 1 | 2, value: string): void => {
    const next = [...size()] as [number, number, number];
    const v = num(value, next[axis]);
    if (!(v > 0)) return;
    next[axis] = v;
    run(() => props.editor.brushResize(props.object.id, next));
  };
  return (
    <div class="brush-ops">
      <h4>Size</h4>
      <div class="row">
        <TextField label="W (X)" type="number" step="16" value={size()[0]} onChange={(v) => setAxis(0, v)} />
        <TextField label="D (Y)" type="number" step="16" value={size()[1]} onChange={(v) => setAxis(1, v)} />
        <TextField label="H (Z)" type="number" step="16" value={size()[2]} onChange={(v) => setAxis(2, v)} />
      </div>
      <h4>Brush operations</h4>
      <div class="row">
        <TextField label="Face" type="number" step="1" value={faceIndex()} onChange={(v) => setFaceIndex(Math.max(0, num(v)))} />
        <TextField label="Distance" type="number" step="1" value={distance()} onChange={(v) => setDistance(num(v, 32))} />
        <button
          type="button"
          disabled={faceCount() === 0}
          onClick={() => run(() => props.editor.brushExtrudeFace(props.object.id, faceIndex(), distance()))}
        >
          Extrude face
        </button>
      </div>
      <div class="row">
        <TextField label="Hollow" type="number" step="1" value={thickness()} onChange={(v) => setThickness(Math.max(0.5, num(v, 8)))} />
        <button type="button" onClick={() => run(() => props.editor.brushHollow(props.object.id, thickness()))}>
          Hollow
        </button>
        <button type="button" onClick={() => run(() => props.editor.brushMerge(selection()))}>
          Merge convex
        </button>
      </div>
      <div class="row">
        <TextField label="Vertex" value={vertex()} onChange={setVertex} />
        <TextField label="Move to" value={to()} onChange={setTo} />
        <button type="button" onClick={() => run(() => props.editor.brushMoveVertex(props.object.id, parseTriple(vertex()), parseTriple(to())))}>
          Move vertex
        </button>
      </div>
      <div class="row">
        <TextField label="Clip point A" value={plane()} onChange={setPlane} />
        <TextField label="Clip point B" value={plane2()} onChange={setPlane2} />
        <TextField label="Clip point C" value={plane3()} onChange={setPlane3} />
        <select value={keep()} aria-label="Clip side" onChange={(e) => setKeep(e.currentTarget.value as "front" | "back" | "both")}>
          <option value="front">front</option>
          <option value="back">back</option>
          <option value="both">both</option>
        </select>
        <button
          type="button"
          onClick={() =>
            run(() => {
              const [normal, dist] = planeFromThree(parseTriple(plane()), parseTriple(plane2()), parseTriple(plane3()));
              return props.editor.brushClip(selection(), normal, dist, keep());
            })
          }
        >
          Clip
        </button>
      </div>
      <div class="row">
        <TextField label="Carver id" value={carver()} onChange={setCarver} />
        <button
          type="button"
          disabled={!carver().trim()}
          onClick={() => run(() => props.editor.brushCarve(carver().trim(), selection().filter((id) => id !== carver().trim())))}
        >
          Carve
        </button>
      </div>
      <div class="row">
        <button
          type="button"
          onClick={() => {
            try {
              const touched = props.editor.brushToMesh(props.object.id);
              if (touched.length > 0) select(touched);
              props.editor.note("converted to an editable mesh");
            } catch (e) {
              props.editor.note(e instanceof Error ? e.message : String(e));
            }
          }}
        >
          Convert to Mesh
        </button>
      </div>
    </div>
  );
}

function BrushEditor(props: { object: DocObject; editor: Editor; materials: readonly string[] }): JSX.Element {
  const brush = () => (props.object.kind.type === "brush" ? props.object.kind.brush : null);
  const [thickness, setThickness] = createSignal(8);
  const setFaceMaterial = (index: number, material: string): void => {
    const b = brush();
    if (!b) return;
    const faces = b.faces.map((f, k) => (k === index ? { ...f, material } : f));
    props.editor.apply({ op: "replace_objects", objects: [{ ...props.object, kind: { type: "brush", brush: { ...b, faces } } }] });
  };
  return (
    <div class="kind-editor">
      <For each={brush()?.faces ?? []}>
        {(face, i) => (
          <div class="face-row" data-face={i()}>
            <span class="face-index">{i()}</span>
            <span class="face-normal">
              {face.normal.map((n) => n.toFixed(2)).join(" ")}
            </span>
            <span class="face-dist">{face.dist.toFixed(1)}</span>
            <MaterialField
              label={`Face ${i()} material`}
              value={face.material}
              project={props.materials}
              onChange={(v) => setFaceMaterial(i(), v)}
            />
            <PairEditor
              label="UV offset"
              value={face.uv.offset}
              step="0.05"
              onChange={(v) => {
                const b = brush();
                if (!b) return;
                const faces = b.faces.map((f, k) => (k === i() ? { ...f, uv: { ...f.uv, offset: v } } : f));
                props.editor.apply({ op: "replace_objects", objects: [{ ...props.object, kind: { type: "brush", brush: { ...b, faces } } }] });
              }}
            />
            <PairEditor
              label="UV scale"
              value={face.uv.scale}
              step="0.01"
              onChange={(v) => {
                const b = brush();
                if (!b) return;
                const faces = b.faces.map((f, k) => (k === i() ? { ...f, uv: { ...f.uv, scale: v } } : f));
                props.editor.apply({ op: "replace_objects", objects: [{ ...props.object, kind: { type: "brush", brush: { ...b, faces } } }] });
              }}
            />
          </div>
        )}
      </For>
      <div class="row">
        <TextField label="Hollow thickness" type="number" step="1" value={thickness()} onChange={(v) => setThickness(num(v, 8))} />
        <button type="button" onClick={() => props.editor.brushHollow(props.object.id, thickness())}>
          Hollow
        </button>
      </div>
      <BrushOps object={props.object} editor={props.editor} />
    </div>
  );
}

function PropEditor(props: { object: DocObject; editor: Editor; suggestions: Record<string, string[]> }): JSX.Element {
  const prop = () => (props.object.kind.type === "prop" ? props.object.kind.prop : null);
  const patch = (next: Partial<NonNullable<ReturnType<typeof prop>>>): void => {
    const p = prop();
    if (!p) return;
    props.editor.apply({
      op: "replace_objects",
      objects: [{ ...props.object, kind: { type: "prop", prop: { ...p, ...next } } }],
    });
  };
  return (
    <div class="kind-editor">
      <div class="field">
        <label class="field-name" for="prop-model">Model</label>
        <input
          id="prop-model"
          list="prop-model-list"
          value={prop()?.model ?? ""}
          onChange={(e) => patch({ model: e.currentTarget.value })}
        />
        <datalist id="prop-model-list">
          <For each={props.suggestions.model ?? []}>{(v) => <option value={v} />}</For>
        </datalist>
      </div>
      <TextField label="Skin" type="number" step="1" value={prop()?.skin ?? 0} onChange={(v) => patch({ skin: num(v) })} />
      <TextField label="Static prop" type="checkbox" value={prop()?.static_prop ? 1 : 0} onChange={(v) => patch({ static_prop: v === "1" })} />
      <TextField label="Solid" type="checkbox" value={prop()?.solid ? 1 : 0} onChange={(v) => patch({ solid: v === "1" })} />
      <TextField label="Cast shadows" type="checkbox" value={prop()?.cast_shadows ? 1 : 0} onChange={(v) => patch({ cast_shadows: v === "1" })} />
      <TextField label="Fade distance" type="number" step="16" value={prop()?.fade_distance ?? 0} onChange={(v) => patch({ fade_distance: num(v) })} />
      <KeyValuesEditor keyvalues={prop()?.keyvalues ?? {}} onChange={(keyvalues) => patch({ keyvalues })} />
    </div>
  );
}

function LightEditor(props: { object: DocObject; editor: Editor }): JSX.Element {
  const light = (): Light | null => (props.object.kind.type === "light" ? props.object.kind.light : null);
  const patch = (next: Partial<Light>): void => {
    const l = light();
    if (!l) return;
    props.editor.apply({ op: "replace_objects", objects: [{ ...props.object, kind: { type: "light", light: { ...l, ...next } } }] });
  };
  return (
    <div class="kind-editor">
      <div class="field">
        <label class="field-name" for="light-kind">Kind</label>
        <select id="light-kind" value={light()?.kind ?? "point"} onChange={(e) => patch({ kind: e.currentTarget.value as Light["kind"] })}>
          <For each={["sun", "point", "spot", "area", "sky"] as const}>{(k) => <option value={k}>{k}</option>}</For>
        </select>
      </div>
      <TripleEditor label="Colour" value={light()?.color ?? [1, 1, 1]} step="0.05" onChange={(v) => patch({ color: v })} />
      <TextField label="Intensity" type="number" step="0.1" value={light()?.intensity ?? 1} onChange={(v) => patch({ intensity: num(v, 1) })} />
      <TextField label="Range" type="number" step="16" value={light()?.range ?? 256} onChange={(v) => patch({ range: num(v, 256) })} />
      <TextField label="Inner angle" type="number" step="1" value={light()?.inner_angle ?? 30} onChange={(v) => patch({ inner_angle: num(v) })} />
      <TextField label="Outer angle" type="number" step="1" value={light()?.outer_angle ?? 45} onChange={(v) => patch({ outer_angle: num(v) })} />
      <TextField label="Baked" type="checkbox" value={light()?.baked ? 1 : 0} onChange={(v) => patch({ baked: v === "1" })} />
      <TextField label="Realtime" type="checkbox" value={light()?.realtime ? 1 : 0} onChange={(v) => patch({ realtime: v === "1" })} />
      <KeyValuesEditor keyvalues={light()?.keyvalues ?? {}} onChange={(keyvalues) => patch({ keyvalues })} />
    </div>
  );
}

function VolumeEditor(props: { object: DocObject; editor: Editor }): JSX.Element {
  const volume = (): Volume | null => (props.object.kind.type === "volume" ? props.object.kind.volume : null);
  const patch = (next: Partial<Volume>): void => {
    const v = volume();
    if (!v) return;
    props.editor.apply({ op: "replace_objects", objects: [{ ...props.object, kind: { type: "volume", volume: { ...v, ...next } } }] });
  };
  return (
    <div class="kind-editor">
      <div class="field">
        <label class="field-name" for="volume-kind">Kind</label>
        <select
          id="volume-kind"
          value={volume()?.kind ?? "trigger"}
          onChange={(e) => patch({ kind: e.currentTarget.value as Volume["kind"] })}
        >
          <For each={["trigger", "trigger_hurt", "trigger_soundscape", "envmap_volume", "light_environment_volume", "fog_volume", "player_clip", "out_of_bounds", "custom"] as const}>
            {(k) => <option value={k}>{k}</option>}
          </For>
        </select>
      </div>
      <TextField label="Classname" value={volume()?.classname ?? ""} onChange={(v) => patch({ classname: v })} />
      <p class="hint">{volume()?.brushes.length ?? 0} brushes</p>
      <KeyValuesEditor keyvalues={volume()?.keyvalues ?? {}} onChange={(keyvalues) => patch({ keyvalues })} />
    </div>
  );
}

function PathEditor(props: { object: DocObject; editor: Editor }): JSX.Element {
  const path = (): PathObject | null => (props.object.kind.type === "path" ? props.object.kind.path : null);
  const [index, setIndex] = createSignal(0);
  const points = createMemo(() => path()?.points ?? []);
  const patch = (next: PathObject): void => {
    props.editor.apply({ op: "replace_objects", objects: [{ ...props.object, kind: { type: "path", path: next } }] });
  };
  return (
    <div class="kind-editor">
      <div class="field">
        <label class="field-name" for="path-kind">Kind</label>
        <select
          id="path-kind"
          value={path()?.kind ?? "zipline"}
          onChange={(e) => {
            const p = path();
            if (p) patch({ ...p, kind: e.currentTarget.value as PathObject["kind"] });
          }}
        >
          <For each={["zipline", "ziprail", "spline"] as const}>{(k) => <option value={k}>{k}</option>}</For>
        </select>
      </div>
      <ol class="path-points">
        <For each={points()}>
          {(point, i) => (
            <li data-point={i()}>
              <button type="button" onClick={() => setIndex(i())}>
                {i()}
              </button>
              <TripleEditor
                label={`Point ${i()}`}
                value={point}
                step="1"
                onChange={(v) => {
                  const p = path();
                  if (!p) return;
                  const next = p.points.map((q, k) => (k === i() ? v : q));
                  patch({ ...p, points: next });
                }}
              />
              <button
                type="button"
                aria-label={`Delete point ${i()}`}
                onClick={() => {
                  const p = path();
                  if (!p || p.points.length <= 2) return;
                  patch({ ...p, points: p.points.filter((_, k) => k !== i()) });
                }}
              >
                x
              </button>
            </li>
          )}
        </For>
      </ol>
      <div class="row">
        <button
          type="button"
          onClick={() => {
            const p = path();
            if (!p) return;
            const last = p.points[p.points.length - 1] ?? [0, 0, 0];
            patch({ ...p, points: [...p.points, [last[0] + 128, last[1], last[2] + 128]] });
          }}
        >
          Add point
        </button>
        <span class="hint">selected {index()}</span>
      </div>
    </div>
  );
}

/** One widget for one field spec, chosen by the spec's type. */
function FieldWidget(props: {
  spec: FieldSpec;
  value: FieldValue;
  onChange: (value: FieldValue) => void;
}): JSX.Element {
  const id = () => `f-${props.spec.path.replace(/\W+/g, "-")}`;
  if (props.spec.type === "bool") {
    return (
      <TextField
        label={props.spec.label}
        type="checkbox"
        value={props.value === true ? 1 : 0}
        onChange={(v) => props.onChange(v === "1")}
      />
    );
  }
  if (props.spec.type === "enum") {
    return (
      <div class="field">
        <label class="field-name" for={id()}>{props.spec.label}</label>
        <select id={id()} value={String(props.value)} onChange={(e) => props.onChange(e.currentTarget.value)}>
          <For each={props.spec.choices ?? []}>{(c) => <option value={c}>{c}</option>}</For>
        </select>
      </div>
    );
  }
  if (props.spec.type === "vec2" || props.spec.type === "vec3") {
    const want = props.spec.type === "vec2" ? 2 : 3;
    const labels = props.spec.type === "vec2" ? ["u", "v"] : ["x", "y", "z"];
    const current = (): number[] => (Array.isArray(props.value) ? props.value : []);
    return (
      <fieldset class="triple-editor">
        <legend>{props.spec.label}</legend>
        <For each={Array.from({ length: want }, (_, i) => i)}>
          {(i) => (
            <TextField
              label={labels[i]}
              type="number"
              step="0.01"
              value={current()[i] ?? 0}
              onChange={(x) => {
                const next = Array.from({ length: want }, (_, k) => current()[k] ?? 0);
                next[i] = num(x, next[i]);
                props.onChange(next);
              }}
            />
          )}
        </For>
      </fieldset>
    );
  }
  if (props.spec.type === "string") {
    return <TextField label={props.spec.label} value={String(props.value)} onChange={(v) => props.onChange(v)} />;
  }
  return (
    <TextField
      label={props.spec.label}
      type="number"
      step={String(props.spec.step ?? 1)}
      min={props.spec.min}
      max={props.spec.max}
      value={Number(props.value) || 0}
      onChange={(v) => props.onChange(num(v))}
    />
  );
}

/** The mask editor a blend or modulate node carries. */
function MaskEditor(props: { mask: MaskNode; onChange: (mask: MaskNode) => void }): JSX.Element {
  const setKind = (kind: MaskKind): void => props.onChange(defaultMask(kind));
  return (
    <div class="mask-editor">
      <div class="field">
        <label class="field-name" for="mask-kind">Mask</label>
        <select id="mask-kind" value={props.mask.kind} onChange={(e) => setKind(e.currentTarget.value as MaskKind)}>
          <For each={MASK_KINDS}>{(k) => <option value={k}>{k}</option>}</For>
        </select>
      </div>
      <For each={MASK_FIELDS[props.mask.kind] ?? []}>
        {(spec) => (
          <FieldWidget
            spec={spec}
            value={getMaskField(props.mask, spec)}
            onChange={(value) => props.onChange(setMaskField(props.mask, spec, value))}
          />
        )}
      </For>
    </div>
  );
}

/** A generator picker plus one form per node, driven by the mirror. */
export function RecipeForm(props: {
  recipe: Recipe;
  onChange: (recipe: Recipe) => void;
  onGenerate: (recipe: Recipe) => void;
}): JSX.Element {
  const [selected, setSelected] = createSignal(0);
  const [imageError, setImageError] = createSignal("");
  const issues = createMemo(() => validateRecipe(props.recipe));
  const node = createMemo(() => props.recipe.nodes[selected()]);
  const setNode = (next: RecipeNode): void => {
    const nodes = props.recipe.nodes.map((q, i) => (i === selected() ? next : q));
    props.onChange({ ...props.recipe, nodes });
  };
  const addNode = (kind: NodeKind): void => {
    const made = defaultNode(kind);
    const source = props.recipe.nodes[props.recipe.nodes.length - 1]?.id ?? "";
    if (kind === "filter" || kind === "modulate") made.source = source;
    if (kind === "blend") {
      const terms = props.recipe.nodes.slice(-2).map((q) => ({ source: q.id, weight: 1, mask: null }));
      made.terms = terms.length >= 2 ? terms : [{ source, weight: 1, mask: null }];
    }
    props.onChange({ ...props.recipe, nodes: [...props.recipe.nodes, made], output: made.id });
    setSelected(props.recipe.nodes.length);
  };
  const removeNode = (id: string): void => {
    const nodes = props.recipe.nodes.filter((q) => q.id !== id);
    const output = props.recipe.output === id ? (nodes[0]?.id ?? "") : props.recipe.output;
    props.onChange({ ...props.recipe, nodes, output });
    setSelected(0);
  };
  return (
    <div class="recipe-form">
      <div class="recipe-head">
        <TextField
          label="Resolution X"
          type="number"
          step="1"
          value={props.recipe.resolution[0]}
          onChange={(v) => props.onChange({ ...props.recipe, resolution: [Math.max(1, num(v, 512)), props.recipe.resolution[1]] })}
        />
        <TextField
          label="Resolution Y"
          type="number"
          step="1"
          value={props.recipe.resolution[1]}
          onChange={(v) => props.onChange({ ...props.recipe, resolution: [props.recipe.resolution[0], Math.max(1, num(v, 512))] })}
        />
        <TextField
          label="Cell size"
          type="number"
          step="1"
          value={props.recipe.cell_size}
          onChange={(v) => props.onChange({ ...props.recipe, cell_size: Math.max(0.01, num(v, 64)) })}
        />
        <div class="field">
          <label class="field-name" for="recipe-output">Output node</label>
          <select
            id="recipe-output"
            value={props.recipe.output}
            onChange={(e) => props.onChange({ ...props.recipe, output: e.currentTarget.value })}
          >
            <For each={props.recipe.nodes}>{(q) => <option value={q.id}>{q.id}</option>}</For>
          </select>
        </div>
      </div>
      <div class="row">
        <div class="field">
          <label class="field-name" for="recipe-add">Add node</label>
          <select
            id="recipe-add"
            value=""
            onChange={(e) => {
              if (e.currentTarget.value) addNode(e.currentTarget.value as NodeKind);
            }}
          >
            <option value="">(choose)</option>
            <For each={NODE_KINDS}>{(k) => <option value={k}>{k}</option>}</For>
          </select>
        </div>
      </div>
      <ul class="recipe-nodes" aria-label="Recipe nodes">
        <For each={props.recipe.nodes}>
          {(n, i) => (
            <li>
              <button type="button" classList={{ active: selected() === i() }} onClick={() => setSelected(i())}>
                {n.node} <span class="node-id">{n.id}</span>
                <Show when={props.recipe.output === n.id}>
                  <span class="output-chip">output</span>
                </Show>
              </button>
              <button type="button" aria-label={`Remove node ${n.id}`} onClick={() => removeNode(n.id)}>
                x
              </button>
            </li>
          )}
        </For>
      </ul>
      <Show when={node()}>
        {(n) => (
          <div class="recipe-params" data-node-type={n().node}>
            <Show when={n().node === "generator"}>
              <div class="field">
                <label class="field-name" for="generator-kind">Generator</label>
                <select
                  id="generator-kind"
                  value={n().generator.kind}
                  onChange={(e) => {
                    const made = defaultNode("generator", e.currentTarget.value);
                    setNode({ ...n(), generator: made.generator });
                  }}
                >
                  <For each={GENERATOR_KINDS}>{(k) => <option value={k}>{k}</option>}</For>
                </select>
              </div>
              <Show when={n().generator.kind === "heightmap"}>
                <div class="field">
                  <label class="field-name" for="heightmap-image">Image (PNG)</label>
                  <input
                    id="heightmap-image"
                    type="file"
                    accept="image/png"
                    onChange={(e) => {
                      const file = e.currentTarget.files?.[0];
                      if (!file) return;
                      void file.arrayBuffer().then((buffer) => {
                        try {
                          const image = decodePng(new Uint8Array(buffer));
                          const grid = heightmapFromImage(image.width, image.height, image.rgba);
                          // The image is the terrain's grid: the core evaluates
                          // a heightmap at its own size, not a resampled one.
                          const next = { ...n(), generator: { ...n().generator, ...grid } };
                          setImageError("");
                          props.onChange({
                            ...props.recipe,
                            resolution: [grid.width, grid.height],
                            nodes: props.recipe.nodes.map((q, i) => (i === selected() ? next : q)),
                          });
                        } catch (err) {
                          setImageError(err instanceof Error ? err.message : String(err));
                        }
                      });
                    }}
                  />
                </div>
                <Show when={imageError()}>
                  <p class="invalid-note" role="alert">{imageError()}</p>
                </Show>
              </Show>
            </Show>
            <Show when={n().node === "filter"}>
              <div class="field">
                <label class="field-name" for="filter-op">Filter op</label>
                <select
                  id="filter-op"
                  value={n().op.kind}
                  onChange={(e) => {
                    const made = defaultNode("filter", e.currentTarget.value);
                    setNode({ ...n(), op: made.op });
                  }}
                >
                  <For each={FILTER_OP_KINDS}>{(k) => <option value={k}>{k}</option>}</For>
                </select>
              </div>
            </Show>
            <Show when={n().node === "filter" || n().node === "modulate"}>
              <div class="field">
                <label class="field-name" for="node-source">Source node</label>
                <select
                  id="node-source"
                  value={n().source}
                  onChange={(e) => setNode({ ...n(), source: e.currentTarget.value })}
                >
                  <option value="">(none)</option>
                  <For each={props.recipe.nodes.filter((q) => q.id !== n().id)}>
                    {(q) => <option value={q.id}>{q.id}</option>}
                  </For>
                </select>
              </div>
            </Show>
            <For each={nodeFields(n())}>
              {(spec) => (
                <FieldWidget
                  spec={spec}
                  value={getField(n(), spec)}
                  onChange={(value) => setNode(setField(n(), spec, value))}
                />
              )}
            </For>
            <Show when={n().node === "blend"}>
              <BlendTerms node={n()} onChange={setNode} />
            </Show>
            <Show when={n().node === "blend" || n().node === "modulate"}>
              <Show
                when={n().mask}
                fallback={
                  <button type="button" onClick={() => setNode({ ...n(), mask: defaultMask("from_field") })}>
                    Add mask
                  </button>
                }
              >
                {(mask) => <MaskEditor mask={mask()} onChange={(next) => setNode({ ...n(), mask: next })} />}
              </Show>
            </Show>
            <Show when={n().node === "volume"}>
              <VolumeOps node={n()} onChange={setNode} />
            </Show>
          </div>
        )}
      </Show>
      <Show when={issues().length > 0}>
        <ul class="issues">
          <For each={issues()}>{(i) => <li>{i.nodeId || "recipe"}: {i.message}</li>}</For>
        </ul>
      </Show>
      <div class="row">
        <button type="button" onClick={() => props.onGenerate(props.recipe)}>
          Generate heights
        </button>
        <span class="hint">{storedRecipeJson(props.recipe).length} bytes of recipe</span>
      </div>
    </div>
  );
}

/** The weighted terms of a blend node. */
function BlendTerms(props: { node: RecipeNode; onChange: (node: RecipeNode) => void }): JSX.Element {
  const set = (index: number, term: BlendTerm): void => {
    props.onChange({ ...props.node, terms: props.node.terms.map((t, i) => (i === index ? term : t)) });
  };
  return (
    <div class="blend-terms">
      <h5>Terms</h5>
      <For each={props.node.terms}>
        {(term, i) => (
          <div class="row" data-term={i()}>
            <TextField label="Source" value={term.source} onChange={(v) => set(i(), { ...term, source: v })} />
            <TextField
              label="Weight"
              type="number"
              step="0.05"
              value={term.weight}
              onChange={(v) => set(i(), { ...term, weight: num(v, 1) })}
            />
            <button
              type="button"
              aria-label={`Remove term ${i()}`}
              onClick={() => props.onChange({ ...props.node, terms: props.node.terms.filter((_, k) => k !== i()) })}
            >
              x
            </button>
          </div>
        )}
      </For>
      <div class="field">
        <label class="field-name" for="blend-mode">Blend mode</label>
        <select
          id="blend-mode"
          value={props.node.mode}
          onChange={(e) => props.onChange({ ...props.node, mode: e.currentTarget.value as BlendMode })}
        >
          <For each={BLEND_MODES}>{(m) => <option value={m}>{m}</option>}</For>
        </select>
      </div>
      <button
        type="button"
        onClick={() => props.onChange({ ...props.node, terms: [...props.node.terms, { source: "", weight: 1, mask: null }] })}
      >
        Add term
      </button>
    </div>
  );
}

/** The SDF operation list of a volume node. */
function VolumeOps(props: { node: RecipeNode; onChange: (node: RecipeNode) => void }): JSX.Element {
  return (
    <SdfOpsEditor
      ops={props.node.terrain.ops}
      onChange={(ops) => props.onChange({ ...props.node, terrain: { ...props.node.terrain, ops } })}
    />
  );
}

/** An editable SDF operation list: a recipe volume node's, or a voxel terrain's. */
function SdfOpsEditor(props: { ops: SdfOpNode[]; onChange: (ops: SdfOpNode[]) => void }): JSX.Element {
  const setOps = (next: SdfOpNode[]): void => props.onChange(next);
  return (
    <div class="volume-ops">
      <h5>SDF operations</h5>
      <For each={props.ops}>
        {(op, i) => (
          <div class="row" data-op={op.op}>
            <div class="field">
              <label class="field-name" for={`sdf-op-${i()}`}>Op</label>
              <select
                id={`sdf-op-${i()}`}
                value={op.op}
                onChange={(e) => setOps(props.ops.map((q, k) => (k === i() ? defaultSdfOp(e.currentTarget.value as SdfOpKind) : q)))}
              >
                <For each={SDF_OP_KINDS}>{(k) => <option value={k}>{k}</option>}</For>
              </select>
            </div>
            <Show when={op.shape}>
              {(shape) => (
                <div class="field">
                  <label class="field-name" for={`sdf-shape-${i()}`}>Shape</label>
                  <select
                    id={`sdf-shape-${i()}`}
                    value={shape().kind}
                    onChange={(e) => setOps(props.ops.map((q, k) => (k === i() ? { ...q, shape: defaultSdfShape(e.currentTarget.value as SdfShapeKind) } : q)))}
                  >
                    <For each={SDF_SHAPE_KINDS}>{(k) => <option value={k}>{k}</option>}</For>
                  </select>
                </div>
              )}
            </Show>
            <For each={SDF_OP_FIELDS[op.op]}>
              {(spec) => (
                <FieldWidget
                  spec={spec}
                  value={getSdfOpField(op, spec)}
                  onChange={(value) => setOps(props.ops.map((q, k) => (k === i() ? setSdfOpField(q, spec, value) : q)))}
                />
              )}
            </For>
            <For each={op.shape ? SDF_SHAPE_FIELDS[op.shape.kind] : []}>
              {(spec) => (
                <FieldWidget
                  spec={spec}
                  value={getSdfShapeField(op.shape as SdfShapeNode, spec)}
                  onChange={(value) => setOps(props.ops.map((q, k) => (k === i() ? { ...q, shape: setSdfShapeField(q.shape as SdfShapeNode, spec, value) } : q)))}
                />
              )}
            </For>
            <button type="button" aria-label={`Remove operation ${i()}`} onClick={() => setOps(props.ops.filter((_, k) => k !== i()))}>
              x
            </button>
          </div>
        )}
      </For>
      <button type="button" onClick={() => setOps([...props.ops, defaultSdfOp(props.ops.length === 0 ? "add" : "union")])}>
        Add operation
      </button>
    </div>
  );
}


function TerrainEditor(props: { object: DocObject; editor: Editor; materials: readonly string[] }): JSX.Element {
  const terrain = (): Terrain | null => (props.object.kind.type === "terrain" ? props.object.kind.terrain : null);
  const [recipe, setRecipe] = createSignal<Recipe>(storedRecipe(terrain()));
  const patch = (next: Partial<Terrain>): void => {
    const t = terrain();
    if (!t) return;
    props.editor.apply({ op: "replace_objects", objects: [{ ...props.object, kind: { type: "terrain", terrain: { ...t, ...next } } }] });
  };
  return (
    <div class="kind-editor">
      <div class="row">
        <TextField
          label="Resolution X"
          type="number"
          step="1"
          value={terrain()?.resolution[0] ?? 512}
          onChange={(v) => patch({ resolution: [Math.max(1, num(v, 512)), terrain()?.resolution[1] ?? 512] })}
        />
        <TextField
          label="Resolution Y"
          type="number"
          step="1"
          value={terrain()?.resolution[1] ?? 512}
          onChange={(v) => patch({ resolution: [terrain()?.resolution[0] ?? 512, Math.max(1, num(v, 512))] })}
        />
        <TextField label="Cell size" type="number" step="1" value={terrain()?.cell_size ?? 64} onChange={(v) => patch({ cell_size: Math.max(0.01, num(v, 64)) })} />
        <TextField label="Chunk size" type="number" step="1" value={terrain()?.chunk_size ?? 64} onChange={(v) => patch({ chunk_size: Math.max(1, num(v, 64)) })} />
      </div>
      <h4>Layers</h4>
      <ul class="terrain-layers">
        <For each={terrain()?.layers ?? []}>
          {(layer, i) => (
            <li>
              <TextField label="Name" value={layer.name} onChange={(v) => patch({ layers: replaceLayer(terrain(), i(), { name: v }) })} />
              <MaterialField
                label={`Layer ${i()} material`}
                value={layer.material}
                project={props.materials}
                onChange={(v) => patch({ layers: replaceLayer(terrain(), i(), { material: v }) })}
              />
              <TextField
                label="UV scale"
                type="number"
                step="0.01"
                value={layer.uv_scale}
                onChange={(v) => patch({ layers: replaceLayer(terrain(), i(), { uv_scale: num(v, 0.05) }) })}
              />
              <span class="hint">{layer.scatter.length} scatter rules</span>
            </li>
          )}
        </For>
      </ul>
      <AutoPaintRules terrain={terrain()} editor={props.editor} objectId={props.object.id} onChange={(rules) => patch({ recipe: { ...(terrain()?.recipe as object), autopaint: rules } })} />
      <h4>Scatter</h4>
      <For each={terrain()?.layers ?? []}>
        {(layer, i) => (
          <div class="scatter-rule" data-layer={layer.name}>
            <h5>{layer.name}</h5>
            <For each={layer.scatter}>
              {(rule, j) => (
                <div class="row">
                  <TextField
                    label="Models"
                    value={rule.models.join(" ")}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { models: v.split(/[\s,]+/).filter(Boolean) }) })}
                  />
                  <TextField
                    label="Density"
                    type="number"
                    step="0.1"
                    value={rule.density}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { density: num(v) }) })}
                  />
                  <TextField
                    label="Min weight"
                    type="number"
                    step="1"
                    value={rule.min_weight}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { min_weight: num(v) }) })}
                  />
                  <TextField
                    label="Slope min"
                    type="number"
                    step="1"
                    value={rule.slope_deg[0]}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { slope_deg: [num(v), rule.slope_deg[1]] }) })}
                  />
                  <TextField
                    label="Slope max"
                    type="number"
                    step="1"
                    value={rule.slope_deg[1]}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { slope_deg: [rule.slope_deg[0], num(v)] }) })}
                  />
                  <TextField
                    label="Height min"
                    type="number"
                    step="1"
                    value={rule.height[0]}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { height: [num(v), rule.height[1]] }) })}
                  />
                  <TextField
                    label="Height max"
                    type="number"
                    step="1"
                    value={rule.height[1]}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { height: [rule.height[0], num(v)] }) })}
                  />
                  <TextField
                    label="Scale min"
                    type="number"
                    step="0.05"
                    value={rule.scale[0]}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { scale: [num(v, 1), rule.scale[1]] }) })}
                  />
                  <TextField
                    label="Scale max"
                    type="number"
                    step="0.05"
                    value={rule.scale[1]}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { scale: [rule.scale[0], num(v, 1)] }) })}
                  />
                  <TextField
                    label="Align to normal"
                    type="number"
                    step="0.05"
                    value={rule.align_to_normal}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { align_to_normal: num(v) }) })}
                  />
                  <TextField
                    label="Seed"
                    type="number"
                    step="1"
                    value={rule.seed}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { seed: num(v) }) })}
                  />
                  <TextField
                    label="Static"
                    type="checkbox"
                    value={rule.static_prop ? 1 : 0}
                    onChange={(v) => patch({ layers: setScatter(terrain(), i(), j(), { static_prop: v === "1" }) })}
                  />
                  <button
                    type="button"
                    aria-label={`Remove scatter rule ${j()}`}
                    onClick={() => patch({ layers: removeScatter(terrain(), i(), j()) })}
                  >
                    x
                  </button>
                </div>
              )}
            </For>
            <button
              type="button"
              onClick={() =>
                patch({
                  layers: addScatter(terrain(), i(), {
                    models: [`mdl/props/${layer.name}.rmdl`],
                    density: 1,
                    min_weight: 128,
                    slope_deg: [0, 45],
                    height: [0, 4096],
                    scale: [0.9, 1.1],
                    align_to_normal: 1,
                    seed: 1,
                    static_prop: true,
                  }),
                })
              }
            >
              Add scatter rule
            </button>
          </div>
        )}
      </For>
      <button
        type="button"
        onClick={() => {
          const t = terrain();
          if (!t) return;
          const placements = props.editor.terrainScatter(props.object.id);
          props.editor.note(`scatter added ${placements.length} objects`);
        }}
      >
        Apply scatter rules
      </button>
      <h4>Splat paint</h4>
      <SplatPanel object={props.object} editor={props.editor} />
      <h4>Recipe</h4>
      <RecipeForm
        recipe={recipe()}
        onChange={setRecipe}
        onGenerate={(r) => {
          try {
            const heights = props.editor.terrainGenerate(generateRequestJson(r));
            const path = `terrain/${props.object.id}.f32`;
            props.editor.apply(setFile(path, new Uint8Array(heights.buffer, heights.byteOffset, heights.byteLength)));
            patch({
              heights: path,
              resolution: r.resolution,
              cell_size: r.cell_size,
              recipe: JSON.parse(storedRecipeJson(r)),
            });
            setRecipe(r);
            props.editor.note(`generated ${r.resolution[0] * r.resolution[1]} samples`);
          } catch (e) {
            props.editor.note(e instanceof Error ? e.message : String(e));
          }
        }}
      />
      <SculptPanel object={props.object} editor={props.editor} />
    </div>
  );
}

/** The auto-paint rule stack, edited per layer and sent to the core. */
function AutoPaintRules(props: {
  terrain: Terrain | null;
  editor: Editor;
  objectId: Id;
  onChange: (rules: LayerRule[]) => void;
}): JSX.Element {
  const rules = createMemo<LayerRule[]>(() => {
    const stored = (props.terrain?.recipe as { autopaint?: LayerRule[] } | null)?.autopaint;
    const layers = props.terrain?.layers ?? [];
    return layers.map((l, i) => stored?.[i] ?? defaultLayerRule(l.name, i === 0 ? "height" : "slope"));
  });
  const set = (index: number, next: LayerRule): void => {
    props.onChange(rules().map((r, i) => (i === index ? next : r)));
  };
  const apply = (): void => {
    try {
      const touched = props.editor.terrainAutoPaint(props.objectId, rulesJson(rules()));
      props.editor.note(`auto-paint: ${touched.length} objects changed`);
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <div class="auto-paint">
      <h4>Auto-paint</h4>
      <For each={rules()}>
        {(rule, i) => (
          <div class="auto-rule" data-layer={rule.name}>
            <h5>{rule.name || `layer ${i()}`}</h5>
            <For each={RULE_SPECS}>
              {(field) => (
                <RuleWidget
                  spec={field}
                  value={getRuleField(rule, field)}
                  onChange={(value) => set(i(), setRuleField(rule, field, value as string | number))}
                />
              )}
            </For>
          </div>
        )}
      </For>
      <button type="button" disabled={!props.terrain} onClick={apply}>
        Auto-paint weights
      </button>
      <span class="hint">{LAYER_DRIVERS.length} drivers</span>
    </div>
  );
}

/** One widget for one splat rule field. */
function RuleWidget(props: {
  spec: RuleSpec;
  value: string | number;
  onChange: (value: string | number) => void;
}): JSX.Element {
  if (props.spec.type === "enum") {
    return (
      <div class="field">
        <label class="field-name">{props.spec.label}</label>
        <select value={String(props.value)} onChange={(e) => props.onChange(e.currentTarget.value)}>
          <For each={props.spec.choices ?? []}>{(c) => <option value={c}>{c}</option>}</For>
        </select>
      </div>
    );
  }
  if (props.spec.type === "string") {
    return <TextField label={props.spec.label} value={String(props.value)} onChange={(v) => props.onChange(v)} />;
  }
  return (
    <TextField
      label={props.spec.label}
      type="number"
      step={String(props.spec.step ?? 1)}
      min={props.spec.min}
      max={props.spec.max}
      value={Number(props.value) || 0}
      onChange={(v) => props.onChange(num(v))}
    />
  );
}

/** The recipe a terrain object carries, or a fresh one when it has none. */
function storedRecipe(terrain: Terrain | null): Recipe {
  const stored = terrain?.recipe;
  if (!stored || typeof stored !== "object") return defaultRecipe();
  try {
    return parseRecipe(JSON.stringify({
      ...(stored as object),
      resolution: terrain?.resolution,
      cell_size: terrain?.cell_size,
    }));
  } catch {
    return defaultRecipe();
  }
}

function replaceLayer(terrain: Terrain | null, index: number, next: Partial<Terrain["layers"][number]>): Terrain["layers"] {
  return (terrain?.layers ?? []).map((l, i) => (i === index ? { ...l, ...next } : l));
}

interface ScatterRule {
  models: string[];
  density: number;
  min_weight: number;
  slope_deg: [number, number];
  height: [number, number];
  scale: [number, number];
  align_to_normal: number;
  seed: number;
  static_prop: boolean;
}

function setScatter(terrain: Terrain | null, layer: number, rule: number, next: Partial<ScatterRule>): Terrain["layers"] {
  return (terrain?.layers ?? []).map((l, i) =>
    i === layer ? { ...l, scatter: l.scatter.map((r, k) => (k === rule ? { ...r, ...next } : r)) } : l,
  );
}

function addScatter(terrain: Terrain | null, layer: number, rule: ScatterRule): Terrain["layers"] {
  return (terrain?.layers ?? []).map((l, i) => (i === layer ? { ...l, scatter: [...l.scatter, rule] } : l));
}

function removeScatter(terrain: Terrain | null, layer: number, rule: number): Terrain["layers"] {
  return (terrain?.layers ?? []).map((l, i) =>
    i === layer ? { ...l, scatter: l.scatter.filter((_, k) => k !== rule) } : l,
  );
}

/** Picks the splat layer the paint tool writes and re-runs the auto-paint. */
function SplatPanel(props: { object: DocObject; editor: Editor }): JSX.Element {
  const terrain = (): Terrain | null => (props.object.kind.type === "terrain" ? props.object.kind.terrain : null);
  const setLayer = (index: number): void => setSculpt({ layer: index });
  const apply = (): void => {
    const t = terrain();
    if (!t) return;
    const rules = t.layers.map((l, index) => defaultLayerRule(l.name, index === state.tool.sculpt.layer ? "height" : "constant"));
    try {
      const touched = props.editor.terrainAutoPaint(props.object.id, rulesJson(rules));
      props.editor.note(`paint: ${touched.length} objects repainted`);
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <div class="splat-panel">
      <div class="row">
        <div class="field">
          <label class="field-name" for="splat-layer">Layer</label>
          <select
            id="splat-layer"
            value={String(state.tool.sculpt.layer)}
            onChange={(e) => setLayer(Math.max(0, num(e.currentTarget.value)))}
          >
            <For each={terrain()?.layers ?? []}>{(l, i) => <option value={String(i())}>{l.name}</option>}</For>
          </select>
        </div>
        <TextField
          label="Brush radius"
          type="number"
          step="16"
          value={state.tool.sculpt.radius}
          onChange={(v) => setSculpt({ radius: Math.max(1, num(v, 128)) })}
        />
        <button type="button" disabled={!terrain()} onClick={apply}>
          Paint layer
        </button>
      </div>
      <p class="hint">
        the paint tool drags this layer over the terrain under the cursor
      </p>
    </div>
  );
}

/** The sculpt brush, writing its configuration into the shared tool state. */
function SculptPanel(props: { object: DocObject; editor: Editor }): JSX.Element {
  const [stroke, setStroke] = createSignal<Stroke>(defaultStroke(state.tool.sculpt.brush));
  const brush = state.tool.sculpt;
  const patchStroke = (next: Partial<Stroke>): void => {
    setStroke({ ...stroke(), ...next });
  };
  const share = (next: Partial<typeof brush>): void => {
    setSculpt(next);
    patchStroke({
      radius: next.radius ?? brush.radius,
      strength: next.strength ?? brush.strength,
      falloff: next.falloff ?? brush.falloff,
    });
  };
  const apply = (): void => {
    const s = stroke();
    const at = props.editor.document();
    void at;
    if (s.points.length === 0) {
      props.editor.note("sculpt: drag in the viewport to place the stroke");
      return;
    }
    try {
      const touched = props.editor.terrainSculpt(props.object.id, strokeJson(s));
      props.editor.note(`sculpt: ${touched.length} objects changed`);
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <div class="sculpt-panel">
      <div class="row">
        <div class="field">
          <label class="field-name" for="sculpt-tool">Tool</label>
          <select
            id="sculpt-tool"
            value={stroke().tool}
            onChange={(e) => {
              const tool = e.currentTarget.value as SculptTool;
              setSculpt({ brush: tool });
              setStroke({ ...defaultStroke(tool), radius: brush.radius, strength: brush.strength, falloff: brush.falloff });
            }}
          >
            <For each={SCULPT_TOOLS}>{(t) => <option value={t}>{t}</option>}</For>
          </select>
        </div>
        <div class="field">
          <label class="field-name" for="sculpt-falloff">Falloff</label>
          <select
            id="sculpt-falloff"
            value={falloffName(stroke().falloff)}
            onChange={(e) => {
              const value = falloffValue(e.currentTarget.value as Falloff);
              share({ falloff: value });
            }}
          >
            <For each={FALLOFFS}>{(f) => <option value={f}>{f}</option>}</For>
          </select>
        </div>
      </div>
      <div class="row">
        <TextField label="Radius" type="number" step="16" value={brush.radius} onChange={(v) => share({ radius: Math.max(1, num(v, 128)) })} />
        <TextField label="Strength" type="number" step="1" value={brush.strength} onChange={(v) => share({ strength: num(v, 32) })} />
        <TextField
          label="Pressure"
          type="number"
          step="0.05"
          value={brush.pressure}
          onChange={(v) => setSculpt({ pressure: Math.min(1, Math.max(0, num(v, 1))) })}
        />
        <TextField
          label="Target height"
          type="number"
          step="1"
          value={stroke().target_height}
          onChange={(v) => patchStroke({ target_height: num(v) })}
        />
        <TextField
          label="Benches per inch"
          type="number"
          step="1"
          value={stroke().step}
          onChange={(v) => patchStroke({ step: Math.max(0.01, num(v, 24)) })}
        />
      </div>
      <div class="row">
        <TextField
          label="Seed"
          type="number"
          step="1"
          value={stroke().seed}
          onChange={(v) => patchStroke({ seed: Math.max(0, Math.round(num(v))) })}
        />
        <TextField
          label="Noise frequency"
          type="number"
          step="0.001"
          value={stroke().noise_frequency}
          onChange={(v) => patchStroke({ noise_frequency: Math.max(0.000001, num(v, 0.01)) })}
        />
        <div class="field">
          <label class="field-name" for="sculpt-blend">Stamp blend</label>
          <select
            id="sculpt-blend"
            value={stroke().blend}
            onChange={(e) => patchStroke({ blend: e.currentTarget.value as Stroke["blend"] })}
          >
            <For each={STAMP_BLEND_MODES}>{(m) => <option value={m}>{m}</option>}</For>
          </select>
        </div>
        <TextField
          label="Stamp rotation"
          type="number"
          step="0.05"
          value={stroke().rotation}
          onChange={(v) => patchStroke({ rotation: num(v) })}
        />
      </div>
      <Show when={stroke().tool === "stamp"}>
        <p class="hint">
          the stamp brush is a heightfield the core reads from the project; {stroke().brush ? `${stroke().brush?.width} by ${stroke().brush?.height}` : "none loaded"}
        </p>
      </Show>
      <button type="button" onClick={apply}>
        Apply stroke
      </button>
    </div>
  );
}

function VoxelEditor(props: { object: DocObject; editor: Editor; materials: readonly string[] }): JSX.Element {
  const voxel = (): VoxelTerrain | null => (props.object.kind.type === "voxel_terrain" ? props.object.kind.voxel_terrain : null);
  const patch = (next: Partial<VoxelTerrain>): void => {
    const v = voxel();
    if (!v) return;
    props.editor.apply({ op: "replace_objects", objects: [{ ...props.object, kind: { type: "voxel_terrain", voxel_terrain: { ...v, ...next } } }] });
  };
  // Stored in the core's form (`rterrain::sdf::SdfOp`), edited in the form's.
  const ops = createMemo(() => sdfOpsFromWire(voxel()?.ops));
  return (
    <div class="kind-editor">
      <TextField label="Voxel size" type="number" step="1" value={voxel()?.voxel_size ?? 16} onChange={(v) => patch({ voxel_size: Math.max(0.1, num(v, 16)) })} />
      <MaterialField label="Material" value={voxel()?.material ?? ""} project={props.materials} onChange={(v) => patch({ material: v })} />
      <TextField label="UV scale" type="number" step="0.01" value={voxel()?.uv_scale ?? 0.05} onChange={(v) => patch({ uv_scale: num(v, 0.05) })} />
      <TripleEditor label="Bounds min" value={voxel()?.bounds[0] ?? [-256, -256, -128]} step="16" onChange={(v) => patch({ bounds: [v, voxel()?.bounds[1] ?? [256, 256, 128]] })} />
      <TripleEditor label="Bounds max" value={voxel()?.bounds[1] ?? [256, 256, 128]} step="16" onChange={(v) => patch({ bounds: [voxel()?.bounds[0] ?? [-256, -256, -128], v] })} />
      <p class="hint">the first operation must be add; the voxel tool adds a sphere where you click</p>
      <SdfOpsEditor ops={ops()} onChange={(next) => patch({ ops: sdfOpsToWire(next) })} />
    </div>
  );
}

/** The right-hand panel: the selected object's fields plus validation. */
function recordLabel(r: RefRecord): string {
  switch (r.type) {
    case "entity":
      return `entity block ${r.index}`;
    case "prop":
      return `static prop ${r.index}`;
    case "mesh":
      return `mesh group ${r.material} of model ${r.model}`;
    case "collision":
      return `collision ${r.surface} (mask 0x${r.contents.toString(16)}) of model ${r.model}`;
  }
}

export function Inspector(props: InspectorProps): JSX.Element {
  const kind = createMemo(() => props.object?.kind.type ?? "none");
  return (
    <aside tabIndex={-1} class="panel inspector" aria-label="Inspector">
      <header class="panel-head">
        <h2>{props.object?.name ?? "Inspector"}</h2>
        <span class="kind-chip">{kind()}</span>
      </header>
      <Show when={!props.object} fallback={
        <Show when={props.object} fallback={<p class="hint">nothing selected</p>}>
          {(object) => (
            <div class="inspector-body">
              <Show when={props.editor.referenceRecord(object().id)}>
                {(record) => (
                  <p class="hint reference-record">
                    {`shipped ${recordLabel(record())}: `}
                    {object().hidden
                      ? "the map's own copy compiles; editing this object compiles it in that copy's place"
                      : "this object compiles in place of the map's copy"}
                  </p>
                )}
              </Show>
              <TransformEditor
                object={object()}
                onChange={(patch) => props.editor.setTransform(object().id, patch)}
              />
              <SwitchKind
                object={object()}
                editor={props.editor}
                suggestions={props.suggestions}
                targets={props.targets}
              />
            </div>
          )}
        </Show>
      }>
        <Show
          when={state.tool.id === "entity"}
          fallback={<p class="hint">select an object to edit it</p>}
        >
          <EntityForm
            schema={props.editor.schema}
            object={null}
            issues={props.issues}
            suggestions={props.suggestions}
            targets={props.targets}
            onCreate={(classname) => {
              setState("tool", "entityClass", classname);
              const at = state.cursorWorld ?? [0, 0, 0];
              const added = props.editor.addEntity(classname, at);
              if (added.length > 0) select(added);
            }}
            onChange={() => undefined}
          />
        </Show>
      </Show>
      <Show when={props.issues.length > 0}>
        <ul class="issues" aria-label="Validation">
          <For each={props.issues}>{(i) => <li>{i.id ? `${i.id.slice(0, 8)}: ` : ""}{i.message}</li>}</For>
        </ul>
      </Show>
      <RedoPanel editor={props.editor} />
    </aside>
  );
}

function SwitchKind(props: {
  object: DocObject;
  editor: Editor;
  suggestions: Record<string, string[]>;
  targets: { id: string; name: string }[];
}): JSX.Element {
  const kind = () => props.object.kind.type;
  return (
    <>
      <Show when={kind() === "brush"}>
        <BrushEditor object={props.object} editor={props.editor} materials={props.suggestions.material ?? []} />
      </Show>
      <Show when={kind() === "prop"}>
        <PropEditor object={props.object} editor={props.editor} suggestions={props.suggestions} />
      </Show>
      <Show when={kind() === "mesh"}>
        <MeshEditor object={props.object} editor={props.editor} suggestions={props.suggestions} />
      </Show>
      <Show when={kind() === "light"}>
        <LightEditor object={props.object} editor={props.editor} />
      </Show>
      <Show when={kind() === "volume"}>
        <VolumeEditor object={props.object} editor={props.editor} />
      </Show>
      <Show when={kind() === "path"}>
        <PathEditor object={props.object} editor={props.editor} />
      </Show>
      <Show when={kind() === "terrain"}>
        <TerrainEditor object={props.object} editor={props.editor} materials={props.suggestions.material ?? []} />
      </Show>
      <Show when={kind() === "voxel_terrain"}>
        <VoxelEditor object={props.object} editor={props.editor} materials={props.suggestions.material ?? []} />
      </Show>
      <Show when={kind() === "entity"}>
        <EntityForm
          schema={props.editor.schema}
          object={props.object}
          issues={props.editor.validate()}
          suggestions={props.suggestions}
          targets={props.targets}
          onCreate={null}
          onChange={(next) => props.editor.apply({ op: "replace_objects", objects: [next] })}
        />
      </Show>
      <Show when={kind() === "reference"}>
        <div class="kind-editor">
          <p class="hint">
            reference {props.object.kind.type === "reference" ? props.object.kind.reference.source : ""}
          </p>
        </div>
      </Show>
      <Show when={kind() === "prefab_instance"}>
        <div class="kind-editor">
          <TextField
            label="Prefab"
            value={props.object.kind.type === "prefab_instance" ? props.object.kind.prefab_instance.prefab : ""}
            onChange={(v) => props.editor.apply({
              op: "replace_objects",
              objects: [{ ...props.object, kind: { type: "prefab_instance", prefab_instance: { prefab: v } } }],
            })}
          />
        </div>
      </Show>
    </>
  );
}

function MeshEditor(props: { object: DocObject; editor: Editor; suggestions: Record<string, string[]> }): JSX.Element {
  const mesh = () => (props.object.kind.type === "mesh" ? props.object.kind.mesh : null);
  const patch = (next: Partial<NonNullable<ReturnType<typeof mesh>>>): void => {
    const m = mesh();
    if (!m) return;
    props.editor.apply({ op: "replace_objects", objects: [{ ...props.object, kind: { type: "mesh", mesh: { ...m, ...next } } }] });
  };
  return (
    <div class="kind-editor">
      <TextField label="Source" value={mesh()?.source ?? ""} onChange={(v) => patch({ source: v })} />
      <div class="row">
        <button
          type="button"
          onClick={() => {
            select([props.object.id]);
            props.editor.toggleMeshEdit();
          }}
        >
          Edit mesh (Tab)
        </button>
        <Show when={!(mesh()?.source ?? "").endsWith(".r5mesh")}>
          <button
            type="button"
            onClick={() => {
              try {
                props.editor.meshMakeEditable(props.object.id);
                props.editor.note("made editable");
              } catch (e) {
                props.editor.note(e instanceof Error ? e.message : String(e));
              }
            }}
          >
            Make Editable
          </button>
        </Show>
      </div>
      <div class="field">
        <label class="field-name" for="mesh-collision">Collision</label>
        <select
          id="mesh-collision"
          value={mesh()?.collision ?? "triangles"}
          onChange={(e) => patch({ collision: e.currentTarget.value as "triangles" | "convex_hulls" | "none" })}
        >
          <option value="triangles">triangles</option>
          <option value="convex_hulls">convex hulls</option>
          <option value="none">none</option>
        </select>
      </div>
      <TextField label="Cast shadows" type="checkbox" value={mesh()?.cast_shadows ? 1 : 0} onChange={(v) => patch({ cast_shadows: v === "1" })} />
      <TextField
        label="Cutter (boolean operand: drawn as wire, not compiled)"
        type="checkbox"
        value={mesh()?.role === "cutter" ? 1 : 0}
        onChange={(v) => patch({ role: v === "1" ? "cutter" : "normal" })}
      />
      <ModifierStack object={props.object} editor={props.editor} modifiers={mesh()?.modifiers ?? []} />
      <TextField label="Lightmap scale" type="number" step="1" value={mesh()?.lightmap_scale ?? 0} onChange={(v) => patch({ lightmap_scale: num(v, 0) })} />
      <h4>Material map</h4>
      <ul class="material-map">
        <For each={Object.entries(mesh()?.material_map ?? {})}>
          {([gl, game]) => (
            <li>
              <MaterialField
                label={gl}
                value={game}
                project={props.suggestions.material ?? []}
                onChange={(v) => patch({ material_map: { ...(mesh()?.material_map ?? {}), [gl]: v } })}
              />
            </li>
          )}
        </For>
      </ul>
    </div>
  );
}

/** The mesh's modifier stack: each entry's toggle, order, apply, remove and fields. */
function ModifierStack(props: { object: DocObject; editor: Editor; modifiers: Modifier[] }): JSX.Element {
  const [adding, setAdding] = createSignal<string>(MODIFIER_KINDS[0].type);
  const run = (request: string): void => {
    try {
      props.editor.runMeshOp(request);
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };
  /** Objects a ref field may name, the edited mesh excluded. */
  const candidates = (kinds: readonly string[]): DocObject[] =>
    props.editor
      .currentObjects()
      .map((o) => o.doc)
      .filter((d) => d.id !== props.object.id && kinds.includes(d.kind.type));
  /** A mesh picked as a boolean operand is offered the cutter role, as the spec asks. */
  const offerCutter = (id: unknown): void => {
    if (typeof id !== "string") return;
    const doc = props.editor.currentObjects().find((o) => o.doc.id === id)?.doc;
    if (!doc || doc.kind.type !== "mesh" || doc.kind.mesh.role === "cutter") return;
    if (!window.confirm(`Make "${doc.name}" a cutter? It then draws as wire and the compiler leaves it out.`)) return;
    props.editor.apply({ op: "replace_objects", objects: [{ ...doc, kind: { type: "mesh", mesh: { ...doc.kind.mesh, role: "cutter" } } }] });
  };
  const add = (): void => {
    const kind = modifierKind(adding());
    if (!kind) return;
    const made = kind.make((kinds) => candidates(kinds)[0]?.id ?? null);
    if (!made) {
      props.editor.note(`a ${kind.label} modifier needs another object to point at; add one first`);
      return;
    }
    run(stackRequest.add(props.object.id, made));
    if (made.type === "boolean") offerCutter(made.object);
  };
  return (
    <section class="modifier-stack" aria-label="Modifiers">
      <h4>Modifiers</h4>
      <For each={props.modifiers}>
        {(m, i) => {
          const kind = modifierKind(m.type);
          return (
            <fieldset class="modifier" data-modifier={m.type}>
              <legend>
                {kind?.label ?? m.type}
                <Show when={m.enabled === false}> (off)</Show>
              </legend>
              <div class="row">
                <TextField label="Enabled" type="checkbox" value={m.enabled === false ? 0 : 1} onChange={() => run(stackRequest.toggle(props.object.id, i()))} />
                <button type="button" disabled={i() === 0} onClick={() => run(stackRequest.move(props.object.id, i(), i() - 1))}>
                  Up
                </button>
                <button type="button" disabled={i() === props.modifiers.length - 1} onClick={() => run(stackRequest.move(props.object.id, i(), i() + 1))}>
                  Down
                </button>
                <button type="button" onClick={() => run(stackRequest.apply(props.object.id, i()))}>
                  Apply
                </button>
                <button type="button" onClick={() => run(stackRequest.remove(props.object.id, i()))}>
                  Remove
                </button>
              </div>
              <For each={kind?.fields ?? []}>
                {(f) => (
                  <ModifierField
                    field={f}
                    value={m[f.key]}
                    candidates={f.objects ? candidates(f.objects) : []}
                    onChange={(v) => {
                      run(stackRequest.set(props.object.id, i(), withField(m, f.key, v)));
                      if (m.type === "boolean" && f.key === "object") offerCutter(v);
                    }}
                  />
                )}
              </For>
            </fieldset>
          );
        }}
      </For>
      <div class="row">
        <select aria-label="Modifier kind" value={adding()} onChange={(e) => setAdding(e.currentTarget.value)}>
          <For each={MODIFIER_KINDS}>{(k) => <option value={k.type}>{k.label}</option>}</For>
        </select>
        <button type="button" onClick={add}>
          Add modifier
        </button>
      </div>
    </section>
  );
}

/** One modifier param; empty text on an optional field clears it back to the default. */
function ModifierField(props: { field: ModField; value: unknown; candidates: DocObject[]; onChange: (v: unknown) => void }): JSX.Element {
  const f = props.field;
  const id = `mod-${f.key}`;
  const text = (v: unknown): string => (Array.isArray(v) ? v.join(" ") : v === undefined || v === null ? "" : String(v));
  switch (f.kind) {
    case "bool":
      return <TextField label={f.key} type="checkbox" value={props.value ? 1 : 0} onChange={(v) => props.onChange(v === "1")} />;
    case "enum":
      return (
        <div class="field">
          <label class="field-name" for={id}>
            {f.key}
          </label>
          <select id={id} value={String(props.value ?? f.options?.[0] ?? "")} onChange={(e) => props.onChange(e.currentTarget.value)}>
            <For each={f.options ?? []}>{(o) => <option value={o}>{o}</option>}</For>
          </select>
        </div>
      );
    case "object":
      return (
        <div class="field">
          <label class="field-name" for={id}>
            {f.key}
          </label>
          <select id={id} value={String(props.value ?? "")} onChange={(e) => props.onChange(e.currentTarget.value === "" ? null : e.currentTarget.value)}>
            <option value="">(none)</option>
            <For each={props.candidates}>{(o) => <option value={o.id}>{o.name}</option>}</For>
          </select>
        </div>
      );
    case "axes": {
      const axes = (Array.isArray(props.value) ? props.value : [false, false, false]) as boolean[];
      return (
        <div class="row">
          <For each={["X", "Y", "Z"]}>
            {(label, k) => (
              <TextField
                label={`mirror ${label}`}
                type="checkbox"
                value={axes[k()] ? 1 : 0}
                onChange={(v) => props.onChange(axes.map((a, j) => (j === k() ? v === "1" : a)))}
              />
            )}
          </For>
        </div>
      );
    }
    case "vec3":
    case "vec2":
    case "optional_vec3": {
      const n = f.kind === "vec2" ? 2 : 3;
      return (
        <TextField
          label={f.key}
          value={text(props.value)}
          onChange={(v) => {
            if (v.trim() === "" && f.kind === "optional_vec3") return props.onChange(null);
            const vec = parseVec(v, n);
            if (vec) props.onChange(vec);
          }}
        />
      );
    }
    case "optional_number":
      return (
        <TextField
          label={f.key}
          type="number"
          step="any"
          min={f.min}
          max={f.max}
          value={text(props.value)}
          onChange={(v) => props.onChange(v.trim() === "" ? null : Number(v))}
        />
      );
    default:
      return (
        <TextField
          label={f.key}
          type="number"
          step={f.kind === "int" ? "1" : "any"}
          min={f.min}
          max={f.max}
          value={typeof props.value === "number" ? props.value : 0}
          onChange={(v) => props.onChange(f.kind === "int" ? Math.round(Number(v)) : Number(v))}
        />
      );
  }
}

export { parseRecipe, defaultRecipe };
