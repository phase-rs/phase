import { useMultiplayerStore } from "../stores/multiplayerStore";
import {
  DEFAULT_MULTIPLAYER_SERVER_URL,
  OFFICIAL_MULTIPLAYER_SERVER_URL,
  parseWebSocketUrl,
} from "../config/multiplayerServer";

// Re-exported from the config leaf, which needs it to validate the runtime
// /config.js override and cannot import this module without a cycle. One
// implementation, and every existing import of it from here still resolves.
export { parseWebSocketUrl };

const DEFAULT_PORT = 9374;

/** Which national flag to show beside a canonical region. A union (not a bare
 * literal) so a future self-host preset can add its own flag. */
export type FlagCode = "us";

export interface ServerPreset {
  labelKey: string;
  url: string;
  flag?: FlagCode;
}

/**
 * User-pickable canonical endpoints. The official deployment is a single global
 * lobby; a self-hosted build may prepend its configured default so the picker
 * clearly shows where this static bundle connects by default. Additional
 * one-off self-hosted servers are still entered through the custom URL field.
 */
export const SERVER_PRESETS: ServerPreset[] = [
  ...(DEFAULT_MULTIPLAYER_SERVER_URL === OFFICIAL_MULTIPLAYER_SERVER_URL
    ? []
    : [{ labelKey: "serverPicker.selfHosted", url: DEFAULT_MULTIPLAYER_SERVER_URL }]),
  {
    labelKey: "serverPicker.official",
    url: OFFICIAL_MULTIPLAYER_SERVER_URL,
    flag: "us",
  },
];

/** The default region's URL — first entry in {@link SERVER_PRESETS}. */
export const DEFAULT_SERVER = SERVER_PRESETS[0].url;

/** Flag for a known preset server, or `null` for self-hosted/custom addresses. */
export function flagForServer(url: string): FlagCode | null {
  return SERVER_PRESETS.find((p) => p.url === url)?.flag ?? null;
}

export function isValidWebSocketUrl(value: string): boolean {
  return parseWebSocketUrl(value) !== null;
}

/**
 * The fallback server a socket opens on when the caller carries no explicit
 * origin: the hosting server the player chose in the server picker, falling
 * back to this build's default when none is chosen or the stored value is
 * unusable.
 *
 * A join or spectate origin is NOT read from here — it is carried explicitly
 * by the route (`?server=`) from the source that listed the game, so browsing
 * one authority and joining another cannot silently switch the player's
 * hosting target.
 *
 * Reachability is deliberately not consulted. The desktop shell reaches its own
 * native engine over a Tauri IPC bridge (`services/nativeEngineSocket.ts`) on an
 * ephemeral port, never through a URL from here, so probing localhost can only
 * capture an unrelated phase-server and silently override the player's choice.
 *
 * Stays `async` for its awaiting callers; there is nothing left to wait for.
 */
export async function detectServerUrl(): Promise<string> {
  const stored = useMultiplayerStore.getState().hostingServer;
  return stored !== null && isValidWebSocketUrl(stored) ? stored : DEFAULT_SERVER;
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
 * `ws://localhost` is permitted even from an HTTPS origin — which is what makes
 * a `CODE@localhost:9374` join code work. Outside a browser (`window`
 * undefined) nothing is blocked.
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
