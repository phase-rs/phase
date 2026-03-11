import type { GameAction, GameEvent, GameState } from "../adapter/types";

export type P2PMessage =
  | { type: "guest_deck"; deckData: unknown }
  | { type: "game_setup"; state: GameState; events: GameEvent[]; legalActions: GameAction[] }
  | { type: "action"; action: GameAction }
  | { type: "state_update"; state: GameState; events: GameEvent[]; legalActions: GameAction[] }
  | { type: "action_rejected"; reason: string }
  | { type: "ping"; timestamp: number }
  | { type: "pong"; timestamp: number }
  | { type: "disconnect"; reason: string }
  | { type: "emote"; emote: string }
  | { type: "concede" };

const VALID_TYPES = new Set([
  "guest_deck",
  "game_setup",
  "action",
  "state_update",
  "action_rejected",
  "ping",
  "pong",
  "disconnect",
  "emote",
  "concede",
]);

/** Validate an already-parsed object as a P2PMessage. Throws on malformed data. */
export function validateMessage(raw: unknown): P2PMessage {
  if (typeof raw !== "object" || raw === null || !("type" in raw)) {
    throw new Error("Invalid message: missing type field");
  }
  const msg = raw as { type: string };
  if (!VALID_TYPES.has(msg.type)) {
    throw new Error(`Invalid message type: ${msg.type}`);
  }
  return raw as P2PMessage;
}
