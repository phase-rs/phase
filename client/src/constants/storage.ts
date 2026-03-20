/** Prefix for saved deck data in localStorage. Full key: `${STORAGE_KEY_PREFIX}${deckName}` */
export const STORAGE_KEY_PREFIX = "phase-deck:";

/** Key for the currently selected/active deck name in localStorage */
export const ACTIVE_DECK_KEY = "phase-active-deck";

/** Prefix for per-game saved state. Full key: `${GAME_KEY_PREFIX}${gameId}` */
export const GAME_KEY_PREFIX = "phase-game:";

/** Key for the active game metadata (id, mode, difficulty) */
export const ACTIVE_GAME_KEY = "phase-active-game";

/** Key for deck metadata (timestamps, source tracking) */
export const DECK_METADATA_KEY = "phase-deck-metadata";

export interface DeckMeta {
  addedAt: number;
}

function loadMetadataStore(): Record<string, DeckMeta> {
  try {
    const raw = localStorage.getItem(DECK_METADATA_KEY);
    return raw ? (JSON.parse(raw) as Record<string, DeckMeta>) : {};
  } catch {
    return {};
  }
}

function saveMetadataStore(store: Record<string, DeckMeta>): void {
  localStorage.setItem(DECK_METADATA_KEY, JSON.stringify(store));
}

/** Stamp metadata for a deck. Call whenever a deck is saved or seeded. */
export function stampDeckMeta(deckName: string, addedAt?: number): void {
  const store = loadMetadataStore();
  if (!store[deckName]) {
    store[deckName] = { addedAt: addedAt ?? Date.now() };
    saveMetadataStore(store);
  }
}

/** Get metadata for a single deck, or null if not tracked. */
export function getDeckMeta(deckName: string): DeckMeta | null {
  return loadMetadataStore()[deckName] ?? null;
}

/** Remove metadata for a deleted deck. */
export function removeDeckMeta(deckName: string): void {
  const store = loadMetadataStore();
  delete store[deckName];
  saveMetadataStore(store);
}

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
