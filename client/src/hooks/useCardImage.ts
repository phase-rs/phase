import { useEffect, useMemo, useRef, useState } from "react";

import {
  fetchCardImageAsset,
  fetchCardImageAssetByOracleId,
  fetchTokenImageByRef,
  fetchTokenImageUrl,
  findPrintingById,
  getCardPrintings,
  isCardImageFlipLayoutSync,
  isCardImageRotatedSync,
  pickOldestPrinting,
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
/**
 * Oracle IDs we've already checked and found to have no printings in
 * `scryfall-printings.json`. Without this negative cache, every render of a
 * deck tile whose representative card is missing from the printings catalog
 * (tokens, name mismatches, newly-released cards not yet in the cached JSON)
 * spin-loops: cache miss → background fetch returns [] → dispatch update
 * event → tile re-renders → cache still missing → fetch again, forever. The
 * empty-result case must short-circuit subsequent calls just like a positive
 * cache hit does. Profile recording confirmed 30+ tiles updating per commit
 * across 670 commits — one missing oracleId per tile is enough to stall the
 * deck-select screen.
 */
const printingsNegativeCache = new Set<string>();

/**
 * Oracle IDs where `printings.length > 0` but `applyChain` returned `null` —
 * the card has printings, but none match the user's current art-chain
 * preferences (e.g., user prefers borderless but no borderless exists).
 * Without this set, render-time misses on `strategyCacheMap` re-trigger
 * `resolveStrategyInBackground` → cached fetch returns instantly → dispatch
 * `update` event → `setArtCacheTick(+1)` → re-render → re-fetch loop at
 * ~70 Hz. Distinct from `printingsNegativeCache` which covers
 * `printings.length === 0` (no printings at all). Cleared together when art
 * preferences change.
 */
const strategyNoWinnerCache = new Set<string>();

registerStrategyCacheClearFn(() => {
  strategyCacheMap.clear();
  strategyInflight.clear();
  printingsNegativeCache.clear();
  strategyNoWinnerCache.clear();
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
      return pickOldestPrinting(printings);
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
  if (printingsNegativeCache.has(oracleId)) return;
  // Already determined the chain produces no winner for this oracleId; refetching
  // would land back here and dispatch another update event, looping the consumer.
  if (strategyNoWinnerCache.has(oracleId)) return;
  strategyInflight.add(oracleId);

  getCardPrintings(oracleId).then((printings) => {
    if (printings.length > 0) {
      printingsCacheMap.set(oracleId, printings);
      const winner = applyChain(chain, printings);
      if (winner) {
        strategyCacheMap.set(oracleId, winner);
      } else {
        // Printings exist but the chain matched nothing — remember that so the
        // next render's strategyCacheMap miss does not re-enter the fetch loop.
        strategyNoWinnerCache.add(oracleId);
      }
    } else {
      printingsNegativeCache.add(oracleId);
    }
    strategyInflight.delete(oracleId);
    artCacheEvents.dispatchEvent(new CustomEvent("update", { detail: oracleId }));
  }).catch(() => {
    strategyInflight.delete(oracleId);
  });
}

function loadPrintingsInBackground(oracleId: string): void {
  if (strategyInflight.has(oracleId)) return;
  if (printingsNegativeCache.has(oracleId)) return;
  strategyInflight.add(oracleId);

  getCardPrintings(oracleId).then((printings) => {
    if (printings.length > 0) {
      printingsCacheMap.set(oracleId, printings);
    } else {
      printingsNegativeCache.add(oracleId);
    }
    strategyInflight.delete(oracleId);
    artCacheEvents.dispatchEvent(new CustomEvent("update", { detail: oracleId }));
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
  if (printingsNegativeCache.has(oracleId)) return null;

  getCardPrintings(oracleId).then((printings) => {
    if (printings.length > 0) {
      printingsCacheMap.set(oracleId, printings);
      artCacheEvents.dispatchEvent(new CustomEvent("update", { detail: oracleId }));
    } else {
      printingsNegativeCache.add(oracleId);
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
  // Stabilize the token ref's identity to tokenImageRefKey. fetchTokenImageByRef
  // reads only scryfall_id / scryfall_oracle_id / face_name — all captured by the
  // key (preset_id is intentionally excluded; it doesn't affect image lookup) —
  // so a caller passing a fresh inline {scryfall_id,...} object on every render
  // would otherwise re-fire the image-load effect (release + refetch the cached
  // src) for an unchanged image. exhaustive-deps can't see that the key fully
  // captures the object, so the disable is scoped to this one line rather than
  // blinding the dependency check on the large effect below.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const stableTokenImageRef = useMemo(() => tokenImageRef, [tokenImageRefKey]);
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

  const resolvedOracleId = oracleId || resolveOracleIdSync(cardName) || "";

  // Scope the cache subscription to this hook's oracleId so a background
  // printings fetch for card A doesn't force re-renders on every other deck
  // tile mounted with `useCardImage`. With ~100 deck tiles on the deck-select
  // screen and ~200 lazy printings fetches, an unscoped bus produced ~20,000
  // re-renders (heap snapshot Heap-20260526T075828 — the sawtooth that peaked
  // the tab at 500 MB). The ref keeps the subscription stable (mounted once
  // like the original) so we don't race a re-subscribe against the first
  // dispatch; the per-oracleId filter happens inside the handler.
  const oracleIdRef = useRef(resolvedOracleId);
  oracleIdRef.current = resolvedOracleId;
  useEffect(() => {
    const handler = (e: Event) => {
      const target = oracleIdRef.current;
      if (!target) return;
      const detail = (e as CustomEvent<string>).detail;
      // Be tolerant of any plain `Event` dispatch (no detail) — treat as a
      // global invalidation match. All in-tree dispatchers send a CustomEvent
      // with detail; this is defensive against future callers.
      if (detail && detail !== target) return;
      setArtCacheTick((t) => t + 1);
    };
    artCacheEvents.addEventListener("update", handler);
    return () => artCacheEvents.removeEventListener("update", handler);
  }, []);

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
          stableTokenImageRef,
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
    stableTokenImageRef,
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
