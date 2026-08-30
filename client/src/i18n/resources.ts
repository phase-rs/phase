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

function isSupportedLng(value: string): value is SupportedLng {
  return (SUPPORTED_LNGS as readonly string[]).includes(value);
}

/**
 * Reduces a browser or persisted language tag to the app's closed locale set.
 * Content sidecars and card-art maps are named by their two-letter app locale,
 * so allowing an otherwise-valid tag such as `pt-BR` through would make chrome
 * i18next fall back while those consumers request nonexistent assets.
 */
export function normalizeSupportedLng(value: unknown, fallback: SupportedLng): SupportedLng {
  if (typeof value !== "string") return fallback;
  const prefix = value.trim().split("-", 1)[0]?.toLowerCase() ?? "";
  return isSupportedLng(prefix) ? prefix : fallback;
}

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
  return normalizeSupportedLng(navigator.language, "en");
}
