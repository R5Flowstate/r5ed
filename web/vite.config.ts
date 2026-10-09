import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  // Vite's own build summary draws U+2713 and U+2502, which a parent process
  // that captures stdout and replays it on a legacy code page cannot encode.
  // scripts/size.mjs prints the asset table in plain ASCII instead.
  logLevel: "warn",
  build: {
    target: "esnext",
    outDir: "dist",
    emptyOutDir: true,
    assetsInlineLimit: 0,
    rollupOptions: { output: { manualChunks: undefined } },
  },
  server: { port: 5273, proxy: { "/api": "http://127.0.0.1:7797" } },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx", "src/**/*.test.mjs"],
    // Keeps stdout inside cp1252 so a parent process that captures and
    // replays it on a legacy console code page does not choke on the
    // default reporter's U+2713 check marks.
    reporters: ["./scripts/ascii-reporter.mjs"],
  },
});
