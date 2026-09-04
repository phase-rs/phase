import { create } from "zustand";
import { persist } from "zustand/middleware";
import { applyBackup, buildBackup, mergeDeckCollections, type PhaseBackup } from "../services/backup";
import {
  getCloudSyncProvider,
  isCloudSyncConfigured,
  pauseCloudSyncProvider,
  SyncConflictError,
  type CloudSyncProvider,
  type RemoteMeta,
  type RemoteSnapshot,
  type SyncAuthProvider,
  type SyncIdentity,
} from "../services/cloudSync";
import { computeBackupDigest, summarizeBackupDiff, type ConflictDiffSummary } from "../services/cloudSync/backupDiff";
import { watchUserStorage, withStorageWatchSuppressed } from "../services/cloudSync/storageWatcher";
import { getEffectiveOffline } from "./connectivityStore";
import { usePreferencesStore } from "./preferencesStore";

const AUTO_SYNC_DEBOUNCE_MS = 3000;
export const PROFILE_REPLACED_EVENT = "phase:profile-replaced";
export type SyncStatus = "idle" | "syncing" | "synced" | "conflict" | "error";
export type ConflictChoice = "cloud" | "local" | "merge";

interface CloudSyncState {
  available: boolean;
  paused: boolean;
  identity: SyncIdentity | null;
  sessionResolved: boolean;
  status: SyncStatus;
  error: string | null;
  dirty: boolean;
  lastSyncedRevision: number | null;
  lastSyncedAt: string | null;
  conflict: RemoteSnapshot | null;
  conflictDiff: ConflictDiffSummary | null;
  init: () => () => void;
  pause: () => void;
  signIn: (provider: SyncAuthProvider) => Promise<void>;
  signOut: () => Promise<void>;
  syncNow: () => Promise<void>;
  resolveConflict: (choice: ConflictChoice) => Promise<void>;
}

interface Generation {
  version: number;
  provider: CloudSyncProvider;
  cancelled: boolean;
  ready: boolean;
  authVersion: number;
  authTransition: Promise<void> | null;
  timer: ReturnType<typeof setTimeout> | null;
  visibility: (() => void) | null;
  unsubscribe: (() => Promise<void>) | null;
  inFlight: Promise<void> | null;
  trailing: Promise<void> | null;
  trailingQueued: boolean;
  preserveError: boolean;
}

export interface CloudSyncHmrState {
  lifecycleVersion: number;
  channelCleanupCompletion: Promise<void>;
  retainedUnsubscribe: (() => Promise<void>) | null;
}

function createCloudSyncHmrState(): CloudSyncHmrState {
  return {
    lifecycleVersion: 0,
    channelCleanupCompletion: Promise.resolve(),
    retainedUnsubscribe: null,
  };
}

/** Adopts Vite's predecessor registry before this module creates any lifecycle state. */
export function adoptCloudSyncHmrState(data: { cloudSyncLifecycle?: unknown }): CloudSyncHmrState {
  const predecessor = data.cloudSyncLifecycle;
  return predecessor && typeof predecessor === "object"
    ? predecessor as CloudSyncHmrState
    : createCloudSyncHmrState();
}

const cloudSyncHmrState = adoptCloudSyncHmrState(import.meta.hot?.data ?? {});

let lifecycleVersion = cloudSyncHmrState.lifecycleVersion;
let active: Generation | null = null;
let unwatchStorage: (() => void) | null = null;
let localWriteVersion = 0;
let conflictWriteVersion: number | null = null;
/** Completion-aware teardown survives React cleanup, HMR, and replacement init. */

function retainUnsubscribe(disposer: (() => Promise<void>) | null): void {
  cloudSyncHmrState.retainedUnsubscribe = disposer;
}

function retainCleanupCompletion(completion: Promise<void>): Promise<void> {
  cloudSyncHmrState.channelCleanupCompletion = completion;
  return completion;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function hasUserData(backup: PhaseBackup): boolean {
  return Object.keys(backup.decks).length > 0 || backup.preferences !== null ||
    backup.deckMetadata !== null || backup.activeDeck !== null ||
    backup.feedSubscriptions !== null || backup.feedDeckOrigins !== null;
}

function current(generation: Generation, authVersion = generation.authVersion): boolean {
  return active === generation && lifecycleVersion === generation.version && !generation.cancelled &&
    generation.ready && generation.authVersion === authVersion && !getEffectiveOffline() &&
    !useCloudSyncStore.getState().paused;
}

function actionGeneration(): Generation | null {
  const generation = active;
  return generation && current(generation) && !generation.authTransition ? generation : null;
}

function ensureWatcher(): void {
  if (unwatchStorage) return;
  unwatchStorage = watchUserStorage(() => {
    // Advance before scheduling Zustand work: same-task conflict selections
    // cannot publish an old local snapshot after a storage write.
    localWriteVersion += 1;
    queueMicrotask(() => {
      useCloudSyncStore.setState({ dirty: true });
      const generation = actionGeneration();
      if (!generation || !useCloudSyncStore.getState().identity) return;
      if (generation.timer) clearTimeout(generation.timer);
      generation.timer = setTimeout(() => {
        generation.timer = null;
        if (current(generation) && !generation.authTransition) void useCloudSyncStore.getState().syncNow();
      }, AUTO_SYNC_DEBOUNCE_MS);
    });
  });
}

function stopSyncResources(generation: Generation): void {
  generation.cancelled = true;
  if (generation.timer) clearTimeout(generation.timer);
  generation.timer = null;
  if (generation.visibility) document.removeEventListener("visibilitychange", generation.visibility);
  generation.visibility = null;
}

async function disposeChannel(generation: Generation): Promise<void> {
  const disposer = generation.unsubscribe ?? cloudSyncHmrState.retainedUnsubscribe;
  generation.unsubscribe = null;
  if (!disposer) return;
  try {
    await disposer();
    if (cloudSyncHmrState.retainedUnsubscribe === disposer) retainUnsubscribe(null);
  } catch (error) {
    retainUnsubscribe(disposer);
    throw error;
  }
}

function invalidate(generation: Generation | null): Promise<void> {
  if (!generation) return Promise.resolve();
  stopSyncResources(generation);
  return disposeChannel(generation);
}

/**
 * Observe every asynchronous disposer immediately, while retaining one module
 * completion boundary that successors await before subscribing.
 */
function observeCleanup(cleanup: Promise<void>): Promise<void> {
  const settled = cleanup.catch(() => undefined);
  retainCleanupCompletion(cloudSyncHmrState.channelCleanupCompletion.then(() => settled));
  return cleanup;
}

function trackCleanup(generation: Generation | null): Promise<void> {
  return observeCleanup(invalidate(generation));
}

function trackChannelCleanup(generation: Generation): Promise<void> {
  return observeCleanup(disposeChannel(generation));
}

function queueChannelCleanup(operation: () => Promise<void>): Promise<void> {
  const queued = cloudSyncHmrState.channelCleanupCompletion.then(operation);
  retainCleanupCompletion(queued.catch(() => undefined));
  return queued;
}

function retryRetainedChannelCleanup(): Promise<void> {
  return queueChannelCleanup(async () => {
    if (!cloudSyncHmrState.retainedUnsubscribe) return;
    const disposer = cloudSyncHmrState.retainedUnsubscribe;
    try {
      await disposer();
      if (cloudSyncHmrState.retainedUnsubscribe === disposer) retainUnsubscribe(null);
    } catch (error) {
      retainUnsubscribe(disposer);
      throw error;
    }
  });
}

async function finishSignOutChannelCleanup(generation: Generation, auth: number): Promise<void> {
  // A predecessor/HMR disposal clears its generation unsubscribe before its
  // promise settles. Join that authoritative completion before considering a
  // retry or this generation's own channel, so sign-out never double-disposes
  // the same channel and still surfaces late retained-disposer failures.
  await cloudSyncHmrState.channelCleanupCompletion;
  if (!current(generation, auth)) return;
  await retryRetainedChannelCleanup();
  if (!current(generation, auth)) return;
  if (generation.unsubscribe) {
    await trackChannelCleanup(generation);
    if (!current(generation, auth)) return;
  }
}

function requestTrailing(generation: Generation): void {
  // This trailing reconciliation already carries the newest acknowledged
  // revision. A pending debounce from the write it subsumes would otherwise
  // wake later and redundantly reread metadata during the trailing operation.
  if (generation.timer) clearTimeout(generation.timer);
  generation.timer = null;
  if (generation.trailingQueued) return;
  generation.trailingQueued = true;
  queueMicrotask(() => {
    generation.trailingQueued = false;
    if (current(generation) && !generation.authTransition && !useCloudSyncStore.getState().conflict) {
      void useCloudSyncStore.getState().syncNow();
    }
  });
}

function publishConflict(generation: Generation, auth: number, write: number, local: PhaseBackup, remote: RemoteSnapshot): boolean {
  if (!current(generation, auth) || localWriteVersion !== write) return false;
  conflictWriteVersion = write;
  useCloudSyncStore.setState({ status: "conflict", error: null, conflict: remote, conflictDiff: summarizeBackupDiff(local, remote.backup) });
  return true;
}

function applyRemote(generation: Generation, auth: number, write: number, remote: RemoteSnapshot): boolean {
  if (!current(generation, auth) || localWriteVersion !== write) return false;
  withStorageWatchSuppressed(() => applyBackup(remote.backup, "overwrite"));
  conflictWriteVersion = null;
  useCloudSyncStore.setState({ status: "synced", error: null, dirty: false, conflict: null, conflictDiff: null, lastSyncedRevision: remote.meta.revision, lastSyncedAt: new Date().toISOString() });
  void usePreferencesStore.persist.rehydrate();
  window.dispatchEvent(new CustomEvent(PROFILE_REPLACED_EVENT));
  return true;
}

function applyMerged(backup: PhaseBackup): void {
  withStorageWatchSuppressed(() => applyBackup(backup, "overwrite"));
  void usePreferencesStore.persist.rehydrate();
  window.dispatchEvent(new CustomEvent(PROFILE_REPLACED_EVENT));
}

function acknowledgePush(generation: Generation, auth: number, write: number, meta: RemoteMeta, mergeSnapshot: RemoteSnapshot | null = null): void {
  if (!current(generation, auth)) return;
  const state: Partial<CloudSyncState> = { lastSyncedRevision: meta.revision, lastSyncedAt: new Date().toISOString() };
  if (mergeSnapshot) {
    const local = buildBackup();
    conflictWriteVersion = localWriteVersion;
    Object.assign(state, { status: "conflict", dirty: true, conflict: mergeSnapshot, conflictDiff: summarizeBackupDiff(local, mergeSnapshot.backup) });
  } else if (localWriteVersion !== write) {
    conflictWriteVersion = null;
    Object.assign(state, { status: "synced", dirty: true, conflict: null, conflictDiff: null });
    requestTrailing(generation);
  } else {
    conflictWriteVersion = null;
    Object.assign(state, { status: "synced", error: null, dirty: false, conflict: null, conflictDiff: null });
  }
  useCloudSyncStore.setState(state);
}

function restorePreservedAuthError(generation: Generation, auth: number, write: number, preserveError: boolean, error: string | null): void {
  if (preserveError && error && current(generation, auth) && localWriteVersion === write && !useCloudSyncStore.getState().conflict) {
    useCloudSyncStore.setState({ status: "error", error });
  }
}

type PullConflictResult = "published" | "vanished" | "stale";

async function pullConflict(generation: Generation, auth: number): Promise<PullConflictResult> {
  const remote = await generation.provider.pull();
  if (!current(generation, auth)) return "stale";
  if (!remote) return "vanished";
  // Re-capture after the await. A newer local write requires a new truthful
  // conflict/diff, not the pre-CAS snapshot and never a false reseed.
  const currentWrite = localWriteVersion;
  return publishConflict(generation, auth, currentWrite, buildBackup(), remote) ? "published" : "stale";
}

async function reconcile(generation: Generation, preserveError = false): Promise<void> {
  const auth = generation.authVersion;
  if (!current(generation, auth) || generation.authTransition) return;
  const identity = generation.provider.identity();
  if (!identity) return;
  const write = localWriteVersion;
  const local = buildBackup();
  const oldError = useCloudSyncStore.getState().error;
  useCloudSyncStore.setState({ status: "syncing", identity, ...(preserveError ? {} : { error: null }) });
  try {
    const meta = await generation.provider.pullMeta();
    if (!current(generation, auth)) return;
    const { lastSyncedRevision, dirty } = useCloudSyncStore.getState();
    if (!meta) {
      const pushed = await generation.provider.push(local, null);
      acknowledgePush(generation, auth, write, pushed);
      restorePreservedAuthError(generation, auth, write, preserveError, oldError);
      return;
    }
    const remoteAhead = meta.revision !== lastSyncedRevision;
    const localChanged = dirty || (lastSyncedRevision === null && hasUserData(local));
    if (remoteAhead && localChanged) {
      const remote = await generation.provider.pull();
      if (!current(generation, auth)) return;
      if (!remote) {
        const pushed = await generation.provider.push(local, null);
        acknowledgePush(generation, auth, write, pushed);
        restorePreservedAuthError(generation, auth, write, preserveError, oldError);
        return;
      }
      const [localDigest, remoteDigest] = await Promise.all([computeBackupDigest(local), computeBackupDigest(remote.backup)]);
      if (!current(generation, auth) || localWriteVersion !== write) return;
      if (localDigest === remoteDigest) {
        conflictWriteVersion = null;
        useCloudSyncStore.setState({ status: preserveError && oldError ? "error" : "synced", error: preserveError ? oldError : null, dirty: false, conflict: null, conflictDiff: null, lastSyncedRevision: remote.meta.revision, lastSyncedAt: new Date().toISOString() });
      } else {
        publishConflict(generation, auth, write, local, remote);
      }
      return;
    }
    if (remoteAhead) {
      const remote = await generation.provider.pull();
      if (!current(generation, auth)) return;
      if (remote) {
        applyRemote(generation, auth, write, remote);
        restorePreservedAuthError(generation, auth, write, preserveError, oldError);
      }
      else {
        const pushed = await generation.provider.push(local, null);
        acknowledgePush(generation, auth, write, pushed);
        restorePreservedAuthError(generation, auth, write, preserveError, oldError);
      }
      return;
    }
    if (localChanged) {
      const pushed = await generation.provider.push(local, meta.revision);
      acknowledgePush(generation, auth, write, pushed);
      restorePreservedAuthError(generation, auth, write, preserveError, oldError);
      return;
    }
    if (current(generation, auth) && localWriteVersion === write) {
      useCloudSyncStore.setState({ status: preserveError && oldError ? "error" : "synced", error: preserveError ? oldError : null, lastSyncedAt: new Date().toISOString() });
    }
  } catch (error) {
    if (!current(generation, auth)) return;
    if (error instanceof SyncConflictError) {
      try {
        const result = await pullConflict(generation, auth);
        if (result !== "vanished") return;
        if (!current(generation, auth)) return;
        const pushed = await generation.provider.push(local, null);
        acknowledgePush(generation, auth, write, pushed);
        restorePreservedAuthError(generation, auth, write, preserveError, oldError);
      } catch (reseedError) {
        if (current(generation, auth)) useCloudSyncStore.setState({ status: "error", error: message(reseedError) });
      }
    } else {
      useCloudSyncStore.setState({ status: "error", error: message(error) });
    }
  }
}

function startReconcile(generation: Generation, preserveError = false): Promise<void> {
  if (generation.inFlight) {
    generation.preserveError ||= preserveError;
    if (!generation.trailing) {
      const base = generation.inFlight;
      const trailing = base.then(() => {
        // The completion continuation may run before the generic finally that
        // clears this field. Clear this generation-owned slot first so the
        // trailing request is a real reconciliation, not a self-reference.
        if (generation.inFlight === base) generation.inFlight = null;
        if (generation.trailing === trailing) generation.trailing = null;
        if (!current(generation) || generation.authTransition || useCloudSyncStore.getState().conflict) return;
        return startReconcile(generation, generation.preserveError);
      });
      generation.trailing = trailing;
      void trailing.finally(() => {
        if (generation.trailing === trailing) generation.trailing = null;
      });
    }
    return generation.trailing;
  }
  // Conflicts are authoritative until a new explicit choice. Every ordinary
  // caller (startup, manual, watcher, realtime, or trailing) shares this gate.
  if (useCloudSyncStore.getState().conflict) return Promise.resolve();
  generation.preserveError = preserveError;
  const task = reconcile(generation, generation.preserveError);
  generation.inFlight = task;
  void task.finally(() => {
    if (generation.inFlight === task) generation.inFlight = null;
    if (!generation.inFlight && !generation.trailing) generation.preserveError = false;
  });
  return task;
}

async function armRealtime(
  generation: Generation,
  predecessorCleanup: Promise<void>,
  auth = generation.authVersion,
): Promise<void> {
  try { await predecessorCleanup; } catch { /* retry the retained disposer below */ }
  if (!current(generation, auth) || generation.authTransition || !useCloudSyncStore.getState().identity || generation.unsubscribe) return;
  try { await retryRetainedChannelCleanup(); } catch (error) {
    if (current(generation, auth)) useCloudSyncStore.setState({ status: "error", error: message(error) });
    return;
  }
  if (!current(generation, auth) || generation.authTransition || !useCloudSyncStore.getState().identity || generation.unsubscribe) return;
  generation.unsubscribe = generation.provider.subscribe((revision) => {
    if (current(generation, auth) && !generation.authTransition && revision !== useCloudSyncStore.getState().lastSyncedRevision) {
      void useCloudSyncStore.getState().syncNow();
    }
  });
}

async function replaceStaleChannelAndReconcile(
  generation: Generation,
  auth: number,
  preserveError: boolean,
): Promise<void> {
  // An auth transition advances authVersion before the provider call, so any
  // existing callback is intentionally stale. It must not remain installed:
  // armRealtime correctly refuses to stack a second subscription.
  if (generation.unsubscribe) {
    try { await trackChannelCleanup(generation); } catch { /* armRealtime retries retained cleanup */ }
  }
  if (!current(generation, auth)) return;
  await startReconcile(generation, preserveError);
  if (current(generation, auth)) {
    await armRealtime(generation, cloudSyncHmrState.channelCleanupCompletion, auth);
  }
}

export const useCloudSyncStore = create<CloudSyncState>()(persist((set, get) => ({
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

  pause: () => {
    const version = ++lifecycleVersion;
    const predecessor = active;
    active = null;
    trackCleanup(predecessor);
    // This is intentionally read after tracking the local generation. It is
    // the module/HMR-owned completion boundary, so an already-detached older
    // disposer still settles before the provider receives its global pause.
    const cleanup = cloudSyncHmrState.channelCleanupCompletion;
    set({ paused: true, available: isCloudSyncConfigured(), sessionResolved: true });
    ensureWatcher();
    void cleanup.finally(async () => {
      if (lifecycleVersion !== version || !getEffectiveOffline()) return;
      try {
        await pauseCloudSyncProvider();
        if (lifecycleVersion === version && getEffectiveOffline()) retainUnsubscribe(null);
      } catch { /* preserve existing state; online init retries the channel disposer */ }
    });
  },

  init: () => {
    if (getEffectiveOffline()) { get().pause(); return () => {}; }
    const version = ++lifecycleVersion;
    const predecessor = active;
    active = null;
    trackCleanup(predecessor);
    const predecessorCleanup = cloudSyncHmrState.channelCleanupCompletion;
    set({ paused: false, available: isCloudSyncConfigured(), sessionResolved: false });
    ensureWatcher();
    if (!isCloudSyncConfigured()) { set({ sessionResolved: true }); return () => {}; }
    const provider = getCloudSyncProvider();
    if (!provider) { set({ available: false, sessionResolved: true }); return () => {}; }
    const generation: Generation = { version, provider, cancelled: false, ready: false, authVersion: 0, authTransition: null, timer: null, visibility: null, unsubscribe: null, inFlight: null, trailing: null, trailingQueued: false, preserveError: false };
    active = generation;
    generation.visibility = () => {
      if (!current(generation) || generation.authTransition || !get().identity) return;
      if (document.visibilityState !== "hidden" || get().dirty) void get().syncNow();
    };
    document.addEventListener("visibilitychange", generation.visibility);
    void (async () => {
      try {
        await provider.resume();
        if (active !== generation || lifecycleVersion !== version || getEffectiveOffline()) return;
        const identity = await provider.restoreSession();
        if (active !== generation || lifecycleVersion !== version || getEffectiveOffline()) return;
        generation.ready = true;
        set({ identity, sessionResolved: true });
        if (!identity) return;
        await startReconcile(generation);
        if (current(generation)) await armRealtime(generation, predecessorCleanup);
      } catch (error) {
        if (active !== generation || lifecycleVersion !== version || getEffectiveOffline()) return;
        generation.ready = true;
        set({ identity: null, sessionResolved: true, status: "error", error: message(error) });
      }
    })();
    return () => {
      if (active !== generation || lifecycleVersion !== version) return;
      lifecycleVersion += 1;
      active = null;
      void trackCleanup(generation);
    };
  },

  signIn: async (authProvider) => {
    const generation = actionGeneration();
    if (!generation) return;
    if (generation.authTransition) return generation.authTransition;
    const auth = ++generation.authVersion;
    generation.preserveError = false;
    const operation = (async () => {
      let signInError: unknown = null;
      try {
        set({ error: null });
        await generation.provider.signIn(authProvider);
      } catch (error) {
        signInError = error;
      }
      if (!current(generation, auth)) return;
      const identity = generation.provider.identity();
      const error = signInError ? message(signInError) : null;
      set({ identity, status: error ? "error" : "idle", error });
      // Release the shared auth gate before follow-up work; reconciliation
      // itself correctly rejects a transition that supersedes this one.
      generation.authTransition = null;
      if (!identity) return;
      await replaceStaleChannelAndReconcile(generation, auth, signInError !== null);
      if (error && current(generation, auth)) set({ status: "error", error });
    })().finally(() => { if (current(generation, auth)) generation.authTransition = null; });
    generation.authTransition = operation;
    return operation;
  },

  signOut: async () => {
    const generation = actionGeneration();
    if (!generation) return;
    if (generation.authTransition) return generation.authTransition;
    const auth = ++generation.authVersion;
    generation.preserveError = false;
    const operation = (async () => {
      let authError: unknown = null;
      try { await generation.provider.signOut(); } catch (error) { authError = error; }
      if (!current(generation, auth)) return;
      const identity = generation.provider.identity();
      if (!identity) {
        // Signed-out identity and the released auth gate are authoritative as
        // soon as the provider says so. Channel teardown is still awaited for
        // this call, but it must not hold subsequent lifecycle transitions.
        const signedOutError = authError ? message(authError) : null;
        set({ identity: null, status: signedOutError ? "error" : "idle", error: signedOutError });
        generation.authTransition = null;
        let cleanupError: unknown = null;
        try { await finishSignOutChannelCleanup(generation, auth); } catch (error) { cleanupError = error; }
        if (!current(generation, auth)) return;
        if (cleanupError && !signedOutError) {
          set({ status: "error", error: message(cleanupError) });
        }
        return;
      }
      // A rejected sign-out may still have changed SDK auth state. Preserve the
      // observable error and recover exactly once from the provider's identity.
      const recoveryError = authError ? message(authError) : "Sign out did not clear the session";
      set({ identity, status: "error", error: recoveryError });
      generation.authTransition = null;
      await replaceStaleChannelAndReconcile(generation, auth, true);
      if (current(generation, auth)) set({ status: "error", error: recoveryError });
    })().finally(() => { if (current(generation, auth)) generation.authTransition = null; });
    generation.authTransition = operation;
    return operation;
  },

  syncNow: async () => {
    const generation = actionGeneration();
    if (!generation) return;
    // A conflict choice owns the same reconciliation mutex. Queue its one
    // trailing pass before the authoritative-conflict gate so a successful
    // choice can continue syncing, while a rebuilt/failed choice keeps that
    // pass blocked when it actually executes.
    if (generation.inFlight) return startReconcile(generation);
    if (get().conflict) return;
    return startReconcile(generation);
  },

  resolveConflict: async (choice) => {
    const generation = actionGeneration();
    const conflict = get().conflict;
    if (!generation || !conflict) return;
    if (generation.inFlight) return generation.inFlight;
    const auth = generation.authVersion;
    const local = buildBackup();
    const write = localWriteVersion;
    // A conflict normally originates from publishConflict, which records the
    // write version. Treat a freshly restored/injected conflict equivalently
    // on its first choice instead of silently dropping the selection.
    if (conflictWriteVersion === null) conflictWriteVersion = write;
    if (conflictWriteVersion !== write) { publishConflict(generation, auth, write, local, conflict); return; }
    const task = (async () => {
      try {
        const meta = await generation.provider.pullMeta();
        if (!current(generation, auth)) return;
        if (!meta) {
          conflictWriteVersion = null;
          set({ conflict: null, conflictDiff: null, status: "idle" });
          requestTrailing(generation);
          return;
        }
        if (meta.revision !== conflict.meta.revision) {
          const remote = await generation.provider.pull();
          if (!current(generation, auth)) return;
          if (remote) publishConflict(generation, auth, localWriteVersion, buildBackup(), remote);
          else {
            conflictWriteVersion = null;
            set({ conflict: null, conflictDiff: null, status: "idle" });
            requestTrailing(generation);
          }
          return;
        }
        if (choice === "cloud") {
          // The row can disappear after metadata revalidation. Pull the body
          // before replacing local storage; null releases into a fresh seed.
          const remote = await generation.provider.pull();
          if (!current(generation, auth)) return;
          if (!remote) {
            conflictWriteVersion = null;
            set({ conflict: null, conflictDiff: null, status: "idle" });
            requestTrailing(generation);
            return;
          }
          if (localWriteVersion !== write) {
            publishConflict(generation, auth, localWriteVersion, buildBackup(), remote);
            return;
          }
          if (remote.meta.revision !== conflict.meta.revision) {
            publishConflict(generation, auth, write, local, remote);
            return;
          }
          applyRemote(generation, auth, write, remote);
          return;
        }
        const next = choice === "merge" ? mergeDeckCollections(local, conflict.backup) : local;
        set({ status: "syncing" }); // retain conflict/diff while publication is pending
        const pushed = await generation.provider.push(next, conflict.meta.revision);
        if (!current(generation, auth)) return;
        const staleMerge = choice === "merge" && localWriteVersion !== write ? { backup: next, meta: pushed } : null;
        if (choice === "merge" && !staleMerge && localWriteVersion === write) applyMerged(next);
        acknowledgePush(generation, auth, write, pushed, staleMerge);
      } catch (error) {
        if (!current(generation, auth)) return;
        if (error instanceof SyncConflictError) {
          try {
            const result = await pullConflict(generation, auth);
            if (result !== "vanished") return;
            if (current(generation, auth)) { set({ conflict: null, conflictDiff: null, status: "idle" }); requestTrailing(generation); }
          } catch (refreshError) {
            if (current(generation, auth)) set({ status: "error", error: message(refreshError) });
          }
        } else {
          set({ status: "error", error: message(error) });
        }
      }
    })();
    generation.inFlight = task;
    try { await task; } finally { if (generation.inFlight === task) generation.inFlight = null; }
  },
}), {
  name: "phase-cloud-sync",
  partialize: (state) => ({ dirty: state.dirty, lastSyncedRevision: state.lastSyncedRevision, lastSyncedAt: state.lastSyncedAt }),
}));

function disposeCloudSyncModule(data: { cloudSyncLifecycle?: CloudSyncHmrState }): void {
  lifecycleVersion += 1;
  cloudSyncHmrState.lifecycleVersion = lifecycleVersion;
  unwatchStorage?.();
  unwatchStorage = null;
  const generation = active;
  active = null;
  // Store the mutable registry object, not a snapshot: an async disposer can
  // still fail after this callback and must hand its retained retry state to
  // the replacement module.
  data.cloudSyncLifecycle = cloudSyncHmrState;
  void trackCleanup(generation);
}

/** Test seam for the exact HMR disposal authority used by Vite. */
export function disposeCloudSyncModuleForTest(data: { cloudSyncLifecycle?: unknown }): void {
  disposeCloudSyncModule(data as { cloudSyncLifecycle?: CloudSyncHmrState });
}

if (import.meta.hot) {
  import.meta.hot.dispose(disposeCloudSyncModule);
}
