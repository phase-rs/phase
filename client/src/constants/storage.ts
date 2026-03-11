/** Prefix for saved deck data in localStorage. Full key: `${STORAGE_KEY_PREFIX}${deckName}` */
export const STORAGE_KEY_PREFIX = "phase-deck:";

/** Key for the currently selected/active deck name in localStorage */
export const ACTIVE_DECK_KEY = "phase-active-deck";

/** Prefix for per-game saved state. Full key: `${GAME_KEY_PREFIX}${gameId}` */
export const GAME_KEY_PREFIX = "phase-game:";

/** Key for the active game metadata (id, mode, difficulty) */
export const ACTIVE_GAME_KEY = "phase-active-game";

/** List all saved deck names from localStorage, sorted alphabetically. */
export function listSavedDeckNames(): string[] {
  const names: string[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (key?.startsWith(STORAGE_KEY_PREFIX)) {
      names.push(key.slice(STORAGE_KEY_PREFIX.length));
    }
  }
  return names.sort();
}
