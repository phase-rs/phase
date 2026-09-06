import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PhaseBackup } from "../../services/backup";
import type { CloudSyncProvider, RemoteMeta, RemoteSnapshot } from "../../services/cloudSync";

const mocks = vi.hoisted(() => ({
  buildBackup: vi.fn(),
  applyBackup: vi.fn(),
  mergeDeckCollections: vi.fn(),
  getProvider: vi.fn(),
  configured: vi.fn(),
  pauseProvider: vi.fn(),
  watch: vi.fn(),
  unwatch: vi.fn(),
  suppress: vi.fn(),
  effectiveOffline: { value: false },
}));

vi.mock("../../services/backup", () => ({
  buildBackup: mocks.buildBackup,
  applyBackup: mocks.applyBackup,
  mergeDeckCollections: mocks.mergeDeckCollections,
}));
vi.mock("../../services/cloudSync", () => ({
  getCloudSyncProvider: mocks.getProvider,
  isCloudSyncConfigured: mocks.configured,
  pauseCloudSyncProvider: mocks.pauseProvider,
  SyncConflictError: class SyncConflictError extends Error {},
}));
vi.mock("../../services/cloudSync/storageWatcher", () => ({
  watchUserStorage: mocks.watch,
  withStorageWatchSuppressed: mocks.suppress,
}));
vi.mock("../connectivityStore", () => ({
  getEffectiveOffline: () => mocks.effectiveOffline.value,
}));

import { adoptCloudSyncHmrState, disposeCloudSyncModuleForTest, useCloudSyncStore } from "../cloudSyncStore";
import { SyncConflictError } from "../../services/cloudSync";

const identity = { userId: "user-1", label: "Tester" };

function backup(overrides: Partial<PhaseBackup> = {}): PhaseBackup {
  return {
    version: 1,
    exportedAt: "2026-01-01T00:00:00.000Z",
    preferences: null,
    decks: {},
    deckMetadata: null,
    activeDeck: null,
    feedSubscriptions: null,
    feedDeckOrigins: null,
    ...overrides,
  };
}

function remote(revision: number): RemoteSnapshot {
  return { backup: backup({ decks: { Cloud: "{}" } }), meta: { revision, updatedAt: "remote" } };
}

function meta(revision: number): RemoteMeta {
  return { revision, updatedAt: "remote" };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

let watched: (() => void) | undefined;
let cleanup: (() => void) | undefined;
let provider: Record<string, ReturnType<typeof vi.fn>>;

async function settle(): Promise<void> {
  await vi.waitFor(() => expect(useCloudSyncStore.getState().sessionResolved).toBe(true));
}

async function readySignedIn(): Promise<void> {
  cleanup = useCloudSyncStore.getState().init();
  await settle();
  provider.identity.mockReturnValue(identity);
  useCloudSyncStore.setState({ identity });
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.effectiveOffline.value = false;
  mocks.configured.mockReturnValue(true);
  mocks.pauseProvider.mockResolvedValue(undefined);
  mocks.watch.mockImplementation((callback: () => void) => {
    watched = callback;
    return mocks.unwatch;
  });
  mocks.suppress.mockImplementation((callback: () => void) => callback());
  provider = {
    resume: vi.fn().mockResolvedValue(undefined),
    restoreSession: vi.fn().mockResolvedValue(null),
    identity: vi.fn(() => null),
    signIn: vi.fn().mockResolvedValue(undefined),
    signOut: vi.fn().mockResolvedValue(undefined),
    pullMeta: vi.fn().mockResolvedValue(meta(1)),
    pull: vi.fn().mockResolvedValue(remote(1)),
    push: vi.fn().mockResolvedValue(meta(2)),
    subscribe: vi.fn(() => async () => {}),
  };
  mocks.getProvider.mockReturnValue(provider as unknown as CloudSyncProvider);
  mocks.buildBackup.mockReturnValue(backup());
  useCloudSyncStore.setState({
    available: false,
    paused: false,
    identity: null,
    sessionResolved: false,
    status: "idle",
    error: null,
    dirty: false,
    lastSyncedRevision: null,
    lastSyncedAt: null,
    conflict: null,
    conflictDiff: null,
  });
});

afterEach(async () => {
  cleanup?.();
  cleanup = undefined;
  // The real HMR completion registry intentionally outlives a module instance.
  // Reset that ownership through its public offline pause boundary so one
  // rejected test disposer cannot become the next test's predecessor.
  mocks.effectiveOffline.value = true;
  useCloudSyncStore.getState().pause();
  await vi.waitFor(() => expect(mocks.pauseProvider).toHaveBeenCalled());
  vi.useRealTimers();
});

describe("cloud sync offline lifecycle", () => {
  it("does not construct a provider on a configured offline cold boot and still observes local writes", async () => {
    mocks.effectiveOffline.value = true;

    useCloudSyncStore.getState().pause();
    watched?.();
    await Promise.resolve();

    expect(mocks.getProvider).not.toHaveBeenCalled();
    expect(provider.resume).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState()).toMatchObject({ paused: true, available: true, sessionResolved: true, dirty: true });
  });

  it("reports an unconfigured offline cold boot without resolving a provider", () => {
    mocks.effectiveOffline.value = true;
    mocks.configured.mockReturnValue(false);

    useCloudSyncStore.getState().pause();

    expect(useCloudSyncStore.getState()).toMatchObject({ paused: true, available: false, sessionResolved: true });
    expect(mocks.getProvider).not.toHaveBeenCalled();
  });

  it("preserves sync state while pausing an online generation", async () => {
    const snapshot = remote(7);
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    mocks.effectiveOffline.value = true;
    useCloudSyncStore.setState({
      dirty: true,
      lastSyncedRevision: 6,
      lastSyncedAt: "local-clock",
      conflict: snapshot,
      conflictDiff: {
        decksAdded: 1,
        decksRemoved: 0,
        decksModified: 0,
        prefsChanged: false,
        feedsChanged: false,
        otherChanged: false,
      },
      status: "conflict",
      error: "keep me",
    });

    useCloudSyncStore.getState().pause();

    expect(useCloudSyncStore.getState()).toMatchObject({
      paused: true,
      available: true,
      sessionResolved: true,
      identity,
      dirty: true,
      lastSyncedRevision: 6,
      lastSyncedAt: "local-clock",
      conflict: snapshot,
      conflictDiff: { decksAdded: 1 },
      status: "conflict",
      error: "keep me",
    });
  });

  it("suppresses every provider action while paused", async () => {
    mocks.effectiveOffline.value = true;
    useCloudSyncStore.getState().pause();

    await Promise.all([
      useCloudSyncStore.getState().signIn("google"),
      useCloudSyncStore.getState().signOut(),
      useCloudSyncStore.getState().syncNow(),
      useCloudSyncStore.getState().resolveConflict("cloud"),
    ]);

    expect(mocks.getProvider).not.toHaveBeenCalled();
  });

  it("blocks network work until both resume and session restoration resolve", async () => {
    const resumed = deferred<void>();
    const restored = deferred<typeof identity | null>();
    provider.resume.mockReturnValue(resumed.promise);
    provider.restoreSession.mockReturnValue(restored.promise);
    provider.identity.mockReturnValue(identity);

    cleanup = useCloudSyncStore.getState().init();
    await useCloudSyncStore.getState().syncNow();
    expect(provider.pullMeta).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState().sessionResolved).toBe(false);

    resumed.resolve();
    await Promise.resolve();
    await useCloudSyncStore.getState().syncNow();
    expect(provider.restoreSession).toHaveBeenCalledTimes(1);
    expect(provider.pullMeta).not.toHaveBeenCalled();

    restored.resolve(identity);
    await settle();
    await vi.waitFor(() => expect(provider.pullMeta).toHaveBeenCalledTimes(1));
    expect(provider.subscribe).toHaveBeenCalledTimes(1);
  });

  it("publishes only a current resume failure and leaves a stale one inert", async () => {
    const oldResume = deferred<void>();
    provider.resume.mockReturnValueOnce(oldResume.promise).mockResolvedValueOnce(undefined);
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);

    cleanup = useCloudSyncStore.getState().init();
    cleanup();
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    oldResume.reject(new Error("stale resume failure"));
    await Promise.resolve();

    expect(useCloudSyncStore.getState()).toMatchObject({ identity, sessionResolved: true });
    expect(useCloudSyncStore.getState().error).not.toBe("stale resume failure");
  });

  it("publishes a current restore failure after readiness instead of leaving pending state", async () => {
    provider.restoreSession.mockRejectedValue(new Error("restore failed"));
    cleanup = useCloudSyncStore.getState().init();
    await settle();

    expect(useCloudSyncStore.getState()).toMatchObject({
      identity: null,
      sessionResolved: true,
      status: "error",
      error: "restore failed",
    });
  });

  it("preserves an offline write and reconciles its newest backup once on resume", async () => {
    mocks.effectiveOffline.value = true;
    useCloudSyncStore.getState().pause();
    mocks.buildBackup.mockReturnValue(backup({ decks: { Newest: "{}" } }));
    watched?.();
    await Promise.resolve();

    mocks.effectiveOffline.value = false;
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockResolvedValue(null);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));

    expect(provider.push).toHaveBeenCalledWith(backup({ decks: { Newest: "{}" } }), null);
    expect(useCloudSyncStore.getState().dirty).toBe(false);
  });

  it("keeps dirty observation active after an online generation pauses", async () => {
    provider.restoreSession.mockResolvedValue(null);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    mocks.effectiveOffline.value = true;
    useCloudSyncStore.getState().pause();
    watched?.();
    await Promise.resolve();

    expect(useCloudSyncStore.getState()).toMatchObject({ paused: true, dirty: true });
    expect(provider.pullMeta).not.toHaveBeenCalled();
  });

  it("suppresses an older delayed pause after a successor resumes online", async () => {
    const disposed = deferred<void>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockReturnValue(() => disposed.promise);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    mocks.effectiveOffline.value = true;
    useCloudSyncStore.getState().pause();
    mocks.effectiveOffline.value = false;
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    expect(provider.subscribe).toHaveBeenCalledTimes(1);

    disposed.resolve();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(2));
    expect(mocks.pauseProvider).not.toHaveBeenCalled();
  });

  it("acknowledges a stale ordinary push before trailing once with its returned revision", async () => {
    const pushed = deferred<RemoteMeta>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockResolvedValueOnce(meta(1)).mockResolvedValue(meta(2));
    provider.push.mockReturnValueOnce(pushed.promise).mockResolvedValue(meta(3));
    mocks.buildBackup.mockReturnValue(backup({ decks: { Local: "{}" } }));
    useCloudSyncStore.setState({ dirty: true, lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));

    watched?.();
    pushed.resolve(meta(2));
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(2));

    expect(provider.push).toHaveBeenNthCalledWith(2, backup({ decks: { Local: "{}" } }), 2);
    expect(useCloudSyncStore.getState()).toMatchObject({ dirty: false, lastSyncedRevision: 3 });
  });

  it("cancels the subsumed debounce while a stale-write trailing reconciliation remains in flight", async () => {
    const firstPush = deferred<RemoteMeta>();
    const trailingPush = deferred<RemoteMeta>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockResolvedValueOnce(meta(1)).mockResolvedValueOnce(meta(2));
    provider.push.mockReturnValueOnce(firstPush.promise).mockReturnValueOnce(trailingPush.promise);
    mocks.buildBackup.mockReturnValue(backup({ decks: { Local: "{}" } }));
    useCloudSyncStore.setState({ dirty: true, lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));
    vi.useFakeTimers();

    watched?.();
    await Promise.resolve();
    firstPush.resolve(meta(2));
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(2));
    await vi.advanceTimersByTimeAsync(3001);

    expect(provider.pullMeta).toHaveBeenCalledTimes(2);
    trailingPush.resolve(meta(3));
    await Promise.resolve();
  });

  it("invalidates a pre-transition sync when sign-out supersedes its metadata read", async () => {
    const metadata = deferred<RemoteMeta | null>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockReturnValue(metadata.promise);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.pullMeta).toHaveBeenCalledTimes(1));
    provider.signOut.mockImplementation(async () => provider.identity.mockReturnValue(null));

    const signOut = useCloudSyncStore.getState().signOut();
    metadata.resolve(meta(1));
    await signOut;

    expect(provider.push).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState()).toMatchObject({ identity: null, status: "idle" });
  });

  it("silently drops a stale reconciliation rejection after a replacement generation", async () => {
    const metadata = deferred<RemoteMeta | null>();
    provider.restoreSession.mockResolvedValueOnce(identity).mockResolvedValueOnce(null);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockReturnValue(metadata.promise);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.pullMeta).toHaveBeenCalledTimes(1));

    cleanup();
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    useCloudSyncStore.setState({ status: "idle", error: null });
    metadata.reject(new Error("stale sync failure"));
    await Promise.resolve();
    await Promise.resolve();

    expect(useCloudSyncStore.getState()).toMatchObject({ status: "idle", error: null, identity: null });
  });

  it("silently drops a stale push publication after a replacement generation", async () => {
    const pushed = deferred<RemoteMeta>();
    provider.restoreSession.mockResolvedValueOnce(identity).mockResolvedValueOnce(null);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockResolvedValue(meta(1));
    provider.push.mockReturnValue(pushed.promise);
    useCloudSyncStore.setState({ dirty: true, lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));

    cleanup();
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    useCloudSyncStore.setState({ lastSyncedRevision: null, dirty: true });
    pushed.resolve(meta(9));
    await Promise.resolve();
    await Promise.resolve();

    expect(useCloudSyncStore.getState()).toMatchObject({ lastSyncedRevision: null, dirty: true, identity: null });
  });
});

describe("cloud sync serialization", () => {
  it("seeds an empty account with the local revision and reconciliation clock", async () => {
    const local = backup({ decks: { Local: "{}" } });
    await readySignedIn();
    mocks.buildBackup.mockReturnValue(local);
    provider.pullMeta.mockResolvedValue(null);
    provider.push.mockResolvedValue(meta(1));

    await useCloudSyncStore.getState().syncNow();

    expect(provider.push).toHaveBeenCalledWith(local, null);
    expect(provider.pull).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState()).toMatchObject({
      status: "synced",
      dirty: false,
      lastSyncedRevision: 1,
    });
    expect(useCloudSyncStore.getState().lastSyncedAt).not.toBe("remote");
  });

  it("keeps prefs-only first-sign-in data as a conflict", async () => {
    await readySignedIn();
    mocks.buildBackup.mockReturnValue(backup({ preferences: '{"volume":1}' }));
    provider.pullMeta.mockResolvedValue(meta(5));
    provider.pull.mockResolvedValue(remote(5));

    await useCloudSyncStore.getState().syncNow();

    expect(useCloudSyncStore.getState()).toMatchObject({ status: "conflict", conflict: remote(5) });
    expect(mocks.applyBackup).not.toHaveBeenCalled();
    expect(provider.push).not.toHaveBeenCalled();
  });

  it("fast-forwards local changes with metadata CAS and no remote body", async () => {
    const local = backup({ decks: { Local: "{}" } });
    await readySignedIn();
    mocks.buildBackup.mockReturnValue(local);
    useCloudSyncStore.setState({ dirty: true, lastSyncedRevision: 5 });
    provider.pullMeta.mockResolvedValue(meta(5));
    provider.push.mockResolvedValue(meta(6));

    await useCloudSyncStore.getState().syncNow();

    expect(provider.push).toHaveBeenCalledWith(local, 5);
    expect(provider.pull).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState()).toMatchObject({ lastSyncedRevision: 6, dirty: false });
  });

  it("records a metadata-only reconciliation without pulling or pushing", async () => {
    await readySignedIn();
    useCloudSyncStore.setState({ dirty: false, lastSyncedRevision: 5 });
    provider.pullMeta.mockResolvedValue(meta(5));

    await useCloudSyncStore.getState().syncNow();

    expect(provider.pull).not.toHaveBeenCalled();
    expect(provider.push).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState()).toMatchObject({ status: "synced", lastSyncedRevision: 5 });
    expect(useCloudSyncStore.getState().lastSyncedAt).not.toBe("remote");
  });

  it("applies a current remote snapshot with storage suppression and profile notification", async () => {
    const profileReplaced = vi.fn();
    window.addEventListener("phase:profile-replaced", profileReplaced);
    await readySignedIn();
    useCloudSyncStore.setState({ dirty: false, lastSyncedRevision: 1 });
    provider.pullMeta.mockResolvedValue(meta(2));
    provider.pull.mockResolvedValue(remote(2));

    await useCloudSyncStore.getState().syncNow();

    expect(mocks.suppress).toHaveBeenCalled();
    expect(mocks.applyBackup).toHaveBeenCalledWith(remote(2).backup, "overwrite");
    expect(profileReplaced).toHaveBeenCalledTimes(1);
    expect(useCloudSyncStore.getState()).toMatchObject({ lastSyncedRevision: 2, dirty: false });
    window.removeEventListener("phase:profile-replaced", profileReplaced);
  });

  it("reseeds local data when the remote row vanishes between metadata and body pulls", async () => {
    const local = backup({ decks: { Local: "{}" } });
    await readySignedIn();
    mocks.buildBackup.mockReturnValue(local);
    useCloudSyncStore.setState({ dirty: false, lastSyncedRevision: 1 });
    provider.pullMeta.mockResolvedValue(meta(2));
    provider.pull.mockResolvedValue(null);
    provider.push.mockResolvedValue(meta(3));

    await useCloudSyncStore.getState().syncNow();

    expect(provider.push).toHaveBeenCalledWith(local, null);
    expect(useCloudSyncStore.getState()).toMatchObject({ lastSyncedRevision: 3, dirty: false });
  });

  it("does not apply a remote body after a newer local write", async () => {
    const pulled = deferred<RemoteSnapshot | null>();
    await readySignedIn();
    useCloudSyncStore.setState({ dirty: false, lastSyncedRevision: 1 });
    provider.pullMeta.mockResolvedValue(meta(2));
    provider.pull.mockReturnValue(pulled.promise);

    const sync = useCloudSyncStore.getState().syncNow();
    await vi.waitFor(() => expect(provider.pull).toHaveBeenCalledTimes(1));
    watched?.();
    pulled.resolve(remote(2));
    await sync;

    expect(mocks.applyBackup).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState().dirty).toBe(true);
  });

  it("coalesces sync callers into one trailing reconciliation", async () => {
    const firstMeta = deferred<RemoteMeta>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockReturnValueOnce(firstMeta.promise).mockResolvedValue(meta(1));
    useCloudSyncStore.setState({ lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    expect(provider.pullMeta).toHaveBeenCalledTimes(1);

    const second = useCloudSyncStore.getState().syncNow();
    firstMeta.resolve(meta(1));
    await second;

    expect(provider.pullMeta).toHaveBeenCalledTimes(2);
  });

  it("keeps conflict UI until conflict publication succeeds and prevents a duplicate choice push", async () => {
    const pushed = deferred<RemoteMeta>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    const snapshot = remote(3);
    useCloudSyncStore.setState({ conflict: snapshot, status: "conflict" });
    provider.pullMeta.mockResolvedValue(meta(3));
    provider.push.mockReturnValue(pushed.promise);

    const first = useCloudSyncStore.getState().resolveConflict("local");
    const duplicate = useCloudSyncStore.getState().resolveConflict("local");
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));
    expect(useCloudSyncStore.getState().conflict).toBe(snapshot);
    pushed.resolve(meta(4));
    await Promise.all([first, duplicate]);

    expect(provider.push).toHaveBeenCalledTimes(1);
    expect(useCloudSyncStore.getState().conflict).toBeNull();
  });

  it("runs one trailing reconciliation when a newer realtime notification arrives during a cloud choice", async () => {
    const callbacks: Array<(revision: number) => void> = [];
    const pulled = deferred<RemoteSnapshot | null>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockImplementation((callback: (revision: number) => void) => {
      callbacks.push(callback);
      return async () => {};
    });
    useCloudSyncStore.setState({ lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));
    useCloudSyncStore.setState({ conflict: remote(3), status: "conflict" });
    provider.pullMeta.mockClear();
    provider.pullMeta.mockResolvedValueOnce(meta(3)).mockResolvedValueOnce(meta(3));
    provider.pull.mockReturnValueOnce(pulled.promise);

    const choice = useCloudSyncStore.getState().resolveConflict("cloud");
    await vi.waitFor(() => expect(provider.pull).toHaveBeenCalledTimes(1));
    callbacks[0](4);
    const manual = useCloudSyncStore.getState().syncNow();
    pulled.resolve(remote(3));
    await Promise.all([choice, manual]);

    expect(provider.pullMeta).toHaveBeenCalledTimes(2);
    expect(provider.push).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState()).toMatchObject({ status: "synced", conflict: null, lastSyncedRevision: 3 });
  });

  it("keeps a queued conflict-choice trailing sync blocked when cloud revalidation rebuilds conflict", async () => {
    const pulled = deferred<RemoteSnapshot | null>();
    await readySignedIn();
    useCloudSyncStore.setState({ conflict: remote(3), status: "conflict" });
    provider.pullMeta.mockResolvedValue(meta(4));
    provider.pull.mockReturnValue(pulled.promise);

    const choice = useCloudSyncStore.getState().resolveConflict("cloud");
    await vi.waitFor(() => expect(provider.pull).toHaveBeenCalledTimes(1));
    const trailing = useCloudSyncStore.getState().syncNow();
    pulled.resolve(remote(4));
    await Promise.all([choice, trailing]);

    expect(provider.pullMeta).toHaveBeenCalledTimes(1);
    expect(provider.push).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState()).toMatchObject({ status: "conflict", conflict: remote(4) });
  });

  it("applies a successful merge under storage suppression and notifies profile readers", async () => {
    const merged = backup({ decks: { Merged: "{}" } });
    const profileReplaced = vi.fn();
    window.addEventListener("phase:profile-replaced", profileReplaced);
    await readySignedIn();
    useCloudSyncStore.setState({ conflict: remote(3), status: "conflict" });
    provider.pullMeta.mockResolvedValue(meta(3));
    provider.push.mockResolvedValue(meta(4));
    mocks.mergeDeckCollections.mockReturnValue(merged);

    await useCloudSyncStore.getState().resolveConflict("merge");

    expect(mocks.suppress).toHaveBeenCalled();
    expect(mocks.applyBackup).toHaveBeenCalledWith(merged, "overwrite");
    expect(profileReplaced).toHaveBeenCalledTimes(1);
    expect(useCloudSyncStore.getState()).toMatchObject({ status: "synced", lastSyncedRevision: 4 });
    window.removeEventListener("phase:profile-replaced", profileReplaced);
  });

  it("rebuilds a stale merge conflict and blocks queued writes until a fresh choice", async () => {
    const pushed = deferred<RemoteMeta>();
    const merged = backup({ decks: { Merged: "{}" } });
    await readySignedIn();
    useCloudSyncStore.setState({ conflict: remote(3), status: "conflict" });
    provider.pullMeta.mockResolvedValueOnce(meta(3)).mockResolvedValueOnce(meta(4));
    provider.push.mockReturnValueOnce(pushed.promise).mockResolvedValue(meta(5));
    mocks.mergeDeckCollections.mockReturnValue(merged);

    const merge = useCloudSyncStore.getState().resolveConflict("merge");
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));
    watched?.();
    const trailing = useCloudSyncStore.getState().syncNow();
    pushed.resolve(meta(4));
    await Promise.all([merge, trailing]);

    expect(mocks.applyBackup).not.toHaveBeenCalled();
    expect(provider.push).toHaveBeenCalledTimes(1);
    expect(useCloudSyncStore.getState()).toMatchObject({
      status: "conflict",
      dirty: true,
      lastSyncedRevision: 4,
      conflict: { backup: merged, meta: meta(4) },
    });

    await useCloudSyncStore.getState().resolveConflict("local");
    expect(provider.push).toHaveBeenCalledTimes(2);
    expect(provider.push).toHaveBeenNthCalledWith(2, backup(), 4);
  });

  it("resumes a stale-merge conflict without ordinary reconciliation until a fresh choice", async () => {
    const pushed = deferred<RemoteMeta>();
    const merged = backup({ decks: { Merged: "{}" } });
    const callbacks: Array<(revision: number) => void> = [];
    await readySignedIn();
    useCloudSyncStore.setState({ conflict: remote(3), status: "conflict" });
    provider.pullMeta.mockResolvedValueOnce(meta(3)).mockResolvedValue(meta(4));
    provider.push.mockReturnValueOnce(pushed.promise).mockResolvedValue(meta(5));
    provider.subscribe.mockImplementation((callback: (revision: number) => void) => {
      callbacks.push(callback);
      return async () => {};
    });
    mocks.mergeDeckCollections.mockReturnValue(merged);

    const merge = useCloudSyncStore.getState().resolveConflict("merge");
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));
    watched?.();
    pushed.resolve(meta(4));
    await merge;
    const staleConflict = useCloudSyncStore.getState().conflict;
    const staleDiff = useCloudSyncStore.getState().conflictDiff;
    expect(staleConflict).toMatchObject({ backup: merged, meta: meta(4) });

    mocks.effectiveOffline.value = true;
    useCloudSyncStore.getState().pause();
    mocks.effectiveOffline.value = false;
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockClear();
    provider.push.mockClear();
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    expect(provider.pullMeta).not.toHaveBeenCalled();
    expect(provider.push).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState()).toMatchObject({
      sessionResolved: true,
      dirty: true,
      lastSyncedRevision: 4,
      conflict: staleConflict,
      conflictDiff: staleDiff,
    });
    expect(callbacks).toHaveLength(1);

    await useCloudSyncStore.getState().resolveConflict("local");
    expect(provider.push).toHaveBeenCalledWith(backup(), 4);
  });

  it("refreshes a retained conflict when a storage write precedes its choice in the same task", async () => {
    await readySignedIn();
    useCloudSyncStore.setState({ dirty: true, lastSyncedRevision: 1 });
    provider.pullMeta.mockResolvedValue(meta(2));
    provider.pull.mockResolvedValue(remote(2));
    await useCloudSyncStore.getState().syncNow();
    expect(useCloudSyncStore.getState().status).toBe("conflict");
    provider.pullMeta.mockClear();

    watched?.();
    await useCloudSyncStore.getState().resolveConflict("cloud");

    expect(provider.pullMeta).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState()).toMatchObject({ status: "conflict", conflict: remote(2) });
  });

  it("refreshes rather than applies a cloud choice when the remote revision advanced", async () => {
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    mocks.applyBackup.mockClear();
    const stale = remote(3);
    const fresh = remote(4);
    useCloudSyncStore.setState({ conflict: stale, status: "conflict" });
    provider.pullMeta.mockResolvedValue(meta(4));
    provider.pull.mockResolvedValue(fresh);

    await useCloudSyncStore.getState().resolveConflict("cloud");

    expect(mocks.applyBackup).not.toHaveBeenCalled();
    expect(useCloudSyncStore.getState().conflict).toBe(fresh);
  });

  it("retains a retryable conflict when its publish hits a CAS conflict", async () => {
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    mocks.applyBackup.mockClear();
    useCloudSyncStore.setState({ conflict: remote(3), status: "conflict" });
    provider.pullMeta.mockResolvedValue(meta(3));
    provider.push.mockRejectedValue(new SyncConflictError());
    provider.pull.mockResolvedValue(remote(4));

    await useCloudSyncStore.getState().resolveConflict("local");

    expect(useCloudSyncStore.getState()).toMatchObject({ status: "conflict", conflict: remote(4) });
  });

  it("awaits predecessor realtime cleanup before subscribing its replacement", async () => {
    const disposed = deferred<void>();
    const disposer = vi.fn(() => disposed.promise);
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockReturnValue(disposer);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    cleanup();
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await Promise.resolve();
    expect(disposer).toHaveBeenCalledTimes(1);
    expect(provider.subscribe).toHaveBeenCalledTimes(1);

    disposed.resolve();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(2));
  });

  it("releases a successful sign-in gate before reconciling and arming realtime", async () => {
    provider.restoreSession.mockResolvedValue(null);
    provider.identity.mockReturnValue(null);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    provider.signIn.mockImplementation(async () => provider.identity.mockReturnValue(identity));
    provider.pullMeta.mockResolvedValue(meta(1));

    await useCloudSyncStore.getState().signIn("google");

    expect(provider.pullMeta).toHaveBeenCalled();
    expect(provider.subscribe).toHaveBeenCalledTimes(1);
  });

  it.each([
    ["successful", undefined],
    ["rejected", new Error("sign-in failed")],
  ] as const)("replaces stale realtime callbacks after a %s sign-in with a valid identity", async (_outcome, signInError) => {
    const callbacks: Array<(revision: number) => void> = [];
    const unsubscribe = vi.fn(async () => {});
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockResolvedValue(meta(1));
    provider.subscribe.mockImplementation((callback: (revision: number) => void) => {
      callbacks.push(callback);
      return unsubscribe;
    });
    provider.signIn.mockImplementation(async () => {
      if (signInError) throw signInError;
    });
    useCloudSyncStore.setState({ lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    await useCloudSyncStore.getState().signIn("google");

    expect(unsubscribe).toHaveBeenCalledTimes(1);
    expect(provider.subscribe).toHaveBeenCalledTimes(2);
    expect(useCloudSyncStore.getState()).toMatchObject({
      identity,
      ...(signInError ? { status: "error", error: signInError.message } : {}),
    });
    provider.pullMeta.mockClear();

    callbacks[0](2);
    await Promise.resolve();
    expect(provider.pullMeta).not.toHaveBeenCalled();

    callbacks[1](2);
    await vi.waitFor(() => expect(provider.pullMeta).toHaveBeenCalledTimes(1));
  });

  it("coalesces duplicate and opposing auth actions behind the first transition", async () => {
    const signedIn = deferred<void>();
    provider.restoreSession.mockResolvedValue(null);
    provider.identity.mockReturnValue(null);
    provider.signIn.mockReturnValue(signedIn.promise);
    cleanup = useCloudSyncStore.getState().init();
    await settle();

    const first = useCloudSyncStore.getState().signIn("google");
    const duplicate = useCloudSyncStore.getState().signIn("google");
    const opposing = useCloudSyncStore.getState().signOut();
    expect(provider.signIn).toHaveBeenCalledTimes(1);
    expect(provider.signOut).not.toHaveBeenCalled();

    provider.identity.mockReturnValue(identity);
    signedIn.resolve();
    await Promise.all([first, duplicate, opposing]);
    expect(useCloudSyncStore.getState().identity).toEqual(identity);
  });

  it("preserves a rejected sign-out error after recovery with a valid provider identity", async () => {
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockResolvedValue(meta(1));
    useCloudSyncStore.setState({ lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    provider.signOut.mockRejectedValue(new Error("logout failed"));

    await useCloudSyncStore.getState().signOut();

    expect(useCloudSyncStore.getState()).toMatchObject({
      identity,
      status: "error",
      error: "logout failed",
    });
  });

  it("keeps a rejected sign-out error across coalesced recovery reconciliation", async () => {
    const callbacks: Array<(revision: number) => void> = [];
    const recoveryMeta = deferred<RemoteMeta>();
    const trailingMeta = deferred<RemoteMeta>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockImplementation((callback: (revision: number) => void) => {
      callbacks.push(callback);
      return async () => {};
    });
    useCloudSyncStore.setState({ lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));
    provider.pullMeta.mockClear();
    provider.pullMeta.mockReturnValueOnce(recoveryMeta.promise).mockReturnValueOnce(trailingMeta.promise);
    provider.signOut.mockRejectedValue(new Error("logout failed"));
    vi.useFakeTimers();

    const signOut = useCloudSyncStore.getState().signOut();
    await vi.waitFor(() => expect(provider.pullMeta).toHaveBeenCalledTimes(1));
    callbacks[0](2);
    const manual = useCloudSyncStore.getState().syncNow();
    watched?.();
    await Promise.resolve();
    // The storage notification must coalesce into the auth-recovery chain,
    // not manufacture unrelated local data for this preservation assertion.
    useCloudSyncStore.setState({ dirty: false, lastSyncedRevision: 1 });
    await vi.advanceTimersByTimeAsync(3001);
    expect(provider.pullMeta).toHaveBeenCalledTimes(1);

    recoveryMeta.resolve(meta(1));
    await vi.waitFor(() => expect(provider.pullMeta).toHaveBeenCalledTimes(2));
    trailingMeta.resolve(meta(1));
    await Promise.all([signOut, manual]);

    expect(provider.pullMeta).toHaveBeenCalledTimes(2);
    expect(useCloudSyncStore.getState()).toMatchObject({
      identity,
      status: "error",
      error: "logout failed",
    });
  });

  it("replaces the stale realtime callback after a rejected sign-out with a valid identity", async () => {
    const callbacks: Array<(revision: number) => void> = [];
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockResolvedValue(meta(1));
    provider.subscribe.mockImplementation((callback: (revision: number) => void) => {
      callbacks.push(callback);
      return async () => {};
    });
    useCloudSyncStore.setState({ lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));
    provider.signOut.mockRejectedValue(new Error("logout failed"));

    await useCloudSyncStore.getState().signOut();

    expect(provider.subscribe).toHaveBeenCalledTimes(2);
    expect(useCloudSyncStore.getState()).toMatchObject({ status: "error", error: "logout failed" });
    provider.pullMeta.mockClear();
    callbacks[0](2);
    await Promise.resolve();
    expect(provider.pullMeta).not.toHaveBeenCalled();

    callbacks[1](2);
    await vi.waitFor(() => expect(provider.pullMeta).toHaveBeenCalledTimes(1));
  });

  it("clears an accepted local conflict after a newer write, acknowledges revision, and trails once", async () => {
    const pushed = deferred<RemoteMeta>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    const snapshot = remote(3);
    useCloudSyncStore.setState({ conflict: snapshot, status: "conflict" });
    provider.pullMeta.mockResolvedValueOnce(meta(3)).mockResolvedValue(meta(4));
    provider.push.mockReturnValueOnce(pushed.promise).mockResolvedValue(meta(5));

    const choice = useCloudSyncStore.getState().resolveConflict("local");
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));
    watched?.();
    pushed.resolve(meta(4));
    await choice;
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(2));

    expect(useCloudSyncStore.getState()).toMatchObject({
      conflict: null,
      conflictDiff: null,
      dirty: false,
      lastSyncedRevision: 5,
    });
    expect(provider.push).toHaveBeenNthCalledWith(2, backup(), 4);
  });

  it("reseeds through a fresh reconciliation when a cloud-choice row vanishes after metadata", async () => {
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    mocks.applyBackup.mockClear();
    useCloudSyncStore.setState({ conflict: remote(3), status: "conflict" });
    provider.pullMeta.mockResolvedValueOnce(meta(3)).mockResolvedValueOnce(null);
    provider.pull.mockResolvedValue(null);
    provider.push.mockResolvedValue(meta(4));

    await useCloudSyncStore.getState().resolveConflict("cloud");
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));

    expect(mocks.applyBackup).not.toHaveBeenCalled();
    expect(provider.push).toHaveBeenCalledWith(backup(), null);
    expect(useCloudSyncStore.getState()).toMatchObject({ conflict: null, lastSyncedRevision: 4, dirty: false });
  });

  it("releases an advanced conflict when its subsequent body pull finds a vanished row", async () => {
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    useCloudSyncStore.setState({ conflict: remote(3), status: "conflict" });
    provider.pullMeta.mockResolvedValue(meta(4));
    provider.pull.mockResolvedValue(null);

    await useCloudSyncStore.getState().resolveConflict("cloud");

    expect(useCloudSyncStore.getState()).toMatchObject({ conflict: null, conflictDiff: null });
  });

  it("rebuilds a CAS conflict from the current local write and remote body", async () => {
    const remotePull = deferred<RemoteSnapshot | null>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.pullMeta.mockResolvedValue(meta(1));
    provider.push.mockRejectedValue(new SyncConflictError());
    provider.pull.mockReturnValue(remotePull.promise);
    useCloudSyncStore.setState({ dirty: true, lastSyncedRevision: 1 });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.push).toHaveBeenCalledTimes(1));

    watched?.();
    remotePull.resolve(remote(2));
    await Promise.resolve();
    await Promise.resolve();

    expect(provider.push).toHaveBeenCalledTimes(1);
    expect(useCloudSyncStore.getState()).toMatchObject({
      status: "conflict",
      conflict: remote(2),
    });
    expect(useCloudSyncStore.getState().conflictDiff).not.toBeNull();
  });

  it("publishes signed-out state before an async channel disposer settles", async () => {
    const disposed = deferred<void>();
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockReturnValue(() => disposed.promise);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));
    provider.signOut.mockImplementation(async () => provider.identity.mockReturnValue(null));

    const signOut = useCloudSyncStore.getState().signOut();
    await vi.waitFor(() => expect(useCloudSyncStore.getState().identity).toBeNull());
    expect(useCloudSyncStore.getState().status).toBe("idle");

    mocks.effectiveOffline.value = true;
    useCloudSyncStore.getState().pause();
    expect(mocks.pauseProvider).not.toHaveBeenCalled();
    disposed.resolve();
    await signOut;
    await vi.waitFor(() => expect(mocks.pauseProvider).toHaveBeenCalledTimes(1));
  });

  it("joins an existing predecessor cleanup before completing authoritative-null sign-out", async () => {
    const disposed = deferred<void>();
    const disposer = vi.fn(() => disposed.promise);
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockReturnValue(disposer);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    cleanup();
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    provider.signOut.mockImplementation(async () => provider.identity.mockReturnValue(null));
    let settled = false;
    const signOut = useCloudSyncStore.getState().signOut().then(() => { settled = true; });
    await vi.waitFor(() => expect(useCloudSyncStore.getState().identity).toBeNull());

    expect(disposer).toHaveBeenCalledTimes(1);
    expect(settled).toBe(false);
    disposed.resolve();
    await signOut;

    expect(disposer).toHaveBeenCalledTimes(1);
    expect(useCloudSyncStore.getState()).toMatchObject({ identity: null, status: "idle", error: null });
  });

  it("retries and surfaces a late predecessor cleanup rejection during authoritative-null sign-out", async () => {
    const disposed = deferred<void>();
    const disposer = vi.fn(() => disposed.promise);
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockReturnValue(disposer);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    cleanup();
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    provider.signOut.mockImplementation(async () => provider.identity.mockReturnValue(null));
    let settled = false;
    const signOut = useCloudSyncStore.getState().signOut().then(() => { settled = true; });
    await vi.waitFor(() => expect(useCloudSyncStore.getState().identity).toBeNull());

    expect(disposer).toHaveBeenCalledTimes(1);
    expect(settled).toBe(false);
    disposed.reject(new Error("late predecessor cleanup failed"));
    await signOut;

    expect(disposer).toHaveBeenCalledTimes(2);
    expect(useCloudSyncStore.getState()).toMatchObject({
      identity: null,
      status: "error",
      error: "late predecessor cleanup failed",
    });
  });

  it("does not let a stale sign-out retry dispose a fresh sign-in channel", async () => {
    const predecessorCleanup = deferred<void>();
    const signOutRetry = deferred<void>();
    const callbacks: Array<(revision: number) => void> = [];
    const disposer = vi.fn()
      .mockReturnValueOnce(predecessorCleanup.promise)
      .mockReturnValueOnce(signOutRetry.promise);
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockImplementation((callback: (revision: number) => void) => {
      callbacks.push(callback);
      return disposer;
    });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    cleanup();
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    provider.signOut.mockImplementation(async () => provider.identity.mockReturnValue(null));
    const signOut = useCloudSyncStore.getState().signOut();
    await vi.waitFor(() => expect(useCloudSyncStore.getState().identity).toBeNull());
    predecessorCleanup.reject(new Error("predecessor failed"));
    await vi.waitFor(() => expect(disposer).toHaveBeenCalledTimes(2));

    provider.signIn.mockImplementation(async () => provider.identity.mockReturnValue(identity));
    const signIn = useCloudSyncStore.getState().signIn("google");
    signOutRetry.resolve();
    await Promise.all([signOut, signIn]);
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(2));

    expect(disposer).toHaveBeenCalledTimes(2);
    expect(useCloudSyncStore.getState().identity).toEqual(identity);
    provider.pullMeta.mockClear();
    callbacks[1](2);
    await vi.waitFor(() => expect(provider.pullMeta).toHaveBeenCalledTimes(1));
  });

  it("surfaces a rejecting signed-out channel cleanup while retaining signed-out identity", async () => {
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockReturnValue(async () => { throw new Error("channel cleanup failed"); });
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));
    provider.signOut.mockImplementation(async () => provider.identity.mockReturnValue(null));

    await useCloudSyncStore.getState().signOut();

    expect(useCloudSyncStore.getState()).toMatchObject({
      identity: null,
      status: "error",
      error: "channel cleanup failed",
    });
  });

  it("uses the authoritative null identity and keeps a rejected sign-out error", async () => {
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    provider.signOut.mockImplementation(async () => {
      provider.identity.mockReturnValue(null);
      throw new Error("sign-out reported failure");
    });

    await useCloudSyncStore.getState().signOut();

    expect(useCloudSyncStore.getState()).toMatchObject({
      identity: null,
      status: "error",
      error: "sign-out reported failure",
    });
  });

  it("waits for a detached HMR-style cleanup before global pause and clears its late retry", async () => {
    const disposed = deferred<void>();
    const disposer = vi.fn(() => disposed.promise);
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockReturnValue(disposer);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    cleanup();
    mocks.effectiveOffline.value = true;
    useCloudSyncStore.getState().pause();
    await Promise.resolve();
    expect(mocks.pauseProvider).not.toHaveBeenCalled();

    disposed.reject(new Error("late cleanup failure"));
    await vi.waitFor(() => expect(mocks.pauseProvider).toHaveBeenCalledTimes(1));

    mocks.effectiveOffline.value = false;
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(2));
    expect(disposer).toHaveBeenCalledTimes(1);
  });

  it("hands watcher and retained channel cleanup to the HMR successor authority", async () => {
    const disposed = deferred<void>();
    const hmrData: { cloudSyncLifecycle?: unknown } = {};
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockReturnValue(() => disposed.promise);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    disposeCloudSyncModuleForTest(hmrData);
    expect(mocks.unwatch).toHaveBeenCalledTimes(1);
    expect(hmrData.cloudSyncLifecycle).toBeDefined();
    const successorRegistry = adoptCloudSyncHmrState(hmrData);
    expect(successorRegistry).toBe(hmrData.cloudSyncLifecycle);
    let predecessorCleanupComplete = false;
    void successorRegistry.channelCleanupCompletion.then(() => { predecessorCleanupComplete = true; });

    cleanup = useCloudSyncStore.getState().init();
    await settle();
    expect(provider.subscribe).toHaveBeenCalledTimes(1);
    expect(predecessorCleanupComplete).toBe(false);
    disposed.resolve();
    await successorRegistry.channelCleanupCompletion;
    expect(predecessorCleanupComplete).toBe(true);
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(2));
  });

  it("blocks successor subscription on an adopted retained HMR disposer", async () => {
    const hmrData: { cloudSyncLifecycle?: unknown } = {};
    const disposer = vi.fn(async () => { throw new Error("dispose failed"); });
    provider.restoreSession.mockResolvedValue(identity);
    provider.identity.mockReturnValue(identity);
    provider.subscribe.mockReturnValue(disposer);
    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await vi.waitFor(() => expect(provider.subscribe).toHaveBeenCalledTimes(1));

    disposeCloudSyncModuleForTest(hmrData);
    const successorRegistry = adoptCloudSyncHmrState(hmrData);
    await successorRegistry.channelCleanupCompletion;
    expect(successorRegistry.retainedUnsubscribe).toBe(disposer);

    cleanup = useCloudSyncStore.getState().init();
    await settle();
    await Promise.resolve();
    expect(disposer).toHaveBeenCalledTimes(2);
    expect(provider.subscribe).toHaveBeenCalledTimes(1);
  });
});
