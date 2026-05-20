import { useEffect, useState } from "react";

import {
  fetchCardImageByOracleId,
  fetchCardImageUrl,
  fetchTokenImageUrl,
  findPrintingById,
  getCardPrintings,
  resolveOracleIdSync,
  resolvePrintingImageUrl,
} from "../services/scryfall.ts";
import type { ImageSize, PrintingEntry, TokenSearchFilters } from "../services/scryfall.ts";
import { usePreferencesStore, registerStrategyCacheClearFn } from "../stores/preferencesStore.ts";
import type { ArtChainEntry } from "../stores/preferencesStore.ts";

export interface SourcePrinting {
  setCode: string;
  collectorNumber: string;
}

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
  /** When set, resolves the image from this specific Scryfall printing ID
   * instead of using the default/strategy resolution. Used by the printing
   * picker to preview a specific printing's art. Requires `oracleId` to
   * look up the printings list. */
  scryfallId?: string;
  /** Source printing context from a draft pack or imported deck list. When no
   * explicit art rule applies, this set+collector pair is matched against the
   * printings list before falling back to default Scryfall art. If the art
   * chain contains a `source_printing` entry, the chain controls priority. */
  sourcePrinting?: SourcePrinting;
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

const strategyCacheMap = new Map<string, PrintingEntry>();
const printingsCacheMap = new Map<string, PrintingEntry[]>();
const strategyInflight = new Set<string>();
const artCacheEvents = new EventTarget();

registerStrategyCacheClearFn(() => {
  strategyCacheMap.clear();
  strategyInflight.clear();
});

function applyChainEntry(
  entry: ArtChainEntry,
  printings: PrintingEntry[],
  source?: SourcePrinting,
): PrintingEntry | null {
  switch (entry.type) {
    case "set":
      return printings.find((p) => p.set === entry.setCode) ?? null;
    case "newest":
      return printings[0];
    case "oldest":
      return printings[printings.length - 1];
    case "prefer_borderless":
      return printings.find((p) => p.border_color === "borderless") ?? null;
    case "prefer_extended":
      return printings.find((p) => p.frame_effects.includes("extendedart")) ?? null;
    case "source_printing": {
      if (!source) return null;
      const setLower = source.setCode.toLowerCase();
      return printings.find((p) => p.set === setLower && p.collector_number === source.collectorNumber) ?? null;
    }
  }
}

function applyChain(chain: ArtChainEntry[], printings: PrintingEntry[], source?: SourcePrinting): PrintingEntry | null {
  if (printings.length === 0) return null;
  for (const entry of chain) {
    const match = applyChainEntry(entry, printings, source);
    if (match) return match;
  }
  return null;
}

function resolveStrategyInBackground(oracleId: string, chain: ArtChainEntry[]): void {
  if (strategyInflight.has(oracleId)) return;
  strategyInflight.add(oracleId);

  getCardPrintings(oracleId).then((printings) => {
    if (printings.length > 0) {
      printingsCacheMap.set(oracleId, printings);
      const winner = applyChain(chain, printings);
      if (winner) {
        strategyCacheMap.set(oracleId, winner);
      }
    }
    strategyInflight.delete(oracleId);
    artCacheEvents.dispatchEvent(new Event("update"));
  }).catch(() => {
    strategyInflight.delete(oracleId);
  });
}

function loadPrintingsInBackground(oracleId: string): void {
  if (strategyInflight.has(oracleId)) return;
  strategyInflight.add(oracleId);

  getCardPrintings(oracleId).then((printings) => {
    if (printings.length > 0) {
      printingsCacheMap.set(oracleId, printings);
    }
    strategyInflight.delete(oracleId);
    artCacheEvents.dispatchEvent(new Event("update"));
  }).catch(() => {
    strategyInflight.delete(oracleId);
  });
}

function resolveOverrideUrl(
  oracleId: string,
  scryfallId: string,
  faceIndex: number,
  size: ImageSize,
): string | null {
  const cached = printingsCacheMap.get(oracleId);
  if (cached) {
    const entry = findPrintingById(cached, scryfallId);
    return entry ? resolvePrintingImageUrl(entry, faceIndex, size) : null;
  }

  getCardPrintings(oracleId).then((printings) => {
    if (printings.length > 0) {
      printingsCacheMap.set(oracleId, printings);
      artCacheEvents.dispatchEvent(new Event("update"));
    }
  }).catch(() => {});

  return null;
}

function resolveSourcePrintingUrl(
  oracleId: string,
  source: SourcePrinting,
  faceIndex: number,
  size: ImageSize,
): string | null {
  const cached = printingsCacheMap.get(oracleId);
  if (cached) {
    const setLower = source.setCode.toLowerCase();
    const entry = cached.find((p) => p.set === setLower && p.collector_number === source.collectorNumber);
    return entry ? resolvePrintingImageUrl(entry, faceIndex, size) : null;
  }

  loadPrintingsInBackground(oracleId);
  return null;
}

function imageRequestKey(
  cardName: string,
  size: string,
  faceIndex: number,
  isToken: boolean,
  filterPower: number | null,
  filterToughness: number | null,
  filterColors: string,
  filterSubtypes: string,
  filterHasAbilities: boolean | null,
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
    filterSubtypes,
    String(filterHasAbilities),
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
  filterSubtypes: string,
  filterHasAbilities: boolean | null,
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
        subtypes: filterSubtypes ? filterSubtypes.split(",") : undefined,
        hasAbilities: filterHasAbilities ?? undefined,
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
  const scryfallId = options?.scryfallId ?? "";
  const sourcePrinting = options?.sourcePrinting;
  const filterPower = tokenFilters?.power ?? null;
  const filterToughness = tokenFilters?.toughness ?? null;
  const filterSubtypes = tokenFilters?.subtypes?.join(",") ?? "";
  const filterColors = tokenFilters?.colors?.join(",") ?? "";
  const filterHasAbilities = tokenFilters?.hasAbilities ?? null;

  const artOverrides = usePreferencesStore((s) => s.artOverrides);
  const artChain = usePreferencesStore((s) => s.artChain);

  const [src, setSrc] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [, setArtCacheTick] = useState(0);

  useEffect(() => {
    const handler = () => setArtCacheTick((t) => t + 1);
    artCacheEvents.addEventListener("update", handler);
    return () => artCacheEvents.removeEventListener("update", handler);
  }, []);

  const resolvedOracleId = oracleId || resolveOracleIdSync(cardName) || "";

  let overrideUrl: string | null = null;
  if (!isToken && resolvedOracleId) {
    if (scryfallId) {
      overrideUrl = resolveOverrideUrl(resolvedOracleId, scryfallId, faceIndex, size);
    } else if (artOverrides[resolvedOracleId]) {
      overrideUrl = resolveOverrideUrl(resolvedOracleId, artOverrides[resolvedOracleId].scryfallId, faceIndex, size);
    } else if (artChain.length > 0) {
      if (sourcePrinting && artChain.some((e) => e.type === "source_printing")) {
        const printings = printingsCacheMap.get(resolvedOracleId);
        if (printings) {
          const winner = applyChain(artChain, printings, sourcePrinting);
          if (winner) {
            overrideUrl = resolvePrintingImageUrl(winner, faceIndex, size);
          }
        } else {
          resolveStrategyInBackground(resolvedOracleId, artChain);
        }
      } else {
        const cached = strategyCacheMap.get(resolvedOracleId);
        if (cached) {
          overrideUrl = resolvePrintingImageUrl(cached, faceIndex, size);
        } else {
          resolveStrategyInBackground(resolvedOracleId, artChain);
        }
      }
    } else if (sourcePrinting) {
      overrideUrl = resolveSourcePrintingUrl(resolvedOracleId, sourcePrinting, faceIndex, size);
    }
  }

  const requestKey = imageRequestKey(
    cardName,
    size,
    faceIndex,
    isToken,
    filterPower,
    filterToughness,
    filterColors,
    filterSubtypes,
    filterHasAbilities,
    oracleId,
    faceName,
  );

  useEffect(() => {
    if (overrideUrl) {
      setSrc(overrideUrl);
      setIsLoading(false);
      return;
    }

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
          filterSubtypes,
          filterHasAbilities,
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
    filterHasAbilities,
    filterPower,
    filterSubtypes,
    filterToughness,
    isToken,
    oracleId,
    overrideUrl,
    requestKey,
    size,
  ]);

  return { src, isLoading };
}
