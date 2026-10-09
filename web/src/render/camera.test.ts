import { describe, expect, it } from "vitest";
import type { Vec3 } from "../core/api";
import { Camera, defaultCamera, dolly, eyeFor, flipView, fly, frameBounds, isPerspective, isPerspectiveState, orbit, pan, panScale, setAxisView, setOrtho, viewAxis, viewForward, viewLabel, viewUp, slide, zoomAbout, zoomOrtho } from "./camera";
import { frustumFromViewProj, aabbInFrustum, sphereInFrustum } from "./frustum";
import { invert, lookAt, mat4, multiply, perspective, ortho, transformPoint, unproject } from "../math/mat4";
import { add, normalize, scale, sub } from "../math/vec3";

function viewOf(state: Partial<ReturnType<typeof defaultCamera>> = {}) {
  const s = { ...defaultCamera(), ...state };
  const camera = new Camera(s);
  camera.setAspect(16 / 9);
  return camera;
}

describe("projection matrices", () => {
  it("maps the near and far planes onto 0 and 1 in depth", () => {
    const p = perspective(mat4(), Math.PI / 3, 1.5, 10, 1000);
    const near = transformPoint(p, [0, 0, -10]);
    const far = transformPoint(p, [0, 0, -1000]);
    expect(near[2] / near[3]).toBeCloseTo(0, 6);
    expect(far[2] / far[3]).toBeCloseTo(1, 6);
    expect(near[3]).toBeCloseTo(10, 6);
  });

  it("keeps ortho depth linear across the range", () => {
    const o = ortho(mat4(), 512, 512, -2048, 2048);
    expect(transformPoint(o, [0, 0, 2048])[2]).toBeCloseTo(0, 6);
    expect(transformPoint(o, [0, 0, -2048])[2]).toBeCloseTo(1, 6);
    expect(transformPoint(o, [512, 0, 0])[0]).toBeCloseTo(1, 6);
  });

  it("places the eye at the origin of view space", () => {
    const v = lookAt(mat4(), [100, 200, 300], [0, 0, 0], [0, 0, 1]);
    const atEye = transformPoint(v, [100, 200, 300]);
    expect(atEye[0]).toBeCloseTo(0, 5);
    expect(atEye[1]).toBeCloseTo(0, 5);
    expect(atEye[2]).toBeCloseTo(0, 5);
    // Looking down at the origin from +Z puts the origin below the eye.
    const atOrigin = transformPoint(v, [0, 0, 0]);
    expect(atOrigin[2]).toBeLessThan(0);
  });

  it("inverts a view-projection pair", () => {
    const view = lookAt(mat4(), [10, 10, 10], [0, 0, 0], [0, 0, 1]);
    const proj = perspective(mat4(), 1, 1.3, 1, 500);
    const vp = multiply(mat4(), proj, view);
    const inv = invert(mat4(), vp);
    expect(inv).not.toBeNull();
    const p: Vec3 = [12, -4, 30];
    const clip = transformPoint(vp, p);
    const ndc: Vec3 = [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]];
    const back = transformPoint(inv as Float32Array, ndc);
    const world: Vec3 = [back[0] / back[3], back[1] / back[3], back[2] / back[3]];
    world.forEach((n, i) => expect(n).toBeCloseTo(p[i], 3));
  });

  it("stays orthogonal when the up vector is parallel to the view", () => {
    // Looking straight down -Z with +Z up leaves no screen right to build on.
    const v = lookAt(mat4(), [0, 0, 0], [0, 0, -100], [0, 0, 1]);
    const xAxis: Vec3 = [v[0], v[1], v[2]];
    const yAxis: Vec3 = [v[4], v[5], v[6]];
    const zAxis: Vec3 = [v[8], v[9], v[10]];
    for (const axis of [xAxis, yAxis, zAxis]) expect(Math.hypot(axis[0], axis[1], axis[2])).toBeCloseTo(1, 5);
    expect(xAxis[0] * yAxis[0] + xAxis[1] * yAxis[1] + xAxis[2] * yAxis[2]).toBeCloseTo(0, 5);
  });
});

describe("camera views", () => {
  it("reports a fixed forward axis for each ortho view", () => {
    expect(viewAxis("top")).toEqual([0, 0, -1]);
    expect(viewAxis("front")).toEqual([0, 1, 0]);
    expect(viewAxis("side")).toEqual([1, 0, 0]);
    expect(viewAxis("persp")).toBeNull();
    expect(viewUp("top")).toEqual([0, 1, 0]);
    expect(viewUp("front")).toEqual([0, 0, 1]);
  });

  it("derives the perspective forward from the eye and target", () => {
    const s = defaultCamera();
    const f = viewForward(s);
    const want: Vec3 = [s.target[0] - s.eye[0], s.target[1] - s.eye[1], s.target[2] - s.eye[2]];
    const l = Math.hypot(want[0], want[1], want[2]);
    f.forEach((n, i) => expect(n).toBeCloseTo(want[i] / l, 6));
  });

  it("pulls the eye back along the view axis for ortho views", () => {
    const s = { ...defaultCamera(), kind: "top" as const, target: [10, 20, 30] as Vec3, distance: 500 };
    const eye = eyeFor(s);
    expect(eye).toEqual([10, 20, 530]);
  });

  it("switches projection with the view kind", () => {
    const persp = viewOf({ kind: "persp" });
    const top = viewOf({ kind: "top" });
    const a = persp.matrices();
    const b = top.matrices();
    // A perspective matrix has a non-zero w row; the ortho one does not.
    expect(Math.abs(a.proj[11])).toBeGreaterThan(0.5);
    expect(Math.abs(b.proj[11])).toBeLessThan(1e-6);
    expect(isPerspective("persp")).toBe(true);
    expect(isPerspective("top")).toBe(false);
  });

  it("recomputes when the aspect changes", () => {
    const camera = viewOf();
    const wide = camera.matrices().proj[0];
    camera.setAspect(1);
    expect(camera.matrices().proj[0]).not.toBeCloseTo(wide, 6);
    camera.setAspect(0);
    expect(Number.isFinite(camera.matrices().proj[0])).toBe(true);
  });
});

describe("pick rays", () => {
  it("aims a perspective ray from the eye through the cursor", () => {
    const s = { ...defaultCamera(), kind: "persp" as const, eye: [0, 0, 0] as Vec3, target: [0, 0, -100] as Vec3, fov: 90 };
    const camera = new Camera(s);
    camera.setAspect(1);
    const centre = camera.pickRay(500, 500, 1000, 1000);
    expect(centre.origin).toEqual([0, 0, 0]);
    expect(centre.dir[0]).toBeCloseTo(0, 5);
    expect(centre.dir[2]).toBeCloseTo(-1, 5);
    const right = camera.pickRay(1000, 500, 1000, 1000);
    expect(right.dir[0]).toBeGreaterThan(0.2);
    const up = camera.pickRay(500, 0, 1000, 1000);
    expect(up.dir[1]).toBeGreaterThan(0.2);
  });

  it("keeps ortho rays parallel and spaced by the frustum", () => {
    const s = { ...defaultCamera(), kind: "side" as const, target: [0, 0, 0] as Vec3, orthoHeight: 100, distance: 2000 };
    const camera = new Camera(s);
    camera.setAspect(2);
    const a = camera.pickRay(0, 0, 800, 400);
    const b = camera.pickRay(800, 400, 800, 400);
    const dot = a.dir[0] * b.dir[0] + a.dir[1] * b.dir[1] + a.dir[2] * b.dir[2];
    expect(dot).toBeCloseTo(1, 5);
    // Half the height is 100 units either side of the centre plane.
    const centre = camera.pickRay(400, 200, 800, 400);
    const edge = camera.pickRay(400, 0, 800, 400);
    const span = Math.hypot(edge.origin[1] - centre.origin[1], edge.origin[2] - centre.origin[2]);
    expect(span).toBeCloseTo(100, 3);
  });

  it("unprojects the centre of the screen back to the target", () => {
    const s = { ...defaultCamera(), kind: "persp" as const, eye: [0, 0, 500] as Vec3, target: [0, 0, 0] as Vec3, fov: 60 };
    const camera = new Camera(s);
    camera.setAspect(2);
    const ray = camera.pickRay(500, 400, 1000, 800);
    // The ray passes through the origin.
    const t = -(ray.origin[2] / ray.dir[2]);
    const hit: Vec3 = [ray.origin[0] + ray.dir[0] * t, ray.origin[1] + ray.dir[1] * t, ray.origin[2] + ray.dir[2] * t];
    hit.forEach((n) => expect(n).toBeCloseTo(0, 3));
  });
});

describe("frustum culling", () => {
  it("keeps what the camera looks at and drops what is behind it", () => {
    const s = { ...defaultCamera(), kind: "persp" as const, eye: [0, 0, 500] as Vec3, target: [0, 0, 0] as Vec3, fov: 60 };
    const camera = new Camera(s);
    camera.setAspect(1);
    const f = frustumFromViewProj(camera.matrices().viewProj);
    expect(aabbInFrustum(f, { min: [-10, -10, -10], max: [10, 10, 10] })).toBe(true);
    expect(aabbInFrustum(f, { min: [-10, -10, 900], max: [10, 10, 920] })).toBe(false);
    expect(aabbInFrustum(f, { min: [900, -10, -10], max: [920, 10, 10] })).toBe(false);
  });

  it("keeps a box that only pokes into the frustum", () => {
    const s = { ...defaultCamera(), kind: "persp" as const, eye: [0, 0, 500] as Vec3, target: [0, 0, 0] as Vec3, fov: 60 };
    const camera = new Camera(s);
    camera.setAspect(1);
    const f = frustumFromViewProj(camera.matrices().viewProj);
    expect(aabbInFrustum(f, { min: [-200, -200, -20], max: [-190, -190, -10] })).toBe(true);
  });

  it("tests spheres with their radius", () => {
    const s = { ...defaultCamera(), kind: "persp" as const, eye: [0, 0, 500] as Vec3, target: [0, 0, 0] as Vec3, fov: 60 };
    const camera = new Camera(s);
    camera.setAspect(1);
    const f = frustumFromViewProj(camera.matrices().viewProj);
    expect(sphereInFrustum(f, [0, 0, 0], 5)).toBe(true);
    expect(sphereInFrustum(f, [0, 0, 600], 5)).toBe(false);
    expect(sphereInFrustum(f, [0, 0, 495], 20)).toBe(true);
  });
});

describe("camera movement", () => {
  it("orbits around the target and keeps the distance", () => {
    const s = { ...defaultCamera(), kind: "persp" as const };
    const before = Math.hypot(s.eye[0] - s.target[0], s.eye[1] - s.target[1], s.eye[2] - s.target[2]);
    orbit(s, Math.PI / 2, 0);
    const after = Math.hypot(s.eye[0] - s.target[0], s.eye[1] - s.target[1], s.eye[2] - s.target[2]);
    expect(after).toBeCloseTo(before, 5);
    expect(s.distance).toBeCloseTo(before, 5);
  });

  it("clamps the orbit pitch so the eye never flips over", () => {
    const s = { ...defaultCamera(), kind: "persp" as const };
    orbit(s, 0, 10);
    const offset = s.eye[2] - s.target[2];
    const r = Math.hypot(s.eye[0] - s.target[0], s.eye[1] - s.target[1], offset);
    expect(Math.abs(offset / r)).toBeLessThanOrEqual(1.0001);
  });

  it("moves eye and target together when panning or flying", () => {
    const s = { ...defaultCamera(), kind: "persp" as const };
    const targetBefore = [...s.target];
    const eyeBefore = [...s.eye];
    pan(s, 100, 50);
    expect(s.target[0]).not.toBe(targetBefore[0]);
    expect(s.eye[0]).toBeCloseTo(eyeBefore[0] + (s.target[0] - targetBefore[0]), 5);
    const eyeMid = [...s.eye];
    fly(s, 200, 0, 0, 1);
    expect(Math.hypot(s.eye[0] - eyeMid[0], s.eye[1] - eyeMid[1], s.eye[2] - eyeMid[2])).toBeCloseTo(200, 3);
  });

  it("dollies without changing the direction", () => {
    const s = { ...defaultCamera(), kind: "persp" as const };
    const before = normalize3(sub3(s.target, s.eye));
    dolly(s, 500);
    const after = normalize3(sub3(s.target, s.eye));
    before.forEach((n, i) => expect(after[i]).toBeCloseTo(n, 5));
    expect(s.distance).toBeGreaterThan(0);
  });

  it("frames a bounding box at a distance that fits it", () => {
    const s = { ...defaultCamera(), kind: "persp" as const, fov: 90 };
    frameBounds(s, [-100, -100, -100], [100, 100, 100], 90);
    expect(s.target).toEqual([0, 0, 0]);
    const r = Math.hypot(s.eye[0], s.eye[1], s.eye[2]);
    expect(r).toBeGreaterThan(150);
  });

  it("scales drag distances with the zoom level", () => {
    const near = { ...defaultCamera(), kind: "top" as const, orthoHeight: 100 };
    const far = { ...defaultCamera(), kind: "top" as const, orthoHeight: 1000 };
    expect(panScale(near, 400)).toBeLessThan(panScale(far, 400));
    const persp = { ...defaultCamera(), kind: "persp" as const, distance: 1000, fov: 60 };
    expect(panScale(persp, 400)).toBeCloseTo((2 * 1000 * Math.tan(Math.PI / 6)) / 400, 6);
  });
});

function sub3(a: Vec3, b: Vec3): Vec3 {
  return [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
}

function normalize3(v: Vec3): Vec3 {
  const l = Math.hypot(v[0], v[1], v[2]) || 1;
  return [v[0] / l, v[1] / l, v[2] / l];
}

describe("unproject helper", () => {
  it("produces a normalized direction", () => {
    const m = invert(mat4(), perspective(mat4(), 1, 1, 1, 100));
    if (!m) throw new Error("invert failed");
    const { origin, dir } = unproject(m, 0, 0);
    expect(Math.hypot(dir[0], dir[1], dir[2])).toBeCloseTo(1, 6);
    expect(Number.isFinite(origin[0])).toBe(true);
  });
});

describe("framing in 2D views", () => {
  it("zooms an ortho view to the box as well as centring it", () => {
    const state = { ...defaultCamera(), kind: "top" as const, orthoHeight: 100000 };
    frameBounds(state, [0, 0, 0], [256, 256, 256], state.fov);
    expect(state.target).toEqual([128, 128, 128]);
    expect(state.orthoHeight).toBeGreaterThan(128);
    expect(state.orthoHeight).toBeLessThan(512);
  });
});

describe("zoom about a point", () => {
  it("keeps the point on screen and the view direction, and shrinks the orbit with the zoom", () => {
    const s = defaultCamera();
    const forward = viewForward(s);
    const eye0 = eyeFor(s);
    // A point off the view axis, as a cursor away from the centre gives.
    const point: Vec3 = [eye0[0] + forward[0] * 1000 + 50, eye0[1] + forward[1] * 1000, eye0[2] + forward[2] * 1000 - 30];
    const dirTo = (): Vec3 => {
      const e = eyeFor(s);
      const d: Vec3 = [point[0] - e[0], point[1] - e[1], point[2] - e[2]];
      const l = Math.hypot(...d);
      return [d[0] / l, d[1] / l, d[2] / l];
    };
    const before = dirTo();
    const distance = s.distance;
    zoomAbout(s, point, 0.8);
    const after = dirTo();
    for (let k = 0; k < 3; k++) expect(after[k]).toBeCloseTo(before[k], 9);
    expect(s.distance).toBeCloseTo(distance * 0.8, 6);
    const f = viewForward(s);
    for (let k = 0; k < 3; k++) expect(f[k]).toBeCloseTo(forward[k], 9);
    for (let i = 0; i < 200; i++) zoomAbout(s, point, 0.8);
    expect(s.distance).toBeGreaterThanOrEqual(1);
  });

  it("over empty sky it is a plain dolly about the target", () => {
    const s = defaultCamera();
    const target = [...s.target] as Vec3;
    const distance = s.distance;
    zoomAbout(s, s.target, 1.25);
    expect(s.target).toEqual(target);
    expect(s.distance).toBeCloseTo(distance * 1.25, 6);
  });
});

describe("slide", () => {
  it("moves eye and target the same fixed distance, through anything", () => {
    const s = defaultCamera();
    const eye = eyeFor(s);
    const target = [...s.target] as Vec3;
    const distance = s.distance;
    slide(s, [0, 0, 2], 64);
    expect(eyeFor(s)[2]).toBeCloseTo(eye[2] + 64, 6);
    expect(s.target[2]).toBeCloseTo(target[2] + 64, 6);
    expect(s.distance).toBe(distance);
    slide(s, [0, 0, 1], -64);
    expect(eyeFor(s)[2]).toBeCloseTo(eye[2], 6);
  });
});

describe("blender views", () => {
  it("snaps to an axis view in ortho and back to perspective on orbit", () => {
    const s = { ...defaultCamera() };
    setAxisView(s, "top");
    expect(isPerspectiveState(s)).toBe(false);
    expect(viewForward(s)).toEqual([0, 0, -1]);
    expect(viewLabel(s)).toBe("Top Orthographic");
    orbit(s, 0, -0.2);
    expect(s.kind).toBe("persp");
    expect(isPerspectiveState(s)).toBe(true);
    // Leaving the top view leans toward the front, not a random yaw.
    expect(eyeFor(s)[1]).toBeLessThan(s.target[1]);
  });

  it("keeps an explicit ortho when orbiting", () => {
    const s = { ...defaultCamera() };
    setOrtho(s, true);
    orbit(s, 0.3, 0);
    expect(isPerspectiveState(s)).toBe(false);
    expect(viewLabel(s)).toBe("User Orthographic");
  });

  it("keeps the on-screen size across a projection toggle", () => {
    const s = { ...defaultCamera(), distance: 1000, fov: 60 };
    s.eye = add(s.target, scale(normalize(sub(s.eye, s.target)), 1000));
    const before = panScale(s, 800);
    setOrtho(s, true);
    expect(panScale(s, 800)).toBeCloseTo(before, 6);
    setOrtho(s, false);
    expect(panScale(s, 800)).toBeCloseTo(before, 6);
  });

  it("flips to the opposite axis view", () => {
    const s = { ...defaultCamera() };
    setAxisView(s, "front");
    flipView(s);
    expect(s.kind).toBe("back");
    expect(viewForward(s)).toEqual([0, -1, 0]);
    setAxisView(s, "right");
    expect(viewForward(s)).toEqual([-1, 0, 0]);
  });

  it("zooms ortho about a point that stays put on screen", () => {
    const s = { ...defaultCamera() };
    setAxisView(s, "top");
    s.orthoHeight = 100;
    const point: Vec3 = [50, 0, 0];
    zoomOrtho(s, 0.5, point);
    expect(s.orthoHeight).toBe(50);
    expect(s.target[0]).toBeCloseTo(25, 6);
  });
});
