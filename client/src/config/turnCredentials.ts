// Build-time resolution for the TURN credential endpoint. Keep this define-free
// so both Vite config files and their tests can use the same fallback order.

/** The official endpoint used by existing builds when no override is set. */
export const OFFICIAL_TURN_CREDENTIALS_URL =
  "https://lobby.phase-rs.dev/turn-credentials";

/**
 * Resolve the endpoint configured for a web build.
 *
 * Empty values are treated as unset so self-hosted build scripts can pass
 * through an optional environment variable without changing the default.
 */
export function resolveTurnCredentialsUrl(value: string | undefined): string {
  return value || OFFICIAL_TURN_CREDENTIALS_URL;
}
