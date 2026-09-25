import { describe, expect, it } from "vitest";

import type { GroupedPermanent } from "../../../viewmodel/battlefieldProps.ts";
import {
  getGroupRenderMode,
  groupCardScale,
  groupStaggerPx,
  visibleCardSlotCount,
  visibleCardSlotWidth,
  visibleStaggerCount,
} from "../groupRenderMode.ts";
import { MELDED_CARD_SCALE } from "../boardSizing.ts";

function group(count: number): GroupedPermanent {
  return {
    name: "Saproling",
    ids: Array.from({ length: count }, (_, index) => index + 1),
    count,
    representative: {} as GroupedPermanent["representative"],
    isUnboundedPile: false,
  };
}

describe("getGroupRenderMode", () => {
  it("keeps one permanent as a single card", () => {
    expect(getGroupRenderMode(group(1), {
      manualExpanded: false,
      containsCommittedAttackerDuringBlockers: false,
    })).toBe("single");
  });

  it("keeps two to four matching permanents staggered", () => {
    for (const count of [2, 3, 4]) {
      expect(getGroupRenderMode(group(count), {
        manualExpanded: false,
        containsCommittedAttackerDuringBlockers: false,
      })).toBe("staggered");
    }
  });

  it("collapses five or more matching permanents", () => {
    for (const count of [5, 8, 20]) {
      expect(getGroupRenderMode(group(count), {
        manualExpanded: false,
        containsCommittedAttackerDuringBlockers: false,
      })).toBe("collapsed");
    }
  });

  it("lets manual expansion and committed attackers win over collapsed mode", () => {
    expect(getGroupRenderMode(group(5), {
      manualExpanded: true,
      containsCommittedAttackerDuringBlockers: false,
    })).toBe("expanded");
    expect(getGroupRenderMode(group(5), {
      manualExpanded: false,
      containsCommittedAttackerDuringBlockers: true,
    })).toBe("expanded");
  });

  it("reports sizing slots and stagger counts from the render mode", () => {
    const five = group(5);

    expect(visibleCardSlotCount("collapsed", five)).toBe(1);
    expect(visibleStaggerCount("collapsed", five)).toBe(0);
    expect(visibleCardSlotCount("expanded", five)).toBe(5);
    expect(visibleStaggerCount("expanded", five)).toBe(0);
    expect(visibleCardSlotCount("staggered", five)).toBe(1);
    expect(visibleStaggerCount("staggered", five)).toBe(4);
  });

  it("stacks lands tighter than creatures", () => {
    expect(groupStaggerPx("lands")).toBeLessThan(groupStaggerPx("creatures"));
  });
});

describe("oversized melded cards", () => {
  const melded: GroupedPermanent = {
    ...group(1),
    representative: { isMelded: true } as GroupedPermanent["representative"],
  };

  it("scales only a melded group's card size", () => {
    expect(groupCardScale(group(1))).toBe(1);
    expect(groupCardScale(melded)).toBe(MELDED_CARD_SCALE);
  });

  it("reserves a melded card's extra row width", () => {
    expect(visibleCardSlotWidth("single", group(1))).toBe(1);
    expect(visibleCardSlotWidth("single", melded)).toBe(MELDED_CARD_SCALE);
    expect(visibleCardSlotWidth("expanded", group(3))).toBe(3);
  });
});
