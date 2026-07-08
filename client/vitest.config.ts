import path from "node:path";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";

/**
 * Resolves the @wasm/* aliases to the real WASM build artifacts when present,
 * otherwise to a virtual empty module. Vitest does not inherit vite.config.ts
 * aliases, and the artifacts are gitignored (absent on CI), so without this
 * any test whose import graph reaches a `import("@wasm/...")` fails at
 * transform time. The stub also lets vi.mock("@wasm/engine", factory) work.
 */
function wasmStubPlugin(): Plugin {
  const artifacts: Record<string, string> = {
    "@wasm/engine": path.resolve(__dirname, "src/wasm/engine_wasm.js"),
    "@wasm/draft": path.resolve(__dirname, "src/wasm/draft_wasm.js"),
  };
  return {
    name: "wasm-stub",
    enforce: "pre",
    async resolveId(id) {
      const artifact = artifacts[id];
      if (!artifact) return;
      try {
        await import("node:fs/promises").then((fs) => fs.access(artifact));
        return artifact;
      } catch {
        return `\0${id}-stub`;
      }
    },
    load(id) {
      if (id.startsWith("\0@wasm/") && id.endsWith("-stub")) {
        return "export default function init() {}";
      }
    },
  };
}

export default defineConfig({
  plugins: [wasmStubPlugin()],
  define: {
    __SCRYFALL_DATA_URL__: JSON.stringify("/scryfall-data.json"),
    __SCRYFALL_TOKEN_IMAGES_URL__: JSON.stringify("/scryfall-token-images.json"),
    __SCRYFALL_PRINTINGS_URL__: JSON.stringify("/scryfall-printings.json"),
    __SCRYFALL_SETS_URL__: JSON.stringify("/scryfall-sets.json"),
    __DECKS_URL__: JSON.stringify("/decks.json"),
    __CARD_DATA_URL__: JSON.stringify("/card-data.json"),
    __CARD_DATA_LOCALE_URL_TEMPLATE__: JSON.stringify("/card-data.{lng}.json"),
    __CHANGELOG_URL__: JSON.stringify("/changelog.json"),
    __CHANGELOG_META_URL__: JSON.stringify("/changelog-meta.json"),
    __APP_VERSION__: JSON.stringify("0.0.0-test"),
    __BUILD_HASH__: JSON.stringify("testhash"),
    __DEFAULT_MULTIPLAYER_SERVER_URL__: JSON.stringify(
      process.env.DEFAULT_MULTIPLAYER_SERVER_URL || "wss://lobby.phase-rs.dev/ws",
    ),
    __GIT_REPO_URL__: JSON.stringify("https://github.com/phase-rs/phase"),
    __IS_RELEASE_BUILD__: JSON.stringify(false),
    // Empty ⇒ telemetry is build-disabled in tests (no network egress).
    __TELEMETRY_URL__: JSON.stringify(""),
  },
  test: {
    environment: "happy-dom",
    include: ["src/**/*.test.{ts,tsx}"],
    exclude: ["src/**/*.integration.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
    pool: "threads",
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
