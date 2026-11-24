import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PhaseBackup } from "../../services/backup";
import type { CloudSyncProvider, RemoteMeta, RemoteSnapshot } from "../../services/cloudSync";

// The persisted sync markers are tagged with the Supabase project they
// describe. These cases load the real store and the real project resolver
// fresh over a seeded `phase-cloud-sync` record, so hydration runs exactly as
// it does on a page load.

const mocks = vi.hoisted(() => ({
  buildBackup: vi.fn(),
  getProvider: vi.fn(),
  configured: vi.fn(),
  pauseProvider: vi.fn(),
  watch: vi.fn(),
  effectiveOffline: { value: false },
}));

vi.mock("../../services/backup", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/backup")>();
  return {
    ...actual,
    buildBackup: mocks.buildBackup,
    buildCloudBackup: () => actual.projectCloudBackup(mocks.buildBackup()),
  };
});
vi.mock("../../services/cloudSync", () => ({
  getCloudSyncProvider: mocks.getProvider,
  isCloudSyncConfigured: mocks.configured,
  pauseCloudSyncProvider: mocks.pauseProvider,
  SyncConflictError: class SyncConflictError extends Error {},
}));
vi.mock("../../services/cloudSync/storageWatcher", () => ({
  watchUserStorage: mocks.watch,
  withStorageWatchSuppressed: (callback: () => void) => callback(),
}));
vi.mock("../connectivityStore", () => ({
  getEffectiveOffline: () => mocks.effectiveOffline.value,
}));

const STORAGE_KEY = "phase-cloud-sync";
const PROJECT_A = "https://project-a.supabase.co";
const PROJECT_B = "https://project-b.supabase.co";
const REVISION = 5;
const identity = { userId: "user-1", label: "Tester" };

function backup(decks: Record<string, string>): PhaseBackup {
  return {
    version: 1,
    exportedAt: "2026-01-01T00:00:00.000Z",
    preferences: null,
    decks,
    deckMetadata: null,
    activeDeck: null,
    feedSubscriptions: null,
    feedDeckOrigins: null,
  };
}

function meta(revision: number): RemoteMeta {
  return { revision, updatedAt: "remote" };
}

/** What project B holds: a different backup at the same small revision. */
const projectBBackup: RemoteSnapshot = {
  backup: backup({ "Project B deck": "{}" }),
  meta: meta(REVISION),
};

let provider: Record<string, ReturnType<typeof vi.fn>>;
let watched: (() => void) | undefined;
let store: typeof import("../cloudSyncStore").useCloudSyncStore;
let cleanup: (() => void) | undefined;

function seedMarkers(record: { state: Record<string, unknown>; version: number }): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(record));
}

/** Synced with project A at REVISION, nothing pending: the pre-switch state. */
const syncedWithProjectA = {
  dirty: false,
  lastSyncedRevision: REVISION,
  lastSyncedDigest: "project-a-digest",
  lastSyncedAt: "2026-01-01T00:00:00.000Z",
};

/** Loads the store as a page would, with this build and this `/config.js`. */
async function loadStore(runtimeConfig: Record<string, string> | undefined): Promise<void> {
  vi.resetModules();
  vi.stubGlobal("__SUPABASE_URL__", PROJECT_A);
  vi.stubGlobal("__SUPABASE_ANON_KEY__", "sb_publishable_a");
  if (runtimeConfig) {
    (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__ = runtimeConfig;
  }
  ({ useCloudSyncStore: store } = await import("../cloudSyncStore"));
}

const projectBConfig = { supabaseUrl: PROJECT_B, supabaseAnonKey: "sb_publishable_b" };

async function signedInStart(): Promise<void> {
  cleanup = store.getState().init();
  await vi.waitFor(() => expect(store.getState().sessionResolved).toBe(true));
  await vi.waitFor(() => expect(store.getState().status).not.toBe("syncing"));
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  mocks.effectiveOffline.value = false;
  mocks.configured.mockReturnValue(true);
  mocks.pauseProvider.mockResolvedValue(undefined);
  mocks.watch.mockImplementation((callback: () => void) => {
    watched = callback;
    return () => {};
  });
  provider = {
    resume: vi.fn().mockResolvedValue(undefined),
    restoreSession: vi.fn().mockResolvedValue(identity),
    identity: vi.fn(() => identity),
    signIn: vi.fn().mockResolvedValue(undefined),
    signOut: vi.fn().mockResolvedValue(undefined),
    pullMeta: vi.fn().mockResolvedValue(meta(REVISION)),
    pull: vi.fn().mockResolvedValue(projectBBackup),
    push: vi.fn().mockResolvedValue(meta(REVISION + 1)),
    subscribe: vi.fn(() => async () => {}),
  };
  mocks.getProvider.mockReturnValue(provider as unknown as CloudSyncProvider);
  mocks.buildBackup.mockReturnValue(backup({ "Project A deck": "{}" }));
});

afterEach(async () => {
  cleanup?.();
  cleanup = undefined;
  mocks.effectiveOffline.value = true;
  store.getState().pause();
  await vi.waitFor(() => expect(mocks.pauseProvider).toHaveBeenCalled());
  delete (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__;
  vi.unstubAllGlobals();
  localStorage.clear();
});

describe("cloud sync markers are scoped to their Supabase project", () => {
  it.each([
    ["a pre-tagging record (written against the build-time project)", { state: syncedWithProjectA, version: 0 }],
    ["a record tagged with another project", { state: { ...syncedWithProjectA, project: PROJECT_A }, version: 1 }],
  ])("reads the new project's backup and raises a conflict for %s", async (_label, record) => {
    seedMarkers(record);
    await loadStore(projectBConfig);

    // Project A's markers describe nothing about project B.
    expect(store.getState()).toMatchObject({ dirty: false, lastSyncedRevision: null, lastSyncedDigest: null });

    await signedInStart();

    expect(provider.pull).toHaveBeenCalled();
    expect(store.getState().status).toBe("conflict");
    expect(store.getState().conflict).toEqual(projectBBackup);

    // The next local edit must not push project A's data over project B's
    // backup: the conflict holds until the player settles it.
    watched?.();
    await store.getState().syncNow();
    expect(provider.push).not.toHaveBeenCalled();
  });

  it("keeps the markers when the project is unchanged", async () => {
    seedMarkers({ state: syncedWithProjectA, version: 0 });
    await loadStore(undefined);

    expect(store.getState()).toMatchObject(syncedWithProjectA);

    await signedInStart();

    // Same project, same revision, nothing pending: no pull, no prompt.
    expect(provider.pull).not.toHaveBeenCalled();
    expect(store.getState().status).toBe("synced");
    expect(store.getState().conflict).toBeNull();
  });

  it("tags what it persists with the project this page syncs with", async () => {
    seedMarkers({ state: syncedWithProjectA, version: 0 });
    await loadStore(projectBConfig);

    store.setState({ lastSyncedRevision: 9 });

    const persisted = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as {
      state: Record<string, unknown>;
      version: number;
    };
    expect(persisted.version).toBe(1);
    expect(persisted.state).toMatchObject({ project: PROJECT_B, lastSyncedRevision: 9 });
  });
});
