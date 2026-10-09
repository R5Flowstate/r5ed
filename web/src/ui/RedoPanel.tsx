import { type JSX, For, Show, createEffect, createSignal } from "solid-js";
import type { Editor } from "../state/editor";
import type { LastMeshOp } from "../state/meshEdit";
import { type OpField, specForRequest } from "../state/meshOps";
import { state } from "../state/stores";
import { TextField } from "./fields";

/** Adjust Last Operation: the last mesh op's params as fields; editing one re-runs undo + apply. */
export function RedoPanel(props: { editor: Editor }): JSX.Element {
  const last = (): LastMeshOp | null => (void state.assets.revision, props.editor.lastMeshOp);
  const key = (): string => {
    const l = last();
    return l ? `${l.request.op}:${l.depth}` : "";
  };
  const [draft, setDraft] = createSignal<Record<string, unknown> | null>(null);
  createEffect(() => {
    void key();
    setDraft(null);
  });
  const params = (): Record<string, unknown> => draft() ?? last()?.request.params ?? {};
  const setParam = (name: string, value: unknown): void => {
    if (!last()) return;
    const next = { ...params(), [name]: value };
    setDraft(next);
    try {
      props.editor.adjustLastOp(next);
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <Show when={last()}>
      {(l) => (
        <section class="redo-panel" data-redo-panel aria-label="Adjust last operation">
          <h3>Adjust: {l().request.op}</h3>
          <For each={Object.entries(params())}>
            {([name, value]) => (
              <ParamField
                name={name}
                value={value}
                field={specForRequest(l().request.op, params())?.fields.find((f) => f.key === name)}
                onChange={(v) => setParam(name, v)}
              />
            )}
          </For>
          <p class="hint">
            V{l().report.verts} E{l().report.edges} F{l().report.faces}
          </p>
          <Show when={l().report.warnings.length > 0}>
            <ul class="issues" aria-label="Op warnings">
              <For each={l().report.warnings}>{(w) => <li>{w}</li>}</For>
            </ul>
          </Show>
        </section>
      )}
    </Show>
  );
}

function ParamField(props: { name: string; value: unknown; field?: OpField; onChange: (v: unknown) => void }): JSX.Element {
  const v = (): unknown => props.value;
  const f = props.field;
  if (f?.kind === "enum" && f.options) {
    const id = `redo-${props.name}`;
    return (
      <div class="field">
        <label class="field-name" for={id}>
          {props.name}
        </label>
        <select id={id} value={String(v())} onChange={(e) => props.onChange(e.currentTarget.value)}>
          <For each={f.options}>{(o) => <option value={o}>{o}</option>}</For>
        </select>
      </div>
    );
  }
  if (typeof v() === "number" && f && (f.kind === "int" || f.kind === "number")) {
    return (
      <TextField
        label={props.name}
        type="number"
        step={f.kind === "int" ? "1" : "any"}
        min={f.min}
        max={f.max}
        value={v() as number}
        onChange={(s) => props.onChange(f.kind === "int" ? Math.round(Number(s)) : Number(s))}
      />
    );
  }
  if (typeof v() === "boolean") {
    return <TextField label={props.name} type="checkbox" value={v() ? 1 : 0} onChange={(s) => props.onChange(s === "1")} />;
  }
  if (typeof v() === "number") {
    return <TextField label={props.name} type="number" step="any" value={v() as number} onChange={(s) => props.onChange(Number(s))} />;
  }
  if (typeof v() === "string") {
    return <TextField label={props.name} value={v() as string} onChange={props.onChange} />;
  }
  if (Array.isArray(v()) && (v() as unknown[]).every((e) => typeof e === "number")) {
    const arr = v() as number[];
    return <TextField label={props.name} value={arr.join(" ")} onChange={(s) => props.onChange(s.trim().split(/[\s,]+/).map(Number))} />;
  }
  if (v() && typeof v() === "object" && !Array.isArray(v())) {
    const obj = v() as Record<string, unknown>;
    return (
      <fieldset>
        <legend>{props.name}</legend>
        <For each={Object.entries(obj)}>
          {([sub, subValue]) => (
            <ParamField name={sub} value={subValue} onChange={(nv) => props.onChange({ ...obj, [sub]: nv })} />
          )}
        </For>
      </fieldset>
    );
  }
  return <TextField label={props.name} value={JSON.stringify(v() ?? null)} onChange={(s) => props.onChange(s)} />;
}
