import type { Session, User } from "@supabase/supabase-js";
import type { PhaseBackup } from "../backup";
import {
  type CloudSyncProvider,
  type RemoteMeta,
  type RemoteSnapshot,
  type SyncAuthProvider,
  type SyncIdentity,
  SyncConflictError,
} from "./types";
import { getSupabaseClient, isSupabaseConfigured } from "./supabaseClient";

const BACKUP_TABLE = "user_backups";
/**
 * Lightweight projection of (user_id, revision, updated_at) maintained
 * transactionally by `upsert_backup`. Realtime CDC watches this table instead
 * of `user_backups` so the heavy `payload` jsonb is never broadcast over the
 * WebSocket. See `supabase/schema.sql`.
 */
const REVISION_TABLE = "user_backup_revisions";
/** PostgREST surfaces a PL/pgSQL `raise exception` as this SQLSTATE. */
const PG_RAISE_EXCEPTION = "P0001";

/**
 * Discord and Google populate `user_metadata` under different keys, so fall
 * through the common ones to always produce a human label and an avatar.
 */
function identityFromUser(user: User): SyncIdentity {
  const meta = user.user_metadata ?? {};
  const label =
    (meta.full_name as string) ||
    (meta.name as string) ||
    (meta.user_name as string) ||
    user.email ||
    user.id;
  const avatarUrl =
    (meta.avatar_url as string) || (meta.picture as string) || undefined;
  return { userId: user.id, label, avatarUrl };
}

export class SupabaseSyncProvider implements CloudSyncProvider {
  readonly id = "supabase" as const;
  private current: SyncIdentity | null = null;

  isConfigured(): boolean {
    return isSupabaseConfigured();
  }

  async restoreSession(): Promise<SyncIdentity | null> {
    const client = getSupabaseClient();
    const { data } = await client.auth.getSession();
    // Realtime authenticates the WebSocket via the user's JWT. On session-
    // restore (vs interactive sign-in), supabase-js does NOT propagate the
    // access token to the realtime module — the WebSocket connects then
    // immediately closes with `socket closed: 1000` / CHANNEL_ERROR because
    // it has no token to send. Set it explicitly so subscribe() works on
    // the boot path the same way it does after a fresh sign-in.
    await client.realtime.setAuth(data.session?.access_token ?? null);
    return this.adopt(data.session);
  }

  async signIn(provider: SyncAuthProvider): Promise<void> {
    const { error } = await getSupabaseClient().auth.signInWithOAuth({
      provider,
      // Return to the same page; restoreSession() adopts the session on reload.
      options: { redirectTo: window.location.href },
    });
    if (error) throw error;
  }

  async signOut(): Promise<void> {
    const client = getSupabaseClient();
    await client.auth.signOut();
    // Switch realtime back to the unauthenticated default so any lingering
    // channel reconnect doesn't reuse the just-revoked JWT.
    await client.realtime.setAuth();
    this.current = null;
  }

  identity(): SyncIdentity | null {
    return this.current;
  }

  async pullMeta(): Promise<RemoteMeta | null> {
    // Metadata-only read: omit the `payload` column so the common "did anything
    // change?" check transfers a handful of bytes instead of the whole backup.
    // RLS + PK make this at most one row; null = never synced.
    const { data, error } = await getSupabaseClient()
      .from(BACKUP_TABLE)
      .select("revision, updated_at")
      .maybeSingle();
    if (error) throw error;
    if (!data) return null;
    return {
      // Postgres `bigint` may serialize as a JSON string; coerce so the value
      // compares (===) against lastSyncedRevision and is safe as expectedRevision.
      revision: Number(data.revision),
      updatedAt: data.updated_at as string,
    };
  }

  async pull(): Promise<RemoteSnapshot | null> {
    // RLS scopes the table to auth.uid(); the PK is user_id so there is at most
    // one row. maybeSingle() returns null when the account has never synced.
    const { data, error } = await getSupabaseClient()
      .from(BACKUP_TABLE)
      .select("payload, revision, updated_at")
      .maybeSingle();
    if (error) throw error;
    if (!data) return null;
    return {
      backup: data.payload as PhaseBackup,
      meta: {
        revision: Number(data.revision), // bigint-as-string guard (see pullMeta)
        updatedAt: data.updated_at as string,
      },
    };
  }

  async push(
    backup: PhaseBackup,
    expectedRevision: number | null,
  ): Promise<RemoteMeta> {
    // Atomic compare-and-set in Postgres: inserts when absent, updates only when
    // the stored revision matches expectedRevision, otherwise raises. Doing the
    // CAS server-side (not read-then-write here) closes the two-device race.
    const { data, error } = await getSupabaseClient().rpc("upsert_backup", {
      p_payload: backup,
      p_expected_revision: expectedRevision,
    });
    if (error) {
      if (error.code === PG_RAISE_EXCEPTION) {
        throw new SyncConflictError(error.message);
      }
      throw error;
    }
    const row = Array.isArray(data) ? data[0] : data;
    return {
      revision: Number(row.revision), // bigint-as-string guard (see pullMeta)
      updatedAt: row.updated_at as string,
    };
  }

  /**
   * Subscribe to Postgres CDC (Postgres Change Data Capture) on
   * `public.user_backup_revisions` filtered to this user's row — the
   * lightweight projection, NOT `user_backups`, so the heavy `payload` jsonb is
   * never streamed over the WebSocket. Supabase Realtime delivers INSERT/UPDATE
   * rows over a single WebSocket; we map them to a revision number for the
   * caller to compare against its lastSyncedRevision.
   *
   * Requires the projection table to be in the `supabase_realtime` publication —
   * see `supabase/schema.sql` for the ALTER PUBLICATION statement. Without it,
   * `subscribe` succeeds but no events ever fire (silent degrade).
   */
  subscribe(onChange: (newRevision: number) => void): () => void {
    if (!this.current) return () => {};
    const userId = this.current.userId;
    const client = getSupabaseClient();
    const channel = client
      .channel(`user_backup_revisions:${userId}`)
      // Postgres `bigint` may be serialized as a JSON string to preserve
      // precision past 2^53, so type the payload column as `number | string`
      // and coerce at the boundary. The revision counter is realistically
      // tiny, but the safer typing prevents a future "12" !== 12 mismatch.
      .on<{ revision: number | string; user_id: string }>(
        "postgres_changes",
        {
          event: "*",
          schema: "public",
          table: REVISION_TABLE,
          filter: `user_id=eq.${userId}`,
        },
        (payload) => {
          // INSERT/UPDATE carry the new row in `payload.new`; DELETE only has
          // `payload.old`. Forward-progress only — guard on revision present.
          const rev = payload.new && "revision" in payload.new
            ? Number(payload.new.revision)
            : NaN;
          if (Number.isFinite(rev)) onChange(rev);
        },
      )
      .subscribe((status, err) => {
        // SUBSCRIBED = channel is live. CHANNEL_ERROR / TIMED_OUT / CLOSED
        // mean the realtime feed is not running and we'll receive no events.
        // Surface to the console so the operator can spot misconfigured
        // publication membership (the #1 silent-degrade failure mode).
        if (status !== "SUBSCRIBED") {
          console.warn(
            `[cloudSync] realtime channel status=${status}`,
            err ?? "",
          );
        }
      });
    return () => {
      void client.removeChannel(channel);
    };
  }

  private adopt(session: Session | null): SyncIdentity | null {
    this.current = session?.user ? identityFromUser(session.user) : null;
    return this.current;
  }
}
