import { useEffect, useState } from "react";
import { getCachedImage, revokeImageUrl } from "../services/imageCache.ts";
import { fetchCardImageUrl, fetchTokenImageUrl } from "../services/scryfall.ts";
import type { TokenSearchFilters } from "../services/scryfall.ts";

interface UseCardImageOptions {
  size?: "small" | "normal" | "large" | "art_crop";
  faceIndex?: number;
  isToken?: boolean;
  tokenFilters?: TokenSearchFilters;
}

interface UseCardImageResult {
  src: string | null;
  isLoading: boolean;
}

export function useCardImage(
  cardName: string,
  options?: UseCardImageOptions,
): UseCardImageResult {
  const size = options?.size ?? "normal";
  const faceIndex = options?.faceIndex ?? 0;
  const isToken = options?.isToken ?? false;
  const tokenFilters = options?.tokenFilters;
  // Stabilize tokenFilters into primitives so the effect doesn't re-fire on every render
  const filterPower = tokenFilters?.power ?? null;
  const filterToughness = tokenFilters?.toughness ?? null;
  const filterColors = tokenFilters?.colors?.join(",") ?? "";
  const [src, setSrc] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    if (!cardName) {
      setSrc(null);
      setIsLoading(false);
      return;
    }

    let cancelled = false;
    let objectUrl: string | null = null;

    async function loadImage() {
      setIsLoading(true);
      setSrc(null);

      try {
        // Check IndexedDB cache first (tokens use a prefixed key with filters to avoid collisions)
        const filterSuffix = isToken ? `:${filterPower ?? ""}/${filterToughness ?? ""}/${filterColors}` : "";
        const key = isToken ? `token:${cardName}${filterSuffix}` : cardName;
        const cached = await getCachedImage(key, size);
        if (cached) {
          if (!cancelled) {
            objectUrl = cached;
            setSrc(cached);
            setIsLoading(false);
          } else {
            revokeImageUrl(cached);
          }
          return;
        }

        // Cache miss — resolve Scryfall CDN URL and set directly as img src
        // (cross-origin images can't be fetched as blobs without CORS headers,
        // but <img src> bypasses CORS; the browser HTTP cache handles repeat loads)
        const imageUrl = isToken
          ? await fetchTokenImageUrl(cardName, size, {
              power: filterPower,
              toughness: filterToughness,
              colors: filterColors ? filterColors.split(",") : undefined,
            })
          : await fetchCardImageUrl(cardName, faceIndex, size);
        if (!cancelled) {
          setSrc(imageUrl);
          setIsLoading(false);
        }
      } catch {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    }

    loadImage();

    return () => {
      cancelled = true;
      if (objectUrl) {
        revokeImageUrl(objectUrl);
      }
    };
  }, [cardName, size, faceIndex, isToken, filterPower, filterToughness, filterColors]);

  return { src, isLoading };
}
