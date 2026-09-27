import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const platform = vi.hoisted(() => ({ desktop: false }));

vi.mock("../../../services/platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../services/platform")>()),
  isDesktopTauri: () => platform.desktop,
}));

import { useCloudSyncStore } from "../../../stores/cloudSyncStore";
import { setSavedDeckTxnLockWaitForTests, withSavedDeckLibrary } from "../../../services/savedDeckTransaction";
import { useAppNotificationStore } from "../../../stores/appToastStore";
import {
  installFifoWebLocks,
  resetSavedDeckLibraryForTests,
  uninstallWebLocks,
} from "../../../test/helpers/webLocks";
import { PreferencesModal } from "../PreferencesModal";

const actions = {
  signIn: vi.fn(async () => {}),
  signOut: vi.fn(async () => {}),
  syncNow: vi.fn(async () => {}),
  resolveConflict: vi.fn(async () => {}),
};

function setCloudState(overrides: Record<string, unknown> = {}) {
  useCloudSyncStore.setState({
    available: true,
    paused: false,
    identity: null,
    sessionResolved: true,
    status: "idle",
    error: null,
    dirty: false,
    lastSyncedAt: null,
    conflict: null,
    conflictDiff: null,
    ...actions,
    ...overrides,
  });
}

const BACKUP_JSON = JSON.stringify({
  version: 1,
  exportedAt: "2026-01-01T00:00:00.000Z",
  preferences: null,
  decks: {},
  deckMetadata: null,
  activeDeck: null,
  feedSubscriptions: null,
  feedDeckOrigins: null,
});

beforeEach(async () => {
  vi.clearAllMocks();
  platform.desktop = false;
  setCloudState();
  installFifoWebLocks();
  await resetSavedDeckLibraryForTests();
  useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
});

afterEach(() => {
  uninstallWebLocks();
  cleanup();
  setCloudState();
});

describe("PreferencesModal — import refused by a busy saved-deck library", () => {
  it("shows the busy toast, imports nothing, and reports no error", async () => {
    const user = userEvent.setup();
    render(<PreferencesModal onClose={vi.fn()} initialTab="data" />);

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });
    setSavedDeckTxnLockWaitForTests(20);

    const file = new File([BACKUP_JSON], "backup.json", { type: "application/json" });
    const fileInput = document.querySelector('input[type="file"]') as HTMLInputElement;
    await user.upload(fileInput, file);
    await user.click(await screen.findByRole("button", { name: "Replace all" }));

    await vi.waitFor(() => {
      expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't restore backup");
    });
    // The refusal must resolve through attemptSavedDeckWrite's `{ ok: false }`
    // path, not fall through to onImport's own catch and its inline error text.
    expect(screen.queryByText(/unavailable/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/^Imported \d/i)).not.toBeInTheDocument();

    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
    releaseHolder();
    await holder;
  });
});
