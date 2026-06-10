import { isTauri } from "./sidecar";
import { useMultiplayerStore } from "../stores/multiplayerStore";

const DEFAULT_PORT = 9374;

/** Which national flag to show beside a canonical region. A union (not a bare
 * literal) so a future self-host preset can add its own flag. */
export type FlagCode = "us";

export interface ServerPreset {
  label: string;
  url: string;
  flag: FlagCode;
}

/**
 * User-pickable canonical regions. The official deployment is a single global
 * lobby, so there is intentionally one entry — additional regional lobbies would
 * fragment the matchmaking pool, defeating the broker's purpose. Self-hosted
 * servers are entered via the custom-URL field, not here. See
 * .planning/lobby-failover-federation-plan.md.
 */
export const SERVER_PRESETS: ServerPreset[] = [
  { label: "Official", url: "wss://lobby.phase-rs.dev/ws", flag: "us" },
];

/** The default region's URL — first entry in {@link SERVER_PRESETS}. */
export const DEFAULT_SERVER = SERVER_PRESETS[0].url;

/** Flag for a known preset server, or `null` for self-hosted/custom addresses. */
export function flagForServer(url: string): FlagCode | null {
  return SERVER_PRESETS.find((p) => p.url === url)?.flag ?? null;
}

export function parseWebSocketUrl(value: string): URL | null {
  try {
    const url = new URL(value);
    if ((url.protocol !== "ws:" && url.protocol !== "wss:") || !url.host) {
      return null;
    }
    return url;
  } catch {
    return null;
  }
}

export function isValidWebSocketUrl(value: string): boolean {
  return parseWebSocketUrl(value) !== null;
}

/**
 * Detect the best WebSocket server URL by trying in order:
 * 1. Tauri sidecar on localhost
 * 2. Last-used server address from store
 * 3. Default production server
 */
export async function detectServerUrl(): Promise<string> {
  // Step 1: If running in Tauri, check localhost sidecar
  if (isTauri()) {
    const sidecarUrl = await tryHealthCheck(`http://localhost:${DEFAULT_PORT}/health`);
    if (sidecarUrl) {
      return `ws://localhost:${DEFAULT_PORT}/ws`;
    }
  }

  // Step 2: Try the stored server address
  const stored = useMultiplayerStore.getState().serverAddress;
  if (stored && isValidWebSocketUrl(stored)) {
    const httpUrl = wsUrlToHealthUrl(stored);
    if (httpUrl) {
      const reachable = await tryHealthCheck(httpUrl);
      if (reachable) {
        return stored;
      }
    }
  }

  // Step 3: Fall back to stored address or default production server
  return isValidWebSocketUrl(stored) ? stored : DEFAULT_SERVER;
}

/**
 * Parse a join code that may contain a server address.
 *
 * An explicit `ws://`/`wss://` scheme in the address is respected; otherwise the
 * scheme defaults to `wss://` for remote hosts and `ws://` for loopback. A bare
 * remote host with no port resolves to the standard TLS port (443), since a
 * remote `wss://` endpoint is virtually always a reverse proxy or tunnel
 * (ngrok, Cloudflare, Caddy) on 443 rather than the raw phase-server port.
 *
 * Formats:
 *   "ABC123"                        -> { code: "ABC123" }
 *   "ABC123@play.example.com"       -> { ..., serverAddress: "wss://play.example.com/ws" }
 *   "ABC123@ws://192.168.1.5:9374"  -> { ..., serverAddress: "ws://192.168.1.5:9374/ws" }
 *   "ABC123@192.168.1.5:9374"       -> { ..., serverAddress: "wss://192.168.1.5:9374/ws" }
 */
export function parseJoinCode(input: string): { code: string; serverAddress?: string } {
  const trimmed = input.trim();
  const atIndex = trimmed.indexOf("@");

  if (atIndex === -1) {
    return { code: trimmed };
  }

  const code = trimmed.slice(0, atIndex);
  const address = trimmed.slice(atIndex + 1);

  if (!address) {
    return { code };
  }

  // Respect an explicit scheme the host typed (e.g. `ws://` for a plain-ws LAN
  // server, or a `wss://` tunnel URL). Check `wss://` first; neither prefix is a
  // prefix of the other, but the ordering keeps the intent obvious.
  let explicitScheme: "ws" | "wss" | null = null;
  let hostPort = address;
  if (address.startsWith("wss://")) {
    explicitScheme = "wss";
    hostPort = address.slice("wss://".length);
  } else if (address.startsWith("ws://")) {
    explicitScheme = "ws";
    hostPort = address.slice("ws://".length);
  }

  // Split host:port on the last colon so a host:port pair splits correctly.
  const colonIndex = hostPort.lastIndexOf(":");
  const hasPort = colonIndex !== -1 && colonIndex < hostPort.length - 1;
  const host = hasPort ? hostPort.slice(0, colonIndex) : hostPort;

  const isLocal = host === "localhost" || host === "127.0.0.1";
  const scheme = explicitScheme ?? (isLocal ? "ws" : "wss");

  // No explicit port: a wss:// host is a standard-port (443) TLS endpoint; a
  // ws:// host is the phase-server default.
  let port: number;
  if (hasPort) {
    const parsedPort = parseInt(hostPort.slice(colonIndex + 1), 10);
    port = isNaN(parsedPort) ? DEFAULT_PORT : parsedPort;
  } else {
    port = scheme === "wss" ? 443 : DEFAULT_PORT;
  }

  // Omit the suffix when the port is the scheme's default.
  const isDefaultPort = (scheme === "wss" && port === 443) || (scheme === "ws" && port === 80);
  const portSuffix = isDefaultPort ? "" : `:${port}`;

  return {
    code,
    serverAddress: `${scheme}://${host}${portSuffix}/ws`,
  };
}

/**
 * Build the shareable join string `CODE@host` for a server-run host from the
 * server-advertised public URL — the inverse of {@link parseJoinCode}. An
 * `https`/`wss` URL (tunnel or TLS proxy) yields a scheme-less host that
 * parseJoinCode resolves to `wss://`; an `http`/`ws` URL (a plain-ws LAN
 * `PUBLIC_URL`) keeps an explicit `ws://` so the joiner reaches the non-TLS
 * server. Returns `null` on a malformed URL.
 *
 * Only meaningful when the server advertised a `publicUrl` (server-run hosting).
 * P2P/broker hosts have no game-server URL and share the bare code instead.
 */
export function formatJoinShare(code: string, publicUrl: string): string | null {
  let url: URL;
  try {
    url = new URL(publicUrl);
  } catch {
    return null;
  }
  const insecure = url.protocol === "ws:" || url.protocol === "http:";
  return `${code}@${insecure ? "ws://" : ""}${url.host}`;
}

/**
 * Returns an actionable error message if connecting to `serverAddress` would be
 * blocked by the browser's mixed-content policy, or `null` if it is allowed.
 *
 * A page served over HTTPS cannot open an insecure `ws://` WebSocket — the
 * browser blocks it before the handshake is ever attempted, so the failure is
 * otherwise indistinguishable from an unreachable server. Loopback hosts are
 * exempt: browsers treat `localhost`/`127.0.0.1` as potentially trustworthy, so
 * `ws://localhost` is permitted even from an HTTPS origin (this is the Tauri
 * sidecar path). Outside a browser (`window` undefined) nothing is blocked.
 */
export function mixedContentBlockReason(serverAddress: string): string | null {
  const url = parseWebSocketUrl(serverAddress);
  if (!url || url.protocol !== "ws:") {
    return null;
  }
  if (typeof window === "undefined" || window.location.protocol !== "https:") {
    return null;
  }
  if (url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "[::1]") {
    return null;
  }
  return (
    `This page is served over HTTPS, so it can't connect to an insecure ws:// server (${url.host}). ` +
    `Host phase-server behind HTTPS — a wss:// reverse proxy or tunnel (ngrok, Cloudflare, Caddy) — ` +
    `or open the app over http://.`
  );
}

/** Convert ws:// URL to http:// health check URL. */
function wsUrlToHealthUrl(wsUrl: string): string | null {
  if (!isValidWebSocketUrl(wsUrl)) {
    return null;
  }
  return wsUrl
    .replace(/^ws:\/\//, "http://")
    .replace(/^wss:\/\//, "https://")
    .replace(/\/ws\/?$/, "/health");
}

async function tryHealthCheck(url: string): Promise<boolean> {
  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 2000);
    const response = await fetch(url, { signal: controller.signal });
    clearTimeout(timeoutId);
    return response.ok;
  } catch {
    return false;
  }
}
