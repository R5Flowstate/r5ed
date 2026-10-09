// Inputs for the importers, copied from the core's own importer tests, and a
// one-triangle glb and a small PNG built in code so no binary fixture is checked in.

import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

const VMF = `
VersionInfo
{
    "editorversion" "400"
}
world
{
    "id" "1"
    "classname" "worldspawn"
    "solidcolor" "255 0 0"
    side
    {
        "id" "2"
        "plane" "(0 0 1) (0 1 0) (0 0 0)"
        "material" "BRICK/BRICKWALL"
    }
}
brush
{
    "id" "10"
    side
    {
        "id" "11"
        "plane" "(-64 -64 0) (-64 64 0) (64 64 0)"
        "material" "CONCRETE/BRICK"
    }
    side
    {
        "id" "12"
        "plane" "(-64 -64 128) (64 -64 128) (64 64 128)"
        "material" "CONCRETE/BRICK"
        "uaxis" "[1 0 0 0] 0.25"
        "vaxis" "[0 -1 0 0] 0.25"
        "rotation" "0"
        "lightmapscale" "16"
    }
    side
    {
        "id" "13"
        "plane" "(-64 -64 0) (-64 -64 128) (-64 64 128)"
    }
    side
    {
        "id" "14"
        "plane" "(64 -64 0) (64 64 0) (64 64 128)"
    }
    side
    {
        "id" "15"
        "plane" "(-64 -64 0) (64 -64 0) (64 -64 128)"
    }
    side
    {
        "id" "16"
        "plane" "(-64 64 0) (-64 64 128) (64 64 128)"
    }
}
brush
{
    "id" "20"
    side { "plane" "(0 0 0) (0 128 0) (128 128 0)" }
    side { "plane" "(0 0 64) (128 0 64) (128 128 64)" }
    side { "plane" "(0 0 0) (0 0 64) (0 128 64)" }
    side { "plane" "(128 0 0) (128 128 0) (128 128 64)" }
    side { "plane" "(0 0 0) (128 0 0) (128 0 64)" }
    side { "plane" "(0 128 0) (0 128 64) (128 128 64)" }
}
entity
{
    "id" "30"
    "classname" "func_door"
    "origin" "256 256 0"
    "targetname" "door_main"
    brush
    {
        "id" "31"
        side { "plane" "(0 0 0) (0 64 0) (64 64 0)" }
        side { "plane" "(0 0 32) (64 0 32) (64 64 32)" }
        side { "plane" "(0 0 0) (0 0 32) (0 64 32)" }
        side { "plane" "(64 0 0) (64 64 0) (64 64 32)" }
        side { "plane" "(0 0 0) (64 0 0) (64 0 32)" }
        side { "plane" "(0 64 0) (0 64 32) (64 64 32)" }
    }
}
entity
{
    "id" "40"
    "classname" "info_target"
    "origin" "0 0 200"
    "targetname" "marker"
}
`;

const QUAKE = `
// a comment
{
    "classname" "worldspawn"
    "wad" "test.wad"
    {
        ( -64 -64 -64 ) ( -64 -63 -64 ) ( -63 -64 -64 ) SKY 0 0 0 1 1
        ( -64 -64 -64 ) ( -63 -64 -64 ) ( -63 -63 -64 ) SKY 0 0 0 1 1
        ( -64 -64 -64 ) ( -64 -63 -64 ) ( -64 -63 -63 ) SKY 0 0 0 1 1
        ( 64 64 64 ) ( 64 63 64 ) ( 63 64 64 ) SKY 0 0 0 1 1
        ( 64 64 64 ) ( 63 64 64 ) ( 63 63 64 ) SKY 0 0 0 1 1
        ( 64 64 64 ) ( 64 63 64 ) ( 64 63 63 ) SKY 0 0 0 1 1
    }
}
{
    "classname" "func_door"
    "origin" "0 0 0"
    "targetname" "door"
    {
        ( 0 0 0 ) ( 0 128 0 ) ( 128 128 0 ) METAL 16 32 0 0.5 0.25
        ( 0 0 64 ) ( 128 0 64 ) ( 128 128 64 ) METAL 16 32 0 0.5 0.25
        ( 0 0 0 ) ( 0 0 64 ) ( 0 128 64 ) METAL 16 32 0 0.5 0.25
        ( 128 0 0 ) ( 128 128 0 ) ( 128 128 64 ) METAL 16 32 0 0.5 0.25
        ( 0 0 0 ) ( 128 0 0 ) ( 128 0 64 ) METAL 16 32 0 0.5 0.25
        ( 0 128 0 ) ( 0 128 64 ) ( 128 128 64 ) METAL 16 32 0 0.5 0.25
    }
}
{
    "classname" "trigger_multiple"
    "origin" "256 0 0"
    "spawnflags" "1"
    {
        ( 0 0 0 ) ( 0 64 0 ) ( 64 64 0 ) TRIGGER 0 0 0 1 1
        ( 0 0 32 ) ( 64 0 32 ) ( 64 64 32 ) TRIGGER 0 0 0 1 1
        ( 0 0 0 ) ( 0 0 32 ) ( 0 64 32 ) TRIGGER 0 0 0 1 1
        ( 64 0 0 ) ( 64 64 0 ) ( 64 64 32 ) TRIGGER 0 0 0 1 1
        ( 0 0 0 ) ( 64 0 0 ) ( 64 0 32 ) TRIGGER 0 0 0 1 1
        ( 0 64 0 ) ( 0 64 32 ) ( 64 64 32 ) TRIGGER 0 0 0 1 1
    }
}
{
    "classname" "info_target"
    "targetname" "marker"
    "origin" "0 0 512"
}
`;

const REMAP = `{
 "schemaVersion": 1,
 "name": "duel_plateau",
 "coordinateSystem": "unity-y-up-meters",
 "gameTarget": "r5flowstate",
 "editingMap": "mp_rr_canyonlands_staging_mu1",
 "targetMaps": ["mp_rr_canyonlands_staging_mu1"],
 "originOffset": { "x": 0, "y": 0, "z": 0 },
 "objects": [
  {
   "id": "000000000000000000000000000000a8",
   "displayName": "pad_00",
   "parentId": "",
   "isGroup": false,
   "gameModelPath": "mdl/canyonlands/firingrange_mu1_vert_course_platform_01.rmdl",
   "customType": "",
   "position": { "x": -13.0048, "y": -2.105025, "z": 19.5072 },
   "rotation": { "x": 0.0, "y": -0.0, "z": 0.0 },
   "scale": { "x": 1.0, "y": 1.0, "z": 1.0 },
   "fadeDistance": 50000.0,
   "realmId": -1,
   "allowMantle": true,
   "solid": true,
   "x-headless": { "posApex": [-512.0, 768.0, -82.875], "angApex": [0.0, 0.0, 0.0] }
  },
  {
   "id": "000000000000000000000000000000a9",
   "displayName": "desks",
   "parentId": "",
   "isGroup": true,
   "gameModelPath": "",
   "customType": "",
   "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
   "rotation": { "x": 0.0, "y": 0.0, "z": 0.0 },
   "scale": { "x": 1.0, "y": 1.0, "z": 1.0 }
  },
  {
   "id": "000000000000000000000000000000aa",
   "displayName": "chair",
   "parentId": "000000000000000000000000000000a9",
   "isGroup": false,
   "gameModelPath": "mdl/props/canyonlands/chair_01.rmdl",
   "customType": "",
   "position": { "x": 1.0, "y": 0.0, "z": 0.0 },
   "rotation": { "x": 0.0, "y": 0.0, "z": 90.0 },
   "scale": { "x": 1.0, "y": 1.0, "z": 1.0 }
  },
  {
   "id": "000000000000000000000000000000ab",
   "displayName": "cable",
   "parentId": "",
   "isGroup": true,
   "customType": "zipline",
   "ziplineStartId": "000000000000000000000000000000ac",
   "ziplineEndId": "000000000000000000000000000000ad",
   "ziplineMode": "vertical",
   "ziplineWidth": 2.0,
   "ziplineSpeed": 1.0,
   "ziplineLengthScale": 1.0,
   "ziplineFadeDistance": -1.0,
   "ziplineDropToBottom": true,
   "ziplinePreserveVelocity": false,
   "position": { "x": 0.0, "y": 0.0, "z": 0.0 }
  },
  {
   "id": "000000000000000000000000000000ac",
   "displayName": "cable_start",
   "parentId": "000000000000000000000000000000ab",
   "customType": "zipline-endpoint",
   "customRole": "start",
   "position": { "x": 0.0, "y": 0.0, "z": 3.0 }
  },
  {
   "id": "000000000000000000000000000000ad",
   "displayName": "cable_end",
   "parentId": "000000000000000000000000000000ab",
   "customType": "zipline-endpoint",
   "customRole": "end",
   "position": { "x": 0.0, "y": 0.0, "z": 8.0 }
  },
  {
   "id": "000000000000000000000000000000ae",
   "displayName": "hotzone",
   "parentId": "",
   "isGroup": true,
   "customType": "trigger",
   "triggerRadius": 45.0,
   "triggerHalfHeight": 50.0,
   "position": { "x": 2.54, "y": 0.0, "z": 0.0 }
  },
  {
   "id": "000000000000000000000000000000af",
   "displayName": "launcher",
   "parentId": "",
   "isGroup": false,
   "gameModelPath": "mdl/props/octane_jump_pad/octane_jump_pad.rmdl",
   "customType": "jump-pad",
   "jumpPadLaunchVelocity": 1000.0,
   "jumpPadRadius": 45.0,
   "jumpPadDoubleJump": true,
   "position": { "x": 5.08, "y": 0.0, "z": 0.0 }
  },
  {
   "id": "000000000000000000000000000000b0",
   "displayName": "gate",
   "parentId": "",
   "isGroup": true,
   "customType": "door",
   "doorType": "double",
   "doorGold": true,
   "gameModelPath": "mdl/props/cabinets/industrial1/cabinet1_door.rmdl",
   "position": { "x": 7.62, "y": 0.0, "z": 0.0 }
  },
  {
   "id": "000000000000000000000000000000b1",
   "displayName": "human_spawn",
   "parentId": "",
   "isGroup": false,
   "customType": "spawn-point",
   "spawnPointTeam": 1,
   "position": { "x": 10.16, "y": 0.0, "z": 0.0 }
  }
 ]
}`;

/** A minimal glTF 2.0 binary: one triangle, positions and indices. */
function glb() {
  const positions = new Float32Array([0, 0, 0, 64, 0, 0, 0, 64, 0]);
  const indices = new Uint16Array([0, 1, 2, 0]);
  const bin = Buffer.concat([Buffer.from(positions.buffer), Buffer.from(indices.buffer)]);
  const json = {
    asset: { version: "2.0" },
    scene: 0,
    scenes: [{ nodes: [0] }],
    nodes: [{ mesh: 0 }],
    meshes: [{ primitives: [{ attributes: { POSITION: 0 }, indices: 1 }] }],
    buffers: [{ byteLength: bin.length }],
    bufferViews: [
      { buffer: 0, byteOffset: 0, byteLength: 36, target: 34962 },
      { buffer: 0, byteOffset: 36, byteLength: 6, target: 34963 },
    ],
    accessors: [
      { bufferView: 0, componentType: 5126, count: 3, type: "VEC3", min: [0, 0, 0], max: [64, 64, 0] },
      { bufferView: 1, componentType: 5123, count: 3, type: "SCALAR" },
    ],
  };
  let text = Buffer.from(JSON.stringify(json));
  text = Buffer.concat([text, Buffer.alloc((4 - (text.length % 4)) % 4, 0x20)]);
  const header = Buffer.alloc(12);
  header.writeUInt32LE(0x46546c67, 0);
  header.writeUInt32LE(2, 4);
  header.writeUInt32LE(12 + 8 + text.length + 8 + bin.length, 8);
  const chunk = (type, body) => {
    const h = Buffer.alloc(8);
    h.writeUInt32LE(body.length, 0);
    h.writeUInt32LE(type, 4);
    return Buffer.concat([h, body]);
  };
  return Buffer.concat([header, chunk(0x4e4f534a, text), chunk(0x004e4942, bin)]);
}

/** A 4x4 greyscale PNG ramp, for the heightmap generator. */
function png() {
  const zlib = require("node:zlib");
  const w = 4;
  const h = 4;
  const raw = Buffer.alloc((w + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (w + 1)] = 0;
    for (let x = 0; x < w; x++) raw[y * (w + 1) + 1 + x] = (x + y) * 32;
  }
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf) => {
    let c = 0xffffffff;
    for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type, body) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(body.length);
    const tb = Buffer.concat([Buffer.from(type, "ascii"), body]);
    const c = Buffer.alloc(4);
    c.writeUInt32BE(crc(tb));
    return Buffer.concat([len, tb, c]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 0;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

export const FIXTURES = {
  text: { vmf: VMF, quake: QUAKE, remap: REMAP },
  glb,
  png,
};
