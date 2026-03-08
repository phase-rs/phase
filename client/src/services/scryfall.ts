import { get } from "idb-keyval";
import { cacheImage } from "./imageCache.ts";

const SCRYFALL_DELAY_MS = 75;

type ImageSize = "small" | "normal" | "large";

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
  const url = `https://api.scryfall.com/cards/named?exact=${encodeURIComponent(cardName)}`;
  const response = await rateLimitedFetch(url);
  if (!response.ok) {
    throw new Error(`Scryfall API error: ${response.status} for "${cardName}"`);
  }
  return response.json() as Promise<ScryfallCard>;
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
      await fetchCardImage(name, "normal");
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
