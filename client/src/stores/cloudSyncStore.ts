import { create } from "zustand";
import { persist } from "zustand/middleware";
import { applyBackup, buildBackup, type PhaseBackup } from "../services/backup";
import {
  getCloudSyncProvider,
  SyncConflictError,
  type RemoteSnapshot,
  type SyncAuthProvider,
  type SyncIdentity,
} from "../services/cloudSync";
import {
  watchUserStorage,
  withStorageWatchSuppressed,
} from "../services/cloudSync/storageWatcher";
import {
  computeBackupDigest,
  summarizeBackupDiff,
  type ConflictDiffSummary,
} from "../services/cloudSync/backupDiff";
import { usePreferencesStore } from "./preferencesStore";

/** Debounce window for auto-sync after a user-owned storage change. */
const AUTO_SYNC_DEBOUNCE_MS = 3000;

/**
 * Broadcast when the local profile has been replaced wholesale by a remote
 * snapshot. Components reading non-Zustand localStorage data directly (deck
 * lists, feed subscriptions, metadata) subscribe to this event and re-read.
 * Zustand-persisted stores rehydrate via their own `persist.rehydrate()` API.
 */
export const PROFILE_REPLACED_EVENT = "phase:profile-replaced";

export type SyncStatus = "idle" | "syncing" | "synced" | "conflict" | "error";
export type ConflictChoice = "cloud" | "local";

interface CloudSyncState {
  /** True when a provider is configured for this deployment. */
  available: boolean;
  identity: SyncIdentity | null;
  /**
   * False until `restoreSession()` has resolved on boot. Distinguishes
   * "we haven't checked yet" (transient) from "no session" (confirmed
   * signed-out). UI must NOT render the sign-in CTA while this is false —
   * otherwise the icon flashes "Sign in" between mount and session restore
   * even though the user is signed in, because `identity` starts null and
   * doesn't populate until the async restore completes.
   */
  sessionResolved: boolean;
  status: SyncStatus;
  error: string | null;
  /** Local profile has changes not yet pushed. Persisted so a tab close
   *  mid-debounce doesn't strand the changes — next boot still pushes them. */
  dirty: boolean;
  /** Revision this device last reconciled with. null = never synced here. */
  lastSyncedRevision: number | null;
  /**
   * Local clock at the moment this device last reconciled with cloud — in
   * either direction (push, pull, or fast-forward on digest equality). Not the
   * cloud's `updatedAt`, which would be stale after a pull from data written
   * by another device. Answers "when did this device last sync?" honestly.
   */
  lastSyncedAt: string | null;
  /** Pending remote snapshot awaiting a user keep-cloud/keep-local decision. */
  conflict: RemoteSnapshot | null;
  /** Per-envelope-section diff summary for the current conflict, or null. */
  conflictDiff: ConflictDiffSummary | null;

  /** Installs the storage watcher + session restore; returns an uninstaller. */
  init: () => () => void;
  signIn: (provider: SyncAuthProvider) => Promise<void>;
  signOut: () => Promise<void>;
  syncNow: () => Promise<void>;
  resolveConflict: (choice: ConflictChoice) => Promise<void>;
}

let debounceTimer: ReturnType<typeof setTimeout> | null = null;
/**
 * Serializes syncNow calls. The auto-sync debounce, visibility-flush, post-OAuth
 * boot, and manual button can all fire close enough together to race the
 * pull→push window — without this mutex two in-flight syncs can interleave their
 * snapshot of `lastSyncedRevision`, leaving the second one to misread a freshly
 * pushed revision as "remote ahead" and surface a false conflict.
 */
let syncInFlight: Promise<void> | null = null;
/**
 * Latest trailing-edge sync requested by a caller that arrived while another
 * sync was in flight. Each coalesced caller gets back a promise that resolves
 * after its follow-up sync, not merely after the older snapshot drains.
 */
let nextSyncInFlight: Promise<void> | null = null;
let nextSyncBase: Promise<void> | null = null;
/**
 * Active realtime CDC channel for the current session. Held at module scope
 * so signIn/signOut can re-arm or tear down without going through `init()`.
 */
let unsubscribeRealtime: (() => void) | null = null;

function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/**
 * Does this device hold profile data the cloud might not? Checked over the WHOLE
 * backup envelope — decks AND preferences AND feed state — not just decks, so a
 * device whose only local data is settings/feeds is never silently overwritten
 * by a remote pull on first sign-in.
 */
function backupHasUserData(b: PhaseBackup): boolean {
  return (
    Object.keys(b.decks).length > 0 ||
    b.preferences !== null ||
    b.deckMetadata !== null ||
    b.activeDeck !== null ||
    b.feedSubscriptions !== null ||
    b.feedDeckOrigins !== null
  );
}

export const useCloudSyncStore = create<CloudSyncState>()(
  persist(
    (set, get) => ({
      available: false,
      identity: null,
      sessionResolved: false,
      status: "idle",
      error: null,
      dirty: false,
      lastSyncedRevision: null,
      lastSyncedAt: null,
      conflict: null,
      conflictDiff: null,

      init: () => {
        // Each call to init() runs to completion. Idempotency is provided
        // by the inner singletons it talks to (watchUserStorage's `installed`
        // flag, armRealtime's `unsubscribeRealtime?.()` teardown-before-resub)
        // — NOT by a module-level "already initialized" gate, which used to
        // strand the UI across HMR: the OLD bundle's init flipped the flag
        // true, the NEW bundle's init early-returned, and the OLD init's
        // restoreSession promise was orphaned with no .then ever firing.
        // Captured by every async callback below; if cleanup has run before
        // those callbacks resolve (React StrictMode double-invokes the
        // mounting effect, and HMR does the same), they no-op instead of
        // arming a realtime channel that nobody owns. Without this guard the
        // first init's restoreSession resolves, subscribes channel A, then
        // the second init's restoreSession resolves and tears down channel A
        // mid-handshake to subscribe channel B — surfacing as
        // CHANNEL_ERROR: socket closed: 1000 → CLOSED in the console.
        let cancelled = false;
        const provider = getCloudSyncProvider();
        set({ available: provider !== null });
        if (!provider) {
          // No provider configured (self-hoster build). Session is trivially
          // resolved — there's nothing to wait for — so the UI escapes the
          // "unknown" state immediately instead of stranding on the loading
          // affordance forever.
          set({ sessionResolved: true });
          return () => {};
        }

        // Mark dirty + debounce a push whenever user-owned storage changes.
        // Deferred to a microtask so writes that happen inside a React render
        // commit (legacy code paths) don't trigger the "Cannot update a
        // component while rendering" warning. The microtask drains at the
        // end of the current task, before the next paint or user input.
        const uninstallWatcher = watchUserStorage(() => {
          queueMicrotask(() => {
            set({ dirty: true });
            if (!get().identity) return;
            if (debounceTimer) clearTimeout(debounceTimer);
            debounceTimer = setTimeout(() => {
              void get().syncNow();
            }, AUTO_SYNC_DEBOUNCE_MS);
          });
        });

        // Tab visibility ↔ sync:
        //   hidden  → flush pending local changes so they don't wait out the
        //             debounce window if the user closes/backgrounds the tab.
        //   visible → pull to learn about peer changes that landed while this
        //             tab was backgrounded. This is the universal fallback for
        //             when realtime CDC is unavailable or temporarily broken;
        //             with realtime working, the pull is usually a no-op.
        const onVisibility = () => {
          if (!get().identity) return;
          if (document.visibilityState === "hidden" && get().dirty) {
            void get().syncNow();
          } else if (document.visibilityState === "visible") {
            void get().syncNow();
          }
        };
        document.addEventListener("visibilitychange", onVisibility);

        // Realtime subscription to peer-device pushes. While signed in we hold
        // one CDC channel; on every remote revision tick that's newer than
        // ours, fire a syncNow so this tab catches up. Required to make
        // "green = currently in sync" honest in multi-device/multi-tab setups
        // — without this the icon lies until the next local write or boot.
        const armRealtime = () => {
          unsubscribeRealtime?.();
          unsubscribeRealtime = provider.subscribe((newRevision) => {
            if (newRevision !== get().lastSyncedRevision) {
              void get().syncNow();
            }
          });
        };

        // Adopt any existing session (including one just returned from an OAuth
        // redirect), then reconcile and arm realtime. The `cancelled` guard
        // makes the callback a no-op if the mounting effect's cleanup has
        // already run — otherwise a stale init would arm a realtime channel
        // after the current init already armed its own, racing two subscribes.
        void provider
          .restoreSession()
          .then((identity) => {
            if (cancelled) return;
            set({ identity, sessionResolved: true });
            if (identity) {
              void get().syncNow();
              armRealtime();
            }
          })
          .catch((e) => {
            if (cancelled) return;
            // Mark resolved even on failure — the UI must transition out of
            // "unknown" so the signed-out CTA renders. The error surfaces
            // through `status: "error"` for visibility.
            set({
              identity: null,
              sessionResolved: true,
              status: "error",
              error: errorMessage(e),
            });
          });

        return () => {
          cancelled = true;
          uninstallWatcher();
          document.removeEventListener("visibilitychange", onVisibility);
          if (debounceTimer) clearTimeout(debounceTimer);
          unsubscribeRealtime?.();
          unsubscribeRealtime = null;
        };
      },

      signIn: async (authProvider) => {
        const provider = getCloudSyncProvider();
        if (!provider) return;
        set({ error: null });
        // Redirects away; the session is adopted by init() on return.
        await provider.signIn(authProvider);
      },

      signOut: async () => {
        const provider = getCloudSyncProvider();
        if (!provider) return;
        // Tear down realtime first so we stop receiving notifications for a
        // user we are no longer authenticated as.
        unsubscribeRealtime?.();
        unsubscribeRealtime = null;
        await provider.signOut();
        set({ identity: null, status: "idle", error: null });
      },

      syncNow: async () => {
        // Coalesce concurrent callers onto the in-flight promise so the
        // pull→push window can't be straddled by a stale snapshot. A caller that
        // arrives mid-flight may have observed a newer remote revision than the
        // running sync snapshotted, so chain a trailing sync and return that
        // promise to the caller.
        if (syncInFlight) {
          if (nextSyncInFlight && nextSyncBase === syncInFlight) {
            return nextSyncInFlight;
          }
          const base = syncInFlight;
          const followUp = base
            .then(() => get().syncNow())
            .finally(() => {
              if (nextSyncInFlight === followUp) {
                nextSyncInFlight = null;
                nextSyncBase = null;
              }
            });
          nextSyncBase = base;
          nextSyncInFlight = followUp;
          return followUp;
        }
        const provider = getCloudSyncProvider();
        const identity = provider?.identity() ?? null;
        if (!provider || !identity) return;
        set({ status: "syncing", error: null, identity });
        syncInFlight = (async () => {
          try {
            // Cheap metadata-only read first. The full payload is fetched only
            // in the two branches below that actually reconcile remote data, so
            // the common "nothing changed" tab-focus/realtime sync transfers a
            // few bytes instead of the entire backup envelope.
            const meta = await provider.pullMeta();
            const local = buildBackup();
            const { lastSyncedRevision, dirty } = get();

            if (!meta) {
              // Account is empty — seed it with this device's data.
              const seeded = await provider.push(local, null);
              set({
                status: "synced",
                dirty: false,
                lastSyncedRevision: seeded.revision,
                lastSyncedAt: new Date().toISOString(),
              });
              return;
            }

            const remoteAhead = meta.revision !== lastSyncedRevision;
            // On first sign-in (no revision history here) treat any existing
            // local profile data — decks, prefs, or feeds — as unsynced changes
            // so we never silently discard them to a remote pull.
            const localChanged =
              dirty ||
              (lastSyncedRevision === null && backupHasUserData(local));

            if (remoteAhead && localChanged) {
              // Diverged: need the remote body for the digest + conflict diff.
              const remote = await provider.pull();
              if (!remote) {
                // Row deleted between pullMeta and pull — reseed as empty.
                const seeded = await provider.push(local, null);
                set({
                  status: "synced",
                  dirty: false,
                  lastSyncedRevision: seeded.revision,
                  lastSyncedAt: new Date().toISOString(),
                });
                return;
              }
              // Suppress false conflicts: if local and remote payloads are
              // byte-identical (after excluding the volatile exportedAt
              // timestamp), there's nothing to reconcile — just adopt the
              // remote revision and continue silently.
              const [localDigest, remoteDigest] = await Promise.all([
                computeBackupDigest(local),
                computeBackupDigest(remote.backup),
              ]);
              if (localDigest === remoteDigest) {
                set({
                  status: "synced",
                  dirty: false,
                  lastSyncedRevision: remote.meta.revision,
                  lastSyncedAt: new Date().toISOString(),
                });
                return;
              }
              set({
                status: "conflict",
                conflict: remote,
                conflictDiff: summarizeBackupDiff(local, remote.backup),
              });
              return;
            }
            if (remoteAhead && !localChanged) {
              // Remote moved, nothing local to preserve: fetch the body and
              // adopt it wholesale.
              const remote = await provider.pull();
              if (!remote) {
                const seeded = await provider.push(local, null);
                set({
                  status: "synced",
                  dirty: false,
                  lastSyncedRevision: seeded.revision,
                  lastSyncedAt: new Date().toISOString(),
                });
                return;
              }
              applyRemote(set, remote);
              return;
            }
            if (!remoteAhead && localChanged) {
              // Local-only changes: push with the meta revision as the CAS
              // guard — no remote body needed.
              const pushed = await provider.push(local, meta.revision);
              set({
                status: "synced",
                dirty: false,
                lastSyncedRevision: pushed.revision,
                lastSyncedAt: new Date().toISOString(),
              });
              return;
            }
            // Already in sync: nothing moved over the wire, but the meta read
            // confirmed both sides agree on the current revision. That IS a
            // successful reconciliation — stamp lastSyncedAt so the user
            // pressing "Sync now" gets visible confirmation.
            set({ status: "synced", lastSyncedAt: new Date().toISOString() });
          } catch (e) {
            if (e instanceof SyncConflictError) {
              // Lost the write race — re-pull and ask the user.
              const remote = await provider.pull();
              if (remote) {
                const local = buildBackup();
                set({
                  status: "conflict",
                  conflict: remote,
                  conflictDiff: summarizeBackupDiff(local, remote.backup),
                });
                return;
              }
              // The row is gone (deleted, or a delete+reinsert race that turned
              // our null-revision reseed into P0001). Reseed this device's data
              // as the new account state rather than dead-ending at an error.
              try {
                const seeded = await provider.push(buildBackup(), null);
                set({
                  status: "synced",
                  dirty: false,
                  lastSyncedRevision: seeded.revision,
                  lastSyncedAt: new Date().toISOString(),
                });
              } catch (reseedErr) {
                set({ status: "error", error: errorMessage(reseedErr) });
              }
              return;
            }
            set({ status: "error", error: errorMessage(e) });
          }
        })();
        try {
          await syncInFlight;
        } finally {
          syncInFlight = null;
        }
      },

      resolveConflict: async (choice) => {
        const { conflict } = get();
        const provider = getCloudSyncProvider();
        if (!conflict || !provider) return;

        if (choice === "cloud") {
          applyRemote(set, conflict);
          return;
        }
        // Keep this device: fast-forward over the remote we just pulled.
        set({ status: "syncing", conflict: null, conflictDiff: null });
        try {
          const meta = await provider.push(buildBackup(), conflict.meta.revision);
          set({
            status: "synced",
            dirty: false,
            lastSyncedRevision: meta.revision,
            lastSyncedAt: meta.updatedAt,
          });
        } catch (e) {
          set({ status: "error", error: errorMessage(e) });
        }
      },
    }),
    {
      name: "phase-cloud-sync",
      // Identity + transient status are re-derived at runtime; only the sync
      // bookkeeping needs to survive reloads.
      partialize: (s) => ({
        dirty: s.dirty,
        lastSyncedRevision: s.lastSyncedRevision,
        lastSyncedAt: s.lastSyncedAt,
      }),
    },
  ),
);

/**
 * Overwrite the local profile with a remote snapshot and rehydrate in place.
 *
 * No page reload: a reload destroys in-progress UI state (modals, navigation,
 * multiplayer DataChannels) and used to drive a two-tab ping-pong loop with
 * CDC. Instead we (a) write the snapshot to localStorage with the watcher
 * suppressed, (b) rehydrate every Zustand store whose persisted slice lives
 * in the backup envelope, and (c) broadcast `PROFILE_REPLACED_EVENT` so the
 * remaining direct-localStorage readers (deck list, feed metadata) re-fetch.
 */
function applyRemote(
  set: (partial: Partial<CloudSyncState>) => void,
  remote: RemoteSnapshot,
): void {
  withStorageWatchSuppressed(() => {
    applyBackup(remote.backup, "overwrite");
  });
  set({
    status: "synced",
    dirty: false,
    conflict: null,
    conflictDiff: null,
    lastSyncedRevision: remote.meta.revision,
    lastSyncedAt: new Date().toISOString(),
  });
  // Preferences is the only Zustand-persisted slice carried in the backup
  // envelope; multiplayerStore is session-scoped and not synced. If the
  // envelope ever grows to include another persisted store, rehydrate it here.
  void usePreferencesStore.persist.rehydrate();
  window.dispatchEvent(new CustomEvent(PROFILE_REPLACED_EVENT));
}
