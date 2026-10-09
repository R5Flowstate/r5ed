import { type JSX, For, Show, createSignal } from "solid-js";
import { setTool, state, type ToolId } from "../state/stores";
import { chordFor, type CommandId, type Keymap } from "../input/keymap";

interface ToolButton {
  id: ToolId;
  label: string;
  command: CommandId;
  /** The chord of the modal operator the tool mirrors, when it has one. */
  modal?: CommandId;
  icon: () => JSX.Element;
}

/** 24x24 stroke icons; `currentColor` so the active tint applies. */
const Icon = (props: { children: JSX.Element }): JSX.Element => (
  <svg viewBox="0 0 24 24" aria-hidden="true">
    {props.children}
  </svg>
);

const GROUPS: ToolButton[][] = [
  [
    {
      id: "select",
      label: "Select",
      command: "tool_select",
      icon: () => (
        <Icon>
          <path d="M6 3l12 8.5-5.2 1 3.2 6.2-2.4 1.2-3.2-6.2L6 17.5z" class="fill" />
        </Icon>
      ),
    },
  ],
  [
    {
      id: "move",
      label: "Move",
      command: "tool_move",
      modal: "grab",
      icon: () => (
        <Icon>
          <path d="M12 2v20M2 12h20M12 2l-3 3M12 2l3 3M12 22l-3-3M12 22l3-3M2 12l3-3M2 12l3 3M22 12l-3-3M22 12l-3 3" />
        </Icon>
      ),
    },
    {
      id: "rotate",
      label: "Rotate",
      command: "tool_rotate",
      modal: "rotate",
      icon: () => (
        <Icon>
          <path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3" />
          <path d="M18 2.5v4.5h-4.5" />
        </Icon>
      ),
    },
    {
      id: "scale",
      label: "Scale",
      command: "tool_scale",
      modal: "resize",
      icon: () => (
        <Icon>
          <rect x="3" y="10" width="11" height="11" />
          <path d="M11 13l9-9M14 4h6v6" />
        </Icon>
      ),
    },
  ],
  [
    {
      id: "brush",
      label: "Add Brush Box",
      command: "tool_brush",
      icon: () => (
        <Icon>
          <path d="M4 8l7-4 7 4v8l-7 4-7-4z" />
          <path d="M4 8l7 4 7-4M11 12v8" />
          <path d="M19 15v6M16 18h6" class="accent" />
        </Icon>
      ),
    },
    {
      id: "clip",
      label: "Clip",
      command: "tool_clip",
      icon: () => (
        <Icon>
          <rect x="4" y="6" width="16" height="13" />
          <path d="M2 21L22 3" class="accent" />
        </Icon>
      ),
    },
  ],
  [
    {
      id: "vertex",
      label: "Edit Vertices",
      command: "tool_vertex",
      icon: () => (
        <Icon>
          <rect x="5" y="5" width="14" height="14" class="dim" />
          <rect x="3" y="3" width="4" height="4" class="fill" />
          <rect x="17" y="3" width="4" height="4" />
          <rect x="3" y="17" width="4" height="4" />
          <rect x="17" y="17" width="4" height="4" />
        </Icon>
      ),
    },
    {
      id: "edge",
      label: "Edit Edges",
      command: "tool_edge",
      icon: () => (
        <Icon>
          <rect x="5" y="5" width="14" height="14" class="dim" />
          <path d="M5 5h14" class="thick" />
        </Icon>
      ),
    },
    {
      id: "face",
      label: "Edit Faces",
      command: "tool_face",
      icon: () => (
        <Icon>
          <rect x="5" y="5" width="14" height="14" class="fill-soft" />
        </Icon>
      ),
    },
  ],
  [
    {
      id: "terrain_sculpt",
      label: "Sculpt Terrain",
      command: "tool_sculpt",
      icon: () => (
        <Icon>
          <path d="M2 19c4 0 5-9 10-9s6 9 10 9" />
          <circle cx="12" cy="8" r="4" class="accent" />
        </Icon>
      ),
    },
    {
      id: "terrain_paint",
      label: "Paint Terrain",
      command: "tool_paint",
      icon: () => (
        <Icon>
          <path d="M14 3l7 7-7 7-4-4" />
          <path d="M10 13c-3 0-5 2-5 4s-2 3-3 3c3 1 8 0 8-4" class="fill-soft" />
        </Icon>
      ),
    },
    {
      id: "terrain_voxel",
      label: "Voxel Terrain",
      command: "tool_voxel",
      icon: () => (
        <Icon>
          <rect x="3" y="13" width="8" height="8" />
          <rect x="11" y="13" width="8" height="8" />
          <rect x="7" y="5" width="8" height="8" class="fill-soft" />
        </Icon>
      ),
    },
  ],
  [
    {
      id: "entity",
      label: "Place Entity",
      command: "tool_entity",
      icon: () => (
        <Icon>
          <path d="M12 22s-7-7.5-7-12a7 7 0 0 1 14 0c0 4.5-7 12-7 12z" />
          <circle cx="12" cy="10" r="2.5" class="fill" />
        </Icon>
      ),
    },
  ],
];

/** Blender's T panel: a vertical strip of tool icons at the left of the viewports. */
export function Toolbar(props: { keymap: Keymap }): JSX.Element {
  const [tip, setTip] = createSignal<{ tool: ToolButton; y: number } | null>(null);
  const keys = (t: ToolButton): string => {
    const own = chordFor(props.keymap, t.command);
    const modal = t.modal ? chordFor(props.keymap, t.modal) : "";
    return own || modal;
  };
  return (
    <nav class="toolbar" role="radiogroup" aria-label="Tools" aria-orientation="vertical">
      <For each={GROUPS}>
        {(group) => (
          <div class="tool-group">
            <For each={group}>
              {(t) => (
                <button
                  type="button"
                  role="radio"
                  aria-label={t.label}
                  aria-checked={state.tool.id === t.id}
                  classList={{ active: state.tool.id === t.id }}
                  onClick={() => setTool(t.id)}
                  onPointerEnter={(e) => setTip({ tool: t, y: e.currentTarget.offsetTop })}
                  onPointerLeave={() => setTip(null)}
                >
                  {t.icon()}
                </button>
              )}
            </For>
          </div>
        )}
      </For>
      <Show when={tip()}>
        {(t) => (
          <div class="tool-tip" style={{ top: `${t().y}px` }}>
            <strong>{t().tool.label}</strong>
            <Show when={keys(t().tool)}>
              <kbd>{keys(t().tool)}</kbd>
            </Show>
          </div>
        )}
      </Show>
    </nav>
  );
}
