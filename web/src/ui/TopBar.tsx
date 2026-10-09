import { type JSX, For, Show, createMemo } from "solid-js";
import type { JobKind } from "../host/client";
import {
  openDialog,
  setBrush,
  setLayout,
  setSnap,
  setState,
  setUi,
  setViewportKind,
  state,
  toggleTheme,
} from "../state/stores";
import type { Editor } from "../state/editor";
import { MaterialField } from "./MaterialPicker";
import { chordFor, type CommandId } from "../input/keymap";
import { GRID_SIZES, type GridSize } from "../render/grid";

const BUILD_JOBS: { kind: JobKind; label: string }[] = [
  { kind: "compile", label: "Compile" },
  { kind: "bake", label: "Bake" },
  { kind: "package", label: "Package" },
  { kind: "deploy", label: "Deploy" },
  { kind: "reload", label: "Reload" },
];

export interface TopBarProps {
  editor: Editor;
  onCommand: (id: CommandId) => void;
}

/** Project menu, undo/redo, tool palette, view layout and the build buttons. */
export function TopBar(props: TopBarProps): JSX.Element {
  // The menu is the "project" dialog in the store; Escape closes it like
  // any other dialog.
  const menuOpen = (): boolean => state.ui.dialog === "project";
  const setMenuOpen = (open: boolean): void => openDialog(open ? "project" : null);
  const undo = createMemo(() => state.canUndo);
  const redo = createMemo(() => state.canRedo);
  const keyOf = (id: CommandId): string => chordFor(props.editor.keymap, id);
  return (
    <header class="top-bar" role="toolbar" aria-label="Main toolbar">
      <div class="group project-group">
        <button type="button" aria-haspopup="menu" aria-expanded={menuOpen()} onClick={() => setMenuOpen(!menuOpen())}>
          Project
        </button>
        <Show when={menuOpen()}>
          <div class="menu" role="menu">
            <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); props.onCommand("new"); }}>
              New <kbd>{keyOf("new")}</kbd>
            </button>
            <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); props.onCommand("open"); }}>
              Open <kbd>{keyOf("open")}</kbd>
            </button>
            <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); props.onCommand("save"); }}>
              Save <kbd>{keyOf("save")}</kbd>
            </button>
            <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); props.onCommand("save_as"); }}>
              Save as <kbd>{keyOf("save_as")}</kbd>
            </button>
            <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); props.onCommand("import"); }}>
              Import <kbd>{keyOf("import")}</kbd>
            </button>
            <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); props.onCommand("map_settings"); }}>
              Map settings <kbd>{keyOf("map_settings")}</kbd>
            </button>
            <button type="button" role="menuitem" onClick={() => { setMenuOpen(false); props.onCommand("new_terrain"); }}>
              New terrain <kbd>{keyOf("new_terrain")}</kbd>
            </button>
          </div>
        </Show>
        <span class="project-name">{state.project || "no project"}</span>
        <span class="stem">{state.stem}</span>
        <span class="dirty" aria-label={state.dirty ? "unsaved changes" : "saved"}>
          {state.dirty ? "*" : ""}
        </span>
      </div>
      <div class="group">
        <button type="button" disabled={!undo()} onClick={() => props.editor.undo()} title={`Undo ${keyOf("undo")}`}>
          Undo
        </button>
        <button type="button" disabled={!redo()} onClick={() => props.editor.redo()} title={`Redo ${keyOf("redo")}`}>
          Redo
        </button>
      </div>
      <div class="group">
        <label class="inline">
          <span>Snap</span>
          <input
            type="checkbox"
            checked={state.snap.enabled}
            aria-label="Toggle snapping"
            onChange={(e) => setSnap({ enabled: e.currentTarget.checked })}
          />
        </label>
        <label class="inline">
          <span>Grid</span>
          <select
            value={String(state.snap.size)}
            aria-label="Grid size"
            onChange={(e) => setSnap({ size: Number(e.currentTarget.value) })}
          >
            <For each={GRID_SIZES}>{(g: GridSize) => <option value={String(g)}>{g}</option>}</For>
          </select>
        </label>
        <button
          type="button"
          classList={{ active: state.gizmoSpace === "local" }}
          onClick={() => setState("gizmoSpace", (s) => (s === "world" ? "local" : "world"))}
        >
          {state.gizmoSpace}
        </button>
      </div>
      <Show when={state.tool.id === "brush"}>
        <div class="group brush-options" aria-label="Brush options">
          <label class="inline">
            <span>Height</span>
            <input
              type="number"
              step="16"
              min="1"
              value={state.tool.brush.height}
              aria-label="Brush height"
              onChange={(e) => setBrush({ height: Number(e.currentTarget.value) })}
            />
          </label>
          <label class="inline">
            <span>W</span>
            <input
              type="number"
              step="16"
              min="1"
              value={state.tool.brush.width}
              aria-label="Brush width"
              onChange={(e) => setBrush({ width: Math.max(1, Math.round(Number(e.currentTarget.value) || 1)) })}
            />
          </label>
          <label class="inline">
            <span>D</span>
            <input
              type="number"
              step="16"
              min="1"
              value={state.tool.brush.depth}
              aria-label="Brush depth"
              onChange={(e) => setBrush({ depth: Math.max(1, Math.round(Number(e.currentTarget.value) || 1)) })}
            />
          </label>
          <MaterialField
            label="Material"
            value={state.tool.brush.material}
            project={props.editor.projectMaterials()}
            onChange={(v) => setBrush({ material: v })}
          />
        </div>
      </Show>
      <div class="group">
        <button type="button" classList={{ active: state.ui.layout === "one" }} onClick={() => setLayout("one")}>
          1 pane
        </button>
        <button type="button" classList={{ active: state.ui.layout === "four" }} onClick={() => setLayout("four")}>
          4 panes
        </button>
        <For each={state.ui.viewports}>
          {(view, i) => (
            <select
              value={view.kind}
              aria-label={`Viewport ${i() + 1} kind`}
              onChange={(e) => setViewportKind(i(), e.currentTarget.value as typeof view.kind)}
            >
              <option value="persp">persp</option>
              <option value="top">top</option>
              <option value="front">front</option>
              <option value="side">side</option>
            </select>
          )}
        </For>
        <label class="inline">
          <span>Grid</span>
          <input
            type="checkbox"
            checked={state.ui.showGrid}
            aria-label="Toggle grid"
            onChange={(e) => setUi({ showGrid: e.currentTarget.checked })}
          />
        </label>
        <label class="inline">
          <span>Gizmo</span>
          <input
            type="checkbox"
            checked={state.ui.showGizmo}
            aria-label="Toggle gizmo"
            onChange={(e) => setUi({ showGizmo: e.currentTarget.checked })}
          />
        </label>
        <label class="inline">
          <span>Lightmaps</span>
          <input
            type="checkbox"
            checked={state.ui.showLightmaps}
            aria-label="Toggle lightmap preview"
            onChange={(e) => setUi({ showLightmaps: e.currentTarget.checked })}
          />
        </label>
        <label class="inline">
          <span>Volumes</span>
          <input
            type="checkbox"
            checked={state.ui.showVolumes}
            aria-label="Toggle volumes"
            onChange={(e) => setUi({ showVolumes: e.currentTarget.checked })}
          />
        </label>
        <label class="inline">
          <span>Billboards</span>
          <input
            type="checkbox"
            checked={state.ui.showEntityBillboards}
            aria-label="Toggle entity billboards"
            onChange={(e) => setUi({ showEntityBillboards: e.currentTarget.checked })}
          />
        </label>
        <label class="inline">
          <span>Light gizmos</span>
          <input
            type="checkbox"
            checked={state.ui.showLightGizmos}
            aria-label="Toggle light gizmos"
            onChange={(e) => setUi({ showLightGizmos: e.currentTarget.checked })}
          />
        </label>
      </div>
      <div class="group build">
        <For each={BUILD_JOBS}>
          {(job) => (
            <button
              type="button"
              onClick={() => {
                void props.editor
                  .startJob(job.kind, { stem: state.stem })
                  .catch((e: unknown) => props.editor.note(e instanceof Error ? e.message : String(e)));
              }}
            >
              {job.label}
            </button>
          )}
        </For>
        <button type="button" onClick={() => setState("jobs", "panelOpen", !state.jobs.panelOpen)}>
          Jobs
        </button>
      </div>
      <div class="group right">
        <button
          type="button"
          classList={{ active: state.live.enabled }}
          onClick={() => props.onCommand("live_toggle")}
        >
          Live {state.live.status}
        </button>
        <button type="button" onClick={toggleTheme} aria-label="Toggle theme">
          {state.ui.theme}
        </button>
        <button type="button" onClick={() => props.onCommand("shortcuts")} aria-label="Keyboard shortcuts">
          ?
        </button>
      </div>
    </header>
  );
}
