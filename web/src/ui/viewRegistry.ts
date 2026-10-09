import type { Vec3 } from "../core/api";
import type { CameraState } from "../render/camera";
import type { CommandId } from "../input/keymap";
import { state } from "../state/stores";

/** What the shell can ask of one viewport. */
export interface ViewController {
  /** Runs a viewport command; false when this view does not handle it. */
  run(command: CommandId): boolean;
  frame(min: Vec3, max: Vec3): void;
  /** True while a modal operator (transform, walk, box) owns this view. */
  busy(): boolean;
  /** The view's camera, for tests and scripts. */
  camera(): CameraState;
  /** Applies a camera change made outside the view and re-projects its overlay. */
  cameraChanged(): void;
  /** Frames this view has presented; the control socket waits on it. */
  frameSeq(): number;
  /** Offscreen shot at an exact size, without touching the swap chain. */
  capture(width: number, height: number, overlays: boolean): Promise<Uint8Array>;
  /** The pick ray through a point in the view, in CSS pixels. */
  ray(x: number, y: number): { origin: Vec3; dir: Vec3 };
}

export const views = new Map<number, ViewController>();

/** Frames presented across every viewport. */
export let presentedFrames = 0;

/** Records one presented frame. */
export function markPresented(): void {
  presentedFrames++;
}

/** The view under the pointer, or the first one when the pointer is elsewhere. */
export function targetView(): ViewController | undefined {
  const hovered = state.ui.hoverView;
  if (hovered !== null) {
    const v = views.get(hovered);
    if (v) return v;
  }
  return views.values().next().value;
}

export function anyViewBusy(): boolean {
  for (const v of views.values()) if (v.busy()) return true;
  return false;
}
