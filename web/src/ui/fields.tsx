import { type JSX, For, Show, createMemo } from "solid-js";
import { type KeyDef, observedFlagBits } from "../doc/schema";

export interface FieldProps {
  def: KeyDef;
  value: string;
  onChange: (value: string) => void;
  /** Suggestions offered as a datalist, e.g. model paths already in the map. */
  suggestions?: readonly string[];
}

function attr(def: KeyDef, extra?: Record<string, string>): Record<string, string> {
  return { "data-key": def.name, "data-keytype": def.ty, ...extra };
}

/** Three number inputs bound to one string key, for vec3/angles/color. */
function Triple(
  props: { def: KeyDef; value: string; onChange: (v: string) => void; step: number | string; min?: number; max?: number; labels: [string, string, string] },
): JSX.Element {
  const parts = createMemo<[string, string, string]>(() => {
    const raw = props.value.split(/[\s,]+/).filter((p) => p.length > 0);
    return [raw[0] ?? "0", raw[1] ?? "0", raw[2] ?? "0"];
  });
  const write = (i: number, v: string): void => {
    const next = [...parts()] as [string, string, string];
    next[i] = v;
    props.onChange(next.join(" "));
  };
  return (
    <span class="triple">
      <For each={[0, 1, 2] as const}>
        {(i) => (
          <input
            class="triple-field"
            type="number"
            step={props.step}
            min={props.min}
            max={props.max}
            value={parts()[i]}
            aria-label={`${props.def.name} ${props.labels[i]}`}
            {...attr(props.def, { "data-component": props.labels[i] })}
            onChange={(e) => write(i, e.currentTarget.value)}
          />
        )}
      </For>
    </span>
  );
}

function Flags(props: FieldProps): JSX.Element {
  const bits = createMemo(() => observedFlagBits(props.def));
  const current = createMemo(() => {
    const n = Number(props.value);
    return Number.isFinite(n) ? n : 0;
  });
  const toggle = (value: number, on: boolean): void => {
    props.onChange(String(on ? current() | value : current() & ~value));
  };
  return (
    <span class="flag-list" {...attr(props.def)}>
      <Show when={bits().length > 0} fallback={<span class="hint">no bits observed</span>}>
        <For each={bits()}>
          {(bit) => (
            <label class="flag">
              <input
                type="checkbox"
                checked={(current() & bit.value) !== 0}
                data-flag={bit.value}
                onChange={(e) => toggle(bit.value, e.currentTarget.checked)}
              />
              <span>{bit.label}</span>
            </label>
          )}
        </For>
      </Show>
    </span>
  );
}

/** The widget for one schema key, chosen by its `KeyType`. */
export function KeyWidget(props: FieldProps): JSX.Element {
  const list = (): string => `${props.def.name}-values`;
  const datalist = (): JSX.Element => (
    <datalist id={list()}>
      <For each={[...props.def.observed_values, ...(props.suggestions ?? [])]}>
        {(v) => <option value={v} />}
      </For>
    </datalist>
  );
  switch (props.def.ty) {
    case "int":
      return (
        <input
          type="number"
          step={1}
          value={props.value}
          {...attr(props.def)}
          onChange={(e) => props.onChange(e.currentTarget.value)}
        />
      );
    case "float":
      return (
        <input
          type="number"
          step="any"
          value={props.value}
          {...attr(props.def)}
          onChange={(e) => props.onChange(e.currentTarget.value)}
        />
      );
    case "bool":
      return (
        <select {...attr(props.def)} value={props.value} onChange={(e) => props.onChange(e.currentTarget.value)}>
          <option value="">(unset)</option>
          <option value="0">0</option>
          <option value="1">1</option>
        </select>
      );
    case "vec3":
      return <Triple def={props.def} value={props.value} onChange={props.onChange} step="any" labels={["x", "y", "z"]} />;
    case "angles":
      return <Triple def={props.def} value={props.value} onChange={props.onChange} step="any" labels={["pitch", "yaw", "roll"]} />;
    case "color":
      return (
        <Triple def={props.def} value={props.value} onChange={props.onChange} step={1} min={0} max={255} labels={["r", "g", "b"]} />
      );
    case "enum": {
      const values = createMemo(() => {
        const set = new Set(props.def.observed_values);
        if (props.value) set.add(props.value);
        return [...set].sort();
      });
      return (
        <select {...attr(props.def)} value={props.value} onChange={(e) => props.onChange(e.currentTarget.value)}>
          <For each={values()}>{(v) => <option value={v}>{v}</option>}</For>
        </select>
      );
    }
    case "flags":
      return <Flags def={props.def} value={props.value} onChange={props.onChange} />;
    case "model":
    case "material":
    case "sound":
    case "target":
    case "string":
    default:
      return (
        <span class="with-list">
          <input
            type="text"
            list={list()}
            value={props.value}
            {...attr(props.def)}
            onChange={(e) => props.onChange(e.currentTarget.value)}
          />
          {datalist()}
        </span>
      );
  }
}

/** A labelled row: name, required marker, widget and any validation message. */
export function Field(props: {
  def: KeyDef;
  value: string;
  onChange: (value: string) => void;
  message?: string;
  suggestions?: readonly string[];
}): JSX.Element {
  return (
    <div class="field" classList={{ invalid: !!props.message }}>
      <label class="field-name" for={`k-${props.def.name}`}>
        <span>{props.def.name}</span>
        <Show when={props.def.required}>
          <em class="required" title="present on nearly every instance">*</em>
        </Show>
        <span class="type-chip">{props.def.ty}</span>
      </label>
      <KeyWidget def={props.def} value={props.value} onChange={props.onChange} suggestions={props.suggestions} />
      <Show when={props.message}>
        <p class="field-message" role="alert">
          {props.message}
        </p>
      </Show>
    </div>
  );
}

/** A plain labelled text/number input used by the non-entity inspectors. */
export function TextField(props: {
  label: string;
  value: string | number;
  type?: "text" | "number" | "checkbox";
  /**
   * Reports every keystroke. Off by default: a field that edits the document
   * commits once, on Enter or when focus leaves, so typing 128 is one undo
   * step and never a 1 then a 12 inch brush on the way.
   */
  live?: boolean;
  step?: string;
  min?: number;
  max?: number;
  onChange: (value: string) => void;
}): JSX.Element {
  const id = () => `f-${props.label.replace(/\W+/g, "-").toLowerCase()}`;
  return (
    <div class="field">
      <label class="field-name" for={id()}>
        {props.label}
      </label>
      <Show
        when={props.type !== "checkbox"}
        fallback={
            <input
              id={id()}
              type="checkbox"
              checked={props.value === 1 || props.value === "1"}
              onChange={(e) => props.onChange(e.currentTarget.checked ? "1" : "0")}
            />
        }
      >
        <input
          id={id()}
          type={props.type ?? "text"}
          step={props.step}
          min={props.min}
          max={props.max}
          value={String(props.value)}
          onInput={(e) => props.live && props.onChange(e.currentTarget.value)}
          onChange={(e) => !props.live && props.onChange(e.currentTarget.value)}
        />
      </Show>
    </div>
  );
}
