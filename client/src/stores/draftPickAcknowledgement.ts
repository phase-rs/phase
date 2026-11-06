import type { DraftPlayerView } from "../adapter/draft-adapter";

/** How many copies of each instance id the pool holds. */
export function poolMultiplicity(pool: DraftPlayerView["pool"]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const card of pool) counts.set(card.instance_id, (counts.get(card.instance_id) ?? 0) + 1);
  return counts;
}

/**
 * The ids the pool gained, each going from absent to exactly one copy; `null`
 * when any id changed in another way.
 */
export function exactAddedIds(
  before: ReadonlyMap<string, number>,
  after: ReadonlyMap<string, number>,
): string[] | null {
  const added: string[] = [];
  for (const instanceId of new Set([...before.keys(), ...after.keys()])) {
    const previous = before.get(instanceId) ?? 0;
    const current = after.get(instanceId) ?? 0;
    if (current === previous) continue;
    if (previous !== 0 || current !== 1) return null;
    added.push(instanceId);
  }
  return added;
}
