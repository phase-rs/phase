import { runtimeConfigValue } from "../../config/runtimeConfig";
import { isOpenableExternalUrl } from "../openExternal";

/**
 * The Supabase project cloud sync talks to. Empty strings mean unconfigured,
 * which disables cloud sync and leaves file backup as the only
 * data-portability path.
 *
 * Runtime configuration wins over the build-time defines so one generic image
 * can be deployed anywhere: a self-hosted deployment serves its own
 * `/config.js` (see `client/public/config.js`) naming its own project. Without
 * that, a self-hosted copy of an official build signs players in against the
 * official project, whose OAuth redirect allow-list does not include the
 * self-hosted origin, so Supabase sends them back to the official site.
 *
 * The URL and anon key identify one project, so they are taken as a pair: a
 * runtime override applies only when both are present and the URL is an
 * http(s) URL. A half-configured override falls back to the defines entirely
 * rather than pairing one project's URL with another project's key.
 *
 * The anon key is PUBLIC by design — Row-Level Security is the actual access
 * control, so shipping it to the client is its intended use, not a leak.
 *
 * Read once at module load, like `DEFAULT_MULTIPLAYER_SERVER_URL`: the client
 * singleton and the session-storage key must agree on one project for the
 * page's lifetime. The `typeof` guards keep this module importable under
 * Vitest and the desktop bootstrap, where some defines may be absent.
 */
export interface SupabaseConfig {
  readonly url: string;
  readonly anonKey: string;
}

/**
 * The project the build-time defines name. It is the fallback for
 * `SUPABASE_CONFIG`, and the project that any cloud-sync state written before
 * runtime configuration existed belongs to.
 */
export const BUILD_TIME_SUPABASE_CONFIG: SupabaseConfig = {
  url: typeof __SUPABASE_URL__ !== "undefined" ? __SUPABASE_URL__ : "",
  anonKey:
    typeof __SUPABASE_ANON_KEY__ !== "undefined" ? __SUPABASE_ANON_KEY__ : "",
};

function resolveSupabaseConfig(): SupabaseConfig {
  const url = runtimeConfigValue("supabaseUrl", isOpenableExternalUrl);
  const anonKey = runtimeConfigValue("supabaseAnonKey", () => true);
  if (url !== null && anonKey !== null) return { url, anonKey };
  return BUILD_TIME_SUPABASE_CONFIG;
}

export const SUPABASE_CONFIG: SupabaseConfig = resolveSupabaseConfig();
