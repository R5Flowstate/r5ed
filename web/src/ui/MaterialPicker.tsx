import { type JSX, For, Show, createMemo } from "solid-js";
import { DEV_MATERIALS } from "../doc/model";

export type MaterialGroup = "dev" | "project" | "current";

export interface MaterialOption {
  value: string;
  group: MaterialGroup;
}

const GROUP_LABELS: Record<MaterialGroup, string> = {
  dev: "Dev materials",
  project: "Map materials",
  current: "Other",
};

const DEV_SET = new Set(DEV_MATERIALS.map((m) => m.toLowerCase()));

/**
 * The picker's list: the shipped dev set first, in the order the maps name it,
 * then whatever else the document already uses, sorted, and finally the value
 * in hand so a hand-typed path is never lost.
 */
export function materialOptions(current: string, project: readonly string[]): MaterialOption[] {
  const out: MaterialOption[] = DEV_MATERIALS.map((value) => ({ value, group: "dev" as const }));
  const seen = new Set(DEV_SET);
  const rest = [...new Set(project.filter((m) => m.length > 0))]
    .filter((m) => !seen.has(m.toLowerCase()))
    .sort();
  for (const value of rest) {
    out.push({ value, group: "project" });
    seen.add(value.toLowerCase());
  }
  if (current.length > 0 && !seen.has(current.toLowerCase())) out.push({ value: current, group: "current" });
  return out;
}

/** The options of one group, in the order the picker shows them. */
export function groupOptions(options: readonly MaterialOption[], group: MaterialGroup): MaterialOption[] {
  return options.filter((o) => o.group === group);
}

export interface MaterialFieldProps {
  label: string;
  value: string;
  /** Material paths the document already uses. */
  project?: readonly string[];
  onChange: (value: string) => void;
}

/** A material picker that leads with the dev set and keeps the current value. */
export function MaterialField(props: MaterialFieldProps): JSX.Element {
  const id = () => `mat-${props.label.replace(/\W+/g, "-").toLowerCase()}`;
  const options = createMemo(() => materialOptions(props.value, props.project ?? []));
  const dev = createMemo(() => groupOptions(options(), "dev"));
  const project = createMemo(() => groupOptions(options(), "project"));
  const current = createMemo(() => groupOptions(options(), "current"));
  return (
    <div class="field material-field">
      <label class="field-name" for={id()}>
        {props.label}
      </label>
      <select
        id={id()}
        value={props.value}
        data-material-options={options().length}
        onChange={(e) => props.onChange(e.currentTarget.value)}
      >
        <Show when={current().length > 0}>
          <optgroup label={GROUP_LABELS.current}>
            <For each={current()}>{(o) => <option value={o.value}>{o.value}</option>}</For>
          </optgroup>
        </Show>
        <optgroup label={GROUP_LABELS.dev}>
          <For each={dev()}>{(o) => <option value={o.value}>{o.value}</option>}</For>
        </optgroup>
        <Show when={project().length > 0}>
          <optgroup label={GROUP_LABELS.project}>
            <For each={project()}>{(o) => <option value={o.value}>{o.value}</option>}</For>
          </optgroup>
        </Show>
      </select>
      <input
        class="material-path"
        type="text"
        value={props.value}
        aria-label={`${props.label} path`}
        placeholder="material path"
        onChange={(e) => {
          // Any material the install carries, not only the listed ones.
          const next = e.currentTarget.value.trim();
          if (next && next !== props.value) props.onChange(next);
        }}
      />
    </div>
  );
}
