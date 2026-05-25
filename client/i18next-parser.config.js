// Config for `i18next-parser` — extracts t()/<Trans> keys from src into the
// per-namespace catalogs. Run manually (not in CI); it only collects keys already
// referenced in code. Settings must match i18n/index.ts so extraction round-trips
// into the multi-namespace layout instead of collapsing to one file.
/** @type {import('i18next-parser').UserConfig} */
export default {
  locales: ["en", "es", "fr", "de", "it", "pt"],
  defaultNamespace: "common",
  keySeparator: ".",
  nsSeparator: ":",
  pluralSeparator: "_",
  // Preserve existing translations; only add newly-discovered keys.
  keepRemoved: true,
  output: "src/i18n/locales/$LOCALE/$NAMESPACE.json",
  input: ["src/**/*.{ts,tsx}", "!src/**/*.test.{ts,tsx}", "!src/wasm/**"],
  sort: true,
};
