import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";
import { MELDED_CARD_SCALE } from "./boardSizing.ts";

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

/** Size of a group's cards as a multiple of the row's card size. */
export function groupCardScale(group: GroupedPermanent): number {
  return group.representative?.isMelded ? MELDED_CARD_SCALE : 1;
}

/** Row width a group's visible cards occupy, in normal-card slots. */
export function visibleCardSlotWidth(
  renderMode: GroupRenderMode,
  group: GroupedPermanent,
): number {
  return visibleCardSlotCount(renderMode, group) * groupCardScale(group);
}

export function visibleStaggerCount(
  renderMode: GroupRenderMode,
  group: GroupedPermanent,
): number {
  return renderMode === "staggered" ? Math.max(0, group.count - 1) : 0;
}
