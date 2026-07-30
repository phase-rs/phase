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

/**
 * Convert a multiplayer WebSocket URL to the HTTP origin that serves
 * `/p2p-draft-backup` (and `/health`). Strips a trailing `/ws` path.
 *
 * Examples:
 * - `wss://lobby.phase-rs.dev/ws` → `https://lobby.phase-rs.dev`
 * - `ws://127.0.0.1:9374/ws` → `http://127.0.0.1:9374`
 */
export function wsUrlToHttpOrigin(wsUrl: string): string | null {
  try {
    const url = new URL(wsUrl);
    if (url.protocol === "wss:") {
      url.protocol = "https:";
    } else if (url.protocol === "ws:") {
      url.protocol = "http:";
    } else {
      return null;
    }
    if (url.pathname === "/ws" || url.pathname.endsWith("/ws")) {
      url.pathname = url.pathname.replace(/\/ws\/?$/, "") || "/";
    }
    url.search = "";
    url.hash = "";
    const path = url.pathname === "/" ? "" : url.pathname.replace(/\/$/, "");
    return `${url.origin}${path}`;
  } catch {
    return null;
  }
}

/**
 * HTTP base URL for best-effort P2P draft server backups.
 * Prefer an explicit Vite override, else the official multiplayer lobby.
 */
export function resolveP2pBackupEndpoint(
  wsUrl: string = import.meta.env.VITE_WS_URL ?? OFFICIAL_MULTIPLAYER_SERVER_URL,
): string | null {
  return wsUrlToHttpOrigin(wsUrl);
}
