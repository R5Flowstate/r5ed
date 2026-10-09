import type { Core, Id, MeshBuffers, PickHit } from "../core/api";
import { type Command, commandJson, parseCommand } from "./command";
import { objectFromWire, type DocObject, type MapDoc } from "./model";

export interface ValidationIssue {
  severity: "error" | "warning" | "info";
  id: string | null;
  message: string;
}

export interface DocumentSnapshot {
  doc: MapDoc;
  /** Objects by id, in draw order. */
  objects: Map<Id, DocObject>;
  order: Id[];
  dirty: boolean;
  canUndo: boolean;
  canRedo: boolean;
}

/**
 * The document itself lives in the wasm core. This mirrors only what the UI
 * renders and refreshes it from the ids each `apply`/`undo` returns.
 */
export class DocumentStore {
  private core: Core;
  private doc: MapDoc;
  private objects = new Map<Id, DocObject>();
  private order: Id[] = [];
  private dirtyFlag = false;
  private undoDepth = 0;
  private redoDepth = 0;

  constructor(core: Core, doc: MapDoc) {
    this.core = core;
    this.doc = headerOf(doc);
    this.refresh(core.objectIds());
  }

  static async open(core: Core, docJson: string): Promise<DocumentStore> {
    const text = core.load(docJson);
    return new DocumentStore(core, parseDoc(text));
  }

  /** Starts from the core's default document. */
  static empty(core: Core): DocumentStore {
    return new DocumentStore(core, parseDoc(core.save()));
  }

  snapshot(): DocumentSnapshot {
    return {
      doc: this.doc,
      objects: this.objects,
      order: this.order,
      dirty: this.dirtyFlag,
      canUndo: this.core.canUndo(),
      canRedo: this.core.canRedo(),
    };
  }

  get meta(): MapDoc["meta"] {
    return this.doc.meta;
  }

  get settings(): MapDoc["settings"] {
    return this.doc.settings;
  }

  get layers(): MapDoc["layers"] {
    return this.doc.layers;
  }

  /** Undo depths, for the control socket's undo/redo replies. */
  depths(): { undo: number; redo: number } {
    return { undo: this.undoDepth, redo: this.redoDepth };
  }

  layerVisible(id: Id): boolean {
    return this.doc.layers.find((l) => l.id === id)?.visible ?? true;
  }

  object(id: Id): DocObject | null {
    return this.objects.get(id) ?? null;
  }

  ids(): Id[] {
    return this.order;
  }

  /** Applies a command and refreshes the touched objects. */
  apply(command: Command): Id[] {
    const touched = this.core.apply(commandJson(command));
    this.dirtyFlag = true;
    this.undoDepth++;
    this.redoDepth = 0;
    this.refresh(touched);
    return touched;
  }

  /** Applies command JSON produced by the core (brush tools, importers). */
  applyJson(text: string): Id[] {
    parseCommand(text);
    const touched = this.core.apply(text);
    this.dirtyFlag = true;
    this.undoDepth++;
    this.redoDepth = 0;
    this.refresh(touched);
    return touched;
  }

  undo(): Id[] {
    const touched = this.core.undo();
    if (this.undoDepth > 0) this.undoDepth--;
    this.redoDepth++;
    this.dirtyFlag = true;
    this.refresh(touched);
    return touched;
  }

  redo(): Id[] {
    const touched = this.core.redo();
    if (this.redoDepth > 0) this.redoDepth--;
    this.undoDepth++;
    this.dirtyFlag = true;
    this.refresh(touched);
    return touched;
  }

  clearDirty(): void {
    this.dirtyFlag = false;
  }

  save(): string {
    return this.core.save();
  }

  /**
   * Re-reads the objects a command touched and picks up any the core added or
   * dropped. A command that names no object changed the document itself
   * (meta, settings, layers), so only then is the whole document re-read.
   */
  private refresh(touched: readonly Id[]): void {
    const seen = new Set<Id>();
    let sameSet = touched.length > 0;
    for (const id of touched) {
      if (seen.has(id)) continue;
      seen.add(id);
      const known = this.objects.has(id);
      // wasm-bindgen hands back `undefined` for a Rust `None`, not `null`.
      const text = this.core.object(id);
      if (text == null) this.objects.delete(id);
      else this.objects.set(id, parseObject(text));
      if (!known || text == null) sameSet = false;
    }
    if (sameSet) {
      // Only existing objects changed, so the id list and order stand.
      this.doc = { ...this.doc };
      return;
    }
    const all = this.core.objectIds();
    const live = new Set(all);
    for (const id of all) {
      if (this.objects.has(id) || seen.has(id)) continue;
      const text = this.core.object(id);
      if (text != null) this.objects.set(id, parseObject(text));
    }
    if (live.size !== this.objects.size) {
      for (const id of [...this.objects.keys()]) if (!live.has(id)) this.objects.delete(id);
    }
    this.order = all;
    // A new document identity is what tells the UI and the scene something changed.
    this.doc = touched.length === 0 ? headerOf(parseDoc(this.core.save())) : { ...this.doc };
  }
}

/** The document without its object list; the store keeps objects by id. */
function headerOf(doc: MapDoc): MapDoc {
  return { ...doc, objects: [] };
}

function parseJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    throw new Error("core returned malformed JSON");
  }
}

export function parseDoc(text: string): MapDoc {
  const v = parseJson(text);
  if (!v || typeof v !== "object" || Array.isArray(v)) throw new Error("document JSON is not an object");
  const doc = v as MapDoc;
  return { ...doc, objects: (doc.objects ?? []).map(objectFromWire) };
}

export function parseObject(text: string): DocObject {
  const v = parseJson(text);
  if (!v || typeof v !== "object" || Array.isArray(v)) throw new Error("object JSON is not an object");
  return objectFromWire(v);
}

export function objectMesh(core: Core, id: Id): MeshBuffers | null {
  return core.objectMesh(id);
}

export function pick(core: Core, origin: readonly number[], dir: readonly number[], maxDist: number): PickHit | null {
  return core.pick(origin as never, dir as never, maxDist);
}
