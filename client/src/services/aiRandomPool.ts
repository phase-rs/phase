import type { GameFormat } from "../adapter/types";
import type { CommanderBracket } from "../types/bracket";
import { isCommanderFamilyFormat } from "../types/bracket";
import { AI_DECK_RANDOM, type AiArchetypeFilter, type AiDeckSelection } from "../stores/preferencesStore";
import type { AiDeckCandidate } from "./aiDeckCatalog";
import { CEDH_BRACKET } from "./cedhLock";
import { pickRandomDeckCandidate } from "./randomDeckSelection";

/**
 * Minimal shape for AI random-pool filtering: anything carrying the resolved
 * bracket tier (`AiDeckCandidate` in production, lighter fakes in tests).
 */
export interface BracketTaggedCandidate {
  bracket: CommanderBracket | null;
}

export interface AiRandomPoolOptions {
  /** Manual multi-bracket selection. Empty = filter off (no constraint). */
  bracketFilter: readonly CommanderBracket[];
  /** Table-wide cEDH toggle. Commander formats only; wins over `bracketFilter`. */
  cedhMode: boolean;
  selectedFormat?: GameFormat | null;
}

/**
 * Restrict the AI Random pool to the table's bracket constraint.
 *
 * Single source of truth shared by the setup page (`AiOpponentConfig` pool
 * preview) and game start (`GameProvider` deck resolution), so both sides
 * apply the same restriction:
 *
 * - Non-Commander formats (or no format): brackets are Commander-family
 *   metadata only — no constraint, pool unchanged.
 * - cEDH mode: bracket 5 only. cEDH is a table property ("every deck must be
 *   bracket 5"), so it wins over the manual multi-bracket selection.
 * - Manual selection non-empty: only candidates tagged with a selected tier.
 *   Untagged candidates (`bracket === null`) are excluded.
 * - Otherwise (filter off): pool unchanged.
 *
 * Pinned seats bypass the pool entirely — callers resolve explicit deck ids
 * against the full legal catalog.
 */
export function restrictAiPoolByBracket<T extends BracketTaggedCandidate>(
  candidates: readonly T[],
  options: AiRandomPoolOptions,
): T[] {
  if (!isCommanderFamilyFormat(options.selectedFormat ?? undefined)) return [...candidates];
  if (options.cedhMode) return candidates.filter((candidate) => candidate.bracket === CEDH_BRACKET);
  if (options.bracketFilter.length === 0) return [...candidates];
  return candidates.filter(
    (candidate) => candidate.bracket !== null && options.bracketFilter.includes(candidate.bracket),
  );
}

/**
 * Whether a candidate survives the Random-pool archetype + coverage filters.
 * Candidates with unknown coverage or archetype are never excluded by the
 * corresponding filter.
 */
export function candidatePassesFilters(
  candidate: AiDeckCandidate,
  archetypeFilter: AiArchetypeFilter,
  coverageFloor: number,
): boolean {
  if (candidate.coveragePct != null && candidate.coveragePct < coverageFloor) return false;
  return archetypeFilter === "Any" || !candidate.archetype || candidate.archetype === archetypeFilter;
}

/**
 * Resolve one AI seat's deck. An explicit `requestedDeckId` is looked up in the
 * full legal `catalog` (bypassing every filter); a Random or unknown id draws
 * from `pool`.
 */
export function pickOpponentDeck(
  catalog: AiDeckCandidate[],
  pool: AiDeckCandidate[],
  requestedDeckId: AiDeckSelection,
  excludeIds: Set<string>,
  archetypeFilter: AiArchetypeFilter,
  coverageFloor: number,
  selectedFormat?: GameFormat | null,
): AiDeckCandidate {
  if (requestedDeckId !== AI_DECK_RANDOM) {
    const pinned = catalog.find((candidate) => candidate.id === requestedDeckId);
    if (pinned) return pinned;
  }

  // Random seats draw from `pool`: the bracket-restricted pool when it is
  // non-empty, else the full legal catalog (an empty restriction warns at
  // setup, so reaching here means the fallback was accepted). Archetype +
  // coverage are soft preferences applied within that pool.
  const filtered = pool.filter((candidate) =>
    candidatePassesFilters(candidate, archetypeFilter, coverageFloor)
  );
  return pickRandomDeckCandidate(filtered.length > 0 ? filtered : pool, {
    selectedFormat,
    excludeIds,
  }) ?? catalog[0];
}

export interface CedhSeatProblemOptions {
  /** Full legal catalog for the table. */
  catalog: AiDeckCandidate[];
  /** One entry per AI seat: a catalog id or `AI_DECK_RANDOM`. */
  requestedDeckIds: readonly AiDeckSelection[];
  bracketFilter: readonly CommanderBracket[];
  cedhMode: boolean;
  selectedFormat?: GameFormat | null;
}

/** Why a cEDH table's AI seats cannot all be served a bracket-5 deck. */
export type CedhSeatProblem = "PinnedDeckNotCedh" | "NoCedhDeckForRandom";

/**
 * The first reason a cEDH table (Commander family only) has an AI seat that no
 * legal deck can serve, or `null` when every seat can be served. The engine's
 * cEDH bracket gate refuses any non-bracket-5 deck at init, so callers must
 * refuse to start on a non-null result:
 *
 * - `PinnedDeckNotCedh`: a seat is pinned to a catalog deck that is not
 *   bracket 5. Pinned seats bypass the Random pool, so the deck would reach the
 *   gate unchanged and be refused.
 * - `NoCedhDeckForRandom`: the bracket-restricted pool is empty and some seat is
 *   Random or pinned to an id missing from `catalog`. Random would fall back to
 *   a non-bracket-5 deck.
 *
 * Archetype and coverage filters are soft preferences and play no part here.
 */
export function cedhSeatProblem(options: CedhSeatProblemOptions): CedhSeatProblem | null {
  const { catalog, requestedDeckIds, bracketFilter, cedhMode, selectedFormat } = options;
  if (!cedhMode || !isCommanderFamilyFormat(selectedFormat ?? undefined)) return null;
  const pinnedNonCedh = requestedDeckIds.some((requestedDeckId) =>
    catalog.some((candidate) => candidate.id === requestedDeckId && candidate.bracket !== CEDH_BRACKET),
  );
  if (pinnedNonCedh) return "PinnedDeckNotCedh";
  if (restrictAiPoolByBracket(catalog, { bracketFilter, cedhMode, selectedFormat }).length > 0) {
    return null;
  }
  const randomUnservable = requestedDeckIds.some((requestedDeckId) =>
    requestedDeckId === AI_DECK_RANDOM || !catalog.some((candidate) => candidate.id === requestedDeckId),
  );
  return randomUnservable ? "NoCedhDeckForRandom" : null;
}

export interface PickAiSeatDecksOptions {
  /** Full legal catalog for the table. Callers guarantee it is non-empty. */
  catalog: AiDeckCandidate[];
  /** One entry per AI seat, in seat order: a catalog id or `AI_DECK_RANDOM`. */
  requestedDeckIds: readonly AiDeckSelection[];
  archetypeFilter: AiArchetypeFilter;
  coverageFloor: number;
  bracketFilter: readonly CommanderBracket[];
  cedhMode: boolean;
  selectedFormat?: GameFormat | null;
  /** Decks already taken at the table (e.g. a random player deck); Random seats
   *  avoid them while alternatives exist. */
  excludeIds?: Iterable<string>;
}

/**
 * Assign a concrete catalog deck to every AI seat. The single authority shared
 * by local game start (`GameProvider`) and the Host Game form, so both apply
 * the same Random-pool rules:
 *
 * - Pinned seats resolve against the full legal catalog and bypass every filter.
 * - Random seats draw from the bracket/cEDH-restricted pool (falling back to the
 *   full catalog when that restriction matches nothing), narrowed by the
 *   archetype + coverage preferences, and never repeat a deck already taken at
 *   the table while alternatives exist.
 */
export function pickAiSeatDecks(options: PickAiSeatDecksOptions): AiDeckCandidate[] {
  const {
    catalog,
    requestedDeckIds,
    archetypeFilter,
    coverageFloor,
    bracketFilter,
    cedhMode,
    selectedFormat,
  } = options;
  const bracketPool = restrictAiPoolByBracket(catalog, { bracketFilter, cedhMode, selectedFormat });
  const randomPool = bracketPool.length > 0 ? bracketPool : catalog;
  const excludeIds = new Set<string>(options.excludeIds);
  return requestedDeckIds.map((requestedDeckId) => {
    const pick = pickOpponentDeck(
      catalog,
      randomPool,
      requestedDeckId,
      excludeIds,
      archetypeFilter,
      coverageFloor,
      selectedFormat,
    );
    excludeIds.add(pick.id);
    return pick;
  });
}
