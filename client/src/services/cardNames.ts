let cachedNames: string[] | null = null;

/** Lazily load and cache all card names from card-names.json. */
export async function getCardNames(): Promise<string[]> {
  if (cachedNames) return cachedNames;
  const resp = await fetch("/card-names.json");
  if (!resp.ok) return [];
  cachedNames = (await resp.json()) as string[];
  return cachedNames;
}
