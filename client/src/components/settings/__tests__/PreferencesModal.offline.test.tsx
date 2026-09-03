import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

const platform = vi.hoisted(() => ({ desktop: false }));

vi.mock("../../../services/platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../services/platform")>()),
  isDesktopTauri: () => platform.desktop,
}));
vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

import { useCloudSyncStore } from "../../../stores/cloudSyncStore";
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

function renderPreferences() {
  render(<PreferencesModal onClose={vi.fn()} initialTab="data" />);
}

beforeEach(() => {
  vi.clearAllMocks();
  platform.desktop = false;
  setCloudState();
});

afterEach(() => {
  cleanup();
  setCloudState();
});

describe("PreferencesModal offline cloud controls", () => {
  it("places desktop offline preparation first in Data and hides it on web", () => {
    platform.desktop = true;
    renderPreferences();

    const offlinePreparation = screen.getByRole("heading", { name: "Offline preparation" });
    const visualPacks = screen.getByRole("heading", { name: "Offline card images" });
    const cloudSync = screen.getByRole("heading", { name: "Cloud Sync" });
    const data = screen.getByRole("heading", { name: "Backup & Restore" });
    expect(offlinePreparation.compareDocumentPosition(visualPacks) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(visualPacks.compareDocumentPosition(cloudSync) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(cloudSync.compareDocumentPosition(data) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);

    cleanup();
    platform.desktop = false;
    renderPreferences();
    expect(screen.queryByRole("heading", { name: "Offline preparation" })).not.toBeInTheDocument();
  });

  it("shows paused state and disables signed-out provider actions without invoking them", () => {
    setCloudState({ paused: true, status: "syncing" });
    renderPreferences();

    expect(screen.getByText("Cloud sync is paused while you're offline.")).toBeInTheDocument();
    expect(screen.queryByText("Syncing…")).not.toBeInTheDocument();

    const discord = screen.getByRole("button", { name: "Sign in with Discord" });
    const google = screen.getByRole("button", { name: "Sign in with Google" });
    expect(discord).toBeDisabled();
    expect(google).toBeDisabled();

    fireEvent.click(discord);
    fireEvent.click(google);
    expect(actions.signIn).not.toHaveBeenCalled();
  });

  it("keeps signed-in conflict, diff, error, and provider controls visible but disabled", () => {
    setCloudState({
      paused: true,
      identity: { userId: "tester", label: "Tester" },
      status: "error",
      error: "network unavailable",
      conflict: {} as never,
      conflictDiff: {
        decksAdded: 1,
        decksModified: 2,
        decksRemoved: 3,
        prefsChanged: true,
        feedsChanged: true,
        otherChanged: true,
      },
    });
    renderPreferences();

    expect(screen.getByText("Signed in as Tester")).toBeInTheDocument();
    expect(screen.getByText("Both copies have changes")).toBeInTheDocument();
    expect(screen.getByText("Decks: 1 added, 2 changed, 3 removed")).toBeInTheDocument();
    expect(screen.getByText("Preferences differ")).toBeInTheDocument();
    expect(screen.getByText("Feed subscriptions differ")).toBeInTheDocument();
    expect(screen.getByText("Other settings differ")).toBeInTheDocument();
    expect(screen.getByText("Sync failed: network unavailable")).toBeInTheDocument();

    const controls = [
      screen.getByRole("button", { name: "Use cloud" }),
      screen.getByRole("button", { name: "Keep this device" }),
      screen.getByRole("button", { name: "Keep both deck collections" }),
    ];
    controls.forEach((control) => {
      expect(control).toBeDisabled();
      fireEvent.click(control);
    });
    expect(actions.resolveConflict).not.toHaveBeenCalled();
  });

  it("uses last-sync detail and suppresses the stale syncing spinner while paused", () => {
    setCloudState({
      paused: true,
      identity: { userId: "tester", label: "Tester" },
      status: "syncing",
      lastSyncedAt: "2026-01-02T03:04:05.000Z",
    });
    renderPreferences();

    expect(screen.getByText(/Last synced/)).toBeInTheDocument();
    expect(screen.queryByText("Syncing…")).not.toBeInTheDocument();
    expect(document.querySelector(".animate-spin")).toBeNull();

    const syncNow = screen.getByRole("button", { name: "Sync now" });
    const signOut = screen.getByRole("button", { name: "Sign out" });
    expect(syncNow).toBeDisabled();
    expect(signOut).toBeDisabled();
    fireEvent.click(syncNow);
    fireEvent.click(signOut);
    expect(actions.syncNow).not.toHaveBeenCalled();
    expect(actions.signOut).not.toHaveBeenCalled();
  });

  it("disables Sync now solely because paused when the preserved status is synced", () => {
    setCloudState({
      paused: true,
      identity: { userId: "tester", label: "Tester" },
      status: "synced",
    });
    renderPreferences();

    const syncNow = screen.getByRole("button", { name: "Sync now" });
    expect(syncNow).toBeDisabled();
    fireEvent.click(syncNow);
    expect(actions.syncNow).not.toHaveBeenCalled();
  });

  it("does not flash enabled sign-in controls before an online session resolves", () => {
    setCloudState({ paused: false, sessionResolved: false, status: "idle" });
    renderPreferences();

    expect(screen.getByText("Syncing…")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Sign in with Discord" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Sign in with Google" })).not.toBeInTheDocument();
  });
});
