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
 * Formats:
 *   "ABC123"                     -> { code: "ABC123" }
 *   "ABC123@192.168.1.5:9374"   -> { code: "ABC123", serverAddress: "ws://192.168.1.5:9374/ws" }
 *   "ABC123@myserver.com"       -> { code: "ABC123", serverAddress: "ws://myserver.com:9374/ws" }
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

  // Parse host:port
  const colonIndex = address.lastIndexOf(":");
  let host: string;
  let port: number;

  if (colonIndex !== -1 && colonIndex < address.length - 1) {
    host = address.slice(0, colonIndex);
    const parsedPort = parseInt(address.slice(colonIndex + 1), 10);
    port = isNaN(parsedPort) ? DEFAULT_PORT : parsedPort;
  } else {
    host = address;
    port = DEFAULT_PORT;
  }

  const isLocal = host === "localhost" || host === "127.0.0.1";
  const scheme = isLocal ? "ws" : "wss";
  const portSuffix = isLocal ? `:${port}` : port !== 443 ? `:${port}` : "";

  return {
    code,
    serverAddress: `${scheme}://${host}${portSuffix}/ws`,
  };
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
