import type { FormatConfig, MatchType } from "../adapter/types";

export const WS_SESSION_STORAGE_KEY = "phase-ws-session";
export const WS_SESSION_TTL_MS = 2 * 60 * 60 * 1000;

export interface WsHostSessionData {
  formatConfig: FormatConfig;
  timerSeconds: number | null;
  matchType: MatchType;
}

export interface WsSessionData {
  gameCode: string;
  playerToken: string;
  serverUrl: string;
  timestamp: number;
  hostSession?: WsHostSessionData;
  hostIsPublic?: boolean;
}

export function isWsSessionValid(session: WsSessionData): boolean {
  return Date.now() - (session.timestamp ?? 0) < WS_SESSION_TTL_MS;
}

export function loadWsSession(): WsSessionData | null {
  try {
    const raw = localStorage.getItem(WS_SESSION_STORAGE_KEY);
    if (!raw) return null;

    const session = JSON.parse(raw) as WsSessionData;
    if (!isWsSessionValid(session)) {
      clearWsSession();
      return null;
    }
    return session;
  } catch {
    clearWsSession();
    return null;
  }
}

export function saveWsSession(session: WsSessionData): void {
  try {
    localStorage.setItem(WS_SESSION_STORAGE_KEY, JSON.stringify(session));
  } catch {
    // A blocked/quota-limited store disables reconnect persistence, not hosting.
  }
}

export function clearWsSession(): void {
  try {
    localStorage.removeItem(WS_SESSION_STORAGE_KEY);
  } catch {
    // Nothing else can be done if the browser refuses storage access.
  }
}
