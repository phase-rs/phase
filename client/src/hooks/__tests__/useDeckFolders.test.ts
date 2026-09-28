import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { groupSavedDecks, useDeckFolders } from "../useDeckFolders";
import { DECK_FOLDERS_KEY, STORAGE_KEY_PREFIX, type DeckFolder, type DeckMeta } from "../../constants/storage";
import { setSavedDeckTxnLockWaitForTests, withSavedDeckLibrary } from "../../services/savedDeckTransaction";
import { installFifoWebLocks, resetSavedDeckLibraryForTests, uninstallWebLocks } from "../../test/helpers/webLocks";
import { PROFILE_REPLACED_EVENT } from "../../stores/cloudSyncStore";

beforeEach(() => {
  localStorage.clear();
});

const folders: DeckFolder[] = [
  { id: "control", name: "Control", order: 0 },
  { id: "aggro", name: "Aggro", order: 1 },
];

/** Build a `metaOf` lookup from a plain name → meta record. */
function metaLookup(
  record: Record<string, Partial<DeckMeta>>,
): (name: string) => DeckMeta | null {
  return (name) => {
    const meta = record[name];
    return meta ? { addedAt: 0, ...meta } : null;
  };
}

describe("groupSavedDecks", () => {
  it("buckets decks into their folder, leaving the rest Unfiled", () => {
    const result = groupSavedDecks(
      ["Teferi", "Goblins", "Loose Deck"],
      metaLookup({
        Teferi: { folderId: "control" },
        Goblins: { folderId: "aggro" },
        // Loose Deck has no meta at all.
      }),
      folders,
    );
    expect(result.folders[0].decks).toEqual(["Teferi"]);
    expect(result.folders[1].decks).toEqual(["Goblins"]);
    expect(result.unfiled).toEqual(["Loose Deck"]);
    expect(result.starred).toEqual([]);
  });

  it("lifts starred decks out of their folder and pins them on top", () => {
    const result = groupSavedDecks(
      ["Teferi", "Goblins"],
      metaLookup({
        Teferi: { folderId: "control", starred: true },
        Goblins: { folderId: "aggro" },
      }),
      folders,
    );
    // Starred Teferi is pinned, NOT duplicated inside its folder.
    expect(result.starred).toEqual(["Teferi"]);
    expect(result.folders[0].decks).toEqual([]);
    expect(result.folders[1].decks).toEqual(["Goblins"]);
  });

  it("preserves input order within each section", () => {
    const result = groupSavedDecks(
      ["C", "A", "B"],
      metaLookup({
        C: { folderId: "control" },
        A: { folderId: "control" },
        B: { folderId: "control" },
      }),
      folders,
    );
    // Caller-provided order is kept verbatim — grouping never re-sorts decks.
    expect(result.folders[0].decks).toEqual(["C", "A", "B"]);
  });

  it("keeps empty folders so they remain visible move targets", () => {
    const result = groupSavedDecks([], metaLookup({}), folders);
    expect(result.folders.map((f) => f.folder.name)).toEqual(["Control", "Aggro"]);
    expect(result.folders.every((f) => f.decks.length === 0)).toBe(true);
  });

  it("treats a dangling folderId (deleted folder) as Unfiled", () => {
    const result = groupSavedDecks(
      ["Orphan"],
      metaLookup({ Orphan: { folderId: "deleted-folder" } }),
      folders,
    );
    expect(result.unfiled).toEqual(["Orphan"]);
  });

  it("places a bundled/starter deck (no folder) in Unfiled and is star-eligible", () => {
    // Bundled starter decks are real saved-deck keys with metadata, so they
    // participate in folders/stars like any user deck.
    const unstarred = groupSavedDecks(
      ["Starter: Mono-Red"],
      metaLookup({ "Starter: Mono-Red": {} }),
      folders,
    );
    expect(unstarred.unfiled).toEqual(["Starter: Mono-Red"]);

    const starred = groupSavedDecks(
      ["Starter: Mono-Red"],
      metaLookup({ "Starter: Mono-Red": { starred: true } }),
      folders,
    );
    expect(starred.starred).toEqual(["Starter: Mono-Red"]);
    expect(starred.unfiled).toEqual([]);
  });
});

describe("useDeckFolders (reactive)", () => {
  it("regroups after membership + star mutations made through the hook", async () => {
    // assignDeck/toggleStar now require the deck to exist (captured, unchanged) at the mutation.
    localStorage.setItem(STORAGE_KEY_PREFIX + "Burn", JSON.stringify({ main: [], sideboard: [] }));
    const { result } = renderHook(() => useDeckFolders());

    let folderId = "";
    await act(async () => {
      folderId = (await result.current.createFolder("Aggro"))!.id;
    });
    await act(async () => {
      await result.current.assignDeck("Burn", folderId);
    });

    // A membership change (metadata only — folder registry unchanged) must
    // still re-drive grouping; this is the load-bearing notify contract.
    const grouped = result.current.group(["Burn"]);
    expect(grouped.folders.find((f) => f.folder.id === folderId)?.decks).toEqual(["Burn"]);

    await act(async () => {
      await result.current.toggleStar("Burn");
    });
    const afterStar = result.current.group(["Burn"]);
    expect(afterStar.starred).toEqual(["Burn"]);
    expect(afterStar.folders.find((f) => f.folder.id === folderId)?.decks).toEqual([]);
  });

  it("refreshes folder state on a cross-tab storage event", () => {
    const { result } = renderHook(() => useDeckFolders());
    expect(result.current.folders).toHaveLength(0);

    // Simulate another tab writing the registry, then the native storage event.
    act(() => {
      localStorage.setItem(
        DECK_FOLDERS_KEY,
        JSON.stringify([{ id: "x", name: "External", order: 0 }]),
      );
      window.dispatchEvent(new Event("storage"));
    });

    expect(result.current.folders.map((f) => f.name)).toEqual(["External"]);
  });

  it("refreshes folder state when a cloud-sync snapshot replaces the profile", () => {
    const { result } = renderHook(() => useDeckFolders());
    expect(result.current.folders).toHaveLength(0);

    // A same-tab cloud-sync restore overwrites the registry then broadcasts
    // PROFILE_REPLACED_EVENT (not DECKS_CHANGED / storage); the hook must still
    // re-read so grouping doesn't desync from the freshly-synced decks.
    act(() => {
      localStorage.setItem(
        DECK_FOLDERS_KEY,
        JSON.stringify([{ id: "synced", name: "From Phone", order: 0 }]),
      );
      window.dispatchEvent(new CustomEvent(PROFILE_REPLACED_EVENT));
    });

    expect(result.current.folders.map((f) => f.name)).toEqual(["From Phone"]);
  });

  it("returns stable identities for createFolder/renameFolder/deleteFolder/assignDeck/toggleStar across rerenders", () => {
    const { result, rerender } = renderHook(() => useDeckFolders());
    const first = {
      createFolder: result.current.createFolder,
      renameFolder: result.current.renameFolder,
      deleteFolder: result.current.deleteFolder,
      assignDeck: result.current.assignDeck,
      toggleStar: result.current.toggleStar,
    };

    rerender();

    expect(result.current.createFolder).toBe(first.createFolder);
    expect(result.current.renameFolder).toBe(first.renameFolder);
    expect(result.current.deleteFolder).toBe(first.deleteFolder);
    expect(result.current.assignDeck).toBe(first.assignDeck);
    expect(result.current.toggleStar).toBe(first.toggleStar);
  });

  it("removes the window listeners on unmount", () => {
    const { result, unmount } = renderHook(() => useDeckFolders());
    unmount();
    // After unmount, an event must not throw or mutate the captured result.
    act(() => {
      window.dispatchEvent(new Event("phase-decks-changed"));
    });
    expect(result.current.folders).toHaveLength(0);
  });
});

describe("useDeckFolders: create waits behind the saved-deck library lock", () => {
  beforeEach(async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests();
  });

  afterEach(() => {
    uninstallWebLocks();
  });

  it("a folder create refused behind a held lock does not write the folder registry unguarded", async () => {
    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    setSavedDeckTxnLockWaitForTests(20);
    const { result } = renderHook(() => useDeckFolders());
    let created: DeckFolder | null = null;
    await act(async () => {
      created = await result.current.createFolder("Aggro");
    });

    expect(created).toBeNull();
    expect(localStorage.getItem(DECK_FOLDERS_KEY)).toBeNull();

    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
    releaseHolder();
    await holder;
  });
});
