import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import wasm from "vite-plugin-wasm";
import topLevelAwait from "vite-plugin-top-level-await";
import { VitePWA } from "vite-plugin-pwa";
import { compression } from "vite-plugin-compression2";
import type { Plugin } from "vite";

// wasm-bindgen emits `import * as importN from "env"` for WASM host-environment
// imports (LLVM intrinsics). These are provided at instantiation time by the JS
// glue code and are never loaded as ES modules. Resolve them to an empty shim
// so Vite's import analysis doesn't error on the bare "env" specifier.
function wasmEnvShim(): Plugin {
  const VIRTUAL_ID = "\0wasm-env-shim";
  return {
    name: "wasm-env-shim",
    enforce: "pre",
    resolveId(id) {
      if (id === "env") return VIRTUAL_ID;
    },
    load(id) {
      if (id === VIRTUAL_ID) return "export default {};";
    },
  };
}

function gitHash(): string {
  try {
    return execSync("git rev-parse --short HEAD").toString().trim();
  } catch {
    return "dev";
  }
}

function workspaceVersion(): string {
  try {
    const toml = readFileSync(path.resolve(__dirname, "../Cargo.toml"), "utf-8");
    const match = toml.match(/^version\s*=\s*"([^"]+)"/m);
    return match?.[1] ?? "0.0.0";
  } catch {
    return "0.0.0";
  }
}

export default defineConfig({
  resolve: {
    alias: {
      "@wasm/engine": path.resolve(__dirname, "src/wasm/engine_wasm"),
    },
  },
  plugins: [
    wasmEnvShim(),
    react(),
    tailwindcss(),
    wasm(),
    topLevelAwait(),
    VitePWA({
      registerType: "autoUpdate",
      manifest: false, // Use public/manifest.json
      includeAssets: ["**/*.mp3", "**/*.m4a"],
      workbox: {
        maximumFileSizeToCacheInBytes: 15 * 1024 * 1024,
        runtimeCaching: [
          {
            urlPattern: /card-data\.json$/,
            handler: "CacheFirst",
            options: {
              cacheName: "card-database",
              expiration: { maxEntries: 1, maxAgeSeconds: 604800 },
            },
          },
          {
            urlPattern: /^https:\/\/pub-fc5b5c2c6e774356ae3e730bb0326394\.r2\.dev\/audio\//,
            handler: "CacheFirst",
            options: {
              cacheName: "audio-r2",
              expiration: { maxEntries: 50, maxAgeSeconds: 2592000 },
            },
          },
        ],
      },
    }),
    compression({ algorithms: ["brotliCompress"] }),
  ],
  define: {
    __APP_VERSION__: JSON.stringify(workspaceVersion()),
    __BUILD_HASH__: JSON.stringify(gitHash()),
    __CARD_DATA_URL__: JSON.stringify(
      process.env.CARD_DATA_URL || "/card-data.json",
    ),
    __COVERAGE_DATA_URL__: JSON.stringify(
      process.env.COVERAGE_DATA_URL || "/coverage-data.json",
    ),
    __COVERAGE_SUMMARY_URL__: JSON.stringify(
      process.env.COVERAGE_SUMMARY_URL || "/coverage-summary.json",
    ),
    __CARD_DATA_META_URL__: JSON.stringify(
      process.env.CARD_DATA_META_URL || "/card-data-meta.json",
    ),
    __AUDIO_BASE_URL__: JSON.stringify(
      process.env.AUDIO_BASE_URL || "",
    ),
    __GIT_REPO_URL__: JSON.stringify("https://github.com/phase-rs/phase"),
  },
  worker: {
    plugins: () => [wasmEnvShim()],
  },
  build: {
    target: "esnext",
  },
});
