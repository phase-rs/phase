import type { DraftPackChoice } from "../../stores/draftStore";

/**
 * How a random lineup spreads its boosters over the available sets.
 *
 * - `OneSet` — one set, chosen at random, fills every booster.
 * - `EachPack` — every booster gets its own random set; a set may repeat.
 * - `DistinctSets` — that many different sets, for a Chaos candidate pool,
 *   where a repeat would only bias the host's own random assignment.
 */
export type RandomPackMode = "OneSet" | "EachPack" | "DistinctSets";

/** A uniform draw in `[0, 1)`, the contract of `Math.random`. */
export type RandomSource = () => number;

function randomIndex(length: number, random: RandomSource): number {
  return Math.min(length - 1, Math.floor(random() * length));
}

/**
 * Pick boosters on the player's behalf from the sets the selector offers.
 *
 * The result is the same pack list the player could have clicked together by
 * hand — the selector shows it before anything starts, and the engine's own
 * selection authority still validates it — so this only stands in for the
 * clicks. `DistinctSets` returns fewer than `count` entries when fewer sets
 * exist; the other modes always return `count`. No sets yields no packs.
 */
export function randomPackLineup(
  sets: readonly DraftPackChoice[],
  count: number,
  mode: RandomPackMode,
  random: RandomSource = Math.random,
): DraftPackChoice[] {
  if (sets.length === 0 || count <= 0) return [];
  const choice = (set: DraftPackChoice): DraftPackChoice => ({ code: set.code, name: set.name });

  switch (mode) {
    case "OneSet": {
      const set = sets[randomIndex(sets.length, random)];
      return Array.from({ length: count }, () => choice(set));
    }
    case "EachPack":
      return Array.from({ length: count }, () => choice(sets[randomIndex(sets.length, random)]));
    case "DistinctSets": {
      // Partial Fisher–Yates: the first `count` slots of a shuffled copy.
      const pool = [...sets];
      const picked = Math.min(count, pool.length);
      for (let i = 0; i < picked; i++) {
        const j = i + randomIndex(pool.length - i, random);
        [pool[i], pool[j]] = [pool[j], pool[i]];
      }
      return pool.slice(0, picked).map(choice);
    }
  }
}
