/** Action types that don't reveal hidden information and are safe to undo. */
export const UNDOABLE_ACTIONS = new Set([
  "PassPriority",
  "DeclareAttackers",
  "DeclareBlockers",
  "ActivateAbility",
]);

/** Maximum number of undo history entries. */
export const MAX_UNDO_HISTORY = 5;

/** Player ID for the human player (always player 0). */
export const PLAYER_ID = 0;

/** Player ID for the AI opponent (always player 1 in WASM mode). */
export const AI_PLAYER_ID = 1;

/** Brief visual beat between auto-passed phases in ms. */
export const AUTO_PASS_BEAT_MS = 200;

/** Base delay in ms before the AI acts (humanizing pause). */
export const AI_BASE_DELAY_MS = 500;

/** Random variance added to AI delay in ms. */
export const AI_DELAY_VARIANCE_MS = 400;
