import { DRAFT_KINDS } from "../adapter/draftKinds";
import { isCommanderBracket, type CommanderBracket } from "../types/bracket";
import type { FeedSubscription } from "../types/feed";
import { repairParsedDeck, type ParsedDeck } from "../services/deckParser";
import { projectSavedDeckSpecialSlots } from "../services/savedDeckProjection";
import {
  savedDeckTxnGate,
  withSavedDeckLibrary,
  withSavedDeckLibraryOrSkip,
  SavedDeckChangedError,
  type SavedDeckTxn,
  type SavedDeckTxnResult,
} from "../services/savedDeckTransaction";

/** Every draft-autosave slot: one per draft kind, plus solo cube drafts (which run as kind `Quick`). */
export const DRAFT_AUTOSAVE_SLOTS = [...DRAFT_KINDS, "Cube"] as const;
export type DraftAutosaveSlot = (typeof DRAFT_AUTOSAVE_SLOTS)[number];
export function isDraftAutosaveSlot(value: unknown): value is DraftAutosaveSlot {
  return DRAFT_AUTOSAVE_SLOTS.some((slot) => slot === value);
}

/** Prefix for saved deck data in localStorage. Full key: `${STORAGE_KEY_PREFIX}${deckName}` */
export const STORAGE_KEY_PREFIX = "phase-deck:";

/** Key for the currently selected/active deck name in localStorage */
export const ACTIVE_DECK_KEY = "phase-active-deck";

/** Sentinel stored as the active deck when setup should randomize at game start. */
export const RANDOM_DECK_SELECTION = "__phase_random_deck__";

export function isRandomDeckSelection(deckName: string | null | undefined): deckName is typeof RANDOM_DECK_SELECTION {
  return deckName === RANDOM_DECK_SELECTION;
}

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

/**
 * Non-secret pointer to the most recent guest draft. The reconnect capability
 * itself remains in IndexedDB; this record exists only so a reloaded guest can
 * find the pod again from its room code.
 */
export const ACTIVE_DRAFT_GUEST_KEY = "phase-active-draft-guest";

/** Prefix for quick-draft session blobs in IndexedDB. Full key: `${QUICK_DRAFT_KEY_PREFIX}${draftId}` */
export const QUICK_DRAFT_KEY_PREFIX = "phase-quick-draft:";

/** Prefix for draft run state in IndexedDB. Full key: `${DRAFT_RUN_KEY_PREFIX}${draftId}` */
export const DRAFT_RUN_KEY_PREFIX = "phase-draft-run:";

/** localStorage key for the Zustand-persisted preferences store. */
export const PREFERENCES_KEY = "phase-preferences";

/**
 * localStorage key for configured LLM opponent endpoints.
 *
 * Deliberately NOT part of {@link isUserOwnedStorageKey}: these records hold
 * provider API keys, and the backup export and cloud-sync mirror are both
 * off-device destinations a player has not consented to send a credential to.
 * An LLM opponent is re-configured per device, on purpose.
 */
export const LLM_ENDPOINTS_KEY = "phase-llm-endpoints";

/** localStorage key for personal draft workspace preferences. */
export const DRAFT_WORKSPACE_PREFERENCES_KEY = "phase-draft-workspace-preferences";

/**
 * localStorage key for a per-device counter bumped by `bumpProfileReplacementGeneration`,
 * called only from `backup.ts::applyBackup` — reached via cloud sync's apply-remote
 * and apply-merged paths (`cloudSyncStore.ts`), manual backup-file import
 * (`backup.ts::importBackupFromFile`), and the legacy-storage migration
 * (`legacyMigration.ts`). `feedService.ts` captures this before waiting on
 * the saved-deck library lock and compares after re-acquiring it, so a feed
 * sync queued behind one of these replacements can detect it and skip.
 *
 * Deliberately NOT part of {@link isUserOwnedStorageKey}: syncing this counter
 * would let a remote profile apply overwrite the very value used to detect a
 * profile replacement, defeating the guard it exists to provide.
 */
export const PROFILE_REPLACEMENT_KEY = "phase-profile-replacement";

/** Current profile-replacement generation (see {@link PROFILE_REPLACEMENT_KEY}). */
export function profileReplacementGeneration(): number {
  return Number(localStorage.getItem(PROFILE_REPLACEMENT_KEY) ?? 0);
}

/**
 * Bump the profile-replacement generation. Requires a {@link SavedDeckTxn} so it
 * can only run inside a saved-deck library transaction body, alongside the
 * profile replacement it accompanies.
 */
export function bumpProfileReplacementGeneration(txn: SavedDeckTxn): void {
  void txn;
  localStorage.setItem(PROFILE_REPLACEMENT_KEY, String(profileReplacementGeneration() + 1));
}

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
    key === DRAFT_WORKSPACE_PREFERENCES_KEY ||
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
  /** The draft-autosave slot that owns this deck. */
  autosaveSlot?: DraftAutosaveSlot;
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

function saveMetadataStore(txn: SavedDeckTxn, store: Record<string, DeckMeta>): void {
  void txn;
  localStorage.setItem(DECK_METADATA_KEY, JSON.stringify(store));
}

/** Remove draft-autosave ownership: the autosave will no longer overwrite or rename this deck. */
export function clearDeckAutosaveMarker(txn: SavedDeckTxn, deckName: string): void {
  const store = loadMetadataStore();
  if (store[deckName]?.autosaveSlot === undefined) return;
  delete store[deckName].autosaveSlot;
  saveMetadataStore(txn, store);
}

/** Stamp metadata for a deck saved or seeded by anything other than the draft autosave, and clear any autosave ownership. */
export function stampDeckMeta(txn: SavedDeckTxn, deckName: string, addedAt?: number): void {
  const store = loadMetadataStore();
  if (!store[deckName]) {
    store[deckName] = { addedAt: addedAt ?? Date.now() };
    saveMetadataStore(txn, store);
  }
  clearDeckAutosaveMarker(txn, deckName);
}

/** Update the lastPlayedAt timestamp for a deck. Call when starting a game. */
export function touchDeckPlayed(txn: SavedDeckTxn, deckName: string): void {
  if (isRandomDeckSelection(deckName)) return;
  const store = loadMetadataStore();
  const existing = store[deckName];
  // Spread the existing entry so folder/star membership survives a play.
  store[deckName] = {
    ...existing,
    addedAt: existing?.addedAt ?? Date.now(),
    lastPlayedAt: Date.now(),
  };
  saveMetadataStore(txn, store);
}

/**
 * Move a deck's metadata from one name to another, preserving folder/star
 * membership and timestamps. Used by the in-place rename path (Save under a
 * new name), which would otherwise drop the deck's organization. No-op when
 * the source has no metadata — the caller's `stampDeckMeta` then seeds a
 * fresh entry under the new name.
 */
export function migrateDeckMeta(txn: SavedDeckTxn, oldName: string, newName: string): void {
  if (oldName === newName) return;
  const store = loadMetadataStore();
  const src = store[oldName];
  if (!src) return;
  store[newName] = { ...src };
  delete store[oldName];
  saveMetadataStore(txn, store);
  notifyDecksChanged();
}

/** The first of `baseName`, `candidate(2)`, `candidate(3)`, … not in `takenNames`. */
export function uniqueDeckName(
  baseName: string,
  takenNames: Iterable<string>,
  candidate: (index: number) => string = (index) => `${baseName} ${index}`,
): string {
  const taken = new Set(takenNames);
  if (!taken.has(baseName)) return baseName;
  for (let i = 2; ; i++) {
    const next = candidate(i);
    if (!taken.has(next)) return next;
  }
}

/** `uniqueDeckName` against the library as the transaction that will write the name sees it. */
export function freeDeckName(
  txn: SavedDeckTxn,
  baseName: string,
  candidate?: (index: number) => string,
): string {
  void txn;
  return uniqueDeckName(baseName, listSavedDeckNames(), candidate);
}

/** A saved deck's stored data, read when the user chose an action on it. */
export interface SavedDeckSnapshot {
  readonly name: string;
  /** `null` when no deck was saved under `name`. */
  readonly raw: string | null;
}

/** Read the deck saved under `deckName` before waiting for the library lock; pass the result into the transaction. */
export function captureSavedDeck(deckName: string): SavedDeckSnapshot {
  return { name: deckName, raw: localStorage.getItem(STORAGE_KEY_PREFIX + deckName) };
}

/** Whether `deck.name` still holds the deck `deck` captured. Metadata is not compared, so organizing or playing a deck does not make it a different deck. */
export function savedDeckUnchanged(txn: SavedDeckTxn, deck: SavedDeckSnapshot): boolean {
  void txn;
  return deck.raw !== null && localStorage.getItem(STORAGE_KEY_PREFIX + deck.name) === deck.raw;
}

/** Throw `SavedDeckChangedError`, before anything is written, unless `savedDeckUnchanged`. */
export function requireSavedDeckUnchanged(txn: SavedDeckTxn, deck: SavedDeckSnapshot): void {
  if (!savedDeckUnchanged(txn, deck)) throw new SavedDeckChangedError(deck.name);
}

/** Write a saved deck's data, returning the snapshot of what was written. */
export function writeSavedDeckData(txn: SavedDeckTxn, deckName: string, raw: string): SavedDeckSnapshot {
  void txn;
  localStorage.setItem(STORAGE_KEY_PREFIX + deckName, raw);
  return { name: deckName, raw };
}

/** Remove a saved deck's data. */
export function removeSavedDeckData(txn: SavedDeckTxn, deckName: string): void {
  void txn;
  localStorage.removeItem(STORAGE_KEY_PREFIX + deckName);
}

/** Move a saved deck from `oldName` to `newName`: removes `oldName`'s data, carries its metadata
 *  (`migrateDeckMeta`), and repoints the active deck. The caller writes `newName`'s data. */
export function moveSavedDeck(txn: SavedDeckTxn, oldName: string, newName: string): void {
  if (oldName === newName) return;
  removeSavedDeckData(txn, oldName);
  migrateDeckMeta(txn, oldName, newName);
  if (localStorage.getItem(ACTIVE_DECK_KEY) === oldName) {
    localStorage.setItem(ACTIVE_DECK_KEY, newName);
  }
}

/** Assign a deck to a folder, or pass `null` to move it to Unfiled. */
export function setDeckFolder(txn: SavedDeckTxn, deckName: string, folderId: string | null): void {
  const store = loadMetadataStore();
  const meta = store[deckName] ?? { addedAt: Date.now() };
  if (folderId === null) delete meta.folderId;
  else meta.folderId = folderId;
  store[deckName] = meta;
  saveMetadataStore(txn, store);
  notifyDecksChanged();
}

/** Toggle a deck's starred flag. Returns the resulting starred state. */
export function toggleDeckStar(txn: SavedDeckTxn, deckName: string): boolean {
  const store = loadMetadataStore();
  const meta = store[deckName] ?? { addedAt: Date.now() };
  const starred = !meta.starred;
  if (starred) meta.starred = true;
  else delete meta.starred;
  store[deckName] = meta;
  saveMetadataStore(txn, store);
  notifyDecksChanged();
  return starred;
}

/** Get metadata for a single deck, or null if not tracked. */
export function getDeckMeta(deckName: string): DeckMeta | null {
  return loadMetadataStore()[deckName] ?? null;
}

/** Remove metadata for a deleted deck. */
export function removeDeckMeta(txn: SavedDeckTxn, deckName: string): void {
  const store = loadMetadataStore();
  delete store[deckName];
  saveMetadataStore(txn, store);
}

/** Delete the saved deck `deck` captured, clearing its metadata and the active-deck pointer if it names it.
 *  Throws SavedDeckChangedError, writing nothing, if the name no longer holds that deck. */
export function deleteDeck(txn: SavedDeckTxn, deck: SavedDeckSnapshot): void {
  requireSavedDeckUnchanged(txn, deck);
  removeSavedDeckData(txn, deck.name);
  removeDeckMeta(txn, deck.name);
  if (localStorage.getItem(ACTIVE_DECK_KEY) === deck.name) {
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

/** Write a draft autosave into `slot` as a saved-deck library transaction, and return the deck name used or why it was skipped.
 *  Overwrites only the deck carrying `slot`, moving it to the first free name among `label`, `label (2)`, …; never overwrites another deck. */
export function writeDraftAutosaveDeck(
  slot: DraftAutosaveSlot,
  label: string,
  data: string,
): Promise<SavedDeckTxnResult<string>> {
  return withSavedDeckLibraryOrSkip(async (txn) => {
    const store = loadMetadataStore();
    const owners = Object.entries(store)
      .filter(([name, meta]) => meta.autosaveSlot === slot && localStorage.getItem(STORAGE_KEY_PREFIX + name) !== null)
      .map(([name]) => name)
      .sort();
    const current = owners.includes(label) ? label : owners[0];
    const name = uniqueDeckName(
      label,
      listSavedDeckNames().filter((n) => n !== current),
      (i) => `${label} (${i})`,
    );

    const pause = savedDeckTxnGate("draft-autosave-after-owner-selection");
    if (pause) await pause;

    // Write the data first: if this throws (e.g. quota), nothing else has changed.
    writeSavedDeckData(txn, name, data);

    if (current !== undefined && current !== name) {
      moveSavedDeck(txn, current, name);
    }

    const nextStore = loadMetadataStore();
    // An orphaned marker at `name` (no deck key, so not an owner) must not carry its stale metadata forward
    // when there is no owner to move: this write starts a fresh entry, not a continuation of that orphan.
    nextStore[name] = current === undefined
      ? { addedAt: Date.now(), autosaveSlot: slot }
      : { ...nextStore[name], addedAt: nextStore[name]?.addedAt ?? Date.now(), autosaveSlot: slot };
    for (const other of owners) {
      if (other === current) continue;
      delete nextStore[other]?.autosaveSlot;
    }
    saveMetadataStore(txn, nextStore);

    return name;
  }, "skip");
}

/**
 * Save the deck builder's deck as `nextName`. When renamed, move it from `previous.name` only if
 * that name still holds the deck `previous` captured; otherwise leave that name's deck alone. The
 * check reads `savedDeckRef` under the lock, not just `previous`: when an earlier queued save or
 * clone to that same name has already committed by the time this transaction runs, its write is
 * what `previous` should be compared against, not the value captured back at this click.
 *
 * On success, `savedDeckRef` is updated to this write's snapshot only if `claimsEditor` (checked
 * again after the write) still says so — a Load that switched the editor to a different deck
 * while this save waited owns the ref, not this save's now-stale claim to it.
 */
export function saveBuilderDeck(
  previous: SavedDeckSnapshot | null,
  savedDeckRef: { current: SavedDeckSnapshot | null },
  claimsEditor: () => boolean,
  nextName: string,
  data: string,
): Promise<SavedDeckSnapshot> {
  return withSavedDeckLibrary(async (txn) => {
    const live = savedDeckRef.current;
    const effective = claimsEditor() && previous && live && live.name === previous.name ? live : previous;
    if (effective && effective.name !== nextName && savedDeckUnchanged(txn, effective)) {
      // If nextName already names another deck, the writeSavedDeckData below overwrites
      // its data (pre-existing Save behavior) and moveSavedDeck's metadata
      // carry likewise replaces its metadata — both correctly reflect the
      // surviving deck's identity now living under nextName.
      moveSavedDeck(txn, effective.name, nextName);
    }
    const saved = writeSavedDeckData(txn, nextName, data);
    const pause = savedDeckTxnGate("builder-save-after-data-write");
    if (pause) await pause;
    stampDeckMeta(txn, nextName);
    if (claimsEditor()) savedDeckRef.current = saved;
    return saved;
  });
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

function saveFolderStore(txn: SavedDeckTxn, folders: DeckFolder[]): void {
  void txn;
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
export function createFolder(txn: SavedDeckTxn, name: string): DeckFolder | null {
  const trimmed = name.trim().slice(0, MAX_FOLDER_NAME_LENGTH);
  if (!trimmed) return null;
  const folders = loadFolderStore();
  const order = folders.reduce((max, f) => Math.max(max, f.order), -1) + 1;
  const folder: DeckFolder = { id: crypto.randomUUID(), name: trimmed, order };
  folders.push(folder);
  saveFolderStore(txn, folders);
  return folder;
}

/** Rename a folder in place. No-op when the id is unknown or name is blank. */
export function renameFolder(txn: SavedDeckTxn, id: string, name: string): void {
  const trimmed = name.trim().slice(0, MAX_FOLDER_NAME_LENGTH);
  if (!trimmed) return;
  const folders = loadFolderStore();
  const folder = folders.find((f) => f.id === id);
  if (!folder) return;
  folder.name = trimmed;
  saveFolderStore(txn, folders);
}

/**
 * Delete a folder. Member decks are reassigned to Unfiled (never deleted).
 * Metadata is updated first so a single notify carries a consistent state.
 */
export function deleteFolder(txn: SavedDeckTxn, id: string): void {
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
  if (changed) saveMetadataStore(txn, store);
  saveFolderStore(txn, folders.filter((f) => f.id !== id));
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
  if (isRandomDeckSelection(deckName)) return null;
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as ParsedDeck & Record<string, unknown>;
    return projectSavedDeckSpecialSlots(parsed, repairParsedDeck(parsed));
  } catch {
    return null;
  }
}

/** Read the persisted deck-construction format without projecting deck data. */
export function loadSavedDeckFormat(deckName: string): string | undefined {
  if (isRandomDeckSelection(deckName)) return undefined;
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return undefined;
  try {
    const parsed = JSON.parse(raw) as { format?: unknown };
    return typeof parsed.format === "string" ? parsed.format : undefined;
  } catch {
    return undefined;
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
  if (isRandomDeckSelection(deckName)) return null;
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
export function saveSavedDeckBracket(txn: SavedDeckTxn, deckName: string, bracket: CommanderBracket | null): void {
  void txn;
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
