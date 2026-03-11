import { isTauri } from "./sidecar";
import { useMultiplayerStore } from "../stores/multiplayerStore";

const DEFAULT_PORT = 9374;

/**
 * Detect the best WebSocket server URL by trying in order:
 * 1. Tauri sidecar on localhost
 * 2. Last-used server address from store
 * 3. Default fallback
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
  if (stored) {
    const httpUrl = wsUrlToHealthUrl(stored);
    if (httpUrl) {
      const reachable = await tryHealthCheck(httpUrl);
      if (reachable) {
        return stored;
      }
    }
  }

  // Step 3: Fall back to stored address or default
  return stored || `ws://localhost:${DEFAULT_PORT}/ws`;
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

  return {
    code,
    serverAddress: `ws://${host}:${port}/ws`,
  };
}

/** Convert ws:// URL to http:// health check URL. */
function wsUrlToHealthUrl(wsUrl: string): string | null {
  try {
    const url = wsUrl
      .replace(/^ws:\/\//, "http://")
      .replace(/^wss:\/\//, "https://")
      .replace(/\/ws\/?$/, "/health");
    return url;
  } catch {
    return null;
  }
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
