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

// Single source of truth: ../data-files.json lists every shared JSON the
// frontend fetches at runtime. Generate one `__<NAME>_URL__` define per
// filename so adding a new file is one line in data-files.json + one line
// in vite-env.d.ts. The same manifest drives the upload + verify loops in
// .github/workflows/{deploy,release}.yml — see those files.
//
// Resolution: at deploy time, set DATA_BASE_URL to the R2 prefix; defines
// resolve to `${BASE}/<filename>`. Local dev with no env defaults to
// site-root paths.
//
// `__CARD_DATA_URL__` is NOT manifest-driven — the WASM bundle is pinned to
// a content-addressed `card-data-<hash>.json` URL via CARD_DATA_URL at build
// time (see release.yml / deploy.yml). That hashed file lives on R2 only;
// uploading an additional non-hashed `card-data.json` to R2 would be dead
// weight since no frontend code fetches it. Local dev falls back to the
// public/ copy served at `/card-data.json` (also used by Tauri bundles and
// phase-server via `data/card-data.json`).
function dataFileDefines(): Record<string, string> {
  const manifest = JSON.parse(
    readFileSync(path.resolve(__dirname, "../data-files.json"), "utf-8"),
  ) as string[];
  const base = process.env.DATA_BASE_URL || "";
  const defines: Record<string, string> = {
    __APP_VERSION__: JSON.stringify(workspaceVersion()),
    __BUILD_HASH__: JSON.stringify(gitHash()),
    __AUDIO_BASE_URL__: JSON.stringify(process.env.AUDIO_BASE_URL || ""),
    __GIT_REPO_URL__: JSON.stringify("https://github.com/phase-rs/phase"),
    __CARD_DATA_URL__: JSON.stringify(process.env.CARD_DATA_URL || "/card-data.json"),
  };
  for (const filename of manifest) {
    // "card-names.json" → "__CARD_NAMES_URL__"
    const token = `__${filename.replace(/\.json$/, "").replace(/-/g, "_").toUpperCase()}_URL__`;
    defines[token] = JSON.stringify(`${base}/${filename}`);
  }
  return defines;
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
        globIgnores: ["**/engine_wasm_bg-*.wasm"],
        runtimeCaching: [
          {
            urlPattern: /engine_wasm_bg-.*\.wasm$/,
            handler: "CacheFirst",
            options: {
              cacheName: "engine-wasm",
              expiration: { maxEntries: 2, maxAgeSeconds: 2592000 },
            },
          },
          {
            urlPattern: /card-data\.json$/,
            handler: "StaleWhileRevalidate",
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
  define: dataFileDefines(),
  worker: {
    plugins: () => [wasmEnvShim()],
  },
  build: {
    target: "esnext",
  },
});
