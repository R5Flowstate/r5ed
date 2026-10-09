import { type JSX, For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import type { AssetEntry, Editor } from "../state/editor";
import type { JobStatus } from "../host/client";
import { setState, state } from "../state/stores";
import { PngError, decodePng } from "../render/png";

// Sounds ship in audio banks, not in rpaks, so the asset index never holds one.
const KINDS = ["model", "material", "texture", "map"];
const THUMB_PX = 64;

export interface AssetBrowserProps {
  editor: Editor;
  onPlace: (asset: AssetEntry) => void;
}

/** Host-backed asset search with a thumbnail grid and drag-out. */
export function AssetBrowser(props: AssetBrowserProps): JSX.Element {
  const [query, setQuery] = createSignal("");
  const [kind, setKind] = createSignal("model");
  const [busy, setBusy] = createSignal(false);
  const items = createMemo(() => (void state.assets.revision, props.editor.assetResults));
  const warnings = createMemo(() => (void state.assets.revision, props.editor.materialWarnings));
  const total = createMemo(() => (void state.assets.revision, props.editor.assetTotal));
  const [thumbs, setThumbs] = createSignal<Record<string, string>>({});
  const run = (): void => {
    setBusy(true);
    void props.editor
      .searchAssets(query(), kind())
      .catch((e: unknown) => props.editor.note(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  };
  // Every result gets a real thumbnail when the host has one; a missing or
  // malformed image just leaves the kind chip in place.
  createEffect(() => {
    const wanted = items();
    if (wanted.length === 0) return;
    let cancelled = false;
    const urls: string[] = [];
    void (async () => {
      const found: Record<string, string> = {};
      for (const asset of wanted) {
        if (cancelled) break;
        const url = await thumbnailUrl(props.editor, asset.path);
        if (url) {
          found[asset.path] = url;
          urls.push(url);
        }
      }
      if (!cancelled) setThumbs((prev) => ({ ...prev, ...found }));
    })();
    onCleanup(() => {
      cancelled = true;
      for (const url of urls) URL.revokeObjectURL(url);
    });
  });
  return (
    <aside tabIndex={-1} class="panel assets" aria-label="Asset browser">
      <header class="panel-head">
        <h2>Assets</h2>
        <span class="count">{total()}</span>
      </header>
      <div class="asset-search">
        <input
          type="search"
          placeholder="Search assets"
          aria-label="Search assets"
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") run();
          }}
        />
        <select value={kind()} aria-label="Asset kind" onChange={(e) => { setKind(e.currentTarget.value); run(); }}>
          <For each={KINDS}>{(k) => <option value={k}>{k}</option>}</For>
        </select>
        <button type="button" onClick={run} disabled={busy()}>
          {busy() ? "..." : "go"}
        </button>
      </div>
      <ul class="thumb-grid" aria-label="Asset results">
        <For each={items()}>
          {(asset) => (
            <li>
              <button
                type="button"
                draggable={true}
                title={asset.path}
                data-kind={asset.kind}
                onDragStart={(e) => e.dataTransfer?.setData("text/r5ed-asset", JSON.stringify(asset))}
                onClick={() => props.onPlace(asset)}
              >
                <span class="thumb" aria-hidden="true">
                  <Show when={thumbs()[asset.path]} fallback={asset.kind.slice(0, 2)}>
                    <img src={thumbs()[asset.path]} alt="" width={THUMB_PX} height={THUMB_PX} loading="lazy" />
                  </Show>
                </span>
                <span class="asset-path">{shorten(asset.path)}</span>
              </button>
            </li>
          )}
        </For>
      </ul>
      <Show when={warnings().length > 0}>
        <ul class="material-warnings" aria-label="Material warnings">
          <For each={warnings()}>
            {(w) => (
              <li data-source={w.source}>
                <strong>{w.material}</strong> {w.message}
              </li>
            )}
          </For>
        </ul>
      </Show>
    </aside>
  );
}

/** Fetches and decodes one thumbnail into a blob URL the browser can draw. */
async function thumbnailUrl(editor: Editor, path: string): Promise<string | null> {
  const host = editor.host;
  if (!host) return null;
  try {
    const bytes = await host.assetThumbnail(path);
    const image = decodePng(bytes);
    if (image.width < 1 || image.height < 1) return null;
    const canvas = document.createElement("canvas");
    canvas.width = image.width;
    canvas.height = image.height;
    const context = canvas.getContext("2d");
    if (!context) return null;
    context.putImageData(new ImageData(new Uint8ClampedArray(image.rgba), image.width, image.height), 0, 0);
    return canvas.toDataURL("image/png");
  } catch (e) {
    if (e instanceof PngError) return null;
    editor.note(`thumbnail ${path}: ${e instanceof Error ? e.message : String(e)}`);
    return null;
  }
}

function shorten(path: string): string {
  const parts = path.split("/");
  return parts.length <= 2 ? path : `${parts[0]}/.../${parts[parts.length - 1]}`;
}

export interface JobPanelProps {
  editor: Editor;
}

/** Job list with progress, a log tail, cancel and the artifacts produced. */
export function JobPanel(props: JobPanelProps): JSX.Element {
  const jobs = createMemo(() => state.jobs.jobs);
  return (
    <section class="panel jobs" aria-label="Jobs">
      <header class="panel-head">
        <h2>Jobs</h2>
        <button type="button" onClick={() => setState("jobs", "panelOpen", false)} aria-label="Close jobs">
          x
        </button>
      </header>
      <ul class="job-list">
        <For each={jobs()}>{(job) => <JobRow job={job} editor={props.editor} />}</For>
      </ul>
      <Show when={jobs().length === 0}>
        <p class="hint">no jobs yet</p>
      </Show>
    </section>
  );
}

function JobRow(props: { job: JobStatus; editor: Editor }): JSX.Element {
  const [expanded, setExpanded] = createSignal(props.job.state === "failed");
  const tail = createMemo(() => props.job.log.split("\n").slice(-40).join("\n"));
  return (
    <li class="job" data-state={props.job.state}>
      <div class="job-head">
        <span class="job-kind">{props.job.kind}</span>
        <progress max={1} value={clamp01(props.job.progress)} aria-label={`${props.job.kind} progress`} />
        <span class="job-state">{props.job.state}</span>
        <Show when={props.job.state === "running" || props.job.state === "queued"}>
          <button type="button" onClick={() => void props.editor.cancelJob(props.job.id)}>
            cancel
          </button>
        </Show>
        <button type="button" aria-expanded={expanded()} onClick={() => setExpanded(!expanded())}>
          log
        </button>
      </div>
      <Show when={expanded()}>
        <pre class="job-log">{tail()}</pre>
      </Show>
      <Show when={props.job.artifacts.length > 0}>
        <ul class="artifacts">
          <For each={props.job.artifacts}>{(a) => <li>{a}</li>}</For>
        </ul>
      </Show>
    </li>
  );
}

function clamp01(v: number): number {
  return Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0;
}
