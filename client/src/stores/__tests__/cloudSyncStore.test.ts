import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PhaseBackup } from "../../services/backup";
import type { CloudSyncProvider, RemoteSnapshot } from "../../services/cloudSync";

// Hoisted mock fns so the vi.mock factories below can reference them.
const { buildBackupMock, applyBackupMock, getProvider } = vi.hoisted(() => ({
  buildBackupMock: vi.fn(),
  applyBackupMock: vi.fn(),
  getProvider: vi.fn(),
}));

vi.mock("../../services/backup", () => ({
  buildBackup: buildBackupMock,
  applyBackup: applyBackupMock,
}));
vi.mock("../../services/cloudSync", () => ({
  getCloudSyncProvider: getProvider,
  SyncConflictError: class SyncConflictError extends Error {},
}));
vi.mock("../../services/cloudSync/storageWatcher", () => ({
  watchUserStorage: () => () => {},
  withStorageWatchSuppressed: (fn: () => void) => fn(),
}));

import { useCloudSyncStore } from "../cloudSyncStore";
import { SyncConflictError } from "../../services/cloudSync";

const reloadMock = vi.fn();

function fakeBackup(over: Partial<PhaseBackup> = {}): PhaseBackup {
  return {
    version: 1,
    exportedAt: "2026-05-26T00:00:00.000Z",
    preferences: null,
    decks: {},
    deckMetadata: null,
    activeDeck: null,
    feedSubscriptions: null,
    feedDeckOrigins: null,
    ...over,
  };
}

function remote(revision: number): RemoteSnapshot {
  return {
    backup: fakeBackup({ decks: { "Cloud Deck": "{}" } }),
    meta: { revision, updatedAt: "2026-05-26T01:00:00.000Z" },
  };
}

let provider: {
  identity: ReturnType<typeof vi.fn>;
  pull: ReturnType<typeof vi.fn>;
  push: ReturnType<typeof vi.fn>;
};

beforeEach(() => {
  vi.clearAllMocks();
  Object.defineProperty(window, "location", {
    configurable: true,
    value: { href: "http://localhost/", reload: reloadMock },
  });
  provider = {
    identity: vi.fn(() => ({ userId: "u1", label: "Tester" })),
    pull: vi.fn(),
    push: vi.fn(),
  };
  getProvider.mockReturnValue(provider as unknown as CloudSyncProvider);
  useCloudSyncStore.setState({
    available: true,
    identity: { userId: "u1", label: "Tester" },
    status: "idle",
    error: null,
    dirty: false,
    lastSyncedRevision: null,
    lastSyncedAt: null,
    conflict: null,
  });
});

describe("cloudSyncStore.syncNow reconciliation", () => {
  it("seeds an empty account by pushing local with no expected revision", async () => {
    provider.pull.mockResolvedValue(null);
    buildBackupMock.mockReturnValue(fakeBackup({ decks: { Local: "{}" } }));
    provider.push.mockResolvedValue({ revision: 1, updatedAt: "t" });

    await useCloudSyncStore.getState().syncNow();

    expect(provider.push).toHaveBeenCalledWith(expect.anything(), null);
    const s = useCloudSyncStore.getState();
    expect(s.status).toBe("synced");
    expect(s.lastSyncedRevision).toBe(1);
    expect(s.dirty).toBe(false);
  });

  it("first sign-in with ONLY local preferences (no decks) conflicts instead of overwriting", async () => {
    // Regression guard for the data-loss bug: prefs-only local must not be
    // silently replaced by a remote pull.
    provider.pull.mockResolvedValue(remote(5));
    buildBackupMock.mockReturnValue(fakeBackup({ preferences: "{\"vol\":1}" }));

    await useCloudSyncStore.getState().syncNow();

    expect(useCloudSyncStore.getState().status).toBe("conflict");
    expect(applyBackupMock).not.toHaveBeenCalled();
    expect(provider.push).not.toHaveBeenCalled();
  });

  it("adopts the cloud copy when local is genuinely empty", async () => {
    provider.pull.mockResolvedValue(remote(5));
    buildBackupMock.mockReturnValue(fakeBackup()); // nothing local at all

    await useCloudSyncStore.getState().syncNow();

    expect(applyBackupMock).toHaveBeenCalledWith(expect.anything(), "overwrite");
    const s = useCloudSyncStore.getState();
    expect(s.lastSyncedRevision).toBe(5);
    expect(s.status).toBe("synced");
  });

  it("fast-forwards local changes when the remote is unchanged", async () => {
    useCloudSyncStore.setState({ lastSyncedRevision: 5, dirty: true });
    provider.pull.mockResolvedValue(remote(5));
    buildBackupMock.mockReturnValue(fakeBackup({ decks: { Local: "{}" } }));
    provider.push.mockResolvedValue({ revision: 6, updatedAt: "t" });

    await useCloudSyncStore.getState().syncNow();

    expect(provider.push).toHaveBeenCalledWith(expect.anything(), 5);
    expect(useCloudSyncStore.getState().lastSyncedRevision).toBe(6);
  });

  it("surfaces a lost write race as a conflict, not an error", async () => {
    useCloudSyncStore.setState({ lastSyncedRevision: 5, dirty: true });
    provider.pull
      .mockResolvedValueOnce(remote(5)) // initial read: not ahead
      .mockResolvedValueOnce(remote(6)); // re-read after the failed push
    buildBackupMock.mockReturnValue(fakeBackup({ decks: { Local: "{}" } }));
    provider.push.mockRejectedValue(new SyncConflictError());

    await useCloudSyncStore.getState().syncNow();

    expect(useCloudSyncStore.getState().status).toBe("conflict");
  });
});
