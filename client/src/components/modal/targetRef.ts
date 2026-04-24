import type { GameObject, TargetRef } from "../../adapter/types.ts";
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
