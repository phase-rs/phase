export const OFFICIAL_MULTIPLAYER_SERVER_URL = "wss://lobby.phase-rs.dev/ws";
export const DEFAULT_MULTIPLAYER_SERVER_URL = __DEFAULT_MULTIPLAYER_SERVER_URL__;

const OFFICIAL_MULTIPLAYER_SERVER_HOSTS = new Set([
  "lobby.phase-rs.dev",
  "us.phase-rs.dev",
]);

export function isOfficialMultiplayerServerUrl(value: string): boolean {
  try {
    return OFFICIAL_MULTIPLAYER_SERVER_HOSTS.has(new URL(value).hostname);
  } catch {
    return false;
  }
}
