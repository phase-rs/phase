import { useEffect, useState } from "react";

import {
  fetchCardImageAsset,
  fetchCardImageAssetByOracleId,
  fetchTokenImageByRef,
  fetchTokenImageUrl,
  findPrintingById,
  getCardPrintings,
  isCardImageFlipLayoutSync,
  isCardImageRotatedSync,
  resolveFaceIndexSync,
  resolveOracleIdSync,
  resolvePrintingImageUrl,
} from "../services/scryfall.ts";
import type { ImageSize, PrintingEntry, TokenSearchFilters } from "../services/scryfall.ts";
import type { CardImageAsset } from "../services/scryfall.ts";
import type { TokenImageRef } from "../adapter/types.ts";
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
  tokenImageRef?: TokenImageRef | null;
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
  isRotated: boolean;
  /** True for Kamigawa-style flip cards (`layout: "flip"`), whose alternate half
   *  is the same image rotated 180°. The preview uses this to enable Ctrl-spin. */
  isFlip: boolean;
}

interface MemoryCacheEntry {
  promise: Promise<CardImageAsset | null> | null;
  refCount: number;
  asset: CardImageAsset | null;
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
  tokenImageRefKey: string,
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
    tokenImageRefKey,
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
  tokenImageRef: TokenImageRef | null,
  oracleId: string,
  faceName: string,
): Promise<CardImageAsset | null> {
  const existing = imageRequestCache.get(key);
  if (existing) {
    existing.refCount += 1;
    if (existing.asset !== null) return existing.asset;
    if (existing.promise) return existing.promise;
  }

  const entry: MemoryCacheEntry = {
    promise: null,
    refCount: 1,
    asset: null,
  };
  imageRequestCache.set(key, entry);

  entry.promise = (async () => {
    let asset: CardImageAsset | null;
    if (isToken) {
      let remoteSrc: string | null = null;
      if (tokenImageRef) {
        try {
          remoteSrc = await fetchTokenImageByRef(tokenImageRef, size);
        } catch {
          remoteSrc = null;
        }
      }
      remoteSrc ??= await fetchTokenImageUrl(cardName, size, {
        power: filterPower,
        toughness: filterToughness,
        colors: filterColors ? filterColors.split(",") : undefined,
        subtypes: filterSubtypes ? filterSubtypes.split(",") : undefined,
        hasAbilities: filterHasAbilities ?? undefined,
      });
      asset = { src: remoteSrc, isRotated: false };
    } else if (oracleId) {
      asset = await fetchCardImageAssetByOracleId(oracleId, faceName, size);
    } else {
      asset = await fetchCardImageAsset(cardName, faceIndex, size);
    }
    entry.asset = asset;
    entry.promise = null;
    if (entry.refCount === 0) {
      imageRequestCache.delete(key);
    }
    return asset;
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
  const tokenImageRef = options?.tokenImageRef ?? null;
  const tokenImageRefKey = tokenImageRef
    ? [
        tokenImageRef.scryfall_id,
        tokenImageRef.scryfall_oracle_id ?? "",
        tokenImageRef.face_name ?? "",
      ].join(":")
    : "";
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
  const [isRotated, setIsRotated] = useState(false);
  const [isFlip, setIsFlip] = useState(false);
  const [isLoading, setIsLoading] = useState(true);
  const [, setArtCacheTick] = useState(0);

  useEffect(() => {
    const handler = () => setArtCacheTick((t) => t + 1);
    artCacheEvents.addEventListener("update", handler);
    return () => artCacheEvents.removeEventListener("update", handler);
  }, []);

  const resolvedOracleId = oracleId || resolveOracleIdSync(cardName) || "";

  // The printings/art-strategy path indexes faces numerically, but for a
  // DFC/MDFC the reliable signal is the engine's `faceName` (an MDFC cast as its
  // back face reports `transformed: false`, so the caller's `faceIndex` is 0 —
  // the front). Resolve the real index from `faceName` here so every override
  // path renders the active face; fall back to the caller's `faceIndex`.
  const resolvedFaceIndex =
    resolveFaceIndexSync(resolvedOracleId, faceName) ?? faceIndex;

  let overrideUrl: string | null = null;
  if (!isToken && resolvedOracleId) {
    if (scryfallId) {
      overrideUrl = resolveOverrideUrl(resolvedOracleId, scryfallId, resolvedFaceIndex, size);
    } else if (artOverrides[resolvedOracleId]) {
      overrideUrl = resolveOverrideUrl(resolvedOracleId, artOverrides[resolvedOracleId].scryfallId, resolvedFaceIndex, size);
    } else if (artChain.length > 0) {
      if (sourcePrinting && artChain.some((e) => e.type === "source_printing")) {
        const printings = printingsCacheMap.get(resolvedOracleId);
        if (printings) {
          const winner = applyChain(artChain, printings, sourcePrinting);
          if (winner) {
            overrideUrl = resolvePrintingImageUrl(winner, resolvedFaceIndex, size);
          }
        } else {
          resolveStrategyInBackground(resolvedOracleId, artChain);
        }
      } else {
        const cached = strategyCacheMap.get(resolvedOracleId);
        if (cached) {
          overrideUrl = resolvePrintingImageUrl(cached, resolvedFaceIndex, size);
        } else {
          resolveStrategyInBackground(resolvedOracleId, artChain);
        }
      }
    } else if (sourcePrinting) {
      overrideUrl = resolveSourcePrintingUrl(resolvedOracleId, sourcePrinting, resolvedFaceIndex, size);
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
    tokenImageRefKey,
    oracleId,
    faceName,
  );

  useEffect(() => {
    if (overrideUrl) {
      setSrc(overrideUrl);
      setIsRotated(isCardImageRotatedSync(resolvedOracleId, cardName));
      setIsFlip(isCardImageFlipLayoutSync(resolvedOracleId, cardName));
      setIsLoading(false);
      return;
    }

    if (!cardName && !oracleId) {
      setSrc(null);
      setIsRotated(false);
      setIsFlip(false);
      setIsLoading(false);
      return;
    }

    let cancelled = false;

    async function loadImage() {
      setIsLoading(true);
      setSrc(null);

      try {
        const imageAsset = await acquireCachedImageSrc(
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
          tokenImageRef,
          oracleId,
          faceName,
        );
        if (!cancelled) {
          setSrc(imageAsset?.src || null);
          setIsRotated(imageAsset?.isRotated ?? false);
          setIsFlip(isCardImageFlipLayoutSync(resolvedOracleId, cardName));
          setIsLoading(false);
        }
      } catch {
        if (!cancelled) {
          setIsRotated(false);
          setIsFlip(false);
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
    tokenImageRef,
    tokenImageRefKey,
    isToken,
    oracleId,
    overrideUrl,
    requestKey,
    resolvedOracleId,
    size,
  ]);

  return { src, isLoading, isRotated, isFlip };
}
