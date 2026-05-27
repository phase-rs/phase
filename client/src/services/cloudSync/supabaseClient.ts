import { createClient, type SupabaseClient } from "@supabase/supabase-js";

/**
 * Build-time config injected via Vite defines (see vite.config.ts). The anon
 * key is PUBLIC by design — Row-Level Security is the actual access control, so
 * shipping it in the client bundle is its intended use, not a leak. Both are
 * empty when a deployment doesn't configure Supabase (e.g. self-hosters), which
 * disables cloud sync and leaves file backup as the only data-portability path.
 *
 * The `typeof` guard keeps this module importable under Vitest, where the
 * defines may be absent.
 */
const SUPABASE_URL =
  typeof __SUPABASE_URL__ !== "undefined" ? __SUPABASE_URL__ : "";
const SUPABASE_ANON_KEY =
  typeof __SUPABASE_ANON_KEY__ !== "undefined" ? __SUPABASE_ANON_KEY__ : "";

export function isSupabaseConfigured(): boolean {
  return SUPABASE_URL.length > 0 && SUPABASE_ANON_KEY.length > 0;
}

let client: SupabaseClient | null = null;

/**
 * Lazily construct the singleton client. Callers must guard with
 * `isSupabaseConfigured()` first — calling this when unconfigured throws.
 */
export function getSupabaseClient(): SupabaseClient {
  if (!client) {
    client = createClient(SUPABASE_URL, SUPABASE_ANON_KEY, {
      auth: {
        // supabase-js owns token storage + refresh, so silent background sync
        // works (unlike a bare access-token model that can't refresh).
        persistSession: true,
        autoRefreshToken: true,
        // Process the OAuth fragment on return from the provider redirect.
        detectSessionInUrl: true,
      },
    });
    // Bridge auth → realtime: supabase-js refreshes the JWT every ~hour, but
    // the realtime WebSocket does NOT learn about the refresh on its own. When
    // the old JWT's `exp` passes, the server closes the channel with empty
    // error (the documented "CLOSED with no err" pattern). Forwarding every
    // session change to realtime.setAuth keeps the WS authenticated indefinitely.
    const c = client;
    c.auth.onAuthStateChange((_event, session) => {
      void c.realtime.setAuth(session?.access_token ?? null);
    });
  }
  return client;
}
