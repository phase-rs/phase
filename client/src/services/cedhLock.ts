import type { AiSeatPref } from "../stores/preferencesStore";
import type { CommanderBracket } from "../types/bracket";

/**
 * Single source of truth for cEDH bracket-lock semantics on the frontend.
 *
 * - `anyAiOpponentIsCedh` — does any AI seat want cEDH difficulty?
 * - `applyCedhCascade` — when one AI is cEDH, all AI seats must be cEDH.
 *   Pure: returns a new array; never mutates input.
 * - `isDeckCedhLegal` — does this deck's declared bracket qualify as B5 cEDH?
 *
 * Every cEDH-lock decision in the frontend flows through these three helpers.
 * Adding new checks elsewhere is a defect — they belong here.
 */

/** The difficulty string used by the Rust engine for cEDH. */
export const CEDH_DIFFICULTY = "CEDH";

/** The numeric bracket tier for cEDH (bracket 5). */
export const CEDH_BRACKET: CommanderBracket = 5;

/**
 * Returns true when at least one AI seat has cEDH difficulty selected.
 *
 * Accepts `AiSeatPref[]` directly rather than the full `GameSetupConfig` to
 * avoid coupling the helper to the store's shape.
 */
export function anyAiOpponentIsCedh(seats: AiSeatPref[]): boolean {
  return seats.some((s) => s.difficulty === CEDH_DIFFICULTY);
}

/**
 * When any AI seat is set to cEDH, all AI seats must also be cEDH (the
 * bracket-5 lock is table-wide). Returns a new array; never mutates input.
 *
 * If no seat is cEDH, returns the original array reference unchanged.
 */
export function applyCedhCascade(seats: AiSeatPref[]): AiSeatPref[] {
  if (!anyAiOpponentIsCedh(seats)) {
    return seats;
  }
  return seats.map((s) => ({ ...s, difficulty: CEDH_DIFFICULTY as AiSeatPref["difficulty"] }));
}

/**
 * Returns true when the deck's declared bracket tier is bracket 5 (cEDH).
 * `null` means the deck has no bracket tag and is therefore not cEDH-legal.
 */
export function isDeckCedhLegal(bracket: CommanderBracket | null): boolean {
  return bracket === CEDH_BRACKET;
}
