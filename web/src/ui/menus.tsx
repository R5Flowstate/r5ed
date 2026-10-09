import { type JSX, For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";

export interface MenuItem {
  label: string;
  /** The chord that runs the same thing, shown on the right. */
  hint?: string;
  run: () => void;
  /** A heading row that cannot be chosen. */
  heading?: boolean;
}

export type MenuSpec =
  | { type: "list"; title: string; x: number; y: number; items: MenuItem[] }
  | { type: "pie"; title: string; x: number; y: number; items: (MenuItem | null)[] }
  | { type: "search"; title: string; x: number; y: number; items: MenuItem[] }
  | { type: "prompt"; title: string; x: number; y: number; value: string; submit: (value: string) => void };

/** Closes on Escape or a press outside, the way Blender popups do. */
function useDismiss(close: () => void, root: () => HTMLElement | undefined): void {
  onMount(() => {
    const onDown = (e: PointerEvent): void => {
      const el = root();
      if (el && !el.contains(e.target as Node)) close();
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        close();
      }
    };
    document.addEventListener("pointerdown", onDown, true);
    document.addEventListener("keydown", onKey, true);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onDown, true);
      document.removeEventListener("keydown", onKey, true);
    });
  });
}

function clampTo(x: number, y: number, w: number, h: number): { left: string; top: string } {
  const vw = globalThis.innerWidth || 1280;
  const vh = globalThis.innerHeight || 800;
  return { left: `${Math.max(4, Math.min(x, vw - w - 4))}px`, top: `${Math.max(4, Math.min(y, vh - h - 4))}px` };
}

export function Menu(props: { spec: MenuSpec; onClose: () => void }): JSX.Element {
  return (
    <>
      <Show when={props.spec.type === "list"}>
        <ListMenu spec={props.spec as Extract<MenuSpec, { type: "list" }>} onClose={props.onClose} />
      </Show>
      <Show when={props.spec.type === "pie"}>
        <PieMenu spec={props.spec as Extract<MenuSpec, { type: "pie" }>} onClose={props.onClose} />
      </Show>
      <Show when={props.spec.type === "search"}>
        <SearchMenu spec={props.spec as Extract<MenuSpec, { type: "search" }>} onClose={props.onClose} />
      </Show>
      <Show when={props.spec.type === "prompt"}>
        <PromptMenu spec={props.spec as Extract<MenuSpec, { type: "prompt" }>} onClose={props.onClose} />
      </Show>
    </>
  );
}

function ListMenu(props: { spec: Extract<MenuSpec, { type: "list" }>; onClose: () => void }): JSX.Element {
  let root: HTMLDivElement | undefined;
  const choosable = createMemo(() => props.spec.items.filter((i) => !i.heading));
  const [index, setIndex] = createSignal(0);
  useDismiss(props.onClose, () => root);
  const choose = (item: MenuItem): void => {
    props.onClose();
    item.run();
  };
  onMount(() => {
    const onKey = (e: KeyboardEvent): void => {
      const items = choosable();
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        setIndex((i) => (i + (e.key === "ArrowDown" ? 1 : items.length - 1)) % Math.max(1, items.length));
      } else if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        const item = items[index()];
        if (item) choose(item);
      } else if (e.key.length === 1 && !e.ctrlKey && !e.altKey) {
        // A letter jumps to the first item starting with it, as Blender's underlined accelerators do.
        const hit = items.find((i) => i.label.toLowerCase().startsWith(e.key.toLowerCase()));
        if (hit) {
          e.preventDefault();
          e.stopPropagation();
          choose(hit);
        }
      }
    };
    document.addEventListener("keydown", onKey, true);
    onCleanup(() => document.removeEventListener("keydown", onKey, true));
  });
  return (
    <div ref={root} class="popup-menu" role="menu" style={clampTo(props.spec.x - 20, props.spec.y - 12, 240, props.spec.items.length * 24 + 32)}>
      <div class="popup-title">{props.spec.title}</div>
      <For each={props.spec.items}>
        {(item) => (
          <Show when={!item.heading} fallback={<div class="popup-heading">{item.label}</div>}>
            <button
              type="button"
              role="menuitem"
              classList={{ hot: choosable()[index()] === item }}
              onPointerEnter={() => setIndex(choosable().indexOf(item))}
              onClick={() => choose(item)}
            >
              <span>{item.label}</span>
              <Show when={item.hint}>
                <kbd>{item.hint}</kbd>
              </Show>
            </button>
          </Show>
        )}
      </For>
    </div>
  );
}

/** Slot order of a Blender pie: left, right, bottom, top, then the four diagonals. */
const PIE_ANGLES = [180, 0, 270, 90, 135, 45, 225, 315];
const PIE_RADIUS = 110;

function PieMenu(props: { spec: Extract<MenuSpec, { type: "pie" }>; onClose: () => void }): JSX.Element {
  let root: HTMLDivElement | undefined;
  const [hot, setHot] = createSignal(-1);
  useDismiss(props.onClose, () => root);
  const slots = createMemo(() =>
    props.spec.items.map((item, i) => {
      const a = (PIE_ANGLES[i] * Math.PI) / 180;
      return { item, i, x: Math.cos(a) * PIE_RADIUS, y: -Math.sin(a) * PIE_RADIUS * 0.75 };
    }),
  );
  const choose = (item: MenuItem | null): void => {
    if (!item) return;
    props.onClose();
    item.run();
  };
  onMount(() => {
    // The pointer's direction from the center picks a slot, so a flick and a click choose.
    const onMove = (e: PointerEvent): void => {
      const dx = e.clientX - props.spec.x;
      const dy = e.clientY - props.spec.y;
      if (Math.hypot(dx, dy) < 24) {
        setHot(-1);
        return;
      }
      const angle = (Math.atan2(-dy, dx) * 180) / Math.PI;
      let best = -1;
      let bestD = 360;
      slots().forEach((s) => {
        if (!s.item) return;
        const d = Math.abs(((angle - PIE_ANGLES[s.i] + 540) % 360) - 180);
        if (d < bestD) {
          bestD = d;
          best = s.i;
        }
      });
      setHot(best);
    };
    const onUp = (e: PointerEvent): void => {
      if (e.button !== 0) return;
      const i = hot();
      if (i >= 0) choose(props.spec.items[i]);
    };
    const onKey = (e: KeyboardEvent): void => {
      // Numpad digits pick a slot by direction, as Blender pies do.
      const dir: Record<string, number> = { Numpad4: 0, Numpad6: 1, Numpad2: 2, Numpad8: 3, Numpad7: 4, Numpad9: 5, Numpad1: 6, Numpad3: 7 };
      const i = dir[e.code];
      if (i !== undefined) {
        e.preventDefault();
        e.stopPropagation();
        choose(props.spec.items[i] ?? null);
      }
    };
    document.addEventListener("pointermove", onMove, true);
    document.addEventListener("pointerup", onUp, true);
    document.addEventListener("keydown", onKey, true);
    onCleanup(() => {
      document.removeEventListener("pointermove", onMove, true);
      document.removeEventListener("pointerup", onUp, true);
      document.removeEventListener("keydown", onKey, true);
    });
  });
  return (
    <div ref={root} class="pie-menu" style={{ left: `${props.spec.x}px`, top: `${props.spec.y}px` }}>
      <div class="pie-center">{props.spec.title}</div>
      <For each={slots()}>
        {(s) => (
          <Show when={s.item}>
            <button
              type="button"
              class="pie-item"
              classList={{ hot: hot() === s.i }}
              style={{ transform: `translate(calc(${s.x}px - 50%), calc(${s.y}px - 50%))` }}
              onClick={() => choose(s.item)}
            >
              {s.item!.label}
            </button>
          </Show>
        )}
      </For>
    </div>
  );
}

/** Scores a fuzzy match: every query character in order, earlier and denser is better. */
export function fuzzyScore(query: string, text: string): number {
  const q = query.toLowerCase().trim();
  if (!q) return 1;
  const t = text.toLowerCase();
  if (t.includes(q)) return 1000 - t.indexOf(q);
  let score = 0;
  let at = 0;
  for (const ch of q) {
    if (ch === " ") continue;
    const i = t.indexOf(ch, at);
    if (i < 0) return -1;
    score += i === at ? 10 : 1;
    at = i + 1;
  }
  return score;
}

function SearchMenu(props: { spec: Extract<MenuSpec, { type: "search" }>; onClose: () => void }): JSX.Element {
  let root: HTMLDivElement | undefined;
  let input: HTMLInputElement | undefined;
  const [query, setQuery] = createSignal("");
  const [index, setIndex] = createSignal(0);
  useDismiss(props.onClose, () => root);
  const results = createMemo(() =>
    props.spec.items
      .map((item) => ({ item, score: fuzzyScore(query(), item.label) }))
      .filter((r) => r.score >= 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, 14)
      .map((r) => r.item),
  );
  onMount(() => input?.focus());
  const choose = (item: MenuItem | undefined): void => {
    if (!item) return;
    props.onClose();
    item.run();
  };
  return (
    <div ref={root} class="popup-menu search-menu" style={clampTo(props.spec.x - 150, props.spec.y - 16, 320, 400)}>
      <input
        ref={input}
        type="search"
        placeholder={props.spec.title}
        value={query()}
        onInput={(e) => {
          setQuery(e.currentTarget.value);
          setIndex(0);
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setIndex((i) => Math.min(results().length - 1, i + 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setIndex((i) => Math.max(0, i - 1));
          } else if (e.key === "Enter") {
            e.preventDefault();
            choose(results()[index()]);
          }
        }}
      />
      <For each={results()}>
        {(item, i) => (
          <button type="button" classList={{ hot: i() === index() }} onPointerEnter={() => setIndex(i())} onClick={() => choose(item)}>
            <span>{item.label}</span>
            <Show when={item.hint}>
              <kbd>{item.hint}</kbd>
            </Show>
          </button>
        )}
      </For>
    </div>
  );
}

function PromptMenu(props: { spec: Extract<MenuSpec, { type: "prompt" }>; onClose: () => void }): JSX.Element {
  let root: HTMLDivElement | undefined;
  let input: HTMLInputElement | undefined;
  useDismiss(props.onClose, () => root);
  onMount(() => {
    input?.focus();
    input?.select();
  });
  return (
    <div ref={root} class="popup-menu" style={clampTo(props.spec.x - 120, props.spec.y - 16, 260, 64)}>
      <div class="popup-title">{props.spec.title}</div>
      <input
        ref={input}
        type="text"
        value={props.spec.value}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            const v = e.currentTarget.value;
            props.onClose();
            props.spec.submit(v);
          }
        }}
      />
    </div>
  );
}
