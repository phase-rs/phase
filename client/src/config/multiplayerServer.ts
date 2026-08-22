/**
 * The official lobby broker for THIS build's release channel.
 *
 * Channel-scoped rather than a single fixed address: the lobby advertises one
 * protocol version blind (it sends `ServerHello` before the client's
 * `ClientHello`), and a client only accepts a lobby within
 * `[PROTOCOL_VERSION - 1, PROTOCOL_VERSION]` of its OWN build. Production's
 * Worker redeploys only at release while preview rebuilds from `main`, so once
 * `main` is two protocol bumps past the last tag those windows are disjoint and
 * a single shared lobby must lock one channel out. Each channel therefore
 * points at its own broker; `deploy.yml` sets this to the preview lobby.
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
