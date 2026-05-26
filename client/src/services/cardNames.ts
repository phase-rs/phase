let cachedNames: string[] | null = null;

/** Lazily load and cache all card names from card-names.json. */
export async function getCardNames(): Promise<string[]> {
  if (cachedNames) return cachedNames;
  // Resolve via the build-time define (R2 prefix on deploy, "/card-names.json"
  // in local dev) — matching every other data-file consumer. A hardcoded
  // site-root path 404s on R2-backed deploys where data files aren't bundled.
  const resp = await fetch(__CARD_NAMES_URL__);
  if (!resp.ok) return [];
  cachedNames = (await resp.json()) as string[];
  return cachedNames;
}
