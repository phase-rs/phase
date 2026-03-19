import path from "node:path";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";

/**
 * Resolves @wasm/engine to the real WASM build artifact when present,
 * otherwise to a virtual empty module. This allows vi.mock("@wasm/engine", factory)
 * to work on CI where WASM build artifacts are absent (gitignored).
 */
function wasmStubPlugin(): Plugin {
  const resolved = path.resolve(__dirname, "src/wasm/engine_wasm.js");
  const virtualId = "\0@wasm/engine-stub";
  return {
    name: "wasm-stub",
    enforce: "pre",
    async resolveId(id) {
      if (id === "@wasm/engine") {
        try {
          await import("node:fs/promises").then((fs) => fs.access(resolved));
          return resolved;
        } catch {
          return virtualId;
        }
      }
    },
    load(id) {
      if (id === virtualId) {
        return "export default function init() {}";
      }
    },
  };
}

export default defineConfig({
  plugins: [wasmStubPlugin()],
  test: {
    environment: "happy-dom",
    include: ["src/**/*.test.{ts,tsx}"],
    exclude: ["src/**/*.integration.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
    pool: "threads",
    poolOptions: {
      threads: {
        singleThread: false,
      },
    },
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov"],
      include: ["src/**/*.{ts,tsx}"],
      exclude: ["src/**/__tests__/**", "src/**/*.test.*", "src/wasm/**"],
      thresholds: {
        lines: 10,
        functions: 10,
      },
    },
  },
});
