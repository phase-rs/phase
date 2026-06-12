import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";

export type BattlefieldRowType = "creatures" | "lands" | "support" | "planeswalkers" | "other";
export type GroupRenderMode = "single" | "staggered" | "expanded" | "collapsed";

export const GROUP_COLLAPSE_THRESHOLD = 5;

const GROUP_STAGGER_PX_BY_ROW: Record<BattlefieldRowType, number> = {
  creatures: 20,
  lands: 10,
  support: 14,
  planeswalkers: 14,
  other: 14,
};

export function groupStaggerPx(rowType: BattlefieldRowType): number {
  return GROUP_STAGGER_PX_BY_ROW[rowType];
}

interface GroupRenderOptions {
  manualExpanded: boolean;
  containsCommittedAttackerDuringBlockers: boolean;
}

export function getGroupRenderMode(
  group: GroupedPermanent,
  { manualExpanded, containsCommittedAttackerDuringBlockers }: GroupRenderOptions,
): GroupRenderMode {
  if (group.count <= 1) return "single";
  if (manualExpanded || containsCommittedAttackerDuringBlockers) return "expanded";
  if (group.count >= GROUP_COLLAPSE_THRESHOLD) return "collapsed";
  return "staggered";
}

export function visibleCardSlotCount(
  renderMode: GroupRenderMode,
  group: GroupedPermanent,
): number {
  return renderMode === "expanded" ? group.count : 1;
}

export function visibleStaggerCount(
  renderMode: GroupRenderMode,
  group: GroupedPermanent,
): number {
  return renderMode === "staggered" ? Math.max(0, group.count - 1) : 0;
}
