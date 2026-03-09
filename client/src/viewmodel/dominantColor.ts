import type { GameObject, ManaColor, PlayerId } from "../adapter/types";

export function getDominantManaColor(
  battlefieldIds: number[],
  objects: Record<string, GameObject>,
  playerId: PlayerId,
): ManaColor | null {
  const colorCounts = new Map<ManaColor, number>();

  for (const id of battlefieldIds) {
    const obj = objects[String(id)];
    if (!obj) continue;
    if (obj.controller !== playerId) continue;
    if (!obj.card_types.core_types.includes("Land")) continue;

    for (const color of obj.color) {
      colorCounts.set(color, (colorCounts.get(color) ?? 0) + 1);
    }
  }

  if (colorCounts.size === 0) return null;

  let maxColor: ManaColor | null = null;
  let maxCount = 0;

  for (const [color, count] of colorCounts) {
    if (count > maxCount) {
      maxCount = count;
      maxColor = color;
    }
  }

  return maxColor;
}
