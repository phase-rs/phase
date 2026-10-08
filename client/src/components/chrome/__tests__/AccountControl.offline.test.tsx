import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("../../../services/cloudSync/supabaseClient", () => ({
  isSupabaseConfigured: () => true,
}));

import { useCloudSyncStore } from "../../../stores/cloudSyncStore";
import { AccountControl } from "../AccountControl";

const actions = {
  signIn: vi.fn(async () => {}),
  signOut: vi.fn(async () => {}),
  syncNow: vi.fn(async () => {}),
  resolveConflict: vi.fn(async () => {}),
};

function setCloudState(overrides: Record<string, unknown> = {}) {
  useCloudSyncStore.setState({
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

function openAccountControl(autoOpened = false) {
  render(<AccountControl />);
  if (!autoOpened) {
    fireEvent.click(screen.getByRole("button", { name: "Cloud Sync" }));
  }
}

function cloudIconClasses(): string {
  return screen.getByRole("button", { name: "Cloud Sync" }).querySelector("svg")?.getAttribute("class") ?? "";
}

beforeEach(() => {
  vi.clearAllMocks();
  setCloudState();
});

afterEach(() => {
  cleanup();
  setCloudState();
});

describe("AccountControl offline cloud controls", () => {
  it("shows paused state and disables signed-out provider actions without invoking them", () => {
    setCloudState({ paused: true, status: "syncing" });
    openAccountControl();

    expect(screen.getByText("Cloud sync is paused while you're offline.")).toBeInTheDocument();
    expect(screen.queryByText("Syncing…")).not.toBeInTheDocument();
    expect(document.querySelector(".animate-spin")).toBeNull();

    const discord = screen.getByRole("button", { name: "Sign in with Discord" });
    const google = screen.getByRole("button", { name: "Sign in with Google" });
    expect(discord).toBeDisabled();
    expect(google).toBeDisabled();
    fireEvent.click(discord);
    fireEvent.click(google);
    expect(actions.signIn).not.toHaveBeenCalled();
  });

  it("preserves signed-in conflict, error, last-sync detail, and icon priority while paused", () => {
    setCloudState({
      paused: true,
      identity: { userId: "tester", label: "Tester" },
      status: "error",
      error: "network unavailable",
      dirty: true,
      conflict: {} as never,
      conflictDiff: {
        decksAdded: 1,
        decksModified: 0,
        decksRemoved: 0,
        prefsChanged: false,
        feedsChanged: false,
        otherChanged: false,
      },
    });
    openAccountControl(true);

    expect(screen.getByText("Signed in as Tester")).toBeInTheDocument();
    expect(screen.getByText("Both copies have changes")).toBeInTheDocument();
    expect(screen.getByText("Decks: 1 added, 0 changed, 0 removed")).toBeInTheDocument();
    expect(screen.getByText("Sync failed: network unavailable")).toBeInTheDocument();
    expect(cloudIconClasses()).toContain("text-rose-400");

    const controls = [
      screen.getByRole("button", { name: "Use cloud" }),
      screen.getByRole("button", { name: "Keep this device" }),
      screen.getByRole("button", { name: "Keep both deck collections" }),
      screen.getByRole("button", { name: "Sign out" }),
    ];
    controls.forEach((control) => {
      expect(control).toBeDisabled();
      fireEvent.click(control);
    });
    expect(actions.resolveConflict).not.toHaveBeenCalled();
    expect(actions.signOut).not.toHaveBeenCalled();

    cleanup();
    setCloudState({
      paused: true,
      identity: { userId: "tester", label: "Tester" },
      status: "syncing",
      lastSyncedAt: "2026-01-02T03:04:05.000Z",
    });
    openAccountControl();
    expect(screen.getByText(/Last synced/)).toBeInTheDocument();
    expect(cloudIconClasses()).toContain("text-slate-400");
    expect(document.querySelector(".animate-spin")).toBeNull();

    const syncNow = screen.getByRole("button", { name: "Sync now" });
    expect(syncNow).toBeDisabled();
    fireEvent.click(syncNow);
    expect(actions.syncNow).not.toHaveBeenCalled();
  });

  it("disables Sync now solely because paused when the preserved status is synced", () => {
    setCloudState({
      paused: true,
      identity: { userId: "tester", label: "Tester" },
      status: "synced",
    });
    openAccountControl();

    const syncNow = screen.getByRole("button", { name: "Sync now" });
    expect(syncNow).toBeDisabled();
    fireEvent.click(syncNow);
    expect(actions.syncNow).not.toHaveBeenCalled();
  });

  it("keeps conflict and dirty icon priority above paused while withholding the synced glow", () => {
    setCloudState({
      paused: true,
      identity: { userId: "tester", label: "Tester" },
      status: "conflict",
      conflict: {} as never,
    });
    openAccountControl(true);
    expect(cloudIconClasses()).toContain("text-amber-500");

    cleanup();
    setCloudState({
      paused: true,
      identity: { userId: "tester", label: "Tester" },
      status: "synced",
      dirty: true,
    });
    openAccountControl();
    expect(cloudIconClasses()).toContain("text-amber-400");

    cleanup();
    setCloudState({
      paused: true,
      identity: { userId: "tester", label: "Tester" },
      status: "synced",
    });
    openAccountControl();
    expect(cloudIconClasses()).toContain("text-slate-400");
    expect(cloudIconClasses()).not.toContain("drop-shadow-");
  });

  it("keeps the online unresolved session state neutral without sign-in controls", () => {
    setCloudState({ paused: false, sessionResolved: false, status: "idle" });
    openAccountControl();

    expect(screen.getByText("Syncing…")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Sign in with Discord" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Sign in with Google" })).not.toBeInTheDocument();
    expect(cloudIconClasses()).toContain("text-slate-400");
  });
});
