import { createStore, del, get, set } from "idb-keyval";

/**
 * Recipient-scoped terminal delivery issued by the Full server. This is kept
 * separate from normal game persistence: a terminal frame must never be
 * mistaken for an engine snapshot that can be resumed.
 */
export interface FullTerminalDelivery {
  key: { game_code: string; generation: number };
  terminal_revision: number;
  delivery_id: string;
  credential: string;
  display: {
    winner: number | null;
    reason: string;
    ranked_result?: unknown;
  };
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

/** A legacy state snapshot is never a terminal delivery capability. */
export function isValidFullTerminalDelivery(value: unknown): value is FullTerminalDelivery {
  if (!value || typeof value !== "object") return false;
  const delivery = value as Partial<FullTerminalDelivery>;
  const key = delivery.key;
  const display = delivery.display;
  return key !== undefined
    && display !== undefined
    && typeof key.game_code === "string"
    && key.game_code.length > 0
    && typeof key.generation === "number"
    && Number.isSafeInteger(key.generation)
    && key.generation > 0
    && typeof delivery.terminal_revision === "number"
    && Number.isSafeInteger(delivery.terminal_revision)
    && delivery.terminal_revision >= 0
    && typeof delivery.delivery_id === "string"
    && delivery.delivery_id.length > 0
    && typeof delivery.credential === "string"
    && delivery.credential.length > 0
    && typeof display.reason === "string"
    && (typeof display.winner === "number" || display.winner === null);
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
      return existing.delivery_id === delivery.delivery_id
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
    await set(recordKey(delivery.key), delivery, getTerminalStore());
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
