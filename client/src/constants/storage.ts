import type { FeedSubscription } from "../types/feed";
import { repairParsedDeck, type ParsedDeck } from "../services/deckParser";

/** Prefix for saved deck data in localStorage. Full key: `${STORAGE_KEY_PREFIX}${deckName}` */
export const STORAGE_KEY_PREFIX = "phase-deck:";

/** Key for the currently selected/active deck name in localStorage */
export const ACTIVE_DECK_KEY = "phase-active-deck";

/** Prefix for per-game saved state. Full key: `${GAME_KEY_PREFIX}${gameId}` */
export const GAME_KEY_PREFIX = "phase-game:";

/** Prefix for per-game debug checkpoints. Full key: `${GAME_CHECKPOINTS_PREFIX}${gameId}` */
export const GAME_CHECKPOINTS_PREFIX = "phase-game-checkpoints:";

/** Key for the active game metadata (id, mode, difficulty) */
export const ACTIVE_GAME_KEY = "phase-active-game";

/** Key for deck metadata (timestamps, source tracking) */
export const DECK_METADATA_KEY = "phase-deck-metadata";

/** Key for the list of subscribed feeds */
export const FEED_SUBSCRIPTIONS_KEY = "phase-feed-subscriptions";

/** Key for mapping deck names to their originating feed ID */
export const FEED_DECK_ORIGINS_KEY = "phase-feed-deck-origins";

/** Flag to short-circuit async feed init on subsequent loads */
export const FEEDS_INITIALIZED_KEY = "phase-feeds-initialized";

/** Key for active quick-draft metadata in localStorage (synchronous resume detection) */
export const ACTIVE_QUICK_DRAFT_KEY = "phase-active-quick-draft";

/** Prefix for quick-draft session blobs in IndexedDB. Full key: `${QUICK_DRAFT_KEY_PREFIX}${draftId}` */
export const QUICK_DRAFT_KEY_PREFIX = "phase-quick-draft:";

export interface DeckMeta {
  addedAt: number;
  lastPlayedAt?: number;
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

/** Update the lastPlayedAt timestamp for a deck. Call when starting a game. */
export function touchDeckPlayed(deckName: string): void {
  const store = loadMetadataStore();
  const existing = store[deckName];
  store[deckName] = { addedAt: existing?.addedAt ?? Date.now(), lastPlayedAt: Date.now() };
  saveMetadataStore(store);
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

/** Delete a saved deck from localStorage, clearing metadata and active-deck if needed. */
export function deleteDeck(deckName: string): void {
  localStorage.removeItem(STORAGE_KEY_PREFIX + deckName);
  removeDeckMeta(deckName);
  if (localStorage.getItem(ACTIVE_DECK_KEY) === deckName) {
    localStorage.removeItem(ACTIVE_DECK_KEY);
  }
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

export function loadSavedDeck(deckName: string): ParsedDeck | null {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as ParsedDeck & Record<string, unknown>;
    const repaired = repairParsedDeck(parsed);
    if (parsed.companion && !repaired.sideboard.some((e) => e.name === parsed.companion)) {
      repaired.sideboard.push({ count: 1, name: parsed.companion });
    }

    const repairedPayload = {
      ...parsed,
      ...repaired,
    };
    const repairedRaw = JSON.stringify(repairedPayload);
    if (repairedRaw !== raw) {
      localStorage.setItem(STORAGE_KEY_PREFIX + deckName, repairedRaw);
    }
    return repaired;
  } catch {
    return null;
  }
}

/** Load the currently active deck from localStorage. */
export function loadActiveDeck(): ParsedDeck | null {
  const activeName = localStorage.getItem(ACTIVE_DECK_KEY);
  if (!activeName) return null;
  return loadSavedDeck(activeName);
}

// --- Feed storage helpers ---

export function loadFeedSubscriptions(): FeedSubscription[] {
  try {
    const raw = localStorage.getItem(FEED_SUBSCRIPTIONS_KEY);
    return raw ? (JSON.parse(raw) as FeedSubscription[]) : [];
  } catch {
    return [];
  }
}

export function saveFeedSubscriptions(subs: FeedSubscription[]): void {
  localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify(subs));
}

export function loadDeckOrigins(): Record<string, string> {
  try {
    const raw = localStorage.getItem(FEED_DECK_ORIGINS_KEY);
    return raw ? (JSON.parse(raw) as Record<string, string>) : {};
  } catch {
    return {};
  }
}

export function saveDeckOrigins(origins: Record<string, string>): void {
  localStorage.setItem(FEED_DECK_ORIGINS_KEY, JSON.stringify(origins));
}
