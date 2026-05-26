import type { GameObject, PlayerId, TargetRef } from "../../adapter/types.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";

export function targetLabel(
  target: TargetRef,
  objects: Record<string, GameObject> | undefined,
): string {
  if ("Object" in target) {
    return objects?.[String(target.Object)]?.name ?? `Object ${target.Object}`;
  }
  return getPlayerDisplayName(target.Player);
}

export function targetKey(target: TargetRef): string {
  if ("Object" in target) return `obj-${target.Object}`;
  return `player-${target.Player}`;
}

/** Filters targets down to those on a player's own side: permanents they control
 *  plus their own player target. Used by chooser quick-selects so a player can
 *  pick "my side" in one click instead of deselecting every opponent target. */
export function filterTargetsByController(
  targets: TargetRef[],
  objects: Record<string, GameObject> | undefined,
  playerId: PlayerId,
): TargetRef[] {
  return targets.filter((target) =>
    "Object" in target
      ? objects?.[String(target.Object)]?.controller === playerId
      : target.Player === playerId,
  );
}
