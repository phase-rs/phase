import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { loadFeedSubscriptions, saveFeedSubscriptions, STORAGE_KEY_PREFIX } from "../../../constants/storage";
import { setSavedDeckTxnLockWaitForTests, withSavedDeckLibrary } from "../../../services/savedDeckTransaction";
import { useAppNotificationStore } from "../../../stores/appToastStore";
import {
  installFifoWebLocks,
  resetSavedDeckLibraryForTests,
  uninstallWebLocks,
} from "../../../test/helpers/webLocks";
import { FeedManagerModal } from "../FeedManagerModal";

const FEED_JSON = {
  id: "starter-decks",
  name: "Starter Decks",
  version: 2,
  updated: "2026-01-02T00:00:00.000Z",
  decks: [{ name: "Fresh Deck", colors: [], main: [{ name: "Island", count: 60 }], sideboard: [] }],
};

describe("FeedManagerModal — refused by a busy saved-deck library", () => {
  beforeEach(async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests(); // clears localStorage; seed the subscription after
    saveFeedSubscriptions([
      { sourceId: "starter-decks", url: "/feeds/starter-decks.json", type: "bundled", subscribedAt: 1, lastRefreshedAt: 1, lastVersion: 1 },
    ]);
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      statusText: "OK",
      json: () => Promise.resolve(FEED_JSON),
    }));
  });

  afterEach(() => {
    uninstallWebLocks();
    vi.unstubAllGlobals();
    cleanup();
  });

  it("Refresh refused by a busy library shows the toast, leaves no local error, and does not sync the fetched feed", async () => {
    const user = userEvent.setup();
    render(<FeedManagerModal open onClose={vi.fn()} />);

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    setSavedDeckTxnLockWaitForTests(20);
    await user.click(screen.getAllByRole("button", { name: "Refresh" })[0]);

    await vi.waitFor(() => {
      expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't update deck feeds");
    });
    // The refusal is surfaced as a toast, not the modal's own inline error banner —
    // attemptSavedDeckWrite's `{ ok: false }` short-circuit must not fall through to `catch`.
    expect(screen.queryByText(/unavailable/i)).not.toBeInTheDocument();
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Fresh Deck")).toBeNull();
    // A lock refusal is not the feed's fault: it must not be persisted onto the
    // subscription as a feed error, which would otherwise show a stale, misleading
    // "feed error" line even once the library frees up.
    expect(loadFeedSubscriptions().find((s) => s.sourceId === "starter-decks")?.error).toBeUndefined();

    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
    releaseHolder();
    await holder;
  });
});

describe("FeedManagerModal — custom feed URL field", () => {
  beforeEach(async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests();
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      statusText: "OK",
      json: () => Promise.resolve({ ...FEED_JSON, id: "custom-feed" }),
    }));
  });

  afterEach(() => {
    uninstallWebLocks();
    vi.unstubAllGlobals();
    cleanup();
  });

  it("a URL typed while another is being added is kept", async () => {
    const user = userEvent.setup();
    render(<FeedManagerModal open onClose={vi.fn()} />);
    const urlInput = screen.getByPlaceholderText("https://example.com/feed.json");

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    await user.type(urlInput, "https://a.example.com/feed.json");
    await user.click(screen.getByRole("button", { name: "Add" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    await user.clear(urlInput);
    await user.type(urlInput, "https://b.example.com/feed.json");

    releaseHolder();
    await holder;
    await vi.waitFor(async () => {
      const q = await navigator.locks.query();
      expect(q.held).toHaveLength(0);
      expect(q.pending).toHaveLength(0);
    });

    expect(urlInput).toHaveValue("https://b.example.com/feed.json");
    expect(vi.mocked(fetch)).toHaveBeenCalledWith("https://a.example.com/feed.json", expect.anything());
  });

  it("adding a custom feed clears its URL (paired positive)", async () => {
    const user = userEvent.setup();
    render(<FeedManagerModal open onClose={vi.fn()} />);
    const urlInput = screen.getByPlaceholderText("https://example.com/feed.json");

    await user.type(urlInput, "https://a.example.com/feed.json");
    await user.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => expect(urlInput).toHaveValue(""));
  });
});
