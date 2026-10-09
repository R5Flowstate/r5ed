import { describe, expect, it } from "vitest";
import type { Quat, Vec3 } from "../core/api";
import { quatFromSourceAngles, sourceAnglesFromQuat } from "../math/quat";
import { rayAxis, rayPlane } from "../math/ray";
import {
  type GizmoMode,
  type GizmoSpace,
  type HandleHit,
  applyDrag,
  axisAngle,
  beginDrag,
  handleAxis,
  pickHandle,
  snapAngle,
  snapPosition,
  snapValue,
  wrapDegrees,
} from "./gizmo";

const IDENTITY: Quat = [0, 0, 0, 1];

/** Projects world points to pixels: +X right, +Y up, origin at 500,500. */
function orthoProject(p: Vec3): { x: number; y: number } {
  return { x: p[0] + 500, y: 500 - p[1] };
}

describe("gizmo snapping", () => {
  it("snaps to multiples and handles negatives symmetrically", () => {
    expect(snapValue(17, 16)).toBe(16);
    expect(snapValue(24, 16)).toBe(32);
    expect(snapValue(-17, 16)).toBe(-16);
    expect(snapValue(-24, 16)).toBe(-32);
    expect(snapValue(5, 0)).toBe(5);
    expect(snapValue(5, -3)).toBe(5);
    expect(snapValue(Number.NaN, 16)).toBeNaN();
    expect(snapValue(7, 64)).toBe(0);
  });

  it("snaps angles to the configured step and wraps into range", () => {
    expect(snapAngle(20, 15)).toBe(15);
    expect(snapAngle(23, 15)).toBe(30);
    expect(snapAngle(-23, 15)).toBe(-30);
    expect(snapAngle(10, 0)).toBe(10);
    expect(wrapDegrees(370)).toBe(10);
    expect(wrapDegrees(-190)).toBe(170);
  });

  it("snaps a whole position only when snapping is on", () => {
    const snap = { enabled: true, size: 16, angle: 15, scale: 0.1 };
    expect(snapPosition([17, -17, 512], snap)).toEqual([16, -16, 512]);
    expect(snapPosition([17, -17, 512], { ...snap, enabled: false })).toEqual([17, -17, 512]);
  });
});

describe("ray/axis intersection", () => {
  it("returns the parameter of closest approach along the axis", () => {
    // The ray crosses the X axis at [-100, 0, 0]: t = -100 along X.
    const behind = rayAxis({ origin: [-100, 200, 0], dir: [0, -1, 0] }, [0, 0, 0], [1, 0, 0]);
    expect(behind as number).toBeCloseTo(-100, 6);
    const ahead = rayAxis({ origin: [50, 200, 0], dir: [0, -1, 0] }, [0, 0, 0], [1, 0, 0]);
    expect(ahead as number).toBeCloseTo(50, 6);
    // A ray through the pivot's own plane gives zero.
    expect(rayAxis({ origin: [0, 0, 100], dir: [0, 0, -1] }, [0, 0, 0], [1, 0, 0]) as number).toBeCloseTo(0, 6);
  });

  it("returns null for a parallel ray and for a degenerate axis", () => {
    expect(rayAxis({ origin: [0, 0, 0], dir: [1, 0, 0] }, [0, 0, 0], [1, 0, 0])).toBeNull();
    expect(rayAxis({ origin: [0, 0, 0], dir: [1, 0, 0] }, [0, 0, 0], [0, 0, 0])).toBeNull();
  });

  it("intersects a plane and rejects a parallel ray", () => {
    expect(rayPlane({ origin: [0, 0, 100], dir: [0, 0, -1] }, [0, 0, 0], [0, 0, 1])).toBeCloseTo(100, 6);
    expect(rayPlane({ origin: [0, 0, 100], dir: [1, 0, 0] }, [0, 0, 0], [0, 0, 1])).toBeNull();
  });
});

/** Picks the handle of `mode` at a world point; the cursor is that point's pixel. */
function grabAt(mode: GizmoMode, point: Vec3, space: GizmoSpace = "world"): HandleHit {
  const pivot: Vec3 = [0, 0, 0];
  const hit = pickHandle(
    { origin: [point[0], point[1], point[2] + 400], dir: [0, 0, -1] },
    pivot,
    IDENTITY,
    space,
    mode,
    orthoProject,
    orthoProject(point),
    200,
    4,
  );
  if (!hit) throw new Error(`no ${mode} handle at ${point.join(",")}`);
  return hit;
}

describe("gizmo handle picking", () => {
  it("grabs an axis only along the drawn arm, not anywhere on the infinite axis", () => {
    const at = (x: number) =>
      pickHandle({ origin: [x, 0, 400], dir: [0, 0, -1] }, [0, 0, 0], IDENTITY, "world", "translate", orthoProject, orthoProject([x, 0, 0]), 200, 4);
    expect(at(150)?.handle).toBe("x");
    expect(at(1000)).toBeNull();
  });

  it("grabs the axis handle under the cursor and measures travel along it", () => {
    const hit = grabAt("translate", [200, 0, 0]);
    expect(hit.handle).toBe("x");
    expect(hit.t).toBeCloseTo(200, 6);
    expect(hit.point[0]).toBeCloseTo(200, 6);
    expect(hit.screenDist).toBeLessThan(0.001);
  });

  it("grabs the plane handle off the axes and reports its two components", () => {
    // Inside the corner square, which is drawn out to 0.35 of the arm.
    const hit = grabAt("translate", [60, 60, 0]);
    expect(hit.handle).toBe("xy");
    expect(hit.plane[0]).toBeCloseTo(60, 4);
    expect(hit.plane[1]).toBeCloseTo(60, 4);
  });

  it("ranks handles by pixel distance, so a nearer axis beats a plane", () => {
    const point: Vec3 = [200, 0, 0];
    const hit = grabAt("translate", point);
    const planeOnly = pickHandle(
      { origin: [200, 0, 400], dir: [0, 0, -1] },
      [0, 0, 0],
      IDENTITY,
      "world",
      "translate",
      orthoProject,
      orthoProject(point),
      200,
      4,
    );
    expect(hit.handle).toBe("x");
    // The screen handle sits on the same pixel; the axis wins on the smaller distance.
    expect(planeOnly?.screenDist).toBeLessThan(0.001);
  });

  it("misses when the cursor is far from every handle", () => {
    const miss = pickHandle(
      { origin: [0, 0, 400], dir: [0, 0, -1] },
      [0, 0, 0],
      IDENTITY,
      "world",
      "translate",
      orthoProject,
      { x: 120, y: 120 },
      200,
      4,
    );
    expect(miss).toBeNull();
  });

  it("rotates on rings, so a point on the axis is not a rotate handle", () => {
    // The ring is drawn at 0.85 of the arm, so it is grabbed at 170.
    expect(grabAt("rotate", [170, 0, 0]).handle).toBe("z");
    const onAxis = pickHandle(
      { origin: [0, 0, 400], dir: [0, 0, -1] },
      [0, 0, 0],
      IDENTITY,
      "world",
      "rotate",
      orthoProject,
      orthoProject([0, 0, 0]),
      200,
      4,
    );
    expect(onAxis).toBeNull();
  });
});

describe("gizmo space", () => {
  it("keeps world axes in world space and rotates them in local space", () => {
    expect(handleAxis("x", IDENTITY, "world")).toEqual([1, 0, 0]);
    // A yaw of 90 turns local X onto world Y.
    const aboutZ = quatFromSourceAngles([0, 90, 0]);
    const localX = handleAxis("x", aboutZ, "local");
    expect(localX[0]).toBeCloseTo(0, 5);
    expect(localX[1]).toBeCloseTo(1, 5);
    // A pitch of 90 turns local Z onto world X.
    const aboutX = quatFromSourceAngles([90, 0, 0]);
    const localZ = handleAxis("z", aboutX, "local");
    expect(localZ[0]).toBeCloseTo(1, 5);
    expect(localZ[2]).toBeCloseTo(0, 5);
  });

  it("picks a local-space axis handle at the rotated position", () => {
    const rotated = quatFromSourceAngles([0, 90, 0]);
    const hit = pickHandle(
      { origin: [0, 200, 400], dir: [0, 0, -1] },
      [0, 0, 0],
      rotated,
      "local",
      "translate",
      orthoProject,
      orthoProject([0, 200, 0]),
      200,
      4,
    );
    expect(hit?.handle).toBe("x");
    expect(hit?.t).toBeCloseTo(200, 4);
  });
});

describe("gizmo drag", () => {
  const pivot: Vec3 = [0, 0, 0];
  const snap = { enabled: true, size: 16, angle: 15, scale: 0.1 };

  function dragFrom(mode: GizmoMode, grab: Vec3, to: Vec3, space: GizmoSpace = "world") {
    const hit = grabAt(mode, grab, space);
    const state = beginDrag({
      r: { origin: [grab[0], grab[1], grab[2] + 400], dir: [0, 0, -1] },
      pivot,
      rotation: IDENTITY,
      position: [0, 0, 0],
      objectScale: [1, 1, 1],
      mode,
      space,
      hit,
      snap,
      cameraDir: [0, 0, -1],
    });
    return applyDrag(state, { origin: [to[0], to[1], to[2] + 400], dir: [0, 0, -1] });
  }

  it("moves along the grabbed axis only", () => {
    const delta = dragFrom("translate", [200, 0, 0], [260, 90, 0]);
    // 60 units of travel snapped to the 16 inch grid.
    expect((delta.position as Vec3)[0]).toBeCloseTo(64, 6);
    expect((delta.position as Vec3)[1]).toBe(0);
    expect((delta.position as Vec3)[2]).toBe(0);
  });

  it("does not snap when snapping is off", () => {
    const hit = grabAt("translate", [200, 0, 0]);
    const state = beginDrag({
      r: { origin: [200, 0, 400], dir: [0, 0, -1] },
      pivot,
      rotation: IDENTITY,
      position: [0, 0, 0],
      objectScale: [1, 1, 1],
      mode: "translate",
      space: "world",
      hit,
      snap: { ...snap, enabled: false },
      cameraDir: [0, 0, -1],
    });
    const delta = applyDrag(state, { origin: [260, 0, 400], dir: [0, 0, -1] });
    expect((delta.position as Vec3)[0]).toBeCloseTo(60, 6);
  });

  it("moves on the grabbed plane by the cursor delta, not the grab point", () => {
    const delta = dragFrom("translate", [60, 60, 0], [110, 60, 0]);
    const p = delta.position as Vec3;
    // 50 units of travel on the XY plane, snapped to the 16 inch grid.
    expect(p[0]).toBeCloseTo(48, 6);
    expect(p[1]).toBe(0);
    expect(p[2]).toBe(0);
  });

  it("scales one axis by the ratio of the drag to the grab", () => {
    const delta = dragFrom("scale", [200, 0, 0], [300, 0, 0]);
    expect((delta.scale as Vec3)[0]).toBeCloseTo(1.5, 6);
    expect((delta.scale as Vec3)[1]).toBe(1);
    expect((delta.scale as Vec3)[2]).toBe(1);
  });

  it("snaps scale to the configured fraction", () => {
    const delta = dragFrom("scale", [200, 0, 0], [280, 0, 0]);
    // 1.4 rounds to 1.4 at a 0.1 step; 1.36 rounds to 1.4 too.
    expect((delta.scale as Vec3)[0]).toBeCloseTo(1.4, 6);
  });

  it("rotates in 15 degree steps and accumulates across moves", () => {
    const hit = grabAt("rotate", [170, 0, 0]);
    const state = beginDrag({
      r: { origin: [170, 0, 400], dir: [0, 0, -1] },
      pivot,
      rotation: IDENTITY,
      position: [0, 0, 0],
      objectScale: [1, 1, 1],
      mode: "rotate",
      space: "world",
      hit,
      snap,
      cameraDir: [0, 0, -1],
    });
    // 90 degrees about +Z, which the .ent angles report as yaw.
    const first = applyDrag(state, { origin: [0, 200, 400], dir: [0, 0, -1] });
    expect(sourceAnglesFromQuat(first.rotation as Quat)[1]).toBeCloseTo(90, 3);
    // A second move continues from the accumulated total instead of restarting:
    // the raw total is 270, and a quaternion only reports it wrapped.
    const again = applyDrag(state, { origin: [-200, 0, 400], dir: [0, 0, -1] });
    expect(state.totalAngle).toBeCloseTo(270, 6);
    expect(sourceAnglesFromQuat(again.rotation as Quat)[1]).toBeCloseTo(-90, 3);
  });

  it("snaps a rotate drag to the angle step", () => {
    const hit = grabAt("rotate", [170, 0, 0]);
    const state = beginDrag({
      r: { origin: [170, 0, 400], dir: [0, 0, -1] },
      pivot,
      rotation: IDENTITY,
      position: [0, 0, 0],
      objectScale: [1, 1, 1],
      mode: "rotate",
      space: "world",
      hit,
      snap: { ...snap, angle: 15 },
      cameraDir: [0, 0, -1],
    });
    // 53.13 degrees raw snaps to 60.
    const delta = applyDrag(state, { origin: [120, 160, 400], dir: [0, 0, -1] });
    const yawed = sourceAnglesFromQuat(delta.rotation as Quat)[1];
    expect(Math.abs(yawed / 15 - Math.round(yawed / 15))).toBeLessThan(1e-6);
    expect(yawed).toBeCloseTo(60, 3);
  });
});

describe("axis angle", () => {
  it("measures the signed angle about an axis and is zero on the axis", () => {
    expect(axisAngle([200, 0, 0], [0, 0, 0], [0, 0, 1])).toBeCloseTo(0, 6);
    expect(axisAngle([0, 200, 0], [0, 0, 0], [0, 0, 1])).toBeCloseTo(90, 6);
    expect(axisAngle([0, -200, 0], [0, 0, 0], [0, 0, 1])).toBeCloseTo(-90, 6);
    expect(axisAngle([0, 0, 200], [0, 0, 0], [0, 0, 1])).toBe(0);
  });

  it("is right-handed about X too", () => {
    expect(axisAngle([0, 200, 0], [0, 0, 0], [1, 0, 0])).toBeCloseTo(0, 6);
    expect(axisAngle([0, 0, 200], [0, 0, 0], [1, 0, 0])).toBeCloseTo(90, 6);
  });
});
