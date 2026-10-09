export interface SessionInfo {
  version: string;
  game_root: string;
  projects_root: string;
  capabilities: Record<string, boolean>;
}

export interface ProjectListEntry {
  name: string;
  modified: number;
}

export interface ProjectFileEntry {
  path: string;
  size: number;
  modified: number;
}

export interface AssetSearchItem {
  kind: string;
  path: string;
  guid: string;
}

export interface AssetSearchResult {
  total: number;
  items: AssetSearchItem[];
}

/** One document material as the host resolved it; `albedo` is cached when set. */
export interface MaterialPrefetch {
  name: string;
  path: string | null;
  albedo: string | null;
  translucent: boolean;
  error: string | null;
}

/** What a model prefetch did; `missing` are paths the asset index does not know. */
export interface ModelPrefetch {
  ready: number;
  exported: number;
  missing: string[];
}

export interface ModelInfo {
  path: string;
  bounds: { min: [number, number, number]; max: [number, number, number] };
  parts: string[];
  skin_count: number;
}

/** One material as the material route reports it. */
export interface MaterialInfo {
  path: string;
  textures: Record<string, string>;
  guid: string;
  /** No depth write: drawn in the game's translucent pass. */
  translucent?: boolean;
}

/** One folder as the rpak browser lists it. */
export interface BrowseListing {
  dir: string;
  parent: string | null;
  entries: { name: string; path: string; kind: "dir" | "pak"; stem: string }[];
}

export interface MapEntry {
  stem: string;
  has_client: boolean;
  has_dedi: boolean;
}

export interface MapReference {
  chunks: string[];
  entities: { origin: [number, number, number]; classname: string }[];
  bounds: { min: [number, number, number]; max: [number, number, number] };
}

export type JobKind =
  | "compile" | "bake" | "navmesh" | "package" | "deploy" | "reload"
  | "import_tf2" | "import_map" | "export_minimap" | "export_loadscreen";

export interface JobStatus {
  id: string;
  kind: JobKind;
  state: JobState;
  progress: number;
  log: string;
  artifacts: string[];
}

export type JobState = "queued" | "running" | "done" | "failed" | "cancelled";

/** A job as the host serves it (`r5ed_host::jobs::JobView`). */
export interface WireJob {
  id: string;
  kind: JobKind;
  state: "queued" | "running" | "succeeded" | "failed" | "cancelled" | JobState;
  /** 0-100. */
  progress: number;
  log: { seq: number; text: string; progress?: number | null }[] | string;
  artifacts: string[];
  error?: string | null;
}

/**
 * The host names a finished job `succeeded`, reports progress in percent and
 * sends the log as numbered lines; the panel wants `done`, 0-1 and text.
 */
export function jobFromWire(job: WireJob): JobStatus {
  const lines = typeof job.log === "string" ? [job.log] : (job.log ?? []).map((l) => l.text);
  if (job.error && !lines.includes(job.error)) lines.push(job.error);
  return {
    id: job.id,
    kind: job.kind,
    state: job.state === "succeeded" ? "done" : job.state,
    progress: Math.min(1, Math.max(0, (Number(job.progress) || 0) / 100)),
    log: lines.join("\n"),
    artifacts: job.artifacts ?? [],
  };
}

export class HostError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "HostError";
  }
}


/** A tab's token is handed over in the fragment and never kept in the URL. */
export function readToken(hash: string): string {
  return readHashParam(hash, "t");
}

/** One `key=value` of a URL fragment, decoded, or "" when it is absent. */
export function readHashParam(hash: string, key: string): string {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  for (const part of raw.split("&")) {
    const eq = part.indexOf("=");
    if (eq < 0 || part.slice(0, eq) !== key) continue;
    try {
      return decodeURIComponent(part.slice(eq + 1));
    } catch {
      return "";
    }
  }
  return "";
}

/** Returns the URL without its fragment and reports the clean form. */
export function stripHash(url: string, replace: (clean: string) => void): string {
  const hashAt = url.indexOf("#");
  if (hashAt < 0) return url;
  const clean = url.slice(0, hashAt);
  replace(clean);
  return clean;
}

export interface HostOptions {
  base?: string;
  fetch?: typeof fetch;
}

/** Typed client for every host route. The token is held in memory only. */
export class HostClient {
  private readonly base: string;
  private readonly doFetch: typeof fetch;
  private token: string;
  private inflight = 0;

  /**
   * An empty token relies on the session cookie the host sets when it serves
   * the editor page; a token from the URL fragment is sent as a header.
   */
  constructor(token: string, options: HostOptions = {}) {
    this.token = token;
    this.base = options.base ?? "";
    this.doFetch = options.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  }

  /** Replaces the in-memory token; used when a relaunch issues a new one. */
  setToken(token: string): void {
    if (!token) throw new HostError(0, "missing host token");
    this.token = token;
  }

  private headers(extra?: Record<string, string>): Record<string, string> {
    return this.token ? { "X-R5ED-Token": this.token, ...extra } : { ...extra };
  }

  /** Host fetches in flight; the control socket waits on it. */
  pending(): number {
    return this.inflight;
  }

  private async request<T>(method: string, path: string, init?: RequestInit & { query?: Record<string, string | number | undefined> }): Promise<T> {
    const url = this.base + path + queryString(init?.query);
    this.inflight++;
    try {
      const send = (): Promise<Response> =>
        this.doFetch(url, { ...init, method, headers: this.headers(init?.headers as Record<string, string> | undefined) });
      let res = await send();
      if (res.status === 401) {
        // A restarted host issues a new token; the page it serves carries the
        // new one in its cookie, and a token from before the restart is stale.
        await this.doFetch(`${this.base}/`, { method: "GET" }).catch(() => undefined);
        this.token = "";
        res = await send();
      }
      return await readResponse<T>(res);
    } finally {
      this.inflight--;
    }
  }

  session(): Promise<SessionInfo> {
    return this.request("GET", "/api/session");
  }

  projects(): Promise<ProjectListEntry[]> {
    return this.request("GET", "/api/projects");
  }

  createProject(name: string): Promise<{ name: string }> {
    return this.request("POST", "/api/projects", {
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    });
  }

  projectFiles(name: string): Promise<ProjectFileEntry[]> {
    return this.request("GET", `/api/projects/${encodeURIComponent(name)}/files`);
  }

  readProjectFile(name: string, path: string): Promise<Uint8Array> {
    return this.request("GET", `/api/projects/${encodeURIComponent(name)}/file`, { query: { path } });
  }

  writeProjectFile(name: string, path: string, bytes: Uint8Array): Promise<void> {
    return this.request("PUT", `/api/projects/${encodeURIComponent(name)}/file`, {
      query: { path },
      headers: { "Content-Type": "application/octet-stream" },
      body: bytes as unknown as BodyInit,
    }).then(() => undefined);
  }

  assetSearch(q: string, kind: string, offset: number, limit: number): Promise<AssetSearchResult> {
    return this.request("GET", "/api/assets/search", { query: { q, kind, offset, limit } });
  }

  assetModel(path: string): Promise<Uint8Array> {
    return this.request("GET", "/api/assets/model", { query: { path } });
  }

  /** Exports a batch of models in one pass per pak, so their fetches that follow are cache reads. */
  prefetchModels(paths: string[]): Promise<ModelPrefetch> {
    return this.request("POST", "/api/assets/models/prefetch", {
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ paths }),
    });
  }

  /** Resolves a batch of document materials to their albedo textures, exporting and caching them. */
  prefetchMaterials(names: string[], suffixes: readonly string[]): Promise<MaterialPrefetch[]> {
    return this.request("POST", "/api/assets/materials/prefetch", {
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ names, suffixes }),
    });
  }

  assetModelInfo(path: string): Promise<ModelInfo> {
    return this.request("GET", "/api/assets/model_info", { query: { path } });
  }

  assetMaterial(path: string): Promise<MaterialInfo> {
    return this.request("GET", "/api/assets/material", { query: { path } });
  }

  assetTexture(guid: string): Promise<Uint8Array> {
    return this.request("GET", "/api/assets/texture", { query: { guid } });
  }

  assetThumbnail(path: string): Promise<Uint8Array> {
    return this.request("GET", "/api/assets/thumbnail", { query: { path } });
  }

  surfaceProps(): Promise<Record<string, number>> {
    return this.request("GET", "/api/assets/surface_props");
  }

  /** Rescans the asset index; returns the new entry count. */
  rebuildAssets(): Promise<{ assets: number }> {
    return this.request("POST", "/api/assets/rebuild", {
      headers: { "Content-Type": "application/json" },
      body: "{}",
    });
  }

  maps(): Promise<MapEntry[]> {
    return this.request("GET", "/api/maps");
  }

  mapReference(stem: string): Promise<MapReference> {
    return this.request("GET", `/api/maps/${encodeURIComponent(stem)}/reference`);
  }

  mapChunk(stem: string, n: number): Promise<ArrayBuffer> {
    return this.request("GET", `/api/maps/${encodeURIComponent(stem)}/chunk/${n}`);
  }

  schema(): Promise<string> {
    return this.request("GET", "/api/schema").then((s) => (typeof s === "string" ? s : JSON.stringify(s)));
  }

  /** Subfolders and map client paks of one folder; empty opens the install's paks. */
  browse(path: string): Promise<BrowseListing> {
    return this.request("GET", "/api/browse", { query: { path } });
  }

  createJob(kind: JobKind, project: string, params: Record<string, unknown>): Promise<{ id: string }> {
    return this.request("POST", "/api/jobs", {
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ kind, project, params }),
    });
  }

  job(id: string): Promise<JobStatus> {
    return this.request<WireJob>("GET", `/api/jobs/${encodeURIComponent(id)}`).then(jobFromWire);
  }

  /** The most recent jobs, newest first. */
  jobs(): Promise<JobStatus[]> {
    return this.request<WireJob[]>("GET", "/api/jobs").then((jobs) => (Array.isArray(jobs) ? jobs.map(jobFromWire) : []));
  }

  cancelJob(id: string): Promise<void> {
    return this.request("POST", `/api/jobs/${encodeURIComponent(id)}/cancel`, {
      headers: { "Content-Type": "application/json" },
      body: "{}",
    }).then(() => undefined);
  }
}

function queryString(query: Record<string, string | number | undefined> | undefined): string {
  if (!query) return "";
  const parts: string[] = [];
  for (const [k, v] of Object.entries(query)) {
    if (v === undefined || v === "") continue;
    parts.push(`${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`);
  }
  return parts.length ? `?${parts.join("&")}` : "";
}

async function readResponse<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = res.statusText;
    try {
      message = await res.text();
    } catch {
      // The body is optional on error responses.
    }
    throw new HostError(res.status, message || `host request failed with ${res.status}`);
  }
  const type = res.headers.get("content-type") ?? "";
  if (type.includes("application/json")) return (await res.json()) as T;
  if (type.includes("octet-stream") || type.includes("gltf-binary") || type.includes("dds")) {
    return (new Uint8Array(await res.arrayBuffer())) as T;
  }
  return (await res.text()) as T;
}

/** WebSocket URL for the live link, carrying the token as a query parameter. */
export function liveUrl(base: string, token: string, origin?: string): string {
  const root = base || origin || globalThis.location?.origin || "http://127.0.0.1";
  const url = new URL("/api/live", root);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  if (token) url.searchParams.set("token", token);
  return url.toString();
}

