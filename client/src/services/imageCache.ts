import { get, set } from "idb-keyval";

function cacheKey(cardName: string, size: string): string {
  return `scryfall:${cardName}:${size}`;
}

export async function getCachedImage(
  cardName: string,
  size: "small" | "normal" | "large" | "art_crop",
): Promise<string | null> {
  const blob = await get<Blob>(cacheKey(cardName, size));
  if (!blob) return null;
  return URL.createObjectURL(blob);
}

export async function cacheImage(
  cardName: string,
  size: string,
  blob: Blob,
): Promise<void> {
  await set(cacheKey(cardName, size), blob);
}

export function revokeImageUrl(url: string): void {
  URL.revokeObjectURL(url);
}
