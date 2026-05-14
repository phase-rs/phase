import type { CommanderBracketTier } from "../adapter/types";

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
