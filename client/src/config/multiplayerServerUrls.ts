// Build-time resolution of the two multiplayer-server URLs. Imported by
// vite.config.ts AND vitest.config.ts so the resolution ORDER has one authority
// — it is subtle enough that duplicating it produced a real defect (see below),
// and a config file cannot import the define-consuming module it feeds.
//
// Deliberately define-free: this runs in the Node config context, where
// `__DEFAULT_MULTIPLAYER_SERVER_URL__` and friends do not exist yet.

/** Fallback for both URLs. Keeps release and self-hosted builds unchanged. */
export const PRODUCTION_MULTIPLAYER_SERVER_URL = "wss://lobby.phase-rs.dev/ws";

export interface MultiplayerServerUrls {
  /** This channel's official broker. */
  official: string;
  /** What the bundle connects to by default. */
  buildDefault: string;
}

/**
 * Resolve both URLs from the build environment.
 *
 * `buildDefault` falls back to the RESOLVED `official`, never to
 * {@link PRODUCTION_MULTIPLAYER_SERVER_URL} directly. That distinction is
 * load-bearing: `serverDetection.ts` reads `buildDefault !== official` as "this
 * is a self-hosted build" and prepends a self-hosted preset, which becomes
 * `SERVER_PRESETS[0]` and therefore `DEFAULT_SERVER`. Chaining the fallback to
 * the production constant would leave a preview build with
 * `official` = preview but `buildDefault` = production, so the DEFAULT PICK
 * would be the one broker that build cannot handshake with.
 *
 * @param readEnv returns "" or undefined for an unset variable.
 */
export function resolveMultiplayerServerUrls(
  readEnv: (name: string) => string | undefined,
): MultiplayerServerUrls {
  const official =
    readEnv("OFFICIAL_MULTIPLAYER_SERVER_URL") || PRODUCTION_MULTIPLAYER_SERVER_URL;
  return {
    official,
    buildDefault: readEnv("DEFAULT_MULTIPLAYER_SERVER_URL") || official,
  };
}
