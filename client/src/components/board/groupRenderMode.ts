import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";

export type BattlefieldRowType = "creatures" | "lands" | "support" | "other";
export type CreatureGroupRenderMode = "single" | "staggered" | "expanded" | "collapsed";

export const CREATURE_GROUP_COLLAPSE_THRESHOLD = 5;
export const GROUP_STAGGER_PX = 20;

interface CreatureGroupRenderOptions {
  manualExpanded: boolean;
  containsCommittedAttackerDuringBlockers: boolean;
}

export function getCreatureGroupRenderMode(
  group: GroupedPermanent,
  rowType: BattlefieldRowType,
  { manualExpanded, containsCommittedAttackerDuringBlockers }: CreatureGroupRenderOptions,
): CreatureGroupRenderMode {
  if (group.count <= 1) return "single";
  if (manualExpanded || containsCommittedAttackerDuringBlockers) return "expanded";
  if (rowType === "creatures" && group.count >= CREATURE_GROUP_COLLAPSE_THRESHOLD) return "collapsed";
  return "staggered";
}

export function visibleCardSlotCount(
  renderMode: CreatureGroupRenderMode,
  group: GroupedPermanent,
): number {
  return renderMode === "expanded" ? group.count : 1;
}

export function visibleStaggerCount(
  renderMode: CreatureGroupRenderMode,
  group: GroupedPermanent,
): number {
  return renderMode === "staggered" ? Math.max(0, group.count - 1) : 0;
}
