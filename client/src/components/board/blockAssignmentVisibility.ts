import type { ObjectId, PlayerId } from "../../adapter/types.ts";

export function filterVisibleBlockerPairs(
  pairs: Map<ObjectId, ObjectId>,
  objects: Record<string, { controller: PlayerId }> | null,
  visiblePlayerIds: ReadonlySet<PlayerId>,
): Map<ObjectId, ObjectId> {
  if (!objects) return pairs;
  return new Map(
    Array.from(pairs.entries()).filter(([blockerId]) => {
      const blockerController = objects[String(blockerId)]?.controller;
      return blockerController == null || visiblePlayerIds.has(blockerController);
    }),
  );
}
