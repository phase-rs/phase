import { useCallback, useEffect, useState } from "react";

import {
  DECKS_CHANGED_EVENT,
  captureSavedDeck,
  createFolder as createFolderStore,
  deleteFolder as deleteFolderStore,
  getDeckMeta,
  listFolders,
  renameFolder as renameFolderStore,
  requireSavedDeckUnchanged,
  setDeckFolder,
  toggleDeckStar,
  type DeckFolder,
  type DeckMeta,
} from "../constants/storage";
import { attemptSavedDeckWrite } from "../services/savedDeckWriteFailure";
import { withSavedDeckLibrary } from "../services/savedDeckTransaction";
import { PROFILE_REPLACED_EVENT } from "../stores/cloudSyncStore";

export interface FolderGroup {
  folder: DeckFolder;
  decks: string[];
}

export interface GroupedDecks {
  /** Starred decks, lifted out of their folder and pinned above everything. */
  starred: string[];
  /** Every folder in display order — including empty ones, so they remain
   * visible as move targets. */
  folders: FolderGroup[];
  /** Decks belonging to no folder. */
  unfiled: string[];
}

/**
 * Pure grouping authority shared by the deck library and the builder's deck
 * switcher. `deckNames` is assumed to already be in the caller's desired sort
 * order; that order is preserved within each section. A deck appears in
 * exactly one place: Starred if starred, else its folder, else Unfiled. A
 * deck whose `folderId` no longer matches any folder falls through to Unfiled,
 * so a dangling reference is self-healing rather than hidden.
 */
export function groupSavedDecks(
  deckNames: string[],
  metaOf: (name: string) => DeckMeta | null,
  folders: DeckFolder[],
): GroupedDecks {
  const folderIds = new Set(folders.map((f) => f.id));
  const starred: string[] = [];
  const unfiled: string[] = [];
  const byFolder = new Map<string, string[]>();

  for (const name of deckNames) {
    const meta = metaOf(name);
    if (meta?.starred) {
      starred.push(name);
      continue;
    }
    const folderId = meta?.folderId;
    if (folderId && folderIds.has(folderId)) {
      const bucket = byFolder.get(folderId);
      if (bucket) bucket.push(name);
      else byFolder.set(folderId, [name]);
    } else {
      unfiled.push(name);
    }
  }

  return {
    starred,
    folders: folders.map((folder) => ({
      folder,
      decks: byFolder.get(folder.id) ?? [],
    })),
    unfiled,
  };
}

/** Module-level so the hook returns the same identity on every render (it closes over nothing). */
function createFolder(name: string, deckName?: string): Promise<DeckFolder | null> {
  const deck = deckName === undefined ? null : captureSavedDeck(deckName);
  return attemptSavedDeckWrite("organize", () =>
    withSavedDeckLibrary((txn) => {
      if (deck) requireSavedDeckUnchanged(txn, deck);
      const folder = createFolderStore(txn, name);
      // One transaction, so a move of this deck queued while it waits runs after it instead of being overwritten.
      if (folder && deck) setDeckFolder(txn, deck.name, folder.id);
      return folder;
    }),
  ).then((r) => (r.ok ? r.value : null));
}
/** Module-level so the hook returns the same identity on every render (it closes over nothing). */
function renameFolder(id: string, name: string): Promise<boolean> {
  return attemptSavedDeckWrite("organize", () => withSavedDeckLibrary((txn) => renameFolderStore(txn, id, name))).then(
    (r) => r.ok,
  );
}
/** Module-level so the hook returns the same identity on every render (it closes over nothing). */
function deleteFolder(id: string): Promise<boolean> {
  return attemptSavedDeckWrite("organize", () => withSavedDeckLibrary((txn) => deleteFolderStore(txn, id))).then(
    (r) => r.ok,
  );
}
/** Module-level so the hook returns the same identity on every render (it closes over nothing). */
function assignDeck(deckName: string, folderId: string | null): Promise<boolean> {
  const deck = captureSavedDeck(deckName);
  return attemptSavedDeckWrite("organize", () =>
    withSavedDeckLibrary((txn) => {
      requireSavedDeckUnchanged(txn, deck);
      setDeckFolder(txn, deck.name, folderId);
    }),
  ).then((r) => r.ok);
}
/** Module-level so the hook returns the same identity on every render (it closes over nothing). */
function toggleStar(deckName: string): Promise<boolean> {
  const deck = captureSavedDeck(deckName);
  return attemptSavedDeckWrite("organize", () =>
    withSavedDeckLibrary((txn) => {
      requireSavedDeckUnchanged(txn, deck);
      return toggleDeckStar(txn, deck.name);
    }),
  ).then((r) => r.ok);
}

export interface UseDeckFoldersResult {
  folders: DeckFolder[];
  /** Group a (pre-sorted) list of saved deck names into Starred/folders/Unfiled. */
  group: (deckNames: string[]) => GroupedDecks;
  createFolder: (name: string, deckName?: string) => Promise<DeckFolder | null>;
  renameFolder: (id: string, name: string) => Promise<boolean>;
  deleteFolder: (id: string) => Promise<boolean>;
  assignDeck: (deckName: string, folderId: string | null) => Promise<boolean>;
  toggleStar: (deckName: string) => Promise<boolean>;
}

/**
 * Reactive view over the folder registry + deck metadata. Re-reads on
 * {@link DECKS_CHANGED_EVENT} (our own writes), the native `storage` event
 * (writes from another tab), and {@link PROFILE_REPLACED_EVENT} (a cloud-sync
 * snapshot overwriting the folder registry in this same tab). Each refresh
 * produces a fresh `folders` array, so `group()` re-memoizes and consumers
 * recompute with current metadata.
 */
export function useDeckFolders(): UseDeckFoldersResult {
  const [folders, setFolders] = useState<DeckFolder[]>(listFolders);

  useEffect(() => {
    const refresh = () => setFolders(listFolders());
    window.addEventListener(DECKS_CHANGED_EVENT, refresh);
    window.addEventListener("storage", refresh);
    window.addEventListener(PROFILE_REPLACED_EVENT, refresh);
    return () => {
      window.removeEventListener(DECKS_CHANGED_EVENT, refresh);
      window.removeEventListener("storage", refresh);
      window.removeEventListener(PROFILE_REPLACED_EVENT, refresh);
    };
  }, []);

  const group = useCallback(
    (deckNames: string[]) => groupSavedDecks(deckNames, getDeckMeta, folders),
    [folders],
  );

  return {
    folders,
    group,
    createFolder,
    renameFolder,
    deleteFolder,
    assignDeck,
    toggleStar,
  };
}
