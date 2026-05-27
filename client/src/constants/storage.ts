import { isCommanderBracket, type CommanderBracket } from "../types/bracket";
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

/** Key for active draft-pod metadata in localStorage (synchronous resume detection) */
export const ACTIVE_DRAFT_POD_KEY = "phase-active-draft-pod";

/** Prefix for quick-draft session blobs in IndexedDB. Full key: `${QUICK_DRAFT_KEY_PREFIX}${draftId}` */
export const QUICK_DRAFT_KEY_PREFIX = "phase-quick-draft:";

/** Prefix for draft run state in IndexedDB. Full key: `${DRAFT_RUN_KEY_PREFIX}${draftId}` */
export const DRAFT_RUN_KEY_PREFIX = "phase-draft-run:";

/** localStorage key for the Zustand-persisted preferences store. */
export const PREFERENCES_KEY = "phase-preferences";

/**
 * Single authority for "is this localStorage key part of the user's portable
 * profile?" — the decks, preferences, metadata, active-deck pointer, and feed
 * state that `buildBackup`/`applyBackup` round-trip and that cloud sync mirrors.
 *
 * Deliberately excludes transient/rehydratable keys (per-game state, draft
 * blobs, IndexedDB caches): those regenerate at runtime and must NOT trigger a
 * cloud push. Consumed by `backup.ts` (export/import) and the cloud-sync
 * storage watcher so all three share one definition and cannot drift.
 */
export function isUserOwnedStorageKey(key: string): boolean {
  return (
    key === PREFERENCES_KEY ||
    key === DECK_METADATA_KEY ||
    key === ACTIVE_DECK_KEY ||
    key === FEED_SUBSCRIPTIONS_KEY ||
    key === FEED_DECK_ORIGINS_KEY ||
    key.startsWith(STORAGE_KEY_PREFIX)
  );
}

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

/**
 * Read a saved deck and return its repaired in-memory form.
 *
 * Pure read: never writes to localStorage. The repair-on-disk concern is
 * owned by the one-shot `migrateSavedDecks()` boot migration — doing the
 * write here used to fire during JSX render (`DeckTile` calls this), which
 * is a React-rule violation AND ping-pongs cloud sync between tabs. Repairs
 * still run on every read (cheap) so the in-memory shape is always
 * well-formed even if the migration hasn't run yet.
 */
export function loadSavedDeck(deckName: string): ParsedDeck | null {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as ParsedDeck & Record<string, unknown>;
    const repaired = repairParsedDeck(parsed);
    if (parsed.companion && !repaired.sideboard.some((e) => e.name === parsed.companion)) {
      repaired.sideboard.push({ count: 1, name: parsed.companion });
    }
    return repaired;
  } catch {
    return null;
  }
}

/**
 * Read the bracket sidecar field from a persisted saved-deck JSON. Bracket
 * is pre-game metadata stored alongside `format` — kept off the
 * engine-bound `ParsedDeck` so the engine boundary stays clean. Returns
 * `null` when the deck does not exist, has no bracket field, or carries
 * an invalid value.
 */
export function loadSavedDeckBracket(deckName: string): CommanderBracket | null {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as { bracket?: unknown };
    return isCommanderBracket(parsed.bracket) ? parsed.bracket : null;
  } catch {
    return null;
  }
}

/**
 * Write the bracket sidecar field on a persisted saved-deck JSON. Passing
 * `null` removes the field. Acts as a no-op when the deck does not exist;
 * the deck builder is responsible for the initial save before tagging.
 */
export function saveSavedDeckBracket(deckName: string, bracket: CommanderBracket | null): void {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return;
  try {
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    if (bracket === null) {
      delete parsed.bracket;
    } else {
      parsed.bracket = bracket;
    }
    localStorage.setItem(STORAGE_KEY_PREFIX + deckName, JSON.stringify(parsed));
  } catch {
    // Corrupt JSON: leave it alone. The deck builder will overwrite on save.
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
