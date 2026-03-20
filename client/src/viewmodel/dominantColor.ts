import type { GameObject, ManaColor, PlayerId } from "../adapter/types";

const LAND_SUBTYPE_TO_COLOR: Record<string, ManaColor> = {
  Plains: "White",
  Island: "Blue",
  Swamp: "Black",
  Mountain: "Red",
  Forest: "Green",
};

const SHARD_TO_COLOR: Record<string, ManaColor> = {
  W: "White",
  U: "Blue",
  B: "Black",
  R: "Red",
  G: "Green",
};

function countColors(
  ids: number[],
  objects: Record<string, GameObject>,
  playerId: PlayerId,
  colorCounts: Map<ManaColor, number>,
): void {
  for (const id of ids) {
    const obj = objects[String(id)];
    if (!obj || obj.owner !== playerId) continue;

    const isLand = obj.card_types.core_types.includes("Land");

    if (isLand) {
      // Lands are colorless — infer color from subtypes (Plains, Island, etc.)
      for (const subtype of obj.card_types.subtypes) {
        const color = LAND_SUBTYPE_TO_COLOR[subtype];
        if (color) colorCounts.set(color, (colorCounts.get(color) ?? 0) + 1);
      }
    } else if (obj.mana_cost.type === "Cost") {
      // Non-land permanents: count colored mana shards
      for (const shard of obj.mana_cost.shards) {
        // Handle hybrid shards like "W/U" — count both halves
        for (const part of shard.split("/")) {
          const color = SHARD_TO_COLOR[part];
          if (color) colorCounts.set(color, (colorCounts.get(color) ?? 0) + 1);
        }
      }
    }
  }
}

function resolveMax(colorCounts: Map<ManaColor, number>): ManaColor | null {
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

export function getDominantManaColor(
  battlefieldIds: number[],
  objects: Record<string, GameObject>,
  playerId: PlayerId,
): ManaColor | null {
  const colorCounts = new Map<ManaColor, number>();
  countColors(battlefieldIds, objects, playerId, colorCounts);
  return resolveMax(colorCounts);
}

/** Determine dominant color from the full deck (library + hand + battlefield). */
export function getDeckDominantColor(
  libraryIds: number[],
  handIds: number[],
  battlefieldIds: number[],
  objects: Record<string, GameObject>,
  playerId: PlayerId,
): ManaColor | null {
  const colorCounts = new Map<ManaColor, number>();
  countColors(libraryIds, objects, playerId, colorCounts);
  countColors(handIds, objects, playerId, colorCounts);
  countColors(battlefieldIds, objects, playerId, colorCounts);
  return resolveMax(colorCounts);
}
