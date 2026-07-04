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

/** Key for deck metadata (timestamps, source tracking, folder/star) */
export const DECK_METADATA_KEY = "phase-deck-metadata";

/** Key for the user's deck-folder registry (an array of {@link DeckFolder}). */
export const DECK_FOLDERS_KEY = "phase-deck-folders";

/** Window event fired when folder/star/membership state changes, so views
 * (library, builder switcher) can re-read without prop-drilling. Decks
 * themselves are tracked separately (callers re-list saved-deck keys). */
export const DECKS_CHANGED_EVENT = "phase-decks-changed";

/** Max length for a folder name; longer input is trimmed on create/rename. */
export const MAX_FOLDER_NAME_LENGTH = 40;

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
    key === DECK_FOLDERS_KEY ||
    key === ACTIVE_DECK_KEY ||
    key === FEED_SUBSCRIPTIONS_KEY ||
    key === FEED_DECK_ORIGINS_KEY ||
    key.startsWith(STORAGE_KEY_PREFIX)
  );
}

export interface DeckMeta {
  addedAt: number;
  lastPlayedAt?: number;
  /** Id of the folder this deck lives in. Absent ⇒ "Unfiled". */
  folderId?: string;
  /** Whether the deck is starred (pinned above folders in the library). */
  starred?: boolean;
}

/** A user-created folder for organizing saved decks. Folders are flat
 * (single-level); a deck belongs to at most one via {@link DeckMeta.folderId}. */
export interface DeckFolder {
  id: string;
  name: string;
  /** Manual position; folders render sorted by `order` then `name`. */
  order: number;
}

/**
 * Fire {@link DECKS_CHANGED_EVENT} so mounted views re-read folder state.
 * Contract: every mutator that changes folder membership or star state
 * (`setDeckFolder`, `toggleDeckStar`, `migrateDeckMeta`) MUST call this, and
 * every registry write goes through `saveFolderStore` which calls it — this is
 * what drives `useDeckFolders` to regroup. A new mutator that forgets it will
 * leave the library/switcher showing stale groupings.
 */
function notifyDecksChanged(): void {
  if (typeof window !== "undefined") {
    window.dispatchEvent(new Event(DECKS_CHANGED_EVENT));
  }
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
  // Spread the existing entry so folder/star membership survives a play.
  store[deckName] = {
    ...existing,
    addedAt: existing?.addedAt ?? Date.now(),
    lastPlayedAt: Date.now(),
  };
  saveMetadataStore(store);
}

/**
 * Move a deck's metadata from one name to another, preserving folder/star
 * membership and timestamps. Used by the in-place rename path (Save under a
 * new name), which would otherwise drop the deck's organization. No-op when
 * the source has no metadata — the caller's `stampDeckMeta` then seeds a
 * fresh entry under the new name.
 */
export function migrateDeckMeta(oldName: string, newName: string): void {
  if (oldName === newName) return;
  const store = loadMetadataStore();
  const src = store[oldName];
  if (!src) return;
  store[newName] = { ...src };
  delete store[oldName];
  saveMetadataStore(store);
  notifyDecksChanged();
}

/** Assign a deck to a folder, or pass `null` to move it to Unfiled. */
export function setDeckFolder(deckName: string, folderId: string | null): void {
  const store = loadMetadataStore();
  const meta = store[deckName] ?? { addedAt: Date.now() };
  if (folderId === null) delete meta.folderId;
  else meta.folderId = folderId;
  store[deckName] = meta;
  saveMetadataStore(store);
  notifyDecksChanged();
}

/** Toggle a deck's starred flag. Returns the resulting starred state. */
export function toggleDeckStar(deckName: string): boolean {
  const store = loadMetadataStore();
  const meta = store[deckName] ?? { addedAt: Date.now() };
  const starred = !meta.starred;
  if (starred) meta.starred = true;
  else delete meta.starred;
  store[deckName] = meta;
  saveMetadataStore(store);
  notifyDecksChanged();
  return starred;
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

// --- Folder registry helpers ---

function loadFolderStore(): DeckFolder[] {
  try {
    const raw = localStorage.getItem(DECK_FOLDERS_KEY);
    return raw ? (JSON.parse(raw) as DeckFolder[]) : [];
  } catch {
    return [];
  }
}

function saveFolderStore(folders: DeckFolder[]): void {
  localStorage.setItem(DECK_FOLDERS_KEY, JSON.stringify(folders));
  notifyDecksChanged();
}

/** List folders sorted by manual `order`, then name as a stable tiebreak. */
export function listFolders(): DeckFolder[] {
  return loadFolderStore()
    .slice()
    .sort((a, b) => a.order - b.order || a.name.localeCompare(b.name));
}

/**
 * Create a folder, appending it after the last by `order`. Returns the new
 * folder, or `null` when the name is blank. Duplicate names are permitted —
 * folders are identified by `id`, not name.
 */
export function createFolder(name: string): DeckFolder | null {
  const trimmed = name.trim().slice(0, MAX_FOLDER_NAME_LENGTH);
  if (!trimmed) return null;
  const folders = loadFolderStore();
  const order = folders.reduce((max, f) => Math.max(max, f.order), -1) + 1;
  const folder: DeckFolder = { id: crypto.randomUUID(), name: trimmed, order };
  folders.push(folder);
  saveFolderStore(folders);
  return folder;
}

/** Rename a folder in place. No-op when the id is unknown or name is blank. */
export function renameFolder(id: string, name: string): void {
  const trimmed = name.trim().slice(0, MAX_FOLDER_NAME_LENGTH);
  if (!trimmed) return;
  const folders = loadFolderStore();
  const folder = folders.find((f) => f.id === id);
  if (!folder) return;
  folder.name = trimmed;
  saveFolderStore(folders);
}

/**
 * Delete a folder. Member decks are reassigned to Unfiled (never deleted).
 * Metadata is updated first so a single notify carries a consistent state.
 */
export function deleteFolder(id: string): void {
  const folders = loadFolderStore();
  if (!folders.some((f) => f.id === id)) return;
  const store = loadMetadataStore();
  let changed = false;
  for (const meta of Object.values(store)) {
    if (meta.folderId === id) {
      delete meta.folderId;
      changed = true;
    }
  }
  if (changed) saveMetadataStore(store);
  saveFolderStore(folders.filter((f) => f.id !== id));
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
