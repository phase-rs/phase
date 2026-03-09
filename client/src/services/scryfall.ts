import { get } from "idb-keyval";
import { cacheImage } from "./imageCache.ts";

const SCRYFALL_DELAY_MS = 75;

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

async function fetchCardData(cardName: string): Promise<ScryfallCard> {
  const exactUrl = `https://api.scryfall.com/cards/named?exact=${encodeURIComponent(cardName)}`;
  const exactResponse = await rateLimitedFetch(exactUrl);
  if (exactResponse.ok) {
    return exactResponse.json() as Promise<ScryfallCard>;
  }

  if (exactResponse.status !== 404) {
    throw new Error(`Scryfall API error: ${exactResponse.status} for "${cardName}"`);
  }

  const fuzzyUrl = `https://api.scryfall.com/cards/named?fuzzy=${encodeURIComponent(cardName)}`;
  const fuzzyResponse = await rateLimitedFetch(fuzzyUrl);
  if (!fuzzyResponse.ok) {
    throw new Error(`Scryfall API error: ${fuzzyResponse.status} for "${cardName}"`);
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
  const card = await fetchCardData(cardName);
  return getImageUrl(card, size, faceIndex);
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
