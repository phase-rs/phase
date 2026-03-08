/** Action types that don't reveal hidden information and are safe to undo. */
export const UNDOABLE_ACTIONS = new Set([
  "PassPriority",
  "DeclareAttackers",
  "DeclareBlockers",
  "ActivateAbility",
]);

/** Maximum number of undo history entries. */
export const MAX_UNDO_HISTORY = 5;

/** Player ID for the AI opponent (always player 1 in WASM mode). */
export const AI_PLAYER_ID = 1;

/** Base delay in ms before the AI acts (humanizing pause). */
export const AI_BASE_DELAY_MS = 800;

/** Random variance added to AI delay in ms. */
export const AI_DELAY_VARIANCE_MS = 400;
