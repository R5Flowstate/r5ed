import { objectFromWire, objectToWire, type DocObject, type Layer, type MapMeta, type Settings } from "./model";

function mapObjects(command: Command, f: (o: DocObject) => unknown): unknown {
  if (command.op === "add_objects" || command.op === "replace_objects") return { ...command, objects: command.objects.map(f) };
  if (command.op === "batch") return { ...command, commands: command.commands.map((c) => mapObjects(c, f)) };
  return command;
}

export type Command =
  | { op: "add_objects"; objects: DocObject[] }
  | { op: "remove_objects"; ids: string[] }
  | { op: "replace_objects"; objects: DocObject[] }
  | { op: "add_layer"; layer: Layer }
  | { op: "remove_layer"; id: string }
  | { op: "replace_layer"; layer: Layer }
  | { op: "set_meta"; meta: MapMeta }
  | { op: "set_settings"; settings: Settings }
  | { op: "set_file"; path: string; bytes: number[] | null }
  | { op: "batch"; label: string; commands: Command[] };

/** Serialises a command exactly as the wasm core expects it. */
export function commandJson(command: Command): string {
  return JSON.stringify(mapObjects(command, objectToWire));
}

export function addObjects(objects: DocObject[]): Command {
  return { op: "add_objects", objects };
}

export function removeObjects(ids: string[]): Command {
  return { op: "remove_objects", ids };
}

export function replaceObjects(objects: DocObject[]): Command {
  return { op: "replace_objects", objects };
}

export function addLayer(layer: Layer): Command {
  return { op: "add_layer", layer };
}

export function replaceLayer(layer: Layer): Command {
  return { op: "replace_layer", layer };
}

export function removeLayer(id: string): Command {
  return { op: "remove_layer", id };
}

export function setMeta(meta: MapMeta): Command {
  return { op: "set_meta", meta };
}

export function setSettings(settings: Settings): Command {
  return { op: "set_settings", settings };
}

export function setFile(path: string, bytes: Uint8Array | null): Command {
  return { op: "set_file", path, bytes: bytes ? Array.from(bytes) : null };
}

export function batch(label: string, commands: Command[]): Command {
  return { op: "batch", label, commands };
}

/** Parses command JSON from the core, rejecting anything malformed. */
export function parseCommand(text: string): Command {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new Error("command JSON does not parse");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("command is not an object");
  const op = (parsed as { op?: unknown }).op;
  if (typeof op !== "string") throw new Error("command has no op");
  return mapObjects(parsed as Command, (o) => objectFromWire(o)) as Command;
}
