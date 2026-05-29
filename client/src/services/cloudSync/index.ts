import { SupabaseSyncProvider } from "./supabaseProvider";
import type { CloudSyncProvider } from "./types";

let resolved = false;
let provider: CloudSyncProvider | null = null;

/**
 * Returns the configured cloud-sync provider, or null when the deployment has
 * none (self-hosters with no Supabase build env). Callers treat null as "cloud
 * sync unavailable" and fall back to file backup. Resolved once and cached.
 */
export function getCloudSyncProvider(): CloudSyncProvider | null {
  if (!resolved) {
    const supabase = new SupabaseSyncProvider();
    provider = supabase.isConfigured() ? supabase : null;
    resolved = true;
  }
  return provider;
}

export type {
  CloudSyncProvider,
  SyncIdentity,
  SyncAuthProvider,
  RemoteSnapshot,
  RemoteMeta,
} from "./types";
export { SyncConflictError } from "./types";
