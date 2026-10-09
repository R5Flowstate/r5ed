import type { Editor } from "../state/editor";
import { readHashParam } from "../host/client";
import { createHandler } from "./dispatch";
import { installErrorTrap, takeErrors } from "./errors";

export type ControlHandle = (msg: unknown) => Promise<Record<string, unknown>>;

/** The page exposes its dispatcher when the fragment asks for it. */
export function controlRequested(hash: string): boolean {
  return readHashParam(hash, "control") === "1";
}

/** WebSocket URL of the host's editor relay, carrying the token as a query parameter. */
export function editorSocketUrl(token: string, origin?: string): string {
  const root = origin || globalThis.location?.origin || "http://127.0.0.1";
  const url = new URL("/api/editor", root);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  if (token) url.searchParams.set("token", token);
  return url.toString();
}

function expose(handle: ControlHandle | null): void {
  const g = globalThis as { __r5edControl?: { handle: ControlHandle } };
  if (handle) g.__r5edControl = { handle };
  else delete g.__r5edControl;
}

/** The host's envelope around one verb call. */
interface Envelope {
  id: unknown;
  verb: unknown;
  params: unknown;
}

function parseEnvelope(text: string): Envelope | null {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof v !== "object" || v === null || Array.isArray(v)) return null;
  return v as Envelope;
}

/** Relay socket with exponential-backoff reconnect, shaped like the live link. */
export class ControlSocket {
  private socket: WebSocket | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private closed = false;
  private attempts = 0;

  constructor(
    private readonly handle: ControlHandle,
    private readonly url: string,
    private readonly keepExposed: boolean,
  ) {}

  connect(): void {
    if (this.closed) return;
    let socket: WebSocket;
    try {
      socket = new WebSocket(this.url);
    } catch {
      this.schedule();
      return;
    }
    this.socket = socket;
    socket.onopen = () => {
      this.attempts = 0;
      expose(this.handle);
    };
    socket.onclose = () => {
      this.socket = null;
      if (!this.keepExposed) expose(null);
      if (!this.closed) this.schedule();
    };
    socket.onerror = () => {
      socket.close();
    };
    socket.onmessage = (ev) => {
      const raw = typeof ev.data === "string" ? ev.data : "";
      const env = raw ? parseEnvelope(raw) : null;
      if (!env || typeof env.id !== "string" || env.id.length === 0) return;
      const id = env.id;
      void this.handle({ verb: env.verb, params: env.params ?? {} })
        .catch((e: unknown) => ({ error: e instanceof Error ? e.message : String(e) }))
        .then((reply) => {
          if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ id, ...reply }));
        });
    };
  }

  private schedule(): void {
    if (this.closed || this.timer !== null) return;
    const delay = Math.min(8000, 250 * Math.pow(2, this.attempts));
    this.attempts++;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.connect();
    }, delay);
  }

  close(): void {
    this.closed = true;
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.socket?.close();
    this.socket = null;
  }
}

/**
 * Exposes the dispatcher for the headless path and connects the host relay
 * when this page was served by the host. The handle stays hidden otherwise.
 */
export function enableControl(editor: Editor): ControlSocket | null {
  installErrorTrap();
  const flag = controlRequested(globalThis.location?.hash ?? "");
  const handle = createHandler({ editor, takeErrors });
  if (flag) expose(handle);
  if (!editor.session) return null;
  const socket = new ControlSocket(handle, editorSocketUrl(editor.token ?? ""), flag);
  socket.connect();
  return socket;
}
