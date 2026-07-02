import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { defineConfig, loadEnv } from "vite";
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
function dataFileDefines(mode: string): Record<string, string> {
  const manifest = JSON.parse(
    readFileSync(path.resolve(__dirname, "../data-files.json"), "utf-8"),
  ) as string[];
  // Bridge a gitignored repo-root .env into build-time defines for local dev.
  // Vite does not auto-populate process.env from .env files, so without this the
  // __SUPABASE_*__ tokens would never resolve from a .env. CI/deploy sets these
  // as real env vars, which take precedence over any .env entry.
  const fileEnv = loadEnv(mode, path.resolve(__dirname, ".."), "");
  const envVar = (name: string): string =>
    process.env[name] ?? fileEnv[name] ?? "";
  const base = process.env.DATA_BASE_URL || "";
  const defines: Record<string, string> = {
    __APP_VERSION__: JSON.stringify(workspaceVersion()),
    __BUILD_HASH__: JSON.stringify(gitHash()),
    __AUDIO_BASE_URL__: JSON.stringify(process.env.AUDIO_BASE_URL || ""),
    __GIT_REPO_URL__: JSON.stringify("https://github.com/phase-rs/phase"),
    __PREVIEW_SITE_URL__: JSON.stringify("https://preview.phase-rs.dev"),
    // True only for tagged production releases (release.yml sets RELEASE_BUILD).
    // The staging deploy (deploy.yml) is also a production Vite build, so we
    // cannot key off import.meta.env.PROD — that would surface the "try the
    // preview" link on the preview site itself. dev + staging → false (hidden);
    // tagged release → true (shown).
    __IS_RELEASE_BUILD__: JSON.stringify(process.env.RELEASE_BUILD === "true"),
    // Supabase cloud-sync config. Anon key is public by design (RLS is the
    // access control), so it ships in the bundle. Empty when unset → cloud sync
    // is disabled, leaving file backup as the only data-portability path. This
    // keeps self-hosted builds working with no Supabase account.
    __SUPABASE_URL__: JSON.stringify(envVar("SUPABASE_URL")),
    __SUPABASE_ANON_KEY__: JSON.stringify(envVar("SUPABASE_ANON_KEY")),
    __CARD_DATA_URL__: JSON.stringify(process.env.CARD_DATA_URL || "/card-data.json"),
    // Per-locale content-i18n sidecar URL template ({lng} replaced at runtime).
    // The sidecars are listed in data-files.json, so on deploy they are uploaded
    // to `${DATA_BASE_URL}/card-data.<lng>.json` and stripped from the Pages
    // bundle — this template must point there, mirroring the manifest files. With
    // no DATA_BASE_URL (local dev, Tauri offline) it resolves to the site-root
    // copy in public/. A missing sidecar (404) degrades to English per-field
    // (see ensureCardLocale). An explicit env override still wins.
    __CARD_DATA_LOCALE_URL_TEMPLATE__: JSON.stringify(
      process.env.CARD_DATA_LOCALE_URL_TEMPLATE ||
        (base ? `${base}/card-data.{lng}.json` : "/card-data.{lng}.json"),
    ),
  };
  for (const filename of manifest) {
    // "card-names.json" → "__CARD_NAMES_URL__"; "card-data.de.json" →
    // "__CARD_DATA_DE_URL__". Collapse both "-" and "." so dotted locale
    // sidecars don't yield a dotted (member-expression) define key. The
    // content-i18n code reads these via the {lng} template above, not the
    // per-file token, but every manifest entry still gets a valid token.
    const token = `__${filename.replace(/\.json$/, "").replace(/[.-]/g, "_").toUpperCase()}_URL__`;
    defines[token] = JSON.stringify(`${base}/${filename}`);
  }
  return defines;
}

export default defineConfig(({ mode }) => ({
  resolve: {
    alias: {
      "@wasm/engine": path.resolve(__dirname, "src/wasm/engine_wasm"),
      "@wasm/draft": path.resolve(__dirname, "src/wasm/draft_wasm"),
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
        // changelog{,-meta}.json are committed to public/ but stripped from the
        // Pages bundle on deploy (manifest-driven rm in deploy.yml/release.yml)
        // and served from R2. The precache manifest is generated BEFORE that
        // strip, so without ignoring them Workbox would precache files that 404
        // at SW-install time. They're fetched at runtime via the rules below.
        globIgnores: [
          "**/engine_wasm_bg-*.wasm",
          "**/draft_wasm_bg-*.wasm",
          "**/changelog.json",
          "**/changelog-meta.json",
        ],
        runtimeCaching: [
          {
            // Tiny constant-size "what's new" pointer fetched on every app load
            // to drive the unread dot. StaleWhileRevalidate serves the cached
            // copy instantly (and offline) while refreshing in the background,
            // so the dot reflects the latest deploy on the NEXT load — never
            // blocking startup on a network round-trip.
            urlPattern: /changelog-meta\.json$/,
            handler: "StaleWhileRevalidate",
            options: {
              cacheName: "changelog-meta",
              expiration: { maxEntries: 1, maxAgeSeconds: 2592000 },
            },
          },
          {
            // Full changelog, lazy-loaded only when the user opens the modal.
            // NetworkFirst prefers fresh entries but falls back to the cached
            // copy after a short timeout (slow network) or when offline, so the
            // modal always renders the last-seen list rather than hanging.
            urlPattern: /changelog\.json$/,
            handler: "NetworkFirst",
            options: {
              cacheName: "changelog-full",
              networkTimeoutSeconds: 3,
              expiration: { maxEntries: 1, maxAgeSeconds: 2592000 },
            },
          },
          {
            urlPattern: /engine_wasm_bg-.*\.wasm$/,
            handler: "CacheFirst",
            options: {
              cacheName: "engine-wasm",
              expiration: { maxEntries: 2, maxAgeSeconds: 2592000 },
            },
          },
          {
            urlPattern: /draft_wasm_bg-.*\.wasm$/,
            handler: "CacheFirst",
            options: {
              cacheName: "draft-wasm",
              expiration: { maxEntries: 2, maxAgeSeconds: 2592000 },
            },
          },
          {
            // Production publishes card data as a content-addressed
            // `card-data-<hash>.json` on R2 (see deploy.yml); local dev and
            // Tauri serve a plain `card-data.json`. Match both — the earlier
            // `/card-data\.json$/` pattern silently missed the hashed
            // production URL, so the SW never cached the card database.
            // Content addressing makes the file immutable: `CacheFirst` is
            // correct, mirroring the hashed WASM-bundle rules above.
            urlPattern: /card-data(-[0-9a-f]+)?\.json$/,
            handler: "CacheFirst",
            options: {
              cacheName: "card-database",
              expiration: { maxEntries: 1, maxAgeSeconds: 2592000 },
            },
          },
          {
            // Per-locale content-i18n sidecars (`card-data.<lng>.json`) fetched
            // from R2 (or public/ in dev/Tauri). The card-database pattern above
            // does NOT match the `.<lng>.` infix, so these need their own rule.
            // They are mutable (regenerated each deploy), so StaleWhileRevalidate
            // serves the cached copy instantly — and offline — while refreshing
            // in the background, giving non-English PWA users offline card text.
            urlPattern: /card-data\.[a-z]{2}\.json$/,
            handler: "StaleWhileRevalidate",
            options: {
              cacheName: "card-locale-sidecars",
              expiration: { maxEntries: 6, maxAgeSeconds: 2592000 },
            },
          },
          {
            urlPattern: /^https:\/\/data\.phase-rs\.dev\/audio\//,
            handler: "CacheFirst",
            options: {
              cacheName: "audio-r2",
              expiration: { maxEntries: 50, maxAgeSeconds: 2592000 },
            },
          },
          {
            // Remaining data-manifest JSONs (Scryfall lookup maps, precon
            // decks, draft pools, coverage, set metadata) — served from R2 in
            // production, site-root in dev/Tauri; the pattern matches both.
            // R2 serves `max-age=60, must-revalidate` with ETags, so the
            // StaleWhileRevalidate background refresh is a 304 revalidation,
            // not a re-download — the cached copy serves instantly and
            // offline, mirroring the card-locale-sidecars reasoning. This keeps
            // the image-URL lookup layer (scryfall-data.json) available offline.
            urlPattern:
              /\/(scryfall-data|scryfall-printings|scryfall-token-images|scryfall-sets|card-names|card-data-meta|set-list|decks|draft-pools|coverage-data|coverage-summary)\.json$/,
            handler: "StaleWhileRevalidate",
            options: {
              cacheName: "data-json",
              expiration: { maxEntries: 12, purgeOnQuotaError: true },
            },
          },
          {
            // Same-origin deck feeds fetched by the home dashboard
            // (see src/data/feedRegistry.ts). Mutable — regenerated
            // periodically — so StaleWhileRevalidate.
            urlPattern: /\/feeds\/[^/]+\.json$/,
            handler: "StaleWhileRevalidate",
            options: {
              cacheName: "deck-feeds",
              expiration: { maxEntries: 16 },
            },
          },
          {
            // Card imagery from Scryfall's CDNs. Image URLs are content-stable
            // (upstream serves `max-age=31556952`), so CacheFirst is correct.
            // `<img>` elements issue no-cors requests whose opaque responses
            // Chrome pads to ~7MB each against origin quota; `fetchOptions`
            // upgrades the SW-side fetch to CORS (Scryfall sends
            // `access-control-allow-origin: *`), so cached entries count at
            // true size. `credentials: "omit"` is required: crossorigin-less
            // <img> requests carry `credentials: "include"`, which fetchOptions
            // would otherwise inherit, and the CORS check hard-fails wildcard
            // ACAO for credentialed requests — every Scryfall fetch would
            // reject. This cache is also the offline card-image store that an
            // explicit "download deck for offline" prefetch warms.
            urlPattern: /^https:\/\/(cards|backs|svgs)\.scryfall\.io\//,
            handler: "CacheFirst",
            options: {
              cacheName: "scryfall-images",
              fetchOptions: { mode: "cors", credentials: "omit" },
              expiration: {
                maxEntries: 4000,
                maxAgeSeconds: 31536000,
                purgeOnQuotaError: true,
              },
            },
          },
          {
            // Same-origin static imagery from public/ (battlefield art, nav
            // icons, logos). Not in the precache manifest — the default glob
            // only covers js/css/html — and unhashed, so StaleWhileRevalidate
            // keeps them offline-available without pinning stale copies past
            // a deploy.
            urlPattern: ({ sameOrigin, request }) =>
              sameOrigin && request.destination === "image",
            handler: "StaleWhileRevalidate",
            options: {
              cacheName: "static-images",
              expiration: { maxEntries: 300, purgeOnQuotaError: true },
            },
          },
        ],
      },
    }),
    compression({ algorithms: ["brotliCompress"] }),
  ],
  define: dataFileDefines(mode),
  worker: {
    plugins: () => [wasmEnvShim()],
  },
  // Vite's host-check rejects requests with a Host header outside its
  // known list — required to allow the Caddy proxy at local.phase-rs.dev
  // (see Caddyfile). HMR's injected websocket client connects back to the
  // page origin, so it needs `clientPort: 443` and `protocol: wss` to
  // hit Caddy rather than the bare :5173 dev server. Both are gated on a
  // hostname presence check so plain `pnpm dev` on localhost still works.
  server: {
    allowedHosts: ["local.phase-rs.dev", ".local.phase-rs.dev"],
    hmr: process.env.CADDY_PROXY === "1"
      ? { protocol: "wss", host: "local.phase-rs.dev", clientPort: 443 }
      : undefined,
    // Forward deck-import-service calls to a locally-running `wrangler dev` so
    // the browser sees a same-origin response (no CORS) and the client can use
    // a relative URL identical to its production same-origin proxy path. The
    // production build sets VITE_IMPORT_DECK_URL to the absolute lobby host.
    proxy: {
      "/import-deck": {
        target: process.env.VITE_IMPORT_DECK_PROXY ?? "http://localhost:8787",
        changeOrigin: true,
      },
    },
  },
  build: {
    target: "esnext",
  },
}));
