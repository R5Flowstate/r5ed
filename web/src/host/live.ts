export interface LiveProp {
  op: "prop";
  id: string;
  model: string;
  origin: [number, number, number];
  angles: [number, number, number];
  scale: [number, number, number];
}

export interface LiveMove {
  op: "move";
  id: string;
  origin: [number, number, number];
  angles: [number, number, number];
}

export interface LiveDelete {
  op: "delete";
  id: string;
}

export interface LiveReload {
  op: "reload";
  stem: string;
}

export interface LiveStatus {
  op: "status";
  connected?: boolean;
  level?: string;
}

export type LiveMessage = LiveProp | LiveMove | LiveDelete | LiveReload | LiveStatus;

export type LiveStatusLabel = "off" | "connecting" | "open" | "reconnecting" | "error";

const MODEL_PATTERN = /^mdl\/[A-Za-z0-9_/.-]+\.rmdl$/;
const UUID_PATTERN = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const STEM_PATTERN = /^[A-Za-z0-9_]{1,64}$/;

export class LiveValidationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "LiveValidationError";
  }
}

function finiteTriple(v: unknown): [number, number, number] {
  if (!Array.isArray(v) || v.length !== 3) throw new LiveValidationError("expected three numbers");
  const out = v as unknown[];
  const nums: number[] = [];
  for (const n of out) {
    if (typeof n !== "number" || !Number.isFinite(n) || Math.abs(n) > 65536) {
      throw new LiveValidationError("coordinate out of range");
    }
    nums.push(n);
  }
  return [nums[0], nums[1], nums[2]];
}

/** Rejects messages the host would refuse, before they leave the tab. */
export function validateLiveMessage(msg: unknown): LiveMessage {
  if (!msg || typeof msg !== "object") throw new LiveValidationError("message is not an object");
  const m = msg as Record<string, unknown>;
  switch (m.op) {
    case "prop": {
      if (typeof m.id !== "string" || !UUID_PATTERN.test(m.id)) throw new LiveValidationError("bad id");
      if (typeof m.model !== "string" || !MODEL_PATTERN.test(m.model)) throw new LiveValidationError("bad model path");
      return {
        op: "prop",
        id: m.id,
        model: m.model,
        origin: finiteTriple(m.origin),
        angles: finiteTriple(m.angles),
        scale: finiteTriple(m.scale),
      };
    }
    case "move": {
      if (typeof m.id !== "string" || !UUID_PATTERN.test(m.id)) throw new LiveValidationError("bad id");
      return { op: "move", id: m.id, origin: finiteTriple(m.origin), angles: finiteTriple(m.angles) };
    }
    case "delete": {
      if (typeof m.id !== "string" || !UUID_PATTERN.test(m.id)) throw new LiveValidationError("bad id");
      return { op: "delete", id: m.id };
    }
    case "reload": {
      if (typeof m.stem !== "string" || !STEM_PATTERN.test(m.stem)) throw new LiveValidationError("bad map stem");
      return { op: "reload", stem: m.stem };
    }
    case "status":
      return { op: "status", connected: m.connected === true, level: typeof m.level === "string" ? m.level : undefined };
    default:
      throw new LiveValidationError(`unknown op ${String(m.op)}`);
  }
}

export interface SocketLike {
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onclose: ((ev: unknown) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
}

export interface LiveOptions {
  url: string;
  socketFactory: (url: string) => SocketLike;
  onStatus?: (status: LiveStatusLabel, detail?: string) => void;
  onMessage?: (msg: LiveMessage) => void;
  /** Base delay in ms; the reconnect delay doubles up to `maxBackoff`. */
  backoffMs?: number;
  maxBackoffMs?: number;
  setTimeoutFn?: (fn: () => void, ms: number) => number;
  clearTimeoutFn?: (id: number) => void;
}

/** Live-link socket with exponential-backoff reconnect. */
export class LiveLink {
  private socket: SocketLike | null = null;
  private timer: number | null = null;
  private closed = false;
  private attempts = 0;
  private readonly backoff: number;
  private readonly maxBackoff: number;
  private readonly setTimeoutFn: (fn: () => void, ms: number) => number;
  private readonly clearTimeoutFn: (id: number) => void;
  status: LiveStatusLabel = "off";

  constructor(private readonly options: LiveOptions) {
    this.backoff = options.backoffMs ?? 250;
    this.maxBackoff = options.maxBackoffMs ?? 8000;
    const g = globalThis as { setTimeout?: (f: () => void, m: number) => number; clearTimeout?: (i: number) => void };
    this.setTimeoutFn = options.setTimeoutFn ?? ((fn, ms) => g.setTimeout!(fn, ms));
    this.clearTimeoutFn = options.clearTimeoutFn ?? ((id) => g.clearTimeout!(id));
  }

  private emit(status: LiveStatusLabel, detail?: string): void {
    this.status = status;
    this.options.onStatus?.(status, detail);
  }

  connect(): void {
    if (this.closed) return;
    this.emit(this.attempts === 0 ? "connecting" : "reconnecting");
    let socket: SocketLike;
    try {
      socket = this.options.socketFactory(this.options.url);
    } catch (e) {
      this.emit("error", e instanceof Error ? e.message : String(e));
      this.scheduleReconnect();
      return;
    }
    this.socket = socket;
    socket.onopen = () => {
      this.attempts = 0;
      this.emit("open");
      this.send({ op: "status" });
    };
    socket.onclose = () => {
      this.socket = null;
      if (this.closed) return;
      this.scheduleReconnect();
    };
    socket.onerror = () => this.emit("error");
    socket.onmessage = (ev) => {
      const raw = typeof ev.data === "string" ? ev.data : "";
      if (!raw) return;
      let parsed: unknown;
      try {
        parsed = JSON.parse(raw);
      } catch {
        return;
      }
      try {
        this.options.onMessage?.(validateLiveMessage(parsed));
      } catch (e) {
        this.emit("error", e instanceof Error ? e.message : String(e));
      }
    };
  }

  private scheduleReconnect(): void {
    if (this.closed || this.timer !== null) return;
    this.emit("reconnecting");
    const delay = Math.min(this.maxBackoff, this.backoff * Math.pow(2, this.attempts));
    this.attempts++;
    this.timer = this.setTimeoutFn(() => {
      this.timer = null;
      this.connect();
    }, delay);
  }

  /** Sends a validated message; false when the link is not open. */
  send(msg: LiveMessage): boolean {
    if (!this.socket || this.status !== "open") return false;
    this.socket.send(JSON.stringify(msg));
    return true;
  }

  close(): void {
    this.closed = true;
    if (this.timer !== null) {
      this.clearTimeoutFn(this.timer);
      this.timer = null;
    }
    this.socket?.close();
    this.socket = null;
    this.emit("off");
  }
}
