// Drives the REAL importBackupFromFile against the REAL feedService/savedDeckTransaction
// stack (unlike backup.test.ts, which exercises applyBackup directly against a stub txn) to
// prove a file restore queued ahead of a feed sync actually causes the sync to abort instead
// of overwriting the just-restored subscriptions.
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { initializeFeeds } from "../feedService";
import { applyBackup, importBackupFromFile } from "../backup";
import { _resetFeedCacheForTests } from "../feedPersistence";
import { FEED_SUBSCRIPTIONS_KEY, STORAGE_KEY_PREFIX, profileReplacementGeneration } from "../../constants/storage";
import { FEED_REGISTRY } from "../../data/feedRegistry";
import { withSavedDeckLibraryOrSkip } from "../savedDeckTransaction";
import { installFifoWebLocks, resetSavedDeckLibraryForTests, testSavedDeckTxn, uninstallWebLocks } from "../../test/helpers/webLocks";

const deck = (name: string) => ({ name, colors: ["R"], main: [{ count: 4, name: "Lightning Bolt" }], sideboard: [] });
const bundledSubs = FEED_REGISTRY.filter((s) => s.type === "bundled").map((s) => ({
  sourceId: s.id,
  url: s.url,
  type: "bundled" as const,
  subscribedAt: 1,
  lastRefreshedAt: Date.now(),
  lastVersion: 1,
}));

beforeEach(async () => {
  await resetSavedDeckLibraryForTests();
  _resetFeedCacheForTests();
  installFifoWebLocks();
});
afterEach(() => uninstallWebLocks());

function seedRemoteSubscription(): void {
  localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify([
    { sourceId: "remote", url: "u", type: "remote", subscribedAt: 1, lastRefreshedAt: 0, lastVersion: 0 },
    ...bundledSubs,
  ]));
}

function mockFetchRemoteAndBundled(): void {
  global.fetch = vi.fn().mockImplementation((url: string) => Promise.resolve({
    ok: true,
    status: 200,
    statusText: "ok",
    json: () => Promise.resolve(
      url === "u"
        ? { id: "remote", name: "R", version: 1, updated: "x", decks: [deck("Stale Remote Deck")] }
        : { id: "b", name: "B", version: 1, updated: "x", decks: [deck("Bundled Deck")] },
    ),
  }));
}

function backupWithOnlyBundledSubscriptions() {
  return {
    version: 1 as const,
    exportedAt: "x",
    preferences: null,
    decks: {},
    deckMetadata: null,
    activeDeck: null,
    feedSubscriptions: JSON.stringify(bundledSubs),
    feedDeckOrigins: null,
  };
}

it("skips a feed sync queued behind a file restore that dropped its subscription", async () => {
  seedRemoteSubscription();
  mockFetchRemoteAndBundled();
  const backup = backupWithOnlyBundledSubscriptions();

  // Hold the saved-deck lock so both the restore and the feed sync queue
  // behind it; the feed sync captures its pre-restore generation before
  // waiting.
  let release!: () => void;
  const held = new Promise<void>((resolve) => { release = resolve; });
  const gate = withSavedDeckLibraryOrSkip(() => held, "run-unguarded");
  await vi.waitFor(async () => expect((await navigator.locks.query()).held).toHaveLength(1));

  const restore = importBackupFromFile(new File([JSON.stringify(backup)], "b.json"), "overwrite");
  await vi.waitFor(async () => expect((await navigator.locks.query()).pending).toHaveLength(1));
  const init = initializeFeeds().then(() => "resolved", (e: unknown) => `rejected:${(e as Error).name}`);
  await vi.waitFor(async () => expect((await navigator.locks.query()).pending).toHaveLength(2));

  release();
  await gate;
  await restore;
  const outcome = await init;

  const subs = (JSON.parse(localStorage.getItem(FEED_SUBSCRIPTIONS_KEY)!) as { sourceId: string }[])
    .map((s) => s.sourceId);
  expect(outcome).toBe("rejected:AbortError");
  expect(profileReplacementGeneration()).toBe(1);
  expect(subs).not.toContain("remote");
  expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Stale Remote Deck")).toBeNull();
});

it("a single applyBackup call bumps the profile-replacement generation by exactly one", () => {
  const before = profileReplacementGeneration();
  applyBackup(testSavedDeckTxn, backupWithOnlyBundledSubscriptions(), "overwrite");
  expect(profileReplacementGeneration() - before).toBe(1);
});

it("control: without a queued restore, the feed sync commits its subscription changes", async () => {
  seedRemoteSubscription();
  mockFetchRemoteAndBundled();

  let release!: () => void;
  const held = new Promise<void>((resolve) => { release = resolve; });
  const gate = withSavedDeckLibraryOrSkip(() => held, "run-unguarded");
  await vi.waitFor(async () => expect((await navigator.locks.query()).held).toHaveLength(1));
  const init = initializeFeeds().then(() => "resolved", (e: unknown) => `rejected:${(e as Error).name}`);
  await vi.waitFor(async () => expect((await navigator.locks.query()).pending).toHaveLength(1));

  release();
  await gate;
  const outcome = await init;

  expect(outcome).toBe("resolved");
  expect(profileReplacementGeneration()).toBe(0);
});
