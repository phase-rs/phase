import type { GameFormat } from "../adapter/types";
import { formatMetadata } from "../data/formatRegistry";
import { BRACKET_TIER_NUMERIC, type CommanderBracketTier } from "./bracketEstimate";
export {
  BRACKET_AXES,
  BRACKET_TIER_NUMERIC,
  type BracketAxis,
  type BracketDeckRequest,
  type BracketEstimate,
  type BracketViolation,
  type CommanderBracketTier,
  isBracketEstimate,
} from "./bracketEstimate";

/**
 * WotC Commander bracket tiers (1 Exhibition → 5 cEDH). Used only as
 * pre-game metadata for filtering the AI random deck pool and for an
 * optional descriptive tag on user-saved Commander decks. The value
 * never reaches the Rust engine.
 */
export type CommanderBracket = 1 | 2 | 3 | 4 | 5;

export const COMMANDER_BRACKETS: readonly CommanderBracket[] = [1, 2, 3, 4, 5] as const;

export const BRACKET_LABEL: Record<CommanderBracket, string> = {
  1: "Exhibition",
  2: "Core",
  3: "Upgraded",
  4: "Optimized",
  5: "cEDH",
};

/** Type guard for arbitrary persisted/external values. */
export function isCommanderBracket(value: unknown): value is CommanderBracket {
  return value === 1 || value === 2 || value === 3 || value === 4 || value === 5;
}

/** Inverse of `BRACKET_TIER_NUMERIC`: numeric bracket → engine tier string. */
export const BRACKET_NUMERIC_TIER: Record<CommanderBracket, CommanderBracketTier> = {
  1: "exhibition",
  2: "core",
  3: "upgraded",
  4: "optimized",
  5: "cedh",
};

/**
 * Effective bracket for display and pool filtering: an explicit
 * human-declared tag always wins over the engine's computed estimate, and
 * untagged decks fall back to the estimate. The estimate is a floor — a
 * pilot declaring *above* it (a 0-Game Changer deck they call B3, a B4
 * estimate they call cEDH) is expected, not a conflict — so the manual
 * tag is returned as-is in both directions. Under-reporting (manual
 * below the estimate) is still flagged via `isBracketMismatch`, but the
 * declared tag remains the effective value: the pilot's word is final.
 */
export function effectiveBracket(
  manual: CommanderBracket | null,
  estimateTier: CommanderBracketTier | null,
): CommanderBracket | null {
  if (manual !== null) return manual;
  if (estimateTier === null) return null;
  return BRACKET_TIER_NUMERIC[estimateTier];
}

/**
 * True when a manual tag under-reports the deck: the engine estimate (a
 * floor) sits above the declared bracket, so the deck contains cards
 * illegal at the declared tier. Declaring *at or above* the estimate is
 * always allowed, as is declaring B1 Exhibition on a B2-floor estimate —
 * the estimator never returns B1 (its card criteria are nearly identical
 * to B2's), so B1 is purely a pilot self-declaration.
 */
export function isBracketMismatch(
  manual: CommanderBracket | null,
  estimateTier: CommanderBracketTier,
): boolean {
  if (manual === null) return false;
  const estimated = BRACKET_TIER_NUMERIC[estimateTier];
  if (manual >= estimated) return false;
  if (manual === 1 && estimated === 2) return false;
  return true;
}

/**
 * Returns true when the format uses a commander zone and would benefit
 * from bracket analysis. Reads `FormatConfig.uses_commander` from
 * `FORMAT_REGISTRY` (verified against the engine's `getFormatRegistry`
 * WASM export), so this never drifts from the engine's authoritative list.
 */
export function isCommanderFamilyFormat(format: GameFormat | undefined | null): boolean {
  if (format == null) return false;
  return formatMetadata(format)?.default_config.uses_commander === true;
}

/**
 * Tailwind utility classes for the colored chip rendered for each tier.
 * Shared by `BracketAuditPanel` and `BracketEstimateChip`.
 */
export const BRACKET_TIER_CHIP_CLASS: Record<CommanderBracketTier, string> = {
  exhibition: "border-slate-300/60 bg-slate-500/30 text-slate-100",
  core: "border-emerald-300/60 bg-emerald-500/30 text-emerald-100",
  upgraded: "border-indigo-300/60 bg-indigo-500/30 text-indigo-100",
  optimized: "border-amber-300/60 bg-amber-500/30 text-amber-100",
  cedh: "border-rose-300/60 bg-rose-500/30 text-rose-100",
};
