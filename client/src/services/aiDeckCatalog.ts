import { useEffect, useState } from "react";

import type { GameFormat, MatchType } from "../adapter/types";
import { getSharedAdapter } from "../adapter/wasm-adapter";
import { evaluateDeckCompatibility } from "./deckCompatibility";
import {
  buildDeckCatalog,
  savedDeckCatalogId,
  type DeckCatalogSource,
} from "./deckCatalog";
import type { DeckArchetype } from "./engineRuntime";
import { expandParsedDeck, type ParsedDeck } from "./deckParser";
import type { CommanderBracket } from "../types/bracket";
import { effectiveBracket, isCommanderFamilyFormat } from "../types/bracket";
import {
  DECKS_CHANGED_EVENT,
  FEED_DECK_ORIGINS_KEY,
  FEED_SUBSCRIPTIONS_KEY,
  STORAGE_KEY_PREFIX,
  onSavedDeckRewritten,
} from "../constants/storage";
import { PROFILE_REPLACED_EVENT } from "../stores/cloudSyncStore";

export type AiDeckSource = DeckCatalogSource;

export interface AiDeckCandidate {
  id: string;
  name: string;
  source: AiDeckSource;
  deck: ParsedDeck;
  knownFormat?: GameFormat;
  coveragePct: number | null;
  archetype: DeckArchetype | null;
  bracket: CommanderBracket | null;
}

export interface AiDeckCatalogOptions {
  selectedFormat?: GameFormat | null;
  selectedMatchType?: MatchType | null;
  /**
   * Set false when the caller receives the catalog from a hoisted call
   * instead (e.g. `AiOpponentConfig` under `GameSetupPage`, which shares one
   * analysis with the human pool): the hook stays idle and returns empty.
   */
  enabled?: boolean;
}

export interface AiDeckCatalogResult {
  candidates: AiDeckCandidate[];
  /**
   * Complete raw universe the legal candidates were filtered from: every
   * saved, feed, and precon deck regardless of format or legality. Pool
   * group toggles clear overrides against this membership (an override on a
   * currently-unlisted deck must not survive a folder-wide unselect), while
   * display, counts, and draws use the legal `candidates`.
   */
  allCandidates: AiDeckCandidate[];
}

export interface UseAiDeckCatalogResult extends AiDeckCatalogResult {
  loading: boolean;
  error: string | null;
  /**
   * Key of the format/match request that produced `candidates`
   * (`catalogRequestKey`), or null before the first scan settles. Survives
   * loading transitions: while a new request scans, the previous production
   * (and its key) is still the visible value, so retained-snapshot consumers
   * must compare this against the key they are currently showing.
   */
  requestKey: string | null;
}

/** Identity of one legal-catalog scan: the format/match request it answers. */
export function catalogRequestKey(
  selectedFormat?: GameFormat | null,
  selectedMatchType?: MatchType | null,
): string {
  return `${selectedFormat ?? ""}::${selectedMatchType ?? ""}`;
}

/**
 * Resolve a candidate's bracket tier for the AI random-pool filter.
 *
 * Applies `effectiveBracket`: prefers an explicit human-declared tag (a
 * curated precon entry, a bundled cEDH deck, or a user-saved bracket) when
 * one exists, and otherwise falls back to the engine's computed bracket
 * estimate — the same `estimate_bracket_for_deck` path the deck-builder
 * audit panel and MyDecks chips use — so the filter has data for the decks
 * that carry no manual tag (feed decks, untagged precons, most saved
 * decks), which would otherwise all surface as `null` and be excluded by
 * every bracket selection. The estimate is a floor (B2–B4; B1/B5 are
 * manual-only), so a manual tag above it is expected, not a conflict.
 *
 * Pre-game metadata only: the value filters the candidate pool and never
 * reaches the Rust game loop. The estimate is meaningful only for the
 * Commander family (CR 903), so non-Commander formats and decks with no
 * commander short-circuit to `null`.
 */
async function resolveBracket(
  deck: ParsedDeck,
  staticBracket: CommanderBracket | null,
  format: GameFormat | undefined,
): Promise<CommanderBracket | null> {
  if (staticBracket !== null) return staticBracket;
  if (!isCommanderFamilyFormat(format)) return null;
  const request = expandParsedDeck(deck);
  if (request.commander.length === 0) return null;
  try {
    const estimate = await getSharedAdapter().estimateBracket(request);
    return effectiveBracket(staticBracket, estimate?.tier ?? null);
  } catch {
    // Adapters without local estimation (Tauri/WebSocket/P2P/server-draft)
    // throw BRACKET_ESTIMATION_UNSUPPORTED. Treat as untagged — the filter
    // simply won't constrain these candidates in those builds.
    return null;
  }
}

async function legalCandidate(
  candidate: AiDeckCandidate,
  options: AiDeckCatalogOptions,
): Promise<AiDeckCandidate | null> {
  const { knownFormat } = candidate;
  if (knownFormat && options.selectedFormat && knownFormat !== options.selectedFormat) return null;

  // Precon decks MUST still pass the legality check (CR 903 + the Commander
  // Rules Committee ban list). WotC ships precons with cards that later get
  // banned (Jeweled Lotus, Mana Crypt, Dockside Extortionist in 2024+) and
  // never retroactively curates the precon lists. The previous short-circuit
  // "if precon, skip compat" let AI opponents auto-pick decks containing
  // banned cards — the engine is the rules authority, no catalog bypass.
  const result = await evaluateDeckCompatibility(candidate.deck, {
    selectedFormat: options.selectedFormat,
    selectedMatchType: options.selectedMatchType,
    summaryOnly: true,
  });
  if (result.selected_format_compatible !== true) return null;
  return {
    ...candidate,
    bracket: await resolveBracket(candidate.deck, candidate.bracket, options.selectedFormat ?? undefined),
    coveragePct: result.coverage && result.coverage.total_unique > 0
      ? Math.round((result.coverage.supported_unique / result.coverage.total_unique) * 100)
      : candidate.coveragePct,
  };
}

export function legacyAiDeckNameToId(name: string): string {
  return savedDeckCatalogId(name);
}

/**
 * Pure bracket filter for AI deck candidates.
 *
 * - `tier === null` — no constraint; returns all candidates unchanged.
 * - `tier !== null` — returns only candidates whose `bracket` equals `tier`.
 *   Untagged candidates (`bracket === null`) are excluded.
 */
export function filterByBracket(
  decks: AiDeckCandidate[],
  tier: CommanderBracket | null,
): AiDeckCandidate[] {
  if (tier === null) return decks;
  return decks.filter((d) => d.bracket === tier);
}

export async function buildLegalAiDeckCatalog(
  options: AiDeckCatalogOptions,
): Promise<AiDeckCatalogResult> {
  const rawCandidates = (await buildDeckCatalog()).map((candidate) => ({
    id: candidate.id,
    name: candidate.name,
    source: candidate.source,
    deck: candidate.deck,
    coveragePct: candidate.coveragePct ?? null,
    archetype: null,
    bracket: candidate.bracket ?? null,
    knownFormat: candidate.knownFormat,
  }));

  // The shared engine worker owns a full card database. Issuing the complete
  // catalog at once fills its message queue with cloned deck lists before the
  // worker can service any of them, which can exhaust WebKit's process budget
  // while the setup page opens. Keep only one compatibility request in flight.
  const candidates: AiDeckCandidate[] = [];
  for (const candidate of rawCandidates) {
    const legalCandidateResult = await legalCandidate(candidate, options);
    if (legalCandidateResult !== null) candidates.push(legalCandidateResult);
  }
  return { candidates, allCandidates: rawCandidates };
}

/**
 * Trailing debounce for library-inventory rescan triggers. Mutations arrive
 * in bursts (migrations rewrite every deck at startup; feed refresh writes
 * each mirror), and each trigger would otherwise start a full sequential
 * analysis; the existing cancellation only drops their results, not their
 * engine work. One trailing run per burst keeps previews fresh without it.
 */
const LIBRARY_RESCAN_DEBOUNCE_MS = 500;

/** Deck-inventory localStorage keys: a cross-tab write to one of these changes the candidate universe. */
function isInventoryStorageKey(key: string | null): boolean {
  return (
    // `clear()` reports a null key: re-read everything.
    key === null ||
    key.startsWith(STORAGE_KEY_PREFIX) ||
    key === FEED_SUBSCRIPTIONS_KEY ||
    key === FEED_DECK_ORIGINS_KEY
  );
}

export function useAiDeckCatalog({
  selectedFormat,
  selectedMatchType,
  enabled = true,
}: AiDeckCatalogOptions): UseAiDeckCatalogResult {
  const [result, setResult] = useState<UseAiDeckCatalogResult>({
    candidates: [],
    allCandidates: [],
    loading: enabled,
    error: null,
    requestKey: null,
  });
  // Library inventory revision: importing, deleting, adopting, or refreshing
  // decks (here, in another tab, or via cloud sync) rescans the catalog so
  // mounted previews never serve decks game start can no longer draw, or
  // hide decks it can. Folder-only changes need no rescan — grouping reacts
  // through the folder index instead.
  const [libraryRevision, setLibraryRevision] = useState(0);

  useEffect(() => {
    if (!enabled) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const scheduleRescan = () => {
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        setLibraryRevision((revision) => revision + 1);
      }, LIBRARY_RESCAN_DEBOUNCE_MS);
    };
    const onStorage = (event: StorageEvent) => {
      if (isInventoryStorageKey(event.key)) scheduleRescan();
    };
    window.addEventListener(DECKS_CHANGED_EVENT, scheduleRescan);
    window.addEventListener(PROFILE_REPLACED_EVENT, scheduleRescan);
    window.addEventListener("storage", onStorage);
    const unsubscribeRewritten = onSavedDeckRewritten(() => scheduleRescan());
    return () => {
      if (timer !== null) clearTimeout(timer);
      window.removeEventListener(DECKS_CHANGED_EVENT, scheduleRescan);
      window.removeEventListener(PROFILE_REPLACED_EVENT, scheduleRescan);
      window.removeEventListener("storage", onStorage);
      unsubscribeRewritten();
    };
  }, [enabled]);

  useEffect(() => {
    if (!enabled) return;
    const key = catalogRequestKey(selectedFormat, selectedMatchType);
    let cancelled = false;
    setResult((current) => ({ ...current, loading: true, error: null }));
    buildLegalAiDeckCatalog({ selectedFormat, selectedMatchType })
      .then((catalog) => {
        if (!cancelled) setResult({ ...catalog, loading: false, error: null, requestKey: key });
      })
      .catch((error) => {
        if (!cancelled) {
          setResult({
            candidates: [],
            allCandidates: [],
            loading: false,
            error: error instanceof Error ? error.message : String(error),
            requestKey: key,
          });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [selectedFormat, selectedMatchType, enabled, libraryRevision]);

  return result;
}

/**
 * Grouping-relevant source equality: `poolGroupForCandidate` routes saved
 * feed mirrors by `feedId` and feed decks by `feedId`, so a provenance-only
 * refresh (adopt a mirror, same id/bracket) must compare as changed.
 * Precon `releaseDate` is display metadata, not grouping, and is ignored.
 */
function sameCatalogSource(a: AiDeckSource, b: AiDeckSource): boolean {
  switch (a.type) {
    case "saved":
      return b.type === "saved" && (a.feedId ?? null) === (b.feedId ?? null);
    case "feed":
      return b.type === "feed" && a.feedId === b.feedId;
    case "precon":
      return b.type === "precon" && a.deckId === b.deckId && a.code === b.code;
  }
}

/**
 * Semantic equality for settled legal candidates: every preview/draw-visible
 * field. The settle bailout must be sensitive, not just stable — dropping a
 * refresh whose ids repeat but whose bracket or provenance changed serves a
 * stale legal universe, while a false "changed" only rebuilds one snapshot.
 */
function sameCatalogCandidates(a: readonly AiDeckCandidate[], b: readonly AiDeckCandidate[]): boolean {
  return (
    a.length === b.length
    && a.every(
      (candidate, index) =>
        candidate.id === b[index].id
        && candidate.bracket === b[index].bracket
        && (candidate.knownFormat ?? null) === (b[index].knownFormat ?? null)
        && (candidate.coveragePct ?? null) === (b[index].coveragePct ?? null)
        && sameCatalogSource(candidate.source, b[index].source),
    )
  );
}

/** Semantic equality for the raw toggle universe: membership plus grouping. */
function sameRawCandidates(a: readonly AiDeckCandidate[], b: readonly AiDeckCandidate[]): boolean {
  return (
    a.length === b.length
    && a.every(
      (candidate, index) =>
        candidate.id === b[index].id
        && candidate.bracket === b[index].bracket
        && sameCatalogSource(candidate.source, b[index].source),
    )
  );
}

/**
 * Last settled catalog for the human pool, bound to the request that produced
 * it. A library-inventory rescan flips `loading` back on, and the deferred
 * preview must keep showing the previous legal universe until the fresh one
 * lands (not flash back to the display universe) — but only within the same
 * format/match request. After the parameters change, the previous production
 * (or a failed new request) must never surface as the current legal snapshot,
 * so the settled value is exposed only while its producing key matches
 * `currentRequestKey`. Updates bail out on equal content rather than identity:
 * callers may pass a freshly-built result object every render, and an
 * unconditional set would loop against that.
 */
export function useSettledAiCatalog(
  catalog: UseAiDeckCatalogResult,
  currentRequestKey: string,
): AiDeckCatalogResult | undefined {
  const [settled, setSettled] = useState<{ key: string; result: AiDeckCatalogResult } | undefined>(
    undefined,
  );
  useEffect(() => {
    if (catalog.loading || catalog.error !== null || catalog.requestKey == null) return;
    const producedKey = catalog.requestKey;
    const produced: AiDeckCatalogResult = {
      candidates: catalog.candidates,
      allCandidates: catalog.allCandidates,
    };
    setSettled((prev) => {
      if (
        prev !== undefined
        && prev.key === producedKey
        && sameCatalogCandidates(prev.result.candidates, produced.candidates)
        && sameRawCandidates(prev.result.allCandidates, produced.allCandidates)
      ) {
        return prev;
      }
      return { key: producedKey, result: produced };
    });
  }, [catalog]);
  return settled !== undefined && settled.key === currentRequestKey ? settled.result : undefined;
}
