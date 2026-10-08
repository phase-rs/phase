import { createStore, del, get, set } from "idb-keyval";

import type { GameState, PlayerId } from "../adapter/types";
import type { P2PAuthorityStamp, P2PSessionKey } from "./p2pSession";

/**
 * The host's terminal statement for one P2P authority incarnation. Unlike a
 * normal state update this is durable browser state: once accepted it fences
 * reconnect and is the only terminal display record a tab may use.
 */
export interface P2PTerminalResult {
  key: P2PSessionKey;
  lease: P2PAuthorityStamp;
  /** Player whose filtered terminal state this commitment authenticates. */
  recipient: PlayerId;
  revision: number;
  terminalId: string;
  finalStateCommitment: string;
  display: {
    winner: PlayerId | null;
    reason: string;
  };
}

const P2P_TERMINAL_PREFIX = "phase-p2p-terminal:";
let terminalStore: ReturnType<typeof createStore> | undefined;

function getTerminalStore(): ReturnType<typeof createStore> {
  if (!terminalStore) {
    terminalStore = createStore("phase-p2p-terminal", "phase-p2p-terminal");
  }
  return terminalStore;
}

function recordKey(key: P2PSessionKey): string {
  return `${P2P_TERMINAL_PREFIX}${key}`;
}

/** The terminal frame binds one key to the host incarnation that issued it. */
export function isValidP2PTerminalResult(value: unknown): value is P2PTerminalResult {
  if (!value || typeof value !== "object") return false;
  const result = value as Partial<P2PTerminalResult>;
  return typeof result.key === "string"
    && result.key.length > 0
    && typeof result.terminalId === "string"
    && result.terminalId.length > 0
    && typeof result.revision === "number"
    && Number.isSafeInteger(result.revision)
    && result.revision >= 0
    && typeof result.recipient === "number"
    && Number.isSafeInteger(result.recipient)
    && result.recipient >= 0
    && typeof result.finalStateCommitment === "string"
    && result.finalStateCommitment.startsWith("sha256:")
    && typeof result.lease?.sessionKey === "string"
    && result.lease.sessionKey === result.key
    && typeof result.lease.hostIncarnation === "string"
    && result.lease.hostIncarnation.length > 0
    && typeof result.display?.reason === "string"
    && (typeof result.display?.winner === "number" || result.display?.winner === null);
}

function sameTerminalResult(left: P2PTerminalResult, right: P2PTerminalResult): boolean {
  return left.key === right.key
    && left.lease.sessionKey === right.lease.sessionKey
    && left.lease.hostIncarnation === right.lease.hostIncarnation
    && left.recipient === right.recipient
    && left.revision === right.revision
    && left.terminalId === right.terminalId
    && left.finalStateCommitment === right.finalStateCommitment
    && left.display.winner === right.display.winner
    && left.display.reason === right.display.reason;
}

/**
 * First valid terminal ID wins. Replays of exactly that statement are
 * idempotent; a different ID or a mutated same-ID statement is rejected.
 */
export async function commitP2PTerminalResult(result: P2PTerminalResult): Promise<boolean> {
  if (!isValidP2PTerminalResult(result)) return false;
  try {
    const key = recordKey(result.key);
    const existing = await get<P2PTerminalResult>(key, getTerminalStore());
    if (existing) return sameTerminalResult(existing, result);
    await set(key, result, getTerminalStore());
    return true;
  } catch {
    return false;
  }
}

export async function loadP2PTerminalResult(key: P2PSessionKey): Promise<P2PTerminalResult | null> {
  try {
    const result = await get<P2PTerminalResult>(recordKey(key), getTerminalStore());
    return result && isValidP2PTerminalResult(result) ? result : null;
  } catch {
    return null;
  }
}

export async function clearP2PTerminalResult(key: P2PSessionKey): Promise<void> {
  try {
    await del(recordKey(key), getTerminalStore());
  } catch {
    // The terminal result only suppresses reconnect in this browser. A failed
    // deletion must not turn an explicit Return to Menu into an app error.
  }
}

/**
 * SHA-256 commitment to the exact terminal state delivered over the ordered
 * P2P channel. JSON is already the transport's canonical state encoding, so
 * hashing this representation detects a terminal claim paired with another
 * final state without adding a second state serializer.
 */
export async function p2pFinalStateCommitment(state: GameState): Promise<string> {
  const bytes = new TextEncoder().encode(JSON.stringify(state));
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  const hex = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `sha256:${hex}`;
}
