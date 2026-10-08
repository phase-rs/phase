import { SupabaseSyncProvider } from "./supabaseProvider";
import { isSupabaseConfigured } from "./supabaseClient";
import type { CloudSyncProvider } from "./types";

let resolved = false;
let provider: CloudSyncProvider | null = null;

/**
 * Returns whether this build has cloud-sync configuration without resolving a
 * provider or constructing the Supabase SDK client.
 */
export function isCloudSyncConfigured(): boolean {
  return isSupabaseConfigured();
}

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

/**
 * Pauses cloud transport activity only when a provider has already been
 * resolved. This intentionally does not construct the provider on offline boot.
 */
export function pauseCloudSyncProvider(): Promise<void> {
  return provider?.pause() ?? Promise.resolve();
}

export type {
  CloudSyncProvider,
  SyncIdentity,
  SyncAuthProvider,
  RemoteSnapshot,
  RemoteMeta,
} from "./types";
export { SyncConflictError } from "./types";
