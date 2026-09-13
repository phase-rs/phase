import type { StorybookConfig } from "@storybook/react-vite";
import type { Alias, AliasOptions, PluginOption } from "vite";

import { join } from "node:path";

/**
 * Plugins from `client/vite.config.ts` that only make sense for a shipped app
 * build. The PWA plugin registers a service worker that would cache
 * Storybook's own preview iframe, and brotli compression is dead weight for a
 * dev catalog. Matched by prefix: the PWA plugin splits itself into several
 * named plugins (`vite-plugin-pwa:build`, `vite-plugin-pwa:dev`, …), and
 * `vite-plugin-compression2` registers itself as `vite-plugin-compression`.
 *
 * Everything else in that config is inherited deliberately: the `__*_URL__`
 * build defines, Tailwind, and the wasm-bindgen import shims all have to match
 * the app, or a component would compile differently here than it renders in
 * production and the catalog would stop being evidence.
 */
const APP_ONLY_PLUGIN_PREFIXES = ["vite-plugin-pwa", "vite-plugin-compression"];

/**
 * Hooks swapped for story doubles. Both reach for build artifacts a plain
 * checkout does not have: `useCardImage` reads the gitignored Scryfall lookup
 * maps under `client/public/`, and `useEngineCardData` loads the WASM engine
 * bundle. Without these two aliases Storybook could not boot until someone had
 * run the full card-data and wasm-pack pipelines.
 *
 * The doubles are typed against the real modules, so a signature change on
 * either hook fails `pnpm type-check` instead of drifting silently.
 */
const DOUBLED_HOOKS = ["useCardImage", "useEngineCardData"];

/**
 * The wasm-bindgen bundles, which wasm-pack generates into a gitignored
 * `client/src/wasm/`. Every consumer reaches them through `await import(...)`,
 * which the dev server never evaluates but a static build still has to
 * resolve, so both point at a stub that throws if a story ever calls in.
 */
const WASM_BUNDLE_ALIASES = ["@wasm/engine", "@wasm/draft"];

function isAppOnlyPlugin(plugin: PluginOption): boolean {
  if (!plugin || !("name" in plugin)) return false;
  return APP_ONLY_PLUGIN_PREFIXES.some((prefix) => plugin.name.startsWith(prefix));
}

function normaliseAliases(alias: AliasOptions | undefined): Alias[] {
  if (!alias) return [];
  if (Array.isArray(alias)) return alias;
  return Object.entries(alias).map(([find, replacement]) => ({ find, replacement }));
}

const config: StorybookConfig = {
  stories: ["../src/**/*.stories.@(ts|tsx)"],
  addons: ["@storybook/addon-docs", "@storybook/addon-a11y"],
  framework: "@storybook/react-vite",
  // Card backs, battlefield art and icons are served from the site root in the
  // app; mirror that here so a story's asset URLs need no rewriting.
  staticDirs: ["../public"],
  viteFinal(viteConfig, { configDir }) {
    // A plugin factory may return an array of plugins — VitePWA returns four —
    // so the list has to be flattened before any of them can be matched by
    // name. Depth is bounded because Vite's `PluginOption` is recursive and
    // `flat(Infinity)` makes the compiler give up expanding it.
    const plugins = (viteConfig.plugins ?? []).flat(2) as PluginOption[];

    return {
      ...viteConfig,
      plugins: plugins.filter((plugin) => !isAppOnlyPlugin(plugin)),
      // `staticDirs` above is the single authority for publishing `client/public`.
      // The app's config leaves `publicDir` at its default, which resolves to that
      // same directory, so a static build otherwise hands two concurrent `fs.cp`
      // walks the same destination tree. Node's `cp` checks for a directory and
      // then creates it in separate steps, so the two races and the loser fails
      // the build with `EEXIST` on whichever nested directory it reached second.
      publicDir: false,
      resolve: {
        ...viteConfig.resolve,
        // Storybook's entries come first, because Vite takes the first
        // matching alias and the stubs have to win over the app's own
        // `@wasm/*` entries.
        //
        // The hook doubles are matched by RegExp, which only the array form
        // supports: callers import these by relative path and the depth of
        // that path differs per component. Each pattern spans the whole
        // specifier, since Vite substitutes only the matched span and would
        // otherwise leave the caller's `../..` glued to an absolute path.
        alias: [
          ...DOUBLED_HOOKS.map((hook) => ({
            find: new RegExp(`^.*/hooks/${hook}(\\.ts)?$`),
            replacement: join(configDir, "mocks", `${hook}.ts`),
          })),
          ...WASM_BUNDLE_ALIASES.map((bundle) => ({
            find: bundle,
            replacement: join(configDir, "mocks", "engineWasm.ts"),
          })),
          ...normaliseAliases(viteConfig.resolve?.alias),
        ],
      },
    };
  },
};

export default config;
