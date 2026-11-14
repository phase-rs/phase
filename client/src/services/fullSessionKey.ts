import type { FullSessionKey } from "./multiplayerSession";

/** Whether two Full keys name the same generation of the same game. */
export function sameFullSessionKey(a: FullSessionKey, b: FullSessionKey): boolean {
  return a.game_code === b.game_code && a.generation === b.generation;
}
