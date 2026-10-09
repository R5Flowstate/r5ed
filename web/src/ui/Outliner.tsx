import { type JSX, For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { DocObject, Layer } from "../doc/model";
import { state, select, setIsolate } from "../state/stores";
import { NEW_OBJECT_KINDS, type NewObjectKind } from "../state/editor";
import type { Editor } from "../state/editor";

export interface OutlinerProps {
  editor: Editor;
  objects: readonly DocObject[];
  layers: readonly Layer[];
  onSelect: (id: string, additive: boolean) => void;
  onLayer: (layer: Layer) => void;
  onCreate: (kind: NewObjectKind) => void;
  onAddLayer: () => void;
  onRemoveLayer: (id: string) => void;
  onRename: (object: DocObject, name: string) => void;
}

function kindOf(object: DocObject): string {
  return object.kind.type;
}

/** Height of one tree row in pixels; the list is laid out from it. */
const ROW_HEIGHT = 22;
/** Rows drawn above and below the visible window, so a scroll does not show gaps. */
const OVERSCAN = 12;

export interface OutlineRow {
  object: DocObject;
  depth: number;
  hasKids: boolean;
}

/**
 * The tree flattened to the rows it shows, top to bottom: a row whose object
 * or one of its children matches the filter, and the children of every row
 * that is not collapsed.
 */
export function outlineRows(
  roots: readonly DocObject[],
  childrenOf: (id: string) => readonly DocObject[],
  match: (o: DocObject) => boolean,
  collapsed: ReadonlySet<string>,
): OutlineRow[] {
  const out: OutlineRow[] = [];
  const stack: { object: DocObject; depth: number }[] = [];
  for (let i = roots.length - 1; i >= 0; i--) stack.push({ object: roots[i], depth: 0 });
  while (stack.length > 0) {
    const { object, depth } = stack.pop()!;
    const kids = childrenOf(object.id);
    if (!match(object) && !kids.some(match)) continue;
    out.push({ object, depth, hasKids: kids.length > 0 });
    if (collapsed.has(object.id)) continue;
    for (let i = kids.length - 1; i >= 0; i--) stack.push({ object: kids[i], depth: depth + 1 });
  }
  return out;
}

/** One tree row with a drop target for reparenting. */
function Row(props: {
  row: OutlineRow;
  open: boolean;
  selected: boolean;
  hovered: boolean;
  onToggle: (id: string) => void;
  onContextMenu: (id: string) => void;
  props: OutlinerProps;
}): JSX.Element {
  const [over, setOver] = createSignal(false);
  const object = (): DocObject => props.row.object;
  return (
    <li>
      <div
        class="tree-row"
        classList={{ selected: props.selected, hovered: props.hovered, drop: over() }}
        style={{ "padding-left": `${props.row.depth * 14 + 4}px`, height: `${ROW_HEIGHT}px` }}
        data-object-id={object().id}
        data-kind={kindOf(object())}
        draggable={true}
        onDragStart={(e) => e.dataTransfer?.setData("text/r5ed-object", object().id)}
        onDragOver={(e) => {
          e.preventDefault();
          setOver(true);
        }}
        onDragLeave={() => setOver(false)}
        onDrop={(e) => {
          e.preventDefault();
          setOver(false);
          const dragged = e.dataTransfer?.getData("text/r5ed-object");
          if (dragged && dragged !== object().id) props.props.editor.reparent(dragged, object().id);
        }}
        onClick={(e) => props.props.onSelect(object().id, e.shiftKey || e.ctrlKey)}
        onDblClick={() => props.onToggle(object().id)}
        onContextMenu={(e) => {
          e.preventDefault();
          props.props.onSelect(object().id, false);
          props.onContextMenu(object().id);
        }}
      >
        <button
          type="button"
          class="twisty"
          aria-label={props.open ? "Collapse" : "Expand"}
          onClick={(e) => {
            e.stopPropagation();
            props.onToggle(object().id);
          }}
        >
          {props.row.hasKids ? (props.open ? "-" : "+") : " "}
        </button>
        <input
          class="name"
          value={object().name}
          aria-label={`Name of ${object().name}`}
          onChange={(e) => props.props.onRename(object(), e.currentTarget.value)}
        />
        <span class="kind-chip">{kindOf(object())}</span>
        <Show when={object().locked}>
          <span class="lock" title="locked">L</span>
        </Show>
      </div>
    </li>
  );
}

/** Layers, the hierarchy tree, search and the per-object context menu. */
export function Outliner(props: OutlinerProps): JSX.Element {
  const [query, setQuery] = createSignal("");
  const [menu, setMenu] = createSignal<string | null>(null);
  const [createOpen, setCreateOpen] = createSignal(false);
  const match = createMemo(() => {
    const q = query().trim().toLowerCase();
    if (!q) return () => true;
    return (o: DocObject) => o.name.toLowerCase().includes(q) || o.kind.type.includes(q);
  });
  const byParent = createMemo(() => {
    const map = new Map<string | null, DocObject[]>();
    for (const o of props.objects) {
      const key = o.parent ?? null;
      const list = map.get(key);
      if (list) list.push(o);
      else map.set(key, [o]);
    }
    return map;
  });
  const roots = createMemo(() => {
    const all = byParent().get(null) ?? [];
    const isolated = state.selection.isolate ? new Set(state.selection.isolated) : null;
    if (!isolated) return all;
    return all.filter((o) => isolated.has(o.id) || descendants(o.id, byParent()).some((id) => isolated.has(id)));
  });
  const childrenOf = (id: string): DocObject[] => byParent().get(id) ?? [];
  const layerCounts = createMemo(() => {
    const out = new Map<string, number>();
    for (const o of props.objects) out.set(o.layer, (out.get(o.layer) ?? 0) + 1);
    return out;
  });
  const [collapsed, setCollapsed] = createSignal<ReadonlySet<string>>(new Set());
  const toggle = (id: string): void => {
    const next = new Set(collapsed());
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setCollapsed(next);
  };
  const rows = createMemo(() => outlineRows(roots(), childrenOf, match(), collapsed()));
  // A map holds tens of thousands of objects; only the rows in view exist in
  // the page, and selection is read through one set rather than per row.
  const selectedIds = createMemo(() => new Set(state.selection.ids));
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewHeight, setViewHeight] = createSignal(600);
  let list: HTMLUListElement | undefined;
  const windowed = createMemo(() => {
    const all = rows();
    const first = Math.max(0, Math.floor(scrollTop() / ROW_HEIGHT) - OVERSCAN);
    const last = Math.min(all.length, Math.ceil((scrollTop() + viewHeight()) / ROW_HEIGHT) + OVERSCAN);
    return { first, rows: all.slice(first, last), total: all.length };
  });
  onMount(() => {
    if (!list) return;
    const el = list;
    const measure = (): void => {
      setViewHeight(el.clientHeight || 600);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    onCleanup(() => observer.disconnect());
  });
  // A pick in the viewport brings its row into view.
  createEffect(() => {
    const id = state.selection.primary;
    if (!id || !list) return;
    const at = rows().findIndex((r) => r.object.id === id);
    if (at < 0) return;
    const top = at * ROW_HEIGHT;
    if (top < list.scrollTop || top + ROW_HEIGHT > list.scrollTop + list.clientHeight) {
      list.scrollTop = Math.max(0, top - list.clientHeight / 2);
    }
  });

  return (
    <aside tabIndex={-1} class="panel outliner" aria-label="Outliner">
      <header class="panel-head">
        <h2>Outliner</h2>
        <button type="button" onClick={() => setCreateOpen(!createOpen())} aria-expanded={createOpen()} title="Add an object">
          + object
        </button>
        <button type="button" onClick={props.onAddLayer} title="Add layer">
          + layer
        </button>
      </header>
      <Show when={createOpen()}>
        <div class="create-menu" role="menu" aria-label="Add an object">
          <For each={NEW_OBJECT_KINDS}>
            {(kind) => (
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  props.onCreate(kind.id);
                  setCreateOpen(false);
                }}
              >
                {kind.label}
              </button>
            )}
          </For>
        </div>
      </Show>
      <div class="layers">
        <For each={props.layers}>
          {(layer) => (
            <div class="layer-row" data-layer-id={layer.id}>
              <label class="vis">
                <input
                  type="checkbox"
                  checked={layer.visible}
                  aria-label={`Visible: ${layer.name}`}
                  onChange={(e) => props.onLayer({ ...layer, visible: e.currentTarget.checked })}
                />
              </label>
              <label class="vis">
                <input
                  type="checkbox"
                  checked={layer.locked}
                  aria-label={`Locked: ${layer.name}`}
                  onChange={(e) => props.onLayer({ ...layer, locked: e.currentTarget.checked })}
                />
              </label>
              <span class="layer-name">{layer.name}</span>
              <span class="count">{layerCounts().get(layer.id) ?? 0}</span>
              <button type="button" aria-label={`Remove layer ${layer.name}`} onClick={() => props.onRemoveLayer(layer.id)}>
                x
              </button>
            </div>
          )}
        </For>
      </div>
      <input
        class="search"
        type="search"
        placeholder="Filter objects"
        aria-label="Filter objects"
        value={query()}
        onInput={(e) => setQuery(e.currentTarget.value)}
      />
      <ul class="tree" role="tree" aria-label="Objects" ref={list} onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}>
        <li aria-hidden="true" style={{ height: `${windowed().first * ROW_HEIGHT}px` }} />
        <For each={windowed().rows}>
          {(row) => (
            <Row
              row={row}
              open={!collapsed().has(row.object.id)}
              selected={selectedIds().has(row.object.id)}
              hovered={state.selection.hover === row.object.id}
              onToggle={toggle}
              onContextMenu={setMenu}
              props={props}
            />
          )}
        </For>
        <li aria-hidden="true" style={{ height: `${(windowed().total - windowed().first - windowed().rows.length) * ROW_HEIGHT}px` }} />
      </ul>
      <Show when={state.selection.isolate}>
        <div class="selection-bar isolate-bar" role="status">
          <span>isolated: {state.selection.isolated.length} objects</span>
          <button type="button" onClick={() => setIsolate(false)}>
            Show all
          </button>
        </div>
      </Show>
      <Show when={state.selection.ids.length > 0}>
        <div class="selection-bar">
          <span>{state.selection.ids.length} selected</span>
          <button type="button" onClick={() => props.editor.group(state.selection.ids)}>
            Group
          </button>
          <button type="button" onClick={() => props.editor.ungroup(state.selection.ids)}>
            Ungroup
          </button>
          <button type="button" onClick={() => setIsolate(true, [...state.selection.ids])}>
            Isolate
          </button>
        </div>
      </Show>
      <Show when={menu()}>
        {(id) => (
          <ContextMenu
            id={id()}
            editor={props.editor}
            onClose={() => setMenu(null)}
          />
        )}
      </Show>
    </aside>
  );
}

function ContextMenu(props: { id: string; editor: Editor; onClose: () => void }): JSX.Element {
  const selected = createMemo(() =>
    state.selection.ids.includes(props.id) ? [...state.selection.ids] : [props.id],
  );
  const kind = (): string | null => props.editor.store?.object(props.id)?.kind.type ?? null;
  const meshSource = (): string | null => {
    const o = props.editor.store?.object(props.id);
    return o && o.kind.type === "mesh" ? o.kind.mesh.source : null;
  };
  const convertBrush = (): void => {
    try {
      const touched = props.editor.brushToMesh(props.id);
      if (touched.length > 0) select(touched);
      props.editor.note("converted to an editable mesh");
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
    props.onClose();
  };
  return (
    <div class="context-menu" role="menu">
      <button type="button" role="menuitem" onClick={() => { props.editor.duplicate(selected()); props.onClose(); }}>
        Duplicate
      </button>
      <Show when={kind() === "brush"}>
        <button type="button" role="menuitem" onClick={convertBrush}>
          Convert to Mesh
        </button>
      </Show>
      <Show when={meshSource() !== null && !meshSource()?.endsWith(".r5mesh")}>
        <button
          type="button"
          role="menuitem"
          onClick={() => {
            try {
              props.editor.meshMakeEditable(props.id);
              props.editor.note("made editable");
            } catch (e) {
              props.editor.note(e instanceof Error ? e.message : String(e));
            }
            props.onClose();
          }}
        >
          Make Editable
        </button>
      </Show>
      <Show when={meshSource()?.endsWith(".r5mesh")}>
        <button
          type="button"
          role="menuitem"
          onClick={() => {
            select([props.id]);
            props.editor.toggleMeshEdit();
            props.onClose();
          }}
        >
          Edit mesh (Tab)
        </button>
      </Show>
      <button type="button" role="menuitem" onClick={() => { props.editor.group(selected()); props.onClose(); }}>
        Group
      </button>
      <button type="button" role="menuitem" onClick={() => { props.editor.savePrefab(selected()); props.onClose(); }}>
        Save prefab
      </button>
      <button type="button" role="menuitem" onClick={() => { setIsolate(true, selected()); props.onClose(); }}>
        Isolate
      </button>
      <button type="button" role="menuitem" onClick={() => { props.editor.removeObjects(selected()); props.onClose(); }}>
        Delete
      </button>
    </div>
  );
}

function descendants(id: string, map: Map<string | null, DocObject[]>): string[] {
  const out: string[] = [];
  const stack = [...(map.get(id) ?? []).map((c) => c.id)];
  while (stack.length) {
    const next = stack.pop() as string;
    out.push(next);
    for (const c of map.get(next) ?? []) stack.push(c.id);
  }
  return out;
}
