import { get } from "idb-keyval";
import { cacheImage } from "./imageCache.ts";

interface ImageMapEntry {
  normal: string;
  art_crop: string;
}
type ImageMap = Record<string, ImageMapEntry[]>;

let imageMapPromise: Promise<ImageMap | null> | null = null;

function loadImageMap(): Promise<ImageMap | null> {
  if (!imageMapPromise) {
    imageMapPromise = fetch("/scryfall-images.json")
      .then((r) => r.json() as Promise<ImageMap>)
      .catch(() => null);
  }
  return imageMapPromise;
}

const SCRYFALL_DELAY_MS = 75;
const NOT_FOUND_TTL_MS = 10 * 60 * 1000; // 10 minutes

/** Cards that returned 404 from both exact and fuzzy lookup. */
const notFoundCache = new Map<string, number>();

export type ImageSize = "small" | "normal" | "large" | "art_crop";

export interface ScryfallCard {
  id: string;
  name: string;
  mana_cost: string;
  cmc: number;
  type_line: string;
  oracle_text?: string;
  colors?: string[];
  color_identity: string[];
  keywords?: string[];
  legalities: Record<string, string>;
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

let lastRequestTime = 0;

async function rateLimitedFetch(url: string): Promise<Response> {
  const now = Date.now();
  const elapsed = now - lastRequestTime;
  if (elapsed < SCRYFALL_DELAY_MS) {
    await new Promise((resolve) =>
      setTimeout(resolve, SCRYFALL_DELAY_MS - elapsed),
    );
  }
  lastRequestTime = Date.now();
  return fetch(url);
}

/** Strip set code brackets (e.g. "Goblin Lackey [UZ]" → "Goblin Lackey"). */
function normalizeCardName(name: string): string {
  return name.replace(/\s*\[[^\]]*\]\s*$/, "").trim();
}

export async function fetchCardData(cardName: string): Promise<ScryfallCard> {
  const name = normalizeCardName(cardName);

  const cachedAt = notFoundCache.get(name);
  if (cachedAt !== undefined && Date.now() - cachedAt < NOT_FOUND_TTL_MS) {
    throw new Error(`Card not found (cached): "${name}"`);
  }

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

export async function fetchCardImage(
  cardName: string,
  size: ImageSize = "normal",
): Promise<Blob> {
  const cachedBlob = await get<Blob>(`scryfall:${cardName}:${size}`);
  if (cachedBlob) return cachedBlob;

  const card = await fetchCardData(cardName);
  const imageUrl = getImageUrl(card, size, 0);
  const imageResponse = await rateLimitedFetch(imageUrl);
  if (!imageResponse.ok) {
    throw new Error(
      `Scryfall image fetch error: ${imageResponse.status} for "${cardName}"`,
    );
  }
  const blob = await imageResponse.blob();
  await cacheImage(cardName, size, blob);
  return blob;
}

export async function prefetchDeckImages(
  cardNames: string[],
): Promise<void> {
  const unique = [...new Set(cardNames)];
  for (const name of unique) {
    try {
      const imageUrl = await fetchCardImageUrl(name, 0, "normal");
      await new Promise<void>((resolve, reject) => {
        const img = new Image();
        img.onload = () => resolve();
        img.onerror = () => reject(new Error(`Failed to preload image for "${name}"`));
        img.src = imageUrl;
      });
    } catch {
      // Skip failed fetches during prefetch
    }
  }
}

export async function fetchCardImageUrl(
  cardName: string,
  faceIndex: number,
  size: ImageSize = "normal",
): Promise<string> {
  // Local image map covers normal and art_crop — skip API round-trip for these.
  if (size === "normal" || size === "art_crop") {
    const map = await loadImageMap();
    const name = normalizeCardName(cardName).toLowerCase();
    const faces = map?.[name];
    if (faces) {
      const face = faces[faceIndex] ?? faces[0];
      const url = face[size];
      if (url) return url;
    }
  }
  // Fall back to Scryfall API for cache misses or other sizes (small, large).
  const card = await fetchCardData(cardName);
  return getImageUrl(card, size, faceIndex);
}

export async function fetchTokenImageUrl(
  tokenName: string,
  size: ImageSize = "normal",
): Promise<string> {
  const query = `t:token !"${tokenName}"`;
  const url = `https://api.scryfall.com/cards/search?q=${encodeURIComponent(query)}&order=released&dir=desc`;
  const response = await rateLimitedFetch(url);
  if (!response.ok) {
    throw new Error(`No token image found for "${tokenName}"`);
  }
  const data: ScryfallSearchResponse = await response.json();
  if (data.data.length === 0) {
    throw new Error(`No token image found for "${tokenName}"`);
  }
  return getImageUrl(data.data[0], size, 0);
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

  if (response.status === 429) {
    const retryAfter = parseInt(response.headers.get("Retry-After") ?? "1", 10);
    await new Promise((r) => setTimeout(r, retryAfter * 1000));
    return searchScryfall(query, signal);
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
