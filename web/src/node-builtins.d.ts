// Ambient declarations for the Node built-ins the test suite uses, so the web
// code itself needs no Node typings. Nothing here is bundled.
declare module "node:zlib" {
  export interface ZlibOptions {
    level?: number;
  }
  export function deflateSync(data: Uint8Array, options?: ZlibOptions): Uint8Array;
}
