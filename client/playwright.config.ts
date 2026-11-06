import { defineConfig, devices } from "@playwright/test";

/**
 * `vite preview` serving the built client (`pnpm build` first): a production
 * build that bundles the WASM engines in `src/wasm/`. The port is
 * fixed and strict, so a stale server on it fails the run instead of being
 * tested in its place.
 */
const PREVIEW_URL = "http://127.0.0.1:4173";

export default defineConfig({
  testDir: "./e2e",
  outputDir: "./test-results",
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  workers: 1,
  // The debug-profile engines are large and unoptimized; a slow runner needs
  // room to fetch and compile them before the first draft view.
  timeout: 120_000,
  expect: { timeout: 15_000 },
  reporter: process.env.CI ? [["list"], ["github"]] : "list",
  use: {
    baseURL: PREVIEW_URL,
    locale: "en-US",
    // The built client registers a Workbox service worker that caches the
    // data files, and `page.route` does not see requests a service worker
    // answers. Blocking it keeps every data file the spec serves in view.
    serviceWorkers: "block",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "pnpm exec vite preview --host 127.0.0.1 --port 4173 --strictPort",
    url: PREVIEW_URL,
    reuseExistingServer: false,
  },
});
