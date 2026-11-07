import type { GameAction } from "../adapter/types";

/**
 * The host clock for timed spoken payments (Toy Boat's "Say \"Toy Boat\"
 * quickly"), in whole Unix-epoch milliseconds.
 *
 * The engine stores the payment's deadline in this domain and judges every
 * host-stamped `SayPhrase` observation against it. Epoch milliseconds stay
 * comparable across a page reload or a reconnect, where `performance.now()`
 * restarts from a new time origin, and they are integers, which is the wire
 * contract of `SayPhrase.at_ms`. The server stamps the same domain.
 */
export function sayPhraseHostClockMs(): number {
  return Math.floor(Date.now());
}

/**
 * Replace the instant a remote seat claimed for a spoken-payment observation
 * with this host's own clock, mirroring the engine's
 * `GameAction::stamped_by_host`. A host must never let a guest choose the time
 * the engine judges; every other action is returned unchanged.
 */
export function stampSayPhraseAtHost(action: GameAction, nowMs: number): GameAction {
  if (action.type !== "SayPhrase") return action;
  return { ...action, data: { ...action.data, at_ms: Math.floor(nowMs) } };
}
