import type { GameFormat } from "../adapter/types";
import type { CommanderBracket } from "../types/bracket";
import { isCommanderFamilyFormat } from "../types/bracket";
import { CEDH_BRACKET } from "./cedhLock";

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
