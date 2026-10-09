import { type JSX, For, Show, createMemo, createSignal } from "solid-js";
import type { DocObject, Entity } from "../doc/model";
import { type ClassDef, type EntitySchema, orderedKeys, searchClasses } from "../doc/schema";
import { Field, TextField } from "./fields";
import { commandJson, replaceObjects } from "../doc/command";

export interface EntityFormProps {
  schema: EntitySchema;
  /** The object being edited; null renders the class picker on its own. */
  object: DocObject | null;
  issues: { id: string | null; message: string }[];
  /** Model and material paths already used in the map, offered as options. */
  suggestions: Record<string, string[]>;
  /** Every object an OnX output can point at. */
  targets: { id: string; name: string }[];
  /** Creates a new entity of the picked class; null when nothing is selected. */
  onCreate: ((classname: string) => void) | null;
  onChange: (next: DocObject) => void;
}

function entityOf(object: DocObject): Entity | null {
  return object.kind.type === "entity" ? object.kind.entity : null;
}

/** The command that writes an edited entity back into the document. */
export function entityCommand(object: DocObject): string {
  return commandJson(replaceObjects([object]));
}

/** Replaces the entity's keyvalues, leaving every other field alone. */
export function withKeyvalues(object: DocObject, keys: Record<string, string>): DocObject {
  const entity = entityOf(object);
  if (!entity) return object;
  const next: Record<string, string> = {};
  for (const [k, v] of Object.entries(keys)) {
    if (v === "") continue;
    next[k] = v;
  }
  return { ...object, kind: { type: "entity", entity: { ...entity, keyvalues: next } } };
}

/** Replaces the entity's OnX links, resolved to targetnames at compile time. */
export function withLinks(object: DocObject, links: Record<string, string>): DocObject {
  const entity = entityOf(object);
  if (!entity) return object;
  const next: Record<string, string> = {};
  for (const [k, v] of Object.entries(links)) {
    if (v === "") continue;
    next[k] = v;
  }
  return { ...object, kind: { type: "entity", entity: { ...entity, links: next } } };
}

function issueFor(props: EntityFormProps, key: string): string | undefined {
  if (!props.object) return undefined;
  return props.issues.find((i) => i.id === props.object?.id && i.message.includes(key))?.message;
}

/**
 * The class picker. It searches every class in the schema, not just the ones
 * the map already uses, so a factory-only class is still reachable.
 */
export function ClassPicker(props: {
  schema: EntitySchema;
  onPick: (classname: string) => void;
}): JSX.Element {
  const [query, setQuery] = createSignal("");
  const results = createMemo(() => searchClasses(props.schema, query(), 60));
  return (
    <div class="class-picker">
      <TextField label="Class" live value={query()} onChange={setQuery} />
      <ul class="class-list" role="listbox" aria-label="Entity classes">
        <For each={results()}>
          {(def) => (
            <li>
              <button type="button" role="option" aria-selected="false" onClick={() => props.onPick(def.classname)}>
                <span class="classname">{def.classname}</span>
                <span class="class-meta">
                  {def.cpp_class || "no datamap"} | {Object.keys(def.keys).length} keys | {def.example_count} examples
                </span>
              </button>
            </li>
          )}
        </For>
      </ul>
    </div>
  );
}

/**
 * The schema-driven form. One widget per key, chosen by its `KeyType`, with
 * observed values offered as suggestions and validation messages from the
 * core shown under the offending key.
 */
export function EntityForm(props: EntityFormProps): JSX.Element {
  const entity = createMemo(() => (props.object ? entityOf(props.object) : null));
  const def = createMemo<ClassDef | null>(() => {
    const name = entity()?.classname ?? "";
    return props.schema.classes[name] ?? null;
  });
  const keys = createMemo<Record<string, string>>(() => ({ ...(entity()?.keyvalues ?? {}) }));
  const links = createMemo<Record<string, string>>(() => ({ ...(entity()?.links ?? {}) }));
  const setKey = (name: string, value: string): void => {
    if (!props.object) return;
    props.onChange(withKeyvalues(props.object, { ...keys(), [name]: value }));
  };
  const setLink = (name: string, value: string): void => {
    if (!props.object) return;
    props.onChange(withLinks(props.object, { ...links(), [name]: value }));
  };
  const objectIssues = createMemo(() => props.issues.filter((i) => i.id === props.object?.id));
  const outputs = createMemo(() => {
    const listed = def()?.outputs ?? [];
    const stored = Object.keys(links());
    return [...new Set([...listed, ...stored])].sort();
  });

  return (
    <Show
      when={props.object}
      fallback={
        <div class="entity-create">
          <h4>Add an entity</h4>
          <ClassPicker schema={props.schema} onPick={(classname) => props.onCreate?.(classname)} />
        </div>
      }
    >
      {(object) => (
        <div class="entity-form">
          <header class="entity-head">
            <h3>{entity()?.classname ?? "entity"}</h3>
            <Show when={def()}>
              {(d) => (
                <p class="entity-meta">
                  {d().cpp_class} | kind {d().kind} | {objectIssues().length} issues
                </p>
              )}
            </Show>
            <Show when={!def()}>
              <p class="hint">this class is not in the schema; keys are free text</p>
            </Show>
          </header>
          <div class="fields">
            <Show when={def()} fallback={<FreeKeys keys={keys()} onChange={setKey} />}>
              {(d) => (
                <For each={orderedKeys(d())}>
                  {(keyDef) => (
                    <Field
                      def={keyDef}
                      value={keys()[keyDef.name] ?? keyDef.default ?? ""}
                      message={issueFor(props, keyDef.name)}
                      suggestions={props.suggestions[keyDef.ty] ?? []}
                      onChange={(v) => setKey(keyDef.name, v)}
                    />
                  )}
                </For>
              )}
            </Show>
          </div>
          <Show when={outputs().length > 0}>
            <div class="entity-links">
              <h4>Outputs</h4>
              <p class="hint">each OnX output points at another object, resolved to its targetname</p>
              <For each={outputs()}>
                {(output) => (
                  <div class="field">
                    <label class="field-name" for={`link-${output}`}>{output}</label>
                    <select
                      id={`link-${output}`}
                      value={links()[output] ?? ""}
                      onChange={(e) => setLink(output, e.currentTarget.value)}
                    >
                      <option value="">(none)</option>
                      <For each={props.targets}>
                        {(target) => (
                          <option value={target.id}>
                            {target.name} ({target.id.slice(0, 8)})
                          </option>
                        )}
                      </For>
                    </select>
                  </div>
                )}
              </For>
            </div>
          </Show>
          <Show when={objectIssues().length > 0}>
            <ul class="issues" aria-label="Validation">
              <For each={objectIssues()}>{(issue) => <li>{issue.message}</li>}</For>
            </ul>
          </Show>
          <span class="hidden-probe" data-object-id={object().id} data-classname={entity()?.classname ?? ""} />
        </div>
      )}
    </Show>
  );
}

/** Key/value rows for a class the schema does not describe. */
function FreeKeys(props: {
  keys: Record<string, string>;
  onChange: (name: string, value: string) => void;
}): JSX.Element {
  const [name, setName] = createSignal("");
  const names = createMemo(() => Object.keys(props.keys).sort());
  return (
    <div class="free-keys">
      <For each={names()}>
        {(key) => (
          <Field
            def={{ name: key, ty: "string", observed_values: [], default: null, required: false, sources: [] }}
            value={props.keys[key]}
            onChange={(v) => props.onChange(key, v)}
          />
        )}
      </For>
      <div class="add-key">
        <TextField label="New key" value={name()} onChange={setName} />
        <button
          type="button"
          disabled={!name().trim()}
          onClick={() => {
            if (name().trim()) props.onChange(name().trim(), "");
            setName("");
          }}
        >
          Add
        </button>
      </div>
    </div>
  );
}
