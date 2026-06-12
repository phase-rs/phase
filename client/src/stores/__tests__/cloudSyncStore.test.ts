import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PhaseBackup } from "../../services/backup";
import type {
  CloudSyncProvider,
  RemoteMeta,
  RemoteSnapshot,
} from "../../services/cloudSync";

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

/** The cheap metadata-only read syncNow now leads with. */
function meta(revision: number): RemoteMeta {
  return { revision, updatedAt: "2026-05-26T01:00:00.000Z" };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function flushAsync() {
  return new Promise<void>((resolve) => {
    setTimeout(resolve, 0);
  });
}

let provider: {
  identity: ReturnType<typeof vi.fn>;
  pullMeta: ReturnType<typeof vi.fn>;
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
    pullMeta: vi.fn(),
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
    provider.pullMeta.mockResolvedValue(null);
    buildBackupMock.mockReturnValue(fakeBackup({ decks: { Local: "{}" } }));
    provider.push.mockResolvedValue({ revision: 1, updatedAt: "t" });

    await useCloudSyncStore.getState().syncNow();

    expect(provider.push).toHaveBeenCalledWith(expect.anything(), null);
    // Seeding never needs the remote body.
    expect(provider.pull).not.toHaveBeenCalled();
    const s = useCloudSyncStore.getState();
    expect(s.status).toBe("synced");
    expect(s.lastSyncedRevision).toBe(1);
    expect(s.dirty).toBe(false);
  });

  it("first sign-in with ONLY local preferences (no decks) conflicts instead of overwriting", async () => {
    // Regression guard for the data-loss bug: prefs-only local must not be
    // silently replaced by a remote pull.
    provider.pullMeta.mockResolvedValue(meta(5));
    provider.pull.mockResolvedValue(remote(5));
    buildBackupMock.mockReturnValue(fakeBackup({ preferences: "{\"vol\":1}" }));

    await useCloudSyncStore.getState().syncNow();

    expect(useCloudSyncStore.getState().status).toBe("conflict");
    expect(applyBackupMock).not.toHaveBeenCalled();
    expect(provider.push).not.toHaveBeenCalled();
  });

  it("adopts the cloud copy when local is genuinely empty", async () => {
    provider.pullMeta.mockResolvedValue(meta(5));
    provider.pull.mockResolvedValue(remote(5));
    buildBackupMock.mockReturnValue(fakeBackup()); // nothing local at all

    await useCloudSyncStore.getState().syncNow();

    expect(applyBackupMock).toHaveBeenCalledWith(expect.anything(), "overwrite");
    // The body is fetched exactly once to adopt it — no redundant re-pull.
    expect(provider.pull).toHaveBeenCalledTimes(1);
    const s = useCloudSyncStore.getState();
    expect(s.lastSyncedRevision).toBe(5);
    expect(s.status).toBe("synced");
  });

  it("fast-forwards local changes when the remote is unchanged WITHOUT pulling the body", async () => {
    useCloudSyncStore.setState({ lastSyncedRevision: 5, dirty: true });
    provider.pullMeta.mockResolvedValue(meta(5));
    buildBackupMock.mockReturnValue(fakeBackup({ decks: { Local: "{}" } }));
    provider.push.mockResolvedValue({ revision: 6, updatedAt: "t" });

    await useCloudSyncStore.getState().syncNow();

    expect(provider.push).toHaveBeenCalledWith(expect.anything(), 5);
    // Egress guarantee: a local-only push must not fetch the remote envelope.
    expect(provider.pull).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState().lastSyncedRevision).toBe(6);
  });

  it("confirms in-sync state from the metadata read alone, never pulling the body", async () => {
    // The hot path: revision unchanged and nothing dirty (e.g. a tab refocus).
    // This is the egress win — it must cost a pullMeta and nothing else.
    useCloudSyncStore.setState({ lastSyncedRevision: 5, dirty: false });
    provider.pullMeta.mockResolvedValue(meta(5));
    buildBackupMock.mockReturnValue(fakeBackup({ decks: { Local: "{}" } }));

    await useCloudSyncStore.getState().syncNow();

    expect(provider.pull).not.toHaveBeenCalled();
    expect(provider.push).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState().status).toBe("synced");
  });

  it("coalesced callers wait for the trailing sync they requested", async () => {
    useCloudSyncStore.setState({ lastSyncedRevision: 5, dirty: false });
    const firstMeta = deferred<RemoteMeta>();
    const secondMeta = deferred<RemoteMeta>();
    provider.pullMeta
      .mockReturnValueOnce(firstMeta.promise)
      .mockReturnValueOnce(secondMeta.promise);
    provider.pull.mockResolvedValue(remote(6));
    buildBackupMock.mockReturnValue(fakeBackup());

    const firstSync = useCloudSyncStore.getState().syncNow();
    expect(provider.pullMeta).toHaveBeenCalledTimes(1);

    let secondResolved = false;
    const secondSync = useCloudSyncStore
      .getState()
      .syncNow()
      .then(() => {
        secondResolved = true;
      });

    firstMeta.resolve(meta(5));
    await firstSync;
    await flushAsync();

    expect(provider.pullMeta).toHaveBeenCalledTimes(2);
    expect(secondResolved).toBe(false);
    secondMeta.resolve(meta(6));
    await secondSync;

    expect(provider.pullMeta).toHaveBeenCalledTimes(2);
    expect(provider.pull).toHaveBeenCalledTimes(1);
    expect(useCloudSyncStore.getState().lastSyncedRevision).toBe(6);
    expect(secondResolved).toBe(true);
  });

  it("surfaces a lost write race as a conflict, pulling the body only in the catch", async () => {
    useCloudSyncStore.setState({ lastSyncedRevision: 5, dirty: true });
    // Meta read says not-ahead → local-only push path; the push loses the race
    // and the catch re-pulls the full body to build the conflict diff.
    provider.pullMeta.mockResolvedValue(meta(5));
    provider.pull.mockResolvedValue(remote(6));
    buildBackupMock.mockReturnValue(fakeBackup({ decks: { Local: "{}" } }));
    provider.push.mockRejectedValue(new SyncConflictError());

    await useCloudSyncStore.getState().syncNow();

    expect(useCloudSyncStore.getState().status).toBe("conflict");
    // The body is fetched exactly once — in the catch, not on the routine path.
    expect(provider.pull).toHaveBeenCalledTimes(1);
  });

  it("reseeds instead of erroring when the row vanished mid-conflict", async () => {
    // Lost the write race, but the re-pull finds the row gone (account deletion
    // or a delete+reinsert race). Reseed this device's data rather than
    // dead-ending at status:"error".
    useCloudSyncStore.setState({ lastSyncedRevision: 5, dirty: true });
    provider.pullMeta.mockResolvedValue(meta(5));
    provider.pull.mockResolvedValue(null); // row gone on the catch re-pull
    buildBackupMock.mockReturnValue(fakeBackup({ decks: { Local: "{}" } }));
    provider.push
      .mockRejectedValueOnce(new SyncConflictError()) // the racing push
      .mockResolvedValueOnce({ revision: 1, updatedAt: "t" }); // the reseed

    await useCloudSyncStore.getState().syncNow();

    const s = useCloudSyncStore.getState();
    expect(s.status).toBe("synced");
    expect(s.lastSyncedRevision).toBe(1);
    expect(provider.push).toHaveBeenLastCalledWith(expect.anything(), null);
  });
});
