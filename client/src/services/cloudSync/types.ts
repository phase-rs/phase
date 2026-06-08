import type { PhaseBackup } from "../backup";

/** Provider-scoped identity, used for display and account keying. */
export interface SyncIdentity {
  readonly userId: string;
  readonly label: string;
  readonly avatarUrl?: string;
}

/** Opaque remote version marker for optimistic-concurrency conflict detection. */
export interface RemoteMeta {
  /** Monotonic revision the provider returns on every read/write. */
  readonly revision: number;
  /** ISO timestamp of the remote write (mirrors PhaseBackup.exportedAt). */
  readonly updatedAt: string;
}

export interface RemoteSnapshot {
  readonly backup: PhaseBackup;
  readonly meta: RemoteMeta;
}

/** OAuth identity providers offered for sign-in. */
export type SyncAuthProvider = "discord" | "google";

export type SyncProviderId = "supabase";

/**
 * Thrown by `push` when the caller's expectedRevision is stale — another device
 * wrote since this one last synced. Callers re-pull and surface a choice.
 */
export class SyncConflictError extends Error {
  constructor(message = "Remote copy changed since last sync") {
    super(message);
    this.name = "SyncConflictError";
  }
}

/**
 * Transport-agnostic cloud-sync backend, mirroring the EngineAdapter pattern.
 * Supabase is the first implementation; Google Drive / Dropbox can slot in
 * behind the same contract without touching callers. The synced payload is the
 * existing `PhaseBackup` envelope — cloud sync introduces no new data model.
 */
export interface CloudSyncProvider {
  readonly id: SyncProviderId;
  /** True only when build-time config is present. False → UI hides cloud sync. */
  isConfigured(): boolean;
  /** Rehydrate a persisted session silently on boot, if one exists. */
  restoreSession(): Promise<SyncIdentity | null>;
  /** Interactive OAuth sign-in; the SDK handles the redirect/popup dance. */
  signIn(provider: SyncAuthProvider): Promise<void>;
  signOut(): Promise<void>;
  identity(): SyncIdentity | null;
  /**
   * Cheap metadata-only read — the revision marker + timestamp, never the
   * payload. null if the account has never synced. This is the routine
   * change-detection path: `syncNow` compares `revision` against its last-seen
   * value and only falls through to the full `pull()` when reconciliation
   * actually needs the envelope body. Reading the 8-byte revision must not cost
   * the whole backup over the wire on every sync trigger.
   */
  pullMeta(): Promise<RemoteMeta | null>;
  /** Read the full remote envelope, or null if the account has never synced. */
  pull(): Promise<RemoteSnapshot | null>;
  /**
   * Write the envelope with optimistic concurrency. `expectedRevision` is the
   * revision last seen by this device (null for the first-ever write). Throws
   * SyncConflictError if the remote advanced past it.
   */
  push(
    backup: PhaseBackup,
    expectedRevision: number | null,
  ): Promise<RemoteMeta>;
  /**
   * Subscribe to remote-side changes on this user's backup row so peer
   * devices' pushes are detected within ~1s instead of "whenever this device
   * next checks." `onChange` fires with the new revision number; callers
   * should trigger a sync when `newRevision !== lastSyncedRevision`. Returns
   * an unsubscribe function. May be a no-op for providers without realtime.
   */
  subscribe(onChange: (newRevision: number) => void): () => void;
}
