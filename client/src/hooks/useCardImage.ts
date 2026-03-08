import { useEffect, useState } from "react";
import { getCachedImage, revokeImageUrl } from "../services/imageCache.ts";
import { fetchCardImage } from "../services/scryfall.ts";

interface UseCardImageOptions {
  size?: "small" | "normal" | "large";
  faceIndex?: number;
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
  const [src, setSrc] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    let objectUrl: string | null = null;

    async function loadImage() {
      setIsLoading(true);
      setSrc(null);

      try {
        // Check cache first
        const cached = await getCachedImage(cardName, size);
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
        const blob = await fetchCardImage(cardName, size);
        if (!cancelled) {
          const url = URL.createObjectURL(blob);
          objectUrl = url;
          setSrc(url);
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
  }, [cardName, size]);

  return { src, isLoading };
}
