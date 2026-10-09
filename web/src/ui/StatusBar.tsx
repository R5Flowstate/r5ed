import { type JSX, For, Show, createMemo } from "solid-js";
import { state } from "../state/stores";
import type { Editor } from "../state/editor";
import { chordFor, type CommandId } from "../input/keymap";

/** What the mouse and a few keys do over a viewport, shown when no operator runs. */
const VIEW_HINTS: [string, string][] = [
  ["LMB", "Select"],
  ["MMB", "Rotate view"],
  ["Shift MMB", "Pan"],
  ["Wheel", "Zoom"],
  ["Shift RMB", "3D cursor"],
];

const VIEW_COMMAND_HINTS: [CommandId, string][] = [
  ["add_menu", "Add"],
  ["grab", "Move"],
  ["rotate", "Rotate"],
  ["resize", "Scale"],
  ["search", "Search"],
];

/** Key hints, cursor, grid, selection count, frame rate and messages. */
export function StatusBar(props: { editor: Editor }): JSX.Element {
  const cursor = createMemo(() => state.cursorWorld);
  const hints = createMemo((): [string, string][] => {
    if (state.modal) return state.modal.hints;
    if (state.ui.hoverView === null) return [];
    const keyed = VIEW_COMMAND_HINTS.map(([id, label]): [string, string] => [chordFor(props.editor.keymap, id), label]).filter(([k]) => k);
    return [...VIEW_HINTS, ...keyed];
  });
  return (
    <footer class="status-bar" role="status" aria-live="polite">
      <span class="cell grow status-hints" data-cell="hints">
        <Show when={state.modal}>
          {(m) => (
            <>
              <span class="modal-name">{m().name}</span>
              <span class="modal-header">{m().header}</span>
            </>
          )}
        </Show>
        <For each={hints()}>
          {([key, label]) => (
            <span>
              <kbd>{key}</kbd>
              {label}
            </span>
          )}
        </For>
      </span>
      <span class="cell message" data-cell="message">
        {state.ui.status}
      </span>
      <span class="cell" data-cell="cursor" title="3D cursor">
        {cursor() ? cursor()!.map((n) => n.toFixed(1)).join(" ") : "0 0 0"}
      </span>
      <span class="cell" data-cell="grid">
        grid {state.snap.size}
        {state.snap.enabled ? "" : " (off)"}
      </span>
      <span class="cell" data-cell="pivot">
        pivot {state.pivot}
      </span>
      <span class="cell" data-cell="selection">
        {state.selection.ids.length} selected
      </span>
      <span class="cell" data-cell="fps">
        {state.fps} fps
      </span>
      <span class="cell" data-cell="objects">
        {(void state.docRevision, props.editor.store?.ids().length ?? 0)} objects
      </span>
      <span class="cell" data-cell="live">
        live {state.live.status}
        <Show when={state.live.detail}> {state.live.detail}</Show>
      </span>
      <span class="cell" data-cell="material-warnings">
        {(void state.assets.revision, props.editor.materialWarnings.length)} material warnings
      </span>
    </footer>
  );
}

/** Message list for problems the user should be able to re-read. */
export function MessageLog(props: { editor: Editor }): JSX.Element {
  return (
    <ul class="message-log" aria-label="Messages">
      <For each={(void state.assets.revision, props.editor.messages.slice(0, 12))}>{(m) => <li>{m}</li>}</For>
    </ul>
  );
}
