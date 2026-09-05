import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  CARD_BACK_URL,
  fetchCardImageAsset,
  fetchCardImageAssetByOracleId,
  fetchTokenImageAssetByRef,
  fetchTokenImageUrl,
  deriveImageUrl,
  findPrintingById,
  getCardPrintings,
  isCardImageFlipLayoutSync,
  isCardImageRotatedSync,
  isLocaleArtReady,
  imageUrlSize,
  loadLocaleArt,
  resolveFaceIndexSync,
  resolveOracleIdSync,
  resolvePrintingImageUrl,
} from "../services/scryfall.ts";
import type { ImageSize, PrintingEntry, TokenSearchFilters } from "../services/scryfall.ts";
import type { CardImageAsset } from "../services/scryfall.ts";
import { applyChain } from "../services/artSelection.ts";
import type { TokenImageRef } from "../adapter/types.ts";
import {
  cardBackCandidate,
  cardCandidateGroups,
  semanticCardCandidateGroups,
  tokenCandidateGroups,
} from "../services/visualPacks/candidateKeys.ts";
import type { VisualVariant } from "../services/visualPacks/candidateKeys.ts";
import type { VisualCandidateGroup } from "../services/visualPacks/repository.ts";
import { visualPackRepository } from "../services/visualPacks/repository.ts";
import type {
  CandidateKey,
  CardImageSource,
  ImageRungs,
  VisualImageRung,
} from "../services/visualPacks/types.ts";
import { packId } from "../services/visualPacks/types.ts";
import { useEffectiveOffline } from "../stores/connectivityStore.ts";
import { usePreferencesStore, registerStrategyCacheClearFn } from "../stores/preferencesStore.ts";
import type { ArtChainEntry } from "../stores/preferencesStore.ts";
import { useFixedVisualImage } from "./useFixedVisualImage.ts";
import { nextImageSourceIndex } from "./imageSourceLadder.ts";

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
  source?: CardImageSource | null;
  rungs?: ImageRungs;
  advanceFailedSource?(failedSrc: string): void;
}

export interface UseCardBackImageResult {
  src: string | null;
  isLoading: boolean;
  source?: CardImageSource | null;
  advanceFailedSource?(failedSrc: string): void;
}

interface MemoryCacheEntry {
  promise: Promise<CardImageAsset | null> | null;
  refCount: number;
  asset: CardImageAsset | null;
}

interface RemoteContinuation {
  generation: string;
  promise: Promise<void> | null;
  settled: boolean;
  start: (() => Promise<void>) | null;
}

function remoteRungs(src: string, size: ImageSize): ImageRungs | undefined {
  return size === "art_crop" || imageUrlSize(src) === null
    ? undefined
    : { small: deriveImageUrl(src, "small"), normal: deriveImageUrl(src, "normal") };
}

function remoteAsset(
  src: string,
  size: ImageSize,
  semantic: CardImageAsset["semantic"],
  isRotated: boolean,
): CardImageAsset {
  const rungs = remoteRungs(src, size);
  return { src, isRotated, rungs, source: { kind: "remote", src, rungs }, semantic };
}

function metadataRepositoryGroups(
  asset: CardImageAsset,
  size: ImageSize,
  cardName: string,
  faceName: string,
  language: string,
  isToken: boolean,
  tokenImageRef: TokenImageRef | null,
) {
  if (isToken && size === "art_crop") return [];
  const requestedRung: VisualImageRung | "large" = size;
  try {
    const build = (rung: VisualImageRung | "large") => isToken
      ? tokenCandidateGroups({
          scryfallId: tokenImageRef?.scryfall_id || undefined,
          oracleId: tokenImageRef?.scryfall_oracle_id || undefined,
          faceName: tokenImageRef?.face_name || undefined,
          presetId: tokenImageRef?.preset_id || undefined,
          faceIndex: asset.semantic.faceIndex,
          rung: rung === "art_crop" ? "normal" : rung,
        })
      : cardCandidateGroups({
          language: language === "en" ? undefined : language,
          englishPrintingId: asset.semantic.englishPrintingId,
          oracleId: asset.semantic.oracleId,
          localizedAliases: [cardName, faceName].filter(Boolean),
          englishAliases: [cardName, faceName].filter(Boolean),
          oracleAliases: [cardName, faceName].filter(Boolean),
          faceIndex: asset.semantic.faceIndex,
          variant: size === "art_crop" ? "art_crop" : "full_card",
          rung,
        });
    const requested = build(requestedRung);
    if (size === "art_crop") {
      return requested.map((group) => ({ requested: group.keys }));
    }
    const small = build("small");
    const normal = build("normal");
    return requested.map((group, index) => ({
      requested: group.keys,
      small: small[index]?.keys,
      normal: normal[index]?.keys,
    }));
  } catch {
    return [];
  }
}

function localCandidateGroups(
  size: ImageSize,
  language: string,
  cardName: string,
  faceName: string,
  resolvedOracleId: string,
  resolvedFaceIndex: number,
  isToken: boolean,
  tokenImageRef: TokenImageRef | null,
  explicitPrintingId: string,
  sourcePrinting: SourcePrinting | undefined,
): VisualCandidateGroup[] {
  if (isToken && size === "art_crop") return [];
  const requestedRung: VisualImageRung | "large" = size;
  const variant: VisualVariant = size === "art_crop" ? "art_crop" : "full_card";
  const groupsWithRungs = (
    build: (rung: VisualImageRung | "large") => Array<{ keys: CandidateKey[] }>,
    options?: Pick<VisualCandidateGroup, "packId" | "requireUnambiguousAsset">,
  ): VisualCandidateGroup[] => {
    try {
      const requested = build(requestedRung);
      if (size === "art_crop") {
        return requested.map((group) => ({ requested: group.keys, ...options }));
      }
      const small = build("small");
      const normal = build("normal");
      return requested.map((group, index) => ({
        requested: group.keys,
        small: small[index]?.keys,
        normal: normal[index]?.keys,
        ...options,
      }));
    } catch {
      return [];
    }
  };
  const exactCardGroups = (printingId: string, includeLocalized = true) => groupsWithRungs((rung) =>
    cardCandidateGroups({
      language: includeLocalized && language !== "en" ? language : undefined,
      englishPrintingId: printingId,
      faceIndex: resolvedFaceIndex,
      variant,
      rung,
    }));
  if (isToken) {
    const exact = tokenImageRef?.scryfall_id ? exactCardGroups(tokenImageRef.scryfall_id, false) : [];
    const reference = groupsWithRungs((rung) => tokenCandidateGroups({
      scryfallId: tokenImageRef?.scryfall_id || undefined,
      oracleId: tokenImageRef?.scryfall_oracle_id || undefined,
      faceName: tokenImageRef?.face_name || undefined,
      faceIndex: resolvedFaceIndex,
      rung: rung === "art_crop" ? "normal" : rung,
    }).slice(0, 1));
    const preset = tokenImageRef?.preset_id
      ? groupsWithRungs((rung) => tokenCandidateGroups({
          presetId: tokenImageRef.preset_id,
          faceIndex: resolvedFaceIndex,
          rung: rung === "art_crop" ? "normal" : rung,
        }).slice(-1))
      : [];
    const tokenFaceName = tokenImageRef?.face_name || faceName || cardName;
    const tokenOracleId = tokenImageRef?.scryfall_oracle_id;
    const oracle = tokenOracleId
      ? groupsWithRungs((rung) => semanticCardCandidateGroups({
          cardName: tokenFaceName,
          faceName: tokenFaceName,
          variant: "full_card",
          oracleId: tokenOracleId.toLowerCase(),
        rung,
      }).slice(0, 1), { requireUnambiguousAsset: true })
      : [];
    const name = tokenFaceName
      ? groupsWithRungs((rung) => semanticCardCandidateGroups({
          cardName: tokenFaceName,
          faceName: tokenFaceName,
          variant: "full_card",
          rung,
        }).slice(-1), { requireUnambiguousAsset: true })
      : [];
    return [...exact, ...reference, ...preset, ...oracle, ...name];
  }

  const exact = explicitPrintingId ? exactCardGroups(explicitPrintingId) : [];
  const semanticIntent = { cardName, faceName: faceName || cardName, variant };
  const source = sourcePrinting?.setCode && sourcePrinting.collectorNumber
    ? groupsWithRungs((rung) => semanticCardCandidateGroups({
        ...semanticIntent,
        sourceSetCode: sourcePrinting.setCode,
        sourceCollectorNumber: sourcePrinting.collectorNumber,
        rung,
      }).slice(0, 1), { packId: packId("deck_library") })
    : [];
  const oracle = resolvedOracleId
    ? groupsWithRungs((rung) => semanticCardCandidateGroups({
        ...semanticIntent,
        oracleId: resolvedOracleId.toLowerCase(),
        rung,
      }).slice(0, 1), { packId: packId("deck_library") })
    : [];
  const name = groupsWithRungs((rung) => semanticCardCandidateGroups({
    ...semanticIntent,
    rung,
  }).slice(-1), { packId: packId("deck_library") });
  return [...exact, ...source, ...oracle, ...name];
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

/**
 * Locales whose card-art map is currently being fetched. Same anti-spin-loop
 * discipline as `strategyInflight`: without it, every render before the map
 * lands would start another fetch.
 *
 * A failed fetch cannot loop either — `loadLocaleArt` swallows errors and
 * resolves an empty map, which still installs, so `isLocaleArtReady` flips true
 * and every card simply keeps its English art.
 */
const localeArtInflight = new Set<string>();

/**
 * Fetch the active locale's card-art map, then invalidate every mounted tile.
 *
 * The dispatch deliberately carries no `detail`: unlike a printings fetch (which
 * concerns one oracleId), a language change re-resolves the URL of every card on
 * screen, and the listener treats a detail-less event as a global invalidation.
 */
function loadLocaleArtInBackground(lang: string): void {
  if (isLocaleArtReady(lang) || localeArtInflight.has(lang)) return;
  localeArtInflight.add(lang);
  loadLocaleArt(lang)
    .then(() => {
      localeArtInflight.delete(lang);
      artCacheEvents.dispatchEvent(new Event("update"));
    })
    .catch(() => {
      localeArtInflight.delete(lang);
    });
}

/**
 * Cache-key component for a resolved image URL. It encodes the *art vocabulary*
 * the URL was produced with, not merely the language: before the locale map
 * arrives every card legitimately resolves to English art, and caching that
 * under a bare `"de"` key would pin it there forever — the background load
 * dispatches an invalidation, but the request key would be unchanged, so the
 * resolution effect would never re-run. Distinguishing pending from ready makes
 * the map's arrival a genuine key change.
 */
function localeArtCacheKey(lang: string): string {
  return isLocaleArtReady(lang) ? lang : `${lang}:pending`;
}

/**
 * Load the active language's card-art map and re-render the caller when it
 * lands, returning the art-locale key its URLs were resolved with.
 *
 * For components that resolve art through `resolvePrintingImageUrl` directly
 * instead of through `useCardImage` — they otherwise render whatever vocabulary
 * happened to be installed at mount and never hear about the map arriving.
 *
 * `useCardImage` deliberately does NOT call this: it already owns an
 * `artCacheEvents` subscription filtered by oracleId, and an unfiltered second
 * one per tile would resurrect the unscoped re-render storm that filter exists
 * to prevent (see the subscription comment in the hook body). Callers of this
 * hook subscribe once per component, not once per rendered card.
 */
export function useLocaleArt(): string {
  const language = usePreferencesStore((s) => s.language);
  const [, setLocaleArtTick] = useState(0);

  useEffect(() => {
    const handler = () => setLocaleArtTick((t) => t + 1);
    artCacheEvents.addEventListener("update", handler);
    return () => artCacheEvents.removeEventListener("update", handler);
  }, []);

  useEffect(() => {
    loadLocaleArtInBackground(language);
  }, [language]);

  return localeArtCacheKey(language);
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
  resolvedOracleId: string,
  resolvedFaceIndex: number,
  // `imageRequestCache` stores the FINAL resolved URL, which differs per
  // language once localized art is applied — so the art locale belongs in the
  // key. The printing-selection caches (`strategyCacheMap`, `printingsCacheMap`)
  // stay language-neutral on purpose: which printing wins is a function of the
  // user's art preferences, not of their language.
  artLocaleKey: string,
  repositoryRevision: string,
  sourcePrinting: SourcePrinting | undefined,
  explicitPrintingId: string,
  artChainKey: string,
  effectiveOffline: boolean,
  artCacheTick: number,
): string {
  return [
    oracleId || cardName,
    cardName,
    oracleId ? faceName : String(faceIndex),
    resolvedOracleId,
    String(resolvedFaceIndex),
    size,
    isToken ? "token" : "card",
    filterPower ?? "",
    filterToughness ?? "",
    filterColors,
    filterSubtypes,
    String(filterHasAbilities),
    tokenImageRefKey,
    artLocaleKey,
    repositoryRevision,
    sourcePrinting ? `${sourcePrinting.setCode.toLowerCase()}:${sourcePrinting.collectorNumber}` : "",
    explicitPrintingId,
    artChainKey,
    String(effectiveOffline),
    String(artCacheTick),
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
  sourcePrinting: SourcePrinting | undefined,
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
    let asset: CardImageAsset;
    if (isToken) {
      let remoteSrc: string | null = null;
      let resolvedFaceIndex = faceIndex;
      if (tokenImageRef) {
        try {
          const tokenAsset = await fetchTokenImageAssetByRef(tokenImageRef, size);
          remoteSrc = tokenAsset?.src ?? null;
          resolvedFaceIndex = tokenAsset?.faceIndex ?? resolvedFaceIndex;
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
      asset = remoteAsset(
        remoteSrc,
        size,
        {
          oracleId: tokenImageRef?.scryfall_oracle_id?.toLowerCase() || undefined,
          faceIndex: resolvedFaceIndex,
          alias: cardName.toLowerCase().normalize("NFC"),
        },
        false,
      );
    } else if (oracleId) {
      asset = await fetchCardImageAssetByOracleId(oracleId, faceName, size);
    } else {
      asset = await fetchCardImageAsset(cardName, faceIndex, size);
    }
    if (sourcePrinting && asset.semantic.oracleId) {
      const printings = await getCardPrintings(asset.semantic.oracleId);
      printingsCacheMap.set(asset.semantic.oracleId, printings);
      const source = printings.find((printing) =>
        printing.set === sourcePrinting.setCode.toLowerCase()
        && printing.collector_number === sourcePrinting.collectorNumber);
      const sourceUrl = source && resolvePrintingImageUrl(source, asset.semantic.faceIndex, size);
      if (sourceUrl) {
        asset = remoteAsset(sourceUrl, size, {
          ...asset.semantic,
          englishPrintingId: source.id.toLowerCase(),
        }, asset.isRotated);
      }
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
        tokenImageRef.preset_id ?? "",
      ].join(":")
    : "";
  // Stabilize the token ref's identity to tokenImageRefKey. fetchTokenImageAssetByRef
  // reads the Scryfall identity while the visual-pack candidate also reads the
  // preset identity. All four axes are captured by the key, so a caller passing
  // a fresh inline {scryfall_id,...} object on every render
  // would otherwise re-fire the image-load effect (release + refetch the cached
  // src) for an unchanged image. exhaustive-deps can't see that the key fully
  // captures the object, so the disable is scoped to this one line rather than
  // blinding the dependency check on the large effect below.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const stableTokenImageRef = useMemo(() => tokenImageRef, [tokenImageRefKey]);
  // A token ref is only a pointer when it names a printing: with BOTH ids
  // empty there is nothing to resolve, so such a ref must not hold the
  // empty-name guards open — the request would fall through to a
  // `fetchTokenImageUrl("")` junk search. Our face-down markers carry only an
  // oracle id (empty `scryfall_id`), so either id keeps the guard open.
  const resolvableTokenImageRef =
    stableTokenImageRef &&
    (stableTokenImageRef.scryfall_id || stableTokenImageRef.scryfall_oracle_id)
      ? stableTokenImageRef
      : null;
  const oracleId = options?.oracleId ?? "";
  const faceName = options?.faceName ?? "";
  const scryfallId = options?.scryfallId ?? "";
  const sourcePrinting = options?.sourcePrinting;
  const sourcePrintingKey = sourcePrinting
    ? `${sourcePrinting.setCode.toLowerCase()}:${sourcePrinting.collectorNumber}`
    : "";
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const stableSourcePrinting = useMemo(() => sourcePrinting, [sourcePrintingKey]);
  const filterPower = tokenFilters?.power ?? null;
  const filterToughness = tokenFilters?.toughness ?? null;
  const filterSubtypes = tokenFilters?.subtypes?.join(",") ?? "";
  const filterColors = tokenFilters?.colors?.join(",") ?? "";
  const filterHasAbilities = tokenFilters?.hasAbilities ?? null;

  const artOverrides = usePreferencesStore((s) => s.artOverrides);
  const artChain = usePreferencesStore((s) => s.artChain);
  const effectiveOffline = useEffectiveOffline();
  // Card art follows the UI language: the printing the user chose is kept, and
  // only its image is swapped for the same printing in their language. Cards
  // with no localized sibling keep their English art.
  const language = usePreferencesStore((s) => s.language);
  const artLocaleKey = localeArtCacheKey(language);

  const [src, setSrc] = useState<string | null>(null);
  const [isRotated, setIsRotated] = useState(false);
  const [isFlip, setIsFlip] = useState(false);
  const [isLoading, setIsLoading] = useState(true);
  const [stateRequestKey, setStateRequestKey] = useState<string | null>(null);
  const [sources, setSources] = useState<CardImageSource[]>([]);
  const [sourceIndex, setSourceIndex] = useState(0);
  const [repositoryRevision, setRepositoryRevision] = useState(
    visualPackRepository.currentRevision(),
  );
  const failedSources = useRef<{ generation: string; values: Set<string> }>({
    generation: "",
    values: new Set(),
  });
  const [artCacheTick, setArtCacheTick] = useState(0);
  const remoteContinuation = useRef<RemoteContinuation>({
    generation: "",
    promise: null,
    settled: true,
    start: null,
  });

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

  useEffect(() => visualPackRepository.subscribe(() => {
    setRepositoryRevision(visualPackRepository.currentRevision());
  }), []);

  // The printings/art-strategy path indexes faces numerically, but for a
  // DFC/MDFC the reliable signal is the engine's `faceName` (an MDFC cast as its
  // back face reports `transformed: false`, so the caller's `faceIndex` is 0 —
  // the front). Resolve the real index from `faceName` here so every override
  // path renders the active face; fall back to the caller's `faceIndex`.
  const resolvedFaceIndex =
    resolveFaceIndexSync(resolvedOracleId, faceName) ?? faceIndex;

  const explicitPrintingId = !isToken
    ? scryfallId || (resolvedOracleId ? artOverrides[resolvedOracleId]?.scryfallId ?? "" : "")
    : "";
  const artChainKey = JSON.stringify(artChain);

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
    resolvedOracleId,
    resolvedFaceIndex,
    artLocaleKey,
    repositoryRevision,
    stableSourcePrinting,
    explicitPrintingId,
    artChainKey,
    effectiveOffline,
    artCacheTick,
  );

  useEffect(() => {
    let cancelled = false;
    let acquiredRemoteCache = false;
    failedSources.current = { generation: requestKey, values: new Set() };
    setStateRequestKey(requestKey);
    setSrc(null);
    setSources([]);
    setSourceIndex(0);
    setIsLoading(true);

    const fallback: CardImageSource[] = [{ kind: "fallback", src: null }];
    const canResolveRemotely = Boolean(cardName || oracleId || resolvableTokenImageRef);
    const publish = (
      nextSources: CardImageSource[],
      imageAsset?: CardImageAsset,
      settled = true,
    ) => {
      if (cancelled) return;
      setSources(nextSources);
      setSourceIndex(0);
      setSrc(nextSources[0]?.src ?? null);
      setIsRotated(imageAsset?.isRotated ?? isCardImageRotatedSync(resolvedOracleId, cardName));
      setIsFlip(isCardImageFlipLayoutSync(resolvedOracleId, cardName));
      setIsLoading(!settled);
    };

    const continuation: RemoteContinuation = {
      generation: requestKey,
      promise: null,
      settled: effectiveOffline,
      start: null,
    };
    remoteContinuation.current = continuation;

    const selectedRemoteOverride = (): CardImageAsset | null => {
      if (isToken || !resolvedOracleId) return null;
      let overrideUrl: string | null = null;
      let overridePrintingId = "";
      if (scryfallId) {
        overrideUrl = resolveOverrideUrl(resolvedOracleId, scryfallId, resolvedFaceIndex, size);
        overridePrintingId = scryfallId;
      } else if (explicitPrintingId) {
        overrideUrl = resolveOverrideUrl(
          resolvedOracleId,
          explicitPrintingId,
          resolvedFaceIndex,
          size,
        );
        overridePrintingId = explicitPrintingId;
      } else if (artChain.length > 0) {
        if (stableSourcePrinting && artChain.some((entry) => entry.type === "source_printing")) {
          const printings = printingsCacheMap.get(resolvedOracleId);
          if (printings) {
            const winner = applyChain(artChain, printings, stableSourcePrinting);
            if (winner) {
              overrideUrl = resolvePrintingImageUrl(winner, resolvedFaceIndex, size);
              overridePrintingId = winner.id;
            }
          } else {
            resolveStrategyInBackground(resolvedOracleId, artChain);
          }
        } else {
          const cached = strategyCacheMap.get(resolvedOracleId);
          if (cached) {
            overrideUrl = resolvePrintingImageUrl(cached, resolvedFaceIndex, size);
            overridePrintingId = cached.id;
          } else {
            resolveStrategyInBackground(resolvedOracleId, artChain);
          }
        }
      } else if (stableSourcePrinting) {
        overrideUrl = resolveSourcePrintingUrl(resolvedOracleId, stableSourcePrinting, resolvedFaceIndex, size);
        const source = printingsCacheMap.get(resolvedOracleId)?.find((printing) =>
          printing.set === stableSourcePrinting.setCode.toLowerCase()
          && printing.collector_number === stableSourcePrinting.collectorNumber);
        overridePrintingId = source?.id ?? "";
      }
      return overrideUrl
        ? remoteAsset(
            overrideUrl,
            size,
            {
              oracleId: resolvedOracleId.toLowerCase(),
              englishPrintingId: overridePrintingId.toLowerCase() || undefined,
              faceIndex: resolvedFaceIndex,
              alias: cardName.toLowerCase().normalize("NFC"),
            },
            isCardImageRotatedSync(resolvedOracleId, cardName),
          )
        : null;
    };

    continuation.start = () => {
      if (continuation.promise) return continuation.promise;
      if (!cancelled) setIsLoading(true);
      continuation.promise = (async () => {
        if (effectiveOffline || !canResolveRemotely) {
          continuation.settled = true;
          return;
        }
        loadLocaleArtInBackground(language);
        try {
          let imageAsset = selectedRemoteOverride();
          if (!imageAsset) {
            acquiredRemoteCache = true;
            imageAsset = await acquireCachedImageSrc(
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
              artChain.length === 0 ? stableSourcePrinting : undefined,
            );
          }
          if (!imageAsset) {
            publish(fallback);
            return;
          }
          const result = await visualPackRepository.resolve({
            groups: metadataRepositoryGroups(
              imageAsset,
              size,
              cardName,
              faceName,
              language,
              isToken,
              stableTokenImageRef,
            ),
            rung: size,
            allowRemote: true,
            remote: { src: imageAsset.src, rungs: imageAsset.rungs },
          });
          const viable = result.sources.filter((source) =>
            source.src === null || !failedSources.current.values.has(source.src));
          publish(viable.length > 0 ? viable : fallback, imageAsset);
        } catch {
          publish(fallback);
        } finally {
          continuation.settled = true;
        }
      })();
      return continuation.promise;
    };

    async function resolveLocal(): Promise<void> {
      const groups = localCandidateGroups(
        size,
        language,
        cardName,
        faceName,
        resolvedOracleId,
        resolvedFaceIndex,
        isToken,
        stableTokenImageRef,
        explicitPrintingId,
        stableSourcePrinting,
      );
      const result = await visualPackRepository.resolve({
        groups,
        rung: size,
        allowRemote: false,
      }).catch(() => ({ sources: fallback }));
      if (cancelled) return;
      const installed = result.sources.some((source) => source.kind === "installed");
      const settled = installed || effectiveOffline || !canResolveRemotely;
      publish(result.sources, undefined, settled);
      if (settled) return;
      void continuation.start?.();
    }

    void resolveLocal();

    return () => {
      cancelled = true;
      if (acquiredRemoteCache) releaseCachedImageSrc(requestKey);
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
    resolvableTokenImageRef,
    stableTokenImageRef,
    tokenImageRefKey,
    isToken,
    language,
    oracleId,
    explicitPrintingId,
    effectiveOffline,
    requestKey,
    resolvedOracleId,
    resolvedFaceIndex,
    size,
    scryfallId,
    stableSourcePrinting,
    artChain,
  ]);

  const activeSource = sources[sourceIndex] ?? null;
  const advanceFailedSource = useCallback((failedSrc: string) => {
    if (failedSources.current.generation !== requestKey) return;
    const nextIndex = nextImageSourceIndex(
      sources,
      sourceIndex,
      failedSources.current.values,
      failedSrc,
    );
    if (nextIndex === null) return;
    const next = sources[nextIndex];
    const continuation = remoteContinuation.current;
    if (
      next?.kind === "fallback"
      && continuation.generation === requestKey
      && !continuation.settled
    ) {
      setSourceIndex(nextIndex);
      setSrc(null);
      void continuation.start?.();
      return;
    }
    setSourceIndex(nextIndex);
    setSrc(next?.src ?? null);
  }, [requestKey, sourceIndex, sources]);

  // Effects reset the state after render, so a component reused for a new card
  // would otherwise expose the previous card's src for one frame. Hand previews
  // intentionally keep one mounted component while scrubbing; gate the result
  // by request identity until the new generation's local or remote stage
  // publishes its own source.
  if (stateRequestKey !== requestKey) {
    return {
      src: null, isLoading: true, isRotated: false, isFlip: false,
      source: null, advanceFailedSource,
    };
  }

  return {
    src,
    isLoading,
    isRotated,
    isFlip,
    source: activeSource,
    rungs: activeSource?.kind === "fallback" ? undefined : activeSource?.rungs,
    advanceFailedSource,
  };
}

/**
 * Resolve the one public fixed card-back identity without accepting any card
 * or object identity from the caller. Hidden-information surfaces use this
 * closed hook instead of submitting a blank or private face to useCardImage.
 */
export function useCardBackImage(): UseCardBackImageResult {
  return useFixedVisualImage(cardBackCandidate(), CARD_BACK_URL);
}
