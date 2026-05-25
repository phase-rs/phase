// Eagerly bundle every locale catalog at build time. Vite inlines the JSON into
// the bundle so the app works fully offline (PWA + Tauri) with no network fetch.
// This is the single source of i18n catalog data — `index.ts` feeds it to i18next
// and `react-i18next.d.ts` derives typed keys from the English files.
const modules = import.meta.glob("./locales/*/*.json", {
  eager: true,
  import: "default",
}) as Record<string, Record<string, unknown>>;

/** Languages the app ships chrome catalogs for. English is the typing oracle and
 *  the `fallbackLng`; the others may lag without breaking the build. */
export const SUPPORTED_LNGS = ["en", "es", "fr", "de", "it", "pt", "pl"] as const;
export type SupportedLng = (typeof SUPPORTED_LNGS)[number];

/** `{ en: { common: {...}, ... }, es: {...}, ... }` reshaped from the flat glob
 *  keyed by `./locales/<lng>/<ns>.json`. */
export const resources: Record<string, Record<string, Record<string, unknown>>> =
  Object.entries(modules).reduce<
    Record<string, Record<string, Record<string, unknown>>>
  >((acc, [path, mod]) => {
    const match = /\.\/locales\/([^/]+)\/([^/]+)\.json$/.exec(path);
    if (!match) return acc;
    const [, lng, ns] = match;
    (acc[lng] ??= {})[ns] = mod;
    return acc;
  }, {});

/** Map the browser's locale prefix to a supported language, else English. The
 *  preferences store calls this for the cold-start default (no detector needed). */
export function detectInitialLanguage(): SupportedLng {
  if (typeof navigator === "undefined") return "en";
  const prefix = navigator.language.split("-")[0]?.toLowerCase() ?? "";
  return (SUPPORTED_LNGS as readonly string[]).includes(prefix)
    ? (prefix as SupportedLng)
    : "en";
}
