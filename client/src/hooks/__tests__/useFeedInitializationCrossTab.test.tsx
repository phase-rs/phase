// Drives the REAL hook against the REAL feedService (unlike
// useFeedInitialization.test.tsx, which mocks feedService entirely) so a
// cross-tab profile replacement actually reaches publishUnlessSuperseded's
// generation check and produces a real AbortError, not a mocked one.
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { useFeedInitialization } from "../useFeedInitialization";
import { _resetFeedCacheForTests } from "../../services/feedPersistence";
import { FEED_SUBSCRIPTIONS_KEY, PROFILE_REPLACEMENT_KEY, profileReplacementGeneration } from "../../constants/storage";
import { FEED_REGISTRY } from "../../data/feedRegistry";
import { withSavedDeckLibraryOrSkip } from "../../services/savedDeckTransaction";
import { installFifoWebLocks, resetSavedDeckLibraryForTests, uninstallWebLocks } from "../../test/helpers/webLocks";

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

it("re-runs after a cross-tab profile replacement dispatched as a storage event", async () => {
  localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify(bundledSubs));
  global.fetch = vi.fn().mockImplementation(() => Promise.resolve({
    ok: true,
    status: 200,
    statusText: "ok",
    json: () => Promise.resolve({ id: "b", name: "B", version: 1, updated: "x", decks: [deck("Bundled Deck")] }),
  }));

  let release!: () => void;
  const held = new Promise<void>((resolve) => { release = resolve; });
  const other = withSavedDeckLibraryOrSkip(async () => {
    await held;
    localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify(bundledSubs));
    const next = profileReplacementGeneration() + 1;
    localStorage.setItem(PROFILE_REPLACEMENT_KEY, String(next));
  }, "run-unguarded");
  await vi.waitFor(async () => expect((await navigator.locks.query()).held).toHaveLength(1));

  const hook = renderHook(() => useFeedInitialization(false));
  await vi.waitFor(async () => expect((await navigator.locks.query()).pending).toHaveLength(1));

  release();
  await other;
  // The queued feed sync loses the race against the "other tab" and throws
  // AbortError; without a re-run trigger the hook would stay unsettled.
  await act(async () => { await new Promise((r) => setTimeout(r, 50)); });
  expect(hook.result.current).toBe(false);

  // The cross-tab counterpart of PROFILE_REPLACED_EVENT: a real `storage`
  // event fires in every window but the one that made the write, carrying
  // the bumped key.
  act(() => {
    window.dispatchEvent(new StorageEvent("storage", { key: PROFILE_REPLACEMENT_KEY, newValue: String(profileReplacementGeneration()) }));
  });
  await vi.waitFor(() => expect(hook.result.current).toBe(true));

  hook.unmount();
});

it("does not react to a storage event for an unrelated key", async () => {
  localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify(bundledSubs));
  global.fetch = vi.fn().mockImplementation(() => Promise.resolve({
    ok: true,
    status: 200,
    statusText: "ok",
    json: () => Promise.resolve({ id: "b", name: "B", version: 1, updated: "x", decks: [deck("Bundled Deck")] }),
  }));

  const hook = renderHook(() => useFeedInitialization(false));
  await vi.waitFor(() => expect(hook.result.current).toBe(true));
  const fetchCallsBefore = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.length;

  act(() => {
    window.dispatchEvent(new StorageEvent("storage", { key: "some-other-key", newValue: "1" }));
  });
  await act(async () => { await new Promise((r) => setTimeout(r, 20)); });

  expect((global.fetch as ReturnType<typeof vi.fn>).mock.calls.length).toBe(fetchCallsBefore);
  hook.unmount();
});
