import { useEffect, useState } from "react";
import {
  fetchCardImageByOracleId,
  fetchCardImageUrl,
  fetchTokenImageUrl,
} from "../services/scryfall.ts";
import type { TokenSearchFilters } from "../services/scryfall.ts";

interface UseCardImageOptions {
  size?: "small" | "normal" | "large" | "art_crop";
  faceIndex?: number;
  isToken?: boolean;
  tokenFilters?: TokenSearchFilters;
  /** Canonical lookup id from `printed_ref.oracle_id`. When provided, the
   * Scryfall service resolves the image by oracle id (preferred) and
   * `cardName`/`faceIndex` are used only as cache-key disambiguators and
   * `aria-label`/diagnostic context. Battlefield call sites should set this. */
  oracleId?: string;
  /** Companion to `oracleId` — the engine-reported face name selects which
   * Scryfall `card_faces` entry to render. */
  faceName?: string;
}

interface UseCardImageResult {
  src: string | null;
  isLoading: boolean;
}

interface MemoryCacheEntry {
  promise: Promise<string | null> | null;
  refCount: number;
  src: string | null;
}

const imageRequestCache = new Map<string, MemoryCacheEntry>();

function imageRequestKey(
  cardName: string,
  size: string,
  faceIndex: number,
  isToken: boolean,
  filterPower: number | null,
  filterToughness: number | null,
  filterColors: string,
  oracleId: string,
  faceName: string,
): string {
  return [
    oracleId || cardName,
    oracleId ? faceName : String(faceIndex),
    size,
    isToken ? "token" : "card",
    filterPower ?? "",
    filterToughness ?? "",
    filterColors,
  ].join("|");
}

function releaseCachedImageSrc(key: string): void {
  const entry = imageRequestCache.get(key);
  if (!entry) return;
  entry.refCount = Math.max(0, entry.refCount - 1);
  if (entry.refCount === 0 && !entry.promise) {
    imageRequestCache.delete(key);
  }
}

async function acquireCachedImageSrc(
  key: string,
  cardName: string,
  size: "small" | "normal" | "large" | "art_crop",
  faceIndex: number,
  isToken: boolean,
  filterPower: number | null,
  filterToughness: number | null,
  filterColors: string,
  oracleId: string,
  faceName: string,
): Promise<string | null> {
  const existing = imageRequestCache.get(key);
  if (existing) {
    existing.refCount += 1;
    if (existing.src !== null) return existing.src;
    if (existing.promise) return existing.promise;
  }

  const entry: MemoryCacheEntry = {
    promise: null,
    refCount: 1,
    src: null,
  };
  imageRequestCache.set(key, entry);

  entry.promise = (async () => {
    let remoteSrc: string | null;
    if (isToken) {
      remoteSrc = await fetchTokenImageUrl(cardName, size, {
        power: filterPower,
        toughness: filterToughness,
        colors: filterColors ? filterColors.split(",") : undefined,
      });
    } else if (oracleId) {
      remoteSrc = await fetchCardImageByOracleId(oracleId, faceName, size);
    } else {
      remoteSrc = await fetchCardImageUrl(cardName, faceIndex, size);
    }
    entry.src = remoteSrc;
    entry.promise = null;
    if (entry.refCount === 0) {
      imageRequestCache.delete(key);
    }
    return remoteSrc;
  })().catch(() => {
    imageRequestCache.delete(key);
    return null;
  });

  return entry.promise;
}

export function useCardImage(
  cardName: string,
  options?: UseCardImageOptions,
): UseCardImageResult {
  const size = options?.size ?? "normal";
  const faceIndex = options?.faceIndex ?? 0;
  const isToken = options?.isToken ?? false;
  const tokenFilters = options?.tokenFilters;
  const oracleId = options?.oracleId ?? "";
  const faceName = options?.faceName ?? "";
  // Stabilize tokenFilters into primitives so the effect doesn't re-fire on every render
  const filterPower = tokenFilters?.power ?? null;
  const filterToughness = tokenFilters?.toughness ?? null;
  const filterColors = tokenFilters?.colors?.join(",") ?? "";
  const [src, setSrc] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const requestKey = imageRequestKey(
    cardName,
    size,
    faceIndex,
    isToken,
    filterPower,
    filterToughness,
    filterColors,
    oracleId,
    faceName,
  );

  useEffect(() => {
    if (!cardName && !oracleId) {
      setSrc(null);
      setIsLoading(false);
      return;
    }

    let cancelled = false;

    async function loadImage() {
      setIsLoading(true);
      setSrc(null);

      try {
        const imageUrl = await acquireCachedImageSrc(
          requestKey,
          cardName,
          size,
          faceIndex,
          isToken,
          filterPower,
          filterToughness,
          filterColors,
          oracleId,
          faceName,
        );
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
      releaseCachedImageSrc(requestKey);
    };
  }, [
    cardName,
    faceIndex,
    faceName,
    filterColors,
    filterPower,
    filterToughness,
    isToken,
    oracleId,
    requestKey,
    size,
  ]);

  return { src, isLoading };
}
