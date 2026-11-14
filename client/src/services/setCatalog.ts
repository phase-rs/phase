/**
 * The Scryfall set catalog (code → printed name, release date, icon).
 *
 * A standalone data loader rather than part of `hooks/useSetSymbols`, which
 * also pulls in the visual-pack image stack: a non-React consumer (the LLM
 * drafter's format brief) must be able to read set names without dragging the
 * visual-pack repository into its import graph.
 *
 * The module-level cache is shared with `useSetCatalog`, so whichever consumer
 * loads first pays for the fetch and the other reuses it.
 */

export interface ScryfallSetInfo {
  name: string;
  released_at: string;
  icon_svg_uri?: string;
}

export type ScryfallSetCatalog = Readonly<Record<string, ScryfallSetInfo>>;

let cachedCatalog: ScryfallSetCatalog | null = null;
let catalogPromise: Promise<ScryfallSetCatalog | null> | null = null;

function validateSetCatalog(value: unknown): ScryfallSetCatalog | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;

  const catalog: Record<string, ScryfallSetInfo> = {};
  for (const [code, raw] of Object.entries(value)) {
    if (
      !raw
      || typeof raw !== "object"
      || Array.isArray(raw)
      || typeof (raw as Record<string, unknown>).name !== "string"
      || typeof (raw as Record<string, unknown>).released_at !== "string"
    ) {
      continue;
    }
    const icon = (raw as Record<string, unknown>).icon_svg_uri;
    if (icon !== undefined && icon !== null && typeof icon !== "string") continue;
    catalog[code] = {
      name: (raw as Record<string, string>).name,
      released_at: (raw as Record<string, string>).released_at,
      ...(typeof icon === "string" ? { icon_svg_uri: icon } : {}),
    };
  }
  return catalog;
}

/**
 * Load the catalog, or resolve to `null` when it is unavailable. Every caller
 * must degrade rather than fail: the catalog is presentation metadata, and
 * nothing that needs it is blocked without it.
 */
export function ensureSetCatalog(): Promise<ScryfallSetCatalog | null> {
  if (cachedCatalog) return Promise.resolve(cachedCatalog);
  if (catalogPromise) return catalogPromise;

  catalogPromise = fetch(__SCRYFALL_SETS_URL__)
    .then(async (response) => response.ok ? validateSetCatalog(await response.json()) : null)
    .then((catalog) => {
      if (catalog) cachedCatalog = catalog;
      return catalog;
    })
    .catch(() => null)
    .finally(() => {
      catalogPromise = null;
    });
  return catalogPromise;
}

/** The already-loaded catalog, without triggering a fetch. */
export function cachedSetCatalog(): ScryfallSetCatalog | null {
  return cachedCatalog;
}
