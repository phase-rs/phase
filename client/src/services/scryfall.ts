interface ScryfallDataEntry {
  faces: Array<{ normal: string; art_crop: string }>;
  name: string;
  mana_cost: string;
  cmc: number;
  type_line: string;
  colors: string[];
  color_identity: string[];
  keywords: string[];
}

type ScryfallDataMap = Record<string, ScryfallDataEntry>;

let scryfallDataPromise: Promise<ScryfallDataMap | null> | null = null;
let scryfallQueue: Promise<void> = Promise.resolve();

function loadScryfallData(): Promise<ScryfallDataMap | null> {
  if (!scryfallDataPromise) {
    scryfallDataPromise = fetch("/scryfall-data.json")
      .then((r) => r.json() as Promise<ScryfallDataMap>)
      .catch(() => null);
  }
  return scryfallDataPromise;
}

const SCRYFALL_DELAY_MS = 100;
const NOT_FOUND_TTL_MS = 10 * 60 * 1000; // 10 minutes
const MAX_RETRIES = 3;
const BASE_BACKOFF_MS = 1000;

/** Cards that returned 404 from both exact and fuzzy lookup. */
const notFoundCache = new Map<string, number>();
const cardDataCache = new Map<string, Promise<ScryfallCard>>();

export type ImageSize = "small" | "normal" | "large" | "art_crop";

export interface ScryfallCard {
  id?: string;
  name: string;
  mana_cost: string;
  cmc: number;
  type_line: string;
  oracle_text?: string;
  colors?: string[];
  color_identity: string[];
  keywords?: string[];
  legalities?: Record<string, string>;
  image_uris?: Record<string, string>;
  card_faces?: Array<{
    name: string;
    image_uris?: Record<string, string>;
  }>;
}

interface ScryfallSearchResponse {
  data: ScryfallCard[];
  total_cards: number;
  has_more: boolean;
}

let nextRequestAt = 0;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function claimScryfallQueueSlot(): Promise<() => void> {
  const prior = scryfallQueue.catch(() => undefined);
  let release!: () => void;
  scryfallQueue = new Promise<void>((resolve) => {
    release = resolve;
  });
  await prior;
  return release;
}

function parseRetryDelayMs(retryAfter: string | null, attempt: number): number {
  if (!retryAfter) {
    return BASE_BACKOFF_MS * 2 ** attempt;
  }

  const retryAfterSeconds = Number.parseInt(retryAfter, 10);
  if (Number.isFinite(retryAfterSeconds)) {
    return retryAfterSeconds * 1000;
  }

  const retryAfterAt = Date.parse(retryAfter);
  if (Number.isFinite(retryAfterAt)) {
    return Math.max(0, retryAfterAt - Date.now());
  }

  return BASE_BACKOFF_MS * 2 ** attempt;
}

/**
 * Rate-limited fetch with 429 backoff and retry.
 *
 * Enforces a minimum delay between requests (Scryfall asks for 50-100ms),
 * and automatically retries on 429 using the Retry-After header with
 * exponential backoff as a fallback.
 *
 * On 429, the queue slot is held during the backoff sleep so that no other
 * requests can interleave and overwrite the backoff timestamp.
 */
async function rateLimitedFetch(
  url: string,
): Promise<Response> {
  let attempt = 0;

  const release = await claimScryfallQueueSlot();
  try {
    while (true) {
      const delayMs = Math.max(0, nextRequestAt - Date.now());
      if (delayMs > 0) {
        await sleep(delayMs);
      }

      try {
        const response = await fetch(url);
        if (response.status === 429 && attempt < MAX_RETRIES) {
          const backoffMs = parseRetryDelayMs(
            response.headers.get("Retry-After"),
            attempt,
          );
          nextRequestAt = Date.now() + backoffMs;
          attempt += 1;
          continue;
        }

        nextRequestAt = Date.now() + SCRYFALL_DELAY_MS;
        return response;
      } catch (error) {
        if (attempt >= MAX_RETRIES) {
          throw error;
        }

        nextRequestAt = Date.now() + BASE_BACKOFF_MS * 2 ** attempt;
        attempt += 1;
      }
    }
  } finally {
    release();
  }
}

/** Strip set code brackets (e.g. "Goblin Lackey [UZ]" → "Goblin Lackey"). */
export function normalizeCardName(name: string): string {
  return name.replace(/\s*\[[^\]]*\]\s*$/, "").trim();
}

export async function fetchCardData(cardName: string): Promise<ScryfallCard> {
  const name = normalizeCardName(cardName);

  const cachedAt = notFoundCache.get(name);
  if (cachedAt !== undefined && Date.now() - cachedAt < NOT_FOUND_TTL_MS) {
    throw new Error(`Card not found (cached): "${name}"`);
  }

  const cachedCard = cardDataCache.get(name);
  if (cachedCard) {
    return cachedCard;
  }

  const cardPromise = (async () => {
    // Check local bulk data first — avoids API round-trips for ~99% of cards.
    const localMap = await loadScryfallData();
    const entry = localMap?.[name.toLowerCase()];
    if (entry) {
      return {
        name: entry.name,
        mana_cost: entry.mana_cost,
        cmc: entry.cmc,
        type_line: entry.type_line,
        colors: entry.colors,
        color_identity: entry.color_identity,
        keywords: entry.keywords,
      };
    }

    // Fall back to Scryfall API for cards not in bulk data (very new cards).
    const exactUrl = `https://api.scryfall.com/cards/named?exact=${encodeURIComponent(name)}`;
    const exactResponse = await rateLimitedFetch(exactUrl);
    if (exactResponse.ok) {
      return exactResponse.json() as Promise<ScryfallCard>;
    }

    if (exactResponse.status !== 404) {
      throw new Error(`Scryfall API error: ${exactResponse.status} for "${name}"`);
    }

    const fuzzyUrl = `https://api.scryfall.com/cards/named?fuzzy=${encodeURIComponent(name)}`;
    const fuzzyResponse = await rateLimitedFetch(fuzzyUrl);
    if (!fuzzyResponse.ok) {
      notFoundCache.set(name, Date.now());
      throw new Error(`Scryfall API error: ${fuzzyResponse.status} for "${name}"`);
    }
    return fuzzyResponse.json() as Promise<ScryfallCard>;
  })().catch((error) => {
    cardDataCache.delete(name);
    throw error;
  });

  cardDataCache.set(name, cardPromise);
  return cardPromise;
}

function getImageUrl(
  card: ScryfallCard,
  size: ImageSize,
  faceIndex: number,
): string {
  if (card.card_faces?.[faceIndex]?.image_uris?.[size]) {
    return card.card_faces[faceIndex].image_uris![size];
  }
  if (card.image_uris?.[size]) {
    return card.image_uris[size];
  }
  throw new Error("No image URI found for card");
}

export async function fetchCardImageUrl(
  cardName: string,
  faceIndex: number,
  size: ImageSize = "normal",
): Promise<string> {
  // Local data covers normal and art_crop — skip API round-trip for these.
  if (size === "normal" || size === "art_crop") {
    const data = await loadScryfallData();
    const name = normalizeCardName(cardName).toLowerCase();
    const entry = data?.[name];
    if (entry) {
      const face = entry.faces[faceIndex] ?? entry.faces[0];
      const url = face?.[size];
      if (url) return url;
    }
  }
  // Fall back to Scryfall API for cache misses or other sizes (small, large).
  const card = await fetchCardData(cardName);
  return getImageUrl(card, size, faceIndex);
}

const MANA_COLOR_TO_SCRYFALL: Record<string, string> = {
  White: "w", Blue: "u", Black: "b", Red: "r", Green: "g",
};

export interface TokenSearchFilters {
  power?: number | null;
  toughness?: number | null;
  colors?: string[];
}

export async function fetchTokenImageUrl(
  tokenName: string,
  size: ImageSize = "normal",
  filters?: TokenSearchFilters,
): Promise<string> {
  const colorClause = buildTokenColorClause(filters?.colors);

  // Try with exact P/T first, then fall back without P/T if no results.
  const queries = [
    buildTokenQuery(tokenName, filters?.power, filters?.toughness, colorClause),
    ...(filters?.power != null || filters?.toughness != null
      ? [buildTokenQuery(tokenName, null, null, colorClause)]
      : []),
  ];

  for (const query of queries) {
    const url = `https://api.scryfall.com/cards/search?q=${encodeURIComponent(query)}&order=released&dir=desc`;
    const response = await rateLimitedFetch(url);
    if (!response.ok) continue;
    const data: ScryfallSearchResponse = await response.json();
    if (data.data.length > 0) {
      return getImageUrl(data.data[0], size, 0);
    }
  }

  throw new Error(`No token image found for "${tokenName}"`);
}

function buildTokenQuery(
  name: string,
  power: number | null | undefined,
  toughness: number | null | undefined,
  colorClause: string,
): string {
  let query = `t:token !"${name}"`;
  if (power != null) query += ` pow=${power}`;
  if (toughness != null) query += ` tou=${toughness}`;
  query += colorClause;
  return query;
}

function buildTokenColorClause(colors: string[] | undefined | null): string {
  if (colors == null) return "";
  const colorStr = colors.map((c) => MANA_COLOR_TO_SCRYFALL[c] ?? "").join("");
  return colorStr ? ` c=${colorStr}` : " c=c";
}

/**
 * Search Scryfall for cards matching query. Uses rate limiting and handles 429s.
 */
export async function searchScryfall(
  query: string,
  signal?: AbortSignal,
): Promise<{ cards: ScryfallCard[]; total: number }> {
  const url = `https://api.scryfall.com/cards/search?q=${encodeURIComponent(query)}`;
  const response = await rateLimitedFetch(url);

  if (signal?.aborted) {
    return { cards: [], total: 0 };
  }

  if (response.status === 404) {
    return { cards: [], total: 0 };
  }

  if (!response.ok) {
    throw new Error(`Scryfall search error: ${response.status}`);
  }

  const data: ScryfallSearchResponse = await response.json();
  return { cards: data.data, total: data.total_cards };
}

/** Build Scryfall query string from filter options. */
export function buildScryfallQuery(options: {
  text?: string;
  colors?: string[];
  type?: string;
  cmcMax?: number;
  cmcMin?: number;
  format?: string;
}): string {
  const parts: string[] = [];

  if (options.text) parts.push(options.text);
  if (options.colors?.length) parts.push(`c:${options.colors.join("")}`);
  if (options.type) parts.push(`t:${options.type}`);
  if (options.cmcMin !== undefined) parts.push(`cmc>=${options.cmcMin}`);
  if (options.cmcMax !== undefined) parts.push(`cmc<=${options.cmcMax}`);
  if (options.format) parts.push(`f:${options.format}`);

  return parts.join(" ");
}

/** Get the best image URI for a card (handles double-faced cards). */
export function getCardImageSmall(card: ScryfallCard): string {
  return card.image_uris?.small
    ?? card.card_faces?.[0]?.image_uris?.small
    ?? "";
}
