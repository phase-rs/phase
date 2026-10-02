import type { FormatConfig, MatchType } from "../adapter/types";

export const WS_SESSION_STORAGE_KEY = "phase-ws-session";
export const WS_SESSION_TTL_MS = 2 * 60 * 60 * 1000;

export interface WsHostSessionData {
  formatConfig: FormatConfig;
  timerSeconds: number | null;
  matchType: MatchType;
}

/** Exact server-issued identity for a resumable Full session. */
export interface FullSessionKey {
  game_code: string;
  generation: number;
}

export interface WsSessionData {
  gameCode: string;
  playerToken: string;
  fullKey: FullSessionKey;
  serverUrl: string;
  timestamp: number;
  hostSession?: WsHostSessionData;
  hostIsPublic?: boolean;
}

export function isWsSessionValid(session: WsSessionData): boolean {
  return Date.now() - (session.timestamp ?? 0) < WS_SESSION_TTL_MS;
}

// The latest session the store refused; null once a write lands.
let pageSession: WsSessionData | null = null;

export function loadWsSession(): WsSessionData | null {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(WS_SESSION_STORAGE_KEY);
  } catch {
    // Kept out of the parse `try`, whose catch would drop the page copy.
  }
  try {
    const session = pageSession ?? (raw ? (JSON.parse(raw) as WsSessionData) : null);
    if (!session) return null;
    if (
      !isWsSessionValid(session)
      || !session.fullKey
      || session.fullKey.game_code !== session.gameCode
      || !Number.isInteger(session.fullKey.generation)
      || session.fullKey.generation < 1
    ) {
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
    pageSession = null;
  } catch {
    // A blocked/quota-limited store keeps the session for this page only.
    pageSession = session;
  }
}

export function clearWsSession(): void {
  pageSession = null;
  try {
    localStorage.removeItem(WS_SESSION_STORAGE_KEY);
  } catch {
    // Nothing else can be done if the browser refuses storage access.
  }
}
