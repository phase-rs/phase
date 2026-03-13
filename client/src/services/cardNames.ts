interface CardEntry {
  name: string;
}

let cachedNames: string[] | null = null;

/** Lazily load and cache all card names from card-data.json. */
export async function getCardNames(): Promise<string[]> {
  if (cachedNames) return cachedNames;
  const resp = await fetch("/card-data.json");
  if (!resp.ok) return [];
  const data: Record<string, CardEntry> = await resp.json();
  cachedNames = Object.values(data)
    .map((entry) => entry.name)
    .sort();
  return cachedNames;
}
