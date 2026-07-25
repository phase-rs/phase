import type { ObjectId, PlayerId } from "../../adapter/types.ts";

export type BlockerAssignmentPair = [ObjectId, ObjectId];

export function filterVisibleBlockerPairs(
  pairs: readonly BlockerAssignmentPair[],
  objects: Record<string, { controller: PlayerId }> | null,
  visiblePlayerIds: ReadonlySet<PlayerId>,
): BlockerAssignmentPair[] {
  if (!objects) return [...pairs];
  return pairs.filter(([blockerId]) => {
    const blockerController = objects[String(blockerId)]?.controller;
    return blockerController == null || visiblePlayerIds.has(blockerController);
  });
}
