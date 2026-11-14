import { createStore, del, get, set } from "idb-keyval";
import type { GameState } from "../adapter/types";

/**
 * Recipient-scoped terminal delivery issued by the Full server. This is kept
 * separate from normal game persistence: a terminal frame must never be
 * mistaken for an engine snapshot that can be resumed.
 */
export interface FullTerminalDelivery {
  key: { game_code: string; generation: number };
  terminalRevision: number;
  deliveryId: string;
  credential: string;
  display: {
    winner: number | null;
    reason: string;
    rankedResult?: unknown;
  };
  finalView?: GameState;
}

const FULL_TERMINAL_PREFIX = "phase-full-terminal:";
let terminalStore: ReturnType<typeof createStore> | undefined;

function getTerminalStore(): ReturnType<typeof createStore> {
  if (!terminalStore) {
    terminalStore = createStore("phase-full-terminal", "phase-full-terminal");
  }
  return terminalStore;
}

function recordKey(key: FullTerminalDelivery["key"]): string {
  return `${FULL_TERMINAL_PREFIX}${key.game_code}:${key.generation}`;
}

/** Validates that a finalView has the required structure of a GameState to prevent crashes. */
export function isValidFinalView(value: unknown): value is GameState {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const state = value as Partial<GameState>;
  return Array.isArray(state.players)
    && state.objects !== null
    && typeof state.objects === "object"
    && !Array.isArray(state.objects);
}

/** A legacy state snapshot is never a terminal delivery capability. */
export function isValidFullTerminalDelivery(value: unknown): value is FullTerminalDelivery {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const delivery = value as Partial<FullTerminalDelivery>;
  const key = delivery.key;
  const display = delivery.display;
  if (!key || typeof key !== "object" || Array.isArray(key) || !display || typeof display !== "object" || Array.isArray(display)) {
    return false;
  }
  return typeof key.game_code === "string"
    && key.game_code.length > 0
    && typeof key.generation === "number"
    && Number.isSafeInteger(key.generation)
    && key.generation > 0
    && typeof delivery.terminalRevision === "number"
    && Number.isSafeInteger(delivery.terminalRevision)
    && delivery.terminalRevision >= 0
    && typeof delivery.deliveryId === "string"
    && delivery.deliveryId.length > 0
    && typeof delivery.credential === "string"
    && delivery.credential.length > 0
    && typeof display.reason === "string"
    && (typeof display.winner === "number" || display.winner === null)
    && (delivery.finalView === undefined || isValidFinalView(delivery.finalView));
}

/**
 * Commits a delivery before normal websocket session state is cleared. Equal
 * deliveries are idempotent; a different terminal for the same key must use
 * the explicit replacement operation below.
 */
export async function commitFullTerminalDelivery(
  delivery: FullTerminalDelivery,
): Promise<boolean> {
  if (!isValidFullTerminalDelivery(delivery)) return false;
  const key = recordKey(delivery.key);
  try {
    const existing = await get<FullTerminalDelivery>(key, getTerminalStore());
    if (existing) {
      return existing.deliveryId === delivery.deliveryId
        && existing.credential === delivery.credential;
    }
    await set(key, delivery, getTerminalStore());
    return true;
  } catch {
    return false;
  }
}

export async function loadFullTerminalDelivery(
  key: FullTerminalDelivery["key"],
): Promise<FullTerminalDelivery | null> {
  try {
    const delivery = await get<FullTerminalDelivery>(recordKey(key), getTerminalStore());
    return delivery && isValidFullTerminalDelivery(delivery) ? delivery : null;
  } catch {
    return null;
  }
}

/** Replaces a stale cached delivery only after the server has issued a newer tuple. */
export async function replaceFullTerminalDelivery(
  delivery: FullTerminalDelivery,
): Promise<boolean> {
  if (!isValidFullTerminalDelivery(delivery)) return false;
  try {
    const key = recordKey(delivery.key);
    const existing = await get<FullTerminalDelivery>(key, getTerminalStore());
    const retainsSameAuthority = existing?.deliveryId === delivery.deliveryId
      && existing.credential === delivery.credential;
    const replacement = delivery.finalView === undefined && retainsSameAuthority
      && existing.finalView !== undefined
      ? { ...delivery, finalView: existing.finalView }
      : delivery;
    await set(key, replacement, getTerminalStore());
    return true;
  } catch {
    return false;
  }
}

export async function clearFullTerminalDelivery(
  key: FullTerminalDelivery["key"],
): Promise<void> {
  try {
    await del(recordKey(key), getTerminalStore());
  } catch {
    // A terminal result is durable best-effort browser state. Server delivery
    // remains the authority and can be re-read with its credential.
  }
}
