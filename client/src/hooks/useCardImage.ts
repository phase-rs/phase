import { useEffect, useState } from "react";
import { getCachedImage, revokeImageUrl } from "../services/imageCache.ts";
import { fetchCardImageUrl, fetchTokenImageUrl } from "../services/scryfall.ts";

interface UseCardImageOptions {
  size?: "small" | "normal" | "large" | "art_crop";
  faceIndex?: number;
  isToken?: boolean;
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
  const [src, setSrc] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    let objectUrl: string | null = null;

    async function loadImage() {
      setIsLoading(true);
      setSrc(null);

      try {
        // Check cache first (tokens use a prefixed key to avoid collisions)
        const cacheKey = isToken ? `token:${cardName}` : cardName;
        const cached = await getCachedImage(cacheKey, size);
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

        // Cache miss - fetch from Scryfall
        const directUrl = isToken
          ? await fetchTokenImageUrl(cardName, size)
          : await fetchCardImageUrl(cardName, faceIndex, size);
        if (!cancelled) {
          setSrc(directUrl);
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
  }, [cardName, size, faceIndex, isToken]);

  return { src, isLoading };
}
