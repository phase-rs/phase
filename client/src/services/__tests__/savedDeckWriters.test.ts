import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";

import type { DeckEntry } from "../../hooks/useDecks";
import { savePreconDeck } from "../preconDecks";
import { adoptFeedDeck, unsubscribe } from "../feedService";
import { importBackupFromFile, type PhaseBackupV1 } from "../backup";
import {
  getDeckMeta,
  listFolders,
  loadDeckOrigins,
  removeDeckMeta,
  saveBuilderDeck,
  stampDeckMeta,
  writeSavedDeckData,
  type DeckFolder,
} from "../../constants/storage";
import { useDeckFolders } from "../../hooks/useDeckFolders";
import { SavedDeckChangedError, setSavedDeckTxnLockWaitForTests, withSavedDeckLibrary } from "../savedDeckTransaction";
import { useAppNotificationStore } from "../../stores/appToastStore";
import {
  installFifoWebLocks,
  resetSavedDeckLibraryForTests,
  uninstallWebLocks,
} from "../../test/helpers/webLocks";
import { STORAGE_KEY_PREFIX, DECK_METADATA_KEY, FEED_DECK_ORIGINS_KEY } from "../../constants/storage";

/**
 * Each row holds the saved-deck library lock with a deferred body, invokes the writer, waits
 * for the writer's own request to reach the queue (the reach guard proving it took the lock),
 * asserts storage is unchanged while it waits, then releases and asserts the write landed.
 */

beforeEach(async () => {
  localStorage.clear();
  installFifoWebLocks();
  await resetSavedDeckLibraryForTests();
});

afterEach(() => {
  uninstallWebLocks();
});

async function heldLock(): Promise<{ release: () => void; holder: Promise<void> }> {
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const holder = withSavedDeckLibrary(() => held);
  await vi.waitFor(async () => {
    expect((await navigator.locks.query()).held).toHaveLength(1);
  });
  return { release, holder };
}

async function waitPendingThenRelease(release: () => void, holder: Promise<void>): Promise<void> {
  await vi.waitFor(async () => {
    expect((await navigator.locks.query()).pending).toHaveLength(1);
  });
  release();
  await holder;
  await vi.waitFor(async () => {
    expect((await navigator.locks.query()).held).toHaveLength(0);
    expect((await navigator.locks.query()).pending).toHaveLength(0);
  });
}

describe("savedDeckWriters: each takes the saved-deck library lock", () => {
  it("savePreconDeck waits for the lock and then writes", async () => {
    const { release, holder } = await heldLock();
    const precon: DeckEntry = {
      name: "Precon",
      code: "SET",
      type: "Commander Deck",
      coveragePct: 100,
      mainBoard: [{ name: "Forest", count: 40 }],
      sideBoard: [],
      commander: undefined,
    };
    const call = savePreconDeck("Precon Deck", precon, { type: "replace" });
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Precon Deck")).toBeNull();
    await waitPendingThenRelease(release, holder);
    await call;
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Precon Deck")).not.toBeNull();
  });

  it("adoptFeedDeck waits for the lock and then writes", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Feed Deck",
      JSON.stringify({ main: [{ name: "Bear", count: 1 }], sideboard: [] }),
    );
    const { release, holder } = await heldLock();
    const call = adoptFeedDeck("Feed Deck", "Adopted Deck");
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Adopted Deck")).toBeNull();
    await waitPendingThenRelease(release, holder);
    await call;
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Adopted Deck")).not.toBeNull();
  });

  it("unsubscribe waits for the lock and then removes feed-owned decks", async () => {
    localStorage.setItem(
      FEED_DECK_ORIGINS_KEY,
      JSON.stringify({ "Feed Deck": "feed-1" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Feed Deck",
      JSON.stringify({ main: [], sideboard: [] }),
    );
    const { release, holder } = await heldLock();
    const call = unsubscribe("feed-1");
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Feed Deck")).not.toBeNull();
    await waitPendingThenRelease(release, holder);
    await call;
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Feed Deck")).toBeNull();
  });

  it("importBackupFromFile waits for the lock and then applies the backup", async () => {
    const backup: PhaseBackupV1 = {
      version: 1,
      exportedAt: new Date(0).toISOString(),
      preferences: null,
      decks: { "Imported Deck": JSON.stringify({ main: [], sideboard: [] }) },
      deckMetadata: null,
      activeDeck: null,
      feedSubscriptions: null,
      feedDeckOrigins: null,
    };
    const file = new File([JSON.stringify(backup)], "phase-backup.json", { type: "application/json" });
    const { release, holder } = await heldLock();
    const call = importBackupFromFile(file, "merge");
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Imported Deck")).toBeNull();
    await waitPendingThenRelease(release, holder);
    const result = await call;
    expect(result.decksImported).toBe(1);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Imported Deck")).not.toBeNull();
  });

  it("saveBuilderDeck waits for the lock and then writes", async () => {
    const { release, holder } = await heldLock();
    const call = saveBuilderDeck(null, { current: null }, () => true, "Built Deck", JSON.stringify({ main: [], sideboard: [] }));
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Built Deck")).toBeNull();
    await waitPendingThenRelease(release, holder);
    await call;
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Built Deck")).not.toBeNull();
  });

  it("useDeckFolders().toggleStar waits for the lock and then flips the star", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Starrable Deck",
      JSON.stringify({ main: [], sideboard: [] }),
    );
    const { result } = renderHook(() => useDeckFolders());
    const { release, holder } = await heldLock();
    let call!: Promise<boolean>;
    act(() => {
      call = result.current.toggleStar("Starrable Deck");
    });
    expect(JSON.parse(localStorage.getItem(DECK_METADATA_KEY) ?? "{}")["Starrable Deck"]?.starred).toBeFalsy();
    await waitPendingThenRelease(release, holder);
    await act(async () => {
      await call;
    });
    expect(JSON.parse(localStorage.getItem(DECK_METADATA_KEY) ?? "{}")["Starrable Deck"]?.starred).toBe(true);
  });
});

describe("refused on a lock-wait timeout", () => {
  beforeEach(() => {
    setSavedDeckTxnLockWaitForTests(20);
  });
  afterEach(() => {
    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
  });

  it("savePreconDeck rejects with lock-timeout and writes nothing", async () => {
    const { release, holder } = await heldLock();
    const precon: DeckEntry = {
      name: "Precon", code: "SET", type: "Commander Deck", coveragePct: 100,
      mainBoard: [{ name: "Forest", count: 40 }], sideBoard: [], commander: undefined,
    };
    const before = localStorage.getItem(STORAGE_KEY_PREFIX + "Precon Deck");
    const call = savePreconDeck("Precon Deck", precon, { type: "replace" });
    await expect(call).rejects.toMatchObject({ reason: "lock-timeout" });
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Precon Deck")).toBe(before);
    release();
    await holder;
  });

  it("adoptFeedDeck rejects with lock-timeout and writes nothing", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Feed Deck",
      JSON.stringify({ main: [{ name: "Bear", count: 1 }], sideboard: [] }),
    );
    const { release, holder } = await heldLock();
    const before = localStorage.getItem(STORAGE_KEY_PREFIX + "Adopted Deck");
    const call = adoptFeedDeck("Feed Deck", "Adopted Deck");
    await expect(call).rejects.toMatchObject({ reason: "lock-timeout" });
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Adopted Deck")).toBe(before);
    release();
    await holder;
  });

  it("unsubscribe rejects with lock-timeout and writes nothing", async () => {
    localStorage.setItem(FEED_DECK_ORIGINS_KEY, JSON.stringify({ "Feed Deck": "feed-1" }));
    localStorage.setItem(STORAGE_KEY_PREFIX + "Feed Deck", JSON.stringify({ main: [], sideboard: [] }));
    const { release, holder } = await heldLock();
    const before = localStorage.getItem(STORAGE_KEY_PREFIX + "Feed Deck");
    const call = unsubscribe("feed-1");
    await expect(call).rejects.toMatchObject({ reason: "lock-timeout" });
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Feed Deck")).toBe(before);
    release();
    await holder;
  });

  it("importBackupFromFile rejects with lock-timeout and writes nothing", async () => {
    const backup: PhaseBackupV1 = {
      version: 1,
      exportedAt: new Date(0).toISOString(),
      preferences: null,
      decks: { "Imported Deck": JSON.stringify({ main: [], sideboard: [] }) },
      deckMetadata: null,
      activeDeck: null,
      feedSubscriptions: null,
      feedDeckOrigins: null,
    };
    const file = new File([JSON.stringify(backup)], "phase-backup.json", { type: "application/json" });
    const { release, holder } = await heldLock();
    const before = localStorage.getItem(STORAGE_KEY_PREFIX + "Imported Deck");
    const call = importBackupFromFile(file, "merge");
    await expect(call).rejects.toMatchObject({ reason: "lock-timeout" });
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Imported Deck")).toBe(before);
    release();
    await holder;
  });

  it("saveBuilderDeck rejects with lock-timeout and writes nothing", async () => {
    const { release, holder } = await heldLock();
    const before = localStorage.getItem(STORAGE_KEY_PREFIX + "Built Deck");
    const call = saveBuilderDeck(null, { current: null }, () => true, "Built Deck", JSON.stringify({ main: [], sideboard: [] }));
    await expect(call).rejects.toMatchObject({ reason: "lock-timeout" });
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Built Deck")).toBe(before);
    release();
    await holder;
  });

  it("useDeckFolders().toggleStar resolves false, leaves the star unchanged, and shows the busy toast", async () => {
    localStorage.setItem(STORAGE_KEY_PREFIX + "Starrable Deck", JSON.stringify({ main: [], sideboard: [] }));
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
    const { result } = renderHook(() => useDeckFolders());
    const { release, holder } = await heldLock();
    let call!: Promise<boolean>;
    act(() => {
      call = result.current.toggleStar("Starrable Deck");
    });
    await act(async () => {
      expect(await call).toBe(false);
    });
    expect(JSON.parse(localStorage.getItem(DECK_METADATA_KEY) ?? "{}")["Starrable Deck"]?.starred).toBeFalsy();
    expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't update deck organization");
    release();
    await holder;
  });
});

describe("precon overwrite consent inside the transaction", () => {
  const precon: DeckEntry = {
    name: "Precon", code: "SET", type: "Commander Deck", coveragePct: 100,
    mainBoard: [{ name: "Forest", count: 40 }], sideBoard: [], commander: undefined,
  };

  it("\"keep\" leaves a deck written by the lock's holder untouched", async () => {
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const holder = withSavedDeckLibrary((txn) => {
      void txn;
      localStorage.setItem(STORAGE_KEY_PREFIX + "P", "HOLDER-DATA");
      return held;
    });
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });
    const savedCall = savePreconDeck("P", precon, { type: "keep" });
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });
    release();
    await holder;
    await expect(savedCall).resolves.toBe("kept-existing");
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "P")).toBe("HOLDER-DATA");
  });

  it("\"replace\" overwrites the existing deck (paired positive)", async () => {
    localStorage.setItem(STORAGE_KEY_PREFIX + "P", "HOLDER-DATA");
    await expect(savePreconDeck("P", precon, { type: "replace" })).resolves.toBe("saved");
    const persisted = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "P") ?? "{}");
    expect(persisted.main).toEqual([{ name: "Forest", count: 40 }]);
  });
});

describe("queued organization and adopt refuse against a deck that changed while they waited", () => {
  async function heldReplacement(name: string): Promise<{ release: () => void; holder: Promise<void> }> {
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const holder = withSavedDeckLibrary(async (txn) => {
      await held;
      removeDeckMeta(txn, name);
      writeSavedDeckData(txn, name, "REPLACEMENT-DATA");
      stampDeckMeta(txn, name, 2000);
    });
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });
    return { release, holder };
  }

  it("useDeckFolders().toggleStar queued behind a delete of the deck stars nothing and leaves no metadata", async () => {
    localStorage.setItem(STORAGE_KEY_PREFIX + "S", JSON.stringify({ main: [], sideboard: [] }));
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
    const { result } = renderHook(() => useDeckFolders());
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const holder = withSavedDeckLibrary(async (txn) => {
      await held;
      localStorage.removeItem(STORAGE_KEY_PREFIX + "S");
      removeDeckMeta(txn, "S");
    });
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });
    let call!: Promise<boolean>;
    act(() => {
      call = result.current.toggleStar("S");
    });
    await waitPendingThenRelease(release, holder);
    await act(async () => {
      expect(await call).toBe(false);
    });
    expect(getDeckMeta("S")).toBeNull();
    expect(useAppNotificationStore.getState().notification?.description).toBe(
      "This deck changed before your action ran, so nothing was changed. Check the deck and try again.",
    );
  });

  it("useDeckFolders().assignDeck queued behind a replacement leaves the replacement unfiled (paired positive: an unchanged deck is filed)", async () => {
    localStorage.setItem(STORAGE_KEY_PREFIX + "F", JSON.stringify({ main: [], sideboard: [] }));
    localStorage.setItem(STORAGE_KEY_PREFIX + "G", JSON.stringify({ main: [], sideboard: [] }));
    const { result } = renderHook(() => useDeckFolders());

    const { release, holder } = await heldReplacement("F");
    let call!: Promise<boolean>;
    act(() => {
      call = result.current.assignDeck("F", "folder-x");
    });
    await waitPendingThenRelease(release, holder);
    await act(async () => {
      expect(await call).toBe(false);
    });
    expect(getDeckMeta("F")?.folderId).toBeUndefined();

    // Positive: the same mutator on an untouched deck files it.
    let positiveCall!: Promise<boolean>;
    act(() => {
      positiveCall = result.current.assignDeck("G", "folder-x");
    });
    await act(async () => {
      expect(await positiveCall).toBe(true);
    });
    expect(getDeckMeta("G")?.folderId).toBe("folder-x");
  });

  it("useDeckFolders().createFolder for a deck queued behind a replacement creates no folder and files nothing", async () => {
    localStorage.setItem(STORAGE_KEY_PREFIX + "N", JSON.stringify({ main: [], sideboard: [] }));
    const { result } = renderHook(() => useDeckFolders());
    const { release, holder } = await heldReplacement("N");
    let call!: Promise<DeckFolder | null>;
    act(() => {
      call = result.current.createFolder("Aggro", "N");
    });
    await waitPendingThenRelease(release, holder);
    const created = await call;
    expect(created).toBeNull();
    expect(listFolders()).toEqual([]);
  });

  it("adoptFeedDeck queued behind a replacement of the feed deck copies nothing", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Feed Deck",
      JSON.stringify({ main: [{ name: "Bear", count: 1 }], sideboard: [] }),
    );
    const { release, holder } = await heldReplacement("Feed Deck");
    const call = adoptFeedDeck("Feed Deck", "Adopted Deck");
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });
    release();
    await holder;
    await expect(call).rejects.toBeInstanceOf(SavedDeckChangedError);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Adopted Deck")).toBeNull();
    expect(loadDeckOrigins()).toEqual({});
  });
});
