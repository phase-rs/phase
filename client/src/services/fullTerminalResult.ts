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

function isObjectRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** A player record the result screen reads: seat identity, life, elimination. */
function isTerminalPlayerRecord(value: unknown): boolean {
  if (!isObjectRecord(value)) return false;
  return typeof value.id === "number"
    && Number.isInteger(value.id)
    && value.id >= 0
    && typeof value.life === "number"
    && Number.isFinite(value.life)
    && (value.is_eliminated === undefined || typeof value.is_eliminated === "boolean");
}

function isObjectId(value: unknown): boolean {
  return typeof value === "number" && Number.isInteger(value);
}

/**
 * The resolved ability a stack entry exposes. The stack renderer walks
 * `targets` and the nested `sub_ability` chain, and reads `source_card_id` and
 * `description` for the entry label, so those are the fields admission
 * guarantees before the view can be retained, published, or rendered.
 */
function isTerminalAbilityRecord(value: unknown): boolean {
  if (!isObjectRecord(value)) return false;
  return Array.isArray(value.targets)
    && (value.sub_ability === undefined || value.sub_ability === null || isTerminalAbilityRecord(value.sub_ability))
    && (value.else_ability === undefined || value.else_ability === null || isTerminalAbilityRecord(value.else_ability))
    && (value.description === undefined || typeof value.description === "string")
    && (value.selected_mode_labels === undefined
      || (Array.isArray(value.selected_mode_labels)
        && value.selected_mode_labels.every((label) => typeof label === "string")))
    && (value.source_card_id === undefined || isObjectId(value.source_card_id));
}

/** One frozen combat-damage assignment of a pre-M10 `CombatDamage` entry. */
function isTerminalCombatDamageAssignment(value: unknown): boolean {
  if (!isObjectRecord(value) || !isObjectRecord(value.source) || !isObjectRecord(value.target)) {
    return false;
  }
  if (!isObjectId(value.source.object_id) || typeof value.amount !== "number") return false;
  if (typeof value.target.type !== "string") return false;
  switch (value.target.type) {
    case "Object": {
      const data = value.target.data;
      return isObjectRecord(data) && isObjectId(data.object_id);
    }
    case "Player":
      return isObjectId(value.target.data);
    default:
      return false;
  }
}

/**
 * The typed kind the stack renderer switches on (`StackEntry.tsx`,
 * `stackTargets.ts`). Every variant carries a `data` payload; the variants the
 * renderer dereferences have their payload fields checked here as well.
 */
function isTerminalStackEntryKind(value: unknown): boolean {
  if (!isObjectRecord(value) || typeof value.type !== "string" || !isObjectRecord(value.data)) {
    return false;
  }
  const data = value.data;
  switch (value.type) {
    case "Spell":
      return isObjectId(data.card_id)
        && (data.ability === undefined || isTerminalAbilityRecord(data.ability));
    case "ActivatedAbility":
      return isObjectId(data.source_id) && isTerminalAbilityRecord(data.ability);
    case "TriggeredAbility":
      return isObjectId(data.source_id)
        && isTerminalAbilityRecord(data.ability)
        && (data.description === undefined || typeof data.description === "string")
        && (data.source_name === undefined || typeof data.source_name === "string")
        && (data.provenance === undefined || isObjectRecord(data.provenance));
    case "KeywordAction":
      return isObjectRecord(data.action);
    case "CombatDamage":
      return (data.sub_step === "FirstStrike" || data.sub_step === "Regular")
        && Array.isArray(data.assignments)
        && data.assignments.every(isTerminalCombatDamageAssignment);
    default:
      return false;
  }
}

/** A stack entry the board renders: identity, controller, and a typed kind. */
function isTerminalStackEntry(value: unknown): boolean {
  if (!isObjectRecord(value)) return false;
  return isObjectId(value.id)
    && isObjectId(value.source_id)
    && isObjectId(value.controller)
    && isTerminalStackEntryKind(value.kind);
}

/**
 * Validates the final Full state a terminal delivery carries: the typed player
 * records the result screen labels and the top-level collections GamePage
 * renders behind it. A delivery whose view fails this contract is rejected as
 * a whole by `isValidFullTerminalDelivery`, so it can never be retained,
 * acknowledged, or installed as the live game state.
 */
export function isValidFinalView(value: unknown): value is GameState {
  if (!isObjectRecord(value)) return false;
  const state = value as Partial<GameState>;
  const waitingFor = state.waiting_for;
  return Array.isArray(state.players)
    && state.players.length > 0
    && state.players.every(isTerminalPlayerRecord)
    && typeof state.turn_number === "number"
    && Number.isInteger(state.turn_number)
    && typeof state.active_player === "number"
    && typeof state.priority_player === "number"
    && typeof state.phase === "string"
    && Array.isArray(state.battlefield)
    && state.battlefield.every(isObjectId)
    && Array.isArray(state.stack)
    && state.stack.every(isTerminalStackEntry)
    && (state.resolving_stack_entry === undefined
      || isTerminalStackEntry(state.resolving_stack_entry))
    && Array.isArray(state.exile)
    && state.exile.every(isObjectId)
    && isObjectRecord(state.objects)
    && isObjectRecord(waitingFor)
    && typeof waitingFor.type === "string";
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
