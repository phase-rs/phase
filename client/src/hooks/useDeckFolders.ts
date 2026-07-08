import { useCallback, useEffect, useState } from "react";

import {
  DECKS_CHANGED_EVENT,
  createFolder as createFolderStore,
  deleteFolder as deleteFolderStore,
  getDeckMeta,
  listFolders,
  renameFolder as renameFolderStore,
  setDeckFolder,
  toggleDeckStar,
  type DeckFolder,
  type DeckMeta,
} from "../constants/storage";
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

export interface UseDeckFoldersResult {
  folders: DeckFolder[];
  /** Group a (pre-sorted) list of saved deck names into Starred/folders/Unfiled. */
  group: (deckNames: string[]) => GroupedDecks;
  createFolder: (name: string) => DeckFolder | null;
  renameFolder: (id: string, name: string) => void;
  deleteFolder: (id: string) => void;
  assignDeck: (deckName: string, folderId: string | null) => void;
  toggleStar: (deckName: string) => boolean;
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
    createFolder: createFolderStore,
    renameFolder: renameFolderStore,
    deleteFolder: deleteFolderStore,
    assignDeck: setDeckFolder,
    toggleStar: toggleDeckStar,
  };
}
