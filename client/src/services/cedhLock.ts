import type { AIDifficulty } from "../constants/ai";
import type { CommanderBracket } from "../types/bracket";

/**
 * Single source of truth for cEDH semantics on the frontend.
 *
 * cEDH is a table-wide property (every deck must be bracket 5), not a per-seat
 * difficulty. The user enables it via the `cedhMode` toggle (preferencesStore);
 * these helpers translate that flag into the engine's per-seat contract and
 * deck-legality checks.
 *
 * - `effectiveAiDifficulty` — resolve a seat's engine difficulty given `cedhMode`.
 * - `isDeckCedhLegal` — does this deck's declared bracket qualify as B5 cEDH?
 *
 * Every cEDH decision in the frontend flows through these helpers. Adding new
 * checks elsewhere is a defect — they belong here.
 */

/** The difficulty string the Rust engine uses for cEDH. */
export const CEDH_DIFFICULTY: AIDifficulty = "CEDH";

/** The numeric bracket tier for cEDH (bracket 5). */
export const CEDH_BRACKET: CommanderBracket = 5;

/**
 * Resolve a seat's engine difficulty given the table-wide cEDH toggle.
 *
 * When cEDH mode is on, every AI seat plays at cEDH (bracket 5) regardless of
 * its remembered per-seat difficulty; otherwise the seat's own difficulty
 * applies. This is the single chokepoint mapping the UX-level `cedhMode` to the
 * engine's per-seat difficulty contract — used both when persisting the game's
 * seat snapshot and when building the deck list at game start.
 */
export function effectiveAiDifficulty(
  difficulty: AIDifficulty,
  cedhMode: boolean,
): AIDifficulty {
  return cedhMode ? CEDH_DIFFICULTY : difficulty;
}

/**
 * Returns true when the deck's declared bracket tier is bracket 5 (cEDH).
 * `null` means the deck has no bracket tag and is therefore not cEDH-legal.
 */
export function isDeckCedhLegal(bracket: CommanderBracket | null): boolean {
  return bracket === CEDH_BRACKET;
}
