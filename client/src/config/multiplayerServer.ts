/**
 * The official lobby broker for THIS build's release channel.
 *
 * Channel-scoped rather than a single fixed address. The lobby advertises its
 * versions blind (it sends `ServerHello` before the client's `ClientHello`), and
 * clients built before `lobby_protocol_version` existed accept a lobby only
 * within `[PROTOCOL_VERSION - 1, PROTOCOL_VERSION]` of their OWN build.
 * Production's Worker redeploys only at release while preview rebuilds from
 * `main`, so once `main` is two protocol bumps past the last tag those windows
 * are disjoint and a single shared lobby must lock one channel out.
 *
 * `LOBBY_PROTOCOL_VERSION` (see `ws-adapter.ts`) removes that coupling for
 * current builds, but already-deployed clients still gate on the shared number —
 * so each channel keeps its own broker. `deploy.yml` sets this to the preview
 * lobby.
 *
 * Defaults to the production lobby, so release and self-hosted builds are
 * unchanged.
 */
export const OFFICIAL_MULTIPLAYER_SERVER_URL = __OFFICIAL_MULTIPLAYER_SERVER_URL__;
export const DEFAULT_MULTIPLAYER_SERVER_URL = __DEFAULT_MULTIPLAYER_SERVER_URL__;

/** Hosts we operate. Every channel's broker belongs here — `isOfficial…` gates
 * the persisted-address migration, which treats an address on any of these as a
 * deployment default rather than user intent. */
const OFFICIAL_MULTIPLAYER_SERVER_HOSTS = new Set([
  "lobby.phase-rs.dev",
  "lobby-preview.phase-rs.dev",
  "us.phase-rs.dev",
]);

export function isOfficialMultiplayerServerUrl(value: string): boolean {
  try {
    return OFFICIAL_MULTIPLAYER_SERVER_HOSTS.has(new URL(value).hostname);
  } catch {
    return false;
  }
}
